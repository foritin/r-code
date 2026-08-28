// M4-01：Run Capsule 全合同（§5.4 矩阵唯一 detail_state、终态级联、迟到帧、live/replay 重放、脱敏）。

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import ts from "typescript";

const frontendDir = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..");

async function loadCapsule() {
  const source = readFileSync(path.join(frontendDir, "src", "lib", "run-capsule.ts"), "utf8");
  const { outputText } = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
  });
  return import(`data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`);
}

function capsule() {
  const m = { runId: "run-1", fold: "auto_compact", events: [], terminal: null, rejectedLate: 0, timerRunning: true };
  return m;
}
function mkCapsuleLib(m) {
  return { ...m };
}

test("A1 §5.4 矩阵：approval/question/error/final/warning 自动展开；纯 running 紧凑", async () => {
  const m = await loadCapsule();
  let c = { ...capsule(), fold: m.autoFold([], null) };
  assert.equal(c.fold, "auto_compact", "无 Attention 的 running 默认紧凑");
  for (const kind of ["approval", "question", "error", "warning"]) {
    c = m.ingestEvent({ ...c }, { seq: c.events.length + 1, kind, status: "running", summary: "x" }, 0);
    assert.equal(c.fold, "auto_expanded", `${kind} 必须自动展开`);
  }
  // forced 可见性：final/approval/error 永不可藏
  for (const kind of ["approval", "question", "final", "error", "warning", "attention"]) {
    assert.equal(m.isForcedVisible(kind), true, `${kind} 必须强制可见`);
  }
  assert.equal(m.isForcedVisible("tool"), false);
});

test("A2 父终态：计时封口、running 工具转 error、迟到帧只计诊断不复活", async () => {
  const m = await loadCapsule();
  let c = {
    runId: "run-2", fold: "auto_compact", events: [
      { seq: 1, kind: "tool", status: "running", summary: "t1" },
      { seq: 2, kind: "tool", status: "ok", summary: "t2" },
    ], terminal: null, rejectedLate: 0, timerRunning: true,
  };
  c = m.terminateCapsule(c, "failed", 1000);
  assert.ok(c.terminal);
  assert.equal(c.timerRunning, false, "计时器必须封口");
  assert.equal(c.events[0].status, "error", "未完工具转 error");
  assert.equal(c.events[1].status, "ok", "已完工具保持原样");
  // 迟到帧
  const before = c.rejectedLate;
  c = m.ingestEvent(c, { seq: 3, kind: "tool", status: "running", summary: "late" }, 1001);
  assert.equal(c.rejectedLate, before + 1, "迟到帧只计诊断");
  // 单调：重复 terminate 不改终态
  const again = m.terminateCapsule(c, "completed", 2000);
  assert.deepEqual(again.terminal, c.terminal);
});

test("A3 live 序列化→重放：结构/顺序/终态/fold 一致", async () => {
  const m = await loadCapsule();
  let c = {
    runId: "run-3", fold: "auto_expanded", events: [
      { seq: 1, kind: "commentary", status: "ok", summary: "c" },
      { seq: 2, kind: "final", status: "ok", summary: "f" },
    ], terminal: { state: "completed", at: 12345 }, rejectedLate: 3, timerRunning: false,
  };
  const rebuilt = m.deserializeCapsule(m.serializeCapsule(c));
  assert.deepEqual(JSON.parse(m.serializeCapsule(rebuilt)), JSON.parse(m.serializeCapsule(c)));
  assert.equal(rebuilt.fold, c.fold);
  assert.equal(rebuilt.terminal?.state, c.terminal.state);
});

test("A4 latest update 脱敏：raw reasoning/secret 命中替换", async () => {
  const m = await loadCapsule();
  const dirty = m.sanitizeCapsuleText("reasoning: 内部思考 sk-abcdefghijklmnop1234");
  assert.ok(!dirty.includes("内部思考"), "raw reasoning 未清洗");
  assert.ok(!/sk-[A-Za-z0-9]/.test(dirty), "secret 未清洗");
  assert.ok(dirty.includes("[REDACTED]"));
});
