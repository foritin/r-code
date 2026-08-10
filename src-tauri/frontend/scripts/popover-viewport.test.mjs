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

async function assertBounded(locator, { scrollable = false, innerSelector } = {}) {
  const metrics = await locator.evaluate((element, selector) => {
    const rect = element.getBoundingClientRect();
    const inner = selector ? element.querySelector(selector) : element;
    return {
      rect: { top: rect.top, right: rect.right, bottom: rect.bottom, left: rect.left },
      viewport: { width: innerWidth, height: innerHeight },
      scrollHeight: inner?.scrollHeight ?? 0,
      clientHeight: inner?.clientHeight ?? 0,
    };
  }, innerSelector);

  // CSS zoom and fractional device pixels can turn the intended 8px margin into ~6px.
  // The regression boundary only needs to prove that no edge is clipped.
  const visibleGap = 3;
  assert.ok(metrics.rect.top >= visibleGap, `top ${metrics.rect.top} must remain inside the viewport`);
  assert.ok(metrics.rect.left >= visibleGap, `left ${metrics.rect.left} must remain inside the viewport`);
  assert.ok(metrics.rect.bottom <= metrics.viewport.height - visibleGap, `bottom ${metrics.rect.bottom} exceeds ${metrics.viewport.height}`);
  assert.ok(metrics.rect.right <= metrics.viewport.width - visibleGap, `right ${metrics.rect.right} exceeds ${metrics.viewport.width}`);
  if (scrollable) {
    assert.ok(metrics.scrollHeight > metrics.clientHeight, "long floating content must scroll internally");
  }
}

async function rect(locator) {
  return locator.evaluate((element) => {
    const bounds = element.getBoundingClientRect();
    return {
      top: bounds.top,
      right: bounds.right,
      bottom: bounds.bottom,
      left: bounds.left,
      width: bounds.width,
      height: bounds.height,
    };
  });
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

test("floating surfaces stay inside a very small viewport and scroll internally", async () => {
  const page = await browser.newPage({ viewport: { width: 520, height: 180 } });
  const runtimeErrors = [];
  page.on("pageerror", (error) => runtimeErrors.push(String(error)));
  page.on("console", (message) => {
    if (message.type() === "error") runtimeErrors.push(message.text());
  });

  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.getByRole("button", { name: "对话", exact: true }).click();
  await page.locator(".conversation-row").filter({ hasText: "更新依赖并修复告警" }).locator(".conversation-main").click();
  await page.locator("#main-content > .scene-room").waitFor({ state: "visible" });

  const modelTrigger = page.locator(".model-config-trigger");
  await modelTrigger.click();
  const modelDialog = page.getByRole("dialog", { name: "Codex 模型与推理配置", exact: true });
  await modelDialog.waitFor({ state: "visible" });
  await modelDialog.getByRole("button", { name: /^模型 / }).click();
  await page.waitForTimeout(100);
  await assertBounded(modelDialog, { scrollable: true });
  await modelTrigger.click();

  const composer = page.getByRole("textbox", { name: "给 Agent 的消息", exact: true });
  await composer.fill("/");
  const slashMenu = page.getByRole("listbox", { name: "斜杠命令", exact: true });
  await slashMenu.waitFor({ state: "visible" });
  await page.waitForTimeout(100);
  await assertBounded(slashMenu, { scrollable: true, innerSelector: ".slash-menu-list" });
  await composer.press("Escape");
  assert.equal(await slashMenu.count(), 0, "Escape must dismiss command completion");

  await page.getByRole("button", { name: /通知中心/ }).click();
  const notificationDialog = page.getByRole("dialog", { name: "通知中心", exact: true });
  await notificationDialog.waitFor({ state: "visible" });
  await page.waitForTimeout(100);
  await assertBounded(notificationDialog, { scrollable: true, innerSelector: ".notification-menu-list" });

  assert.deepEqual(runtimeErrors, []);
  await page.close();
});

test("sidebar project and conversation actions share one compact side-menu pattern", async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const runtimeErrors = [];
  page.on("pageerror", (error) => runtimeErrors.push(String(error)));
  page.on("console", (message) => {
    if (message.type() === "error") runtimeErrors.push(message.text());
  });

  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const sidebar = page.locator(".app-sidebar");
  const sidebarRect = await rect(sidebar);

  assert.equal(await page.locator(".sidebar-live").count(), 0, "projects must not duplicate the running marker");
  assert.ok(await page.locator(".sidebar-task .task-state-dot.running").count() > 0, "running sessions keep their status dot");

  const projectTrigger = page.getByRole("button", { name: "r-code 项目操作", exact: true });
  await projectTrigger.click();
  const projectMenu = page.getByRole("menu", { name: "r-code 项目操作", exact: true });
  await projectMenu.waitFor({ state: "visible" });
  await page.waitForTimeout(120); // compare settled geometry, not the entry-scale transition
  const projectRect = await rect(projectMenu);
  assert.ok(
    projectRect.left >= sidebarRect.right + 3,
    `project menu ${JSON.stringify(projectRect)} must open beside sidebar ${JSON.stringify(sidebarRect)}`,
  );
  assert.equal(await projectMenu.locator(".menu-item small").count(), 0, "project actions stay compact");
  const projectStyle = await projectMenu.evaluate((element) => {
    const style = getComputedStyle(element);
    return { borderRadius: style.borderRadius, padding: style.padding };
  });

  await page.keyboard.press("Escape");
  assert.equal(await projectMenu.count(), 0, "Escape must dismiss the project menu");
  assert.equal(await projectTrigger.evaluate((element) => document.activeElement === element), true, "focus returns to the project trigger");

  const taskRow = page.locator(".sidebar-task-row").filter({ hasText: "更新依赖并修复告警" });
  await taskRow.hover();
  const taskTrigger = taskRow.getByRole("button", { name: "管理对话：更新依赖并修复告警", exact: true });
  await taskTrigger.click();
  const taskMenu = page.getByRole("menu", { name: "管理对话：更新依赖并修复告警", exact: true });
  await taskMenu.waitFor({ state: "visible" });
  await page.waitForTimeout(120);
  const taskRect = await rect(taskMenu);
  assert.ok(
    taskRect.left >= sidebarRect.right + 3,
    `conversation menu ${JSON.stringify(taskRect)} must open beside sidebar ${JSON.stringify(sidebarRect)}`,
  );
  assert.ok(
    Math.abs(taskRect.width - projectRect.width) < 1,
    `project ${JSON.stringify(projectRect)} and conversation ${JSON.stringify(taskRect)} menus use the same width`,
  );
  assert.equal(await taskMenu.locator(".menu-item small").count(), 0, "conversation actions stay compact");

  const taskStyle = await taskMenu.evaluate((element) => {
    const style = getComputedStyle(element);
    return { borderRadius: style.borderRadius, padding: style.padding };
  });
  assert.deepEqual(taskStyle, projectStyle, "both sidebar menus use the same surface treatment");

  await page.keyboard.press("Escape");
  await projectTrigger.click();
  await projectMenu.getByRole("menuitem", { name: "新建对话", exact: true }).click();
  await page.locator("#main-content > .scene-room").waitFor({ state: "visible" });
  assert.equal(
    await page.locator(".sidebar-task-row.active").getByText("新对话", { exact: true }).count(),
    1,
    "project-scoped creation must persist and select the empty conversation immediately",
  );
  assert.match(
    await page.locator("#main-content > .scene-room").innerText(),
    /r-code · 会话就绪/,
    "the durable conversation keeps the selected project attached",
  );

  assert.deepEqual(runtimeErrors, []);
  await page.close();
});
