# R-Code Documentation

The top level of `docs/` contains navigation, active plans in `prd/`, and supporting material in `support/`. A published plan does not mean its product changes have shipped. When documentation and behavior differ, tested code remains the implementation reference.

## Maintainer Entry Points

| Document | Purpose |
| --- | --- |
| [R-Code v1 unified architecture and implementation PRD](./support/archive/prd/r-code-v1/index.md) | Archived: unified execution entry, evidence-backed progress, provider/planning/execution/safety boundaries, and recovery protocol (Chinese) |
| [Harness v1 reference](./support/harness/architecture.md) | Implemented plugin architecture, [protocol](./support/harness/protocol-v1.md), and [author guide](./support/harness/plugin-author-guide.md); the original PRD is archived |
| [Pre-refactor architecture baseline](./support/archive/architecture-before-pluggable-harness.md) | Archived implementation reference for migration and code investigation (Chinese) |
| [Web tools and MCP](./support/guides/mcp.md) | Native web access, MCP management, Registry, security confirmation, cross-platform startup, and failure recovery (Chinese) |
| [Evolution memory](./support/guides/memory.md) | Global/project scope, automatic triggers, Reviewer, approval, injection, persistence, and privacy boundaries (Chinese) |
| [Plan mode and enhanced review](./support/guides/plan-mode.en.md) | Goals, structured human confirmation, Plan projection, feature todos, enhanced review, concurrency, and crash recovery |
| [macOS real-device validation checklist](./support/platform/macos-validation.md) | Validation that Windows/Linux cannot substitute for: local encrypted credentials, Finder, terminal, RTK, MCP, and installer runtime (Chinese) |
| [Release handbook](./support/operations/releasing.md) | Versioning, CHANGELOG, tags, GitHub Release, signing, failure recovery, and the first-release checklist (Chinese) |
| [Installation, backup, restore, and uninstall](./support/operations/operations.en.md) | Install, upgrade, full data backup, migration restore, uninstall, and support-bundle flows for users and operators |
| [Support-material index](./support/README.md) | Unified guide to guides, operations, platform checks, historical contracts, legacy UI, and archives |
| [CHANGELOG](../CHANGELOG.md) | User-visible changes and release history for each version |
| [Security Policy](../SECURITY.md) | Supported scope, private vulnerability reporting, and security boundaries |
| [Privacy Notice](../PRIVACY.md) | Data flows for local storage, model providers, Codex, updates, and support bundles |
| [English README](../README.md) / [简体中文 README](../README.zh-CN.md) | Product overview, quick development, validation commands, and repository navigation |

## Historical Implementation Contracts

There is no active implementation contract right now; new plans start in the [`prd/`](./prd/README.md) workspace.

| Document | Status |
| --- | --- |
| [R-Code v1 unified implementation](./support/archive/prd/r-code-v1/index.md) | Archived: P-GATE 9/9, M-GATE 4/4, Safety 5/39 kept. Historical Harness T00–T42 evidence is preserved in the [archive](./support/archive/pluggable-harness-prd/progress.md). |
| [Pi-alignment + TUI plan](./support/archive/pi-alignment/pi-alignment-and-tui-prd.md) | Archived; original status belongs to its historical revision, not the active worklist |
| [TUI v1 / R-Code CLI plan](./support/archive/tui-v1/r-code-cli-prd.md) | Archived with its research, prototype, and freeze; normative digests and historical evidence are preserved |
| [Historical Codex rich-interaction contract](./support/contracts/codex-rich-interaction-prd.md) | Its `38/38` evidence applies to a specific 2026-08-25 revision; the current dirty `dev` must be revalidated by M0-02 |
| [Historical Windows command-reliability contract](./support/contracts/windows-command-reliability-prd.md) | Frozen contract for its completed revision; retained for maintenance and traceability, not as a new todo source |

## UI Reference Images

- [Product-experience redesign prototype (archived)](./support/archive/product-experience-redesign/): clickable HTML, key-state images, design notes (frozen, 42/42 closed).
- [`support/ui-reference/legacy/light/`](./support/ui-reference/legacy/light/): historical light-mode UI references.
- [`support/ui-reference/legacy/dark/`](./support/ui-reference/legacy/dark/): historical dark-mode UI references.

Legacy images are comparison material, not proof of the current implementation. The executable prototype and capture script live under the archived `product-experience-redesign` directory.

## Historical Archive

[`support/archive/`](./support/archive/) stores implemented one-off plans, experiment baselines, historical prototypes, and phase-specific decision records. Archived documents are no longer the current implementation or todo list; current behavior is determined by the maintenance documents listed here and tested code. See [`support/README.md`](./support/README.md) and [`support/archive/readme.md`](./support/archive/readme.md) for the migration map and archive reasons.
