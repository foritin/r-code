// P31 — tests for the safety conformance gate runner.
//
// The report builder and redaction are the deterministic core: identical
// inputs produce identical digests, SafeDisabled targets never relabel
// themselves Activated, native-CI-only targets are honestly separated, and
// the redaction strips credential-shaped material from every string.

import { strict as assert } from "node:assert";
import { test } from "node:test";
import { buildReport, digestOf, redact } from "./verify-safety-v1.mjs";

function green(name) {
  return { name, ok: true, seconds: "0.1", tail: ["test result: ok. 5 passed"] };
}
function red(name) {
  return { name, ok: false, seconds: "0.1", tail: ["test result: FAILED. 4 passed; 1 failed"] };
}

test("identical inputs produce identical report digests (determinism)", () => {
  const input = {
    suites: [green("s04"), green("s05")],
    guard: { ok: true, tail: ["5 areas clean"] },
    packaging: { ok: true, tail: ["lockstep"] },
  };
  const first = buildReport(input);
  const second = buildReport(structuredClone(input));
  assert.deepEqual(first, second);
  for (const target of first.targets) {
    assert.match(target.digest, /^[0-9a-f]{16}$/);
  }
});

test("a failing suite is SafeDisabled and never relabels Activated", () => {
  const report = buildReport({
    suites: [green("s04"), red("s05")],
    guard: { ok: true, tail: [] },
    packaging: { ok: true, tail: [] },
  });
  const failed = report.targets.find((t) => t.target === "s05");
  assert.equal(failed.verdict, "SafeDisabled");
  assert.ok(failed.reason.includes("FAILED"));
  // s04 + guard + packaging are Activated; the three native-only targets
  // and the failing s05 are SafeDisabled.
  assert.equal(report.summary.activated, 3);
  assert.ok(
    report.targets.filter((t) => t.verdict === "Activated").length
      < report.targets.length,
  );
});

test("native-CI-only targets are executed_here=false and SafeDisabled", () => {
  const report = buildReport({
    suites: [green("s04")],
    guard: { ok: true, tail: [] },
    packaging: { ok: true, tail: [] },
  });
  const native = report.targets.filter((t) => t.executed_here === false);
  assert.equal(native.length, 3);
  for (const target of native) {
    assert.equal(target.verdict, "SafeDisabled");
    assert.ok(target.reason.includes("native CI"));
  }
});

test("redaction strips credential-shaped material everywhere", () => {
  const dirty =
    "token=abc123XYZlongvalueSECRET and secret= hunter2 plus " +
    "AKIA1234567890abcdefghij012345678901234567890 and a 40+-char blob AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
  const clean = redact(dirty);
  for (const marker of ["hunter2", "AKIA1234567890", "AAAAAAAAA"]) {
    assert.ok(!clean.includes(marker), `${marker} must be redacted`);
  }
  assert.ok(clean.includes("token=[redacted]"));
});

test("digestOf is stable and distinct", () => {
  assert.equal(digestOf("abc"), digestOf("abc"));
  assert.notEqual(digestOf("abc"), digestOf("abd"));
});
