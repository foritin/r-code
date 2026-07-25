//! PTY Management — 多终端 PTY 管理 [doc-03 §2]
//!
//! 使用 `portable-pty` crate 管理多个 PTY 终端实例。
//! 每个终端拥有独立的子进程、读写管道和 scrollback 缓冲区。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize, PtySystem};
use r_code_core::error::ProductError;

use crate::block::BlockParser;

/// Scrollback 缓冲区最大大小（约 200KB）。
const MAX_SCROLLBACK: usize = 200_000;

/// 终端 ID 类型。
pub type TerminalId = String;

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
    /// PTY 子进程
    child: Box<dyn Child + Send + Sync>,
    /// PTY stdin 写入器
    writer: Box<dyn std::io::Write + Send>,
    /// PTY master（用于 resize 和 reader 克隆）
    master: Box<dyn MasterPty + Send>,
    /// 后台读取线程的输出通道
    output_rx: mpsc::UnboundedReceiver<Vec<u8>>,
    /// 是否已被显式 kill（防止 Drop 重复 kill 导致 PID 复用风险）
    killed: bool,
    /// OSC 133 块解析器（用于追踪命令退出码）[doc-03 §4]
    block_parser: BlockParser,
    /// 最后观测到的命令退出码
    last_exit_code: Option<i32>,
    /// 退出码版本号（每次新观测到退出码时递增）
    exit_code_version: u64,
    /// 已处理的 block_parser 块数（用于增量检测新完成的块）
    blocks_seen: usize,
}

impl std::fmt::Debug for TerminalHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalHandle")
            .field("id", &self.id)
            .field("state", &self.state)
            .field("shell", &self.shell)
            .field("working_dir", &self.working_dir)
            .field("scrollback_len", &self.scrollback.len())
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
    }
}

/// TerminalManager — 管理多个 PTY 终端。
pub struct TerminalManager {
    pty_system: std::sync::Mutex<Box<dyn PtySystem + Send>>,
    terminals: Arc<Mutex<HashMap<TerminalId, TerminalHandle>>>,
}

impl TerminalManager {
    pub fn new() -> Self {
        Self {
            pty_system: std::sync::Mutex::new(native_pty_system()),
            terminals: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// 创建一个新终端。
    pub async fn create(
        &self,
        shell: &str,
        working_dir: &Path,
        env: Vec<(String, String)>,
    ) -> Result<TerminalId, ProductError> {
        let id = uuid::Uuid::new_v4().to_string();

        let pair = {
            let pty_system = self.pty_system.lock().expect("pty_system mutex poisoned");
            pty_system
                .openpty(PtySize::default())
                .map_err(|e| ProductError::TerminalError(format!("openpty failed: {e}")))?
        };

        let mut cmd = CommandBuilder::new(shell);
        cmd.cwd(working_dir);
        for (k, v) in &env {
            cmd.env(k, v);
        }

        let child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| ProductError::TerminalError(format!("spawn failed: {e}")))?;

        let writer = pair
            .master
            .take_writer()
            .map_err(|e| ProductError::TerminalError(format!("take_writer failed: {e}")))?;

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| ProductError::TerminalError(format!("try_clone_reader failed: {e}")))?;

        // 丢弃 slave — 关闭我们的 slave 引用，使子进程退出时 master reader 能收到 EOF
        drop(pair.slave);
        let master = pair.master;

        // 后台线程持续读取 PTY 输出，通过 unbounded channel 传递
        let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break, // EOF
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break; // receiver dropped
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        let handle = TerminalHandle {
            id: id.clone(),
            state: TerminalState::Idle,
            shell: shell.to_string(),
            working_dir: working_dir.to_path_buf(),
            scrollback: Vec::new(),
            child,
            writer,
            master,
            output_rx: rx,
            killed: false,
            block_parser: BlockParser::new(),
            last_exit_code: None,
            exit_code_version: 0,
            blocks_seen: 0,
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

        // 排空通道中所有可用数据
        let mut raw_data = Vec::new();
        loop {
            match handle.output_rx.try_recv() {
                Ok(chunk) => raw_data.extend_from_slice(&chunk),
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    handle.state = TerminalState::Exited;
                    break;
                }
            }
        }

        // Feed raw data to block parser for OSC 133 exit code tracking [doc-03 §4]
        if !raw_data.is_empty() {
            handle.block_parser.feed(&raw_data);
            let new_block_count = handle.block_parser.blocks().len();
            while handle.blocks_seen < new_block_count {
                let block = &handle.block_parser.blocks()[handle.blocks_seen];
                if let Some(code) = block.exit_code {
                    handle.last_exit_code = Some(code);
                    handle.exit_code_version = handle.exit_code_version.wrapping_add(1);
                }
                handle.blocks_seen += 1;
            }
        }

        // 去除 ANSI 转义序列
        let stripped = strip_ansi(&raw_data);

        // 追加到 scrollback
        handle.scrollback.extend_from_slice(&stripped);

        // 超出上限时截断最旧的数据
        if handle.scrollback.len() > MAX_SCROLLBACK {
            let excess = handle.scrollback.len() - MAX_SCROLLBACK;
            handle.scrollback.drain(..excess);
        }

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

    /// 终止一个终端。
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
        let terminals = self.terminals.lock().await;
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

        if handle.state != TerminalState::Exited
            && handle
                .child
                .try_wait()
                .map_err(|e| ProductError::TerminalError(format!("try_wait failed: {e}")))?
                .is_some()
        {
            handle.state = TerminalState::Exited;
        }

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
        let terminals = self.terminals.lock().await;
        terminals
            .values()
            .map(|h| (h.id.clone(), h.state.clone(), h.shell.clone()))
            .collect()
    }
}

impl Default for TerminalManager {
    fn default() -> Self {
        Self::new()
    }
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
