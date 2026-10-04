#!/usr/bin/env node
// Appendix-A fixture preparation (project-context acceptance).
// Clones + pins + verifies the A/B fixtures under
// sandbox/project-context-acceptance/fixtures/ and writes a machine-readable
// fixtures.json manifest. Idempotent: an existing fixture pinned at the
// recorded commit is left untouched.
//
// Usage:
//   node docs/prd/project-context/acceptance/scripts/prepare-fixtures.mjs \
//     [--source <r-code checkout>] [--group a|b] [--force]

import { execFileSync } from "node:child_process";
import { dirname } from "node:path";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import process from "node:process";

const ACCEPT_ROOT = path.resolve(dirname(fileURLToPath(import.meta.url)), "..");
const WORK_ROOT = path.join(path.resolve(ACCEPT_ROOT, "../../../.."), "sandbox", "project-context-acceptance");
const FIXTURE_ROOT = path.join(WORK_ROOT, "fixtures");
const MANIFEST = path.join(FIXTURE_ROOT, "fixtures.json");

const A_COMMIT = "2b894c21feddee5efba079ae38009b9b38a5ed02";
const B_REPO = "https://github.com/jqlang/jq.git";
const B_TAG = "jq-1.7.1";
const B_LINE_RANGE = [50_000, 100_000];

// Instruction files whose presence defines the A/B contrast (FR-1.1 plus the
// sibling formats named by FR-2.5). B must have none of them anywhere.
const INSTRUCTION_FILES = ["AGENTS.md", "CLAUDE.md", "GEMINI.md", ".cursorrules", ".windsurfrules"];

function parseArgs() {
  const args = { source: path.resolve(ACCEPT_ROOT, "../../../.."), group: null, force: false };
  const argv = process.argv.slice(2);
  for (let i = 0; i < argv.length; i += 1) {
    if (argv[i] === "--source") args.source = path.resolve(argv[++i]);
    else if (argv[i] === "--group") args.group = argv[++i];
    else if (argv[i] === "--force") args.force = true;
    else { console.error(`unknown argument ${argv[i]}`); process.exit(1); }
  }
  if (args.group && !["a", "b"].includes(args.group)) {
    console.error("--group must be a or b"); process.exit(1);
  }
  return args;
}

function git(cwd, ...rest) {
  return execFileSync("git", ["-C", cwd, ...rest], { encoding: "utf8", stdio: ["ignore", "pipe", "inherit"] }).trim();
}

function revParseHead(cwd) {
  return git(cwd, "rev-parse", "HEAD");
}

function manifest() {
  if (!existsSync(MANIFEST)) return {};
  try { return JSON.parse(readFileSync(MANIFEST, "utf8")); } catch { return {}; }
}

function findFiles(root, names) {
  // git ls-files keeps this fast and ignores nothing we care about; the
  // untracked check via ls-files --others covers freshly created files too.
  const tracked = git(root, "ls-files");
  const others = git(root, "ls-files", "--others", "--exclude-standard");
  const wanted = new Set(names);
  return [...tracked.split("\n"), ...others.split("\n")]
    .filter((f) => wanted.has(path.posix.basename(f.replace(/\\/g, "/"))));
}

function countCLines(root) {
  const tracked = git(root, "ls-files", "*.c", "*.h");
  let total = 0;
  for (const rel of tracked.split("\n").filter(Boolean)) {
    const abs = path.join(root, rel);
    try { total += readFileSync(abs, "utf8").split("\n").length; } catch { /* binary-ish; skip */ }
  }
  return total;
}

function prepareGroupA(args, mfst) {
  const dir = path.join(FIXTURE_ROOT, "a-r-code");
  console.log(`[A] r-code snapshot -> ${dir}`);
  if (!existsSync(path.join(dir, ".git")) || args.force) {
    mkdirSync(FIXTURE_ROOT, { recursive: true });
    execFileSync("git", ["clone", "--no-hardlinks", "--recurse-submodules", args.source, dir], { stdio: "inherit" });
  }
  git(dir, "checkout", "--detach", A_COMMIT);
  git(dir, "submodule", "update", "--init", "--recursive");
  const commit = revParseHead(dir);
  if (commit !== A_COMMIT) throw new Error(`A fixture pinned at ${commit}, expected ${A_COMMIT}`);
  const found = findFiles(dir, ["AGENTS.md"]).filter((f) => path.posix.dirname(f) === ".");
  if (found.length === 0) throw new Error("A fixture must have a root AGENTS.md (it is the instruction-bearing group)");
  console.log(`[A] pinned ${commit}; root AGENTS.md present`);
  mfst.a = { repo: args.source, commit, verifiedAt: new Date().toISOString() };
}

function prepareGroupB(args, mfst) {
  const dir = path.join(FIXTURE_ROOT, "b-jq");
  console.log(`[B] jq snapshot -> ${dir}`);
  const pinned = mfst.b?.commit;
  if (!existsSync(path.join(dir, ".git")) || args.force) {
    mkdirSync(FIXTURE_ROOT, { recursive: true });
    execFileSync("git", ["clone", "--branch", B_TAG, "--single-branch", B_REPO, dir], { stdio: "inherit" });
  }
  git(dir, "checkout", "--detach", pinned ?? B_TAG);
  const commit = revParseHead(dir);
  if (pinned && commit !== pinned) throw new Error(`B fixture drifted: ${commit} != pinned ${pinned}`);
  if (!pinned) console.log(`[B] resolved ${B_TAG} -> ${commit} (record as the frozen pin)`);
  const hits = findFiles(dir, INSTRUCTION_FILES);
  if (hits.length > 0) throw new Error(`B fixture must have zero instruction files, found: ${hits.join(", ")}`);
  const lines = countCLines(dir);
  if (lines < B_LINE_RANGE[0] || lines > B_LINE_RANGE[1]) {
    console.warn(`[B] WARNING: C line count ${lines} outside target range ${B_LINE_RANGE.join("-")} — re-pin per fixtures.md if this is a first prepare`);
  } else {
    console.log(`[B] C lines ${lines} in range`);
  }
  console.log(`[B] pinned ${commit}; instruction-file scan clean`);
  mfst.b = { repo: B_REPO, tag: B_TAG, commit, cLines: lines, verifiedAt: new Date().toISOString() };
}

const args = parseArgs();
const mfst = manifest();
if (!args.group || args.group === "a") prepareGroupA(args, mfst);
if (!args.group || args.group === "b") prepareGroupB(args, mfst);
writeFileSync(MANIFEST, `${JSON.stringify(mfst, null, 2)}\n`);
console.log(`manifest -> ${MANIFEST}`);
