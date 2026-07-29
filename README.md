# R-Code

Session-first desktop application for AI-assisted coding, built with Rust + Tauri 2.

## 架构

三层进程模型（基于 [agent-core](vendor/agent-core) 公共合同）：

| 层 | crate | 职责 |
| --- | --- | --- |
| **Host (Main)** | `r-code-host` | Tauri 应用壳、IPC Server、进程编排、SQLite |
| **Worker** | `r-code-agent-worker` | Agent 运行时、Tool Gateway、状态机 |
| **Renderer** | `src-tauri/frontend/` | React + TypeScript（Vite 构建） |

公共合同层在 `vendor/agent-core/`（git 子模块），提供 `hermes-*` 系列 crate。

## 开发

```powershell
# 前置：Rust (msvc target)、VS Build Tools 2022、WebView2 Runtime

# 安装 Tauri 2 CLI（首次）
cargo install tauri-cli --version "^2.0.0" --locked

# 拉取子模块
git submodule update --init --recursive

# 安装前端依赖（首次）
cd src-tauri/frontend && npm install && cd ../..

# Windows：自动检查并补齐 Tauri CLI、子模块和 npm 依赖
./dev.ps1

# Windows：仅补齐并验证依赖，不启动应用
./dev.ps1 -BootstrapOnly

# macOS：自动检查 Xcode Command Line Tools 及项目依赖，然后启动
bash ./dev.sh

# macOS：仅补齐并验证依赖，不启动应用
bash ./dev.sh --bootstrap-only

# 初始化完成后，上述启动命令等价于：cargo tauri dev

# 仅编译主进程
cargo build -p r-code-host
```

开发服务器固定使用 `127.0.0.1:5173`。如果 Windows 启动时持续显示
`Waiting for your frontend dev server`，请检查 VPN/TUN 是否拦截本地回环。启动脚本会
在运行前验证该端口并给出明确错误，避免留下失去响应的 Vite 进程。

## 打包

```powershell
# Windows（生成 NSIS .exe + MSI .msi）
cargo tauri build --bundles nsis,msi

# macOS（须在 Mac 上执行）
cargo tauri build --bundles app,dmg
```

产物在 `target/release/bundle/`。

## 项目结构

```
r-code/
├─ crates/
│  ├─ r-code-host/          # Tauri 应用壳 + IPC + 进程编排
│  │  ├─ tauri.conf.json    # Tauri 配置（打包、窗口、签名）
│  │  ├─ build.rs           # tauri-build
│  │  ├─ frontend/          # React + TypeScript 前端（Vite）
│  │  └─ src/               # 主进程源码
│  ├─ r-code-core/          # 产品私有 DTO
│  ├─ r-code-store/         # SQLite 持久化
│  ├─ r-code-gateway/       # Tool Gateway + 权限引擎
│  ├─ r-code-terminal/      # PTY 终端系统
│  └─ r-code-agent-worker/  # Agent 运行时
├─ vendor/agent-core/       # 公共合同子模块（hermes-*）
├─ icons/                   # 应用图标（打包用 + 源素材）
│  ├─ *.png / *.ico / *.icns
│  └─ source/               # 原始设计素材与图层
├─ docs/                    # 离线 HTML 重构文档
└─ Cargo.toml               # workspace 根
```

## 文档

完整架构、产品合同、实施路线图见 [`docs/index.html`](docs/index.html)（离线 HTML 文档集）。

## License

MIT © R-Code Team
