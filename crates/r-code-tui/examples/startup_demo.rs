//! 启动首屏序列复现（对齐截图场景）：先渲染会话引导行（含 \n 的多物理行）+
//! 输入行，再追加一行模型标签变化。PTY 测试断言无撕裂。
//! 运行：cargo run -p r-code-tui --example startup_demo

use r_code_tui::inline_render::InlineRenderer;

fn main() {
    use std::io::Write;
    let mut renderer = InlineRenderer::new();
    let mut stdout = std::io::stdout();
    // 模拟首帧：3 行历史（第 2 行含 \n，会拆成多物理行）+ 2 行输入带。
    let frame1: Vec<String> = vec![
        "R-Code CLI 尚未配置模型服务".to_string(),
        "  1) 桌面端 R-Code Dev「设置 → 模型服务」选择并保存；\n  2) 直接编辑 /root/.../config.toml".to_string(),
        "> ask anything".to_string(),
    ];
    let _ = stdout.write_all(renderer.update(&frame1).as_bytes());
    // 第二帧：追加一行（append），历史保留。
    let mut frame2 = frame1.clone();
    frame2.push("· 新状态行".to_string());
    let _ = stdout.write_all(renderer.update(&frame2).as_bytes());
    println!("\n__STARTUP_END__");
    let _ = stdout.flush();
}
