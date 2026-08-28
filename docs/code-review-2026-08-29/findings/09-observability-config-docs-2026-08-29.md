# RV-09 日志与可观测性 / 配置管理 / 文档与代码漂移 — 深度审查

- 日期：2026-08-29；审查人：code-review 代理（RV-09）
- 仓库：D:\project\rust\r-code @ 工作树现状（HEAD 49f9193，含未提交 WIP）
- 证据：`docs/code-review-2026-08-29/evidence/RV-09-obs-config-docs.md`

## 扫描方法与覆盖声明

方法：全文精读 `logging.rs` / `log_buffer.rs` / `support_bundle.rs` / `feature_flags.rs` / AGENTS.md / dev 脚本 / tauri conf 全部变体；`settings.rs`（60K）按 rg 定位读关键段（load/save/write_global/migrate/凭据后端）；`provider_catalog.rs`（93K）与 `model_capabilities.rs` 只读结构、入口函数与交叉引用；README 英/中、CONTRIBUTING、CHANGELOG 头部、docs/readme.md、ci.yml/release.yml 事实核查；i18n 基线脚本全文精读 + node 统计。全程只读，未跑 build/test。

覆盖结论（清单项 → 结论）：

| 清单项 | 结论 |
| --- | --- |
| 1a 日志初始化/轮转/保留 | 已审。按日滚动+7 天保留，但单日文件无大小上限 → F-obs-01 |
| 1b 日志密度/级别纪律 | 已审，**未发现问题**：`llm_runtime.rs` 全部 12 处 `info!` 均为低频事件（压缩档位、缓存形状变化、派生确认、run 收尾），每条带 session_id/task_id；main.rs/commands.rs 的 info! 均为启动/迁移类一次性事件；前端 `console.log|info|debug` 全仓仅 1 处，不存在"前后端双写"；error! 场景真实（DB 迁移失败、tool panic、重试耗尽），无滥用。RUST_LOG 在 release 同样生效（logging.rs:62-63 `EnvFilter::try_from_default_env`），debug/trace 可在 release 打开 |
| 1c 故障可诊断性 | 基本达标：task_id/run_id/session_id/agent_id/tool_call_id 贯穿 worker/gateway 日志；provider 失败有 `log_provider_request_failure`（llm_runtime.rs:4253）记录请求形状。support_bundle 双重脱敏+白名单 DTO+只读打开+原子写+预览不落盘，质量好 |
| 1d 指标面 | 无 metrics crate。仅有两个窄口径进程内计数器（request_audit 信封自检、diagnosis hint 计数）→ F-obs-04 |
| 2a settings 结构/原子性/迁移 | 优先级链（默认<全局<工作区<env<显式参数）实现清晰；默认值集中在 `agent_config::Config::default()`+serde default，settings.rs 内 `unwrap_or*` 仅 7 处（错误分支为主）；**write_global 非原子** → F-obs-05；无版本化迁移框架 → F-obs-08 |
| 2b 多配置源优先级/feature flags | 优先级实现与模块注释一致；feature flags 默认全关（fail-closed）与测试断言一致，但 save 非原子 → F-obs-06 |
| 2c provider_catalog/model_capabilities | capabilities 单一入口消费 catalog（`preset_for`/`resolve_protocol`/`vision_budget_for`），**无双源冲突**；catalog 为 93K 静态硬编码，无时效性对账机制 → F-obs-09 |
| 2d gen/schemas 与 tauri.conf | gen/schemas 两文件为 Tauri 构建生成物（git status 中 modified 属 WIP 正常），无手改漂移证据；conf 变体（dev/macos/presign/local-package）为合法 overlay 合并，presign 变体 `cwd:"../scripts"` 指向真实存在的 `scripts/presign-macos-bin.sh`。未发现漂移 |
| 3a README 事实核查 | **全部通过**：dev.ps1/dev.sh 存在且行为一致；`npm run dev/build` 与 frontend/package.json scripts 一符；`cargo tauri dev --config src-tauri/tauri.dev.conf.json` 与 dev.conf（R-Code Dev 身份+dev-latest.json 通道）一致；README 引用的 12 个文档/脚本文件全部存在 |
| 3b AGENTS.md | 工具名与实际开发环境脱节 → F-obs-11；提交约定（feat:/fix:/docs: 前缀）与 git log 实际一致 |
| 3c CONTRIBUTING vs CI | **一致**：fmt/clippy -D warnings/test/npm audit --audit-level=high/secret scanning 均在 ci.yml；cargo-deny 按 CI 固定版本 0.20.2；supply-chain --strict 在 release.yml |
| 3d docs/ 大盘 | 一级目录 3 个（product-experience-redesign 2026-08-28、support 2026-08-27、code-review-2026-08-29 未提交），均活跃不陈旧；releasing.md 索引引用抽查无失效。**readme.md 进度表述过期** → F-obs-10 |
| 3e CHANGELOG vs version | **同步**：[1.0.0] 2026-08-23 = workspace Cargo.toml version 1.0.0 = tauri.conf.json 1.0.0 |
| 4 i18n | 基线门禁真实有效（count+sha256 deepEqual，防增长）；默认中文落点完整（localeFromLanguages 恒 zh-CN、旧档一次性重置、fallbackLng zh-CN）；**存量硬编码 73 文件/3557 条** → F-obs-12 |

## Findings 总表

| ID | 位置 | severity | 根因描述 | 修复方向 |
| --- | --- | --- | --- | --- |
| F-obs-01 | src-tauri/src/logging.rs:49 | major | 日志按日滚动但单日文件无大小上限，7 天保留不构成字节上界 | 自定义按大小滚动的 appender 或对当日 JSONL 做大小闸门+截断告警 |
| F-obs-02 | src-tauri/src/log_buffer.rs:123-141 | minor | 启动水合全量读取 7 天日志文件后仅截取 1000 条 | 从每个文件尾部倒读有限行数（rev-lines）再合并截断 |
| F-obs-03 | src-tauri/src/log_buffer.rs:84-96 | minor | 每条日志事件争抢全局 Mutex 并同步格式化，热路径串行化 | try_lock 丢弃溢出或改无锁环形缓冲/通道 |
| F-obs-04 | Cargo.toml:53-55（workspace 依赖）；src-tauri/src/tauri_commands.rs:1760 | major | 无指标面：仅有窄口径自检计数器，无 provider 延迟/错误率/重试聚合 | 引入 metrics crate 或最小自研计数器+直方图，暴露到诊断页/support bundle |
| F-obs-05 | src-tauri/src/settings.rs:763-772 | major | 主配置 config.toml 用 `std::fs::write` 非原子写，且损坏后无备份/恢复路径 | 改 NamedTempFile+persist（同仓 mcp_settings.rs:444-448 已有范式），可加 .bak |
| F-obs-06 | src-tauri/src/feature_flags.rs:91-98 | minor | features.toml 同样非原子 `std::fs::write` | 同上改原子写 |
| F-obs-07 | src-tauri/src/security_config.rs:37 vs src-tauri/tauri.conf.json | minor | CSP 双源漂移：rust 副本 img-src 缺 `blob:`（tauri.conf 含），SecurityConfig::production 当前无消费者 | 删除未用的 CSP 副本或加测试强制两源一致 |
| F-obs-08 | src-tauri/src/settings.rs:715-761 | minor | config.toml 无 schema version/迁移框架，字段演进依赖 serde default+一次性迁移函数 | 引入 config_version 字段+集中迁移表；重命名字段时显式迁移 |
| F-obs-09 | src-tauri/src/provider_catalog.rs:239-1098 | minor | 93K 静态内置目录无自动时效校验，模型生态漂移只能靠人工发现 | 增加目录快照+字段完备性 lint，或上线人工核对 checklist 进 release 流程 |
| F-obs-10 | docs/readme.md:26 | major | 文档索引称产品体验重构"当前进度 1/42，产品代码尚未实施"，与 worklist-gate.json passed:true（42/42）及 HEAD 提交"42/42 闭环"矛盾 | 更新该表行为当前真实状态，并约定 gate 通过后同步 readme.md |
| F-obs-11 | AGENTS.md:8-10,19-20,26 | minor | 仓库规约声明的工具名（glob/search_files/read_file/edit/apply_patch/git_status）与当前代理环境（Bash/Read/Edit 等）不匹配 | 改为工具无关表述（"用可用工具"），或按环境分节 |
| F-obs-12 | src-tauri/frontend/scripts/i18n-hardcoded-baseline.json；scripts/i18n-hardcoded.test.mjs:146-153 | major | 硬编码文案存量 73 文件/3557 条（SettingsScene 509、Canvas 422），基线只锁不增；en-US 切换后大面积中文 | 按文件设递减预算（baseline 上限单调下降）驱动清偿，优先 SettingsScene/Canvas |

## 逐条展开

### F-obs-01（major）单日日志文件无大小上限，保留策略不是字节上界

- 位置：`src-tauri/src/logging.rs:49` — `tracing_appender::rolling::daily(&log_dir, LOG_FILE_PREFIX)`；`log_buffer.rs:27-28` — `LOG_RETENTION_DAYS: i64 = 7`（注释明示"产品安全边界，不暴露为用户设置"）。
- 事实：滚动策略只按自然日切分 + 删除 7 天前文件（`prune_expired_logs_at`，log_buffer.rs:221-249，仅在启动与 support bundle 收集时执行）。tracing-appender 0.2 的 daily appender 不支持单文件 max_size。`RUST_LOG` 覆盖在 release 同样生效（logging.rs:62-63），一旦用户/support 指引设置 `RUST_LOG=trace`（或 debug），agent loop 的逐 token 级事件会让当日文件在数小时内增长到 GB 级；日志目录在 `app_data_dir()/r-code/logs`（logging.rs:15-19），与用户数据同盘，7 天 × 无上限单文件 = 磁盘可被占满，且写满后 non_blocking 队列只会静默积压内存。
- 影响：运维盲区→直接故障（磁盘占满影响整个用户卷）；日志写满后诊断能力同时丧失。
- 修复方向：包一层按大小强制轮换的 writer（文件名序号），或 BufferLayer 写盘前检查当日文件大小，超过阈值（如 50MB）降级为 warn 摘要+丢弃并打一条 `log_file_size_cap_hit` 事件。

### F-obs-02（minor）启动水合全量读取 7 天日志

- 位置：`log_buffer.rs:123-141`（`hydrate_from_persistence` → `read_persisted_entries` 176-204 对每个文件逐行 `serde_json::from_str`）。
- 事实：启动时读取全部 7 天文件的全部行、全部反序列化、再 `drain` 到 1000 条。单日文件 100MB 时启动要做 ~百万次 JSON parse，拖慢启动且制造内存尖峰。与 F-obs-01 叠加放大。
- 修复方向：每个文件只倒序读尾部 N 行（如 1000），按时间合并后截断。

### F-obs-03（minor）日志事件全局锁串行化

- 位置：`log_buffer.rs:84-96` — `on_event` 中 `buffer().lock().unwrap()`、push、drop，再走 non_blocking writer。
- 事实：所有线程的所有 tracing 事件（含 debug）都要先抢同一个 `Mutex<VecDeque>`，并在持锁路径外同步做 `redact_text`+`serde_json::to_writer` 格式化（redact 在锁内构建 message）。文件写入虽经 non_blocking，但缓冲入队本身是全局串行点。trace 级别长会话下为可测量的吞吐税。
- 修复方向：`try_lock`，失败即丢弃（诊断缓冲允许丢）；或改 crossbeam 通道由专职线程消费。

### F-obs-04（major）指标面缺失（盲区清单）

- 位置：workspace `Cargo.toml:53-55` 仅有 tracing/tracing-subscriber/tracing-appender，无 `metrics`/`prometheus`；全仓无 histogram/counter crate 引用（rg 证据见 evidence）。
- 已有的窄口径计数器（不是通用指标）：
  - `cmd_request_audit_counters`（tauri_commands.rs:1758-1767）：请求信封 headers_appended/mismatches 自检，按 task_id 查询，"Real runtime 不在场时返回 None"，soak/devtools 用；
  - `cmd_diagnosis_hint_counters`：诊断提示 (类别, 次数)；
  - support bundle 的 DbStats（task/run/tool_call 行数）；
  - request-audit 的 token 分项审计（四类 token、wire bytes，CHANGELOG 1.0.0 有记录）——是逐请求审计记录，不是可聚合指标。
- 盲区清单：provider 请求延迟（P50/P95，TTFT 与整请求分离）、provider 错误分类计数（4xx/5xx/超时/取消）、自动重试次数、上下文压缩触发频率与会话存活时长、MCP server 启动失败率、日志写入退化（logging.rs:85/111 的 `diagnostic log persistence degraded` 只是一条 warn，无计数）。这些目前都只能靠人肉翻 7 天 JSONL。
- 修复方向：不必引入 prometheus；最小方案是进程内 `metrics` crate + `metrics-exporter-text` 挂到诊断页/support bundle（与现有产品形态一致），把上述 5 类以 counter/histogram 落地。

### F-obs-05（major）主配置 config.toml 非原子写且无损坏恢复

- 位置：`settings.rs:763-772` `write_global` — `std::fs::write(&path, toml_str)?` 直接 truncate+写。
- 对照（同仓库已用原子写的地方）：`mcp_settings.rs:444-448`（NamedTempFile+persist）、`settings.rs:464`（agent-prompts.toml）、`support_bundle.rs:128-130`（导出 JSON）。主配置反而是唯一裸写的 TOML。
- 后果链：写一半崩溃/断电 → config.toml 半截 TOML → `parse_config_file`（settings.rs:786-790）返回 ConfigError → `load_global_unvalidated` 失败 → 任务启动（commands.rs:5085、15114 等）、模型发现、设置页全部报错；无 .bak、无恢复 UI（对比：DB 迁移前有 pre-migration backup，main.rs:483-488，说明仓库对 SQLite 做了此类防护，对主配置没有）。用户唯一自救是手删文件（丢失全部 provider 配置与路由偏好）。
- 修复方向：`write_global`/`save_execution_settings` 改为 `NamedTempFile::new_in(parent)` + `persist`；可选在成功写后保留上一份为 `config.toml.bak`，parse 失败时提示恢复。

### F-obs-06（minor）features.toml 非原子写

- 位置：`feature_flags.rs:91-98` — `std::fs::write(self.path(), content)?`。
- 后果：损坏后 `load()`（:78-89）toml parse 报 ConfigError。browser/automation/worktree 默认 fail-closed 全关，损坏文件会把已开启的实验功能静默打回关闭或直接报错（取决于调用方），且同样无恢复路径。修复同 F-obs-05。

### F-obs-07（minor）CSP 双源漂移（rust 副本缺 blob:）

- 位置：`security_config.rs:37`（`SecurityConfig::production().csp`）= `img-src 'self' data:`；`tauri.conf.json` `app.security.csp` = `img-src 'self' data: blob:`（node 精确比对 equal:false，证据见 evidence）。
- 事实：Tauri 实际注入的是 conf 里的 CSP（blob: 是附件 blob 预览所需）；`SecurityConfig::production()` 当前无任何调用方（rg 全仓仅 lib.rs:81 re-export 类型 + main.rs:327 使用 `should_block_navigation`），即 rust 侧 CSP 是一份已经漂移的"文档化安全配置"，一旦未来被当作真实源启用，会立刻破坏附件预览或给出错误的安全审计基线。
- 修复方向：删掉未用的 CSP 字段，或加一个单测 `assert_eq!(SecurityConfig::production().csp, tauri_conf_csp())` 钉死两源一致。

### F-obs-08（minor）配置无版本化迁移框架

- 位置：`settings.rs:715-761`（`migrate_legacy_provider_secrets`/`migrate_legacy_provider_kinds` 两个一次性迁移，靠字段是否为空/None 触发）；config.toml 无 `config_version` 字段。
- 后果：字段类型/名字演进时 serde default 会把旧值静默吞掉（用户设置回默认而不报错）；一次性迁移函数无法区分"从未配置"与"上版本已迁移"，rename 类变更没有可扩展的挂载点。
- 修复方向：顶层加 `config_version`，集中迁移表按版本链执行；短期至少在 CHANGELOG/运维手册记录已知静默丢弃字段。

### F-obs-09（minor）provider_catalog 93K 静态目录的时效性维护

- 位置：`provider_catalog.rs:239-1098`（`PRESETS` 常量，~860 行硬编码预设；含 124 处 base_url/endpoint 引用），最后实质更新 2026-08-23（git log 证据）。
- 事实：模型 ID/窗口/能力（如 DeepSeek V4 系列标注）与真实 provider 生态同步只能靠人工；仓库内没有对账脚本或快照测试（无任何访问网络的 catalog 校验）。model_capabilities 与 catalog 的关系是**单一入口消费**（`model_capabilities.rs:18,95,126,154` 调 `preset_for`/`resolve_protocol`/`vision_budget_for`），此维度无双源问题——风险纯粹是目录内容过期。
- 修复方向：为 PRESETS 加结构化 lint（id 唯一、协议合法、vision 标注与 vision_budget 一致）；发布流程加"目录人工核对"步骤；把目录最后核对日期写进目录数据本身。

### F-obs-10（major）docs/readme.md 进度表述与 gate/代码矛盾

- 位置：`docs/readme.md:26` — "产品体验重构 PRD / AI 实施清单 | `frozen`，当前进度 `1/42`；本次只完成原型和实施合同，产品代码尚未按清单实施"。
- 事实：`docs/product-experience-redesign/worklist-gate.json`（工作树现状）`"passed": true`，counts `{requirements:64, checklist_tasks:42, task_cards:42, assertions:176}`；HEAD 提交 49f9193（2026-08-28）标题即 "feat(*): product-experience worklist 42/42 闭环"；且工作树 WIP（SettingsScene/Canvas/runs-panel 等）仍在该工作流上迭代。文档索引首页（维护者入口的第一张表）声称 1/42 且"产品代码尚未实施"，会直接误导新维护者对仓库状态的判断（例如以为 Settings/Runs 面板还是原型）。
- 修复方向：更新该行为 "42/42 已闭环，追踪 gate.json"；约定 gate.json 状态变化时同步 readme.md 的"当前实施合同"表。

### F-obs-11（minor）AGENTS.md 工具规约与实际开发环境脱节

- 位置：`AGENTS.md:8-10`（`glob`/`search_files`/`search`+pattern/queries 参数）、`:19-20`（`read_file`/`edit`/`apply_patch`）、`:26`（`git_status`）。
- 事实：这些是另一套 CLI 代理的工具名与参数协议；当前会话实际环境是 ZCode（Bash/Read/Edit/Write 等），并不存在名为 glob/search_files/git_status 的工具。该文件自称"每次会话开始时自动读取"，其"避免低级报错"的速查表在新环境里反而制造错误联想（例如引导代理去找不存在的 `search_files`）。技术栈、目录、提交约定部分仍然准确。
- 修复方向：改写为工具无关表述（"按文件名搜/按内容搜/读/精确替换，用当前环境提供的对应工具"），删掉具体参数协议；或明确标注适用的代理环境与版本。

### F-obs-12（major）i18n 硬编码存量 3557 条，en-US 支持名不符实

- 位置：机制在 `src-tauri/frontend/scripts/i18n-hardcoded.test.mjs:146-153`（baseline deepEqual 断言）与 `scripts/i18n-hardcoded-baseline.json`（294 行，73 个文件条目）。
- 数据（node 统计，evidence 有命令）：73 文件 / 共 3557 条硬编码用户文案；Top：SettingsScene.tsx 509、Canvas.tsx 422、PlanPanel.tsx 170、Composer.tsx 132、McpPanel/MemoryPanel 各 125。locale 目录 zh-CN/en-US 各 201 行，即已迁移键约百级。
- 机制评估：门禁本身**有效**——检测器扫 .tsx 的 JSX text + 白名单属性 + 表达式字符串字面量，per-file count+sha256 锁定，任何新增硬编码都会让 `npm test` 失败，"基线锁定"目标达成；但基线只防增长、无清偿机制（无递减预算），存量不收敛。
- 影响：i18n/index.ts 声明支持 en-US 显式切换（:82-95 fallbackLng zh-CN；README.md 为英文用户提供完整英文文档），实际切换后 Settings/Canvas/Plan 等主界面大面积仍是中文硬编码（SettingsScene 509 条）。默认中文策略（2026-08-28 产品决定）掩盖了该缺口。
- 修复方向：baseline 从"冻结值"改为"上限"（允许 count 下降不允许上升），并按文件设递减里程碑；优先迁移 SettingsScene/Canvas（两者占 26%）。

## 正面确认（无需修复，供交叉参考）

- 日志脱敏三道防线：字段名白名单（log_buffer.rs:270-299）、落盘前 `redact_text`（:89）、磁盘读回再脱敏防旧规则（:196-199）+ support bundle 导出时第三次脱敏（support_bundle.rs:169）。
- support bundle：MCP 信息走白名单 DTO（无命令/参数/URL/凭据，:67-76）、DB 只读打开不创建（:195-204）、tempfile+persist 原子导出（:128-130）、preview 不写盘。
- settings 优先级链与文档注释一致；save_global 先写凭据后落 TOML 的顺序有明确防丢注释（settings.rs:626-628）。
- CHANGELOG/Cargo/tauri.conf 版本三者同步 1.0.0；README 双语命令与脚本全部实测存在；CONTRIBUTING 门禁与 CI 一致。
- 日志关联键纪律好：worker/gateway 日志普遍携带 task_id/run_id/session_id/agent_id/tool_call_id。
