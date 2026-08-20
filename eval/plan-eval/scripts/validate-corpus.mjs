#!/usr/bin/env node
/**
 * 冻结 corpus 验证器（docs §16.1/§16.2/§16.4）：
 * - 数量与分层：5 类 × 5 = 25 case；路由 probe 20 simple + 20 complex；
 * - 初始测试红：每个 verify.mjs 在原始 fixture 上必须失败；
 * - oracle patch 绿：应用 oracle.patch 后 verify.mjs 必须通过；
 * - corpus-lock.json（存在时）的 sha256 必须逐文件匹配；`--freeze` 生成新锁。
 *
 * 退出码非 0 即验证失败；fail closed，不产出部分结论。
 */
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

const root = join(import.meta.dirname, "..");
const corpusDir = join(root, "corpus");
const lockPath = join(root, "corpus-lock.json");
const CATEGORIES = ["bugfix", "feature", "migration", "performance", "safety"];

const freeze = process.argv.includes("--freeze");

function fail(message) {
  console.error(`validate-corpus: FAIL: ${message}`);
  process.exit(1);
}

function sh(command, args, options = {}) {
  return execFileSync(command, args, { stdio: ["ignore", "pipe", "pipe"], timeout: 120_000, ...options })
    .toString();
}

const caseDirs = readdirSync(corpusDir).filter((name) =>
  statSync(join(corpusDir, name)).isDirectory(),
).sort();
if (caseDirs.length !== 25) fail(`expected 25 cases, found ${caseDirs.length}`);

const byCategory = new Map(CATEGORIES.map((category) => [category, []]));
for (const caseId of caseDirs) {
  const dir = join(corpusDir, caseId);
  for (const required of ["case.json", "oracle.patch", "verify.mjs"]) {
    if (!existsSync(join(dir, required))) fail(`${caseId} missing ${required}`);
  }
  const meta = JSON.parse(readFileSync(join(dir, "case.json"), "utf8"));
  if (meta.id !== caseId) fail(`${caseId} case.json id mismatch: ${meta.id}`);
  if (!CATEGORIES.includes(meta.category)) fail(`${caseId} bad category ${meta.category}`);
  if (!Array.isArray(meta.expected_signals) || meta.expected_signals.length === 0) {
    fail(`${caseId} must declare expected_signals`);
  }
  byCategory.get(meta.category).push(caseId);
}
for (const category of CATEGORIES) {
  if (byCategory.get(category).length !== 5) {
    fail(`category ${category} must have exactly 5 cases, has ${byCategory.get(category).length}`);
  }
}

// 路由 probe 分层（与能力实验完全分离）。
const probes = JSON.parse(readFileSync(join(root, "routing", "probes.json"), "utf8"));
const simple = probes.filter((probe) => probe.label === "simple").length;
const complex = probes.filter((probe) => probe.label === "complex").length;
if (simple !== 20 || complex !== 20) {
  fail(`routing probes must be 20 simple + 20 complex, got ${simple}+${complex}`);
}
const probeIds = new Set(probes.map((probe) => probe.id));
if (probeIds.size !== probes.length) fail("routing probe ids must be unique");

// 初始红 / oracle 绿：把 fixture 复制到临时 git 工作区再 apply，绝不污染 corpus。
const stagingRoot = mkdtempSync(join(tmpdir(), "plan-eval-corpus-"));
let failures = 0;
try {
  for (const caseId of caseDirs) {
    const dir = join(corpusDir, caseId);
    // argv[2] 指向被测目录：RED 检查传原始 fixture，GREEN 检查传打过 oracle
    // patch 的临时工作区；verify 脚本本身永远留在 corpus 目录。
    const runVerify = (cwd) =>
      sh(process.execPath, [join(dir, "verify.mjs"), cwd], { cwd, timeout: 60_000 });

    let red = false;
    try {
      runVerify(join(dir, "fixture"));
    } catch {
      red = true;
    }
    if (!red) {
      console.error(`${caseId}: verify must be RED on the pristine fixture`);
      failures += 1;
      continue;
    }

    const staging = join(stagingRoot, caseId);
    mkdirSync(staging, { recursive: true });
    cpSync(join(dir, "fixture"), staging, { recursive: true });
    sh("git", ["init", "-q"], { cwd: staging });
    sh("git", ["add", "."], { cwd: staging });
    sh("git", ["-c", "user.email=eval@r-code", "-c", "user.name=plan-eval", "commit", "-qm", "fixture"], { cwd: staging });
    try {
      sh("git", ["apply", "--whitespace=nowarn", join(dir, "oracle.patch")], { cwd: staging });
      runVerify(staging);
    } catch (error) {
      console.error(`${caseId}: oracle patch must make verify GREEN (${String(error).split("\n")[0]})`);
      failures += 1;
    }
  }
} finally {
  rmSync(stagingRoot, { recursive: true, force: true });
}
if (failures > 0) fail(`${failures} case(s) failed the red/green contract`);

// corpus 锁：case + probe 文件的 sha256 清单。
function hashTree() {
  const entries = {};
  for (const caseId of caseDirs) {
    const dir = join(corpusDir, caseId);
    for (const name of ["case.json", "oracle.patch", "verify.mjs"]) {
      entries[`corpus/${caseId}/${name}`] = createHash("sha256")
        .update(readFileSync(join(dir, name)))
        .digest("hex");
    }
    const walk = (relative) => {
      const base = join(dir, "fixture");
      for (const entry of readdirSync(join(base, relative))) {
        const child = join(relative, entry);
        if (statSync(join(base, child)).isDirectory()) {
          walk(child);
        } else {
          entries[`corpus/${caseId}/fixture/${child.split("\\").join("/")}`] = createHash("sha256")
            .update(readFileSync(join(base, child)))
            .digest("hex");
        }
      }
    };
    walk("");
  }
  entries["routing/probes.json"] = createHash("sha256")
    .update(readFileSync(join(root, "routing", "probes.json")))
    .digest("hex");
  return entries;
}

const tree = hashTree();
if (freeze) {
  writeFileSync(lockPath, JSON.stringify({
    schema: "r-code-plan-corpus-lock/v1",
    cases: caseDirs.length,
    probes: probes.length,
    files: tree,
  }, null, 2) + "\n");
  console.log(`validate-corpus: OK (25 cases red/green; lock frozen over ${Object.keys(tree).length} files)`);
} else if (existsSync(lockPath)) {
  const lock = JSON.parse(readFileSync(lockPath, "utf8"));
  const drift = [];
  for (const [path, digest] of Object.entries(lock.files ?? {})) {
    if (tree[path] !== digest) drift.push(path);
  }
  for (const path of Object.keys(tree)) {
    if (!(path in (lock.files ?? {}))) drift.push(`${path} (unlocked)`);
  }
  if (drift.length > 0) {
    fail(`corpus drift vs corpus-lock.json:\n  ${drift.join("\n  ")}`);
  }
  console.log("validate-corpus: OK (25 cases red/green; corpus-lock matches)");
} else {
  console.log("validate-corpus: OK (25 cases red/green; no lock present — run with --freeze to freeze)");
}
