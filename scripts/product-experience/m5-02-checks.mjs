#!/usr/bin/env node
// M5-02 验收断言执行器（体验 E2E/性能/视觉/隐私 总聚合门）：
//   a1: 关键 E2E 链（关闭确认/搜索深链/四 GuideSheet/通知降级）
//   a2: 性能（timeline 万级 delta 有界 + readiness 单飞/TTL + capsule 级联）
//   a3: 视觉（三视口×亮暗溢出矩阵 + day 材质黑影扫描 + 对比度门）
//   a4: security-negative（evidence-hygiene + debug-detail + capsule 脱敏）
//   a5: §5.8 闭环（provider 健康/关闭确认/planned-demo 拒绝面）
//   a6: 12 Pane registry 与路由/搜索/locale 派生一一对应
//   a7: 真实导航入口到达全部 baseline CapabilityID（SettingsScene 全 12 页渲染）
//   a8: 可写能力矩阵（保存/Host 拒绝保持旧值/CAS 恢复）+ Provider snapshot + 动作状态
//   a9: capability coverage 四零门（missing/unexecuted/unexpected_noop/prototype_only）
//   a10: 12 页统一 Settings lifecycle 全矩阵 + 三路 recovery
//   a11: 合同束（Provider/Subagent/MCP/GuideSheet/terminal ACK）+ 篡改负例

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { parseArgs } from "node:util";
import path from "node:path";
import process from "node:process";

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..");
const FRONTEND = path.join(ROOT, "src-tauri", "frontend");
const CARGO_ENV = { ...process.env, CARGO_NET_GIT_FETCH_WITH_CLI: "true" };
const PY = process.env.VERIFY_PRODUCT_EXPERIENCE_PYTHON ?? "python3";

function run(name, bin, args, options = {}) {
  const started = Date.now();
  const r = spawnSync(bin, args, {
    cwd: options.cwd ?? FRONTEND,
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
    stdout_tail: (r.stdout ?? "").split("\n").slice(-8).join("\n").slice(0, 1200),
    stderr_tail: (r.stderr ?? "").split("\n").slice(-8).join("\n").slice(0, 1200),
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
      run("e2e:search-deeplink-narrow-return", process.execPath, ["--test", "scripts/m2-03-a6-search-e2e.test.mjs"]),
      run("e2e:guidesheets-four-entries", process.execPath, ["--test", "scripts/m2-03-a13-guidesheets.test.mjs"]),
      run("e2e:notification-routing-degradation", process.execPath, ["--test", "scripts/native-notification-routing.test.mjs"]),
    ];
  },
  async a2() {
    return [
      run("perf:timeline-10k-delta-bounded", process.execPath, ["--test", "scripts/timeline-incremental-performance.test.mjs"], { timeout_ms: 30 * 60 * 1000 }),
      run("perf:readiness-single-flight", "cargo", ["test", "-p", "r-code-host", "--lib", "provider_readiness::"], { env: { ...process.env, CARGO_NET_GIT_FETCH_WITH_CLI: "true" }, cwd: ROOT }),
      run("perf:cascade-bounded", process.execPath, ["--test", "scripts/m4-01-capsule.test.mjs"]),
    ];
  },
  async a3() {
    return [
      run("visual:viewport-theme-overflow-matrix", process.execPath, ["--test", "scripts/m2-04-theme-responsive.test.mjs"]),
      run("visual:day-material-black-shadow-scan", process.execPath, [`${ROOT}/scripts/product-experience/m2-01-checks.mjs`, "--part", "a4"]),
      run("visual:contrast-gate", process.execPath, [`${ROOT}/scripts/product-experience/m2-01-checks.mjs`, "--part", "a2"]),
    ];
  },
  async a4() {
    return [
      run("security:evidence-hygiene", process.execPath, [`${ROOT}/scripts/product-experience/evidence-hygiene.mjs`], { cwd: ROOT }),
      run("security:debug-detail-containment", process.execPath, ["--test", "scripts/debug-detail-containment.test.mjs"]),
      run("security:capsule-sanitize", process.execPath, ["--test", "scripts/m4-01-capsule.test.mjs"]),
    ];
  },
  async a5() {
    return [
      run("e2e:provider-health-non-color", process.execPath, ["--test", "scripts/m3-04-provider-health.test.mjs"]),
      run("e2e:mcp-agent-actions", process.execPath, ["--test", "scripts/mcp-agent-actions.test.mjs"]),
      run("e2e:close-prompt-contract", process.execPath, ["--test", "scripts/m2-04-theme-responsive.test.mjs"]),
    ];
  },
  async a6() {
    return [
      run("contract:pane-registry-derivation", process.execPath, ["--test", "scripts/m2-03-a4-pane-registry.test.mjs"]),
    ];
  },
  async a7() {
    const scene = readFileSync(path.join(FRONTEND, "src", "components", "scenes", "SettingsScene.tsx"), "utf8");
    const panes = ["providers", "agents", "subagents", "tools", "knowledge", "permissions",
      "security", "appearance", "notifications", "lifecycle", "updates", "diagnostics"];
    const missing = panes.filter((p) => !scene.includes(`key: "${p}"`));
    return [
      static_missing_check(missing),
      run("e2e:capability-navigation-matrix", process.execPath, ["--test", "scripts/m2-03-a6-search-e2e.test.mjs"]),
    ];
    function static_missing_check(missing) {
      return {
        name: "static:all-12-panes-navigable",
        command: "scan SettingsScene SETTINGS_PANES",
        exit_code: missing.length === 0 ? 0 : 1,
        timed_out: false,
        duration_ms: 0,
        stdout_tail: missing.length === 0 ? "12/12 pane 导航入口齐备" : "",
        stderr_tail: missing.length ? `缺 pane: ${missing.join(",")}` : "",
      };
    }
  },
  async a8() {
    return [
      run("writable:save-reject-cas-recovery", process.execPath, ["--test", "scripts/m2-03-a10-lifecycle.test.mjs"]),
      run("writable:provider-snapshot-default-ack", process.execPath, ["--test", "scripts/m2-03-a2-provider-snapshot.test.mjs"]),
      run("writable:knowledge-pane-save-flow", process.execPath, ["--test", "scripts/knowledge-settings-ui.test.mjs"]),
      run("actions:mcp-management-states", process.execPath, ["--test", "scripts/mcp-management.test.mjs"]),
    ];
  },
  async a9() {
    return [
      run("coverage:capability-four-zero-gate", PY, [path.join(ROOT, "docs/product-experience-redesign/tools/settings_capability_gate.py"), "--check"], { cwd: ROOT }),
    ];
  },
  async a10() {
    return [
      run("lifecycle:settings-12-pane-matrix", process.execPath, ["--test", "scripts/m2-03-a10-lifecycle.test.mjs"]),
    ];
  },
  async a11() {
    return [
      run("contract:subagent-provider-authority", process.execPath, ["--test", "scripts/subagent-provider-autoprobe.test.mjs"]),
      run("contract:mcp-server-id-tamper-negative", process.execPath, ["--test", "scripts/mcp-agent-actions.test.mjs"]),
      run("contract:guidesheets-four-entries", process.execPath, ["--test", "scripts/m2-03-a13-guidesheets.test.mjs"]),
      run("contract:codex-tool-terminate-lifecycle", process.execPath, ["--test", "scripts/subagent-tool-lifecycle.test.mjs"]),
      run("contract:terminal-ack-cascade", process.execPath, ["--test", "scripts/m4-01-capsule.test.mjs"]),
    ];
  },
};

let args;
try {
  args = parseArgs({ options: { part: { type: "string" } }, strict: true });
} catch (error) {
  console.error(`用法: m5-02-checks.mjs --part a1..a11\n${error.message}`);
  process.exit(2);
}

const runner = parts[(args.values.part ?? "").toLowerCase()];
if (!runner) {
  console.error(`未知 part: ${args.values.part}`);
  process.exit(2);
}
const results = await runner();
const allOk = results.every((r) => r.exit_code === 0 && !r.timed_out);
console.log(JSON.stringify({ schema_version: "m5-02-checks.v1", part: (args.values.part ?? "").toLowerCase(), ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
