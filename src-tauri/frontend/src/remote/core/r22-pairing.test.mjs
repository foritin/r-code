/**
 * R22.A1 — 配对载荷解析与错误态 node-test（与 Rust parse_qr_payload
 * 同一契约）；R22.A2 的安全存储断言=PlatformAdapter 接口 + adapter
 * 骨架归属（真机取证归外部放行清单）。
 */
import test from "node:test";
import assert from "node:assert/strict";
import {
  parsePairPayload,
  cameraPermissionOutcome,
  pairingFailureCopy,
} from "./pairing.ts";

test("R22.A1: rcode://pair v1 载荷解析（与手动码等效）", () => {
  const payload = parsePairPayload(
    "rcode://pair?v=1&h=192.168.1.10&p=8787&s=the-secret&fp=" + "ab".repeat(32),
  );
  assert.deepEqual(payload, {
    version: 1,
    host: "192.168.1.10",
    port: 8787,
    pairSecret: "the-secret",
    fingerprint: "ab".repeat(32),
  });
});

test("R22.A1: 畸形载荷全拒（fail closed）", () => {
  for (const bad of [
    "https://x/pair?v=1",
    "rcode://pair?v=2&h=x&p=1&s=y&fp=" + "ab".repeat(32),
    "rcode://pair?v=1&p=1&s=y&fp=" + "ab".repeat(32),
    "rcode://pair?v=1&h=x&p=0&s=y&fp=" + "ab".repeat(32),
    "rcode://pair?v=1&h=x&p=1&s=&fp=" + "ab".repeat(32),
    "rcode://pair?v=1&h=x&p=1&s=y&fp=nothex",
    "rcode://pair?v=1&h=x&p=1&s=y&fp=" + "ab".repeat(32) + "&extra=1",
  ]) {
    assert.equal(parsePairPayload(bad), null, `必须拒绝：${bad}`);
  }
});

test("R22.A1: 相机权限拒绝降级手动输入（不阻塞配对）", () => {
  const denied = cameraPermissionOutcome(false);
  assert.equal(denied.canScan, false);
  assert.match(denied.guidance, /手动输入/);
  assert.ok(pairingFailureCopy["expired"].includes("重新开始配对"));
});
