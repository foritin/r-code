//! 真实 PTY：启动首屏多物理行 + append 无撕裂（回归 M6-03 截图场景）。
use portable_pty::{native_pty_system, CommandBuilder, PtySize};

fn run_demo() -> Option<String> {
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: 24,
            cols: 90,
            pixel_width: 0,
            pixel_height: 0,
        })
        .ok()?;
    let mut cmd = CommandBuilder::new(std::env::var("CARGO_BIN_EXE_startup_demo").ok()?);
    cmd.env("RUST_BACKTRACE", "0");
    let mut child = pair.slave.spawn_command(cmd).ok()?;
    drop(pair.slave);
    let mut out = String::new();
    let mut r = pair.master.try_clone_reader().ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let mut buf = [0u8; 4096];
    loop {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            break;
        }
        match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                out.push_str(&String::from_utf8_lossy(&buf[..n]));
                if out.contains("__STARTUP_END__") {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let _ = child.wait();
    Some(out)
}

#[test]
fn startup_multi_physical_lines_render_without_tearing() {
    let Some(out) = run_demo() else {
        eprintln!("pty 不可用，跳过（确定性语义已由 display/inline_render 单测覆盖）");
        return;
    };
    // 含 \n 的引导行被拆成独立物理行，且完整可见。
    assert!(out.contains("1) 桌面端 R-Code Dev"), "引导行可见：{out}");
    assert!(out.contains("2) 直接编辑"), "第二物理行可见：{out}");
    assert!(out.contains("> ask anything"), "输入行可见：{out}");
    // append 后新行可见且无整屏清屏（历史保留）。
    assert!(out.contains("· 新状态行"), "append 行可见：{out}");
    assert!(!out.contains("\x1b[2J"), "不得整屏清屏：{out:?}");
}
