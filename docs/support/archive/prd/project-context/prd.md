# R-Code 项目上下文引擎 PRD（Project Context Engine）

> 状态：`ready-to-implement`（三轮评审完成并修正并入：① 代码事实核对 ② 可实施性 ③ 功能明确性/完备性）
> 版本：v1.0（2026-09-30）。本文取代并汇总原 `plan.md` 草案的全部内容。
> 前置基线：Harness v1 全闭环（commit `6fbf7fd`）——聊天链路统一为 前端 → r-code-client → r-code-service 守护进程（daemon）→ Harness 插件。
> 证据基础：[大型项目上下文调研(../../../../research/large-project-context-strategies.md)、[巨仓/多人增补调研(../../../../research/team-scale-context-strategies.md)、[压缩与子代理调研(../../../../research/compaction-and-subagent-best-practices.md)（PRD 自包含，调研文档仅作证据存档）。

## 0. 术语表

| 术语 | 定义 |
| --- | --- |
| 外来指令文件 | 仓库根/子目录的 AGENTS.md、CLAUDE.md、GEMINI.md 等生态标准文件；对 R-Code **只读** |
| 自有上下文 | `.r-code/` 命名空间内的 context.md、invariants、team 工件；R-Code 的唯一写入目标 |
| managed block | context.md 中由引擎维护的段落，以 `<!-- r-code:begin:xxx -->`/`<!-- r-code:end -->` HTML 注释标记 |
| 不变量（invariants） | `.r-code/invariants/*.yml` 中的 ast-grep 规则，error/warning 级，可机械执行 |
| 冻结快照 | run 启动时对指令/记忆等易变输入的一次性读取结果，运行期间不再变化 |
| 提案制写入 | 任何对项目文件的写入必须先形成 diff 提案、经审批面板确认后由可信写边界执行 |
| 共建模式 | 显式开启后 R-Code 才维护根 AGENTS.md（出站生态共享）；默认关闭 |
| doc-sync | Plan 结算后自动比对外来/自有文档与实际代码差异、产出 patch 提案的机制 |
| R1/R2/R3 | 混合需求：daemon 压缩引擎 / 记忆注入接线 / children 执行体接线（见 FR-6/7/8） |

## 1. 背景与问题

### 1.1 业界结论（调研摘要）

1. **规则文档是标配**：全行业收敛到 AGENTS.md/CLAUDE.md 分层指令文件（6 万+ 开源项目，GitHub/OpenAI/Google/Cursor/Anthropic 原生支持）；正确形态是 map-not-manual（约 100 行目录 + 指针，OpenAI 内部结论"大而全的 AGENTS.md 必然失败"）。
2. **向量索引退潮、grep 自足有边界**：Cursor 转向本地 trigram 索引并声明不存 embeddings；Sourcegraph 企业版弃 embeddings 改 BM25；Claude Code/Codex/Gemini CLI 均为零索引 agentic search。量化边界（Sourcegraph CodeScaleBench，1,281 次 run）：40 万行以下加检索工具为负收益，40 万–200 万行区间增益最大（+0.259）。
3. **长任务可靠性公式**：外部化状态（plan/tasks/progress 文件）+ 确定性压缩优先（tool-result clearing）+ LLM 摘要兜底 + 隔离探索窗口（subagent）+ 可机械执行的验收。压缩方面六家（Claude/Codex/Gemini/pi/opencode/Letta）独立收敛到同一套结构化摘要模板与"清理→摘要→重注入"分层；宿主/会话层统一压缩有直接先例（Codex compact.rs、opencode projector、pi 会话树）。
4. **多人协作规律**：规则进 Git = 天然共享；agent 产出永远人审；路径作用域规则配 CODEOWNERS；上下文预算要进 CI；并行靠 worktree 物理隔离 + 按文件集分工。
5. **防熵最优实践**：不变量从提示词升级为可执行 AST 规则（ast-grep，Rust crate 可进程内嵌）；Memory Bank 模式（28k+ 衍生 stars）验证"项目上下文作为 Git 资产"，短板（手动更新、progress 无验证）恰是宿主权威架构可补的。

### 1.2 R-Code 现状缺口（2026-09-30 两轮代码探查）

**断线与缺口**：

| # | 缺口 | 证据 |
| --- | --- | --- |
| G1 | daemon 链路零压缩：native 插件 manifest 声明 `compaction` feature 但源码零实现，每轮全量投影 `ConversationState` 进模型请求 | `plugins/native/harness.json`；`plugins/native/src/loop_engine.rs:179` → `request_projection.rs:70` |
| G2 | `/compact` 是旧路径残留：完整实现读写桌面旧库，对 daemon 任务报 "task not found" | `src-tauri/src/commands.rs:2999`；前端命令仍注册（`slash-commands.ts:62`） |
| G3 | vendor `agent-compaction` 库（CompactionManager/auto_compact）只有测试引用 | `crates/r-code-core/tests/contract_tests.rs` |
| G4 | 记忆注入端全断：`record_injection` 零调用、`rendered_prompt` 仅测试、`FrozenChildMemorySeed` 零使用、Codex 委派 `memory_context` 恒 `None`；`memory.md` 声称的"同一快照传给主 Agent 与其子代理"不成立 | `crates/r-code-core/src/memory.rs:302`；`src-tauri/src/commands.rs:19407`、`:19589` |
| G5 | children 子代理无执行体：daemon 构造 router 未 `with_children`，`cross-harness-delegation` 生产未接线 | `crates/r-code-runtime/src/run_manager.rs:802`；`parallel.rs:1171` |
| G6 | 子 run 上下文极薄：WorkUnit 子 run 只拿 `unit.description` 字符串 | `crates/r-code-runtime/src/parallel.rs:1273` |
| G7 | 无项目指令读取引擎：system prompt 只来自内置默认（代码常量）+ 两级 TOML（全局/项目 `agent-prompts.toml`），仓库内 AGENTS.md/CLAUDE.md 不被读取 | `src-tauri/src/settings.rs:498`；`crates/r-code-runtime/src/services/run_snapshots.rs:358` |

**已扎实的地基（可依赖，勿重复建设）**：

- 写冲突三层防护：持久路径租约 `path_leases`（跨 task/session/进程重启，`crates/r-code-store/src/v1/mutations.rs:196`）+ 进程内 PathCoordinator + OS 级 workspace flock；有界并行（`DEFAULT_DISPATCH_BOUND=2`）、先落库再 spawn、读集漂移 revalidate。
- 子代理权限三态：rank 天花板（`crates/r-code-kernel/src/children.rs:161`）、`subagent:` 执行闸（`crates/r-code-gateway/src/gateway.rs:923`）、审计 `caller=subagent:<id>`、Codex 档位父级钳制。
- bash 逃逸兜底：普通 Git 审核 before/after hash 复核。
- harness 协议预留位：`CatalogSnapshot.instructions` 投影字段已定义未填充（`crates/r-code-runtime/src/services/context.rs:318`）。

### 1.3 一句话问题定义

多 agent 并行不会写坏文件（写侧安全已达标），但每个 agent 都在"裸奔上下文"：不读项目指令、不记得任何东西、长会话必炸、子代理跑不起来。本 PRD 同时补齐读侧供给（R1–R3 修复）与大项目服务能力（L1–L5 功能）。

## 2. 目标与非目标

### 2.1 五个终态

1. **读得懂**：运行时原生读取并注入外来指令文件（分层 + 子目录 JIT），冲突以 `.r-code/` 为准；
2. **建得起**：`/init` 引擎化——侦察 → 生成/增量更新 `.r-code/context.md` → 审批写入；
3. **转得动**：文档与 Plan/实施/验证闭环联动，代码迭代后说明书不烂尾；
4. **烂得慢**：不变量可执行（ast-grep）+ 结构漂移监控 + `/doctor` 体检；
5. **协得了**：团队工件经 Git 共享、多会话 worktree 隔离、agent 产出永远人审。

### 2.2 非目标（明确排除）

- 不做服务端/云代码索引（与本地优先冲突）；
- 不做向量/embedding 索引（首期；BM25/tree-sitter 之外的路线待任务级评测证据）；
- 不做无审批的自动写项目文件（一切写入提案制）；
- 不做多 agent 自由重构编排（防熵靠闸门与监控）；
- 不做组织级/MDM 策略下发（企业方向后置，首期两级：全局个人 + 仓库 Git）；
- 不做 Codex 路线的宿主统一压缩（Codex 模型流量不经宿主，见 FR-6 范围限定；`/compact` 对 Codex 任务透传其自身能力，统一方案远期另立项）；
- 不写仓库根共享文件（默认；共建模式 opt-in 例外）；
- 私有状态不进项目目录（AppData）。

## 3. 用户与使用场景

### 3.1 用户旅程

**旅程 A · 首次接入（一次性）**：用户在 50 万行项目打开 R-Code，输入 `/init` → 只读子代理数分钟侦察（构建/测试命令、目录结构、git log 活跃区、技术栈、环境标记文件；根上有外来 AGENTS.md 则一并读取去重）→ 产出 `.r-code/context.md` 草案 → 审批面板 diff 确认 → 写入。若项目已有外来 AGENTS.md：读取自动生效，`/init` 只生成补充内容到自有文件，不改外来文件。

**旅程 B · 日常开发（无感知）**：用户说"把登录改成支持 OAuth"，agent 已带项目知识开工；`/context` 可查看本次注入了哪些指令及预算占用；深入子目录时该目录的 AGENTS.md 自动 JIT 注入。

**旅程 C · 大需求（防跑偏）**：目标 + Plan 模式 → 计划审批（闸门对照不变量打标）→ 逐事项实施验证 → **结算时收件箱弹提案**："新增了 tenants/ 模块，context.md 架构地图需要加一行" → 确认即同步。

**旅程 D · 防熵体检**：`/doctor` 报告命令可运行性、路径有效性、预算、双源矛盾、不变量命中；`/doctor --architecture` 出架构健康报告。

**旅程 E · 多人并行**：队友 clone 即得 `.r-code/`（入库部分）；各开 worktree 会话并行，文件持有提示防覆盖；agent 变更全部进审核面板人审。

**旅程 F · 长会话（R1 的用户面）**：长任务跑数小时，用量达阈值（默认 80%）自动压缩；时间线出现 `ContextCompacted` 事件与"多次压缩可能降低准确性"提醒；`/compact` 可手动触发；阈值与压缩模型在设置中配置（§8）。

**旅程 G · 委派子代理（R3 的用户面）**：主 agent 并行派出只读侦察子代理（运行树可见、逐个可取消）；完成后收到摘要与可点击的文件定位；并发受配置限制（默认 6，超限排队）；R2 记忆对用户无感（`/context` 可见注入摘要）。

### 3.2 命令与入口一览

| 入口 | 类型 | 行为 |
| --- | --- | --- |
| `/init` | 引擎命令（daemon 侧流程） | 无 context.md → 生成；有 → 增量 patch；`--reset` 重新生成自有文件；`--shared` 进共建模式维护根 AGENTS.md |
| `/doctor` | 引擎命令 | 机械体检；`--ci` 机器输出（exit 非零表失败）；`--architecture` 架构健康报告（只读子代理） |
| `/context` | 查询命令 | 显示本次 run 注入的指令文件、预算占用、来源（外来/自有）、记忆注入摘要，跳过/被裁剪条目带标注 |
| `/compact` | 引擎命令 | 手动触发 R1 压缩（改接 daemon 链路，修复 G2） |
| 审批收件箱 | UI | doc-sync 提案、不变量建议、（全局）记忆候选统一审批 |
| worktree 会话 | 会话属性 | 并行会话可选绑定独立 worktree + 分支 |
| TUI | 命令对齐 | `/init`、`/doctor`、`/context`、`/compact` 与 GUI 同义 |

## 4. 功能需求

验收标准统一格式：每条 FR 附可测的 Given/When/Then 或检查清单。

### FR-1 项目指令读取引擎（L1，修 G7）

**需求**：

1. 注入层级（拼接从根到叶，越靠后优先级越高）：
   - 全局层 `~/.r-code/context.md`（个人，R-Code 自有）；
   - 仓库层·外来 `<repo-root>/AGENTS.md`，无则 CLAUDE.md（回退名单可配；`AGENTS-ui.md` 类非标准名默认关闭、仅显式配置开启——非标准名无生态互认价值；**CLAUDE.local.md 不在回退名单**——个人层职责由记忆系统承担）；
   - 仓库层·自有 `<repo-root>/.r-code/context.md`；
   - 子目录层 `<subdir>/AGENTS.md`（JIT：read_file/search 工具命中该目录后，注入该目录及祖先链上未注入的指令）。子目录层仅外来 AGENTS.md；子目录 `.r-code/` 不识别（`/doctor` 提示，monorepo 嵌套自有文件后续版本再议）。
2. **repo root 发现**：从 canonical workspace root 沿祖先链查找 `.git` 目录；若 `.git` 是文件（linked worktree）则解析 gitdir 指回主仓 common dir——worktree 场景外来指令取主仓根与 worktree 根两者、就近优先；无 `.git` 时以 workspace root 充当 repo root 并在 `/doctor` 提示；`/init` 适用同一规则。
3. **冲突裁决：`.r-code/` 为准**——外来与自有内容冲突时，注入与不变量闸门一律采用自有版本（实现：拼接顺序 + 同名条目去重取后者）；优先级仅低于会话中用户显式指令。
4. 预算与 JIT 交互：合计默认 32 KiB（可配），单文件 >4 MiB 跳过；**冻结层（全局/仓库外来/仓库自有）在运行中不被驱逐**；JIT 使用独立追加额度（默认 8 KiB，计入合计上限），超限时放弃该次 JIT 注入并在时间线留痕；启动时预算分配按"子目录 JIT > 仓库自有 > 仓库外来 > 全局"的逆序裁剪。
5. 冻结：run 启动时冻结快照（与现有 prompt 快照同构；**冻结指 canonical 注入集不变，投影层不得驱逐冻结层**）；**JIT 注入走宿主投影层**——模型请求可见、canonical transcript 不落（不污染只增前缀与重放一致性），gateway 只向投影层上报"工具命中了哪些目录"；账本 + 时间线低噪声事件审计。
6. 注入账本：复用 `memory_injections` 表结构模式，**落 daemon V1Store**（不跨进程写桌面库），记录文件路径 + 内容 hash + 字节数，审计面板可查。
7. 默认开启；首次发现外来指令文件时会话头部提示"已注入 N 条项目指令（查看）"，可一键对本项目关闭（配置存放见 §8 配置项一览）。注入层按内容 hash 对等价条目去重（兜底迁移双注入）。

**验收**：a) 有外来 AGENTS.md 的仓库，`/context` 显示注入条目、字节数与记忆注入摘要，跳过（>4 MiB）与被裁剪条目带标注；b) 同一命令在 context.md 与外来文件冲突时，模型行为遵循 context.md 版本；c) JIT 注入后时间线出现对应事件且 token 计量更新；d) 关闭开关后重开 run 不再注入；e) 32 KiB 预算裁剪顺序符合第 4 条；f) JIT 超额度时该次注入被放弃且时间线留痕（冻结层不受影响）。

### FR-2 `/init` 引擎化（L2）

**需求**：

1. 引擎化为 daemon 侧流程 `project_context.init`：侦察阶段经 R3 的只读子代理执行（工具白名单 read_file/list_files/search/glob/git_status/git_log），输出结构化侦察报告（JSON：命令探测结果、目录树、活跃区、技术栈、环境标记）；渲染阶段由宿主按模板生成草案；写入阶段提案制。**侦察资源上限**：文件数上限 10 万、目录深度 16、超时 5 分钟——超限产出部分报告并标注；空仓库产出最小骨架（仅全局层与指针段）。**基线重建**：无 AppData 快照但文件含管理块时，以当前文件为基线建立快照、走增量；clone 缺 `.r-code/` 时首次 `/init` 全新生成，`/doctor` 提示"本项目未共享上下文工件，仅外来指令生效"。
2. 模板（map-not-manual，≤100 行）：架构地图 / 常用命令（managed block，与工具链版本对齐）/ 规范与不变量（指向 invariants）/ 当前活跃区（默认不自动维护）/ 指针（CONTRIBUTING、ARCHITECTURE、模块 README、CODEOWNERS）。
3. **幂等增量**：再次 `/init` 与上次快照（AppData）对比，无变化返回"已最新"；有变化只提议 managed block 内 patch；managed block 被用户删除则整文件视为纯用户内容，改动前显式询问。
4. **外来文件只读 + 补充**：识别依据（无 r-code 管理块 + AppData 无快照）；不改写外来文件；`/doctor` 检测双源矛盾并标注"生效值为自有版"。
5. **兄弟格式迁移**：可将 CLAUDE.md / **CLAUDE.local.md** / GEMINI.md / .cursorrules / .windsurfrules 现存规则并入自有 context.md（原文件不动，diff 审批）；**迁移收尾**：accept 后建议将源文件移出回退名单（项目注入设置）；注入层内容 hash 去重兜底（FR-1.7）。
6. **共建模式**（opt-in，默认关）：**持久开关**（AppData 项目设置；`/init --shared` 首次开启、设置页可关）；开启后 managed 内容以根 AGENTS.md 为宿主（管理块 + diff 审批维护），`.r-code/context.md` 保留用户段与指向根文件的指针——避免双写漂移；doc-sync 在共建模式下提案落根 AGENTS.md；开启时不自动托管既有外来内容（迁移显式进行）。
7. **`/init --reset`**：只整体重新生成自有 context.md；外来文件永远仅在共建模式下被修改。
8. TUI 补同名命令。注意：替换前端旧 workflow 展开时同步移除旧文案（`slash-commands.ts:595` 现指导"创建或完善根目录 AGENTS.md"，与本 PRD 的外来只读方向相反，勿残留）。

**验收**：a) 新仓库 `/init` 产出 ≤100 行 context.md 且命令经沙箱 dry-run 验证；b) 连续两次 `/init` 第二次无 diff；c) 改动构建命令后 `/init` 只 patch 命令 managed block；d) 手写段在任何流程中不被触碰；e) 存在外来 AGENTS.md 时草案只含补充内容且外来文件 hash 不变；f) `--reset` 后自有 context.md 重建且外来文件 hash 不变；g) 迁移 accept 后源文件内容不再双重注入（`/context` 验证仅一条）；h) 共建模式下共享改动落根 AGENTS.md、管理块标记齐全、context.md 指针同步。

### FR-3 Plan/实施闭环联动（L3）

**需求（五个挂钩，全部宿主权威、全部提案制）**：

1. **Plan 发布闸门**：Plan 草案发布前注入架构地图与不变量；增强审核对疑似违反不变量的写操作打标（触达禁改区、超行数阈值、ast-grep error 命中）。
2. **事项完成同步检查**：`plan_item_update(completed)` 时比对涉及文件与结构敏感区——M2 为**路径级**比对（context.md 架构地图/命令段引用的路径集合），符号级（repo map）随 M3 增强；命中则生成文档同步待办挂 Plan 收尾（fail-open 留痕）。
3. **结算 doc-sync**：`ReviewReady`/verified 结算处触发后台比对，patch 提案进审批收件箱（新表 `context_proposals`，复用记忆候选的审批 UI 模式）。
4. **压缩重注入**：R1 压缩后重注入包含进行中 Plan 的目标与当前事项（详见 FR-6 L3 层）。
5. **一事项 = 一 PR（可选）**：叶子事项可绑定独立分支/PR 约定，与 worktree 会话配合。

**验收**：a) 违反不变量的 Plan 事项在增强审核中带标记；b) 三轮模拟迭代后 context.md 与实际结构一致（路径级；符号级验收随 M3）；c) 压缩后模型能继续未完成事项且不重复询问已完成事项；d) 收件箱提案均为 diff 形式且拒绝后不落盘；e) 一事项一 PR 模式开启时，叶子事项产出独立分支与可审 diff（人工验收）。

### FR-4 防熵引擎（L4）

**需求**：

1. **不变量引擎 = ast-grep 进程内嵌**：规则 `.r-code/invariants/*.yml`（进 Git 共享），YAML 含 id/language/severity/rule；`/init` 提议初始集（可选模板库：禁 import 路径、pub fn 必须有 doc、模块 LoC 上限辅助检查——ast-grep 是否胜任行数类规则待 spike 验证），用户审批维护；三个消费点共用：Plan 闸门、增强审核打标、`/doctor` 体检（`--ci` 输出机器可读 JSON；error 级命中 exit 1，warning 级不改变退出码仅入报告，可挂 CI/pre-commit）。
2. **结构漂移监控**（M3）：tree-sitter tags 符号图（aider repomap 算法：def/ref 建图 + PageRank；`tree_sitter_tags` + petgraph）按 Plan 结算点快照存 AppData；模块依赖/体积/重复度越限在 `/doctor` 报告。
3. **架构回顾**（可选、用户主动）：`/doctor --architecture` 只读子代理出报告（边界侵蚀、死代码、与声明偏差、CODEOWNERS 覆盖缺口）。本期人工验收（产出物为报告）。
4. **`/doctor` 检查项清单**（旅程 D 承诺的五类全部落定义）：① 命令可运行性——仅对 managed block 内命令做沙箱 dry-run（超时/失败仅报告，不执行有副作用命令）；② 路径有效性——context.md 架构地图/指针引用的路径存在性；③ 预算——当前注入合计是否超限及各文件占比；④ 双源矛盾——外来与自有不一致项（标注生效值为自有版）；⑤ 不变量——ast-grep 命中（error/warning 分列）；⑥ 团队工件健康——team/invariants 已开启但被 .gitignore 命中、或 clone 缺 `.r-code/` 时提示"仅外来指令生效"。

**验收**：a) error 级不变量命中时增强审核出现标记且 `--ci` exit 1（warning 不改退出码）；b) 规则文件语法错误被 `/doctor` 报出而非崩溃；c) 漂移快照可在两次结算间对比出新增跨模块依赖；d) `/doctor` 输出覆盖检查项清单①–⑥。

### FR-5 多人协作（L5）

**需求**：

1. **团队共享工件（opt-in）**：项目可开启 `.r-code/team/`（activeContext.md / progress.md），Plan 结算 doc-sync 一并提案更新；progress 中"已完成"只引用宿主 EvidenceRecord（杜绝"说完成 ≠ 真完成"）。默认关闭，开启前明确告知会进项目 Git。**工件最小骨架**：activeContext.md（当前焦点 / 进行中事项 / 近期决策）；progress.md（里程碑清单 + EvidenceRecord 引用 + 已知问题）。**并发语义**：team 工件提案基于文件 hash 基线，accept 时基线已变（如另一 worktree 会话已改）则拒绝自动应用、转"需人工合并"状态并通知（§5.5）。
2. **规则治理**：`.r-code/` 变更经 Git PR 人审（不另发明流程）；CODEOWNERS 可指定 owner；`/doctor --ci` 供 pre-commit/CI 挂钩。
3. **worktree 并行会话**：会话可选属性，绑定独立 worktree + 分支。地基已备（`task_workspace_binding.rs` 的 ManagedWorktreeBinding 拓扑验证、`git_service.rs:499` create_worktree、`feature_flags.rs` Worktree 开关默认关——缺生产创建路径）。**会话锁定需新增 bash 命令级防护**：现状 PathGuard 只绑定 bash 的 cwd（`tools_command.rs:82`），不分析命令内容，`git -C <外部路径>` 不会被拦截——需在 bash 审批/解析层新增 `git -C` 等目标路径校验（fail-closed）。基于增强审核归属数据给出"此文件正被事项 X 持有"提示（提示不阻塞）。
4. **人审底线**：agent 变更永远进审核面板；连续自迭代熔断提示——**计数语义**：同一 task 内未经用户审批/steer 的连续 agent turn 结束计一次，用户任何审批或输入重置计数；提醒为会话内横幅，不阻塞；N 可配默认 5。

**验收**：a) 双 worktree 会话并行修改不相交文件集全程无覆盖事故（租约兜底）；b) 同文件并行时后到者收到持有提示；c) team 工件的 progress 完成项可跳转到对应 EvidenceRecord；d) 熔断横幅在连续 5 次无用户交互的自动续跑后出现、用户输入后计数重置；e) worktree 会话内 bash 的 `git -C`（及等价形式）指向 worktree 外路径被拒绝（fail-closed）。

### FR-6 R1 daemon 压缩引擎（修 G1/G2/G3）

**需求（分层）**：

- **L0 源头限流**：现有工具输出截断（read 2000 行/100KB、search 100 条、bash 30k）保持；新增超限内容落盘冷存储并在结果中保留路径引用（可回读）；冷存储保留策略：单工作区配额 + TTL（默认 7 天），随"忘记项目"清理。
- **L1 确定性清理**：每次模型请求前，对"已深入历史"的工具结果做占位替换，仅保留最近 3–5 条全文；**单调性约束**（一旦清理不再恢复，保证前缀缓存稳定）；切点永不拆 tool_use/tool_result 配对；占位符保留调用摘要（命令/查询词）。
- **L2 LLM 摘要**（阈值触发，默认 80% 用量，可配 50–95%）：结构化模板（Goal/Constraints/Progress(Done/Active/Blocked)/Decisions/Next Steps/Critical Context+Relevant Files）；质量组合拳：迭代更新（PRESERVE 规则）+ 拒绝被截断的摘要 + 膨胀检查（摘要比原文还大则放弃）+ 失败不落盘仅降级为 L1 强化；压缩模型可独立配置（低档即可）；**预留输出空间**（触发阈值考虑 max output tokens）。
- **L3 重注入包**：摘要（附"不要复述、直接继续"指令）+ 最近 5 文件（单文件 5000 token 上限，超出留路径引用）+ 项目指令与记忆（磁盘重注入）+ **全部用户消息（20K 预算，新到旧）** + 进行中 Plan 目标与当前事项 + 文件操作账本（readFiles/modifiedFiles 累积）。
- **架构（宿主侧；首期范围 = native/HostProvider 路线）**：daemon 在模型请求边界（`host.model.stream` 通路）应用投影函数——完整 transcript 为 source of truth（插件状态不动），发给 provider 的是投影。**范围限定（评审修正）**：Codex 路线（`ModelRoute::HarnessManaged`）的模型流量在 Codex app-server 子进程内直连 provider、不经宿主（codex manifest 不请求 `host.model.stream`；router 对 HarnessManaged 显式置 `model_stream:false`）——Codex 任务的 `/compact` 首期透传 Codex 自身 compact 能力或明确提示不支持，宿主统一压缩覆盖 Codex 列为远期另立项。native 插件无感知但获得：(a) 统一 token 计数暴露（防插件误判窗口）、(b) 压缩前后事件（`ContextCompacted` 事件，补 TUI 数据缺口）、(c) 压缩后用户提醒（Codex 式文案："长会话与多次压缩可能降低准确性，建议适时新开会话"）。
- **token 计数策略**：以 provider 返回 usage 回填为主、近似估算（chars/4）做触发阈值，不引入大体积 tokenizer 依赖。
- **成本注记**：插件每轮全量投影 ConversationState 过 IPC 的线性增长本 PRD 接受（长期项：分段请求 / checkpoint 裁剪）。
- **cache 纪律**：投影函数只做单调裁剪与尾部追加；工具目录现状已有启动冻结 + 排序 digest（`run_snapshots.rs:318`）+ 插件循环前一次性获取（`loop_engine.rs:162`），本 FR 补运行中途复验（防 MCP 动态工具枚举漂移导致的缓存失效）。
- `/compact` 改接 daemon（修复 G2）；vendor `agent-compaction` 作为策略库复用（修 G3）。
- 摘要生成失败重试一次，仍失败仅强化 L1 并记录事件，不阻塞会话。

**验收**：a) 长会话（>窗口 80%）自动触发压缩且会话可继续、不撞 provider 上限；b) 连续请求间未压缩前缀的缓存命中不被 L1 破坏（在提供 cache 计费字段的 provider 上以 cache_read 用量对比基线，劣化 ≤10%）；c) `/compact` 对 daemon **native** 任务生效且产生 `ContextCompacted` 事件（TUI 可见；Codex 任务透传其自身能力或有明确不支持提示）；d) 压缩后模型能正确回答早前对话的关键决策（摘要保留验证：抽查 10 问 8 对）；e) 摘要服务故障时会话不中断；f) 冷存储落盘可回读、TTL 过期与配额清理生效（日志抽验，人工验收）。

### FR-7 R2 记忆注入接线（修 G4）

**需求**：

1. 桌面侧（记忆 DB 属主）在 **task 创建时**计算冻结快照一次（`load_snapshot().rendered_prompt()`），随 TaskContract 传递（frozen 字符串 + entry id 列表 + snapshot hash）；后续 attempt/run/steer/repair **继承不重算**。TUI 直发 `task.create` 时无桌面属主在场——首期 TUI 创建的任务无记忆注入（开放问题 7 跟进）。
2. daemon 将其并入 PromptSnapshot 并调用 `record_injection` 记账；`FrozenChildMemorySeed` 落地为子 run 的最小注入集。
3. Codex 委派 `memory_context` 接真值（两个生产调用点）。
4. 记忆关闭/项目 read_only/off 模式下行为：off 不注入、read_only 只注入不采集（与 `memory.md` 现有语义一致）。
5. 实现完成后修正 `memory.md`，使文档与实现一致（消除 G4 的声明落差）。

**验收**：a) 开启记忆的项目，新 run 的 PromptSnapshot 含记忆段且 `memory_injections` 有记账；b) 同一 run 内主 agent 与其子代理看到同一快照 hash；c) 关闭记忆后无注入；d) Codex 委派 prompt 含记忆段。

### FR-8 R3 子代理执行系统（修 G5/G6；构建新子系统，非简单接线）

**需求**：

1. **构建真实执行体**（现状 `ChildrenSupervisor` 是纯记账桩：spawn 只插 HashMap 记录、wait 非阻塞、complete 零调用方、无并发上限）：真实子 TaskState 创建 + 独立 PluginSession/router/transcript；完成回调回填 report；wait 改为**阻塞原语**（长超时防忙轮询，分钟级默认）；并发上限闸门（默认 6，**超限排队**、额度释放后执行——可配为拒绝）与嵌套限制（默认 1）；close 摘除回收（修 HashMap 条目泄漏）；spawn 序号改**单调计数器**（防 close 后 len 缩导致 id 复用）；解除 `RouterServiceAvailability` 的 Children* 硬编码 false（`router.rs:131`）与 run_manager 侧 debug_assert；RunManager/parallel 构造 router 加 `with_children`（`router.rs:268` 已定义、零调用方）。
2. **工具暴露路径（决策 D9）**：`children_spawn` / `children_wait` / `children_close` 作为**宿主目录工具**暴露给模型（进入 tool catalog、参与 digest 冻结与 capability 协商；目录变更需版本说明），native 插件自身无需改造。
3. **委派契约四要素**（写入 children_spawn 工具描述，Codex 范本）：objective / 输出格式 / 工具与来源指引 / 任务边界；附委派纪律（用户未要求不得 spawn、关键路径任务本地做、子任务自包含且写集不相交、委派后勿重复劳动勿反射式 wait）。
4. **子代理上下文组装**：objective + 冻结记忆 seed + git 快照 + 工具白名单；**不含主对话全量**；侦察类角色跳过记忆与指令重上下文；可选 fork 中间档（none/all/N，默认 none）。
5. **回传三件套**：1–2K token Markdown 摘要 + 结构化 JSON（Schema：SummaryOfFindings / ExplorationTrace / RelevantLocations[{FilePath, Reasoning, KeySymbols}]）+ 修改文件路径清单；回传内容视为不可信输入做注入扫描（复用现有扫描标记）。
6. **状态机与控制**：`pending_init/running/interrupted/shutdown/completed/errored`（状态转换触发条件在技术设计阶段定义，PRD 只约束终态语义）；完成后显式 close 回收并发额度（Codex 教训：不 close 会占额度）；per-child 取消沿用现有 UI。
7. WorkUnit 子 run 上下文从"仅 description"升级为同一组装规则（修 G6）。

**实施分两步**（对应 M1a/M1b）：第一步执行体最小闭环（真实子任务 + 只读侦察子代理可用 + 并发/嵌套闸门）；第二步委派契约与回传三件套打磨 + WorkUnit 上下文升级。

**验收**：a) 主 agent 可 spawn 只读子代理并收到三件套结果；b) 子代理调用白名单外工具被拒且审计带 `caller=subagent:<id>`；c) 并发第 7 个 spawn 进入排队并在额度释放后执行；d) 子代理尝试再 spawn 被拒；e) wait 不产生忙轮询（CPU/请求频率指标）；f) WorkUnit 子 run 的 prompt 含记忆 seed 与 Plan 上下文；g) 目录工具有 digest 版本记录且旧 catalog 任务不受影响。

## 5. 技术设计

### 5.1 架构总览

```
桌面进程（src-tauri）                     r-code-service daemon（r-code-runtime）
├─ 前端（React）                          ├─ RunManager / parallel（WorkUnit 调度，租约）
│   ├─ 斜杠命令 /init /doctor /context    ├─ 指令读取引擎（新，L1）──► CatalogSnapshot.instructions 投影
│   ├─ 审批收件箱（context_proposals）    ├─ 压缩引擎（新，R1）──► model.stream 边界投影 + ContextCompacted 事件
│   └─ 记忆属主（r-code.db）──冻结快照──► ├─ 记忆注入（R2）──► PromptSnapshot + record_injection
│       task 创建时随 start contract 传递  ├─ children supervisor（R3，with_children）──► 委派契约/回传三件套
└─ TUI（r-code-tui）──同义命令──►         └─ Harness 插件（native / codex）── 只消费投影，无压缩感知
                                              │
                                              ▼
                                         r-code-gateway（工具/审批/路径边界/租约）──► 工作区
```

### 5.2 挂载点清单（实施索引）

| 能力 | 挂载点 | 动作 |
| --- | --- | --- |
| 指令注入 | `crates/r-code-runtime/src/services/run_snapshots.rs`（prompt_snapshot/harness_config 冻结点）+ `services/context.rs`（填充已预留的 instructions 投影）+ `plugins/native/src/loop_engine.rs`（effective_system_prompt 拼接） | 修改 |
| JIT 注入 | 宿主投影层注入（模型请求可见、canonical transcript 不落）；gateway 工具结果后处理点只向投影层上报命中目录 | 新增 |
| 扫描/去重 | 复用 `crates/r-code-gateway/src/tools_search.rs` 的 ignore/WalkBuilder 模式 | 复用 |
| /init 流程 | daemon 新服务 `project_context` + 前端/TUI 命令接线（替换 `slash-commands.ts:595` 的纯提示词展开） | 新增 |
| doc-sync | `run_manager.rs` settle/ReviewReady 结算处 + 新表 `context_proposals` | 新增 |
| 压缩引擎 | daemon model 通路（router 的 `host.model.stream` 路由处）+ 新事件类型 + `/compact` 命令改接 | 新增 |
| 记忆交接 | 桌面 task 创建路径 → daemon start contract 扩展字段（三处同步改：`task.create` RPC 参数（`r-code-service.rs:383`）、`CreateTaskInput`、`TaskContract`（`r-code-kernel/src/task.rs:38`，加 `#[serde(default)]` 字段）；现状 contract 无记忆字段，属待建） | 修改 |
| children | `run_manager.rs:803` / `parallel.rs:1171` 构造 router 加 `with_children`（`router.rs:268` 已定义、零调用方）；`r-code-kernel/src/children.rs` 已有 complete/cancel/can_finalize，需补 close 摘除回收（条目当前永不退出 HashMap）与生产接线 | 修改 |
| 不变量 | 新 crate 依赖：`ast-grep-core`（引擎）+ `ast-grep-config`（RuleConfig/YAML/scan）+ `ast-grep-language`（编译期 feature 选语言，首期 rust/typescript/python；Rust 侧无运行时动态 grammar）；官方声明 Rust API 未稳定——锁版本 + spike 先行（M2 前完成） | 新增 |
| repo map | `tree_sitter_tags` + petgraph，AppData 快照 | 新增（M3） |

### 5.3 harness 协议扩展（评审项）

1. `instructions` 投影填充（字段已预留，仅填充逻辑）；
2. `ContextCompacted` 事件（含 before/after token 数、策略、保留条目摘要）；
3. token 计数暴露（投影后计数供插件记账，形如 `host.context.stats`）；
4. 压缩前后钩子事件（插件可订阅；首期 native 插件不消费，Codex 不受影响）；
5. children 宿主目录工具（`children_spawn/wait/close` 的工具描述、output_schema、capability 协商与 tool catalog digest 版本说明）。

### 5.4 数据与存储

```
项目内（用户资产，提案制写入，入库由用户决定；仓库内 .r-code/ 布局系本 PRD 新建——
现状产品从不主动在仓库内创建 .r-code/（settings.rs 明言 never creates .r-code files），
仅可能有用户手写 config.toml）：
.r-code/
├─ context.md            # 自有说明书（managed block + 用户段）
├─ invariants/*.yml      # ast-grep 规则
├─ team/                 # opt-in：activeContext.md / progress.md
├─ agent-prompts.toml    # 可选迁入（现状在 AppData project_knowledge_dir，见开放问题 8）
└─ skills/               # 可选迁入（现状扫描 .r-code/skills/ 仅设置页热重载，未接模型上下文）

AppData（私有，随"忘记项目"清理）：
├─ init 快照（上次 /init 的结构与命令指纹，供增量对比）
├─ repo map 漂移快照（M3）
├─ 冷存储（L0 超限工具输出，可回读；单工作区配额 + TTL 7 天）
└─ project_knowledge_dir（现有：项目级 agent-prompts.toml / skills 扫描）
```

SQLite：
├─ context_proposals（新，daemon V1Store）：doc-sync/不变量/团队工件提案（kind、diff、状态、来源结算点、幂等键）
├─ 指令注入账本（新，daemon V1Store）：复用 memory_injections 表结构模式（kind 扩展），不跨进程写桌面库
└─ 既有 plans/plan_items/effect_approvals/path_leases 等不变
```

注：context.md（daemon 指令投影管线）与项目级 agent-prompts.toml（桌面解析后并入 system prompt 冻结管线）是两条管线；层叠与未来迁移关系见开放问题 8。
```

### 5.5 提案生命周期与数据流（doc-sync、/init 写入、不变量建议共用）

1. 表 `context_proposals` 放 **daemon V1Store**（提案的生产者是 daemon 的结算/init 流程，避免跨进程写桌面库）；迁移编号顺延（`MIGRATION_036`，同文件 const SQL 追加）；字段：id、workspace_key、kind（context_md_patch / team_artifact / invariant_suggestion）、diff、来源（结算点 / init 会话）、状态（pending/accepted/rejected）、幂等键。
2. 审批：新 RPC（对齐 `plan.approve` 的幂等操作模式）由桌面收件箱调用；accept 后由 **daemon 经提案制写边界执行写盘**（复用 path_leases 防冲突）。
3. 通知通路：提案产生即发 task 事件（EventEnvelope），桌面收件箱经 task.events 订阅（与现有事件面一致）。
4. 拒绝：不落盘，保留审计记录。
5. 生命周期：pending 提案 TTL 30 天、上限 50 条，超限 oldest-first 归档；accept 时若目标文件 hash 基线已变，拒绝自动应用、转"需人工合并"状态并通知。

### 5.6 关键决策记录

| # | 决策 | 理由 | 代价 |
| --- | --- | --- | --- |
| D1 | 压缩在宿主侧（daemon）而非插件；首期仅覆盖 native/HostProvider 路线 | 宿主权威（"插件永不说已压缩"）；对 native 类 harness 一致；Codex/opencode/pi 三家先例同构 | 协议需加投影/事件；Codex 路线流量不经宿主（透传其自身能力，统一压缩远期另立项）；放弃各 harness 专有缓存优化 |
| D2 | 写入收束 `.r-code/`，外来文件只读 | 对其他工具协作者零干扰；消除双 doc-gardening 冲突；命名空间与产品既有项目级目录习惯一致（注：现存项目级 prompts 在 AppData，仓库内 `.r-code/` 布局系本 PRD 新建） | 知识孤岛（共建模式 opt-in 兜底） |
| D3 | 冲突裁决 `.r-code/` 为准 | 用户在本产品的显式配置优先于外来遗产；仅低于会话指令 | 双源漂移（`/doctor` 双源一致性检查兜底） |
| D4 | 命名保留 `/init`，体检并入 `/doctor` | 生态惯例（Claude/Copilot/OpenCode/Gemini 同名），迁移成本最低 | 无 |
| D5 | 不变量用 ast-grep 进程内嵌（`ast-grep-core` + `ast-grep-config` + `ast-grep-language`，编译期 feature 选语言） | Rust crate 原生（0.45.x，MIT，活跃）、error 级 exit 1 天然 CI 闸门、规则可单测、三消费点共用 | Rust API 未稳定需锁版本；语言为编译期 feature（新增语言要重编译）；只做语法层（无类型推断） |
| D6 | 子代理不继承主对话全量 | 四方一致的最强共识；状态靠外部化工件重建 | 委派 prompt 必须自含上下文（四要素契约） |
| D7 | 并发 6 / 嵌套 1 起步 | Anthropic 成本数据（multi-agent ≈15× token）+ Codex 默认值 | 大规模扇出受限（可配置上调） |
| D8 | 记忆经桌面属主冻结后跨进程交接 | 记忆 DB 单一属主（桌面），避免跨进程写竞争 | TaskContract 扩展字段；TUI 直建任务首期无记忆注入 |
| D9 | children 工具作为宿主目录工具暴露 | 工具目录宿主所有并冻结（digest），native 插件零改造；capability 协商已有机制 | 目录变更需版本说明；旧 catalog 任务不受影响的验证成本 |

## 6. 安全与边界

1. **私有 vs 共享**：私有状态全在 AppData；项目内写入全部收束 `.r-code/` 且提案制；外来文件默认只读（共建模式例外且 diff 审批）。
2. **读写分离**：读取引擎只读；一切写入走可信写边界 + 审核面板 + 增强 review 归属；doc-sync 永远进收件箱。
3. **冻结一致性**：指令与记忆均 run 启动冻结；worktree 会话用既有 capability 强制锁定。
4. **审计**：JIT 注入、子代理调用（`caller=subagent:<id>`）、压缩事件、提案审批全链路可查。
5. **不可信输入**：子代理回传与外来指令文件内容均视为不可信（注入扫描；指令文件不授予任何权限语义）。
6. **人审底线**：agent 产出永远人审；熔断提示防自迭代失控。
7. **平台细节（Windows 优先）**：managed block 标记匹配与内容 hash 统一按去 BOM、归一 EOL 处理；指令文件发现按平台文件系统语义做大小写不敏感匹配。

## 7. 里程碑与验收

| 里程碑 | 内容 | 验收（任务级） | 依赖 |
| --- | --- | --- | --- |
| **M1a 读侧供给修复** | FR-7（R2 记忆接线）、FR-1（L1 读取引擎 + repo root 发现 + `/context` + TUI）、FR-8 第一步（执行体最小闭环：真实子任务 + 只读侦察子代理 + 并发/嵌套闸门） | 附录 A 标准任务集（A/B 组）：注入组 token 下降 ≥10% 且完成度均分不降；记忆注入有账；只读子代理可跑并回传摘要 | 无新增外部依赖 |
| **M1b 建得起** | FR-8 第二步（委派契约/回传三件套/WorkUnit 上下文升级）、FR-2（/init 三模式 + TUI + 审批收件箱 UI，含 §5.5 提案通路） | FR-2 验收 a–e 全过；WorkUnit 子 run prompt 含记忆 seed 与 Plan 上下文 | M1a |
| **M2 转得动 + 协得了 + 压缩引擎** | FR-6（R1 压缩引擎全套 + `/compact` 改接 + 事件；范围 = native 路线；TUI `/compact`、`/doctor`）、FR-3（五挂钩）、FR-4.1（ast-grep 不变量 + 三消费点）、FR-5.1/5.2（团队工件 + CI 输出） | 长会话不撞窗口且续聊质量抽查通过；缓存命中率劣化 ≤10%；三轮模拟迭代后文档与结构一致（路径级）、不变量违规全拦截或留痕 | M1b；ast-grep 三 crate（spike 先行）；harness 协议小扩展 |
| **M3 烂得慢 + 并行** | FR-4.2/4.3（repo map 漂移 + 架构回顾）、FR-5.3/5.4（worktree 会话 + 熔断 + bash git -C 防护） | >40 万行仓库检索任务时间/成本对比；双 worktree 并行零覆盖事故；worktree 外路径写被拒 | M1b、M2；tree_sitter_tags 与 ast-grep-language 的 tree-sitter grammar 版本需统一配套（避免 ABI 不匹配与 C 代码重复编译） |

每个 FR 的细粒度验收见 §4；里程碑验收以任务级指标为准（调研结论：检索好 ≠ 任务完成好）。

## 8. 配置项一览

| 配置项 | 默认 | 范围 | 存储位置（层级） |
| --- | --- | --- | --- |
| 指令合计预算 | 32 KiB | 8–128 KiB | AppData 项目设置 |
| JIT 追加额度 | 8 KiB | 0–32 KiB | AppData 项目设置 |
| 指令回退名单 | `[CLAUDE.md]` | 任意文件名；非标准名需显式加入 | AppData 项目设置 |
| 指令注入开关 | 开 | 开/关 | AppData 项目设置 |
| 压缩阈值 | 80% | 50–95% | AppData 全局（可项目覆盖） |
| 压缩模型 | 主模型低档 | 任意已配置模型 | AppData 全局 |
| 保留最近工具结果条数 | 5 | 1–20 | AppData 全局 |
| 冷存储 TTL / 配额 | 7 天 / 50 MB | 1–90 天 | AppData 全局 |
| 子代理并发 / 嵌套 | 6 / 1 | 1–20 / 0–3 | AppData 项目设置 |
| 熔断阈值 N | 5 | 1–50 | AppData 项目设置 |
| team 工件开关 | 关 | 开/关 | AppData 项目设置 |
| 共建模式 | 关 | 开/关 | AppData 项目设置 |

## 9. 风险与缓解

| 风险 | 等级 | 缓解 |
| --- | --- | --- |
| 宿主投影破坏插件请求组装/缓存 | 高 | 单调裁剪 + 只追加不改写 + 工具列表中途不变；缓存命中率纳入 M2 验收 |
| 压缩质量损失（多次压缩累积） | 中 | Codex 式用户提醒；摘要质量组合拳；冷存储可回读；建议适时新开会话的产品引导 |
| ast-grep crate 依赖风险（维护/体积） | 中 | M2 前做 spike 验证（库用法、编译体积、规则覆盖）；失败预案：tree-sitter 手写规则或退化为 semgrep CLI 外挂 |
| daemon↔桌面跨进程协作（记忆交接、提案审批）复杂度 | 中 | 冻结快照字符串交接（无共享写）；提案走既有审批通道 |
| 双源（外来/自有）漂移 | 中 | `/doctor` 双源一致性 + 冲突裁决明确 |
| 多 agent token 成本 | 中 | 并发默认 6；委派纪律写进工具描述；熔断提示 |
| 团队工件被其他工具误改 | 低 | Git PR 人审 + CODEOWNERS；`.r-code/` 语义对外来工具无意义 |

## 10. 开放问题（实施前需定）

1. 全局层 `~/.r-code/context.md` 独立文件 vs 并入 `agent-prompts.toml` —— 倾向独立文件。
2. ~~共建模式入口~~ 已定稿（FR-2.6）：持久开关存 AppData 项目设置，`/init --shared` 首次开启；开启时不自动托管既有外来内容。
3. harness 协议扩展的具体字段（`host.context.stats` 形态、压缩事件 payload schema）—— 协议评审定。
4. ast-grep 初始规则模板库的范围 —— 倾向内置 5–10 条通用模板起步。
5. worktree 会话与 session branches/queues 的关系 —— 倾向"会话的可选属性"。
6. repo map 首发语言（Rust/TS/Python 先行，与 ast-grep-language feature 矩阵对齐）。
7. TUI 任务的记忆注入通路（daemon 代理 RPC 由桌面属主校验回填 vs 桌面常驻快照服务）—— 首期 TUI 无记忆注入。
8. 项目级 agent-prompts.toml / skills 是否迁入仓库内 `.r-code/`（迁移另列任务），以及 AppData prompts 管线与 context.md 指令管线的层叠顺序。

## 11. 附录

### 附录 A · 验收前置（标准任务集与基线定义）

1. **fixture 仓库**：A 组 = 本仓库 r-code（有 AGENTS.md 与完整指令体系）；B 组 = 无任何指令文件的中型开源仓库快照（约 5–10 万行，固定 commit）；C 组（仅 M3）= 一个 >40 万行仓库快照（固定 commit）。
2. **标准任务集**：12 条/组——4 条定位类（"找到 X 的实现并解释"）、4 条修改类（小重构 + 补测试）、4 条规划类（Plan 模式拆解特性）；每条附 0–2 评分 rubric（0 未完成 / 1 部分 / 2 完成），取均分为完成度。
3. **基线与判定**：关闭新功能跑一遍采集基线（token / 耗时 / 完成度均分），开启后重跑；M1a 判定：注入组 token 下降 ≥10% 且完成度均分不降。压缩验收在同一会话按固定脚本重放（非并行对照），cache 基线取压缩前 5 轮 cache_read 用量均值。M3 判定：B/C 组各 6 条检索任务，时间或成本下降且完成度不降。
4. 任务集与 fixture 由实施方在 M1a 开工时落盘为 `docs/support/archive/prd/project-context/acceptance/`（脚本 + 清单），作为独立交付物维护。

- **调研证据**：三份调研文档（见文件头链接）；关键数字：AGENTS.md 6 万+ 项目采用；CodeScaleBench 1,281 run（40 万行阈值）；摘要模板六家趋同；Anthropic multi-agent 15× token；Memory Bank 衍生 28k+ stars；ast-grep 16k★。
- **本文取代**：`docs/support/archive/prd/project-context/plan.md`（v2 草案，已删除；其全部内容并入本文）。
