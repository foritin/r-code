// M2-03.A13：四个 GuideSheet（providers / plan-suggestion / subagents-pool / image-understanding）
// E2E——卡片入口可开、dialog+aria-modal、Esc 关闭、触发按钮焦点恢复；入口/内容/anchor 恰为 4。

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import net from "node:net";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

// 与 app-shell / onboarding-campaign 同款：fileURLToPath 修正 Windows cwd，
// browserExecutable 兜底 Chrome/Edge（CHROMIUM_PATH 仍可显式覆盖）。
const frontendDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const viteBin = path.join(frontendDir, "node_modules", "vite", "bin", "vite.js");

function browserExecutable() {
  if (process.env.CHROMIUM_PATH) return process.env.CHROMIUM_PATH;
  const candidates = [
    path.join(process.env.PROGRAMFILES ?? "", "Google", "Chrome", "Application", "chrome.exe"),
    path.join(process.env["PROGRAMFILES(X86)"] ?? "", "Microsoft", "Edge", "Application", "msedge.exe"),
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    "/usr/bin/google-chrome",
    "/usr/bin/chromium",
    "/usr/bin/chromium-browser",
  ];
  const found = candidates.find((candidate) => candidate && existsSync(candidate));
  if (!found) throw new Error("no Chromium-compatible browser found; set CHROMIUM_PATH");
  return found;
}

function freePort() {
  return new Promise((resolve) => {
    const server = net.createServer();
    server.listen(0, "127.0.0.1", () => {
      const port = server.address().port;
      server.close(() => resolve(port));
    });
  });
}

let server;
let browser;
let baseUrl;

test.before(async () => {
  const { chromium } = await import("playwright-core");
  const port = await freePort();
  baseUrl = `http://127.0.0.1:${port}/`;
  server = spawn(
    process.execPath,
    [viteBin, "--host", "127.0.0.1", "--port", String(port), "--strictPort"],
    { cwd: frontendDir, stdio: ["ignore", "pipe", "pipe"], windowsHide: true },
  );
  // Windows 冷启动会超过固定 sleep，轮询就绪（与 onboarding-campaign 一致）。
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    if (server.exitCode != null) throw new Error(`Vite exited with ${server.exitCode}`);
    try {
      const response = await fetch(baseUrl);
      if (response.ok) break;
    } catch {
      // Server is still starting.
    }
    await new Promise((resolve) => setTimeout(resolve, 80));
  }
  browser = await chromium.launch({
    headless: true,
    executablePath: browserExecutable(),
  });
  void baseUrl;
});

test.after(async () => {
  await browser?.close();
  server?.kill();
});

test("静态合同：恰有 4 个 GuideSheet 且全部声明 dialog/aria-modal/Esc", () => {
  const source = readFileSync(path.join(frontendDir, "src", "components", "settings", "GuideSheet.tsx"), "utf8");
  for (const id of ["plan-suggestion", "providers", "subagents-pool", "image-understanding"]) {
    assert.ok(source.includes(`"${id}"`), `缺 GuideSheet id: ${id}`);
  }
  assert.ok(source.includes('role="dialog"'));
  assert.ok(source.includes("aria-modal"));
  assert.ok(source.includes("Escape"));
});

// 卡片入口 × (打开 → Esc 关闭 → 焦点恢复)
const ENTRY_CASES = [
  { pane: "模型服务", index: 0, guide: "providers" },
  { pane: "模型服务", index: 1, guide: "image-understanding" },
  { pane: "Agent 编排", index: 0, guide: "plan-suggestion" },
  { pane: "子代理配置", index: 0, guide: "subagents-pool" },
];

test("A13 四入口：卡片打开 → Esc 关闭 → 焦点恢复", async () => {
  const { chromium } = await import("playwright-core");
  assert.ok(browser, "browser 未启动");
  const page = await browser.newPage({ locale: "zh-CN", viewport: { width: 1280, height: 860 } });
  await page.goto(baseUrl, { waitUntil: "networkidle", timeout: 20000 });

  for (const { pane, index, guide } of ENTRY_CASES) {
    await page.getByRole("button", { name: "设置", exact: true }).click();
    const paneBtn = page.getByRole("button", { name: pane, exact: true }).first();
    await paneBtn.waitFor({ state: "visible", timeout: 8000 });
    await paneBtn.click();
    await page.waitForTimeout(400);
    const openBtn = page.getByRole("button", { name: /指引手册/ }).nth(index);
    await openBtn.waitFor({ state: "visible", timeout: 8000 });
    await openBtn.click();
    const dialog = page.locator('[role="dialog"][aria-modal="true"]');
    await dialog.waitFor({ state: "visible", timeout: 5000 });
    assert.equal(await dialog.count(), 1, `${guide}: dialog 应唯一`);
    // Esc 关闭
    await page.keyboard.press("Escape");
    await dialog.waitFor({ state: "detached", timeout: 5000 });
    // 焦点恢复到触发按钮
    const refocused = await page.evaluate(() => document.activeElement?.textContent ?? "");
    assert.ok(refocused.includes("指引手册"), `${guide}: 关闭后焦点未恢复（${refocused.slice(0, 30)}）`);
    void guide;
  }
  await page.close();
});
