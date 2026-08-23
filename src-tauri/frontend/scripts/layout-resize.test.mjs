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

test("desktop sidebar collapses, resizes, persists, and fits narrow viewports", async () => {
  const page = await browser.newPage({ viewport: { width: 1600, height: 960 } });
  await page.addInitScript(() => {
    if (localStorage.getItem("r-code.rail.collapsed") == null) localStorage.setItem("r-code.rail.collapsed", "0");
    if (localStorage.getItem("r-code.rail.width") == null) localStorage.setItem("r-code.rail.width", "300");
  });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const app = page.locator("#app");
  const sidebar = page.locator(".app-sidebar");
  const toggle = page.locator(".desktop-sidebar-toggle");
  const separator = page.getByRole("separator", { name: "调整左侧边栏宽度" });

  assert.ok(Math.abs((await sidebar.boundingBox()).width - 300) < 2);
  await toggle.click();
  await assert.doesNotReject(() => app.waitFor({ state: "visible" }));
  assert.equal(await app.evaluate((element) => element.classList.contains("rail-is-collapsed")), true);
  assert.ok((await sidebar.boundingBox()).width <= 65);

  await toggle.click();
  assert.equal(await app.evaluate((element) => element.classList.contains("rail-is-collapsed")), false);
  assert.ok(Math.abs((await sidebar.boundingBox()).width - 300) < 2);

  const initialHandle = await separator.boundingBox();
  await page.mouse.move(initialHandle.x + initialHandle.width / 2, initialHandle.y + 100);
  await page.mouse.down();
  await page.mouse.move(388, initialHandle.y + 100, { steps: 4 });
  await page.mouse.up();
  assert.ok(Math.abs((await sidebar.boundingBox()).width - 388) < 3);
  assert.equal(await page.evaluate(() => localStorage.getItem("r-code.rail.width")), "388");

  await page.reload({ waitUntil: "networkidle" });
  assert.ok(Math.abs((await sidebar.boundingBox()).width - 388) < 3, "saved width must survive reload");
  await separator.focus();
  await separator.press("ArrowLeft");
  assert.ok(Math.abs((await sidebar.boundingBox()).width - 380) < 3, "keyboard resize should use an 8px step");

  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().setRailWidth(520);
  });
  await page.setViewportSize({ width: 860, height: 960 });
  assert.ok(
    Math.abs((await sidebar.boundingBox()).width - 440) < 3,
    "a narrow window should temporarily preserve at least 420px for the workspace",
  );
  assert.equal(
    await page.evaluate(() => localStorage.getItem("r-code.rail.width")),
    "520",
    "temporary viewport fitting must not overwrite the saved preference",
  );
  await page.setViewportSize({ width: 1600, height: 960 });
  assert.ok(
    Math.abs((await sidebar.boundingBox()).width - 520) < 3,
    "expanding the window should restore the saved sidebar width",
  );

  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().setRailWidth(380);
  });
  const widthBeforeDrag = Number(await page.evaluate(() => localStorage.getItem("r-code.rail.width")));
  const resizedHandle = await separator.boundingBox();
  await page.mouse.move(resizedHandle.x + resizedHandle.width / 2, resizedHandle.y + 80);
  await page.mouse.down();
  await page.mouse.move(resizedHandle.x + resizedHandle.width / 2 + 60, resizedHandle.y + 80, { steps: 4 });
  await page.mouse.up();
  const widthAfterDrag = Number(await page.evaluate(() => localStorage.getItem("r-code.rail.width")));
  assert.ok(
    widthAfterDrag >= widthBeforeDrag + 58 && widthAfterDrag <= widthBeforeDrag + 62,
    `60 rendered pixels should resize the sidebar by 60 CSS pixels: ${widthBeforeDrag} -> ${widthAfterDrag}`,
  );

  await page.close();
});

test("project conversation keeps every control while using a flat workspace hierarchy", async () => {
  const page = await browser.newPage({ viewport: { width: 1600, height: 960 } });
  const runtimeErrors = [];
  page.on("pageerror", (error) => runtimeErrors.push(String(error)));
  page.on("console", (message) => {
    if (message.type() === "error") runtimeErrors.push(message.text());
  });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().openRoom("mock-task-queue");
  });

  const room = page.locator("#main-content > .scene-room");
  await room.waitFor({ state: "visible" });
  await page.getByRole("textbox", { name: "给 Agent 的消息" }).waitFor({ state: "visible" });

  const visualContract = await page.evaluate(() => {
    const composer = document.querySelector(".composer");
    const box = document.querySelector(".scene-room .comp-box");
    const activity = document.querySelector(".activity-strip");
    const userMessage = document.querySelector(".you");
    const titleMarker = document.querySelector(".room-conversation-title");
    if (!(composer instanceof HTMLElement) || !(box instanceof HTMLElement)
      || !(userMessage instanceof HTMLElement) || !(titleMarker instanceof HTMLElement)) {
      throw new Error("room visual contract elements are missing");
    }
    const boxStyle = getComputedStyle(box);
    return {
      boxBorder: [boxStyle.borderTopWidth, boxStyle.borderRightWidth, boxStyle.borderBottomWidth, boxStyle.borderLeftWidth],
      boxRadius: boxStyle.borderRadius,
      boxShadow: boxStyle.boxShadow,
      composerBackground: getComputedStyle(composer).backgroundColor,
      activityPresent: activity instanceof HTMLElement,
      userShadow: getComputedStyle(userMessage).boxShadow,
      titleMarkerDisplay: getComputedStyle(titleMarker, "::before").display,
    };
  });

  // 居中悬浮对话框：限定宽度的浮层卡（1px 描边 + 16px 圆角 + 投影），不再通栏贴底。
  assert.deepEqual(visualContract.boxBorder, ["1px", "1px", "1px", "1px"]);
  assert.equal(visualContract.boxRadius, "16px");
  assert.notEqual(visualContract.boxShadow, "none", "the floating composer card must cast a shadow");
  assert.equal(visualContract.activityPresent, false, "the composer should not repeat tool activity above the input");
  assert.equal(visualContract.userShadow, "none");
  assert.equal(visualContract.titleMarkerDisplay, "none");
  await page.locator(".agent-engine-pill").waitFor({ state: "visible" });
  await page.locator(".model-config-trigger").waitFor({ state: "visible" });
  await page.getByRole("button", { name: /选择发送方式/ }).waitFor({ state: "visible" });

  if (process.env.R_CODE_LAYOUT_SHOT) {
    await page.screenshot({ path: process.env.R_CODE_LAYOUT_SHOT, fullPage: true });
  }
  assert.deepEqual(runtimeErrors, []);
  await page.close();
});
