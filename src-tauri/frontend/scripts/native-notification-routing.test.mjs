import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

import ts from "typescript";

const frontendDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const modulePath = path.join(
  frontendDir,
  "src",
  "lib",
  "native-notification-routing.ts",
);
const toastPath = path.join(frontendDir, "src", "components", "ui", "Toast.tsx");

async function loadRoutingModule() {
  const source = fs.readFileSync(modulePath, "utf8");
  const output = ts.transpileModule(source, {
    compilerOptions: {
      module: ts.ModuleKind.ESNext,
      target: ts.ScriptTarget.ES2021,
    },
    fileName: modulePath,
  }).outputText;
  const encoded = Buffer.from(`${output}\n//# sourceURL=${pathToFileURL(modulePath).href}`).toString("base64");
  return import(`data:text/javascript;base64,${encoded}`);
}

class MemoryStorage {
  values = new Map();

  getItem(key) {
    return this.values.get(key) ?? null;
  }

  setItem(key, value) {
    this.values.set(key, value);
  }
}

test("AutomationRun notification clicks survive reload and are consumed exactly once", async () => {
  const {
    AutomationDeepLinkQueue,
    routeNativeNotificationOpen,
  } = await loadRoutingModule();
  const storage = new MemoryStorage();
  let pendingSignals = 0;
  const firstWebView = new AutomationDeepLinkQueue(
    storage,
    () => { pendingSignals += 1; },
    () => "2026-08-26T08:00:00.000Z",
  );
  let openedTask = null;
  const payload = {
    notification_id: "notification-automation-1",
    target: {
      type: "automation_run",
      automation_id: "automation-1",
      run_id: "run-1",
    },
  };

  const routed = routeNativeNotificationOpen(
    payload,
    (taskId) => { openedTask = taskId; },
    firstWebView,
  );
  assert.equal(routed.destination, "automation_pending");
  assert.equal(openedTask, null, "an unfinished Automations UI must not be replaced with a fake task route");
  assert.equal(pendingSignals, 1, "the future consumer must receive an availability signal");
  assert.deepEqual(firstWebView.snapshot(), [{
    type: "automation_run",
    notification_id: "notification-automation-1",
    automation_id: "automation-1",
    run_id: "run-1",
    queued_at: "2026-08-26T08:00:00.000Z",
  }]);

  // A second queue instance models a new WebView/app launch reading the persisted hand-off.
  const restartedWebView = new AutomationDeepLinkQueue(storage, () => {});
  assert.deepEqual(restartedWebView.peek(), routed.intent);
  assert.deepEqual(restartedWebView.consume(), routed.intent);
  assert.equal(restartedWebView.consume(), null, "the claimed intent must not be delivered twice");
  assert.equal(
    new AutomationDeepLinkQueue(storage, () => {}).peek(),
    null,
    "consumption must remain committed after another reload",
  );
});

test("the router opens Task targets immediately and never enqueues them", async () => {
  const {
    AutomationDeepLinkQueue,
    routeNativeNotificationOpen,
  } = await loadRoutingModule();
  const storage = new MemoryStorage();
  const queue = new AutomationDeepLinkQueue(storage, () => {});
  const opened = [];

  const result = routeNativeNotificationOpen({
    notification_id: "notification-task-1",
    target: { type: "task", task_id: "task-1" },
  }, (taskId) => opened.push(taskId), queue);

  assert.deepEqual(result, { destination: "task", task_id: "task-1" });
  assert.deepEqual(opened, ["task-1"]);
  assert.deepEqual(queue.snapshot(), []);
});

test("duplicate activation is idempotent and corrupt storage recovers", async () => {
  const {
    AUTOMATION_DEEP_LINK_STORAGE_KEY,
    AutomationDeepLinkQueue,
    routeNativeNotificationOpen,
  } = await loadRoutingModule();
  const storage = new MemoryStorage();
  storage.setItem(AUTOMATION_DEEP_LINK_STORAGE_KEY, "{not valid json");
  let pendingSignals = 0;
  const queue = new AutomationDeepLinkQueue(
    storage,
    () => { pendingSignals += 1; },
    () => "2026-08-26T08:00:00.000Z",
  );
  const payload = {
    notification_id: "notification-automation-1",
    target: {
      type: "automation_run",
      automation_id: "automation-1",
      run_id: "run-1",
    },
  };

  routeNativeNotificationOpen(payload, () => assert.fail("must not open a Task"), queue);
  routeNativeNotificationOpen(payload, () => assert.fail("must not open a Task"), queue);

  assert.equal(queue.snapshot().length, 1, "one native notification id creates one pending intent");
  assert.equal(pendingSignals, 1, "a duplicate activation must not emit duplicate work");
  assert.doesNotThrow(() => JSON.parse(storage.getItem(AUTOMATION_DEEP_LINK_STORAGE_KEY)));
});

test("Toast sends every native-open payload through the unified router before mark-read", () => {
  const source = fs.readFileSync(toastPath, "utf8");
  const start = source.indexOf("const handleNativeOpen");
  assert.notEqual(start, -1, "Toast must register a native-open handler");
  const handler = source.slice(start, source.indexOf("let disposed", start));
  const routeAt = handler.indexOf("routeNativeNotificationOpen(payload");
  const markReadAt = handler.indexOf("notificationMarkRead(payload.notification_id)");
  assert.notEqual(routeAt, -1, "native targets must not be handled by an ad-hoc Task-only branch");
  assert.ok(markReadAt > routeAt, "AutomationRun must be durably queued before it is marked read");
  assert.doesNotMatch(handler, /payload\.target\.type\s*===\s*["']task["']/);
});
