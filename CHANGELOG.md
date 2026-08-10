# Changelog

R-Code 的用户可见变化记录在此。格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

提交和 Pull Request 是实现级历史；本文件记录每个发布版本对用户和运维者有意义的变化。

## [Unreleased]

## [0.3.3] - 2026-08-11

### Added

- 完成双向 MCP 集成：R-Code Agent 会并行发现已启用服务的 `tools/list`，把真实描述与输入 schema 动态暴露为可直接调用的 `mcp__<服务>__<工具>`；R-Code 也可作为 stdio MCP Server 向 Codex 等宿主公开委派、只读委派、状态查询、结果等待和取消任务 5 个工具。
- 设置页新增 RTK 加速开关：开启时会检测并按当前系统安装 RTK、校验下载并原子写入 R-Code 全局可用目录，再为新对话启用优先命令策略；关闭时仅停用托管配置，保留已安装程序便于再次启用。
- 项目页新增项目记忆管理入口，手动复盘可真实触发项目记忆提取；未发送的输入按项目与对话自动保存草稿，切换页面、项目或功能后可继续编辑。
- 模型切换器按已配置 Provider 分组并支持折叠，清楚标出「当前 Provider」和当前模型；Kimi 类 Provider 新增思考开关与思考强度选项。

### Changed

- Codex 子代理改用可复用的 App Server 会话链路，并加入面向完成任务的运行引导：优先批量读取相关文件、减少低价值往返，不再设置固定工具调用次数或总时长上限，仅在约 5 分钟无实质进展时触发软性提醒。
- 子代理短结果完整回传，长结果先按范围总结并保留关键证据，避免固定长度一刀切截断；运行、命令、文件编辑和工具调用统一使用可折叠活动卡片，主 Agent 与子代理采用明显不同的 R-Code/Codex 图标。
- Windows 桌面端支持关闭窗口后驻留后台，并可从托盘恢复或显式退出；初始化与本机配置读取移到后台复用，降低首次提问和重复启动等待。
- 项目内的对话添加按钮会立即预创建空对话并依次命名，单项目最多保留 5 个并行空会话；项目添加按钮恢复为只创建项目，不再误建对话。
- DeepSeek、Kimi 等兼容 Provider 可分别走 OpenAI Chat、Responses、Anthropic 兼容口或自定义网关，统一保留流式输出、thinking/reasoning 与缓存用量；稳定前缀和协议路由由客户端无感处理。

### Fixed

- 修复 Kimi 类 Provider 请求开始后无内容便直接结束的问题，补齐各兼容协议的流式事件、推理内容、工具调用和 usage 解析。
- 主 Agent 的完全访问权限现在会作为子代理权限上限与默认值直接继承，不再为同一作用域重复弹出审批；显式只读任务仍保持只读，权限不会反向越级。
- 修复 `clear` 无法清空当前对话、手动复盘无实际效果、项目记忆触发状态与界面表现不一致，以及页面切换导致未提交输入丢失的问题。
- 设置、安装和后台操作提示改为短暂反馈后自动消失；RTK 安装或配置失败时开关自动回弹，详细原因只写入诊断日志。
- 修复子代理工具活动终态不一致、命令/编辑记录难以收起、长任务过早超时，以及首次创建对话仍在首问阶段重复初始化的问题。

### Security

- 第三方 MCP 直连工具统一经过 R2 权限、审批与审计；Plan/严格只读模式不暴露变更型直连工具，工具名称会稳定规范化，离线服务自动退避，`tools/list` 握手限制为 15 秒且不限制实际长任务执行时间。
- RTK Windows 安装使用固定版本与下载地址、SHA-256 校验、原子替换和失败回滚；Provider 密钥仍只通过凭据引用传递，不写入提示、日志或 MCP 配置。

## [0.3.2] - 2026-08-07

### Changed

- DeepSeek 线路长会话显著提速降价：请求前缀改为逐字节稳定以命中 DeepSeek 字节级自动前缀缓存——system prompt 移除秒级时间戳并在 run 内冻结复用；时间（分钟级）、任务上下文、Plan 模式与委派提示统一改为尾部消息注入且不落历史；工具列表按名称排序，Codex 可用性判定在 run 内冻结；历史严格只追加，悬挂工具调用的修复结果落盘固化。真实 API 14 轮实测尾部命中率 93%（基线存档 `docs/deepseek-cache-baseline.md`）。
- 网络抖动下运行不再直接失败：连接层指数退避重试（≤10 次，仅 408/429/5xx/连接类，4xx 与鉴权失败不重试）；流式响应在产出任何内容前停滞超过 120s（空闲 watchdog）时，用与首试逐字节一致的冻结请求静默重放（≤5 次），失败尝试不写会话；发生重放时运行条目显示「重试 N 次」。
- 用量统计可观测缓存收益：DeepSeek 流式请求启用 `stream_options.include_usage`，解析 `prompt_cache_hit/miss_tokens`（兼容 OpenAI `prompt_tokens_details.cached_tokens`），原生 Agent 线路 usage 持久化，时间线运行条目显示缓存命中率；前缀形状（system/tools 哈希 + 改写版本）逐轮归因记录缓存变化原因。
- 长会话接入分层压缩：相对上下文窗口 50% 提示一次、60% 剪除旧工具结果、80% 摘要折叠（保留首个小 user 轮次与尾部原文），连续 2 次压缩即防抖暂停，token 估算用真实 usage 逐轮校准。
- GitHub Release 说明内联当版本 CHANGELOG 内容，不再仅给出 Full Changelog 跳转链接。

### Fixed

- thinking 模式（deepseek-reasoner）请求兼容性：assistant tool_calls 轮恒发 `reasoning_content` 键、tool 消息恒发 `name` 键，消除 DeepSeek 400 类报错并保持请求字节确定。

## [0.3.1] - 2026-08-07

### Security

- 文件 I/O 改为经受工作区目录 capability 限制的句柄打开（`cap_std`），消除路径校验后符号链接替换带来的 TOCTOU 竞态逃逸窗口。
- 修复 IPv4-compatible IPv6 地址（`::a.b.c.d`）绕过私网/IP 拦截检查的 SSRF 缺口；仅对能无歧义映射为 IPv4 的形式套用 IPv4 拦截规则。
- 发布与 CI 工作流按最小权限收紧（`contents: read`），第三方 actions 固定到提交 SHA，并启用 Dependabot 依赖更新。

### Fixed

- 桌面端与 MCP 进程独立启动时并发升级同一数据目录，由独立的 SQLite 锁数据库串行化备份、迁移与恢复关键区，防止后到进程用旧快照覆盖新数据。
- 发布 finalize 会先以资产 API 元数据核对草稿 Release，再为 updater manifest 构造当前不可变标签的规范下载 URL；新增 `finalize_only` 恢复模式可安全复用完整草稿资产，并保守保留平台未签名警告。

### Changed

- 发布流程新增不可变 tag 溯源与 CI 质量门校验；发布前核对 tag 精确提交的完整 CI run，缺失时拒绝创建 tag。
- 仓库文档精简：移除历史归档（`docs/archive`）与过时 UI 参考图，仅保留当前亮/暗两套；新增 DeepSeek Provider 前缀缓存优化 PRD 与安装/备份/恢复运维手册。

## [0.3.0] - 2026-08-06

### Added

- Codex App Server 主 Agent 可通过会话内动态工具把有界任务委派给 R-Code 子代理：复用当前任务/运行树，不再创建独立 session；同一任务最多 3 个子代理并发，支持逐个取消。
- Codex 运行的公开推理摘要（reasoning summary）显示在时间线并本地持久化；原始思维链内容从不进入 UI 或存储。
- 助手回复中的工作区文件引用可点击打开右侧 Files 工作台，并跳转到指定行。

### Changed

- 子代理权限以“只读 / 需审批 / 完全访问”三态实时展示并写入 schema 25；重启或重新打开任务后保持与运行时一致。
- Codex 委派的 R-Code 子代理按父运行预设继承权限：只读父运行保持只读，审批类父运行允许看到写入/命令工具但必须逐次审批，只有完全访问父运行才能直接授予完全访问；显式 `read_only` 永不升级。
- hosted Codex 运行仅禁用由 R-Code 管理的 legacy `mcp_servers.r-code`，保留用户配置的其他 Codex MCP；同树委派由会话内动态工具提供，避免旧工具创建第二个顶层 session。
- 动态委派审计记录使用宿主派生的唯一 ID，外部 callId 只作展示/关联；委派标签、目标摘要与有效权限档位一并入库。
- 运行时长改为共享时钟约每秒刷新，并隔离到计时组件，避免整条时间线随计时重渲染。
- 任务开始后锁定其工作区绑定；需要切换目录时必须先停止当前运行，避免工具访问边界在执行中变化。
- 发布准备改为事务式写入并在失败时逐字节回滚；发布前同时核对所有 Tauri updater 平台/安装器条目、Release 资产 URL 与对应 `.sig` 内容。
- GitHub CI、波动测试与发布工作流统一使用 Node 24 原生的 checkout、setup-node 与 artifact actions，消除 Node 20 运行时弃用警告。

### Fixed

- 修复审批模式子代理看到工具却因 Host 模式缺少 `bash` 而直接报错的问题；工具现在保持可见，并统一经过 Gateway 审批和审计。
- Provider 建连、内置/外部工具、Shell 与 MCP 调用均可响应取消；Shell 会终止并回收完整进程树，父运行收尾只清理自己的子树，20 秒兜底后幂等关闭遗留 Run 与工具审计，不再永久显示“运行中”。
- 子代理只产生一个终态事件；满队列时先排空保留的工具事件再写入终态，避免工具审计在终态之后重新变成 running。
- Codex App Server 的审批与动态工具请求可并发处理，未知反向请求会得到 JSON-RPC 错误；setup 可取消，JSONL 单行有内存上限，steer 应答不会误吞请求帧。
- 动态同树委派会核对实际选中的 Codex CLI（最低 `0.145.0`）与 R-Code Provider；能力不可用时只隐藏动态工具并给出可见降级原因，Codex 主任务仍可继续。
- 取消或审批超时会原子清理 pending 请求；并发的拒绝/长期允许只有一个决策能生效，不会在已结束运行后遗留可点击卡片或错误 standing rule。
- 主运行的停止、自然收尾与显式委派共用同一原子启动边界；收尾开始后的引导/立即发送会持久化到下一轮，不再产生已结束父运行下的“幽灵”子代理或被旧收尾覆盖的新状态。
- Codex App Server 的在途反向请求、stdin writer 和 stdout 帧队列均有硬上限；32 MiB 大帧的原始排队预算限为 64 MiB，异常或恶意 CLI 输出不再可无界占用内存。
- Windows updater 清单严格区分 en-US MSI、zh-CN MSI 与 NSIS setup，并要求三个唯一、完整的安装资产映射。

### Security

- standing allow 同时绑定任务、工具、调用提供的精确目标与批准时的风险上限；R2 授权不能放行同工具的 R3 调用，App Server profile 审批也不会扩成无目标通配符。
- 第三方 generic `mcp_call` 始终按 R2 处理，`annotations.readOnlyHint` 仅供展示，不能降低授权要求；Plan/严格只读策略不暴露或执行 generic MCP。
- Codex `permissions/requestApproval` 的文件范围会与物理工作区求交，拒绝 `..` 与符号链接逃逸，并以完整请求指纹隔离 session standing rule。
- 需审批的 Codex 子进程固定使用 `read-only` sandbox 与 `on-request` 审批；更宽松的全局 Codex 配置不能绕过 R-Code 权限引擎。
- 四平台构建只上传 updater 产物与签名，由唯一 finalize job 生成、交叉验证并上传 `latest.json`，消除并行覆盖平台键的竞态。

## [0.2.2] - 2026-08-04

### Added

- Plan 实施支持 `1 / 1.1 / 1.2` 层级进度、依赖解锁、连续派发与可并行事项提示。
- 诊断日志按日持久化并固定保留最近 7 天，覆盖模型、工具、子代理、MCP 和恢复链路的重要失败事件。
- Goal 直接复用主输入框创建和执行，并可在运行中编辑、停止、继续或删除。
- 统一的排队、引导、立即发送策略覆盖新对话和后续对话；队列独立显示在输入框上方，支持拖拽/键盘排序、编辑、引导和删除。
- 新增归档中心，可恢复或确认永久删除只读历史；活动页改为任务级进展与待处理结果，并自动排除归档对话。
- 项目级请求审批、风险代审和完全访问权限在新对话与任务输入区保持可见，并同时约束 R-Code 与 Codex 主 Agent。

### Changed

- Agent 可在确有需要时自行进入 Plan；Plan 只有经用户确认后才回到实施，计划整体、阶段和功能点均可折叠。
- 运行、终端、审核、Plan 与子代理共用可持久恢复的任务工作台标签；键盘方向键、Home 和 End 可切换标签。
- 质量复核仍默认关闭，启用后的默认复核者改为 R-Code；增强审核在没有对应 Plan 功能点时明确保持为空。
- 记忆与知识控制面改为更紧凑的本机作用域布局，并把启用入口、复盘状态和隐私边界直接呈现给用户。

### Fixed

- 诊断页会合并当前进程和重启前的近期日志；支持包不再从导出目录误读不存在的日志文件，只导出带原始时间戳与模块名的脱敏 warning/error，并通过系统目录选择器选择导出位置。
- 发送按钮在消息真正接纳前显示加载状态，运行中使用清晰的停止按钮；输入框不再重复展示低价值工具调用动态。
- 侧边工作台切换、隐藏和重开后保留审核内容与全部标签，子代理详情不再独占并清空其他工具。
- 终端输出改为事件唤醒和 200KB 有界尾部缓冲，消除高输出或终端不可见时的输入卡顿与无界内存增长。
- 队列排序与空闲分发并发时采用原子认领和有限退避重试，避免消息永久停住；大量队列的序号计算改为线性复杂度。
- 运行中引导会先以稳定操作标识持久化；确认丢失时不重复派发，重启后会自动收敛遗留队列状态。
- Plan 展示顺序始终与实际执行顺序一致；连续提前结束的 Plan 运行不再错误进入待审核状态。
- 临时数据库/运行时失败先在后台重试并只记录 warning，多次失败后才记录 error，用户只看到可行动的产品级错误。

### Security

- Codex 主 Agent 的有效沙箱和审批策略不会越过当前项目权限选择，也不会被更宽松的全局配置意外提升。
- 持久化日志和支持包会再次脱敏结构化凭据字段、URL 用户信息、私钥及常见云端/Provider token。

## [0.2.1] - 2026-08-04

### Added

- 新增一键发布闸门：在创建版本标签前检查 `main`、CI、GitHub Actions Secrets 和版本一致性，并在构建完成后核对四个平台的 Release 资产与更新清单。

### Changed

- 正式发布在缺少 Windows/macOS 平台证书时按平台降级为未签名构建，不再阻断 Release；Latest 页面会明确标出未签名平台和安装风险，updater 完整性签名仍为必需。

### Security

- Windows 卸载清理只会终止 R-Code AppData 下符合受管命名规则的 MCP Host，并拒绝可能逃逸数据目录的 Bundle ID。

### Fixed

- 勾选删除数据后，卸载器会先释放 R-Code 受管进程并重试清理本地数据；只有仍被 Windows 或安全软件占用时才回退到重启后删除。

## [0.2.0] - 2026-08-04

### Added

- Plan 模式：任务目标、结构化 human-in-the-loop 问题、稳定 AppData Markdown 投影、按功能拆分的依赖待办和确认实施流程。
- Plan 确认后的可靠实施队列、重启恢复与失败重试，以及不会回滚工作区的二次确认取消流程。
- 增强审核：仅展示当前 Plan 的功能变更，支持功能/文件级接受与拒绝，并通过逆向三方合并保留同一文件中其他功能的改动。
- 中英文双 README 入口，补充 Plan/HITL、增强审核、并发恢复与隐私边界文档。

### Security

- Plan 模式禁止写工具、Shell、变更型 MCP 和委派；等待用户回答后会关闭同一 Run 的后续工具执行。
- 执行中的 Plan 没有活动功能时进入暂停态，直接写入、变更型 Shell 和 MCP 均 fail-closed，直到显式恢复被阻塞事项。
- 功能级拒绝使用路径有序锁、durable journal、rollback Blob 和原子替换；冲突时保持文件不变并要求人工处理。
- 删除会话/项目时按事务引用计数清理审核 Blob 和 UUID Plan 投影；启动清理不信任数据库提供的文件路径。

## [0.1.0] - 2026-08-03

### Added

- 基于 Tauri 2、Rust、React 和 TypeScript 的跨平台 AI 编程桌面工作台。
- 任务、会话分支、消息队列、流式时间线、回放、变更审核、验证与崩溃恢复链路。
- 原生模型 Provider 与 Codex CLI/MCP 协作，可按策略委派只读或完整访问的子智能体。
- 默认关闭的单机演进记忆：成功轮次自动复盘、全局候选审批、项目自动记忆、冻结快照注入与 AppData-only 管理页。
- 无密钥原生联网、可关闭的内置深度调研服务，以及带确认、凭据引用和官方 Registry 搜索的 MCP 管理。
- 带工作区路径边界、动态风险分级、审批和审计记录的统一 Tool Gateway。
- SQLite 产品状态与 JSONL 会话事件双存储，以及内容寻址 Blob、基线和回滚能力。
- 基于 PTY 与 OSC 133 的集成终端，支持原始输出增量读取和外部 CLI 会话解析。
- Windows x64、macOS Apple Silicon/Intel 与 Linux x64 的 GitHub Actions 发布矩阵及 Tauri 自动更新产物。
- Windows Authenticode 强制签名验收，以及自动生成的 CycloneDX SBOM 与第三方许可证清单。
- Windows 品牌安装器，支持自定义安装位置、快捷方式选项、真实阶段进度、取消保护和完成后启动。
- macOS 原生 traffic-light 标题栏、GUI shell PATH 恢复、可见 Codex 登录终端，以及 Developer ID 签名/公证打包脚本。
- GitHub 可直接预览的架构、发布、安全和隐私文档，以及版本一致性检查脚本。

### Security

- Provider 密钥保存到操作系统凭据库；旧版配置中的明文密钥会在启动时尝试迁移。
- R4 高危命令前置拒绝，R3/R4 授权不能保存为长期放行规则；子智能体默认只读。
- 文件工具在解析符号链接和非现存路径祖先后再次校验工作区 containment，并采用 fail-closed 行为。

### Fixed

- 自动更新源与 Cargo 仓库元数据改为当前 GitHub 仓库，避免客户端从错误仓库查询 `latest.json`。
- Windows 安装器、卸载器与应用程序使用 R-Code 图标，release 启动不再打开命令行窗口。

[0.1.0]: https://github.com/foritin/r-code/releases/tag/v0.1.0
[0.2.0]: https://github.com/foritin/r-code/releases/tag/v0.2.0
[0.2.1]: https://github.com/foritin/r-code/releases/tag/v0.2.1
[0.2.2]: https://github.com/foritin/r-code/releases/tag/v0.2.2
[0.3.0]: https://github.com/foritin/r-code/releases/tag/v0.3.0
[0.3.1]: https://github.com/foritin/r-code/releases/tag/v0.3.1
[0.3.2]: https://github.com/foritin/r-code/releases/tag/v0.3.2
[Unreleased]: https://github.com/foritin/r-code/compare/v0.3.3...HEAD
[0.3.3]: https://github.com/foritin/r-code/releases/tag/v0.3.3
