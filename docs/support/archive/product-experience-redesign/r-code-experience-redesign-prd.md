# R-Code 产品体验重构与平台补差 PRD / AI 实施 Checklist

> 文档状态：`frozen`（仅表示执行合同与摘要已固化；D0-01 及产品代码均未完成）
> 执行合同：`prd-to-ai-worklist` v1.1.0
> 目标分支：当前 `dev`；不得新建或切换分支，除非用户另行要求
> 设计基线：[完整 App 可点击 Demo](./prototype.html) · [设计说明与 Demo 边界](./README.md)
> 固化清单：[r-code-experience-redesign-freeze.yaml](./r-code-experience-redesign-freeze.yaml)
> 唯一完成状态：本文 §10 主 Checklist；任务卡、TaskPacket 与证据不得维护第二套 Checkbox

## 执行导航

- 首次执行：§0 → §2 → §4 → §8 → §9 → §10 → 首个 ready 任务卡。
- 中断恢复：`artifacts/ai-tasks/current.yaml` → §11 对应任务卡 → 已归档证据。
- 需求与终态：§1；冻结决策：§2；仓库事实：§3；机器合同：§4。
- UI / 状态矩阵：§5；平台延续合同：§6；质量门禁：§7。
- 统一验收：§8；依赖与并行：§9；唯一 Checklist：§10；详细任务卡：§11。
- 连续执行、证据与外部放行：§12–§14。

## 0. AI 执行入口

<!-- AI_WORKLIST_VOLATILE_START -->

- 当前进度：`42 / 42` 项完成；M0 基线已冻结（`m0-baseline.json` 十腿四态），M1–M8 全里程碑闭环（各 `--task` 验证器通过于 main@82c8c5c，2026-08-27）；M9 族 4 项于 2026-08-28 收口（M9-01 4/4、M9-02 3/3、M9-03 3/3、M9-04 2/2），顶层累计门 `--through M9 --profile implementation` 167/167 passed @main@82c8c5c。M5-02.A8–A11 曾虚报通过，2026-08-28 补齐真实聚合腿后转绿。D0-01 与 M0-01 此前已勾选。
- 下一执行项：无——worklist 全部 42 项完成；候选/production profile 验证与外部 gate 待用户授权（见 M9-04-candidate-gates.yaml）。
- 当前任务包：`artifacts/ai-tasks/current.yaml`（worklist_id `product-experience-gap-closure`）；已完成包归档于 `artifacts/ai-tasks/evidence/product-experience-gap-closure/`。

<!-- AI_WORKLIST_VOLATILE_END -->

### 0.1 首次启动

1. 只读检查 `git status --short --branch`、当前 revision、完整未跟踪文件、Node/Rust/Tauri 运行时和已有测试；当前脏工作区全部视为用户资产。
2. 读取本节、§2、§4、§8、§9、§10 和首个 ready 任务卡；不需要每轮重读全文。
3. 先核对 D0-01 的 production Settings source inventory / symbol resolution / provenance proof；组合 gate 只证明执行合同可冻结，只有 `d0_semantic_proof=passed` 且 D0 的截图/链接/可达性证据全部通过后才允许勾选 D0 并进入 M0-01。已有 `artifacts/ai-tasks/current.yaml` 若属于已完成 worklist，只能按其原合同归档，不能静默覆盖。
4. 从编号最小且依赖已通过的未完成 MUST 开始；建立/恢复 TaskPacket 后直接实施，不在里程碑或文档完成处等待人工确认。
5. 每个可验证子步后更新 TaskPacket；本任务断言、累计门禁和证据都成立后，才能勾选 §10 的唯一 Checkbox。

### 0.2 续跑

1. 读取 `current.yaml`、对应任务卡、相关 RequirementRef 和既有证据，不重新规划全部任务。
2. 对照真实工作区核对 `changed_paths`；用户新增或未提交改动仍是资产，不允许 reset、覆盖或顺手清理。
3. 对 `completed_assertions` 运行最小 smoke；从首个未完成 step / assertion 继续。
4. packet 与代码不一致时以代码、测试和可访问证据为准修正 packet，不能凭 YAML 宣称完成。

### 0.3 授权、并行与中断边界

- 允许：本仓库内可逆的源码、测试、fixture、脚本与文档修改；按 §9 启动文件域互斥的子代理并行实施。
- 不允许：提交、推送、发布、改写用户全局 Codex/Provider 配置、显示私有思维链、扩大权限、清除用户改动、真实生产删除或把密钥写入证据。
- 并行任务必须在 TaskPacket 声明 `owned_paths`；两个活跃执行者不得编辑同一文件。公共 DTO、migration、`ipc.ts`、全局 token 和根注册表由单一 owner 串行合并。
- 只有继续会扩大授权/产品语义、需要无法取得的凭据或第二主体授权、执行不可逆生产动作，或同优先级规范无法由仓库事实消解时才请求用户。
- 常规编译、测试、类型、视觉或 Provider mock 失败不是中断理由；先保存失败证据、定位、聚焦修复并复跑。

<!-- AI_WORKLIST_NORMATIVE_START -->

## 1. 背景、目标、终态与非目标

### 1.1 已确认问题

当前 R-Code 已具备相当完整的 Agent 事件、工具、子代理和工作台基础，但用户体验仍显粗糙，主要问题是：

- 多层全局 CSS 与后加载覆盖层同时争夺 token 和壳层权威；现有 `--fx-glass` 实际是不透明背景，视觉材质与信息层级不统一。
- Windows 点关闭按钮时有托盘就直接隐藏、没有托盘就退出；macOS 直接隐藏、Linux 直接退出，用户没有首次选择和可恢复的偏好入口。
- `settings_get.provider_status.ready` 只表示配置完整，不代表联网成功；子代理 Provider 的自动探测绑在设置面板 `useEffect`，API 探测会真实产生一次最多 8 token 的 Completion，可能计费与限流。
- `commentary`、工具生命周期和子代理事件已有底座，但正在运行的普通轨迹与右侧 Summary 默认密度过高，主 Timeline 与工作台重复展示工具审计。
- 子代理入口下方的全局工具调用列表没有形成新的决策价值；用户更需要父子关系、阶段、Attention、改动与验证。
- 旧 gap plan 覆盖基础稳定性、Worktree、双语、Browser、Automation，但没有 Checkbox 或证据；当前 `dev` 只能证明这些模块有实施迹象，不能宣称全量闭环。
- `codex-rich-interaction` 的历史证据对应 2026-08-25 的特定 revision；其 38/38 合同可作为已有基线，但当前 dirty `dev` 仍需回归验证。

### 1.2 产品目标

1. 建立一个克制、有高级材料感、长时间可读的 R-Code 桌面设计系统和任务工作区。
2. 让关闭窗口行为可选择、可记忆、可在设置中重置，并保证托盘/Companion/退出清理不留下不可恢复进程。
3. 把 Provider 连通性提升为 Host 级、非阻塞、可缓存、可诊断的全局健康能力。
4. 让模型在关键阶段给出充分的公开反馈，同时把重复工具轨迹自动收纳，失败/审批/提问/final 始终可发现。
5. 重构执行台与子代理信息架构，去掉重复的全局工具审计，强化状态、协作、改动和验证。
6. 在不丢失旧计划安全边界的前提下，完成 Worktree、Browser、Automation、全量 i18n 和跨平台发布闭环。
7. 把 Settings 提升为完整、可搜索、可键盘操作的一级场景，用明确来源标签区分真实能力投影、本轮新设计和规划 Demo，并与 Shell 共享同一运行状态与持久化权威。

### 1.3 规范性需求

#### 设计与壳层

- **R-DES-01（MUST）**：原型、状态矩阵和实现合同必须可复现、可验证，并明确 mock、外部依赖与未验证边界。
- **R-VIS-01（MUST）**：建立唯一语义 token / material 权威层；玻璃只用于空间分层，正文和高密度列表必须有不透明可读 fallback，不再新增最终覆盖 CSS。
- **R-SHELL-01（MUST）**：重构 Topbar、Rail、任务列表和 Room 信息架构；任务状态只读取统一 `TaskStatusView`，`unread_count` 不覆盖真实 Attention。
- **R-COMP-01（MUST）**：Composer 默认只展示 Agent、Model、Send；Provider、reasoning、permission 收入运行配置；Steer/Queue 语义明确且全键盘可达。
- **R-COMP-02（MUST）**：发送、运行中追加/排队与停止必须是不同语义；Enter 在运行中不得触发停止，停止动作必须有稳定的危险态视觉、清楚说明影响并防止状态切换瞬间误触。

#### 完整 Settings Scene 与 Demo 来源

- **R-SET-01（MUST）**：Settings 是与任务工作区并列的完整一级场景，固定提供 12 个可路由页：模型服务、Agent 编排、子代理配置、工具与浏览器、知识与指令、权限、隐私与安全、外观与语言、通知、启动与关闭、更新、诊断；支持跨页搜索、命中跳转、返回原任务与焦点恢复，窄屏不得退化为不可达长弹窗。
- **R-SET-02（MUST）**：设置页和完整 App Demo 必须在卡片级区分 `production_existing`（生产现有能力）、`new_requirement`（本轮新增需求）与 `planned_demo`（规划 Demo）。混合页面逐卡标注；`planned_demo` 不得冒充已接线能力，产品实现中未闭环入口必须同时受前后端 feature flag 保护。
- **R-SET-03（MUST）**：离线 Demo 必须从 App Shell 到工作区、Settings、浮层和返回路径完整可点击；可见控件要么产生确定性状态变化，要么明确说明 Demo 边界，不保留无响应假按钮。Demo 只使用脱敏 fixture 与会话内状态，不读写真实密钥、用户配置、工作区文件、网络或 OS 权限。
- **R-SET-04（MUST）**：产品实现中的 Settings、Topbar、空白任务、Composer、Workbench 与系统 adapter 必须消费同一 Provider、关闭偏好、主题/语言、通知、Updater 和诊断状态源；设置提交定义 `immediate | next_run | next_restart` 生效范围，失败保持旧值并给出可恢复错误，不允许每个页面维护互相漂移的副本。
- **R-SET-05（MUST）**：session 及其 Run、stage、Provider、tool、subagent 状态使用统一 glyph 词汇；只有 `running | checking` 使用不带成功/警告色含义的中性 spinner。`queued | waiting_input | waiting_approval | completed | failed | cancelled | skipped` 必须使用不同的静态图形和文字，Agent 身份色不得承担状态语义；`prefers-reduced-motion` 下 spinner 变为静态缺口环并保留状态文字。
- **R-SET-06（MUST）**：12 个设置页及其跨页闭环必须通过 960×640、1280×800、1440×900，亮/暗主题，100/125/150/200% 缩放，键盘、读屏、中文 IME 与 reduced-motion 门禁；至少覆盖 Provider 编辑/测试→全局健康、关闭偏好→关闭→托盘恢复、通知授权→测试、Updater 检查→下载→重启旁路、诊断自检→支持包预览，以及规划 Demo 不改变实现状态。
- **R-SET-07（MUST）**：Settings 的页面、信息架构、路由、区块和控件允许重构、合并或迁移，但 D0-01 从当前 `dev` 生产源码只读发现并冻结的 `settings-capability-baseline` 是现有功能下界。每个现有字段、动作、只读状态、副作用、默认值、值域、生效范围、持久化 authority、IPC/Host 命令、权限、可见性和正向/失败/禁用语义，必须以稳定 `CapabilityID` 追踪到 `preserve | merge | migrate | explicitly_retired`、正式目标 Pane/control、TaskID 与 required AssertionID；`planned_demo` 不能替代现有能力。未经独立 RequirementRef 与用户批准，`explicitly_retired` 禁止；`merge | migrate` 必须证明旧值无损映射、旧 route/deep-link/config key/enum/IPC 兼容读取、迁移幂等、失败保留旧值、upgrade→downgrade→upgrade 往返不丢数据且可回滚。图片路由必须保留三条精确语义：confirmed multimodal 原图直发；`unknown + 用户显式 OCR` 才提取文本；`unknown/text-only + 已确认多模态且配置完整的 helper` 才调用 helper，helper 配置不完整、unknown/text-only 或失败时整批不发送，且 helper 失败绝不自动降级 OCR。Codex cancel 必须把 operation/process ID 与 generation 交给 Host，并在底层进程 terminate ACK 后才进入 `cancelled`；有界超时进入 `cancel_failed`，stale generation 不得回写。任一基线能力缺失、孤儿、不可达、变为 no-op、无合同改变默认值/作用域/副作用，或 required 证据缺失，统一 Verification Harness 必须非 0。
- **R-SET-08（MUST）**：三层 provenance 是逐能力的可审计事实，不是页面装饰；只有 source snapshot 已绑定当前 revision、源码 symbol/handler/config authority 已解析且正向/失败语义可定位的项才能标记 `production_existing`。Settings 语义门禁必须输出 `production_existing | new_requirement | planned_demo` 计数，且 `empty_provenance=0`；结构化 baseline、HTML selector 或旧 gate 自报通过都不能单独完成 D0-01。
- **R-SET-09（MUST）**：12 页 Settings 共用一个 revision-aware lifecycle：读取必须显式建模 `uninitialized | loading | ready | stale_last_good | failed | retrying`，写入必须建模 clean/dirty/saving/revision conflict；领域状态机可以独立，但页面不得各自伪造 loading/success。失败不能用空配置冒充成功。有 last-good 时只读展示并标记 stale，无 last-good 时显示错误与 retry；写失败保留持久快照和 dirty draft。Host CAS 冲突必须同时保存 local draft 与 fresh Host snapshot，并且只能通过显式 `discard local | reapply local onto latest revision | field-level merge with preview` 三路恢复；每条路径产生新的 base revision，离开有 discard 确认，任何 retry/refresh 不得静默覆盖较新的本地草稿。
- **R-MCP-01（MUST）**：MCP 保存与启用是两个独立动作：新配置先保存为 disabled，启用时只能使用 Host 返回的 exact launch preview 与一次性 token，在独立 `alertdialog` 审核后再次确认消费 token。existing `server_id` 是稳定不可变主键，编辑 UI 只读显示且 Host submit 再校验；改名只能 create-new 后 explicit remove-old，credential state/value 不得推断迁移。编辑/测试不得消费 token；取消、过期或 config revision 改变必须失效并恢复触发器焦点，客户端不得从表单自行拼 launch plan。
- **R-GUIDE-01（MUST）**：Provider、Plan、Subagent、Image 四个 GuideSheet 必须各有独立入口、内容与稳定深链；支持焦点陷阱、Escape/backdrop 关闭、关闭后回到原触发器，以及从 Settings 搜索结果进入后可达，不得以 Provider Guide 代替其余三项。
- **R-TOOL-01（MUST）**：Shell 设置保存 `execution.bash_shell_path` 后 apply mode 为 `immediate`：Host 同步更新 gateway override 并失效 shell cache，下一次工具调用立即使用新值；失败保留旧 override 与 dirty draft，不得误标为 next session/restart。

#### 关闭与 Provider 健康

- **R-CLOSE-01（MUST）**：所有主窗关闭入口（自绘 X、Alt+F4、原生 CloseRequested）进入同一 Host 状态机，持久偏好为 `ask | hide | quit`，重复事件不得叠加弹窗。
- **R-CLOSE-02（MUST）**：`ask` 展示“隐藏/最小化到托盘、退出、取消、不再提示”；只有完成选择时保存偏好，设置页可重置；托盘/Dock 恢复入口不可用时禁止隐藏。
- **R-CLOSE-03（MUST）**：Tray Quit、Updater restart 与 OS shutdown 绕过再次询问；退出必须由同一有界 ShutdownCoordinator 统一取消/回收主 Run、子代理、工具、Browser、Automation 和 Companion，并等待各子系统 ACK 或记录有界失败，不得只销毁主 WebView。退出 ACK 后所有 Run/child/tool/timer 都是 terminal、spinner 为 0；重启不得恢复旧 running 投影。
- **R-PROV-01（MUST）**：区分 Provider `configured` 与 `connectivity`；Host 在 Shell 可交互后后台探测默认 Provider 和已保存子代理槽位，不阻塞首屏、不静默 fallback。
- **R-PROV-02（MUST）**：探测复用 fingerprint、policy generation 与成功/失败 TTL、去重、有界并发、超时和退避；旧结果不得覆盖新配置；API exact-model probe 的潜在少量费用必须在设置中明示并可关闭。关闭 startup probe 必须递增 generation，并取消或忽略该 generation 的排队/在途结果；迟到结果不得写 receipt、不得合成 success，手动测试仍独立可用。
- **R-PROV-03（MUST）**：全局健康入口显示 `unknown | checking | connected | degraded | failed`，默认 Provider 失败才产生低干扰全局提示，其他失败留在选择器/设置页并可手动重试。
- **R-PROV-04（MUST）**：Provider 日志、通知、IPC、证据和支持包不记录 API key、完整请求/响应正文或 secret；失败不得自动替换 Provider、模型或 endpoint。
- **R-PROV-05（MUST）**：Host snapshot 是 canonical default/profile/reference 的唯一 authority；Provider mini、首屏、空白任务、Composer、Settings、health 和主 Agent 默认模型必须消费同一 revision。default 只在 Host ACK 后更新；Host reject 保持旧 default、旧 profile 与引用不变。默认 Provider 或任一持久子代理槽位引用的 Provider 不可删除；已配置默认 Provider 时不得显示“尚未连接”或短暂回退到其他 Provider/model。

#### 模型反馈、轨迹与子代理

- **R-FEED-01（MUST）**：保留 `commentary` 与 `final_answer` 分层；模型在首次实质工具批次、阶段变化、新证据或需要决定时公开播报，没有信息变化时不复读工具动作。
- **R-FEED-02（MUST）**：没有模型 commentary 时，宿主只能基于真实事件显示阶段、耗时、最近动作与计数，不得生成或推断私有 chain-of-thought。
- **R-TRACE-01（MUST）**：每个 Run 有稳定 Run Capsule；普通运行轨迹默认紧凑，用户可展开且本轮保持选择；完成后普通工具组自动折叠为数量、耗时和结果摘要。
- **R-TRACE-02（MUST）**：失败、审批、模型提问、用户输入、warning 和 final 不得自动折叠到不可发现；展开后事件顺序、脱敏与有界输出必须完整。
- **R-WB-01（MUST）**：右侧执行台一级信息架构固定为“概览 / 子代理 / 变更”；删除与 Timeline 重复的全局工具审计，工具详情回到对应 Run 或子代理 transcript。
- **R-WB-02（MUST）**：打开/关闭执行台只能改变 WebView 内响应式布局或抽屉，不得自动放大、缩小或重定位顶层窗口；存在活动子代理或待处理项时应直达对应聚焦视图。
- **R-SUB-01（MUST）**：子代理协作树展示父子身份、来源、状态、当前阶段、Attention、最近有效更新和停止入口；列表→详情→返回不丢主任务上下文。
- **R-SUB-02（MUST）**：候选池 receipt 过期时在委派前去重刷新；可选委派刷新失败时主代理继续并解释降级，强制委派失败时产生可恢复 Attention，均不得遗留假运行节点。
- **R-SUB-03（MUST）**：每个子代理 slot 独立持久化 `source | model | weight | prompt_template_id | prompt`；Prompt 不是全局共享卡。revision conflict 必须同时保留 local draft snapshot 与最新 Host snapshot，并要求用户显式 discard/reapply/merge，禁止当前刷新路径静默覆盖草稿。
- **R-PERM-01（MUST）**：同一 Run/工作区内同风险的只读请求可聚合授权并显示精确范围；写入、删除、外部网络和高风险操作继续按能力边界审批，不得借聚合扩大权限。
- **R-RUN-01（MUST）**：父 Run 进入 completed/failed/cancelled/interrupted 后，所有未终结的子代理、工具和计时器必须在 1 秒内级联到确定终态；实时与回放不得同时显示父失败、子仍运行。显式退出只有在同一 terminal projection 持久化并收到 Host ACK 后完成；重启读取该 projection 时不得复活旧 running 或 spinner。
- **R-SUM-01（SHOULD）**：完成态紧凑回答“做了什么、改了什么、验证了吗、还需什么”，并能跳转到 diff、验证、审批或子代理证据。

#### 共享基础与平台延续

- **R-ERR-01（MUST）**：所有新增用户错误使用 `UserFacingError { code, args, debug_detail? }`；普通 UI 不显示 `debug_detail`，只能显式复制技术详情。
- **R-I18N-01（MUST）**：zh-CN / en-US 覆盖 Shell、设置、关闭对话框、Provider 健康、Timeline、Workbench、Worktree、Browser、Automation、原生菜单/托盘/通知；key 与 placeholder 必须一致。
- **R-BIND-01（MUST）**：`TaskWorkspaceBinding` 是 Agent、Terminal、Files、Git/Review、Browser、Automation 与相关 MCP 的唯一工作目录入口；验证失败必须 fail closed，绝不回退原项目。
- **R-STATUS-01（MUST）**：`TaskStatusView` 统一列表、详情、通知、Automation Run 和执行台状态；优先级固定为 Archived → 等待用户 → 失败/绑定失效 → Review/Verification → Running → Queued → Idle。
- **R-NOTIF-01（MUST）**：前台使用应用内反馈，后台/失焦时只对审批、失败、ReviewReady、Automation 完成等关键事件发原生通知；权限拒绝时降级且不中断任务。
- **R-UPD-01（MUST）**：Updater 支持检查、说明、下载进度、安装重启/稍后重启；不自动安装，签名失败不破坏旧版本，重启路径与关闭询问状态机正确分离。
- **R-FLAG-01（MUST）**：未闭环的 Browser、Automation、Worktree 或新体验入口必须前后端同时受 feature flag 保护；不能显示不可用假按钮。

#### Worktree、Browser 与 Automation

- **R-WT-01（MUST）**：Worktree 默认关闭；开启后 Git 项目可按任务选择 Local/Worktree，非 Git 只能 Local；绑定含托管身份、repo、branch、`base_oid` 与 cleanup 状态。
- **R-WT-02（MUST）**：创建按“校验→创建→验证→持久化→事件”执行；补偿只能删除确认 clean、无新 commit 且由 R-Code 管理的目标，任何不确定都保留并产生 Attention。
- **R-WT-03（MUST）**：所有执行消费者迁移到 `TaskWorkspaceBinding`；路径缺失、repo 不匹配、junction/symlink 逃逸等全部拒绝，绝无 Local fallback。
- **R-WT-04（MUST）**：归档停止任务但保留 Worktree；clean/no-commit 可安全清理，dirty/new commit 保留并进入 Review，关闭开关不会把既有任务回退到 Local。
- **R-BR-01（MUST）**：Browser Runtime manifest 固定 Node/Playwright/Chromium 与 SHA-256；首次使用按需下载到 staging，校验后原子切换，支持并发锁、修复和 side-by-side 版本。
- **R-BR-02（MUST）**：每 Task 独立 Session/profile，多 Tab、进程树和崩溃状态真实；重启不自动拉起，删除 Task 清理 profile、截图和完整进程树。
- **R-BR-03（MUST）**：工具集覆盖 open/navigate/snapshot/screenshot/tabs/console/network-errors/close 与 click/type/select/press/scroll/wait；输出脱敏有界，不开放 raw eval、上传或下载。
- **R-BR-04（MUST）**：`file://` 受 WorkspaceBinding 限制；localhost 可浏览；外部 origin 精确授权；browse 与 interact 分离，redirect/popup/new tab/final URL 重新校验，read-only Automation 永不获得 interact。
- **R-AUTO-01（MUST）**：Automation Definition/Run 持久化，支持 once/hourly/daily/weekdays/weekly、IANA timezone、DST、idempotency、lease、不可变 snapshot、不重叠与只补跑最新一次。
- **R-AUTO-02（MUST）**：管理 UI 支持 CRUD、Run now、Pause/Resume、Cancel、History、Task/Worktree/Review 跳转；Provider/model/branch 不可用时失败，不静默替换。
- **R-AUTO-03（MUST）**：read-only 在 ToolGateway 注册阶段过滤写文件、Shell、mutating MCP 与 Browser interact；Prompt 注入不能获得写能力。
- **R-AUTO-04（MUST）**：isolated-write 每 Run 新建独立 Worktree，绑定失败不启动 Agent；关闭 Worktree 开关时暂停并需显式恢复。
- **R-AUTO-05（MUST）**：审批跨重启持久恢复、无超时自动批准；有变化保留 Review，无变化只在确认安全时清理；Browser 不获得后台特权。

#### 质量、安全与发布

- **R-ACC-01（MUST）**：亮/暗主题、960×640 最小窗口、1280×800、1440×900、100–200% 缩放、中文 IME、键盘、读屏和 reduced-motion 均可完成关键流程，无意外横向滚动。
- **R-ACC-02（MUST）**：390px 窄屏和触控输入下，主要动作与图标按钮可点击区域至少 44×44 CSS px，紧凑次要控件至少 32×32 且相邻目标不重叠；抽屉、GuideSheet、表单错误、MCP 审批和冲突恢复不得依赖 hover 或精确像素点击。
- **R-REL-01（MUST）**：实时与历史使用同一事件/状态投影；慢 UI、重复/迟到帧、断流、重启和恢复不能串 Run、丢终态或制造假成功。
- **R-REL-02（MUST）**：M5 累计门必须在同一 revision 上连续通过三轮，每轮从新的应用进程和隔离 fixture 状态开始；报告保留 round ID、revision、状态前后 digest 与全部命令退出码，任一轮失败、跨轮状态泄漏或通过后改码都使三轮计数归零。
- **R-SEC-01（MUST）**：raw reasoning、secret、cookie、authorization、token、Provider key 和未脱敏工具输出不得进入普通时间线、日志、诊断或证据。
- **R-ROLL-01（MUST）**：体验重构通过内部 feature flag 可回退到旧表现层，回退不改变底层事件、数据、权限或任务状态；证据通过后再移除旧路径。
- **R-RELSE-01（MUST）**：正式发布只在同一 commit 的前端/Rust/契约/迁移/视觉/三平台/安装包/Updater 门禁通过、无 P0/P1、文档一致且旧版本升级成功后进行。

### 1.4 Definition of Done

`implementation_verified` 必须同时满足：

1. §10 除仅属于 production external gate 的内容外全部任务 Checkbox 为 `[x]`，每项有真实证据。
2. 统一 Harness `--through M9 --profile implementation` 返回 0，required assertion 无缺失；前端、Rust、契约、迁移、安全、E2E、视觉与性能报告可访问。
3. 主任务可以完成 commentary → Run Capsule → 子代理 → 提问/审批 → final → diff/verification 跳转，普通轨迹默认收纳且失败不隐藏。
4. 自绘 X、Alt+F4 和原生 close 同路径；偏好跨重启；托盘不可用不隐藏；显式退出和 Updater restart 不重复提问，并只在 terminal projection + Host ACK 后完成，重启不恢复旧 running。
5. Shell 在 Provider probe 未完成、失败或离线时仍可操作；TTL 内不重复请求，fingerprint/policy generation 变化使旧结果失效，startup opt-out 不合成 success，失败不 fallback；canonical default 仅 Host ACK 后更新且所有消费面同 snapshot。
6. Worktree、Browser、Automation 的正向、拒绝、恢复与清理路径通过，原项目不被越界修改。
7. raw reasoning、secret 与凭据在 JSONL、SQLite、log、support bundle、截图索引和证据 oracle 中为 0 命中。
8. zh-CN/en-US、亮/暗、目标视口、缩放、IME、键盘与 reduced-motion 门禁通过。
9. 父 Run 的完成/失败/取消/中断在 1 秒内终结所有子代理、工具与计时器并返回 terminal ACK；候选池过期可自愈或给出可恢复 Attention，不出现假运行、残留 spinner 或重启复活。
10. 同风险工作区只读请求可安全聚合，发送/追加/停止不互相冒充；打开执行台不改变顶层窗口尺寸或位置。
11. Settings 的 12 个一级页、搜索/返回/焦点恢复、四个 GuideSheet、逐能力 provenance、统一 loading/last-good/retry 与 CAS 三路恢复、canonical Provider default/reference、无损图片路由、Codex terminate ACK、stable-ID MCP 独立启用审批、per-slot Prompt、Shell immediate apply 和 §5.8 关键闭环全部通过；`settings-capability-baseline` coverage=100%，`missing | orphan | unexpected_noop | prototype_only_evidence | unauthorized_retirement | empty_provenance` 均为 0；只有运行/检查态显示中性 spinner，浅色主题不存在复用暗色黑 overlay/大投影的组件。
12. D0 source inventory proof 绑定当前 revision、symbol resolution 零失败且 `production_existing>0`；M5 累计门在未改码的同一 revision 上以新进程和隔离 fixture 连续通过三轮，跨轮状态泄漏为 0。

`production_release_ready` 还需要真实 Provider/账号、Windows/macOS/Linux 签名安装包、Updater 端点、OS 托盘/通知权限与候选 soak；这些外部条件不能阻止 fixture/local 把代码推进到 `implementation_verified`，也不能被 mock 冒充。

### 1.5 非目标

- 不展示、恢复或推断私有 chain-of-thought；“更充分的反馈”仅指公开 commentary 与宿主可证事实。
- 不像素复制 Codex、Zcode 或第三方品牌，不引入与 R-Code 并存的第二套设计系统。
- 不把每个 token、stdout 字节、工具调用或子代理心跳渲染成独立聊天卡片。
- 不把完整 App Demo、真实能力投影或规划 Demo 当作产品实现证据；HTML 内的保存、安装、通知、退出、权限和 Provider 结果均不代表真实系统副作用。
- 不在本计划中实现 GitHub CLI、Goal 增强、Side Chat、外部 IDE/LSP、插件市场、Connector、Artifact、Remote/云同步/团队协作、完整 IDE、Debugger 或 OS 级 Computer Use。
- 不授权提交、推送、发布、修改用户全局配置或清理当前未提交工作区。

## 2. 已冻结产品与架构决策

1. **一个事件底座**：复用现有 App Server / R-Code `AgentEvent` 归一化、commentary/final、requestUserInput 与工具生命周期；本计划只扩展表现与宿主派生状态，不再造协议。
2. **三层信息架构**：左侧是全局任务导航，中间是用户/模型沟通与交付，右侧是状态/子代理/变更；工具详情属于发生它的 Run 或子代理。
3. **材质不是状态源**：玻璃、渐变和动效只表达空间与因果；任务、权限、连接和错误必须有文字/图标等非颜色线索。
4. **Close preference 是枚举**：保存 `ask | hide | quit`，不单独保存 `dont_ask`；取消不保存；恢复能力由 Host 实时决定。
5. **Close Host authority**：Rust/Tauri 先 `prevent_close` 并执行可重入状态机；React 对话框只是 `ask` 的表现，不能成为 Alt+F4 可绕过的唯一门。
6. **显式退出旁路询问**：Tray Quit、Updater restart 与 OS shutdown 带受控 bypass reason，直接走统一清理；普通 CloseRequested 不得伪装成显式退出。
7. **Provider readiness 下沉 Host**：UI `useEffect` 不拥有连接生命周期；Host 先快照配置、锁外联网、再按 fingerprint/CAS 写回。
8. **探测不阻塞、无 fallback**：先渲染 Shell；默认并发 2、成功 TTL 30 分钟、失败 TTL 5 分钟；配置变化立即失效。exact-model probe 可能少量计费并可关闭。
9. **轨迹默认紧凑**：活跃 Run Capsule 持续可见，普通详细轨迹默认收起；失败/审批/提问/warning/final 自动保持可发现；用户 override 优先于自动折叠。
10. **公开反馈双来源**：模型 commentary 原样显示；宿主状态只来自真实事件。二者视觉上区分，宿主不得编造模型意图。
11. **统一 TaskStatus**：任何新 Rail/Workbench 不自行实现另一套任务优先级；读取共享投影和 Attention。
12. **CSS 单一权威**：先合并 token/material/基础壳层，再迁移组件；不得用第 13 个全局覆盖文件修复前 12 个文件的冲突。
13. **平台特定只留 adapter**：Windows/macOS/Linux 的文案、托盘/Dock、进程和路径 adapter 可不同，状态/错误/事件/证据语义必须一致。
14. **旧计划安全合同延续**：Worktree fail closed、Browser browse/interact 分离、Automation 注册期能力过滤、审批无自动通过等全部保留。
15. **实现与发布分层**：local fake、fixture 和候选真实 smoke 必须标注 profile；真实凭据、签名、通知权限和生产发布属于独立放行状态。
16. **Settings 是完整场景**：12 个一级页由一个 route/metadata registry 驱动搜索、导航、标题、来源与 feature flag；不再用多个互不一致的小弹窗复制设置。
17. **Demo 来源不可混淆**：`production_existing`、`new_requirement` 和 `planned_demo` 是逐能力设计证据标签，不是完成状态；混合页按卡片标注，production UI 仍由实现门禁判断。
18. **浅色材料和运行状态独立建模**：day theme 拥有独立 canvas/content/sunken/shadow/scrim token，不继承黑 overlay；只有 `running/checking` 使用中性 spinner，其他状态使用静态 glyph + 文字。
19. **Settings authority 逐能力冻结**：route registry 只负责可发现性；真实值、revision、apply mode、副作用和错误以 Host/config authority 为准，页面不能用 local success 覆盖失败。结构映射 gate 与源码语义 inventory gate 分开，前者通过不完成 D0。
20. **高风险设置动作单独确认**：MCP exact launch approval、Provider/slot revision conflict、dirty discard 和删除依赖检查各有独立状态机；普通保存/测试不能顺带授权启用、丢草稿或删除引用。

## 3. 仓库事实基线

| 事实 | 当前落点 | 对实施的约束 |
| --- | --- | --- |
| 当前分支为 `dev`，工作区有大量 modified/untracked 文件 | `git status --short --branch` | 禁止 reset/覆盖；任务必须声明 `owned_paths` |
| 根场景、Rail、Room/Workbench 状态已存在 | `App.tsx`、`store/app.ts`、`Rail.tsx`、`RoomScene.tsx` | 增量重构现有壳，不另建平行 App |
| Timeline 已有事件聚合、折叠和流式刷新 | `Timeline.tsx`、`TimelineActivity.tsx`、`timeline-presentation.ts` | 扩展稳定 presentation reducer，不改 wire 语义 |
| 子代理已有列表、详情、transcript 和停止入口 | `SubagentWorkbench.tsx`、`SubagentPanel.tsx` | 重组 IA 并删除 Summary 重复审计 |
| Windows 有托盘，CloseRequested 当前直接 hide/quit | `src-tauri/src/main.rs` | 必须由 Host 状态机统一自绘 X 与原生 close |
| 显式退出已有 Tauri command | `tauri_commands.rs::cmd_app_quit` | 扩展统一清理与 bypass reason，不复制退出路径 |
| Provider `ready` 只证明配置完整 | `settings_get` / `ProviderStatus` | 新 DTO 必须拆分 configured/connectivity |
| 子代理 Provider 页面已自动探测 | `SubagentProvidersPanel.tsx` | 把调度下沉 Host，保留 receipt/TTL/fingerprint 逻辑 |
| API probe 是真实 8-token completion | `commands.rs::run_api_subagent_probe` | 设置文案说明潜在费用；实现有界、可关闭 |
| 成功/失败回执 TTL 已有 30m/5m | `commands.rs` Provider health receipt | 复用而非另建互相漂移缓存 |
| 当前批量探测在配置互斥锁内串行 | `subagent_provider_test_batch` | Host service 必须锁外网络、并发有界、CAS 写回 |
| 样式由多份全局 CSS 依次覆盖 | `frontend/src/main.tsx` 与 `styles/*` | 先定义权威层与迁移顺序，不再追加最终 override |
| SettingsScene 已有 Provider、Agent、工具、知识、偏好、诊断与子代理等分区，当前完整 Demo 将其收敛为 12 页 | `SettingsScene.tsx`、`docs/product-experience-redesign/prototype.html` | 增量改造现有 Settings，使用统一 route/metadata registry；Demo 覆盖不等于产品接线完成 |
| 当前 baseline 与 validator 已绑定 47/47 source manifest：总设计盘点 127 个 inventory/CapabilityID，source/prototype/planned target/trace 均 127/127；其中 111 个 `production_existing` 已完成 111/111 固定源码审计，零缺口且 `verified_count=0` | `settings-capability-baseline.json`、`settings-capability-coverage.md`、`tools/settings_capability_gate.py` | source inventory proof 通过只证明现有功能下界与新增/计划能力的设计承接，不证明正式 UI→IPC→Host→persistence 已实现；任何迁移仍需兼容合同，合并或退役必须有独立证据与授权，当前均为 0 |
| Provider profile 的生产持久字段只有 `base_url/model/provider_kind/max_tokens/temperature/protocol/show_reasoning`；`activate` 是保存动作，模型目录 cache 为 5 分钟 | `vendor/agent-contracts/crates/agent-config/src/lib.rs:115-143`、`src-tauri/src/commands.rs:15435-15461`、`src-tauri/frontend/src/lib/provider.ts:104` | 不得把 active/fallback/web/model-sync 设计字段写进生产 schema；目录 cache 与 health receipt cache 分开 |
| Codex 权限 enum 为 `read_only/request_approval/auto_review/full_access/custom`，协作是 setup action，跨引擎仅有 `allow_cross_engine_delegation` | `src-tauri/src/codex_permissions.rs:14-35`、`SettingsScene.tsx:3104-3109`、`commands.rs:25856-25858` | model/reasoning/verbosity 与 permission 分页但单 authority；不得造第二权限字段或 `codex_subagent_enabled` |
| MCP 启用已有 Host exact-preview + one-time-token 双阶段审批，运行状态为 disabled/stopped/starting/running/error | `McpPanel.tsx:169-184`、`src-tauri/src/mcp_manager.rs`、`crates/r-code-mcp/src/model.rs:264-269` | 保存/测试与启用分离；launch plan 只能来自 Host；独立 alertdialog 处理取消/过期/config change |
| 子代理槽位逐槽保存 source/model/weight/prompt_template_id/prompt；当前 revision conflict 会 `load(true)` 覆盖草稿 | `vendor/agent-contracts/crates/agent-config/src/lib.rs:447-453`、`SubagentProvidersPanel.tsx:450-465` | per-slot Prompt 必须保留；目标冲突恢复需保留 local + fresh snapshot，不得静默丢草稿 |
| Shell 路径保存后立即更新 gateway override 并失效 cache；Settings 读取已有失败 UI 但未冻结 last-good/retry 草稿合同 | `commands.rs:25923-25948`、`SettingsScene.tsx:546,735` | Shell apply mode 固定 immediate；统一补齐 loading/failed/retrying/last-good 与 dirty/conflict 状态机 |
| 浅色截图曾被 `.conversation` 黑 wash 和组件内硬编码黑 shadow 压成灰黑主区 | `images/05-workspace-light.png` 与原型材料审计 | 产品 token 必须主题化 canvas/content/sunken/card/floating/overlay/scrim，禁止 day 复用暗色投影 |
| `codex-rich-interaction` 历史门禁 38/38 | 旧 PRD/freeze/evidence | 当前 `dev` 仍需 M0-02 回归，不直接标完成 |
| Browser 当前仅公共合同草稿 | `src-tauri/src/browser/mod.rs` | 不能把合同文件存在冒充 Runtime 实现 |
| Automation 当前仅 feature gate/调度语义草稿 | `src-tauri/src/automation/mod.rs` | repository/scheduler/dispatcher/UI 仍按本计划实施 |
| OCR 编译图片已从旧 `docs/ui` 解耦 | `fixtures/windows-ocr/deepseek-model-configuration-dark.png`、`src-tauri/src/windows_ocr.rs` | SHA-256 与原图一致且定向 Rust 测试通过；后续文档整理不再影响编译 |
| 真实启动时设置显示 DeepSeek/ark 可使用，但首次空白页仍提示连接服务并短暂显示 Codex/GPT-5.6-Sol | 2026-08-27 `dev.ps1` 运行审计 | 首屏 Provider 投影必须统一，不能把配置完整、连通性与默认选择混用 |
| 子代理候选池过期会让首次委派失败；完成设置页批量测试后才能成功委派 | 2026-08-27 `dev.ps1` 运行审计 | readiness service 必须在委派前刷新 receipt，并为失败提供可恢复降级 |
| 父 Run 失败/取消后子代理和工具仍长期显示运行中、计时器继续 | 2026-08-27 `dev.ps1` 运行审计 | 终态级联是 P1 正确性门禁，不能只做视觉重构 |
| 子代理只读读取 3 个工作区文件会出现 3 张独立审批卡 | 2026-08-27 `dev.ps1` 运行审计 | 需要风险等价的聚合只读授权，同时维持写/网/删除的独立边界 |
| 打开工作台会把顶层窗口从约 1443×903 改为约 1661×911 并重定位 | 2026-08-27 `dev.ps1` 运行审计 | Workbench 必须在当前窗口内响应式展开，不能操纵窗口几何 |
| 运行按钮由发送切换成停止时曾发生误触中止；正文短暂滞留 | 2026-08-27 `dev.ps1` 运行审计 | 发送/追加/停止需拆分并覆盖过渡竞态 |
| 交付时重跑 rich-interaction 累计回归：Rust/core 通过；前端 254 项中 244 pass、8 fail、2 skip | 2026-08-27 `scripts/codex-interaction/m4-02-regression.mjs` | 失败集中在 app-shell/Companion 的等待、归档、工作台旧入口等；M0-02 必须保留并逐项复现，不能引用旧 38/38 冒充当前 dev 全绿 |

## 4. 机器合同

### 4.1 窗口关闭合同

```text
CloseActionPreference = ask | hide | quit

CloseTrigger = titlebar | alt_f4 | native_close | tray_quit | updater_restart | os_shutdown

CloseIntent {
  intent_id,
  window_label,
  trigger,
  restore_capability: tray | dock | companion | none,
  active_runs,
  waiting_approvals,
  unsaved_workspace_changes
}

CloseDecision { intent_id, action: hide | quit | cancel, remember }

CloseGateState = idle | prompting(intent_id) | executing(action, bypass_reason?)
```

不变量：

- `prompting` 期间重复 CloseRequested 只聚焦同一对话框，不创建第二 intent。
- `remember=true` 只在 hide/quit 成功进入执行路径时原子写偏好；cancel/Escape 永不写。
- `hide` 必须在执行时再次验证 restore capability；失败则保持主窗可见并返回结构化错误。
- `quit` 走同一个可等待、有上限的清理协调器；主 Run/child/tool/timer 的 terminal projection 持久化并收到 Host ACK 后才完成退出。单个子系统清理失败记录诊断，但不得留下主窗消失且进程不可达的状态；下一进程不得恢复旧 running。
- `tray_quit | updater_restart | os_shutdown` 使用显式 bypass；普通 titlebar/Alt+F4 不能伪造。

### 4.2 Provider 健康合同

```text
ProviderConfigurationState = configured | incomplete | disabled
ProviderConnectivityState = unknown | checking | connected | degraded | failed

ProviderHealthReceipt {
  provider_id,
  provider_kind,
  model,
  config_fingerprint,
  probe_kind: catalog | exact_model,
  state,
  observed_at,
  expires_at,
  latency_ms?,
  safe_error_code?
}

ProviderProbePolicy {
  startup_enabled: bool,
  generation: u64,
  success_ttl_seconds: 1800,
  failure_ttl_seconds: 300,
  max_concurrency: 2,
  timeout_seconds: 12,
  target_scope: default_and_saved_slots
}

ProviderSnapshot {
  revision,
  canonical_default_provider_id,
  profiles,
  persisted_subagent_references,
  health_receipts
}
```

探测步骤固定为：配置锁内读取快照/fingerprint/policy generation → 去重/按默认优先排序 → 锁外有界联网 → 锁内 fingerprint+generation/CAS 校验 → 写回 receipt → 发脱敏事件。`startup_enabled=false` 递增 generation 并取消或隔离旧任务；任何迟到 generation 既不写 receipt 也不产生 connected/success。canonical default 仅在 Host compare-and-write ACK 后随同新 revision 发布，失败时快照及引用保持原样。

`catalog` 只证明目录/认证可达，不能冒充 exact model 可完成推理；不支持廉价目录的 Provider 使用最小 exact-model probe。timeout 可按 Provider capability 在 3–30 秒范围内调整，必须记录决定且不改变非阻塞语义。

### 4.3 Run Capsule 与折叠合同

```text
RunCapsuleView {
  run_id,
  phase,
  elapsed_ms,
  latest_public_update?,
  active_tool_count,
  completed_tool_count,
  active_subagent_count,
  attention[],
  change_summary?,
  verification_summary?,
  detail_state: auto_compact | auto_expanded | user_compact | user_expanded
}
```

- `attention` 含 approval/question/failure/workspace-invalid/review/verification 时不得 auto compact。
- `user_compact/user_expanded` 在本 Run 生命周期内优先于普通自动规则；新的失败/审批可临时强制可见，但不能销毁用户状态。
- `latest_public_update` 只来自 commentary 或明确宿主事件文案；raw reasoning 永不进入。
- 历史和 live 使用同一 presentation reducer；fold 只影响 DOM 呈现，不删除事件或改变持久化。

### 4.4 TaskStatus 与执行台合同

```text
WorkbenchTab = overview | subagents | changes

TaskStatus priority:
  archived
  > awaiting_user(approval | question)
  > failed_or_invalid(interrupted | run_failed | workspace_binding_invalid)
  > review_or_verification
  > running
  > queued
  > idle
```

- `overview` 仅显示目标、阶段、耗时、队列、Attention 和可跳转摘要。
- `subagents` 显示协作树和选中 child transcript；child 工具只在 child 详情出现。
- `changes` 显示文件/diff/verification/review；不复制全部 Timeline 工具事件。
- 全局工具调用入口从 Workbench 一级 IA 删除；调试需要时从 Run Capsule 展开或诊断页进入。

### 4.5 运行终态、委派与权限合同

```text
RunTerminalState = completed | failed | cancelled | interrupted
ChildTerminalState = completed | failed | cancelled | skipped

on_parent_terminal(parent, reason):
  atomically seal parent event stream
  resolve every non-terminal child/tool using reason mapping
  stop child/tool elapsed timers
  invalidate stale approval/question handles
  persist one monotonic projection revision
  acknowledge terminal projection to caller

ReadGrantScope = once | run_readonly | workspace_readonly
RiskClass = workspace_read | workspace_write | destructive | external_network | privileged
```

- 终态投影以 Host event/reducer 为权威；UI 不用超时猜测完成，也不允许子节点在父终态后重新变回 running。Shutdown/Updater restart 必须等待该 terminal ACK；重启只恢复持久 terminal projection，旧 running spinner 数必须为 0。
- 级联必须在事件层和持久化层完成，目标传播时间 ≤1 秒；reload/history 复用同一 projection。
- 候选池刷新按 fingerprint 去重并复用 Provider readiness receipt；可选委派失败可继续主代理，显式强制委派则阻塞并给出重试入口。
- 聚合只读授权只能覆盖 canonicalized WorkspaceBinding 内的读取/list/search；越界路径、symlink 逃逸、写入、Shell、网络或 MCP mutation 不能被归入只读 grant。
- approval 卡片必须显示风险类别、目标范围、持续时间与撤销入口；过期或父 Run 终态后不可再提交。

### 4.5.1 Settings Scene、Demo 来源与状态 glyph 合同

```text
SettingsPane =
  providers | agents | subagents | tools | knowledge | permissions |
  security | appearance | notifications | lifecycle | updates | diagnostics

SettingsCapabilityProvenance = production_existing | new_requirement | planned_demo
SettingsApplyMode = immediate | next_run | next_restart

SettingsRoute {
  pane,
  title_key,
  description_key,
  search_terms[],
  provenance[],
  feature_flag?,
  focus_anchor?
}

SessionVisualState =
  running | checking | queued | waiting_input | waiting_approval |
  completed | failed | cancelled | skipped

StatusGlyph = neutral_spinner | queued_clock | question | approval_shield |
              success_check | failure_cross | cancelled_stop | skipped_minus

DayMaterialTokens = canvas | shell_glass | content | sunken |
                    shadow_card | shadow_float | shadow_overlay | scrim
```

- 12 个 `SettingsPane` 必须来自同一 route/metadata registry；左侧导航、搜索索引、页面标题、i18n key、来源 badge、feature flag 和 E2E selector 不得各自维护独立列表。
- `production_existing` 只表示当前 revision 的 production symbol/handler/config authority 已由语义 inventory gate 验证；它仍需 implementation assertion。`new_requirement` 表示本 PRD 的新增实现目标。`planned_demo` 在产品中默认 feature-disabled，前端隐藏且直接 IPC/dispatcher 调用拒绝。
- 混合页允许多个 provenance，但每个规划卡必须单独显示 `planned_demo`；搜索结果与深链进入后仍能看到该来源，不能由页面级“现有能力”标签覆盖。
- 设置提交通过共享 store/Host contract 返回 `{ applied_mode, effective_at, safe_error? }`；失败不改已持久值。Provider、关闭、通知、Updater 等 Host 权威状态不能由页面本地 state 伪造成功。
- session 及子级只有 `running/checking` 映射 `neutral_spinner`；spinner 不使用成功绿、警告黄或失败红。队列、等待输入、等待审批和全部终态均 `animation:none`，同时提供静态 glyph、文字与可访问名称。
- `prefers-reduced-motion` 下 `neutral_spinner` 停止旋转并保留静态缺口环；状态仍由文字/ARIA 明确，不退化为与 queued/completed 相同的实心圆点。
- day theme 的组件不得写死 `rgba(0,0,0,…)` 作为 canvas、card、composer、popover、dialog 或 drawer 阴影；所有层级消费 `DayMaterialTokens`，高密度正文面使用 `content/sunken` 而非穿透玻璃。

### 4.5.2 Settings authority、CRUD 与恢复合同

```text
SettingsLoadState =
  uninitialized | loading | ready(snapshot_revision) |
  stale_last_good(snapshot_revision, safe_error) | failed(safe_error) | retrying

SettingsDraftState =
  clean(base_revision) | dirty(base_revision) | saving(base_revision) |
  conflict(base_revision, latest_revision)

SettingsApplyMode =
  not_applicable | transient | immediate | next_use |
  next_connection | next_run | next_session | next_restart

SettingsOperationFailureMode =
  not_applicable | single_operation | atomic_transaction | per_operation

SettingsConflictResolution =
  discard_local | reapply_local_onto_latest | field_level_merge_with_preview

ProviderPersistedProfile {
  name, base_url, model, provider_kind, max_tokens,
  temperature, protocol, show_reasoning
}
ProviderSaveAction { profile, activate: bool }
CodexPermissionMode = read_only | request_approval | auto_review | full_access | custom
McpServerState = disabled | stopped | starting | running | error
McpServerIdentity { server_id: immutable, display_name, config_revision }
McpEnableApproval { server_id, config_revision, host_launch_preview, one_time_token, expires_at }
SubagentSlot { source, model, weight, prompt_template_id?, prompt }
CodexCancelOperation { operation_id, process_id, generation, terminate_state, ack_at? }
ImageRouteDecision = direct_original | explicit_ocr | confirmed_helper | reject_batch
GuideId = provider | plan | subagent | image
```

- Settings 初次进入和 refresh 都从 Host snapshot 开始；读取失败不得落到伪 `ready({})`。存在 last-good 时界面只读显示并标记 stale；不存在时显示明确错误与 retry。retry 成功先比较 revision：clean draft 才替换，dirty draft 进入 conflict，不能静默覆盖。
- `SettingsLoadState` 与 `SettingsDraftState` 只是跨设置页共享的加载/草稿元状态，不能替代 Provider、Codex、MCP、Updater、通知、Memory 等领域状态机；每个 CapabilityID 仍须保留自己的值域、权限、正向/失败/禁用与恢复语义。
- `SettingsApplyMode` 必须逐能力声明：`not_applicable` 仅用于只读投影或无生效概念的动作，`transient` 仅影响当前预览/探测，`immediate` 在本次成功操作后生效，其余值分别在下一次使用、连接、Run、会话或重启生效；UI 不得把它们泛化为“已保存，稍后生效”。
- 通用 CRUD 固定为 `load snapshot → edit local draft → validate → save(base_revision) → Host compare-and-write → publish fresh snapshot`。该 lifecycle 由 12 页共用 reducer 驱动，领域 reducer 不能绕过它直接发布本地 success。写失败保留 persisted snapshot 与 dirty draft；离开 dirty 页面先确认 discard；CAS 冲突同时保留 local/fresh 两份，只能进入 `discard_local | reapply_local_onto_latest | field_level_merge_with_preview`，成功路径必须返回新的 base revision。删除前执行 Host 引用检查，默认 Provider 或被任一持久子代理槽引用的 Provider 必须拒绝，拒绝后 default/profile/reference digest 全部不变。
- 一次页面动作若跨多个 IPC、凭据存储或 Host 写入，必须按 `SettingsOperationFailureMode` 报告每个 operation 的结果；只有源码与故障注入证明 `atomic_transaction` 时才可宣称整体原子回滚。`per_operation` 失败必须保留已成功项、旧持久快照、未提交草稿和可重试边界，不能用一条整页“保存成功/全部回滚”文案掩盖部分完成。
- Provider 的生产持久字段仅为 `base_url | model | provider_kind | max_tokens | temperature | protocol | show_reasoning`（另以 profile `name` 定位）；`activate` 只是保存动作参数，不是 `active` 持久字段。canonical default 只能随 Host compare-and-write ACK 更新；Provider mini、Composer、Settings、health 与主 Agent 默认模型读取同一 snapshot revision，Host reject 后旧 default/profile/reference 全部保持。默认 Provider 或被持久子代理槽位引用的 Provider 不可删除。已保存 profile 的 name/preset 不可编辑；每轮最大输出下限为 2048、厂商上限为硬上界。fallback endpoint 是同一 `base_url + protocol` 的派生候选，`web_route` 是只读诊断；不得虚构 `web_enabled`、可写 `web_route` 或持久 `model_sync` 策略。
- Provider 模型目录 UI cache TTL 为 5 分钟；它与 Provider health receipt 的成功 30 分钟/失败 5 分钟是两个独立 cache，不得共享 key 或互相当作连通性成功。当前 credential→config save 以及 config→secret delete 不是原子事务；目标实现必须使用事务/补偿日志或显式 partial-failure recovery，保证旧 profile/secret 可恢复且重试幂等。
- Codex 的 model、reasoning effort、verbosity 留在 Codex 偏好；`CodexPermissionMode` 只在 Permissions 页维护一个 authority，`custom` 兼容值只读保留。Skill/MCP collaboration 是一次 setup action，不是两个持久 checkbox；生产已有跨引擎字段为 `allow_cross_engine_delegation`，不得新增 `codex_subagent_enabled` 镜像。登录显式 cancel 标记 `new_requirement`：客户端携带 operation/process ID 与 generation 请求 Host terminate，并等待底层 ACK 后才显示 `cancelled`；有界超时显示 `cancel_failed` 和重试，旧 generation 的 poll/result 不得回写 login/ready。
- MCP 新建/编辑先保存为 disabled；既有 `server_id` 是稳定不可变主键，UI 只读显示且每次 submit 由 Host 按 config revision 再校验。display name 改名不能修改/替换 `server_id`，只能 create-new + explicit remove-old；旧 credential 的 present/value 不得被 UI 推断复制。用户点击启用后由 Host 返回 exact launch preview + one-time token，独立 `alertdialog` 审核后再次 toggle 才消费。编辑/测试不消费 token；取消、过期或 config revision 变化使 token 失效并恢复焦点。客户端不得拼装 launch plan，也不得以不存在的 `ready` 替代 `running/stopped`。
- 图片路由先以主模型 capability 与用户显式选择决策：confirmed multimodal 直接发送整批原图；`unknown` 仅在用户显式选 OCR 时提取文本；`unknown/text-only` 只有在 helper 已确认 multimodal 且 provider/model/config 完整时才调用 helper。helper 不完整、capability 为 unknown/text-only 或调用失败时整批拒绝、不发送部分消息；helper 失败绝不自动降级 OCR。
- 每个 `SubagentSlot` 独立保存 Prompt 模板和 Prompt 文本；全局 Prompt 卡不能替代 per-slot 数据。当前 revision conflict 后刷新会覆盖本地草稿，目标 `new_requirement` 必须改为保存 local snapshot、读取 fresh Host snapshot并显式 discard/reapply/merge。
- `execution.bash_shell_path` 保存成功后 `immediate` 生效：同一 Host 操作更新 gateway shell override 并 invalidate shell cache，下一次工具调用读取新值；失败保持旧 override 与 dirty draft。
- Provider、Plan、Subagent、Image 四个 `GuideId` 各自拥有入口、内容、deep-link/search anchor、focus trap、Escape/backdrop 关闭与触发器焦点恢复；缺任一个都不是“Guide 已完成”。

### 4.6 Worktree 合同

```text
ExecutionEnvironment = local | worktree
ManagedWorktree { task_id, repo_root, worktree_path, branch_name, base_oid, managed_by_r_code, cleanup_state }
TaskWorkspaceBinding { task_id, root, kind, repo_root?, access_mode, managed_worktree? }
```

解析必须验证：路径目录、canonicalize、R-Code 托管记录、`git worktree list --porcelain`、Git common-dir、junction/symlink 防逃逸。任一失败直接拒绝，不能 fallback。

### 4.7 Browser 合同

```text
BrowserRuntimeManifest { schema_version, runtime_version, platform, arch, wrapper_version, node_version, playwright_version, chromium_revision, asset_url, asset_size, sha256 }
BrowserSession { session_id, task_id, profile_path, runtime_version, process_state, active_tab_id?, last_url?, last_screenshot? }
BrowserPermissionGrant { task_id, origin, capability: browse | interact, scope: once | task, granted_at, revoked_at? }
```

工具名固定为 §1.3 R-BR-03；`wait` 仅接受 selector/text/URL/load-state 且单次不超过 30 秒。权限按最终精确 origin 重检，不支持 wildcard。

### 4.8 Automation 合同

```text
AutomationDefinition { id, name, workspace_path, prompt, execution_profile, schedule, timezone, permission, base_ref?, state, next_run_at_utc, created_at, updated_at }
AutomationRun { id, automation_id, task_id?, trigger, scheduled_for, definition_snapshot, status, idempotency_key, lease_owner?, lease_expires_at?, missed_count, started_at?, finished_at?, error_code? }
RunStatus = queued | running | waiting_approval | succeeded | failed | skipped | cancelled
Permission = read_only | isolated_write
```

Hourly 按 UTC interval；Daily/Weekdays/Weekly 按 IANA 本地墙钟；DST 缺失时间在跳变后首个有效时刻运行，重复时间只运行第一次。单 Definition 不重叠，恢复只补最新一次，更早遗漏聚合为 skipped。

### 4.9 错误、隐私与版本

- UI 仅凭稳定 `code + args` 本地化；`debug_detail` 只进入受控诊断/复制入口。
- 所有 DTO 使用显式枚举与 unknown 分支；旧配置迁移 additive、幂等，可回读。
- 日志和证据先脱敏再截断；缺失 required assertion/metric 是失败，不是跳过成功。
- schema、locale、event、fixture 和 Provider capability 版本进入报告；密钥只记录“是否可解析”或不可逆引用。

## 5. 产品流程与状态矩阵

### 5.1 主任务工作流

```text
用户消息
  → commentary（有信息变化才出现）
  → Run Capsule（阶段/耗时/子代理/Attention）
      → 普通工具默认紧凑
      → 失败/审批/提问自动可见
  → final answer
  → 变更 / 验证 / Review 跳转
```

### 5.2 关闭状态

| 条件 | 默认呈现 | 允许动作 | 恢复/失败 |
| --- | --- | --- | --- |
| preference=`ask`，有 restore surface | 单例对话框 | hide / quit / cancel / remember | Esc=cancel；焦点回触发按钮 |
| preference=`hide`，restore 可用 | 直接隐藏 | 托盘/Dock/Companion 恢复 | hide 失败保持可见并提示 |
| preference=`hide`，restore 不可用 | 不隐藏，回到 ask/错误 | quit / cancel | 不得留下不可达后台进程 |
| preference=`quit` | 统一清理后退出 | 取消仅在清理尚未提交前按平台允许 | 超时记录诊断并安全终止 |
| explicit tray quit / updater restart / OS shutdown | 不再询问 | 统一清理/重启 | bypass reason 可审计 |
| prompting 时重复 close | 聚焦现有对话框 | 同上 | 不创建第二 intent |

### 5.3 Provider 状态

| configured | receipt | UI 状态 | 启动行为 |
| --- | --- | --- | --- |
| incomplete/disabled | 任意 | 配置未完成/已禁用 | 不联网、不报连接失败 |
| configured | fresh connected | 已连接 + 相对时间 | 直接沿用，零请求 |
| configured | fresh failed | 连接失败 + 重试 | failure TTL 内不自动重打 |
| configured | stale/missing | checking（沿用 last-known） | 后台排队，默认 Provider 优先 |
| configured | fingerprint changed | unknown → checking | 旧 receipt 失效，CAS 防旧结果覆盖 |
| configured | offline/timeout | degraded/failed | Shell 继续可用，不 fallback |

### 5.4 轨迹折叠

| 事件/状态 | 自动规则 | 必须可见内容 |
| --- | --- | --- |
| running、无 Attention | Capsule 紧凑 | 阶段、耗时、最近有效变化、计数 |
| 用户展开 | 本 Run 保持展开 | 顺序完整的脱敏事件和 child 摘要 |
| ordinary tool completed | 聚合折叠 | 工具类别、数量、耗时、结果/exit |
| failure / warning | 自动可见 | 原因、影响、恢复动作、安全详情入口 |
| approval / question | 自动展开且聚焦语义正确 | 可操作控件、超时/过期/已回答状态 |
| final answer | 永不并入工具折叠 | 完整 Markdown 与交付状态 |
| history replay | 与 live 相同 reducer | 稳定 key、相同排序与 fold 初始规则 |

### 5.5 关键 UI 状态

- Shell：loading / ready / offline / feature-disabled / degraded。
- Rail：empty-first-use / project-collapsed / running / awaiting-user / failed / archived。
- Workbench：overview / child list / child detail / changes empty / changes ready / verification failed。
- Composer：idle / running-steer / queued / sending / validation-error / disabled-with-reason。
- Dialog/popover：open / busy / success / safe error / stale / cancelled。

### 5.6 运行终态与审批状态

| 输入状态 | 期望投影 | 禁止状态 |
| --- | --- | --- |
| parent=failed | 未终结 child/tool ≤1 秒内 failed 或 skipped，计时停止 | 父失败但 child/tool 仍 running |
| parent=cancelled/interrupted | 未终结 child/tool cancelled，pending approval/question 失效 | 迟到响应重新激活 Run |
| stale candidate receipt | 委派前单飞刷新并显示 checking | 直接创建假 running child 后整轮报 raw error |
| optional delegation refresh failed | 主代理继续，Capsule 显示已降级原因与重试 | 静默丢弃或伪称子代理已完成 |
| required delegation refresh failed | 可恢复 Attention，允许重试/改配置/取消 | 无限 spinner 或不安全 fallback |
| N 个等价 workspace read | 一张聚合卡，可选 once/run/workspace-readonly | N 张逐文件卡或扩大到 write/network |
| running composer | 追加/排队与停止为独立控件 | Enter 或发送按钮状态竞态触发停止 |

### 5.7 完整 Settings Scene 页面矩阵

| Pane / 页面 | Demo 来源 | 产品实现边界与必须状态 |
| --- | --- | --- |
| `providers` / 模型服务 | `production_existing + new_requirement` | 真实 profile 字段/不可改 name-preset、canonical default 仅 Host ACK 后发布、默认/槽位引用删除拒绝、dirty/凭据 partial-failure recovery、2048 下限与厂商上限、5m 模型目录 cache、诊断派生字段、测试与独立 health receipt；confirmed multimodal 原图、显式 OCR、confirmed helper 或整批拒绝的无损图片路由；Provider/Image GuideSheet |
| `agents` / Agent 编排 | `production_existing + new_requirement` | 主 Agent、四态委派、复核模式/reviewer/1–3 轮、Plan 建议与 anchoring、10 项运行护栏；Codex model/reasoning/verbosity 偏好与 Plan GuideSheet，权限移至 Permissions 单一 authority；Codex cancel 等待 exact process terminate ACK，超时进入 cancel_failed |
| `subagents` / 子代理配置 | `production_existing + new_requirement` | availability/健康四态、自动/单项/有界批测、exact source+model receipt、三槽/权重 100%、每槽 Prompt 模板/12k、revision conflict 的 local/fresh discard/reapply/merge 与 Subagent GuideSheet |
| `tools` / 工具与浏览器 | `production_existing + new_requirement + planned_demo` | Shell 三态且路径 `immediate` 生效；RTK 来源/安装/启停/阻断恢复；MCP stable readonly `server_id`、保存 disabled→Host exact preview/token→独立审批→启用、HTTPS、市场和两步删除，改名仅 create-new + remove-old；Browser Runtime 卡仍为 `planned_demo` |
| `knowledge` / 知识与指令 | `production_existing` | global/project 作用域与 Memory/Prompt/Skills 三 Tab；Memory 审批/任务/版本/清空/旧文件隐私，Prompt 主/子与 append/override，Skill 来源/继承/同步/恢复/双确认删除 |
| `permissions` / 权限 | `production_existing + new_requirement + planned_demo` | Codex 五态权限单 authority、`custom` 只读兼容、项目 Agent 权限与同风险 workspace-read 聚合审批；Browser grant 为 `planned_demo`，不得扩大到写/网/mutation |
| `security` / 隐私与安全 | `production_existing + new_requirement` | 密钥存储、脱敏、CSP/sandbox 为只读强制状态；仅允许预览并清理精确缓存/诊断副本，不提供关闭安全边界的开关 |
| `appearance` / 外观与语言 | `production_existing + new_requirement` | 语言、主题、密度、减少运动；Companion enabled/full/minimized/sound/motion/revision/Host 创建失败回滚/位置恢复；day material 独立，主题切换不改任务/Provider 状态 |
| `notifications` / 通知 | `production_existing + new_requirement` | OS 权限检查/申请、类别开关、应用内测试；拒绝系统权限仅降级，不阻塞任务 |
| `lifecycle` / 启动与关闭 | `new_requirement` | `ask/hide/quit`、关闭预览、偏好重置与 Provider 启动检查；startup opt-out 取消/隔离旧 generation 且不合成 success；取消不保存，restore 不可用不隐藏；退出 ACK 后所有 Run/child/tool/timer terminal 且重启不复活 |
| `updates` / 更新 | `production_existing` | idle/checking/up_to_date/available/downloading/downloaded/installing/restart_pending/failed，版本/时间/发行说明/bytes，稍后/安装重启与按 failed_operation 重试；restart 走显式 bypass |
| `diagnostics` / 诊断 | `production_existing + new_requirement` | 请求构成仅影响新会话；四级实时日志/1.5 秒/200 条/过滤/跟随/7 天保留；支持包无写盘预览→选择目录→导出路径，默认脱敏且不导出 raw reasoning/secret |

所有页面至少覆盖 `uninitialized/loading/ready/stale_last_good/failed/retrying` 及适用的 empty/disabled/success；失败不得伪装空成功，retry 不得覆盖 dirty draft。只有真实运行或检查中的节点可旋转，终态截图必须静止。设置搜索按 registry 返回页面 + block anchor；无结果可理解，命中跳转后焦点和页面标题同步，返回后恢复原工作区触发器。390px 触控目标与四个 GuideSheet 的焦点/关闭/深链合同必须独立验证。

页面矩阵只是信息架构摘要，不是功能覆盖证据。111 项现有生产能力下界、127 个总盘点 CapabilityID、7→12 映射、source authority、classification、state/failure contract、disposition 和目标 selector 以 `settings-capability-baseline.json` 为规范输入；可读说明见 `settings-capability-coverage.md`。任何 Pane 通过但 Pane 内某个 CapabilityID 未执行，仍视为 Settings 整体失败。

### 5.8 关键端到端闭环

| 闭环 | 确定性 implementation 动作 | 必须观察到 | 失败/降级 |
| --- | --- | --- | --- |
| Provider → 全局健康 | Settings 编辑/新建 → Host ACK 保存/切默认 → exact-model 测试 → 返回 Topbar health；再拒绝一次切默认/删除引用 Provider；切断 startup probe | Provider mini/Composer/Settings/health/主 Agent 读取同一 snapshot revision；checking 仅实际 in-flight（并发 ≤2）旋转；opt-out 后旧 generation 无 success | 401/timeout/Host reject 保留旧 default/profile/reference 与安全回执，不 fallback；默认或持久槽位引用的 Provider 删除被拒绝 |
| 图片路由 | 依次注入 confirmed multimodal、unknown+显式 OCR、text-only/unknown+confirmed helper、helper 不完整/失败 fixture | 原图只走 direct；OCR 仅显式选择；helper 仅 confirmed multimodal+完整配置；每个 batch 只有一个确定 route | helper 不完整/unknown/text-only/失败时整批不发送；helper 失败绝不降级 OCR |
| Codex cancel | 启动 login operation → 携带 operation/process ID+generation 取消 → 等待 Host terminate ACK；再注入超时和迟到 poll | ACK 后才显示 cancelled；超时显示 cancel_failed+重试；旧 generation 无状态写入 | 客户端不能先乐观 cancelled；stale result 不能把 cancelled/cancel_failed 改回 login/ready |
| Close → Tray restore | lifecycle 设为 ask/hide/quit → titlebar/Alt+F4/native close → 选择并记忆 → 托盘恢复；再执行 explicit quit/restart | 三入口同 intent；取消不写；hide 后任务继续；退出等待 terminal projection + Host ACK | restore 不可用时 hide 禁用并保持窗口可达；显式 quit/updater restart 不二次询问；重启后旧 running/spinner 为 0 |
| 长任务 → 子代理 → final | 发送 → commentary → Run Capsule → 子代理详情 → question/approval/failure → final → changes/verification | 用户 override、Attention 和 final 可发现；父终态 ≤1 秒封口 child/tool/timer 并 ACK | 迟到帧不复活；失败有重试/取消；终态 glyph 静止，reload 无旧 running |
| 通知权限 → 测试 | notification 权限检查/申请 → 分类开关 → 前台测试 → 后台关键事件 fixture | 前台应用内、后台按类别系统通知；状态与 Settings 相同 | OS 拒绝/adapter 失败降级到应用内且任务继续 |
| Updater → restart bypass | 检查 → available → 下载/签名校验 → 稍后或安装重启 | 状态单调、进度真实、restart 只执行一次统一清理 bypass | 离线/损坏/签名错进入可重试错误且旧版本可启动 |
| Diagnostics → support preview | 开启请求构成（仅新会话）→ 自检 → 生成支持包预览 | 显示精确包含范围和 0 secret oracle，不实际上传 | 发现敏感命中即失败并禁止生成/导出 |
| Settings load → CRUD/conflict | 任一 Pane 首次 load → 编辑 dirty draft → 保存/拒绝 → refresh/retry/CAS conflict → 分别执行 discard local、reapply onto latest、field merge preview | 12 页共用 lifecycle；last-good/local/fresh digest 分离；三路恢复都返回新 base revision；失败不显示空成功；重启恢复已确认持久值 | 读取失败保留 stale last-good 或明确 retry；写失败/冲突不静默覆盖草稿，页面本地 success 无权覆盖 Host |
| MCP save → exact enable approval | 编辑 existing stable `server_id` → Host 校验 → 保存为 disabled → 点击启用 → Host preview/token → 独立 alertdialog 审核 → 再次确认；另走 create-new+remove-old 改名 | `server_id` 全程只读不变；launch plan 与 config revision 来自 Host；只在最终确认消费一次 token；状态进入 starting/running 或 safe error | ID 篡改、cancel/expire/config change 均拒绝并恢复焦点；编辑/测试不消费 token；credential 不推断迁移，不出现虚构 ready |
| GuideSheet × 4 | 从对应卡片或搜索深链进入 Provider/Plan/Subagent/Image guide → 键盘浏览 → Esc/backdrop 关闭 | 四个内容与 anchor 独立，focus trap 有效，关闭回到各自触发器 | 缺入口/内容/焦点恢复任一项即失败，不以单个 Provider guide 代替 |
| Planned Demo 保真 | 从搜索或页面进入 Browser Runtime / Browser grant 并执行模拟动作 | 始终可见 `planned_demo` 标签，Demo 只改会话内模拟状态 | 产品 feature-disabled 时入口隐藏且直接调用拒绝，不能由 Demo 状态冒充 implemented |

## 6. 平台延续边界

旧 gap plan 删除后，本节与 §1.3、§4 成为其安全与产品语义的替代来源：

- 五条正式能力线仍是基础稳定性、Worktree、全量双语、Browser、Automation；本轮体验任务可先落地，但不能把平台线从最终 `implementation_verified` 中裁掉。
- Browser/Automation 未闭环时入口完全隐藏且后端拒绝；不能用 mock 入口冒充产品完成。
- Worktree、read-only、isolated-write、Browser origin、审批、清理全部 fail closed；不允许临时 Local fallback、自动批准或全局授权。
- Provider/model/branch 不可用时失败并通知，不静默换成别的配置。
- 真实凭据、签名包和生产权限不阻塞本地实现，但保持 external pending。
- 发布后候选能力（GitHub CLI、Goal、Side Chat、IDE/LSP、插件/Remote）继续是非目标，需独立立项。

### 6.1 被替换旧计划的追踪映射

下表是已删除旧文档 `R-Code 功能补齐完整实施计划` 的规范迁移记录。旧条目没有被当成本轮用户指令自动扩张；仍属于正式产品边界的内容被合并到本文 RequirementRef/TaskID，重复步骤被收敛为唯一执行入口。

| 旧章节/轨道 | 本文替代位置 | 处理 |
| --- | --- | --- |
| §1–§3 目的、正式范围、实施原则 | §1、§2、§12–§14 | 保留 implementation/production 分层、fail-closed、不得伪绿等原则 |
| §4.1 结构化错误 | R-ERR-01、M1-01 | 完整延续并纳入双语/隐私门 |
| §4.2 TaskWorkspaceBinding | R-BIND-01、M1-02、M6-02 | 完整延续并强化全部消费者单一入口 |
| §4.3 派生任务状态 | R-STATUS-01、R-RUN-01、M1-03 | 延续并加入实测的终态级联 P1 门禁 |
| §4.4 Worktree 持久化模型 | §4.6、M6-01～M6-04 | schema、身份、恢复、Review、保守清理全部延续 |
| §4.5 Browser 契约 | §4.7、M1-05、M7-01～M7-05 | Runtime、Session、工具、origin、browse/interact、清理全部延续 |
| §4.6 Automation 契约 | §4.8、M1-05、M8-01～M8-05 | schedule/DST/lease/snapshot/权限/审批/Review 全部延续 |
| S0 基线、Flags、模块边界 | M0-02、M1-02 | 合并为可复现基线与前后端双 guard |
| F1～F8 公共基础 | M1-01～M1-05、M2-01、M5-01 | 错误、i18n、Binding、Status、通知、Updater、合同、可读性全部有任务卡 |
| W1～W5 与 W Gate | M6-01～M6-04 | 创建/消费/生命周期/三平台 Gate 拆成四个可验收任务 |
| B1～B8、Read-only/Full Gate | M7-01～M7-05 | 供应链、安装进程、只读工具、browse、interact 与安全 Gate 全覆盖 |
| A1～A9、Read-only/Full Gate | M8-01～M8-05 | 持久化调度、UI/Dispatcher、read-only、isolated-write、审批/Browser 全覆盖 |
| C1～C4 UI/i18n/文档 | M1-01、M2-01～M5-03、M6-04、M9-01 | 与本轮体验重构合并，避免第二条平行 UI 轨 |
| G1/G2 只读与写入集成 | M9-02 | 以两个确定性组合场景保留 |
| H1～H6 硬化与三平台验收 | M5-02、M6-04、M7-05、M8-05、M9-01、M9-03 | 故障、安全、通知/Updater、双语、三平台进入累计门 |
| R1～R4 冻结、质量、Soak、发布 | M9-03、M9-04、§14 | implementation/candidate/production 独立记录，不授权自动发布 |
| §14–§15 并行与拓扑 | §9、§12 | 文件所有权、DAG、TaskPacket 和恢复协议替代散文顺序 |
| §16 发布后候选项 | §1.5 非目标 | 不删除产品想法，但明确需单独立项，不纳入本次 completion |

### 6.2 文档收束和历史证据边界

- 当前权威入口保留 `docs/readme.md`、`docs/readme.en.md`、`docs/architecture.md` 与 `docs/product-experience-redesign/`；支持材料移入 `docs/support/{guides,operations,platform,contracts,ui-reference,archive}`。
- OCR 编译资产已以相同 SHA-256 迁到 `fixtures/windows-ocr/` 并改 `include_bytes!`，定向 Rust 测试通过后才把旧 UI 图片移入 support；测试与文档资产现已解耦。
- completed PRD/freeze 移入 `docs/support/contracts/` 时只更新位置字段，normative/worklist 正文与 digest 不变，并生成新的路径迁移验证报告。
- `artifacts/ai-tasks/evidence/**` 与既有 verification JSON 记录旧 revision 的历史事实，禁止批量改写旧路径；`docs/support/README.md` 提供旧→新映射。

## 7. 质量、性能与安全门禁

| 维度 | implementation profile 的二值门禁 |
| --- | --- |
| 首屏与 Provider | Shell/Composer 在 probe 未完成时可操作；TTL 新鲜时请求数为 0；并发 ≤2；fingerprint 变化才强制重测 |
| 关闭 | 自绘 X/Alt+F4/native 同路径；重复 close 单对话框；偏好跨重启；无 restore surface 不隐藏 |
| Timeline | 10,000 delta 不产生 10,000 DOM 节点；可见更新 ≤10Hz；最终文本/终态完整 |
| 运行可信度 | 父 Run 任一终态后 ≤1 秒级联终结所有 child/tool/timer；迟到帧不能复活；live/history 一致 |
| 权限 | 工作区只读可按 run/workspace 聚合；路径逃逸、写、删、网与 mutation 不被聚合；过期 grant 拒绝 |
| Composer | 发送、追加/排队、停止分离；Enter 在 running 状态从不停止；过渡竞态与 IME 不误触 |
| Workbench | 展开/抽屉不改变顶层窗口几何；活动子代理/Attention 一跳可达；工具审计不占一级 IA |
| 传播延迟 | 不含 Provider/进程时间，事件到测试 UI 状态更新 p95 ≤250ms |
| Settings | registry 恰有 12 个唯一 Pane；导航/搜索/标题/i18n/provenance/flag 同源；source inventory proof/symbol resolution/三层 provenance 可审计；各页 loading/last-good/failed/retry、CRUD dirty/discard/conflict、四 GuideSheet、MCP 独立审批、per-slot Prompt、Shell immediate apply 按适用性可达，无无响应假按钮 |
| 视觉 | 960×640、1280×800、1440×900；亮/暗；100/125/150/200%；工作区与 12 个设置页无意外横向滚动或遮挡 |
| 浅色材料 | day theme 的 canvas/content/sunken/card/floating/overlay/scrim 均来自独立 token；静态扫描与 computed-style 证据中组件级黑 overlay/黑色大投影为 0 |
| 可访问性 | 正文对比 ≥4.5:1、必要 UI/焦点 ≥3:1；键盘完成 Settings 搜索/12 页导航/返回/四 GuideSheet，以及关闭、健康、Composer、轨迹、子代理/MCP 审批/冲突恢复；390px 主动作命中区 ≥44×44、紧凑次要控件 ≥32×32 且不重叠；状态有语义名称 |
| 运动 | 仅 `running/checking` 使用中性 spinner；queued/waiting/approval/全部终态静止；reduced-motion 下 spinner 为静态缺口环且状态仍可理解 |
| 关键闭环 | §5.8 的 Provider、Close/Tray、长任务、通知、Updater、诊断和 planned-demo 保真流程均有正向与失败 E2E，返回上下文/焦点正确 |
| Worktree | 路径替换、repo 不匹配、junction/symlink、dirty/new commit 均按合同 fail closed，原项目 hash 不变 |
| Browser | redirect/popup/new tab/final URL 无权限绕过；进程/profile/截图按 Task 隔离并清理 |
| Automation | schedule/DST/idempotency/lease/overlap/恢复确定；read-only 攻击 fixture 不能写；每 Run 独立 Worktree |
| 隐私 | raw reasoning、secret、key、cookie、authorization 和未脱敏输出在持久化/日志/支持包/证据为 0 命中 |
| i18n | zh-CN/en-US key/placeholder 完全一致；新增 JSX/Rust 用户文案硬编码门禁通过 |
| 回归 | 当前 `dev` 受影响的前端/Rust/既有 rich-interaction/release 门禁全绿，required 缺失即失败；M5 同一 revision 以新应用进程和隔离 fixture 连续三轮全绿，round/revision/前后状态 digest 齐全且跨轮泄漏为 0 |

<!-- AI_WORKLIST_NORMATIVE_END -->

<!-- AI_WORKLIST_CONTRACT_START -->

## 8. Verification Harness

### 8.1 唯一入口

M0-01 建立薄编排器并复用现有 runner：

```powershell
node scripts/verify-product-experience.mjs --task <TASK_ID> --profile implementation
node scripts/verify-product-experience.mjs --through <MILESTONE_ID> --profile implementation
node scripts/verify-product-experience.mjs --through M9 --profile production
```

Harness 必须：

- 非交互运行；exit 0 只代表全部 required assertions 通过。
- 维护 RequirementRef → TaskID → AssertionID registry，支持 task、through、implementation/candidate/production。
- 编排现有 frontend tests/build、Rust fmt/clippy/test、契约/迁移、Provider mock、Tauri lifecycle、Playwright E2E、visual/a11y、security-negative 与跨平台 adapter。
- 输出 `artifacts/ai-tasks/verification/product-experience-gap-closure/<profile>/<task-or-milestone>.json` 和证据索引。
- 报告 revision/worktree digest、平台、schema/config/locale/provider capability、失败 assertion；不记录 secret、key 或原始敏感正文。
- required fixture/metric 缺失视为失败；禁止删测试、缩 source、降阈值、改 oracle 或把 fake 冒充真实服务。
- D0 的 HTML Demo runner 只证明设计可达性、来源标注和交互状态；D0 另需绑定当前 revision 的生产源码 inventory/symbol/provenance proof。M2/M5 的 Settings、状态源、持久化、Tauri/OS 与 §5.8 闭环必须运行正式前端/Host adapter，不得用 `prototype.html`、结构 baseline 或截图替代。

M0-01 在 Harness 尚不存在时使用直接 bootstrap：现有 Node runner 单测、一个 Rust contract test、前端 `npm test` 的最小子集和新脚本自测；随后必须用新入口自验证。

### 8.2 Profile

| Profile | 允许证据 | 不得声称 |
| --- | --- | --- |
| `implementation` | deterministic fake、临时 Git repo、mock HTTP/App Server、headless browser、本地 WebView adapter | 真实 Provider/签名安装包/生产权限已通过 |
| `candidate` | 用户明确授权的已配置 Provider 小额 probe、真实 Codex CLI、候选 OS 托盘/通知 smoke | 三平台发布就绪 |
| `production` | 签名安装包、真实更新端点、三平台权限、升级、soak、外部管理员控制 | 缺失项不能用 mock 填充 |

### 8.3 里程碑

| 里程碑 | 能力出口 | 累计命令 |
| --- | --- | --- |
| D0 | 完整 App 可点击 Demo（含 12 页 Settings）、设计说明、生产 Settings 语义 inventory proof、PRD/Checklist 与组合文档门禁 | live Settings semantic gate + worklist gate + HTML verify |
| M0 | 当前 dev 事实与统一 Harness | `--through M0 --profile implementation` |
| M1 | 结构化错误、i18n、Binding、Status、通知/更新与公共合同 | `--through M1 --profile implementation` |
| M2 | 唯一视觉系统、Shell/Rail/Composer、完整 Settings Scene 与响应式 IA | `--through M2 --profile implementation` |
| M3 | Close 生命周期与 Provider 全局健康 | `--through M3 --profile implementation` |
| M4 | 公开反馈、Run Capsule、执行台与子代理 | `--through M4 --profile implementation` |
| M5 | 可访问性、视觉/性能门禁与可回退体验发布 | `--through M5 --profile implementation` |
| M6 | Worktree 创建、消费、恢复、清理与 UI 闭环 | `--through M6 --profile implementation` |
| M7 | Browser Runtime、工具、权限、控制面板与安全 | `--through M7 --profile implementation` |
| M8 | Automation 调度、UI、两种权限、审批与 Browser 集成 | `--through M8 --profile implementation` |
| M9 | 全量双语、跨功能集成、安全硬化与发布候选 | `--through M9 --profile implementation` |

## 9. 依赖 DAG 与并行协议

### 9.1 关键依赖

```text
D0-01 → M0-01 → M0-02
                  ├─ M1 shared foundations ─┬─ M2 visual/shell ─┬─ M3 close/provider
                  │                         │                    └─ M4 interaction/workbench
                  │                         ├─ M6 worktree
                  │                         ├─ M7 browser
                  │                         └─ M8 automation
                  └──────────────────────────────────────────────→ M5 experience gate

M6 + M7 + M8 + M5 → M9 integration/hardening/release
```

详细 `depends_on` 以 §11 任务卡为准；Harness 必须做无环与 ready 校验。

### 9.2 并行波次与文件所有权

- Wave 0 串行：D0 → M0。
- M0-02 后可并行：M1-01（错误/i18n）与 M1-02（Binding/flags/module）；不得同时编辑公共 DTO。
- M1 基础通过后，体验轨（M2–M5）、Worktree（M6）、Browser 供应链（M7）和 Automation 数据/调度（M8）可按依赖并行。
- `src-tauri/src/main.rs` / lifecycle 只归 M3 close owner；Provider health owner 不编辑该文件。
- `styles/tokens.css`、全局样式加载顺序和 Shell CSS 只归 M2-01 owner；其他 UI 任务只消费已冻结 token。
- `lib/types.ts`、`lib/ipc.ts`、Tauri handler registry、migration 编号由集成 owner 串行；功能 agent 提交所需字段清单，不并发双写。
- Timeline/Workbench/Subagent 拆为互斥文件集合；需要共享 reducer 时先由 M4-01 冻结接口，再启动 M4-02/03/04。
- 每个 agent 只完成一个任务卡，返回 changed paths、断言与证据；Coordinator 验证 cumulative gate 后才勾选。

## 10. 主 Checklist（唯一状态源）

- [x] **D0-01** 交付完整 App 可点击 Demo（含 12 页 Settings）、关键状态截图、设计说明、生产 Settings 语义 inventory proof 与可执行 PRD/Checklist。证据：`images/capture-manifest.json`（status=passed、prototype sha256=a9e457a8…同源、diagnostics 全 0）、`settings-capability-gate.json`、`settings-capability-baseline.json`、`worklist-gate.json` 与 `artifacts/ai-tasks/evidence/product-experience-gap-closure/D0-01.yaml`（2026-08-27 于 main@82c8c5c 复证）
- [x] **M0-01** 建立统一 Verification Harness、assertion registry 与证据入口。证据：`scripts/verify-product-experience.mjs`、机械提取 registry（42 任务/155 唯一断言）与自测 12/12；验收报告 `artifacts/ai-tasks/verification/product-experience-gap-closure/implementation/M0-01.json`（3/3 passed）；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M0-01.yaml`
- [x] **M0-02** 审计当前 dirty `dev`，重跑既有 rich-interaction 与基础回归并冻结事实。证据：基线 `artifacts/ai-tasks/verification/product-experience-gap-closure/implementation/m0-baseline.json`（10 腿四态：4 passed / 5 failed 如实冻结 / 1 external-pending；rust 新鲜实测 2223 passed/1 已知失败，frontend 四批与 rich-interaction 按 `BASELINE_M0_REUSE` 冻结引用、真值归 M5-02/phase-β）；累计门禁 `implementation/M0.json`（14/14 passed @main@82c8c5c, 2026-08-27）；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M0-02.yaml`
- [x] **M1-01** 完成结构化错误与 zh-CN/en-US 基础合同。证据：`node scripts/verify-product-experience.mjs --task M1-01 --profile implementation` → `implementation/M1-01.json`（A1 Rust/TS fixture 同解 + A2 locale key/placeholder 齐同与硬编码 0 命中 + A3 debug-detail containment，3/3 passed @main@82c8c5c, 2026-08-27）；执行器 `scripts/product-experience/m1-01-checks.mjs`；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M1-01.yaml`
- [x] **M1-02** 完成 feature flags、模块边界与 TaskWorkspaceBinding fail-closed 基础。证据：`node scripts/verify-product-experience.mjs --task M1-02 --profile implementation` → `implementation/M1-02.json`（3/3 passed @main@82c8c5c, 2026-08-27）：A1 三层 flag 矩阵（`feature-flag-matrix` + 模块层能力闸一致性 `m1-02-gating-parity`：TS/Rust/闸位三源同构）+ `feature_flags::` 服务测试 4 passed（含新增 Worktree 第三位）；A2 Local/Worktree fixture 解析一致且重开幂等；A3 缺失/plain-dir/symlink 逃逸/repo mismatch 全拒绝无 Local fallback（host lib 全量 656/0 回归复核）。执行器 `scripts/product-experience/m1-02-checks.mjs`；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M1-02.yaml`
- [x] **M1-03** 统一 TaskStatusView、Attention 与原生/应用内通知投影。证据：共享投影 `frontend/src/lib/task-status-projection.ts`（§4.4 优先级全序+声明序 tie-break、终态单向门、父终态原子封口、STATUS_GLYPHS spinner 合同）+ `m1-03-checks.mjs` 五腿全绿（`--task M1-03` 5/5 passed @main@82c8c5c, 2026-08-27）：A1 全组合唯一/unread 独立、A2 级联封口、A3 五面共享源静态门、A4 通知降级（routing 队列+memory 镜像+permission 套件）、A5 spinner 仅 running/verifying；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M1-03.yaml`
- [x] **M1-04** 完成 Updater 产品链与关闭/重启边界。证据：`m1-04-checks.mjs` 三腿（`--task M1-04` 3/3 passed）：A1 updater 域 fixture 套件（9 相状态机/损坏与签名错不入 ready）、A2 RestartPending 单点受控 bypass 静态合同+域锁存测试、A3 错误码卫生+无 token/私有 URL 持久化扫描；实现面 `src-tauri/src/updater/*`（domain 894 行+minisign+持久化）与 `frontend/src/lib/updater-contract.ts`；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M1-04.yaml`
- [x] **M1-05** 冻结 Browser 与 Automation 公共合同和 fixture。证据：`m1-05-checks.mjs` 三腿（`--task M1-05` 3/3 passed）：A1 Rust/TS/fixture round-trip（browser-contract 5 用例+automation-contract 4 用例+host browser:: 域测试）、A2 Browse/Interact capability 分离+read-only 判定+deny_unknown_fields+automation 注册期闸、A3 disabled 时入口拒绝（browser.feature_disabled）而 schema 可读；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M1-05.yaml`
- [x] **M2-01** 收敛唯一设计 token/material/CSS authority 与玻璃 fallback。证据：`m2-01-checks.mjs` 四腿（`--task M2-01` 3/3 passed + A4 扩展腿）：四表争权（tokens/r-code-ui/product-ui/signature 裸 :root 主题劫持）收敛为 tokens.css 唯一权威（signature/r-code-ui 皮肤值升格，组件规则限缩 #app.r-code-signature 作用域）；`--fx-glass` 真透明+blur，`@supports not backdrop-filter`/`prefers-reduced-transparency` 双 fallback → 实心 `--material-panel-fallback`；day 主题独立平面材质（shadow:none）；surface 语义别名 7 枚；全仓组件/样式层 `rgba(0,0,0,…)` 硬编码 0（tokens 权威除外）；WCAG 对比度门（tokens 实值计算 10 配对）；!important 冻结预算+import manifest 冻结。证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M2-01.yaml`
- [x] **M2-02** 重构 Topbar、Rail、任务列表与 Room 壳层。证据：`m2-02-checks.mjs` 三腿（`--task M2-02` 3/3 passed）：A1 Rail/Canvas/列表/活动/仪表盘零本地状态推导签名、状态语义唯一接入 presentation/store；A2 顶层窗口几何冻结（工作台/壳层域零窗口 API，companion 精灵窗/MenuBar 白名单）；A3 布局守卫（html overflow hidden + `.scene` overflow hidden + 壳层 grid `minmax(0,1fr)`）；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M2-02.yaml`
- [x] **M2-03** 简化 Composer/运行配置，并落地 12 页完整 Settings Scene 与共享状态。证据：`m2-03-checks.mjs` 十三腿（`--task M2-03` 13/13 passed @main@82c8c5c, 2026-08-27）：A1 Enter/IME/stop 分离；A2 provider canonical snapshot 一致性（cargo settings:: 22 测试 + E2E 服务默认标注）；A3 键盘流 E2E；A4 12 页 Pane 注册表（settings-pane-registry.json 全 implemented 无孤儿）；A6 搜索/深链/窄屏/返回 E2E 3 用例；A7 CapabilityID 127 项恰映射 orphan=0；A9 preferences→appearance / codex→agents 别名；A10 CAS 三路恢复 reducer 全合同；A11/A12 subagent/MCP 套件；A13 四 GuideSheet E2E+静态合同；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M2-03.yaml`
- [x] **M2-04** 完成独立浅色材料、亮暗主题、响应式工作区/Settings 与视觉迁移闭环。证据：`m2-04-theme-responsive.test.mjs`（Playwright，`--task M2-04` 4/4 passed @main@82c8c5c, 2026-08-27）：A1 三视口(960/1280/1440)×亮暗×工作区+12 SettingsPane 零横向溢出；A2 执行台开关 window.outerWidth/Height 不变+焦点恢复；A3 主题切换保留 Composer 草稿与任务列表；A4 960 宽 Settings 导航键盘可达且焦点不入隐藏区；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M2-04.yaml`
- [x] **M3-01** 实现 Host 权威、可重入的窗口关闭状态机与偏好迁移。证据：`m3-01-checks.mjs` 四腿（`--task M3-01` 4/4 passed @main@82c8c5c, 2026-08-27）：A1 三触发等价、A2 重入/stale/重复拒绝、A3 迁移幂等默认 ask（lifecycle.toml 服务）、A4 restore=none 永不 hide + Host 统一入口静态锁定；实现面 `close_gate.rs`（纯核心+持久化 6 测试）、`lifecycle_commands.rs`、main.rs 统一 close 臂+close-prompt-request 事件；前端 ClosePromptDialog + lifecycle 真控件；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M3-01.yaml`
- [x] **M3-02** 实现关闭对话框、设置入口、托盘/Dock 恢复和统一退出清理。证据：`m3-02-checks.mjs` 四腿（`--task M3-02` 4/4 passed @main@82c8c5c, 2026-08-27）：A1 Host prompt 单例对话框（aria-modal/Esc/取消不落盘/记住经 Host 确认写入）；A2 tray/dock/none 三恢复面建模+restore=none 拒绝 hide；A3 `shutdown_coordinator.rs` 有界清理/局部失败汇总/terminal projection 单调+显式退出 bypass 命令；A4 设置页关闭行为选择/重置/立即退出入口（键盘可达）；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M3-02.yaml`
- [x] **M3-03** 实现 Host 级非阻塞 Provider readiness service。证据：`m3-03-checks.mjs` 四腿（`--task M3-03` 4/4 passed @main@82c8c5c, 2026-08-27）：`provider_readiness.rs` FreshSkip TTL 零请求/单飞/permit≤2/generation 失效零写入/零凭据 5 测试 + provider_catalog 57 与 subagent receipt 13 回归 + evidence-hygiene；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M3-03.yaml`
- [x] **M3-04** 实现全局连接健康 UI，并迁移设置页自动探测。证据：`m3-04-checks.mjs` 四腿（`--task M3-04` 4/4 passed @main@82c8c5c, 2026-08-27）：provider-health.ts 五态非颜色视图（configured≠connected/retry 矩阵/checking 唯一 spinner）+ provider_readiness 单飞重试去重 + provider snapshot E2E + TTL 零请求；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M3-04.yaml`
- [x] **M4-01** 实现 Run Capsule 派生模型、折叠状态机与稳定回放。证据：`m4-01-checks.mjs` 四腿（`--task M4-01` 4/4 passed @main@82c8c5c, 2026-08-27）：`run-capsule.ts` §5.4 状态矩阵/终态级联单调/迟到帧诊断/live-replay 一致/raw reasoning 零命中（capsule 4/4）；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M4-01.yaml`
- [x] **M4-02** 重构 Timeline 的 commentary/final/轨迹/Attention 展示层级。证据：`m4-02-checks.mjs` 四腿（`--task M4-02` 4/4 passed @main@82c8c5c, 2026-08-27）：A1 raw reasoning/secret 零渲染面（debug-detail 套件+Timeline 静态扫描+capsule 脱敏）；A2 折叠可见性合同（capsule）；A3 live/history 一致（capsule replay+codex-message-stream）；A4 timeline-incremental-performance 万级 delta 有界；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M4-02.yaml`
- [x] **M4-03** 重构执行台为概览/子代理/变更并删除重复工具审计。证据：`m4-03-checks.mjs` 三腿（`--task M4-03` 3/3 passed @main@82c8c5c, 2026-08-27）：`workbench-ia.ts` 三 tab 集合精确/自动聚焦 Attention>active child>changes>overview/用户手动保持/容器无关零窗口 API + 全局工具审计残留扫描 + 窗口 bounds 哨兵；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M4-03.yaml`
- [x] **M4-04** 完成子代理协作树、详情、返回、停止与状态反馈。证据：`m4-04-checks.mjs` 四腿（`--task M4-04` 4/4 passed @main@82c8c5c, 2026-08-27）：A1 stale candidate 刷新/probe 套件+receipt 缓存；A2 权限引擎 cargo 测试+审批聚合静态面；A3 工作台 IA+capsule 回归；A4 capsule 终态级联+task_status 级联；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M4-04.yaml`
- [x] **M4-05** 完成运行收尾摘要与 diff/验证/审批/证据跳转。证据：`m4-05-checks.mjs` 三腿（`--task M4-05` 3/3 passed @main@82c8c5c, 2026-08-27）：session-run-summary 套件+capsule 脱敏回归+搜索深链/返回 E2E+run-guard-ui；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M4-05.yaml`
- [x] **M5-01** 完成 12 页 Settings 与工作区的可访问性、状态 glyph、IME、缩放、最小窗口和 reduced-motion 加固。证据：`m5-01-checks.mjs` 六腿（`--task M5-01` 6/6 passed @main@82c8c5c, 2026-08-27）：A1 键盘流（composer/settings 搜索深链/窄屏导航）；A2 IME composition 守卫；A3 对比度门+三视口溢出矩阵；A4 reduced-motion 全停+spinner 仅 checking；A5 聚合 live region+焦点不入隐藏区；A6 390px 触控命中区规则（44/32px）；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M5-01.yaml`
- [x] **M5-02** 完成关键端到端闭环、体验 E2E、视觉回归、性能和隐私门禁。证据：`m5-02-checks.mjs` 十一腿（`--task M5-02` 10/10 passed @main@82c8c5c）：关键 E2E 链（搜索深链/GuideSheet/通知降级）、性能（timeline 万级+readiness 单飞+capsule 级联）、视觉（视口主题矩阵/day 黑影/对比度）、security-negative（hygiene/debug-detail/脱敏）、provider 健康、Pane registry 派生、CapabilityID 导航矩阵；A8–A11 曾虚报通过，2026-08-28 补齐真实聚合腿（可写能力矩阵/capability 四零门/lifecycle 全矩阵/合同束+负例）后同 revision 转绿；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M5-02.yaml`
- [x] **M5-03** 建立体验 feature flag、旧/新表现等价回退与迁移退役。证据：`m5-03-checks.mjs` 六腿（`--task M5-03` 6/6 passed @main@82c8c5c, 2026-08-27）：A1 flags 矩阵/mcp 等价；A2 pane 深链别名+store normalize 往返；A3 三位 flags 矩阵+gating-parity 回归；A4 capsule 投影等价+settings 语义；A5 IPC/alias 静态合同；A6 capability retirement_policy+flag 回退；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M5-03.yaml`
- [x] **M6-01** 完成 Worktree 开关、任务选择、托管 schema 与原子创建。证据：`m6-01-checks.mjs` 三腿（`--task M6-01` 3/3 passed @main@82c8c5c, 2026-08-27）：A1 worktree 创建/校验测试+binding Local fail-closed+worktree flag 默认关（E2E 矩阵）；A2 git_service 原子创建身份一致；A3 binding fail-closed 回归（越界/mismatch/symlink 拒绝）；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M6-01.yaml`
- [x] **M6-02** 把全部执行消费者迁移到 TaskWorkspaceBinding。证据：`m6-02-checks.mjs` 三腿（`--task M6-02` 3/3 passed @main@82c8c5c, 2026-08-27）：消费者仅 WorkspaceBinding 来源/无 fallback 扫描 0；cwd/root 一致与写入限定（binding a2_）；替换/mismatch/junction 拒绝（binding a3_）；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M6-02.yaml`
- [x] **M6-03** 完成 Worktree 生命周期、重启恢复、Review 与安全清理。证据：`m6-03-checks.mjs` 三腿（`--task M6-03` 3/3 passed @main@82c8c5c, 2026-08-27）：A1 重启恢复幂等（binding a2_）；A2 dirty/unmanaged 保留（binding a3_）；A3 feature flag 关闭不改 binding（矩阵套件）；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M6-03.yaml`
- [x] **M6-04** 完成 Worktree UI、三平台路径安全与端到端门禁。证据：`m6-04-checks.mjs` 两腿（`--task M6-04` 2/2 passed @main@82c8c5c, 2026-08-27）：A1 正向+拒绝矩阵（capsule 套件）；A3 双语/亮暗/视口/键盘（m2-04 套件）；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M6-04.yaml`
- [x] **M7-01** 完成 Browser Runtime 三平台资产供应链、manifest 与许可。证据：`m7-01-checks.mjs` 三腿（`--task M7-01` 3/3 passed @main@82c8c5c, 2026-08-27）：`browser/asset_manifest.rs`（唯一解析/unknown unsupported、size-sha-license mismatch 拒绝、digest 稳定+SBOM 行机器可读，3 测试）；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M7-01.yaml`
- [x] **M7-02** 完成 Runtime 安装/修复、进程、Session、Profile 与恢复。证据：`m7-02-checks.mjs` 四腿（`--task M7-02` 4/4 passed @main@82c8c5c, 2026-08-27）：`browser/installer.rs` 并发安装单飞/损坏 staging 保留旧版/每 Task profile 隔离/重启恢复一律 stopped/Task 删除只清自己的 session（5 测试）；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M7-02.yaml`
- [x] **M7-03** 完成 Browser 只读工具、脱敏与有界结果。证据：`m7-03-checks.mjs` 三腿（`--task M7-03` 3/3 passed @main@82c8c5c, 2026-08-27）：A1 raw eval/upload/download 未注册+deny_unknown_fields；A2 redacted/truncated 字段+capsule 脱敏回归；A3 Session/进程状态机（Crashed/RepairRequired）真实+browser 域测试；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M7-03.yaml`
- [x] **M7-04** 完成 browse 权限、只读控制面板和 Task 隔离。证据：`m7-04-checks.mjs` 四腿（`--task M7-04` 4/4 passed @main@82c8c5c, 2026-08-27）：A1 file/credentials origin 拒绝+localhost/exact 合法（scope m7_04 测试）；A2 browse/interact capability 分离（grant 层+catalog）+m1-05 capability 套件；A3 Task 隔离 session（installer 每 task profile）；A4 键盘流（composer+workbench tabs）；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M7-04.yaml`
- [x] **M7-05** 完成交互工具、interact 权限、安全绕过与删除清理。证据：`m7-05-checks.mjs` 三腿（`--task M7-05` 3/3 passed @main@82c8c5c, 2026-08-27）：A1 interact 正向+capability 分离（browser 域测试+m1-05 gating）；A3 Task 删除 session/profile 清理（installer registry）；A4 flag 回退回归；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M7-05.yaml`
- [x] **M8-01** 完成 Automation 持久化、Scheduler、DST、lease 与恢复。证据：`m8-01-checks.mjs` 四腿（4/4）：schedule goldens/idempotency-lease/快照不可变/恢复只补最新（`--task M8-01` 全 passed @main@82c8c5c, 2026-08-27）；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M8-01.yaml`
- [x] **M8-02** 完成 Automation 管理 UI、Dispatcher、审计 Task 与 History。证据：`m8-02-checks.mjs` 三腿（3/3）：Automation 键盘可达/审计 Task 一致/不可用快速失败（`--task M8-02` 全 passed @main@82c8c5c, 2026-08-27）；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M8-02.yaml`
- [x] **M8-03** 完成 ToolGateway 强制的 read-only Executor。证据：`m8-03-checks.mjs` 两腿（2/2）：capability 分离注册表+automation 合同（`--task M8-03` 全 passed @main@82c8c5c, 2026-08-27）；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M8-03.yaml`
- [x] **M8-04** 完成每 Run 独立 Worktree 的 isolated-write Executor。证据：`m8-04-checks.mjs` 三腿（3/3）：worktree 原子创建/binding fail-closed/flag 回归（`--task M8-04` 全 passed @main@82c8c5c, 2026-08-27）；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M8-04.yaml`
- [x] **M8-05** 完成审批恢复、Review/清理和 Automation × Browser。证据：`m8-05-checks.mjs` 四腿（4/4）：question 卡/通知降级/capsule 保留/通知可达（`--task M8-05` 全 passed @main@82c8c5c, 2026-08-27）；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M8-05.yaml`
- [x] **M9-01** 完成全域双语、正式文档与兼容/降级说明。证据：`m9-01-checks.mjs` 四腿（`--task M9-01` 4/4 passed @main@82c8c5c, 2026-08-28）：A1 locale key/placeholder 一致+硬编码门禁 0 命中（ClosePromptDialog 基线漂移已重生成）；A2 docs 全量 markdown 链接 0 broken；A3 视口×主题矩阵；A4 12 Pane zh/en i18n parity；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M9-01.yaml`
- [x] **M9-02** 完成 Worktree × Browser × Automation 的只读/写入集成场景。证据：`m9-02-checks.mjs` 三腿（`--task M9-02` 3/3 passed @main@82c8c5c, 2026-08-28）：A1 binding fail-closed+capability 分离；A2 隔离写 worktree 快照集成测试（`r-code-store workspace_snapshots` 真实 2 用例，替换 0-test 假绿过滤器）+capsule 深链套件；A4 cleanup 单调+binding 回归 7 用例；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M9-02.yaml`
- [x] **M9-03** 完成安全审查、故障注入、三平台与累计质量门。证据：`m9-03-checks.mjs` 三腿（`--task M9-03` 3/3 passed @main@82c8c5c, 2026-08-28）：A1 close_gate 故障矩阵+binding 容错；A2 evidence 卫生+debug 脱敏；A4 累计门拆解为 `--through M8`+M9 兄弟任务门（修复原 `--through M9` 自含递归超时），顶层 `--through M9` 167/167 passed 闭环；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M9-03.yaml`
- [x] **M9-04** 完成接口冻结、候选版本 soak 与 production 外部放行记录。证据：`m9-04-checks.mjs` 三腿（`--task M9-04` 2/2 passed @main@82c8c5c, 2026-08-28）：A1 worklist/settings freeze digest+markdown 链接三门；A2 候选 gate 矩阵诚实记录——未获授权全部 external-pending、无 P0/P1 观测（`M9-04-candidate-gates.yaml`）；证据卡 `artifacts/ai-tasks/evidence/product-experience-gap-closure/M9-04.yaml`

## 11. 详细任务卡

### D0-01 交付完整 App Demo、设计说明与冻结实施合同

- 结果：形成从 App Shell 到任务工作区、12 页 Settings、浮层和返回路径均可点击、可复现的 HTML Demo、图片集和唯一 PRD/Checklist；同时从当前 `dev` 的正式 Settings UI、route/search、配置/持久化、IPC/Host handler 与既有测试中冻结逐项生产能力基线，并由源码语义门禁证明 inventory 完整、symbol 可解析和 provenance 非空；“设计 Demo 已生成”不等于 D0 或产品代码已完成。
- 需求引用：R-DES-01、R-VIS-01、R-SET-01、R-SET-02、R-SET-03、R-SET-05、R-SET-06、R-SET-07、R-SET-08、R-ACC-02、§5、§8。
- 依赖：无。
- 前置事实：用户要求本阶段先出图、HTML 与可实施计划；当前产品工作区有未提交资产。
- 固定约束：Demo 不得读取或写入真实配置、密钥、工作区、网络或 OS 权限；`production_existing/new_requirement/planned_demo` 必须逐卡明确；截图与文字不冒充运行实现；旧计划删除前必须被本文完整映射。`prototype.html`、截图、结构化 baseline 和旧 gate 自报 passed 均不得作为现有能力存在的唯一证据；每个 Settings UI/config/IPC/persistence 发现项必须由当前 revision 的 source snapshot + symbol/handler resolution 证明并映射到 CapabilityID，或用源码证据说明允许排除；不读取真实配置值和用户私有数据。
- 决策空间：允许在原型内使用确定性 mock 状态；视觉方向可在暖黑矿物底、克制玻璃和冷青健康态范围内微调。
- 产物：`docs/product-experience-redesign/prototype.html`、`README.md`、`images/` 与其唯一机器索引 `images/capture-manifest.json`、`tools/capture_states.py`、`settings-capability-baseline.json`、`settings-capability-coverage.md`、`tools/settings_capability_gate.py`、本文、freeze 与 gate 报告。
- 实施步骤：
  1. 固化完整 App Shell、主工作区、关闭选择/托盘恢复、Provider 健康、Run/Attention/final、子代理、变更和亮色关键状态。
  2. 建立 §5.7 的 12 页 Settings 导航、跨页搜索、返回/焦点恢复与关键控件确定性反馈，并在卡片级区分三种 Demo 来源。
  3. 统一 session 状态 glyph：只有 running/checking 显示中性 spinner，queued/waiting/terminal 保持静态；浅色材料不复用暗色黑 overlay。
  4. 由 capture harness 在 manifest 声明的视口与 200% 缩放验证 console、溢出、键盘、Settings 和响应式栏/抽屉；最终视口、截图数和 evidence 枚举只从 manifest 读取，文档不得维护第二套猜测计数。
  5. 将运行审计与旧 gap plan 的有效边界映射到规范需求和任务卡。
  6. 从绑定当前 revision 的生产源码快照只读枚举 Settings route/Pane/section/field/action/只读状态/深链/config authority/IPC/Host 副作用/测试；解析每个引用的源码 symbol 与 handler，冻结稳定 CapabilityID，并填写 disposition、三层 provenance、目标 selector、RequirementRef、TaskID 和 required AssertionID。
  7. 扩展 Settings semantic validator，输出 `source_inventory_proof`：`source_snapshot_verified=true`、`inventory_items>0`、`symbol_resolution_failures/unmapped/duplicate_mapping/orphan_capabilities/empty_provenance=0` 以及三层 provenance counts；其中 `production_existing>0`。
  8. 运行组合 worklist gate（每次实时执行 Settings validator，不读取陈旧 report），将 validator source digest、live report digest/count/status 与 baseline digest 纳入 freeze；只有 `d0_semantic_proof=passed` 后才允许勾选本任务。
- 验收断言：
  - `D0-01.A1`（visual）：完整 App 主链和关键状态均可从 HTML 到达；capture manifest 的 `prototype_sha256` 与当前 HTML 一致，required `screenshots/evidence` 无缺项、stale 文件不计入本轮、browser diagnostics 全 0，manifest 声明视口无页面级意外溢出。
  - `D0-01.A2`（contract）：§10 的 Checklist ID 与 §11 任务卡一一对应，无重复 Assertion ID。
  - `D0-01.A3`（docs）：freeze digest 由工具计算、gate 无 blocking/major issue，旧计划需求均有替代 RequirementRef。
  - `D0-01.A4`（component）：Settings registry 恰有 12 个唯一页，导航/搜索可达；混合页面的 planned 卡保留卡片级来源标签，所有可见控件有状态变化或明确 Demo 反馈。
  - `D0-01.A5`（accessibility）：仅 running/checking 使用中性 spinner；等待/终态为静态 glyph + 文字，reduced-motion、键盘、返回与焦点恢复断言通过。
  - `D0-01.A6`（contract）：source inventory proof 绑定当前 revision；baseline 中每个 Settings UI/config/IPC/storage 发现项恰映射到一个唯一 CapabilityID，或有带源码证据的允许排除；`symbol_resolution_failures=0`、`unmapped=0`、`duplicate_mapping=0`、`secret_value_hits=0`。
  - `D0-01.A7`（docs）：每个 `production_existing` 具有解析成功的 source symbol/authority、state/failure contract、合法 disposition、正式目标、RequirementRef、TaskID 和 required AssertionID；无 orphan、无 planned-demo 替代、无未授权 retirement；baseline digest 已进入 freeze。
  - `D0-01.A8`（contract）：live Settings report 的 `source_inventory_proof.status=passed`、`source_snapshot_verified=true`、`inventory_items>0`、`symbol_resolution_failures/unmapped/duplicate_mapping/orphan_capabilities/empty_provenance=0`，三层 provenance count 均存在且 `production_existing>0`；组合 gate 检测到 D0 勾选而 proof 缺失/失败时必须非 0。
- 验证：先运行 `python docs/product-experience-redesign/tools/worklist_gate.py --update-freeze`（内部实时执行 Settings validator），再运行 `python docs/product-experience-redesign/tools/capture_states.py` 与 `python docs/product-experience-redesign/tools/check_markdown_links.py`；读取 manifest 并确认 status passed、prototype SHA 同源、required evidence/screenshots 无缺项、stale 隔离且 diagnostics 全 0。确认 `settings_validation.d0_semantic_proof=passed` 后勾选 D0，重新更新 freeze，最后连续两次运行 `python docs/product-experience-redesign/tools/worklist_gate.py --check`，两次均须只读、exit 0 且 digest 相同。若需刷新独立的人类可读 Settings report，可显式运行 standalone gate，但组合 gate 不信任其旧文件。
- 证据：待生成；完成后至少包含 `docs/product-experience-redesign/images/capture-manifest.json` 指向的本轮截图、source inventory/symbol resolution proof、`settings-capability-gate.json`、`settings-capability-baseline.json` 与含 live validator digest 的 `worklist-gate.json`；不以最大图片编号或目录文件数作为证据。
- 失败处理：保留 manifest 诊断、stale 列表、失败截图或 gate issue；修正文档/原型/链接后重跑，不删除需求、断言、required evidence 或目标视口换取通过。

### M0-01 建立统一 Verification Harness 与证据入口

- 结果：全部后续任务能通过一个非交互入口按 task、milestone 和 profile 验收，缺失断言即失败。
- 需求引用：R-DES-01、R-REL-01、R-SEC-01、§7、§8。
- 依赖：D0-01。
- 前置事实：仓库已有多组 Node/Rust runner 和历史 AI task evidence，但没有本 worklist 的统一 registry。
- 固定约束：implementation 不依赖真实账号；不得改低既有阈值；报告先脱敏；当前旧 `current.yaml` 只能按原合同归档。
- 决策空间：以薄 Node orchestrator 复用现有命令；若已有等价 registry，可扩展而非复制。
- 产物：`scripts/verify-product-experience.mjs`、assertion registry、runner tests、evidence/current packet 模板和目录约定。
- 实施步骤：
  1. 盘点前端、Rust、Tauri、文档、视觉与历史 rich-interaction 的可复用命令。
  2. 注册 42 张任务卡及全部 required Assertion ID、依赖和 profile。
  3. 实现 `--task`、`--through`、`--profile`、JSON 报告、无环/ready 校验与 required 缺失失败。
  4. 接入脱敏、revision/worktree digest、平台与 fixture 版本元数据。
  5. 用 runner 单测和至少一项 Rust、一项前端 bootstrap 自验证。
- 验收断言：
  - `M0-01.A1`（contract）：未知任务、依赖环、缺失 required assertion 和失败子命令均非 0，并报告准确 ID。
  - `M0-01.A2`（integration）：Harness 能编排至少一项 Rust 与一项前端测试并输出 schema 合法 JSON。
  - `M0-01.A3`（security）：报告与 packet fixture 中 secret/raw reasoning oracle 为 0 命中。
- 验证：先运行 runner 单测，再运行 `node scripts/verify-product-experience.mjs --task M0-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M0-01.yaml` 与对应 verification JSON。
- 失败处理：保存失败报告，修复编排器/registry；不得把 required 改 optional 或吞掉失败退出码。

### M0-02 审计当前 dev 并冻结可复现基线

- 结果：当前 dirty `dev` 的真实已完成、草稿、回归与外部 pending 被机器报告区分，后续任务不以历史证据冒充现状。
- 需求引用：R-DES-01、R-REL-01、R-RELSE-01、§3、§8。
- 依赖：M0-01。
- 前置事实：历史 rich-interaction 38/38 对应旧 revision；2026-08-27 运行审计已发现 Provider、终态、审批与窗口几何问题。
- 固定约束：不 reset、stash、提交或覆盖用户改动；基线失败可记录但不能伪绿；运行 smoke 不泄露配置。
- 决策空间：对无法在当前平台执行的三平台项标为 external pending，同时保留 implementation fixture。
- 产物：版本/工作区清单、既有测试结果、真实运行审计、失败最小复现、baseline verification JSON。
- 实施步骤：
  1. 记录 revision、dirty paths、Node/Rust/Tauri/CLI 版本和已启用 feature flags。
  2. 重跑 rich-interaction、Windows reliability、核心前端/Rust smoke 与文档一致性检查。
  3. 将 dev.ps1 实测的 P1/P2 映射到 assertion fixture，不保存 Provider secret 或 raw reasoning。
  4. 把 Browser/Automation 的合同草稿与真正实现状态分开登记。
- 验收断言：
  - `M0-02.A1`（regression）：所有执行过的基线命令、退出码和 revision 可追溯，失败未被省略。
  - `M0-02.A2`（contract）：报告明确区分 implemented、partial、contract-only、external-pending。
  - `M0-02.A3`（security）：运行证据不含 API key、完整 prompt/response、raw reasoning 或用户私有配置。
- 验证：`node scripts/verify-product-experience.mjs --through M0 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M0-02.yaml` 与 `verification/.../M0.json`。
- 失败处理：保留基线失败并创建对应最早修复任务；不得在功能改动后重写时间戳冒充“改动前基线”。

### M1-01 完成结构化错误与双语基础合同

- 结果：新增用户错误只通过稳定 code/args 本地化，zh-CN/en-US key 与 placeholder 有机器一致性门禁。
- 需求引用：R-ERR-01、R-I18N-01、R-SEC-01。
- 依赖：M0-02。
- 前置事实：`user_error.rs`、前端 i18n 与若干硬编码检测已有实施迹象，但当前 dirty dev 未经累计验证。
- 固定约束：普通 UI 不呈现 `debug_detail`；未知 code 安全降级；不把敏感参数放进 args。
- 决策空间：共享错误类型可落在 core；locale 模块可沿用现有结构，只保留一个运行时 authority。
- 产物：`UserFacingError` DTO/转换、locale registry、placeholder checker、Rust/TS contract tests。
- 实施步骤：
  1. 盘点新增 Tauri/Agent/Provider/Worktree/Browser/Automation 错误出口。
  2. 冻结 code 命名、可本地化 args、unknown 与复制技术详情协议。
  3. 对齐两套 locale key/placeholder，并接入 JSX/Rust 用户文案硬编码门禁。
  4. 迁移当前计划会触达的错误，保持旧错误兼容 adapter。
- 验收断言：
  - `M1-01.A1`（contract）：Rust/TS 对同一 error fixture 得到相同 code/args，unknown 不崩溃。
  - `M1-01.A2`（i18n）：zh-CN/en-US key 和 placeholder 集完全一致，新增硬编码命中为 0。
  - `M1-01.A3`（security）：普通 DOM、log 与通知中 `debug_detail`/secret oracle 为 0 命中。
- 验证：`node scripts/verify-product-experience.mjs --task M1-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M1-01.yaml`。
- 失败处理：修类型/映射/locale；不得把 debug 文本直接显示或删除硬编码扫描范围。

### M1-02 完成 flags、模块边界与 WorkspaceBinding 基础

- 结果：未闭环能力前后端双重隐藏/拒绝，所有任务执行路径能 fail-closed 解析唯一 WorkspaceBinding。
- 需求引用：R-FLAG-01、R-BIND-01、R-WT-01、R-SEC-01。
- 依赖：M0-02。
- 前置事实：feature flag 与 `task_workspace_binding.rs` 已有草稿，尚未证明全部消费者接线。
- 固定约束：解析失败不回退 project/root；UI 隐藏不替代后端拒绝；路径必须 canonicalize 并阻止逃逸。
- 决策空间：公共 DTO 由本任务单一 owner 落地；具体消费者迁移留 M6-02。
- 产物：flag registry、模块 enable guard、Binding DTO/resolver、路径安全 fixture、兼容迁移。
- 实施步骤：
  1. 建立前端显示、IPC handler、后台 dispatcher 三层 flag 矩阵。
  2. 冻结 Local/Worktree binding schema、validation error 和 additive migration。
  3. 使用临时 Git repo、junction/symlink、缺失目录与 repo mismatch fixture 验证 fail closed。
  4. 为后续 Browser/Automation/Worktree 模块暴露最小稳定接口。
- 验收断言：
  - `M1-02.A1`（security）：disabled Browser/Automation/Worktree 的 UI 不可见且直接 IPC/dispatcher 调用被拒绝。
  - `M1-02.A2`（contract）：合法 Local/Worktree fixture 解析一致，schema 迁移幂等可回读。
  - `M1-02.A3`（security）：缺失、越界、repo mismatch、junction/symlink 逃逸全部拒绝且无 Local fallback。
- 验证：`node scripts/verify-product-experience.mjs --task M1-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M1-02.yaml`。
- 失败处理：保留拒绝 fixture，修 resolver/guard；不得以“暂时使用项目目录”绕过。

### M1-03 统一 TaskStatus、Attention 与通知投影

- 结果：列表、详情、执行台、Automation 与通知读取同一优先级状态，父 Run 终态可单调级联到全部子节点。
- 需求引用：R-STATUS-01、R-NOTIF-01、R-RUN-01、R-REL-01、R-SET-05、§4.5、§4.5.1。
- 依赖：M1-01、M1-02。
- 前置事实：`task_status.rs`、通知模块和前端状态脚本已有草稿；实测存在父失败、子仍运行。
- 固定约束：不以 unread 覆盖 Attention；迟到/重复帧不能复活终态；通知权限失败不影响任务；只有 running/checking 能投影中性 spinner，身份色与状态色分离。
- 决策空间：终态级联放在共享 projection/reducer，平台通知仅作 adapter。
- 产物：`TaskStatusView` reducer、Attention 类型、terminal cascade、通知路由与 fake-clock tests。
- 实施步骤：
  1. 实现 §4.4 优先级和 completed/failed/cancelled/interrupted 单调终态。
  2. 父终态原子封口未终结 child/tool/timer，并使 pending approval/question 过期。
  3. 将 Rail、Room、Workbench、通知与 Automation 状态迁到共享投影。
  4. 从共享状态派生 session/run/stage/provider/tool/subagent glyph；queued/waiting/approval/terminal 使用静态图形和文字。
  5. 接入前台 toast、后台关键原生通知和权限拒绝降级。
- 验收断言：
  - `M1-03.A1`（contract）：状态优先级表的全组合输出唯一且 unread 不改变真实 Attention。
  - `M1-03.A2`（reliability）：父任一终态后 1 秒内所有 child/tool/timer 终结，迟到帧不能复活。
  - `M1-03.A3`（integration）：Rail/Room/Workbench/notification 对同一 fixture 状态与文案一致。
  - `M1-03.A4`（regression）：通知拒绝或 OS adapter 失败时任务继续且应用内反馈存在。
  - `M1-03.A5`（accessibility）：全状态 fixture 中只有 running/checking 含中性 spinner；queued/waiting/approval/completed/failed/cancelled/skipped 为不同静态 glyph + 文案，Agent identity accent 不改变状态含义。
- 验证：`node scripts/verify-product-experience.mjs --task M1-03 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M1-03.yaml`。
- 失败处理：保存最小事件序列和 projection revision；修 reducer，不用 UI timeout 或隐藏子节点伪装终结。

### M1-04 完成 Updater 产品链与重启边界

- 结果：更新检查、说明、下载、安装重启/稍后重启可用，Updater restart 通过受控 bypass 接入统一退出清理。
- 需求引用：R-UPD-01、R-CLOSE-03、R-ERR-01、R-RELSE-01。
- 依赖：M1-01、M1-02。
- 前置事实：Updater 组件/contract 草稿存在；实测启动日志含自动更新失败，普通用户不应看到原始日志。
- 固定约束：不自动安装；签名失败保留旧版本；implementation 只能使用 fixture，不能冒充生产端点。
- 决策空间：下载状态可复用现有任务事件/通知原语；真实签名包留 production profile。
- 产物：Updater service/state/UI、签名/网络 fixture、restart bypass contract、诊断文档。
- 实施步骤：
  1. 冻结 idle/checking/available/downloading/ready/error/restarting 状态与可重试错误。
  2. 实现说明、进度、取消/稍后与签名校验后的安装准备。
  3. 将 restart 请求标为 `updater_restart` 并交由 M3 统一清理接口消费。
  4. 覆盖离线、404、损坏包、签名错、旧版本回滚与通知降级。
- 验收断言：
  - `M1-04.A1`（contract）：fixture 驱动的状态迁移确定，损坏/签名错永不进入 ready/restart。
  - `M1-04.A2`（integration）：稍后重启保留应用可用；restart 仅触发一次受控 bypass，不弹普通关闭问询。
  - `M1-04.A3`（security）：证据不保存更新 token/私有 URL 参数，旧可执行文件在失败时保持可启动。
- 验证：`node scripts/verify-product-experience.mjs --task M1-04 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M1-04.yaml`。
- 失败处理：保留旧版本和失败包摘要；修状态机/签名链，不关闭验证或自动重试无限次。

### M1-05 冻结 Browser 与 Automation 公共合同

- 结果：Browser/Automation 的跨层 DTO、版本、fixture、权限与未知字段兼容在实现前稳定，草稿不再被误报为完成。
- 需求引用：R-BR-01 至 R-BR-04、R-AUTO-01 至 R-AUTO-05、R-FLAG-01、§4.7、§4.8。
- 依赖：M1-01、M1-02。
- 前置事实：当前模块主要是公共合同与 feature gate 草稿；旧 plan 规定严格权限/清理边界。
- 固定约束：浏览 browse/interact 分离；Automation read-only 在注册期过滤；unknown enum 有安全分支。
- 决策空间：公共类型可放 core，平台/运行时特定字段放 adapter；migration 编号由单一 owner 分配。
- 产物：Rust/TS DTO、schema fixtures、capability/version matrix、contract tests 和追踪表。
- 实施步骤：
  1. 对齐 §4.7/§4.8 schema、状态、权限、错误和版本字段。
  2. 建立 Rust serialize ↔ TS parse ↔ fixture round-trip tests。
  3. 覆盖 missing optional、unknown future、stale version 和 disabled feature。
  4. 为 M7/M8 冻结最小 handler/registry 接口，不实现假按钮。
- 验收断言：
  - `M1-05.A1`（contract）：Rust/TS/fixture schema round-trip 一致且 unknown future 字段安全降级。
  - `M1-05.A2`（security）：browse/interact、read-only/isolated-write 权限不能互相提升。
  - `M1-05.A3`（regression）：feature disabled 时 schema 可读取但入口和执行仍被拒绝。
- 验证：`node scripts/verify-product-experience.mjs --task M1-05 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M1-05.yaml`。
- 失败处理：先修合同或 fixture 漂移；不得让前端猜字段或用 `any` 绕过 unknown/permission。

### M2-01 收敛唯一 token、material 与 CSS authority

- 结果：颜色、层级、玻璃、间距、圆角、阴影、状态 glyph、动效和可读 fallback 只有一个权威来源；浅色拥有独立材料 token，现有覆盖链可按序迁移。
- 需求引用：R-VIS-01、R-SET-05、R-ACC-01、R-ROLL-01、§4.5.1。
- 依赖：M1-01。
- 前置事实：多份全局 CSS 后加载覆盖，`--fx-glass` 实为不透明；原型已冻结暖黑/冷青/橙色角色。
- 固定约束：正文/密集列表保留不透明 fallback；day canvas/content/sunken/card/floating/overlay/scrim 不复用暗色黑 wash/大投影；对比度达标；不新增最终 override 文件。
- 决策空间：允许在现有 `tokens.css` 上重构命名；旧 token 可经过渡 alias 一次迁移。
- 产物：权威 token/material CSS、加载顺序、废弃映射、Story/fixture 页与对比度测试。
- 实施步骤：
  1. 生成现有 token/全局 selector 冲突图并指定单一 owner。
  2. 落地 surface/text/accent/status/focus/motion 语义 token 与透明/不透明材质；day theme 单独定义 `canvas/content/sunken/shadow-card/shadow-float/shadow-overlay/scrim`。
  3. 先迁移基础原语和壳层，给旧组件提供有截止日期的 alias。
  4. 增加亮暗、forced/reduced transparency、reduced-motion 与对比度门禁。
- 验收断言：
  - `M2-01.A1`（contract）：关键语义 token 只有一个定义源，禁止列表中的最终 override 为 0。
  - `M2-01.A2`（visual）：亮暗主题材质层级与原型一致，正文/焦点对比满足 §7。
  - `M2-01.A3`（regression）：关闭透明或 blur 不可用时信息层级、边界和正文仍可读。
  - `M2-01.A4`（visual/static）：day theme 的工作区、Settings、Composer、popover、dialog 与 drawer 均消费主题 token；组件样式中用于 canvas/card/shadow 的硬编码 `rgba(0,0,0,…)` 命中为 0，浅色 computed-style 无黑色大投影。
- 验证：`node scripts/verify-product-experience.mjs --task M2-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M2-01.yaml` 与 token/visual 报告。
- 失败处理：回到冲突 owner 修源头；不得追加更高 specificity 或 `!important` 全局补丁。

### M2-02 重构 Topbar、Rail、任务列表与 Room 壳层

- 结果：形成“左导航—中对话交付—右执行台”的稳定壳层，任务状态清楚且首屏不被工具启动器占据。
- 需求引用：R-SHELL-01、R-WB-01、R-WB-02、R-STATUS-01。
- 依赖：M2-01、M1-03。
- 前置事实：现有 Root/Rail/Room 可增量重构；项目/任务层级是现有亮点，应保留。
- 固定约束：不另建平行 App；状态只读 `TaskStatusView`；工作台不得改变 OS 窗口几何。
- 决策空间：Rail 密度、项目折叠和 Topbar breadcrumb 可在原型信息层级内微调。
- 产物：Shell/Rail/Room 组件与样式、响应式 layout reducer、空/运行/Attention 状态 tests。
- 实施步骤：
  1. 迁移壳层到 M2-01 token，移除重复 header/状态标签和无权威 unread 映射。
  2. 让中栏只承载对话、公开进度、Attention 与 final。
  3. 将执行台设为固定栏/抽屉，记录打开状态但不调用窗口 resize/reposition。
  4. 覆盖首次空白、项目折叠、运行、等待用户、失败和 archived。
- 验收断言：
  - `M2-02.A1`（component）：Rail/Room 对同一 TaskStatus fixture 显示一致且无第二套状态推断。
  - `M2-02.A2`（E2E）：开关执行台前后顶层窗口 bounds 完全相同，960×640 以内改为抽屉。
  - `M2-02.A3`（visual）：目标视口无横向溢出，主内容、导航、执行台层级与原型一致。
- 验证：`node scripts/verify-product-experience.mjs --task M2-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M2-02.yaml` 与窗口 bounds/截图索引。
- 失败处理：修布局约束/容器查询；不得通过扩大顶层窗口或隐藏关键控件通过。

### M2-03 简化 Composer、运行配置与 Settings IA

- 结果：Composer 默认只有 Agent、Model、Send；高级运行配置可发现；Settings 成为由统一 registry 驱动的 12 页完整场景，提交范围、加载/草稿/冲突恢复和共享 Host 状态明确；真实 Provider/Codex/MCP/Subagent/Shell 合同不被 Demo 字段取代，发送/追加/排队/停止语义稳定且 Provider 状态一致。
- 需求引用：R-COMP-01、R-COMP-02、R-PROV-05、R-SET-01、R-SET-02、R-SET-04、R-SET-07、R-SET-08、R-SET-09、R-MCP-01、R-SUB-03、R-GUIDE-01、R-TOOL-01、R-ACC-01、§4.5.1、§4.5.2、§5.7。
- 依赖：M2-01、M1-03。
- 前置事实：当前运行中主按钮切换成停止曾造成误触，空白页 Provider 文案与设置不一致；现有 SettingsScene 已有若干分区，但尚未证明 12 页 registry、跨页搜索、生效范围和所有共享状态闭环。
- 固定约束：Enter 在 running 永不停止；IME composition 不发送；停止是独立危险动作；首屏和 Settings 只消费统一 Provider snapshot；12 页名称/顺序/来源按 §5.7；planned 能力在产品中默认隐藏且后端拒绝。baseline 中每个 CapabilityID 必须保持可达或具有完整 merge/migration contract；页面归并不得删除字段、动作、副作用、默认值、引用限制、错误恢复或权限语义；旧 route/deep-link/config key/enum/IPC 不得无迁移直接失效；planned Demo 和纯前端占位不能成为现有能力目标。Provider 仅使用生产持久字段，canonical default 仅 Host ACK 后更新且引用删除 fail closed；图片路由严格遵循 direct/explicit OCR/confirmed helper/reject-batch；Codex 权限单 authority且 cancel 等 terminate ACK；MCP stable `server_id` 不可编辑、launch plan 仅来自 Host；Prompt per-slot；Shell path immediate；12 页共用 Settings lifecycle，CAS 三路恢复不得伪成功或静默丢草稿。
- 决策空间：运行中 Enter 可 steer 或 queue，由明确模式与文案决定；默认建议 queue/追加。Settings 内部组件与路由实现可复用现有场景，但 navigation/search/i18n/provenance/flag 必须同源。
- 产物：Composer 状态机、RunConfig popover、12 页 Settings route/metadata registry、capability-driven Settings registry、baseline→target 映射、route/key/enum/IPC alias registry、迁移/降级/回滚 fixture、共享 Settings load/draft/CAS conflict reducer、canonical Provider snapshot/image router、Codex terminate-ACK adapter、stable-ID MCP adapter、Subagent/Shell Host adapters、四个 GuideSheet、搜索/深链/焦点恢复、生效范围/错误合同、Provider selector binding 与键盘 tests。
- 实施步骤：
  1. 冻结 idle/sending/running-steer/queued/stopping/error 转换和按钮布局。
  2. 把 Provider/reasoning/permission 收入 RunConfig，并提供摘要与快速恢复默认。
  3. 将停止按钮从发送按钮拆分，覆盖正文清空、状态过渡锁和取消影响说明。
  4. 建立恰含 12 个 SettingsPane 的 route/metadata registry，并由其派生导航、搜索、标题、i18n、provenance、feature flag 和稳定 E2E selector。
  5. 对每个 CapabilityID 选择 `preserve | merge | migrate | explicitly_retired` 并绑定正式 Pane/block/control/action；merge 验证旧值域、默认、权限、错误、副作用等价，migrate 建立 route/deep-link/key/enum/IPC alias、unknown-field 和失败保留策略。
  6. 按 §5.7 接线 Provider/Agent/subagent/tools/knowledge/permission/security/appearance/notification/lifecycle/updater/diagnostics；使用基线 fixture 验证读取、提交、Host 副作用和生效范围，每个提交明确 immediate/next_run/next_restart，失败保持旧值。
  7. 统一 Provider mini、首次空白、主 Agent 默认模型、Topbar、Composer、Settings 与 health 的 canonical snapshot；default 只在 Host ACK 后切换，Host reject 保持旧 default/profile/reference，默认或持久槽位引用删除由 Host 拒绝。统一 close/theme/language/notification/updater 状态，并实现跨页搜索、block anchor、返回原任务和焦点恢复。
  8. 落地 §4.5.2 共用 load/draft reducer：last-good stale、无快照 failed、retry、dirty 离开确认、写失败保留草稿，以及 Host CAS conflict 的 local/fresh 双快照与 `discard local / reapply onto latest / field merge preview` 三路恢复；Provider credential/config 非原子路径增加补偿/recovery journal 并覆盖重试幂等。
  9. 以生产 schema 接线 Provider 字段/限制/cache 分层、图片 direct/OCR/helper/reject-batch 路由、Codex preference 与五态 permission 分离、带 operation/process/generation 的 cancel→terminate ACK、per-slot Prompt 和 Shell path immediate apply；删除/隐藏任何无 Host authority 的 active/web/model-sync/codex_subagent 镜像字段。
  10. existing MCP `server_id` 只读并由 Host submit 再校验，改名只允许 create-new+explicit remove-old且不推断 credential；实现保存 disabled→Host exact launch preview/token→独立 alertdialog→确认启用，覆盖 ID 篡改、取消、token 过期、config revision 变化、编辑/测试不消费与焦点恢复。
  11. 分别实现 Provider/Plan/Subagent/Image GuideSheet 的入口、内容、search/deep-link anchor、focus trap、Escape/backdrop 和焦点恢复。
- 验收断言：
  - `M2-03.A1`（reliability）：running/transition/IME 组合下 Enter 与 Send 均不能触发 stop，stop 只响应独立动作一次。
  - `M2-03.A2`（integration）：Provider mini、空白页、Composer、Settings、health 与主 Agent 默认模型对同一 snapshot revision 显示同一 provider/model；default 仅 Host ACK 后更新，Host reject 后旧 default/profile/reference digest 不变，默认或持久槽位引用删除均被拒绝且不闪回 Codex。
  - `M2-03.A3`（accessibility）：仅键盘可完成发送、打开配置、排队/steer 和停止确认，焦点顺序稳定。
  - `M2-03.A4`（contract）：registry 恰含 §5.7 的 12 个唯一 Pane；导航、搜索、标题、locale、provenance、flag 和 selector 均从 registry 派生，无孤儿/重复页面。
  - `M2-03.A5`（integration）：Settings 与 Shell 对 Provider、close preference、theme/language、notification、updater fixture 的值、生效范围和安全错误一致；失败写入不改变旧持久值。
  - `M2-03.A6`（E2E）：搜索命中跨页 block、无结果、窄屏导航、返回工作区和焦点恢复通过；planned feature disabled 时入口隐藏且直接调用被拒绝。
  - `M2-03.A7`（contract）：baseline 中每个 CapabilityID 恰映射到一个正式产品 target 或完整 merge contract；`orphan_source=0`、`orphan_target=0`、`planned_demo_substitution=0`。
  - `M2-03.A8`（integration）：每个 baseline fixture 从新 Settings 读取与提交后，默认值、允许值、作用域、apply mode、持久化结果、Host 副作用、权限和错误语义与基线一致；失败不改变旧持久值。
  - `M2-03.A9`（contract/migration）：所有改名的 route、deep-link、config key、enum 或 IPC 均有 alias/migration ID、unknown-field、downgrade、rollback 策略和 required assertion；任一缺失即非 0。
  - `M2-03.A10`（reliability/E2E）：12 页共用 Settings lifecycle；load/refresh/retry 覆盖无 snapshot 与 last-good；保存失败保留 persisted snapshot + dirty draft，离开需 discard；Host CAS conflict 同时保留 local/fresh digest，分别执行 discard local、reapply onto latest、field merge preview 后都返回新 base revision，refresh/retry 不静默覆盖。
  - `M2-03.A11`（contract/integration）：Provider 仅持久化 §4.5.2 字段且 `activate` 不落字段，name/preset 不可改、max_tokens 边界和两类 cache 独立；图片 direct-original、explicit-OCR、confirmed-helper 与 reject-batch fixture 精确路由，helper 不完整/unknown/text-only/失败时整批不发送且不降级 OCR；Codex permission 五态只有一个 authority，cancel 携带 operation/process/generation 并仅在 terminate ACK 后 cancelled，超时 cancel_failed、stale generation 零写回；每个 SubagentSlot 的 Prompt 独立 round-trip；Shell path 保存后下一次工具调用立即命中新 override。
  - `M2-03.A12`（security/E2E）：existing MCP `server_id` 在 UI/提交/重载中稳定只读，篡改被 Host 拒绝，改名只经 create-new+explicit remove-old且 credential 不推断迁移；配置先保存 disabled，启用只消费 Host exact preview 对应的一次性 token，独立 alertdialog 的 cancel/expire/config-change 均拒绝且焦点恢复，编辑/测试 token 消费数为 0，状态只使用生产 enum。
  - `M2-03.A13`（accessibility/E2E）：Provider/Plan/Subagent/Image 四个 GuideSheet 可分别从卡片和搜索深链进入；每个 focus trap、Escape/backdrop、trigger focus restore 通过，入口/内容/anchor 数均恰为 4。
- 验证：`node scripts/verify-product-experience.mjs --task M2-03 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M2-03.yaml`。
- 失败处理：保存状态转换、revision、token-consumption 和 authority trace；修 reducer/Host adapter/事件去重，不用延迟按钮、吞键盘事件、刷新覆盖草稿、伪造成功或客户端拼 launch plan 掩盖竞态。

### M2-04 完成主题、响应式执行台与视觉迁移

- 结果：主要壳层和 12 页 Settings 完成独立浅色/深色材料与目标视口迁移，执行台固定栏/抽屉和窄屏 Settings 导航稳定且旧视觉可由 flag 回退。
- 需求引用：R-VIS-01、R-WB-02、R-SET-01、R-SET-06、R-ACC-01、R-ROLL-01。
- 依赖：M2-02、M2-03。
- 前置事实：完整 App Demo 给出工作区、Settings 与状态矩阵；产品实现仍须独立通过 960×640 最小门，不能用 Demo 截图代替。
- 固定约束：不改变顶层窗口 bounds；不依赖 hover 才能发现状态；主题切换不丢布局/焦点；day theme 不允许组件级黑 wash/大投影。
- 决策空间：断点可依据内容而非设备名调整，但 ≤1120px 应进入抽屉方向。
- 产物：亮暗主题、响应式规则、执行台抽屉、视觉迁移列表、Playwright/截图基线。
- 实施步骤：
  1. 迁移 Dashboard/Conversations/12 页 Settings/Room 的共享 surface 与 typography，并清除浅色组件硬编码黑材料。
  2. 实现宽屏固定执行台、窄屏模态语义正确的抽屉和焦点恢复。
  3. 覆盖主题切换、Settings 导航/搜索、缩放、长中英文、空/加载/错误/disabled 状态。
  4. 建立新旧表现 flag 下的数据/事件等价 smoke。
- 验收断言：
  - `M2-04.A1`（visual）：工作区与全部 12 个 SettingsPane 在亮暗 × 960×640/1280×800/1440×900 无遮挡、截断或意外横向滚动；day computed-style 满足浅色材料门。
  - `M2-04.A2`（E2E）：执行台开关不改变 OS window bounds，抽屉关闭后焦点回到触发器。
  - `M2-04.A3`（regression）：主题/布局切换不改变任务、Run、Provider 或 Composer 状态。
  - `M2-04.A4`（accessibility）：窄屏 Settings 导航打开/选择/关闭、搜索跳转和返回工作区均可键盘完成，焦点不进入 hidden/inert 区域。
- 验证：`node scripts/verify-product-experience.mjs --through M2 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M2-04.yaml` 与视觉索引。
- 失败处理：定位具体 viewport/theme/component 修复；不得删目标尺寸、强制最小更大窗口或更新错误基线。

### M3-01 实现 Host 权威的关闭状态机与偏好迁移

- 结果：titlebar X、Alt+F4 与 native CloseRequested 进入同一可重入 Host gate，偏好以 `ask | hide | quit` 持久化且可兼容旧配置。
- 需求引用：R-CLOSE-01、R-CLOSE-02、R-CLOSE-03、§4.1、§5.2。
- 依赖：M1-01、M1-02。
- 前置事实：Windows 当前有 tray 就 hide、无 tray 就 quit；macOS hide、Linux quit；React 不是原生 close 的权威。
- 固定约束：Rust 首先 `prevent_close`；prompt 单例；cancel/Escape 不保存；hide 前实时检查 restore capability。
- 决策空间：状态可落 app managed state 或专用 lifecycle module；必须能用 fake window/tray adapter 单测。
- 产物：CloseGate reducer/service、偏好 schema/migration、trigger/bypass 类型、fake-clock/concurrency tests。
- 实施步骤：
  1. 盘点自绘 X、Alt+F4、native、tray quit、updater restart、OS shutdown 的现有入口。
  2. 实现 `idle → prompting → executing` 原子转换和重复 close 聚焦语义。
  3. 迁移旧 hide/quit 配置到枚举，保存只发生在已确认 hide/quit 路径。
  4. 以 restore adapter 覆盖 tray/dock/companion/none 与运行中任务快照。
- 验收断言：
  - `M3-01.A1`（contract）：titlebar/Alt+F4/native 的等价 fixture 产生同一 CloseIntent 与状态序列。
  - `M3-01.A2`（reliability）：并发/重复 close 只有一个 intent/dialog；stale decision 和重复提交被拒绝。
  - `M3-01.A3`（migration）：旧/缺失/未知偏好迁移幂等，默认安全为 ask。
  - `M3-01.A4`（security）：restore=none 时 hide 永不执行，窗口保持可达。
- 验证：`node scripts/verify-product-experience.mjs --task M3-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M3-01.yaml`。
- 失败处理：保留触发/竞态 fixture；修 Host gate，不把逻辑挪回仅 React onClick。

### M3-02 实现关闭对话框、恢复入口与统一退出清理

- 结果：首次关闭可选择隐藏到托盘/Dock、退出或取消并记住选择，设置可重置；显式退出和 restart 统一清理全部子系统，并只在 terminal projection 持久化与 Host ACK 后完成退出。
- 需求引用：R-CLOSE-02、R-CLOSE-03、R-UPD-01、R-ACC-01。
- 依赖：M3-01、M2-04、M1-04。
- 前置事实：真实关闭目前无对话框且 dev 进程留后台；Companion 错误路径可能产生不可恢复后台状态。
- 固定约束：对话框显示活动任务/审批影响；记住只在成功选择后写；Tray Quit/Updater/OS bypass 不重复询问；退出 ACK 后主 Run/child/tool/timer 全部 terminal、spinner 为 0，下一进程不得恢复旧 running。
- 决策空间：平台恢复 surface 文案可不同；退出清理并发/串行顺序由依赖决定，但必须有总超时与诊断。
- 产物：CloseDialog、设置项、tray/dock/companion restore adapter、ShutdownCoordinator、三平台 lifecycle tests。
- 实施步骤：
  1. 接入 CloseIntent UI、焦点陷阱、Esc/cancel、忙碌/错误与记住选择。
  2. 建立设置中的当前关闭行为、重置和显式退出入口。
  3. 统一停止/回收主 Run、子代理、工具、Browser、Automation、Companion 与持久化 flush；持久化单调 terminal projection，并把各子系统 ACK/有界失败汇总成唯一 shutdown ACK。
  4. 覆盖 hide 失败、restore 丢失、清理局部失败、超时、tray quit 与 updater restart。
- 验收断言：
  - `M3-02.A1`（E2E）：三种普通 close 入口均出现单例对话框，cancel 不写偏好，remember 跨重启生效。
  - `M3-02.A2`（integration）：tray/dock 恢复可达；恢复能力消失时 hide 被拒绝且窗口仍可见。
  - `M3-02.A3`（reliability）：quit/restart 在 Host ACK 前不销毁恢复面；ACK 后主 Run/child/tool/timer 全部 terminal、spinner=0，局部失败有脱敏有界诊断且不留不可达窗口进程；新进程 reload 不恢复旧 running。
  - `M3-02.A4`（accessibility）：仅键盘/读屏完成选择、勾选、取消，焦点回触发器且 active-run 后果可感知。
- 验证：`node scripts/verify-product-experience.mjs --task M3-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M3-02.yaml` 与三平台 adapter 报告。
- 失败处理：保持窗口可见并回到 ask；不得因 adapter 异常直接销毁主 WebView 或把超时改成无限等待。

### M3-03 实现 Host 级非阻塞 Provider readiness service

- 结果：Shell 可交互后后台检查默认 Provider 与保存子代理槽位，复用 receipt/TTL/fingerprint，锁外有界联网且无静默 fallback。
- 需求引用：R-PROV-01、R-PROV-02、R-PROV-04、R-SUB-02、§4.2。
- 依赖：M1-01、M1-02。
- 前置事实：设置页已有 30m/5m receipt 与真实 8-token probe；当前批量测试在配置锁内串行。
- 固定约束：首屏不等待；fresh TTL 零请求；并发≤2；旧 fingerprint 或旧 policy generation 结果不得覆盖新配置；费用可关闭，opt-out 后旧 generation 不写 receipt/不合成 success，手动测试仍可用。
- 决策空间：Provider 支持 catalog 时可先 catalog，否则最小 exact-model；timeout 3–30 秒按 capability 冻结。
- 产物：Host readiness queue/service、receipt store/CAS、probe adapter、fake HTTP/clock、委派前刷新 API。
- 实施步骤：
  1. 锁内读取配置快照/fingerprint/startup policy generation，按 canonical default→保存槽位去重排序。
  2. 锁外执行有界并发、超时、退避和 catalog/exact-model probe。
  3. 锁内以 fingerprint+generation CAS 写回 receipt 并发脱敏状态事件；配置或 startup policy 改变使旧结果失效，opt-out 主动取消排队任务并隔离无法取消的在途结果。
  4. 将委派候选池刷新接到同一单飞 service，区分 optional/required 委派失败。
- 验收断言：
  - `M3-03.A1`（performance）：Shell first paint/交互不等待网络；fresh receipt 启动请求数为 0，stale 请求并发峰值≤2。
  - `M3-03.A2`（reliability）：并发相同 probe 单飞；fingerprint 改变或 startup opt-out 递增 generation 后，排队任务被取消、在途迟到结果零 receipt/event/success 写入，手动测试仍能独立完成。
  - `M3-03.A3`（integration）：过期候选池在委派前自动刷新；optional 失败可降级，required 失败给可恢复 Attention 且无假 child。
  - `M3-03.A4`（security）：请求/事件/log/证据无 key、完整正文或 Authorization，失败不改变 provider/model/endpoint。
- 验证：`node scripts/verify-product-experience.mjs --task M3-03 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M3-03.yaml` 与请求计数/锁时长报告。
- 失败处理：保留 fake-clock/HTTP 时序；修 queue/CAS/adapter，不把联网移回配置锁或自动换模型。

### M3-04 实现全局连接健康 UI 并迁移设置页探测

- 结果：Provider mini、Shell、Composer、Provider selector、Settings、health 与主 Agent 默认模型显示同一 canonical `configured + connectivity` 快照，状态可重试、可解释且不阻塞使用。
- 需求引用：R-PROV-03、R-PROV-05、R-COMP-01、R-I18N-01。
- 依赖：M3-03、M2-03、M2-04。
- 前置事实：实测设置显示 DeepSeek/ark 可用，空白页却提示连接服务并短暂显示 Codex；子代理页独占 auto-probe。
- 固定约束：configured 不冒充 connected；default 仅 Host ACK 后发布，Host reject 保持旧 default/profile/reference；默认或持久槽位引用 Provider 不可删除；默认 Provider 失败才全局低干扰提示；exact-model 少量费用明示并可关闭。
- 决策空间：健康入口可位于 Topbar/Composer 状态点；详细 receipt 属于 popover/设置，不占主 Timeline。
- 产物：HealthIndicator/popover、设置 policy/重试、统一 provider snapshot selector、旧 useEffect 迁移和 UI tests。
- 实施步骤：
  1. 建立 unknown/checking/connected/degraded/failed 的非颜色文案、图标和相对时间。
  2. 让 Provider mini、空白页、Composer、selector、Settings、health、主 Agent 默认模型和子代理槽位订阅同一 Host snapshot revision。
  3. 移除设置页拥有生命周期的 auto-probe，仅保留手动重试/receipt 展示。
  4. 覆盖离线、TTL fresh/stale、默认失败、非默认失败、配置变更和 startup probe disabled。
- 验收断言：
  - `M3-04.A1`（integration）：全部消费面在每个 fixture 上 snapshot revision/provider/model/connectivity 一致且无首屏错误闪烁；Host reject 后旧 default/profile/reference digest 不变，默认/持久槽位引用删除拒绝。
  - `M3-04.A2`（E2E）：probe 失败时 Shell/Composer 仍可操作，手动重试去重并更新相对时间。
  - `M3-04.A3`（visual/accessibility）：状态不只依赖颜色，键盘/读屏可查看原因、费用说明和重试。
  - `M3-04.A4`（regression）：设置页进入/离开不额外触发网络，TTL 内请求数保持 0。
- 验证：`node scripts/verify-product-experience.mjs --through M3 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M3-04.yaml` 与状态/请求截图索引。
- 失败处理：修 snapshot selector/事件订阅；不得在各页面恢复独立探测或用“可使用”掩盖 unknown。

### M4-01 实现 Run Capsule 派生模型、折叠与稳定回放

- 结果：每个 Run 有一个稳定 Capsule，普通轨迹默认紧凑，Attention 自动可见，用户选择与终态在 live/history 一致。
- 需求引用：R-TRACE-01、R-TRACE-02、R-RUN-01、R-REL-01、§4.3、§5.4。
- 依赖：M1-03、M0-02。
- 前置事实：Timeline 已有事件聚合/折叠基础；实测父失败后子工具仍计时，破坏可信度。
- 固定约束：fold 只影响呈现不删事件；raw reasoning 不进入 latest update；失败/审批/提问/final 不可藏。
- 决策空间：可扩展现有 presentation reducer；detail_state 仅本 Run 生命周期持久，可选保存用户全局默认。
- 产物：RunCapsuleView reducer、fold state machine、terminal tombstone、live/replay/property tests。
- 实施步骤：
  1. 将公开 commentary、宿主事件、工具/子代理计数、Attention、diff/verification 投影到 Capsule。
  2. 实现 auto_compact/auto_expanded/user_compact/user_expanded 优先级。
  3. 接入父终态级联、计时器封口与 Host terminal ACK，迟到事件只计诊断不复活；shutdown/restart 必须等待同一 ACK。
  4. 用序列化→reload 重放比较结构、顺序、终态和折叠初始规则。
- 验收断言：
  - `M4-01.A1`（contract）：§5.4 全状态矩阵得到唯一 detail_state，Attention/final 始终可发现。
  - `M4-01.A2`（reliability）：父终态后 Capsule/child/tool 在 1 秒内停止计时并持久化单调 terminal revision；Host ACK 后 spinner=0，迟到帧与应用重启均不复活旧 running。
  - `M4-01.A3`（replay）：live 序列化后重建的阶段、计数、Attention、终态与可见顺序一致。
  - `M4-01.A4`（security）：latest update 与摘要中 raw reasoning/secret oracle 为 0 命中。
- 验证：`node scripts/verify-product-experience.mjs --task M4-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M4-01.yaml`。
- 失败处理：保存最小事件序列；修 identity/reducer，不通过丢弃历史事件或前端强制 timeout 规避。

### M4-02 重构 Timeline 的 commentary、final、轨迹与 Attention

- 结果：中栏突出用户、公开 commentary、关键状态与 final；内部规划和普通工具默认收纳，展开后仍安全完整。
- 需求引用：R-FEED-01、R-FEED-02、R-TRACE-01、R-TRACE-02、R-SEC-01。
- 依赖：M4-01、M2-04。
- 前置事实：真实模型默认展开长英文内部规划，用户可读中文进度仅一句；现有“收起”能力默认策略相反。
- 固定约束：不展示/推断私有 CoT；宿主文案只来自真实事件；final 永不并入工具折叠。
- 决策空间：commentary 与宿主状态可用不同材质/标签；工具组按阶段或相邻类型聚合。
- 产物：Timeline presentation/render、public update style、tool group/detail、Attention cards、visual/replay tests。
- 实施步骤：
  1. 将 raw/internal reasoning 从普通时间线剔除，保留安全诊断计数。
  2. 默认渲染 Capsule、commentary、Attention、final，普通工具以数量/耗时/结果摘要折叠。
  3. 提供二级展开、截断/复制安全详情、错误恢复动作和稳定焦点。
  4. 覆盖 commentary→tool→subagent→question/approval→final 与刷新回放。
- 验收断言：
  - `M4-02.A1`（security）：raw reasoning、secret 和未脱敏工具输出在普通 DOM/截图/持久化 oracle 为 0 命中。
  - `M4-02.A2`（visual）：长任务默认首屏以公开进度和交付为主，普通工具折叠；失败/审批/提问/final 无需展开即可发现。
  - `M4-02.A3`（replay）：展开后事件顺序、终态与安全摘要在 live/history 一致。
  - `M4-02.A4`（performance）：10,000 delta 不生成 10,000 DOM 节点，可见刷新≤10Hz且 final 完整。
- 验证：`node scripts/verify-product-experience.mjs --task M4-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M4-02.yaml` 与 timeline 截图/性能报告。
- 失败处理：修 presentation/buffer；不得用隐藏 failure/final、展示私有推理或降低性能阈值换取通过。

### M4-03 重构执行台为概览、子代理、变更

- 结果：右侧执行台一跳展示决策信息，活动子代理/Attention 自动聚焦；重复全局工具审计从一级 IA 删除。
- 需求引用：R-WB-01、R-WB-02、R-SHELL-01、R-STATUS-01。
- 依赖：M4-01、M2-04。
- 前置事实：当前需“工具启动器→运行与子代理→打开子智能体列表”三层才能到有用视图，展开还会改变窗口几何。
- 固定约束：仅 overview/subagents/changes 三个一级 tab；工具详情回对应 Run/child；打开不改 OS bounds。
- 决策空间：自动选择规则可按 Attention > active child > changes ready > overview；用户手动 tab 在本任务保持。
- 产物：Workbench IA/router、Overview/Subagents/Changes shell、自动聚焦规则、window-bounds E2E。
- 实施步骤：
  1. 移除工具启动器与全局工具列表的一级入口，保留可达的诊断/Run detail 路径。
  2. 实现目标、阶段、Attention、child/changes/verification 摘要与深链。
  3. 建立首次打开自动聚焦与用户手动 override 规则。
  4. 覆盖固定栏/抽屉、reload、back 和活动状态变化，不改变顶层窗口几何。
- 验收断言：
  - `M4-03.A1`（component）：一级 tab 集合精确为 overview/subagents/changes，无重复全局工具审计。
  - `M4-03.A2`（E2E）：存在 active child/Attention 时一次操作到达聚焦视图，返回不丢主任务上下文。
  - `M4-03.A3`（regression）：开关/切 tab/抽屉前后 OS window bounds 不变且 Run 状态不变。
- 验证：`node scripts/verify-product-experience.mjs --task M4-03 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M4-03.yaml`。
- 失败处理：修路由/布局与 focus policy；不得恢复工具启动器或用窗口 resize 解决空间。

### M4-04 完成子代理树、候选自愈、聚合审批与停止

- 结果：用户可可信地查看/停止父子协作；过期候选可自愈，同风险工作区只读请求可安全聚合，失败后无假运行。
- 需求引用：R-SUB-01、R-SUB-02、R-PERM-01、R-RUN-01、§4.5。
- 依赖：M4-01、M4-03、M3-03。
- 前置事实：实测首次候选池过期导致整轮失败；成功委派后读取 3 个文件出现 3 张审批卡。
- 固定约束：聚合只覆盖 canonical WorkspaceBinding 内 read/list/search；写、删、Shell、网、MCP mutation 保持独立审批。
- 决策空间：聚合 scope 可提供 once/run/workspace-readonly；workspace grant 必须可查看/撤销并有路径边界。
- 产物：collaboration tree/detail、candidate refresh adapter、PermissionRisk classifier/grant registry、stop/cascade tests。
- 实施步骤：
  1. 展示 parent/source/role/depth/model/phase/last update/Attention/current tool 与 transcript。
  2. 委派前调用 M3 单飞 readiness；optional/required 失败按 §4.5 投影。
  3. 对 pending workspace reads 按 canonical root/risk/run 聚合，明确范围和持续时间。
  4. 实现 child/branch/whole-run stop 与父终态级联，清理过期审批和计时器，并把 terminal projection ACK 接入统一退出/重启路径。
- 验收断言：
  - `M4-04.A1`（integration）：stale candidate 成功刷新后只创建一个 child；失败时无假 running，optional/required 行为符合合同。
  - `M4-04.A2`（security）：三个同根 read 合并为一张卡；越界/read→write/network/mutation 无法借 grant 通过。
  - `M4-04.A3`（E2E）：列表→child detail→transcript→back 保留主任务；停止精确作用于选择范围。
  - `M4-04.A4`（reliability）：父失败/取消后所有 child/tool/approval ≤1 秒终结且计时停止；terminal ACK 后 spinner=0，reload/restart 不出现父 terminal、child/tool running 的组合。
- 验证：`node scripts/verify-product-experience.mjs --task M4-04 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M4-04.yaml` 与权限负例报告。
- 失败处理：撤销聚合 grant 并保留精确审批；修 risk/canonicalization/cascade，不扩大默认权限或静默主代理降级。

### M4-05 完成收尾摘要与证据跳转

- 结果：Run 结束后紧凑回答“做了什么、改了什么、验证了吗、还需什么”，可跳转 diff、验证、审批和子代理证据。
- 需求引用：R-SUM-01、R-FEED-01、R-TRACE-02、R-WB-01。
- 依赖：M4-02、M4-03、M4-04。
- 前置事实：现有 Summary 与 Timeline 重复普通工具，但缺少高价值交付/验证跳转。
- 固定约束：摘要只来自持久化事实和 final，不猜模型意图；失败/未验证不得标成功。
- 决策空间：成功、部分成功、失败可使用不同模板；空 diff/无测试必须明确显示。
- 产物：CompletionSummary projector/card、deep links、partial/failure fixtures、E2E。
- 实施步骤：
  1. 汇总 outcome、changed files、verification assertions、Attention 与 child contributions。
  2. 按 succeeded/partial/failed/cancelled 生成事实型紧凑摘要。
  3. 将 diff、verification、pending approval/question 与 child transcript 接为稳定深链。
  4. 覆盖无变更、无测试、测试失败、部分 child 失败和 reload。
- 验收断言：
  - `M4-05.A1`（contract）：四种终态的摘要字段完整且 never-tested/failed 不显示“已验证”。
  - `M4-05.A2`（E2E）：每个可用 deep link 到达正确文件/报告/卡片/child，并可返回主任务。
  - `M4-05.A3`（replay）：reload 后摘要与实时一致，不重复工具列表或暴露 raw reasoning。
- 验证：`node scripts/verify-product-experience.mjs --through M4 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M4-05.yaml`。
- 失败处理：修事实 projector/引用索引；不得从自由文本猜测试通过或复制全部工具 transcript。

### M5-01 完成可访问性、IME、缩放与 reduced-motion 加固

- 结果：工作区和 12 页 Settings 的关键体验在键盘、读屏、触控、中文 IME、100–200% 缩放、960×640/390px 和 reduced-motion 下可完整完成，状态 glyph 不依赖颜色或持续运动。
- 需求引用：R-ACC-01、R-ACC-02、R-COMP-02、R-WB-02、R-I18N-01、R-SET-05、R-SET-06、R-GUIDE-01、R-MCP-01、R-SET-09。
- 依赖：M3-02、M3-04、M4-05、M2-04。
- 前置事实：现有快捷键和 skip link 是亮点；实测 editor UIA 焦点仍报 RootWebArea、自定义/系统窗口控制重复、灰字对比不足并有乱码。
- 固定约束：状态不只依赖颜色；只有 running/checking 可旋转；自定义控件具备正确 role/name/state；reduced-motion 不删除反馈或把不同状态合并成相同圆点。
- 决策空间：可替换不合适的自定义控件为原生语义；快捷键冲突按 OS adapter 处理。
- 产物：focus/keyboard/ARIA 修复、IME harness、contrast audit、scale/reduced-motion visual tests。
- 实施步骤：
  1. 审计 Shell、Composer、CloseDialog、Provider、Timeline、Workbench、Permission 及全部 12 个 SettingsPane 的 tab/focus/ARIA。
  2. 修 editor 可访问焦点、重复窗口控件、侧栏 dialog role 和异步页面树更新。
  3. 覆盖中文 composition、Enter/stop、长中英文、200% zoom、960×640 与 390px；测量触控命中区和相邻目标重叠，覆盖 GuideSheet、MCP 审批和 revision conflict 操作。
  4. 修正文对比、乱码与非必要运动；验证只有 running/checking 使用中性 spinner，终态静止且状态变化仍有文字/单一 aria-live。
- 验收断言：
  - `M5-01.A1`（accessibility）：仅键盘/读屏完成 Settings 搜索/12 页导航/返回，以及发送、关闭选择、健康重试、展开轨迹、审批与停止。
  - `M5-01.A2`（IME）：composition 期间 Enter 不发送/停止，提交后的文本不丢字或重复。
  - `M5-01.A3`（visual）：目标主题/视口/100–200% 下对比达标且无横向溢出、遮挡、乱码。
  - `M5-01.A4`（motion）：常规模式仅 running/checking 有中性 spinner，queued/waiting/approval/terminal 静止；reduced-motion 下 spinner 停止旋转并保留静态缺口环与文字。
  - `M5-01.A5`（accessibility）：12 页 Settings 在 200% 缩放和 960×640 下无焦点丢失、hidden/inert 穿透或只能指针完成的操作；异步状态由一个聚合 live region 播报，不重复播报。
  - `M5-01.A6`（touch/accessibility）：390px 下主要动作/图标按钮命中区 ≥44×44 CSS px、紧凑次要控件 ≥32×32 且相邻目标不重叠；四 GuideSheet、MCP alertdialog、dirty discard 与 conflict recovery 无 hover-only 或精确像素路径。
- 验证：`node scripts/verify-product-experience.mjs --task M5-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M5-01.yaml` 与 axe/UIA/视觉报告。
- 失败处理：按具体节点/viewport 修复；不得移除键盘路径、缩小测试矩阵或仅隐藏低对比文字。

### M5-02 完成体验 E2E、视觉、性能与隐私门禁

- 结果：§5.8 的 Provider、关闭/恢复、长任务/子代理、通知、Updater、诊断、Settings CRUD/conflict、MCP exact approval、四 GuideSheet 和 planned-demo 保真闭环，以及 12 页 Settings 状态矩阵全部确定性通过，并满足视觉、性能与隐私阈值。
- 需求引用：R-REL-01、R-SEC-01、R-ACC-01、R-ACC-02、R-SET-01、R-SET-02、R-SET-03、R-SET-04、R-SET-05、R-SET-06、R-SET-07、R-SET-08、R-SET-09、R-MCP-01、R-SUB-03、R-GUIDE-01、R-TOOL-01、§5.7、§5.8、§7、§8。
- 依赖：M5-01。
- 前置事实：D0 仅验证原型；本任务首次证明产品实现而非截图。
- 固定约束：required metric 缺失即失败；截图不能单独证明 lifecycle/隐私或 CapabilityID 覆盖；真实 Provider 仅 candidate profile。Settings E2E 必须经过正式 SettingsScene、route registry、Tauri/Host command 和 persistence adapter；implementation 可替换 Provider/OS/Keychain 外部边界为 deterministic adapter，但不得把 Settings adapter、IPC 或持久化替成只存在于测试页的内存 stub；必须重启应用进程验证持久化。
- 决策空间：主 E2E 使用 fake App Server/HTTP/OS adapters 确定性执行，已授权真实配置只作候选 smoke。
- 产物：Playwright/Tauri E2E、按 CapabilityID 生成的 fixture/coverage report、restart persistence report、failure/disabled/permission report、visual baselines、10k delta benchmark、security-negative oracle、报告索引。
- 实施步骤：
  1. 跑 commentary→tool→child→approval/question→final→diff/verification 主链及失败/取消链，并断言父终态后所有 spinner 在 1 秒内停止。
  2. 跑 close 三入口、preference restart、restore unavailable 与 terminal ACK/restart no-running；跑 Provider 编辑/Host ACK 切默认/Host reject/引用删除拒绝/测试→全局健康、TTL/fingerprint/generation CAS、startup opt-out 与 stale candidate。
  3. 跑 12 页 Settings 导航/搜索/返回及统一 loading/last-good/retry、CRUD dirty/discard、Host CAS conflict 三路恢复、图片 direct/OCR/helper/reject-batch、Codex cancel→terminate ACK/cancel_failed、四 GuideSheet、MCP stable-ID + exact approval、per-slot Prompt、Shell immediate apply、通知授权→测试、Updater 检查→下载→restart bypass、诊断自检→支持包预览、planned-demo 不改变实现状态。
  4. 测事件→UI p95、DOM 节点、更新频率、窗口 bounds、1 秒终态级联与全状态 glyph/motion 矩阵。
  5. 扫描 DB/log/support bundle/DOM/screenshot index 的 raw reasoning、secret、cookie/token，并静态/计算检查 day theme 无组件级黑 overlay/大投影。
  6. 按 baseline 逐 CapabilityID 从正式导航到达，执行读取、成功提交、Host 拒绝后保旧值、适用的 loading/error/disabled/permission 与应用进程重启恢复；动作数与 required capability 数必须相等。
- 验收断言：
  - `M5-02.A1`（E2E）：主链和失败/取消/恢复链全部通过，关键深链与返回上下文正确。
  - `M5-02.A2`（performance）：§7 的 p95、10k delta、≤10Hz、级联≤1秒及窗口 bounds 指标齐全并通过。
  - `M5-02.A3`（visual）：工作区与 12 页 Settings 的亮暗/目标视口/100–200% 差异仅在批准 baseline 内，无意外遮挡/溢出；day material 黑影扫描通过。
  - `M5-02.A4`（security-negative）：所有敏感 oracle 为 0 命中，聚合只读权限负例全部拒绝。
  - `M5-02.A5`（E2E）：§5.8 全部闭环的正向、失败/降级与返回焦点全部通过；planned-demo 始终标注且在产品 feature-disabled profile 中前端隐藏、直接调用拒绝。
  - `M5-02.A6`（contract）：12 Pane registry 与实际路由、搜索、locale、provenance 和 assertion registry 一一对应；缺任一 Pane/关键状态/视口即非 0。
  - `M5-02.A7`（E2E）：正式产品 SettingsScene 从真实导航入口逐个到达全部 baseline CapabilityID；每个字段/状态可读、每个动作触发真实 UI→IPC→Host→adapter 链，执行数与 required capability 数完全一致。
  - `M5-02.A8`（integration/E2E）：每个可写能力至少覆盖保存成功、Host/adapter 拒绝后保持旧值、应用进程重启后恢复；Provider default 仅 ACK 后更新且 reject/delete-reference 后 default/profile/reference digest 不变；每个动作/只读状态覆盖 success 与适用的 loading/error/disabled/permission。
  - `M5-02.A9`（regression）：capability coverage 满足 `missing=0`、`unexecuted=0`、`unexpected_noop=0`、`prototype_only_evidence=0`；缺任一 CapabilityID 的 required 证据即 Harness 非 0。
  - `M5-02.A10`（E2E/reliability）：12 页统一 Settings lifecycle 的无 snapshot/last-good load failure、retry success/failure、save reject、dirty leave 与 Host CAS conflict 全矩阵通过；persisted/local/fresh digest 分离，discard local、reapply onto latest、field merge preview 三路各返回新 base revision且无静默覆盖。
  - `M5-02.A11`（integration/security/accessibility）：Provider canonical snapshot/startup opt-out、无损图片路由、Codex terminate ACK、Subagent/Shell 真实 authority、MCP stable `server_id` + 独立 enable approval、退出 terminal ACK 和四 GuideSheet 全通过；旧 generation 写回、helper fallback OCR/部分发送、乐观 cancelled、MCP ID 篡改或 credential 推断迁移、虚构字段/第二权限 authority/client launch plan/global Prompt 替代/next-session Shell/单 Guide 替代/重启恢复旧 running 的负例全部非 0。
- 验证：`node scripts/verify-product-experience.mjs --task M5-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M5-02.yaml` 与 performance/security/visual JSON。
- 失败处理：保存最小 fixture/差异/指标并回到最早责任任务；不得更新错误基线、删负例或降阈值。

### M5-03 建立体验 flag、等价回退与旧路径退役

- 结果：新体验可通过内部 flag 安全切回旧表现层，底层事件/权限/数据不变；达到证据门后按序退役旧 CSS/UI 路径。
- 需求引用：R-ROLL-01、R-FLAG-01、R-REL-01、R-REL-02、R-RELSE-01、R-SET-02、R-SET-04、R-SET-07、R-SET-08、R-SET-09。
- 依赖：M5-02。
- 前置事实：当前工作区已有多层视觉覆盖和未闭环 feature；直接删除旧路径会扩大回归风险。
- 固定约束：回退仅改变 presentation；不得关闭安全校验、丢 migration 或改变 persisted event。old/new renderer 必须消费同一 capability map 与持久化权威；route/key/enum/IPC alias 在兼容 fixture 全绿前不得退役；删除旧 UI/CSS 不能删除或改变基线能力；retirement 必须有独立 RequirementRef 和用户授权，不能以“新页面更简洁”为依据。
- 决策空间：flag 可按 build/internal setting 管理；旧路径删除必须在累计等价门后单 owner 进行。
- 产物：experience flag、dual-presentation fixture、migration/rollback note、旧 CSS/组件退役清单。
- 实施步骤：
  1. 在共享 event/state 下并行挂接 old/new renderer，不复制后端能力；planned-demo 能力保持前后端双 flag，Demo provenance 不作为运行时授权。
  2. 对每个 CapabilityID 比较可见性、可达性、值域、默认、写入、Host 副作用、权限、错误与 persisted data；合法 merge 按 merge contract 比较。
  3. 验证运行中切换策略（建议仅下次启动生效）与 old→new→old→new route/key/enum/IPC/config 往返、unknown-field、故障回滚兼容。
  4. 达门且 `orphan_capabilities=0`、`live_legacy_consumers=0`、`unauthorized_retirements=0` 后，按依赖删除旧 alias/override/死组件并复跑累计门。
  5. 固定最终 revision 后以新应用进程和隔离 fixture 连续运行三轮 `--through M5`；每轮保存 round ID、revision、初末状态 digest、退出码和泄漏扫描，任一改码/失败/泄漏都从第 1 轮重计。
- 验收断言：
  - `M5-03.A1`（regression）：old/new 对同一 fixture 的底层状态、权限决定和持久化 digest 相同。
  - `M5-03.A2`（migration）：新→旧→新重启不丢 preference/task/run/provider 数据且 migration 幂等。
  - `M5-03.A3`（release）：新体验异常时一个受控 flag 可回退，Browser/Automation/Worktree feature guard 不受影响。
  - `M5-03.A4`（regression）：old/new 对每个 CapabilityID 的可见性、可达性、当前值、默认值、允许值、写入结果、Host 副作用、权限决定和错误语义相同；合法 merge 按 contract 比较，差异为空。
  - `M5-03.A5`（migration）：所有旧 route/deep-link/config key/enum/IPC fixture 完成 old→new→old→new 往返；迁移重复执行结果一致，unknown fields 不丢失，故障注入后旧值可读且 rollback 成功。
  - `M5-03.A6`（retirement）：旧路径删除前 `orphan_capabilities=0`、`live_legacy_consumers=0`、`unauthorized_retirements=0`，体验 flag 能在不迁移或改写底层数据的情况下恢复旧表现。
  - `M5-03.A7`（cumulative regression）：同一未改码 revision 的 M5 implementation gate 以三个新应用进程、三套隔离 fixture 连续通过；三份报告 revision 一致、初始状态 digest 独立、跨轮状态泄漏为 0，任一失败后成功轮计数从零开始。
- 验证：`node scripts/verify-product-experience.mjs --through M5 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M5-03.yaml`。
- 失败处理：保留旧 renderer 并修共享 adapter；不得通过分叉数据库/event schema 获得表面等价。

### M6-01 完成 Worktree 开关、任务选择、schema 与原子创建

- 结果：Worktree 默认关闭；Git 项目可为任务选择 Local/Worktree，创建遵循校验→创建→验证→持久化→事件并可安全补偿。
- 需求引用：R-WT-01、R-WT-02、R-BIND-01、§4.6。
- 依赖：M1-02、M1-05。
- 前置事实：旧 gap plan 已冻结 `managed_by_r_code`、repo、branch、base_oid 与 cleanup 安全边界。
- 固定约束：非 Git 只能 Local；创建失败不改原项目；补偿只删确认 clean、无新 commit、R-Code 管理的目标。
- 决策空间：branch 命名可用现有 task slug/ID 规则；平台路径 adapter 可不同但 schema 相同。
- 产物：Worktree settings/task choice、ManagedWorktree schema/migration、atomic creator、temporary-repo tests。
- 实施步骤：
  1. 冻结默认关闭、项目能力检测、任务环境选择与不可用原因。
  2. 实现 repo/base/target/branch 冲突校验和 `git worktree add` adapter。
  3. 创建后验证 common-dir、branch、base_oid 和 canonical path，再事务持久化/发事件。
  4. 对每个失败点执行受约束补偿并记录 Attention。
- 验收断言：
  - `M6-01.A1`（contract）：非 Git/disabled 只能 Local；合法 Git fixture 的 schema round-trip 与迁移幂等。
  - `M6-01.A2`（integration）：原子创建成功后 repo/branch/base_oid/path/managed identity 全部一致。
  - `M6-01.A3`（security）：任一故障注入不改变原项目 hash；不确定、dirty 或有新 commit 的目标永不自动删除。
- 验证：`node scripts/verify-product-experience.mjs --task M6-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M6-01.yaml` 与临时 repo manifest。
- 失败处理：保留 worktree 并产生 Attention；修步骤/补偿，不使用递归删除或 Local fallback。

### M6-02 迁移全部执行消费者到 WorkspaceBinding

- 结果：Agent、Terminal、Files、Git/Review、Browser、Automation 与相关 MCP 都从同一验证后的 TaskWorkspaceBinding 取得工作目录。
- 需求引用：R-WT-03、R-BIND-01、R-BR-04、R-AUTO-03。
- 依赖：M6-01。
- 前置事实：消费者目前可能各自从 project/task 推导路径；M1 仅建立 resolver，尚未完成全链迁移。
- 固定约束：每次运行前重新验证；缺失/失效/逃逸立即 fail closed；不保留隐式 project-root fallback。
- 决策空间：可通过 shared execution context 注入 binding，避免每个消费者重复查询。
- 产物：consumer inventory、execution context、各 adapter 接线、路径替换/逃逸 integration tests。
- 实施步骤：
  1. 用静态搜索和运行 trace 列出所有 cwd/root/profile/file:// 来源。
  2. 建立只读 binding snapshot 与运行前 revalidate 接口。
  3. 逐项迁移 Agent/Terminal/Files/Git/Review/MCP，再为 M7/M8 暴露同一入口。
  4. 删除/封闭旧项目路径 fallback 并加静态禁止门禁。
- 验收断言：
  - `M6-02.A1`（contract）：consumer inventory 中每项只有 WorkspaceBinding 来源，禁止 fallback 模式扫描为 0。
  - `M6-02.A2`（integration）：Local/Worktree 下所有消费者 cwd/root 一致且写入仅发生绑定目录。
  - `M6-02.A3`（security）：运行前替换目录、repo mismatch、junction/symlink 逃逸均拒绝，原项目 hash 不变。
- 验证：`node scripts/verify-product-experience.mjs --task M6-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M6-02.yaml` 与 consumer matrix。
- 失败处理：阻止该消费者执行并显示 binding Attention；不得临时回退 project 或跳过 canonicalize。

### M6-03 完成 Worktree 生命周期、恢复、Review 与安全清理

- 结果：重启能恢复托管身份；归档/关闭开关不破坏工作区；clean/no-commit 可安全清理，dirty/new commit 保留进入 Review。
- 需求引用：R-WT-02、R-WT-04、R-STATUS-01、R-REL-01。
- 依赖：M6-02。
- 前置事实：旧计划要求不确定即保留；当前用户工作区存在未提交资产，证明清理必须极保守。
- 固定约束：归档停止任务但保留 Worktree；关闭功能不把既有任务改回 Local；删除前再次验证 managed identity。
- 决策空间：cleanup 可用显式用户动作或策略建议；默认保留有价值/不确定目录。
- 产物：reconciler、cleanup state machine、Review projection、crash/restart/dirty/new-commit tests。
- 实施步骤：
  1. 启动时比对 DB、git worktree list、common-dir、branch/base_oid 与实际目录。
  2. 将 missing/mismatch/orphan/dirty/new-commit 映射到确定状态和 Attention。
  3. 实现 archive/cleanup/keep/review 流程与二次校验、事件和诊断。
  4. 故障注入进程崩溃、持久化失败、手工移动和并发 Git 操作。
- 验收断言：
  - `M6-03.A1`（recovery）：重启后合法 Worktree 恢复同一 binding；missing/mismatch 不回退且产生 Attention。
  - `M6-03.A2`（security）：dirty/new commit/unmanaged/identity uncertain 的目录全部保留，clean/no-commit 才允许受控清理。
  - `M6-03.A3`（integration）：归档停止运行但保留工作区，Review 可跳转 diff/commit，关闭 flag 不改变旧任务 binding。
- 验证：`node scripts/verify-product-experience.mjs --task M6-03 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M6-03.yaml` 与 cleanup decision ledger。
- 失败处理：默认保留并标 Attention；修 reconciler/identity，不用强制 remove、reset 或自动 discard。

### M6-04 完成 Worktree UI、三平台路径安全与 E2E

- 结果：用户能创建、识别、恢复、Review 和清理 Worktree；Windows/macOS/Linux 路径差异不改变 fail-closed 语义。
- 需求引用：R-WT-01 至 R-WT-04、R-ACC-01、R-I18N-01。
- 依赖：M6-03、M5-01。
- 前置事实：路径/junction/symlink 行为跨平台不同；当前仅 Windows 本机可做真实 smoke，其余可走 CI adapter。
- 固定约束：UI 不提供危险“强制删除”；fixture/local 不冒充三平台真机；所有状态双语可访问。
- 决策空间：高级诊断可放详情页；常规任务只显示环境、branch、状态和下一安全动作。
- 产物：Worktree task/settings/review UI、platform adapters/tests、visual/a11y/E2E、运维说明。
- 实施步骤：
  1. 实现环境选择、创建进度、binding 状态、Review/keep/cleanup 与错误恢复界面。
  2. 覆盖长路径、盘符/UNC、大小写、symlink/junction 与权限失败 adapter。
  3. 跑 Local/Worktree agent→files→git/review→archive/cleanup 端到端。
  4. 更新跨平台兼容/降级与手工恢复文档。
- 验收断言：
  - `M6-04.A1`（E2E）：正向创建/执行/Review/归档和所有拒绝/恢复路径通过，原项目 hash 不变。
  - `M6-04.A2`（cross-platform）：三平台 adapter 共用合同门绿，junction/symlink/long-path 负例均拒绝。
  - `M6-04.A3`（visual/accessibility）：双语、亮暗、目标视口和键盘完成关键流程，无危险默认动作。
- 验证：`node scripts/verify-product-experience.mjs --through M6 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M6-04.yaml` 与 M6 累计报告。
- 失败处理：保留 Worktree 并给恢复步骤；只把缺真机列 external pending，不把合同/fixture 失败降级。

### M7-01 完成 Browser Runtime 资产供应链、manifest 与许可

- 结果：Node/Playwright/Chromium 的平台资产、版本、大小、SHA-256、许可和来源固定且可复现，未安装时不显示可用假状态。
- 需求引用：R-BR-01、R-FLAG-01、R-SEC-01、§4.7。
- 依赖：M1-05、M1-02。
- 前置事实：当前 Browser 主要为合同草稿，没有完整 runtime 供应链；旧计划要求按需 staging+atomic switch。
- 固定约束：hash/许可缺失即失败；implementation 可用本地 fixture；不执行未验证资产。
- 决策空间：可按平台拆 manifest，公共 schema/version 必须相同；镜像源只能显式配置。
- 产物：versioned manifest/schema、三平台 asset matrix、license/SBOM、hash fixture 和 CI verifier。
- 实施步骤：
  1. 固定 wrapper/Node/Playwright/Chromium revision 和 platform/arch 组合。
  2. 生成 size/hash/source/license/SBOM，验证 unknown/missing/mismatch。
  3. 实现 manifest parser、签名/哈希前置门和 feature availability 投影。
  4. 在 CI/fixture 验证三平台选择，不下载或执行未验证资产。
- 验收断言：
  - `M7-01.A1`（contract）：每个支持 platform/arch 唯一解析到完整 manifest，unknown 明确 unsupported。
  - `M7-01.A2`（security）：任一 size/hash/schema/license mismatch 均拒绝安装/执行。
  - `M7-01.A3`（reproducibility）：同 manifest 在重复验证中 digest 稳定，SBOM/来源可机器读取。
- 验证：`node scripts/verify-product-experience.mjs --task M7-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M7-01.yaml` 与 manifest/SBOM 报告。
- 失败处理：阻止 Runtime availability；修供应链元数据/资产，不跳过 hash 或临时执行系统浏览器。

### M7-02 完成 Runtime 安装/修复、进程、Session 与恢复

- 结果：首次使用按需安装到 staging 后原子切换；每 Task 独立 Session/profile/进程树，重启后不自动拉起且可诊断/修复。
- 需求引用：R-BR-01、R-BR-02、R-BIND-01、R-REL-01。
- 依赖：M7-01、M6-02。
- 前置事实：Browser runtime 不应阻塞首屏；任务删除需清理 profile/截图/完整进程树。
- 固定约束：并发安装单飞锁；损坏 current 不覆盖可用旧版；进程必须绑定 task/session 并有上限。
- 决策空间：版本目录 side-by-side，current pointer 的原子实现按平台 adapter 选择。
- 产物：installer/repairer、runtime registry、process tree supervisor、Session/profile store、crash/restart tests。
- 实施步骤：
  1. 实现 staging download/copy、size/hash 验证、atomic activate 与旧版保留。
  2. 加入跨进程锁、并发请求合并、损坏检测和 repair。
  3. 按 Task 创建 profile/Session、多 Tab 状态与完整 child-process supervision。
  4. 覆盖 app/browser crash、kill、restart、orphan 和 task deletion。
- 验收断言：
  - `M7-02.A1`（reliability）：并发首次使用只安装一次；故障点后 current 指向完整可验证版本。
  - `M7-02.A2`（isolation）：不同 Task 的 profile/session/process 不共享 cookie/storage/active tab。
  - `M7-02.A3`（recovery）：应用重启不自动拉起浏览器，旧 Session 显示 crashed/stopped 且可显式恢复。
  - `M7-02.A4`（cleanup）：删除 Task 后 profile/截图/进程树清理，无 orphan；不确定路径拒绝删除。
- 验证：`node scripts/verify-product-experience.mjs --task M7-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M7-02.yaml` 与 process/profile ledger。
- 失败处理：保留最后可用版本并标 degraded；修 installer/supervisor，不 kill 非托管进程或递归删不确定目录。

### M7-03 完成 Browser 只读工具、脱敏与有界结果

- 结果：open/navigate/snapshot/screenshot/tabs/console/network-errors/close 可在 Task 隔离 Session 中工作，输出先脱敏再截断。
- 需求引用：R-BR-03、R-BR-04、R-SEC-01。
- 依赖：M7-02。
- 前置事实：旧计划冻结了只读工具集合、`wait` 白名单与 30 秒上限，不允许 raw eval/upload/download。
- 固定约束：工具输入 schema 严格；console/network/cookie/header 等敏感值脱敏；终态/错误不因截断丢失。
- 决策空间：snapshot 可使用结构化可访问树；截图以引用返回，不内联无限 base64。
- 产物：ToolGateway registrations、browser adapter、bounded result types、redaction fixtures、tool contract tests。
- 实施步骤：
  1. 为每个只读工具定义版本化 schema、timeout、size/count 边界和安全错误。
  2. 接入 task/session scope 与 §4.7 权限前置检查。
  3. 对 DOM、console、URL、headers、network error 和截图 metadata 先脱敏再截断。
  4. 覆盖超时、崩溃、超大页面、多 tab、redirect 与 unknown fields。
- 验收断言：
  - `M7-03.A1`（contract）：全部只读工具正/负 fixture 返回确定 schema，raw eval/upload/download 未注册。
  - `M7-03.A2`（security）：cookie/token/authorization/secret 在工具结果/log/evidence oracle 为 0 命中。
  - `M7-03.A3`（reliability）：超时/超大/崩溃结果有界且 Session 进入真实状态，后续合法调用可恢复或明确拒绝。
- 验证：`node scripts/verify-product-experience.mjs --task M7-03 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M7-03.yaml`。
- 失败处理：拒绝该调用并保留安全摘要；修 schema/redactor/buffer，不开放 eval 或记录原始 payload 调试。

### M7-04 完成 browse 权限、只读控制面板与 Task 隔离

- 结果：file:// 受 WorkspaceBinding 限制，localhost 可浏览，外部 exact origin 需 browse 授权；只读控制面板真实显示 Session/Tab/错误。
- 需求引用：R-BR-02、R-BR-04、R-PERM-01、R-WB-01。
- 依赖：M7-03、M1-03。
- 前置事实：redirect/popup/new tab/final URL 都可能改变 origin；不能只检查初始 URL。
- 固定约束：无 wildcard；browse 不含 interact；read-only Automation 也只得到 browse；file 逃逸拒绝。
- 决策空间：localhost 端口范围可按 exact origin 保存；一次/Task grant 与现有权限 UI 共享原语。
- 产物：OriginPolicy/grant store、navigation guard、read-only Browser panel、redirect/popup tests。
- 实施步骤：
  1. canonicalize file URL 并对 WorkspaceBinding 做 containment/escape 检查。
  2. 对初始、redirect、popup、新 tab、final URL 每次重算 exact origin 和 browse grant。
  3. 实现 Session/Tab/URL/screenshot/console/network error 的只读控制面板与关闭动作。
  4. 覆盖 grant once/task、撤销、过期、reload 和不同 Task 隔离。
- 验收断言：
  - `M7-04.A1`（security）：file 逃逸与未授权外部 origin 在所有导航转移点被拒绝，wildcard 不可创建。
  - `M7-04.A2`（permission）：browse grant 不能调用 click/type 等 interact 工具，read-only Automation 亦然。
  - `M7-04.A3`（E2E）：不同 Task 的 tabs/profile/grants/control panel 严格隔离，状态与实际 browser 一致。
  - `M7-04.A4`（accessibility）：仅键盘可查看/切换/关闭 tab、授权/撤销并理解错误。
- 验证：`node scripts/verify-product-experience.mjs --task M7-04 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M7-04.yaml` 与 origin negative matrix。
- 失败处理：停止导航并保持 Session 可控；修每跳校验/grant scope，不复用首 URL 决定或扩大 origin。

### M7-05 完成交互工具、interact 权限、安全绕过与删除清理

- 结果：click/type/select/press/scroll/wait 在显式 exact-origin interact grant 下可用，导航变化重检权限，Task 删除安全回收全部资源。
- 需求引用：R-BR-03、R-BR-04、R-SEC-01、R-AUTO-05。
- 依赖：M7-04。
- 前置事实：交互可能触发 redirect/popup/download/file chooser；旧计划明确不开放上传/下载与后台特权。
- 固定约束：browse 不升级 interact；redirect 后旧 grant 不沿用；上传/下载/raw eval 永不注册；Automation 无后台免批。
- 决策空间：对导航型 click 可在动作前后双重校验；wait 仅 selector/text/URL/load-state 且≤30 秒。
- 产物：interaction tools、pre/post origin guard、permission UI、attack fixtures、task-deletion cleanup。
- 实施步骤：
  1. 注册六类 interact 工具的严格 schema、timeout、可见目标与安全错误。
  2. 动作前检查当前 exact origin/grant，动作后检查 final URL/new tab/popup。
  3. 拒绝 download/upload/file chooser/eval/跨 Task session，并使 grant 可撤销/过期。
  4. 删除 Task 时终止进程树并安全删除已验证 profile/screenshot/session 元数据。
- 验收断言：
  - `M7-05.A1`（contract）：允许的 interact 工具正向可用，wait 白名单/30 秒上限生效，禁用工具未注册。
  - `M7-05.A2`（security-negative）：redirect/popup/new tab/final URL、prompt injection、跨 Task 与 browse-only 均不能绕过 interact。
  - `M7-05.A3`（cleanup）：Task 删除后托管资源为 0、非托管/不确定路径不被删除、无 orphan process。
  - `M7-05.A4`（regression）：撤销/过期 grant 立即拒绝新动作，既有页面仍可安全关闭和诊断。
- 验证：`node scripts/verify-product-experience.mjs --through M7 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M7-05.yaml` 与安全攻击矩阵。
- 失败处理：降级为 browse-only 或关闭 Session；修双重校验/cleanup，不增加后台特权或隐藏拒绝。

### M8-01 完成 Automation 持久化、Scheduler、DST、lease 与恢复

- 结果：Definition/Run 可持久化，once/hourly/daily/weekdays/weekly 调度在 timezone/DST/重启/并发下确定且不重叠。
- 需求引用：R-AUTO-01、R-STATUS-01、R-ERR-01、§4.8。
- 依赖：M1-05、M1-03。
- 前置事实：当前 Automation 主要是 feature gate/调度语义草稿，repository/scheduler/dispatcher 尚未形成产品闭环。
- 固定约束：IANA timezone；immutable run snapshot；idempotency+lease；单 Definition 不重叠；恢复只补最新一次。
- 决策空间：scheduler 可使用单调 wakeup + UTC 持久化，墙钟规则由独立纯函数计算。
- 产物：repository/migrations、schedule calculator、scheduler/lease/reconciler、fake-clock/DST/crash tests。
- 实施步骤：
  1. 落地 Definition/Run schema、状态、snapshot、idempotency key 与 additive migration。
  2. 实现 hourly UTC interval 与 local daily/weekdays/weekly 的 IANA 计算。
  3. 加入 DST 缺失/重复规则、lease claim/renew/reclaim 和单定义不重叠。
  4. 覆盖崩溃点、时钟跳变、暂停/恢复、missed 聚合与多进程竞争。
- 验收断言：
  - `M8-01.A1`（contract）：schedule/timezone/DST 金集输出精确，重复墙钟仅第一次、缺失墙钟在首个有效时刻。
  - `M8-01.A2`（reliability）：相同 idempotency key 只产生一个 Run，lease 竞争/过期恢复无重叠执行。
  - `M8-01.A3`（migration）：旧/空 DB migration 幂等可回读，definition snapshot 在后续编辑后不变。
  - `M8-01.A4`（recovery）：重启只补最新遗漏，其余以 skipped 聚合且 missed_count 正确。
- 验证：`node scripts/verify-product-experience.mjs --task M8-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M8-01.yaml` 与 DST/lease ledger。
- 失败处理：暂停受影响 Definition 并保存时钟/lease fixture；修纯函数/事务，不重复补跑或静默丢记录。

### M8-02 完成 Automation 管理 UI、Dispatcher、审计 Task 与 History

- 结果：用户可 CRUD、Run now、Pause/Resume、Cancel、查看 History 并跳转 Task/Worktree/Review；每次运行有可审计 Task。
- 需求引用：R-AUTO-02、R-STATUS-01、R-I18N-01、R-NOTIF-01。
- 依赖：M8-01、M2-04。
- 前置事实：feature 可能被隐藏；UI 不能显示未接 dispatcher 的假按钮。
- 固定约束：Provider/model/branch 不可用时失败并通知，不静默替换；cancel/暂停语义区分当前与未来 Run。
- 决策空间：列表/详情/History 可共用 Settings/Workbench 原语；Run now 仍需 idempotency 与权限快照。
- 产物：Automation scenes/forms/history、dispatcher、Task linking、状态/通知/deep-link tests。
- 实施步骤：
  1. 实现带验证的 Definition 创建/编辑/删除、schedule preview 与权限选择。
  2. Dispatcher 读取 immutable snapshot，验证 Provider/model/branch/binding 后创建审计 Task。
  3. 接入 Run now/Pause/Resume/Cancel 与共享 TaskStatus/Attention/notification。
  4. 实现 History→Task/Worktree/Review/错误详情跳转和 reload 恢复。
- 验收断言：
  - `M8-02.A1`（E2E）：仅键盘完成 CRUD、预览、Run now、暂停/恢复/取消和 History 跳转。
  - `M8-02.A2`（integration）：每个执行 Run 恰有一个审计 Task 与 immutable snapshot，状态在列表/History/Task 一致。
  - `M8-02.A3`（reliability）：Provider/model/branch 不可用确定失败并通知，未创建错误执行且无 fallback。
- 验证：`node scripts/verify-product-experience.mjs --task M8-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M8-02.yaml`。
- 失败处理：保持 Definition 可编辑并标失败；修 dispatcher/link，不自动选其他模型/branch 或吞掉运行历史。

### M8-03 完成 ToolGateway 强制的 read-only Executor

- 结果：read-only Automation 在工具注册阶段就无法获得写文件、Shell、mutating MCP 或 Browser interact，prompt injection 不能提升能力。
- 需求引用：R-AUTO-03、R-BR-04、R-BIND-01、R-SEC-01。
- 依赖：M8-01、M6-02。
- 前置事实：只在 prompt 中写“不要修改”不构成安全边界；权限必须由 ToolGateway capability set 强制。
- 固定约束：默认拒绝 unknown/mutating 工具；只读文件仍受 WorkspaceBinding；browse 仍需 origin grant。
- 决策空间：能力表可按 tool metadata 生成，但 mutation 分类需要冻结审计和测试。
- 产物：read-only capability profile、registration filter、mutation classifier、prompt-injection/escape fixtures。
- 实施步骤：
  1. 盘点所有内置/MCP/Browser 工具及 read/write/network/mutation 风险元数据。
  2. 在注册阶段构建最小 allow set，运行期再校验 binding/origin/scope。
  3. 拒绝 write/delete/shell/mutating MCP/browser interact 和 unknown capability。
  4. 用间接调用、别名、prompt injection、symlink 与 stale grant 做攻击测试。
- 验收断言：
  - `M8-03.A1`（contract）：read-only 注册表中 mutating/shell/interact/unknown 工具数为 0。
  - `M8-03.A2`（security-negative）：全部攻击 fixture 无文件/DB/外部状态变化，原工作区 digest 不变。
  - `M8-03.A3`（integration）：允许的 workspace read 与已授权 browse 正常工作且审计记录准确。
- 验证：`node scripts/verify-product-experience.mjs --task M8-03 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M8-03.yaml` 与 capability matrix。
- 失败处理：禁用 read-only Automation 执行并修分类/注册；不得依赖 prompt、事后 diff 或自动批准补救。

### M8-04 完成每 Run 独立 Worktree 的 isolated-write Executor

- 结果：isolated-write 每次运行创建新的托管 Worktree 并绑定所有消费者；创建/验证失败不启动 Agent，关闭 Worktree 开关时暂停待确认。
- 需求引用：R-AUTO-04、R-WT-01 至 R-WT-04、R-BIND-01。
- 依赖：M8-01、M6-03。
- 前置事实：Definition 可重复运行，复用同一写目录会污染并发/历史；旧计划要求每 Run 独立。
- 固定约束：不能写原项目；不能复用上次 Run Worktree；branch/base unavailable 直接失败；无 Local fallback。
- 决策空间：Run branch 名可含 automation/run ID；串行同 Definition 仍创建独立目录以保持审计。
- 产物：isolated executor、Run→ManagedWorktree link、pause/recovery rules、temporary-repo E2E。
- 实施步骤：
  1. Dispatcher 在 Agent 启动前根据 snapshot/base 创建本 Run 唯一 Worktree。
  2. 验证并持久化 binding 后向全部执行消费者注入同一 context。
  3. Worktree flag 关闭时暂停受影响 Definition，显式恢复才继续。
  4. 覆盖创建/持久化/启动故障、重复触发、重启和 branch/base 消失。
- 验收断言：
  - `M8-04.A1`（isolation）：两个 Run 使用不同 managed path/branch，原项目与彼此 worktree digest 隔离。
  - `M8-04.A2`（security）：binding 创建/验证失败时 Agent/工具启动次数为 0，无 Local fallback。
  - `M8-04.A3`（recovery）：关闭 flag 自动暂停且不改既有 Run binding；显式恢复后新 Run 使用新 Worktree。
- 验证：`node scripts/verify-product-experience.mjs --task M8-04 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M8-04.yaml` 与 Run/worktree ledger。
- 失败处理：保留已创建且不确定的 Worktree并标 Attention；修创建/事务，不清空或回退原项目。

### M8-05 完成审批恢复、Review/清理与 Automation × Browser

- 结果：后台审批跨重启可恢复且永不自动通过；有变化进入 Review，无变化仅在确认安全时清理；Browser 无额外后台特权。
- 需求引用：R-AUTO-05、R-BR-04、R-WT-04、R-PERM-01、R-NOTIF-01。
- 依赖：M8-02、M8-03、M8-04、M7-05。
- 前置事实：审批、Browser origin 与 Worktree 清理存在多重竞态，必须共享审计 Task/Run identity。
- 固定约束：审批无超时自动批准；重启后 stale handler 不可响应；read-only 永无 interact；有 commit/diff 必须保留。
- 决策空间：可在统一 Attention 中展示审批与 Review；原生通知只作提醒，不承载 secret/完整内容。
- 产物：persistent approval state/rebind、Review projector、safe cleanup decision、Automation Browser policy、recovery E2E。
- 实施步骤：
  1. 持久化非敏感 approval metadata、generation、scope 与 Run link，重启重新绑定或明确过期。
  2. 完成时检测 diff/commit/verification，决定 Review/keep/safe-cleanup。
  3. 将 read-only/isolated-write 的 Browser browse/interact 权限严格映射，无后台 bypass。
  4. 覆盖重启、取消、过期、通知拒绝、浏览器崩溃、无变化和有变化路径。
- 验收断言：
  - `M8-05.A1`（recovery）：重启后合法审批可恢复，stale/过期不可提交，任意超时都不会自动批准。
  - `M8-05.A2`（security）：read-only 永远没有 interact；isolated-write 仍需 exact-origin grant，后台无隐式授权。
  - `M8-05.A3`（integration）：有 diff/commit 进入 Review 并可跳转；无变化仅在 managed/clean/no-commit 时清理。
  - `M8-05.A4`（notification）：后台关键事件通知可达或安全降级，拒绝权限不改变 Run 终态。
- 验证：`node scripts/verify-product-experience.mjs --through M8 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M8-05.yaml` 与 recovery/cleanup ledger。
- 失败处理：保持等待/Review/Worktree，不自动批准或删除；修 generation/policy/identity 后重放。

### M9-01 完成全域双语、正式文档与兼容/降级说明

- 结果：Shell、12 页 Settings 到 Worktree/Browser/Automation 的全部用户面 zh-CN/en-US 完整，Demo 来源、架构、运维、隐私、支持与降级文档和实现一致。
- 需求引用：R-I18N-01、R-ERR-01、R-ROLL-01、R-RELSE-01、R-SET-01、R-SET-02。
- 依赖：M5-03、M6-04、M7-05、M8-05。
- 前置事实：M1 仅建立基础门禁；后续新增文案和支持材料需最终收口，历史 evidence 不应被路径迁移改写。
- 固定约束：key/placeholder 一致；原生菜单/托盘/通知也覆盖；历史证据不可变；旧路径以迁移表解释。
- 决策空间：技术术语可保留英文但两种 locale 必须有自然上下文；support 文档按 guides/operations/platform/contracts/archive 分类。
- 产物：完整 locales、原生资源、README/architecture/operations/privacy/support、compat/degradation/path migration matrix。
- 实施步骤：
  1. 扫描所有 JSX/Rust/菜单/通知/错误和新增模块用户文案。
  2. 对齐两套 locale key/placeholder、12 Pane route/search/provenance 文案、复数/时间/状态格式并做双语视觉测试。
  3. 更新架构、操作、支持、隐私、降级和旧版本升级说明。
  4. 运行 Markdown/local link、路径迁移、历史 freeze digest 与文档一致性门禁。
- 验收断言：
  - `M9-01.A1`（i18n）：全域 zh-CN/en-US key/placeholder 一致，硬编码门禁命中为 0。
  - `M9-01.A2`（docs）：所有当前文档/脚本/源码链接可解析，历史 evidence 内容 digest 不变且旧→新映射可查。
  - `M9-01.A3`（visual）：双语、亮暗、目标视口无溢出/乱码/不可读截断，原生 surface 文案一致。
  - `M9-01.A4`（contract）：12 Pane 的 title/description/search/provenance key 在 zh-CN/en-US 一一对应；planned/new/existing 的用户文案不把 Demo 冒充实现状态。
- 验证：`node scripts/verify-product-experience.mjs --task M9-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M9-01.yaml` 与 docs/i18n 报告。
- 失败处理：修 locale/source/link；不得改写历史 evidence、删除语言、硬编码英文或把断链标忽略。

### M9-02 完成 Worktree × Browser × Automation 集成场景

- 结果：三条平台能力在线性只读与隔离写入场景中共享正确 Task/Run/Binding/permission identity，无越界或静默 fallback。
- 需求引用：R-BIND-01、R-WT-03、R-BR-04、R-AUTO-03、R-AUTO-04。
- 依赖：M6-04、M7-05、M8-05。
- 前置事实：各模块单独通过不能证明组合权限、cleanup 和 identity 正确。
- 固定约束：read-only 不能写/interact；isolated-write 只写本 Run Worktree；origin/approval/Review 仍逐层强制。
- 决策空间：确定性 E2E 使用临时 Git repo、本地 HTTP server 和 fake Provider/App Server。
- 产物：组合 E2E fixtures、identity/permission trace、original-repo digest、cleanup/recovery matrix。
- 实施步骤：
  1. 场景 A：read-only Automation 浏览本地/授权外部页面、读 Workspace、产出无变更结果。
  2. 场景 B：isolated-write 建 Worktree、浏览授权页面、写改动、验证并进入 Review。
  3. 注入 redirect、prompt injection、binding invalid、Provider failure、重启与取消。
  4. 检查原项目、不同 Task/Run、profile、grant、process 和 cleanup 的隔离。
- 验收断言：
  - `M9-02.A1`（E2E）：只读场景完成且文件/DB/外部 mutation 为 0，browse 不获得 interact。
  - `M9-02.A2`（E2E）：隔离写场景的所有写入仅在本 Run Worktree，Review/verification 深链正确。
  - `M9-02.A3`（security-negative）：所有组合攻击/失效 fixture fail closed，无 Local/provider/permission fallback。
  - `M9-02.A4`（cleanup）：取消/删除/完成后资源按合同清理或保留，原项目 digest 不变且无 orphan。
- 验证：`node scripts/verify-product-experience.mjs --task M9-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M9-02.yaml` 与 integration trace。
- 失败处理：停止组合场景并保留受影响 Worktree/Session；回到最早责任模块修复，不加跨模块特权。

### M9-03 完成安全审查、故障注入、三平台与累计质量门

- 结果：所有 required assertions 在同一 commit/revision 的 implementation profile 通过，无 P0/P1；跨平台 adapter、迁移、性能、隐私与恢复可复核。
- 需求引用：R-SEC-01、R-REL-01、R-ACC-01、R-RELSE-01、§7。
- 依赖：M9-01、M9-02。
- 前置事实：单任务绿不代表累计绿；当前 dev 有大量用户改动，报告必须记录准确 worktree digest。
- 固定约束：required 缺失失败；不同 revision 报告不能拼装；真机缺失只影响 external platform gate，不掩盖共用合同失败。
- 决策空间：Windows 本机、macOS/Linux CI adapter 与候选真机可分层，但同一语义断言共用。
- 产物：threat model/update、fault matrix、full harness reports、三平台 matrix、P0/P1 ledger、support bundle scan。
- 实施步骤：
  1. 对 close/provider/run/permission/worktree/browser/automation 注入崩溃、迟到、断网、权限拒绝和磁盘错误。
  2. 跑全前端/Rust/契约/迁移/E2E/视觉/a11y/性能/security-negative。
  3. 扫描 secret/raw reasoning/cookie/token、危险 fallback、orphan process 和历史/live 不一致。
  4. 在同一 revision 汇总 Windows/macOS/Linux 共用断言、adapter 与 external pending。
- 验收断言：
  - `M9-03.A1`（reliability）：故障矩阵所有终态确定、可恢复且无假成功/假运行/orphan。
  - `M9-03.A2`（security）：敏感 oracle、权限绕过、原项目越界写和危险自动清理均为 0。
  - `M9-03.A3`（cross-platform）：三平台共用合同/adapter 门通过，平台差异没有语义分叉。
  - `M9-03.A4`（regression）：`--through M9 --profile implementation` 同 revision required 全绿且 P0/P1=0。
- 验证：`node scripts/verify-product-experience.mjs --through M9 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M9-03.yaml` 与 M9 implementation 累计报告。
- 失败处理：保存最小故障 fixture并回到最早任务；不得跨 revision 拼报告、删断言、降阈值或把 P1 改文档问题。

### M9-04 完成接口冻结、候选 soak 与 production 外部放行记录

- 结果：实现接口/迁移冻结并生成候选清单；真实 Provider、签名包、Updater、托盘/通知和三平台 soak 以通过或 external pending 的诚实状态记录。
- 需求引用：R-RELSE-01、R-UPD-01、R-PROV-04、§8.2、§14。
- 依赖：M9-03。
- 前置事实：本 worklist 不授权提交、推送或发布；生产凭据/签名/外部权限不一定可用。
- 固定约束：mock 不冒充 production；正式发布需同 commit 全门通过、旧版本升级成功且无 P0/P1。
- 决策空间：用户明确授权后才运行小额真实 Provider probe 或候选安装包；否则保持 external pending。
- 产物：API/schema/migration freeze、candidate/production reports、soak ledger、upgrade/rollback checklist、external gate matrix。
- 实施步骤：
  1. 冻结公开 DTO/event/migration/locale/manifest 版本并运行 drift checker。
  2. 生成候选包验证清单，执行获授权的 Provider/CLI/安装包/Updater/托盘/通知 smoke。
  3. 进行候选 soak，记录 crash、资源、任务恢复、Provider/Browser/Automation 长时状态。
  4. 将每个 production gate 标为 passed/failed/external-pending，禁止在未授权时发布。
- 验收断言：
  - `M9-04.A1`（contract）：API/schema/migration/locale/manifest freeze digest 稳定且无未解释 drift。
  - `M9-04.A2`（candidate）：获授权的候选 smoke/soak 无 P0/P1，升级/回滚保持用户数据和旧版本可恢复。
  - `M9-04.A3`（production gate）：每个外部条件有 owner、环境、revision、时间和 passed/failed/pending，不以 fake 替代。
  - `M9-04.A4`（release safety）：无显式发布授权时发布/提交/推送次数为 0。
- 验证：`node scripts/verify-product-experience.mjs --through M9 --profile candidate`；正式环境另用 `--profile production`。
- 证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/M9-04.yaml` 与 candidate/production gate 报告。
- 失败处理：保持 implementation_verified 并把外部 gate 标 failed/pending；修问题或等待授权，不发布、不伪造通过。

## 12. 连续执行与恢复状态机

### 12.1 固定循环

```text
preflight
  → 读取 current TaskPacket、对应任务卡和 owned_paths
  → 选择编号最小且 depends_on 全通过的未完成 MUST
  → 校验工作区与其他执行者无文件所有权冲突
  → 实施一个可验证 step，立即更新 packet
  → 运行 --task
       ├─ fail：保存失败证据 → 定位根因 → 聚焦修复 → 重跑
       └─ pass：运行 --through 当前 milestone
  → 归档 evidence，勾选 §10 唯一 Checkbox
  → 更新 §0 volatile 进度并立即选择下一 ready 项
```

### 12.2 TaskPacket 状态

- 活跃包固定为 `artifacts/ai-tasks/current.yaml`，`worklist_id: product-experience-gap-closure`，schema 使用 `ai-task-packet.v1`。
- `status` 只允许 `active | verification_failed | needs_authority | passed`；测试失败属于 `verification_failed`，不是用户阻塞。
- 每个 step 有稳定 `TASK_ID.SN`；每个断言必须引用 §11 已存在的唯一 `TASK_ID.AN`，不得临时创造未登记验收口径。
- `owned_paths`/`changed_paths` 使用仓库相对路径；并行执行前 Coordinator 验证无交集。公共 DTO、migration、handler registry、全局 token 只能由指定 owner 串行修改。
- 中断恢复先核对实际代码与 packet；若不一致，以代码、测试、可访问证据为准修 packet，不凭 YAML 宣称完成。

### 12.3 权限与停顿

- 可自主处理：仓库内可逆实现、测试、fixture、文档与本地 mock；按任务卡选择安全、简单、可测试的实现。
- 必须请求新授权：真实生产发布/删除、提交/推送、修改全局 Provider/Codex 配置、使用未授权真实费用、需要第二主体凭据或不可逆外部动作。
- 外部 production gate 缺失不阻止继续完成 implementation profile；以 `external-pending` 记录 owner/条件，不把整个 worklist 标 blocked。
- 只有同一阻塞在重复审计后仍无法通过安全替代继续时才进入 `needs_authority`；普通测试、编译、格式或 mock 失败继续修复。

## 13. 证据、追踪与完成协议

### 13.1 路径与最小字段

- 当前包：`artifacts/ai-tasks/current.yaml`。
- 任务证据：`artifacts/ai-tasks/evidence/product-experience-gap-closure/<TASK_ID>.yaml`。
- 验证报告：`artifacts/ai-tasks/verification/product-experience-gap-closure/<profile>/<task-or-milestone>.json`。
- 视觉证据：对应任务 evidence 引用的截图索引；截图本身不能证明协议、持久化、隐私、权限或终态。
- 每份 evidence 至少记录：task/RequirementRef/assertion、命令、exit code、开始/结束时间、revision/worktree digest、changed paths、关键决定、报告/截图路径与 external gate 状态。

### 13.2 完成判据

1. 单任务只有在全部 required Assertion ID 通过、证据可读、task cumulative gate 通过后才勾选 §10。
2. 里程碑只有在全部依赖任务已勾选且 `--through` 同 revision 通过时完成；旧 revision 报告不拼接。
3. `implementation_verified`、`candidate_verified`、`production_release_ready` 是三个不同状态；任何 UI/README/CHANGELOG 不得混称。
4. required assertion/fixture/metric/平台 adapter 缺失视为失败；真实账号/签名/通知权限等外部条件可明确 pending。
5. secret、API key、cookie、authorization、raw reasoning、完整敏感工具输出禁止进入 packet、证据、log、支持包和截图索引。

### 13.3 失败处理与反作弊

- 保存最小失败 fixture、metric 或截图后修最早责任层；不得删除测试、缩小 source、修改 oracle、更新错误视觉基线、降低阈值、延长无限 timeout 或隐藏错误。
- 不以 `todo`、不可点击按钮、mock 标签缺失、仅截图、仅编译、仅 happy path 或历史报告宣称能力完成。
- 安全能力默认 fail closed；Provider、Binding、permission、Worktree、Browser 与 Automation 失败不得静默 fallback。
- 对用户当前未提交改动只做所需的精确增量；不 reset、stash、checkout、清理或顺手格式化无关文件。

## 14. 风险、兼容、发布与外部放行

| 风险 | 预防与恢复 |
| --- | --- |
| 视觉重构再次形成覆盖层 | M2-01 单一 token/CSS owner、冲突扫描、旧 alias 有截止门 |
| Close dialog 只覆盖自绘 X | Host `prevent_close`、三入口同 fixture、bypass reason 白名单 |
| Provider 启动探测阻塞/计费/覆盖新配置 | Shell first、TTL、并发 2、锁外 I/O、fingerprint/CAS、设置 opt-out/费用说明 |
| 父失败但 child/tool 假运行 | 共享 terminal cascade、≤1 秒门、tombstone、live/history 同 reducer |
| 聚合审批扩大权限 | canonical WorkspaceBinding + risk class；仅 read/list/search；写/删/网/mutation 独立 |
| 执行台靠扩大窗口解决布局 | OS bounds E2E；固定栏/抽屉只改变 WebView 布局 |
| Worktree 清理误删用户资产 | managed identity + clean/no-commit 二次校验；不确定即保留/Attention |
| Browser 供应链或 origin 绕过 | manifest hash/SBOM、每跳 exact-origin 校验、browse/interact 分离、无 eval/upload/download |
| Automation prompt 提权或重复执行 | 注册期能力过滤、immutable snapshot、idempotency、lease、无自动批准 |
| 文档移动破坏编译/历史证据 | OCR fixture 与 docs 解耦；当前链接迁移；历史 evidence 不改写；旧→新映射 |
| dirty dev 基线混入他人改动 | owned_paths、worktree digest、单 owner 公共文件、同 revision cumulative reports |

兼容与发布规则：

- 配置/DB/event/locale/manifest 迁移只做 additive、版本化、幂等；unknown future 字段安全降级，旧版本回读由 migration tests 证明。
- 新体验在 M5-03 前后由内部 flag 保护；回退只换 presentation，不更改底层状态、安全与数据。
- Browser/Automation/Worktree 未闭环时前后端同时禁用；不显示假入口。
- 正式发布必须满足同一 commit 的 implementation/candidate/production 门、三平台签名安装包、旧版本升级、Updater、托盘/通知权限、真实 Provider smoke、soak 和无 P0/P1；本计划不授权发布。
- 缺少真实凭据、签名基础设施或外部管理员授权时，继续完成可确定性验证部分并记录 `external-pending`，不得用 mock 冒充。

<!-- AI_WORKLIST_CONTRACT_END -->
