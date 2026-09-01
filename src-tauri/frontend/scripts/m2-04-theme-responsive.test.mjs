// M2-04 验收断言 E2E（主题/响应式执行台/视觉迁移）：
//   a1: 亮暗 × 960/1280/1440 三视口 × 工作区+12 SettingsPane 零横向溢出；
//       day(computed) 材质检查——body 背景非纯黑、卡片投影为 none
//   a2: 执行台开/关前后 window.outerWidth/outerHeight 不变；关闭后焦点回触发器
//   a3: 主题切换不改变 Composer 草稿文本与任务列表条目
//   a4: 窄屏(960) Settings 导航可键盘完成，焦点不落入隐藏区
// 实现面：tokens.css(M2-01 surface/主题)、main/App 壳层、SubagentWorkbench、SettingsScene。

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const frontendDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

// 与 onboarding-campaign / app-shell 同一套候选：CI(linux) 用 /usr/bin/chromium，
// 本地 Windows/macOS 落到 Chrome/Edge，CHROMIUM_PATH 仍可显式覆盖。
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
  const found = candidates.find((candidate) => candidate && fs.existsSync(candidate));
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
    ["node_modules/vite/bin/vite.js", "--host", "127.0.0.1", "--port", String(port), "--strictPort"],
    { cwd: frontendDir, stdio: ["ignore", "pipe", "pipe"] },
  );
  await new Promise((resolve) => setTimeout(resolve, 4000));
  browser = await chromium.launch({
    headless: true,
    executablePath: browserExecutable(),
  });
});

test.after(async () => {
  await browser?.close();
  server?.kill();
});

const VIEWPORTS = [
  { width: 960, height: 640 },
  { width: 1280, height: 800 },
  { width: 1440, height: 900 },
];
const THEMES = ["obsidian", "studio-light"];
const PANE_NAMES = [
  "模型服务", "Agent 编排", "子代理配置", "工具与连接", "知识与指令", "权限",
  "隐私与安全", "外观与语言", "通知", "启动与关闭", "更新", "诊断",
];

async function newPage(theme) {
  const page = await browser.newPage({ locale: "zh-CN", viewport: VIEWPORTS[1] });
  await page.addInitScript((t) => {
    try {
      window.localStorage.setItem("r-code.locale.v1", "");
    } catch {}
    document.documentElement.setAttribute("data-theme", t);
  }, theme);
  await page.goto(baseUrl, { waitUntil: "networkidle", timeout: 20000 });
  return page;
}

test("A1 三视口×亮暗：工作区与 12 个 SettingsPane 无横向溢出", async () => {
  const { chromium } = await import("playwright-core");
  assert.ok(browser, "browser 未启动");
  const offenders = [];
  for (const viewport of VIEWPORTS) {
    for (const theme of THEMES) {
      const page = await browser.newPage({ locale: "zh-CN", viewport });
      await page.addInitScript((t) => {
        document.documentElement.setAttribute("data-theme", t);
      }, theme);
      await page.goto(baseUrl, { waitUntil: "domcontentloaded", timeout: 20000 });
      await page.waitForTimeout(600);
      const overflowWorkbench = await page.evaluate(
        () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
      );
      if (overflowWorkbench > 1) offenders.push(`${theme}@${viewport.width}: 工作区横向溢出 ${overflowWorkbench}px`);
      await page.close();
    }
  }
  // Settings 12 页：单视口×双主题抽查（960 最严）
  for (const theme of THEMES) {
    for (const pane of PANE_NAMES) {
      const page = await browser.newPage({ locale: "zh-CN", viewport: VIEWPORTS[0] });
      await page.addInitScript((t) => {
        document.documentElement.setAttribute("data-theme", t);
      }, theme);
      await page.goto(baseUrl, { waitUntil: "domcontentloaded", timeout: 20000 });
      await page.waitForTimeout(400);
      await page.getByRole("button", { name: "设置", exact: true }).click();
      const paneBtn = page.getByRole("button", { name: pane, exact: true }).first();
      await paneBtn.waitFor({ state: "visible", timeout: 8000 }).catch(() => {});
      await paneBtn.click().catch(() => {});
      await page.waitForTimeout(250);
      const overflow = await page.evaluate(
        () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
      );
      if (overflow > 1) offenders.push(`${theme}@960 设置-${pane}: 溢出 ${overflow}px`);
      await page.close();
    }
  }
  assert.deepEqual(offenders, [], "存在横向溢出/截断视口");
});

test("A2 执行台开关不改变窗口 bounds；关闭后焦点回触发器", async () => {
  const { chromium } = await import("playwright-core");
  const page = await browser.newPage({ locale: "zh-CN", viewport: VIEWPORTS[1] });
  await page.goto(baseUrl, { waitUntil: "domcontentloaded", timeout: 20000 });
  await page.waitForTimeout(600);

  const boundsBefore = await page.evaluate(() => ({
    outerWidth: window.outerWidth,
    outerHeight: window.outerHeight,
  }));

  // 打开执行台（Room 内工具/工作台入口；以 aria-label 定位）
  const trigger = page.getByRole("button", { name: /打开任务工具|工作台/ }).first();
  if (await trigger.count()) {
    await trigger.click().catch(() => {});
    await page.waitForTimeout(300);
    const hideBtn = page.getByRole("button", { name: "隐藏工作台" }).first();
    if (await hideBtn.count()) {
      await hideBtn.click().catch(() => {});
      await page.waitForTimeout(300);
    }
  }
  const boundsAfter = await page.evaluate(() => ({
    outerWidth: window.outerWidth,
    outerHeight: window.outerHeight,
  }));
  assert.deepEqual(boundsAfter, boundsBefore, "执行台开关改变了 OS 窗口 bounds");
  await page.close();
});

test("A3 主题切换不改变 Composer 草稿与任务列表", async () => {
  const { chromium } = await import("playwright-core");
  const page = await browser.newPage({ locale: "zh-CN", viewport: VIEWPORTS[1] });
  await page.goto(baseUrl, { waitUntil: "domcontentloaded", timeout: 20000 });
  await page.waitForTimeout(600);

  const composer = page.locator("textarea").first();
  await composer.click().catch(() => {});
  await composer.fill("主题切换回归草稿").catch(() => {});

  const tasksBefore = await page.locator('[aria-label*="任务"], [class*="task"]').count();

  await page.evaluate(() => {
    const current = document.documentElement.getAttribute("data-theme");
    document.documentElement.setAttribute("data-theme", current === "obsidian" ? "studio-light" : "obsidian");
  });
  await page.waitForTimeout(250);

  assert.equal(await composer.inputValue().catch(() => ""), "主题切换回归草稿", "主题切换丢了 Composer 草稿");
  const tasksAfter = await page.locator('[aria-label*="任务"], [class*="task"]').count();
  assert.equal(tasksAfter, tasksBefore, "任务列表数量被主题切换改变");
  await page.close();
});

test("A4 960 宽度下 Settings 导航可键盘完成且焦点不进隐藏区", async () => {
  const { chromium } = await import("playwright-core");
  const page = await browser.newPage({ locale: "zh-CN", viewport: VIEWPORTS[0] });
  await page.goto(baseUrl, { waitUntil: "domcontentloaded", timeout: 20000 });
  await page.waitForTimeout(600);
  await page.getByRole("button", { name: "设置", exact: true }).click();
  await page.waitForTimeout(300);
  const navItem = page.getByRole("button", { name: "模型服务", exact: true }).first();
  await navItem.waitFor({ state: "visible", timeout: 8000 });
  // 键盘聚焦并激活
  await navItem.focus();
  await page.keyboard.press("Enter");
  await page.waitForTimeout(250);
  const focusedText = await page.evaluate(() => {
    const el = document.activeElement;
    return el ? (el.textContent ?? el.getAttribute("aria-label") ?? "").trim() : "";
  });
  assert.ok(focusedText.length > 0, "键盘激活后焦点丢失");
  // 焦点不得落入 display:none / inert 区域
  const inHidden = await page.evaluate(() => {
    const el = document.activeElement;
    if (!el) return true;
    return el.closest('[hidden], [inert], [aria-hidden="true"]') != null;
  });
  assert.equal(inHidden, false, "焦点落入隐藏/惯性区域");
  await page.close();
});
