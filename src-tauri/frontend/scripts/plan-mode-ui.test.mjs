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

test("Plan outline keeps visual leaf order identical to execution order", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const outline = await page.evaluate(async () => {
    const { planOutline } = await import("/src/components/plan/PlanPanel.tsx");
    const make = (id, title, section_path) => ({
      id,
      title,
      description: "",
      section_path,
      state: "pending",
      depends_on: [],
    });
    return planOutline([
      make("first", "First", ["Backend"]),
      make("root", "Root", []),
      make("last", "Last", ["Backend"]),
    ]).map((entry) => entry.kind === "item"
      ? { kind: "item", id: entry.item.id, number: entry.number }
      : { kind: "section", title: entry.title, number: entry.number });
  });

  assert.deepEqual(outline, [
    { kind: "section", title: "Backend", number: "1" },
    { kind: "item", id: "first", number: "1.1" },
    { kind: "item", id: "root", number: "2" },
    { kind: "section", title: "Backend", number: "3" },
    { kind: "item", id: "last", number: "3.1" },
  ]);
  await page.close();
});

test("Plan mode carries Goal into the task, asks per-question HITL, and approves feature todos", async () => {
  const page = await browser.newPage({ viewport: { width: 1180, height: 820 } });
  const runtimeErrors = [];
  page.on("pageerror", (error) => runtimeErrors.push(String(error)));
  page.on("console", (message) => {
    if (message.type() === "error") runtimeErrors.push(message.text());
  });

  await page.goto(baseUrl, { waitUntil: "networkidle" });

  // The ordinary first prompt is task context, not an explicitly configured Goal. Existing
  // conversations must therefore upgrade without inventing a Goal lifecycle the user never set.
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().openRoom("mock-task-queue");
  });
  await page.locator(".scene.scene-room").waitFor({ state: "visible" });
  assert.equal(await page.getByRole("region", { name: "当前目标" }).count(), 0);
  assert.equal(await page.locator(".sum-goal").count(), 0, "ordinary task context must not render as an explicit Goal");
  await page.getByRole("button", { name: "添加到任务", exact: true }).click();
  const ordinaryTaskGoalItem = page.getByRole("dialog", { name: "添加到任务" }).getByRole("button", { name: /^目标/ });
  assert.doesNotMatch(await ordinaryTaskGoalItem.innerText(), /已设置|进行中/);
  await page.keyboard.press("Escape");
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().goHome();
  });
  await page.locator(".scene.scene-home").waitFor({ state: "visible" });

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
  assert.equal(await addDialog.locator("textarea").count(), 0, "Goal must not open a second composer");
  await addDialog.getByRole("button", { name: /目标/ }).click();
  await addDialog.waitFor({ state: "hidden" });
  const goalModeChip = page.getByRole("button", { name: "退出目标模式", exact: true });
  await goalModeChip.waitFor({ state: "visible" });
  await goalModeChip.hover();
  assert.equal(await goalModeChip.locator(".goal-mode-close").isVisible(), true, "Goal hover must expose the × affordance");
  await goalModeChip.click();
  await page.getByLabel("描述新任务").waitFor();

  // Plan is an interaction mode. Goal remains the main composer input and its send action starts
  // the task immediately; there is no intermediate "save goal, then write another prompt" step.
  await page.getByRole("button", { name: "添加到任务", exact: true }).click();
  await addDialog.getByRole("button", { name: /^计划模式/ }).click();
  await page.getByRole("button", { name: "添加到任务", exact: true }).click();
  await addDialog.getByRole("button", { name: /目标/ }).click();
  await page.getByLabel("任务目标", { exact: true }).fill("交付一个可逐功能验收的计划模式；先澄清边界，再生成实施计划");
  await page.getByRole("button", { name: "执行目标", exact: true }).click();
  await page.locator(".scene.scene-room").waitFor({ state: "visible" });
  assert.equal(await page.evaluate(async () => {
    const [{ useAppStore }, { useTasksStore }] = await Promise.all([
      import("/src/store/app.ts"),
      import("/src/store/tasks.ts"),
    ]);
    const taskId = useAppStore.getState().currentTaskId;
    return taskId ? useTasksStore.getState().details[taskId]?.task.goal_active : null;
  }), true);

  const panel = page.getByRole("region", { name: "当前计划" });
  await panel.waitFor({ state: "visible" });
  assert.equal(await page.getByTestId("workbench-panel").getAttribute("data-workbench-section"), "plan");
  assert.equal(await page.getByRole("tab").filter({ hasText: "计划" }).getAttribute("aria-selected"), "true");
  assert.equal(await page.locator(".convo > .plan-panel").count(), 0, "the full Plan must not occupy the conversation column");
  assert.match(await panel.locator(".plan-goal").innerText(), /逐功能验收/);
  assert.match(await panel.locator(".plan-metadata").innerText(), /demo-plan-.*\/plan\.md/);
  await page.getByText("在整理计划前还需要你确认两项边界", { exact: false }).waitFor();

  const questions = page.getByRole("group", { name: "计划需要你的回答" });
  await questions.getByLabel(/聚焦核心流程/).check();
  await questions.getByLabel(/两者结合/).check();
  await questions.getByRole("button", { name: "提交回答", exact: true }).click();

  await panel.getByRole("button", { name: "确认", exact: true }).waitFor();
  assert.equal(await panel.getByText(/回答已接纳/).count(), 0, "ready Plan state should replace redundant success notices");
  const sections = panel.locator(".plan-feature-list > .plan-outline-section");
  const features = panel.locator(".plan-feature-list > li:not(.plan-outline-section)");
  assert.equal(await sections.count(), 2);
  assert.equal(await features.count(), 2);
  assert.equal(await sections.first().locator(".plan-feature-index").innerText(), "1");
  assert.equal(await features.first().locator(".plan-feature-index").innerText(), "1.1");
  assert.equal(await sections.nth(1).locator(".plan-feature-index").innerText(), "2");
  assert.equal(await features.nth(1).locator(".plan-feature-index").innerText(), "2.1");
  assert.equal(await features.first().locator(".plan-feature-details").isVisible(), false);
  await features.nth(1).getByRole("button", { name: /展开功能 2\.1/ }).click();
  assert.match(await features.nth(1).innerText(), /依赖：明确实现边界/);
  await features.nth(1).getByRole("button", { name: /收起功能 2\.1/ }).click();

  await sections.first().getByRole("button", { name: /收起阶段 1/ }).click();
  assert.equal(await features.count(), 1, "collapsing a section must hide its descendant features");
  await sections.first().getByRole("button", { name: /展开阶段 1/ }).click();
  assert.equal(await features.count(), 2);

  const planToggle = panel.getByRole("button", { name: "收起计划", exact: true });
  await planToggle.click();
  assert.equal(await panel.locator(".plan-panel-body").isVisible(), false);
  await panel.getByRole("button", { name: "展开计划", exact: true }).click();
  assert.equal(await panel.locator(".plan-panel-body").isVisible(), true);
  assert.match(await panel.getByLabel("计划进度明细").innerText(), /完成 0.*进行中 0.*待处理 2/s);
  const decisionButtons = panel.locator(".plan-decision-actions > button");
  const [cancelBox, confirmBox] = await Promise.all([
    decisionButtons.filter({ hasText: /^取消$/ }).boundingBox(),
    decisionButtons.filter({ hasText: /^确认$/ }).boundingBox(),
  ]);
  assert.ok(cancelBox && confirmBox && Math.abs(cancelBox.y - confirmBox.y) <= 1, "Confirm and Cancel must stay on one row");
  await panel.getByRole("button", { name: "确认", exact: true }).click();
  await panel.getByText("实施中", { exact: false }).first().waitFor();
  assert.match(await features.first().innerText(), /进行中/);
  assert.match(await panel.getByLabel("计划进度明细").innerText(), /完成 0.*进行中 1.*待处理 1/s);

  // Parallel children belong to the current feature by its durable started_at boundary. The Plan
  // shows every live task at once and opens the existing child session directly, without an extra
  // intermediary card.
  const planSubagentIds = await page.evaluate(async () => {
    const [{ planGet }, { browserMockDetails }, { useAppStore }, { useTasksStore }] = await Promise.all([
      import("/src/lib/ipc.ts"),
      import("/src/lib/mock-data.ts"),
      import("/src/store/app.ts"),
      import("/src/store/tasks.ts"),
    ]);
    const taskId = useAppStore.getState().currentTaskId;
    if (!taskId) throw new Error("Room task is missing");
    const plan = await planGet(taskId);
    const active = plan?.items.find((item) => item.state === "in_progress");
    const detail = browserMockDetails[taskId];
    if (!active?.started_at || !detail) throw new Error("Active Plan feature is missing");
    const parentRunId = detail.runs.find((run) => run.agent_kind === "main")?.id ?? null;
    const startedAt = active.started_at;
    const completedAt = new Date(Date.parse(startedAt) + 1_000).toISOString();
    const makeRun = (
      id,
      label,
      runtimeKind,
      summary,
      endedAt = null,
      reviewState = "pending",
      runStartedAt = startedAt,
    ) => ({
      id,
      task_id: taskId,
      branch_id: detail.active_branch.id,
      parent_run_id: parentRunId,
      agent_kind: "subagent",
      agent_label: label,
      summary,
      delegated_by_tool_call_id: `delegate-${id}`,
      model: runtimeKind === "native" ? "gpt-5.6-terra" : "gpt-5.6-sol",
      runtime_kind: runtimeKind,
      access_mode: "read_only",
      require_approval: false,
      routing_reason: "Plan 并行验证",
      external_session_id: null,
      review_state: reviewState,
      started_at: runStartedAt,
      ended_at: endedAt,
      usage_json: null,
    });
    const ids = {
      native: `${taskId}-plan-native`,
      codex: `${taskId}-plan-codex`,
      completed: `${taskId}-plan-completed`,
      historical: `${taskId}-before-plan-feature`,
    };
    const historicalStartedAt = new Date(Date.parse(startedAt) - 60_000).toISOString();
    const historicalEndedAt = new Date(Date.parse(startedAt) - 30_000).toISOString();
    detail.runs.push(
      makeRun(ids.historical, "Codex CLI · 上一阶段检查", "codex_exec", "早于当前功能", historicalEndedAt, "answered", historicalStartedAt),
      makeRun(ids.native, "R-Code · 核对状态机", "native", "正在核对 revision 交接"),
      makeRun(ids.codex, "Codex CLI · 验证并发调用", "codex_exec", "正在验证并行工具调用"),
      makeRun(ids.completed, "R-Code · 检查迁移", "native", "迁移结构检查完成", completedAt, "answered"),
    );
    await useTasksStore.getState().refreshDetail(taskId);
    return ids;
  });
  const cluster = panel.getByTestId("plan-subagent-cluster");
  await cluster.waitFor({ state: "visible" });
  assert.match(await cluster.innerText(), /并行执行 2\/3/);
  assert.equal(await cluster.locator(".plan-subagent-row").count(), 3);
  assert.equal(await cluster.locator(".plan-subagent-row.status-running").count(), 2);
  assert.equal(await cluster.locator(".plan-subagent-row.status-completed").count(), 1);
  assert.equal(await cluster.locator(".subagent-avatar.runtime-rcode").count(), 2);
  assert.equal(await cluster.locator(".subagent-avatar.runtime-codex").count(), 1);
  await cluster.locator(`[data-subagent-id="${planSubagentIds.codex}"]`).click();
  await page.getByTestId("subagent-detail").waitFor({ state: "visible" });
  assert.equal(await page.getByTestId("subagent-detail").getAttribute("data-subagent-view"), "detail");
  assert.equal(
    await page.locator(`.subagent-session-tab [data-subagent-id="${planSubagentIds.codex}"]`).count(),
    1,
    "Plan row must open one stable existing child Tab",
  );
  await page.getByRole("tab", { name: "计划", exact: true }).click();
  await panel.waitFor({ state: "visible" });
  await page.evaluate(async ({ ids }) => {
    const [{ browserMockDetails }, { useAppStore }, { useTasksStore }] = await Promise.all([
      import("/src/lib/mock-data.ts"),
      import("/src/store/app.ts"),
      import("/src/store/tasks.ts"),
    ]);
    const taskId = useAppStore.getState().currentTaskId;
    if (!taskId) throw new Error("Room task is missing");
    const detail = browserMockDetails[taskId];
    const endedAt = new Date().toISOString();
    for (const run of detail.runs) {
      if (![ids.native, ids.codex].includes(run.id)) continue;
      run.ended_at = endedAt;
      run.review_state = "answered";
      run.summary = `${run.summary}，检查完成`;
    }
    await useTasksStore.getState().refreshDetail(taskId);
  }, { ids: planSubagentIds });
  await page.waitForFunction(() => document.querySelector("[data-testid='plan-subagent-cluster']")?.textContent?.includes("并行执行已完成 · 3"));
  assert.equal(await cluster.locator(".plan-subagent-toggle").getAttribute("aria-expanded"), "false");
  assert.equal(await cluster.locator(".plan-subagent-rows").isVisible(), false, "all-completed children default to a compact summary");
  await cluster.locator(".plan-subagent-toggle").click();
  assert.equal(await cluster.locator(".plan-subagent-rows").isVisible(), true);

  // Approval switches the task back to auto mode. Leaving and reopening the
  // room must still discover the durable Plan instead of hiding the panel.
  const approvedTaskId = await page.evaluate(async () => {
    const [{ useAppStore }, { useTasksStore }] = await Promise.all([
      import("/src/store/app.ts"),
      import("/src/store/tasks.ts"),
    ]);
    const taskId = useAppStore.getState().currentTaskId;
    if (!taskId) throw new Error("Room task is missing");
    const task = useTasksStore.getState().details[taskId]?.task;
    if (task?.mode !== "auto") throw new Error(`Approved task stayed in ${task?.mode ?? "unknown"} mode`);
    useAppStore.getState().goHome();
    return taskId;
  });
  await page.locator(".scene.scene-home").waitFor({ state: "visible" });
  await page.evaluate(async (taskId) => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().openRoom(taskId);
  }, approvedTaskId);
  await page.locator(".scene.scene-room").waitFor({ state: "visible" });
  await panel.waitFor({ state: "visible" });
  await panel.getByText("实施中", { exact: false }).first().waitFor();

  // An existing task edits its durable Goal through the conversation composer too.
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().hideWorkbench();
  });
  await page.getByRole("button", { name: "添加到任务", exact: true }).click();
  const roomGoalDialog = page.getByRole("dialog", { name: "添加到任务" });
  assert.equal(await roomGoalDialog.locator("textarea").count(), 0);
  await roomGoalDialog.getByRole("button", { name: /^目标/ }).click();
  assert.equal(await page.getByLabel("任务目标", { exact: true }).inputValue(), "交付一个可逐功能验收的计划模式；先澄清边界，再生成实施计划");
  await page.getByLabel("任务目标", { exact: true }).fill("交付计划并逐功能完成增强审核");
  await page.getByRole("button", { name: "更新并执行目标", exact: true }).click();
  await page.getByLabel("给 Agent 的消息", { exact: true }).waitFor();
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().setCanvasTab("plan");
  });
  await panel.waitFor({ state: "visible" });
  await page.waitForFunction(() => document.querySelector(".plan-goal")?.textContent?.includes("逐功能完成增强审核"));

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
  await completedFiles.nth(0).getByRole("button", { name: /展开文件/ }).click();
  await completedFiles.nth(1).getByRole("button", { name: /展开文件/ }).click();
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
    useAppStore.getState().setCanvasTab("plan");
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
  await panel.getByRole("button", { name: "取消", exact: true }).click();
  await panel.getByText("取消当前计划？", { exact: true }).waitFor();
  await panel.getByRole("button", { name: "取消", exact: true }).click();
  await panel.getByText("取消当前计划？", { exact: true }).waitFor({ state: "detached" });
  await panel.getByRole("button", { name: "取消", exact: true }).click();
  await panel.getByText("取消当前计划？", { exact: true }).waitFor();
  await panel.getByRole("button", { name: "确认", exact: true }).click();
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

  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().hideWorkbench();
  });

  await page.getByRole("button", { name: "添加到任务", exact: true }).click();
  const roomAddDialog = page.getByRole("dialog", { name: "添加到任务" });
  await roomAddDialog.getByRole("button", { name: /^计划模式/ }).click();
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().setCanvasTab("plan");
  });
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

  // Plan is a docked collaboration surface; it must not replace conversation history.
  assert.equal(await page.locator(".timeline").isVisible(), true);

  // Goal is an executable lifecycle, not a detached metadata field. It stays editable while a
  // run is active, Stop ends that run without clearing the goal, and Delete clears it explicitly.
  await page.evaluate(async () => {
    const [{ browserMockChangeRequest }, { useAppStore }, { useTasksStore }] = await Promise.all([
      import("/src/lib/mock-data.ts"),
      import("/src/store/app.ts"),
      import("/src/store/tasks.ts"),
    ]);
    const taskId = useAppStore.getState().currentTaskId;
    if (!taskId) throw new Error("Room task is missing");
    browserMockChangeRequest(taskId);
    await useTasksStore.getState().refreshDetail(taskId);
    useAppStore.getState().hideWorkbench();
  });
  const activeGoal = page.getByRole("region", { name: "当前目标" });
  await activeGoal.getByText("进行中的目标", { exact: true }).waitFor();
  const goalActionBoxes = await activeGoal.locator(".active-goal-actions button").evaluateAll((buttons) => buttons.map((button) => {
    const rect = button.getBoundingClientRect();
    return { width: rect.width, height: rect.height };
  }));
  assert.ok(goalActionBoxes.every(({ width, height }) => width >= 36 && height >= 36), "Goal actions need comfortable desktop hit targets");
  await activeGoal.getByRole("button", { name: "编辑目标", exact: true }).click();
  assert.equal(await page.getByLabel("任务目标", { exact: true }).inputValue(), "交付计划并逐功能完成增强审核");
  await page.getByLabel("任务目标", { exact: true }).press("Escape");
  await activeGoal.getByRole("button", { name: "停止目标", exact: true }).click();
  await activeGoal.getByText("已停止的目标", { exact: true }).waitFor();
  await activeGoal.getByRole("button", { name: "删除目标", exact: true }).click();
  await activeGoal.waitFor({ state: "detached" });
  assert.equal(await page.getByText(/目标已(?:停止并)?删除。/, { exact: true }).count(), 0);
  const clearedGoal = await page.evaluate(async () => {
    const [{ useAppStore }, { useTasksStore }] = await Promise.all([
      import("/src/store/app.ts"),
      import("/src/store/tasks.ts"),
    ]);
    const taskId = useAppStore.getState().currentTaskId;
    const task = taskId ? useTasksStore.getState().details[taskId]?.task : null;
    return task ? { goal: task.goal, active: task.goal_active } : null;
  });
  assert.deepEqual(clearedGoal, { goal: "", active: false });

  assert.deepEqual(runtimeErrors, []);
  await page.close();
});
