# Changelog

R-Code 的用户可见变化记录在此。格式参考 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)，版本号遵循 [Semantic Versioning](https://semver.org/lang/zh-CN/)。

提交和 Pull Request 是实现级历史；本文件记录每个发布版本对用户和运维者有意义的变化。

## [Unreleased]

### Added

- 基于 Tauri 2、Rust、React 和 TypeScript 的跨平台 AI 编程桌面工作台。
- 任务、会话分支、消息队列、流式时间线、回放、变更审核、验证与崩溃恢复链路。
- 原生模型 Provider 与 Codex CLI/MCP 协作，可按策略委派只读或完整访问的子智能体。
- 带工作区路径边界、动态风险分级、审批和审计记录的统一 Tool Gateway。
- SQLite 产品状态与 JSONL 会话事件双存储，以及内容寻址 Blob、基线和回滚能力。
- 基于 PTY 与 OSC 133 的集成终端，支持原始输出增量读取和外部 CLI 会话解析。
- Windows x64、macOS Apple Silicon 与 Linux x64 的 GitHub Actions 发布矩阵及 Tauri 自动更新产物。
- Windows 品牌安装器，支持自定义安装位置、快捷方式选项、真实阶段进度、取消保护和完成后启动。
- GitHub 可直接预览的架构、发布、安全和隐私文档，以及版本一致性检查脚本。

### Security

- Provider 密钥保存到操作系统凭据库；旧版配置中的明文密钥会在启动时尝试迁移。
- R4 高危命令前置拒绝，R3/R4 授权不能保存为长期放行规则；子智能体默认只读。
- 文件工具在解析符号链接和非现存路径祖先后再次校验工作区 containment，并采用 fail-closed 行为。

### Fixed

- 自动更新源与 Cargo 仓库元数据改为当前 GitHub 仓库，避免客户端从错误仓库查询 `latest.json`。
- Windows 安装器、卸载器与应用程序使用 R-Code 图标，release 启动不再打开命令行窗口。
