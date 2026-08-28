#!/usr/bin/env node
// M4-03 验收断言执行器（执行台概览/子代理/变更）：
//   a1: 一级 tab 集合恰为 overview/subagents/changes，无重复全局工具审计
//   a2: 自动聚焦规则 + 用户手动保持
//   a3: 窗口 bounds 不变 + Run 状态单调（capsule 单测回归）
// 实现面：src/lib/workbench-ia.ts + scripts/m4-03-workbench-ia.test.mjs + m2-02 窗口静态扫描。

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { parseArgs } from "node:util";
import path from "node:path";
import process from "node:process";

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..");
const FRONTEND = path.join(ROOT, "src-tauri", "frontend");

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
    const workbench = readFileSync(path.join(FRONTEND, "src", "components", "room", "SubagentWorkbench.tsx"), "utf8");
    return [
      run("ia:tab-set-and-autofocus", process.execPath, ["--test", "scripts/m4-03-workbench-ia.test.mjs"]),
      staticCheck("static:no-duplicate-global-tool-audit", [
        { ok: !/工具审计/.test(workbench), detail: "执行台残留全局工具审计入口" },
      ], "一级 IA 仅 overview/subagents/changes；工具详情回对应 Run/child"),
    ];
  },
  async a2() {
    return [
      run("ia:autofocus-and-user-override", process.execPath, ["--test", "scripts/m4-03-workbench-ia.test.mjs"]),
    ];
  },
  async a3() {
    const main = readFileSync(path.join(ROOT, "src-tauri", "src", "main.rs"), "utf8");
    const windowApiInWorkbench = /setSize|setPosition|LogicalSize|outerPosition/.test(
      readFileSync(path.join(FRONTEND, "src", "components", "room", "SubagentWorkbench.tsx"), "utf8"),
    );
    return [
      staticCheck("static:window-bounds-frozen", [
        { ok: !windowApiInWorkbench, detail: "执行台组件含窗口几何 API" },
        { ok: main.includes("close-prompt-request"), detail: "Host 关闭状态机缺席（回归哨兵）" },
      ], "开关/切 tab/抽屉不改顶层窗口几何；Run 状态经 capsule 单测回归（M4-01）"),
    ];
  },
};

let args;
try {
  args = parseArgs({ options: { part: { type: "string" } }, strict: true });
} catch (error) {
  console.error(`用法: m4-03-checks.mjs --part a1|a2|a3\n${error.message}`);
  process.exit(2);
}

const runner = parts[(args.values.part ?? "").toLowerCase()];
if (!runner) {
  console.error(`未知 part: ${args.values.part}`);
  process.exit(2);
}
const results = await runner();
const allOk = results.every((r) => r.exit_code === 0 && !r.timed_out);
console.log(JSON.stringify({ schema_version: "m4-03-checks.v1", part: (args.values.part ?? "").toLowerCase(), ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
