# AGENTS.md — R-Code 仓库开发规约

本文件由 AI 编码代理在每次会话开始时自动读取。请先遵循以下约定，再开始任何任务。

## 工具使用速查（避免低级报错）

### 搜索三件套，别混用
- 按**文件名**找文件 → `glob`，必填 `pattern`（如 `**/*.rs`、`**/AGENTS.md`）
- 按**内容**搜文件 → `search_files`，必填 `path` + `pattern`
- 搜**网页** → `search`，必填 `queries`

### 反例（禁止）
- 想搜本地文件却调用网页 `search` 并传 `pattern`（会报 missing `path`）
- 用 `search_files` 却不给 `path`
- 想按文件名找文件却用 `search_files`

### 通则
- 工具报 missing/required 参数时，按报错提示补参，**不要重复原调用**。
- 读文件用 `read_file`，不用 cat/type/find/ls。
- 修改文件用 `edit`（精确替换），不要用 `apply_patch` 整文件重写。
- 多个独立只读操作可在同一轮并行调用。

## 本仓库技术栈
- Rust / Tauri 桌面应用（Crates 位于 `crates/`，Tauri 入口在 `src-tauri/`）
- 文档位于 `docs/`，脚本位于 `scripts/`
- 变更前先 `git_status` 了解当前状态；提交遵循现有 `CHANGELOG.md` 与 `CONTRIBUTING.md` 约定
