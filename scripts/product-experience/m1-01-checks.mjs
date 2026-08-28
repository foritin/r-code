#!/usr/bin/env node
// M1-01 验收断言执行器：
//   node scripts/product-experience/m1-01-checks.mjs --part a1   # Rust+TS 同 fixture 契约
//   node scripts/product-experience/m1-01-checks.mjs --part a2   # locale 一致性 + 新增硬编码 0
//   node scripts/product-experience/m1-01-checks.mjs --part a3   # debug_detail 封锁扫描
// 每部分非交互、输出 JSON 结果、全部子命令 exit 0 才通过。

import { spawnSync } from "node:child_process";
import { parseArgs } from "node:util";
import path from "node:path";
import process from "node:process";

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..");
const FRONTEND = path.join(ROOT, "src-tauri", "frontend");
const CARGO_ENV = { ...process.env, CARGO_NET_GIT_FETCH_WITH_CLI: "true" };

function run(name, bin, args, options = {}) {
  const started = Date.now();
  const r = spawnSync(bin, args, {
    cwd: options.cwd ?? ROOT,
    encoding: "utf8",
    shell: false,
    timeout: options.timeout_ms ?? 20 * 60 * 1000,
    env: options.env ?? process.env,
  });
  const result = {
    name,
    command: [bin, ...args].join(" "),
    exit_code: r.status ?? null,
    timed_out: r.signal === "SIGTERM",
    duration_ms: Date.now() - started,
    stdout_tail: (r.stdout ?? "").split("\n").slice(-8).join("\n").slice(0, 1500),
    stderr_tail: (r.stderr ?? "").split("\n").slice(-8).join("\n").slice(0, 1500),
  };
  console.log(`${result.exit_code === 0 && !result.timed_out ? "PASS" : "FAIL"} ${name} (${result.duration_ms}ms)`);
  if (result.exit_code !== 0) {
    console.error((result.stderr_tail || result.stdout_tail || "").split("\n").slice(0, 6).join("\n"));
  }
  return result;
}

const parts = {
  async a1() {
    const results = [
      run("rust:user-error-fixture-contract", "cargo", ["test", "-p", "r-code-core", "--test", "user_error_contract"], { env: CARGO_ENV }),
      run("ts:user-error-fixture-contract", process.execPath, ["--test", "scripts/user-error-contract.test.mjs"], { cwd: FRONTEND }),
    ];
    return results;
  },
  async a2() {
    const results = [
      run("locale:key-placeholder-parity", process.execPath, ["scripts/product-experience/locale-consistency.mjs"]),
      run("locale:no-new-hardcoded-copy", process.execPath, ["--test", "scripts/i18n-hardcoded.test.mjs"], { cwd: FRONTEND }),
    ];
    return results;
  },
  async a3() {
    const results = [
      run("security:debug-detail-containment", process.execPath, ["--test", "scripts/debug-detail-containment.test.mjs"], { cwd: FRONTEND }),
    ];
    return results;
  },
};

let args;
try {
  args = parseArgs({
    options: { part: { type: "string" } },
    strict: true,
  });
} catch (error) {
  console.error(error.message);
  console.error("usage: m1-01-checks.mjs --part a1|a2|a3");
  process.exitCode = 2;
  process.exit();
}
const key = args.values.part?.replace(/^A/, "a");
const runner = parts[key];
if (!runner) {
  console.error(`unknown part: ${args.values.part}`);
  process.exitCode = 2;
  process.exit();
}

console.log(JSON.stringify({ schema_version: "m1-01-checks.v1", part: key.toLowerCase(), started_at: new Date().toISOString() }));
const results = await runner();
const ok = results.every((r) => r.exit_code === 0 && !r.timed_out);
process.exitCode = ok ? 0 : 1;
