## 变更摘要

<!-- 用 2–5 句话说明用户问题、解决方式和可见结果。 -->

## 变更内容

- <!-- 变更 1 -->

## 验证

<!-- 列出实际运行的命令和结果；不适用的项目说明原因。 -->

- [ ] 受影响的 Rust 测试通过
- [ ] 前端 `npm test` 通过
- [ ] 前端 `npm run build` 通过
- [ ] `cargo fmt --all -- --check` 通过
- [ ] 已在受影响的 Windows / macOS / Linux 平台实测

## 风险与数据边界

- [ ] 未提交凭据、AppData、记忆正文、真实对话、私有路径或机器专属配置
- [ ] 权限、工作区路径、Provider、MCP、Codex 或子智能体边界没有被意外放宽
- [ ] 迁移、失败恢复、取消和旧版本兼容性已评估
- [ ] `.gitignore`、审核范围和发布产物没有引入敏感文件

## UI 与文档

<!-- UI 变更附脱敏截图或录屏。行为、架构、隐私或发布变化请同步文档。 -->

- [ ] 用户可见变化已写入 `CHANGELOG.md` 的 `Unreleased`
- [ ] 相关架构、Memory、MCP、Privacy、Security 或 Release 文档已同步
- [ ] 不需要文档或截图，并已在上方说明原因

## 提交前确认

- [ ] PR 只包含一个连贯变更，默认目标分支为 `dev`
- [ ] 新功能包含测试，Bug 修复包含回归测试
- [ ] 我已阅读 [CONTRIBUTING.md](https://github.com/foritin/r-code/blob/main/CONTRIBUTING.md) 并同意遵守 [CODE_OF_CONDUCT.md](https://github.com/foritin/r-code/blob/main/CODE_OF_CONDUCT.md)
