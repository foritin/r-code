/**
 * S19B-C — the desktop effect-approval renderer. `Permissions.tsx` is the one
 * shared renderer the workbench reuses, so this pins its pure presentation
 * model: the canonical six keys in the pinned `EFFECT_MATERIAL_FIELDS` order,
 * the identity rows that follow, the daemon error-prefix classification, and
 * the capability → control projection. The JSX itself is not executed (it
 * needs React); the pure functions are transpiled out of the real source, so
 * this cannot drift from what ships.
 */
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import ts from "typescript";

const frontendDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const source = readFileSync(
  path.join(frontendDir, "src", "components", "room", "Permissions.tsx"),
  "utf8",
);
const locales = JSON.parse(
  readFileSync(path.join(frontendDir, "src", "i18n", "locales", "zh-CN.json"), "utf8"),
);

/** The block of pure presentation functions, from the pinned key list down to
 * the capability projection — everything between is React-only code. */
function extract(block) {
  const from = source.indexOf(block);
  assert.notEqual(from, -1, `missing ${block} in Permissions.tsx`);
  // Section banners come in pairs (banner + underline). Interleaved
  // sections (the E09-C override renderer sits between effect blocks)
  // must not truncate the extraction: end at the second banner line.
  const banner = "\n// ----";
  let cursor = source.indexOf(banner, from);
  let seen = 0;
  let end = -1;
  while (cursor !== -1) {
    seen += 1;
    if (seen === 2) {
      end = cursor;
      break;
    }
    cursor = source.indexOf(banner, cursor + banner.length);
  }
  assert.notEqual(end, -1, `unterminated ${block} block`);
  return source.slice(from, end);
}

async function loadRenderer() {
  const { outputText } = ts.transpileModule(extract("export const EFFECT_MATERIAL_FIELDS"), {
    compilerOptions: {
      module: ts.ModuleKind.ESNext,
      target: ts.ScriptTarget.ES2022,
      jsx: ts.JsxEmit.ReactJSX,
    },
  });
  return import(`data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`);
}

const MATERIAL = {
  taskId: "task-s19bc",
  planRevision: "rev-1",
  workUnitId: "unit-shell",
  effectClass: "workspace-mutation",
  network: "public-internet-client",
  payloadHash: "sha256:abc",
};

const APPROVAL = {
  ...MATERIAL,
  approvalId: "op-s19bc-1",
  actorId: "actor-x",
  sessionId: "cmd-x",
  scope: "effect.approve",
  state: "active",
  createdAtMs: 1_700_000_000_000,
};

test("S19B-C.A1 the renderer pins the six canonical keys in daemon order", async () => {
  const m = await loadRenderer();
  assert.deepEqual(
    [...m.EFFECT_MATERIAL_FIELDS],
    ["taskId", "planRevision", "workUnitId", "effectClass", "network", "payloadHash"],
  );
  // The remote projection must publish the identical order.
  const remote = readFileSync(
    path.join(frontendDir, "src", "remote", "core", "approvals-aggregate.ts"),
    "utf8",
  );
  const remoteBlock = remote.slice(
    remote.indexOf("export const EFFECT_MATERIAL_FIELDS"),
    remote.indexOf("as const;", remote.indexOf("export const EFFECT_MATERIAL_FIELDS")),
  );
  assert.deepEqual(
    [...remoteBlock.matchAll(/"(\w+)"/g)].map((match) => match[1]),
    [...m.EFFECT_MATERIAL_FIELDS],
    "desktop and remote pin the same order",
  );
});

test("S19B-C.A2 a granted approval renders the six columns then its identity", async () => {
  const m = await loadRenderer();
  assert.deepEqual(m.effectAuthorityRows(APPROVAL), [
    { key: "taskId", value: "task-s19bc" },
    { key: "planRevision", value: "rev-1" },
    { key: "workUnitId", value: "unit-shell" },
    { key: "effectClass", value: "workspace-mutation" },
    { key: "network", value: "public-internet-client" },
    { key: "payloadHash", value: "sha256:abc" },
    { key: "approvalId", value: "op-s19bc-1" },
    { key: "actorId", value: "actor-x" },
    { key: "sessionId", value: "cmd-x" },
    { key: "scope", value: "effect.approve" },
  ]);
  // A pending request shows the same six columns plus the operation id.
  const pending = { ...MATERIAL, operationId: "op-pending" };
  assert.deepEqual(m.effectRequestRows(pending), [
    ...m.EFFECT_MATERIAL_FIELDS.map((key) => ({ key, value: pending[key] })),
    { key: "operationId", value: "op-pending" },
  ]);
  // Supersession does not change the shape — the same authority row, audited.
  const superseded = m.effectAuthorityRows({ ...APPROVAL, state: "superseded", supersededAtMs: 2 });
  assert.deepEqual(superseded, m.effectAuthorityRows(APPROVAL));
});

test("S19B-C.A3 effectErrorCode classifies every daemon error prefix", async () => {
  const m = await loadRenderer();
  const cases = {
    plan_not_approved: "plan_not_approved: task t1 has no active plan approval",
    effect_approval_stale: "effect_approval_stale: op-1 no longer matches the approved head",
    effect_approval_active_exists: "effect_approval_active_exists: revoke it first",
    effect_approval_conflict: "effect_approval_conflict: op-1 is already superseded",
    approval_unknown: "approval_unknown: no such operation",
    approval_conflict: "approval_conflict: op-1 was already decided",
    needs_desktop_confirm: "needs_desktop_confirm: this task requires a desktop approval",
  };
  for (const [code, message] of Object.entries(cases)) {
    assert.equal(m.effectErrorCode(message), code, message);
  }
  // An unclassified failure stays null so the panel shows the raw error.
  assert.equal(m.effectErrorCode("connection reset by peer"), null);
  assert.equal(m.effectErrorCode(""), null);
  // Every prefix the daemon actually emits is covered by the classifier.
  const runtime = readFileSync(
    path.join(frontendDir, "..", "..", "crates", "r-code-runtime", "src", "application", "effect_approvals.rs"),
    "utf8",
  );
  for (const match of runtime.matchAll(/"(plan_not_approved|effect_approval_\w+|approval_\w+|needs_desktop_confirm)/g)) {
    assert.notEqual(m.effectErrorCode(match[1]), null, `unclassified daemon code ${match[1]}`);
  }
});

test("S19B-C.A4 a missing capability hides the control instead of disabling it", async () => {
  const m = await loadRenderer();
  const readOnly = m.effectGrantUi(true, true);
  assert.equal(readOnly.canControl, false, "no request control");
  assert.equal(readOnly.canRevoke, false, "no revoke control");
  assert.equal(readOnly.readOnlyBadge, "approvals.effectReadOnly");
  const controlled = m.effectGrantUi(false, true);
  assert.equal(controlled.canControl, true);
  assert.equal(controlled.canRevoke, true);
  assert.equal(controlled.readOnlyBadge, null);
  // No live grant means there is nothing to revoke, even when permitted.
  const idle = m.effectGrantUi(false, false);
  assert.equal(idle.canControl, true);
  assert.equal(idle.canRevoke, false);
  assert.equal(idle.stateLabel, "pending");
  assert.equal(m.effectGrantUi(false, true).stateLabel, "active");
  // The read-only badge key must exist in every shipped locale.
  for (const locale of ["zh-CN", "en-US"]) {
    const text = JSON.parse(
      readFileSync(path.join(frontendDir, "src", "i18n", "locales", `${locale}.json`), "utf8"),
    );
    assert.equal(typeof text.approvals.effectReadOnly, "string", `${locale} effectReadOnly`);
  }
});

test("S19B-C.A5 the superseded row states both standing warnings", () => {
  // The desktop previously rendered a bare 已撤销 label, which hid the
  // future-runs-only semantics the TUI and the remote already state.
  const row = extract("function SupersededRow");
  assert.match(row, /approvals\.effectRevokeWarning/);
  assert.match(row, /approvals\.effectReauthorizeHint/);
  // The strings the keys resolve to are the real ones, not placeholders.
  assert.match(locales.approvals.effectRevokeWarning, /仅影响未来运行/);
  assert.match(locales.approvals.effectReauthorizeHint, /终身不复用/);
  assert.match(locales.approvals.effectDecisionHint, /拒绝或过期不会留下任何授权记录/);
});

test("S19B-C.B1 the workbench reuses the Permissions renderer verbatim", () => {
  // P19B-C.3: Canvas must not re-implement the presentation.
  const canvas = readFileSync(
    path.join(frontendDir, "src", "components", "room", "Canvas.tsx"),
    "utf8",
  );
  assert.match(canvas, /import \{[^}]*EffectApprovalPanel[^}]*\} from "\.\/Permissions"/);
  assert.match(canvas, /<EffectApprovalPanel\b/);
  assert.ok(!/<EffectApprovalPanel[^>]*\breadOnly\b/.test(canvas), "the workbench panel is not read-only");
  // Permissions itself renders the panel in both scenes; the shared import
  // count must stay at exactly one definition site.
  assert.equal((source.match(/export function EffectApprovalPanel/g) ?? []).length, 1);
});
