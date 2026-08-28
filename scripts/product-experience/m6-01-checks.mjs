#!/usr/bin/env node
// M6-01 验收断言执行器（Worktree 开关、任务选择、托管 schema 与原子创建）：
//   a1: 非 Git/disabled 只能 Local；合法 Git fixture schema round-trip 与迁移幂等
//   a2: 原子创建后 repo/branch/base_oid/path/managed identity 一致
//   a3: 故障注入不改变原项目；不确定/dirty/有新 commit 目标永不自动删除
// 实现面：task_workspace_binding.rs（M1-02 fail-closed fixture）、git_service.rs（worktree 原子创建/校验/清理）、
//         feature_flags（M1-02 Worktree 位）。

import { spawnSync } from "node:child_process";
import { parseArgs } from "node:util";
import path from "node:path";
import process from "node:process";

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..");
const CARGO_ENV = { ...process.env, CARGO_NET_GIT_FETCH_WITH_CLI: "true" };

function run(name, bin, args, options = {}) {
  const started = Date.now();
  const r = spawnSync(bin, args, {
    cwd: options.cwd ?? ROOT,
    encoding: "utf8",
    shell: false,
    timeout: options.timeout_ms ?? 30 * 60 * 1000,
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
      run("rust:worktree-create-validate", "cargo", ["test", "-p", "r-code-host", "--lib", "git_service"], { env: CARGO_ENV }),
      run("binding:local-and-fail-closed", "cargo", ["test", "-p", "r-code-host", "--lib", "task_workspace_binding::tests::a2_local"], { env: CARGO_ENV }),
      run("flags:worktree-default-off", process.execPath, ["--test", "scripts/feature-flag-matrix.test.mjs"], { cwd: path.join(ROOT, "src-tauri", "frontend") }),
    ];
  },
  async a2() {
    return [
      run("rust:worktree-atomic-creation", "cargo", ["test", "-p", "r-code-host", "--lib", "git_service::tests"], { env: CARGO_ENV }),
    ];
  },
  async a3() {
    return [
      run("binding:fail-closed-regression", "cargo", ["test", "-p", "r-code-host", "--lib", "task_workspace_binding::tests::a3_"], { env: CARGO_ENV }),
    ];
  },
};

let args;
try {
  args = parseArgs({ options: { part: { type: "string" } }, strict: true });
} catch (error) {
  console.error(`用法: m6-01-checks.mjs --part a1|a2|a3\n${error.message}`);
  process.exit(2);
}

const runner = parts[(args.values.part ?? "").toLowerCase()];
if (!runner) {
  console.error(`未知 part: ${args.values.part}`);
  process.exit(2);
}
const results = await runner();
const allOk = results.every((r) => r.exit_code === 0 && !r.timed_out);
console.log(JSON.stringify({ schema_version: "m6-01-checks.v1", part: (args.values.part ?? "").toLowerCase(), ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
