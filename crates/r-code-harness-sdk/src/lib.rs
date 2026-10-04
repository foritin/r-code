//! # r-code-harness-sdk
//!
//! Rust SDK for harness plugin processes. A plugin built on this SDK:
//! - serves the host lifecycle (`initialize`, `harness.start/resume/steer/
//!   cancel`, `shutdown`) through [`HarnessHandlers`];
//! - calls host services through the typed [`SdkHandle`] client with
//!   attempt-stable operation keys;
//! - model/process results arrive as single aggregated RPC replies;
//! - observes cancellation through a cooperative flag + notifier.
//!
//! The SDK depends only on the public protocol; it never links host
//! runtime, storage, gateway or Tauri code.

use r_code_harness_protocol::rpc::{
    decode_frame, encode_frame, RpcError, RpcId, RpcMessage, RpcNotification, RpcRequest,
    RpcResponse, MAX_FRAME_BYTES,
};
use r_code_harness_protocol::services::*;
use r_code_harness_protocol::{EventKind, HarnessEventParams};
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
    /// A14：本轮流式输出的推理文本聚合（推理模型；普通流为 None）。
    #[serde(default)]
    pub reasoning: Option<String>,
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
        // A07：等待期取消可中断（此前只在发送前查一次，之后等满超时）。
        // 取消优先于响应：已取消 run 的迟到响应本就应丢弃。
        if self.shared.cancelled.load(Ordering::SeqCst) {
            self.shared.pending.lock().await.remove(&id);
            return Err(SdkError::Cancelled);
        }
        let mut cancel_wait = std::pin::pin!(self.shared.cancel_notify.notified());
        tokio::select! {
            biased;
            _ = &mut cancel_wait => {
                self.shared.pending.lock().await.remove(&id);
                Err(SdkError::Cancelled)
            }
            outcome = tokio::time::timeout(timeout, rx) => match outcome {
                Err(_) => {
                    self.shared.pending.lock().await.remove(&id);
                    Err(SdkError::Timeout("host response"))
                }
                Ok(Err(_)) => Err(SdkError::Closed("reader stopped".into())),
                Ok(Ok(Ok(value))) => Ok(value),
                Ok(Ok(Err(error))) => Err(error.into()),
            }
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

    /// Idempotent, non-destructive `host.process.read`. Reuse the same cursor
    /// after a lost response and advance only to the validated `next_cursor`.
    /// Delivery is at-least-once; callers deduplicate by frame `sequence`.
    pub async fn process_read(
        &self,
        request: ProcessReadRequest,
    ) -> Result<ProcessReadReply, SdkError> {
        validate_process_read_request(&request)?;
        let timeout = Duration::from_millis(u64::from(request.wait_ms.unwrap_or(0)) + 5_000);
        let value = self
            .host_call(
                "host.process.read",
                serde_json::to_value(&request)
                    .map_err(|error| SdkError::Fault(error.to_string()))?,
                timeout,
            )
            .await?;
        let reply: ProcessReadReply =
            serde_json::from_value(value).map_err(|error| SdkError::Fault(error.to_string()))?;
        validate_process_read_reply(&request, &reply)?;
        Ok(reply)
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

    /// `host.plan.publish`. Success means the host validated and durably
    /// selected the immutable revision; a harness must not propose a plan
    /// draft before receiving this typed acknowledgement.
    pub async fn plan_publish(
        &self,
        request: PlanPublishRequest,
    ) -> Result<PlanPublishReply, SdkError> {
        let value = self
            .host_call(
                "host.plan.publish",
                serde_json::to_value(request)
                    .map_err(|error| SdkError::Fault(error.to_string()))?,
                Duration::from_secs(60),
            )
            .await?;
        serde_json::from_value(value).map_err(|error| SdkError::Fault(error.to_string()))
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
        // A07：先注册再查标志——notify_waiters 不留 permit，check-then-register
        // 会丢掉注册前一刻的取消（取消是一次性事件，丢了就永远等）。
        let notified = self.shared.cancel_notify.notified();
        tokio::pin!(notified);
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }

    /// The harness configuration passed at initialize.
    pub async fn harness_config(&self) -> serde_json::Value {
        self.shared.harness_config.lock().await.clone()
    }
}

fn validate_process_read_request(request: &ProcessReadRequest) -> Result<(), SdkError> {
    if request.handle.is_empty() {
        return Err(SdkError::Fault("process read handle is empty".into()));
    }
    if request.max_bytes == 0 || request.max_bytes > PROCESS_READ_MAX_BYTES {
        return Err(SdkError::Fault(format!(
            "process read maxBytes must be in 1..={PROCESS_READ_MAX_BYTES}"
        )));
    }
    if request
        .wait_ms
        .is_some_and(|wait_ms| wait_ms > PROCESS_READ_MAX_WAIT_MS)
    {
        return Err(SdkError::Fault(format!(
            "process read waitMs exceeds {PROCESS_READ_MAX_WAIT_MS}"
        )));
    }
    Ok(())
}

fn validate_process_read_reply(
    request: &ProcessReadRequest,
    reply: &ProcessReadReply,
) -> Result<(), SdkError> {
    use base64::Engine as _;

    let mut expected = request.cursor;
    let mut decoded_bytes = 0usize;
    let mut stdout_eof = false;
    let mut stderr_eof = false;
    let mut saw_exit = false;
    for (index, frame) in reply.frames.iter().enumerate() {
        if frame.sequence() != expected {
            return Err(SdkError::Fault(
                "process read frame sequence has a gap".into(),
            ));
        }
        expected = expected
            .checked_add(1)
            .ok_or_else(|| SdkError::Fault("process read cursor overflow".into()))?;
        match frame {
            ProcessOutputFrame::Data {
                stream,
                data_base64,
                ..
            } => {
                let after_eof = match stream {
                    ProcessOutputStream::Stdout => stdout_eof,
                    ProcessOutputStream::Stderr => stderr_eof,
                };
                if after_eof || saw_exit {
                    return Err(SdkError::Fault(
                        "process read data follows a terminal frame".into(),
                    ));
                }
                let bytes = base64::engine::general_purpose::STANDARD
                    .decode(data_base64)
                    .map_err(|_| SdkError::Fault("process read data is not base64".into()))?;
                if bytes.is_empty() {
                    return Err(SdkError::Fault("process read data frame is empty".into()));
                }
                decoded_bytes = decoded_bytes
                    .checked_add(bytes.len())
                    .ok_or_else(|| SdkError::Fault("process read byte count overflow".into()))?;
            }
            ProcessOutputFrame::Eof { stream, .. } => {
                let seen = match stream {
                    ProcessOutputStream::Stdout => &mut stdout_eof,
                    ProcessOutputStream::Stderr => &mut stderr_eof,
                };
                if *seen || saw_exit {
                    return Err(SdkError::Fault(
                        "process read contains duplicate or late EOF".into(),
                    ));
                }
                *seen = true;
            }
            ProcessOutputFrame::Exit { exit_code, .. } => {
                if saw_exit || index + 1 != reply.frames.len() || !reply.terminal {
                    return Err(SdkError::Fault("process read exit frame is invalid".into()));
                }
                if *exit_code != reply.exit_code {
                    return Err(SdkError::Fault(
                        "process read exit metadata disagrees".into(),
                    ));
                }
                saw_exit = true;
            }
        }
    }
    if reply.next_cursor != expected {
        return Err(SdkError::Fault(
            "process read nextCursor does not follow the page".into(),
        ));
    }
    if reply.terminal && !reply.frames.is_empty() && !saw_exit {
        return Err(SdkError::Fault(
            "non-empty terminal process read lacks a final exit frame".into(),
        ));
    }
    if decoded_bytes > request.max_bytes as usize {
        return Err(SdkError::Fault("process read page exceeds maxBytes".into()));
    }
    if !reply.terminal && reply.exit_code.is_some() {
        return Err(SdkError::Fault(
            "non-terminal process read contains exit metadata".into(),
        ));
    }
    Ok(())
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
    let mut writer_task = tokio::spawn(async move {
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
            // A07：stdin EOF 即宿主已走——置取消、唤醒等待者、排空全部挂起
            // host_call（此前它们会等满各自超时，最长 300s）。
            shared.cancelled.store(true, Ordering::SeqCst);
            shared.cancel_notify.notify_waiters();
            let mut pending = shared.pending.lock().await;
            for (_, sender) in pending.drain() {
                let _ = sender.send(Err(RpcError::internal(String::from(
                    "reader stopped (stdin EOF)",
                ))));
            }
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
                let dispatch_task = tokio::spawn(async move {
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
                    // A07：等派发任务把 shutdown 响应帧入队后再退出循环，
                    // writer 正常排空而非 abort（ack 不再丢失）。JoinError
                    // 仅在派发任务 panic 时出现——那时 ack 已无从谈起，
                    // 循环照常退出。
                    let _ = dispatch_task.await;
                    break;
                }
            }
            RpcMessage::Notification(notification) => {
                if notification.method == "harness.steer" {
                    // A07：steer 派发到独立任务——serve loop 绝不内联等待插件
                    // handler（否则 Response 分发与事件转发全部停摆，与
                    // session 侧跨 await 的 state 锁叠加成互等死锁）。
                    let params = notification.params.unwrap_or(serde_json::Value::Null);
                    match serde_json::from_value::<HarnessSteerParams>(params) {
                        Ok(steer) => {
                            let handlers = handlers.clone();
                            let handle = handle.clone();
                            tokio::spawn(async move {
                                handlers.on_steer(handle, steer).await;
                            });
                        }
                        Err(error) => {
                            eprintln!("malformed harness.steer params dropped: {error}");
                        }
                    }
                }
                // A07：stream.event 注册表从未被订阅（死代码），与其文档假象
                // 一并移除——model_stream 是单次 RPC 聚合，无增量通道。
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
    // A07：给 writer 一个有界排空窗口（让已入队响应帧写出）再兜底 abort。
    // 不能无限等待：共享 Arc<Shared> 仍持有 outbound 发送端，通道不会自然
    // 关闭，无限 await 会挂死（实测教训）。
    drop(outbound_clone(&shared));
    let _ = tokio::time::timeout(std::time::Duration::from_secs(1), &mut writer_task).await;
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
            // A07：与其他方法一致——坏参数显式报错，不再合成空身份静默通过。
            let parsed: HarnessCancelParams = serde_json::from_value(params)
                .map_err(|e| RpcError::invalid_params(e.to_string()))?;
            handle.shared.cancelled.store(true, Ordering::SeqCst);
            handle.shared.cancel_notify.notify_waiters();
            let result = handlers.on_cancel(parsed.reason).await;
            serde_json::to_value(&result).map_err(|e| RpcError::internal(e.to_string()))
        }
        "shutdown" => Ok(serde_json::Value::Null),
        other => Err(RpcError::method_not_found(other)),
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
