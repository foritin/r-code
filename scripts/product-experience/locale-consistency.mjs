#!/usr/bin/env node
// M1-01.A2：zh-CN / en-US locale key 集与 interpolation placeholder 集完全一致。
// - 逐 key 对比，缺失/多出一侧立即失败并列出精确 key；
// - 每个 string leaf 的 {{name}} 占位符集合必须双侧相同；
// - errors.unknown 兜底键必须双侧存在（UserFacingIpcError 降级依赖）。
// 输出 schema 合法 JSON 报告；命中差集 exit 1。

import assert from "node:assert/strict";
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import path from "node:path";
import process from "node:process";

const frontendDir = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..", "src-tauri", "frontend");
const OUT = path.join(
  frontendDir,
  "..",
  "..",
  "artifacts",
  "ai-tasks",
  "verification",
  "product-experience-gap-closure",
  "implementation",
  "locale-consistency.json",
);

function flatten(node, prefix = "", out = new Map()) {
  if (node == null || typeof node !== "object") {
    out.set(prefix, node);
    return out;
  }
  for (const [key, value] of Object.entries(node)) {
    flatten(value, prefix ? `${prefix}.${key}` : key, out);
  }
  return out;
}

function placeholders(text) {
  const found = [];
  for (const match of String(text).matchAll(/\{\{\s*([\w.$-]+)\s*\}\}/g)) {
    found.push(match[1]);
  }
  return found.sort();
}

const zh = JSON.parse(readFileSync(path.join(frontendDir, "src/i18n/locales/zh-CN.json"), "utf8"));
const en = JSON.parse(readFileSync(path.join(frontendDir, "src/i18n/locales/en-US.json"), "utf8"));
const zhFlat = flatten(zh);
const enFlat = flatten(en);

const missing_in_en = [...zhFlat.keys()].filter((k) => !enFlat.has(k));
const missing_in_zh = [...enFlat.keys()].filter((k) => !zhFlat.has(k));
const type_mismatches = [...zhFlat.keys()]
  .filter((k) => enFlat.has(k) && (typeof zhFlat.get(k) === "string") !== (typeof enFlat.get(k) === "string"))
  .map((k) => ({ key: k, zh_type: typeof zhFlat.get(k), en_type: typeof enFlat.get(k) }));

const placeholder_issues = [];
for (const [key, zhText] of zhFlat) {
  if (!enFlat.has(key) || typeof zhText !== "string" || typeof enFlat.get(key) !== "string") continue;
  const zhPh = placeholders(zhText);
  const enPh = placeholders(enFlat.get(key));
  if (JSON.stringify(zhPh) !== JSON.stringify(enPh)) {
    placeholder_issues.push({ key, zh: zhPh, en: enPh });
  }
}

const report = {
  schema_version: "locale-consistency.v1",
  locales: ["zh-CN", "en-US"],
  zh_keys: zhFlat.size,
  en_keys: enFlat.size,
  missing_in_en,
  missing_in_zh,
  type_mismatches,
  placeholder_issues,
  fallback_key_errors_unknown_present: Boolean(zhFlat.get("errors.unknown") && enFlat.get("errors.unknown")),
  ok:
    missing_in_en.length === 0 &&
    missing_in_zh.length === 0 &&
    type_mismatches.length === 0 &&
    placeholder_issues.length === 0 &&
    Boolean(zhFlat.get("errors.unknown") && enFlat.get("errors.unknown")),
};

mkdirSync(path.dirname(OUT), { recursive: true });
writeFileSync(OUT, JSON.stringify(report, null, 2) + "\n", "utf8");
console.log(JSON.stringify(report, null, 2));

if (!report.ok) {
  console.error("locale inconsistency detected; see report above");
  process.exitCode = 1;
}
