# Changelog

R-Code 的用户可见变化记录在此。格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

提交和 Pull Request 是实现级历史；本文件记录每个发布版本对用户和运维者有意义的变化。

## [Unreleased]

## [0.2.0] - 2026-08-04

### Added

- Plan 模式：任务目标、结构化 human-in-the-loop 问题、稳定 AppData Markdown 投影、按功能拆分的依赖待办和确认实施流程。
- Plan 确认后的可靠实施队列、重启恢复与失败重试，以及不会回滚工作区的二次确认取消流程。
- 增强审核：仅展示当前 Plan 的功能变更，支持功能/文件级接受与拒绝，并通过逆向三方合并保留同一文件中其他功能的改动。
- 中英文双 README 入口，补充 Plan/HITL、增强审核、并发恢复与隐私边界文档。

### Security

- Plan 模式禁止写工具、Shell、变更型 MCP 和委派；等待用户回答后会关闭同一 Run 的后续工具执行。
- 执行中的 Plan 没有活动功能时进入暂停态，直接写入、变更型 Shell 和 MCP 均 fail-closed，直到显式恢复被阻塞事项。
- 功能级拒绝使用路径有序锁、durable journal、rollback Blob 和原子替换；冲突时保持文件不变并要求人工处理。
- 删除会话/项目时按事务引用计数清理审核 Blob 和 UUID Plan 投影；启动清理不信任数据库提供的文件路径。

## [0.1.0] - 2026-08-03

### Added

- 基于 Tauri 2、Rust、React 和 TypeScript 的跨平台 AI 编程桌面工作台。
- 任务、会话分支、消息队列、流式时间线、回放、变更审核、验证与崩溃恢复链路。
- 原生模型 Provider 与 Codex CLI/MCP 协作，可按策略委派只读或完整访问的子智能体。
- 默认关闭的单机演进记忆：成功轮次自动复盘、全局候选审批、项目自动记忆、冻结快照注入与 AppData-only 管理页。
- 无密钥原生联网、可关闭的内置深度调研服务，以及带确认、凭据引用和官方 Registry 搜索的 MCP 管理。
- 带工作区路径边界、动态风险分级、审批和审计记录的统一 Tool Gateway。
- SQLite 产品状态与 JSONL 会话事件双存储，以及内容寻址 Blob、基线和回滚能力。
- 基于 PTY 与 OSC 133 的集成终端，支持原始输出增量读取和外部 CLI 会话解析。
- Windows x64、macOS Apple Silicon/Intel 与 Linux x64 的 GitHub Actions 发布矩阵及 Tauri 自动更新产物。
- Windows Authenticode 强制签名验收，以及自动生成的 CycloneDX SBOM 与第三方许可证清单。
- Windows 品牌安装器，支持自定义安装位置、快捷方式选项、真实阶段进度、取消保护和完成后启动。
- macOS 原生 traffic-light 标题栏、GUI shell PATH 恢复、可见 Codex 登录终端，以及 Developer ID 签名/公证打包脚本。
- GitHub 可直接预览的架构、发布、安全和隐私文档，以及版本一致性检查脚本。

### Security

- Provider 密钥保存到操作系统凭据库；旧版配置中的明文密钥会在启动时尝试迁移。
- R4 高危命令前置拒绝，R3/R4 授权不能保存为长期放行规则；子智能体默认只读。
- 文件工具在解析符号链接和非现存路径祖先后再次校验工作区 containment，并采用 fail-closed 行为。

### Fixed

- 自动更新源与 Cargo 仓库元数据改为当前 GitHub 仓库，避免客户端从错误仓库查询 `latest.json`。
- Windows 安装器、卸载器与应用程序使用 R-Code 图标，release 启动不再打开命令行窗口。

[0.1.0]: https://github.com/foritin/r-code/releases/tag/v0.1.0

[Unreleased]: https://github.com/foritin/r-code/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/foritin/r-code/releases/tag/v0.2.0
