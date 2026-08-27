# R-Code 支持材料索引

`docs/` 根目录只保留当前导航、当前架构和活跃产品体验合同。本目录收纳仍有维护或追溯价值、但不应与当前权威入口并列的指南、运维资料、平台清单、已完成合同、历史 UI 与归档。

## 目录

| 目录 | 内容 | 权威边界 |
| --- | --- | --- |
| [`guides/`](./guides/) | MCP、演进记忆、Plan 模式等专题指南 | 解释当前能力，若与测试代码冲突以测试代码为准 |
| [`operations/`](./operations/) | 安装/备份/恢复与发布手册 | 用户和发布维护者的操作入口 |
| [`platform/`](./platform/) | macOS 等真机验证清单 | 不能由其他平台 fixture 冒充通过 |
| [`contracts/`](./contracts/) | 已完成 revision 的冻结 PRD/freeze | 历史实施合同，不是本轮待办状态源 |
| [`ui-reference/legacy/`](./ui-reference/legacy/) | 旧亮/暗 UI 截图 | 仅供视觉追溯，不证明当前实现 |
| [`archive/`](./archive/) | 一次性方案、实验基线、历史原型和阶段决策 | 不作为当前产品要求或未完成 Checklist |

当前活跃入口：

- [文档导航](../readme.md) / [English](../readme.en.md)
- [当前架构](../architecture.md)
- [产品体验重构原型与实施合同](../product-experience-redesign/)

## 旧路径迁移

| 旧路径 | 新路径 |
| --- | --- |
| `docs/mcp.md` | `docs/support/guides/mcp.md` |
| `docs/memory.md` | `docs/support/guides/memory.md` |
| `docs/plan-mode.md` / `plan-mode.en.md` | `docs/support/guides/` |
| `docs/operations.md` / `operations.en.md` / `releasing.md` | `docs/support/operations/` |
| `docs/macos-validation.md` | `docs/support/platform/macos-validation.md` |
| `docs/codex-rich-interaction-*` | `docs/support/contracts/codex-rich-interaction-*` |
| `docs/windows-command-reliability-*` | `docs/support/contracts/windows-command-reliability-*` |
| `docs/ui/**` | `docs/support/ui-reference/legacy/**` |
| `docs/archive/**` | `docs/support/archive/**` |

OCR 单测原来从 `docs/ui` 编译图片。该资产已按相同字节和 SHA-256 `10177db7cd6bb1265c95c66518385c50910d53e9d0cd94fea95ca7ed2d8723aa` 独立到 `fixtures/windows-ocr/deepseek-model-configuration-dark.png`，文档整理不再决定测试能否编译。

## 历史证据不可变

以下历史 evidence/verification 记录的是当时 revision 和旧路径上的事实，路径字符串没有因为本次文档移动而批量改写：

- `artifacts/ai-tasks/evidence/codex-rich-interaction/`
- `artifacts/ai-tasks/verification/codex-rich-interaction/implementation/`
- `artifacts/ai-tasks/evidence/windows-reliability/`
- `artifacts/ai-tasks/verification/windows-reliability/implementation/`

两个已完成 freeze 只更新了 `source_document`、`normative_input` 和 `worklist.refs` 的位置字段；规范/任务正文、digest、完成状态和历史 report path 保持不变。需要解释旧 evidence 中的路径时使用上面的迁移表，不修改历史记录。

## Archive 可执行性说明

`archive/` 中的 HTML 和脚本默认是历史快照。仍保留的原型截图脚本已更新为新目录；它们不属于当前产品体验原型的验收入口。当前可复现原型只使用 [`../product-experience-redesign/`](../product-experience-redesign/) 中的 HTML、脚本和截图。
