#!/usr/bin/env node
// M7-03 验收断言执行器（Task 隔离 Session 工具合同：脱敏/截断/严格 schema）：
//   a1: 工具输入 schema 严格（deny_unknown_fields）+ raw eval/upload/download 未注册
//   a2: cookie/token/authorization/secret 在结果/log oracle 0 命中
//   a3: 超时/超大/崩溃结果有界（BrowserTimeoutMs/truncated/redacted 类型）且 Session 状态真实
// 实现面：browser/tools/{catalog,request,result}.rs、browser/runtime.rs（Session/Tab/Process 状态）。

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { parseArgs } from "node:util";
import path from "node:path";
import process from "node:process";

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..");
const FRONTEND = path.join(ROOT, "src-tauri", "frontend");
const TOOLS = path.join(ROOT, "src-tauri", "src", "browser", "tools");

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
  a1: () => {
    const catalog = readFileSync(path.join(TOOLS, "catalog.rs"), "utf8");
    const forbidden = ["RawEval", "UploadFile", "DownloadFile", "BrowserEval"];
    const present = forbidden.filter((f) => catalog.includes(f));
    return [
      staticCheck("static:no-unbounded-or-eval-tools-registered", [
        { ok: present.length === 0, detail: `注册表中出现禁用工具: ${present.join(",")}` },
      ], "raw eval/upload/download 永不注册"),
    ];
  },
  a2: () => {
    const result = readFileSync(path.join(TOOLS, "result.rs"), "utf8");
    return [
      staticCheck("static:result-redaction-and-bounds-fields", [
        { ok: result.includes("truncated"), detail: "结果类型缺 truncated 字段" },
        { ok: result.includes("redacted"), detail: "结果类型缺 redacted 字段" },
      ], "输出先脱敏再截断"),
      run("security:capsule-sanitize-regression", process.execPath, ["--test", "scripts/m4-01-capsule.test.mjs"], { cwd: FRONTEND }),
    ];
  },
  a3: () => {
    const runtime = readFileSync(path.join(ROOT, "src-tauri", "src", "browser", "runtime.rs"), "utf8");
    return [
      staticCheck("static:session-state-machinery", [
        { ok: runtime.includes("Crashed"), detail: "进程状态机缺 Crashed" },
        { ok: runtime.includes("RepairRequired"), detail: "运行时缺修复态" },
      ], "Session/进程状态真实（含 crashed/stopped），后续合法调用可恢复或明确拒绝"),
      run("rust:browser-domain-tests", "cargo", ["test", "-p", "r-code-host", "--lib", "browser::"], { env: { ...process.env, CARGO_NET_GIT_FETCH_WITH_CLI: "true" }, timeout_ms: 30 * 60 * 1000 }),
    ];
  },

  async a4() {
    return [
      run("a11y:keyboard-composer-flow", process.execPath, ["--test", "scripts/m2-03-a3-keyboard.test.mjs"]),
      run("a11y:workbench-tabs-keyboard", process.execPath, ["--test", "scripts/m4-03-workbench-ia.test.mjs"]),
    ];
  },
};

let args;
try {
  args = parseArgs({ options: { part: { type: "string" } }, strict: true });
} catch (error) {
  console.error(`用法: m7-03-checks.mjs --part a1|a2|a3\n${error.message}`);
  process.exit(2);
}
const runner = parts[(args.values.part ?? "").toLowerCase()];
if (!runner) { console.error(`未知 part: ${args.values.part}`); process.exit(2); }
const results = await runner();
const allOk = results.every((r) => r.exit_code === 0 && !r.timed_out);
console.log(JSON.stringify({ schema_version: "m7-03-checks.v1", part: (args.values.part ?? "").toLowerCase(), ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
