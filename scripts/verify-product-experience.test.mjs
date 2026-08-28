// verify-product-experience.mjs 自测（M0-01 实施步骤 5，断言 M0-01.A1/A3 的判定逻辑）。
// 运行：node --test scripts/verify-product-experience.test.mjs

import assert from "node:assert/strict";
import test from "node:test";
import { spawnSync } from "node:child_process";
import path from "node:path";
import process from "node:process";

import { sanitizeText } from "./product-experience/runner.mjs";
import {
  MILESTONES,
  REGISTRY,
  findCycles,
  registryDigest,
  tasksThroughMilestone,
  validateRegistry,
} from "./product-experience/registry.mjs";

const CLI = path.resolve(process.cwd(), "scripts", "verify-product-experience.mjs");

function cli(args) {
  return spawnSync(process.execPath, [CLI, ...args], {
    cwd: process.cwd(),
    encoding: "utf8",
    timeout: 120_000,
  });
}

test("真实注册表结构合法（42 任务、无环、无重复断言）", () => {
  const issues = validateRegistry();
  assert.deepEqual(issues, [], `issues=${JSON.stringify(issues)}`);
  assert.equal(Object.keys(REGISTRY).length >= 42, true);
});

test("--through 选集包含 D0 与全部 M0 任务且依赖闭包完整", () => {
  const sel = tasksThroughMilestone("M1");
  assert.ok(sel.includes("D0-01"));
  for (const t of sel) {
    for (const dep of REGISTRY[t].depends_on) {
      assert.ok(sel.includes(dep), `${t} 的依赖 ${dep} 不在选集内`);
    }
  }
  const order = ["D0", "M0", "M1"];
  assert.equal(sel.every((t) => order.includes(REGISTRY[t].milestone)), true);
});

test("依赖环检测：注入环形依赖被精确报告", () => {
  const cyclicTasks = {
    "M1-01": { depends_on: ["M1-02"] },
    "M1-02": { depends_on: ["M1-03"] },
    "M1-03": { depends_on: ["M1-01"] },
  };
  const issues = findCycles(cyclicTasks);
  assert.equal(issues.length, 1);
  assert.match(issues[0], /M1-01 -> M1-02 -> M1-03 -> M1-01/);
});

test("findCycles 对真实注册表报告 0 环", () => {
  assert.deepEqual(findCycles(), []);
});

test("registry digest 稳定且非空", () => {
  assert.equal(registryDigest(), registryDigest());
  assert.match(registryDigest(), /^[0-9a-f]{64}$/);
});

test("MILESTONES 覆盖 D0..M9 且单调", () => {
  assert.deepEqual(MILESTONES.slice(0, 3), ["D0", "M0", "M1"]);
  assert.equal(MILESTONES.at(-1), "M9");
});

test("sanitizeText 打码常见密钥形态但不误伤普通文本", () => {
  const masked = sanitizeText(
    "key sk-abcdefghijklmnop1234 ghp_abcdefghijklmnopqrstuvwxyz012345 Bearer abc.def/h-i j=token:abcd1234",
  );
  assert.doesNotMatch(masked, /sk-abcde/);
  assert.doesNotMatch(masked, /ghp_abcd/);
  assert.match(masked, /\[REDACTED\]/);
  assert.match(sanitizeText("普通中文输出 exit=0"), /exit=0/);
});

test("CLI：未知任务 → 非 0 且报准确 ID", () => {
  const r = cli(["--task", "NOPE-99"]);
  assert.notEqual(r.status, 0);
  assert.match(r.stderr + r.stdout, /NOPE-99/);
});

test("CLI：--task 与 --through 并用 → usage 非 0", () => {
  const r = cli(["--task", "M0-01", "--through", "M1", "--profile", "implementation"]);
  assert.notEqual(r.status, 0);
});

test("CLI：未知里程碑 → 非 0 且报值", () => {
  const r = cli(["--through", "M99"]);
  assert.notEqual(r.status, 0);
  assert.match(r.stderr + r.stdout, /M99/);
});

test("CLI：失败子命令 → 非 0 且失败列表含准确断言 ID（not_implemented 视为显式失败）", () => {
  // M9-04 全部断言未接线 → 必须整体失败并逐 ID 列出
  const pending = Object.values(REGISTRY["M9-04"].assertions)
    .filter((a) => a.not_implemented)
    .map((a) => a.id);
  assert.ok(pending.length > 0, "fixture 需要 M9-04 存在未接线断言");
  const r = cli(["--task", "M9-04", "--report-root", ".tmp-selftest-reports"]);
  assert.equal(r.status, 1);
  for (const id of pending) {
    assert.match(r.stdout, new RegExp(`FAIL ${id}`));
  }
});

test("CLI：--list 合法退出并列出任务统计", () => {
  const r = cli(["--list"]);
  assert.equal(r.status, 0);
  assert.match(r.stdout, /M0-01\tM0\t/);
});
