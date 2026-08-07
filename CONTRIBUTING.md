# Contributing to R-Code

感谢你愿意改进 R-Code。为了让问题能够复现、变更容易审核、各平台不会互相破坏，请按下面的约定提交 issue 和 pull request。

参与本项目即表示你同意遵守 [Code of Conduct](./CODE_OF_CONDUCT.md)。安全漏洞请走 [私密报告流程](./SECURITY.md)，不要在公开 issue、日志或截图中附带密钥、私有源码或 AppData 原始数据。

## 提交问题前

- 先搜索现有 issue，避免重复报告。
- Bug 请提供 R-Code 版本、操作系统与架构、安装来源、最小复现步骤、期望结果和实际结果。
- 日志必须先脱敏。删除 API key、Token、用户名、绝对私有路径、项目源码和对话内容。
- 功能建议应先说明用户问题，再说明期望方案；不必先提交完整技术设计。

仓库已提供对应的 GitHub Issue Forms。一般使用表单比空白 issue 更容易得到有效处理。

## 开发环境

需要 Git、Rust stable、Node.js 20，以及 [Tauri 2 对应平台依赖](https://v2.tauri.app/start/prerequisites/)。产品构建依赖 `vendor/agent-core` 私有子模块；`.agents` 子模块只用于仓库协作，不参与产品构建。

```bash
git clone git@github.com:foritin/r-code.git
cd r-code
git submodule update --init --recursive -- vendor/agent-core
```

`vendor/agent-core` 当前是私有构建依赖，需要相应仓库读权限。没有权限时仍可检出主仓并处理文档或不依赖该子模块的前端工作，但无法完成 Rust 全工作区构建。

Windows：

```powershell
./dev.ps1 -BootstrapOnly
./dev.ps1
```

macOS / Linux：

```bash
bash ./dev.sh --bootstrap-only
bash ./dev.sh
```

`.agents` 是可选的仓库协作技能；需要时单独初始化：

```bash
git submodule update --init -- .agents
```

## 分支与 Pull Request

1. 从最新 `dev` 创建短生命周期功能分支；`main` 是发布基线。
2. 一个 PR 只解决一个连贯问题，功能、测试和对应文档放在同一个 PR 中。
3. 提交信息使用 `feat:`、`fix:`、`docs:`、`test:`、`refactor:` 或 `chore:` 等清晰前缀。
4. PR 默认目标分支为 `dev`。发布准备或经维护者确认的紧急修复才直接面向 `main`。
5. 不提交本地记忆、Provider 密钥、MCP 环境变量、AppData 数据库、构建产物或机器专属配置。
6. 更新子模块时，同时说明上游仓库、目标提交和兼容性验证；不要只留下 `-dirty` gitlink。

## 本地验证

按变更范围运行测试；提交前至少保证受影响测试、格式检查和构建通过。

```bash
# 版本与发布元数据
node --test scripts/release.test.mjs scripts/release-quality-gate.test.mjs scripts/flaky-test-report.test.mjs
node scripts/release.mjs check

# Rust
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-features

# Frontend
cd src-tauri/frontend
npm ci
npm test
npm run build
```

依赖或 CI/Release 工作流改动还必须运行 `npm --prefix src-tauri/frontend audit --package-lock-only --audit-level=high` 和 `cargo deny check advisories`；后者需要按 CI 固定版本安装 `cargo-deny`。不要仅因为本机缺少工具而跳过相应的安全门。

平台相关改动还应在受影响平台实测：Windows 安装器与 WebView2、macOS 两种架构及签名路径、Linux AppImage/deb 等不能只依赖另一平台的结果。

## 文档与用户可见变化

- 架构、数据流、隐私边界或安全边界变化时，同步 `docs/ARCHITECTURE.md`、`PRIVACY.md` 或 `SECURITY.md`。
- 记忆、MCP、Provider、Codex 或发布行为变化时，同步对应专题文档。
- 用户可见变化写入 `CHANGELOG.md` 的 `Unreleased` 部分。
- 截图中不得出现密钥、私有路径、私有项目名或真实对话内容。

## 审核标准

维护者会重点检查：行为是否可复现、边界条件、跨平台影响、数据与权限边界、错误恢复、测试覆盖、性能退化，以及 UI 状态是否与真实后台状态一致。PR 模板中的项目并非形式要求，它们用于降低桌面 Agent 产品中最常见的回归风险。
