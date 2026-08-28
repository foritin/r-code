// 金集执行编排（M0-01.A3 / M1-01.A3 / M1-02.A4 / M4-02.A1 共用）：
//   node scripts/windows-reliability/corpus-run.mjs --tier fast|slow|all \
//     --check dialect-field --check dialect=git-bash \
//     --check categories=dialect-chain,env-prefix,quoting \
//     --check no-mojibake --check both-executed=1 --check thresholds
//
// 行为：
// 1. 在净化 PATH（剥离 Git Bash 的 bin/usr/bin/mingw64 目录，模拟 GUI 启动的
//    干净环境）下运行 `cargo test -p r-code-gateway --test command_corpus_runner
//    -- --ignored`（金集测试标记 #[ignore]，默认 cargo test 如实报 ignored），
//    通过 CORPUS_RUN / CORPUS_GIT_SHA 环境变量选择档位与报告 sha；
// 2. 读取生成的 report-<sha>-<platform>.json，按 --check 断言检查；
// 3. 任一检查失败退出码 1 并列出精确条目。
//
// `ok` 语义见 Rust runner 头注：结果符合 expect 即 ok（含预期失败）。

import { spawnSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import path from "node:path";
import process from "node:process";

const ROOT = path.resolve(import.meta.dirname, "..", "..");
const REPORT_DIR = path.join(ROOT, "artifacts", "metrics", "command-corpus");
const CORPUS_TIMEOUT_MS = 15 * 60 * 1000;

function hostPlatform() {
  if (process.platform === "win32") {
    return "windows";
  }
  if (process.platform === "darwin") {
    return "darwin";
  }
  console.error(`corpus-run: unsupported host platform ${process.platform}`);
  process.exit(2);
}

/// 剥离 PATH 中 Git Bash 注入的目录（bin/usr/bin/mingw64/bin），
/// 还原 GUI 启动时的干净 PATH——基线（PowerShell 档）与改造后
/// （Git Bash 经绝对路径解析）都在同一环境下结算。
function sanitizedPathEnv(pathEnv) {
  if (process.platform !== "win32") {
    return pathEnv;
  }
  const entries = pathEnv.split(";");
  const gitBashDir = /\\git\\(usr\\bin|mingw64\\bin|bin)\\?$/i;
  const kept = entries.filter((entry) => !gitBashDir.test(entry.trim()));
  return kept.join(";");
}

function gitSha() {
  const rev = spawnSync("git", ["rev-parse", "HEAD"], { cwd: ROOT, encoding: "utf8", timeout: 15_000 });
  if (rev.status !== 0) {
    console.error("corpus-run: git rev-parse HEAD failed");
    process.exit(2);
  }
  return rev.stdout.trim();
}

function parseChecks(args) {
  const checks = [];
  for (let index = 0; index < args.length; index += 1) {
    if (args[index] === "--check" || args[index] === "-c") {
      checks.push(args[index + 1]);
      index += 1;
    }
  }
  return checks;
}

function applyCheck(check, report, failures) {
  if (check === "dialect-field") {
    if (typeof report.dialect !== "string" || report.dialect.length === 0) {
      failures.push(`dialect-field: 报告缺少非空 dialect 字段`);
    }
    return;
  }
  if (check.startsWith("dialect=")) {
    const expected = check.slice("dialect=".length);
    if (report.dialect !== expected) {
      failures.push(`dialect: 期望 ${expected}，实际 ${report.dialect}`);
    }
    return;
  }
  if (check.startsWith("categories=")) {
    const categories = check.slice("categories=".length).split(",");
    for (const category of categories) {
      const unmet = (report.commands ?? []).filter(
        (entry) => entry.category === category && !entry.met,
      );
      if (unmet.length > 0) {
        failures.push(
          `categories: ${category} 类未全绿 — ${unmet.map((entry) => `${entry.id}(expect=${entry.expect},exit=${entry.exit_code},blocked=${entry.blocked},hint=${entry.hint_present})`).join(", ")}`,
        );
      } else if (!(report.commands ?? []).some((entry) => entry.category === category)) {
        failures.push(`categories: ${category} 类在本报告中没有任何执行条目`);
      }
    }
    return;
  }
  if (check === "no-mojibake") {
    const lossy = (report.commands ?? []).filter((entry) => entry.utf8_loss);
    if (lossy.length > 0) {
      failures.push(`no-mojibake: 输出含 U+FFFD 替换符 — ${lossy.map((entry) => entry.id).join(", ")}`);
    }
    return;
  }
  if (check.startsWith("both-executed=")) {
    const minimum = Number(check.slice("both-executed=".length));
    const executed = (report.tiers_run ?? []).length >= 0 && report.total;
    if (!executed || report.total < minimum) {
      failures.push(`both-executed: 报告执行条数 ${report.total ?? 0} 低于 ${minimum}`);
    }
    return;
  }
  if (check === "thresholds") {
    const total = report.total ?? 0;
    const okRate = total > 0 ? (report.ok ?? 0) / total : 0;
    const dialectRate = total > 0 ? (report.dialect_failures ?? 0) / total : 1;
    if (okRate < 0.96) {
      failures.push(`thresholds: 符合率 ${(okRate * 100).toFixed(1)}% 低于 96%（ok=${report.ok}/${total}）`);
    }
    if (dialectRate >= 0.02) {
      failures.push(
        `thresholds: 方言类失败占比 ${(dialectRate * 100).toFixed(1)}% 不低于 2%（dialect_failures=${report.dialect_failures}/${total}）`,
      );
    }
    return;
  }
  failures.push(`未知 check: ${check}`);
}

async function main() {
  const args = process.argv.slice(2);
  const tierFlag = args.indexOf("--tier");
  const tier = tierFlag >= 0 ? args[tierFlag + 1] : "fast";
  if (!["fast", "slow", "all"].includes(tier)) {
    console.error("corpus-run: --tier 必须是 fast|slow|all");
    process.exit(2);
  }
  const checks = parseChecks(args);
  const platform = hostPlatform();
  const sha = gitSha();
  const tagFlag = args.indexOf("--tag");
  const tag = tagFlag >= 0 ? (args[tagFlag + 1] ?? "") : "";

  const childEnv = {
    ...process.env,
    PATH: sanitizedPathEnv(process.env.PATH ?? ""),
    CORPUS_RUN: tier,
    CORPUS_GIT_SHA: sha,
    ...(tag ? { CORPUS_REPORT_TAG: tag } : {}),
  };
  process.stdout.write(`corpus-run: CORPUS_RUN=${tier} sha=${sha.slice(0, 10)} platform=${platform}（PATH 已净化）\n`);
  const run = spawnSync(
    "cargo",
    ["test", "-p", "r-code-gateway", "--test", "command_corpus_runner", "--", "--ignored", "--nocapture"],
    { cwd: ROOT, encoding: "utf8", timeout: CORPUS_TIMEOUT_MS, maxBuffer: 32 * 1024 * 1024, env: childEnv },
  );
  if (run.error) {
    console.error(`corpus-run: cargo 启动失败 — ${run.error.message}`);
    process.exit(1);
  }
  if (run.status !== 0) {
    console.error(`corpus-run: 金集 runner 退出码 ${run.status}`);
    const tail = `${run.stdout ?? ""}\n${run.stderr ?? ""}`.trimEnd().slice(-4000);
    console.error(tail);
    process.exit(1);
  }
  const summaryLine = `${run.stdout ?? ""}`.split(/\r?\n/).filter((line) => line.startsWith("corpus-"));
  for (const line of summaryLine) {
    process.stdout.write(`${line}\n`);
  }

  const reportName = tag
    ? `report-${sha}-${platform}-${tag}.json`
    : `report-${sha}-${platform}.json`;
  const reportPath = path.join(REPORT_DIR, reportName);
  let report;
  try {
    report = JSON.parse(await readFile(reportPath, "utf8"));
  } catch (error) {
    console.error(`corpus-run: 无法读取报告 ${reportPath} — ${error.message}`);
    process.exit(1);
  }

  const failures = [];
  for (const check of checks) {
    applyCheck(check, report, failures);
  }
  if (failures.length > 0) {
    console.error(`corpus-run: ${failures.length} 项检查失败：`);
    for (const failure of failures) {
      console.error(`  - ${failure}`);
    }
    process.exit(1);
  }
  console.log(
    `corpus-run OK: report=${path.relative(ROOT, reportPath)} checks=${checks.length} total=${report.total} ok=${report.ok} dialect=${report.dialect}`,
  );
}

await main();
