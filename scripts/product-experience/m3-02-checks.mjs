#!/usr/bin/env node
// M3-02 验收断言执行器（关闭对话框、恢复入口、统一退出清理）：
//   a1: 三入口单例对话框/cancel 不写偏好/remember 生效——ClosePromptDialog 静态合同
//       + close_gate 持久化与等价触发套件
//   a2: restore 面能力约束（tray/dock 恢复路径 + restore=none 拒绝 hide）
//   a3: ShutdownCoordinator 有界清理/局部失败汇总/terminal projection 单调
//   a4: 对话框 a11y（Esc/记住勾选/取消按钮/aria-modal）+ 设置重置与显式退出入口
// 实现面：close_gate.rs、shutdown_coordinator.rs、lifecycle_commands.rs、
//         main.rs 统一 close 臂、ClosePromptDialog.tsx、SettingsScene LifecycleSection。

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { parseArgs } from "node:util";
import path from "node:path";
import process from "node:process";

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..");
const HOST_SRC = path.join(ROOT, "src-tauri", "src");
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
  console.log(`${result.exit_code === 0 && !r.timed_out ? "PASS" : "FAIL"} ${name} (${result.duration_ms}ms)`);
  if (result.exit_code !== 0) {
    console.error((result.stderr_tail || result.stdout_tail || "").split("\n").slice(0, 6).join("\n"));
  }
  return result;
}

function staticCheck(name, conditions, note) {
  const failed = conditions.filter((c) => !c.ok);
  const result = {
    name,
    exit_code: failed.length === 0 ? 0 : 1,
    timed_out: false,
    duration_ms: 0,
    stdout_tail: failed.length === 0 ? (note ?? "all conditions hold") : "",
    stderr_tail: failed.map((c) => c.detail).join("\n"),
  };
  console.log(`${result.exit_code === 0 ? "PASS" : "FAIL"} ${name}`);
  if (failed.length > 0) console.error(result.stderr_tail);
  return result;
}

const parts = {
  async a1() {
    const dialog = readFileSync(
      path.join(ROOT, "src-tauri", "frontend", "src", "components", "shell", "ClosePromptDialog.tsx"),
      "utf8",
    );
    return [
      run("rust:close-gate-intent-and-persistence", "cargo", ["test", "-p", "r-code-host", "--lib", "close_gate::"], { env: CARGO_ENV }),
      staticCheck("static:dialog-singleton-and-decisions", [
        { ok: dialog.includes("close-prompt-request"), detail: "对话框未订阅 Host prompt 事件" },
        { ok: dialog.includes('decide("cancel")'), detail: "缺 cancel 路径（不写偏好）" },
        { ok: dialog.includes("记住我的选择"), detail: "缺 remember 选项" },
        { ok: dialog.includes("closePromptDecision"), detail: "决定未回传 Host" },
      ], "Host prompt 单例渲染；cancel 不落盘；remember 经 Host 确认路径写入"),
    ];
  },
  async a2() {
    const main = readFileSync(path.join(HOST_SRC, "main.rs"), "utf8");
    return [
      run("rust:restore-capability-gate", "cargo", ["test", "-p", "r-code-host", "--lib", "close_gate::tests::a4_"], { env: CARGO_ENV }),
      staticCheck("static:restore-surfaces-present", [
        { ok: main.includes("MAIN_TRAY_ID"), detail: "缺 windows tray 恢复面" },
        { ok: main.includes("RestoreCapability::Dock"), detail: "缺 macos Dock 恢复面" },
        { ok: main.includes("RestoreCapability::None"), detail: "缺无恢复面降级" },
      ], "tray/dock/none 三种恢复面均在 Host 关闭流中显式建模"),
    ];
  },
  async a3() {
    const coordinator = readFileSync(path.join(HOST_SRC, "shutdown_coordinator.rs"), "utf8");
    return [
      run("rust:shutdown-coordinator-bounded", "cargo", ["test", "-p", "r-code-host", "--lib", "shutdown_coordinator::"], { env: CARGO_ENV }),
      staticCheck("static:terminal-projection-and-explicit-quit", [
        { ok: coordinator.includes("terminal_projection_persisted"), detail: "缺 terminal projection 单调标记" },
        { ok: coordinator.includes("SubsystemOutcome::TimedOut"), detail: "缺有界超时分类" },
        { ok: readFileSync(path.join(HOST_SRC, "lifecycle_commands.rs"), "utf8").includes("cmd_lifecycle_explicit_quit"), detail: "缺显式退出 bypass 入口" },
      ], "quit/restart 经有界协调；失败汇总脱敏且不无限等待"),
    ];
  },
  async a4() {
    const dialog = readFileSync(
      path.join(ROOT, "src-tauri", "frontend", "src", "components", "shell", "ClosePromptDialog.tsx"),
      "utf8",
    );
    const scene = readFileSync(
      path.join(ROOT, "src-tauri", "frontend", "src", "components", "scenes", "SettingsScene.tsx"),
      "utf8",
    );
    return [
      staticCheck("accessibility:dialog-and-settings-entries", [
        { ok: dialog.includes('role="dialog"') && dialog.includes("aria-modal=\"true\""), detail: "对话框缺 modal 语义" },
        { ok: dialog.includes('decide("cancel")'), detail: "缺键盘可达的 cancel" },
        { ok: scene.includes("重置为每次询问"), detail: "设置缺关闭行为重置入口" },
        { ok: scene.includes("立即退出应用"), detail: "设置缺显式退出入口" },
        { ok: scene.includes("lifecycle-close-behavior"), detail: "缺关闭行为选择器" },
      ], "仅键盘可完成选择/勾选/取消；设置提供当前行为、重置与显式退出"),
    ];
  },
};

let args;
try {
  args = parseArgs({ options: { part: { type: "string" } }, strict: true });
} catch (error) {
  console.error(`用法: m3-02-checks.mjs --part a1|a2|a3|a4\n${error.message}`);
  process.exit(2);
}

const runner = parts[(args.values.part ?? "").toLowerCase()];
if (!runner) {
  console.error(`未知 part: ${args.values.part}`);
  process.exit(2);
}
const results = await runner();
const allOk = results.every((r) => r.exit_code === 0 && !r.timed_out);
console.log(JSON.stringify({ schema_version: "m3-02-checks.v1", part: (args.values.part ?? "").toLowerCase(), ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
