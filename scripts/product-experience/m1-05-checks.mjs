#!/usr/bin/env node
// M1-05 验收断言执行器（Browser/Automation 公共合同冻结）：
//   a1: Rust/TS/fixture round-trip——browser-contract 与 automation-contract 套件 + host browser:: 测试
//   a2: 权限不互相提升——Browse/Interact capability 分离 + read-only 过滤 + automation 注册期门
//   a3: feature disabled 时 schema 仍可读、入口与执行被拒（模块层闸 + 契约可解析）
// 实现面：src-tauri/src/browser/{tools/catalog.rs,tools/request.rs,commands.rs,tool_gateway.rs}、
//         src-tauri/src/automation/mod.rs、frontend/scripts/{browser,automation}-contract.test.mjs

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { parseArgs } from "node:util";
import path from "node:path";
import process from "node:process";

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..");
const FRONTEND = path.join(ROOT, "src-tauri", "frontend");
const HOST = path.join(ROOT, "src-tauri", "src");
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

function staticCheck(name, conditions, filesNote) {
  const failed = conditions.filter((c) => !c.ok);
  const result = {
    name,
    command: filesNote,
    exit_code: failed.length === 0 ? 0 : 1,
    timed_out: false,
    duration_ms: 0,
    stdout_tail: failed.length === 0 ? "all conditions hold" : "",
    stderr_tail: failed.map((c) => c.detail).join("\n"),
  };
  console.log(`${result.exit_code === 0 ? "PASS" : "FAIL"} ${name}`);
  if (failed.length > 0) console.error(result.stderr_tail);
  return result;
}

const parts = {
  async a1() {
    return [
      run(
        "ts:browser-contract-roundtrip",
        process.execPath,
        ["--test", "scripts/browser-contract.test.mjs"],
        { cwd: FRONTEND },
      ),
      run(
        "ts:automation-contract-roundtrip",
        process.execPath,
        ["--test", "scripts/automation-contract.test.mjs"],
        { cwd: FRONTEND },
      ),
      run("rust:browser-contract-domain", "cargo", ["test", "-p", "r-code-host", "--lib", "browser::"], { env: CARGO_ENV }),
    ];
  },
  async a2() {
    const catalog = readFileSync(path.join(HOST, "browser", "tools", "catalog.rs"), "utf8");
    const automationMod = readFileSync(path.join(HOST, "automation", "mod.rs"), "utf8");
    const request = readFileSync(path.join(HOST, "browser", "tools", "request.rs"), "utf8");
    return [
      staticCheck(
        "static:capability-separation-and-unknown-hardening",
        [
          { ok: catalog.includes("BrowserPermissionCapability::Browse"), detail: "catalog 缺 Browse capability" },
          { ok: catalog.includes("BrowserPermissionCapability::Interact"), detail: "catalog 缺 Interact capability" },
          { ok: /fn is_read_only|is_read_only\(/.test(catalog), detail: "catalog 缺 read-only 判定" },
          { ok: request.includes("deny_unknown_fields"), detail: "request DTO 未拒绝未知字段" },
          { ok: automationMod.includes("require_feature_enabled"), detail: "automation 注册期缺 feature 门" },
        ],
        "browser/tools/{catalog,request}.rs + automation/mod.rs",
      ),
    ];
  },
  async a3() {
    const browserCommands = readFileSync(path.join(HOST, "browser", "commands.rs"), "utf8");
    const contractStart = browserCommands.indexOf("pub fn browser_agent_contract");
    const gated = /require\(ProductFeature::Browser\)/.test(
      browserCommands.slice(contractStart, contractStart + 700),
    );
    return [
      staticCheck(
        "static:disabled-feature-rejects-execution",
        [
          { ok: gated, detail: "browser contract 入口未接 feature 闸" },
          { ok: browserCommands.includes("browser.feature_disabled"), detail: "缺结构化禁用错误码" },
        ],
        "browser/commands.rs（schema 可读由 a1 round-trip 覆盖；disabled 时入口拒绝在此锁定）",
      ),
    ];
  },
};

let args;
try {
  args = parseArgs({ options: { part: { type: "string" } }, strict: true });
} catch (error) {
  console.error(`用法: m1-05-checks.mjs --part a1|a2|a3\n${error.message}`);
  process.exit(2);
}

const runner = parts[(args.values.part ?? "").toLowerCase()];
if (!runner) {
  console.error(`未知 part: ${args.values.part}`);
  process.exit(2);
}

const results = await runner();
const allOk = results.every((r) => r.exit_code === 0 && !r.timed_out);
console.log(JSON.stringify({ schema_version: "m1-05-checks.v1", part: (args.values.part ?? "").toLowerCase(), ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
