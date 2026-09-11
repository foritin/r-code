# 远程控制（Remote Control）

R-Code 守护进程的远程接入计划：配对、网络传输、设备能力分档、手机端 PWA 与可选公网中继。

- [实施计划](./plan.md) —— 目标、架构、安全模型、分期与任务索引
- [任务权威源](./tasks.json) —— 原子任务、依赖与验收标准
- [AI 实施工作清单](./worklist.md) —— 长任务执行契约（任务卡/统一验收/恢复协议），AI 连续执行入口
- [统一验收入口](../../../scripts/verify-remote.mjs) —— `node scripts/verify-remote.mjs --through R0`
- [转换固化清单](./transformation-freeze.yaml) —— 稳定指纹与解冻条件
- [架构说明](./architecture.md) —— 传输/配对/中继的数据流与威胁模型
- [自托管中继部署](./relay.md) —— 自有云服务器/公网 IP 作中继（R5，R16–R20）

状态：`draft`（计划评审中，未开始实施）。
