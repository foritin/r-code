/**
 * R07c.A1 — 审批聚合 node-test：多 op 排序、决策消失、重连 after_seq
 * 不重复不丢失。
 */
import test from "node:test";
import assert from "node:assert/strict";
import {
  mergePendingList,
  applyApprovalEvent,
  optimisticRemove,
  emptyAggregation,
} from "./approvals-aggregate.ts";

function envelope(seq, journalKind, payload, task = "t1") {
  return {
    seq,
    task_id: task,
    run_id: `run-${task}`,
    kind: "Progress",
    payload: { journalKind, ...payload },
  };
}

test("R07c.A1: 多 op 按 createdSeq 升序聚合", () => {
  let state = mergePendingList(emptyAggregation, [
    { opId: "op-c", summary: "later", taskId: "t2", createdSeq: 30 },
    { opId: "op-a", summary: "first", taskId: "t1", createdSeq: 10 },
    { opId: "op-b", summary: "mid", taskId: "t1", createdSeq: 20 },
  ]);
  assert.deepEqual(
    state.pending.map((row) => row.opId),
    ["op-a", "op-b", "op-c"],
  );
  // 坏行（无 opId）被丢弃。
  state = mergePendingList(state, [{ summary: "broken" }]);
  assert.equal(state.pending.length, 3);
});

test("R07c.A1: decided 事件移除对应 op；requested 去重添加", () => {
  let state = mergePendingList(emptyAggregation, [
    { opId: "op-a", summary: "a", taskId: "t1", createdSeq: 10 },
    { opId: "op-b", summary: "b", taskId: "t1", createdSeq: 20 },
  ]);
  state = applyApprovalEvent(state, envelope(31, "approval.decided", { opId: "op-a" }));
  assert.deepEqual(
    state.pending.map((row) => row.opId),
    ["op-b"],
  );
  assert.equal(state.lastSeq, 31);
  // 新请求经事件流加入（不重复）。
  state = applyApprovalEvent(state, envelope(32, "approval.requested", { opId: "op-d", summary: "d" }, "t3"));
  state = applyApprovalEvent(state, envelope(33, "approval.requested", { opId: "op-d", summary: "d" }, "t3"));
  assert.equal(state.pending.filter((row) => row.opId === "op-d").length, 1);
});

test("R07c.A1: 重连 after_seq 重放不重复不丢失", () => {
  // 第一段连接消费到 seq 35。
  let state = applyApprovalEvent(emptyAggregation, envelope(30, "approval.requested", { opId: "op-1", summary: "one" }, "t1"));
  state = applyApprovalEvent(state, envelope(35, "approval.requested", { opId: "op-2", summary: "two" }, "t2"));
  assert.equal(state.pending.length, 2);

  // 断线重连：服务端从 afterSeq=35 重放 33..36（含已消费的 33 旧区间）
  const replay = [
    envelope(33, "approval.requested", { opId: "op-0", summary: "stale" }, "t1"),
    envelope(36, "approval.decided", { opId: "op-1" }),
  ];
  for (const event of replay) {
    state = applyApprovalEvent(state, event);
  }
  // seq<=35 的事件被跳过（不重复）；36 生效（不丢失）。
  assert.equal(state.pending.some((row) => row.opId === "op-0"), false, "重放不重复生效");
  assert.equal(state.pending.some((row) => row.opId === "op-1"), false, "decided 丢失即失败");
  assert.equal(state.pending.some((row) => row.opId === "op-2"), true);
  assert.equal(state.lastSeq, 36);
});

test("R07c: 乐观移除与列表回滚", () => {
  let state = mergePendingList(emptyAggregation, [
    { opId: "op-x", summary: "x", taskId: "t1", createdSeq: 5 },
  ]);
  state = optimisticRemove(state, "op-x");
  assert.equal(state.pending.length, 0);
  // 决策失败：重新拉列表回滚。
  state = mergePendingList(state, [
    { opId: "op-x", summary: "x", taskId: "t1", createdSeq: 5 },
  ]);
  assert.equal(state.pending.length, 1);
});
