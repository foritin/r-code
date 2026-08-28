//! Task-isolated ownership for long-lived Codex App Server transports.
//!
//! The registry owns only initialized process transports. Approval, delegation, observer,
//! steering, and cancellation state belong to a single run and must never be stored here.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use r_code_core::process::{hide_background_console, kill_tree};
use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, Mutex, OwnedMutexGuard};
use tokio::task::JoinHandle;
use tokio::time::{timeout, Duration, Instant};
use tokio_util::sync::CancellationToken;

const MAX_LINE_BYTES: usize = 32 * 1024 * 1024;
const MAX_LINE_QUEUE: usize = 2;
const MAX_WRITER_QUEUE: usize = 64;
const MAX_DEFERRED_BYTES: usize = 64 * 1024 * 1024;
const SHUTDOWN_JOIN_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const CODEX_DISABLE_R_CODE_MCP_OVERRIDE: &str =
    "mcp_servers.r-code={enabled=false,command='r-code-disabled'}";
const _: () = assert!(MAX_LINE_BYTES * MAX_LINE_QUEUE <= 64 * 1024 * 1024);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexAppServerError {
    Workspace,
    Cli,
    Config,
    Launch,
    StartupTimeout,
    Protocol,
    Stream,
}

impl std::fmt::Display for CodexAppServerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Workspace => "Codex App Server workspace identity is unavailable",
            Self::Cli => "Codex App Server CLI identity is unavailable",
            Self::Config => "Codex App Server configuration identity is unavailable",
            Self::Launch => "Codex App Server could not be launched",
            Self::StartupTimeout => "Codex App Server made no startup progress",
            Self::Protocol => "Codex App Server returned invalid protocol data",
            Self::Stream => "Codex App Server transport disconnected",
        })
    }
}

impl std::error::Error for CodexAppServerError {}

#[derive(Debug)]
pub(crate) enum CodexAppServerLineEvent {
    Line(String),
    Eof,
    TooLong,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CodexAppServerIdentity {
    canonical_workspace: PathBuf,
    resolved_cli_path: PathBuf,
    config_fingerprint: String,
}

#[derive(Default)]
struct CodexAppServerTaskState {
    identity: Option<CodexAppServerIdentity>,
    transport: Option<CodexAppServerTransport>,
}

struct CodexAppServerTaskSlot {
    state: Arc<Mutex<CodexAppServerTaskState>>,
    shutdown: CancellationToken,
}

impl Default for CodexAppServerTaskSlot {
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(CodexAppServerTaskState::default())),
            shutdown: CancellationToken::new(),
        }
    }
}

/// Application-owned registry. Each task has its own independently locked transport slot.
pub struct CodexAppServerRegistry {
    slots: Mutex<HashMap<String, Arc<CodexAppServerTaskSlot>>>,
    shutting_down: AtomicBool,
    shutdown: CancellationToken,
}

impl Default for CodexAppServerRegistry {
    fn default() -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            shutting_down: AtomicBool::new(false),
            shutdown: CancellationToken::new(),
        }
    }
}

impl CodexAppServerRegistry {
    pub(crate) async fn acquire(
        &self,
        task_id: &str,
        workspace: &Path,
        cli_path: Option<PathBuf>,
        config_path: &Path,
        startup_timeout: Duration,
    ) -> Result<CodexAppServerLease, CodexAppServerError> {
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(CodexAppServerError::Stream);
        }
        let (identity, resolved_cli_path) = resolve_identity(workspace, cli_path, config_path)?;
        let slot = {
            let mut slots = self.slots.lock().await;
            slots
                .entry(task_id.to_string())
                .or_insert_with(|| Arc::new(CodexAppServerTaskSlot::default()))
                .clone()
        };
        let mut state = tokio::select! {
            _ = self.shutdown.cancelled() => return Err(CodexAppServerError::Stream),
            _ = slot.shutdown.cancelled() => return Err(CodexAppServerError::Stream),
            state = slot.state.clone().lock_owned() => state,
        };
        if self.shutting_down.load(Ordering::Acquire) || slot.shutdown.is_cancelled() {
            return Err(CodexAppServerError::Stream);
        }

        if state.identity.as_ref() != Some(&identity) {
            if let Some(transport) = state.transport.take() {
                tokio::select! {
                    _ = self.shutdown.cancelled() => return Err(CodexAppServerError::Stream),
                    _ = slot.shutdown.cancelled() => return Err(CodexAppServerError::Stream),
                    _ = transport.shutdown() => {}
                }
            }
            state.identity = None;
        }
        if state
            .transport
            .as_mut()
            .is_some_and(|transport| !transport.is_quiescent())
        {
            if let Some(transport) = state.transport.take() {
                tokio::select! {
                    _ = self.shutdown.cancelled() => return Err(CodexAppServerError::Stream),
                    _ = slot.shutdown.cancelled() => return Err(CodexAppServerError::Stream),
                    _ = transport.shutdown() => {}
                }
            }
            state.identity = None;
        }
        if state.transport.is_none() {
            let launch_workspace = codex_app_server_launch_path(&identity.canonical_workspace);
            let transport = tokio::select! {
                _ = self.shutdown.cancelled() => return Err(CodexAppServerError::Stream),
                _ = slot.shutdown.cancelled() => return Err(CodexAppServerError::Stream),
                transport = CodexAppServerTransport::connect(
                    Some(resolved_cli_path),
                    &launch_workspace,
                    startup_timeout,
                ) => transport?,
            };
            state.transport = Some(transport);
            state.identity = Some(identity);
        }

        Ok(CodexAppServerLease {
            state,
            shutdown: slot.shutdown.clone(),
            slot,
            reusable: false,
        })
    }

    #[cfg(test)]
    pub(crate) async fn prepare(
        &self,
        task_id: &str,
        workspace: &Path,
        cli_path: Option<PathBuf>,
        config_path: &Path,
        startup_timeout: Duration,
    ) -> Result<(), CodexAppServerError> {
        self.prepare_tracked(task_id, workspace, cli_path, config_path, startup_timeout)
            .await
            .map(drop)
    }

    pub(crate) async fn prepare_tracked(
        &self,
        task_id: &str,
        workspace: &Path,
        cli_path: Option<PathBuf>,
        config_path: &Path,
        startup_timeout: Duration,
    ) -> Result<CodexAppServerPreparation, CodexAppServerError> {
        let mut lease = self
            .acquire(task_id, workspace, cli_path, config_path, startup_timeout)
            .await?;
        let preparation = CodexAppServerPreparation {
            slot: lease.slot.clone(),
        };
        lease.mark_reusable();
        Ok(preparation)
    }

    pub(crate) async fn invalidate(&self, task_id: &str) {
        let slot = self.slots.lock().await.remove(task_id);
        if let Some(slot) = slot {
            reclaim_app_server_slot(slot).await;
        }
    }

    /// Reclaim only the exact slot produced by one earlier prepare. A lifecycle reset may already
    /// have removed that slot and a first send may have installed a fresh one under the same task
    /// ID; stale prepare cleanup must never cancel the replacement.
    pub(crate) async fn invalidate_prepared(
        &self,
        task_id: &str,
        preparation: &CodexAppServerPreparation,
    ) {
        let slot = {
            let mut slots = self.slots.lock().await;
            match slots.get(task_id) {
                Some(current) if Arc::ptr_eq(current, &preparation.slot) => slots.remove(task_id),
                _ => None,
            }
        };
        if let Some(slot) = slot {
            reclaim_app_server_slot(slot).await;
        }
    }

    pub(crate) async fn invalidate_all(&self) {
        let slots = {
            let mut slots = self.slots.lock().await;
            slots.drain().map(|(_, slot)| slot).collect::<Vec<_>>()
        };
        for slot in &slots {
            slot.shutdown.cancel();
        }
        futures::future::join_all(slots.into_iter().map(reclaim_app_server_slot)).await;
    }

    #[cfg(test)]
    pub(crate) async fn contains_task(&self, task_id: &str) -> bool {
        self.slots.lock().await.contains_key(task_id)
    }

    /// Signal active leases first, then reclaim every idle or newly released child.
    /// The desktop exit hook places a process-wide bound around this future.
    pub async fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
        self.shutdown.cancel();
        self.invalidate_all().await;
    }
}

async fn reclaim_app_server_slot(slot: Arc<CodexAppServerTaskSlot>) {
    slot.shutdown.cancel();
    let mut state = slot.state.lock().await;
    if let Some(transport) = state.transport.take() {
        transport.shutdown().await;
    }
    state.identity = None;
}

/// Exclusive main-run lease for one task. A lease is broken by default; callers may mark it
/// reusable only after observing a matching `turn/completed` and settling run-local callbacks.
pub(crate) struct CodexAppServerLease {
    state: OwnedMutexGuard<CodexAppServerTaskState>,
    slot: Arc<CodexAppServerTaskSlot>,
    shutdown: CancellationToken,
    reusable: bool,
}

pub(crate) struct CodexAppServerPreparation {
    slot: Arc<CodexAppServerTaskSlot>,
}

impl CodexAppServerLease {
    pub(crate) fn transport_mut(&mut self) -> &mut CodexAppServerTransport {
        self.state
            .transport
            .as_mut()
            .expect("an acquired Codex App Server lease owns a transport")
    }

    pub(crate) fn shutdown_token(&self) -> CancellationToken {
        self.shutdown.clone()
    }

    pub(crate) fn mark_reusable(&mut self) {
        self.reusable = true;
    }
}

impl Drop for CodexAppServerLease {
    fn drop(&mut self) {
        if self.reusable {
            return;
        }
        self.state.identity = None;
        let Some(transport) = self.state.transport.take() else {
            return;
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(transport.shutdown());
        }
        // Without a runtime, `transport` is dropped here and `kill_on_drop` terminates the child.
    }
}

/// An initialized App Server child and its long-lived stdin/stdout brokers.
pub(crate) struct CodexAppServerTransport {
    child: Child,
    writer: Option<mpsc::Sender<Value>>,
    line_events: mpsc::Receiver<CodexAppServerLineEvent>,
    writer_failures: mpsc::UnboundedReceiver<CodexAppServerError>,
    deferred_lines: VecDeque<String>,
    deferred_bytes: usize,
    next_request_id: u64,
    writer_task: Option<JoinHandle<()>>,
    reader_task: Option<JoinHandle<()>>,
    stderr_task: Option<JoinHandle<()>>,
}

impl CodexAppServerTransport {
    fn is_quiescent(&mut self) -> bool {
        if !self.deferred_lines.is_empty() {
            return false;
        }
        match self.writer_failures.try_recv() {
            Ok(_) | Err(mpsc::error::TryRecvError::Disconnected) => return false,
            Err(mpsc::error::TryRecvError::Empty) => {}
        }
        matches!(
            self.line_events.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        )
    }

    pub(crate) async fn connect(
        cli_path: Option<PathBuf>,
        workspace: &Path,
        startup_timeout: Duration,
    ) -> Result<Self, CodexAppServerError> {
        let mut command = codex_app_server_command(cli_path, workspace)
            .map_err(|_| CodexAppServerError::Launch)?;
        let mut child = command.spawn().map_err(|error| {
            tracing::warn!(kind = ?error.kind(), "failed to launch Codex App Server");
            CodexAppServerError::Launch
        })?;
        let stdin = child.stdin.take().ok_or(CodexAppServerError::Launch)?;
        let stdout = child.stdout.take().ok_or(CodexAppServerError::Launch)?;
        let stderr_task = child.stderr.take().map(|mut stderr| {
            tokio::spawn(async move {
                let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
            })
        });

        let (writer, mut writer_rx) = mpsc::channel::<Value>(MAX_WRITER_QUEUE);
        let (writer_failure_tx, writer_failures) = mpsc::unbounded_channel();
        let writer_task = tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(frame) = writer_rx.recv().await {
                if write_value(&mut stdin, &frame).await.is_err() {
                    let _ = writer_failure_tx.send(CodexAppServerError::Stream);
                    break;
                }
            }
        });

        let (line_tx, line_events) = mpsc::channel(MAX_LINE_QUEUE);
        let reader_task = tokio::spawn(async move {
            let mut stdout = BufReader::new(stdout);
            loop {
                let event = match read_bounded_line(&mut stdout, MAX_LINE_BYTES).await {
                    Ok(Some(line)) => CodexAppServerLineEvent::Line(line),
                    Ok(None) => CodexAppServerLineEvent::Eof,
                    Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                        CodexAppServerLineEvent::TooLong
                    }
                    Err(_) => CodexAppServerLineEvent::Error,
                };
                let terminal = !matches!(event, CodexAppServerLineEvent::Line(_));
                if line_tx.send(event).await.is_err() || terminal {
                    break;
                }
            }
        });

        let mut transport = Self {
            child,
            writer: Some(writer),
            line_events,
            writer_failures,
            deferred_lines: VecDeque::new(),
            deferred_bytes: 0,
            next_request_id: 0,
            writer_task: Some(writer_task),
            reader_task: Some(reader_task),
            stderr_task,
        };
        let initialized = transport
            .request(
                "initialize",
                json!({
                    "clientInfo": { "name": "r-code", "version": env!("CARGO_PKG_VERSION") },
                    "capabilities": { "experimentalApi": true }
                }),
                startup_timeout,
            )
            .await;
        if initialized.is_err() || transport.notify("initialized", json!({})).await.is_err() {
            let error = initialized.err().unwrap_or(CodexAppServerError::Stream);
            transport.shutdown().await;
            return Err(error);
        }
        Ok(transport)
    }

    pub(crate) async fn start_run(
        &mut self,
        thread_params: Value,
        input: Value,
        startup_timeout: Duration,
    ) -> Result<(String, String), CodexAppServerError> {
        if self
            .child
            .try_wait()
            .map_err(|_| CodexAppServerError::Stream)?
            .is_some()
        {
            return Err(CodexAppServerError::Stream);
        }
        let thread = self
            .request("thread/start", thread_params, startup_timeout)
            .await?;
        let thread_id = protocol_id(&thread, "/thread/id", "threadId")?;
        let turn = self
            .request(
                "turn/start",
                json!({
                    "threadId": thread_id,
                    "input": input,
                    "summary": "concise",
                }),
                startup_timeout,
            )
            .await?;
        let turn_id = protocol_id(&turn, "/turn/id", "turnId")?;
        Ok((thread_id, turn_id))
    }

    pub(crate) fn writer(&self) -> mpsc::Sender<Value> {
        self.writer
            .as_ref()
            .expect("live App Server transport has a writer")
            .clone()
    }

    pub(crate) fn next_request_id(&mut self) -> Result<u64, CodexAppServerError> {
        allocate_request_id(&mut self.next_request_id)
    }

    /// Return deferred startup notifications before reading fresh frames. Writer failure is folded
    /// into the same event stream so a run owns only one mutable transport receive future.
    pub(crate) async fn next_event(&mut self) -> CodexAppServerLineEvent {
        if let Some(line) = self.deferred_lines.pop_front() {
            self.deferred_bytes = self.deferred_bytes.saturating_sub(line.len());
            return CodexAppServerLineEvent::Line(line);
        }
        tokio::select! {
            failure = self.writer_failures.recv() => match failure {
                Some(_) => CodexAppServerLineEvent::Error,
                None => self.line_events.recv().await.unwrap_or(CodexAppServerLineEvent::Error),
            },
            event = self.line_events.recv() => event.unwrap_or(CodexAppServerLineEvent::Error),
        }
    }

    async fn notify(&self, method: &str, params: Value) -> Result<(), CodexAppServerError> {
        self.send(json!({ "method": method, "params": params }))
            .await
    }

    async fn request(
        &mut self,
        method: &str,
        params: Value,
        startup_timeout: Duration,
    ) -> Result<Value, CodexAppServerError> {
        let request_id = allocate_request_id(&mut self.next_request_id)?;
        self.send(json!({
            "id": request_id,
            "method": method,
            "params": params,
        }))
        .await?;
        self.wait_for_response(request_id, startup_timeout).await
    }

    async fn send(&self, value: Value) -> Result<(), CodexAppServerError> {
        self.writer
            .as_ref()
            .ok_or(CodexAppServerError::Stream)?
            .send(value)
            .await
            .map_err(|_| CodexAppServerError::Stream)
    }

    async fn wait_for_response(
        &mut self,
        expected_id: u64,
        startup_timeout: Duration,
    ) -> Result<Value, CodexAppServerError> {
        let mut deadline = Instant::now() + startup_timeout;
        let mut writer_failures_open = true;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let event = tokio::select! {
                _ = tokio::time::sleep(remaining) => {
                    return Err(CodexAppServerError::StartupTimeout);
                }
                failure = self.writer_failures.recv(), if writer_failures_open => {
                    match failure {
                        Some(failure) => return Err(failure),
                        None => {
                            writer_failures_open = false;
                            continue;
                        }
                    }
                }
                event = self.line_events.recv() => event,
            };
            let Some(event) = event else {
                return Err(CodexAppServerError::Stream);
            };
            let line = match event {
                CodexAppServerLineEvent::Line(line) => line,
                CodexAppServerLineEvent::Eof | CodexAppServerLineEvent::Error => {
                    return Err(CodexAppServerError::Stream)
                }
                CodexAppServerLineEvent::TooLong => return Err(CodexAppServerError::Protocol),
            };
            let value: Value =
                serde_json::from_str(line.trim()).map_err(|_| CodexAppServerError::Protocol)?;
            if recognized_protocol_progress(&value) {
                deadline = Instant::now() + startup_timeout;
            }
            let is_response = value.get("method").is_none()
                && value.get("id").and_then(Value::as_u64) == Some(expected_id);
            if is_response {
                if value.get("error").is_some() {
                    return Err(CodexAppServerError::Protocol);
                }
                return value
                    .get("result")
                    .cloned()
                    .ok_or(CodexAppServerError::Protocol);
            }
            if value.get("method").is_some() {
                if let Some(request_id) = value.get("id") {
                    let method = value
                        .get("method")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    self.send(json!({
                        "id": request_id,
                        "error": {
                            "code": -32601,
                            "message": format!("R-Code does not handle {method} during App Server startup"),
                        }
                    }))
                    .await?;
                } else {
                    self.deferred_bytes = self.deferred_bytes.saturating_add(line.len());
                    if self.deferred_bytes > MAX_DEFERRED_BYTES {
                        return Err(CodexAppServerError::Protocol);
                    }
                    self.deferred_lines.push_back(line);
                }
            }
        }
    }

    pub(crate) async fn shutdown(mut self) {
        self.writer.take();
        // Windows 下 app-server 经 cmd.exe wrapper 启动（npm shim）：单 kill 只杀
        // wrapper，codex node 进程及 MCP/shell 后代成孤儿持锁存活，必须树杀。
        kill_tree(&mut self.child).await;
        let _ = self.child.wait().await;
        join_bounded(self.writer_task.take()).await;
        join_bounded(self.reader_task.take()).await;
        join_bounded(self.stderr_task.take()).await;
    }
}

fn allocate_request_id(next: &mut u64) -> Result<u64, CodexAppServerError> {
    let current = *next;
    *next = next.checked_add(1).ok_or(CodexAppServerError::Protocol)?;
    Ok(current)
}

fn protocol_id(
    result: &Value,
    pointer: &str,
    fallback: &str,
) -> Result<String, CodexAppServerError> {
    result
        .pointer(pointer)
        .or_else(|| result.get(fallback))
        .or_else(|| result.get("id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or(CodexAppServerError::Protocol)
}

pub(crate) fn recognized_protocol_progress(value: &Value) -> bool {
    let Some(method) = value.get("method").and_then(Value::as_str) else {
        return false;
    };
    // M0-02：方法表唯一事实源移至 codex_interaction；本函数保持
    // “识别为协议进度”的旧语义，同时覆盖丰富交互新增的事件方法。
    crate::codex_interaction::is_recognized_protocol_progress(method)
}

async fn join_bounded(task: Option<JoinHandle<()>>) {
    let Some(mut task) = task else {
        return;
    };
    if timeout(SHUTDOWN_JOIN_TIMEOUT, &mut task).await.is_err() {
        task.abort();
    }
}

async fn write_value(
    stdin: &mut tokio::process::ChildStdin,
    value: &Value,
) -> Result<(), CodexAppServerError> {
    let mut payload = serde_json::to_vec(value).map_err(|_| CodexAppServerError::Protocol)?;
    payload.push(b'\n');
    stdin
        .write_all(&payload)
        .await
        .map_err(|_| CodexAppServerError::Stream)?;
    stdin.flush().await.map_err(|_| CodexAppServerError::Stream)
}

async fn read_bounded_line<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    max_bytes: usize,
) -> std::io::Result<Option<String>> {
    let mut buffer = Vec::new();
    loop {
        let chunk = reader.fill_buf().await?;
        let chunk_len = chunk.len();
        if chunk.is_empty() {
            if buffer.is_empty() {
                return Ok(None);
            }
            let line = std::str::from_utf8(&buffer)
                .map_err(|_| invalid_data("JSONL line is not valid UTF-8"))?
                .to_owned();
            return Ok(Some(line));
        }
        if let Some(position) = chunk.iter().position(|byte| *byte == b'\n') {
            if buffer.len().saturating_add(position) > max_bytes {
                reader.consume(position + 1);
                return Err(invalid_data("JSONL line exceeds maximum length"));
            }
            buffer.extend_from_slice(&chunk[..position]);
            reader.consume(position + 1);
            let line = std::str::from_utf8(&buffer)
                .map_err(|_| invalid_data("JSONL line is not valid UTF-8"))?
                .to_owned();
            return Ok(Some(line));
        }
        if buffer.len().saturating_add(chunk_len) > max_bytes {
            reader.consume(chunk_len);
            return Err(invalid_data("JSONL line exceeds maximum length"));
        }
        buffer.extend_from_slice(chunk);
        reader.consume(chunk_len);
    }
}

fn invalid_data(message: &'static str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

fn resolve_identity(
    workspace: &Path,
    cli_path: Option<PathBuf>,
    config_path: &Path,
) -> Result<(CodexAppServerIdentity, PathBuf), CodexAppServerError> {
    let canonical_workspace = workspace
        .canonicalize()
        .map_err(|_| CodexAppServerError::Workspace)?;
    let resolved_cli_path = resolve_cli_path(cli_path)?;
    let config_fingerprint = match std::fs::read(config_path) {
        Ok(content) => format!("blake3:{}", blake3::hash(&content).to_hex()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "missing:v1".to_string(),
        Err(_) => return Err(CodexAppServerError::Config),
    };
    Ok((
        CodexAppServerIdentity {
            canonical_workspace,
            resolved_cli_path: resolved_cli_path.clone(),
            config_fingerprint,
        },
        resolved_cli_path,
    ))
}

fn resolve_cli_path(cli_path: Option<PathBuf>) -> Result<PathBuf, CodexAppServerError> {
    if let Some(path) = cli_path {
        if let Ok(canonical) = path.canonicalize() {
            return Ok(canonical);
        }
        if path.components().count() == 1 {
            return resolve_cli_from_path(path.as_os_str());
        }
        return Err(CodexAppServerError::Cli);
    }
    resolve_cli_from_path(std::ffi::OsStr::new("codex"))
}

fn resolve_cli_from_path(name: &std::ffi::OsStr) -> Result<PathBuf, CodexAppServerError> {
    let search_path = std::env::var_os("PATH").ok_or(CodexAppServerError::Cli)?;
    #[cfg(windows)]
    let names = {
        let stem = name.to_string_lossy();
        if Path::new(name).extension().is_some() {
            vec![stem.into_owned()]
        } else {
            vec![
                format!("{stem}.exe"),
                format!("{stem}.cmd"),
                format!("{stem}.bat"),
                stem.into_owned(),
            ]
        }
    };
    #[cfg(not(windows))]
    let names = vec![name.to_string_lossy().into_owned()];
    for directory in std::env::split_paths(&search_path) {
        for candidate_name in &names {
            let candidate = directory.join(candidate_name);
            if candidate.is_file() {
                return candidate
                    .canonicalize()
                    .map_err(|_| CodexAppServerError::Cli);
            }
        }
    }
    Err(CodexAppServerError::Cli)
}

pub(crate) fn codex_app_server_command(
    cli_path: Option<PathBuf>,
    workspace: &Path,
) -> Result<Command, String> {
    #[cfg(windows)]
    let mut command = match cli_path {
        Some(path)
            if path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("exe")) =>
        {
            let mut command = Command::new(path);
            command.arg("app-server");
            command
        }
        path => {
            let executable = path.unwrap_or_else(|| PathBuf::from("codex"));
            let executable = codex_app_server_launch_path(&executable);
            windows_cmd_safe_path(&executable)?;
            let mut command = Command::new("cmd.exe");
            command
                .args(["/D", "/S", "/C", "call"])
                .arg(executable)
                .arg("app-server");
            command
        }
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut command = Command::new(cli_path.unwrap_or_else(|| PathBuf::from("codex")));
        command.arg("app-server");
        command
    };
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        command.as_std_mut().process_group(0);
    }
    command
        .arg("-c")
        .arg(CODEX_DISABLE_R_CODE_MCP_OVERRIDE)
        .current_dir(workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    crate::rtk::configure_codex_child(&mut command);
    hide_background_console(command.as_std_mut());
    Ok(command)
}

#[cfg(not(windows))]
fn codex_app_server_launch_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

#[cfg(windows)]
fn codex_app_server_launch_path(path: &Path) -> PathBuf {
    let Some(text) = path.to_str() else {
        return path.to_path_buf();
    };
    if text
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(r"\\?\UNC\"))
    {
        return PathBuf::from(format!(r"\\{}", &text[8..]));
    }
    text.strip_prefix(r"\\?\")
        .map(PathBuf::from)
        .unwrap_or_else(|| path.to_path_buf())
}

#[cfg(windows)]
fn windows_cmd_safe_path(path: &Path) -> Result<(), String> {
    let text = path
        .to_str()
        .ok_or_else(|| "Codex CLI path is not valid Unicode".to_string())?;
    if text.chars().any(|character| {
        matches!(
            character,
            '\0' | '\r' | '\n' | '"' | '&' | '|' | '<' | '>' | '^' | '%' | '!'
        )
    }) {
        return Err("Codex CLI path contains unsupported command characters".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    #[test]
    fn synthesized_path_child_applied_to_app_server_command() {
        // R-ENV-01/M2-01.A2：app-server 拉起的子进程 PATH 必须与
        // configure_codex_child 基准一致（RTK 前缀 + 注册表合成基底单次拼装）。
        let directory = tempfile::tempdir().unwrap();
        let cli = directory.path().join("codex-appserver-probe.exe");
        std::fs::write(&cli, b"stub").unwrap();
        let command =
            codex_app_server_command(Some(cli), directory.path()).expect("command must build");
        let mut probe = tokio::process::Command::new("probe");
        crate::rtk::configure_codex_child(&mut probe);
        let expected = probe
            .as_std()
            .get_envs()
            .find_map(|(key, value)| {
                key.eq_ignore_ascii_case("PATH")
                    .then(|| value.map(std::ffi::OsString::from))
                    .flatten()
            })
            .expect("configure_codex_child must set PATH");
        let path_env = command
            .as_std()
            .get_envs()
            .find_map(|(key, value)| {
                key.eq_ignore_ascii_case("PATH")
                    .then(|| value.map(std::ffi::OsString::from))
                    .flatten()
            })
            .expect("app-server child must carry an explicit PATH env");
        assert_eq!(path_env, expected);
    }

    use super::*;

    use tempfile::TempDir;

    const FIXTURE_TIMEOUT: Duration = Duration::from_secs(15);

    struct AppServerFixture {
        directory: TempDir,
        cli_path: PathBuf,
        config_path: PathBuf,
        log_path: PathBuf,
        workspace: PathBuf,
    }

    impl AppServerFixture {
        fn new() -> Option<Self> {
            Self::with_behavior("healthy")
        }

        fn with_behavior(behavior: &str) -> Option<Self> {
            let directory = TempDir::new().expect("create App Server fixture directory");
            let workspace = directory.path().join("workspace");
            std::fs::create_dir_all(&workspace).expect("create fixture workspace");
            let config_path = directory.path().join("config.toml");
            std::fs::write(&config_path, "model = 'fixture'\n").expect("write fixture config");
            let log_path = directory.path().join("app-server.jsonl");
            let node = find_node()?;
            let source = APP_SERVER_FIXTURE_SOURCE
                .replace(
                    "__LOG_PATH__",
                    &serde_json::to_string(&log_path.to_string_lossy()).unwrap(),
                )
                .replace("__BEHAVIOR__", &serde_json::to_string(behavior).unwrap());
            let script_path = directory.path().join("app-server-fixture.js");
            std::fs::write(&script_path, source).expect("write App Server fixture script");
            let syntax = std::process::Command::new(&node)
                .arg("--check")
                .arg(&script_path)
                .output()
                .expect("check App Server fixture syntax");
            assert!(
                syntax.status.success(),
                "fixture syntax error: {}",
                String::from_utf8_lossy(&syntax.stderr)
            );
            let cli_path = write_cli_shim(directory.path(), &node, &script_path);
            Some(Self {
                directory,
                cli_path,
                config_path,
                log_path,
                workspace,
            })
        }

        fn log(&self) -> Vec<Value> {
            let contents = std::fs::read_to_string(&self.log_path).unwrap_or_default();
            contents
                .lines()
                .map(|line| serde_json::from_str(line).expect("fixture log line is valid JSON"))
                .collect()
        }

        fn spawn_count(&self) -> usize {
            self.log()
                .iter()
                .filter(|entry| entry["kind"] == "spawn")
                .count()
        }

        fn alternate_cli_path(&self) -> PathBuf {
            #[cfg(windows)]
            let alternate = self.directory.path().join("codex-fixture-alternate.cmd");
            #[cfg(not(windows))]
            let alternate = self.directory.path().join("codex-fixture-alternate");
            std::fs::copy(&self.cli_path, &alternate).expect("copy alternate CLI fixture");
            alternate
        }

        fn alternate_workspace(&self) -> PathBuf {
            let workspace = self.directory.path().join("workspace-alternate");
            std::fs::create_dir_all(&workspace).expect("create alternate fixture workspace");
            workspace
        }
    }

    const APP_SERVER_FIXTURE_SOURCE: &str = r#"
const fs = require('node:fs');
const readline = require('node:readline');
const logPath = __LOG_PATH__;
const behavior = __BEHAVIOR__;
const append = (value) => fs.appendFileSync(logPath, `${JSON.stringify(value)}\n`);
const send = (value) => process.stdout.write(`${JSON.stringify(value)}\n`);
append({ kind: 'spawn', pid: process.pid, cwd: process.cwd() });
const input = readline.createInterface({ input: process.stdin, crlfDelay: Infinity });
input.on('line', (line) => {
  const message = JSON.parse(line);
  append({ kind: 'message', pid: process.pid, cwd: process.cwd(), message });
  if (message.method === 'initialize') {
    if (behavior === 'malformed_initialize') {
      process.stdout.write('{not-json\n');
      return;
    }
    if (behavior === 'dirty_startup') {
      send({ method: 'thread/started', params: { thread: { id: 'stale-thread' } } });
    }
    send({ id: message.id, result: {} });
  } else if (message.method === 'initialized') {
    if (behavior === 'exit_after_initialized') {
      process.exit(0);
    }
  } else if (message.method === 'thread/start') {
    send({ id: message.id, result: { thread: { id: `thread-${process.pid}-${message.id}` } } });
  } else if (message.method === 'turn/start') {
    send({ id: message.id, result: { turn: { id: `turn-${process.pid}-${message.id}` } } });
    if (behavior === 'runtime_malformed') {
      process.stdout.write('{not-json\n');
    }
  }
});
"#;

    fn find_node() -> Option<PathBuf> {
        let search_path = std::env::var_os("PATH")?;
        #[cfg(windows)]
        let names = ["node.exe", "node.cmd", "node.bat", "node"];
        #[cfg(not(windows))]
        let names = ["node", "node", "node", "node"];
        std::env::split_paths(&search_path)
            .flat_map(|directory| names.map(move |name| directory.join(name)))
            .find(|candidate| candidate.is_file())
            .and_then(|candidate| candidate.canonicalize().ok())
    }

    #[cfg(windows)]
    fn write_cli_shim(directory: &Path, node: &Path, _script: &Path) -> PathBuf {
        let shim = directory.join("codex-fixture.cmd");
        let node = codex_app_server_launch_path(node);
        std::fs::write(
            &shim,
            format!(
                "@echo off\r\n\"{}\" \"%~dp0app-server-fixture.js\"\r\n",
                node.display()
            ),
        )
        .expect("write Windows fixture shim");
        shim
    }

    #[cfg(unix)]
    fn write_cli_shim(directory: &Path, node: &Path, script: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        fn quote(path: &Path) -> String {
            format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
        }

        let shim = directory.join("codex-fixture");
        std::fs::write(
            &shim,
            format!("#!/bin/sh\nexec {} {}\n", quote(node), quote(script)),
        )
        .expect("write Unix fixture shim");
        let mut permissions = std::fs::metadata(&shim).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&shim, permissions).unwrap();
        shim
    }

    async fn acquire_with(
        registry: &CodexAppServerRegistry,
        task_id: &str,
        workspace: &Path,
        cli_path: &Path,
        config_path: &Path,
    ) -> Result<CodexAppServerLease, CodexAppServerError> {
        timeout(
            FIXTURE_TIMEOUT,
            registry.acquire(
                task_id,
                workspace,
                Some(cli_path.to_path_buf()),
                config_path,
                FIXTURE_TIMEOUT,
            ),
        )
        .await
        .expect("registry acquire exceeded the fixture bound")
    }

    async fn acquire(
        registry: &CodexAppServerRegistry,
        task_id: &str,
        fixture: &AppServerFixture,
    ) -> CodexAppServerLease {
        match acquire_with(
            registry,
            task_id,
            &fixture.workspace,
            &fixture.cli_path,
            &fixture.config_path,
        )
        .await
        {
            Ok(lease) => lease,
            Err(error) => panic!(
                "acquire fixture transport: {error:?}; fixture log: {:?}",
                fixture.log()
            ),
        }
    }

    async fn acquire_reusable_with(
        registry: &CodexAppServerRegistry,
        task_id: &str,
        workspace: &Path,
        cli_path: &Path,
        config_path: &Path,
    ) {
        let mut lease = acquire_with(registry, task_id, workspace, cli_path, config_path)
            .await
            .expect("acquire reusable fixture transport");
        lease.mark_reusable();
    }

    async fn shutdown_registry(registry: &CodexAppServerRegistry) {
        timeout(FIXTURE_TIMEOUT, registry.shutdown())
            .await
            .expect("registry shutdown exceeded the fixture bound");
    }

    #[cfg(windows)]
    fn process_exists(process_id: u32) -> bool {
        let filter = format!("PID eq {process_id}");
        let output = std::process::Command::new("tasklist.exe")
            .args(["/FI", &filter, "/FO", "CSV", "/NH"])
            .output()
            .expect("query Windows fixture process");
        String::from_utf8_lossy(&output.stdout).contains(&format!(",\"{process_id}\","))
    }

    #[cfg(unix)]
    fn process_exists(process_id: u32) -> bool {
        // SAFETY: signal zero only checks whether the process exists.
        unsafe { libc::kill(process_id as i32, 0) == 0 }
    }

    async fn wait_for_process_exit(process_id: u32) {
        timeout(FIXTURE_TIMEOUT, async {
            while process_exists(process_id) {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("fixture process {process_id} survived registry shutdown"));
    }

    #[tokio::test]
    async fn healthy_runs_reuse_one_process_and_keep_request_ids_monotonic() {
        let Some(fixture) = AppServerFixture::new() else {
            return;
        };
        let registry = CodexAppServerRegistry::default();

        for run in 0..2 {
            let mut lease = acquire(&registry, "task-reused", &fixture).await;
            let (thread_id, turn_id) = timeout(
                FIXTURE_TIMEOUT,
                lease.transport_mut().start_run(
                    json!({ "cwd": fixture.workspace }),
                    json!([]),
                    FIXTURE_TIMEOUT,
                ),
            )
            .await
            .expect("healthy fixture run exceeded the bound")
            .expect("start healthy fixture run");
            assert!(thread_id.starts_with("thread-"));
            assert!(turn_id.starts_with("turn-"));
            lease.mark_reusable();
            drop(lease);
            assert_eq!(
                fixture
                    .log()
                    .iter()
                    .filter(|entry| entry["kind"] == "spawn")
                    .count(),
                1,
                "healthy run {run} unexpectedly spawned another App Server"
            );
        }

        let log = fixture.log();
        let methods = log
            .iter()
            .filter_map(|entry| entry.pointer("/message/method").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(
            methods,
            [
                "initialize",
                "initialized",
                "thread/start",
                "turn/start",
                "thread/start",
                "turn/start",
            ]
        );
        let request_ids = log
            .iter()
            .filter_map(|entry| entry.pointer("/message/id").and_then(Value::as_u64))
            .collect::<Vec<_>>();
        assert_eq!(request_ids, [0, 1, 2, 3, 4]);

        shutdown_registry(&registry).await;
    }

    #[tokio::test]
    async fn the_same_task_serializes_leases() {
        let Some(fixture) = AppServerFixture::new() else {
            return;
        };
        let registry = Arc::new(CodexAppServerRegistry::default());
        let mut first = acquire(&registry, "task-serial", &fixture).await;

        let registry_for_second = registry.clone();
        let workspace = fixture.workspace.clone();
        let cli_path = fixture.cli_path.clone();
        let config_path = fixture.config_path.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let second = tokio::spawn(async move {
            let _ = started_tx.send(());
            acquire_with(
                &registry_for_second,
                "task-serial",
                &workspace,
                &cli_path,
                &config_path,
            )
            .await
        });
        started_rx.await.unwrap();
        tokio::task::yield_now().await;
        assert!(
            !second.is_finished(),
            "a second lease for the same task bypassed the active lease"
        );

        first.mark_reusable();
        drop(first);
        let mut second = timeout(FIXTURE_TIMEOUT, second)
            .await
            .expect("second lease remained blocked")
            .expect("second lease task panicked")
            .expect("second lease acquisition failed");
        second.mark_reusable();
        drop(second);
        assert_eq!(
            fixture
                .log()
                .iter()
                .filter(|entry| entry["kind"] == "spawn")
                .count(),
            1
        );

        shutdown_registry(&registry).await;
    }

    #[tokio::test]
    async fn different_tasks_never_share_a_transport() {
        let Some(fixture) = AppServerFixture::new() else {
            return;
        };
        let registry = CodexAppServerRegistry::default();

        let mut first = acquire(&registry, "task-a", &fixture).await;
        let mut second = acquire(&registry, "task-b", &fixture).await;
        first.mark_reusable();
        second.mark_reusable();
        drop(first);
        drop(second);

        let log = fixture.log();
        let process_ids = log
            .iter()
            .filter(|entry| entry["kind"] == "spawn")
            .filter_map(|entry| entry["pid"].as_u64())
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(process_ids.len(), 2, "different tasks shared one child");

        shutdown_registry(&registry).await;
    }

    #[tokio::test]
    async fn a_config_content_change_rebuilds_once_under_concurrent_acquire() {
        let Some(fixture) = AppServerFixture::new() else {
            return;
        };
        let registry = CodexAppServerRegistry::default();
        acquire_reusable_with(
            &registry,
            "task-config",
            &fixture.workspace,
            &fixture.cli_path,
            &fixture.config_path,
        )
        .await;
        std::fs::write(&fixture.config_path, "model = 'changed'\n").expect("change fixture config");

        let _ = tokio::join!(
            acquire_reusable_with(
                &registry,
                "task-config",
                &fixture.workspace,
                &fixture.cli_path,
                &fixture.config_path,
            ),
            acquire_reusable_with(
                &registry,
                "task-config",
                &fixture.workspace,
                &fixture.cli_path,
                &fixture.config_path,
            ),
        );

        assert_eq!(fixture.spawn_count(), 2);
        shutdown_registry(&registry).await;
    }

    #[tokio::test]
    async fn a_resolved_cli_change_rebuilds_once_under_concurrent_acquire() {
        let Some(fixture) = AppServerFixture::new() else {
            return;
        };
        let registry = CodexAppServerRegistry::default();
        acquire_reusable_with(
            &registry,
            "task-cli",
            &fixture.workspace,
            &fixture.cli_path,
            &fixture.config_path,
        )
        .await;
        let alternate_cli = fixture.alternate_cli_path();

        let _ = tokio::join!(
            acquire_reusable_with(
                &registry,
                "task-cli",
                &fixture.workspace,
                &alternate_cli,
                &fixture.config_path,
            ),
            acquire_reusable_with(
                &registry,
                "task-cli",
                &fixture.workspace,
                &alternate_cli,
                &fixture.config_path,
            ),
        );

        assert_eq!(fixture.spawn_count(), 2);
        shutdown_registry(&registry).await;
    }

    #[tokio::test]
    async fn a_canonical_workspace_change_rebuilds_once_under_concurrent_acquire() {
        let Some(fixture) = AppServerFixture::new() else {
            return;
        };
        let registry = CodexAppServerRegistry::default();
        acquire_reusable_with(
            &registry,
            "task-workspace",
            &fixture.workspace,
            &fixture.cli_path,
            &fixture.config_path,
        )
        .await;
        let alternate_workspace = fixture.alternate_workspace();

        let _ = tokio::join!(
            acquire_reusable_with(
                &registry,
                "task-workspace",
                &alternate_workspace,
                &fixture.cli_path,
                &fixture.config_path,
            ),
            acquire_reusable_with(
                &registry,
                "task-workspace",
                &alternate_workspace,
                &fixture.cli_path,
                &fixture.config_path,
            ),
        );

        assert_eq!(fixture.spawn_count(), 2);
        let workspaces = fixture
            .log()
            .iter()
            .filter(|entry| entry["kind"] == "spawn")
            .filter_map(|entry| entry["cwd"].as_str())
            .map(PathBuf::from)
            .map(|path| path.canonicalize().expect("canonicalize logged workspace"))
            .collect::<std::collections::HashSet<_>>();
        assert_eq!(workspaces.len(), 2);
        shutdown_registry(&registry).await;
    }

    #[tokio::test]
    async fn a_missing_config_fingerprint_is_stable_and_reusable() {
        let Some(fixture) = AppServerFixture::new() else {
            return;
        };
        std::fs::remove_file(&fixture.config_path).expect("remove fixture config");
        let registry = CodexAppServerRegistry::default();

        let _ = tokio::join!(
            acquire_reusable_with(
                &registry,
                "task-missing-config",
                &fixture.workspace,
                &fixture.cli_path,
                &fixture.config_path,
            ),
            acquire_reusable_with(
                &registry,
                "task-missing-config",
                &fixture.workspace,
                &fixture.cli_path,
                &fixture.config_path,
            ),
        );

        assert_eq!(fixture.spawn_count(), 1);
        shutdown_registry(&registry).await;
    }

    #[tokio::test]
    async fn eof_makes_even_an_explicitly_reusable_lease_rebuild() {
        let Some(fixture) = AppServerFixture::with_behavior("exit_after_initialized") else {
            return;
        };
        let registry = CodexAppServerRegistry::default();
        let mut first = acquire(&registry, "task-eof", &fixture).await;
        let event = timeout(FIXTURE_TIMEOUT, first.transport_mut().next_event())
            .await
            .expect("fixture EOF exceeded the bound");
        assert!(matches!(event, CodexAppServerLineEvent::Eof));
        first.mark_reusable();
        drop(first);

        let mut second = acquire(&registry, "task-eof", &fixture).await;
        second.mark_reusable();
        drop(second);
        assert_eq!(fixture.spawn_count(), 2);
        shutdown_registry(&registry).await;
    }

    #[tokio::test]
    async fn a_deferred_startup_frame_makes_the_transport_non_reusable() {
        let Some(fixture) = AppServerFixture::with_behavior("dirty_startup") else {
            return;
        };
        let registry = CodexAppServerRegistry::default();
        let mut first = acquire(&registry, "task-dirty", &fixture).await;
        assert!(
            !first.transport_mut().is_quiescent(),
            "a deferred startup notification was incorrectly quiescent"
        );
        first.mark_reusable();
        drop(first);

        let mut second = acquire(&registry, "task-dirty", &fixture).await;
        second.mark_reusable();
        drop(second);
        assert_eq!(fixture.spawn_count(), 2);
        shutdown_registry(&registry).await;
    }

    #[tokio::test]
    async fn malformed_protocol_during_initialize_rebuilds_on_the_next_attempt() {
        let Some(fixture) = AppServerFixture::with_behavior("malformed_initialize") else {
            return;
        };
        let registry = CodexAppServerRegistry::default();

        for _ in 0..2 {
            let result = acquire_with(
                &registry,
                "task-malformed-initialize",
                &fixture.workspace,
                &fixture.cli_path,
                &fixture.config_path,
            )
            .await;
            assert!(matches!(result, Err(CodexAppServerError::Protocol)));
        }

        assert_eq!(fixture.spawn_count(), 2);
        shutdown_registry(&registry).await;
    }

    #[tokio::test]
    async fn a_runtime_protocol_failure_dropped_without_reuse_rebuilds_next_round() {
        let Some(fixture) = AppServerFixture::with_behavior("runtime_malformed") else {
            return;
        };
        let registry = CodexAppServerRegistry::default();
        let mut first = acquire(&registry, "task-runtime-protocol", &fixture).await;
        timeout(
            FIXTURE_TIMEOUT,
            first.transport_mut().start_run(
                json!({ "cwd": fixture.workspace }),
                json!([]),
                FIXTURE_TIMEOUT,
            ),
        )
        .await
        .expect("runtime fixture setup exceeded the bound")
        .expect("start runtime protocol fixture");
        let line = timeout(FIXTURE_TIMEOUT, first.transport_mut().next_event())
            .await
            .expect("runtime malformed frame exceeded the bound");
        let CodexAppServerLineEvent::Line(line) = line else {
            panic!("expected a malformed line frame");
        };
        assert!(serde_json::from_str::<Value>(&line).is_err());
        drop(first);

        let mut second = acquire(&registry, "task-runtime-protocol", &fixture).await;
        second.mark_reusable();
        drop(second);
        assert_eq!(fixture.spawn_count(), 2);
        shutdown_registry(&registry).await;
    }

    #[tokio::test]
    async fn shutdown_interrupts_an_active_lease_and_same_task_waiter_then_reaps_the_child() {
        let Some(fixture) = AppServerFixture::new() else {
            return;
        };
        let registry = Arc::new(CodexAppServerRegistry::default());
        let mut active = acquire(&registry, "task-shutdown", &fixture).await;
        let child_process_id = active
            .transport_mut()
            .child
            .id()
            .expect("fixture child has a process id");
        let node_process_id = fixture
            .log()
            .iter()
            .find(|entry| entry["kind"] == "spawn")
            .and_then(|entry| entry["pid"].as_u64())
            .expect("fixture logged its Node process id") as u32;
        let active_shutdown = active.shutdown_token();

        let waiting_registry = registry.clone();
        let workspace = fixture.workspace.clone();
        let cli_path = fixture.cli_path.clone();
        let config_path = fixture.config_path.clone();
        let (waiting_tx, waiting_rx) = tokio::sync::oneshot::channel();
        let waiting = tokio::spawn(async move {
            let _ = waiting_tx.send(());
            acquire_with(
                &waiting_registry,
                "task-shutdown",
                &workspace,
                &cli_path,
                &config_path,
            )
            .await
        });
        waiting_rx.await.unwrap();
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished());

        let shutdown_registry = registry.clone();
        let shutdown = tokio::spawn(async move { shutdown_registry.shutdown().await });
        timeout(FIXTURE_TIMEOUT, active_shutdown.cancelled())
            .await
            .expect("active lease did not observe registry shutdown");
        let waiting = timeout(FIXTURE_TIMEOUT, waiting)
            .await
            .expect("waiting acquire ignored registry shutdown")
            .expect("waiting acquire task panicked");
        assert!(matches!(waiting, Err(CodexAppServerError::Stream)));
        assert!(
            !shutdown.is_finished(),
            "registry shutdown did not wait for the active lease"
        );

        active.mark_reusable();
        drop(active);
        timeout(FIXTURE_TIMEOUT, shutdown)
            .await
            .expect("registry shutdown remained blocked")
            .expect("registry shutdown task panicked");
        wait_for_process_exit(child_process_id).await;
        wait_for_process_exit(node_process_id).await;
    }

    #[tokio::test]
    async fn prepare_initializes_without_creating_a_thread_or_turn_and_first_run_reuses_it() {
        let Some(fixture) = AppServerFixture::new() else {
            return;
        };
        let registry = CodexAppServerRegistry::default();

        timeout(
            FIXTURE_TIMEOUT,
            registry.prepare(
                "task-prepared",
                &fixture.workspace,
                Some(fixture.cli_path.clone()),
                &fixture.config_path,
                FIXTURE_TIMEOUT,
            ),
        )
        .await
        .expect("prepare exceeded the fixture bound")
        .expect("prepare fixture transport");

        timeout(FIXTURE_TIMEOUT, async {
            while !fixture.log().iter().any(|entry| {
                entry.pointer("/message/method").and_then(Value::as_str) == Some("initialized")
            }) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("initialized notification was not written");
        let prepared_log = fixture.log();
        let prepared_methods = prepared_log
            .iter()
            .filter_map(|entry| entry.pointer("/message/method").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(prepared_methods, ["initialize", "initialized"]);
        assert_eq!(fixture.spawn_count(), 1);

        let mut lease = acquire(&registry, "task-prepared", &fixture).await;
        timeout(
            FIXTURE_TIMEOUT,
            lease.transport_mut().start_run(
                json!({ "cwd": fixture.workspace }),
                json!([]),
                FIXTURE_TIMEOUT,
            ),
        )
        .await
        .expect("prepared first run exceeded the fixture bound")
        .expect("start first run on prepared transport");
        lease.mark_reusable();
        drop(lease);
        assert_eq!(fixture.spawn_count(), 1);
        shutdown_registry(&registry).await;
    }

    #[tokio::test]
    async fn concurrent_prepare_and_first_acquire_share_one_initialization() {
        let Some(fixture) = AppServerFixture::new() else {
            return;
        };
        let registry = Arc::new(CodexAppServerRegistry::default());
        let mut contenders = Vec::new();
        for index in 0..8 {
            let registry = registry.clone();
            let workspace = fixture.workspace.clone();
            let cli_path = fixture.cli_path.clone();
            let config_path = fixture.config_path.clone();
            contenders.push(tokio::spawn(async move {
                if index % 2 == 0 {
                    registry
                        .prepare(
                            "task-single-flight",
                            &workspace,
                            Some(cli_path),
                            &config_path,
                            FIXTURE_TIMEOUT,
                        )
                        .await
                } else {
                    acquire_reusable_with(
                        &registry,
                        "task-single-flight",
                        &workspace,
                        &cli_path,
                        &config_path,
                    )
                    .await;
                    Ok(())
                }
            }));
        }
        timeout(FIXTURE_TIMEOUT, futures::future::join_all(contenders))
            .await
            .expect("single-flight contenders exceeded the fixture bound")
            .into_iter()
            .for_each(|result| {
                result
                    .expect("single-flight contender panicked")
                    .expect("single-flight contender failed")
            });

        assert_eq!(fixture.spawn_count(), 1);
        assert_eq!(
            fixture
                .log()
                .iter()
                .filter(|entry| entry.pointer("/message/method") == Some(&json!("initialize")))
                .count(),
            1
        );
        shutdown_registry(&registry).await;
    }

    #[tokio::test]
    async fn invalidate_interrupts_the_task_slot_and_reaps_its_prepared_child() {
        let Some(fixture) = AppServerFixture::new() else {
            return;
        };
        let registry = Arc::new(CodexAppServerRegistry::default());
        let mut active = acquire(&registry, "task-invalidated", &fixture).await;
        let child_process_id = active
            .transport_mut()
            .child
            .id()
            .expect("fixture child has a process id");
        let node_process_id = fixture
            .log()
            .iter()
            .find(|entry| entry["kind"] == "spawn")
            .and_then(|entry| entry["pid"].as_u64())
            .expect("fixture logged its Node process id") as u32;
        let active_shutdown = active.shutdown_token();

        let waiting_registry = registry.clone();
        let workspace = fixture.workspace.clone();
        let cli_path = fixture.cli_path.clone();
        let config_path = fixture.config_path.clone();
        let waiting = tokio::spawn(async move {
            acquire_with(
                &waiting_registry,
                "task-invalidated",
                &workspace,
                &cli_path,
                &config_path,
            )
            .await
        });
        tokio::task::yield_now().await;

        let invalidating_registry = registry.clone();
        let invalidating =
            tokio::spawn(async move { invalidating_registry.invalidate("task-invalidated").await });
        timeout(FIXTURE_TIMEOUT, active_shutdown.cancelled())
            .await
            .expect("active lease did not observe task invalidation");
        let waiting = timeout(FIXTURE_TIMEOUT, waiting)
            .await
            .expect("waiting acquire ignored task invalidation")
            .expect("waiting acquire task panicked");
        assert!(matches!(waiting, Err(CodexAppServerError::Stream)));
        assert!(!invalidating.is_finished());

        active.mark_reusable();
        drop(active);
        timeout(FIXTURE_TIMEOUT, invalidating)
            .await
            .expect("task invalidation remained blocked")
            .expect("task invalidation task panicked");
        wait_for_process_exit(child_process_id).await;
        wait_for_process_exit(node_process_id).await;
        shutdown_registry(&registry).await;
    }

    #[tokio::test]
    async fn stale_prepare_cleanup_never_invalidates_a_replacement_slot() {
        let Some(fixture) = AppServerFixture::new() else {
            return;
        };
        let registry = CodexAppServerRegistry::default();
        let stale = timeout(
            FIXTURE_TIMEOUT,
            registry.prepare_tracked(
                "task-prepare-generation",
                &fixture.workspace,
                Some(fixture.cli_path.clone()),
                &fixture.config_path,
                FIXTURE_TIMEOUT,
            ),
        )
        .await
        .expect("tracked prepare exceeded the fixture bound")
        .expect("tracked prepare failed");

        registry.invalidate("task-prepare-generation").await;
        let mut replacement = acquire(&registry, "task-prepare-generation", &fixture).await;
        let replacement_shutdown = replacement.shutdown_token();

        registry
            .invalidate_prepared("task-prepare-generation", &stale)
            .await;
        assert!(registry.contains_task("task-prepare-generation").await);
        assert!(
            !replacement_shutdown.is_cancelled(),
            "cleanup from an old branch prepare cancelled the replacement transport"
        );

        replacement.mark_reusable();
        drop(replacement);
        assert_eq!(fixture.spawn_count(), 2);
        shutdown_registry(&registry).await;
    }

    #[tokio::test]
    async fn invalidate_all_reaps_every_prepared_task_transport() {
        let Some(fixture) = AppServerFixture::new() else {
            return;
        };
        let registry = CodexAppServerRegistry::default();
        for task_id in ["task-settings-a", "task-settings-b"] {
            timeout(
                FIXTURE_TIMEOUT,
                registry.prepare(
                    task_id,
                    &fixture.workspace,
                    Some(fixture.cli_path.clone()),
                    &fixture.config_path,
                    FIXTURE_TIMEOUT,
                ),
            )
            .await
            .expect("settings prepare exceeded the fixture bound")
            .expect("prepare settings fixture transport");
        }
        let process_ids = fixture
            .log()
            .iter()
            .filter(|entry| entry["kind"] == "spawn")
            .filter_map(|entry| entry["pid"].as_u64())
            .map(|process_id| process_id as u32)
            .collect::<Vec<_>>();
        assert_eq!(process_ids.len(), 2);

        timeout(FIXTURE_TIMEOUT, registry.invalidate_all())
            .await
            .expect("global invalidation exceeded the fixture bound");
        assert!(!registry.contains_task("task-settings-a").await);
        assert!(!registry.contains_task("task-settings-b").await);
        for process_id in process_ids {
            wait_for_process_exit(process_id).await;
        }
        shutdown_registry(&registry).await;
    }
}
