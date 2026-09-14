# Harness 与 Provider 接入审计（只读核查）

> 核查问题：**r-code 是否实现了 harness，所有 provider 都能使用这套链路吗？**
> 结论时间：2026-09-13 ｜ 全篇结论均带文件绝对路径与行号，可逐条复核。

---

## 0. 结论速览

| 问题 | 结论 |
| --- | --- |
| r-code 是否实现了 harness？ | **是，已完整落地并可运行**（协议/SDK/宿主路由/2 个内置插件/第三方示例 + CI 门禁） |
| 所有 provider 是否都能走这套链路？ | **机制上统一，实际范围受三处约束限制**：只有目录内 30 条预设 + 有凭据的 selection 能进；broker 投影有缺口（图片/推理/托管工具）；**桌面 GUI 保存的 provider 根本不进这条链路** |
| 是否"所有 harness"共用同一 provider 链？ | **不成立**。`native.r-code` 与第三方示例走 `host.model.stream`；`codex.r-code` 刻意旁路，不使用宿主 provider |

一句话：**harness 引擎是真的；"所有 provider 通用"要打折——按 provider 目录 + TUI `/setup` 是通的，按桌面 GUI 设置页则不通。**

---

## 1. Harness 已实现：证据链

### 1.1 架构分层（三条独立 crate，依赖方向受守卫）

- 协议：`crates/r-code-harness-protocol/src/manifest.rs`（`HarnessManifest` / `HarnessId` / 19 项 `HostService` / `negotiate()`）
- 线路：`crates/r-code-harness-protocol/src/rpc.rs:11`（`MAX_FRAME_BYTES = 1MB`）、`:20-27`（宿主→插件 6 个生命周期方法）、`:30-52`（插件→宿主 21 个方法）
- SDK：`crates/r-code-harness-sdk/src/lib.rs`（`HarnessHandlers` trait + `SdkHandle` + `serve()`）
- 宿主路由：`crates/r-code-runtime/src/plugins/router.rs:73` 方法→服务映射，`:166-180` 五道闸门（已知方法 → 已授予服务 → 存活 generation → run 作用域 → 操作去重）
- 进程与会话：`crates/r-code-runtime/src/plugins/session.rs`（派生进程 + `initialize` 握手）
- 目录与版本：`crates/r-code-runtime/src/plugins/catalog.rs`（`HOST_API = v1.0`、平台可用性、pin）
- 任务驱动：`crates/r-code-runtime/src/run_manager.rs`（`DEFAULT_HARNESS_ID = "native.r-code"`；`:350-357` 把每任务偏好塞进 `harness_config`）

### 1.2 内置插件与第三方示例

- `plugins/native/harness.json`：申请 `host.model.stream` 等 14 项服务；`plugins/native/src/loop_engine.rs:129-237` 实现多轮 model/tool 循环（每轮 `host.tools.call` 带 `turn-{n}-{id}` 稳定操作键、每轮 `host.checkpoint.save`、结束 `host.completion.propose`）
- `plugins/codex/harness.json`：申请 `host.process.open` 等 14 项服务，**不含 `host.model.stream`**
- `examples/repair-harness`：第三方 harness，仅依赖 SDK + 协议

### 1.3 本次实际运行验证（非仅阅读源码）

| 验证项 | 命令 | 结果 |
| --- | --- | --- |
| 模型 broker 行为契约 | `cargo test -p r-code-runtime --test t15_expose_model_providers_through_a_broker` | **4 passed / 0 failed**（0.31s） |
| native 真实插件进程跑完整工具循环 | `cargo test -p r-code-harness-native --test t26_port_the_native_model_tool_loop_to_a_harness_bin` | **2 passed / 0 failed**（15.10s） |

t26 的通过含义是实质性的：真实派生 native 插件二进制，经公共宿主服务真的改写了 fixture 文件、存了 checkpoint、投递了 completion proposal（`plugins/native/tests/t26_...rs:225-249`）。

### 1.4 CI 门禁真实存在

- `.github/workflows/ci.yml:229` `cargo test --workspace --all-features -- --test-threads=1`
- `.github/workflows/ci.yml:232` `node scripts/verify-harness-v2.mjs --profile quick`
- `scripts/verify-harness-v2.mjs:44-90` 四类架构守卫（protocol/kernel/runtime/sdk/两个插件存在；kernel 无 tauri/sqlite/gateway；protocol 中立；native 插件 host-free；T42 旧链路退役断言）
- `:93` 一致性套件作为硬门禁：`cargo run -p r-code-evals --bin harness-conformance`
- `crates/r-code-evals/src/harness_conformance.rs:88-237` 五项确定性检查（假完成拒签、陈旧证据拒签、跨 run 句柄拒签、重复副作用重放、已吊销代次拒绝迟到调用）

**注意**：CI 只跑 `--profile quick`。`scripts/verify-harness-v2.mjs:95-109` 的 `full` 档（含 `harness_v2_chat`、t33、t35、t36、打包检查）**不在 CI 中执行**。

---

## 2. Provider 统一链路：机制成立

### 2.1 正交设计（这是架构的关键优点）

provider（模型后端）与 harness（执行引擎）是两层，且 provider 只由宿主持有：

- 插件侧只能传**不透明 selection 字符串**，拿不到密钥与地址
- `crates/r-code-runtime/src/services/models.rs:25-31` `ProviderResolver` trait 明确注释 "Host-owned; implementations hold credentials"
- `:163-165` 未知 selection 直接报错 `unknown model selection`
- 密钥不上线有测试断言：`crates/r-code-runtime/tests/t15_...rs:288-295` 断言 wire-safe 视图不含 `api_key` / `apiKey` / `Bearer` / `credential`

因此**机制上，任意 harness（native / codex / 第三方）都可以通过 `host.model.stream` 复用同一套 provider**。

### 2.2 生产装配（daemon 侧）

```
plugins/native (host.model.stream)
  → crates/r-code-runtime/src/plugins/router.rs:248  host.model.stream 分支
  → crates/r-code-runtime/src/services/models.rs:152 ModelBroker::stream (impl ModelService)
  → ProviderResolver（生产实现 = SettingsBackedResolver）
  → agent_llm::create_provider(...)  → Anthropic/OpenAi/Responses/DeepSeek/Kimi/Ark …
```

- 生产注入点：`crates/r-code-runtime/src/bin/r-code-service.rs:520-523`
  `ModelBroker::new(SettingsBackedResolver::new(SettingsStore::new(profile.harness_v2_root())))`
- `crates/r-code-runtime/src/services/settings_store.rs:404-422` `SettingsBackedResolver` **每次解析都重建 registry**，所以运行期改设置下一次模型调用即生效，无需重启 daemon

### 2.3 但 provider 范围有三重约束

**约束一：必须是目录内预设。**
`crates/r-code-runtime/src/services/settings_store.rs:275-277`：

```rust
let preset = provider_catalog::find(&entry.selection)?;   // ← 不在目录里直接返回 None
```

`crates/r-code-runtime/src/services/provider_catalog.rs` 中 `id:` 共 **30 条**（anthropic / openai / xai / azure_openai / bedrock / deepseek / deepseek_anthropic / kimi / kimi_coding / zhipu / zhipu_coding / zai / ark_coding / ark_coding_openai / ark_agent / ark / byteplus / bailian / bailian_coding / dashscope / minimax / minimax_intl / qianfan_coding / stepfun / longcat / xiaomi_mimo / xiaomi_mimo_plan / bailing / kat_coder / openrouter）。
→ **自定义 selection、目录外厂商一律跳过**（`provider_support.rs:12-16` 注释也确认"目录才是唯一事实来源"）。

**约束二：必须有可解析凭据。**
`settings_store.rs:239-241` —— 无凭据的 entry 直接 `continue`，连 provider 都不构造。凭据来源二选一：平台凭据库（服务名 `"r-code-harness-v2"`，`settings_store.rs:31`）或 `env_var` 环境变量（`:205-212`）。

**约束三：只有 3 种线路协议。**
`provider_catalog.rs:34-44` `Protocol` 仅 `AnthropicMessages` / `OpenAiChat` / `OpenAiResponses`。目录里 30 条预设按其 id + 协议映射到 `agent_llm::ProviderConfig` 的 11 个变体（`settings_store.rs:303-388`）。目录自身也标注了未实现项，例如 `:368` Bedrock "需要 SigV4 签名；用长期 API Key 模式才能走现有的 Bearer 分支"。

### 2.4 broker 投影缺口（"理论上都通"与"实际都行"的差距）

`crates/r-code-runtime/src/services/models.rs` 把线路请求投影为 `agent_llm::CompletionRequest` 时：

| 项 | 实现 | 影响 |
| --- | --- | --- |
| 图片 | `:132-134` 投影为**文本占位** `[image: {blob_id}]` | 原生多模态**不通**（t15 也只断言"图片以 artifact 引用传递"） |
| 推理旋钮 | `:85-90` 只读 `temperature` | thinking / effort 等被丢弃（native 侧 `loop_engine.rs:24-25` 确实传了 `inference`） |
| `max_tokens` | `:84` 硬编码 **8192** | 未走目录的 `recommended_output_tokens` / `max_output_tokens` 钳制 |
| `system` | `:64` 恒为 `None`；系统提示先塞成 System 消息（`:99-100`）再降级为 user 角色 | 与 `deepseek_anthropic` 预设"system 必须走顶层字段"的注意事项（`provider_catalog.rs:435`）存在张力 |
| `enable_caching` | `:91` 硬编码 true | 无法按 provider 关闭 |
| 推理/托管工具事件 | `:248-251` `ReasoningDelta` / `ToolUseComplete` / `HostedToolUse` / `HostedToolResult` **全部丢弃** | 服务端联网工具（web_search 等）结果不透出给 harness |
| 选择列表视图 | `:305-310` `SELECTION_PROBE` 只有 4 个**硬编码** selection | `selections_view()`（`:293-302`）不是真实目录，具有误导性 |

### 2.5 一个需要真实 provider 复验的疑点（工具结果角色）

`models.rs:97-103` 的角色映射：

```rust
ModelRole::System | ModelRole::User  => Role::User,
ModelRole::Assistant | ModelRole::Tool => Role::Assistant,   // ← Tool → Assistant
```

而 `agent-llm` provider 层的不变量是 **ToolResult 必须落在 User 消息里**：

- `vendor/agent-contracts/crates/agent-llm/src/openai.rs:657` 只对 `m.role == Role::User` 的 tool_result 做孤儿过滤
- `vendor/agent-contracts/crates/agent-llm/src/anthropic.rs:1733-1736` 测试构造的正是 `Role::User` + `ContentBlock::ToolResult`
- Anthropic Messages 协议本身要求 `tool_result` 仅出现在 user 消息

native 插件确实以 `role: "tool"` 推入工具结果（`plugins/native/src/request_projection.rs:118-126`），该角色即映射到 `Role::Assistant`。
→ **静态证据提示多轮工具回传在真实 provider（尤其 Anthropic 口）上可能被拒。**
现有测试**未覆盖**这一点：`crates/r-code-runtime/tests/t15_...rs:246-253` 只断言内容块类型含 `tool_use` / `tool_result`，**未断言角色**。
**诚信标注：本条为源码级推断，本次未做真实厂商 API 端到端实测（无可用密钥），需要以真实 key 复验后才能定性为缺陷。**

---

## 3. 桌面 GUI 的配置链路断点（影响最大的实际可用性问题）

### 3.1 两条互不相通的配置通道

| | 桌面 GUI | harness v2 daemon |
| --- | --- | --- |
| 保存入口 | `src-tauri/src/tauri_commands.rs:2147` `cmd_settings_save_provider` | — |
| 落地实现 | `src-tauri/src/commands.rs:11705` `settings_save_provider` | — |
| 配置写入 | `commands.rs:11875` `SettingsService::save_global` → `config_dir/config.toml`（`src-tauri/src/settings.rs:371-373`、`:682-711`） | `harness_v2_root/settings.json`（`crates/r-code-runtime/src/services/settings_store.rs:134-151`） |
| 凭据服务名 | `"r-code"` / `"r-code-dev"`（`src-tauri/src/app_paths.rs:19-20`） | **`"r-code-harness-v2"`**（`settings_store.rs:31`） |
| 读配置 | GUI 进程内 | `r-code-service.rs:520` |

**关键证据**：在 `src-tauri/src/` 全量搜索 `SettingsStore` / `SettingsBackedResolver` / `settings.apply` / `ProviderEntry` —— **零命中**。即桌面端**没有任何路径**把用户在设置页保存的 provider、密钥、默认 selection 写进 daemon 的 `harness_v2_root/settings.json`。

### 3.2 GUI 聊天已切 v2，但模型选择被显式丢弃

- 聊天命令已改走 v2 投影层：`src-tauri/src/tauri_commands.rs:27-28`（注释 "T42 阶段 1：聊天链路命令改走 v2 daemon 投影层"）、`cmd_task_create` 走 `chat_v2.task_create`
- 但：`src-tauri/src/harness_v2_chat.rs:104-113` 注释明说 **"provider/agent 参数诚实忽略——v2 默认 pin 内置 native harness，模型选择走 v2 settings 的默认 selection"**，形参 `_provider_name` / `_agent_engine` 带下划线前缀
- 后果被测试自己承认：`src-tauri/tests/harness_v2_chat.rs:5-6` "无 provider 时 run 诚实失败同样算投影成功"；`:99-102` 断言只要求 "at least one run must reach a terminal projection (assistant reply or honest failure)"

### 3.3 TUI 侧反而是通的

- `crates/r-code-tui/src/engine.rs:297-314` `/setup` → 调 daemon 的 `settings.apply`（apiKey 或 envVar 二选一）
- `crates/r-code-tui/src/lib.rs:397-406` 首屏直接读 `SettingsStore::new(profile_root).load()` 判定是否有可用 provider
- `crates/r-code-tui/src/engine.rs:129-135`、`harness_client.rs:92-98` 均经 `profile.harness_v2_root()`

→ **当前状态：TUI 执行过 `/setup` 之后，桌面 GUI 的对话才有模型可用；否则 run 会失败。** 这是阶段 1 的已知迁移缺口（`src-tauri/src/harness_v2_chat.rs:10-11` 自述"旧实现原地保留，本期只挂新路径"）。

---

## 4. Codex harness 不走宿主 provider（架构意图，非缺陷）

- `plugins/codex/harness.json:23-38` `requestedHostServices` **不含 `host.model.stream`**
- 它经 `host.process.open` 以 profile `codex-app-server` 派生外部 codex 进程：`plugins/codex/src/app_server.rs:34-45`
- 目的明确写在 `plugins/codex/src/lib.rs:1-7`："provider credentials never reach this process"
- 因此 **"所有 harness 都用同一 provider 链路"不成立**——codex 是刻意的旁路，模型凭据由 codex 自行管理

**附带发现（可能未完成）**：`app_server.rs:127-140` 调用 `codex.event.next`，但该方法**既不在** `crates/r-code-harness-protocol/src/rpc.rs:30-52` 的 `PLUGIN_TO_HOST_METHODS` 中，**也没有**在 `router.rs` 的 `service_for_method` 里注册 → 宿主无实现，调用会落到 `method_not_found`。插件源码自述这也是首个切面：`plugins/codex/src/main.rs:5-7` "this first cut serves the lifecycle surface"。即 Codex harness 当前只走了生命周期/初始化的协议面，完整 App Server 事件流尚未闭环。

---

## 5. 审计方法与可复核性

- **方式**：只读源码核查（Grep/Read），排除 `sandbox/`、`target/`、`artifacts/`；未修改任何生产代码；未访问用户真实凭据库
- **实测**：t15（4/4）、t26（2/2）已在本机实跑，日志留存于 `target/harness-provider-audit-t15-20260913.log`、`target/harness-provider-audit-native-20260913.log`
- **未验证项（如实标注）**：
  1. 真实厂商 API 端到端调用（无密钥）——含第 2.5 节的工具结果角色疑点
  2. `verify-harness-v2 --profile full` 全档（CI 只跑 quick）
  3. 桌面 GUI 在"未执行 TUI `/setup`"之外的使用路径

## 6. 建议的修复优先级

| 优先级 | 事项 | 位置 |
| --- | --- | --- |
| P0 | 用真实 key 复验多轮工具回传角色映射，若确认则修 `ModelRole::Tool` 的 role 投影 | `models.rs:97-103` |
| P0 | 打通桌面设置页 → daemon `settings.apply`（或提供一次性导入），否则 GUI 对话无模型可用 | `tauri_commands.rs:2147` / `harness_v2_chat.rs:104-113` |
| P1 | broker 投影补齐：图片走 artifact 物化、`max_tokens` 走目录、推理旋钮透传、推理/托管工具事件透出 | `models.rs:61-94`、`:248-251` |
| P1 | `selections_view()` 改为读真实目录，去掉 4 项硬编码探针 | `models.rs:293-310` |
| P2 | 补齐 Codex 的 `codex.event.next` 宿主实现，或从插件中移除该调用 | `app_server.rs:127-140` / `router.rs:73` |
| P2 | 把 `verify-harness-v2 --profile full` 纳入 CI | `ci.yml:232` |
