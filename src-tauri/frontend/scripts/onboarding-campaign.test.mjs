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
  const candidates = [
    path.join(process.env.PROGRAMFILES ?? "", "Google", "Chrome", "Application", "chrome.exe"),
    path.join(process.env["PROGRAMFILES(X86)"] ?? "", "Microsoft", "Edge", "Application", "msedge.exe"),
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    "/usr/bin/google-chrome",
    "/usr/bin/chromium",
    "/usr/bin/chromium-browser",
  ];
  return candidates.find((candidate) => fs.existsSync(candidate));
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

test("completion never blocks on optional setup and only auto-opens once", async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  const runtimeErrors = [];
  page.on("pageerror", (error) => runtimeErrors.push(String(error)));
  page.on("console", (message) => {
    if (message.type() === "error") runtimeErrors.push(message.text());
  });

  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const mock = await import("/src/lib/mock-data.ts");
    mock.browserMockSettings.config.default_provider = undefined;
    mock.browserMockSettings.config.providers = {};
    mock.browserMockSettings.provider_status = {};
    window.localStorage.removeItem("r-code.onboarding.campaign.v1");
    window.dispatchEvent(new Event("r-code:onboarding:open"));
  });

  await page.locator(".onboarding-tour").waitFor();
  await page.locator(".onboarding-loading").waitFor({ state: "hidden" });
  await page.locator(".onboarding-dot").nth(4).click();
  await page.locator(".onboarding-footer > button").last().click();
  await page.locator(".onboarding-layer").waitFor({ state: "detached" });

  const firstRun = await page.evaluate(async () => {
    const onboarding = await import("/src/lib/onboarding.ts");
    const receipt = JSON.parse(window.localStorage.getItem("r-code.onboarding.campaign.v1") ?? "null");
    Object.defineProperty(window, "__TAURI_INTERNALS__", { value: {}, configurable: true });
    const opensAfterCompletion = onboarding.shouldOpenOnboarding();
    window.localStorage.removeItem("r-code.onboarding.campaign.v1");
    const opensWithoutReceipt = onboarding.shouldOpenOnboarding();
    onboarding.saveOnboardingReceipt("completed");
    Reflect.deleteProperty(window, "__TAURI_INTERNALS__");
    return { receipt, opensAfterCompletion, opensWithoutReceipt };
  });

  assert.equal(firstRun.receipt?.outcome, "completed");
  assert.equal(firstRun.opensAfterCompletion, false, "a completed tour must not auto-open again");
  assert.equal(firstRun.opensWithoutReceipt, true, "a real first run must auto-open the tour");

  await page.getByRole("button", { name: "帮助", exact: true }).click();
  await page.getByRole("menuitem", { name: "首次设置" }).click();
  await page.locator(".onboarding-tour").waitFor();
  assert.deepEqual(runtimeErrors, []);
  await page.close();
});
