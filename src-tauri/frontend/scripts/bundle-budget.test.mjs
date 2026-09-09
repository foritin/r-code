import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { checkBundleBudget, DEFAULT_BUDGETS } from "./check-bundle-budget.mjs";

test("production bundle budgets keep a separate one-megabyte asset ceiling", () => {
  assert.equal(DEFAULT_BUDGETS.maxAssetBytes, 1_000_000);
  assert.ok(DEFAULT_BUDGETS.maxJavaScriptBytes < DEFAULT_BUDGETS.maxAssetBytes);
  assert.ok(DEFAULT_BUDGETS.maxCssBytes < DEFAULT_BUDGETS.maxAssetBytes);
});

test("bundle budget reports aggregate and individual file violations", () => {
  const dir = mkdtempSync(path.join(os.tmpdir(), "r-code-bundle-budget-"));
  try {
    mkdirSync(path.join(dir, "assets"));
    writeFileSync(path.join(dir, "index.html"), "ok");
    writeFileSync(path.join(dir, "assets", "oversized.js"), Buffer.alloc(11));
    writeFileSync(path.join(dir, "assets", "oversized.webp"), Buffer.alloc(21));
    const result = checkBundleBudget(dir, {
      totalBytes: 30,
      totalJavaScriptBytes: 10,
      totalCssBytes: 10,
      maxJavaScriptBytes: 10,
      maxCssBytes: 10,
      maxAssetBytes: 20,
    });
    assert.ok(result.violations.some((value) => value.startsWith("total bundle")));
    assert.ok(result.violations.some((value) => value.startsWith("total JavaScript")));
    assert.ok(result.violations.some((value) => value.startsWith("JavaScript chunk")));
    assert.ok(result.violations.some((value) => value.startsWith("static asset")));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
