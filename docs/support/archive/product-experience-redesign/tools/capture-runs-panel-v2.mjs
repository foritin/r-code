// 真实应用内截图：运行与子代理面板 v2（浏览器 mock 数据驱动）。
// 用法: node docs/product-experience-redesign/tools/capture-runs-panel-v2.mjs
import { spawn } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import path from "node:path";
import process from "node:process";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";

const require = createRequire("D:/project/rust/r-code/src-tauri/frontend/package.json");
const { chromium } = require("playwright-core");

const frontendDir = path.resolve("D:/project/rust/r-code/src-tauri/frontend");
const viteBin = path.join(frontendDir, "node_modules", "vite", "bin", "vite.js");
const outDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

function browserExecutable() {
  const localAppData = process.env.LOCALAPPDATA ?? "";
  const playwrightCache = path.join(localAppData, "ms-playwright");
  const cached = fs.existsSync(playwrightCache)
    ? fs.readdirSync(playwrightCache)
      .filter((entry) => /^chromium-\d+$/.test(entry))
      .sort((left, right) => Number(right.split("-")[1]) - Number(left.split("-")[1]))
      .map((entry) => path.join(playwrightCache, entry, "chrome-win64", "chrome.exe"))
      .find((candidate) => fs.existsSync(candidate))
    : undefined;
  return [
    cached,
    path.join(process.env.PROGRAMFILES ?? "", "Google", "Chrome", "Application", "chrome.exe"),
    path.join(process.env.PROGRAMFILES ?? "", "Microsoft", "Edge", "Application", "msedge.exe"),
    path.join(process.env["PROGRAMFILES(X86)"] ?? "", "Microsoft", "Edge", "Application", "msedge.exe"),
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
    } catch { /* starting */ }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error("Timed out waiting for the frontend test server");
}

const port = await freePort();
const baseUrl = `http://127.0.0.1:${port}/`;
const server = spawn(process.execPath, [viteBin, "--host", "127.0.0.1", "--port", String(port), "--strictPort"], {
  cwd: frontendDir,
  stdio: ["ignore", "pipe", "pipe"],
  windowsHide: true,
});
const browser = await chromium.launch({ executablePath: browserExecutable(), headless: true });

try {
  await waitForServer(baseUrl, server);

  async function openSummary(taskText) {
    const page = await browser.newPage({ viewport: { width: 1600, height: 1000 }, deviceScaleFactor: 2 });
    const errors = [];
    page.on("pageerror", (err) => errors.push(String(err)));
    await page.goto(baseUrl, { waitUntil: "networkidle" });
    const taskRow = page.locator(".sidebar-task-row").filter({ hasText: taskText });
    await taskRow.locator(".sidebar-task").click();
    await page.locator("#main-content > .scene-room").waitFor({ state: "visible" });
    await page.evaluate(async () => {
      const { useAppStore } = await import("/src/store/app.ts");
      useAppStore.getState().setCanvasTab("summary");
    });
    await page.locator(".sum-brief").waitFor({ state: "visible" });
    await page.waitForTimeout(700);
    if (errors.length > 0) throw new Error(`页面脚本错误: ${errors.join(" | ")}`);
    return page;
  }

  // 1. 并发任务（子代理编队：Codex 已完成 + 2 个 R-Code 运行中）
  const queue = await openSummary("修复任务队列并发问题");
  await queue.getByTestId("workbench-panel").screenshot({ path: path.join(outDir, "runs-panel-v2-actual-queue.png") });
  console.log("已输出 runs-panel-v2-actual-queue.png");

  // 2. 展开原始审计流
  await queue.locator(".sum-audit-toggle").click();
  await queue.waitForTimeout(300);
  await queue.getByTestId("workbench-panel").screenshot({ path: path.join(outDir, "runs-panel-v2-actual-audit-open.png") });
  console.log("已输出 runs-panel-v2-actual-audit-open.png");
  await queue.close();

  // 3. 待批权限内联卡
  const perm = await openSummary("优化 Rust 编译性能");
  await perm.getByTestId("workbench-panel").screenshot({ path: path.join(outDir, "runs-panel-v2-actual-permission.png") });
  console.log("已输出 runs-panel-v2-actual-permission.png");
  await perm.close();
} finally {
  await browser.close();
  server.kill();
}
