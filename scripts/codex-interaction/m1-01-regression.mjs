#!/usr/bin/env node
// M1-01.A3 回归门禁：既有 Codex final 消息路径（host）与原生 R-Code
// 流式消息路径（agent-worker）在 agentMessage 投影改造后保持通过。

import { spawnSync } from "node:child_process";
import process from "node:process";

const commands = [
  {
    label: "host codex app-server suite",
    args: ["test", "-p", "r-code-host", "--all-features", "--lib", "codex_app_server", "--", "--test-threads=1"],
    timeout: 20 * 60 * 1000,
  },
  {
    label: "native r-code streaming message test",
    args: ["test", "-p", "r-code-agent-worker", "--all-features", "text_delta_produces_message_events", "--", "--exact"],
    timeout: 20 * 60 * 1000,
  },
  {
    label: "subagent message sealing tests",
    args: ["test", "-p", "r-code-agent-worker", "--all-features", "message", "--", "--test-threads=1"],
    timeout: 20 * 60 * 1000,
  },
];

let failed = 0;
for (const command of commands) {
  console.log(`[m1-01-regression] ${command.label}: cargo ${command.args.join(" ")}`);
  const result = spawnSync("cargo", command.args, {
    stdio: "inherit",
    timeout: command.timeout,
    maxBuffer: 32 * 1024 * 1024,
  });
  if (result.status !== 0) {
    console.error(`[m1-01-regression] FAILED: ${command.label} (exit ${result.status ?? "signal"})`);
    failed = result.status ?? 1;
  }
}
if (failed !== 0) {
  process.exitCode = failed;
} else {
  console.log("[m1-01-regression] all regression suites passed");
}
