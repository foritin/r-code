# Ark / Kimi Provider 适配改造清单

> 状态：已实施（2026-08-16，单一 PR）。本文档把「Ark、Kimi 的厂商方言适配」
> 拆成可独立验收的工作项，第 2 节表格为真实 key 探针冻结后的最终值。
> 每个工作项都给出目标文件、具体改动、验收标准和测试位置。
>
> 前置结论（已完成调研）：Ark 与 Kimi 的预设、线路、协议分派已存在，缺的是
> 预设之后的**按厂商/模型族的参数方言与能力声明**；Kimi 额外缺**思考内容回传**。
> DeepSeek 的 governor（廉价探索轮、快摘要、思维链回放）不在本期范围。

## 0. 术语

- **厂商身份 provider_kind**：配置里持久化的稳定标识，例如 `ark`、`ark_coding`、
  `ark_agent`、`kimi`、`kimi_coding`、`deepseek`。不随展示名或地址变化。
- **模型族 model family**：按模型名前缀归类的方言组，例如 `doubao-seed-2.*`、
  `deepseek-*`、`glm-*`、`kimi-k2*`。
- **方言 WireDialect**：把「本地推理选项 → 厂商 wire 参数」的映射和该线路的
  能力声明集中成一个值对象，由 `(provider_kind, model)` 解析。

## 1. 目标与非目标

### 目标

1. Ark/Kimi 用户不再遇到可归因于参数方言的 400；
2. Kimi k2.7-code/k3、Ark 上的 DeepSeek 系列多步工具调用能正常回传思考内容；
3. 压缩窗口、输出上限、思考开关按真实厂商/模型能力生效；
4. Responses 线路获得与 Chat 线路同等的重试与健壮性；
5. 前端为 Ark 各模型族暴露正确的思考/推理强度入口。

### 非目标（本期不做）

- DeepSeek 式 governor（廉价探索轮 / 快摘要 / 思维链回放）；
- Responses `store + previous_response_id` 服务端会话（留 feature flag 待 A/B）；
- 各厂商原生 SDK；
- 任意中转站的完整方言适配（只适配目录内官方直连线路）。

## 2. 方言事实表（编码前必须逐格冻结）

下表是 wire 层唯一事实来源。标注「待探针」的格子，在阶段 1 写代码前必须用
阶段 3 的探针矩阵确认，并回填结论；未确认前不得凭猜写入映射。

| provider_kind | 模型族 | thinking 词表 | 本地 adaptive → | temperature | 回传 reasoning | stream usage | 上下文 | 输出上限 |
|---|---|---|---|---|---|---|---|---|
| ark_coding / ark_coding_openai | ark-code-latest | enabled/disabled/auto/adaptive 全透传 | 不翻译 | 保留 | 否（不带也 200） | OpenAI 口开启 | 256000 | 0（套餐未声明） |
| ark_agent | ark-code-latest 及套餐托管模型 | 同 coding（Anthropic 口） | 不翻译 | 保留 | 否 | — | 1048576 | 0 |
| kimi_coding | kimi-for-coding / kimi-for-coding-highspeed | 只发 enabled 或不发 | adaptive→enabled | 一律不发送（0.3 实测 400） | 是（吃 prefix cache，非硬性 400） | — | 262144 | 32768 |
| kimi_coding | k3 / k3-256k | 只发 enabled 或不发 | adaptive→enabled | 一律不发送 | 是 | — | k3=1048576，k3-256k=262144 | 32768 |

已确认的外部事实：

- Ark `GET /api/v3/models` 返回模型列表（Bearer 鉴权），模型发现可直连；
- Ark `model` 字段同时接受 Model ID（如 `doubao-seed-2-1-pro-260628`）与
  推理接入点 ID（`ep-xxx`）；
- Kimi `/coding` 网关对非白名单 `User-Agent` 返回 429 `engine overloaded`，
  参考 [opencode#27902](https://github.com/anomalyco/opencode/issues/27902)、
  [pi#3538](https://github.com/earendil-works/pi/issues/3538)、
  [Kimi 错误参考](https://www.kimi.com/code/docs/kimi-code/error-reference.html)。

## 3. 阶段 0：探针矩阵（0.5 天，不改代码）

用真实 key 逐条 curl，把上表「待探针」格回填，并把结果固化为 fixtures。

### Ark 按量（`https://ark.cn-beijing.volces.com/api/v3`）

1. `model` 分别传 Model ID 与 `ep-xxx`，确认两种都 200；
2. 同一模型分别发 `thinking: {"type":"auto"}` / `enabled` / `disabled` /
   不传，确认 200 或 400 及错误码；
3. `reasoning_effort` 传 `minimal/low/medium/high`（doubao-seed-2.*）；
4. 一轮「assistant tool_calls + tool 结果」：第二轮把上一轮 assistant 的
   `reasoning_content` 原样带回 vs 不带回，对比 200/400；
5. `stream:true` + `stream_options:{"include_usage":true}`，确认流末 usage 帧
   与 `prompt_tokens_details.cached_tokens`；
6. `GET /models` 记录返回形状与模型名；
7. 触发一次 429（低并发反复请求），记录 `Retry-After` 与错误体。

### Kimi

1. `api.moonshot.cn/anthropic/v1/messages` 分别用 `x-api-key` 与
   `Authorization: Bearer`，确认哪种 200；
2. `kimi-k2.7-code`：发 `thinking:{"type":"disabled"}` 是否 400；
   `kimi-k3`：发/不发 thinking 分别是否 200；
3. 思考开启时，工具轮是否必须把上一轮 `reasoning_content` 原样带回（去掉则 400？）；
4. 思考模型请求里带 `temperature` 是否 400；
5. `GET api.moonshot.cn/v1/models` 与 `/anthropic/v1/models` 的形状；
6. `api.kimi.com/coding/v1/messages` 分别以无 UA、`R-Code/x.y.z`、
   其它 UA 各发一次，记录 429 触发情况。

验收：探针结果表回填到第 2 节，标注日期与 key 环境；每个 400 保存原文错误码。

## 4. 阶段 1：协议层 WireDialect（P0）

### 4.1 新增 `vendor/agent-contracts/crates/agent-llm/src/dialect.rs`

定义：

```rust
pub enum ThinkingWire { None, Object, EnableThinkingBool }
pub struct WireDialect {
    pub thinking_vocab: &'static [&'static str], // 合法 wire 值
    pub adaptive_maps_to: Option<&'static str>,  // 本地 adaptive 的 wire 值
    pub thinking_wire: ThinkingWire,
    pub omit_temperature_when_thinking: bool,
    pub force_stream_usage: bool,
    pub echo_reasoning: bool,
    pub max_context_tokens: u32,
    pub max_output_tokens: u32,
    pub supports_vision: bool,
    pub emit_cache_control: bool, // 仅 Anthropic 口
}
pub fn dialect_for(kind: &str, model: &str) -> WireDialect
```

`dialect_for` 先按 `kind` 再按模型名前缀匹配；未命中回落到与现状等价的通用
OpenAI/Anthropic 方言（保证不回归）。映射表与第 2 节冻结结果一一对应，并为
每一行写单测。

### 4.2 `openai.rs`

- 结构体增加 `dialect: WireDialect` 字段与 `pub(crate) fn with_dialect(...)`；
  保留 `with_stream_usage()` 作为向后兼容包装；
- `build_body` 里把 `is_deepseek` 分支替换为读 `self.dialect`：
  thinking 词表校验 + `adaptive_maps_to` 翻译（当前 `openai.rs:126-150`）、
  temperature 豁免（`:174`）、`stream_options`（`:190`）；
- `apply_key_emission` 的 `deepseek_thinking` 参数改为 `dialect.echo_reasoning`
  且 thinking 开启（`:594-633`）；
- `capabilities()` 的 128K 硬编码改为 `dialect.max_context_tokens`。

验收：DeepSeek 现有单测不改一字仍通过；新增 Ark/Kimi 请求体断言（thinking
翻译、不发 temperature、`include_usage`、reasoning 键）。

### 4.3 `anthropic.rs`

- 增加 `with_dialect`；thinking 翻译、temperature、`cache_control` 注入、
  `max_context_tokens`/`max_output_tokens` 都读方言；
- `new_deepseek` / `new_kimi_coding` 改为 `with_dialect(dialect_for(...))`
  的薄封装，保持外部调用不变（`anthropic.rs:61-88`）。

### 4.4 `agent-contract/src/provider.rs`

- `Capabilities` 增加 `#[serde(default)] pub supports_reasoning_echo: bool`；
- 所有构造点补字段，测试里断言 Ark/Kimi 线路为 true、通用线路为 false。

### 4.5 `agent-llm/src/lib.rs`

- `ProviderConfig` 增加 `ArkChat`、`ArkAnthropic`、`KimiAnthropic` 三个变体
  （`KimiCodingAnthropic` 保留）；
- `create_provider` 用 `dialect_for(kind, model)` 构造通用 Provider；
- 工厂单测覆盖新变体的 name 与 capabilities。

### 4.6 `src-tauri/src/commands.rs`

- `build_provider_config`（`:12363`）新增
  `is_ark_provider`（kind ∈ ark/ark_coding/ark_coding_openai/ark_agent）与
  `is_kimi_provider`（kind = kimi）分派到新变体；
- 构造时把 `provider_preset(name)` 的 `context_window` / `max_output_tokens`
  传入，覆盖方言默认值——这一步让 [llm_runtime.rs](D:/project/rust/r-code/crates/r-code-agent-worker/src/llm_runtime.rs:2941)
  的压缩窗口从 128K 修正为 256K/1M。

验收：Ark 按量会话的压缩窗口按 262144 计；Agent Plan 按 1048576 计；DeepSeek
行为不变。

## 5. 阶段 2：reasoning 回传泛化（P0，收益最大）

### 5.1 `crates/r-code-agent-worker/src/agent_loop.rs`

- 把 `preserve_plaintext_reasoning = provider.name() == "deepseek_responses"`
  （`:786`）改为 `provider.capabilities().supports_reasoning_echo`；
- 对 OpenAI Chat 线路（Kimi k2.7-code/k3、Ark 上的 DeepSeek）回放时，给
  assistant 消息补 `reasoning_content`；Anthropic 口沿用既有 thinking 块
  回传路径（先确认 `anthropic.rs` 的 parse/serialize 已 round-trip，缺则补）。

### 5.2 消息模型承载 reasoning

- 方案 A（推荐，最小侵入）：`Message` 增加
  `#[serde(default, skip_serializing_if="Option::is_none")] pub reasoning_text: Option<String>`，
  `messages_to_openai` 在 assistant 轮写出 `reasoning_content` 键，
  Anthropic 转换同理；流式累积写入该字段；
- 方案 B（备选）：新增 `ContentBlock::Reasoning` 变体，语义更干净但会波及
  序列化、压缩、记忆等全部消费方，风险大。
- 选 A，并在 `crates/r-code-core/tests/memory_contracts.rs` 与 agent-* 公共契约
  测试里验证历史 round-trip 字节稳定（不破坏前缀缓存形状）。

验收：Kimi/Ark-R1 连续两轮工具调用无 400、无空转；`reasoning_content` 在
下一轮请求体中出现且与上一轮逐字一致。

## 6. 阶段 3：目录、分派与前端（P1）

### 6.1 `src-tauri/src/provider_catalog.rs`

- `ark` 按量预设扩充模型清单（按 `GET /models` 实测结果，至少补托管
  deepseek/glm/kimi/doubao 当前可用 ID）；保留注释说明「静态快照 + 设置页实时
  同步兜底」；
- `kimi` 按量预设增加 OpenAI 兼容口候选
  `Endpoint { url: "https://api.moonshot.cn/v1", protocol: OpenAiChat, native: P_C }`；
- 按探针结果修正 `kimi`/`ark_coding`/`ark_agent` 的 `auth` 声明；
- `note` 增加：Ark 支持 `ep-xxx`；k2.7-code 思考约束。

### 6.2 `src-tauri/frontend/src/components/room/model-capabilities.ts`

- 在 `capabilitiesFor`（`:108` 附近）新增 ark* 分支，按模型族返回：
  - doubao-seed-2.*：thinking `enabled/disabled/adaptive`（adaptive 展示为
    「自适应(auto)」，wire 层翻译为 auto）+ effort `minimal/low/medium/high`；
  - deepseek-*：thinking `enabled/disabled/adaptive` + effort（按 v4 档位）；
  - glm-*：`enabled/disabled`；
  - kimi-k2*：`enabled/disabled`（按探针结果决定是否隐藏 disabled）；
  - 其余：只给 note；
- kimi 分支按探针结果收紧（k2.7-code 不发 disabled、k3 不发 thinking）；
- `imageCapabilityFor`：doubao 多模态模型族 → supported，`ark-code-*`/
  `code-latest` 保持 unsupported；
- 复用现有 `normalizeInference` 的 defaultValue 机制，避免把本地 `adaptive`
  误传给厂商；
- 在 `model-switcher.test.mjs` 或新增能力单测覆盖每个模型族。

## 7. 阶段 4：通用健壮性（P1，惠及所有 provider）

1. `responses.rs`：`complete`/`stream` 的直接 `post()`（`:203`、`:233`）改为
   复用重试逻辑；把 `openai.rs` 的 `send_with_retry` 提为 `pub(crate)` 并参数化
   URL，保持 408/429/5xx、`Retry-After` 语义一致；
2. 三个传输层 `reqwest::Client::new()` 增加
   `.connect_timeout(15s).user_agent("R-Code/{version}")`；Kimi Coding 的 UA
   白名单问题先发真实 R-Code UA 实测，若仍 429，再评估是否提供用户可配置的
   UA 覆盖项（决策点，见第 9 节）；
3. `complete()` 旁路补 deadline：`commands.rs:3050`（压缩摘要）与
   `memory_runtime.rs:85`（记忆评审）用 `tokio::time::timeout(120s, ...)` 包裹；
   `llm_runtime.rs` 的自动压缩摘要同样处理；
4. 保持错误脱敏与「不回复」时的可恢复错误标记，不改变现有重放语义。

## 8. 阶段 5：测试与验收

1. 单测：`dialect.rs` 映射全表；openai/anthropic 请求体契约；工厂与分派；
2. 回归：`cargo test -p agent-llm`、`cargo test -p r-code-agent-worker`、
   `cargo test -p r-code-core`；前端 `npm test` 相关用例；
3. 真实 key 冒烟：重跑阶段 0 探针矩阵，逐项 PASS/FAIL 记录；
4. 观测：确认日志能区分错误码（400/429/超时/reasoning 丢失）、usage 与
   缓存命中可见；上线后统计各厂商 4xx/429 占比与压缩触发频率；
5. 文档：更新 `CHANGELOG.md` 与 `docs/readme.md` 的 provider 说明，标注
   本期覆盖的 Ark/Kimi 线路与已知限制。

## 9. 需要拍板的决策点

1. **Kimi Coding 的 UA 策略**：发真实 `R-Code/x.y.z`（诚实但可能仍被 429）
   还是提供用户可配置 UA 覆盖（可绕过峰值 QoS，但更接近伪装第三方客户端）。
   建议先诚实 UA 实测，再决定是否需要配置项。
2. **reasoning 承载方式**：方案 A（`Message.reasoning_text` 字段） vs 方案 B
   （`ContentBlock::Reasoning` 变体）。推荐 A。
3. **优先级**：建议 Ark 先行（工作量小、风险低），Kimi 的 reasoning 回传
   随后单独一个 PR，因为它是唯一深改造。

## 10. Responses 线路补充（2026-08-16 探针冻结）

- **端点**：Coding Plan `POST /api/coding/v3/responses`；Agent Plan
  `POST /api/plan/v3/responses`（`/api/plan/v3/models` 返回 404）。
- **主入口无 Responses**：`/api/coding/responses` 与 `/api/plan/responses` 均返回
  404——不带 `/v3` 的套餐网关只有 Anthropic Messages 口。`/api/plan/v3` 同时
  验证了 Chat（`/chat/completions` 200）；`/api/coding/v3/chat/completions` 用
  Agent Plan key 返回 401，需 Coding Plan 专属 key 复验。
- **无状态重放**：`store=false` 下整段历史重放可用；续轮 input 必须同时携带
  `message(user)` + `function_call` + `function_call_output`，缺前导 user 消息会 400。
- **reasoning 形状**：item 只有 `summary[].summary_text`，没有 `content` 或
  `encrypted_content`；流式事件为 `response.reasoning_summary_text.delta`。回传
  summary-only 的 reasoning item 被接受（`id` / `status` 可省略），但不是硬性要求。
  R-Code 采用 `ReasoningMode::SummaryReplay` 保存并回传，保持多轮思维链不丢。
- **推理强度词表**（glm-5.3 实测）：`low / medium / high / xhigh / max` 全部 200；
  `none` 与 `minimal` 直接 400，实现层遇到时省略参数而不是发送。Ark 没有真正关闭
  reasoning 的 wire 值（不传参数服务端仍输出 reasoning summary），`disabled` 退化为
  「不发送 reasoning 参数」。`temperature` 与 `text.verbosity` 均被接受。
- **usage**：标准 input/output tokens，`input_tokens_details.cached_tokens=0`、
  `caching.type=disabled`，`output_tokens_details.reasoning_tokens=0`。
- **待复验**：Coding Plan 的 `ark-code-latest` 精确 effort 词表需用 Coding Plan
  专属 key 复验（本次用 Agent Plan key 打 `/api/coding/v3/responses` 被路由到
  `glm-5-2-260617`，未验证 ark-code-latest 自身）；Coding Plan 与 Agent Plan 当前
  共用同一套 Responses 方言，差异只有上下文窗口（256K / 1M）。
