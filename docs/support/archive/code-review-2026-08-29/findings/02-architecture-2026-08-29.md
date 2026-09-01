# RV-02 架构与模块边界 — Findings（2026-08-29）

审查者：code-review 代理（RV-02）。全程只读，未修改任何代码。

## 扫描方法与覆盖范围

- **方法**：纯静态。rg 定位 + Read 局部读取；未运行 cargo build/test/clippy。
- **已扫**：
  - `src-tauri/src` 全部 55 个顶层 .rs 的 `crate::X::` 全路径依赖图（含 use 与 inline path），环检测；
  - `commands.rs`（41,299 行）的函数域分布、内嵌 SQL/HTTP/文件 IO 计数、测试区边界、逐条 SQL 归属核对；
  - `llm_runtime.rs`（11,921 行）类型/impl/职责分布（按行号抽样，未通读）；
  - codex_* 五文件族、mcp_{manager,server,settings} 与 `crates/r-code-mcp` 分工；
  - 前端 App.tsx 全读、store/（5 文件）、components/scenes、components/room 抽样；
  - vendor/agent-contracts 快扫（合同泄漏检查）。
- **未扫**：commands.rs / llm_runtime.rs 函数体逐行语义；crates/r-code-* 内部模块级依赖；browser/、automation/、updater/ 子目录内部；任何运行时行为。
- **样本量**：见每条 finding 内 rg 计数；证据命令原文全部在 `../evidence/RV-02-architecture.md`。

## Findings 总表

| ID | 位置 | severity | 根因描述 | 修复方向 |
|---|---|---|---|---|
| F-arch-01 | src-tauri/src/commands.rs（41,299 行） | major | 上帝模块：单文件承载约 20 个业务域、196 个 pub fn、30 个兄弟模块扇入，无内部 mod 结构，15k 行测试同文件 | 按域拆为 commands/ 目录 + 子模块（切分线见正文） |
| F-arch-02 | commands.rs:1099/5679/7416/10199/11982-12160 等；plan_review_tools.rs:121-135；recovery.rs:381-405 | major | host 层对 store 所属表裸 SQL 双写路径：r-code-store repositories 已封装同表操作，host 侧再写 schema 耦合 SQL | 将这些语句下沉为 r-code-store 仓储方法，host 只调服务 |
| F-arch-03 | commands.rs:41 ↔ plan_entry_commands.rs:127/421/460/474；commands.rs:296 ↔ memory_runtime.rs:13 | major | 文件级模块环×2（生产代码）：共享 provider 辅助函数与 CommandState 被锁进上帝模块，反向依赖被迫形成 | 把 build_provider_config/provider_readiness_error/resolve_effective_protocol 下沉到 provider_catalog 或独立 provider 域模块 |
| F-arch-04 | crates/r-code-agent-worker/src/llm_runtime.rs（11,921 行） | major | 第二上帝模块：provider 特化 governor、子代理编排、Codex 适配器、提示策略、上下文注入、plan 目录、核心 runtime 七类职责同文件（68 类型/258 fn） | 至少拆出 provider-governor、subagent-orchestration、codex-adapter 三个子模块 |
| F-arch-05 | commands.rs:17324-18700、25948-25982（codex CLI 生命周期约 1,400+ 行） | minor | codex_* 模块族边界本身清晰，但 CLI 安装/登录/同步/偏好落在 commands.rs，"codex 域"横跨两处 | 迁出为 codex_cli_lifecycle.rs（与 codex_* 族并列） |
| F-arch-06 | lifecycle_commands.rs:8 对照 commands.rs:4-6 声明、Cargo.toml:76 | minor | 「lib 不依赖 tauri」的文档化边界已失真：lib.rs:25 无条件 pub mod lifecycle_commands 且 use tauri::，tauri 是无条件依赖 | 要么补 cfg 门恢复声明，要么更正文档声明 |
| F-arch-07 | mcp_server.rs:20-22 | minor | stdio MCP 端点直依赖上帝模块 commands（agent_send/task_create_with_agent 等），端点二进制依赖闭包被放大 | 抽出端点所需的窄接口（trait 或独立 facade）供 mcp_server 依赖 |
| F-arch-08 | 前端 store/（5 文件）vs 48 个直接 import lib/ipc 的组件文件 | minor | 服务端状态访问模式不统一：部分命令走 zustand store + selectors，其余 48 文件直接调用 typed ipc 函数（有 lib/ipc.ts 边界兜底，非裸 invoke） | 明确约定：跨场景共享状态走 store，场景私有一次性调用走 ipc；或引入统一数据层 |
| F-arch-09 | settings.rs/provider_catalog.rs/provider_models.rs/model_capabilities.rs/provider_readiness.rs/subagent_providers.rs + commands.rs:15039-16508 | minor | provider 域分散 6 个模块 + 上帝模块入口：分层方向尚清晰（provider_catalog 居底），但同一域的 IPC 入口、目录、能力映射、就绪检查分置多处 | 以 provider_catalog 为锚收敛域内聚，减少 commands.rs 中的编排段 |

非 finding 的核对结论（无问题，记录备查）：
- host 与 `crates/r-code-mcp` **无协议实现重复**：mcp_manager.rs 只做视图映射与编排，协议/注册表/网络全在 r-code-mcp；host 层仅 rtk.rs、provider_models.rs 直用 reqwest（E8）。
- vendor/agent-contracts 合同 crate 无产品类型泄漏；agent-ipc 仅管道名前缀含 "r-code-"（产品自持 submodule，可接受，E10）。
- codex_interaction.rs / codex_app_server.rs / codex_mcp.rs / codex_permissions.rs 四文件职责声明与实现一致，边界清晰（E6）。
- tauri_commands.rs（190 个 `#[tauri::command]`）确为薄包装，lib/bin 拆分意图在 commands↔tauri_commands 之间成立（E1）。

---

## F-arch-01 上帝模块 commands.rs（major）

**位置**：`D:\project\rust\r-code\src-tauri\src\commands.rs`，41,299 行；生产区 1–26,168（`mod tests` 起于 26,169，同文件再带约 15k 行测试）。

**事实**：
- 196 个 `pub async/fn`（生产区）、708 个全部 fn、约 30 个 struct/enum 状态类型（含 `CommandState` 982–1031、`AgentBridge` 847、`AgentRuntimePool` 889、`ExternalAgentRegistry` 452）；
- rg 逐名枚举出的业务域 ≥ 20 个：task、plan、agent 发送/队列/steer/resend/abort、codex CLI 生命周期、MCP、memory、review/changes/rollback、git、verification、workspace、terminal、settings/provider、subagent provider 池、attachment、search、notification、recovery、replay、file IO、rtk、workflow skills、knowledge prompts、legacy memory、native notification、support bundle、dashboard/activity；
- 扇入 30 个兄弟 host 模块（E4），是全仓被依赖最广的文件；
- 生产区无任何内部 `mod` 声明——纯平铺；15 处 `#[cfg(test)]` 与 3 个测试 mod 与生产代码同文件交织。

**为什么长成这样/为什么没人拦住**：模块头注释（commands.rs:1-22）显示初始设计是「lib 侧可测核心 + bin 侧薄包装」，这一层确实成立（见非 finding 结论）；但「lib 侧核心」从未再分域——每个新业务域的 IPC 实现都自然落进同一个 `commands.rs`，因为 `CommandState` 在这里、事件出口在这里、测试基建也在这里。文件大小没有任何机制性约束（Rust 不限文件长度，review 时 diff 又按函数粒度看），只有惯性累积。

**修复方向（可行切分线，按现有函数分布，行号为生产区实测）**：
1. `164–1953` 状态与桥接基建（CommandState/AgentBridge/AgentRuntimePool/recovery snapshot）→ `commands/state.rs`；
2. `1954–4764` 任务域（task_*/project_conversation_*）；
3. `2438–3003` plan 域（plan_*/plan_review_*）；
4. `4397–4840` dashboard/活动/通知；
5. `6395–8274` 附件域（attachment_*、QueuedAttachmentsV2 7297+）；
6. `8275–10322` agent 发送/中止/队列/steer/resend（含 7949 send_with_attachment_refs）；
7. `10323–10807` 变更/审查/git；`10808–11006` workflow skills + knowledge prompts；
8. `11007–11436` change request/diff；`11437–11489`+`14595–14663` 验证；
9. `11504–11666` workspace；`11667–11772` 搜索；`11773–12018` 终端；
10. `12019–12335` 恢复/支持包；`12336–13549` 回放/附件预览；
11. `13550–13763` 子代理会话消息；`13764–14490` 记忆/旧版记忆；`14491–14594` 文件 IO；
12. `15039–15497` provider/设置/MCP；`16398–16593` 子代理池/RTK；
13. `17324–18700`、`25599–26168` codex 委派/CLI/技能/协作（配合 F-arch-05）。
测试 `mod tests`（26,169+）随各自域拆走。第一刀建议先做 1（state 基建）与 5/6（附件+agent 发送，最大连续块），它们之间耦合最浅。

**severity 依据**：不直接产生 bug，但 20 域 × 1 文件意味着所有并行改动的合并冲突面、review 盲区（rg 才能找函数）、以及 F-arch-02/03/05 的耦合都由此放大——明确抬高缺陷概率与维护成本。

## F-arch-02 host 层裸 SQL：对 store 所属表的双写路径（major）

**位置与证据**（生产代码，均已在证据文件 E2/E3 复核）：
- `commands.rs:1099` `UPDATE queued_messages SET state = 'sent' ...`（reconcile_durable_steer_queue_claims）；
- `commands.rs:1382-1408` UPDATE tool_calls / SELECT agent_runs（reconcile_tool_calls_for_finished_runs、capture_startup_recovery）；
- `commands.rs:5679` `SELECT COUNT(*) FROM tool_calls WHERE id=?1 AND run_id=?2`（ensure_subagent_run 5651+ 的外键锚点补建）；
- `commands.rs:7416` `UPDATE queued_messages SET attachments_json=?2 ... WHERE ... attachments_json IS ?4`（CAS 改写）；
- `commands.rs:10199` `SELECT input_json FROM tool_calls ...`（reconcile_legacy_task_changes_uncached 10141+）；
- `commands.rs:11982-12160` startup_recovery_items（11970+）：SELECT/UPDATE agent_runs、tool_calls、tasks、INSERT task_events、UPDATE/SELECT permission_requests；
- `commands.rs:14600` `SELECT output_blob_key FROM verifications`；
- `plan_review_tools.rs:121-135` PlanExecutionToolGuard 读 plans/plan_items 判暂停；
- `recovery.rs:381-405` 直接 Connection 写 tool_runs/权限（头部注释自述理由：启动早期池未建）；
- `support_bundle.rs:13` 自开只读 Connection 做统计。

对照：`crates/r-code-store/src/repositories.rs:1690-1798` 已有对 queued_messages 的 UPDATE/INSERT 封装（QueueRepository），`plan_store.rs`/`verification.rs` 各自管理 plans/verifications 表。即同一张表的写路径分布在 crate 两侧。

**为什么长成这样**：`CommandState.db` 直接暴露 `Arc<Database>`（commands.rs:984），`db.conn()` 顺手可得；当一条命令需要"跨表小修"（CAS、审计锚点、启动恢复）时，在 commands.rs 里就地写 SQL 比在 store crate 增加一个针对性方法改动面更小——每次都合理，累积成 20 处。store 的 schema 迁移（migrations.rs）只对自己的调用方负责，host 侧这些字符串不会被迁移测试覆盖。

**风险**：schema 演进（改列名/语义）时 store 侧改完编译通过，host 侧字符串 SQL 直到运行时才炸；CAS 与状态机（如 queued_messages 的 dispatching→sent）被切成两处实现，行为漂移无编译期信号。

**修复方向**：逐条下沉——CAS/锚点/启动恢复都是明确的仓储级操作，应在 r-code-store 增加带语义的方法（如 `QueueRepository::mark_sent_if_in(...)`、`ToolCallRepository::exists_in_run()`），host 只保留编排。recovery.rs 的历史理由（池未初始化）可用"store 提供裸 Connection 级恢复 API"满足。

## F-arch-03 文件级模块环：commands ↔ plan_entry_commands、commands ↔ memory_runtime（major）

**证据**（E4）：
- 环①：`memory_runtime.rs:13` `use crate::commands::{build_provider_config, provider_readiness_error};` ←→ `commands.rs:296/13776/13922/13934` 调 `memory_runtime::spawn_memory_review_worker`；
- 环②：`plan_entry_commands.rs:127`（resolve_effective_protocol）、`:421/:460/:474`（`&crate::commands::CommandState`）、`:435`（provider_readiness_error）←→ `commands.rs:41/332/4232/8037/8382/8687/14076/25332` 使用 PlanningRuntimeState 与 envelope 构造；
- 伪环③（仅测试）：`settings.rs:1000` 在 910 行 `#[cfg(test)]` 模块内调 `crate::commands::execution_env_probe`。

**为什么长成这样**：三个被依赖的符号（`build_provider_config`、`provider_readiness_error`、`resolve_effective_protocol`）是 provider 配置组装的公共助手，最初服务于某个 IPC 命令所以落在 commands.rs；后续 memory_runtime / plan_entry_commands 需要同样逻辑时，最短路径就是反向 import 上帝模块——Rust 模块系统允许同 crate 内互引，无任何机制拦截。crates 级无环（阶段0 结论）恰恰因为这类耦合全部被压进了 host crate 内部。

**风险**：依赖方向失真——memory_runtime（应是底层 worker）依赖了含终端管理、附件、回放的 41k 行模块；对 commands.rs 做任何拆分（F-arch-01）都会被这两个环卡住，必须先解环。

**修复方向**：三个助手函数下沉到 `provider_catalog.rs`（已是 provider 域底层，settings/model_capabilities/provider_models 都指向它）；CommandState 对 plan_entry_commands 的暴露改为一小组显式参数或窄 trait。settings 的测试引用随函数下沉自然消除。

## F-arch-04 第二上帝模块 llm_runtime.rs（major）

**位置**：`crates/r-code-agent-worker/src/llm_runtime.rs`，11,921 行（534KB）；生产区 <10,613 行内 68 个 struct/enum、258 个 fn。

**职责枚举**（行号实测，E7）：
1. provider 特化策略：DeepSeek V4 Flash/Pro、Ark、Kimi 的 ReasoningGovernor（252–505）；
2. 子代理编排类型与池：DelegationLimits/RouterMode/OrchestrationPolicy、FrozenSubagentSlot*、SubagentCandidate*（205–520、1455–1820）；
3. 外部 CLI 适配：CodexSubagentRequest/Outcome、CodexExternalAgentAdapter impl ExternalAgentRunner（1731–1868）；
4. 提示策略 AgentPromptPolicy（787+）；
5. 上下文注入/附件解析 ContextInjectionProfile/ContextSource/ResolvedAttachment（602–780）；
6. plan 原生目录 PlanNativeCatalog*（549–600）；
7. 核心 LlmAgentRuntime + SessionState + impl AgentRuntime（1910–10,612）。

**为什么长成这样**：agent-worker crate 里其余模块（agent_loop 4,266、run_guard 946、delegation_tree 628、cache_shape 470）说明作者有拆分意识，且测试已单独拆到 llm_runtime_tests.rs（9,967 行）——但生产侧"runtime + 它直接需要的每一种策略类型"持续堆在同一文件，因为类型与 runtime 生命周期强绑定，搬动需要动 pub 导出面（lib.rs re-export），惯性下每个新特性都往里加。

**修复方向**：`llm_runtime.rs` 降为 runtime 本体（职责 7）；1 拆 `provider_governor.rs`，2 拆 `subagent_orchestration.rs`（与 delegation_tree.rs 合并考虑），3 拆 `codex_adapter.rs`，4/5/6 各自成文件。lib.rs 的 re-export 面不变则下游（host 的 commands.rs:53-70 等）零改动。

## F-arch-05 codex CLI 生命周期滞留 commands.rs（minor）

**证据**：codex_* 四模块职责清晰（E6），但 `codex_integration_status`(17324)、`codex_cli_preferences`(17585)、`codex_save_cli_preferences`(17705)、`codex_install_cli`(17795，内含 npm 安装进程派生与错误文案)、`codex_sync_cli`(17856)、`codex_install_mcp_server`(18171)、`codex_start_login/start_device_login`(18506/18511)、`codex_install_skill`(25948)、`codex_setup_collaboration`(25956) 全部在 commands.rs；该段（17324–18700）含 312 行 codex 相关命中、9 处进程派生。根因：这些是"IPC 命令实现"，按 F-arch-01 的惯性自然进了上帝模块，而 codex_* 模块是按"运行时交互"边界切的，没给生命周期留位置。修复：迁出为 `codex_cli_lifecycle.rs`，与 codex_* 族并列，commands.rs 留转发或直接由 tauri_commands.rs 指向新模块。

## F-arch-06 「lib 不依赖 tauri」声明失真（minor）

**证据**：`commands.rs:4-6` 文档声明「lib 不依赖 tauri——保持单元测试二进制无 GUI/comctl32 链接」；但 `lib.rs:25` 无条件 `pub mod lifecycle_commands;`，其 `lifecycle_commands.rs:8` `use tauri::{AppHandle, Manager, State};`，且 `Cargo.toml:76` tauri 为 `[dependencies]` 无条件成员——lib 目标实际链接 tauri，单元测试同样带 GUI 链接。根因：lifecycle 命令需要 AppHandle 做关闭门控，放进 lib 最省事；声明写就后无人回头核对。风险在误导后来者按"lib 无 tauri"假设做模块放置决策。修复：更正注释，或把 lifecycle_commands 移到 bin 侧（tauri_commands.rs 同层）恢复声明的真实性。

## F-arch-07 MCP stdio 端点直依赖上帝模块（minor）

**证据**：`mcp_server.rs:20-22` `use crate::commands::{agent_abort, agent_send, session_messages, task_create_with_agent, task_detail, CommandState}`。该端点在 main.rs:277-285 作为独立 stdio 形态先行启动，但其依赖闭包经 commands.rs 拉入终端/附件/回放/供应商目录等全部 30 个模块。非环、无重复实现（协议在本文件、编排委托 commands），属过度耦合：端点只需要 5 个命令。修复：定义窄 trait（如 `AgentEndpoint`）由 CommandState 实现，mcp_server 依赖 trait；这也为 F-arch-01 拆分解除了一处扇入。

## F-arch-08 前端服务端状态访问模式不统一（minor）

**证据**：`store/` 仅 5 文件（app/tasks/toast/companion/sync-health，共 1,533 行）承载共享状态；48 个组件文件直接 `import { 具体命令 } from "../../lib/ipc"`（如 `components/scenes/DashboardScene.tsx:2` 引 permissionApprove/taskDelete/taskRestore）；`ConversationsScene.tsx` 同时用 useTasksStore 读状态与直接调 taskList() 刷新。`lib/ipc.ts` 是 typed 封装且 App.tsx 零直连、仅 5 个 shell/companion 组件用 @tauri-apps 窗口/事件 API——分层底线守住了，不存在"绕过 store 摸裸 invoke"的越权。问题是两套模式并存缺乏约定：哪些命令该进 store（缓存/选择器复用）哪些直调，全凭各场景作者自行判断，长期会分化出重复拉取与不一致刷新。修复：补一页前端数据访问约定（共享态走 store、一次性动作用 ipc），不必然引入新层。

## F-arch-09 provider 域六模块 + 上帝模块入口分散（minor）

**证据**：provider 相关代码分布：`provider_catalog.rs`(2,168 行，域底层)、`provider_models.rs`、`model_capabilities.rs`、`provider_readiness.rs`、`subagent_providers.rs`(2,103 行)、`settings.rs`(1,574 行，provider 持久化部分) ，依赖方向 settings/model_capabilities/provider_models → provider_catalog、subagent_providers → settings（E4，无环、方向清晰）；但面向 IPC 的编排入口（provider_catalog/provider_models/provider_balance/settings_save|select|delete_provider，commands.rs:15039-15497）与子代理池编排（16398-16508）又叠在 commands.rs。分层未破坏，仅域入口分散、单域认知成本高。修复：随 F-arch-01 拆分把该域收拢为一个 provider 命令子模块；不必合并底层六文件。

---

## 汇总

- blocking：0
- major：4（F-arch-01、02、03、04）
- minor：5（F-arch-05、06、07、08、09）

根因主线：`CommandState` + 测试基建沉淀在 commands.rs 形成引力中心，同 crate 内模块互引无机制约束，使「每个新域/新助手就近落位」的局部合理决策累积为双上帝模块 + 双写 SQL + 模块环；crate 级无环（阶段0）成立的原因恰是这些耦合全部留在 host crate 内部。
