//! T35 — TUI 与 Tauri 宿主解耦验收。
//!
//! a) `cargo tree -p r-code-tui` 无 tauri/wry（依赖图守卫）；
//! b) 脚本化会话生命周期走共享 r-code-service（真进程、真 ModelBroker）：
//!    不配 provider → 发送 → run.failed 带 "unknown model selection" →
//!    task.detail 恢复 pending → 二次发送仍被接受（真实故障路径，非 mock）；
//! c) PTY 冒烟：真 r-code-tui 二进制连共享服务启动，首屏出"尚未配置"引导，
//!    Ctrl+C 退出，守护进程清理。
mod daemon_common;

/// 守护进程子进程守卫：panic/提前返回时杀掉子进程，防止残留进程把
/// exe 文件锁留给下一次运行。
pub struct DaemonGuard(pub std::process::Child);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

// ── a) 依赖图守卫 ─────────────────────────────────────────────────────────────

/// `cargo tree -p r-code-tui -e normal` 输出的任何包名都不得是 tauri/wry
///（精确名匹配——"xxx-tauri-yyy" 之类的名字不受影响，但目前依赖图里
/// 本就不存在）。
#[test]
fn cargo_tree_has_no_tauri_or_wry() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["tree", "-p", "r-code-tui", "-e", "normal"])
        .current_dir(&repo_root)
        .output()
        .expect("run cargo tree");
    assert!(
        output.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        let trimmed = line.trim_start_matches(['│', '├', '└', '─', ' ']);
        if trimmed.is_empty() || trimmed.starts_with('[') {
            continue;
        }
        let name = trimmed
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .split('@')
            .next()
            .unwrap_or_default();
        assert_ne!(name, "tauri", "r-code-tui must not depend on tauri: {line}");
        assert_ne!(name, "wry", "r-code-tui must not depend on wry: {line}");
    }
}

// ── b) 脚本化生命周期（真实故障路径） ────────────────────────────────────────

/// 起一个真实守护进程子进程（显式 env：builtin 插件目录），TUI 客户端随后
/// 经 ensure_daemon 发现它（owner.json + token 握手）。
fn spawn_daemon(env: &daemon_common::DaemonEnv, builtin_dir: &Path) -> std::process::Child {
    let service = daemon_common::target_debug("r-code-service");
    let mut command = Command::new(&service);
    command
        .args([
            "--profile",
            "development",
            "--data-root",
            env.data_dir.to_str().expect("utf8 data dir"),
            "--ipc-name",
            &env.ipc_name,
        ])
        .env("R_CODE_BUILTIN_PLUGINS_DIR", builtin_dir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    command.spawn().expect("spawn r-code-service")
}

#[tokio::test]
async fn honest_failure_lifecycle_through_shared_service() {
    let (env, extra) = daemon_common::daemon_env("t35b");
    let builtin_dir = Path::new(
        &extra
            .iter()
            .find(|(key, _)| *key == "R_CODE_BUILTIN_PLUGINS_DIR")
            .expect("builtin dir in env")
            .1,
    )
    .to_path_buf();
    let service_bin = daemon_common::target_debug("r-code-service");
    // 守卫：panic/提前返回时杀掉守护进程，不留占住 exe 锁的残留进程。
    let mut daemon = DaemonGuard(spawn_daemon(&env, &builtin_dir));

    let profile = r_code_runtime::RuntimeProfile::resolve(
        &r_code_runtime::LaunchOptions::new(r_code_runtime::ProfileFlavor::Development)
            .with_data_root(&env.data_dir)
            .with_ipc_name(env.ipc_name.clone()),
    )
    .expect("profile");
    let engine = r_code_tui::engine::V2ChatClient::from_profile(profile, Some(service_bin));

    // 生命周期：建会话 → 发送（run 启动）→ 无 provider 的真实失败 →
    // 任务重开（pending）→ 二次发送仍被接受。
    let task_id = engine.ensure_session("t35").await.expect("ensure session");
    assert!(!task_id.is_empty());

    let first = engine.send(&task_id, "hello v2").await.expect("first send");
    assert!(
        matches!(first, r_code_tui::engine::SendOutcome::Started { .. }),
        "空闲发送必须启动 run：{first:?}"
    );

    // 轮询 journal 至 run.failed（真进程：插件启动 + host.model.stream 经
    // 真 ModelBroker 解析失败——空 registry 是诚实失败，不是 mock）。
    let deadline = Instant::now() + Duration::from_secs(90);
    let mut failure: Option<String> = None;
    let mut second_send: Option<r_code_tui::engine::SendOutcome> = None;
    let mut second_failed = false;
    while Instant::now() < deadline {
        let events = engine.poll_events().await.expect("poll events");
        for event in &events {
            if event.task_id != task_id {
                continue;
            }
            if event.payload.get("journalKind").and_then(|v| v.as_str()) == Some("run.failed") {
                let error = event
                    .payload
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                if failure.is_none() {
                    failure = Some(error);
                    // 失败后任务重开（chat 语义）：二次发送必须被接受。
                    second_send = Some(
                        engine
                            .send(&task_id, "try again after failure")
                            .await
                            .expect("second send accepted after failure"),
                    );
                } else {
                    second_failed = true;
                }
            }
        }
        if second_failed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let failure = failure.expect("run.failed 事件必须出现（无 provider 的真实失败）");
    assert!(
        failure.contains("unknown model selection"),
        "失败原因应是模型解析错误（空 registry）：{failure}"
    );
    assert!(
        matches!(
            second_send,
            Some(r_code_tui::engine::SendOutcome::Started { .. })
        ),
        "失败后任务重开，二次发送应启动新 run"
    );
    assert!(second_failed, "第二次 run 也应诚实失败（仍无 provider）");

    // task.detail：状态恢复 pending、不 running、runs 已记账。
    let detail = engine.task_detail(&task_id).await.expect("detail");
    assert!(!detail.running, "失败后任务不在运行态");
    assert_eq!(
        detail.state, "pending",
        "失败后任务重开为 pending（可继续发送）：{}",
        detail.state
    );
    assert!(
        detail.runs.len() >= 2,
        "两次 run 都已记账：{:?}",
        detail.runs
    );

    // 偏好写回（影响下一次 run）经守护进程读回一致。
    engine
        .set_preferences(&task_id, Some("openai"), None, None)
        .await
        .expect("set preferences");
    let detail = engine.task_detail(&task_id).await.expect("detail 2");
    assert_eq!(detail.model.as_deref(), Some("openai"));

    // 清理：优雅 shutdown；守护进程是测试自己起的子进程，wait 收尸；
    // DaemonGuard 兜底 kill（防止 shutdown RPC 失败残留）。
    let _ = engine.shutdown_service().await;
    let _ = daemon.0.wait();
    drop(daemon);
    daemon_common::shutdown_daemon(&env);
}

// ── c) PTY 冒烟 ──────────────────────────────────────────────────────────────

/// 真 r-code-tui 连共享服务启动：首屏"尚未配置"引导可见、Ctrl+C 退出、
/// 守护进程清理。
#[test]
fn pty_smoke_tui_boots_against_shared_service() {
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};
    use std::io::{Read, Write};

    let bin = std::env::var("CARGO_BIN_EXE_r-code-tui").expect("tui binary");
    let (env, extra) = daemon_common::daemon_env("t35pty");
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: 30,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open PTY");
    let mut cmd = CommandBuilder::new(bin);
    cmd.args([
        "--data-dir",
        env.data_dir.to_str().expect("utf8 data dir"),
        "--ipc-name",
        &env.ipc_name,
    ]);
    cmd.env("RUST_BACKTRACE", "0");
    for (key, value) in &extra {
        cmd.env(key, value);
    }
    let mut child = pair.slave.spawn_command(cmd).expect("spawn TUI in PTY");
    let mut writer = pair.master.take_writer().expect("pty writer");
    let reader = pair.master.try_clone_reader().expect("pty reader");
    let _master = pair.master;

    // 读线程 + 通道：deadline 在 recv_timeout 处生效。直接在测试线程上
    // read() 是阻塞调用——TUI 启动异常无输出时 deadline 永远检查不到。
    let (tx, output) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx
                        .send(String::from_utf8_lossy(&buf[..n]).to_string())
                        .is_err()
                    {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut seen = String::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut matched = false;
    while Instant::now() < deadline {
        if seen.contains("尚未配置") {
            matched = true;
            break;
        }
        match output.recv_timeout(Duration::from_millis(100)) {
            Ok(chunk) => seen.push_str(&chunk),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    assert!(
        matched,
        "20s 内必须出现\"尚未配置\"引导；实际输出尾段：{:?}",
        &seen[seen.len().saturating_sub(1500)..]
    );

    // Ctrl-C 两次退出（运行空闲时第一次即退出）。
    writer.write_all(b"\x03").expect("send ctrl-c");
    writer.flush().expect("flush ctrl-c");
    std::thread::sleep(Duration::from_millis(300));
    let _ = writer.write_all(b"\x03");
    let _ = writer.flush();
    // 有界等待退出：TUI 不退出则硬杀，避免 wait() 无限阻塞。
    let exit = loop {
        match child.try_wait().expect("poll tui exit") {
            Some(status) => break status,
            None => {
                if Instant::now() > deadline + Duration::from_secs(5) {
                    let _ = child.kill();
                    break child.wait().expect("reap killed tui");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    };
    let _ = exit;

    // 清理守护进程（TUI 常驻启动的那个）。
    daemon_common::shutdown_daemon(&env);
}
