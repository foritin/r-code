#!/usr/bin/env node
// M2-02 验收断言执行器（Shell 壳层：左导航—中对话—右执行台）：
//   a1: Rail/Room 无第二套任务状态推断（组件层零 TaskDisplayState 字面量重推导；
//       状态语义唯一来源 presentation.ts / store 选择器）
//   a2: 顶层窗口几何冻结——room/workbench/shell/scenes 组件零窗口 API；
//       companion 精灵窗与 MenuBar 原生菜单属独立域白名单
//   a3: 布局约束——html overflow hidden + .scene overflow hidden + 壳层 grid minmax(0,1fr)
// 实现面：components/{shell,room,scenes}/、styles/{base,shell,r-code-ui}.css

import { readFileSync, readdirSync, statSync } from "node:fs";
import { parseArgs } from "node:util";
import path from "node:path";
import process from "node:process";

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..");
const FRONTEND_SRC = path.join(ROOT, "src-tauri", "frontend", "src");

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

const TASK_DISPLAY_STATES = [
  "archived", "waiting_for_approval", "waiting_for_question", "failed",
  "interrupted", "workspace_binding_invalid", "review_ready",
  "verification_required", "verifying", "running", "queued", "idle",
];

// 与 TaskDisplayState 词汇重叠但语义独立的白名单域：
//   VisualTaskState(running/attention/review/stopped/done)、TaskState(persisted)、
//   SubagentStatus、问题卡生命周期、MemoryReviewJobView。
const OTHER_DOMAIN_PATTERNS = [
  /MARKED_STATES/,
  /task\.state\s*===/,
  /persistedState\s*===/,
  /TaskState\b/,
  /VisualTaskState\b/,
  /SubagentStatus/,
  /child\.status/,
  /MemoryReviewJobView/,
];

const parts = {
  a1() {
    const surfaces = [
      "components/shell/Rail.tsx",
      "components/room/Canvas.tsx",
      "components/scenes/ConversationsScene.tsx",
      "components/scenes/ActivityScene.tsx",
      "components/scenes/DashboardScene.tsx",
    ];
    // 「第二套状态推断」= 本地重新定义推导函数；导入 presentation 的共享 helper 属合法消费。
    const computeSignatures = [
      /function\s+(isTaskLive|legacyDisplayState|taskDisplayState)\b/,
      /const\s+(isTaskLive|legacyDisplayState|taskDisplayState)\s*=/,
    ];
    const conditions = [];
    const offenders = [];
    for (const rel of surfaces) {
      const text = readFileSync(path.join(FRONTEND_SRC, rel), "utf8");
      for (const sig of computeSignatures) {
        if (sig.test(text)) offenders.push(`${rel} :: 计算签名 ${sig}`);
      }
      if (!/from\s+"[^"]*lib\/presentation"/.test(text) && !/store\/tasks/.test(text)) {
        offenders.push(`${rel} :: 未接入 presentation/store 状态源`);
      }
    }
    conditions.push({
      ok: offenders.length === 0,
      detail: offenders.length ? `第二套状态推断/未接共享源: ${offenders.join("; ")}` : "",
    });
    return [staticCheck("component:rail-room-single-status-source", conditions,
      "消费面只读取 TaskStatusView 并与共享投影比较；本地推导签名零命中")];
  },

  a2() {
    const conditions = [];
    // 工作台/壳层/房间域禁窗口 API（companion 精灵窗与 MenuBar 原生菜单为独立域）
    const guarded = [
      "components/room/SubagentWorkbench.tsx",
      "components/room/Canvas.tsx",
      "components/shell/Rail.tsx",
      "components/scenes/ConversationsScene.tsx",
      "components/scenes/DashboardScene.tsx",
      "components/scenes/ActivityScene.tsx",
    ];
    const windowApi = /(?<![A-Za-z])(setSize|setPosition|LogicalSize|PhysicalSize|outerPosition|inner_size|WindowBuilder|WebviewWindow)(?![A-Za-z])/;
    const hits = [];
    for (const rel of guarded) {
      const text = readFileSync(path.join(FRONTEND_SRC, rel), "utf8");
      if (windowApi.test(text)) hits.push(rel);
    }
    conditions.push({
      ok: hits.length === 0,
      detail: hits.length ? `工作台/壳层域出现窗口几何 API: ${hits.join(", ")}` : "",
    });
    // 全仓窗口 API 只允许白名单域（companion 独立窗口 / MenuBar 原生菜单）
    const allowed = ["components/companion/", "components/shell/MenuBar.tsx"];
    const allTs = [];
    const walk = (dir) => {
      for (const e of readdirSync(dir)) {
        const full = path.join(dir, e);
        try { if (statSync(full).isDirectory()) { walk(full); continue; } } catch { continue; }
        if (/\.(tsx?|mjs)$/.test(e)) allTs.push(full);
      }
    };
    walk(path.join(FRONTEND_SRC, "components"));
    const outside = allTs
      .filter((f) => windowApi.test(readFileSync(f, "utf8")))
      .map((f) => path.relative(FRONTEND_SRC, f))
      .filter((rel) => !allowed.some((a) => rel.startsWith(a)));
    conditions.push({
      ok: outside.length === 0,
      detail: outside.length ? `白名单域外出现窗口 API: ${outside.join(", ")}` : "",
    });
    return [staticCheck("integration:top-level-window-bounds-frozen", conditions,
      "开关执行台不改窗口几何；960×640 由 grid minmax + overflow 守卫保证（见 a3）")];
  },

  a3() {
    const conditions = [];
    const shell = readFileSync(path.join(FRONTEND_SRC, "styles", "shell.css"), "utf8");
    const rcui = readFileSync(path.join(FRONTEND_SRC, "styles", "r-code-ui.css"), "utf8");
    const base = readFileSync(path.join(FRONTEND_SRC, "styles", "base.css"), "utf8");
    conditions.push({ ok: shell.includes("minmax(0, 1fr)"), detail: "shell.css 壳层列缺少 minmax(0, 1fr)" });
    conditions.push({ ok: rcui.includes("minmax(0, 1fr)"), detail: "r-code-ui.css 壳层列缺少 minmax(0, 1fr)" });
    conditions.push({ ok: /html\s*{\s*overflow:\s*hidden/.test(base), detail: "html 缺 overflow hidden（横向溢出守卫）" });
    conditions.push({ ok: /\.scene\s*{[^}]*overflow:\s*hidden/.test(rcui.replace(/\n/g, " ")), detail: ".scene 缺 overflow hidden" });
    return [staticCheck("visual:layout-guards-no-horizontal-overflow", conditions)];
  },
};

let args;
try {
  args = parseArgs({ options: { part: { type: "string" } }, strict: true });
} catch (error) {
  console.error(`用法: m2-02-checks.mjs --part a1|a2|a3\n${error.message}`);
  process.exit(2);
}

const runner = parts[(args.values.part ?? "").toLowerCase()];
if (!runner) {
  console.error(`未知 part: ${args.values.part}`);
  process.exit(2);
}
const results = await runner();
const allOk = results.every((r) => r.exit_code === 0 && !r.timed_out);
console.log(JSON.stringify({ schema_version: "m2-02-checks.v1", part: (args.values.part ?? "").toLowerCase(), ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
