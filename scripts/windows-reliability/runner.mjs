// M0-01 建立的统一编排引擎（windows-reliability 项目）：执行 registry 断言命令
// 并产出 artifacts/ai-tasks/verification/windows-reliability/<profile>/<id>.json。
//
// 安全合同对齐 codex-interaction runner：报告只记录命令、退出码、时长、输出
// 摘要 sha256 与失败时的脱敏节选；stdout/stderr 全文永不落盘。
//
// 差异点：断言支持可选 `env`（例如金集 runner 的 CORPUS_RUN / CORPUS_GIT_SHA），
// 与进程环境合并后传给子进程。

import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { spawnSync } from "node:child_process";
import path from "node:path";
import process from "node:process";

export const REPORT_SCHEMA_VERSION = "windows-reliability-verification.v1";
export const DEFAULT_REPORT_ROOT = "artifacts/ai-tasks/verification/windows-reliability";
export const CORPUS_SCHEMA_VERSION = "command-corpus.v1";

const REDACTION_PATTERNS = [
  [/sk-[A-Za-z0-9_-]{6,}/g, "[REDACTED:token]"],
  [/bearer\s+[A-Za-z0-9._-]{6,}/gi, "[REDACTED:bearer]"],
  [/(api[_-]?key|password|passwd|secret)\s*[:=]\s*\S+/gi, "[REDACTED:credential]"],
];

export function redact(text) {
  return REDACTION_PATTERNS.reduce((acc, [pattern, replacement]) => acc.replace(pattern, replacement), text);
}

function outputDigest(stdout, stderr) {
  return createHash("sha256").update(`${stdout ?? ""}\n${stderr ?? ""}`).digest("hex");
}

function failureExcerpt(stdout, stderr) {
  const combined = `${stdout ?? ""}\n${stderr ?? ""}`.trimEnd();
  if (combined.length === 0) {
    return null;
  }
  const tail = combined.slice(-1200);
  return redact(tail);
}

async function gitInfo(rootDir) {
  const run = (args) => spawnSync("git", args, { cwd: rootDir, encoding: "utf8", timeout: 15_000 });
  const revision = run(["rev-parse", "HEAD"]);
  const branch = run(["rev-parse", "--abbrev-ref", "HEAD"]);
  const status = run(["status", "--porcelain"]);
  if (revision.status !== 0) {
    return { git_revision: null, branch: null, worktree_digest: null };
  }
  return {
    git_revision: revision.stdout.trim(),
    branch: branch.status === 0 ? branch.stdout.trim() : null,
    worktree_digest: createHash("sha256").update(status.stdout ?? "").digest("hex"),
  };
}

function runAssertion(assertion, taskId, rootDir) {
  const startedAt = Date.now();
  const base = {
    id: assertion.id,
    task: taskId,
    level: assertion.level,
    profiles: assertion.profiles,
    note: assertion.note ?? null,
    evidence_path: assertion.evidence_path ?? null,
  };

  if (assertion.not_implemented) {
    return {
      ...base,
      status: "not_implemented",
      passed: false,
      exit_code: null,
      duration_ms: 0,
      reason: `断言已注册但尚未实施：${assertion.note ?? taskId}`,
    };
  }
  if (assertion.external) {
    return {
      ...base,
      status: "external_pending",
      passed: false,
      exit_code: null,
      duration_ms: 0,
      reason: "外部放行条件（真实凭据/外部主机），implementation profile 不参与判定",
    };
  }

  const child = spawnSync(assertion.command[0], assertion.command.slice(1), {
    cwd: path.join(rootDir, assertion.cwd ?? "."),
    encoding: "utf8",
    timeout: assertion.timeout_ms ?? 10 * 60 * 1000,
    maxBuffer: 32 * 1024 * 1024,
    env: { ...process.env, ...(assertion.env ?? {}) },
    shell: false,
  });
  const durationMs = Date.now() - startedAt;

  if (child.error) {
    return {
      ...base,
      command: assertion.command,
      status: "error",
      passed: false,
      exit_code: null,
      duration_ms: durationMs,
      reason: child.error.message,
    };
  }
  if (child.signal === "SIGTERM" || child.status === null) {
    return {
      ...base,
      command: assertion.command,
      status: "timeout",
      passed: false,
      exit_code: null,
      duration_ms: durationMs,
      reason: `超过 ${assertion.timeout_ms ?? 600000}ms 超时`,
      output_sha256: outputDigest(child.stdout, child.stderr),
    };
  }
  return {
    ...base,
    // argv 理论上不含 secret；仍统一脱敏，防止断言命令把敏感值带进报告。
    command: assertion.command.map(redact),
    status: child.status === 0 ? "passed" : "failed",
    passed: child.status === 0,
    exit_code: child.status,
    duration_ms: durationMs,
    output_sha256: outputDigest(child.stdout, child.stderr),
    failure_excerpt: child.status === 0 ? null : failureExcerpt(child.stdout, child.stderr),
  };
}

export async function runVerification({ mode, registry, rootDir, reportRoot, profile = "implementation", stdout = process.stdout }) {
  if (!["task", "through"].includes(mode.kind)) {
    throw new Error(`mode.kind must be task|through, got ${mode.kind}`);
  }
  if (!["implementation", "production"].includes(profile)) {
    throw new Error(`profile must be implementation|production, got ${profile}`);
  }

  const taskIds = mode.kind === "task" ? [mode.id] : mode.taskIds;
  const results = [];
  for (const taskId of taskIds) {
    const task = registry[taskId];
    if (!task) {
      throw new Error(`unknown task id: ${taskId}`);
    }
    for (const assertion of task.assertions) {
      if (!assertion.profiles.includes(profile)) {
        continue;
      }
      results.push(runAssertion(assertion, taskId, rootDir));
    }
  }

  const summary = {
    total: results.length,
    passed: results.filter((r) => r.status === "passed").length,
    failed: results.filter((r) => r.status === "failed" || r.status === "error" || r.status === "timeout").length,
    not_implemented: results.filter((r) => r.status === "not_implemented").length,
    external_pending: results.filter((r) => r.status === "external_pending").length,
  };
  // required 语义：not_implemented 也是失败（显式失败，不允许静默跳过）。
  const exitCode = summary.passed === summary.total ? 0 : 1;

  const revision = await gitInfo(rootDir);
  const report = {
    schema_version: REPORT_SCHEMA_VERSION,
    generated_at: new Date().toISOString(),
    mode: { kind: mode.kind, id: mode.id, tasks: taskIds, profile },
    revision,
    corpus_schema_version: CORPUS_SCHEMA_VERSION,
    platform: { node: process.version, os: process.platform, arch: process.arch },
    exit_code: exitCode,
    summary,
    assertions: results,
    evidence_index: {
      task_packet: "artifacts/ai-tasks/current.yaml",
      evidence_dir: "artifacts/ai-tasks/evidence/windows-reliability",
      corpus_reports: "artifacts/metrics/command-corpus",
      redaction: "stdout/stderr 全文不落盘；失败节选先脱敏再截断（1200 字符）",
    },
  };

  const reportBase = path.isAbsolute(reportRoot) ? reportRoot : path.join(rootDir, reportRoot);
  const reportDir = path.join(reportBase, profile);
  await mkdir(reportDir, { recursive: true });
  const reportPath = path.join(reportDir, `${mode.id}.json`);
  await writeFile(reportPath, `${JSON.stringify(report, null, 2)}\n`, "utf8");

  const failedIds = results.filter((r) => !r.passed).map((r) => r.id);
  stdout.write(`verification ${mode.kind} ${mode.id} [${profile}]: ${summary.passed}/${summary.total} passed, exit=${exitCode}\n`);
  for (const result of results.filter((r) => !r.passed)) {
    stdout.write(`  FAILED ${result.id} (${result.status})${result.reason ? `: ${result.reason}` : ""}\n`);
  }
  stdout.write(`report: ${path.relative(rootDir, reportPath)}\n`);

  return { report, reportPath, exitCode, failedIds };
}
