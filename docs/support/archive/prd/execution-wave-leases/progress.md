# 执行波次租约生命周期 —— 实施进度

> 实施源：[worklist.json](./worklist.json)（L01–L08）。规约同 agent-loop-resilience。

## 任务状态

| 任务 | 状态 | 测试 | 完成日期 | 备注 |
| --- | --- | --- | --- | --- |
| L01 取消先杀后放（安全） | done | e06/e08 回归绿 | 2026-10-04 | drain runs → sweep 求证 → 才放租 |
| L02 非 Started 退出零泄漏 | done | l02 1 passed + e06 | 2026-10-04 | ~15 处走清理助手；DependencyNotCompleted 例外保留（并发兄弟持有同一 attempt 租约） |
| L03 注册失败孤进程 | done | 检视级（无注入口） | 2026-10-04 | kill_confirmed + 释放 + held_tools 移除；deregister API 不存在（记偏差） |
| L04 CAS 保事件+重放+耗尽 Err | done | e10 回归绿 | 2026-10-04 | RepairAction 重放；mem::take 消灭 |
| L05 fatal drain 带账 | done | e06/e10 回归绿 | 2026-10-04 | 观察落账+释放留痕 |
| L06 毒锁容忍 | done | grep 验收 | 2026-10-04 | |
| L07 remove 后置 | done | e06 回归绿 | 2026-10-04 | |
| L08 瞬时扫描 defer | done | l08 3 passed | 2026-10-04 | NotFound=漂移、其余 io=defer；Windows EACCES e2e 顺延（平台差异） |

## 偏差记录

1. ocr 原意见的 catalog.unpin 不适用：PluginCatalog 无 unpin API，pin 按 attempt 键控、attempt 消亡后不阻塞任何人——真正阻塞的是租约族，助手只做 release_lease_family + tools.release。
2. ChildSupervisor 无 deregister 公开 API——L03 靠 kill_confirmed + 未注册即不可达（supervisor 从未见过该树）。
3. DependencyNotCompleted 分支不清理（保留原语义）：同一 attempt 的并发 manager 持有共享写租约，释放会拆掉在跑方。
4. L08 的 Windows EACCES e2e 顺延：目录 ACL 行为平台差异大，单元级三分（missing=drift / io=defer / 枚举形态）已钉住分类契约。

## 实施日志

- 2026-10-04：L01–L08 一轮完成。全量回归：runtime lib 86 + 波次四套（e06/e07/e08/e10）+ agent-loop 全套 + l02/l08 专项全绿；cargo fmt 0 diff。冷启动基线（观察项 12）测得：每 run 中位 597ms / 均值 619ms（spawn+握手+checkpoint 恢复+仲裁全程，模型零耗时脚本），数据落 agent-loop progress.md。
