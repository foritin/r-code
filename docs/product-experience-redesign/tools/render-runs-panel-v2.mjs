// 渲染 runs-panel-v2.html 的两个截图区域为 PNG。
// 用法: node docs/product-experience-redesign/tools/render-runs-panel-v2.mjs
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { createRequire } from "node:module";
import { fileURLToPath, pathToFileURL } from "node:url";

const require = createRequire("D:/project/rust/r-code/src-tauri/frontend/package.json");
const { chromium } = require("playwright-core");

const here = path.dirname(fileURLToPath(import.meta.url));
const htmlPath = path.resolve(here, "..", "runs-panel-v2.html");
const outDir = path.resolve(here, "..");

function browserExecutable() {
  const localAppData = process.env.LOCALAPPDATA ?? "";
  const playwrightCache = path.join(localAppData, "ms-playwright");
  const cached = fs.existsSync(playwrightCache)
    ? fs.readdirSync(playwrightCache)
      .filter((entry) => /^chromium-\d+$/.test(entry))
      .sort((left, right) => Number(right.split("-")[1]) - Number(left.split("-")[1]))
      .map((entry) => path.join(playwrightCache, entry, "chrome-win64", "chrome.exe"))
      .find((candidate) => fs.existsSync(candidate))
    : undefined;
  return [
    cached,
    path.join(process.env.PROGRAMFILES ?? "", "Google", "Chrome", "Application", "chrome.exe"),
    path.join(process.env.PROGRAMFILES ?? "", "Microsoft", "Edge", "Application", "msedge.exe"),
    path.join(process.env["PROGRAMFILES(X86)"] ?? "", "Microsoft", "Edge", "Application", "msedge.exe"),
  ].find((candidate) => candidate && fs.existsSync(candidate));
}

const executable = browserExecutable();
if (!executable) throw new Error("找不到可用的 Chromium/Chrome/Edge 可执行文件");

const browser = await chromium.launch({ executablePath: executable, headless: true });
try {
  const page = await browser.newPage({ viewport: { width: 1920, height: 1200 }, deviceScaleFactor: 2 });
  const errors = [];
  page.on("pageerror", (err) => errors.push(String(err)));
  page.on("console", (msg) => { if (msg.type() === "error") errors.push(msg.text()); });
  await page.goto(pathToFileURL(htmlPath).href, { waitUntil: "networkidle" });
  await page.waitForTimeout(400);

  const targets = [
    ["#shot-context", "design-runs-panel-v2-context.png"],
    ["#shot-states", "design-runs-panel-v2-states.png"],
  ];
  for (const [selector, name] of targets) {
    const el = page.locator(selector);
    await el.scrollIntoViewIfNeeded();
    await page.waitForTimeout(150);
    await el.screenshot({ path: path.join(outDir, name) });
    console.log(`已输出 ${name}`);
  }
  if (errors.length > 0) {
    console.error("页面存在控制台/脚本错误:");
    for (const err of errors) console.error("  -", err);
    process.exitCode = 1;
  }
} finally {
  await browser.close();
}
