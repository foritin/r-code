//! `!` 直通执行（M4-04 / R-SHELL-01）。
//!
//! 经宿主 shell 执行链（`LocalShellBackend` = plan_shell 五级解析 + tokio
//! spawn + kill_tree），不开新裸进程通道；输出进 transcript 的 Shell 行（dim，
//! 与 ToolCard 类型层区分）。用户亲手键入 `!` 即用户授权（codex `!` 语义）。

use std::path::Path;

use r_code_gateway::execution_backend::{CommandExecutionBackend, CommandSpec, LocalShellBackend};

/// 执行一条 `!command`，返回 (合并输出, 退出码)。
/// 超时 120s（collect 内部先 kill_tree 再收尾）。
pub async fn run_bang(command: &str, cwd: &Path) -> (String, Option<i32>) {
    let backend = LocalShellBackend::new();
    let spec = CommandSpec {
        command: command.to_string(),
        cwd: cwd.to_path_buf(),
        timeout: std::time::Duration::from_secs(120),
    };
    let handle = match backend.spawn(&spec, None).await {
        Ok(handle) => handle,
        Err(error) => return (format!("启动失败：{error}"), None),
    };
    match backend.collect(handle, &spec, None).await {
        Ok(output) => {
            let mut text = output.stdout;
            if !output.stderr.trim().is_empty() {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&output.stderr);
            }
            (text, output.exit_code)
        }
        Err(error) => (format!("执行失败：{error}"), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M4-04.A1：! 执行输出进 Shell 行（成功含输出 + 退出码 0；失败退出码 1）。
    #[tokio::test]
    async fn bang_execution_collects_output_and_exit_code() {
        let cwd = tempfile::tempdir().expect("tempdir");
        let (output, exit) = run_bang("echo bang-ok", cwd.path()).await;
        assert_eq!(exit, Some(0), "成功退出码");
        assert!(
            output.trim().contains("bang-ok"),
            "stdout 必须进输出：{output}"
        );
        // Shell 行投影：prompt + output（dim 渲染在 app 层，类型层见 ShellRow）。
        let rows = crate::bang_command::shell_rows("echo bang-ok", &output, exit);
        assert!(matches!(
            &rows[1],
            crate::TranscriptRow::Shell(crate::bang_command::ShellRow::Output {
                exit_code: Some(0),
                ..
            })
        ));
        // 失败命令：退出码 1。
        let (_, fail_exit) = run_bang("sh -c 'exit 1'", cwd.path()).await;
        assert_eq!(fail_exit, Some(1), "失败退出码必须透传");
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
