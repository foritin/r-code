#!/usr/bin/env node
// M1-02.A3 回归门禁：原生 R-Code 与 Codex 两条主代理路径共享同一进度
// 合同语义（r-code-core 单一事实源），且不相互覆盖、不漂移。

import { spawnSync } from "node:child_process";
import process from "node:process";

const commands = [
  {
    label: "core shared contract fixture",
    args: ["test", "-p", "r-code-core", "m1_02_a3", "--", "--test-threads=1"],
    timeout: 10 * 60 * 1000,
  },
  {
    label: "native workspace prompt embeds shared contract",
    args: ["test", "-p", "r-code-agent-worker", "--all-features", "m1_02_a3", "--", "--test-threads=1"],
    timeout: 20 * 60 * 1000,
  },
  {
    label: "codex subagent concise-delivery boundary",
    args: ["test", "-p", "r-code-host", "--all-features", "--lib", "m1_02_a3", "--", "--test-threads=1"],
    timeout: 20 * 60 * 1000,
  },
];

let failed = 0;
for (const command of commands) {
  console.log(`[m1-02-regression] ${command.label}: cargo ${command.args.join(" ")}`);
  const result = spawnSync("cargo", command.args, {
    stdio: "inherit",
    timeout: command.timeout,
    maxBuffer: 32 * 1024 * 1024,
  });
  if (result.status !== 0) {
    console.error(`[m1-02-regression] FAILED: ${command.label} (exit ${result.status ?? "signal"})`);
    failed = result.status ?? 1;
  }
}
if (failed !== 0) {
  process.exitCode = failed;
} else {
  console.log("[m1-02-regression] shared progress contract verified on both agent paths");
}
