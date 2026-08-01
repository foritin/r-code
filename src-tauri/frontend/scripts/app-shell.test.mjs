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
  const localAppData = process.env.LOCALAPPDATA ?? "";
  const playwrightCache = path.join(localAppData, "ms-playwright");
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

for (const viewport of [{ width: 800, height: 600 }, { width: 1200, height: 800 }, { width: 1800, height: 1200 }]) {
  test(`room fills and scrolls within ${viewport.width}x${viewport.height}`, async () => {
    const page = await browser.newPage({ viewport });
    const runtimeErrors = [];
    page.on("pageerror", (error) => runtimeErrors.push(String(error)));
    page.on("console", (message) => {
      if (message.type() === "error") runtimeErrors.push(message.text());
    });

    await page.goto(baseUrl, { waitUntil: "networkidle" });
    if (viewport.width < 1120) {
      await page.locator(".sidebar-nav-item").filter({ hasText: "对话" }).click();
      await page.locator(".conversation-main").first().click();
    } else {
      await page.locator(".sidebar-task:visible").first().click();
    }
    await page.locator("#main-content > .scene-room").waitFor({ state: "visible" });

    const layout = await page.evaluate(() => {
      const main = document.querySelector("#main-content");
      const room = document.querySelector("#main-content > .scene-room");
      const timeline = document.querySelector("#main-content > .scene-room .timeline");
      assertElement(main, "main");
      assertElement(room, "room");
      assertElement(timeline, "timeline");

      for (let index = 0; index < 80; index += 1) {
        const row = document.createElement("p");
        row.textContent = `scroll-regression-${index}`;
        timeline.append(row);
      }
      timeline.scrollTop = timeline.scrollHeight;

      const mainRect = main.getBoundingClientRect();
      const roomRect = room.getBoundingClientRect();
      return {
        mainRect: [mainRect.x, mainRect.y, mainRect.width, mainRect.height],
        roomRect: [roomRect.x, roomRect.y, roomRect.width, roomRect.height],
        timeline: [timeline.clientHeight, timeline.scrollHeight, timeline.scrollTop],
        page: [document.documentElement.scrollWidth, document.documentElement.scrollHeight, innerWidth, innerHeight],
      };

      function assertElement(value, label) {
        if (!(value instanceof HTMLElement)) throw new Error(`${label} missing`);
      }
    });

    assert.deepEqual(layout.roomRect, layout.mainRect, "room must occupy the complete main viewport");
    assert.ok(layout.timeline[1] > layout.timeline[0], "long conversations must overflow the timeline");
    assert.ok(layout.timeline[2] > 0, "the timeline must accept vertical scrolling");
    assert.ok(layout.page[0] <= layout.page[2] + 1, "the app must not create page-level horizontal scrolling");
    assert.ok(layout.page[1] <= layout.page[3] + 1, "the app must not create page-level vertical scrolling");
    assert.deepEqual(runtimeErrors, []);
    await page.close();
  });
}

test("project conversations expose archive and confirmed permanent delete", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const taskRow = page.locator(".sidebar-task-row").filter({ hasText: "更新依赖并修复告警" });
  await taskRow.locator(".sidebar-task").click();
  await page.locator("#main-content > .scene-room").waitFor({ state: "visible" });
  await taskRow.hover();
  await taskRow.locator(".task-actions-trigger").click();

  const menu = page.locator('.task-actions-popover[role="menu"]');
  await menu.waitFor({ state: "visible" });
  assert.match(await menu.innerText(), /归档对话/);
  assert.match(await menu.innerText(), /永久删除/);
  await menu.getByRole("menuitem", { name: /永久删除/ }).click();

  const dialog = page.getByRole("alertdialog", { name: "永久删除这段对话？" });
  await dialog.waitFor({ state: "visible" });
  assert.match(await dialog.innerText(), /项目目录和其中的文件不会被删除/);
  await dialog.getByRole("button", { name: "永久删除", exact: true }).click();
  await page.getByText("对话已永久删除", { exact: true }).waitFor({ state: "visible" });
  await page.locator("#main-content > .scene-conversations").waitFor({ state: "visible" });
  assert.equal(await page.locator(".sidebar-task-row").filter({ hasText: "更新依赖并修复告警" }).count(), 0);
  await page.close();
});

test("archived conversations remain available as read-only history", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.locator(".sidebar-nav-item").filter({ hasText: "对话" }).click();

  const conversation = page.locator(".conversation-row").filter({ hasText: "更新依赖并修复告警" });
  await conversation.locator(".task-actions-trigger").click();
  await page.getByRole("menuitem", { name: /归档对话/ }).click();
  await page.getByText("对话已归档", { exact: true }).waitFor({ state: "visible" });

  await page.getByRole("tab", { name: "已归档" }).click();
  const archived = page.locator(".conversation-row").filter({ hasText: "更新依赖并修复告警" });
  await archived.waitFor({ state: "visible" });
  await archived.locator(".conversation-main").click();
  await page.getByText("此对话已归档，只能查看历史。可通过右上角对话选项永久删除。").waitFor({ state: "visible" });
  assert.equal(await page.locator(".composer").count(), 0);
  await page.close();
});

test("workspace mock keeps opaque identity stable until forget", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const result = await page.evaluate(async () => {
    const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
    const { browserMockWorkspaces } = await import("/src/lib/mock-data.ts");
    const path = "D:/identity-regression/shared-name";
    const siblingPath = "D:/other-root/shared-name";
    const legacyPath = "D:/legacy/workspace";
    const trackedPaths = new Set([path, siblingPath, legacyPath]);

    try {
      const first = await browserMockInvoke("cmd_workspace_open", { path });
      const second = await browserMockInvoke("cmd_workspace_open", { path });
      const sibling = await browserMockInvoke("cmd_workspace_open", { path: siblingPath });
      const listed = await browserMockInvoke("cmd_workspace_list");
      const firstListed = listed.find((workspace) => workspace.canonical_path === path);
      const routed = await browserMockInvoke("cmd_workspace_set_access_mode", {
        workspacePath: path,
        accessMode: "full_access",
      });

      const legacy = {
        canonical_path: legacyPath,
        display_name: "workspace",
        access_mode: "request_approval",
        last_opened_at: "2026-01-01T00:00:00.000Z",
      };
      browserMockWorkspaces.unshift(legacy);
      const legacyListed = (await browserMockInvoke("cmd_workspace_list"))
        .find((workspace) => workspace.canonical_path === legacyPath);
      const legacyOpened = await browserMockInvoke("cmd_workspace_open", { path: legacyPath });
      const legacyListedAgain = (await browserMockInvoke("cmd_workspace_list"))
        .find((workspace) => workspace.canonical_path === legacyPath);

      legacy.memory_mode = "future_mode";
      let invalidListError = "";
      let invalidOpenError = "";
      try {
        await browserMockInvoke("cmd_workspace_list");
      } catch (error) {
        invalidListError = String(error);
      }
      try {
        await browserMockInvoke("cmd_workspace_open", { path: legacyPath });
      } catch (error) {
        invalidOpenError = String(error);
      }
      legacy.memory_mode = "inherit";

      await browserMockInvoke("cmd_workspace_forget", { workspacePath: path });
      const reopened = await browserMockInvoke("cmd_workspace_open", { path });

      return {
        first,
        second,
        sibling,
        firstListed,
        routed,
        legacyListed,
        legacyOpened,
        legacyListedAgain,
        invalidListError,
        invalidOpenError,
        reopened,
      };
    } finally {
      for (let index = browserMockWorkspaces.length - 1; index >= 0; index -= 1) {
        if (trackedPaths.has(browserMockWorkspaces[index].canonical_path)) {
          browserMockWorkspaces.splice(index, 1);
        }
      }
    }
  });

  assert.equal(result.first.id, result.second.id);
  assert.equal(result.firstListed.id, result.first.id);
  assert.equal(result.firstListed.memory_mode, "inherit");
  assert.equal(result.firstListed.memory_generation, 1);
  assert.equal(result.first.canonical_path, "D:/identity-regression/shared-name");
  assert.equal(result.routed.canonical_path, result.first.canonical_path);
  assert.equal(result.routed.id, result.first.id, "path-based navigation must not replace identity");
  assert.notEqual(result.first.id, result.first.canonical_path);
  assert.notEqual(result.first.id, result.first.display_name);
  assert.notEqual(result.first.id, result.sibling.id, "display name must not determine identity");
  assert.notEqual(result.first.id, result.reopened.id, "forget must discard the old identity");

  assert.equal(result.legacyListed.memory_mode, "inherit");
  assert.equal(result.legacyListed.memory_generation, 1);
  assert.equal(result.legacyOpened.id, result.legacyListed.id);
  assert.equal(result.legacyListedAgain.id, result.legacyListed.id);
  assert.match(result.invalidListError, /memory_mode/);
  assert.match(result.invalidOpenError, /memory_mode/);
  await page.close();
});

test("clearing a project removes app records without implying disk deletion", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  // The preview data intentionally starts this project with a live task. Stop it through the
  // same mock IPC runtime so the product guard and the successful removal path are both exercised.
  await page.evaluate(async () => {
    const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
    await browserMockInvoke("cmd_agent_abort", { taskId: "mock-task-api" });
  });

  await page.getByRole("button", { name: "管理项目" }).click();
  const row = page.locator(".workspace-row").filter({ hasText: "api-server" });
  const remove = row.getByRole("button", { name: "从 R-Code 中清除 api-server" });
  await page.waitForFunction(
    () => !document.querySelector('.workspace-row .workspace-remove[aria-label*="api-server"]')?.hasAttribute("disabled"),
  );
  await remove.click();

  const dialog = page.getByRole("alertdialog", { name: "从 R-Code 中清除这个项目？" });
  await dialog.waitFor({ state: "visible" });
  const copy = await dialog.innerText();
  assert.match(copy, /真实文件夹及其中的文件不会被删除、移动或修改/);
  assert.match(copy, /1 段对话以及关联的运行与审计数据/);
  await dialog.getByRole("button", { name: "清除项目", exact: true }).click();

  await page.getByText("项目已从 R-Code 清除", { exact: true }).waitFor({ state: "visible" });
  assert.equal(await page.locator(".workspace-row").filter({ hasText: "api-server" }).count(), 0);
  assert.equal(await page.locator(".sidebar-project").filter({ hasText: "api-server" }).count(), 0);

  await page.locator(".sidebar-nav-item").filter({ hasText: "对话" }).click();
  assert.equal(await page.locator(".conversation-row").filter({ hasText: "添加请求限流中间件" }).count(), 0);
  await page.close();
});
