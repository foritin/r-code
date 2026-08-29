// M4-03：执行台 IA 合同——tab 集合精确、自动聚焦优先级、用户手动保持、容器无关。

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import ts from "typescript";

const frontendDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

async function loadIA() {
  const source = readFileSync(path.join(frontendDir, "src", "lib", "workbench-ia.ts"), "utf8");
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
  });
  return import(`data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`);
}

test("A1 一级 tab 集合恰为 overview/subagents/changes", async () => {
  const m = await loadIA();
  assert.deepEqual([...m.WORKBENCH_TABS], ["overview", "subagents", "changes"]);
  assert.equal(m.isWorkbenchIATab("tools"), false, "全局工具列表不是一级 IA");
  assert.equal(m.isWorkbenchIATab("overview"), true);
});

test("A2 自动聚焦：Attention > active child > changes ready > overview", async () => {
  const m = await loadIA();
  assert.equal(m.autoSelectTab({ attentionCount: 1, activeChildCount: 1, changesReady: true }), "subagents");
  assert.equal(m.autoSelectTab({ attentionCount: 0, activeChildCount: 2, changesReady: true }), "subagents");
  assert.equal(m.autoSelectTab({ attentionCount: 0, activeChildCount: 0, changesReady: true }), "changes");
  assert.equal(m.autoSelectTab({ attentionCount: 0, activeChildCount: 0, changesReady: false }), "overview");
  // 用户手动选择保持
  assert.equal(m.initialTab({ attentionCount: 1, activeChildCount: 0, changesReady: false }, "changes"), "changes");
  assert.equal(m.initialTab({ attentionCount: 0, activeChildCount: 0, changesReady: false }, null), "overview");
});

test("A3 容器无关：模块不引用窗口/几何 API", async () => {
  const { readFileSync: rf } = await import("node:fs");
  const src = rf(path.join(frontendDir, "src", "lib", "workbench-ia.ts"), "utf8");
  assert.ok(!/outerWidth|setSize|setPosition|LogicalSize/.test(src), "IA 模块不得触碰窗口几何");
});
