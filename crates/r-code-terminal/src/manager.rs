//! PTY Management — 多终端 PTY 管理 [doc-03 §2]
//!
//! 使用 `portable-pty` crate 管理多个 PTY 终端实例。
//! 每个终端拥有独立的子进程、读写管道和 scrollback 缓冲区。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex as StdMutex,
};
use tokio::sync::{broadcast, Mutex};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize, PtySystem};
use r_code_core::error::ProductError;

use crate::block::BlockParser;
use crate::cli_detector::{CliDetector, ExternalCli};
use crate::shell_integration::{shell_integration_spawn, ShellIntegrationConfig};

/// Scrollback 缓冲区最大大小（约 200KB）。
const MAX_SCROLLBACK: usize = 200_000;

/// Reader 线程与异步管理器之间的有界交接缓冲。
///
/// PTY 可能在终端面板未挂载时持续高速输出，因此不能用无界 channel 暂存字节。
/// Reader 直接把最新尾部写入这里；`received_cursor` 保留绝对位置，让渲染器在
/// 旧内容被裁剪后能通过 `reset` 安全恢复。
#[derive(Debug, Default)]
struct PendingOutput {
    bytes: Vec<u8>,
    received_cursor: u64,
    disconnected: bool,
}

impl PendingOutput {
    fn append(&mut self, bytes: &[u8]) {
        self.received_cursor = self.received_cursor.saturating_add(bytes.len() as u64);
        self.bytes.extend_from_slice(bytes);
        trim_scrollback(&mut self.bytes);
    }
}

/// 终端 ID 类型。
pub type TerminalId = String;

/// 终端渲染器用的完整原始快照。
///
/// 普通 `terminal.read` 故意仍然返回去除了 ANSI 的文本，以便 agent 安全地读取；
/// 只有桌面渲染器使用本类型将控制序列交给真正的终端模拟器解释。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TerminalRawSnapshot {
    pub output: String,
    /// 原始输出的绝对尾部游标。后续增量读取必须原样传回。
    pub cursor: u64,
}

/// 自某个原始输出游标以来的终端更新。
#[derive(Debug, Clone, serde::Serialize)]
pub struct TerminalRawBatch {
    pub output: String,
    pub cursor: u64,
    /// 请求的游标已经落在滚动缓冲区之前时为 true；渲染器应 reset 后重放 output。
    pub reset: bool,
}

/// 终端状态。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum TerminalState {
    /// Shell 正在运行，等待输入
    Idle,
    /// 命令正在运行
    Busy,
    /// 检测到外部 CLI agent（claude/codex）
    Agent,
    /// 终端已退出
    Exited,
}

/// 一个受管理的终端实例。
pub struct TerminalHandle {
    pub id: TerminalId,
    pub state: TerminalState,
    pub shell: String,
    pub working_dir: PathBuf,
    /// Scrollback 缓冲区（ANSI-stripped，约 200KB 上限）
    pub scrollback: Vec<u8>,
    /// 原始 PTY 字节流的 scrollback，供受控的本地终端模拟器恢复画面。
    raw_scrollback: Vec<u8>,
    /// 原始 PTY 字节流已接收的总字节数；即使 scrollback 截断也单调递增。
    raw_output_cursor: u64,
    /// PTY 子进程
    child: Box<dyn Child + Send + Sync>,
    /// PTY stdin 写入器
    writer: Box<dyn std::io::Write + Send>,
    /// PTY master（用于 resize 和 reader 克隆）
    master: Box<dyn MasterPty + Send>,
    /// 后台读取线程的有界输出交接缓冲。
    pending_output: Arc<StdMutex<PendingOutput>>,
    /// 合并尚未被任一消费者排空的输出通知，避免高吞吐命令淹没 WebView 事件队列。
    output_signal_pending: Arc<AtomicBool>,
    /// 是否已被显式 kill（防止 Drop 重复 kill 导致 PID 复用风险）
    killed: bool,
    /// OSC 133 块解析器（用于追踪命令退出码）[doc-03 §4]
    block_parser: BlockParser,
    /// 最后观测到的命令退出码
    last_exit_code: Option<i32>,
    /// 退出码版本号（每次新观测到退出码时递增）
    exit_code_version: u64,
    /// shell integration 为本终端建立的临时 profile shim。
    integration_dir: Option<PathBuf>,
}

impl std::fmt::Debug for TerminalHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalHandle")
            .field("id", &self.id)
            .field("state", &self.state)
            .field("shell", &self.shell)
            .field("working_dir", &self.working_dir)
            .field("scrollback_len", &self.scrollback.len())
            .field("raw_scrollback_len", &self.raw_scrollback.len())
            .finish()
    }
}

impl Drop for TerminalHandle {
    fn drop(&mut self) {
        if !self.killed {
            // 确保子进程被终止和回收，避免孤儿进程
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(path) = self.integration_dir.take() {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

/// TerminalManager — 管理多个 PTY 终端。
pub struct TerminalManager {
    pty_system: std::sync::Mutex<Box<dyn PtySystem + Send>>,
    terminals: Arc<Mutex<HashMap<TerminalId, TerminalHandle>>>,
    output_events: broadcast::Sender<TerminalId>,
}

impl TerminalManager {
    pub fn new() -> Self {
        let (output_events, _) = broadcast::channel(256);
        Self {
            pty_system: std::sync::Mutex::new(native_pty_system()),
            terminals: Arc::new(Mutex::new(HashMap::new())),
            output_events,
        }
    }

    /// 订阅“某终端已有新 PTY 输出”的轻量通知。
    ///
    /// 通知只负责唤醒渲染器；原始字节仍通过 [`Self::raw_since`] 的有界 scrollback
    /// 读取，因此慢消费者不会丢失内容，也不会让 PTY reader 阻塞在 WebView 上。
    pub fn subscribe_output(&self) -> broadcast::Receiver<TerminalId> {
        self.output_events.subscribe()
    }

    /// 创建一个新终端。
    pub async fn create(
        &self,
        shell: &str,
        working_dir: &Path,
        env: Vec<(String, String)>,
    ) -> Result<TerminalId, ProductError> {
        self.create_with_args(shell, working_dir, env, Vec::new())
            .await
    }

    /// 创建带固定启动参数的终端。只供宿主为受信任的内置入口（如 Codex CLI）使用；
    /// WebView 不能直接提供这些参数。
    pub async fn create_with_args(
        &self,
        shell: &str,
        working_dir: &Path,
        env: Vec<(String, String)>,
        initial_args: Vec<String>,
    ) -> Result<TerminalId, ProductError> {
        let id = uuid::Uuid::new_v4().to_string();
        let shell = resolve_shell(shell)?;
        // `Path::canonicalize` uses Win32's verbatim form (`\\?\C:\...`) on Windows.
        // That spelling is useful for containment checks, but PowerShell exposes it as a
        // provider-qualified location and then fails to resolve ordinary relative `cd` paths.
        // Keep the canonical path inside the security boundary and hand the interactive shell
        // its equivalent DOS/UNC spelling only at the process-launch boundary.
        let launch_working_dir = shell_working_dir(working_dir);

        let pair = {
            let pty_system = self.pty_system.lock().expect("pty_system mutex poisoned");
            pty_system
                .openpty(PtySize::default())
                .map_err(|e| ProductError::TerminalError(format!("openpty failed: {e}")))?
        };

        let integration = shell_integration_spawn(&ShellIntegrationConfig {
            shell: shell.clone(),
            working_dir: launch_working_dir.clone(),
            enabled: true,
        });

        let mut cmd = CommandBuilder::new(&shell);
        cmd.cwd(&launch_working_dir);
        for arg in &initial_args {
            cmd.arg(arg);
        }
        for arg in &integration.args {
            cmd.arg(arg);
        }
        for (k, v) in env.iter().chain(integration.env.iter()) {
            cmd.env(k, v);
        }
        // 仅作为终端身份标记；不携带 token、凭据或宿主控制通道。
        cmd.env("R_CODE_TERMINAL", "1");

        let mut child = match pair.slave.spawn_command(cmd) {
            Ok(child) => child,
            Err(err) => {
                cleanup_integration_dir(integration.cleanup_dir.as_deref());
                return Err(ProductError::TerminalError(format!("spawn failed: {err}")));
            }
        };

        let writer = match pair.master.take_writer() {
            Ok(writer) => writer,
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                cleanup_integration_dir(integration.cleanup_dir.as_deref());
                return Err(ProductError::TerminalError(format!(
                    "take_writer failed: {err}"
                )));
            }
        };

        let reader = match pair.master.try_clone_reader() {
            Ok(reader) => reader,
            Err(err) => {
                let _ = child.kill();
                let _ = child.wait();
                cleanup_integration_dir(integration.cleanup_dir.as_deref());
                return Err(ProductError::TerminalError(format!(
                    "try_clone_reader failed: {err}"
                )));
            }
        };

        // 丢弃 slave — 关闭我们的 slave 引用，使子进程退出时 master reader 能收到 EOF
        drop(pair.slave);
        let master = pair.master;

        // 后台线程持续读取 PTY 输出，直接写入固定上限的共享尾部缓冲。
        // 这保证即使终端页未挂载、没有消费者排空，内存也不会随输出无限增长。
        let pending_output = Arc::new(StdMutex::new(PendingOutput::default()));
        let reader_output = pending_output.clone();
        let output_events = self.output_events.clone();
        let output_terminal_id = id.clone();
        let output_signal_pending = Arc::new(AtomicBool::new(false));
        let reader_signal_pending = output_signal_pending.clone();
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        {
                            let mut output = reader_output
                                .lock()
                                .expect("terminal pending output mutex poisoned");
                            output.append(&buf[..n]);
                        }
                        if !reader_signal_pending.swap(true, Ordering::AcqRel)
                            && output_events.send(output_terminal_id.clone()).is_err()
                        {
                            // 没有订阅者时不要永久保持 pending；稍后建立的订阅者会
                            // 先读取快照，后续输出仍应能够再次发出通知。
                            reader_signal_pending.store(false, Ordering::Release);
                        }
                    }
                    Err(_) => break,
                }
            }
            if let Ok(mut output) = reader_output.lock() {
                output.disconnected = true;
            }
            if !reader_signal_pending.swap(true, Ordering::AcqRel)
                && output_events.send(output_terminal_id).is_err()
            {
                reader_signal_pending.store(false, Ordering::Release);
            }
        });

        let handle = TerminalHandle {
            id: id.clone(),
            state: TerminalState::Idle,
            shell,
            working_dir: launch_working_dir,
            scrollback: Vec::new(),
            raw_scrollback: Vec::new(),
            raw_output_cursor: 0,
            child,
            writer,
            master,
            pending_output,
            output_signal_pending,
            killed: false,
            block_parser: BlockParser::new(),
            last_exit_code: None,
            exit_code_version: 0,
            integration_dir: integration.cleanup_dir,
        };

        self.terminals.lock().await.insert(id.clone(), handle);

        Ok(id)
    }

    /// 向终端发送输入。
    pub async fn send(&self, id: &str, input: &str) -> Result<(), ProductError> {
        let mut terminals = self.terminals.lock().await;
        let handle = terminals
            .get_mut(id)
            .ok_or_else(|| ProductError::TerminalError(format!("terminal not found: {id}")))?;

        handle
            .writer
            .write_all(input.as_bytes())
            .map_err(|e| ProductError::TerminalError(format!("write failed: {e}")))?;
        handle
            .writer
            .flush()
            .map_err(|e| ProductError::TerminalError(format!("flush failed: {e}")))?;
        Ok(())
    }

    /// 从终端读取输出（非阻塞，返回当前可用的数据）。
    ///
    /// 返回自上次 `read` 以来的新输出（ANSI-stripped），同时追加到 scrollback。
    pub async fn read(&self, id: &str) -> Result<Vec<u8>, ProductError> {
        let mut terminals = self.terminals.lock().await;
        let handle = terminals
            .get_mut(id)
            .ok_or_else(|| ProductError::TerminalError(format!("terminal not found: {id}")))?;

        Self::drain_output(handle)
    }

    /// 获取终端的完整可见输出快照。
    ///
    /// 此接口会先排空尚未读取的 PTY 输出，再返回受大小上限保护的 scrollback。它
    /// 专供 UI 重新挂载、切换终端及与 agent 共用终端时恢复画面；`read` 仍维持
    /// 增量读取语义，避免破坏工具调用方。
    pub async fn snapshot(&self, id: &str) -> Result<Vec<u8>, ProductError> {
        let mut terminals = self.terminals.lock().await;
        let handle = terminals
            .get_mut(id)
            .ok_or_else(|| ProductError::TerminalError(format!("terminal not found: {id}")))?;

        Self::drain_output(handle)?;
        Ok(handle.scrollback.clone())
    }

    /// 获取完整原始终端画面及其尾部游标。
    ///
    /// 这条通道只供本机 UI 的终端模拟器使用。Agent 和 MCP 工具仍应使用
    /// [`Self::read`]，后者保持 ANSI-free 的文本契约。
    pub async fn raw_snapshot(&self, id: &str) -> Result<TerminalRawSnapshot, ProductError> {
        let mut terminals = self.terminals.lock().await;
        let handle = terminals
            .get_mut(id)
            .ok_or_else(|| ProductError::TerminalError(format!("terminal not found: {id}")))?;

        Self::drain_output(handle)?;
        Ok(TerminalRawSnapshot {
            output: String::from_utf8_lossy(&handle.raw_scrollback).to_string(),
            cursor: handle.raw_output_cursor,
        })
    }

    /// 获取某个渲染游标之后的原始终端输出。
    ///
    /// 由于 agent 读取和 UI 读取共用 PTY，增量不能直接依赖 receiver 的“谁先读到”
    /// 语义。这里以滚动缓冲区和绝对游标为真源，因此即使 agent 先消费了输出，UI
    /// 仍能补回缺失内容；缓冲区滚出时返回 `reset = true` 让模拟器安全重放快照。
    pub async fn raw_since(&self, id: &str, cursor: u64) -> Result<TerminalRawBatch, ProductError> {
        let mut terminals = self.terminals.lock().await;
        let handle = terminals
            .get_mut(id)
            .ok_or_else(|| ProductError::TerminalError(format!("terminal not found: {id}")))?;

        Self::drain_output(handle)?;
        let end = handle.raw_output_cursor;
        let start = end.saturating_sub(handle.raw_scrollback.len() as u64);
        let reset = cursor < start || cursor > end;
        let output = if reset {
            handle.raw_scrollback.clone()
        } else {
            let offset = (cursor - start) as usize;
            handle.raw_scrollback[offset..].to_vec()
        };

        Ok(TerminalRawBatch {
            output: String::from_utf8_lossy(&output).to_string(),
            cursor: end,
            reset,
        })
    }

    fn drain_output(handle: &mut TerminalHandle) -> Result<Vec<u8>, ProductError> {
        // 先允许 reader 发下一次通知，再排空当前队列。若 reader 恰好在排空期间写入，
        // 最多产生一次无内容的冗余唤醒，不会出现“新字节到了但没有事件”的丢失窗口。
        handle.output_signal_pending.store(false, Ordering::Release);
        // 原子取走当前尾部。Reader 已在写入时裁剪，因此这里的峰值始终有界。
        let (raw_data, received_cursor, disconnected) = {
            let mut output = handle
                .pending_output
                .lock()
                .expect("terminal pending output mutex poisoned");
            (
                std::mem::take(&mut output.bytes),
                output.received_cursor,
                output.disconnected,
            )
        };

        // Feed raw data to block parser for OSC 133 exit code tracking [doc-03 §4]
        if !raw_data.is_empty() {
            for code in handle.block_parser.feed(&raw_data) {
                handle.last_exit_code = Some(code);
                handle.exit_code_version = handle.exit_code_version.wrapping_add(1);
            }

            // 保持两份 scrollback：ANSI-free 文本供 agent 读取；原始字节流只给
            // 受控桌面模拟器渲染，避免 UI 因丢失 cursor/colour 序列退化为日志框。
            // received_cursor 包含交接缓冲被裁掉的字节，因此游标不能只累加
            // raw_data.len()；否则慢消费者无法识别需要 reset。
            handle.raw_output_cursor = received_cursor;
            handle.raw_scrollback.extend_from_slice(&raw_data);
            trim_scrollback(&mut handle.raw_scrollback);

            Self::refresh_state_from_shell_markers(handle);
        }

        if disconnected {
            handle.state = TerminalState::Exited;
        }

        // 去除 ANSI 转义序列
        let stripped = strip_ansi(&raw_data);

        // 追加到 scrollback
        handle.scrollback.extend_from_slice(&stripped);

        // 超出上限时截断最旧的数据
        trim_scrollback(&mut handle.scrollback);

        // 检查子进程是否已退出
        if handle.state != TerminalState::Exited
            && handle
                .child
                .try_wait()
                .map_err(|e| ProductError::TerminalError(format!("try_wait failed: {e}")))?
                .is_some()
        {
            handle.state = TerminalState::Exited;
        }

        Ok(stripped)
    }

    /// 依据 shell integration 的 OSC 133 边界更新活动状态。
    ///
    /// 不存在 shell integration 时保守地保留 Idle，而不是把一次键盘输入误报为
    /// "正在执行"。当已知 CLI（Codex/Claude）正处于命令输出区时，明确标为
    /// Agent；其他命令为 Busy。
    fn refresh_state_from_shell_markers(handle: &mut TerminalHandle) {
        if handle.state == TerminalState::Exited {
            return;
        }

        if !handle.block_parser.command_is_running() {
            handle.state = TerminalState::Idle;
            return;
        }

        handle.state = match handle.block_parser.running_command() {
            Some(command)
                if matches!(
                    CliDetector::detect(command),
                    ExternalCli::Claude | ExternalCli::Codex
                ) =>
            {
                TerminalState::Agent
            }
            _ => TerminalState::Busy,
        };
    }

    /// 终止一个终端。
    /// 有界终止全部终端（应用退出路径，F-robust-08）。ConPTY 句柄随进程关闭
    /// 通常会令附着进程退出，但那是 OS 行为而非合同保证；显式 kill+收尸，
    /// 消灭 shell 内仍在运行的孙进程（如 cargo build）。
    pub async fn kill_all(&self) {
        let ids: Vec<TerminalId> = self.terminals.lock().await.keys().cloned().collect();
        for id in ids {
            if let Err(error) = self.kill(&id).await {
                tracing::warn!(terminal_id = %id, %error, "terminal kill_all: kill failed");
            }
        }
    }

    pub async fn kill(&self, id: &str) -> Result<(), ProductError> {
        let mut terminals = self.terminals.lock().await;
        let mut handle = terminals
            .remove(id)
            .ok_or_else(|| ProductError::TerminalError(format!("terminal not found: {id}")))?;

        handle
            .child
            .kill()
            .map_err(|e| ProductError::TerminalError(format!("kill failed: {e}")))?;
        // 回收子进程，避免僵尸
        let _ = handle.child.wait();
        handle.killed = true;
        Ok(())
    }

    /// 列出所有终端。
    pub async fn list(&self) -> Vec<(TerminalId, TerminalState)> {
        let mut terminals = self.terminals.lock().await;
        for handle in terminals.values_mut() {
            // 状态/scrollback 由 PTY 输出驱动；列表刷新也应推进它们，不能只在
            // 某个消费者显式 read 时才发现进程已退出。
            let _ = Self::drain_output(handle);
        }
        terminals
            .values()
            .map(|h| (h.id.clone(), h.state.clone()))
            .collect()
    }

    /// 调整终端大小。
    pub async fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<(), ProductError> {
        let terminals = self.terminals.lock().await;
        let handle = terminals
            .get(id)
            .ok_or_else(|| ProductError::TerminalError(format!("terminal not found: {id}")))?;

        handle
            .master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| ProductError::TerminalError(format!("resize failed: {e}")))?;
        Ok(())
    }

    /// 获取终端状态。
    pub async fn get_state(&self, id: &str) -> Result<TerminalState, ProductError> {
        let mut terminals = self.terminals.lock().await;
        let handle = terminals
            .get_mut(id)
            .ok_or_else(|| ProductError::TerminalError(format!("terminal not found: {id}")))?;

        Self::drain_output(handle)?;

        Ok(handle.state.clone())
    }

    /// 获取终端的退出码状态 (version, last_exit_code)。
    /// version 每次观测到新退出码时递增；调用方应先调用 `read` 排空通道。
    pub async fn exit_code_status(&self, id: &str) -> Result<(u64, Option<i32>), ProductError> {
        let terminals = self.terminals.lock().await;
        let handle = terminals
            .get(id)
            .ok_or_else(|| ProductError::TerminalError(format!("terminal not found: {id}")))?;
        Ok((handle.exit_code_version, handle.last_exit_code))
    }

    /// 列出所有终端（含 shell 路径），用于 terminal.list [doc-03 §8]。
    pub async fn list_with_shell(&self) -> Vec<(TerminalId, TerminalState, String)> {
        let mut terminals = self.terminals.lock().await;
        for handle in terminals.values_mut() {
            let _ = Self::drain_output(handle);
        }
        terminals
            .values()
            .map(|h| (h.id.clone(), h.state.clone(), h.shell.clone()))
            .collect()
    }
}

/// 将 scrollback 截到固定上限，保留最新部分。
fn trim_scrollback(buffer: &mut Vec<u8>) {
    if buffer.len() > MAX_SCROLLBACK {
        let excess = buffer.len() - MAX_SCROLLBACK;
        buffer.drain(..excess);
    }
}

fn cleanup_integration_dir(path: Option<&Path>) {
    if let Some(path) = path {
        let _ = std::fs::remove_dir_all(path);
    }
}

/// Convert Windows' canonical verbatim spelling into the path form expected by interactive
/// shells. Security checks continue to use the canonical path before this launch-only step.
fn shell_working_dir(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let rendered = path.to_string_lossy();
        if let Some(rest) = rendered.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = rendered.strip_prefix(r"\\?\") {
            let bytes = rest.as_bytes();
            if bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && matches!(bytes[2], b'\\' | b'/')
            {
                return PathBuf::from(rest);
            }
        }
    }
    path.to_path_buf()
}

impl Default for TerminalManager {
    fn default() -> Self {
        Self::new()
    }
}

/// 解析 UI 请求的 shell。
///
/// Windows 机器不一定装有 PowerShell 7；前端传入 `auto` 时优先使用 pwsh，再回退
/// 到系统自带的 powershell.exe，避免终端创建成功但子进程无法启动的空白体验。
fn resolve_shell(shell: &str) -> Result<String, ProductError> {
    let requested = shell.trim();
    if !requested.eq_ignore_ascii_case("auto") {
        if requested.is_empty() {
            return Err(ProductError::TerminalError(
                "shell must not be empty".to_string(),
            ));
        }
        return Ok(requested.to_string());
    }

    #[cfg(windows)]
    let candidates: &[&str] = &["pwsh.exe", "powershell.exe"];
    #[cfg(target_os = "macos")]
    let candidates: &[&str] = &["zsh", "bash", "sh"];
    #[cfg(all(unix, not(target_os = "macos")))]
    let candidates: &[&str] = &["bash", "sh"];
    #[cfg(not(any(windows, unix)))]
    let candidates: &[&str] = &[];

    candidates
        .iter()
        .copied()
        .find(|candidate| executable_on_path(candidate))
        .map(str::to_string)
        .ok_or_else(|| {
            ProductError::TerminalError(
                "未找到可用 shell，请检查系统 shell 是否已安装并加入 PATH。".to_string(),
            )
        })
}

fn executable_on_path(command: &str) -> bool {
    let direct = Path::new(command);
    if direct.components().count() > 1 {
        return direct.is_file();
    }
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|directory| directory.join(command).is_file())
    })
}

/// 去除 ANSI 转义序列（CSI、OSC、DCS/SOS/PM/APC、字符集指定等）。
fn strip_ansi(input: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == 0x1b {
            if i + 1 >= input.len() {
                // ESC 在缓冲区末尾 - 不完整序列，跳过
                i += 1;
                continue;
            }
            match input[i + 1] {
                b'[' => {
                    // CSI: ESC [ ... final byte (0x40–0x7e)
                    i += 2;
                    while i < input.len() && !(input[i] >= 0x40 && input[i] <= 0x7e) {
                        i += 1;
                    }
                    if i < input.len() {
                        i += 1; // 跳过 final byte
                    }
                }
                b']' => {
                    // OSC: ESC ] ... BEL(0x07) 或 ST(ESC \)
                    i += 2;
                    while i < input.len() {
                        if input[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if input[i] == 0x1b && i + 1 < input.len() && input[i + 1] == b'\\' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                b'P' | b'X' | b'^' | b'_' => {
                    // DCS/SOS/PM/APC: 以 ST(ESC \) 结束
                    i += 2;
                    while i < input.len() {
                        if input[i] == 0x1b && i + 1 < input.len() && input[i + 1] == b'\\' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                b'(' | b')' | b'*' | b'+' | b'-' | b'.' | b'/' => {
                    // 字符集指定：ESC ( B（3 字节）
                    i += 2;
                    if i < input.len() {
                        i += 1;
                    }
                }
                _ => {
                    // 其他转义：ESC + 单字节
                    i += 2;
                }
            }
        } else {
            output.push(input[i]);
            i += 1;
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// 辅助：重试读取直到条件满足或超时。
    async fn read_until(
        manager: &TerminalManager,
        id: &str,
        predicate: impl Fn(&[u8]) -> bool,
        timeout: Duration,
    ) -> Vec<u8> {
        let start = std::time::Instant::now();
        let mut accumulated = Vec::new();
        while start.elapsed() < timeout {
            let data = manager.read(id).await.unwrap_or_default();
            accumulated.extend_from_slice(&data);
            if predicate(&accumulated) {
                return accumulated;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        accumulated
    }

    #[test]
    fn strip_ansi_removes_csi() {
        let input = b"\x1b[31mred text\x1b[0m";
        let output = strip_ansi(input);
        assert_eq!(output, b"red text");
    }

    #[cfg(windows)]
    #[test]
    fn powershell_working_dir_uses_regular_drive_and_unc_paths() {
        assert_eq!(
            shell_working_dir(Path::new(r"\\?\D:\project\rust\demo")),
            PathBuf::from(r"D:\project\rust\demo")
        );
        assert_eq!(
            shell_working_dir(Path::new(r"\\?\UNC\server\share\project")),
            PathBuf::from(r"\\server\share\project")
        );
        assert_eq!(
            shell_working_dir(Path::new(r"D:\project\rust\demo")),
            PathBuf::from(r"D:\project\rust\demo")
        );
    }

    #[test]
    fn strip_ansi_removes_osc_bel() {
        let input = b"\x1b]133;A\x07hello";
        let output = strip_ansi(input);
        assert_eq!(output, b"hello");
    }

    #[test]
    fn strip_ansi_removes_osc_st() {
        let input = b"\x1b]133;A\x1b\\hello";
        let output = strip_ansi(input);
        assert_eq!(output, b"hello");
    }

    #[test]
    fn strip_ansi_preserves_plain_text() {
        let input = b"just plain text\n";
        let output = strip_ansi(input);
        assert_eq!(output, input);
    }

    #[test]
    fn strip_ansi_handles_empty() {
        assert!(strip_ansi(b"").is_empty());
    }

    #[test]
    fn strip_ansi_handles_esc_at_end() {
        let input = b"text\x1b";
        let output = strip_ansi(input);
        assert_eq!(output, b"text");
    }

    #[test]
    fn strip_ansi_handles_charset_designation() {
        let input = b"\x1b(Bhello";
        let output = strip_ansi(input);
        assert_eq!(output, b"hello");
    }

    #[test]
    fn strip_ansi_handles_dcs() {
        let input = b"\x1bP1;2;3\x1b\\data";
        let output = strip_ansi(input);
        assert_eq!(output, b"data");
    }

    #[test]
    fn strip_ansi_multiple_sequences() {
        let input = b"\x1b[31m\x1b]133;A\x07\x1b[0mtext\x1b]8;;\x1b\\";
        let output = strip_ansi(input);
        assert_eq!(output, b"text");
    }

    #[test]
    fn terminal_state_debug() {
        assert_eq!(format!("{:?}", TerminalState::Idle), "Idle");
        assert_eq!(format!("{:?}", TerminalState::Busy), "Busy");
        assert_eq!(format!("{:?}", TerminalState::Agent), "Agent");
        assert_eq!(format!("{:?}", TerminalState::Exited), "Exited");
    }

    #[test]
    fn default_creates_manager() {
        let _manager = TerminalManager::default();
    }

    #[test]
    fn pending_output_stays_bounded_without_a_consumer() {
        let mut output = PendingOutput::default();
        let chunk = vec![b'x'; 4096];

        for _ in 0..256 {
            output.append(&chunk);
        }

        assert_eq!(output.received_cursor, (chunk.len() * 256) as u64);
        assert_eq!(output.bytes.len(), MAX_SCROLLBACK);
        assert!(output.received_cursor > output.bytes.len() as u64);
    }

    // === PTY 集成测试 ===
    // Unix: 使用 /bin/cat（回显 stdin）和 /bin/echo（立即退出）
    // Windows: 使用 cmd.exe（保持运行）和 cmd.exe + exit（退出）

    fn cat_path() -> Option<&'static str> {
        #[cfg(unix)]
        {
            ["/bin/cat", "/usr/bin/cat"]
                .iter()
                .find(|p| Path::new(p).exists())
                .copied()
        }
        #[cfg(windows)]
        {
            // cmd.exe 始终可用，保持运行并处理输入
            Some("cmd.exe")
        }
        #[cfg(not(any(unix, windows)))]
        {
            None
        }
    }

    fn echo_path() -> Option<&'static str> {
        #[cfg(unix)]
        {
            ["/bin/echo", "/usr/bin/echo"]
                .iter()
                .find(|p| Path::new(p).exists())
                .copied()
        }
        #[cfg(windows)]
        {
            // Windows 上用 cmd.exe，测试中会发送 exit 使其退出
            Some("cmd.exe")
        }
        #[cfg(not(any(unix, windows)))]
        {
            None
        }
    }

    #[tokio::test]
    async fn create_and_list_terminal() {
        let cat = match cat_path() {
            Some(p) => p,
            None => return, // shell 不可用，跳过
        };

        let tmp = std::env::temp_dir();
        let manager = TerminalManager::new();
        let id = manager
            .create(cat, &tmp, vec![])
            .await
            .expect("create should succeed");

        assert!(!id.is_empty());

        let list = manager.list().await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].0, id);
        assert_eq!(list[0].1, TerminalState::Idle);

        // 清理
        let _ = manager.kill(&id).await;
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn create_with_args_passes_fixed_windows_command_arguments() {
        let manager = TerminalManager::new();
        let id = manager
            .create_with_args(
                "cmd.exe",
                &std::env::temp_dir(),
                vec![],
                vec![
                    "/D".to_string(),
                    "/C".to_string(),
                    "echo".to_string(),
                    "r_code_initial_args".to_string(),
                ],
            )
            .await
            .expect("create with args should succeed");

        let output = read_until(
            &manager,
            &id,
            |data| String::from_utf8_lossy(data).contains("r_code_initial_args"),
            Duration::from_secs(3),
        )
        .await;
        assert!(String::from_utf8_lossy(&output).contains("r_code_initial_args"));
        let _ = manager.kill(&id).await;
    }

    #[tokio::test]
    async fn send_and_read() {
        let cat = match cat_path() {
            Some(p) => p,
            None => return,
        };

        let tmp = std::env::temp_dir();
        let manager = TerminalManager::new();
        let id = manager
            .create(cat, &tmp, vec![])
            .await
            .expect("create should succeed");

        // 发送文本 — cat 会回显（PTY echo + cat 输出）
        manager
            .send(&id, "hello_world\n")
            .await
            .expect("send should succeed");

        // 读取输出，等待 "hello_world" 出现
        let output = read_until(
            &manager,
            &id,
            |data| {
                let s = String::from_utf8_lossy(data);
                s.contains("hello_world")
            },
            Duration::from_secs(3),
        )
        .await;

        let text = String::from_utf8_lossy(&output);
        assert!(
            text.contains("hello_world"),
            "output should contain 'hello_world': got {text:?}"
        );

        // scrollback 也应包含
        {
            let terminals = manager.terminals.lock().await;
            let handle = terminals.get(&id).expect("terminal should exist");
            let sb = String::from_utf8_lossy(&handle.scrollback);
            assert!(
                sb.contains("hello_world"),
                "scrollback should contain 'hello_world': got {sb:?}"
            );
        }

        // UI 快照在增量 read 已消费输出后仍可恢复完整内容。
        let snapshot = manager.snapshot(&id).await.unwrap();
        assert!(
            String::from_utf8_lossy(&snapshot).contains("hello_world"),
            "snapshot should retain prior output: {:?}",
            String::from_utf8_lossy(&snapshot)
        );

        let _ = manager.kill(&id).await;
    }

    #[tokio::test]
    async fn output_notification_wakes_renderer_without_polling() {
        let shell = match cat_path() {
            Some(path) => path,
            None => return,
        };

        let manager = TerminalManager::new();
        let mut output_notifications = manager.subscribe_output();
        let id = manager
            .create(shell, &std::env::temp_dir(), vec![])
            .await
            .expect("create should succeed");

        // Discard shell startup output and its notification. The assertion below is about
        // input echo, which must wake the renderer instead of waiting for its recovery poll.
        tokio::time::sleep(Duration::from_millis(50)).await;
        let initial = manager
            .raw_snapshot(&id)
            .await
            .expect("initial raw snapshot");
        while output_notifications.try_recv().is_ok() {}

        #[cfg(windows)]
        let input = "echo r_code_event_driven_echo\r";
        #[cfg(not(windows))]
        let input = "r_code_event_driven_echo\n";

        manager.send(&id, input).await.expect("send should succeed");
        let notified_id = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let candidate = output_notifications
                    .recv()
                    .await
                    .expect("output notification channel should stay open");
                if candidate == id {
                    break candidate;
                }
            }
        })
        .await
        .expect("PTY output should notify the renderer without polling");

        assert_eq!(notified_id, id);
        let batch = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let batch = manager
                    .raw_since(&id, initial.cursor)
                    .await
                    .expect("notified output should be readable");
                if batch.output.contains("r_code_event_driven_echo") {
                    break batch;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("notified PTY output should become readable");
        assert!(
            batch.output.contains("r_code_event_driven_echo"),
            "notification must correspond to readable PTY output: {:?}",
            batch.output
        );

        let _ = manager.kill(&id).await;
    }

    #[tokio::test]
    async fn raw_cursor_recovers_output_consumed_by_text_reader() {
        let shell = match cat_path() {
            Some(path) => path,
            None => return,
        };

        let manager = TerminalManager::new();
        let id = manager
            .create(shell, &std::env::temp_dir(), vec![])
            .await
            .expect("create should succeed");
        let initial = manager
            .raw_snapshot(&id)
            .await
            .expect("initial raw snapshot");

        #[cfg(windows)]
        let input = "echo r_code_raw_cursor\r";
        #[cfg(not(windows))]
        let input = "r_code_raw_cursor\n";
        manager.send(&id, input).await.expect("send should succeed");

        // 模拟一个 agent 先读取 ANSI-free 文本，UI 随后仍应能由 raw scrollback
        // 通过绝对游标补回同一段输出。
        let mut text_seen = String::new();
        for _ in 0..75 {
            text_seen.push_str(&String::from_utf8_lossy(
                &manager.read(&id).await.expect("text read should succeed"),
            ));
            if text_seen.contains("r_code_raw_cursor") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(text_seen.contains("r_code_raw_cursor"));

        let batch = manager
            .raw_since(&id, initial.cursor)
            .await
            .expect("raw batch should succeed");
        assert!(!batch.reset, "fresh cursor must not require a reset");
        assert!(batch.cursor > initial.cursor, "cursor should advance");
        assert!(
            batch.output.contains("r_code_raw_cursor"),
            "raw UI stream should recover text already consumed by agent: {:?}",
            batch.output
        );

        let empty = manager
            .raw_since(&id, batch.cursor)
            .await
            .expect("second raw batch should succeed");
        assert!(!empty.reset);
        assert!(empty.output.is_empty());

        let _ = manager.kill(&id).await;
    }

    #[tokio::test]
    async fn read_strips_ansi() {
        let cat = match cat_path() {
            Some(p) => p,
            None => return,
        };

        let tmp = std::env::temp_dir();
        let manager = TerminalManager::new();
        let id = manager
            .create(cat, &tmp, vec![])
            .await
            .expect("create should succeed");

        // 发送包含 ANSI 转义的文本
        manager.send(&id, "\x1b[31mred\x1b[0m\n").await.unwrap();

        let output = read_until(
            &manager,
            &id,
            |data| {
                let s = String::from_utf8_lossy(data);
                s.contains("red")
            },
            Duration::from_secs(3),
        )
        .await;

        // 输出不应包含 ANSI 转义序列
        assert!(
            !output.windows(2).any(|w| w == b"\x1b["),
            "output should not contain CSI sequences: got {:?}",
            String::from_utf8_lossy(&output)
        );
        // 但应包含 "red"
        assert!(String::from_utf8_lossy(&output).contains("red"));

        let _ = manager.kill(&id).await;
    }

    #[tokio::test]
    async fn kill_removes_terminal() {
        let cat = match cat_path() {
            Some(p) => p,
            None => return,
        };

        let tmp = std::env::temp_dir();
        let manager = TerminalManager::new();
        let id = manager
            .create(cat, &tmp, vec![])
            .await
            .expect("create should succeed");

        // kill 前存在
        assert_eq!(manager.list().await.len(), 1);

        manager.kill(&id).await.expect("kill should succeed");

        // kill 后不存在
        assert!(manager.list().await.is_empty());

        // 再次 kill 应返回错误
        let result = manager.kill(&id).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn kill_already_exited_process() {
        let echo = match echo_path() {
            Some(p) => p,
            None => return,
        };

        let tmp = std::env::temp_dir();
        let manager = TerminalManager::new();
        let id = manager
            .create(echo, &tmp, vec![])
            .await
            .expect("create should succeed");

        // Unix: echo 立即退出
        // Windows: cmd.exe 需要发送 exit 使其退出
        #[cfg(windows)]
        {
            let _ = manager.send(&id, "exit\r\n").await;
        }

        tokio::time::sleep(Duration::from_millis(500)).await;

        // kill 一个已退出的进程应仍然成功（资源清理）
        let result = manager.kill(&id).await;
        assert!(result.is_ok(), "kill should succeed even if process exited");
    }

    #[tokio::test]
    async fn resize_terminal() {
        let cat = match cat_path() {
            Some(p) => p,
            None => return,
        };

        let tmp = std::env::temp_dir();
        let manager = TerminalManager::new();
        let id = manager
            .create(cat, &tmp, vec![])
            .await
            .expect("create should succeed");

        manager
            .resize(&id, 120, 40)
            .await
            .expect("resize should succeed");

        let _ = manager.kill(&id).await;
    }

    #[tokio::test]
    async fn get_state_idle() {
        let cat = match cat_path() {
            Some(p) => p,
            None => return,
        };

        let tmp = std::env::temp_dir();
        let manager = TerminalManager::new();
        let id = manager
            .create(cat, &tmp, vec![])
            .await
            .expect("create should succeed");

        let state = manager
            .get_state(&id)
            .await
            .expect("get_state should succeed");
        assert_eq!(state, TerminalState::Idle);

        let _ = manager.kill(&id).await;
    }

    #[tokio::test]
    async fn get_state_exited() {
        let echo = match echo_path() {
            Some(p) => p,
            None => return,
        };

        let tmp = std::env::temp_dir();
        let manager = TerminalManager::new();
        let id = manager
            .create(echo, &tmp, vec![])
            .await
            .expect("create should succeed");

        // Unix: echo 立即退出
        // Windows: cmd.exe 需要发送 exit 使其退出
        #[cfg(windows)]
        {
            let _ = manager.send(&id, "exit\r\n").await;
        }

        // 等待进程退出
        tokio::time::sleep(Duration::from_millis(500)).await;

        let state = manager
            .get_state(&id)
            .await
            .expect("get_state should succeed");
        assert_eq!(state, TerminalState::Exited);

        let _ = manager.kill(&id).await;
    }

    #[tokio::test]
    async fn send_to_nonexistent_returns_error() {
        let manager = TerminalManager::new();
        let result = manager.send("nonexistent", "hello").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn read_from_nonexistent_returns_error() {
        let manager = TerminalManager::new();
        let result = manager.read("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn resize_nonexistent_returns_error() {
        let manager = TerminalManager::new();
        let result = manager.resize("nonexistent", 80, 24).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn get_state_nonexistent_returns_error() {
        let manager = TerminalManager::new();
        let result = manager.get_state("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn multiple_terminals() {
        let cat = match cat_path() {
            Some(p) => p,
            None => return,
        };

        let manager = TerminalManager::new();
        let id1 = manager
            .create(cat, Path::new("/tmp"), vec![])
            .await
            .unwrap();
        let id2 = manager
            .create(cat, Path::new("/tmp"), vec![])
            .await
            .unwrap();

        assert_ne!(id1, id2, "IDs should be unique");

        let list = manager.list().await;
        assert_eq!(list.len(), 2);

        let _ = manager.kill(&id1).await;
        let _ = manager.kill(&id2).await;

        assert!(manager.list().await.is_empty());
    }

    #[tokio::test]
    async fn scrollback_truncation() {
        let cat = match cat_path() {
            Some(p) => p,
            None => return,
        };

        let manager = TerminalManager::new();
        let id = manager
            .create(cat, Path::new("/tmp"), vec![])
            .await
            .expect("create should succeed");

        // 发送大量数据 — 每行约 10 字节，发送 30000 行 = ~300KB（超过 200KB 上限）
        for _ in 0..30 {
            // 每次发送 ~10KB
            let chunk = "0123456789".repeat(1000) + "\n"; // ~10KB
            manager.send(&id, &chunk).await.unwrap();
            // 读取以排空通道
            tokio::time::sleep(Duration::from_millis(10)).await;
            let _ = manager.read(&id).await;
        }

        // 最终读取
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = manager.read(&id).await;

        // scrollback 应被截断到 MAX_SCROLLBACK
        {
            let terminals = manager.terminals.lock().await;
            let handle = terminals.get(&id).expect("terminal should exist");
            assert!(
                handle.scrollback.len() <= MAX_SCROLLBACK,
                "scrollback should be <= {} bytes, got {}",
                MAX_SCROLLBACK,
                handle.scrollback.len()
            );
        }

        let _ = manager.kill(&id).await;
    }

    #[tokio::test]
    async fn env_vars_passed_to_child() {
        let cat = match cat_path() {
            Some(p) => p,
            None => return,
        };

        let manager = TerminalManager::new();
        let id = manager
            .create(
                cat,
                Path::new("/tmp"),
                vec![("R_CODE_TEST_VAR".to_string(), "test_value_42".to_string())],
            )
            .await
            .expect("create should succeed");

        // 发送命令检查环境变量
        // 注意：cat 不是 shell，无法执行命令。
        // 这个测试仅验证 create 带 env 不报错。
        let state = manager.get_state(&id).await.unwrap();
        assert_eq!(state, TerminalState::Idle);

        let _ = manager.kill(&id).await;
    }
}
