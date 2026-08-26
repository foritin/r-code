// M0-01.A2：金集语料 schema 校验（PRD §4.4）。
// 用法：node scripts/windows-reliability/corpus-schema.mjs [--corpus <path>]
// 任一违规退出码 1 并逐条列出问题。

import { readFile } from "node:fs/promises";
import path from "node:path";
import process from "node:process";

export const PLATFORMS = ["windows", "macos", "both"];
export const TIERS = ["fast", "slow"];
export const CATEGORIES = [
  "dialect-chain",
  "env-prefix",
  "quoting",
  "encoding",
  "path",
  "pipe",
  "exit-code",
  "policy",
];
export const EXPECTS = ["ok", "fail", "fail-with-hint"];
// 八类数量下限（PRD §4.4）。
export const CATEGORY_MINIMUMS = {
  "dialect-chain": 6,
  "env-prefix": 4,
  quoting: 6,
  encoding: 4,
  path: 6,
  pipe: 4,
  "exit-code": 4,
  policy: 4,
};
const TOTAL_MINIMUM = 40;
const REQUIRED_FIELDS = ["id", "cmd", "platform", "tier", "category", "expect"];

export async function validateCorpusFile(corpusPath) {
  const issues = [];
  const text = await readFile(corpusPath, "utf8");
  const lines = text.split(/\r?\n/);
  const ids = new Set();
  const categoryCounts = Object.fromEntries(CATEGORIES.map((c) => [c, 0]));
  let entryCount = 0;

  lines.forEach((raw, index) => {
    const line = raw.trim();
    if (!line) {
      return;
    }
    const where = `${path.basename(corpusPath)}:${index + 1}`;
    let entry;
    try {
      entry = JSON.parse(line);
    } catch (error) {
      issues.push(`${where}: JSON 解析失败 — ${error.message}`);
      return;
    }
    entryCount += 1;
    for (const field of REQUIRED_FIELDS) {
      if (!(field in entry)) {
        issues.push(`${where}: 缺少必填字段 ${field}`);
      }
    }
    const extra = Object.keys(entry).filter((key) => !REQUIRED_FIELDS.includes(key));
    if (extra.length > 0) {
      issues.push(`${where}: 多余字段 ${extra.join(",")}（schema 冻结为六字段）`);
    }
    if (typeof entry.id !== "string" || entry.id.length === 0) {
      issues.push(`${where}: id 必须是非空字符串`);
    } else if (ids.has(entry.id)) {
      issues.push(`${where}: 重复 id ${entry.id}`);
    } else {
      ids.add(entry.id);
    }
    if (typeof entry.cmd !== "string" || entry.cmd.trim().length === 0) {
      issues.push(`${where}: cmd 必须是非空字符串`);
    }
    if (!PLATFORMS.includes(entry.platform)) {
      issues.push(`${where}: platform 必须是 ${PLATFORMS.join("|")}，实际 ${entry.platform}`);
    }
    if (!TIERS.includes(entry.tier)) {
      issues.push(`${where}: tier 必须是 ${TIERS.join("|")}，实际 ${entry.tier}`);
    }
    if (!CATEGORIES.includes(entry.category)) {
      issues.push(`${where}: category 必须是八类之一，实际 ${entry.category}`);
    } else {
      categoryCounts[entry.category] += 1;
    }
    if (!EXPECTS.includes(entry.expect)) {
      issues.push(`${where}: expect 必须是 ${EXPECTS.join("|")}，实际 ${entry.expect}`);
    }
  });

  if (entryCount < TOTAL_MINIMUM) {
    issues.push(`语料总数 ${entryCount} 低于下限 ${TOTAL_MINIMUM}`);
  }
  for (const [category, minimum] of Object.entries(CATEGORY_MINIMUMS)) {
    if (categoryCounts[category] < minimum) {
      issues.push(`类别 ${category} 数量 ${categoryCounts[category]} 低于下限 ${minimum}`);
    }
  }
  return { issues, entryCount, categoryCounts };
}

async function main() {
  const args = process.argv.slice(2);
  let corpus = "crates/r-code-gateway/tests/command_corpus/corpus.jsonl";
  const corpusFlag = args.indexOf("--corpus");
  if (corpusFlag >= 0) {
    corpus = args[corpusFlag + 1];
  }
  const rootDir = process.cwd();
  const { issues, entryCount, categoryCounts } = await validateCorpusFile(path.join(rootDir, corpus));
  if (issues.length > 0) {
    console.error(`corpus schema 校验失败（${issues.length} 项）：`);
    for (const issue of issues) {
      console.error(`  - ${issue}`);
    }
    process.exitCode = 1;
    return;
  }
  console.log(
    `corpus schema OK: ${entryCount} 条，各类数量 ${JSON.stringify(categoryCounts)}`,
  );
}

if (process.argv[1] && path.resolve(process.argv[1]) === path.resolve(import.meta.filename)) {
  await main();
}
