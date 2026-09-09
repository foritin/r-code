import { readdirSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptsDir = path.dirname(fileURLToPath(import.meta.url));
const frontendDir = path.resolve(scriptsDir, "..");

export const DEFAULT_BUDGETS = Object.freeze({
  totalBytes: 3_250_000,
  totalJavaScriptBytes: 1_700_000,
  totalCssBytes: 540_000,
  maxJavaScriptBytes: 475_000,
  maxCssBytes: 525_000,
  maxAssetBytes: 1_000_000,
});

function filesUnder(root) {
  return readdirSync(root, { withFileTypes: true }).flatMap((entry) => {
    const target = path.join(root, entry.name);
    return entry.isDirectory() ? filesUnder(target) : [target];
  });
}

function formatBytes(bytes) {
  return `${(bytes / 1024).toFixed(1)} KiB`;
}

export function checkBundleBudget(distDir, budgets = DEFAULT_BUDGETS) {
  const files = filesUnder(distDir).map((file) => ({
    file,
    relative: path.relative(distDir, file).replaceAll("\\", "/"),
    bytes: statSync(file).size,
    extension: path.extname(file).toLowerCase(),
  }));
  const javascript = files.filter((file) => file.extension === ".js");
  const css = files.filter((file) => file.extension === ".css");
  const assets = files.filter((file) => ![".js", ".css", ".html"].includes(file.extension));
  const sum = (items) => items.reduce((total, item) => total + item.bytes, 0);
  const largest = (items) => items.reduce((current, item) => (
    !current || item.bytes > current.bytes ? item : current
  ), null);
  const totals = {
    all: sum(files),
    javascript: sum(javascript),
    css: sum(css),
  };
  const largestFiles = {
    javascript: largest(javascript),
    css: largest(css),
    asset: largest(assets),
  };
  const violations = [];
  const enforceTotal = (label, actual, limit) => {
    if (actual > limit) violations.push(`${label}: ${formatBytes(actual)} > ${formatBytes(limit)}`);
  };
  const enforceFile = (label, file, limit) => {
    if (file && file.bytes > limit) {
      violations.push(`${label} ${file.relative}: ${formatBytes(file.bytes)} > ${formatBytes(limit)}`);
    }
  };

  enforceTotal("total bundle", totals.all, budgets.totalBytes);
  enforceTotal("total JavaScript", totals.javascript, budgets.totalJavaScriptBytes);
  enforceTotal("total CSS", totals.css, budgets.totalCssBytes);
  enforceFile("JavaScript chunk", largestFiles.javascript, budgets.maxJavaScriptBytes);
  enforceFile("CSS file", largestFiles.css, budgets.maxCssBytes);
  enforceFile("static asset", largestFiles.asset, budgets.maxAssetBytes);

  return { files, totals, largestFiles, violations };
}

export function main() {
  const distDir = path.join(frontendDir, "dist");
  const result = checkBundleBudget(distDir);
  console.log(
    `[bundle-budget] total ${formatBytes(result.totals.all)}, `
      + `JS ${formatBytes(result.totals.javascript)}, CSS ${formatBytes(result.totals.css)}`,
  );
  for (const [kind, file] of Object.entries(result.largestFiles)) {
    if (file) console.log(`[bundle-budget] largest ${kind}: ${file.relative} (${formatBytes(file.bytes)})`);
  }
  if (result.violations.length > 0) {
    for (const violation of result.violations) console.error(`[bundle-budget] ${violation}`);
    return 1;
  }
  return 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    process.exitCode = main();
  } catch (error) {
    console.error(`[bundle-budget] ${error instanceof Error ? error.message : String(error)}`);
    process.exitCode = 1;
  }
}
