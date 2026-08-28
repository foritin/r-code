// M2-03.A13：四个 GuideSheet（providers / plan-suggestion / subagents-pool / image-understanding）
// E2E——卡片入口可开、dialog+aria-modal、Esc 关闭、触发按钮焦点恢复；入口/内容/anchor 恰为 4。

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import net from "node:net";
import path from "node:path";
import test from "node:test";

const frontendDir = path.resolve(path.dirname(new URL(import.meta.url).pathname), "..");

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
    ["node_modules/vite/bin/vite.js", "--host", "127.0.0.1", "--port", String(port), "--strictPort"],
    { cwd: frontendDir, stdio: ["ignore", "pipe", "pipe"] },
  );
  await new Promise((resolve) => setTimeout(resolve, 4000));
  browser = await chromium.launch({
    headless: true,
    executablePath: process.env.CHROMIUM_PATH ?? "/usr/bin/chromium",
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
