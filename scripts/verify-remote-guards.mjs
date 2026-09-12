#!/usr/bin/env node
// verify-remote-guards.mjs — R14 安全守卫：四项安全不变量 + 文档结构校验。
// 非交互；全绿退出 0，任何失败退出 1。CI 直接调用（无真实网络依赖）。
//
// 四安全守卫（对应 worklist R14.A1）：
//   1. 默认无口      — runtime 网络监听收口于 remote/listener.rs（F2）
//   2. 配对开关      — 无设备/开关关时 listener 拒绝启动（R04.A1 语义）
//   3. 证书钉扎      — 错指纹在 TLS 握手层失败（R03.A2 语义）
//   4. 能力矩阵      — forbidden 方法全拒 + 缺能力拒绝（R04.A3/R11 语义）
// 文档守卫（R14.A2）：remote-control.md 必备小节齐全，无 TBD/TODO。

import { execFileSync } from "node:child_process";
import { readFileSync, existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const failures = [];

function cargoTest(testName, filter) {
  try {
    execFileSync(
      "cargo",
      ["test", "-p", "r-code-runtime", "--test", testName, filter, "--", "--exact"],
      { cwd: root, stdio: "pipe", timeout: 10 * 60 * 1000, encoding: "utf8" },
    );
    return true;
  } catch (error) {
    console.error(`guard failed (${testName}::${filter}):`);
    console.error(String(error.stdout || error.message).slice(-1500));
    return false;
  }
}

function libTest(filter) {
  try {
    execFileSync(
      "cargo",
      ["test", "-p", "r-code-runtime", "--lib", filter, "--", "--exact"],
      { cwd: root, stdio: "pipe", timeout: 10 * 60 * 1000, encoding: "utf8" },
    );
    return true;
  } catch (error) {
    console.error(`guard failed (lib::${filter}):`);
    console.error(String(error.stdout || error.message).slice(-1500));
    return false;
  }
}

// 1. 默认无口：除 remote/listener.rs 外无 TcpListener。
const listenerSource = join(root, "crates/r-code-runtime/src/remote/listener.rs");
const guard1 = existsSync(listenerSource);
if (guard1) {
  const { readdirSync } = await import("node:fs");
  const offenders = [];
  const stack = [join(root, "crates/r-code-runtime/src")];
  while (stack.length > 0) {
    const dir = stack.pop();
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const path = join(dir, entry.name);
      if (entry.isDirectory()) stack.push(path);
      else if (entry.name.endsWith(".rs")) {
        const text = readFileSync(path, "utf8");
        const normalized = path.replaceAll("\\", "/");
        if (text.includes("TcpListener") && !normalized.includes("remote/listener.rs")) {
          offenders.push(normalized);
        }
      }
    }
  }
  if (offenders.length > 0) {
    failures.push(`默认无口守卫：TcpListener 越界 ${offenders.join(", ")}`);
  }
} else {
  failures.push("默认无口守卫：remote/listener.rs 不存在");
}

// 2. 配对开关 / 3. 钉扎 / 4. 能力矩阵：真实测试断言。
const guards = [
  ["配对开关（无设备拒绝监听）", () => cargoTest("r04_remote_listener", "r04_a1_a4_listener_lifecycle_follows_devices_and_switch")],
  ["证书钉扎（错指纹握手失败）", () => cargoTest("r03_tls_pinning", "r03_a2_wrong_fingerprint_fails_in_the_tls_handshake")],
  ["能力矩阵（forbidden 全拒）", () => cargoTest("r04_remote_listener", "r04_a3_capabilities_and_forbidden_methods_are_enforced")],
  ["吊销即断（设备生命周期）", () => cargoTest("r11_device_management", "r11_a2_revocation_drops_the_live_connection_and_refuses_reconnect")],
  ["绑定校验（公网地址拒绝）", () => libTest("public_and_wildcard_binds_are_refused")],
];
for (const [name, run] of guards) {
  if (!run()) failures.push(name);
}

// R14.A2：文档结构校验（必备小节 + 无 TBD/TODO）。
const docPath = join(root, "docs/support/guides/remote-control.md");
if (!existsSync(docPath)) {
  failures.push("文档守卫：docs/support/guides/remote-control.md 不存在");
} else {
  const doc = readFileSync(docPath, "utf8");
  for (const section of [
    "## 配对",
    "## 能力",
    "## 吊销与管理",
    "## 安全模型",
    "## 端口与防火墙",
    "Windows",
    "macOS",
    "Linux",
    "## 与中继的关系",
    "## 故障排查",
  ]) {
    if (!doc.includes(section)) {
      failures.push(`文档守卫：缺少小节「${section}」`);
    }
  }
  const match = doc.match(/\b(TBD|TODO|FIXME)\b/);
  if (match) {
    failures.push(`文档守卫：存在未完成标记 ${match[0]}`);
  }
}

if (failures.length > 0) {
  console.error(`verify-remote-guards: ${failures.length} FAILED`);
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}
console.log("verify-remote-guards: all security guards green");
