#!/usr/bin/env node
// M4-01 验收断言执行器（Run Capsule 派生模型、折叠与稳定回放）：
//   a1: §5.4 全状态矩阵唯一 detail_state，Attention/final 恒可发现
//   a2: 父终态级联封口 + 迟到帧不复活（纯函数 ≤1s 墙钟语义）+ Host terminal 单调
//   a3: live 序列化→重放一致
//   a4: latest update/摘要 raw reasoning/secret 零命中
// 实现面：src/lib/run-capsule.ts + scripts/m4-01-capsule.test.mjs。

import { spawnSync } from "node:child_process";
import { parseArgs } from "node:util";
import path from "node:path";
import process from "node:process";

const FRONTEND = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..", "src-tauri", "frontend");

function run(name, bin, args, options = {}) {
  const started = Date.now();
  const r = spawnSync(bin, args, {
    cwd: options.cwd ?? FRONTEND,
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
  console.log(`${result.exit_code === 0 && !r.timed_out ? "PASS" : "FAIL"} ${name} (${result.duration_ms}ms)`);
  if (result.exit_code !== 0) {
    console.error((result.stderr_tail || result.stdout_tail || "").split("\n").slice(0, 6).join("\n"));
  }
  return result;
}

const parts = {
  async a1() {
    return [run("capsule:state-matrix", process.execPath, ["--test", "scripts/m4-01-capsule.test.mjs"])];
  },
  async a2() {
    return [run("capsule:terminal-cascade-monotonic", process.execPath, ["--test", "scripts/m4-01-capsule.test.mjs"])];
  },
  async a3() {
    return [run("capsule:live-replay-consistency", process.execPath, ["--test", "scripts/m4-01-capsule.test.mjs"])];
  },
  async a4() {
    return [run("capsule:sanitized-latest-update", process.execPath, ["--test", "scripts/m4-01-capsule.test.mjs"])];
  },
};

let args;
try {
  args = parseArgs({ options: { part: { type: "string" } }, strict: true });
} catch (error) {
  console.error(`用法: m4-01-checks.mjs --part a1|a2|a3|a4\n${error.message}`);
  process.exit(2);
}

const runner = parts[(args.values.part ?? "").toLowerCase()];
if (!runner) {
  console.error(`未知 part: ${args.values.part}`);
  process.exit(2);
}
const results = await runner();
const allOk = results.every((r) => r.exit_code === 0 && !r.timed_out);
console.log(JSON.stringify({ schema_version: "m4-01-checks.v1", part: (args.values.part ?? "").toLowerCase(), ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
