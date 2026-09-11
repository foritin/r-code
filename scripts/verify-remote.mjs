#!/usr/bin/env node
// verify-remote.mjs — 远程控制（含 R0 审批通道）统一验收入口。
//
// 用法：
//   node scripts/verify-remote.mjs --task <ID> [--profile implementation]
//   node scripts/verify-remote.mjs --through <MILESTONE>
//   node scripts/verify-remote.mjs --list
//
// 契约（prd/remote-control/worklist.md §8）：
//   - 非交互；exit 0 仅当全部 required assertion 通过
//   - 输出机器可读 JSON 报告到 artifacts/ai-tasks/verification/<profile>/<id>.json
//   - 不删除/跳过失败断言；缺断言=失败
//
// 当前状态（计划 draft）：注册表已建立，任务实现后在此文件追加 assertion 映射。
// 每个 assertion 映射到一个非交互命令（cargo test 过滤名 / node --test 文件）。

import { execFileSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

// ── 里程碑出口（累计门禁，按依赖顺序执行组）─────────────────────────────────
const MILESTONES = {
  R0: ["RA1", "RA2", "RA3"],
  R1: ["R00", "R01", "R02", "R03", "R04", "R05", "R06", "R07", "R08"],
  R2: ["R09", "R10", "R11"],
  R3: ["R12", "R07b", "R07c"],
  R4: ["R13", "R14"],
  R5: ["R15", "R16", "R17", "R18", "R19", "R20"],
};

// ── Assertion registry ──────────────────────────────────────────────────────
// 实现任务时把每个 A 断言填成可二值判定的非交互命令。
// kind: cargo-test | node-test | static-guard | script
// 未实现任务的断言状态为 "pending"：--task 遇到 pending 断言以 exit 2 报告
// （“断言尚未实现”），绝不静默通过。
export const ASSERTIONS = {
  RA1: {
    milestone: "R0",
    required: ["RA1.A1", "RA1.A2", "RA1.A3"],
  },
  RA2: {
    milestone: "R0",
    required: ["RA2.A1", "RA2.A2"],
  },
  RA3: {
    milestone: "R0",
    required: ["RA3.A1", "RA3.A2"],
  },
  R00: { milestone: "R1", required: ["R00.A1"] },
  R01: { milestone: "R1", required: ["R01.A1", "R01.A2", "R01.A3"] },
  R02: { milestone: "RR0", required: ["R02.A1", "R02.A2", "R02.A3"] },
  R03: { milestone: "RR0", required: ["R03.A1", "R03.A2", "R03.A3"] },
  R04: { milestone: "RR1", required: ["R04.A1", "R04.A2", "R04.A3", "R04.A4"] },
  R05: { milestone: "RR1", required: ["R05.A1", "R05.A2", "R05.A3"] },
  R06: { milestone: "R1", required: ["R06.A1", "R06.A2"] },
  R07: { milestone: "R1", required: ["R07.A1", "R07.A2"] },
  R08: { milestone: "R1", required: ["R08.A1", "R08.A2", "R08.A3"] },
  R09: { milestone: "RR2", required: ["R09.A1", "R09.A2"] },
  R10: { milestone: "RR2", required: ["R10.A1", "R10.A2", "R10.A3"] },
  R11: { milestone: "RR2", required: ["R11.A1", "R11.A2", "R11.A3"] },
  R12: { milestone: "R3", required: ["R12.A1", "R12.A2"] },
  "R07b": { milestone: "R3", required: ["R07b.A1", "R07b.A2"] },
  "R07c": { milestone: "R3", required: ["R07c.A1"] },
  R13: { milestone: "R4", required: ["R13.A1"] },
  R14: { milestone: "R4", required: ["R14.A1", "R14.A2"] },
  R15: { milestone: "R5", required: ["R15.A1"] },
  R16: { milestone: "R5", required: ["R16.A1", "R16.A2"] },
  R17: { milestone: "R5", required: ["R17.A1", "R17.A2"] },
  R18: { milestone: "R5", required: ["R18.A1"] },
  R19: { milestone: "RR5", required: ["R19.A1", "R19.A2", "R19.A3"] },
  R20: { milestone: "R5", required: ["R20.A1", "R20.A2"] },
};

// 断言→命令映射在实现任务时填充。键为 assertion id。
// 例：
// "RA1.A1": { kind: "cargo-test", pkg: "r-code-runtime", filter: "approval_request_is_persisted" }
export const COMMANDS = {};

function runCommand(spec) {
  const started = Date.now();
  try {
    let args;
    if (spec.kind === "cargo-test") {
      args = ["test", "-p", spec.pkg];
      if (spec.test) args.push("--test", spec.test);
      if (spec.lib) args.push("--lib");
      args.push(spec.filter, "--", "--exact", "--nocapture");
    } else if (spec.kind === "node-test") {
      args = ["--test", spec.file];
    } else if (spec.kind === "script") {
      args = spec.args;
    } else {
      throw new Error(`unknown spec kind: ${spec.kind}`);
    }
    execFileSync(spec.kind === "node-test" ? process.execPath : "cargo", args, {
      cwd: root,
      stdio: ["ignore", "pipe", "pipe"],
      timeout: 10 * 60 * 1000,
      encoding: "utf8",
    });
    return { passed: true, ms: Date.now() - started };
  } catch (error) {
    return {
      passed: false,
      ms: Date.now() - started,
      detail: String(error.stderr || error.message).slice(0, 4000),
    };
  }
}

function evaluate(ids, profile) {
  const results = [];
  for (const id of ids) {
    const entry = ASSERTIONS[id];
    if (!entry) {
      results.push({ task: id, status: "unknown-task" });
      continue;
    }
    for (const assertion of entry.required) {
      const spec = COMMANDS[assertion];
      if (!spec) {
        results.push({ task: id, assertion, status: "pending-implementation" });
        continue;
      }
      const outcome = runCommand(spec);
      results.push({ task: id, assertion, ...outcome });
    }
  }
  const failed = results.filter((r) => r.passed === false);
  const pending = results.filter((r) => r.status === "pending-implementation");
  const report = {
    profile,
    generated_at: new Date().toISOString(),
    tasks: ids,
    totals: {
      evaluated: results.length,
      passed: results.filter((r) => r.passed === true).length,
      failed: failed.length,
      pending: pending.length,
    },
    results,
  };
  return report;
}

function main() {
  const args = process.argv.slice(2);
  if (args.includes("--list")) {
    for (const [id, entry] of Object.entries(ASSERTIONS)) {
      console.log(`${id}\t${entry.milestone}\t${entry.required.length} assertions`);
    }
    return;
  }
  const profile = args.includes("--profile")
    ? args[args.indexOf("--profile") + 1]
    : "implementation";
  let ids;
  const taskIdx = args.indexOf("--task");
  const throughIdx = args.indexOf("--through");
  if (taskIdx >= 0) {
    ids = [args[taskIdx + 1]];
  } else if (throughIdx >= 0) {
    const milestone = args[throughIdx + 1];
    const chain = ["R0", "R1", "R2", "R3", "R4", "R5"];
    const upto = chain.indexOf(milestone);
    if (upto < 0) {
      console.error(`unknown milestone: ${milestone}`);
      process.exit(64);
    }
    ids = chain.slice(0, upto + 1).flatMap((m) => MILESTONES[m]);
  } else {
    console.error("usage: verify-remote.mjs --task <ID> | --through <R0..R5> [--profile ...]");
    process.exit(64);
  }

  const report = evaluate(ids, profile);
  const outDir = join(root, "artifacts/ai-tasks/verification", profile);
  mkdirSync(outDir, { recursive: true });
  const name = taskIdx >= 0 ? ids[0] : `through-${args[throughIdx + 1]}`;
  const outPath = join(outDir, `${name}.json`);
  writeFileSync(outPath, JSON.stringify(report, null, 2));

  const ok = report.totals.failed === 0 && report.totals.pending === 0;
  console.log(
    `verify-remote ${name}: ${report.totals.passed}/${report.totals.evaluated} passed` +
      (report.totals.pending ? `, ${report.totals.pending} pending implementation` : "") +
      (report.totals.failed ? `, ${report.totals.failed} FAILED` : "")
  );
  console.log(`report: ${outPath}`);
  // exit 2 while assertions are unimplemented (plan draft); flips to 0/1 only.
  process.exit(ok ? 0 : report.totals.failed ? 1 : 2);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  main();
}