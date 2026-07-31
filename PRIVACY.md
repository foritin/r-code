# R-Code Privacy Notice

Last updated: 2026-07-31

This notice describes the data flows implemented by the open-source R-Code desktop application. It is a technical baseline for a public release, not a substitute for legal review or for the privacy terms of a distributor, organisation, model provider or hosted service.

## Data stored on your device

R-Code stores application data under the operating system's application-data location. Depending on the features you use, this can include:

- tasks, workspace references, run state, approvals, tool-call metadata and notifications in SQLite;
- conversation and Agent events in JSONL session files;
- file baselines, verification output and other large content in content-addressed Blob storage;
- non-secret Provider configuration, application logs and recovery metadata;
- project memory files that you explicitly create in a workspace.

R-Code reads and modifies workspace files only when a workspace is attached and the selected access/approval policy allows the operation. Forgetting a workspace removes the application's workspace reference; it does not delete the source directory itself.

Local data remains on the device until it is removed through the application, by the user, or by operating-system cleanup. Uninstall behaviour varies by platform and installer, so do not assume uninstalling also erases the application-data directory.

## Data sent to model providers

When you use an LLM Provider, R-Code sends data needed to answer the request to the Provider endpoint you selected. This may include:

- system instructions and conversation messages;
- source snippets, file content or search results selected by you or read by approved tools;
- attachment content and metadata;
- tool definitions, tool results and error context;
- model, reasoning and inference settings.

The Provider processes and retains that data under its own terms and privacy policy. Before attaching confidential code or personal data, review the endpoint, account and data-retention settings shown in R-Code and the Provider's current policy. A custom Provider URL sends data to the operator of that URL.

Codex CLI and MCP collaboration use the Codex installation and account configured on the device. Their network traffic, authentication and service-side retention are governed by the applicable Codex/OpenAI configuration and terms, not by R-Code's local storage policy.

## Other network requests

R-Code may make network requests to:

- discover models from the configured Provider;
- execute a model request or an explicitly approved network-capable tool;
- install, authenticate or coordinate with an external CLI when you request that action;
- query GitHub Releases for `latest.json` and download a signed application update.

The current application code does not include first-party analytics, advertising SDKs or automatic product-usage telemetry. Local logs and support bundles are not uploaded automatically.

## Credentials

Provider API keys are intended to be stored in the operating-system credential store. Non-secret Provider settings are stored in the local config directory. On startup, R-Code attempts to migrate legacy plaintext Provider keys out of its config file.

Do not place credentials in prompts, source files, logs or public issues. R-Code redacts common secret patterns from logs, but pattern-based redaction cannot guarantee removal of every sensitive value.

## Support bundles and issue reports

Support bundles are generated locally. Preview and inspect them before sharing, and send them only through a channel appropriate for the data they contain. They may reveal paths, runtime metadata or diagnostic output even after known credential patterns are redacted.

Never attach proprietary source, raw application-data directories or credentials to a public GitHub issue. Security-sensitive reports should follow [SECURITY.md](./SECURITY.md).

## Your controls

You can reduce disclosure by:

- using pure chat without attaching a workspace;
- selecting `request_approval` or `risk_based` instead of `full_access`;
- keeping delegated subagents read-only;
- reviewing attachments, tool requests and diffs before approval;
- choosing a Provider and account with suitable retention controls;
- deleting tasks/local application data according to your organisation's retention policy.

## Changes and questions

Material implementation changes that affect these data flows should update this notice in the same release and be recorded in [CHANGELOG.md](./CHANGELOG.md). General questions can use the repository's GitHub Issues without including sensitive data; vulnerabilities must use the private process in [SECURITY.md](./SECURITY.md).
