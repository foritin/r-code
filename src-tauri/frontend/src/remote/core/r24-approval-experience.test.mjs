/**
 * R24 — 原生审批体验 core 投影：needs_desktop_confirm 态不可在手机终决
 * （显示引导）、无 decide 能力隐藏按钮（复用 capability-ui）、决策审计
 * 携带 device id、乐观更新回滚（复用 approvals-aggregate）。
 */
import test from "node:test";
import assert from "node:assert/strict";
import { mergePendingList, optimisticRemove, emptyAggregation } from "./approvals-aggregate.ts";
import { approvalCardUi, capabilitiesFromLabels } from "./capability-ui.ts";

/** needs_desktop_confirm 错误 → 用户引导（协议字段 R12 冻结）。 */
export function desktopConfirmGuidance(error) {
  const out = {
    isDesktopConfirm: false,
    headline: "",
    detail: "",
  };
  out.isDesktopConfirm = error.startsWith("needs_desktop_confirm");
  out.headline = out.isDesktopConfirm ? "需要在电脑上确认" : "决策失败";
  out.detail = out.isDesktopConfirm
    ? "这是一个高敏操作——请在电脑端完成批准。"
    : error;
  return out;
}

test("R24.A1: needs_desktop_confirm 态阻断手机终决并给出引导", () => {
  const g = desktopConfirmGuidance(
    "needs_desktop_confirm: this task requires a desktop approval",
  );
  assert.equal(g.isDesktopConfirm, true);
  assert.match(g.headline, /电脑上确认/);
  assert.match(g.detail, /高敏/);
  const generic = desktopConfirmGuidance("approval_conflict: op-1");
  assert.equal(generic.isDesktopConfirm, false);
});

test("R24.A1: 无 decide 能力隐藏决策按钮（复用 capability-ui）", () => {
  const readOnly = approvalCardUi(capabilitiesFromLabels(["events-read"]));
  assert.equal(readOnly.showDecideButtons, false);
  const full = approvalCardUi(
    capabilitiesFromLabels(["events-read", "tasks-write", "approvals-decide"]),
  );
  assert.equal(full.showDecideButtons, true);
});

test("R24.A2: 决策审计含设备 id（断言由 r19_a2 的 decidedBy 端到端锁定；此处锁聚合回滚）", () => {
  // 乐观移除 → 失败回滚（needs_desktop_confirm 时 UI 恢复卡片）。
  let state = mergePendingList(emptyAggregation, [
    { opId: "op-hi", summary: "high risk", taskId: "t1", createdSeq: 5 },
  ]);
  state = optimisticRemove(state, "op-hi");
  assert.equal(state.pending.length, 0);
  state = mergePendingList(state, [
    { opId: "op-hi", summary: "high risk", taskId: "t1", createdSeq: 5 },
  ]);
  assert.equal(state.pending.length, 1, "回滚恢复");
});
