# 远程控制 — AI 实施工作清单（Execution Contract）

> 状态：`frozen`（长任务化转换完成，等待首次实施）
> 转换契约：prd-to-ai-worklist v1.1.0 / ai-worklist-contract-norm.v1
> 规范输入：[plan.md](./plan.md)、[architecture.md](./architecture.md)、[relay.md](./relay.md)、[prototype.html](./prototype.html)、[tasks.json](./tasks.json)
> 统一验收入口：`node scripts/verify-remote.mjs`；任务包目录：`artifacts/ai-tasks/`
> 取证基线：commit `91a18a8`（Harness v2 全闭环后）

---

## 执行导航（新会话从这里开始）

1. 首次执行：读本文件「§0 执行协议」「§2 冻结决策」+ 编号最小的 ready 任务卡。
2. 续跑：读 `artifacts/ai-tasks/current.yaml` + 该任务卡 + 已归档证据；先对已完成断言跑最小 smoke。
3. 不要通读全部任务卡；未完成任务的细节按需加载。
4. 每个可验证子步后更新 current.yaml；任务通过后归档 evidence 并勾选 §6 唯一 Checklist。

---

## §0 执行协议

固定循环：`preflight → 选编号最小且依赖已满足的 MUST → 建/恢复任务包 → 实现 → 任务断言 → 累计门禁 → 归档证据 → 勾选 → 下一项`。

- **测试全部前台执行**，日志写 `target/test-logs/<task-id>.log`（禁止 `| tail` 隐藏进度；参考仓库既有约定）。单命令超时 ≤10 分钟。
- 不等待人工确认：里程碑出口、文档更新、测试通过都不是暂停点。
- 失败处理：定位根因 → 聚焦修复 → 复跑 → 换受约束方案；同方案无进展不重复微调。
- 允许真正中断的条件：需要用户未授权的权限扩张/不可逆生产动作/无法获取的真实凭据（且无 adapter/fake 可验证）。
- **禁止**：删测试/降阈值/缩断言范围/把缺断言当通过来修绿；禁止 reset 工作区既有改动。
- 外部放行（真实云服务器部署、真实手机网络）标 `production_release_ready`，不阻塞 `implementation_verified`。

### Preflight（每次会话开始）

```bash
git rev-parse --short HEAD
cargo check --workspace --all-targets     # 基线必须绿
node scripts/verify-remote.mjs --list     # 断言注册表可读
```

---

## §1 终态与完成层级

**Definition of Done（implementation_verified）**：

- 手机 PWA 可经配对连接本机 daemon（局域网）与自托管中继（公网），完成只读观察、发送/取消、审批三类能力；
- 未配对状态下机器不存在任何远程监听口；所有远程调用受设备能力位强制（非前端隐藏）；
- R0 审批通道同时让 TUI/本地审批闭环；
- 统一验收 `--through R5 --profile implementation` 退出码 0；
- `cargo clippy --workspace --all-targets -- -D warnings` 0 警告；`cargo fmt --all -- --check` 0；
- 前端构建（含 remote entry）通过；文档与实现一致。

**production_release_ready（外部放行，AI 不自称完成）**：真实 VPS 部署 relay + 域名/证书、真实 4G 外网配对与连接取证、防火墙提示三平台手测。

**明确不做**：官方运营中继/账号体系、原生 App 上架、多用户协作、远程插件安装/凭据读取/设备管理、远程任意 shell 通用化。

---

## §2 冻结决策（不可变约束）

| ID | 决策 |
| --- | --- |
| F1 | 远程是 daemon 的**第三个 transport**（管道/Unix socket/局域网 TLS/中继），同一 ApplicationHandler，不建第二套服务 |
| F2 | 默认面为零：不配对不监听；最后一台设备吊销后监听自动关闭；永不绑 0.0.0.0（仅配对选定网卡的私网地址） |
| F3 | 远程传输必须 TLS；自签证书指纹经 QR 钉死（TOFU 一次），指纹不符硬拒，无“继续访问”绕过 |
| F4 | 配对码一次性、≥128 位熵、TTL 120s、内存态不持久化；设备令牌 32 字节随机，只存 SHA-256 |
| F5 | 能力三档：`events:read`（默认）/`tasks:write`/`approvals:decide`（默认关）；`settings/plugins/device/service.*` 任何组合都不可远程（分发器硬编码拒绝） |
| F6 | 设备能力在 daemon 侧强制，按 client_id（=device_id）；无能力返回结构化 PermissionDenied{required_capability} |
| F7 | 命令去重复用 CommandDedup（(profile, client_id, command_id)），跨 transport 行为一致 |
| F8 | 事件推送与 task.events 逐字节同形（EventEnvelope，seq 单调）；断线 after_seq 重放不丢不重 |
| F9 | R0 审批：op 由宿主创建、journal 持久化、重启可重建；远程批准只是第二决策来源，不改变审批语义 |
| F10 | 凭据不出机；PWA 不缓存任务正文（SW 只缓存壳）；中继无会话密钥（E2EE），被攻破最坏=断网 |
| F11 | 视觉沿用桌面 obsidian 签名皮肤（#181818/#f4742b，既有间距/字阶/圆角刻度）；prototype.html 四屏为验收基线 |
| F12 | 中继是独立小二进制（crates/r-code-relay/），不属于 r-code-service；daemon/PWA 均出站 443 |
| F13 | 不引入 tauri/新原生依赖到 r-code-client/协议层；PWA 静态资源由 daemon 自身 /app 托管 |

### 决策空间（AI 可自行决定，按 安全>简单>一致>可测试>易回滚 排序）

WS 帧库选择（优先 tokio-tungstenite，已是间接依赖时直接用）、证书生成库（rcgen 优先）、mDNS 库、前端状态管理细节、组件拆分、PWA 图标生成方式、relay Web 框架（axum 优先）、测试中端口分配（用 0 随机端口）。

---

## §3 仓库事实基线（commit 91a18a8 已核实）

- 现有 IPC：`crates/r-code-runtime/src/ipc.rs` `IpcListener`（Windows named pipe `first_pipe_instance` 独占 / Unix 0600 socket）；连接读循环在 `src/daemon.rs serve_connection`，经 `CommandDedup`（`application_receipts.rs`）到 `ApplicationHandler`。
- 命令/事件：`r-code-harness-protocol::application::{ApplicationCommand{client_id,command_id,method,params}, ApplicationResult, ApplicationFrame}`；`DaemonClient`（crates/r-code-client/src/lib.rs）持有帧连接。
- 审批现状（R0 缺口的精确位置）：`plugins/router.rs` 有 `ApprovalRegistry{decisions: Mutex<HashMap>}`（内存、只有 set/decide）、`QuestionSink` trait + `IgnoreQuestions`（RunManager:354 实际挂的是它）；wire 已有 `ApprovalsRequest{pending_operation, summary}` / `ApprovalsReply{decision}` 与 `operations::PendingOperationRef`；`host.approvals.request` 分支在 router.rs:383。
- journal：V2Store（crates/r-code-store/src/v2/）save_task_and_events/read_events；事件经 `run_manager::envelope_of` 映射；EventFanout 需要新建（参考 RunManager 的 host_observations 抽头模式）。
- 设备/设置：v2 profile 根 `profile.harness_v2_root()`；凭据走 `services/settings_store.rs`（平台 SecretStore）。
- 前端：React + Vite + TS（src-tauri/frontend），现有 invoke 层 src/lib/ipc.ts；projection 在 harness_v2_chat.rs（Rust 侧 DTO 投影可参考其事件→AgentEvent 映射）。
- 验证基建：`scripts/verify-harness-v2.mjs`（execFileSync + steps + guard 模式）、PTY/daemon 测试 helper 模式在 `crates/r-code-tui/tests/daemon_common/`（kill_stale_target_daemens/DaemonGuard/真守护进程 staging）可直接借鉴。
- 三平台：Windows x64/macOS arm64+x64/Linux x64；TLS/mDNS/防火墙行为必须平台分支验证，不许只在 Win 测就宣称跨平台。

---

## §4 需求追踪

| Requirement | 任务 | 关键断言 |
| --- | --- | --- |
| REQ-1 默认无远程面 | R04, R14 | R04.A2 未配对 netstat/端口扫描无监听；R14.A1 安全守卫 |
| REQ-2 扫码/配对码配对 | R02, R09 | R02.A1 一次性/TTL/重放拒绝；R09.A1 QR 与手动等价登记 |
| REQ-3 证书钉扎无绕过 | R03 | R03.A2 指纹不符硬拒 |
| REQ-4 设备令牌与能力强制 | R01, R04 | R01.A1 无明文存储；R04.A3 越权方法 PermissionDenied |
| REQ-5 实时事件不丢不重 | R05 | R05.A1/A2 扇出+after_seq |
| REQ-6 手机只读观察 | R07,R08 | R08.A1 真 TLS 读到任务事件 |
| REQ-7 发送/取消 | R10 | R10.A1/A2 有能力可写/无能力拒绝/排队语义 |
| REQ-8 远程审批 | RA1,RA2,RA3,R12 | 端到端 request→事件→decide→插件继续 |
| REQ-9 设备管理/吊销 | R11 | R11.A1 吊销立即断连且重连被拒 |
| REQ-10 PWA 可安装/移动端可用 | R07b,R13,R14 | manifest/安全区/命中区/真实移动视口 |
| REQ-11 公网中继 E2EE | R16–R20 | R17.A2 抓包密文；R19.A2 吊销在 daemon 侧生效 |
| REQ-12 凭据/插件不可远程 | R04 | R04.A3 硬编码拒绝清单 |

---

## §5 里程碑出口

| 里程碑 | 累计命令 | 出口不变量 |
| --- | --- | --- |
| R0 审批通道 | `node scripts/verify-remote.mjs --through R0` | 审批 op 持久化+事件+daemon 决策面；TUI 本地审批闭环；无远程监听口 |
| R1 局域网只读 | `--through R1` | 默认无口；配对+TLS+只读 PWA 闭环（手动配对码即可） |
| R2 局域网控制 | `--through R2` | QR/mDNS+写能力+设备管理 |
| R3 远程审批 | `--through R3` | 手机批准/拒绝真实工具调用 |
| R4 打磨 | `--through R4` | 通知/打包/CI 守卫/用户文档 |
| R5 公网中继 | `--through R5` | 自托管 relay 端到端；实现验收在本地 loopback+伪造 TLS 完成，真实 VPS 归外部放行 |

---

## §6 主 Checklist（唯一完成状态源）

- [ ] **RA1** 审批 pending op 持久化与事件外发
- [ ] **RA2** daemon 审批决策方法面
- [ ] **RA3** 审批接线进 RunManager（本地闭环）
- [ ] **R00** 传输抽象
- [ ] **R01** DeviceRegistry 与能力模型
- [ ] **R02** 一次性配对会话
- [ ] **R03** 自签证书与指纹钉扎
- [ ] **R04** RemoteListener（WS over TLS）
- [ ] **R05** 事件长连接扇出
- [ ] **R06** r-code-client WebSocket transport
- [ ] **R07** PWA 只读控制台（列表/事件）
- [ ] **R08** 手动配对端到端（PoC 收口）
- [ ] **R09** QR 配对与 mDNS 发现
- [ ] **R10** tasks:write 远程写路径
- [ ] **R11** 设备管理（桌面端）
- [ ] **R12** approvals:decide 远程审批
- [ ] **R07b** PWA 四屏交互实现（原型基线）
- [ ] **R07c** 审批聚合 tab
- [ ] **R13** 通知与移动端打磨
- [ ] **R14** 防火墙/安装/文档
- [ ] **R15** 中继接口冻结门
- [ ] **R16** r-code-relay 中继二进制
- [ ] **R17** RelayTransport（daemon 出站+E2EE）
- [ ] **R18** 桌面端中继配置 UX
- [ ] **R19** PWA 经中继配对与连接
- [ ] **R20** 中继部署物料与安全验收

进度：0/23 MUST 完成（RA 3 + R 20；R15 为设计门）。下一执行项：**RA1**。

---

## §7 任务卡

> 断言 ID 在 `scripts/verify-remote.mjs` 的 ASSERTIONS/COMMANDS 注册表登记；实现任务时把命令映射填上（cargo-test/node-test/script）。证据路径 `artifacts/ai-tasks/evidence/<id>.yaml`。

### RA1 审批 pending op 持久化与事件外发

- **结果**：插件调 host.approvals.request 后，op 落 journal（approval.requested 事件）；决策落 approval.decided；daemon 重启后 pending op 可重建；插件在决策前阻塞、决策/超时后继续。
- **需求引用**：REQ-8、F9；架构 §6、tasks RA1。
- **依赖**：无（最先做）。
- **前置事实**：router.rs:383 的请求分支当前直接查内存 registry；ApprovalRegistry 无持久化；envelope_of（run_manager.rs）是事件映射单点。
- **固定约束**：op 只能宿主创建（PendingOperationRef 不接受插件自造 id 这一既有校验保留）；事件与 assistant.message 同通道同形；无人连接时保持 pending 不自动拒绝；重复/迟到决策拒绝（复用 kernel 既有裁决门）。
- **决策空间**：op 存储可新建 v2 表（migrations 递增）或复用 events 投影重建——优先“事件投影+内存索引”（无新表、重启天然一致），若查询性能需要再加表；超时策略（默认值与可配置项）自行定，先 300s 默认。
- **产物**：router/registry 改造、journal 事件 kind（approval.requested/decided 加入 envelope_of 映射）、`crates/r-code-runtime/tests/ra1_persistent_approvals.rs`。
- **步骤**：①只读核实 PendingOperationRef 校验与 ApprovalsReply 流；②定义事件 payload（op_id/summary/runId/created_seq；decided 带 decision/decided_by_client）；③ApprovalRegistry 加持久句柄（store Arc）+ pending 列表/set_decision 写事件，启动时从 journal 重建；④router 请求分支：op 不存在→写 requested 事件并注册等待通道（tokio::sync::oneshot/watch），决策到达前 await（有超时）；⑤问题/审批统一（QuestionSink 与 approvals 的关系在本任务澄清：保留 questions 作为通用问题，审批走 approvals；RunManager 的 IgnoreQuestions 替换见 RA3）。
- **断言**：
  - RA1.A1（integration）真插件或 router 直调：request→journal 出现 approval.requested（payload 字段齐全）；decide 后 approval.decided；插件 await 返回 Granted/Denied。
  - RA1.A2（reliability）决策前杀 daemon 重启：pending op 从 journal 重建，op 仍可决策；重复 decide 第二次被拒。
  - RA1.A3（security-negative）插件提交自造 op id 被拒（既有门不回归）。
- **验证**：`node scripts/verify-remote.mjs --task RA1`
- **失败处理**：保留旧内存路径不破坏 t39 一致性测试；事件投影方案若无法表达等待语义，oneshot 等待放内存、状态放 journal（分层不混淆）。

### RA2 daemon 审批决策方法面

- **结果**：ServiceHandler 新增 `approvals.list`/`approvals.decide`；命令去重同既有面；决策审计含 client_id。
- **依赖**：RA1。
- **固定约束**：decide 参数 {operationId, decision:"granted"|"denied"}；未知 op→结构化错误 not_found，不创建任何状态；decide 幂等语义=同 op 同决策重放返回原结果（经 CommandDedup），异决策拒绝。
- **产物**：r-code-service.rs 方法分支、protocol application methods 常量（若 methods 模块有登记惯例则加）、测试。
- **断言**：RA2.A1 list 只返回 pending（decided 不出现）；RA2.A2 decide 唤醒 RA1 的等待并审计 client_id。
- **验证**：`--task RA2`

### RA3 审批接线进 RunManager（本地闭环）

- **结果**：RunManager 构造不再无条件 IgnoreQuestions：挂持久 registry/sink；TUI 经 daemon approvals.* 完成一次真实工具审批（本地链路先绿）。
- **依赖**：RA2。
- **固定约束**：无客户端决策时 run 等到超时（默认拒绝或可配，取安全默认=拒绝并记事件），不静默放行。
- **决策空间**：TUI 审批 UI 可复用既有 approval_overlay.rs（本地桥），engine.rs 增 approvals 轮询/事件驱动——优先事件驱动（approval.requested 弹浮层）。
- **产物**：run_manager.rs 装配改造、tui engine/approval 接线、`crates/r-code-runtime/tests/` 端到端（参考 conversation_engine.rs 的真插件 staging）。
- **断言**：RA3.A1 真插件请求审批→daemon 事件→TUI/客户端 decide→插件拿到结果继续（全链真进程）；RA3.A2 拒绝路径插件收到 Denied 且事件完整。
- **验证**：`--task RA3`；回归 `cargo test -p r-code-tui`、verify-harness-v2 full。

### R00 传输抽象

- **结果**：DaemonClient 与 daemon serve 侧可按 transport 注入；命名管道/Unix socket 行为零变化。
- **依赖**：无（可与 RA 并行，但 R04 需要它）。
- **固定约束**：不引入任何网络监听；现有 t06a/daemon_owner/t35 测试不允许改弱。
- **决策空间**：trait 形状（`AsyncRead+AsyncWrite` 帧流即可，不必过度抽象）。
- **产物**：ipc.rs/client 的 transport seam。
- **断言**：R00.A1（regression）现有管道/socket 全部连接/去重/owner 测试通过；无新监听口（静态守卫：无 TcpListener 新增）。
- **验证**：`--task R00`

### R01 DeviceRegistry 与能力模型

- **结果**：devices/registry.json（架构 §7）落盘；注册/令牌哈希校验/吊销/能力持久化；永不远程的方法清单在分发器可引用。
- **依赖**：R00。
- **固定约束**：令牌仅存 SHA-256；无明文落盘（测试断言文件内容不含令牌）；Capability 枚举三档；FORBIDDEN_REMOTE_METHODS 常量（settings.*/plugins.install|remove|setEnabled/device.*/service.shutdown）。
- **产物**：`src/remote/{registry.rs,capabilities.rs}`、单测。
- **断言**：R01.A1 注册往返+文件无明文令牌；R01.A2 吊销后校验失败；R01.A3 能力默认值正确（read 开/其余关）。
- **验证**：`--task R01`

### R02 一次性配对会话

- **结果**：pairingStart（仅本机 transport）→pair_secret（≥128 熵/TTL 120s/一次性/内存）→device.pair 颁发设备令牌。
- **依赖**：R01。
- **固定约束**：非本机连接调 pairingStart 被拒；pair 消费后 secret 立即失效；重启不保留。
- **产物**：`src/remote/pairing.rs` + 测试。
- **断言**：R02.A1 过期/重放/二次使用拒绝；R02.A2 颁发后 registry 有设备且返回一次性令牌；R02.A3 远程调用 pairingStart 拒绝。
- **验证**：`--task R02`

### R03 自签证书与指纹钉扎

- **结果**：首次配对启用时生成 CA/服务证书（私钥入平台 SecretStore），指纹进 QR；WS 仅 TLS。
- **依赖**：R02。
- **固定约束**：指纹不符硬拒（无绕过 API）；证书重启复用不重新生成。
- **产物**：`src/remote/tls.rs`、测试（rcgen 生成测试证书链）。
- **断言**：R03.A1 证书持久化+复用；R03.A2 错误指纹连接被拒（真 TLS 握手失败，非应用层假装）；R03.A3 明文口连接失败。
- **验证**：`--task R03`

### R04 RemoteListener（WS over TLS）

- **结果**：配对后在选定私网网卡监听；hello 帧认证；能力注入分发；最后设备吊销自动关监听。
- **依赖**：R03。
- **固定约束**：F2/F5/F6/F12；仅 RFC1918/链路本地绑定候选；未认证立即断连；FORBIDDEN 方法远程硬拒（不论能力）。
- **产物**：`src/remote/listener.rs`、daemon 装配、`tests/r4_remote_listener.rs`。
- **步骤**：listener accept→TLS→hello{device_id,token}→registry 校验→取 capabilities 包成连接上下文→复用 serve_connection 的命令处理（加能力中间层）。
- **断言**：
  - R04.A1 未配对时无任何 TCP 监听（loopback 也不开，配对临时口除外且随配对结束关闭）；
  - R04.A2 认证失败立即断连；同 command_id 跨 transport 重放返回同结果；
  - R04.A3 无能力方法/forbidden 方法均 PermissionDenied 或硬拒；settings/plugins/device/service 远程调用测试全拒绝；
  - R04.A4 吊销最后一台设备后端口关闭（连接被拒）。
- **验证**：`--task R04`

### R05 事件长连接扇出

- **结果**：events.subscribe{after_seq} 长连接推送；与轮询共存；背压关连。
- **依赖**：R04。
- **固定约束**：推送字节与 task.events 同形；1MiB/1000 帧背压（架构 §6）。
- **产物**：`src/remote/fanout.rs`（journal append 订阅者，挂在 save_task_and_events 同一写路径之后）。
- **断言**：R05.A1 subscribe 后实时收到；R05.A2 杀连接 after_seq 重连不丢不重（seq 连续）；R05.A3 慢消费者被断开而非阻塞 journal 写。
- **验证**：`--task R05`

### R06 r-code-client WebSocket transport

- **结果**：DaemonClient 可经 wss 连接（指纹+设备令牌注入），API 与管道版一致。
- **依赖**：R05。
- **产物**：`crates/r-code-client/src/ws.rs`。
- **断言**：R06.A1 真 TLS 往返 task.list/events；R06.A2 断线重连+command_id 重放原结果。
- **验证**：`--task R06`

### R07 PWA 只读控制台（列表/事件）

- **结果**：daemon /app 托管静态 PWA；配对后可见任务列表+实时事件；可加主屏。
- **依赖**：R06。
- **固定约束**：prototype 屏①②视觉基线；任务数据不入 SW 缓存；列表只用 task.list 真实字段。
- **产物**：frontend remote entry（vite 独立 config）、daemon 静态托管、mjs 测试。
- **断言**：R07.A1 WS 连接/投影/重连 mjs 测试；R07.A2 daemon 托管构建产物（404 未构建时诚实报错，不内嵌假 bundle）。
- **验证**：`--task R07`

### R08 手动配对端到端（PoC 收口）

- **结果**：真 TLS 局域网只读闭环（不经 QR）。
- **依赖**：R07。
- **产物**：`crates/r-code-runtime/tests/r8_remote_e2e.rs`（借鉴 tui tests/daemon_common 的 staging/清理）。
- **断言**：R08.A1 本机 pairingStart→另一 WS 客户端凭码+指纹连上并读到事件；R08.A2 未认证连接零数据；R08.A3 三平台监听编译/绑定测试（macOS/linux 用 CI，Win 本机；平台分支不可 cfg 死代码）。
- **验证**：`--task R08`

### R09 QR 配对与 mDNS 发现

- **结果**：QR（rcode://pair 载荷架构 §3）+ mDNS `_rcode._tcp.local`；桌面 GUI/TUI 两处显示入口。
- **依赖**：R08。
- **产物**：`src/remote/{qr.rs,mdns.rs}`、TUI `/pair`、桌面设置弹层（可只出 TUI 端，GUI 端在 R11 同做）。
- **断言**：R09.A1 QR 解析→连接与手动码登记等价；R09.A2 mDNS 广播/发现/消失。
- **验证**：`--task R09`

### R10 tasks:write 远程写路径

- **结果**：PWA 发送/取消；能力默认关、配对授予后可用；运行中发送=排队（v2 语义不新造）。
- **依赖**：R09。
- **固定约束**：写动作审计可追到 device_id（journal 扩展或事件字段，不破坏既有消费者）。
- **断言**：R10.A1 有权限 send/cancel 成功并产生既有事件；R10.A2 无权限 PermissionDenied；R10.A3 queued 响应与自动派发。
- **验证**：`--task R10`

### R11 设备管理（桌面端）

- **结果**：device.list/revoke/setListener/updateCapabilities（仅本机）；GUI 设置页 + TUI /remote。
- **依赖**：R10。
- **断言**：R11.A1 列表字段（名称/能力/最后连接）；R11.A2 吊销立即断连且重连拒绝；R11.A3 关闭监听使全部远程连接断开但设备不吊销。
- **验证**：`--task R11`

### R12 approvals:decide 远程审批

- **结果**：手机批准/拒绝落到 RA1 的 pending op；prototype 屏③审批卡。
- **依赖**：R11、RA3。
- **固定约束**：能力默认关；决策审计含 device_id；高敏动作“桌面二次确认”配置项（本任务实现配置与协议位，桌面联动可同卡）。
- **断言**：R12.A1 无 approvals:decide 能力时 decide 被拒；R12.A2 有权限时端到端审批真实工具调用（真插件）。
- **验证**：`--task R12`

### R07b PWA 四屏交互实现（原型基线）

- **结果**：prototype 四屏全部可操作（非静态）；底部三 tab；安全区/命中区/obsidian 皮肤。
- **依赖**：R07（在 R2 末并入，但卡片放这里明确交互债）。
- **断言**：R07b.A1 组件 mjs：列表真实字段/审批卡缺能力隐藏决策按钮/断网全屏重连；R07b.A2 移动视口（390×844）无横向滚动、命中区 ≥24px、底部安全区。
- **验证**：`--task R07b`（node --test + 视觉走查脚本，截图人工只作辅助证据）

### R07c 审批聚合 tab

- **结果**：跨任务 pending op 聚合、快速决策、“需桌面确认”态。
- **依赖**：R07b、R12。
- **断言**：R07c.A1 多 op 排序/决策后消失/重连 after_seq 不丢。
- **验证**：`--task R07c`

### R13 通知与移动端打磨

- **结果**：run 完成/待审批通知（Web Push 或前台通知分层实现）；图标/启动色/离线壳。
- **依赖**：R12。
- **固定约束**：不缓存凭据与任务正文；iOS PWA 推送限制若阻断 production，implementation 用前台通知+SW 后台同步短窗验证，并明确标外部放行。
- **断言**：R13.A1 前台通知触发；SW 缓存清单只含壳资源（mjs 断言缓存名单）。
- **验证**：`--task R13`

### R14 防火墙/安装/文档

- **结果**：docs/support/guides/remote-control.md 用户文档；verify 脚本 CI 守卫（loopback+自签，不依赖真实网卡）；打包不默认开端口。
- **依赖**：R13。
- **断言**：R14.A1 安全四守卫（默认无口/配对开关/证书钉扎/能力拒绝）进 verify-remote 且在 CI 可跑；R14.A2 三平台防火墙首监听行为文档化（Windows 实测弹授权，其余文档+代码路径）。
- **验证**：`--task R14`

### R15 中继接口冻结门

- **结果**：relay-interface.md 冻结（owner 注册码/device 令牌两类认证、路由键、Noise XX、限速背压）；评审式验收（无服务端代码）。
- **依赖**：R14。
- **断言**：R15.A1 文档可实现性审查表全部有答案（帧格式/错误码/重连/限流数值）。
- **验证**：`--task R15`（node 脚本校验文档必备小节存在且无 TBD）

### R16 r-code-relay 中继二进制

- **结果**：crates/r-code-relay/：daemon 长连接注册、device→owner 转发、限速/连接上限/元数据审计；不落地明文。
- **依赖**：R15。
- **断言**：R16.A1 owner 码一次性注册；device 绑定校验；无 owner 在线拒绝；R16.A2 中继进程内存/日志不含可解读命令（密文断言）。
- **验证**：`--task R16`

### R17 RelayTransport（daemon 出站+E2EE）

- **结果**：IpcTransport 第三种实现；Noise XX 经中继；对 ApplicationHandler 无感知。
- **依赖**：R16。
- **断言**：R17.A1 daemon 出站注册+退避重连，命令行为与直连一致；R17.A2 模拟恶意中继只能断连不能解密/篡改（篡改检测断连）。
- **验证**：`--task R17`

### R18 桌面端中继配置 UX

- **结果**：owner 注册码生成、QR 含中继信息、连接状态；默认无中继地址。
- **依赖**：R17。
- **断言**：R18.A1 配置持久化+状态（在线/退避）；空配置不走中继。
- **验证**：`--task R18`

### R19 PWA 经中继配对与连接

- **结果**：4G/外网场景（测试用 loopback relay+域名映射模拟）；直连优先/回落中继。
- **依赖**：R18。
- **断言**：R19.A1 经中继完成配对与读写；R19.A2 设备吊销即使中继仍转发也被 daemon 拒绝（纵深防御）；R19.A3 错误配对密语失败。
- **验证**：`--task R19`；真实移动网络取证=外部放行。

### R20 中继部署物料与安全验收

- **结果**：Dockerfile/compose 或 systemd 二选一+ACME 文档+运维手册；verify-relay 守卫。
- **依赖**：R19。
- **断言**：R20.A1 物料在最小规格（1vCPU/512MB）起服务；R20.A2 守卫：密文不可见/注册码一次性/吊销纵深/默认无中继。
- **验证**：`--task R20`；真实 VPS 部署=production_release_ready。

---

## §8 证据与恢复

- 任务包：`artifacts/ai-tasks/current.yaml`（模板见 `.agents/skills/prd-to-ai-worklist/assets/current-task.template.yaml`，project_id 填 `remote-control`）。
- 证据：`artifacts/ai-tasks/evidence/<id>.yaml`；报告：`artifacts/ai-tasks/verification/implementation/<id>.json`（verify-remote 自动生成）。
- 恢复时先跑该任务断言的 smoke（直接跑对应 cargo test 过滤名），失败先修再续；current.yaml 与代码冲突以代码事实为准并更正 packet。
- 大文件手术纪律（T42 事故教训）：分阶段 commit；批量删除脚本必须单项验证+先备份；长任务测试前台跑并输出到 target/test-logs/。
- 守护进程测试清理：复用/抽出 tui tests/daemon_common 的 kill_stale_target_daemons+DaemonGuard 模式到 runtime 测试公共模块（注意 canonicalize 的 `\\?\` 前缀坑）。
