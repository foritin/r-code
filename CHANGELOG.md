# Changelog

R-Code 的用户可见变化记录在此。格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

提交和 Pull Request 是实现级历史；本文件记录每个发布版本对用户和运维者有意义的变化。

## [Unreleased]

### Added

- DeepSeek 复杂任务 Plan 建议（Phase 0，默认关闭、证据门控）：DeepSeek 主代理识别到复杂请求时可调用新工具 `propose_plan_mode` 提交受控信号（multi_subsystem / migration_or_data / design_decision / expensive_rollback / multi_stage_verification），宿主按固定本地化模板生成客户弹窗——「直接继续 / 先制定计划」二选一，拒绝（含关闭与 Escape）后同任务分支持久安静、仍可手动选择 Plan；建议以 SQLite `plan_entry_offers` 聚合持久化（同请求键唯一、同分支一次预算、revision CAS、Provider 快照比对 supersede、崩溃窗口显式重试），接受事务原子完成建 Plan、切模式与续接。同一真实请求获得稳定 `origin_request_key`（direct/queued/steer/host-continuation 全路径），普通客户设置只新增一个 DeepSeek 专属开关 `planning.suggest_complex_tasks`；「Plan 模式会做什么」指引手册可从弹窗、DeepSeek 设置卡与 Help 菜单打开（替换决策弹窗而非叠加）。
- Plan 原生双轨（DeepSeek 证据门控）：符合条件的 DeepSeek Plan 冻结 5→8 只读目录（bootstrap：glob/plan_publish/read_file/request_user_input/search_files；resident 追加 git_status/list_files/load_skill）与最小上下文（不注入本地时钟、委派说明与 memory），目录阶段以 `plans.catalog_phase` 为权威、经宿主 CAS 确认持久化后才允许下一轮请求，clear context/fork/重启不回退；执行硬门保持原生状态机（只读调查 → plan_publish → 用户批准 → 实施），隐藏的 edit/Shell/MCP/委派调用在副作用前硬拒。Plan 运行 profile 在创建时冻结（UI plan_create / 显式 enter_plan_mode / 建议接受共用同一 resolver），其他 Provider 与子代理不受影响。
- 真实 DeepSeek 三臂评估基建（`eval/plan-eval/`）：25 个冻结 case（5 类 × 5，初始测试红 / oracle patch 绿）+ 40 个路由 probe（20 simple + 20 complex），预注册发布门与 corpus sha256 锁；`plan_eval` 二进制提供 eval-only 自动 accept/approve 的三臂/路由运行器（非 dry-run 只认原生 DeepSeek，dry-run 记录不得作为证据）；`score.mjs` 只消费 raw results 生成 manifest，`verify-manifest.mjs` 独立重算每个数字；manifest 经 `build.rs` 嵌入并由 `plan_policy` 运行时重验，未通过前 validated 恒为关闭。

### Changed

- 自动复杂度路由不再调用 `enter_plan_mode`：该工具只保留给用户显式选择 Plan 或明确要求先做结构化计划的路径；Agent 模式提示词按建议资格分档（eligible DeepSeek 注入建议策略，其余保持显式入口）。
- 旧 `first_round_*` 实验档位从客户设置移除：`orchestration.first_round_catalog` / `first_round_promote_on` 仍可解析但写入时返回明确诊断警告，不再映射为新语义；首轮工具清单锚定实验（含 `plan_ready` 规划门）整体下线，Plan 原生目录取而代之。


- 请求信封审计（诊断，默认关闭）：新增配置 `diagnostics.request_audit`，开启后每轮模型派发前向 `sessions/request-audit/{会话id}.jsonl` 追加旁路快照（system/tools/messages 指纹、工具清单名单、hosted 工具与实际派发的输出预算），并做重建自检（只记录不阻断）；canonical 会话文件与既有读者零改动。配套只读命令 `request_audit_counters` 可读取（追加数, 不一致数）计数。「设置 → 诊断」新增「请求构成审计」开关。
- 首轮工具清单锚定实验（opt-in，默认关闭）：新增配置 `orchestration.first_round_catalog`（`full` 默认/`readonly`/`editor_pair`）与 `orchestration.first_round_promote_on`（`either` 默认/`tool_call`）。非默认值时主代理首轮只看到收窄的工具清单（每次请求携带的 tools 菜单，与项目文件夹无关），首轮出现任意回复（或按配置：首次工具调用）后恢复完整清单，会话级粘性保证每会话至多一次清单变化；清单裁剪只是呈现层，工具执行与审批边界不变。「设置 → Agent 编排」新增「首轮工具清单锚定（实验）」卡片。
- 子代理派生确认回路：新增 `plan_subagents` 工具，主代理要并行开出第 2 个及更多子代理前，必须先提交「每个方向一条条目」的计划并带 `confirm` 确认；运行时返回数量、角色槽位分布与同角色警告的分析，超出确认计划的 `delegate_task` 会被拒绝并引导修订计划。首个子代理仍可直接派生，子代理受阻时开孙代理（深度 2）的能力保持不变。
- 子代理任务提示词全程可见：委派事件 scope 携带 goal 有界摘要，宿主在子代理独立会话记录首位落盘任务全文（优先取 `delegate_task` 审计输入，外部路径回退摘要）；子代理详情新增「任务 · 来自主代理」卡片，转录按任务样式渲染主代理下发的 user 消息，不再静默丢弃。
- 时间线新增可展开的「子智能体」事件行（原型 C 的藏青高亮）：行首图标 + 藏青子代理名称 + 任务提示词摘要 + 运行状态计数，展开后为既有状态芯片；藏青色收敛为 `--subagent-run` 设计令牌，与目录状态环、运行中芯片描边同源。
- 「首轮工具清单锚定（实验）」卡片新增「指引手册 →」入口：随应用内置的手册浮层（这是什么 / 三个档位 / 恢复时机 / 推荐组合 / 如何验证 / 边界与事实），档位卡与设置下拉共用同一份枚举文案，离线可用且不随文档站漂移；页脚「去开启请求构成审计」一键切到「诊断」页并闪烁高亮目标开关。
- 首轮工具清单锚定的生效过程在时间线直接可见：会话首个受限请求派发时出现「工具清单已收窄（锚定期）」行（档位 + 收窄/完整工具数），锚定期结束时出现「锚定期结束 · 工具清单恢复完整」行；两行随会话记录持久化，重开任务详情仍可回放，不锚定（完整清单）时零噪音。收窄回合内托管联网工具（web_search / web_fetch）一并从请求剥离，「读写最小对」真的只剩 read_file + edit；审计 RequestHeader 的 hosted_tool_names 同步反映剥离，晋升后联网工具随配置回归。
- 规划门档位与规划完成晋升信号（opt-in 实验扩展）：`orchestration.first_round_catalog` 新增 `plan_gate`（首轮起零工作工具），`orchestration.first_round_promote_on` 新增 `plan_complete`——收窄目录注入唯一「门铃」工具 `plan_ready`（worker 侧拦截执行，不转发网关、无审批），工具剥夺跨回合、跨 run 持续（会话级粘性），直到模型自己调用 plan_ready 声明规划完成才恢复完整工具目录；纯文本终答自然结束 run，粘性保持到下一个 run 的首轮。「设置 → Agent 编排」的锚定卡片新增总开关滑纽（开 = 记住的档位，关 = 完整清单现状，档位记忆保留在本地）。

### Changed

- 循环重放护栏从两轮放宽为连续三轮：相邻两轮工具调用完全一致不再立即停止——上下文压缩可能裁掉上一轮工具结果，模型相邻轮重发同一读调用是合法恢复行为；连续第三轮原样重放才触发，错误轮不延续连胜（仍由同错连败统计）。触发后的收尾行为不变：一次无工具总结后结束、工作区改动保留。
- 同目标子代理硬阀门：`plan_subagents` 确认时，批内完全相同的 goal（忽略大小写与空白）或与仍在运行的子代理相同的 goal 会以 `needs_revision` 拒绝锁定，必须合并/改写条目后重新确认；`delegate_task` 对与运行中子代理完全相同的目标直接拒绝并引导先 `collect_subagents` 等待结果——不再只给软警告，杜绝「多名同角色子代理」的滥用观感。
- `bash` 成功执行约 3 步及以上的串联命令（`&&`/`||`/`;`/`|`，引号内不计）时，在结果中附加一次拆解提示，引导逐次调用、逐步检查输出；失败/超时结果不叠加提示。

### Changed

- 文件行图标改为真实扩展名图标并移到文件名前：常用扩展名（ts/tsx/js/rust/css/html/md/json/yaml/python/docker 等 40+）使用 vscode-icons 图标集（MIT，经 Iconify 分发），未知扩展回退按语义分色的通用文档形；布局由「图标在行首」改为「动词 → 图标 → 文件名」。
- 时间线计划卡改为待办卡：默认折叠的计数卡（当前步骤标题 + n/N 计数 + 圆形箭头），展开后为 ✓ 完成 / → 进行中 / ○ 待处理三态步骤列表。
- 执行中文字采用「霓虹闪烁与余辉」效果：运行状态行（处理中 + 时长）与进行中工具卡动词在白光瞬时曝光后衰减为暖橙余辉，流式光标改为白→暖橙渐变带光晕；`prefers-reduced-motion` 下全部停用。
- 子智能体目录对齐原型 C+：分组标题改为「正在运行 / 已结束 · N」，目录行的 40px 头像改为状态标记（运行中藏青环 / 已完成 ✓ 圈），行密度收紧；运行中子代理芯片与状态环统一藏青高亮。
- 会话头部项目名与状态以 inset chip 呈现（边框 + 圆角 + 内嵌底色），对齐原型头部的项目/分支 chip 观感。
- 主交互页对话流去除菱形节点与左侧轨道线：工具卡、轮次折叠条与定位锚点上的旋转方块标记全部移除，头部计划快捷条的状态菱形改为圆点，活动行回归安静的图标单行。
- 文件活动不再折叠成「已编辑 N 个文件」：每个文件独立成行，带按扩展名着色的类型图标，行内即时显示 `+N −N` 行数差异（编辑按 old/new 片段行数，写入按整份内容行数），超过 6 个文件折叠尾部按需展开。
- 读取/查看文件的活动与编辑同流呈现：同样渲染为彩色类型图标的文件行（动词「读取/查看」），目录清单类仍归「探索」行；探索行与思考行文案对齐原型（「已探索 N 项」「思考过程」）。
- 完成轮次摘要条改为「已处理 N 步 · 耗时 3m 12s」紧凑格式（原「耗时 1小时 34分」），时长格式与运行态统一为 `5s / 1m 42s / 1h 02m`。
- 会话输入框改为居中悬浮卡：宽度 `min(760px, 100%)`、16px 圆角与投影，不再通栏贴底；待发送队列宽度与浮层卡对齐。主操作按钮改为带文字的胶囊（运行中「■ 中断」，空闲「发送」），替换纯图标方块。

### Fixed

- 小助手（伴生窗）透明区的点击穿透改为自愈循环：此前原生 IPC 连续失败数次后穿透会一次性停摆，整窗（含透明区）从此变成吞掉背后所有点击的「隐形大框」；现在降级期间保持宠物可点可拖并持续慢速探活，原生调用恢复后自动回到 80ms 命中判定。同时修正三类误判来源：WebView2 的 `visibilityState` 滞留 hidden 时改用原生窗口可见性复核；窗口几何每约 1 秒主动重取（原生移动/DPI 事件可能漏发，旧坐标会把命中框整体偏移成「别处被挡、宠物点不中」）；`visibility:hidden` / `display:none` 的隐藏元素不再认领原生交互。原生拖拽结束但未派发 pointerup 时，移动停顿约 0.6 秒即恢复穿透判定（原只靠 3 秒兜底）。

## [0.9.1] - 2026-08-17

> **预上线版本（Pre-release）**
>
> 0.9.1 是 R-Code 1.0 正式上线前的候选版本，用于验证 DeepSeek V4 推理协议连续性、子代理候选池与跨平台凭据。它不会标记为 GitHub Latest，也不会进入稳定版自动更新通道；升级前建议备份应用数据。

### Added

- 火山方舟 Coding Plan（Anthropic/OpenAI 两个口）与 Agent Plan、Kimi For Coding 增加按「厂商 + 模型族」实测冻结的方言适配：thinking 词表与发送形状、temperature 策略、推理强度档位、流式 usage、上下文/输出能力与 User-Agent 均按真实接口行为发送；Ark 模型列表同步时过滤 `Shutdown/Retiring` 条目，前端为 Ark 各套餐口与 Kimi Coding 暴露正确的思考/推理强度入口。
- 火山方舟 Coding Plan 与 Agent Plan 新增 Responses 线路：`/api/coding/v3/responses` 与 `/api/plan/v3/responses` 接入 `ArkResponses` Provider，reasoning 以 Ark 的明文 summary 形式保存并回传（`SummaryReplay`），推理强度按探针冻结的 low/medium/high/xhigh/max 档发送，不支持 none/minimal 时安全省略。

### Changed

- DeepSeek 的 `deepseek-chat` 按服务端实测别名声明 1M 上下文窗口与 393,216 单次输出上限，避免过早触发压缩。
- DeepSeek V4 新增“智能平衡”推理策略：默认关键判断使用高档，昂贵的纯只读探索后仅给下一轮一个快速取证档，写入、命令、验证与最终收束自动恢复高档；Pro 在 Chat/Anthropic 口用关闭思考替代其不支持的 low 档，Responses 口的快速取证轮保持 low 档思考开启以维持 reasoning 连续性，三者均使用各自合法参数。显式开启、关闭或固定 high/max 始终优先，自动/手动上下文压缩与长子代理报告在智能模式下使用快速总结档。
- 原生 R-Code 子代理接入与主 Agent 相同的分层压缩闸门：按输入可用窗口（上下文窗口减去输出预留）在 75% 提示、85% 摘要折叠并连续两次防抖，压缩只改写交给模型的投影，canonical 完整证据仍用于最终报告重建。
- 网络/MCP 政策按 run 内实际工具能力分层注入：网络条款常驻，MCP 管理条款仅在有 MCP 生命周期工具时注入，MCP 使用条款仅在已启用 `mcp__` 直连服务时注入；未启用任何 MCP 服务时每请求少付约 1,300 字符的固定开销。
- MCP 草稿创建解除工作区绑定：MCP 是全局配置，未挂工作区的会话也能调用 `mcp_create_draft`，`source_path` 改为接受任意绝对路径；原有暂存目录清理契约与“创建后保持关闭”边界不变。
- `mcp-creator` 技能精简为“创建 + 声明凭据变量名”：需要凭据时只声明 stdio 的 `environment_names` 或 HTTP 的 `header_names`，变量值由用户在“设置 → 工具与连接”的 MCP 条目中点击“配置”输入。
- 纯聊天会话默认以用户主目录作为工作区：本地文件与终端工具以主目录为根、写入和命令仍需批准，Codex 主引擎同样可以直接在聊天会话中运行。

### Fixed

- “上下文已超过模型上限”与“手动压缩单块超过上限”两类错误改为结构性防护：发送前按 1.15 安全系数做硬闸门（超窗先强制折叠、失败则保留首尾并裁剪中段），收到服务端上下文超限时自动压缩重试一次；手动压缩对超长消息/工具对与超长摘要一律切分或重试截断，不再以“本次未应用压缩”拒绝。
- OpenAI Responses 线路补齐与 Chat 一致的 408/429/5xx 重试与 120 秒流空闲 watchdog；所有模型传输层统一连接超时与 User-Agent，压缩摘要与记忆评审的旁路请求增加 120 秒 deadline。
- 小助手补齐 Windows 原生适配：恢复可编译/可打包的 Tauri feature 配置，防止 Alt+F4 销毁助手后无法重新开启，设置开启前会确认原生窗口存在并在失败时自动回弹；多显示器混合 DPI 定位、隐藏后的鼠标穿透与动画轮询、会话跳转后的 Windows 焦点容错也同步修正。
- DeepSeek V4 Pro Responses 智能平衡的快速取证轮不再用 `none` 关闭思考——那会让该轮的工具调用没有 `reasoning_text`，下一轮切回高档时因未回传而被服务端 400（`reasoning_text must be passed back`）；改为 `low` 档保持 thinking 开启，主 Agent 与原生子代理共用该修复。

## [0.9.0] - 2026-08-12

> **预上线版本（Pre-release）**
>
> 0.9.0 是 R-Code 1.0 正式上线前的候选版本，用于验证跨平台凭据与数据目录、数据库升级、长会话、子代理日志和终端隔离。它不会标记为 GitHub Latest，也不会进入稳定版自动更新通道；升级前建议备份应用数据。

### Added

- 新增内置 MCP Creator：Agent 可引导用户把 MCP 服务生成到 R-Code 全局应用数据目录中的草案区，不污染当前项目，也不会擅自启动；用户检查后可在设置中显式启用。
- Plan 工作台支持结构化 Markdown 描述，任务目标、步骤、依赖和执行结果不再挤成单段文字。

### Changed

- 长上下文改为保留不可丢失的 canonical 完整历史，并把给模型使用的压缩投影单独持久化；高水位压缩会汇总全部旧证据、保留精确尾部，摘要为空或被 `max_tokens` 截断时拒绝替换，避免多轮后因永久丢失工具结果而降智。
- `read_file` 对大文件采用约 100 KiB 的可靠分页和行内字节游标，明确返回续读位置；主 Agent 可批量读取相关文件，减少重复工具往返，同时不会把超长单行或文件尾部静默截断。
- 主时间线只挂载最近 80 轮并按流式增量重建尾轮；子代理日志首次加载最近 80 条，之后按游标增量轮询并可向前加载。屏外 Markdown 延迟布局，折叠的长代码和工具输出只挂载前 16 行，降低长窗口滚动、展开和流式输出卡顿。
- Provider 配置请求在前端跨组件合并并复用缓存，切换会话不再重复读取持久化凭据；DeepSeek、Kimi、自定义 OpenAI Chat、Responses 与 Anthropic 兼容链路完整保留长工具证据和 thinking/reasoning 输出。
- 子代理任务页按运行状态分组并可整体收起；命令、文件编辑、工具调用及审核文件默认使用可折叠卡片，文件链接在工作台追加标签而不替换子代理页面。

### Fixed

- 修复数据库 schema 26 已存在但迁移元数据缺失时，启动直接报“expected 27”的问题；迁移现在会识别实际结构并安全补齐 schema 27。
- 修复 Plan 并发完成时的 `stale Plan revision` 与重复 `completed -> completed` 状态机错误；持久化 receipt 让完成、重试和重复回执幂等收敛。
- 终端进程、输出、尺寸和停止操作按 task/session 隔离，切换会话后不会再读取、输入或结束其他会话的终端。
- 修复子代理日志分页时跨页 `ToolCall`/`ToolResult` 无法配对、结果穿过空页后丢失、半写 JSONL 或日志替换后游标失效，以及 message/reasoning 正文被 20,000 字符静默截断的问题。
- 修复长时间线每个流式 token 都扫描和复制完整历史、长 reasoning 即使收起仍挂载正文、单窗口信息过多时上滑明显卡顿的问题。
- 修复 Provider 设置与会话切换期间重复凭据查询造成的卡顿，并确保配置更新后缓存会正确失效而不会继续使用旧模型或旧凭据引用。
- macOS Provider API Key 与 MCP 凭据改用应用数据目录内的 ChaCha20-Poly1305 加密文件，主密钥与密文权限均为 `0600`，避免开发重编译和升级后反复触发 Keychain 授权；旧 Keychain 项不会被自动读取，需重新输入或使用环境变量。
- 统一 macOS 桌面、启动期日志、托管 RTK 与独立 MCP Server 的应用数据目录，避免 Bundle ID 差异产生第二套空数据库、RTK 无法被 Codex 子进程发现或支持包缺少日志。

### Security

- macOS 加密凭据采用随机独立主密钥、每次写入新 nonce、认证加密、跨进程锁和原子替换；Windows/Linux 保持原平台凭据后端。诊断日志、Provider 错误正文和 MCP 配置继续经过脱敏，不回显 API Key、令牌或私钥材料。
- 模型压缩只改变可重建的投影视图，不覆盖审计所需的完整会话与工具证据；第三方 MCP 与子代理仍继承主 Agent 的权限边界、审批和审计策略。

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

- DeepSeek 线路长会话显著提速降价：请求前缀改为逐字节稳定以命中 DeepSeek 字节级自动前缀缓存——system prompt 移除秒级时间戳并在 run 内冻结复用；时间（分钟级）、任务上下文、Plan 模式与委派提示统一改为尾部消息注入且不落历史；工具列表按名称排序，Codex 可用性判定在 run 内冻结；历史严格只追加，悬挂工具调用的修复结果落盘固化。真实 API 14 轮实测尾部命中率 93%（基线存档 `docs/archive/deepseek-cache-baseline.md`）。
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
- 会话首次附加工作区后即永久锁定该绑定；需要使用其他目录时新建会话，避免历史变更、回滚和工具访问边界被静默改到另一项目。
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
[0.3.3]: https://github.com/foritin/r-code/releases/tag/v0.3.3
[0.9.0]: https://github.com/foritin/r-code/releases/tag/v0.9.0
[Unreleased]: https://github.com/foritin/r-code/compare/v0.9.1...HEAD
[0.9.1]: https://github.com/foritin/r-code/releases/tag/v0.9.1
