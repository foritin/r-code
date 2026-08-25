#!/usr/bin/env node
// M4-02.A4 production 门禁（--profile production 才会运行）：
// 真实 Codex 登录与目标平台安装包冒烟。外部条件不满足时如实报告
// external_pending（非零退出 + JSON 报告），绝不伪造 passed。

import { spawnSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const reportPath = path.join(
  rootDir,
  "artifacts",
  "ai-tasks",
  "verification",
  "codex-rich-interaction",
  "production",
  "M4-02-production-gate.json"
);

const checks = [];

// 1) 真实 Codex CLI 在场且已登录（不触发交互）。
const version = spawnSync("codex", ["--version"], { encoding: "utf8", timeout: 30_000 });
const cliPresent = version.status === 0;
checks.push({
  id: "real_codex_cli",
  status: cliPresent ? "available" : "missing",
  detail: cliPresent ? version.stdout.trim() : "codex CLI not found on PATH",
});
let loggedIn = false;
if (cliPresent) {
  const loginStatus = spawnSync("codex", ["login", "status"], { encoding: "utf8", timeout: 30_000 });
  loggedIn = loginStatus.status === 0 && !/not logged in/i.test(loginStatus.stdout ?? "");
  checks.push({
    id: "real_codex_login",
    status: loggedIn ? "available" : "external_pending",
    detail: (loginStatus.stdout ?? loginStatus.stderr ?? "").trim().slice(0, 200),
  });
} else {
  checks.push({ id: "real_codex_login", status: "external_pending", detail: "cli missing" });
}

// 2) 安装包冒烟：本仓库不在此环境构建安装包；如实报告 pending。
checks.push({
  id: "windows_installer_smoke",
  status: "external_pending",
  detail: "requires a built Windows installer; not produced in this gate",
});
checks.push({
  id: "macos_installer_smoke",
  status: "external_pending",
  detail: "requires a macOS host with signing/notarization environment",
});

const pending = checks.filter((check) => check.status === "external_pending");
const report = {
  schema_version: "codex-interaction-production-gate.v1",
  generated_at: new Date().toISOString(),
  platform: { node: process.version, os: process.platform },
  verdict: pending.length === 0 ? "passed" : "external_pending",
  checks,
};
mkdirSync(path.dirname(reportPath), { recursive: true });
writeFileSync(reportPath, `${JSON.stringify(report, null, 2)}\n`);

if (pending.length > 0) {
  console.error(`production gate: external_pending (${pending.map((check) => check.id).join(", ")})`);
  console.error(`report: ${path.relative(rootDir, reportPath)}`);
  process.exitCode = 5;
} else {
  console.log("production gate: passed");
}
