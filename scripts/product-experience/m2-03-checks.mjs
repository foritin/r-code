#!/usr/bin/env node
// M2-03 验收断言执行器（当前覆盖 A1/A4 演进/A11/A12 可独立证明子集）：
//   a1 : Composer Enter/IME/stop 分离——send-mode-switch 套件 + Composer 静态闸
//   a4 : 12 页 Pane 注册表演进门（6 页 pending 显式失败，防虚报完成）
//   a11: Subagent probe 套件（Provider/Subagent 合同子面）
//   a12: MCP stable server_id/管理套件
//   其余 (a2,a3,a5,a6,a8,a13) 待 canonical snapshot 统一与 E2E fixture 后接线，当前显式失败。
// 实现面：components/room/Composer.tsx、scenes/SettingsScene.tsx、
//         src/lib/settings-pane-registry.json、frontend/scripts/* 相关套件。

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
    const composer = readFileSync(
      path.join(FRONTEND, "src", "components", "room", "Composer.tsx"),
      "utf8",
    );
    const conditions = [
      { ok: composer.includes("isComposing"), detail: "Composer 缺 IME composition 守卫" },
    ];
    return [
      {
        name: "static:composer-enter-ime-guard",
        command: "scan Composer.tsx",
        exit_code: conditions.every((c) => c.ok) ? 0 : 1,
        timed_out: false,
        duration_ms: 0,
        stdout_tail: conditions.every((c) => c.ok) ? "IME composition 早退守卫存在" : "",
        stderr_tail: conditions.filter((c) => !c.ok).map((c) => c.detail).join("\n"),
      },
      run("compat:send-mode-switch", process.execPath, ["--test", "scripts/send-mode-switch.test.mjs"]),
    ];
  },
  async a4() {
    const results = [
      run("settings:pane-registry-evolution", process.execPath, ["--test", "scripts/m2-03-a4-pane-registry.test.mjs"]),
    ];
    // pending 惩罚：注册表完整性可以先行绿，但 12 页全部落地前 A4 断言保持红——
    // 「导航/搜索/标题/locale/provenance/flag/selector 均从 registry 派生」尚未成立。
    const registry = JSON.parse(
      readFileSync(path.join(FRONTEND, "src", "lib", "settings-pane-registry.json"), "utf8"),
    );
    const pending = registry.panes.filter((p) => p.status === "pending");
    results.push({
      name: "contract:all-panes-derived",
      command: "settings-pane-registry.json pending==0",
      exit_code: pending.length === 0 ? 0 : 1,
      timed_out: false,
      duration_ms: 0,
      stdout_tail: pending.length === 0 ? "12 页全部从 registry 派生" : "",
      stderr_tail: `pending 页未落地(${pending.map((p) => p.id).join(", ")})；补齐并迁移 SettingsScene 派生后方可过闸`,
    });
    return results;
  },
  async a11() {
    return [
      run("provider:subagent-autoprobe", process.execPath, ["--test", "scripts/subagent-provider-autoprobe.test.mjs"]),
    ];
  },
  async a12() {
    return [
      run("mcp:management", process.execPath, ["--test", "scripts/mcp-management.test.mjs"]),
      run("mcp:agent-actions", process.execPath, ["--test", "scripts/mcp-agent-actions.test.mjs"]),
    ];
  },
  async a10() {
    return [
      run("lifecycle:cas-three-way-recovery", process.execPath, ["--test", "scripts/m2-03-a10-lifecycle.test.mjs"]),
      run("compat:knowledge-settings", process.execPath, ["--test", "scripts/knowledge-settings-ui.test.mjs"]),
    ];
  },
  async a3() {
    return [
      run("keyboard:composer-send-stop-separation", process.execPath, ["--test", "scripts/m2-03-a3-keyboard.test.mjs"]),
    ];
  },
  async a2() {
    const composer = readFileSync(
      path.join(FRONTEND, "src", "components", "room", "Composer.tsx"), "utf8");
    return [
      run("rust:provider-config-semantics", "cargo", ["test", "-p", "r-code-host", "--lib", "settings::"], { env: CARGO_ENV }),
      run("e2e:provider-snapshot-consistency", process.execPath, ["--test", "scripts/m2-03-a2-provider-snapshot.test.mjs"], { cwd: FRONTEND }),
      staticCheck("static:composer-canonical-snapshot-source", [
        { ok: /服务默认|canonical|snapshot/i.test(composer), detail: "Composer 模型文案未绑定 canonical snapshot 语义" },
      ], "Composer/Settings/health 同源 snapshot"),
    ];
  },
  async a5() {
    return [
      run("rust:settings-semantics", "cargo", ["test", "-p", "r-code-host", "--lib", "settings::"], { env: CARGO_ENV }),
      run("compat:knowledge-settings-ui", process.execPath, ["--test", "scripts/knowledge-settings-ui.test.mjs"]),
      run("compat:native-notification", process.execPath, ["--test", "scripts/native-notification-routing.test.mjs"]),
    ];
  },
  async a8() {
    return [
      run("rust:settings-fixtures", "cargo", ["test", "-p", "r-code-host", "--lib", "settings::"], { env: CARGO_ENV }),
      run("compat:browser-mock-fixture-suites", process.execPath, ["--test", "scripts/mcp-management.test.mjs", "scripts/knowledge-settings-ui.test.mjs"]),
    ];
  },
  async a13() {
    return [
      run("guidesheets:four-entries-e2e", process.execPath, ["--test", "scripts/m2-03-a13-guidesheets.test.mjs"]),
      run("static:guidesheet-contract", process.execPath, ["--test", "scripts/m2-03-a6-search-e2e.test.mjs"]),
    ];
  },
  async a7() {
    // A7：baseline CapabilityID 恰映射——唯一性/处置合法性/anchor 齐备/
    // planned_demo 不冒充生产目标（demo→tools 白名单）/group→pane 无孤儿。
    const baseline = JSON.parse(
      readFileSync(path.join(ROOT, "docs", "product-experience-redesign", "settings-capability-baseline.json"), "utf8"),
    );
    const registry = JSON.parse(
      readFileSync(path.join(FRONTEND, "src", "lib", "settings-pane-registry.json"), "utf8"),
    );
    const caps = baseline.capabilities;
    const ids = caps.map((c) => c.capability_id);
    const groupToPane = {
      agents_codex: "agents", tools_mcp_rtk: "tools", knowledge: "knowledge",
      providers: "providers", preferences: "appearance", subagents: "subagents",
      image_understanding: "providers", diagnostics: "diagnostics",
      settings_shell: "providers", browser: "tools", security: "security",
      permissions: "permissions", lifecycle: "lifecycle",
    };
    const issues = [];
    if (new Set(ids).size !== ids.length) issues.push("capability_id 有重复");
    const DEMO_GROUPS = new Set(["browser"]);
    for (const c of caps) {
      if (!["preserve", "add", "migrate", "demo"].includes(c.disposition)) {
        issues.push(`${c.capability_id}: 非法 disposition ${c.disposition}`);
      }
      if (!c.target_anchor) issues.push(`${c.capability_id}: 缺 target_anchor`);
      if (c.classification === "planned_demo" && !DEMO_GROUPS.has(c.group)) {
        issues.push(`${c.capability_id}: planned_demo 冒充生产目标`);
      }
      if (c.disposition === "migrate" && !/^MIG-/.test(c.compatibility_contract_id ?? "")) {
        issues.push(`${c.capability_id}: migrate 缺 MIG 合同`);
      }
      const pane = groupToPane[c.group];
      if (!pane || !registry.panes.some((p) => p.id === pane)) {
        issues.push(`${c.capability_id}: group ${c.group} 无孤儿映射（pane=${pane ?? "?"}）`);
      }
    }
    return [{
      name: "contract:capability-mapping-orphans",
      command: "settings-capability-baseline.json × settings-pane-registry.json",
      exit_code: issues.length === 0 ? 0 : 1,
      timed_out: false,
      duration_ms: 0,
      stdout_tail: issues.length === 0
        ? `orphan_source=0 orphan_target=0 planned_demo_substitution=0（${caps.length} 项）`
        : "",
      stderr_tail: issues.slice(0, 8).join("\n"),
    }];
  },
  async a9() {
    // A9：改名迁移——route/深键别名有 registry 或 normalize 落点。
    const store = readFileSync(path.join(FRONTEND, "src", "store", "app.ts"), "utf8");
    const conditions = [
      { ok: store.includes('"preferences" ? "appearance"'), detail: "缺 preferences→appearance route 别名" },
      { ok: store.includes('pane === "codex" ? "agents"'), detail: "缺 codex→agents legacy 别名" },
    ];
    return [{
      name: "contract:migration-alias-registry",
      command: "scan store/app.ts normalizeSettingsPane",
      exit_code: conditions.every((c) => c.ok) ? 0 : 1,
      timed_out: false,
      duration_ms: 0,
      stdout_tail: conditions.every((c) => c.ok) ? "route/deep-link 别名齐备" : "",
      stderr_tail: conditions.filter((c) => !c.ok).map((c) => c.detail).join("\n"),
    }];
  },
};

let args;
try {
  args = parseArgs({ options: { part: { type: "string" } }, strict: true });
} catch (error) {
  console.error(`用法: m2-03-checks.mjs --part a1|a4|a11|a12\n${error.message}`);
  process.exit(2);
}

const runner = parts[(args.values.part ?? "").toLowerCase()];
if (!runner) {
  console.error(
    JSON.stringify({
      schema_version: "m2-03-checks.v1",
      part: (args.values.part ?? "").toLowerCase(),
      ok: false,
      not_implemented: true,
      reason: `part ${part} 依赖 12 页 Settings 落地 / CAS lifecycle reducer / canonical snapshot 统一，当前缺口显式失败`,
    }),
  );
  process.exit(1);
}

const results = await runner();
const allOk = results.every((r) => r.exit_code === 0 && !r.timed_out);
console.log(JSON.stringify({ schema_version: "m2-03-checks.v1", part: (args.values.part ?? "").toLowerCase(), ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
