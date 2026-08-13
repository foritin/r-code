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

async function settleWithin(promise, timeoutMs) {
  let timer;
  const settled = await Promise.race([
    promise.then(() => true, () => true),
    new Promise((resolve) => {
      timer = setTimeout(() => resolve(false), timeoutMs);
    }),
  ]);
  clearTimeout(timer);
  return settled;
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
  if (browser) {
    const closed = await settleWithin(browser.close().catch(() => {}), 3_000);
    if (!closed) browser._connection?.close?.("forced companion test teardown");
  }
  if (server?.exitCode == null) server.kill();
});

test("controller handshake replays lost READY/PREF revisions and transparent padding passes clicks through", async () => {
  const page = await browser.newPage({ viewport: { width: 900, height: 600 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const result = await page.evaluate(async () => {
    const {
      attachMainCompanionHandshake,
      pointHitsCompanionSurface,
    } = await import("/src/components/companion/CompanionWindowController.tsx");
    const {
      COMPANION_PREFERENCES_APPLIED_EVENT,
      COMPANION_PREFERENCES_EVENT,
      COMPANION_READY_EVENT,
    } = await import("/src/components/companion/bridge.ts");

    const listeners = new Map();
    const cleaned = [];
    const sent = [];
    const applied = [];
    let current = {
      revision: 1,
      enabled: true,
      minimized: false,
      soundEnabled: false,
      motion: "system",
    };
    const cleanup = await attachMainCompanionHandshake({
      listen: async (event, handler) => {
        listeners.set(event, handler);
        return () => {
          cleaned.push(event);
          listeners.delete(event);
        };
      },
      readSnapshot: () => current,
      sendSnapshot: async (snapshot) => { sent.push(structuredClone(snapshot)); },
      applySnapshot: (snapshot) => { applied.push(structuredClone(snapshot)); },
    });

    await listeners.get(COMPANION_READY_EVENT)();
    current = { ...current, revision: 3, minimized: true };
    await listeners.get(COMPANION_PREFERENCES_APPLIED_EVENT)({ revision: 1 });
    const incoming = { ...current, revision: 4, soundEnabled: true };
    await listeners.get(COMPANION_PREFERENCES_EVENT)(incoming);

    const sprite = document.createElement("span");
    sprite.className = "companion-sprite-frame";
    Object.assign(sprite.style, {
      position: "fixed",
      left: "100px",
      top: "120px",
      width: "80px",
      height: "100px",
    });
    document.body.append(sprite);
    const visibleHit = pointHitsCompanionSurface(document, 140, 160);
    const transparentPaddingHit = pointHitsCompanionSurface(document, 20, 20);
    sprite.remove();
    cleanup();

    return {
      sent,
      applied,
      cleaned: cleaned.sort(),
      visibleHit,
      transparentPaddingHit,
    };
  });

  assert.deepEqual(result.sent.map((snapshot) => snapshot.revision), [1, 1, 3],
    "initial delivery, replayed READY and stale ACK recovery must all use current snapshots");
  assert.deepEqual(result.applied.map((snapshot) => snapshot.revision), [4]);
  assert.deepEqual(result.cleaned, [
    "r-code:companion-preferences",
    "r-code:companion-preferences-applied",
    "r-code:companion-ready",
  ]);
  assert.equal(result.visibleHit, true);
  assert.equal(result.transparentPaddingHit, false);
  await page.close();
});

test("native companion normalizes restored coordinates before Tauri position IPC", async () => {
  const page = await browser.newPage({ viewport: { width: 420, height: 360 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const normalized = await page.evaluate(async () => {
    const { integerPhysicalPosition } = await import("/src/components/companion/CompanionWindow.tsx");
    return integerPhysicalPosition({
      x: 1253.9999999999998,
      y: 223.99999999999997,
    });
  });

  assert.deepEqual(normalized, { x: 1254, y: 224 });
  await page.close();
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
  const collapsedRects = await page.evaluate(() => {
    const avatar = document.querySelector(".companion-avatar")?.getBoundingClientRect();
    const sprite = document.querySelector(".companion-sprite-frame")?.getBoundingClientRect();
    return {
      avatar: avatar && { width: avatar.width, height: avatar.height },
      sprite: sprite && { width: sprite.width, height: sprite.height },
    };
  });
  const before = await frame.getAttribute("data-frame-index");
  await page.waitForTimeout(360);
  const after = await frame.getAttribute("data-frame-index");
  assert.notEqual(before, after, "the authored sprite sequence must advance frames over time");

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
  const expandedRects = await page.evaluate(() => {
    const avatar = document.querySelector(".companion-avatar")?.getBoundingClientRect();
    const sprite = document.querySelector(".companion-sprite-frame")?.getBoundingClientRect();
    return {
      avatar: avatar && { width: avatar.width, height: avatar.height },
      sprite: sprite && { width: sprite.width, height: sprite.height },
    };
  });
  assert.deepEqual(expandedRects, collapsedRects,
    "opening progress must not squeeze the avatar or its sprite plane");
  assert.equal(await dialog.locator(".companion-session-card:not(.is-exiting)").count(), 4);
  await page.waitForFunction(() => document.activeElement?.classList.contains("companion-session-row"));
  assert.match(await dialog.innerText(), /等待确认|待审阅|正在实施|正在分析项目/);

  const liveRow = dialog.locator(
    ".companion-session-card:has(.companion-session-row.state-working), .companion-session-card:has(.companion-session-row.state-attention)",
  ).first();
  await liveRow.hover();
  const followUp = liveRow.getByRole("button", { name: "继续跟进" });
  const stop = liveRow.getByRole("button", { name: "停止当前运行" });
  assert.equal(await followUp.evaluate((element) => getComputedStyle(element.parentElement).visibility), "visible",
    "hover must reveal the companion pill actions");
  assert.equal(await stop.count(), 1, "live sessions expose the stop action");
  assert.equal(await liveRow.locator("button button").count(), 0, "pill actions must never nest interactive controls");

  await liveRow.locator(".companion-session-row").focus();
  await page.keyboard.press("Tab");
  assert.equal(await page.evaluate(() => document.activeElement?.textContent?.trim()), "继续跟进",
    "focus-within must reveal actions before keyboard focus reaches them");

  const bounds = await page.locator(".companion-window-root").evaluate((root) => ({
    viewport: { width: innerWidth, height: innerHeight },
    root: root.getBoundingClientRect().toJSON(),
    panel: root.querySelector(".companion-session-panel")?.getBoundingClientRect().toJSON(),
    list: root.querySelector(".companion-session-list")?.getBoundingClientRect().toJSON(),
  }));
  assert.equal(bounds.viewport.width, 420);
  assert.equal(bounds.viewport.height, 360);
  assert.ok(bounds.panel.left >= 0 && bounds.panel.right <= 420 && bounds.panel.top >= 0 && bounds.panel.bottom <= 360,
    `panel overflowed 420x360: ${JSON.stringify(bounds.panel)}`);
  assert.ok(bounds.list.left >= 0 && bounds.list.right <= 420 && bounds.list.top >= 0 && bounds.list.bottom <= 360,
    `scroll list overflowed 420x360: ${JSON.stringify(bounds.list)}`);
  await dialog.getByRole("button", { name: "关闭最近任务" }).click();
  await dialog.waitFor({ state: "detached" });

  const transitionedTask = await page.evaluate(async () => {
    const { useTasksStore } = await import("/src/store/tasks.ts");
    const state = useTasksStore.getState();
    const task = state.tasks.find((item) => item.state === "in_progress");
    if (!task) throw new Error("browser mock is missing an in-progress task");
    const completedAt = new Date().toISOString();
    const detail = state.details[task.id];
    useTasksStore.setState({
      tasks: state.tasks.map((item) => item.id === task.id
        ? { ...item, state: "review_ready", updated_at: completedAt }
        : item),
      details: detail ? {
        ...state.details,
        [task.id]: {
          ...detail,
          task: { ...detail.task, state: "review_ready", updated_at: completedAt },
          runs: detail.runs.map((run) => ({ ...run, ended_at: run.ended_at ?? completedAt })),
        },
      } : state.details,
    });
    return { id: task.id, title: task.title };
  });
  await page.waitForFunction(() => document.querySelector(".companion-unread-badge")?.textContent === "1");
  assert.match(await avatar.getAttribute("aria-label"), /1 个未读/);

  await avatar.click();
  const transitionedCard = dialog.locator(".companion-session-card").filter({ hasText: transitionedTask.title });
  await transitionedCard.hover();
  await transitionedCard.getByRole("button", { name: "继续跟进" }).click();
  await dialog.waitFor({ state: "detached" });
  assert.equal(await page.locator(".companion-unread-badge").count(), 0,
    "browser navigation acknowledgement clears only the selected session unread state");

  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.waitForTimeout(100);
  const reducedFrame = await frame.getAttribute("data-frame-index");
  await page.waitForTimeout(360);
  assert.equal(await frame.getAttribute("data-frame-index"), reducedFrame,
    "system reduced-motion freezes the authored sequence on a representative frame");
  await page.evaluate(async () => {
    const { useCompanionStore } = await import("/src/store/companion.ts");
    useCompanionStore.getState().setMotion("full");
  });
  await page.waitForFunction(() => document.querySelector(".companion-window-root")?.classList.contains("motion-full"));
  const currentFrame = page.locator(".companion-frame-layer.is-current .companion-sprite-frame");
  const fullBefore = await currentFrame.getAttribute("data-frame-index");
  await page.waitForTimeout(360);
  assert.notEqual(await currentFrame.getAttribute("data-frame-index"), fullBefore,
    "an explicit full-motion preference must override the OS reduced-motion setting");
  await page.evaluate(async () => {
    const { useCompanionStore } = await import("/src/store/companion.ts");
    useCompanionStore.getState().setMotion("system");
  });
  await page.waitForTimeout(80);
  await page.emulateMedia({ reducedMotion: "no-preference" });
  const resumedBefore = await frame.getAttribute("data-frame-index");
  await page.waitForTimeout(360);
  assert.notEqual(await frame.getAttribute("data-frame-index"), resumedBefore);

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

test("running and unread sessions surface automatically above the assistant", async () => {
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
      "mock-task-review",
      "mock-task-complete",
    ]));
  });
  await page.goto(`${baseUrl}?window=companion`, { waitUntil: "networkidle" });
  const stack = page.getByRole("region", { name: "Session 进度提醒" });
  await stack.waitFor({ state: "visible" });
  assert.equal(await stack.locator(".companion-session-card").count(), 2,
    "the automatic stack is bounded while keeping urgent/live sessions visible");
  assert.equal(await page.getByRole("dialog", { name: "最近任务" }).count(), 0,
    "automatic progress does not require opening the recent-session dialog");
  const live = stack.locator(".companion-session-card").filter({ hasText: "优化 Rust 编译性能" });
  await live.hover();
  assert.equal(await live.getByRole("button", { name: "继续跟进" }).count(), 1);
  assert.equal(await live.getByRole("button", { name: "停止当前运行" }).count(), 1);
  const bounds = await stack.evaluate((element) => {
    const rect = element.getBoundingClientRect();
    return { top: rect.top, right: rect.right, bottom: rect.bottom, left: rect.left };
  });
  assert.ok(bounds.top >= 0 && bounds.left >= 0 && bounds.right <= 420 && bounds.bottom <= 360,
    `automatic pulse stack overflowed: ${JSON.stringify(bounds)}`);
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
        runs: detail.runs.map((run) => ({
          ...run,
          ended_at: run.ended_at ?? new Date().toISOString(),
        })),
      }])),
    });
  });
  await page.waitForFunction(() => document.querySelector(".companion-window-root")?.classList.contains("state-idle"));
  await page.mouse.move(0, 0);
  await avatar.hover();
  await page.waitForFunction(() => document.querySelector('[data-sprite-state="sing"]') !== null);
  const idleReference = await page.locator('[data-sprite-state="sing"]').evaluate((frame) => ({
    image: getComputedStyle(frame).backgroundImage,
    size: getComputedStyle(frame).backgroundSize,
    width: frame.getBoundingClientRect().width,
    height: frame.getBoundingClientRect().height,
  }));
  assert.match(idleReference.image, /r-code-miku-v4\.webp/,
    "hover gestures must reuse the registered silhouette instead of swapping to a narrower atlas");
  assert.equal(idleReference.size, "800% 900%");
  assert.ok(Math.abs(idleReference.width - 168) < 0.1);
  assert.ok(Math.abs(idleReference.height - 182) < 0.1);
  await page.mouse.move(0, 0);
  await avatar.hover();
  await page.waitForTimeout(220);
  assert.equal(await page.locator('[data-sprite-state="dance"]').count(), 0,
    "rapid pointer re-entry must not interrupt a performance mid-transition");
  await page.mouse.move(0, 0);
  await page.waitForTimeout(3_250);
  await page.mouse.move(0, 0);
  await avatar.hover();
  await page.waitForFunction(() => {
    const frame = document.querySelector(".companion-frame-layer.is-current .companion-sprite-frame");
    return frame?.getAttribute("data-sprite-state") === "dance"
      || frame?.getAttribute("data-sprite-state") === "sing";
  });

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

test("session pills stop a live run once and keep failed stops unread", async () => {
  const page = await browser.newPage({ viewport: { width: 420, height: 360 } });
  await page.addInitScript(() => {
    localStorage.setItem("r-code.companion.preferences.v2", JSON.stringify({
      revision: 1,
      enabled: true,
      minimized: false,
      soundEnabled: false,
      motion: "reduced",
    }));
    localStorage.setItem("r-code.companion.unread-sessions.v1", JSON.stringify(["mock-task-queue"]));
    window.__abortCalls = 0;
    window.__rCodePerformanceIpcProbe = (command) => {
      if (command === "cmd_agent_abort") window.__abortCalls += 1;
    };
    window.__rCodeBrowserMockDelayMs = { cmd_agent_abort: 180 };
  });
  await page.goto(`${baseUrl}?window=companion`, { waitUntil: "networkidle" });
  const avatar = page.getByRole("button", { name: /R-Code session 助手/ });
  await avatar.waitFor({ state: "visible" });
  await avatar.click();
  const dialog = page.getByRole("dialog", { name: "最近任务" });
  const liveCard = dialog.locator(".companion-session-card").filter({ hasText: "修复任务队列并发问题" });
  await liveCard.hover();
  const stopButton = liveCard.getByRole("button", { name: "停止当前运行" });
  await stopButton.dblclick({ delay: 10 });
  await page.waitForFunction(() => window.__abortCalls === 1);
  await page.waitForFunction(async () => {
    const { useTasksStore } = await import("/src/store/tasks.ts");
    return useTasksStore.getState().tasks.find((task) => task.id === "mock-task-queue")?.state === "interrupted";
  });
  assert.equal(await page.evaluate(() => window.__abortCalls), 1,
    "a rapid double click must issue exactly one abort command per task");

  await page.evaluate(async () => {
    const { browserMockTasks, browserMockDetails } = await import("/src/lib/mock-data.ts");
    const task = browserMockTasks.find((item) => item.id === "mock-task-api");
    if (!task) throw new Error("browser mock is missing mock-task-api");
    task.state = "exploring";
    task.updated_at = new Date(Date.now() + 2_000).toISOString();
    browserMockDetails[task.id].task = { ...task };
    window.__rCodeBrowserMockFailures = { cmd_agent_abort: "demo abort unavailable" };
    const { useTasksStore } = await import("/src/store/tasks.ts");
    useTasksStore.setState({
      tasks: useTasksStore.getState().tasks.map((item) => item.id === task.id ? { ...task } : item),
    });
  });
  const failedCard = dialog.locator(".companion-session-card").filter({ hasText: "添加请求限流中间件" });
  await failedCard.hover();
  await failedCard.getByRole("button", { name: "停止当前运行" }).click();
  const alert = failedCard.getByRole("alert");
  await alert.waitFor({ state: "visible" });
  assert.match(await alert.innerText(), /停止失败.*demo abort unavailable/);
  assert.match(await avatar.getAttribute("aria-label"), /[1-9] 个未读/,
    "a failed stop must retain an unread reminder");
  await page.waitForFunction(() => {
    const card = [...document.querySelectorAll(".companion-session-card")]
      .find((element) => element.textContent?.includes("添加请求限流中间件"));
    const stop = card?.querySelector("button.is-stop");
    return stop instanceof HTMLButtonElement && !stop.disabled;
  });
  assert.equal(await failedCard.locator("button.is-stop").isEnabled(), true,
    "a failed stop can be retried after the in-flight guard is released");
  await page.close();
});
