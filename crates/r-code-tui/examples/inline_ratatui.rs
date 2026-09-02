//! M5-01 路线 C PoC：ratatui InlineViewport（视口内重绘语义演示）。
//! 非 TTY 环境直接跳过（CI 可复跑 exit 0）。运行：cargo run -p r-code-tui --example inline_ratatui

use std::io::IsTerminal;

fn main() -> std::io::Result<()> {
    if !std::io::stdout().is_terminal() {
        println!("inline_ratatui PoC: 非 TTY 环境，跳过视口绘制（语义结论见 m5-01-poc-report.md）");
        return Ok(());
    }
    let mut terminal = ratatui::init_with_options(ratatui::TerminalOptions {
        viewport: ratatui::Viewport::Inline(8),
    });
    for frame in 0..3 {
        terminal.draw(|f| {
            f.render_widget(
                ratatui::widgets::Paragraph::new(format!(
                    "frame {frame}: InlineViewport 视口内重绘（历史不进 scrollback）"
                )),
                f.area(),
            );
        })?;
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    ratatui::restore();
    Ok(())
}
