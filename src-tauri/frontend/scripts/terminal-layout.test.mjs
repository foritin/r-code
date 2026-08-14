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
const storageKey = "r-code.terminal.sidebar-collapsed";

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

async function openTerminal(page, { rawSnapshotDelayMs = 0, failFirstResize = false } = {}) {
  await page.evaluate(async ({ rawSnapshotDelayMs, failFirstResize }) => {
    const { terminalCreate } = await import("/src/lib/ipc.ts");
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    globalThis.__rCodeTerminalResizeCalls = [];
    globalThis.__rCodeTerminalSnapshotStartedAt = null;
    if (rawSnapshotDelayMs > 0) {
      globalThis.__rCodeBrowserMockDelayMs = { cmd_terminal_raw_snapshot: rawSnapshotDelayMs };
    }
    if (failFirstResize) {
      globalThis.__rCodeBrowserMockFailures = { cmd_terminal_resize: "transient resize failure" };
    }
    globalThis.__rCodePerformanceIpcProbe = (command, args) => {
      if (command === "cmd_terminal_raw_snapshot") {
        globalThis.__rCodeTerminalSnapshotStartedAt = performance.now();
      }
      if (command === "cmd_terminal_resize") {
        globalThis.__rCodeTerminalResizeCalls.push({ ...args, at: performance.now() });
        if (failFirstResize && globalThis.__rCodeTerminalResizeCalls.length === 1) {
          // browserMockInvoke reads the forced failure synchronously after this hook.
          // Removing it in a microtask makes only the first call fail.
          queueMicrotask(() => {
            delete globalThis.__rCodeBrowserMockFailures?.cmd_terminal_resize;
          });
        }
      }
    };
    const taskId = "mock-task-review";
    await terminalCreate(taskId, "pwsh.exe");
    await useTasksStore.getState().refreshDetail(taskId);
    useAppStore.getState().openRoom(taskId, "terminal");
  }, { rawSnapshotDelayMs, failFirstResize });
  await page.locator(".term-viewport .xterm").waitFor({ state: "visible" });
  await page.waitForFunction(() => globalThis.__rCodeTerminalResizeCalls?.length > 0);
}

async function clearTerminalProbes(page) {
  await page.evaluate(() => {
    delete globalThis.__rCodePerformanceIpcProbe;
    delete globalThis.__rCodeTerminalResizeCalls;
    delete globalThis.__rCodeTerminalSnapshotStartedAt;
    delete globalThis.__rCodeBrowserMockDelayMs;
    delete globalThis.__rCodeBrowserMockFailures;
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

test("terminal list collapses into a quiet rail, refits the PTY, and remembers the choice", async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate((key) => localStorage.removeItem(key), storageKey);
  await openTerminal(page);

  const toggle = page.getByRole("button", { name: "收起终端列表" });
  const expanded = await page.locator(".term-wrap").evaluate((element) => {
    const side = element.querySelector(".term-side").getBoundingClientRect();
    const main = element.querySelector(".term-main").getBoundingClientRect();
    const calls = globalThis.__rCodeTerminalResizeCalls ?? [];
    return { sideWidth: side.width, mainWidth: main.width, cols: calls.at(-1)?.cols ?? 0 };
  });
  assert.ok(expanded.sideWidth >= 120, `expanded terminal list was only ${expanded.sideWidth}px wide`);
  assert.ok(expanded.cols > 0, "xterm should report its initial PTY columns");
  if (process.env.R_CODE_TERMINAL_LAYOUT_EXPANDED_SHOT) {
    await page.screenshot({ path: process.env.R_CODE_TERMINAL_LAYOUT_EXPANDED_SHOT, fullPage: true });
  }

  await toggle.click();
  await page.locator('.term-wrap[data-terminal-sidebar="collapsed"]').waitFor({ state: "visible" });
  await page.waitForFunction((previousCols) => {
    const calls = globalThis.__rCodeTerminalResizeCalls ?? [];
    return calls.some((call) => call.cols > previousCols);
  }, expanded.cols);

  const collapsed = await page.locator(".term-wrap").evaluate((element, key) => {
    const side = element.querySelector(".term-side").getBoundingClientRect();
    const main = element.querySelector(".term-main").getBoundingClientRect();
    const button = element.querySelector(".term-side-toggle");
    const calls = globalThis.__rCodeTerminalResizeCalls ?? [];
    return {
      sideWidth: side.width,
      mainWidth: main.width,
      cols: calls.at(-1)?.cols ?? 0,
      expanded: button.getAttribute("aria-expanded"),
      stored: localStorage.getItem(key),
    };
  }, storageKey);
  assert.ok(collapsed.sideWidth <= 36, `collapsed rail still occupied ${collapsed.sideWidth}px`);
  assert.ok(collapsed.mainWidth >= expanded.mainWidth + 80, "terminal should reclaim the list width");
  assert.ok(collapsed.cols > expanded.cols, "PTY columns should grow after the list collapses");
  assert.equal(collapsed.expanded, "false");
  assert.equal(collapsed.stored, "true");

  if (process.env.R_CODE_TERMINAL_LAYOUT_SHOT) {
    await page.screenshot({ path: process.env.R_CODE_TERMINAL_LAYOUT_SHOT, fullPage: true });
  }

  await page.reload({ waitUntil: "networkidle" });
  await openTerminal(page);
  await page.locator('.term-wrap[data-terminal-sidebar="collapsed"]').waitFor({ state: "visible" });
  const restore = page.getByRole("button", { name: "展开终端列表" });
  assert.equal(await restore.getAttribute("aria-expanded"), "false");
  await restore.click();
  await page.locator('.term-wrap[data-terminal-sidebar="expanded"]').waitFor({ state: "visible" });
  assert.equal(await page.evaluate((key) => localStorage.getItem(key), storageKey), "false");

  await clearTerminalProbes(page);
  await page.close();
});

test("terminal sizes the PTY before a slow snapshot and retries a transient resize failure", async () => {
  const page = await browser.newPage({ viewport: { width: 1280, height: 840 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate((key) => localStorage.removeItem(key), storageKey);
  await openTerminal(page, { rawSnapshotDelayMs: 900, failFirstResize: true });

  await page.waitForFunction(() => globalThis.__rCodeTerminalResizeCalls?.length >= 2);
  const result = await page.evaluate(() => ({
    snapshotStartedAt: globalThis.__rCodeTerminalSnapshotStartedAt,
    calls: [...(globalThis.__rCodeTerminalResizeCalls ?? [])],
    error: document.querySelector(".term-main .panel-error")?.textContent ?? null,
  }));
  assert.ok(result.snapshotStartedAt != null, "the delayed terminal snapshot should have started");
  assert.ok(
    result.calls[0].at - result.snapshotStartedAt < 500,
    "the initial PTY size must not wait for the delayed terminal snapshot",
  );
  assert.deepEqual(
    { cols: result.calls[1].cols, rows: result.calls[1].rows },
    { cols: result.calls[0].cols, rows: result.calls[0].rows },
    "a failed resize should retry the same desired dimensions",
  );
  assert.equal(result.error, null, "a recovered resize transport failure should not become a persistent panel error");

  await clearTerminalProbes(page);
  await page.close();
});

test("terminal resize stays single-flight and coalesces rapid width changes", async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate((key) => localStorage.removeItem(key), storageKey);
  await openTerminal(page);
  await page.evaluate(() => {
    globalThis.__rCodeTerminalResizeCalls = [];
    globalThis.__rCodeBrowserMockDelayMs = { cmd_terminal_resize: 500 };
  });

  // Start one delayed resize, then change width twice while that IPC is in flight.
  await page.setViewportSize({ width: 1180, height: 900 });
  await page.waitForFunction(() => (globalThis.__rCodeTerminalResizeCalls?.length ?? 0) === 1);
  await page.setViewportSize({ width: 920, height: 900 });
  await page.setViewportSize({ width: 760, height: 900 });
  await page.waitForTimeout(100);
  assert.equal(
    await page.evaluate(() => globalThis.__rCodeTerminalResizeCalls?.length ?? 0),
    1,
    "rapid measurements must not launch concurrent PTY resize IPCs",
  );

  await page.waitForFunction(() => (globalThis.__rCodeTerminalResizeCalls?.length ?? 0) >= 2);
  await page.waitForTimeout(560);
  const result = await page.locator(".term-wrap").evaluate((element) => {
    const calls = globalThis.__rCodeTerminalResizeCalls ?? [];
    return {
      calls,
      mainWidth: element.querySelector(".term-main").getBoundingClientRect().width,
    };
  });
  assert.equal(result.calls.length, 2, "intermediate widths should be coalesced into the latest resize");
  assert.notDeepEqual(
    { cols: result.calls[1].cols, rows: result.calls[1].rows },
    { cols: result.calls[0].cols, rows: result.calls[0].rows },
    "the latest measured dimensions must replace the in-flight size",
  );
  await page.waitForTimeout(560);
  assert.equal(
    await page.evaluate(() => globalThis.__rCodeTerminalResizeCalls?.length ?? 0),
    2,
    "the final fitted size should settle without replaying stale intermediate widths",
  );
  assert.ok(result.mainWidth > 0, "the terminal must retain a usable viewport at the narrow boundary");

  await clearTerminalProbes(page);
  await page.close();
});
