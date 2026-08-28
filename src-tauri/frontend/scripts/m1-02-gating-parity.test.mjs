// M1-02.A1（后端接线一致性静态门）：三处事实必须同源——
//   1) TS feature-flags.ts：三位 key、DEFAULT 全关、三个 visibility 入口；
//   2) Rust feature_flags.rs：同名三位变体 + <feature>.feature_disabled 错误码；
//   3) 能力闸在模块层真实生效：browser/commands.rs 的 contract 入口与
//      browser/tool_gateway.rs 的注册入口都经 flags.require(Browser) 拒绝。
// 动态行为（矩阵语义、require 拒绝码）分别在 feature-flag-matrix.test.mjs 与
// cargo test feature_flags:: 覆盖；这里锁定跨层一致性本身，防"文档写了、门禁没接"。

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";

const frontendDir = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..");
const hostSrc = path.resolve(frontendDir, "..", "src");

const FEATURES = ["browser", "automation", "worktree"];

test("TS 层导出与默认值：三位全关 + 三个可见性入口", () => {
  const source = readFileSync(path.join(frontendDir, "src", "lib", "feature-flags.ts"), "utf8");
  for (const feature of FEATURES) {
    assert.ok(source.includes(`${feature}_enabled`), `flags 字段缺失: ${feature}`);
    assert.ok(
      source.includes(`${feature}EntryVisibility`),
      `可见性入口缺失: ${feature}`,
    );
    const defaultBlock = source.slice(source.indexOf("DEFAULT_FEATURE_FLAGS"));
    assert.match(
      defaultBlock,
      new RegExp(`${feature}_enabled:\\s*false`),
      `DEFAULT 必须默认关闭: ${feature}`,
    );
  }
});

test("Rust 层同源：ProductFeature 三位 + feature_disabled 错误码", () => {
  const rust = readFileSync(path.join(hostSrc, "feature_flags.rs"), "utf8");
  for (const feature of FEATURES) {
    const capitalized = feature[0].toUpperCase() + feature.slice(1);
    assert.ok(rust.includes(capitalized), `ProductFeature 变体缺失: ${capitalized}`);
    assert.ok(rust.includes(`${feature}.feature_disabled`), `错误码缺失: ${feature}`);
  }
});

test("Browser 契约入口已接闸：读 features.toml 并 require(Browser)", () => {
  const commands = readFileSync(path.join(hostSrc, "browser", "commands.rs"), "utf8");
  const fnStart = commands.indexOf("pub fn browser_agent_contract");
  assert.ok(fnStart >= 0, "找不到 browser_agent_contract");
  const body = commands.slice(fnStart, fnStart + 700);
  assert.match(body, /FeatureFlagService::new/, "未加载能力开关");
  assert.match(body, /require\(ProductFeature::Browser\)/, "Browser 入口未过闸");
});

test("Dispatcher 注册面已接闸：register_browser_agent_tools 过 require(Browser)", () => {
  const gateway = readFileSync(path.join(hostSrc, "browser", "tool_gateway.rs"), "utf8");
  const fnStart = gateway.indexOf("pub fn register_browser_agent_tools");
  assert.ok(fnStart >= 0, "找不到 register_browser_agent_tools");
  const body = gateway.slice(fnStart, fnStart + 400);
  assert.match(body, /require\(ProductFeature::Browser\)/, "工具注册面未过闸");
});
