# DeepSeek Provider 前缀缓存与响应速度优化 PRD

> 归档状态：方案已实施，本文件只保留历史设计、验收与已知例外，不是当前待办。
> 状态：**已实施（2026-08-07，分支 `feat/deepseek-prefix-cache`；两处已记录例外：P1-F 的 missing-reasoning 回放因公共层（agent-llm）无 reasoning 事件流未实施、P2-G 命中率恢复场景测试未建，见 §5 对应注释。Review 收尾已补齐：§8 两项缓解（双套 usage 字段解析、重试计数展示）与 P2-H 运行时归因接线）**
> 范围：DeepSeek Provider 的请求字节稳定性、缓存观测与响应路径优化；主路径为 Chat Completions，并覆盖用户手动切换到 Responses、Anthropic Messages 兼容口及 DeepSeek 自定义网关时的协议适配。
> 目标读者：维护者、评审者、实施者。
> 参照实现：Reasonix（`esengine/DeepSeek-Reasonix`，MIT）——围绕 DeepSeek 字节级自动前缀缓存专门设计的 agent。

---

## 1. 背景与动机

### 1.1 用户痛点

- R-Code 的 DeepSeek 线路（`DeepSeekProvider → OpenAiProvider`，走 `/chat/completions`）在长会话中**缓存命中率趋近于 0**：输入 token 全量按 miss 计费，且随历史增长 prefill 越来越慢（首 token 延迟 TTFT 线性上升）。
- 对照 Reasonix：长会话命中率 90%+（守卫阈值）、输入 token 成本降至约 1/5（营销宣称，见 §2 出处说明）。

### 1.2 根因（一句话）

DeepSeek 的 prefix cache 是**字节级自动**的：两次请求的公共前缀字节完全一致即命中，无需任何 API 开关。R-Code 缓存全 miss 不是 API 不支持，而是**客户端每次请求都改变了前缀字节**（system 含秒级时间戳等，见 §3）。

### 1.3 本 PRD 的目标

在**不改变产品行为**（时间感知、任务上下文、记忆注入、委派提示等功能照旧）的前提下，让 DeepSeek 请求前缀字节逐轮稳定，建立缓存观测手段，并落地长会话压缩与重试方案。**本 PRD 只交付设计与验收标准，不实施。**

---

## 2. 术语与原理

| 术语 | 含义 |
| --- | --- |
| prefix cache | DeepSeek 服务端按请求前缀（从第一个字节到首个差异点）自动缓存 KV；命中部分按 `prompt_cache_hit_tokens` 低价计费，且免去 prefill |
| 字节稳定（byte-stable） | 同一会话相邻两轮请求，公共前缀逐字节相同 |
| 前缀形状（PrefixShape） | 请求可缓存前缀的指纹：system 哈希、tools 哈希、历史改写版本号 |
| append-only | 历史只追加、永不改写已发送的消息 |
| 缓存重置点 | 允许改写前缀字节的事件（用户切换模型/工作区、压缩等），会拉低下一轮命中率 |
| TTFT | Time To First Token，首 token 延迟；输入 token 命中缓存时服务端免 prefill，TTFT 大幅缩短 |

**核心原理（出处与限定）**：DeepSeek `/chat/completions` 的自动缓存对 `system + messages + tools` 的整体字节前缀敏感，命中率不依赖任何客户端配置，只依赖字节一致性。证据链：
- Reasonix 的 `internal/provider/openai/realcache_test.go`（`//go:build live` 探针）用构造的大块稳定文本重复请求实测前缀命中；其 mock（`cachehit_e2e_test.go:59-74`）按"与前一次请求逐字节相同的消息前缀"推导 hit tokens。
- "90%+ 命中率"是 Reasonix **release 门禁守卫**（`cachehit_e2e_test.go:378-477`，默认阈值 90、非严格模式仅警告）与营销文案（`docs/index.html:160-161`）中的宣称；"~1/5 成本"对应 DeepSeek cache-hit/miss 定价比。**r-code 的真实命中率以本 PRD §6 的 P0-B 基线实测为准。**
- DeepSeek 无 `cache_control` 类开关（Reasonix `internal/provider/anthropic/anthropic_test.go:651` 明言 "DeepSeek ignores cache_control; system/tools must omit it"）。

### 2.1 手动切换协议时的缓存契约（2026-08-09 补齐）

| 线路 | Provider 身份 | 缓存开关 | usage 观测 |
| --- | --- | --- | --- |
| Chat Completions | `DeepSeekProvider` | 服务端自动缓存；自定义网关也请求 `stream_options.include_usage`，不支持时移除该字段兼容重试 | `prompt_cache_hit_tokens` / `prompt_cache_miss_tokens` |
| Responses | `DeepSeekResponses` | 服务端自动缓存；`store: false`，每轮稳定重放完整历史 | `input_tokens_details.cached_tokens`；miss 按 `input_tokens - cached_tokens` 推导，若服务端给出 DeepSeek 显式 hit/miss 字段则优先使用 |
| Anthropic Messages | `DeepSeekAnthropic` | 服务端自动缓存；即使 `enable_caching=true` 也不注入 Anthropic `cache_control` | 读取 `cache_read_input_tokens` / `cache_creation_input_tokens`；按 Anthropic 口的排除式 `input_tokens` 归一成总输入及 hit/miss，含流式 `message_start` |

三条线路都声明 DeepSeek V4 的 1M context window，并保持普通 OpenAI Responses、官方 Anthropic 与其它兼容 provider 的既有行为。DeepSeek 身份由 `deepseek`/`deepseek_*` provider id 或精确官方 host 保留；不会用模型名猜测，也不会把 `api.deepseek.com.example` 一类相似域名误判为官方服务。

---

## 3. 现状盘点（r-code 实测，含对应实施项）

DeepSeek 线路架构：`llm_runtime.rs`（请求构建/轮次编排）→ `agent-llm` 的 `OpenAiProvider`（序列化与发送，`vendor/agent-core/crates/agent-llm/src/`）。

| # | 现状 | 位置 | 对缓存/速度的影响 | 实施项 |
| --- | --- | --- | --- | --- |
| A1 | system prompt 含**秒级时间戳** `%Y-%m-%dT%H:%M:%S%:z`，位于 system **中部**（base + `NETWORK_TOOL_POLICY` 之后），每轮重建 | `llm_runtime.rs:224-240`、`1200-1219` | 🔴 **头号杀手**：时间戳之后所有静态文本与全部历史前缀每轮必 miss | P0-A |
| A2 | `task_context` 每次发送前由宿主刷新并注入 system 内部 | `llm_runtime.rs:278-296`（注入点 `1203`，取值 `1147-1164`）；宿主刷新 `src-tauri/src/commands.rs:1661-1679` | 🟠 内容变化即切断前缀 | P0-A |
| A3 | `memory_context` 注入 system（每 run 冻结） | `llm_runtime.rs:261-276`；冻结点 `829`、`commands.rs:8457` | 🟡 run 间变化切断跨 run 前缀 | P0-A |
| A4 | tools 顺序来自 `HashMap::values()` 迭代，进程内稳定、**重启后随机** | `gateway.rs:479, 531-542` | 🟡 重启后首个请求必 miss | P1-C |
| A5 | `repair_dangling_tool_uses` 向历史**插入合成消息**（仅异常路径，但永久改变该会话前缀） | `agent_loop.rs:162-235, 330` | 🟡 异常恢复后前缀改写 | P1-D |
| A6 | usage 解析把缓存字段**硬编码丢弃**（`cache_read_tokens: None, cache_write_tokens: None`），`prompt_cache_hit_tokens`/`prompt_cache_miss_tokens` 未读取 | `openai.rs:773-785`（字段本身是 `Option<u32>`，`agent-contract/src/usage.rs:13,15`） | 🔵 无观测手段 | P0-B |
| A7 | **agent-worker 原生 LlmAgentRuntime 线路的 usage 不持久化**（仅 `tracing::debug!`）；注意 Codex CLI 线路**已实时持久化**（`commands.rs:12885-12891, 15160-15162, 15371-15373`，写库 API `repositories.rs:854-858` 已存在） | `agent_loop.rs:562-567`、`dto.rs:366` | 🔵 原生线路无法量化收益 | P0-B |
| A8 | `supports_prompt_caching = false`（仅能力声明，不影响字节级自动缓存） | `deepseek.rs:60`、`openai.rs:225` | 🔵 信息错误，需修正 | P0-B |
| A9 | 无重试：连接/流式失败即终止 run（`gateway.rs` 的工具执行重试与 provider 请求无关） | `agent_loop.rs:345-405` | 🔵 网络抖动直接失败，无"冻结请求重放" | P1-E |
| A10 | 无历史压缩：`agent-compaction` 未接入生产代码（`crates/r-code-agent-worker/Cargo.toml:17` **声明了但生产代码未使用**；`crates/r-code-core/Cargo.toml:46` 为 dev-dependencies，仅 `tests/contract_tests.rs:9,600-633` 引用） | — | 🔵 64k 模型会超窗；1M 模型成本无界 | P2-G |
| A11 | 运行期注入的动态消息（steer、continuation、子代理收集、findings）均 append 到历史**尾部** | `llm_runtime.rs:1154-1159, 1281-1283, 1324-1329, 1382-1387` | ✅ 位置正确，保持"只追加"即可，**不纳入改动** | — |
| A12 | serde_json 未启用 `preserve_order`（键字典序，字节稳定）；全流式实时转发；工具并发执行；子代理 system 无时间戳 | `Cargo.toml:42`、`agent_loop.rs:410-419, 525` | ✅ 有利基础 | — |
| A13 | system 中段还有三类**按轮动态内容**：①委派提示（`delegation_allowed` 由 `delegation_directive(&messages)` 基于最新用户消息每轮重算，`llm_runtime.rs:1182, 1204-1219`；`codex_available()` 于 `1207`）；②plan mode policy（`session.mode` run 中可变，`1204` + `commands.rs:1679`）；③workspace attach/detach（base 在 `WORKSPACE_SYSTEM_PROMPT`/`CHAT_SYSTEM_PROMPT` 间切换，`148,154`，连带 tools 过滤 `1744-1748`） | `llm_runtime.rs:1204-1219` | 🟠 用户行为变化即改 system 中段（属**合法重置点**，但需显式表达） | P0-A/P1-D |
| A14 | **DeepSeek 流式请求不带 `stream_options.include_usage`**：`supports_stream_usage()` 仅认 `api.openai.com`（`openai.rs:44-52`），`build_body` 仅在 true 时发 include_usage（`90-95`）；DeepSeek 默认 base_url 是 `https://api.deepseek.com`（`deepseek.rs:9`）→ 真实流式会话**收不到 usage 帧**，A6 的解析路径实际不触发 | `openai.rs:44-52, 90-95` | 🔴 **P0-B 验收的前置阻塞** | P0-B |
| A15 | tools 数组**内容级漂移**：`delegation_tool_specs` 的 `delegate_task` description/enum 随 `codex_available()` 变化（`llm_runtime.rs:2198-2228`）；hosted_tools 切换整体改变 tools 前缀（`openai.rs:96-105, 112-138`） | `llm_runtime.rs:2198-2228` | 🟡 可用性变化改 tools 哈希（属合理重置点，需记录） | P1-C |

> 已确认非破坏点：`run_id`/子代理 `run_id`（`llm_runtime.rs:799, 2705` 的 `Uuid::new_v4()`）只进 supervisor/session/事件，不进请求体，无字节影响。
> 说明：A 项与实施项的对应关系如上表；P2-H（缓存归因与守卫）是新增工程保障，无对应 A 项（非现状修复）。

---

## 4. 设计原则（对齐 Reasonix）

1. **system prompt 是不可变常量**。任何每轮/每次变化的内容（时间、语言偏好、hook 输出、动态指令）一律作为**独立 user 消息追加到尾部**，永不写入 system 中段。
2. **历史严格 append-only**。失败重试不写 session；对上下文的改写只作用于发送前的临时副本；异常修复（A5）改为"线路侧一次性修复 + 落盘固化"，不逐轮重写。
3. **请求字节确定性**。工具列表按名称排序输出；JSON 键存在性与顺序固定——包括 DeepSeek 兼容必需的 `reasoning_content` 键（thinking 模式 assistant tool_calls 轮缺失直接 400）与 tool 消息 `name` 键（Reasonix `openai.go:688-735`）。
4. **归一化走零拷贝快路径**。健康历史直接透传原对象，修复动作幂等且一次固化。
5. **压缩是唯一且罕见的缓存重置点**。分层阈值（提示 → 剪旧工具结果 → 摘要折叠），折叠只动中间段，保留小 user 轮次（有上限）与尾部 verbatim；配防抖守卫。
6. **多会话隔离**。planner/executor/子代理各自独立历史，互不污染前缀（R-Code 若引入多会话模型，遵循同一原则）。
7. **可观测优先**。先落地 usage 解析与命中率上报，再谈优化——没有数字的优化不可验收。
8. **重试复用冻结请求**。重试字节与首试逐字节一致，不因重试制造新前缀。

---

## 5. 实施计划（分阶段，按性价比排序）

> 每项给出：目标 / 方案 / 涉及文件 / 改动规模 / 验收标准（均为可执行验证）。改动规模为量级估计（文件数）。

### P0-A：system prompt 去时间戳 + 动态内容下沉

**目标**：让 system 前缀在同 run 内及跨 run 稳定，消除头号 miss 源。

**方案**：
1. 从 system 中段删除 `Current local time: ...`（`llm_runtime.rs:224-236`）。**时间戳不再进入 system 中段**——这是 r-code 自己的设计决策（保留时间感知），不挂靠 Reasonix（Reasonix 是"时间完全不进模型输入"：其 `CreatedAt` 清零是删掉 UI 元数据字段，`internal/agent/agent.go:2887-2893` 注释 "durable UI metadata, not model input"）。
2. 时间信息承载方式**唯一方案**：作为**每轮尾部 user 消息**注入（`%A` 星期几保留，粒度放宽为分钟级；跨分钟边界只影响尾部追加，不伤已发送前缀）。不采用"独立 system 消息"或"写入会话首条 user 消息"——前者在消息中段引入变化点，后者首轮之后无法更新。
3. `task_context`（A2）从 system 内部移出，作为**每轮尾部 user 消息**注入（Reasonix：动态上下文一律走 user-turn 后缀，`docs/SPEC.md:296-300`）。
4. `memory_context`（A3）保持 run 冻结，从 system 拼接中拆为**独立 system 消息**（内容变化不波及主 system 前缀；**跨 run 的 memory 变化仍是合法缓存重置点**，记入 PrefixShape 归因，见 P2-H）。
5. 委派提示、plan mode、workspace 三类按轮动态内容（A13）**维持其语义**，但承载方式改为尾部注入或显式重置：其中委派提示与 plan mode 走尾部 user 消息（`delegation_directive` 每轮重算结果注入尾部）；workspace attach/detach 是用户可见行为，作为**合法缓存重置点**（切换时完整重建 system + tools，记为 PrefixShape 变化原因）。

**涉及文件**：`crates/r-code-agent-worker/src/llm_runtime.rs`（224-240、253-326、1176-1219）、`src-tauri/src/commands.rs`（1661-1679 宿主刷新点）。

**改动规模**：2 个文件。

**验收标准**：
- [x] 单测：同一 run 内连续工具回合，system 字节完全一致（两次构建序列化相等）；时间戳**不出现在** system 中段（重写 `llm_runtime.rs:5637` 的 `system_prompt_includes_fixed_local_clock`——**同步改名**如 `system_prompt_excludes_local_clock`，断言目标从"包含时间戳"改为"不包含"）。
- [x] 时间问答回归：`今天星期几` 类集成测试正确（含**跨分钟边界**用例：尾部时间戳消息变化但历史前缀不变）。
- [x] 相邻两轮请求的 messages 前缀（system + 已发送历史）逐字节相同，**允许尾部追加新的时间戳/任务上下文消息**；该断言限定在**同一 run 内**（跨 run 且 memory 变化时属合法重置点，不要求前缀相同）。
- [x] 委派语义回归：`llm_runtime.rs:4270, 4324, 4401` 三条测试的断言目标迁移到尾部注入后仍成立（`delegation_directive` 按轮生效）。

### P0-B：usage 缓存字段解析与上报（含 include_usage 前置）

**目标**：让命中率可见、可量化，为所有后续优化提供验收基线。

**方案**：
1. **前置（A14）**：为 DeepSeek 流式请求启用 `stream_options.include_usage`——`supports_stream_usage()`（`openai.rs:44-52`）按 provider/base_url 判断（DeepSeek 端点返回 true，其余 OpenAI 兼容口维持现状，避免破坏 `openai.rs:965` `stream_usage_is_requested_only_for_official_openai_endpoint` 测试）。
2. `openai.rs:773-785`：解析 `prompt_cache_hit_tokens` → `cache_read_tokens`、`prompt_cache_miss_tokens` → `cache_write_tokens`。**语义**：缺省时置 `Some(0)`（字段是 `Option<u32>`，`usage.rs:13,15`；`AddAssign` 对 None 有专门合并逻辑 `38-44`，需区分"服务端未返回"与"命中 0"）。
3. 持久化与展示：agent-worker 原生线路补写 `usage_json`（`dto.rs:366` 列已存在；复用 Codex 线路的 `set_usage` 写库 API `repositories.rs:854-858`）；UI 展示会话累计命中率——扩展 `Timeline.tsx:107-120` 的 `runUsageLabel` 解析 `cache_read_tokens/cache_write_tokens`（前端文件位置以实施时为准）。
4. `deepseek.rs:60` / `openai.rs:225`：`supports_prompt_caching` 置 true（DeepSeek 自动缓存确实存在；注意 `openai.rs:225` 影响**所有** OpenAI 兼容 provider 的能力声明，需确认 UI 只把它用于提示展示，生产代码不消费 `enable_caching`——已确认 `openai.rs` 不消费，风险见 §8）。

**涉及文件**：`vendor/agent-core/crates/agent-llm/src/openai.rs`（44-52、90-95、773-785）、`vendor/agent-core/crates/agent-llm/src/deepseek.rs`（60）；`crates/r-code-agent-worker/src/agent_loop.rs`（562-567）、`crates/r-code-core/src/dto.rs`（366）、`crates/r-code-store/src/repositories.rs`（854-858 复用）、`src-tauri/src/commands.rs`、前端 `Timeline.tsx`。

**改动规模**：5-6 个文件（含 2 个 vendor 文件）。

**验收标准**：
- [x] 单测：构造含 `prompt_cache_hit_tokens` 的 SSE usage 帧，断言解析为 `cache_read_tokens = Some(n)`；缺省帧断言 `Some(0)`。
- [x] 单测：DeepSeek base_url 下 `build_body` 含 `stream_options.include_usage`；openrouter 等兼容端点**不含**（`openai.rs:965` 测试保持绿色，另新增 deepseek 用例）。
- [x] `deepseek.rs:75` `name_and_capabilities` 测试更新为 `supports_prompt_caching == true`。
- [x] 真实 API 集成测试（`#[ignore]`，需 key）：连续 5 轮工具会话，日志能读到 `cache_read_tokens > 0`（探针参考 Reasonix `realcache_test.go`：前缀需明显超过 64-token 缓存块粒度）。
- [x] 既有 run 数据兼容：新增 `usage_json` 写入后，历史 run（无该字段）读取不报错（migration 兼容测试）。**注意：不存在"回填历史数据"的需求**——原生线路此前从未写 usage_json。

### P1-C：tools 按名称排序 + 内容稳定性

**目标**：跨进程重启后工具列表字节稳定；内容级漂移可归因。

**方案**：
1. 排序点：`gateway.rs:531-542` `tool_specs()` 收集后按 `name` 排序；同时明确**最终请求体**的排序点——`llm_runtime.rs:1739-1764` `SessionToolHost::tool_specs`（gateway 段 + external 段 + delegation 段）或 `openai.rs:96` 组装处整体排序（Reasonix：`internal/tool/tool.go:522-544` `sort.Strings(names)`）。
2. 内容稳定性（A15）：同一 run 内冻结 `codex_available()` 判定结果（委派工具 description/enum 不随轮变化）；`delegation_tool_specs` 变化作为合法重置点记录（PrefixShape 归因，见 P2-H）。

**涉及文件**：`crates/r-code-gateway/src/gateway.rs`、`crates/r-code-agent-worker/src/llm_runtime.rs`（1739-1764、2198-2228）。

**改动规模**：2 个文件。

**验收标准**：
- [x] 单测：注册顺序打乱的两组注册，`tool_specs()` 输出顺序一致；最终请求体 tools 顺序跨轮一致。
- [x] 现有工具行为测试全绿（`gateway.rs:1738/1761`、`llm_runtime.rs:5197/5242/5273` 均不依赖顺序，已核实）。

### P1-D：run 内 system 冻结复用 + 修复固化（含 A5）

**目标**：避免每轮重复构建引入漂移；异常修复不再逐轮重写历史。

**方案**：
1. run loop 每轮不再调用 `build_main_system_prompt`（`llm_runtime.rs:1200-1219`），改为 run 开始时构建一次、冻结缓存，后续轮复用同一字符串（Reasonix：system 只在会话创建时写入、运行期零追加——拼接函数 `agent.go:3268-3280` + 机制 `session.go:61-72`）。
2. 动态语义（委派提示、plan mode）经 P0-A 的尾部注入通道按轮生效（`codex_available()`、`delegation_directive` 仍每轮计算，但结果注入尾部而非 system）。
3. **A5 修复固化**：`repair_dangling_tool_uses`（`agent_loop.rs:162-235`）改为——修复动作在发送前一次性完成（线路侧），修复结果**落盘固化到 session**，后续轮直接透传健康历史（Reasonix 归一化快路径：健康历史零拷贝 passthrough，`internal/provider/provider.go:318-455`）。

**涉及文件**：`crates/r-code-agent-worker/src/llm_runtime.rs`、`crates/r-code-agent-worker/src/agent_loop.rs`（162-235、330）。

**改动规模**：2 个文件。

**验收标准**：
- [x] 单测：同 run 多轮迭代中 system 字符串字节相同（同一实例复用）。
- [x] 委派回归：`llm_runtime.rs:4324`（codex 委派暴露）、`4401`（opt-out 隐藏）在尾部注入语义下仍成立——`explicit_opt_out_hides_delegation_tools...` 同时是 `delegation_directive` 按轮生效的行为契约。
- [x] repair 固化：模拟悬挂 ToolUse 的会话，首次修复后 session 落盘；后续请求透传修复后历史（字节断言），不再重复插入。

### P1-E：流式中断重试 + 流空闲 watchdog

**目标**：网络抖动不丢缓存、不重发历史、不终止 run；SSE 挂死不无限等待。

**方案**：
1. 发送前 deep-copy 冻结请求（消息/工具/温度；Reasonix `freezeProviderRequest`，`internal/agent/agent.go:2918-2958`）。
2. 分层重试：连接层指数退避（Reasonix 连接层 `MaxRetries=10`、`maxRetryAfter=60s` 定义于 `retry.go:18-28`，退避公式 `221-228`、Retry-After 应用 `235, 290`）；body 层流恢复（Reasonix `maxStreamRecoveries=5` + 首试共 6 次、退避 0.5/1/2/4/8s，`internal/agent/agent.go:45-48`、`run_loop.go:537-556`）。重试一律复用冻结请求；**失败不写 session**（Reasonix `run_loop.go:363-365` "Failed attempts never write Session state"）。
3. 流空闲 watchdog：SSE 无数据超过阈值（Reasonix `defaultStreamIdleTimeout = 120s`，`openai.go:43-50`）视为流死，关闭连接走恢复路径——与连接超时互补（区分"连接断了"与"流死了"）。
4. 重试边界：仅 5xx/连接类/流中断可重试；4xx/AuthFailed 不重试（`agent_loop.rs:1547` `provider_error_propagates` 测试契约）。

**涉及文件**：`vendor/agent-core/crates/agent-llm/src/openai.rs`、`crates/r-code-agent-worker/src/agent_loop.rs`（345-405、1547）。

**改动规模**：2 个文件。

**验收标准**：
- [x] mock 服务器集成测试（**需新建**，当前 mock 均为进程内 `MockProvider`）：首请求 5xx 后重试成功，两次请求体字节一致（断言相等）；session 历史在重试期间不变。
- [x] 4xx 不重试（单测）。**注**：实现对齐 Reasonix `RetryableStatus`，将 408/429 纳入可重试（`openai.rs` `retryable_status`）；400/401/402/422 与 AuthFailed 不重试。
- [x] abort 语义回归：`agent_loop.rs:831`（连接期 abort 立即返回）、`872`（强制 abort 工具调用）保持绿色。
- [x] SSE 停流超时后快速失败并走恢复路径（mock 停流用例）。

### P1-F：DeepSeek 兼容性硬化（400 键恒发 + missing-reasoning 回放）

**目标**：thinking 模式（deepseek-reasoner）下请求可用且字节稳定；工具轮 reasoning 缺失自动恢复。

**方案**：
1. **键恒发**（Reasonix `openai.go:688-735`）：thinking 模式对 assistant tool_calls 轮**恒发 `reasoning_content` 键**（空串可接受，缺键 DeepSeek 400 "must be passed back"，724-730）；tool 消息恒发 `name` 键（719-723）；发送前修复悬空工具调用对（`SanitizeToolPairing`，689-692）。对应 r-code 的 `openai.rs` 序列化层。
2. **missing-reasoning 精确回放**（Reasonix `run_loop.go:444-493`）：工具轮 reasoning 缺失时用**同一冻结请求**精确重放一次（不造合成 prompt），带审计事件——与 P1-E 同一冻结基础设施。

**涉及文件**：`vendor/agent-core/crates/agent-llm/src/openai.rs`、`crates/r-code-agent-worker/src/agent_loop.rs`。

**改动规模**：2 个文件。

**验收标准**：
- [x] 单测：thinking 模式序列化后 assistant 消息含 `reasoning_content` 键（值为空串也可）；tool 消息含 `name` 键。
- [ ] 单测：缺 reasoning 的响应触发一次冻结请求重放，session 无新增消息。**（未实施：公共层（agent-llm）无 reasoning 事件流，响应侧无法观测 reasoning 缺失；键恒发已消除 DeepSeek 400 的主要触发面。若未来公共层暴露 reasoning 事件，按本节方案补回放。）**

### P2-G：接入分层压缩（长会话）

**目标**：长会话不超窗、成本有界，压缩尽可能少打穿缓存。

**方案**：接入 `agent-compaction`（worker 的 `Cargo.toml:17` 已声明依赖），对齐 Reasonix 分层（`internal/agent/compact.go`）：
1. 阈值（相对 provider context window，默认 0.5/0.6/0.8，`config.go:1846-1848`）：50% 仅提示一次、60% 剪旧工具结果（保留头尾 + 引用，`prune.go`）、80% 摘要折叠。
2. 折叠边界（`compact.go:217-342`）：保留 (a) system（b) 小 user 轮次 verbatim——**有上限**：`maxPinnedFirstUserTokens=1500` 且不超过窗口 15%（`compact.go:35-36`）(c) 旧摘要（不重复折叠）(d) 尾部预算 `defaultTailTokens=16384`（`compact.go:31`；64k 模型约占 25%，需按模型窗口调整）。压缩前**归档原始消息**（`archiveMessages`）；摘要失败时用确定性机械折叠兜底（不循环、不丢 verbatim 小轮次，`compact.go:293-315`）。
3. **防抖守卫**：连续 2 次压缩即暂停自动压缩并提示"窗口太小"（压缩后仍超阈值才会触发第二次，`compact.go:136-166`）；context window < 16384 显示非阻断警告（Reasonix `docs/GUIDE.md:441-442`）。
4. **token 估算校准**：用上一轮真实 usage 反推 tokens/字符比（0.05~2 范围过滤，reasoning 不计入，`compact.go:674-688`）。
5. 压缩改写通过 `RewriteVersion` 标记（与 P2-H 归因联动）。

**涉及文件**：`crates/r-code-agent-worker`（新增压缩接入点）、`crates/r-code-core`（若需配置项）。**依赖 P0-A + P0-B**（命中率观测是验收前提）。

**改动规模**：3-4 个文件。

**验收标准**：
- [ ] 模拟窗口测试：压缩后尾部连续 5 轮命中率恢复 ≥85%（对齐 Reasonix `cachehit_e2e_test.go:231-276`，tail=5 断言 ≥85%）。**（未实施：该场景需贯通 run 循环压缩路径 + 字节前缀 mock 的重型 harness，当前守卫路径不含压缩轮；分层阈值/防抖/机械折叠兜底已由 `llm_runtime.rs` compaction_tests 13 个用例覆盖。列为后续测试债。）**
- [x] 防抖：窗口过小场景连续压缩 ≤2 次后暂停（`cachehit_e2e_test.go:231-276` 同场景）。
- [x] 压缩后记忆/任务上下文语义保持（回归用例）；`contract_tests.rs:600-633` 压缩契约测试保持绿色（仅接入不修改 vendor 时）。

### P2-H：缓存归因与守卫测试

**目标**：防回归的工程保障。

**方案**：
1. **PrefixShape**（system 哈希 + tools 哈希 + 改写版本号）每轮请求前后捕获比对，归因缓存变化原因（system/tools/compact/repair/workspace/委派开关；Reasonix `internal/agent/cache_shape.go`）。**归因规则**：仅"真正改写 provider 可见字节"的操作上报缓存变化；纯本地元数据（决策回执、preview 替换）bump 版本号但**不算 miss**（`cache_shape.go:66-73`）。
2. **守卫测试**（对齐 Reasonix `TestReleaseCacheHitGuard`，`cachehit_e2e_test.go:378-477`）：多轮工具循环场景 tail_avg（末 3 轮）命中率 ≥90%。**注意 Reasonix 的定位是 release 门禁**：默认 skip、env 启用（`REASONIX_RELEASE_CACHE_GUARD=1`）、非严格模式只警告不失败（`REASONIX_CACHE_GUARD_STRICT=1` 才 Fatal）——r-code 照搬此定位，避免"CI 里根本不跑"的落差。**压缩轮不计入 tail_avg**（P2-G 造成的预期命中率下降不算回归）。
3. mock 按字节前缀模拟命中（对齐 Reasonix `cachehit_e2e_test.go:59-74` 的 mockDeepSeek），守卫测试在 P0/P1 完成后全绿；故意注入时间戳漂移时测试变红（验证守卫有效性）。

**涉及文件**：`crates/r-code-agent-worker/src/llm_runtime.rs`（新增 cache_shape 模块）、测试目录。

**改动规模**：2-3 个文件。

**验收标准**：
- [x] 守卫测试在 P0/P1 完成后全绿；注入漂移变红（有效性验证）。
- [x] 归因规则单测：纯本地元数据编辑不上报缓存变化（对齐 Reasonix `cache_shape_test.go:39-53`）。（另：运行时归因已接入 run 循环——每轮请求前 capture/compare，前缀变化时 tracing 记录 cause。）

### 依赖与批次（实施顺序）

| 项 | 前置 | 可并行 | 批次建议 |
| --- | --- | --- | --- |
| P0-A | — | P0-B | 批次 1（核心收益） |
| P0-B | —（含 A14 前置） | P0-A | 批次 1 |
| P1-C | — | 无 | 批次 2 |
| P1-D | P0-A | 无 | 批次 2 |
| P1-E | —（建议 P0-B 后，便于观测重试效果） | 无 | 批次 2 |
| P1-F | P1-E（共享冻结基础设施） | 无 | 批次 2 |
| P2-G | P0-A + P0-B | 无 | 批次 3 |
| P2-H | P0-A + P0-B + P1-C | 无 | 批次 3（随批次 1-2 逐步搭建） |

---

## 6. 观测与验证（整体）

| 层级 | 手段 | 说明 |
| --- | --- | --- |
| 单元 | 序列化字节断言 | system/messages/tools 的字节稳定性测试（P0-A/P1-C/P1-D） |
| 集成 | mock SSE 服务器 / 进程内 MockProvider | 断言请求前缀逐轮不变；usage 帧解析；重试请求体一致（P0-B/P1-E） |
| 真实 API | `#[ignore]` 集成测试（需 key） | 实测 `prompt_cache_hit_tokens` 增长曲线；探针设计参考 Reasonix `realcache_test.go`（前缀需明显超过 64-token 缓存块粒度） |
| 产品 | UI 命中率展示 | P0-B 之后，状态栏/会话详情显示累计命中率（`runUsageLabel` 扩展） |

协议切换回归位于 `vendor/agent-core/crates/agent-llm/tests/deepseek_protocol_routes.rs`：使用本机 loopback HTTP/SSE 逐条验证 Chat 自定义网关、Responses、Anthropic 兼容口与旧网关兼容回退的 endpoint、公共请求前缀、缓存 usage 及能力声明。它证明 R-Code 的协议适配，不宣称任意第三方网关都完整兼容 DeepSeek。

**发布门槛**（两个时点，缺一不可）：① **P0-A 落地前**采集 10 轮以上工具会话的缓存命中率基线并**存档**（预期 ≈0%，作为"优化前"对照）；② **P0-A 完成后**复测同场景，基线应 ≥85%（作为"优化后"对照）。基线采集细节见 P0-B 验收；若未达 85%，回退路径见 §8。

**实测记录（2026-08-07）**：① 未单独采集（P0-A 先行实施），以冷启动全 miss 数据替代，存档于 `docs/archive/deepseek-cache-baseline.md`。② **达成**：真实 API 14 轮探针 tail_avg(3)=93.0% ≥85%（第 12-13 轮单轮 95.4%/97.0%；字节前缀 mock 守卫 tail_avg=96.5%，commit `69c49e9`）。10 轮内 tail_avg（82.2%）偏低属短会话结构性稀释——每轮追加占比高，轮次增加后趋近 95%+，完整曲线与分析见基线文档。命中按 ~128 token 块量化；探针跨运行仍命中（round 0 hit=128）证明服务端缓存持久。

**协议切换实测（2026-08-09）**：使用同一段稳定长 system、每条线路连续 3 次低输出请求，官方 Responses（`deepseek-v4-flash`，`/v1/responses`）3/3 返回非空且每轮解析为 input=248、cache hit=128、miss=120；官方 Anthropic Messages（`deepseek-v4-pro`，`/anthropic/v1/messages`）3/3 返回非空，归一后每轮 input=169、cache hit=128、miss=41。实测同时发现并修复 Anthropic `message_delta` 将 `usage` 与 `stop_reason` 放在同一帧时丢失 output usage 的问题。忽略型实网探针位于 `deepseek_cache_probe.rs`；自定义网关使用 loopback HTTP/SSE 覆盖 endpoint 拼接、usage 解析及不支持 `stream_options` 时的兼容回退，未对未知第三方服务作可用性承诺。

### 6.1 受影响测试清单（实施时同步更新）

| 测试 | 位置 | 影响 |
| --- | --- | --- |
| `system_prompt_includes_fixed_local_clock` | `llm_runtime.rs:5637` | P0-A **必红**：断言目标改为"时间戳不在 system 中段" |
| `name_and_capabilities` | `vendor/.../deepseek.rs:75` | P0-B **必红**：断言改为 `supports_prompt_caching == true` |
| `pure_chat_main_run_is_not_misidentified_as_a_subagent` | `llm_runtime.rs:4270` | P0-A/P1-D：system 断言迁移到尾部注入 |
| `ask_main_run_exposes_codex_delegation_after_workspace_is_attached` | `llm_runtime.rs:4324` | P0-A/P1-D：同上 |
| `explicit_opt_out_hides_delegation_tools_even_with_codex_and_workspace` | `llm_runtime.rs:4401` | P0-A/P1-D：同上（delegation_directive 契约） |
| `task_context_contract_prefers_the_latest_successful_plan_tool_result` / `plan_mode_policy_requires_functional_acceptance_slices` | `llm_runtime.rs:3868, 3892` | P0-A：断言函数输出改为尾部消息形态 |
| `stream_usage_is_requested_only_for_official_openai_endpoint` | `openai.rs:965` | P0-B：仅当"所有兼容口都发 include_usage"才红；方案限定只给 DeepSeek 加，保持绿色 + 新增用例 |
| `provider_error_propagates` / abort 两条 | `agent_loop.rs:1547, 831, 872` | P1-E：重试边界与 abort 语义回归对象 |
| `contract_tests.rs:600-633` 压缩契约 | `crates/r-code-core/tests/` | P2-G：仅接入不修改 vendor 时不受影响 |

---

## 7. 非目标（明确不做）

- ❌ 不修改 DeepSeek API 参数换取缓存（DeepSeek 无显式缓存开关；`cache_control` 仅 Anthropic 口用，OpenAI 兼容口忽略，Reasonix `anthropic_test.go:651` 佐证）。
- ❌ 不为未知第三方网关猜测 DeepSeek 身份或承诺完整兼容；自定义 profile 需沿用 `deepseek` 预设或使用 `deepseek_*` id 显式声明。
- ❌ 不做多会话分离的架构重构（R-Code 当前无 planner/executor 双模型架构；若未来引入，遵循 §4 原则 6）。
- ❌ 不改变时间感知、记忆、任务上下文、委派提示的**用户可见语义**，只改承载位置。
- ❌ 输出截断续写（DeepSeek Beta `prefix` 参数，Reasonix `openai.go:535-606`）本期**暂缓**：仅官方 `/chat/completions` 时收益有限，记为后续可选项。
- ❌ 环境探针跨重启快照（Reasonix `internal/environment/probe.go:61-121`）本期**不做**：r-code 的环境探测发生在宿主层且频率低，收益有限，记为后续可选项。

---

## 8. 风险与开放问题

| 风险/问题 | 说明 | 缓解 |
| --- | --- | --- |
| vendor 子模块本地修改 | `agent-llm` 是固定 commit 子模块（`.gitmodules`）；全仓无本地补丁机制；发布流程含 `git submodule status` 检查（`docs/releasing.md:153`） | 已确认可本地改（用户决策）；改动保持小面、注释标记；发布前把 vendor 子模块**提升到新 commit**（在 vendor 仓库内提交本地改动）或建立 vendor 补丁目录，二选一需在实施前定 |
| P1-E 重试的重复计费与重复输出 | body 层最多 6 次尝试，用户可能看到重复的流式内容片段 | 参照 Reasonix `RequestAttemptCounter`（`agent.go:2976-2978`）统计并展示重试次数；仅对"连接/流中断"重试，已产出内容后不重试 |
| P2-G 压缩基数与模型窗口 | 50/60/80% 相对 provider context window；16K 尾部预算在 64k 模型占 25%、1M 模型占 1.6% | 阈值与尾部预算按模型窗口配置化；超窗兜底：升级模型或强制压缩（90% force 档） |
| P0-B 通用 usage 解析影响其他 OpenAI 兼容 provider | DeepSeek 字段名 `prompt_cache_hit_tokens` 与 OpenAI 官方 `prompt_tokens_details.cached_tokens` 不同 | 解析时两套字段都尝试；缺省回落 `Some(0)` |
| P0-B `supports_prompt_caching` 置 true 的声明变化 | `openai.rs:225` 影响所有 OpenAI 兼容 provider | 已确认生产代码不消费 `enable_caching`；UI 仅用于提示展示，影响小；实施时核查前端 |
| 时间戳下沉后模型时间感知 | 秒级时间 → 分钟级 + 尾部注入，跨分钟边界模型可能答"几分钟前"不准 | 接受：用户可见收益（速度/成本）大于此损失；此设计不挂靠 Reasonix（其完全不注入时间） |
| 压缩改变历史语义 | 摘要折叠可能丢细节 | 分层阈值 + 防抖 + 小 user 轮次 verbatim（≤1500 token/15%）保留 |
| task_context 尾部注入的行为变化 | 模型对 system 中指令的遵循度可能弱于 user 尾部 | 集成测试回归覆盖 Plan 工具调用、active_feature 解析等场景 |
| 守卫测试失效风险 | Reasonix 的守卫默认 skip、非严格只警告 | r-code 照搬定位并写入发布手册；严格模式在 release 前跑一次 |
| P0-A 后基线未达 85% | 时间戳/动态内容下沉后仍 miss（如 tools 内容漂移、其他未识别字节源） | 用 P2-H 的 PrefixShape 归因定位变化原因；若为 tools 哈希漂移则检查 A15；未解决前不推进 P1 批次 |

---

## 9. 完成定义（DoD）

- [x] §5 全部 P 项验收 checkbox 除两处已记录例外外全绿（P1-F missing-reasoning 回放、P2-G 命中率恢复场景测试，见 §5 对应注释）。
- [x] §6 真实 API 基线报告采集并**存档到 `docs/`**（`deepseek-cache-baseline.md`：2 轮基线 + 14 轮真实 API 曲线，tail_avg(3)=93.0% ≥85%，发布门槛②达成）。
- [x] §6.1 受影响测试清单全部同步更新，既有测试无回归。
- [x] vendor 改动带注释标记，且发布流程（`git submodule status` 检查）通过（gitlink `d26f02e`，子模块仓内提交 `fe64d90`/`34cd10f`/`d26f02e`）。
- [x] `docs/readme.md` 索引与本文档状态从"草案"更新为"已实施"；`docs/architecture.md` 中受影响的章节（Agent 执行链路 §6.3、Provider §12）已同步。
- [x] §8 开放问题中"vendor 修改机制二选一"已决策并记录：**采用「vendor 子模块提升到新 commit」方案**（父仓 gitlink 指向子模块新 commit，`git submodule status` 一致）。

---

## 10. 参考实现（Reasonix 对照表）

| R-Code 实施项 | Reasonix 参照 | 文件 |
| --- | --- | --- |
| P0-A system 不可变 + 动态内容下沉 | system 仅会话创建时写入；CreatedAt 清零（时间不进模型输入）；偏好块注入 user turn 前缀 | `internal/agent/agent.go:2887-2893, 3268-3280`、`internal/agent/reasoning_language.go:39-67, 191-204`、`internal/agent/session.go:61-72` |
| P0-A/P1-D 动态上下文走 user-turn 后缀 | memory 检索追加 user-turn 后缀，永不改写 system | `docs/SPEC.md:296-300` |
| P1-C tools 按名排序 | `sort.Strings(names)` | `internal/tool/tool.go:522-544` |
| P1-D append-only + 失败不写 session + 归一化快路径 | Failed attempts never write Session；健康历史零拷贝 passthrough | `internal/agent/run_loop.go:363-365`、`internal/provider/provider.go:318-455` |
| P1-E 冻结请求重试 + 分层退避 + 流空闲 watchdog | 深拷贝冻结；连接 10 次 / body 6 次；120s 流空闲 | `internal/agent/agent.go:45-48, 2918-2958`、`internal/provider/retry.go:18-28, 221-228, 290`、`internal/provider/openai/openai.go:43-50` |
| P1-F 400 兼容（键恒发 + SanitizeToolPairing）+ missing-reasoning 回放 | `reasoning_content`/`name` 键恒发；同一冻结请求精确重放 | `internal/provider/openai/openai.go:688-735`、`internal/agent/run_loop.go:444-493` |
| P2-G 分层压缩 + 防抖 + 归档/机械折叠 + tokPerChar | 50/60/80% 阈值；pin 上限 1500/15%；尾部 16K；防抖；归档 + 机械折叠；真实 usage 校准 | `internal/agent/compact.go:31-36, 100-167, 217-342, 674-688` |
| P2-H 缓存归因 + release 守卫 | PrefixShape/CompareShape；本地元数据不算 miss；env 门控守卫 tail_avg≥90% | `internal/agent/cache_shape.go:66-73`、`internal/agent/cachehit_e2e_test.go:378-477` |
| 可选借鉴（本期不做） | 环境探针跨重启快照 + flap merge；输出截断续写（Beta prefix） | `internal/environment/probe.go:61-121`、`internal/provider/openai/openai.go:535-606` |

---

## 11. 交付物清单

1. 本文档（PRD）。
2. （实施阶段）P0-A/B 代码变更 + 单测/集成测试（含 §6.1 清单更新）。
3. （实施阶段）P0-B 真实 API 基线报告（命中率曲线，存档 `docs/`）。
4. （实施阶段）P1/P2 按「依赖与批次」表推进，每项附验收记录。
