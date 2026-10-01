#!/usr/bin/env node
// Appendix-A baseline runner (project-context acceptance).
// Runs ONE arm (off|on) of ONE group's 12 standard tasks end to end through
// the context-baseline driver against the shared r-code-service daemon, then
// writes the per-task metrics artifact used by score-summary.mjs.
//
// Usage:
//   node docs/prd/project-context/acceptance/scripts/run-baseline.mjs \
//     --group a|b --arm off|on [--only A-1,A-5] [--timeout-secs 900]
//     [--seed-settings <settings.json>] [--require-context-rpc true|false]
//     [--driver <path to prebuilt context-baseline binary>]
//
// Artifacts:
//   docs/prd/project-context/acceptance/artifacts/baseline-<group>-<arm>.json
//   .../artifacts/patches/<group>-<arm>/<taskId>.patch
//   sandbox/project-context-acceptance/events/<group>-<arm>/<taskId>.jsonl
//
// Baseline policy: pristine fixture reset before each task, auto-granted
// approvals, identical policy across arms (see README).

import { execFileSync, spawnSync } from "node:child_process";
import { dirname } from "node:path";
import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import process from "node:process";

const ACCEPT_ROOT = path.resolve(dirname(fileURLToPath(import.meta.url)), "..");
const REPO_ROOT = path.resolve(ACCEPT_ROOT, "../../../..");
const WORK_ROOT = path.join(REPO_ROOT, "sandbox", "project-context-acceptance");
const ART_ROOT = path.join(ACCEPT_ROOT, "artifacts");
const TASKS = JSON.parse(readFileSync(path.join(ACCEPT_ROOT, "tasks.json"), "utf8"));

const GROUP_DIRS = { a: "a-r-code", b: "b-jq" };

function parseArgs() {
  const args = {
    group: null, arm: null, only: null, timeoutSecs: 900,
    seedSettings: path.join(WORK_ROOT, "seed", "settings.json"),
    requireContextRpc: null, driver: null,
  };
  const argv = process.argv.slice(2);
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === "--group") args.group = argv[++i];
    else if (argv[i] === "--arm") args.arm = argv[++i];
    else if (argv[i] === "--only") args.only = argv[++i].split(",").map((s) => s.trim());
    else if (argv[i] === "--timeout-secs") args.timeoutSecs = Number(argv[++i]);
    else if (argv[i] === "--seed-settings") args.seedSettings = argv[++i];
    else if (argv[i] === "--require-context-rpc") args.requireContextRpc = argv[++i] === "true";
    else if (argv[i] === "--driver") args.driver = argv[++i];
    else { console.error(`unknown argument ${argv[i]}`); process.exit(1); }
  }
  if (!args.group || !GROUP_DIRS[args.group]) { console.error("--group a|b is required"); process.exit(1); }
  if (!args.arm || !["off", "on"].includes(args.arm)) { console.error("--arm off|on is required"); process.exit(1); }
  // Default: the on arm requires the FR-1 switch RPC; the off arm never
  // calls it (it is the pre-feature baseline).
  if (args.requireContextRpc === null) args.requireContextRpc = args.arm === "on";
  return args;
}

function git(cwd, ...rest) {
  return execFileSync("git", ["-C", cwd, ...rest], { encoding: "utf8", stdio: ["ignore", "pipe", "inherit"] }).trim();
}

function resetPristine(fixtureDir) {
  git(fixtureDir, "checkout", "--", ".");
  git(fixtureDir, "clean", "-xfd");
  // Nested submodules (A fixture carries vendor/agent-contracts + .agents).
  const subs = git(fixtureDir, "submodule", "status").split("\n").filter(Boolean);
  if (subs.length > 0) {
    execFileSync(
      "git",
      ["-C", fixtureDir, "submodule", "foreach", "--recursive", "git checkout -- . && git clean -xfd"],
      { stdio: ["ignore", "ignore", "inherit"] },
    );
  }
}

function seedProfile(dataRoot, seedSettings) {
  // Obsolete under the shared-daemon design: the bootstrap script owns
  // provider configuration. Retained for standalone (non-shared) profiles.
  // settings.json sits under <data-root>/harness-v1/<flavor>/ (SettingsStore
  // layout); credentials resolve from the platform store by provider name,
  // so seeding must happen on a machine where those providers exist.
  const flavor = process.env.R_CODE_BASELINE_FLAVOR ?? "development";
  const profileDir = path.join(dataRoot, "harness-v1", flavor);
  const target = path.join(profileDir, "settings.json");
  if (existsSync(target)) return;
  if (!existsSync(seedSettings)) {
    throw new Error(
      `no settings at ${target} and no seed file at ${seedSettings} — copy a configured ` +
        "profile's settings.json to the seed path first (see acceptance/README.md)",
    );
  }
  mkdirSync(profileDir, { recursive: true });
  copyFileSync(seedSettings, target);
}

function resolveDriver(args) {
  if (args.driver) return args.driver;
  const exe = process.platform === "win32" ? "context-baseline.exe" : "context-baseline";
  const beside = path.join(REPO_ROOT, "target", "debug", exe);
  if (existsSync(beside)) return beside;
  console.error("[runner] prebuilt driver not found; building via cargo (one-off)");
  const built = spawnSync(
    "cargo",
    ["build", "--quiet", "-p", "r-code-evals", "--bin", "context-baseline"],
    { cwd: REPO_ROOT, stdio: "inherit", shell: process.platform === "win32" },
  );
  if (built.status !== 0) throw new Error("cargo build of context-baseline failed");
  return beside;
}

function run() {
  const args = parseArgs();
  const fixtureDir = path.join(WORK_ROOT, "fixtures", GROUP_DIRS[args.group]);
  if (!existsSync(fixtureDir)) {
    throw new Error(`fixture missing: ${fixtureDir} — run prepare-fixtures.mjs first`);
  }
  // Arms share ONE bootstrapped daemon profile (data-baseline + m1a-baseline
  // pipe): the off/on lever is the per-workspace context.settings RPC, and a
  // single daemon keeps provider credentials and the native plugin install
  // from the one-time bootstrap (see bootstrap-baseline-env.mjs).
  const dataRoot = process.env.R_CODE_BASELINE_DATA_ROOT
    ?? path.join(WORK_ROOT, "data-baseline");
  const ipcName = process.env.R_CODE_BASELINE_IPC_NAME ?? "m1a-baseline";
  seedProfile(dataRoot, args.seedSettings);

  const driver = resolveDriver(args);
  const eventsDir = path.join(WORK_ROOT, "events", `${args.group}-${args.arm}`);
  const patchDir = path.join(ART_ROOT, "patches", `${args.group}-${args.arm}`);
  mkdirSync(eventsDir, { recursive: true });
  mkdirSync(patchDir, { recursive: true });

  const group = TASKS.groups[args.group];
  const wanted = args.only ? new Set(args.only) : null;
  const rows = [];
  for (const task of group.tasks) {
    if (wanted && !wanted.has(task.id)) continue;
    console.log(`\n[runner] ${task.id} (${task.category}/${task.mode}) arm=${args.arm}`);
    resetPristine(fixtureDir);
    const objectiveFile = path.join(eventsDir, `${task.id}.objective.txt`);
    writeFileSync(objectiveFile, task.prompt);

    const cli = [
      "--data-root", dataRoot,
      "--ipc-name", ipcName,
      "--workspace", fixtureDir,
      "--mode", task.mode,
      "--objective-file", objectiveFile,
      "--title", task.id,
      "--timeout-secs", String(args.timeoutSecs),
      "--events-out", path.join(eventsDir, `${task.id}.jsonl`),
    ];
    if (args.requireContextRpc || args.arm === "on") {
      cli.push("--context-injection", args.arm === "on" ? "on" : "off");
    }
    const result = spawnSync(driver, cli, { encoding: "utf8", shell: false });
    let parsed = null;
    try { parsed = JSON.parse((result.stdout ?? "").split("\n").filter(Boolean).pop() ?? ""); } catch { /* keep null */ }
    if (parsed == null) {
      console.error(`[runner] ${task.id}: driver failed (exit ${result.status})\n${result.stderr ?? ""}`);
    }

    // Post-task diff evidence for modify tasks (and harmless for others).
    const patchFile = path.join(patchDir, `${task.id}.patch`);
    try {
      const diff = git(fixtureDir, "diff", "HEAD");
      writeFileSync(patchFile, diff);
    } catch (e) {
      console.warn(`[runner] ${task.id}: diff capture failed: ${e.message}`);
    }

    rows.push({
      taskId: task.id,
      category: task.category,
      mode: task.mode,
      settled: parsed?.settled ?? false,
      terminalJournalKind: parsed?.terminalJournalKind ?? null,
      usage: parsed?.usage ?? null,
      wallMs: parsed?.wallMs ?? null,
      toolCalls: parsed?.toolCalls ?? null,
      assistantChars: parsed?.assistantChars ?? null,
      approvalsGranted: parsed?.approvalsGranted ?? 0,
      runCount: parsed?.runCount ?? null,
      driverExit: result.status,
      patch: `patches/${args.group}-${args.arm}/${task.id}.patch`,
    });
    writeArtifact(args, rows);
    console.log(
      `[runner] ${task.id}: settled=${rows.at(-1).settled} tokens=${rows.at(-1).usage?.totalTokens ?? "n/a"}`,
    );
  }

  writeArtifact(args, rows);
  console.log(`\n[runner] wrote ${rows.length} rows for group ${args.group} arm ${args.arm}`);
}

function writeArtifact(args, rows) {
  const out = path.join(ART_ROOT, `baseline-${args.group}-${args.arm}.json`);
  mkdirSync(ART_ROOT, { recursive: true });
  const totalTokens = rows.reduce((sum, r) => sum + (r.usage?.totalTokens ?? 0), 0);
  const body = {
    generatedAt: new Date().toISOString(),
    group: args.group,
    arm: args.arm,
    fixturePinnedBy: "sandbox/project-context-acceptance/fixtures/fixtures.json",
    baselinePolicy: { autoApprove: true, pristineReset: true, singleRunPerTask: true },
    totals: { tasks: rows.length, settled: rows.filter((r) => r.settled).length, totalTokens },
    rows,
  };
  writeFileSync(out, `${JSON.stringify(body, null, 2)}\n`);
}

try {
  run();
} catch (error) {
  console.error(`run-baseline: ${error.message}`);
  process.exit(1);
}
