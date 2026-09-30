#!/usr/bin/env node
// P24A.3 — the process-launch boundary guard.
//
// Every raw process creation in production Rust sources must live in the
// measured allowlist below (guardian, supervisor, supervised transport,
// helper binaries, or a documented legacy site owned by a later task).
// Anything new fails CI: a fresh Command::new / LocalShellBackend::new
// outside the allowlist is a launch bypass, not a review question.

import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

// The raw-launch constructors this guard recognizes. Async task spawning
// (tokio::spawn / runtime.spawn / Handle::spawn) is deliberately NOT here:
// those create tasks, never OS processes.
const RAW_LAUNCH_PATTERN = /Command::new\(|LocalShellBackend::new\(/;

// The four production areas P24A.1 inventoried. Test trees (crates/*/tests,
// scripts) are out of scope: tests own their own children.
const SCAN_ROOTS = [
  "crates/r-code-runtime/src",
  "crates/r-code-gateway/src",
  "crates/r-code-tui/src",
  "plugins/native/src",
  "plugins/codex/src",
];

// The measured inventory (2026-09-29). Keys are repo-relative, forward-slash
// paths. Every entry names WHY raw launches there are owned and which task
// owns the legacy ones; owners may remove entries, nobody may add one
// silently (that is the point of the guard).
const ALLOWED = new Map(
  Object.entries({
    "crates/r-code-runtime/src/process_guard/unix.rs":
      "the Unix guardian launch gate (P08) — owned launch primitive",
    "crates/r-code-runtime/src/process_guard/windows.rs":
      "the Windows guardian and suspended-job primitives (P06/P07)",
    "crates/r-code-runtime/src/process_guard/macos.rs":
      "the macOS guardian (P10)",
    "crates/r-code-runtime/src/services/process_supervisor.rs":
      "the supervisor contract and the real Windows job backend (P04/P05/P07)",
    "crates/r-code-runtime/src/plugins/transport.rs":
      "the supervised harness launch: frozen plan, creation-time containment, proof before resume (P23)",
    "crates/r-code-runtime/src/bin/r-code-process-guardian.rs":
      "the guardian helper binary itself",
    "crates/r-code-runtime/src/bin/r-code-safety-probe.rs":
      "the deny-probe helper binary (P13)",
    "crates/r-code-runtime/src/bin/process-tree-helper.rs":
      "the process-tree test helper binary",
    "crates/r-code-runtime/src/services/codex_cli.rs":
      "the Codex-account exception (PRD §3): bounded, output-capturing CLI probes for the one harness-managed login",
    "crates/r-code-runtime/src/services/sandbox/linux.rs":
      "pinned bwrap verification and test-stub ownership fixups (P16)",
    "crates/r-code-runtime/src/services/execution.rs":
      "SEAM (owner P28): ExecutionService::local() is test-only (t13 callers); production binds no shell backend",
    "crates/r-code-runtime/src/services/verification.rs":
      "SEAM (owner P28): VerificationRunner::new() retained for T19 fixtures only; the production checks path uses sandboxed()",
    "crates/r-code-gateway/src/execution_backend.rs":
      "LEGACY (owner P28): LocalShellBackend/Docker definitions; constructed by tests only after P24A",
    "crates/r-code-gateway/src/tools.rs":
      "LEGACY (owner P29): direct git invocations pending the read-only gix reader",
    "crates/r-code-gateway/src/tools_command.rs":
      "LEGACY (owner P28): the command tool pending exact-approved sandboxed Shell",
    "crates/r-code-tui/src/external_editor.rs":
      "user-invoked $EDITOR — an interactive user action, never agent-launched",
    "crates/r-code-tui/src/image_attach.rs":
      "user-invoked osascript/clipboard paste helpers",
  }),
);

function toRepoRelative(root, absolute) {
  return relative(root, absolute).split(sep).join("/");
}

function* walkRs(dir) {
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) {
      yield* walkRs(full);
    } else if (entry.endsWith(".rs")) {
      yield full;
    }
  }
}

/// Scan one repo tree. Returns violations as
/// { file, line, text } records (1-based lines).
export function scanTree(root) {
  const violations = [];
  for (const scanRoot of SCAN_ROOTS) {
    const absoluteRoot = resolve(root, scanRoot);
    let files;
    try {
      files = [...walkRs(absoluteRoot)];
    } catch {
      violations.push({
        file: scanRoot,
        line: 0,
        text: "scan root is missing — the guard must scan every production area",
      });
      continue;
    }
    for (const file of files) {
      const repoRelative = toRepoRelative(root, file);
      const allowed = ALLOWED.get(repoRelative);
      const lines = readFileSync(file, "utf8").split(/\r?\n/);
      lines.forEach((text, index) => {
        if (RAW_LAUNCH_PATTERN.test(text) && !allowed) {
          violations.push({ file: repoRelative, line: index + 1, text: text.trim() });
        }
      });
    }
  }
  return violations;
}

export const SCAN_INFO = { roots: SCAN_ROOTS, allowed: [...ALLOWED.keys()] };

function main() {
  const here = dirname(fileURLToPath(import.meta.url));
  const repoRoot = resolve(here, "..");
  const violations = scanTree(repoRoot);
  if (violations.length === 0) {
    console.log(
      `check-process-launch-boundary: ${SCAN_ROOTS.length} areas clean, ` +
        `${ALLOWED.size} owned sites allowlisted`,
    );
    return;
  }
  console.error(
    `check-process-launch-boundary: ${violations.length} raw launch site(s) ` +
      "outside the owned allowlist — move the launch into the " +
      "guardian/supervisor seam or record an owned allowlist entry with its " +
      "reason and owner task:",
  );
  for (const violation of violations) {
    console.error(`  ${violation.file}:${violation.line}: ${violation.text}`);
  }
  process.exitCode = 1;
}

const invokedDirectly =
  process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (invokedDirectly) {
  main();
}
