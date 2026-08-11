import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const frontendDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const switcher = fs.readFileSync(
  path.join(frontendDir, "src/components/room/ModelSwitcher.tsx"),
  "utf8",
);
const menu = fs.readFileSync(path.join(frontendDir, "src/components/ui/Menu.tsx"), "utf8");
const home = fs.readFileSync(
  path.join(frontendDir, "src/components/scenes/HomeScene.tsx"),
  "utf8",
);
const capabilities = fs.readFileSync(
  path.join(frontendDir, "src/components/room/model-capabilities.ts"),
  "utf8",
);

test("model picker only groups configured providers and prioritizes the current provider", () => {
  assert.match(switcher, /\.filter\(\(choice\) => choice\.ready\)/);
  assert.match(switcher, /right\.name === active\.name/);
  assert.match(switcher, /model-current-provider-badge[^]*当前使用/);
  assert.doesNotMatch(switcher, /默认 Provider/);
});

test("provider groups are single-expand sections without inline custom-model entry", () => {
  assert.match(switcher, /expandedProvider === choice\.name/);
  assert.match(switcher, /aria-expanded=\{expanded\}/);
  assert.match(switcher, /setExpandedProvider\(expanded \? null : choice\.name\)/);
  assert.match(switcher, /\{expanded && \([^]*className="model-group-body"/);
  assert.doesNotMatch(switcher, /添加自定义模型|model-custom|customFor|customValue/);
});

test("provider default remains a null model binding", () => {
  assert.match(switcher, /model: string \| null/);
  assert.match(switcher, /taskSetModel\(taskId, nextModel\)/);
  assert.match(switcher, /使用服务默认模型/);
  assert.match(switcher, /onSelect=\{\(\) => chooseModel\(choice, null\)\}/);
  assert.match(home, /<ModelSwitcher[^]*model=\{draftModel\}/);
  assert.doesNotMatch(home, /<ModelSwitcher[^]*model=\{activeModel \|\| null\}/);
});

test("only an existing conversation confirms a cross-provider switch", () => {
  assert.match(switcher, /if \(!taskId \|\| provider\.name === active\.name\)/);
  assert.match(switcher, /setPending\(\{ provider, model: nextModel \}\)/);
});

test("ready styling and radio checks reflect actual state", () => {
  assert.match(switcher, /active\.provider\?\.ready \? " ready" : ""/);
  assert.match(menu, /className="menu-item-check"/);
  assert.match(menu, /checked && <IconCheck/);
});

test("Kimi providers expose thinking and reasoning controls", () => {
  assert.match(capabilities, /providerKind === "kimi" \|\| providerKind === "kimi_coding"/);
  assert.match(capabilities, /thinking: \{ label: "思考模式", options: THINKING/);
  assert.match(capabilities, /reasoning: effort\(\["low", "medium", "high"\]\)/);
});
