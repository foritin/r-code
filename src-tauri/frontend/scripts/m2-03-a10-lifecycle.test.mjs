// M2-03.A10：Settings lifecycle reducer 全合同用例（R-SET-09）。
// 读取六态、草稿三态、保存失败保留、刷新不覆盖 dirty、CAS 三路恢复各产生新 base revision。

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import ts from "typescript";

const frontendDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

async function loadLifecycle() {
  const source = readFileSync(path.join(frontendDir, "src", "lib", "settings-lifecycle.ts"), "utf8");
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
  });
  return import(`data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`);
}

test("读取六态：首载失败无快照→failed；有 last-good 失败→stale_last_good；retry 回 loading", async () => {
  const m = await loadLifecycle();
  let s = m.initialSettingsLifecycle();
  assert.equal(s.load, "uninitialized");
  s = m.reduceSettingsLifecycle(s, { type: "LOAD_START" });
  assert.equal(s.load, "loading");
  s = m.reduceSettingsLifecycle(s, { type: "LOAD_FAILURE", error: "offline" });
  assert.equal(s.load, "failed");
  assert.equal(s.persisted, null, "无快照失败不得伪造值");
  s = m.reduceSettingsLifecycle(s, { type: "RETRY" });
  assert.equal(s.load, "retrying");

  s = m.initialSettingsLifecycle();
  s = m.reduceSettingsLifecycle(s, { type: "LOAD_START" });
  s = m.reduceSettingsLifecycle(s, { type: "LOAD_SUCCESS", snapshot: { revision: "r1", value: { a: 1 } } });
  s = m.reduceSettingsLifecycle(s, { type: "LOAD_FAILURE", error: "network" });
  assert.equal(s.load, "stale_last_good", "有 last-good 时失败必须保留可读旧值");
  assert.equal(s.persisted.revision, "r1");
});

test("草稿：编辑→dirty；保存失败保留 persisted+dirty draft", async () => {
  const m = await loadLifecycle();
  let s = m.initialSettingsLifecycle();
  s = m.reduceSettingsLifecycle(s, { type: "LOAD_START" });
  s = m.reduceSettingsLifecycle(s, { type: "LOAD_SUCCESS", snapshot: { revision: "r1", value: { a: 1 } } });
  s = m.reduceSettingsLifecycle(s, { type: "EDIT_DRAFT", draft: { a: 2 } });
  assert.equal(s.draftPhase, "dirty");
  s = m.reduceSettingsLifecycle(s, { type: "SAVE_START" });
  assert.equal(s.draftPhase, "saving");
  s = m.reduceSettingsLifecycle(s, { type: "SAVE_FAILURE", error: "500" });
  assert.equal(s.draftPhase, "dirty", "失败后草稿必须保留");
  assert.equal(s.persisted.value.a, 1, "失败不得改变持久值");
  assert.deepEqual(s.draft, { a: 2 });
  s = m.reduceSettingsLifecycle(s, { type: "SAVE_SUCCESS", revision: "r2" });
  assert.equal(s.draftPhase, "clean");
  assert.equal(s.persisted.revision, "r2");
  assert.deepEqual(s.persisted.value, { a: 2 });
});

test("刷新不静默覆盖：clean 静默采用新值；dirty 转 stale_last_good 且草稿原样", async () => {
  const m = await loadLifecycle();
  let s = m.initialSettingsLifecycle();
  s = m.reduceSettingsLifecycle(s, { type: "LOAD_START" });
  s = m.reduceSettingsLifecycle(s, { type: "LOAD_SUCCESS", snapshot: { revision: "r1", value: { a: 1 } } });
  s = m.reduceSettingsLifecycle(s, { type: "EDIT_DRAFT", draft: { a: 2 } });
  s = m.reduceSettingsLifecycle(s, { type: "REMOTE_REFRESH", snapshot: { revision: "r2", value: { a: 9 } } });
  assert.equal(s.load, "stale_last_good");
  assert.deepEqual(s.draft, { a: 2 }, "dirty 草稿被刷新覆盖 = 违反合同");
  assert.equal(s.persisted.revision, "r2", "last-good 应更新到最新已知 Host 值");

  let clean = m.initialSettingsLifecycle();
  clean = m.reduceSettingsLifecycle(clean, { type: "LOAD_START" });
  clean = m.reduceSettingsLifecycle(clean, { type: "LOAD_SUCCESS", snapshot: { revision: "r1", value: { a: 1 } } });
  clean = m.reduceSettingsLifecycle(clean, { type: "REMOTE_REFRESH", snapshot: { revision: "r2", value: { a: 9 } } });
  assert.equal(clean.load, "ready");
  assert.deepEqual(clean.persisted.value, { a: 9 });
});

test("CAS 三路恢复：每条路径产生新 base revision 并离开冲突态", async () => {
  const m = await loadLifecycle();
  const base = () => {
    let s = m.initialSettingsLifecycle();
    s = m.reduceSettingsLifecycle(s, { type: "LOAD_START" });
    s = m.reduceSettingsLifecycle(s, { type: "LOAD_SUCCESS", snapshot: { revision: "r1", value: { a: 1 } } });
    s = m.reduceSettingsLifecycle(s, { type: "EDIT_DRAFT", draft: { a: 2 } });
    s = m.reduceSettingsLifecycle(s, {
      type: "CAS_CONFLICT",
      localDigest: "d-local",
      fresh: { revision: "r-host", value: { a: 5 } },
    });
    assert.ok(s.conflict, "冲突态必须同时保留 local digest 与 fresh 快照");
    assert.equal(s.conflict.localDigest, "d-local");
    assert.equal(s.conflict.fresh.revision, "r-host");
    return s;
  };

  // 路径一：discard local
  let s = base();
  s = m.reduceSettingsLifecycle(s, { type: "CONFLICT_DISCARD_LOCAL" });
  assert.equal(s.conflict, null);
  assert.equal(s.persisted.revision, "r-host");
  assert.equal(s.baseRevision, "r-host");
  assert.equal(s.baseEpoch, 1);

  // 路径二：reapply onto latest
  s = base();
  s = m.reduceSettingsLifecycle(s, { type: "CONFLICT_REAPPLY_LOCAL", revision: "r3" });
  assert.equal(s.conflict, null);
  assert.deepEqual(s.persisted.value, { a: 2 });
  assert.equal(s.baseRevision, "r3");
  assert.equal(s.baseEpoch, 1);

  // 路径三：field merge preview 接受
  s = base();
  const preview = m.reduceSettingsLifecycle(s, { type: "CONFLICT_MERGE_ACCEPT", merged: { a: 7 }, revision: "r4" });
  assert.equal(preview.conflict, null);
  assert.deepEqual(preview.persisted.value, { a: 7 });
  assert.equal(preview.baseRevision, "r4");
  assert.equal(preview.baseEpoch, 1);
});
