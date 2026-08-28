// M1-02.A1（前端层）：能力开关矩阵——关闭的能力入口不可见且带统一结构化
// 错误码；未知/脏 payload 不得翻转任何位；locale 必须已为每个 reasonCode
// 准备 errors.<feature>.feature_disabled 文案（与 Rust disabled_error_code 同名）。

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";

import ts from "typescript";

const frontendDir = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..");

async function loadFlagsModule() {
  const source = readFileSync(path.join(frontendDir, "src", "lib", "feature-flags.ts"), "utf8");
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: {
      module: ts.ModuleKind.ESNext,
      target: ts.ScriptTarget.ES2022,
      verbatimModuleSyntax: false,
    },
  });
  const dataUrl = `data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`;
  return import(dataUrl);
}

test("默认全关：默认 flags 下三个入口全部不可见且带各自 feature_disabled 码", async () => {
  const m = await loadFlagsModule();
  const flags = m.DEFAULT_FEATURE_FLAGS;
  for (const [feature, visibility] of [
    ["browser", m.browserEntryVisibility(flags)],
    ["automation", m.automationEntryVisibility(flags)],
    ["worktree", m.worktreeEntryVisibility(flags)],
  ]) {
    assert.equal(visibility.visible, false, feature);
    assert.equal(visibility.reasonCode, `${feature}.feature_disabled`, feature);
  }
});

test("启用矩阵：任一位开启仅影响自身入口", async () => {
  const m = await loadFlagsModule();
  const onlyBrowser = m.normalizeFeatureFlags({ browser_enabled: true });
  assert.deepEqual(
    {
      browser: m.browserEntryVisibility(onlyBrowser).visible,
      automation: m.automationEntryVisibility(onlyBrowser).visible,
      worktree: m.worktreeEntryVisibility(onlyBrowser).visible,
    },
    { browser: true, automation: false, worktree: false },
  );

  const allOn = m.normalizeFeatureFlags({
    browser_enabled: true,
    automation_enabled: true,
    worktree_enabled: true,
  });
  assert.deepEqual(
    [
      m.browserEntryVisibility(allOn),
      m.automationEntryVisibility(allOn),
      m.worktreeEntryVisibility(allOn),
    ].map((v) => v.visible),
    [true, true, true],
  );
});

test("污染防御：未知字段、字符串布尔、非对象输入都无法把任何位置为 enabled", async () => {
  const m = await loadFlagsModule();
  for (const dirty of [
    { browser_enabled: "yes" },
    { browser_enabled: 1 },
    { browser_enabled: true, automation_enabled: "true" },
    { extra_future_flag: true },
    null,
    "browser_enabled=true",
    42,
    [],
  ]) {
    const flags = m.normalizeFeatureFlags(dirty);
    // 第三个用例的 browser 位是合法布尔 true，其余输入必须全部保持关闭。
    const expectBrowserOn = Array.isArray(dirty) === false && dirty !== null && typeof dirty === "object" && dirty.browser_enabled === true;
    assert.equal(flags.browser_enabled, expectBrowserOn, JSON.stringify(dirty));
    assert.equal(flags.automation_enabled, false, JSON.stringify(dirty));
    assert.equal(flags.worktree_enabled, false, JSON.stringify(dirty));
  }
});

test("reasonCode 与 locale 的 errors.<feature>.feature_disabled 一一存在（双语）", async () => {
  const m = await loadFlagsModule();
  const codes = new Set(
    [
      m.browserEntryVisibility(m.DEFAULT_FEATURE_FLAGS),
      m.automationEntryVisibility(m.DEFAULT_FEATURE_FLAGS),
      m.worktreeEntryVisibility(m.DEFAULT_FEATURE_FLAGS),
    ].map((v) => v.reasonCode),
  );
  for (const locale of ["zh-CN", "en-US"]) {
    const table = JSON.parse(
      readFileSync(path.join(frontendDir, "src", "i18n", "locales", `${locale}.json`), "utf8"),
    );
    for (const code of codes) {
      const [ns, key] = code.split(".");
      assert.ok(table.errors?.[ns]?.[key], `${locale} 缺少 errors.${code}`);
      assert.equal(typeof table.errors[ns][key], "string");
    }
  }
});
