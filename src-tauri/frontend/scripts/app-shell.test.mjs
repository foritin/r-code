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

test("only the current text frontier owns the animated caret", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const contract = await page.evaluate(async () => {
    const { applyAgentEvent } = await import("/src/components/room/model.ts");
    let nextId = 0;
    let items = [];
    const nid = () => `event-${nextId += 1}`;
    const streamingText = () => items
      .filter((item) => item.kind === "agent" && item.streaming)
      .map((item) => item.text);

    items = applyAgentEvent(items, { type: "message", text: "先看代码", delta: true }, 1, nid);
    const whileWritingFirst = streamingText();

    items = applyAgentEvent(items, { type: "activity", phase: "tool", detail: "read_file" }, 2, nid);
    const whenToolStarts = streamingText();
    items = applyAgentEvent(items, {
      type: "tool_call",
      name: "read_file",
      input: { path: "src/main.rs" },
      call_id: "call-1",
    }, 2, nid);
    items = applyAgentEvent(items, {
      type: "tool_result",
      call_id: "call-1",
      output: "ok",
      is_error: false,
    }, 3, nid);

    items = applyAgentEvent(items, { type: "activity", phase: "streaming" }, 4, nid);
    items = applyAgentEvent(items, { type: "message", text: "再看测试", delta: true }, 4, nid);
    const whileWritingSecond = streamingText();
    items = applyAgentEvent(items, { type: "message", text: "，继续", delta: true }, 4, nid);
    const afterAppending = streamingText();

    items = applyAgentEvent(items, { type: "activity", phase: "finalizing" }, 5, nid);
    const whileFinalizing = streamingText();
    items = applyAgentEvent(items, { type: "state", state: "review_ready" }, 6, nid);

    const caret = document.createElement("span");
    caret.className = "caret";
    document.body.append(caret);
    const style = getComputedStyle(caret);
    const caretStyle = {
      width: style.width,
      animationName: style.animationName,
      pointerEvents: style.pointerEvents,
    };
    caret.remove();

    return {
      whileWritingFirst,
      whenToolStarts,
      whileWritingSecond,
      afterAppending,
      whileFinalizing,
      afterRun: streamingText(),
      agentText: items.filter((item) => item.kind === "agent").map((item) => item.text),
      caretStyle,
    };
  });

  assert.deepEqual(contract.whileWritingFirst, ["先看代码"]);
  assert.deepEqual(contract.whenToolStarts, []);
  assert.deepEqual(contract.whileWritingSecond, ["再看测试"]);
  assert.deepEqual(contract.afterAppending, ["再看测试，继续"]);
  assert.deepEqual(contract.whileFinalizing, []);
  assert.deepEqual(contract.afterRun, []);
  assert.deepEqual(contract.agentText, ["先看代码", "再看测试，继续"]);
  assert.deepEqual(contract.caretStyle, {
    width: "2px",
    animationName: "blink",
    pointerEvents: "none",
  });

  await page.close();
});

for (const viewport of [{ width: 800, height: 600 }, { width: 1200, height: 800 }, { width: 1800, height: 1200 }]) {
  test(`room fills and scrolls within ${viewport.width}x${viewport.height}`, async () => {
    const page = await browser.newPage({ viewport });
    const runtimeErrors = [];
    page.on("pageerror", (error) => runtimeErrors.push(String(error)));
    page.on("console", (message) => {
      if (message.type() === "error") runtimeErrors.push(message.text());
    });

    await page.goto(baseUrl, { waitUntil: "networkidle" });
    if (viewport.width < 1120) {
      await page.locator(".sidebar-nav-item").filter({ hasText: "对话" }).click();
      await page.locator(".conversation-main").first().click();
    } else {
      await page.locator(".sidebar-task:visible").first().click();
    }
    await page.locator("#main-content > .scene-room").waitFor({ state: "visible" });

    const layout = await page.evaluate(() => {
      const main = document.querySelector("#main-content");
      const room = document.querySelector("#main-content > .scene-room");
      const timeline = document.querySelector("#main-content > .scene-room .timeline");
      assertElement(main, "main");
      assertElement(room, "room");
      assertElement(timeline, "timeline");

      for (let index = 0; index < 80; index += 1) {
        const row = document.createElement("p");
        row.textContent = `scroll-regression-${index}`;
        timeline.append(row);
      }
      timeline.scrollTop = timeline.scrollHeight;

      const mainRect = main.getBoundingClientRect();
      const roomRect = room.getBoundingClientRect();
      return {
        mainRect: [mainRect.x, mainRect.y, mainRect.width, mainRect.height],
        roomRect: [roomRect.x, roomRect.y, roomRect.width, roomRect.height],
        timeline: [timeline.clientHeight, timeline.scrollHeight, timeline.scrollTop],
        page: [document.documentElement.scrollWidth, document.documentElement.scrollHeight, innerWidth, innerHeight],
      };

      function assertElement(value, label) {
        if (!(value instanceof HTMLElement)) throw new Error(`${label} missing`);
      }
    });

    assert.deepEqual(layout.roomRect, layout.mainRect, "room must occupy the complete main viewport");
    assert.ok(layout.timeline[1] > layout.timeline[0], "long conversations must overflow the timeline");
    assert.ok(layout.timeline[2] > 0, "the timeline must accept vertical scrolling");
    assert.ok(layout.page[0] <= layout.page[2] + 1, "the app must not create page-level horizontal scrolling");
    assert.ok(layout.page[1] <= layout.page[3] + 1, "the app must not create page-level vertical scrolling");
    assert.deepEqual(runtimeErrors, []);
    await page.close();
  });
}

test("project conversations expose archive and confirmed permanent delete", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const taskRow = page.locator(".sidebar-task-row").filter({ hasText: "更新依赖并修复告警" });
  await taskRow.locator(".sidebar-task").click();
  await page.locator("#main-content > .scene-room").waitFor({ state: "visible" });
  await taskRow.hover();
  await taskRow.locator(".task-actions-trigger").click();

  const menu = page.locator('.task-actions-popover[role="menu"]');
  await menu.waitFor({ state: "visible" });
  assert.match(await menu.innerText(), /归档对话/);
  assert.match(await menu.innerText(), /永久删除/);
  await menu.getByRole("menuitem", { name: /永久删除/ }).click();

  const dialog = page.getByRole("alertdialog", { name: "永久删除这段对话？" });
  await dialog.waitFor({ state: "visible" });
  assert.match(await dialog.innerText(), /项目目录和其中的文件不会被删除/);
  await dialog.getByRole("button", { name: "永久删除", exact: true }).click();
  await page.getByText("对话已永久删除", { exact: true }).waitFor({ state: "visible" });
  await page.locator("#main-content > .scene-conversations").waitFor({ state: "visible" });
  assert.equal(await page.locator(".sidebar-task-row").filter({ hasText: "更新依赖并修复告警" }).count(), 0);
  await page.close();
});

test("archived conversations remain available as read-only history", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.locator(".sidebar-nav-item").filter({ hasText: "对话" }).click();

  const conversation = page.locator(".conversation-row").filter({ hasText: "更新依赖并修复告警" });
  await conversation.locator(".task-actions-trigger").click();
  await page.getByRole("menuitem", { name: /归档对话/ }).click();
  await page.getByText("对话已归档", { exact: true }).waitFor({ state: "visible" });

  await page.getByRole("tab", { name: "已归档" }).click();
  const archived = page.locator(".conversation-row").filter({ hasText: "更新依赖并修复告警" });
  await archived.waitFor({ state: "visible" });
  await archived.locator(".conversation-main").click();
  await page.getByText("此对话已归档，只能查看历史。可通过右上角对话选项永久删除。").waitFor({ state: "visible" });
  assert.equal(await page.locator(".composer").count(), 0);
  await page.close();
});

test("workspace mock keeps opaque identity stable until forget", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const result = await page.evaluate(async () => {
    const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
    const { browserMockWorkspaces } = await import("/src/lib/mock-data.ts");
    const path = "D:/identity-regression/shared-name";
    const siblingPath = "D:/other-root/shared-name";
    const legacyPath = "D:/legacy/workspace";
    const trackedPaths = new Set([path, siblingPath, legacyPath]);

    try {
      const first = await browserMockInvoke("cmd_workspace_open", { path });
      const second = await browserMockInvoke("cmd_workspace_open", { path });
      const sibling = await browserMockInvoke("cmd_workspace_open", { path: siblingPath });
      const listed = await browserMockInvoke("cmd_workspace_list");
      const firstListed = listed.find((workspace) => workspace.canonical_path === path);
      const routed = await browserMockInvoke("cmd_workspace_set_access_mode", {
        workspacePath: path,
        accessMode: "full_access",
      });

      const legacy = {
        canonical_path: legacyPath,
        display_name: "workspace",
        access_mode: "request_approval",
        last_opened_at: "2026-01-01T00:00:00.000Z",
      };
      browserMockWorkspaces.unshift(legacy);
      const legacyListed = (await browserMockInvoke("cmd_workspace_list"))
        .find((workspace) => workspace.canonical_path === legacyPath);
      const legacyOpened = await browserMockInvoke("cmd_workspace_open", { path: legacyPath });
      const legacyListedAgain = (await browserMockInvoke("cmd_workspace_list"))
        .find((workspace) => workspace.canonical_path === legacyPath);

      legacy.memory_mode = "future_mode";
      let invalidListError = "";
      let invalidOpenError = "";
      try {
        await browserMockInvoke("cmd_workspace_list");
      } catch (error) {
        invalidListError = String(error);
      }
      try {
        await browserMockInvoke("cmd_workspace_open", { path: legacyPath });
      } catch (error) {
        invalidOpenError = String(error);
      }
      legacy.memory_mode = "inherit";

      await browserMockInvoke("cmd_workspace_forget", { workspacePath: path });
      const reopened = await browserMockInvoke("cmd_workspace_open", { path });

      return {
        first,
        second,
        sibling,
        firstListed,
        routed,
        legacyListed,
        legacyOpened,
        legacyListedAgain,
        invalidListError,
        invalidOpenError,
        reopened,
      };
    } finally {
      for (let index = browserMockWorkspaces.length - 1; index >= 0; index -= 1) {
        if (trackedPaths.has(browserMockWorkspaces[index].canonical_path)) {
          browserMockWorkspaces.splice(index, 1);
        }
      }
    }
  });

  assert.equal(result.first.id, result.second.id);
  assert.equal(result.firstListed.id, result.first.id);
  assert.equal(result.firstListed.memory_mode, "inherit");
  assert.equal(result.firstListed.memory_generation, 1);
  assert.equal(result.first.canonical_path, "D:/identity-regression/shared-name");
  assert.equal(result.routed.canonical_path, result.first.canonical_path);
  assert.equal(result.routed.id, result.first.id, "path-based navigation must not replace identity");
  assert.notEqual(result.first.id, result.first.canonical_path);
  assert.notEqual(result.first.id, result.first.display_name);
  assert.notEqual(result.first.id, result.sibling.id, "display name must not determine identity");
  assert.notEqual(result.first.id, result.reopened.id, "forget must discard the old identity");

  assert.equal(result.legacyListed.memory_mode, "inherit");
  assert.equal(result.legacyListed.memory_generation, 1);
  assert.equal(result.legacyOpened.id, result.legacyListed.id);
  assert.equal(result.legacyListedAgain.id, result.legacyListed.id);
  assert.match(result.invalidListError, /memory_mode/);
  assert.match(result.invalidOpenError, /memory_mode/);
  await page.close();
});

test("legacy memory notices stay metadata-only and preserve workspace identity", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  const addedPaths = [
    "D:/project/rust/legacy-unknown",
    "D:/project/rust/legacy-deleted-tracked",
    "D:/project/rust/legacy-absent",
    "D:/project/rust/legacy-unmapped",
  ];

  try {
    await page.goto(baseUrl, { waitUntil: "networkidle" });
    const contract = await page.evaluate(async (paths) => {
      const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
      const { browserMockWorkspaces } = await import("/src/lib/mock-data.ts");
      const { useTasksStore } = await import("/src/store/tasks.ts");
      const additions = paths.map((canonicalPath, index) => ({
        id: `9000000000000000000000000000000${index}`,
        canonical_path: canonicalPath,
        display_name: canonicalPath.split("/").at(-1),
        access_mode: "request_approval",
        last_opened_at: `2026-07-31T00:0${index}:00.000Z`,
        memory_mode: "inherit",
        memory_generation: 1,
      }));
      browserMockWorkspaces.push(...additions);
      await useTasksStore.getState().refreshWorkspaces();
      useTasksStore.getState().setCurrentProject(null);

      const statusResponses = await Promise.all([
        "D:/project/rust/r-code",
        "D:/project/rust/api-server",
        ...paths,
      ].map(async (workspacePath) => ({
        workspacePath,
        response: await browserMockInvoke("cmd_legacy_memory_status", { workspacePath }),
      })));
      const retiredErrors = [];
      for (const command of ["cmd_memory_get", "cmd_memory_set"]) {
        try {
          await browserMockInvoke(command, { workspacePath: paths[0], content: "PRIVATE_BODY_SENTINEL" });
        } catch (error) {
          retiredErrors.push(String(error));
        }
      }

      return {
        initialIdentity: Object.fromEntries(
          useTasksStore.getState().workspaces.map((workspace) => [
            workspace.canonical_path,
            { id: workspace.id, canonical_path: workspace.canonical_path },
          ]),
        ),
        retiredErrors,
        statusResponses,
      };
    }, addedPaths);

    assert.equal(contract.retiredErrors.length, 2);
    for (const error of contract.retiredErrors) assert.match(error, /尚未实现命令/);
    for (const { response } of contract.statusResponses) {
      assert.deepEqual(Object.keys(response).sort(), ["exists", "git_tracking"]);
    }

    await page.getByRole("button", { name: "管理项目" }).click();
    await page.locator("#main-content > .scene-projects").waitFor({ state: "visible" });

    const scenarios = [
      {
        path: "D:/project/rust/r-code",
        heading: /可能已进入 Git 历史/,
        copy: /自行审查|人工审查/,
      },
      {
        path: "D:/project/rust/api-server",
        heading: /发现未被 Git 跟踪/,
        copy: /不会读取、导入、修改或删除/,
      },
      {
        path: "D:/project/rust/legacy-unknown",
        heading: /无法检测旧版记忆文件的 Git 跟踪状态/,
        copy: /工作树中发现了旧版记忆文件/,
        forbidden: /未被 Git 跟踪|Git 未跟踪|无需处理|历史安全/,
      },
      {
        path: "D:/project/rust/legacy-deleted-tracked",
        heading: /Git 仍有跟踪记录/,
        copy: /索引仍记录|可能保留内容/,
      },
      {
        path: "D:/project/rust/legacy-absent",
        heading: /未发现旧版记忆文件/,
        copy: /未检查.*Git 历史/,
        forbidden: /无需处理|历史安全/,
      },
      {
        path: "D:/project/rust/legacy-unmapped",
        heading: /无法检测旧版记忆文件的 Git 跟踪状态/,
        copy: /无法据此判断 Git 历史/,
        forbidden: /未被 Git 跟踪|Git 未跟踪|无需处理|历史安全/,
      },
    ];

    for (const scenario of scenarios) {
      const displayName = scenario.path.split("/").at(-1);
      const row = page.locator(".workspace-row").filter({ hasText: displayName });
      await row.locator(".workspace-main").click();

      const notice = page.locator(".workspace-memory .legacy-memory-status");
      await notice.locator("strong").filter({ hasText: scenario.heading }).waitFor({ state: "visible" });
      const copy = await notice.innerText();
      assert.match(copy, scenario.copy);
      if (scenario.forbidden) assert.doesNotMatch(copy, scenario.forbidden);
      assert.ok(!copy.includes(scenario.path), "notice must not reveal the absolute workspace path");
      assert.ok(!copy.includes("PRIVATE_BODY_SENTINEL"), "notice must not reveal file content");
      assert.equal(
        await notice.locator('textarea,input,select,button,a,[contenteditable="true"],[role="button"]').count(),
        0,
        "legacy memory status must not expose edit/import/delete/untrack actions",
      );

      const navigation = await page.evaluate(async (workspacePath) => {
        const { useTasksStore } = await import("/src/store/tasks.ts");
        const state = useTasksStore.getState();
        const workspace = state.workspaces.find((item) => item.canonical_path === workspacePath);
        return {
          currentProjectId: state.currentProjectId,
          identity: workspace && { id: workspace.id, canonical_path: workspace.canonical_path },
        };
      }, scenario.path);
      assert.equal(navigation.currentProjectId, scenario.path);
      assert.deepEqual(navigation.identity, contract.initialIdentity[scenario.path]);
      assert.equal(await row.getAttribute("class"), "workspace-row current");
    }

    const statusResponsesAfterNavigation = await page.evaluate(async (workspacePaths) => {
      const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
      return Promise.all(workspacePaths.map(async (workspacePath) => ({
        workspacePath,
        response: await browserMockInvoke("cmd_legacy_memory_status", { workspacePath }),
      })));
    }, contract.statusResponses.map(({ workspacePath }) => workspacePath));
    assert.deepEqual(
      statusResponsesAfterNavigation,
      contract.statusResponses,
      "viewing notices must not import, delete, or untrack a legacy file",
    );

    const memorySection = page.locator(".workspace-memory");
    assert.equal(await memorySection.locator("textarea").count(), 0);
    assert.doesNotMatch(await memorySection.innerText(), /保存记忆|记录架构约定、开发偏好与重要上下文/);
  } finally {
    await page.evaluate(async (paths) => {
      const { browserMockWorkspaces } = await import("/src/lib/mock-data.ts");
      const { useTasksStore } = await import("/src/store/tasks.ts");
      for (let index = browserMockWorkspaces.length - 1; index >= 0; index -= 1) {
        if (paths.includes(browserMockWorkspaces[index].canonical_path)) browserMockWorkspaces.splice(index, 1);
      }
      useTasksStore.setState((state) => ({
        workspaces: state.workspaces.filter((workspace) => !paths.includes(workspace.canonical_path)),
        currentProjectId: null,
      }));
    }, addedPaths).catch(() => {});
    await page.close();
  }
});

test("clearing a project removes app records without implying disk deletion", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  // The preview data intentionally starts this project with a live task. Stop it through the
  // same mock IPC runtime so the product guard and the successful removal path are both exercised.
  await page.evaluate(async () => {
    const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
    await browserMockInvoke("cmd_agent_abort", { taskId: "mock-task-api" });
  });

  await page.getByRole("button", { name: "管理项目" }).click();
  const row = page.locator(".workspace-row").filter({ hasText: "api-server" });
  const remove = row.getByRole("button", { name: "从 R-Code 中清除 api-server" });
  await page.waitForFunction(
    () => !document.querySelector('.workspace-row .workspace-remove[aria-label*="api-server"]')?.hasAttribute("disabled"),
  );
  await remove.click();

  const dialog = page.getByRole("alertdialog", { name: "从 R-Code 中清除这个项目？" });
  await dialog.waitFor({ state: "visible" });
  const copy = await dialog.innerText();
  assert.match(copy, /真实文件夹及其中的文件不会被删除、移动或修改/);
  assert.match(copy, /1 段对话以及关联的运行与审计数据/);
  await dialog.getByRole("button", { name: "清除项目", exact: true }).click();

  await page.getByText("项目已从 R-Code 清除", { exact: true }).waitFor({ state: "visible" });
  assert.equal(await page.locator(".workspace-row").filter({ hasText: "api-server" }).count(), 0);
  assert.equal(await page.locator(".sidebar-project").filter({ hasText: "api-server" }).count(), 0);

  await page.locator(".sidebar-nav-item").filter({ hasText: "对话" }).click();
  assert.equal(await page.locator(".conversation-row").filter({ hasText: "添加请求限流中间件" }).count(), 0);
  await page.close();
});
