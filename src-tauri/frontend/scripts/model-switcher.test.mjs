import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import ts from "typescript";

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
const capabilitiesModule = await import(`data:text/javascript;base64,${Buffer.from(
  ts.transpileModule(capabilities, {
    compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
  }).outputText,
).toString("base64")}`);

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

test("entering the model list keeps every provider group collapsed by default", () => {
  assert.match(switcher, /const openModels = \(\) => \{[^]*?setExpandedProvider\(null\);[^]*?setView\("models"\)/);
  assert.doesNotMatch(switcher, /setExpandedProvider\(active\.name/);
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

test("DeepSeek exposes an adaptive local strategy and model-specific native effort levels", () => {
  const provider = { name: "deepseek", kind: "deepseek" };
  const flash = capabilitiesModule.capabilitiesFor(provider, "deepseek-v4-flash");
  const pro = capabilitiesModule.capabilitiesFor(provider, "deepseek-v4-pro");

  assert.equal(flash.thinking.defaultValue, "adaptive");
  assert.deepEqual(flash.thinking.options.map(({ value }) => value), ["adaptive", "enabled", "disabled"]);
  assert.deepEqual(flash.reasoning.options.map(({ value }) => value), ["low", "high", "max"]);
  assert.deepEqual(pro.reasoning.options.map(({ value }) => value), ["high", "max"]);
  assert.equal(pro.reasoning.defaultLabel, "跟随智能平衡");
  assert.doesNotMatch(pro.note, /支持低/);
  assert.match(pro.note, /不会发送不受支持的低档/);
});

test("the adaptive default is shown once and remains compatible with empty inference", () => {
  assert.match(switcher, /!current \|\| current === control\?\.defaultValue/);
  assert.match(switcher, /chooseOption\(field, null\)/);
  assert.match(switcher, /filter\(\(option\) => option\.value !== control\.defaultValue\)/);
  assert.match(switcher, /capabilities\.thinking\?\.defaultValue[^]*恢复智能平衡/);
  assert.match(switcher, /normalized\.thinking !== capabilities\.thinking\?\.defaultValue/);

  const pro = capabilitiesModule.capabilitiesFor(
    { name: "deepseek", kind: "deepseek" },
    "deepseek-v4-pro",
  );
  assert.deepEqual(capabilitiesModule.normalizeInference({}, pro), {});
  assert.deepEqual(
    capabilitiesModule.normalizeInference({ thinking: "adaptive", reasoning_effort: "low" }, pro),
    { thinking: "adaptive" },
  );
  assert.deepEqual(
    capabilitiesModule.normalizeInference({ reasoning_effort: "max" }, pro),
    { thinking: "enabled", reasoning_effort: "max" },
  );
  assert.deepEqual(
    capabilitiesModule.normalizeInference({ thinking: "adaptive", reasoning_effort: "max" }, pro),
    { thinking: "enabled", reasoning_effort: "max" },
  );
  assert.equal(capabilitiesModule.inferenceSummary(pro, {}), "智能平衡（推荐）");
  assert.equal(
    capabilitiesModule.inferenceSummary(pro, { thinking: "adaptive", reasoning_effort: "max" }),
    "智能平衡（推荐） · 最大",
  );
});

test("DeepSeek fixed effort and smart balance cannot persist as an ambiguous pair", () => {
  assert.match(switcher, /field === "reasoning_effort"[^]*next\.thinking = "enabled"/);
  assert.match(switcher, /field === "reasoning_effort"[^]*delete next\.reasoning_effort[^]*delete next\.thinking/);
  assert.match(switcher, /field === "thinking"[^]*delete next\.reasoning_effort/);
});
