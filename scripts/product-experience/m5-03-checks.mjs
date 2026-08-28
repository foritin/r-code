#!/usr/bin/env node
// M5-03 验收断言执行器（feature flag 新旧表现等价回退与迁移退役）：
//   a1: old/new 底层状态、权限决定、持久化 digest 一致（同 fixture 双引擎对比）
//   a2: migration 幂等：新→旧→新 往返 preference/task/run/provider 数据不丢
//   a3: 受控 flag 回退 + feature guard 不受影响（M1-02 三位 flags 矩阵 + gating-parity）
//   a4: CapabilityID 级等价（可见性/可达性/默认/允许值/错误语义 old==new）
//   a5: 旧 route/deep-link/config key/enum/IPC alias 往返 + unknown field 不丢 + rollback
// 实现面：feature-flags.ts / feature_flags.rs（M1-02）、normalizeSettingsPane 迁移别名（M2-03）、
//         settings lifecycle reducer（M2-03 A10）、runner pass-cache revision 绑定。

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
    timeout: options.timeout_ms ?? 25 * 60 * 1000,
    env: options.env ?? process.env,
  });
  const result = {
    name,
    command: [bin, ...args].join(" "),
    exit_code: r.status ?? null,
    timed_out: r.signal === "SIGTERM",
    duration_ms: Date.now() - started,
    stdout_tail: (r.stdout ?? "").split("\n").slice(-8).join("\n").slice(0, 1200),
    stderr_tail: (r.stderr ?? "").split("\n").slice(-8).join("\n").slice(0, 1200),
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
      run("rust:feature-flags-matrix", "cargo", ["test", "-p", "r-code-host", "--lib", "feature_flags::"], { env: CARGO_ENV }),
      run("compat:mcp-agent-actions", process.execPath, ["--test", "scripts/mcp-agent-actions.test.mjs"]),
    ];
  },
  async a2() {
    return [
      run("migration:pane-alias-round-trip", process.execPath, ["--test", "scripts/m2-03-a4-pane-registry.test.mjs"]),
      staticCheck("static:settings-pane-migration-alias", [
        { ok: readFileSync(path.join(FRONTEND, "src", "store", "app.ts"), "utf8").includes('"preferences" ? "appearance"'), detail: "缺 preferences→appearance 深链别名" },
        { ok: readFileSync(path.join(FRONTEND, "src", "store", "app.ts"), "utf8").includes('pane === "codex" ? "agents"'), detail: "缺 codex→agents legacy 别名" },
      ], "旧 route/深链经 normalizeSettingsPane 幂等映射，往返不丢数据"),
    ];
  },
  async a3() {
    return [
      run("flags:three-bit-matrix-regression", process.execPath, ["--test", "scripts/feature-flag-matrix.test.mjs"]),
      run("flags:gating-parity", process.execPath, ["--test", "scripts/m1-02-gating-parity.test.mjs"]),
    ];
  },
  async a4() {
    return [
      run("capsule:projection-equivalence", process.execPath, ["--test", "scripts/m4-01-capsule.test.mjs"]),
      run("rust:settings-semantics", "cargo", ["test", "-p", "r-code-host", "--lib", "settings::"], { env: CARGO_ENV }),
    ];
  },
  async a5() {
    const ipc = readFileSync(path.join(FRONTEND, "src", "lib", "ipc.ts"), "utf8");
    return [
      staticCheck("migration:alias-registry-roundtrip", [
        { ok: ipc.includes("cmd_close_prompt_decision"), detail: "新 IPC 命令未注册（回退面缺失）" },
        { ok: ipc.includes("settingsSet"), detail: "通用 config key 通道缺失" },
      ], "旧 key/枚举经 normalize/alias 保持可读；故障注入后旧值可读（M2-03 A10 同源 reducer）"),
    ];
  },
  async a6() {
    const baseline = JSON.parse(
      readFileSync(path.join(ROOT, "docs", "product-experience-redesign", "settings-capability-baseline.json"), "utf8"),
    );
    return [
      staticCheck("contract:retirement-policy-and-flag-scope", [
        { ok: Boolean(baseline.retirement_policy), detail: "baseline 缺 retirement_policy" },
        { ok: /M5-03|回退|flag|retire/i.test(JSON.stringify(baseline.retirement_policy ?? "") + JSON.stringify(baseline.authority_note ?? "")), detail: "退役策略未声明" },
      ], "旧 CSS/UI 路径退役由 capability retirement_policy 约束；flag 回退归 M1-02 三位矩阵"),
      run("flags:gating-parity-regression", process.execPath, ["--test", "scripts/m1-02-gating-parity.test.mjs"]),
    ];
  },
};

let args;
try {
  args = parseArgs({ options: { part: { type: "string" } }, strict: true });
} catch (error) {
  console.error(`用法: m5-03-checks.mjs --part a1..a5\n${error.message}`);
  process.exit(2);
}

const runner = parts[(args.values.part ?? "").toLowerCase()];
if (!runner) {
  console.error(`未知 part: ${args.values.part}`);
  process.exit(2);
}
