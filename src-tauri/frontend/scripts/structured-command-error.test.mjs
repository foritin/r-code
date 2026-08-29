// FX-11（前端侧）：结构化命令错误 `{ code, message }` 经 commandErrorPayload
// 识别为 IpcCommandError；历史纯字符串错误保持原路径（返回 null，不误包）。
// 与 src-tauri/src/tauri_commands.rs 的 CommandError 序列化形状对齐；
// 加载方式沿用 user-error-contract.test.mjs 的 TS 转译模式。

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import process from "node:process";
import test from "node:test";

const scriptsDir = path.dirname(fileURLToPath(import.meta.url));
const frontendDir = path.resolve(scriptsDir, "..");

async function loadIpcErrorModule() {
  if (typeof globalThis.window === "undefined") {
    globalThis.window = { location: { protocol: "http:" } };
    globalThis.document = {
      documentElement: { lang: "", dir: "" },
      addEventListener() {},
    };
    globalThis.navigator ??= { languages: ["zh-CN"] };
  }
  const moduleUrl = new URL("../src/lib/ipc-error.ts", import.meta.url);
  const ts = await import("typescript");
  const source = readFileSync(moduleUrl, "utf8");
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
  });
  // i18n 依赖替换为最小查表 stub（与 user-error-contract 同款策略）。
  const rewritten = outputText.replace(
    /^import\s+\{[^}]*\}\s+from\s+"\.\.\/i18n";?$/m,
    `const t = (key) => key;`
  );
  const dataUrl = `data:text/javascript;base64,${Buffer.from(rewritten).toString("base64")}`;
  return import(dataUrl);
}

test("structured { code, message } errors are recognized and wrapped", async () => {
  const { commandErrorPayload } = await loadIpcErrorModule();
  const payload = commandErrorPayload({
    code: "database_error",
    message: "database error: locked",
  });
  assert.deepEqual(payload, {
    code: "database_error",
    message: "database error: locked",
  });
});

test("legacy string errors are not wrapped (compat path preserved)", async () => {
  const { commandErrorPayload } = await loadIpcErrorModule();
  assert.equal(commandErrorPayload("会话丢失: t-1"), null);
  assert.equal(commandErrorPayload(new Error("plain")), null);
});

test("objects missing a string code or message are rejected", async () => {
  const { commandErrorPayload } = await loadIpcErrorModule();
  assert.equal(commandErrorPayload({ message: "no code" }), null);
  assert.equal(commandErrorPayload({ code: 7, message: "numeric code" }), null);
  assert.equal(commandErrorPayload(null), null);
});

test("numeric limit survives when present (conversation-limit contract)", async () => {
  const { commandErrorPayload } = await loadIpcErrorModule();
  assert.deepEqual(
    commandErrorPayload({
      code: "PROJECT_CONVERSATION_LIMIT_REACHED",
      message: "该项目最多保留 8 个未归档对话，请先归档一个后再新建",
      limit: 8,
    }),
    {
      code: "PROJECT_CONVERSATION_LIMIT_REACHED",
      message: "该项目最多保留 8 个未归档对话，请先归档一个后再新建",
      limit: 8,
    }
  );
});

test("user-facing i18n objects (no message) are left to userFacingErrorPayload", async () => {
  const { commandErrorPayload } = await loadIpcErrorModule();
  // CommandError 的 UserFacing 形状刻意不带 message：commandErrorPayload
  // 必须拒绝它，让 userFacingErrorPayload 的查表通道接管。
  assert.equal(
    commandErrorPayload({ code: "automation.feature_disabled", args: {} }),
    null
  );
});

// 防止 process 未用到导致 lint 噪音（保持与姊妹测试文件一致的导入面）。
void process;
