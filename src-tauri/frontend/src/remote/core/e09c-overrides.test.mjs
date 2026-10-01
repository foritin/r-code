/**
 * E09-C — remote override projection：canonical 行投影 + 坏行丢弃（失败
 * 关闭，绝不补默认值）+ 与桌面同一键序。node --test 直跑 TS。
 */
import test from "node:test";
import assert from "node:assert/strict";
import {
  emptyAggregation,
  OVERRIDE_MATERIAL_FIELDS,
  overrideAuthorityRows,
  projectUnverifiedOverrides,
} from "./approvals-aggregate.ts";

function row(overrides = {}) {
  return {
    overrideId: "ov-1",
    taskId: "t-1",
    candidateDigest: "sha256:c",
    actorId: "user",
    sessionId: "s",
    reason: "explicit risk acceptance",
    checks: ["check:a", "check:b"],
    createdAtMs: 42,
    ...overrides,
  };
}

test("E09-C: canonical 行完整投影", () => {
  const state = projectUnverifiedOverrides(emptyAggregation, [row()]);
  assert.equal(state.unverifiedOverrides.length, 1);
  const projected = state.unverifiedOverrides[0];
  for (const key of OVERRIDE_MATERIAL_FIELDS) {
    assert.equal(projected[key], row()[key]);
  }
  assert.deepEqual(projected.checks, ["check:a", "check:b"]);
  assert.equal(projected.createdAtMs, 42);
});

test("E09-C: 缺任何 canonical 键的行被丢弃，绝不默认", () => {
  const malformed = [
    {}, // nothing
    { ...row(), overrideId: "" }, // empty string is not authority
    { ...row(), taskId: 7 }, // wrong type
    { ...row(), checks: [] }, // an override without its exact checks proves nothing
    { ...row(), checks: "check:a" }, // checks must be the array
    { ...row(), createdAtMs: "42" }, // timestamp must be a number
    "not-even-an-object",
  ];
  const state = projectUnverifiedOverrides(emptyAggregation, malformed);
  assert.deepEqual(state.unverifiedOverrides, []);
  // Non-array answer never fabricates rows.
  const none = projectUnverifiedOverrides(emptyAggregation, { approvals: [row()] });
  assert.deepEqual(none.unverifiedOverrides, []);
});

test("E09-C: 好坏混排只留好行（逐行判别，非全有全无）", () => {
  const state = projectUnverifiedOverrides(emptyAggregation, [
    { ...row(), overrideId: "bad" , candidateDigest: null },
    row({ overrideId: "good-1" }),
    row({ overrideId: "good-2", checks: ["check:z"] }),
  ]);
  assert.deepEqual(
    state.unverifiedOverrides.map((entry) => entry.overrideId),
    ["good-1", "good-2"],
  );
});

test("E09-C: 展示行键序与桌面逐字一致（六列材料 + checks + createdAtMs）", () => {
  const keys = overrideAuthorityRows(row()).map((entry) => entry.key);
  assert.deepEqual(keys, [
    ...OVERRIDE_MATERIAL_FIELDS,
    "checks",
    "createdAtMs",
  ]);
  const checks = overrideAuthorityRows(row({ checks: ["check:b", "check:a"] })).find(
    (entry) => entry.key === "checks",
  );
  assert.equal(checks.value, "check:b, check:a");
});

test("E09-C: 聚合空态带空审计数组", () => {
  assert.deepEqual(emptyAggregation.unverifiedOverrides, []);
});
