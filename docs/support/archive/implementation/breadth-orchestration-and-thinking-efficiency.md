# 广度任务编排与 DeepSeek 思考效率——AI 执行清单

> 本文档已按 `.agents/skills/prd-to-ai-worklist`（执行契约 v1.1.0）完成长任务化转换。
> 转换固化清单：`docs/archive/implementation/breadth-orchestration-freeze.yaml`（状态 `draft`，稳定指纹由 M0-01 交付的规范化工具回填后转 `frozen`）。
> 本文档是需求、任务状态与验收口径的**唯一事实源**。主 Checklist（§8）持有唯一完成状态；任务卡（§9）与任务包不重复维护 Checkbox。

## 执行导航

新会话不需要阅读全文。按场景进入：

- **首次启动**：§0 执行入口 → §8 主 Checklist 选首个 ready 任务 → 读该任务卡 → 开工。
- **中断恢复**：`artifacts/ai-tasks/current.yaml` → §9 对应任务卡 → 已归档证据 `artifacts/ai-tasks/evidence/`。
- **验收 / 判断某任务是否真完成**：§6 Harness → §8 该任务的断言 ID → `artifacts/ai-tasks/verification/`。
- **改需求 / 判断要不要解冻重写**：§5 需求追踪表 → 固化清单的 material change 触发条件。
- **背景与动机**：§1.1 对照实验。

| 区块 | 章节 |
| --- | --- |
| 执行入口与安全边界 | §0 |
| 背景、目标、终态、非目标 | §1 |
| 已冻结决策（固定约束） | §2 |
| 仓库事实表 | §3 |
| 需求追踪表 | §4 |
| 机器契约 | §5 |
| Verification Harness | §6 |
| 里程碑与主 Checklist | §7 §8 |
| 详细任务卡 | §9 |
| 进度、任务包与证据 | §10 |
| Bootstrap 默认值与 AI 决策原则 | §11 |
| 风险与外部放行 | §12 |

---

## 0. AI 执行入口

- **首次启动**：只读 preflight（git revision、worktree 状态、`cargo --version`、`node --version`、`git submodule status`、基线 `cargo clippy --workspace --all-targets -- -D warnings` 与 `node scripts/release.mjs check` 通过性）；随后从 §8 选择编号最小且依赖满足的未完成任务，复制 `artifacts/ai-tasks/templates/current-task.template.yaml` 生成 `artifacts/ai-tasks/current.yaml`，按 §9 任务卡实施。
- **续跑**：读 `artifacts/ai-tasks/current.yaml` 与对应证据；对 `completed_assertions` 跑最小 smoke（Harness `--task` 单项）确认未失效；从 `remaining_assertions` 继续。工作区用户新增改动视为资产，不得 reset/覆盖。
- **完成状态**：任务全部 required 断言通过 + Harness `--task` 返回 0 + 累计门禁通过 + 证据归档后，把 §8 主 Checklist 对应项改为 `[x]` 并更新 §10 进度；立即进入下一任务，不等待确认。
- **安全写入范围**：允许——主仓库源码/测试/文档/脚本、`vendor/agent-contracts` 子模块（在其 `main` 分支提交并推送后，父仓库指针随任务提交更新）、`artifacts/ai-tasks/`。禁止——真实凭据写入任何文件、修改 `.agents` 子模块、删除或重置用户已有改动、发布 tag、改动 `CHANGELOG.md` 既有已发布小节（新条目只进 `[Unreleased]`）。
- **本清单只做实施**：文档即契约，实施过程中发现规范冲突按 §11 决策阶梯处理；需要改规范本身时按固化清单解冻规则走，不得边实施边顺手改需求。

## 1. 背景、目标、终态与非目标

### 1.1 对照实验（动机证据）

同一 Python 项目（falib，`D:\project\py\falib`）双审计对照：GPT sol max 单代理版（`docs/audit/falib-project-audit-a.md`，12 P1）全面优于 DeepSeek + Codex 多子代理版（`docs/audit/falib-code-audit-2026-08-23.md`，4 P1 且误报 basedpyright"不可用"）。归因四个失败模式：

| ID | 失败模式 | 实证 |
| --- | --- | --- |
| M1 | 探测早停 | basedpyright 只探测 test venv 与 PATH 即断言"不可用"，实际根 `.venv/Scripts/` 存在（51 errors/8 warnings） |
| M2 | 横切面缺失 | 未查锁文件/CI/凭据/供应链——不属于任何单一代码模块，按模块切分的子代理天然扫不到 |
| M3 | 跨文件契约丢失 | `settings.db.backend`、`clickhouse.echo`、`fs.secure`、`env_nested_delimiter` 四个 P1 均需远距离两文件同上下文 diff |
| M4 | 汇总门缺失 | 各子代理发现仅做并集，无强模型去重、跨项综合与"非发现"整理 |

结论：差距 ≈ 70% harness（M1–M4 均可在编排层治理）+ 30% 模型。DeepSeek 单点求证质量不差，问题是深度思考挤占枚举预算。

### 1.2 目标（RequirementRef）

| 需求 ID | 内容 | 由哪些特性承载 |
| --- | --- | --- |
| F1 | 探测纪律：环境/工具探测不得在证据不足时输出"不可用/不存在"级断言（治 M1） | M1-02、M3-03 |
| F2 | 思考占比可视化：reasoning token 落账并在 UI 可见，数据来自真实分账（治"不可观测"） | M1-01、M1-03 |
| F3 | Breadth Mode：审计/枚举类任务可路由到"编排者横切清单 + 并行只读子代理 + 强模型汇总门"形态（治 M2/M4） | M3-01～M3-04 |
| F4 | 跨文件契约任务：schema/契约比对成为一等任务模板（治 M3） | M2-01 |
| F5 | Governor 可评估：调速器阈值可配置、可通过 eval 量化对比 | M4-01～M4-03 |
| C1 | 探测断言合同（见 §2） | M1-02、M3-03 |
| C2 | 广度编排合同（见 §2） | M3-02、M3-03 |
| C3 | 思考数据合同（见 §2） | M1-01、M1-03 |
| C4 | 配置与回滚合同（见 §2） | M0-02、M4-01 |

### 1.3 Definition of Done（终态，全部为可观察系统状态）

- 全部新配置默认值下，行为与当前版本一致：既有全量测试绿、Clippy 绿、无新增审计事件。
- `reasoning_tokens` 在 DeepSeek 系 Provider 请求审计中真实落账；旧 JSONL 反序列化为 0 不报错；UI 展示"未记录"而不是估算值。
- `breadth_mode = "auto"` 下对固定语料执行广度任务：时间线呈现编排结构（分片 run 分组 + 汇总门 run）；最终报告含"非发现"小节；全链 run_id/request_id 可追溯。
- `schema-contract-scan` 模板对四形态契约缺陷语料全部命中（见 M2-01 断言）。
- `breadth-eval` 双臂跑批产出机器可读报告，预注册覆盖率阈值达成。
- Harness `--through M4 --profile implementation` 返回 0 且有证据索引。

### 1.4 非目标

- 不改变 Provider 模型本身的思考行为。
- 不替代 Plan 锚定（`PlanMinimalV1`）：广度编排是执行阶段任务形态，与规划阶段收窄互不替代。
- 不引入新的子代理执行引擎：复用现有子代理池与 Codex 跨引擎委派。
- 不做"审计报告生成器"产品化封装。

## 2. 已冻结决策（固定约束，实施不得推翻）

**C1 探测断言合同**
- 探测类结论只允许两种形式：「在 ⟨已探测位置清单⟩ 未找到」或「已执行，结果为 ⟨事实⟩」。
- "不可用/不存在/未安装"级断言必须以探测清单覆盖当前 venv、项目根 venv、PATH、包管理器清单四类为前提。
- 违反形式的结论在汇总门必须被降级或退回。

**C2 广度编排合同**
- 横切清单（锁文件/CI/凭据/供应链/配置契约/测试环境）由编排者持有并执行，不得下放给模块分片子代理。
- 分片子代理默认只读；任何写操作走既有审批链，不因广度模式放宽。
- 汇总门必须由 reviewer 配置的模型执行，产出必须包含「非发现」小节。
- 编排全程每个子代理的 run_id / request_id 必须可追溯。

**C3 思考数据合同**
- UI 展示的 reasoning 占比必须来自 request-audit 落账的真实数值；Provider 未返回 reasoning 用量的旧数据展示「未记录」，不得用字符估算冒充。
- `reasoning_tokens` 字段必须 `#[serde(default)]`；旧 JSONL 反序列化为 0 且不报错。

**C4 配置与回滚合同**
- 所有新行为挂 `[orchestration]` 新键，默认值等于当前行为；关闭后与当前版本行为一致。
- 提示词注入必须作为 system 尾部注入并计入 request-audit 可见的记录；用户可在审计中看到注入发生。

**架构冻结决策**
- 配置结构体落在 `agent-config`（子模块）`OrchestrationConfig`，主仓库只做透传——沿用 `delegation_router` 既有模式。
- `reasoning_tokens` 落在 `agent-contract`（子模块）`SessionEvent::RequestHeader` 预算组，不新增事件类型承载用量。
- Governor 状态机语义（`DeepSeekReasoningGovernor`）不变，只做阈值/开关外部化。
- 汇总门复用 `quality_reviewer` 的解析与回退语义，不建第二套 reviewer 配置。

## 3. 仓库事实表

| 事实 | 锚点 | 对计划的影响 |
| --- | --- | --- |
| 推理调速器已存在：Standard→CheapExploration→FullFinalization 状态机，只读探索轮 reasoning ≥ 6000 字符（≈1500 token）降档 | `crates/r-code-agent-worker/src/llm_runtime.rs:243`–345 | M4-01 只做配置外部化 |
| `RequestHeader` 预算组有 text/tool schema/image/document 四类 token 与 wire bytes，**无 reasoning 字段** | `vendor/agent-contracts/crates/agent-contract/src/session.rs`（预算审计组） | M1-01 需改子模块 |
| `OrchestrationConfig`（`delegation_router` 等）在 `agent-config` 子模块 | `vendor/agent-contracts/crates/agent-config/src/lib.rs:338` | M0-02/M4-01 需改子模块；子模块在其 main 提交推送后父仓库更新指针 |
| 只读工具同轮最多 4 路并发 | `crates/r-code-agent-worker/src/agent_loop.rs:434`、`:530` | 子代理内并发沿用，进程级并行靠分片数 |
| 配置→运行时透传模式 | `src-tauri/src/commands.rs:4871` 一带 | M0-02 照此模式 |
| thinking 三档 `enabled/disabled/adaptive` | `src-tauri/src/commands.rs:3968`–3977 | 不新增档位 |
| 子代理报告摘要包络 `SUBAGENT_REPORT_SUMMARY_TARGET_MAX_CHARS = 5000` | `crates/r-code-agent-worker/src/llm_runtime.rs:240` | M3-03 汇总输入复用包络 |
| 质量复核 `quality_loop`/`quality_reviewer`（auto/r_code/codex） | `src-tauri/src/plan_policy.rs`、设置页编排区块 | 汇总门复用其语义 |
| eval 基建：预注册、语料锁、评分脚本、隔离 state | `eval/plan-eval/`、`src-tauri/src/bin/plan_eval.rs`（`build_isolated_state`） | M4-02 复用 |
| 脚本测试惯例：`scripts/*.mjs` + `node --test scripts/*.test.mjs` | `scripts/release.mjs` + `release.test.mjs` | Harness 沿用该模式 |
| 子模块提交流程：子模块 main 提交推送 → 父仓库指针随功能提交 | 本次 v1.0.0 发布已验证 | 各子模块任务卡内置该步骤 |
| CI：`cargo clippy --workspace --all-targets -- -D warnings` 在 ubuntu stable 上跑；本地 Windows 看不到 linux-cfg lint | `.github/workflows/ci.yml` | 所有 Rust 任务卡要求本地全量 clippy + fmt |

## 4. 需求追踪表

```text
RequirementRef → 里程碑 → TaskID → AssertionID（任务卡内定义） → EvidencePath
F1  → M1/M3 → M1-02, M3-03 → M1-02.A1..A3, M3-03.A3 → artifacts/ai-tasks/evidence/{M1-02,M3-03}.yaml
F2  → M1    → M1-01, M1-03 → M1-01.A1..A3, M1-03.A1..A3 → 同上规则
F3  → M3    → M3-01..M3-04 → 各卡断言
F4  → M2    → M2-01 → M2-01.A1..A3
F5  → M4    → M4-01..M4-03 → 各卡断言
C1  → M1/M3 → M1-02, M3-03
C2  → M3    → M3-02, M3-03
C3  → M1    → M1-01, M1-03
C4  → M0/M4 → M0-02, M4-01
```

孤儿检查：F1–F5、C1–C4 均有任务与断言；所有任务均可回溯需求（M0-01/M0-02 是 F/C 的执行基础设施，追溯到 C4 与 Harness 要求）。

## 5. 机器契约

### 5.1 配置（agent-config，子模块）

```toml
[orchestration]
# 既有键不动。新增：
breadth_mode = "off"            # off | suggest | auto；默认 off
probe_discipline = false        # F1 注入开关，独立于 breadth_mode；默认 false

[orchestration.reasoning_governor]
enabled = true                  # 默认 true = 现状（调速器已启用）
exploration_threshold_tokens = 1500   # 默认 1500 = 现 6000 字符常量；内部换算 chars = tokens * 4
applies_to = ["deepseek-v4", "ark-adaptive", "kimi-adaptive"]  # 与现 reasoning_governor_kind 集合一致
```

- 全部新键 `#[serde(default)]`；缺省旧配置文件双读兼容。
- `reasoning_governor` 配置变更只影响新 run；进行中 run 不热更新。
- 前端设置项：`设置 → Agent 编排 → 委派路由` 区块下方新增"广度任务编排"行（三档 select + InfoTip）；governor 高级配置仅诊断页入口。

### 5.2 协议（agent-contract，子模块）

- `SessionEvent::RequestHeader` 预算组新增 `reasoning_tokens: u64`，`#[serde(default)]`，序列化规则与相邻数值字段一致（缺 0 不省略或统一 skip 规则——实现时与相邻 u32 字段保持同一种风格并记录决定）。
- 旧版本写入的行反序列化为 0；0 语义 = "未记录"，UI 不得当作"思考为零"参与占比计算。

### 5.3 注入与事件审计

- F1 探测纪律注入：system 尾部单次注入，注入文案为 §9 M1-02 任务卡冻结文本；注入必须计入 request-audit（锚定阶段记录或 `SessionEvent::System` 事件，二选一，实现时记录决定）。
- Breadth Mode 编排结构事件：进入编排、分片派发、汇总门执行三类事实必须可审计（复用 `SessionEvent::System` 或既有子代理事件；不新增表）。
- Governor 降档（Standard→CheapExploration）事实必须在时间线可见（事件或审计字段，实现时记录决定）。

### 5.4 Harness 产物格式

- 报告：`artifacts/ai-tasks/verification/<profile>/<task-or-milestone>.json`，含 `task_id`、`assertions[]`（id/passed/command/exit_code）、`revision`、`worktree_digest`、`exit_code`。
- 固化指纹：`ai-worklist-norm.v1` 规范化（checkbox 状态、进度统计、证据路径、时间戳不计入；§5 全部小节 + §8 任务 ID/依赖 + §9 任务卡契约字段计入），SHA-256，由 `scripts/verify-breadth.mjs digest` 子命令计算。

## 6. Verification Harness

统一入口（M0-01 交付）：

```text
node scripts/verify-breadth.mjs --task <TASK_ID>     --profile implementation
node scripts/verify-breadth.mjs --through <MILESTONE> --profile implementation
node scripts/verify-breadth.mjs --through M4          --profile production
node scripts/verify-breadth.mjs digest                # 计算固化指纹并回填 freeze
```

- 断言注册表：脚本内 `REGISTRY` 映射每个断言 ID → 非交互命令（cargo test 过滤器 / node --test 文件 / 脚本检查），每项标注需求引用与层级（contract/unit/integration/e2e/regression）。
- profile：`implementation` = 本地可离线完成的全部断言（fixture/fake 级）；`production` = 需要真实 Provider 凭据与真实跑批的断言（仅 M4-03 与 M3 验收的语料实跑项）。
- 退出码 0 仅表示全部 required 断言通过；输出机器可读 JSON 报告与失败断言列表；记录 revision 与 worktree digest；密钥只记录"是否可解析"。
- 反作弊：required 断言缺失按失败；不得删测试/降阈值/缩范围修绿；fake 结果必须带 profile 标签。

## 7. 里程碑与出口判据

| 里程碑 | 能力范围 | 出口判据（累计 Harness） |
| --- | --- | --- |
| M0 执行地基 | 统一 Harness + 断言注册表 + 固化指纹工具；配置契约与透传 | `--through M0 --profile implementation` = 0；freeze 转 frozen |
| M1 探测纪律与思考可视化 | F1 注入、F2 全链（协议→落账→聚合→UI） | `--through M1` = 0 |
| M2 契约模板 | F4 模板 + 四形态语料全命中 | `--through M2` = 0 |
| M3 Breadth Mode | F3 完整编排（意图→分片→汇总→可视化）与 C1/C2 合同 | `--through M3` = 0 |
| M4 评估与收口 | F5 governor 配置化 + breadth-eval + 阈值修订 | `--through M4 --profile implementation` = 0；production 档见 §12 |

里程碑通过后直接进入下一阶段，不设汇报/确认节点。

## 8. 主 Checklist（唯一状态源）

```markdown
- [ ] **M0-01** 统一验证 Harness、断言注册表与固化指纹工具。证据：待生成
- [ ] **M0-02** 配置契约与运行时透传（agent-config 子模块 + 主仓库）。证据：待生成
- [ ] **M1-01** RequestHeader.reasoning_tokens 协议字段与 DeepSeek 落账。证据：待生成
- [ ] **M1-02** F1 探测纪律注入与审计记录。证据：待生成
- [ ] **M1-03** reasoning 聚合与 UI 可视化（含 governor 降档可见性）。证据：待生成
- [ ] **M2-01** schema-contract-scan 模板与四形态回归语料。证据：待生成
- [ ] **M3-01** 编排提示三件套与广度意图识别（suggest 交互）。证据：待生成
- [ ] **M3-02** 横切清单执行与分片子代理调度（含审计分组）。证据：待生成
- [ ] **M3-03** 汇总门：reviewer 解析、失败回退与非发现产出。证据：待生成
- [ ] **M3-04** 编排时间线可视化、设置项与中断降级路径。证据：待生成
- [ ] **M4-01** Governor 配置外部化（阈值/开关/模型族）。证据：待生成
- [ ] **M4-02** breadth-eval 评估臂：预注册、语料锁、评分脚本。证据：待生成
- [ ] **M4-03** 双臂跑批、实验报告与默认阈值收口。证据：待生成
```

依赖 DAG（`depends_on`）：

```text
M0-01: —                 M3-01: M0-02
M0-02: M0-01             M3-02: M3-01, M2-01
M1-01: M0-01             M3-03: M3-02
M1-02: M0-01, M0-02      M3-04: M3-02, M3-03
M1-03: M1-01             M4-01: M0-02
M2-01: M0-01             M4-02: M0-01, M2-01
                         M4-03: M3-04, M4-01, M4-02
```

首个 ready 任务：**M0-01**。

## 9. 详细任务卡

> 任务卡与主 Checklist 同 ID，不重复 Checkbox。

### M0-01 统一验证 Harness、断言注册表与固化指纹工具

- **结果**：一条非交互命令可验证任一任务断言、累计到任一里程碑、按 profile 隔离运行，并产出机器可读报告；固化指纹可确定性复算。
- **需求引用**：C4、§5.4、§6。
- **依赖**：无。
- **前置事实**：仓库脚本惯例为 `scripts/*.mjs` + `node --test scripts/*.test.mjs`（`release.mjs`/`release.test.mjs` 先例）；无既有统一 runner。
- **固定约束**：非交互、标准退出码；required 缺失按失败；报告含 revision/worktree digest；密钥只记录是否可解析；`digest` 规范化规则按 §5.4，checkbox 状态与进度统计不计入指纹。
- **决策空间**：注册表结构（对象/数组）、报告字段冗余度——按 `release.mjs` 风格取最简可测实现；`worktree_digest` 用 `git status --porcelain` 输出的 SHA-256。
- **产物**：`scripts/verify-breadth.mjs`、`scripts/verify-breadth.test.mjs`、`artifacts/ai-tasks/` 目录骨架（templates 副本就位）、freeze 指纹回填。
- **实施步骤**：
  1. 只读预检：确认 `scripts/` 现有模式与 `artifacts/ai-tasks/` 状态。
  2. 实现注册表与命令解析（`--task`/`--through`/`--profile`/`digest`）；初始注册本任务与 M0-02 起的全部断言占位（未实现断言标记 `pending`，`--task` 对未实现任务报错并提示先实现）。
  3. 实现 `digest` 子命令：按 §5.4 规范化本 PRD 文档并计算 SHA-256，写入 `docs/archive/implementation/breadth-orchestration-freeze.yaml`（含 `normative_input.digest`、`worklist.digest`、门禁报告路径）。
  4. 测试：参数解析、required 缺失判失败、报告结构、digest 对 checkbox 翻转不敏感（同一文档 `[ ]`→`[x]` 后 digest 不变）。
  5. 运行 `digest` 回填 freeze，状态 `draft`→`frozen`（前提：门禁自查通过）。
  6. 证据归档：验证报告 + freeze 更新。
- **验收断言**：
  - `M0-01.A1`（unit）：`node --test scripts/verify-breadth.test.mjs` 退出码 0。
  - `M0-01.A2`（contract）：`node scripts/verify-breadth.mjs --task M0-01 --profile implementation` 退出码 0 且生成 `artifacts/ai-tasks/verification/implementation/M0-01.json`。
  - `M0-01.A3`（contract）：连续两次 `digest` 调用，第二次工作区 diff 为空；手工翻转任一 checkbox 后 digest 不变。
- **验证**：`node scripts/verify-breadth.mjs --task M0-01 --profile implementation`。
- **证据**：`artifacts/ai-tasks/evidence/M0-01.yaml`。
- **失败处理**：digest 不稳定时优先排查规范化规则遗漏（哪些字段属于易变派生状态），不得用"手动固定值"绕过。

### M0-02 配置契约与运行时透传

- **结果**：`breadth_mode`、`probe_discipline`、`reasoning_governor` 三组配置可从 `config.toml` 读取并透传到运行时结构；默认值下行为与现状一致。
- **需求引用**：C4、F1/F3/F5 的配置面、§5.1。
- **依赖**：M0-01。
- **前置事实**：`OrchestrationConfig` 在 `agent-config` 子模块（`vendor/agent-contracts/crates/agent-config/src/lib.rs:338`）；透传先例 `src-tauri/src/commands.rs:4871`；设置往返测试先例（规划门档位透传往返）。
- **固定约束**：全部新键 `#[serde(default)]`；默认值 = 现状行为；governor 配置只影响新 run；子模块提交在其 main 分支推送后父仓库指针随本任务提交。
- **决策空间**：枚举命名（如 `BreadthMode::Off/Suggest/Auto`）；governor 配置作为 `OrchestrationConfig` 内嵌结构体的字段组织方式。
- **产物**：agent-config 结构与默认值、主仓库透传、settings 往返测试、子模块指针更新。
- **实施步骤**：
  1. 只读预检：子模块状态（`git -C vendor/agent-contracts status`）、现有枚举与默认值模式。
  2. 子模块：新增枚举与字段 + `#[serde(default)]` + 默认值 + 单元测试（缺省解析、非法值报错口径与既有键一致）。
  3. 子模块 main 提交推送（`feat(config): orchestration breadth/governor 配置契约`）。
  4. 主仓库：透传到运行时结构（照 4871 模式）+ 前端 types 同步（`src-tauri/frontend/src/lib/types.ts` 相邻字段处）。
  5. 测试：settings 往返（三键各非默认值写入读回）、默认配置解析等于现状枚举值。
  6. Harness 注册表登记断言；本地 `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --all`。
- **验收断言**：
  - `M0-02.A1`（unit，子模块）：agent-config 缺省配置解析出 `breadth_mode=off`、`probe_discipline=false`、governor 默认块。
  - `M0-02.A2`（integration，主仓库）：settings 往返测试通过（既有测试文件追加）。
  - `M0-02.A3`（regression）：`node scripts/verify-breadth.mjs --through M0 --profile implementation` 退出码 0。
- **验证**：`node scripts/verify-breadth.mjs --task M0-02 --profile implementation`。
- **证据**：`artifacts/ai-tasks/evidence/M0-02.yaml`。
- **失败处理**：子模块推送权限或 CI 问题时，主仓库部分不可先行合并（指针一致性优先），按 §11 隔离外部阻塞继续 M1-01。

### M1-01 RequestHeader.reasoning_tokens 协议字段与 DeepSeek 落账

- **结果**：DeepSeek 系 Provider 每轮请求审计记录真实 reasoning token；其他 Provider 记 0；旧数据兼容。
- **需求引用**：F2、C3、§5.2。
- **依赖**：M0-01。
- **前置事实**：`RequestHeader` 预算组在 agent-contract 子模块；worker 组装点在 `llm_runtime.rs:5166` 一带（预算审计组同位置）；DeepSeek 响应 usage 含 reasoning 字段（OpenAI Chat 兼容格式 `reasoning_tokens` 或 DeepSeek 变体——实现时以实际响应为准记录）。
- **固定约束**：`#[serde(default)]`；0 = 未记录；估算值（4 字符/token）只允许作为 governor 内部调速信号，不得落账冒充真实用量。
- **决策空间**：从响应哪个字段取值（usage.completion_tokens_details.reasoning_tokens 等，按实际协议响应确定并记录）；序列化风格与相邻字段对齐。
- **产物**：agent-contract 字段、worker 落账、单元/集成测试、子模块指针更新。
- **实施步骤**：
  1. 子模块：字段 + serde 规则 + 测试（旧 JSONL 反序列化 = 0）。
  2. 子模块 main 提交推送。
  3. worker：DeepSeek 响应解析落账；fixture 构造带/不带 reasoning 的响应各一。
  4. 测试：落账有值/0/缺省三态；`docs/archive/implementation/request-audit-and-anchoring.md` 字段说明。
  5. Harness 断言登记；clippy + fmt；CHANGELOG `[Unreleased]`。
- **验收断言**：
  - `M1-01.A1`（unit）：带 reasoning 的 fixture 响应落账后 `RequestHeader.reasoning_tokens > 0` 且等于 fixture 值。
  - `M1-01.A2`（unit）：不带 reasoning 的响应落账为 0；旧 JSONL（无字段）解析不报错且为 0。
  - `M1-01.A3`（contract）：request-audit 文档包含该字段定义。
- **验证**：`node scripts/verify-breadth.mjs --task M1-01 --profile implementation`。
- **证据**：`artifacts/ai-tasks/evidence/M1-01.yaml`。
- **失败处理**：DeepSeek 实际响应字段与预期不符时，以一次真实响应（脱敏）记录实际结构并调整解析，不得凭记忆猜字段名。

### M1-02 F1 探测纪律注入与审计记录

- **结果**：`probe_discipline = true` 时，命中探测意图的 run 在首轮前注入一次探测纪律 system 提示，注入可审计；false 时零行为差异。
- **需求引用**：F1、C1、C4、§5.3。
- **依赖**：M0-01、M0-02。
- **前置事实**：既有 system 尾部注入先例（`DEEPSEEK_FULL_FINALIZATION_PROMPT`，`llm_runtime.rs:247`）；意图词表可参考 plan 建议注册的复杂度信号模式。
- **固定约束**：注入文案冻结为下述文本（语义不得弱化，措辞微调需在 freeze 层面记录）；每 run 至多一次；注入计入审计。
- **决策空间**：词表具体条目（审计/检查/探测/可用性/环境/工具链/依赖/--check/版本核验为种子，可按仓库惯例扩充）；审计载体（锚定阶段记录 vs `SessionEvent::System` 事件）二选一并记录。
- **冻结文案**：
  ```text
  [system] Environment probing discipline: before claiming a tool is
  "unavailable" or "not installed", you must have checked, in this order:
  (1) the active virtualenv, (2) the project root's default venv directories,
  (3) PATH, (4) the package manager inventory for the relevant ecosystem
  (pip/uv list, cargo install --list, npm ls -g as applicable). Allowed
  conclusion forms are: "not found in <list of probed locations>" or
  "executed, result: <facts>". A claim of nonexistence without an exhausted
  probe list is an unsupported assertion and must not be written.
  ```
- **产物**：词表常量、注入点、审计记录、单元测试。
- **实施步骤**：
  1. 词表常量 + 意图判定（任务文本与 Plan 产物信号）。
  2. 注入点接入 run 启动路径；审计记录；每 run 一次的防重。
  3. 测试：开关 off 零差异（审计无注入记录）；on 时命中词注入一次、不命中不注入、同 run 第二轮不重注。
  4. Harness 断言登记；CHANGELOG。
- **验收断言**：
  - `M1-02.A1`（unit）：off 时 run 审计中不存在注入记录（与现状一致的黄金断言）。
  - `M1-02.A2`（unit）：on + 命中任务 → 恰好一次注入且审计可见。
  - `M1-02.A3`（unit）：on + 不命中任务 → 零注入。
- **验证**：`node scripts/verify-breadth.mjs --task M1-02 --profile implementation`。
- **证据**：`artifacts/ai-tasks/evidence/M1-02.yaml`。
- **失败处理**：意图误报率高时不扩大拦截，只调词表精度并在 evidence 记录调整依据。

### M1-03 reasoning 聚合与 UI 可视化

- **结果**：时间线 run 详情与请求审计面板展示每轮 reasoning token 与占比、run 汇总占比；governor 降档事实可见；旧数据"未记录"。
- **需求引用**：F2、C3、§5.3。
- **依赖**：M1-01。
- **前置事实**：前端已有请求审计消费链（`src-tauri/frontend/src/lib/types.ts`、时间线 run 详情组件）；占比口径在 §5.2 定为 `reasoning / (estimated_input_tokens + reasoning)`（若实现中发现该口径歧义，记录决定后统一）。
- **固定约束**：数据只来自落账字段；0 一律展示"未记录"参与豁免；不得前端估算。
- **决策空间**：占比条视觉形态（对齐现有 token 展示样式）；governor 降档展示为系统事件行还是徽标。
- **产物**：host 聚合、IPC 字段、前端展示、组件测试（`node --test` 先例）。
- **实施步骤**：
  1. host 聚合（复用现有 token 汇总口径处）；IPC 透出。
  2. 前端展示 + 旧数据分支；governor 事件可见化。
  3. 测试：三态展示（有值/0/旧数据）、降档事件渲染。
  4. Harness 断言登记；CHANGELOG。
- **验收断言**：
  - `M1-03.A1`（unit）：聚合逻辑对多轮样本计算正确的总量与占比。
  - `M1-03.A2`（e2e，前端脚本测试）：审计面板三态渲染正确（参照 `app-shell.test.mjs` 模式新增用例）。
  - `M1-03.A3`（unit）：governor 降档事实在时间线可见。
- **验证**：`node scripts/verify-breadth.mjs --task M1-03 --profile implementation`。
- **证据**：`artifacts/ai-tasks/evidence/M1-03.yaml`。
- **失败处理**：UI 测试时序不稳时按既有"轮询等待"模式改造，不得放宽断言。

### M2-01 schema-contract-scan 模板与四形态回归语料

- **结果**：一等任务模板可产出「声明未使用 / 使用未声明 / 语义不匹配（含静默回退单列）」三类清单，每项带双端 file:line 证据；四形态语料全部命中。
- **需求引用**：F4、§1.1 M3。
- **依赖**：M0-01。
- **前置事实**：四形态来自 falib 实验（`settings.db.backend`=使用未声明、`clickhouse.echo`=注释字段仍被访问、`fs.secure`=getattr 静默回退、`env_nested_delimiter`=文档承诺未实现）。
- **固定约束**：静默回退类必须单列；输出结构固定为三类 + 双端证据；语料为最小化脱敏快照（不含真实凭据）。
- **决策空间**：模板载体（提示模板 + 只读工具组合，不新增 IPC 命令）；语料目录位置（`eval/breadth-eval/corpus/contract-forms/` 或独立目录，实现时定）。
- **产物**：模板文本、四形态语料快照、结构校验脚本（参照 plan-eval evidence-scripts 模式）。
- **实施步骤**：
  1. 模板文本定稿（含输出 schema）；语料四快照构造（每形态一个最小 Python 包）。
  2. 结构校验脚本：对模板产出 JSON 做三类分类 + 双端证据存在性校验。
  3. 测试：四形态各一断言；`node --test` 脚本测试。
  4. Harness 登记；CHANGELOG。
- **验收断言**：
  - `M2-01.A1..A4`（integration）：四形态语料各自命中正确类别与双端证据（每形态一条）。
  - `M2-01.A5`（contract）：校验脚本 `node --test` 通过。
- **验证**：`node scripts/verify-breadth.mjs --task M2-01 --profile implementation`。
- **证据**：`artifacts/ai-tasks/evidence/M2-01.yaml`。
- **失败处理**：形态命中失败时先修模板输出结构，不改语料真值。

### M3-01 编排提示三件套与广度意图识别

- **结果**：`breadth_mode = suggest` 时命中广度意图的任务在输入区给出一次性建议（复用 plan 建议滑钮交互模式）；三件套提示（编排者职责收窄 / 分片计划生成 / 汇总门）定稿并冻结。
- **需求引用**：F3、C2、C4。
- **依赖**：M0-02。
- **前置事实**：plan 建议交互先例（`plan_entry_commands.rs` 的 ArmedPlanSuggestion、输入区滑钮）；`suggest_complex_tasks` 开关模式。
- **固定约束**：三件套提示语义冻结（职责收窄=编排者只做横切清单/分片计划/汇总门三件事；分片计划=每片带必查问题模板；汇总门=去重+统一分级+非发现+断言形式校验）；建议每 branch 至多一次；`off` 零行为差异。
- **决策空间**：词表与意图判定阈值；三件套具体措辞（语义不变前提下）。
- **产物**：三件套常量、意图识别、suggest 交互、测试。
- **实施步骤**：
  1. 三件套文本定稿并作为冻结产物写入本卡 evidence（后续改动走 freeze 解冻）。
  2. 意图识别 + suggest 交互接入（照 plan 建议模式）。
  3. 测试：off/suggest/auto 三档行为、建议不重复、接受后进入编排（auto 直接进入并提示可退出）。
  4. Harness 登记；CHANGELOG。
- **验收断言**：
  - `M3-01.A1`（unit）：off 档零行为差异黄金断言。
  - `M3-01.A2`（unit）：suggest 档命中意图给一次建议，拒绝后同 branch 不再出现。
  - `M3-01.A3`（unit）：auto 档进入编排且 run 审计记录 BreadthModeEntered。
- **验证**：`node scripts/verify-breadth.mjs --task M3-01 --profile implementation`。
- **证据**：`artifacts/ai-tasks/evidence/M3-01.yaml`（含三件套冻结文本）。
- **失败处理**：意图误报时收窄词表，不得为覆盖率放宽到普通任务。

### M3-02 横切清单执行与分片子代理调度

- **结果**：编排者亲自执行六项横切清单（锁文件/CI/凭据/供应链/配置契约[转 M2-01 模板]/测试环境），生成垂直分片计划并以只读子代理并行执行（默认 4 路，上限可配）；编排结构落审计。
- **需求引用**：F3、C2、§1.1 M2。
- **依赖**：M3-01、M2-01。
- **前置事实**：子代理池与只读默认、报告摘要包络（5000 字符）；`is_parallel_read_tool` 4 路并发为单代理内限制，分片间并行由子代理数量提供。
- **固定约束**：横切清单六项不得下放；子代理只读强制；分片数上限默认 4（配置面进 governor 块相邻位置或独立键——实现时记录）；每片 run_id 可追溯；子代理全失败时出降级报告并显式标注，不得伪装完整审计。
- **决策空间**：分片策略（按目录/按模块）；横切清单各项的具体检查动作描述；审计分组事件命名。
- **产物**：横切清单常量、分片调度、审计分组、失败降级、测试。
- **实施步骤**：
  1. 横切清单常量与编排者执行路径（含把配置契约项实例化为 M2-01 模板调用）。
  2. 分片计划生成 + 子代理调度（复用现有池；只读策略校验）。
  3. 审计分组事件（进入/分片派发）落账。
  4. 测试：只读不可越权（安全负向）、全失败降级、审计可追溯。
  5. Harness 登记；CHANGELOG。
- **验收断言**：
  - `M3-02.A1`（security）：分片子代理写操作被拒（对照既有子代理安全测试）。
  - `M3-02.A2`（integration）：编排 run 审计含横切清单执行记录与每片 run_id 分组。
  - `M3-02.A3`（unit）：子代理全失败路径产出显式降级报告。
- **验证**：`node scripts/verify-breadth.mjs --task M3-02 --profile implementation`。
- **证据**：`artifacts/ai-tasks/evidence/M3-02.yaml`。
- **失败处理**：子代理并发触发 Provider 限流时按既有退避处理并在降级报告记录，不减少断言。

### M3-03 汇总门：reviewer 解析、失败回退与非发现产出

- **结果**：全部分片完成后，由 reviewer 配置的模型执行汇总（去重、统一严重度、补「非发现」、校验 C1 断言形式）；reviewer 不可用时按既有回退语义处理并标注"未经强模型汇总"。
- **需求引用**：F3、C1、C2。
- **依赖**：M3-02。
- **前置事实**：`quality_reviewer`（auto/r_code/codex）解析与回退语义已存在；汇总输入走 5000 字符摘要包络。
- **固定约束**：汇总门为编排必经步骤（与可选的 quality_loop 终局复核独立计费与审计）；产出必须含「非发现」小节；C1 违反形式的探测结论必须被降级或退回。
- **决策空间**：reviewer 默认解析策略（能力目录中最强综合模型的选择规则）；退回重做的次数上限（默认 1 次并记录）。
- **产物**：汇总门执行、reviewer 解析、回退标注、断言形式校验、测试。
- **实施步骤**：
  1. 汇总提示组装（分片报告 + 横切结果 + C1 校验职责 + 非发现要求）。
  2. reviewer 解析与回退；审计 SynthesisGate 事件。
  3. 测试：正常汇总、reviewer 不可用回退、C1 违规退回、非发现存在性。
  4. Harness 登记；CHANGELOG。
- **验收断言**：
  - `M3-03.A1`（integration）：完整编排 fixture 跑通后最终报告含非发现小节与去重后的问题清单。
  - `M3-03.A2`（unit）：含"不可用"级违规断言的分片报告被退回或降级标注。
  - `M3-03.A3`（unit）：reviewer 全不可用时报告带"未经强模型汇总"标注且 run 正常结束。
- **验证**：`node scripts/verify-breadth.mjs --task M3-03 --profile implementation`。
- **证据**：`artifacts/ai-tasks/evidence/M3-03.yaml`。
- **失败处理**：汇总输出结构不稳定时收紧输出 schema 校验，不放宽非发现要求。

### M3-04 编排时间线可视化、设置项与中断降级路径

- **结果**：时间线呈现编排结构（分片 run 分组、汇总门 run）；`breadth_mode` 设置项进"Agent 编排"页；用户打断时已完成分片结果保留。
- **需求引用**：F3、C2、§5.1 设置面。
- **依赖**：M3-02、M3-03。
- **前置事实**：时间线已有子代理详情呈现；设置页编排区块（`SettingsScene.tsx` 委派路由下方）与 `InfoTip` 组件可用。
- **固定约束**：打断走既有中断语义，不新增静默丢弃；设置项文案与 §5.1 一致。
- **决策空间**：分组视觉形态（对齐现有子代理时间线样式）。
- **产物**：前端展示、设置项、中断路径测试、脚本测试。
- **实施步骤**：
  1. 时间线分组渲染（分片/汇总门 run）。
  2. 设置项（三档 select + InfoTip）。
  3. 中断路径：打断后已完成分片保留在时间线。
  4. 测试：`node --test` 前端用例（参照 `app-shell.test.mjs`）；`--through M3` 累计门禁。
- **验收断言**：
  - `M3-04.A1`（e2e）：编排 run 在时间线呈现分组结构（脚本测试）。
  - `M3-04.A2`（e2e）：设置项三档往返与 InfoTip 存在。
  - `M3-04.A3`（unit）：打断后已完成分片结果保留。
  - `M3-04.A4`（regression）：`--through M3 --profile implementation` 退出码 0。
- **验证**：`node scripts/verify-breadth.mjs --task M3-04 --profile implementation`。
- **证据**：`artifacts/ai-tasks/evidence/M3-04.yaml`。
- **失败处理**：UI 时序不稳按轮询模式改造。

### M4-01 Governor 配置外部化

- **结果**：`exploration_threshold_tokens`/`enabled`/`applies_to` 三配置生效（换算 chars = tokens × 4）；只影响新 run；默认值 = 现状。
- **需求引用**：F5、C4、§5.1。
- **依赖**：M0-02。
- **前置事实**：`DEEPSEEK_GOVERNOR_REASONING_CHARS = 6000`（`llm_runtime.rs:245`）；`reasoning_governor_kind` 模型族集合。
- **固定约束**：状态机语义不变；配置变更不热更新进行中 run；默认 1500 token 等价现状。
- **决策空间**：无重大决策（机械外部化）；`applies_to` 匹配大小写规则照 provider_kind 现有惯例。
- **产物**：配置接线、测试。
- **实施步骤**：
  1. 常量改为运行时配置读取（默认值保持）。
  2. 测试：默认等价现状（黄金断言）；自定义阈值生效于新 run；进行中 run 不变。
  3. Harness 登记；CHANGELOG。
- **验收断言**：
  - `M4-01.A1`（unit）：默认配置下 governor 触发行为与现状一致（既有测试不变绿）。
  - `M4-01.A2`（unit）：自定义阈值 + 新 run 生效。
- **验证**：`node scripts/verify-breadth.mjs --task M4-01 --profile implementation`。
- **证据**：`artifacts/ai-tasks/evidence/M4-01.yaml`。
- **失败处理**：无特殊路径；回归失败优先查换算精度。

### M4-02 breadth-eval 评估臂

- **结果**：`eval/breadth-eval/` 具备预注册、语料锁与评分脚本，度量四指标（横切清单覆盖率 / 总 token / 墙钟 / M1 类不实断言数），双臂（baseline vs breadth auto）与 governor 阈值档位可配。
- **需求引用**：F5、§1.1。
- **依赖**：M0-01、M2-01。
- **前置事实**：plan-eval 规范（`preregistration.json`、语料锁、`score.mjs`、`build_isolated_state` 隔离环境）；falib 四形态语料已在 M2-01 沉淀；audit-a 的 12 个 P1 清单可作为预注册命中集来源（脱敏摘录进预注册文件）。
- **固定约束**：预注册先行（阈值在跑批前冻结进 `preregistration.json`）；语料快照锁 SHA-256；评分 fail-closed（缺失指标按失败）。
- **决策空间**：臂的具体组合矩阵；语料是 falib 快照最小化重制还是新构造（按脱敏与最小化原则定，记录决定）。
- **产物**：`eval/breadth-eval/`（预注册、语料、评分脚本、README）、Harness 断言。
- **实施步骤**：
  1. 预注册文件（指标、臂、阈值、命中集清单）。
  2. 语料快照与锁。
  3. 评分脚本 + 结构校验测试（dry-run 模式不依赖真实 Provider）。
  4. Harness 登记（implementation profile 只跑结构与 dry-run 断言）。
- **验收断言**：
  - `M4-02.A1`（contract）：预注册与语料锁存在且校验脚本通过。
  - `M4-02.A2`（unit）：评分脚本 dry-run 模式对样例报告输出正确四指标。
  - `M4-02.A3`（unit）：缺失指标按失败（fail-closed 断言）。
- **验证**：`node scripts/verify-breadth.mjs --task M4-02 --profile implementation`。
- **证据**：`artifacts/ai-tasks/evidence/M4-02.yaml`。
- **失败处理**：语料脱敏争议时收窄命中集，不引入真实凭据。

### M4-03 双臂跑批、实验报告与默认阈值收口

- **结果**：真实双臂跑批产出机器可读报告与实验文档；按结论修订 governor 默认阈值与 breadth 建议默认档；本 PRD 状态收口。
- **需求引用**：F5、§1.3 DoD。
- **依赖**：M3-04、M4-01、M4-02。
- **前置事实**：需要真实 DeepSeek API key（外部条件）；预注册阈值已在 M4-02 冻结。
- **固定约束**：跑批结果不改预注册阈值（改阈值=重新预注册）；报告含能力/profile 标签（真实 Provider 数据不得用 fake 冒充）。
- **决策空间**：报告落点（`docs/breadth-eval-report-<date>.md`）与阈值修订幅度（按数据）。
- **产物**：跑批报告、实验文档、阈值修订、CHANGELOG、本 PRD 状态更新（含固化清单解冻→重冻结）。
- **实施步骤**：
  1. `--profile production` 跑批（key 从环境注入，不入库）。
  2. 报告与结论；阈值修订 PR + 本文档状态更新（走 freeze material change 流程）。
  3. `--through M4 --profile implementation` 终局累计门禁。
- **验收断言**：
  - `M4-03.A1`（reliability，production）：双臂跑批退出码 0 且报告含全部 required 指标。
  - `M4-03.A2`（regression）：breadth auto 臂达到预注册覆盖率阈值。
  - `M4-03.A3`（contract）：实验文档存在且引用真实报告路径。
- **验证**：`node scripts/verify-breadth.mjs --through M4 --profile production`。
- **证据**：`artifacts/ai-tasks/evidence/M4-03.yaml`。
- **失败处理**：覆盖率未达标时如实报告差距并给出归因，不得调阈值口径或删命中集。

## 10. 进度、任务包与证据

- **当前进度**：总任务 13，完成 0，未完成 13。下一执行项：**M0-01**。
- **任务包**：`artifacts/ai-tasks/current.yaml`（从 `artifacts/ai-tasks/templates/current-task.template.yaml` 生成，`project_id: r-code-breadth`）。每个可验证子步后更新；只是单项恢复状态，不是第二份总计划。
- **证据归档**：任务通过后写 `artifacts/ai-tasks/evidence/<task-id>.yaml`（模板 `task-evidence.template.yaml`），验证报告在 `artifacts/ai-tasks/verification/<profile>/`。证据文件随仓库提交（体积小、可复核）。
- **勾选规则**：仅当任务全部 required 断言通过 + Harness `--task` 0 + 累计门禁通过 + 证据真实存在时，改 §8 Checkbox 并重算本节统计。

## 11. Bootstrap 默认值与 AI 决策原则

- **决策阶梯**：查文档与仓库 → 复用既有模式（本仓库几乎所有形态都有先例，§3）→ 仓库内可逆选择按 安全 > 正确 > 简单 > 一致 > 可测试 > 性能 自行决定并记入任务包 `decisions` → 缺外部能力时用 fixture/fake/dry-run 做到 implementation_verified，真实放行留给 production profile。
- **既有默认**（不再询问）：子模块改动走子模块 main + 父仓库指针；Rust 改动后本地 `cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --all`；前端测试走 `node --test`；CHANGELOG 只进 `[Unreleased]`；提交信息按仓库 Conventional Commits 风格。
- **不是中断理由**：里程碑到达、测试失败、lint 失败、可诊断环境问题、缺少真实 Provider（fixture 可验证时）、想汇报进度。
- **允许中断**：需要真实 API key 且无 fixture 替代路径（仅 M4-03 production 档）、需扩大任务范围/权限、规范冲突无法从事实消解且会改变产品语义。

## 12. 风险与外部放行

| 风险 | 缓解 |
| --- | --- |
| 编排提示词劣化单代理体验 | 默认 `off`；`suggest` 先行；M4-03 覆盖率门槛 |
| 子代理并发放大 Provider 限流 | 分片上限 + 既有退避；限流计入降级报告 |
| 协议字段漂移 | serde default 双读；M1-01.A2 兼容断言 |
| 汇总门成本 | 5000 字符摘要包络；汇总 run 独立计费可审计 |
| eval 语料过拟合 | 语料最小化脱敏；预注册阈值保守；改阈值=重新预注册 |

**完成层级**：`implementation_verified` = M0–M4 全部 implementation profile 断言通过（AI 可独立达成，本清单终点）。`production_release_ready` = M4-03 production 跑批（真实 DeepSeek key）+ 用户对 breadth 建议默认档的采纳决定——外部放行状态，缺失时如实标注，不得伪造。
