#!/usr/bin/env node
// P31 — the deterministic safety conformance gate runner.
//
// Runs the wave's safety test corpus, the launch-boundary guard and the
// packaging declarations, then emits a redacted, deterministic per-target
// report: Activated or SafeDisabled/Unsupported with an exact digest of
// the material that produced the verdict. The report never contains
// secrets — only identities, statuses and digests.

import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(here, "..");

function run(name, command, args, options = {}) {
  const started = Date.now();
  const result = spawnSync(command, args, {
    cwd: repoRoot,
    encoding: "utf8",
    ...options,
  });
  return {
    name,
    ok: result.status === 0,
    seconds: ((Date.now() - started) / 1000).toFixed(1),
    tail: (result.stdout || "").split(/\r?\n/).filter(Boolean).slice(-3),
  };
}

export function digestOf(value) {
  return createHash("sha256").update(value).digest("hex").slice(0, 16);
}

export function redact(text) {
  // The report carries statuses/digests only: strip anything shaped like a
  // credential value the environment might leak into test output.
  return text
    .replace(/[A-Za-z0-9+/]{40,}={0,2}/g, "[redacted-blob]")
    .replace(
      /(token|secret|password|apikey|authorization)\s*[=:]\s*\S+/gi,
      "$1=[redacted]",
    );
}

export function buildReport({ suites, guard, packaging }) {
  const targets = [];
  // Platform-suites: executed on their native CI, reported separately.
  for (const [target, native] of [
    ["s08/s09/s16/s17 (linux-native)", true],
    ["s10/s11/s18 (macos-native)", true],
    ["s23 linux/macos arms (native)", true],
  ]) {
    targets.push({
      target,
      executed_here: false,
      verdict: "SafeDisabled",
      reason: "certified by native CI, not by this host",
      digest: digestOf(`${target}:${native}`),
    });
  }
  for (const suite of suites) {
    targets.push({
      target: suite.name,
      executed_here: true,
      verdict: suite.ok ? "Activated" : "SafeDisabled",
      reason: suite.ok ? "all tests green" : `failures: ${suite.tail.join("; ")}`,
      digest: digestOf(`${suite.name}:${suite.ok}:${suite.tail.join("|")}`),
    });
  }
  targets.push({
    target: "launch-boundary guard",
    executed_here: true,
    verdict: guard.ok ? "Activated" : "SafeDisabled",
    reason: guard.ok ? `${guard.tail[0] ?? ""}`.trim() : "guard failed",
    digest: digestOf(`guard:${guard.ok}`),
  });
  targets.push({
    target: "packaging declarations",
    executed_here: true,
    verdict: packaging.ok ? "Activated" : "SafeDisabled",
    reason: packaging.ok ? "declarations in lockstep" : "packaging failed",
    digest: digestOf(`packaging:${packaging.ok}`),
  });
  return {
    schema: "safety-v1-report/1",
    generated_by: "verify-safety-v1",
    targets,
    summary: {
      total: targets.length,
      activated: targets.filter((t) => t.verdict === "Activated").length,
      safe_disabled: targets.filter((t) => t.verdict !== "Activated").length,
    },
  };
}

function main() {
  const suites = [
    run("s04 supervisor contract", "cargo", [
      "test", "-p", "r-code-runtime", "--test", "s04_supervisor_contract", "--all-features",
    ]),
    run("s05 supervisor faults", "cargo", [
      "test", "-p", "r-code-runtime", "--test", "s05_supervisor_faults", "--all-features",
    ]),
    run("s12 safety reports", "cargo", [
      "test", "-p", "r-code-runtime", "--test", "s12_safety_api", "--all-features",
    ]),
    run("s13 sandbox contract", "cargo", [
      "test", "-p", "r-code-runtime", "--test", "s13_sandbox_contract", "--all-features",
    ]),
    run("s24A composition guard", "cargo", [
      "test", "-p", "r-code-runtime", "--test", "s24a_composition", "--all-features",
    ]),
    run("s24B activation", "cargo", [
      "test", "-p", "r-code-runtime", "--test", "s24b_activation", "--all-features",
    ]),
    run("s25 process-effect store", "cargo", [
      "test", "-p", "r-code-store", "--test", "s25_process_effect_store", "--all-features",
    ]),
    run("s26 process delta", "cargo", [
      "test", "-p", "r-code-runtime", "--test", "s26_process_delta", "--all-features",
    ]),
    run("s26A effect artifacts", "cargo", [
      "test", "-p", "r-code-runtime", "--test", "s26a_effect_artifacts", "--all-features",
    ]),
    run("s27 effect recovery", "cargo", [
      "test", "-p", "r-code-runtime", "--test", "s27_process_effect_recovery", "--all-features",
    ]),
    run("s28 shell gate", "cargo", [
      "test", "-p", "r-code-runtime", "--test", "s28_shell_gate", "--all-features",
    ]),
    run("s29 gix reader", "cargo", [
      "test", "-p", "r-code-runtime", "--test", "s29_gix_status_log", "--all-features",
    ]),
    run("s30 gix diff tools", "cargo", [
      "test", "-p", "r-code-runtime", "--test", "s30_gix_diff_tools", "--all-features",
    ]),
    run("s31 safety gate", "cargo", [
      "test", "-p", "r-code-runtime", "--test", "s31_safety_gate", "--all-features",
    ]),
  ];
  const guard = run("guard", "node", ["scripts/check-process-launch-boundary.mjs"]);
  const packaging = run("packaging", "node", ["scripts/harness-packaging.test.mjs"]);

  const report = buildReport({ suites, guard, packaging });
  const redacted = JSON.parse(redact(JSON.stringify(report)));
  const failed = redacted.targets.filter((target) => target.executed_here && target.verdict !== "Activated");
  for (const target of redacted.targets) {
    console.log(
      `${target.verdict === "Activated" ? "PASS" : "NATIVE"} ${target.target} [${target.digest}] ${target.reason}`,
    );
  }
  console.log(
    `summary: ${redacted.summary.activated}/${redacted.summary.total} activated ` +
      `(${redacted.summary.safe_disabled} SafeDisabled incl. native-CI-only targets)`,
  );
  const outPath = join(repoRoot, "target", "safety-v1-report.json");
  writeFileSync(outPath, JSON.stringify(redacted, null, 2));
  console.log(`report: ${outPath}`);
  if (failed.length > 0) {
    console.error(`${failed.length} executed target(s) failed`);
    process.exitCode = 1;
  }
}

const invokedDirectly =
  process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (invokedDirectly) {
  main();
}
