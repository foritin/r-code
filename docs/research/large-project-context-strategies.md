# 大型项目上下文服务调研：业界如何让 AI 编码代理驾驭大型代码库

> 调研日期：2026-09-30。方法：5 路并行调研（商业工具方案 / 开源索引实现 / 检索范式之争 / 规则与标准生态 / 上下文工程），全部结论尽量锚定一手来源（官方文档、工程博客、GitHub 源码），来源 URL 附于文末。个别无法核验的信息点已在正文明确标注。
>
> 调研动机：决定 R-Code 如何服务大型项目编程。结论与建议见第 7 节。

---

## 0. 一句话回答调研问题

**"大型项目管理一般使用什么方式？建文档索引吗？"**

1. **建"文档"，而且正在标准化**：几乎所有主流工具（Codex、Claude Code、Cursor、Copilot、Gemini CLI、Windsurf、Amp、Zed、Jules…）都收敛到 **AGENTS.md / CLAUDE.md 分层规则文件**——这是全行业标配的"文档索引"，采用量 6 万+ 开源项目，已交由 Linux Foundation 旗下基金会托管。它的正确写法是 **map-not-manual**（目录 + 指针，不是百科全书），并按"全局 → 仓库根 → 子目录"分层、就近覆盖、按需（JIT/路径触发）加载。
2. **机器索引（向量/embeddings）不是共识，甚至在退潮**：Cursor 已转向本地 trigram 索引（Instant Grep）并声明"不为搜索存储 embeddings"；Sourcegraph 企业版 2024 年就弃用了 embeddings 改用 BM25；Claude Code / Codex / Gemini CLI / Amp 全部是 grep/glob/读文件驱动的 agentic search，零预建索引。
3. **真正分层的答案是三件事叠加**：① 规则/文档文件（人写给 agent 的）+ ② 检索（agent 自己 grep，或轻量本地索引：trigram/BM25/tree-sitter）+ ③ 上下文工程（compaction、plan 文件持久化、记忆、subagent 隔离探索）。长任务的可靠性来自"把状态外部化成文件"，不来自更大的索引。

---

## 1. 业界全景：五种范式与代表产品

| 范式 | 代表 | 一句话 |
| --- | --- | --- |
| (a) 文档/规则驱动 | Codex、Claude Code、Amp、Zed、OpenCode | AGENTS.md/CLAUDE.md 分层注入，模型自己探索 |
| (b) Agentic search（零索引） | Claude Code、Codex CLI、Gemini CLI、Cline | Grep/Glob/Read 工具循环，永远新鲜、零维护 |
| (c) 预建语义索引（embeddings） | Cursor（旧）、Windsurf、Continue（可选）、Tabby | 向量检索，语义召回强但会过期、有隐私/成本问题 |
| (d) 轻量本地索引（trigram/BM25） | Cursor 2.1+（Instant Grep）、Sourcegraph（zoekt/BM25）、Continue（FTS5） | 本地全文/倒排索引，"跑赢 ripgrep"，不上传代码 |
| (e) 精确符号索引（AST/LSP/SCIP） | Sourcegraph Cody（SCIP graph）、GitHub code navigation（tree-sitter tags）、Claude Code LSP 插件 | 定义/引用级导航，工程重但精确 |
| 隐性范式：文档即索引（wiki） | DeepWiki/Devin Wiki、Mutable.ai（已下线） | 自动生成整仓知识库，"Deep Research for GitHub" |

### 主流工具横向对比（2026-09 快照）

| 工具 | 方案类型 | 索引 | 规则/指令文件 | 记忆 / 规划机制 |
| --- | --- | --- | --- | --- |
| Claude Code | agentic search（官方反对预建索引） | 无（可选 LSP 插件） | CLAUDE.md 多层 + `@import` + 原生兼容 AGENTS.md + `.claude/rules/` 路径作用域 | auto memory（MEMORY.md 索引）、`/compact`（先清工具输出）、subagents、skills、plan mode |
| Cursor | 本地 trigram 索引（2.1 Instant Grep）；旧版为服务端 embeddings + Merkle 同步 + 团队索引复用 | 本地（声明不存 embeddings、不上传代码） | `.cursor/rules/*.mdc` 四触发 + AGENTS.md（纯 Markdown 场景官方推荐 AGENTS.md） | Explore subagent、plan mode、@ mentions |
| GitHub Copilot | 混合 | 本地工作区索引 + GitHub/Azure DevOps 远端索引（#codebase），无索引回退 grep/usages | `.github/copilot-instructions.md` + `.github/instructions/*.md`（applyTo）+ 兼容 AGENTS.md/CLAUDE.md | 内置 plan agent（VS Code 1.105）、coding agent 计划先行、Copilot Memory（preview） |
| OpenAI Codex | 文档驱动 + 沙箱内 agentic 检索 | 无 | AGENTS.md 三层（全局→仓库根→子目录逐层拼接，默认 32KiB 上限，override + fallback 文件名） | 服务端加密 compaction（encrypted_content 保留隐态）、multi-agent（experimental）、长跑四文件栈 |
| Gemini CLI / Jules | 文档驱动 + 检索 | 无 | GEMINI.md 三层 + **JIT 加载**（工具触达某目录时注入该目录文件）；文件名可配成 AGENTS.md | Auto Memory（历史会话挖掘→审批收件箱）、subagents、graph 化压缩管线（阈值 0.5） |
| Windsurf（Devin Desktop） | 预建索引 | 本地全库 RAG（M-Query）+ 企业远端 embeddings（代码即删） | `.devin/rules/`/`.windsurf/rules/`（trigger 四档）+ AGENTS.md 同引擎 | Cascade memories（legacy，迁移向 Skills） |
| Sourcegraph Cody → Amp | 混合 | zoekt（trigram）+ BM25 + SCIP code graph；**企业版已弃用 embeddings** | Amp：AGENTS.md cwd+父目录恒载 + 子树按需 | Code Finder 检索子代理、Librarian、threads |
| Devin / Cognition | 文档/知识驱动 | DeepWiki 整仓 wiki（已索引 5 万+ 公开仓库） | 仓库 AGENTS.md | Knowledge（按 trigger 按需检索 + 自动建议沉淀，迁移中→Skills） |
| aider | repo map（tree-sitter + PageRank） | 本地符号图（mtime 增量缓存） | `.aider.conf.yml` 可读 AGENTS.md | 聊天内文件即"工作集" |
| Continue.dev | 混合 + rerank | 本地四索引：AST chunk + 函数片段 + SQLite FTS5(trigram, BM25) + LanceDB 向量（可选本地 Ollama） | config instructions + 兼容多家规则 | @codebase / @repo-map provider；也内置 agent 工具集 |
| Tabby | 本地全文 + 符号 | tantivy（Rust Lucene）+ tree-sitter tags，AST 感知切块（512 字符） | — | code browser / 问答 RAG |

**关键动向（时间线）**：

- 2024-02：Sourcegraph Cody 企业版弃用 embeddings → BM25（三条理由：代码出域、运维负担、万仓级成本失控）。
- 2025-04：Anthropic《Claude Code best practices》确立 agentic coding 范式（CLAUDE.md + 工具循环）。
- 2025-05：Cline 发文《Why Cline Doesn't Index Your Codebase》；Cognition 发布 DeepWiki。
- 2025-08/11：GitHub Copilot coding agent 支持（嵌套 + 组织级）AGENTS.md。
- 2025-09：Anthropic《Effective context engineering》提出 just-in-time context + compaction/memory/subagent 三策略。
- 2025-11：Cursor 2.1 发布 Instant Grep（本地索引、声明不存 embeddings）——embeddings 索引的代名词转向 grep 系。
- 2026：Sourcegraph 发布 CodeScaleBench 量化"检索增益的规模门槛"；Cursor 文档明示检索路径不涉 embeddings；Claude Code 把 grep/glob 工具并入原生 bash（进一步 primitive 化）。

---

## 2. 争论与证据：grep vs 索引 vs 混合

### 2.1 双方立场（原话）

**反索引方**：

- Cline："Imagine trying to understand a symphony by listening to random 10-second clips."（切块撕裂结构）"Every merge is a potential divergence between reality and your AI's understanding."（索引过期）口号："No RAG. No embeddings. No vector databases."
- Anthropic（context engineering）："agents built with the 'just in time' approach maintain lightweight identifiers (file paths, stored queries, web links)"——持引用、运行时加载；同时诚实承认 "runtime exploration is slower than retrieving pre-computed data"（推荐部分预取 + 按需探索的混合）。
- Claude Code 团队（2026-07 炉边访谈）："we removed our grep and other search tools — glob tools — in favor of native bash"。

**索引方**：

- LlamaIndex（2026-05）：grep 三轴退化——延迟（百万文件 >4s vs ANN 几十 ms）、词汇失配（搜 "revenue recognition" 找不到 "ASC 606"）、信噪比；"You can't grep a PDF"。
- Sourcegraph（2026-05，CodeScaleBench，1,281 次 run / 40+ 仓库 / 9 语言）：本地 grep/read/glob **在约 40 万行以上系统性失效**；五种 agent 失败模式（迷路读 import 链烧完超时、wrong file wrong symbol、部分完成、工具抖动、上下文溢出）全部与检索相关。名句："The difference between complete failure and near-perfect completion wasn't intelligence, it was efficient access to context."
- Cursor（2026）：Instant Grep 是 "a custom search engine that outperforms ripgrep on large codebases"。

### 2.2 带数据的结论汇总

| 数据来源 | 对比 | 结果 | 注意事项 |
| --- | --- | --- | --- |
| Sourcegraph CodeScaleBench（2026-05） | 本地 grep vs 混合检索 MCP | 检索增益随规模分层：**<400K LOC 加工具 −0.080；400K–2M LOC +0.259**；MCP 版省 30% 成本快 38% | 厂商自建基准 |
| Sourcegraph（2026-06） | 便宜模型+检索 MCP vs 贵模型+全库直读 | 0.698 分/$1.02 每质量点 vs 0.568/$1.83——**长上下文 ≠ 好检索** | 仅 9 任务 |
| arXiv 2605.15184《Is Grep All You Need?》（2026-05） | grep vs 向量检索（跨 Claude Code/Codex/Gemini CLI harness） | "grep generally yields higher accuracy than vector retrieval"，但结论随 harness 剧烈变化 | LongMemEval 非代码域，社区公认外推有限 |
| Ory Lumen（2026-03） | Claude Code 基线 vs +本地语义搜索工具 | 成本 −26%、时间 −28%、质量持平 | 自测 |
| Zilliz claude-context（2025-08） | 同上 | token −40% | 自测 |
| Semble（2026-05，HN 445 分） | 静态向量+BM25（RRF） vs grep+read | 检索质量达 transformer 方案 99%，CPU 索引 250ms/查询 1.5ms；"省 98% token" 未经独立验证、社区复测毁誉参半 | 端到端评测缺失 |

**诚实的结论**：目前不存在中立、可复现、端到端的大规模对比；但**趋势性证据**（Sourcegraph、Cursor、GitHub、aider 各自独立收敛到"轻结构 + 文本排序"）比任何单点数字可信。

### 2.3 按规模选型（证据加权后的共识）

| 场景 | 推荐范式 |
| --- | --- |
| <10 万行，个人/小团队 | 纯 agentic search + AGENTS.md/CLAUDE.md 轻量文档索引（加索引在此区间为负收益） |
| 10 万–40 万行，单仓库 | agentic search 为主 + 按需补符号导航（LSP/go-to-definition） |
| 40 万–200 万行 monorepo | 混合：即时 grep + 符号索引 + 可选本地语义检索 + 检索子代理（增益最大区间 +0.259） |
| 多仓库 / 跨 org / 万仓级 | 集中式代码搜索平台（keyword+semantic+find-references）经 MCP 供给 agent |
| 概念性查询（"哪里处理权限？"）、非文本资产 | 语义检索不可替代 |
| 陌生大库 onboarding（人用） | DeepWiki 式"文档即索引" |

三条工程原则：

1. **不要为小库建索引**——维护、隐私、成本三输（2024–2026 被反复验证的第一教训）。
2. **检索好 ≠ 任务完成好**（Sourcegraph 自己的方法论忠告）——上线检索增强前用任务级评测把关。
3. **harness 与模型的适配比检索算法本身更影响结果**——换一个 harness，grep 与向量的胜负可以翻转；先测自己的模型+工具组合。

---

## 3. 规则/文档标准：AGENTS.md 生态（"建文档索引"的正解）

### 3.1 AGENTS.md 是什么

- 开放、工具无关的 Markdown 格式，"给 agent 看的 README"，无 schema；由 OpenAI、Google、Cursor、Factory 等协作发起，现由 **Linux Foundation 旗下基金会托管**；官网口径 **6 万+ 开源项目**采用（被学术论文独立引用）。
- 官网 adopters（2026-09）：Codex、Jules 与 Gemini CLI、Factory、Aider、goose、opencode、Zed、Warp、VS Code、Devin、Junie（JetBrains）、Amp、Cursor、RooCode、Windsurf、GitHub Copilot coding agent、Augment Code 等。
- GitHub 官方已整合：2025-08-28 changelog 支持（根 + 嵌套）AGENTS.md；2025-11-05 支持组织级自定义指令。

### 3.2 加载层级（以最完整的 Codex 实现为准）

1. 全局层：`~/.codex/AGENTS.md`（`AGENTS.override.md` 优先；`CODEX_HOME` 可改）。
2. 项目层：从项目根（通常 git root）逐目录走到 cwd，每目录至多取一个文件（override → AGENTS.md → 可配置的 fallback 名单，可含 `CLAUDE.md`）。
3. 合并：**从根到叶拼接，越靠近当前目录的越靠后 = 越优先**；总量上限默认 **32 KiB**（`project_doc_max_bytes`）。
4. 冲突原则："离被编辑文件最近的 AGENTS.md 胜出；用户聊天中的显式指令覆盖一切。"

各家同构实现：Claude Code（managed → user → project → `CLAUDE.local.md` → 子目录 JIT → `.claude/rules/` 路径作用域；`@import` 递归 4 跳；v2.1.277+ 原生读 AGENTS.md）；Gemini CLI（三层 + **JIT：工具访问某文件时自动注入该目录及祖先的 GEMINI.md**）；Amp（cwd+父目录恒载 + 子树按需 + 系统级 `/etc/ampcode/AGENTS.md`）；Cursor/Windsurf/VS Code 各自兼容。

### 3.3 触发方式收敛为四档

1. **always-on**（恒载）
2. **模型按描述自取**（Cursor "Agent Requested" / Windsurf `model_decision` / Devin Knowledge trigger / Claude skills 的 description 路由）
3. **glob 路径触发**（Cursor `globs` / Windsurf `trigger: glob` / Claude `.claude/rules/` frontmatter paths / Copilot `applyTo`）
4. **手动 @ 引用**

### 3.4 大项目的两种成熟文风（实地考察）

| 仓库 | 形态 | 要点 |
| --- | --- | --- |
| openai/codex | 工程规范型 | just 命令体系、模块 500/800 LoC 上限、单次变更 ≤800 行、"模型可见上下文注入项 <10K token、>1K 标 P0" |
| vercel/next.js | 最完整生态 | AGENTS.md + `.agents/skills/` 6 个内嵌 skill（深层知识从主文件拆进 skills）+ PR 标记 + 任务分解守则 |
| rust-lang/rust | 治理合规型 | AGENTS.md 管 LLM 使用政策（禁写 PR 描述、soundness 区禁改、测试先行），工程知识路由到 CONTRIBUTING/dev-guide；`CLAUDE.md` 仅一行 `@AGENTS.md` |
| facebook/react | 导航卡 + 知识库 | 根 10 行导航卡，compiler/CLAUDE.md 300+ 行知识库 |
| microsoft/vscode | 桥接型 | AGENTS.md 4 行 → 主体在 copilot-instructions.md |
| tauri-apps/tauri | 纯 docs-as-context | 无规则文件，靠 ARCHITECTURE.md + 每 crate README |

共性：**只写"从代码里读不出来的东西"**，其余路由到人类文档；把大文件拆成"目录 + 指针"（OpenAI 内部团队结论："大而全的 AGENTS.md 必然失败"，AGENTS.md ≈ 100 行目录，真知识放结构化 docs/ 并用 CI/doc-gardening agent 保新鲜）。

### 3.5 记忆系统（agent 写给自己的）

- Claude Code auto memory：`~/.claude/projects/<project>/memory/`，MEMORY.md 索引（启动只载前 200 行/25KB）+ 主题文件按需读；frontmatter 分 user/feedback/project/reference。
- Gemini CLI Auto Memory：后台挖历史会话 → memory patch + SKILL.md 草案 → **人工审批收件箱**才生效（"记忆提炼管线 + 人工把关"是共识方向）。
- Windsurf memories：本地私有，官方建议"可共享知识写 Rule/AGENTS.md 而非 memories"。
- 外挂：mem0/OpenMemory（MCP 跨工具共享）、Letta/MemGPT（有状态 agent 运行时）。
- R-Code 对照：R-Code 的"演进记忆"（默认关、AppData 存储、审批+快照注入）与 Gemini 的审批式管线方向一致，是对的。

### 3.6 规划工件（把大需求变成可执行任务）

- **Plan 文件持久化**：Claude Code 官方明确"plan 文件在 compaction 后会被重注入，比对话历史更持久"；判据："If you could describe the diff in one sentence, skip the plan"。
- **spec-kit（GitHub 官方）**：constitution → specify → plan → tasks → implement → converge，产物（spec/plan/tasks.md）入库，`[NEEDS CLARIFICATION]` 强制标记防瞎猜，"测试先行非谈判"。
- **Kiro（AWS）**：requirements.md（EARS 风格）/ design.md / tasks.md 三件套 + 审批门 + 任务依赖图按 waves 并行。
- **OpenSpec**：轻量 brownfield 友好（explore → propose → apply → archive，Delta 格式规格）。
- **Task Master**：PRD → 依赖任务树 → next/expand 逐个执行，MCP 接多家工具。
- **OpenAI 25 小时长跑实验**：durable project memory 四文件栈（prompt.md/plan.md/implement.md/documentation.md）——"最重要的技术是 durable project memory，防 drift、稳定 done 的定义"。
- **Anthropic harnesses 文章**：Initializer + Coding 两段式，交接靠 git 历史 + progress 文件 + `feature_list.json`（**用 JSON 而非 Markdown，因为模型更不容易擅自改写 JSON**），每 session 开场"get bearings"。

---

## 4. 上下文工程与长任务（2025–2026 的核心共识）

公式（所有官方实践收敛到同一处）：

> **长任务可靠性 = 外部化状态（plan/tasks/progress/spec 文件） + 确定性压缩优先（tool-result clearing） + LLM 摘要兜底（compaction） + 隔离的探索/验证窗口（subagent） + 可机械执行的验收（tests/lint/"done when"）**

机制清单（解决什么 → 怎么做）：

1. **Compaction**：历史逼近窗口时 LLM 摘要重启。Claude Code 保留架构决策/未解 bug/实现细节，丢冗余工具输出，重启后重注入最近 5 个文件 + CLAUDE.md + memory + skills（每 skill 5K/合计 25K 预算）。Codex 演化到服务端 in-stream compaction（`encrypted_content` 保留隐态）。Gemini CLI 阈值 0.5（用量过半即压）。
2. **Microcompact / tool-result clearing**：确定性、零模型成本——历史里只保留最近 N 条工具结果，其余换占位文本；配合 cache 友好设计（cache_edits 定点删除，缓存前缀完整保留）。Claude Code 工具响应默认上限 25,000 tokens。
3. **Subagent**：探索/验证类海量 tool output 不进主窗口；子代理消耗数万 token 只回传 1,000–2,000 token 摘要（Anthropic："the essence of search is compression"）。Claude Code 内置 Explore/Plan 角色（**跳过 CLAUDE.md 保持轻量**），并发默认 20、嵌套 3 层；Codex 角色 default/worker/explorer/monitor + `spawn_agents_on_csv` 批量 fan-out。
4. **Skills 渐进披露**：metadata 常驻（预算 ≈ 上下文 1%）→ body 按需 → 资源文件引用。description 要写成路由逻辑并**附 negative examples**（Glean 数据：不加反向例触发率掉 20%）。
5. **Hooks**：`PreToolUse`（过滤/改写参数）、`PostToolUse`（`updatedToolOutput` 替换工具结果——输出整形点；过滤测试输出"数万 token 压到数百"）、`PreCompact`（压缩前抢救细节到文件）、`SessionStart(additionalContext)`。
6. **TODO/task 工具**：单 in_progress 契约、跨压缩存活；agent teams 用共享任务表 + 文件锁。
7. **工具设计**：consolidated tools（做 `search_logs` 不做 `read_logs`，把计算移回工具内部）；响应带可链式调用的 ID；error 要 actionable；工具基数保持很低。MCP 场景把工具生成为文件树模块（progressive disclosure，工具发现 token 150K→2K，省 98.7%）。
8. **多代理 vs 单代理+压缩**：多代理并行研究任务 +90.2%（Anthropic 内部 eval），但 token 15×，且"多数 coding 任务强依赖共享上下文，不适合多代理"；Cursor 数百 agent 实验最终收敛为**递归 planner 层级 + 互不通信 workers + handoff 报告**（扁平自协调和集中 integrator 都失败）。对编码场景，官方更一致的方向是**把工程纪律编码进 harness**。

学术底座：Context Rot（所有模型随输入长度退化，结构连贯的 haystack 比打乱的更伤性能）、NoLiMa（32K token 时 13 个模型中 11 个跌破短上下文基线 50%——长上下文不能可靠替代检索）、Lost in the Middle（U 形曲线，关键信息放两端）、DeepSeek-v3 超 30 个工具开始混淆、METR 任务时间范围每 ~7 个月翻倍。

---

## 5. 开源实现技术细节（可"抄作业"清单）

### 5.1 aider repo map（tree-sitter + PageRank，已读源码）

- 符号提取：tree-sitter 跑每语言 `tags.scm`（只取 def/ref 两类捕获；缺 ref 查询的语言用 Pygments 兜底）。
- 建图：`文件A 引用 文件B 定义的标识符` 为边；边权规则——用户提及的标识符 ×10、复合命名（≥8 字符）×10、下划线私有 ×0.1、>5 文件定义的常见词 ×0.1、**聊天中正在编辑的文件 ×50**、引用次数开平方压缩。
- `networkx.pagerank(G, personalization={当前编辑文件})`——个人化 PageRank 让"工作集"偏置随机游走。
- 按文件 PageRank 值把得分摊到 (文件, 符号) 对，**token 预算二分**（默认 map 1,024 token；无聊天文件时放大到 8,192），渲染成"def 行 + AST 父作用域头"的骨架文本。
- 增量：SQLite/diskcache 按 mtime 缓存 tags（解析最贵，mtime 未变直接跳过）；PageRank 每次重算（万级边毫秒级）。

### 5.2 轻量本地索引三件套（Rust 生态现成）

- **tree-sitter tags**：`tree_sitter_tags::TagsContext::generate_tags()`（crates/tags），每语言 tags.scm + locals.scm，输出自带 docs 区间与 syntax_type；GitHub code navigation 对 19 种语言就这么做；**Tabby 的 `tabby-index` crate 是现成 Rust 参考实现**。
- **全文**：tantivy（Tabby 同款，Rust 版 Lucene）或 SQLite FTS5（trigram tokenizer + BM25；Continue 同款，路径权重 ×10、bm25 阈值 −2.5）。
- **切块**：AST 感知切块显著优于固定窗口——Continue 的"smart collapsed chunks"（类折叠方法体、签名+折叠体、折叠版与完整版双索引）；Tabby 用 text-splitter crate 的 `CodeSplitter`（512 字符）。
- **向量（可选后置）**：LanceDB 有官方 Rust crate，embed 指向本地 Ollama 或任意 OpenAI 兼容端点。

### 5.3 精确索引

- **SCIP 协议**（Sourcegraph）：protobuf 序列化的定义/引用图（Symbol 描述符链 + Occurrence + Relationship）；生产者 scip-java/-typescript/-clang/-python…；**rust-analyzer 内置 `rust-analyzer scip` 命令**——Rust 项目精确索引零成本可得。
- **stack-graphs（GitHub）已停更**、姊妹仓库已删除——"学术级精确解析"路线在生产上输给了"tags 查询 + 搜索排序"的工程化路线。
- **LSP**：Claude Code 官方大仓库指南明确"用 LSP 插件做 jump-to-definition，instead of scanning the tree"。

### 5.4 混合检索管线（Continue.dev 参考）

四索引（chunk/函数片段/FTS5/LanceDB）统一接口；检索 = 最近编辑文件 1/4 + FTS 1/4 + 向量 1/2 + LLM 从 repo map 选文件，去重合并；带 reranker 时四路各取 2×n → rerank → embedding 扩展。同一代码库内也提供 glob/grep/readFile/viewRepoMap 的 agent 工具集——两种范式正在收敛。

### 5.5 "零索引" agent 工具范式（Claude Code 三段式）

- **Glob**（`**/*.ts`，mtime 排序，上限 100）→ **Grep**（基于 ripgrep；默认 `files_with_matches` 先定位文件再读；遵守 .gitignore）→ **Read**（行号 + 分页 + PARTIAL 提示控 token）。
- Codex 更极端：只给 shell，模型自己跑 `rg/find`。
- 为什么可行：新鲜度 100%、零构建成本、ripgrep 对 GB 级仓库百 ms 级、无状态可审计。代价：单查精度低、token 贵——所以大库要叠加 d/e 范式。

---

## 6. 明确"不要做"的事（行业教训）

1. **不要为小库预建索引**（<10 万行负收益；Sourcegraph/Cursor/Cline 三方独立验证）。
2. **不要把代码上传服务端建索引**——与 R-Code 本地优先定位直接冲突；Cursor 团队索引复用虽优雅但隐私争议大（embedding 可被逆转的学术风险），Cursor 自己 2026 年也转向本地。
3. **不要做"大而全"的项目指令文件**——OpenAI 内部教训：必然失败；AGENTS.md ≈ 100 行目录 + 指针。
4. **不要上来就 multi-agent 编排**——官方一致口径：多数 coding 任务强依赖共享上下文，多代理 15× token；先单代理 + 强 harness。
5. **不要做无审批的自动记忆**——方向是"提炼管线 + 人工收件箱"（Gemini）或"审批 + 快照"（R-Code 现状）。
6. **stack-graphs 式重型精确解析不要碰**（已停更）；SCIP 只对 Rust 白捡（rust-analyzer），不为其自建 per-language 管线。

---

## 7. 对 R-Code 的建议

R-Code 现状（与业界对照）：已有 Plan mode（durable goals + 结构化确认 + 待办投影）、演化记忆（默认关 + 审批 + 快照注入）、只读/审批/全权三态子代理委派、MCP 管理、JSONL+SQLite 会话持久化、审计边界。**缺的是"大仓库导航层"与"上下文工程层"的显式产品化。**

按成本×收益分三档：

### A 档：低成本高收益，建议优先做

1. **AGENTS.md 原生兼容（读侧）**：按 Codex 语义实现三层加载（全局 → 仓库根 → 子目录，根到叶拼接、就近优先、总量上限 32KiB、支持 override 与 fallback 名单含 CLAUDE.md）。这是生态入口，成本最低、杠杆最大。已有 AGENTS.md（本仓库自用）说明团队已熟悉该格式，产品化只是读取引擎。
2. **grep/glob/read 三段式工具打磨**：对齐 Claude Code 工具语义（Glob mtime 排序上限、Grep 默认 files_with_matches + 遵守 .gitignore、Read 行号分页）。R-Code 已有 Workspace search 工具，按此契约收紧即可。
3. **Tool-result 截断 + microcompact**：每工具响应硬上限；历史只保留最近 N 条工具结果（确定性清理，零模型成本）；再加 auto-compact（阈值触发 LLM 摘要，重注入 plan/记忆/最近文件）。JSONL 源正好支撑"定点删除旧工具结果"的投影。
4. **Plan 文件在压缩后重注入**：官方明说 plan 文件比对话历史更持久——R-Code 已有 Plan 持久化，补上"compaction 后把进行中 plan + todo 重新注入"即闭环。
5. **TODO/task 工具契约**：单 in_progress、跨压缩存活；R-Code 的 feature-oriented todos 已有产品形态，补 agent 侧工具即可。

### B 档：中成本，差异化竞争力（"大项目"定位的核心）

6. **tree-sitter tags + PageRank repo map**：Rust 生态最顺（`tree_sitter_tags` + petgraph；Tabby `tabby-index` 是现成参考），mtime 增量缓存，1K token 起步的仓库骨架图注入会话开头——aider 已验证该形态对小中库够用且零网络零模型依赖。可复用已有的 `.agents/skills` 机制按需触发。
7. **本地 BM25 全文索引（tantivy 或 SQLite FTS5 trigram）**：作为 agent 可选工具（`search_codebase`），不替代 grep——索引管"概览与召回"，grep 管"新鲜度兜底与精确验证"。40 万行以上项目增益最大（CodeScaleBench）。
8. **LSP/符号导航**：Rust 项目先白捡 `rust-analyzer`（跳转定义/找引用，甚至 SCIP）；对外提供 go-to-definition 工具，"instead of scanning the tree"。
9. **Explore 只读子代理强化**：R-Code 已有 read-only subagents——对齐 Claude Code 实践（跳过项目记忆文件保持轻量、very thorough 分档、只回摘要），把它定位成"检索子代理"（Sourcegraph Code Finder 证明专职搜索子服务快 2 倍省 40%）。
10. **Hooks 事件系统（最小集）**：PreToolUse 过滤、PostToolUse 结果整形、PreCompact 抢救、SessionStart 注入——R-Code 的审计边界天然是挂点。

### C 档：高成本/依赖生态，谨慎后置

11. 本地向量检索（LanceDB + Ollama）——语义泛化好但 Sourcegraph/Cursor 双反例说明可推迟；先 BM25。
12. Skills 生态完整三层渐进披露（已有 .agents/skills 基础，可渐进做）。
13. git worktree 并行会话 / checkpoint 回滚（桌面端独有体验优势，Gemini `/restore` 是参照）。
14. spec-kit 式 SDD 流水线（R-Code 的 plan-loop / prd-to-ai-worklist skills 已在同方向，可产品化对接 tasks.md 工件）。
15. DeepWiki 式仓库 wiki 生成——可结合已有的 built-in deep-research MCP 做"项目知识库"增值功能，但注意时效性维护成本。

### 一条主线

**R-Code 服务大项目的公式 = AGENTS.md 兼容（文档索引） + grep 三段式与可选本地索引（代码索引） + plan/记忆/todo 外部化状态 + microcompact/compaction + 只读检索子代理。** 前 5 项（A 档）几乎零新增依赖，B 档三项全部有 Rust 原生生态支撑，与 R-Code 本地优先、审计边界的定位完全一致；不做的只有"服务端索引"和"重型多代理"。

---

## 8. 来源索引（节选核心一手来源）

**官方文档/博客**
- Anthropic：Effective context engineering（anthropic.com/engineering/effective-context-engineering-for-ai-agents）、Claude Code best practices、memory / sub-agents / skills / hooks / costs / large-codebases / context-window（code.claude.com/docs/en/*）、Writing tools for agents、Code execution with MCP、Multi-agent research system、Effective harnesses for long-running agents
- OpenAI：Codex AGENTS.md 指南（developers.openai.com/codex/guides/agents-md）、local-config、Unrolling the Codex agent loop（openai.com/index/unrolling-the-codex-agent-loop）、Harness engineering、Run long horizon tasks with Codex、Shell + Skills + Compaction、Codex multi-agent
- Cursor：Securely indexing large codebases（cursor.com/blog/secure-codebase-indexing）、docs/context/rules、changelog 2.1（Instant Grep）、self-driving codebases / scaling-agents
- GitHub/VS Code：Copilot coding agent AGENTS.md changelog（github.blog，2025-08-28 / 2025-11-05）、workspace-context、custom instructions、VS Code 1.105（plan agent）
- Google：gemini-cli docs（gemini-md / auto-memory / subagents / plan-mode / commands）、Google Cloud context engineering、ADK event compaction
- Sourcegraph：How Cody understands your codebase、Cody is cheating、Why coding agents fail in large codebases（CodeScaleBench）、Sourcegraph MCP vs Mythos、Code Finder、technical changelog
- Cognition：deepwiki（cognition.com/blog/deepwiki）、docs.devin.ai（knowledge / rules / memories / remote-indexing）
- Cline：Why Cline Doesn't Index Your Codebase（cline.bot/blog）
- 其他：agents.md（官网 + github.com/openai/agents.md）、zed.dev/docs/ai、ampcode.com/docs、opencode.ai/docs/rules、LlamaIndex blog（Is grep all you need）、spec-kit（github.com/github/spec-kit）、Kiro（kiro.dev/docs/specs）、OpenSpec（github.com/Fission-AI/OpenSpec）、Task Master（github.com/eyaltoledano/claude-task-master）

**源码级**
- aider：github.com/Aider-AI/aider（aider/repomap.py、aider/queries/）
- tree-sitter：github.com/tree-sitter/tree-sitter（crates/tags）；github/code-navigation
- SCIP：github.com/sourcegraph/scip（scip.proto）；rust-analyzer crates/rust-analyzer/src/cli/scip.rs
- stack-graphs（已停更）：github.com/github/stack-graphs
- Continue：github.com/continuedev/continue（core/indexing/*、core/context/retrieval/pipelines/*）
- zoekt：github.com/sourcegraph/zoekt；Tabby：github.com/TabbyML/tabby（crates/tabby-index）

**学术/研究**
- Context Rot（Chroma, 2025, trychroma.com/research/context-rot）、NoLiMa（arXiv:2502.05167）、Lost in the Middle（arXiv:2307.03172）、Is Grep All You Need?（arXiv:2605.15184）、Galster et al. 2026（arXiv:2602.14690，2,853 仓库 context files 实证）、METR time-horizon

**未能核验（明确标注）**：rulesfile.dev（DNS 不可解析）、Cursor Merkle tree 原文（仅存档/二手转述）、Claude Code "microcompaction" 专有名词（行为有一手描述，词已从官方文档消失；社区逆向为二手）、Mutable.ai AutoWiki（原站下线）、Sourcegraph《Code search: where we are...》（疑下线）。
