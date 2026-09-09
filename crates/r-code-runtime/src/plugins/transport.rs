//! Bounded bidirectional stdio transport for plugin processes.
//!
//! One plugin process per run. stdout carries NDJSON JSON-RPC frames only;
//! stderr is captured into a bounded diagnostic tail. An independent reader
//! task keeps serving plugin callbacks (nested host calls) while host
//! requests are pending. Frame, queue, initialize-timeout and
//! cancellation-grace limits are enforced; after the grace period the
//! process is killed. Descendant containment beyond the direct child is the
//! guardians' job (T14a/T14b).

use r_code_harness_protocol::rpc::{
    decode_frame, encode_frame, FrameError, RpcError, RpcId, RpcMessage, RpcNotification,
    RpcRequest, RpcResponse, CANCEL_GRACE, INITIALIZE_TIMEOUT, MAX_FRAME_BYTES, MAX_QUEUE_BYTES,
};
use r_code_harness_protocol::services::{InitializeParams, InitializeResult};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Mutex};

/// Bounded stderr tail kept for diagnostics.
pub const STDERR_TAIL_BYTES: usize = 64 * 1024;

/// Limits enforced by one transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportLimits {
    pub max_frame_bytes: usize,
    pub max_queue_bytes: usize,
    pub initialize_timeout: Duration,
    pub cancel_grace: Duration,
}

impl Default for TransportLimits {
    fn default() -> Self {
        Self {
            max_frame_bytes: MAX_FRAME_BYTES,
            max_queue_bytes: MAX_QUEUE_BYTES,
            initialize_timeout: INITIALIZE_TIMEOUT,
            cancel_grace: CANCEL_GRACE,
        }
    }
}

/// Transport-level failures.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum TransportError {
    #[error("failed to spawn plugin: {0}")]
    Spawn(String),
    #[error("protocol fault: {0}")]
    Fault(String),
    #[error("frame error: {0}")]
    Frame(#[from] FrameError),
    #[error("outbound queue overflow (> {0} bytes pending)")]
    QueueOverflow(usize),
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
    #[error("plugin connection closed while a call was pending: {0}")]
    Closed(String),
    #[error("rpc error {code}: {message}")]
    Rpc { code: i64, message: String },
}

/// Callbacks the plugin may invoke while the host is waiting.
#[async_trait::async_trait]
pub trait PluginCallbacks: Send + Sync {
    async fn handle_request(&self, request: RpcRequest) -> Result<serde_json::Value, RpcError> {
        Err(RpcError::method_not_found(&request.method))
    }

    async fn handle_notification(&self, _notification: RpcNotification) {}
}

/// Default callbacks: every request fails closed with method-not-found.
#[derive(Default)]
pub struct DenyCallbacks;

#[async_trait::async_trait]
impl PluginCallbacks for DenyCallbacks {}

/// One spawned plugin process with its transport machinery.
pub struct PluginProcess {
    child: Mutex<Child>,
    outbound: mpsc::UnboundedSender<Vec<u8>>,
    queue_bytes: Arc<AtomicUsize>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<PendingReply>>>>,
    next_id: AtomicU64,
    limits: TransportLimits,
    stderr_tail: Arc<Mutex<Vec<u8>>>,
    alive: Arc<AtomicBool>,
}

/// What a pending call resolves to.
enum PendingReply {
    Reply(Result<serde_json::Value, RpcError>),
    Fault(String),
}

/// Spawn a plugin: direct executable + argv, hidden Windows console.
pub async fn spawn_plugin(
    executable: &std::path::Path,
    argv: &[String],
    callbacks: Arc<dyn PluginCallbacks>,
    limits: TransportLimits,
) -> Result<PluginProcess, TransportError> {
    let mut command = Command::new(executable);
    command.args(argv);
    command.stdin(std::process::Stdio::piped());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|e| TransportError::Spawn(e.to_string()))?;
    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");

    let pending: Arc<Mutex<HashMap<String, oneshot::Sender<PendingReply>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let queue_bytes = Arc::new(AtomicUsize::new(0));
    let stderr_tail = Arc::new(Mutex::new(Vec::new()));
    let alive = Arc::new(AtomicBool::new(true));

    // Outbound writer: drains the frame queue with byte accounting.
    let (outbound_tx, outbound_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    {
        let queue_bytes = queue_bytes.clone();
        let alive = alive.clone();
        tokio::spawn(async move {
            let mut writer = tokio::io::BufWriter::new(stdin);
            let mut receiver = outbound_rx;
            while let Some(frame) = receiver.recv().await {
                if writer.write_all(&frame).await.is_err() {
                    alive.store(false, Ordering::SeqCst);
                    break;
                }
                // NDJSON framing requires each frame to reach the plugin
                // immediately; never let a frame linger in the buffer.
                if writer.flush().await.is_err() {
                    alive.store(false, Ordering::SeqCst);
                    break;
                }
                queue_bytes.fetch_sub(frame.len(), Ordering::SeqCst);
            }
        });
    }

    // Reader: decodes frames, resolves pending calls, serves callbacks.
    {
        let pending = pending.clone();
        let alive = alive.clone();
        let stderr_tail = stderr_tail.clone();
        let max_frame = limits.max_frame_bytes;
        let callbacks_outbound = outbound_tx.clone();
        let callbacks_queue_bytes = queue_bytes.clone();
        let callbacks_max_frame = limits.max_frame_bytes;
        tokio::spawn(async move {
            // Bounded stderr capture (ring buffer).
            tokio::spawn(async move {
                let mut reader = BufReader::new(stderr);
                let mut chunk = [0u8; 4096];
                loop {
                    match reader.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let mut tail = stderr_tail.lock().await;
                            tail.extend_from_slice(&chunk[..n]);
                            let overflow = tail.len().saturating_sub(STDERR_TAIL_BYTES);
                            if overflow > 0 {
                                tail.drain(..overflow);
                            }
                        }
                    }
                }
            });

            let mut reader = BufReader::new(stdout);
            loop {
                match read_frame(&mut reader, max_frame).await {
                    Ok(Some(line)) => {
                        let message = match decode_frame(&line) {
                            Ok(message) => message,
                            Err(error) => {
                                fault_pending(&pending, format!("malformed frame: {error}")).await;
                                alive.store(false, Ordering::SeqCst);
                                break;
                            }
                        };
                        match message {
                            RpcMessage::Response(response) => {
                                let key = id_key(&response.id);
                                let mut guard = pending.lock().await;
                                if let Some(sender) = guard.remove(&key) {
                                    let result = if let Some(error) = response.error {
                                        Err(error)
                                    } else {
                                        Ok(response.result.unwrap_or(serde_json::Value::Null))
                                    };
                                    let _ = sender.send(PendingReply::Reply(result));
                                }
                            }
                            RpcMessage::Request(request) => {
                                // Nested callback: the plugin calls the host
                                // while the host awaits its own response. The
                                // independent reader keeps serving.
                                let request_id = request.id.clone();
                                let outcome = callbacks.handle_request(request).await;
                                let response = match outcome {
                                    Ok(result) => RpcResponse {
                                        jsonrpc: "2.0".into(),
                                        id: request_id,
                                        result: Some(result),
                                        error: None,
                                    },
                                    Err(error) => RpcResponse {
                                        jsonrpc: "2.0".into(),
                                        id: request_id,
                                        result: None,
                                        error: Some(error),
                                    },
                                };
                                if let Ok(frame) = encode_frame(&RpcMessage::Response(response)) {
                                    if frame.len() <= callbacks_max_frame {
                                        let queued = callbacks_queue_bytes
                                            .fetch_add(frame.len(), Ordering::SeqCst);
                                        if queued + frame.len() > MAX_QUEUE_BYTES {
                                            callbacks_queue_bytes
                                                .fetch_sub(frame.len(), Ordering::SeqCst);
                                        } else if callbacks_outbound.send(frame).is_err() {
                                            alive.store(false, Ordering::SeqCst);
                                        }
                                    }
                                }
                            }
                            RpcMessage::Notification(notification) => {
                                callbacks.handle_notification(notification).await;
                            }
                        }
                    }
                    Ok(None) => {
                        // Clean EOF: resolve pending calls as closed.
                        fault_pending(&pending, "plugin stdout closed".into()).await;
                        alive.store(false, Ordering::SeqCst);
                        break;
                    }
                    Err(error) => {
                        fault_pending(&pending, format!("frame limit violated: {error}")).await;
                        alive.store(false, Ordering::SeqCst);
                        break;
                    }
                }
            }
        });
    }

    Ok(PluginProcess {
        child: Mutex::new(child),
        outbound: outbound_tx,
        queue_bytes,
        pending,
        next_id: AtomicU64::new(1),
        limits,
        stderr_tail,
        alive,
    })
}

fn id_key(id: &RpcId) -> String {
    match id {
        RpcId::Number(n) => format!("n:{n}"),
        RpcId::Text(t) => format!("t:{t}"),
    }
}

async fn fault_pending(
    pending: &Arc<Mutex<HashMap<String, oneshot::Sender<PendingReply>>>>,
    reason: String,
) {
    let mut guard = pending.lock().await;
    for (_, sender) in guard.drain() {
        let _ = sender.send(PendingReply::Fault(reason.clone()));
    }
}

/// Read one newline-terminated frame, enforcing the byte limit strictly
/// (a line longer than `max` fails even before its terminator arrives).
async fn read_frame<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> Result<Option<Vec<u8>>, FrameError> {
    let mut limited = reader.take(max as u64 + 1);
    let mut buf = Vec::new();
    let read = limited
        .read_until(b'\n', &mut buf)
        .await
        .map_err(|_| FrameError::InvalidUtf8)?;
    if read == 0 {
        return Ok(None);
    }
    if buf.len() > max {
        return Err(FrameError::TooLarge {
            size: buf.len(),
            max,
        });
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
    }
    Ok(Some(buf))
}

impl PluginProcess {
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Bounded stderr tail for diagnostics.
    pub async fn stderr_tail(&self) -> Vec<u8> {
        self.stderr_tail.lock().await.clone()
    }

    fn send_frame(&self, frame: Vec<u8>) -> Result<(), TransportError> {
        let bytes = frame.len();
        if bytes > self.limits.max_frame_bytes {
            return Err(FrameError::TooLarge {
                size: bytes,
                max: self.limits.max_frame_bytes,
            }
            .into());
        }
        let queued = self.queue_bytes.load(Ordering::SeqCst);
        if queued + bytes > self.limits.max_queue_bytes {
            return Err(TransportError::QueueOverflow(self.limits.max_queue_bytes));
        }
        self.queue_bytes.fetch_add(bytes, Ordering::SeqCst);
        self.outbound
            .send(frame)
            .map_err(|_| TransportError::Fault("writer task stopped".into()))
    }

    /// Perform a correlated call with a deadline.
    pub async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value, TransportError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) as i64;
        let key = format!("n:{id}");
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(key, tx);
        let request = RpcMessage::Request(RpcRequest {
            jsonrpc: "2.0".into(),
            id: RpcId::Number(id),
            method: method.to_string(),
            params: Some(params),
        });
        let frame = encode_frame(&request)?;
        if let Err(error) = self.send_frame(frame) {
            // Remove the pending entry again; the call never left.
            self.pending.lock().await.remove(&format!("n:{id}"));
            return Err(error);
        }
        match tokio::time::timeout(timeout, rx).await {
            Err(_) => {
                self.pending.lock().await.remove(&format!("n:{id}"));
                Err(TransportError::Timeout("rpc response"))
            }
            Ok(Err(_)) => Err(TransportError::Closed(method.to_string())),
            Ok(Ok(PendingReply::Fault(reason))) => Err(TransportError::Fault(reason)),
            Ok(Ok(PendingReply::Reply(Ok(value)))) => Ok(value),
            Ok(Ok(PendingReply::Reply(Err(error)))) => Err(TransportError::Rpc {
                code: error.code,
                message: error.message,
            }),
        }
    }

    /// Fire-and-forget notification.
    pub fn notify(&self, method: &str, params: serde_json::Value) -> Result<(), TransportError> {
        let notification = RpcMessage::Notification(RpcNotification {
            jsonrpc: "2.0".into(),
            method: method.to_string(),
            params: Some(params),
        });
        self.send_frame(encode_frame(&notification)?)
    }

    /// Handshake: initialize within the configured timeout.
    pub async fn initialize(
        &self,
        params: &InitializeParams,
    ) -> Result<InitializeResult, TransportError> {
        let value = self
            .request(
                "initialize",
                serde_json::to_value(params).map_err(|e| TransportError::Fault(e.to_string()))?,
                self.limits.initialize_timeout,
            )
            .await?;
        serde_json::from_value(value)
            .map_err(|e| TransportError::Fault(format!("bad initialize result: {e}")))
    }

    /// Cancel: request `harness.cancel`, wait at most the grace period for
    /// acknowledgement, then kill the process regardless.
    pub async fn cancel(&self, reason: &str) -> bool {
        let outcome = tokio::time::timeout(
            self.limits.cancel_grace,
            self.request(
                "harness.cancel",
                serde_json::json!({"identity": {"runId": ""}, "reason": reason}),
                self.limits.cancel_grace + Duration::from_millis(250),
            ),
        )
        .await;
        let acknowledged = matches!(outcome, Ok(Ok(_)));
        self.kill().await;
        acknowledged
    }

    /// Terminate the child and wait briefly for it.
    pub async fn kill(&self) -> Option<i32> {
        let mut child = self.child.lock().await;
        let _ = child.start_kill();
        match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
            Ok(Ok(status)) => status.code(),
            _ => None,
        }
    }

    /// Wait for natural exit.
    pub async fn wait(&self) -> Option<i32> {
        let mut child = self.child.lock().await;
        child.wait().await.ok().and_then(|status| status.code())
    }
}
