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
  const playwrightCache = path.join(process.env.LOCALAPPDATA ?? "", "ms-playwright");
  const cached = fs.existsSync(playwrightCache)
    ? fs.readdirSync(playwrightCache)
      .filter((entry) => /^chromium-\d+$/.test(entry))
      .sort((left, right) => Number(right.split("-")[1]) - Number(left.split("-")[1]))
      .flatMap((entry) => [
        path.join(playwrightCache, entry, "chrome-win64", "chrome.exe"),
        path.join(playwrightCache, entry, "chrome-linux", "chrome"),
        path.join(playwrightCache, entry, "chrome-mac", "Chromium.app", "Contents", "MacOS", "Chromium"),
      ])
      .find((candidate) => fs.existsSync(candidate))
    : undefined;
  return [
    cached,
    path.join(process.env.PROGRAMFILES ?? "", "Google", "Chrome", "Application", "chrome.exe"),
    path.join(process.env.PROGRAMFILES ?? "", "Microsoft", "Edge", "Application", "msedge.exe"),
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    "/usr/bin/google-chrome",
    "/usr/bin/chromium",
    "/usr/bin/chromium-browser",
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
      // Server is still starting.
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

test("queue stays above the composer and drag order becomes the dispatch order", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().openRoom("mock-task-queue");
  });

  const composerBox = page.locator(".comp-box");
  await composerBox.waitFor({ state: "visible" });
  const emptyHeight = (await composerBox.boundingBox())?.height;
  assert.ok(emptyHeight, "composer should have a stable measurable height");

  await page.evaluate(async () => {
    const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    await browserMockInvoke("cmd_agent_send", {
      taskId: "mock-task-queue",
      message: "先执行：补充队列并发测试",
      mode: "queue",
    });
    await browserMockInvoke("cmd_agent_send", {
      taskId: "mock-task-queue",
      message: "后执行：整理队列交互",
      mode: "queue",
    });
    await useTasksStore.getState().refreshDetail("mock-task-queue");
  });

  const queue = page.getByRole("region", { name: "待发送队列，越靠上越先执行" });
  await queue.waitFor({ state: "visible" });
  assert.equal(await composerBox.locator(".composer-queue-stack").count(), 0, "queue must not be nested inside the input box");
  const queueBox = await queue.boundingBox();
  const filledComposerBox = await composerBox.boundingBox();
  assert.ok(queueBox && filledComposerBox && queueBox.y + queueBox.height <= filledComposerBox.y, "queue should sit above the input box");
  assert.ok(Math.abs(filledComposerBox.height - emptyHeight) <= 1, "showing queue rows must not resize the input box");

  const rows = queue.locator(".composer-queue-row");
  assert.equal(await rows.count(), 2);
  assert.match(await rows.nth(0).innerText(), /先执行/);
  assert.match(await rows.nth(1).innerText(), /后执行/);
  assert.equal(
    await page.locator(".timeline .user-message-queued").count(),
    0,
    "queued messages belong to the composer queue and must not duplicate into conversation history",
  );

  const firstMessage = rows.nth(0).locator(".queue-message");
  const firstHandle = rows.nth(0).locator(".queue-reorder-handle");
  const messageBeforeHover = await firstMessage.boundingBox();
  assert.equal(await firstHandle.evaluate((element) => getComputedStyle(element).opacity), "0");
  await rows.nth(0).hover();
  await page.waitForFunction(() => getComputedStyle(document.querySelector(".queue-reorder-handle")).opacity === "1");
  const messageAfterHover = await firstMessage.boundingBox();
  assert.equal(messageAfterHover?.x, messageBeforeHover?.x, "hover handle must use reserved space instead of shifting text");

  await firstHandle.dragTo(rows.nth(1), { targetPosition: { x: 120, y: 42 } });
  await page.waitForFunction(() => document.querySelector(".composer-queue-row .queue-message")?.textContent?.includes("后执行"));
  assert.match(await rows.nth(0).innerText(), /后执行/);
  assert.match(await rows.nth(1).innerText(), /先执行/);
  assert.deepEqual(
    await page.evaluate(async () => {
      const { browserMockDetails } = await import("/src/lib/mock-data.ts");
      return browserMockDetails["mock-task-queue"].queued_messages.map((item) => item.message);
    }),
    ["后执行：整理队列交互", "先执行：补充队列并发测试"],
  );

  const secondHandle = rows.nth(1).locator(".queue-reorder-handle");
  await secondHandle.focus();
  await secondHandle.press("ArrowUp");
  await page.waitForFunction(() => document.querySelector(".composer-queue-row .queue-message")?.textContent?.includes("先执行"));
  assert.match(await rows.nth(0).innerText(), /先执行/, "keyboard users should be able to restore the same order");

  await page.evaluate(async () => {
    const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    await browserMockInvoke("cmd_agent_send", {
      taskId: "mock-task-queue",
      message: "第三条：优先作为当前运行的引导",
      mode: "queue",
    });
    await useTasksStore.getState().refreshDetail("mock-task-queue");
  });
  await page.waitForFunction(() => document.querySelectorAll(".composer-queue-row").length === 3);
  const thirdHandle = rows.nth(2).locator(".queue-reorder-handle");
  // 新行位于队列顶部，恰好落在 headless 鼠标默认位置 (0,0) 时会被 :hover
  // 命中（handle 变为可见）。先把鼠标移开，再等待 100ms 过渡收敛，然后断言
  // handle 默认隐藏——避免"严格等于 0"在过渡窗口/悬停下的时序脆弱断言。
  await page.mouse.move(640, 480);
  await page.waitForFunction(
    () => {
      const handles = document.querySelectorAll(".queue-reorder-handle");
      return (
        handles.length === 3 && Number(getComputedStyle(handles[2]).opacity) < 0.05
      );
    },
    { timeout: 4_000 },
  );
  assert.ok(
    Number(await thirdHandle.evaluate((element) => getComputedStyle(element).opacity)) < 0.05,
  );
  await rows.nth(2).hover();
  await page.waitForFunction(() => getComputedStyle(document.querySelectorAll(".queue-reorder-handle")[2]).opacity === "1");
  if (process.env.R_CODE_QUEUE_SHOT) {
    await page.screenshot({ path: process.env.R_CODE_QUEUE_SHOT, fullPage: true });
  }

  await rows.nth(1).getByRole("button", { name: /更多队列操作/ }).click();
  if (process.env.R_CODE_QUEUE_MENU_SHOT) {
    await page.screenshot({ path: process.env.R_CODE_QUEUE_MENU_SHOT, fullPage: true });
  }
  await page.getByRole("menuitem", { name: "编辑消息" }).click();
  const editor = rows.nth(1).locator(".queue-edit-input");
  await editor.fill("后执行：已编辑的队列交互");
  await rows.nth(1).getByRole("button", { name: "保存", exact: true }).click();
  await page.waitForFunction(() => document.querySelectorAll(".queue-message")[1]?.textContent?.includes("已编辑"));
  assert.match(await rows.nth(1).innerText(), /已编辑的队列交互/);

  await rows.nth(0).getByRole("button", { name: /更多队列操作/ }).click();
  await page.getByRole("menuitem", { name: "关闭排队" }).click();
  await page.waitForFunction(() => document.querySelector(".run-send-mode-label")?.textContent?.includes("引导"));
  assert.equal(await rows.count(), 3, "turning off queueing must preserve already queued messages");

  await rows.nth(2).getByRole("button", { name: /引导当前运行/ }).click();
  await page.waitForFunction(() => document.querySelectorAll(".composer-queue-row").length === 2);
  assert.deepEqual(
    await page.evaluate(async () => {
      const { browserMockDetails } = await import("/src/lib/mock-data.ts");
      return browserMockDetails["mock-task-queue"].queued_messages.map((item) => item.message);
    }),
    ["先执行：补充队列并发测试", "后执行：已编辑的队列交互"],
    "steering one selected message must preserve the relative order of all remaining messages",
  );

  await rows.nth(0).getByRole("button", { name: /删除队列消息/ }).click();
  await page.waitForFunction(() => document.querySelectorAll(".composer-queue-row").length === 1);
  assert.match(await rows.nth(0).innerText(), /已编辑的队列交互/);
  await page.close();
});
