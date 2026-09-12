/**
 * R07b.A2 — 移动布局静态断言（mjs 布局检查；任务卡允许"playwright 视口断言
 * 或 mjs 像素/布局检查"）：viewport meta、无横向滚动声明、命中区下限、
 * 安全区 inset、深色皮肤 tokens 与 prototype.html 基线一致。
 */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync, existsSync, readdirSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const frontendRoot = join(here, "..", "..", "..");
const bundle = join(frontendRoot, "dist");

test("R07b.A2: remote.html 声明移动 viewport（含 viewport-fit=cover）", () => {
  const html = readFileSync(join(frontendRoot, "remote.html"), "utf8");
  assert.match(html, /width=device-width/);
  assert.match(html, /viewport-fit=cover/, "安全区需要 cover");
  assert.match(html, /theme-color" content="#181818"/, "深色皮肤底色");
});

test("R07b.A2: PWA bundle 携带布局约束（overflow-x/命中区/安全区/tokens）", () => {
  assert.ok(existsSync(bundle), "需要先 npm run build");
  const assets = join(bundle, "assets");
  const jsFiles = readdirSync(assets).filter((name) => name.startsWith("remote-") && name.endsWith(".js"));
  assert.ok(jsFiles.length > 0, "remote entry bundle 存在");
  const code = jsFiles.map((name) => readFileSync(join(assets, name), "utf8")).join("\n");

  assert.match(code, /overflowX:\s*"hidden"/, "无横向滚动");
  assert.match(code, /minHeight:\s*44/, "命中区 ≥44px");
  assert.match(code, /env\(safe-area-inset-bottom\)/, "底部安全区");
  assert.match(code, /"18,?18,?18"|#181818/, "obsidian 底色 #181818");
  assert.match(code, /#f4742b/, "obsidian accent #f4742b");
  // 三 tab + 断网态 + 只读徽标都进了 bundle。
  for (const text of ["任务", "审批", "设置", "只读设备", "后重试"]) {
    assert.ok(code.includes(text), `bundle 包含「${text}」`);
  }
});

test("R07b.A2: SW 缓存名单只含壳（数据网络必需，F10）", () => {
  const sw = readFileSync(join(bundle, "remote", "sw.js"), "utf8");
  // 预缓存白名单只有壳页面——数据接口/帧不进任何缓存（F10）。
  assert.match(sw, /cache\.addAll\(\["\/app\/"\]\)/, "预缓存仅壳页面");
  assert.doesNotMatch(sw, /cache\.(put|match)\(.*(?:task|approvals|events)/, "数据路径不进缓存逻辑");
});

