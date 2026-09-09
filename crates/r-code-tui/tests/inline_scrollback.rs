//! M5-02.A1/A3 PTY 集成测试：历史行进终端 scrollback、resize 稳定。
//!
//! 用 portable-pty 起当前测试的子进程，读 master 输出断言 scrollback 含完整历史
//!（append-only 路径语义）。PTY 创建或子进程启动失败必须让测试失败。

mod common;

const CHILD_ENV: &str = "R_CODE_INLINE_SCROLLBACK_PTY_CHILD";

fn render_demo() {
    use r_code_tui::inline_render::InlineRenderer;
    use std::io::Write;

    let mut renderer = InlineRenderer::new();
    let mut stdout = std::io::stdout();
    println!("__SCROLLBACK_BEGIN__");
    let history: Vec<String> = (1..=5).map(|n| format!("history line {n}")).collect();
    let live: Vec<String> = vec!["> ask anything".into()];
    stdout
        .write_all(renderer.frame(&history, &live).as_bytes())
        .expect("write history frame");
    let more: Vec<String> = vec!["appended line 6".into(), "appended line 7".into()];
    stdout
        .write_all(renderer.frame(&more, &live).as_bytes())
        .expect("write appended frame");
    println!("\n__SCROLLBACK_END__");
    stdout.flush().expect("flush scrollback demo");
}

#[test]
fn history_lines_reach_scrollback_and_resize_stable() {
    if std::env::var_os(CHILD_ENV).is_some() {
        render_demo();
        return;
    }
    let output = common::run_current_test_in_pty(
        "history_lines_reach_scrollback_and_resize_stable",
        CHILD_ENV,
        portable_pty::PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        },
        "__SCROLLBACK_END__",
        std::time::Duration::from_secs(20),
    )
    .expect("scrollback demo must run inside a PTY");
    let output = output
        .split_once("__SCROLLBACK_BEGIN__")
        .and_then(|(_, output)| output.split_once("__SCROLLBACK_END__"))
        .map(|(output, _)| output)
        .expect("scrollback markers must delimit renderer output");
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
