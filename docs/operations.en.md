# R-Code operations guide

This guide is for people installing, upgrading, backing up, restoring, or removing a released R-Code desktop application. It describes the current `0.x` data layout and is separate from the maintainer-facing [release guide](./releasing.md).

## Safety rules

- Download installers only from the project's [GitHub Releases page](https://github.com/foritin/r-code/releases). Match the package to your operating system and CPU architecture, and check the signing state stated in the release notes before installing.
- Close R-Code and any Codex client using R-Code's managed MCP server before copying, moving, or replacing application data. Never copy only `r-code.db` while the application is running: SQLite may have active `-wal` and `-shm` sidecar files.
- Treat a full application-data copy as sensitive. It can contain conversations, workspace references, diagnostic output, and credential material. Store backups in an encrypted location with the same access controls as the workspaces you use in R-Code.
- On macOS, Provider and MCP credentials live in local encrypted files under `config/credentials/`; R-Code does not access Keychain. A complete profile backup includes both the ciphertext and its master key, so it can restore those credentials. Windows and Linux continue to use their operating-system credential stores, which are not exported by a file backup.

## Install

| Platform | Choose | Notes |
| --- | --- | --- |
| Windows x64 | Branded `.exe`, NSIS `.exe`, or WiX `.msi` | The `.msi` is appropriate for managed software deployment. Close an earlier R-Code instance before installing an upgrade. |
| macOS Apple Silicon / Intel | Matching architecture `.dmg` | Use the Apple Silicon build on M-series Macs and the Intel build on Intel Macs. Do not work around a Gatekeeper warning with bypass commands: confirm the published signing/notarization status first. |
| Linux x86_64 | `.deb` or `.AppImage` | Install a `.deb` with your distribution's package installer, or make an AppImage executable (`chmod +x`) and run it. |

For a normal upgrade, install the newer package over the existing application; do not remove the application-data directory first. The supported update path is the package published for the new release. Updater manifests and signatures are release artifacts, not a reason to skip the operating system's package and signature checks.

If an operating system reports that an installer is unsigned or untrusted, stop and compare the message with the release's stated signing status. Do not disable system security controls just to complete installation. Windows and macOS signing credentials are release-operations requirements; their absence is made explicit in the release notes rather than being hidden from users.

## Application-data location

The normal desktop profile is derived from the platform bundle identifier:

| Platform | Default profile path |
| --- | --- |
| Windows | `%APPDATA%\\com.r-code.app\\r-code` |
| macOS | `~/Library/Application Support/com.rcode.desktop/r-code` |
| Linux | `${XDG_DATA_HOME:-~/.local/share}/com.r-code.app/r-code` |

The profile normally contains:

```text
r-code/
├─ db/r-code.db     # product state in SQLite
├─ blobs/           # content-addressed baselines and large outputs
├─ sessions/        # JSONL conversation and agent events
├─ config/          # settings and references; also encrypted credentials on macOS
├─ logs/            # redacted diagnostic JSONL, retained for seven days
├─ plans/           # generated Plan Markdown projections, if Plan mode is used
└─ mcp-host/        # managed local MCP host binaries, if Codex integration is enabled
```

The standalone `mcp-server` mode can instead receive its entire data root via `--data-dir` or `R_CODE_DATA_DIR`. That setting is for the MCP process; it does not move an already-installed desktop application's profile. Do not point it at a source workspace or at a partial `db` directory.

## Backup before upgrade, reinstall, or recovery

Use a whole-profile backup, not a database-only copy:

1. Quit R-Code. Close Codex or other clients that may keep the managed R-Code MCP host alive.
2. Confirm that no R-Code process is still using the profile.
3. Copy the complete `r-code` directory to an encrypted backup destination. Preserve the directory structure and record the R-Code version and backup time.
4. Keep the backup until the upgraded application has opened the expected tasks, conversations, settings, and workspaces.

The application also protects schema upgrades. When an existing database needs a migration, both desktop and standalone MCP startup paths first run an integrity check and create a verified, WAL-safe SQLite snapshot in `db/` named like `r-code-pre-migration-<timestamp>-<uuid>.db`. The migration and a second integrity check then run before R-Code opens its connection pool. If either step fails, R-Code restores that snapshot and aborts startup instead of opening partially migrated data. Fresh profiles have no pre-migration snapshot because they have no prior user data.

Keep a known-good snapshot until the release has been accepted. Do not delete old snapshots as part of an installer cleanup unless they are covered by your own retention policy.

## Restore

### Restore a complete profile

1. Stop R-Code and all managed MCP host processes.
2. Make a second copy of the current profile before changing anything, even if it appears broken.
3. Replace the complete profile with the chosen full-profile backup, preserving the `db`, `blobs`, `sessions`, and `config` directories together.
4. Start R-Code and verify the expected task list, a representative conversation, and provider settings before deleting the failed profile copy.

Do not combine a database from one backup with `blobs` or `sessions` from a different point in time unless you are deliberately performing incident recovery. Those stores contain references to one another.

### If a schema upgrade fails

Read the startup error first. A normal migration failure has already attempted to restore its verified pre-migration snapshot and deliberately leaves startup stopped. Preserve the error and the snapshot named in it. If a second startup still fails, restore the latest complete profile backup. Database-only manual recovery is a last resort: keep a copy of the current profile, ensure every R-Code/MCP process is stopped, replace only `db/r-code.db` with the verified snapshot, and remove stale `r-code.db-wal` and `r-code.db-shm` files before starting the app. Do not perform those replacement steps while any process is running.

If automatic and manual recovery both fail, collect the redacted logs or a support bundle when possible and report the version, operating system, installation source, and exact error through [Support](../SUPPORT.md). Do not attach the database itself to a public issue.

## Uninstall and data retention

Uninstalling the application and deleting its data are separate decisions.

- The Windows NSIS uninstaller offers a delete-app-data choice. Leave it unchecked for an ordinary reinstall or when you need to preserve history. If selected, the uninstaller also attempts to stop R-Code-owned MCP hosts before removing the product's Roaming and Local AppData roots; a locked file can be scheduled for removal on the next reboot.
- On macOS, moving the app to Trash removes the application bundle, not necessarily its profile. On Linux, package removal likewise does not guarantee removal of the user data directory.
- To fully retire local history, first create and verify a backup if required by policy, then remove the profile path above. Removing it is irreversible for local conversations and task history.
- Current macOS credentials live under `config/credentials/` inside the profile and are removed with that app-data tree; legacy `r-code` Keychain entries are neither read nor deleted automatically. Windows/Linux system-credential entries likewise survive an ordinary uninstall. Remove an OS-store item only after confirming that it belongs to R-Code; never delete unrelated credentials by guessing names.

## Diagnostics and support bundles

Open **Settings → Diagnostics** to view the local log tail. The app retains the most recent seven calendar days of diagnostic logs. Select **Generate preview** to inspect a support bundle without creating a file, then select **Choose folder and export** only after reviewing what you intend to share.

The export is created locally and is not uploaded automatically. It includes the app version, platform, local counters, a restricted MCP summary, and recent redacted warning/error entries. Redaction reduces risk but is not a guarantee: inspect the resulting JSON before sending it. Never include a raw profile, provider key, MCP credential, private source file, or real conversation in a public issue. Security-sensitive reports must follow [SECURITY.md](../SECURITY.md).

## Windows command execution troubleshooting

- **Low command success rate / `ParserError` / `is not recognized`**: the `bash` tool on Windows prefers **Git Bash** (five-level resolution: settings override -> known locations -> derive from `git.exe` on PATH -> `bash.exe` on PATH (skipping the WSL `System32\bash.exe` launcher) -> PowerShell fallback). Installing Git for Windows takes effect without restarting R-Code (detection is cached for 5 minutes). Check the current dialect tier under Settings -> Tools & Connections -> Execution environment; the card shows a fallback warning when Git Bash is missing.
- **Force PowerShell**: in Settings -> Execution environment, clear the bash path override and save it as an **empty string** (`execution.bash_shell_path=""`), or fill in an absolute `bash.exe` path (a missing path errors loudly; it never silently falls back).
- **Newly installed tools not found**: child-process PATH is synthesized live from the registry (HKLM + HKCU), so no R-Code restart is needed; retry after confirming the tool wrote itself into the system or user PATH. Diagnosis hints suggest installation/PATH fixes automatically.
- **Codex subagent commands rejected (`blocked by policy`)**: expected behavior of the read-only delegation tier; re-delegate with `full_access` for write/network/exec operations (still gated by the approval matrix). Two consecutive rejections trigger a system-level read-only tier notice.
- **Golden-corpus self check**: `node scripts/verify-windows-reliability.mjs --through M4 --profile implementation` (full local assertions); corpus only: `node scripts/windows-reliability/corpus-run.mjs --tier fast --tag local --check thresholds`.

## Operator acceptance checklist

After an installation or upgrade, verify that the application opens, a known task and conversation are present, a provider can be configured without revealing its secret, and a support-bundle preview succeeds. Before a production rollout, perform the same install, upgrade, uninstall-with-data-preserved, and restore tests on a clean machine or VM for every distributed platform.
