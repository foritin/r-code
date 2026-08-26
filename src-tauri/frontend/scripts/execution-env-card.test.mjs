// M4-03.A2：执行环境设置卡组件测试。
// 检出 Git Bash 时展示路径；未检出时警示（role=alert）可见。
// 复用 knowledge-settings-ui 的浏览器编排（vite dev server + chromium + store 驱动）。

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
  const cache = path.join(process.env.LOCALAPPDATA ?? "", "ms-playwright");
  const cached = fs.existsSync(cache)
    ? fs.readdirSync(cache)
      .filter((entry) => /^chromium-\d+$/.test(entry))
      .sort((left, right) => Number(right.split("-")[1]) - Number(left.split("-")[1]))
      .flatMap((entry) => [
        path.join(cache, entry, "chrome-win64", "chrome.exe"),
        path.join(cache, entry, "chrome-linux", "chrome"),
      ])
      .find((candidate) => fs.existsSync(candidate))
    : undefined;
  return [
    cached,
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
      const port = typeof address === "object" && address ? address.port : 0;
      candidate.close((error) => (error ? reject(error) : resolve(port)));
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
      /* retry */
    }
    await new Promise((resolve) => setTimeout(resolve, 80));
  }
  throw new Error("Timed out waiting for the frontend test server");
}

async function newToolsPage(browserInstance, baseUrl, probeOverride) {
  const page = await browserInstance.newPage({ viewport: { width: 1280, height: 800 } });
  if (probeOverride) {
    await page.addInitScript((override) => {
      window.__R_CODE_TEST_EXECUTION_ENV_PROBE = override;
    }, probeOverride);
  }
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().setSettingsPane("tools");
  });
  return page;
}

let server;
let browser;

test.before(async () => {
  const port = await freePort();
  const baseUrl = `http://127.0.0.1:${port}/`;
  server = spawn(process.execPath, [viteBin, "--host", "127.0.0.1", "--port", String(port), "--strictPort"], {
    cwd: frontendDir,
    stdio: ["ignore", "pipe", "pipe"],
    windowsHide: true,
  });
  await waitForServer(baseUrl, server);
  globalThis.__baseUrl = baseUrl;
  browser = await chromium.launch({ executablePath: browserExecutable(), headless: true });
});

test.after(async () => {
  await browser?.close();
  server?.kill();
});

test("execution env card shows detected git bash with its path", async () => {
  const page = await newToolsPage(browser, globalThis.__baseUrl, {
    dialect: "git-bash",
    program: "C:\\Program Files\\Git\\bin\\bash.exe",
    git_bash_detected: true,
  });
  try {
    const card = page.getByTestId("execution-env-card");
    await card.waitFor({ state: "visible" });
    const probe = await card.getByTestId("execution-env-probe").getAttribute("data-dialect");
    assert.equal(probe, "git-bash");
    const detected = card.getByTestId("execution-env-detected");
    await detected.waitFor({ state: "visible" });
    assert.match(await detected.textContent(), /Git Bash 已检出/);
    assert.match(await detected.textContent(), /bash\.exe/);
    // 检出场景不得出现回落警示。
    assert.equal(await card.getByTestId("execution-env-warning").count(), 0);
    // 路径覆盖输入框与保存按钮可用（M4-03.A1 的设置链路由 Rust 单测覆盖）。
    const input = card.getByTestId("execution-bash-path-input");
    assert.equal(await input.isEditable(), true);
  } finally {
    await page.close();
  }
});

test("execution env card warns when git bash is missing", async () => {
  const page = await newToolsPage(browser, globalThis.__baseUrl, {
    dialect: "pwsh",
    program: "pwsh.exe",
    git_bash_detected: false,
  });
  try {
    const card = page.getByTestId("execution-env-card");
    await card.waitFor({ state: "visible" });
    const warning = card.getByTestId("execution-env-warning");
    await warning.waitFor({ state: "visible" });
    assert.equal(await warning.getAttribute("role"), "alert");
    assert.match(await warning.textContent(), /未检出 Git Bash/);
    assert.match(await warning.textContent(), /PowerShell/);
    assert.equal(await card.getByTestId("execution-env-detected").count(), 0);
  } finally {
    await page.close();
  }
});
