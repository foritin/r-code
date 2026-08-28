#!/usr/bin/env node
// M6-03/M6-04 验收断言执行器（Worktree 生命周期与端到端）。
// M6-03: a1 重启恢复 binding 幂等；a2 dirty/unmanaged 目标保留；a3 归档/Review/flag。
// M6-04: a1 正向+拒绝/恢复组合；a2 双语亮暗视口键盘。

import { spawnSync } from "node:child_process";
import { parseArgs } from "node:util";
import path from "node:path";
import process from "node:process";

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..");
const FRONTEND = path.join(ROOT, "src-tauri", "frontend");
const CARGO_ENV = { ...process.env, CARGO_NET_GIT_FETCH_WITH_CLI: "true" };

function run(name, bin, args, options = {}) {
  const started = Date.now();
  const r = spawnSync(bin, args, { cwd: ROOT, encoding: "utf8", shell: false,
    timeout: options.timeout_ms ?? 30 * 60 * 1000, env: options.env ?? process.env });
  const result = { name, command: [bin, ...args].join(" "), exit_code: r.status ?? null,
    timed_out: r.signal === "SIGTERM", duration_ms: Date.now() - started,
    stdout_tail: (r.stdout ?? "").split("\n").slice(-8).join("\n").slice(0, 1200),
    stderr_tail: (r.stderr ?? "").split("\n").slice(-8).join("\n").slice(0, 1200) };
  console.log(`${result.exit_code === 0 && !r.timed_out ? "PASS" : "FAIL"} ${name} (${result.duration_ms}ms)`);
  if (result.exit_code !== 0) console.error((result.stderr_tail || "").split("\n").slice(0, 6).join("\n"));
  return result;
}

const PARTS = {
  "M6-03.A1": [["rust:worktree-restore-idempotent", "cargo", ["test", "-p", "r-code-host", "--lib", "task_workspace_binding::tests::a2_"], { env: CARGO_ENV }]],
  "M6-03.A2": [["rust:dirty-unmanaged-preserved", "cargo", ["test", "-p", "r-code-host", "--lib", "task_workspace_binding::tests::a3_"], { env: CARGO_ENV }]],
  "M6-03.A3": [["compat:feature-flag-matrix", process.execPath, ["--test", "scripts/feature-flag-matrix.test.mjs"], { cwd: FRONTEND, timeout_ms: 25 * 60 * 1000 }]],
  "a1": [["capsule:positive-and-rejection-matrix", process.execPath, ["--test", "scripts/m4-01-capsule.test.mjs"], { cwd: FRONTEND, timeout_ms: 25 * 60 * 1000 }]],
  "a3": [["a11y:i18n-theme-viewport-keyboard", process.execPath, ["--test", "scripts/m2-04-theme-responsive.test.mjs"], { cwd: FRONTEND, timeout_ms: 30 * 60 * 1000 }]],
};

let args;
try {
  args = parseArgs({ options: { part: { type: "string" } }, strict: true });
} catch (error) {
  console.error(`用法: m6-04-checks.mjs --part a1|a3（M6-03 残余断言由 m6-03 覆盖）\n${error.message}`);
  process.exit(2);
}

const runner = PARTS[(args.values.part ?? "").toLowerCase()];
if (!runner) {
  console.error(`未知 part: ${args.values.part}`);
  process.exit(2);
}
const results = runner.map(([name, bin, args2, opts]) => {
  const started = Date.now();
  const r = spawnSync(bin, args2, { cwd: opts?.cwd ?? ROOT, encoding: "utf8", shell: false,
    timeout: opts?.timeout_ms ?? 25 * 60 * 1000, env: opts?.env ?? process.env });
  const result = { name, command: [bin, ...args2].join(" "), exit_code: r.status ?? null,
    timed_out: r.signal === "SIGTERM", duration_ms: Date.now() - started,
    stdout_tail: (r.stdout ?? "").split("\n").slice(-8).join("\n").slice(0, 1200),
    stderr_tail: (r.stderr ?? "").split("\n").slice(-8).join("\n").slice(0, 1200) };
  console.log(`${result.exit_code === 0 && !result.timed_out ? "PASS" : "FAIL"} ${name} (${result.duration_ms}ms)`);
  return result;
});
const allOk = results.every((r) => r.exit_code === 0 && !r.timed_out);
console.log(JSON.stringify({ schema_version: "m6-04-checks.v1", part: args.values.part, ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
