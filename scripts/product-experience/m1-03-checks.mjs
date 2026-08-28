#!/usr/bin/env node
// M1-03 验收断言执行器（当前覆盖 A1/A2 共享投影内核）：
//   node scripts/product-experience/m1-03-checks.mjs --part a1   # §4.4 优先级全组合 + unread 独立
//   node scripts/product-experience/m1-02-checks 同构：a2          # 终态单调 + 父终态级联封口
//   --part a3|a4|a5 尚未实施（五消费面迁移/通知降级/静态 glyph 合同），显式 fail-fast。
// 每部分非交互、输出 JSON 结果、全部子命令 exit 0 才通过。

import { spawnSync } from "node:child_process";
import { parseArgs } from "node:util";
import path from "node:path";
import process from "node:process";

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..");
const FRONTEND = path.join(ROOT, "src-tauri", "frontend");

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
    // 注：task-status-view.test.mjs 属于 S3 五面迁移面（Playwright 型 UI 套件），
    // 迁移完成前不纳入 A1 内核，避免环境依赖伪失败。
    return [
      run(
        "projection:priority-unique-and-unread-independent",
        process.execPath,
        ["--test", "scripts/m1-03-a1-a2-projection.test.mjs"],
        { cwd: FRONTEND },
      ),
    ];
  },
  async a2() {
    // cascade/monotonic 用例包含在同一个投影套件（纯函数注入，无墙钟依赖）；
    // 与历史 automation 合同同跑一次防回归。
    const results = [
      run(
        "projection:terminal-monotonic-and-cascade",
        process.execPath,
        ["--test", "scripts/m1-03-a1-a2-projection.test.mjs"],
        { cwd: FRONTEND },
      ),
      run(
        "compat:automation-contract",
        process.execPath,
        ["--test", "scripts/automation-contract.test.mjs"],
        { cwd: FRONTEND },
      ),
    ];
    return results;
  },
  async a3() {
    return [
      run(
        "surfaces:five-surface-consistency",
        process.execPath,
        ["--test", "scripts/m1-03-s3-s5-consistency.test.mjs"],
        { cwd: FRONTEND },
      ),
    ];
  },
  async a4() {
    return [
      run(
        "notification:routing-degradation",
        process.execPath,
        ["--test", "scripts/native-notification-routing.test.mjs"],
        { cwd: FRONTEND },
      ),
      run(
        "notification:permission-and-fallback",
        process.execPath,
        ["--test", "scripts/native-notification.test.mjs"],
        { cwd: FRONTEND },
      ),
    ];
  },
  async a5() {
    // glyph/spinner 合同（spinning 恰为 running+verifying、12 态静态图形）
    // 由一致性套件承担；投影套件复核终态集合。
    return [
      run(
        "glyph:static-status-projection",
        process.execPath,
        ["--test", "scripts/m1-03-s3-s5-consistency.test.mjs"],
        { cwd: FRONTEND },
      ),
      run(
        "projection:terminal-set",
        process.execPath,
        ["--test", "scripts/m1-03-a1-a2-projection.test.mjs"],
        { cwd: FRONTEND },
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
  console.error(`用法: m1-03-checks.mjs --part a1|a2|a3|a4|a5\n${error.message}`);
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
console.log(JSON.stringify({ schema_version: "m1-03-checks.v1", part, ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
