/**
 * 手动脚本：为主交互页复刻验收刷新 docs/support/archive/prototypes/screenshots/impl-room-*.png。
 *
 * 历史用法：node docs/support/archive/prototypes/tools/capture-room-screens.mjs
 * 依赖 src-tauri/frontend/node_modules（vite + playwright-core），无需 Tauri 后端：
 * 走 mock 数据通道（与 room-file-activity.test.mjs 同一套基建）。
 * - mock-task-complete 折叠/展开 两张 → 覆盖 docs/support/archive/prototypes/screenshots/impl-room-*.png
 * - mock-task-permission / mock-task-queue → target-qa/room-parity/（仅供人工核验，不提交）
 */
import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..", "..", "..", "..");
const frontendDir = path.join(repoRoot, "src-tauri", "frontend");
const requireFromFrontend = createRequire(path.join(frontendDir, "package.json"));
const { chromium } = requireFromFrontend("playwright-core");
const viteBin = path.join(frontendDir, "node_modules", "vite", "bin", "vite.js");

function browserExecutable() {
  const playwrightCache = path.join(process.env.LOCALAPPDATA ?? "", "ms-playwright");
  const cached = fs.existsSync(playwrightCache)
    ? fs.readdirSync(playwrightCache)
        .filter((entry) => /^chromium-\d+$/.test(entry))
        .sort((left, right) => Number(right.split("-")[1]) - Number(left.split("-")[1]))
        .flatMap((entry) => [path.join(playwrightCache, entry, "chrome-win64", "chrome.exe")])
        .find((candidate) => fs.existsSync(candidate))
    : undefined;
  return [
    cached,
    path.join(process.env.PROGRAMFILES ?? "", "Google", "Chrome", "Application", "chrome.exe"),
    path.join(process.env.PROGRAMFILES ?? "", "Microsoft", "Edge", "Application", "msedge.exe"),
  ].find((candidate) => candidate && fs.existsSync(candidate));
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
  throw new Error("Timed out waiting for the frontend dev server");
}

async function openRoom(page, baseUrl, taskId) {
  await page.goto(baseUrl, { waitUntil: "domcontentloaded" });
  await page.evaluate(async (id) => {
    // mock 通道（fetch 拦截）在导入 mock-data 模块时初始化，必须先于 store 使用。
    await import("/src/lib/mock-data.ts");
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    await useTasksStore.getState().refreshDetail(id);
    useAppStore.getState().openRoom(id);
  }, taskId);
  try {
    await page.locator(".scene-room").first().waitFor({ state: "visible", timeout: 15_000 });
  } catch (error) {
    console.error("scene-room 未出现，当前 app class =", await page.locator("#app").getAttribute("class").catch(() => "?"));
    throw error;
  }
  await page.waitForTimeout(800);
}

const port = 39877;
const baseUrl = `http://127.0.0.1:${port}/`;
const server = spawn(
  process.execPath,
  [viteBin, "--host", "127.0.0.1", "--port", String(port), "--strictPort"],
  { cwd: frontendDir, stdio: ["ignore", "pipe", "pipe"], windowsHide: true },
);
server.stdout.on("data", () => {});
server.stderr.on("data", () => {});
try {
  await waitForServer(baseUrl, server);
  const browser = await chromium.launch({ executablePath: browserExecutable(), headless: true });
  try {
    // 已完成会话：折叠态（默认）+ 展开态，覆盖提交的 impl 截图。
    const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
    await openRoom(page, baseUrl, "mock-task-complete");
    await page.waitForTimeout(400);
    await page.screenshot({ path: path.join(repoRoot, "docs", "support", "archive", "prototypes", "screenshots", "impl-room-1440.png") });
    const toggle = page.locator(".timeline-process-toggle").first();
    if (await toggle.count()) {
      await toggle.click();
      await page.locator(".timeline-process-body").waitFor({ state: "visible" });
      await page.waitForTimeout(400);
    }
    await page.screenshot({ path: path.join(repoRoot, "docs", "support", "archive", "prototypes", "screenshots", "impl-room-expanded.png") });
    await page.close();

    // 人工核验用：权限卡场景 + 运行中场景（不提交）。
    const parityDir = path.join(repoRoot, "target-qa", "room-parity");
    fs.mkdirSync(parityDir, { recursive: true });
    for (const [taskId, name] of [
      ["mock-task-permission", "perm-room-1440.png"],
      ["mock-task-queue", "queue-room-1440.png"],
    ]) {
      const scene = await browser.newPage({ viewport: { width: 1440, height: 900 } });
      await openRoom(scene, baseUrl, taskId);
      await scene.waitForTimeout(400);
      await scene.screenshot({ path: path.join(parityDir, name) });
      await scene.close();
    }
  } finally {
    await browser.close();
  }
} finally {
  server.kill();
}
console.log("captured impl-room-1440.png / impl-room-expanded.png (+ target-qa/room-parity)");
