#!/usr/bin/env node
// Windows 命令执行可靠性唯一产品验收入口（PRD §7.1，M0-01 建立）：
//   node scripts/verify-windows-reliability.mjs --task M0-01 --profile implementation
//   node scripts/verify-windows-reliability.mjs --through M1 --profile implementation
//   node scripts/verify-windows-reliability.mjs --through M4 --profile production
//
// 非交互：退出码 0 仅代表全部 required 断言通过。报告写入
// artifacts/ai-tasks/verification/windows-reliability/<profile>/<id>.json。

import { parseArgs } from "node:util";
import process from "node:process";
import { DEFAULT_REPORT_ROOT, runVerification } from "./windows-reliability/runner.mjs";
import { MILESTONES, REGISTRY, getTask, tasksThroughMilestone, validateRegistry } from "./windows-reliability/registry.mjs";

function usage(message) {
  console.error(
    `${message}\nusage: verify-windows-reliability.mjs (--task <ID> | --through <MILESTONE>) [--profile implementation|production] [--report-root <dir>] [--list]`,
  );
  process.exitCode = 2;
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
      },
      strict: true,
    });
  } catch (error) {
    usage(error.message);
    return;
  }

  const rootDir = process.cwd();
  const values = args.values;

  if (values.list) {
    const issues = validateRegistry();
    if (issues.length > 0) {
      console.error(`registry invalid:\n${issues.map((issue) => `  - ${issue}`).join("\n")}`);
      process.exitCode = 1;
      return;
    }
    for (const taskId of Object.keys(REGISTRY)) {
      const task = REGISTRY[taskId];
      const state = task.assertions.every((a) => a.not_implemented) ? "pending" : "in-progress";
      console.log(`${taskId}\t${task.milestone}\t${state}\t${task.assertions.length} assertions`);
    }
    return;
  }

  if (values.task && values.through) {
    usage("--task and --through are mutually exclusive");
    return;
  }
  if (!values.task && !values.through) {
    usage("one of --task or --through is required");
    return;
  }
  if (!["implementation", "production"].includes(values.profile)) {
    usage(`--profile must be implementation|production, got ${values.profile}`);
    return;
  }

  const registryIssues = validateRegistry();
  if (registryIssues.length > 0) {
    console.error(
      `assertion registry invalid (missing required assertions are a hard failure):\n${registryIssues.map((issue) => `  - ${issue}`).join("\n")}`,
    );
    process.exitCode = 1;
    return;
  }

  let mode;
  if (values.task) {
    const task = getTask(REGISTRY, values.task);
    if (!task) {
      usage(`unknown task id: ${values.task}. known: ${Object.keys(REGISTRY).join(", ")}`);
      return;
    }
    mode = { kind: "task", id: values.task };
  } else {
    if (!MILESTONES.includes(values.through)) {
      usage(`unknown milestone: ${values.through}. known: ${MILESTONES.join(", ")}`);
      return;
    }
    // 累计语义：目标里程碑及其之前全部里程碑的产品任务。
    const taskIds = tasksThroughMilestone(values.through);
    if (taskIds.length === 0) {
      usage(`milestone ${values.through} has no tasks`);
      return;
    }
    mode = { kind: "through", id: values.through, taskIds };
  }

  try {
    const { exitCode } = await runVerification({
      mode,
      registry: REGISTRY,
      rootDir,
      reportRoot: values["report-root"],
      profile: values.profile,
    });
    process.exitCode = exitCode;
  } catch (error) {
    console.error(`verification error: ${error.message}`);
    process.exitCode = 2;
  }
}

await main();
