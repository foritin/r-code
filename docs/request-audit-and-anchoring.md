# 请求构成审计与首轮锚定实验（落地方案）

> **状态更新（Phase 0）**：本文描述的首轮目录锚定实验（含「规划门 plan_gate /
> plan_complete / plan_ready」扩展）已随 DeepSeek 复杂任务 Plan 建议与 Plan-only
> 双轨（`docs/plan-mode-dual-track-gate.md`）整体下线：客户设置移除
> `first_round_*` 档位（legacy 输入只返回诊断警告），worker 不再收窄 Main 模式
> 目录，`plan_ready` 工具与门铃注入已删除。请求信封审计（`diagnostics.request_audit`）
> 继续有效。本文保留为实验记录与请求审计协议的权威。


> 操作手册。本文承接 [harness-migration.md](./harness-migration.md) 阶段 1.3 的
> 「request/header 快照 + 重建自检」：合同半与运行时半已在测试接线中完成，
> 本文补齐**宿主接线**（阶段 A），并在其上叠加**目录构成审计**（阶段 B）与
> **首轮锚定实验**（阶段 C，opt-in、默认关闭）。
>
> 背景来自社区项目
> [`xiaobright/dsh-anchored-standard`](https://github.com/xiaobright/dsh-anchored-standard)
> 的实测结论（DeepSeek V4 Pro，Project2 评测）：
>
> 1. **首轮工具 schema 的身份是轨迹锚定的决定变量**——adapter 默认 maxTokens
>    （256000）下，Minimal schema 5/5 锚定目标风格，standard 系 schema 11/11
>    落入另一种风格；输出封顶不是必要条件；
> 2. **首轮自动注入的上下文参与锚定**——该 preset 在首轮同时剥离工作区摘要
>    与技能目录注入，第二轮起恢复；
> 3. **阶段转换必须从持久事件推导并带逃生舱**——晋升信号默认
>    `promoteOn: either`（首次 `tool/call` 或首次 `assistant/message`，先到者
>    为准），避免纯文字首答把会话困死在受限目录。
>
> 该结论是模型特定的（不承诺跨模型、跨任务增益）。因此本方案的核心姿态是：
> **先把「模型每轮实际看到了什么」变成可审计的事实，再做可 A/B 的实验，
> 最后按模型分别决策**——而不是把锚定当作普适规律直接产品化。
>
> 文中行号以 2026-08-18（dev 分支，含未提交改动）调研为准，执行时先重新定位。
> 执行节奏见下方「执行契约」——**一次执行连续完成阶段 A 与阶段 C（全部可实施
> 内容），全程不停顿，做完统一汇报**。

## 总览

```
阶段 A  审计基线（无行为变化，~2-3 天）        ← 实施阶段①（本次连续执行）
        A1 合同层 RequestHeader 增量字段（子模块 bump）
        A2 运行时填充新字段
        A3 宿主接线：旁路审计 journal（sidecar）
        A4 观测面：计数暴露 + jq 验证清单
阶段 B  目录变更核算（零产品代码，离线配方 + 实验协议）  ← 非实施：配方供人使用
阶段 C  首轮锚定实验（opt-in，默认关闭，~3-4 天）      ← 实施阶段②（A 完成后
                                                        连续实施，不中途停顿）
阶段 D  证据评估与阶段门审计（决策门）                  ← 非实施：依赖实验证据
```

与 harness-migration 的关系：A1 沿用其 §0.3 的子模块 bump 标准流程；
A3 是其 §1.3 留下的「接线决定权留给宿主组合根」的正式行使；C 阶段的新代码
全部落在现有接缝上（config → policy → SessionState → run_loop），不引入
新的层间依赖，阶段 2/3 的 harness 拆分不受影响（run_loop 内的过滤点与
晋升判定属运行时逻辑，随 loop_core 一起迁移即可）。

---

## 执行契约（本文件的用法，执行代理必读）

本节把「实施这个文档」变成一个无需中途询问的一次性任务。执行代理把本节
当作硬指令：与后文任何表述冲突时，以本节为准。

### 1. 执行范围：前置清理 → 阶段 A → 阶段 C，连续完成，最后统一汇报

- **第 0 步（前置，必须最先做）**：把当前 `dev` 上**全部**未提交变更提交
  并推送（见 P0），得到干净的 dev 起点；
- **然后从 dev 切出功能分支**，**连续**完成阶段 A（A1→A4）与阶段 C
  （C1→C4）的全部实施内容，中途**不**为汇报、确认或阶段切换而停顿；
- **阶段 B 不是实施对象**：它的 jq 配方与实验协议是给人用的文档产物，
  禁止把它们写成任何产品代码、脚本或命令；
- **阶段 D 不是实施对象**：证据表与决策门依赖尚不存在的实验数据，
  禁止「预填」任何行；B2 实验协议已随本文定稿，执行 C 时直接遵循，
  无需外部确认；
- 全部实施内容（A+C）完成并通过完成定义后**停止在功能分支上**，输出
  完成报告（格式见下）。**不把功能分支合回 dev**，不创建 PR，不做任何
  「顺手改进」。

### 2. 预授权动作（已获用户授权，无需再问）

| # | 动作 | 具体内容 |
| --- | --- | --- |
| P0 | dev 全量提交推送（前置，仅此一次） | 在 `dev` 上分两个提交收编当前**全部**未提交变更并 `git push origin dev`：①`docs(plan): 请求构成审计与首轮锚定落地方案`（本文档自身，当前为未跟踪新文件）；②其余变更（前端 svg、agent_loop/llm_runtime 遗留修改等）按变更主题归纳成 feat/fix 前缀提交（参考近期提交风格）。两条都推送后 dev 必须干净 |
| P1 | 子模块提交并推送（可发生两次） | 在 `vendor/agent-contracts`（远端 `git@github.com:foritin/agent-contracts.git`，分支 `main`）内 commit（第一次：A1 的 RequestHeader 字段；第二次：C1 的 orchestration 枚举）并各自 `git push origin main` |
| P2 | 父仓分支 | P0 完成后从**干净的** `dev` 创建 `feat/request-audit-anchoring`，全部实施工作在此分支 |
| P3 | 父仓提交 | 按提交序列逐个 commit（分支起点已干净，常规 stage 即可） |
| P4 | 父仓推送 | `git push -u origin feat/request-audit-anchoring` |
| P5 | 构建与测试 | 运行本文规定的**阶段性功能测试**命令集（见完成定义第 2 条，不做全量 workspace 测试） |

**明确不做**（即使看起来是流程的自然延伸）：不把功能分支合回或推送到
`dev`（P0 的 dev 推送是唯一例外，且发生在切分支之前）；不创建 PR（留给
用户）；不改前端（`src-tauri/frontend` 零改动，P0 例外同上）；不安装新
依赖。

### 3. 子模块 push 失败的降级路径（预先决定，届时不再询问）

`git push origin main` 因网络/权限失败时：保留子模块本地 commit，**照常**
在父仓 bump gitlink 并继续后续步骤；在完成报告的「遗留与偏差」中置顶标注：
「子模块 commit <hash> 未 push，CI submodule-pin 会红，补 push 后自愈」。
不静默回滚，不重试超过 2 次。

### 4. 无确认点决策表（全部预先裁定）

| 决策点 | 裁定 |
| --- | --- |
| 分支名 | `feat/request-audit-anchoring`（P0 后从干净 dev 创建） |
| 提交信息 | 遵循 CONTRIBUTING 前缀（feat/chore/docs）；子模块 bump 的 commit body 必须写明上游仓库、目标提交、兼容性验证（CONTRIBUTING「分支与 Pull Request」第 6 条）；P0 的 dev 提交按变更主题归纳，参考近期提交风格 |
| 既有未提交改动 | 由 P0 一次性收编（全量提交到 dev），切分支后工作区即干净，无需绕行 |
| CHANGELOG | 在 `CHANGELOG.md` 的 `[Unreleased] → Added` 追加两条：①请求信封审计（新配置 `diagnostics.request_audit`，默认关闭；旁路 journal + 每轮 RequestHeader 快照 + 重建自检计数命令）；②首轮工具目录锚定实验（新配置 `orchestration.first_round_catalog` / `orchestration.first_round_promote_on`，默认 `full`/`either` 即现状行为） |
| docs/architecture.md | 按 CONTRIBUTING「文档与用户可见变化」，在数据流/存储小节补 3-5 句：旁路审计文件的位置（`sessions/request-audit/{storage_id}.jsonl`）、单写方、默认关闭；在编排/任务模式小节补一句首轮目录锚定为 opt-in 实验 |
| 配置默认值 | `diagnostics.request_audit = false`；`first_round_catalog = "full"`；`first_round_promote_on = "either"`——任何情况下不改默认 |
| A3.2 签名改法 | `ensure_real_runtime` 增 `sessions_dir: &Path` 参（9 处调用点见 A3.2），不用其他方案 |
| 测试命令集 | 见下方「完成定义」第 2 条（阶段性功能测试），缺一不可 |
| 中途发现设计不可行 | 停在该步，写明卡点与两个候选方案后**终止执行并汇报**——不自行改设计。这是唯一合法的中途停止 |

### 5. 提交序列（每个 commit 时点受影响 crate 必须可编译、其测试绿）

```
 0a. [dev]    docs(plan): 请求构成审计与首轮锚定落地方案
              （本文档自身，当前为未跟踪新文件）
 0b. [dev]    feat(...): 其余全部未提交变更的主题归纳提交
              （0a + 0b 都 push origin dev；完成后从干净 dev 切
              feat/request-audit-anchoring）

—— 阶段 A ——————————————————————————————————————————————
 1. [子模块] feat(contract): request_header 增加 tool_names/hosted_tool_names/max_tokens
    （含 session.rs 往返 + 缺省双测试）→ push origin main
 2. [父仓]   chore(vendor): bump agent-contracts 至 <目标提交>
    （body 注明上游仓库/目标提交/兼容性验证）
 3. [父仓]   feat(worker): RequestHeader 填充目录与输出预算字段      ← A2
 4. [父仓]   feat(worker): journal 目标 id 的会话级映射               ← A3.1
 5. [父仓]   feat(host): 请求审计旁路 journal 接线                    ← A3.2 + A3.3
 6. [父仓]   feat(host): request_audit_counters 观测命令              ← A4

—— 阶段 C（紧接 A，不切换分支、不停顿）——————————————————
 7. [子模块] feat(config): orchestration 增加 first_round_catalog/first_round_promote_on
    （含枚举 serde 缺省测试）→ push origin main
 8. [父仓]   chore(vendor): bump agent-contracts 至 <第二次目标提交>
 9. [父仓]   feat(worker): 首轮目录锚定——策略镜像、粘性标志、目录过滤与晋升
    （含 C4 单测）                                                   ← C2 运行时侧
10. [父仓]   feat(host): 锚定配置透传与 runtime 重建指纹纳入          ← C2 宿主侧
11. [父仓]   docs: CHANGELOG 与 architecture.md 同步                 ← 收尾
12. [父仓]   push -u origin feat/request-audit-anchoring，停留在此分支
```

顺序约束：3 依赖 2；5 依赖 4；6 依赖 5；9 依赖 8；10 依赖 9。阶段 A 冒烟
（完成定义第 3 条）在 commit 6 之后、commit 7 之前做一次；阶段 C 冒烟
（第 4 条）在 commit 10 之后做。

### 6. 完成定义（全部满足才算完成）与汇报格式

完成判据：

1. 提交序列 0a-11 全部落地（或按 §3 降级并标注），当前停留在
   `feat/request-audit-anchoring` 分支且已推送（P4）；
2. 阶段性功能测试（**不做全量 workspace 测试**），在对应 commit 之后立即跑：
   - A1 后：`cargo test -p agent-contract`；
   - A2/A3.1 后：`cargo test -p r-code-agent-worker`；
   - A3.2/A4 后：`cargo test -p r-code-host`（含既有命令与接线回归）；
   - C1 后：`cargo test -p agent-config`；
   - C2 后：`cargo test -p r-code-agent-worker && cargo test -p r-code-host`；
   - 每次父仓提交前：`cargo fmt --all -- --check`；clippy 只对触碰的
     crate 跑（`cargo clippy -p r-code-agent-worker -p r-code-host -- -D warnings`）；
3. 阶段 A 冒烟（commit 6 后）：配置 `diagnostics.request_audit = true`，
   新建任务发一条消息，确认 `sessions/request-audit/{storage_id}.jsonl`
   出现、canonical 文件与开关关闭时逐行一致、`jq` 能抽出
   `reason=="initial"` 的 `request_header`；关闭开关后新会话不再产生审计
   文件；
4. 阶段 C 冒烟（commit 10 后）：开启审计 + `first_round_catalog =
   "readonly"`，跑一个短任务，用 B1 配方 1/5 核对首轮 `tool_names` 恰为
   五件套、`distinct tools_sha256 == 2`；改回 `full` 后新会话行为与现状
   一致（目录时间线恒定）；
5. 汇报后停止（停留在功能分支，不合回 dev）。

汇报格式（最后一条消息，供用户一分钟读完）：

```
## 请求审计与首轮锚定 实施完成报告
- 分支状态：dev 前置提交 <hash>；feat/request-audit-anchoring @ <hash>（已推送）
- 提交清单：<hash> <message>（子模块/父仓分组，按提交序列编号）
- 测试结果：各阶段性测试结论 + 失败详情（如有）
- 冒烟结果：A 段（审计文件样例、canonical 零变化、计数命令返回值）；
  C 段（首轮目录清单、distinct 目录数、full 回归一致）
- 偏差与遗留：设计偏离（应为零）、降级路径触发情况、建议的下一步
  （合回 dev / 跑 B2 实验 / 其他）
```

---

## 0. 硬红线（先评估的结论：以下事项一律不做）

本方案对现有架构的全部改动都约束在这几条红线内。任何 PR 触碰红线即回退：

1. **canonical JSONL（`{storage_id}.jsonl`）保持宿主单写方。**
   宿主（src-tauri）目前是它的唯一写方（AgentEvent → SessionEvent 映射 +
   run 收尾 HistorySnapshot，`llm_runtime.rs:1763-1770` 注释已声明该前提），
   且有 14 处 `{storage_id}.jsonl` 读取路径（harness-migration §0.1）。
   运行时侧新增的全部落盘走**旁路审计文件**（见 §1 选型记录），
   canonical 文件的读者、fork 与恢复路径零改动。
2. **工具目录过滤只是「呈现」，不是安全边界。** 模型可见目录的裁剪
   （阶段 C）不改变任何执行判定：`SessionToolHost::tool_allowed` /
   `scoped_input` / gateway 审批链原样生效。模型即使在受限轮次调用了
   目录外的工具（例如受历史提示诱导），执行侧仍按既有 policy 处理——
   与 dsh「工具执行即使失败，晋升照常」同一语义。
3. **不动 P0-A 前缀缓存设计。** system 仍 run 内冻结、动态内容仍走尾部
   user 消息、tools 仍按名排序（P1-C）。锚定实验引入的目录变化是
   **每会话至多一次的显式预算内变化**，由 P2-H 照常归因记录，不新增
   隐式变化源。
4. **合同层（vendor/agent-contracts）只做纯增量。** 新字段一律
   `#[serde(default)]`，旧 JSONL 行反序列化不报错、新读取器读旧行得默认值；
   `agent-store` 对 `RequestHeader` 的 no-op 语义（`session_store.rs` load
   投影跳过）不变。
5. **一切默认值 = 现状。** 审计开关默认 off，锚定目录默认 `full`
   （即不过滤）。未改配置的用户升级后行为字节不变。
6. **不做的事**（评估后明确排除）：不引入 bootstrap 轮 maxTokens 封顶
   （dsh issue #11 已证明 schema 身份在默认 maxTokens 下即可锚定，封顶是
   对 standard 系 schema 的 opt-in 补救）；不自动注入 AGENTS.md/CLAUDE.md
   或技能目录（r-code 现状本就不注入，技能按需 `load_skill`，比 dsh 的
   Standard 基线更干净）；不改尾部注入架构；不碰 Codex 引擎链路。

---

## 1. 现状盘点与选型记录

### 1.1 相关事实（2026-08-18 调研）

| # | 事实 | 位置 |
| --- | --- | --- |
| F1 | `RequestHeader` 只存三段 SHA-256 + reason + excluded_tails，load 时 no-op | `vendor/agent-contracts/crates/agent-contract/src/session.rs:78-93`、`agent-store/src/session_store.rs` load |
| F2 | 运行时侧 journal 已实现：每轮派发前 append + 重建自检（log-only），`with_request_journal(store)` 是 opt-in 接线，**宿主尚未调用** | `crates/r-code-agent-worker/src/llm_runtime.rs:3996-4069`、`:1771` |
| F3 | journal 调用全部以 `ctx.session_id`（runtime 内部 UUID）为键；宿主 canonical 文件名是 `branch.storage_id`，两者**不同 id**（`ensure_runtime_session` 建立映射） | `llm_runtime.rs:2064-2097,3610-4569 各 journal 调用`、`src-tauri/src/commands.rs:5687-5737` |
| F4 | 宿主是 canonical JSONL 唯一写方：Meta/assistant/ToolCall/ToolResult/goal/HistorySnapshot/ModelProjection/user 全部由宿主事件流落盘 | `src-tauri/src/commands.rs:4945,5342,5371,5444,5541,6302,7088,7094` |
| F5 | `SessionStore` 追加锁按**进程内文件路径**归一（全局 Weak 注册表），多个 Store 实例指向同一目录不会交错写 | `vendor/agent-contracts/crates/agent-store/src/session_store.rs:37-47` |
| F6 | 主 run 每轮目录装配单点：`summary_only` → 空表，否则 `client_tools_for_hosted_tools(tool_host.tool_specs(), …)`；子代理走独立装配点 | `llm_runtime.rs:3696-3700`（主）、`:752-760`（子代理） |
| F7 | 会话级粘性标志的现成模式：`SessionState.delegation_disabled: Arc<AtomicBool>`，run_loop 与 SessionToolHost 共享同一 Arc | `llm_runtime.rs:1657-1696,2000,3680-3692` |
| F8 | runtime 重建指纹包含 orchestration 各字段——新增目录旋钮必须入指纹，否则改配置不生效 | `src-tauri/src/commands.rs:4757-4771` |
| F9 | P2-H 每轮捕获前缀形状并记录缓存变化归因日志 | `llm_runtime.rs:3985-3994`、`cache_shape.rs` |
| F10 | 全局配置 schema 在 `agent_config::Config`，orchestration 段为 `OrchestrationConfig`；runtime 侧镜像为 `OrchestrationPolicy`（`llm_runtime.rs:518`），由 `ensure_real_runtime` 组装 | `vendor/agent-contracts/crates/agent-config/src/lib.rs:14,266`、`src-tauri/src/commands.rs:4644,4740-4813` |
| F11 | 每轮 outcome 的追加消息（assistant Text/ToolUse + 工具结果）在 run_loop 内可见——晋升信号的判定点 | `llm_runtime.rs:4092-4107` |

### 1.2 选型记录：为什么是旁路审计文件（sidecar）

**问题**：F3 指出 runtime journal 用 `session_id` 作文件名；直接把宿主的
`SessionStore`（base_dir = sessions/）接进去，RequestHeader 会落到
`{runtime_session_id}.jsonl`——宿主永不读取的孤儿文件，且 runtime 重建
（provider 配置变更触发 `bridge.sessions.clear()`）后 id 更换、孤儿累积。

**候选与裁决**：

| 方案 | 裁决 | 理由 |
| --- | --- | --- |
| A. journal 直接写 canonical `{storage_id}.jsonl` | **否决** | 违反红线 1：runtime 的 goal 消息/appended 消息与宿主写方（F4）双写同一内容；且派发时 load 自检与宿主事件流异步落盘存在竞态，会产生**系统性**误报，污染 soak 观察信号 |
| B. RequestHeader 写 canonical、自检改到 run 结束后离线做 | 否决 | 双写问题消失但竞态仍在（宿主 round N-1 事件未 flush 时 RH_N 已插入文件，轮次边界交错）；离线校验器复杂度高、收益不明确 |
| C. **旁路审计文件**：`sessions/request-audit/{storage_id}.jsonl`，runtime 唯一写方 | **采纳** | 单写方无竞态（runtime 在派发前同步 append，self-check 无 race）；canonical 文件与其 14 处读者零改动；子目录隔离避免被会话枚举/glob 误读；F5 保证与宿主 Store 实例并存安全；与现有测试接线的写方模型完全一致（只是把 id 从随机 UUID 换成 storage_id） |
| D. 宿主提供 sink 通道（channel 回调），runtime 不持 Store | 否决 | API 面更大，且现有 `with_request_journal(SessionStore)` 与三处测试接线全要改；收益仅是「少一个文件」，不值 |

**代价（明示）**：sidecar 重复一份消息内容（约等于 canonical 体积，即被
审计会话 JSONL 总量 ×2）。因此审计开关默认 off，只在 soak 与实验期间按需
开启。每轮 self-check 的 `journal.load()` 全量重读文件是 O(轮数²) IO，
长会话下为 MB 级，可接受；未来若成为热点，沿 `llm_runtime.rs:3256-3257`
注释预留的「store 原始事件增量读取 API」优化，不在本方案范围。

### 1.3 侵入度评级

| 改动 | 触及层 | 侵入度 | 兼容性处理 |
| --- | --- | --- | --- |
| A1 RequestHeader 增字段 | agent-contracts 子模块 | 低（纯增量） | `#[serde(default)]`；旧行读回得默认值 |
| A2 填充字段 | r-code-agent-worker | 低（一处构造点） | 无 |
| A3 宿主接线 | src-tauri + worker（id 映射） | 中（~10 个 journal 调用点机械替换 + 1 个新 bridge 方法） | 开关默认 off，未开启时零路径执行 |
| A4 计数命令 | src-tauri | 低 | 无 |
| C 锚定 | agent-config + worker | 中（两个枚举 + 一个粘性标志 + 两处 run_loop 逻辑） | 默认 `full`，现有全部测试不动 |

---

## 阶段 A：审计基线（无行为变化；连续实施的第一段，完成后紧接阶段 C）

### A1 合同层：`RequestHeader` 增量字段

```text
仓库：vendor/agent-contracts
文件：crates/agent-contract/src/session.rs:78-93（RequestHeader 变体）
```

dsh 的「验证加载」清单要求前两项是决定锚定的变量：首请求 maxTokens 值与
工具 schema 来源。现在只有哈希无法直接审计这两项（哈希只能答「变没变」，
不能答「是什么」）。增量三个字段：

```rust
RequestHeader {
    system_sha256: String,
    tools_sha256: String,
    messages_sha256: String,
    reason: String,
    #[serde(default)]
    excluded_tails: Vec<String>,
    /// 新增：本轮 tools 数组的名字清单（按派发顺序，含 hosted 工具别名
    /// 后的名字）。与 tools_sha256 互补：哈希负责字节级身份判等，
    /// 名字清单负责 jq 级人可读审计。体积 ~25 名 × ~30B ≈ 1KB/轮。
    #[serde(default)]
    tool_names: Vec<String>,
    /// 新增：本轮 hosted 工具名（summary_only 轮为空）。
    #[serde(default)]
    hosted_tool_names: Vec<String>,
    /// 新增：本轮实际派发的 max_tokens（钳制后）。0 表示旧版本写入的行。
    /// dsh issue #11 的教训：adapterDefaults 可能静默覆盖配置封顶，
    /// 该字段让「模型看到的输出预算」直接可审计。
    #[serde(default)]
    max_tokens: u32,
},
```

配套修改：

1. 同文件测试 `request_header_roundtrips_with_snake_case_tag`（`:243-286`）
   扩展：新字段往返 + 旧行（无新字段）反序列化得默认值，模仿现有
   `excluded_tails` 缺省断言；
2. `agent-store` 无需改动（load 对 RequestHeader 整体 no-op）；
3. 按 harness-migration §0.3 流程：子模块 commit → push → 父仓 bump gitlink
   （CI submodule-pin job 会核对）。

**验收**：`cargo test -p agent-contract` 绿；父仓
`cargo test -p r-code-core --test contract_tests` 绿；手工构造一行旧格式
`request_header` JSON 能被反序列化且 `tool_names.is_empty()`。

**回滚**：git revert 子模块 commit + 回退父仓 gitlink；新字段从未被旧
读取器消费，回滚无残留。

### A2 运行时：填充新字段

```text
文件：crates/r-code-agent-worker/src/llm_runtime.rs:4015-4021（RequestHeader 构造点）
```

```rust
let header = SessionEvent::RequestHeader {
    system_sha256: envelope.system_sha256.clone(),
    tools_sha256: envelope.tools_sha256.clone(),
    messages_sha256: envelope.messages_sha256.clone(),
    reason: reason.to_string(),
    excluded_tails: tail_labels.iter().map(|label| label.to_string()).collect(),
    tool_names: tools.iter().map(|tool| tool.name.clone()).collect(),
    hosted_tool_names: if summary_only {
        Vec::new()
    } else {
        active_hosted_tools.iter().map(|tool| tool.name.clone()).collect()
    },
    max_tokens: request.max_tokens,
};
```

注意三点：`tools` 此处已是 `client_tools_for_hosted_tools` 处理后的最终派发
目录（含 `search → search_files` 别名，审计记录的是模型实际看到的名字）；
`request` 在该构造点之后才被 move 进迭代调用（`:4078-4089`），可直接取
`request.max_tokens`（`clamp_request_max_tokens` 已在 `:3958` 完成）；`HostedToolSpec`
是变体枚举（`WebSearch`/`WebFetch`，`agent-contract/src/provider.rs:93`），名字
用现有判定器映射（`is_web_search()` → `"web_search"`、`is_web_fetch()` →
`"web_fetch"`），在 worker 内写一个小 helper 即可，不动合同层。

**验收**：`cargo test -p r-code-agent-worker`（既有三处 journal 测试
`llm_runtime_tests.rs:8900/8966/9022` 附近）扩展断言新字段非空且首轮
`reason == "initial"`；`cargo test -p r-code-agent-worker -p r-code-host` 绿
（阶段性口径，见执行契约完成定义）。

### A3 宿主接线：旁路审计 journal

分三小步，每步独立 commit。

#### A3.1 运行时：journal 目标 id 的会话级映射

**问题**：F3 的 id 分裂。改法完全镜像 `set_next_memory_context` 模式：

1. `SessionState`（`llm_runtime.rs:1657`）增字段：

   ```rust
   /// A3：本会话 journal 落盘使用的目标 id（宿主传入 branch.storage_id）。
   /// None 时回退 ctx.session_id（保持既有测试接线行为不变）。
   request_journal_id: Option<String>,
   ```

2. `LlmAgentRuntime` 增方法（完全镜像 `set_next_memory_context` 的 trait
   方法形态：`async fn (&mut self)`，见 `llm_runtime.rs:2303-2307`）：

   ```rust
   /// A3：声明本会话的审计 journal 目标 id。必须在 start_run 前调用。
   async fn set_request_journal_target(
       &mut self,
       session_id: &str,
       journal_id: String,
   ) -> Result<(), ProductError> {
       // sessions.lock() → get_mut(session_id) → session.request_journal_id = Some(journal_id)
   }
   ```

3. `RunLoopCtx` 增字段 `request_journal_id: Option<String>`，在 start_run
   的 spawn 构造点（`:2099-2126`）从 SessionState 读出传入；
4. 提供唯一取值helper并替换全部 journal 调用点的 id 实参（机械替换，
   共 11 处：start_run 引导块 `:2064-2097` 一次 + run_loop 内
   `:3610, :3652, :3784, :3850, :3998, :4115, :4257, :4357, :4440, :4569`）：

   ```rust
   fn journal_key(ctx: &RunLoopCtx) -> &str {
       ctx.request_journal_id.as_deref().unwrap_or(&ctx.session_id)
   }
   ```

   start_run 引导块在 spawn 之前执行，直接从 sessions map 读映射即可。

**验收**：`cargo test -p r-code-agent-worker` 全绿（未设置映射时行为与
现状逐字节一致——`unwrap_or(&ctx.session_id)` 保证）；新增单测：设映射后
journal 事件落在 `{journal_id}.jsonl` 而非 `{session_id}.jsonl`。

#### A3.2 宿主：组装根接线

```text
文件：src-tauri/src/commands.rs
落点：ensure_real_runtime（:4644）与 ensure_runtime_session（:5687）
```

1. `agent_config::Config`（子模块，同 A1 批次 bump）增顶层段：

   ```rust
   /// 诊断开关：会话请求信封审计（旁路 journal + 重建自检）。
   /// 默认关闭。开启后每轮派发前向
   /// sessions/request-audit/{storage_id}.jsonl 追加 RequestHeader。
   #[serde(default)]
   pub diagnostics: DiagnosticsConfig,

   #[derive(Debug, Clone, Serialize, Deserialize)]
   pub struct DiagnosticsConfig {
       #[serde(default)]
       pub request_audit: bool,
   }
   ```

2. `ensure_real_runtime`（`:4644`）签名增参 `sessions_dir: &Path`，9 处
   调用点机械补参 `&state.sessions_dir`（个别调用点若持的是解构变量，从其
   来源 `CommandState` 补取）：
   `commands.rs:2050, 6581, 7406, 19141, 19844, 27033, 27114, 30179, 30477`。
   构建链（`:4801-4813`）条件追加：

   ```rust
   let runtime = r_code_agent_worker::LlmAgentRuntime::new(/* … */)
       // …既有 builder…
       ;
   let runtime = if config.diagnostics.request_audit {
       let audit_dir = sessions_dir.join("request-audit");
       std::fs::create_dir_all(&audit_dir).map_err(err_str)?;
       runtime.with_request_journal(agent_store::SessionStore::new(audit_dir))
   } else {
       runtime
   };
   ```

   （若 builder 链式风格更顺，可用 `Option` 展开写入；要点只有两个：
   **子目录隔离** + **开关关闭时不构造 Store**。）

3. `ensure_runtime_session` 在 `create_session` 成功后（`:5703-5716` 与
   `replace_context` 之间）声明映射：

   ```rust
   bridge
       .kind
       .set_request_journal_target(&session.meta.id, branch.storage_id.clone())
       .await
       .map_err(err_str)?;
   ```

   `AgentRuntimeKind` 枚举加分发臂（Real 转发 / Mock no-op），仿
   `set_next_memory_context`（`:4462-4484` 区域）。journal 未接线时该映射
   惰性无害，因此**无条件调用**，不再读一次配置。

**验收**（对应 dsh「验证加载」清单的本地版）：

1. 开启 `diagnostics.request_audit`，新建任务发一条消息，确认出现
   `sessions/request-audit/{storage_id}.jsonl`（不是
   `{runtime_session_id}.jsonl`），且 canonical `{storage_id}.jsonl` 行数与
   关闭开关时一致（diff 两份日志确认 canonical 零变化）；
2. `jq -c 'select(.request_header)' sessions/request-audit/<id>.jsonl` 能抽出
   事件，首行 `reason=="initial"`，`tool_names` 与该任务模式的预期目录一致；
3. 重启应用、同任务续发消息：审计文件继续追加（映射随
   `ensure_runtime_session` 重建）；runtime 因 provider 配置变更重建后同样
   追加到同一文件（storage_id 不变）；
4. 自检计数经 `request_self_check_counters()` 读取，正常会话
   mismatches 恒为 0；
5. 关闭开关：`request-audit/` 不再出现新写入，其余行为与现状一致。

**回滚**：配置翻回 false 即产品级回滚；代码级 revert A3.2（宿主侧）即可
断开接线，A3.1 的映射代码在未接线时是死路径。

#### A3.3 边界行为（实现时明确处理）

- **会话中途开启开关**：sidecar 从空文件起步，但 canonical 历史里的旧轮次
  不在 sidecar 中 → 首轮 self-check 报一次消息数不一致（log-only）。
  处理：可接受（计数器可见）；文档化「审计开关建议在会话开始前开启」。
  不做增量回填。
- **写失败**：沿用既有降级——append/load 失败仅 `tracing::warn`，绝不阻断
  run（`:4028-4032, :4062-4066` 现状保持）。
- **分支 fork**：fork 产生新 storage_id → 新 sidecar，互不污染；旧分支的
  sidecar 留档。
- **清理**：sidecar 不参与宿主既有会话清理/导出逻辑；后续如需随会话删除，
  在宿主删除 canonical 的同一处补删 `request-audit/{storage_id}.jsonl`
  （本阶段不做，登记为后续项）。

### A4 观测面：计数暴露

```text
文件：src-tauri/src/tauri_commands.rs（命令注册）+ commands.rs（实现）
```

新增只读命令 `request_audit_counters`：从当前 bridge 的 Real runtime 读
`(headers_appended, mismatches)`（`llm_runtime.rs:1780` 已有访问器，补
`AgentRuntimeKind` 分发臂即可）；命令在 `src-tauri/src/main.rs:625` 的
`invoke_handler` 列表注册，返回 `Option<(usize, usize)>`（Real 不在场时
`None`）。不进设置 UI——soak 期间用 devtools/日志消费即可，UI 化另议。

**验收**：开启审计跑一会话后命令返回 `(N, 0)`；`mismatches` 非零时日志中
能对到具体 mismatch 记录。

---

## 阶段 B：目录变更核算与实验协议（零产品代码，非实施阶段）

> 本阶段的产物是**给人用的配方与协议**（jq 命令、指标定义、决策规则），
> 不是代码任务。执行代理不得据此编写任何产品代码或内置命令；配方由
> 维护者在 soak 与实验期间于 shell 中直接使用。

### B1 离线核算配方

数据源 = sidecar（A 阶段产物）。四个配方（PowerShell 下可用 jq.exe，或
Git Bash）：

```sh
# 1) 每轮目录构成时间线（对应 dsh 验证清单第 2 项）
jq -c 'select(.request_header) | .request_header
       | {reason, max_tokens, n: (.tool_names|length), tool_names}' \
   sessions/request-audit/<id>.jsonl

# 2) 一个会话经过了几种不同的工具目录（缓存断点预算）
jq -r 'select(.request_header) | .request_header.tools_sha256' \
   sessions/request-audit/<id>.jsonl | sort -u | wc -l

# 3) 目录发生变化的轮次定位（变化点前后对照）
jq -c 'select(.request_header) | .request_header
       | {reason, tools_sha256, tool_names}' \
   sessions/request-audit/<id>.jsonl \
  | awk 'prev != $0 {print NR": catalog change"} {prev = $0}'

# 4) 派发预算审计（对应 dsh 验证清单第 1 项：首请求 maxTokens）
jq -r 'select(.request_header) | .request_header.max_tokens' \
   sessions/request-audit/<id>.jsonl | head -1
```

**预算原则**（评审口径，不写代码）：一个会话的 distinct
`tools_sha256` 数应尽量小；现状合法变化源 = TaskMode 切换、委派批次锁定、
summary-only 空目录轮、hosted web 回退。配方 2 的输出进入会话质量抽查
清单；若发现非预期变化源（例如某轮莫名多/少一个工具），用配方 3 定位并
回溯 P2-H 的归因日志（`llm_runtime.rs:3987-3993` 的
`prefix cache shape changed` 事件）。

已知且接受的现状项（登记，不修）：summary-only 恢复轮 tools 为空表
（`:3696-3697`）——每次恢复轮都是一次目录变化 + 风格重置（dsh zero 变体
的旁证：空工具轮的轨迹风格不延续到后续轮）。这是有意设计，核算时单独
归类，不算漂移。

### B2 实验协议（阶段 C 的评估设计，先于代码定稿）

- **对象**：按 provider × model 分组（r-code 是多 provider 架构；dsh 结论
  只在 DeepSeek V4 Pro 上成立，不得跨模型外推）。首期建议一组：
  当前主力 DeepSeek 系配置。
- **控制变量**：同一 model、同一 max_tokens、同一 temperature、同一
  system prompt（锚定实验不改 system）、同一任务集。任务集建议 5 个
  代表性编码任务（读代码答问 / 定位修复 / 小功能实现 / 重构 / 多文件
  调查），每任务 N=5 次重复。
- **分组**：`first_round_catalog = full`（对照组）vs `readonly` vs
  `editor_pair`（两个实验组）；组内 `promote_on` 固定 `either`。规划门
  扩展后增设第四组 `plan_gate + plan_complete`（目标形态组：剥夺跨回合
  持续到模型调用 plan_ready，重点观察指标 1/2 的首轮思考质量与指标 3
  的总轮数代价）。
- **指标**（每 run 记录，来源 sidecar + 产品状态）：
  1. 首轮 reasoning 首行文本（风格定性）；
  2. 风格标记计数（首轮 reasoning + 正文上正则计数；英文任务用
     `let me|I'll|I will` vs `we|we need|let's`；中文任务对应
     `我来|让我|我将` vs `我们|我们一起`。标记集按模型实际输出校准后
     固化进实验记录，避免事后择优）；
  3. 任务成败与完成轮数（run_budget / 工具轮数）；
  4. 首轮之后的工具使用分布（验证没有因首轮受限产生行为残留）；
  5. distinct `tools_sha256` 数（应恰为 2：bootstrap 目录 + 完整目录）。
- **决策门**（阶段 D 用）：实验组相对对照组在 1/2/3 上有一致方向且
  N 次重复稳定（无单次翻转主导），才讨论改默认；否则保持 opt-in 并在
  本文档登记「该模型下无效应/负效应」。
- **证据登记**：每轮实验结论（配置快照 + 指标原始值 + 判定）追加到本文
  §5 的证据表，模仿 dsh 把聚合证据放独立仓库（modeltest）的纪律——
  r-code 侧以文档 + sidecar 归档为准，不建新仓库。

---

## 阶段 C：首轮锚定实验（opt-in，默认关闭；随阶段 A 连续实施）

> 执行契约约束：本阶段紧接阶段 A 连续实施，不切换分支、不停顿汇报。
> B2 实验协议已随本文定稿，实现时直接遵循其控制变量与指标定义；
> 实验的**运行**（跑任务集、采数据）仍属人工/后续工作，本次只交付代码
> 与测试。

### C1 配置面

```text
仓库：vendor/agent-contracts（与 A1 同一子模块，可同批 bump）
文件：crates/agent-config/src/lib.rs（OrchestrationConfig :266）
```

```rust
/// 首轮派发的工具目录策略（锚定实验）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FirstRoundCatalog {
    /// 默认：不过滤，首轮即完整目录（现状）。
    #[default]
    Full,
    /// 首轮只暴露只读探索五件套。
    ReadOnly,
    /// 首轮只暴露 read_file + edit（对标 dsh Minimal 工具对的编辑变体）。
    EditorPair,
    /// 规划门：首轮起零工作工具，目录仅含 plan_ready（需配 plan_complete
    /// 晋升信号）。剥夺跨回合持续到模型声明规划完成——恶劣环境压榨首轮
    /// 思考的实验形态。
    PlanGate,
}

/// 晋升信号。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FirstRoundPromoteOn {
    /// 默认：本轮 outcome 含任意 assistant 内容（Text 或 ToolUse）即晋升。
    /// 等价 dsh promoteOn: either——纯文字首答不会困死在受限目录。
    #[default]
    Either,
    /// 仅首次 ToolUse 晋升；模型一直不调工具则一直停留在受限目录
    ///（dsh 的 tool-call 模式，保留作对照）。
    ToolCall,
    /// 仅当模型调用 plan_ready 才晋升。剥夺跨回合、跨 run 持续（会话级
    /// 粘性），直到模型自己声明规划完成。纯文本终答自然结束 run，粘性
    /// 保持到下一个 run 的首轮。
    PlanComplete,
}
```

`OrchestrationConfig` 增两字段（`#[serde(default)]`）：
`first_round_catalog: FirstRoundCatalog`、
`first_round_promote_on: FirstRoundPromoteOn`；`Default` 实现同步。

### C2 运行时面

```text
文件：crates/r-code-agent-worker/src/llm_runtime.rs
```

1. **策略镜像**：`OrchestrationPolicy`（`:518`）增同名字段，类型直接复用
   agent-config 的两个枚举（沿用 `delegation_router` 的镜像方式）；
   `ensure_real_runtime` 组装点（`:4735-4752`）透传。
2. **指纹**：两个新枚举加入 runtime 重建指纹元组（`:4760-4770`）——
   红线级要求：漏加会导致改配置后旧 runtime 残留旧策略。
3. **会话级粘性标志**（镜像 F7 的 `delegation_disabled` 模式）：

   ```rust
   // SessionState 增：
   /// C：首轮目录锚定是否仍待晋升。会话级粘性：跨 run 保持，
   /// 避免每个 run 的首轮都重付一次目录变化（缓存断点预算 ≤ 1/会话）。
   catalog_bootstrap_pending: Arc<AtomicBool>,
   ```

   `create_session` 初始化：`orchestration.first_round_catalog != Full`
   时置 true，否则 false。
4. **目录过滤点**（run_loop 每轮装配，`:3696-3700`）：

   ```rust
   let bootstrap_pending = /* sessions.lock() 读 session.catalog_bootstrap_pending 的 Arc 克隆，
                               与 delegation_disabled 同法在 run_loop 序言取一次 */;
   let tools = if summary_only {
       Vec::new()
   } else {
       let mut specs = client_tools_for_hosted_tools(tool_host.tool_specs(), &active_hosted_tools);
       // C：首轮锚定过滤。仅主代理 Main 策略（Ask/Plan 已是受限目录，
       // 二次过滤无意义且会与 hosted 别名逻辑纠缠）；目录裁剪是呈现层，
       // 执行边界仍在 tool_allowed/scoped_input（红线 2）。
       if policy == ToolPolicy::Main && bootstrap_pending.load(Ordering::SeqCst) {
           let allowlist = first_round_allowlist(ctx.orchestration.first_round_catalog);
           specs.retain(|tool| allowlist.contains(&tool.name));
       }
       specs
   };
   ```

   允许清单常量：

   ```rust
   const FIRST_ROUND_READONLY_TOOLS: &[&str] =
       &["read_file", "list_files", "search", "glob", "load_skill"];
   const FIRST_ROUND_EDITOR_PAIR_TOOLS: &[&str] = &["read_file", "edit"];
   ```

   委派工具、MCP、bash、生命周期工具（enter_plan_mode 等）首轮全部不在
   目录中——模型首轮只能读不能委派不能写，这是有意的（对齐 dsh 的
   Minimal 身份：极少工具）。hosted 工具维持现状随行（`ReadOnly` 组如需
   连 hosted 一起剥，登记为实验变体 C-variant-1，不在首期）。
5. **晋升判定点**（outcome 处理，`:4106-4107` 之后）：

   ```rust
   if bootstrap_pending.load(Ordering::SeqCst) {
       let promoted = match ctx.orchestration.first_round_promote_on {
           FirstRoundPromoteOn::Either => outcome
               .appended_messages
               .iter()
               .any(|m| m.role == Role::Assistant),
           FirstRoundPromoteOn::ToolCall => outcome
               .appended_messages
               .iter()
               .any(|m| m.role == Role::Assistant
                   && m.content.iter().any(|b| b.is_tool_use())),
       };
       if promoted {
           bootstrap_pending.store(false, Ordering::SeqCst);
           tracing::info!(session_id = %ctx.session_id,
               "C first-round catalog promoted to full");
       }
   }
   ```

   语义对应 dsh：Either 下首轮 outcome 必然含 assistant 消息 → 晋升恒发生
   在第 2 轮派发前，受限窗口 = 恰好首轮；ToolCall 下模型纯文字回复则
   停留（对照组用途）；PlanComplete 下剥夺跨回合、跨 run 持续（会话级
   粘性），仅模型调用 plan_ready 才晋升。工具执行失败不影响晋升（只看
   ToolUse 是否产生，与 dsh「执行失败仍晋升」一致）。

6. **规划门扩展（`plan_gate` / `plan_complete`，后补实施）**：

   - 允许清单：`first_round_allowlist` 增臂 `PlanGate => &[]`（零工作
     工具）；
   - **spec 注入**：`promote_on == PlanComplete` 且主 run 处于 pending 时，
     在过滤点 retain 之后、锚定计数与 Narrowed 事件之前注入
     `plan_ready_tool_spec()`（Builtin、空 schema、无审批），并按名重排序
     （P1-C 派发字节稳定）——`tool_count` 因此反映真实派发
     （plan_gate + plan_complete = 1；readonly/editor_pair + plan_complete
     = 原清单 + plan_ready）；
   - **执行拦截**：`SessionToolHost` 增共享的 `catalog_bootstrap_pending`
     字段（主 run 传真实标志、子代理恒 false）；`call_inner` 在 gateway
     派发之前拦截 `plan_ready`——pending 返回成功语义（「下一轮恢复完整
     目录」），非 pending 返回可修正错误；全程不转发 gateway、无审批
     （红线 2 的唯一例外）。`host_owned_tool_name` /
     `host_lifecycle_tool_allowed`（`plan_ready` 仅 Main 策略）同步接线；
   - **尾部指令**：每轮尾部 user 消息注入 `build_plan_gate_message`
     （`TAIL_LABEL_PLAN_GATE`，与 plan_mode 消息同法登记进
     excluded_tails），文案按档位区分（plan_gate = 零工作工具版本），
     直到晋升才停；
   - **晋升新臂**：`PlanComplete => appended_messages 中存在 assistant
     ToolUse 且 `tool_name() == "plan_ready"``——唯一的关门信号。

### C3 边界与不变量（实现与评审清单）

- **子代理**：过滤点只在主 run 装配处（`:3699`）；子代理装配点
  （`:752-760`）不经过该逻辑，恒为完整目录——对齐 dsh「子 agent 始终看到
  完整目录」。规划门同样不影响子代理：子代理 host 的
  `catalog_bootstrap_pending` 恒 false，`plan_ready` 拦截直接走非 pending
  分支（可修正错误），`host_lifecycle_tool_allowed` 亦仅 Main 策略放行。
- **summary_only / 恢复轮**：tools 已为空表，过滤不介入；晋升判定与
  governor 流程正交。
- **system prompt**：run 冻结逻辑（`:3508-3519`）零改动。首轮 system 中
  的 MCP 策略段、工具选择规则与受限目录存在**有意的错配**（system 提到
  的个别工具首轮不可见）——这是 dsh 同款取舍（Minimal system 配 Standard
  目录演进），错配程度进入 B2 指标 4 的观察项，不作为缺陷修。
- **前缀缓存**：开启锚定的会话恰好多一次目录变化（bootstrap → full），
  P2-H 照常记录。可选优化（不强制）：给 `cache_shape` 的 cause 词表加
  `catalog_promotion` 标注，便于日志区分有意变化与漂移。
- **进程重启 / runtime 重建**：粘性标志在内存，重启后同会话首轮重新
  锚定（多付一次目录变化）。V1 接受；后续如需持久推导，从 sidecar 的
  RequestHeader.tool_names 链推导（dsh「从持久事件推导」的 r-code 对应
  物），登记为后续项。
- **逃生舱**：默认 `either` 下不存在困死路径；`tool_call` 模式困死是
  对照组的有意行为，配置注释写明。规划门（`plan_complete`）的模型外
  逃生路径有三条：设置页总开关滑纽关闭（catalog 回 `full`，经 runtime
  重建指纹即时重建 runtime，新会话即现状）；`run_budget` 照常终止超时
  run；纯文本终答自然结束 run（粘性只影响下一个 run 的首轮，不阻断
  交付）。
- **enable_caching**：bootstrap 目录非空 → `:3966` 的
  `enable_caching: !tools.is_empty()` 行为不变。规划门下注意：`plan_gate`
  档 + `either`/`tool_call`（退化组合，UI 联动规避）首轮 tools 为空 →
  该轮 `enable_caching = false`，与 summary-only 空表轮同款行为；
  `plan_gate + plan_complete` 下目录恰含 plan_ready（非空），不受影响。

### C4 测试

1. 单测（`llm_runtime_tests.rs`，复用既有 mock provider 接线）：
   - `full`（默认）：两轮请求的 tools 一致——回归保护，断言现有行为字节
     不变；
   - `readonly + either`：首轮 tools ⊆ 五件套且不含 hosted/委派；纯文字
     首答后第二轮恢复完整目录；同会话第三个 run（模拟多轮对话）首轮即
     完整目录（粘性）；
   - `tool_call`：首轮纯文字 → 第二轮仍受限；首轮带 ToolUse → 第二轮
     完整；
   - 执行边界：受限轮模型仍调用目录外工具（构造历史诱导）时，
     `scoped_input` 按 policy 正常执行/拒绝——证明目录过滤不碰安全边界。
2. 手工验收：开启 `first_round_catalog = "readonly"` + 审计开关，跑 B2
   任务集中 1 个任务，用 B1 配方 1/5 核对：首轮 `tool_names` 恰为五件套、
   `distinct tools_sha256 == 2`、max_tokens 与对照组一致。

**回滚**：配置回 `full` 即产品级回滚；代码级按 commit revert，无数据
迁移（sidecar 里多出的记录天然向后兼容）。

---

## 阶段 D：证据评估与阶段门审计（决策门，非实施阶段）

> 本阶段消费实验证据、产出决策，不产出代码。执行代理不得预填证据表，
> 也不得在证据缺席时推动任何默认值变更。

### D1 证据表（实验结论登记处，初始为空）

| 日期 | provider/model | 分组 | 任务集 | 指标 1/2/3 摘要 | 判定 |
| --- | --- | --- | --- | --- | --- |
| — | — | — | — | — | — |

判定取值：`正效应（候选转默认）` / `无效应` / `负效应（保留 opt-in 或移除）`
/ ` inconclusive（加样本重跑）`。每个 model 独立一行结论。

### D1.1 效应的模型边界与生效范围收敛（预先记录的分析结论）

dsh 的实证只在 DeepSeek V4 Pro 上成立；机制（轨迹策略与可见工具目录强
绑定）在重度工具 RL 训练的模型族（GLM/Qwen/Kimi 系）可能存在但幅度与
符号未知，在 Claude/GPT 系上无公开证据且首轮 system/目录有意错配的困惑
成本可能反超收益。由此预先裁定两条：

1. **效应问题是按 model 分组回答的**（B2 协议），任何「跨模型普遍有效」
   的表述都不得写入结论；
2. **若某模型结论为正效应并讨论转默认，生效范围必须按 provider 收敛**，
   不是全局开关：实现形态是在 `create_session` 按 provider 身份判定是否
   武装粘性标志——判别点复用 `is_deepseek_native_provider()`
   （`llm_runtime.rs:5026`）的既有模式扩展为 provider-kind allowlist
   （DeepSeek reasoning governor 已是同款 DeepSeek-only 请求形状干预的
   先例）；全局配置保留为显式覆盖（强制开/强制关）。实施细节属阶段 C+
   的设计输入，本阶段只锁定「按 provider 收敛」这一方向。

### D2 阶段门逃生舱审计（dsh `promoteOn: either` 教训的推广）

每道「以模型发出的信号为条件」的门都确认存在**模型外逃生路径**（用户
操作或预算终止），否则按 C2 的晋升模式补备选信号。现状盘点（实现时
逐项核对，本表为待确认清单而非结论）：

| 门 | 触发信号 | 现有逃生路径 | 待确认 |
| --- | --- | --- | --- |
| Plan→Main（`plan_publish` 只在 Plan 模式放行） | 模型调用 plan_publish | 用户在 UI 切换 TaskMode（`update_task_context` 刷新 policy）；run_budget 终止 run | UI 切换路径是否覆盖「run 进行中」场景 |
| 委派批次锁（第二个及以后的子代理须 `plan_subagents(confirm=true)`） | 模型确认批次 | 拒绝提示文本引导（`:1102-1108`）模型自查补救；用户 steer | 无（已达标） |
| summary-only 恢复轮（空工具） | governor 判定 | governor 自身状态机推进，有界 | 无（已达标） |
| suspension/continuation 门 | run 内事件 | run_budget / abort | 无（已达标） |
| C 新增：首轮目录晋升 | 首轮 outcome（either 默认） | 默认恒晋升；`tool_call` 模式困死为有意对照组；`plan_complete` 模式：设置页总开关滑纽关闭（经指纹重建 runtime）+ run_budget 终止 + 纯文本终答自然结束 run（粘性不阻断交付） | 无（设计内） |

审计产出：如发现新困死路径，单独开 issue，不混入本方案 PR。

---

## 风险与回滚总表

| 阶段 | 主要风险 | 缓解 | 回滚 |
| --- | --- | --- | --- |
| A1 | 子模块字段演进破坏旧读取器 | 全部 `#[serde(default)]` + 往返/缺省双测试 | revert 子模块 commit + 回退 gitlink |
| A3 | sidecar 与 canonical 内容分歧引发自检误报 | 单写方 + 派发前同步 append（无竞态）；log-only 不阻断；中途开启开关的首次 mismatch 属已知并文档化 | 配置翻 false；revert A3.2 |
| A3 | 磁盘体积翻倍（审计会话） | 默认 off；子目录隔离；后续随会话清理联动（登记项） | 同上 |
| A4 | 命令暴露内部计数 | 只读、无敏感内容 | 移除命令 |
| B | 配方误读（别名/hashed 名） | 配方固定用派发后名字；指标定义先于实验定稿（B2） | 无代码，改文档即可 |
| C | 目录变化引入额外缓存断点 | 每会话预算 ≤1 次；P2-H 归因可见；默认 full 零变化 | 配置回 full；revert C commits |
| C | 首轮 system 与目录错配引发模型困惑 | 有意取舍，进入 B2 指标观察；失败模式 = 模型首轮多问一句，either 晋升兜底 | 同上 |
| C | 效应被误泛化到其他模型 | B2 协议强制按 model 分组；D1 证据表逐 model 结论 | 文档纪律 |
| 全局 | 与 harness-migration 阶段 2/3 冲突 | 新代码全在现有接缝内，迁移时随 loop_core/config 镜像一起走；C 的过滤点与晋升判定属运行时逻辑 | 见各阶段 |

## 参照

- 上游洞见与实证：`xiaobright/dsh-anchored-standard`（README.zh-CN 的
  「为什么这样做 / 实测结果 / 验证加载」三节）及其证据仓库
  `xiaobright/modeltest`
- 承接的本地文档：`docs/harness-migration.md`（§0.3 子模块 bump 流程、
  §1.3 本方案的合同半/运行时半、风险表的 log-only soak 策略）
- 前缀缓存设计依据：`docs/archive/deepseek-prefix-cache.md`（P0-A/P1-C/P2-G/P2-H）
- 相关代码入口（行号基准 2026-08-18）：
  `vendor/agent-contracts/crates/agent-contract/src/session.rs:78`、
  `crates/r-code-agent-worker/src/llm_runtime.rs:1763,3996,3696,4092`、
  `src-tauri/src/commands.rs:4644,4801,5687,5342-5541`
