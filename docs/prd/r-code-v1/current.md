# R-Code v1 当前状态：O-GATE 全 KEEP（8/8），F/G/H/I 待需求恢复（人工 open question）

> 状态：`o-gate complete 8/8；F decomposition BLOCKED on requirement recovery`。**O00、E05、E06、E07、E08、E09-R、E09-C、E10 全 KEEP**（2026-09-30）。O-GATE-CONTINUE.txt 已按指示删除。
> 权威合同：[o-gate.tasks.json](./worklists/o-gate.tasks.json)（已完成）；执行台账：[o-gate.ledger.tsv](./progress/o-gate.ledger.tsv) iter 1-8。

## 0. 阻塞（需人决策）——F/G/H/I 的需求定义缺失

§9.2-9.5 的 F01–F04 / G01–G06 / H01–H05 / I01–I07 **只有一行主题名**（index.md:244-247），整个仓库（docs、history/、.gstack、artifacts）与全部 git 历史均无任务级定义；四个 F 主题（H0/H1、authority marker、writer freeze、forward-only recovery）在代码与文档中零落点。plan-loop 明令禁止臆造需求（"不得 invent packages/files/APIs；真正的未知记 open_question"）。**恢复路径（三选一，需人拍板）**：① 提供旧 master 的 F/G/H/I worklist 原文；② 指认需求出处（另一仓库/文档）；③ 授权按四主题名做需求再发明（走一轮 PRD 补写而非直接拆任务）。在此之前 F 不开工——这是计划性阻塞，非实现性阻塞。

## 1. 当前基线（2026-09-30，E10 之后，本机双跑）

- **House 六组累计 = 155 行 `test result:` / 1158 / 1157 / 1**：唯一声明红 = plan_schema 18-vs-35（**整个 O-GATE 三个 store 迁移全部走 string-keyed ledger，编号注册表未动，红对自始至终未移动**）。
- O-GATE 全部成果：kernel 多单元状态机（E05）+ wave 调度器与 work_unit_attempts（E06）+ LeaseFamily 全有或全无/同事务结算/整 family reconcile（E07）+ ChildSupervisor 单 owner 注册表与逐树死证 sweep（E08）+ unverified_overrides 不可变审计表与 review.overrides.list 四端投影（E09-R/C）+ 启动单次全链恢复 resume-once/quarantine（E10，readiness 发布 reconciled 计数）。
- **已记录待办（后续组合轮认领）**：main.rs invoke_handler 一行注册 `cmd_harness_v1_overrides_list`（E09-C 裁量缺口）；`with_child_supervisor` 装配进 application.rs 的 ManagedProcessService 构造（E08 裁量缺口）。
- 本机环境注记（非基线机器）：无 rtk/python——cargo 直连（git 依赖 SSH 拉取）、file_loc 以非空物理行等价复算；484 .rs 已规范化 LF；t14a 死 pid 断言按 5s settle 上限加固；git status 对这批文件报 stat 缓存幻影 M——以 `git diff` 为准；外部用户侧 `docs/readme.md` 一行 + `docs/research/` 勿动。
- 全部工作未提交（树上未提交改动共存）。

## 2. 下一手（阻塞解除后）

1. 需求恢复三选一（见 §0）拍板后，跑 `plan-loop` 产出 `docs/prd/r-code-v1/worklists/f-migration.tasks.json`（validate_plan.py 过门；本机无 python——换台带 python 的机器跑校验，或按 schema 手工对拍并记录）。
2. 拆解定稿后按 swe-loop 六步逐任务至全 KEEP；计数纪律不变（155/1158/1157/1 起步）。
3. 之后依次 G（一致性）→ H（四客户端）→ I（发布），每项同样先 plan-loop、同样先解决需求出处。
4. 顺手活（后续任一组合轮认领）：main.rs invoke_handler 一行注册 `cmd_harness_v1_overrides_list`（E09-C 裁量缺口）；`with_child_supervisor` 装配进 application.rs 的 ManagedProcessService 构造（E08 裁量缺口）。

## 3. 基线复现

```powershell
cargo test -p r-code-runtime -p r-code-harness-protocol -p r-code-harness-sdk -p r-code-kernel -p r-code-store -p r-code-harness-codex --all-features --no-fail-fast   # 155 行 / 1158 / 1157 / 1（本机无 rtk，直连 cargo）
node scripts/verify-safety-v1.mjs        # 16/19 activated, exit 0
node scripts/check-process-launch-boundary.mjs   # 17 站点绿
```

## 4. 恢复入口

- O-GATE 执行：`progress/o-gate.ledger.tsv` iter 1-8；evidence `sandbox/r-code-v1-safety/iter/{O00,E05,E06,E07,E08,E09R,E09C,E10}/`（本机重建，E07 起含 change.json/verdict.json/evidence house 日志）。
- E10 关键裁量（ledger iter 8）：链恢复只在启动跑一次（compose，effect 恢复后、写入口前）；每输入/每波重跑会把**活** attempt 误隔离（m03 竞争回归即证明）。
- Safety：ledger iter 55-68；P23 遗留披露兑现为后续组合轮的活。
