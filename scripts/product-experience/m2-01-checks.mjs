#!/usr/bin/env node
// M2-01 验收断言执行器（唯一 token/material/CSS authority + 玻璃 fallback）：
//   a1: canonical token 只在 tokens.css 定义；后加载表零 canonical 重定义；
//       !important 只减不增（冻结预算）；CSS import manifest 冻结（禁新增最终覆盖文件）
//   a2: 亮暗主题关键配对 WCAG 对比度（解析 tokens.css 实际值计算）
//   a3: 玻璃可读 fallback（@supports not backdrop / prefers-reduced-transparency → 实心面板）
//   a4: 组件与样式层 rgba(0,0,0,…) 硬编码为 0（tokens 权威定义除外）；
//       studio-light 阴影保持 none/无黑投影；surface 语义别名齐备
// 实现面：src-tauri/frontend/src/styles/*、main.tsx import manifest、task-status-projection 无关。

import { readFileSync, readdirSync, statSync } from "node:fs";
import { parseArgs } from "node:util";
import path from "node:path";
import process from "node:process";

const ROOT = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..", "..");
const STYLES = path.join(ROOT, "src-tauri", "frontend", "src", "styles");
const TOKENS = path.join(STYLES, "tokens.css");
const MAIN = path.join(ROOT, "src-tauri", "frontend", "src", "main.tsx");

// 冻结于 2026-08-27（M2-01 收敛后实测基线，按出现次数计）：只许下降。
const IMPORTANT_BUDGET = {
  "base.css": 2,
  "components.css": 1,
  "workbench.css": 1,
  "onboarding.css": 4,
  "product-ui.css": 5,
  "r-code-ui.css": 8,
  "companion.css": 12,
};

// main.tsx 的冻结 CSS import manifest：不允许新增后加载覆盖文件。
const FROZEN_CSS_MANIFEST = [
  "tokens.css", "base.css", "components.css", "markdown.css", "shell.css",
  "scenes.css", "r-code-ui.css", "product-ui.css", "memory.css",
  "workbench.css", "signature.css", "onboarding.css", "companion.css",
];

// canonical token 名（权威定义只允许出现在 tokens.css）。
const CANONICAL = /^--(bg|fg|accent|border|shadow|scrim|fx|surface|overlay|material|font|sp|text|lh)-/;

function read(p) { return readFileSync(p, "utf8"); }

function cssFiles() { return readdirSync(STYLES).filter((f) => f.endsWith(".css")); }

function results_ok(name, conditions, note) {
  const failed = conditions.filter((c) => !c.ok);
  const result = {
    name,
    exit_code: failed.length === 0 ? 0 : 1,
    timed_out: false,
    duration_ms: 0,
    stdout_tail: failed.length === 0 ? (note ?? "all conditions hold") : "",
    stderr_tail: failed.map((c) => c.detail).join("\n"),
  };
  console.log(`${result.exit_code === 0 ? "PASS" : "FAIL"} ${name}`);
  if (failed.length > 0) console.error(result.stderr_tail);
  return result;
}

// ---- WCAG ----
function parseHex(hex) {
  const h = hex.replace("#", "");
  const v = h.length === 3 ? h.split("").map((c) => c + c).join("") : h;
  return [parseInt(v.slice(0, 2), 16), parseInt(v.slice(2, 4), 16), parseInt(v.slice(4, 6), 16)];
}
function luminance([r, g, b]) {
  const f = (c) => {
    const s = c / 255;
    return s <= 0.03928 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
  };
  return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
}
function contrast(fg, bg) {
  const l1 = luminance(parseHex(fg));
  const l2 = luminance(parseHex(bg));
  const [hi, lo] = l1 > l2 ? [l1, l2] : [l2, l1];
  return (hi + 0.05) / (lo + 0.05);
}

function themeBlock(theme) {
  const src = read(TOKENS);
  const start = src.indexOf(`:root[data-theme='${theme}'] {`);
  if (start < 0) return {};
  let depth = 0, i = start, body = "";
  for (; i < src.length; i += 1) {
    const ch = src[i];
    if (ch === "{") depth += 1;
    if (ch === "}") { depth -= 1; if (depth === 0) break; }
    if (depth > 0) body += ch;
  }
  const vars = {};
  for (const m of body.matchAll(/--([a-z0-9-]+):\s*([^;]+);/g)) vars[m[1]] = m[2].trim();
  return vars;
}

const parts = {
  a1() {
    const conditions = [];
    const canonicalInFiles = [];
    for (const file of cssFiles()) {
      if (file === "tokens.css") continue;
      const text = read(path.join(STYLES, file));
      // :root / :root[data-theme] 块内出现 canonical token 定义 = 越权重定义
      const rootBlocks = text.match(/:root[^{]*\{[^}]*\}/g) ?? [];
      for (const block of rootBlocks) {
        for (const m of block.matchAll(/--([a-z0-9-]+)\s*:/g)) {
          if (CANONICAL.test(`--${m[1]}`)) {
            canonicalInFiles.push(`${file}: --${m[1]}`);
          }
        }
      }
    }
    conditions.push({
      ok: canonicalInFiles.length === 0,
      detail: canonicalInFiles.length ? `canonical token 越权重定义: ${canonicalInFiles.join("; ")}` : "",
    });

    const budgetViolations = [];
    for (const file of cssFiles()) {
      const text = read(path.join(STYLES, file));
      const count = (text.match(/!important/g) ?? []).length;
      const budget = IMPORTANT_BUDGET[file];
      if (budget != null && count > budget) {
        budgetViolations.push(`${file}: ${count} > 冻结预算 ${budget}`);
      }
    }
    conditions.push({
      ok: budgetViolations.length === 0,
      detail: budgetViolations.length ? `!important 预算超支: ${budgetViolations.join("; ")}` : "",
    });

    const mainText = read(MAIN);
    const imported = [...mainText.matchAll(/styles\/([a-z0-9-]+\.css)/g)].map((m) => m[1]);
    const newFiles = imported.filter((f) => !FROZEN_CSS_MANIFEST.includes(f));
    conditions.push({
      ok: newFiles.length === 0,
      detail: newFiles.length ? `新增后加载 CSS（禁止）: ${newFiles.join(", ")}` : "",
    });
    return [results_ok("static:token-authority-and-override-freeze", conditions)];
  },

  a2() {
    const conditions = [];
    // 关键配对与阈值（§7 可读性：正文 ≥4.5，辅助/大字 ≥3.0）
    const PAIRS = [
      ["obsidian", "fg", "bg-app", 4.5],
      ["obsidian", "fg", "bg-card", 4.5],
      ["obsidian", "fg-muted", "bg-panel", 4.5],
      ["obsidian", "fg-faint", "bg-card", 3.0],
      ["obsidian", "accent-fg", "accent", 3.0],
      ["studio-light", "fg", "bg-app", 4.5],
      ["studio-light", "fg", "bg-card", 4.5],
      ["studio-light", "fg-muted", "bg-panel", 4.5],
      ["studio-light", "fg-faint", "bg-card", 3.0],
      ["studio-light", "accent-fg", "accent", 3.0],
    ];
    const themeVars = { obsidian: themeBlock("obsidian"), "studio-light": themeBlock("studio-light") };
    const failures = [];
    for (const [theme, fgKey, bgKey, min] of PAIRS) {
      const vars = themeVars[theme];
      const fg = vars[fgKey];
      const bg = vars[bgKey];
      if (!fg || !bg || !fg.startsWith("#") || !bg.startsWith("#")) {
        failures.push(`${theme} ${fgKey}/${bgKey}: 值缺失或非纯色(${fg ?? "?"} / ${bg ?? "?"})`);
        continue;
      }
      const ratio = contrast(fg, bg);
      if (ratio < min) failures.push(`${theme} ${fgKey} on ${bgKey}: ${ratio.toFixed(2)} < ${min}`);
    }
    conditions.push({ ok: failures.length === 0, detail: failures.join("; ") });
    return [results_ok("visual:theme-contrast-gate", conditions, "WCAG 配对全达标")];
  },

  a3() {
    const tokens = read(TOKENS);
    const conditions = [
      { ok: tokens.includes("@supports not (backdrop-filter: blur(1px))"), detail: "缺少 backdrop-filter 不可用 fallback" },
      { ok: tokens.includes("@media (prefers-reduced-transparency: reduce)"), detail: "缺少 reduced-transparency fallback" },
      { ok: tokens.includes("--material-panel-fallback: var(--bg-panel)"), detail: "缺少实心面板 fallback token" },
      { ok: /--fx-glass:\s*color-mix\(/.test(tokens), detail: "--fx-glass 仍为不透明直赋（假玻璃）" },
    ];
    return [results_ok("regression:glass-fallback-readability", conditions)];
  },

  a4() {
    const tokens = read(TOKENS);
    const conditions = [];
    const offenders = [];
    const scanDir = (dir) => {
      for (const entry of readdirSync(dir)) {
        const full = path.join(dir, entry);
        try {
          if (statSync(full).isDirectory()) { scanDir(full); continue; }
        } catch { continue; }
        if (!/\.(css|tsx?)$/.test(entry)) continue;
        if (full.endsWith("tokens.css")) continue;
        const text = read(full);
        if (/rgba\(\s*0\s*,\s*0\s*,\s*0/.test(text)) offenders.push(path.relative(ROOT, full));
      }
    };
    scanDir(STYLES);
    scanDir(path.join(ROOT, "src-tauri", "frontend", "src", "components"));
    conditions.push({
      ok: offenders.length === 0,
      detail: offenders.length ? `硬编码 rgba(0,0,0) 残留: ${offenders.join(", ")}` : "",
    });

    const light = themeBlock("studio-light");
    conditions.push({
      ok: light["shadow-card"] === "none" && light["shadow-popover"] === "none",
      detail: `studio-light 阴影必须保持平面(none)，实际: ${light["shadow-card"]}, ${light["shadow-popover"]}`,
    });
    for (const alias of ["surface-canvas", "surface-content", "surface-sunken", "surface-card", "surface-floating", "overlay-scrim", "material-panel-fallback"]) {
      conditions.push({
        ok: tokens.includes(`--${alias}:`),
        detail: `缺 surface 语义别名 --${alias}`,
      });
    }
    return [results_ok("visual-static:day-material-and-surface-aliases", conditions)];
  },
};

let args;
try {
  args = parseArgs({ options: { part: { type: "string" } }, strict: true });
} catch (error) {
  console.error(`用法: m2-01-checks.mjs --part a1|a2|a3|a4\n${error.message}`);
  process.exit(2);
}

const runner = parts[(args.values.part ?? "").toLowerCase()];
if (!runner) {
  console.error(`未知 part: ${args.values.part}`);
  process.exit(2);
}
const results = await runner();
const allOk = results.every((r) => r.exit_code === 0 && !r.timed_out);
console.log(JSON.stringify({ schema_version: "m2-01-checks.v1", part: (args.values.part ?? "").toLowerCase(), ok: allOk, results }, null, 2));
process.exit(allOk ? 0 : 1);
