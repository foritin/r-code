# R-Code

[English](./README.md) | [简体中文](./README.zh-CN.md)

[![CI](https://github.com/foritin/r-code/actions/workflows/ci.yml/badge.svg?branch=main&event=push)](https://github.com/foritin/r-code/actions/workflows/ci.yml)
[![Flaky Test Report](https://github.com/foritin/r-code/actions/workflows/flaky-tests.yml/badge.svg?branch=main)](https://github.com/foritin/r-code/actions/workflows/flaky-tests.yml)
[![Release](https://img.shields.io/github/v/release/foritin/r-code?include_prereleases&sort=semver&label=release)](https://github.com/foritin/r-code/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-orange.svg)](./LICENSE)

Session-first AI coding desktop app built with Rust, Tauri 2, React, and TypeScript.

R-Code organizes conversations, model runs, tool approvals, file changes, verification, and replay into traceable tasks. The project is currently in the `0.x` stage. See [Releases](https://github.com/foritin/r-code/releases) and [CHANGELOG.md](./CHANGELOG.md) for shipped versions and user-visible changes.

## Highlights

- Native model providers with optional Codex CLI App Server/MCP collaboration, including same-task Codex → R-Code delegation.
- Plan mode with durable goals, structured human-in-the-loop questions, feature-oriented todos, and a crash-safe enhanced review workflow.
- Evolving memory that is off by default and stored only in AppData, with global approval, project review, and frozen snapshot injection.
- Keyless native web access, an optional built-in deep-research MCP, third-party MCP management, and official Registry discovery.
- Session branches, resend, steer, queues, a streaming timeline, and clickable file/line references that open the right-side Files workbench.
- R-Code/Codex subagent delegation with optional quality review, per-child cancellation, and visible read-only / approval-required / full-access states.
- Workspace file, search, Git, and Shell tools behind one audit boundary.
- Risk levels, per-call approval, read-only subagents, and path-escape protection.
- Baseline-aware diffs, verification, file/task rollback, and crash recovery.
- Integrated PTY terminal and Codex/Claude transcript replay.

### Codex collaboration

Codex main-agent runs use the official App Server connection. With Codex CLI `0.145.0` or newer and an available R-Code Provider, Codex can call a bounded in-session tool that creates a child run under the current R-Code task instead of opening another sidebar session. If either capability is unavailable, R-Code hides that dynamic tool and the Codex main run continues normally.

R-Code shows only public reasoning summaries emitted by the App Server, never raw chain-of-thought. Child runs inherit the parent permission ceiling, persist their effective three-state permission, and can be cancelled individually. Assistant references such as `src/main.rs:42` open the workspace file in the right-side workbench at the requested line.

## Supported platforms

| Platform | Release targets | Packages |
| --- | --- | --- |
| Windows | x86_64 MSVC | branded `.exe`, NSIS updater `.exe`, WiX `.msi` |
| macOS | Apple Silicon, Intel | per-architecture `.app`, `.dmg` |
| Linux | x86_64 GNU | `.AppImage`, `.deb` |

GitHub Actions builds installers from `v*` tags. See the [release guide](./docs/releasing.md) for code signing, first-release setup, and updater requirements.

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
| Shared contracts | `vendor/agent-contracts/` | required `agent-*` contract crates Git submodule |

JSONL is the conversation-content source, while SQLite is the product-state source for tasks, runs, permissions, audit, Plan, memory, and changes. See [Architecture](./docs/architecture.md) for the full model and diagrams.

## Development

Prerequisites: Git, stable Rust, Node.js 20, and the platform dependencies required by Tauri 2. Windows additionally needs Visual Studio Build Tools 2022 and WebView2 Runtime. See [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) for macOS and Linux.

```powershell
# Windows: verify the Tauri CLI, agent-contracts submodule, and npm dependencies, then start
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
cargo tauri dev --config src-tauri/tauri.dev.conf.json
```

The development process always uses the `R-Code Dev` identity with isolated
AppData, WebView, SQLite, logs, credentials, Codex/Claude configuration, and npm
global prefix, so it can run beside an installed `R-Code`. Even a bare
`cargo tauri dev` is protected by a runtime flavor guard and cannot open
production data. The development updater reads only the separate
`dev-latest.json` channel; source changes continue to hot reload through
Vite/Tauri.

Initialize only the required product submodule manually with:

```bash
git submodule update --init --recursive -- vendor/agent-contracts
```

The `.agents` submodule contains optional repository collaboration skills and is not part of the product build:

```bash
git submodule update --init -- .agents
```

## Verification

```bash
# Release metadata and release-quality gate
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

When changing dependencies or release automation, also run `npm --prefix src-tauri/frontend audit --package-lock-only --audit-level=high` and `cargo deny check advisories` (install the pinned `cargo-deny` version from CI if it is not already available).

Local packaging:

```bash
# Windows branded installer
powershell -ExecutionPolicy Bypass -File ./scripts/build-branded-installer.ps1

# Raw Windows NSIS / MSI
cargo tauri build --bundles nsis,msi

# macOS ad-hoc Apple Silicon app/dmg
bash ./scripts/manual/package-macos.sh

# Intel macOS package
bash ./scripts/manual/package-macos.sh --target x86_64-apple-darwin

# Signed/notarized macOS distribution
bash ./scripts/manual/package-macos.sh --signed

# Linux
cargo tauri build --bundles appimage,deb
```

See [RELEASING.md](./docs/releasing.md) for output paths, signing variables, and production requirements.

## Release

The version, changelog, tag, and GitHub Release flow is validated as one chain:

```bash
node scripts/release.mjs prepare X.Y.Z
# Review, verify, commit, push main, and wait for CI; then:
node scripts/publish-release.mjs vX.Y.Z --dry-run
node scripts/publish-release.mjs vX.Y.Z
```

The publish gate creates the immutable tag only after the exact `main` commit has a complete successful CI run; the release workflow verifies that provenance and every required CI job again before it can access release work. It then triggers the four-platform GitHub Actions build, waits for it, and verifies the uploaded assets. Stable releases sign each platform when its credentials are available; missing platform certificates produce an explicit unsigned warning instead of blocking the release, while updater integrity signing remains mandatory. Follow [the release guide](./docs/releasing.md) for credentials, repository controls, recovery, and post-release acceptance.

## Repository layout

```text
r-code/
├─ crates/                    # product-specific Rust crates
├─ installer/                 # Windows branded installer and NSIS payload wrapper
├─ src-tauri/                 # Tauri host and production React frontend
├─ vendor/agent-contracts/         # required shared contract submodule
├─ docs/                      # current documentation and UI reference images
├─ icons/                     # package icons and maintainable source assets
├─ scripts/                   # development, signing, and release helpers
├─ .github/workflows/         # CI and release workflows
├─ CHANGELOG.md               # user-visible release history
└─ Cargo.toml                 # Rust workspace and product version baseline
```

## Documentation

- [Documentation index](./docs/readme.md)
- [Contributing](./CONTRIBUTING.md)
- [Support](./SUPPORT.md)
- [Code of Conduct](./CODE_OF_CONDUCT.md)
- [Architecture](./docs/architecture.md)
- [Plan mode and enhanced review](./docs/plan-mode.en.md)
- [Web tools and MCP](./docs/mcp.md)
- [Evolving memory](./docs/memory.md)
- [Installation, backup, recovery, and uninstall](./docs/operations.en.md)
- [Release guide](./docs/releasing.md)
- [Security Policy](./SECURITY.md)
- [Privacy Notice](./PRIVACY.md)
- [CHANGELOG](./CHANGELOG.md)

Do not submit vulnerability details, credentials, or private source code in public issues. Follow [SECURITY.md](./SECURITY.md) for private reporting and [PRIVACY.md](./PRIVACY.md) for local and network data flows.

## License

[MIT](./LICENSE) © R-Code Team
