// M1-01.A1（TS 侧）：同一共享 fixture 在前端 IPC 错误路径下得到相同 code/args，
// unknown code 走 errors.unknown 安全降级，不崩溃、不抛出。
// 与 crates/r-code-core/tests/user_error_contract.rs 保持断言等价。

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import process from "node:process";
import test from "node:test";

const scriptsDir = path.dirname(new URL(import.meta.url).pathname);
const frontendDir = path.resolve(scriptsDir, "..");
const repoRoot = path.resolve(frontendDir, "..", "..");
const zhCN = JSON.parse(
  readFileSync(path.join(frontendDir, "src", "i18n", "locales", "zh-CN.json"), "utf8"),
);

async function loadIpcErrorModule() {
  // i18n 初始化会触碰 window/document；Node 测试环境缺这些全局时退化为最小 stub。
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
  const rewritten = outputText
    .replace(/^import\s+\{[^}]*\}\s+from\s+"\.\.\/i18n";?$/m, `const t = (key, opts) => {
        const table = ${JSON.stringify(zhCN)};
        const resolved = key.split(".").reduce((acc, part) => (acc == null ? undefined : acc[part]), table);
        if (typeof resolved !== "string" || (opts && typeof opts.defaultValue === "string" && resolved === key)) {
          return opts && typeof opts.defaultValue === "string" ? opts.defaultValue : key;
        }
        return String(resolved);
      };`);
  const dataUrl = `data:text/javascript;base64,${Buffer.from(rewritten).toString("base64")}`;
  return import(dataUrl);
}

test("共享 fixture：payload → UserFacingIpcError 的 code/args 与 Rust 解析一致", async () => {
  const fixture = JSON.parse(
    readFileSync(
      path.join(repoRoot, "scripts", "product-experience", "fixtures", "user-error-cases.json"),
      "utf8",
    ),
  );
  const { toUserFacingIpcError } = await loadIpcErrorModule();

  for (const testCase of fixture.cases) {
    const error = toUserFacingIpcError(testCase.payload);
    assert.ok(error, `${testCase.name} 应被识别为 user-facing error`);
    assert.equal(error.code, testCase.payload.code, testCase.name);
    assert.deepEqual(error.args, testCase.payload.args ?? {}, testCase.name);
    assert.equal(error.debugDetail, testCase.payload.debug_detail ?? undefined, testCase.name);
  }
});

test("unknown code 不崩溃并降级为 errors.unknown 文案，code 仍保留原值", async () => {
  const fixture = JSON.parse(
    readFileSync(
      path.join(repoRoot, "scripts", "product-experience", "fixtures", "user-error-cases.json"),
      "utf8",
    ),
  );
  const unknown = fixture.cases.find((c) => c.name === "unknown_code");
  assert.ok(zhCN.errors?.unknown, "两套 locale 必须存在 errors.unknown 兜底键");
  const { toUserFacingIpcError } = await loadIpcErrorModule();
  const error = toUserFacingIpcError(unknown.payload);
  assert.ok(error instanceof Error && error.message.length > 0 && error.message !== unknown.payload.code);
  assert.equal(error.code, unknown.payload.code);
});

test("非结构化错误返回 null（保持既有普通 Error 语义）", async () => {
  const { toUserFacingIpcError } = await loadIpcErrorModule();
  assert.equal(toUserFacingIpcError("boom"), null);
});
