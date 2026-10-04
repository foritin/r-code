# 增补调研：复杂巨仓与多人协作场景（2026-09-30）

> 本文是[大型项目上下文服务调研](./large-project-context-strategies.md)的增补，聚焦三个新问题：① GitHub 社区沉淀了哪些可借鉴的方案；② 多人/团队开发场景的治理机制；③ 超大型 monorepo 的专项实践。全部结论锚定一手来源。

## 1. 社区方案 Top 5（按对桌面端 AI 编码产品的可借鉴度）

### 1.1 Memory Bank 模式（Cline 发源，28k+ 衍生 stars）

- 机制：`memory-bank/` 六文件分层——projectbrief（需求基线）/ productContext / **activeContext（当前焦点，更新最频）** / systemPatterns / techContext / **progress（进度+已知问题）**；`.clinerules` 驱动"每任务开始强制全读 + 四种时机更新"。
- 衍生：[cursor-memory-bank](https://github.com/vanzan01/cursor-memory-bank)（3.1k★，分层懒加载+复杂度分级）、[memory-bank-mcp](https://github.com/alioshr/memory-bank-mcp)（921★，MCP 化跨客户端）、claude 版等。官方文档：[docs.cline.bot/features/memory-bank](https://docs.cline.bot/features/memory-bank)。
- 优点：纯 Markdown 人机共读、进 Git 即团队共享、跨工具可移植。**短板：手动触发 "update memory bank"、LLM 自写自维护会漂移、progress 说完成 ≠ 真完成（无验证）**。
- 启示：把"会话结束自动摘要更新 activeContext/progress"做成产品功能，并用宿主权威验证补上"progress ≠ 事实"的缺口——正是桌面产品相对纯规则文件的机会。

### 1.2 AGENTS.md 分层 + 就近加载（goose 的完整语义参考）

- [goose](https://github.com/aaif-goose/goose)（54.8k★，已转 Linux 基金会）：`.goosehints` 支持全局/项目根/**嵌套目录**三级加载（monorepo 进 frontend/ 自动加载该层）；默认同时找 AGENTS.md；`CONTEXT_FILE_NAMES` 可配；**`@file.md` 显式内联 vs 裸引用按需读取**的两档引用语义；`analyze` 扩展提供符号级/调用图追踪（focus=函数名+follow_depth），>50k 字符结果建议委托 subagent 消化。
- OpenHands（89.6k★）：microagents 已演进为 Agent Skills 标准（`.agents/skills/`，agentskills.io），frontmatter `triggers` 关键字触发 + `paths` 路径触发注入。

### 1.3 确定性闸门：ast-grep（16.1k★，Rust）+ semgrep

- **ast-grep 可作为 Rust crate 进程内嵌**（对 Tauri 产品是决定性优势）：tree-sitter AST 模式匹配，规则 YAML（pattern 元变量/kind/regex + inside/has/follows 关系 + all/any/not 组合）；`ast-grep scan` 遇 error 级命中 exit 1（天然 CI 闸门）；`--json`/`--format github|sarif` 机器输出；规则可单测（`ast-grep test`）。文档：[ast-grep.github.io](https://ast-grep.github.io/guide/rule-config.html)。
- [semgrep/skills](https://github.com/semgrep/skills)（官方，`npx skills add semgrep/skills`）：把 CI 规则转成 Agent Skill，让护栏在**生成阶段**生效而非只在 CI 兜底。
- 启示：**不变量从提示词文字升级为可执行 AST 规则**——"禁止直接 import 某模块""pub fn 必须有 doc"这类纪律用 ast-grep 表达，Plan 闸门和 /doctor 都消费同一份规则。

### 1.4 Spec-driven 三检查点（spec-kit 139.5k★；Backlog.md 6.9k★）

- [Backlog.md](https://github.com/MrLesk/Backlog.md) 的三句话内核：**审 spec → 审 agent 写入的实施 plan → 审代码；一任务 = 一上下文窗口 = 一 PR**——把"大项目上下文问题"转化为"流程问题"，diff 永远人类可读。
- [spec-kit](https://github.com/github/spec-kit)：constitution→specify→plan→tasks→implement→converge；社区生态以 preset/模板形态存在（[awesome-spec-driven-development](https://github.com/Engineering4AI/awesome-spec-driven-development)，281★）。

### 1.5 Worktree 并行编排（vibe-kanban 28.2k★ 已 sunset / claude-squad 8.6k★ / crystal 3.1k★）

- 三项目独立验证同一模式：**每任务一 worktree 一分支一终端 + UI 内 diff 行内评论反馈 agent 迭代**。
- vibe-kanban 的 sunset 是商业失败而非模式失败；教训：**纯并行不是价值，人审环节的效率才是**。
- Claude Code 官方 worktree：`claude --worktree`（可从 PR 建）、四类检查防逃逸（阻止编辑主 checkout、阻止 `git -C`/`GIT_DIR` 逃逸）、`git worktree lock` 防并发清理；与 agent teams 的分工是"worktree 管文件隔离，mailbox 管通信"。

## 2. 多人协作的治理机制（官方一手）

| 机制 | 代表实现 | 要点 |
| --- | --- | --- |
| 规则进 Git = 天然共享 | 所有工具的项目级规则 | 随 clone 分发，无需中心服务；组织级政策才走带外通道 |
| 组织层强制不可覆盖 | Claude managed-settings（"Nothing you set overrides them"，系统目录/MDM/server 三通道）；Cursor Team Rules enforced；Devin `/etc/devin/rules`（IT 下发，用户不可删）；GitHub org instructions（仅 org owner） | 策略来源分层 + 高层锁死低层敏感键 |
| agent PR 永远人审 | GitHub：agent 不能自我 approve/merge，CI 运行需人放行（防提示注入）；CodeRabbit 自动 approve 默认关、`auto_pause_after_reviewed_commits` 防 agent 刷 review | 唯一大规模例外（OpenAI 内网）靠全套机械强制替代人审，作者自认不可外推 |
| 路径作用域 × CODEOWNERS | Cursor Team Rules glob、`.mdc` globs、CodeRabbit path instructions + CODEOWNERS 决定谁审 | "规则在哪、人就在哪"的责任映射 |
| 预算进 CI | Devin 注入上限 16KiB/文件；Windsurf 12K 字符/文件；OpenAI ~100 行 AGENTS.md；社区 Token Guard（GitHub Action 数 token 超限即红） | 指令是上下文成本，预算要机械执行 |
| 知识分层 | 目录（always-on）→ 按需检索（trigger）→ 组织层共享（Devin Knowledge org scope） | always-on 部分越来越小 |
| 并行靠物理隔离 | worktree / 云 VM（Cursor Cloud Agents、Codex cloud）/ 受保护分支命名空间（`copilot/*`） | 多 agent 共享状态用朴素文件（mailbox JSON + 任务表 + 认领文件锁）而非数据库 |
| 合并端收窄 | 小 PR、trunk-based、按文件集分工 | 官方告诫："Two teammates editing the same file leads to overwrites" |
| OpenAI harness engineering 范式 | AGENTS.md ≈100 行目录 + 结构化 docs/ 为 system of record + CI linter 校验知识库新鲜度 + **doc-gardening agent 定期扫描过时文档自动开修复 PR** | "From the agent's point of view, anything it can't access in-context effectively doesn't exist"——知识必须入库 |
| Shopify Under the River | 单 monorepo + Nix everywhere + 仓库即知识层（skills/runbooks/AGENTS.md 入库随评审演进） | 月 6 万会话、3,536 个 River 共同署名 PR（每 8 个合并 PR 有 1 个）；Session/Harness/Sandbox 三分离 |

## 3. 巨仓（monorepo）专项

### 3.1 嵌套 AGENTS.md 实例（GitHub API 实测）

**纠错：上一轮调研引用的"openai/codex 内部 88 个嵌套 AGENTS.md"是谣传**（Trees API 实测 main 分支仅 2 个：根 + codex-rs/tui/src/bottom_pane/；"88"出自第三方博客转述，无可验证出处）。真实范本：

| 仓库 | 嵌套数 | 分工模式 |
| --- | --- | --- |
| [apache/airflow](https://github.com/apache/airflow/blob/main/AGENTS.md) | 16 | 根管环境引导+`<!-- START generated-commands -->` 自动生成命令块+仓库地图+安全模型；providers/ 管领域铁律（依赖上限、安全禁令）；另有 `.agents/skills/` |
| [getsentry/sentry](https://github.com/getsentry/sentry/blob/master/src/AGENTS.md) | 6 | **根管命令（Command Execution Guide）、子目录管领域知识**（IDOR 模式、N+1 反模式、AI 快速决策树），子文件显式回指根文件 |
| [google/perfetto](https://github.com/google/perfetto/blob/master/docs/AGENTS.md) | 非标准命名 | 路由声明式："若做 UI 改动，看 AGENTS-ui.md 并停止阅读本文件其余部分"；NEVER 手改生成的 BUILD 文件 |

### 3.2 Monorepo 工具 × AI

- **Turborepo 官方 AI 章节**：检测到 agent 时在根 AGENTS.md 维护 **managed block**，指向随安装版本打包、离线可用的本地文档（防 agent 用过时训练数据）；`npx skills add vercel/turborepo`；**worktree 间共享本地缓存**（并行 agent 互相吃到构建命中）。
- **Nx**："We're designing every API twice now: once for developers and once for AI agents"；`configure-ai-agents` 校验各家配置；Self-Healing CI（失败任务→自动修复→推回 PR）。

### 3.3 环境可复现与 CI

- Copilot coding agent 官方支持 `.devcontainer` + `copilot-setup-steps.yml`（agent 启动前装依赖——官方明说 agent 自己试错装依赖 "can be slow and unreliable"）；Codex 沙箱三档 + **按命令前缀的细粒度 Rules**；Gemini CLI 五种沙箱后端 + Sandbox Expansion（权限被拒时弹窗按次扩权）。
- spec-kit constitution 的 CLI 优先原则："Every library exposes functionality via CLI; text in/out; support JSON + human-readable formats"——一切能力可被 agent 与 CI 验证。
- 增量验证：airflow `breeze ci selective-check --commit-ref`（changed files → 测试子集）写进 AGENTS.md，等于把 affected 检测做成 agent 可调用的一行命令。

## 4. 对 r-code 的优化点映射（已并入 [project-context plan](../support/archive/prd/project-context/prd.md)）

1. 不变量引擎具体化为 **ast-grep 进程内嵌**（Rust crate）；
2. /init 生成段采用 **managed block 标记**（airflow/Turborepo 模式）+ 命令版本对齐；
3. Plan 闭环对齐 **Backlog 三检查点**，新增可选"一事项=一PR"；
4. 新增 **memory-bank 式团队共享工件**（opt-in 进 Git，宿主验证补齐"progress ≠ 事实"缺口）；
5. 新增 **L5 多人协作层**（团队工件/CODEOWNERS 联动/预算进 CI/worktree 并行会话）。
