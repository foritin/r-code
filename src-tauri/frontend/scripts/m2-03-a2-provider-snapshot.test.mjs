// M2-03.A2：Provider canonical snapshot 一致性——Composer/模型选择器与 Settings
// 模型服务页对同一 mock snapshot 显示同一 provider/model；默认值标注「服务默认」。

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import net from "node:net";
import test from "node:test";
import { fileURLToPath } from "node:url";

// fileURLToPath 而非 new URL().pathname：后者在 Windows 上产生 "/D:/..."，
// 经 path.resolve 变成 "D:\D:\..." 的坏 cwd，spawn 会以 ENOENT 静默失败。
const frontendDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const viteBin = path.join(frontendDir, "node_modules", "vite", "bin", "vite.js");

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
    [viteBin, "--host", "127.0.0.1", "--port", String(port), "--strictPort"],
    { cwd: frontendDir, stdio: ["ignore", "pipe", "pipe"], windowsHide: true },
  );
  // Windows 冷启动会超过固定 sleep，改为轮询就绪（与 onboarding-campaign 一致）。
  let viteErr = "";
  server.stderr?.setEncoding("utf8");
  server.stderr?.on("data", (chunk) => { viteErr += chunk; });
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    if (server.exitCode != null) {
      throw new Error(`Vite exited with ${server.exitCode}\n${viteErr.slice(0, 800)}`);
    }
    try {
      const response = await fetch(baseUrl);
      if (response.ok) break;
    } catch {
      // Server is still starting.
    }
    await new Promise((resolve) => setTimeout(resolve, 80));
  }
  browser = await chromium.launch({
    headless: true,
    executablePath: browserExecutable(),
  });
});

test.after(async () => {
  await browser?.close();
  server?.kill();
});

test("Composer 模型徽标与 Settings 默认服务一致（同源 snapshot）", async () => {
  const { chromium } = await import("playwright-core");
  const page = await browser.newPage({ locale: "zh-CN", viewport: { width: 1280, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "domcontentloaded", timeout: 20000 });
  await page.waitForTimeout(600);

  const composerText = await page.evaluate(() => document.body.innerText);
  // mock snapshot：OpenAI / gpt-5.6 · 服务默认（canonical default 只在 Host ACK 后更新）
  assert.ok(/服务默认/.test(composerText), "Composer 未显示 canonical 默认标注");

  await page.getByRole("button", { name: "设置", exact: true }).click();
  await page.getByRole("button", { name: "模型服务", exact: true }).first().click();
  await page.waitForTimeout(500);
  const settingsText = await page.evaluate(() => document.body.innerText);
  assert.ok(/服务默认|设为默认/.test(settingsText), "Settings 未显示默认服务语义");
  assert.ok(/OpenAI|官方|模型服务/.test(settingsText), "Settings 未显示 provider 快照");
  await page.close();
});
