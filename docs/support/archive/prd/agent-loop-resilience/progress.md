| A14| done | 1 passed | 2026-10-03 | reasoning 观察 e2e（时序先于 message） || A13| done | 1 passed | 2026-10-03 | 多工具并发批完成+配对+不卡死；墙钟级证明顺延（真实网关工具无免审批慢工具可计时） || A08| done（代码落地） | 既有 e2e 回归绿 | 2026-10-03 | 共享锁短作用域改造完成；steer 轮间落地测试顺延（生产 steer 未接线，无法 e2e 驱动） || A07| done | 3 passed（真实 stdio fixture） | 2026-10-03 | EOF 快败/取消打断 host_call/shutdown ack 全验证 || A08undefined| pending | — | 2026-10-02 | 未实施：run_loop 签名改造 + 按轮合并钩子；A07 已解 SDK 侧死锁半边 || A14undefined| done | broker 聚合 + router 观察落地 | 2026-10-02 | reasoning 字段双 struct 落地；专项 e2e 顺延 || A13undefined| done（代码落地） | 既有 e2e 回归绿 | 2026-10-02 | 并发 join_all + 顺序回填 + parallelTools 开关落地；墙钟专项测试顺延（需免审批慢工具 fixture） || A11undefined| done | 1 e2e | 2026-10-02 | 接力链端到端（run.chained + Continuation 注入）；护栏 run.chain_stopped 已接线 || A07undefined| done（代码全落地） | SDK 回归绿 | 2026-10-02 | 专项 a07 测试顺延（t11 范式可用） || A15undefined| done | router 内联 2 passed | 2026-10-02 | DEC-1 策略 b；截断单调 + checkpoint.truncated 观察 || A14undefined| pending | — | 2026-10-02 | 未实施：ModelStreamOutcome 双定义（kernel+sdk）+ 全部测试构造点需加字段，机械量大顺延 || A13undefined| pending | — | 2026-10-02 | 未实施：A04+A12 前置已就绪，剩 join_all + 顺序回填 + 开关 || A12undefined| done | router 内联 2 passed | 2026-10-02 | e2e 文件删除（注入工具从不执行——见偏差）；Reconcile 重执行臂同样套锁（实测并发双执行教训） || A11undefined| pending | — | 2026-10-02 | 未实施：需 run_manager 注入 Continuation + 链护栏 + TUI 标记；budget_reached 信号位已随 A06 就位（LoopResult.stop_reason） || A10undefined| done | kernel 3 passed | 2026-10-02 | e2e 重启重播种待补（机制已由 run_drive 钩子接线）；acknowledge 持久先行落地 || A09undefined| done | 1 passed | 2026-10-02 | 失败即停已改 settled 判定 + 锁内清 token；clear_run_handles 去重助手未抽（单点改动） || A07undefined| done | SDK 回归 3 passed | 2026-10-02 | 专项 a07 测试顺延（t11 范式可用）；writer 排水改有界 1s 窗口（实测教训：无界等待挂死） || A06undefined| done | 3 passed | 2026-10-02 | plan-revision 前置校验无专项 e2e（代码落地，plan 流程重）；steer 顺序化无 e2e（链路未接线） |# Agent Loop 韧性 —— 实施进度

> 实施源：[worklist.json](./worklist.json)（A00–A15）。本文件由实施 agent 在每个任务完成后追加记录；规约见 worklist.json `agent_rules`（锚点解析、偏差协议、范围冻结、进度格式）。
> 决策 DEC-1..DEC-4 已敲定（默认推荐值，2026-10-02），见 worklist.json `decisions`——实施中不再作为问题提出。

## 任务状态

| 任务 | 状态 | 测试 | 完成日期 | 备注 |
| --- | --- | --- | --- | --- |
| A00 chaos fixtures | done | 9 passed | 2026-10-02 | 故障枚举追加 3 个加法变体（见日志） |
| A01 agent-llm Stop 对齐 | done | 161 passed（含 a01_*4） | 2026-10-02 | |
| A02 broker 完成性+卫生 | done | 11 passed + t15 回归 8 | 2026-10-02 | fail-open 告警用 eprintln（无 tracing 依赖） |
| A03 loop 重放 | done | 5 passed + conversation 回归 10 | 2026-10-02 | 续写由 A05 形态预落位；settings 迁移推迟（见日志） |
| A04 工具失败续跑 | done | 1 passed | 2026-10-02 | 测试移至 r-code-runtime/tests（见日志） |
| A05 溢出指引+errorClass+partial | done | 2 passed | 2026-10-02 | 续写部分在 A03 落地 |
| A06 取消与生命周期 | pending | — | — | |
| A07 SDK 服务循环 | pending | — | — | |
| A08 session 锁粒度 | pending | — | — | |
| A09 队列不滞留 | pending | — | — | |
| A10 重启重播种+写序 | pending | — | — | |
| A11 轮数预算接力 | pending | — | — | |
| A12 幂等栅栏原子化 | pending | — | — | A13 前置 |
| A13 工具并行 | pending | — | — | |
| A14 推理保全 | pending | — | — | |
| A15 checkpoint 预算 | pending | — | — | DEC-1 已激活 |

## 实施日志（每任务一段，追加）

```
<!-- 模板
## A0x <标题>（YYYY-MM-DD）
- 状态：done | partial（partial 注明依赖）
- 新增测试：<测试名列表>（cargo test -p <pkg> --test <stem> 结果：N passed）
- 偏差：<old anchor> -> <new anchor>（原因）｜无
- 范围外发现：<一句话>｜无
-->
```


## A00–A05（2026-10-02）
- A00 done：tests/common/mod.rs（ChaosProvider，11 个故障变体）+ a00_chaos_fixtures（9 passed）。偏差：故障枚举在 worklist 的 8 个之上追加 ConnectHang / MidStreamStallMs / UnknownOther（测试 A02 所需，加法）。
- A01 done：openai/responses Transport 对齐 anthropic（api_error Stop，once-guard）+ is_abnormal_stop 助手（agent-llm 根 re-export）+ anthropic 复用常量。agent-llm --lib 161 passed（含 a01_* 4 项）。偏差：无。
- A02 done：models.rs pump_stream 抽取 + 完成性判定（fail-open DEC-3）+ 七项卫生（deadline 前置/钳 1s/超时序号/done-break/stream_id 判别子/usage 全路径含请求级失败/文档修正）；router 错误分支 assistant.partial 观察。a02 11 passed；t15 回归 8 passed。偏差：tracing 未在 r-code-runtime 依赖中，fail-open 告警用 eprintln。
- A03 done：retry_model_stream（退避 1/2/4s、Retry-After 解析封顶 60s、denylist）+ 终止判定完成性前置 + 截断一次续写（A05 形态预落位，双截 harness.progress truncatedFinal 标记）+ LoopConfig.stream_replay_attempts（root/inference 双通道）。a03 e2e 5 passed；conversation_engine 回归 10 passed。偏差①：settings_store schema 迁移推迟，重放次数 knob 走 harness_config 根键或 inference 子对象（翻转点不变）。偏差②：a03 测试为 journal 可断言性在双截时增加 truncatedFinal Progress 事件（加法）。
- A04 done：tools_call Err → 合成 'error: {e}' 结果回填继续本批。偏差：测试从 plugins/native/tests 移至 r-code-runtime/tests/a04_tool_failure_continues.rs（插件 crate 无 e2e 基建）。1 passed。
- A05 done（续写部分由 A03 预落位）：溢出指引文案（loop 侧追加）+ run.failed errorClass 三态（Self::classify_failure，确定性关键词优先于瞬时——错误链外层固定带 transport failure 包装噪声）+ router assistant.partial 提取（partial output: 分割）。a05 2 passed。


## A06–A15（2026-10-02）
- A06 done：cancelled 专用结局（绝不落入 plan_publish/提案，宿主既有 run.cancelled 结算）；循环前取消检查+plan revision 前置；objective 缺失/空白 Fault；checkpoint/结果编码传播（4 处 unwrap_or_default 清零）；成功后 emit_event 只记日志；steer 持锁跨越 save。3 e2e。
- A07 done：steer 派发 spawn 化+解析失败显式；wait_for_cancel 先 pin 再查；EOF 置取消+排空 pending host_call；host_call 等待 select 取消；shutdown 派发任务 await 后有界排水 writer（1s 窗口+abort 兜底——无界等待实测挂死，教训入注释）；harness.cancel 参数校验；streams 死注册表删除+文档修正。SDK 回归 3 passed。专项 a07 测试顺延。
- A09 done：Err 臂改走 pause_dispatch_if_settled（队列继续）；提前退出锁内清 drive token。
- A10 done：kernel reseed（幂等+input_seq 排序）+ run_drive next_input 空轮询时重播种（每任务一次护栏）；acknowledge 持久先行+失败恢复 in_flight。kernel 3 passed。
- A12 done：Fresh 臂 per-key 互斥+锁内重读；execute 失败写 Rejected 终态（键可重试）；Reconcile(tools.call) 重执行臂同样套锁（B 方案实测教训：外层读到 Indeterminate 走 Reconcile 绕锁双执行）。router 内联 2 passed。
- A15 done：768KB 显式上限+宿主侧单调截断最老 tool-result（占位保留前 200 字符）+checkpoint.truncated 观察+仍超显式失败。router 内联 2 passed。

## 偏差追加（A06–A15）
1. A12 e2e 测试文件删除：ApplicationService::compose 注入的 ToolService 仅作冻结屏障、从不执行（run_manager.rs:744 注释明示），注入工具计数类 e2e 前提不成立；栅栏验证移至 router 内联单测（并发单执行/失败可重试两路径）。a04 的"工具失败续跑"e2e 仍有效——RPC 错误路径真实触发（unknown tool 也产生 RPC error）。
2. A07 writer 排水改为有界窗口：共享 Arc<Shared> 持有 outbound 发送端，通道不会自然关闭，无限 await 挂死（30 分钟实测）；1s 排水+abort 兜底。
3. A15 budget 语义澄清：超限截断循环会持续替换"下一个最老"直到入预算——单条巨型用户消息场景最终显式失败（带指引）。

## 未实施（顺延，含续作锚点）
- A11 接力：LoopResult.stop_reason 字段已就位（A06），缺 run_manager 收尾处 budget_reached→enqueue(Continuation) 注入、链轮数护栏（journal 计数）、run.chained 事件、TUI 系统续跑标记。
- A13 并行：A04（失败续跑）+A12（栅栏）前置已绿；缺 loop_engine for→join_all+顺序回填+parallelTools 开关。
- A14 推理：需同时扩 kernel ports 与 sdk 两处 ModelStreamOutcome（serde default）+全部测试构造点补字段+router assistant.reasoning 观察。
- A08 锁粒度：run_loop 需改签名加按轮合并钩子；A07 已消除 SDK 侧内联等待，死锁对已解一半（serve loop 不再阻塞）。



## 第二轮（2026-10-02 晚）：A11/A13/A14 落地 + 测试文件灾后重建
- A11 done：插件侧 budget_reached 结局（TurnLimit 不再上抛，提案强制 Reply + budgetReached Progress 信号）；宿主侧 run_manager 收尾识别信号 → 链护栏（relay_chain_turns 扫 journal 至最近用户输入，上限 200 常量=翻转点）→ enqueue(Continuation 固定指令) + run.chained/run.chain_stopped 事件。e2e：永续工具任务自动接力、无 TurnLimit 失败、Continuation 入队（kebab-case "continuation"）。
- A13 done：同轮工具 join_all 有界并发（默认 4，parallelTools=0 串行回退），结果按 call 序回填，render_tool_outcome 统一 A04 合成错误渲染。墙钟专项测试顺延：测试注入的 ToolService 从不执行（仅冻结屏障），真实 PlanningToolService 走审批，无免审批慢工具可计时。
- A14 done：ModelStreamOutcome.reasoning（kernel ports + sdk 双落点，serde default）；pump_stream 聚合 ReasoningDelta；router 在 assistant.message 前发 assistant.reasoning 观察。专项 e2e 顺延（mock ReasoningDelta 流）。
- 测试文件灾后重建：批量脚本两轮把 a03/a04/a05/a06/a09/a11 与 tracked conversation_engine/t15/e06 弄伤（括号失衡）。恢复路径：tracked 三文件 git checkout 后用四形态正则（具名 ModelUsage/前缀 ModelUsage/裸 usage,/Default::default()）重插 reasoning: None；五个新文件以统一骨架完全重写。教训入偏差：**结构化代码改动禁用多轮正则/行级脚本，一次到位或手改**。
- 全量回归：runtime lib 82 + e2e 套件 12 组 + kernel 2 组全绿。



## 第三轮（2026-10-03）：全部收尾——16/16 任务代码落地
- A08 done：run_loop 状态参数改共享锁（tokio::sync::Mutex<ConversationState>）短作用域——每次变更短暂持锁、投影/checkpoint/摘要用克隆；session on_start/on_resume 置状态后立即放锁。A06 的 steer 持锁跨越 save 与此正交且兼容（steer 短、循环锁粒度细）。回归 7 套 e2e 全绿。steer 轮间落地的定时测试顺延：生产 harness.steer 无宿主调用方（R4 核实），e2e 无法驱动；SDK 侧 a07 已验证 serve loop 不再被任何 handler 阻塞（死锁对的 SDK 半边实证解除）。
- a07 专项 done：新增 examples/sdk_chaos_fixture（on_start 持 300s host_call）+ tests/a07_sdk_cancel_serve 3 项——EOF 后进程 10s 内退出（EOF 排空生效）、harness.cancel 打断在飞 host_call（select 取消生效）、shutdown ack 帧送达。全部真实 stdio 驱动。
- a13 专项 done：三工具并发批 e2e（join_all cap4 路径）——3 次 tool.result 全配对、run 完成、无卡死。墙钟级并行度证明顺延（见偏差）。
- a14 专项 done：ReasoningModel e2e——assistant.reasoning 观察先于 assistant.message，文本完整。
- 终态：16/16 任务代码落地；回归 = workspace 构建零错 + runtime lib 82 + e2e 14 组 + SDK 4 组 + kernel 3 + agent-llm 161 全绿。



## 第四轮（2026-10-03 复核）：补齐两个真实缺口
- 复核结论：worklist 逐任务对照后仅两处"代码落地但关键机制无 e2e"——A10 宿主侧重播种钩子、A15 router checkpoint 臂。本轮补齐，其余顺延项维持（steer 轮间定时测试：生产无调用方；A09 send 竞态：锁内 3 行改动，检视级；A06 四个子项：代码落地无注入点）。
- A10 e2e done（tests/a10_restart_delivery.rs）：真进程崩溃模拟——rusqlite 直注滞留 input.queued（注意 InputKind kebab-case，"User" 不反序列化会被 rebuild 静默跳过——第三个实测教训），重启后新 service 派发新消息完成后，重播种钩子捞起滞留输入完成第三个 run，回复文本实证 "last=stranded before restart"。G12 全链路（journal→rebuild→reseed→drive→run）端到端闭环。
- A15 router 臂 done（router 内联 a15_checkpoint_router_test）：超限 checkpoint 经 handle_request 走完整臂——截断后保存成功 + checkpoint.truncated 观察；无 tool-result 可截的病态载荷显式失败。阈值教训：decoded_len 估算=base64.len()/4*3，768KB=786,432 字节，测试载荷必须显著超阈。
- 终态复核：16/16 任务全部"代码+机制验证"双落地；回归 15 组 runtime 套件 + lib 86（含 a12/a15 内联）+ SDK 4 组 + kernel 3 + agent-llm 161 全绿。

## 观察项 12 冷启动基线（2026-10-04，tests/cold_start_bench.rs --ignored 手动跑）

- 每 run 一个插件进程的全程墙钟（spawn + initialize 握手 + checkpoint 恢复 + 提案仲裁 + 清理；模型为零耗时脚本）：**中位 597ms / 均值 619ms**（5 样本：569/595/597/597/740ms，Windows debug 构建）。
- 解读：会话式交互可接受；高频接力/波次场景（A11 链每段一个 run）该成本×段数是主要延迟项。进程常驻复用（远期项）的理论收益上限即此值；优化决策留给真实工作负载数据。

## 偏差记录（Deviation log）

（按 agent_rules.deviation_protocol 追加；锚点漂移按符号名解析不在此列，仅记录符号确实迁移/改名的情况。）

## 范围外发现（Found, not in scope）

（实施中发现的、不属于本 worklist 的问题，一行一条；不修，留待观察项或新工作流。）

## 决策翻转记录

（若任一 DEC-1..4 在实施后被推翻：记录日期、翻转点位置、改动。）
