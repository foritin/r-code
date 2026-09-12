/**
 * R21 — core 平台纯净性守卫：core/ 下所有模块不得 import DOM-only 或
 * RN-only API（F13/F14——同一套 core 被 PWA 与 RN 消费）。
 */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const coreDir = here;

const FORBIDDEN = [
  /from ["']react["']/,
  /from ["']react-dom/,
  /from ["']react-native["']/,
  /from ["']@tauri-apps/,
  /document\./,
  /window\./,
  /navigator\./,
  /localStorage/,
];

test("R21.A1: core 模块无平台专属 import（node 与 RN 双环境可跑）", () => {
  for (const name of readdirSync(coreDir)) {
    if (!name.endsWith(".ts") || name.endsWith(".test.ts")) continue;
    const text = readFileSync(join(coreDir, name), "utf8");
    for (const pattern of FORBIDDEN) {
      assert.doesNotMatch(text, pattern, `${name} 引入了平台专属 API`);
    }
  }
});

test("R21.A1: core 测试文件齐备（node:test 执行）", () => {
  // 两侧分工（诚实口径）：core 的纯逻辑断言由 `node --test` 在仓库根跑；
  // RN 侧跑的是 mobile/__tests__ 下的渲染与 adapter 测试。不与 RN 环境
  // "同套断言复跑"——jest 的 CJS 环境跑不了 node:test 的模块。
  const tests = readdirSync(coreDir).filter((name) => name.endsWith(".test.mjs"));
  assert.ok(tests.length >= 4, `core 测试文件齐备：${tests.join(",")}`);
});
