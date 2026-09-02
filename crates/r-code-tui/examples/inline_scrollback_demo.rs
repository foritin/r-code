//! M5-02 PTY 集成演示：真实终端的 scrollback 语义验证载体。
//! 模拟历史滚动：先打印 5 行历史（会滚入 scrollback），再 append 2 行，
//! 然后输出一个明确的 END 标记供测试断言。非 TTY 也能跑（直接 stdout）。
//! 运行：cargo run -p r-code-tui --example inline_scrollback_demo

use r_code_tui::inline_render::InlineRenderer;

fn main() {
    use std::io::Write;
    let mut renderer = InlineRenderer::new();
    let mut stdout = std::io::stdout();
    // 首帧：5 行历史（写入后光标停在块尾）。
    let history: Vec<String> = (1..=5).map(|n| format!("history line {n}")).collect();
    let _ = stdout.write_all(renderer.update(&history).as_bytes());
    // 追加 2 行（append-only：历史行真正滚入终端 scrollback）。
    let mut extended = history;
    extended.push("appended line 6".into());
    extended.push("appended line 7".into());
    let _ = stdout.write_all(renderer.update(&extended).as_bytes());
    // 明确 END 标记（测试断言 scrollback 含完整历史）。
    println!("\n__SCROLLBACK_END__");
    let _ = stdout.flush();
}
