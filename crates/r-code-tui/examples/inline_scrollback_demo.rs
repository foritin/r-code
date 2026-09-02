//! PTY 集成演示：历史行只打印一次滚入 scrollback，live 块原位重绘。
//! 运行：cargo run -p r-code-tui --example inline_scrollback_demo

use r_code_tui::inline_render::InlineRenderer;

fn main() {
    use std::io::Write;
    let mut renderer = InlineRenderer::new();
    let mut stdout = std::io::stdout();
    // 帧 1：5 行历史 commit + live 输入行。
    let history: Vec<String> = (1..=5).map(|n| format!("history line {n}")).collect();
    let live: Vec<String> = vec!["> ask anything".into()];
    let _ = stdout.write_all(renderer.frame(&history, &live).as_bytes());
    // 帧 2：再提交 2 行（append-only），live 不变。
    let more: Vec<String> = vec!["appended line 6".into(), "appended line 7".into()];
    let _ = stdout.write_all(renderer.frame(&more, &live).as_bytes());
    println!("\n__SCROLLBACK_END__");
    let _ = stdout.flush();
}
