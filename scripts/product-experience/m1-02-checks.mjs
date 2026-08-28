#!/usr/bin/env node
// M1-02 验收断言执行器：
//   node scripts/product-experience/m1-02-checks.mjs --part a1   # 三层 flag 矩阵 + IPC 接闸一致性
//   node scripts/product-experience/m1-02-checks.mjs --part a2   # 合法 Local/Worktree 解析 + 幂等回读
//   node scripts/product-experience/m1-02-checks.mjs --part a3   # 越界/mismatch/symlink 全拒绝且无 fallback
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
    return [
      run(
        "frontend:feature-flag-matrix",
        process.execPath,
        ["--test", "scripts/feature-flag-matrix.test.mjs", "scripts/m1-02-gating-parity.test.mjs"],
        { cwd: FRONTEND },
      ),
      run("rust:feature-flag-service", "cargo", ["test", "-p", "r-code-host", "--lib", "feature_flags::"], { env: CARGO_ENV }),
    ];
  },
  async a2() {
    return [
      run(
        "rust:binding-local-worktree-idempotent",
        "cargo",
        ["test", "-p", "r-code-host", "--lib", "task_workspace_binding::tests::a2_"],
        { env: CARGO_ENV },
      ),
    ];
  },
  async a3() {
    return [
      run(
        "rust:binding-fail-closed-rejections",
        "cargo",
        ["test", "-p", "r-code-host", "--lib", "task_workspace_binding::tests::a3_"],
        { env: CARGO_ENV },
      ),
    ];
  },
};

let args;
try {
  args = parseArgs({
    options: { part: { type: "string" } },
    strict: true,
  });
} catch (error) {
  console.error(`用法: m1-02-checks.mjs --part a1|a2|a3\n${error.message}`);
  process.exit(2);
}

const part = args.values.part;
const runner = parts[part];
if (!runner) {
  console.error(`未知 part: ${part}`);
  process.exit(2);
}

const results = await runner();
const allOk = results.every((r) => r.exit_code === 0 && !r.timed_out);
console.log(JSON.stringify({ schema_version: "m1-02-checks.v1", part, ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
