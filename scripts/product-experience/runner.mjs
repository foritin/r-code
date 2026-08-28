// product-experience Harness 执行引擎（M0-01 实施步骤 3/4）。
//
// 契约：
// - 非交互；退出码由入口判定，本模块只产出结果与报告。
// - 报告写入 artifacts/ai-tasks/verification/product-experience-gap-closure/<profile>/<id>.json。
// - 所有捕获输出先脱敏再截断，报告不记录 secret、key 或原始敏感正文。
// - not_implemented / evidence_file 缺失都按显式失败处理并携带精确断言 ID。

import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import process from "node:process";

export const DEFAULT_REPORT_ROOT = path.join(
  "artifacts",
  "ai-tasks",
  "verification",
  "product-experience-gap-closure",
);

const STREAM_LIMIT = 32 * 1024;
const TRUNCATION_MARK = "\n…[截断]";

/** 脱敏：常见 token/key 形态一律打码；evidence-hygiene 与 runner 共用同一规则。 */
export function sanitizeText(text) {
  if (!text) return text;
  return String(text)
    .replace(/sk-[A-Za-z0-9_-]{12,}/g, "[REDACTED]")
    .replace(/gh[pousr]_[A-Za-z0-9]{20,}/g, "[REDACTED]")
    .replace(/github_pat_[A-Za-z0-9_]{20,}/g, "[REDACTED]")
    .replace(/AKIA[0-9A-Z]{16}/g, "[REDACTED]")
    .replace(/xox[baprs]-[A-Za-z0-9-]{10,}/g, "[REDACTED]")
    .replace(/Bearer\s+[A-Za-z0-9._~+/-]{8,}/gi, "Bearer [REDACTED]")
    .replace(/(api[_-]?key|token|secret|password)\s*[=:]\s*["']?[^\s"',;)]+/gi, "$1=[REDACTED]")
    .replace(/-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----/g, "[REDACTED-KEY]");
}

function clipStream(text) {
  if (text.length <= STREAM_LIMIT) return text;
  return text.slice(0, STREAM_LIMIT) + TRUNCATION_MARK;
}

function sha256(text) {
  return createHash("sha256").update(text).digest("hex");
}

export function commandLabel(command) {
  if (Array.isArray(command)) return command.map(String).join(" ");
  return String(command);
}

export function collectWorktreeDigest(rootDir) {
  const out = spawnSyncOutput("git", ["status", "--porcelain"], rootDir);
  const rev = spawnSyncOutput("git", ["rev-parse", "HEAD"], rootDir);
  if (out.error || rev.error) {
    return { revision: null, worktree_digest: null, dirty_files: null, error: true };
  }
  const status = out.stdout.trim();
  return {
    revision: rev.stdout.trim() || null,
    worktree_digest: sha256(status),
    dirty_files: status ? status.split("\n").length : 0,
    error: false,
  };
}

function spawnSyncOutput(bin, args, cwd) {
  const r = spawnSync(bin, args, { cwd, shell: false, encoding: "utf8", timeout: 15_000 });
  return { stdout: r.stdout ?? "", stderr: r.stderr ?? "", error: r.error ?? null };
}

export function runCommand(command, options = {}) {
  return new Promise((resolve) => {
    const argv = Array.isArray(command) ? command : commandLabel(command);
    let child;
    try {
      child = spawn(argv[0], argv.slice(1), {
        cwd: options.cwd ?? process.cwd(),
        shell: false,
        env: options.env ?? undefined,
        stdio: ["ignore", "pipe", "pipe"],
      });
    } catch (error) {
      resolve({ exit_code: null, timed_out: false, stdout: "", stderr: sanitizeText(String(error)) });
      return;
    }
    let stdout = "";
    let stderr = "";
    let timedOut = false;
    child.stdout.on("data", (d) => {
      stdout += d;
    });
    child.stderr.on("data", (d) => {
      stderr += d;
    });
    const timer = setTimeout(() => {
      timedOut = true;
      child.kill("SIGKILL");
    }, options.timeout_ms ?? 10 * 60 * 1000);
    child.on("error", (error) => {
      clearTimeout(timer);
      resolve({
        exit_code: null,
        timed_out: false,
        stdout: "",
        stderr: sanitizeText(`spawn error: ${error.message}\n${stderr}`),
      });
    });
    child.on("close", (code) => {
      clearTimeout(timer);
      resolve({
        exit_code: code,
        timed_out: timedOut,
        stdout: clipStream(sanitizeText(stdout)),
        stderr: clipStream(sanitizeText(stderr)),
      });
    });
  });
}

async function executeAssertion(assertion, profile, options = {}) {
  const base = {
    assertion_id: assertion.id,
    task_id: assertion.id.split(".")[0],
    level: assertion.level,
    profiles: assertion.profiles,
    note: assertion.note,
  };

  if (!assertion.profiles.includes(profile)) {
    return { ...base, outcome: "not_registered_for_profile", ok: false };
  }

  // 通过结果缓存：键=断言+profile+revision。失败的断言永远新鲜执行。
  // 目的是让 --through 累计门禁在同 revision 上重复验收时不重跑小时级子命令；
  // 缓存条目回填原报告路径，保持证据可追溯（PRD §13）。
  const revision = options.revision;
  if (options.cache !== false && revision && assertion.type === "command") {
    const cached = readCacheEntry(assertion.id, profile, revision);
    if (cached) {
      return {
        ...base,
        outcome: "passed",
        ok: true,
        command: cached.command,
        cached: true,
        cached_from_report: cached.report,
        duration_ms: 0,
        exit_code: 0,
      };
    }
  }

  switch (assertion.type) {
    case "command": {
      const started = Date.now();
      const run = await runCommand(assertion.command, {
        cwd: assertion.cwd,
        env: assertion.env,
        timeout_ms: assertion.timeout_ms,
      });
      const finished = Date.now();
      return {
        ...base,
        outcome: run.exit_code === 0 && !run.timed_out ? "passed" : "failed",
        ok: run.exit_code === 0 && !run.timed_out,
        command: commandLabel(assertion.command),
        duration_ms: finished - started,
        exit_code: run.exit_code,
        timed_out: run.timed_out,
        stdout: run.stdout || null,
        stderr: run.stderr || null,
      };
    }
    case "evidence_file": {
      let present = true;
      let detail = "";
      try {
        const raw = readFileSync(assertion.path, "utf8");
        detail = `${raw.length} bytes`;
      } catch (error) {
        present = false;
        detail = sanitizeText(error.message);
      }
      return {
        ...base,
        outcome: present ? "passed" : "failed",
        ok: present,
        evidence_path: path.relative(process.cwd(), assertion.path),
        detail,
      };
    }
    default:
      return { ...base, outcome: "not_implemented", ok: false };
  }
}

function cacheDir() {
  return path.join(process.cwd(), "artifacts", "ai-tasks", "verification", "product-experience-gap-closure", ".pass-cache");
}

function cacheFile(id, profile, revision) {
  const safeId = id.replace(/[^\w.-]+/g, "_");
  return path.join(cacheDir(), `${profile}__${safeId}__${revision.slice(0, 12)}.json`);
}

function readCacheEntry(id, profile, revision) {
  try {
    const entry = JSON.parse(readFileSync(cacheFile(id, profile, revision), "utf8"));
    if (entry?.ok === true && entry.revision === revision) return entry;
  } catch {
    /* cache miss */
  }
  return null;
}

export function writeCacheEntry(id, profile, revision, ok, commandLabel_, reportPath) {
  if (!ok || !revision) return;
  try {
    mkdirSync(cacheDir(), { recursive: true });
    writeFileSync(
      cacheFile(id, profile, revision),
      JSON.stringify({ schema_version: "pe-pass-cache.v1", id, profile, revision, ok, command: commandLabel_, report: reportPath }, null, 2),
      "utf8",
    );
  } catch {
    /* cache is best-effort */
  }
}

/** 选择集执行入口：selection 为任务 ID 数组（调用方已做依赖闭包校验）。
 *  报告落盘 <rootDir>/<reportRoot>/<profile>/<fileName>.json 并返回。
 *  registry 可注入 { REGISTRY, registryDigest }（自测合成 fixture 用）；默认读真实注册表。 */
export async function runVerification({
  selection,
  profile,
  reportRoot,
  rootDir,
  targetLabel,
  fileName,
  cache = true,
  registry = null,
}) {
  const { REGISTRY, registryDigest } = registry ?? (await import("./registry.mjs"));
  const meta = collectWorktreeDigest(rootDir);
  const results = [];
  for (const tid of selection) {
    for (const a of REGISTRY[tid].assertions) {
      const result = await executeAssertion(a, profile, { cache, revision: meta.revision });
      if (!result.cached && result.ok && a.type === "command") {
        writeCacheEntry(
          a.id,
          profile,
          meta.revision ?? "",
          true,
          commandLabel(a.command),
          path.join(reportRoot, profile, `${fileName}.json`),
        );
      }
      results.push(result);
    }
  }
  const summary = {
    total: results.length,
    passed: results.filter((r) => r.ok).length,
    failed: results.filter((r) => !r.ok).length,
    not_implemented: results.filter((r) => r.outcome === "not_implemented").length,
  };
  const ok = summary.total > 0 && summary.passed === summary.total;
  const report = {
    schema_version: "product-experience-verification.v1",
    requested_target: targetLabel,
    profile,
    ok,
    platform: {
      os: process.platform,
      arch: process.arch,
      node: process.version,
    },
    revision: meta.revision,
    worktree_digest: meta.worktree_digest,
    dirty_files: meta.dirty_files,
    finished_at: new Date().toISOString(),
    registry_digest: registryDigest(),
    failures: results.filter((r) => !r.ok).map((r) => r.assertion_id),
    summary,
    results,
  };
  const dir = path.join(rootDir, reportRoot, profile);
  mkdirSync(dir, { recursive: true });
  const file = path.join(dir, `${fileName}.json`);
  writeFileSync(file, JSON.stringify(report, null, 2) + "\n", "utf8");
  return { report, file };
}
