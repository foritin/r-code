# R-Code 文档

这里保存 GitHub 可以直接预览的当前文档。代码行为与文档冲突时，以当前测试通过的代码为准，并在同一个变更中修正文档。

## 维护者入口

| 文档 | 用途 |
| --- | --- |
| [架构与实现细节](./architecture.md) | 运行边界、crate 分层、Agent loop、存储、安全、终端、前端和扩展路径 |
| [联网工具与 MCP](./support/guides/mcp.md) | 原生联网、MCP 管理、Registry、安全确认、跨平台启动和故障恢复 |
| [演进记忆](./support/guides/memory.md) | 全局/项目作用域、自动触发、Reviewer、审批、注入、持久化与隐私边界 |
| [Plan 模式与增强审核](./support/guides/plan-mode.md) · [English](./support/guides/plan-mode.en.md) | 目标、结构化人工确认、Plan 投影、功能待办、增强审核、并发与崩溃恢复 |
| [macOS 真机验证清单](./support/platform/macos-validation.md) | Windows/Linux 无法替代的本地加密凭据、Finder、终端、RTK、MCP 与安装包运行验证 |
| [发布手册](./support/operations/releasing.md) | 版本、CHANGELOG、tag、GitHub Release、签名、失败恢复和首次发布清单 |
| [安装、备份、恢复与卸载](./support/operations/operations.md) · [English](./support/operations/operations.en.md) | 用户/运维人员的安装、升级、完整数据备份、迁移恢复、卸载和支持包流程 |
| [支持材料索引](./support/README.md) | 指南、运维、平台、历史合同、旧 UI 与归档的统一入口和迁移表 |
| [CHANGELOG](../CHANGELOG.md) | 每个版本的用户可见变化与发布历史 |
| [Security Policy](../SECURITY.md) | 支持范围、私密漏洞报告和安全边界 |
| [Privacy Notice](../PRIVACY.md) | 本地存储、模型 Provider、Codex、更新和支持包的数据流 |
| [English README](../README.md) / [简体中文 README](../README.zh-CN.md) | 产品概览、快速开发、验证命令和仓库导航 |

## 当前实施合同

| 文档 | 状态 |
| --- | --- |
| [产品体验重构 PRD / AI 实施清单](./product-experience-redesign/r-code-experience-redesign-prd.md) | `frozen`，42/42 已实施闭环；唯一状态源是 [`worklist-gate.json`](./product-experience-redesign/worklist-gate.json)（`--update-freeze` 重刷），本表不再手写进度 |
| [Codex 主代理丰富交互历史合同](./support/contracts/codex-rich-interaction-prd.md) | 特定 2026-08-25 revision 的 `38/38` 历史证据已通过；当前 dirty `dev` 仍需新清单 M0-02 回归 |
| [Windows 命令可靠性历史合同](./support/contracts/windows-command-reliability-prd.md) | 已完成 revision 的冻结合同；当前位置仅作维护与追溯，不是新的待办状态源 |

## UI 参考图

- [本次产品体验原型](./product-experience-redesign/)：可点击 HTML、关键状态截图、设计说明与当前实施合同。
- [`support/ui-reference/legacy/light/`](./support/ui-reference/legacy/light/)：历史亮色 UI 参考图。
- [`support/ui-reference/legacy/dark/`](./support/ui-reference/legacy/dark/)：历史暗色 UI 参考图。

历史图片只用于对照，不代表当前实现；可执行原型与生成脚本只维护在本次产品体验目录。

## 历史归档

[`support/archive/`](./support/archive/) 保存已实施的一次性方案、实验基线、历史原型和阶段性决策记录。
归档内容不再作为当前实现或待办清单；当前行为以本页维护文档与通过的代码测试为准。
完整目录、归档原因与旧路径映射见 [`support/README.md`](./support/README.md) 和 [`support/archive/readme.md`](./support/archive/readme.md)。
