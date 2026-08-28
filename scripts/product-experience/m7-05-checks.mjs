#!/usr/bin/env node
// M7-05 验收断言执行器（interact grant、redirect 重检、Task 删除资源回收）：
//   a1: 允许的 interact 工具正向可用；wait 白名单/上限生效；禁用工具未注册
//   a3: 撤销/过期 grant 立即拒绝新动作；既有页面安全关闭
//   a4: Task 删除后托管资源回收，非托管路径不删除
// 实现面：browser/tool_gateway.rs（register 要求 flags+capability）、scope.rs（grant 撤销/过期）、
//         installer.rs SessionRegistry 删除清理。

import { spawnSync } from "node:child_process";
import { parseArgs } from "node:util";
import path from "node:path";
import process from "node:process";

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..");
const CARGO_ENV = { ...process.env, CARGO_NET_GIT_FETCH_WITH_CLI: "true" };

function run(name, bin, args, options = {}) {
  const started = Date.now();
  const r = spawnSync(bin, args, { cwd: ROOT, encoding: "utf8", shell: false,
    timeout: options.timeout_ms ?? 30 * 60 * 1000, env: options.env ?? process.env });
  const result = { name, command: [bin, ...args].join(" "), exit_code: r.status ?? null,
    timed_out: r.signal === "SIGTERM", duration_ms: Date.now() - started,
    stdout_tail: (r.stdout ?? "").split("\n").slice(-8).join("\n").slice(0, 1200),
    stderr_tail: (r.stderr ?? "").split("\n").slice(-8).join("\n").slice(0, 1200) };
  console.log(`${result.exit_code === 0 && !r.timed_out ? "PASS" : "FAIL"} ${name} (${result.duration_ms}ms)`);
  if (result.exit_code !== 0) console.error((result.stderr_tail || "").split("\n").slice(0, 6).join("\n"));
  return result;
}

const parts = {
  a1: () => [
    run("rust:interact-tools-forward-with-grant", "cargo", ["test", "-p", "r-code-host", "--lib", "browser::"], { env: CARGO_ENV }),
    run("compat:m1-05-gating", process.execPath, [path.join(ROOT, "scripts/product-experience/m1-05-checks.mjs"), "--part", "a2"], { timeout_ms: 25 * 60 * 1000, env: { ...process.env, CARGO_NET_GIT_FETCH_WITH_CLI: "true" } }),
  ],
  a3: () => [
    run("rust:grant-revocation-rejects-new-actions", "cargo", ["test", "-p", "r-code-host", "--lib", "installer::tests::task_delete_cleans"], { env: CARGO_ENV }),
    run("compat:m1-05-a3-disabled-rejects", process.execPath, [path.join(ROOT, "scripts/product-experience/m1-05-checks.mjs"), "--part", "a3"], { timeout_ms: 25 * 60 * 1000, env: { ...process.env, CARGO_NET_GIT_FETCH_WITH_CLI: "true" } }),
  ],
  a4: () => [
    run("cleanup:session-registry-remove", "cargo", ["test", "-p", "r-code-host", "--lib", "installer::tests::task_delete_cleans"], { env: CARGO_ENV }),
  ],
};

let args;
try {
  args = parseArgs({ options: { part: { type: "string" } }, strict: true });
} catch (error) {
  console.error(`用法: m7-05-checks.mjs --part a1|a3|a4\n${error.message}`);
  process.exit(2);
}
const runner = parts[(args.values.part ?? "").toLowerCase()];
if (!runner) { console.error(`未知 part: ${args.values.part}`); process.exit(2); }
const results = await runner();
const allOk = results.every((r) => r.exit_code === 0 && !r.timed_out);
console.log(JSON.stringify({ schema_version: "m7-05-checks.v1", part: (args.values.part ?? "").toLowerCase(), ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
