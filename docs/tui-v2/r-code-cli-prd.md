# R-Code CLI（TUI v2）PRD / AI 实施清单

> 文档状态：`frozen`（只表示执行合同已完整、通过文档门禁，不表示产品功能已经实现）
> 执行合同：`prd-to-ai-worklist` v1.1.0
> 取证基线：2026-09-02；调研与选型依据 [`pi-tui-deep-research.md`](./pi-tui-deep-research.md)（pi / Claude Code / Codex CLI 三方对照，§10 codex 快照级取证）；视觉 ground truth：[`tui-v4-prototype.html`](./tui-v4-prototype.html)（codex 风，唯一保留原型）；R-Code 现状来自源码逐条核查（见 §3）
> 固化清单：`docs/tui-v2/tui-v2-freeze.yaml`（M0-01 交付）
> 唯一完成状态：本文 §8 主 Checklist；任务卡、任务包与证据不得维护第二套 Checkbox
> 前置文档：`docs/support/archive/pi-alignment/`（v1 PRD，28/28 完成，已归档——本清单是其 M8 TUI 能力的迭代续篇，不重做宿主层）

## 执行导航

- 首次执行：§0 → §2 → §4 → §7 → §8 → 首个 ready 任务卡（§9）。
- 中断恢复：`artifacts/ai-tasks/tui-v2/current.yaml` → §9 对应任务卡 → `artifacts/ai-tasks/evidence/tui-v2/`。
- 判断完成：§7 统一 Harness → §8 唯一 Checklist 勾选 → `artifacts/ai-tasks/verification/tui-v2/implementation/`。
- 产品终态与非目标：§1。
- 不可变决策（红线、键位、色彩、视觉基准）：§2。
- 机器合同与规范性需求：§4。
- 验收与里程碑：§7。

## 0. AI 执行入口

<!-- AI_WORKLIST_VOLATILE_START -->

- 当前进度：`11 / 24` 项完成（M0/M1/M2 三个里程碑全部收口）。
- 下一执行项：M3-01（footer 统计投影）。
- 当前任务包：`artifacts/ai-tasks/tui-v2/current.yaml`（M1-01 进行中时建立；根 `current.yaml` 与 `artifacts/ai-tasks/pi-alignment/current.yaml` 属于其他 worklist 的活跃资产，不得占用或覆盖）。
- 注意：工作区可能存在未提交改动（含 `docs/tui-v2/` 下文档与原型），一律视为用户资产，任何任务不得 reset/覆盖/回滚它们。

<!-- AI_WORKLIST_VOLATILE_END -->

### 0.1 首次启动

1. 只读检查 Git revision、完整 worktree、Rust 运行时、`cargo test -p r-code-tui` 与 `node --test scripts/release.test.mjs` 基线；已有未提交改动一律视为用户资产。
2. 读取本节、§2、§4、§7、§8 和首个 ready 任务卡（M0-01），不需要每轮重读全文。
3. 从编号最小且依赖已通过的未完成 MUST 任务开始；建立 `current.yaml` 后直接进入实现，不在里程碑边界等待人工确认。
4. 每个可验证子步更新任务包；断言和累计门禁均通过、证据真实存在后，才能勾选 §8 中唯一 Checkbox。

### 0.2 续跑

1. 读取 `current.yaml`、对应任务卡和已归档证据，不重做全文规划。
2. 核对真实工作区与 `changed_paths`，把用户新增改动视为资产。
3. 对 `completed_assertions` 跑最小 smoke，确认未失效。
4. 从 `remaining_assertions` 和首个未完成 step 继续。
5. packet 与代码不一致时以可复核事实为准并修正 packet，不凭 packet 伪造完成。

### 0.3 授权与中断边界

- 允许中断：需要扩张用户未授权范围/权限/外部副作用、需要无法从安全存储取得的密钥或付费资源、即将执行不可逆生产操作且授权不明确、两条同优先级 MUST 不可兼容、关键依赖损坏且受约束替代与重试均无路径。
- 不是中断理由：到达里程碑、切换渲染/测试阶段、组件命名/文件组织等可逆偏好、首轮测试或 lint 失败、缺少真实模型但 fake/local profile 足以验收实现。
- 外部放行项（见 §11.3）不阻塞可离线完成的实现；实现侧做到 `implementation_verified` 即视为本清单完成终点。

<!-- AI_WORKLIST_NORMATIVE_START -->

## 1. 背景、目标、终态与非目标

### 1.1 背景

v1（pi-alignment M8）交付了 `r-code-tui` 骨架：独立 bin、Host 编排复用、共享 data-dir、print/json 非交互模式。但 [`pi-tui-deep-research.md`](./pi-tui-deep-research.md) §4 差距清单判定：协议层已对齐（`InferenceOptions` 与 pi `ThinkingLevel` 逐值一致），表现层是空白——TUI 跑在隐式 Mock 演示场景上、thinking 硬编码 disabled、无模型切换、无 footer 状态、键盘事件双写、历史锁在 alt-screen。本清单把 TUI 从"演示骨架"升级为可日常使用的 **r-code cli** 终端客户端，视觉与交互基准定案为 codex CLI 风（v4 原型）。

### 1.2 Definition of Done

终态必须是可观察的系统状态：

1. **真实化**：交互/print/json 三模式装配即真实 provider（`enable_real_agent_mode`）；无 `--mock` 显式旗标时 mock 演示场景不可达；无 provider 配置时首屏/输出为显式引导（指向桌面设置页与 config 路径），不降级、不回放演示。
2. **交互三角**：`/model` 模型弹层、思考级别弹层与 `alt+,`/`alt+.` 升降（写 `task_set_inference`，per-task 记忆）、`Shift+Tab` TaskMode 循环；footer 右侧 `(provider) model • thinking` 常驻。
3. **状态呈现**：footer 统计（token/成本/上下文百分比/compaction 标记 + 变色阈值）、`/status` 卡、`/usage`。
4. **编辑器与命令面**：多行编辑（undo/词导航/CJK 折行）、粘贴折叠、Ctrl+G 外编、斜杠菜单、`?` 面板、`!` bash 直通、`@` 文件提及、历史导航、Ctrl+T transcript 浮层。
5. **审批**：codex 风内联审批浮层（编号选项、`›` cyan bold 选中、y/a/esc、前缀放行注记），决策经宿主 PermissionEngine 同源落账。
6. **inline 渲染**：历史进终端 scrollback、行差分重绘 + CSI ?2026 同步输出、编辑器/footer 贴底；无独立 fullscreen 模式，全屏语义由 transcript 浮层承担。
7. **会话**：`/resume` 列表（共享 data-dir）、`/new` `/rename` `/compact`。
8. **收口**：bin 命名决策落地并同步分发/脚本/文档；累计门禁 `--through M6 --profile implementation` 返回 0，全部 required 断言有真实证据。

### 1.3 非目标

- 不做独立 fullscreen/alt-screen 双模式切换（2026-09-02 拍板废弃 F10 方案）。
- 不复刻 WebView 桌面场景（工作台、Plan 面板、记忆管理页等 GUI 重交互）。
- 不自研 TUI 框架（继续 ratatui + crossterm；inline 路线按 M5-01 PoC 定案）。
- 不做终端图片渲染（Kitty/iTerm2 图形协议；文本占位即可）。
- 不做 pi 式扩展系统、主题文件热重载、`keybindings.json` 用户自定义（一期键位硬编码于 §4.2 键位表；/theme 后置）。
- 不做 RPC server / 远程会话客户端。
- 不改宿主层 Agent 循环语义（`AgentEvent` 唯一事件源；编排经 `r_code_host::commands` 既有入口）。
- 不做 vim 模式、readline flavor、历史反搜 `Ctrl+R`（可后置为 MAY）。

## 2. 已冻结决策

1. **产品定位（R3）**：`r-code-tui` 即 **r-code cli** 本体，`--mode tui|print|json` 三形态；是否 bin 别名 `r-code` 由 M6-03 按分发影响定案并记录，不新增第二 CLI 入口。
2. **红线 R1 禁 mock**：交互/print/json 装配即 `enable_real_agent_mode()`；`install_mock_scenario` 仅 `--mock` 显式旗标（评估/演示线路）可达；`push_demo_scenario` 的隐式调用点（`src-tauri/src/commands.rs:14229`）不得被 TUI 生产路径触达。
3. **红线 R2 显式失败**：provider 不可用/未配置必须显式报错 + 可操作指引（配置文件绝对路径、桌面设置页途径），禁止静默降级。
4. **渲染形态（R3 二次拍板 2026-09-02）**：默认且唯一交互形态 = inline 滚动式（pi regular / claude code / codex 同款语义）；**无独立 fullscreen 模式**，"全屏"语义由 Ctrl+T transcript 浮层覆盖。现有 alt-screen 路径在 M5 退役。
5. **视觉基准**：[`tui-v4-prototype.html`](./tui-v4-prototype.html)（codex 风）是唯一视觉 ground truth；调研报告 §10 的 codex insta 快照事实为规范依据。消息符号系统：用户消息 `›` + 整段背景带；助手消息 `•` + markdown 无名签；执行单元 `• Ran/Running`（成功绿 bold / 失败红 bold）+ `  └ ` dim 输出；编辑单元 `• Edited N files (+a -d)`；排队 `• Queued follow-up inputs` + `  ↳`；composer 无边框背景带；会话头圆角框 ≤56 内宽。
6. **键位表（冻结）**：`Shift+Tab`=TaskMode 循环（ask→edit→auto→plan，宿主枚举序）；`alt+T`=思考级别弹层；`alt+,`/`alt+.`=思考降/升；`/model`=模型弹层（fuzzy + provider 分组）；`Enter`=发送，运行中=排队 follow-up；`Esc`=中止/关浮层/拒绝审批；`Ctrl+C`=清空输入，空输入二次=退出；`Ctrl+D`=空输入退出；`Ctrl+T`=transcript 浮层；`Ctrl+G`=外部编辑器；`?`=快捷键面板（空输入时）；`/`=斜杠菜单（输入起始）；`!`=bash 直通（输入起始）；`@`=文件提及补全；审批 `y`/`a`/`esc`。pi 式 `Ctrl+P` 模型循环**不采用**（让位历史导航）。
7. **色彩语义（冻结，调研 §10.10）**：cyan=强调/选中/链接/代码；绿=成功；红=失败/删除；magenta=模式态（Plan 等）；dim=一切辅助信息；bold=标题；italic=推理/原因/空态；无品牌橙（唯一例外：max effort 态 `›` 金黄）。阈值变色：上下文 >70% warning、>90% error。
8. **CJK 宽度**：字符宽度一律 vw=2（原型 IAB 字体 1.77× 是已知环境边界，不作为实现依据）。
9. **复用不重做**：编排经 `r_code_host::commands`（`task_create`/`task_set_inference`/`agent_send`/`agent_abort`/`task_detail`/`enable_real_agent_mode`）；排队用宿主 `AgentSendMode::Queue`；审批决策经既有 `ApprovalDecision` → PermissionEngine；存储 JSONL+SQLite 不动；共享 data-dir 与桌面互通不变。
10. **实现完成 vs 外部放行分层**：真实 provider 连通复测、Windows 终端/IME 真机、跨终端色彩一致性属 `production_release_ready`（§11.3），不阻塞 `implementation_verified`。

## 3. 仓库事实表

| 事实 | 证据 |
| --- | --- |
| TUI crate 结构 | `crates/r-code-tui/src/`：`main.rs`（装配/参数）、`app.rs`（渲染循环）、`input.rs`（`map_key` 键位归一）、`interaction.rs`/`approval.rs`（审批卡）、`bang_command.rs`（`!` 直通雏形）、`ime.rs`、`fullscreen.rs`、`lib.rs`（`TuiState`/`TranscriptRow`/`EventBridge`） |
| thinking 硬编码 | `crates/r-code-tui/src/main.rs:123`、`main.rs:231`：`thinking: Some("disabled")`，交互与非交互两处 |
| 装配未真实化 | `main.rs` 装配未调 `enable_real_agent_mode`（定义于 `src-tauri/src/commands.rs:1369`；`commands.rs:32043` 注释明确"生产路径由 bin 侧 enable_real_agent_mode 打开；测试默认 Mock"） |
| mock 注入点 | `install_mock_scenario`：`commands.rs:4096`；`push_demo_scenario`：`commands.rs:5339`，隐式调用点 `commands.rs:14229`；TUI print 模式经 `main.rs:241` 主动安装 mock 场景 |
| 键盘双写 bug | `crates/r-code-tui/src/app.rs:81`：`event::read()` 后未过滤 `KeyEventKind`，Windows 下 Press+Release 双写 |
| 键位归一层 | `crates/r-code-tui/src/input.rs:113` `map_key(KeyEvent) -> KeyAction`，已有单测（`:139`） |
| TaskMode 枚举 | `crates/r-code-core/src/dto.rs:129`：`Ask/Edit/Auto/Plan`（产品语义：Ask/Edit/Auto 统称 Agent，Plan 为另一交互模式）；`task_create(state, workspace_path, title, goal, mode)`（`commands.rs:1930`），TUI 现建死 `"ask"`（`main.rs` 两处调用） |
| 发送模式枚举 | `crates/r-code-core/src/dto.rs` `AgentSendMode`：`Auto/Steer/Queue/SendNow`——宿主已有持久化排队语义；TUI 现用默认 Auto（运行中=steer） |
| 审批既有面 | `crates/r-code-tui/src/approval.rs`：`ApprovalDecision { Approve, ApproveAlways, Deny }`，经宿主 PermissionEngine 落账（standing rule） |
| usage 投影 | `src-tauri/src/commands.rs:23706` `safe_usage.total` 等已有投影；footer 数据源无需新建 |
| InferenceOptions | `vendor/agent-contracts/crates/agent-contract/src/provider.rs:46`：`{ thinking, reasoning_effort, verbosity }`；`reasoning_effort` 枚举 `none/minimal/low/medium/high/xhigh/max` 与 pi 逐值一致 |
| per-task 推理记忆 | `task_set_inference`（`r_code_host::commands` 导出，TUI 已调用）持久化于任务 |
| 渲染现状 | `main.rs` `run_interactive_tui`：EnterAlternateScreen + ratatui 全屏循环；`fullscreen.rs` 存在 alt-screen 辅助 |
| provider 目录 | `src-tauri/src/provider_catalog.rs`（30 个 Preset）；config providers 经共享 data-dir `config/` |
| 启动脚本 | `dev-tui.ps1` / `dev-tui.sh`：工具链 + 子模块同步 + cargo run，显式 Dev data-dir；`scripts/release.test.mjs` 已有 TUI 启动脚本 Dev 命名空间漂移断言 |
| 验收 Harness 形状 | `scripts/verify-r-code-alignment.mjs`：REGISTRY + `--task/--through/--profile`，exit 0/1/2，报告 `artifacts/ai-tasks/verification/<project>/<profile>/`；文档门禁 `scripts/verify-ai-worklist.mjs --mode compute/check`（markers：`AI_WORKLIST_VOLATILE/NORMATIVE/CONTRACT`）；模板 `artifacts/ai-tasks/templates/{current-task,task-evidence}.template.yaml` |
| 质量门 | `docs/architecture.md §14`：`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace --all-features`、前端 `npm test`+`npm run build`（本清单原则上不动前端） |
| 既有 worklist 资产 | `artifacts/ai-tasks/pi-alignment/`（28/28 完成）与其他活跃 `current.yaml`；本清单项目命名空间 = `tui-v2` |

## 4. 机器合同与规范性需求

### 4.1 规范性需求登记

- **R-GEN-01（MUST）**：统一非交互验收 Harness（`scripts/verify-tui-v2.mjs`）支持 `--task <TASK_ID>`、`--through <MILESTONE_ID>`、`--profile implementation|production`；0 仅表示全部 required assertion 通过；输出 `artifacts/ai-tasks/verification/tui-v2/<profile>/<task-or-milestone>.json` 与日志；记录 revision/worktree digest 与失败断言列表；required fixture/metric 缺失视为失败。
- **R-GEN-02（MUST）**：改造前回归基线登记（r-code-tui 测试、clippy、release.test.mjs、print 冒烟形态对照）且在累计门禁中持续可复跑。
- **R-REAL-01（MUST）**：交互/print/json 三模式装配即真实 provider（`enable_real_agent_mode`）；无 `--mock` 显式旗标时 mock 演示场景不可达；交互模式不接受 `--mock`。
- **R-REAL-02（MUST）**：print/json 无 provider 配置时显式报错引导（exit≠0 + config 绝对路径 + 桌面设置页途径），不降级、不回放演示。
- **R-REAL-03（MUST）**：键盘事件单次性——仅 `KeyEventKind::Press`（及明确的 Repeat 语义）产生 `KeyAction`，Windows 双写消除。
- **R-REAL-04（MUST）**：发送/运行错误进 transcript（`TranscriptRow::System` 行）；`eprintln!` 不得作为用户可见错误通道（alt-screen 下不可见）。
- **R-REAL-05（MUST）**：provider 不可用 → 可操作指引（config 绝对路径 + 桌面设置页途径）。
- **R-REAL-06（MUST）**：无 provider 配置时 TUI 首屏为引导卡且不可发送；配置就位后引导消失。
- **R-MODEL-01（MUST）**：`/model` 弹层（fuzzy + provider 分组 + 预选当前 + 选中写 task + footer 右侧联动）。
- **R-THINK-01（MUST）**：思考级别弹层 + `alt+,`/`alt+.` 升降 + per-task 记忆（`task_set_inference`）+ 能力 clamp + footer `• thinking` 联动。
- **R-MODE-01（MUST）**：`Shift+Tab` 循环 TaskMode（ask→edit→auto→plan）+ 输入区模式态呈现 + 写回 task。
- **R-QUEUE-01（MUST）**：运行中 Enter = 排队（宿主 `AgentSendMode::Queue`）+ `• Queued follow-up inputs`/`↳` 显示 + 中止后宿主派发语义。
- **R-APPR-01（MUST）**：codex 风内联审批浮层（带面、编号选项、`›` cyan bold、y/a/esc、前缀放行注记），决策经 `ApprovalDecision` → PermissionEngine 同源落账。
- **R-STAT-01（MUST）**：footer 统计（token/成本/上下文百分比/compaction 标记）来自会话 usage 投影 + >70%/90% 阈值变色。
- **R-STAT-02（MUST）**：`/status` 状态卡 + `/usage` 成本汇总 + footer 右侧 context 余量。
- **R-EDIT-01（MUST）**：多行编辑器内核（显式换行、undo/redo、词导航、CJK/grapheme 安全折行、光标完备）。
- **R-EDIT-02（MUST）**：粘贴折叠（>1000 字符 `[Pasted Content N chars]`）+ Ctrl+G 外部编辑器（`$VISUAL/$EDITOR`）回填，折叠原文不丢失。
- **R-CMD-01（MUST）**：斜杠命令菜单（上方插入式、fuzzy、Tab 补全、no matches dim italic）+ 空输入 `?` 两列快捷键面板 + 一期命令集注册表。
- **R-SHELL-01（MUST）**：`!` bash 直通（经宿主 shell 链、输出进 transcript dim）+ `@` 文件提及补全。
- **R-HIST-01（MUST）**：历史导航（↑/↓、Ctrl+P/N）+ Ctrl+T transcript 浮层（全屏语义载体，q/esc 关闭、滚动）。
- **R-INL-01（MUST）**：inline 渲染路线 PoC 定案（ratatui InlineViewport vs 自研行差分，带可复跑基准数据与决策记录）。
- **R-INL-02（MUST）**：inline 落地——历史进终端 scrollback、行差分重绘 + CSI ?2026 同步输出、编辑器/footer 贴底、M1–M4 交互回归。
- **R-INL-03（MUST）**：alt-screen 独立路径退役（主路径无 EnterAlternateScreen）+ IME 假光标 inline 适配。
- **R-SESS-01（MUST）**：`/resume` 会话列表（共享 data-dir、`❯` 光标、双行行目、排序、enter 接续）。
- **R-SESS-02（MUST）**：`/new` `/rename`（持久化 + footer 更新）`/compact`（宿主压缩入口）。
- **R-SHIP-01（MUST）**：bin 命名决策落地（含分发影响分析）+ 脚本/文档/externalBin/断言四面一致 + `--through M6` 累计门禁 exit 0。
- **R-NICE-01（MAY）**：Emacs kill-ring、Ctrl+R 反搜、vim 模式、`/theme`（后置，不建任务卡；升级为 MUST 需按解冻条件走 §12 流程）。

### 4.2 关键机器合同

- **MC-1 装配契约**：TUI `main` 装配序列 = `shared_state` → `enable_real_agent_mode()` → 事件桥；`--mock` 旗标存在且仅存在于评估线路（print/json 演示），交互模式不接受 `--mock`。
- **MC-2 键位表**：见 §2.6，唯一键位事实源；`input.rs::map_key` 为归一层，键位单测以表为准。
- **MC-3 推理契约**：思考级别 = `InferenceOptions.reasoning_effort` 七档；thinking 字段随级别映射（off 档 → `thinking: disabled`）；能力 clamp 依据 provider 目录声明。
- **MC-4 transcript 投影**：`AgentEvent` → `TranscriptRow` 单向投影；渲染层不持有权威状态（snapshot 权威经 JSONL+SQLite，v1 既定）。
- **MC-5 审批契约**：`y`=`ApprovalDecision::Approve`、`a`=`ApproveAlways`（会话级/前缀 standing rule，语义对齐 codex `(p)` 前缀放行并出注记）、`esc`=`Deny`；浮层内联于底部面板，不占全屏。
- **MC-6 Harness 接口**：`node scripts/verify-tui-v2.mjs --task <ID>|--through <M> --profile implementation|production`；exit 0/1/2 语义与报告路径同 v1 Harness（`artifacts/ai-tasks/verification/tui-v2/<profile>/`）。
- **MC-7 色彩语义**：§2.7 表；实现以具名语义色（`accent`/`success`/`error`/`mode`/`dim`）为唯一取色入口，禁止散落裸色值。
- **MC-8 宽度契约**：所有行渲染经统一宽度核算函数（CJK=2 列）；框/带/对齐不允许手写空格对齐。
- **MC-9 data-dir**：默认解析 = 宿主 `AppFlavor` 同源规则；dev 脚本显式传 Dev 命名空间（`release.test.mjs` 漂移断言守护）。

## 5. 质量、性能与安全门禁

- 每任务：`cargo fmt --all -- --check`、`cargo clippy -p r-code-tui --all-targets -- -D warnings`（触及宿主则扩到 workspace）、新增单测全绿。
- 行为护栏：`cargo test -p r-code-tui` 既有测试不得删除/缩小；`r_code_host::commands` 公开面不得为 TUI 破坏既有调用方（桌面 IPC 回归）。
- 渲染性能预算（M5）：单帧重绘 ≤16ms（差分路径）、全量重绘仅允许宽度变化/清屏触发；预算在 PoC 基准中断言。
- 安全不变量：审批/权限语义全部经宿主 PermissionEngine；TUI 侧只产生意图（v1 `approval.rs` 既定，不降级）；`!` 直通经宿主 shell 执行链（bang_command 既有链路），不新开裸 `std::process` 通道。
- 反作弊：required 断言缺失=失败；不得删测试/降阈值/改 fixture 真值修绿；fake/local 结果带 profile 标签。

## 6. 需求追踪表

| 需求 | 任务 | 验收断言 |
| --- | --- | --- |
| R-GEN-01 | M0-01 | `M0-01.A1`、`M0-01.A2`、`M0-01.A3` |
| R-GEN-02 | M0-02 | `M0-02.A1`、`M0-02.A2`、`M0-02.A3`、`M0-02.A4` |
| R-REAL-01 | M1-01 | `M1-01.A1`、`M1-01.A2`、`M1-01.A4` |
| R-REAL-02 | M1-01 | `M1-01.A3` |
| R-REAL-03 | M1-02 | `M1-02.A1`、`M1-02.A2` |
| R-REAL-04 | M1-03 | `M1-03.A1`、`M1-03.A3` |
| R-REAL-05 | M1-03 | `M1-03.A2` |
| R-REAL-06 | M1-04 | `M1-04.A1`、`M1-04.A2` |
| R-MODEL-01 | M2-01 | `M2-01.A1`、`M2-01.A2`、`M2-01.A3` |
| R-THINK-01 | M2-02 | `M2-02.A1`、`M2-02.A2`、`M2-02.A3`、`M2-02.A4` |
| R-MODE-01 | M2-03 | `M2-03.A1`、`M2-03.A2`、`M2-03.A3` |
| R-QUEUE-01 | M2-04 | `M2-04.A1`、`M2-04.A2`、`M2-04.A3` |
| R-APPR-01 | M2-05 | `M2-05.A1`、`M2-05.A2`、`M2-05.A3`、`M2-05.A4` |
| R-STAT-01 | M3-01 | `M3-01.A1`、`M3-01.A2`、`M3-01.A3` |
| R-STAT-02 | M3-02 | `M3-02.A1`、`M3-02.A2`、`M3-02.A3` |
| R-EDIT-01 | M4-01 | `M4-01.A1`、`M4-01.A2`、`M4-01.A3`、`M4-01.A4` |
| R-EDIT-02 | M4-02 | `M4-02.A1`、`M4-02.A2`、`M4-02.A3` |
| R-CMD-01 | M4-03 | `M4-03.A1`、`M4-03.A2`、`M4-03.A3`、`M4-03.A4` |
| R-SHELL-01 | M4-04 | `M4-04.A1`、`M4-04.A2`、`M4-04.A3` |
| R-HIST-01 | M4-05 | `M4-05.A1`、`M4-05.A2`、`M4-05.A3`、`M4-05.A4` |
| R-INL-01 | M5-01 | `M5-01.A1`、`M5-01.A2`、`M5-01.A3` |
| R-INL-02 | M5-02 | `M5-02.A1`、`M5-02.A2`、`M5-02.A3`、`M5-02.A4` |
| R-INL-03 | M5-03 | `M5-03.A1`、`M5-03.A2`、`M5-03.A3` |
| R-SESS-01 | M6-01 | `M6-01.A1`、`M6-01.A2`、`M6-01.A3` |
| R-SESS-02 | M6-02 | `M6-02.A1`、`M6-02.A2`、`M6-02.A3` |
| R-SHIP-01 | M6-03 | `M6-03.A1`、`M6-03.A2`、`M6-03.A3` |
| R-NICE-01 | —（MAY 后置，不建任务卡） | — |

<!-- AI_WORKLIST_NORMATIVE_END -->

<!-- AI_WORKLIST_CONTRACT_START -->

## 7. Verification Harness 与里程碑

### 7.1 唯一产品验收入口

M0-01 建立并由后续任务扩展（复用 v1 Harness 形状，项目命名空间 `tui-v2`）：

```bash
node scripts/verify-tui-v2.mjs --task <TASK_ID> --profile implementation
node scripts/verify-tui-v2.mjs --through <MILESTONE_ID> --profile implementation
node scripts/verify-tui-v2.mjs --through M6 --profile production
```

Harness 必须：

- 非交互运行；0 仅代表全部 required assertions 通过；缺参/未知任务 exit 2。
- assertion registry（kind：command / gate / self / file，同 v1）；支持 task、through、implementation/production profile。
- 编排：`cargo test -p r-code-tui`（及触及面的 workspace 测试）、键位/投影/编辑器纯逻辑单测、print 模式冒烟（`--mock` 显式线路）、文档门禁。
- 输出 `artifacts/ai-tasks/verification/tui-v2/<profile>/<task-or-milestone>.json` 与日志；报告 revision/worktree digest、失败断言列表；不记录 secret。
- required fixture/metric 缺失视为失败；不得删测试/降阈值/改 fixture 真值修绿。

M0-01 自身在 Harness 尚未存在时，先用任务卡列出的直接命令验收；随后必须用新 Harness 自验证一次。

### 7.2 里程碑

| 里程碑 | 能力出口 | 累计门禁 |
| --- | --- | --- |
| M0 验收地基 | 统一 Harness、文档门禁、回归基线 | `--through M0 --profile implementation` |
| M1 真实化 | 真实 provider 装配、禁 mock、键盘单次性、错误与首屏引导 | `--through M1 --profile implementation` |
| M2 交互三角与核心交互 | 模型/思考/模式循环、排队、审批浮层 | `--through M2 --profile implementation` |
| M3 状态呈现 | footer 统计、/status、/usage | `--through M3 --profile implementation` |
| M4 编辑器与命令面 | 多行编辑、粘贴折叠、斜杠菜单、!/@、历史、transcript 浮层 | `--through M4 --profile implementation` |
| M5 inline 渲染 | PoC 定案、inline 落地、alt-screen 退役、IME 适配 | `--through M5 --profile implementation` |
| M6 会话与收口 | /resume、会话命令、bin 命名与文档收口 | `--through M6 --profile implementation` |

> 执行顺序说明（调研 §5 技术取舍定案）：M1–M4 先在现有 alt-screen 渲染上完成（用户价值最快兑现），M5 统一迁移到 inline；M5-02 的验收包含"M1–M4 交互回归"。

## 8. 主 Checklist（唯一状态源）

- [x] **M0-01** 建立统一验收 Harness 与文档门禁。证据：`artifacts/ai-tasks/evidence/tui-v2/M0-01.yaml`
- [x] **M0-02** 回归基线登记。证据：`artifacts/ai-tasks/evidence/tui-v2/M0-02.yaml`
- [x] **M1-01** 真实 runtime 接线与禁 mock 红线。证据：`artifacts/ai-tasks/evidence/tui-v2/M1-01.yaml`
- [x] **M1-02** 键盘事件单次性修复。证据：`artifacts/ai-tasks/evidence/tui-v2/M1-02.yaml`
- [x] **M1-03** 错误进 transcript 与 provider 不可用引导。证据：`artifacts/ai-tasks/evidence/tui-v2/M1-03.yaml`
- [x] **M1-04** 无配置首屏引导。证据：`artifacts/ai-tasks/evidence/tui-v2/M1-04.yaml`
- [x] **M2-01** `/model` 模型选择器弹层。证据：`artifacts/ai-tasks/evidence/tui-v2/M2-01.yaml`
- [x] **M2-02** 思考级别弹层、升降与 per-task 记忆。证据：`artifacts/ai-tasks/evidence/tui-v2/M2-02.yaml`
- [x] **M2-03** TaskMode 循环与输入区模式态。证据：`artifacts/ai-tasks/evidence/tui-v2/M2-03.yaml`
- [x] **M2-04** 运行中排队 follow-up。证据：`artifacts/ai-tasks/evidence/tui-v2/M2-04.yaml`
- [x] **M2-05** 审批浮层 codex 化。证据：`artifacts/ai-tasks/evidence/tui-v2/M2-05.yaml`
- [ ] **M3-01** footer 统计投影。证据：待生成
- [ ] **M3-02** `/status` 与 `/usage`。证据：待生成
- [ ] **M4-01** 多行编辑器内核。证据：待生成
- [ ] **M4-02** 粘贴折叠与外部编辑器。证据：待生成
- [ ] **M4-03** 斜杠命令菜单与 `?` 面板。证据：待生成
- [ ] **M4-04** `!` 直通与 `@` 文件提及。证据：待生成
- [ ] **M4-05** 历史导航与 transcript 浮层。证据：待生成
- [ ] **M5-01** inline 渲染路线 PoC 定案。证据：待生成
- [ ] **M5-02** inline 渲染落地。证据：待生成
- [ ] **M5-03** alt-screen 退役与 IME 适配。证据：待生成
- [ ] **M6-01** `/resume` 会话列表。证据：待生成
- [ ] **M6-02** `/new` `/rename` `/compact`。证据：待生成
- [ ] **M6-03** CLI 收口（命名决策、脚本与文档同步、累计门禁）。证据：待生成

## 9. 详细任务卡

### M0-01 建立统一验收 Harness 与文档门禁

- 结果：`scripts/verify-tui-v2.mjs` 支持 `--task/--through/--profile`；`docs/tui-v2/tui-v2-freeze.yaml` 固化；文档门禁 `--mode check` 通过；本文状态改 `frozen`。
- 需求引用：§4.1 R-GEN-01。
- 依赖：无。
- 前置事实：`scripts/verify-r-code-alignment.mjs`（v1 Harness，形状可复制）、`scripts/verify-ai-worklist.mjs`（compute/check + markers）、`artifacts/ai-tasks/templates/` 两模板就位。
- 固定约束：非交互；exit 0/1/2 语义同 MC-6；required 缺失=失败；报告含 revision/worktree digest；不记录 secret。
- 决策空间：REGISTRY 内联结构照抄 v1（kind: command/gate/self/file）；`--through` = 里程碑闭包。
- 产物：`scripts/verify-tui-v2.mjs`、`docs/tui-v2/tui-v2-freeze.yaml`、`artifacts/ai-tasks/verification/tui-v2/implementation/worklist-gate.json`。
- 实施步骤：
  1. 只读预检：v1 Harness 参数契约、模板字段、`artifacts/ai-tasks/` 现有命名空间（不得占用 pi-alignment）。
  2. 写 `verify-tui-v2.mjs`（MILESTONE_ORDER = M0..M6；REGISTRY 初始含 M0-01 自身断言）。
  3. 写 freeze（schema `ai-worklist-freeze.v1`，status 先 `draft`，source_document 指本文）。
  4. `verify-ai-worklist.mjs --mode compute` 生成 digest 回填 → `--mode check` 通过 → freeze `status: frozen`、`blocking=0/major=0`；本文头部状态改 `frozen`。
  5. Harness 自验证：`--task M0-01 --profile implementation` exit 0 且产出报告。
- 验收断言：`M0-01.A1`（三参数解析、缺参/未知任务 exit 2）、`M0-01.A2`（文档门禁 check 通过：digest 一致、blocking=0、major=0）、`M0-01.A3`（报告含 revision/worktree digest 与失败断言列表）。
- 验证：`node scripts/verify-tui-v2.mjs --task M0-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/tui-v2/M0-01.yaml`、`artifacts/ai-tasks/verification/tui-v2/implementation/M0-01.json`。
- 失败处理：digest mismatch 用 `--mode compute` 重新固化并核对无规范性改动；报告缺字段补 report 生成逻辑。

### M0-02 回归基线登记

- 结果：改造前基线可复跑，基线命令全部通过并记录 revision。
- 需求引用：§4.1 R-GEN-02。
- 依赖：M0-01。
- 前置事实：质量门命令见 §5；`r-code-tui` 现有单测（input/approval/lib 投影）全绿；print 模式现状 = mock 演示回放（M1-01 将改变）。
- 固定约束：不为建基线修改任何测试/阈值；基线只记录不修绿；既有失败单独记录外部原因不阻断。
- 决策空间：基线命令集 = §5 三条 + `node --test scripts/release.test.mjs` + `bash dev-tui.sh --print --message smoke`（记录当前 mock 回放形态作为对照基线）。
- 产物：基线证据（命令 + exit code + revision + worktree digest）。
- 实施步骤：
  1. `cargo test -p r-code-tui` 记录结果。2. `cargo clippy -p r-code-tui --all-targets -- -D warnings`。3. `node --test scripts/release.test.mjs`。4. `bash dev-tui.sh --print --message baseline` 记录输出形态（应为 mock 演示回放）。5. 回填 evidence，注册基线断言供累计回归。
- 验收断言：`M0-02.A1`（r-code-tui 测试绿）、`M0-02.A2`（clippy 绿）、`M0-02.A3`（release.test.mjs 绿）、`M0-02.A4`（print 冒烟输出形态已记录为基线对照）。
- 验证：`node scripts/verify-tui-v2.mjs --task M0-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/tui-v2/M0-02.yaml`。
- 失败处理：既有失败先定位是否工作区脏改动；外部原因注明不阻断。

### M1-01 真实 runtime 接线与禁 mock 红线

- 结果：三模式装配即真实 provider；无 `--mock` 时 mock 不可达；print/json 无配置显式报错引导。
- 需求引用：§4.1 R-REAL-01、R-REAL-02；§2.2、§2.3。
- 依赖：M0-02。
- 前置事实：`enable_real_agent_mode`（`commands.rs:1369`）未在 TUI 装配调用；print 模式 `main.rs:241` 主动安装 mock；`push_demo_scenario` 隐式调用点 `commands.rs:14229`。
- 固定约束：交互模式不接受 `--mock`；`--mock` 仅 print/json 评估线路；无 provider 配置时 print/json exit≠0 且输出含 config 绝对路径指引（MC-1/R2）；不改宿主 `install_mock_scenario`/`push_demo_scenario` 的评估线路语义。
- 决策空间：装配时序（enable_real_agent_mode 在事件桥前后）按最小改动定；"无 provider 配置"的判定复用宿主既有配置读取，不新建探测。
- 产物：`main.rs` 装配改造、`--mock` 参数、装配单测、print/json 引导输出。
- 实施步骤：
  1. 预检：确认 `enable_real_agent_mode` 副作用面（runtime 装配、无桌面进程时的行为）。
  2. 交互/print/json 统一装配真实模式；`--mock` 旗标仅评估线路注入场景。
  3. 无配置路径：print/json 输出引导（config 绝对路径 + 桌面设置页途径）后 exit 2。
  4. 单测：装配契约（mock 不可达性用代码路径断言）；空 data-dir 集成测试验证引导输出。
  5. 注册 Harness 断言并跑累计门禁 `--through M1`。
- 验收断言：`M1-01.A1`（装配契约：真实模式调用链单测通过；无 `--mock` 时 TUI 侧无 mock 注入调用点——静态断言 `main.rs` 不再含无条件 `install_mock_scenario`）、`M1-01.A2`（`--mode print --mock --message x` 评估线路 exit 0）、`M1-01.A3`（空 data-dir 下 `--mode print` exit 2 且输出含 config 路径）、`M1-01.A4`（交互模式传 `--mock` 被拒：exit 2 + 用法提示）。
- 验证：`node scripts/verify-tui-v2.mjs --task M1-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/tui-v2/M1-01.yaml`。
- 失败处理：真实模式装配破坏既有单测时，优先修 TUI 侧装配而非改宿主测试默认（宿主测试 Mock 语义不动，见 `commands.rs:32043`）。

### M1-02 键盘事件单次性修复

- 结果：每键只产生一次动作；Windows 下无双写。
- 需求引用：§4.1 R-REAL-03。
- 依赖：M0-02。
- 前置事实：`app.rs:81` `event::read()` 未过滤 `KeyEventKind`；crossterm Windows 发送 Press+Release。
- 固定约束：仅 `KeyEventKind::Press`（及 Repeat，若与长按语义一致）产生 `KeyAction`；不改变其余轮询节律。
- 决策空间：过滤点放 `app.rs` 读取处或 `input.rs::map_key` 入口（推荐后者，可单测）；Repeat 语义按实现验证定并记录。
- 产物：过滤逻辑 + 单测。
- 实施步骤：1. 在归一层入口丢弃非 Press/Repeat。2. 构造 Release/Repeat 事件单测。3. 累计门禁。
- 验收断言：`M1-02.A1`（Release 事件不产生 KeyAction——单测）、`M1-02.A2`（Press 产生且仅产生一次动作——单测计数）。
- 验证：`node scripts/verify-tui-v2.mjs --task M1-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/tui-v2/M1-02.yaml`。
- 失败处理： kitty 协议下 Release 语义差异若引入新问题，回退为仅 Press 并记录。

### M1-03 错误进 transcript 与 provider 不可用引导

- 结果：发送/运行错误对用户可见（transcript System 行）；provider 不可用给出可操作指引。
- 需求引用：§4.1 R-REAL-04、R-REAL-05；§2.3。
- 依赖：M1-01。
- 前置事实：现错误通道 = `eprintln!`（alt-screen 下不可见，`main.rs` send/abort 闭包）。
- 固定约束：错误行经 `TranscriptRow::System` 投影；指引文案必须含 config 绝对路径与桌面设置页途径；不引入 toast/浮层（一期）。
- 决策空间：错误分级（发送失败/运行失败/provider 不可用）的文案与 dim/red 用色按色彩语义表。
- 产物：错误投影、指引文案、单测。
- 实施步骤：1. send/abort 失败改投 `TuiState`。2. provider 不可用错误映射为引导文案。3. 单测文案与投影。4. 累计门禁。
- 验收断言：`M1-03.A1`（agent_send 失败 → System 行投影单测）、`M1-03.A2`（provider 不可用文案含 config 路径与设置页途径——单测）、`M1-03.A3`（TUI 交互路径不再以 eprintln 作为用户可见错误通道——静态断言）。
- 验证：`node scripts/verify-tui-v2.mjs --task M1-03 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/tui-v2/M1-03.yaml`。
- 失败处理：文案断言过脆时改为结构断言（含路径前缀 + 关键词集合）。

### M1-04 无配置首屏引导

- 结果：无 provider 配置时 TUI 首屏为引导卡；配置就位后引导消失。
- 需求引用：§4.1 R-REAL-06；§2.3。
- 依赖：M1-01。
- 前置事实：当前首屏无引导；配置读取走共享 data-dir `config/`。
- 固定约束：引导卡不进入可发送状态（或发送即出引导错误）；列出两条配置途径（桌面 dev 设置页 / 直接编辑 config 文件）；不降级到 mock。
- 决策空间：引导卡形态（信息行集合 vs 框）按 v4 视觉语言；检测时机（启动 + 配置变更轮询）按最小实现定。
- 产物：引导状态、渲染、单测。
- 实施步骤：1. 配置就绪判定函数（复用宿主读取）。2. `TuiState` 引导态 + 渲染行。3. fixture 单测（空配置→引导；有配置→正常）。4. 累计门禁。
- 验收断言：`M1-04.A1`（空配置首屏渲染行 = 引导卡——行快照单测）、`M1-04.A2`（有配置时引导行不存在——fixture 单测）。
- 验证：`node scripts/verify-tui-v2.mjs --task M1-04 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/tui-v2/M1-04.yaml`。
- 失败处理：配置判定与桌面行为不一致时以宿主读取为准修 TUI 侧。

### M2-01 `/model` 模型选择器弹层

- 结果：`/model` 打开弹层（fuzzy + provider 分组 + 预选当前），选中写 task 并反映 footer 右侧。
- 需求引用：§4.1 R-MODEL-01；§2.6；调研 §10.7。
- 依赖：M1-01。
- 前置事实：provider 目录 `provider_catalog.rs`（30 Preset）+ 共享 data-dir config providers；宿主已有会话级 provider 绑定入口（`task_create_with_provider` 及设置面）。
- 固定约束：弹层内联于底部面板上方（不居中模态）；选中行 `›` + cyan bold（非反色）；选项含 provider 分组与模型说明 dim；键位 ↑↓/enter/esc；预选当前值。
- 决策空间：模型切换写回宿主的既有命令以宿主公开面为准（检索 `r_code_host::commands` 的 provider 设置入口并记录）；fuzzy 算法自实现简单子串/评分即可。
- 产物：弹层组件、数据投影、键位接驳、footer 联动、单测。
- 实施步骤：1. 预检宿主 provider 切换命令。2. 目录投影（健康集过滤）。3. 弹层渲染 + 过滤 + 选择。4. footer 右侧 `(provider) model` 联动。5. 单测 + 累计门禁。
- 验收断言：`M2-01.A1`（目录投影/分组/fuzzy 过滤单测）、`M2-01.A2`（选中写 task 且 footer 联动——投影单测）、`M2-01.A3`（弹层键位与预选当前值——键位单测）。
- 验证：`node scripts/verify-tui-v2.mjs --task M2-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/tui-v2/M2-01.yaml`。
- 失败处理：宿主无会话级切换命令时经 `task_set_inference` 同层的既有配置面写回，不新造宿主 API。

### M2-02 思考级别弹层、升降与 per-task 记忆

- 结果：`alt+T` 弹层选档、`alt+,`/`alt+.` 升降；写 `task_set_inference` 持久；能力 clamp；footer `• thinking` 联动。
- 需求引用：§4.1 R-THINK-01；§2.6、§2.7；MC-3。
- 依赖：M1-01。
- 前置事实：`task_set_inference` 已被 TUI 调用（现写死 disabled）；`reasoning_effort` 七档枚举在 vendor contract；max 档提示符金黄（§2.7 例外）。
- 固定约束：档位集合 = MC-3 契约枚举，不得自造；超出模型能力 clamp 到最高支持档并提示；thinking 随档位映射。
- 决策空间：能力声明来源（provider 目录 thinking_level_map / dialect 探测）以宿主既有声明为准。
- 产物：弹层、升降键、clamp、footer 联动、单测。
- 实施步骤：1. 档位枚举对齐 contract。2. 弹层（预选当前档）。3. 升降键 + clamp。4. 写回 + footer。5. 单测 + 累计门禁。
- 验收断言：`M2-02.A1`（档位枚举与 contract 一致——契约单测）、`M2-02.A2`（升降 + clamp 单测）、`M2-02.A3`（per-task 记忆：设置后 task_detail 读回一致——集成）、`M2-02.A4`（footer thinking 段联动、不支持时省略——单测）。
- 验证：`node scripts/verify-tui-v2.mjs --task M2-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/tui-v2/M2-02.yaml`。
- 失败处理：clamp 数据缺失时按保守默认（medium）并记录数据缺口，不阻断。

### M2-03 TaskMode 循环与输入区模式态

- 结果：`Shift+Tab` 循环 ask→edit→auto→plan；输入区按模式着色；写回 task。
- 需求引用：§4.1 R-MODE-01；§2.6、§2.7。
- 依赖：M1-02。
- 前置事实：`TaskMode` 枚举 `dto.rs:129`（Ask/Edit/Auto/Plan）；TUI 建死 `"ask"`；产品语义 Ask/Edit/Auto 统称 Agent、Plan 独立（dto 注释）。
- 固定约束：循环序 = 宿主枚举序；Plan 态 magenta 系；模式经宿主既有 task 更新命令写回（不新造 API）。
- 决策空间：四档呈现文案（Ask/Edit/Auto 合称 Agent 时的显示）按产品语义定并记录；codex 四档权限预设映射（Ask before edits/Plan/Workspace Write/Full Access）中 Full Access 属 access_mode 维度，经 `/permissions` 类命令调整是否入一期由本任务检索宿主既有权限面后决定（无既有面则后置并记录）。
- 产物：循环键、模式态渲染、写回、单测。
- 实施步骤：1. 检索宿主 task mode 更新命令。2. 循环 + 写回。3. 输入区模式态（背景带/提示符色）。4. 单测 + 累计门禁。
- 验收断言：`M2-03.A1`（循环序单测）、`M2-03.A2`（模式写回 task 后续 run 生效——集成）、`M2-03.A3`（Plan 态 magenta 语义色——色彩契约单测）。
- 验证：`node scripts/verify-tui-v2.mjs --task M2-03 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/tui-v2/M2-03.yaml`。
- 失败处理：宿主无运行中改模式命令时仅允许新建 run 前生效，记录语义限制。

### M2-04 运行中排队 follow-up

- 结果：运行中 Enter = 排队（不 steer）；排队列表可见；中止后优先派发排队项。
- 需求引用：§4.1 R-QUEUE-01；§2.5、§2.6；MC-4。
- 依赖：M1-01。
- 前置事实：宿主 `AgentSendMode::Queue`（持久化队列）+ `SendNow`（中止后优先分发）已存在（`dto.rs`）；TUI 现用默认 Auto（运行中=steer）。
- 固定约束：排队经 `AgentSendMode::Queue`；显示 = `• Queued follow-up inputs` bold + 每条 `  ↳ <消息>` dim；中止后经宿主既有派发语义（不自行调度）。
- 决策空间：排队事件 → TranscriptRow 的投影挂点按事件面定；Tab 显式排队（codex `tab to queue`）是否加由实现定并记录。
- 产物：发送模式切换、排队投影、单测/集成。
- 实施步骤：1. 运行态 Enter 改 Queue 模式发送。2. 队列状态投影。3. 中止后派发验证（宿主语义）。4. 集成测试 + 累计门禁。
- 验收断言：`M2-04.A1`（运行中 Enter 入队且不打断当前 run——集成）、`M2-04.A2`（排队渲染行格式——投影单测）、`M2-04.A3`（中止后排队项派发/排空——集成）。
- 验证：`node scripts/verify-tui-v2.mjs --task M2-04 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/tui-v2/M2-04.yaml`。
- 失败处理：宿主队列事件不可观察时在 TUI 侧维护发送意图镜像（仅展示，不复制权威状态）并记录。

### M2-05 审批浮层 codex 化

- 结果：审批卡改为 codex 风内联浮层；y/a/esc 三键；a 出前缀放行注记；esc 出错误单元。
- 需求引用：§4.1 R-APPR-01；§2.5、§2.7；MC-5。
- 依赖：M1-02。
- 前置事实：`approval.rs` 三态决策（Approve/ApproveAlways/Deny）已对齐宿主 PermissionEngine；现渲染为边框卡（v1 形态）。
- 固定约束：浮层 = 底部面板内联带面（背景带、左右内缩 2 列）；标题 bold + 可选 Reason italic；选项从 1 编号、选中 `›` cyan bold；`a` 语义 = 前缀/会话级 standing rule 并在浮层内出注记文案；决策仍经 `ApprovalDecision` 意图链（不绕过 PermissionEngine）。
- 决策空间：`a` 的放行粒度（命令前缀 vs 会话级）以宿主 ApproveAlways 既有语义为准，差异在浮层文案如实呈现；三键之外 d/n/c 等次要键不实现。
- 产物：浮层渲染、键位、注记、单测/集成。
- 实施步骤：1. 渲染改造（带面/编号/选中态）。2. 键位 y/a/esc → ApprovalDecision。3. 拒绝出错误单元（transcript 投影）。4. 行快照单测 + PermissionEngine 集成。5. 累计门禁。
- 验收断言：`M2-05.A1`（浮层渲染契约——行快照单测：带面、bold 标题、编号、`›` cyan bold）、`M2-05.A2`（y/a/esc → 三态映射单测）、`M2-05.A3`（a 放行后 standing rule 生效——PermissionEngine 集成）、`M2-05.A4`（esc 拒绝出错误单元且会话可继续——集成）。
- 验证：`node scripts/verify-tui-v2.mjs --task M2-05 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/tui-v2/M2-05.yaml`。
- 失败处理：行快照过脆时冻结关键行集合（标题/选项行）而非全浮层字节。

### M3-01 footer 统计投影

- 结果：footer 显示 token（输入/输出/缓存）、成本、上下文占用百分比、compaction 标记；阈值变色。
- 需求引用：§4.1 R-STAT-01；§2.7。
- 依赖：M1-01。
- 前置事实：宿主 usage 投影已有数据源（`commands.rs:23706` 一带）；compaction 事件面宿主已有（v1 PRD M5-02）。
- 固定约束：数据来自会话 usage 累加投影（非 runtime 私有态，resume 后仍准确——pi 同款原则）；>70% warning、>90% error 变色；`(auto)` compaction 标记。
- 决策空间：缓存读/写是否一期全显按数据可得性裁剪并记录；紧凑格式（`1.9K`/`42.1%/200k`）照 codex 格式。
- 产物：统计投影、格式化、变色、单测。
- 实施步骤：1. usage 投影接驳。2. 格式化 + 阈值色。3. compaction 标记。4. 单测 + 累计门禁。
- 验收断言：`M3-01.A1`（格式化单测：紧凑格式与百分比）、`M3-01.A2`（阈值变色契约单测）、`M3-01.A3`（usage 累加来自会话投影——resume 后数值一致的单测/集成）。
- 验证：`node scripts/verify-tui-v2.mjs --task M3-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/tui-v2/M3-01.yaml`。
- 失败处理：usage 细分字段缺失时显示可得子集并在断言中记录数据边界。

### M3-02 `/status` 与 `/usage`

- 结果：`/status` 圆角框状态卡（模型/目录/token/上下文）；`/usage` 成本汇总；footer 右侧 context 余量。
- 需求引用：§4.1 R-STAT-02；调研 §10.9。
- 依赖：M3-01。
- 前置事实：codex /status 卡结构有快照依据（圆角框、标签 padEnd(18)、Directory 行不对齐 quirk）；footer 右侧 `{N}% context left` / `{x.xK} used`。
- 固定约束：卡内标签对齐用统一宽度核算（MC-8）；限速条形 `[██████░░░░░░]` 仅数据可得时显示。
- 决策空间：Directory 行是否保留 codex 不对齐 quirk 由实现定并记录（视觉还原 vs 一致性）；命令解析在斜杠菜单（M4-03）之前先支持裸命令输入。
- 产物：状态卡渲染、usage 输出、footer 右侧、单测。
- 实施步骤：1. `/status` 卡数据投影 + 渲染。2. `/usage` 汇总。3. footer 右侧。4. 行快照单测 + 累计门禁。
- 验收断言：`M3-02.A1`（/status 卡行快照：框、标签对齐、Token usage/Context window 行）、`M3-02.A2`（/usage 输出含累计成本——数据源单测）、`M3-02.A3`（footer 右侧格式单测）。
- 验证：`node scripts/verify-tui-v2.mjs --task M3-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/tui-v2/M3-02.yaml`。
- 失败处理：限速数据不可得时隐藏该行并记录。

### M4-01 多行编辑器内核

- 结果：多行输入（显式换行键）、undo/redo、词导航、CJK/grapheme 安全折行、光标编辑完备。
- 需求引用：§4.1 R-EDIT-01；MC-8。
- 依赖：M1-02。
- 前置事实：现 `InputBuffer` 为单行（v1）；workspace 无 unicode-segmentation 依赖记录（预检确认）。
- 固定约束：折行 CJK=2 列（MC-8）；grapheme 边界安全（不截断组合字符）；undo/redo 栈正确处理组合编辑。
- 决策空间：换行键集合（`\`+Enter / Shift+Enter / Ctrl+J）按终端可得性选并记录；grapheme 分段依赖新增 `unicode-segmentation` 或复用 workspace 既有（预检后选最小改动）。
- 产物：编辑器内核（纯逻辑，可单测）、键位、单测。
- 实施步骤：1. 预检依赖。2. 内核实现（rows/insert/delete/undo/redo/word-nav/折行）。3. 键位接驳。4. 纯逻辑单测（CJK 边界用例）。5. 累计门禁。
- 验收断言：`M4-01.A1`（多行编辑与显式换行单测）、`M4-01.A2`（undo/redo 单测）、`M4-01.A3`（词导航 + CJK/grapheme 折行边界单测）、`M4-01.A4`（光标移动/编辑不越界单测）。
- 验证：`node scripts/verify-tui-v2.mjs --task M4-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/tui-v2/M4-01.yaml`。
- 失败处理：终端换行键不可用时以文档记录可用键集，内核语义不变。

### M4-02 粘贴折叠与外部编辑器

- 结果：大粘贴折叠为 `[Pasted Content N chars]`；Ctrl+G 起 `$VISUAL/$EDITOR` 外编并回填。
- 需求引用：§4.1 R-EDIT-02；调研 §10.6。
- 依赖：M4-01。
- 前置事实：codex 阈值 >1000 字符、编号 #N；TUI 需临时退出 raw mode 跑外编（v1 fullscreen.rs 有先例可参）。
- 固定约束：折叠占位符进上下文但原文不丢失（发送内容含原文，与宿主消息契约一致）；外编失败（无 EDITOR/退出码非 0）回编辑器态不丢内容。
- 决策空间：折叠原文缓存位置（内存 vs 临时文件）按生命周期定；阈值 1000 字符照 codex。
- 产物：折叠、外编链路、单测/集成。
- 实施步骤：1. 粘贴检测（bracketed paste）+ 折叠。2. Ctrl+G 外编（raw mode 退出/恢复）。3. 回填。4. 单测（EDITOR=fixture 脚本集成）。5. 累计门禁。
- 验收断言：`M4-02.A1`（>1000 字符折叠占位 + 编号单测）、`M4-02.A2`（外编回填——fixture EDITOR 集成）、`M4-02.A3`（发送内容含折叠原文——契约单测）。
- 验证：`node scripts/verify-tui-v2.mjs --task M4-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/tui-v2/M4-02.yaml`。
- 失败处理：外编环境不可用时集成测试跳过并标注 profile 边界，单测部分仍须绿。

### M4-03 斜杠命令菜单与 `?` 面板

- 结果：`/` 起始输入弹出上方插入式命令菜单（fuzzy、Tab 补全、no matches dim italic）；空输入 `?` 出两列快捷键面板。
- 需求引用：§4.1 R-CMD-01；§2.6；调研 §10.7。
- 依赖：M4-01。
- 前置事实：codex 弹层最多 8 行、`/名` + dim 描述、选中 cyan bold；一期命令集裁剪建议在调研 §10.7。
- 固定约束：一期命令集（冻结）= `/model` `/thinking` `/status` `/usage` `/new` `/resume` `/rename` `/compact` `/permissions`（若 M2-03 检索到宿主权限面，否则移除并记录）`/quit` `/clear` `/help`；未实现命令不出现在菜单。
- 决策空间：菜单行数上限（≤8 行 + 滚动）；`/help` 与 `?` 面板关系（同内容复用）。
- 产物：命令注册表、菜单、面板、单测。
- 实施步骤：1. 命令注册表（id + 描述 + 可用性）。2. 菜单渲染 + 过滤 + Tab 补全。3. `?` 面板。4. 行快照 + 键位单测。5. 累计门禁。
- 验收断言：`M4-03.A1`（注册表 = 冻结命令集——契约单测）、`M4-03.A2`（fuzzy 过滤 + no matches dim italic——行快照单测）、`M4-03.A3`（Tab 补全与 enter 执行——键位单测）、`M4-03.A4`（`?` 面板两列渲染——行快照单测）。
- 验证：`node scripts/verify-tui-v2.mjs --task M4-03 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/tui-v2/M4-03.yaml`。
- 失败处理：命令可用性动态变化时注册表按数据源过滤，不硬编码缺席。

### M4-04 `!` 直通与 `@` 文件提及

- 结果：`!` 起始 = bash 直通（输出进 transcript dim）；`@` 触发文件路径补全。
- 需求引用：§4.1 R-SHELL-01；§2.7。
- 依赖：M4-01。
- 前置事实：`bang_command.rs` 已有 ShellRow（Prompt/Output）雏形；宿主 shell 执行链经 ToolGateway（§5 安全不变量）。
- 固定约束：`!` 执行经宿主既有 bang/shell 链（不新开裸进程通道）；`!` 输入态 light-red 提示符；输出 dim、退出码呈现。
- 决策空间：`@` 补全数据源（workspace 相对路径扫描）范围与深度按性能定；补全菜单复用 M4-03 菜单组件。
- 产物：直通接驳、补全、单测。
- 实施步骤：1. `!` 直通经宿主链执行 + ShellRow 投影。2. `@` 扫描 + 补全菜单。3. 色彩契约。4. 单测/集成。5. 累计门禁。
- 验收断言：`M4-04.A1`（`!` 执行输出进 transcript dim + 退出码——集成）、`M4-04.A2`（`@` 补全列表过滤单测）、`M4-04.A3`（`!` 态 light-red——色彩契约单测）。
- 验证：`node scripts/verify-tui-v2.mjs --task M4-04 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/tui-v2/M4-04.yaml`。
- 失败处理：宿主 bang 链不可直达时走 ToolGateway 审批直通并记录路径差异。

### M4-05 历史导航与 transcript 浮层

- 结果：↑/↓ 与 Ctrl+P/N 翻已发消息；Ctrl+T 打开 transcript 浮层（完整输出、滚动、q/esc 关闭）。
- 需求引用：§4.1 R-HIST-01；§2.4（fullscreen 语义由本浮层承担）。
- 依赖：M4-01。
- 前置事实：codex transcript 顶行 `/ T R A N S C R I P T /…` dim、底部 hints、`q to quit   esc to edit prev`；r-code `Ctrl+P` 让位历史导航（§2.6 决策）。
- 固定约束：浮层内容 = transcript 全量（含展开工具输出）；键位 ↑↓/pgup/pgdn/home/end/q/esc；浮层关闭后回到编辑器焦点。
- 决策空间：浮层实现形态（底部面板扩展 vs 全屏覆盖）——因无独立 fullscreen 模式，本浮层是唯一"全屏"语义载体，默认全屏覆盖、内缩 2 列带面。
- 产物：历史栈、浮层、键位、单测。
- 实施步骤：1. 已发消息历史栈 + 导航键。2. transcript 浮层渲染（顶行/hints）。3. 滚动与关闭。4. 键位 + 行快照单测。5. 累计门禁。
- 验收断言：`M4-05.A1`（历史导航单测）、`M4-05.A2`（浮层开合 + 滚动键位单测）、`M4-05.A3`（浮层顶行/hints 行快照）、`M4-05.A4`（浮层含展开工具输出——投影单测）。
- 验证：`node scripts/verify-tui-v2.mjs --task M4-05 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/tui-v2/M4-05.yaml`。
- 失败处理：全屏覆盖与 inline 渲染冲突时先在 alt-screen 实现并在 M5-02 迁移（依赖顺序已保证）。

### M5-01 inline 渲染路线 PoC 定案

- 结果：ratatui InlineViewport vs 自研行差分两条路线的 PoC 基准报告 + 定案记录。
- 需求引用：§4.1 R-INL-01；调研 §5 技术取舍。
- 依赖：M1-01。
- 前置事实：pi 行差分 + CSI 2026 在 Rust 无现成等价物；ratatui `InlineViewport` 仍是 viewport 内重绘（语义不同）；§5 性能预算 ≤16ms/帧。
- 固定约束：PoC 必须可复跑（命令进报告）；评判维度（冻结）：scrollback 语义完整性（历史行真正进终端 scrollback、退出保留）、resize 行为、闪烁（同步输出）、单帧耗时；以数据定案并记录决策依据。
- 决策空间：PoC 形态（独立 example/bin vs 分支原型）按最小侵入定；两路线皆不达标时允许第三路线（如 ratatui inline + 尾部 append 混合）并同基准记录。
- 产物：PoC 代码、基准报告、决策记录（进 evidence + freeze 注记）。
- 实施步骤：1. 写两路线最小渲染 demo（滚动历史 + 贴底编辑器）。2. 基准（帧耗时、resize、scrollback 行为验证脚本）。3. 报告 + 定案。4. 注册断言。
- 验收断言：`M5-01.A1`（基准报告存在且含两路线数据——文件断言）、`M5-01.A2`（PoC 可复跑——命令断言 exit 0）、`M5-01.A3`（定案记录含依据与被否路线差距——文件断言）。
- 验证：`node scripts/verify-tui-v2.mjs --task M5-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/tui-v2/M5-01.yaml`。
- 失败处理：基准环境噪声大时固定终端尺寸 + 多轮取中位数并记录方法。

### M5-02 inline 渲染落地

- 结果：交互模式默认 inline：历史进 scrollback、行差分重绘 + CSI ?2026、编辑器/footer 贴底；M1–M4 全部交互回归通过。
- 需求引用：§4.1 R-INL-02；§2.4；MC-8。
- 依赖：M5-01、M4-01（及 M2/M3/M4 既有组件面）。
- 前置事实：现渲染 = alt-screen + ratatui 全屏；按 M5-01 定案路线实施。
- 固定约束：退出 TUI 后历史保留在终端 scrollback；重绘包 CSI ?2026 同步输出；宽度变化才允许全量重绘；组件契约"超宽行必须截断不 crash"。
- 决策空间：渲染层拆分（沿 PoC 结构）；M2–M4 组件的适配层最小化。
- 产物：inline 渲染层、组件适配、集成测试。
- 实施步骤：1. 按定案路线落渲染骨架。2. 组件适配（弹层/浮层/带面在 inline 下的呈现）。3. PTY 集成测试（scrollback/resize/同步输出字节断言）。4. M1–M4 交互回归（累计门禁 `--through M4` + 本任务）。5. `--through M5`。
- 验收断言：`M5-02.A1`（历史行进 scrollback——PTY 集成：退出后 scrollback 可读）、`M5-02.A2`（重绘包 CSI ?2026——字节级断言）、`M5-02.A3`（resize 下编辑器/footer 贴底稳定——PTY 集成）、`M5-02.A4`（M1–M4 断言在 inline 下全绿——累计门禁）。
- 验证：`node scripts/verify-tui-v2.mjs --task M5-02 --profile implementation`；累计 `--through M5`。
- 证据：`artifacts/ai-tasks/evidence/tui-v2/M5-02.yaml`。
- 失败处理：PTY 测试环境不可用时以确定性渲染 harness（无真实终端的行缓冲断言）替代并标注边界，真实终端验证归 §11.3。

### M5-03 alt-screen 退役与 IME 适配

- 结果：独立 alt-screen 路径移除（或仅作内部实现细节且不可达）；IME 假光标在 inline 下坐标正确。
- 需求引用：§4.1 R-INL-03；§2.4。
- 依赖：M5-02。
- 前置事实：`fullscreen.rs` 存在 alt-screen 辅助；`ime.rs` 做坐标计算（假光标跟随）。
- 固定约束：交互主路径不再 EnterAlternateScreen；IME 候选窗跟随编辑器光标位（vw 核算含 CJK）；print/json 不受影响。
- 决策空间：`fullscreen.rs` 删除或收缩为渲染层内部工具按复用度定。
- 产物：路径清理、IME 适配、单测。
- 实施步骤：1. 移除/内部化 alt-screen 主路径。2. `ime.rs` 对接 inline 光标位。3. 单测（坐标核算）。4. 累计门禁。
- 验收断言：`M5-03.A1`（交互主路径无 EnterAlternateScreen——静态断言）、`M5-03.A2`（IME 光标坐标单测：含 CJK 双宽用例）、`M5-03.A3`（print/json 回归绿）。
- 验证：`node scripts/verify-tui-v2.mjs --task M5-03 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/tui-v2/M5-03.yaml`。
- 失败处理：IME 真机行为归 §11.3；单测覆盖坐标核算即可。

### M6-01 `/resume` 会话列表

- 结果：`/resume` 列出共享 data-dir 会话（`❯` 光标、双行行目、排序），enter 接续会话。
- 需求引用：§4.1 R-SESS-01；调研 §10.9。
- 依赖：M4-01。
- 前置事实：会话 JSONL + SQLite 经共享 data-dir（与桌面互通——桌面可 resume TUI 会话，v1 既定）；codex resume 列列头 `Created at Updated at Branch CWD Conversation`、底行 hints、`tab to toggle sort`。
- 固定约束：数据源 = 宿主会话/task 列表命令（不直连 SQLite 绕过宿主）；enter 后接续 = 宿主 resume 语义（JSONL 重建）。
- 决策空间：列裁剪（无 Branch 时隐藏）、排序键（时间/标题）按数据面定。
- 产物：列表投影、选择器、resume 接续、单测/集成。
- 实施步骤：1. 检索宿主会话列表/resume 命令。2. 投影 + 选择器渲染。3. enter resume 接续（transcript 重建）。4. 行快照单测 + 集成。5. 累计门禁。
- 验收断言：`M6-01.A1`（列表投影/排序单测）、`M6-01.A2`（选择器行快照：`❯` 光标、双行行目、底行 hints）、`M6-01.A3`（resume 后 transcript 与持久化记录一致——集成）。
- 验证：`node scripts/verify-tui-v2.mjs --task M6-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/tui-v2/M6-01.yaml`。
- 失败处理：宿主 resume 命令缺失时补最薄宿主入口（复用桌面 IPC 同源实现），记录新增面。

### M6-02 `/new` `/rename` `/compact`

- 结果：三个会话命令落地：新建、改名持久化、触发宿主压缩。
- 需求引用：§4.1 R-SESS-02。
- 依赖：M6-01。
- 前置事实：宿主 compaction 机制已有（v1 PRD M5-02 retained_tail）；task/session 改名宿主已有（桌面命名会话）。
- 固定约束：`/compact` 走宿主既有压缩入口（不自研摘要）；`/rename` 持久化且 footer 会话名更新。
- 决策空间：`/new` 是否保留当前会话上下文（不保留——新 task）。
- 产物：三命令、单测/集成。
- 实施步骤：1. 检索宿主命令面。2. 实现三命令 + 状态联动。3. 集成测试。4. 累计门禁。
- 验收断言：`M6-02.A1`（/new 新建空会话——集成）、`M6-02.A2`（/rename 持久化 + footer 更新——集成）、`M6-02.A3`（/compact 触发宿主压缩事件——集成）。
- 验证：`node scripts/verify-tui-v2.mjs --task M6-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/tui-v2/M6-02.yaml`。
- 失败处理：宿主压缩入口不可直达时经设置面同源入口并记录。

### M6-03 CLI 收口（命名决策、脚本与文档同步、累计门禁）

- 结果：bin 命名决策落地（别名 `r-code` 或维持 `r-code-tui`）；分发/脚本/文档同步；`--through M6` 累计门禁 exit 0。
- 需求引用：§4.1 R-SHIP-01；§2.1。
- 依赖：M5-03、M6-02。
- 前置事实：现 bin = `r-code-tui`（`crates/r-code-tui/Cargo.toml` `[[bin]]`）；`tauri.conf.json` bundle externalBin 声明 `binaries/r-code-tui`（v1 M8-04）；`dev-tui.ps1`/`dev-tui.sh` 与 README 两份文档引用；`release.test.mjs` 有 TUI 断言。
- 固定约束：决策必须记录依据（分发影响：externalBin/安装 PATH/脚本/文档一致性成本 vs 品牌收益）；别名方案不得破坏既有 externalBin 契约；同步面 = Cargo bin/externalBin、dev 脚本、README×2、release.test.mjs 断言。
- 决策空间：别名实现方式（cargo `[[bin]]` 双名 vs 安装期 symlink vs 维持现状）按 v1 M8-04 分发管线事实选最小改动。
- 产物：决策记录、（如需）别名与同步变更、收口报告。
- 实施步骤：1. 评估三方案对分发管线影响并定案记录。2. 落地（如维持现状则仅记录）。3. 同步脚本/文档/断言。4. `--through M6 --profile implementation` 累计门禁。5. 归档收口报告。
- 验收断言：`M6-03.A1`（命名决策记录存在且含分发影响分析——文件断言）、`M6-03.A2`（脚本/文档/externalBin/断言四面一致——契约断言：别名方案时 `release.test.mjs` 扩断言绿；维持现状时一致性断言绿）、`M6-03.A3`（累计门禁 `--through M6 --profile implementation` exit 0）。
- 验证：`node scripts/verify-tui-v2.mjs --task M6-03 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/tui-v2/M6-03.yaml`。
- 失败处理：别名破坏安装管线时回退维持现状并记录。

## 10. 连续执行、恢复与证据协议

### 10.1 固定循环

选择编号最小且依赖已满足的未完成 MUST → 建立/恢复 `artifacts/ai-tasks/tui-v2/current.yaml` → 实现一个可验证子步 → 更新 packet → 运行任务断言 → 失败则诊断/修复/换受约束方案 → 通过则运行累计门禁 → 归档 evidence → 勾选 §8 唯一 Checklist 并重算进度 → 立即进入下一项。里程碑、汇报、文档更新、测试通过均不是等待人工确认的节点。

### 10.2 证据规则

- 每个可验证子步后更新 `current.yaml`：实际修改路径、已完成/剩余步骤、已完成/剩余断言、失败尝试、关键决定。
- 完成项证据真实存在且可关联当前实现；"证据待生成"只出现在未完成项。
- 不记录隐藏推理，只记录可复核的选择、依据和结果；不写 secret。

### 10.3 自主决策与失败处理

- 可逆、仓库内、未扩张权限的选择按安全 > 正确 > 简单 > 一致 > 可测试 > 性能 > 新颖性自行决定并记录。
- 失败依次：定位根因 → 聚焦修复 → 重试 → 换受约束方案 → 隔离外部阻塞继续不依赖项。
- 缺少真实 Provider/真实终端时，用 fake/fixture/PTY harness 做到 `implementation_verified`，真实放行保持未满足（§11.3）；不中途停掉整个编码任务。

## 11. 风险、兼容与外部放行

### 11.1 风险与回滚

- M5 是最大单项（渲染层重写）：前置 PoC 定案 + PTY 集成护栏；M5-02 失败时可回退 alt-screen 路径（M5-03 之前保持双路径共存策略由实现记录）。
- M1-01 真实化改变 print 基线行为：以 M0-02 基线为对照，行为变化必须可解释。
- 宿主公开面扩展（M2-01/M6-01 可能的最薄入口）不得破坏桌面 IPC 回归；每任务跑 `cargo test --workspace` 相关子集。
- 键位/视觉是冻结契约（§2.6/§2.7），实现偏差以 `tui-v4-prototype.html` 与行快照单测拦截。

### 11.2 提交切片（建议，非门禁）

按里程碑 M0→M6 分切片提交；每个任务完成后单独 commit 或与同里程碑任务合并，保持可回退粒度。

### 11.3 外部放行（production profile）

| 项 | 说明 |
| --- | --- |
| 真实 Provider 复测 | M1/M2/M3 真实模型连通、思考档位映射、usage 数值真实性 |
| Windows 终端实测 | 双写修复、代码页 UTF-8、Windows Terminal/legacy conhost 双环境 |
| IME 真机 | 中文输入法候选窗跟随假光标（inline 模式） |
| 跨终端色彩/宽度 | macOS Terminal.app/iTerm2/Windows Terminal/PTY CI 环境外的主流终端 vw=2 与语义色一致性 |
| 安装分发 | M6-03 别名方案（如采用）在四平台 bundle 的实机验证 |

这些项不影响 `implementation_verified` 完成判定；仅当追求 `production_release_ready` 时才需满足。

<!-- AI_WORKLIST_CONTRACT_END -->
