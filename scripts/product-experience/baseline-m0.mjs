#!/usr/bin/env node
// M0-02 基线采集器：重跑既有回归与核心 smoke，把「真实已完成 / 失败 / 外部 pending」
// 机器化记录为四态区分的 JSON。契约（PRD M0-02）：
// - 不 reset、不覆盖用户改动；基线失败如实入库，但"失败可追溯"本身即通过语义（A1），
//   因此本脚本 exit 0 ⇔ 每条计划腿都有被记录的结果；命令失败不吞、不省略。
// - 证据先脱敏再落盘：stdout/stderr 只保留尾部若干行且经 sanitizeText 清洗。
// - 三平台项在非 Windows 宿主上记 external-pending 并注明原因，同时保留
//   平台无关的实现 fixture（corpus-schema）作为本机可执行部分。

import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import path from "node:path";
import process from "node:process";

const ROOT = process.cwd();
const OUT_DIR = path.join(
  ROOT,
  "artifacts",
  "ai-tasks",
  "verification",
  "product-experience-gap-closure",
  "implementation",
);

// ---- revision 绑定的结果复用（与本仓库 runner.mjs 的 pass-cache 同思路）----
// - 上一份 m0-baseline.json 中 ok=true 且 HEAD revision 相同的腿直接引用，不重跑；
// - BASELINE_M0_REUSE="leg-a|leg-b" 显式指定失败腿按原记录冻结（不重跑），
//   用于天然超长（如 >70min 性能批）或环境失真已甄别的腿；复用会带
//   reused_no_rerun/reuse_reason 字段并打日志，审计可辨。
// - 其余腿一律现场重跑，保证失败不陈旧。
function loadPrevReport() {
  const file = path.join(OUT_DIR, "m0-baseline.json");
  if (!existsSync(file)) return null;
  try {
    const rep = JSON.parse(readFileSync(file, "utf8"));
    if (rep?.schema_version !== "product-experience-m0-baseline.v1") return null;
    return rep;
  } catch {
    return null;
  }
}

const PREV_REPORT = loadPrevReport();
function headRevision() {
  const r = spawnSync("git", ["rev-parse", "HEAD"], { cwd: ROOT, encoding: "utf8", shell: false });
  return r.status === 0 ? r.stdout.trim() : null;
}
const PREV_BY_NAME = new Map((PREV_REPORT?.legs ?? []).map((l) => [l.name, l]));
const REUSE_FAILED = new Set(
  String(process.env.BASELINE_M0_REUSE ?? "")
    .split("|")
    .map((s) => s.trim())
    .filter(Boolean),
);

/** 命中返回可复用腿（附审计字段），未命中返回 null（由调用方现场执行）。 */
function cachedLeg(name) {
  const prevLeg = PREV_BY_NAME.get(name);
  if (!prevLeg) return null;
  if (
    PREV_REPORT &&
    PREV_REPORT.revision === headRevision() &&
    prevLeg.ok === true &&
    !process.env.BASELINE_M0_NO_CACHE
  ) {
    console.log(`[leg-cached ${name}] ok 腿 revision 绑定复用自 ${PREV_REPORT.finished_at}`);
    return { ...prevLeg, cached: true, cached_from_finished_at: PREV_REPORT.finished_at };
  }
  if (REUSE_FAILED.has(name)) {
    console.log(`[leg-reused-no-rerun ${name}] BASELINE_M0_REUSE 指定：保留上一份结果不重跑`);
    return {
      ...prevLeg,
      reused_no_rerun: true,
      reuse_reason: `BASELINE_M0_REUSE 显式指定：冻结上一份(${PREV_REPORT?.finished_at ?? "?"})记录`,
    };
  }
  return null;
}
const PY = process.env.VERIFY_PRODUCT_EXPERIENCE_PYTHON ?? (process.platform === "win32" ? "python" : "python3");
const CARGO_ENV = { ...process.env, CARGO_NET_GIT_FETCH_WITH_CLI: "true" };

function sanitizeText(text) {
  return String(text ?? "")
    .replace(/sk-[A-Za-z0-9_-]{12,}/g, "[REDACTED]")
    .replace(/gh[pousr]_[A-Za-z0-9]{20,}/g, "[REDACTED]")
    .replace(/github_pat_[A-Za-z0-9_]{20,}/g, "[REDACTED]")
    .replace(/AKIA[0-9A-Z]{16}/g, "[REDACTED]")
    .replace(/Bearer\s+[A-Za-z0-9._~+/-]{8,}/gi, "Bearer [REDACTED]")
    .replace(/(api[_-]?key|password)\s*[:=]\s*["']?[^\s"',;)]+/gi, "$1=[REDACTED]");
}

function tail(text, lines = 6) {
  const clean = sanitizeText(text).trimEnd();
  if (!clean) return null;
  return clean.split("\n").slice(-lines).join("\n").slice(0, 2000);
}

// 2026-08-27 运行审计（PRD README §生产运行审计边界）→ 最早修复任务映射。
// 基线只登记问题与承接任务，不在 M0 阶段修复。
export const AUDIT_ISSUE_MAP = [
  { id: "AUDIT-1", issue: "canonical default 与 Settings 页投影不一致（空白页提示连接服务并回退 Codex）", owner_task: "M2-03/M3-04" },
  { id: "AUDIT-2", issue: "子代理候选回执过期导致首次委派失败，readiness 错误绑定设置页生命周期", owner_task: "M3-03" },
  { id: "AUDIT-3", issue: "父 Run 终态后子代理/工具/计时器仍 running，缺少 Host 级联与退出 ACK", owner_task: "M3-01/M3-02/M4-04" },
  { id: "AUDIT-4", issue: "同工作区三文件三张审批卡，缺 canonical WorkspaceBinding 同风险聚合", owner_task: "M1-02/M4-04" },
  { id: "AUDIT-5", issue: "打开执行台改变并重定位顶层窗口，而非 WebView 内响应式布局", owner_task: "M2-04/M4-03" },
  { id: "AUDIT-6", issue: "发送/停止按钮过渡误触中止且正文滞留，发送/追加/停止未解耦", owner_task: "M2-03" },
];

function version(cmdline) {
  const r = spawnSync(cmdline[0], cmdline.slice(1), { cwd: ROOT, encoding: "utf8", shell: false });
  return r.status === 0 ? (r.stdout.trim().split("\n")[0] || null) : null;
}

function runLeg(name, category, bin, args, options = {}) {
  const started = Date.now();
  const state = options.external_pending ? "external-pending" : undefined;
  if (state) {
    const pendingLeg = {
      name,
      category,
      command: [bin, ...args].join(" "),
      classification: state,
      reason: options.reason,
      exit_code: null,
      duration_ms: 0,
      stdout_tail: null,
      stderr_tail: null,
    };
    console.log(`[leg-start ${name}] external-pending（${options.reason ?? ""}）`);
    return pendingLeg;
  }
  console.log(`[leg-start ${name}] ${bin} ${args.join(" ")}`);
  const r = spawnSync(bin, args, {
    cwd: options.cwd ?? ROOT,
    encoding: "utf8",
    shell: false,
    timeout: options.timeout_ms ?? 45 * 60 * 1000,
    env: options.env ?? process.env,
    maxBuffer: 64 * 1024 * 1024,
  });
  const timedOut = r.signal === "SIGTERM";
  const leg = {
    name,
    category,
    command: [bin, ...args].join(" "),
    classification: options.classification_hint ?? "implemented",
    exit_code: r.status ?? null,
    timed_out: timedOut,
    ok: r.status === 0 && !timedOut,
    duration_ms: Date.now() - started,
    stdout_tail: tail(r.stdout),
    stderr_tail: tail(r.stderr),
  };
  console.log(
    `[leg-end ${name}] exit=${leg.exit_code}${timedOut ? " TIMEOUT" : ""} ok=${leg.ok} (${Math.round(leg.duration_ms / 1000)}s)`,
  );
  if (!leg.ok && leg.stderr_tail) {
    console.log(`[leg-err ${name}] ${(leg.stderr_tail.split("\n").slice(-3).join(" | ") || "").slice(0, 300)}`);
  }
  return leg;
}

// 前端套件串行单进程在本机 >100min；改为按权重分散到 N 个并行批次，
// 每批一个 `node --test` 进程（内部仍串行），总墙钟 ≈ 最慢批次。
// 批次支持缓存/复用：按批名（frontend:npm-test-batch-N）查 revision 绑定
// 缓存与 BASELINE_M0_REUSE，命中则不 spawn 子进程、直接引用上一份记录；
// 校验上一份 command 的脚本清单与本次分桶一致才允许复用。
const FRONTEND_BATCHES = 4;
const KNOWN_SLOW_FRONTEND = new Set([
  "activity-archive-ui.test.mjs",
  "app-shell.test.mjs",
  "companion-window-ui.test.mjs",
  "companion.test.mjs",
  "codex-full-flow-visual.test.mjs",
  "codex-performance.test.mjs",
  "long-content-performance.test.mjs",
  "timeline-incremental-performance.test.mjs",
]);

function buildFrontendBuckets() {
  const dir = path.join(ROOT, "src-tauri", "frontend", "scripts");
  const files = readdirSync(dir)
    .filter((f) => f.endsWith(".test.mjs") && f !== "verify-product-experience.test.mjs")
    .sort();
  // 已知 >20min 的极重性能用例独占批次，避免把同批文件拖入 70 分钟强杀。
  const SOLO_WEIGHT_FILES = new Set(["codex-performance.test.mjs"]);
  const buckets = Array.from({ length: FRONTEND_BATCHES }, () => ({ files: [], weight: 0 }));
  const ordered = [...files].sort(
    (a, b) => (KNOWN_SLOW_FRONTEND.has(b) ? 1 : 0) - (KNOWN_SLOW_FRONTEND.has(a) ? 1 : 0),
  );
  let soloIndex = 0;
  for (const f of ordered) {
    if (SOLO_WEIGHT_FILES.has(f) && soloIndex < FRONTEND_BATCHES) {
      buckets[soloIndex].files.push(f);
      buckets[soloIndex].weight += 40;
      soloIndex += 1;
      continue;
    }
    const target = buckets.reduce((min, b) => (b.weight < min.weight ? b : min), buckets[0]);
    const w = KNOWN_SLOW_FRONTEND.has(f) ? 12 : 1;
    target.files.push(f);
    target.weight += w;
  }
  return { files, buckets };
}

function batchCommand(files) {
  return `node --test scripts/${files.map((f) => `scripts/${f}`).join(" scripts/")}`;
}

function spawnFrontendBatch(bucket, index) {
  return new Promise((resolve) => {
    const startedAt = Date.now();
    const child = spawn(process.execPath, ["--test", ...bucket.files.map((f) => `scripts/${f}`)], {
      cwd: path.join(ROOT, "src-tauri", "frontend"),
      stdio: ["ignore", "pipe", "pipe"],
    });
    let out = "";
    let err = "";
    child.stdout.on("data", (d) => {
      out += d;
    });
    child.stderr.on("data", (d) => {
      err += d;
    });
    let timedOut = false;
    const timer = setTimeout(() => {
      timedOut = true;
      child.kill("SIGKILL");
    }, 70 * 60 * 1000);
    child.on("error", (error) => {
      clearTimeout(timer);
      resolve({ ok: false, exit_code: null, timed_out: false, stderr_tail: sanitizeText(error.message), duration_ms: Date.now() - startedAt });
    });
    child.on("close", (code) => {
      clearTimeout(timer);
      const tallyLines = out
        .split("\n")
        .filter((l) => /^ℹ (tests|pass|fail|skipped)\b/.test(l.trim()))
        .slice(-4)
        .map((l) => l.trim());
      console.log(`[leg-end frontend:batch-${index + 1}] exit=${code}${timedOut ? " TIMEOUT" : ""} files=${bucket.files.length} :: ${tallyLines.join(" ") || "(被强杀，无汇总)"}`);
      resolve({
        ok: code === 0 && !timedOut,
        exit_code: code,
        timed_out: timedOut,
        stdout_tail: tail(out, 24),
        stderr_tail: tail(err, 14),
        duration_ms: Date.now() - startedAt,
      });
    });
  });
}

async function frontendSection(recorder) {
  const { files, buckets } = buildFrontendBuckets();
  console.log(`[leg-start frontend:npm-test-batched] ${FRONTEND_BATCHES} 并行批次 / ${files.length} 文件`);
  const outcomes = Array.from({ length: FRONTEND_BATCHES }, () => null);
  const pending = [];
  for (let i = 0; i < FRONTEND_BATCHES; i += 1) {
    if (buckets[i].files.length === 0) continue;
    const name = `frontend:npm-test-batch-${i + 1}`;
    const hit = cachedLeg(name);
    // 显式复用（BASELINE_M0_REUSE）不受清单一致性约束——操作者冻结的就是历史事实；
    // ok 缓存仍要求脚本清单一致，防止 revision 相同但分桶漂移的陈旧通过被引用。
    if (hit && (hit.reused_no_rerun || hit.command === batchCommand(buckets[i].files))) {
      outcomes[i] = hit;
      continue;
    }
    if (hit) console.log(`[leg-cache-miss ${name}] 脚本清单与上一份报告不一致，现场重跑`);
    pending.push(
      spawnFrontendBatch(buckets[i], i).then((r) => {
        outcomes[i] = { ...r, files: buckets[i].files };
      }),
    );
  }
  await Promise.all(pending);
  let anyOk = true;
  for (let i = 0; i < FRONTEND_BATCHES; i += 1) {
    const r = outcomes[i];
    if (!r) continue;
    anyOk = anyOk && r.ok;
    recorder({
      name: `frontend:npm-test-batch-${i + 1}`,
      category: "frontend-core",
      command: r.command ?? batchCommand(r.files ?? []),
      classification: r.classification ?? "implemented",
      exit_code: r.exit_code,
      timed_out: r.timed_out ?? false,
      ok: r.ok && !(r.timed_out ?? false),
      duration_ms: r.duration_ms ?? 0,
      stdout_tail: r.stdout_tail ?? null,
      stderr_tail: r.stderr_tail ?? null,
      ...(r.cached
        ? { cached: true, cached_from_finished_at: r.cached_from_finished_at }
        : {}),
      ...(r.reused_no_rerun ? { reused_no_rerun: true, reuse_reason: r.reuse_reason } : {}),
    });
  }
  return anyOk;
}

async function mainAsync() {
  legs.push(cachedLeg("docs:worklist-gate-check") ?? runLeg("docs:worklist-gate-check", "doc-consistency", PY, ["docs/product-experience-redesign/tools/worklist_gate.py", "--check"]));
  legs.push(cachedLeg("docs:markdown-links") ?? runLeg("docs:markdown-links", "doc-consistency", PY, ["docs/product-experience-redesign/tools/check_markdown_links.py"]));
  await frontendSection((leg) => legs.push(leg));
  legs.push(cachedLeg("rust:cargo-test-workspace") ?? runLeg("rust:cargo-test-workspace", "rust-core", "cargo", ["test", "--workspace"], { env: CARGO_ENV }));
  legs.push(cachedLeg("rich-interaction:through-M4") ?? runLeg("rich-interaction:through-M4", "regression-rerun", process.execPath, ["scripts/verify-codex-interaction.mjs", "--through", "M4", "--profile", "implementation"]));
  if (process.platform === "win32") {
    legs.push(runLeg("windows-reliability:corpus-fast", "regression-rerun", process.execPath, ["scripts/windows-reliability/corpus-run.mjs", "--tier", "fast"]));
  } else {
    const pending = cachedLeg("windows-reliability:corpus-fast");
    legs.push(
      pending ?? runLeg("windows-reliability:corpus-fast", "regression-rerun", "", [], {
        external_pending: true,
        reason: "corpus 依赖 pwsh/Git Bash 方言与真实 gateway 执行路径，属 Windows 宿主项；本平台以 corpus-schema 保持实现面",
      }),
    );
  }
  legs.push(
    cachedLeg("windows-reliability:corpus-schema") ??
      runLeg("windows-reliability:corpus-schema", "regression-rerun", process.execPath, ["scripts/windows-reliability/corpus-schema.mjs"], {
        classification_hint: "implemented-platform-neutral-fixture",
      }),
  );
}

const legs = [];

mainAsync()
  .then(() => {

const summary = {
  total: legs.length,
  // 冻结复用（reused_no_rerun）的腿视为已记录——exit_code 沿用历史值（可能是
  // 强杀产生的 null），A1 的通过语义是「每条计划腿都有被记录的结果」。
  executed: legs.filter((l) => l.exit_code !== null || l.reused_no_rerun).length,
  passed: legs.filter((l) => l.ok).length,
  failed: legs.filter((l) => (l.exit_code !== null || l.reused_no_rerun) && !l.ok).length,
  external_pending: legs.filter((l) => l.classification === "external-pending").length,
};

const report = {
  schema_version: "product-experience-m0-baseline.v1",
  baseline_of_worklist: "product-experience-gap-closure",
  finished_at: new Date().toISOString(),
  revision: version(["git", "rev-parse", "HEAD"]) ?? null,
  environment: {
    node: process.version,
    npm: version(["npm", "--version"]),
    cargo: version(["cargo", "--version"]),
    python: version([PY, "--version"]),
    platform: `${process.platform}/${process.arch}`,
    dirty_files: (() => {
      const g = spawnSync("git", ["status", "--porcelain"], { cwd: ROOT, encoding: "utf8", shell: false });
      return g.status === 0 && typeof g.stdout === "string" && g.stdout.length > 0
        ? g.stdout.trimEnd().split("\n").length
        : 0;
    })(),
  },
  audit_issue_map: AUDIT_ISSUE_MAP,
  summary,
  legs,
};

mkdirSync(OUT_DIR, { recursive: true });
const file = path.join(OUT_DIR, "m0-baseline.json");
writeFileSync(file, JSON.stringify(report, null, 2) + "\n", "utf8");

for (const l of legs) {
  const mark =
    l.classification === "external-pending"
      ? "PEND"
      : l.ok
        ? "PASS"
        : "FAIL";
  console.log(`${mark} ${l.name}${l.duration_ms ? ` (${Math.round(l.duration_ms / 1000)}s)` : ""}`);
}
console.log(`report: ${path.relative(ROOT, file)} legs=${summary.total} failed=${summary.failed} external_pending=${summary.external_pending}`);
process.exitCode = summary.executed + summary.external_pending === summary.total ? 0 : 1;
  })
  .catch((error) => {
    console.error("baseline collector crashed:", error);
    process.exitCode = 1;
  });
