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

test("activity shows task-level outcomes while archive owns restore and feed visibility", async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.getByRole("button", { name: "活动", exact: true }).click();

  const activity = page.locator(".activity-page");
  await activity.getByRole("heading", { name: "需要处理", exact: true }).waitFor({ state: "visible" });
  await activity.getByRole("heading", { name: "正在进行", exact: true }).waitFor({ state: "visible" });
  await activity.getByRole("heading", { name: "最近结束", exact: true }).waitFor({ state: "visible" });
  assert.equal(await activity.getByText(/调用了工具|收到了工具结果/).count(), 0, "raw tool events must not be the activity product");
  assert.equal(await activity.locator(".activity-recent-row").filter({ hasText: "更新依赖并修复告警" }).count(), 1);

  if (process.env.R_CODE_ACTIVITY_SHOT) {
    await page.screenshot({ path: process.env.R_CODE_ACTIVITY_SHOT, fullPage: true });
  }

  await page.evaluate(async () => {
    const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    await browserMockInvoke("cmd_task_archive", { taskId: "mock-task-complete" });
    await useTasksStore.getState().refreshTasks();
  });
  await page.waitForFunction(() => !document.querySelector(".activity-page")?.textContent?.includes("更新依赖并修复告警"));
  const archivedFeed = await page.evaluate(async () => {
    const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
    return browserMockInvoke("cmd_activity_list", { cursor: null, limit: 50 });
  });
  assert.equal(archivedFeed.items.some((item) => item.task_id === "mock-task-complete"), false, "archived task events must leave the global feed");

  await page.getByRole("button", { name: "归档", exact: true }).click();
  const archive = page.locator(".archive-page");
  const archivedRow = archive.locator(".archive-row").filter({ hasText: "更新依赖并修复告警" });
  await archivedRow.waitFor({ state: "visible" });
  await archivedRow.getByRole("button", { name: "还原", exact: true }).waitFor({ state: "visible" });

  if (process.env.R_CODE_ARCHIVE_SHOT) {
    await page.screenshot({ path: process.env.R_CODE_ARCHIVE_SHOT, fullPage: true });
  }

  await archivedRow.getByRole("button", { name: "还原", exact: true }).click();
  await page.getByText("对话已还原", { exact: true }).waitFor({ state: "visible" });
  await archivedRow.waitFor({ state: "detached" });

  await page.getByRole("button", { name: "活动", exact: true }).click();
  await activity.locator(".activity-recent-row").filter({ hasText: "更新依赖并修复告警" }).waitFor({ state: "visible" });
  await page.close();
});
