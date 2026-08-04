# R-Code Privacy Notice

Last updated: 2026-08-04

This notice describes the data flows implemented by the open-source R-Code desktop application. It is a technical baseline for a public release, not a substitute for legal review or for the privacy terms of a distributor, organisation, model provider or hosted service.

## Data stored on your device

R-Code stores application data under the operating system's application-data location. Depending on the features you use, this can include:

- tasks, workspace references, run state, approvals, tool-call metadata and notifications in SQLite;
- conversation and Agent events in JSONL session files;
- file baselines, verification output and other large content in content-addressed Blob storage;
- non-secret Provider configuration, application logs and recovery metadata;
- non-secret MCP server configuration and cached MCP Registry search results;
- when evolving memory is enabled: its settings, approved global/project entries, sanitized short-lived review turns, review jobs/candidates and injection references in SQLite.
- Plan-mode goals, revisions, structured questions/answers, feature todos, feature-change ownership and enhanced-review decisions in SQLite, plus human-readable Plan projections and recovery blobs under application data.

R-Code reads and modifies workspace files only when a workspace is attached and the selected access/approval policy allows the operation. Forgetting a workspace removes the application's workspace reference; it does not delete the source directory itself.

Evolving memory is off by default. R-Code stores its content only in the application-data SQLite database and does not create `.r-code/memory.md`, edit `.gitignore`, or add memory data to Git. Global suggestions require explicit user approval; validated project suggestions can apply automatically only to their source workspace. Project memory can also be set to read-only or off.

Plan documents follow the same local-data boundary: the durable state is stored in SQLite and the Markdown projection is written to `<AppData>/r-code/plans/<plan-id>/plan.md`, never into the attached project. Enhanced review may store before/after and rollback snapshots in the application Blob store so a feature-scoped rejection can preserve unrelated edits and recover safely after interruption. These files are not added to the project's Git index by R-Code. Permanently deleting a task or forgetting a workspace releases its Plan-owned Blob references and removes only canonical UUID Plan projection directories under application data; R-Code never follows a stored projection path into the workspace or another location.

Local data remains on the device until it is removed through the application, by the user, or by operating-system cleanup. Uninstall behaviour varies by platform and installer, so do not assume uninstalling also erases the application-data directory.

## Data sent to model providers

When you use an LLM Provider, R-Code sends data needed to answer the request to the Provider endpoint you selected. This may include:

- system instructions and conversation messages;
- source snippets, file content or search results selected by you or read by approved tools;
- attachment content and metadata;
- tool definitions, tool results and error context;
- model, reasoning and inference settings.
- in Plan mode, the current task goal, Plan revision/state, pending structured questions and active feature context required to continue the workflow.

The Provider processes and retains that data under its own terms and privacy policy. Before attaching confidential code or personal data, review the endpoint, account and data-retention settings shown in R-Code and the Provider's current policy. A custom Provider URL sends data to the operator of that URL.

Codex CLI collaboration uses the Codex installation and account configured on the device. Its network traffic, authentication and service-side retention are governed by the applicable Codex/OpenAI configuration and terms, not by R-Code's local storage policy.

General MCP services are separate third-party integrations. A remote MCP receives the tool arguments sent to it; a local stdio MCP runs as a separate process with the operating-system access granted to that program. Review the service, publisher, requested credentials and data policy before enabling it.

If evolving memory is enabled, R-Code sends sanitized visible user/assistant turn text and bounded existing-memory context to the Reviewer Provider selected in the memory settings. It does not include attachment bodies, tool arguments/output, hidden reasoning or complete subagent transcripts in that review envelope. Sanitization is a risk reduction, not a guarantee that all sensitive natural-language content has been removed; select a Provider whose privacy and retention terms are suitable for the conversation.

## Other network requests

R-Code may make network requests to:

- discover models from the configured Provider;
- execute a model request or an explicitly approved network-capable tool;
- search or fetch public web content through the configured native web provider;
- query the official MCP Registry when you open or search the MCP market;
- call an enabled remote MCP service, or let an enabled local MCP process make its own requests;
- install, authenticate or coordinate with an external CLI when you request that action;
- query GitHub Releases for `latest.json` and download a signed application update.

The current application code does not include first-party analytics, advertising SDKs or automatic product-usage telemetry. Local logs and support bundles are not uploaded automatically.

## Credentials

Provider API keys and MCP environment/header credentials are intended to be stored in the operating-system credential store. Non-secret Provider settings, MCP launch metadata and credential references are stored in the local config directory. On startup, R-Code attempts to migrate legacy plaintext Provider and MCP values out of configuration files where supported.

Do not place credentials in prompts, source files, logs or public issues. R-Code redacts common secret patterns from logs, but pattern-based redaction cannot guarantee removal of every sensitive value.

## Support bundles and issue reports

Support bundles are generated locally. Preview and inspect them before sharing, and send them only through a channel appropriate for the data they contain. The MCP section is a strict summary containing only server IDs, transport kinds, enabled/state values and error classes; it omits launch commands, arguments, URLs, header/environment names, credential references and values. Other bundle sections may still reveal paths, runtime metadata or diagnostic output even after known credential patterns are redacted.

Never attach proprietary source, raw application-data directories or credentials to a public GitHub issue. Security-sensitive reports should follow [SECURITY.md](./SECURITY.md).

## Your controls

You can reduce disclosure by:

- using pure chat without attaching a workspace;
- selecting `request_approval` or `risk_based` instead of `full_access`;
- keeping delegated subagents read-only;
- reviewing attachments, tool requests and diffs before approval;
- choosing a Provider and account with suitable retention controls;
- keeping optional MCP services disabled, using native web for ordinary research, and enabling a third-party MCP only after reviewing its exact launch or endpoint plan;
- leaving evolving memory disabled, setting a project to read-only/off, rejecting global candidates, editing/deleting individual entries, or clearing all local memory data;
- deleting tasks/local application data according to your organisation's retention policy.

## Changes and questions

Material implementation changes that affect these data flows should update this notice in the same release and be recorded in [CHANGELOG.md](./CHANGELOG.md). General questions can use the repository's GitHub Issues without including sensitive data; vulnerabilities must use the private process in [SECURITY.md](./SECURITY.md).
