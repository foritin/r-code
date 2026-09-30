//! Codex App Server protocol adapter over managed host processes.
//!
//! The plugin opens the `codex` process through `host.process.open`
//! (authorized + frame-validated host-side against the pinned process
//! profile), writes App Server JSON-RPC frames and folds the event stream
//! into typed interactions. Recorded fixtures drive tests; no raw provider
//! reasoning ever crosses the boundary.

use r_code_harness_protocol::{
    ProcessOutputFrame, ProcessOutputStream, ProcessReadRequest, PROCESS_READ_MAX_WAIT_MS,
};
use r_code_harness_sdk::{SdkError, SdkHandle};
use serde_json::Value;
use std::collections::VecDeque;
use std::time::Duration;

const APP_SERVER_READ_MAX_BYTES: u32 = 64 * 1024;
const APP_SERVER_MAX_PARTIAL_LINE_BYTES: usize = 1024 * 1024;

/// Typed App Server events the plugin understands.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum AppServerEvent {
    Initialized { thread_id: String },
    ItemStarted { item_id: String },
    ItemCompleted { item_id: String, output: Value },
    TurnCompleted { usage: Value },
    Error { message: String },
    Unknown { method: String },
}

/// An App Server client over one managed process handle.
pub struct AppServerClient {
    handle_id: String,
    next_request_id: u64,
    read_cursor: u64,
    stdout_buffer: Vec<u8>,
    pending_events: VecDeque<Value>,
}

impl AppServerClient {
    /// Open the App Server process through the host (profile-gated,
    /// frame-validated, guardian-contained).
    pub async fn open(handle: &SdkHandle, cwd: &str) -> Result<Self, SdkError> {
        let opened = handle
            .host_call(
                "host.process.open",
                serde_json::json!({
                    "profile": "codex-app-server",
                    "arguments": ["app-server"],
                    "cwd": cwd,
                }),
                Duration::from_secs(30),
            )
            .await?;
        let handle_id = opened["handle"]
            .as_str()
            .ok_or_else(|| SdkError::Fault("missing process handle".into()))?
            .to_string();
        Ok(Self {
            handle_id,
            next_request_id: 1,
            read_cursor: 0,
            stdout_buffer: Vec::new(),
            pending_events: VecDeque::new(),
        })
    }

    /// Send `initialize` and fold the reply into a typed event.
    pub async fn initialize(
        &mut self,
        handle: &SdkHandle,
        cwd: &str,
    ) -> Result<AppServerEvent, SdkError> {
        let id = self.next_request_id;
        self.next_request_id += 1;
        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "cwd": cwd,
                "approvalPolicy": "on-request",
                "sandboxMode": "workspace-write",
            }
        });
        self.write_frame(handle, &frame).await?;
        // The App Server answers asynchronously via its event stream; the
        // plugin folds events until the initialized marker.
        self.next_event(handle).await
    }

    /// Send a user turn on the thread.
    pub async fn send_user_turn(
        &mut self,
        handle: &SdkHandle,
        cwd: &str,
        text: &str,
    ) -> Result<(), SdkError> {
        let id = self.next_request_id;
        self.next_request_id += 1;
        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "sendUserTurn",
            "params": { "cwd": cwd, "input": text }
        });
        self.write_frame(handle, &frame).await
    }

    /// Interrupt the current turn.
    pub async fn interrupt(&mut self, handle: &SdkHandle) -> Result<(), SdkError> {
        let frame = serde_json::json!({
            "jsonrpc": "2.0", "id": self.next_request_id, "method": "interrupt", "params": {}
        });
        self.next_request_id += 1;
        self.write_frame(handle, &frame).await
    }

    async fn write_frame(&self, handle: &SdkHandle, frame: &Value) -> Result<(), SdkError> {
        use base64::Engine as _;
        let mut line = serde_json::to_string(frame).map_err(|e| SdkError::Fault(e.to_string()))?;
        line.push('\n');
        handle
            .host_call(
                "host.process.write",
                serde_json::json!({
                    "handle": self.handle_id,
                    "data_base64": base64::engine::general_purpose::STANDARD.encode(line),
                }),
                Duration::from_secs(30),
            )
            .await?;
        Ok(())
    }

    /// Read the next folded event from the non-destructive process log. The
    /// cursor advances only after a validated page, so a lost response can be
    /// retried and frame sequence provides at-least-once deduplication.
    pub async fn next_event(&mut self, handle: &SdkHandle) -> Result<AppServerEvent, SdkError> {
        if let Some(value) = self.pending_events.pop_front() {
            return Ok(parse_event(&value));
        }
        loop {
            let page = handle
                .process_read(ProcessReadRequest {
                    handle: self.handle_id.clone(),
                    cursor: self.read_cursor,
                    max_bytes: APP_SERVER_READ_MAX_BYTES,
                    wait_ms: Some(PROCESS_READ_MAX_WAIT_MS),
                })
                .await?;
            let mut saw_exit = false;
            for frame in &page.frames {
                match frame {
                    ProcessOutputFrame::Data {
                        stream: ProcessOutputStream::Stdout,
                        data_base64,
                        ..
                    } => {
                        use base64::Engine as _;
                        let bytes = base64::engine::general_purpose::STANDARD
                            .decode(data_base64)
                            .map_err(|_| SdkError::Fault("invalid App Server stdout".into()))?;
                        self.push_stdout(&bytes)?;
                    }
                    ProcessOutputFrame::Data {
                        stream: ProcessOutputStream::Stderr,
                        ..
                    }
                    | ProcessOutputFrame::Eof {
                        stream: ProcessOutputStream::Stderr,
                        ..
                    } => {}
                    ProcessOutputFrame::Eof {
                        stream: ProcessOutputStream::Stdout,
                        ..
                    } => self.flush_stdout_eof()?,
                    ProcessOutputFrame::Exit { .. } => saw_exit = true,
                }
            }
            self.read_cursor = page.next_cursor;
            if let Some(value) = self.pending_events.pop_front() {
                return Ok(parse_event(&value));
            }
            if saw_exit || (page.terminal && page.frames.is_empty()) {
                return Err(SdkError::Closed(match page.exit_code {
                    Some(code) => format!("App Server exited with code {code}"),
                    None => "App Server exited without an exit code".into(),
                }));
            }
        }
    }

    fn push_stdout(&mut self, bytes: &[u8]) -> Result<(), SdkError> {
        for byte in bytes {
            if *byte == b'\n' {
                self.queue_stdout_line()?;
            } else {
                if self.stdout_buffer.len() == APP_SERVER_MAX_PARTIAL_LINE_BYTES {
                    return Err(SdkError::Fault(
                        "App Server stdout line exceeds the bounded buffer".into(),
                    ));
                }
                self.stdout_buffer.push(*byte);
            }
        }
        Ok(())
    }

    fn queue_stdout_line(&mut self) -> Result<(), SdkError> {
        if self.stdout_buffer.last() == Some(&b'\r') {
            self.stdout_buffer.pop();
        }
        if !self.stdout_buffer.is_empty() {
            let line = std::mem::take(&mut self.stdout_buffer);
            self.pending_events.push_back(
                serde_json::from_slice(&line)
                    .map_err(|error| SdkError::Fault(error.to_string()))?,
            );
        }
        Ok(())
    }

    fn flush_stdout_eof(&mut self) -> Result<(), SdkError> {
        if !self.stdout_buffer.is_empty() {
            self.queue_stdout_line()?;
        }
        Ok(())
    }

    /// Close the App Server process.
    pub async fn close(&self, handle: &SdkHandle) -> Result<(), SdkError> {
        handle
            .host_call(
                "host.process.close",
                serde_json::json!({"handle": self.handle_id, "wait": true}),
                Duration::from_secs(30),
            )
            .await?;
        Ok(())
    }
}

/// Fold a raw App Server notification into a typed event.
pub fn parse_event(value: &Value) -> AppServerEvent {
    let method = value["method"].as_str().unwrap_or_default();
    let params = &value["params"];
    match method {
        "initialized" => AppServerEvent::Initialized {
            thread_id: params["threadId"].as_str().unwrap_or_default().to_string(),
        },
        "item/started" => AppServerEvent::ItemStarted {
            item_id: params["itemId"].as_str().unwrap_or_default().to_string(),
        },
        "item/completed" => AppServerEvent::ItemCompleted {
            item_id: params["itemId"].as_str().unwrap_or_default().to_string(),
            output: params["output"].clone(),
        },
        "turn/completed" => AppServerEvent::TurnCompleted {
            usage: params["usage"].clone(),
        },
        "error" => AppServerEvent::Error {
            message: params["message"].as_str().unwrap_or_default().to_string(),
        },
        other => AppServerEvent::Unknown {
            method: other.to_string(),
        },
    }
}
