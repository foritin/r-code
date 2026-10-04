# 增补调研：上下文压缩与子代理协作最优实践（2026-09-30）

> 动机：二次代码探查确认 daemon 链路零压缩、记忆注入断线、children 无执行体（见 [project-context plan §2](../support/archive/prd/project-context/prd.md)）。本调研为混合需求 R1（压缩引擎）/ R2（记忆接线）/ R3（子代理执行体）提供业界最优实践依据。来源以官方文档与源码为主，社区逆向均标注。

## 1. 上下文压缩

### 1.1 各家机制对照（要点版）

| 维度 | Claude Code | Codex | Gemini CLI | pi（badlogic/pi-mono） | opencode（sst） | Letta |
| --- | --- | --- | --- | --- | --- | --- |
| 触发 | 接近上限自动 + `/compact`；阈值可配（原生 1M 模型约 967K）【官方】 | 超 `auto_compact_limit`；turn 内撞窗错误也触发重试；配置硬钳在窗口 90%【源码】 | token > 0.5 × 窗口【源码】 | tokens > 窗口 − 16K reserve【源码】 | 估算(system+messages+tools) > 窗口 − max(输出, 20K)——**预留输出空间**【源码】 | 消息数超限（10）触发【源码】 |
| 确定性清理 | tool-result clearing：清旧工具结果保最近 3–5 条（机制官方确认；具体条数二手） | 压缩请求超限时从头删保前缀【源码】 | 50K 工具输出逆向预算：旧的截尾 30 行+落盘留路径【源码】 | 无独立层，但**永不在 toolResult 处切**（配对不拆）【源码】 | 序列化时每工具输出截 2000 字符【源码】 | 截断钳制作降级手段【源码】 |
| LLM 摘要 | 结构化摘要（意图/概念/文件/错误修复/待办/当前工作），摘要调用复用主对话前缀保缓存（二手） | 三路：本地摘要 / 服务端 `encrypted_content`（保隐态）/ 按提供商能力选择【官方+源码】 | 专用压缩模型（flash 档 + thinkingLevel HIGH）；**两遍验证**（第二遍自我批判补漏）；膨胀检查（越压越大则拒绝）【源码】 | 独立摘要请求 + 结构化模板 + **迭代更新摘要**（PRESERVE 规则）；拒绝 length 截断的摘要【源码】 | 单条 user 消息 + Markdown 模板 + `<prior-summary>` 合并更新【源码】 | 摘要调用注入 assistant ACK（"Let me summarize."）防续写【源码】 |
| 重注入 | 根 CLAUDE.md、auto memory（200 行/25KB）、**最近 5 文件（单文件 5000 token，超出留路径引用）**、skills（每 5K/合计 25K）、plan、hooks 重跑【官方】 | **全部用户消息**（20K 预算、新到旧、超预算截断）+ 摘要垫底【源码】 | 摘要 + 保留末 30% + 初始环境上下文【源码】 | 摘要 + 近期 20K + **文件操作账本**（readFiles/modifiedFiles 累积）【源码】 | 摘要 + 近期 8K 原文（双字段存储）【源码】 | core memory blocks 常驻（独立于压缩的持久层）【源码】 |
| cache | 清理/压缩 miss 单列为 "expected rebuild"【官方】 | 严格 exact-prefix；**配置变化靠追加消息而非改写历史**【官方+源码】 | 不保 cache | 摘要请求显式关缓存写入（省 1.25x 写费）【源码】 | 独立一次性请求 | — |

关键源码：[codex compact.rs](https://github.com/openai/codex/blob/master/codex-rs/core/src/compact.rs)、[gemini chatCompressionService.ts](https://github.com/google-gemini/gemini-cli/blob/master/packages/core/src/context/chatCompressionService.ts)、[pi compaction.ts](https://github.com/badlogic/pi-mono/blob/master/packages/coding-agent/src/core/compaction/compaction.ts)、[opencode compaction.ts](https://github.com/sst/opencode/blob/master/packages/core/src/session/compaction.ts)、[letta summarizer.py](https://github.com/letta-ai/letta/blob/master/letta/services/summarizer/summarizer.py)。

### 1.2 三条路线的结论

- **A 确定性清理**：Anthropic 官方定性"最安全最轻量"；Gemini 证明纯规则可做且保证"压缩必然减上下文"。**必须配冷存储**（清理只丢 blob 不丢可回读引用）——社区实证（Reddit "246M tokens in 22 hours"）：清了旧工具结果模型反复重读文件，成本反增。
- **B LLM 摘要**：六家独立收敛到**同一套结构化模板**（Goal / Constraints / Progress(Done/Active/Blocked) / Decisions / Next Steps / Critical Context+Relevant Files——pi 与 opencode 逐字段相同，强趋同信号）。Codex 源码在每次压缩后发官方警告："Long threads and multiple compactions can cause the model to be less accurate"——多次压缩累积精度损失是官方承认的。
- **C 服务端加密压缩**（Codex `encrypted_content`）：保隐态质量高但锁死 Responses API、opaque 不可审计——**对多 harness 宿主产品不可移植**，只可作为"提供商能力探测"的可选路径。
- 行业基线：20 家 harness 调查（二手）——15 家自动压缩、10 家开放阈值配置；**阈值可配是标配预期**。

### 1.3 宿主侧统一压缩：业界印证与风险

**印证**（方向正确）：Codex 的压缩就全在 harness（compact.rs）完成、模型侧只收替换后 input；opencode 有独立 projector.ts（存储完整 transcript、发模型的是投影）；pi 以 JSONL 为 source of truth、压缩只是追加 CompactionEntry 记 firstKeptEntryId；Claude Code 的重注入表证明宿主完全可以按需重组供给（CLAUDE.md/skills/文件全部磁盘按需重注入）。

**风险**（设计规避）：
1. 改写已发出前缀 = 缓存全灭（Anthropic 层级 tools→system→messages；OpenAI 中途改 tools/model 即 miss）→ 宿主只做**尾部追加**，工具列表/系统提示会话中途不改，工具列表稳定排序；
2. 宿主投影与插件 token 记账不一致会让插件"以为没满" → 宿主向插件暴露统一 token 计数（Gemini 模式）；
3. 裁剪拆散 tool_use/tool_result 配对 = API 400/缓存失效 → 切点只在消息边界（pi 规则）；
4. 压缩窗口期竞态 → 原子事务、失败不落盘（Codex post-turn 模式）；
5. **四家都有压缩前/后钩子**（Claude PreCompact/PostCompact、Codex、Gemini PreCompress、pi before_compact）→ 宿主必须给插件同等的压缩事件。

### 1.4 推荐分层方案（r-code R1 的蓝图）

- **L0 源头限流**：工具输出上限（10–25K）+ 超限落盘冷存储留路径引用（现有 read/search/bash 截断已在此层，补冷存储）；
- **L1 确定性清理**：每次请求前清旧 tool_result 保最近 3–5 条；判据"工具调用已深入历史"；占位符保留调用摘要（跑过什么命令/搜过什么）；切点永不拆配对；
- **L2 LLM 摘要**（阈值触发，可配）：趋同模板 + 质量组合拳（两遍验证 + 迭代更新 PRESERVE + 拒绝截断摘要 + 膨胀检查 + 失败不落盘仅截断降级）；压缩模型可独立配置（低档即可，Gemini 给 HIGH thinking 的做法可选）；
- **L3 重注入包**：摘要（带"不要复述、直接继续"指令）+ 最近 5 文件（单文件 5000 token 上限）+ 记忆/项目指令磁盘重注入 + **全部用户消息（20K 预算）** + Plan 目标/todo + 文件操作账本；
- **L4 长期记忆**：压缩解决不了的交给外部化状态（本计划 L1/L3 的指令与 Plan 工件正是这一层）。

## 2. 子代理协作

### 2.1 各家体系对照（要点版）

| 维度 | Claude Code subagents | Codex multi-agent | Gemini CLI | Cursor | opencode/goose/OpenHands |
| --- | --- | --- | --- | --- | --- |
| 角色 | 自定义 + 内置 Explore/Plan（跳过 CLAUDE.md） | default/worker/explorer（monitor 已移除——长等待职责并入 wait 工具） | codebase_investigator（只读四工具）等 + 自定义 | Explore/Bash/Browser + 自定义编排 | 各有内置+自定义；均**禁止子代理再派生子代理**（防递归） |
| 上下文 | 独立窗口、不继承历史（fork 显式例外）；注入 CLAUDE.md 层级+git 快照+skills+sibling roster | 父 turn 派生 base/developer instructions + 运行时继承 approval/cwd/sandbox；**fork_turns: none/all/N 中间档** | 独立 system prompt + 显式工具集 | "start with a clean context"，父须在 prompt 自带信息 | 工厂构造，父只传 prompt 文本 |
| 触发 | description 自动 / @agent 强制 | spawn_agent 工具（描述明令：用户未要求不得 spawn） | description 自动 / @name | 自动 / 命令强制 | Task tool + subagent_type |
| 回传 | Agent tool 结果（带注入扫描） | final answer 经 mailbox 通知 | **zod 强制三段式 JSON**（SummaryOfFindings/ExplorationTrace/RelevantLocations[{FilePath,Reasoning,KeySymbols}]）经 complete_task 结构化返回 | 结构化 handoff | TaskObservation / JSON |
| 并发上限 | 默认 20 / 嵌套 3（env 可调） | 文档默认 max_threads=6 / max_depth=1；完成后不 close 占额度 | max_turns 30 / timeout 10min | 单消息多 Task 并行 | OpenHands 显式串行 |
| 失败 | partial+截断说明；per-child 取消 | 状态机 pending_init/running/interrupted/shutdown/completed/errored | 截断 | 可 resume | goose：失败静默丢弃，只收成功者结果 |

源码级关键点：[codex role.rs](https://github.com/openai/codex/blob/master/codex-rs/core/src/agent/role.rs) 首行注释："role may customize the child or **reduce** its capabilities, but **never replace the parent session's authority**"——宿主权威的源码级表述；Codex worker 角色描述要求明确 assign **ownership**（disjoint 写集）并告知 "not alone in the codebase… should not revert the edits made by others"；[spawn 工具描述](https://github.com/openai/codex/blob/master/codex-rs/core/src/tools/handlers/multi_agents_spec.rs) 编码了委派纪律：未明确要求不得 spawn、关键路径任务本地做、子任务必须 concrete/self-contained/disjoint、委派后勿重复劳动勿反射式 wait。

### 2.2 子代理上下文组装共识（回答开放问题 10）

1. **不继承主对话历史**（最强共识，Claude/Cursor/Gemini/Anthropic 四方一致）——状态靠持久化工件重建，不靠窗口继承；
2. **委派 prompt 四要素**（Anthropic）：objective、output format、工具与来源指引、任务边界；
3. **系统级最小注入集**：记忆/规则层级 + git 快照 + 工具白名单；**侦察角色跳过重上下文**（共享记忆按角色裁剪）；
4. **历史 fork 旋钮**：none / all / N 中间档（Codex），不是二选一；
5. **角色决定工具集**；角色层只减不增（宿主权威）；
6. **worker 特殊上下文**：明确写集归属 + "你不是一个人在改代码"；
7. **记忆按需持久化而非随窗口传递**（Claude：推荐子代理把学到的东西写进自己的 memory）；
8. **摘要预算 1,000–2,000 token**（"sub-agents operate as intelligent filters"）。

### 2.3 回传格式推荐

- 给人/主代理读：Markdown 摘要 1–2K token；
- 给机器消费：**结构化 JSON/schema**（Gemini 三段式是最佳样板；Anthropic："模型更不容易擅自改写 JSON"）；
- 重产物：**文件落盘 + 只回路径清单**（"A message does not transfer files"）；
- 回传内容视为**不可信输入**（注入扫描；Claude 给子代理输出打标"carries no authorization"；Codex 定义 ExternalMessage）；
- "handoff 四要素（完成/疑虑/偏差/发现）"在 Cursor 官方文档无逐字验证——采纳为设计惯例，标注社区出处。

### 2.4 宿主权威架构映射（r-code R3 的蓝图）

**直接可抄**：角色只减不增（Codex role.rs，r-code 的 rank 天花板已同构）；审批上浮宿主（Claude teammate 不能代批权限——r-code 已同构）；状态机 + 显式 close 回收并发额度；wait 长超时防忙轮询（做 wait 原语而非 monitor 角色）；委派纪律写进 spawn 工具描述（Codex 范本全文可嵌）；回传三件套 + 注入扫描；mailbox "写入成功才算送达" 的确认语义。

**需要变通**：并发默认从 Codex 的 6/嵌套 1 起步（Anthropic 数据：agents ≈ 4× chat token、multi-agent ≈ 15×，且多数 coding 任务并行度低）；文件型 mailbox → 宿主进程内队列（但子代理 transcript JSONL 持久化 + 断点恢复保留）；防递归用"子代理不可再派生"最简方案；批量扇出（spawn_agents_on_csv）实现为结构化任务数组。

## 3. 对 r-code 的落地结论

1. **R1 蓝图确认**：宿主侧统一压缩有业界先例（Codex/opencode/pi 三家同构），分层 L0–L4 如 §1.4；开放问题 11 的答案：transcript 裁剪由**宿主执行**（daemon 维护完整 transcript 为 source of truth，投影给插件），cache 纪律为"只追加不改写 + 工具列表中途不变 + 稳定排序"，压缩前后给插件事件钩子。
2. **R2 细化**：重注入包组成照 §1.4 L3（含"全部用户消息 20K 预算"这条 Codex 独有洞见——用户原话最不该丢）；`FrozenChildMemorySeed` 的语义对齐"系统级最小注入集 + 侦察角色跳过"。
3. **R3 细化**：委派 prompt 四要素 + 回传三件套 + 状态机/显式 close + wait 原语 + 委派纪律工具描述；子代理上下文 = objective + 冻结记忆 seed + git 快照 + 工具白名单（+可选 fork 中间档），**不含主对话全量**——回答开放问题 10。
4. **负责任的产品细节**：压缩后向用户显示 Codex 式提醒（"多次压缩可能降低准确性，建议适时新开会话"）；确定性清理必须配冷存储引用，防"清理导致反复重读"的成本反增。
