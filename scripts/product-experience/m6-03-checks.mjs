#!/usr/bin/env node
// M6-03 验收断言执行器（Worktree 生命周期、重启恢复、Review 与安全清理）：
//   a1: 重启后合法 Worktree 恢复同一 binding；missing/mismatch 不回退且产生 Attention
//   a2: dirty/new commit/unmanaged 目录全部保留，clean/no-commit 才受控清理
//   a3: 归档停止运行保留工作区，Review 可跳转，关闭 flag 不改旧任务 binding

import { spawnSync } from "node:child_process";
import path from "node:path";
import { parseArgs } from "node:util";
import process from "node:process";

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..");
const CARGO_ENV = { ...process.env, CARGO_NET_GIT_FETCH_WITH_CLI: "true" };

function run(name, bin, args, options = {}) {
  const started = Date.now();
  const r = spawnSync(bin, args, { cwd: options.cwd ?? ROOT, encoding: "utf8", shell: false,
    timeout: options.timeout_ms ?? 30 * 60 * 1000, env: options.env ?? process.env });
  const result = { name, command: [bin, ...args].join(" "), exit_code: r.status ?? null,
    timed_out: r.signal === "SIGTERM", duration_ms: Date.now() - started,
    stdout_tail: (r.stdout ?? "").split("\n").slice(-8).join("\n").slice(0, 1200),
    stderr_tail: (r.stderr ?? "").split("\n").slice(-8).join("\n").slice(0, 1200) };
  console.log(`${result.exit_code === 0 && !r.timed_out ? "PASS" : "FAIL"} ${name} (${result.duration_ms}ms)`);
  if (result.exit_code !== 0) console.error((result.stderr_tail || "").split("\n").slice(0, 6).join("\n"));
  return result;
}

const parts = {
  a1: () => [run("rust:worktree-restore-idempotent", "cargo", ["test", "-p", "r-code-host", "--lib", "task_workspace_binding::tests::a2_"], { env: CARGO_ENV })],
  a2: () => [run("rust:dirty-unmanaged-preserved", "cargo", ["test", "-p", "r-code-host", "--lib", "task_workspace_binding::tests::a3_"], { env: CARGO_ENV })],
  a3: () => [run("compat:feature-flag-matrix", process.execPath, ["--test", path.join("scripts", "feature-flag-matrix.test.mjs")], { cwd: path.join(ROOT, "src-tauri", "frontend"), timeout_ms: 25 * 60 * 1000 })],
};

let args;
try {
  args = parseArgs({ options: { part: { type: "string" } }, strict: true });
} catch (error) {
  console.error(`用法: m6-03-checks.mjs --part a1|a2|a3\n${error.message}`);
  process.exit(2);
}
const runner = parts[(args.values.part ?? "").toLowerCase()];
if (!runner) { console.error(`未知 part: ${args.values.part}`); process.exit(2); }
const results = await runner();
const allOk = results.every((r) => r.exit_code === 0 && !r.timed_out);
console.log(JSON.stringify({ schema_version: "m6-03-checks.v1", part: (args.values.part ?? "").toLowerCase(), ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
