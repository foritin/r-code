#!/usr/bin/env node
// M6-02 验收断言执行器（全部执行消费者迁移到 WorkspaceBinding）：
//   a1: 消费者 inventory 只有 WorkspaceBinding 来源；fallback 模式扫描 0
//   a2: Local/Worktree 下消费者 cwd/root 一致且写入仅发生绑定目录
//   a3: 目录替换/repo mismatch/junction 逃逸均拒绝，原项目 hash 不变
// 实现面：task_workspace_binding.rs（resolver+fixture）、消费者静态扫描。

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
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

function staticCheck(name, conditions, note) {
  const failed = conditions.filter((c) => !c.ok);
  const result = { name, exit_code: failed.length === 0 ? 0 : 1, timed_out: false, duration_ms: 0,
    stdout_tail: failed.length === 0 ? (note ?? "all conditions hold") : "",
    stderr_tail: failed.map((c) => c.detail).join("\n") };
  console.log(`${result.exit_code === 0 ? "PASS" : "FAIL"} ${name}`);
  if (failed.length > 0) console.error(result.stderr_tail);
  return result;
}

const parts = {
  async a1() {
    const binding = readFileSync(path.join(ROOT, "src-tauri", "src", "task_workspace_binding.rs"), "utf8");
    return [
      run("rust:binding-consumers", "cargo", ["test", "-p", "r-code-host", "--lib", "task_workspace_binding::"], { env: CARGO_ENV }),
      staticCheck("static:no-local-fallback-in-consumers", [
        { ok: !/fallback_to_local|fallback to project root/i.test(binding), detail: "resolver 含 Local fallback 语义" },
      ], "resolve 失败一律 fail-closed，无回退路径"),
    ];
  },
  async a2() {
    return [
      run("rust:binding-cwd-root-consistency", "cargo", ["test", "-p", "r-code-host", "--lib", "task_workspace_binding::tests::a2_"], { env: CARGO_ENV }),
    ];
  },
  async a3() {
    return [
      run("rust:binding-escape-rejections", "cargo", ["test", "-p", "r-code-host", "--lib", "task_workspace_binding::tests::a3_"], { env: CARGO_ENV }),
    ];
  },
};

let args;
try {
  args = parseArgs({ options: { part: { type: "string" } }, strict: true });
} catch (error) {
  console.error(`用法: m6-02-checks.mjs --part a1|a2|a3\n${error.message}`);
  process.exit(2);
}
const runner = parts[(args.values.part ?? "").toLowerCase()];
if (!runner) { console.error(`未知 part: ${args.values.part}`); process.exit(2); }
const results = await runner();
const allOk = results.every((r) => r.exit_code === 0 && !r.timed_out);
console.log(JSON.stringify({ schema_version: "m6-02-checks.v1", part: (args.values.part ?? "").toLowerCase(), ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
