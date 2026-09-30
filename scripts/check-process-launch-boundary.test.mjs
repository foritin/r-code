// P24A — tests for the process-launch boundary guard.
//
// The guard must catch fresh raw-launch sites (Command::new /
// LocalShellBackend::new) outside the owned allowlist, ignore async-task
// spawning (which never creates OS processes), honour the allowlisted
// guardian/supervisor sites, fail when a scan root disappears, and hold the
// real repository tree clean.

import { strict as assert } from "node:assert";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { scanTree } from "./check-process-launch-boundary.mjs";

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");

function fixtureRoot() {
  return mkdtempSync(join(tmpdir(), "p24a-guard-"));
}

function writeFixture(root, relative, body) {
  const file = join(root, relative);
  mkdirSync(dirname(file), { recursive: true });
  writeFileSync(file, body);
  return file;
}

test("a fresh Command::new outside the allowlist is caught, allowlisted sites and async tasks are not", () => {
  const root = fixtureRoot();
  try {
    // All five scan roots exist; four are empty, one carries the violations.
    for (const empty of [
      "crates/r-code-gateway/src/empty.rs",
      "crates/r-code-tui/src/empty.rs",
      "plugins/native/src/empty.rs",
      "plugins/codex/src/empty.rs",
    ]) {
      writeFixture(root, empty, "// clean\n");
    }
    writeFixture(
      root,
      "crates/r-code-runtime/src/services/new_shell_shortcut.rs",
      [
        "use tokio::spawn_task as _;",
        "async fn helper() {",
        "    tokio::spawn(async {}); // async task: NOT a process launch",
        "    let child = std::process::Command::new(\"sh\").spawn();",
        "    let _ = child;",
        "}",
        "",
      ].join("\n"),
    );
    const violations = scanTree(root);
    assert.equal(violations.length, 1, `exactly one violation: ${JSON.stringify(violations)}`);
    assert.ok(violations[0].file.endsWith("services/new_shell_shortcut.rs"));
    assert.equal(violations[0].line, 4, "the violating line is the Command::new line");
    assert.ok(violations[0].text.includes("Command::new"));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("a fresh LocalShellBackend construction is caught in every scan area", () => {
  const root = fixtureRoot();
  try {
    writeFixture(
      root,
      "crates/r-code-runtime/src/services/new_backend_user.rs",
      "let backend = LocalShellBackend::new();\n",
    );
    writeFixture(
      root,
      "crates/r-code-tui/src/another_shell_escape.rs",
      "let backend = LocalShellBackend::new();\n",
    );
    for (const empty of [
      "crates/r-code-gateway/src/empty.rs",
      "plugins/native/src/empty.rs",
      "plugins/codex/src/empty.rs",
    ]) {
      writeFixture(root, empty, "// clean\n");
    }
    const violations = scanTree(root);
    assert.equal(violations.length, 2, JSON.stringify(violations));
    const files = violations.map((violation) => violation.file).sort();
    assert.ok(files[0].includes("new_backend_user.rs"));
    assert.ok(files[1].includes("another_shell_escape.rs"));
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("an allowlisted guardian site is exempt, and a dropped scan root fails the guard", () => {
  const contained = fixtureRoot();
  try {
    // The supervised transport sits in the allowlist: its contained
    // Command::new is the owned launch, not a violation.
    writeFixture(
      contained,
      "crates/r-code-runtime/src/plugins/transport.rs",
      "let mut command = Command::new(&plan.executable);\n",
    );
    for (const empty of [
      "crates/r-code-gateway/src/empty.rs",
      "crates/r-code-tui/src/empty.rs",
      "plugins/native/src/empty.rs",
      "plugins/codex/src/empty.rs",
    ]) {
      writeFixture(contained, empty, "// clean\n");
    }
    assert.deepEqual(scanTree(contained), [], "the allowlisted site is exempt");
  } finally {
    rmSync(contained, { recursive: true, force: true });
  }

  const emptyRoot = fixtureRoot();
  try {
    const violations = scanTree(emptyRoot);
    assert.equal(
      violations.length,
      5,
      "a missing scan root is itself a violation — the guard never silently narrows",
    );
  } finally {
    rmSync(emptyRoot, { recursive: true, force: true });
  }
});

test("the real repository tree holds the boundary", () => {
  assert.deepEqual(
    scanTree(repoRoot),
    [],
    "the production tree must stay inside the owned allowlist; new raw launches belong in the guardian/supervisor seam",
  );
});
