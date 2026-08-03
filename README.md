# R-Code

[![CI](https://github.com/foritin/r-code/actions/workflows/ci.yml/badge.svg?branch=main&event=push)](https://github.com/foritin/r-code/actions/workflows/ci.yml)
[![Flaky Test Report](https://github.com/foritin/r-code/actions/workflows/flaky-tests.yml/badge.svg?branch=main)](https://github.com/foritin/r-code/actions/workflows/flaky-tests.yml)
[![Release](https://img.shields.io/github/v/release/foritin/r-code?include_prereleases&sort=semver&label=release)](https://github.com/foritin/r-code/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-orange.svg)](./LICENSE)

Session-first AI coding desktop app, built with Rust, Tauri 2, React and TypeScript.

R-Code 把对话、模型执行、工具审批、文件变更、验证和回放组织成可追溯的任务。项目当前处于 `0.x` 阶段；发布版本和用户可见变化见 [Releases](https://github.com/foritin/r-code/releases) 与 [CHANGELOG.md](./CHANGELOG.md)。

## 能力概览

- 原生模型 Provider 与可选 Codex CLI/MCP 协作；
- 默认关闭、仅存 AppData 的演进记忆；支持全局审批、项目自动复盘和冻结快照注入；
- 无密钥原生联网、可关闭的内置深度调研 MCP、第三方 MCP 管理与官方 Registry 市场；
- 会话分支、重发、Steer、消息队列和流式时间线；
- R-Code/Codex 子智能体委派与可选质量复核；
- 工作区内文件、搜索、Git、Shell 工具及统一审计；
- 风险分级、逐次审批、只读子智能体和路径逃逸防护；
- 变更基线、diff、验证、按文件/任务回滚与崩溃恢复；
- PTY 集成终端和 Codex/Claude transcript 回放。

## 支持平台

| 平台 | 当前发布目标 | 安装包 |
| --- | --- | --- |
| Windows | x86_64 MSVC | 品牌安装器 `.exe`、NSIS updater `.exe`、WiX `.msi` |
| macOS | Apple Silicon、Intel | 各架构 `.app`、`.dmg` |
| Linux | x86_64 GNU | `.AppImage`、`.deb` |

安装包由 `v*` tag 的 GitHub Actions 构建。平台代码签名、首次发布和自动更新要求见 [发布手册](./docs/RELEASING.md)。

## 架构

正常桌面模式不是三个固定独立进程：Tauri Host、原生 Agent runtime、Tool Gateway 和存储服务位于同一 Rust 进程的逻辑层，React 运行在 WebView；Codex CLI、面向 Codex 的 MCP server 和启用后的本地 stdio MCP 等可选集成会额外创建进程。

| 层 | 位置 | 职责 |
| --- | --- | --- |
| Desktop Host | `src-tauri/` | Tauri 壳、IPC、Run 编排、Provider/Codex 集成与系统服务 |
| Agent runtime | `crates/r-code-agent-worker/` | 多轮模型循环、Steer、子智能体与质量复核 |
| Web / MCP client | `crates/r-code-mcp/` | 安全网页访问、MCP 客户端、Registry、惰性会话与生命周期 |
| Tool/security | `crates/r-code-gateway/`、`r-code-core/` | 工具执行、路径边界、风险、权限、DTO 与密钥 |
| Persistence | `crates/r-code-store/` | SQLite、JSONL 投影、Blob、变更、审核与验证 |
| Terminal | `crates/r-code-terminal/` | PTY、OSC 133、原始输出与外部 CLI 回放 |
| Renderer | `src-tauri/frontend/` | React 场景、Zustand 状态与 typed Tauri IPC |
| Shared contracts | `vendor/agent-core/` | `hermes-*` 公共合同 crates；构建必需 Git 子模块 |

数据采用双存储：JSONL 是会话内容源，SQLite 是任务、Run、权限、审计和变更等产品状态源。完整说明和 Mermaid 时序图见 [架构与实现细节](./docs/ARCHITECTURE.md)。

## 开发

前置环境：Git、Rust stable、Node.js 20、Tauri 2 的平台系统依赖。Windows 还需要 Visual Studio Build Tools 2022 与 WebView2 Runtime；macOS/Linux 依赖见 [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/)。

```powershell
# Windows：检查 Tauri CLI、agent-core 子模块和 npm 依赖，然后启动
./dev.ps1

# 只安装并验证依赖
./dev.ps1 -BootstrapOnly
```

```bash
# macOS / Linux
bash ./dev.sh

# 只安装并验证依赖
bash ./dev.sh --bootstrap-only
```

初始化完成后，启动阶段等价于：

```bash
cargo tauri dev
```

只手动初始化产品构建所需子模块时：

```bash
git submodule update --init --recursive -- vendor/agent-core
```

`.agents` 是可选的仓库协作技能，不参与产品构建：

```bash
git submodule update --init -- .agents
```

## 验证

```bash
# 版本元数据
node --test scripts/release.test.mjs
node scripts/release.mjs check

# Rust
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-features

# 前端
cd src-tauri/frontend
npm ci
npm run test:dev-server
npm run test:popover
npm run test:mcp
npm run build
```

本地打包：

```bash
# Windows：构建带品牌界面的最终安装器（内部复用 NSIS）
./scripts/build-branded-installer.ps1

# 如需单独构建原始 NSIS / MSI
cargo tauri build --bundles nsis,msi

# macOS：默认生成仅供本机测试的 ad-hoc 签名 Apple Silicon app/dmg
bash ./scripts/build-macos.sh

# Intel Mac 本地包（GitHub Release 同时提供此架构）
bash ./scripts/build-macos.sh --target x86_64-apple-darwin

# macOS 正式分发：使用 Keychain 中的 Developer ID，并完成 notarization/stapling
bash ./scripts/build-macos.sh --signed

# Linux
cargo tauri build --bundles appimage,deb
```

Windows 最终安装器位于 `target/release/bundle/branded/`。macOS 脚本默认输出到 `target/aarch64-apple-darwin/release/bundle/`；其他产物位于 `target/release/bundle/`，指定 `--target` 时位于 `target/<triple>/release/bundle/`。`--signed` 所需 Apple 环境变量见发布手册。

## 发布

版本准备、CHANGELOG、tag 和 GitHub Release 已形成一条可校验链路：

```bash
# 同步版本并把 Unreleased 盖章为正式版本
node scripts/release.mjs prepare 0.1.0

# 提交后创建 tag；push tag 会启动跨平台 Draft Release 流程
git tag -a v0.1.0 -m "R-Code v0.1.0"
git push origin v0.1.0
```

不要只照抄这两条命令直接上线。首次发布的 Secrets、操作系统签名、完整验证、失败恢复和发布后验收见 [docs/RELEASING.md](./docs/RELEASING.md)。

## 项目结构

```text
r-code/
├─ crates/                    # 产品私有 Rust crates
├─ installer/                 # Windows 品牌安装器与 NSIS 载荷封装
├─ src-tauri/                 # Tauri Host 与正式 React 前端
├─ vendor/agent-core/         # 公共合同子模块
├─ docs/                      # 当前 Markdown 文档与 UI 参考图
├─ icons/                     # 打包图标和可维护源素材
├─ scripts/                   # 开发、签名与发布辅助脚本
├─ .github/workflows/         # CI 与 Release 工作流
├─ CHANGELOG.md               # 用户可见版本历史
└─ Cargo.toml                 # Rust workspace 与产品版本基线
```

## 文档

- [文档索引](./docs/README.md)
- [贡献指南](./CONTRIBUTING.md)
- [支持与问题反馈](./SUPPORT.md)
- [Code of Conduct](./CODE_OF_CONDUCT.md)
- [架构与实现细节](./docs/ARCHITECTURE.md)
- [联网工具与 MCP](./docs/mcp.md)
- [演进记忆](./docs/memory.md)
- [发布手册](./docs/RELEASING.md)
- [Security Policy](./SECURITY.md)
- [Privacy Notice](./PRIVACY.md)
- [CHANGELOG](./CHANGELOG.md)

## Security

请不要在公开 issue 中提交漏洞细节、密钥或私有源码。私密报告流程见 [SECURITY.md](./SECURITY.md)。

本地数据、模型 Provider、Codex 和 updater 的数据流见 [PRIVACY.md](./PRIVACY.md)。

## License

[MIT](./LICENSE) © R-Code Team
