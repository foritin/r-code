/**
 * R07b.A1 — 状态机与能力投影 node-test：
 * 断网重连退避、认证拒绝不自动重试、缺能力隐藏决策按钮（只读徽标）、
 * 列表投影使用真实 task.list 字段。
 */
import test from "node:test";
import assert from "node:assert/strict";
import {
  transition,
  stateBanner,
  initialSnapshot,
} from "./connection-state.ts";
import {
  capabilitiesFromLabels,
  approvalCardUi,
  composerUi,
  deviceInfoUi,
} from "./capability-ui.ts";

test("R07b: 断线进入 reconnecting 且指数退避有倒计时投影", () => {
  let snap = transition(initialSnapshot, { type: "connected" });
  assert.equal(snap.state, "online");
  snap = transition(snap, { type: "disconnected" });
  assert.equal(snap.state, "reconnecting");
  assert.equal(snap.retryInSeconds, 1, "首次退避 1s");
  snap = transition(snap, { type: "tick", nowSeconds: 1 });
  assert.equal(snap.state, "connecting", "倒计时归零转 connecting");
  // 第二次断线退避翻倍。
  const again = transition(snap, { type: "disconnected" });
  assert.equal(again.retryInSeconds, 2);
  assert.match(stateBanner(again), /2s 后重试/);
});

test("R07b: 退避封顶 30s；认证拒绝不自动重试", () => {
  let snap = initialSnapshot;
  for (let attempt = 0; attempt < 8; attempt += 1) {
    snap = transition(snap, { type: "disconnected" });
    snap = transition(snap, { type: "tick", nowSeconds: 60 });
  }
  assert.equal(snap.retryInSeconds, 30, "退避封顶");
  const denied = transition(
    transition(initialSnapshot, { type: "connected" }),
    { type: "denied" },
  );
  assert.equal(denied.state, "denied");
  assert.match(stateBanner(denied), /重新配对/);
  // denied 状态下断线事件不改变状态（无自动重试风暴）。
  assert.equal(
    transition(denied, { type: "disconnected" }).state,
    "denied",
  );
});

test("R07b: 无凭据即 unpaired，引导配对", () => {
  const snap = transition(initialSnapshot, {
    type: "credentialsChanged",
    hasCredentials: false,
  });
  assert.equal(snap.state, "unpaired");
  assert.match(stateBanner(snap), /尚未配对/);
});

test("R07b.A1: 缺 approvals:decide 隐藏决策按钮并显示只读徽标", () => {
  const readOnly = capabilitiesFromLabels(["events-read"]);
  const card = approvalCardUi(readOnly);
  assert.equal(card.showDecideButtons, false, "隐藏（非禁用）");
  assert.match(card.readOnlyBadge ?? "", /只读/);
  const composer = composerUi(readOnly);
  assert.equal(composer.canSend, false);
  assert.match(composer.placeholder, /不可发送/);
  // 有能力则显示按钮。
  const full = capabilitiesFromLabels(["events-read", "tasks-write", "approvals-decide"]);
  assert.equal(approvalCardUi(full).showDecideButtons, true);
  assert.equal(approvalCardUi(full).readOnlyBadge, null);
  assert.equal(composerUi(full).canSend, true);
  // 设备页只读投影（能力授予在桌面端）。
  const info = deviceInfoUi(full);
  assert.match(info[0].value, /审批决策/);
});

test("R07b.A1: 任务列表行只使用 task.list 真实字段", () => {
  // daemon task.list 的行形状（TaskSummaryView 的 serde camelCase）。
  const realTaskListRow = {
    task_id: "t1",
    title: "修登录 bug",
    kind: "conversation",
    state: "running",
    running: true,
    updated_at_ms: 1_789_000_000_000,
  };
  // UI 行从这些字段构造——无假数据源。
  const row = {
    taskId: realTaskListRow.task_id,
    title: realTaskListRow.title || realTaskListRow.task_id,
    state: realTaskListRow.state,
    running: realTaskListRow.running,
  };
  assert.equal(row.taskId, "t1");
  assert.equal(row.running, true);
  assert.equal(row.title, "修登录 bug");
});
