//! CliDetector - 外部 CLI 检测 [doc-03 §7] [doc-10 §2]
//!
//! TerminalManager 轮询 PTY 前台进程标题（~700ms），匹配已知 Agent CLI
//! 列表（`claude`、`codex`）进入 agent 状态。
//!
//! ## 信号选择 [doc-10 §2.3]
//! - 使用 PTY 前台进程标题作为信号。
//! - 不使用 OSC 标题（不稳定）。
//! - 不使用 alternate-screen 切换（Claude Code renderer 模式不稳定）。

use std::time::Duration;

/// 检测到的外部 CLI 类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalCli {
    /// Claude CLI (anthropic)
    Claude,
    /// Codex CLI (openai)
    Codex,
    /// 未检测到已知 CLI
    None,
}

/// CliDetector - 轮询 PTY 前台进程标题以检测外部 CLI。
///
/// 使用进程标题作为信号，**不**使用 OSC 标题或 alternate-screen。
/// 轮询间隔：~700ms。
pub struct CliDetector {
    poll_interval: Duration,
}

impl CliDetector {
    /// 创建检测器，使用默认 700ms 轮询间隔。
    pub fn new() -> Self {
        Self {
            poll_interval: Duration::from_millis(700),
        }
    }

    /// 检测进程名是否指示外部 CLI agent。
    ///
    /// 在进程名中匹配 "claude" 或 "codex"（大小写不敏感）。
    pub fn detect(process_name: &str) -> ExternalCli {
        let lower = process_name.to_ascii_lowercase();
        if lower.contains("claude") {
            ExternalCli::Claude
        } else if lower.contains("codex") {
            ExternalCli::Codex
        } else {
            ExternalCli::None
        }
    }

    /// 获取轮询间隔。
    pub fn poll_interval(&self) -> Duration {
        self.poll_interval
    }
}

impl Default for CliDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_claude_exact() {
        assert_eq!(CliDetector::detect("claude"), ExternalCli::Claude);
    }

    #[test]
    fn detect_codex_exact() {
        assert_eq!(CliDetector::detect("codex"), ExternalCli::Codex);
    }

    #[test]
    fn detect_none() {
        assert_eq!(CliDetector::detect("bash"), ExternalCli::None);
        assert_eq!(CliDetector::detect("zsh"), ExternalCli::None);
        assert_eq!(CliDetector::detect("vim"), ExternalCli::None);
        assert_eq!(CliDetector::detect(""), ExternalCli::None);
    }

    #[test]
    fn detect_case_insensitive() {
        assert_eq!(CliDetector::detect("CLAUDE"), ExternalCli::Claude);
        assert_eq!(CliDetector::detect("Claude"), ExternalCli::Claude);
        assert_eq!(CliDetector::detect("CODEX"), ExternalCli::Codex);
        assert_eq!(CliDetector::detect("Codex"), ExternalCli::Codex);
    }

    #[test]
    fn detect_substring_match() {
        // 进程名可能包含路径或其他前缀
        assert_eq!(
            CliDetector::detect("/usr/local/bin/claude"),
            ExternalCli::Claude
        );
        assert_eq!(
            CliDetector::detect("node /opt/codex/bin/codex.js"),
            ExternalCli::Codex
        );
        assert_eq!(
            CliDetector::detect("claude-code-worker"),
            ExternalCli::Claude
        );
    }

    #[test]
    fn detect_does_not_false_positive_on_unrelated() {
        // 包含 "code" 但不是 "codex"
        assert_eq!(CliDetector::detect("vscode"), ExternalCli::None);
        assert_eq!(CliDetector::detect("code-server"), ExternalCli::None);
    }

    #[test]
    fn poll_interval_default_700ms() {
        let detector = CliDetector::new();
        assert_eq!(detector.poll_interval(), Duration::from_millis(700));
    }

    #[test]
    fn default_equals_new() {
        let a = CliDetector::new();
        let b = CliDetector::default();
        assert_eq!(a.poll_interval(), b.poll_interval());
    }
}
