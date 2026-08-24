# R-Code Documentation

This directory holds the current documentation that can be previewed directly on GitHub. When the documented behavior conflicts with the code, the currently tested code takes precedence, and the documentation must be fixed in the same change.

## Maintainer Entry Points

| Document | Purpose |
| --- | --- |
| [Architecture and implementation details](./architecture.md) | Runtime boundaries, crate layering, agent loop, storage, security, terminal, frontend, and extension paths (Chinese) |
| [Web tools and MCP](./mcp.md) | Native web access, MCP management, Registry, security confirmation, cross-platform startup, and failure recovery (Chinese) |
| [Evolution memory](./memory.md) | Global/project scope, automatic triggers, Reviewer, approval, injection, persistence, and privacy boundaries (Chinese) |
| [Plan mode and enhanced review](./plan-mode.en.md) | Goals, structured human confirmation, Plan projection, feature todos, enhanced review, concurrency, and crash recovery |
| [macOS real-device validation checklist](./macos-validation.md) | Validation that Windows/Linux cannot substitute for: local encrypted credentials, Finder, terminal, RTK, MCP, and installer runtime (Chinese) |
| [Release handbook](./releasing.md) | Versioning, CHANGELOG, tags, GitHub Release, signing, failure recovery, and the first-release checklist (Chinese) |
| [Installation, backup, restore, and uninstall](./operations.en.md) | Install, upgrade, full data backup, migration restore, uninstall, and support-bundle flows for users and operators |
| [CHANGELOG](../CHANGELOG.md) | User-visible changes and release history for each version |
| [Security Policy](../SECURITY.md) | Supported scope, private vulnerability reporting, and security boundaries |
| [Privacy Notice](../PRIVACY.md) | Data flows for local storage, model providers, Codex, updates, and support bundles |
| [English README](../README.md) / [简体中文 README](../README.zh-CN.md) | Product overview, quick development, validation commands, and repository navigation |

## Active Implementation Contract

| Document | Status |
| --- | --- |
| [Codex rich-interaction PRD and AI implementation checklist](./codex-rich-interaction-prd.md) | Frozen execution contract in Chinese; product tasks remain incomplete and §9 is the only status source |

## UI Reference Images

- [`ui/light/`](./ui/light/): light-mode UI reference images.
- [`ui/dark/`](./ui/dark/): dark-mode UI reference images.

`ui/` only holds static reference images; it contains no executable demos, generation scripts, or implementation contracts.

## Historical Archive

[`archive/`](./archive/) stores implemented one-off plans, experiment baselines, historical prototypes, and phase-specific decision records. Archived documents are no longer the current implementation or todo list; current behavior is determined by the maintenance documents listed here and the tested code. See [`archive/readme.md`](./archive/readme.md) for the inventory and archive reasons.
