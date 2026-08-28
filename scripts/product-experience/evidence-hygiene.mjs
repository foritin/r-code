#!/usr/bin/env node
// M0-01.A3：对 Harness 报告与 TaskPacket/证据产物做 secret / raw reasoning oracle 扫描。
// 命中 > 0 即退出 1。与 runner.mjs 的 sanitizeText 共享同一组脱敏签名，
// 这里额外扫描「原始 prompt/response、思维链」类标记，确保证据只保留结果元数据。

import { readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import process from "node:process";

const ROOT = process.cwd();
const SCAN_EXT = new Set([".json", ".yaml", ".yml", ".md", ".log", ".txt"]);
const SKIP_DIRS = new Set([".git", "node_modules", "target", ".venv"]);
const MAX_BYTES = 2 * 1024 * 1024;

const SECRET_PATTERNS = [
  ["sk-token", /\bsk-[A-Za-z0-9_-]{12,}\b/],
  ["github-token", /\bgh[pousr]_[A-Za-z0-9]{20,}\b|\bgithub_pat_[A-Za-z0-9_]{20,}\b/],
  ["aws-key", /\bAKIA[0-9A-Z]{16}\b/],
  ["slack-token", /\bxox[baprs]-[A-Za-z0-9-]{10,}\b/],
  ["bearer-literal", /Bearer\s+[A-Za-z0-9._~+/-]{8,}/i],
  // 只认 key=value / key:"value" 赋值形态，避免把 prose 中的
  // `…::secret::redact_text`、`secret、raw reasoning` 等叙述性引用误报为泄漏。
  ["kv-secret", /(api[_-]?key|access[_-]?token|auth[_-]?token|client[_-]?secret|password)\s*[:=]\s*["']?\s*[A-Za-z0-9._~+/-]{10,}/i],
  ["private-key-block", /-----BEGIN [A-Z ]*PRIVATE KEY-----/],
];

const RAW_REASONING_PATTERNS = [
  ["raw-prompt-field", /"(raw_prompt|full_prompt|prompt_text)"\s*:/],
  ["raw-response-field", /"(raw_response|response_body|model_output_raw)"\s*:/],
  ["reasoning-trace", /"(raw_reasoning|thinking_trace|chain_of_thought)"\s*:/],
];

const patterns = [...SECRET_PATTERNS, ...RAW_REASONING_PATTERNS].map(([name, re]) => ({ name, re }));

function* walk(dir) {
  for (const entry of readdirSync(dir)) {
    const full = path.join(dir, entry);
    let st;
    try {
      st = statSync(full);
    } catch {
      continue;
    }
    if (st.isDirectory()) {
      if (SKIP_DIRS.has(entry)) continue;
      yield* walk(full);
    } else if (
      st.isFile() &&
      st.size <= MAX_BYTES &&
      SCAN_EXT.has(path.extname(entry).toLowerCase())
    ) {
      yield full;
    }
  }
}

function* iterRoot(root) {
  let st;
  try {
    st = statSync(root);
  } catch {
    return;
  }
  if (st.isFile()) {
    yield root;
  } else if (st.isDirectory()) {
    yield* walk(root);
  }
}

function defaultRoots() {
  // 合同边界：M0-01.A3 只审计「报告与 packet」——即本 worklist 的产物；
  // 其他已完成 worklist 的历史归档不属于本入口的裁决范围。
  return [
    path.join(ROOT, "artifacts", "ai-tasks", "current.yaml"),
    path.join(ROOT, "artifacts", "ai-tasks", "evidence", "product-experience-gap-closure"),
    path.join(ROOT, "artifacts", "ai-tasks", "verification", "product-experience-gap-closure"),
    path.join(ROOT, "artifacts", "ai-tasks", "templates"),
  ];
}

function scan() {
  const targets = process.argv.slice(2).map((p) => path.resolve(ROOT, p));
  const roots = targets.length > 0 ? targets : defaultRoots();
  const hits = [];
  const scanned = [];
  for (const root of roots) {
    if (!existsSafe(root)) {
      console.error(`path not found: ${root}`);
      process.exitCode = 2;
      return null;
    }
    for (const file of iterRoot(root)) {
      scanned.push(path.relative(ROOT, file));
      const lines = readFileSync(file, "utf8").split("\n");
      lines.forEach((line, i) => {
        for (const { name, re } of patterns) {
          if (re.test(line)) hits.push({ file: path.relative(ROOT, file), line: i + 1, pattern: name });
        }
      });
    }
  }
  return { scanned, hits };
}

function existsSafe(p) {
  try {
    statSync(p);
    return true;
  } catch {
    return false;
  }
}

const result = scan();
if (!result) process.exit();
console.log(
  JSON.stringify(
    {
      schema_version: "evidence-hygiene.v1",
      files_scanned: result.scanned.length,
      oracle_hits: result.hits.length,
      hits: result.hits.slice(0, 50),
    },
    null,
    2,
  ),
);
process.exitCode = result.hits.length === 0 ? 0 : 1;
