//! Codex App Server protocol adapter over managed host processes.
//!
//! The plugin opens the `codex` process through `host.process.open`
//! (authorized + frame-validated host-side against the pinned process
//! profile), writes App Server JSON-RPC frames and folds the event stream
//! into typed interactions. Recorded fixtures drive tests; no raw provider
//! reasoning ever crosses the boundary.

use r_code_harness_sdk::{SdkError, SdkHandle};
use serde_json::Value;
use std::time::Duration;

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

    /// Read the next folded event (notifications routed through the host's
    /// stream bridge arrive as `stream.event` payloads; this first cut
    /// polls the process stream via a host-provided cursor call).
    pub async fn next_event(&mut self, handle: &SdkHandle) -> Result<AppServerEvent, SdkError> {
        let value = handle
            .host_call(
                "codex.event.next",
                serde_json::json!({"handle": self.handle_id}),
                Duration::from_secs(120),
            )
            .await
            .map_err(|error| SdkError::Rpc {
                code: -32000,
                message: format!("app-server event stream: {error}"),
            })?;
        Ok(parse_event(&value))
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
