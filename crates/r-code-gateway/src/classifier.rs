//! Command Classifier -- 对注入文本进行动态风险分类。 [doc-02 §2.3]
//!
//! 用于 `terminal.send` 等动态风险工具：根据目标前台进程类型与注入内容
//! 动态确定风险级别。
//!
//! ## 分类规则 [doc-18 M13-01 2026-07-22 修订 R1->R0]
//! | 场景 | 风险 |
//! |------|------|
//! | TUI/Agent 注入（`is_tui_agent = true`） | R0（内容注入） |
//! | 裸 shell 命令（`is_tui_agent = false`） | R2（命令执行） |
//! | 含控制字符 | R2（潜在注入） |
//!
//! 最终风险 = `max(base, control_char_risk)`（两层封顶）。

use r_code_core::dto::RiskLevel;

/// 对命令进行动态风险分类。
///
/// - `is_tui_agent = true`：目标前台进程是 TUI/Agent（如 claude/codex），
///   注入为内容注入，base = R0。
/// - `is_tui_agent = false`：裸 shell，注入为命令执行，base = R2。
/// - 若文本含控制字符（`has_control_chars`），封顶至 R2。
///
/// [doc-02 §2.3] [doc-18 M13-01 2026-07-22 修订 R1->R0]
pub fn classify_command(command: &str, is_tui_agent: bool) -> RiskLevel {
    // 第一层：基础风险
    let base = if is_tui_agent {
        RiskLevel::R0
    } else {
        RiskLevel::R2
    };
    // 第二层：控制字符封顶
    let control_floor = if has_control_chars(command) {
        RiskLevel::R2
    } else {
        RiskLevel::R0
    };
    max_risk(base, control_floor)
}

/// 检查文本是否包含控制字符（排除常见空白符 `\n` `\r` `\t`）。
pub fn has_control_chars(text: &str) -> bool {
    text.chars()
        .any(|c| c.is_control() && c != '\n' && c != '\r' && c != '\t')
}

/// 检测进程是否为外部 CLI Agent（claude / codex）。
pub fn is_agent_process(process_name: &str) -> bool {
    let name = process_name.to_lowercase();
    name.contains("claude") || name.contains("codex")
}

/// 返回两个风险等级中较高者。
fn max_risk(a: RiskLevel, b: RiskLevel) -> RiskLevel {
    let rank = |r: RiskLevel| match r {
        RiskLevel::R0 => 0,
        RiskLevel::R1 => 1,
        RiskLevel::R2 => 2,
        RiskLevel::R3 => 3,
        RiskLevel::R4 => 4,
    };
    if rank(a) >= rank(b) {
        a
    } else {
        b
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_shell_is_r2() {
        assert_eq!(classify_command("ls -la", false), RiskLevel::R2);
        assert_eq!(classify_command("rm -rf /", false), RiskLevel::R2);
        assert_eq!(classify_command("", false), RiskLevel::R2);
    }

    #[test]
    fn tui_agent_is_r0() {
        assert_eq!(classify_command("hello world", true), RiskLevel::R0);
        assert_eq!(classify_command("type some text", true), RiskLevel::R0);
        assert_eq!(classify_command("", true), RiskLevel::R0);
    }

    #[test]
    fn control_chars_bump_to_r2() {
        // 控制字符将 TUI/Agent 从 R0 提升到 R2
        assert_eq!(classify_command("hello\x03world", true), RiskLevel::R2);
        assert_eq!(classify_command("hello\x07", true), RiskLevel::R2);
        // 裸 shell 本就是 R2，控制字符不变
        assert_eq!(classify_command("ls\x03", false), RiskLevel::R2);
    }

    #[test]
    fn common_whitespace_not_control() {
        // \n \r \t 不算控制字符
        assert!(!has_control_chars("hello\nworld"));
        assert!(!has_control_chars("hello\tworld"));
        assert!(!has_control_chars("hello\r\nworld"));
        assert_eq!(classify_command("multi\nline\ntext", true), RiskLevel::R0);
    }

    #[test]
    fn has_control_chars_detection() {
        assert!(has_control_chars("\x01"));
        assert!(has_control_chars("text\x1b["));
        assert!(!has_control_chars("normal text"));
        assert!(!has_control_chars(""));
        assert!(!has_control_chars("tab\there"));
    }

    #[test]
    fn is_agent_process_detection() {
        assert!(is_agent_process("claude"));
        assert!(is_agent_process("codex"));
        assert!(is_agent_process("claude-code"));
        assert!(is_agent_process("CODEX"));
        assert!(is_agent_process("/usr/bin/claude"));
        assert!(!is_agent_process("bash"));
        assert!(!is_agent_process("zsh"));
        assert!(!is_agent_process("vim"));
    }

    #[test]
    fn max_risk_helper() {
        assert_eq!(max_risk(RiskLevel::R0, RiskLevel::R2), RiskLevel::R2);
        assert_eq!(max_risk(RiskLevel::R2, RiskLevel::R0), RiskLevel::R2);
        assert_eq!(max_risk(RiskLevel::R3, RiskLevel::R4), RiskLevel::R4);
        assert_eq!(max_risk(RiskLevel::R1, RiskLevel::R1), RiskLevel::R1);
    }
}
