#!/usr/bin/env node
// M9-03 验收断言执行器：故障矩阵确定性 + 敏感 oracle/权限绕过/越界写零容忍
// + `--through M9 --profile implementation` 同 revision required 全绿。

import { spawnSync } from "node:child_process";
import { parseArgs } from "node:util";
import path from "node:path";
import process from "node:process";

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..");
const CARGO_ENV = { ...process.env, CARGO_NET_GIT_FETCH_WITH_CLI: "true" };

function run(name, bin, args, options = {}) {
  const started = Date.now();
  const r = spawnSync(bin, args, { cwd: options.cwd ?? ROOT, encoding: "utf8", shell: false,
    timeout: options.timeout_ms ?? 60 * 60 * 1000, env: options.env ?? process.env });
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
    run("rust:fault-matrix-regression", "cargo", ["test", "-p", "r-code-host", "--lib", "close_gate::"], { env: CARGO_ENV }),
    run("rust:binding-fault-matrix", "cargo", ["test", "-p", "r-code-host", "--lib", "task_workspace_binding"], { env: CARGO_ENV }),
  ],
  a2: () => [
    run("security:evidence-hygiene", process.execPath, [path.join(ROOT, "scripts/product-experience/evidence-hygiene.mjs")]),
    run("security:debug-detail-containment", process.execPath, ["--test", "scripts/debug-detail-containment.test.mjs"], { cwd: path.join(ROOT, "src-tauri", "frontend") }),
  ],
  a4: () => {
    // 累计门不能直接跑 --through M9：内部会再次执行 M9-03.A4 自身（失败断言无缓存）→ 无限递归直至超时。
    // 等价拆解：through-M8 全量累计门 + 其余 M9 任务门；顶层最终验收另跑 --through M9 闭环。
    const gateEnv = { ...process.env, CARGO_NET_GIT_FETCH_WITH_CLI: "true", VERIFY_PRODUCT_EXPERIENCE_PYTHON: process.env.VERIFY_PRODUCT_EXPERIENCE_PYTHON ?? "python3" };
    const gate = (name, extra) => run(name, process.execPath,
      [path.join(ROOT, "scripts/verify-product-experience.mjs"), ...extra, "--profile", "implementation"],
      { env: gateEnv, timeout_ms: 60 * 60 * 1000 });
    return [
      gate("gate:through-M8", ["--through", "M8"]),
      gate("gate:M9-01", ["--task", "M9-01"]),
      gate("gate:M9-02", ["--task", "M9-02"]),
      gate("gate:M9-04", ["--task", "M9-04"]),
    ];
  },
};

let args;
try {
  args = parseArgs({ options: { part: { type: "string" } }, strict: true });
} catch (error) {
  console.error(error.message);
  process.exit(2);
}
const runner = parts[(args.values.part ?? "").toLowerCase()];
if (!runner) { console.error(`未知 part: ${args.values.part}`); process.exit(2); }
const results = await runner();
const allOk = results.every((r) => r.exit_code === 0 && !r.timed_out);
console.log(JSON.stringify({ schema_version: "m9-03-checks.v1", part: (args.values.part ?? "").toLowerCase(), ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
