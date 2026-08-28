#!/usr/bin/env node
// M4-02 验收断言执行器（Timeline commentary/final/轨迹/Attention）：
//   a1: raw reasoning/secret/未脱敏工具输出 oracle 零命中（DOM 静态扫描 + 脱敏单测 + debug-detail 套件）
//   a2: 折叠语义——普通工具默认折叠摘要，失败/审批/提问/final 恒可见（capsule fold 合同）
//   a3: 展开后事件顺序/终态/安全摘要 live/history 一致（capsule replay + codex-message-stream）
//   a4: 10,000 delta 不产生万级 DOM 节点、可见刷新限频且 final 完整（timeline-incremental-performance）

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
    const timeline = readFileSync(path.join(FRONTEND, "src", "components", "room", "Timeline.tsx"), "utf8");
    const toolActivity = readFileSync(path.join(FRONTEND, "src", "components", "room", "tool-activity.ts"), "utf8");
    return [
      run("security:debug-detail-containment", process.execPath, ["--test", "scripts/debug-detail-containment.test.mjs"]),
      run("capsule:sanitize-latest-update", process.execPath, ["--test", "scripts/m4-01-capsule.test.mjs"]),
      staticCheck("static:timeline-no-reasoning-render", [
        { ok: !/reasoning_content|raw_reasoning|thinking_text/.test(timeline + toolActivity), detail: "Timeline 出现 raw reasoning 渲染路径" },
        { ok: !/innerHTML\s*=/.test(timeline), detail: "Timeline 使用 innerHTML（注入面）" },
      ], "raw reasoning/secret 零渲染面"),
    ];
  },
  async a2() {
    return [
      run("capsule:fold-visibility-contract", process.execPath, ["--test", "scripts/m4-01-capsule.test.mjs"]),
    ];
  },
  async a3() {
    return [
      run("capsule:live-replay-consistency", process.execPath, ["--test", "scripts/m4-01-capsule.test.mjs"]),
      run("compat:codex-message-stream", process.execPath, ["--test", "scripts/codex-message-stream.test.mjs"]),
    ];
  },
  async a4() {
    return [
      run("perf:timeline-incremental", process.execPath, ["--test", "scripts/timeline-incremental-performance.test.mjs"], { timeout_ms: 30 * 60 * 1000 }),
    ];
  },
};

let args;
try {
  args = parseArgs({ options: { part: { type: "string" } }, strict: true });
} catch (error) {
  console.error(`用法: m4-02-checks.mjs --part a1|a2|a3|a4\n${error.message}`);
  process.exit(2);
}

const runner = parts[(args.values.part ?? "").toLowerCase()];
if (!runner) {
  console.error(`未知 part: ${args.values.part}`);
  process.exit(2);
}
const results = await runner();
const allOk = results.every((r) => r.exit_code === 0 && !r.timed_out);
console.log(JSON.stringify({ schema_version: "m4-02-checks.v1", part: (args.values.part ?? "").toLowerCase(), ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
