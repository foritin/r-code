//! Terminal Control Service - 六个 `terminal.*` 原语 [doc-03 §8] [doc-05 §2.2]
//!
//! 所有操作走相同的 Tool Gateway + Permission Engine + Ledger 路径。
//! 本服务提供原语级别的终端控制，权限/审计由上层网关统一处理。
//!
//! ## 六个原语
//! - `list`   - 列出可见兄弟终端（含 busy/idle 状态）
//! - `read`   - 读取 ANSI-free 内存 tail（约 200KB）
//! - `send`   - 注入文本 + 可选 Enter（bracketed paste for multi-line）
//! - `create` - 创建 shell/CLI worker 终端
//! - `wait`   - 等待命令退出/安静/正则匹配
//! - `kill`   - 终止终端（agent 调用禁止）

use std::sync::Arc;
use std::time::{Duration, Instant};

use r_code_core::error::ProductError;

use crate::manager::{TerminalId, TerminalManager, TerminalState};

/// Terminal control service - implements the six terminal.* primitives.
/// All operations go through the same Tool Gateway + Permission Engine + Ledger.
/// [doc-05 §2.1]
pub struct TerminalControlService {
    manager: Arc<TerminalManager>,
}

/// Terminal info returned by terminal.list.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TerminalInfo {
    pub id: TerminalId,
    pub state: TerminalState,
    pub shell: String,
    pub is_busy: bool,
}

/// Result of terminal.wait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitResult {
    /// Command exited with code
    Exited(i32),
    /// Terminal became quiet (no output for duration)
    Quiet,
    /// Regex pattern matched in output
    PatternMatched(String),
    /// Timeout reached
    Timeout,
    /// Cancelled
    Cancelled,
}

/// Send options for terminal.send.
#[derive(Debug, Clone, Default)]
pub struct SendOptions {
    /// Text to inject
    pub text: String,
    /// Whether to press Enter after text (default: false)
    pub press_enter: bool,
}

/// Wait detection mode.
#[derive(Debug, Clone)]
pub enum WaitMode {
    /// Wait for OSC 133 exit code marker
    ExitCode,
    /// Wait for quiet period (no output for `quiet_ms` milliseconds)
    Quiet { quiet_ms: u64 },
    /// Wait for regex pattern to match in output
    Pattern { pattern: String },
}

/// 轮询间隔（毫秒）。
const POLL_INTERVAL_MS: u64 = 20;

impl TerminalControlService {
    pub fn new(manager: Arc<TerminalManager>) -> Self {
        Self { manager }
    }

    /// terminal.list - List visible sibling terminals with busy/idle status.
    /// Risk: R0 (read-only)
    pub async fn list(&self) -> Result<Vec<TerminalInfo>, ProductError> {
        let raw = self.manager.list_with_shell().await;
        Ok(raw
            .into_iter()
            .map(|(id, state, shell)| TerminalInfo {
                id,
                is_busy: state == TerminalState::Busy,
                state,
                shell,
            })
            .collect())
    }

    /// terminal.read - Read ANSI-free memory tail (~200KB).
    /// Risk: R1 (may leak info)
    pub async fn read(&self, terminal_id: &str) -> Result<String, ProductError> {
        let data = self.manager.read(terminal_id).await?;
        Ok(String::from_utf8_lossy(&data).to_string())
    }

    /// terminal.snapshot - 为 UI 读取完整保留 scrollback。
    ///
    /// 与 `read` 不同，此接口刻意返回此前输出，使新挂载的终端面板也能呈现已被
    /// agent 或其他工具调用方消费的内容。
    pub async fn snapshot(&self, terminal_id: &str) -> Result<String, ProductError> {
        let data = self.manager.snapshot(terminal_id).await?;
        Ok(String::from_utf8_lossy(&data).to_string())
    }

    /// terminal.send - Inject text + optional Enter.
    /// Dynamic risk: bare shell -> R2, TUI/Agent -> R0, control chars -> R2.
    /// Uses \r for Enter, bracketed paste for multi-line.
    /// Self-control rejected (cannot send to own terminal).
    pub async fn send(
        &self,
        terminal_id: &str,
        caller_terminal_id: Option<&str>,
        options: SendOptions,
    ) -> Result<(), ProductError> {
        // Self-control rejection [doc-05 §6.2]
        if let Some(caller) = caller_terminal_id {
            if caller == terminal_id {
                return Err(ProductError::PermissionError(
                    "self-control rejected: cannot send to own terminal".to_string(),
                ));
            }
        }

        // Detect multi-line input (original text contains newlines)
        let is_multiline = options.text.contains('\n');

        // Convert \n to \r (Enter uses \r in PTY)
        let mut text = options.text.replace('\n', "\r");
        if options.press_enter {
            text.push('\r');
        }

        // Wrap multi-line in bracketed paste sequence [doc-03 §8]
        let final_text = if is_multiline {
            format!("\x1b[200~{text}\x1b[201~")
        } else {
            text
        };

        self.manager.send(terminal_id, &final_text).await
    }

    /// terminal.create - Create a new shell/CLI worker terminal.
    /// Risk: R2 (may modify state)
    /// After launch + settle, can autorun if configured.
    pub async fn create(
        &self,
        shell: &str,
        working_dir: &std::path::Path,
        env: Vec<(String, String)>,
    ) -> Result<TerminalId, ProductError> {
        self.manager.create(shell, working_dir, env).await
    }

    /// terminal.wait - Wait for terminal to reach a state.
    /// Three detection modes:
    /// 1. OSC 133 exit code (from shell integration)
    /// 2. Quiet period (no output for N ms)
    /// 3. Regex pattern match in output
    ///
    /// Risk: R0 (read-only)
    pub async fn wait(
        &self,
        terminal_id: &str,
        mode: WaitMode,
        timeout_ms: u64,
    ) -> Result<WaitResult, ProductError> {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        let poll = Duration::from_millis(POLL_INTERVAL_MS);

        match mode {
            WaitMode::ExitCode => {
                // 排空通道并记录初始版本号
                let _ = self.manager.read(terminal_id).await?;
                let (initial_version, _) = self.manager.exit_code_status(terminal_id).await?;
                loop {
                    let _ = self.manager.read(terminal_id).await?;
                    let (version, code) = self.manager.exit_code_status(terminal_id).await?;
                    if version > initial_version {
                        if let Some(c) = code {
                            return Ok(WaitResult::Exited(c));
                        }
                    }
                    if Instant::now() >= deadline {
                        return Ok(WaitResult::Timeout);
                    }
                    tokio::time::sleep(poll).await;
                }
            }
            WaitMode::Quiet { quiet_ms } => {
                let quiet_duration = Duration::from_millis(quiet_ms);
                let mut last_output = Instant::now();
                loop {
                    let data = self.manager.read(terminal_id).await?;
                    if !data.is_empty() {
                        last_output = Instant::now();
                    }
                    if last_output.elapsed() >= quiet_duration {
                        return Ok(WaitResult::Quiet);
                    }
                    if Instant::now() >= deadline {
                        return Ok(WaitResult::Timeout);
                    }
                    tokio::time::sleep(poll).await;
                }
            }
            WaitMode::Pattern { pattern } => {
                let re = regex::Regex::new(&pattern)
                    .map_err(|e| ProductError::Other(format!("invalid regex: {e}")))?;
                loop {
                    let data = self.manager.read(terminal_id).await?;
                    let text = String::from_utf8_lossy(&data);
                    if let Some(m) = re.find(&text) {
                        return Ok(WaitResult::PatternMatched(m.as_str().to_string()));
                    }
                    if Instant::now() >= deadline {
                        return Ok(WaitResult::Timeout);
                    }
                    tokio::time::sleep(poll).await;
                }
            }
        }
    }

    /// terminal.kill - Kill a terminal.
    /// Risk: R3 (high risk, agent calls forbidden)
    /// Agent calls to kill are rejected.
    pub async fn kill(&self, terminal_id: &str, caller_is_agent: bool) -> Result<(), ProductError> {
        if caller_is_agent {
            return Err(ProductError::PermissionError(
                "agent calls to kill are forbidden".to_string(),
            ));
        }
        self.manager.kill(terminal_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::path::Path;

    /// Unix: /bin/cat（回显 stdin）；Windows: cmd.exe。
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
            Some("cmd.exe")
        }
        #[cfg(not(any(unix, windows)))]
        {
            None
        }
    }

    fn make_service() -> Arc<TerminalControlService> {
        Arc::new(TerminalControlService::new(
            Arc::new(TerminalManager::new()),
        ))
    }

    #[tokio::test]
    async fn list_empty() {
        let service = make_service();
        let list = service.list().await.expect("list should succeed");
        assert!(list.is_empty());
    }

    #[tokio::test]
    async fn list_with_terminal() {
        let cat = match cat_path() {
            Some(p) => p,
            None => return,
        };
        let manager = Arc::new(TerminalManager::new());
        let id = manager
            .create(cat, &std::env::temp_dir(), vec![])
            .await
            .expect("create should succeed");
        let service = TerminalControlService::new(manager.clone());

        let list = service.list().await.expect("list should succeed");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, id);
        assert_eq!(list[0].shell, cat);
        assert!(!list[0].is_busy);

        let _ = manager.kill(&id).await;
    }

    #[tokio::test]
    async fn read_returns_output() {
        let cat = match cat_path() {
            Some(p) => p,
            None => return,
        };
        let manager = Arc::new(TerminalManager::new());
        let id = manager
            .create(cat, &std::env::temp_dir(), vec![])
            .await
            .expect("create should succeed");
        let service = TerminalControlService::new(manager.clone());

        service
            .send(
                &id,
                None,
                SendOptions {
                    text: "hello_r_code".to_string(),
                    press_enter: true,
                },
            )
            .await
            .expect("send should succeed");

        // 轮询读取直到看到输出
        let mut found = false;
        for _ in 0..50 {
            let text = service.read(&id).await.expect("read should succeed");
            if text.contains("hello_r_code") {
                found = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        assert!(found, "read should contain echoed text");

        let _ = manager.kill(&id).await;
    }

    #[tokio::test]
    async fn send_rejects_self_control() {
        let service = make_service();
        let result = service
            .send(
                "term1",
                Some("term1"),
                SendOptions {
                    text: "hello".to_string(),
                    press_enter: false,
                },
            )
            .await;
        assert!(result.is_err(), "self-control send should be rejected");
        let err = result.unwrap_err();
        assert!(
            matches!(err, ProductError::PermissionError(ref msg) if msg.contains("self-control")),
            "expected self-control rejection, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn send_allows_other_terminal() {
        let cat = match cat_path() {
            Some(p) => p,
            None => return,
        };
        let manager = Arc::new(TerminalManager::new());
        let id = manager
            .create(cat, &std::env::temp_dir(), vec![])
            .await
            .expect("create should succeed");
        let service = TerminalControlService::new(manager.clone());

        // caller != target -> allowed
        let result = service
            .send(
                &id,
                Some("other_terminal"),
                SendOptions {
                    text: "test".to_string(),
                    press_enter: false,
                },
            )
            .await;
        assert!(result.is_ok(), "send to other terminal should succeed");

        let _ = manager.kill(&id).await;
    }

    #[tokio::test]
    async fn create_returns_id() {
        let cat = match cat_path() {
            Some(p) => p,
            None => return,
        };
        let service = make_service();
        let id = service
            .create(cat, &std::env::temp_dir(), vec![])
            .await
            .expect("create should succeed");
        assert!(!id.is_empty());

        let list = service.list().await.expect("list should succeed");
        assert_eq!(list.len(), 1);

        let _ = service.kill(&id, false).await;
    }

    #[tokio::test]
    async fn wait_quiet_detects_silence() {
        let cat = match cat_path() {
            Some(p) => p,
            None => return,
        };
        let manager = Arc::new(TerminalManager::new());
        let id = manager
            .create(cat, &std::env::temp_dir(), vec![])
            .await
            .expect("create should succeed");
        let service = TerminalControlService::new(manager.clone());

        // 发送一些文本触发输出
        service
            .send(
                &id,
                None,
                SendOptions {
                    text: "noise".to_string(),
                    press_enter: true,
                },
            )
            .await
            .ok();
        // 等待输出排空
        tokio::time::sleep(Duration::from_millis(300)).await;
        let _ = service.read(&id).await;

        // 等待安静（200ms 无输出），总超时 3s
        let result = service
            .wait(&id, WaitMode::Quiet { quiet_ms: 200 }, 3000)
            .await
            .expect("wait should succeed");
        assert_eq!(result, WaitResult::Quiet);

        let _ = manager.kill(&id).await;
    }

    #[tokio::test]
    async fn wait_pattern_matches_output() {
        let cat = match cat_path() {
            Some(p) => p,
            None => return,
        };
        let manager = Arc::new(TerminalManager::new());
        let id = manager
            .create(cat, &std::env::temp_dir(), vec![])
            .await
            .expect("create should succeed");
        let service = TerminalControlService::new(manager.clone());

        service
            .send(
                &id,
                None,
                SendOptions {
                    text: "unique_pattern_xyz".to_string(),
                    press_enter: true,
                },
            )
            .await
            .ok();

        let result = service
            .wait(
                &id,
                WaitMode::Pattern {
                    pattern: r"unique_pattern_xyz".to_string(),
                },
                3000,
            )
            .await
            .expect("wait should succeed");
        assert!(
            matches!(result, WaitResult::PatternMatched(ref s) if s.contains("unique_pattern_xyz")),
            "expected pattern match, got: {result:?}"
        );

        let _ = manager.kill(&id).await;
    }

    #[tokio::test]
    async fn wait_timeout_when_no_match() {
        let cat = match cat_path() {
            Some(p) => p,
            None => return,
        };
        let manager = Arc::new(TerminalManager::new());
        let id = manager
            .create(cat, &std::env::temp_dir(), vec![])
            .await
            .expect("create should succeed");
        let service = TerminalControlService::new(manager.clone());

        let result = service
            .wait(
                &id,
                WaitMode::Pattern {
                    pattern: r"never_appears_12345".to_string(),
                },
                200,
            )
            .await
            .expect("wait should succeed");
        assert_eq!(result, WaitResult::Timeout);

        let _ = manager.kill(&id).await;
    }

    #[tokio::test]
    async fn wait_exit_code_times_out_without_shell_integration() {
        // /bin/cat 不会发送 OSC 133 标记，ExitCode 模式应超时
        let cat = match cat_path() {
            Some(p) => p,
            None => return,
        };
        let manager = Arc::new(TerminalManager::new());
        let id = manager
            .create(cat, &std::env::temp_dir(), vec![])
            .await
            .expect("create should succeed");
        let service = TerminalControlService::new(manager.clone());

        let result = service
            .wait(&id, WaitMode::ExitCode, 300)
            .await
            .expect("wait should succeed");
        assert_eq!(result, WaitResult::Timeout);

        let _ = manager.kill(&id).await;
    }

    #[cfg(unix)] // 依赖 cat 的纯回显语义；Windows 下 cat_path() 返回 cmd.exe（无等价回显）
    #[tokio::test]
    async fn wait_exit_code_detects_osc133() {
        // 通过 cat 回显 OSC 133 序列来模拟 shell 集成发送的退出码标记
        let cat = match cat_path() {
            Some(p) => p,
            None => return,
        };
        let manager = Arc::new(TerminalManager::new());
        let id = manager
            .create(cat, &std::env::temp_dir(), vec![])
            .await
            .expect("create should succeed");
        let service = TerminalControlService::new(manager.clone());

        // 发送 OSC 133 序列：prompt start (A) + command exit (D;42)
        // cat 会回显这些字节，block parser 解析后得到 exit code 42
        service
            .send(
                &id,
                None,
                SendOptions {
                    text: "\x1b]133;A\x07\x1b]133;D;42\x07".to_string(),
                    press_enter: true,
                },
            )
            .await
            .ok();

        let result = service
            .wait(&id, WaitMode::ExitCode, 3000)
            .await
            .expect("wait should succeed");
        assert!(
            matches!(result, WaitResult::Exited(42)),
            "expected Exited(42), got: {result:?}"
        );

        let _ = manager.kill(&id).await;
    }

    #[tokio::test]
    async fn kill_rejects_agent_caller() {
        let service = make_service();
        let result = service.kill("any_id", true).await;
        assert!(result.is_err(), "agent kill should be rejected");
        let err = result.unwrap_err();
        assert!(
            matches!(err, ProductError::PermissionError(ref msg) if msg.contains("agent")),
            "expected agent rejection, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn kill_allows_non_agent_caller() {
        let cat = match cat_path() {
            Some(p) => p,
            None => return,
        };
        let service = make_service();
        let id = service
            .create(cat, &std::env::temp_dir(), vec![])
            .await
            .expect("create should succeed");

        service
            .kill(&id, false)
            .await
            .expect("non-agent kill should succeed");

        let list = service.list().await.expect("list should succeed");
        assert!(list.is_empty(), "terminal should be gone after kill");
    }

    #[tokio::test]
    async fn kill_nonexistent_returns_error() {
        let service = make_service();
        let result = service.kill("nonexistent", false).await;
        assert!(result.is_err(), "kill nonexistent should fail");
    }

    #[tokio::test]
    async fn send_multiline_uses_bracketed_paste() {
        // 验证多行文本不会因 self-control 或格式问题失败
        let cat = match cat_path() {
            Some(p) => p,
            None => return,
        };
        let manager = Arc::new(TerminalManager::new());
        let id = manager
            .create(cat, &std::env::temp_dir(), vec![])
            .await
            .expect("create should succeed");
        let service = TerminalControlService::new(manager.clone());

        let result = service
            .send(
                &id,
                None,
                SendOptions {
                    text: "line1\nline2\nline3".to_string(),
                    press_enter: false,
                },
            )
            .await;
        assert!(result.is_ok(), "multiline send should succeed");

        let _ = manager.kill(&id).await;
    }

    #[test]
    fn send_options_default() {
        let opts = SendOptions::default();
        assert!(opts.text.is_empty());
        assert!(!opts.press_enter);
    }

    #[test]
    fn wait_result_equality() {
        assert_eq!(WaitResult::Quiet, WaitResult::Quiet);
        assert_eq!(WaitResult::Exited(0), WaitResult::Exited(0));
        assert_ne!(WaitResult::Exited(0), WaitResult::Exited(1));
        assert_eq!(WaitResult::Timeout, WaitResult::Timeout);
    }
}
