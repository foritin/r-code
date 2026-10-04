# R-Code 执行波次租约生命周期 PRD（Execution-Wave Leases）——实施版

> 状态：`implementation-ready`（v1.0，2026-10-04；来源：agent-loop 韧性 PRD 观察项 9 的 8 条 ocr scan 发现，DEC-2 拍板独立工作流，安全项优先级不低于 M2——M2 已完成，本工作流随即启动）
> **实施源**：[worklist.json](./worklist.json)（L01–L08：文件、锚点、验收、依赖、loc）。本文为人类契约。
> 前置取证：全部 8 条发现带行号记录于 [agent-loop 观察项 9](../agent-loop-resilience/prd.md)；ocr 原始报告存档于 2026-10-02 会话。

## 0. 一句话问题

执行波次（plan 批准后的 WorkUnit 并发派发）的租约生命周期绑定在控制流上而非持久性上：**取消先放租约后杀进程**（活 harness 与可被他人获取的写作用域重叠——安全项）、**~15 个非 Started 退出泄漏租约/pin**、**CAS 调和丢事件后假装成功**（楔死任务）、**fatal 分支丢弃证据与资源**、**瞬时 IO 错被当语义漂移永久失败**。

## 1. 缺口与修复映射

| # | 缺口（严重度） | 锚点 | 任务 |
| --- | --- | --- | --- |
| 1 | 非 Started 退出泄漏写租约 + pin（high） | `parallel.rs:997` 后 ~15 处 early return；`ExecutionToolService::Drop` 刻意不释放 | L02（DEC-5：清理助手） |
| 2 | **取消先放租约后杀进程（安全）** | `settle_wave_cancelled :1727` | L01（先杀+证明，后放租；drain runs） |
| 3 | supervisor 注册失败孤儿进程（high） | `:1284` | L03（kill_confirmed + 释放 + unpin） |
| 4 | CAS 重试吞事件、耗尽返回 Ok（high） | `:215-231` | L04（动作化重放 + 耗尽 Err） |
| 5 | fatal 分支丢弃 join 结果（medium） | `:718-726` | L05（与 L01 共享 drain 助手） |
| 6 | 毒锁 expect 带走 manager（medium） | `:1396` | L06 |
| 7 | held_tools.remove 先于落盘（medium） | `:826` | L07 |
| 8 | 瞬时 IO 当语义漂移永久 FAILED（low） | `process_effects.rs:266`、`parallel.rs:399` | L08（ScanError 分类 + Deferred） |

## 2. 决策

| DEC | 问题 | 敲定 | 翻转点 |
| --- | --- | --- | --- |
| DEC-5 | 15 处泄漏退出的修复形态 | 清理助手（`defer_with_cleanup`/`fatal_with_cleanup`：release+unpin 后返回 outcome），不重构 250 行 dispatch 体 | 助手调用点；guard 式重构可后行且无行为差异 |

## 3. 里程碑

- **M-A 安全与无泄漏**：L06→L07→L02→L03→L01→L05（先机械项热身，安全项随后，共享 drain）。
- **M-B 正确性分类**：L08→L04。

回归底盘：e06_parallel_dispatch / e07_lease_family / e08_child_supervisor / e10_chain_recovery 全程保持绿（每任务一个边界）。

## 4. 验收与顺延的诚实边界

- e2e 可证：L01（取消序）、L02（deferred 后无 active 租约行）、L04（stale 重试保事件）、L08（EACCES deferred）。
- 检视级+既有回归：L03（无强制注册失败注入口）、L05（无 store 故障注入口）、L06（grep 验收）、L07（纯顺序）。理由与 agent-loop 工作流的顺延记录方式一致。
