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
  return [
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    path.join(process.env.PROGRAMFILES ?? "", "Google", "Chrome", "Application", "chrome.exe"),
    path.join(process.env.PROGRAMFILES ?? "", "Microsoft", "Edge", "Application", "msedge.exe"),
    "/usr/bin/google-chrome",
    "/usr/bin/chromium",
  ].find((candidate) => candidate && fs.existsSync(candidate));
}

async function freePort() {
  return new Promise((resolve, reject) => {
    const candidate = net.createServer();
    candidate.once("error", reject);
    candidate.listen(0, "127.0.0.1", () => {
      const address = candidate.address();
      candidate.close((error) => error
        ? reject(error)
        : resolve(typeof address === "object" && address ? address.port : 0));
    });
  });
}

async function waitForServer(url, child) {
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    if (child.exitCode != null) throw new Error(`Vite exited with ${child.exitCode}`);
    try {
      if ((await fetch(url)).ok) return;
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

test("independent session assistant animates, reports unread progress and exposes only Close on right click", async () => {
  const page = await browser.newPage({ viewport: { width: 420, height: 360 } });
  await page.addInitScript(() => {
    localStorage.removeItem("r-code.companion.preferences.v2");
    localStorage.removeItem("r-code.companion.unread-sessions.v1");
  });
  await page.goto(`${baseUrl}?window=companion`, { waitUntil: "networkidle" });

  const root = page.locator(".companion-window-root");
  const avatar = page.getByRole("button", { name: /R-Code session 助手/ });
  await avatar.waitFor({ state: "visible" });
  assert.equal(await page.locator(".app-shell, .rail, .app-topbar").count(), 0,
    "the companion entry must not mount the main application shell");

  await page.waitForTimeout(2_300);
  assert.equal(await page.locator(".companion-unread-badge").count(), 0,
    "the startup snapshot must establish a baseline without fake unread progress");

  const frame = page.locator(".companion-sprite-frame");
  const before = await frame.evaluate((element) => getComputedStyle(element).transform);
  await page.waitForTimeout(360);
  const after = await frame.evaluate((element) => getComputedStyle(element).transform);
  assert.notEqual(before, after, "the full-motion sprite must visibly move over time");

  await avatar.hover();
  await page.waitForFunction(() => document.querySelector(".companion-window-root")?.classList.contains("is-hovered"));
  assert.equal(await page.locator(".companion-hover-spark").count(), 1,
    "pointer entry should show a lightweight reaction without pointermove tracking");

  await avatar.click({ button: "right" });
  const menu = page.getByRole("menu");
  await menu.waitFor({ state: "visible" });
  assert.equal(await menu.getByRole("menuitem").count(), 1);
  assert.equal(await menu.getByRole("menuitem").innerText(), "关闭小助手");
  await page.keyboard.press("Escape");
  await menu.waitFor({ state: "detached" });
  await page.waitForFunction(() => document.activeElement?.classList.contains("companion-avatar"));

  await page.keyboard.press("Shift+F10");
  await menu.waitFor({ state: "visible" });
  assert.equal(await menu.getByRole("menuitem").count(), 1);
  await page.keyboard.press("Escape");

  await avatar.click();
  const dialog = page.getByRole("dialog", { name: "最近任务" });
  await dialog.waitFor({ state: "visible" });
  assert.equal(await dialog.getByRole("listitem").count(), 4);
  await page.waitForFunction(() => document.activeElement?.classList.contains("companion-session-row"));
  assert.match(await dialog.innerText(), /等待确认|待审阅|正在实施|正在分析项目/);
  await dialog.getByRole("button", { name: "关闭最近任务" }).click();
  await dialog.waitFor({ state: "detached" });

  const transitionedTask = await page.evaluate(async () => {
    const { useTasksStore } = await import("/src/store/tasks.ts");
    const state = useTasksStore.getState();
    const task = state.tasks.find((item) => item.state === "in_progress");
    if (!task) throw new Error("browser mock is missing an in-progress task");
    useTasksStore.setState({
      tasks: state.tasks.map((item) => item.id === task.id
        ? { ...item, state: "review_ready", updated_at: new Date().toISOString() }
        : item),
    });
    return { id: task.id, title: task.title };
  });
  await page.waitForFunction(() => document.querySelector(".companion-unread-badge")?.textContent === "1");
  assert.match(await avatar.getAttribute("aria-label"), /1 个未读/);

  await avatar.click();
  const transitionedRow = dialog.getByRole("button", { name: new RegExp(transitionedTask.title) });
  await transitionedRow.click();
  await dialog.waitFor({ state: "detached" });
  assert.equal(await page.locator(".companion-unread-badge").count(), 0,
    "browser navigation acknowledgement clears only the selected session unread state");

  await page.emulateMedia({ reducedMotion: "reduce" });
  assert.equal(await frame.evaluate((element) => getComputedStyle(element).animationName), "none");
  await page.evaluate(async () => {
    const { useCompanionStore } = await import("/src/store/companion.ts");
    useCompanionStore.getState().setMotion("full");
  });
  await page.waitForFunction(() => document.querySelector(".companion-window-root")?.classList.contains("motion-full"));
  const currentFrame = page.locator(".companion-frame-layer.is-current .companion-sprite-frame");
  assert.notEqual(await currentFrame.evaluate((element) => getComputedStyle(element).animationName), "none",
    "an explicit full-motion preference must override the OS reduced-motion setting");
  await page.evaluate(async () => {
    const { useCompanionStore } = await import("/src/store/companion.ts");
    useCompanionStore.getState().setMotion("system");
  });
  await page.waitForFunction(() => getComputedStyle(document.querySelector(".companion-sprite-frame")).animationName === "none");
  await page.emulateMedia({ reducedMotion: "no-preference" });
  assert.notEqual(await frame.evaluate((element) => getComputedStyle(element).animationName), "none");

  await avatar.click({ button: "right" });
  await menu.getByRole("menuitem", { name: "关闭小助手" }).click();
  await root.waitFor({ state: "detached" });

  await page.evaluate(async () => {
    const { useCompanionStore } = await import("/src/store/companion.ts");
    useCompanionStore.getState().setEnabled(true);
  });
  await avatar.waitFor({ state: "visible" });
  await page.close();
});

test("urgent progress wins the four-row limit and archived sessions cannot leave ghost unread badges", async () => {
  const page = await browser.newPage({ viewport: { width: 420, height: 360 } });
  await page.addInitScript(() => {
    localStorage.setItem("r-code.companion.preferences.v2", JSON.stringify({
      revision: 1,
      enabled: true,
      minimized: false,
      soundEnabled: false,
      motion: "full",
    }));
    localStorage.setItem("r-code.companion.unread-sessions.v1", JSON.stringify([
      "mock-task-queue",
      "mock-task-review",
      "mock-task-permission",
      "mock-task-api",
      "mock-task-complete",
    ]));
  });
  await page.goto(`${baseUrl}?window=companion`, { waitUntil: "networkidle" });
  const avatar = page.getByRole("button", { name: /R-Code session 助手/ });
  await avatar.waitFor({ state: "visible" });
  await page.waitForFunction(() => document.querySelector(".companion-unread-badge")?.textContent === "5");
  await avatar.click();
  const dialog = page.getByRole("dialog", { name: "最近任务" });
  await dialog.waitFor({ state: "visible" });
  assert.equal(await dialog.getByRole("button", { name: /优化 Rust 编译性能/ }).count(), 1,
    "a pending-permission session must remain visible even when five sessions are unread");
  assert.equal(await dialog.getByRole("button", { name: /更新依赖并修复告警/ }).count(), 0,
    "the low-priority completed session should yield the four-row slot");
  await dialog.getByRole("button", { name: "关闭最近任务" }).click();

  await page.evaluate(async () => {
    const { useTasksStore } = await import("/src/store/tasks.ts");
    const { browserMockTasks, browserMockDetails } = await import("/src/lib/mock-data.ts");
    browserMockTasks.forEach((task) => { task.state = "idle"; });
    Object.values(browserMockDetails).forEach((detail) => {
      detail.task.state = "idle";
      detail.permissions.forEach((permission) => { permission.decision = "allow"; });
    });
    const state = useTasksStore.getState();
    useTasksStore.setState({
      tasks: state.tasks.map((task) => task.id === "mock-task-complete"
        ? { ...task, state: "archived", updated_at: new Date().toISOString() }
        : task),
    });
  });
  await page.waitForFunction(() => document.querySelector(".companion-unread-badge")?.textContent === "4");
  assert.match(await avatar.getAttribute("aria-label"), /4 个未读/);

  await page.evaluate(async () => {
    const { useTasksStore } = await import("/src/store/tasks.ts");
    const state = useTasksStore.getState();
    useTasksStore.setState({
      tasks: state.tasks.map((task) => ({ ...task, state: "idle" })),
      details: Object.fromEntries(Object.entries(state.details).map(([taskId, detail]) => [taskId, {
        ...detail,
        task: { ...detail.task, state: "idle" },
        permissions: detail.permissions.map((permission) => ({ ...permission, decision: "allow" })),
      }])),
    });
  });
  await page.waitForFunction(() => document.querySelector(".companion-window-root")?.classList.contains("state-idle"));
  await page.mouse.move(0, 0);
  await avatar.hover();
  await page.waitForFunction(() => document.querySelector(".sprite-state-sing") !== null);
  await page.mouse.move(0, 0);
  await avatar.hover();
  await page.waitForTimeout(220);
  assert.equal(await page.locator(".sprite-state-dance").count(), 0,
    "rapid pointer re-entry must not interrupt a performance mid-transition");
  await page.mouse.move(0, 0);
  await page.waitForTimeout(3_100);
  await avatar.hover();
  await page.waitForFunction(() => document.querySelector(".sprite-state-dance") !== null);

  await page.evaluate(async () => {
    window.__companionAudioClosed = 0;
    window.AudioContext = class {
      state = "suspended";
      resume() { return Promise.reject(new Error("autoplay denied")); }
      close() { window.__companionAudioClosed += 1; return Promise.resolve(); }
    };
    const { useCompanionStore } = await import("/src/store/companion.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    useCompanionStore.getState().setSoundEnabled(true);
    const state = useTasksStore.getState();
    const target = state.tasks.find((task) => task.state === "idle" && task.id !== "mock-task-complete");
    if (!target) throw new Error("browser mock is missing an idle task for the audio failure case");
    useTasksStore.setState({
      tasks: state.tasks.map((task) => task.id === target.id
        ? { ...task, state: "interrupted", updated_at: new Date().toISOString() }
        : task),
    });
  });
  await page.waitForFunction(() => window.__companionAudioClosed === 1);
  await page.close();
});
