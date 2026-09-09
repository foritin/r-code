# TUI v2 历史方案与原型

2026-09-09 从 `docs/tui-v2/` 整目录归档。原有未提交内容随文件保留；归档仅调整文档入口，不重新判定历史任务状态。

- [R-Code CLI PRD](./r-code-cli-prd.md)
- [冻结元数据](./tui-v2-freeze.yaml)
- [调研与选型](./pi-tui-deep-research.md)
- [差距清单](./pi-parity-gap-list.md)
- [命名决策](./cli-naming-decision.md)
- [交互原型](./tui-v4-prototype.html)
- [历史基准报告](./m5-01-poc-report.md)

当前实施方案是 [可插拔 Harness 重构计划](../../../prd/pluggable-harness/plan.md)。本目录保留的 PRD 规范／任务正文、摘要、完成状态与历史 evidence 引用不因移动而改写；freeze 只调整文件位置元数据。历史文本中的 `docs/tui-v2/` 对应本目录。

`scripts/verify-tui-v2.mjs` 继续读取这里的冻结规范和历史决策。重新运行 `inline_bench` 产生的报告位于 `artifacts/metrics/tui-v2/m5-01-poc-report.md`，不会覆盖本目录中的历史报告。
