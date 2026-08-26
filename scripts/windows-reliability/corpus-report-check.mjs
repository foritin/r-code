// M0-02.A1：金集基线报告存在性与 schema 检查。
//   node scripts/windows-reliability/corpus-report-check.mjs --platform windows --min-total 40
// 扫描 artifacts/metrics/command-corpus/report-*-<platform>.json，取 generated_at
// 最新一份，校验 §4.4 必填字段与 total 下限。

import { readdir, readFile } from "node:fs/promises";
import path from "node:path";
import process from "node:process";

const ROOT = path.resolve(import.meta.dirname, "..", "..");
const REPORT_DIR = path.join(ROOT, "artifacts", "metrics", "command-corpus");
const REQUIRED_FIELDS = [
  "git_sha",
  "platform",
  "dialect",
  "total",
  "ok",
  "fail",
  "dialect_failures",
  "hint_hits",
];

async function main() {
  const args = process.argv.slice(2);
  const readFlag = (name, fallback) => {
    const index = args.indexOf(`--${name}`);
    return index >= 0 ? args[index + 1] : fallback;
  };
  const platform = readFlag("platform", "windows");
  const suffix = readFlag("suffix", "");
  const minTotal = Number(readFlag("min-total", "40"));

  let files;
  try {
    files = (await readdir(REPORT_DIR)).filter(
      (name) =>
        name.startsWith("report-") &&
        name.endsWith(suffix ? `-${platform}-${suffix}.json` : `-${platform}.json`),
    );
  } catch (error) {
    console.error(`corpus-report-check: 无法读取 ${REPORT_DIR} — ${error.message}`);
    process.exit(1);
  }
  if (files.length === 0) {
    console.error(`corpus-report-check: 没有任何 report-*-${platform}.json 基线报告`);
    process.exit(1);
  }

  const reports = [];
  for (const name of files) {
    try {
      reports.push({ name, report: JSON.parse(await readFile(path.join(REPORT_DIR, name), "utf8")) });
    } catch {
      console.error(`corpus-report-check: ${name} 不是合法 JSON`);
      process.exit(1);
    }
  }
  reports.sort((a, b) => String(b.report.generated_at ?? "").localeCompare(String(a.report.generated_at ?? "")));
  const { name, report } = reports[0];

  const failures = [];
  for (const field of REQUIRED_FIELDS) {
    if (!(field in report)) {
      failures.push(`缺少字段 ${field}`);
    }
  }
  if (typeof report.git_sha !== "string" || report.git_sha === "unknown") {
    failures.push("git_sha 缺失或为 unknown");
  }
  if (report.platform !== platform) {
    failures.push(`platform 字段 ${report.platform} 与请求 ${platform} 不符`);
  }
  if (typeof report.dialect !== "string" || report.dialect.length === 0) {
    failures.push("dialect 为空");
  }
  if (typeof report.total !== "number" || report.total < minTotal) {
    failures.push(`total=${report.total} 低于下限 ${minTotal}`);
  }
  if (typeof report.ok !== "number" || typeof report.fail !== "number" || report.ok + report.fail !== report.total) {
    failures.push(`ok(${report.ok}) + fail(${report.fail}) ≠ total(${report.total})`);
  }

  if (failures.length > 0) {
    console.error(`corpus-report-check: ${name} 校验失败：`);
    for (const failure of failures) {
      console.error(`  - ${failure}`);
    }
    process.exit(1);
  }
  console.log(
    `corpus-report-check OK: ${name} dialect=${report.dialect} total=${report.total} ok=${report.ok} fail=${report.fail} dialect_failures=${report.dialect_failures} hint_hits=${report.hint_hits}`,
  );
}

await main();
