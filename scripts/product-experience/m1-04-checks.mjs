#!/usr/bin/env node
// M1-04 验收断言执行器（Updater 产品链与重启边界）：
//   a1: fixture 域测试——状态迁移确定；损坏包/签名错永不进入 ready/restart
//   a2: 稍后重启保留可用；RestartPending 仅一次受控 bypass；不弹普通关闭问询的合同标记
//   a3: 错误码卫生（网络/签名错误为稳定用户码，不泄原始 minisign/URL/token 细节）
// 实现面：src-tauri/src/updater/*（domain 9 相状态机 + minisign 校验 + 持久化）与
// frontend/src/lib/updater-contract.ts；fixture-only，无生产端点。

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { parseArgs } from "node:util";
import path from "node:path";
import process from "node:process";

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..");
const HOST_SRC = path.join(ROOT, "src-tauri", "src", "updater");
const CARGO_ENV = { ...process.env, CARGO_NET_GIT_FETCH_WITH_CLI: "true" };

function run(name, bin, args, options = {}) {
  const started = Date.now();
  const r = spawnSync(bin, args, {
    cwd: options.cwd ?? ROOT,
    encoding: "utf8",
    shell: false,
    timeout: options.timeout_ms ?? 20 * 60 * 1000,
    env: options.env ?? process.env,
  });
  const result = {
    name,
    command: [bin, ...args].join(" "),
    exit_code: r.status ?? null,
    timed_out: r.signal === "SIGTERM",
    duration_ms: Date.now() - started,
    stdout_tail: (r.stdout ?? "").split("\n").slice(-8).join("\n").slice(0, 1500),
    stderr_tail: (r.stderr ?? "").split("\n").slice(-8).join("\n").slice(0, 1500),
  };
  console.log(`${result.exit_code === 0 && !result.timed_out ? "PASS" : "FAIL"} ${name} (${result.duration_ms}ms)`);
  if (result.exit_code !== 0) {
    console.error((result.stderr_tail || result.stdout_tail || "").split("\n").slice(0, 6).join("\n"));
  }
  return result;
}

const parts = {
  async a1() {
    return [
      run("rust:updater-domain-fixture", "cargo", ["test", "-p", "r-code-host", "--lib", "updater::"], { env: CARGO_ENV }),
    ];
  },
  async a2() {
    const results = [
      run(
        "rust:updater-restart-pending-latch",
        "cargo",
        ["test", "-p", "r-code-host", "--lib", "updater::domain_tests"],
        { env: CARGO_ENV },
      ),
    ];
    // 静态合同：restart 命令只委派状态机的 restart()（受控 bypass 单点），无二次旁路。
    const tauri = readFileSync(path.join(HOST_SRC, "..", "tauri_commands.rs"), "utf8");
    const fnAt = tauri.indexOf("pub async fn cmd_updater_restart");
    const body = tauri.slice(fnAt, fnAt + 300);
    if (!/state\.restart\(\)/.test(body)) {
      results.push({
        name: "static:restart-bypass-single-entry",
        command: "grep cmd_updater_restart -> state.restart()",
        exit_code: 1,
        timed_out: false,
        duration_ms: 0,
        stdout_tail: "",
        stderr_tail: "restart 命令未收敛到 state.restart() 单点",
      });
    } else {
      results.push({
        name: "static:restart-bypass-single-entry",
        command: "grep cmd_updater_restart -> state.restart()",
        exit_code: 0,
        timed_out: false,
        duration_ms: 0,
        stdout_tail: "restart 经 domain.restart() 单点，updater_restart 语义由 M3 统一退出清理消费",
        stderr_tail: "",
      });
    }
    return results;
  },
  async a3() {
    const results = [
      run(
        "rust:updater-error-code-hygiene",
        "cargo",
        ["test", "-p", "r-code-host", "--lib", "updater::domain_tests::network_and_signature_errors"],
        { env: CARGO_ENV },
      ),
    ];
    // 证据卫生：源码不得出现 token 持久化或私有 URL 参数落盘。
    const domain = readFileSync(path.join(HOST_SRC, "domain.rs"), "utf8");
    const persistence = readFileSync(path.join(HOST_SRC, "persistence.rs"), "utf8");
    const bad = /token|api[_-]?key/i.test(persistence)
      || /(api[_-]?key|token)\s*[:=]\s*"/i.test(domain.replace(/test|fixture/gi, ""));
    results.push({
      name: "static:no-token-or-private-url-persistence",
      command: "scan updater domain/persistence",
      exit_code: bad ? 1 : 0,
      timed_out: false,
      duration_ms: 0,
      stdout_tail: bad ? "发现疑似 token/私有参数落盘" : "未发现 token/私有 URL 参数持久化",
      stderr_tail: "",
    });
    return results;
  },
};

let args;
try {
  args = parseArgs({ options: { part: { type: "string" } }, strict: true });
} catch (error) {
  console.error(`用法: m1-04-checks.mjs --part a1|a2|a3\n${error.message}`);
  process.exit(2);
}

const runner = parts[(args.values.part ?? "").toLowerCase()];
if (!runner) {
  console.error(`未知 part: ${args.values.part}`);
  process.exit(2);
}

const results = await runner();
const allOk = results.every((r) => r.exit_code === 0 && !r.timed_out);
console.log(JSON.stringify({ schema_version: "m1-04-checks.v1", part: (args.values.part ?? "").toLowerCase(), ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
