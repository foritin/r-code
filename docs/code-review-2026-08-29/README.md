# Code Review 2026-08-29 — 索引与恢复说明

全仓深度 review + 根因级修复管线。**执行分支 `feat/code-review-2026-08-29`**，全程只在此分支提交。

## 目录

| 文件 | 用途 |
| --- | --- |
| `review-plan-2026-08-29.yaml` | 冻结的 review 任务清单（RV-01..RV-09） |
| `current-task.yaml` | **唯一恢复锚点**，每完成一个可验证子步更新 |
| `findings/` | 按维度分文件的 findings（`F-<维度>-<序号>` 编号） |
| `fix-plan-2026-08-29.yaml` | finding -> 修复任务映射（阶段 A->B 产出） |
| `evidence/` | 验收命令输出、测试日志、同类问题清零证明 |
| `final-report-2026-08-29.md` | 最终交付报告 |

## 中断后恢复协议

1. 读本文件 -> 读 `current-task.yaml`。
2. 对 `status: done` 的任务跑最小 smoke 验证（任务卡 `acceptance` 字段的第一条命令）。
3. 从 `current-task.yaml` 记录的断点继续，**不重做已完成任务**。

## 硬性约束（对所有阶段生效）

- **用户资产保护**：仓库中存在本次任务开始前的未提交改动（见 `evidence/phase0-git-status.txt`），一律视为用户资产，不 reset、不覆盖、不回滚、不删除。提交时只 `git add` 本次任务产出/修改的文件；若修复必须触碰用户 WIP 文件，先在任务卡记录，提交信息注明包含既有 WIP。
- 阶段 A 只读不改代码（临时验证脚本除外，跑完删除）。
- 每次提交前确认当前分支是 `feat/code-review-2026-08-29`。
- 修复消除根因类别，禁止 `#[allow]`/空 catch/压告警式"通过"。

## 任务编号规则

- review 任务 `RV-NN`，修复任务 `FX-NN`。
- finding 编号 `F-<dim>-NN`，dim ∈ {base, arch, corr, sec, robust, perf, maint, test, obs}。
