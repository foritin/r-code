/**
 * 主交互页文件活动回归：
 * - 文件工具不再折叠成「已编辑 N 个文件」，而是每个文件一行；
 * - 行内即时显示 +N −N（edit 用 old/new_string 行数，write 按整份内容行数）；
 * - composer 为限宽居中浮层卡（min(760px, 100%) + 16px 圆角）。
 */
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
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
  throw new Error("Timed out waiting for the frontend test server");
}

let server;
let browser;
let baseUrl;

test.before(async () => {
  // 固定 uncommon 端口 + 重试：测试以 --test-concurrency=1 串行执行，
  // 避开部分开发机上动态端口探测后立刻被系统回收再分配的竞态。
  for (let attempt = 0; attempt < 3 && !browser; attempt += 1) {
    const port = 39870 + attempt;
    baseUrl = `http://127.0.0.1:${port}/`;
    const handle = spawn(process.execPath, [viteBin, "--host", "127.0.0.1", "--port", String(port), "--strictPort"], {
      cwd: frontendDir,
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
    });
    handle.stdout.on("data", () => {});
    handle.stderr.on("data", () => {});
    try {
      await waitForServer(baseUrl, handle);
      server = handle;
      browser = await chromium.launch({ executablePath: browserExecutable(), headless: true });
    } catch (error) {
      handle.kill();
      if (attempt === 2) throw error;
    }
  }
});

test.after(async () => {
  await browser?.close();
  server?.kill();
});

test("file activities render one row per file with inline diff stats", async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(baseUrl, { waitUntil: "domcontentloaded" });
  const taskId = "mock-task-complete";
  await page.evaluate(async (id) => {
    const { browserMockSetMessages } = await import("/src/lib/mock-data.ts");
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    browserMockSetMessages(id, [
      { id: `${id}-u1`, branch_id: "main", kind: "message", role: "user", text: "把监控页详情区做细一点。" },
      { id: `${id}-t1`, branch_id: "main", kind: "tool_call", tool_name: "read_file", call_id: "r1", input_json: '{"path":"src/pages/Monitor.tsx"}' },
      { id: `${id}-t1r`, branch_id: "main", kind: "tool_result", call_id: "r1", output_json: '{"content":"读取完成"}', is_error: false },
      { id: `${id}-t2`, branch_id: "main", kind: "tool_call", tool_name: "edit", call_id: "r2", input_json: JSON.stringify({ path: "src/pages/MonitorDetail.tsx", old_string: "a\nb\nc", new_string: "a\nb\nc\nd" }) },
      { id: `${id}-t2r`, branch_id: "main", kind: "tool_result", call_id: "r2", output_json: '{"content":"已更新"}', is_error: false },
      { id: `${id}-t3`, branch_id: "main", kind: "tool_call", tool_name: "write", call_id: "r3", input_json: JSON.stringify({ path: "src/styles/monitor.css", content: "one\ntwo\nthree" }) },
      { id: `${id}-t3r`, branch_id: "main", kind: "tool_result", call_id: "r3", output_json: '{"content":"已写入"}', is_error: false },
      { id: `${id}-a1`, branch_id: "main", kind: "message", role: "assistant", text: "详情区已改为抽屉结构。" },
    ]);
    await useTasksStore.getState().refreshDetail(id);
    useAppStore.getState().openRoom(id);
  }, taskId);

  const toggle = page.getByRole("button", { name: /耗时/ });
  await toggle.waitFor({ state: "visible" });
  await toggle.click();

  const rows = page.locator(".timeline-process-body .timeline-file-row");
  await rows.first().waitFor({ state: "visible" });
  assert.equal(await rows.count(), 3, "读取/编辑/写入各占一行，读取也是彩色图标文件行");
  assert.match(await rows.nth(0).locator(".timeline-file-name").innerText(), /Monitor\.tsx/);
  assert.match(await rows.nth(0).locator(".timeline-file-verb").innerText(), /读取/);
  assert.ok(
    await rows.nth(0).locator(".timeline-file-icon img").count() > 0
      || await rows.nth(0).locator(".timeline-file-icon svg").count() > 0,
    "读取行必须有扩展名类型图标（已知扩展为 img 资产，未知扩展回退 svg）",
  );
  const tsxIcon = rows.nth(0).locator(".timeline-file-icon img");
  if (await tsxIcon.count() > 0) {
    assert.ok(
      await tsxIcon.evaluate((img) => img.complete && img.naturalWidth > 0),
      "tsx 扩展名应加载真实图标资产",
    );
  }
  assert.match(await rows.nth(1).locator(".timeline-file-name").innerText(), /MonitorDetail\.tsx/);
  assert.match(await rows.nth(1).locator(".timeline-file-stat").innerText(), /\+4/, "edit 行的新增行数来自 new_string");
  assert.match(await rows.nth(1).locator(".timeline-file-stat").innerText(), /−3/, "edit 行的删除行数来自 old_string");
  assert.match(await rows.nth(2).locator(".timeline-file-name").innerText(), /monitor\.css/);
  assert.match(await rows.nth(2).locator(".timeline-file-stat").innerText(), /\+3/, "write 行按整份内容行数计新增");
  assert.match(await rows.nth(2).locator(".timeline-file-stat").innerText(), /−0/, "write 行没有可知的删除行数");

  await rows.nth(1).click();
  await page.locator(".timeline-process-body .timeline-activity-single-detail").waitFor({ state: "visible" });
  await page.close();
});

test("composer floats as a centered card instead of a full-width strip", async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(baseUrl, { waitUntil: "domcontentloaded" });
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    await useTasksStore.getState().refreshDetail("mock-task-queue");
    useAppStore.getState().openRoom("mock-task-queue");
  });
  await page.getByRole("textbox", { name: "给 Agent 的消息" }).waitFor({ state: "visible" });

  const contract = await page.locator(".scene-room .comp-box").evaluate((element) => {
    const style = getComputedStyle(element);
    const rect = element.getBoundingClientRect();
    const host = element.parentElement?.getBoundingClientRect();
    return {
      maxWidth: style.maxWidth,
      radius: style.borderRadius,
      shadow: style.boxShadow,
      // 居中相对 composer 容器测量：会话列两侧还有侧栏/工作台时，
      // 相对窗口的左右边距天然不相等。
      insetLeft: host ? rect.left - host.left : 0,
      insetRight: host ? host.right - rect.right : 0,
    };
  });
  assert.equal(contract.maxWidth, "min(760px, 100%)");
  assert.equal(contract.radius, "16px");
  assert.notEqual(contract.shadow, "none", "浮层卡必须有投影");
  assert.ok(
    Math.abs(contract.insetLeft - contract.insetRight) < 2,
    "浮层卡必须在其容器内水平居中",
  );
  await page.close();
});
