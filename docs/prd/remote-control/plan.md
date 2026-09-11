# R-Code 远程控制计划

> 状态：`draft`（计划评审中，未开始实施）
> 发布日期：2026-09-10
> 前置基线：Harness v2 全闭环（commit `6fbf7fd`）——聊天链路统一为 前端 → r-code-client → r-code-service → Harness 插件。
> 任务权威源：[tasks.json](./tasks.json)；架构说明：[architecture.md](./architecture.md)。
> **AI 执行**：[worklist.md](./worklist.md)（任务卡/验收/恢复协议）+ `node scripts/verify-remote.mjs`。

## 1. 目标、终态与范围

让用户在**另一台设备**（手机、平板、另一台电脑）上观察和控制本机运行的 R-Code 任务：查看进行中的 run 与事件流、继续对话、中止 run、批准工具审批、管理排队消息。

完成标志：

1. 用户在桌面端发起一次**显式配对**（扫码或手动输入配对码），手机浏览器打开控制页（PWA，可加主屏）即可在同一局域网内连接本机 daemon；
2. 手机端实时看到任务事件（assistant 回复、工具调用、run 状态），可以发送消息、取消 run；
3. 工具调用的审批在手机端可批准/拒绝（能力可单独关闭；依赖 R0 的持久审批通道先行落地）；
4. 每台已配对设备有独立令牌，可在桌面端列出和吊销；daemon 默认不监听任何网络地址，配对是唯一开启远程口的方式；
5. 可选里程碑：通过自托管中继在公网（NAT 外）访问，端到端加密，中继看不到明文。

**不做（明确排除）**：

- 不做官方托管云服务、不做账号体系（首期）；
- 不做原生 App 上架（PWA 覆盖；原生留待品牌/推送需求明确后另立计划）；
- 不做远程执行任意 shell 的通用 SSH 替代——远程能力严格限定在 daemon 既有命令面；
- 不做多用户协作/共享会话（一台机器一个 owner，多设备是同一人的多个终端）；
- 不做远程插件安装（插件管理仅本机；远程可看不可装）。

## 2. 现状事实与复用边界

Harness v2 已把“客户端”与“执行面”切开，这是本计划能低成本成立的前提：

- **命令面已中立**：`r-code-service` 的方法为宿主无关的 JSON-RPC（`ApplicationCommand{client_id, command_id, method, params}`），持久去重回执（`CommandDedup`：同 id 重放原结果）。聊天所需方法齐备：`task.create/sendMessage/cancel/list/detail/rename/clone/branches/events`、`models.available`、`settings.get`、`plugins.list`。
- **事件面已中立**：`task.events`（afterSeq 游标）输出 `EventEnvelope{seq, task_id, run_id, kind, source, payload}`；前端轮询即可投影全部 UI 状态。
- **客户端**：`r-code-client::DaemonClient`（ensure_daemon 探活拉起 + token 握手 + 帧读写），TUI 的 `engine.rs` 与 GUI 的 `harness_v2_chat.rs` 都是薄客户端，证明“换一个 transport 就能再挂一个客户端”。
- **唯一的本机约束**：daemon 只绑命名管道（Windows，`share_mode(0)` 独占）/ Unix socket（0600），`ProfileLock` 保证单 owner。**无网络监听、无设备认证、无能力分档**——这是本计划的全部核心新增。
- **可复用范式**：OAuth device flow 已在 `codex.startLogin {mode:"device"}` 落地（一次性码、短 TTL、新终端确认）；配对 UX 与安全模型照搬。
- **前端可复用**：GUI 是 React + Tauri WebView；远程控制台是同一套投影/组件 + 一个 WebSocket client（替代 `invoke("cmd_*")`），不重写状态机。

## 3. 架构与数据流

### 3.1 传输分层

```
桌面 daemon (r-code-service)
 ├── IpcTransport（现状）
 │    ├── Windows named pipe / Unix socket（本机，不变）
 │    └── ── TCP+TLS / WebSocket（新增，仅配对后监听）
 │              绑定 127.0.0.1 之外的网卡时必须：显式开启 + 设备令牌 + TLS
 └── ApplicationHandler（同一方法面，不分本地/远程）
        │
        ▼  CommandDedup / ApplicationService / RunManager（零改动）

手机 PWA ──wss://──► RemoteListener ──► 同一条命令管道
```

关键点：**远程不是第二套服务**。RemoteListener 只是 `IpcListener` 之外的另一个连接来源，握手后产生同样的认证身份，进入同一个 `ApplicationHandler`。这保证本地/远程行为一致、去重回执一致。

### 3.2 配对流程（扫码）

1. 桌面端命令/界面：`remote.enablePairing`（仅本机管道可调用）→ daemon 生成一次性 `pair_secret`（≥128 位熵），TTL 120 秒，**不落 journal**，只存内存；同时打开临时发现端口（或复用远程监听口的“配对模式”）。
2. 桌面显示 QR：内容 `rcode://pair?host=<lan-ip>&port=<n>&tok=<pair_secret>`（mDNS 可选：daemon 广播 `_rcode._tcp.local`，QR 可只带 token）。
3. 手机 PWA 扫码 → 与 daemon 建立 TLS 连接 → `device.pair {pair_secret, device_name, platform}`。
4. daemon 校验（一次性、未过期、未用过）→ 生成长期**设备令牌**（每设备独立，存 v2 持久区 `devices/`，复用平台凭据存储加密），返回令牌 + profile 信息；随后 pair_secret 立即失效，发现口关闭。
5. 之后手机每次连接：mTLS 或 bearer 设备令牌 + `client_id`（= 设备 id，正好复用命令去重的 client 维度）。

手动配对（无摄像头/跨网段）：桌面显示 8 位分组配对码，手机手动输入 host:port + 码，流程相同。

### 3.3 能力分档（远程连接的命令授权）

设备令牌带能力位，握手时声明、daemon 强制（不是前端自觉）：

| 能力 | 方法面 | 默认 |
| --- | --- | --- |
| `events:read` | `task.events/list/detail/branches`、`models.available`、`plugins.list` | 开 |
| `tasks:write` | `task.create/sendMessage/cancel/rename/clone` | **配对时询问，默认关** |
| `approvals:decide` | 审批 pending op 的批准/拒绝 | **默认关**（最敏感：等于放行工具执行）；**硬前置 R0 完成**，否则该能力位永不出现 |
| `settings:write` / `plugins:write` | 设置与插件管理 | **永不授予远程**（首期硬拒） |

命令分发处按 `(client_id → capabilities)` 校验；无能力方法返回结构化 `PermissionDenied{required_capability}`，不静默降级。

### 3.4 事件推送（省电）

现状轮询在局域网上可用但费电。RemoteListener 对远程连接提供两种模式：

- **SSE / WebSocket 推送**：daemon 在 journal 有新事件时向已认证连接扇出（复用 RunManager 的观察抽头模式，新增一个 per-connection 订阅者；游标恢复仍走 `task.events`，断线重连不丢事件——seq 单调）；
- 轮询保留为降级路径。

### 3.5 公网中继（原生 App 的外网通道，必需）

家宽无公网 IP，原生 App 在 4G/外网 Wi-Fi 下必须经中继（详见 [relay.md](./relay.md)）：App 与 daemon 均出站 443，中继只转 E2EE 密文。R5 是 R6 的前置；不配置中继时 App 仅局域网可用，产品不内置官方运营中继。

### 3.6 威胁模型与红线

- **默认面为零**：不配对就没有任何网络监听；配对码短 TTL、一次性；远程口可一键全局关闭（关闭后所有设备令牌失效）。
- **传输必加密**：局域网同样要求 TLS（自签证书指纹在配对 QR 里钉死，TOFU 一次）；禁止明文远程口。
- **最小权限**：远程默认只读；写/审批逐项授权；插件安装/凭据读取永不可远程。
- **审批权威不破坏**：v2 已有“审批只认宿主创建的 pending op”。远程批准只是新增一个**决策来源**，op 的创建、作用域、单 op 语义不变；批准动作记审计事件（哪个设备、何时）。
- **凭据不出机**：`settings.apply` 不可远程；手机端不接触 provider API key。
- **会话劫持面**：设备令牌被盗=该设备能力上限内的风险；因此高敏能力默认关 + 可一键吊销 + 批准动作可要求二次确认（桌面通知联动）。
- 绑定范围：RemoteListener 初始只绑配对时选定的网卡（默认仅局域网私网段），不绑 0.0.0.0。

### 3.7 客户端形态：原生 App（终态）与 PWA（开发通道）

- **原生 App（终态，R6）**：React Native 出 iOS + Android。网络/加密/配对/投影全部复用 TS core（RN 与 Web 共用，平台仅适配 WebSocket、相机、Keychain/Keystore、推送）。执行永远在 daemon/插件侧：App 不含模型调用、工具执行、脚本引擎，也不运行下载代码（iOS 3.3.2/2.5.2 合规姿态：用户自有主机的远程终端，配对=显式主机授权）。
- **PWA（R1–R4）**：daemon `/app` 托管的独立 Vite entry，复用桌面前端投影；用于协议联调/内测/免安装体验；保留但非正式分发形态。
- 两形态共用同一套任务/会话/审批 UI 状态机；无账号、无多 profile 切换（配对绑定一个 profile）。

## 4. 分期

- **R0（主链前置：v2 审批通道，3 任务 RA1–RA3）**：远程审批暴露的是一个主链路既有缺口——协议有 `host.approvals.request/reply`、router 有内存 ApprovalRegistry，但 RunManager 目前喂的是 IgnoreQuestions 占位，op 不持久化、不外发事件、无决策方法面。R0 把它补齐：pending op 持久化 + `approval.requested/decided` 事件（RA1）、daemon `approvals.list/decide` 方法（RA2）、RunManager 真实接线并让 TUI 本地审批先绿（RA3）。**这是远程能力 approvals:decide 的硬前置，且独立于远程控制本身就有产品价值**（插件工具授权目前事实上无 UI 闭环）。
- **R1（局域网只读 PoC）**：transport 抽象 + TLS/WS listener + 配对码（先手动输入，不做 QR）+ 设备令牌 + 事件推送；PWA 能看任务列表与实时事件。
- **R2（完整局域网控制）**：QR 配对 + mDNS 发现 + `tasks:write` + 取消；PWA 发送/中止；设备管理（列出/吊销）。
- **R3（远程审批）**：`approvals:decide` 能力 + 手机审批卡片 + 审计；桌面端能力授予 UI。
- **R4（打磨与打包）**：PWA manifest/图标/离线壳、通知（完成/待审批）、自签证书钉扎的首次配对 UX、文档。
- **R5（公网中继，支持自托管 VPS）**：见 [relay.md](./relay.md)——用户自有云服务器作为中继的标准部署（r-code-relay 二进制/容器、owner 注册、E2EE 握手、RelayTransport、配置 UX、运维物料，任务 R16–R20）；不依赖 Tailscale 等组网，daemon 与 PWA 均出站 443。**不内置官方运营中继**（除非另立项目）。

## 4.5 原型与实现基线

手机端交互原型：[prototype.html](./prototype.html)（v1，2026-09-12 自查修正）。实施时四屏为验收基线，不是“参考”：

1. **配对屏**：QR 扫描态、证书指纹明示、端到端加密/默认只读两条安全说明、手动配对码入口；
2. **任务列表**：只用 `task.list` 真实字段（title/state/running/updatedAt），工具数/usage 等 detail 字段不许伪造到列表行；
3. **会话屏**：事件流投影、工具调用卡、排队提示、中止/发送；**远程审批卡是 R3 核心**，但它依赖一个当前尚不存在的前置——daemon 把 `host.approvals` 的 pending op 以外发事件（现状 router 用 IgnoreQuestions 占位、审批事件未接线），该接线显式归入 R12，能力在此之前不得授予；
4. **设备设置**：三档能力开关、设备吊销、局域网/中继接入。

视觉沿用桌面 obsidian 签名皮肤（#181818/#f4742b、同一间距/字阶/圆角刻度）。待 v2 原型：审批聚合 tab 的双端二次确认态、diff 展开、离线全屏态、Web Push、中继 owner 注册码页。

## 5. 测试与验收原则

- 安全测试必须是真实的：未配对连接拿不到任何数据；过期/重放配对码被拒；吊销令牌立即失效；无能力方法被硬拒（非前端隐藏）；明文口不存在。
- 协议一致性走既有模式：新增方法进 verify 脚本守卫；远程与本机对同一命令的去重行为一致（command_id 跨 transport 重放返回同结果）。
- 事件可靠性：杀 PWA、断网重连后 afterSeq 游标恢复，不丢不重（seq 单调 + journal 持久）。
- 平台：Windows/macOS/Linux 三端监听与配对均覆盖；防火墙提示走平台惯例（Windows 首次监听弹系统授权）。
- 禁 mock 红线延续 Harness v2：传输/配对用真实 TLS 连接测试，证书与令牌用真实生成链。

## 6. 文档与归档

- 新增 `docs/support/guides/remote-control.md`（用户向：如何配对、手机使用、安全含义）；
- 本计划完成后状态与取证进 `progress.md`；若 R5 中继落地，中继运维另出 ops 文档；
- 不改动 Harness v2 既有协议文档；远程是传输/认证层，插件协议无感知。
