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
  const cache = path.join(process.env.LOCALAPPDATA ?? "", "ms-playwright");
  const cached = fs.existsSync(cache)
    ? fs.readdirSync(cache)
      .filter((entry) => /^chromium-\d+$/.test(entry))
      .sort((left, right) => Number(right.split("-")[1]) - Number(left.split("-")[1]))
      .map((entry) => path.join(cache, entry, "chrome-win64", "chrome.exe"))
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

test("activity shows task-level outcomes while archive owns restore and feed visibility", async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.getByRole("button", { name: "活动", exact: true }).click();

  const activity = page.locator(".activity-page");
  await activity.getByRole("heading", { name: "需要处理", exact: true }).waitFor({ state: "visible" });
  await activity.getByRole("heading", { name: "正在进行", exact: true }).waitFor({ state: "visible" });
  await activity.getByRole("heading", { name: "最近结束", exact: true }).waitFor({ state: "visible" });
  assert.equal(await activity.getByText(/调用了工具|收到了工具结果/).count(), 0, "raw tool events must not be the activity product");
  assert.equal(await activity.locator(".activity-recent-row").filter({ hasText: "更新依赖并修复告警" }).count(), 1);

  if (process.env.R_CODE_ACTIVITY_SHOT) {
    await page.screenshot({ path: process.env.R_CODE_ACTIVITY_SHOT, fullPage: true });
  }

  await page.evaluate(async () => {
    const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    await browserMockInvoke("cmd_task_archive", { taskId: "mock-task-complete" });
    await useTasksStore.getState().refreshTasks();
  });
  await page.waitForFunction(() => !document.querySelector(".activity-page")?.textContent?.includes("更新依赖并修复告警"));
  const archivedFeed = await page.evaluate(async () => {
    const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
    return browserMockInvoke("cmd_activity_list", { cursor: null, limit: 50 });
  });
  assert.equal(archivedFeed.items.some((item) => item.task_id === "mock-task-complete"), false, "archived task events must leave the global feed");

  await page.getByRole("button", { name: "归档", exact: true }).click();
  const archive = page.locator(".archive-page");
  const archivedRow = archive.locator(".archive-row").filter({ hasText: "更新依赖并修复告警" });
  await archivedRow.waitFor({ state: "visible" });
  await archivedRow.getByRole("button", { name: "还原", exact: true }).waitFor({ state: "visible" });

  if (process.env.R_CODE_ARCHIVE_SHOT) {
    await page.screenshot({ path: process.env.R_CODE_ARCHIVE_SHOT, fullPage: true });
  }

  await archivedRow.getByRole("button", { name: "还原", exact: true }).click();
  await page.getByText("对话已还原", { exact: true }).waitFor({ state: "visible" });
  await archivedRow.waitFor({ state: "detached" });

  await page.getByRole("button", { name: "活动", exact: true }).click();
  await activity.locator(".activity-recent-row").filter({ hasText: "更新依赖并修复告警" }).waitFor({ state: "visible" });
  await page.close();
});

test("partial success is presented as reviewable work with an explicit warning", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const result = await page.evaluate(async () => {
    const { PARTIAL_SUCCESS_RUN_SUMMARY, reviewAttentionDescription } = await import("/src/lib/presentation.ts");
    const { completionDetailReady, completionToast } = await import("/src/components/ui/Toast.tsx");
    const task = {
      id: "partial-task",
      title: "修复生成流程",
      goal: "修复生成流程",
      state: "review_ready",
    };
    const detail = {
      changes: [{ path: "src/example.ts" }],
      runs: [{
        agent_kind: "main",
        review_state: "pending",
        summary: PARTIAL_SUCCESS_RUN_SUMMARY,
        ended_at: "2026-08-13T00:00:00Z",
      }],
    };
    return {
      activity: reviewAttentionDescription(detail),
      toast: completionToast(task, detail),
      staleReady: completionDetailReady(
        { ...task, updated_at: "2026-08-13T00:00:01Z" },
        { ...detail, task: { ...task, updated_at: "2026-08-13T00:00:00Z" } },
      ),
      currentReady: completionDetailReady(
        { ...task, updated_at: "2026-08-13T00:00:01Z" },
        { ...detail, task: { ...task, updated_at: "2026-08-13T00:00:01Z" } },
      ),
      currentRunStillOpen: completionDetailReady(
        { ...task, updated_at: "2026-08-13T00:00:01Z" },
        {
          ...detail,
          task: { ...task, updated_at: "2026-08-13T00:00:01Z" },
          runs: [
            { agent_kind: "main", ended_at: null },
            { agent_kind: "main", ended_at: "2026-08-12T23:59:59Z" },
          ],
        },
      ),
    };
  });

  assert.match(result.activity, /修改存在但总结失败/);
  assert.match(result.activity, /1 个文件等待审核/);
  assert.equal(result.toast.kind, "warn");
  assert.match(result.toast.title, /修改待审阅/);
  assert.match(result.toast.body, /工作区改动已保留/);
  assert.equal(result.staleReady, false, "an older detail snapshot must not announce a new completion");
  assert.equal(result.currentReady, true);
  assert.equal(result.currentRunStillOpen, false, "an older ended run must not mask the current open run");
  await page.close();
});

test("completion toast waits for the matching detail snapshot before announcing", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.waitForFunction(async () => {
    const { useTasksStore } = await import("/src/store/tasks.ts");
    return Object.keys(useTasksStore.getState().details).length > 0;
  });

  const taskId = await page.evaluate(async () => {
    const { PARTIAL_SUCCESS_RUN_SUMMARY } = await import("/src/lib/presentation.ts");
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    const { useToastStore } = await import("/src/store/toast.ts");
    const state = useTasksStore.getState();
    const templateDetail = Object.values(state.details)[0];
    const templateTask = templateDetail.task;
    const id = "toast-detail-race";
    const activeTask = {
      ...templateTask,
      id,
      title: "Toast 分批竞态",
      goal: "Toast 分批竞态",
      state: "in_progress",
      updated_at: "2026-08-13T00:00:01Z",
    };
    const staleDetail = {
      ...templateDetail,
      task: activeTask,
      runs: [{
        ...templateDetail.runs[0],
        id: `${id}-old-run`,
        task_id: id,
        agent_kind: "main",
        ended_at: "2026-08-13T00:00:00Z",
      }],
    };
    useAppStore.setState({ scene: "settings", currentTaskId: null });
    useToastStore.setState({ toasts: [] });
    useTasksStore.setState({
      tasks: [...state.tasks, activeTask],
      details: { ...state.details, [id]: staleDetail },
    });

    const reviewTask = {
      ...activeTask,
      state: "review_ready",
      updated_at: "2026-08-13T00:00:02Z",
    };
    useTasksStore.setState((current) => ({
      tasks: current.tasks.map((task) => task.id === id ? reviewTask : task),
    }));

    // Publish the matching detail on a later turn, as the real task/detail pollers do.
    // 延迟留足余量：慢速 CI runner 上「evaluate 返回 → waitForTimeout(60) → 二次检查」
    // 的 wall-clock 可能超过 120ms，导致 matching detail 提前发布、early 检查误判。
    window.setTimeout(() => {
      useTasksStore.setState((current) => ({
        details: {
          ...current.details,
          [id]: {
            ...staleDetail,
            task: reviewTask,
            runs: [{
              ...staleDetail.runs[0],
              id: `${id}-current-run`,
              summary: PARTIAL_SUCCESS_RUN_SUMMARY,
              review_state: "pending",
              ended_at: "2026-08-13T00:00:02Z",
            }],
          },
        },
      }));
    }, 500);
    return id;
  });

  await page.waitForTimeout(60);
  const early = await page.evaluate(async () => {
    const { useToastStore } = await import("/src/store/toast.ts");
    return useToastStore.getState().toasts.some((toast) => toast.title.includes("Toast 分批竞态"));
  });
  assert.equal(early, false, "stale detail must not produce an early green success toast");
  const announced = page.locator(".toast--warn").filter({ hasText: "Toast 分批竞态" });
  await announced.waitFor({ state: "visible" });
  await announced.getByRole("button", { name: "审阅改动", exact: true }).waitFor({ state: "visible" });
  assert.equal(taskId, "toast-detail-race");
  await page.close();
});

test.skip("legacy in-window companion behavior was replaced by a native companion window", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    const { useCompanionStore } = await import("/src/store/companion.ts");
    const { useToastStore } = await import("/src/store/toast.ts");
    useCompanionStore.getState().setEnabled(true);
    useCompanionStore.getState().setMinimized(false);
    useCompanionStore.getState().setSoundEnabled(false);
    useCompanionStore.getState().setMotion("full");
    useCompanionStore.getState().setPosition({ edge: "right", y: 1 });
    useToastStore.setState({ toasts: [] });
  });

  const companion = page.locator(".companion-host");
  await companion.waitFor({ state: "visible" });
  assert.equal(await companion.locator(".companion-character").count(), 1);
  const homeMetrics = await page.evaluate(() => {
    const host = document.querySelector(".companion-host");
    const actions = document.querySelector(".scene-home .composer-actions");
    if (!(host instanceof HTMLElement) || !(actions instanceof HTMLElement)) return null;
    const companionBox = host.getBoundingClientRect();
    const actionsBox = actions.getBoundingClientRect();
    return {
      companionLeft: companionBox.left,
      actionsRight: actionsBox.right,
      animationName: getComputedStyle(host.querySelector(".companion-character")).animationName,
    };
  });
  assert.ok(homeMetrics && homeMetrics.companionLeft >= homeMetrics.actionsRight,
    "the floating Home companion must not cover composer actions");
  assert.notEqual(homeMetrics.animationName, "none", "full motion should animate the companion continuously");

  const dragHandle = companion.getByRole("button", { name: "移动 R-Code 小助手", exact: true });
  const handleBox = await dragHandle.boundingBox();
  assert.ok(handleBox, "drag handle must be measurable");
  await page.mouse.move(handleBox.x + handleBox.width / 2, handleBox.y + handleBox.height / 2);
  await page.mouse.down();
  await page.mouse.move(38, 210, { steps: 8 });
  await page.mouse.up();
  await page.waitForFunction(() => document.querySelector(".companion-host")?.classList.contains("edge-left"));
  const persisted = await page.evaluate(() => JSON.parse(
    window.localStorage.getItem("r-code.companion.position") ?? "null",
  ));
  assert.equal(persisted.edge, "left");
  assert.ok(persisted.y > 0 && persisted.y < 1, "dragged vertical position must persist as a ratio");
  const draggedBox = await companion.boundingBox();
  assert.ok(draggedBox && draggedBox.x >= 0 && draggedBox.y >= 0
    && draggedBox.x + draggedBox.width <= 1200 && draggedBox.y + draggedBox.height <= 800,
  "dragged companion must stay inside the viewport");

  await page.reload({ waitUntil: "networkidle" });
  await companion.waitFor({ state: "visible" });
  assert.equal(await companion.evaluate((element) => element.classList.contains("edge-left")), true,
    "the snapped edge must survive a reload");
  const reloadedBox = await companion.boundingBox();
  assert.ok(reloadedBox && Math.abs(reloadedBox.y - draggedBox.y) < 3,
    "the relative vertical position must survive a reload");

  await dragHandle.press("End");
  await page.waitForFunction(() => document.querySelector(".companion-host")?.classList.contains("edge-right"));
  await dragHandle.press("Home");
  await page.waitForFunction(() => document.querySelector(".companion-host")?.classList.contains("edge-left"));

  await page.evaluate(() => {
    const handle = document.querySelector(".companion-drag-handle");
    window.__companionPointerId = null;
    handle?.addEventListener("pointerdown", (event) => {
      window.__companionPointerId = event.pointerId;
    }, { once: true });
  });
  const recaptureBox = await dragHandle.boundingBox();
  assert.ok(recaptureBox);
  await page.mouse.move(recaptureBox.x + recaptureBox.width / 2, recaptureBox.y + recaptureBox.height / 2);
  await page.mouse.down();
  await page.mouse.move(260, 310, { steps: 6 });
  await page.waitForFunction(() => document.querySelector(".companion-host")?.classList.contains("is-dragging"));
  await page.evaluate(() => {
    const handle = document.querySelector(".companion-drag-handle");
    if (handle?.hasPointerCapture(window.__companionPointerId)) {
      handle.releasePointerCapture(window.__companionPointerId);
    }
  });
  await page.waitForFunction(() => !document.querySelector(".companion-host")?.classList.contains("is-dragging"));
  await page.mouse.up();

  await page.emulateMedia({ reducedMotion: "reduce" });
  const reducedAnimation = await companion.locator(".companion-character").evaluate((element) =>
    getComputedStyle(element).animationName
  );
  assert.equal(reducedAnimation, "none", "the OS reduced-motion preference must suppress pet animation");
  await page.emulateMedia({ reducedMotion: "no-preference" });

  await page.getByRole("button", { name: "活动", exact: true }).click();
  await page.locator(".activity-page").waitFor({ state: "visible" });
  assert.equal(await companion.evaluate((element) => element.classList.contains("is-minimized")), false);
  assert.equal(await companion.locator(".companion-character").count(), 1);

  const layerOrder = await page.evaluate(() => {
    const host = document.querySelector(".companion-host");
    const modal = document.createElement("div");
    modal.className = "confirm-backdrop";
    document.body.append(modal);
    const result = {
      companion: Number(getComputedStyle(host).zIndex),
      modal: Number(getComputedStyle(modal).zIndex),
    };
    modal.remove();
    return result;
  });
  assert.ok(layerOrder.companion < layerOrder.modal, "the companion must stay below modal content");

  const modalDragHandleBox = await dragHandle.boundingBox();
  assert.ok(modalDragHandleBox);
  await page.mouse.move(
    modalDragHandleBox.x + modalDragHandleBox.width / 2,
    modalDragHandleBox.y + modalDragHandleBox.height / 2,
  );
  await page.mouse.down();
  await page.mouse.move(430, 260, { steps: 6 });
  await page.waitForFunction(() => document.querySelector(".companion-host")?.classList.contains("is-dragging"));

  await page.evaluate(() => {
    const modal = document.createElement("section");
    modal.id = "companion-modal-boundary-test";
    modal.setAttribute("role", "dialog");
    modal.setAttribute("aria-modal", "true");
    document.body.append(modal);
  });
  await companion.waitFor({ state: "detached" });
  await page.mouse.up();
  await page.evaluate(() => document.querySelector("#companion-modal-boundary-test")?.remove());
  await companion.waitFor({ state: "visible" });
  assert.equal(await companion.evaluate((element) => element.classList.contains("is-dragging")), false,
    "opening a modal mid-drag must clear pointer and draft position state");

  const cuePolicy = await page.evaluate(async () => {
    const { shouldPlayCompanionCue } = await import("/src/components/companion/policy.ts");
    return {
      duplicateReview: shouldPlayCompanionCue("success", "review"),
      firstSuccess: shouldPlayCompanionCue("working", "success"),
      unchanged: shouldPlayCompanionCue("attention", "attention"),
    };
  });
  assert.deepEqual(cuePolicy, { duplicateReview: false, firstSuccess: true, unchanged: false });

  await page.setViewportSize({ width: 640, height: 480 });
  await page.evaluate(async () => {
    const { useCompanionStore } = await import("/src/store/companion.ts");
    useCompanionStore.getState().setPosition({ edge: "left", y: 0 });
  });
  const compact = page.locator(".companion-host.is-responsive-compact");
  await compact.waitFor({ state: "visible" });
  await page.waitForFunction(() => {
    const host = document.querySelector(".companion-host");
    const topbar = document.querySelector(".app-topbar");
    const rect = host?.getBoundingClientRect();
    const topbarRect = topbar?.getBoundingClientRect();
    return rect && topbarRect && rect.left < 20 && rect.top >= topbarRect.bottom + 10;
  });
  const topbarClearance = await page.evaluate(() => {
    const topbar = document.querySelector(".app-topbar").getBoundingClientRect();
    const host = document.querySelector(".companion-host").getBoundingClientRect();
    const handle = document.querySelector(".companion-drag-handle").getBoundingClientRect();
    return { topbarBottom: topbar.bottom, hostTop: host.top, handleTop: handle.top };
  });
  assert.ok(topbarClearance.hostTop > topbarClearance.topbarBottom);
  assert.ok(topbarClearance.handleTop >= topbarClearance.topbarBottom,
    "the companion drag handle must never cover native window controls or the titlebar drag region");
  assert.equal(await compact.locator(".companion-character").count(), 0);
  const compactButton = compact.getByRole("button", { name: /查看 R-Code 小助手状态/ });
  assert.equal(await compactButton.getAttribute("aria-controls"), "r-code-companion-status");
  await compactButton.click();
  const panel = compact.locator("#r-code-companion-status");
  await panel.waitFor({ state: "visible" });
  assert.equal(await compactButton.getAttribute("aria-expanded"), "true");
  const actionHeights = await panel.locator(".companion-panel-actions button").evaluateAll((buttons) =>
    buttons.map((button) => button.getBoundingClientRect().height)
  );
  assert.ok(actionHeights.every((height) => height >= 24), "panel controls need accessible targets");
  let panelBox = await panel.boundingBox();
  assert.ok(panelBox && panelBox.x >= 0 && panelBox.x + panelBox.width <= 640
    && panelBox.y >= 0 && panelBox.y + panelBox.height <= 480,
    "top-docked compact panel must expand into the viewport");
  await page.evaluate(async () => {
    const { useCompanionStore } = await import("/src/store/companion.ts");
    useCompanionStore.getState().setPosition({ edge: "right", y: 1 });
  });
  await page.waitForFunction(() => {
    const host = document.querySelector(".companion-host");
    const rect = host?.getBoundingClientRect();
    return host?.classList.contains("panel-above") && host.classList.contains("edge-right")
      && rect && rect.right > 620 && rect.bottom > 460;
  });
  panelBox = await panel.boundingBox();
  assert.ok(panelBox && panelBox.x >= 0 && panelBox.x + panelBox.width <= 640
    && panelBox.y >= 0 && panelBox.y + panelBox.height <= 480,
    `bottom-docked compact panel must expand upward into the viewport: ${JSON.stringify(panelBox)}`);
  await page.keyboard.press("Escape");
  await panel.waitFor({ state: "detached" });

  await page.evaluate(async () => {
    const { pushToast } = await import("/src/store/toast.ts");
    for (let index = 1; index <= 4; index += 1) {
      pushToast({
        kind: "error",
        title: `持久通知 ${index}`,
        body: "这是一条用于验证短窗口滚动与可达性的较长通知正文。".repeat(4),
      });
    }
  });
  const toastHost = page.locator(".toast-host");
  await page.waitForFunction(() => {
    const host = document.querySelector(".toast-host");
    return host && host.scrollHeight > host.clientHeight && host.scrollTop > 0;
  });
  const toastMetrics = await toastHost.evaluate((host) => ({
    ariaLabel: host.getAttribute("aria-label"),
    overflowY: getComputedStyle(host).overflowY,
    clientHeight: host.clientHeight,
    scrollHeight: host.scrollHeight,
    scrollTop: host.scrollTop,
    bottom: host.getBoundingClientRect().bottom,
  }));
  assert.equal(toastMetrics.ariaLabel, "通知");
  assert.equal(toastMetrics.overflowY, "auto");
  assert.ok(toastMetrics.scrollHeight > toastMetrics.clientHeight);
  assert.ok(toastMetrics.scrollTop > 0);
  assert.ok(toastMetrics.bottom <= 480);
  const newestToast = await toastHost.locator(".toast").last().boundingBox();
  assert.ok(newestToast && newestToast.y >= 0 && newestToast.y + newestToast.height <= 480);
  await page.close();
});

test.skip("legacy in-window Settings behavior was replaced by cross-window synchronization", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    const { useCompanionStore } = await import("/src/store/companion.ts");
    useCompanionStore.getState().setEnabled(false);
    useCompanionStore.getState().setPosition({ edge: "left", y: 0 });
    useAppStore.getState().setSettingsPane("preferences");
  });

  const settingsPane = page.getByRole("button", { name: "外观与小助手", exact: true });
  await settingsPane.waitFor({ state: "visible" });
  const visibility = page.getByRole("switch", { name: "显示小助手", exact: true });
  assert.equal(await visibility.isChecked(), false);
  assert.equal(await page.locator(".companion-host").count(), 0);

  await visibility.click();
  const companion = page.locator(".companion-host");
  await companion.waitFor({ state: "visible" });
  assert.equal(await visibility.isChecked(), true);
  assert.equal(await companion.locator(".companion-character").count(), 1);

  await page.getByRole("radio", { name: "迷你宠物", exact: true }).click();
  await page.locator(".companion-host.is-minimized").waitFor({ state: "visible" });
  assert.equal(await companion.locator(".companion-compact-sprite").count(), 1);

  await page.getByRole("radio", { name: "完整宠物", exact: true }).click();
  await companion.locator(".companion-character").waitFor({ state: "visible" });
  await page.getByRole("button", { name: "恢复右下角", exact: true }).click();
  await page.waitForFunction(() => document.querySelector(".companion-host")?.classList.contains("edge-right"));
  const resetPosition = await page.evaluate(() => JSON.parse(
    window.localStorage.getItem("r-code.companion.position") ?? "null",
  ));
  assert.deepEqual(resetPosition, { edge: "right", y: 1 });
  await page.close();
});
