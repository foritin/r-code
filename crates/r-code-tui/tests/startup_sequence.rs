//! 真实 PTY：启动首屏多物理行 + append 无撕裂（回归 M6-03 截图场景）。
mod common;

const CHILD_ENV: &str = "R_CODE_STARTUP_SEQUENCE_PTY_CHILD";

fn render_demo() {
    use r_code_tui::inline_render::InlineRenderer;
    use std::io::Write;

    let mut renderer = InlineRenderer::new();
    let mut stdout = std::io::stdout();
    println!("__STARTUP_BEGIN__");
    let live1: Vec<String> = vec!["> ".into(), "状态行".into()];
    stdout
        .write_all(
            renderer
                .frame(
                    &[
                        "R-Code CLI 尚未配置模型服务".into(),
                        "  1) 桌面端 R-Code Dev「设置 → 模型服务」选择并保存；\n  2) 直接编辑 config.toml".into(),
                    ],
                    &live1,
                )
                .as_bytes(),
        )
        .expect("write startup frame");
    let live2: Vec<String> = vec!["> ask anything".into(), "状态行 v2".into()];
    stdout
        .write_all(
            renderer
                .frame(&["· 新状态行".to_string()], &live2)
                .as_bytes(),
        )
        .expect("write appended frame");
    println!("\n__STARTUP_END__");
    stdout.flush().expect("flush startup demo");
}

#[test]
fn startup_multi_physical_lines_render_without_tearing() {
    if std::env::var_os(CHILD_ENV).is_some() {
        render_demo();
        return;
    }
    let out = common::run_current_test_in_pty(
        "startup_multi_physical_lines_render_without_tearing",
        CHILD_ENV,
        portable_pty::PtySize {
            rows: 24,
            cols: 90,
            pixel_width: 0,
            pixel_height: 0,
        },
        "__STARTUP_END__",
        std::time::Duration::from_secs(15),
    )
    .expect("startup demo must run inside a PTY");
    let out = out
        .split_once("__STARTUP_BEGIN__")
        .and_then(|(_, output)| output.split_once("__STARTUP_END__"))
        .map(|(output, _)| output)
        .expect("startup markers must delimit renderer output");
    // 含 \n 的引导行被拆成独立物理行，且完整可见。
    assert!(out.contains("1) 桌面端 R-Code Dev"), "引导行可见：{out}");
    assert!(out.contains("2) 直接编辑"), "第二物理行可见：{out}");
    assert!(out.contains("> ask anything"), "输入行可见：{out}");
    // append 后新行可见且无整屏清屏（历史保留）。
    assert!(out.contains("· 新状态行"), "append 行可见：{out}");
    assert!(!out.contains("\x1b[2J"), "不得整屏清屏：{out:?}");
}
