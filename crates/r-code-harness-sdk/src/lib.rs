//! # r-code-harness-sdk
//!
//! Rust SDK for harness plugin processes. A plugin built on this SDK:
//! - serves the host lifecycle (`initialize`, `harness.start/resume/steer/
//!   cancel`, `shutdown`) through [`HarnessHandlers`];
//! - calls host services through the typed [`SdkHandle`] client with
//!   attempt-stable operation keys;
//! - consumes model/process streams via correlated notifications;
//! - observes cancellation through a cooperative flag + notifier.
//!
//! The SDK depends only on the public protocol; it never links host
//! runtime, storage, gateway or Tauri code.

use r_code_harness_protocol::rpc::{
    decode_frame, encode_frame, RpcError, RpcId, RpcMessage, RpcNotification, RpcRequest,
    RpcResponse, MAX_FRAME_BYTES,
};
use r_code_harness_protocol::services::*;
use r_code_harness_protocol::{EventKind, HarnessEventParams, StreamEvent};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, Mutex, Notify};

/// Final outcome of a model stream, as reported by the host.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelStreamOutcome {
    pub stream_id: String,
    pub finish_reason: Option<String>,
    pub usage: r_code_harness_protocol::ModelUsage,
}

/// The loop-facing turn result: outcome + the assistant's wire turn.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModelTurn {
    #[serde(flatten)]
    pub outcome: ModelStreamOutcome,
    /// {"role": "assistant", "blocks": [...]}, blocks of type text /
    /// tool-call.
    pub assistant: serde_json::Value,
}

impl ModelTurn {
    /// The assistant text of the turn.
    pub fn text(&self) -> String {
        self.assistant["blocks"]
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|block| block["type"] == "text")
                    .filter_map(|block| block["text"].as_str())
                    .collect::<String>()
            })
            .unwrap_or_default()
    }

    /// The tool calls of the turn: (id, name, input) triples.
    pub fn tool_calls(&self) -> Vec<(String, String, serde_json::Value)> {
        self.assistant["blocks"]
            .as_array()
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|block| block["type"] == "tool-call")
                    .filter_map(|block| {
                        Some((
                            block["id"].as_str()?.to_string(),
                            block["name"].as_str().unwrap_or_default().to_string(),
                            block["input"].clone(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// SDK failures.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SdkError {
    #[error("protocol fault: {0}")]
    Fault(String),
    #[error("rpc error {code}: {message}")]
    Rpc { code: i64, message: String },
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
    #[error("channel closed: {0}")]
    Closed(String),
    #[error("cancelled")]
    Cancelled,
}

impl From<RpcError> for SdkError {
    fn from(error: RpcError) -> Self {
        SdkError::Rpc {
            code: error.code,
            message: error.message,
        }
    }
}

/// Lifecycle handlers implemented by a harness.
#[async_trait::async_trait]
pub trait HarnessHandlers: Send + Sync + 'static {
    /// Identify the harness and finish the handshake.
    async fn on_initialize(&self, params: InitializeParams) -> Result<InitializeResult, SdkError> {
        let _ = params;
        Ok(InitializeResult {
            harness_id: "unnamed.harness".into(),
            harness_version: env!("CARGO_PKG_VERSION").into(),
            ready_checkpoint: None,
        })
    }

    /// Begin a fresh attempt.
    async fn on_start(
        &self,
        handle: SdkHandle,
        params: HarnessStartParams,
    ) -> Result<serde_json::Value, SdkError>;

    /// Continue from a host-stored checkpoint.
    async fn on_resume(
        &self,
        handle: SdkHandle,
        params: HarnessResumeParams,
    ) -> Result<serde_json::Value, SdkError> {
        let _ = (handle, params);
        Ok(serde_json::Value::Null)
    }

    /// A steer notification arrived mid-run.
    async fn on_steer(&self, handle: SdkHandle, params: HarnessSteerParams) {
        let _ = (handle, params);
    }

    /// Cancellation was requested; acknowledge by returning.
    async fn on_cancel(&self, reason: Option<String>) -> HarnessCancelResult {
        let _ = reason;
        HarnessCancelResult { acknowledged: true }
    }
}

#[async_trait::async_trait]
impl<H: HarnessHandlers> HarnessHandlers for std::sync::Arc<H> {
    async fn on_initialize(&self, params: InitializeParams) -> Result<InitializeResult, SdkError> {
        (**self).on_initialize(params).await
    }

    async fn on_start(
        &self,
        handle: SdkHandle,
        params: HarnessStartParams,
    ) -> Result<serde_json::Value, SdkError> {
        (**self).on_start(handle, params).await
    }

    async fn on_resume(
        &self,
        handle: SdkHandle,
        params: HarnessResumeParams,
    ) -> Result<serde_json::Value, SdkError> {
        (**self).on_resume(handle, params).await
    }

    async fn on_steer(&self, handle: SdkHandle, params: HarnessSteerParams) {
        (**self).on_steer(handle, params).await
    }

    async fn on_cancel(&self, reason: Option<String>) -> HarnessCancelResult {
        (**self).on_cancel(reason).await
    }
}

struct Shared {
    next_id: AtomicI64,
    pending: Mutex<HashMap<i64, oneshot::Sender<Result<serde_json::Value, RpcError>>>>,
    outbound: mpsc::UnboundedSender<Vec<u8>>,
    cancelled: AtomicBool,
    cancel_notify: Notify,
    streams: Mutex<HashMap<String, mpsc::UnboundedSender<StreamEvent>>>,
    harness_config: Mutex<serde_json::Value>,
}

/// Cloneable client handle used by handlers to call the host.
#[derive(Clone)]
pub struct SdkHandle {
    shared: Arc<Shared>,
}

impl SdkHandle {
    /// Raw host call with a deadline.
    pub async fn host_call(
        &self,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value, SdkError> {
        if self.shared.cancelled.load(Ordering::SeqCst) {
            return Err(SdkError::Cancelled);
        }
        let id = self.shared.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        self.shared.pending.lock().await.insert(id, tx);
        let request = RpcMessage::Request(RpcRequest {
            jsonrpc: "2.0".into(),
            id: RpcId::Number(id),
            method: method.to_string(),
            params: Some(params),
        });
        let frame = encode_frame(&request).map_err(|e| SdkError::Fault(e.to_string()))?;
        if self.shared.outbound.send(frame).is_err() {
            self.shared.pending.lock().await.remove(&id);
            return Err(SdkError::Closed("writer stopped".into()));
        }
        match tokio::time::timeout(timeout, rx).await {
            Err(_) => {
                self.shared.pending.lock().await.remove(&id);
                Err(SdkError::Timeout("host response"))
            }
            Ok(Err(_)) => Err(SdkError::Closed("reader stopped".into())),
            Ok(Ok(Ok(value))) => Ok(value),
            Ok(Ok(Err(error))) => Err(error.into()),
        }
    }

    /// Progress event to the host.
    pub async fn emit_event(
        &self,
        kind: EventKind,
        payload: serde_json::Value,
    ) -> Result<(), SdkError> {
        let params = serde_json::to_value(HarnessEventParams {
            kind,
            payload,
            stream_id: None,
        })
        .map_err(|e| SdkError::Fault(e.to_string()))?;
        self.host_notify("harness.event", params)
    }

    fn host_notify(&self, method: &str, params: serde_json::Value) -> Result<(), SdkError> {
        let notification = RpcMessage::Notification(RpcNotification {
            jsonrpc: "2.0".into(),
            method: method.to_string(),
            params: Some(params),
        });
        let frame = encode_frame(&notification).map_err(|e| SdkError::Fault(e.to_string()))?;
        self.shared
            .outbound
            .send(frame)
            .map_err(|_| SdkError::Closed("writer stopped".into()))
    }

    /// `host.tools.list`.
    pub async fn tools_list(&self) -> Result<ToolsListReply, SdkError> {
        let value = self
            .host_call(
                "host.tools.list",
                serde_json::to_value(ToolsListRequest { cursor: None }).unwrap_or_default(),
                Duration::from_secs(30),
            )
            .await?;
        serde_json::from_value(value).map_err(|e| SdkError::Fault(e.to_string()))
    }

    /// `host.tools.call` with an attempt-stable operation key.
    pub async fn tools_call(
        &self,
        tool: &str,
        input: serde_json::Value,
        operation_key: Option<&str>,
    ) -> Result<ToolCallReply, SdkError> {
        let mut params = serde_json::json!({"tool": tool, "input": input});
        if let Some(key) = operation_key {
            params["operation_key"] = serde_json::json!(key);
        }
        let value = self
            .host_call("host.tools.call", params, Duration::from_secs(120))
            .await?;
        serde_json::from_value(value).map_err(|e| SdkError::Fault(e.to_string()))
    }

    /// `host.model.stream`, consuming correlated notifications until done.
    /// Returns the final outcome plus the assistant turn (text + tool
    /// calls) projected by the host.
    pub async fn model_stream(&self, request: ModelStreamRequest) -> Result<ModelTurn, SdkError> {
        let value = self
            .host_call(
                "host.model.stream",
                serde_json::to_value(&request).map_err(|e| SdkError::Fault(e.to_string()))?,
                Duration::from_secs(300),
            )
            .await?;
        serde_json::from_value(value).map_err(|e| SdkError::Fault(e.to_string()))
    }

    /// `host.questions.ask`.
    pub async fn ask_question(&self, text: &str, blocking: bool) -> Result<String, SdkError> {
        let value = self
            .host_call(
                "host.questions.ask",
                serde_json::to_value(QuestionsAskRequest {
                    text: text.into(),
                    options: vec![],
                    blocking,
                })
                .unwrap_or_default(),
                Duration::from_secs(30),
            )
            .await?;
        Ok(serde_json::from_value::<QuestionsAskReply>(value)
            .map_err(|e| SdkError::Fault(e.to_string()))?
            .question_id)
    }

    /// `host.approvals.request` citing a host-created pending operation.
    pub async fn request_approval(
        &self,
        pending_operation: r_code_harness_protocol::PendingOperationRef,
        summary: &str,
    ) -> Result<ApprovalDecision, SdkError> {
        let value = self
            .host_call(
                "host.approvals.request",
                serde_json::to_value(ApprovalsRequest {
                    pending_operation,
                    summary: summary.into(),
                })
                .unwrap_or_default(),
                Duration::from_secs(60),
            )
            .await?;
        Ok(serde_json::from_value::<ApprovalsReply>(value)
            .map_err(|e| SdkError::Fault(e.to_string()))?
            .decision)
    }

    /// `host.checkpoint.save` with opaque plugin state.
    pub async fn save_checkpoint(
        &self,
        state: &[u8],
        consumed_input_seq: u64,
    ) -> Result<CheckpointSaveReply, SdkError> {
        use base64::Engine as _;
        let params = serde_json::to_value(CheckpointSaveRequest {
            state_base64: base64::engine::general_purpose::STANDARD.encode(state),
            consumed_input_seq,
        })
        .unwrap_or_default();
        let value = self
            .host_call("host.checkpoint.save", params, Duration::from_secs(30))
            .await?;
        serde_json::from_value(value).map_err(|e| SdkError::Fault(e.to_string()))
    }

    /// `host.completion.propose`.
    pub async fn propose_completion(
        &self,
        request: CompletionProposalRequest,
    ) -> Result<CompletionProposalReply, SdkError> {
        let value = self
            .host_call(
                "host.completion.propose",
                serde_json::to_value(&request).unwrap_or_default(),
                Duration::from_secs(60),
            )
            .await?;
        serde_json::from_value(value).map_err(|e| SdkError::Fault(e.to_string()))
    }

    /// `host.children.spawn`.
    pub async fn spawn_child(
        &self,
        objective: &str,
        harness: Option<&str>,
        permissions: r_code_harness_protocol::services::PermissionCeiling,
    ) -> Result<ChildrenSpawnReply, SdkError> {
        let value = self
            .host_call(
                "host.children.spawn",
                serde_json::to_value(ChildrenSpawnRequest {
                    objective: objective.into(),
                    harness: harness.map(str::to_string),
                    permissions,
                    budget_share: None,
                })
                .unwrap_or_default(),
                Duration::from_secs(30),
            )
            .await?;
        serde_json::from_value(value).map_err(|e| SdkError::Fault(e.to_string()))
    }

    /// `host.children.wait` (non-blocking poll; errors carry "not completed").
    pub async fn wait_child(&self, child_task_id: &str) -> Result<ChildReport, SdkError> {
        let value = self
            .host_call(
                "host.children.wait",
                serde_json::to_value(ChildrenWaitRequest {
                    child_task_id: child_task_id.into(),
                    timeout_ms: None,
                })
                .unwrap_or_default(),
                Duration::from_secs(30),
            )
            .await?;
        serde_json::from_value(value).map_err(|e| SdkError::Fault(e.to_string()))
    }

    /// `host.children.cancel`.
    pub async fn cancel_child(&self, child_task_id: &str) -> Result<(), SdkError> {
        self.host_call(
            "host.children.cancel",
            serde_json::to_value(ChildrenCancelRequest {
                child_task_id: child_task_id.into(),
            })
            .unwrap_or_default(),
            Duration::from_secs(30),
        )
        .await?;
        Ok(())
    }

    /// True once the host asked this run to cancel.
    pub fn is_cancelled(&self) -> bool {
        self.shared.cancelled.load(Ordering::SeqCst)
    }

    /// Wait until cancellation is requested.
    pub async fn wait_for_cancel(&self) {
        if self.is_cancelled() {
            return;
        }
        self.shared.cancel_notify.notified().await;
    }

    /// The harness configuration passed at initialize.
    pub async fn harness_config(&self) -> serde_json::Value {
        self.shared.harness_config.lock().await.clone()
    }
}

/// Serve the protocol on stdin/stdout until `shutdown` or EOF.
pub async fn serve<H: HarnessHandlers>(handlers: H) -> Result<(), SdkError> {
    serve_with_limits(handlers, MAX_FRAME_BYTES).await
}

/// Serve with an explicit frame limit (used by tests).
pub async fn serve_with_limits<H: HarnessHandlers>(
    handlers: H,
    max_frame: usize,
) -> Result<(), SdkError> {
    let handlers: Arc<dyn HarnessHandlers> = Arc::new(handlers);
    let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    // Writer: one frame per line, flushed immediately.
    let writer_task = tokio::spawn(async move {
        let mut stdout = tokio::io::BufWriter::new(tokio::io::stdout());
        while let Some(frame) = outbound_rx.recv().await {
            if stdout.write_all(&frame).await.is_err() {
                break;
            }
            if stdout.flush().await.is_err() {
                break;
            }
        }
    });

    let shared = Arc::new(Shared {
        next_id: AtomicI64::new(1),
        pending: Mutex::new(HashMap::new()),
        outbound: outbound_tx,
        cancelled: AtomicBool::new(false),
        cancel_notify: Notify::new(),
        streams: Mutex::new(HashMap::new()),
        harness_config: Mutex::new(serde_json::Value::Null),
    });
    let handle = SdkHandle {
        shared: shared.clone(),
    };
    let reply_tx = outbound_clone(&shared);

    let mut reader = BufReader::new(tokio::io::stdin());
    loop {
        let mut line = Vec::new();
        let mut limited = (&mut reader).take(max_frame as u64 + 1);
        let n = limited
            .read_until(b'\n', &mut line)
            .await
            .map_err(|e| SdkError::Fault(e.to_string()))?;
        if n == 0 {
            break;
        }
        if line.len() > max_frame {
            return Err(SdkError::Fault("inbound frame exceeds the limit".into()));
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.is_empty() {
            continue;
        }
        let message = decode_frame(&line).map_err(|e| SdkError::Fault(e.to_string()))?;
        match message {
            RpcMessage::Request(request) => {
                // Dispatch in a spawned task so the reader keeps consuming
                // frames while handlers run — nested host calls made inside
                // a handler must not deadlock against this loop.
                let id = request.id.clone();
                let is_shutdown = request.method == "shutdown";
                let handlers = handlers.clone();
                let handle = handle.clone();
                let reply_tx = reply_tx.clone();
                tokio::spawn(async move {
                    let outcome = dispatch_request(&handlers, handle, request).await;
                    let response = match outcome {
                        Ok(result) => RpcResponse {
                            jsonrpc: "2.0".into(),
                            id,
                            result: Some(result),
                            error: None,
                        },
                        Err(error) => RpcResponse {
                            jsonrpc: "2.0".into(),
                            id,
                            result: None,
                            error: Some(error),
                        },
                    };
                    if let Ok(frame) = encode_frame(&RpcMessage::Response(response)) {
                        let _ = reply_tx.send(frame);
                    }
                });
                if is_shutdown {
                    break;
                }
            }
            RpcMessage::Notification(notification) => {
                if notification.method == "harness.steer" {
                    if let Some(params) = notification.params {
                        if let Ok(steer) = serde_json::from_value::<HarnessSteerParams>(params) {
                            handlers.on_steer(handle.clone(), steer).await;
                        }
                    }
                } else if notification.method == "stream.event" {
                    if let Some(params) = notification.params {
                        if let Ok(event) = serde_json::from_value::<StreamEvent>(params) {
                            let streams = shared.streams.lock().await;
                            if let Some(sender) = streams.get(&event.stream_id) {
                                let _ = sender.send(event);
                            }
                        }
                    }
                }
            }
            RpcMessage::Response(response) => {
                if let RpcId::Number(id) = response.id {
                    let mut pending = shared.pending.lock().await;
                    if let Some(sender) = pending.remove(&id) {
                        let result = if let Some(error) = response.error {
                            Err(error)
                        } else {
                            Ok(response.result.unwrap_or(serde_json::Value::Null))
                        };
                        let _ = sender.send(result);
                    }
                }
            }
        }
    }
    writer_task.abort();
    Ok(())
}

fn outbound_clone(shared: &Arc<Shared>) -> mpsc::UnboundedSender<Vec<u8>> {
    shared.outbound.clone()
}

async fn dispatch_request(
    handlers: &Arc<dyn HarnessHandlers>,
    handle: SdkHandle,
    request: RpcRequest,
) -> Result<serde_json::Value, RpcError> {
    let params = request.params.clone().unwrap_or(serde_json::Value::Null);
    match request.method.as_str() {
        "initialize" => {
            let parsed: InitializeParams = serde_json::from_value(params)
                .map_err(|e| RpcError::invalid_params(e.to_string()))?;
            *handle.shared.harness_config.lock().await = parsed.harness_config.clone();
            let result = handlers
                .on_initialize(parsed)
                .await
                .map_err(sdk_error_to_rpc)?;
            serde_json::to_value(&result).map_err(|e| RpcError::internal(e.to_string()))
        }
        "harness.start" => {
            let parsed: HarnessStartParams = serde_json::from_value(params)
                .map_err(|e| RpcError::invalid_params(e.to_string()))?;
            let result = handlers
                .on_start(handle, parsed)
                .await
                .map_err(sdk_error_to_rpc)?;
            Ok(result)
        }
        "harness.resume" => {
            let parsed: HarnessResumeParams = serde_json::from_value(params)
                .map_err(|e| RpcError::invalid_params(e.to_string()))?;
            let result = handlers
                .on_resume(handle, parsed)
                .await
                .map_err(sdk_error_to_rpc)?;
            Ok(result)
        }
        "harness.cancel" => {
            let parsed: HarnessCancelParams =
                serde_json::from_value(params).unwrap_or(HarnessCancelParams {
                    identity: empty_identity(),
                    reason: None,
                });
            handle.shared.cancelled.store(true, Ordering::SeqCst);
            handle.shared.cancel_notify.notify_waiters();
            let result = handlers.on_cancel(parsed.reason).await;
            serde_json::to_value(&result).map_err(|e| RpcError::internal(e.to_string()))
        }
        "shutdown" => Ok(serde_json::Value::Null),
        other => Err(RpcError::method_not_found(other)),
    }
}

fn empty_identity() -> r_code_harness_protocol::RunIdentity {
    r_code_harness_protocol::RunIdentity {
        task_id: String::new(),
        branch_id: String::new(),
        run_id: String::new(),
        attempt_id: String::new(),
        generation: 0,
    }
}

fn sdk_error_to_rpc(error: SdkError) -> RpcError {
    match error {
        SdkError::Rpc { code, message } => RpcError {
            code,
            message,
            data: None,
        },
        other => RpcError::internal(other.to_string()),
    }
}
