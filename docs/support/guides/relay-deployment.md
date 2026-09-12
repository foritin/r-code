# r-code-relay 部署手册（R20）

自托管中继的最小运维面。中继是无状态字节转发器（F12）：不持有会话密钥、
不解密内容、不落盘任务数据。被攻破的最坏情况 = 断网 + 连接元数据泄露。

## 1. 最小规格与运行

- 1 vCPU / 512MB 内存即可（实测启动 < 20MB RSS，转发为 IO 密集）。
- 开放 443（TLS 由 Caddy/nginx 终结，或 relay 前置自备证书网关）。
- 非 root 运行（systemd `User=rcode-relay` 或容器 `USER` 指令）。

### systemd

```ini
# /etc/systemd/system/r-code-relay.service
[Unit]
Description=R-Code self-hosted relay
After=network-online.target

[Service]
User=rcode-relay
Environment=R_CODE_RELAY_BIND=127.0.0.1:8787
# 一次性 owner 注册码（32B hex），由桌面端 relay.issueCode 生成后导入；
# 用掉即失效。轮换：追加新码后 systemctl restart。
Environment=R_CODE_RELAY_CODES=<hex-64>
ExecStart=/usr/local/bin/r-code-relay
Restart=on-failure
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true

[Install]
WantedBy=multi-user.target
```

Caddy 反代（自动 ACME）：

```
relay.example.com {
    reverse_proxy 127.0.0.1:8787
}
```

## 2. 升级 / 日志 / 备份

- **升级**：替换单个二进制 + restart。注册表是内存态——重启后 owner 用
  长期密钥 `owner.resume` 重连，设备绑定同样持久于 daemon 侧的重连注册
  （v1 单实例内绑定表随重启清空，device 重新 dial 时 owner 重发
  `owner.bind`，行为见 relay-interface.md v1.1）。
- **日志**：仅连接元数据（方向/身份哈希/字节数）。systemd journal 自带
  轮转（`journald.conf SystemMaxUse=200M` 建议）。
- **备份**：无状态——备份 = 配置文件（service 文件 + 码列表）。

## 3. 安全验收清单（R20.A2 四守卫）

| 守卫 | 验证方式 | 状态 |
| --- | --- | --- |
| 密文不可见 | 用已知随机串过桥，审计 dump 不含该串（r16_a2） | ✅ implementation_verified |
| 注册码一次性 | 同码二次注册 → `owner_code_used`（r16_a1） | ✅ |
| 吊销纵深 | 中继放行的设备，daemon 侧仍按吊销状态拒绝（r19_a2） | ✅ |
| 默认无中继配置 | 未配置 url 时 daemon 零出站拨号（r18_a1） | ✅ |

## 4. 与真实 VPS 的边界（外部放行）

以下属 `production_release_ready`（不在本清单的实现验收内）：

- 真实域名 + Let's Encrypt 证书签发；
- 真实蜂窝/外网网络下的端到端取证（R28 清单）；
- 持续运行的安全更新与监控。
