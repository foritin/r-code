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

test("Plan workbench stays scoped to the current project task and exposes an empty state", async () => {
  const page = await browser.newPage({ viewport: { width: 1320, height: 820 } });
  const runtimeErrors = [];
  page.on("pageerror", (error) => runtimeErrors.push(String(error)));
  page.on("console", (message) => {
    if (message.type() === "error") runtimeErrors.push(message.text());
  });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const ids = await page.evaluate(async () => {
    const [{ taskCreate, planCreate }, { useAppStore }, { useTasksStore }] = await Promise.all([
      import("/src/lib/ipc.ts"),
      import("/src/store/app.ts"),
      import("/src/store/tasks.ts"),
    ]);
    const planned = await taskCreate(
      "D:/project/rust/r-code",
      "项目 A 的计划",
      "PROJECT-A-PLAN-GOAL",
      "plan",
      "openai",
      "r_code",
    );
    await planCreate(planned.id);
    const empty = await taskCreate(
      "D:/project/rust/api-server",
      "项目 B 的普通任务",
      "PROJECT-B-GOAL",
      "edit",
      "openai",
      "r_code",
    );
    await Promise.all([
      useTasksStore.getState().refreshTasks(),
      useTasksStore.getState().refreshWorkspaces(),
    ]);
    useTasksStore.getState().setCurrentProject("D:/project/rust/r-code");
    useAppStore.getState().openRoom(planned.id, "plan");
    return { plannedId: planned.id, emptyId: empty.id };
  });

  const panel = page.getByRole("region", { name: "当前计划" });
  await panel.locator(".plan-goal").filter({ hasText: "PROJECT-A-PLAN-GOAL" }).waitFor();
  assert.equal(await panel.getAttribute("data-task-id"), ids.plannedId);

  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().showWorkbenchLauncher();
  });
  const launcher = page.getByRole("dialog", { name: "工作台工具启动器" });
  await launcher.getByRole("button", { name: /^计划/ }).waitFor();

  await page.evaluate(async (emptyId) => {
    const [{ useAppStore }, { useTasksStore }] = await Promise.all([
      import("/src/store/app.ts"),
      import("/src/store/tasks.ts"),
    ]);
    useTasksStore.getState().setCurrentProject("D:/project/rust/api-server");
    useAppStore.getState().openRoom(emptyId, "plan");
  }, ids.emptyId);

  await panel.getByText("当前对话没有计划", { exact: true }).waitFor();
  assert.equal(await panel.getAttribute("data-task-id"), ids.emptyId);
  assert.equal(await panel.locator(".plan-goal").count(), 0);

  await page.evaluate(async (plannedId) => {
    const [{ useAppStore }, { useTasksStore }] = await Promise.all([
      import("/src/store/app.ts"),
      import("/src/store/tasks.ts"),
    ]);
    useTasksStore.getState().setCurrentProject("D:/project/rust/r-code");
    useAppStore.getState().openRoom(plannedId, "plan");
  }, ids.plannedId);
  await panel.locator(".plan-goal").filter({ hasText: "PROJECT-A-PLAN-GOAL" }).waitFor();
  assert.equal(await panel.getAttribute("data-task-id"), ids.plannedId);
  assert.deepEqual(runtimeErrors, []);
  await page.close();
});

test("daemon Plan projections expose exact approval without legacy cancel or retry actions", async () => {
  const page = await browser.newPage({ viewport: { width: 1320, height: 820 } });
  const revisionHash = `sha256:${"a".repeat(64)}`;
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const taskId = await page.evaluate(async ({ revisionHash }) => {
    const [{ taskCreate }, { useAppStore }, { useTasksStore }] = await Promise.all([
      import("/src/lib/ipc.ts"),
      import("/src/store/app.ts"),
      import("/src/store/tasks.ts"),
    ]);
    const task = await taskCreate(
      "D:/project/rust/r-code",
      "Daemon Plan projection",
      "Approve the exact daemon revision",
      "plan",
      "openai",
      "r_code",
    );
    const now = "2026-09-26T00:00:00.000Z";
    globalThis.__rCodeBrowserMockPlanOverride = {
      [task.id]: {
        plan: {
          id: revisionHash,
          task_id: task.id,
          revision: 1,
          state: "ready",
          approved_revision: null,
          projection_path: null,
          projection_revision: null,
          projection_error: null,
          created_at: now,
          updated_at: now,
          approved_at: null,
          implementation_dispatch_state: "not_requested",
          implementation_dispatch_error: null,
          implementation_queue_message_id: null,
          implementation_dispatched_at: null,
        },
        goal: {
          task_id: task.id,
          goal: "Approve the exact daemon revision",
          updated_at: now,
        },
        items: [
          {
            id: "inspect",
            plan_id: revisionHash,
            revision: 1,
            ordinal: 0,
            title: "Inspect the checkout",
            description: "Inspect the checkout",
            section_path: [],
            state: "proposed",
            depends_on: [],
            created_at: now,
            updated_at: now,
            started_at: null,
            completed_at: null,
          },
          {
            id: "implement",
            plan_id: revisionHash,
            revision: 1,
            ordinal: 1,
            title: "Implement the PRD",
            description: "Implement the PRD",
            section_path: [],
            state: "proposed",
            depends_on: ["inspect"],
            created_at: now,
            updated_at: now,
            started_at: null,
            completed_at: null,
          },
        ],
        pending_question_set: null,
        continuation_question_set: null,
      },
    };
    await useTasksStore.getState().refreshDetail(task.id);
    await useTasksStore.getState().refreshTasks();
    useTasksStore.getState().setCurrentProject("D:/project/rust/r-code");
    useAppStore.getState().openRoom(task.id, "plan");
    return task.id;
  }, { revisionHash });

  const directPlan = await page.evaluate(async (taskId) => {
    const { planGet } = await import("/src/lib/ipc.ts");
    return planGet(taskId);
  }, taskId);
  assert.equal(directPlan.plan.id, revisionHash);
  assert.equal(directPlan.goal.goal, "Approve the exact daemon revision");
  await page.locator("#main-content > .scene-room").waitFor({ state: "visible" });
  const panel = page.getByRole("region", { name: "当前计划" });
  await panel.waitFor({ state: "visible" });
  await page.waitForTimeout(2_200);
  assert.match(await panel.innerText(), /Approve the exact daemon revision/);
  const confirm = panel.getByRole("button", { name: "确认", exact: true });
  await confirm.waitFor({ state: "visible" });
  assert.equal(
    await panel.getByRole("button", { name: "取消", exact: true }).count(),
    0,
    "daemon Plan must not expose cmd_plan_cancel from the old database",
  );
  assert.equal(await panel.getByText("重试实施", { exact: true }).count(), 0);
  assert.equal(await panel.getByText("继续当前功能", { exact: true }).count(), 0);

  await page.evaluate((taskId) => {
    const view = globalThis.__rCodeBrowserMockPlanOverride[taskId];
    view.plan.state = "approved";
    view.plan.approved_revision = view.plan.revision;
    view.items.forEach((item) => { item.state = "pending"; });
  }, taskId);
  await panel.getByRole("button", { name: "刷新计划" }).click();
  await page.waitForFunction(() => document.querySelector(".plan-panel")?.classList.contains("state-approved"));
  assert.equal(
    await panel.getByRole("button", { name: "取消", exact: true }).count(),
    0,
    "approved daemon Plan must still hide the legacy cancel path",
  );
  assert.equal(await panel.getByText("重试实施", { exact: true }).count(), 0);
  await page.close();
});
