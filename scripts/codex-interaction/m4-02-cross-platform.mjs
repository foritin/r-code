#!/usr/bin/env node
// M4-02.A1 跨平台门禁：共用 contract 断言在本平台全绿 + 归一化层与
// 共用 e2e 无平台 cfg 分叉 + CI 三平台矩阵覆盖这些测试。
// macOS/Linux 的执行由 CI（cargo test --workspace 三平台）承担；本脚本
// 静态验证“同一套断言、无平台语义分叉”，不伪造远端结果。

import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");

function fail(message) {
  console.error(`[m4-02-cross-platform] FAILED: ${message}`);
  process.exitCode = 1;
}

// 1) 共用 contract 套件（纯逻辑 + Node fixture e2e，平台无关）本机全绿。
const suite = spawnSync(
  "cargo",
  ["test", "-p", "r-code-host", "--all-features", "--lib", "codex_interaction", "--", "--test-threads=1"],
  { cwd: rootDir, stdio: "inherit", timeout: 20 * 60 * 1000, maxBuffer: 32 * 1024 * 1024 },
);
if (suite.status !== 0) fail(`shared contract suite exited ${suite.status ?? "signal"}`);

const e2e = spawnSync(
  "cargo",
  ["test", "-p", "r-code-host", "--all-features", "--lib", "m1_01 m2_01 m2_02 m3_01 m3_03 m4_02", "--", "--test-threads=1"],
  { cwd: rootDir, stdio: "inherit", timeout: 30 * 60 * 1000, maxBuffer: 32 * 1024 * 1024 },
);
if (e2e.status !== 0) fail(`shared e2e suite exited ${e2e.status ?? "signal"}`);

// 2) 归一化层与共用测试无平台条件编译：事件语义不得分叉。
for (const file of [
  "src-tauri/src/codex_interaction.rs",
  "src-tauri/src/codex_interaction_tests.rs",
]) {
  const content = fs.readFileSync(path.join(rootDir, file), "utf8");
  if (/#\[cfg\((windows|unix|target_os)/.test(content)) {
    fail(`${file} contains a platform cfg fork in shared event semantics`);
  }
}
const commands = fs.readFileSync(path.join(rootDir, "src-tauri/src/commands.rs"), "utf8");
const sharedTests = commands.slice(commands.indexOf("async fn m1_01_a1_"));
for (const test of ["m1_01_a1_", "m2_01_a2_", "m2_02_a1_", "m3_01_a1_", "m3_03_a1_", "m4_02_a2_"]) {
  const start = sharedTests.indexOf(`async fn ${test}`);
  if (start < 0) continue;
  const preceding = sharedTests.slice(Math.max(0, start - 120), start);
  if (/#\[cfg\(/.test(preceding)) {
    fail(`shared e2e ${test} is platform-gated`);
  }
}

// 3) CI 矩阵包含三平台的 workspace 测试（共用断言随 cargo test 运行）。
const ci = fs.readFileSync(path.join(rootDir, ".github/workflows/ci.yml"), "utf8");
if (!/os:\s*\[ubuntu-latest,\s*macos-latest,\s*windows-latest\]/.test(ci)) {
  fail("ci.yml must run the workspace tests on ubuntu/macos/windows");
}
if (!ci.includes("cargo test --workspace --all-features")) {
  fail("ci.yml must run cargo test --workspace --all-features");
}

if (process.exitCode == null || process.exitCode === 0) {
  console.log(
    "[m4-02-cross-platform] shared assertions green locally, no platform forks, CI matrix covers ubuntu/macos/windows"
  );
}
