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

async function openEnhancedReview(page) {
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.getByRole("button", { name: "添加到任务", exact: true }).click();
  const addDialog = page.getByRole("dialog", { name: "添加到任务" });
  await addDialog.getByRole("button", { name: /^计划模式/ }).click();
  await page.getByLabel("描述新任务").fill("验证增强审核的拒绝反馈与折叠行为");
  await page.getByRole("button", { name: "发送", exact: true }).click();

  const plan = page.getByRole("region", { name: "当前计划" });
  await plan.waitFor({ state: "visible" });
  await page.getByText("在整理计划前还需要你确认两项边界", { exact: false }).waitFor();
  const questions = page.getByRole("group", { name: "计划需要你的回答" });
  await questions.getByLabel(/聚焦核心流程/).check();
  await questions.getByLabel(/两者结合/).check();
  await questions.getByRole("button", { name: "提交回答", exact: true }).click();
  await plan.getByRole("button", { name: "确认", exact: true }).waitFor();
  await plan.getByRole("button", { name: "确认", exact: true }).click();

  await page.evaluate(async () => {
    const [{ planGet, planUpdateItem }, { useAppStore }] = await Promise.all([
      import("/src/lib/ipc.ts"),
      import("/src/store/app.ts"),
    ]);
    const taskId = useAppStore.getState().currentTaskId;
    if (!taskId) throw new Error("Room task is missing");
    const current = await planGet(taskId);
    if (!current) throw new Error("Plan is missing");
    await planUpdateItem(taskId, {
      plan_id: current.plan.id,
      item_id: current.items[0].id,
      expected_revision: current.plan.revision,
      state: "completed",
    });
    useAppStore.getState().setCanvasTab("changes");
  });
  await page.getByRole("tab", { name: "增强", exact: true }).click();
  const review = page.getByTestId("enhanced-review");
  await review.waitFor({ state: "visible" });
  return review;
}

test("enhanced review stays empty when the task has no corresponding feature Plan", async () => {
  const page = await browser.newPage({ viewport: { width: 1180, height: 820 } });
  const runtimeErrors = [];
  page.on("pageerror", (error) => runtimeErrors.push(String(error)));
  page.on("console", (message) => {
    if (message.type() === "error") runtimeErrors.push(message.text());
  });

  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.getByLabel("描述新任务").fill("直接实现一个不经过功能计划的普通改动");
  await page.getByRole("button", { name: "发送", exact: true }).click();
  await page.locator(".scene.scene-room").waitFor({ state: "visible" });
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().setCanvasTab("changes");
  });
  await page.getByRole("tab", { name: "增强", exact: true }).click();

  const review = page.getByTestId("enhanced-review");
  await review.waitFor({ state: "visible" });
  assert.match(await review.innerText(), /没有对应的功能计划/);
  assert.match(await review.innerText(), /增强模式只显示由 Plan 功能点产生的变更/);
  assert.equal(await review.locator(".enhanced-feature").count(), 0);
  assert.deepEqual(runtimeErrors, []);
  await page.close();
});

test("enhanced review keeps rejection conflicts visible, collapses groups, and removes resolved groups", async () => {
  const page = await browser.newPage({ viewport: { width: 1180, height: 820 } });
  const runtimeErrors = [];
  page.on("pageerror", (error) => runtimeErrors.push(String(error)));
  page.on("console", (message) => {
    if (message.type() === "error") runtimeErrors.push(message.text());
  });

  const review = await openEnhancedReview(page);
  const groups = review.locator(".enhanced-feature");
  assert.equal(await groups.count(), 2);
  const completed = groups.nth(0);
  const completedItemId = await completed.getAttribute("data-item-id");
  assert.ok(completedItemId);

  const collapse = completed.getByRole("button", { name: /收起功能/ });
  assert.equal(await collapse.getAttribute("aria-expanded"), "true");
  await collapse.click();
  assert.equal(await completed.locator(".enhanced-files").count(), 0);
  const expand = completed.getByRole("button", { name: /展开功能/ });
  assert.equal(await expand.getAttribute("aria-expanded"), "false");
  await expand.click();
  await completed.locator(".enhanced-files").waitFor({ state: "visible" });

  const firstFile = completed.locator(".enhanced-file").first();
  const expandFile = firstFile.getByRole("button", { name: /展开文件/ });
  assert.equal(await expandFile.getAttribute("aria-expanded"), "false");
  assert.equal(await firstFile.locator(".enhanced-events").count(), 0, "file patches must default to collapsed");
  assert.match(await firstFile.innerText(), /\+\d+\s+-\d+/, "collapsed files must summarize added and deleted lines");
  await expandFile.click();
  assert.equal(await firstFile.getByRole("button", { name: /收起文件/ }).getAttribute("aria-expanded"), "true");
  await firstFile.locator(".enhanced-events").waitFor({ state: "visible" });

  await page.evaluate(() => {
    globalThis.__rCodeBrowserMockFailures = {
      cmd_plan_review_reject_feature: "rollback error: feature rejection conflicts at event demo-event",
    };
  });
  await completed.getByRole("button", { name: "拒绝整组", exact: true }).click();
  await completed.getByRole("button", { name: "再次确认拒绝", exact: true }).click();
  const conflict = completed.getByRole("alert");
  await conflict.waitFor({ state: "visible" });
  assert.match(await conflict.innerText(), /未拒绝整组.*重叠修改.*安全停止回滚/s);
  await page.waitForTimeout(1_200);
  assert.equal(await conflict.isVisible(), true, "a successful status refresh must not erase an action failure");
  assert.equal(await groups.count(), 2, "a failed rejection must remain pending for review");

  await page.evaluate(() => {
    globalThis.__rCodeBrowserMockFailures = {};
  });
  await completed.getByRole("button", { name: "拒绝整组", exact: true }).click();
  await completed.getByRole("button", { name: "再次确认拒绝", exact: true }).click();
  await page.waitForFunction(() => document.querySelectorAll(".enhanced-feature").length === 1);
  assert.equal(
    await review.locator(`.enhanced-feature[data-item-id="${completedItemId}"]`).count(),
    0,
    "resolved groups belong in the ledger, not the pending review list",
  );
  assert.deepEqual(runtimeErrors, []);
  await page.close();
});

test("enhanced review restores the same Plan content after leaving and reopening the task", async () => {
  const page = await browser.newPage({ viewport: { width: 1180, height: 820 } });
  const runtimeErrors = [];
  page.on("pageerror", (error) => runtimeErrors.push(String(error)));
  page.on("console", (message) => {
    if (message.type() === "error") runtimeErrors.push(message.text());
  });

  const review = await openEnhancedReview(page);
  const itemIds = await review.locator(".enhanced-feature").evaluateAll((groups) =>
    groups.map((group) => group.getAttribute("data-item-id")),
  );
  assert.equal(itemIds.length, 2);

  await page.locator(".sidebar-nav-item").filter({ hasText: "对话" }).click();
  await page.locator(".conversation-row")
    .filter({ hasText: "验证增强审核的拒绝反馈与折叠行为" })
    .locator(".conversation-main")
    .click();

  const restored = page.getByTestId("enhanced-review");
  await restored.waitFor({ state: "visible" });
  await page.waitForFunction(
    (expected) => document.querySelectorAll(".enhanced-feature").length === expected,
    itemIds.length,
  );
  assert.deepEqual(
    await restored.locator(".enhanced-feature").evaluateAll((groups) =>
      groups.map((group) => group.getAttribute("data-item-id")),
    ),
    itemIds,
  );
  assert.equal(
    await page.getByRole("tab", { name: "增强", exact: true }).getAttribute("aria-selected"),
    "true",
  );
  assert.deepEqual(runtimeErrors, []);
  await page.close();
});
