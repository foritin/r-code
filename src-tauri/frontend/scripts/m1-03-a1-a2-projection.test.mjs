// M1-03.A1/A2 内核：§4.4 优先级全组合唯一、unread 独立性、终态单调、父终态级联。
// 只依赖被测投影模块；与运行时 UI/时钟解耦（级联为纯同步封口，可映射 ≤1s 墙钟合同）。

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import ts from "typescript";

const frontendDir = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..");

async function loadProjection() {
  const source = readFileSync(
    path.join(frontendDir, "src", "lib", "task-status-projection.ts"),
    "utf8",
  );
  // 去掉仅含类型的 import（transpile 后无法解析 ./types 的运行时路径）。
  const trimmed = source.replace(/import type[\s\S]*?from\s*"[^"]+";\n/, "");
  const { outputText } = ts.transpileModule(trimmed, {
    compilerOptions: {
      module: ts.ModuleKind.ESNext,
      target: ts.ScriptTarget.ES2022,
      verbatimModuleSyntax: false,
    },
  });
  const dataUrl = `data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`;
  return import(dataUrl);
}

const ALL_STATES = [
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

test("A1 全组合归并输出唯一且属于 12 态全集", async () => {
  const m = await loadProjection();
  for (let i = 0; i < ALL_STATES.length; i += 1) {
    for (let j = 0; j < ALL_STATES.length; j += 1) {
      const a = ALL_STATES[i];
      const b = ALL_STATES[j];
      const ab = m.projectStatus({ display_states: [a, b], pending_permissions: 0 });
      const ba = m.projectStatus({ display_states: [b, a], pending_permissions: 0 });
      assert.equal(ab.display_state, ba.display_state, `${a} vs ${b}`);
      assert.ok(ALL_STATES.includes(ab.display_state), `结果越界: ${ab.display_state}`);
    }
  }
});

test("A1 优先级锚点：archived > awaiting > failed族 > review族 > running > queued > idle", async () => {
  const m = await loadProjection();
  const cases = [
    [["archived", "waiting_for_approval"], "archived"],
    [["waiting_for_question", "running"], "waiting_for_question"],
    [["failed", "running"], "failed"],
    [["workspace_binding_invalid", "queued"], "workspace_binding_invalid"],
    [["review_ready", "verifying"], "review_ready"],
    [["verification_required", "queued"], "verification_required"],
    [["verifying", "queued"], "verifying"],
    [["running", "idle"], "running"],
    [["queued", "idle"], "queued"],
  ];
  for (const [states, expect] of cases) {
    assert.equal(
      m.projectStatus({ display_states: states, pending_permissions: 0 }).display_state,
      expect,
    );
  }
});

test("A1 unread 完全不改变 display_state 与 attention", async () => {
  const m = await loadProjection();
  const base = {
    display_states: ["running"],
    pending_permissions: 1,
    binding_invalid: true,
    latest_run_failed: false,
    review_pending: false,
  };
  const zero = m.projectStatus({ ...base, unread_count: 0 });
  assert.deepEqual(zero.attention.sort(), ["approval_required", "workspace_binding_invalid"]);
  for (const n of [1, 7, 999]) {
    assert.deepEqual(m.projectStatus({ ...base, unread_count: n }), zero, `unread=${n} 改变了投影`);
  }
  const empty = m.projectStatus({ display_states: ["idle"], unread_count: 42 });
  assert.equal(empty.display_state, "idle");
  assert.deepEqual(empty.attention, []);
});

test("A2 终态单调：迟到的任何帧不能复活 archived/failed/interrupted", async () => {
  const m = await loadProjection();
  for (const terminal of m.TERMINAL_DISPLAY_STATES) {
    for (const late of ALL_STATES) {
      assert.equal(m.mergeMonotonic(terminal, late), terminal);
    }
  }
  assert.equal(m.mergeMonotonic("queued", "running"), "running");
  assert.equal(m.mergeMonotonic(undefined, "queued"), "queued");
});

test("A2 父终态一次 reduce 原子封口 child/tool/timer；迟到帧不复活；非终态父不误伤", async () => {
  const m = await loadProjection();
  const outcome = m.cascadeParentTerminal({
    parent_display_state: "failed",
    children: [
      { id: "c1", display_state: "running" },
      { id: "c2", display_state: "failed" },
    ],
    tools: [
      { id: "t1", status: "running" },
      { id: "t2", status: "ok" },
    ],
    timers: [{ id: "k1", running: true }],
  });
  assert.deepEqual(outcome.children.map((c) => c.display_state), ["interrupted", "failed"]);
  assert.deepEqual(outcome.tools.map((t) => t.status), ["error", "ok"]);
  assert.deepEqual(outcome.timers.map((t) => t.running), [false]);

  for (const child of outcome.children) {
    const revived = m.mergeMonotonic(child.display_state, "running");
    assert.equal(revived === "running" && child.display_state !== "running", false);
  }

  const live = m.cascadeParentTerminal({
    parent_display_state: "running",
    children: [{ id: "c1", display_state: "running" }],
    tools: [{ id: "t1", status: "running" }],
    timers: [{ id: "k1", running: true }],
  });
  assert.equal(live.children[0].display_state, "running");
  assert.equal(live.tools[0].status, "running");
  assert.equal(live.timers[0].running, true);
});
