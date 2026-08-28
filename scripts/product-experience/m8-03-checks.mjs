#!/usr/bin/env node
// M8-03 验收断言执行器：read-only Automation 注册期过滤（mutating/shell/interact/unknown=0）
// 与允许的 workspace read/browse 正常工作且审计准确。

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { parseArgs } from "node:util";
import path from "node:path";
import process from "node:process";

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..");

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
  a1: () => [
    run("rust:capability-separation-registry", "cargo", ["test", "-p", "r-code-host", "--lib", "m7_04_tests"], { env: { ...process.env, CARGO_NET_GIT_FETCH_WITH_CLI: "true" } }),
  ],
  a3: () => [
    run("compat:automation-contract", process.execPath, ["--test", "scripts/automation-contract.test.mjs"], { cwd: path.join(ROOT, "src-tauri", "frontend") }),
  ],
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
console.log(JSON.stringify({ schema_version: "m8-03-checks.v1", part: (args.values.part ?? "").toLowerCase(), ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
