// M1-01.A3：debug_detail / secret 不得进入普通 DOM、log 与通知。
// 扫描规则：`debugDetail`/`debug_detail` 与 `copyTechnicalDetail` 只允许出现在
// 白名单文件（IPC 错误适配层）；其余前端源码命中即失败。普通 UI 若需要技术
// 详情，必须走显式的"复制诊断"交互，而不是渲染进 message/toast/notification。

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const frontendDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const sourceDir = path.join(frontendDir, "src");

const ALLOWLIST_FILES = new Set([
  path.join(sourceDir, "lib", "ipc-error.ts"),
]);

const FORBIDDEN_PATTERNS = [
  [/\.debugDetail\b/, "直接引用 debugDetail"],
  [/\bdebug_detail\b/, "直接引用 debug_detail 字段"],
  [/copyTechnicalDetail\(/, "复制技术详情入口被越权调用"],
];

function* walkTsFiles(dir) {
  for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
    const abs = path.join(dir, entry.name);
    if (entry.isDirectory()) yield* walkTsFiles(abs);
    else if (/\.tsx?$/.test(entry.name)) yield abs;
  }
}

test("debug_detail 仅存在于 IPC 错误适配层，未泄漏到 UI/日志/通知", () => {
  const violations = [];
  for (const file of walkTsFiles(sourceDir)) {
    if (ALLOWLIST_FILES.has(file)) continue;
    const lines = fs.readFileSync(file, "utf8").split("\n");
    lines.forEach((line, index) => {
      for (const [pattern, reason] of FORBIDDEN_PATTERNS) {
        if (pattern.test(line)) {
          violations.push(
            `${path.relative(frontendDir, file)}:${index + 1} ${reason}: ${line.trim().slice(0, 120)}`,
          );
        }
      }
    });
  }
  assert.deepEqual(violations, []);
});

test("白名单层确实解析 debug_detail（防止有人顺手删掉能力导致扫描空转）", () => {
  const adapter = fs.readFileSync(ALLOWLIST_FILES.values().next().value, "utf8");
  assert.match(adapter, /debug_detail/);
  assert.match(adapter, /copyTechnicalDetail/);
});
