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

async function launchBrowser() {
  try {
    // Use Playwright's own registry first. This honors PLAYWRIGHT_BROWSERS_PATH and selects the
    // Chromium revision paired with the installed playwright-core package.
    return await chromium.launch({ headless: true });
  } catch (registryError) {
    const executablePath = browserExecutable();
    if (!executablePath) throw registryError;
    // Developer machines may intentionally rely on an installed Chrome/Edge instead of downloading
    // Playwright Chromium. Keep that local fallback without guessing another project's cache entry.
    return chromium.launch({ executablePath, headless: true });
  }
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
  browser = await launchBrowser();
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
    const avatar = document.createElement("button");
    avatar.className = "companion-avatar";
    Object.assign(avatar.style, {
      position: "fixed",
      left: "400px",
      top: "300px",
      width: "168px",
      height: "196px",
    });
    document.body.append(avatar);
    const visibleHit = pointHitsCompanionSurface(document, 140, 160);
    const avatarEdgeHit = pointHitsCompanionSurface(document, 404, 480);
    const transparentPaddingHit = pointHitsCompanionSurface(document, 20, 20);
    avatar.remove();
    sprite.remove();
    cleanup();

    return {
      sent,
      applied,
      cleaned: cleaned.sort(),
      visibleHit,
      avatarEdgeHit,
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
  assert.equal(result.avatarEdgeHit, true,
    "the avatar button's own bounds must stay interactive; only inner sprite rects would leave the pet's fringe click-through");
  assert.equal(result.transparentPaddingHit, false);
  await page.close();
});

test("native companion normalizes restored coordinates before Tauri position IPC", async () => {
  const page = await browser.newPage({ viewport: { width: 420, height: 360 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const result = await page.evaluate(async () => {
    const {
      compactLayoutForNativeHeight,
      integerPhysicalPosition,
      pulseWindowSize,
    } = await import("/src/components/companion/CompanionWindow.tsx");
    return {
      normalized: integerPhysicalPosition({
        x: 1253.9999999999998,
        y: 223.99999999999997,
      }),
      layouts: [
        compactLayoutForNativeHeight(false, true, 2, 360),
        compactLayoutForNativeHeight(false, true, 2, 440),
        compactLayoutForNativeHeight(false, true, 3, 520),
        compactLayoutForNativeHeight(true, true, 2, 196),
      ],
      // 窗口按可见内容包围盒收缩后，各种行数/模式的精确请求尺寸。
      pulseSizes: [
        pulseWindowSize(1, false),
        pulseWindowSize(2, false),
        pulseWindowSize(3, false),
        pulseWindowSize(1, true),
        pulseWindowSize(2, true),
      ],
      // 收缩后的精确适配：一行 342、两行 438，mini 头像一行 262。
      exactFitLayouts: [
        compactLayoutForNativeHeight(false, true, 2, 342),
        compactLayoutForNativeHeight(false, true, 2, 437),
        compactLayoutForNativeHeight(false, true, 2, 435),
        compactLayoutForNativeHeight(true, true, 1, 262),
        compactLayoutForNativeHeight(true, true, 2, 259),
      ],
    };
  });

  assert.deepEqual(result.normalized, { x: 1254, y: 224 });
  assert.deepEqual(result.layouts, [
    { minimized: false, hasTracking: true, rows: 1 },
    { minimized: false, hasTracking: true, rows: 2 },
    { minimized: false, hasTracking: true, rows: 2 },
    { minimized: true, hasTracking: false, rows: 0 },
  ], "the DOM must expose only task rows that the native WebView actually accepted");
  assert.deepEqual(result.pulseSizes, [
    { width: 272, height: 342 },
    { width: 272, height: 438 },
    { width: 272, height: 534 },
    { width: 272, height: 262 },
    { width: 272, height: 358 },
  ], "the tracking window must hug the visible avatar+card bounding box, not the old 420x360 slab");
  assert.deepEqual(result.exactFitLayouts, [
    { minimized: false, hasTracking: true, rows: 1 },
    { minimized: false, hasTracking: true, rows: 2 },
    { minimized: false, hasTracking: true, rows: 1 },
    { minimized: true, hasTracking: true, rows: 1 },
    { minimized: true, hasTracking: false, rows: 0 },
  ], "row capacity must follow the shrunk per-mode content floor (full 342 / mini 262)");
  await page.close();
});

test("task completion keeps the tracked avatar footprint continuous and fully visible", async () => {
  const page = await browser.newPage({ viewport: { width: 420, height: 520 } });
  await page.addInitScript(() => {
    localStorage.setItem("r-code.companion.preferences.v2", JSON.stringify({
      revision: 1,
      enabled: true,
      minimized: true,
      soundEnabled: false,
      motion: "reduced",
    }));
    localStorage.removeItem("r-code.companion.unread-sessions.v1");
  });
  await page.goto(`${baseUrl}?window=companion`, { waitUntil: "networkidle" });
  const avatar = page.getByRole("button", { name: /R-Code session 助手/ });
  await avatar.waitFor({ state: "visible" });
  await page.waitForTimeout(2_300);

  const target = await page.evaluate(async () => {
    const { useTasksStore } = await import("/src/store/tasks.ts");
    const state = useTasksStore.getState();
    const task = state.tasks.find((item) => item.id === "mock-task-queue");
    if (!task) throw new Error("browser mock is missing the completion target");
    const endedAt = new Date().toISOString();
    useTasksStore.setState({
      tasks: state.tasks.map((item) => item.id === task.id
        ? { ...item, state: "idle", updated_at: endedAt }
        : { ...item, state: "archived", updated_at: endedAt }),
      details: Object.fromEntries(Object.entries(state.details).map(([taskId, detail]) => [taskId, {
        ...detail,
        task: {
          ...detail.task,
          state: taskId === task.id ? "idle" : "archived",
          updated_at: endedAt,
        },
        permissions: detail.permissions.map((permission) => ({ ...permission, decision: "allow" })),
        runs: detail.runs.map((run) => ({ ...run, ended_at: run.ended_at ?? endedAt })),
      }])),
    });
    return { id: task.id, title: task.title };
  });
  await page.waitForFunction(() => document.querySelector(".companion-unread-badge")?.textContent === "1");
  await avatar.click();
  const dialog = page.getByRole("dialog", { name: "最近任务" });
  await dialog.waitFor({ state: "visible" });
  const targetCard = dialog.locator(".companion-session-card").filter({ hasText: target.title });
  await targetCard.locator(".companion-session-row").click();
  await dialog.waitFor({ state: "detached" });
  await page.waitForFunction(() => !document.querySelector(".companion-window-root")?.classList.contains("has-tracking"));

  await page.evaluate(async (taskId) => {
    const { useTasksStore } = await import("/src/store/tasks.ts");
    const state = useTasksStore.getState();
    const startedAt = new Date().toISOString();
    const detail = state.details[taskId];
    useTasksStore.setState({
      tasks: state.tasks.map((item) => item.id === taskId
        ? { ...item, state: "in_progress", updated_at: startedAt }
        : item),
      details: detail ? {
        ...state.details,
        [taskId]: {
          ...detail,
          task: { ...detail.task, state: "in_progress", updated_at: startedAt },
          runs: detail.runs.map((run, index) => index === 0
            ? { ...run, started_at: run.started_at ?? startedAt, ended_at: null }
            : run),
        },
      } : state.details,
    });
  }, target.id);
  await page.waitForFunction(() => {
    const root = document.querySelector(".companion-window-root");
    return root?.classList.contains("state-working") && root.classList.contains("has-tracking");
  });
  await page.waitForTimeout(80);
  const before = await avatar.evaluate((element) => element.getBoundingClientRect().toJSON());

  await page.evaluate(async (taskId) => {
    window.__companionTrackingTransitions = [];
    const root = document.querySelector(".companion-window-root");
    const sample = () => {
      const avatar = document.querySelector(".companion-avatar")?.getBoundingClientRect();
      window.__companionTrackingTransitions.push({
        hasTracking: root?.classList.contains("has-tracking") ?? false,
        avatarTop: avatar?.top ?? null,
      });
    };
    const observer = new MutationObserver(sample);
    observer.observe(root, { attributes: true, attributeFilter: ["class"] });
    window.__companionTrackingObserver = observer;
    sample();

    const { useTasksStore } = await import("/src/store/tasks.ts");
    const state = useTasksStore.getState();
    const endedAt = new Date().toISOString();
    const detail = state.details[taskId];
    useTasksStore.setState({
      tasks: state.tasks.map((item) => item.id === taskId
        ? { ...item, state: "idle", updated_at: endedAt }
        : item),
      details: detail ? {
        ...state.details,
        [taskId]: {
          ...detail,
          task: { ...detail.task, state: "idle", updated_at: endedAt },
          runs: detail.runs.map((run) => ({ ...run, ended_at: endedAt })),
        },
      } : state.details,
    });
  }, target.id);
  await page.waitForFunction(() => {
    const root = document.querySelector(".companion-window-root");
    return root?.classList.contains("state-success") && root.classList.contains("has-tracking");
  });
  await page.waitForTimeout(120);
  const result = await page.evaluate(() => {
    window.__companionTrackingObserver?.disconnect();
    const avatar = document.querySelector(".companion-avatar")?.getBoundingClientRect();
    const sprite = document.querySelector(".companion-sprite-frame")?.getBoundingClientRect();
    return {
      transitions: window.__companionTrackingTransitions,
      avatar: avatar?.toJSON(),
      sprite: sprite?.toJSON(),
      spriteState: document.querySelector(".companion-sprite-frame")?.getAttribute("data-sprite-state"),
      backgroundSize: getComputedStyle(document.querySelector(".companion-sprite-frame")).backgroundSize,
    };
  });
  assert.ok(result.transitions.every((entry) => entry.hasTracking),
    `completion must not briefly collapse the native tracking footprint: ${JSON.stringify(result.transitions)}`);
  assert.equal(result.avatar.top, before.top);
  assert.equal(result.avatar.left, before.left);
  assert.ok(result.sprite.top >= result.avatar.top && result.sprite.bottom <= result.avatar.bottom,
    `the completion sprite must remain fully inside its avatar viewport: ${JSON.stringify(result)}`);
  assert.equal(result.spriteState, "success");
  assert.equal(result.backgroundSize, "800% 900%");
  await page.close();
});

test("Windows mixed-DPI restore and hit testing use destination physical coordinates", async () => {
  const page = await browser.newPage({ viewport: { width: 420, height: 360 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const result = await page.evaluate(async () => {
    const { restoredAnchorForMonitor } = await import("/src/components/companion/CompanionWindow.tsx");
    const {
      createAsyncCleanupScope,
      createCursorEventPolicy,
      physicalCursorToLogicalPoint,
      restoreMainWindowBestEffort,
    } = await import("/src/components/companion/CompanionWindowController.tsx");
    const monitor = {
      name: "Left 150%",
      position: { x: -2560, y: 0 },
      size: { width: 2560, height: 1440 },
      workArea: {
        position: { x: -2560, y: 0 },
        size: { width: 2560, height: 1400 },
      },
      scaleFactor: 1.5,
    };
    const restored = restoredAnchorForMonitor({
      x: 100,
      y: 100,
      monitorName: monitor.name,
      relativeX: 1,
      relativeY: 1,
      scaleFactor: 1,
    }, { width: 168, height: 196 }, monitor);
    const point = physicalCursorToLogicalPoint(
      { x: -2252.5, y: 187.5 },
      { x: -2400, y: 75 },
      1.5,
    );
    const operations = [];
    const failures = [];
    await restoreMainWindowBestEffort({
      show: async () => { operations.push("show"); },
      unminimize: async () => {
        operations.push("unminimize");
        throw new Error("Windows denied the transition");
      },
      setFocus: async () => { operations.push("focus"); },
    }, (operation) => failures.push(operation));

    let releaseStaleEnable;
    let markStaleEnableStarted;
    let nativeIgnored = false;
    const cursorOperations = [];
    const staleEnableStarted = new Promise((resolve) => { markStaleEnableStarted = resolve; });
    const cursorPolicy = createCursorEventPolicy(async (ignored) => {
      cursorOperations.push(ignored);
      if (ignored) {
        markStaleEnableStarted();
        await new Promise((resolve) => { releaseStaleEnable = resolve; });
      }
      nativeIgnored = ignored;
    });
    const staleEnable = cursorPolicy.setIgnored(true);
    await staleEnableStarted;
    const disable = cursorPolicy.setIgnored(false);
    releaseStaleEnable();
    await Promise.all([staleEnable, disable]);

    // Shared-policy state tracking: redundant writes are skipped, failures keep the stale
    // applied value so the same request retries, and every caller observes one truth.
    const dedupOps = [];
    let failNextApply = false;
    const dedupPolicy = createCursorEventPolicy(async (ignored) => {
      if (failNextApply) {
        failNextApply = false;
        throw new Error("transient native failure");
      }
      dedupOps.push(ignored);
    });
    const appliedInitial = dedupPolicy.applied();
    await dedupPolicy.setIgnored(true);
    const appliedAfterEnable = dedupPolicy.applied();
    await dedupPolicy.setIgnored(true);
    const appliedAfterRedundant = dedupPolicy.applied();
    failNextApply = true;
    const failedWrite = await dedupPolicy.setIgnored(false)
      .then(() => "resolved")
      .catch(() => "rejected");
    const appliedAfterFailure = dedupPolicy.applied();
    await dedupPolicy.setIgnored(false);
    const appliedAfterRetry = dedupPolicy.applied();

    let releaseLateListener;
    let lateCleanupCalls = 0;
    const lateRegistration = new Promise((resolve) => {
      releaseLateListener = () => resolve(() => { lateCleanupCalls += 1; });
    });
    const listenerScope = createAsyncCleanupScope();
    const retainedLateListener = listenerScope.retain(lateRegistration);
    listenerScope.dispose();
    releaseLateListener();
    const lateListenerRetained = await retainedLateListener;

    return {
      restored,
      point,
      operations,
      failures,
      cursorOperations,
      nativeIgnored,
      appliedInitial,
      appliedAfterEnable,
      appliedAfterRedundant,
      failedWrite,
      appliedAfterFailure,
      appliedAfterRetry,
      dedupOps,
      lateCleanupCalls,
      lateListenerRetained,
    };
  });

  assert.deepEqual(result.restored, { x: -252, y: 1106 });
  assert.deepEqual(result.point, { x: 98.33333333333333, y: 75 });
  assert.deepEqual(result.operations, ["show", "unminimize", "focus"]);
  assert.deepEqual(result.failures, ["unminimize"]);
  assert.deepEqual(result.cursorOperations, [true, false]);
  assert.equal(result.nativeIgnored, false);
  assert.equal(result.appliedInitial, null);
  assert.equal(result.appliedAfterEnable, true);
  assert.equal(result.appliedAfterRedundant, true);
  assert.deepEqual(result.dedupOps, [true, false],
    "a redundant same-value write must not reach the native window again");
  assert.equal(result.failedWrite, "rejected");
  assert.equal(result.appliedAfterFailure, true,
    "a failed apply must leave the previous native state as the recorded truth");
  assert.equal(result.appliedAfterRetry, false);
  assert.equal(result.lateListenerRetained, false);
  assert.equal(result.lateCleanupCalls, 1);
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
  assert.equal(await dialog.locator(".companion-session-card:not(.is-exiting)").count(), 5,
    "the scrollable task panel keeps every non-archived session reachable");
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
  await page.waitForFunction(() => {
    const badge = document.querySelector(".companion-unread-badge")?.textContent;
    const trackerLabel = document.querySelector(".companion-pulse-stack")?.getAttribute("aria-label") ?? "";
    const trackingCount = trackerLabel.match(/共\s+(\d+)\s+个任务/)?.[1];
    return trackingCount && badge === trackingCount;
  });
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
  const page = await browser.newPage({ viewport: { width: 420, height: 520 } });
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
      "mock-task-permission",
    ]));
  });
  await page.goto(`${baseUrl}?window=companion`, { waitUntil: "networkidle" });
  const stack = page.getByRole("region", { name: "Session 进度提醒" });
  await stack.waitFor({ state: "visible" });
  assert.equal(await stack.locator(".companion-session-card").count(), 2,
    "two task reminders must render as two independently actionable tracker rows");
  assert.equal(await page.getByRole("dialog", { name: "最近任务" }).count(), 0,
    "automatic progress does not require opening the recent-session dialog");
  const live = stack.locator(".companion-session-card").filter({ hasText: "优化 Rust 编译性能" });
  await live.hover();
  assert.equal(await live.getByRole("button", { name: "继续跟进" }).count(), 1);
  assert.equal(await live.getByRole("button", { name: "停止当前运行" }).count(), 1);
  await page.waitForFunction(() => {
    const card = document.querySelector(".companion-pulse-stack .companion-session-card:hover");
    const actions = card?.querySelector(".companion-session-actions");
    const supporting = card?.querySelector(".companion-session-copy small");
    const title = card?.querySelector(".companion-session-copy strong")?.getBoundingClientRect();
    const actionButtons = [...(card?.querySelectorAll(".companion-session-actions button") ?? [])]
      .map((button) => button.getBoundingClientRect());
    const actionTop = Math.min(...actionButtons.map((rect) => rect.top));
    return actions && supporting
      && getComputedStyle(actions).opacity === "1"
      && getComputedStyle(supporting).opacity === "0"
      && title && Number.isFinite(actionTop) && title.bottom + 4 <= actionTop;
  });
  const geometry = await stack.evaluate((element) => {
    const rect = element.getBoundingClientRect();
    const avatar = document.querySelector(".companion-avatar")?.getBoundingClientRect();
    const toggle = document.querySelector(".companion-tracking-toggle")?.getBoundingClientRect();
    const hoveredCard = element.querySelector(".companion-session-card:hover");
    const hoveredRow = hoveredCard?.querySelector(".companion-session-row");
    const unreadMarker = hoveredRow?.querySelector(":scope > i")?.getBoundingClientRect();
    const hoverActions = [...(hoveredCard?.querySelectorAll(".companion-session-actions button") ?? [])];
    const hoverAction = hoverActions.at(-1)?.getBoundingClientRect();
    const actionTop = Math.min(...hoverActions.map((button) => button.getBoundingClientRect().top));
    const title = hoveredCard?.querySelector(".companion-session-copy strong")?.getBoundingClientRect();
    const firstRow = element.querySelector(".companion-session-row")?.getBoundingClientRect();
    const background = hoveredRow ? getComputedStyle(hoveredRow).backgroundColor : "";
    const alphaMatch = background.match(/rgba\([^)]*,\s*([\d.]+)\)$/);
    return {
      stack: { top: rect.top, right: rect.right, bottom: rect.bottom, left: rect.left, width: rect.width },
      avatar: avatar && { top: avatar.top, right: avatar.right, bottom: avatar.bottom, left: avatar.left },
      toggle: toggle && { top: toggle.top, right: toggle.right, bottom: toggle.bottom, left: toggle.left },
      row: firstRow && { width: firstRow.width, height: firstRow.height },
      unreadMarker: unreadMarker && { top: unreadMarker.top, right: unreadMarker.right, bottom: unreadMarker.bottom, left: unreadMarker.left },
      hoverAction: hoverAction && { top: hoverAction.top, right: hoverAction.right, bottom: hoverAction.bottom, left: hoverAction.left },
      title: title && { top: title.top, right: title.right, bottom: title.bottom, left: title.left },
      actionTop: Number.isFinite(actionTop) ? actionTop : null,
      hoverBackground: background,
      hoverBackgroundAlpha: alphaMatch ? Number(alphaMatch[1]) : 1,
    };
  });
  assert.ok(geometry.stack.top >= 0 && geometry.stack.left >= 0
    && geometry.stack.right <= 420 && geometry.stack.bottom <= 520,
  `automatic pulse stack overflowed: ${JSON.stringify(geometry.stack)}`);
  assert.ok(geometry.avatar, "the assistant must remain visible beside automatic progress");
  assert.ok(geometry.toggle, "automatic progress exposes a foot-level collapse control");
  assert.ok(geometry.stack.bottom <= geometry.avatar.top,
    `automatic progress must stay above the assistant: ${JSON.stringify(geometry)}`);
  assert.ok(geometry.avatar.top - geometry.stack.bottom >= 12
    && geometry.avatar.top - geometry.stack.bottom <= 22,
    `automatic progress drifted too far from the assistant's head: ${JSON.stringify(geometry)}`);
  assert.ok(geometry.toggle.top >= geometry.avatar.bottom
    && geometry.toggle.top - geometry.avatar.bottom <= 8,
  `the collapse control must sit directly below the assistant's feet: ${JSON.stringify(geometry)}`);
  assert.ok(geometry.stack.width <= 320.5,
    `automatic progress should remain a compact head-anchored card: ${JSON.stringify(geometry.stack)}`);
  assert.ok(geometry.hoverBackgroundAlpha >= 0.96,
    `hover must preserve an opaque readable surface: ${JSON.stringify(geometry)}`);
  assert.ok(geometry.unreadMarker && geometry.hoverAction,
    `hovered unread reminders must expose both the unread marker and actions: ${JSON.stringify(geometry)}`);
  assert.ok(geometry.hoverAction.right + 4 <= geometry.unreadMarker.left,
    `hover actions must reserve a visible trailing lane for the unread marker: ${JSON.stringify(geometry)}`);
  assert.ok(geometry.title && geometry.actionTop !== null && geometry.title.bottom + 4 <= geometry.actionTop,
    `hover actions must occupy a separate lower lane below the session title: ${JSON.stringify(geometry)}`);
  if (process.env.R_CODE_COMPANION_STACK_SHOT) {
    await page.screenshot({ path: process.env.R_CODE_COMPANION_STACK_SHOT });
  }

  const collapse = page.getByRole("button", { name: "收起 Session 追踪" });
  await collapse.click();
  await stack.waitFor({ state: "detached" });
  const runningCount = page.getByRole("button", { name: /个任务正在运行，展开 Session 追踪/ });
  await runningCount.waitFor({ state: "visible" });
  assert.match(await runningCount.innerText(), /^(?:[1-9]|9\+)$/,
    "collapsed tracking shows the number of currently running tasks");
  const collapsedAvatar = await page.locator(".companion-avatar").evaluate((element) => {
    const rect = element.getBoundingClientRect();
    return { top: rect.top, right: rect.right, bottom: rect.bottom, left: rect.left };
  });
  assert.deepEqual(collapsedAvatar, geometry.avatar,
    "collapsing tracking must not move the assistant");
  await runningCount.click();
  await stack.waitFor({ state: "visible" });
  const reopenedAvatar = await page.locator(".companion-avatar").evaluate((element) => {
    const rect = element.getBoundingClientRect();
    return { top: rect.top, right: rect.right, bottom: rect.bottom, left: rect.left };
  });
  assert.deepEqual(reopenedAvatar, geometry.avatar,
    "reopening tracking must not move the assistant");

  await page.evaluate(async () => {
    const { useCompanionStore } = await import("/src/store/companion.ts");
    useCompanionStore.getState().setMinimized(true);
  });
  await page.locator(".companion-window-root.is-mini").waitFor({ state: "visible" });
  await page.waitForFunction(() => document.querySelector(".companion-pulse-stack")?.getBoundingClientRect().width <= 264.5);
  await live.hover();
  await page.waitForFunction(() => {
    const card = document.querySelector(".companion-pulse-stack .companion-session-card:hover");
    const actions = card?.querySelector(".companion-session-actions");
    const supporting = card?.querySelector(".companion-session-copy small");
    const title = card?.querySelector(".companion-session-copy strong")?.getBoundingClientRect();
    const actionButtons = [...(card?.querySelectorAll(".companion-session-actions button") ?? [])]
      .map((button) => button.getBoundingClientRect());
    const actionTop = Math.min(...actionButtons.map((rect) => rect.top));
    return actions && supporting
      && getComputedStyle(actions).opacity === "1"
      && getComputedStyle(supporting).opacity === "0"
      && title && Number.isFinite(actionTop) && title.bottom + 4 <= actionTop;
  });
  const miniGeometry = await stack.evaluate((element) => {
    const stackRect = element.getBoundingClientRect();
    const row = element.querySelector(".companion-session-row");
    const rowRect = row?.getBoundingClientRect();
    const avatarRect = document.querySelector(".companion-avatar")?.getBoundingClientRect();
    const hoveredCard = element.querySelector(".companion-session-card:hover");
    const unreadMarker = hoveredCard?.querySelector(".companion-session-row > i")?.getBoundingClientRect();
    const hoverActions = [...(hoveredCard?.querySelectorAll(".companion-session-actions button") ?? [])];
    const hoverAction = hoverActions.at(-1)?.getBoundingClientRect();
    const actionTop = Math.min(...hoverActions.map((button) => button.getBoundingClientRect().top));
    const title = hoveredCard?.querySelector(".companion-session-copy strong")?.getBoundingClientRect();
    return {
      stack: { top: stackRect.top, right: stackRect.right, bottom: stackRect.bottom, left: stackRect.left, width: stackRect.width },
      row: rowRect && { width: rowRect.width, height: rowRect.height },
      avatar: avatarRect && { top: avatarRect.top, right: avatarRect.right, bottom: avatarRect.bottom, left: avatarRect.left },
      unreadMarker: unreadMarker && { right: unreadMarker.right, left: unreadMarker.left },
      hoverAction: hoverAction && { right: hoverAction.right, left: hoverAction.left },
      title: title && { top: title.top, bottom: title.bottom },
      actionTop: Number.isFinite(actionTop) ? actionTop : null,
      titleFontSize: row ? getComputedStyle(row.querySelector(".companion-session-copy strong")).fontSize : "",
    };
  });
  assert.ok(geometry.row && miniGeometry.row && miniGeometry.avatar,
    `mini progress geometry must remain measurable: ${JSON.stringify({ geometry, miniGeometry })}`);
  assert.ok(miniGeometry.stack.width <= geometry.stack.width - 40,
    `mini appearance should use a visibly narrower progress stack: ${JSON.stringify({ geometry, miniGeometry })}`);
  assert.ok(miniGeometry.row.height <= geometry.row.height - 10,
    `mini appearance should use shorter progress rows: ${JSON.stringify({ geometry, miniGeometry })}`);
  assert.equal(miniGeometry.titleFontSize, "12px");
  assert.ok(miniGeometry.stack.bottom <= miniGeometry.avatar.top,
    `mini progress must remain above the assistant: ${JSON.stringify(miniGeometry)}`);
  assert.ok(miniGeometry.avatar.top - miniGeometry.stack.bottom >= 12
    && miniGeometry.avatar.top - miniGeometry.stack.bottom <= 22,
    `mini progress must remain close to the assistant's head: ${JSON.stringify(miniGeometry)}`);
  assert.ok(miniGeometry.unreadMarker && miniGeometry.hoverAction
    && miniGeometry.hoverAction.right + 4 <= miniGeometry.unreadMarker.left,
  `mini hover actions must not cover the unread marker: ${JSON.stringify(miniGeometry)}`);
  assert.ok(miniGeometry.title && miniGeometry.actionTop !== null
    && miniGeometry.title.bottom + 4 <= miniGeometry.actionTop,
  `mini hover actions must remain below the session title: ${JSON.stringify(miniGeometry)}`);
  if (process.env.R_CODE_COMPANION_MINI_STACK_SHOT) {
    await page.screenshot({ path: process.env.R_CODE_COMPANION_MINI_STACK_SHOT });
  }

  await page.evaluate(async () => {
    const { useCompanionStore } = await import("/src/store/companion.ts");
    useCompanionStore.getState().setMinimized(false);
  });
  await page.close();
});

test("progress overflow expands inline, collapses again and stays synchronized as tasks change", async () => {
  const page = await browser.newPage({ viewport: { width: 420, height: 520 } });
  await page.addInitScript(() => {
    localStorage.setItem("r-code.companion.preferences.v2", JSON.stringify({
      revision: 1,
      enabled: true,
      minimized: false,
      soundEnabled: false,
      motion: "reduced",
    }));
    localStorage.setItem("r-code.companion.unread-sessions.v1", JSON.stringify([
      "mock-task-review",
      "mock-task-permission",
    ]));
  });
  await page.goto(`${baseUrl}?window=companion`, { waitUntil: "networkidle" });

  const stack = page.getByRole("region", { name: /Session 进度提醒，共 \d+ 个任务/ });
  const cards = stack.locator(".companion-session-card");
  const overflow = stack.locator(".companion-pulse-more");
  await overflow.waitFor({ state: "visible" });
  const compactCount = await cards.count();
  const overflowText = await overflow.innerText();
  const hiddenCount = Number(overflowText.match(/还有\s+(\d+)\s+个任务/)?.[1] ?? 0);
  const trackingCount = compactCount + hiddenCount;
  assert.equal(compactCount, 2, "the compact tracker keeps its two-row scan target");
  assert.ok(hiddenCount > 0, "the fixture must exercise a real overflow");
  assert.equal(await overflow.getAttribute("aria-expanded"), "false");
  assert.equal(await overflow.getAttribute("aria-controls"), "companion-pulse-list");
  await page.waitForFunction((count) => (
    document.querySelector(".companion-unread-badge")?.textContent === String(count)
  ), trackingCount);

  await overflow.click();
  await page.waitForFunction((count) => (
    document.querySelectorAll(".companion-pulse-stack .companion-session-card").length === count
  ), trackingCount);
  assert.equal(await overflow.getAttribute("aria-expanded"), "true");
  assert.equal(await overflow.innerText(), `全部 ${trackingCount} 个任务 · 收起`);
  assert.equal(await page.getByRole("dialog", { name: "最近任务" }).count(), 0,
    "view all expands the tracker in place instead of replacing it with another panel");
  assert.equal(await page.evaluate(() => document.activeElement?.classList.contains("companion-pulse-more")), true,
    "the persistent toggle keeps keyboard focus after expansion");
  const scrollReach = await stack.locator("#companion-pulse-list").evaluate((list) => {
    list.scrollTop = list.scrollHeight;
    const listRect = list.getBoundingClientRect();
    const lastRect = list.lastElementChild?.getBoundingClientRect();
    return {
      overflow: list.scrollHeight - list.clientHeight,
      lastBottom: lastRect?.bottom ?? Infinity,
      viewportBottom: listRect.bottom,
    };
  });
  assert.ok(scrollReach.overflow > 0 && scrollReach.lastBottom <= scrollReach.viewportBottom + 1,
    `every expanded task must be reachable inside the bounded tracker: ${JSON.stringify(scrollReach)}`);

  await overflow.click();
  await page.waitForFunction((count) => (
    document.querySelectorAll(".companion-pulse-stack .companion-session-card").length === count
  ), compactCount);
  assert.equal(await overflow.getAttribute("aria-expanded"), "false");
  assert.match(await overflow.innerText(), new RegExp(`还有 ${hiddenCount} 个任务 · 查看全部`));

  await overflow.click();
  await page.waitForFunction((count) => (
    document.querySelectorAll(".companion-pulse-stack .companion-session-card").length === count
  ), trackingCount);

  const archiveTask = async (taskId) => {
    await page.evaluate(async (id) => {
      const { browserMockTasks, browserMockDetails } = await import("/src/lib/mock-data.ts");
      const { useTasksStore } = await import("/src/store/tasks.ts");
      const updatedAt = new Date().toISOString();
      const mockTask = browserMockTasks.find((task) => task.id === id);
      if (mockTask) {
        mockTask.state = "archived";
        mockTask.updated_at = updatedAt;
      }
      const mockDetail = browserMockDetails[id];
      if (mockDetail) {
        mockDetail.task = { ...mockDetail.task, state: "archived", updated_at: updatedAt };
        mockDetail.runs = mockDetail.runs.map((run) => ({
          ...run,
          ended_at: run.ended_at ?? updatedAt,
        }));
      }
      const state = useTasksStore.getState();
      const currentDetail = state.details[id];
      useTasksStore.setState({
        tasks: state.tasks.map((task) => task.id === id
          ? { ...task, state: "archived", updated_at: updatedAt }
          : task),
        details: currentDetail ? {
          ...state.details,
          [id]: {
            ...currentDetail,
            task: { ...currentDetail.task, state: "archived", updated_at: updatedAt },
            runs: currentDetail.runs.map((run) => ({
              ...run,
              ended_at: run.ended_at ?? updatedAt,
            })),
          },
        } : state.details,
      });
    }, taskId);
  };

  await archiveTask("mock-task-queue");
  await page.waitForFunction((count) => {
    const listCount = document.querySelectorAll(".companion-pulse-stack .companion-session-card").length;
    const badge = document.querySelector(".companion-unread-badge")?.textContent;
    return listCount === count && badge === String(count);
  }, trackingCount - 1);
  assert.equal(await overflow.innerText(), `全部 ${trackingCount - 1} 个任务 · 收起`,
    "the expanded footer mirrors the live list length after a task disappears");

  await archiveTask("mock-task-review");
  await page.waitForFunction((count) => {
    const listCount = document.querySelectorAll(".companion-pulse-stack .companion-session-card").length;
    const badge = document.querySelector(".companion-unread-badge")?.textContent;
    return listCount === count && badge === String(count)
      && !document.querySelector(".companion-pulse-more")
      && !document.querySelector(".companion-pulse-stack")?.classList.contains("is-showing-all");
  }, trackingCount - 2);
  assert.match(await page.getByRole("button", { name: /R-Code session 助手/ }).getAttribute("aria-label"),
    new RegExp(`${trackingCount - 2} 个任务正在追踪`));

  await page.close();
});

test("the full task panel keeps every unread session accessible and archived sessions cannot leave ghost badges", async () => {
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
  assert.equal(await dialog.getByRole("button", { name: /更新依赖并修复告警/ }).count(), 1,
    "the scrollable full panel must not silently hide the fifth unread session");
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
