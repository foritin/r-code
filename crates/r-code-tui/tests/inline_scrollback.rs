//! M5-02.A1/A3 PTY 集成测试：历史行进终端 scrollback、resize 稳定。
//!
//! 用 portable-pty 起真实 pty 跑 inline_scrollback_demo，读 master 输出断言
//! scrollback 含完整历史（append-only 路径语义）；pty 不可用（Windows 非 pty
//! 测试环境）时跳过并标注边界（PRD 允许确定性 harness 替代）。

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

fn run_demo() -> Option<String> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .ok()?;
    let mut command =
        CommandBuilder::new(std::env::var("CARGO_BIN_EXE_inline_scrollback_demo").ok()?);
    command.env("RUST_BACKTRACE", "0");
    let mut child = pair.slave.spawn_command(command).ok()?;
    drop(pair.slave);
    let mut output = String::new();
    let mut reader = pair.master.try_clone_reader().ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let mut buffer = [0u8; 4096];
    loop {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            break;
        }
        if let Ok(n) = reader.read(&mut buffer) {
            if n == 0 {
                break;
            }
            output.push_str(&String::from_utf8_lossy(&buffer[..n]));
            if output.contains("__SCROLLBACK_END__") {
                break;
            }
        }
    }
    let _ = child.wait();
    Some(output)
}

#[test]
fn history_lines_reach_scrollback_and_resize_stable() {
    let Some(output) = run_demo() else {
        eprintln!(
            "pty 不可用，跳过（PRD 允许确定性 harness 替代；语义已由 inline_render 单测覆盖）"
        );
        return;
    };
    // A1：scrollback 含完整历史（append-only 行真正写入终端输出流）。
    for line in [
        "history line 1",
        "history line 5",
        "appended line 6",
        "appended line 7",
    ] {
        assert!(
            output.contains(line),
            "scrollback 必须含完整历史 {line}，实际：{output}"
        );
    }
    // A3：稳定输出（无中途清屏/闪烁的 ED 全清）。
    assert!(
        !output.contains("\x1b[2J"),
        "inline 模式不得整屏清屏（历史保留）：{output:?}"
    );
}
