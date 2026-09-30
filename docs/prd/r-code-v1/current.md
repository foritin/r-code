# R-Code v1 当前状态：O-GATE 执行中（O00/E05/E06 KEEP，E07 待起步）

> 状态：`o-gate executing 3/8，paused at E07`。**O00、E05、E06 已 KEEP**；下一任务 **E07**（LeaseFamily——fencing 复用现有 workspace ownership epoch，绝不第三 epoch 通货）。**E07 的 before 快照已拍（`sandbox/r-code-v1-safety/iter/E07/before/{store,runtime}/` 四文件含 sha256），Engineer 尚未开始——恢复者直接从 Engineer 阶段起步，勿重复拍快照。**
> 权威合同：[o-gate.tasks.json](./worklists/o-gate.tasks.json)；执行台账：[o-gate.ledger.tsv](./progress/o-gate.ledger.tsv)。
> 顺序：O00 ✅ → E05 ✅ → E06 ✅ → **E07** → E08 → E09-R → E09-C → E10（E07/E08 依赖 E06；E09-R 依赖 O00+E05；E10 等齐 E07+E08）。

## 1. 当前基线（2026-09-29，E06 之后，原始日志重数）

- **House 五组累计 = 1141/1140/1**（151 行 `test result:`）：唯一声明红 = plan_schema 18-vs-35（**基线对未移动**——E06 的 work_unit_attempts 迁移走 string-keyed ledger，编号注册表未动；E07/E09-R 若同走此路则同样无需重声明，走编号注册表则必须重声明新对）。
- E06 成果：wave 调度器（parallel.rs，#[path] 挂在 run_manager 内，lib.rs 未动）：ready set=Pending+可写+依赖 Completed+P26/P27 漂移复验（依赖写域 delta 对照其 journal 变更，未解释条目=外部编辑→依赖者 re-block）；bound 2；per-unit RunSnapshot 冻结→**durable attempt 行先于任何 spawn**（content=snapshot id）；frontier-hold 种子（真启动前 InFlight 记录保聚合开放）；settle durable-first；中途 settle 记 unit.completed、聚合才 review-ready。run_manager 2253→1644。attempt_id=`attempt-{task}-{rev12}-{unit}` 幂等收敛/分歧 ContentConflict/settle 恰好一次。QA 种子泄漏探针抓到真缺陷（失败 settle 泄漏 InFlight 种子→任务楔死 Running 无 run.failed 无 ack）已 refine 修复（retraction 谓词带 journal 判据 + 幂等 require_repair 重推导聚合）。e06 overlap 断言一次负载 flake 已加固（200ms→1500ms）并两边都报。
- 全部工作未提交（树上大量未提交改动共存）。

## 2. 下一手（E07 执行者按序做）

0. **暂停时点**：E07 before 快照已拍（store/{schema.rs,mutations.rs} + runtime/{mutations.rs,parallel.rs}），Engineer 未动工。恢复者从第 2 步 Engineer 直接开始。
1. 读 [o-gate.tasks.json](./worklists/o-gate.tasks.json) 任务 E07（E07.1-E07.3：lease_families 表 keyed by attempt_id；全有或全无获取；fencing 复用现有 workspace ownership epoch；family release 恰好结算其 member 与 attempt 记录；重启对账整 family）。
2. 六步：~~拍 before~~（已完成）→ Engineer → QA `crates/r-code-runtime/tests/e07_lease_family.rs` → 双跑 house → 仪式 → 进 E08。
3. E07 语义注意：MutationExecutor 的租约加入所属 attempt 的 family（E06 已有 attempt 行可挂）；stale attempt 不能 release/extend；重启时 family 的 attempt 已 settle→release、未完成→quarantine 并 fence members；**不同任务的 family 互不 fence（pinned）**。
4. 计数纪律不变。

## 3. 基线复现

```powershell
rtk proxy cargo test -p r-code-runtime -p r-code-harness-protocol -p r-code-harness-sdk -p r-code-kernel -p r-code-store -p r-code-harness-codex --all-features --no-fail-fast   # 151 行 / 1140 / 1
node scripts/verify-safety-v1.mjs        # 16/19 activated, exit 0
node scripts/check-process-launch-boundary.mjs   # 17 站点绿
```

## 4. 恢复入口

- O-GATE 执行：`progress/o-gate.ledger.tsv` iter 1-3；evidence `o-gate-{O00,E05,E06}.verdict.json`；`sandbox/r-code-v1-safety/iter/{O00,E05,E06}/`。
- 拆解坑清单在 `sandbox/r-code-v1-safety/o-gate-plan.md`（E08 出生点缝=session.rs/processes.rs、E09 表为真值 journal 派生、E10 从 work_unit_attempts 枚举）。
- Safety：ledger iter 55-68；P23 遗留披露兑现为后续组合轮的活。



