// T34 — desktop plugin management UI flows (real backend surface).
//
// The UI commands (cmd_harness_v2_*) delegate to the same ApplicationService
// operations this test drives through r-code-harness-admin: install an
// independent example, select it for a new branch, and reject removal of a
// live pinned version.

import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, copyFileSync, readFileSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const adminExe = join(repoRoot, "target", "debug", process.platform === "win32" ? "r-code-harness-admin.exe" : "r-code-harness-admin");
const repairExe = join(repoRoot, "target", "debug", process.platform === "win32" ? "repair-harness.exe" : "repair-harness");

function cargo(...args) {
  execFileSync("cargo", args, { cwd: repoRoot, timeout: 10 * 60 * 1000, stdio: ["ignore", "pipe", "pipe"] });
}

function admin(work, ...args) {
  return execFileSync(adminExe, ["--profile", "development", "--data-root", work, "--ipc-name", "t34-ui", ...args], {
    timeout: 60_000,
    encoding: "utf8",
  });
}

function adminFails(work, ...args) {
  try {
    admin(work, ...args);
    return null;
  } catch (error) {
    return String(error.stderr || error.message);
  }
}

function stagePackage(dir) {
  const source = join(dir, "pkg");
  mkdirSync(join(source, "bin"), { recursive: true });
  copyFileSync(repairExe, join(source, "bin", process.platform === "win32" ? "repair-harness.exe" : "repair-harness"));
  const platform = process.platform === "win32" ? "windows-x64"
    : process.platform === "darwin" && process.arch === "arm64" ? "macos-arm64"
    : process.platform === "darwin" ? "macos-x64" : "linux-x64";
  const executable = process.platform === "win32" ? "bin/repair-harness" : "bin/repair-harness";
  writeFileSync(join(source, "harness.json"), JSON.stringify({
    schema_version: "1",
    id: "repair-harness.example",
    version: "1.0.0",
    apiMajor: 1,
    apiMinor: 0,
    displayName: "Repair Harness (example)",
    supportedPlatforms: [{ platform, executable }],
    supportedFeatures: ["multi-turn-tools", "plan-hitl"],
    requestedHostServices: [
      "host.model.stream", "host.tools.list", "host.tools.call",
      "host.checkpoint.save", "host.completion.propose",
    ],
    configSchema: { type: "object" },
  }));
  return source;
}

test("UI plugin flows: install, select for a new branch, pinned removal rejected", () => {
  cargo("build", "-p", "r-code-runtime", "--bin", "r-code-harness-admin");
  cargo("build", "-p", "repair-harness");

  const work = mkdtempSync(join(tmpdir(), "t34-ui-"));
  try {
    const packageDir = stagePackage(work);

    // Install the independent example.
    const installed = JSON.parse(admin(work, "install", packageDir));
    assert.equal(installed.id, "repair-harness.example");
    assert.ok(installed.contentDigest.length >= 16);

    // The catalog lists it as available.
    const listing = JSON.parse(admin(work, "list"));
    const entry = listing.find((candidate) => candidate.manifest.id === "repair-harness.example");
    assert.ok(entry, "installed entry visible in the catalog");
    assert.equal(entry.availability, "Available");
    assert.equal(entry.manifest.displayName, "Repair Harness (example)");

    // Select it for a new branch (task): the pin records the exact bytes.
    admin(work, "task-create", "task-branch-1", "repair the fixture");
    const selected = JSON.parse(admin(work, "select", "task-branch-1", "repair-harness.example"));
    assert.equal(selected.pinned, "repair-harness.example");
    assert.equal(selected.digest, installed.contentDigest);

    // Removing the live pinned version is rejected.
    const failure = adminFails(work, "remove", "repair-harness.example", installed.contentDigest);
    assert.ok(failure && /pinned/.test(failure), `expected pinned rejection, got: ${failure}`);

    // Enable/disable is reversible without touching the pin.
    admin(work, "disable", "repair-harness.example", installed.contentDigest);
    const disabled = JSON.parse(admin(work, "list"));
    assert.deepEqual(
      disabled.find((candidate) => candidate.manifest.id === "repair-harness.example").availability,
      { Unavailable: "Disabled" },
    );
    admin(work, "enable", "repair-harness.example", installed.contentDigest);
    const reenabled = JSON.parse(admin(work, "list"));
    assert.equal(
      reenabled.find((candidate) => candidate.manifest.id === "repair-harness.example").availability,
      "Available",
    );

    // The frontend bindings expose the same command surface.
    const ipc = readFileSync(join(repoRoot, "src-tauri/frontend/src/lib/ipc.ts"), "utf8");
    for (const command of [
      "cmd_harness_v2_plugins_list",
      "cmd_harness_v2_plugins_install",
      "cmd_harness_v2_plugins_set_enabled",
      "cmd_harness_v2_plugins_remove",
      "cmd_harness_v2_task_select_harness",
    ]) {
      assert.ok(ipc.includes(command), `ipc.ts missing ${command}`);
    }
  } finally {
    // Stop this test's daemon explicitly (tests own their daemon).
    try {
      const owner = JSON.parse(readFileSync(join(work, "harness-v2", "owner.json"), "utf8"));
      process.kill(owner.pid);
    } catch {
      /* already gone */
    }
    rmSync(work, { recursive: true, force: true });
  }
});
