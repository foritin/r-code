# Code Review：Codex App Server → R-Code 动态子代理委派（修订版）

> **2026-08-06 发布门禁提示**：§1–§13 保留了每一轮审查当时的证据和历史结论，其中的“当前”行号、未修限制和测试数量不再代表最新工作树。请以 **§14 发布前最终复核** 作为 0.3.0 的权威结论。

- **日期**：2026-08-05
- **基线**：`835373a`（Merge pull request #5 from foritin/dev）
- **审查对象**：工作区 7 个未提交源文件，`+1056/-50`
- **协议基准**：本机 `codex-cli 0.145.0` 生成的 App Server JSON Schema，并对照 OpenAI 官方 App Server 文档
- **结论**：已按 §9 建议顺序全部修复（F1–F17），并经独立复核补修（见 §11.3）与全量验证；首次启动的动态委派可用性（F8）、RequestApproval 审批链路（F3）均已打通。第四轮独立复核（§12）新发现的 3 个 HIGH 缺陷（H1 idle 误杀、H2 取消断链幽灵执行、H3 ReadOnly 父 inherit 升权）与 M5–M7 已在工作区落实；**第五轮（§13）逐条核实上述修复，并补修 M4（审批 head-of-line 阻塞）与 L1（setup 反向请求静默）、同步三处文档，全量验证通过——当前无已知阻断项。**
- **本次操作**：§10 独立复核 + §11 修复 + §11.3 复核补修 + §12 第四轮独立复核 + §13 第五轮核实与补修（本表上方内容保留原始审查结论，作为修复前后对照）

---

## 1. 对用户最初四个问题的直接结论

### 1.1 Codex 主 Agent 委派 R-Code 时仍可能打开新 session

**属实，而且当前 diff 只解决了一半。**

新的 `rcode_delegate_subagent` 动态工具在被选中时，确实会复用当前 task、parent run 和 child run，不会创建第二个顶层 Task。但“不要调用全局 R-Code MCP”目前只是提示词和工具描述中的软约束：

- 提示词要求模型改用动态工具：[commands.rs:11668](../../src-tauri/src/commands.rs#L11668)
- 动态工具描述也说明全局 MCP 会创建独立 session：[commands.rs:12612](../../src-tauri/src/commands.rs#L12612)
- App Server 启动命令没有禁用用户 Codex 配置中的 `[mcp_servers.r-code]`：[commands.rs:12679](../../src-tauri/src/commands.rs#L12679)
- 项目仍有意保留该全局 MCP 配置：[commands.rs:18494](../../src-tauri/src/commands.rs#L18494)
- 旧 MCP 的 `r_code_delegate` 会调用 `task_create_with_agent`，因此必然创建顶层任务：[mcp_server.rs:229](../../src-tauri/src/mcp_server.rs#L229)

所以准确表述只能是：**Codex 选择新动态工具时会同树委派；模型若仍选择旧全局 MCP，原问题会复发。**

### 1.2 子代理报 `tool 'bash' is not available in the current task mode`

该错误与当前权限映射一致，不代表 Bash 工具未注册。

- 只有父 Codex 的有效权限模式为 `FullAccess` 时，child ceiling 才是 FullAccess；RequestApproval、AutoReview、Custom 都被折叠成 ReadOnly：[commands.rs:13885](../../src-tauri/src/commands.rs#L13885)
- `ToolPolicy::ReadOnly` 不暴露 Bash：[llm_runtime.rs:1653](../../crates/r-code-agent-worker/src/llm_runtime.rs#L1653)

因此当前名为 `inherit` 的行为并不真正继承“可经审批执行命令”的父能力，而是把所有非 FullAccess 父运行降成硬只读。这能直接解释截图中的错误。产品需要明确选择：

1. 子代理默认硬只读，并把 schema、提示词和 UI 文案改准确；或
2. 增加能表示 RequestApproval/RiskBased 的 child policy，让 Bash 可见但仍经过 Gateway 审批。

另外还有一个相反的权限缺陷：显式 ReadOnly child 在 FullAccess workspace 下仍可能通过 external/MCP 执行变更，见 §4.3。

### 1.3 运行计时器刷新不实时

本次改动已让**前台且 WebView 聚焦**的活跃 run 约每秒刷新；针对性浏览器测试通过。但：

- 时钟在窗口不可见或 WebView 失焦时主动停走：[shared-clock.ts:16](../../src-tauri/frontend/src/lib/shared-clock.ts#L16)
- 每次 tick 会重新渲染整个可见 Timeline，而不是只更新 duration：[Timeline.tsx:152](../../src-tauri/frontend/src/components/room/Timeline.tsx#L152)
- 测试只验证 4 秒内文本变化，没有验证约 1 秒的刷新频率，也没有测量“无关组件未重渲染”：[app-shell.test.mjs:472](../../src-tauri/frontend/scripts/app-shell.test.mjs#L472)

原报告关于“已结束 run 使用陈旧快照”的判断不成立：活跃状态由任一 `ended_at === null` 推导，结束后 duration 又直接使用 `ended_at`。

### 1.4 是否能显示 Codex CLI 的思考过程

**原始 chain-of-thought 不应获取或嵌入；公开 reasoning summary 可以。**

当前实现请求 `summary: "concise"`，只读取 App Server 明确提供的 `summary` 数组，并忽略 raw `content`、`text` 和 reasoning delta：[commands.rs:11434](../../src-tauri/src/commands.rs#L11434)、[commands.rs:13474](../../src-tauri/src/commands.rs#L13474)。这个隐私边界方向正确；剩余问题是输入内存边界、事件结构和测试覆盖，而不是去获取私有推理。

---

## 2. 审查方法与证据边界

本修订版使用以下可重复证据：

1. 通读完整未提交 diff，并追到 Gateway、MCP、SQLite、前端时钟和测试夹具。
2. 三路只读专项复核：协议/并发生命周期、安全/审计、测试/前端/兼容性。
3. 本机 `codex-cli 0.145.0` 的 `ServerRequest`、`PermissionsRequestApprovalResponse`、`CommandExecutionRequestApprovalResponse` 等生成 schema。
4. OpenAI 官方 [App Server README](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md) 与 [outgoing_message.rs](https://raw.githubusercontent.com/openai/codex/main/codex-rs/app-server/src/outgoing_message.rs)。
5. 当前工作区实测，结果见 §8。

前版报告提到的“第一轮曾失败的 registry 测试”“临时 tokio demo”和当时的代理数量属于旧作者执行记录；当前工作区无法独立重建其原始现场。本修订版不再把它们当作当前可复现实证，所有结论均可由现有代码、当前 schema 或本轮测试支撑。

归因标签：

- **新增**：由这 7 个文件的当前 diff 引入。
- **相邻既有**：基线已有，但本次新路径使其更容易触发或使影响扩大。
- **混合**：基线设计与新路径组合后形成问题。
- **测试空白**：不能仅凭现有绿灯证明功能成立。

---

## 3. 发现总览

| 编号 | 发现 | 优先级 | 归因 | 置信度 |
|---|---|---:|---|---:|
| F1 | 旧全局 R-Code MCP 未被禁用，“不再开新 session”不是硬保证 | P1 | 新增 | 10/10 |
| F2 | 动态 child 阻塞 App Server 唯一读泵，deadline/steer 不能抢占，取消也不保证回收 | P1 | 新增 | 10/10 |
| F3 | 子代理权限契约同时存在“inherit 能力塌缩”和 ReadOnly external/MCP 绕过 | P1 | 新增/混合 | 10/10 |
| F4 | `item/permissions/requestApproval` 返回了错误响应 schema | P1 | 相邻既有，触发面扩大 | 10/10 |
| F5 | 多种合法 App Server 反向请求被静默忽略 | P1 | 相邻既有，触发面扩大 | 9/10 |
| F6 | 核心动态委派链路没有纵向集成测试 | P1 | 测试空白 | 10/10 |
| F7 | 动态调用实际串行，“主 + 3 child 并发”结论不成立 | P2 | 新增 | 10/10 |
| F8 | 实验 API 无版本/能力降级，且无 R-Code Provider 时仍宣传工具 | P2 | 新增 | 9/10 |
| F9 | JSONL 读取、summary 处理与双事件队列没有真实内存上界 | P2 | 新增/混合 | 10/10 |
| F10 | 外部 `callId` 直接作为全库主键，可交叉污染审计 | P2 | 混合 | 9/10 |
| F11 | 持久化风险等级与委派 assignment 均不可信/不完整 | P2 | 混合 | 10/10 |
| F12 | `pending_steers` 未区分请求与响应，但原报告严重度和触发概率被高估 | P2 | 相邻既有 | 10/10 |
| F13 | 动态结果可被截成无效内层 JSON，内部错误还会进入模型上下文 | P2 | 新增 | 9/10 |
| F14 | goal/参数边界在各委派入口不统一，类型错误可能 fail-open 为 inherit | P3 | 新增/既有 | 9/10 |
| F15 | reasoning summary 用魔法前缀跨 Rust/TS 识别 | P3 | 新增 | 10/10 |
| F16 | 计时器功能基本成立，但失焦停走、整 Timeline tick 与测试命名仍有缺口 | P3 | 新增 | 10/10 |
| F17 | 架构、安全、隐私和 CHANGELOG 未同步新行为 | P2 | 文档缺口 | 10/10 |

---

## 4. 合入前阻断项

### 4.1 F1：旧全局 MCP 仍能创建独立 session

提示词无法构成路由不变量。只要用户曾安装 R-Code Codex MCP，hosted Codex 仍可能同时看到：

- 新动态工具 `rcode_delegate_subagent`：同 task/run 树；
- 旧 MCP 工具 `r_code_delegate`：调用 `task_create_with_agent` 创建顶层 Task。

**影响**：用户最初问题仍可能复发，且模型选择哪条路径具有非确定性。

**建议**：

1. 启动该 hosted run 时，通过进程级配置覆盖、thread config 或工具 allowlist 技术性禁用受管的 `mcp_servers.r-code`；
2. 宿主再对旧 `r_code_delegate*` 做拒绝或重定向，不能只依赖 prompt；
3. 增加测试，断言 hosted main 的工具目录中只有同树委派入口。

### 4.2 F2：动态 child 阻塞唯一 App Server 读泵

外层主循环在 `lines.next_line()` 分支里同步等待整个 request handler：[commands.rs:13534](../../src-tauri/src/commands.rs#L13534)、[commands.rs:13631](../../src-tauri/src/commands.rs#L13631)。动态 handler 又等待 child 进入终态：[commands.rs:13027](../../src-tauri/src/commands.rs#L13027)。

在 child 完成前，外层无法处理：

- 5 分钟 idle 与 30 分钟 hard deadline；
- steer；
- 后续 server requests、`turn/completed`、stdout EOF 或 App Server 崩溃。

原报告有两处需要同时纠正：

- “cancellation 完全不被轮询”不准确：内层确实会监听父/子 cancellation 并设置 `abort`：[commands.rs:13041](../../src-tauri/src/commands.rs#L13041)。
- “取消传播完整”也不准确：`abort` 只是协作信号。进行中的 external MCP 在 `execute().await` 阶段没有超时或抢占：[gateway.rs:840](../../crates/r-code-gateway/src/gateway.rs#L840)、[client.rs:293](../../crates/r-code-mcp/src/client.rs#L293)。Provider 建连和部分工具阶段也可能迟迟不返回。Bash 本身已有 120 秒默认、600 秒上限，不能泛化成“所有工具永久卡死”。

简单给 `runner.run()` 外包一层 `timeout` 仍不够：`spawn_with_run_id` 内部又 detached spawn 真正的 `run_child`，[SubagentHandle](../../crates/r-code-agent-worker/src/llm_runtime.rs#L2207) 没有 `JoinHandle`，[wait_for_subagent](../../crates/r-code-agent-worker/src/llm_runtime.rs#L3025) 也无期限。drop 外层 future 可能遗留真实 child、事件转发器和 registry reservation。

**建议**：分离 stdout 读泵与 request 执行；用受管任务组保存真实 child handle；超时/取消时 abort 后 bounded join；为 Provider/MCP 增加调用期限；用 RAII/finally 清理 registry 和持久化状态。

### 4.3 F3：权限契约有两个相反方向的缺陷

#### A. `inherit` 并不真正继承父策略

动态 schema 默认 `inherit`：[commands.rs:12628](../../src-tauri/src/commands.rs#L12628)，但 child ceiling 只有 ReadOnly/FullAccess 两档。RequestApproval、AutoReview、Custom 父运行都变为硬 ReadOnly，因此 Bash/edit 不可见。这与“继承父权限”文案不符，也解释了用户截图。

同时，这个默认值又与原生委派和文档的“子代理默认只读”冲突：

- 原生 `delegate_task` 默认 ReadOnly：[llm_runtime.rs:2078](../../crates/r-code-agent-worker/src/llm_runtime.rs#L2078)
- [ARCHITECTURE.md:262](../ARCHITECTURE.md#L262)
- [SECURITY.md:42](../../SECURITY.md#L42)

FullAccess 父运行省略 `access` 时，child 会自动获得完整写入/命令能力，不再需要显式请求 FullAccess。

#### B. 显式 ReadOnly 仍可执行 mutating external/MCP

- external 工具对所有 policy 无条件暴露：[llm_runtime.rs:1642](../../crates/r-code-agent-worker/src/llm_runtime.rs#L1642)
- ReadOnly policy 调 external 时仍取 workspace 的持久化 access mode：[llm_runtime.rs:1726](../../crates/r-code-agent-worker/src/llm_runtime.rs#L1726)
- workspace 为 FullAccess 时，Gateway 的 ReadOnly subagent R2 拒绝条件不成立：[gateway.rs:754](../../crates/r-code-gateway/src/gateway.rs#L754)

所以 `FullAccess workspace + access:"read_only" + mutating MCP` 可在 UI/SQLite 仍标 ReadOnly 的情况下执行变更。这没有突破项目总体 FullAccess 授权，但确实突破了本次 child 的显式降权边界，违反 [SECURITY.md:31](../../SECURITY.md#L31)。

**建议**：

1. 先决定默认是严格 ReadOnly，还是增加能表达 RequestApproval/RiskBased 的 child policy；
2. external/MCP 的 effective access 必须同时受 `ToolPolicy` 与 workspace policy 约束；
3. ReadOnly 下 R2 external 应在调用 closure 前确定拒绝；
4. 用 fake mutating `ExternalToolHost` 做回归测试。

### 4.4 F4：permissions approval 响应不符合当前 schema

handler 把 command、fileChange、permissions 三类审批合并处理，并统一回：

```json
{ "result": { "decision": "accept" } }
```

相关代码：[commands.rs:13112](../../src-tauri/src/commands.rs#L13112)、[commands.rs:13205](../../src-tauri/src/commands.rs#L13205)。

当前 Codex 0.145 schema 中：

- command/fileChange 响应必填 `decision`；
- `PermissionsRequestApprovalResponse` 必填 `permissions`，可选 `scope` 和 `strictAutoReview`。

因此 permissions 的允许和拒绝分支都发送了错误结构。Codex 可能拒绝反序列化或把它视为空授权，用户点击允许也不能按协议完成授权。官方契约见 [Permission requests](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md#permission-requests)。

**建议**：按 method 分开编码；允许时只返回请求 profile 的受限子集，拒绝时返回空 grant 或协议规定的错误；用本机生成 schema 校验 fixture 响应。

### 4.5 F5：合法反向请求被静默忽略

`handle_codex_app_server_request` 除动态工具和三类审批外一律返回 `Ignored`，且不写 result/error：[commands.rs:13112](../../src-tauri/src/commands.rs#L13112)。

当前 `ServerRequest` 还包含：

- `item/tool/requestUserInput`
- `mcpServer/elicitation/request`
- `account/chatgptAuthTokens/refresh`
- `attestation/generate`
- `currentTime/read`
- legacy approvals

`requestUserInput` 和 MCP elicitation 明确要求 client 回应；静默忽略后，turn 通常只能等 R-Code 的 idle/hard timeout 收场。attestation/currentTime/auth 的可达性取决于 capability/config，不能一概视为当前必触发，但 unknown request 仍不应无响应。官方契约见 [Approvals and server requests](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md#approvals)。

setup helper 还有两个邻接问题：[wait_for_codex_app_server_response](../../src-tauri/src/commands.rs#L12740) 不能处理初始化期间的反向请求，而且每读一条无关通知都会重新开始 startup timeout，通知洪泛可延长总等待。

**建议**：支持的 request 走明确 UI/宿主流程；不支持的带 id 请求立即返回标准 JSON-RPC error 或安全 cancel/decline；startup 使用绝对 deadline 并轮询 cancellation。

### 4.6 F6：核心路径没有纵向集成测试

当前没有测试真正贯通：

```text
Codex item/tool/call
  → R-Code provider
  → Bash/MCP/Gateway
  → SQLite + JSONL
  → dynamic tool response
  → 前端实时投影与重放
```

现有覆盖的实际边界：

- access 测试只检查工具 JSON 与纯函数：[commands.rs:18780](../../src-tauri/src/commands.rs#L18780)
- App Server fixtures 全部传 `rcode_delegate: None`：[commands.rs:18999](../../src-tauri/src/commands.rs#L18999)
- runner 测试只让 mock provider 返回文本：[llm_runtime.rs:4548](../../crates/r-code-agent-worker/src/llm_runtime.rs#L4548)
- Bash 测试只验证 schema/scoped input，没有执行 Bash：[llm_runtime.rs:4720](../../crates/r-code-agent-worker/src/llm_runtime.rs#L4720)
- 三个新增前端测试是纯投影或浏览器 mock integration，不是 Tauri/Rust E2E

这正是为什么原截图中的“bash unavailable”、误开 session、协议挂起和权限问题可以在全量 Rust 测试通过、三个新增前端测试通过时仍存在。

**合入门槛测试**：

1. 新动态 request → child → response → DB/JSONL；
2. hosted run 不暴露旧 `r_code_delegate*`；
3. ReadOnly + FullAccess workspace + mutating external 必须拒绝；
4. hard timeout、父取消、child 取消、App Server EOF 后均 bounded join 且 registry 为空；
5. permissions/unknown request 用当前 schema 校验；
6. 2–3 个并发 dynamic calls 可重叠、乱序响应且 id 正确；
7. 旧 CLI/缺 Provider 的明确降级；
8. 超长帧、高频 delta、巨大 MCP result 的有界失败。

---

## 5. 其他确定问题

### 5.1 F7：动态调用实际串行

官方 App Server 可让多个 dynamic requests 同时在途；R-Code 读到第一个请求后却在 stdout 分支内等 child 完成，后续请求只能滞留管道。因此 registry 的原子 cap=4 只能证明“1 主 + 3 child”的配额计算正确，不能证明 Codex→R-Code 路径支持 3 个 child 并发。

如果产品需要并发，request handler 应独立 dispatch，并通过单一 writer/mutex 序列化响应；如果不需要，应删除并发承诺和误导性正面结论。

### 5.2 F8：实验 API 与运行能力没有协商

CLI probe 只要 `codex --version` 成功就视为可用：[commands.rs:9808](../../src-tauri/src/commands.rs#L9808)。随后：

- 无条件发送 `experimentalApi:true`：[commands.rs:13423](../../src-tauri/src/commands.rs#L13423)
- 有 delegate 时发送 `dynamicTools`：[commands.rs:13453](../../src-tauri/src/commands.rs#L13453)
- 固定发送 `summary:"concise"`：[commands.rs:13474](../../src-tauri/src/commands.rs#L13474)

旧 CLI 拒绝任一步时只得到笼统 `ApprovalBridge`，没有最低版本提示或无动态工具重试。并且 Codex main 启动时不预检 R-Code Provider，工具会被宣传，直到调用才在 [commands.rs:12958](../../src-tauri/src/commands.rs#L12958) 失败。

**建议**：只在确有动态委派且能力满足时 opt in；设最低版本或 capability probe；不支持时给明确升级提示/安全降级；Provider 不可用时隐藏工具或标记 disabled。

### 5.3 F9：输入与事件流没有真实内存上界

原报告“受 32MB 上限约束”和“峰值约 200–400MB”都不准确：

- `BufRead::lines().next_line()` 先把完整行读入 `String`，之后才检查 32MB：[commands.rs:13595](../../src-tauri/src/commands.rs#L13595)
- setup 同样是后检查：[commands.rs:12746](../../src-tauri/src/commands.rs#L12746)
- `codex exec --json` 路径没有对应行长检查：[commands.rs:12416](../../src-tauri/src/commands.rs#L12416)
- summary 先全量 collect/join，再经过 11 轮 regex redaction，最后才截成 800 字符：[commands.rs:11439](../../src-tauri/src/commands.rs#L11439)、[secret.rs:182](../../crates/r-code-core/src/secret.rs#L182)
- observer 还会 clone 完整 params：[commands.rs:13238](../../src-tauri/src/commands.rs#L13238)
- 新路径叠加两个 `unbounded_channel`：[commands.rs:13002](../../src-tauri/src/commands.rs#L13002)、[llm_runtime.rs:2950](../../crates/r-code-agent-worker/src/llm_runtime.rs#L2950)

无换行 stdout、高频 delta、慢 DB/WebView sink 或巨大 MCP 结果都可能继续增长内存。没有测量数据支持精确峰值，报告不应伪造数字。

**建议**：使用读取前限制 frame 长度的 codec；限制 summary parts 数和累计字节；事件通道改有界并合并 delta；桥接层限制 tool result。

### 5.4 F10：`callId` 与数据库主键作用域不匹配

动态 request 的 `callId` 只截到 160 字符：[commands.rs:12997](../../src-tauri/src/commands.rs#L12997)，随后直接成为 ToolCall id/委派外键：[commands.rs:3708](../../src-tauri/src/commands.rs#L3708)。

但：

- `tool_calls.id` 是全库主键：[migrations.rs:295](../../crates/r-code-store/src/migrations.rs#L295)
- 冲突用 `INSERT OR IGNORE` 静默吞掉：[repositories.rs:876](../../crates/r-code-store/src/repositories.rs#L876)
- finish 只按 id 更新，不校验 task/run：[repositories.rs:902](../../crates/r-code-store/src/repositories.rs#L902)

重复 id 或超长同前缀截断碰撞会让第二个 run 丢审计行，并可能更新第一条记录。协议没有在这里提供“跨所有 R-Code tasks 全局唯一”的数据库契约。

原报告的 NUL/C 字符串截断推论没有当前消费方证据，应删除。

**建议**：宿主生成数据库 UUID，外部 id 单独保存；至少使用 `(run_id, external_call_id)` 唯一键，并在 finish 中带 run/task ownership。

### 5.5 F11：持久化审计不记录真实风险和 assignment

`observed_tool_risk` 仅识别 `read_file` 与少数写工具，其他都记为 R0：[commands.rs:3648](../../src-tauri/src/commands.rs#L3648)。所以 Bash、`edit`、mutating `mcp_call` 的 SQLite 风险可能与 Gateway 实际判定不一致。

同时动态委派参数被折叠为固定摘要：

- dynamic item 只映射成“委派 R-Code 子智能体”：[commands.rs:11260](../../src-tauri/src/commands.rs#L11260)
- SQLite input 只得到 `{"summary":"委派 R-Code 子智能体"}`：[commands.rs:13256](../../src-tauri/src/commands.rs#L13256)
- child goal 只进入内存消息：[llm_runtime.rs:2806](../../crates/r-code-agent-worker/src/llm_runtime.rs#L2806)

完成后无法从持久化记录回答“委派了什么、请求/实际 access 是什么、为何允许、风险是多少”。

**建议**：直接持久化 Gateway 的实际 risk/caller；保存有界 goal、label、requested/effective access，敏感场景可额外保存 hash。

### 5.6 F12：steer id 类型混淆真实，但不是确定性高危

[commands.rs:13609](../../src-tauri/src/commands.rs#L13609) 对任何数字 id 先查 `pending_steers`，没有先排除带 `method` 的请求帧。若 Codex server request id 恰好等于仍在等待的 steer id，请求会被吞。

当前 App Server server request id 从 0 自增，证据见官方 [outgoing_message.rs](https://raw.githubusercontent.com/openai/codex/main/codex-rs/app-server/src/outgoing_message.rs)。但原报告以下说法不成立：

- 不是“到第 1000 个请求后确定碰撞”；必须在同一 run 内达到同号，同时该 steer 仍 pending。
- 没有证据表明“反向碰撞会让 Codex 错解析 steer 响应”；request 带 `method`，response 不带。

**判定**：低概率、中影响的协议稳健性缺陷，不是确定性高危 DoS。

**建议**：只用无 `method` 的帧匹配 response；最好让 R-Code client request 使用带前缀的 string id；补同号 request/response 回归测试。

### 5.7 F13：动态结果和错误输出契约不稳

child summary 先被塞进内层 JSON，再由外层 helper 重新按 8K 字符截断：[commands.rs:13071](../../src-tauri/src/commands.rs#L13071)、[commands.rs:12658](../../src-tauri/src/commands.rs#L12658)。引号、反斜杠和换行会在 JSON escape 后膨胀，外层截断可能让 `contentItems[0].text` 不再是可解析 JSON；外层 JSON-RPC 仍合法，但代码注释承诺的内层 JSON 完整性不成立。

另外 DB/config/runtime 原始错误会进入动态工具结果：[commands.rs:12930](../../src-tauri/src/commands.rs#L12930)、[commands.rs:13069](../../src-tauri/src/commands.rs#L13069)。虽然已有两层常见 secret redaction，但本地路径与内部配置细节仍可能进入后续 Codex 模型上下文，不能说“只返回本地进程”。

**建议**：避免嵌套 JSON 文本或按序列化后预算构造；错误返回稳定 code + 用户可操作文案，原始 error 只进本地受控日志；补 escape-heavy Unicode 和路径错误测试。

### 5.8 F14–F16：低优先级一致性与 UI 问题

**F14 goal/参数契约**：

- 动态 R-Code child：16K，硬拒绝；
- Codex exec/subagent：12K，静默截断；
- 全局 R-Code MCP：12K，硬拒绝；
- 原生 `delegate_task`：未见等价 goal 上限。

这不是“同一路径两个常量”的简单错误，而是所有委派入口缺少统一契约。另有 `access` 存在但类型错误时被 `as_str().unwrap_or("inherit")` 当缺省值处理：[commands.rs:12643](../../src-tauri/src/commands.rs#L12643)。即使 App Server通常会做 schema 校验，宿主边界仍应 typed deserialize、fail-closed。

**F15 summary 魔法前缀**：Rust 用 `Requesting + "Codex 思考摘要："` 持久化，[commands.rs:4074](../../src-tauri/src/commands.rs#L4074)；前端又重复同一常量并按前缀识别，[model.ts:19](../../src-tauri/frontend/src/components/room/model.ts#L19)。当前没有已知伪造源，但这是脆弱的跨端契约，应改成结构化 event kind。

**F16 timer**：前台聚焦时功能成立；失焦暂停是明确设计，[ARCHITECTURE.md:147](../ARCHITECTURE.md#L147) 也这样记录。真正缺口是整 Timeline 每秒 render、测试没有验证 cadence/render isolation，以及用户是否期望“窗口可见但未聚焦”也持续更新。

### 5.9 F17：文档与数据流未同步

当前新功能形成双 Provider 数据流：

```text
Codex Provider 生成 child goal
  → R-Code Provider 接收并执行
  → child summary 回到 Codex Provider 上下文
```

[PRIVACY.md:89](../../PRIVACY.md#L89) 要求影响数据流的实现变更同步 notice 与 CHANGELOG。当前还需要同步：

- `ARCHITECTURE.md` / `SECURITY.md` 的“默认只读”与新 `inherit` 行为；
- Codex→R-Code 同树委派与旧全局 MCP 的区别；
- 公开 reasoning summary 的本地持久化边界；
- `CHANGELOG.md` 的用户可见行为。

---

## 6. 原报告 #1–#9 的逐项修正

| 原编号 | 修订判定 |
|---|---|
| #1 timeout/steer 冻结 | **核心属实，高**；但 cancellation 会被内层轮询，只是协作取消且无法保证回收；原修复建议会遗留 detached child，需受管 JoinHandle |
| #2 ReadOnly external/MCP | **原报告降级错误**；FullAccess workspace 下显式 ReadOnly 的 mutating MCP 确可执行，同时默认 inherit 与项目默认只读契约冲突 |
| #3 pending steers | **缺陷属实但严重度高估**；需要同一 run 内同号且 steer 正 pending，不是请求数到 1000 后确定触发；“反向碰撞”无证据 |
| #4 summary 内存 | **风险属实但描述错误**；32MB 是完整分配后的拒绝阈值，不是内存上限；200–400MB 未测量，应删除 |
| #5 魔法前缀 | **属实，低**；当前无已知伪造源 |
| #6 callId 主键 | **数据完整性风险属实**；影响可跨 task/run；删除 NUL/C 字符串消费方推论 |
| #7 goal 上限 | **部分属实**；应改成所有委派入口的阈值与 reject/truncate 语义不统一 |
| #8 内部错误回传 | **属实，低到中**；已有 secret redaction，但路径/内部细节会进入模型上下文，不只是本地进程 |
| #9 duration 陈旧 | **原功能判断不成立**；剩余问题是失焦停走、整 Timeline tick 和测试未验证其名称所声称的 render isolation |

原报告正面清单也需删除或收窄以下表述：

- 删除“取消传播完整”；
- “3 child 并发”只能说 registry 配额原子，不能说动态路径可并发；
- “同树且不会开新 session”必须加前提：模型选择新动态工具，且旧全局 MCP 未被调用；
- “事件无丢失/投影正确”只能说代码路径看起来闭合，尚无纵向持久化/UI 测试；
- “workspace 绑定”只约束内置 PathGuard 工具；已启用 MCP 仍按其自身服务权限运行；
- 三个新增前端测试应称为纯函数/浏览器 mock integration，不应统称完整 E2E。

---

## 7. 已验证正确的部分

- 当前 Codex 0.145 的 dynamic tool 形状基本正确：`experimentalApi`、`dynamicTools[].inputSchema`、`item/tool/call`、`{success, contentItems}` 与官方契约一致。
- **在新动态 handler 被选中时**，child 复用 task、parent run 和预生成 child run id，不调用 `task_create`。
- 一次性 Codex subagent 不获得反向委派上下文；R-Code child 也禁用递归委派。
- 父 ReadOnly ceiling 不能被显式 `full_access` 提升；问题在默认语义和 external 降权，而不是父 ReadOnly 被直接提权。
- workspace 路径来自当前任务的 DB 绑定；内置工具仍受 PathGuard。
- reasoning 只消费公开 summary，raw reasoning content/delta 不进入 UI/持久化。
- 文件链接提示已进入 native/Codex 主子代理 prompt，浏览器 mock 测试证明现有链接点击可以打开右侧 Files 并定位行；尚未证明真实模型输出到真实 backend resolver 的全链路。
- registry 的 count+insert 在同一锁内，配额本身没有 TOCTOU；但 handler 串行限制了实际利用率。

---

## 8. 本轮验证结果

| 验证 | 结果 |
|---|---|
| `codex --version` | `codex-cli 0.145.0` |
| `cargo test -p r-code-host -p r-code-agent-worker` | **通过** |
| `cargo clippy -p r-code-host --all-targets -- -D warnings` | **通过，0 warning** |
| 三个新增前端测试（reasoning summary / duration / file link） | **3/3 通过** |
| 前端完整 `npm test` | **67/68，通过项之外有 1 个可重复失败** |
| `git diff --check` | 未见 whitespace error；仅现有 LF→CRLF 警告 |

完整前端失败为 [queue-reorder-ui.test.mjs:174](../../src-tauri/frontend/scripts/queue-reorder-ui.test.mjs#L174)：断言 opacity 必须瞬时严格等于 `"0"`，实测多次约为 `0.025x`。对应 CSS 明确有 100ms transition：[room.css:988](../../src-tauri/frontend/src/styles/scenes/room.css#L988)。

该测试、Composer 与 CSS 都不在本次 diff 中；新增 1 秒 Timeline tick 最多可能改变调度时序并暴露既有脆弱断言，没有证据证明它改变 queue 样式语义。应先移出鼠标并等待 transition 收敛，再断言接近/等于 0。无论归因如何，当前文档不能再写“前端全量测试全部通过”。

---

## 9. 建议修复顺序

1. **先保证功能路由**：hosted run 技术性隐藏旧全局 R-Code delegate，确保不再创建顶层 session。
2. **重构生命周期**：独立 App Server 读泵、受管 child task、硬超时/取消的 bounded join、RAII cleanup。
3. **统一权限模型**：确定默认只读或真实 inherit；修掉 ReadOnly external/MCP 绕过。
4. **修协议**：permissions 专用响应；所有带 id 的 server request 必须得到 result/error；加入版本/能力降级。
5. **补纵向测试**：先覆盖用户截图和上述 P1，再谈合入。
6. **修资源与审计**：有界 frame/channel/result，宿主审计 id，持久化真实 risk 与 assignment。
7. **收尾一致性**：nested JSON、稳定错误、goal contract、结构化 summary、timer render isolation、文档/隐私/CHANGELOG。

---

*所有本地 file:line 引用基于 2026-08-05 当前工作区。官方协议是实验性接口，合入前应以项目支持的最低 Codex CLI 版本重新生成 schema 并固化契约测试。*

---

## 10. 独立复核记录（第三轮，2026-08-05）

本机独立复核本修订版全部论断。方法：① 本机 `codex-cli 0.145.0` 实际执行 `codex app-server generate-json-schema --out $env:TEMP\codex-schema-0145` 生成协议 schema 并核对；② 前端全量 `npm test` 复现；③ 逐条对照工作区代码。

### 10.1 本机 schema 实证（F4 / F5 / F12）

| 项目 | 本机 0.145.0 schema 实测 | 结论 |
|---|---|---|
| `PermissionsRequestApprovalResponse` | `required: ["permissions"]`（**不含** decision） | **F4 属实**：R-Code 对三类审批统一回 `{decision}`（commands.rs:13157/13207），permissions 路径响应缺必填字段，Codex 侧反序列化失败或视为空授权 |
| `CommandExecutionRequestApprovalResponse` / `FileChangeRequestApprovalResponse` | `required: ["decision"]` | command/fileChange 响应结构正确，问题仅限 permissions 路径 |
| `ServerRequest` 方法全集（oneOf） | `item/commandExecution/requestApproval`、`item/fileChange/requestApproval`、`item/permissions/requestApproval`、`item/tool/call`、**`item/tool/requestUserInput`**、**`mcpServer/elicitation/request`**、**`account/chatgptAuthTokens/refresh`**、**`attestation/generate`** | **F5 属实**：后 4 个方法不在 R-Code 白名单（commands.rs:13112-13118），被静默 Ignored |
| `RequestId` | `anyOf: [string, integer(int64)]` | **F12 修复建议可行**：R-Code 可用带前缀 string id 根除碰撞 |

触发面精确化：`item/tool/requestUserInput` 标注 EXPERIMENTAL，而 R-Code 已无条件发送 `capabilities.experimentalApi: true`（commands.rs:13423），**该请求实际可达**；`mcpServer/elicitation/request` 依赖客户端在 initialize 声明 `openai/form` 扩展（R-Code 未声明，当前不触发）；`attestation/generate`、`account/chatgptAuthTokens/refresh` 取决于 attestation 能力与认证模式。

### 10.2 前端全量测试复现（§8 声明）

- `npm test`（前端 68 项）：**67 通过 / 1 失败**，与修订版 §8 完全一致。
- 失败项：`queue-reorder-ui.test.mjs:174`，`strictEqual(opacity, "0")` 实测 `0.0262762`。
- 单独运行 `node --test scripts/queue-reorder-ui.test.mjs`：**通过**（1/1）。证实为时序敏感（CSS 100ms transition 与断言竞争，全量/负载下触发），归因结论（既有脆弱断言，非本 diff 语义变更）成立。

### 10.3 F1–F17 与修订版修正的逐条复核

| 论断 | 复核结果 | 关键证据 |
|---|---|---|
| F1 旧全局 MCP 未禁用 | ✅ 属实 | `codex_app_server_command`（commands.rs:12679-12725）仅设 cwd/stdio/kill_on_drop，**无 env/config 覆盖**；thread/start 参数（13441-13459）无工具 allowlist |
| F2 阻塞读泵 + 无法回收 | ✅ 属实 | 主循环 13595→13631-13638 分支体 await；`SubagentHandle` 无 JoinHandle（llm_runtime.rs:2207-2212）；`run_child` 为 detached `tokio::spawn`（2525） |
| F3A inherit 塌缩 | ✅ 属实 | `spawn_codex_main` max_access 映射：仅 `CodexPermissionMode::FullAccess` → FullAccess，其余折叠 ReadOnly（13885 附近） |
| F3B ReadOnly + FullAccess workspace 绕过 | ✅ 属实（边界情形） | `gateway.rs:754-764` 条件 + `call_inner` 取 workspace access_mode（llm_runtime.rs:1726-1735）；上调为 P1 合理（违反显式降权边界） |
| F4 permissions 响应 schema | ✅ 属实（本 schema 实证，见 10.1） | — |
| F5 反向请求被忽略 | ✅ 属实（本 schema 实证，见 10.1） | — |
| F6 无纵向集成测试 | ✅ 属实 | App Server fixtures 全部 `rcode_delegate: None`（18999/19072/19157/19239）；runner 测试仅 mock provider 文本（llm_runtime.rs:4548）；Bash 测试不执行真实 Bash（4720） |
| F7 动态调用串行 | ✅ 属实 | 主循环单线程逐行处理；registry cap 仅配额证明 |
| F8 无版本/能力协商 | ✅ 属实 | probe 仅 `codex --version`（9808）；initialize 无条件 `experimentalApi:true`（13423） |
| F9 行长检查在分配后 | ✅ 属实（修订正确） | `wait_for_codex_app_server_response` 先 `next_line()` 后检查（12746-12755）；主循环同（13595-13600）；exec JSONL 路径无检查（12416）；12758 `continue` 使通知洪泛重置 startup timeout |
| F10 callId 主键 | ✅ 属实 | 12997-13000 → 3716-3717；`repositories.rs:876-899` INSERT OR IGNORE；删除 NUL/C 串推论正确（无消费方证据） |
| F11 审计风险不完整 | ✅ 属实 | `observed_tool_risk`（3648）仅识别少数工具；动态委派折叠为固定摘要（11260/13256） |
| F12 严重度下调 | ✅ 修正合理 | 碰撞条件 = App Server 进程内请求计数 ≥1000 且与当前 run 中 pending steer id 同号；"反向碰撞"无证据 |
| F13 嵌套 JSON 截断 | ✅ 属实 | 13071-13081 外层截断 + 12658-12668 helper 再截断，JSON escape 膨胀可破坏内层 JSON |
| F14 goal 契约 | ✅ 属实 | 16K reject（12898）vs 12K truncate（10947/11047/14288/14428）；`as_str().unwrap_or("inherit")` fail-open（12643） |
| F15 魔法前缀 | ✅ 属实 | commands.rs:4074-4090 + model.ts:19/764-775 双端重复常量 |
| F16 timer | ✅ 修正正确 | 失焦暂停为明确设计（ARCHITECTURE.md:147）；"陈旧快照"场景不成立（活跃态由 `ended_at === null` 推导，结束后直接用 `ended_at`）；剩余为整 Timeline tick 与测试命名 |
| F17 文档未同步 | ✅ 属实 | 工作区无任何文档/CHANGELOG 变更 |

### 10.4 对原报告（前版）修正的确认

原报告所有被修订点均修正正确，其中三项尤为重要：

1. **"cancellation 不被轮询"→ 修正**：内层 select 确实监听父/子 cancellation（commands.rs:13041/13045）并置位 `abort`；但 `abort` 是协作信号，进行中工具调用不可打断（gateway.rs:840、client.rs:293 无超时）——"取消传播完整"应删除。✅
2. **"32MB 内存上限/200-400MB 峰值"→ 删除**：32MB 是**分配后**拒绝阈值，不是分配前上限；峰值数字无测量依据。✅
3. **#9 陈旧快照 → 不成立**：运行时长在 `ended_at` 存在时直接用 `ended_at`，不依赖 `now`。✅

### 10.5 复核结论

本修订版**全部 17 项发现（F1–F17）经独立复核属实**，对前版报告的 3 处修正亦全部正确。新增的 F4（permissions 响应 schema 错误）为本轮最有价值的发现——已用本机 0.145.0 schema 实证，且是用户可见的功能性阻断（permissions 审批路径响应无法被 Codex 正确解析）。**"当前不建议合入"的结论维持。**

*第三轮复核执行记录：`codex app-server generate-json-schema --out %TEMP%\codex-schema-0145`；`cargo test -p r-code-host -p r-code-agent-worker`（通过）；`cargo clippy -p r-code-host --all-targets -- -D warnings`（通过）；`npm test`（67/68，见 10.2）。*

---

## 11. 修复记录（2026-08-05，按 §9 建议顺序）

本轮已按 §9 修复全部 17 项发现（F1–F17），并补纵向集成测试。

### 11.1 修复清单

| 发现 | 修复 | 关键位置 |
|---|---|---|
| F1 旧全局 MCP 未禁用 | hosted App Server 启动命令加 `-c mcp_servers={}` 清空 Codex 侧 MCP；新增断言测试 `hosted_app_server_command_clears_legacy_mcp_servers` | `codex_app_server_command` |
| F2 阻塞读泵 + 无法回收 | stdout 读泵独立 task（限长读行）；单一 writer task + fail channel；动态委派独立 dispatch；child 以 `JoinHandle` 受管回收（`SubagentHandle.join`）；取消/超时 abort + bounded join；收尾 `drain_for_task` | 主循环、`SubagentHandle`/`spawn_with_run_id`/`RCodeSubagentRunner::run` |
| F3 权限契约 | 新增 `ToolPolicy::RequestApproval`（inherit 自非 FullAccess 父）；`RCodeSubagentRequest.require_approval`；external/MCP 有效权限强制 `RequestApproval`（`external_access_mode`）；prompt 描述按权限档位 | `llm_runtime.rs`、`spawn_codex_main`、`handle_codex_rcode_dynamic_tool` |
| F4 permissions 响应 schema | `codex_approval_response` 按 method 编码：permissions 返回 `{permissions, scope}`（允许回显/拒绝空 profile）；command/fileChange 保持 `{decision}` | `handle_codex_app_server_request` |
| F5 未知请求静默忽略 | 白名单外带 id 请求返回 JSON-RPC error（-32601）；无 id 通知仍忽略 | `handle_codex_app_server_request` |
| F6 无纵向测试 | `dynamic_delegate_handler_flows_through_db_and_cleans_registry`：真实 runtime + DB 全链路（响应/subagent_id/落库/审计锚点/registry 清理）；另有 F1/F3/F12 单测 | tests |
| F7 串行 | `FuturesUnordered` 并发 dispatch；registry 注释更新为实际语义 | 主循环 |
| F8 无能力协商 | 版本门槛（`parse_codex_version` ≥ 0.145.0）+ Provider 预检，不满足时隐藏动态工具并提示 | `run_codex_app_server_process_with_images` |
| F9 内存上界 | `read_bounded_line` 限长读行（setup + 主循环 + 超限帧拒绝）；summary parts ≤64 + 逐 part 截断；child 事件队列有界（2048）+ 丢弃计数 | `read_bounded_line`、`safe_codex_reasoning_summary` |
| F10 callId 主键 | 审计锚点用宿主派生 id（`delegate:{run_id}`），外部 callId 存锚点 input；child run 引用锚点 id 满足外键 | `ensure_subagent_run` |
| F11 审计不完整 | 锚点 input 记录 `external_call_id`/`label`/`access_mode`；`observed_tool_risk` 扩展 bash/edit/web_fetch | `ensure_subagent_run`、`observed_tool_risk` |
| F12 steer id 误吞 | `codex_steer_response` 仅匹配无 `method` 的响应帧；回归测试 | 主循环 |
| F13 错误回传 | 内部错误/失败 summary 稳定文案 + 日志；响应 JSON 序列化后验证完整性（超限收缩重建） | `handle_codex_rcode_dynamic_tool` |
| F14 参数契约 | goal 统一 `CODEX_EXEC_MAX_GOAL_CHARS`（12K，reject）；`access` 非字符串显式拒绝（fail-closed） | `handle_codex_rcode_dynamic_tool`、`codex_rcode_delegate_access` |
| F15 魔法前缀 | detail 改为结构化 JSON（`{kind, text}`）；前后端按结构解析，旧前缀兼容回退 | `codex_reasoning_activity`、`codex_reasoning_summary_text`、`model.ts` |
| F16 duration | `RunDuration` memo 子组件；测试改名并补 cadence/render-isolation 断言 | `Timeline.tsx`、`app-shell.test.mjs` |
| F17 文档 | ARCHITECTURE 13.4 节 / SECURITY 边界 / PRIVACY summary 边界 / CHANGELOG Unreleased | 4 个文档 |

### 11.2 验证

- `cargo test -p r-code-host -p r-code-agent-worker`：**全绿**（lib 351 项，10.89s）。
- `cargo clippy -p r-code-host -p r-code-agent-worker --all-targets -- -D warnings`：通过。
- 前端：新增 2 个测试（F15 summary 结构化、F16 duration）+ 全量 `npm test` **68/68 通过**（queue-reorder-ui.test.mjs:174 本轮通过，但该断言仍属时序脆弱——严格等于 `"0"` vs CSS 100ms transition，建议后续按 §8 建议加固）。
- F6 纵向测试修复过程中发现并修复：`delegated_by_tool_call_id` 外键（`REFERENCES tool_calls(id)`）在 F10 改动后断裂——锚点 id 派生化后 child run 仍引用外部 callId；现按"锚点已存在则引用原 id，否则引用派生 id"处理。
- 时序回归修复：收尾先 terminate 再回收读泵；idle_timer 恢复逐行重置语义（全量 fixture 测试 30.83s → 10.89s）。

*修复后的状态：代码已修改并通过全量验证。*

### 11.3 独立复核补修（2026-08-05，第三轮复核后）

外部复核指出首轮修复中的不实/缺口，逐条核实并补修如下：

| 复核发现 | 核实 | 补修 |
|---|---|---|
| F8 未完成：`existing_bridge_for` 使首次启动时动态工具永远隐藏 | **属实**（功能级 bug） | 改用 `bridge_for` + `ensure_real_runtime`（含任务 provider 名），首次启动即初始化 runtime，配置缺失才隐藏 |
| F3 未完成：Gateway 在权限引擎前硬拒绝 RequestApproval 子代理的 bash/edit | **属实** | `gateway.rs` 622 条件放行 `ProjectAccessMode::RequestApproval`（走审批）；ReadOnly 子代理仍被拦截（工具不可见 + scoped_input 先行拒绝） |
| F2 部分：child 卡在不可打断调用时 10s 清理后 detach | **属实** | `r-code-mcp` `call_tool` 加 300s 超时（`MCP_CALL_TIMEOUT`）——MCP 卡死不再无限挂起 |
| F9 部分：有界队列丢终态 lifecycle，child 长期显示运行中 | **属实**（严重） | 终态 `SubagentLifecycle`（Completed/Failed/Cancelled）走独立无界通道，永不丢弃 |
| F10 部分：外部 callId 跨任务复用 | **属实** | `existing` 核验加 `run_id = ?`（仅当前父 run 下的既有记录才复用） |
| permissions 允许时原样回显 profile、无授权摘要 | **属实** | 审批摘要纳入授权内容（文件系统条目数/网络域名数）；profile 回显保留（受 R-Code 审批引擎判定约束） |
| F6 未覆盖 transport/首次启动/并发 | **部分属实** | 新增 `dynamic_delegate_handlers_run_concurrently_and_stay_isolated`（两个 item/tool/call 并发、registry 独立槽位）；transport 层由既有 5 个 fixture 测试覆盖；完整 E2E（真实 CLI）留待接入 CI 环境 |
| F14 语义不统一 | **属实** | 全部入口统一为超长硬拒绝（fail-closed），移除静默截断 |
| F15 exec 路径仍用前缀 | **属实** | `parse_codex_exec_json_line` reasoning 分支改结构化 detail（`{kind,text}`），测试同步更新 |
| F16 时钟订阅仍在 Timeline | **属实** | `useSharedNow` 下沉到 `RunDuration` 子组件，Timeline 不再整体每秒重渲染 |
| F17 文档描述未成立的保证 | **属实** | 本文档（本表）与 ARCHITECTURE/SECURITY/PRIVACY/CHANGELOG 已按补修后行为更新 |
| `cargo fmt --check` 失败 | **属实** | `cargo fmt --all` 已执行并保持通过 |
| 前端 queue-reorder 测试单独跑仍失败（opacity≈0.025） | **属实** | 根因：新行位于队列顶部、落在 headless 鼠标默认位置 (0,0) 被 `:hover` 命中（opacity=1 属正确行为）。测试改为先 `mouse.move` 移开指针再断言收敛值 <0.05 |

**复核后验证**：

- `cargo test -p r-code-host -p r-code-agent-worker -p r-code-gateway -p r-code-mcp`：全绿（host 352 + agent-worker 80 + gateway 149 + mcp 22 + 其余 suite）。
- `cargo clippy --all-targets -- -D warnings`：通过。
- `cargo fmt --all -- --check`：通过。
- 前端 `npm test`：68/68（含 queue-reorder 修复后通过）。
- 新增：`dynamic_delegate_handlers_run_concurrently_and_stay_isolated`（F7 并发验证——registry 直接观测"主槽 + 两 child 同时在途"，返回后槽位已回收）。
- **复核中定位并修复的深层次 bug**：读泵接管 stdout 时 `BufReader::into_inner()` 丢弃缓冲区未读帧（`fill_buf` 可能预读 turn/completed）→ 全量负载下 fixture 测试偶发 30s Deadline 超时（单独跑通过）。修复：直接把 setup 阶段的 BufReader move 进读泵，保留缓冲（lib 全量 31–61s → 11s 稳定）。

**剩余已知限制**（非阻断）：

1. ~~Provider 建连/流式调用无显式超时~~ → **已修复**：agent_loop 层包装 `LLM_PROVIDER_CONNECT_TIMEOUT`（60s）+ `LLM_PROVIDER_IDLE_TIMEOUT`（10 分钟流空闲）——vendor 锁定不再构成无界等待；主循环收尾 join 上限同步提升到 `CODEX_CHILD_JOIN_TIMEOUT`（310s，覆盖 MCP 300s 兜底），取消后 child 终态事件必定持久化，DB 不会留下永远 running 的 child。
2. ~~permissions 允许时原样回显 profile~~ → **已修复**：`codex_approval_response` 与工作区求交——fileSystem entries/legacy read/write 仅保留工作区内路径（无法判定时保守剔除全部），网络 profile 不受影响；授权内容仍纳入审批摘要；R-Code 审批引擎判定为最终闸门。
3. queue-reorder 断言已稳定（鼠标移开 + 收敛等待），但其 `opacity` 语义仍属 UI 行为级断言，后续如调整 hover 样式需同步。
4. 完整 App Server transport E2E（真实 `codex` CLI 进程）未纳入自动化（依赖本机 CLI 版本与登录态），由 fixture 测试 + 纵向 handler 测试覆盖。

---

## 12. 第四轮独立复核（2026-08-05）：修复落实确认与新发现

方法：逐条核实 §11.1/§11.3 每项修复的工作区落点；复跑全量验证；对当前 diff 做对抗性审查（独立子代理 + 复核者亲自逐行验证全部 HIGH 证据链）。

### 12.1 修复落实确认

§11.1/§11.3 声明的修复全部在工作区找到对应实现（抽样落点，行号为当前工作区）：

| 修复 | 落点 |
|---|---|
| F1 `-c mcp_servers={}` + 断言测试 | `commands.rs:12864`、`19910` |
| F2 读泵解耦 / `FuturesUnordered` / `SubagentHandle.join` | `commands.rs:14000/14028`；`llm_runtime.rs:2232` |
| F3 `ToolPolicy::RequestApproval` + gateway 放行 | `llm_runtime.rs:1499/2826-2828`；`gateway.rs:622-629` |
| F4/F5 审批按 method 编码 / 未知请求 -32601 | `commands.rs:13589/13467` |
| F6/F7 纵向测试 + 并发隔离测试 | `commands.rs:19336/19514` |
| F8 版本门槛 + Provider 预检 | `commands.rs:11346/13783/13797` |
| F9 `read_bounded_line` 分配前限长 / 终态独立通道 | `commands.rs:12890/13252-13259` |
| F10 派生锚点 + run_id 限定复用 | `commands.rs:3736/3746` |
| F12 steer 仅匹配无 method 帧 + 回归测试 | `commands.rs:12982/19745` |
| F16 `RunDuration` memo 下沉时钟 | `Timeline.tsx:140-144` |
| F17 四文档 | CHANGELOG Unreleased；ARCHITECTURE 430-435；PRIVACY.md:44；SECURITY.md:32 |

复跑验证（与 §11.3 数字一致）：

- `cargo test -p r-code-host -p r-code-agent-worker -p r-code-gateway -p r-code-mcp`：**全绿**（352 + 80 + 149 + 22 + 其余套件，0 failed）。
- 前端 `npm test`：**68/68 通过**。

### 12.2 新发现（对抗性复核）

| 编号 | 发现 | 优先级 | 状态 |
|---|---|---|---|
| H1 | child 活动不喂 idle_timer：>5 分钟的动态委派确定性误杀整个 Codex run | HIGH | 已修（§13 核实） |
| H2 | 取消断链：parent_abort 与 per-child abort 独立，run 取消后审批中的 detached child 经用户批准仍会幽灵执行写操作 | HIGH | 已修（§13 核实） |
| H3 | ReadOnly 父运行的 inherit 被升权为 RequestApproval 子代理 | HIGH | 已修（§13 核实） |
| M4 | 审批等待期间 in_flight child 整体冻结（head-of-line blocking） | MEDIUM | 已修（§13 补修，影响面修正见 §13.2） |
| M5 | RequestApproval 子代理的 mutating external/MCP 仍被硬拒（fail-closed），错误消息误称 read-only | MEDIUM | 已修（§13 核实） |
| M6 | `codex exec --json` 路径仍用无界 `.lines()`，§11.1 "JSONL 统一限长"声明不实 | MEDIUM | 已修（§13 核实） |
| M7 | 审计只记 requested access 而非 effective，`require_approval` 无持久化（F11 残留） | MEDIUM | 已修（§13 核实） |
| L1–L4 | setup 通知重置 startup timeout / in_flight 返回值静默丢弃 / 孤儿 run 行 / `from_utf8_lossy` 静默替换 | LOW | L1/L2/L4 已修；L3 由 bounded join + 启动恢复兜底（保留已知限制） |

#### H1：idle_timer 不感知 child 活动（HIGH）

idle_timer 重置点只有四处：steer（`commands.rs:14094`）、stdout 新行（14105）、已处理请求（14163）、child 完成（14197）。Codex 主 Agent 发起 `item/tool/call` 后等工具响应期间 stdout 静默，child 事件经 `child_event_sink` 旁路直发（13310-13329），不经过主循环。结果：任何运行超过 `idle_timeout`（5 分钟）的单个委派 → `commands.rs:14036-14047` `IdleTimeout` → `cancellation.cancel()` + `terminate_codex_child` → 整个 Codex run 被标记失败。代码任务中 >5 分钟的委派是常态，此为 F2 重构引入的确定性误杀。

**修复方向**：child 事件活动重置 idle_timer（如 child 事件经主循环转发或向主循环发心跳）；或 `in_flight` 非空时挂起/放宽 idle 判定（child 进度由独立上限约束）。

#### H2：取消断链 → 幽灵执行（HIGH）

取消链在三处断裂：

1. 父/子取消只置位 `request.abort`（`commands.rs:13331-13337`），该 flag 作为 `parent_abort` 传入 supervisor（`llm_runtime.rs:3008` → `2259`）。
2. 每个 child 有独立的 per-child abort（`llm_runtime.rs:2516`），`SessionToolHost.abort`（2824）与 Gateway 审批轮询用它；parent_abort 只在 LLM 迭代边界检查（2845 起的循环）。
3. run 取消后宿主只等 10s bounded join（`commands.rs:14204-14210`），超时 drop in-flight future；detached child 的审批等待不受影响——用户点"允许"后 bash/edit 真实落盘，事件进 void，DB run 行停留 Running（孤儿，依赖重启时 `scan_orphaned_runs` 清理）。

§11.3 所称"流式 50ms 轮询响应宿主取消"对该路径不成立。**修复方向**：parent_abort 桥接到 per-child abort（或 child 的审批/流式轮询同时检查两者）；run 取消时批量 Deny 该 run 的 pending 审批，使"取消后批准"不再可执行。

#### H3：ReadOnly 父运行的 inherit 升权（HIGH）

`spawn_codex_main` 把所有非 FullAccess 父统一映射为 `permission_mode: ProjectAccessMode::RequestApproval`（`commands.rs:14437-14441`）；inherit 时 access 档位给 FullAccess + `require_approval`（13296-13297）→ `ToolPolicy::RequestApproval`（`llm_runtime.rs:2826-2828`）。对 RequestApproval/AutoReview 父这是正确语义；但对显式"仅查看"（ReadOnly）预设的父运行，子代理从"无 bash/edit"升权为"bash/edit 可见、经审批可写"，放大预设边界（习惯性点允许即放行）。F3 修复从"全部折叠 ReadOnly"（过严，即原始 bug）矫枉过正为"全部折叠 RequestApproval"。

**修复方向**：`spawn_codex_main` 区分父档位——ReadOnly 父 inherit 映射 `ToolPolicy::ReadOnly`；RequestApproval/AutoReview/Custom 父映射 `RequestApproval`。

#### M4–M7（MEDIUM）

- **M4 head-of-line blocking**：审批请求在分支体内 `await`（`commands.rs:14145-14152`），等待期间 select 不 poll `in_flight`，全部在途 child 的 socket/审批轮询冻结；长闲连接可被对端断开。
- **M5 external/MCP 审批链未打通**：内置工具路径已放行 RequestApproval（`gateway.rs:622-629`），但 external 路径的 R2+ 硬拒条件未同步（`759-769`：`access_mode != FullAccess && risk ≥ R2` 一律拒绝，消息写死 "read-only subagent"）。fail-closed 无安全风险，但 F3 的审批语义对 external/MCP 不成立，且错误消息误导。
- **M6 exec JSONL 无界读行残留**：`codex exec --json` 路径仍是 `BufReader::new(stdout).lines()`（`commands.rs:12489`），无换行/超长行可无限增长内存。§11.1 F9"JSONL 读取统一 read_bounded_line"的声明不实（仅 app-server 路径修了三处）。
- **M7 审计记 requested 而非 effective**：`scope.access_mode` 记录 FullAccess（`llm_runtime.rs:2513`），`require_approval` 决定 policy 但不回写 scope 也不持久化（2826-2834；锚点 input 只记 label/access_mode，`commands.rs:3756-3758`）。事后无法从持久化记录回答"该 child 实际权限策略是什么"（F11 残留）。

#### L1–L4（LOW，记录不阻断）

1. setup 阶段 startup timeout 每收一条无关通知重置，且带 id 反向请求被静默 continue（`commands.rs:12927-12945`；F5 修复只覆盖主循环）。
2. `in_flight` 完成返回值（Cancelled/Failed）被静默丢弃，仅靠 fail channel 间接兜底（`commands.rs:14195`）。
3. detached child 的 run 行在 DB 停留 Running 孤儿（`commands.rs:14208-14215`）。
4. `read_bounded_line` 用 `from_utf8_lossy`，非法 UTF-8 帧被静默替换 U+FFFD 后进入 JSON 解析（`commands.rs:12904/12909`）。

#### 文档一致性问题

- §11.1 F2 声称"取消/超时 abort + bounded join"，但 `llm_runtime.rs:3045-3047` 的 `join.await` 无 timeout；bounded join 仅存在于宿主 in_flight（10s）。
- `SECURITY.md:43` 仍写 "delegated subagents default to read-only"，与 `ARCHITECTURE.md:434` 的 inherit 新语义（FullAccess 父 → 全权 child）冲突，需同步。

### 12.3 复核结论

F1–F17 修复**全部落实且测试复跑一致**，但 H1–H3 均为生产可复现的功能/信任边界缺陷：**修复 H1（idle 误杀）、H2（取消断链）、H3（ReadOnly 升权）前不建议合入**；M5/M6 建议同批修复。

*第四轮复核执行记录：`cargo test -p r-code-host -p r-code-agent-worker -p r-code-gateway -p r-code-mcp`（全绿）；前端 `npm test`（68/68）；对抗性审查子代理 + 复核者逐行验证全部 HIGH/MEDIUM 证据链。*

---

## 13. 第五轮核实与补修（2026-08-05）

方法：逐条对照 §12.2 每项发现核实工作区落点；补修仍开放的 M4/L1 与文档一致性；新增回归测试；复跑全量验证。

### 13.1 H1–H3、M5–M7 落实确认

| 发现 | 落点（当前工作区） | 结论 |
|---|---|---|
| H1 idle 误杀 | 主循环 idle 分支加 `if in_flight.is_empty()` 守卫（`commands.rs` 主循环，注释标 H1）：委派在途时挂起 idle 判定，deadline/父取消兜底不变 | 落实 |
| H2 取消断链 | parent→child abort 桥接任务（`llm_runtime.rs` `PARENT_ABORT_BRIDGE_POLL` 50ms，child 结束即停止转发）；Gateway 审批轮询先查 abort 再查 decision（`gateway.rs` NeedsApproval 分支）——"取消后批准"不再可执行 | 落实 |
| H3 ReadOnly 父升权 | `codex_rcode_delegate_access` 按父预设分档：ReadOnly 父 inherit → ReadOnly child；审批类父 → FullAccess 档位 + `require_approval` 运行时钳制；`spawn_codex_main` 如实记录 `permission_mode` | 落实 |
| M5 external 审批链 | Gateway external 路径放行 `RequestApproval` 子代理进权限引擎（`gateway.rs` `execute_external_with_wait`）；显式 ReadOnly 的 mutating external 在 `SessionToolHost` policy 层硬拒（`llm_runtime.rs` `call_inner`）；`external_access_mode` 映射 RequestApproval | 落实 |
| M6 exec JSONL | `codex exec --json` 路径统一 `read_bounded_line_into`（分配前限长） | 落实 |
| M7 审计 | `require_approval` 进 `AgentEventScope`（`llm_runtime.rs`）与委派锚点 input（`commands.rs`，标 M7 注释） | 落实 |

### 13.2 本轮补修

- **M4（审批 head-of-line 阻塞）**：先修正影响面——§12 所称"in_flight child 的 socket/审批轮询冻结"在当前架构不成立：child 本体跑在独立 `tokio::spawn`（`llm_runtime.rs` `spawn_with_run_id`），宿主 in_flight future 只是 watch 结果，不 poll 它不会冻结 child。真实风险是**一个审批等待期间后续 App Server 请求（第二个审批、requestUserInput 等）整体排队**。修复：三类审批请求与动态委派一样经 `FuturesUnordered` 独立 dispatch（`commands.rs` 主循环），新增 `CodexInFlightOutcome::{Delegate, Approval}` 区分收尾语义——审批的 Cancelled/Failed 仍终止 run（与串行处理一致），委派异常完成只告警。审批自身有 `CODEX_APP_SERVER_APPROVAL_TIMEOUT` 上限，dispatch 后不会无界挂起。
- **L1（setup 反向请求静默）**：`wait_for_codex_app_server_response` 取得 stdin 写句柄，初始化期间收到的带 id 反向请求立即返回 -32601 JSON-RPC error（与主循环 F5 行为一致）；绝对启动期限语义不变。
- **文档一致性三处**（§12 遗留 + H3 新语义同步）：`SECURITY.md` "delegated subagents default to read-only" 改为按父档位精确描述；`docs/ARCHITECTURE.md` 13.4 权限模型补"只读父 → 只读 child"；`CHANGELOG.md` Unreleased 同步只读父 inherit 语义。§12 点名的 `llm_runtime.rs` join 无 timeout 已在此前落实（`RCodeSubagentRunner::run` 收尾 10s 上限）。

### 13.3 新增回归测试（fixture shim，与既有 App Server 测试同设施）

- `codex_app_server_answers_requests_while_approval_is_pending`（M4）：审批 pending 未决定期间，后续带 id 请求必须已得到应答（哨兵文件断言）——退回串行处理时哨永不出现。
- `codex_app_server_setup_answers_reverse_requests`（L1）：setup 期间收到 id=99 反向请求必须得到 -32601 应答（哨兵文件断言）。

### 13.4 验证

- `cargo test -p r-code-host --lib`：**355/355 通过**（含 2 个新测试；此前四 crate 全量 353+149+80+22 全绿，本轮改动集中在 host）。
- `cargo clippy -p r-code-host --all-targets -- -D warnings`：通过。
- `cargo fmt --all -- --check`：通过。
- 复跑中出现过的 `gateway::tests::exhausted_transient_retries_record_only_the_final_failure` 单次失败为 tracing subscriber 跨并行测试串扰的既有脆弱断言（单独 3/3 通过、全量重跑通过），与本 diff 无关。

### 13.5 结论

§12 的全部 HIGH/MEDIUM 均已修复并验证，**当前无已知阻断项**。保留的已知限制（非阻断）：L3 孤儿 run 行由 310s bounded join + 启动恢复兜底；完整真实 Codex CLI transport E2E 未纳入自动化（fixture + 纵向 handler 测试覆盖）。

*第五轮执行记录：逐条代码对照（落点见 13.1/13.2）；`cargo test -p r-code-host --lib`（355/355）；`cargo clippy -p r-code-host --all-targets -- -D warnings`（通过）；`cargo fmt --all -- --check`（通过）。*

---

## 14. 发布前最终复核（2026-08-06）

本轮不以 §13 的“已完成”作为前提，而是由主审查者与两个独立子审查者重新检查当前全部 diff、并发/取消边界、前端持久化以及发布链路。审查中发现的问题均先修复再重跑门禁，不仅复述旧文档。

### 14.1 对最初四个用户问题的最终答复

| 用户问题 | 0.3.0 最终状态 |
|---|---|
| Codex 主 Agent 请求 R-Code 子代理却新建 session | **已修复**：hosted App Server 只禁用 legacy `mcp_servers.r-code`，保留其他用户 MCP；动态工具在原 task / parent run 树下建 child run，不新建顶层会话。 |
| 审批型子代理报 `tool 'bash' is not available` | **已修复**：子代理权限为只读 / 需审批 / 完全访问三态；需审批时 Bash/写工具可见，但必须经 Gateway 审批。子 Codex 进程固定使用 `read-only` sandbox + `on-request` approval。 |
| 运行计时器长时间不刷新 | **已修复**：共享时钟约每秒更新，订阅下沉到 memo 计时组件，不会迫使整条 Timeline 每秒重渲染。 |
| Codex CLI 思考过程能否嵌入 | **以可安全获取的边界完成**：UI 只显示并持久化 Codex 公开 reasoning summary；原始隐藏思维链不获取、不展示、不落盘。 |

同期要求的文件引用也已落实：回复中的工作区相对 Markdown 链接先经可信 Host 解析，点击后在右侧 Files 打开，并传递 line / column 定位。

### 14.2 本轮新发现并补修的问题

| 问题 | 修复与回归保证 |
|---|---|
| 审批请求的 pending / decision / standing rule 分属多把锁，取消、超时、hard-drop 与 `AllowAlways` 可竞态 | 改为单一 `PermissionState` 原子转移；RAII lease 在 future drop 的同步瞬间标记失效，迟到的决策无法生成 standing rule。 |
| Stop 或父运行自然完成与显式委派竞态，可创建 ended-parent ghost child；旧收尾还可覆盖新 run 状态 | Stop / delegate / natural completion 共用 task-local bridge 启动边界；`closing_run_id` 在终态 DB 持久化前不清除，迟到 steer / SendNow 转为持久队列。回归同时覆盖“child 先赢”与“completion 先赢”。 |
| 运行中可切换 workspace，使权限与物理路径边界漂移 | Task 处于 Exploring / InProgress 或存在活跃主 run 时拒绝重绑 workspace，原绑定保持不变。 |
| App Server 虽有单行 32 MiB 上限，但 64 格 stdout 队列的理论原始占用约 2 GiB | stdout 队列降为 2 格，编译期断言原始排队预算不超过 64 MiB；在途请求与 writer 也有独立硬上限。 |
| Windows updater 的 generic / `-msi` 键都指向 en-US MSI，zh-CN 资产无正确平台映射 | 固定 generic=en-US MSI、`-msi`=zh-CN MSI、`-nsis`=setup；validator 要求三个唯一 Windows 资产完整覆盖。 |
| 四个 matrix job 并发读改写 `latest.json` 可丢平台键 | matrix 只上传产物和签名，由单一 finalize job 生成并交叉验证唯一 manifest。 |

### 14.3 最终自动化门禁

- `cargo fmt --all -- --check`：通过。
- `cargo test --workspace --all-features`：全工作区通过，0 failed；其中 Host 当前 372 项 lib 测试、Gateway 163 项全量测试通过。
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`：通过。
- 前端 `npm test`：69/69 通过；`npm run build`：TypeScript + Vite 生产构建通过。
- `node --test scripts/release.test.mjs`：25/25 通过；`node scripts/release.mjs check v0.3.0`：所有版本一致为 0.3.0。
- `cargo audit`：退出码 0，仅报告 18 个已允许的上游 GTK/未维护 warning；`cargo deny check`：advisories / bans / licenses / sources 全部通过（仅依赖重复警告）。
- `node scripts/generate-supply-chain.mjs target/supply-chain --strict`：生成 756 个 CycloneDX / license 组件，严格模式通过。

首轮 PR 远端 CI 额外暴露了两个只在调度/平台差异下出现的测试竞态，均先复现根因再修复：

- macOS 会立即拒绝不可达端口，使第一个动态子运行可能在 registry 轮询前结束；并发测试现改用受控本地 OpenAI-compatible SSE 服务，在主槽与两个 child 槽同时可见后才统一释放响应。测试仍经过真实 handler、Provider runtime、DB 和 registry，不以放宽超时替代并发证明。
- Linux headless 前端在启动器 DOM 已可见、下一帧焦点迁移尚未执行的窗口立即发送 Escape，按键会落到已卸载的旧标签；测试现先验证焦点确已进入启动器，再验证 Escape 恢复同一子代理详情。该等待条件就是产品的可访问性契约，焦点回归仍会超时失败。
- 修复后本地全量门禁再次全绿；原失败的 Rust 并发用例另连续运行 10 次、前端用例连续运行 5 次，均全部通过。新的 Windows / macOS / Linux CI 是合并与打 tag 的硬闸门，任一 job 未通过都不会发布。

### 14.4 Windows 真实打包与运行冒烟

最新源码在 Windows x64 本机成功重建主程序、NSIS、品牌安装器与双语 MSI：

| 产物 | 字节数 | SHA-256 |
|---|---:|---|
| `target/release/r-code-host.exe` | 23,437,824 | `DBF5E3C525A990C6746DC505C9F4D59FA014649647A49FD13AD3F3FB2889EE38` |
| `R-Code_0.3.0_x64-setup.exe` | 7,336,102 | `939D3A85F85E3C677223A32963D1F2067FEB570890C0A44FC82533AAB2393BA2` |
| `R-Code_0.3.0_x64-installer.exe` | 12,188,894 | `98A386A307AE798D7748DFC4E4E2D798AF199EEF71454DEF0883DBB3233F757B` |
| `R-Code_0.3.0_x64_en-US.msi` | 10,387,456 | `1D8293F58ADE89AF3394D1B9F340396C0A7562D6ADCC7919F7E94BF0BDC81FB7` |
| `R-Code_0.3.0_x64_zh-CN.msi` | 10,383,360 | `737D10BEFDECFDFDF3BDAE3091AC0C4E766726CD9A9BAFB5064B03BE2797E26D` |

直接启动该 Release `r-code-host.exe`，保持真实 Windows `USERPROFILE`，并临时覆盖 `APPDATA` / `LOCALAPPDATA` 以减少一般子进程缓存污染：进程在 12 秒观察窗口内持续存活，随后只终止本次启动的精确 PID。这是启动冒烟，**不声称应用数据完全隔离**：Tauri 在 Windows 上的 `app_data_dir()` 通过 `SHGetKnownFolderPath(FOLDERID_RoamingAppData)` 解析，而不是只读这两个环境变量。如需严格数据隔离，应使用临时 Windows 用户、Windows Sandbox 或 VM。

首次冒烟脚本曾同时把 `USERPROFILE` 指向一个空目录，结果 Tauri setup 报 `unknown path` 并以 Windows fast-fail `0xC0000409` 退出。对照试验确认：同一二进制只在这个不完整的伪 Windows profile 下失败，保留真实 Known Folders 后立即通过。这是冒烟 harness 的环境构造错误，不是 0.3.0 产物崩溃；未为掩盖它而修改产品代码。

### 14.5 已知验证边界与最终结论

唯一保留边界是：需要真实登录态和可用 Provider 的 Codex CLI ↔ App Server 完整端到端会话尚未纳入无凭据 CI。当前已由 fixture transport、真实进程 shim、handler 纵向测试、持久化测试和 UI 测试覆盖协议与产品边界。它是发布后/有登录环境的运维验收项，不是当前可重现的缺陷。另外，“Known Folder 缺失时提供更友好错误”或“增加专用数据目录覆盖”可作为后续非阻断加固；产品当前未声称支持伪造 portable profile 或未加载 profile 的服务账户。

**最终结论：PASS。** 截至 2026-08-06，当前工作树无已知 P0/P1 发布阻断，0.3.0 的源码、数据迁移、权限边界、前端交互、发布脚本、Windows 打包与启动冒烟均已通过。
