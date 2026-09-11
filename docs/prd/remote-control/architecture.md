# 远程控制架构说明

> 配套 [实施计划](./plan.md)。本文冻结连接生命周期、握手、令牌/能力、事件扇出与中继接口；任务实现以本文为契约。

## 1. 连接拓扑

```
┌─────────────────────────── r-code-service（单 owner，ProfileLock） ───────────────────────────┐
│                                                                                                │
│  IpcListener（现状）              RemoteListener（新增，R1）                                    │
│  ├ Windows named pipe            ├ WS / TLS over TCP，仅配对后监听                             │
│  └ Unix socket 0600               ├ 每连接：TLS 握手 → 设备令牌认证 → 能力注入                  │
│        │                                │                                                      │
│        └──────────────┬─────────────────┘                                                      │
│                       ▼                                                                        │
│            CommandDedup（(profile, client_id, command_id) 持久回执，跨 transport 共享）        │
│                       │                                                                        │
│                       ▼                                                                        │
│            ApplicationHandler → ApplicationService → RunManager（零改动）                      │
│                                                                                                │
│  DeviceRegistry（新增）：devices/<id>.json（令牌哈希、能力、名称、配对时间、最后连接、吊销位）   │
│  PairingSession（新增，内存态）：pair_secret、TTL、一次性                                       │
│  EventFanout（新增）：journal append → 已认证远程订阅连接                                      │
└───────────────────────────────────────────────────────────────────────────────────────────────┘
        ▲ 本机                                   ▲ 局域网（R1–R4）        ▲ 公网（R5，经中继出站）
   GUI / TUI / MCP                            手机 PWA（浏览器）        PWA ─ relay ─ daemon(出站)
```

## 2. 监听生命周期

1. daemon 启动时 RemoteListener **不启动**（默认面为零）。
2. `device.pairingStart`（仅本机 transport 可调）→ 生成 PairingSession：
   - `pair_secret`：≥128 位，base32 分组显示（如 `RCODE-7K2M-...`）；
   - TTL 120 秒；消费一次即销毁；进程重启不保留；
   - 返回 `{pair_secret, qr_payload, lan_endpoints[], expires_at}`；
   - 此时 RemoteListener 临时绑定候选网卡的私网地址（RFC1918/链路本地），端口可固定或随机。
3. `device.pair`（在临时监听口上，凭 pair_secret）→ 颁发设备令牌 → PairingSession 销毁 → 监听器转为“正式监听”（只要 ≥1 台未吊销设备；最后一台吊销后自动关监听）。
4. `device.pairingStop`/过期/daemon 重启：临时口关闭；正式监听的开关持久化在 `devices/listener.json {enabled, bind_addresses}`，但默认 `enabled=false`，且每次 OS 启动后仍需至少一台有效设备才实际绑定。
5. `device.setListener {enabled:false}`（仅本机）→ 立即关监听，所有远程连接断开（设备不吊销，可再开）。

## 3. TLS 与证书

- 自签 CA/服务证书在**首次启用配对时**生成，私钥进平台凭据存储（复用 `r_code_core::secret`：Win keyring / macOS 加密文件 / Linux keyring，服务名 `r-code-remote`）。
- 证书指纹（SHA-256）放进 QR 载荷；PWA 首次连接**钉死指纹**（TOFU），之后不匹配即拒连并提示重新配对。
- 不依赖公共 CA（局域网 IP/主机名无证书）；不接受用户点“继续访问”式绕过——指纹不匹配是硬错误。
- R5 中继场景：TLS 到中继一层（中继域名有公共证书）；端到端一层（daemon 自签证书指纹同样在配对时钉死）。

QR 载荷（v1）：

```text
rcode://pair?v=1&h=<lan-ip|mdns-name>&p=<port>&s=<pair_secret>&fp=<sha256-hex>
```

PWA 解析后直接发起 `wss://h:p/remote/v1`；同一路径支持手动输入。

## 4. 设备认证握手（应用层，帧 1）

WebSocket/TLS 连接后第一帧：

```jsonc
// → daemon
{"hello": "r-code-remote/1", "device_id": "<id>", "token": "<设备令牌>", "client_id": "<复用 device_id>"}
// ← daemon
{"ok": true, "capabilities": ["events:read", "tasks:write"], "profile": "development", "server": {"version": "..."}}
// 或
{"ok": false, "error": {"code": "unauthorized" | "revoked" | "capability_missing"}}
```

- 设备令牌：32 字节随机，仅在配对响应中明文出现一次；daemon 只存 `SHA-256(token)`。
- `client_id = device_id`：命令去重天然按设备隔离；同一设备 PWA 重连带相同 command_id 返回原结果。
- 认证失败立即关连接（指数退避留给客户端，daemon 不做账户锁定以外的复杂策略；同 IP 暴力尝试计数并在 pairing 关闭后只记日志）。

## 5. 能力模型

```text
events:read      只读：events/list/detail/branches/models.available/plugins.list
tasks:write      task.create/sendMessage/cancel/rename/clone
approvals:decide 审批 pending op 的批准/拒绝（R3；每次决策审计落 journal）
```

- `settings.*`、`plugins.install/remove/setEnabled`、`device.*`、`service.shutdown`：**任何能力组合都不可远程调用**，分发器硬编码拒绝（`forbidden_remote_method`）。
- 能力在配对时由用户在桌面端勾选；`device.updateCapabilities` 仅本机可调；变更对新连接生效（已建立连接立即重算，收窄时推送 `capabilities_changed` 并断开不再满足的长订阅）。

## 6. 命令与事件（复用既有协议）

- 远程连接上的命令帧仍是 `ApplicationCommand/ApplicationResult`（JSON-RPC 风格，与管道同形）；CommandDedup 不区分 transport。
- 事件订阅：连接认证后可发 `events.subscribe {after_seq}`（长连接推送）；daemon 在 journal append 时向匹配 task 范围的订阅连接推 `EventEnvelope`（与 `task.events` 逐字节同形）。
  - 无 task 过滤参数（首期：订阅=该 profile 全部任务，只读能力持有者即可看全部——配对本身是 owner 授权行为）；
  - 背压：单连接发送缓冲超过上限（1 MiB / 1000 帧）→ 关连接，客户端 after_seq 重连续传，不丢事件。
- 审批事件：待审批 op 以既有事件形状到达；`approvals:decide` 的方法形状与本地审批一致（op 由宿主创建这一前提不变）。

## 7. 设备登记格式

`harness-v2/<profile>/devices/registry.json`：

```jsonc
{
  "version": 1,
  "devices": [
    {
      "id": "dev_<rand>",
      "name": "iPhone 15",
      "platform": "ios-pwa",
      "token_sha256": "<hex>",
      "cert_fingerprint": "<hex>",
      "capabilities": ["events:read"],
      "paired_at": "2026-09-10T12:00:00Z",
      "last_seen_at": null,
      "revoked": false
    }
  ],
  "listener": {"enabled": false, "bind": ["lan"], "port": null}
}
```

令牌密文/哈希外不存任何设备可联系信息（无 push token，首期）。

## 8. PWA 形态

- 交付物：daemon 在**配对模式临时口**与**正式口**上同源托管一组静态资源（`/app`）：单页 bundle，由前端构建产物的一个独立 entry 产出（复用现有 React 投影层；client 从 `invoke("cmd_*")` 编译特性切换为 WebSocket transport）。
- PWA manifest + Service Worker：可加主屏、全屏、离线壳（断网显示重连，不缓存任务数据）。
- 页面：会话列表（task.list）、会话视图（事件流投影，复用 transcript 投影）、输入框（tasks:write）、中止、审批卡片（approvals:decide）、设备信息页。
- 不做账号/多 profile 切换 UI（配对即绑定一个 profile；要换 profile 重新配对）。

## 9. 中继接口（R5 冻结，不实现）

- relay 公开 wss，认证对象只有两类：daemon（持 owner 注册码，桌面端生成、一次性输入/扫码给 PWA）与 device（持设备令牌，同 §4）。
- 路由键 = profile owner id + device id；relay 只做字节转发与限速，E2EE：配对阶段 PWA 经中继完成与 daemon 的 Noise XX，relay 无会话密钥。
- daemon 侧 `RelayTransport` 是 IpcTransport 的第三种实现：对上层仍是 ApplicationFrame 字节流，ApplicationHandler 无感知。
- relay 代码位置预留 `crates/r-code-relay/`，本计划不创建。

## 10. 与 Harness v2 不变量的兼容

| v2 不变量 | 远程控制的遵守方式 |
| --- | --- |
| 单 owner（OS 锁） | 远程是 owner 持有的额外终端，不产生第二个 daemon；锁语义不变 |
| 审批只认宿主 pending op | 远程决策引用同一 op id；op 创建/作用域/单 op 语义零改动 |
| verified 只能 kernel 签发 | 远程无任何裁决写路径 |
| 凭据不出机 | settings/plugins/device 管理方法硬编码不可远程 |
| 命令持久去重 | 复用 CommandDedup，device_id 作 client_id |
| 事件来源区分 | 远程只读 EventEnvelope，无伪造 Provenance 的写口 |
