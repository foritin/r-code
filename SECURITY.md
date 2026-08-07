# Security Policy

R-Code can read source code, call external model providers and execute approved local tools. Please report security issues privately so maintainers can investigate before details become public.

## Supported versions

During the `0.x` phase, only the latest published GitHub Release receives security fixes. Older builds may be asked to update before a report can be reproduced.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting for this repository:

<https://github.com/foritin/r-code/security/advisories/new>

If that entry is unavailable, open a public issue asking for a private maintainer contact, but do **not** include exploit details, secrets, private source code or user data in the issue.

Include only what is necessary to reproduce and assess the problem:

- affected R-Code version, operating system and architecture;
- attack preconditions and expected security boundary;
- minimal reproduction steps or proof of concept;
- impact and whether the issue has been exploited;
- suggested mitigation, if known.

We will acknowledge receipt, establish a private coordination channel, assess severity and plan disclosure. Please allow a reasonable remediation window before publishing details.

## Security boundaries worth reporting

- escaping the attached workspace through paths, symlinks, non-existing ancestors, or a path replacement between validation and file I/O;
- executing an R4 action, or bypassing the selected approval mode;
- a read-only subagent modifying files or running commands;
- a delegated subagent exceeding its requested access ceiling (e.g. `read_only` executing mutating MCP tools under a full-access workspace);
- an `AllowAlways` decision authorising a different target or a higher-risk invocation than the one the user approved;
- a third-party MCP `readOnlyHint` bypassing generic MCP approval, or an App Server permission profile escaping the physical workspace;
- leaking Provider credentials through config, logs, support bundles or UI;
- leaking MCP environment/header credentials, forwarding them across origins, or changing a reviewed launch plan without renewed confirmation;
- installing an updater artifact whose signature does not match the embedded public key;
- cross-task/session data disclosure or unauthorised Codex/MCP control;
- unsafe parsing of attachments, terminal output, JSONL sessions or IPC payloads.

Model mistakes or a command that the user knowingly approved are not automatically vulnerabilities. A policy bypass, misleading approval summary, boundary escape or secret exposure is.

## Handling sensitive data

Provider and MCP secrets are intended to live in the operating-system credential store. MCP Registry entries are unreviewed third-party metadata: adding one does not start it, and first enable requires a second exact launch-plan confirmation. Windows script and shell launchers are rejected for MCP stdio; remote MCP endpoints require HTTPS. Generic third-party `mcp_call` is always treated as R2 regardless of the server's `readOnlyHint`; native `web_search` and `web_fetch` retain their separately audited read classification.

Tool access remains constrained to an attached workspace where applicable, R4 actions are denied, and persistent allow decisions are scoped to the exact task/tool, any target supplied by that call, and the risk level the user approved. Existing workspace file operations use a directory capability fixed at operation start instead of reopening a checked ambient path; App Server approval requests always use an exact request fingerprint, and file permission profiles are resolved against the physical workspace and reject traversal or symlink escape. Delegated subagents are capped by the parent run's access mode: read-only parents produce read-only subagents, approval-mode parents produce subagents whose writes and commands require approval, and only full-access parents can delegate full access. These controls reduce risk but do not make `full_access` or an enabled local MCP equivalent to a sandbox: review workspace access, MCP publishers and tool approvals as carefully as local shell access.

Never attach raw application data directories, credentials or proprietary repositories to a public issue. Generate a support-bundle preview first and inspect every file before sharing it privately.
