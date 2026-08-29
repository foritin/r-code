// M1-03.A3/A5 静态一致性门（五消费面统一 + spinner 合同）：
//   1) 任务状态中文标签只允许出现在 lib 权威源（presentation.ts）；
//      组件不得各自手写 display_state → 文案映射（历史漂移根因）。
//   2) 组件层禁止 rotate/spinner 状态指示；spinning 唯一来源是
//      task-status-projection.STATUS_GLYPHS，且集合恰为 {running, verifying}。
//   3) 五个消费面（Rail/Room Canvas/会话列表/活动与通知/Workbench/仪表盘）
//      都必须从 presentation 导入状态语义，而不是本地重推导。

import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import ts from "typescript";
import { fileURLToPath } from "node:url";

const frontendDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const srcDir = path.join(frontendDir, "src");

const ALL_DISPLAY_STATES = [
  "archived",
  "waiting_for_approval",
  "waiting_for_question",
  "failed",
  "interrupted",
  "workspace_binding_invalid",
  "review_ready",
  "verification_required",
  "verifying",
  "running",
  "queued",
  "idle",
];

function* walkTsFiles(dir) {
  for (const entry of readdirSync(dir)) {
    const full = path.join(dir, entry);
    if (statSync(full).isDirectory()) {
      yield* walkTsFiles(full);
    } else if (/\.(tsx?|mjs)$/.test(entry)) {
      yield full;
    }
  }
}

test("A3 任务状态标签唯一源：组件层零散射", () => {
  // 只锁任务状态专属文案；「等待回答」另属问题卡生命周期（pending/answered/...），放行。
  const canonicalLabels = [
    "等待审批",
    "工作区失效",
    "等待审查",
    "需要验证",
  ];
  const offenders = [];
  for (const file of walkTsFiles(path.join(srcDir, "components"))) {
    const text = readFileSync(file, "utf8");
    for (const label of canonicalLabels) {
      if (text.includes(label)) offenders.push(`${path.relative(srcDir, file)} :: ${label}`);
    }
  }
  assert.deepEqual(offenders, [], "组件手写任务状态文案（应导入 presentation 权威映射）");
});

test("A3 五消费面统一从 presentation 导入状态语义", () => {
  const surfaces = [
    "components/shell/Rail.tsx",
    "components/room/Canvas.tsx",
    "components/scenes/ConversationsScene.tsx",
    "components/scenes/ActivityScene.tsx",
    "components/scenes/DashboardScene.tsx",
    // 注：SubagentWorkbench.tsx 的 statusLabel 属 SubagentStatus 域（子代理运行态），
    // 与 TaskDisplayState 词汇重叠但语义独立——归 M4-03 执行台重构 / M4-04 协作树处理。
  ];
  const missing = surfaces.filter((rel) => {
    const text = readFileSync(path.join(srcDir, rel), "utf8");
    // 规则一：直接导入 presentation（Rail/Canvas/列表/活动/仪表盘均如此）。
    if (/from\s+"[^"]*lib\/presentation"/.test(text)) return false;
    // 规则二：文件内不含任何 TaskDisplayState 字面量 → 无任务状态重推导，
    // 其本地 statusLabel 属于其他域（如子代理 SubagentStatus，M4-03 重构面）。
    const taskStateLiterals = ALL_DISPLAY_STATES.filter((s) => text.includes(`"${s}"`));
    return taskStateLiterals.length > 0;
  });
  assert.deepEqual(missing, [], "以下消费面未接入共享状态源");
});

test("A5 spinner 合同：组件零旋转指示；投影层 spinning 恰为 running+verifying", async () => {
  const spinners = [];
  for (const file of walkTsFiles(path.join(srcDir, "components"))) {
    if (readFileSync(file, "utf8").includes("animate-spin")) {
      spinners.push(path.relative(srcDir, file));
    }
  }
  assert.deepEqual(spinners, [], "组件层出现旋转指示（状态动画只能来自 STATUS_GLYPHS）");

  const source = readFileSync(path.join(srcDir, "lib", "task-status-projection.ts"), "utf8");
  const { outputText } = ts.transpileModule(
    source.replace(/import type[\s\S]*?from\s*"[^"]+";\n/, ""),
    { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 } },
  );
  const mod = await import(
    `data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`
  );
  const spinning = Object.entries(mod.STATUS_GLYPHS)
    .filter(([, g]) => g.spinning === true)
    .map(([state]) => state)
    .sort();
  assert.deepEqual(spinning, ["running", "verifying"], "spinning 集合违反 A5 合同");
  for (const [state, g] of Object.entries(mod.STATUS_GLYPHS)) {
    assert.ok(g.glyph.length > 0, `${state} 缺少静态图形`);
  }
});
