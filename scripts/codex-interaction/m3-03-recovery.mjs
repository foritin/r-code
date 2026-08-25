#!/usr/bin/env node
// M3-03.A3 恢复门禁：Rust 标记合并（终态覆盖 pending）+ 前端历史重建
// （run 存活保留可答 pending；终止后 expired 只读）。

import { spawnSync } from "node:child_process";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const frontendDir = path.join(rootDir, "src-tauri", "frontend");
const node = process.execPath;

const commands = [
  {
    label: "rust pending-question marker merge",
    cmd: "cargo",
    args: ["test", "-p", "r-code-host", "--all-features", "--lib", "m3_03_a3", "--", "--test-threads=1"],
    cwd: rootDir,
    timeout: 20 * 60 * 1000,
  },
  {
    label: "frontend rebuild pending/expired",
    cmd: node,
    args: ["--test", "scripts/codex-question-card.test.mjs", "--test-name-pattern", "m3_03_a3"],
    cwd: frontendDir,
    timeout: 10 * 60 * 1000,
  },
];

let failed = 0;
for (const command of commands) {
  console.log(`[m3-03-recovery] ${command.label}: ${command.cmd} ${command.args.join(" ")}`);
  const result = spawnSync(command.cmd, command.args, {
    cwd: command.cwd,
    stdio: "inherit",
    timeout: command.timeout,
    maxBuffer: 32 * 1024 * 1024,
  });
  if (result.status !== 0) {
    console.error(`[m3-03-recovery] FAILED: ${command.label} (exit ${result.status ?? "signal"})`);
    failed = result.status ?? 1;
  }
}
process.exitCode = failed;
