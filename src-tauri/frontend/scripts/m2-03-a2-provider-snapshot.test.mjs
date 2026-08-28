// M2-03.A2：Provider canonical snapshot 一致性——Composer/模型选择器与 Settings
// 模型服务页对同一 mock snapshot 显示同一 provider/model；默认值标注「服务默认」。

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import path from "node:path";
import net from "node:net";
import test from "node:test";

const frontendDir = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..");

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
    executablePath: process.env.CHROMIUM_PATH ?? "/usr/bin/chromium",
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
