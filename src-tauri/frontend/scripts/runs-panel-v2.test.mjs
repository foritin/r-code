// 运行与子代理面板 v2：运行简报聚合、子代理编队状态环、关键事件折叠、待批权限内联卡。
// 数据走浏览器 mock（mock-task-queue 有 5 个子代理 + 工具调用；mock-task-permission 有待批权限）。
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
  const deadline = Date.now() + 20_000;
  while (Date.now() < deadline) {
    if (processHandle.exitCode != null) throw new Error(`Vite exited with ${processHandle.exitCode}`);
    try {
      const response = await fetch(url);
      if (response.ok) return;
    } catch {
      // Server is still starting.
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
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

async function openSummary(page, taskText) {
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const taskRow = page.locator(".sidebar-task-row").filter({ hasText: taskText });
  await taskRow.locator(".sidebar-task").click();
  await page.locator("#main-content > .scene-room").waitFor({ state: "visible" });
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().setCanvasTab("summary");
  });
  await page.locator(".sum-brief").waitFor({ state: "visible" });
}

test("summary briefing aggregates tool composition and folds routine reads out of the timeline", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await openSummary(page, "修复任务队列并发问题");

  // 运行简报四格：变更 / 验证 / 待批 / 失败操作。
  const outcomes = page.locator(".sum-brief-outcomes .sum-oc");
  assert.equal(await outcomes.count(), 4);
  assert.equal((await outcomes.nth(0).locator(".sum-oc-v").textContent())?.trim(), "2");
  assert.equal((await outcomes.nth(1).locator(".sum-oc-v").textContent())?.trim(), "未运行");

  // 构成条：mock 会话含命令与写入，读取/检索为 0 时不渲染分段。
  const segments = page.locator(".sum-bar span");
  assert.equal(await segments.count(), 2);
  assert.match((await page.locator(".sum-legend").textContent()) ?? "", /命令 3/);
  assert.match((await page.locator(".sum-legend").textContent()) ?? "", /写入 1/);
  assert.match((await page.locator(".sum-legend-total").textContent()) ?? "", /4 次操作/);

  // 关键事件：不允许出现常规「读取/检索」行（折叠进构成条）。
  const kinds = await page.locator(".sum-tl-row .sum-tl-kind").allTextContents();
  assert.ok(kinds.length > 0, "关键事件不能为空");
  assert.ok(!kinds.some((kind) => kind.trim() === "读取" || kind.trim() === "检索"), `常规读取/检索必须折叠: ${kinds}`);

  // 相对时间替代绝对时刻。
  const at = await page.locator(".sum-tl-row .sum-tl-top em").first().textContent();
  assert.match(at ?? "", /刚刚|分钟前|小时前|天前|昨天/, `应为相对时间: ${at}`);

  // 原始审计流默认折叠，展开后出现旧版逐行列表。
  assert.equal(await page.locator(".audit-list").count(), 0);
  await page.locator(".sum-audit-toggle").click();
  await page.locator(".audit-list .audit-row").first().waitFor({ state: "visible" });
  assert.ok((await page.locator(".audit-list .audit-row").count()) > 0);

  await page.close();
});

test("subagent squad shows per-agent status rings, outcome lines and live first ordering", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await openSummary(page, "修复任务队列并发问题");

  const cards = page.locator(".sum-sq");
  // 直出上限 4 + 「其余 N 个」折叠行（mock 有 5 个子代理运行）。
  assert.equal(await cards.count(), 4);
  assert.match((await page.locator(".sum-sq-more").textContent()) ?? "", /其余 1 个子代理/);

  // 运行中置顶且带 run 状态环；已完成带 ok 环。
  const firstAvatar = cards.nth(0).locator(".subagent-avatar");
  assert.equal(await firstAvatar.getAttribute("data-status"), "run");
  const rings = await cards.locator(".subagent-avatar").evaluateAll(
    (nodes) => nodes.map((node) => node.getAttribute("data-status")),
  );
  assert.ok(rings.includes("ok"), `完成态子代理应有 ok 状态环: ${rings}`);

  // 完成卡带结果摘要行。
  const outcomes = await cards.locator(".sum-sq-sub").allTextContents();
  assert.ok(outcomes.some((text) => text.startsWith("✓")), `完成卡应展示摘要: ${outcomes}`);
  assert.ok(outcomes.some((text) => text.includes("正在")), `运行卡应展示当前动作: ${outcomes}`);

  await page.close();
});

test("live card ticks elapsed seconds while the main run is active", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await openSummary(page, "修复任务队列并发问题");

  const elapsed = page.locator(".sum-live-elapsed");
  await elapsed.waitFor({ state: "visible" });
  const first = await elapsed.textContent();
  await page.waitForFunction(
    (previous) => document.querySelector(".sum-live-elapsed")?.textContent !== previous,
    first,
    { timeout: 5_000 },
  );
  await page.close();
});

test("pending permission renders an inline decision card in the summary", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await openSummary(page, "优化 Rust 编译性能");

  const card = page.locator(".sum-perm");
  await card.waitFor({ state: "visible" });
  assert.match((await card.locator(".sum-perm-top").textContent()) ?? "", /待批权限 · 1/);
  assert.match((await card.locator(".sum-perm-why").textContent()) ?? "", /cargo test/);
  const actions = card.locator(".sum-perm-actions button");
  assert.equal(await actions.count(), 3);
  assert.ok(await actions.nth(2).isEnabled(), "允许一次应可用");

  // 简报格同步警示待批数。
  const pendingCell = page.locator(".sum-brief-outcomes .sum-oc").nth(2).locator(".sum-oc-v");
  assert.equal((await pendingCell.textContent())?.trim(), "1");
  assert.match((await pendingCell.getAttribute("class")) ?? "", /warn/);

  await page.close();
});
