# R-Code 完整 App 可点击 Demo 与产品体验重构交付

本目录是本轮 UI 与交互重构的设计交付区。`prototype.html` 以完整 App 壳层为入口，覆盖任务工作区、窗口生命周期、Provider 健康、运行反馈、子代理和完整 Settings Scene。它回答的是“产品应该怎样工作和呈现”，不是已经合入生产 UI、真实 IPC 已接线或未来能力已经可用的证明。production Settings source inventory 语义门禁已经通过；D0-01 仍保持未完成，直到截图清单、原型 SHA、浏览器诊断、链接与组合门禁在同一 revision 上由 `images/capture-manifest.json` 和 gate 报告共同复证，不能凭目录中的历史 PNG 数量或文件名判断完成。

## 可打开的成果

- [可点击 HTML 原型](./prototype.html)
- [PRD / AI 实施 Checklist](./r-code-experience-redesign-prd.md)
- [Settings 功能保全说明](./settings-capability-coverage.md)
- [127 项三层 Settings 能力盘点（其中 111 项为生产零丢失下界）](./settings-capability-baseline.json)
- [Settings 能力门禁报告](./settings-capability-gate.json)
- [文档固化清单](./r-code-experience-redesign-freeze.yaml)
- [文档门禁报告](./worklist-gate.json)

## Demo 证据边界

原型中的区块使用三种来源语义；来源标签描述的是设计证据，不是第二套产品状态：

| 标签 | 含义 | 可以据此声称 | 不可以据此声称 |
| --- | --- | --- | --- |
| `production_existing` / **生产现有能力** | 当前 revision 的 production symbol、handler/config authority 与语义已由 source inventory gate 解析；HTML 只用脱敏 mock 重现其信息结构 | 该能力可作为现有产品事实输入，但仍需 implementation assertion | 当前 dirty `dev` 已通过完整实现门禁，或 Demo 数据来自真实用户配置 |
| `new_requirement` / **本轮新增需求** | 本轮新增或重构的目标交互，如关闭选择、Provider 启动健康、聚合只读审批和浅色材料规范 | PRD 将其冻结为待实施合同 | Tauri Host、持久化、跨重启或系统权限已经接线 |
| `planned_demo` / **规划 Demo** | 为 Browser Runtime / Browser 授权等后续正式能力展示入口与状态，不属于当前已验证产品实现 | 可评审未来 IA、文案和安全边界 | 入口可在生产使用，或 feature flag / 后端能力已经完成 |

混合页面必须在卡片级标注来源。例如“工具与浏览器”中的 Shell、RTK、MCP 是真实能力投影，而 Browser Runtime 是规划 Demo；不能用整页标题把规划能力包装成现状。

现有功能不是按“页面数量”保全，而是按 source-pinned baseline 的逐项能力合同保全。当前共盘点 127 个 `CapabilityID`：其中 111 项 `production_existing` 构成零丢失下界，处置为 103 项原位保留、8 项带兼容合同迁移、0 项合并、0 项退役；另有 14 项 `new_requirement` 与 2 项 `planned_demo`，分别以 `add` / `demo` 追踪，不能反向伪装成生产现状。当前语义 gate 已验证 47/47 source manifest，总 inventory / mapped / prototype-target / planned-product-target / trace 均为 127/127，生产 source-audited 为 111/111，symbol resolution、unmapped、duplicate、orphan、empty provenance 全部为 0；三层 provenance counts 为 `111 / 14 / 2`。这证明生产能力下界及设计/计划承接，不证明产品实现；正式 UI→IPC→Host→persistence 的 `verified_count` 仍为 0。任何字段、动作、只读状态、默认值、引用约束、失败恢复或 Host 副作用都不能以“合并收束”为名消失；没有独立 RequirementRef 和用户批准的删除继续使门禁失败。

## 完整 App 可点击范围

工作区入口覆盖新建任务、项目切换、任务状态、模型/Agent 选择、发送与排队、Run Capsule、Attention、子代理、变更、Provider 健康、关闭选择和托盘恢复模拟。设置不再是小型弹窗，而是可搜索、可返回且保持焦点上下文的完整场景，共 12 个一级页：

| 设置页 | Demo 来源 | 关键闭环 |
| --- | --- | --- |
| 模型服务 | `production_existing + new_requirement` | 真实持久字段、不可改 name/preset、canonical default 仅在 Host ACK 后切换、默认/槽位引用删除拒绝、dirty/partial failure、2048 下限、5m 模型目录 cache 与独立 health receipt；原图/显式 OCR/helper 的无损图片路由；Provider/Image GuideSheet |
| Agent 编排 | `production_existing + new_requirement` | 四态委派、质量复核、Plan 双开关、10 项运行护栏；Codex model/reasoning/verbosity 偏好与 Plan GuideSheet，权限移至单一 Permissions authority；Codex 取消等待底层进程 terminate ACK |
| 子代理配置 | `production_existing + new_requirement` | 六态来源 → exact source+model 测试 → 三槽/100%/每槽 Prompt → revision conflict 的 local/fresh discard/reapply/merge；Subagent GuideSheet |
| 工具与浏览器 | `production_existing + new_requirement + planned_demo` | Shell 三态且路径保存立即生效、RTK 来源/回滚、MCP 稳定只读 `server_id`、保存 disabled→Host exact preview/token→独立启用审批；Browser Runtime 仅作规划演示 |
| 知识与指令 | `production_existing` | 全局/项目作用域；Memory 审批/任务/版本/清空；Prompt append/override；Skill 继承/同步/恢复/删除 |
| 权限 | `production_existing + new_requirement + planned_demo` | Codex 五态单 authority、`custom` 只读兼容、项目权限与同风险只读聚合审批；Browser grant 仅作规划演示 |
| 隐私与安全 | `production_existing + new_requirement` | 凭据/CSP/脱敏只读状态 → 本地缓存清理范围预览 |
| 外观与语言 | `production_existing + new_requirement` | 语言/主题/密度 → 独立浅色材料 → 减少运动 → Companion revision/声音/形态/位置恢复 |
| 通知 | `production_existing + new_requirement` | 权限检查/申请 → 分类开关 → 应用内测试通知 |
| 启动与关闭 | `new_requirement` | `ask / hide / quit` → 关闭预览 → 偏好重置；Provider 启动检查可关闭且取消旧 generation；退出 ACK 后 Run/子代理/工具/计时器全部终结 |
| 更新 | `production_existing` | 完整状态机、版本/发行说明/bytes、检查→下载→安装/稍后重启，以及 failed_operation 精确恢复 |
| 诊断 | `production_existing + new_requirement` | 请求构成、实时日志/过滤/暂停/保留 → 自检 → 无写盘预览 → 选择目录 → 导出路径 |

表中的 `production_existing` 已由当前 source inventory gate 逐能力确认，但仍只是来源分类；它不能替代 M2/M5 的实现与跨重启证据。

设置搜索可跨页跳转到 Provider、OCR、Agent、权限、主题、关闭、更新和诊断项；搜索无结果、返回工作区、键盘焦点恢复和窄屏导航都是交付的一部分。产品实现还必须覆盖 `loading/ready/stale_last_good/failed/retrying`、dirty/discard/revision conflict、四个独立 GuideSheet、MCP exact-launch 独立审批，以及 390px 下主要操作 44×44 CSS px 的触控门禁；原型画面不能替代这些 Host/持久化证明。

关键画面：

| 画面 | 目的 |
| --- | --- |
| [主工作区运行态](./images/05-workspace-running-dark.png) | 验证中性 loading、公开 commentary、折叠 Run 与执行台职责 |
| [关闭窗口选择](./images/32-close-choice-dark.png) | 验证“托盘 / 退出 / 不再提示”及活动任务说明 |
| [Provider 启动检查](./images/01-launch-provider-checking-dark.png) | 验证非阻塞、有界并发的连接检查与队列 |
| [子代理详情](./images/21-subagent-created-detail-dark.png) | 验证父子层级、槽位/模型、公开 transcript 和停止反馈 |
| [主工作区浅色](./images/65-workspace-light.png) | 验证浅色使用独立材料而非暗色黑 wash |
| [Provider 浅色编辑](./images/82-provider-editor-light.png) | 验证复杂配置表单、遮罩和投影在亮色下可读 |
| [Knowledge 深层配置](./images/93-knowledge-memory-review-dark.png) | 验证记忆审批、复盘任务和作用域没有被压成单一文本框 |
| [390px 配置中心](./images/81-settings-responsive-390-dark.png) | 验证窄屏导航和复杂设置卡片重排 |

## 设计主张

为长时间运行 Agent 任务的开发者设计一个安静、可诊断的玻璃化任务驾驶舱：主对话只承载用户内容、模型公开阶段反馈和最终交付；执行轨迹收进可展开摘要；右侧执行台只承载状态、子代理和变更。

五个关键决策：

1. **玻璃用于空间分层，不用于所有内容。** Topbar、Composer、浮层和执行台使用有深度的透明材料；长文本区域维持稳定底色和足够对比度。
2. **公开反馈与原始轨迹分层。** `commentary` 仍在主对话中出现；命令、文件、MCP 和重复工具动作默认聚合为一条 Run Capsule。失败、审批、提问和最终回答永远保持可发现。
3. **执行台不是第二条时间线。** 右侧只提供“概览 / 子代理 / 变更”，删除与主 Timeline 重复的全局工具调用列表；工具详情回到其发生的运行或子代理 transcript。
4. **连接健康是全局能力。** Provider 探测不再绑定设置页生命周期；启动后后台复用回执并只检查过期的关键连接，不阻塞首屏、不自动 fallback。
5. **运动只表示真实活动。** session、Run、Provider、工具和子代理只有 `running / checking` 使用同一套中性 spinner；`queued / waiting / approval / completed / failed / cancelled / skipped` 使用静态图形和文字，Agent 身份色不承担状态语义。

## 生产运行审计边界

2026-08-27 已在当前 `dev` 上通过 `dev.ps1` 构建并启动生产开发壳，使用已经配置的默认模型完成主代理与子代理交互。运行审计确认六个必须由实施任务关闭的真实问题：

1. Settings 显示 DeepSeek/ark 可使用时，首次空白页仍提示连接服务并短暂回退到 Codex/GPT-5.6-Sol，说明 canonical default 与页面投影不一致。
2. 子代理候选回执过期会让首次委派失败，进入设置批量测试后才可成功，说明 readiness 仍错误绑定设置页生命周期。
3. 父 Run 失败或取消后，子代理、工具和计时器仍可长期显示 running，缺少 Host 终态级联与退出 ACK。
4. 子代理只读读取同一工作区三个文件会出现三张审批卡，缺少 canonical WorkspaceBinding 范围内的同风险聚合。
5. 打开执行台会改变并重定位顶层窗口，而不是仅在当前 WebView 中完成响应式布局。
6. 发送按钮切换为停止按钮的过渡曾造成误触中止，且 Composer 正文短暂滞留，发送/追加/停止没有完全解耦。

同一轮累计回归中 Rust/core 通过；前端共 254 项，结果为 `244 pass / 8 fail / 2 skip`。8 项失败集中在 app-shell/Companion 的等待、归档和工作台旧入口等路径，属于 M0-02 的真实红线，不能用历史 `38/38` 或原型通过替代。运行证据只记录脱敏状态与结果，不把真实 Provider 密钥、完整 prompt/response 或 raw reasoning 写入本目录。

本目录中的功能合同同时以生产源码、配置/IPC/Host/persistence 路径和上述运行审计为事实输入，HTML 交互仍只使用脱敏 mock。机器 gate 已把 source snapshot、symbol resolution、逐项 contract/trace 与三层 provenance 写入报告，但正式逐控件 UI→IPC→Host→persistence 和跨重启 `verified_count` 仍为 0。

源码冻结的关键实现下界包括：Provider 只持久化 `base_url/model/provider_kind/max_tokens/temperature/protocol/show_reasoning`，保存动作的 `activate` 不是字段，canonical default 只能在 Host ACK 后发布，默认 Provider 或持久子代理槽位引用的 Provider 不能删除；模型目录 5 分钟 cache 与健康回执 30/5 分钟 cache 相互独立；图片路由保留 confirmed-multimodal 原图直发、用户显式 OCR 和完整 helper 三条互斥路径，helper 失败不自动降级 OCR；Codex 权限只有 `read_only/request_approval/auto_review/full_access/custom` 一个 authority，登录取消须等待对应 Host 进程 terminate ACK；MCP 的既有 `server_id` 是稳定只读主键，启用仍由 Host exact preview + one-time token 独立审批；每个子代理槽单独保存 Prompt；Shell path 保存后立即更新 gateway override 并失效 cache。它们必须由 D0 semantic gate 与后续正式 E2E 逐层证明，不能由原型控件替代。

设计保留了仓库中已有价值的项目/任务层级、固定 Composer、暖色品牌信号、Provider 延迟/槽位信息、权限卡和子代理聚焦视图；用户反馈与源码合同进一步冻结为共享 Provider 快照、公开 commentary、折叠轨迹、父终态级联、聚合只读审批、固定执行台、关闭选择及独立停止动作。它们在 Demo 中可评审，在生产中仍必须通过 PRD 的 implementation/candidate/production 分层门禁。

## 核心流程与状态

### 1. 模型工作反馈

```text
用户请求
  → 模型公开 commentary（发现 / 阶段 / 下一步）
  → Run Capsule（默认紧凑，当前阶段持续可见）
      → 按需展开阶段、子代理与脱敏工具摘要
  → 失败 / 审批 / 提问自动展开
  → final answer 始终独立、完整可见
```

- 当前 Run：显示阶段、耗时、最近有效变化、子代理数和 Attention。
- 已完成普通工具组：自动折叠为数量、耗时与结果摘要。
- 用户手动展开后：本轮内保持用户选择，不因新事件抢回折叠状态。
- 无模型 commentary 时：宿主只显示由真实事件确定的阶段和活动，不伪造“模型正在思考什么”。

### 2. 关闭窗口

```text
CloseRequested
  → close_action = ask ? 阻止关闭并显示单例对话框
  → hide / minimize_to_tray ? 验证恢复入口后隐藏
  → quit ? 走统一退出与运行清理路径
```

- `不再提示` 不是独立布尔值；它把用户实际选择保存为 `hide` 或 `quit`。
- 取消和 Escape 不保存偏好，焦点返回原关闭按钮。
- 有活动任务时默认聚焦“最小化到托盘”，并说明任务会继续运行。
- Windows 使用“最小化到系统托盘”；macOS 使用“隐藏窗口”；没有恢复入口的平台禁用隐藏选项。
- Tray 菜单退出、Updater 重启和 OS shutdown 不得再次弹同一个对话框。
- 所有显式退出共享一个有界 ShutdownCoordinator；只有 Agent、子代理、工具、Browser、Automation、Companion 与持久化 flush 返回 ACK 或被记录为有界失败后才完成退出。重启后不得把旧 running 投影复活，退出终态页面不得残留 spinner。

### 3. Provider 启动健康

```text
Shell 可交互
  → Host 读取 configured + fingerprint + health receipt
  → receipt 新鲜：沿用
  → receipt 缺失 / 过期 / fingerprint 变化：后台有界探测
  → 全局健康入口更新 connected / degraded / failed
```

- `configured` 与 `connectivity` 明确分开。
- 只优先探测默认 Provider 和已保存子代理槽位；并发有上限，失败有退避。
- 成功回执 TTL 为 30 分钟，失败回执 TTL 为 5 分钟；配置指纹变化会让旧回执失效。
- API Provider 的 exact-model probe 可能产生少量费用；设置中必须明确说明并可关闭启动探测。
- 关闭启动探测会递增 policy generation，并取消或忽略该 generation 的排队/在途结果；迟到结果既不能写回 receipt，也不能合成 connected/success。手动测试仍保持独立可用。
- 失败不阻塞首屏、不自动替换 Provider 或模型、不暴露密钥和请求正文。
- canonical default 只在 Host 成功 ACK 后发布；Host reject 必须保持旧 default、旧 profile 和所有引用，Topbar、空白页、Composer、Settings、健康入口和主 Agent 默认模型都消费同一 snapshot。

### 4. 子代理与执行台

- 协作树显示父子关系、运行状态、当前阶段、Attention 和最近有效更新。
- 点击子代理进入详情；返回后保持主任务上下文。
- 子代理自己的工具轨迹留在其 transcript，不在全局 Summary 再复制一遍。
- “变更”只显示文件、diff 概要、验证与审核入口。

### 5. Settings 加载、草稿与高风险动作

```text
load → ready(snapshot)
  ├─ failed + last-good → stale read-only + retry
  ├─ failed + no snapshot → explicit error + retry
  └─ edit → dirty(base revision) → save(base revision) / discard / conflict

conflict(local, fresh Host)
  → discard local
  → reapply local onto latest revision
  → field-level merge with explicit preview

MCP save(disabled) → Host exact preview + token → 独立 alertdialog → confirm enable
```

- 12 页共享同一 Settings lifecycle；领域状态机仍独立，但 load/refresh/retry/save/back 都必须经过 revision-aware reducer，不能各页自建“保存成功”。
- refresh/retry 先比较 revision，不能覆盖较新的 dirty draft；Host CAS 冲突同时保留 local 与 fresh Host snapshot，并只允许显式 `discard / reapply / field merge` 三路恢复，每条路径都产生新的 base revision 和可复验证据。
- Provider 被子代理槽引用时删除由 Host 拒绝；credential/config 非原子失败必须可补偿或恢复。
- MCP `server_id` 是既有稳定主键：编辑器只读显示，submit 时 Host 再校验；“改名”只能 create-new 后显式 remove-old，credential 状态和值不得由 UI 猜测迁移。
- Codex cancel 必须把 operation/process ID 和 generation 交给 Host，并等待底层 terminate ACK；超时进入可重试 `cancel_failed`，旧 generation 不能回写 login/ready。
- Provider、Plan、Subagent、Image 四个 GuideSheet 各有入口、深链、焦点陷阱、Escape/backdrop 与触发器焦点恢复。
- 390px 下主操作/图标按钮命中区至少 44×44 CSS px，紧凑次要控件至少 32×32 且不重叠。

## 视觉系统摘要

- 深色底：矿物质蓝黑，避免延续当前大面积棕色。
- 品牌信号：暖橙用于主行动和重要阶段；冷青用于运行成功与连接健康。
- 材料：透明面板必须同时具备边界、内高光和不透明 fallback；浅色主题使用独立的 canvas、content、sunken、card/floating/overlay shadow 和 scrim token，不复用黑色 overlay 或暗色大投影。
- 字体：离线系统字体，中文正文不低于 13–14px，必要元信息不低于 11–12px。
- 动效：只解释展开、浮层、状态切换和空间连续性；只有 `running / checking` 可持续旋转，终态保持静止；`prefers-reduced-motion` 下移除位移、缩放和旋转，同时保留静态缺口环与状态文字。

## 原型交互

原型为离线单文件，不加载 CDN、字体或第三方素材，也不包含 API key。所有保存、安装、退出、权限和系统通知动作都只在当前 Demo 会话中模拟。可操作项：

- 顶部连接状态：打开 Provider 健康面板，触发 checking、offline、401、timeout、recovered 与 opt-out；401 和 timeout 都有恢复动作。
- 顶部月亮按钮：切换深色 / 浅色，并同步可访问状态名称。
- 运行摘要：展开阶段，再展开脱敏工具摘要；右侧“原型状态预览”可切换 running、question、approval、failure、completed。
- 执行台：切换概览、子代理、变更；方向键切换 tab；协作树展示主从层级，子代理详情含槽位、Provider/模型和公开 transcript。
- 窗口关闭按钮：根据有无活动任务和 tray 可用性更新对话框；焦点限制在 modal 内，Escape/取消返回原按钮且取消不会保留 checkbox 草稿。
- 设置：从 Rail 进入完整 Settings Scene，在 12 个一级页间导航或搜索跳转；可演示 Provider 编辑/测试、子代理权重、权限聚合、主题/语言/减少运动、通知、关闭偏好、更新状态和诊断支持包，再返回原任务并恢复焦点。
- Composer：`Enter` 发送、`Alt+Enter` 排队、`Shift+Enter` 换行；每条路径都有可见及 `aria-live` 反馈。
- 其他可点击控件也提供状态变化或明确的原型反馈，不保留无响应假按钮。

## 关键端到端 Demo 闭环

1. **Provider**：设置 → 模型服务 → 编辑或新建 → 测试连接 → 回到全局健康入口；配置、连通性和默认选择始终是不同字段。
2. **关闭与恢复**：设置 → 启动与关闭 → 选择/重置偏好 → 点击窗口关闭 → 托盘或退出 → 托盘恢复；取消不保存，托盘不可用不隐藏。
3. **长任务协作**：发送任务 → commentary → Run Capsule → 子代理详情 → question/approval/failure → final → 变更/验证；父终态后不存在仍旋转的 child/tool。
4. **通知**：设置 → 通知 → 权限检查/申请 → 分类开关 → 应用内测试；系统权限失败只降级，不阻塞任务。
5. **更新**：设置 → 更新 → 检查 → 下载/校验 → 安装重启或稍后；安装重启不得再次触发普通关闭询问。
6. **规划能力**：工具与浏览器 / 权限中的 Browser 卡始终显示“规划 Demo”，模拟安装或授权不能把产品状态改成“已实现”。
7. **Settings 恢复**：load failure → last-good/no-snapshot → retry → dirty save reject → revision conflict → discard/reapply/merge；任何失败都不显示空成功或覆盖草稿。
8. **MCP 启用**：保存 disabled → Host exact preview/token → 独立 alertdialog → 确认；cancel/expire/config change 拒绝并恢复焦点，编辑/测试 token 消费为 0。
9. **四个 GuideSheet**：Provider/Plan/Subagent/Image 从卡片与搜索深链分别进入，键盘关闭后回到对应触发器；缺任一个不能用 Provider guide 代替。

## 截图证据与历史代表图

截图的唯一机器索引是 [capture-manifest.json](./images/capture-manifest.json)。不要根据最大编号、目录 PNG 总数或某张历史失败图推断“截图已齐”；只有 manifest 的 `status`、`prototype_sha256`、`generated_screenshots`、`screenshots`、`deleted_orphan_pngs`、`evidence` 与 `diagnostics` 共同描述某一次可复现运行。只有 manifest SHA 与当前 `prototype.html` 一致、required evidence 完整且浏览器诊断为零时，截图才可用于 D0；被本轮清理的 orphan 只能从 `deleted_orphan_pngs` 审计，不能再作为当前设计证据。下面链接只是便于人工浏览的代表图，不是完整清单，准确范围与数量始终以 manifest 为准。

### 启动、任务与 Composer

- Provider 启动健康：[01 检查中](./images/01-launch-provider-checking-dark.png) · [02 回执恢复](./images/02-launch-provider-recovered-dark.png) · [03 关闭自动检查后手动检查](./images/03-provider-manual-check-with-auto-off-dark.png) · [04 手动恢复](./images/04-provider-manual-check-recovered-dark.png)
- Run 反馈：[05 工作中](./images/05-workspace-running-dark.png) · [06 阶段展开](./images/06-run-expanded-dark.png) · [07 工具摘要展开](./images/07-run-tools-dark.png) · [08 终态不保留 spinner](./images/08-terminal-state-without-spinner-dark.png)
- 任务闭环：[09 新任务空态](./images/09-new-task-empty-dark.png) · [10 创建成功](./images/10-new-task-created-dark.png) · [11 任务/草稿隔离](./images/11-task-switch-and-draft-isolation-dark.png) · [12 运行中筛选](./images/12-filter-running-dark.png) · [13 需要处理筛选](./images/13-filter-attention-dark.png) · [14 历史筛选](./images/14-filter-history-dark.png)
- Composer 与项目：[15 Agent 选择](./images/15-selector-agent-dark.png) · [16 模型选择](./images/16-selector-model-dark.png) · [17 审批策略](./images/17-selector-policy-dark.png) · [18 添加项目](./images/18-project-add-dialog-dark.png) · [19 项目已加入](./images/19-project-added-dark.png)

### 子代理、变更与 Provider 恢复

- 子代理：[20 协作树](./images/20-subagent-tree-dark.png) · [21 创建并进入详情](./images/21-subagent-created-detail-dark.png) · [22 停止后的详情](./images/22-subagent-stopped-detail-dark.png) · [23 聚合终态](./images/23-subagent-stopped-tree-dark.png)
- 变更审核：[24 文件列表](./images/24-changes-list-dark.png) · [25 Diff](./images/25-diff-open-dark.png) · [26 已审阅](./images/26-diff-reviewed-dark.png)
- Provider 401 修复闭环：[27 全局失败入口](./images/27-provider-401-dark.png) · [28 编辑凭据](./images/28-provider-credential-edit-dark.png) · [29 测试中](./images/29-provider-credential-testing-dark.png) · [30 测试成功](./images/30-provider-credential-tested-dark.png) · [31 保存并恢复](./images/31-provider-recovered-after-save-dark.png)
- 窗口生命周期：[32 关闭选择](./images/32-close-choice-dark.png) · [33 隐藏到托盘](./images/33-tray-hidden-dark.png) · [34 从托盘恢复](./images/34-tray-restored-dark.png) · [35 退出终态](./images/35-exit-terminal-without-spinner-dark.png)

### 完整配置中心：12 个一级页

| 设置页 | 设计图 |
| --- | --- |
| 模型服务 | [36-settings-01-providers-dark.png](./images/36-settings-01-providers-dark.png) |
| Agent 编排 | [37-settings-02-agents-dark.png](./images/37-settings-02-agents-dark.png) |
| 子代理配置 | [38-settings-03-subagents-dark.png](./images/38-settings-03-subagents-dark.png) |
| 工具与浏览器 | [39-settings-04-tools-dark.png](./images/39-settings-04-tools-dark.png) |
| 知识与指令 | [40-settings-05-knowledge-dark.png](./images/40-settings-05-knowledge-dark.png) |
| 权限 | [41-settings-06-permissions-dark.png](./images/41-settings-06-permissions-dark.png) |
| 隐私与安全 | [42-settings-07-security-dark.png](./images/42-settings-07-security-dark.png) |
| 外观与语言 | [43-settings-08-appearance-dark.png](./images/43-settings-08-appearance-dark.png) |
| 通知 | [44-settings-09-notifications-dark.png](./images/44-settings-09-notifications-dark.png) |
| 启动与关闭 | [45-settings-10-lifecycle-dark.png](./images/45-settings-10-lifecycle-dark.png) |
| 更新 | [46-settings-11-updates-dark.png](./images/46-settings-11-updates-dark.png) |
| 诊断 | [47-settings-12-diagnostics-dark.png](./images/47-settings-12-diagnostics-dark.png) |

配置深层状态：

- 搜索与深链：[48 OCR 搜索结果](./images/48-settings-search-ocr-dark.png) · [49 跳转到图片理解](./images/49-settings-search-ocr-deeplink-dark.png)
- 子代理与规划能力：[50 权重校验失败](./images/50-subagent-slots-invalid-dark.png) · [51 保存成功](./images/51-subagent-slots-saved-dark.png) · [52 Browser 安装中](./images/52-browser-planning-installing-dark.png) · [53 Browser 规划就绪](./images/53-browser-planning-ready-dark.png) · [54 聚合只读授权](./images/54-permissions-approved-dark.png)
- 通知：[55 权限检查中](./images/55-notification-permission-checking-dark.png) · [56 已授权](./images/56-notification-permission-granted-dark.png)
- 更新：[57 检查中](./images/57-update-checking-dark.png) · [58 有可用版本](./images/58-update-available-dark.png) · [59 下载中](./images/59-update-downloading-dark.png) · [60 已下载](./images/60-update-downloaded-dark.png) · [61 安装并重启后已是最新](./images/61-update-install-and-restart-up-to-date-dark.png)
- 诊断：[62 自检与支持包预览](./images/62-diagnostics-self-check-and-support-dark.png)

### Settings 功能保全深层状态

- 能力地图与 Provider：[83 旧 7→新 12 功能地图](./images/83-settings-capability-map-dark.png) · [84 Provider 模板目录](./images/84-provider-preset-catalog-dark.png) · [85 校验阻断保存](./images/85-provider-validation-blocked-dark.png) · [86 未保存更改确认](./images/86-provider-unsaved-confirm-dark.png) · [87 图片 Provider 悬空](./images/87-image-provider-missing-dark.png)
- Agent 与子代理：[88 10 项运行护栏](./images/88-agent-run-guardrails-dark.png) · [89 Codex 运行偏好](./images/89-codex-runtime-preferences-dark.png) · [90 子代理 Prompt 与 revision](./images/90-subagent-prompt-and-revision-dark.png)
- MCP：[91 审批请求中](./images/91-mcp-approval-requesting-dark.png) · [92 exact launch plan 就绪](./images/92-mcp-launch-ready-dark.png)
- Knowledge：[93 Memory 审批与任务](./images/93-knowledge-memory-review-dark.png) · [94 Prompt append/override](./images/94-knowledge-prompt-append-override-dark.png) · [95 Skill 继承](./images/95-knowledge-skills-inheritance-dark.png) · [96 Skill 编辑器](./images/96-skill-editor-dark.png)
- 权限、Companion 与诊断：[97 Codex 全局权限作用域](./images/97-codex-permission-scope-dark.png) · [98 Companion 完整设置](./images/98-companion-complete-settings-dark.png) · [99 支持包选择目录与导出路径](./images/99-support-bundle-export-path-dark.png)

### 浅色、响应式、缩放与减少运动

- 浅色材料：[63 外观设置](./images/63-settings-appearance-light.png) · [64 模型服务](./images/64-settings-providers-light.png) · [65 工作区](./images/65-workspace-light.png) · [66 Diff](./images/66-diff-light.png) · [82 Provider 编辑弹窗](./images/82-provider-editor-light.png)
- 六档主界面：[67 1600×960](./images/67-responsive-1600x960-dark.png) · [68 1280×800](./images/68-responsive-1280x800-dark.png) · [69 1024×768](./images/69-responsive-1024x768-dark.png) · [70 960×640](./images/70-responsive-960x640-dark.png) · [71 740×800](./images/71-responsive-740x800-dark.png) · [72 390×844](./images/72-responsive-390x844-dark.png)
- 抽屉与窄屏导航：[73 1024 执行台](./images/73-responsive-1024-dock-open-dark.png) · [74 740 任务导航](./images/74-responsive-740-task-navigation-open-dark.png) · [75 390 任务导航](./images/75-responsive-390-task-navigation-open-dark.png)
- 缩放与运动：[76 200% 工作区](./images/76-scale-200-workspace-dark.png) · [77 200% 设置](./images/77-scale-200-settings-dark.png) · [78 200% Provider 面板](./images/78-scale-200-provider-popover-dark.png) · [79 减少运动运行态](./images/79-reduced-motion-running-state-dark.png)
- 配置中心窄屏：[80 740×800](./images/80-settings-responsive-740-dark.png) · [81 390×844](./images/81-settings-responsive-390-dark.png)

## 验证记录与当前缺口

- Playwright 捕获 console warning/error、page error 和 request failure；是否为 0 只读取与当前原型 SHA 匹配的 manifest `diagnostics`，历史报告不能沿用冒充通过。
- 捕获视口、生成截图、required evidence、stale 文件和每项断言均从 manifest 读取；PRD 的产品实现矩阵仍独立要求 960×640、1280×800、1440×900、390px 与 100–200% 缩放。
- 响应式：`≤1120px` 时执行台变成抽屉；关闭时同时 `aria-hidden + inert + visibility`，等待 180ms transition 完成后才截图；打开后焦点进入，关闭后回到触发按钮。
- 键盘：modal 正反向焦点圈闭、Escape、焦点恢复、tablist 左右/Home/End、Composer 三种 Enter 语义均由脚本断言。
- 可访问名称：所有当前可见按钮在 manifest 声明的视口和 200% 设备缩放下都经过 accessibility tree 检查；窄 Rail 不依赖被隐藏的视觉文字生成名称。
- 字体与对比：正文保持 13–14px，密集元信息设置 11px 可读下限；浅色主题的次要文字 token 已加深。
- 状态与运动：只有 `running / checking` 呈现中性 spinner；排队、等待和全部终态使用静态 glyph + 文字。`prefers-reduced-motion` 下 spinner 变为静态缺口环，transition/animation 近即时；200% 设备缩放单独截图验证。
- 截图与断言统一入口是从本目录运行 `python tools/capture_states.py`，等待字体、两帧布局和 transition settle 后写入 `images/` 与 manifest。成功标准是 exit 0、manifest 与当前原型 SHA 一致、required evidence/截图无缺项、stale 列表被显式隔离且 console/page/request diagnostics 全 0；不再在本文复制脚本内部状态枚举或猜测最终截图数。

已知限制：这是基于仓库事实与 mock 内容的完整 App 设计 Demo，未接入 Tauri IPC、真实 Provider、真实任务事件、持久化、系统通知、Updater 端点或 OS 托盘。source inventory gate 已通过，但产品实现、跨重启一致性、真实权限或跨平台生命周期仍未证明，当前 `verified_count=0`、主 Checklist `0/42`。D0 是否具备视觉证据只由同 SHA capture manifest、链接和组合 gate 判定；M5 还需同一 revision 的三个新进程/隔离 fixture 连续通过，任一失败或跨轮泄漏都从零重计。

## 可重复门禁

在仓库根目录按顺序运行：

```powershell
python docs/product-experience-redesign/tools/worklist_gate.py --update-freeze
python docs/product-experience-redesign/tools/capture_states.py
python docs/product-experience-redesign/tools/check_markdown_links.py
python docs/product-experience-redesign/tools/worklist_gate.py --check
```

第一条只在 PRD、能力基线或 validator source 发生规范性变化时更新 freeze；它会在进程内实时执行 `settings_capability_gate.py`、禁止其写入 standalone report，并把 validator source digest、live report digest/status/count 记录到 freeze 与 worklist report。`--check` 每次同样实时执行 validator，绝不信任可能陈旧的 `settings-capability-gate.json`，且不会写任何文件；连续两次检查必须得到相同摘要。

如需刷新独立的人类可读 Settings 报告，可显式运行 `python docs/product-experience-redesign/tools/settings_capability_gate.py --update-report`，但该文件不是组合门禁的输入；日常只读复核使用 `--check`。当前组合门禁允许在任务尚未完成时冻结执行合同，因此 `status: frozen` 不表示 D0 或产品实现完成；一旦 D0 Checkbox 被勾选而 live report 没有满足 source snapshot、symbol resolution、inventory/provenance 零缺口的 `source_inventory_proof`，或 capture manifest 与原型 SHA/required evidence/diagnostics 不一致，门禁必须非 0。当前主 Checklist 保持 `0/42`，`verified_count=0`；结构与追踪的准确计数以最新 live gate 报告为准。生产实施阶段还必须把 111 项现有生产能力的正式 UI→IPC→Host→persistence 证据补齐，并按任务实施 14 项新增需求与 2 项计划 Demo；设计门禁不会提前把它们标绿。
