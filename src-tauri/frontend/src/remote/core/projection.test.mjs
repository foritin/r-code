/**
 * R07.A1 — remote core 投影器单测（node --test 直跑 TS：strip-types）。
 * 断言与桌面 transcript 投影同一判别口径（payload.journalKind）。
 */
import test from "node:test";
import assert from "node:assert/strict";
import { projectEvents, applyEvent, emptyProjection } from "./projection.ts";

function envelope(seq, journalKind, payload = {}) {
  return {
    seq,
    task_id: "t1",
    run_id: "run-t1-1",
    kind: "Progress",
    payload: { journalKind, ...payload },
  };
}

test("R07.A1: 事件序列投影出正确的用户/助手/工具/状态行", () => {
  const state = projectEvents([
    envelope(1, "task.created"),
    envelope(2, "run.started"),
    envelope(3, "input.queued", { text: "跑一下测试", message_id: "m1" }),
    envelope(4, "assistant.message", { text: "好的，正在跑 cargo test" }),
    envelope(5, "tool.call", { name: "bash" }),
    envelope(6, "tool.result", { name: "bash", ok: true }),
    envelope(7, "run.completed", { verdict: "unverified" }),
  ]);
  assert.equal(state.rows.length, 5, "started+user+assistant+tool+completed");
  assert.equal(state.rows[0].type, "state");
  assert.deepEqual(state.rows[1], { type: "user", runId: "run-t1-1", text: "跑一下测试" });
  assert.deepEqual(state.rows[2], {
    type: "assistant",
    runId: "run-t1-1",
    text: "好的，正在跑 cargo test",
  });
  assert.equal(state.rows[3].type, "tool");
  assert.equal(state.rows[3].ok, true, "tool.result 关联最近同名工具行");
  assert.equal(state.running, false, "run.completed 置闲");
  assert.equal(state.rows[4].tone, "ok");
});

test("R07.A1: 审批事件驱动 pendingApproval 状态", () => {
  let state = emptyProjection();
  state = applyEvent(state, envelope(1, "approval.requested", { opId: "op-9", summary: "run tests" }));
  assert.deepEqual(state.pendingApproval, { opId: "op-9", summary: "run tests" });
  state = applyEvent(state, envelope(2, "approval.decided", { opId: "op-9", decision: "granted" }));
  assert.equal(state.pendingApproval, null);
});

test("R07.A1: 未知 journalKind 与空文本不产生行（向前兼容）", () => {
  const state = projectEvents([
    envelope(1, "model.usage", { usage: { input_tokens: 3 } }),
    envelope(2, "harness.progress", { note: "…" }),
    envelope(3, "assistant.message", {}),
    envelope(4, "future.kind.v2", {}),
  ]);
  assert.deepEqual(state.rows, []);
  assert.equal(state.running, false);
});

test("R07.A1: 失败与取消的终态色调", () => {
  const failed = projectEvents([
    envelope(1, "run.started"),
    envelope(2, "run.failed", { error: "provider 未配置" }),
  ]);
  assert.equal(failed.running, false);
  assert.equal(failed.rows.at(-1).tone, "fail");
  assert.match(failed.rows.at(-1).label, /provider 未配置/);
});
