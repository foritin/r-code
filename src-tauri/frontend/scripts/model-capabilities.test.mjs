import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import ts from "typescript";

const frontendDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const capabilitiesSource = fs.readFileSync(
  path.join(frontendDir, "src/components/room/model-capabilities.ts"),
  "utf8",
);
const capabilities = await import(`data:text/javascript;base64,${Buffer.from(
  ts.transpileModule(capabilitiesSource, {
    compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
  }).outputText,
).toString("base64")}`);

const deepseekPresetModels = [
  { id: "deepseek-v4-flash", vision: false },
  { id: "deepseek-v4-pro", vision: false },
];
const anthropicPresetModels = [
  { id: "claude-sonnet-5", vision: true },
];

test("preset catalog annotation wins over the name heuristic in both directions", () => {
  // 目录标注 false 优先于启发式的 "deepseek → unsupported"：结论一致但来源权威。
  assert.equal(
    capabilities.resolveImageCapability("deepseek-v4-flash", {
      presetModels: deepseekPresetModels,
    }),
    "unsupported",
  );
  // 目录标注 true 优先于启发式可能给出的 unknown（新模型族首发布时最常见）。
  assert.equal(
    capabilities.resolveImageCapability("claude-sonnet-5", {
      presetModels: anthropicPresetModels,
    }),
    "supported",
  );
});

test("preset annotation must match the exact model id, not a prefix", () => {
  // 未命中目录的自定义模型回落启发式：claude 前缀仍判 supported。
  assert.equal(
    capabilities.resolveImageCapability("claude-opus-4-7-custom", {
      presetModels: anthropicPresetModels,
    }),
    "supported",
  );
  // 未命中目录且无启发式线索 → unknown（照发）。
  assert.equal(
    capabilities.resolveImageCapability("totally-unknown-model", {
      presetModels: anthropicPresetModels,
    }),
    "unknown",
  );
  // 无目录输入时同样走启发式。
  assert.equal(capabilities.resolveImageCapability("deepseek-v3-custom"), "unsupported");
  assert.equal(capabilities.resolveImageCapability("random-model"), "unknown");
});

test("imageCapabilityFor consumes presetModels injected through ProviderChoice", () => {
  const provider = {
    name: "deepseek",
    label: "DeepSeek",
    model: "deepseek-v4-flash",
    models: ["deepseek-v4-flash"],
    ready: true,
    presetModels: deepseekPresetModels,
  };
  // 目录命中：权威覆盖。
  assert.equal(
    capabilities.imageCapabilityFor(provider, "deepseek-v4-flash").state,
    "unsupported",
  );
  // 目录未命中：回落启发式（qwen-vl 家族 → supported）。
  assert.equal(
    capabilities.imageCapabilityFor(provider, "qwen-vl-max").state,
    "supported",
  );
  // 同一 provider 下未知模型 → unknown。
  assert.equal(
    capabilities.imageCapabilityFor(provider, "mystery-model").state,
    "unknown",
  );
});

test("modalityLabel renders badges only for confirmed capabilities", () => {
  assert.equal(capabilities.modalityLabel("supported"), "多模态");
  assert.equal(capabilities.modalityLabel("unsupported"), "文本");
  assert.equal(capabilities.modalityLabel("unknown"), null);
});
