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

test("running send strategy stays beside Send, cycles, and controls the transmitted mode", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().setZoom(100);
    useAppStore.getState().openRoom("mock-task-queue");
  });

  const composer = page.getByRole("textbox", { name: "给 Agent 的消息" });
  const actions = page.getByLabel("运行中消息操作");
  const modeSwitch = actions.locator(".run-send-mode-label");
  const directSelect = actions.locator(".run-send-mode-trigger");
  const send = actions.locator(".running-send-button");
  const stop = page.getByRole("button", { name: "停止当前运行", exact: true });
  await modeSwitch.waitFor({ state: "visible" });
  await stop.waitFor({ state: "visible" });
  assert.match(await stop.getAttribute("class"), /composer-primary-button/, "Stop should replace the primary Send button while the draft is empty");
  const stopButtonBox = await stop.boundingBox();
  assert.ok(stopButtonBox && stopButtonBox.width >= 40 && stopButtonBox.height >= 40, "Stop needs the same substantial target as Send");
  assert.equal(await stop.locator("svg").count(), 1, "running controls should expose a square Stop icon");
  const stopIconBox = await stop.locator("svg").boundingBox();
  const stopGlyphBox = await stop.locator("svg rect").boundingBox();
  assert.ok(stopIconBox && stopIconBox.width >= 19, "Stop icon should have enough visual weight inside the primary circle");
  assert.ok(stopGlyphBox && stopGlyphBox.width >= 12, "Stop square must read clearly inside the primary circle");
  const stopCopyBox = await stop.locator(".sr-only").boundingBox();
  assert.ok(!stopCopyBox || stopCopyBox.width <= 1, "Stop copy should stay accessible without a text button");

  await composer.fill("准备补充当前运行");
  await send.waitFor({ state: "visible" });

  const switchBox = await modeSwitch.boundingBox();
  const sendBox = await send.boundingBox();
  assert.ok(switchBox && sendBox, "mode and send controls must have measurable positions");
  assert.ok(sendBox.x > switchBox.x + switchBox.width, "the strategy label must sit immediately before Send");
  assert.ok(sendBox.x - (switchBox.x + switchBox.width) < 40, "the direct-select affordance must stay compact");

  const observedColors = [];
  for (const label of ["排队", "引导", "立即发送"]) {
    await page.waitForFunction(
      ({ selector, expected }) => document.querySelector(selector)?.textContent?.includes(expected),
      { selector: ".run-send-mode-label", expected: label },
    );
    observedColors.push(await modeSwitch.evaluate((element) => getComputedStyle(element).color));
    await modeSwitch.click();
  }
  assert.equal(new Set(observedColors).size, 3, "each strategy needs a distinct semantic text color");
  assert.match(await modeSwitch.innerText(), /排队/);

  await modeSwitch.focus();
  await modeSwitch.press("Enter");
  assert.match(await modeSwitch.innerText(), /引导/, "keyboard activation must cycle the strategy too");
  await modeSwitch.press("Space");
  assert.match(await modeSwitch.innerText(), /立即发送/);
  await modeSwitch.click();
  assert.match(await modeSwitch.innerText(), /排队/);

  await directSelect.click();
  const menu = page.getByRole("menu", { name: "选择发送方式" });
  await menu.waitFor({ state: "visible" });
  assert.equal(await menu.getByRole("menuitemradio").count(), 3, "direct selection must retain all three strategies");
  await page.keyboard.press("Escape");

  await modeSwitch.click();
  assert.match(await modeSwitch.innerText(), /引导/);
  if (process.env.R_CODE_SEND_MODE_SHOT) {
    await page.screenshot({ path: process.env.R_CODE_SEND_MODE_SHOT, fullPage: true });
  }
  await page.evaluate(async () => {
    const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
    globalThis.__rCodeObservedSendModes = [];
    globalThis.__TAURI_INTERNALS__ = {
      invoke: async (command, args = {}) => {
        if (command === "cmd_agent_send") globalThis.__rCodeObservedSendModes.push(args.mode);
        return browserMockInvoke(command, args);
      },
    };
  });
  await composer.fill("补充当前运行的验收条件");
  await send.click();
  await page.waitForFunction(() => globalThis.__rCodeObservedSendModes?.length === 1);
  assert.deepEqual(await page.evaluate(() => globalThis.__rCodeObservedSendModes), ["steer"]);
  await page.close();
});

test("the same send strategy stays on Home and idle follow-ups while idle sends directly", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.addInitScript(() => window.localStorage.removeItem("r-code:agent-send-mode"));
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().setScene("home");
  });

  const homeComposer = page.getByRole("textbox", { name: "描述新任务" });
  await homeComposer.waitFor({ state: "visible" });
  const homeModeSwitch = page.locator(".scene-home .run-send-mode-label");
  assert.equal(await homeModeSwitch.count(), 1, "Home must expose the same cycling send strategy as an active task");
  assert.equal(
    await page.locator(".scene-home .send-hint").count(),
    0,
    "normal Home composition must not fall back to the old static Enter/Shift+Enter hint",
  );
  assert.match(await homeModeSwitch.innerText(), /排队/);
  await homeModeSwitch.click();
  assert.match(await homeModeSwitch.innerText(), /引导/);
  if (process.env.R_CODE_SEND_MODE_HOME_SHOT) {
    await page.screenshot({ path: process.env.R_CODE_SEND_MODE_HOME_SHOT, fullPage: true });
  }

  await page.evaluate(async () => {
    const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
    globalThis.__rCodeObservedSendModes = [];
    globalThis.__TAURI_INTERNALS__ = {
      invoke: async (command, args = {}) => {
        if (command === "cmd_agent_send") globalThis.__rCodeObservedSendModes.push(args.mode);
        return browserMockInvoke(command, args);
      },
    };
  });

  await homeComposer.fill("从新对话验证统一发送策略");
  await page.locator(".scene-home .composer-primary-button").click();
  await page.waitForFunction(() => globalThis.__rCodeObservedSendModes?.length === 1);
  assert.deepEqual(
    await page.evaluate(() => globalThis.__rCodeObservedSendModes),
    ["auto"],
    "an idle Home composer must start immediately regardless of the retained strategy",
  );

  await page.locator("#main-content > .scene-room").waitFor({ state: "visible" });
  const roomComposer = page.getByRole("textbox", { name: "给 Agent 的消息" });
  const roomModeSwitch = page.locator(".scene-room .run-send-mode-label");
  await roomModeSwitch.waitFor({ state: "visible" });
  assert.match(await roomModeSwitch.innerText(), /引导/, "the selected strategy should follow the user into the task");

  await roomComposer.fill("回答结束后继续追问");
  await page.locator(".scene-room .composer-primary-button").click();
  await page.waitForFunction(() => globalThis.__rCodeObservedSendModes?.length === 2);
  assert.deepEqual(
    await page.evaluate(() => globalThis.__rCodeObservedSendModes),
    ["auto", "auto"],
    "an idle follow-up must send immediately instead of queueing or trying to steer",
  );

  await page.close();
});
