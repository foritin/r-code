# R-Code Agent Loop 韧性 PRD（Agent Loop Resilience）——实施版

> 状态：`implementation-ready`（v2.1 零中断化：决策敲定 + 完整路径/命令/符号锚点；前置 v0.1→v0.4 经四轮评审——R1 可行性核验、R2 简化评审、R3/R4 open-code-review 外评，记录见 §9）
> **实施源**：[worklist.json](./worklist.json)（机器任务 A00–A15：完整文件路径、符号锚点、验收测试名、verify 命令、依赖、loc 估算、`agent_rules` 零中断规约、`decisions` DEC-1..4 已敲定）。实施进度记录于 [progress.md](./progress.md)。本文件是其人类可读契约：缺口证据、任务总览、设计要点与门禁。两者冲突时以 worklist.json 为准并回改本文。
> 前置基线：Harness v1 全闭环——聊天链路 前端 → r-code-client → r-code-service 守护进程 → Harness 插件（native.r-code 默认）。
> 关联 PRD：[项目上下文引擎](../project-context/prd.md)——其 FR-6（压缩引擎）是并行依赖，接口契约见 §5.3。

## 0. 术语表

| 术语 | 定义 |
| --- | --- |
| run / turn | run = 一次用户输入触发的完整执行（一个插件进程）；turn = run 内一次"采样 → 工具 → 回填"。默认 `max_turns=25` |
| drive loop | 每任务一个的派发循环（`run_drive.rs`），从持久队列取输入逐个执行 run |
| 静默截断 | 流中途断开后 run 以"成功"返回半截回答且无错误标记（G1，本 PRD 的起因） |
| 完成性契约 | `finish_reason` 由 provider 层忠实产生、broker 层强制校验、loop 层强制消费（A01–A03） |
| 冻结请求重放 | 流中断后同一 `ModelStreamRequest` 原样重发（模型请求无副作用；工具调用绝不自动重放） |
| 预算接力 | turn 达上限不报错，宿主注入 `InputKind::Continuation` 输入续跑（协议已预留该变体） |
| broker | 宿主模型代理（`host.model.stream`，`services/models.rs`） |
| 幂等栅栏 | router 的操作键收据机制（`load_receipt → execute → save_receipt`） |

## 1. 背景与缺口证据

**一句话问题**：native loop 骨架（协议、操作键幂等、每轮 checkpoint、进程隔离、权限闸）扎实，但缺"生存策略"层——上下文超窗、网络中断、输出截断、轮数耗尽四类预期内状态全部表现为 run 失败或静默错误结果；弱网与长任务两个场景不可用。

### 1.1 缺口登记表（G1–G13，均带 2026-10-02 代码取证；修复任务映射见 §3.3）

| # | 缺口（严重度） | 要点与证据 |
| --- | --- | --- |
| G1 | 流中断静默截断（**致命**） | openai 解析器 Transport 错误静默终止不补 Stop（`agent-llm/src/openai.rs:1266-1270`；responses 同款 `:992`；**anthropic 已正确**，`:868-877` 即范本）；broker 无 Stop 也返回 `Ok{finish_reason: None}`；loop 从不读 finish_reason（`loop_engine.rs:204-217`） |
| G2 | finish_reason 契约缺失 | `max_tokens`/`None` 被当正常轮次；无 tool calls + 无正常 finish 即判"模型说完了"结束 run |
| G3 | 上下文溢出即失败 | 400 不在重试白名单（`openai.rs:486-488`）；压缩归 project-context FR-6，本 PRD 只管降级语义 |
| G4 | TurnLimit 硬失败 | `max_turns=25` 超限抛错（`loop_engine.rs:54,151,194`） |
| G5 | 失败即停队列 | drive loop 遇错 break（`run_drive.rs:180-186`） |
| G6 | 工具串行 | for 循环逐个执行（`loop_engine.rs:226-250`） |
| G7 | 推理内容丢弃 | broker 把 `ReasoningDelta` 投影为 None |
| G8 | **取消被误当完成（critical，R3）** | `is_cancelled` break 落入 plan_publish + 提案路径，plan 模式发布空文本合成单元（`loop_engine.rs:189-192 → 262-293`） |
| G9 | **steer 死锁对（R3）** | SDK 内联 await on_steer（`harness-sdk/src/lib.rs:697-699`）+ session state 锁跨整个 run_loop（`session.rs:56-59`）；注（R4 核实）：steer 全链路当前无生产调用方，属"接线前必修" |
| G10 | **checkpoint/结果静默失败（R3）** | 最终 checkpoint `let _ =` 吞错、`unwrap_or_default()` 空载荷落盘、steer 乱序提交、成功后 emit_event 变 Fault、Ok(Null)（`loop_engine.rs:219-223`；`session.rs:62-71,107-110`） |
| G11 | **幂等栅栏非原子（R4，生产活跃）** | 收据 check-then-act + 盲 upsert（`router.rs:1135-1138`、`store/journal.rs:509`）；execute 失败 Indeterminate 永不终结 → 键永久砖死；FR-6 并行的先决修复 |
| G12 | **重启丢消息（R4，生产活跃）** | `TaskService::new` 空 map（`tasks.rs:82-89`）、`reload` 零生产调用、`rebuild_queue` 仅作 pending 检查——崩溃时未投递输入重启后永不再派发 |
| G13 | **checkpoint 768KB 悬崖（R4）** | base64 塞 1MB RPC 帧；router checkpoint 路径无显式上限（artifacts 有 FrameTooLarge，守卫漂移 `router.rs:760`）；接力链 200 轮加速撞墙 |

### 1.2 已扎实的地基（勿重复建设）

请求级重试完善（agent-llm `send_with_retry` 11 次尝试 + Retry-After + 抖动）；正常流必发 Stop（`[DONE]` 兜底，`openai.rs:1289-1300`）；空闲超时显式标记 `stream_idle_timeout`；工具输出上限（read 100KB 分页 / bash 30k 截断）；操作键幂等 + router 去重；每轮 checkpoint + handoff 转存（`run_manager.rs:531-566`，接力零新存储）；`InputKind::Continuation` 协议预留；队列租约语义（poll/acknowledge/rebuild）；无逐 token 前端通道（重放零可见重复、推理做整段保全）；插件握手已有超时（initialize_timeout + SERVICE_TIMEOUT 120s）。

### 1.3 Codex 对标（设计参照）

Codex（`codex-rs/core`）：流级重试含传输降级与用户可见通知；turn 错误是轮级（用户可继续会话）；无 turn 数上限（token/压缩为闸）；max_tokens 有专门分支；工具并行 `ToolCallRuntime`。r-code 的每 run 进程 + checkpoint 模型崩溃恢复更强，本 PRD 不改该拓扑。

## 2. 目标与非目标

### 2.1 终态

1. 弱网不产出假成功：断流要么重放后完整，要么显式失败——不存在第三态。
2. 输出截断有续写：`max_tokens` 续写一次拼接，仍截则显式标记。
3. 长任务不被 25 轮掐断：预算接力对用户与模型透明，链总上限护栏。
4. 失败不卡队列，重启不丢消息：run 失败队列继续；崩溃时未投递输入重启后继续派发。
5. 取消 ≠ 完成；同轮工具在原子栅栏后并行执行。

### 2.2 非目标

上下文压缩引擎（归 project-context FR-6）；输入级失败重投（R2 否决，流级重放已覆盖）；执行波次租约缺陷族（观察项 9，独立工作流）；Codex harness 路线；插件进程常驻复用；实时逐 token 前端流。

## 3. 实施总览

### 3.1 组件（worklist.json `components` 的索引）

chaos-fixtures / stream-contract / sampling-recovery / tool-failure-continuity / cancel-lifecycle / sdk-serve / steer-interleave / queue-recovery / turn-relay / idempotency-fence / tool-parallelism / reasoning-observability / checkpoint-budget。

### 3.2 任务表（A00–A15；明细与文件/测试/验收以 worklist.json 为准）

| 任务 | 标题 | 服务 | 依赖 | ~loc | 里程碑 |
| --- | --- | --- | --- | --- | --- |
| A00 | chaos 故障注入 fixtures（8 故障点 + e2e 接缝） | chaos-fixtures | — | 220 | **M0** |
| A01 | agent-llm Transport 对齐 anthropic（补 api_error Stop） | stream-contract | — | 80 | M1 |
| A02 | broker 完成性判定 + 协议卫生七项 + usage 全路径 | stream-contract | A00,A01 | 200 | M1 |
| A03 | loop 消费 finish + 冻结请求重放 + denylist + 配置 | sampling-recovery | A00,A02 | 180 | M1 |
| A04 | 工具 RPC 失败→合成错误结果继续 | tool-failure-continuity | A03 | 40 | M1 |
| A05 | max_tokens 续写 + 溢出指引 + errorClass + partial | sampling-recovery | A03 | 150 | M2 |
| A06 | 取消≠完成 + 三项 fail-fast + 四处传播修复 | cancel-lifecycle | A03 | 220 | M2 |
| A07 | SDK：steer 下 loop、唤醒竞态、EOF/shutdown 排水 | sdk-serve | — | 140 | M2 |
| A08 | session 锁粒度（不跨 run_loop，按轮合并） | steer-interleave | A06,A07 | 90 | M2 |
| A09 | drive loop 失败继续 + drive-token 不变量 | queue-recovery | — | 60 | M2 |
| A10 | 启动重播种 + acknowledge 持久先行 + 投递顺序 | queue-recovery | A00 | 160 | M2 |
| A11 | 轮数预算接力（Continuation + maxTotalTurns + run.chained） | turn-relay | A05,A06,A10 | 200 | M2 |
| A12 | 幂等栅栏原子化（预留语义 + Rejected 终态） | idempotency-fence | — | 140 | M3 |
| A13 | 工具并行（有界并发 + 顺序回填 + 回退开关） | tool-parallelism | A04,A12 | 110 | M3 |
| A14 | 推理整段保全（assistant.reasoning 观察） | reasoning-observability | A02 | 70 | M3 |
| A15 | checkpoint 体量上限（DEC-1 策略 b，已激活） | checkpoint-budget | A06 | 90 | M3 |

### 3.3 缺口 → 任务覆盖映射

G1→A01/A02/A03；G2→A02/A03/A05；G3→A05；G4→A11；G5→A09；G6→A13；G7→A14；G8→A06；G9→A07/A08；G10→A06；G11→A12（A13 前置）；G12→A10；G13→A15。全部 13 个缺口有归属。

### 3.4 执行顺序与并行波次

串行主干：`A00 → A01 → A02 → A03 →（A04/A05/A06 分叉）→ A11`。可并行侧线（不同文件/crate，无编辑冲突）：A07/A09/A12 任意时点；A10 在 A00 后任意时点；A14 在 A02 后；A08 收敛 A06+A07；A13 收敛 A04+A12。

### 3.5 零中断实施规约（worklist.json `agent_rules` 的索引）

- **锚点解析**：每个文件条目带 `anchors`（符号名 + 2026-10-02 行号提示）。行号必然随先行任务漂移——**按符号名定位**，行号只作提示，漂移不是错误。
- **偏差协议**：符号找不到 → `git log -S` 查迁移 → 在任务列出的最近语义等价位置实施 → progress.md 记一行偏差。不停、不问、不重新设计。
- **范围冻结**：只做任务 `what` 写明的事；途中所见非本表缺陷记入 progress.md"范围外发现"，不修（阻断本任务验收的例外：最小修复 + 记偏差）。
- **决策已定**：DEC-1..4（见 §11）不再作为问题提出；与实现冲突时按 DEC 执行并记录。
- **进度与测试**：任务测试名以任务 id 前缀（如 `a02_incomplete_stream_returns_error_not_ok`）；每任务完成后按模板追加 progress.md 并附 `cargo test -p <pkg> --test <stem>` 结果。
- **环境命令**：构建/单测/格式化/静态检查命令与各 crate 包名映射见 worklist.json `environment.commands`（含 plugins/native 包名以 `plugins/native/Cargo.toml` 为准的提示）。

## 4. 任务契约（人类可读摘要；完整 files/tests/acceptance/depends 见 worklist.json）

**A00 chaos fixtures**：在 t15 `ScriptedProvider` 模式上建共享 fixtures（断流 TCP/EOF、无 Stop 结束、idle 停滞、429+Retry-After、5xx、400 溢出、max_tokens、双 Stop），按调用脚本化 + 调用计数；必须能装进宿主 broker 走全链路。**每个后续验收都依赖它**。

**A01 Stop 对齐**：openai/responses 两处 Transport 分支改为 anthropic 既有范本（`Stop{Other("api_error: …")}`，once-guard），共享 `is_abnormal_stop(reason)` 助手；三解析器"正常完成必发 Stop"单测守门。

**A02 broker 完成性 + 卫生**：判定表——`end_turn/tool_use/stop_sequence/max_tokens` 为完成（max_tokens 带 truncated 标记）；`None`/`stream_idle_timeout`/`api_error:` 为未完成，返回携带部分文本（截 4KB）的 `ServiceError`；未知 Other 值 **fail-open 放行 + 记日志**。同函数顺带修七项：deadline 前置到首连 await 之前、`deadline_ms≤0` 钳 1s、超时 Failed 先递增 sequence、`done:true` 后即 break、stream_id 加每流判别、usage 全退出路径入账（scope guard）、删除模块文档假 retry 声明。router 错误分支补 `assistant.partial` 观察。

**A03 重放**：loop 层 `retry_model_stream`——可重放错误原样重发同一请求，退避 1s/2s/4s（Retry-After 优先封顶 60s），默认 3 次（配置 `streamReplayAttempts`）；denylist（溢出关键词/401/403/内容策略）零重放直达 A05；失败轮不入 state 不 checkpoint；每次重放发 Progress；"无 tool calls 结束 run"追加"正常 finish"前置条件。

**A04 工具失败续跑**：`tools_call` Err 渲染为 `error: {e}` 合成结果回填并继续本批——与 `reply.error` 同款；消除整轮作废（接力链下放大）。

**A05 降级**：max_tokens 续写一次（追加"接着写"指令重采样），双截则拼接 + `[回答因长度上限被截断]` 标记正常结束；溢出失败文案指引 + `run.failed.errorClass`（transient/deterministic/cancelled）；journal 含 `assistant.partial`。

**A06 取消与生命周期**：`stop_reason: "cancelled"` 专用结局，绝不落入提案路径（宿主记 `run.cancelled`）；循环前检查取消、plan 模式前置校验 revision、objective 缺失 Fault；四处 `unwrap_or_default`/`let _ =` 改传播；成功后 emit_event 只记日志；steer 持锁跨越 save。

**A07 SDK 服务循环**：steer 通知 `tokio::spawn` 派发 + 解析失败显式化；`wait_for_cancel` 先 pin `notified()` 再查标志；EOF 置取消 + drain 全部 pending host_call；`host_call` 等待 select 取消；shutdown 排水而非 abort；`harness.cancel` 参数校验对齐；清理死 `streams` 注册表并修正 model_stream 文档。L2/L4 修复在**普通 abort 路径即活跃**，steer 项为接线前契约修复。

**A08 锁粒度**：session 不持 state 锁跨 run_loop——克隆出、按轮合并回；steer 落在轮间（"steerable between turns" 兑现）。

**A09 队列不滞留**：Err 臂 break→continue（复用 pause 判定）；全部退出路径锁内清 drive token（抽 `clear_run_handles` 助手去重）。

**A10 队列持久性**：daemon 装配处重播种（store rebuild_queue → kernel 内存队列，跳过已有成功 run 的输入）；acknowledge 持久成功后再改内存；pending 按 `input_seq` 排序（rebuild 与 push 两处）。

**A11 接力**：达 `max_turns` 返回 `budget_reached` + 摘要，提案强制 Reply；run_manager 注入 `Continuation` 固定模板输入（drive loop 自然继续，handoff checkpoint 恢复，模型无感）；`maxTotalTurns=200` 链护栏（journal 计数）+ `run.chained` 事件；cancel 随时断链；TUI 标记为系统续跑。

**A12 栅栏原子化**：收据改 insert-only 预留（冲突= replay/conflict；store 条件 upsert 返回既有行）或 per-key 进程内互斥；execute 失败写 `Rejected` 终态（键可重试）；`load_receipt` 错误不得映射 None。**A13 的先决条件**。

**A13 并行**：有界 `join_all`（信号量，默认 4，配置 `parallelTools`）；结果按 call 顺序回填；abort 全量传播；回退开关恢复串行。

**A14 推理保全**：broker 聚合 ReasoningDelta，轮完成时 router 发一次 `assistant.reasoning`（时序在 `assistant.message` 前）；整段而非增量（无前端通道）。

**A15 体量上限**（DEC-1 已激活）：router 显式 `MAX_CHECKPOINT_STATE_BYTES`（与 1MB 帧上限耦合，注释写明）；超限时**宿主侧**单调截断最老 tool result（占位保留调用摘要）重存一次并记 journal；仍超则显式失败。

## 5. 技术设计要点

### 5.1 修复层次（宿主管事实，插件管策略）

```
agent-llm（vendor，2 处）    A01：Transport 对齐范本
broker / router（宿主）       A02 完成性与卫生；A05/A14 观察；A15 上限
native loop_engine/session（插件） A03/A04/A05/A06/A08/A11/A13
harness-sdk                   A07
run_manager / run_drive / application（宿主） A09/A10/A11 注入与护栏
kernel tasks / store          A10 写序与顺序；A12 收据条件写
协议                          A06 新增 cancelled 结局（向后兼容）；Continuation 复用；Stop 语义不变
```

### 5.2 A02 完成性判定表（broker 侧）

| finish_reason | 判定 |
| --- | --- |
| `end_turn` / `tool_use` / `stop_sequence` | 完成 ✓ |
| `max_tokens` | 完成 ✓（truncated 标记 → A05 续写） |
| `stream_idle_timeout` / `api_error:` 前缀 / `None`（流结束无 Stop） | 未完成 ✗ → `ServiceError`（内嵌 ≤4KB 部分文本） |
| 其他未知值 | **fail-open** 放行 + 记日志（DEC-3） |

### 5.3 与 project-context FR-6（压缩引擎）的接口契约

FR-6 在 `host.model.stream` 边界投影（L1 清理/L2 摘要），A02 完成性校验位于投影之后的流消费侧——**投影不得改写/吞掉 Stop 事件**；A05 溢出降级依赖其 `ContextCompacted` 事件判定"已压缩仍溢出 → 确定性失败"；两 PRD 并行实施，都落地前长任务上限 = `maxTotalTurns` 与上下文窗口的先到者。A15 的宿主侧截断与 FR-6 L1 清理同向（单调、只追加），不得互相破坏前缀缓存。

### 5.4 配置（3 项）

| 配置项 | 默认 | 范围 | 载体任务 |
| --- | --- | --- | --- |
| `streamReplayAttempts` | 3 | 0–10 | A03 |
| `maxTotalTurns` | 200 | 25–1000 | A11 |
| `parallelTools` | 开（并发 4） | 关 / 1–16 | A13 |

（`max_turns` 每 run 段维持现有 harness_config 覆盖；max_tokens 续写始终开启无配置。）

## 6. 安全与边界

仅模型采样请求可重放（无副作用）；工具绝不自动重放（A12 后栅栏自身安全）；审批等待（600s）不进入重试范围；取消优先于一切恢复（接力注入/重放/重播种每步先查取消）；Continuation 为固定模板非用户可控、TUI 系统续跑标记；`assistant.partial` 带 partial 标记防与正常回答混淆；重放（≤3）+ 请求级（≤11）有界叠加；并行工具 ≤16 且写路径 path lease 串行化。

## 7. 里程碑门禁

| 门禁 | 任务 | 出口判据（全部机器验收，依赖 A00 fixtures） |
| --- | --- | --- |
| **M0** | A00 | 8 故障点全部可注入且基线行为被钉住 |
| **M1 静默截断清零** | A01–A04 | 断流两态（恢复/显式失败），无第三态；三解析器 Stop 必发；重放/耗尽/denylist/Retry-After 全绿；工具失败不中止 run |
| **M2 失败可恢复** | A05–A11 | max_tokens 拼接与标记；溢出指引 + errorClass + partial；接力 30 轮任务 + 护栏 + cancel 断链 + Reply 提案；队列不滞留 + token 不变量；重启重播种 + acknowledge 可重试 + 顺序一致；取消 ≠ 完成；steer 轮间吸收 + EOF/取消快速失败 |
| **M3 吞吐与观测** | A12–A15 | 栅栏并发单放行 + 失败键可重试；并行墙钟 + 回填序 + abort 清理 + 回退开关；assistant.reasoning 时序；checkpoint 截断单调且有 journal |

发布纪律：每任务独立可合并、可回退（A03/A11/A13 有配置开关，A01/A02/A07 为正确性修复无开关但有回归基线钉住行为）。

## 8. 风险与缓解

| 风险 | 等级 | 缓解 |
| --- | --- | --- |
| 整轮重放浪费 token | 中 | 接受（SSE 无标准续传）；有界 + 可见；远期接 Responses resume |
| finish 判定误伤方言 | 中 | A01 三解析器单测守门 + 未知值 fail-open（宁放假失败不杀正常流） |
| A02 七项卫生改动面大 | 中 | 同函数内原子交付；逐项命名单测；A00 基线先行 |
| A11 接力与压缩引擎时序纠缠 | 中 | §5.3 契约；集成测试互留对方未就位降级路径 |
| A09 删 break 引入 send 竞态 | 高 | 沿用 slot mutex 串行化；A09.2 专门交错测试 |
| A12 store 条件写迁移风险 | 中 | 保留盲 upsert 读路径兼容；冲突回读语义有专项测试 |
| A15 截断破坏前缀缓存 | 中 | 单调截断 + 只动 tool result 文本 + 与 FR-6 L1 同向；缓存命中纳入验收观察 |

## 9. 评审历史（详细记录见 git 历史 v0.1–v0.4）

- **R1 可行性核验**：推翻 v0.1 三假设——anthropic 已是范本（修复从"三方言新事件"降为"两处对齐"）；`[DONE]` 兜底使 None 判定可靠；`Continuation` 已预留（接力免改协议/免改 drive loop）；发现无前端逐 token 通道（重放零重复、A14 改整段）。
- **R2 简化评审**：删输入重投子系统与 SDK 反序列化防线；配置 7→3；每 FR 收敛修改面。
- **R3 ocr 外评一**（6 文件 51 条）：G8/G9/G10 入册；FR-1 并入协议卫生层；两条原文夸大降级（悬空调用不毒化持久层；队列滞留为窄竞态）；8 条入观察项。
- **R4 ocr 外评二**（router/parallel/kernel 27 条）：G11/G12/G13 入册（调用面核验定级：G11/G12 生产活跃，kernel 多数未接线）；纠正 v0.2 一句错误声明（重启队列重建）；parallel.rs 租约族 8 条登记为观察项 9（建议独立立项，含安全相关项）；A00 缺口识别（无故障注入则 M1 验收不可机械化）。
- **覆盖缺口（如实登记）**：`run_manager.rs`、`agent-llm/src/openai.rs` 两次 ocr 扫描均因超 token 预算跳过，仅有人工关键段阅读；完整工具化评审待分片支持。

## 10. 观察项登记（属实、不立项、防丢失）

1. 视觉内容三处降级（image 占位/空串、`is_error` 恒 false）——多模态保真域。
2. 长时 daemon 资源增长（`usage_log` 无界、`slots` 不清、SDK 出站通道无界）。
3. checkpoint 信任边界仅校验 role；wire 格式无版本字段、重命名静默破坏旧 checkpoint。
4. `replay_inputs` 仅执行 first()（宿主恰好只传 1；协议文档应补注；A11 接力恰好只用 1）。
5. `send` 返回的 runId 是 `count_runs+1` 猜测（建议改返回 messageId）。
6. `prepare_for_new_input` 先持久 reopen 后 enqueue 的部分失败窗口。
7. plan 解析器不接受 ```json 围栏，静默降级合成只读单元。
8. `next_input`/`pause_dispatch` 在 slot 锁内做阻塞 SQLite 全 journal 扫描（建议廉价 pending-count）。
9. **parallel.rs 执行波次租约缺陷族（8 条，建议独立工作流）**：非 Started 退出泄漏写租约；**取消先放租约后杀进程（活 harness 与可再获取写作用域重叠——安全相关）**；supervisor 注册失败孤儿进程；CAS 重试 `mem::take` 吞修复事件；fatal 分支丢弃观察与 release；毒锁 `.expect` 可带走 manager；`held_tools.remove` 先于落盘；revalidate 把瞬时 IO 错当语义漂移永久 FAILED。
10. router 守卫漂移三件：instructions 投影缺任务身份校验（安全）；JIT 注入无 System 消息时账实分离；`publish_plan_revision` 错误变体折叠。
11. kernel 契约级竞态（生产未接线，接线前必修）：enqueue 收据 check-then-act（现无调用方传操作键）、收据先于 `input.queued`、seq 分配与持久乱序、`create_task/branch` 不查重、`reload` 不重建 pins/parents/in_flight 且 O(N×journal)、kernel steer 内存先于持久。
12. 性能与测量：router/parallel 同步 SQLite 与全树 fs 扫描跑在 async worker；每 run 进程 spawn 冷启动从未测量（先加计时再谈常驻复用）。

## 11. 决策记录（DEC；原 OQ 已全部敲定，2026-10-02 按推荐默认值锁定，可经翻转点单点推翻、无需重新评审）

| DEC | 问题 | 敲定结果 | 翻转点（改动位置） |
| --- | --- | --- | --- |
| DEC-1 | G13 checkpoint 悬崖策略 | **b+a**：A15 激活（router 显式上限 + 宿主侧单调截断）；压缩引擎落地后长期接管 | A15 截断策略全部在 router.rs checkpoint 分支内；转 (a) = 删截断保留显式失败；转 (c) = 该分支改分块写 |
| DEC-2 | parallel.rs 租约缺陷族（观察项 9，8 条含安全项） | **独立工作流立项**，本 PRD 范围外；若日后并入则按观察项 9 清单追加 A16+ | 无代码翻转点；纯排期决策 |
| DEC-3 | 未知 finish_reason 值 | **fail-open**（按完成处理 + tracing 告警）；已知异常集封闭（None / stream_idle_timeout / api_error: 前缀） | models.rs 完成性分类器的单个 match 臂；配套测试 `a02_unknown_reason_fail_open` 钉住 |
| DEC-4 | 接力形态与配比 | 普通用户侧输入（协议零改动，TUI 已标系统续跑）；`maxTotalTurns=200` / 段 25 | 形态：A11 注入点 + request_projection.rs 角色投射；配比：settings 默认值两处常量 |

> 敲定依据：2026-10-02 向使用者提出四项决策问题未获回复，按工具规约以推荐默认值继续并全量记录翻转点；任一决策被推翻时只需改翻转点单点 + progress.md 决策翻转记录，不影响其余任务。
