# Harness Protocol v1

> 位置：`docs/prd/pluggable-harness/protocol-v1.md`（T41 交付）。实现以
> `crates/r-code-harness-protocol` 为权威源。

## 概览

宿主与 Harness 插件之间通过 **stdin/stdout 上的 UTF-8 NDJSON JSON-RPC 2.0** 双向通信：

- stdout 仅承载协议帧；stderr 为有界诊断（尾部 64 KiB 环形保留）。
- 单帧上限 **1 MiB**；两方向待发队列合计上限 **16 MiB**。
- 初始化超时 **10 s**；取消宽限 **5 s** 后终止进程树。
- 独立 reader 在宿主等待响应期间持续服务反向调用（嵌套回调）。
- 每个 Run 一个插件进程；连接绑定 task/run/attempt/generation，句柄不可跨 Run。

## 身份与能力

`harness.json`（schema 见 `crates/r-code-harness-protocol/schema/harness-v1.schema.json`）声明：

`schema_version, id, version, apiMajor/apiMinor, displayName, supportedPlatforms(platform+executable+argv), supportedFeatures, requestedHostServices, configSchema, processProfiles`。

宿主以 `manifest.negotiate(host_api, platform, host_services)` 做**纯函数**协商：版本不匹配、平台缺失、宿主未提供所请求服务 → 在任何进程 spawn 之前失败。

## 方法

宿主→插件：`initialize, harness.start, harness.resume, harness.steer, harness.cancel, shutdown`。

插件→宿主：

| 服务 | 方法 |
| --- | --- |
| 模型 | host.model.stream |
| 工具 | host.tools.list / call |
| 进程 | host.process.open / write / close |
| 上下文 | host.context.read、host.artifacts.put / read |
| 计划 | host.plan.publish / update |
| 交互 | host.questions.ask、host.approvals.request |
| 子任务 | host.children.spawn / wait / cancel |
| 验证 | host.verification.run、host.checkpoint.save |
| 完成 | host.completion.propose |

进度用 `harness.event` 通知；模型/进程流经关联 `stream.event` 通知。

## 持久操作与输入

- JSON-RPC id 仅连接内相关；副作用方法携带 Attempt 内稳定的 `operation_key`。
- 宿主保存规范化输入 hash 与结果：同 key 同 hash → 重放回执；同 key 异 hash → 拒绝；generation 变化不抹去去重历史。
- 用户输入带宿主生成的 `message_id/input_seq`；checkpoint 原子保存 `consumed_input_seq` 且必须为已投递连续前缀。

## 大内容

版本化 `ArtifactRef { schema, blob_id, bytes, sha256, media_type }`：帧内只传引用，字节存宿主内容寻址 Blob 库。

## 来源区分

`Provenance::Host`（宿主执行事实）与 `Provenance::Plugin { harness_id, package_digest }`（插件观察）在任何事件/证据上不混淆——只有 Host 来源可产生验收证据。
