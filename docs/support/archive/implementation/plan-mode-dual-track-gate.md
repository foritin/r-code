# DeepSeek 复杂任务 Plan 建议与 Plan-only 双轨

> 文档类型：产品需求文档（PRD）+ 实施设计，二合一。
>
> 状态：Phase 0 已按第 17 节顺序实施（M0-00…M0-11 基建完成；`plan_ready` 与 Main 模式首轮收窄已下线）。
> **证据门已废弃（2026-08-22，见 `docs/archive/implementation/settings-ux-and-image-understanding.md` A3）**：
> §14.1 的 manifest 嵌入、§16 的预注册发布门与 experiment 环境变量均已移除，相关
> 章节仅作历史设计记录保留。客户滑钮 `planning.suggest_complex_tasks` 是唯一开关，
> 打开即生效；`R_CODE_PLANNING_EMERGENCY_OFF=1` 急停是唯一宿主级兜底。
> `eval/plan-eval/` 降级为可选的事后质量回归工具，不再阻塞功能启用。
> 配置入口：**设置 → Agent 编排 → 复杂任务先建议制定计划**，卡片始终可见；
> 存在任一可用的 DeepSeek 服务即可操作开关（按任务实际 route 生效，无需设为默认）。
>
> 本文同时回答两类问题：客户会看到什么、为什么这样设计；工程团队如何实现、验证和安全发布。

## 0. 文档定位

本文分成两部分：

- **第一部分：产品需求（PRD）**，定义首发 Provider、目标客户、用户旅程、交互文案、频率预算、手册指引和产品验收指标；
- **第二部分：实施设计**，定义状态机、Provider 资格判断、持久化、幂等续接、Plan 目录、安全硬门、证据门和任务顺序。

PRD 是客户体验的权威。实施设计不能因为技术方便而增加弹窗、暴露内部术语或扩大 Provider 范围。

# 第一部分：产品需求（PRD）

## 1. 产品摘要

首发版本采用以下产品边界：

1. **整个新功能只对经过验证的 DeepSeek Provider 生效。**其他 Provider 不获得复杂度建议工具、不出现建议弹窗、不启用 Plan 双轨；后续 Provider 必须单独验证后再开放。
2. DeepSeek 主 Agent 在处理普通请求时，复用本轮模型判断请求是否复杂，不新增分类模型或后台请求。
3. 判断为复杂时，主 Agent 只能建议“先制定计划”，不能替客户切换模式。
4. 建议先持久化为待决 `PlanEntryOffer`，此时任务仍保持原 Agent 模式，也不会创建 Plan。
5. 客户接受后才原子创建 Plan、切换模式并续接原请求；客户拒绝后继续原 Agent 流程。
6. 客户手动选择 Plan 或明确要求先做结构化计划，视为已经同意，不再二次确认。
7. “轨迹锚定 + 执行硬门”双轨只在符合资格的 DeepSeek Plan 内运行。普通 Agent、其他 Provider、Codex 主 Agent 和所有子 Agent 都不启用。
8. 产品移除 `plan_ready` 软门。Plan 的唯一执行硬门是现有原生状态机：只读调查、发布计划、客户批准、实施。

产品向客户表达的是“这个任务适合先列计划”，不是“工具目录锚定”“双轨”“CAS”或“provider profile”。技术名词只出现在实施部分和诊断界面。

## 2. 背景与问题

当前首轮锚定原型是全局设置。如果把它直接产品化，会产生三个客户问题：

- **触发过于频繁。**简单任务也可能被迫经历规划步骤，破坏“说完就做”的直接感；
- **模式切换缺少客户控制。**模型直接进入 Plan，会让客户不知道为什么停下来、何时开始修改；
- **解释成本过高。**把工具清单、晋升时机和实验档位放在设置页，要求客户先理解实现细节才能做选择。

本方案把新能力限制在已经获得证据的 DeepSeek 路径，并只在复杂任务上提出一次通俗建议。客户只需回答“先计划还是直接继续”。深入解释放进可选手册，而不是塞进阻断弹窗。

## 3. 产品目标、非目标与指标

### 3.1 产品目标

- 复杂任务开始修改前，给客户一次确认范围和实施顺序的机会；
- 简单任务、非 DeepSeek Provider 和子 Agent 保持现有直接体验；
- 客户能在 10–15 秒内理解并完成选择，不需要知道 R-Code 内部工具和状态机；
- 客户拒绝后不再被同一任务反复打断，仍可随时手动选择 Plan；
- 接受前零副作用，接受后 Plan 仍需再次批准才进入实施；
- 通过内置、离线、可随时打开的手册解释 Plan，而不是强制新手教程。

### 3.2 非目标

- 首发不支持 OpenAI、Anthropic、Kimi、Ark、自定义兼容网关或其他 Provider 的自动建议和双轨；
- 不自动把客户切入 Plan，不增加“以后都自动进入”选项；
- 不把 `off | experiment | validated`、catalog 档位或 evidence version 暴露给普通客户；
- 不改变其他 Provider 已有的手动 Plan 能力；它们继续使用 baseline Plan；
- 不在首次设置流程里强制增加一页教学，也不自动弹出长手册；
- 不用多文件数量、提示词长度或模型自评作为唯一复杂度证据。

### 3.3 成功指标与发布硬门

| 指标 | 发布硬门 | 目标值 |
| --- | ---: | ---: |
| 非 DeepSeek 自动建议或双轨触发 | 0 | 0 |
| simple 请求阻断弹窗误触发率 | 不高于 10% | 不高于 5% |
| complex 请求建议召回率 | 至少 80% | 至少 90% |
| 同一真实请求重复弹窗率 | 0 | 0 |
| 同一 task branch 主动阻断弹窗数 | 最多 1 次 | 最多 1 次 |
| 接受前写入、Shell、变更型 MCP 或委派 | 0 | 0 |
| 弹窗键盘可完成率 | 100% | 100% |
| GuideSheet 关闭后的焦点正确归还率 | 100% | 100% |

路由指标只记录分类结果、Provider kind、用户决定和时延，不上传请求正文、模型 reason、文件内容或 API 凭据。

## 4. 首发范围：仅 DeepSeek Provider

### 4.1 资格判定

首发资格必须同时满足：

- 主运行时是 R-Code；
- 当前冻结 Provider 的稳定身份 `provider_kind == "deepseek"`；
- 当前 model、wire protocol 和 endpoint class 位于通过证据的 allowlist；
- 当前任务绑定 R-Code workspace；
- suggestion emergency off 未启用；
- 当前不是子 Agent、Codex 主 Agent 或临时 scope-decision Plan。

资格判断复用仓库现有的稳定 `provider_kind`，不能用可编辑的 Provider 名称、模型名或 URL 字符串猜测。`DeepSeek Relay` 之类的显示名和 `api.deepseek.com.example` 之类的地址不能冒充 DeepSeek。

自定义中转即使保存了 `provider_kind = "deepseek"`，也只有在 endpoint class 被证据 manifest 明确覆盖时才符合资格；否则保持 baseline。

### 4.2 非 DeepSeek 行为

非 DeepSeek Provider：

- Agent 请求中不注册、不展示 `propose_plan_mode`；
- Agent 尾部提示不注入复杂度建议策略；
- 不创建 `PlanEntryOffer`，不出现建议弹窗；
- 手动选择 Plan 或明确要求先计划时，继续进入现有 baseline Plan；
- 不使用 5→8 目录，不读取 DeepSeek evidence manifest 的启用结论。

### 4.3 Provider 切换

offer 保存创建时的 Provider snapshot。如果客户在决定前切换到非 DeepSeek 或不匹配的 DeepSeek route，pending offer 进入 `superseded_provider_changed`，弹窗关闭并显示一次非阻断提示：“模型服务已切换，这次建议已取消；你仍可手动选择 Plan。”

已经接受的 Plan 使用创建时冻结的 Provider/profile。普通 Settings 变更不能把该 Plan 重新解释成另一 Provider 的已验证能力。

### 4.4 后续开放原则

Provider allowlist 首发只有 `deepseek`。增加其他 Provider 必须作为独立发布事项完成：

1. 为该 Provider 冻结模型、协议、endpoint class 和 profile version；
2. 重跑能力、路由、安全、成本和交互证据；
3. 形成独立 manifest；
4. 增加 Provider-specific resolver 与回归测试；
5. 更新 PRD、GuideSheet 和发布说明。

不能仅通过 Settings 开关把 DeepSeek 的证据借给其他 Provider。

## 5. 客户旅程

| 场景 | 客户看到的行为 | 阻断弹窗 | Plan 双轨 |
| --- | --- | --- | --- |
| 非 DeepSeek Provider 的任意普通请求 | 保持现有 Agent 行为 | 否 | 否 |
| DeepSeek 的单点、可立即验证修改 | Agent 直接执行 | 否 | 否 |
| DeepSeek 的解释、状态检查或代码评审 | Agent 直接回答 | 否 | 否 |
| 客户明确说“直接做，不要 Plan” | Agent 直接执行 | 否 | 否 |
| DeepSeek 首次识别到复杂请求 | 询问是否先制定计划 | 是，本 branch 最多一次 | 尚未启用 |
| 客户选择“先制定计划” | 创建 Plan，续接原请求 | 已完成 | 是 |
| 客户选择“直接继续” | 恢复 Agent，当前 branch 进入安静状态 | 已完成 | 否 |
| 客户手动选择 Plan | 直接创建 Plan，不二次确认 | 否 | 仅符合资格的 DeepSeek 启用；其他 Provider baseline |
| 客户明确要求“先给计划” | 直接进入 Plan | 否 | 同上 |
| 任意子 Agent 或 Codex 主 Agent | 沿用既有边界 | 否 | 否 |

## 6. 交互与心智负担

### 6.1 决策弹窗

客户弹窗只保留一个原因、两个动作和一个低层级帮助入口。推荐文案：

```text
这个任务适合先列个计划

它涉及多个相互关联的改动。先制定计划可以让你确认范围和顺序，再开始修改。

[直接继续]                     [先制定计划]
Plan 模式会做什么？
```

具体要求：

- 主按钮使用“先制定计划”，不使用“进入 Plan 模式”作为唯一文案；
- 次按钮使用“直接继续”，不使用“拒绝”“跳过”或“仍要执行”；
- UI 不展示 `multi_subsystem` 等内部 signal、工具名、目录数量、模型分数或原始审计 reason；
- 客户文案由宿主按最主要 signal 映射到固定本地化模板，模型 reason 只进入脱敏审计；
- 最多显示两行正文，不展开技术原理；
- 补充一句低强调说明：“选择直接继续后，本任务不再主动弹出；你仍可随时手动选择 Plan。”

关闭按钮和 `Escape` 等价于“直接继续”，不能只隐藏弹窗并留下语义不明的 pending 状态。提交期间按钮进入 busy 状态，并复用同一个幂等键，避免双击产生两个决定。

### 6.2 频率预算

为避免连续复杂请求造成“每句话都被拦一次”的心智负担，采用双层抑制：

- request 级：同一个 `origin_request_key` 永远最多创建一个 offer；
- task-branch 级：同一个 branch 最多出现一次主动阻断弹窗。

客户拒绝、关闭或按 `Escape` 后，branch 进入持久 `quiet_after_decline`。同 branch 的后续复杂请求直接执行，不再弹窗。客户仍可通过模式控件手动选择 Plan；新建任务或显式 fork 到新 branch 后才恢复一次主动建议预算。

不增加第三个“这个任务不再提醒”按钮，也不增加倒计时、自动接受或连续 toast。安静策略由“直接继续”的辅助文案一次讲清。

### 6.3 内置指引手册

复用现有 `GuideSheet.tsx`，新增 `GuideId = "plan-suggestion"`，不另造教学框架。入口有三处：

- 决策弹窗中的“Plan 模式会做什么？”；
- DeepSeek 设置卡标题行的低层级“指引手册”按钮；
- Help 菜单中的“Plan 模式与复杂任务建议”。

GuideSheet 只解释客户需要知道的四件事：

1. 什么时候会建议先计划；
2. 进入后会先调查、再给计划，尚未修改文件；
3. 客户需要再次批准才开始实施，随时可以取消；
4. 首发只支持经过验证的 DeepSeek，其他 Provider 可手动使用普通 Plan。

不要把 catalog、bootstrap/resident、证据统计或工具 schema 放进客户手册。深入技术内容链接到维护者文档。

弹窗打开 GuideSheet 时不能叠两个 modal：先保留 offer 和决定表单状态，临时替换为 GuideSheet；关闭手册后恢复原弹窗，并把焦点归还到“Plan 模式会做什么？”链接。此处的 `Escape` 只关闭手册，不代表客户拒绝建议。

GuideSheet 沿用已有能力：离线内置、内容注册表、Portal、focus trap、初始焦点、Esc/背板关闭、焦点归还、窄窗布局、reduced-motion 和页脚动作。手册不自动打开，也不插入首次设置 campaign。

### 6.4 客户设置

普通客户在 **设置 → Agent 编排** 只看到一个 DeepSeek 专属开关：

```text
复杂任务先建议制定计划    [开/关]
仅在 DeepSeek 识别到复杂任务时询问；每个任务最多一次。
```

- 卡片在 Agent 编排设置中始终可见；当前默认 Provider 的稳定 `provider_kind`、证据状态和 route 覆盖只决定开关能否操作，不能让配置入口消失；
- 当前默认 Provider 非 DeepSeek 时开关禁用，并说明“切换到 DeepSeek 后可配置”；
- 证据状态、profile version、experiment 和 emergency off 留在诊断/开发者层；
- 证据未通过时客户开关不可启用，产品保持 baseline，并用一句话说明“功能仍在验证中”；证据已通过但当前 route 未覆盖时也保持禁用并说明原因；
- 切换开关只影响新 offer，不改变已经接受的 Plan；
- 旧 `first_round_*` 实验档位从客户设置中移除。

### 6.5 可访问性、恢复与状态反馈

- 对话框使用 `role="dialog"`、`aria-modal="true"`、可感知标题和说明；
- 初始焦点落在推荐主动作“先制定计划”，但 `Tab` 顺序必须先允许到达“直接继续”和帮助入口；
- 焦点被困在当前 modal，关闭后回到原触发位置；
- 非当前 task 只显示 `Needs You`，不能跨任务抢焦点；
- 重启后恢复同一 offer，不重新生成文案或重置选择；
- 决策保存失败时保留原选择，显示内联 retry，不弹第二个错误 modal；
- Provider 变化、offer 过期或任务归档使用非阻断状态条说明，不要求客户理解 revision/CAS。

## 7. PRD 验收标准

- 自动建议和 Plan 双轨只在证据匹配的 DeepSeek route 上运行；设置卡始终可见，Provider 或证据不满足时不可操作并解释原因；非 DeepSeek 自动触发数为 0；
- 客户弹窗不包含内部 signal、工具名、双轨或目录术语；
- 客户只做“直接继续 / 先制定计划”一个二选一决定；
- 同 request 最多一个 offer，同 task branch 最多一次主动阻断弹窗；
- 拒绝后 branch 持久安静，手动 Plan 入口仍可用；
- GuideSheet 可从弹窗、DeepSeek 设置和 Help 打开，且不与决策弹窗叠层；
- GuideSheet 与决策弹窗的键盘、焦点归还、窄窗和 reduced-motion 测试全绿；
- 接受前零副作用，接受后必须经发布计划和客户再次批准才实施；
- 其他 Provider 的现有手动 baseline Plan 不回归；
- 所有成功指标、隐私边界和 DeepSeek 证据门通过后才允许默认开启。

# 第二部分：实施设计

## 8. 目标状态机

```text
新 Agent Run
  |
  +-- 非 R-Code / 非 DeepSeek / route 未验证 ----------> 正常 Agent
  |                                                       （可手动进入 baseline Plan）
  |
  +-- DeepSeek eligible
        |
        +-- branch 已 quiet / simple / explicit no-plan --> 直接执行
        |
        +-- 用户已显式选择 Plan ------------------------> DeepSeek Plan 双轨
        |
        +-- complex + propose_plan_mode -----------------> PlanEntryOffer(pending)
                                                            |
                                                            +-- 拒绝/关闭/Escape
                                                            |     -> Agent
                                                            |     -> branch quiet
                                                            |
                                                            +-- Provider 已切换
                                                            |     -> superseded
                                                            |     -> Agent
                                                            |
                                                            +-- 接受
                                                                  -> 创建 Plan
                                                                  -> task.mode = plan
                                                                  -> DeepSeek Plan 双轨

DeepSeek Plan（原生只读规划）
  |
  +-- request_user_input -> 等待客户回答 -> 继续 Plan
  |
  +-- plan_publish ------> ready，仍然只读
                            |
                            +-- 客户拒绝/继续提问 -> Plan
                            |
                            +-- 客户批准 --------> Agent 实施
```

核心不变量：

- `PlanEntryOffer(pending)` 与 `Plan` 是不同状态；出现建议不等于已经进入 Plan；
- Provider 资格判断发生在注册工具和构建提示之前；非 DeepSeek 模型不能“碰巧”调用该功能；
- branch quiet 是交互频率预算，request key 是幂等与恢复边界，两者不能互相替代。

## 9. 复杂度判断合同

复杂度判断仍由 DeepSeek 主 Agent 在正常回复中完成，不增加独立分类请求。宿主不判断自然语言是否复杂，只负责资格解析、工具注册、结构化信号校验、调用时机和幂等去重。

在构建每次 Provider 请求前，宿主先运行同一个 `DeepSeekPlanEligibilityResolver`。只有同时满足以下条件，才向模型注册 `propose_plan_mode` 并注入一段简短的复杂度判断策略：

- 冻结 route 满足第 4.1 节的 DeepSeek 资格；
- 当前是 R-Code 主 Agent 的 Agent 模式，不是 Plan、Codex 或子 Agent；
- 客户开关已启用，内部 release state 允许，emergency off 未启用；
- 当前 branch 尚有一次建议预算，也未处于 `quiet_after_decline`；
- 当前 `origin_request_key` 没有已存在的 offer 或决定。

任一条件不满足时，工具和提示同时缺席。非 DeepSeek 不能依靠猜测工具名、历史 ToolUse 或兼容协议碰巧进入这条路径。

### 9.1 强触发信号

`propose_plan_mode` 至少提交一个受控信号：

| 信号 | 内部含义 | 宿主拥有的客户说明模板（示意） |
| --- | --- | --- |
| `multi_subsystem` | 变更跨越多个相互依赖的模块或子系统，需要先确定顺序与边界 | “它涉及多个相互关联的改动。” |
| `migration_or_data` | 涉及数据迁移、协议兼容、持久化格式或不可随意重放的状态 | “它涉及数据或兼容性变化，先确认步骤会更稳妥。” |
| `design_decision` | 存在需要用户批准的架构、产品或交互取舍 | “开始前有几项方案需要你确认。” |
| `expensive_rollback` | 错误尝试回滚成本高，或可能影响大量用户数据/工作区状态 | “如果直接修改，出错后的恢复成本会比较高。” |
| `multi_stage_verification` | 无法在一次局部修改中安全完成，需要分阶段验证和回退点 | “它需要分阶段完成和验证。” |

宿主按固定优先级只选一个本地化模板，拼接通用的“先制定计划可以让你确认范围和顺序，再开始修改”。模板和优先级随客户端版本发布，不能让模型自由生成阻断弹窗文案。

### 9.2 明确排除

以下条件本身不能触发建议：

- 仅仅修改了多个文件；
- 单个、隔离、可立即验证的修复；
- 解释、总结、只读检查或报告状态；
- 用户已经明确要求直接执行；
- 当前任务已经处于 Plan；
- 调用者是子 Agent、Codex 主 Agent 或非 R-Code 主运行时。

### 9.3 工具合同

目标工具为：

```text
propose_plan_mode({
  reason: string,       // 1..1000 字符，仅用于本地脱敏审计，不直接给客户看
  signals: Signal[]     // 1..5 个唯一受控枚举
})
```

模型不能传入 `task_id`、`run_id`、`request_key`、目标模式或运行 profile。这些字段全部由宿主从可信执行上下文绑定。

`reason` 不是产品文案。宿主只做长度限制、控制字符清理和支持包脱敏；普通 UI、通知、GuideSheet 和遥测都不能显示或上传它。客户看到的原因只来自上一节的固定模板，避免模型输出泄露文件名、客户数据或内部术语。

成功调用后，当前 Run 立即进入等待用户状态。同一模型响应中排在后面的编辑、Shell、委派或外部工具调用必须在进入 Gateway 前被 suspension gate 拒绝。

现有 `enter_plan_mode` 只保留给已经获得用户同意的路径，包括显式 Plan 选择。自动复杂度路由不得再调用它。

## 10. 真实用户请求身份

“同一请求只提示一次”不能依赖提示词，也不能假设每条消息都先进入 `queued_messages`。当前宿主同时存在空闲直发、排队发送、运行中 Steer 和 Steer 失败后回退队列等路径，因此必须在所有分支之前创建统一的宿主信封。

### 10.1 `OriginRequestEnvelope`

在 `agent_send_with_mode_and_attachments` 进入 `Auto`、`Queue`、`SendNow` 或 `Steer` 分支前，宿主生成：

```text
OriginRequestEnvelope {
  request_key: UUID,
  kind: direct | queued | steer | host_continuation,
  parent_request_key: UUID?,
  operation_id: UUID,
  created_at: timestamp
}
```

`request_key` 的生成与继承规则：

| 发送路径 | 规则 |
| --- | --- |
| 空闲 `Auto` | 进入 `start_run_locked_with_message` 前生成并持久化新键 |
| 空闲 `SendNow` | 与空闲 `Auto` 相同 |
| `Queue` | 队列行保存新键；领取后继续使用该键 |
| 运行中 `Steer` 接受 | 使用持久 steer operation ID 作为请求身份，并更新 runtime 当前请求键 |
| `Steer` 结束竞态后回退队列 | 复用原 steer 信封，不能生成第二个真实请求键 |
| 带附件直发 | 文本与附件共享同一个请求键 |
| 宿主 continuation | 继承触发它的原始请求键，并标记为 `host_continuation` |
| 下一条真实用户消息 | 总是生成新键；只有 branch 预算仍可用且未 quiet 时，才可能创建新 offer |

当前 request key 必须进入 `AgentRuntime` 的 start/steer 状态，并随 `ToolExecutionContext` 传给 `SessionToolHost`。`propose_plan_mode` 只能使用该宿主字段进行查重。

`request_key` 回答“这是不是同一次真实请求”，branch suggestion state 回答“还要不要再打断客户”。新消息拥有新键，但如果客户已经拒绝、关闭或按过 `Escape`，`quiet_after_decline` 仍优先阻止工具注册和后续阻断弹窗。只有新建任务或显式 fork 出新 branch 才重置一次建议预算。

## 11. `PlanEntryOffer` 持久状态

建议是独立聚合，存储在 SQLite。建议表至少包含：

```text
id
task_id
branch_id
source_run_id
request_key
original_mode
reason_audit
signals_json
primary_signal
customer_copy_key / customer_copy_version
provider_kind
provider_profile_id / provider_profile_version
provider_route_revision
model_id
wire_protocol
endpoint_class
eligibility_profile_version / evidence_version
resolved_plan_runtime_profile_json
revision
state                 // pending | accepted | declined | superseded_provider_changed | expired
decision              // accept | continue | close | escape
plan_id
continuation_state    // none | queued | dispatching | sent | failed
continuation_operation_id
error
created_at / updated_at / decided_at
```

Provider snapshot 只保存稳定身份、版本、协议和 endpoint class，不复制 API key、授权头或可包含凭据的完整 URL。`customer_copy_key` 来自宿主模板注册表；恢复时不再询问模型生成解释。

另建 branch 级持久状态，不能用前端内存布尔值代替：

```text
PlanSuggestionBranchState {
  task_id
  branch_id
  suggestion_budget_consumed_at
  quiet_after_decline
  quiet_reason          // decline | close | escape | null
  revision
  updated_at
}
```

数据库约束：

- 每个 task 最多一个 `pending` 建议；
- 每个 `(task_id, branch_id, request_key)` 最多一个建议；
- offer 插入与 branch 建议预算消耗在同一事务完成，确保同一 branch 最多产生一个阻断建议；
- 决策使用 `revision` CAS，过期 UI 不能覆盖较新的状态；
- 接受前重新比较当前 route 与冻结 Provider snapshot；不一致时 CAS 为 `superseded_provider_changed`，不得创建 Plan；
- 拒绝、关闭和 `Escape` 在决定事务中持久写入 `quiet_after_decline`；应用重启不能恢复建议预算；
- 所有状态都能从 SQLite 恢复，不依赖进程内布尔值。

创建 `pending` 建议时不得：

- 修改 `tasks.mode`；
- 创建 `plans` 行；
- 缩减 Agent 工具目录；
- 自动派发 Plan continuation。

## 12. 用户决定与可靠续接

### 12.1 接受事务

用户接受时，一笔 `IMMEDIATE` SQLite 事务完成：

1. 以 offer revision、task branch 和 `provider_route_revision` 校验当前 route 仍等于冻结 Provider snapshot；
2. CAS 将 offer 从 `pending` 改为 `accepted`；
3. 使用宿主已经解析好的冻结 profile 创建 draft Plan；
4. 将任务模式切换为 `plan`；
5. 创建确定性的 continuation operation ID；
6. 排入继承原 request key 的 Plan continuation；
7. 保存 `plan_id`、continuation 状态和决定幂等键。

任一步失败都回滚，不留下半个 Plan、错误模式或孤立队列行。

Plan 冻结的是非秘密 route 身份和 profile。执行时仍从同一个 Provider profile 读取当前凭据；profile 被删除、身份改变或 endpoint class 不再匹配时 fail closed，不把 Plan 静默改派给默认 Provider。

### 12.2 拒绝事务

用户拒绝时，一笔事务完成：

1. CAS 将 offer 改为 `declined`，并记录 `continue | close | escape` 的决定来源；
2. 将对应 branch 持久设为 `quiet_after_decline`；
3. 保持原 `tasks.mode`；
4. 创建继承原 request key 的 Agent continuation；
5. 在 continuation 上加入宿主抑制上下文；
6. 保存决定幂等键和 continuation 状态。

同 request key 的后续模型轮、重试和应用重启都不能再次创建建议。新用户请求使用新键，但仍受 branch 的持久 quiet 状态约束，不再主动弹窗；手动 Plan 入口不受影响。

### 12.3 Provider 变化、过期与恢复

Provider 设置保存、任务恢复和 accept IPC 都调用同一个 snapshot comparator：

- 只要 `provider_kind`、profile identity/version、model、protocol、endpoint class 或 route revision 不再匹配，pending offer 就 CAS 为 `superseded_provider_changed`；
- superseded 不创建 continuation Plan，也不改任务模式；若原 Run 仍在等待，则幂等续接 Agent；
- 当前任务显示一次非阻断状态条，后台任务只更新 `Needs You`，不能抢焦点；
- suggestion budget 已在 offer 创建时消耗，因此 Provider 来回切换不会再次弹阻断建议；客户仍可按当前 Provider 手动选择 Plan，由 resolver 决定使用 eligible DeepSeek 双轨或 baseline；
- offer 因任务归档、branch 删除或产品定义的 TTL 过期时进入 `expired`，采用同样的无副作用清理和 Agent 恢复合同。

恢复顺序必须先加载 offer、branch suggestion state 和 durable continuation，再决定是否展示对话框。不能先启动新的 Agent Run，再晚到地恢复 pending offer。

### 12.4 “恰好一次效果”协议

不能只在内存中 claim 队列项、启动 runtime 后再标记 `sent`。这个顺序在崩溃窗口内会重复续接。目标协议是：

1. 从 `(offer_id, decision_revision)` 派生稳定的 `continuation_operation_id`；
2. runtime 接受前，先幂等写入携带 operation ID 与 origin request key 的 durable user-message envelope；
3. runtime 以 operation ID 去重消费，同一个 operation 不追加第二条普通 `Message`；
4. runtime 明确接受后才将 continuation 标记为 `sent`；
5. 启动恢复先用 durable envelope 对账，再决定是标记已发送还是暴露失败重试；
6. 显式重试复用同一个 operation ID。

必须覆盖四个崩溃点：durable append 前、append 后、runtime 接受后、`sent` 确认前。

如果实现无法提供端到端 operation-id 幂等性，产品不得声称 exactly-once；技术合同必须降级为 at-least-once/manual-retry，客户界面则用通俗文案明确“续接可能重复，请检查后重试”。

### 12.5 决策弹窗与 GuideSheet 协调

Room 宿主持有一个表面状态，而不是让两个组件各自打开 Portal：

```text
PlanEntrySurfaceState = decision | guide
```

- 点击“Plan 模式会做什么？”只把表面从 `decision` 切到 `guide`；SQLite offer 仍是 `pending`，已选动作、revision、busy/error 草稿保留在宿主；
- `guide` 状态只渲染现有 `GuideSheet` 壳，决策弹窗不再带 `aria-modal`、不留在可访问性树中，因此页面上始终只有一个 modal；
- GuideSheet 的关闭、背板和 `Escape` 只把表面切回 `decision`，不能调用 decline IPC；
- 决策弹窗重新挂载后，宿主用明确的 return-focus token 聚焦“Plan 模式会做什么？”，不能只依赖已卸载 DOM 节点的通用 focus-return ref；
- 从 Settings 或 Help 打开的同一 guide 没有 offer 上下文，关闭时分别归还到原入口，不创建或改变任何 `PlanEntryOffer`。

## 13. Plan-only 双轨

用户进入 Plan 后，同时启用两条互补轨道。目录收敛只改善模型轨迹，不承担安全职责；执行硬门独立阻止副作用。

### 13.1 轨道 A：Plan 原生轨迹锚定

启用的 Plan 首轮使用稳定 bootstrap 目录，精确顺序为：

```text
glob
plan_publish
read_file
request_user_input
search_files
```

首次持久 assistant/tool outcome 后，Plan 进入 resident 目录：

```text
glob
plan_publish
read_file
request_user_input
search_files
git_status
list_files
load_skill
```

resident 只增加三个只读工具，不恢复完整目录。整个 Plan 期间不自动注入 memory、local clock、委派说明或 MCP 管理尾部；只保留原用户请求和权威 `PlanContextCapsule`。

该产品 profile 是 Plan 原生只读目录，不声称复刻可写的 canonical DSH `bash + str_replace_editor`。canonical schema 只能用于隔离评估对照，不能进入生产 Plan 权限面。

### 13.2 轨道 B：Plan 原生执行硬门

执行权威保持为现有状态机：

```text
ToolPolicy::Plan
  -> 只读调查 / request_user_input
  -> plan_publish
  -> Plan state = ready（仍只读）
  -> 用户批准 CAS
  -> 下一次实施 Run 才获得实施策略
```

Plan 模式在执行侧拒绝：

- `edit`、`apply_patch`、`create_file`、`delete_file`；
- Shell；
- mutation-capable hosted/external tools；
- 直接变更型 MCP 调用；
- `delegate_task` 和所有子 Agent 生命周期工具；
- `plan_item_update` 等实施期工具；
- 由历史消息诱导的隐藏调用。

工具不出现在目录里不是安全边界。Gateway、`SessionToolHost::scoped_input` 和 `call_inner` 都必须在副作用前执行相同的 Plan policy 检查。

生产代码删除 `plan_ready` schema、提示、拦截和“恢复完整目录”路径。模型自己的“计划好了”声明不能代替 `plan_publish` 和用户批准。

## 14. 每个 Plan 的冻结运行 profile

全局设置只参与创建新 Plan 时的解析。创建后，Plan 使用不可变 profile：

```text
ResolvedPlanRuntimeProfileV1 {
  enabled
  catalog_profile          // baseline | plan_native_v1
  context_profile          // default | minimal_v1
  profile_version
  evidence_version
  provider_kind
  model_id
  endpoint_class
  catalog_phase            // bootstrap | resident
}
```

### 14.1 权威层次

- `vendor/agent-contracts/.../agent-config` 只定义配置 schema 与枚举，不读取父仓证据文件；
- 宿主 `src-tauri/src/plan_policy.rs` 解析 `off | experiment | validated`；
- `src-tauri/build.rs` 校验并把匹配的证据 manifest 嵌入 `OUT_DIR`；
- `plans` 行保存冻结 profile、profile version 和权威 `catalog_phase`；
- Session timeline 的 anchor event 只是审计投影，不是状态权威。

### 14.2 所有 Plan 创建路径

以下路径必须显式接收宿主解析的 `ResolvedPlanRuntimeProfile`，存储层不能自己读取 Provider、Settings 或证据文件：

- UI `plan_create`；
- 用户已同意后的 `enter_plan_mode`；
- 接受 `PlanEntryOffer`；
- `request_scope_decision` 创建的临时 Plan。

前 3 条使用同一 resolver 快照。`request_scope_decision` 保持 baseline，避免把临时范围问题误当成用户选择了强化 Plan。

### 14.3 目录阶段持久化

`plans.catalog_phase` 是 task-level 权威。`PlanStore` 提供只允许匹配 plan/profile version 的 `bootstrap -> resident` CAS：

1. worker 产生首次 durable outcome；
2. 宿主等待 PlanStore CAS 成功并收到持久化确认；
3. 之后才允许发送下一次 Provider 请求；
4. timeline event 在成功后作为二级审计投影追加。

`task_clear_context`、fork、branch reload、runtime 重建、配置热更新和应用重启都不能让同一个 Plan 回退到 bootstrap。CAS/审计追加失败时 fail closed，不发送下一轮请求。

## 15. 配置与发布范围

客户偏好与发布控制必须拆开。普通客户只操作一个布尔偏好，Provider 资格、实验档位和证据版本由宿主控制，不能混在同一张设置卡里。

### 15.1 客户配置

客户配置合同：

```toml
[planning]
suggest_complex_tasks = true
```

它只对应 DeepSeek 设置卡里的“复杂任务先建议制定计划”开关：

- 关闭后不再注册 `propose_plan_mode`，只影响以后创建的 offer；
- 关闭不取消 pending/accepted 决定，也不禁用手动 Plan；
- 手动进入符合资格的 DeepSeek Plan 时，是否启用双轨仍由冻结 release profile 决定；
- 默认 Provider 不是符合资格的 DeepSeek 时不展示该卡，也不保存一个看似生效、实际无效的 Provider 开关；
- DeepSeek 身份成立但证据未通过时，卡片只显示“功能仍在验证中”，开关不可启用；
- 首次默认开启必须等第 3.3 节全部发布硬门通过，并经过分批 rollout；在此之前默认关闭。

### 15.2 内部发布控制

内部诊断/发布合同至少包含：

```text
PlanningReleaseControlV1 {
  provider_kind = "deepseek"
  release_state          // off | experiment | validated
  emergency_off
  eligibility_profile_version
  evidence_version
  allowed_models
  allowed_protocols
  allowed_endpoint_classes
}
```

解析顺序为：

1. `provider_kind != deepseek` 直接判定不符合资格，不再读取 DeepSeek 的 experiment/validated 结果；
2. emergency off 开启时，建议和双轨都关闭，Plan 退回 baseline，原生只读硬门保持；
3. model、protocol、endpoint class、profile 或 evidence version 任一不匹配时 fail closed；
4. `off` 只提供现有 baseline Plan；
5. `experiment` 仅供内部开发/评估环境的 allowlisted DeepSeek route 使用，不能从普通 Settings 选择，也不能对其他 Provider 生效；
6. `validated` 只有在证据 manifest 与冻结运行环境完全匹配时，才允许 DeepSeek 建议和 Plan 双轨；是否弹建议还要叠加客户开关与 branch 预算。

正常 Settings 变更只影响以后创建的 Plan。紧急关闭可以在请求时覆盖冻结 profile 的“启用”位，使既有 Plan 暂时退回 baseline 目录；Plan 的只读硬门仍然存在。

当前 `first_round_catalog` 与 `first_round_promote_on` 是未发布实验字段。迁移后从客户设置中移除，只作为 legacy 输入返回明确诊断警告，不得静默映射为新 Plan 语义，也不能继续复用旧 GuideSheet 文案解释新功能。

## 16. DeepSeek 证据门

自动 `validated` 必须经过真实 DeepSeek 证据，而不是只通过 MockProvider、提示风格检查或自评。

### 16.1 能力实验

冻结 25 个工程 case，每类 5 个：

- bug 修复；
- 多文件 feature；
- migration/data；
- performance；
- safety。

每个 case 运行三臂，共 75 次完整运行：

| Arm | 入口 | Plan 硬门 | Plan 轨迹锚定 |
| --- | --- | --- | --- |
| Direct Agent | Agent 直接执行 | 否 | 否 |
| Plan baseline | harness 模拟用户进入并批准 | 是 | 否 |
| Plan dual-track | harness 模拟用户进入并批准 | 是 | 是 |

三臂能力实验必须关闭自动复杂度建议，防止 Direct Agent 自己调用 `propose_plan_mode` 污染控制组。路由质量由独立 probe 测量。

### 16.2 每个 arm 完全隔离

每个 `(case_id, arm)` 从相同、已验证 hash 的只读 fixture 创建全新：

- workspace；
- SQLite 数据库；
- session 目录；
- runtime/provider session；
- request/run 标识。

不同 arm 不能共享被修改的工作区、数据库、历史、cache 或 runtime。随机化的是运行顺序，不是状态所有权。

### 16.3 路由实验

另冻结 40 个只读 probe：20 个 simple、20 个 complex。它们只测量：

- simple 误弹率；
- complex 建议召回率；
- 同 request 重弹率；
- 显式 Plan 与 explicit no-plan 路由。

路由 probe 不进入 75 次能力成功率统计。

### 16.4 Provider 来源与原始证据

非 dry-run 评估必须 fail closed，只有经过允许的原生 DeepSeek adapter、冻结 model/profile version 和批准 endpoint class 才能运行。Mock、synthetic、未知 Provider、代理成其他模型的兼容 endpoint 都不能生成发布证据。

保留一棵经过脱敏的 raw-results 树或不可变 artifact URI + digest。它必须包含：

- 75 条唯一 capability record；
- 40 条唯一 routing record；
- case/arm/request/run/operation ID；
- provider kind、resolved model、endpoint class；
- fixture、commit、config、profile、preregister hash；
- 时间戳、重试原因与次数；
- RequestHeader/audit hash；
- 测试结果、diff digest、未批准副作用；
- rounds、tokens、wall time 和 cost。

验证器必须拒绝缺失、重复、共享 arm 状态、非法重试、不可获取 raw artifact 或 Provider 来源不匹配的 manifest。

### 16.5 预注册发布门

真实运行前冻结指标和失败规则。建议主门：

- dual-track 相对 Plan baseline 净多解至少 4 个；
- 回退不超过 1 个；
- 单侧 exact McNemar `p <= 0.10`；
- 未批准副作用为 0；
- simple 误弹不超过 10%；
- complex 建议率至少 80%；
- 同 request 重弹为 0；
- dual median tokens 不超过 baseline 的 1.20 倍；
- dual p95 wall time 不超过 baseline 的 1.30 倍。

`score.mjs` 只消费 raw results 与 preregistration 自动生成 manifest。独立 claim verification 再重算每个数字并检查缺失、离群、重试和 arm 污染。

任一门失败时：

- `validated` 解析为关闭；
- 客户产品保持 baseline；内部环境只保留 DeepSeek `off`/受控 `experiment`；
- M1–M6 不解锁；
- 修改 profile 后必须以新版本重跑完整证据，不能挑选 case 补跑后覆盖原结论。

### 16.6 DeepSeek 限定

即使证据通过，自动 `validated` 也只在以下条件同时成立时启用：

- Provider 是证据支持的原生 DeepSeek 路由；
- model、profile version、endpoint class 与 manifest 一致；
- 当前是绑定 R-Code workspace 的 Plan；
- emergency off 未启用。

自定义中转只有在 `provider_kind = deepseek` 且 endpoint class 被本 manifest 明确覆盖时才可能通过；显示名、模型前缀或相似 URL 都不是证据。

其他 Provider 无条件使用 baseline Plan。即使内部 release state 是 `experiment`，也不能绕过 `provider_kind` 和 Provider-specific manifest 检查。未来开放新 Provider 时必须新建独立 manifest、resolver 规则、交互回归和发布记录，不能借用 DeepSeek 的 experiment 或 validated 标记。

## 17. 实施顺序

该能力是原优化路线的阻塞 Phase 0。Phase 0 未通过证据门，不开始发布可信度、数据安全、性能、UI 和治理阶段。

| ID | 任务 | 核心出口 |
| --- | --- | --- |
| M0-00 | 修复当前 Rust 编译/Clippy 基线 | 后续 M0 Rust 测试和评估 binary 可以真实构建 |
| M0-01 | 固定 Agent/Plan/subagent/Provider characterization | 非 DeepSeek 自动触发为 0；其他 Provider 手动 baseline Plan、普通 Agent 目录和子 Agent 排除有回归测试 |
| M0-02 | 实现 `DeepSeekPlanEligibilityResolver` | 只认稳定 `provider_kind` 与证据匹配的 model/protocol/endpoint；其他 Provider fail closed |
| M0-03 | 建立 `PlanEntryOffer`、Provider snapshot 与 SQLite 约束 | pending 不切 mode；task/request 唯一；非秘密 route 冻结；重启可恢复 |
| M0-04 | 接通 `OriginRequestEnvelope` 与 branch suggestion state | direct/queue/steer/attachment/continuation 全覆盖；一次预算、拒绝后 quiet 与 Provider supersede 可持久恢复 |
| M0-05 | 按资格注册 `propose_plan_mode` 并改写路由提示 | 仅 eligible DeepSeek 看见工具；reason 只审计；客户文案由 signal 模板生成 |
| M0-06 | 建立决定事务、operation-id 幂等续接和启动恢复 | 接受/拒绝/Provider 变化原子；四个崩溃窗口不重复产生用户消息或 Run |
| M0-07 | 实现低负担弹窗、`Needs You` 与 Plan GuideSheet | 两动作、无内部术语、不叠 modal；三处手册入口、焦点归还、窄窗、reduced-motion、retry 全覆盖 |
| M0-08 | 删除 `plan_ready`，固化原生硬门 | 隐藏 edit/Shell/MCP/delegation 在副作用前拒绝 |
| M0-09 | 持久化冻结 DeepSeek Plan profile 与 catalog phase | 所有创建路径有 profile；Provider 变化不改写既有 Plan；`task_clear_context` 不重武装 |
| M0-10 | 实现 5→8 Plan 目录、最小上下文与简化客户设置 | 仅 DeepSeek 显示一个建议开关；内部档位留在诊断；普通 Agent、其他 Provider 和 subagent 不受影响 |
| M0-11a | 建立真实 DeepSeek headless 三臂评估器 | eval-only 自动 accept/approve；非 DeepSeek fail closed |
| M0-11b | 冻结 corpus schema、preregistration 和 raw-result 合同 | 运行前锁定协议、阈值、hash 与证据字段 |
| M0-11c1..c5 | 按五类分别构造 25 个自包含 case | 每类独立 PR；初始测试红、oracle patch 绿 |
| M0-11r | 冻结 20+20 路由 probe | 与能力实验完全分离 |
| M0-11d | 验证并冻结完整 corpus | 数量、分层、hash、allowlist 和 oracle 校验全绿 |
| M0-12 | 运行真实评估并落发布证据门 | raw→score→独立重算一致；通过后仅 DeepSeek Plan 启用 |

M0-00 必须位于所有 Rust M0 任务之前。证据 gate 之后仍保留正式发布任务，重新执行完整 workspace test、Clippy、fmt、前端测试和 build；不能把 M0 的局部通过当作最终发布通过。

## 18. 验证矩阵

| 风险面 | 必须覆盖的测试 |
| --- | --- |
| Provider 资格 | eligible DeepSeek 各批准 route；改名/相似 URL 不冒充；自定义 relay 未覆盖时 fail closed；非 DeepSeek 的 tool、prompt、offer、弹窗和双轨触发数全部为 0 |
| 非 DeepSeek 基线 | OpenAI/Anthropic/Kimi/自定义 Provider 手动选择 Plan 仍可用，使用 baseline 目录和现有原生硬门 |
| 路由 | DeepSeek simple、complex、explicit Plan、explicit no-plan、客户开关关闭、internal off、Codex、subagent |
| 请求身份 | idle Auto、idle SendNow、active Queue、accepted Steer、Steer fallback、attachments、host continuation、新用户请求 |
| branch 频率 | 同 request 最多一个 offer；同 branch 最多一个阻断弹窗；decline/close/Escape 后新 request 仍 quiet；重启后 quiet 保持；新 task/显式 fork 才恢复预算 |
| Offer | 同 task pending 唯一、stale revision、双窗口竞态、跨 task 决定、客户 copy key 固定、重启恢复且不重新询问模型 |
| Provider 切换 | pending 时切到非 DeepSeek、另一个 DeepSeek route、删除 profile、修改协议；均转 `superseded_provider_changed`、零 Plan 副作用、幂等恢复 Agent |
| 决定事务 | accept/decline 每个写点故障注入；失败时 Plan/mode/queue/offer 全回滚 |
| 续接 | durable append 前后、runtime accept 后、sent ack 前崩溃；retry 复用 operation ID |
| 客户弹窗 | 只出现“直接继续 / 先制定计划”；客户 UI 不含 signal、reason、tool/catalog/profile/evidence/CAS/双轨等内部词；关闭和 Escape 等价于继续 |
| GuideSheet | 从决策弹窗、DeepSeek 设置和 Help 打开；替换而非叠加 modal；关闭后恢复原决定与焦点；Guide 内 Escape 不拒绝 offer；focus trap、窄窗、reduced-motion 全覆盖 |
| 客户设置 | Agent 编排中始终显示一个建议开关；默认 Provider 非 DeepSeek、证据未通过或 route 未覆盖时不可启用但卡片不消失；切换只影响新 offer |
| UI 恢复 | 双击、IPC 失败、内联 retry、重载、非当前 task 不抢焦点、键盘全流程与 `Needs You` 投影 |
| Plan 硬门 | 历史 ToolUse、直接 `call_inner`、外部 host 三条路径尝试 edit/Shell/MCP/delegate |
| Plan 发布 | 坏 DAG、空叶项、ready 未批准、批准 CAS、取消、提问、发布失败 |
| Profile | UI create、显式 enter、offer accept、scope decision；非 DeepSeek/旧 Plan baseline；DeepSeek profile 不可变；凭据不进入 snapshot |
| Catalog phase | 正常晋升、重复 CAS、CAS/append 失败、clear context、fork、branch reload、runtime rebuild、restart |
| Catalog | 仅 eligible DeepSeek Plan 的名称、顺序和 schema hash；bootstrap=5、resident=8；其他 Provider baseline；隐藏调用仍被硬拒 |
| 能力证据 | 25×3 唯一记录、每 arm 独立环境、同配置预算、确定测试、diff、安全、成本、时延 |
| 路由证据 | 20+20 标签冻结、误弹/召回/重弹、与能力 arm 隔离 |
| 发布解析 | mock/未知 endpoint/模型不符/raw 缺失/共享状态/重复记录全部 fail closed；非 DeepSeek 即使 internal experiment 也不解锁 |

## 19. 回滚与迁移

- 先发布持久化 schema、DeepSeek eligibility resolver 和 baseline profile，再启用 UI 建议，最后才允许内部 DeepSeek experiment/validated 双轨。
- 客户关闭 `suggest_complex_tasks` 或内部切到 `off` 后停止创建新建议；已经 pending 且 Provider snapshot 仍有效的建议允许完成决定，不能静默丢弃。
- emergency off 立即停止新建议，并让双轨退回 baseline Plan；它不会放宽 Plan 的只读硬门。
- 非 DeepSeek 在所有 rollout 阶段都保持 baseline，不能作为灰度 cohort 意外进入 experiment。
- 旧 `first_round_*` 值只告警，不自动迁移；旧手册入口随旧设置一起下线，新 `plan-suggestion` 指引独立注册。
- 旧 Plan 和缺少 profile 的 Plan 一律解析为 baseline。
- branch suggestion state 随 task/branch 持久化；升级不能把已拒绝 branch 的预算重置为可再次弹窗。
- 证据失败时不提交 validated 默认，不解锁后续阶段。
- profile 或证据版本变化时，既有 Plan 保持自己的冻结记录；新版本重新跑完整证据。
- 数据库迁移保持向前兼容；回滚 binary 不能误读新 profile 为已启用。

## 20. 取舍

### 收益

- 普通请求不再频繁触发双轨，首轮目录实验不再全局影响 Agent。
- 用户对是否进入 Plan 有明确最终决定权。
- 客户只处理一个二选一决定；需要解释时再打开内置手册，不必先理解内部状态机。
- DeepSeek-only 首发把证据、回滚和支持边界限定在可验证范围内，不把单一 Provider 结论外推。
- 轨迹优化和权限安全解耦，目录错误不会变成写入漏洞。
- request-key、CAS 和 operation ID 让拒绝抑制与崩溃恢复可证明。
- validated 与真实 Provider/model/profile 绑定，避免把某一模型的结果泛化到所有 Provider。

### 成本

- 多出一个持久 offer 聚合、UI 状态和 continuation 对账协议。
- 主 Agent 可能漏判复杂请求；显式 Plan 入口仍是用户的可靠兜底。
- 主 Agent 可能误判并弹窗；simple 路由门和一次性抑制限制了打扰。
- 一个 branch 只主动询问一次，可能错过后续真正复杂的新请求；手动 Plan 是刻意保留的低成本兜底。
- DeepSeek-only 会让其他 Provider 暂时得不到自动建议和双轨收益；换来的是可解释的首发质量与更小支持面。
- GuideSheet 需要与客户文案、Help 入口和可访问性测试一起维护，不能只更新技术合同。
- Plan 的 5→8 工具目录比完整目录能力更窄，某些调查需要通过 `request_user_input` 或回到 baseline profile。
- 真实三臂评估成本高，但这是自动默认启用所需的证据成本，而不是每次请求的运行成本。

## 21. 与当前代码的对应关系

| 当前代码 | 当前职责 | 本提案中的变化 |
| --- | --- | --- |
| `crates/r-code-agent-worker/src/llm_runtime.rs` | Agent/Plan 提示、ToolPolicy、首轮目录与 `plan_ready` 原型 | 自动路径改用 propose；删除产品 `plan_ready`；只有 eligibility 已通过的 DeepSeek Run 注册建议工具或过滤 Plan 目录 |
| `src-tauri/src/plan_tools.rs` | `enter_plan_mode`、Plan 生命周期工具 | 增加 `propose_plan_mode`；只接受宿主可信上下文；显式 enter 接收宿主 profile；子 Agent 执行侧拒绝 |
| `crates/r-code-store/src/plan_store.rs` | Plan 状态机、问题、批准和 continuation | 接收 frozen profile；新增 catalog phase CAS；与 offer、branch quiet 和决定事务协作 |
| `src-tauri/src/commands.rs` | 发送分支、runtime、队列、Provider 身份、Plan IPC | 在所有发送分支前创建 request envelope；复用稳定 `provider_kind`；实现 snapshot 比对、offer IPC 与幂等续接 |
| `src-tauri/src/provider_catalog.rs` | 官方 DeepSeek preset、协议与 endpoint 候选 | 为 resolver 提供已知 route 元数据；最终资格仍由 evidence manifest 决定，不能只看显示名或 URL |
| `src-tauri/src/plan_policy.rs`（新增） | 无 | 集中实现 DeepSeek eligibility、内部 release control、客户偏好和 frozen profile 解析 |
| `crates/r-code-gateway/src/gateway.rs` | 宿主可信 ToolExecutionContext 和执行边界 | 上下文携带 origin request key；Plan 隐藏调用统一 fail closed |
| `vendor/agent-contracts/crates/agent-config/src/lib.rs` | 配置 schema | 新增 planning 枚举；不读取父仓 evidence manifest |
| `src-tauri/frontend/src/components/plan/PlanPanel.tsx` | Plan 问答、发布、批准与恢复 | 保持 Plan 内工作流，不承担进入 Plan 前的 pending offer |
| `src-tauri/frontend/src/components/room/Canvas.tsx` | 当前 Room 编排 | 挂载 PlanEntryDialog、GuideSheet 替换态和 retry；非当前任务只投影 Needs You |
| `src-tauri/frontend/src/components/settings/GuideSheet.tsx` | 离线 guide registry、Portal、focus trap、Escape 与焦点归还 | 新增 `GuideId = "plan-suggestion"` 和客户向内容；复用壳层，不复用旧实验术语 |
| `src-tauri/frontend/src/components/scenes/SettingsScene.tsx` | Provider 设置与 `openGuide` 宿主状态 | eligible DeepSeek 卡只放一个客户开关和低层级手册入口；内部档位不进入普通表单 |
| `src-tauri/frontend/src/components/shell/MenuBar.tsx` | Help 菜单与首次设置入口 | 新增“Plan 模式与复杂任务建议”，打开同一 guide，不插入强制 onboarding |
| `src-tauri/frontend/src/styles/scenes/misc.css` | GuideSheet、窄窗与 reduced-motion 样式 | 复用现有可访问壳层，只增加 plan guide 内容所需的最小样式 |
| `src-tauri/frontend/scripts/app-shell.test.mjs` | 设置和 GuideSheet 浏览器回归 | 增加三入口、modal 替换、offer 状态保持、焦点归还、客户术语和 Provider 可见性测试 |

现有 `request_scope_decision` 的持久问题和 continuation 机制可以复用设计经验，但不能原样复用：它会先把任务切到 Plan，而 Plan 入口建议必须在用户接受前保持 Agent 模式。

## 22. 完成定义

只有同时满足以下条件，Phase 0 才算完成：

- 非 DeepSeek 的建议工具注册、提示注入、offer、阻断弹窗和双轨触发全部为 0，手动 baseline Plan 仍正常；
- DeepSeek 资格只由稳定 `provider_kind` 与证据匹配的 model/protocol/endpoint/profile 决定，名称和相似 URL 不能冒充；
- 同一真实用户请求最多一个可恢复 offer，同一 task branch 最多一个主动阻断弹窗；
- decline/close/Escape 后 branch 持久 quiet，应用重启和新 request key 都不会再次打扰；新 task/显式 fork 才恢复预算；
- pending 建议不修改任务模式、不创建 Plan；Provider 变化会 supersede，零 Plan 副作用；
- accept/decline/supersede 原子、幂等，所有发送路径都有稳定 request key 和冻结的非秘密 Provider snapshot；
- continuation 在声明 exactly-once 前通过 operation-id 崩溃窗口测试；
- 显式 Plan 不重复确认，explicit no-plan 不弹窗；
- 客户弹窗只有一个通俗原因和两个动作，不显示 reason、signal、工具、catalog、profile、证据或“双轨”等内部词；
- 普通设置只给 eligible DeepSeek 一个建议开关，`off | experiment | validated` 与证据详情仅存在于内部诊断/发布控制；
- `GuideId = "plan-suggestion"` 可从弹窗、DeepSeek 设置和 Help 打开；不叠 modal，关闭后保留决定状态并正确归还焦点；
- 普通 Agent、其他 Provider、Codex 主 Agent 和子 Agent 的目录与执行语义不受 dual-track 配置影响；
- 产品不再包含 `plan_ready`；Plan 变更只能经 `plan_publish` 与用户批准解锁；
- eligible DeepSeek Plan 精确使用 5→8 目录，profile 和 phase 均可跨 clear/restart 重建；
- 75 个能力结果和 40 个路由结果完整、隔离且可从 raw artifact 独立重算；
- validated 只对证据匹配的 DeepSeek workspace Plan 生效；
- 任一证据门失败时默认保持关闭，并阻塞原 M1–M6 路线。

## 相关文档

- [Plan 模式、人工确认与增强审核](../../guides/plan-mode.md)
- [请求构成审计与首轮锚定实验](./request-audit-and-anchoring.md)
- [架构与实现细节](../../../architecture.md)

---

## 实施状态修订（2026-08，docs/archive/implementation/multimodal-attachments-and-deepseek-plan-anchoring-implementation.md 生效后）

新增独立客户滑钮 `planning.deepseek_plan_anchoring`（默认关闭）：

- 与 `suggest_complex_tasks` 互不替代：建议开关只控制是否注册 propose_plan_mode；锚定开关控制实际进入 DeepSeek Plan 后是否启用 5→8 最小只读目录 + PlanMinimal 上下文注入，以及批准实施后的完整能力恢复（`RestoredFull` 事件 + worker 侧 fail-closed 断言 `PLAN_FULL_CATALOG_NOT_RESTORED`）。
- 开关值、Provider route（kind/model/protocol/route revision）在 Plan 创建时冻结进 `ResolvedPlanRuntimeProfile`（profile_version=2）；运行中切换设置只影响之后新建的 Plan。
- `R_CODE_PLANNING_EMERGENCY_OFF=1` 同时关闭建议与锚定，Plan 只读安全硬门保持。
- 上下文注入统一经 `ContextInjectionProfile` 闸门（Standard / PlanMinimalV1）：PlanMinimal 从固定最小模板正向构造 system，禁止 memory、本地时钟、普通 task context、用户协作文案、MCP 文案、peer mailbox（保持 pending 不消费）、Plan 建议尾部、工具进度 checkpoint、委派提示、hosted web fallback 与 governor 尾部。
- 关闭开关时与 baseline Plan 请求形状一致（目录与注入均不受影响）。
