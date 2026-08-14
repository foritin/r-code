import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import fs from "node:fs";
import net from "node:net";
import path from "node:path";
import process from "node:process";
import test from "node:test";
import { fileURLToPath } from "node:url";
import ts from "typescript";

import { chromium } from "playwright-core";

const frontendDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const viteBin = path.join(frontendDir, "node_modules", "vite", "bin", "vite.js");
const modelSource = fs.readFileSync(
  path.join(frontendDir, "src/components/room/session-run-summary-model.ts"),
  "utf8",
);
const componentSource = fs.readFileSync(
  path.join(frontendDir, "src/components/room/SessionRunSummary.tsx"),
  "utf8",
);
const roomStyles = fs.readFileSync(
  path.join(frontendDir, "src/styles/scenes/room.css"),
  "utf8",
);
const model = await import(`data:text/javascript;base64,${Buffer.from(
  ts.transpileModule(modelSource, {
    compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 },
  }).outputText,
).toString("base64")}`);

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

test("summary model reconciles live steps, deduplicates files, and counts real diff rows", () => {
  const steps = model.sessionSummarySteps([
    { description: "定位实现", completed: true },
    { description: "完成交互", completed: false },
    { description: "验证", completed: false },
  ], null, true);
  assert.deepEqual(steps.map((step) => step.state), ["completed", "current", "pending"]);
  assert.equal(model.currentStepNumber(steps), 2);

  const base = {
    task_id: "task",
    tool_call_id: null,
    change_type: "modify",
    before_hash: "before",
    after_hash: "after",
    old_path: null,
  };
  const files = model.latestSessionChanges([
    { ...base, id: "old", path: "src\\main.rs", created_at: "2026-01-01T00:00:00Z" },
    { ...base, id: "new", path: "src/main.rs", created_at: "2026-01-02T00:00:00Z" },
    { ...base, id: "other", path: "README.md", created_at: "2026-01-01T12:00:00Z" },
  ]);
  assert.equal(files.length, 2);
  assert.equal(files.find((file) => file.path === "src/main.rs")?.id, "new");
  assert.deepEqual(model.changeStatFromDiff({
    supported: true,
    path: "src/main.rs",
    lines: [
      { kind: "ctx", text: "context" },
      { kind: "add", text: "one" },
      { kind: "add", text: "two" },
      { kind: "del", text: "old" },
    ],
  }), { additions: 2, deletions: 1, available: true });
  assert.deepEqual(model.changeStatFromDiff({
    supported: true,
    path: "large.log",
    lines: [{ kind: "add", text: "preview only" }],
    truncated: true,
  }), { additions: null, deletions: null, available: false });
});

test("session summary stays on the shared desktop path and has an opaque pre-color-mix fallback", () => {
  assert.doesNotMatch(componentSource, /isMacPlatform|isWindowsPlatform|navigator\.platform|target_os/);
  assert.match(
    roomStyles,
    /\.session-run-summary\s*\{[^}]*background:var\(--bg-card\);\s*background:color-mix/s,
  );
  assert.match(roomStyles, /@supports not \(container-type: inline-size\)[^}]*session-summary-long/s);
});

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

test("session strip exposes mutually exclusive step/change popovers and preserves the room when opening a file", async () => {
  const page = await browser.newPage({ viewport: { width: 1024, height: 768 } });
  await page.addInitScript(() => {
    Object.defineProperty(navigator, "platform", { configurable: true, get: () => "MacIntel" });
  });
  const runtimeErrors = [];
  page.on("pageerror", (error) => runtimeErrors.push(String(error)));
  page.on("console", (message) => {
    if (message.type() === "error") runtimeErrors.push(message.text());
  });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  assert.match(await page.locator("#app").getAttribute("class"), /platform-macos/);
  await page.evaluate(async () => {
    const { browserMockMessages, browserMockSetMessages } = await import("/src/lib/mock-data.ts");
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    const taskId = "mock-task-queue";
    const messages = browserMockMessages(taskId);
    browserMockSetMessages(taskId, [
      ...messages.slice(0, 3),
      {
        id: `${taskId}-summary-plan`,
        branch_id: "main",
        kind: "system",
        text: "plan",
        output_json: JSON.stringify({
          steps: [
            { description: "定位会话状态来源", completed: true },
            { description: "实现步骤与变更汇总", completed: false },
            { description: "完成交互验证", completed: false },
          ],
        }),
      },
      ...messages.slice(3),
    ]);
    await Promise.all([
      useTasksStore.getState().refreshTasks(),
      useTasksStore.getState().refreshDetail(taskId),
      useTasksStore.getState().refreshWorkspaces(),
    ]);
    useAppStore.getState().openRoom(taskId);
  });

  const summary = page.getByTestId("session-run-summary");
  await summary.waitFor({ state: "visible" });
  assert.match(await summary.locator(".session-run-summary").getAttribute("class"), /is-running/);

  const stepTrigger = summary.getByRole("button", { name: /第 2 \/ 3 步/ });
  const changeTrigger = summary.getByRole("button", { name: /2 个文件已更改/ });
  await page.waitForFunction(() => document.querySelector(".change-trigger")?.textContent?.includes("+4"));
  assert.match(await changeTrigger.innerText(), /\+4/);
  assert.match(await changeTrigger.innerText(), /−2/);

  await stepTrigger.click();
  const stepDialog = page.getByRole("dialog", { name: "当前任务步骤", exact: true });
  await stepDialog.waitFor({ state: "visible" });
  assert.equal(await stepDialog.locator(".session-step-row").count(), 3);
  assert.equal(await stepDialog.locator(".state-current").count(), 1);

  await changeTrigger.click();
  await stepDialog.waitFor({ state: "detached" });
  const changeDialog = page.getByRole("dialog", { name: "当前对话的文件变更", exact: true });
  await changeDialog.waitFor({ state: "visible" });
  assert.equal(await changeDialog.locator(".session-change-row").count(), 2);
  if (process.env.R_CODE_SESSION_SUMMARY_SHOT) {
    await page.screenshot({ path: process.env.R_CODE_SESSION_SUMMARY_SHOT, fullPage: true });
  }

  const bounds = await changeDialog.evaluate((element) => {
    const rect = element.getBoundingClientRect();
    return { top: rect.top, right: rect.right, bottom: rect.bottom, left: rect.left };
  });
  assert.ok(bounds.top >= 3 && bounds.left >= 3);
  assert.ok(bounds.right <= 1021 && bounds.bottom <= 765);

  await changeDialog.locator(".session-change-row").first().focus();
  await page.keyboard.press("Escape");
  await changeDialog.waitFor({ state: "detached" });
  assert.equal(await changeTrigger.getAttribute("aria-expanded"), "false");
  assert.equal(await changeTrigger.evaluate((element) => document.activeElement === element), true);

  await page.setViewportSize({ width: 800, height: 600 });
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().setThemeMode("light");
  });
  await stepTrigger.click();
  await stepDialog.waitFor({ state: "visible" });
  const compactBounds = await stepDialog.evaluate((element) => {
    const rect = element.getBoundingClientRect();
    return {
      top: rect.top,
      right: rect.right,
      bottom: rect.bottom,
      left: rect.left,
      animationName: getComputedStyle(element).animationName,
      theme: document.documentElement.dataset.theme,
    };
  });
  assert.ok(compactBounds.top >= 3 && compactBounds.left >= 3);
  assert.ok(compactBounds.right <= 797 && compactBounds.bottom <= 597);
  assert.equal(compactBounds.animationName, "none");
  assert.equal(compactBounds.theme, "studio-light");
  await page.keyboard.press("Escape");
  await stepDialog.waitFor({ state: "detached" });
  await page.emulateMedia({ reducedMotion: "no-preference" });

  await changeTrigger.click();
  await changeDialog.waitFor({ state: "visible" });
  await changeDialog.locator(".session-change-row").first().click();
  const navigation = await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    const state = useAppStore.getState();
    return {
      currentTaskId: state.currentTaskId,
      canvasTab: state.canvasTab,
      openTabs: state.workbenches["mock-task-queue"]?.openTabs ?? [],
    };
  });
  assert.equal(navigation.currentTaskId, "mock-task-queue");
  assert.equal(navigation.canvasTab, "files");
  assert.ok(navigation.openTabs.includes("files"));
  assert.equal(await page.locator(".scene-room .timeline").count(), 1, "opening a file keeps the room mounted");
  assert.deepEqual(runtimeErrors, []);
  await page.close();
});

test("file receipts stay scoped to their run and move from the live strip into the completed turn", async () => {
  const page = await browser.newPage({ viewport: { width: 1024, height: 768 } });
  const runtimeErrors = [];
  page.on("pageerror", (error) => runtimeErrors.push(String(error)));
  page.on("console", (message) => {
    if (message.type() === "error") runtimeErrors.push(message.text());
  });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  await page.evaluate(async () => {
    const { browserMockDetails } = await import("/src/lib/mock-data.ts");
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    const taskId = "mock-task-queue";
    const detail = browserMockDetails[taskId];
    detail.changes.push({
      id: "previous-run-only-change",
      task_id: taskId,
      run_id: "previous-main-run",
      tool_call_id: null,
      path: "src/stale-from-previous-session.ts",
      change_type: "modify",
      before_hash: "previous-before",
      after_hash: "previous-after",
      old_path: null,
      created_at: "2026-01-01T00:00:00Z",
    });
    await Promise.all([
      useTasksStore.getState().refreshTasks(),
      useTasksStore.getState().refreshDetail(taskId),
      useTasksStore.getState().refreshWorkspaces(),
    ]);
    useAppStore.getState().openRoom(taskId);
  });

  const liveSummary = page.getByTestId("session-run-summary");
  await liveSummary.waitFor({ state: "visible" });
  const currentChanges = liveSummary.getByRole("button", { name: /2 个文件已更改/ });
  await currentChanges.click();
  let dialog = page.getByRole("dialog", { name: "当前对话的文件变更", exact: true });
  await dialog.waitFor({ state: "visible" });
  assert.equal(await dialog.locator(".session-change-row").count(), 2);
  assert.equal(await dialog.getByText("stale-from-previous-session.ts").count(), 0,
    "an older run in the same task must not leak into the live receipt");
  await page.keyboard.press("Escape");

  await page.evaluate(async () => {
    const { browserMockDetails, browserMockMessages, browserMockSetMessages, browserMockTasks } = await import("/src/lib/mock-data.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    const taskId = "mock-task-queue";
    const endedAt = new Date().toISOString();
    const detail = browserMockDetails[taskId];
    detail.task.state = "idle";
    detail.task.updated_at = endedAt;
    const task = browserMockTasks.find((item) => item.id === taskId);
    if (task) {
      task.state = "idle";
      task.updated_at = endedAt;
    }
    detail.runs = detail.runs.map((run) => ({
      ...run,
      ended_at: run.ended_at ?? endedAt,
      review_state: run.agent_kind === "main" ? "answered" : run.review_state,
    }));
    browserMockSetMessages(taskId, [
      ...browserMockMessages(taskId),
      {
        id: `${taskId}-run-final`,
        branch_id: detail.active_branch.id,
        kind: "message",
        role: "assistant",
        text: "本轮修改已经完成。",
        timestamp: endedAt,
      },
    ]);
    await useTasksStore.getState().refreshDetail(taskId);
  });

  await liveSummary.waitFor({ state: "detached" });
  const receipt = page.locator(".timeline-run-changes[data-run-id='mock-task-queue-run']");
  await receipt.waitFor({ state: "visible" });
  assert.match(await receipt.locator(".timeline-run-changes-head").innerText(), /已编辑 2 个文件/);
  assert.equal(await receipt.locator(".timeline-run-change-row").count(), 2);
  assert.equal(await receipt.getByText("stale-from-previous-session.ts").count(), 0,
    "the completed turn receipt must preserve the same run boundary");
  if (process.env.R_CODE_RUN_RECEIPT_SHOT) {
    await page.screenshot({ path: process.env.R_CODE_RUN_RECEIPT_SHOT, fullPage: true });
  }

  await page.evaluate(async () => {
    const { browserMockDetails, browserMockTasks } = await import("/src/lib/mock-data.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    const taskId = "mock-task-queue";
    const startedAt = new Date().toISOString();
    const detail = browserMockDetails[taskId];
    detail.task.state = "in_progress";
    detail.task.updated_at = startedAt;
    const task = browserMockTasks.find((item) => item.id === taskId);
    if (task) {
      task.state = "in_progress";
      task.updated_at = startedAt;
    }
    detail.runs.push({
      ...detail.runs.find((run) => run.agent_kind === "main"),
      id: "mock-task-queue-new-run",
      parent_run_id: null,
      summary: "开始下一轮修改",
      review_state: "pending",
      started_at: startedAt,
      ended_at: null,
    });
    detail.changes.push({
      id: "new-run-change",
      task_id: taskId,
      run_id: "mock-task-queue-new-run",
      tool_call_id: null,
      path: "src/current-run-only.ts",
      change_type: "create",
      before_hash: null,
      after_hash: "new-run-after",
      old_path: null,
      created_at: startedAt,
    });
    await useTasksStore.getState().refreshDetail(taskId);
  });

  const nextSummary = page.getByTestId("session-run-summary");
  await nextSummary.waitFor({ state: "visible" });
  const oneFile = nextSummary.getByRole("button", { name: /1 个文件已更改/ });
  await oneFile.click();
  dialog = page.getByRole("dialog", { name: "当前对话的文件变更", exact: true });
  await dialog.waitFor({ state: "visible" });
  assert.equal(await dialog.locator(".session-change-row").count(), 1);
  assert.equal(await dialog.getByText("current-run-only.ts").count(), 1);
  assert.deepEqual(runtimeErrors, []);
  await page.close();
});
