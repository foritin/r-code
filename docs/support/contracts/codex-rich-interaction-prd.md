# Codex 主代理丰富交互与 App Server 事件投影 PRD / AI 实施清单

> 文档状态：`frozen`（只表示执行合同已完整，不表示产品功能已经实现）<br>
> 执行合同：`prd-to-ai-worklist` v1.1.0<br>
> 协议基线：本机 `codex-cli 0.145.0` 生成 schema + OpenAI Codex App Server 官方文档<br>
> 固化清单：[`codex-rich-interaction-freeze.yaml`](./codex-rich-interaction-freeze.yaml)<br>
> 唯一完成状态：本文 §9 主 Checklist；任务卡、任务包与证据不得维护第二套 Checkbox

## 执行导航

- 首次执行：§0 → §2 → §4 → §8 → §9 → §10 的首个 ready 任务。
- 中断恢复：`artifacts/ai-tasks/current.yaml` → §10 对应任务卡 → `artifacts/ai-tasks/evidence/codex-rich-interaction/`。
- 判断完成：§8 统一 Harness → §10 断言 → `artifacts/ai-tasks/verification/codex-rich-interaction/`。
- 产品终态与非目标：§1。
- 不可变决策：§2。
- 协议、状态机与持久化：§4。
- UI 行为：§5。
- 需求追踪：§7。

## 0. AI 执行入口

<!-- AI_WORKLIST_VOLATILE_START -->

- 当前进度：`12 / 12` 项完成（implementation_verified）。
- 下一执行项：无——全部实施任务完成；剩余为 production profile 外部放行。
- 当前任务包：`artifacts/ai-tasks/current.yaml`（M4-02 已通过；总门禁报告 `artifacts/ai-tasks/verification/codex-rich-interaction/implementation/M4.json`：38/38 全绿）。

<!-- AI_WORKLIST_VOLATILE_END -->

### 0.1 首次启动

1. 只读检查 Git revision、完整 worktree、Codex CLI 版本、Rust/Node 运行时和现有测试基线；已有未提交改动一律视为用户资产。
2. 读取本节、§2、§4、§8、§9 和首个 ready 任务卡，不需要每轮重读全文。
3. 从编号最小且依赖已通过的未完成 MUST 任务开始；建立 `current.yaml` 后直接进入实现，不在里程碑边界等待人工确认。
4. 每个可验证子步更新任务包；断言和累计门禁均通过、证据真实存在后，才能勾选 §9 中唯一 Checkbox。

### 0.2 续跑

1. 读取 `current.yaml`、对应任务卡和已归档证据。
2. 核对 `changed_paths` 与真实 worktree；对已完成断言运行最小 smoke。
3. 从首个未完成 step 或 assertion 继续，不重复创建事件合同、fixture、组件或第二套 Harness。
4. 若任务包与代码不一致，以代码、测试和可访问证据为准修正任务包，不能凭状态文件宣称完成。

### 0.3 授权与中断边界

- 允许：仓库内可逆的源码、测试、文档、fixture 与验证脚本修改；复用现有 Rust/React/AgentEvent/Plan 问题 UI 约定。
- 不允许：提交、推送、发布、改写用户全局 Codex 配置、显示私有思维链、放宽现有权限边界、删除用户改动或真实凭据。
- 只有扩大权限/范围、需要不可获得的真实凭据、执行不可逆生产动作，或两条同优先级要求会改变产品语义且无法由事实消解时，才请求用户。
- Windows 与 macOS 是同一功能合同；平台差异只能留在进程启动、路径、终端和系统集成 adapter 中。

<!-- AI_WORKLIST_NORMATIVE_START -->

## 1. 背景、目标、终态与非目标

### 1.1 已确认问题

Codex App Server 已公开提供丰富客户端所需的事件与反向请求，但 R-Code 当前只投影其中一部分：

- `item/agentMessage/delta` 被识别为协议进度，却没有按 `commentary` / `final_answer` 投影到时间线。
- App Server 观察器主要处理 `item/started`、`item/completed`，计划、diff、上下文压缩、warning 与多种 delta 没有完整进入 UI。
- Codex 主代理提示只要求工具活动可观察，没有复用原生 R-Code Agent 的阶段性公开播报规则。
- `item/tool/requestUserInput` 虽被识别并做作用域校验，反向请求处理器仍返回 JSON-RPC `-32601`。
- reasoning summary 与公开 commentary 混淆，导致用户看到低信息摘要，却看不到真正有用的阶段进展。

仓库证据见 §3；协议事实来自 Codex App Server 官方 Events / Approvals 文档和 `codex app-server generate-json-schema` 生成物。

### 1.2 规范性需求

- **R-COM-01（MUST）**：按 `threadId + turnId + itemId` 流式投影公开 `agentMessage`，保留 `phase=commentary|final_answer`，完成事件只能封口，不能产生重复文本。
- **R-COM-02（MUST）**：Codex 主代理采用与原生 R-Code 一致的低噪声进度合同：首次工具批次前或实质阶段变化时播报；没有新发现时不复读工具动作。
- **R-ACT-01（MUST）**：命令、文件修改、MCP、动态工具、协作工具、Web 搜索、图片查看等生命周期以结构化卡片展示，并支持有界输出增量。
- **R-ACT-02（MUST）**：计划更新、聚合 diff、上下文压缩、warning、错误与 token usage 使用明确的非聊天事件呈现；未知事件不得伪装成回答。
- **R-HITL-01（MUST）**：支持 `item/tool/requestUserInput` 的反向 JSON-RPC 请求，在同一 turn 暂停、收集答案、精确回应原 request id 后继续。
- **R-HITL-02（MUST）**：问题 UI 支持选项、自由输入、`isOther`、`isSecret`、提交中、已回答、取消、超时和过期状态，并满足键盘与读屏可用性。
- **R-HITL-03（MUST）**：模型主动提问与用户主动 `turn/steer` 是两条独立链路；取消、turn 完成/中断、连接失效与应用重启必须 fail-closed，不能把过期回答发送给新请求。
- **R-RSN-01（MUST）**：只显示协议公开的 reasoning summary；原始 reasoning `content` / `textDelta`、secret answer 与敏感工具输出不得进入普通时间线或诊断包。
- **R-REL-01（MUST）**：实时与历史重建使用同一归一化合同，保证时序、幂等、去重、运行作用域隔离和最终状态一致。
- **R-REL-02（MUST）**：delta、命令输出和反向请求有内存/队列/文本上限；慢 UI、断流、迟到帧和重复完成帧不得拖死或污染其他 run。
- **R-COMP-01（MUST）**：按已安装 Codex CLI 的能力与 schema 版本降级；不支持的事件给出可诊断降级，Windows/macOS 行为等价。
- **R-UX-01（SHOULD）**：交互密度接近 Codex App 的信息层次，但复用 R-Code 视觉系统，不追求像素复制；长任务保持可扫描且不会生成“一字符一行”。
- **R-OBS-01（SHOULD）**：诊断记录事件名、作用域、状态迁移、降级原因和计数，不记录私密正文；支持定位丢帧、重复帧、超时与未知协议。

### 1.3 Definition of Done

`implementation_verified` 仅在以下状态全部成立时达成：

1. 确定性 App Server fixture 能重放 commentary、final、工具、计划、diff、压缩、warning、usage 和 requestUserInput 正反路径。
2. 同一 `agentMessage` 的 delta 与 completed 合并为一条内容正确的时间线项；重放、重连和历史加载不重复。
3. 模型提问可在同一 turn 收到回答后继续；取消、超时、过期、turn 结束和连接失效均有可观察终态。
4. secret answer 不写入消息 JSONL、SQLite 普通事件、日志、支持包或 UI 历史正文。
5. 原始思维链持续不可见；公开 commentary、reasoning summary 与 final answer 在数据和 UI 上互不冒充。
6. 统一 Harness `--through M4 --profile implementation` 退出码为 0，required assertion 无缺失，并产生机器可读证据索引。
7. 前端构建、相关 Rust/Node 测试、Windows fixture E2E 与 macOS CI/真机候选验证路径通过。
8. 当前维护文档与实现一致，旧的“requestUserInput 不支持”降级只在真实能力缺失时出现。

`production_release_ready` 还需要目标发布候选上的真实 Codex 登录、受支持 CLI 版本、Windows 与 macOS 安装包冒烟；这些外部条件不得阻止离线 fixture 把实现推进到 `implementation_verified`。

### 1.4 非目标

- 不展示或推断私有 chain-of-thought。
- 不像素复制 Codex App，不引入第二套设计系统。
- 不把每个 token、stdout 字节或常规继续动作渲染成独立卡片。
- 不改变 Codex 权限审批语义、R-Code Plan 产品语义、子代理并发上限或 Provider 计费策略。
- 不把 requestUserInput 伪装成新 turn，也不把 steer 伪装成模型提问答案。
- 不承诺旧版 Codex CLI 具备新协议；缺失能力时必须明确降级。

## 2. 已冻结决策

1. **主路径**：Codex 主代理继续使用 App Server；`codex exec --json` 仅保留现有适用的降级/子代理用途，不为丰富 UI 再造一套解析协议。
2. **统一归一化层**：原始 JSON-RPC 先转换成宿主拥有的版本化事件，再进入持久化与前端；React 不直接理解任意 App Server JSON。
3. **稳定身份**：消息与工具项以 `threadId + turnId + itemId` 为身份，server request 额外绑定 transport generation + request id；单独使用 request id 不足以跨连接识别。
4. **消息阶段**：`commentary` 是公开阶段更新，`final_answer` 是正式交付；缺少 phase 的兼容事件按所在 turn 和完成边界保守归类，并记录降级原因。
5. **增量策略**：同一 item 原位追加，按帧/短时间窗合并刷新；`item/completed` 是权威终态，但不得再次追加已消费全文。
6. **公开 reasoning**：只接受 `summary` / `summaryTextDelta`；raw `content`、`textDelta` 和 legacy raw text 继续丢弃。
7. **用户提问协议**：实现 Codex 0.145.0 schema 的 `item/tool/requestUserInput`。问题包含 `id/header/question/options?/isOther/isSecret`，请求包含 `threadId/turnId/itemId/questions/autoResolutionMs?`；响应为 `{answers: {<questionId>: {answers: string[]}}}`。
8. **secret 处理**：secret 值只在前端受控输入、内存中的 pending request 与一次 JSON-RPC 响应中存在；日志、诊断、普通历史和证据只能记录“已回答/长度/字段存在”之类不可逆元数据。
9. **问题 UI 复用**：优先复用现有 Plan 问题组件的视觉与可访问性原语，但 pending Codex request 不写入 Plan 状态机，也不要求切换 Plan 模式。
10. **生命周期**：turn 完成/中断、`serverRequest/resolved`、transport generation 变化或应用重启会使未决请求终止；过期 UI 只读，不能再次提交。
11. **steer 分离**：运行中用户主动补充仍走 `turn/steer`；只有与 pending request 精确匹配的提交才编码为 requestUserInput response。
12. **跨平台**：事件归一化、持久化、UI 和测试 fixture 共用；平台特定代码只负责 CLI 路径、进程、窗口和系统安全存储。
13. **可观察性**：未知事件可以计数和诊断，但不得静默转换为用户消息；敏感 payload 先脱敏再截断。

## 3. 仓库事实表

| 事实 | 当前落点 | 对计划的约束 |
| --- | --- | --- |
| App Server 传输、注册表和已识别进度事件 | `src-tauri/src/codex_app_server.rs` | 扩展既有 transport，不另起进程协议 |
| App Server 主循环、观察器、审批与动态工具 | `src-tauri/src/commands.rs` 中 `observe_codex_app_server_event`、`handle_codex_app_server_request` | requestUserInput 与事件投影在此闭环 |
| 观察器当前只消费少量 item/turn 事件 | `observe_codex_app_server_event` | M1/M2 必须补 delta 和非 item 事件 |
| requestUserInput 当前进入“不支持请求”分支 | `handle_codex_app_server_request` | M3 必须替换 `-32601`，保留未知请求 fail-closed |
| Codex 主提示只有宽泛可观察性要求 | `codex_main_prompt` | M1-02 与原生 progress contract 对齐 |
| 原生 R-Code 已有公开进度规则 | `crates/r-code-agent-worker/src/llm_runtime.rs` | 复用语义，不复制相互漂移的规则 |
| 时间线已有 agent progress、tool、context、reasoning 展示 | `src-tauri/frontend/src/components/room/Timeline.tsx`、`TimelineActivity.tsx`、`model.ts` | 增量扩展现有模型和组件 |
| Plan 已有问题卡与 pending question UX | `src-tauri/frontend/src/components/plan/PlanPanel.tsx` | 复用视觉原语，不复用 Plan 持久化语义 |
| 前端统一测试入口存在 | `src-tauri/frontend/package.json` 的 `npm test` | 新测试接入现有 runner |
| Codex 相关 Rust fixture 已内嵌在 host 测试 | `src-tauri/src/commands.rs` 测试模块 | 先提炼协议 fixture，再扩展真实路径覆盖 |
| 当前协议基线为 Codex CLI 0.145.0 | `codex --version` 与生成 JSON schema | 实验字段必须版本/能力保护 |

## 4. 机器合同

### 4.1 宿主归一化事件

实现可以调整内部类型名，但必须表达下列版本化语义：

```text
CodexTimelineEventV1 =
  AssistantStarted(scope, item_id, phase)
  AssistantDelta(scope, item_id, phase, delta)
  AssistantCompleted(scope, item_id, phase, authoritative_text)
  ReasoningSummaryDelta(scope, item_id, summary_index, delta)
  ReasoningSummaryCompleted(scope, item_id, public_summary)
  ToolStarted(scope, item_id, kind, safe_input)
  ToolOutputDelta(scope, item_id, safe_delta)
  ToolCompleted(scope, item_id, status, safe_output)
  PlanUpdated(scope, explanation?, steps)
  DiffUpdated(scope, unified_diff_or_reference)
  ContextCompacted(scope, item_id)
  Warning(scope?, code?, safe_message)
  UsageUpdated(scope, safe_usage)
  UserInputRequested(scope, item_id, transport_generation, request_id, questions, auto_resolution_ms?)
  UserInputResolved(scope, item_id, outcome)
```

固定要求：

- `scope` 至少包含 task/run/thread/turn；不完整或不匹配的 run 事件不得进入当前时间线。
- phase 与 item kind 使用显式枚举和 unknown 兼容分支，禁止任意字符串直接驱动 UI。
- 所有文本在持久化前执行类型对应的边界、脱敏与长度限制；secret answer 永不进入该事件正文。
- `item/completed` 覆盖状态，不覆盖已经验证一致的增量顺序；权威全文与已累计文本不一致时，以全文修正并记录 mismatch 计数。

### 4.2 Commentary 状态机

```text
Absent -> Streaming(commentary|final_answer) -> Completed
                 | repeated delta: append once by ordered frame
                 | duplicate completed: idempotent no-op
                 | authoritative mismatch: replace text + diagnostic counter
```

- 同一 item 在 live UI 中只有一个节点；刷新/历史重建后 key 保持稳定。
- commentary 出现在其相关工具动作之前或之间；final answer 只在该 turn 的正式交付位置显示作者标签。
- delta UI 刷新必须合并，测试默认上限为每 item 每秒 10 次 React 可见更新；内部累计不能丢字。

### 4.3 requestUserInput 合同

Codex 0.145.0 schema：

```json
{
  "method": "item/tool/requestUserInput",
  "id": 41,
  "params": {
    "threadId": "thr",
    "turnId": "turn",
    "itemId": "item",
    "autoResolutionMs": null,
    "questions": [
      {
        "id": "scope",
        "header": "范围",
        "question": "本次处理哪一部分？",
        "isOther": true,
        "isSecret": false,
        "options": [
          { "label": "当前模块", "description": "限制变更范围" }
        ]
      }
    ]
  }
}
```

成功响应：

```json
{
  "id": 41,
  "result": {
    "answers": {
      "scope": { "answers": ["当前模块"] }
    }
  }
}
```

状态机：

```text
Received -> Pending -> Submitting -> Resolved
                  \-> Cancelled
                  \-> AutoResolved
                  \-> Expired(turn/transport/app lifecycle)
```

不变量：

- 一个 request 只发送一次 result/error；提交按钮需要原子 claim。
- 问题 ID 在请求内唯一；重复/空 ID、未知 schema、超限 payload 以协议错误结束并留下无正文诊断。
- `autoResolutionMs` 非空时由宿主单调时钟驱动；用户提交与自动解决竞争时只有一个胜者。
- `serverRequest/resolved` 清除 pending UI；迟到回答拒绝且不发送到其他 request。
- 普通取消返回协议允许的空答案或显式错误，具体编码以当前生成 schema/上游测试为准并冻结 fixture。

### 4.4 持久化与回放

- commentary、final、公开 reasoning summary、工具生命周期、context compaction 和非敏感状态进入现有会话/运行事件存储。
- pending request 的非敏感问题与状态可以持久化为 UI 恢复标记；transport request id 只在原 generation 有效。
- secret question 可以持久化问题文本和“需要 secret 输入”标志，但不能持久化答案值。
- 应用重启后仍显示未完成问题时必须标记 `expired`，提示用户重新发送/重试；不能伪装为可恢复的原 JSON-RPC 请求。
- 历史重建使用同一 reducer/normalizer，不能通过单独的“历史专用解析器”产生不同排序或标签。

### 4.5 能力与降级

- 启动时记录 Codex CLI 版本与相关能力；实验字段必须接受缺省，不得因旧版缺少字段导致整个 turn 崩溃。
- 能力缺失：保留 final answer 与既有工具卡，隐藏不可用交互，并产生一次可操作提示；禁止循环重试或反复弹窗。
- 未知 server request：仍返回 JSON-RPC 方法不支持；只有精确支持且通过 scope/schema 校验的 requestUserInput 进入等待。
- schema fixture 标注来源版本；升级 Codex CLI 时先运行兼容门禁，新增字段默认忽略并诊断，删除/改义字段触发失败。

## 5. 产品与 UI 行为

### 5.1 时间线信息层次

1. 用户消息。
2. 有内容的 commentary：无 `R-CODE` 作者头，使用较轻视觉层次，保持 Markdown 与流式光标。
3. 结构化活动：命令、文件、搜索、图片、MCP、动态/协作工具按现有分组卡展示。
4. 上下文与状态：计划、压缩、warning、usage 使用紧凑 context 行；失败可展开安全详情。
5. final answer：正式作者头、完整 Markdown、明确交付状态。

同一 item 不因 delta 生成多行。连续同类工具可以聚合，但失败、审批、提问和最终结果不能被折叠到不可发现。

### 5.2 用户提问卡

- 出现在触发它的 turn 内，自动滚动但不抢夺正在输入的编辑器焦点。
- 每题显示 header、question、选项与可用的“其他”输入；`isSecret` 使用密码输入和不可回显摘要。
- 支持选项与自由文本的多答案数组；未回答 required 问题时提交禁用。
- 提交中禁止重复点击；成功后显示非敏感答案摘要，secret 只显示“已安全提交”。
- 取消、超时、turn 结束、连接断开和应用重启显示不同原因；过期卡只读。
- 键盘可完整完成选择、输入、提交/取消；状态变化通过 `aria-live`，错误与问题关联。

### 5.3 进度播报合同

Codex 主代理提示必须表达以下公开行为，并与原生 Agent 的语义共用测试 fixture：

- 首个实质工具批次前，或方案发生实质变化时，给出一句简短公开进度。
- 新证据改变诊断、完成一个阶段或需要用户决定时，说明发现和下一步。
- 不逐条复述工具名/参数，不输出“继续读取”等无新信息句子，不泄露私有推理。
- 简单任务不制造播报；长任务按信息变化而不是固定 token/时间强制刷屏。

### 5.4 Reasoning 展示

- `Codex 思考摘要` 只展示公开 summary；通用、重复、仅标题式内容继续过滤。
- commentary 不能标成 reasoning，reasoning 也不能当正式回答。
- raw reasoning 开关、配置或上游字段不得绕过宿主安全过滤。

## 6. 质量、性能与安全门禁

| 维度 | implementation profile 的二值门禁 |
| --- | --- |
| 正确性 | fixture 中所有公开字符按序出现一次；final、工具终态与 request response 无重复 |
| 传播延迟 | 不含 Provider/进程时间，从收到 delta 到测试 UI 状态更新 p95 ≤ 250ms |
| UI 更新密度 | 单 item 10,000 个小 delta 不产生 10,000 个 DOM 节点；可见更新频率 ≤ 10Hz，最终文本完整 |
| 内存/边界 | 超限行、输出、问题、答案和未知 payload 被有界拒绝或截断；进程持续可用 |
| 作用域 | stale/missing thread 或 turn 的帧不能污染当前 run；迟到 answer 不发送 |
| 隐私 | secret answer、raw reasoning 和未脱敏输出在 JSONL/SQLite/log/support bundle fixture 中均不存在 |
| 恢复 | reload 历史与 live 结果一致；transport/app 重启后的 pending request 为 expired |
| 可访问性 | 问题卡可仅键盘完成；状态/错误有语义关联；亮/暗主题关键视口无横向溢出 |
| 兼容性 | 0.145.0 fixture 全绿；缺字段/未知字段/能力缺失 fixture 按合同降级 |
| 跨平台 | Windows 与 macOS 共用 contract suite；平台 adapter 测试不改变事件语义 |

## 7. 需求追踪表

| RequirementRef | 能力 | TaskID | AssertionID | 预期证据 |
| --- | --- | --- | --- | --- |
| R-COM-01 | 流式消息投影 | M0-02、M1-01、M1-03 | M1-01.A1、M1-03.A1 | backend/frontend JSON 报告 |
| R-COM-02 | 低噪声进度合同 | M1-02 | M1-02.A1、M1-02.A2 | prompt fixture 报告 |
| R-ACT-01 | 结构化工具卡 | M2-01 | M2-01.A1、M2-01.A2 | tool lifecycle 报告 |
| R-ACT-02 | 计划/diff/压缩/状态 | M2-02 | M2-02.A1、M2-02.A2 | context event 报告 |
| R-HITL-01 | 反向请求与响应 | M0-01、M3-01 | M3-01.A1、M3-01.A2 | protocol fixture 报告 |
| R-HITL-02 | 问题 UI | M3-02 | M3-02.A1、M3-02.A2、M3-02.A3 | E2E/可访问性报告 |
| R-HITL-03 | 生命周期与 steer 分离 | M3-03 | M3-03.A1、M3-03.A2、M3-03.A3 | race/recovery 报告 |
| R-RSN-01 | reasoning/secret 安全 | M4-01 | M4-01.A1、M4-01.A2 | security-negative 报告 |
| R-REL-01 | 排序、幂等、回放 | M1-01、M1-03、M4-01 | M1-01.A2、M1-03.A2、M4-01.A3 | replay 报告 |
| R-REL-02 | 有界与断流可靠性 | M0-02、M2-01、M4-01 | M0-02.A3、M2-01.A3、M4-01.A4 | reliability 报告 |
| R-COMP-01 | 版本/平台兼容 | M0-01、M0-02、M4-02 | M0-01.A2、M0-02.A2、M4-02.A1 | capability/CI 报告 |
| R-UX-01 | 信息层次与视觉 | M1-03、M2-02、M3-02、M4-02 | M1-03.A3、M2-02.A3、M4-02.A2 | visual/E2E 证据 |
| R-OBS-01 | 无敏感诊断 | M0-02、M4-01 | M0-02.A4、M4-01.A2 | diagnostics 报告 |

<!-- AI_WORKLIST_NORMATIVE_END -->

<!-- AI_WORKLIST_CONTRACT_START -->

## 8. Verification Harness 与里程碑

### 8.1 唯一产品验收入口

M0-01 建立并由后续任务扩展：

```powershell
node scripts/verify-codex-interaction.mjs --task <TASK_ID> --profile implementation
node scripts/verify-codex-interaction.mjs --through <MILESTONE_ID> --profile implementation
node scripts/verify-codex-interaction.mjs --through M4 --profile production
```

Harness 必须：

- 非交互运行；0 仅代表全部 required assertions 通过。
- 维护 assertion registry，支持 task、through、implementation/production profile。
- 编排 Rust contract/integration、前端 reducer/组件、browser E2E、schema drift、security-negative、性能与跨平台 adapter 测试。
- 输出 `artifacts/ai-tasks/verification/codex-rich-interaction/<profile>/<task-or-milestone>.json` 和证据索引。
- 报告 revision/worktree digest、Codex fixture schema version、平台、失败断言；不记录 secret、raw reasoning 或凭据。
- required fixture/metric 缺失视为失败；禁止删测试、降阈值、缩小 source 或把 mock 冒充真实 Codex。

M0-01 自身在 Harness 尚未存在时，先用任务卡列出的直接 Rust/Node 测试和脚本单测验收；随后必须用新 Harness 自验证一次。

### 8.2 里程碑

| 里程碑 | 能力出口 | 累计门禁 |
| --- | --- | --- |
| M0 协议与验收地基 | fixture、schema/capability 合同、统一事件类型、Harness | `--through M0 --profile implementation` |
| M1 公开消息流 | commentary/final 流式、低噪声提示、live/history 一致 | `--through M1 --profile implementation` |
| M2 结构化活动 | 工具、输出、计划、diff、压缩、warning、usage 完整投影 | `--through M2 --profile implementation` |
| M3 同轮人机交互 | requestUserInput 后端、UI、取消/超时/恢复、steer 分离 | `--through M3 --profile implementation` |
| M4 安全与发布收口 | 隐私、有界、性能、跨平台 E2E、维护文档 | `--through M4 --profile implementation` |

## 9. 主 Checklist（唯一状态源）

- [x] **M0-01** 建立统一验证 Harness、官方协议 fixture 与 AI 任务证据入口。证据：`artifacts/ai-tasks/evidence/codex-rich-interaction/M0-01.yaml`、`artifacts/ai-tasks/verification/codex-rich-interaction/implementation/M0-01.json`
- [x] **M0-02** 冻结版本化事件归一化、作用域、能力协商与诊断合同。证据：`artifacts/ai-tasks/evidence/codex-rich-interaction/M0-02.yaml`、`artifacts/ai-tasks/verification/codex-rich-interaction/implementation/M0-02.json`
- [x] **M1-01** 后端投影 `agentMessage` started/delta/completed，保留 phase 并保证幂等顺序。证据：`artifacts/ai-tasks/evidence/codex-rich-interaction/M1-01.yaml`、`artifacts/ai-tasks/verification/codex-rich-interaction/implementation/M1-01.json`
- [x] **M1-02** 对齐 Codex 主代理与原生 R-Code 的低噪声公开进度合同。证据：`artifacts/ai-tasks/evidence/codex-rich-interaction/M1-02.yaml`、`artifacts/ai-tasks/verification/codex-rich-interaction/implementation/M1-02.json`
- [x] **M1-03** 前端实现 commentary/final 流式呈现、稳定 key 与历史一致回放。证据：`artifacts/ai-tasks/evidence/codex-rich-interaction/M1-03.yaml`、`artifacts/ai-tasks/verification/codex-rich-interaction/implementation/M1-03.json`
- [x] **M2-01** 完整投影工具生命周期与有界命令/工具输出增量。证据：`artifacts/ai-tasks/evidence/codex-rich-interaction/M2-01.yaml`、`artifacts/ai-tasks/verification/codex-rich-interaction/implementation/M2-01.json`
- [x] **M2-02** 投影计划、diff、上下文压缩、warning、错误和 usage 状态。证据：`artifacts/ai-tasks/evidence/codex-rich-interaction/M2-02.yaml`、`artifacts/ai-tasks/verification/codex-rich-interaction/implementation/M2-02.json`
- [x] **M3-01** 实现 `item/tool/requestUserInput` 后端反向请求桥与原子响应。证据：`artifacts/ai-tasks/evidence/codex-rich-interaction/M3-01.yaml`、`artifacts/ai-tasks/verification/codex-rich-interaction/implementation/M3-01.json`
- [x] **M3-02** 实现可访问的问题卡、选项/自由/secret 输入和提交状态。证据：`artifacts/ai-tasks/evidence/codex-rich-interaction/M3-02.yaml`、`artifacts/ai-tasks/verification/codex-rich-interaction/implementation/M3-02.json`
- [x] **M3-03** 完成取消、自动解决、turn/transport/app 失效与 steer 分离。证据：`artifacts/ai-tasks/evidence/codex-rich-interaction/M3-03.yaml`、`artifacts/ai-tasks/verification/codex-rich-interaction/implementation/M3-03.json`
- [x] **M4-01** 完成 reasoning/secret 安全、有界队列、乱序/重复/断流与性能加固。证据：`artifacts/ai-tasks/evidence/codex-rich-interaction/M4-01.yaml`、`artifacts/ai-tasks/verification/codex-rich-interaction/implementation/performance.json`
- [x] **M4-02** 完成跨平台累计 E2E、视觉/可访问性、兼容文档与发布候选门禁。证据：`artifacts/ai-tasks/evidence/codex-rich-interaction/M4-02.yaml`、`artifacts/ai-tasks/verification/codex-rich-interaction/implementation/M4.json`

## 10. 详细任务卡

### M0-01 建立统一验证 Harness、官方协议 fixture 与证据入口

- 结果：后续每项能力都有同一个非交互验收入口，协议 fixture 可离线重放且标注真实来源版本。
- 需求引用：R-HITL-01、R-COMP-01、§8。
- 依赖：无。
- 前置事实：前端已有 `npm test`；host 已有 Codex 内嵌 fixture；本机 CLI 0.145.0 可生成 JSON schema。
- 固定约束：fixture 不含凭据；不能依赖真实账号才能跑 implementation profile；缺失 required assertion 必须失败。
- 决策空间：优先用薄 Node orchestrator 调用既有 Rust/Node runner；若仓库已有等价通用 runner，扩展而不是复制。
- 产物：`scripts/verify-codex-interaction.mjs`、assertion registry、0.145.0 最小协议 fixture、runner tests、任务/证据目录约定。
- 实施步骤：
  1. 只读盘点相关 Rust/Node 测试命令和 CI 平台矩阵。
  2. 从生成 schema 提取本 PRD涉及的最小事件/request/response fixture，并记录 CLI 版本与字段来源。
  3. 实现 `--task`、`--through`、`--profile`、JSON 报告和 required 缺失失败。
  4. 注册全部任务断言，未实现断言可以明确失败但不能静默跳过。
  5. 复制并泛化 `current-task` / `task-evidence` 模板，运行 runner 自测。
- 验收断言：
  - `M0-01.A1`（contract）：未知 task、缺失 required assertion、失败子命令均返回非 0，报告列出准确失败 ID。
  - `M0-01.A2`（contract）：fixture schema 与 0.145.0 的 requestUserInput 必需字段/响应映射一致，来源版本可机器读取。
  - `M0-01.A3`（regression）：Harness 能编排至少一个 Rust 和一个前端测试，并生成无 secret 的 JSON 索引。
- 验证：先运行 runner 单测，再运行 `node scripts/verify-codex-interaction.mjs --task M0-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/codex-rich-interaction/M0-01.yaml` 与对应 verification JSON。
- 失败处理：保存失败报告；修复 runner/registry，不得把失败断言改成 optional 来通过。

### M0-02 冻结事件归一化、作用域、能力协商与诊断合同

- 结果：原始 App Server 帧先转换为稳定、安全、可版本化的宿主事件；旧/未知协议有确定降级。
- 需求引用：R-COM-01、R-REL-02、R-COMP-01、R-OBS-01、§4.1、§4.5。
- 依赖：M0-01。
- 前置事实：`recognized_protocol_progress`、scope requirement 和 observer 已存在但事件集合不一致。
- 固定约束：stale/missing scope fail-closed；未知 request 仍返回方法不支持；诊断不保存原始敏感 payload。
- 决策空间：类型可以位于 host 或可复用 core 模块；选择依赖最少且能被 Rust contract test 直接构造的位置。
- 产物：归一化类型/转换器、能力快照、协议计数、scope/compat fixture 与断言注册。
- 实施步骤：
  1. 对齐 transport progress、scope、observer 与 request dispatcher 的方法表。
  2. 实现 §4.1 事件 union 和 unknown/compat 分支。
  3. 加入文本、数组、输出与问题数量/大小边界及脱敏元数据。
  4. 冻结 transport generation 与 request scope 标识。
  5. 覆盖 stale、missing、unknown、重复、超限和旧字段命名 fixture。
- 验收断言：
  - `M0-02.A1`（contract）：所有 §4.1 已知帧转换为预期事件，unknown 只产生安全诊断。
  - `M0-02.A2`（compatibility）：缺少可选字段与新增未知字段不崩溃；缺少必需 scope 不进入当前 run。
  - `M0-02.A3`（reliability）：超限 frame/payload 有界失败，后续合法 frame 仍可处理。
  - `M0-02.A4`（security）：诊断 fixture 不包含 raw reasoning、secret、凭据或未脱敏命令输出。
- 验证：`node scripts/verify-codex-interaction.mjs --task M0-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/codex-rich-interaction/M0-02.yaml`。
- 失败处理：保留旧 observer 路径可回退；只修转换边界，不放宽 scope 或大小限制。

### M1-01 后端投影 agentMessage 并保证幂等顺序

- 结果：commentary/final 的 started、delta、completed 在宿主持久化为一条稳定消息，权威完成帧不重复正文。
- 需求引用：R-COM-01、R-REL-01、§4.2。
- 依赖：M0-02。
- 前置事实：现有 `AgentEvent::Message {delta}` 与 pending text 机制可复用。
- 固定约束：phase 不丢失；item identity 稳定；迟到/重复完成幂等；作用域不匹配丢弃并计数。
- 决策空间：可扩展 AgentEvent metadata 或增加 Codex 专用 envelope；选择能让 live/history 共用且不破坏其他 Provider 的最小方案。
- 产物：observer delta 处理、item buffer、完成校正、持久化/replay tests。
- 实施步骤：
  1. 添加 agentMessage started/delta/completed 的转换和 phase 保留。
  2. 按 item 维护有界累计文本与完成状态。
  3. completed 全文与累计一致时只封口；不一致时权威替换并记录 mismatch。
  4. flush turn 完成/中断时残留 buffer，并覆盖多 turn/多 run 交错。
- 验收断言：
  - `M1-01.A1`（integration）：commentary 和 final fixture 字符按序各出现一次，phase 正确。
  - `M1-01.A2`（reliability）：重复 delta/completed、迟到帧和交错 run 不产生重复或串线。
  - `M1-01.A3`（regression）：既有 Codex final、原生 R-Code 与子代理消息测试保持通过。
- 验证：`node scripts/verify-codex-interaction.mjs --task M1-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/codex-rich-interaction/M1-01.yaml`。
- 失败处理：保存最小事件序列 fixture；修 reducer/identity，不通过关闭 delta 回退成只看 final。

### M1-02 对齐低噪声公开进度合同

- 结果：Codex 主代理在长任务中主动给出有信息增量的 commentary，简单任务不被强制刷屏。
- 需求引用：R-COM-02、§5.3。
- 依赖：M0-01。
- 前置事实：原生 `WORKSPACE_SYSTEM_PROMPT` 已有成熟规则；Codex main prompt 仅有宽泛要求。
- 固定约束：不暴露私有推理；不按固定工具次数制造空播报；用户语言保持一致。
- 决策空间：共享常量、生成函数或 contract fixture 均可；优先避免两个 prompt 副本漂移。
- 产物：共享进度语义、Codex prompt 接线、正/负 prompt fixture。
- 实施步骤：
  1. 提取两条主代理路径共同的公开进度原则。
  2. 注入 Codex main，不改变子代理“简洁交付”边界。
  3. 增加多阶段、新证据、简单问答、重复工具四类 fixture。
  4. 验证最终回答规则和用户语言合同未被覆盖。
- 验收断言：
  - `M1-02.A1`（contract）：Codex prompt 包含首次实质批次、阶段变化、新发现、低噪声和私有推理禁令。
  - `M1-02.A2`（regression）：简单任务 fixture 不要求播报；“继续读取”类空更新被明确禁止。
  - `M1-02.A3`（regression）：原生 R-Code 与 Codex 共享语义测试同时通过且没有相互覆盖的重复合同。
- 验证：`node scripts/verify-codex-interaction.mjs --task M1-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/codex-rich-interaction/M1-02.yaml`。
- 失败处理：保持行为约束简短；不通过增加大量示例或强制固定更新频率修补。

### M1-03 前端流式呈现与历史一致回放

- 结果：commentary 原位流式更新，final 正式呈现；live、刷新与历史加载得到同一结构和顺序。
- 需求引用：R-COM-01、R-REL-01、R-UX-01、§5.1。
- 依赖：M1-01。
- 前置事实：Timeline 已能标识 progress update，但当前消息模型不保存完整 App Server item/phase 身份。
- 固定约束：一 item 一节点；稳定 key；不得为每个字符创建 DOM；final 作者层次明确。
- 决策空间：扩展 TimelineItem 或 reducer metadata；复用现有 Markdown 流式组件和活动分组。
- 产物：前端类型/reducer/render、coalescing、live/history fixtures、视觉基线。
- 实施步骤：
  1. 接入版本化消息身份、phase 与 streaming 状态。
  2. 实现 ≤10Hz 可见刷新且不丢内部文本。
  3. completed 封口、authoritative correction 和 stable key 回放。
  4. 覆盖 commentary→tool→commentary→final 的顺序与分组。
  5. 检查亮/暗主题和长 Markdown。
- 验收断言：
  - `M1-03.A1`（unit/component）：10,000 delta 最终文本完整且单 item 只有一个消息节点。
  - `M1-03.A2`（integration）：live state 序列化再历史重建后结构、顺序、phase 一致。
  - `M1-03.A3`（visual）：commentary、活动与 final 层次清楚，关键视口无横向溢出。
- 验证：`node scripts/verify-codex-interaction.mjs --task M1-03 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/codex-rich-interaction/M1-03.yaml`。
- 失败处理：用 reducer fixture 定位，不通过隐藏中间消息或把全部 commentary 合成 final 规避。

### M2-01 完整投影工具生命周期与有界输出

- 结果：所有支持的 App Server 工具 item 有 started/active/completed/failed/declined 终态，命令输出增量安全可展开。
- 需求引用：R-ACT-01、R-REL-02、§4.1、§5.1。
- 依赖：M0-02、M1-03。
- 前置事实：`codex_item_tool` 已映射部分工具；UI 已有工具卡和输出摘要。
- 固定约束：safe input/output 先脱敏再截断；item id 关联；失败和审批不可被普通成功分组遮蔽。
- 决策空间：按现有 tool activity kind 扩展；稀有 kind 可使用明确“其他 Codex 工具”卡但保留安全类型名。
- 产物：command/file/MCP/dynamic/collab/web/image 映射、output delta buffer、卡片状态与 tests。
- 实施步骤：
  1. 建立 App Server item kind → R-Code activity kind 映射表。
  2. 接入 started/completed 和 command output delta；file diff 使用引用/摘要，避免复制超大文本。
  3. 处理 failed/declined/cancelled/exit code/success=false。
  4. 覆盖重复完成、输出迟到、未知 kind 和并发工具交错。
- 验收断言：
  - `M2-01.A1`（contract）：每个支持 kind 的 started/completed fixture 映射到正确安全卡片与终态。
  - `M2-01.A2`（integration）：命令输出按序、可展开、截断可见，失败状态与 exit code 一致。
  - `M2-01.A3`（reliability）：超大/高频输出有界，慢消费不阻塞 turn 且终态不丢。
- 验证：`node scripts/verify-codex-interaction.mjs --task M2-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/codex-rich-interaction/M2-01.yaml`。
- 失败处理：保留原始 fixture hash 与安全摘要；不得为调试把未脱敏输出写入证据。

### M2-02 投影计划、diff、压缩、warning、错误与 usage

- 结果：非工具过程事件具有紧凑、可区分、可回放的 UI，不再丢失或伪装成聊天消息。
- 需求引用：R-ACT-02、R-UX-01、§4.1、§5.1。
- 依赖：M0-02、M1-03。
- 前置事实：前端已有 plan/context/usage 基础类型与 `r_code_context_compacted` 展示。
- 固定约束：`item/*` 是 item 真相源；plan/diff 不能靠空 turn.items；warning payload 必须安全。
- 决策空间：复用 PlanTodoCard、TimelineContextEvent、Files workbench diff 引用；选择最小一致 UI。
- 产物：turn plan/diff、contextCompaction、warning/error/usage normalizer 与 UI/tests。
- 实施步骤：
  1. 接入 `turn/plan/updated`、`turn/diff/updated`、contextCompaction 与 usage/warning。
  2. 定义更新覆盖、合并和历史回放规则。
  3. diff 大文本存引用/摘要，点击进入现有 Files workbench。
  4. 覆盖空、重复、长内容、失败和未知 warning code。
- 验收断言：
  - `M2-02.A1`（integration）：计划、diff、压缩、warning、usage fixture 各自生成正确事件而非 agent 消息。
  - `M2-02.A2`（replay）：重复更新幂等，历史重建保留最终计划、diff 引用和压缩位置。
  - `M2-02.A3`（visual）：紧凑状态可扫描、失败可发现、长 diff 不撑破时间线。
- 验证：`node scripts/verify-codex-interaction.mjs --task M2-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/codex-rich-interaction/M2-02.yaml`。
- 失败处理：按事件类型单独降级为安全 context 行；不得合并成模糊“Codex 正在处理”。

### M3-01 实现 requestUserInput 后端反向请求桥

- 结果：合法 requestUserInput 进入 pending，用户答案精确响应原请求，同一 turn 继续；未知请求仍 fail-closed。
- 需求引用：R-HITL-01、§4.3。
- 依赖：M0-02、M1-01。
- 前置事实：主循环已有 FuturesUnordered 分发、审批等待和 writer channel；requestUserInput 当前返回 `-32601`。
- 固定约束：scope/schema 先验证；一个请求一个响应；secret 不持久化；等待用户时 idle watchdog 不误杀。
- 决策空间：可复用 pending permission 生命周期原语或新增专用 registry；必须避免把用户问题当 R2 审批。
- 产物：pending registry、事件/IPC、answer encoder、serverRequest/resolved 接线、race tests。
- 实施步骤：
  1. 解析并验证 0.145.0 request schema、问题 ID、长度与 transport generation。
  2. 原子登记 pending 并向前端发非敏感请求事件。
  3. 增加提交/取消 IPC，claim 后编码 `{answers:{...}}` 返回 writer。
  4. 等待期间调整 watchdog；resolved/turn/transport 终态回收 registry。
  5. 覆盖成功、多题、自由输入、secret、重复提交、writer 失败。
- 验收断言：
  - `M3-01.A1`（integration）：合法单/多题答案按 question id 返回，Codex fixture 随后继续并完成 turn。
  - `M3-01.A2`（security）：secret 答案只出现在捕获的单次 writer 响应，不出现在持久化/日志 fixture。
  - `M3-01.A3`（reliability）：重复提交、错误 scope、重复 ID、writer 关闭均只有一个确定终态且无悬挂 handler。
- 验证：`node scripts/verify-codex-interaction.mjs --task M3-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/codex-rich-interaction/M3-01.yaml`。
- 失败处理：保留未知请求的标准错误；不得自动把问题内容作为 steer 新开消息。

### M3-02 实现可访问的问题卡与输入状态

- 结果：用户可在时间线中完成普通、其他和 secret 问题，清楚看到提交/终态且不会重复发送。
- 需求引用：R-HITL-02、R-UX-01、§5.2。
- 依赖：M3-01。
- 前置事实：PlanPanel 已有结构化问题 UI；RoomScene 已有运行中 composer/steer 行为。
- 固定约束：不写 Plan store；secret 不回显；提交中锁定；过期只读；不抢编辑器焦点。
- 决策空间：抽取共享 QuestionCard 原语或组合现有组件；按最小重复和可测试性选择。
- 产物：类型、question card、IPC、状态 reducer、键盘/读屏/visual E2E。
- 实施步骤：
  1. 抽取/复用问题、选项、自由文本和状态组件。
  2. 处理 `isOther`、`isSecret`、多答案与 validation。
  3. 实现 submitting/resolved/cancelled/expired/error 状态和敏感摘要。
  4. 增加键盘、aria-live、错误关联、亮/暗与窄视口验证。
- 验收断言：
  - `M3-02.A1`（component）：普通/其他/secret 输入编码正确，secret DOM/历史摘要不显示原值。
  - `M3-02.A2`（E2E）：仅键盘完成选择、输入、提交；成功后同一 turn 出现继续运行事件。
  - `M3-02.A3`（visual/accessibility）：亮暗主题和 1280×800、390×844 无横向溢出，状态可被读屏识别。
- 验证：`node scripts/verify-codex-interaction.mjs --task M3-02 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/codex-rich-interaction/M3-02.yaml` 与截图索引。
- 失败处理：保存无敏感值的 DOM/截图；不改成普通聊天输入框规避结构化协议。

### M3-03 完成生命周期、自动解决与 steer 分离

- 结果：所有竞争终态只完成一次；用户主动引导与模型提问互不吞消息、互不串 request。
- 需求引用：R-HITL-03、§4.3、§4.4。
- 依赖：M3-01、M3-02。
- 前置事实：主循环已有 `turn/steer` 与 turn/transport lifecycle；官方 request 有 `autoResolutionMs`。
- 固定约束：单调时钟；generation 失效；turn 完成后不能答；应用重启不伪恢复 writer。
- 决策空间：timeout 可由 host task 或统一 scheduler 管理；选择取消清晰、无 detached task 的方式。
- 产物：竞态状态机、auto-resolution、steer 分流、reload/restart UI、race fixtures。
- 实施步骤：
  1. 定义 answer/cancel/timeout/resolved/turn end/transport end 的原子 claim 顺序。
  2. 接入 autoResolutionMs 和 serverRequest/resolved。
  3. 将普通运行中消息继续路由到 steer，仅 pending 卡提交路由到 response。
  4. 应用 reload 重建非敏感 UI；进程重启统一标记 expired。
  5. 用可控时钟覆盖所有两两竞争。
- 验收断言：
  - `M3-03.A1`（reliability）：answer vs timeout/cancel/resolved/turn end 每组竞态仅一个 writer 结果和一个 UI 终态。
  - `M3-03.A2`（integration）：pending 期间普通 composer 消息走 steer，问题提交只回答 request，二者顺序可追踪。
  - `M3-03.A3`（recovery）：reload 保留可用 pending；transport/app restart 后卡片 expired 且迟到提交被拒绝。
- 验证：`node scripts/verify-codex-interaction.mjs --task M3-03 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/codex-rich-interaction/M3-03.yaml`。
- 失败处理：保留失败序列与 generation；不通过延长无限 timeout 或允许重复响应修复。

### M4-01 完成隐私、有界、乱序/断流与性能加固

- 结果：丰富交互在恶意/异常 payload、慢消费、断流和高频 delta 下保持安全、正确、可恢复。
- 需求引用：R-RSN-01、R-REL-01、R-REL-02、R-OBS-01、§6。
- 依赖：M1-03、M2-01、M2-02、M3-03。
- 前置事实：现有 reasoning summary 已做过滤；App Server transport/动态事件已有边界与 watchdog。
- 固定约束：raw reasoning/secret 永不落证据；required 性能指标缺失失败；不能丢终态换吞吐。
- 决策空间：按 item buffer、LRU/terminal tombstone、bounded channel 等现有模式选择最小可靠方案。
- 产物：security-negative、fuzz/property 或表驱动边界测试、性能基准、断流/重复/乱序 fixture、诊断计数。
- 实施步骤：
  1. 审计所有新事件从 wire 到日志/DB/UI/support bundle 的敏感路径。
  2. 注入 raw reasoning、secret、超长命令、超多问题、无效 UTF/JSON 边界 fixture。
  3. 压测 10,000 delta、并发工具和慢 UI，测传播 p95、节点数与内存上限。
  4. 覆盖断流、重复完成、迟到 output、stale turn 和 transport generation 变化。
  5. 确认终态和 final 不因丢弃中间低优先级事件而丢失。
- 验收断言：
  - `M4-01.A1`（security-negative）：raw reasoning 与 secret 在所有持久化/日志/支持包 oracle 中为 0 命中。
  - `M4-01.A2`（security/observability）：诊断仅含允许元数据，仍能定位 unknown/overflow/timeout/duplicate 类别。
  - `M4-01.A3`（replay/reliability）：乱序/重复/断流 fixture 最终状态确定且不跨 run。
  - `M4-01.A4`（performance）：§6 延迟、更新密度和有界指标全部存在并通过。
- 验证：`node scripts/verify-codex-interaction.mjs --task M4-01 --profile implementation`。
- 证据：`artifacts/ai-tasks/evidence/codex-rich-interaction/M4-01.yaml` 与性能 JSON。
- 失败处理：保留失败指标；优化合并/数据结构，不降阈值、不删 required fixture。

### M4-02 完成跨平台 E2E、文档与发布候选门禁

- 结果：implementation profile 全绿，Windows/macOS 使用同一产品语义；维护文档可指导升级、降级和诊断。
- 需求引用：R-COMP-01、R-UX-01、§1.3、§8。
- 依赖：M4-01。
- 前置事实：仓库有 Windows 开发环境、macOS 验证清单、前端 browser mock 和跨平台 CI。
- 固定约束：fixture/local 不冒充真实安装包；真实登录/安装包属于 production profile；不要求发布或 push。
- 决策空间：E2E 可优先用 fake App Server 做确定性主门，再用已登录 CLI 做候选 smoke。
- 产物：累计 E2E、macOS adapter/CI、视觉证据、architecture/operations/diagnostics 更新、兼容矩阵。
- 实施步骤：
  1. 跑完整 implementation Harness 与受影响 workspace/frontend regression。
  2. 在 Windows 真实 CLI 候选跑 commentary、工具、提问、取消 smoke，证据脱敏。
  3. 在 macOS CI 跑共用 contract 与 adapter tests；将真机安装包检查列入 production profile。
  4. 更新架构、运维和诊断文档，说明 commentary/reasoning/request/steer 区别与降级。
  5. 运行链接、schema drift、视觉/可访问性和最终累计门禁。
- 验收断言：
  - `M4-02.A1`（cross-platform）：Windows/macOS 共用断言全绿，平台 adapter 没有事件语义分叉。
  - `M4-02.A2`（E2E/visual）：完整 commentary→tool→question→answer→final 流程通过并有亮/暗证据。
  - `M4-02.A3`（regression/docs）：workspace/frontend 相关回归与文档链接门禁为 0，兼容/降级说明与实现一致。
  - `M4-02.A4`（production gate）：真实登录和安装包状态明确标为 passed 或 external pending，不伪造结论。
- 验证：`node scripts/verify-codex-interaction.mjs --through M4 --profile implementation`；生产候选使用 `--profile production`。
- 证据：`artifacts/ai-tasks/evidence/codex-rich-interaction/M4-02.yaml` 与 M4 累计报告。
- 失败处理：实现问题回到对应最早任务修复并复跑累计门禁；仅真实凭据/安装包缺失可留 external pending。

## 11. 连续执行、恢复与证据协议

### 11.1 固定循环

```text
preflight
  -> 选择编号最小且 depends_on 已通过的未完成 MUST
  -> 建立/恢复 current.yaml
  -> 实现一个可验证子步
  -> 更新 current.yaml
  -> 运行 --task
       -> 失败：保存失败证据、定位根因、聚焦修复、复跑
       -> 通过：运行 --through 当前里程碑
  -> 归档 task evidence
  -> 勾选 §9 唯一 Checkbox
  -> 更新进度并立即进入下一 ready 项
```

### 11.2 证据规则

- 当前任务：`artifacts/ai-tasks/current.yaml`。
- 通过证据：`artifacts/ai-tasks/evidence/codex-rich-interaction/<TASK_ID>.yaml`。
- 验证报告：`artifacts/ai-tasks/verification/codex-rich-interaction/<profile>/<task-or-milestone>.json`。
- 每份证据记录执行命令、退出码、断言、revision/worktree digest、变更路径与关键可复核决定。
- 截图只证明视觉；不能单独证明协议、持久化、隐私或同 turn 继续。
- secret、凭据、raw reasoning 和完整敏感工具输出禁止进入任何证据。

### 11.3 自主决策与失败处理

1. 先查本文、代码、测试和生成 schema；已有模式优先复用。
2. 可逆选择按安全 > 正确 > 简单 > 一致 > 可测试 > 性能 > 新颖性排序，并记录决定。
3. 测试失败先保存最小复现，再聚焦修复；同一方案无进展时换满足固定约束的实现。
4. 真实 Codex/签名/安装包不可用时，继续完成 fixture、fake、adapter 和 implementation profile；只把外部放行留 pending。
5. 禁止通过关闭中间事件、隐藏错误、持久化 secret、降低性能阈值或把 request 变成新 turn 来“修绿”。

## 12. 风险、兼容与外部放行

| 风险 | 预防/恢复 |
| --- | --- |
| App Server 实验协议变更 | 版本化 fixture、能力检测、unknown 安全分支、schema drift 门禁 |
| delta 与 completed 重复 | item identity + authoritative correction + terminal tombstone |
| UI 更新过密 | 内部完整累计、≤10Hz 可见合并、性能 required 指标 |
| request 与 turn/transport 竞态 | transport generation、原子 claim、可控时钟 race tests |
| secret/raw reasoning 泄露 | wire 前后 security-negative oracle、日志/DB/support bundle 全链审计 |
| 过度播报 | 共享低噪声 prompt contract 与正/负 fixture，不按固定频率强制 |
| Windows/macOS 分叉 | 共用 normalizer/reducer/fixture，平台 adapter 单独测试 |

外部 production 放行只包括真实账号、真实安装包、签名/更新环境与最终人工视觉抽查；这些条件不改变 implementation checklist 的完成判据，也不授权本任务发布。

<!-- AI_WORKLIST_CONTRACT_END -->
