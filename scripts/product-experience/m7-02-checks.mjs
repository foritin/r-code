#!/usr/bin/env node
// M7-02 验收断言执行器（按需安装单飞锁/每 Task Session 隔离/进程树上限）：
//   a1: 并发首次使用只安装一次；损坏 staging 不覆盖可用旧版
//   a2: 每 Task profile 隔离 + 进程数上限
//   a3: 重启恢复一律 stopped（不自动拉起）
//   a4: Task 删除清理其全部 session/profile，不影响其他 task
// 实现面：src-tauri/src/browser/installer.rs。

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
  a1: () => [run("rust:install-single-flight-and-corrupt-keeps-old", "cargo", ["test", "-p", "r-code-host", "--lib", "installer::tests::concurrent_first_use"], { env: CARGO_ENV }),
             run("rust:corrupt-staging-keeps-old", "cargo", ["test", "-p", "r-code-host", "--lib", "installer::tests::corrupt_staging_keeps"], { env: CARGO_ENV })],
  a2: () => [run("rust:sessions-task-isolated", "cargo", ["test", "-p", "r-code-host", "--lib", "installer::tests::sessions_are_task_isolated"], { env: CARGO_ENV })],
  a3: () => [run("rust:restart-restores-stopped", "cargo", ["test", "-p", "r-code-host", "--lib", "installer::tests::restart_restores_as_stopped"], { env: CARGO_ENV })],
  a4: () => [run("rust:task-delete-cleans-own-sessions", "cargo", ["test", "-p", "r-code-host", "--lib", "installer::tests::task_delete_cleans"], { env: CARGO_ENV })],
};

let args;
try {
  args = parseArgs({ options: { part: { type: "string" } }, strict: true });
} catch (error) {
  console.error(`用法: m7-02-checks.mjs --part a1|a2|a3|a4\n${error.message}`);
  process.exit(2);
}
const runner = parts[(args.values.part ?? "").toLowerCase()];
if (!runner) { console.error(`未知 part: ${args.values.part}`); process.exit(2); }
const results = await runner();
const allOk = results.every((r) => r.exit_code === 0 && !r.timed_out);
console.log(JSON.stringify({ schema_version: "m7-02-checks.v1", part: (args.values.part ?? "").toLowerCase(), ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
