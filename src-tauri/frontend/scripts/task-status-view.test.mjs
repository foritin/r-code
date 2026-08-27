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
const repositoryDir = path.resolve(frontendDir, "..", "..");
const viteBin = path.join(frontendDir, "node_modules", "vite", "bin", "vite.js");
const typesPath = path.join(frontendDir, "src", "lib", "types.ts");
const presentationPath = path.join(frontendDir, "src", "lib", "presentation.ts");
const rustContractPath = path.join(repositoryDir, "crates", "r-code-core", "src", "task_status.rs");
const conversationsPath = path.join(frontendDir, "src", "components", "scenes", "ConversationsScene.tsx");
const canvasPath = path.join(frontendDir, "src", "components", "room", "Canvas.tsx");
const dashboardPath = path.join(frontendDir, "src", "components", "scenes", "DashboardScene.tsx");

function browserExecutable() {
  const localAppData = process.env.LOCALAPPDATA ?? "";
  const playwrightCache = path.join(localAppData, "ms-playwright");
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
    "/usr/bin/chromium-browser",
  ].find((candidate) => candidate && fs.existsSync(candidate));
}

async function freePort() {
  return new Promise((resolve, reject) => {
    const socket = net.createServer();
    socket.once("error", reject);
    socket.listen(0, "127.0.0.1", () => {
      const address = socket.address();
      const port = typeof address === "object" && address ? address.port : 0;
      socket.close((error) => error ? reject(error) : resolve(port));
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
  throw new Error("Timed out waiting for the TaskStatusView test server");
}

function typeAliasLiterals(source, name) {
  const match = source.match(new RegExp(`export\\s+type\\s+${name}\\s*=([\\s\\S]*?);`));
  assert.ok(match, `missing TypeScript alias ${name}`);
  return [...match[1].matchAll(/"([a-z][a-z0-9_]*)"/g)].map((entry) => entry[1]);
}

function rustEnumNames(source, name) {
  const match = source.match(new RegExp(`pub\\s+enum\\s+${name}\\s*{([\\s\\S]*?)\\n}`));
  assert.ok(match, `missing Rust enum ${name}`);
  return [...match[1].matchAll(/^\s*([A-Z][A-Za-z0-9]+),/gm)]
    .map((entry) => entry[1].replace(/([a-z0-9])([A-Z])/g, "$1_$2").toLowerCase());
}

function interfaceFields(source, name) {
  const match = source.match(new RegExp(`export\\s+interface\\s+${name}\\s*{([\\s\\S]*?)\\n}`));
  assert.ok(match, `missing TypeScript interface ${name}`);
  return [...match[1].matchAll(/^\s*([a-z][a-z0-9_]*)\??\s*:/gm)].map((entry) => entry[1]);
}

function rustStructFields(source, name) {
  const match = source.match(new RegExp(`pub\\s+struct\\s+${name}\\s*{([\\s\\S]*?)\\n}`));
  assert.ok(match, `missing Rust struct ${name}`);
  return [...match[1].matchAll(/^\s*pub\s+([a-z][a-z0-9_]*)\s*:/gm)].map((entry) => entry[1]);
}

let server;
let browser;
let baseUrl;

test.before(async () => {
  const port = await freePort();
  baseUrl = `http://127.0.0.1:${port}/`;
  server = spawn(
    process.execPath,
    [viteBin, "--host", "127.0.0.1", "--port", String(port), "--strictPort"],
    { cwd: frontendDir, stdio: ["ignore", "pipe", "pipe"], windowsHide: true },
  );
  await waitForServer(baseUrl, server);
  browser = await chromium.launch({ executablePath: browserExecutable(), headless: true });
});

test.after(async () => {
  await browser?.close();
  server?.kill();
});

test("the TypeScript TaskStatusView contract exactly follows the Rust serialized contract", () => {
  const types = fs.readFileSync(typesPath, "utf8");
  const rust = fs.readFileSync(rustContractPath, "utf8");

  assert.deepEqual(
    typeAliasLiterals(types, "TaskDisplayState"),
    rustEnumNames(rust, "TaskDisplayState"),
  );
  assert.deepEqual(
    typeAliasLiterals(types, "TaskAttention"),
    rustEnumNames(rust, "TaskAttention"),
  );
  assert.deepEqual(
    interfaceFields(types, "TaskStatusView"),
    rustStructFields(rust, "TaskStatusView"),
  );
  assert.match(
    types,
    /active_run_id\?:\s*string\s*\|\s*null/,
    "active_run_id must accept the omitted Rust shape and an explicit null from legacy fixtures",
  );
});

test("list, detail, and dashboard surfaces route status through the authoritative display state", () => {
  const presentation = fs.readFileSync(presentationPath, "utf8");
  const conversations = fs.readFileSync(conversationsPath, "utf8");
  const canvas = fs.readFileSync(canvasPath, "utf8");
  const dashboard = fs.readFileSync(dashboardPath, "utf8");

  assert.match(
    presentation,
    /taskStatus\(task, detail\)\?\.display_state\s*\?\?\s*legacyDisplayState\(task, detail\)/,
  );
  assert.match(conversations, /taskDisplayState\(task, details\[task\.id\]\)/);
  assert.match(conversations, /taskStateLabel\(task\.state, detail\)/);
  assert.match(canvas, /taskStateLabel\(task\.state, detail\)/);
  assert.match(canvas, /switch\s*\(taskDisplayState\(detail\.task, detail\)\)/);
  assert.match(dashboard, /visualTaskDisplayState\(summary\.status\.display_state\)/);
  assert.match(
    dashboard,
    /taskDisplayStateLabel\(summary\.status\.display_state, summary\.status\.persisted_state\)/,
  );
});

test("presentation honors backend precedence, preserves attention, and maps every new state", async () => {
  const context = await browser.newContext();
  const page = await context.newPage();
  await page.goto(`${baseUrl}@vite/client`);
  const result = await page.evaluate(async () => {
    const presentation = await import("/src/lib/presentation.ts");
    const { browserMockDetails } = await import("/src/lib/mock-data.ts");
    const source = browserMockDetails["mock-task-queue"] ?? Object.values(browserMockDetails)[0];
    if (!source) throw new Error("missing browser TaskDetail fixture");

    const makeDetail = (displayState, attention = [], unreadCount = 0, queueDepth = 0) => {
      const detail = structuredClone(source);
      detail.task.state = "in_progress";
      detail.status = {
        task_id: detail.task.id,
        persisted_state: detail.task.state,
        display_state: displayState,
        attention: [...attention],
        active_run_id: detail.runs.find((run) => run.ended_at === null)?.id ?? "active-run",
        queue_depth: queueDepth,
        unread_count: unreadCount,
      };
      return detail;
    };

    const approval = makeDetail("waiting_for_approval", ["approval_required"], 11);
    const failure = makeDetail("failed", ["run_failed", "review_required"], 9);
    const mapped = [
      ["waiting_for_question", "等待回答", "attention", "等待你回答问题", 0],
      ["workspace_binding_invalid", "工作区失效", "attention", "工作区绑定失效，需要恢复", 0],
      ["verification_required", "需要验证", "attention", "当前结果需要重新验证", 0],
      ["verifying", "正在验证", "running", "正在验证变更", 0],
      ["queued", "排队中", "running", "已有 4 条消息排队", 4],
    ].map(([display, label, visual, activity, queueDepth]) => {
      const detail = makeDetail(display, [], 0, queueDepth);
      return {
        display: presentation.taskDisplayState(detail.task, detail),
        label: presentation.taskStateLabel(detail.task.state, detail),
        visual: presentation.visualTaskState(detail.task, detail),
        activity: presentation.taskActivity(detail.task, detail),
        expected: { display, label, visual, activity },
      };
    });

    const legacyRunning = structuredClone(source);
    legacyRunning.task.state = "in_progress";
    legacyRunning.permissions = [];
    delete legacyRunning.status;
    const legacyReview = structuredClone(source);
    legacyReview.task.state = "review_ready";
    legacyReview.permissions = [];
    legacyReview.runs = [];
    delete legacyReview.status;

    return {
      approval: {
        display: presentation.taskDisplayState(approval.task, approval),
        visual: presentation.visualTaskState(approval.task, approval),
        label: presentation.taskStateLabel(approval.task.state, approval),
        unread: presentation.taskStatus(approval.task, approval).unread_count,
      },
      failure: {
        display: presentation.taskDisplayState(failure.task, failure),
        visual: presentation.visualTaskState(failure.task, failure),
        label: presentation.taskStateLabel(failure.task.state, failure),
        unread: presentation.taskStatus(failure.task, failure).unread_count,
        attention: presentation.taskStatus(failure.task, failure).attention,
      },
      mapped,
      legacy: {
        running: presentation.taskDisplayState(legacyRunning.task, legacyRunning),
        review: presentation.taskDisplayState(legacyReview.task, legacyReview),
        missingStatus: presentation.taskStatus(legacyRunning.task, legacyRunning) ?? null,
      },
    };
  });

  assert.deepEqual(result.approval, {
    display: "waiting_for_approval",
    visual: "attention",
    label: "等待审批",
    unread: 11,
  });
  assert.deepEqual(result.failure, {
    display: "failed",
    visual: "attention",
    label: "执行失败",
    unread: 9,
    attention: ["run_failed", "review_required"],
  });
  for (const item of result.mapped) {
    assert.deepEqual(
      { display: item.display, label: item.label, visual: item.visual, activity: item.activity },
      item.expected,
    );
  }
  assert.deepEqual(result.legacy, {
    running: "running",
    review: "review_ready",
    missingStatus: null,
  });
  await context.close();
});

test("detail and dashboard stores refresh when status, queue, unread, or attention changes", async () => {
  const context = await browser.newContext();
  const page = await context.newPage();
  await page.goto(`${baseUrl}@vite/client`);
  const result = await page.evaluate(async () => {
    const { browserMockDetails, browserMockWorkspaceDashboard } = await import("/src/lib/mock-data.ts");
    const sourceDetail = browserMockDetails["mock-task-queue"] ?? Object.values(browserMockDetails)[0];
    if (!sourceDetail) throw new Error("missing browser TaskDetail fixture");
    const taskId = sourceDetail.task.id;

    const withStatus = (source, displayState, attention, queueDepth, unreadCount) => {
      const detail = structuredClone(source);
      detail.status = {
        task_id: detail.task.id,
        persisted_state: detail.task.state,
        display_state: displayState,
        attention: [...attention],
        active_run_id: null,
        queue_depth: queueDepth,
        unread_count: unreadCount,
      };
      return detail;
    };
    const detailVariants = [
      withStatus(sourceDetail, "idle", [], 0, 0),
      withStatus(sourceDetail, "failed", ["run_failed"], 0, 0),
      withStatus(sourceDetail, "failed", ["run_failed"], 3, 0),
      withStatus(sourceDetail, "failed", ["run_failed"], 3, 7),
      withStatus(sourceDetail, "failed", ["run_failed", "review_required"], 3, 7),
    ];

    const workspacePath = sourceDetail.task.workspace_path
      ?? "D:/project/rust/r-code";
    const sourceDashboard = browserMockWorkspaceDashboard(workspacePath);
    if (!sourceDashboard.tasks.length) throw new Error("missing browser dashboard task fixture");
    const dashboardVariants = detailVariants.map((detail) => {
      const dashboard = structuredClone(sourceDashboard);
      dashboard.tasks[0].status = structuredClone(detail.status);
      return dashboard;
    });

    let detailCursor = 1;
    let dashboardCursor = 1;
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === "cmd_task_detail") {
          return structuredClone(detailVariants[Math.min(detailCursor++, detailVariants.length - 1)]);
        }
        if (command === "cmd_workspace_dashboard") {
          return structuredClone(dashboardVariants[Math.min(dashboardCursor++, dashboardVariants.length - 1)]);
        }
        throw new Error(`unexpected TaskStatusView QA command: ${command}`);
      },
    };

    const { useTasksStore } = await import("/src/store/tasks.ts?task-status-signature-qa");
    useTasksStore.setState({
      details: { [taskId]: detailVariants[0] },
      dashboards: { [workspacePath]: dashboardVariants[0] },
    });

    const detailRefreshes = [];
    for (let index = 1; index < detailVariants.length; index += 1) {
      const before = useTasksStore.getState().details[taskId];
      await useTasksStore.getState().refreshDetail(taskId);
      const after = useTasksStore.getState().details[taskId];
      detailRefreshes.push({ changed: before !== after, status: structuredClone(after.status) });
    }

    const dashboardRefreshes = [];
    for (let index = 1; index < dashboardVariants.length; index += 1) {
      const before = useTasksStore.getState().dashboards[workspacePath];
      await useTasksStore.getState().refreshDashboard(workspacePath);
      const after = useTasksStore.getState().dashboards[workspacePath];
      dashboardRefreshes.push({
        changed: before !== after,
        status: structuredClone(after.tasks[0].status),
      });
    }
    delete window.__TAURI_INTERNALS__;
    return { detailRefreshes, dashboardRefreshes };
  });

  for (const refresh of [...result.detailRefreshes, ...result.dashboardRefreshes]) {
    assert.equal(refresh.changed, true, `store suppressed status update ${JSON.stringify(refresh.status)}`);
  }
  assert.deepEqual(
    result.detailRefreshes.map((item) => [
      item.status.display_state,
      item.status.queue_depth,
      item.status.unread_count,
      item.status.attention,
    ]),
    [
      ["failed", 0, 0, ["run_failed"]],
      ["failed", 3, 0, ["run_failed"]],
      ["failed", 3, 7, ["run_failed"]],
      ["failed", 3, 7, ["run_failed", "review_required"]],
    ],
  );
  assert.deepEqual(
    result.dashboardRefreshes.map((item) => [
      item.status.display_state,
      item.status.queue_depth,
      item.status.unread_count,
      item.status.attention,
    ]),
    [
      ["failed", 0, 0, ["run_failed"]],
      ["failed", 3, 0, ["run_failed"]],
      ["failed", 3, 7, ["run_failed"]],
      ["failed", 3, 7, ["run_failed", "review_required"]],
    ],
  );
  await context.close();
});
