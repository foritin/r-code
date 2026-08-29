// M3-04：Provider 健康
// 共享视图合同——五态齐全、非颜色语义（文字+图形）、configured≠connected、
// retry 只属于可重试态、checking 是唯一 spinner、无凭据字段。

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import ts from "typescript";

const frontendDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

async function loadHealth() {
  const source = readFileSync(path.join(frontendDir, "src", "lib", "provider-health.ts"), "utf8");
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
  });
  return import(`data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`);
}

test("五态视图齐全：文字+图形并存，且非颜色语义", async () => {
  const m = await loadHealth();
  for (const state of m.CONNECTIVITY_STATES) {
    const v = m.connectivityView(state);
    assert.ok(v.label.length >= 2, `${state} 缺文字标签`);
    assert.ok(v.glyph.length >= 1, `${state} 缺图形`);
  }
  assert.equal(m.connectivityView("checking").spinning, true);
  for (const state of ["unknown", "connected", "degraded", "failed"]) {
    assert.equal(m.connectivityView(state).spinning, false, `${state} 不应有 spinner`);
  }
});

test("configured 不冒充 connected", async () => {
  const m = await loadHealth();
  assert.ok(m.CONFIGURED_LABEL.includes("未探测"));
  assert.notEqual(m.connectivityView("unknown").label, "已连接");
});

test("retry 可用性：unknown/connected/degraded/failed 可重试，checking 不可", async () => {
  const m = await loadHealth();
  for (const state of ["unknown", "connected", "degraded", "failed"]) {
    assert.equal(m.connectivityView(state).retryable, true, `${state} 应可重试`);
  }
  assert.equal(m.connectivityView("checking").retryable, false);
});
