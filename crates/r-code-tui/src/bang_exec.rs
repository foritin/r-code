//! `!` 直通执行（M4-04 / R-SHELL-01）。
//!
//! P24A 起 `!` 不再持有 TUI 进程内的 LocalShellBackend 裸通道：宿主的
//! Shell 面要到 P28 才会按「exact-approved sandboxed Shell」暴露，在那之前
//! 这里以 Unsupported 拒绝（fail-closed），本会话不执行任何命令、不开任何
//! 新裸进程通道。输出仍进 transcript 的 Shell 行（dim，与 ToolCard 类型层
//! 区分）。

/// 执行一条 `!command`：宿主尚未暴露受监督的 Shell，如实返回 Unsupported。
/// 返回 (说明文本, 退出码=None)；命令未被执行。
pub async fn run_bang(_command: &str, _cwd: &std::path::Path) -> (String, Option<i32>) {
    (
        "！直通执行暂不可用：宿主尚未暴露受监督的 Shell（P28 前保持关闭）；本次未执行任何命令。"
            .to_string(),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P24A：`!` 在宿主暴露受监督 Shell 之前 fail-closed——命令不执行，
    /// 退出码为 None，输出如实说明原因（不再借道 LocalShellBackend）。
    #[tokio::test]
    async fn bang_refuses_until_the_host_exposes_a_sandboxed_shell() {
        let cwd = tempfile::tempdir().expect("tempdir");
        let (output, exit) = run_bang("echo must-not-run", cwd.path()).await;
        assert_eq!(exit, None, "未执行的命令没有退出码");
        assert!(
            output.contains("暂不可用"),
            "输出必须如实说明不可用：{output}"
        );
        assert!(!output.contains("must-not-run"), "命令不得被执行：{output}");
        // Shell 行投影仍可用（prompt + 说明行；dim 渲染在 app 层）。
        let rows = crate::bang_command::shell_rows("!echo must-not-run", &output, exit);
        assert!(matches!(
            &rows[1],
            crate::TranscriptRow::Shell(crate::bang_command::ShellRow::Output {
                exit_code: None,
                ..
            })
        ));
    }

    /// M4-04.A3：! 输入态的提示符语义色（light-red 由 app 层映射）。
    #[test]
    fn bang_input_switches_prompt_semantic() {
        assert_eq!(
            crate::bang_command::prompt_semantic("!cargo test"),
            crate::bang_command::PromptSemantic::Bang
        );
        assert_eq!(
            crate::bang_command::prompt_semantic("!"),
            crate::bang_command::PromptSemantic::Bang,
            "输入 ! 即进入 bash 态（命令未完）"
        );
        assert_eq!(
            crate::bang_command::prompt_semantic("normal text"),
            crate::bang_command::PromptSemantic::Normal
        );
    }
}
