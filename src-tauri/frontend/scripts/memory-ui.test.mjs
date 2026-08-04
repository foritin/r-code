import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import path from "node:path";
import process from "node:process";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { chromium } from "playwright-core";

const frontendDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const viteBin = path.join(frontendDir, "node_modules", "vite", "bin", "vite.js");

function browserExecutable() {
  const cache = path.join(process.env.LOCALAPPDATA ?? "", "ms-playwright");
  const cached = fs.existsSync(cache)
    ? fs.readdirSync(cache)
      .filter((entry) => /^chromium-\d+$/.test(entry))
      .sort((left, right) => Number(right.split("-")[1]) - Number(left.split("-")[1]))
      .map((entry) => path.join(cache, entry, "chrome-win64", "chrome.exe"))
      .find((candidate) => fs.existsSync(candidate))
    : undefined;
  return [
    cached,
    path.join(process.env.PROGRAMFILES ?? "", "Google", "Chrome", "Application", "chrome.exe"),
    path.join(process.env.PROGRAMFILES ?? "", "Microsoft", "Edge", "Application", "msedge.exe"),
    "/usr/bin/google-chrome",
    "/usr/bin/chromium",
  ].find((candidate) => candidate && fs.existsSync(candidate));
}

async function freePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      const port = typeof address === "object" && address ? address.port : 0;
      server.close((error) => error ? reject(error) : resolve(port));
    });
  });
}

async function waitForServer(url, processHandle) {
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    if (processHandle.exitCode != null) throw new Error(`Vite exited with ${processHandle.exitCode}`);
    try {
      const response = await fetch(url);
      if (response.ok) return;
    } catch {
      // Vite is still starting.
    }
    await new Promise((resolve) => setTimeout(resolve, 80));
  }
  throw new Error("Timed out waiting for the frontend test server");
}

let server;
let browser;
let baseUrl;

test.before(async () => {
  const port = await freePort();
  baseUrl = `http://127.0.0.1:${port}/`;
  server = spawn(process.execPath, [viteBin, "--host", "127.0.0.1", "--port", String(port), "--strictPort"], {
    cwd: frontendDir,
    stdio: ["ignore", "pipe", "pipe"],
    windowsHide: true,
  });
  await waitForServer(baseUrl, server);
  browser = await chromium.launch({ executablePath: browserExecutable(), headless: true });
});

test.after(async () => {
  await browser?.close();
  server?.kill();
});

test("memory makes its privacy default, reviewer choice, and enable action self-evident", async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const { memoryClearAll } = await import("/src/lib/ipc.ts");
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    await memoryClearAll();
    useTasksStore.getState().setCurrentProject(null);
    useAppStore.getState().openKnowledge("memory");
  });

  const panel = page.locator(".knowledge-memory-panel");
  await panel.waitFor({ state: "visible" });
  await panel.getByText("记忆已关闭", { exact: true }).waitFor({ state: "visible" });
  await panel.getByText(/默认关闭/).waitFor({ state: "visible" });

  const provider = panel.getByLabel("模型服务", { exact: true });
  const model = panel.getByLabel("复盘模型", { exact: true });
  assert.equal(await provider.inputValue(), "openai");
  assert.equal(await model.inputValue(), "gpt-5.6-sol", "default provider model must not be cleared after settings load");
  assert.equal(await page.locator(".knowledge-tabs button span").count(), 0, "tabs should not repeat explanatory copy");

  if (process.env.R_CODE_MEMORY_SHOT_BEFORE) {
    await page.screenshot({ path: process.env.R_CODE_MEMORY_SHOT_BEFORE, fullPage: true });
  }

  await panel.getByRole("button", { name: "启用记忆", exact: true }).click();
  await panel.getByText("记忆已开启", { exact: true }).waitFor({ state: "visible" });
  const settings = await page.evaluate(async () => (await import("/src/lib/ipc.ts")).memoryOverview());
  assert.equal(settings.settings.enabled, true);
  assert.deepEqual(settings.settings.reviewer, { provider_name: "openai", model: "gpt-5.6-sol" });

  if (process.env.R_CODE_MEMORY_SHOT) {
    await page.screenshot({ path: process.env.R_CODE_MEMORY_SHOT, fullPage: true });
  }
  await page.close();
});
