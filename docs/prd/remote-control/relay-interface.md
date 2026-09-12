# 中继线协议冻结（R15 设计门）

> 本文把 [relay.md](./relay.md) §2–§5 的设计落到**字段级**，冻结后 R16/R17 按此实现，
> 变更走 PRD 修订。配套 [architecture.md](./architecture.md) §9。
> 状态：frozen（2026-09-12，R15 设计评审通过）。

## 1. 传输与帧格式

- 中继公开一个 WS 端点：`wss://<relay>/relay`（owner 与 device 同端点，按首帧分流）。
- **控制帧**（文本帧，UTF-8 JSON）：注册、路由、心跳、错误。
- **数据帧**（二进制帧）：端到端密文（Noise transport message），中继零解析、零持久化。

### 1.1 控制帧（字段级）

```jsonc
// ── owner（daemon）侧 ──
{"type":"owner.register","code":"<hex-64>","pubkey":"<b64>","sig":"<b64>"}
// sig = owner 私钥对 ("rcode-owner-register" || pubkey) 的 Ed25519 签名
{"type":"owner.resume","ownerId":"ow_<hex-16>","pubkey":"<b64>","sig":"<b64>"}
// sig = 对 ("rcode-owner-resume" || ownerId) 的签名；重连恢复路由
{"type":"owner.welcome","ownerId":"ow_<hex-16>"}

// ── device（PWA/App）侧 ──
{"type":"device.hello","ownerId":"ow_<hex-16>","devicePubkey":"<b64>","nonce":"<hex-32>"}
{"type":"device.challenge","challenge":"<hex-64>"}
// device 以设备私钥对 challenge 签名：
{"type":"device.answer","sig":"<b64>"}
{"type":"device.welcome","deviceId":"dv_<hex-16>"}

// ── 通用 ──
{"type":"ping"} / {"type":"pong"}          // 心跳（owner 与 relay 间；device 与 relay 间）
{"type":"error","code":"<code>","detail":"<短句>"}
{"type":"route.open"}                       // owner 在线且 device 已绑定：开始转发
{"type":"route.closed","code":"<close-code>"}
```

规则：

- 未知 `type` → `error{code:"bad_frame"}` 并断连（fail closed）。
- 首帧必须是对应侧的 register/resume/hello；先发其他帧 → `error{code:"not_authenticated"}`。
- 注册/绑定成功后，双方进入数据帧通道；此后任何控制帧除 ping/pong/error 外忽略。

### 1.2 数据帧（二进制）

字节布局 = Noise transport message：`[AEAD 密文 || 16B tag]`，载荷内部再含长度前缀
的 ApplicationFrame/WS 子帧（对中继完全不透明）。方向由 Noise 会话角色决定
（device=initiator，owner=responder），无需路由头——中继按连接的注册身份转发。

## 2. 认证流程（时序）

### 2.1 owner 注册（首配，一次性注册码）

```
桌面端                     daemon                        relay
  │ 生成注册码(32B 随机,      │                             │
  │ TTL 10min, 仅显示一次)    │                             │
  ├─────────────────────────►│                             │
  │                          │ owner.register(code,pubkey,sig)
  │                          ├────────────────────────────►│ 校验 code 未用未过期
  │                          │                             │ ownerId = BLAKE3(pubkey)[..16]
  │                          │ owner.welcome(ownerId)      │ 注册表: ownerId→{pubkey, conn}
  │                          │◄────────────────────────────┤
```

- 注册码错/过期/已用 → `error{code:"owner_code_invalid"|"owner_code_expired"|"owner_code_used"}`。
- 注册成功后注册码立即作废（一次性）。

### 2.2 owner 重连（daemon 常驻长连接断线后）

```
daemon → owner.resume(ownerId,pubkey,sig) → relay 校验签名匹配注册表 → owner.welcome
```

- `ownerId` 未知或签名不符 → `error{code:"owner_unknown"}` 断连。

### 2.3 device 绑定与连接

```
PWA                        relay                         daemon(owner)
  │ device.hello(ownerId,    │                              │
  │  devicePubkey, nonce)    │                              │
  ├─────────────────────────►│ 查绑定表                      │
  │ device.challenge(chal)   │                              │
  │◄─────────────────────────┤                              │
  │ device.answer(sig=Sign(devicePriv, chal))               │
  ├─────────────────────────►│ 校验签名                     │
  │ device.welcome(deviceId) │                              │
  │◄─────────────────────────┤                              │
  │      （此后数据帧双向转发；owner 离线时见 §4 close code） │
```

- `ownerId` 无活跃 owner 连接 → `error{code:"owner_offline"}`（device 明确错误，不静默挂起）。
- devicePubkey 未绑定该 owner → `error{code:"device_unbound"}`。
  绑定动作发生在 E2EE 配对（§3）内：daemon 在加密通道里发
  `{bind: devicePubkey}`，中继由 owner 连接以数据帧形态**不可见**——绑定表
  实际由 owner 通过控制帧 `{"type":"owner.bind","devicePubkey":...,"sig":...}` 维护
  （owner 签名证明绑定意图；中继只验签，不理解语义）。绑定成功后中继回
  `{"type":"owner.bind.ack","devicePubkey":...}`（v1.1：console 可时序化配对——
  先 ack 后设备拨入）。

## 3. 端到端密钥协商（Noise XX + PSK 时序）

角色：device=initiator，owner=responder。协议 = Noise XX 模式，PSK=配对密语
（QR 中的一次性 `s=`，≥128bit）：

```
→ e                          （device 临时公钥）
← e, ee, s, es               （owner 临时+静态）
→ s, se                      （device 静态；PSK 混入第三步）
```

- **PSK 验证失败（配对密语错）** = 第三步后的 MAC 校验失败 → 双方立即断连；
  中继只看到连接关闭，无法区分原因。
- **owner 静态公钥钉扎**：device 在握手后校验 owner 静态公钥指纹 == QR 中
  `fp=`（SHA-256，与局域网配对同源）；不符即断连（TOFU，F3 同一不可绕过语义）。
- 握手完成后的 `chaining key` 派生双向 transport 密钥；数据帧即 Noise
  transport message（自动包含 nonce 单调与重放防护）。
- rekey：每 1 小时或 2^32 条消息（先到者）重新走 XX 握手（同 PSK）。
- 帧序号/计数器由 Noise 协议内部维护；上层不重复实现。

## 4. 错误码与客户端行为（无悬空错误）

| code | 语义 | owner 侧行为 | device 侧行为 |
| --- | --- | --- | --- |
| `owner_code_invalid` | 注册码格式错 | 提示重新生成 | — |
| `owner_code_expired` | 注册码超 10min | 提示重新生成 | — |
| `owner_code_used` | 已被使用 | 提示重新生成 | — |
| `owner_unknown` | ownerId 不在注册表 | 重新 register | 提示主机离线 |
| `owner_offline` | owner 无活跃连接 | — | 明确"电脑不在线"，指数退避重试 |
| `device_unbound` | 设备公钥未绑定 | — | 引导重新配对 |
| `not_authenticated` | 首帧不是认证帧 | 修正实现 | 修正实现 |
| `bad_frame` | 未知 type/解析失败 | 断连重连 | 断连重连 |
| `rate_limited` | 超 §5 限流 | 退避重连 | 退避重连 |
| `relay_full` | 连接数超上限 | 退避重连 | 退避重连 |

WS close code 附加语义：`4000=owner 主动关闭路由`，`4001=server 关闭`，
`4002=协议违规`（伴随 error 帧之前）。客户端收到 close 后按上表对应行为退避。

## 5. 重连与限流（数值冻结）

### 5.1 重连策略（owner 与 device 同一形状）

- 指数退避：`min(30s, 2^(attempt-1) s)`，抖动 ±20%；
- 认证类错误（`owner_code_*`/`device_unbound`/`revoked`）**不自动重试**（需要用户行动）；
- 心跳：client 每 30s `ping`，60s 无 `pong` 判死断连。

### 5.2 限流（单实例默认，配置可覆盖但默认值冻结）

| 维度 | 数值 | 超限动作 |
| --- | --- | --- |
| 每 owner 并发连接 | 1（新连接踢旧连接） | 旧连接 close 4001 |
| 每 relay 总连接 | 256 | 新连接 `relay_full` |
| device 数据帧大小 | 64 KiB 上限 | 超大帧直接断连 |
| device 发送速率 | 突发 10 帧 / 持续 10 帧/s | 连续 5s 超限 → `rate_limited` 断连 |
| owner→device 吞吐 | 256 KiB/s，突发 1 MiB | 排队；队列 >4 MiB → `rate_limited` |
| 认证失败尝试 | 每 IP 10 次/10min | IP 级拉黑 10min（内存态） |

- 限流计数器与注册表均为**内存态**（进程重启清零）；审计日志仅连接元数据
  （时间、方向、字节数、close code），无内容，按大小轮转。

## 6. 与 R17 RelayTransport 的实现接口对齐

RelayTransport 是 `IpcTransport` 的第三种实现（F12），对上仍是 `AppStream`
（R00：AsyncRead + AsyncWrite + Unpin + Send）。接口形状：

```
RelayTransport::connect(relay_url, owner_keys, &mut identity) -> AppStream
  - 内部：wss 连接 → owner.register/resume → route.open → Noise responder 会话
  - 返回的流 = Noise 解密后的 ApplicationFrame 字节流（与管道/局域网逐字节同形）
DeviceRelayStream::connect(relay_url, owner_id, device_keys, psk, pinned_fp) -> AppStream
  - 内部：wss 连接 → device.hello/challenge → Noise initiator 会话 → 钉扎校验
```

- 命令行为一致性：同一 DaemonClient 语义（CommandDedup 按 (profile, client_id,
  command_id)），F7 重放不区分 transport；
- 纵深防御：中继放行的连接在 daemon 侧仍走 token/能力校验（吊销在中继仍
  连通时也生效，R19.A2）。

## 7. 评审检查单（R15 设计门通过记录）

- [x] 每个错误码都有客户端行为（§4 表，无悬空）
- [x] 帧格式字段级（§1），未知 type fail closed
- [x] E2EE 时序字段级（§3），密语错误=第三步 MAC 失败
- [x] owner 注册/恢复/设备绑定全流程（§2）
- [x] 重连与限流数值冻结（§5）
- [x] 与 R17 实现接口对齐（§6，AppStream 同形）
- [x] 无 TBD/TODO/FIXME
