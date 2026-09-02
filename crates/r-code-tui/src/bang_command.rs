//! `!command` 直接执行与输出区分（PRD §4.1 R-TUI-07 / M8-03.A3）。
//!
//! `!` 前缀的输入不走 agent——经 r-code-terminal 的本地命令执行（OSC 133
//! prompt/command/output 三段语义）；transcript 中用分区标记把 shell 输出与
//! Agent 工具输出区分（工具输出 = ToolCard；shell 输出 = ShellBlock）。

use crate::TranscriptRow;

/// 判定输入是否为 !command。
pub fn is_bang_command(input: &str) -> bool {
    let trimmed = input.trim_start();
    match trimmed.strip_prefix('!') {
        Some(body) => !body.trim().is_empty(),
        None => false,
    }
}

/// 剥离 `!` 前缀取命令正文。
pub fn command_body(input: &str) -> &str {
    input.trim_start().strip_prefix('!').unwrap_or(input).trim()
}

/// 输入区提示符语义（M4-04.A3：! 态 light-red；lib 层枚举，app 层映射终端色）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptSemantic {
    Normal,
    Bang,
}

/// `!` 起始（含裸 `!`）即 bash 态。
pub fn prompt_semantic(input: &str) -> PromptSemantic {
    if input.trim_start().starts_with('!') {
        PromptSemantic::Bang
    } else {
        PromptSemantic::Normal
    }
}

/// shell 直执行的 transcript 行（与 ToolCard 不同类——渲染与检索都区分）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ShellRow {
    /// OSC 133 语义三段：prompt（命令行本身）/ command / output。
    Prompt { command: String },
    Output {
        text: String,
        exit_code: Option<i32>,
    },
}

/// !command 的完整 transcript 产出（prompt 行 + output 行）。
pub fn shell_rows(command: &str, output: &str, exit_code: Option<i32>) -> Vec<TranscriptRow> {
    // TranscriptRow 需要承载 shell 语义：复用 System 行承载会丢类型区分，
    // 因此扩展 TranscriptRow 为携带 Shell 段（见 lib.rs Shell 变体）。
    vec![
        TranscriptRow::Shell(ShellRow::Prompt {
            command: command.to_string(),
        }),
        TranscriptRow::Shell(ShellRow::Output {
            text: output.to_string(),
            exit_code,
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M8-03.A3：!command 识别与命令剥离。
    #[test]
    fn bang_prefix_detection() {
        assert!(is_bang_command("!cargo test"));
        assert!(is_bang_command("  !ls -la"));
        assert!(!is_bang_command("cargo test"));
        assert!(!is_bang_command("!"), "裸 ! 无命令体不算 !command");
        assert_eq!(command_body("!cargo test"), "cargo test");
        assert_eq!(command_body("  !  ls"), "ls");
    }

    /// M8-03.A3：!command 输出与工具输出区分——Shell 行 ≠ ToolCard，
    /// transcript 类型层可分离两种来源。
    #[test]
    fn shell_output_distinct_from_tool_output() {
        let rows = shell_rows("cargo test", "ok. 3 passed", Some(0));
        assert_eq!(rows.len(), 2);
        assert!(
            matches!(&rows[0], TranscriptRow::Shell(ShellRow::Prompt { command }) if command == "cargo test")
        );
        assert!(matches!(
            &rows[1],
            TranscriptRow::Shell(ShellRow::Output {
                exit_code: Some(0),
                ..
            })
        ));
        // 与工具卡类型互斥（同一 transcript 内可判定来源）。
        let tool_card = TranscriptRow::ToolCard {
            name: "bash".into(),
            summary: "cargo test".into(),
            is_error: false,
        };
        assert_ne!(rows[0], tool_card, "shell 与工具输出类型层不同");
    }
}
