// M2-03.A4 演进门：Settings Pane 注册表与 §5.7 矩阵一致，且已实现页与
// SettingsScene 的实际 pane 数组对齐。6 个 pending 页在补齐前使 A4 显式失败——
// 不以 7 页冒充 12 页完成。

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";

const frontendDir = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..");
const registry = JSON.parse(
  readFileSync(path.join(frontendDir, "src", "lib", "settings-pane-registry.json"), "utf8"),
);
const scene = readFileSync(
  path.join(frontendDir, "src", "components", "scenes", "SettingsScene.tsx"),
  "utf8",
);

const EXPECTED_ORDER = [
  "providers", "agents", "subagents", "tools", "knowledge", "permissions",
  "security", "appearance", "notifications", "lifecycle", "updates", "diagnostics",
];

test("注册表恰含 §5.7 的 12 个唯一 Pane 且顺序一致", () => {
  const ids = registry.panes.map((p) => p.id);
  assert.deepEqual(ids, EXPECTED_ORDER);
  assert.equal(new Set(ids).size, 12);
});

test("implemented 页必须与 SettingsScene 实际 pane 数组对齐", () => {
  const implemented = registry.panes.filter((p) => p.status === "implemented");
  for (const pane of implemented) {
    assert.ok(
      scene.includes(`key: "${pane.scene_key}"`) || scene.includes(`"${pane.scene_key}"`),
      `implemented 页 ${pane.id} 在 SettingsScene 无对应 pane`,
    );
  }
});

test("pending 页显式登记且不虚报", () => {
  const pending = registry.panes.filter((p) => p.status === "pending");
  // M3-01 落地 lifecycle 控件后，12 页全部 implemented，无 pending。
  assert.deepEqual(pending.map((p) => p.id), []);
  for (const pane of pending) {
    assert.ok(pane.note && pane.note.length > 8, `${pane.id} 缺迁移说明`);
    assert.ok(pane.scene_key === null || pane.scene_key !== pane.id, `${pane.id} 不得虚报 scene_key`);
  }
});
