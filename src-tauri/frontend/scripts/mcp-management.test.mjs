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

test("MCP management is redacted, independently busy, and confirmation-bound", async () => {
  const page = await browser.newPage({ viewport: { width: 900, height: 680 } });
  const runtimeErrors = [];
  page.on("pageerror", (error) => runtimeErrors.push(String(error)));
  page.on("console", (message) => {
    if (message.type() === "error") runtimeErrors.push(message.text());
  });

  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.getByRole("button", { name: "知识与指令", exact: true }).click();
  await page.getByRole("tab", { name: "联网与 MCP", exact: true }).click();
  await page.getByText("内置联网工具", { exact: true }).waitFor();

  const builtIn = page.locator(".mcp-server-row").filter({ hasText: "r-code-research" });
  assert.equal(await builtIn.count(), 1);
  assert.match(await builtIn.innerText(), /内置/);

  await page.getByRole("button", { name: /MCP 市场/ }).click();
  await page.getByRole("button", { name: "搜索", exact: true }).click();
  await page.getByRole("button", { name: /npm · demo-search/ }).click();
  const addApproval = page.getByRole("alertdialog", { name: "确认 MCP 启动方案" });
  await addApproval.waitFor({ state: "visible" });
  const preview = await addApproval.locator("pre").innerText();
  assert.match(preview, /可执行文件: npx/);
  assert.match(preview, /参数 2: demo-search@1\.0\.0/);
  assert.match(preview, /环境变量名: DEMO_TOKEN/);
  await addApproval.getByRole("button", { name: "确认添加" }).click();

  const installed = page.locator(".mcp-server-row").filter({ hasText: "demo-search" });
  await installed.waitFor({ state: "visible" });
  assert.match(await installed.innerText(), /已关闭/);

  await installed.getByRole("button", { name: "凭据", exact: true }).click();
  const secretInput = page.locator('.mcp-credentials input[type="password"]');
  await secretInput.waitFor({ state: "visible" });
  assert.equal(await secretInput.inputValue(), "", "saved credentials must never be echoed");
  await secretInput.fill("sentinel-mcp-secret");
  await page.getByRole("button", { name: "保存凭据", exact: true }).click();
  await page.waitForFunction(() => document.querySelector('.mcp-credentials input[type="password"]')?.value === "");
  assert.ok(!(await page.locator("body").innerText()).includes("sentinel-mcp-secret"));

  // Switch to the Tauri path and delay one row. Only that row should become busy.
  await page.evaluate(async () => {
    const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
    globalThis.__TAURI_INTERNALS__ = {
      invoke: async (command, args = {}) => {
        if (command === "cmd_mcp_test_connection" && args.serverId === "r-code-research") {
          await new Promise((resolve) => setTimeout(resolve, 650));
        }
        return browserMockInvoke(command, args);
      },
    };
  });
  await builtIn.getByRole("button", { name: "测试", exact: true }).click();
  await builtIn.getByRole("button", { name: "连接中…", exact: true }).waitFor();
  assert.equal(
    await installed.getByRole("switch").isDisabled(),
    false,
    "a slow operation on one server must not block unrelated rows",
  );

  await page.setViewportSize({ width: 600, height: 420 });
  await installed.getByRole("switch").click();
  const enableApproval = page.getByRole("alertdialog", { name: "确认 MCP 启动方案" });
  await enableApproval.waitFor({ state: "visible" });
  await enableApproval.scrollIntoViewIfNeeded();
  const box = await enableApproval.boundingBox();
  assert.ok(box, "launch confirmation must be measurable");
  assert.ok(box.x >= 0 && box.x + box.width <= 600, "confirmation must not be horizontally clipped");
  assert.ok(box.y >= 0 && box.y + box.height <= 420, "confirmation must not be vertically clipped");

  assert.deepEqual(runtimeErrors, []);
  await page.close();
});
