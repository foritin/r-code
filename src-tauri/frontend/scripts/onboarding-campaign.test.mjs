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

test("an empty provider form applies the complete first preset and keeps the primary flow compact", async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const mock = await import("/src/lib/mock-data.ts");
    mock.browserMockSettings.config.default_provider = undefined;
    mock.browserMockSettings.config.providers = {};
    mock.browserMockSettings.provider_status = {};
  });

  await page.getByRole("button", { name: "设置", exact: true }).click();
  await page.locator("#set-preset").waitFor({ state: "visible" });

  const form = await page.evaluate(() => ({
    preset: document.querySelector("#set-preset")?.value,
    profile: document.querySelector("#set-profile-name")?.value,
    baseUrl: document.querySelector("#set-base-url")?.value,
    protocol: document.querySelector("#set-protocol")?.value,
    model: document.querySelector("#set-model")?.value,
  }));
  assert.deepEqual(form, {
    preset: "openai",
    profile: "openai",
    baseUrl: "https://api.openai.com/v1",
    protocol: "openai_chat",
    model: "gpt-5.6-sol",
  });

  const webCapability = page.getByLabel("当前模型服务的联网能力");
  assert.equal(await webCapability.getAttribute("data-search-state"), "attention");
  await webCapability.getByText(/线路协议切换为 Responses/).waitFor();

  await page.locator("#set-preset").selectOption("deepseek");
  assert.equal(await page.locator("#set-profile-name").inputValue(), "deepseek");
  assert.equal(await webCapability.getAttribute("data-search-state"), "attention");
  await webCapability.getByText("需切换线路", { exact: true }).waitFor();
  await webCapability.getByText(/Chat 只有普通 Tool Call/).waitFor();
  assert.deepEqual(
    await page.locator("#set-protocol option").evaluateAll((options) =>
      options.map((option) => option.value)
    ),
    ["openai_chat", "openai_responses"],
  );

  const primaryLayout = await page.locator(".provider-form").evaluate((form) => {
    const key = form.querySelector("#set-api-key")?.getBoundingClientRect();
    const model = form.querySelector(".provider-model-input")?.getBoundingClientRect();
    const advanced = form.querySelector(".provider-advanced");
    return {
      fieldCount: form.querySelectorAll(":scope > .provider-form-grid > .provider-form-field").length,
      keyLeft: key?.left,
      keyRight: key?.right,
      modelLeft: model?.left,
      modelRight: model?.right,
      advancedOpen: advanced?.hasAttribute("open"),
    };
  });
  assert.equal(primaryLayout.fieldCount, 4, "the default flow should only expose four field groups");
  assert.equal(primaryLayout.advancedOpen, false, "technical fields should stay collapsed for a preset");
  assert.ok(Math.abs(primaryLayout.keyLeft - primaryLayout.modelLeft) < 1, "API key and model must share a left edge");
  assert.ok(Math.abs(primaryLayout.keyRight - primaryLayout.modelRight) < 1, "API key and model must share a right edge");
  assert.equal(await page.locator("#set-base-url").isVisible(), false);
  assert.equal(await page.locator("#set-protocol").isVisible(), false);

  if (process.env.R_CODE_PROVIDER_FORM_SHOT) {
    fs.mkdirSync(path.dirname(process.env.R_CODE_PROVIDER_FORM_SHOT), { recursive: true });
    await page.locator(".provider-editor").screenshot({ path: process.env.R_CODE_PROVIDER_FORM_SHOT });
  }

  await page.locator(".provider-advanced > summary").click();
  assert.equal(await page.locator("#set-base-url").isVisible(), true);
  assert.equal(await page.locator("#set-protocol").isVisible(), true);
  if (process.env.R_CODE_PROVIDER_FORM_ADVANCED_SHOT) {
    fs.mkdirSync(path.dirname(process.env.R_CODE_PROVIDER_FORM_ADVANCED_SHOT), { recursive: true });
    await page.locator(".provider-editor").screenshot({ path: process.env.R_CODE_PROVIDER_FORM_ADVANCED_SHOT });
  }
  await page.getByRole("button", { name: "Anthropic 兼容口" }).click();
  assert.equal(await page.locator("#set-base-url").inputValue(), "https://api.deepseek.com/anthropic");
  assert.equal(await page.locator("#set-protocol").inputValue(), "anthropic_messages");
  assert.equal(await webCapability.getAttribute("data-search-state"), "hosted");
  await webCapability.getByText("DeepSeek 托管", { exact: true }).waitFor();
  await webCapability.getByText(/DeepSeek 服务端 Web Search/).waitFor();
  await page.getByRole("button", { name: "主入口" }).click();
  assert.equal(await page.locator("#set-base-url").inputValue(), "https://api.deepseek.com");
  assert.equal(await page.locator("#set-protocol").inputValue(), "openai_chat");

  await page.locator("#set-base-url").fill("http://api.deepseek.com");
  assert.equal(await webCapability.getAttribute("data-search-state"), "gateway");
  await webCapability.getByText("能力未确认", { exact: true }).waitFor();
  await page.getByRole("button", { name: "主入口" }).click();

  await page.locator("#set-protocol").selectOption("openai_responses");
  assert.equal(await webCapability.getAttribute("data-search-state"), "hosted");
  await webCapability.getByText(/DeepSeek 服务端 Web Search/).waitFor();
  await page.locator("#set-model").fill("deepseek-v4-pro");
  await page.getByText(/Responses 仅支持 deepseek-v4-flash/).waitFor({ state: "visible" });
  assert.equal(await webCapability.getAttribute("data-search-state"), "attention");
  await webCapability.getByText("需切换线路", { exact: true }).waitFor();
  assert.equal(await page.getByRole("button", { name: "保存", exact: true }).isDisabled(), true);
  await page.close();
});
