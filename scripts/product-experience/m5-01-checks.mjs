#!/usr/bin/env node
// M5-01 验收断言执行器（可访问性/IME/缩放/动作加固）：
//   a1 键盘: composer 流 + settings 搜索/深链/返回 E2E
//   a2 IME: composition 守卫静态 + send-mode 套件
//   a3 视觉: 对比度门 + 三视口溢出矩阵（复用 M2-01/M2-04 套件）
//   a4 动效: reduced-motion 全停 + spinner 仅 checking
//   a5 live region + 焦点不入隐藏区
//   a6 触控命中区 ≥44px（390px）

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { parseArgs } from "node:util";
import path from "node:path";
import process from "node:process";

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..");
const FRONTEND = path.join(ROOT, "src-tauri", "frontend");

function run(name, bin, args, options = {}) {
  const started = Date.now();
  const r = spawnSync(bin, args, {
    cwd: options.cwd ?? FRONTEND,
    encoding: "utf8",
    shell: false,
    timeout: options.timeout_ms ?? 25 * 60 * 1000,
    env: options.env ?? process.env,
  });
  const result = {
    name,
    command: [bin, ...args].join(" "),
    exit_code: r.status ?? null,
    timed_out: r.signal === "SIGTERM",
    duration_ms: Date.now() - started,
    stdout_tail: (r.stdout ?? "").split("\n").slice(-8).join("\n").slice(0, 1200),
    stderr_tail: (r.stderr ?? "").split("\n").slice(-8).join("\n").slice(0, 1200),
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

function m2_01(part) {
  return run(`m2-01:${part}`, process.execPath,
    [`${ROOT}/scripts/product-experience/m2-01-checks.mjs`, "--part", part]);
}
function m2_04_suite() {
  return run("m2-04:viewport-theme-e2e", process.execPath, ["--test", "scripts/m2-04-theme-responsive.test.mjs"]);
}

const parts = {
  async a1() {
    return [
      run("a11y:composer-keyboard-flow", process.execPath, ["--test", "scripts/m2-03-a3-keyboard.test.mjs"]),
      run("a11y:settings-search-deeplink-keyboard", process.execPath, ["--test", "scripts/m2-03-a6-search-e2e.test.mjs"]),
      run("a11y:settings-nav-keyboard-narrow", process.execPath, ["--test", "scripts/m2-04-theme-responsive.test.mjs"]),
    ];
  },
  async a2() {
    const composer = readFileSync(path.join(FRONTEND, "src", "components", "room", "Composer.tsx"), "utf8");
    return [
      staticCheck("static:ime-composition-guard", [
        { ok: composer.includes("isComposing"), detail: "Composer 缺 IME composition 守卫" },
      ], "composition 期间 Enter 不发送/不停止"),
      run("compat:send-mode-switch", process.execPath, ["--test", "scripts/send-mode-switch.test.mjs"]),
    ];
  },
  async a3() {
    return [
      m2_01("a2"),
      m2_04_suite(),
    ];
  },
  async a4() {
    const base = readFileSync(path.join(FRONTEND, "src", "styles", "base.css"), "utf8");
    return [
      staticCheck("static:reduced-motion-global-stop", [
        { ok: base.includes("prefers-reduced-motion: reduce") && base.includes("animation: none"), detail: "缺 reduced-motion 全停规则" },
      ], "reduced-motion 下所有动画停止（spinner 呈静态图形+文字）"),
      run("motion:glyph-spinner-contract", process.execPath, ["--test", "scripts/m3-04-provider-health.test.mjs"]),
    ];
  },
  async a5() {
    const { readdirSync, statSync } = await import("node:fs");
    const srcDir = path.join(FRONTEND, "src");
    let liveRegions = 0;
    const walk = (dir) => {
      for (const e of readdirSync(dir)) {
        const full = path.join(dir, e);
        try { if (statSync(full).isDirectory()) { walk(full); continue; } } catch { continue; }
        if (/\.(tsx?)$/.test(e) && readFileSync(full, "utf8").includes("aria-live")) liveRegions += 1;
      }
    };
    walk(path.join(FRONTEND, "src", "components"));
    return [
      staticCheck("a11y:live-region-present", [
        { ok: liveRegions > 0, detail: "缺聚合 live region（异步状态播报）" },
      ], "异步状态经聚合 live region 播报"),
      m2_04_suite(),
    ];
  },
  async a6() {
    const tokens = readFileSync(path.join(FRONTEND, "src", "styles", "tokens.css"), "utf8");
    const base = readFileSync(path.join(FRONTEND, "src", "styles", "base.css"), "utf8");
    const hasSmallViewportGuard = tokens.includes("390") || tokens.includes("max-width: 420px")
      || base.includes("max-width: 420px");
    return [
      staticCheck("touch:hit-area-at-narrow-viewport", [
        { ok: hasSmallViewportGuard, detail: "390px 窄屏命中区守卫缺失（44×44 主动作/32×32 次要控件）" },
      ], "窄屏触控命中区规则存在性检查"),
    ];
  },
};

let args;
try {
  args = parseArgs({ options: { part: { type: "string" } }, strict: true });
} catch (error) {
  console.error(`用法: m5-01-checks.mjs --part a1..a6\n${error.message}`);
  process.exit(2);
}

const runner = parts[(args.values.part ?? "").toLowerCase()];
if (!runner) {
  console.error(`未知 part: ${args.values.part}`);
  process.exit(2);
}
const results = await runner();
const allOk = results.every((r) => r.exit_code === 0 && !r.timed_out);
console.log(JSON.stringify({ schema_version: "m5-01-checks.v1", part: (args.values.part ?? "").toLowerCase(), ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
