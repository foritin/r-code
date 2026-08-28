#!/usr/bin/env node
// M4-05 验收断言执行器（运行收尾摘要与跳转）：
//   a1: 四终态摘要字段完整；never-tested/failed 不显示“已验证”（capsule 脱敏复用）
//   a2: deep link 到达正确目标并可返回主任务（搜索深链 E2E）
//   a3: reload 后摘要与实时一致，不重复工具列表、不暴露 raw reasoning
// 实现面：SessionRunSummary.tsx + session-run-summary-model.ts + session-run-summary.test.mjs。

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
    return [
      run("summary:session-run-summary", process.execPath, ["--test", "scripts/session-run-summary.test.mjs"]),
      run("capsule:sanitize-regression", process.execPath, ["--test", "scripts/m4-01-capsule.test.mjs"]),
    ];
  },
  async a2() {
    return [
      run("e2e:search-deeplink-and-return", process.execPath, ["--test", "scripts/m2-03-a6-search-e2e.test.mjs"]),
      run("compat:run-guard-ui", process.execPath, ["--test", "scripts/run-guard-ui.test.mjs"]),
    ];
  },
  async a3() {
    return [
      run("summary:reload-consistency", process.execPath, ["--test", "scripts/session-run-summary.test.mjs"]),
    ];
  },
};

let args;
try {
  args = parseArgs({ options: { part: { type: "string" } }, strict: true });
} catch (error) {
  console.error(`用法: m4-05-checks.mjs --part a1|a2|a3\n${error.message}`);
  process.exit(2);
}

const runner = parts[(args.values.part ?? "").toLowerCase()];
if (!runner) {
  console.error(`未知 part: ${args.values.part}`);
  process.exit(2);
}
const results = await runner();
const allOk = results.every((r) => r.exit_code === 0 && !r.timed_out);
console.log(JSON.stringify({ schema_version: "m4-05-checks.v1", part: (args.values.part ?? "").toLowerCase(), ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
