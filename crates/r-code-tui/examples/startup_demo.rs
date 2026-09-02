//! 启动序列复现（回归 M6-03 截图场景）：多物理行历史 commit + live 输入行，
//! 再追加历史 + live 变化。PTY 测试断言无撕裂。
//! 运行：cargo run -p r-code-tui --example startup_demo

use r_code_tui::inline_render::InlineRenderer;

fn main() {
    use std::io::Write;
    let mut renderer = InlineRenderer::new();
    let mut stdout = std::io::stdout();
    // 首帧：2 行历史 commit + 2 行 live（含提示）。
    let live1: Vec<String> = vec!["> ".into(), "状态行".into()];
    let _ = stdout.write_all(
        renderer
            .frame(
                &[
                    "R-Code CLI 尚未配置模型服务".into(),
                    "  1) 桌面端 R-Code Dev「设置 → 模型服务」选择并保存；\n  2) 直接编辑 config.toml".into(),
                ],
                &live1,
            )
            .as_bytes(),
    );
    // 第二帧：追加历史 + live 输入变化。
    let live2: Vec<String> = vec!["> ask anything".into(), "状态行 v2".into()];
    let _ = stdout.write_all(
        renderer
            .frame(&["· 新状态行".to_string()], &live2)
            .as_bytes(),
    );
    println!("\n__STARTUP_END__");
    let _ = stdout.flush();
}
