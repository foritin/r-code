#!/usr/bin/env node
// R-Code 产品体验重构唯一 Verification Harness 入口（PRD §8.1，M0-01 建立）：
//   node scripts/verify-product-experience.mjs --task <TASK_ID> --profile implementation
//   node scripts/verify-product-experience.mjs --through <MILESTONE_ID> --profile implementation
//
// 非交互：退出码 0 仅代表全部 required 断言通过（not_implemented / 证据缺失 /
// 命令失败 / profile 未注册均视为失败并携带精确断言 ID）。
// 报告写入 artifacts/ai-tasks/verification/product-experience-gap-closure/<profile>/<id>.json。

import { parseArgs } from "node:util";
import process from "node:process";

import {
  DEFAULT_REPORT_ROOT,
  runVerification,
} from "./product-experience/runner.mjs";
import {
  MILESTONES,
  PROFILES,
  REGISTRY,
  registryDigest,
  tasksThroughMilestone,
  validateRegistry,
} from "./product-experience/registry.mjs";

function usage(message) {
  console.error(
    `${message}\nusage: verify-product-experience.mjs (--task <ID> | --through <MILESTONE>) ` +
      `--profile ${PROFILES.join("|")} [--report-root <dir>] [--list]`,
  );
  process.exitCode = 2;
}

function listMode() {
  const issues = validateRegistry();
  if (issues.length > 0) {
    console.error(`registry invalid:\n${issues.map((i) => `  - ${i}`).join("\n")}`);
    process.exitCode = 1;
    return;
  }
  for (const [tid, task] of Object.entries(REGISTRY)) {
    const wired = task.assertions.filter((a) => !a.not_implemented).length;
    const state =
      wired === task.assertions.length ? "wired" : wired > 0 ? "partial" : "pending";
    console.log(`${tid}\t${task.milestone}\t${state}\t${wired}/${task.assertions.length} assertions`);
  }
  console.log(`# registry_digest=${registryDigest()}`);
}

async function main() {
  let args;
  try {
    args = parseArgs({
      options: {
        task: { type: "string" },
        through: { type: "string" },
        profile: { type: "string", default: "implementation" },
        "report-root": { type: "string", default: DEFAULT_REPORT_ROOT },
        list: { type: "boolean", default: false },
        "no-cache": { type: "boolean", default: false },
      },
      strict: true,
    });
  } catch (error) {
    usage(error.message);
    return;
  }
  const v = args.values;

  if (v.list) return listMode();
  if (!v.task && !v.through) return usage("必须指定 --task 或 --through");
  if (v.task && v.through) return usage("--task 与 --through 互斥");
  if (!PROFILES.includes(v.profile)) return usage(`未知 profile: ${v.profile}`);

  // 结构校验：接线孤儿/依赖环/重复断言等一律拒绝执行
  const issues = validateRegistry();
  if (issues.length > 0) {
    console.error(`registry invalid:\n${issues.map((i) => `  - ${i}`).join("\n")}`);
    process.exitCode = 1;
    return;
  }

  let selection;
  let label;
  if (v.task) {
    if (!(v.task in REGISTRY)) {
      console.error(`unknown task: ${v.task}`);
      process.exitCode = 2;
      return;
    }
    // 只验证目标任务本身；依赖就绪由累计门禁（--through）与依赖环/存在性校验保证，
    // 避免 DAG 根部（D0 重摄等重命令）被每个单任务验证反复执行。
    selection = [v.task];
    label = v.task;
  } else {
    selection = tasksThroughMilestone(v.through);
    if (selection === null || selection.length === 0) {
      console.error(`unknown milestone or empty selection: ${v.through}`);
      process.exitCode = 2;
      return;
    }
    label = `through-${v.through}`;
  }

  const { report, file } = await runVerification({
    selection,
    profile: v.profile,
    reportRoot: v["report-root"],
    rootDir: process.cwd(),
    targetLabel: label,
    fileName: label,
    cache: !v["no-cache"],
  });

  printSummary(report);
  console.log(`report: ${file}`);
  process.exitCode = report.ok ? 0 : 1;
}

function printSummary(report) {
  const s = report.summary;
  console.log(
    `target=${report.requested_target} profile=${report.profile} ` +
      `platform=${report.platform.os}/${report.platform.node} revision=${report.revision ?? "?"}`,
  );
  console.log(`assertions: ${s.passed}/${s.total} passed, ${s.failed} failed`);
  for (const r of report.results) {
    if (r.ok) continue;
    const why =
      r.outcome === "not_implemented"
        ? "尚未接线（待对应里程碑实施）"
        : r.outcome === "not_registered_for_profile"
          ? `profile ${report.profile} 未注册该断言`
          : r.timed_out
            ? `超时 exit=null`
            : `exit=${r.exit_code}`;
    console.log(`FAIL ${r.assertion_id}: ${why}${r.stderr ? `\n  ${r.stderr.split("\n")[0].slice(0, 200)}` : ""}`);
  }
}

await main();
