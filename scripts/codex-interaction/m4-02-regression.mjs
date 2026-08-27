#!/usr/bin/env node
// M4-02.A3 回归 + 文档门禁：受影响 workspace crate 回归、前端全量套件、
// 文档一致性（新增章节引用的文件/脚本真实存在 + PRD 唯一完成状态与
// evidence 文件一致）。

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const frontendDir = path.join(rootDir, "src-tauri", "frontend");

let failed = 0;
const run = (label, cmd, args, cwd, timeout) => {
  console.log(`[m4-02-regression] ${label}: ${cmd} ${args.join(" ")}`);
  const result = spawnSync(cmd, args, { cwd, stdio: "inherit", timeout, maxBuffer: 32 * 1024 * 1024 });
  if (result.status !== 0) {
    console.error(`[m4-02-regression] FAILED: ${label} (exit ${result.status ?? "signal"})`);
    failed = result.status ?? 1;
  }
};

// 1) 受影响 workspace crate 回归（core 合同 + worker 提示注入）。
run(
  "core + agent-worker regression",
  "cargo",
  ["test", "-p", "r-code-core", "-p", "r-code-agent-worker", "--all-features", "--", "--test-threads=1"],
  rootDir,
  40 * 60 * 1000
);

// 2) 前端全量套件（直调内部 runner，避开 npm shim 在 spawnSync 下的信号问题）。
run("frontend full suite", process.execPath, ["scripts/run-tests.mjs"], frontendDir, 40 * 60 * 1000);

// 3) 文档一致性：架构文档引用的脚本与 fixture 存在；evidence 文件与 §9 对齐。
const problems = [];
const architecture = fs.readFileSync(path.join(rootDir, "docs/architecture.md"), "utf8");
for (const referenced of [
  "src-tauri/src/codex_interaction.rs",
  "fixtures/codex-interaction/protocol-0.145.0.json",
  "scripts/codex-interaction/extract-protocol-fixture.mjs",
  "scripts/codex-interaction/check-protocol-fixture.mjs",
  "scripts/verify-codex-interaction.mjs",
]) {
  if (!fs.existsSync(path.join(rootDir, referenced))) {
    problems.push(`architecture.md references missing file: ${referenced}`);
  }
}
if (!architecture.includes("### 7.1 Codex 丰富交互")) {
  problems.push("architecture.md must document the rich-interaction layer (§7.1)");
}
const prd = fs.readFileSync(
  path.join(rootDir, "docs/support/contracts/codex-rich-interaction-prd.md"),
  "utf8",
);
const checked = [...prd.matchAll(/^- \[x\] \*\*(M\d-\d\d)\*\*/gm)].map((match) => match[1]);
for (const taskId of checked) {
  if (!fs.existsSync(path.join(rootDir, `artifacts/ai-tasks/evidence/codex-rich-interaction/${taskId}.yaml`))) {
    problems.push(`§9 checked task ${taskId} has no evidence file`);
  }
}
if (problems.length > 0) {
  for (const problem of problems) console.error(`[m4-02-regression] ${problem}`);
  failed = failed || 1;
}

process.exitCode = failed;
