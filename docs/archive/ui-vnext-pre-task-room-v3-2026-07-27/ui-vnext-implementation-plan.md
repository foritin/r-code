# UI vNext 实施计划 — fusion-obsidian 落地 + 前端功能补全

> 状态：待实施（2026-07-25 定稿）
> 设计依据：`docs/design/ui-vnext/fusion-obsidian.html`（组合方案二稿）、`skin-adapt.html`（token 分层）、`feasibility.html`（可实现性矩阵）
> 适用范围：`src-tauri/frontend/`（React 18 + Vite + TS + zustand）全面重构 + `src-tauri/src/` 最小后端胶水

---

## 1. 现状盘点（实施前已核实）

### 1.1 前端

- 共 ~414 行骨架代码：`App.tsx` + `Sidebar.tsx` + 4 个 placeholder 视图（Home / TaskRoom / Editor / Settings）。
- IPC 层只有 `ping`（`frontend/src/lib/ipc.ts:19`）；Zustand store 只有视图切换 / 缩放 / Diff 模式。
- 依赖：react 18、@tauri-apps/api v2、zustand 4。**无 xterm、无路由库**。

### 1.2 后端能力（27 个已注册命令，前端 26 个未接）

| 功能域 | 命令 | 前端状态 |
|---|---|---|
| 任务 | `cmd_task_create` / `cmd_task_list` / `cmd_task_detail` | 未接 |
| Agent | `cmd_agent_send` / `cmd_agent_abort` | 未接（且 send 只写 JSONL，不驱动任何 runtime） |
| 权限 | `cmd_permission_approve` / `cmd_permission_pending` | 未接 |
| 变更 | `cmd_changes_list` / `cmd_rollback_file` / `cmd_rollback_task` / `cmd_accept_task` | 未接 |
| 验证 | `cmd_run_verification` / `cmd_verification_list` | 未接 |
| Workspace | `cmd_workspace_list` / `cmd_workspace_open` | 未接 |
| 搜索 | `cmd_quick_open` / `cmd_global_search` | 未接 |
| 终端 | `cmd_terminal_list/create/send/read/kill`（轮询式，ANSI-stripped 全量快照） | 未接 |
| 恢复/支持 | `cmd_recovery_data` / `cmd_support_bundle` | 未接 |
| 设置 | `cmd_settings_get` / `cmd_settings_set` | 未接 |

### 1.3 后端缺口（模块存在但无命令封装）

- **三层回放**：`ReplayService::get_replay(session_id, depth)`（`src-tauri/src/replay.rs:139`）+ `get_evidence`（:320）。
- **项目记忆**：`ProjectMemory::load/save/generate_preamble/sync_to_*`（`project_memory.rs:44-96`）。
- **恢复清理**：`RecoveryManager::cancel_orphaned_permissions`（`recovery.rs:109`）+ 完整版 `recovery_page_data`（:96）。
- **支持包预览**：`SupportBundle::preview`（`support_bundle.rs:103`）。
- **替换预览**：`SearchService::replace_preview`（`search.rs:138`）。
- **终端 wait/resize**：`TerminalControlService::wait`（control_service.rs:159）、`TerminalManager::resize`（manager.rs:288）。
- **上下文引用构造**：`create_file_ref / create_selection_ref / create_external_session_injection`（`commands.rs:782-800`，非命令）。
- **Review 就绪检查**：`can_enter_review` / `check_accept_readiness`（`crates/r-code-store/src/review.rs:101,168`）。

### 1.4 关键架构事实

- **后端零 emit**：全仓无 `app.emit()`，前端只能轮询；hermes-tauri 的 broadcast 通道未桥接到 Tauri 事件系统。
- **Agent runtime 未接线**：`crates/r-code-agent-worker` 只有 `MockAgentRuntime`（确定性场景回放，`mock_runtime.rs:21`）和单次迭代函数 `agent_loop`；**真实 LLM runtime 尚不存在**。`main.rs` 未创建任何 runtime 实例。
- **状态不持久**：`main.rs:29-31` 用 `CommandState::in_memory` + 临时目录，每次启动数据全丢。
- **Control Door** 为 `#[cfg(unix)]`（`control_door.rs:94`），Windows 不编译且未启动。
- Tauri v2 命令参数 camelCase 约定（`taskId` / `projectId` / `pressEnter` / `includeArchived` / `outputDir`）。
- 设置接口会原样返回 `api_key`，前端必须脱敏。

---

## 2. 设计决策

### 2.1 目标方案：fusion-obsidian（组合方案）

三场景职责分明（`fusion-obsidian.html`，IA 决策见 `ui-vnext/index.html:139-144`）：

- **Home** = 主页就是 Chat 输入（默认页，只加一行可点的态势摘要，保持安静）。
- **Deck** = 纯监控新入口（activity 列第五位 ◎，带 NEW 微标；**无任何输入框**；右上 Cards ⇄ Rows 密度切换收纳 E 方向行看板）。
- **Room** = 单会话对话 + 画布 + 底部 mini 时间胶片（细带常驻、hover 升起、可拖动回看）。
- 现有 Sessions / Inbox / Projects / Search 四入口零改动。

### 2.2 主题策略：结构一次写，材质两列值

按 `skin-adapt.html` 的"只写一遍"范式：组件全部引用**语义 token + fx token**，不引用字面量。默认主题 **Obsidian**（dark-first 黑曜石玻璃），同时落地 **Atelier**（light-first 印刷工作室）主题列，Settings 可切换——设计文档里 B⇄C 的"品牌二选一"由此变成运行时切换。

### 2.3 Token 体系（照抄 `skin-adapt.html:21-137`）

语义 token：`--bg-app/panel/card/chip/inset`、`--border/strong`、`--fg/muted/faint`、`--accent/accent-fg`、`--warning/success/danger`、`--font-ui/display/mono`、`--radius-card/chip`、`--shadow-card`。

fx token（材质层全部增量，旧 skin 取空值即安静版）：

```css
--fx-glow-warning / -success / -accent   /* 辉光 ×3 语义色 */
--fx-glass                               /* 玻璃底 */
--fx-blur                                /* 模糊半径 */
--fx-edge                                /* 玻璃描边 */
```

Obsidian 基准值（`fusion-obsidian.html:21-39`）：`--void #020204`、glass-1/2/3 = `rgba(255,255,255,0.028/0.05/0.08)`、`--edge rgba(255,255,255,0.075)`、fg `#ecedf2 / #8b8f9c / #4d515e`、光谱色 `--aura-a #6ee7f2`（青）`--aura-b #8b7cf6`（紫）、语义色 amber `#eebf6d` / green `#5fe3a1` / red `#f0716a`。字体分工铁律：**serif=展示层，sans=UI 层，mono=数据层**。

### 2.4 数据策略

纯轮询（窗口聚焦时 2s，失焦暂停——动画预算红线）+ 仅对 agent 流式输出做 Tauri 事件桥（`agent-event`）。不加新 npm 依赖：终端用 ANSI-stripped `<pre>`（xterm 需后端增量输出配合，留后续）。

### 2.5 工程红线（不因视觉升级而破）

1. 动画预算：仅可见且窗口聚焦时动；`prefers-reduced-motion` 全停；只用 opacity/transform 等合成层安全属性。
2. blur 预算：backdrop-filter 只用于卡片/浮层，Deck 底不用。
3. 无 emoji chrome：图标全内联 SVG，字符符号仅限 ✓✗◆⎇。
4. humane language 文案。
5. 审阅门永远显式：review gate 不可被自动跳过/隐藏。

---

## 3. 实施阶段

### 阶段 1 · 后端胶水（Rust，最小改动）

1. **持久化状态** — `main.rs`：改用 AppData 下 `r-code/`（db/blobs/sessions/config），`CommandState::new` + `Database::open(path)`；`in_memory` 保留给测试。
2. **新增命令封装**（`commands.rs` 增补 + `main.rs` 注册，参数一律 camelCase）：
   - `cmd_session_messages(taskId)` — 读 `{taskId}.jsonl`，返回消息/工具调用块序列（Room 时间线数据源）。
   - `cmd_replay(sessionId, depth)` — 包 `ReplayService::get_replay`，depth: recap/explore/verify。
   - `cmd_memory_get()` / `cmd_memory_set(content)` — 包 `ProjectMemory`。
   - `cmd_recovery_cleanup()` — 包 `cancel_orphaned_permissions`。
   - `cmd_support_preview()` — 包 `SupportBundle::preview`。
   - `cmd_terminal_resize(id, cols, rows)` — 包 `TerminalManager::resize`。
   - `cmd_change_diff(taskId, path)` — blob before/after → 简易 unified diff（blob 层不支持则降级返回元信息）。
3. **Agent runtime 接线**：
   - `CommandState` 增加 `runtime: Mutex<MockAgentRuntime>`。
   - `cmd_agent_send`：写 JSONL（保持现行为）→ `create_session`（若无）→ `start_run` / `steer`；推入确定性场景（文本回复 + 一次工具调用）。
   - 后台 drain 循环：`poll_events` → 写 JSONL + 记 TaskEvent → `app.emit("agent-event", …)`。
   - `cmd_agent_abort` 同时调 `runtime.abort`。
4. 验证：`cargo check -p r-code-host` + 现有 `cargo test` 全绿。

### 阶段 2 · 前端设计系统 + 应用壳

1. **Token 体系**：`styles/tokens.css` + `styles/base.css`；`:root[data-theme="obsidian"]`（默认）与 `[data-theme="atelier"]` 两列值；aurora 背景斑、scrollbar、reduced-motion 全停、字体回退链（Charter→Georgia；Berkeley Mono→SF Mono→Menlo→Consolas，**不打包商业字体**）。
2. **类型 + IPC 层**：`lib/types.ts`（全部 serde 形状，注意枚举大小写）+ `lib/ipc.ts`（30+ typed wrapper）+ `lib/poll.ts`（聚焦感知轮询）+ `lib/keys.ts`（快捷键）+ `lib/format.ts`。
3. **Store**：`store/app.ts`（scene + 当前任务 + 主题 + 缩放）、`store/tasks.ts`（任务缓存 + 轮询 + needs-you 派生）。
4. **应用壳**（`components/shell/`，栅格 `44px / 52px 236px 1fr`，`body[data-scene]` 切换）：
   - `Titlebar`：serif 品牌 + 场景上下文 + Room state-chip + 场景按钮（deck: ＋New session；room: Peek changes / Open review；常显 ⌘E）。
   - `ActivityBar`：品牌 → Home / Deck(NEW) / Sessions / Inbox(amber 徽章) / Projects / Search(⌘K) → Editor(⌘E) / Settings。
   - `Rail`：`Sessions | Files` tab + ⌘K；Needs you 琥珀呼吸条；分组 srow（2.5px 光谱色条、live/sel 变体）；rail-foot 当前 workspace。

### 阶段 3 · Home 场景

serif 40px 标题 + glass chat 盒（focus-within 青边光环）+ meta chips（workspace 选择 / mode ask·edit·auto / model / Advanced）+ 渐变 Launch（`cmd_task_create` → 进 Room 自动发首条消息）+ glance 行（running / need you 计数 + Open Deck →）+ 恢复横幅（`cmd_recovery_data` + 一键清理）。

### 阶段 4 · Deck 场景（纯监控，无输入框）

- **聚合器** `lib/deck.ts`：task_list + 逐任务 detail 派生 gauges（Running / Needs you / Verified today / Files in flight / Accepted per wk）。
- **态势带**：5 gauge（mono 24px tabular-nums，hot/live/good 辉光）+ 舰队示波器（40 柱 `waveB` 呼吸 + 事件率驱动）。
- **Needs You 通道**：权限待批卡（Grant once / Deny → `cmd_permission_approve`）+ review-ready 卡（diffstat + checkrow + Open review / Peek / Rollback）；整卡 `needsPulse 3.2s`。
- **Fleet Cards**（3 列）：项目/worktree chip + 耗时、serif 标题、动作行（verb+target+打字光标）、三段门（Plan→Perms→Verify）、验证行（sweep / det）。
- **Rows 密度**：9 列栅格照抄 `fusion-obsidian.html:368-374`；灯变体、门微条、键帽、`j/k/x/esc`、行尾单键（a/e/g/d/r/p/⏎）、批量条（批量=循环单命令）。
- **Settled strip**：accepted / answered / rolled back 三态。
- Rows 模式隐藏 needs-lane/cards；**不做 Deck 输入框**（一稿遗留死代码，明示跳过）。

### 阶段 5 · Room 场景

- **时间线**：`cmd_session_messages` + task events → 里程碑线 / 用户气泡 / agent 消息（渐变署名）/ plan 卡 / 工具行（active 青边 + spin + `beam` 光带）；元素带 `data-t`（秒）供回放调暗；明示 "mock runtime" 小标。
- **Composer**：回复（`cmd_agent_send` / steer）、abort、`@ attach` 文件引用 chip。
- **画布 tabs**：Summary（状态/elapsed/diffstat/最近事件）、Changes·n（diff 视图 + `.fresh` shimmer + 单文件回滚）、Terminal（轮询读 + 输入注入）、Review（**审阅门永远显式**：跑验证 + 记录列表 + Accept all / Rollback）。
- **Mini reel**：`cmd_replay(recap)` → 章节色带 + 事件刻度 + 旗标 + 琥珀播放头；细带 12px 常驻 / hover 44px；拖拽与 ←→ 步进 → `data-t > cur` 元素 `.dimmed`；非 live 浮出 `Jump to live ⏎`。
- titlebar state-chip 同步 run 态。

### 阶段 6 · 其余场景（设计稿未覆盖，按 token 体系补全）

- **Search**（⌘K overlay）：`cmd_quick_open` 文件区 + `cmd_global_search` 命中区，⏎ 打开。
- **Inbox**：跨项目 needs-you 聚合（复用 ncard），就地操作。
- **Projects**：workspace 卡片（名称/路径/trust_state/last_opened_at）+ 项目记忆查看编辑（`cmd_memory_get/set`）。
- **Editor**（⌘E）：只读文件浏览 + 终端定位；编辑能力留后续里程碑。
- **Settings**：provider 编辑（**api_key 脱敏**）、log_level、缩放 80–200%、accessible diff mode（F7/⇧F7）、主题切换、支持包 preview→导出、skill 安装状态。

### 阶段 7 · 打磨与验证

- 动画参数逐项对规格表（pulse / beam / sweep / freshSweep / blink / needsPulse / drift / waveB / rot），失焦冷却，reduced-motion 全停。
- 无 emoji chrome / humane language 复查。
- `cargo check` + `cargo test`（workspace）+ `npm run build`（tsc + vite）全绿。
- 手测主链路：开 workspace → 建任务 → 发消息（mock 回复 + 工具行）→ Deck 审批 → Room 验证 → Accept → reel 回放。

---

## 4. 明确不做（Out of Scope）

| 项 | 原因 |
|---|---|
| 真实 LLM provider runtime | agent-worker 只有 Mock + agent_loop 单次迭代，真实实现不存在，是独立里程碑 |
| Control Door 前端 | `#[cfg(unix)]`，Windows 不编译且未启动 |
| 外部 CLI 受管/旁观模式 | feasibility 降级显示规则随之暂缓 |
| xterm 全功能终端 | 需后端增量输出/emit 配合 |
| 文件编辑器写入 | 后端无文件读写命令 |
| Deck 底部输入框 | 一稿遗留死代码，设计明示不做 |

## 5. 风险与注意

- Tauri v2 参数 camelCase，wrapper 层统一处理。
- Mock runtime 回复是确定性脚本，UI 必须明示，避免误解为真实模型输出。
- Deck 聚合逐任务拉 detail，任务多时并发限制 + 仅可见场景轮询。
- `cmd_settings_get` 原样返回 api_key，前端全程脱敏。
- 中期可评估：Deck 已含 Needs You 通道 + 徽章，Inbox 入口或可省（`ui-vnext/index.html:143`）。

---

## 附 A · 核心数据结构（serde 形状速查）

```
Task        { id, project_id, title, goal, mode: ask|edit|auto,
              state: idle|exploring|in_progress|review_ready|archived,
              worktree_path?, created_at, updated_at }
TaskDetail  { task, runs[], events[], changes[], permissions[], verifications[] }
AgentRun    { id, task_id, model, review_state: pending|accepted|auto_accepted|rolled_back|answered, ... }
ToolCall    { tool_name, input_json, output_json?, risk_level: R0-R4,
              status: running|ok|error|denied, ... }
PermissionRequest { id, task_id, tool_name, risk_level, input_summary,
              decision: pending|allow|allow_always|deny, ... }
FileChange  { path, change_type: create|modify|delete|rename, before_hash?, after_hash?, ... }
Workspace   { canonical_path, display_name, trust_state: untrusted|trusted, last_opened_at }
TaskEvent   { event_type: task_created|state_changed|run_started|run_ended|tool_call|
              tool_result|permission_requested|permission_decided|file_changed|
              verification_run|system, created_at }
VerificationRecord { command, status: running|passed|failed|superseded|stale|timeout,
              output_blob_key?, exit_code?, ... }
AgentEvent  { type: message|tool_call|tool_result|plan|state, ... }   // serde tag="type"
TerminalInfo { id, state: idle|busy|agent|exited, shell, is_busy }
ReplayEntry { event_type, timestamp, summary,
              evidence_level: verified|recorded|observed|inferred|missing, details? }
```

## 附 B · 动画参数速查（全部合成层安全）

| 名称 | 参数 | 用途 |
|---|---|---|
| `pulse` | opacity .5↔1，1.1–3.6s | 呼吸（灯/门/徽章/辉光/录制点） |
| `needsPulse` | 3.2s box-shadow 呼吸 | Needs You 卡 |
| `blink` | 1.1s steps(1) | 打字光标 |
| `beam` | 1.7s translateX -55%→55% | active 工具行光带 |
| `sweep` | 1.8s left -40%→100% | 验证不确定进度 |
| `freshSweep` | 2.4s ease-out | diff 新行 shimmer |
| `waveB` | 2.6s opacity .5↔1 | 示波器柱 |
| `drift` | 26s alternate | aurora 背景 |
| `rot` | 1s linear | spinner |
