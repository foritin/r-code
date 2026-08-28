#!/usr/bin/env node
// M7-04 验收断言执行器（file 逃逸拒绝、origin 授权、Task 隔离控制面板）：
//   a1: file://与 credentials origin 拒绝；localhost/exact origin 合法（scope m7_04 测试）
//   a2: browse/interact capability 分离（grant 层 + catalog capability）
//   a3: Task 隔离（installer session registry 每 task profile 隔离）
// 实现面：browser/scope.rs（BrowserOrigin/Grant）、browser/tools/catalog.rs、browser/installer.rs。

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
  a1: () => [run("rust:origin-parse-policy", "cargo", ["test", "-p", "r-code-host", "--lib", "m7_04_tests"], { env: CARGO_ENV })],
  a2: () => [
    run("rust:capability-separation", "cargo", ["test", "-p", "r-code-host", "--lib", "m7_04_tests::a2_"], { env: CARGO_ENV }),
    run("compat:m1-05-capability-catalog", process.execPath, [path.join(ROOT, "scripts/product-experience/m1-05-checks.mjs"), "--part", "a2"], { timeout_ms: 25 * 60 * 1000, env: { ...process.env, CARGO_NET_GIT_FETCH_WITH_CLI: "true" } }),
  ],
  a3: () => [
    run("rust:task-isolated-sessions", "cargo", ["test", "-p", "r-code-host", "--lib", "installer::tests::sessions_are_task_isolated"], { env: CARGO_ENV }),
  ],
};

let args;
try {
  args = parseArgs({ options: { part: { type: "string" } }, strict: true });
} catch (error) {
  console.error(`用法: m7-04-checks.mjs --part a1|a2|a3\n${error.message}`);
  process.exit(2);
}
const runner = parts[(args.values.part ?? "").toLowerCase()];
if (!runner) { console.error(`未知 part: ${args.values.part}`); process.exit(2); }
const results = await runner();
const allOk = results.every((r) => r.exit_code === 0 && !r.timed_out);
console.log(JSON.stringify({ schema_version: "m7-04-checks.v1", part: (args.values.part ?? "").toLowerCase(), ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
