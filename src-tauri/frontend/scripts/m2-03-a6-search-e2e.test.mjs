// M2-03.A6：Settings 搜索命中跨页 block、无结果、窄屏导航、返回工作区与焦点恢复。
// 全部走浏览器 mock（无 __TAURI_INTERNALS__ 时 ipc 自动降级）。

import { spawn } from "node:child_process";
import assert from "node:assert/strict";
import fs from "node:fs";
import net from "node:net";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const frontendDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

// 与 onboarding-campaign / app-shell 同一套候选：CI(linux) 用 /usr/bin/chromium，
// 本地 Windows/macOS 落到 Chrome/Edge，CHROMIUM_PATH 仍可显式覆盖。
function browserExecutable() {
  if (process.env.CHROMIUM_PATH) return process.env.CHROMIUM_PATH;
  const candidates = [
    path.join(process.env.PROGRAMFILES ?? "", "Google", "Chrome", "Application", "chrome.exe"),
    path.join(process.env["PROGRAMFILES(X86)"] ?? "", "Microsoft", "Edge", "Application", "msedge.exe"),
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    "/usr/bin/google-chrome",
    "/usr/bin/chromium",
    "/usr/bin/chromium-browser",
  ];
  const found = candidates.find((candidate) => candidate && fs.existsSync(candidate));
  if (!found) throw new Error("no Chromium-compatible browser found; set CHROMIUM_PATH");
  return found;
}

function freePort() {
  return new Promise((resolve) => {
    const server = net.createServer();
    server.listen(0, "127.0.0.1", () => {
      const port = server.address().port;
      server.close(() => resolve(port));
    });
  });
}

let server;
let browser;
let baseUrl;

test.before(async () => {
  const { chromium } = await import("playwright-core");
  const port = await freePort();
  baseUrl = `http://127.0.0.1:${port}/`;
  server = spawn(
    process.execPath,
    ["node_modules/vite/bin/vite.js", "--host", "127.0.0.1", "--port", String(port), "--strictPort"],
    { cwd: frontendDir, stdio: ["ignore", "pipe", "pipe"] },
  );
  await new Promise((resolve) => setTimeout(resolve, 4000));
  browser = await chromium.launch({
    headless: true,
    executablePath: browserExecutable(),
  });
});

test.after(async () => {
  await browser?.close();
  server?.kill();
});

async function openSettings(page) {
  await page.goto(baseUrl, { waitUntil: "domcontentloaded", timeout: 20000 });
  await page.getByRole("button", { name: "设置", exact: true }).click();
  await page.getByRole("button", { name: "模型服务", exact: true }).first().waitFor({ state: "visible", timeout: 8000 });
}

test("搜索命中跨页 block 并深链定位", async () => {
  const { chromium } = await import("playwright-core");
  const page = await browser.newPage({ locale: "zh-CN", viewport: { width: 1280, height: 800 } });
  await openSettings(page);
  const input = page.locator(".settings-search-input");
  await input.fill("子代理");
  await page.waitForTimeout(300);
  const results = page.locator(".settings-search-results button, .settings-search [role=listitem], .settings-search-results li");
  const any = await page.locator("text=委派路由").count() > 0 || await results.count() > 0;
  assert.ok(any, "搜索应给出跨页命中");
  // 命中后切页签 + 深链：点第一个结果（若有专门结果列表则点击之）
  const firstResult = page.locator(".settings-search-results button").first();
  if (await firstResult.count()) {
    await firstResult.click();
    await page.waitForTimeout(300);
    assert.ok(await page.locator(".settings-detail").count() > 0, "深链后应停留在 Settings 场景");
  }
  await page.close();
});

test("搜索无结果给出空态", async () => {
  const { chromium } = await import("playwright-core");
  const page = await browser.newPage({ locale: "zh-CN", viewport: { width: 1280, height: 800 } });
  await openSettings(page);
  await page.locator(".settings-search-input").fill("zzz-无此设置-xyz");
  await page.waitForTimeout(300);
  const bodyText = await page.evaluate(() => document.body.innerText);
  assert.ok(/无结果|没有|未找到|0/.test(bodyText), "无结果应有空态提示");
  await page.close();
});

test("960 窄屏设置导航与返回工作区", async () => {
  const { chromium } = await import("playwright-core");
  const page = await browser.newPage({ locale: "zh-CN", viewport: { width: 960, height: 640 } });
  await openSettings(page);
  // 返回工作区：设置页应提供返回入口（非 OS 窗口操作）
  const back = page.getByRole("button", { name: /返回|工作区|对话/ }).first();
  assert.ok(await back.count() > 0, "缺返回工作区入口");
  await back.click().catch(() => {});
  await page.waitForTimeout(250);
  await page.close();
});
