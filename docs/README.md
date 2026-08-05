# R-Code 文档

这里保存 GitHub 可以直接预览的当前文档。代码行为与文档冲突时，以当前测试通过的代码为准，并在同一个变更中修正文档。

## 维护者入口

| 文档 | 用途 |
| --- | --- |
| [架构与实现细节](./ARCHITECTURE.md) | 运行边界、crate 分层、Agent loop、存储、安全、终端、前端和扩展路径 |
| [联网工具与 MCP](./mcp.md) | 原生联网、MCP 管理、Registry、安全确认、跨平台启动和故障恢复 |
| [演进记忆](./memory.md) | 全局/项目作用域、自动触发、Reviewer、审批、注入、持久化与隐私边界 |
| [Plan 模式与增强审核](./plan-mode.md) · [English](./plan-mode.en.md) | 目标、结构化人工确认、Plan 投影、功能待办、增强审核、并发与崩溃恢复 |
| [发布手册](./RELEASING.md) | 版本、CHANGELOG、tag、GitHub Release、签名、失败恢复和首次发布清单 |
| [Codex → R-Code 委派复核记录](./review/2026-08-05-uncommitted-codex-rcode-delegation-review.md) | 0.3.0 前对未提交实现的多轮审阅、修复证据、剩余边界与最终验证记录 |
| [CHANGELOG](../CHANGELOG.md) | 每个版本的用户可见变化与发布历史 |
| [Security Policy](../SECURITY.md) | 支持范围、私密漏洞报告和安全边界 |
| [Privacy Notice](../PRIVACY.md) | 本地存储、模型 Provider、Codex、更新和支持包的数据流 |
| [English README](../README.md) / [简体中文 README](../README.zh-CN.md) | 产品概览、快速开发、验证命令和仓库导航 |

## UI 参考图

- [`ui/light/`](./ui/light/)：亮色 UI 原型图。
- [`ui/dark/`](./ui/dark/)：暗色 UI 原型图。
- [`ui/signature-dark/`](./ui/signature-dark/)：R-Code 个性化暗色原型图。

`ui/` 只存放静态参考图，不包含可执行 Demo、生成脚本或实现合同。历史资料如需恢复，应放在 `archive/` 并明确标注不再是实现基线。
