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
const toastPath = path.join(frontendDir, "src", "components", "ui", "Toast.tsx");
const settingsPath = path.join(
  frontendDir,
  "src",
  "components",
  "settings",
  "NativeNotificationSettings.tsx",
);
const ipcPath = path.join(frontendDir, "src", "lib", "ipc.ts");
const typesPath = path.join(frontendDir, "src", "lib", "types.ts");
const nativeRustPath = path.join(repositoryDir, "src-tauri", "src", "native_notification.rs");

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
        path.join(
          playwrightCache,
          entry,
          "chrome-mac",
          "Chromium.app",
          "Contents",
          "MacOS",
          "Chromium",
        ),
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
    if (processHandle.exitCode != null) {
      throw new Error(`Vite exited with ${processHandle.exitCode}`);
    }
    try {
      const response = await fetch(url);
      if (response.ok) return;
    } catch {
      // Vite is still starting.
    }
    await new Promise((resolve) => setTimeout(resolve, 80));
  }
  throw new Error("Timed out waiting for the native-notification QA server");
}

function typeAliasLiterals(source, name) {
  const match = source.match(new RegExp(`export\\s+type\\s+${name}\\s*=([\\s\\S]*?);`));
  assert.ok(match, `missing TypeScript alias ${name}`);
  return [...match[1].matchAll(/"([a-z][a-z0-9_]*)"/g)].map((entry) => entry[1]);
}

function rustEnumNames(source, name) {
  const match = source.match(new RegExp(`pub\\s+enum\\s+${name}\\s*{([\\s\\S]*?)\\n}`));
  assert.ok(match, `missing Rust enum ${name}`);
  return [...match[1].matchAll(/^\s*([A-Z][A-Za-z0-9]+),?/gm)]
    .map((entry) => entry[1].replace(/([a-z0-9])([A-Z])/g, "$1_$2").toLowerCase());
}

let server;
let browser;
let baseUrl;

test.before(async () => {
  const executablePath = browserExecutable();
  assert.ok(executablePath, "native notification UI QA requires a Chromium-compatible browser");
  const port = await freePort();
  baseUrl = `http://127.0.0.1:${port}/`;
  server = spawn(
    process.execPath,
    [viteBin, "--host", "127.0.0.1", "--port", String(port), "--strictPort"],
    { cwd: frontendDir, stdio: ["ignore", "pipe", "pipe"], windowsHide: true },
  );
  await waitForServer(baseUrl, server);
  browser = await chromium.launch({ executablePath, headless: true });
});

test.after(async () => {
  await browser?.close();
  server?.kill();
});

test("Rust and TypeScript publish the same native notification wire vocabulary", () => {
  const types = fs.readFileSync(typesPath, "utf8");
  const rust = fs.readFileSync(nativeRustPath, "utf8");

  assert.deepEqual(
    typeAliasLiterals(types, "NativeNotificationPermissionState"),
    rustEnumNames(rust, "NativeNotificationPermissionState"),
  );
  assert.deepEqual(
    typeAliasLiterals(types, "NativeNotificationKind"),
    rustEnumNames(rust, "NativeNotificationKind"),
  );
  for (const target of ["task", "automation_run"]) {
    assert.match(types, new RegExp(`type:\\s*"${target}"`));
  }

  const ipc = fs.readFileSync(ipcPath, "utf8");
  assert.match(ipc, /NATIVE_NOTIFICATION_EVENT\s*=\s*"r-code:native-notification"/);
  assert.match(
    ipc,
    /NATIVE_NOTIFICATION_OPEN_EVENT\s*=\s*"r-code:native-notification-open"/,
  );
  assert.match(ipc, /cmd_native_notification_permission_state/);
  assert.match(ipc, /cmd_native_notification_request_permission/);
  assert.match(ipc, /cmd_native_notification_set_locale/);
});

test("settings renders granted, denied, prompt, and unavailable permission states", async () => {
  const context = await browser.newContext();
  const page = await context.newPage();
  await page.goto(`${baseUrl}@vite/client`);

  const result = await page.evaluate(async () => {
    let permission = "granted";
    let failPermissionRead = false;
    const commands = [];
    window.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        commands.push(command);
        if (command === "cmd_native_notification_permission_state") {
          if (failPermissionRead) {
            throw {
              code: "notifications.service_unavailable",
              args: {},
              debug_detail: "authorization=do-not-render-this-secret",
            };
          }
          return permission;
        }
        if (command === "cmd_native_notification_request_permission") {
          permission = "granted";
          return permission;
        }
        throw new Error(`unexpected settings IPC command: ${command}`);
      },
    };

    const refreshRuntime = (await import("/@react-refresh")).default;
    refreshRuntime.injectIntoGlobalHook(window);
    window.$RefreshReg$ = () => {};
    window.$RefreshSig$ = () => (type) => type;
    window.__vite_plugin_react_preamble_installed__ = true;
    const reactModule = await import("/@id/react");
    const React = reactModule.default ?? reactModule;
    const reactDomClient = await import("/@id/react-dom/client");
    const createRoot = reactDomClient.createRoot ?? reactDomClient.default?.createRoot;
    const { setAppLocale } = await import("/src/i18n/index.ts");
    const { NativeNotificationSettings } = await import(
      "/src/components/settings/NativeNotificationSettings.tsx?native-notification-settings-qa"
    );
    await setAppLocale("en-US");

    const waitUntil = async (predicate) => {
      const deadline = Date.now() + 3_000;
      while (Date.now() < deadline) {
        if (predicate()) return;
        await new Promise((resolve) => setTimeout(resolve, 20));
      }
      throw new Error("timed out waiting for notification settings render");
    };
    const render = async () => {
      const container = document.createElement("div");
      document.body.append(container);
      const root = createRoot(container);
      root.render(React.createElement(NativeNotificationSettings));
      await waitUntil(() => container.querySelector(".native-notification-state strong"));
      return { container, root };
    };

    const states = [];
    for (const state of ["granted", "denied", "prompt", "unavailable"]) {
      permission = state;
      failPermissionRead = false;
      const mounted = await render();
      await waitUntil(() => mounted.container.querySelector(`.is-${state}`));
      states.push({
        state,
        label: mounted.container.querySelector(".native-notification-state strong")?.textContent,
        buttons: [...mounted.container.querySelectorAll("button")].map((button) => button.textContent),
      });
      mounted.root.unmount();
      mounted.container.remove();
    }

    permission = "prompt";
    failPermissionRead = false;
    const request = await render();
    await waitUntil(() => request.container.querySelector(".is-prompt"));
    request.container.querySelector("button")?.click();
    await waitUntil(() => request.container.querySelector(".is-granted"));
    const requestedState = request.container
      .querySelector(".native-notification-state strong")?.textContent;
    request.root.unmount();
    request.container.remove();

    failPermissionRead = true;
    const failed = await render();
    await waitUntil(() => failed.container.querySelector("[role=alert]"));
    const visibleError = failed.container.querySelector("[role=alert]")?.textContent ?? "";
    failed.root.unmount();
    failed.container.remove();

    delete window.__TAURI_INTERNALS__;
    return { states, requestedState, visibleError, commands };
  });

  assert.deepEqual(result.states, [
    { state: "granted", label: "System notifications allowed", buttons: [] },
    {
      state: "denied",
      label: "System notifications denied",
      buttons: ["Request notification permission"],
    },
    {
      state: "prompt",
      label: "Notification permission not requested",
      buttons: ["Request notification permission"],
    },
    {
      state: "unavailable",
      label: "System notifications unavailable",
      buttons: ["Check again"],
    },
  ]);
  assert.equal(result.requestedState, "System notifications allowed");
  assert.match(result.visibleError, /System notifications are temporarily unavailable/);
  assert.ok(!result.visibleError.includes("do-not-render-this-secret"));
  assert.ok(result.commands.includes("cmd_native_notification_request_permission"));
  await context.close();
});

test("native bridge enforces delivery routing, source idempotency, mappings, and task deep links", async () => {
  const context = await browser.newContext();
  const page = await context.newPage();
  await page.goto(`${baseUrl}@vite/client`);

  const result = await page.evaluate(async () => {
    const callbackById = new Map();
    const listenerByEvent = new Map();
    const markedRead = [];
    let callbackSequence = 0;
    let listenerSequence = 0;
    window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
      unregisterListener: () => {},
    };
    window.localStorage.removeItem("r-code.deep-links.automation-runs.v1");
    window.__TAURI_INTERNALS__ = {
      transformCallback: (callback) => {
        callbackSequence += 1;
        callbackById.set(callbackSequence, callback);
        return callbackSequence;
      },
      invoke: async (command, args = {}) => {
        if (command === "plugin:event|listen") {
          listenerSequence += 1;
          listenerByEvent.set(args.event, { callbackId: args.handler, eventId: listenerSequence });
          return listenerSequence;
        }
        if (command === "plugin:event|unlisten") return null;
        if (command === "cmd_native_notification_set_locale") return null;
        if (command === "cmd_notification_mark_read") {
          markedRead.push(args.notificationId);
          return true;
        }
        throw new Error(`unexpected native bridge IPC command: ${command}`);
      },
    };

    const refreshRuntime = (await import("/@react-refresh")).default;
    refreshRuntime.injectIntoGlobalHook(window);
    window.$RefreshReg$ = () => {};
    window.$RefreshSig$ = () => (type) => type;
    window.__vite_plugin_react_preamble_installed__ = true;
    const reactModule = await import("/@id/react");
    const React = reactModule.default ?? reactModule;
    const reactDomClient = await import("/@id/react-dom/client");
    const createRoot = reactDomClient.createRoot ?? reactDomClient.default?.createRoot;
    const { setAppLocale } = await import("/src/i18n/index.ts");
    await setAppLocale("en-US");
    const { useTaskCompletionToasts } = await import(
      "/src/components/ui/Toast.tsx?native-notification-bridge-qa"
    );
    const { useToastStore } = await import("/src/store/toast.ts");
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    const { browserMockDetails } = await import("/src/lib/mock-data.ts");

    const emit = (eventName, payload) => {
      const listener = listenerByEvent.get(eventName);
      if (!listener) throw new Error(`missing listener for ${eventName}`);
      const callback = callbackById.get(listener.callbackId);
      if (!callback) throw new Error(`missing callback ${listener.callbackId}`);
      callback({ event: eventName, id: listener.eventId, payload });
    };
    const waitUntil = async (predicate) => {
      const deadline = Date.now() + 3_000;
      while (Date.now() < deadline) {
        if (predicate()) return;
        await new Promise((resolve) => setTimeout(resolve, 20));
      }
      throw new Error("timed out waiting for native notification bridge");
    };

    const sourceDetail = Object.values(browserMockDetails)
      .find((detail) => detail.runs.some((run) => run.agent_kind === "main"));
    if (!sourceDetail) throw new Error("missing task detail fixture for notification QA");
    const activeDetail = structuredClone(sourceDetail);
    const taskId = "native-background-task";
    activeDetail.task.id = taskId;
    activeDetail.task.title = "Background review";
    activeDetail.task.workspace_path = "D:/notification-qa";
    activeDetail.task.state = "in_progress";
    activeDetail.task.updated_at = "2026-08-26T01:00:00Z";
    activeDetail.permissions = [];
    const mainRun = activeDetail.runs.find((run) => run.agent_kind === "main");
    mainRun.id = "native-background-run";
    mainRun.ended_at = null;
    activeDetail.runs = [mainRun];

    useToastStore.setState({ toasts: [] });
    useAppStore.setState({ scene: "home", currentTaskId: null });
    useTasksStore.setState({
      tasks: [activeDetail.task],
      details: { [taskId]: activeDetail },
      currentProjectId: null,
    });
    Object.defineProperty(document, "hasFocus", { configurable: true, value: () => false });

    function NotificationHarness() {
      useTaskCompletionToasts();
      return null;
    }
    const container = document.createElement("div");
    document.body.append(container);
    const root = createRoot(container);
    root.render(React.createElement(NotificationHarness));
    await waitUntil(() => (
      listenerByEvent.has("r-code:native-notification")
      && listenerByEvent.has("r-code:native-notification-open")
    ));

    const finishedDetail = structuredClone(activeDetail);
    finishedDetail.task.state = "review_ready";
    finishedDetail.task.updated_at = "2026-08-26T01:01:00Z";
    finishedDetail.runs[0].ended_at = "2026-08-26T01:01:00Z";
    useTasksStore.setState({
      tasks: [finishedDetail.task],
      details: { [taskId]: finishedDetail },
    });
    await new Promise((resolve) => setTimeout(resolve, 0));
    const backgroundPollToastCount = useToastStore.getState().toasts.length;

    emit("r-code:native-notification", {
      notification_id: "notification-background-review",
      source_key: `review:${taskId}:native-background-run`,
      kind: "review_ready",
      title: "Ready for review: Background review",
      body: "The latest changes are ready for your review.",
      target: { type: "task", task_id: taskId },
      delivery: "in_app",
    });
    const fallbackToastCount = useToastStore.getState().toasts.length;

    useToastStore.setState({ toasts: [] });
    const events = [
      {
        notification_id: "notification-permission",
        source_key: "permission:permission-1",
        kind: "permission_required",
        title: "Approval required",
        body: "terminal needs approval",
        target: { type: "task", task_id: "task-permission" },
        delivery: "in_app",
      },
      {
        notification_id: "notification-failure",
        source_key: "run_failed:task-failure:run-1",
        kind: "run_failed",
        title: "Task failed",
        body: "Open the task",
        target: { type: "task", task_id: "task-failure" },
        delivery: "in_app",
      },
      {
        notification_id: "notification-review",
        source_key: "review:task-review:run-1",
        kind: "review_ready",
        title: "Ready for review",
        body: "Review changes",
        target: { type: "task", task_id: "task-review" },
        delivery: "in_app",
      },
      {
        notification_id: "notification-automation",
        source_key: "automation_completed:automation-1:run-1",
        kind: "automation_completed",
        title: "Automation completed",
        body: "Open the run",
        target: { type: "automation_run", automation_id: "automation-1", run_id: "run-1" },
        delivery: "in_app",
      },
    ];
    for (const event of events) emit("r-code:native-notification", event);
    emit("r-code:native-notification", events[0]);
    emit("r-code:native-notification", {
      notification_id: "notification-system-only",
      source_key: "review:task-system:run-1",
      kind: "review_ready",
      title: "Already delivered by the OS",
      body: "This must not become a duplicate toast",
      target: { type: "task", task_id: "task-system" },
      delivery: "system",
    });

    const mappedToasts = useToastStore.getState().toasts.map((toast) => ({
      id: toast.id,
      kind: toast.kind,
      title: toast.title,
      actionLabel: toast.action?.label ?? null,
    }));
    const reviewAction = useToastStore.getState().toasts
      .find((toast) => toast.id === "native:review:task-review:run-1")?.action;
    reviewAction?.run();
    const reviewNavigation = {
      scene: useAppStore.getState().scene,
      taskId: useAppStore.getState().currentTaskId,
      tab: useAppStore.getState().canvasTab,
    };

    useAppStore.setState({ scene: "home", currentTaskId: null });
    emit("r-code:native-notification-open", {
      notification_id: "notification-open-task",
      target: { type: "task", task_id: "task-from-system-banner" },
    });
    await waitUntil(() => markedRead.includes("notification-open-task"));
    const openNavigation = {
      scene: useAppStore.getState().scene,
      taskId: useAppStore.getState().currentTaskId,
    };

    emit("r-code:native-notification-open", {
      notification_id: "notification-open-automation",
      target: { type: "automation_run", automation_id: "automation-deep-link", run_id: "run-deep-link" },
    });
    await waitUntil(() => markedRead.includes("notification-open-automation"));
    const routing = await import("/src/lib/native-notification-routing.ts");
    const automationPending = routing.pendingAutomationDeepLinkIntents().map((intent) => ({
      type: intent.type,
      notificationId: intent.notification_id,
      automationId: intent.automation_id,
      runId: intent.run_id,
      hasQueuedAt: Number.isFinite(Date.parse(intent.queued_at)),
    }));
    const consumedAutomation = routing.consumePendingAutomationDeepLinkIntent();
    const automationAfterConsume = routing.pendingAutomationDeepLinkIntents();

    root.unmount();
    container.remove();
    delete window.__TAURI_INTERNALS__;
    delete window.__TAURI_EVENT_PLUGIN_INTERNALS__;
    return {
      backgroundPollToastCount,
      fallbackToastCount,
      mappedToasts,
      reviewNavigation,
      openNavigation,
      automationPending,
      consumedAutomation: consumedAutomation
        ? {
            notificationId: consumedAutomation.notification_id,
            automationId: consumedAutomation.automation_id,
            runId: consumedAutomation.run_id,
          }
        : null,
      automationAfterConsume,
      markedRead,
    };
  });

  assert.equal(result.backgroundPollToastCount, 0, "background polling must not pre-claim a source");
  assert.equal(result.fallbackToastCount, 1, "an in-app fallback must remain deliverable");
  assert.deepEqual(result.mappedToasts, [
    {
      id: "native:permission:permission-1",
      kind: "warn",
      title: "Approval required",
      actionLabel: "Review request",
    },
    {
      id: "native:run_failed:task-failure:run-1",
      kind: "error",
      title: "Task failed",
      actionLabel: "Open task",
    },
    {
      id: "native:review:task-review:run-1",
      kind: "success",
      title: "Ready for review",
      actionLabel: "Review changes",
    },
    {
      id: "native:automation_completed:automation-1:run-1",
      kind: "success",
      title: "Automation completed",
      actionLabel: null,
    },
  ]);
  assert.deepEqual(result.reviewNavigation, {
    scene: "room",
    taskId: "task-review",
    tab: "review",
  });
  assert.deepEqual(result.openNavigation, {
    scene: "room",
    taskId: "task-from-system-banner",
  });
  assert.deepEqual(result.automationPending, [{
    type: "automation_run",
    notificationId: "notification-open-automation",
    automationId: "automation-deep-link",
    runId: "run-deep-link",
    hasQueuedAt: true,
  }]);
  assert.deepEqual(result.consumedAutomation, {
    notificationId: "notification-open-automation",
    automationId: "automation-deep-link",
    runId: "run-deep-link",
  });
  assert.deepEqual(result.automationAfterConsume, []);
  assert.deepEqual(result.markedRead, [
    "notification-open-task",
    "notification-open-automation",
  ]);
  await context.close();
});

test("the production notification surfaces retain explicit fallback and dedupe guards", () => {
  const toast = fs.readFileSync(toastPath, "utf8");
  const settings = fs.readFileSync(settingsPath, "utf8");
  assert.match(toast, /event\.delivery\s*!==\s*"in_app"/);
  assert.match(toast, /seenNativeSources\.has\(event\.source_key\)/);
  assert.match(toast, /notificationMarkRead\(payload\.notification_id\)/);
  assert.match(toast, /if\s*\(sourceKey\s*&&\s*!appWindowIsForeground\(\)\)\s*return/);
  assert.match(settings, /permission\s*===\s*"prompt"\s*\|\|\s*permission\s*===\s*"denied"/);
  assert.match(settings, /permission\s*===\s*"unavailable"/);
});
