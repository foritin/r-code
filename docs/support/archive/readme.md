# 文档归档

这里保存已经实施、已完成验收、被替代，或不再作为当前执行依据的一次性文档和原型。

归档表示“保留历史证据”，不表示内容错误。代码注释和历史变更记录可以继续链接到
这些文件；开发与运维决策应优先参考 [`docs/readme.md`](../../readme.md) 列出的当前文档。

## 基线与外部适配

| 文档 | 归档原因 |
| --- | --- |
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

## 历史原型

[`prototypes/`](./prototypes/) 保存旧房间页、设置页交互原型、截图及其历史辅助脚本。它们只用于设计追溯，不能替代当前 UI、自动化测试或验收证据。
