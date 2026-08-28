#!/usr/bin/env node
// M3-01 验收断言执行器（Host 关闭状态机）：
//   a1: 等价触发 fixture——三触发源产生同一 CloseIntent/状态序列（cargo close_gate::）
//   a2: 重入/竞态——重复 close 只聚焦；stale/重复决定被拒（cargo gate 测试子集）
//   a3: 偏好迁移幂等、默认安全 ask（迁移表 + lifecycle.toml 服务）
//   a4: restore=none 时 hide 永不执行（单测）+ Host 统一入口静态锁定
// 实现面：src-tauri/src/close_gate.rs（纯核心+持久化）、main.rs（统一 close 臂+prompt 事件+命令注册）、
//         lifecycle_commands.rs、frontend ClosePromptDialog/LifecycleSection。

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { parseArgs } from "node:util";
import path from "node:path";
import process from "node:process";

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..");
const HOST_SRC = path.join(ROOT, "src-tauri", "src");
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
      run("rust:close-gate-equivalent-triggers", "cargo", ["test", "-p", "r-code-host", "--lib", "close_gate::tests::a1_"], { env: CARGO_ENV }),
    ];
  },
  async a2() {
    return [
      run("rust:close-gate-reentrancy", "cargo", ["test", "-p", "r-code-host", "--lib", "close_gate::tests::a2_"], { env: CARGO_ENV }),
    ];
  },
  async a3() {
    return [
      run("rust:close-preference-migration", "cargo", ["test", "-p", "r-code-host", "--lib", "close_gate::"], { env: CARGO_ENV }),
    ];
  },
  async a4() {
    const conditions = [];
    const main = readFileSync(path.join(HOST_SRC, "main.rs"), "utf8");
    conditions.push({
      ok: main.includes("close-prompt-request") && main.includes("CloseGate"),
      detail: "main.rs 未接入统一 Host 关闭状态机/prompt 事件",
    });
    const ipc = readFileSync(path.join(ROOT, "src-tauri", "frontend", "src", "lib", "ipc.ts"), "utf8");
    conditions.push({
      ok: ipc.includes("cmd_close_prompt_decision"),
      detail: "前端缺少 prompt 决定命令桥",
    });
    const dialog = readFileSync(
      path.join(ROOT, "src-tauri", "frontend", "src", "components", "shell", "ClosePromptDialog.tsx"),
      "utf8",
    );
    conditions.push({
      ok: dialog.includes("aria-modal") && dialog.includes('decide("cancel")'),
      detail: "关闭对话框缺 modal/Esc-cancel 语义",
    });
    return [
      run("rust:close-gate-hide-restore-capability", "cargo", ["test", "-p", "r-code-host", "--lib", "close_gate::tests::a4_"], { env: CARGO_ENV }),
      staticCheck("static:host-unified-close-entry", conditions,
        "restore=none 时 hide 永不执行；Host 单入口 + 前端桥接齐备"),
    ];
  },
};

let args;
try {
  args = parseArgs({ options: { part: { type: "string" } }, strict: true });
} catch (error) {
  console.error(`用法: m3-01-checks.mjs --part a1|a2|a3|a4\n${error.message}`);
  process.exit(2);
}

const runner = parts[(args.values.part ?? "").toLowerCase()];
if (!runner) {
  console.error(`未知 part: ${args.values.part}`);
  process.exit(2);
}
const results = await runner();
const allOk = results.every((r) => r.exit_code === 0 && !r.timed_out);
console.log(JSON.stringify({ schema_version: "m3-01-checks.v1", part: (args.values.part ?? "").toLowerCase(), ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
