//! M5-01 渲染路线基准：自研行差分 vs ratatui InlineViewport（viewport 全量重绘语义）
//! vs 朴素全量重绘。确定性模拟（无终端依赖），报告写入 docs/tui-v2/m5-01-poc-report.md。
//!
//! 运行：cargo run -p r-code-tui --example inline_bench

use r_code_tui::inline_render::InlineRenderer;

fn main() {
    // 模拟会话：20 行静态历史 + 1 行 spinner（每帧变）+ 1 行输入；每 10 帧追加一行历史。
    const FRAMES: usize = 120;
    const SPIN: [&str; 4] = ["⠋", "⠙", "⠹", "⠸"];
    let width_line = |n: usize| format!("history line {n}: {}", "x".repeat(60));

    let mut history: Vec<String> = (0..20).map(width_line).collect();
    let mut diff = InlineRenderer::new();
    let mut diff_committed = 0usize;
    let mut diff_bytes = 0usize;
    let mut naive_bytes = 0usize;
    let mut viewport_bytes = 0usize;
    const VIEWPORT_HEIGHT: usize = 10; // ratatui InlineViewport 固定视口高

    for frame in 0..FRAMES {
        if frame % 10 == 0 && frame > 0 {
            history.push(width_line(20 + frame / 10));
        }
        let next = {
            let mut lines = history.clone();
            lines.push(format!(
                "{} Working ({}s · esc to interrupt)",
                SPIN[frame % 4],
                frame / 4
            ));
            lines.push("› ask anything".to_string());
            lines
        };
        // 路线 A：commit/live 渲染（真实核心）——历史 commit 一次，
        // live 区（spinner+输入）每帧重绘。
        let (tail, live): (Vec<String>, Vec<String>) = {
            let split = next.len().saturating_sub(2);
            (next[..split].to_vec(), next[split..].to_vec())
        };
        let new_commit: Vec<String> = tail[diff_committed..].to_vec();
        diff_committed = tail.len();
        diff_bytes += diff.frame(&new_commit, &live).len();
        // 路线 B：朴素全量重绘（基线下界对照）。
        naive_bytes += next.iter().map(|l| l.len() + 2).sum::<usize>() + 8;
        // 路线 C：ratatui InlineViewport 语义 = 视口内全量重绘（高 ≤10 行，
        // 每帧重写视口全部行 + 光标复位序列）。
        let shown = next.len().min(VIEWPORT_HEIGHT);
        viewport_bytes += (0..shown).map(|_| 64 + 8).sum::<usize>() + 8;
    }

    let report = format!(
        "# M5-01 inline 渲染路线 PoC 基准报告\n\n\
运行：`cargo run -p r-code-tui --example inline_bench`（确定性模拟：{FRAMES} 帧，20 行静态历史起步，每 10 帧追加 1 行，spinner 每帧变化，输入行 1 条）\n\n\
## 写入字节数（终端工作 + 闪烁面代理指标）\n\n\
| 路线 | 写入字节/帧（均值） | 语义 |\n| --- | --- | --- |\n\
| A. 自研行差分（inline_render.rs，CSI ?2026 包裹，append-only 续写） | {:.0} | 历史行真正滚入终端 scrollback；spinner 帧仅重写 1 行 |\n\
| B. 朴素全量重绘 | {:.0} | 对照基线（最差情形） |\n\
| C. ratatui InlineViewport（视口内全量重绘，高 {VIEWPORT_HEIGHT}） | {:.0} | 历史锁在固定视口内，不进 scrollback |\n\n\
差分/朴素 = {:.1}%；差分/viewport = {:.1}%。\n\n\
## 语义对照（PRD 冻结评判维度）\n\n\
| 维度 | A 自研行差分 | C ratatui InlineViewport |\n| --- | --- | --- |\n\
| scrollback 语义 | ✅ 历史行经 append-only 路径滚入宿主终端 scrollback，退出保留 | ❌ 视口内重绘，历史不进 scrollback（需自行打印 + 视口重定位） |\n\
| 闪烁 | ✅ CSI ?2026 同步输出包裹 + 行级差分 | ✅ 框架层缓冲（视口内） |\n\
| resize | ⚠️ 自管（宽度变化全量重绘） | ✅ 框架处理 |\n\
| 单帧写入 | ✅ 仅差分行 | 视口全量（高度 × 宽度） |\n\n\
## 定案\n\n\
**选 A（自研行差分，`crates/r-code-tui/src/inline_render.rs`）**。决定性依据：scrollback 语义完整性是 PRD §2.4 的硬要求（历史进终端 scrollback、退出保留），InlineViewport 的视口内重绘语义与之冲突（被否路线的差距 = 历史锁视口 + 每帧视口全量写入）；差分路线的 resize 自管成本可接受（宽度变化触发全量重绘即可）。被否路线差距量化：写入字节高 {:.1} 倍且不满足 scrollback 判据。\n",
        diff_bytes as f64 / FRAMES as f64,
        naive_bytes as f64 / FRAMES as f64,
        viewport_bytes as f64 / FRAMES as f64,
        diff_bytes as f64 / naive_bytes as f64 * 100.0,
        diff_bytes as f64 / viewport_bytes as f64 * 100.0,
        viewport_bytes as f64 / diff_bytes as f64,
    );
    let path = std::path::Path::new("docs/tui-v2/m5-01-poc-report.md");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir docs");
    }
    std::fs::write(path, &report).expect("write report");
    println!("report written: {}", path.display());
    println!(
        "diff/frame ≈ {:.0}B, naive/frame ≈ {:.0}B, viewport/frame ≈ {:.0}B",
        diff_bytes / FRAMES,
        naive_bytes / FRAMES,
        viewport_bytes / FRAMES
    );
}
