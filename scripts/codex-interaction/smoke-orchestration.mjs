#!/usr/bin/env node
// M0-01.A3 断言命令：用真实 pipeline 编排 ≥1 个 Rust 测试与 ≥1 个前端
// 测试（registry 任务 M0-01-smoke），并验证产物 JSON 索引存在、无 secret、
// 包含 revision 与 fixture schema 版本。首次运行 cargo 编译可能需要数分钟。

import { spawnSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import path from "node:path";
import process from "node:process";

const rootDir = process.cwd();

function fail(message) {
  console.error(`smoke-orchestration FAILED: ${message}`);
  process.exitCode = 1;
}

const child = spawnSync(
  process.execPath,
  ["scripts/verify-codex-interaction.mjs", "--task", "M0-01-smoke", "--profile", "implementation"],
  { cwd: rootDir, encoding: "utf8", timeout: 45 * 60 * 1000, maxBuffer: 32 * 1024 * 1024 },
);

if (child.status !== 0) {
  console.error(`nested harness run exited ${child.status}`);
  if (child.stdout) {
    process.stdout.write(child.stdout);
  }
  if (child.stderr) {
    process.stderr.write(child.stderr);
  }
  process.exitCode = child.status ?? 1;
}

const reportPath = path.join(
  rootDir,
  "artifacts",
  "ai-tasks",
  "verification",
  "codex-rich-interaction",
  "implementation",
  "M0-01-smoke.json",
);

let report;
try {
  report = JSON.parse(await readFile(reportPath, "utf8"));
} catch (error) {
  fail(`cannot read smoke report ${reportPath}: ${error.message}`);
}

const problems = [];
const byId = new Map(report.assertions.map((a) => [a.id, a]));
const rust = byId.get("M0-01-smoke.R1");
const frontend = byId.get("M0-01-smoke.R2");
if (!rust || rust.status !== "passed" || !rust.command?.includes("cargo")) {
  problems.push(`rust orchestration assertion not passed: ${JSON.stringify(rust?.status)}`);
}
if (!frontend || frontend.status !== "passed" || !frontend.command?.some((part) => part.includes("run-tests.mjs"))) {
  problems.push(`frontend orchestration assertion not passed: ${JSON.stringify(frontend?.status)}`);
}
if (!report.revision?.git_revision) {
  problems.push("report missing revision.git_revision");
}
if (!report.fixture_schema_version) {
  problems.push("report missing fixture_schema_version");
}
const reportText = JSON.stringify(report);
if (/sk-[A-Za-z0-9_-]{6,}|bearer\s+[A-Za-z0-9._-]{6,}/i.test(reportText)) {
  problems.push("credential-like content leaked into report");
}
if (report.assertions.some((a) => typeof a.output_sha256 !== "string" || a.output_sha256.length !== 64)) {
  problems.push("assertion missing output_sha256 (stdout must be digested, never stored)");
}

if (problems.length > 0) {
  for (const problem of problems) {
    fail(problem);
  }
}

if (child.status === 0 && problems.length === 0) {
  console.log(
    `smoke-orchestration passed: rust=${rust.exit_code} frontend=${frontend.exit_code}, report=${path.relative(rootDir, reportPath)} (fixture schema ${report.fixture_schema_version})`,
  );
}
