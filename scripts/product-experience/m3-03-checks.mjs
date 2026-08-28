#!/usr/bin/env node
// M3-03 验收断言执行器（Host 非阻塞 Provider readiness service）：
//   a1: fresh TTL 零请求 + 既有 provider/provider_catalog/subagent 回归
//   a2: 单飞 + generation 失效 + receipt CAS
//   a3: 候选池刷新（委派前）
//   a4: probe 记录零凭据 + 证据卫生
// 实现面：src-tauri/src/provider_readiness.rs（ReadinessStore：FreshSkip/单飞/permit≤2/generation 失效）、
//         subagent_providers.rs receipt 摘要、provider_catalog.rs。

import { spawnSync } from "node:child_process";
import { parseArgs } from "node:util";
import path from "node:path";
import process from "node:process";

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..");
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
  console.log(`${result.exit_code === 0 && !r.timed_out ? "PASS" : "FAIL"} ${name} (${result.duration_ms}ms)`);
  if (result.exit_code !== 0) {
    console.error((result.stderr_tail || result.stdout_tail || "").split("\n").slice(0, 6).join("\n"));
  }
  return result;
}

const parts = {
  async a1() {
    return [
      run("rust:readiness-fresh-ttl-zero-request", "cargo", ["test", "-p", "r-code-host", "--lib", "provider_readiness::"], { env: CARGO_ENV }),
      run("rust:provider-catalog-regression", "cargo", ["test", "-p", "r-code-host", "--lib", "provider_catalog"], { env: CARGO_ENV }),
    ];
  },
  async a2() {
    return [
      run("rust:readiness-single-flight-and-generation", "cargo", ["test", "-p", "r-code-host", "--lib", "provider_readiness::"], { env: CARGO_ENV }),
      run("rust:subagent-receipt-cache", "cargo", ["test", "-p", "r-code-host", "--lib", "subagent_providers"], { env: CARGO_ENV }),
    ];
  },
  async a3() {
    return [
      run("rust:subagent-pool-refresh", "cargo", ["test", "-p", "r-code-host", "--lib", "subagent_providers"], { env: CARGO_ENV }),
    ];
  },
  async a4() {
    return [
      run("rust:readiness-no-credential-fields", "cargo", ["test", "-p", "r-code-host", "--lib", "provider_readiness::tests::a4_"], { env: CARGO_ENV }),
      run("security:evidence-hygiene", process.execPath, [`${ROOT}/scripts/product-experience/evidence-hygiene.mjs`]),
    ];
  },
};

let args;
try {
  args = parseArgs({ options: { part: { type: "string" } }, strict: true });
} catch (error) {
  console.error(`用法: m3-03-checks.mjs --part a1|a2|a3|a4\n${error.message}`);
  process.exit(2);
}

const runner = parts[(args.values.part ?? "").toLowerCase()];
if (!runner) {
  console.error(`未知 part: ${args.values.part}`);
  process.exit(2);
}
const results = await runner();
const allOk = results.every((r) => r.exit_code === 0 && !r.timed_out);
console.log(JSON.stringify({ schema_version: "m3-03-checks.v1", part: (args.values.part ?? "").toLowerCase(), ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
