#!/usr/bin/env node
// M3-04 验收断言执行器（全局连接健康 UI + 设置页探测迁移）：
//   a1: 消费面单源 + provider snapshot 一致性 E2E
//   a2: readiness 单飞/重试去重（provider_readiness）+ probe 失败可操作
//   a3: 非颜色语义视图合同 + E2E
//   a4: TTL fresh 零请求 + 设置页切页零额外探测（静态：pane 切换不触发 probe invoke）
// 实现面：src/lib/provider-health.ts、provider_readiness.rs、SettingsScene providerStatus。

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
    const scene = readFileSync(path.join(FRONTEND, "src", "components", "scenes", "SettingsScene.tsx"), "utf8");
    return [
      run("e2e:provider-snapshot-consistency", process.execPath, ["--test", "scripts/m2-03-a2-provider-snapshot.test.mjs"]),
      staticCheck("static:single-provider-status-source", [
        { ok: scene.includes("providerStatus"), detail: "Settings 未消费 provider 快照" },
        { ok: !/providerStatus\s*=\s*new Map/.test(scene), detail: "出现第二套 provider 状态存储" },
      ], "Provider mini/Shell/Composer/Settings/health 消费同一 canonical snapshot"),
    ];
  },
  async a2() {
    return [
      run("rust:readiness-single-flight-and-retry-dedup", "cargo", ["test", "-p", "r-code-host", "--lib", "provider_readiness::"], { env: CARGO_ENV }),
      run("e2e:probe-failure-still-operable", process.execPath, ["--test", "scripts/m2-03-a6-search-e2e.test.mjs"]),
    ];
  },
  async a3() {
    return [
      run("health:non-color-semantics", process.execPath, ["--test", "scripts/m3-04-provider-health.test.mjs"]),
    ];
  },
  async a4() {
    const scene = readFileSync(path.join(FRONTEND, "src", "components", "scenes", "SettingsScene.tsx"), "utf8");
    // 切页路径不得直接调用探测 IPC（探测只在手动重试/启动 policy 下发生）
    const paneSwitchProbe = /activePane === .*probe|setSettingsPane[\s\S]{0,120}probeProvider/.test(scene);
    return [
      run("rust:readiness-fresh-ttl", "cargo", ["test", "-p", "r-code-host", "--lib", "provider_readiness::"], { env: CARGO_ENV }),
      staticCheck("static:no-probe-on-pane-switch", [
        { ok: !paneSwitchProbe, detail: "切页即探测（违反 TTL fresh 零请求）" },
      ], "TTL fresh 内请求数保持 0"),
    ];
  },
};

let args;
try {
  args = parseArgs({ options: { part: { type: "string" } }, strict: true });
} catch (error) {
  console.error(`用法: m3-04-checks.mjs --part a1|a2|a3|a4\n${error.message}`);
  process.exit(2);
}

const runner = parts[(args.values.part ?? "").toLowerCase()];
if (!runner) {
  console.error(`未知 part: ${args.values.part}`);
  process.exit(2);
}
const results = await runner();
const allOk = results.every((r) => r.exit_code === 0 && !r.timed_out);
console.log(JSON.stringify({ schema_version: "m3-04-checks.v1", part: (args.values.part ?? "").toLowerCase(), ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
