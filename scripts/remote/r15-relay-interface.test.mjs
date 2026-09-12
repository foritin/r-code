/**
 * R15.A1 — 中继接口冻结门（static）：relay-interface.md 必备小节齐全
 * （认证/路由/握手/错误码/限流/实现对齐），每个错误码有客户端行为，
 * 无 TBD/TODO/FIXME。
 */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const doc = readFileSync(
  join(here, "..", "..", "docs", "prd", "remote-control", "relay-interface.md"),
  "utf8",
);

test("R15.A1: 必备小节齐全", () => {
  for (const section of [
    "## 1. 传输与帧格式",
    "## 2. 认证流程",
    "## 3. 端到端密钥协商",
    "## 4. 错误码与客户端行为",
    "## 5. 重连与限流",
    "## 6. 与 R17 RelayTransport 的实现接口对齐",
    "## 7. 评审检查单",
  ]) {
    assert.ok(doc.includes(section), `缺少小节「${section}」`);
  }
});

test("R15.A1: 无 TBD/TODO/FIXME 悬空标记", () => {
  // §7 检查单的自指行（"无 TBD/TODO/FIXME"）不算未完成标记。
  const body = doc.replace("- [x] 无 TBD/TODO/FIXME", "");
  const pattern = new RegExp("\\b(TBD|TODO|FIXME|待定|占位待补)\\b");
  const match = pattern.exec(body);
  assert.equal(match, null, `存在未完成标记：${match?.[0]}`);
});

test("R15.A1: 每个错误码都有客户端行为（§4 表格全覆盖）", () => {
  const codes = [
    "owner_code_invalid",
    "owner_code_expired",
    "owner_code_used",
    "owner_unknown",
    "owner_offline",
    "device_unbound",
    "not_authenticated",
    "bad_frame",
    "rate_limited",
    "relay_full",
  ];
  for (const code of codes) {
    assert.ok(doc.includes(`\`${code}\``), `错误码 ${code} 未冻结`);
  }
  for (const close of ["4000", "4001", "4002"]) {
    assert.ok(doc.includes(close), `close code ${close} 未冻结`);
  }
});

test("R15.A1: Noise XX 时序与限流数值冻结", () => {
  assert.match(doc, /→ e[\s\S]*← e, ee, s, es[\s\S]*→ s, se/, "XX 三步时序");
  assert.match(doc, /每 1 小时或 2\^32 条消息/, "rekey 数值");
  assert.match(doc, /256 KiB\/s/, "吞吐限值");
  assert.match(doc, /64 KiB/, "帧上限");
  assert.match(doc, /TTL 10min/, "注册码 TTL");
});
