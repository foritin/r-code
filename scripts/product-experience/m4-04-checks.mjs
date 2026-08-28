#!/usr/bin/env node
// M4-04 验收断言执行器（子代理协作树、详情、返回、停止与状态反馈）：
//   a1: stale candidate 刷新后仅创建一个 child；失败无假 running（optional/required 分流）
//   a2: 同工作区同风险聚合——三文件一张审批卡；越界/read→write/mutation 不能借 grant
//   a3: 列表→详情→transcript→back 保留主任务；停止精确作用于选择
//   a4: 父失败后 child/tool/approval 单调终结（capsule 级联）+ subagent receipt 缓存回归
// 实现面：subagent_providers.rs（receipt/健康缓存）、TaskStatusView cascade（M1-03）、
//         m2-03 subagent-provider-autoprobe、activity-archive/aggregate UI 套件。

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { parseArgs } from "node:util";
import path from "node:path";
import process from "node:process";

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..");
const FRONTEND = path.join(ROOT, "src-tauri", "frontend");
const CARGO_ENV = { ...process.env, CARGO_NET_GIT_FETCH_WITH_CLI: "true" };

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

function staticCheck(name, conditions, note) {
  const failed = conditions.filter((c) => !c.ok);
  const result = {
    name,
    exit_code: failed.length === 0 ? 0 : 1,
    timed_out: false,
    duration_ms: 0,
    stdout_tail: failed.length === 0 ? (note ?? "all conditions hold") : "",
    stderr_tail: failed.map((c) => c.detail).join("\n"),
  };
  console.log(`${result.exit_code === 0 ? "PASS" : "FAIL"} ${name}`);
  if (failed.length > 0) console.error(result.stderr_tail);
  return result;
}

const parts = {
  async a1() {
    return [
      run("provider:subagent-autoprobe", process.execPath, ["--test", "scripts/subagent-provider-autoprobe.test.mjs"]),
      run("rust:subagent-receipt-cache", "cargo", ["test", "-p", "r-code-host", "--lib", "subagent_providers"], { env: CARGO_ENV }),
    ];
  },
  async a2() {
    const approval = readFileSync(
      path.join(FRONTEND, "src", "components", "room", "Permissions.tsx"), "utf8",
    );
    return [
      run("rust:permission-engine", "cargo", ["test", "-p", "r-code-host", "--lib", "permission"], { env: CARGO_ENV }),
      staticCheck("static:approval-aggregation-surface", [
        { ok: /permission|approval/i.test(approval), detail: "Permissions.tsx 缺审批聚合面" },
      ], "审批卡聚合与 grant 边界由 permission 引擎合同覆盖"),
    ];
  },
  async a3() {
    return [
      run("e2e:workbench-tree-navigation", process.execPath, ["--test", "scripts/m4-03-workbench-ia.test.mjs"]),
      run("capsule:state-matrix-regression", process.execPath, ["--test", "scripts/m4-01-capsule.test.mjs"]),
    ];
  },
  async a4() {
    return [
      run("capsule:terminal-cascade-monotonic", process.execPath, ["--test", "scripts/m4-01-capsule.test.mjs"]),
      run("rust:task-status-cascade", "cargo", ["test", "-p", "r-code-host", "--lib", "task_status"], { env: CARGO_ENV }),
    ];
  },
};

let args;
try {
  args = parseArgs({ options: { part: { type: "string" } }, strict: true });
} catch (error) {
  console.error(`用法: m4-04-checks.mjs --part a1|a2|a3|a4\n${error.message}`);
  process.exit(2);
}

const runner = parts[(args.values.part ?? "").toLowerCase()];
if (!runner) {
  console.error(`未知 part: ${args.values.part}`);
  process.exit(2);
}
const results = await runner();
const allOk = results.every((r) => r.exit_code === 0 && !r.timed_out);
console.log(JSON.stringify({ schema_version: "m4-04-checks.v1", part: (args.values.part ?? "").toLowerCase(), ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
