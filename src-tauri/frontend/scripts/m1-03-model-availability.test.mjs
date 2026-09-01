// M1-03：ModelAvailability 三态快照——前端消费语义。
// 共享视图合同——模型选择面只渲染 available；快照未覆盖不臆造过滤；
// composition_errors 逐条可诊断（provider/model/reason）；设置页接线存在。

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import ts from "typescript";

const frontendDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

async function loadModule() {
  const source = readFileSync(
    path.join(frontendDir, "src", "lib", "model-availability.ts"),
    "utf8",
  );
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
  });
  return import(`data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`);
}

const snapshot = {
  all: [
    { provider: "deepseek", model: "deepseek-v4", source: "catalog", has_auth: true },
    { provider: "relay-nokey", model: "relay-alpha", source: "config", has_auth: false },
  ],
  available: [
    { provider: "deepseek", model: "deepseek-v4", source: "catalog", has_auth: true },
  ],
  composition_errors: [
    { provider: "broken-decl", model: null, reason: "api 'grpc' 不是受支持的协议 slug" },
    { provider: "relay-nokey", model: "relay-alpha", reason: "$ENV:RELAY_KEY 未设置，缺鉴权" },
  ],
};

test("模型选择面只渲染 available：缺鉴权服务退出，未覆盖服务保持", async () => {
  const m = await loadModule();
  const choices = [
    { name: "deepseek" },
    { name: "relay-nokey" }, // 覆盖但无 available 条目 → 退出
    { name: "legacy-not-in-snapshot" }, // 快照未覆盖 → 保持（不臆造过滤）
  ];
  const filtered = m.dropUnavailableProviders(choices, snapshot);
  assert.deepEqual(
    filtered.map((c) => c.name),
    ["deepseek", "legacy-not-in-snapshot"],
  );
});

test("快照为 null（旧后端）时不过滤", async () => {
  const m = await loadModule();
  const choices = [{ name: "a" }, { name: "b" }];
  assert.equal(m.dropUnavailableProviders(choices, null).length, 2);
});

test("composition_errors 逐条展开为诊断清单", async () => {
  const m = await loadModule();
  const diagnostics = m.compositionDiagnostics(snapshot);
  assert.equal(diagnostics.length, 2);
  assert.equal(diagnostics[0].provider, "broken-decl");
  assert.equal(diagnostics[0].model, null);
  assert.ok(diagnostics[0].reason.includes("协议"));
  assert.equal(diagnostics[1].provider, "relay-nokey");
  assert.equal(diagnostics[1].model, "relay-alpha");
  assert.deepEqual(m.compositionDiagnostics(null), []);
});

test("接线存在：provider.ts 过滤 + SettingsScene 诊断区 + IPC/types 合同", async () => {
  const providerSource = readFileSync(
    path.join(frontendDir, "src", "lib", "provider.ts"),
    "utf8",
  );
  assert.ok(
    providerSource.includes("dropUnavailableProviders(rawChoices, availability)"),
    "模型选择面必须经 dropUnavailableProviders 过滤",
  );
  const sceneSource = readFileSync(
    path.join(frontendDir, "src", "components", "scenes", "SettingsScene.tsx"),
    "utf8",
  );
  assert.ok(
    sceneSource.includes("composition-diagnostics"),
    "设置页必须有可展开的组装诊断区",
  );
  assert.ok(
    sceneSource.includes("compositionDiagnostics(availability)"),
    "诊断区必须消费三态快照",
  );
  const ipcSource = readFileSync(path.join(frontendDir, "src", "lib", "ipc.ts"), "utf8");
  assert.ok(ipcSource.includes("cmd_model_availability"));
  const typesSource = readFileSync(path.join(frontendDir, "src", "lib", "types.ts"), "utf8");
  for (const key of ["ModelAvailabilitySnapshot", "ModelCompositionError", "has_auth"]) {
    assert.ok(typesSource.includes(key), `types.ts 缺 ${key}`);
  }
});
