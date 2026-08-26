// M2-01.A4：win_env 的 cfg 隔离结构检查。
// 断言：
// 1. crates/r-code-core/src/win_env.rs 整体位于 `#![cfg(windows)]`（或 lib.rs 的
//    `#[cfg(windows)] pub mod win_env`）门内；
// 2. 仓库内所有对 win_env 的**代码引用**（注释除外）要么挂在语句级
//    `#[cfg(windows)]` 属性下，要么其所属 item（沿花括号栈回溯到 fn/mod/impl
//    头部）的属性链含 cfg(windows)。
// 真实 unix/macOS 构建由 CI matrix 的 ubuntu/macos 腿把关（本地另有
// `cargo check --target x86_64-unknown-linux-gnu` 交叉验证）。

import { readFile, readdir } from "node:fs/promises";
import path from "node:path";
import process from "node:process";

const ROOT = path.resolve(import.meta.dirname, "..", "..");

async function collectRsFiles(dir, files = []) {
  for (const name of await readdir(dir, { withFileTypes: true })) {
    if (name.name === "target" || name.name === "node_modules" || name.name.startsWith(".")) {
      continue;
    }
    const full = path.join(dir, name.name);
    if (name.isDirectory()) {
      await collectRsFiles(full, files);
    } else if (name.name.endsWith(".rs")) {
      files.push(full);
    }
  }
  return files;
}

const WIN_ATTR = /#\[cfg\([^)]*windows[^)]*\)\]/;
const ITEM_HEADER = /\b(fn|mod|impl)\b/;

/// 花括号栈扫描：为每行计算其所属 item 链是否被 cfg(windows) 门控。
/// 返回 perLineGated: boolean[]（近似：忽略字符串/注释内的花括号——本仓库
/// 相关文件的控制流足够简单，且该检查另有交叉编译兜底）。
function analyzeGating(lines) {
  const perLineGated = new Array(lines.length).fill(false);
  // contexts[k] = 第 k 层花括号所属 item 的属性链是否含 windows 门。
  const contexts = [true]; // 顶层（模块级）视为未门控——置 true 只作占位，最终 gate 取 AND 语义见下。
  contexts[0] = false;
  let depth = 0;
  let pendingHeaderIndex = null;

  const attrsOfHeader = (index) => {
    // 从 item 头向上收集连续属性/注释/空行，判断是否含 windows 门。
    for (let k = index - 1; k >= 0; k -= 1) {
      const trimmed = lines[k].trim();
      if (trimmed === "" || trimmed.startsWith("//") || trimmed.startsWith("#[")) {
        if (WIN_ATTR.test(trimmed)) {
          return true;
        }
        continue;
      }
      return false;
    }
    return false;
  };

  let pendingAttrs = false;
  for (let i = 0; i < lines.length; i += 1) {
    const line = lines[i];
    const trimmed = line.trim();
    const codeOnly = line.replace(/\/\/.*$/, "");
    const isAttr = trimmed.startsWith("#[");
    const isFiller = trimmed === "" || trimmed.startsWith("//");
    if (pendingHeaderIndex === null && ITEM_HEADER.test(codeOnly) && !isFiller) {
      pendingHeaderIndex = i;
    } else if (isAttr) {
      pendingAttrs ||= WIN_ATTR.test(trimmed);
    }
    let openedBrace = false;
    for (const ch of codeOnly) {
      if (ch === "{") {
        const gated = pendingHeaderIndex !== null
          ? attrsOfHeader(pendingHeaderIndex)
          : (pendingAttrs || contexts[depth] || false);
        contexts[depth + 1] = gated;
        depth += 1;
        pendingHeaderIndex = null;
        openedBrace = true;
      } else if (ch === "}") {
        depth = Math.max(0, depth - 1);
        pendingHeaderIndex = null;
      }
    }
    if (!isAttr && !isFiller) {
      // 属性窗口被随后的语句或裸块消费后复位。
      pendingAttrs = false;
    }
    // 任一祖先门控即视为门控（嵌套：cfg(windows) mod 内的 fn 全部门控）。
    perLineGated[i] = contexts.slice(0, depth + 1).some(Boolean);
  }
  return perLineGated;
}

async function main() {
  const failures = [];

  const winEnvPath = path.join(ROOT, "crates", "r-code-core", "src", "win_env.rs");
  const winEnv = await readFile(winEnvPath, "utf8");
  const selfGated = /^#!\[cfg\(windows\)\]/m.test(winEnv);
  let libGated = false;
  const libPath = path.join(ROOT, "crates", "r-code-core", "src", "lib.rs");
  const libLines = (await readFile(libPath, "utf8")).split(/\r?\n/);
  libLines.forEach((line, index) => {
    if (line.includes("pub mod win_env") && index > 0 && WIN_ATTR.test(libLines[index - 1])) {
      libGated = true;
    }
  });
  if (!selfGated && !libGated) {
    failures.push("win_env.rs 必须整体处于 #![cfg(windows)] 或 lib.rs 的 #[cfg(windows)] pub mod 门内");
  }

  const files = [
    ...(await collectRsFiles(path.join(ROOT, "crates"))),
    ...(await collectRsFiles(path.join(ROOT, "src-tauri", "src"))),
  ];
  const CODE_REFERENCE = /r_code_core::win_env|(^|\s)use\s+.*win_env|win_env::/;
  for (const file of files) {
    if (file === winEnvPath || file === libPath) {
      continue;
    }
    const lines = (await readFile(file, "utf8")).split(/\r?\n/);
    const perLineGated = analyzeGating(lines);
    lines.forEach((line, index) => {
      const trimmed = line.trim();
      if (trimmed.startsWith("//") || trimmed.startsWith("//!")) {
        return; // 注释不构成编译引用
      }
      if (!CODE_REFERENCE.test(line)) {
        return;
      }
      // 语句级属性直查（`#[cfg(windows)] cmd.env(... win_env ...)`）。
      let statementGated = WIN_ATTR.test(line);
      if (!statementGated) {
        for (let k = index - 1; k >= 0; k -= 1) {
          const above = lines[k].trim();
          if (above === "" || above.startsWith("#[") || above.startsWith("//")) {
            if (WIN_ATTR.test(above)) {
              statementGated = true;
              break;
            }
            continue;
          }
          break;
        }
      }
      if (!statementGated && !perLineGated[index]) {
        failures.push(
          `${path.relative(ROOT, file)}:${index + 1}: win_env 代码引用缺少 cfg(windows) 门控 — ${trimmed}`,
        );
      }
    });
  }

  if (failures.length > 0) {
    console.error(`cfg-isolation-check 失败（${failures.length} 项）：`);
    for (const failure of failures) {
      console.error(`  - ${failure}`);
    }
    process.exit(1);
  }
  console.log("cfg-isolation-check OK: win_env 自身整体门控，全部代码引用均处于 cfg(windows) item 链内");
}

await main();
