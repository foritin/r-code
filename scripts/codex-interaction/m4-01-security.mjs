#!/usr/bin/env node
// M4-01.A1 安全负面门禁：投影层 oracle（raw reasoning/secret/凭据 0 泄露）
// + 持久化 oracle（e2e：secret 答案不进会话 JSONL，事件流不携带值）。

import { spawnSync } from "node:child_process";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");

const commands = [
  {
    label: "projection security oracle",
    args: ["test", "-p", "r-code-host", "--all-features", "--lib", "m4_01_a1", "--", "--test-threads=1"],
  },
  {
    label: "persistence security oracle (e2e secret scan)",
    args: ["test", "-p", "r-code-host", "--all-features", "--lib", "m3_01_a2", "--", "--test-threads=1"],
  },
];

let failed = 0;
for (const command of commands) {
  console.log(`[m4-01-security] ${command.label}: cargo ${command.args.join(" ")}`);
  const result = spawnSync("cargo", command.args, {
    cwd: rootDir,
    stdio: "inherit",
    timeout: 20 * 60 * 1000,
    maxBuffer: 32 * 1024 * 1024,
  });
  if (result.status !== 0) {
    console.error(`[m4-01-security] FAILED: ${command.label} (exit ${result.status ?? "signal"})`);
    failed = result.status ?? 1;
  }
}
process.exitCode = failed;
