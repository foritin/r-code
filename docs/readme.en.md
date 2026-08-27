# R-Code Documentation

This directory holds the current documentation that can be previewed directly on GitHub. When the documented behavior conflicts with the code, the currently tested code takes precedence, and the documentation must be fixed in the same change.

## Maintainer Entry Points

| Document | Purpose |
| --- | --- |
| [Architecture and implementation details](./architecture.md) | Runtime boundaries, crate layering, agent loop, storage, security, terminal, frontend, and extension paths (Chinese) |
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

## Active Implementation Contract

| Document | Status |
| --- | --- |
| [Product-experience redesign PRD and AI worklist](./product-experience-redesign/r-code-experience-redesign-prd.md) | Frozen; progress is `1/42`. This delivery completes the prototype and execution contract, not the product implementation |
| [Historical Codex rich-interaction contract](./support/contracts/codex-rich-interaction-prd.md) | Its `38/38` evidence applies to a specific 2026-08-25 revision; the current dirty `dev` must be revalidated by M0-02 |
| [Historical Windows command-reliability contract](./support/contracts/windows-command-reliability-prd.md) | Frozen contract for its completed revision; retained for maintenance and traceability, not as a new todo source |

## UI Reference Images

- [Current product-experience prototype](./product-experience-redesign/): clickable HTML, key-state images, design notes, and the active worklist.
- [`support/ui-reference/legacy/light/`](./support/ui-reference/legacy/light/): historical light-mode UI references.
- [`support/ui-reference/legacy/dark/`](./support/ui-reference/legacy/dark/): historical dark-mode UI references.

Legacy images are comparison material, not proof of the current implementation. The executable prototype and capture script live only in the current product-experience directory.

## Historical Archive

[`support/archive/`](./support/archive/) stores implemented one-off plans, experiment baselines, historical prototypes, and phase-specific decision records. Archived documents are no longer the current implementation or todo list; current behavior is determined by the maintenance documents listed here and tested code. See [`support/README.md`](./support/README.md) and [`support/archive/readme.md`](./support/archive/readme.md) for the migration map and archive reasons.
