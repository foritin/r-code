/**
 * S19B-C — remote effect approval projection (fail-closed) and capability
 * gating. The remote client must display the SAME authority the daemon
 * froze: both active and superseded rows are kept for audit, a row missing
 * any canonical key is DROPPED rather than defaulted (a projection must
 * never fabricate authority), and an absent effects-manage capability
 * yields no control at all — hidden, not merely disabled.
 */
import test from "node:test";
import assert from "node:assert/strict";
import {
  EFFECT_MATERIAL_FIELDS,
  effectAuthorityRows,
  effectPendingRows,
  emptyAggregation,
  projectEffectApprovals,
} from "./approvals-aggregate.ts";
import { capabilitiesFromLabels, effectApprovalUi } from "./capability-ui.ts";

const MATERIAL = {
  taskId: "task-s19bc",
  planRevision: "rev-1",
  workUnitId: "unit-shell",
  effectClass: "workspace-mutation",
  network: "public-internet-client",
  payloadHash: "sha256:abc",
};

const approval = (id, state, extra = {}) => ({
  ...MATERIAL,
  approvalId: id,
  actorId: "actor-x",
  sessionId: `cmd-${id}`,
  scope: "effect.approve",
  state,
  createdAtMs: 1_700_000_000_000,
  ...extra,
});

const answer = (approvals, pending = [], taskId = "task-s19bc") => ({
  taskId,
  pending,
  approvals,
});

test("S19B-C.A1 both active and superseded rows are kept for audit", () => {
  const state = projectEffectApprovals(
    emptyAggregation,
    answer([approval("op-1", "active"), approval("op-2", "superseded", { supersededAtMs: 1_700_000_000_001 })]),
  );
  assert.equal(state.effectApprovals.length, 2, "a revoke supersedes, never deletes");
  assert.deepEqual(state.effectApprovals.map((row) => row.approvalId), ["op-1", "op-2"]);
  assert.deepEqual(state.effectApprovals.map((row) => row.state), ["active", "superseded"]);
  // The optional field is only carried when the daemon actually sent it.
  assert.equal(state.effectApprovals[0].supersededAtMs, undefined);
  assert.equal(state.effectApprovals[1].supersededAtMs, 1_700_000_000_001);
  // Every canonical key survives the projection byte-for-byte.
  for (const key of EFFECT_MATERIAL_FIELDS) {
    assert.equal(state.effectApprovals[0][key], MATERIAL[key], `material key ${key}`);
  }
  assert.equal(state.effectApprovals[0].scope, "effect.approve");
  assert.equal(state.effectApprovals[0].actorId, "actor-x");
  assert.equal(state.effectApprovals[0].sessionId, "cmd-op-1");
});

test("S19B-C.A2 a row missing any canonical key is dropped, not defaulted", () => {
  const identityKeys = ["approvalId", "actorId", "sessionId", "scope", "state", "createdAtMs"];
  const required = [...EFFECT_MATERIAL_FIELDS, ...identityKeys];
  for (const key of required) {
    const broken = approval("op-bad", "active");
    delete broken[key];
    const state = projectEffectApprovals(emptyAggregation, answer([approval("op-ok", "active"), broken]));
    assert.deepEqual(
      state.effectApprovals.map((row) => row.approvalId),
      ["op-ok"],
      `a row missing ${key} must be dropped, never filled with a default`,
    );
  }
  // A mistyped material value, an unknown state and a non-object row are all
  // equally non-authoritative.
  for (const broken of [
    { ...approval("op-bad", "active"), network: 42 },
    { ...approval("op-bad", "granted") },
    { ...approval("op-bad", "active"), createdAtMs: "1700000000000" },
    null,
    "not-a-row",
  ]) {
    const state = projectEffectApprovals(emptyAggregation, answer([broken]));
    assert.deepEqual(state.effectApprovals, [], "malformed rows never become authority");
  }
  // An answer without a taskId is not this task's snapshot: state is untouched.
  const seeded = projectEffectApprovals(emptyAggregation, answer([approval("op-1", "active")]));
  assert.deepEqual(
    projectEffectApprovals(seeded, { approvals: [approval("op-x", "active")] }).effectApprovals,
    seeded.effectApprovals,
    "a taskless answer must not overwrite an existing projection",
  );
  assert.equal(projectEffectApprovals(seeded, null).effectApprovals.length, 1);
  assert.equal(projectEffectApprovals(seeded, undefined).effectApprovals.length, 1);
});

test("S19B-C.A3 pending requests need the same six keys plus operationId", () => {
  const pending = {
    ...MATERIAL,
    operationId: "op-pending",
    summary: "effect approval for unit-shell",
    createdSeq: 3,
    createdMs: 1_700_000_000_000,
    ageMs: 12,
  };
  const state = projectEffectApprovals(emptyAggregation, answer([approval("op-1", "active")], [pending, { ...pending, network: null }]));
  assert.deepEqual(state.effectPending.map((row) => row.operationId), ["op-pending"]);
  // The remote row builder uses the same frozen key order as the desktop.
  assert.deepEqual(
    effectAuthorityRows(state.effectApprovals[0]).slice(0, 6).map((row) => row.key),
    [...EFFECT_MATERIAL_FIELDS],
  );
  assert.deepEqual(effectPendingRows(state.effectPending[0]).map((row) => row.key), [
    ...EFFECT_MATERIAL_FIELDS,
    "operationId",
  ]);
});

test("S19B-C.B1 an absent effect capability yields no control, not a disabled one", () => {
  const without = capabilitiesFromLabels(["events-read", "tasks-write", "approvals-decide"]);
  assert.equal(without.effectsManage, false, "approvals-decide never implies effects-manage");
  const locked = effectApprovalUi(without, true);
  assert.equal(locked.canManage, false, "no request control");
  assert.equal(locked.canRevoke, false, "no revoke control");
  assert.equal(locked.readOnlyBadge, "只读设备 · 不可请求或撤销授权");
  // Deciding an existing pending op is a different, still-granted scope.
  assert.equal(locked.canDecide, true);

  // With the capability the control appears, and revoke needs a live grant.
  const with_ = effectApprovalUi(capabilitiesFromLabels(["effects-manage"]), true);
  assert.equal(with_.canManage, true);
  assert.equal(with_.canRevoke, true);
  assert.equal(with_.readOnlyBadge, null);
  const noActive = effectApprovalUi(capabilitiesFromLabels(["effects-manage"]), false);
  assert.equal(noActive.canManage, true);
  assert.equal(noActive.canRevoke, false, "nothing active means nothing to revoke");
});

test("S19B-C.B2 the standing semantics are stated in every capability combination", () => {
  // They describe frozen behaviour, not a permission, so they never vanish.
  for (const labels of [[], ["events-read"], ["effects-manage"]]) {
    const ui = effectApprovalUi(capabilitiesFromLabels(labels), true);
    assert.match(ui.revokeWarning, /仅影响未来运行/);
    assert.match(ui.reauthorizeHint, /终身不复用/);
    assert.match(ui.decisionHint, /拒绝或过期不会留下任何授权记录/);
  }
});
