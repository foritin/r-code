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

test("Plan mode carries Goal into the task, asks per-question HITL, and approves feature todos", async () => {
  const page = await browser.newPage({ viewport: { width: 1180, height: 820 } });
  const runtimeErrors = [];
  page.on("pageerror", (error) => runtimeErrors.push(String(error)));
  page.on("console", (message) => {
    if (message.type() === "error") runtimeErrors.push(message.text());
  });

  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.getByRole("button", { name: "添加到任务", exact: true }).click();
  const addDialog = page.getByRole("dialog", { name: "添加到任务" });
  await page.setViewportSize({ width: 520, height: 620 });
  await page.waitForFunction(() => {
    const dialog = document.querySelector("[role='dialog'][aria-label='添加到任务']");
    if (!(dialog instanceof HTMLElement)) return false;
    const rect = dialog.getBoundingClientRect();
    return rect.x >= 0 && rect.right <= window.innerWidth && rect.y >= 0 && rect.bottom <= window.innerHeight;
  });
  const addBox = await addDialog.boundingBox();
  assert.ok(addBox && addBox.x >= 0 && addBox.x + addBox.width <= 520, "Add must not be clipped in a narrow window");
  assert.ok(addBox && addBox.y >= 0 && addBox.y + addBox.height <= 620, "Add must remain vertically reachable");
  await page.setViewportSize({ width: 1180, height: 820 });
  await addDialog.getByRole("button", { name: /目标/ }).click();
  await addDialog.getByLabel("目标", { exact: true }).fill("交付一个可逐功能验收的计划模式");
  await addDialog.getByRole("button", { name: "保存目标", exact: true }).click();
  await addDialog.getByRole("status").filter({ hasText: "已保存" }).waitFor();
  await addDialog.getByRole("button", { name: /返回添加/ }).click();
  await addDialog.getByRole("button", { name: /^计划模式/ }).click();

  await page.getByLabel("描述新任务").fill("先澄清边界，再生成实施计划");
  await page.getByRole("button", { name: "发送", exact: true }).click();
  await page.locator(".scene-room").waitFor({ state: "visible" });

  const panel = page.getByRole("region", { name: "当前计划" });
  await panel.waitFor({ state: "visible" });
  assert.match(await panel.locator(".plan-goal").innerText(), /逐功能验收/);
  assert.match(await panel.locator(".plan-metadata").innerText(), /demo-plan-.*\/plan\.md/);
  await page.getByText("在整理计划前还需要你确认两项边界", { exact: false }).waitFor();

  const questions = page.getByRole("group", { name: "计划需要你的回答" });
  await questions.getByLabel(/聚焦核心流程/).check();
  await questions.getByLabel(/两者结合/).check();
  await questions.getByRole("button", { name: "提交回答", exact: true }).click();

  await panel.getByRole("button", { name: "确认实施", exact: true }).waitFor();
  const features = panel.locator(".plan-feature-list > li");
  assert.equal(await features.count(), 2);
  assert.match(await features.nth(1).innerText(), /依赖：明确实现边界/);
  await panel.getByRole("button", { name: "确认实施", exact: true }).click();
  await panel.getByText("实施中", { exact: false }).first().waitFor();
  assert.match(await features.first().innerText(), /进行中/);

  // Complete feature 1 through the same typed IPC used by the runtime. Feature 2 then becomes
  // active, which lets this test cover terminal decisions and live-but-disabled review together.
  await page.evaluate(async () => {
    const [{ planGet, planUpdateItem }, { useAppStore }] = await Promise.all([
      import("/src/lib/ipc.ts"),
      import("/src/store/app.ts"),
    ]);
    const taskId = useAppStore.getState().currentTaskId;
    if (!taskId) throw new Error("Room task is missing");
    const plan = await planGet(taskId);
    if (!plan) throw new Error("Plan is missing");
    await planUpdateItem(taskId, {
      plan_id: plan.plan.id,
      item_id: plan.items[0].id,
      expected_revision: plan.plan.revision,
      state: "completed",
    });
    useAppStore.getState().setCanvasTab("changes");
  });

  await page.getByRole("tab", { name: "增强", exact: true }).click();
  const enhanced = page.getByTestId("enhanced-review");
  await enhanced.waitFor({ state: "visible" });
  const reviewFeatures = enhanced.locator(".enhanced-feature");
  assert.equal(await reviewFeatures.count(), 2);
  assert.match(await reviewFeatures.nth(0).innerText(), /已完成/);
  assert.match(await reviewFeatures.nth(1).innerText(), /功能仍在实施/);
  assert.match(await reviewFeatures.nth(1).innerText(), /二进制/);
  assert.equal(await reviewFeatures.nth(1).getByRole("button", { name: "接受整组" }).isDisabled(), true);

  // One file operation must never globally disable an unrelated file in the same feature.
  await page.evaluate(() => {
    window.__rCodeBrowserMockDelayMs = { cmd_plan_review_accept_file: 320 };
  });
  const completedFiles = reviewFeatures.nth(0).locator(".enhanced-file");
  const changedLine = completedFiles.nth(0).locator(".patch-add").first();
  assert.equal(await changedLine.evaluate((element) => getComputedStyle(element, "::before").width), "5px");
  assert.equal(await changedLine.evaluate((element) => getComputedStyle(element, "::before").opacity), "1");
  const metaLine = completedFiles.nth(0).locator(".patch-meta").first();
  assert.equal(await metaLine.evaluate((element) => getComputedStyle(element, "::before").opacity), "0.5");
  await completedFiles.nth(0).getByRole("button", { name: "接受", exact: true }).click();
  assert.equal(await completedFiles.nth(1).getByRole("button", { name: "接受", exact: true }).isEnabled(), true);
  await completedFiles.nth(0).getByText("已接受", { exact: true }).waitFor();
  const wholeFeatureAccept = reviewFeatures.nth(0).getByRole("button", { name: "接受整组", exact: true });
  await page.waitForFunction(
    (element) => element.disabled && element.title === "已有文件级决定，不能再整组处理",
    await wholeFeatureAccept.elementHandle(),
  );

  // Reject is deliberately a two-step action and stays scoped to this file.
  const reject = completedFiles.nth(1).getByRole("button", { name: "拒绝", exact: true });
  await reject.click();
  const confirmReject = completedFiles.nth(1).getByRole("button", { name: "确认拒绝", exact: true });
  await confirmReject.waitFor();
  await confirmReject.click();
  await completedFiles.nth(1).getByText("已拒绝", { exact: true }).waitFor();

  await page.setViewportSize({ width: 520, height: 620 });
  const viewportFit = await page.evaluate(() => ({
    viewport: window.innerWidth,
    documentWidth: document.documentElement.scrollWidth,
    reviewClientWidth: document.querySelector("[data-testid='enhanced-review']")?.clientWidth ?? 0,
  }));
  assert.ok(viewportFit.reviewClientWidth > 0, "enhanced review must remain visible in a narrow window");
  assert.ok(viewportFit.documentWidth <= viewportFit.viewport, "enhanced review must not create page-level clipping");
  await page.setViewportSize({ width: 1180, height: 820 });

  // Cancellation is intentionally separate from workspace rollback and requires a deliberate
  // second click. Re-entering Plan afterwards must allocate a different aggregate instead of
  // reviving the cancelled revision.
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().hideWorkbench();
  });
  const cancelledPlanId = await page.evaluate(async () => {
    const [{ planGet }, { useAppStore }] = await Promise.all([
      import("/src/lib/ipc.ts"),
      import("/src/store/app.ts"),
    ]);
    const taskId = useAppStore.getState().currentTaskId;
    if (!taskId) throw new Error("Room task is missing");
    return (await planGet(taskId))?.plan.id ?? null;
  });
  await panel.getByRole("button", { name: "取消计划", exact: true }).click();
  await panel.getByText("工作区中的文件不会被回滚", { exact: false }).waitFor();
  await panel.getByRole("button", { name: "确认取消", exact: true }).click();
  await page.waitForTimeout(250);
  const cancelledState = await page.evaluate(async () => {
    const [{ planGet }, { useAppStore }] = await Promise.all([
      import("/src/lib/ipc.ts"),
      import("/src/store/app.ts"),
    ]);
    const taskId = useAppStore.getState().currentTaskId;
    return taskId ? (await planGet(taskId))?.plan.state ?? null : null;
  });
  assert.equal(cancelledState, "cancelled", await panel.innerText());
  await panel.getByText("已取消", { exact: false }).first().waitFor({ timeout: 3_000 });

  await page.getByRole("button", { name: "添加到任务", exact: true }).click();
  const roomAddDialog = page.getByRole("dialog", { name: "添加到任务" });
  await roomAddDialog.getByRole("button", { name: /^计划模式/ }).click();
  await panel.getByText("草拟中", { exact: false }).first().waitFor();
  const replacementPlanId = await page.evaluate(async () => {
    const [{ planGet }, { useAppStore }] = await Promise.all([
      import("/src/lib/ipc.ts"),
      import("/src/store/app.ts"),
    ]);
    const taskId = useAppStore.getState().currentTaskId;
    if (!taskId) throw new Error("Room task is missing");
    return (await planGet(taskId))?.plan.id ?? null;
  });
  assert.ok(cancelledPlanId && replacementPlanId && cancelledPlanId !== replacementPlanId);

  // Plan is an inline collaboration surface; it must not replace conversation history.
  assert.equal(await page.locator(".timeline").isVisible(), true);
  assert.deepEqual(runtimeErrors, []);
  await page.close();
});
