# DeepSeek Harness 与 R-Code 可借鉴性综合评估

> 维护者参考文档。本文记录对官方 DeepSeek Harness（`deepseek-ai/deepseek-harness`）
> 与本仓库本地 Rust 移植版（`.reference/rust-deepseek-harness`）的对照调研，由此得到的
> R-Code 优化建议，以及（2026-08-17 新增）三层架构分层方案：
> **R-Code（产品）→ harness（运行时子模块）→ agent-contracts（合同子模块）**。
> 它是评估结论，不是已批准的实施计划；任何落地改动都应在单独的变更中
> 先做可行性评估与测试。文中引用的行号以调研时为准，可能随代码演进而漂移。
> （2026-08-16 复核：已对照官方仓库 `master` 文档与本仓库代码逐条核验，事实性偏差已在
> 文内修正，新增 P2-H / `request/context` / fork 边界 / 压缩事件化等遗漏点。）
> （2026-08-17 复核：agent-core 已更名 agent-contracts 并固定在 `vendor/agent-contracts`
> @ `1d029f6`；本文新增第 5 章「agent-contracts 现状盘点」与第 7 章「三层架构分层方案」，
> 并按分层重写建议清单落点。复核中发现一个疑似回归（见 §5.4），需要优先处理。）

## 1. 背景与目的

DeepSeek 于 2026-08-13 开源了官方 agent harness（MIT），核心理念是
**「一切皆插件（Everything is a plugin）」** 与 **「每次运行可追溯（Every run is traceable）」**。
本仓库另有一份本地 Rust 移植版，规模小、可读性强，刻意不做大而全。

本文回答四个问题：

1. 哪些设计值得借鉴--能提升 R-Code 的可追溯性、可靠性与可扩展性；
2. 哪些不适合照搬--会破坏现有架构或收益不足；
3. `agent-contracts` 子模块当前覆盖了什么、还缺什么；
4. **架构上是否应当引入中间层 harness 子模块（R-Code → harness → agent-contracts），
   三层各自实现哪些功能才能最大化解耦。**

**结论先行**：

- R-Code 在会话双存储、上下文压缩、权限审批、子代理候选池、三层重放等方面
  已经成型甚至领先；真正缺的是两个 harness 反复强调的「结构性纪律」--尤其是
  「模型可见 ⟺ 已记录」的派发期自检，以及工具管线的失败隔离、超时与可逆注册。
- **三层架构可行且合理**：`agent-contracts` 已经是干净的「合同 + 协议适配」层；
  `r-code-agent-worker` 中约 6,000 行是产品无关的通用运行时逻辑（循环核、调度、
  预算护栏、委派树、检查点、缓存形状、steer 机制），它们应当成为 harness 层；
  权限审批、Plan、记忆、候选池、外部 runner 等约 9,000 行编排逻辑留在产品层。
  下沉需要先做三个接缝：事件枚举泛型化、错误类型抽象、审批网关 trait 化。
- 复核中发现 `agent-store` 的 `session_path` 硬编码文件名且忽略会话 id
  （见 §5.4），与宿主读取路径分裂，疑似 rename 期间引入的回归，应立即修复。

## 2. 四个对照对象

### 2.1 官方 DeepSeek Harness（`deepseek-ai/deepseek-harness`）

- 技术栈：TypeScript / Node（pnpm monorepo），不是 Rust。
- 底层内核：[Cordis](https://github.com/cordiverse/cordis)，一个「时空可组合」的插件内核。
- 核心理念：模型、工具、技能、会话、沙箱、存储、循环、调度、UI 全部抽象为可替换插件。
- 强约束：**Model-visible ⟺ logged**--任何进入模型请求的内容都必须能从会话日志重建，
  否则运行时断言失败。
- 多运行时模式：Standard（全工具）、Code（模型写 TypeScript 编排多轮工具）、
  Minimal（仅 shell + str_replace_editor，用于基准测试）、Creator（运行时自省 + 组合新模式）。
- 状态：developer preview v0.1，明确声明会有破坏性变更。

### 2.2 本地 Rust 移植版（`.reference/rust-deepseek-harness`）

- 用 Rust 写的 agent 运行时，核心思路参考官方 harness，但目标是
  **「小到能读懂、稳到敢用它跑长任务」**。
- 关键能力：事件溯源 JSONL、崩溃后可 `resume` 续跑、`show` 回放、`sessions` 列出历史、
  `fork`/`spawn` 子代理、带 `cite_seq` 引用的长期记忆、`web_search`/`web_fetch`。
- 单 crate 直板结构（src/ 下 session/tool/inbox/subagent/agent/persistence 等平铺），
  无 workspace 分层--这是它与本文提议的 harness 层最大的结构差异。
- 贯穿全项目的九条可靠性硬规矩（见该仓库 README），其中与本评估最相关的是：
  1. 模型可见 ⇔ 已记录（`assert_request_reconstructable`）；
  2. 事件日志是唯一真相，消息列表只是重放投影；
  3. Turn ≠ Step，生命周期括号必须闭合；
  4. 单一收件箱（followup / steer / inject）；
  5. 能力只通过注册表进入，注册返回 `EffectGuard`，析构自动卸载；
  6. 并行不改变模型可见顺序；
  7. 多代理委派是能力，不是第二套运行时；
  8. 工具 panic 是一次调用失败，不是会话死亡（`catch_unwind` -> `TOOL_ERROR`）；
  9. 坏数据宁可拒载，绝不猜着修复。

### 2.3 agent-contracts 子模块（`vendor/agent-contracts` @ `1d029f6`）

- 独立 git 仓库（`foritin/agent-contracts`），由父仓以 gitlink 固定 commit，
  9 个 crate 以 workspace member 方式挂入本仓 workspace（`Cargo.toml:5-13`）。
- 自我定位（README）：R-Code 桌面 IDE 与 Tiny Hermes 共享的「公共层合同」，
  设计原则 Trait 优先 / 零成本抽象 / 错误统一 / 异步优先；
  明文规定公共 crate 不得反向依赖产品私有 crate。
- 2026-08-16 完成 `hermes-*` → `agent-*`、`agent-core` → `agent-contracts` 两连更名。
- 9 crate 分工（详见第 5 章盘点）：`agent-error`（错误合同）、`agent-contract`
  （核心类型合同，约 1,100 行）、`agent-llm`（四协议 provider 适配）、`agent-mcp`
  （MCP 双传输客户端）、`agent-store`（JSONL 事件日志）、`agent-config`（配置 schema）、
  `agent-compaction`（压缩策略）、`agent-ipc`（JSON-RPC 传输）、`agent-tauri`
  （运行时无关壳状态）。
- 边界纪律靠三条线维持：文档编号回链（00–15 篇合同规范）、`contract-lock.json`
  人肉账本（无机器强制）、父仓 CI 的 submodule-pin job（`ci.yml:205`，比对 gitlink
  与 checkout HEAD）。

### 2.4 R-Code（本仓库）

R-Code 是 Rust + Tauri 的多 crate 桌面应用，核心是 session-first 的对话模型与可审计的 Run。
其相关分层见 `docs/architecture.md`。与本评估直接相关的子系统：

- 会话存储：JSONL 为 source of truth（由 `agent-store` 承担），SQLite 为产品投影（双存储）；
- Agent 运行时：`crates/r-code-agent-worker`（约 2.4 万行，其中 8,117 行为测试）；
- 工具网关与权限：`crates/r-code-gateway`；
- 产品存储：`crates/r-code-store`；
- 重放：`src-tauri/src/replay.rs`；
- 子代理与候选池：`crates/r-code-agent-worker/src/llm_runtime.rs`、
  `src-tauri/src/subagent_providers.rs`（HMAC 健康回执在此）、`src-tauri/src/commands.rs`；
- 演进记忆：`crates/r-code-core/src/memory.rs`、`crates/r-code-store/src/memory_store.rs`。

## 3. 两个 harness 独立收敛出的核心设计

官方实现与 Rust 移植版不约而同地把下面几条当作主线。这种跨语言、跨实现的重复收敛，
是「这些设计具有普遍价值」的最强信号，也是本评估建议的主要依据。
每节末尾标注该设计在三层架构中的归属层。

### 3.1 事件溯源：事件日志是唯一真相，消息历史是「投影」【合同 + 存储层】

- Rust 移植版：`Session` 持有权威 `Vec<SessionEvent>`；`derive_messages()` 是纯函数，
  只投影 UserMessage / 非空 AssistantMessage / ToolResult，消息历史**从不单独存储**。
- 官方版：`SessionEvent` append-only，`deriveMessages()` 从日志投影；
  `assistant/chunk` 只用于重放保真、不参与派生。
- 会话日志类型化且可合并扩展（`SessionEventMap` declaration merging），
  `turn/start`、`step/start`、`user/message`、`assistant/message`、`tool/call`、
  `tool/result`、`request/header` 等都是显式事件。
- agent-contracts 已有此形状的骨架：`SessionEvent` 8 变体（Meta / Message / Usage /
  ToolCall / ToolResult / HistorySnapshot / ModelProjection / System，
  `agent-contract/src/session.rs:44-76`），`SessionStore::load` 重建投影、
  `HistorySnapshot` 替换前缀并使旧投影失效（`session_store.rs:223-231`）。
  缺官方那套 turn/step 生命周期事件与显式 seq（见 §6）。

### 3.2 模型可见 ⟺ 已记录，且派发前自检【合同 + 运行时层】

- Rust 移植版核心亮点：`assert_request_reconstructable()`--派发前把
  `request.messages` 与 `derive_messages()`、请求信封（config/system/tools）与最新
  `request/header` **硬比较**，不一致即报错。
- 官方版：`request/header` 快照（system prompt + 工具 schema + 采样参数）进入日志，
  使每个请求都成为日志的纯函数。
- 官方版把路由容量（provider / model / contextWindow）拆到独立的 `request/context`，
  只在路由或容量变化时追加；它不参与重建判等，避免「换路由」被误报成「请求信封变化」。
- agent-contracts **尚无** `RequestHeader` 事件变体；R-Code 的 P2-H
  （`cache_shape.rs`）只做内存级哈希归因、不持久化--合同与运行时两侧都待补。
- 这条纪律的价值在于：重放、fork、上下文压缩、子代理继承父历史，全部变成
  「同一事件流的投影」，而不是「历史存两份、两边不一致」。

### 3.3 能力 = 注册表 + 可逆注册 + 大声失败【运行时层】

- Rust 移植版：工具 / 钩子 / 适配器 / 提示词 / 子代理五个接缝；注册返回
  `EffectGuard`，drop 自动卸载（栈式覆盖）。
- 官方版：capability seam 三角色（Service Definition / Service Provider / Consumer）；
  子代理能力缺失抛 `UNSUPPORTED_CAPABILITY`，绝不静默降级。
- agent-contracts 的 `ToolHost` trait（`tool_host.rs:52-79`）只有裸 `call` 与默认串行
  `call_batch`；`requires_confirmation`（默认 true）是声明字段，审批执行流在产品侧
  SQLite。注册、卸载、单调守卫均缺--全部是 harness 层职责。

### 3.4 工具执行管线：阶段化、可插拔【运行时层】

官方版的管线把横切关注点从工具 body 里拆出来：

```
tool/call
  -> tools/pre-execute   （hook / 权限 / 沙箱；ask -> 转入一次性审批）
  -> approval            （ctx.approval 一次性 prompt；缺席或无法回答即 deny）
  -> monotonic guards    （只可 deny 或弃权，不可把 deny 改回 allow）
  -> tools/execute       （timeout / retry / metrics，围绕 dispatch）
  -> tools/post-execute  （accept / block / replace / 追加上下文）
  -> normalize           （管线/result 快照异常归一为 isError）
  -> finalizeContent     （纯同步的内容校验）
  -> tools/result        （冻结的最终结果）
```

Rust 移植版虽然管线更简单，但保留了其中最关键的可靠性点：工具 panic 用
`catch_unwind` 隔离成一次失败调用；并行执行但按调用顺序提交，不改变模型可见顺序。

整条管线（含 panic 隔离、超时、并发调度、审批 hook 的**调用时机**）属于 harness 层；
审批的**判定本体**（standing rule、risk ceiling、UI 呈现）属于产品层。

### 3.5 子代理、收件箱与生命周期【运行时骨架 + 产品编排】

- Rust 移植版：单一 `Inbox`，双队列 `next_turn` / `next_step`；三种投递语义
  `followup`（NextTurn + wakeup）、`steer`（NextStep + wakeup，当前轮生效）、
  `inject`（NextStep + 不 wakeup，静默注入）；`claim` 优先清 NextStep 再取 1 条 NextTurn。
- 官方版：核心 `Agent` 同样是 `followup`（wake + NextTurn）/ `steer`（wake + NextStep）/
  `inject`（不 wake）三个预设别名，收件箱生命周期由 `agent/inbox/inserted|claimed|discarded`
  观测；多个 named provider 共存（spawn / fork / acp / codex / claude-code）；start-time
  能力标志缺失即大声失败；可延续子代理（durable child session + activation + followup +
  report + settled 通知）；`listChildren` / `listDescendants` 从持久 session store 枚举；
  `maxDepth` 持久化在 `SessionHeader.delegationDepth`，冷恢复不能降低。
- 分层判定：双队列收件箱、深度/数量/并发预算、树拓扑是 harness 件；候选池、
  跨引擎路由（Codex）、能力钳制（TaskMode/ProjectAccessMode）是产品件。

### 3.6 长期记忆【产品层，harness 只提供引用机制】

Rust 移植版用 `cite_seq` 把记忆锚定到事件日志序列号，可审计、可 `memory_expand` 验证原文；
召回采用拉丁词 + CJK 短语/字符二元组（带停用字过滤）；注入分两层（常驻索引 + 关键词命中全文），
每条带 `[id=… cite=event#N]` 引用。

记忆的内容模型、审核流水线、scope 全在产品域；harness 侧只值得借「把引用锚到
事件 seq」这一机制（若做记忆召回，见 P1-7）。

## 4. R-Code 现状

### 4.1 会话与重放

- JSONL 为 source of truth（`agent-store`），SQLite 为产品投影（双存储）；已有 append-only、
  per-path 锁、坏尾截断、分支表。
- 后端 `parse_session_messages`（`commands.rs`）逐行投影 Timeline；`SessionStore::load` 折叠为
  `Session.messages + model_projection`，经 `ensure_runtime_session` / `replace_context` 注入。
- `ReplayService` 三层（Recap / Explore / Verify），过滤 Thinking，证据分级
  Verified > Recorded > Observed > Inferred > Missing，证据缺失返回 "records cannot confirm"。
- 上下文压缩 P2-G 分层（当前实现为 75% 仅提示一次 / 85% 摘要折叠 + 连续 2 次防抖；
  参考设计 50/60/80 三档中的「60% 剪旧工具结果」档尚未落地），从 canonical 重算、
  绝不用旧摘要再摘要，`model_projection` 独立持久化，`rewrite_version` 归因。
  注意：该状态机内联在 `llm_runtime.rs:2397+`（`CompactionState`），**没有**使用
  `agent-compaction` crate（详见 §5.3）。
- fork/resume：`task_fork_context` 复制整段事件到新 storage id；`/clear` 建空分支；
  子代理独立 JSONL。
- 前缀形状归因（P2-H）：`cache_shape.rs` 每轮请求前对 system 哈希 + tools 哈希 +
  `rewrite_version` 捕获 `PrefixShape`，与上一轮比对，变化归因 System / Tools / Rewrite；
  目前只写 tracing 日志、不落 JSONL，也不覆盖尾部注入，是缓存观测而非请求重建。

### 4.2 工具与权限

- `Tool` trait 定义工具；`ToolGateway` 注册为直接 `insert`（后注册覆盖同名），
  无可逆卸载；`policy_guard` 为单槽。
- 权限引擎：standing rule 带 `risk_ceiling`，pending request 与终止决策在同一 `Mutex` 下
  原子提交（这是亮点）。
- 并发调度：仅当整批全为只读工具时才并发（`agent_loop.rs:81`，
  `MAX_PARALLEL_READ_TOOL_CALLS = 4`，`chunks(4)` + `join_all` 分块执行），结果按 `zip`
  顺序提交；缺 per-call exclusive/parallel 语义、滚动并发、失败后其余任务的生命周期管理。
- 前端呈现：`ToolCard` 对少数 MCP 工具硬编码确认卡，属有意设计。

### 4.3 子代理与委派

- `SubagentSupervisor` 持 depth、semaphore、进程内 `DelegationTree`、`children: HashMap`、
  root-run 冻结候选池。
- `delegate_task` -> `route_backend_for_run`（含 deterministic_candidate_roll 按 blake3 加权，
  `llm_runtime.rs:4291`）-> `spawn_with_run_id`；`AbortOnDropJoinHandle` 防 detached 泄漏。
- 候选池：keyed blake3 指纹 + HMAC 健康回执（回执在宿主
  `src-tauri/src/subagent_providers.rs:586-587`，`hmac-sha256-v1` 前缀）、全或无启用；
  `SubagentProviderCapabilities { supports_host_delegation, supports_live_messages,
  supports_full_access }`。
- 收件箱/steer：`steer_queue: VecDeque` + `accepting_steer` 门（`llm_runtime.rs:1642-1644`），
  锁内原子并入下一请求（`:3247-3267`）；`send_agent_message` 限直接父/子/兄弟，
  带背压、幂等键、终态竞态处理。
- 预算常量：`MAX_SUBAGENT_DEPTH = 2`（`:59`）、`MAX_DESCENDANTS_PER_TREE = 12`（`:61`）、
  `MAX_ACTIVE_DESCENDANTS = 5`（`:63`）。
- 无 fork / continuable / per-request max_depth。

### 4.4 演进记忆

- SQLite、global/project 双 scope、审核流水线、`render_snapshot` XML 全量注入 ->
  `build_memory_context_message`（run 冻结）、`record_injection` 审计。
- 无召回层--全量注入、无引用、无检索。

### 4.5 依赖关系现状（2026-08-17 核验）

- 6 个产品 crate（core/store/gateway/mcp/terminal/agent-worker）以
  `{ workspace = true }` 平铺依赖 agent-* crates，**无中间层**。
- `r-code-agent-worker` 的 Cargo.toml 声明了 `agent-store` 与 `agent-compaction`，
  但源码与测试中**零引用**（死依赖）；压缩由 `llm_runtime.rs` 内联实现。
- 双层事件体系：对内（provider 流）用 `agent_contract::StreamEvent`；对外（宿主/WebView）
  用 `r_code_core::dto::AgentEvent`（`dto.rs:1472`）--这是通用层与产品层之间
  最清晰的既有接缝。

## 5. agent-contracts 现状盘点（2026-08-17 新增）

### 5.1 能力清单

| crate | 行数/规模 | 定位 | 关键类型 |
| --- | --- | --- | --- |
| `agent-error` | 小 | 错误合同 + 纯函数恢复策略表 | `Error`、`RecoveryStrategy`（`strategy_for` 只给策略不执行） |
| `agent-contract` | ~1,106 | 核心类型合同 | `Message`/`ContentBlock`（含 Custom 透传、cancelled 配对）、`SessionEvent` 8 变体、`Session`（canonical + `model_projection` 双轨）、`LlmProvider`、`CompletionRequest`、`StreamEvent`（含 HostedToolUse/Result 服务端托管）、`ToolSpec`/`ToolHost`、`Usage`、`CompactionStrategy` |
| `agent-llm` | 大（anthropic 2,178 / openai 2,625 / responses 2,575 行） | 纯协议适配 | Anthropic / OpenAI Chat / OpenAI Responses / DeepSeek 四协议 × 厂商方言（`dialect.rs`）、`MockProvider`、`create_provider` 工厂 |
| `agent-store` | 中 | JSONL 事件日志 | `SessionStore`：per-path 追加锁、坏尾截断恢复（V-STORE-01）、gzip 归档、durable user message 幂等 outbox |
| `agent-mcp` | 中 | MCP 双传输客户端 | stdio + Streamable HTTP、`McpToolHost` 聚合（`server__tool` 命名空间） |
| `agent-compaction` | 中 | 压缩策略 | `SlidingWindow` / `LlmSummary` / `Smart` / `Noop` + `CompactionManager` |
| `agent-config` | 中 | 配置 schema | TOML + env + 验证 + 脱敏；含 `OrchestrationConfig`（编排策略影子，见 5.2） |
| `agent-ipc` | 小 | 通用传输 | JSON-RPC 2.0 over Unix Socket / Named Pipe，`[4B 长度][JSON]` 帧 |
| `agent-tauri` | 小 | 壳状态容器 | `AppEvent` broadcast、`AppState`；MCP 失败回退 `NullToolHost` |

「合同 vs 实现」边界总体清晰：`agent-llm` / `agent-mcp` / `agent-ipc` 是干净的纯协议实现；
模糊点见 5.2。

### 5.2 边界渗漏（应在分层动作中一并清理）

1. **产品事件名前缀**：`agent-store` 的
   `DURABLE_USER_MESSAGE_EVENT = "r_code_durable_user_message"`（`session_store.rs:21-22`）
   把产品身份烧进公共 crate。应改为中性名（如 `durable_user_message`）或允许宿主注入命名空间。
2. **产品引擎枚举**：`agent-config` 的 `MainAgentEngine::RCode` / `QualityReviewer::RCode`
   与默认值 `default_agent_engine = "r_code"`（`lib.rs:296,436,681`）、
   `SubagentProviderSource::CodexCli`--公共 schema 硬编码产品身份。
   应改为 `Engine::Native` 之类的中性命名，产品侧做映射。
3. **运行时策略进配置层**：`OrchestrationConfig`（MainAgentEngine / DelegationRouterMode /
   QualityLoop / SubagentPool 加权槽位 / RunBudget 护栏，`lib.rs:266-291`）是运行时策略
   的影子。分层后这些 schema 的**消费方在 harness 层**，留在 agent-config 不算错
   （配置 schema 本身是合同），但枚举值必须中性化。

### 5.3 与完整 harness 运行时相比缺的机制（全部是 harness 层职责）

1. Agent 循环本体（turn 驱动、`StopReason::ToolUse` 分发）--在 r-code-agent-worker；
2. 工具执行管线：pre/post hook、审批执行流、超时、取消传播、并发调度策略；
3. steer/inbox 的运行时消费端（公共层只有 durable outbox 的写与物化）；
4. 子代理委派执行器、跨引擎路由；
5. panic 隔离（全仓库无 `catch_unwind`）、run budget 执行、停止信号；
6. 检查点/恢复编排（worker 有 `checkpoint.rs`）;
7. 权限模型执行、沙箱（产品域，但需要 harness 提供接缝）；
8. 重试/退避的实际执行（`agent-error` 只给枚举）。

一个重要事实：**`agent-compaction` 与 r-code 的 P2-G 压缩是两套独立实现**。
P2-G 的分层防抖、canonical 重算、`rewrite_version` 归因（产品需求）没有进
`agent-compaction`；`agent-compaction` 的 SlidingWindow/LlmSummary 也没被 r-code 用。
分层时需决定：合并（把 P2-G 状态机下沉为 harness 通用件、`agent-compaction` 策略
作为其可插拔策略）或明确废弃一边。不建议长期维持两套。

### 5.4 疑似回归：`session_path` 硬编码（需立即验证修复）

`agent-store` 的 `SessionStore::session_path` **忽略 `id` 参数**，硬编码返回
`base_dir.join("glm-5.3_common.jsonl")`（`session_store.rs:47-49`），且该内容已提交至
子模块 HEAD（`1d029f6`）。而宿主侧：

- `ensure_session_log` 用 `session_file_path(sessions_dir, storage_id)` 即
  `{storage_id}.jsonl` 判断文件是否存在（`commands.rs:4844-4846, 4869`）；
- Timeline 读取、fork 复制、导出等 14 处全部按 `{storage_id}.jsonl` 定位；
- 单测 `agent_send_creates_session` 断言 `{task.id}.jsonl` 存在（`commands.rs:24663`）。

若 `SessionStore::append` 写 `glm-5.3_common.jsonl` 而读取走 `{storage_id}.jsonl`，
则写读路径分裂：所有会话事件混写进同一文件、Timeline 永远读到空。
该写法随 2026-08-16 的 rename 提交进入子模块，疑似 rename 期间的调试残留。
**建议立即在子模块修复为 `format!("{id}.jsonl")` 并跑宿主测试验证**；
若 `glm-5.3_common.jsonl` 有历史数据需要迁移，应写一次性归并脚本。
（本文档只记录发现，修复应在独立变更中进行。）

### 5.5 contract-lock.json 与 CI 约束

- `contract-lock.json` 是**人肉账本**（publicContract v0.3.0 + 最后验证 commit + 变更日志），
  自述「不是 CI 的 pin 依据--真正的 pin 是父仓记录的 gitlink」。
- 真正的机器约束是父仓 `ci.yml:205` 的 submodule-pin job：比对父仓 gitlink 与
  checkout 出的 HEAD，防漂移；agent-contracts 自身另有 fmt/clippy/test/audit/deny。

## 6. 差距分析（按层标注归属）

### 6.1 会话与重放

| 维度 | R-Code 现状 | 差距 | 归属层 |
| --- | --- | --- | --- |
| 生命周期与 seq | 无 Turn/Step 事件、无显式 seq、无 `validate()` | 无法校验逻辑完整性，只能靠消息层 `repair_dangling_tool_uses` | 合同（事件变体）+ harness（校验器） |
| request/header 快照 | system/tools/tail 注入在 `llm_runtime` 组装；P2-H 只做内存级哈希归因，不持久化、不覆盖尾部注入 | **最大结构性缺口**，无法重建某轮实际发送内容 | 合同（`RequestHeader` 变体）+ harness（重建自检） |
| 历史投影 | `HistorySnapshot` 内嵌完整 messages 与 audit 事件双份存储 | 可能漂移，与「消息历史是投影」相悖 | harness（快照生成策略） |
| 坏数据策略 | `load` 全量解析失败即报错；`replay.rs` 静默跳过坏行 | 策略不一致，非「宁拒载不猜测」；官方规则：未识别事件无 `ignorable` 标记必须拒载 | 合同（serde 策略）+ 产品（replay 标注） |
| fork 边界 | 整段复制（含未平衡 step），无 `parent_session/seed_length` | 边界弱 | 合同（边界事件）+ harness（balanced prefix 判定） |
| 压缩 | P2-G 分层、从证据重算，**已领先 harness** | 缺口在压缩不可审计回放（ModelProjection 在 replay 被静默忽略）；官方把压缩做成日志事件（`compaction/start|summary|end` + surfaceOp） | 合同（事件）+ harness（状态机） |

### 6.2 工具与权限

| 维度 | 差距 | 归属层 |
| --- | --- | --- |
| panic 隔离 | 完全没有（进程内工具 panic 会污染调用） | harness |
| 超时 / 执行模式 | 调度层已有 per-call watchdog 兜底，但 `Tool` trait 缺声明式 `timeout()`、`execution_mode()` | 合同（声明字段）+ harness（执行） |
| 注册可逆 | `insert` 覆盖，无 `EffectGuard` 卸载 | harness |
| 单调守卫 / post-execute | `policy_guard` 单槽；无 deny-only 集合、无 post 接缝 | harness（接缝）+ 产品（守卫本体） |

### 6.3 子代理与委派

| 维度 | 差距 | 归属层 |
| --- | --- | --- |
| 收件箱 | steer 是 Session 两字段，非独立队列、无分层 | harness |
| 能力标志 | 路由时不预检，部分到 spawn 才失败/回退 | 合同（Capabilities 扩展）+ harness（预检） |
| max_depth | 常量 2，无 per-request | harness |
| 子代理回报 | 仅 collect 拉取，无主动 report / settled 通知 | harness（通道）+ 产品（接线） |
| fork | 无（可能是刻意隔离） | 暂不做 |
| continuable | 无 | 暂不做 |
| 记忆召回 | 全量注入，无 recall、无引用 | 产品（机制可借 harness 的 seq 引用） |
| 树枚举 | 进程内，非持久 | 暂不做 |

## 7. 三层架构分层方案（2026-08-17 新增）

### 7.1 总体判断

**「R-Code → harness → agent-contracts」三层成立**，判据是每层的「变化原因」不同：

- **agent-contracts（合同层）**：定义「是什么」--类型、事件形状、trait 签名、协议适配。
  变化原因是协议与数据模型演进（低频、需版本化）。
- **harness（运行时层）**：定义「怎么跑」--循环、调度、管线、收件箱、预算、检查点、
  重建自检。变化原因是可靠性策略演进（中频）。**产品无关**：不 import 任何
  `r_code_*`、不含产品枚举身份、事件名中性。
- **R-Code（产品层）**：定义「跑什么」--权限审批判定、Plan、记忆、候选池、外部 runner、
  Tauri 壳、SQLite 投影、前端事件。变化原因是产品需求（高频）。

这个切分与 Rust 移植版的启示吻合：它的单 crate 里 session/tool/inbox/subagent/agent
就是「运行时层」，而 R-Code 的对应物散落在 agent-worker 与宿主中、并与产品编排交织。

### 7.2 harness 子模块可行性

**可行，且应复用 agent-contracts 的既有模式**：

- `vendor/agent-harness`（或 `rust-agent-harness`）子模块，父仓 gitlink 固定 commit；
- 挂入 workspace members + `workspace.dependencies`，与 agent-contracts 同法；
- 自建 CI：fmt / clippy / test / audit / deny + 父仓 submodule-pin 扩展到两个子模块；
- 依赖方向严格单向：`harness → agent-contracts`，`r-code → harness → agent-contracts`，
  任何 `harness → r-code` 或 `agent-contracts → r-code` 都是边界违规（CI 加 grep 守卫可机器化）。

注意：`.agents`（kimi.git）与 `vendor/agent-contracts` 已证明双子模块工作流在本仓成熟；
harness 作为第三个子模块无新增机制负担。

### 7.3 功能切分总表

**harness 仓库应实现（「怎么跑」）：**

| 模块 | 来源 | 下沉阻力 |
| --- | --- | --- |
| 循环核（turn 驱动、ToolUse 分发、流恢复/空闲 watchdog、只读批次并发） | `agent_loop.rs`（3,933 行） | 低：仅 3 个产品类型（`dto::AgentEvent`、`ProductError`、5 处 `AgentActivityPhase`），泛型化即可 |
| 双队列收件箱（`next_turn`/`next_step` + 原子 claim + followup/steer/inject 三语义） | `llm_runtime.rs:1642-1644, 2023-2040, 3247-3267` 抽取 | 低：机制本身自包含，P1-5 的模块化正好是抽取动作 |
| 工具执行管线（pre/post execute、审批 hook 时机、单调守卫接缝、panic 隔离、EffectGuard 注册、timeout/execution_mode 执行、滚动并发 + 顺序提交） | 参考移植版 `tool.rs` + 官方管线；在 r-code-gateway 之上的通用层 | 中：审批**判定**留产品，harness 只定义 `ApprovalGate` trait 与调用时机 |
| 预算护栏（`RunBudgetPolicy` / `RunLoopGuard` / `TripReason`） | `run_guard.rs`（881 行） | 低：唯一耦合是 `trip_reason_to_dto`（`:151-159`），改返回通用枚举 |
| 委派骨架（`DelegationTree`、深度/数量/并发预算、peer mailbox） | `delegation_tree.rs`（625 行） | 低：仅 `dto::{AgentEventScope, SubagentState}` 4 处引用，换通用枚举 |
| 检查点（`GreenCheckpoint` git 回滚点） | `checkpoint.rs`（236 行） | 零：无产品引用 |
| 缓存形状观测（`PrefixShape` 归因） | `cache_shape.rs`（470 行） | 零：仅依赖 `agent_contract::ToolSpec` |
| request/header 重建自检（`assert_request_reconstructable` 等价物） | 新写（P0-1 运行时半），复用 PrefixShape 捕获点 | 中：需定义注入集排除规则 |
| 压缩状态机（P2-G 分层防抖 + canonical 重算骨架，策略可插拔） | `llm_runtime.rs` 内联 `CompactionState` 下沉；与 `agent-compaction` 合并决策见 §5.3 | 中：与产品 run 循环交织，需先抽接口 |
| 事件 seq / 生命周期校验器 | 新写（P1-8 之后） | 中 |

**agent-contracts 需补充的合同（配合 harness）：**

- `SessionEvent::RequestHeader`（+ 可选 `RequestContext`）变体--P0-1 合同半；
- `ToolSpec` 增加 `timeout()` / `execution_mode()` 声明--P1-4 合同半；
- `ApprovalGate` / `MonotonicGuard` trait 签名（harness 消费、产品实现）；
- 修复 §5.2 三处渗漏 + §5.4 硬编码。

**R-Code 产品层保留（「跑什么」）：**

- `AgentRuntime` trait 全家（`runtime.rs` / `mock_runtime.rs` / `recovery.rs`）--
  `CreateSessionInput` / `TaskMode` / `ProjectAccessMode` 是产品语义；
- `llm_runtime.rs` 的产品编排：`ToolGateway` 审批接线（`SessionToolHost` 经
  `execute_with_wait` 挂起）、`PlanView` 合并、`PathGuard` 沙箱、记忆冻结注入、
  `FrozenSubagentCandidatePool` + Codex 外部 runner + 中英文假名池 + 能力钳制；
- 审批判定本体（standing rule / risk ceiling / SQLite permission_requests / 前端 ToolCard）；
- `dto::AgentEvent` → 前端桥、SQLite 投影、replay 三层、Tauri 壳、`r-code-store` /
  `r-code-gateway` / `r-code-mcp` / `r-code-terminal`；
- HMAC 健康回执（`subagent_providers.rs`）--与宿主 pepper 存储强绑定。

### 7.4 三个接缝（下沉的技术路径）

1. **事件枚举泛型化**：agent_loop 输出从 `dto::AgentEvent` 改为 harness 定义的
   `HarnessEvent`（含 phase / usage / tool-call / error 等通用变体）；产品侧
   `impl From<HarnessEvent> for AgentEvent` 做映射。`AgentActivityPhase` 5 处
   （`agent_loop.rs:927,951,988,1018,1172`）与 `AgentEventScope`/`SubagentState`
   （`delegation_tree.rs:4`）同步换通用枚举。
2. **错误类型抽象**：`ProductError` 出现于 agent_loop / runtime 签名；harness 用
   `agent_error::Error`（已存在）或泛型 `E`，产品侧包装。
3. **审批网关 trait 化**：harness 定义 `ApprovalGate`（ask/approve/deny + 一次性
   prompt 语义），产品侧 `ToolGateway` 实现它。这是官方管线「approval 阶段」与
   R-Code 亮点「Mutex 原子审批」的接合点--判定逻辑一点不动，只抽接口。

### 7.5 迁移策略（防大爆炸）

1. **先内后外**：先在本仓建 `crates/r-code-harness`（或直接在 agent-worker 内划清
   模块边界 + 完成三个接缝泛型化），宿主全量接线、测试全绿；
2. **再拆子模块**：把整 crate 目录迁出到新仓库，父仓以子模块挂回（同 agent-contracts
   模式），一次 PR、零行为变化；
3. **配套**：CI 加「harness 不得依赖 r_code」的 grep 守卫；`contract-lock.json`
   机制扩展记录 harness 的公共合同版本。
4. **顺序建议**：接缝泛型化本身就能兑现 P0-2/P0-3/P1-4/P1-5 的部分收益
   （在哪儿做都是这些事），因此「下沉」与「补管线」不必二选一--先补机制
   （在新模块里写），再挪位置。

## 8. 建议清单（按优先级，落点已按三层更新）

### P-1 -- 前置修复（立即）

0. **修复 `session_path` 硬编码**（§5.4）：子模块内改 `format!("{id}.jsonl")`，
   宿主跑全量测试验证写读路径一致；如需数据迁移写归并脚本。
   同时清理 §5.2 三处边界渗漏（`r_code_` 前缀、`RCode` 枚举、硬编码文件名同类残留）。

### P0 -- 高杠杆、低风险，建议先做

1. **request/header 快照 + 派发前重建校验**（最高价值）
   - 落点（分层后）：`agent-contract::SessionEvent` 增 `RequestHeader` variant（合同半）；
     harness 的派发自检器（运行时半，复用 P2-H 的 PrefixShape 捕获点）。
     分层前可先落在 `r-code-agent-worker/src/llm_runtime.rs`，随下沉迁移。
   - 思路：R-Code 的 provider messages 即 session messages，比对消息列表 + system/tools 字节；
     尾部注入（本地时钟 / task_context / plan mode）不落历史，校验须显式排除注入集。
   - 官方细节：快照原因分 `initial / resume / change`，路由容量拆到 `request/context`，
     避免容量变化被误报为信封变化。
   - 收益：一次性提升 replay / fork / compaction / delegation 全链路可信度。
   - 风险：每轮一次快照（可用哈希）；需先定「字节级 or 语义级」判定标准。

2. **工具 panic 隔离**
   - 落点：harness 工具执行管线（分层前在 `r-code-gateway/src/gateway.rs` 的
     `execute_registered_tool`）。
   - 借鉴 `.reference/rust-deepseek-harness/src/tool.rs` 的 `catch_unwind -> TOOL_ERROR`。
   - 只覆盖进程内工具；bash / MCP 已由 `kill_on_drop` / abort 轮询处理。
   - 风险：低。

3. **工具注册可逆化**
   - 落点：harness 管线（分层前 `gateway.rs` 注册处）。
   - `register` 返回 `EffectGuard`（Drop 卸载），同名改为栈；先加 `register_guarded`，
     保留旧接口以控制编译面。
   - 风险：低-中。

### P1 -- 中高收益、中风险，随后做

4. **工具级 timeout / execution_mode / 有序并发**
   - 合同半：`agent-contract::ToolSpec` 加声明字段；运行时半：harness 调度器用
     `Semaphore + FuturesUnordered` 滚动并发，`Exclusive` 作为排序屏障。
   - 保留现有白名单判定逐步迁移，不一次性切换。
   - 现状兜底：`agent_loop.rs` 已有 per-call watchdog 与外部工具 abort 轮询。
   - 关键：落实「并行执行但按调用顺序提交、不改变模型可见顺序」。

5. **收件箱模块化**（与 harness 下沉天然合并）
   - 新建 harness `inbox` 模块：双队列 `next_turn / next_step` + 原子 claim；
     `steer` 保留为 NextStep，父级高水位走 NextTurn。
   - 抽取源：`llm_runtime.rs:1642-1644, 2023-2040, 3247-3267`。
   - 风险：必须保留现有「锁内取队列 + 判断完成」的原子性。

6. **子代理主动回报 + settled 通知**
   - harness 侧：`DelegationTree::send` 加 `report_to_parent` 通道，区分
     「子代理主动写的内容」与「运行时对子代理结局的陈述」（官方刻意区分
     `subagent-report` vs `subagent-settled`）；产品侧：`collect_subagents` 附
     settled summary；settled 不应唤醒父轮次。

7. **记忆两层注入 + 召回**（产品层）
   - `crates/r-code-store/src/memory_store.rs` 加 `recall(query, limit)`；
     `render_snapshot` 改「索引（≤8 条）+ 命中全文（≤3 条）」两层，每条带 `[id=… cite=…]`。
   - 借鉴 Rust 移植版的 CJK 分词（词 + 二元组）+ cite_seq 可审计召回。
   - 保留全量注入开关，避免破坏现有 `record_injection` 审计。

8. **JSONL 显式 seq**
   - 行号即隐式 seq -> 改显式字段 + 连续性校验（harness 校验器）；老文件按行号回填。
   - 先加 seq 再评估 turn/step 生命周期（后者改造面大）。

9. **harness 接缝泛型化**（§7.4 三接缝，为子模块化铺路）
   - 可与 5 合并做：抽 inbox 时顺带把 `HarnessEvent` / 通用错误立起来。

### P2 -- 渐进收敛，按需做

10. **fork 边界语义**：只带 balanced prefix（到最近 turn/end），记录 parent/seed 链；
    对齐官方 `session/end-seed` 边界事件与「前缀结束于未闭合 turn 即拒绝，不静默裁剪」。
11. **replay 坏行显式标注**：`Missing` 而非静默跳过。
12. **能力标志前移校验**：路由后加 `validate_requested_capabilities`（harness 预检）。
13. **压缩事件化 + 双套压缩合并**：投影安装/替换写成 JSONL 事件（如
    `ProjectionInstalled`），replay 对 `ModelProjection` 从「静默忽略」改为「标注改写区间」；
    对齐官方 `compaction/*` + surfaceOp 的可回放压缩；同步决策 P2-G 状态机与
    `agent-compaction` 的合并（§5.3），避免两套并存。
14. **harness 拆出为子模块**（§7.5 第 2 步）：接缝稳定后整目录迁移，零行为变化。

## 9. 明确不做（及理由）

| 建议 | 理由 |
| --- | --- |
| 完整 Cordis 式插件生态 / PluginManager | R-Code 已有 LLM / ToolHost / SubagentProvider 等多 provider seam；缺的是 Cordis 式可逆时序组合，当前替换需求不足以支撑其工程量 |
| 完整 turn/step 状态机 + `repair_interrupted` 合成 | 已用消息层 `repair_dangling_tool_uses` 覆盖崩溃场景，除非要 step 级精确重放 |
| continuable child + Activation 状态机 | 工程量最大，且与 R-Code「run = task」模型冲突 |
| 「官方无 steering，应移除 steer」 | 前提不成立：官方核心 `Agent` 就有 `followup` / `steer` / `inject` 三别名；官方只是对已接纳的 continuable 子代理不再提供 steering。R-Code 的 steer 是特性，不是缺陷 |
| 通用多 provider 注册表泛化 | 除非 ACP / Claude Code 成为一等公民 |
| 照搬 `InboxSpliced` 事件 | SQLite `queued_messages` + `DURABLE_USER_MESSAGE` 已实现等效 durable steer |
| 把 harness 做成独立网络服务 / 进程外运行时 | 三层解耦的目标是复用与边界纪律，不是部署拓扑；`agent-ipc` 已在合同层提供进程外选项，需要时再启用 |
| 一次性把 agent-worker 全量下沉 | llm_runtime 约 9,000 行是产品编排（审批/Plan/记忆/候选池/外部 runner），强下沉会把产品逻辑漏进公共层，违反 §7.1 判据 |

## 10. 实施路线图

建议分四步，不要一次性全做：

1. **第零步（立即）**：P-1 前置修复--`session_path` 硬编码 + 边界渗漏清理。
   这是数据正确性问题，优先于一切架构动作。
2. **第一步**：P0 三项--request/header 重建自检、工具 panic 隔离、工具注册可逆化。
   风险低、收益高、互相独立；前两项直接落在未来 harness 的模块位上。
3. **第二步**：P1 中与近期工作直接相关的--收件箱分层 + 子代理主动回报
   （延续子代理工作）、记忆召回（若记忆是近期重点）；同时做接缝泛型化（P1-9），
   让「通用件」与「产品件」在代码边界上先分开。
4. **第三步**：会话侧 seq / fork / replay 的渐进收敛（P2），等前面稳定后再做；
   最后执行 harness 整目录拆出为子模块（P2-14，零行为变化迁移）。

最核心的一条：两个 harness 真正有价值的是下面这条尚未在 R-Code 立起来的硬规矩--

> **模型看到的一切，必须能从一条 append-only 事件日志重建，并在派发前自检。**

补上它，现有的 replay / 压缩 / fork / 委派都会从「事后补录」升级为「结构性可信」；
而三层分层（R-Code → harness → agent-contracts）是让这条规矩有唯一归属地的
结构保障--它属于 harness。

## 11. 参考来源

- 官方仓库：<https://github.com/deepseek-ai/deepseek-harness>
- 官方文档：`docs/architecture.md`、`docs/subsystems/core.md`、`docs/subsystems/session.md`、
  `docs/subsystems/subagent.md`、`docs/tool-execution-pipeline.md`
- 本地 Rust 移植版：`.reference/rust-deepseek-harness/`（`README.md`、`docs/architecture.md`、
  `src/session.rs`、`src/persistence.rs`、`src/tool.rs`、`src/subagent.rs`、`src/inbox.rs`、
  `src/agent.rs`、`src/memory.rs`、`src/prompt.rs`）
- agent-contracts 子模块：`vendor/agent-contracts/`（`README.md`、`contract-lock.json`、
  `docs/00–15`、`crates/agent-contract/src/session.rs`、`crates/agent-store/src/session_store.rs`、
  `crates/agent-config/src/lib.rs`）
- R-Code 本仓库：`docs/architecture.md`、`docs/archive/deepseek-prefix-cache.md`、
  `crates/r-code-agent-worker/src/{llm_runtime,agent_loop,run_guard,delegation_tree,cache_shape,checkpoint}.rs`、
  `crates/r-code-agent-worker/Cargo.toml`（死依赖 agent-store/agent-compaction）、
  `crates/r-code-gateway/src/gateway.rs`、`crates/r-code-store/`、
  `src-tauri/src/replay.rs`、`src-tauri/src/subagent_providers.rs`、
  `src-tauri/src/commands.rs`（`session_file_path`:4844、`ensure_session_log`:4864）、
  `.github/workflows/ci.yml`（submodule-pin:205）、根 `Cargo.toml`（workspace members:5-13）
