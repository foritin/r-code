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

test("provider identity is present in frontend DTO, onboarding, settings, and mock paths", () => {
  const types = fs.readFileSync(path.join(frontendDir, "src/lib/types.ts"), "utf8");
  const onboarding = fs.readFileSync(
    path.join(frontendDir, "src/components/onboarding/OnboardingCampaign.tsx"),
    "utf8",
  );
  const settings = fs.readFileSync(
    path.join(frontendDir, "src/components/scenes/SettingsScene.tsx"),
    "utf8",
  );
  const mockRuntime = fs.readFileSync(path.join(frontendDir, "src/lib/browser-mock-runtime.ts"), "utf8");

  assert.match(types, /provider_kind\?: string;/, "settings responses must expose persisted identity");
  assert.match(types, /providerKind\?: string \| null;/, "save DTOs must carry stable identity");
  assert.match(
    onboarding,
    /providerKind:\s*selectedPreset\.id/,
    "onboarding must save the selected preset identity",
  );
  assert.match(
    settings,
    /providerKind:\s*activePreset\?\.id \?\? ""/,
    "settings must save the selected preset identity and explicitly clear custom profiles",
  );
  assert.match(
    mockRuntime,
    /provider\.providerKind == null\s*\? existing\?\.provider_kind/,
    "browser mock must preserve identity when legacy callers omit the field",
  );
});

test("browser settings mock round-trips and preserves providerKind", async () => {
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const identities = await page.evaluate(async () => {
    const ipc = await import("/src/lib/ipc.ts");
    const mock = await import("/src/lib/mock-data.ts");
    const name = "renamed-deepseek-browser-test";

    await ipc.settingsSaveProvider({
      name,
      providerKind: "deepseek",
      baseUrl: "https://relay.internal.example/v1",
      model: "deepseek-v4-flash",
      protocol: "openai_responses",
      showReasoning: false,
      activate: false,
    });
    const first = mock.browserMockSettings.config.providers?.[name]?.provider_kind;
    const firstReasoning = mock.browserMockSettings.config.providers?.[name]?.show_reasoning;

    await ipc.settingsSaveProvider({
      name,
      baseUrl: "https://second-relay.internal.example/v1",
      model: "deepseek-v4-flash",
      protocol: "openai_responses",
      activate: false,
    });
    const omitted = mock.browserMockSettings.config.providers?.[name]?.provider_kind;
    const omittedReasoning = mock.browserMockSettings.config.providers?.[name]?.show_reasoning;

    await ipc.settingsSaveProvider({
      name,
      providerKind: "openai",
      baseUrl: "https://api.deepseek.com",
      model: "deepseek-v4-pro",
      protocol: "openai_chat",
      showReasoning: true,
      activate: false,
    });
    const explicitOther = mock.browserMockSettings.config.providers?.[name]?.provider_kind;
    const explicitReasoning = mock.browserMockSettings.config.providers?.[name]?.show_reasoning;
    const defaultName = `${name}-default-off`;
    await ipc.settingsSaveProvider({
      name: defaultName,
      baseUrl: "https://api.example.com/v1",
      model: "text-model",
      protocol: "openai_chat",
      activate: false,
    });
    const defaultReasoning = mock.browserMockSettings.config.providers?.[defaultName]?.show_reasoning;
    await ipc.settingsDeleteProvider(name);
    await ipc.settingsDeleteProvider(defaultName);
    return { first, omitted, explicitOther, firstReasoning, omittedReasoning, explicitReasoning, defaultReasoning };
  });

  assert.deepEqual(identities, {
    first: "deepseek",
    omitted: "deepseek",
    explicitOther: "openai",
    firstReasoning: false,
    omittedReasoning: false,
    explicitReasoning: true,
    defaultReasoning: true,
  });
  await page.close();
});

test("provider reasoning is visible by default and credential copy is platform-neutral", () => {
  const settings = fs.readFileSync(path.join(frontendDir, "src/components/scenes/SettingsScene.tsx"), "utf8");
  const onboarding = fs.readFileSync(path.join(frontendDir, "src/components/onboarding/OnboardingCampaign.tsx"), "utf8");
  const mcp = fs.readFileSync(path.join(frontendDir, "src/components/scenes/McpPanel.tsx"), "utf8");

  assert.match(settings, /show_reasoning:\s*profile\?\.show_reasoning \?\? true/);
  assert.doesNotMatch(onboarding, /macOS 写入/);
  assert.doesNotMatch(mcp, /macOS 不访问钥匙串|macOS 写入/);
});

test("local configuration hydration never blocks the onboarding tour", async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
    const blockedCommands = new Set(["cmd_settings_get", "cmd_codex_integration_status"]);
    let release;
    const deferred = new Promise((resolve) => {
      release = resolve;
    });
    const control = {
      started: [],
      released: false,
      release: () => {
        control.released = true;
        release();
      },
    };
    globalThis.__rCodeOnboardingProbeControl = control;
    globalThis.__TAURI_INTERNALS__ = {
      invoke: async (command, args = {}) => {
        if (blockedCommands.has(command)) {
          control.started.push(command);
          await deferred;
        }
        return browserMockInvoke(command, args);
      },
    };
    window.dispatchEvent(new Event("r-code:onboarding:open"));
  });

  await page.waitForFunction(() => {
    const started = globalThis.__rCodeOnboardingProbeControl?.started ?? [];
    return started.includes("cmd_settings_get") && started.includes("cmd_codex_integration_status");
  });
  const tour = page.locator(".onboarding-tour");
  await tour.waitFor({ state: "visible" });

  assert.equal(await page.locator(".onboarding-loading").count(), 0, "background hydration must not cover the tour");
  assert.notEqual(await tour.getAttribute("aria-busy"), "true", "background hydration must not mark the tour busy");

  await page.locator(".onboarding-footer > button").last().click();
  assert.equal(await page.locator(".onboarding-header > span").textContent(), "主 Agent");

  await page.locator(".onboarding-dot").nth(2).click();
  await page.locator(".onboarding-provider-pick button").first().waitFor({ state: "visible", timeout: 2_000 });
  assert.equal(
    await page.evaluate(() => globalThis.__rCodeOnboardingProbeControl?.released),
    false,
    "the provider catalog must render while settings and Codex probes are still pending",
  );
  await page.evaluate(() => globalThis.__rCodeOnboardingProbeControl?.release());
  await page.close();
});

test("configuration probe failures stay local and recover in place", async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
    globalThis.__rCodeFailOnboardingBootstrap = true;
    globalThis.__TAURI_INTERNALS__ = {
      invoke: async (command, args = {}) => {
        if (
          globalThis.__rCodeFailOnboardingBootstrap
          && (command === "cmd_settings_get" || command === "cmd_codex_integration_status")
        ) {
          throw new Error(`mock failure: ${command}`);
        }
        return browserMockInvoke(command, args);
      },
    };
    window.dispatchEvent(new Event("r-code:onboarding:open"));
  });

  const tour = page.locator(".onboarding-tour");
  await tour.waitFor({ state: "visible" });
  await page.locator(".onboarding-footer > button").last().click();

  const engineFeedback = page.locator(".onboarding-engine-pick > small");
  await engineFeedback.getByText("Codex 状态暂不可用；R-Code 不受影响。").waitFor();
  assert.notEqual(await tour.getAttribute("aria-busy"), "true");

  await page.evaluate(() => {
    globalThis.__rCodeFailOnboardingBootstrap = false;
  });
  await engineFeedback.getByRole("button", { name: "重试" }).click();
  await page.locator(".onboarding-engine-option.codex > i").getByText("未连接", { exact: true }).waitFor();
  await page.locator(".onboarding-dot").nth(2).click();
  await page.locator(".onboarding-bootstrap-note").waitFor({ state: "hidden" });
  await page.close();
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
  const reasoningToggle = page.getByRole("switch", { name: "显示思考过程" });
  assert.equal(await reasoningToggle.isChecked(), true, "new providers should show reasoning by default");
  await reasoningToggle.click();
  assert.equal(await reasoningToggle.isChecked(), false, "the provider preference should remain user-controlled");

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
  assert.equal(await webCapability.getAttribute("data-search-state"), "hosted");
  await webCapability.getByText("DeepSeek 托管", { exact: true }).waitFor();
  assert.equal(await page.getByRole("button", { name: "保存并用于新对话", exact: true }).isDisabled(), false);

  await page.locator("#set-model").fill("deepseek-v4-unknown");
  await page.getByText(/Responses 支持 deepseek-v4-flash.*deepseek-v4-pro/).waitFor({ state: "visible" });
  assert.equal(await webCapability.getAttribute("data-search-state"), "attention");
  await webCapability.getByText("需切换线路", { exact: true }).waitFor();
  assert.equal(await page.getByRole("button", { name: "保存并用于新对话", exact: true }).isDisabled(), true);
  await page.close();
});

test("model candidate menu lists every preset model while the input is prefilled", async () => {
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  // Avoid CJK literals in test sources: "设置" is composed from code points.
  const settingsLabel = String.fromCodePoint(0x8BBE, 0x7F6E);
  await page.getByRole("button", { name: settingsLabel, exact: true }).click();
  await page.locator("#set-preset").waitFor({ state: "visible" });

  await page.locator(".provider-list .provider-row").filter({ hasText: "DeepSeek" }).click();
  await page.waitForFunction(() => document.querySelector("#set-model")?.value === "deepseek-v4-pro");

  // The native datalist was replaced: it filtered suggestions by the prefilled value,
  // hiding deepseek-v4-flash even though the catalog lists two models.
  assert.equal(await page.locator("#set-model-options").count(), 0);

  const trigger = page.locator(".provider-model-options");
  assert.equal(await trigger.isDisabled(), false);
  await trigger.click();

  const menu = page.getByRole("menu", { name: "候选模型", exact: true });
  const options = page.locator(`[role="menuitemradio"]`);
  await options.first().waitFor({ state: "visible" });
  assert.equal(await options.count(), 2, "both preset candidates must be listed while the input keeps its value");
  for (const name of ["deepseek-v4-flash", "deepseek-v4-pro"]) {
    await page.locator(`[role="menuitemradio"]`, { hasText: name }).waitFor({ state: "visible" });
  }

  // A captured window scroll listener used to re-measure the portal on every wheel
  // tick. WebView2 then reset the menu's scrollTop to zero, so the scrollbar was
  // visible but mouse wheels and trackpads could not move a long model list.
  await menu.evaluate((element) => {
    const template = element.querySelector('[role="menuitemradio"]');
    if (!(template instanceof HTMLElement)) throw new Error("model option template missing");
    for (let index = 0; index < 24; index += 1) {
      const clone = template.cloneNode(true);
      if (!(clone instanceof HTMLElement)) continue;
      clone.textContent = `scroll-regression-model-${index}`;
      clone.removeAttribute("aria-checked");
      element.append(clone);
    }
  });
  await page.waitForFunction(() => {
    const element = document.querySelector('[role="menu"][aria-label="候选模型"]');
    return element && element.scrollHeight > element.clientHeight;
  });
  await menu.hover();
  await page.mouse.wheel(0, 520);
  await page.waitForTimeout(150);
  const scrollMetrics = await menu.evaluate((element) => ({
    scrollTop: element.scrollTop,
    clientHeight: element.clientHeight,
    scrollHeight: element.scrollHeight,
  }));
  assert.ok(scrollMetrics.scrollHeight > scrollMetrics.clientHeight, "long model choices must overflow inside the menu");
  assert.ok(scrollMetrics.scrollTop > 0, "mouse wheel scrolling must persist instead of snapping back to the first model");

  await page.locator(`[role="menuitemradio"]`, { hasText: "deepseek-v4-flash" }).click();
  assert.equal(await page.locator("#set-model").inputValue(), "deepseek-v4-flash");
  await options.first().waitFor({ state: "detached" });
  await page.close();
});
