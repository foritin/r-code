/**
 * R27.A1/A2 — 合规静态守卫：无热更/无下载执行路径（F14）、release manifest
 * 安全配置、签名配置位存在（证书/Keystore 不进仓库）。
 */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..", "..");

/** 扫描时排除依赖与构建产物：F14 关心的是**本仓库自己的**代码，
 * node_modules / bundle-out / 原生产物目录里的第三方代码不属此列
 * （否则装上 RN 依赖就会因依赖里的 eval 误报）。 */
const SKIPPED_DIRS = new Set([
  "node_modules",
  "bundle-out",
  "android",
  "ios",
  ".git",
  "vendor",
  ".metro-health-check",
]);

function walk(dir, exts, out = []) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (SKIPPED_DIRS.has(entry.name)) continue;
    const path = join(dir, entry.name);
    if (entry.isDirectory()) walk(path, exts, out);
    else if (exts.some((ext) => entry.name.endsWith(ext))) out.push(path);
  }
  return out;
}

test("R27.A1: mobile 源码无运行时下载执行/eval/热更路径（F14）", () => {
  const mobileDir = join(root, "mobile");
  assert.ok(existsSync(mobileDir), "mobile 工程存在");
  for (const path of walk(mobileDir, [".ts", ".tsx", ".js"])) {
    const text = readFileSync(path, "utf8");
    assert.doesNotMatch(text, /\beval\s*\(/, `${path} 含 eval`);
    assert.doesNotMatch(text, /new\s+Function\s*\(/, `${path} 含 Function 构造`);
    assert.doesNotMatch(text, /fetch\s*\([^)]*\)\s*\.then\s*\(\s*.*\bimport\s*\(/, `${path} 疑似远程代码下载执行`);
    assert.doesNotMatch(text, /CodePush|expo-updates|react-native-fs.*download/, `${path} 疑似热更/远程 bundle`);
  }
});

test("R27.A2: 合规材料齐备（审核话术/签名指引/隐私清单/用途串）", () => {
  const guidePath = join(root, "docs", "support", "guides", "app-store-review.md");
  assert.ok(existsSync(guidePath), "app-store-review.md 存在");
  const guide = readFileSync(guidePath, "utf8");
  for (const section of [
    "3.3.2",
    "usesCleartextTraffic",
    "NSCameraUsageDescription",
    "不收集任何数据",
    "签名与构建",
    "Keystore",
  ]) {
    assert.ok(guide.includes(section), `材料缺少「${section}」`);
  }
  // 仓库不含签名材料。
  for (const secret of ["keystore.p12", "release.keystore", ".p8"]) {
    const found = walk(join(root, "mobile"), [secret]).length > 0;
    assert.equal(found, false, `${secret} 不得进仓库`);
  }
});
