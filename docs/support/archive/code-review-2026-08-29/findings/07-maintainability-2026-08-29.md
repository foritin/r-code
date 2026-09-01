# RV-07 可维护性维度 Findings（2026-08-29）

## 扫描方法与覆盖声明

- 全程只读；唯一临时产物 `$TMP/rv07_longfns.py`（超长函数扫描脚本，跑完删除，未写入仓库）。
- 工具：`rg`（重复实现/死代码/TODO/命名统计）、`git ls-files`/`git check-ignore`/`du`（遗留物清点）、自写 Python 脚本（>150 行函数扫描、未引用 `pub fn` 全量计数、>20 行连续注释块扫描）。
- 覆盖：`crates/**`（6 个 crate）、`src-tauri/src/**`（含 commands.rs 41k 行、llm_runtime.rs 11.9k 行，rg 定位后局部 Read）、`src-tauri/frontend/src/**`（54k LOC TS/TSX）、根目录遗留物（log/目录 11 个）、`.gitignore`/`.git/info/exclude` 交叉验证。`vendor/agent-contracts` 为 submodule，按要求排除。
- 正面结论（无 finding）：全仓 TODO/FIXME/HACK 计数为 **0**（Rust/前端/脚本/docs 均验证）；连续 >20 行注释掉的代码块为 **0**；Cargo feature 仅 `custom-protocol`/`testing` 两个且均被 `cfg` 消费，无孤儿 feature；`hide_background_console` 单源（r-code-core/src/process.rs）被 10 个文件正确复用；前端 IPC 全部收敛在 `lib/ipc.ts`（组件层直接 `invoke()` 计数为 0）；provider 目录单源（`provider_catalog.rs` 30 个 Preset → `cmd_provider_catalog` IPC），`infer_protocol_never_responses` 等处注释明确声明并实际复用单源。README 与 dev.ps1/dev.sh 无漂移（README.md:66-77 引用两脚本，行为一致）。
- 边界声明：模块级架构拆分（commands.rs/llm_runtime.rs 上帝模块）归 RV-02；本报告 F-maint-05 只给**函数级粒度**清单作为互补。

## 汇总表

| ID | 位置 | severity | 根因描述 | 修复方向 |
| --- | --- | --- | --- | --- |
| F-maint-01 | commands.rs:20741 / tools_command.rs:682 / verification.rs:354 | major | 进程树终止逻辑三份独立实现，平台分支语义已出现分叉 | 收敛到 r-code-core/src/process.rs 单一 `kill_tree` |
| F-maint-02 | commands.rs:14888 / llm_runtime.rs:6287 / llm_runtime.rs:362 | major | DeepSeek（及 ark/kimi）厂商身份判定三份实现、两种口径（provider_kind vs 名字清单），一处还是内联 | 身份判定收敛为单一函数/目录字段 |
| F-maint-03 | commands.rs:20576-22417 死代码簇 + replay.rs:461 | major | 11 处 `#[allow(dead_code)]` 中含整段零引用的 Codex exec/app-server 旧实现与 1 处过时标记 | 删除或移入测试模块；回收过时 allow |
| F-maint-04 | 前端 format.ts / i18n/index.ts / 3 个组件本地副本 | major | 时间/时长格式化两套体系并行（7 个函数），一套硬编码中文绕过 i18n | 收敛到 i18n 入口，缩减 i18n-hardcoded-baseline |
| F-maint-05 | 全仓（清单见下） | major | >150 行生产函数 39 个，Top15 达 585-1587 行 | 见 RV-02；本条提供函数级清单优先级 |
| F-maint-06 | CodexModelConfiguration.tsx:240 / ModelSwitcher.tsx:448 | minor | `ConfigRow`/`ConfigBack` 组件逐字复制两份 | 提取到 room/ 共享模块 |
| F-maint-07 | 6 个 crate/src-tauri 共 21 个 pub fn | minor | `pub` 方法全仓零引用（dead_code lint 因 pub 不报警而潜伏） | 删除或标 `#[cfg(test)]` |
| F-maint-08 | SettingsScene.tsx:170 vs provider_catalog.rs | minor | DeepSeek Responses 支持模型集在前端手写，Rust 目录才是权威源 | 能力字段结构化进 catalog DTO |
| F-maint-09 | format.ts:161-202 | minor | `PLAN_TOOL_NAMES` 是 `TOOL_DISPLAY_NAMES` 键清单的手写子集 | 由显示名表派生 |
| F-maint-10 | 根目录/多个目录 | minor | 遗留物：13 个根 *.log、artifacts/ 175 个跟踪产物无归档策略、design-proto/ 零引用、.reasonix/ 只在本地 exclude、target-qa/ 内脚本不可入库 | 清 log、归档策略、补 .gitignore |
| F-maint-11 | 前端全仓 / Rust 注释 | minor | 概念词四用（task/session/run/conversation）、vendor vs provider 混用、`_with_X_and_Y` 复合后缀家族 | 词汇表约定 + 新代码不再增长 |

---

## F-maint-01（major）进程树终止逻辑三份并行实现

三处均实现「Windows `taskkill /PID <pid> /T /F` + `hide_background_console`；Unix 负 PID 组 SIGKILL；兜底 `child.kill()`」：

1. `src-tauri/src/commands.rs:20741-20761` `terminate_codex_child`（TokioCommand，5s timeout，stderr/stdout null）
2. `crates/r-code-gateway/src/tools_command.rs:682-707` `kill_tree`（成功即 return，无 timeout）
3. `crates/r-code-store/src/verification.rs:353-364` 超时分支内联（无 timeout，非 Windows 直接 `child.kill()` 不杀进程组）

已经分叉的具体点：#1 有 5 秒超时防 taskkill 挂起，#2/#3 无；#3 的 Unix 路径不发送负 PID 组信号（仅 `child.kill()`），与 #1/#2 的「杀整棵树」语义不一致——同名场景下行为不同正是三份实现的典型后果。`hide_background_console` 已经在 `crates/r-code-core/src/process.rs:11` 单源共享，终止逻辑没有理由不放在同一处。

**修复方向**：在 `r-code-core/src/process.rs` 增加唯一 `async fn kill_tree(child: &mut tokio::process::Child)`（带 timeout），三处调用方替换。

## F-maint-02（major）厂商身份判定三份实现、两种口径

- `src-tauri/src/commands.rs:14888-14893` `is_deepseek_provider`：按 `provider.provider_kind == "deepseek"`（大小写不敏感）判定。
- `crates/r-code-agent-worker/src/llm_runtime.rs:6287-6292` `is_deepseek_native_provider`：按名字 ∈ {"deepseek","deepseek_responses","deepseek_anthropic"} 判定。
- `crates/r-code-agent-worker/src/llm_runtime.rs:362-366`：reasoning governor 内联第三份 `matches!(provider.as_str(), "deepseek"|"deepseek_responses"|"deepseek_anthropic")`——同文件 6287 行就有现成函数却未复用。

两种口径在「用户把 profile 改名但保留 provider_kind」或「旧配置只有名字没有 provider_kind」时会给出不同答案；provider_catalog.rs 的注释（"只按主机名分类，绝不用显示名猜测 Provider 身份"）表明仓库自己深知这类分叉的危害。同模式还波及 `is_kimi_coding_provider`（commands.rs:14895，kind 口径）与 llm_runtime.rs:352-358 的 `ark_coding|ark_agent|kimi_coding` 名字清单。

**修复方向**：身份判定（名字别名 → canonical kind）收敛到 `provider_catalog.rs`（已有 `infer_legacy_provider_kind`:1274）或 core 层单一函数，两 crate 复用；内联 matches! 一律改调用。

## F-maint-03（major）commands.rs Codex 旧实现死代码簇 + 过时 allow 标记

`rg "allow\(dead_code\)"` 全仓 16 处（清单见 evidence），其中 commands.rs 11 处几乎全是 Codex exec/app-server 早期实现。逐项验证引用（全仓 rg，含测试）：

| 函数/类型 | 位置 | 状态 |
| --- | --- | --- |
| `wait_for_codex_app_server_response` | commands.rs:22316 | **零引用**，完全死 |
| `codex_app_server_thread_id` | commands.rs:22405 | **零引用**，完全死 |
| `codex_app_server_turn_id` | commands.rs:22417 | **零引用**，完全死 |
| `meta_to_summary` | replay.rs:461 | **零引用**，完全死 |
| `write_codex_app_server_value` / `read_bounded_line` / `codex_app_server_startup_progress` | commands.rs:22242/22253/22380 | 仅被上面的死函数引用，传递性死代码 |
| `codex_exec_command_with_permissions` / `CodexLineEvent` / `run_codex_exec_process` / `run_codex_exec_process_with_options` / `run_codex_exec_process_with_options_and_permissions` / `run_codex_exec_process_with_options_and_permissions_and_images` | commands.rs:20576/22126/20857/20877/20901/20926 | 生产零调用（调用点 37730/37792/40523/40569 均在 26169 行 `#[cfg(test)] mod tests` 之后），仅测试使用 |
| `run_codex_app_server_process` | commands.rs:24009 | **标记过时**：生产链活（25111 `spawn_codex_main` → 25037 → 24084 → 24116 反向调用链），allow(dead_code) 应删除 |

现役实现已迁至 `src-tauri/src/codex_app_server.rs`（lib.rs:15 引出）。这批 20576-22417 区间的旧内联实现约 300+ 行属于「迁移后未删的旧版」。其余 5 处 allow（tools_command.rs:231、security.rs:118、rtk.rs:620、user_error_contract.rs:24 均为 `cfg_attr(not(windows), ...)` 合理平台压制）。

**修复方向**：零引用与传递性死的 7 项直接删除；「仅测试活」的 exec 家族移入 `mod tests` 或删除（测试改走现役入口）；24009 的 allow 移除。

## F-maint-04（major）前端时间/时长格式化两套体系并行，一套绕过 i18n

同一「把 RFC3339 变成人类可读」域内有 7 个实现：

**走 i18n 的（`src-tauri/frontend/src/i18n/index.ts`）**：`formatDateTime`:119、`formatNumber`:128、`formatRelativeTime`:132 —— 使用计数各 **1**。
**硬编码中文的（`src-tauri/frontend/src/lib/format.ts`）**：`elapsedSince`:5（18 用）、`elapsedMinutes`:22（14 用）、`relativeAgo`:37（3 用，「刚刚」「3 分钟前」「昨天」）。i18n 函数几乎无人使用，说明 i18n 化改造停滞在入口建好但未迁移的状态。
**组件本地第三份**：`components/room/audit.ts:438 formatDuration`（`${minutes}分${seconds}秒` 硬编码）；`components/settings/ApplicationUpdaterSettings.tsx:17 formatDate` + `:28 Intl.NumberFormat`（逐字近似 i18n 版）；`components/scenes/MemoryPanel.tsx:51 formatTime`（`toLocaleString`）。

仓库已有 `src-tauri/frontend/scripts/i18n-hardcoded-baseline.json` 冻结既有欠账（防新增不防存量），因此本条不是「未知违规」，而是「双体系 + 存量收敛缺路线」：format.ts 的 35 处调用分布在未来任何一次文案调整中都要与 i18n 词典双改。

**修复方向**：format.ts 三个函数内部改调 `i18n/formatRelativeTime`（保留签名不动调用方），本地三副本删除改用 i18n 入口，随迁移缩减 baseline 计数。

## F-maint-05（major）超长函数 Top15（>150 行生产函数 39 个）

扫描方法：自写脚本（fn 声明 + 大括号深度配对，剥离字符串/正则字面量，排除 `#[cfg(test)]` 尾部模块与 `*tests*` 文件）；Top2 与数个边界项已用 `sed`/`awk` 手工复核（run_loop 4375→5941=1567 行、run_child 9271→10368=987 行、Composer 242→1828=1587 行、ProviderSection 862→1864≈1003 行）。JSX 大括号配对有局限，TSX 数字按「函数起点→下一个顶层声明」人工校准。

| # | 文件:行 | 函数 | 行数 |
| --- | --- | --- | --- |
| 1 | src-tauri/frontend/src/components/room/Composer.tsx:242 | Composer | ~1587 |
| 2 | crates/r-code-agent-worker/src/llm_runtime.rs:4375 | run_loop | 1567（精确） |
| 3 | src-tauri/frontend/src/components/companion/CompanionWindow.tsx:443 | CompanionWindow | ~1304 |
| 4 | src-tauri/frontend/src/lib/browser-mock-runtime.ts:2073 | browserMockInvoke | ~1092（mock 设施） |
| 5 | src-tauri/frontend/src/components/scenes/SettingsScene.tsx:862 | ProviderSection | ~1003 |
| 6 | crates/r-code-agent-worker/src/llm_runtime.rs:9271 | run_child | 987（精确） |
| 7 | src-tauri/src/commands.rs:24116 | run_codex_app_server_process_with_images_and_registry | ~823 |
| 8 | src-tauri/frontend/src/components/scenes/HomeScene.tsx:93 | HomeScene | ~818 |
| 9 | src-tauri/src/main.rs:265 | main | ~726 |
| 10 | src-tauri/frontend/src/components/scenes/RoomScene.tsx:104 | RoomScene | ~723 |
| 11 | src-tauri/frontend/src/components/plan/PlanPanel.tsx:359 | PlanPanel | ~649 |
| 12 | src-tauri/frontend/src/components/room/Canvas.tsx:1092 | NormalChangesPanel | ~621 |
| 13 | src-tauri/frontend/src/components/room/Canvas.tsx:2694 | TerminalPanel | ~607 |
| 14 | src-tauri/frontend/src/components/onboarding/OnboardingCampaign.tsx:67 | OnboardingCampaign | ~585 |
| 15 | src-tauri/frontend/src/components/scenes/SubagentProvidersPanel.tsx:187 | SubagentProvidersPanel | ~585 |

完整 39 项清单见 evidence。Rust 侧另有 `persist_runtime_event`（commands.rs:5738，543 行）、`spawn_with_run_id`（llm_runtime.rs:8604，399 行）、`handle_plan_subagents`（llm_runtime.rs:7974，319 行）、`dispatch_next_queued`（commands.rs:9110，321 行）。模块级拆分归 RV-02，本清单可直接作为其切割顺序输入（建议从纯前端大组件与 `main.rs:265` 这类低风险项起步）。

## F-maint-06（minor）ConfigRow/ConfigBack 组件逐字复制

`components/room/CodexModelConfiguration.tsx:240-254` 与 `components/room/ModelSwitcher.tsx:448-462`：`ConfigRow`、`ConfigBack` 两组件签名、JSX 结构、className（`model-config-row ring-inset`/`model-config-back ring-inset`）逐字相同。任何样式或 a11y 调整需双改。

**修复方向**：提取到 `components/room/model.ts` 旁或独立小模块，两处导入。

## F-maint-07（minor）21 个全仓零引用的 pub 方法

脚本全量比对 1259 个 `pub fn` 名字的文本出现次数（含测试文件），扣除仅定义项后剩 21 个（清单见 evidence）。抽样复核 `disarm`（plan_policy.rs:354，`SuggestionGate::disarm` 与同文件 `armed()` 并存但无人调用）、`quiet_reason_for`（plan_policy.rs:322）、git_service.rs 的 7 个（`apply_cached_patch`/`has_conflict`/`has_staged_change`/`has_worktree_change`/`intent_to_add`/`is_indexed`/`worktree_patch`）。`pub` 方法不触发 dead_code lint，故可长期潜伏。其中 `testing.rs` 2 项位于 `#[cfg(any(test, feature = "testing"))]` 模块且该 feature 无外部启用者，属测试设施残留。

**修复方向**：逐个确认后删除；确为测试辅助的标 `#[cfg(test)]`。

## F-maint-08（minor）DeepSeek Responses 模型集跨侧手写

`src-tauri/frontend/src/components/scenes/SettingsScene.tsx:170`：`const DEEPSEEK_RESPONSES_MODELS = new Set(["deepseek-v4-flash", "deepseek-v4-pro"])`，用于 1271 行的 Responses 协议可用性判断。权威源在 `src-tauri/src/provider_catalog.rs:372-410`（deepseek Preset 的 `native: P_CR` + models 清单 + note 文案），且 `cmd_provider_catalog` IPC 已把 `presetModels` 传到前端（provider.ts:27）。DeepSeek 再开放新模型支持 Responses 时（07-31 就发生过一次），只有改前端这份手写集才生效，后端目录与前端判断会静默分叉。

**修复方向**：在 PresetModel/catalog DTO 增加结构化能力字段（如 `protocols`），前端删掉手写 Set。

## F-maint-09（minor）PLAN_TOOL_NAMES 键清单重复

`src-tauri/frontend/src/lib/format.ts:161-177` `TOOL_DISPLAY_NAMES`（16 键）与 `:191-202` `PLAN_TOOL_NAMES`（10 键）——后者是前者键的子集手抄。新增 plan_* 工具时漏改任一边会导致归组/文案不一致。

**修复方向**：`PLAN_TOOL_NAMES` 由 `Object.keys(TOOL_DISPLAY_NAMES).filter(k => k.startsWith("plan_"))` 派生（或反向单源声明）。

## F-maint-10（minor）仓库遗留物清点

| 项 | 状态 | 判定 |
| --- | --- | --- |
| 根目录 13 个 `*.log`（ci-clippy2/ci-frontend-meta/ci-ubuntu-test/clippy-linux/clippy-run1-6/fix-verify/plan-eval-test/rust-tests，合计约 850KB，最大 ci-ubuntu-test.log 420KB） | 未跟踪，`.gitignore` `*.log` 已覆盖 | 本地运行残留，可直接删除 |
| `artifacts/` 175 个跟踪文件：94 个 metrics JSON（文件名含 git sha + 机器名，如 `report-69ab…-windows-m0-01.json`）+ 71 个 ai-tasks YAML + `current.yaml` 活跃状态 + png | **被 git 跟踪** | metrics 报告按 sha/机器累积无归档策略，仓库只增不减；`.gitignore` 仅排除其中的 `.pass-cache/`。建议约定「基线保留、过程报告归档/删除」 |
| `design-proto/` 2 张 png（settings-card-lite/settings-flat） | 被跟踪 | 全仓零引用（rg 无命中），且 docs/product-experience-redesign/ 已有同类图。建议删除或并入 docs 归档 |
| `target-qa/` 2.6G | 未跟踪，gitignore 覆盖 | 内含 `room-dom-probe.mjs`、`room-todo-probe.mjs`、`room-parity/*.png` 等探针脚本/基线图——**放在被 ignore 的目录里不会进 git**，换机即丢。应移到 scripts/ 或 fixtures/ 并在文档注明 |
| `.reasonix/` 318K | 未跟踪，靠 `.git/info/exclude:7` 忽略 | **未写进 `.gitignore`**（`.reference/` 写了、`.reasonix/` 漏了），其他克隆会显示 untracked 噪音。补一行 `/.reasonix/` |
| `sandbox/` 422M | 未跟踪，gitignore 覆盖 | 本地 agent scratch，合规 |
| `eval/`（125 文件）、`fixtures/`（6 文件） | 被跟踪 | 语料库/合同 fixtures，属测试资产，保留 |
| `loop.run.yaml`、`.tauri/`（密钥）、`.zcode/` | 未跟踪，gitignore 覆盖 | 合规 |
| `dev.ps1`/`dev.sh` vs README | — | 无漂移（正面确认） |

## F-maint-11（minor）命名一致性抽样

- **概念词四用**（前端，rg -ci 计数）：`task` 2028 / `session` 539 / `run` 363 / `conversation` 108。分层本身有语义（conversation=UI 概念 → task=持久实体 → run=执行实例 → session=Codex replay），但交界处已见模糊样本：`useCreateConversation.ts:28` 创建 "conversation" 实际调 `taskCreate`（IPC `cmd_task_create`），标题写 "新对话"。建议在 AGENTS.md 或 docs 写明四层词汇表，新代码按层选词。
- **vendor vs provider**：Rust 注释/标识符 21 处 `vendor`（如 agent_loop.rs:842-843、llm_runtime.rs:1679 "vendor 连接层"）与 145 处 `provider_kind` 并存；前端 types.ts:1233 注释也写 "catalog/vendor identity"。同一概念两个词，建议注释统一为 provider（`vendor/` 目录名除外，那是 submodule 路径）。
- **复合后缀命名家族**：`_and_images`(9)/`_and_permissions`(7)/`_and_registry`(11) 组合出 `run_codex_exec_process_with_options_and_permissions_and_images`（commands.rs:20926，5 个修饰词）这类名字。参数组合式命名正是 F-maint-03 死代码簇的温床（每加一个选项就复制一个变体）。修复方向：改 options struct + builder，停止新增后缀变体。

## 附：正面观察（供修复阶段避免误伤）

- `provider.ts` 头部注释明确记录了它就是为消除 HomeScene/RoomScene/Composer 三处复制而建，且现状确实单源。
- `task-status-projection.ts`、`provider-health.ts` 声明并保持「唯一投影源」地位。
- `infer_protocol_never_responses`（commands.rs:14736）注释强调「唯一实现」且属实。
- 死代码面（16 处 allow、21 个未引用 pub fn）相对 12 万+ 行 Rust 体量属低位；问题集中在 commands.rs 的 Codex 旧实现这一单个簇。
