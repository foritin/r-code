#!/usr/bin/env node
// M0-01.A2 的被编排对象：跑 ≥1 Rust 契约测试 + ≥1 前端测试子集，输出 schema 合法 JSON。
// 由 verify-product-experience.mjs 编排执行；自身也必须非交互、可独立运行。
// Rust 侧通过 CARGO_NET_GIT_FETCH_WITH_CLI=true 走系统 git（CI 与沙箱内均如此）。

import { spawnSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import path from "node:path";
import process from "node:process";

const ROOT = process.cwd();
const FRONTEND_SUBSET = [
  ["frontend:release-quality-gate", "node", ["--test", "scripts/release-quality-gate.test.mjs"]],
  ["frontend:icon-assets", "node", ["--test", "scripts/icon-assets.test.mjs"]],
];
const RUST_CONTRACT = [
  "rust:r-code-mcp-lib",
  "cargo",
  ["test", "-p", "r-code-mcp", "--lib"],
];

function runPart(name, bin, args, extraEnv = {}) {
  const started = Date.now();
  const r = spawnSync(bin, args, {
    cwd: ROOT,
    encoding: "utf8",
    timeout: 35 * 60 * 1000,
    env: { ...process.env, ...extraEnv },
  });
  return {
    name,
    command: [bin, ...args].join(" "),
    exit_code: r.status ?? null,
    timed_out: r.error?.code === "ABORT_ERR" || r.signal === "SIGTERM",
    duration_ms: Date.now() - started,
    stdout_tail: (r.stdout ?? "").split("\n").slice(-6).join("\n").slice(0, 2000),
    stderr_tail: (r.stderr ?? "").split("\n").slice(-6).join("\n").slice(0, 2000),
  };
}

const parts = [
  ...FRONTEND_SUBSET.map(([name, bin, args]) => runPart(name, bin, args)),
  runPart(...RUST_CONTRACT, {}),
];

const report = {
  schema_version: "product-experience-bootstrap.v1",
  profile: "implementation",
  platform: { os: process.platform, node: process.version },
  revision: spawnSync("git", ["rev-parse", "HEAD"], { cwd: ROOT, encoding: "utf8" }).stdout.trim(),
  ok: parts.every((p) => p.exit_code === 0 && !p.timed_out),
  parts,
};

const out = process.argv[2] ?? path.join(ROOT, "artifacts", "ai-tasks", "verification", "product-experience-gap-closure", "implementation", "bootstrap-suite.json");
if (!process.argv[2]) {
  const { mkdirSync } = await import("node:fs");
  mkdirSync(path.dirname(out), { recursive: true });
}
writeFileSync(out, JSON.stringify(report, null, 2) + "\n", "utf8");

for (const p of parts) {
  console.log(`${p.exit_code === 0 && !p.timed_out ? "PASS" : "FAIL"} ${p.name} (${p.duration_ms}ms)`);
}
console.log(`report: ${path.relative(ROOT, out)} ok=${report.ok}`);
process.exitCode = report.ok ? 0 : 1;
