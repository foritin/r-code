# R-Code 与 Pi 对齐 + TUI 方案 PRD / AI 实施清单

> 文档状态：`frozen`（只表示执行合同已完整、通过文档门禁，不表示产品功能已经实现）
> 执行合同：`prd-to-ai-worklist` v1.1.0
> 取证基线：2026-08-30；pi（`earendil-works/pi`，原 `badlogic/pi-mono`）v0.84.x 官方仓库 master 分支逐条核查；R-Code 现状来自 [`docs/architecture.md`](../architecture.md) 与源码
> 固化清单：[`pi-alignment-and-tui-freeze.yaml`](./pi-alignment-and-tui-freeze.yaml)
> 唯一完成状态：本文 §8 主 Checklist；任务卡、任务包与证据不得维护第二套 Checkbox

## 执行导航

- 首次执行：§0 → §2 → §4 → §7 → §8 → 首个 ready 任务卡（§9）。
- 中断恢复：`artifacts/ai-tasks/current.yaml` → §9 对应任务卡 → `artifacts/ai-tasks/evidence/pi-alignment/`。
- 判断完成：§7 统一 Harness → §8 唯一 Checklist 勾选 → `artifacts/ai-tasks/verification/pi-alignment/implementation/`。
- 产品终态与非目标：§1。
- 不可变决策：§2。
- 机器合同与规范性需求：§4。
- 验收与里程碑：§7。

## 0. AI 执行入口

<!-- AI_WORKLIST_VOLATILE_START -->

- 当前进度：`28 / 28` 项完成。
- 下一执行项：全部任务已完成（implementation_verified；收口门禁 --through M8）。
- 当前任务包：`artifacts/ai-tasks/pi-alignment/current.yaml`（根 current.yaml 属于另一 worklist 的活跃资产，本清单项目内偏移，见 M0-01 证据 DM1）。
- 注意：工作区存在未提交改动（`docs/pi-alignment/pi-alignment-and-tui-prd.md` 及归档移动），均视为用户资产，任何任务不得 reset/覆盖/回滚它们。

<!-- AI_WORKLIST_VOLATILE_END -->

### 0.1 首次启动

1. 只读检查 Git revision、完整 worktree、Rust/Node 运行时、现有测试基线（`cargo test`、前端 `npm test`、Windows 金集 `scripts/verify-windows-reliability.mjs`）；已有未提交改动一律视为用户资产。
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
- 不是中断理由：到达里程碑、切换前后端/测试阶段、类名/文件组织/库选择等可逆偏好、首轮测试或 lint 失败、缺少真实模型但 adapter/fake/local profile 足以验收实现。
- 外部放行项（见 §11.3）不阻塞可离线完成的实现；实现侧做到 `implementation_verified` 即视为本清单完成终点。

<!-- AI_WORKLIST_NORMATIVE_START -->

## 1. 背景、目标、终态与非目标

### 1.1 背景

R-Code 与 Pi 是定位不同的两类系统：Pi 是极简终端 Agent harness，靠"组合扩展"补能力；R-Code 是 session-first 桌面应用，把安全、审核、双引擎、记忆、Plan 做成第一等能力，产品纵深远超 Pi。本清单只做"取 Pi 之长补 R-Code 之短"，不否定、不重做 R-Code 已有纵深。

待补的短板（按 P0/P1/P2 分级，详见仓库外《Pi Agent 深度解析（十篇合一）》，本清单自足）：

| 级别 | 能力域 | 一句话差距 |
| --- | --- | --- |
| P0 | Provider 治理通用化 | 接入残缺 OpenAI 兼容端点无统一"关字段"入口；"解析成功 vs 缺鉴权 vs 组装失败"不可定位 |
| P0 | 行为级评估框架 | 无法量化"改 Prompt/模型/编排是否让 Agent 变好" |
| P1 | 缓存主动优化 | 有"归因"（cache_shape.rs）无"主动保前缀"（Deferred Tools） |
| P1 | 运行时扩展/技能生态 | 无"装一个包获得新工具/技能"的运行时机制 |
| P1 | 会话数据模型正交化 | "进上下文 vs 持久化"未在类型层区分；压缩点自包含性未到 Pi 粒度 |
| P2 | 统一遥测契约 | 跨引擎（原生 + Codex）统一观测吃力 |
| P2 | 沙箱化运行 | 无"容器内跑 agent"部署形态 |
| P2 | TUI / headless 模式 | 无 SSH/远程/无桌面 Linux 的终端形态 |

### 1.2 Definition of Done

终态必须是可观察的系统状态，覆盖：

1. Provider 治理：残缺端点经配置接入（不写源码），三态快照可定位失败，分层定价与思考等级映射接入成本归因。
2. 评估：真实 Agent 会话跑真实任务，baseline/candidate 配对输出 Pass Rate Lift 与配对差值，安全红线硬断言触发即 fail，首个金集基线可本地复跑。
3. 缓存：中途新增工具不击穿前缀缓存（有 cache_guard 字节断言），能力探测白名单不破坏正确性。
4. 扩展：技能渐进式披露、扩展工具经 Gateway 审批（不绕过 R0-R4）、可热重载。
5. 会话：进上下文 vs 仅持久化类型层区分；压缩点自包含，恢复与回溯逐字节一致。
6. 遥测：双引擎统一 Span，Adapter 一致性测试通过，默认 NOOP 零开销。
7. 沙箱：命令执行后端可插拔，Docker 后端启用后审批/审计不变，未启用零行为变化。
8. TUI：独立 `r-code-tui` 复用 Host 编排、共享 data-dir、snapshot 权威、内联审批同源、IME 候选窗正确。
9. 分发：CLI 随安装包就位，卸载无残留，PATH 失败不阻断安装。
10. 累计门禁 `--through M8 --profile implementation` 返回 0，全部 required 断言有真实证据。

### 1.3 非目标

- 不照搬 npm 包分发生态（桌面应用不需要）；Skills/Extensions 以本地目录加载为第一形态。
- 不在 TUI 里复刻 WebView 全部场景（文件工作台、Plan 面板、记忆管理页等 GUI 重交互场景不在首版）。
- 不自研 TUI 框架（用 ratatui + crossterm）。
- 不做终端内图片渲染（Kitty/iTerm2 图形协议不在首版，文本占位）。
- 不把 Pi 的 micro-VM/多租户（Gondolin/OpenShell）做进首版；沙箱只落地 Docker 可插拔点。
- 不把行为级评估做成 CI 阻断项起步；先作为本地可复跑的回归门禁。

## 2. 已冻结决策

1. **复用而非重做**：TUI 复用 `r-code-terminal`（portable-pty + OSC 133）、`AgentEvent` 出口、`ToolGateway`（R0-R4 + PathGuard）、`DelegationTree`、JSONL SessionStore、`LlmAgentRuntime`。不在任何新层重新实现安全边界、Agent 循环或持久化。
2. **内核零改动**：TUI 与评估 Harness 只新增壳层/适配，不改 `LlmAgentRuntime` 的 Agent 循环语义；AgentEvent 是唯一事件源。
3. **安全边界不降级**：扩展注册的工具、Docker 后端、TUI 内联审批，全部经 `ToolGateway` 同源入口；R0-R4 拒绝矩阵、PathGuard capability、`defaultProjectTrust`/`httpProxy` 仅全局 等既有约束在任何新入口下不变。compat/声明式端点不得覆盖厂商直连入口既有安全行为。
4. **TUI 二进制形态**：`r-code-tui` 作为独立 `[[bin]]`，复用 `src-tauri/src` 的 Host 编排模块但不启动 WebView；默认 `--mode tui`，兼 `print`/`json` 子形态；默认 data-dir 指向桌面应用同一 AppData。
5. **snapshot 权威 vs 事件瞬时**：渲染层只消费事件做展示，权威状态走 JSONL + SQLite 重建；不把事件流累积成领域状态副本。
6. **默认关闭、白名单开启**：一切可能破坏正确性或引入外部副作用的优化（Deferred Tools 的 tool reference、缓存 long retention、Docker 后端）默认关闭，能力探测白名单启用；未启用时零行为变化。
7. **成本可观测但不噪声**：遥测默认 NOOP；缓存 miss 提示沿用"阈值 + 归因只陈述可观测事实"原则，不引入每轮噪声告警。
8. **渐进式披露**：技能只注入名称 + 一行描述，无 `read` 工具不注入技能列表。
9. **外部放行分层**：真实 Provider 复测、真实容器环境、macOS/Apple 签名实机等属于 `production_release_ready`，不影响 `implementation_verified` 完成。

## 3. 仓库事实表

| 事实 | 证据 |
| --- | --- |
| 语言/包管理 | Rust workspace（`Cargo.toml` members 含 6 个 product crate + 8 个 vendor/agent-contracts crate）；前端 React 18 + Zustand + `@tauri-apps/api` 2 |
| 现有 Agent 循环 | `crates/r-code-agent-worker/src/agent_loop.rs`（4275 行）、`llm_runtime.rs`（12191 行），事件 `AgentEvent` 在 `crates/r-code-core/src/dto.rs:1469` |
| 能力探测现状 | `vendor/agent-contracts/crates/agent-llm/src/dialect.rs`：`supports_vision/supports_streaming/supports_tool_use/supports_prompt_caching` 四布尔 |
| 预设来源 | `src-tauri/src/provider_catalog.rs`（cc-switch 2026-07 快照，`PRESETS` 常量） |
| 缓存归因 | `crates/r-code-agent-worker/src/cache_shape.rs`（PrefixShape compare/compare_with_rewrite_cause） |
| 缓存守卫 | `crates/r-code-agent-worker/tests/cache_guard.rs`（字节前缀 mock，`tail_avg ≥ 90%`，env `R_CODE_CACHE_GUARD=1`） |
| 压缩实现 | `llm_runtime.rs:2957-3206`（50%/60%/80% 分层 + 防抖 + `automatic_compaction_*`） |
| 工具安全边界 | `crates/r-code-gateway/src/gateway.rs`（单一执行入口）、`classifier.rs`（R0-R4）、`core/security.rs`（PathGuard capability） |
| 子代理 | `crates/r-code-agent-worker/src/delegation_tree.rs`（parent/child/sibling 拓扑）；报告契约 `r-code-core::SUBAGENT_REPORTING_CONTRACT` |
| 终端底座 | `crates/r-code-terminal/`（portable-pty + OSC 133 + ReplayParser + CliDetector） |
| 双存储 | JSONL SessionStore（source of truth）+ SQLite（`crates/r-code-store/`，schema 30）+ BLAKE3 Blob |
| 安装体系 | `r-code-host`（Tauri bundle msi/nsis/dmg/appimage，`tauri.conf.json`）；branded installer `installer/`（overlay 机制，`r-code-installer-pack`）；NSIS 钩子 `src-tauri/installer-hooks.nsh` |
| 现有验收脚本 | `scripts/verify-windows-reliability.mjs`、`verify-codex-interaction.mjs`、`verify-product-experience.mjs`、`verify-ai-worklist.mjs`（通用 worklist 门禁） |
| 现有质量门 | `docs/architecture.md §14`：fmt/clippy/`cargo test --workspace --all-features`/前端 `npm test`+`npm run build`/`cargo deny`；单测基线 2332 |
| 已有 eval 雏形 | `src-tauri/src/bin/plan_eval.rs`（Plan 三臂环境隔离），非通用 Agent 行为评估 |
| workspace 无 TUI 依赖 | 无 ratatui/crossterm（需新增 workspace dep） |

## 4. 机器合同与规范性需求

### 4.1 规范性需求登记

- **R-GEN-01（MUST）**：统一非交互验收 Harness（`scripts/verify-r-code-alignment.mjs`）支持 `--task <TASK_ID>`、`--through <MILESTONE_ID>`、`--profile implementation|production`；0 仅表示全部 required assertion 通过；输出 `artifacts/ai-tasks/verification/pi-alignment/<profile>/<task-or-milestone>.json` 与证据索引；记录 revision/worktree digest 与失败断言列表；required fixture/metric 缺失视为失败。
- **R-GEN-02（MUST）**：改造不得破坏既有回归基线：`cargo test --workspace --all-features`、前端 `npm test` + `npm run build`、Windows 金集 `scripts/verify-windows-reliability.mjs`、`scripts/verify-codex-interaction.mjs` 在累计门禁中持续通过。
- **R-PRV-01（MUST）**：`ProviderCompat` 声明式兼容层（`supports_developer_role`/`supports_reasoning_effort`/`supports_long_cache_retention`/`supports_explicit_prompt_cache_mode`/`session_affinity_format`/`thinking_token_budget_field` 等，按 R-Code provider 面裁剪），provider 级 + model 级合并（model 覆盖 provider）；硬编码默认 + 用户 compat 覆盖两级合成，用户 compat 不覆盖厂商直连入口既有安全行为（DeepSeek 等内置 provider 硬编码默认保留）。
- **R-PRV-02（MUST）**：声明式端点接入（`baseUrl + api + apiKey 引用 + models`），值解析支持 `$ENV` 与凭据引用（复用 `provider_kind` 稳定身份与现有密钥后端），不引入明文密钥落盘。
- **R-PRV-03（MUST）**：`ModelAvailability` 三态快照（`all`/`available`/`composition_errors`）；"配置解析但缺鉴权"的模型在 `all` 不在 `available`；模型选择与 `--list-models` 只展示 `available`，`composition_errors` 作为可展开诊断。
- **R-PRV-04（MUST）**：`cost.tiers` 分层定价（整套替换费率、判据 `input + cacheRead + cacheWrite`、最高阈值胜出、整请求适用）+ `thinking_level_map` 三态映射（省略/字符串/null），接入 `usage_json` 成本归因。
- **R-EVL-01（MUST）**：`Harness` 抽象（`name + run(input) -> { output, usage, timings, events }`）与 `createRCodeHarness()`（隔离临时 workspace + 复用 `createAgentSessionServices` 同源工厂 + `thinkingLevel` 固定 off + 隔离检查硬断言：无意外扩展加载即抛错）。
- **R-EVL-02（MUST）**：`Judge` 抽象（`scoringFn -> { score: 0..1, rationale }`），内置确定性 Judge（测试通过率 / 改动面 / 测试文件完整性），留 LLM Judge 扩展点；多条失败原因累积而非单一布尔。
- **R-EVL-03（MUST）**：`evalHarnessTable` 配对对比（baseline/candidate/repetitions），`groupKey = 输入标识（优先 input.id，否则规范化 JSON SHA-256）+ 重复轮次`；Pass Rate Lift（通过 = `score >= 1`）、配对差值（Token/耗时/成本逐对算差，缺失跳过非 0）、五类诊断（缺失/重复/harness 错/缺分/不可打分）单独列出。
- **R-EVL-04（MUST）**：评估中"绝不能发生"的安全红线（执行破坏性命令、越权访问）用硬断言而非 Judge 打分，触发即 fail 并停止；沙箱临时目录无网络/无敏感目录/无扩展。
- **R-CCH-01（MUST）**：`split_deferred_tools` 把中途新增的工具序列化到 transcript 尾而非工具定义前缀；已被实际调用过的工具不搬移；所有工具都被判 deferred 时无条件回退 immediate。
- **R-CCH-02（MUST）**：Deferred Tools 能力探测默认关闭 + 白名单开启（仅支持 tool reference 的 provider/模型启用）；不支持时不影响请求正确性。
- **R-EXT-01（MUST）**：`Skill` 资源（目录 + `SKILL.md` frontmatter）扫描全局（AppData）与项目（`.r-code/skills/`，统一现有 `.agents/skills` 语义与入口）。
- **R-EXT-02（MUST）**：技能渐进式披露——系统提示词只注入名称 + 一行描述；所选工具集不含 `read` 时不注入技能列表。
- **R-EXT-03（MUST）**：`Extension` 生命周期事件面（`session_start`/`tool_before`/`tool_after`/`agent_settled` 等，从 `AgentEvent` 订阅面派生）+ 注册自定义工具经 `ToolGateway` 同源入口；R3/R4 仍受拒绝矩阵（扩展不能绕过安全边界）。
- **R-EXT-04（MUST）**：热重载（`/reload` 或设置页触发），清模块/资源缓存重载，拿到最新内容。
- **R-SES-01（MUST）**：SessionEvent 在类型层显式区分"进入 LLM 上下文"与"仅持久化"，编译期杜绝纯 UI/审计 entry 误发 LLM。
- **R-SES-02（MUST）**：压缩 entry 增加 `retained_tail`（物化保留消息）形成自包含检查点；新压缩写新格式、旧格式（`firstKeptEntryId` 指针）兼容读，加载时迁移。
- **R-SES-03（MUST）**：恢复从最后一个压缩 entry 的 `retained_tail` 自包含重建上下文，与回溯整段 JSONL 的结果逐字节一致。
- **R-TEL-01（MUST）**：`TelemetryContext` 契约（Span + start/end attributes + 属性/事件/状态），提供 `NOOP`（默认，零开销）与 `InMemory`（测试参考）实现。
- **R-TEL-02（MUST）**：原生 `LlmAgentRuntime` 与 Codex 适配层统一打 `r_code.ai.request` / `r_code.harness.run` 两条 Span；`usage_json` 成本归因从 Span 提取。
- **R-TEL-03（MUST）**：Adapter 一致性测试套件（原子性、状态合并、嵌套父子关系）。
- **R-SBX-01（MUST）**：`CommandExecutionBackend` trait（spawn / kill_tree / 输出收集），默认实现 = 现有五级 shell 链；未启用时零行为变化。
- **R-SBX-02（MUST）**：`DockerBackend` 可选后端 + 设置项 `execution.container` 仅全局；启用后命令在容器内执行，Host 侧审批矩阵、风险分级、审计不变。
- **R-TUI-01（MUST）**：`r-code-tui` 独立 `[[bin]]` 复用 Host 编排模块（不启动 WebView），默认共享桌面 data-dir；阶段 1 最小对话（消息流 + 流式 assistant + 工具卡折叠 + 输入 + 发送/steer/abort）。
- **R-TUI-02（MUST）**：snapshot 权威 vs 事件瞬时——TUI 渲染层不累积状态副本，回放/恢复走 JSONL 重建，完成时 flush 全量历史。
- **R-TUI-03（MUST）**：内联审批与风险分级同源（复用 `ToolGateway` R0-R4，AllowAlways 精确目标语义），不在 TUI 层重新实现安全。
- **R-TUI-04（MUST）**：IME 候选窗定位——假光标 + 硬件光标定位 + 容器焦点传播（中文输入候选窗出现在正确位置）。
- **R-TUI-05（SHOULD）**：长会话 turn 级窗口化（不整帧重渲 transcript），滚动不卡顿。
- **R-TUI-06（SHOULD）**：fullscreen/regular 双态切换（备用屏 VStack/HStack 布局 + 全文搜索），默认 regular 不打破终端 scrollback 惯例。
- **R-TUI-07（SHOULD）**：`!command` 直接执行，复用 `r-code-terminal` OSC 133 区分 prompt/command/output，与 Agent 工具输出在 transcript 中正确区分。
- **R-DST-01（MUST）**：CLI 随安装包分发（`tauri.conf.json` `bundle.externalBin` 声明 `r-code-tui`），与 `r-code-host` 同目录、同签名、同版本；四种 bundle target 均含该二进制。
- **R-DST-02（MUST）**：分平台 PATH 接入（Windows 用户级 PATH 免提权 / macOS 符号链接 / Linux deb 入 `/usr/bin`）+ 卸载清理；PATH 写入失败降级提示、不阻断主程序安装，升级不重复追加。

### 4.2 关键机器合同

- **验收 Harness 接口**：`node scripts/verify-r-code-alignment.mjs --task <TASK_ID> --profile implementation`、`--through <MILESTONE_ID> --profile implementation`、`--through M8 --profile production`；exit 0 仅表示全部 required assertion 通过。
- **文档门禁**：`node scripts/verify-ai-worklist.mjs --document docs/pi-alignment/pi-alignment-and-tui-prd.md --freeze docs/pi-alignment/pi-alignment-and-tui-freeze.yaml --report artifacts/ai-tasks/verification/pi-alignment/implementation/worklist-gate.json --mode check`；`compute` 模式用于生成 freeze digest。
- **证据目录**：`artifacts/ai-tasks/evidence/pi-alignment/<TASK_ID>.yaml`、`artifacts/ai-tasks/verification/pi-alignment/<profile>/<task-or-milestone>.json`。
- **任务包**：`artifacts/ai-tasks/current.yaml`（模板 `artifacts/ai-tasks/templates/current-task.template.yaml`）。

## 5. 质量、性能与安全门禁

| 门禁 | 命令/判据 | 阶段 |
| --- | --- | --- |
| 文档门禁 | `verify-ai-worklist.mjs --mode check`：blocking=0、major=0、freeze digest 一致 | 每次修改文档后 |
| Rust 单测 | `cargo test --workspace --all-features` 全绿（2332 基线不降） | 每个 Rust 任务 |
| 静态检查 | `cargo clippy --workspace --all-targets --all-features -- -D warnings`、`cargo fmt --all -- --check` | 每个 Rust 任务 |
| 前端 | `npm test`、`npm run build`、`npm audit --package-lock-only --audit-level=high` | 触达前端的任务 |
| 缓存守卫 | `R_CODE_CACHE_GUARD=1 cargo test -p r-code-agent-worker --test cache_guard`（tail_avg ≥ 90%） | M3 及之后 |
| Windows 金集 | `node scripts/verify-windows-reliability.mjs fast`（44 条八类） | 累计回归 |
| Codex 交互 | `node scripts/verify-codex-interaction.mjs --through M4 --profile implementation` | 触达 Codex 的任务 |
| 安全负向 | 扩展工具/Docker 后端/TUI 审批的 R0-R4 拒绝矩阵与 PathGuard 逃逸测试 fail-closed | 对应任务 |
| 累计门禁 | `--through M8 --profile implementation` 返回 0 | 收口 |

## 6. 需求追踪表

| 需求 | 任务 | 验收断言 |
| --- | --- | --- |
| R-GEN-01 | M0-01 | `M0-01.A1`、`M0-01.A2`、`M0-01.A3` |
| R-GEN-02 | M0-02 | `M0-02.A1`、`M0-02.A2`、`M0-02.A3`、`M0-02.A4` |
| R-PRV-01 | M1-01 | `M1-01.A1`、`M1-01.A2`、`M1-01.A3` |
| R-PRV-02 | M1-02 | `M1-02.A1`、`M1-02.A2`、`M1-02.A3` |
| R-PRV-03 | M1-03 | `M1-03.A1`、`M1-03.A2`、`M1-03.A3` |
| R-PRV-04 | M1-04 | `M1-04.A1`、`M1-04.A2`、`M1-04.A3` |
| R-EVL-01 | M2-01 | `M2-01.A1`、`M2-01.A2`、`M2-01.A3` |
| R-EVL-02 | M2-02 | `M2-02.A1`、`M2-02.A2`、`M2-02.A3` |
| R-EVL-03 | M2-03 | `M2-03.A1`、`M2-03.A2`、`M2-03.A3`、`M2-03.A4` |
| R-EVL-04 | M2-04 | `M2-04.A1`、`M2-04.A2`、`M2-04.A3` |
| R-CCH-01 | M3-01 | `M3-01.A1`、`M3-01.A2`、`M3-01.A3` |
| R-CCH-02 | M3-02 | `M3-02.A1`、`M3-02.A2` |
| R-EXT-01 | M4-01 | `M4-01.A1`、`M4-01.A2`、`M4-01.A3` |
| R-EXT-02 | M4-02 | `M4-02.A1`、`M4-02.A2` |
| R-EXT-03 | M4-03 | `M4-03.A1`、`M4-03.A2`、`M4-03.A3` |
| R-EXT-04 | M4-04 | `M4-04.A1`、`M4-04.A2` |
| R-SES-01 | M5-01 | `M5-01.A1`、`M5-01.A2` |
| R-SES-02 | M5-02 | `M5-02.A1`、`M5-02.A2` |
| R-SES-03 | M5-03 | `M5-03.A1` |
| R-TEL-01 | M6-01 | `M6-01.A1`、`M6-01.A2`、`M6-01.A3` |
| R-TEL-02 | M6-02 | `M6-02.A1`、`M6-02.A2` |
| R-TEL-03 | M6-03 | `M6-03.A1` |
| R-SBX-01 | M7-01 | `M7-01.A1`、`M7-01.A2` |
| R-SBX-02 | M7-02 | `M7-02.A1`、`M7-02.A2`、`M7-02.A3` |
| R-TUI-01 | M8-01 | `M8-01.A1`、`M8-01.A2`、`M8-01.A3` |
| R-TUI-02 | M8-02 | `M8-02.A2` |
| R-TUI-03 | M8-02 | `M8-02.A1` |
| R-TUI-05 | M8-02 | `M8-02.A3` |
| R-TUI-04 | M8-03 | `M8-03.A1` |
| R-TUI-06 | M8-03 | `M8-03.A2` |
| R-TUI-07 | M8-03 | `M8-03.A3` |
| R-DST-01 | M8-04 | `M8-04.A1` |
| R-DST-02 | M8-04 | `M8-04.A2`、`M8-04.A3` |

<!-- AI_WORKLIST_NORMATIVE_END -->

<!-- AI_WORKLIST_CONTRACT_START -->

## 7. Verification Harness 与里程碑

### 7.1 唯一产品验收入口

> **性质澄清（消除歧义）**：本 Harness（`verify-r-code-alignment.mjs`）是 R-Code 自己的验收脚本，**只运行 R-Code 仓库内的自有测试**（Rust/前端/金集/缓存守卫/Codex 交互/评估重放），**不启动、不下载、不依赖开源项目 pi 的任何进程或代码**。"对齐 pi" 指本 PRD Part A 参考 Pi 的设计最佳实践来补齐 R-Code 的短板，pi 仅作为设计参照对象；脚本名中的 `alignment` 是"R-Code 能力对齐"之意。

M0-01 建立并由后续任务扩展：

```powershell
node scripts/verify-r-code-alignment.mjs --task <TASK_ID> --profile implementation
node scripts/verify-r-code-alignment.mjs --through <MILESTONE_ID> --profile implementation
node scripts/verify-r-code-alignment.mjs --through M8 --profile production
```

Harness 必须：

- 非交互运行；0 仅代表全部 required assertions 通过。
- 维护 assertion registry，支持 task、through、implementation/production profile。
- 编排 Rust unit/integration（gateway/core/agent-worker/terminal）、前端组件测试、金集 corpus runner、缓存守卫、codex-interaction 与评估重放脚本。
- 输出 `artifacts/ai-tasks/verification/pi-alignment/<profile>/<task-or-milestone>.json` 和证据索引。
- 报告 revision/worktree digest、provider capability、失败断言；不记录 secret 与用户环境细节。
- required fixture/metric 缺失视为失败；不得删测试/降阈值/改 fixture 真值修绿。

M0-01 自身在 Harness 尚未存在时，先用任务卡列出的直接测试命令验收；随后必须用新 Harness 自验证一次。

### 7.2 里程碑

| 里程碑 | 能力出口 | 累计门禁 |
| --- | --- | --- |
| M0 验收地基 | 统一 Harness、回归基线登记 | `--through M0 --profile implementation` |
| M1 Provider 治理 | compat 兼容层、声明式端点、三态快照、分层定价 | `--through M1 --profile implementation` |
| M2 行为级评估 | Harness/Judge/配对统计、金集配对基线 | `--through M2 --profile implementation` |
| M3 缓存主动优化 | Deferred Tools 分流 + 能力探测 | `--through M3 --profile implementation` |
| M4 扩展/技能 | 技能扫描、渐进式披露、扩展事件、热重载 | `--through M4 --profile implementation` |
| M5 会话数据模型 | 上下文正交标记、自包含检查点、自包含恢复 | `--through M5 --profile implementation` |
| M6 遥测契约 | TelemetryContext、双引擎 Span、Adapter 一致性 | `--through M6 --profile implementation` |
| M7 沙箱化运行 | CommandExecutionBackend、DockerBackend | `--through M7 --profile implementation` |
| M8 TUI 与分发 | r-code-tui、对齐 Pi 交互面、IME、分发安装 | `--through M8 --profile implementation` |

## 8. 主 Checklist（唯一状态源）

- [x] **M0-01** 建立统一验收 Harness 与文档门禁。证据：`artifacts/ai-tasks/evidence/pi-alignment/M0-01.yaml`
- [x] **M0-02** 回归基线登记（Rust/前端/金集/Codex 交互）。证据：`artifacts/ai-tasks/evidence/pi-alignment/M0-02.yaml`
- [x] **M1-01** ProviderCompat 声明式兼容层与两级合成。证据：`artifacts/ai-tasks/evidence/pi-alignment/M1-01.yaml`
- [x] **M1-02** 声明式端点接入与值解析。证据：`artifacts/ai-tasks/evidence/pi-alignment/M1-02.yaml`
- [x] **M1-03** ModelAvailability 三态快照与设置页暴露。证据：`artifacts/ai-tasks/evidence/pi-alignment/M1-03.yaml`
- [x] **M1-04** cost.tiers 分层定价与 thinking_level_map。证据：`artifacts/ai-tasks/evidence/pi-alignment/M1-04.yaml`
- [x] **M2-01** Harness 抽象与 createRCodeHarness。证据：`artifacts/ai-tasks/evidence/pi-alignment/M2-01.yaml`
- [x] **M2-02** Judge 抽象与确定性 Judge。证据：`artifacts/ai-tasks/evidence/pi-alignment/M2-02.yaml`
- [x] **M2-03** evalHarnessTable 配对统计。证据：`artifacts/ai-tasks/evidence/pi-alignment/M2-03.yaml`
- [x] **M2-04** 首个金集配对基准接入门禁。证据：`artifacts/ai-tasks/evidence/pi-alignment/M2-04.yaml`
- [x] **M3-01** Deferred Tools 分流。证据：`artifacts/ai-tasks/evidence/pi-alignment/M3-01.yaml`
- [x] **M3-02** 能力探测白名单与 cache_guard 扩展。证据：`artifacts/ai-tasks/evidence/pi-alignment/M3-02.yaml`
- [x] **M4-01** Skill 资源扫描。证据：`artifacts/ai-tasks/evidence/pi-alignment/M4-01.yaml`
- [x] **M4-02** 技能渐进式披露。证据：`artifacts/ai-tasks/evidence/pi-alignment/M4-02.yaml`
- [x] **M4-03** Extension 事件面与工具注册。证据：`artifacts/ai-tasks/evidence/pi-alignment/M4-03.yaml`
- [x] **M4-04** 热重载。证据：`artifacts/ai-tasks/evidence/pi-alignment/M4-04.yaml`
- [x] **M5-01** 上下文正交标记。证据：`artifacts/ai-tasks/evidence/pi-alignment/M5-01.yaml`
- [x] **M5-02** retained_tail 自包含检查点。证据：`artifacts/ai-tasks/evidence/pi-alignment/M5-02.yaml`
- [x] **M5-03** 自包含恢复。证据：`artifacts/ai-tasks/evidence/pi-alignment/M5-03.yaml`
- [x] **M6-01** TelemetryContext 契约。证据：`artifacts/ai-tasks/evidence/pi-alignment/M6-01.yaml`
- [x] **M6-02** 双引擎统一打点。证据：`artifacts/ai-tasks/evidence/pi-alignment/M6-02.yaml`
- [x] **M6-03** Adapter 一致性测试。证据：`artifacts/ai-tasks/evidence/pi-alignment/M6-03.yaml`
- [x] **M7-01** CommandExecutionBackend trait。证据：`artifacts/ai-tasks/evidence/pi-alignment/M7-01.yaml`
- [x] **M7-02** DockerBackend 与设置项。证据：`artifacts/ai-tasks/evidence/pi-alignment/M7-02.yaml`
- [x] **M8-01** r-code-tui 独立 bin 与阶段 1 最小对话。证据：`artifacts/ai-tasks/evidence/pi-alignment/M8-01.yaml`
- [x] **M8-02** 阶段 2 对齐 Pi 交互面。证据：`artifacts/ai-tasks/evidence/pi-alignment/M8-02.yaml`
- [x] **M8-03** 阶段 3 fullscreen 与 IME。证据：`artifacts/ai-tasks/evidence/pi-alignment/M8-03.yaml`
- [x] **M8-04** 分发安装与 PATH 接入。证据：`artifacts/ai-tasks/evidence/pi-alignment/M8-04.yaml`

## 9. 详细任务卡

### M0-01 建立统一验收 Harness 与文档门禁

- 结果：`scripts/verify-r-code-alignment.mjs` 支持 `--task/--through/--profile`，`docs/pi-alignment/pi-alignment-and-tui-freeze.yaml` 固化清单就位，文档门禁 `--mode check` 通过。
- 需求引用：§4.1 R-GEN-01。
- 依赖：无。
- 前置事实：已有 `scripts/verify-ai-worklist.mjs`（通用门禁，compute/check 两模式）、`artifacts/ai-tasks/templates/`（current-task/task-evidence 模板）、`artifacts/ai-tasks/verification/pi-alignment/` 目录可建。
- 固定约束：非交互；0 仅表示全部 required assertion 通过；required 缺失 = 失败；不记录 secret；report 必须含 revision/worktree digest 与失败断言列表。
- 决策空间：脚本语言沿用仓库既有 Node（`*.mjs`），断言 registry 用 JSON/YAML 内联皆可；`--through` 语义 = 该里程碑及之前所有任务断言。
- 产物：`scripts/verify-r-code-alignment.mjs`、`docs/pi-alignment/pi-alignment-and-tui-freeze.yaml`、`artifacts/ai-tasks/verification/pi-alignment/implementation/worklist-gate.json`。
- 实施步骤：
  1. 只读预检：确认 `verify-ai-worklist.mjs` 参数契约、模板字段、verification 目录现状。
  2. 写 `verify-r-code-alignment.mjs`：解析 `--task/--through/--profile`；缺参打印用法并 exit 2；assertion registry 初始为空表（后续任务注册）；`--through` 求里程碑闭包。
  3. 写 `docs/pi-alignment/pi-alignment-and-tui-freeze.yaml`（schema `ai-worklist-freeze.v1`，status 先 `draft`）。
  4. 用 `verify-ai-worklist.mjs --mode compute` 生成 digest 回填 freeze；再 `--mode check` 验证通过；将 freeze `status` 改为 `frozen`、`completion_gate.passed=true`、`blocking_issues=0`、`major_issues=0`。
  5. 用新 Harness 自验证：`--task M0-01 --profile implementation` 返回 0 且产出 JSON 报告。
- 验收断言：`M0-01.A1`（Harness 三参数解析、缺参 exit 2）、`M0-01.A2`（文档门禁 check 通过：freeze digest 一致、blocking=0、major=0）、`M0-01.A3`（报告含 revision/worktree digest 与失败断言列表）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M0-01 --profile implementation`；文档门禁 `node scripts/verify-ai-worklist.mjs --document docs/pi-alignment/pi-alignment-and-tui-prd.md --freeze docs/pi-alignment/pi-alignment-and-tui-freeze.yaml --report artifacts/ai-tasks/verification/pi-alignment/implementation/worklist-gate.json --mode check`。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M0-01.yaml`、`artifacts/ai-tasks/verification/pi-alignment/implementation/M0-01.json`。
- 失败处理：门禁报 digest mismatch 时用 `--mode compute` 重新固化并核对未发生规范性改动；报告缺字段时补全 report 生成逻辑。

### M0-02 回归基线登记

- 结果：改造前基线可复跑，四条基线命令全部通过并记录 revision。
- 需求引用：§4.1 R-GEN-02。
- 依赖：M0-01。
- 前置事实：`docs/architecture.md §14` 列出本地最小验证命令；单测基线 2332；金集 44 条。
- 固定约束：不得为建立基线而修改任何测试/阈值；基线结果只记录不修绿。
- 决策空间：基线命令以 `architecture.md §14` 为准；记录格式用任务卡 evidence 的 `assertions` 列表承载每条命令与 exit code。
- 产物：基线证据（四条命令的退出码 + revision + worktree digest）。
- 实施步骤：
  1. `cargo test --workspace --all-features` 记录 exit 0 与 revision。
  2. 前端 `cd src-tauri/frontend && npm ci && npm test && npm run build` 记录 exit 0。
  3. `node scripts/verify-windows-reliability.mjs fast` 记录 exit 0。
  4. `node scripts/verify-codex-interaction.mjs --through M4 --profile implementation` 记录 exit 0。
  5. 回填 `M0-02` evidence，注册四条基线断言到 Harness registry 供后续累计回归。
- 验收断言：`M0-02.A1`（cargo test 全绿）、`M0-02.A2`（前端 npm test + build 绿）、`M0-02.A3`（金集 fast 绿）、`M0-02.A4`（codex-interaction M4 绿）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M0-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M0-02.yaml`。
- 失败处理：任一基线失败视为回归已存在，先定位是否工作区脏改动导致；非本清单改动引发的既有失败单独记录不阻断（需注明外部原因）。

### M1-01 ProviderCompat 声明式兼容层与两级合成

- 结果：`ProviderCompat` 结构体存在，provider 级 + model 级合并，硬编码默认 + 用户 compat 覆盖，用户不覆盖厂商直连安全行为。
- 需求引用：§4.1 R-PRV-01。
- 依赖：M0-02。
- 前置事实：能力探测现状在 `vendor/agent-contracts/crates/agent-llm/src/dialect.rs`（四布尔）；预设 `src-tauri/src/provider_catalog.rs`。
- 固定约束：DeepSeek 等内置 provider 硬编码默认保留；compat 不改变现有工具安全/路径行为；合并语义 = model 覆盖 provider。
- 决策空间：`ProviderCompat` 落在 `r-code-core`（DTO）还是 `provider_catalog` 由仓库分层决定；字段集按 R-Code 实际 provider 面裁剪（不必照搬 Pi 全量）。
- 产物：`ProviderCompat` 结构 + 合并函数 + 单测。
- 实施步骤：
  1. 预检：读 `dialect.rs` 四布尔与 `provider_catalog.rs` 预设结构，确认现有能力探测分发点。
  2. 定义 `ProviderCompat`（字段见 R-PRV-01，默认值 = 硬编码现状）。
  3. 实现 `merge(base, override)`（model 覆盖 provider 级）；接入 dialect 合成点（硬编码默认 + 用户 compat 覆盖）。
  4. 单测：合并语义、DeepSeek 默认不被覆盖、缺省字段走硬编码。
  5. 注册断言到 Harness。
- 验收断言：`M1-01.A1`（结构体与字段集完整）、`M1-01.A2`（provider/model 合并单测通过）、`M1-01.A3`（DeepSeek `supports_prompt_caching` 不被用户 compat 覆盖）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M1-01 --profile implementation`；`cargo test -p r-code-core`（或所在 crate）。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M1-01.yaml`。
- 失败处理：合并语义错误先看单测；若发现 dialect 分发点有未文档化的隐藏分支，先补事实再改。

### M1-02 声明式端点接入与值解析

- 结果：`baseUrl + api + apiKey 引用 + models` 配置可接入任意 OpenAI 兼容端点，值解析支持 `$ENV` 与凭据引用，不引入明文落盘。
- 需求引用：§4.1 R-PRV-02。
- 依赖：M1-01。
- 前置事实：密钥存平台凭据后端（`architecture.md §12`）；`provider_kind` 稳定身份；`provider_catalog.rs` 预设。
- 固定约束：不引入明文密钥落盘；`provider_kind` 不因改名/改 URL 变化；声明式端点不覆盖厂商直连安全行为。
- 决策空间：配置文件位置与 schema 沿用现有 config 机制；值解析 `$ENV`/凭据引用复用现有密钥引用解析（若已存在则直接复用）。
- 产物：声明式 provider 配置 schema + 解析器 + 单测 + 文档示例。
- 实施步骤：
  1. 预检：读现有 Provider 配置加载与密钥后端，确认可复用的引用解析。
  2. 定义最小声明式配置（baseUrl/api/apiKey 引用/models）与值解析规则（`$ENV`、凭据引用、字面量）。
  3. 接入 provider 目录构建，使声明式端点进入模型列表。
  4. 单测：最小配置接入、`$ENV` 解析、凭据引用不落明文、`provider_kind` 稳定。
  5. 注册断言到 Harness。
- 验收断言：`M1-02.A1`（最小配置接入后 `--list-models` 列出）、`M1-02.A2`（值解析 `$ENV`/凭据引用且无明文落盘）、`M1-02.A3`（`provider_kind` 改名/改 URL 不变）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M1-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M1-02.yaml`。
- 失败处理：值解析有歧义时以不落明文为优先，宁可报错不静默存明文。

### M1-03 ModelAvailability 三态快照与设置页暴露

- 结果：`all`/`available`/`composition_errors` 三态，模型选择只展示 `available`，`composition_errors` 可展开诊断。
- 需求引用：§4.1 R-PRV-03。
- 依赖：M1-02。
- 前置事实：模型发现 `provider_models.rs`；`/model` 与 `--list-models` 展示路径；设置页模型服务页（`SettingsScene`/模型服务面板）。
- 固定约束：三态语义 = 加载成功 / 有鉴权可用 / 组装失败原因；"配置解析但缺鉴权"必须在 `all` 不在 `available`。
- 决策空间：快照类型放 `r-code-core` DTO；前端诊断展开用现有 Drawer/InfoTip 组件。
- 产物：`ModelAvailabilitySnapshot` 类型 + 构建逻辑 + 前端展示 + 单测。
- 实施步骤：
  1. 预检：读 `provider_models.rs` 与设置页模型列表数据流。
  2. 定义三态快照；模型列表与 `--list-models` 改为只取 `available`；`composition_errors` 附诊断文案。
  3. 前端模型选择列表只渲染 `available`，缺鉴权/组装失败项提供可展开诊断。
  4. 单测：缺鉴权模型在 all 不在 available、组装失败进入 composition_errors。
  5. 注册断言。
- 验收断言：`M1-03.A1`（三态快照结构完整）、`M1-03.A2`（缺鉴权在 all 不在 available）、`M1-03.A3`（设置页只展示 available 且诊断可展开）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M1-03 --profile implementation`；前端组件测试。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M1-03.yaml`。
- 失败处理：前端诊断不可达时先修数据流（后端 → typed IPC → 组件），不绕过。

### M1-04 cost.tiers 分层定价与 thinking_level_map

- 结果：`cost.tiers`（整套替换、判据 input+cacheRead+cacheWrite、最高阈值胜出）+ `thinking_level_map` 三态，接入 `usage_json` 成本归因。
- 需求引用：§4.1 R-PRV-04。
- 依赖：M1-01。
- 前置事实：`usage_json` 成本归因现有实现；DeepSeek 前缀缓存 usage 解析（`architecture.md §6.3`）。
- 固定约束：tier 整套替换而非部分覆盖；判据与"整请求适用"语义不变；`thinking_level_map` 的 `null` 档在 UI 隐藏/切换跳过。
- 决策空间：定价模型放 provider 模型描述；思考等级映射接入现有 Shift+Tab/模型切换路径。
- 产物：`CostTier`/`ThinkingLevelMap` 结构 + 归因接入 + 单测。
- 实施步骤：
  1. 预检：读 usage 解析与思考等级映射现状。
  2. 定义 tier（整套替换 + 判据 + 最高阈值胜出）与三态 map。
  3. 接入成本归因：命中 tier 时按整套费率算，`null` 档跳过。
  4. 单测：tier 阈值边界、整套替换、三态 map。
  5. 注册断言。
- 验收断言：`M1-04.A1`（tier 语义单测通过）、`M1-04.A2`（thinking_level_map 三态、null 档隐藏）、`M1-04.A3`（成本归因接入 usage_json）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M1-04 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M1-04.yaml`。
- 失败处理：成本归因偏差先核对 tier 判据（是否把 cacheWrite/cacheRead 漏计）。

### M2-01 Harness 抽象与 createRCodeHarness

- 结果：`Harness` trait + `createRCodeHarness()`（隔离临时 workspace、同源工厂、thinkingLevel off、隔离检查硬断言）。
- 需求引用：§4.1 R-EVL-01。
- 依赖：M0-02。
- 前置事实：`createAgentSessionServices`（`architecture.md §13` 提及同源工厂）、`bin/plan_eval.rs` 三臂环境隔离、`src-tauri/src/bin/plan_eval.rs`。
- 固定约束：隔离检查（无意外扩展加载）必须硬断言 throw；thinkingLevel 固定 off；与生产同一套工厂。
- 决策空间：评估框架落地位置（独立 crate `r-code-evals` 或 `bin/agent_eval.rs`），按仓库分层选改动面最小者。
- 产物：`Harness` 抽象 + `createRCodeHarness` + 单测。
- 实施步骤：
  1. 预检：读 `plan_eval.rs` 的隔离环境构造，确认可复用组件。
  2. 定义 `Harness` trait（name + run -> {output, usage, timings, events}）。
  3. 实现 `createRCodeHarness`：临时 workspace + `createAgentSessionServices` 同源 + thinkingLevel off + 隔离检查硬断言。
  4. 单测：隔离环境无扩展、stopReason 非 stop 判失败。
  5. 注册断言。
- 验收断言：`M2-01.A1`（Harness 抽象签名完整）、`M2-01.A2`（隔离 workspace + thinkingLevel off + 同源工厂）、`M2-01.A3`（隔离检查硬断言触发即 throw）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M2-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M2-01.yaml`。
- 失败处理：隔离检查未触发说明临时目录设置错误，修目录隔离而非放宽断言。

### M2-02 Judge 抽象与确定性 Judge

- 结果：`createJudge` + 确定性 Judge（测试通过率/改动面/测试完整性），LLM Judge 扩展点。
- 需求引用：§4.1 R-EVL-02。
- 依赖：M2-01。
- 前置事实：无现成 Judge 抽象。
- 固定约束：score ∈ [0,1] + rationale；多条失败原因累积；确定性规则优先。
- 决策空间：Judge 落地在评估框架 crate 内；LLM Judge 以评分函数注入实现（不默认接真实模型）。
- 产物：`createJudge` + 三个内置确定性 Judge + 单测。
- 实施步骤：
  1. 定义 `Judge`（scoringFn -> {score, rationale}），失败累积数组。
  2. 实现 TestPassJudge/FocusJudge/IntegrityJudge（确定性规则）。
  3. 单测：三元判定、失败累积 rationale、LLM 扩展点签名。
  4. 注册断言。
- 验收断言：`M2-02.A1`（score 0..1 + rationale + 失败累积）、`M2-02.A2`（TestPassJudge 确定性可复现）、`M2-02.A3`（LLM Judge 扩展点存在）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M2-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M2-02.yaml`。
- 失败处理：确定性 Judge 不稳先查 fixture 真值，不因随机性改确定性为 LLM 打分。

### M2-03 evalHarnessTable 配对统计

- 结果：`evalHarnessTable`（baseline/candidate/repetitions）+ groupKey + Pass Rate Lift + 配对差值 + 五类诊断。
- 需求引用：§4.1 R-EVL-03。
- 依赖：M2-02。
- 前置事实：无现成配对统计；`groupKey` 输入标识可用 input.id 或规范化 JSON SHA-256。
- 固定约束：通过 = score >= 1；配对差值逐对算差、缺失跳过非 0；五类诊断单独列出。
- 决策空间：统计逻辑独立函数便于单测；报告格式对齐现有 verify 脚本 JSON 报告。
- 产物：配对矩阵生成 + 统计 + 诊断 + 单测。
- 实施步骤：
  1. 实现 `evalHarnessTable`：repetitions × harness 矩阵，每行注入 groupKey。
  2. 实现 groupKey（input.id 优先否则规范化 JSON SHA-256 + 重复轮次）。
  3. 实现 Pass Rate Lift、配对差值、五类诊断。
  4. 单测：groupKey 稳定性、配对正确性、缺失跳过非 0、诊断分类。
  5. 注册断言。
- 验收断言：`M2-03.A1`（groupKey 规则正确）、`M2-03.A2`（Pass Rate Lift 计算正确）、`M2-03.A3`（配对差值缺失跳过非 0）、`M2-03.A4`（五类诊断单独列出）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M2-03 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M2-03.yaml`。
- 失败处理：配对张冠李戴先查 groupKey 规范化是否含易变字段。

### M2-04 首个金集配对基准接入门禁

- 结果：Windows 金集转为 baseline vs candidate 可跑评估，安全红线硬断言，产物可回放。
- 需求引用：§4.1 R-EVL-04。
- 依赖：M2-03。
- 前置事实：金集 `crates/r-code-gateway/tests/command_corpus/` 44 条；`verify-windows-reliability.mjs fast`。
- 固定约束：安全红线（破坏性命令）硬断言触发即 fail；产物（会话 JSONL + 报告）可回放；评估作为本地门禁非 CI 阻断。
- 决策空间：首个基准用"命令可靠性策略"前后对比或固定 prompt 的 baseline/candidate 二臂；重复次数按纯代码任务低随机性取 3~5。
- 产物：金集配对评估脚本 + 报告 + 硬断言 + 证据。
- 实施步骤：
  1. 把金集 44 条包装为评估输入（input.id = 命令编号）。
  2. baseline/candidate 二臂（如：诊断提示 on/off 或方言策略前后），`repetitions=3`。
  3. 跑评估产出 Pass Rate Lift + 配对差值；加安全红线硬断言。
  4. 接入 Harness registry 作为 M2 累计门禁。
  5. 注册断言。
- 验收断言：`M2-04.A1`（配对评估可本地一键复跑并输出 Lift + 差值）、`M2-04.A2`（安全红线硬断言触发即 fail）、`M2-04.A3`（会话 JSONL + 报告可回放）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M2-04 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M2-04.yaml`。
- 失败处理：评估依赖真实模型时用 mock/fake Provider 保证可复现；真实模型复测列为外部放行。

### M3-01 Deferred Tools 分流

- 结果：`split_deferred_tools` 把中途新增工具放 transcript 尾，已调用不搬移，空 immediate 回退。
- 需求引用：§4.1 R-CCH-01。
- 依赖：M0-02。
- 前置事实：`cache_shape.rs` PrefixShape；`LlmAgentRuntime` 每轮请求组装（`llm_runtime.rs`）；工具注册时序。
- 固定约束：分流不减少发送正确性；已调用工具不搬移；空 immediate 无条件回退。
- 决策空间：`addedToolNames` 等价信号从现有工具注册/委派时序推导；transcript 尾序列化对齐 DeepSeek 字节缓存语义。
- 产物：`split_deferred_tools` + 接入请求组装 + 单测。
- 实施步骤：
  1. 预检：定位工具注册与请求组装的"工具定义前缀"构造点。
  2. 实现分流（immediate vs deferred）+ 已调用判定 + 回退。
  3. 接入每轮请求组装：deferred 工具进 transcript 尾。
  4. 单测：分流、已调用不搬移、空 immediate 回退。
  5. 注册断言。
- 验收断言：`M3-01.A1`（分流逻辑正确）、`M3-01.A2`（已调用不搬移）、`M3-01.A3`（空 immediate 回退）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M3-01 --profile implementation`；`cargo test -p r-code-agent-worker`。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M3-01.yaml`。
- 失败处理：分流导致请求非法先查回退路径是否触发。

### M3-02 能力探测白名单与 cache_guard 扩展

- 结果：Deferred Tools 白名单开启，`cache_guard` 新增"中途新增工具不击穿"用例。
- 需求引用：§4.1 R-CCH-02。
- 依赖：M3-01。
- 前置事实：`tests/cache_guard.rs`（tail_avg ≥ 90%，env 开关）；`dialect.rs` supports_* 能力探测。
- 固定约束：默认关闭 + 白名单；不支持 tool reference 的 provider/模型不启用；不破坏正确性。
- 决策空间：能力探测复用 dialect 或 M1 compat（若已落地优先 compat）。
- 产物：能力探测 + cache_guard 扩展用例 + 单测。
- 实施步骤：
  1. 定义 tool-reference 能力探测（默认 false）。
  2. 白名单接入 deferred tools 启用判断。
  3. cache_guard 新增用例：中途注册新工具后 tail_avg 仍 ≥ 阈值、不报 Tools 前缀变化。
  4. 单测：非白名单 provider 不启用。
  5. 注册断言。
- 验收断言：`M3-02.A1`（白名单能力探测正确）、`M3-02.A2`（cache_guard 新增用例通过）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M3-02 --profile implementation`；`R_CODE_CACHE_GUARD=1 cargo test -p r-code-agent-worker --test cache_guard`。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M3-02.yaml`。
- 失败处理：cache_guard tail_avg 不达标先查字节稳定性（tools 排序/冻结键）是否被本次改动破坏。

### M4-01 Skill 资源扫描

- 结果：全局（AppData）+ 项目（`.r-code/skills/`）扫描 `SKILL.md`，统一 `.agents/skills` 语义。
- 需求引用：§4.1 R-EXT-01。
- 依赖：M0-02。
- 前置事实：`.agents/skills` 子模块存在（构建期资产）；`ResourceLoader` 概念在 PRD 已定（本仓库无现成实现，需新建或等价物）。
- 固定约束：扫描全局 + 项目两级；SKILL.md frontmatter（name/description）解析；与现有 `.agents/skills` 入口统一。
- 决策空间：扫描器落 `r-code-core` 或 `src-tauri` 资源加载模块；目录约定 `.r-code/skills/` 为项目级。
- 产物：Skill 扫描器 + frontmatter 解析 + 单测。
- 实施步骤：
  1. 预检：确认 `.agents/skills` 现状与现有资源加载路径。
  2. 定义 `Skill`（目录 + SKILL.md frontmatter）。
  3. 实现全局 + 项目两级扫描；坏文件静默跳过（对齐 Pi 发现规则）。
  4. 单测：扫描、frontmatter 解析、坏文件跳过。
  5. 注册断言。
- 验收断言：`M4-01.A1`（两级扫描正确）、`M4-01.A2`（frontmatter 解析正确）、`M4-01.A3`（与 .agents/skills 语义统一）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M4-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M4-01.yaml`。
- 失败处理：与 .agents/skills 冲突时以现有仓库语义为准并记录决策。

### M4-02 技能渐进式披露

- 结果：系统提示词只注入名称 + 一行描述，无 `read` 工具不注入。
- 需求引用：§4.1 R-EXT-02。
- 依赖：M4-01。
- 前置事实：系统提示词构建（`system-prompt.ts` 对应 `buildSystemPrompt` 等价物，实际在 `llm_runtime.rs` 的提示词构造）。
- 固定约束：只注入名称 + 描述；无 read 不注入技能列表。
- 决策空间：接入现有系统提示词拼装点；描述行格式对齐现有工具清单风格。
- 产物：渐进式披露接入 + 单测。
- 实施步骤：
  1. 预检：定位系统提示词拼装与工具清单注入点。
  2. 技能列表改为名称 + 一行描述。
  3. 无 read 工具时跳过技能列表注入。
  4. 单测：披露内容、无 read 跳过。
  5. 注册断言。
- 验收断言：`M4-02.A1`（只注入名称 + 描述）、`M4-02.A2`（无 read 不注入）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M4-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M4-02.yaml`。
- 失败处理：注入点不唯一时用仓库搜索确认所有系统提示词构造路径。

### M4-03 Extension 事件面与工具注册

- 结果：生命周期事件面 + 自定义工具经 ToolGateway，R3/R4 不绕过。
- 需求引用：§4.1 R-EXT-03。
- 依赖：M4-01。
- 前置事实：`AgentEvent` 订阅面；`ToolGateway` 单一执行入口（`gateway.rs`）；`beforeToolCall` 等价物在 `agent_loop.rs`。
- 固定约束：注册工具经 Gateway 同源入口；R3/R4 仍受拒绝矩阵；扩展不能绕过 PathGuard。
- 决策空间：扩展事件从 `AgentEvent` 订阅面派生（session_start/tool_before/tool_after/agent_settled）；扩展定义文件格式对齐 Pi（TS 或声明式，按仓库实际选）。
- 产物：Extension 事件面 + 工具注册桥 + 安全负向单测。
- 实施步骤：
  1. 预检：读 `AgentEvent` 变体与 Gateway 工具注册入口。
  2. 定义扩展事件面与 `registerTool`（经 Gateway）。
  3. 实现扩展工具的执行体包装（注入 ExtensionContext 等价物）。
  4. 安全负向单测：扩展工具 R3/R4 仍被拒、PathGuard 逃逸 fail-closed。
  5. 注册断言。
- 验收断言：`M4-03.A1`（事件面完整）、`M4-03.A2`（工具经 Gateway 同源）、`M4-03.A3`（R3/R4 不绕过，安全负向通过）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M4-03 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M4-03.yaml`。
- 失败处理：安全负向失败即阻塞，不因功能通过而放宽。

### M4-04 热重载

- 结果：`/reload` 或设置页触发，清缓存重载拿最新内容。
- 需求引用：§4.1 R-EXT-04。
- 依赖：M4-03。
- 前置事实：`/reload` 斜杠命令路径（`slash-commands.ts` 等价物）；资源加载缓存。
- 固定约束：重载清模块/资源缓存；拿最新内容。
- 决策空间：热重载入口复用现有 `/reload` 命令或设置页按钮。
- 产物：热重载 + 单测。
- 实施步骤：
  1. 预检：定位斜杠命令解析与资源缓存位置。
  2. 实现清缓存重载。
  3. 单测：重载后内容更新。
  4. 注册断言。
- 验收断言：`M4-04.A1`（/reload 或设置页触发清缓存）、`M4-04.A2`（重载后拿到最新内容）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M4-04 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M4-04.yaml`。
- 失败处理：缓存未清先查缓存键是否含易变字段。

### M5-01 上下文正交标记

- 结果：SessionEvent 类型层区分"进上下文"与"仅持久化"。
- 需求引用：§4.1 R-SES-01。
- 依赖：M0-02。
- 前置事实：SessionEvent 变体（`dto.rs` + JSONL SessionStore）；`convertToLlm` 等价过滤逻辑。
- 固定约束：类型层显式区分；编译期杜绝纯 UI entry 误发 LLM。
- 决策空间：用类型（新 enum 变体）或标记字段（`exclude_from_context`）实现，选改动面最小且能静态区分者。
- 产物：正交标记 + 编译期约束 + 单测。
- 实施步骤：
  1. 预检：审计现有 SessionEvent 变体，列出进/不进上下文清单。
  2. 为"仅 UI/审计"变体补显式标记或拆分类型。
  3. 调整上下文构建按标记过滤。
  4. 单测：纯 UI entry 不进入 LLM 上下文。
  5. 注册断言。
- 验收断言：`M5-01.A1`（类型层区分显式）、`M5-01.A2`（编译期/单测杜绝误发）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M5-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M5-01.yaml`。
- 失败处理：改造影响现有持久化兼容时先做迁移测试。

### M5-02 retained_tail 自包含检查点

- 结果：压缩 entry 增加 `retained_tail`（物化保留消息），旧格式兼容读。
- 需求引用：§4.1 R-SES-02。
- 依赖：M5-01。
- 前置事实：压缩实现 `llm_runtime.rs:2957-3206`；压缩 entry 现有字段。
- 固定约束：新压缩写新格式（物化 retained_tail）；旧格式（firstKeptEntryId 指针）兼容读，加载时迁移。
- 决策空间：`retained_tail` 字段命名与 schema 版本对齐现有 JSONL schema 演进；保留 `firstKeptEntryId` 作向后兼容。
- 产物：压缩 entry 新字段 + 兼容读 + 迁移 + 单测。
- 实施步骤：
  1. 预检：读压缩 entry 结构与 JSONL 解析。
  2. 新压缩写 retained_tail（物化保留消息）。
  3. 兼容读旧格式，加载时迁移。
  4. 单测：新旧格式互读、迁移正确。
  5. 注册断言。
- 验收断言：`M5-02.A1`（新压缩写 retained_tail）、`M5-02.A2`（旧格式兼容读）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M5-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M5-02.yaml`。
- 失败处理：迁移破坏旧会话读取即阻塞，先修兼容读。

### M5-03 自包含恢复

- 结果：恢复从压缩点自包含重建，与回溯逐字节一致。
- 需求引用：§4.1 R-SES-03。
- 依赖：M5-02。
- 前置事实：`session_messages_for_branch`、恢复页（`architecture.md §9`）。
- 固定约束：自包含重建结果与回溯整段 JSONL 逐字节一致。
- 决策空间：恢复路径复用 `session_messages_for_branch`，新增自包含分支。
- 产物：自包含重建 + 一致性单测。
- 实施步骤：
  1. 预检：读恢复与上下文重建路径。
  2. 从最后压缩 entry 的 retained_tail 自包含重建。
  3. 单测：自包含重建 vs 回溯逐字节一致。
  4. 注册断言。
- 验收断言：`M5-03.A1`（自包含重建与回溯逐字节一致）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M5-03 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M5-03.yaml`。
- 失败处理：逐字节不一致先查 retained_tail 是否漏物化某类消息。

### M6-01 TelemetryContext 契约

- 结果：`TelemetryContext` 契约 + NOOP（默认零开销）+ InMemory。
- 需求引用：§4.1 R-TEL-01。
- 依赖：M0-02。
- 前置事实：结构化日志 + usage_json 现状；无 Span 契约抽象。
- 固定约束：Span + start/end attributes + 属性/事件/状态；NOOP 默认零开销。
- 决策空间：契约落 `r-code-core`；InMemory 供测试。
- 产物：契约 + NOOP + InMemory + 单测。
- 实施步骤：
  1. 定义 TelemetryContext（Span + attributes + NOOP/InMemory）。
  2. 单测：Span 生命周期、NOOP 零开销、InMemory 可断言。
  3. 注册断言。
- 验收断言：`M6-01.A1`（契约完整）、`M6-01.A2`（NOOP + InMemory 实现）、`M6-01.A3`（默认 NOOP 零开销）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M6-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M6-01.yaml`。
- 失败处理：零开销不达标先看是否默认误建 Span。

### M6-02 双引擎统一打点

- 结果：原生 + Codex 统一 `r_code.ai.request`/`r_code.harness.run` 两条 Span，usage_json 从 Span 提取。
- 需求引用：§4.1 R-TEL-02。
- 依赖：M6-01。
- 前置事实：原生 `LlmAgentRuntime` 请求路径；Codex 适配层（`codex_app_server.rs`/`codex_interaction.rs`）。
- 固定约束：双引擎同构 Span；usage_json 归因从 Span 提取。
- 决策空间：打点位置复用现有请求边界；Codex 侧映射到同构 Span 字段。
- 产物：双引擎打点 + 归因接入 + 单测。
- 实施步骤：
  1. 预检：定位双引擎请求边界与 usage 记录点。
  2. 原生打 `r_code.ai.request`/`r_code.harness.run`。
  3. Codex 侧映射同构 Span；usage_json 从 Span 提取。
  4. 单测：同构字段、归因一致。
  5. 注册断言。
- 验收断言：`M6-02.A1`（两条 Span 双引擎同构）、`M6-02.A2`（usage_json 从 Span 提取）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M6-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M6-02.yaml`。
- 失败处理：跨引擎对账不符先核对字段映射。

### M6-03 Adapter 一致性测试

- 结果：一致性测试（原子性、状态合并、嵌套父子）通过。
- 需求引用：§4.1 R-TEL-03。
- 依赖：M6-01。
- 前置事实：NOOP/InMemory 实现（M6-01）。
- 固定约束：原子性、状态合并、嵌套父子三语义各有用例。
- 决策空间：测试套件复用 InMemory；可为未来第三方 Adapter 提供一致性基准。
- 产物：一致性测试套件。
- 实施步骤：
  1. 写三语义用例（原子性/状态合并/嵌套父子）。
  2. 跑通 InMemory。
  3. 注册断言。
- 验收断言：`M6-03.A1`（三语义一致性测试通过）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M6-03 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M6-03.yaml`。
- 失败处理：测试失败先查契约是否把状态合并语义定义清楚。

### M7-01 CommandExecutionBackend trait

- 结果：`CommandExecutionBackend` trait（spawn/kill_tree/输出收集），默认实现 = 现有五级链。
- 需求引用：§4.1 R-SBX-01。
- 依赖：M0-02。
- 前置事实：五级 shell 解析链（`r-code-gateway/src/win_shell.rs`）；`process.rs` 进程组 kill；`gateway.rs` bash 执行。
- 固定约束：默认实现 = 现有五级链；未启用零行为变化。
- 决策空间：trait 落 `r-code-gateway`；抽象面最小（spawn/kill_tree/输出收集）。
- 产物：trait + 默认实现 + 单测。
- 实施步骤：
  1. 预检：读现有 bash 执行与 kill 路径。
  2. 定义 trait；默认实现包装现有五级链。
  3. 单测：默认实现与现有行为一致（金集 fast 绿）。
  4. 注册断言。
- 验收断言：`M7-01.A1`（trait 完整 + 默认实现 = 五级链）、`M7-01.A2`（未启用零行为变化，金集仍绿）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M7-01 --profile implementation`；`node scripts/verify-windows-reliability.mjs fast`。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M7-01.yaml`。
- 失败处理：默认实现行为漂移先跑金集定位。

### M7-02 DockerBackend 与设置项

- 结果：`DockerBackend` 可选后端 + `execution.container` 仅全局；启用后审批/风险/审计不变。
- 需求引用：§4.1 R-SBX-02。
- 依赖：M7-01。
- 前置事实：`defaultProjectTrust`/`httpProxy` 仅全局约束（`architecture.md §8` 设置边界）；审批矩阵在 `gateway.rs`。
- 固定约束：`execution.container` 仅全局；启用后命令在容器内执行，Host 审批/风险分级/审计不变；未启用零行为变化。
- 决策空间：Docker 后端用 `docker run` 封装，鉴权/审批仍在 Host。
- 产物：DockerBackend + 设置项 + 单测 + 安全负向。
- 实施步骤：
  1. 实现 DockerBackend（命令路由进容器，挂载最小工作区）。
  2. 设置项 `execution.container`（仅全局）。
  3. 单测：启用后审批/风险/审计不变；未启用默认后端。
  4. 安全负向：容器后端不绕过 R0-R4/PathGuard。
  5. 注册断言。
- 验收断言：`M7-02.A1`（DockerBackend 路由进容器）、`M7-02.A2`（execution.container 仅全局）、`M7-02.A3`（启用后审批/风险/审计不变）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M7-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M7-02.yaml`。
- 失败处理：无 Docker 环境时用 mock 验证后端选择逻辑；真实容器列为外部放行。

### M8-01 r-code-tui 独立 bin 与阶段 1 最小对话

- 结果：`r-code-tui` 独立 `[[bin]]` 复用 Host 编排（不启动 WebView），阶段 1 最小对话可用，共享 data-dir。
- 需求引用：§4.1 R-TUI-01。
- 依赖：M0-02。
- 前置事实：`src-tauri/src` Host 编排（`CommandState`/`AgentBridge`/`SessionStore` 装配）；`AgentEvent`；workspace 无 ratatui/crossterm（需新增）。
- 固定约束：独立 bin 复用 Host 编排模块不启动 WebView；默认 `--mode tui`；默认 data-dir 指向桌面同一 AppData；内核零改动。
- 决策空间：ratatui + crossterm 作为 workspace 依赖；bin 复用 `src-tauri` 内编排模块（若需重构出可复用模块则做最小抽取）。
- 产物：`r-code-tui` bin + 单列 transcript（消息/流式 assistant/工具卡折叠）+ 输入 + 发送/steer/abort + 单测。
- 实施步骤：
  1. 预检：读 Host 编排装配与 AgentEvent 出口，确认可复用边界。
  2. 新增 workspace 依赖 ratatui/crossterm；若编排逻辑耦合在 Tauri 层则最小抽取可复用模块。
  3. 实现阶段 1：消息流 + 流式 assistant + 工具卡折叠 + 输入 + 发送/steer/abort。
  4. 单测：事件 → widget 映射、发送/steer/abort 语义。
  5. 注册断言。
- 验收断言：`M8-01.A1`（独立 bin 复用 Host 编排不启动 WebView）、`M8-01.A2`（阶段 1 最小对话可用）、`M8-01.A3`（共享 data-dir，GUI 可 resume TUI 会话）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M8-01 --profile implementation`；`cargo test -p r-code-tui`（或所在 crate）。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M8-01.yaml`。
- 失败处理：编排模块耦合过重时做最小抽取，不复制实现；数据目录冲突先核对 AppData 路径解析。

### M8-02 阶段 2 对齐 Pi 交互面

- 结果：内联审批 + 风险分级同源、snapshot 权威、turn 级窗口化。
- 需求引用：§4.1 R-TUI-02、R-TUI-03、R-TUI-05。
- 依赖：M8-01。
- 前置事实：`ToolGateway` R0-R4 + AllowAlways；JSONL source of truth；前端 `Timeline.tsx` 窗口化策略可参考。
- 固定约束：内联审批复用 Gateway（不在 TUI 重实现安全）；渲染层不累积状态副本；长会话不整帧重渲。
- 决策空间：组件树对齐 PRD B2.1；窗口化策略对齐 `Timeline.tsx` 的 turn 级窗口 + memo。
- 产物：工具卡展开 + 内联审批卡 + 子代理编队 + 分支导航 + 模型/思考切换 + StatusBar + 单测。
- 实施步骤：
  1. 工具卡展开（截断 + fullOutputPath）。
  2. 内联审批卡（消费 PermissionRequest，AllowAlways 精确目标）。
  3. 子代理编队（DelegationTree 拓扑 + 呼吸灯）。
  4. `/tree` `/fork` 分支导航、模型/思考切换、StatusBar（usage 四桶 + 命中率）。
  5. turn 级窗口化；单测：审批同源、snapshot 权威、窗口化不丢内容。
  6. 注册断言。
- 验收断言：`M8-02.A1`（内联审批 + 风险分级同源）、`M8-02.A2`（snapshot 权威 vs 事件瞬时）、`M8-02.A3`（turn 级窗口化滚动不卡顿）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M8-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M8-02.yaml`。
- 失败处理：审批绕过即阻塞；窗口化丢内容先查窗口边界计算。

### M8-03 阶段 3 fullscreen 与 IME

- 结果：IME 假光标 + 候选窗定位、fullscreen/regular 双态、`!command` 区分。
- 需求引用：§4.1 R-TUI-04、R-TUI-06、R-TUI-07。
- 依赖：M8-02。
- 前置事实：`r-code-terminal` OSC 133 区分 prompt/command/output；crossterm 输入。
- 固定约束：IME 候选窗位置正确（假光标 + 硬件光标定位 + 容器焦点传播）；fullscreen/regular 两态切换。
- 决策空间：假光标用零宽 APC 序列等价物；硬件光标定位用 crossterm；`!command` 复用 r-code-terminal。
- 产物：IME 定位 + fullscreen 布局 + `!command` + 单测。
- 实施步骤：
  1. 实现 Focusable + 假光标 + 硬件光标定位 + 容器焦点传播。
  2. fullscreen/regular 两态切换（VStack/HStack + 全文搜索）。
  3. `!command` 直接执行（OSC 133 区分）。
  4. 单测：焦点传播、两态切换、!command 输出区分。
  5. 注册断言。
- 验收断言：`M8-03.A1`（IME 候选窗定位 + 焦点传播）、`M8-03.A2`（fullscreen/regular 切换）、`M8-03.A3`（!command 输出与工具输出区分）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M8-03 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M8-03.yaml`。
- 失败处理：IME 定位难以自动断言时用焦点传播单测 + 真机人工验证（人工项列外部放行）。

### M8-04 分发安装与 PATH 接入

- 结果：`bundle.externalBin` 声明 r-code-tui，四种 target 含该二进制；分平台 PATH 接入 + 卸载清理，降级不阻断安装。
- 需求引用：§4.1 R-DST-01、R-DST-02。
- 依赖：M8-01。
- 前置事实：`tauri.conf.json §bundle`；`installer/`（overlay + `r-code-installer-pack`）；`installer-hooks.nsh`。
- 固定约束：CLI 与 host 同目录/同签名/同版本；PATH 只写安装目录；降级不阻断主程序安装；升级不重复追加。
- 决策空间：externalBin 声明在 `tauri.conf.json`；NSIS 钩子写用户级 PATH（免提权）；macOS 符号链接 `~/.local/bin`。
- 产物：bundle 接线 + 分平台 PATH 脚本 + 卸载清理 + 验收记录。
- 实施步骤：
  1. 预检：读 `tauri.conf.json`、`installer-hooks.nsh`、`installer/src/main.rs` payload 校验。
  2. `tauri.conf.json` 增加 `bundle.externalBin` 声明 r-code-tui；确认 installer payload 校验纳入合法成员。
  3. NSIS `POSTINSTALL` 写用户 PATH、`POSTUNINSTALL` 清理（含升级防重复）；macOS 符号链接；Linux deb/AppImage 就位。
  4. 更新 `docs/support/operations/operations.md` 安装/卸载说明。
  5. 注册断言。
- 验收断言：`M8-04.A1`（externalBin 声明，四种 target 含 r-code-tui）、`M8-04.A2`（分平台 PATH 接入 + 卸载清理）、`M8-04.A3`（PATH 失败降级不阻断安装）。
- 验证：`node scripts/verify-r-code-alignment.mjs --task M8-04 --profile implementation`；`node scripts/check-installer-frontend.mjs`（若涉及 installer 前端）。
- 证据：`artifacts/ai-tasks/evidence/pi-alignment/M8-04.yaml`。
- 失败处理：四种 target 打包验证在 CI 全平台腿执行；本地可先验证 nsis/msi 的 externalBin 产物存在。

## 10. 连续执行、恢复与证据协议

### 10.1 固定循环

选择编号最小且依赖已满足的未完成 MUST → 建立/恢复 `current.yaml` → 实现一个可验证子步 → 更新 packet → 运行任务断言 → 失败则诊断/修复/换受约束方案 → 通过则运行累计门禁 → 归档 evidence → 勾选 §8 唯一 Checklist 并重算进度 → 立即进入下一项。里程碑、汇报、文档更新、测试通过均不是等待人工确认的节点。

### 10.2 证据规则

- 每个可验证子步后更新 `current.yaml`：实际修改路径、已完成/剩余步骤、已完成/剩余断言、失败尝试、关键决定。
- 完成项证据真实存在且可关联当前实现；"证据待生成"只出现在未完成项。
- 不记录隐藏推理，只记录可复核的选择、依据和结果；不写 secret。

### 10.3 自主决策与失败处理

- 可逆、仓库内、未扩张权限的选择按安全 > 正确 > 简单 > 一致 > 可测试 > 性能 > 新颖性自行决定并记录。
- 失败依次：定位根因 → 聚焦修复 → 重试 → 换受约束方案 → 隔离外部阻塞继续不依赖项。
- 缺少真实 Provider/容器/签名实机时，用 adapter/fake/fixture/local profile 做到 `implementation_verified`，真实放行保持未满足；不中途停掉整个编码任务。

## 11. 风险、兼容与外部放行

### 11.1 风险与回滚

- 每个任务改动面小、可独立回退；涉及持久化/schema 的任务（M5）必须先做兼容读 + 迁移测试。
- Provider/compat 改动（M1）不得改变既有 provider 直连行为；用金集与现有 provider 单测做回归护栏。
- TUI/沙箱/扩展（M4/M7/M8）新增入口不得绕过 `ToolGateway` 安全边界；安全负向测试失败即阻塞。

### 11.2 提交切片（建议，非门禁）

按里程碑 M0→M8 分切片提交；每个任务完成后单独 commit 或与同里程碑任务合并，保持可回退粒度。提交前确认当前分支（仓库有误提交 main 的既有纪律要求）。

### 11.3 外部放行（production profile）

| 项 | 说明 |
| --- | --- |
| 真实 Provider 复测 | M1/M3 的 compat/deferred-tools 在真实残缺端点与 DeepSeek 实机上的行为验证 |
| 真实容器环境 | M7 DockerBackend 在真实 Docker 环境的端到端验证 |
| 真实模型评估 | M2 评估用真实 Provider 跑（非 mock）确认 Pass Rate 口径 |
| macOS/Apple 签名实机 | M8-04 的 macOS 符号链接与签名实机验证 |
| IME 真机 | M8-03 的 IME 候选窗在真实终端/中文输入法下的人工验证 |

这些项不影响 `implementation_verified` 完成判定；仅当追求 `production_release_ready` 时才需满足。

<!-- AI_WORKLIST_CONTRACT_END -->
