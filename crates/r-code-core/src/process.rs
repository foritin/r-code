//! Cross-platform process configuration shared by desktop-side command runners.

use std::process::Command;
use std::time::Duration;

/// Prevent a background console process from creating a visible terminal window.
///
/// R-Code captures these commands' output inside its own UI. On Windows, a GUI
/// process must opt out of console creation explicitly or short-lived helpers
/// such as `cmd.exe`, `git.exe`, and `taskkill.exe` flash on the desktop.
#[cfg(windows)]
pub fn hide_background_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub fn hide_background_console(_command: &mut Command) {}

/// 结束子进程及其整棵后代进程树（产品唯一实现）。
///
/// 契约：本函数只负责终止，调用方负责随后 `wait()` 收尸。
///
/// 为什么必须树杀：Windows 上 `child.kill()`（TerminateProcess）只结束直接
/// 子进程——经 `cmd.exe /C call <npm-shim>` 启动的 codex/npm CLI 之下是 node
/// 等后代，单杀 wrapper 会留下持有文件锁/端口的孤儿树。Unix 侧要求调用方以
/// `process_group(0)` 启动子进程，这里向负组 id 发 SIGKILL 回收整组。
/// 树杀失败（如 taskkill 不可用）一律回落 `child.kill()` 兜底。
pub async fn kill_tree(child: &mut tokio::process::Child) {
    #[cfg(windows)]
    if let Some(pid) = child.id() {
        let pid = pid.to_string();
        let mut terminate_tree = tokio::process::Command::new("taskkill");
        terminate_tree
            .args(["/PID", pid.as_str(), "/T", "/F"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        hide_background_console(terminate_tree.as_std_mut());
        // 有界等待：taskkill 卡死不能反过来挂死调用方的 shutdown 路径。
        let _ = tokio::time::timeout(Duration::from_secs(5), terminate_tree.status()).await;
    }
    #[cfg(unix)]
    if let Some(process_group) = child.id().and_then(|pid| i32::try_from(pid).ok()) {
        // SAFETY: 调用方以 process_group(0) 启动该子进程并隔离进程组；负 PID
        // 只命中该组，返回值仅用于 best-effort 清理。
        let _ = unsafe { libc::kill(-process_group, libc::SIGKILL) };
    }
    let _ = child.kill().await;
}

#[cfg(test)]
mod kill_tree_tests {
    use super::*;
    use std::process::Stdio;

    #[cfg(windows)]
    fn long_running_child() -> tokio::process::Command {
        let mut command = tokio::process::Command::new("cmd.exe");
        command
            .args([
                "/D",
                "/S",
                "/C",
                "ping",
                "-n",
                "60",
                "-w",
                "1000",
                "127.0.0.1",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    #[cfg(unix)]
    fn long_running_child() -> tokio::process::Command {
        use std::os::unix::process::CommandExt;

        let mut command = tokio::process::Command::new("sleep");
        command
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command.as_std_mut().process_group(0);
        command
    }

    #[tokio::test]
    async fn kill_tree_terminates_the_child_promptly() {
        let mut child = long_running_child()
            .spawn()
            .expect("spawn long-running child");
        let started = std::time::Instant::now();
        kill_tree(&mut child).await;
        let _ = child.wait().await;
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "kill_tree must not hang; took {:?}",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn kill_tree_is_safe_on_an_already_exited_child() {
        #[cfg(windows)]
        let mut command = {
            let mut command = tokio::process::Command::new("cmd.exe");
            command
                .args(["/D", "/S", "/C", "exit", "0"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            command
        };
        #[cfg(unix)]
        let mut command = {
            use std::os::unix::process::CommandExt;

            let mut command = tokio::process::Command::new("true");
            command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            command.as_std_mut().process_group(0);
            command
        };
        let mut child = command.spawn().expect("spawn short-lived child");
        let _ = child.wait().await;
        // 对已退出进程：taskkill 失败、kill 失败都必须被吞掉而不是 panic。
        kill_tree(&mut child).await;
    }
}
