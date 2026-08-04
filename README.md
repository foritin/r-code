# R-Code

[English](./README.md) | [简体中文](./README.zh-CN.md)

[![CI](https://github.com/foritin/r-code/actions/workflows/ci.yml/badge.svg?branch=main&event=push)](https://github.com/foritin/r-code/actions/workflows/ci.yml)
[![Flaky Test Report](https://github.com/foritin/r-code/actions/workflows/flaky-tests.yml/badge.svg?branch=main)](https://github.com/foritin/r-code/actions/workflows/flaky-tests.yml)
[![Release](https://img.shields.io/github/v/release/foritin/r-code?include_prereleases&sort=semver&label=release)](https://github.com/foritin/r-code/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-orange.svg)](./LICENSE)

Session-first AI coding desktop app built with Rust, Tauri 2, React, and TypeScript.

R-Code organizes conversations, model runs, tool approvals, file changes, verification, and replay into traceable tasks. The project is currently in the `0.x` stage. See [Releases](https://github.com/foritin/r-code/releases) and [CHANGELOG.md](./CHANGELOG.md) for shipped versions and user-visible changes.

## Highlights

- Native model providers with optional Codex CLI/MCP collaboration.
- Plan mode with durable goals, structured human-in-the-loop questions, feature-oriented todos, and a crash-safe enhanced review workflow.
- Evolving memory that is off by default and stored only in AppData, with global approval, project review, and frozen snapshot injection.
- Keyless native web access, an optional built-in deep-research MCP, third-party MCP management, and official Registry discovery.
- Session branches, resend, steer, queues, and a streaming timeline.
- R-Code/Codex subagent delegation with optional quality review.
- Workspace file, search, Git, and Shell tools behind one audit boundary.
- Risk levels, per-call approval, read-only subagents, and path-escape protection.
- Baseline-aware diffs, verification, file/task rollback, and crash recovery.
- Integrated PTY terminal and Codex/Claude transcript replay.

## Supported platforms

| Platform | Release targets | Packages |
| --- | --- | --- |
| Windows | x86_64 MSVC | branded `.exe`, NSIS updater `.exe`, WiX `.msi` |
| macOS | Apple Silicon, Intel | per-architecture `.app`, `.dmg` |
| Linux | x86_64 GNU | `.AppImage`, `.deb` |

GitHub Actions builds installers from `v*` tags. See the [release guide](./docs/RELEASING.md) for code signing, first-release setup, and updater requirements.

## Architecture

The normal desktop application is not three permanently separate processes. The Tauri host, native agent runtime, Tool Gateway, and storage services are logical layers in one Rust process, while React runs in the WebView. Optional integrations such as Codex CLI, the Codex-facing MCP server, and enabled local stdio MCP servers create additional processes.

| Layer | Location | Responsibility |
| --- | --- | --- |
| Desktop host | `src-tauri/` | Tauri shell, IPC, run orchestration, providers, Codex, and system services |
| Agent runtime | `crates/r-code-agent-worker/` | multi-turn loop, steer, subagents, quality review, Plan suspension |
| Web / MCP client | `crates/r-code-mcp/` | safe web access, MCP clients, Registry, lazy sessions, lifecycle |
| Tools / security | `crates/r-code-gateway/`, `r-code-core/` | tool execution, path boundaries, risk, permissions, DTOs, secrets |
| Persistence | `crates/r-code-store/` | SQLite, JSONL projections, blobs, changes, Plan, review, verification |
| Terminal | `crates/r-code-terminal/` | PTY, OSC 133, raw output, external CLI replay |
| Renderer | `src-tauri/frontend/` | React scenes, Zustand state, typed Tauri IPC |
| Shared contracts | `vendor/agent-core/` | required `hermes-*` contract crates Git submodule |

JSONL is the conversation-content source, while SQLite is the product-state source for tasks, runs, permissions, audit, Plan, memory, and changes. See [Architecture](./docs/ARCHITECTURE.md) for the full model and diagrams.

## Development

Prerequisites: Git, stable Rust, Node.js 20, and the platform dependencies required by Tauri 2. Windows additionally needs Visual Studio Build Tools 2022 and WebView2 Runtime. See [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) for macOS and Linux.

```powershell
# Windows: verify the Tauri CLI, agent-core submodule, and npm dependencies, then start
./dev.ps1

# Bootstrap only
./dev.ps1 -BootstrapOnly
```

```bash
# macOS / Linux
bash ./dev.sh

# Bootstrap only
bash ./dev.sh --bootstrap-only
```

After bootstrap, the development launch is equivalent to:

```bash
cargo tauri dev
```

Initialize only the required product submodule manually with:

```bash
git submodule update --init --recursive -- vendor/agent-core
```

The `.agents` submodule contains optional repository collaboration skills and is not part of the product build:

```bash
git submodule update --init -- .agents
```

## Verification

```bash
# Release metadata
node --test scripts/release.test.mjs
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

Local packaging:

```bash
# Windows branded installer
powershell -ExecutionPolicy Bypass -File ./scripts/build-branded-installer.ps1

# Raw Windows NSIS / MSI
cargo tauri build --bundles nsis,msi

# macOS ad-hoc Apple Silicon app/dmg
bash ./scripts/build-macos.sh

# Intel macOS package
bash ./scripts/build-macos.sh --target x86_64-apple-darwin

# Signed/notarized macOS distribution
bash ./scripts/build-macos.sh --signed

# Linux
cargo tauri build --bundles appimage,deb
```

See [RELEASING.md](./docs/RELEASING.md) for output paths, signing variables, and production requirements.

## Release

The version, changelog, tag, and GitHub Release flow is validated as one chain:

```bash
node scripts/release.mjs prepare 0.1.0
# Review, verify, commit, push main, and wait for CI; then:
node scripts/publish-release.mjs v0.1.0 --dry-run
node scripts/publish-release.mjs v0.1.0
```

The publish gate creates the immutable tag, triggers the four-platform GitHub Actions build, waits for it, and verifies the uploaded assets. Stable releases sign each platform when its credentials are available; missing platform certificates produce an explicit unsigned warning instead of blocking the release, while updater integrity signing remains mandatory. Follow [the release guide](./docs/RELEASING.md) for credentials, recovery, and post-release acceptance.

## Repository layout

```text
r-code/
├─ crates/                    # product-specific Rust crates
├─ installer/                 # Windows branded installer and NSIS payload wrapper
├─ src-tauri/                 # Tauri host and production React frontend
├─ vendor/agent-core/         # required shared contract submodule
├─ docs/                      # current documentation and UI reference images
├─ icons/                     # package icons and maintainable source assets
├─ scripts/                   # development, signing, and release helpers
├─ .github/workflows/         # CI and release workflows
├─ CHANGELOG.md               # user-visible release history
└─ Cargo.toml                 # Rust workspace and product version baseline
```

## Documentation

- [Documentation index](./docs/README.md)
- [Contributing](./CONTRIBUTING.md)
- [Support](./SUPPORT.md)
- [Code of Conduct](./CODE_OF_CONDUCT.md)
- [Architecture](./docs/ARCHITECTURE.md)
- [Plan mode and enhanced review](./docs/plan-mode.en.md)
- [Web tools and MCP](./docs/mcp.md)
- [Evolving memory](./docs/memory.md)
- [Release guide](./docs/RELEASING.md)
- [Security Policy](./SECURITY.md)
- [Privacy Notice](./PRIVACY.md)
- [CHANGELOG](./CHANGELOG.md)

Do not submit vulnerability details, credentials, or private source code in public issues. Follow [SECURITY.md](./SECURITY.md) for private reporting and [PRIVACY.md](./PRIVACY.md) for local and network data flows.

## License

[MIT](./LICENSE) © R-Code Team
