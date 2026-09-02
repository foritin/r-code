# M5-01 inline 渲染路线 PoC 基准报告

运行：`cargo run -p r-code-tui --example inline_bench`（确定性模拟：120 帧，20 行静态历史起步，每 10 帧追加 1 行，spinner 每帧变化，输入行 1 条）

## 写入字节数（终端工作 + 闪烁面代理指标）

| 路线 | 写入字节/帧（均值） | 语义 |
| --- | --- | --- |
| A. 自研行差分（inline_render.rs，CSI ?2026 包裹，append-only 续写） | 200 | 历史行真正滚入终端 scrollback；spinner 帧仅重写 1 行 |
| B. 朴素全量重绘 | 2069 | 对照基线（最差情形） |
| C. ratatui InlineViewport（视口内全量重绘，高 10） | 728 | 历史锁在固定视口内，不进 scrollback |

差分/朴素 = 9.6%；差分/viewport = 27.4%。

## 语义对照（PRD 冻结评判维度）

| 维度 | A 自研行差分 | C ratatui InlineViewport |
| --- | --- | --- |
| scrollback 语义 | ✅ 历史行经 append-only 路径滚入宿主终端 scrollback，退出保留 | ❌ 视口内重绘，历史不进 scrollback（需自行打印 + 视口重定位） |
| 闪烁 | ✅ CSI ?2026 同步输出包裹 + 行级差分 | ✅ 框架层缓冲（视口内） |
| resize | ⚠️ 自管（宽度变化全量重绘） | ✅ 框架处理 |
| 单帧写入 | ✅ 仅差分行 | 视口全量（高度 × 宽度） |

## 定案

**选 A（自研行差分，`crates/r-code-tui/src/inline_render.rs`）**。决定性依据：scrollback 语义完整性是 PRD §2.4 的硬要求（历史进终端 scrollback、退出保留），InlineViewport 的视口内重绘语义与之冲突（被否路线的差距 = 历史锁视口 + 每帧视口全量写入）；差分路线的 resize 自管成本可接受（宽度变化触发全量重绘即可）。被否路线差距量化：写入字节高 3.6 倍且不满足 scrollback 判据。
