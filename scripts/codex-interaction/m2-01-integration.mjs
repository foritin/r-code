#!/usr/bin/env node
// M2-01.A2 集成门禁：Rust e2e（命令输出生命周期 + 失败/退出码）与
// 前端模型（增量按序追加、终态权威覆盖）。

import { spawnSync } from "node:child_process";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const frontendDir = path.join(rootDir, "src-tauri", "frontend");
const node = process.execPath;

const commands = [
  {
    label: "rust e2e tool lifecycle",
    cmd: "cargo",
    args: ["test", "-p", "r-code-host", "--all-features", "--lib", "m2_01_a2", "--", "--test-threads=1"],
    cwd: rootDir,
    timeout: 20 * 60 * 1000,
  },
  {
    label: "frontend tool output accumulation",
    cmd: node,
    args: ["--test", "scripts/codex-tool-output.test.mjs", "--test-name-pattern", "m2_01_a2"],
    cwd: frontendDir,
    timeout: 10 * 60 * 1000,
  },
];

let failed = 0;
for (const command of commands) {
  console.log(`[m2-01-integration] ${command.label}: ${command.cmd} ${command.args.join(" ")}`);
  const result = spawnSync(command.cmd, command.args, {
    cwd: command.cwd,
    stdio: "inherit",
    timeout: command.timeout,
    maxBuffer: 32 * 1024 * 1024,
  });
  if (result.status !== 0) {
    console.error(`[m2-01-integration] FAILED: ${command.label} (exit ${result.status ?? "signal"})`);
    failed = result.status ?? 1;
  }
}
process.exitCode = failed;
