# 文档归档

这里保存已经实施、已完成验收、被替代，或不再作为当前执行依据的一次性文档和原型。

归档表示“保留历史证据”，不表示内容错误。代码注释和历史变更记录可以继续链接到
这些文件；开发与运维决策应优先参考 [`docs/readme.md`](../../readme.md) 列出的当前文档。

## 基线与外部适配

| 文档 | 归档原因 |
| --- | --- |
| [可插拔 Harness PRD](./pluggable-harness-prd/README.md) | 原计划、任务、评审、进度与 E2E 规划已完成历史职责；后续统一执行依据为 [R-Code v1 PRD](./prd/r-code-v1/index.md) |
| [重构前架构基线](./architecture-before-pluggable-harness.md) | 2026-09-09 按文档整理决策归档，保留当时实际实现；后续方案见 [可插拔 Harness PRD](../harness/plan.md) |
| [DeepSeek 前缀缓存 PRD](./deepseek-prefix-cache.md) | 分阶段方案已实施并完成主要验收，保留设计与例外记录 |
| [DeepSeek 缓存基线](./deepseek-cache-baseline.md) | 一次性真实 API 测量已完成，保留发布门槛证据 |
| [DeepSeek Harness 可借鉴性评估](./deepseek-harness.md) | 调研与差距分析已完成，相关能力已落地 |
| [Ark/Kimi Provider 适配方案](./ark-kimi-provider-adaptation-plan.md) | 适配已实施，保留方案与验收记录 |

## 阶段性实施方案

以下文件集中在 [`implementation/`](./implementation/)；代码注释可以继续把它们作为历史设计依据，但它们不是当前待办：

| 文档 | 归档原因 |
| --- | --- |
| [Harness 三层迁移清单](./implementation/harness-migration.md) | 分阶段架构迁移材料，长期边界已由代码与架构文档承接 |
| [多模态附件与 DeepSeek Plan 锚定规格](./implementation/multimodal-attachments-and-deepseek-plan-anchoring-implementation.md) | 实施规格已落地，保留数据/预算/迁移决策 |
| [DeepSeek Plan 建议与双轨设计](./implementation/plan-mode-dual-track-gate.md) | 阶段性 PRD 已实施或被后续方案修订 |
| [请求审计与首轮锚定实验](./implementation/request-audit-and-anchoring.md) | 实验及落地报告已完成 |
| [设置体验与图片理解实施方案](./implementation/settings-ux-and-image-understanding.md) | 实施方案已由当前设置 UI、测试和维护文档承接 |
| [广度编排与思考效率工作清单](./implementation/breadth-orchestration-and-thinking-efficiency.md) | 未进入当前产品执行链的阶段性草案，连同固化文件保留 |
| [广度工作清单固化记录](./implementation/breadth-orchestration-freeze.yaml) | 与上项配套的历史 draft，不作为当前固化状态 |

## 已实施 PRD（2026-10-04 归档）

五个 PRD 计划目录从 `docs/prd/` 整体迁入 [`prd/`](./prd/)，`docs/prd/` 清空为后续新计划的工作区：

| 计划 | 归档原因 |
| --- | --- |
| [R-Code v1 统一架构与实施](./prd/r-code-v1/index.md) | P-GATE 9/9、M-GATE 4/4、Safety 5/39 KEEP；当前执行入口职责已由代码与维护文档承接 |
| [远程控制](./prd/remote-control/index.md) | 独立产品域，实现/放行状态见其 worklist；不再作为当前待办状态源 |
| [项目上下文引擎](./prd/project-context/prd.md) | M1a 项目上下文引擎已落地（O-GATE KEEP 成果入库），验收证据随目录保留 |
| [Agent Loop 韧性](./prd/agent-loop-resilience/prd.md) | `implemented`（v2.1）：A00–A15 机器任务落地，覆盖流完成性、重放、预算接力、队列/重启恢复、幂等栅栏、工具并行与取消/steer |
| [执行波次租约生命周期](./prd/execution-wave-leases/prd.md) | `implemented`（L01–L08）：取消先杀后放租、非 Started 退出零泄漏、CAS 调和保事件、瞬时扫描失败 defer |

原 `docs/prd/pluggable-harness/` 参考集（架构、协议、开发指南）为活文档，未归档，迁至 [`docs/support/harness/`](../harness/)。

## 历史原型

[`prototypes/`](./prototypes/) 保存旧房间页、设置页交互原型、截图及其历史辅助脚本。它们只用于设计追溯，不能替代当前 UI、自动化测试或验收证据。

## 整目录归档（2026-08-30）

| 目录 | 归档原因 |
| --- | --- |
| [`product-experience-redesign/`](./product-experience-redesign/) | 产品体验重构已完成闭环（`frozen`，42/42），可点击原型、PRD、设置能力盘点与门禁报告已由当前实现承接，不再作为活跃入口 |
| [`code-review-2026-08-29/`](./code-review-2026-08-29/) | 2026-08-29 全仓 Code Review 已完成（21/21 修复、2332:0 全绿），证据与发现报告保留作历史审计 |

## 顶层文档整理（2026-09-09）

| 旧位置 | 归档位置 | 说明 |
| --- | --- | --- |
| `docs/architecture.md` | [architecture-before-pluggable-harness.md](./architecture-before-pluggable-harness.md) | 旧实现参考；归档不表示新架构已经落地 |
| `docs/tui-v1/` | [tui-v1/](./tui-v1/) | 调研、决策、PRD、freeze、原型和基准报告一并保留；原有未提交内容随文件移动 |

TUI 冻结正文和历史报告不因移动而重算或覆盖。代码／旧文档中的历史路径按本表解释；活跃脚本已改为读取归档位置，新生成的 inline 基准报告写入 `artifacts/metrics/tui-v1/`。
