#!/usr/bin/env node
// M8-04 验收断言执行器：isolated-write 每 Run 独立 Worktree；binding 失败零启动；flag 关闭暂停。

import { spawnSync } from "node:child_process";
import { parseArgs } from "node:util";
import path from "node:path";
import process from "node:process";

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..");
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

const parts = {
  a1: () => [run("rust:worktree-atomic-creation", "cargo", ["test", "-p", "r-code-host", "--lib", "git_service::tests"], { env: CARGO_ENV })],
  a2: () => [run("binding:fail-closed-no-local-fallback", "cargo", ["test", "-p", "r-code-host", "--lib", "task_workspace_binding::tests::a3_"], { env: CARGO_ENV })],
  a3: () => [run("flags:worktree-gate-regression", "cargo", ["test", "-p", "r-code-host", "--lib", "feature_flags::"], { env: CARGO_ENV })],
};

let args;
try {
  args = parseArgs({ options: { part: { type: "string" } }, strict: true });
} catch (error) {
  console.error(error.message);
  process.exit(2);
}
const runner = parts[(args.values.part ?? "").toLowerCase()];
if (!runner) { console.error(`未知 part: ${args.values.part}`); process.exit(2); }
const results = await runner();
const allOk = results.every((r) => r.exit_code === 0 && !r.timed_out);
console.log(JSON.stringify({ schema_version: "m8-04-checks.v1", part: (args.values.part ?? "").toLowerCase(), ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
