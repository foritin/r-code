# 远程控制 — AI 实施工作清单（Execution Contract）

> 状态：`frozen`（长任务化 v2：原生 App 终态 + 任务卡深度展开）
> 转换契约：prd-to-ai-worklist v1.1.0
> 规范输入：[plan.md](./plan.md)、[architecture.md](./architecture.md)、[relay.md](./relay.md)、[prototype.html](./prototype.html)、[tasks.json](./tasks.json)
> 统一验收：`node scripts/verify-remote.mjs`；任务包：`artifacts/ai-tasks/`
> 取证基线：commit `7d56a3d` 后

---

## 执行导航（新会话从这里开始）

1. **首次执行**：读本「§0 执行协议」「§2 冻结决策」+ 编号最小的 ready 任务卡（当前 **RA1**）。
2. **续跑**：读 `artifacts/ai-tasks/current.yaml` + 该任务卡 + `artifacts/ai-tasks/evidence/<id>.yaml`；对已完成断言先跑最小 smoke 再继续。
3. 不要通读全部任务卡；未完成任务按需加载。
4. 每个可验证子步后更新 current.yaml（changed_paths/completed_assertions/decisions）；任务全断言绿 + 累计门禁过后归档证据并勾选 §6。
5. 完成任务=在 `scripts/verify-remote.mjs` 的 `COMMANDS` 注册表把该任务断言映射到真实非交互命令，使 `--task <id>` 退出 0。断言未实现时退出码 2，禁止伪造映射。

---

## §0 执行协议

固定循环：`preflight → 选编号最小且依赖已满足的 MUST → 建/恢复任务包 → 实现 → 任务断言 → 累计门禁 → 归档 → 勾选 → 下一项`。

- **测试全部前台**，输出重定向到 `target/test-logs/<task-id>.log`（可随时查看进度；禁止只接 `| tail`）。单命令超时 ≤10 分钟。
- 无人工暂停点：里程碑出口、文档、测试通过都不等确认。
- 失败处理顺序：保留现场日志 → 定位最小根因 → 聚焦修复 → 复跑同断言 → 第二次失败换受约束方案；同方案无进展不第三次微调。
- 允许中断的唯一情形：需要用户未授权的权限扩张/不可逆生产动作/无法获取且无 adapter 可替代的真实凭据。
- **禁止**：删/跳过失败断言、降阈值、缩 coverage、mock 冒充真实 TLS/配对/中继、reset 工作区既有改动。
- 外部放行分层：本地 loopback/自签证书/模拟器内完成的叫 `implementation_verified`；真实 VPS、真实蜂窝网络、App Store 提交、APNs/FCM 生产凭证归 `production_release_ready`，AI 不自称完成。

### Preflight（每次会话）

```bash
git rev-parse --short HEAD
cargo check --workspace --all-targets      # 基线绿
cargo clippy --workspace --all-targets -- -D warnings   # 0 警告（仓库已达此状态）
node scripts/verify-remote.mjs --list       # 34 任务可读
```

### 平台覆盖纪律

三平台（Windows x64 / macOS arm64+x64 / Linux x64）的监听、TLS、mDNS、通知行为差异必须平台分支实现与测试；Windows 本机可测，macOS/Linux 的平台特有断言写进 CI 矩阵而不是 cfg 死代码。声称“跨平台”=三平台编译+各自平台断言在 CI 有归属。

---

## §1 终态与完成层级

**Definition of Done（implementation_verified）**：

1. R0：v2 插件审批闭环（op 持久化、事件外发、daemon 决策、RunManager 真实接线、TUI 本地审批绿）；
2. R1–R4：局域网配对（码→QR/mDNS）、TLS 指纹钉扎、能力强制、事件扇出、PWA 只读→写→设备管理；
3. R5：自托管中继二进制 + E2EE + daemon 出站传输 + 桌面配置 + loopback 全流程；
4. R6：React Native iOS+Android App（TS core 与 PWA 共享），扫码配对、会话/审批、推送（fake adapter 闭环）、release 构建与合规清单；
5. 默认无远程监听口；能力在 daemon 侧强制（负向测试）；`verify-remote.mjs --through R6` 退出 0；clippy/fmt 全绿；三平台 CI 矩阵绿。

**production_release_ready（外部放行，清单式取证，不阻塞实现）**：真实 VPS 部署 + 域名证书、真机 iOS/Android 蜂窝网络端到端、APNs/FCM 生产凭证投递、App Store Connect/Google Play 提交。

**不做**：官方运营中继/账号/订阅；App 内嵌执行引擎或下载执行代码；多用户协作；远程插件安装/凭据读取；远程任意 shell 通用化。

---

## §2 冻结决策（不可变约束）

| ID | 决策 | 违背即失败的验证点 |
| --- | --- | --- |
| F1 | 远程是 daemon 的 transport 扩展（管道/Unix socket/WS-TLS/中继），同一 ApplicationHandler | 无第二套命令分发；R00 回归 |
| F2 | 默认面为零：不配对不监听，最后设备吊销后关监听，不绑 0.0.0.0，仅私网网卡 | R04.A1 端口扫描 |
| F3 | 强制 TLS，自签证书指纹 QR 钉扎（TOFU），不符硬拒，无绕过 UI/API | R03.A2 真握手失败 |
| F4 | 配对码一次性/≥128 熵/120s/内存态；设备令牌 32B 随机只存 SHA-256 | R02 负向 + R01.A1 明文扫描 |
| F5 | 能力三档 events:read(默认)/tasks:write/approvals:decide(默认关)；settings/plugins/device/service 永不远程 | R04.A3 全拒绝矩阵 |
| F6 | 能力 daemon 侧按 client_id 强制；越权返回 PermissionDenied{required_capability} | 前端绕过后端仍拒 |
| F7 | 命令去重复用 CommandDedup，跨 transport 一致（同 id 重放原结果） | R04.A2 |
| F8 | 推送与 task.events 字节同形（EventEnvelope，seq 单调），断线 after_seq 不丢不重 | R05.A2 |
| F9 | 审批 op 宿主创建+journal 持久化+重启重建；远程只是第二决策来源 | RA1.A2/RA2.A2 |
| F10 | 凭据不出机；App/PWA 不缓存正文（SW/移动端存储断言）；中继无会话密钥 | R17.A2/R13/R22 |
| F11 | 视觉=obsidian 皮肤（#181818/#f4742b，桌面 tokens 刻度）；prototype.html 四屏基线 | R07b.A2 |
| F12 | 中继独立二进制 crates/r-code-relay/，双方出站 443；中继不解密 | R16/R17 |
| F13 | r-code-client/协议层不引入 tauri；PWA 由 daemon /app 托管；RN 工程在 mobile/ 独立 | 依赖守卫 |
| F14 | App 为瘦客户端：无模型 SDK/工具执行/远程代码；iOS 合规姿态=自有主机远程终端 | R27 静态分析 |
| F15 | 推送无账号：APNs/FCM 句柄随设备登记，可吊销；通知正文不含命令内容 | R25 快照断言 |

**决策空间（AI 自决，排序 安全>简单>一致>可测试>易回滚）**：WS 库（优先 tokio-tungstenite）、证书库（rcgen 优先）、mDNS 库、axum 路由细节、RN 裸工程 vs Expo prebuild（默认裸工程，若 Expo 不引入云构建依赖可选）、TS core 包位置、组件拆分、测试端口（用 0 随机）、推送 fake adapter 形状。

---

## §3 仓库事实基线（commit 7d56a3d 已核实，实施前若失效先更新本卡）

- **IPC 现状**：`crates/r-code-runtime/src/ipc.rs` `IpcListener`（Win named pipe `first_pipe_instance(true)` 独占；Unix `UnixListener` 0600）；accept 循环 `src/daemon.rs::serve_connection` → `CommandDedup`（`application_receipts.rs`，键 (profile,client_id,command_id)）→ `ApplicationHandler::execute`（`src/bin/r-code-service.rs`）。
- **帧协议**：`r-code-harness-protocol::application::{ApplicationCommand{client_id,command_id,method,params},ApplicationResult,ApplicationFrame,DaemonHandshake,DaemonWelcome}`，换行分隔 NDJSON。
- **审批缺口（R0 靶点）**：`plugins/router.rs`——`ApprovalRegistry{decisions: Mutex<HashMap<String,ApprovalDecision>>}` 仅内存 set/decide；`QuestionSink` trait + `IgnoreQuestions`（RunManager:354 实际挂它）；wire `ApprovalsRequest{pending_operation: PendingOperationRef, summary}` / `ApprovalsReply{decision}` 在 services.rs:486；请求分支 router.rs:383（已校验“只认宿主创建的 op 引用”）。
- **journal**：V2Store `save_task_and_events`/`read_events`（crates/r-code-store/src/v2/）；事件映射单点 `run_manager.rs::envelope_of`（payload.journalKind 判别）；RunManager 已有 host_observations 抽头与事件泵模式（250ms 轮询的客户端在 TUI engine.rs）。
- **profile/凭据**：`RuntimeProfile::{harness_v2_root,database_path,ipc_endpoint,profile_id}`；`services/settings_store.rs`（平台 SecretStore）；服务二进制旁资源 `R_CODE_SERVICE_BIN`、内置插件目录 `R_CODE_BUILTIN_PLUGINS_DIR`。
- **前端**：src-tauri/frontend 为 React+Vite+TS；Tauri invoke 层 src/lib/ipc.ts；Rust 侧事件→DTO 投影参考 src-tauri/src/harness_v2_chat.rs（spawn_event_pump/agent-event 频道）。
- **测试基建**：`scripts/verify-harness-v2.mjs`（execFileSync+guard+JSON 报告模式）；真实守护进程测试 helper：crates/r-code-tui/tests/daemon_common/（kill_stale_target_daemons[PowerShell -like，勿用 canonicalize 的 \\?\ 前缀]、DaemonGuard、staging native 包、独立线程跑 tokio runtime 避免嵌套）；runtime 真插件 e2e：crates/r-code-runtime/tests/conversation_engine.rs（EchoModel+stage_native）。
- **移动端**：仓库当前无任何 RN/移动工程；R21 从零建 `mobile/`。

---

## §4 需求追踪

| REQ | MUST 含义 | 任务 | 核心断言 |
| --- | --- | --- | --- |
| REQ-1 | 默认无远程面 | R04,R14 | 未配对端口扫描零监听 |
| REQ-2 | 两种配对方式等价 | R02,R09 | 一次性/TTL/重放；QR=手动登记 |
| REQ-3 | 证书钉扎不可绕过 | R03,R22 | 错指纹真 TLS 拒绝 |
| REQ-4 | 令牌存储与能力强制 | R01,R04 | 无明文；越权矩阵全拒 |
| REQ-5 | 事件可靠 | R05 | 扇出+游标不丢不重+背压 |
| REQ-6 | 手机只读 | R07,R08,R23 | 真 TLS e2e |
| REQ-7 | 发送/取消/排队 | R10,R23 | 能力双向+排队语义 |
| REQ-8 | 审批闭环（本地+远程） | RA1–RA3,R12,R24 | request→事件→decide→插件继续 |
| REQ-9 | 设备生命周期 | R11,R26 | 吊销即断连即拒绝 |
| REQ-10 | 移动端可用 | R07b,R13,R21–R28 | 视口/安全区/命中/真机构建 |
| REQ-11 | 公网中继 E2EE | R16–R20 | 抓包密文/恶意中继/纵深吊销 |
| REQ-12 | 敏感面不可远程 | R04 | forbidden 方法硬编码拒绝 |
| REQ-13 | 推送无账号无泄密 | R25 | fake 闭环+正文快照 |
| REQ-14 | App 商店合规 | R27,R28 | 无热更/无下载执行/清单齐 |

---

## §5 里程碑出口（累计门禁）

| 里程碑 | 累计命令 | 出口不变量 |
| --- | --- | --- |
| R0 | `node scripts/verify-remote.mjs --through R0` | 审批持久化/事件/决策面齐；TUI 本地审批 e2e 绿；无远程监听口 |
| R1 | `--through R1` | 传输 seam+设备+配对+TLS+listener+扇出+WS client+PWA 只读，手动码 e2e |
| R2 | `--through R2` | QR/mDNS+写能力+设备管理 |
| R3 | `--through R3` | 远程审批 + PWA 四屏交互 + 审批聚合 |
| R4 | `--through R4` | 通知数据源/打包不默认开口/文档/CI 安全守卫 |
| R5 | `--through R5` | 中继+E2EE+出站传输 loopback 全绿（真 VPS 外部放行） |
| R6 | `--through R6` | iOS/Android App 实现验收全绿（真机/商店外部放行） |

---

## §6 主 Checklist（唯一完成状态源）

**R0 主链前置**
- [ ] **RA1** 审批 pending op 持久化与事件外发
- [ ] **RA2** daemon 审批决策方法面
- [ ] **RA3** 审批接线进 RunManager（本地闭环）

**R1 局域网只读**
- [ ] **R00** 传输抽象
- [ ] **R01** DeviceRegistry 与能力模型
- [ ] **R02** 一次性配对会话
- [ ] **R03** 自签证书与指纹钉扎
- [ ] **R04** RemoteListener（WS over TLS）
- [ ] **R05** 事件长连接扇出
- [ ] **R06** r-code-client WebSocket transport
- [ ] **R07** PWA 只读控制台
- [ ] **R08** 手动配对端到端

**R2 局域网控制**
- [ ] **R09** QR 配对与 mDNS 发现
- [ ] **R10** tasks:write 远程写路径
- [ ] **R11** 设备管理（桌面端）

**R3 远程审批**
- [ ] **R12** approvals:decide 远程审批
- [ ] **R07b** PWA 四屏交互
- [ ] **R07c** 审批聚合 tab

**R4 打磨打包**
- [ ] **R13** 通知与连接打磨
- [ ] **R14** 防火墙/安装/文档/CI 守卫

**R5 公网中继**
- [ ] **R15** 中继接口冻结门
- [ ] **R16** r-code-relay 二进制
- [ ] **R17** RelayTransport（出站+E2EE）
- [ ] **R18** 桌面中继配置 UX
- [ ] **R19** PWA/App 经中继配对连接
- [ ] **R20** 中继部署物料与安全验收

**R6 原生 App**
- [ ] **R21** RN 工程与 TS core 抽包
- [ ] **R22** 原生配对与安全存储
- [ ] **R23** 原生任务列表与会话屏
- [ ] **R24** 原生审批体验
- [ ] **R25** 推送句柄与原生通知
- [ ] **R26** 原生设置/诊断屏
- [ ] **R27** 签名/商店素材/合规
- [ ] **R28** 真机外网验收与文档

进度：0/34。下一执行项：**RA1**。

---

## §7 任务卡

> 断言在 scripts/verify-remote.mjs 的 ASSERTIONS/COMMANDS 登记；证据 artifacts/ai-tasks/evidence/<id>.yaml。每张卡结构固定：结果/引用/依赖/前置事实/固定约束/决策空间/产物/步骤（8 步闭环）/断言/验证/失败处理。

<!-- APPEND-MARKER -->
### RA1 审批 pending op 持久化与事件外发

- **结果**：插件调 `host.approvals.request` 后产生可观察、可持久、可决策的审批流：journal 出现 `approval.requested`；客户端决策产生 `approval.decided`；阻塞中的插件调用在决策（或超时拒绝）后继续；daemon 重启后未决 op 可重建。
- **需求引用**：REQ-8、F9；architecture.md §6；tasks RA1。
- **依赖**：无（首个任务）。
- **前置事实**（只读核实，失效则先更新本卡）：
  - router.rs:383 `host.approvals.request` 分支已存在，当前查内存 `ApprovalRegistry.decide()`；
  - `ApprovalRegistry` 只有 `decisions: Mutex<HashMap>`，无 pending 概念；
  - `ApprovalsRequest{pending_operation: PendingOperationRef, summary}` / `ApprovalsReply{decision: ApprovalDecision(Granted|Denied)}`；
  - journal 写入经 RunManager（构造 router 时传入 store Arc）；事件映射在 `envelope_of`。
- **固定约束**：
  - op id 只能宿主生成/校验（保留既有 PendingOperationRef 校验，插件自造 id 继续拒绝）；
  - 事件与 assistant.message 同通道（task.events，EventEnvelope payload.journalKind 判别）；
  - **无人连接时 op 保持 pending，不自动拒绝、不自动放行**；默认决策超时=300s 后 Denied 并记事件（超时值在 RunManager 构造可配）；
  - 同 op 重复 request 是幂等返回等待中的同一个决策，不新建 op；
  - 迟到/重复/冲突决策：第一次有效，之后同决策重放原结果、异决策拒绝（对齐 CommandDedup 与 kernel 既有裁决门）。
- **决策空间**：
  - 状态存储优先“journal 事件投影 + 内存等待索引”（无新表、重启天然一致）；若 RA2 的 list 查询性能证明需要再加 v2 表，表必须有 migration 与空库/升级测试；
  - 等待原语 tokio::sync::watch/oneshot 自选（要求支持“先注册等待者、后决策”与“决策可能早于重连等待者”两种时序，watch 更合适）；
  - 事件 payload 字段名自定但必须在本卡冻结并同步 envelope_of 与协议文档。
- **产物**：
  - `crates/r-code-runtime/src/plugins/approval_store.rs`（新）：持久 pending/decision 索引 + 等待通道；
  - router.rs 接线（ApprovalRegistry 委托或替换为 ApprovalStore，保持 t39 测试引用的方法形状）；
  - run_manager.rs：router 构造传入 store；事件 kind 增加 approval.requested/decided 的 envelope 映射；
  - `crates/r-code-runtime/tests/ra1_persistent_approvals.rs`（新）。
- **步骤**：
  1. **预检**：`cargo test -p r-code-runtime --test harness_conformance` 绿（t39 锁审批语义）；读 router.rs:383 与 ApprovalRegistry 全部调用点。
  2. **契约**：在协议层或 runtime 内冻结事件 payload：requested `{op_id, summary, run_id, task_id, created_seq}`；decided `{op_id, decision:"granted"|"denied", decided_by: client_id|"<timeout>", decided_seq}`。更新 architecture.md §6 引用的形状（文档随实现 PR 同改）。
  3. **数据**：ApprovalStore 提供 `register(op, summary, run_id)->Receiver`、`decide(op, decision, by)`、`pending() -> Vec`、`rebuild_from_events(events)`；写事件复用 RunManager 已有的 journal 写路径（HostRouter 已持 store Arc）。
  4. **核心实现**：router 的 request 分支改为 store.register + 写 requested 事件 + await 接收决策；超时用 tokio::time::timeout 包 await，超时落 Denied 事件并返回 Denied 给插件。
  5. **装配**：RunManager::new 增 approval_store 参数（ApplicationService 构造处同步：application.rs、r-code-service.rs、conversation_engine 测试、t32/t39 fixture）；daemon 启动时从最近事件 rebuild。
  6. **负向/失败测试**：插件伪造 op 被拒；超时 Denied；冲突决策拒绝；重放同决策返回同结果。
  7. **回归**：harness_conformance、conversation_engine、t32 全绿；TUI 无审批 UI 变化（仍走不到该路径，RA3 才接）。
  8. **证据**：在 COMMANDS 注册 RA1.A1–A3 的 cargo-test 映射；`--task RA1` 退出 0；归档 evidence。
- **验收断言**：
  - RA1.A1（integration）真 router：request→journal 有序列递增的 approval.requested（payload 五字段）；granted→approval.decided；被阻塞的插件调用收到 Granted 后继续（用可控制决策时机的 fixture 断言时序）。
  - RA1.A2（reliability）决策前 drop daemon（测试中杀 store 持有者/用持久库重开 ApplicationService）：pending 从事件重建，list 含该 op，决策后 decided 落库；第二次异决策被拒。
  - RA1.A3（security-negative）ApprovalsRequest 带未登记的 pending_operation 返回 RpcError，不产生事件。
- **验证**：`node scripts/verify-remote.mjs --task RA1`（日志 target/test-logs/RA1.log）。
- **失败处理**：若事件投影无法表达“等待中的接收者”，接收者留内存、状态留 journal（分层），不得把决策状态只放内存；若 t39 依赖旧方法名，加薄适配而非改弱 t39。

### RA2 daemon 审批决策方法面

- **结果**：ServiceHandler 暴露 `approvals.list`（返回 pending）与 `approvals.decide`（决策并唤醒等待）；走 CommandDedup；审计含 client_id。
- **需求引用**：REQ-8、F7、F9。
- **依赖**：RA1。
- **前置事实**：ServiceHandler 方法分发在 src/bin/r-code-service.rs；approval store 需要成为 ApplicationService 可访问组件（当前 RunManager 内部持有——本任务把 store 句柄提升到 ApplicationService 字段）。
- **固定约束**：
  - list 只返回 pending（granted/denied 走 task.events 查）；
  - decide 参数 `{operationId, decision:"granted"|"denied"}`；未知 op 返回结构化错误 `approval_unknown`，绝不隐式创建；
  - 决策审计 client_id 来自 ApplicationCommand（连接握手身份），不可由 params 指定；
  - 本机 transport 与未来远程 transport 同方法，能力强制在 R04/R12 加（本任务不区分来源，但默认只允许本机——在方法上加来源检查占位，远程来源 R04 接通前直接拒）。
- **决策空间**：store 在 ApplicationService 的持有方式（Arc clone）；list 排序（created_seq 升序默认）。
- **产物**：r-code-service.rs 两方法分支；application.rs 暴露 approval_store；RA2 测试（runtime integration test 直连 in-proc ApplicationService + 临时 v2 库）。
- **步骤**：
  1. 预检 RA1 三断言绿。
  2. 提升 ApprovalStore 到 ApplicationService（RunManager 与新方法共享同一 Arc）。
  3. 实现 list（pending 投影 → JSON：op_id/summary/run_id/task_id/created_seq/age_ms）。
  4. 实现 decide：未知 op→Err；已知→store.decide + journal decided 事件（decided_by=client_id）；经 CommandDedup 验证“同 command_id 重放=原结果、异决策=拒绝”。
  5. 本机来源占位检查：当前连接全部本机，预留 `source: local|remote` 透传位（remote 在 R04 落地前调用 decide 返回 remote_not_enabled）。
  6. 负向测试：unknown op、非 granted|denied 入参、重复冲突。
  7. 回归：t32/conversation_engine。
  8. 注册断言、归档。
- **验收断言**：
  - RA2.A1（integration）request 后 list 恰好含 1 条且字段正确；decide 后 list 为空、事件含 decided_by。
  - RA2.A2（integration）用相同 (client_id,command_id) 重放 decide 返回首次结果（幂等）；不同 command_id 对同 op 反相决策被拒。
- **验证**：`--task RA2`。
- **失败处理**：CommandDedup 是结果缓存，若决策类命令不适合缓存（有状态唤醒副作用），确认唤醒本身幂等（watch 重复 set 无害）后按“命令幂等”而非“结果回放”实现，并在证据中说明。

### RA3 审批接线进 RunManager（本地闭环）

- **结果**：RunManager 不再挂 IgnoreQuestions；真实审批 store 贯通；TUI 经 daemon 看到 approval.requested 并可 decide，完成一次真工具审批端到端。
- **需求引用**：REQ-8；prototype 屏③前置。
- **依赖**：RA2。
- **前置事实**：run_manager.rs:354 `Arc::new(crate::plugins::IgnoreQuestions)`；TUI engine.rs 有事件轮询但无审批交互；approval_overlay.rs 是旧本地浮层（视觉可复用，数据源要换）。
- **固定约束**：无决策到期默认 Denied（安全默认，与 RA1 一致）；审批事件必须能驱动 UI（TUI 浮层 + PWA/App 后续消费同一事件）。
- **决策空间**：TUI 交互可先用现有 approval_overlay 浮层（y/n），不重做视觉；事件驱动方式（engine 投影出 PendingApproval 状态）。
- **产物**：run_manager.rs 去 IgnoreQuestions；tui engine.rs 审批投影与 approve/deny 调用；approval_overlay 接线；runtime 端到端测试（真插件发起审批）。
- **步骤**：
  1. 预检 RA1/RA2 绿。
  2. RunManager 构造挂真实 store（移除 IgnoreQuestions；IgnoreQuestions 类型保留给明确无人值守测试并在文档标注用途）。
  3. 做一个会发起 host.approvals.request 的测试用 harness fixture（参考 harness-test-helper，加一个“审批型工具”插件脚本/binary，或在现有 native 测试进程加 test-only 触发路径——优先独立 fixture，不污染 native 包）。
  4. runtime e2e：插件 request→事件→客户端 approvals.decide(granted)→插件继续→run.completed；denied 路径插件收到 Denied 并按其逻辑结束。
  5. TUI：engine 投影 approval.requested 为浮层状态，y/n 调 daemon decide；决策后浮层消失；超时显示“已自动拒绝”。
  6. TUI 测试：lib 单测投影纯函数；PTY 测试新增审批脚本路径（daemon_common staging 审批 fixture），断言浮层与决策事件。
  7. 回归：全套 tui 测试 + verify-harness-v2 full。
  8. 注册断言、归档。
- **验收断言**：
  - RA3.A1（e2e）真进程链路 request→decide(granted)→插件继续执行并完成，journal 事件链完整。
  - RA3.A2（e2e）deny 路径：插件收到 Denied、run 终态正确（completed-with-denial 或按插件语义失败，但不是挂起）；无决策时 300s 超时（测试用短超时配置，不真等 300s）。
- **验证**：`--task RA3`；累计 `--through R0`。
- **失败处理**：fixture 进程方案过重时，可用 in-proc 插件会话直接发 host.approvals.request RPC（transport 层注入），但断言必须经过真实 journal 与真实阻塞 await，不许直接调 store。
### R00 传输抽象

- **结果**：daemon 连接处理与客户端连接建立可按 transport 注入；管道/Unix socket 现有行为逐字节不变；全仓无网络监听被引入。
- **需求引用**：F1、F13。
- **依赖**：无（可与 RA 并行；R04 需要）。
- **前置事实**：IpcListener::accept 返回的是帧流（serve_connection 按行读 ApplicationFrame）；DaemonClient::connect 直接持有平台连接。
- **固定约束**：不新增 TcpListener；owner 锁语义、帧上限（1MiB）、队列总量（16MiB）不被抽象稀释；t06a/t35/t06b 测试不改断言。
- **决策空间**：最小 seam——trait `RemoteTransport: AsyncRead+AsyncWrite+Send`（或直接用复合 trait object），不造多层运行时。
- **产物**：ipc.rs 抽出 accept→frame stream 边界；r-code-client 的连接工厂可注入。
- **步骤**：①跑现有 daemon/client 测试取绿基线；②识别 serve_connection 与平台 accept 的边界行；③引入最小抽象并让两处现有路径成为第一个实现；④静态守卫测试：`cargo tree -p r-code-runtime`（在 R04 前）不含 tungstenite 以外的网络监听库被 listener 使用（grep 无 TcpListener）；⑤回归 t06a(owner 竞争)/t35 全流程/t06b 命令去重；⑥clippy/fmt；⑦证据。
- **验证**：`node scripts/verify-remote.mjs --task R00`（前台，日志 target/test-logs/R00.log；外部放行项另见任务卡）。
- **断言**：R00.A1（regression）三个指定测试全绿且无修改断言；新增 grep 守卫“R04 前 runtime 无 TcpListener”。
- **失败处理**：若抽象导致帧处理重复代码，允许 serve_connection 泛型化，但禁止把 owner/握手逻辑复制两份。

### R01 DeviceRegistry 与能力模型

- **结果**：设备登记持久化、令牌只存哈希、能力三档可校验；远程禁用方法清单成为单一常量。
- **依赖**：R00。
- **前置事实**：profile.harness_v2_root() 可放 devices/；SecretStore 已在 settings_store 使用（Win keyring/macOS 加密文件/Linux keyring）——设备令牌与配对码也应走同一存储或至少文件 0600。
- **固定约束**：F4/F5/F6；registry.json 任何位置不含明文令牌（测试对文件内容做子串断言）；默认能力仅 events:read。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **产物**：`src/remote/{mod.rs,capabilities.rs,registry.rs}`。
- **步骤**：①定义 Capability 枚举与 `CapabilitySet`（serde kebab-case：events-read/tasks-write/approvals-decide）；②FORBIDDEN_REMOTE_METHODS 常量列全（settings.apply|setDefault|get 的写面、plugins.install|remove|setEnabled、device.*、service.shutdown、remote.* 管理面）；③DeviceRecord（architecture.md §7）+ registry：register/list/get_by_token(sha256)/revoke/update_capabilities/last_seen；④存储：registry.json 0600 + 令牌明文不落盘（测试用临时 profile 读文件断言）；⑤last_seen 更新走防抖（不每次命令写盘）；⑥单测：注册往返、错误令牌、吊销、能力默认、序列化兼容（version 字段）；⑦clippy；⑧证据。
- **验证**：`node scripts/verify-remote.mjs --task R01`（前台，日志 target/test-logs/R01.log；外部放行项另见任务卡）。
- **断言**：R01.A1（unit）注册返回的令牌可校验，registry.json 不含令牌明文与 apiKey；R01.A2（unit）revoke 后 get_by_token=None；R01.A3（unit）新设备能力=={events:read}。
- **失败处理**：平台 SecretStore 在无 keyring 的 CI Linux 退化文件存储时必须 0600（settings_store 已有降级模式，复用）。

### R02 一次性配对会话

- **结果**：本机命令发起配对→临时监听口→凭一次性码颁发设备→码立即失效。
- **依赖**：R01。
- **固定约束**：F4；pairingStart 仅本机 transport（远程/未来 WS 未认证连接调用=拒绝）；TTL 120s；不持久化（daemon 重启无待决配对）；同一时刻允许一个或多个配对会话（实现决定，默认单会话，新 start 作废旧 start）。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **产物**：`src/remote/pairing.rs`。
- **步骤**：①PairingSession{secret_hash,fp,expires_at,consumed}；②pairingStart 方法（暂挂在 ApplicationService/ServiceHandler，仅接受来自本地 pipe/socket 的调用——R04 前没有远程连接，加 source 标记）返回 {pairingCode, qrPayload 占位, lanEndpoints, expiresAtMs}；③device.pair {pairSecret, deviceName, platform, publicKey?}：校验→DeviceRegistry.register→返回 {deviceId, token, capabilities, serverFingerprint}；④过期后台清理/惰性校验；⑤临时监听口生命周期（R04 提供监听能力前，本任务只做会话逻辑+用注入的 listener trait 测试）；⑥负向测试：过期/重放/二次消费/错误码；⑦证据。
- **验证**：`node scripts/verify-remote.mjs --task R02`（前台，日志 target/test-logs/R02.log；外部放行项另见任务卡）。
- **断言**：R02.A1（integration）过期/重放/二次消费三类各返回明确错误码且不产生设备；R02.A2（integration）成功配对返回一次性令牌且 registry 出现设备；R02.A3（security）非本机 source 调 pairingStart 被拒。
- **失败处理**：lanEndpoints 枚举网卡在无头 CI 不可用→返回空数组不报错（发现逻辑独立可测：mock 网卡列表）。

### R03 自签证书与指纹钉扎

- **结果**：首次配对生成持久自签证书；客户端凭 QR 指纹 TOFU；错指纹在 TLS 层失败。
- **依赖**：R02。
- **决策空间**：rcgen 生成自签证书（优先）；私钥存平台 SecretStore（无 keyring 退化 0600 文件）；证书 CN/SAN 覆盖局域网 IP 与 mDNS 名（IP 进 SAN，客户端校验只看指纹不看主机名）。
- **固定约束**：F3；无“接受风险继续”路径；证书生成一次持久复用（重启不重新生成导致已配对设备全失效）。
- **产物**：`src/remote/tls.rs`。
- **步骤**：①ensure_identity()：存在则加载，否则生成（ECDSA P-256）+ 输出 sha256 指纹；②服务端 acceptor 构造；③客户端校验器：仅接受等于钉扎指纹的证书（其他一律 Err，含系统链合法证书）；④测试：rcgen 生成第二张证书，用错误指纹连接必须 TLS 握手失败（不是应用层 close）；⑤重启复用测试（同一 profile 两次 ensure 指纹一致）；⑥私钥存储测试（不入 registry.json，平台存储或 0600）；⑦证据。
- **验证**：`node scripts/verify-remote.mjs --task R03`（前台，日志 target/test-logs/R03.log；外部放行项另见任务卡）。
- **断言**：R03.A1（integration）两次启动指纹一致；R03.A2（security-negative）错指纹在 TLS 握手阶段失败；R03.A3（security）无 TLS 的裸 TCP 连接发不出 ApplicationFrame（listener 不接受明文帧）。
- **失败处理**：先保留失败现场日志定位根因；同方案第二次失败换受约束实现；外部依赖（真机/商店/VPS）缺失时用 adapter/fixture 完成 implementation_verified，真实放行记入 production 清单不阻塞。
### R04 RemoteListener（WS over TLS）

- **结果**：配对后在私网网卡提供 wss；握手认证→能力上下文→同一命令管道；越权/forbidden 在服务端拒绝；吊销关监听。
- **依赖**：R03。
- **前置事实**：serve_connection 当前接平台 stream；WS 升级后也是字节流，帧复用 NDJSON 读写。
- **固定约束**：F2/F5/F6；绑定地址仅配对时选定网卡的私网段（RFC1918/链路本地/ULA）；无设备不监听；认证失败立即断连且不产生日志洪水（限速）。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **产物**：`src/remote/listener.rs` + daemon 装配 + capability 中间层。
- **步骤**：①WS acceptor（tokio-tungstenite，rustls）挂在 IpcListener 之外；②连接首帧 hello（architecture §4）：校验 token→取 DeviceRecord→注入 (client_id=device_id, caps)；之后帧走与本地相同的 CommandDedup+Handler，但分发前过 capability gate；③capability gate：方法→所需能力映射（task.create/sendMessage/cancel/rename/clone→tasks:write；approvals.decide→approvals:decide；events/list/detail/models.available/plugins.list→events:read）；FORBIDDEN 集合无条件 PermissionDenied/forbidden_remote_method；④生命周期：pairing 临时口→首个设备后正式口；revoke 最后一台→关闭；setListener(false)→断全部；⑤限速：同 IP 认证失败计数（内存，重启清零）；⑥集成测试用 127.0.0.1 上的真实 TLS（loopback 作为“已选定网卡”的测试替身，生产绑定校验单独单测：公网地址被拒绝）；⑦负向矩阵（见断言）；⑧证据。
- **验证**：`node scripts/verify-remote.mjs --task R04`（前台，日志 target/test-logs/R04.log；外部放行项另见任务卡）。
- **断言**：
  - R04.A1（security-negative）未配对/全设备吊销/setListener false 三种状态 connect 被拒（端口级连接失败或 TLS 后立即关闭，且无命令响应）；
  - R04.A2（integration）认证连接的 command_id 重放返回同一结果；错误 token 立即断连；
  - R04.A3（security-negative）无能力调 sendMessage/decide 返回 PermissionDenied{required_capability}；**有全部能力也不能**调 settings.apply/plugins.install/device.*/service.shutdown（forbidden_remote_method）；
  - R04.A4（integration）吊销最后设备后 listener 关闭（新连接失败）。
- **失败处理**：WS 库与现有 rustls 版本冲突时，允许先以 TLS+裸 NDJSON over TLS stream 实现（不升级 WS），但 PWA/App 需要浏览器 WebSocket——此替代仅限测试通道，产品通道必须 WS，冲突要升级为依赖调整决策并记录。

### R05 事件长连接扇出

- **结果**：认证连接可订阅事件流，推送与 task.events 同形；游标恢复可靠；慢消费者不阻塞 journal。
- **依赖**：R04。
- **固定约束**：F8；背压阈值 1MiB 或 1000 帧（超出断开该连接）；扇出失败不影响 run 执行与本地客户端。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **产物**：`src/remote/fanout.rs`。
- **步骤**：①订阅注册中心（per-connection mpsc/广播，按 task 范围——首期全 profile）；②挂接点：journal append 成功后 fanout（与 RunManager 事件写入同一先后关系：先持久化后推送）；③events.subscribe 帧处理：先发 after_seq 历史补齐，再推增量；④心跳（ping 帧，间隔与超时实现冻结，建议 30s/60s）；⑤慢消费者：try_send 满→计数→断连；⑥测试：实时性、重连续传、慢消费者隔离（一个卡住的连接不影响另一个收事件）；⑦证据。
- **验证**：`node scripts/verify-remote.mjs --task R05`（前台，日志 target/test-logs/R05.log；外部放行项另见任务卡）。
- **断言**：R05.A1 订阅后新事件按 seq 到达；R05.A2 断开期间产生 N 事件，after_seq 重连无缺无重（多轮 50 事件随机断开）；R05.A3 慢连接被断开而 journal 写入与其他订阅不受影响。
- **失败处理**：先保留失败现场日志定位根因；同方案第二次失败换受约束实现；外部依赖（真机/商店/VPS）缺失时用 adapter/fixture 完成 implementation_verified，真实放行记入 production 清单不阻塞。
### R06 r-code-client WebSocket transport

- **结果**：DaemonClient 可经 wss 用设备令牌连接；API 与管道版一致；断线重连+命令重放。
- **依赖**：R05。**约束**：F7；不引入 tauri 依赖（client crate 保持纯 tokio）。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **产物**：`crates/r-code-client/src/ws.rs`。
- **步骤**：①transport 枚举/工厂（pipe|unix|wss）；②wss 连接：URL 由 endpoint/host/port 推导，rustls 客户端用钉扎校验器（R03 的指纹）；③首帧 hello 后进入既有命令帧循环；④connect 重试策略复用 ensure_daemon 的退避但针对远程：不可达区分“daemon 未运行”（本地发现，不走远程）与“远程拒绝”；⑤command_id 生成沿用 uuid；⑥测试：真 TLS（R03/04 fixture）往返+杀连接重放原结果；⑦clippy（client 无新警告）；⑧证据。
- **验证**：`node scripts/verify-remote.mjs --task R06`（前台，日志 target/test-logs/R06.log；外部放行项另见任务卡）。
- **断言**：R06.A1（integration）wss 连接调 task.list/events 成功；R06.A2（reliability）连接中断重连后同 command_id 返回首次结果。
- **失败处理**：先保留失败现场日志定位根因；同方案第二次失败换受约束实现；外部依赖（真机/商店/VPS）缺失时用 adapter/fixture 完成 implementation_verified，真实放行记入 production 清单不阻塞。
### R07 PWA 只读控制台（列表/事件）

- **结果**：daemon `/app` 托管远程前端；配对后浏览器看到任务列表与实时事件，可加主屏。
- **依赖**：R06。**约束**：F11/F13；列表只用 task.list 真实字段；SW 只缓存壳（断言缓存名单）；不出现假数据。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **产物**：src-tauri/frontend 独立 Vite entry（remote.html/remote-main.tsx）；daemon 静态托管（embed 或读打包资源，打包随 T38 sidecar 资源机制）；`/app` 路由与 WS 同源策略。
- **步骤**：①Vite multi-entry 配置，remote entry 不引 tauri API（编译期条件：transport 只用 WS core）；②把 harness_v2_chat.rs 的事件投影逻辑的**形状**在 TS 侧复刻（不共享 Rust 代码：TS core 投影 EventEnvelope→会话行，这部分即 R21 抽包的前身，先放 remote/core）；③配对后连接屏（占位：手动输入 host+码，QR 在 R09）→任务列表→任务详情实时流；④manifest.webmanifest+SW（缓存 index/hashed assets，network-only 所有 /events 与数据）；⑤daemon /app 托管（未构建时 404+明确错误，不内嵌假页面）；⑥mjs 测试：构建产物存在、SW 缓存名单、TS core 投影单测；⑦视觉对照 prototype 屏①②（深色 tokens）；⑧证据。
- **验证**：`node scripts/verify-remote.mjs --task R07`（前台，日志 target/test-logs/R07.log；外部放行项另见任务卡）。
- **断言**：R07.A1（node-test）TS 投影：给定事件序列输出正确行（assistant/tool/state）；R07.A2（integration）daemon /app 返回构建产物，未配置时诚实 404。
- **失败处理**：先保留失败现场日志定位根因；同方案第二次失败换受约束实现；外部依赖（真机/商店/VPS）缺失时用 adapter/fixture 完成 implementation_verified，真实放行记入 production 清单不阻塞。
### R08 手动配对端到端（PoC 收口）

- **结果**：不依赖 QR/摄像头，局域网手动码完成只读闭环，三平台监听有归属。
- **依赖**：R07。
- **固定约束**：遵守 §2 冻结决策中本任务相关条目（能力/加密/默认面）；不削弱既有验收。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **产物**：实现模块、正/负向测试、回归验证；落点先以仓库搜索确认（不预设虚构路径）。
- **步骤**：①runtime e2e 测试：staging daemon（真 service binary+自签证书+临时 profile，helper 模式照抄 crates/r-code-tui/tests/daemon_common 的 staging/清理/DaemonGuard）；②测试客户端：pairingStart（本机管道）→ 得码与指纹 → wss 连接走 hello+pair → task.create/sendMessage（由另一个已授权本地客户端制造事件）→ events 读到；③未认证连接零数据断言（握手前不响应任何方法）；④macOS/Linux listener 绑定测试进 CI（平台 cfg，Win 本机）；⑤测试尾 service.shutdown+pid 兜底；⑥证据。
- **验证**：`node scripts/verify-remote.mjs --task R08`（前台，日志 target/test-logs/R08.log；外部放行项另见任务卡）。
- **断言**：R08.A1（e2e）只读闭环绿；R08.A2（security）未认证/错码连接拿不到任何任务数据；R08.A3（static）listener 绑定地址校验拒绝公网 IP（0.0.0.0/8.8.8.8 入参被拒）。
- **失败处理**：先保留失败现场日志定位根因；同方案第二次失败换受约束实现；外部依赖（真机/商店/VPS）缺失时用 adapter/fixture 完成 implementation_verified，真实放行记入 production 清单不阻塞。
### R09 QR 配对与 mDNS 发现

- **结果**：桌面显示 QR（GUI 弹层+TUI /pair），手机扫码直连；同网段 mDNS 可发现服务。
- **依赖**：R08。**决策空间**：QR 用 qrcode crate 渲染（TUI 终端 QR + GUI 图片）；mDNS 选纯 Rust 维护库，无 Avahi 硬依赖。
- **固定约束**：遵守 §2 冻结决策中本任务相关条目（能力/加密/默认面）；不削弱既有验收。
- **产物**：实现模块、正/负向测试、回归验证；落点先以仓库搜索确认（不预设虚构路径）。
- **步骤**：①QR 载荷严格按 architecture §3（rcode://pair?v=1&h=&p=&s=&fp=），编码/解码往返测试+畸形拒绝；②TUI `/pair`：生成码+终端 QR+倒计时（120s）；GUI 设置页弹层（组件最小实现，复用 SettingsScene 风格）；③mDNS 广播 `_rcode._tcp.local`（pairing 期间与正式监听期可发现，含 port/fingerprint TXT）；④手机侧解析（R22 原生相机前，PWA 用输入+链接 deep link 验证载荷解析 TS 逻辑）；⑤等价性测试：QR 与手动码产出同一 DeviceRecord 字段；⑥证据。
- **验证**：`node scripts/verify-remote.mjs --task R09`（前台，日志 target/test-logs/R09.log；外部放行项另见任务卡）。
- **断言**：R09.A1（integration）QR 解析→配对与手动等价；R09.A2（integration）mDNS 注册后同机客户端可发现服务（端口/指纹 TXT 正确），停止后消失。
- **失败处理**：先保留失败现场日志定位根因；同方案第二次失败换受约束实现；外部依赖（真机/商店/VPS）缺失时用 adapter/fixture 完成 implementation_verified，真实放行记入 production 清单不阻塞。
### R10 tasks:write 远程写路径

- **结果**：有权限设备可发送/取消/克隆/重命名；运行中发送=排队；写动作可审计到设备。
- **依赖**：R09。**约束**：F5/F6；审计 device_id（decided_by 已有先例，输入类事件扩展 actor 字段，旧消费者忽略未知字段）。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **产物**：实现模块、正/负向测试、回归验证；落点先以仓库搜索确认（不预设虚构路径）。
- **步骤**：①capability gate 方法映射补全（create/send/cancel/rename/clone 及各自所需能力）；②journal 输入事件记录 actor=client_id（input.queued 扩展可选字段，不破坏现有消费者——验证 GUI/TUI 事件泵忽略未知字段）；③PWA 输入框/中止按钮接通（prototype 屏③）；④测试：有能力成功且 run 状态正确、无能力 PermissionDenied、queued 语义（发送时 run 活跃→queued 响应+run 后派发，复用 conversation_engine 队列测试模式）；⑤证据。
- **验证**：`node scripts/verify-remote.mjs --task R10`（前台，日志 target/test-logs/R10.log；外部放行项另见任务卡）。
- **断言**：R10.A1（integration）有权限 send/cancel 产生既有事件；R10.A2（security）只读设备写操作全拒；R10.A3（integration）排队与自动派发行为同本地。
- **失败处理**：先保留失败现场日志定位根因；同方案第二次失败换受约束实现；外部依赖（真机/商店/VPS）缺失时用 adapter/fixture 完成 implementation_verified，真实放行记入 production 清单不阻塞。
### R11 设备管理（桌面端）

- **结果**：桌面（GUI+TUI /remote）列出/吊销/改能力/开关监听；吊销即时生效。
- **依赖**：R10。**约束**：device.* 仅本机 transport（R04 gate 已硬拒远程）。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **产物**：实现模块、正/负向测试、回归验证；落点先以仓库搜索确认（不预设虚构路径）。
- **步骤**：①device.list（name/platform/capabilities/paired_at/last_seen/online）；②revoke：删令牌+断该设备当前连接（listener 维护连接索引）+重连拒绝；③updateCapabilities：收窄即时生效（推送 capabilities_changed 或直接断连迫重连）；④setListener：enabled 总开关，关时断全部但保留设备；⑤GUI DevicesSettingsPane（最小可用，obsidian 风格）+TUI 列表；⑥测试三连（见断言）；⑦证据。
- **验证**：`node scripts/verify-remote.mjs --task R11`（前台，日志 target/test-logs/R11.log；外部放行项另见任务卡）。
- **断言**：R11.A1 列表字段正确；R11.A2 吊销后活动连接被断、重连被拒；R11.A3 setListener(false) 断连但设备记录保留、再开可重连。
- **失败处理**：先保留失败现场日志定位根因；同方案第二次失败换受约束实现；外部依赖（真机/商店/VPS）缺失时用 adapter/fixture 完成 implementation_verified，真实放行记入 production 清单不阻塞。
### R12 approvals:decide 远程审批

- **结果**：手机批准/拒绝真实工具调用，落到 RA1 的 op；能力默认关闭。
- **依赖**：R11、RA3。
- **固定约束**：遵守 §2 冻结决策中本任务相关条目（能力/加密/默认面）；不削弱既有验收。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **产物**：实现模块、正/负向测试、回归验证；落点先以仓库搜索确认（不预设虚构路径）。
- **步骤**：①R04 gate 映射 approvals.decide→approvals:decide；RA2 的 source 占位检查接通远程来源；②PWA 审批卡（prototype 屏③：命令全文/风险档/来源/两按钮，无能力隐藏按钮）；③高敏“桌面二次确认”配置（任务创建可选 require_desktop_confirm：此时远程 decide 返回 needs_desktop_confirm，桌面本机批准才终决）——配置位与协议字段本任务冻结；④审计 decided_by=device_id；⑤真插件 e2e（RA3 fixture）：手机 decide→插件继续；⑥证据。
- **验证**：`node scripts/verify-remote.mjs --task R12`（前台，日志 target/test-logs/R12.log；外部放行项另见任务卡）。
- **断言**：R12.A1（security）无能力 decide 被拒且 op 仍 pending；R12.A2（e2e）有能力远程 decide 贯通插件执行，审计含设备 id。
- **失败处理**：先保留失败现场日志定位根因；同方案第二次失败换受约束实现；外部依赖（真机/商店/VPS）缺失时用 adapter/fixture 完成 implementation_verified，真实放行记入 production 清单不阻塞。
### R07b PWA 四屏交互（原型基线）

- **结果**：四屏全部可操作；三 tab；移动端可用性达标。
- **依赖**：R07（卡片 ID 按 R3 时序在 R12 后完整，但只读部分随 R07 可用）。
- **固定约束**：遵守 §2 冻结决策中本任务相关条目（能力/加密/默认面）；不削弱既有验收。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **产物**：实现模块、正/负向测试、回归验证；落点先以仓库搜索确认（不预设虚构路径）。
- **步骤**：①底部 tab 任务/审批/设置（审批 tab 无 decide 能力显示只读徽标）；②断网全屏态+自动重连倒计时（WS 状态机）；③安全区 env(safe-area-inset-*)、命中区 ≥24px（CSS 断言 + 390×844 无横向滚动）；④审批卡/设备页交互；⑤组件 node-test（Vitest/jsdom）覆盖状态机与能力投影；⑥视觉走查（prototype 对照，截图只作辅助）；⑦证据。
- **验证**：`node scripts/verify-remote.mjs --task R07b`（前台，日志 target/test-logs/R07b.log；外部放行项另见任务卡）。
- **断言**：R07b.A1（node-test）列表真实字段/缺能力隐藏决策按钮/断网态；R07b.A2（visual）移动视口无横向滚动、命中区、深色皮肤（playwright 视口断言或 mjs 像素/布局检查）。
- **失败处理**：先保留失败现场日志定位根因；同方案第二次失败换受约束实现；外部依赖（真机/商店/VPS）缺失时用 adapter/fixture 完成 implementation_verified，真实放行记入 production 清单不阻塞。
### R07c 审批聚合 tab

- **结果**：跨任务 pending op 聚合列表+快速决策+需桌面确认态。
- **依赖**：R07b、R12。
- **固定约束**：遵守 §2 冻结决策中本任务相关条目（能力/加密/默认面）；不削弱既有验收。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **产物**：实现模块、正/负向测试、回归验证；落点先以仓库搜索确认（不预设虚构路径）。
- **步骤**：①approvals.list 跨任务轮询或 fanout 订阅聚合；②决策后移除+乐观/回滚；③空态/加载态/权限不足态；④node-test 覆盖聚合/排序/权限投影；⑤真服务 e2e（多任务同时 pending，决策后聚合更新）；⑥证据。
- **验证**：`node scripts/verify-remote.mjs --task R07c`（前台，日志 target/test-logs/R07c.log；外部放行项另见任务卡）。
- **断言**：R07c.A1 多 op 排序、决策消失、重连 after_seq 不重复不丢失。
- **失败处理**：先保留失败现场日志定位根因；同方案第二次失败换受约束实现；外部依赖（真机/商店/VPS）缺失时用 adapter/fixture 完成 implementation_verified，真实放行记入 production 清单不阻塞。
### R13 通知与连接打磨

- **结果**：前台通知（完成/待审批）；连接/错误/空态完整；SW 缓存边界固化。
- **依赖**：R12。**约束**：通知正文在远程通道不含命令内容（推送形态）；本地前台通知可含标题。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **产物**：实现模块、正/负向测试、回归验证；落点先以仓库搜索确认（不预设虚构路径）。
- **步骤**：①Notification API（PWA 前台/ SW showNotification）数据源=approval.requested/run.completed；②连接状态机（connecting/online/reconnecting/denied/unpaired）UI；③错误态（指纹不符=明确重新配对引导，非技术报错）；④SW 缓存名单 mjs 断言（壳资源 vs 数据网络必需）；⑤真实移动浏览器对 PWA 推送限制做能力检测，不支持时降级前台通知并在 UI 诚实标注；⑥证据。
- **验证**：`node scripts/verify-remote.mjs --task R13`（前台，日志 target/test-logs/R13.log；外部放行项另见任务卡）。
- **断言**：R13.A1（node-test）缓存名单只含壳；前台通知在对应事件触发（fake Notification 断言）。
- **失败处理**：先保留失败现场日志定位根因；同方案第二次失败换受约束实现；外部依赖（真机/商店/VPS）缺失时用 adapter/fixture 完成 implementation_verified，真实放行记入 production 清单不阻塞。
### R14 防火墙/安装/文档/CI 守卫

- **结果**：安全守卫进 CI（不依赖真实网卡）；三平台防火墙行为文档化；用户文档齐；打包默认不开放端口。
- **依赖**：R13。
- **固定约束**：遵守 §2 冻结决策中本任务相关条目（能力/加密/默认面）；不削弱既有验收。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **产物**：实现模块、正/负向测试、回归验证；落点先以仓库搜索确认（不预设虚构路径）。
- **步骤**：①verify-remote.mjs 的 R0–R4 断言纳入 CI（loopback+自签，无真实网络）；②Windows 首监听防火墙授权提示取证（实现走系统 API 的标准提示，文档+一次手工取证留档）；macOS/Linux 无系统提示时文档说明端口范围；③docs/support/guides/remote-control.md（配对/使用/吊销/安全模型/端口/中继关系）；④打包检查：默认 listener.enabled=false（装机首跑端口扫描为零）；⑤relay/防火墙运维交叉引用；⑥证据。
- **验证**：`node scripts/verify-remote.mjs --task R14`（前台，日志 target/test-logs/R14.log；外部放行项另见任务卡）。
- **断言**：R14.A1（script）四安全守卫 CI 绿（默认无口/配对开关/钉扎/能力矩阵）；R14.A2（static/doc）文档含三平台端口与防火墙章节（结构校验，无 TBD）。
- **失败处理**：先保留失败现场日志定位根因；同方案第二次失败换受约束实现；外部依赖（真机/商店/VPS）缺失时用 adapter/fixture 完成 implementation_verified，真实放行记入 production 清单不阻塞。
### R15 中继接口冻结门（设计门）

- **结果**：中继线协议可实现性冻结，无服务端代码。
- **依赖**：R14。**产物**：relay-interface.md 补全（帧格式、错误码、重连、限流数值、E2EE Noise XX 载荷时序、owner 注册流程）。
- **固定约束**：遵守 §2 冻结决策中本任务相关条目（能力/加密/默认面）；不削弱既有验收。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **步骤**：①把 relay.md §2–§5 的设计落到字段级（owner register/authenticate 帧、device bind、route open、close code 枚举）；②与 R17 RelayTransport 的实现接口对齐（IpcTransport trait 方法集）；③设计评审检查单（无 TBD、每个错误有客户端行为）；④脚本校验必备小节与无 TBD；⑤close code/错误码逐个映射客户端行为（无悬空错误）；⑥证据（评审表填完）。
- **验证**：`node scripts/verify-remote.mjs --task R15`（前台，日志 target/test-logs/R15.log；外部放行项另见任务卡）。
- **断言**：R15.A1（static）文档必备小节齐全（认证/路由/握手/错误码/限流），无 TBD/TODO 字样。
- **失败处理**：先保留失败现场日志定位根因；同方案第二次失败换受约束实现；外部依赖（真机/商店/VPS）缺失时用 adapter/fixture 完成 implementation_verified，真实放行记入 production 清单不阻塞。
### R16 r-code-relay 中继二进制

- **结果**：独立 crate，daemon 长连接注册+device→owner 转发+限速+审计；不落明文。
- **依赖**：R15。**约束**：F12；crates/r-code-relay 独立 workspace 成员，只依赖协议/最小 tokio/axum，不依赖 runtime（防攻击面回流）。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **产物**：实现模块、正/负向测试、回归验证；落点先以仓库搜索确认（不预设虚构路径）。
- **步骤**：①crate 骨架（axum ws 两路由 /owner /device）；②owner 注册码一次性引导→长期 owner 公钥注册（内存注册表，首期单实例）；③device 凭设备凭证+签名绑定 owner，路由键 owner_id；④双向字节转发，零解析应用帧（中继不理解 ApplicationFrame）；⑤限速（每 owner 连接数/带宽/突发，数值冻结进配置）+审计日志（仅连接元数据，定时轮转）；⑥无 owner 在线时 device 明确错误码；⑦测试：注册一次性、未绑定拒绝、转发字节一致、限速生效、日志无明文（转发随机密文断言日志不含）；⑧Dockerfile/compose 在 R20，本任务只产二进制；⑨证据。
- **验证**：`node scripts/verify-remote.mjs --task R16`（前台，日志 target/test-logs/R16.log；外部放行项另见任务卡）。
- **断言**：R16.A1（integration）注册/绑定/拒绝/转发四路径；R16.A2（security）日志与内存快照断言无法还原转发内容（用已知随机串验证）。
- **失败处理**：先保留失败现场日志定位根因；同方案第二次失败换受约束实现；外部依赖（真机/商店/VPS）缺失时用 adapter/fixture 完成 implementation_verified，真实放行记入 production 清单不阻塞。
### R17 RelayTransport（daemon 出站+E2EE）

- **结果**：IpcTransport 第三种实现；Noise XX 经中继协商；对 ApplicationHandler 零感知。
- **依赖**：R16。**约束**：F10/F12；恶意中继只能断连，不能解密/篡改/注入（篡改→认证标签失败断连）。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **产物**：实现模块、正/负向测试、回归验证；落点先以仓库搜索确认（不预设虚构路径）。
- **步骤**：①RelayTransport：daemon 出站 wss 到 relay（owner 长连接、断线指数退避）；②Noise XX：daemon 公钥经 QR 指纹钉扎，配对密语作为带外认证（architecture §3/relay.md §3）；③握手成功后 ApplicationFrame 走加密帧；④与本地/局域网 transport 同一 DaemonClient/Handler 路径（条件编译零分支，纯运行时选择）；⑤恶意中继测试架（可观测/可篡改/可重放的假中继）；⑥命令行为与直连一致性（去重、错误透传）；⑦证据。
- **验证**：`node scripts/verify-remote.mjs --task R17`（前台，日志 target/test-logs/R17.log；外部放行项另见任务卡）。
- **断言**：R17.A1（integration）经中继完整命令往返+重连退避+与直连行为一致；R17.A2（security-negative）假中继：被动窃听得密文、篡改/注入/重放均导致认证失败断连且无副作用。
- **失败处理**：先保留失败现场日志定位根因；同方案第二次失败换受约束实现；外部依赖（真机/商店/VPS）缺失时用 adapter/fixture 完成 implementation_verified，真实放行记入 production 清单不阻塞。
### R18 桌面端中继配置 UX

- **结果**：桌面生成 owner 注册码、QR 含中继信息、显示连接状态；默认不配置中继。
- **依赖**：R17。
- **固定约束**：遵守 §2 冻结决策中本任务相关条目（能力/加密/默认面）；不削弱既有验收。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **产物**：实现模块、正/负向测试、回归验证；落点先以仓库搜索确认（不预设虚构路径）。
- **步骤**：①配置模型（relay URL 存 harness-v2 profile 配置，不进插件清单）；②owner 注册码生成（一次性、短 TTL、仅显示一次）与 daemon 出站注册流程；③QR 载荷扩展中继字段（rcode://pair 增加 relay=）；④GUI 设置+TUI /remote relay 两处入口，状态（未配置/连接中/在线/退避次数）；⑤空配置零出站连接（抓包/连接日志断言）；⑥测试；⑦证据。
- **验证**：`node scripts/verify-remote.mjs --task R18`（前台，日志 target/test-logs/R18.log；外部放行项另见任务卡）。
- **断言**：R18.A1 配置持久化与状态机；空配置不发起任何外网连接。
- **失败处理**：先保留失败现场日志定位根因；同方案第二次失败换受约束实现；外部依赖（真机/商店/VPS）缺失时用 adapter/fixture 完成 implementation_verified，真实放行记入 production 清单不阻塞。
### R19 PWA/App 经中继配对连接

- **结果**：外网路径配对与读写；直连优先/回落中继；纵深吊销。
- **依赖**：R18。**注意**：此时原生 App 尚在 R21+；本任务用 PWA（移动浏览器）验证客户端逻辑，R23 复用 TS core。
- **固定约束**：遵守 §2 冻结决策中本任务相关条目（能力/加密/默认面）；不削弱既有验收。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **产物**：实现模块、正/负向测试、回归验证；落点先以仓库搜索确认（不预设虚构路径）。
- **步骤**：①客户端连接策略（同局域网探测直连→失败回落 relay，可手动强制）；②经中继完成 Noise 配对（密语错误失败）；③事件/发送/取消端到端；④纵深防御：设备在 daemon 吊销后即使中继仍接受连接也被 daemon 拒绝；⑤网络拓扑测试用本地 loopback relay + 代理限速模拟外网（真实 4G 归 R28 外部放行）；⑥证据。
- **验证**：`node scripts/verify-remote.mjs --task R19`（前台，日志 target/test-logs/R19.log；外部放行项另见任务卡）。
- **断言**：R19.A1（integration）中继 e2e 与密语错误拒绝；R19.A2（security）吊销在 daemon 侧生效（中继放行也无用）；R19.A3 直连/回落切换正确。
- **失败处理**：先保留失败现场日志定位根因；同方案第二次失败换受约束实现；外部依赖（真机/商店/VPS）缺失时用 adapter/fixture 完成 implementation_verified，真实放行记入 production 清单不阻塞。
### R20 中继部署物料与安全验收

- **结果**：最小 VPS 可部署；运维手册；安全守卫。
- **依赖**：R19。
- **固定约束**：遵守 §2 冻结决策中本任务相关条目（能力/加密/默认面）；不削弱既有验收。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **产物**：实现模块、正/负向测试、回归验证；落点先以仓库搜索确认（不预设虚构路径）。
- **步骤**：①Dockerfile（非 root、多阶段、最小镜像）+ compose（Caddy/自带 ACME 二选一冻结）或 systemd unit；②1vCPU/512MB 实测启动与内存；③ACME/域名文档；④升级、日志轮转、限速配置、备份（中继无状态，备份=配置）；⑤verify-relay 守卫脚本；⑥证据。
- **验证**：`node scripts/verify-remote.mjs --task R20`（前台，日志 target/test-logs/R20.log；外部放行项另见任务卡）。
- **断言**：R20.A1（reliability）最小规格起服务+转发；R20.A2（script）密文不可见/注册码一次性/吊销纵深/默认无中继配置四守卫。
- **失败处理**：先保留失败现场日志定位根因；同方案第二次失败换受约束实现；外部依赖（真机/商店/VPS）缺失时用 adapter/fixture 完成 implementation_verified，真实放行记入 production 清单不阻塞。
### R21 RN 工程与 TS core 抽包

- **结果**：mobile/ 裸 RN 工程双端可构建；平台无关 TS core 被 PWA 与 RN 共同消费，5 个平台接口有双实现。
- **依赖**：R20（中继面冻结后再做移动终态；R07 的 TS 投影作为前身）。**约束**：F13/F14；mobile/ 不引入 tauri；core 不 import DOM-only/RN-only API。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **产物**：实现模块、正/负向测试、回归验证；落点先以仓库搜索确认（不预设虚构路径）。
- **步骤**：
  1. **预检**：确认 Node 版本、RN CLI 环境；记录裸 RN vs Expo prebuild 决策（默认裸 RN：可完全自管签名/无 Expo 云依赖）。
  2. **契约**：冻结 TS core 接口——`RemoteCore`（connect/pair/send/cancel/subscribe/decide/listTasks/taskDetail）、`PlatformAdapter`（transport: WS、secureStore、camera、notifications、haptics）、事件投影器（EventEnvelope→UI model）。
  3. **抽包**：把 R07/R07b 的 remote/core 移到平台无关位置（packages/remote-core 或 mobile/core 共享构建），DOM 依赖隔离在 PWA adapter。
  4. **RN 实现**：WS（react-native 网络栈）、secureStore（iOS Keychain/Android EncryptedSharedPreferences 库）、notifications/camera/haptics 桩接口先打通。
  5. **装配**：双端 debug 包启动到配对页（无功能数据）；导航骨架（任务/审批/设置三 tab）。
  6. **测试**：core 的 Jest 单测在 node 与 RN 测试环境双跑；metro bundle iOS/Android 构建成功（CI matrix）；adapter 契约测试。
  7. **回归**：PWA 改用共享 core 后行为不回归（R07/R07b 断言重跑）。
  8. 注册断言/证据。
- **验证**：`node scripts/verify-remote.mjs --task R21`（前台，日志 target/test-logs/R21.log；外部放行项另见任务卡）。
- **断言**：R21.A1（unit）core 同套握手/指纹/投影测试 node 与 RN 环境双绿；R21.A2（build）CI 产出 iOS 与 Android debug 构建产物。
- **失败处理**：先保留失败现场日志定位根因；同方案第二次失败换受约束实现；外部依赖（真机/商店/VPS）缺失时用 adapter/fixture 完成 implementation_verified，真实放行记入 production 清单不阻塞。
### R22 原生配对（相机+安全存储）

- **结果**：真机/模拟器扫码完成两路配对；令牌在系统安全区；错指纹硬拒。
- **依赖**：R21、R09。**约束**：F3/F4/F10；相机权限按需请求；iOS 用途描述文案明确“连接你自己的电脑”。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **产物**：实现模块、正/负向测试、回归验证；落点先以仓库搜索确认（不预设虚构路径）。
- **步骤**：①相机扫码（react-native-vision-camera 或等价，许可宽松）解析 rcode://pair；②手动码路径；③Keychain/Keystore 存取 token/指纹，断言不进 UserDefaults/明文 prefs/日志；④TOFU 钉扎 UI（首次指纹确认页说明主机身份）；⑤经局域网与中继两路配对（复用 R19 core）；⑥错误态（过期码/错指纹/不可达/拒绝权限）；⑦iOS Info.plist 用途串、Android 运行时权限；⑧双端 e2e（Detox/Maestro 至少一个）；⑨证据。
- **验证**：`node scripts/verify-remote.mjs --task R22`（前台，日志 target/test-logs/R22.log；外部放行项另见任务卡）。
- **断言**：R22.A1（e2e）两路配对成功；坏码/坏指纹拒绝；R22.A2（security）存储位置断言（平台安全 API 调用证据+明文 grep 无 token）。
- **失败处理**：先保留失败现场日志定位根因；同方案第二次失败换受约束实现；外部依赖（真机/商店/VPS）缺失时用 adapter/fixture 完成 implementation_verified，真实放行记入 production 清单不阻塞。
### R23 原生任务列表与会话屏

- **结果**：对照原型屏②③的原生体验；事件实时、发送/取消/排队、重连不丢。
- **依赖**：R22。
- **固定约束**：遵守 §2 冻结决策中本任务相关条目（能力/加密/默认面）；不削弱既有验收。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **产物**：实现模块、正/负向测试、回归验证；落点先以仓库搜索确认（不预设虚构路径）。
- **步骤**：①列表（真实 task.list 字段，分组运行中/最近）；②会话屏（事件投影消息流/工具卡/run 状态/usage 元信息）；③输入（发送、运行中排队提示、中止）；④长输出折叠；⑤连接状态条与重连（after_seq）；⑥深色 obsidian 主题与字号/命中区（iOS 44pt、Android 48dp 最小）；⑦动态字体/VoiceOver/TalkBack 标签；⑧e2e：列表→会话→发送→取消、杀进程重连续传；⑨证据。
- **验证**：`node scripts/verify-remote.mjs --task R23`（前台，日志 target/test-logs/R23.log；外部放行项另见任务卡）。
- **断言**：R23.A1（e2e）主流程；R23.A2（visual/a11y）命中区/无横向滚动（390×844 与小屏）/深色一致性。
- **失败处理**：先保留失败现场日志定位根因；同方案第二次失败换受约束实现；外部依赖（真机/商店/VPS）缺失时用 adapter/fixture 完成 implementation_verified，真实放行记入 production 清单不阻塞。
### R24 原生审批体验

- **结果**：聚合 tab+通知深链审批；高敏桌面二次确认态；决策审计。
- **依赖**：R23、R12。
- **固定约束**：遵守 §2 冻结决策中本任务相关条目（能力/加密/默认面）；不削弱既有验收。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **产物**：实现模块、正/负向测试、回归验证；落点先以仓库搜索确认（不预设虚构路径）。
- **步骤**：①审批 tab（approvals.list）+详情卡（命令全文/参数/风险档/来源）；②批准/拒绝+乐观更新回滚；③无 decide 能力隐藏按钮；④needs_desktop_confirm 态不可在手机终决（显示引导）；⑤从通知深链进详情（R25 联动）；⑥e2e 真插件审批；⑦证据。
- **验证**：`node scripts/verify-remote.mjs --task R24`（前台，日志 target/test-logs/R24.log；外部放行项另见任务卡）。
- **断言**：R24.A1（e2e）审批贯通与权限隐藏；R24.A2 高敏态阻断手机终决。
- **失败处理**：先保留失败现场日志定位根因；同方案第二次失败换受约束实现；外部依赖（真机/商店/VPS）缺失时用 adapter/fixture 完成 implementation_verified，真实放行记入 production 清单不阻塞。
### R25 推送句柄与原生通知（APNs/FCM）

- **结果**：待审批/完成推送触达（fake 闭环实现验收）；正文无敏感内容；句柄随设备登记可吊销。
- **依赖**：R24。**约束**：F15；无账号（句柄=设备登记字段，经 relay 投递）。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **产物**：实现模块、正/负向测试、回归验证；落点先以仓库搜索确认（不预设虚构路径）。
- **步骤**：
  1. 设备登记 push_handle{platform, token}（iOS APNs/Android FCM）；relay 存 owner→handles（加密静止、可吊销）。
  2. relay push 适配（APNs HTTP2 用 key 鉴权；FCM v1 API；密钥属自托管运维密钥，不进仓库）。
  3. daemon 事件 approval.requested/run.completed→通知 relay 推送；正文通用（“1 项工具调用待审批”），不含命令。
  4. App 前台降级页内通知；点开经 E2EE 取明文详情。
  5. **fake push adapter**（测试构建注入）：句柄登记/吊销/前台降级全闭环，断言通知正文快照无命令/路径。
  6. 吊销设备后不推送；句柄失效（卸载）清理。
  7. implementation 验收=fake；真实 APNs/FCM 投递归 production_release_ready（需开发者账号与真机，写入手册清单）。
  8. 证据。
- **验证**：`node scripts/verify-remote.mjs --task R25`（前台，日志 target/test-logs/R25.log；外部放行项另见任务卡）。
- **断言**：R25.A1（integration）fake adapter 登记/触发/吊销/前台降级全闭环；R25.A2（security）通知 payload 快照不含命令内容/文件路径/密钥。
- **失败处理**：先保留失败现场日志定位根因；同方案第二次失败换受约束实现；外部依赖（真机/商店/VPS）缺失时用 adapter/fixture 完成 implementation_verified，真实放行记入 production 清单不阻塞。
### R26 原生设置/诊断屏

- **结果**：对照原型屏④：能力只读、中继信息、连接策略、令牌清除与诊断。
- **依赖**：R23。
- **固定约束**：遵守 §2 冻结决策中本任务相关条目（能力/加密/默认面）；不削弱既有验收。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **产物**：实现模块、正/负向测试、回归验证；落点先以仓库搜索确认（不预设虚构路径）。
- **步骤**：①设备能力展示（授予在桌面端，明确提示）；②中继地址与连接质量/重连日志（最近 N 条可复制）；③直连优先/仅中继切换；④清除本机令牌（本地注销，桌面吊销另在电脑侧）；⑤关于/指纹核对（防中间人自查）；⑥测试；⑦证据。
- **验证**：`node scripts/verify-remote.mjs --task R26`（前台，日志 target/test-logs/R26.log；外部放行项另见任务卡）。
- **断言**：R26.A1 能力只读不可在 App 内自授；清除后回到未配对态。
- **失败处理**：先保留失败现场日志定位根因；同方案第二次失败换受约束实现；外部依赖（真机/商店/VPS）缺失时用 adapter/fixture 完成 implementation_verified，真实放行记入 production 清单不阻塞。
### R27 签名/商店素材/合规

- **结果**：release 可签名构建；合规清单与审核话术齐备；无热更/下载执行路径。
- **依赖**：R25、R26。**约束**：F14；iOS 3.3.2/2.5.2。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **产物**：实现模块、正/负向测试、回归验证；落点先以仓库搜索确认（不预设虚构路径）。
- **步骤**：①签名配置位（证书/Keystore 不进仓库，文档指引）；②App 图标/启动屏/隐私清单（PrivacyInfo）；③网络安全：Android release cleartextTrafficPermitted=false；ATS 配置（仅 wss/https）；④静态分析：grep 验证无运行时 JS 下载/eval 远端代码、无动态库下载；⑤审核话术（自有主机远程终端：配对是显式主机授权，App 不提供云服务/账号/支付）；⑥新建 app-store-review.md 含截图清单/问卷答案；⑦implementation=产物与清单齐；商店提交=外部放行；⑧证据。
- **验证**：`node scripts/verify-remote.mjs --task R27`（前台，日志 target/test-logs/R27.log；外部放行项另见任务卡）。
- **断言**：R27.A1（static）release manifest 安全配置+无热更路径扫描；R27.A2（build）签名 release 构建产出（CI 用自签名/debug keystore 验证流程）。
- **失败处理**：先保留失败现场日志定位根因；同方案第二次失败换受约束实现；外部依赖（真机/商店/VPS）缺失时用 adapter/fixture 完成 implementation_verified，真实放行记入 production 清单不阻塞。
### R28 真机外网验收与文档收口

- **结果**：实现侧全门禁绿；真机蜂窝取证清单待外部放行；用户文档完整。
- **依赖**：R27。
- **固定约束**：遵守 §2 冻结决策中本任务相关条目（能力/加密/默认面）；不削弱既有验收。
- **决策空间**：库选型、内部命名、目录微调等可逆选择按 §0 排序自决并记入 current.yaml；固定安全语义不变。
- **产物**：实现模块、正/负向测试、回归验证；落点先以仓库搜索确认（不预设虚构路径）。
- **步骤**：①loopback relay 全流程 e2e（配对/事件/发送/审批/推送 fake/吊销）作为可离线复跑的实现终验；②production 清单：真机 iOS+Android 经真实中继在蜂窝网络完成观察/发送/审批/真实推送/吊销的操作步骤与预期（待用户有开发者账号+VPS 后执行取证）；③docs/support/guides/remote-control.md 收口（配对/中继部署链接/商店安装/故障排查/安全模型）；④最终 `--through R6`；⑤progress/冻结状态更新；⑥证据。
- **验证**：`node scripts/verify-remote.mjs --task R28`（前台，日志 target/test-logs/R28.log；外部放行项另见任务卡）。
- **断言**：R28.A1（e2e）loopback 全链路；R28.A2（doc）用户文档含安装→配对→中继→审批→吊销完整路径。
- **失败处理**：先保留失败现场日志定位根因；同方案第二次失败换受约束实现；外部依赖（真机/商店/VPS）缺失时用 adapter/fixture 完成 implementation_verified，真实放行记入 production 清单不阻塞。
