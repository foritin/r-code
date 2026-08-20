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

async function openProjectFiles(page, workspacePath = "D:/project/rust/r-code") {
  await page.evaluate(async (path) => {
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    useTasksStore.getState().setCurrentProject(path);
    useAppStore.setState({ editorFile: null });
    useAppStore.getState().setScene("editor");
  }, workspacePath);
  await page.locator(".file-workspace").waitFor({ state: "visible" });
}

async function openKnowledgeSettings(page, tab = "memory") {
  await page.evaluate(async (target) => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().openKnowledge(target);
  }, tab);
  const center = page.getByRole("region", { name: "知识与指令" });
  await center.waitFor({ state: "visible" });
  return center;
}

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

test("product mode labels collapse compatibility policies into Agent and Plan", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const labels = await page.evaluate(async () => {
    const { modeLabel, modeShortLabel } = await import("/src/lib/format.ts");
    return ["ask", "edit", "auto", "plan"].map((mode) => ({
      mode,
      short: modeShortLabel(mode),
      long: modeLabel(mode),
    }));
  });
  assert.deepEqual(labels.map((item) => item.short), ["Agent", "Agent", "Agent", "Plan"]);
  assert.ok(labels.slice(0, 3).every((item) => item.long.startsWith("Agent —")));
  assert.match(labels[3].long, /^Plan —/);
  await page.close();
});

test("Windows verbatim paths stay canonical internally but render without the device prefix", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const rendered = await page.evaluate(async () => {
    const { displayPath, displayPathsInText, toolTarget } = await import("/src/lib/format.ts");
    const { summarizeOutput } = await import("/src/components/room/model.ts");
    const drive = String.raw`\\?\D:\project\rust\r-code\src-tauri`;
    const unc = String.raw`\\?\UNC\server\share\r-code`;
    const volume = String.raw`\\?\Volume{1234}\r-code`;
    const command = `type ${drive}`;
    const historicalError = JSON.stringify({ error: `failed to edit ${drive}` });
    const historicalJson = displayPathsInText(JSON.stringify({ path: drive, unc, volume }));
    return {
      drive: displayPath(drive),
      unc: displayPath(unc),
      volume: displayPath(volume),
      pathTarget: toolTarget(JSON.stringify({ path: drive })),
      commandTarget: toolTarget(JSON.stringify({ command })),
      queryTarget: toolTarget(JSON.stringify({ query: drive })),
      historicalSummary: summarizeOutput(historicalError, true),
      historicalJson: JSON.parse(historicalJson),
    };
  });

  assert.deepEqual(rendered, {
    drive: String.raw`D:\project\rust\r-code\src-tauri`,
    unc: String.raw`\\server\share\r-code`,
    volume: String.raw`\\?\Volume{1234}\r-code`,
    pathTarget: String.raw`D:\project\rust\r-code\src-tauri`,
    commandTarget: String.raw`type \\?\D:\project\rust\r-code\src-tauri`,
    queryTarget: String.raw`\\?\D:\project\rust\r-code\src-tauri`,
    historicalSummary: String.raw`failed to edit D:\project\rust\r-code\src-tauri`,
    historicalJson: {
      path: String.raw`D:\project\rust\r-code\src-tauri`,
      unc: String.raw`\\server\share\r-code`,
      volume: String.raw`\\?\Volume{1234}\r-code`,
    },
  });
  await page.close();
});

test("permission control stays visible and explains the workspace boundary", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    useTasksStore.getState().setCurrentProject(null);
    useAppStore.getState().setScene("home");
  });

  const trigger = page.getByRole("button", { name: /^权限：/ });
  await trigger.waitFor({ state: "visible" });
  assert.equal(await trigger.isDisabled(), false, "the boundary explanation must remain discoverable");
  await trigger.click();

  const menu = page.getByRole("menu", { name: "项目 Agent 权限" });
  await menu.waitFor({ state: "visible" });
  await menu.getByText("先附加文件夹，才能设置 Agent 的本地工具权限。", { exact: true }).waitFor({ state: "visible" });
  const options = menu.getByRole("menuitemradio");
  assert.equal(await options.count(), 3);
  assert.equal(await options.nth(0).isDisabled(), true);
  assert.equal(await options.nth(1).isDisabled(), true);
  assert.equal(await options.nth(2).isDisabled(), true);

  await page.close();
});

test("permission choices persist and remain available while a run is active", async () => {
  const page = await browser.newPage({ viewport: { width: 1280, height: 840 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const { workspaceSetAccessMode } = await import("/src/lib/ipc.ts");
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    await workspaceSetAccessMode("D:/project/rust/r-code", "risk_based");
    await useTasksStore.getState().refreshWorkspaces();
    useTasksStore.getState().setCurrentProject("D:/project/rust/r-code");
    useAppStore.getState().setScene("home");
  });

  const homeTrigger = page.locator(".scene-home .project-access-trigger");
  await homeTrigger.waitFor({ state: "visible" });
  assert.match(await homeTrigger.innerText(), /权限：替我审批/);
  await homeTrigger.click();
  await page.getByRole("menuitemradio", { name: /完全访问权限/ }).click();
  await page.waitForFunction(() => document.querySelector(".scene-home .project-access-trigger")?.textContent?.includes("完全访问权限"));
  const persistedFromHome = await page.evaluate(async () => {
    const { useTasksStore } = await import("/src/store/tasks.ts");
    return useTasksStore.getState().workspaces.find((workspace) => workspace.canonical_path === "D:/project/rust/r-code")?.access_mode;
  });
  assert.equal(persistedFromHome, "full_access");

  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    await Promise.all([
      useTasksStore.getState().refreshTasks(),
      useTasksStore.getState().refreshDetail("mock-task-queue"),
      useTasksStore.getState().refreshWorkspaces(),
    ]);
    useAppStore.getState().openRoom("mock-task-queue");
  });

  const roomTrigger = page.locator(".scene-room .project-access-trigger");
  await roomTrigger.waitFor({ state: "visible" });
  assert.equal(await roomTrigger.isDisabled(), false, "active runs may configure the next run's permission snapshot");
  await roomTrigger.click();
  await page.getByText("当前运行继续使用启动时的权限；新设置从下一轮开始生效。", { exact: true }).waitFor({ state: "visible" });
  if (process.env.R_CODE_PERMISSION_SHOT) {
    await page.screenshot({ path: process.env.R_CODE_PERMISSION_SHOT, fullPage: true });
  }
  await page.getByRole("menuitemradio", { name: /请求审批/ }).click();
  await page.waitForFunction(() => document.querySelector(".scene-room .project-access-trigger")?.textContent?.includes("请求审批"));

  const persistedFromRoom = await page.evaluate(async () => {
    const { useTasksStore } = await import("/src/store/tasks.ts");
    return useTasksStore.getState().workspaces.find((workspace) => workspace.canonical_path === "D:/project/rust/r-code")?.access_mode;
  });
  assert.equal(persistedFromRoom, "request_approval");

  await page.close();
});

test("native workspace picker closes its menu without stealing focus back from the system dialog", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  try {
    await page.evaluate(async () => {
      const { useAppStore } = await import("/src/store/app.ts");
      const { useTasksStore } = await import("/src/store/tasks.ts");
      useTasksStore.getState().setCurrentProject(null);
      useAppStore.getState().setScene("home");
      globalThis.__rCodeBrowserMockDelayMs = { cmd_workspace_choose: 250 };
    });

    const trigger = page.locator(".scene-home .scope-pill");
    await trigger.click();
    await page.getByRole("menu", { name: "会话可访问的文件夹" })
      .getByRole("menuitem", { name: /选择文件夹/ })
      .click();
    await page.waitForTimeout(50);
    assert.equal(
      await trigger.evaluate((element) => document.activeElement === element),
      false,
      "closing a launcher for a native dialog must not immediately refocus its trigger",
    );
    await page.waitForFunction(async () => {
      const { useTasksStore } = await import("/src/store/tasks.ts");
      return useTasksStore.getState().currentProjectId === "D:/project/rust/r-code";
    });
  } finally {
    await page.evaluate(() => { delete globalThis.__rCodeBrowserMockDelayMs; }).catch(() => {});
    await page.close();
  }
});

test("room project attachment is run-safe, one-time, and synchronizes the next-conversation scope", async () => {
  const page = await browser.newPage({ viewport: { width: 1280, height: 840 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  let taskId;
  try {
    taskId = await page.evaluate(async () => {
      const { taskCreate } = await import("/src/lib/ipc.ts");
      const { useAppStore } = await import("/src/store/app.ts");
      const { useTasksStore } = await import("/src/store/tasks.ts");
      const task = await taskCreate(null, "附加项目安全测试", "", "ask");
      await Promise.all([
        useTasksStore.getState().refreshTasks(),
        useTasksStore.getState().refreshDetail(task.id),
      ]);
      useTasksStore.getState().setCurrentProject("D:/project/rust/api-server");
      useAppStore.getState().openRoom(task.id);
      return task.id;
    });

    await page.getByRole("button", { name: "附加文件夹", exact: true }).waitFor({ state: "visible" });
    await page.evaluate(async (id) => {
      const { useTasksStore } = await import("/src/store/tasks.ts");
      const state = useTasksStore.getState();
      const detail = state.details[id];
      const fixtureRun = state.details["mock-task-queue"]?.runs.find((run) => run.ended_at == null);
      const runningTask = { ...detail.task, state: "in_progress", updated_at: new Date().toISOString() };
      useTasksStore.setState({
        tasks: state.tasks.map((item) => item.id === id ? runningTask : item),
        details: {
          ...state.details,
          [id]: {
            ...detail,
            task: runningTask,
            runs: [{
              ...(fixtureRun ?? {}),
              id: `${id}-ui-running`,
              task_id: id,
              ended_at: null,
            }],
          },
        },
      });
    }, taskId);

    const blockedAttach = page.getByRole("button", { name: "运行结束后可附加", exact: true });
    await blockedAttach.waitFor({ state: "visible" });
    assert.equal(await blockedAttach.isDisabled(), true);
    assert.match(await blockedAttach.getAttribute("title"), /当前运行结束后/);
    await page.waitForFunction(async () => {
      const { useTasksStore } = await import("/src/store/tasks.ts");
      return useTasksStore.getState().currentProjectId === null;
    });

    await page.evaluate(async (id) => {
      const { useTasksStore } = await import("/src/store/tasks.ts");
      const state = useTasksStore.getState();
      const detail = state.details[id];
      const idleTask = { ...detail.task, state: "idle", updated_at: new Date().toISOString() };
      useTasksStore.setState({
        tasks: state.tasks.map((item) => item.id === id ? idleTask : item),
        details: { ...state.details, [id]: { ...detail, task: idleTask, runs: [] } },
      });
    }, taskId);

    const attach = page.getByRole("button", { name: "附加文件夹", exact: true });
    await attach.waitFor({ state: "visible" });
    assert.equal(await attach.isDisabled(), false);
    await attach.click();
    await page.locator(".room-conversation-title").getByText(/r-code/).waitFor({ state: "visible" });
    await page.waitForFunction(async () => {
      const { useTasksStore } = await import("/src/store/tasks.ts");
      return useTasksStore.getState().currentProjectId === "D:/project/rust/r-code";
    });

    await page.getByRole("button", { name: "添加到任务", exact: true }).click();
    const addDialog = page.getByRole("dialog", { name: "添加到任务" });
    await addDialog.getByRole("button", { name: /^附加文件夹中的文件/ }).waitFor({ state: "visible" });
    await page.keyboard.press("Escape");

    const binding = await page.evaluate(async (id) => {
      const { taskDetail, taskSetWorkspace } = await import("/src/lib/ipc.ts");
      let error = "";
      try {
        await taskSetWorkspace(id, "D:/project/rust/api-server");
      } catch (cause) {
        error = String(cause);
      }
      return { error, workspacePath: (await taskDetail(id)).task.workspace_path };
    }, taskId);
    assert.match(binding.error, /已绑定项目/);
    assert.equal(binding.workspacePath, "D:/project/rust/r-code");
  } finally {
    if (taskId) {
      await page.evaluate(async (id) => {
        const { taskDelete } = await import("/src/lib/ipc.ts");
        const { useTasksStore } = await import("/src/store/tasks.ts");
        await taskDelete(id).catch(() => {});
        await useTasksStore.getState().refreshTasks().catch(() => {});
      }, taskId).catch(() => {});
    }
    await page.close();
  }
});

test("macOS uses native traffic-light chrome and Command-key labels", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.addInitScript(() => {
    Object.defineProperty(navigator, "platform", {
      configurable: true,
      get: () => "MacIntel",
    });
  });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const chrome = await page.evaluate(() => {
    const app = document.querySelector("#app");
    const topbar = document.querySelector(".app-topbar");
    if (!(app instanceof HTMLElement) || !(topbar instanceof HTMLElement)) {
      throw new Error("application chrome is missing");
    }
    return {
      macClass: app.classList.contains("platform-macos"),
      paddingLeft: Number.parseFloat(getComputedStyle(topbar).paddingLeft),
      customControls: document.querySelectorAll(".app-window-controls").length,
      topbarDragRegion: topbar.hasAttribute("data-tauri-drag-region"),
      spacerDragRegion: document.querySelector(".topbar-spacer")?.hasAttribute("data-tauri-drag-region") ?? false,
    };
  });
  assert.equal(chrome.macClass, true);
  assert.equal(chrome.customControls, 0);
  assert.equal(chrome.topbarDragRegion, true, "the macOS overlay titlebar must use Tauri's native drag contract");
  assert.equal(chrome.spacerDragRegion, true, "the direct spacer target must remain draggable");
  assert.ok(chrome.paddingLeft >= 70, `traffic lights need a reserved hit area: ${JSON.stringify(chrome)}`);

  await page.locator(".desktop-menu-trigger").filter({ hasText: "文件" }).click();
  const shortcut = page.getByRole("menuitem", { name: /新建任务/ }).locator(".menu-item-key");
  assert.equal(await shortcut.textContent(), "⌘ N");

  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().setScene("settings");
  });
  await page.locator("#app.scene-settings").waitFor({ state: "visible" });
  await page.keyboard.press("Control+N");
  assert.equal(await page.locator("#app.scene-settings").count(), 1, "macOS Control shortcuts must remain available to editors and terminals");
  await page.keyboard.press("Meta+N");
  await page.locator("#app.scene-home").waitFor({ state: "visible" });
  await page.close();
});

test("macOS chrome falls back to the user agent when WebView platform is unavailable", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.addInitScript(() => {
    Object.defineProperty(navigator, "platform", {
      configurable: true,
      get: () => "Unknown",
    });
    Object.defineProperty(navigator, "userAgent", {
      configurable: true,
      get: () => "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_7) AppleWebKit/605.1.15",
    });
  });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  assert.equal(await page.locator("#app.platform-macos").count(), 1);
  assert.equal(await page.locator(".app-window-controls").count(), 0);
  await page.close();
});

test("attachment image preview closes from either side of the dialog", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().setScene("home");
  });
  await page.locator("#app.scene-home").waitFor({ state: "visible" });
  await page.getByRole("button", { name: "添加到任务", exact: true }).click();
  await page.getByRole("dialog", { name: "添加到任务", exact: true }).locator('input[type="file"]').first().setInputFiles({
    name: "preview.png",
    mimeType: "image/png",
    buffer: Buffer.from(
      "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M/wHwAF/gL+X2NDWQAAAABJRU5ErkJggg==",
      "base64",
    ),
  });

  const thumbnail = page.getByRole("button", { name: "预览图片 preview.png", exact: true });
  const dialog = page.getByRole("dialog", { name: "预览图片 preview.png", exact: true });
  const closePreview = dialog.getByRole("button", { name: "关闭预览", exact: true });
  const openPreview = async () => {
    await thumbnail.click();
    await dialog.waitFor({ state: "visible" });
    return page.evaluate(() => {
      const preview = document.querySelector(".attachment-preview");
      if (!(preview instanceof HTMLElement)) throw new Error("attachment preview is missing");
      const rect = preview.getBoundingClientRect();
      const y = Math.max(1, Math.min(window.innerHeight - 2, rect.top + rect.height / 2));
      const left = { x: Math.max(1, rect.left / 2), y };
      const right = { x: Math.min(window.innerWidth - 2, (rect.right + window.innerWidth) / 2), y };
      const hitsBackdrop = ({ x, y: pointY }) => (
        document.elementFromPoint(x, pointY)?.closest(".attachment-preview-backdrop") != null
      );
      return {
        left,
        right,
        leftHitsBackdrop: hitsBackdrop(left),
        rightHitsBackdrop: hitsBackdrop(right),
      };
    });
  };

  const right = await openPreview();
  assert.equal(await closePreview.evaluate((button) => document.activeElement === button), true, "modal focus should start on its close button");
  await page.keyboard.press("Shift+Tab");
  assert.equal(await closePreview.evaluate((button) => document.activeElement === button), true, "modal focus must not escape into the composer");
  assert.equal(right.rightHitsBackdrop, true, "the right empty area should belong to the preview backdrop");
  await page.mouse.click(right.right.x, right.right.y);
  await dialog.waitFor({ state: "detached" });
  assert.equal(await thumbnail.evaluate((button) => document.activeElement === button), true, "closing the preview should restore thumbnail focus");

  const left = await openPreview();
  assert.equal(left.leftHitsBackdrop, true, "the left empty area should belong to the preview backdrop");
  await page.mouse.click(left.left.x, left.left.y);
  await dialog.waitFor({ state: "detached" });

  await openPreview();
  await page.keyboard.press("Escape");
  await dialog.waitFor({ state: "detached" });
  assert.equal(await thumbnail.evaluate((button) => document.activeElement === button), true, "Escape should close the preview and restore focus");

  await openPreview();
  await page.getByRole("button", { name: "删除附件 preview.png", exact: true }).evaluate((button) => button.click());
  await dialog.waitFor({ state: "detached" });
  await page.getByRole("button", { name: "添加到任务", exact: true }).click();
  await page.getByRole("dialog", { name: "添加到任务", exact: true }).locator('input[type="file"]').first().setInputFiles({
    name: "replacement.png",
    mimeType: "image/png",
    buffer: Buffer.from(
      "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M/wHwAF/gL+X2NDWQAAAABJRU5ErkJggg==",
      "base64",
    ),
  });
  await page.getByRole("button", { name: "预览图片 replacement.png", exact: true }).waitFor({ state: "visible" });
  assert.equal(await page.locator(".attachment-preview-backdrop").count(), 0, "a removed image preview must not reappear after adding another attachment");
  await page.close();
});

test("macOS and Windows offer local OCR for images rejected by a text-only model", async () => {
  const attachment = {
    id: "ocr-image",
    name: "screen.png",
    mediaType: "image/png",
    data: "iVBORw0KGgo=",
    kind: "image",
    size: 8,
  };
  const mac = await browser.newPage();
  await mac.addInitScript(() => {
    Object.defineProperty(navigator, "platform", { configurable: true, get: () => "MacIntel" });
  });
  await mac.goto(baseUrl, { waitUntil: "networkidle" });
  const macResult = await mac.evaluate(async ({ attachment }) => {
    const { firstBlockedAttachmentReason, sendableAttachmentInputs } = await import("/src/components/Attachments.tsx");
    const { platformCapabilities } = await import("/src/lib/ipc.ts");
    const unsupported = () => ({ state: "unsupported", reason: "当前模型不支持图片" });
    const capabilities = await platformCapabilities();
    return {
      capabilities,
      blocked: firstBlockedAttachmentReason([attachment], unsupported, capabilities),
      sendable: sendableAttachmentInputs([attachment], unsupported, capabilities),
    };
  }, { attachment });
  assert.equal(macResult.capabilities.platform, "macos");
  assert.equal(macResult.capabilities.nativeOcr, true);
  assert.deepEqual(macResult.capabilities.nativeOcrFormats, ["image/png", "image/jpeg"]);
  assert.equal(macResult.blocked, null);
  assert.equal(macResult.sendable.length, 1);
  assert.equal(macResult.sendable[0].nativeOcr, true);
  await mac.close();

  const windows = await browser.newPage();
  await windows.addInitScript(() => {
    Object.defineProperty(navigator, "platform", { configurable: true, get: () => "Win32" });
  });
  await windows.goto(baseUrl, { waitUntil: "networkidle" });
  const windowsResult = await windows.evaluate(async ({ attachment }) => {
    const { firstBlockedAttachmentReason, sendableAttachmentInputs } = await import("/src/components/Attachments.tsx");
    const { platformCapabilities } = await import("/src/lib/ipc.ts");
    const unsupported = () => ({ state: "unsupported", reason: "当前模型不支持图片" });
    const capabilities = await platformCapabilities();
    return {
      capabilities,
      blocked: firstBlockedAttachmentReason([attachment], unsupported, capabilities),
      sendable: sendableAttachmentInputs([attachment], unsupported, capabilities),
    };
  }, { attachment });
  assert.equal(windowsResult.capabilities.platform, "windows");
  assert.equal(windowsResult.capabilities.nativeOcr, true);
  assert.deepEqual(windowsResult.capabilities.nativeOcrFormats, ["image/png", "image/jpeg"]);
  assert.equal(windowsResult.blocked, null);
  assert.equal(windowsResult.sendable.length, 1);
  assert.equal(windowsResult.sendable[0].nativeOcr, true);
  await windows.close();

  const runningWindows = await browser.newPage();
  await runningWindows.addInitScript(() => {
    Object.defineProperty(navigator, "platform", { configurable: true, get: () => "Win32" });
    // is-deferred 是“运行中引导模式不支持附件”的专属状态；默认策略是排队，
    // 这里显式选成 steer 再附加图片，才会出现延迟 OCR 提示。
    window.localStorage.setItem("r-code:agent-send-mode", "steer");
  });
  await runningWindows.goto(baseUrl, { waitUntil: "networkidle" });
  await runningWindows.evaluate(async () => {
    const { browserMockDetails, browserMockTasks } = await import("/src/lib/mock-data.ts");
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    const task = browserMockTasks.find((candidate) => candidate.id === "mock-task-queue");
    const detail = browserMockDetails["mock-task-queue"];
    if (!task || !detail) throw new Error("running OCR fixture is missing");
    task.provider_name = "deepseek";
    task.model = "deepseek-v4-pro";
    detail.task.provider_name = "deepseek";
    detail.task.model = "deepseek-v4-pro";
    await Promise.all([
      useTasksStore.getState().refreshTasks(),
      useTasksStore.getState().refreshDetail("mock-task-queue"),
      useTasksStore.getState().refreshWorkspaces(),
    ]);
    useAppStore.getState().openRoom("mock-task-queue");
  });
  const runningComposer = runningWindows.locator(".scene-room .composer");
  await runningComposer.getByRole("textbox", { name: "给 Agent 的消息" }).waitFor({ state: "visible" });
  await runningComposer.getByRole("button", { name: "添加到任务" }).click();
  await runningWindows.getByRole("dialog", { name: "添加到任务" }).locator('input[type="file"]').first().setInputFiles({
    name: "screen.png",
    mimeType: "image/png",
    buffer: Buffer.from("iVBORw0KGgo=", "base64"),
  });
  const deferredOcr = runningComposer.locator(".attachment-chip.is-native-ocr");
  await deferredOcr.waitFor({ state: "visible" });
  assert.match(await deferredOcr.innerText(), /本机 OCR → 文本/);
  assert.match(await deferredOcr.getAttribute("class"), /is-deferred/);
  assert.doesNotMatch(await deferredOcr.getAttribute("class"), /is-unsupported/);
  assert.equal(await deferredOcr.locator(".attachment-warning").count(), 0, "deferred OCR must not look like a deleted attachment");
  assert.equal(
    await deferredOcr.locator(".attachment-label").evaluate((element) => getComputedStyle(element).textDecorationLine),
    "none",
    "a retained OCR attachment must not be struck through",
  );
  assert.match(await deferredOcr.getAttribute("title"), /当前运行结束后/);
  await runningWindows.close();

  const linux = await browser.newPage();
  await linux.addInitScript(() => {
    Object.defineProperty(navigator, "platform", { configurable: true, get: () => "Linux x86_64" });
  });
  await linux.goto(baseUrl, { waitUntil: "networkidle" });
  const linuxResult = await linux.evaluate(async ({ attachment }) => {
    const { firstBlockedAttachmentReason, sendableAttachmentInputs } = await import("/src/components/Attachments.tsx");
    const { platformCapabilities } = await import("/src/lib/ipc.ts");
    const unsupported = () => ({ state: "unsupported", reason: "当前模型不支持图片" });
    const capabilities = await platformCapabilities();
    return {
      capabilities,
      blocked: firstBlockedAttachmentReason([attachment], unsupported, capabilities),
      sendable: sendableAttachmentInputs([attachment], unsupported, capabilities),
    };
  }, { attachment });
  assert.equal(linuxResult.capabilities.platform, "linux");
  assert.equal(linuxResult.capabilities.nativeOcr, false);
  assert.match(linuxResult.blocked, /不支持图片/);
  assert.deepEqual(linuxResult.sendable, []);
  await linux.close();
});

test("queued attachment metadata parses image and text attachments safely", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const result = await page.evaluate(async () => {
    const {
      parseQueuedAttachments,
      firstImageAttachment,
      imageAttachmentCount,
    } = await import("/src/lib/queued-attachments.ts");
    const parsed = parseQueuedAttachments(JSON.stringify([
      { name: "screen.png", media_type: "image/png", kind: "image", preview_id: "preview-1" },
      { name: "notes.md", media_type: "text/markdown", kind: "text", data: "bm90ZXM=" },
    ]));
    return {
      count: parsed.length,
      imageCount: imageAttachmentCount(parsed),
      first: firstImageAttachment(parsed),
      invalid: parseQueuedAttachments("{not json"),
      empty: parseQueuedAttachments(null),
    };
  });
  assert.equal(result.count, 2);
  assert.equal(result.imageCount, 1);
  assert.equal(result.first.kind, "image");
  assert.equal(result.first.name, "screen.png");
  assert.equal(result.first.preview_id, "preview-1");
  assert.deepEqual(result.invalid, []);
  assert.deepEqual(result.empty, []);
  await page.close();
});

test("optimistic send previews show the original image name, never the .ocr.txt alias", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const metas = await page.evaluate(async () => {
    const { optimisticAttachmentMeta } = await import("/src/components/Attachments.tsx");
    return optimisticAttachmentMeta([
      { name: "screen.png", mediaType: "image/png", data: "iVBORw0KGgo=", nativeOcr: true },
      { name: "notes.md", mediaType: "text/markdown", data: "bm90ZXM=" },
    ]);
  });
  assert.equal(metas.length, 2);
  assert.deepEqual(
    {
      kind: metas[0].kind,
      name: metas[0].name,
      preview: metas[0].previewUrl,
    },
    {
      kind: "image",
      name: "screen.png",
      preview: "data:image/png;base64,iVBORw0KGgo=",
    },
  );
  assert.equal(metas[1].kind, "text");
  assert.equal(metas[1].previewUrl, undefined);
  assert.doesNotMatch(JSON.stringify(metas), /\.ocr\.txt/);
  await page.close();
});

test("timeline image attachments lazy-load a thumbnail and open in the shared lightbox", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const taskId = "mock-task-complete";

  const baseline = await page.evaluate(async (id) => {
    const { browserMockMessages, browserMockSetMessages } = await import("/src/lib/mock-data.ts");
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    const original = browserMockMessages(id);
    browserMockSetMessages(id, [
      {
        id: `${id}-message-img`,
        branch_id: "main",
        kind: "message",
        role: "user",
        text: "看看这张图",
        image_count: 1,
        image_media_types: ["image/png"],
        attachments: [
          { name: "screen.png", media_type: "image/png", kind: "image", preview_id: "timeline-preview" },
        ],
      },
      {
        id: `${id}-message-txt`,
        branch_id: "main",
        kind: "message",
        role: "user",
        text: "附带一个文本附件",
        attachments: [
          { name: "notes.md", media_type: "text/markdown", kind: "text" },
        ],
      },
    ]);
    await useTasksStore.getState().refreshDetail(id);
    useAppStore.getState().openRoom(id);
    return { original };
  }, taskId);

  try {
    const timeline = page.locator(".timeline");
    await timeline.getByText("看看这张图").waitFor({ state: "visible" });

    const thumbnail = timeline
      .locator(".message-attachment-item.kind-image.is-previewable .message-attachment-thumbnail");
    await thumbnail.waitFor({ state: "visible" });
    assert.match(
      await thumbnail.getAttribute("src") ?? "",
      /^data:image\/png;base64,/,
      "lazy-loaded preview must be an in-memory data URL",
    );

    await timeline.locator(".message-attachment-item.kind-image.is-previewable").click();
    const dialog = page.getByRole("dialog", { name: "预览图片 screen.png" });
    await dialog.waitFor({ state: "visible" });
    assert.equal(await dialog.locator("header span").innerText(), "screen.png");

    await page.keyboard.press("Escape");
    await dialog.waitFor({ state: "hidden" });

    assert.equal(
      await timeline.locator(".message-attachment-item.kind-text").count(),
      1,
      "non-image attachments must keep the existing icon chip",
    );
    assert.equal(
      await timeline.locator(".message-attachment-item.kind-text img").count(),
      0,
    );
  } finally {
    await page.evaluate(async ({ id, original }) => {
      const { browserMockSetMessages } = await import("/src/lib/mock-data.ts");
      browserMockSetMessages(id, original);
    }, { id: taskId, original: baseline.original });
    await page.close();
  }
});

test("queued image attachments show a thumbnail and open the shared lightbox", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const taskId = "mock-task-queue";

  const baseline = await page.evaluate(async (id) => {
    const { browserMockDetails } = await import("/src/lib/mock-data.ts");
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    const detail = browserMockDetails[id];
    const original = structuredClone(detail.queued_messages);
    const now = new Date().toISOString();
    detail.queued_messages = [
      {
        id: "queue-img",
        task_id: id,
        branch_id: detail.active_branch.id,
        message: "看看这张图",
        state: "queued",
        priority: 0,
        attachments_json: JSON.stringify([
          { name: "screen.png", media_type: "image/png", kind: "image", preview_id: "queue-preview" },
          { name: "notes.md", media_type: "text/markdown", kind: "text", data: "bm90ZXM=" },
        ]),
        created_at: now,
        updated_at: now,
      },
      {
        id: "queue-multi",
        task_id: id,
        branch_id: detail.active_branch.id,
        message: "多图消息",
        state: "queued",
        priority: 0,
        attachments_json: JSON.stringify([
          { name: "a.png", media_type: "image/png", kind: "image", preview_id: "queue-a" },
          { name: "b.png", media_type: "image/png", kind: "image", preview_id: "queue-b" },
        ]),
        created_at: now,
        updated_at: now,
      },
      {
        id: "queue-text",
        task_id: id,
        branch_id: detail.active_branch.id,
        message: "纯文本消息",
        state: "queued",
        priority: 0,
        attachments_json: null,
        created_at: now,
        updated_at: now,
      },
    ];
    await useTasksStore.getState().refreshDetail(id);
    useAppStore.getState().openRoom(id);
    return { original };
  }, taskId);

  try {
    const queue = page.locator(".composer-queue-stack");
    await queue.waitFor({ state: "visible" });

    const imageRow = queue.locator("li[data-queue-id='queue-img']");
    await imageRow.locator(".queue-image-thumb").waitFor({ state: "visible" });
    await imageRow.locator(".queue-image-thumb").click();
    const dialog = page.getByRole("dialog", { name: "预览图片 screen.png" });
    await dialog.waitFor({ state: "visible" });
    await page.keyboard.press("Escape");
    await dialog.waitFor({ state: "hidden" });

    const multiRow = queue.locator("li[data-queue-id='queue-multi']");
    assert.equal(await multiRow.locator(".queue-image-count").innerText(), "+1");

    const textRow = queue.locator("li[data-queue-id='queue-text']");
    assert.equal(await textRow.locator(".queue-kind-icon").count(), 1);
    assert.equal(await textRow.locator(".queue-image-thumb").count(), 0);
  } finally {
    await page.evaluate(async ({ id, original }) => {
      const { browserMockDetails } = await import("/src/lib/mock-data.ts");
      browserMockDetails[id].queued_messages = original;
    }, { id: taskId, original: baseline.original });
    await page.close();
  }
});

test("Windows close hides to the tray while explicit quit stays discoverable", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.addInitScript(() => {
    Object.defineProperty(navigator, "platform", {
      configurable: true,
      get: () => "Win32",
    });
    globalThis.__rCodeQuitInvocations = 0;
    globalThis.__rCodePerformanceIpcProbe = (command) => {
      if (command === "cmd_app_quit") globalThis.__rCodeQuitInvocations += 1;
    };
  });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const closeButton = page.getByRole("button", { name: "关闭到系统托盘" });
  assert.equal(
    await closeButton.getAttribute("title"),
    "关闭到系统托盘，后台任务继续运行",
  );

  await page.locator(".desktop-menu-trigger").filter({ hasText: "文件" }).click();
  await page.getByRole("menuitem", { name: "隐藏到系统托盘" }).waitFor({ state: "visible" });
  await page.getByRole("menuitem", { name: "退出 R-Code" }).click();
  await page.waitForFunction(() => globalThis.__rCodeQuitInvocations === 1);

  await page.close();
});

test("Codex login watcher is bounded and never schedules beyond its deadline", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const contract = await page.evaluate(async () => {
    const watcher = await import("/src/components/codex/login-watcher.ts");
    const timeout = watcher.CODEX_LOGIN_WAIT_TIMEOUT_MS;
    return {
      interval: watcher.CODEX_LOGIN_POLL_INTERVAL_MS,
      timeout,
      initialDelay: watcher.nextCodexLoginPollDelay(10_000, 10_000),
      finalDelay: watcher.nextCodexLoginPollDelay(10_000, 10_000 + timeout - 750),
      atDeadline: watcher.nextCodexLoginPollDelay(10_000, 10_000 + timeout),
    };
  });

  assert.deepEqual(contract, {
    interval: 2_000,
    timeout: 180_000,
    initialDelay: 2_000,
    finalDelay: 750,
    atDeadline: null,
  });
  await page.close();
});

test("sidebar status uses a loading spinner while live, orange while waiting, and green after completion", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const colors = await page.evaluate(() => {
    const tokenColor = (token) => {
      const probe = document.createElement("i");
      probe.style.backgroundColor = `var(${token})`;
      document.body.append(probe);
      const color = getComputedStyle(probe).backgroundColor;
      probe.remove();
      return color;
    };
    const dotFor = (title) => {
      const rows = [...document.querySelectorAll(".sidebar-task-row")];
      const row = rows.find((candidate) => candidate.textContent?.includes(title));
      const dot = row?.querySelector(".task-state-dot");
      return dot instanceof HTMLElement ? getComputedStyle(dot).backgroundColor : null;
    };
    return {
      warning: tokenColor("--warning"),
      success: tokenColor("--success"),
      running: dotFor("修复任务队列并发问题"),
      waitingWhileRunning: dotFor("优化 Rust 编译性能"),
      reviewReady: dotFor("统一错误处理规范"),
      finished: dotFor("更新依赖并修复告警"),
    };
  });

  assert.notEqual(colors.running, colors.warning, "a live task shows a loading spinner, not the warning color");
  assert.equal(colors.waitingWhileRunning, colors.warning, "a task waiting for permission keeps the warning color");
  assert.equal(colors.reviewReady, colors.success);
  assert.equal(colors.finished, null,
    "an ordinary finished task stays quiet with no state marker (prototype C quiet list)");
  assert.notEqual(colors.running, colors.finished);

  const runningAnimation = await page.evaluate(() => {
    const rows = [...document.querySelectorAll(".sidebar-task-row")];
    const row = rows.find((candidate) => candidate.textContent?.includes("修复任务队列并发问题"));
    const dot = row?.querySelector(".task-state-dot");
    if (!(dot instanceof HTMLElement)) throw new Error("missing running sidebar dot");
    return getComputedStyle(dot).animationName;
  });
  assert.notEqual(runningAnimation, "none", "running dot must animate");

  await page.evaluate(async () => {
    const { useTasksStore } = await import("/src/store/tasks.ts");
    // Sidebar bootstrap intentionally loads task summaries only. Fetch the detail before
    // mutating its latest run so this assertion exercises the real presentation path instead of
    // depending on another test having opened the conversation first.
    await useTasksStore.getState().refreshDetail("mock-task-complete");
    const state = useTasksStore.getState();
    const detail = state.details["mock-task-complete"];
    if (!detail) throw new Error("completed task detail is missing");
    useTasksStore.setState({
      details: {
        ...state.details,
        [detail.task.id]: {
          ...detail,
          runs: detail.runs.map((run, index) => index === 0
            ? { ...run, review_state: "failed", ended_at: run.ended_at ?? new Date().toISOString() }
            : run),
        },
      },
    });
  });
  const completedWithError = page.locator(".sidebar-task").filter({ hasText: "更新依赖并修复告警" });
  await page.waitForFunction(() => document.querySelector(".sidebar-task[title*='已完成（含错误）']") != null);
  assert.match(await completedWithError.getAttribute("title"), /已完成（含错误）/);
  assert.equal(
    await completedWithError.locator(".task-state-dot").count(),
    0,
    "an ended session stays quiet (no marker) even when its latest run retains an error",
  );

  await page.locator(".sidebar-nav-item").filter({ hasText: "对话" }).click();
  await page.locator("#main-content > .scene-conversations").waitFor({ state: "visible" });
  await page.locator(".conversation-row").filter({ hasText: "修复任务队列并发问题" }).waitFor({ state: "visible" });
  const conversationColors = await page.evaluate(() => {
    const statusFor = (title) => {
      const rows = [...document.querySelectorAll(".conversation-row")];
      const row = rows.find((candidate) => candidate.textContent?.includes(title));
      const status = row?.querySelector(".conversation-status i");
      if (!(status instanceof HTMLElement)) throw new Error(`missing conversation status: ${title}`);
      return getComputedStyle(status).backgroundColor;
    };
    return {
      running: statusFor("修复任务队列并发问题"),
      finished: statusFor("更新依赖并修复告警"),
    };
  });
  assert.equal(conversationColors.running, colors.warning);
  assert.equal(conversationColors.finished, colors.success);
  await page.close();
});

test("Codex one-click setup resumes automatically after browser login", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const normalizedLegacyPane = await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().setSettingsPane("codex");
    useAppStore.getState().setScene("settings");
    return useAppStore.getState().settingsPane;
  });
  assert.equal(normalizedLegacyPane, "subagents", "legacy codex settings key must normalize to the subagents module");

  const subagentSettings = page.getByRole("button", { name: "子代理配置", exact: true });
  await subagentSettings.waitFor({ state: "visible" });
  assert.equal(await subagentSettings.getAttribute("aria-current"), "page");
  assert.equal(
    await page.getByRole("navigation", { name: "设置分类" }).getByRole("button", { name: "Codex CLI", exact: true }).count(),
    0,
    "Codex CLI may be a candidate source, never the top-level settings module",
  );
  assert.equal(await page.getByRole("heading", { name: "子代理配置", exact: true }).count(), 1);
  assert.equal(await page.locator(".local-integrations-panel").count(), 0);
  assert.equal(await page.getByText("其他 Agent 与开发工具", { exact: true }).count(), 0);

  const setup = page.locator(".codex-setup");
  await setup.waitFor({ state: "visible" });
  await setup.getByRole("button", { name: "安装并继续" }).click();
  const gate = page.locator(".codex-gate-dialog");
  await gate.getByRole("button", { name: "确认并安装" }).click();
  await gate.getByRole("button", { name: "使用浏览器登录" }).click();

  await gate.waitFor({ state: "detached", timeout: 10_000 });
  await setup.locator(".codex-setup-status-copy strong", { hasText: "Codex 已就绪" })
    .waitFor({ state: "visible", timeout: 10_000 });
  assert.equal(await setup.locator(".codex-setup-steps li.done").count(), 3);
  await page.close();
});

test("Codex subagent switch persists immediately and remains reversible", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 900 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const { codexInstallCli, codexSetupCollaboration, codexStartLogin } = await import("/src/lib/ipc.ts");
    const { useAppStore } = await import("/src/store/app.ts");
    await codexInstallCli();
    await codexStartLogin();
    await codexSetupCollaboration();
    useAppStore.getState().setSettingsPane("subagents");
    useAppStore.getState().setScene("settings");
  });

  const toggle = page.locator("#codex-subagent-enabled");
  await toggle.waitFor({ state: "visible" });
  assert.equal(await toggle.isChecked(), true);
  await toggle.click();
  await page.getByText("Codex 子代理已关闭；之后的新委派会自动改用 R-Code。", { exact: true })
    .waitFor({ state: "visible" });
  assert.equal(await toggle.isChecked(), false);
  assert.equal(await page.evaluate(async () => {
    const { settingsGet } = await import("/src/lib/ipc.ts");
    return (await settingsGet()).config.orchestration?.allow_cross_engine_delegation;
  }), false);

  await toggle.click();
  await page.getByText("Codex 子代理已开启；之后的新委派可以使用 Codex。", { exact: true })
    .waitFor({ state: "visible" });
  assert.equal(await toggle.isChecked(), true);
  await page.close();
});

test("subagent configuration supports repeatable weighted slots, editable prompts, and fail-closed probes", async () => {
  const page = await browser.newPage({ viewport: { width: 1180, height: 920 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const originalPool = await page.evaluate(async () => {
    const {
      codexInstallCli,
      codexSetupCollaboration,
      codexStartLogin,
      subagentPoolSnapshot,
    } = await import("/src/lib/ipc.ts");
    const { useAppStore } = await import("/src/store/app.ts");
    await codexInstallCli();
    await codexStartLogin();
    await codexSetupCollaboration();
    const snapshot = await subagentPoolSnapshot();
    useAppStore.getState().setSettingsPane("subagents");
    useAppStore.getState().setScene("settings");
    return snapshot.pool;
  });

  try {
    const panel = page.locator(".subagent-providers-panel");
    await panel.waitFor({ state: "visible" });
    assert.equal(await page.getByRole("heading", { name: "子代理配置", exact: true }).count(), 1);
    assert.equal(await panel.getByText("Reasonix", { exact: false }).count(), 0);
    assert.equal(await panel.getByText("Claude Code", { exact: false }).count(), 0);
    assert.equal(await panel.getByText("OpenCode", { exact: false }).count(), 0);
    assert.equal(await panel.getByText("Gemini CLI", { exact: false }).count(), 0);

    await panel.getByRole("button", { name: "全部测试", exact: true }).click();
    await panel.getByText(/批量测试完成：2\/3 项已连通/).waitFor({ state: "visible" });
    const deepseek = panel.locator('[data-source-key="api:deepseek"]');
    const openai = panel.locator('[data-source-key="api:openai"]');
    const codex = panel.locator('[data-source-key="codex_cli"]');
    assert.match(await deepseek.innerText(), /连接失败/);
    assert.match(await openai.innerText(), /已连通/);
    assert.match(await codex.innerText(), /已连通/);
    assert.match(await codex.innerText(), /叶节点 · 不继续派生/);
    assert.match(await openai.innerText(), /原生节点 · 可继续委派/);

    const add = panel.getByTestId("subagent-add-slot");
    await add.click();
    await add.click();
    await add.click();
    const cards = panel.getByTestId("subagent-slot-card");
    assert.equal(await cards.count(), 3, "the pool must allow three stable slots");
    assert.equal(await add.isDisabled(), true, "a fourth slot must never be addable");
    await panel.getByRole("button", { name: "删除槽位 3", exact: true }).click();
    assert.equal(await cards.count(), 2);

    const sources = panel.getByRole("combobox", { name: /槽位 \d 来源/ });
    assert.deepEqual(await sources.evaluateAll((items) => items.map((item) => item.value)), ["api:openai", "api:openai"]);
    assert.equal(
      await sources.first().locator('option[value="api:deepseek"]').evaluate((option) => option.disabled),
      true,
      "a failed source must stay visible but cannot be selected",
    );

    const weights = panel.getByRole("spinbutton", { name: /槽位 \d 权重/ });
    await weights.nth(0).fill("60");
    await weights.nth(1).fill("39");
    const save = panel.getByTestId("subagent-save-pool");
    await panel.getByText("权重合计必须为 100%，当前为 99%。", { exact: true }).waitFor({ state: "visible" });
    assert.equal(await save.isDisabled(), true);
    await weights.nth(1).fill("40");

    const models = panel.getByRole("textbox", { name: /槽位 \d 模型/ });
    await models.nth(1).fill("gpt-5.6-terra");
    await panel.getByText("槽位 2 的来源与模型尚未通过当前配置下的连通测试。", { exact: true })
      .waitFor({ state: "visible" });
    assert.equal(await save.isDisabled(), true);
    await models.nth(1).fill("gpt-5.6-sol");

    const templates = panel.getByRole("combobox", { name: /槽位 \d Prompt 模板/ });
    await templates.nth(0).selectOption("implementation");
    await templates.nth(1).selectOption("test_verification");
    const prompts = panel.getByRole("textbox", { name: /槽位 \d 最终 Prompt/ });
    const implementationPrompt = await prompts.nth(0).inputValue();
    await prompts.nth(0).fill(`${implementationPrompt}\n额外要求：优先保持 CRLF，并报告验证命令。`);

    assert.equal(await save.isDisabled(), false);
    await save.click();
    await panel.getByText("子代理候选池已原子保存。", { exact: true }).waitFor({ state: "visible" });

    const saved = await page.evaluate(async () => {
      const { subagentPoolSnapshot } = await import("/src/lib/ipc.ts");
      return (await subagentPoolSnapshot()).pool;
    });
    assert.equal(saved.slots.length, 2);
    assert.deepEqual(saved.slots.map((slot) => slot.source), [
      { kind: "api_provider", provider_id: "openai" },
      { kind: "api_provider", provider_id: "openai" },
    ]);
    assert.deepEqual(saved.slots.map((slot) => slot.weight), [60, 40]);
    assert.deepEqual(saved.slots.map((slot) => slot.model), ["gpt-5.6-sol", "gpt-5.6-sol"]);
    assert.deepEqual(saved.slots.map((slot) => slot.prompt_template_id), ["implementation", "test_verification"]);
    assert.match(saved.slots[0].prompt, /额外要求：优先保持 CRLF/);

    const staleSaveError = await page.evaluate(async () => {
      const { subagentPoolSave, subagentPoolSnapshot } = await import("/src/lib/ipc.ts");
      const first = await subagentPoolSnapshot();
      const second = await subagentPoolSnapshot();
      await subagentPoolSave(first.revision, first.pool);
      try {
        await subagentPoolSave(second.revision, second.pool);
        return null;
      } catch (error) {
        return String(error);
      }
    });
    assert.match(staleSaveError ?? "", /其他窗口更新|重新加载/);
  } finally {
    await page.evaluate(async (pool) => {
      const { subagentPoolSave, subagentPoolSnapshot } = await import("/src/lib/ipc.ts");
      const current = await subagentPoolSnapshot();
      await subagentPoolSave(current.revision, pool);
    }, originalPool).catch(() => {});
    await page.close();
  }
});

test("RTK setting installs once, configures new model runs globally, and disables without uninstalling", async () => {
  const page = await browser.newPage({ viewport: { width: 860, height: 760 } });
  const consoleErrors = [];
  page.on("console", (message) => {
    if (message.type() === "error") consoleErrors.push(message.text());
  });
  page.on("pageerror", (error) => consoleErrors.push(error.message));
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    globalThis.__rCodeBrowserMockDelayMs = { cmd_rtk_set_enabled: 350 };
    globalThis.__rCodeRtkCalls = [];
    globalThis.__rCodePerformanceIpcProbe = (command, args) => {
      if (command.startsWith("cmd_rtk_")) globalThis.__rCodeRtkCalls.push({ command, args });
    };
    useAppStore.getState().setSettingsPane("tools");
    useAppStore.getState().setScene("settings");
  });

  try {
    const card = page.locator(".rtk-control");
    const toggle = page.getByRole("switch", { name: "为所有模型会话启用 RTK" });
    await card.waitFor({ state: "visible" });
    assert.equal(await toggle.isChecked(), false);

    await toggle.focus();
    assert.equal(await toggle.evaluate((element) => document.activeElement === element), true);
    await page.keyboard.press("Space");
    await card.getByText("正在安装", { exact: true }).waitFor({ state: "visible" });
    assert.equal(await toggle.isChecked(), true, "enable is optimistic while installation runs");
    assert.equal(await toggle.isDisabled(), true);
    await card.getByText("已启用", { exact: true }).waitFor({ state: "visible" });
    assert.match(await card.innerText(), /rtk 0\.45\.0 · R-Code 托管/);
    assert.match(await card.innerText(), /之后启动的所有模型会话与子代理/);
    const bounds = await card.boundingBox();
    assert.ok(bounds && bounds.x >= 0 && bounds.x + bounds.width <= 860, "RTK card must fit the compact settings viewport");
    if (process.env.R_CODE_RTK_SHOT) {
      await page.screenshot({ path: process.env.R_CODE_RTK_SHOT, fullPage: true });
    }

    await toggle.click();
    await card.getByText("已安装", { exact: true }).waitFor({ state: "visible" });
    assert.equal(await toggle.isChecked(), false);
    const disabled = await page.evaluate(async () => {
      const { rtkStatus } = await import("/src/lib/ipc.ts");
      return { status: await rtkStatus(), calls: [...globalThis.__rCodeRtkCalls] };
    });
    assert.equal(disabled.status.enabled, false);
    assert.equal(disabled.status.available, true, "disable must preserve the installed binary");
    assert.deepEqual(
      disabled.calls.filter((call) => call.command === "cmd_rtk_set_enabled").map((call) => call.args.enabled),
      [true, false],
    );
    assert.deepEqual(consoleErrors, []);
  } finally {
    await page.evaluate(() => {
      delete globalThis.__rCodeBrowserMockDelayMs;
      delete globalThis.__rCodePerformanceIpcProbe;
      delete globalThis.__rCodeRtkCalls;
    }).catch(() => {});
    await page.close();
  }
});

test("RTK enable failure rolls the switch back and shows only a short-lived safe message", async () => {
  const page = await browser.newPage({ viewport: { width: 760, height: 700 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    globalThis.__rCodeBrowserMockDelayMs = { cmd_rtk_set_enabled: 250 };
    globalThis.__rCodeBrowserMockFailures = {
      cmd_rtk_set_enabled: "PRIVATE_INSTALL_DIAGNOSTIC should never enter the settings UI",
    };
    useAppStore.getState().setSettingsPane("tools");
    useAppStore.getState().setScene("settings");
  });

  try {
    const toggle = page.getByRole("switch", { name: "为所有模型会话启用 RTK" });
    await toggle.waitFor({ state: "visible" });
    assert.equal(await toggle.isChecked(), false);
    await toggle.click();
    assert.equal(await toggle.isChecked(), true);

    const toast = page.locator(".toast--warn").filter({ hasText: "RTK 未能启用" });
    await toast.waitFor({ state: "visible" });
    assert.equal(await toggle.isChecked(), false, "failed enable must visibly spring back off");
    assert.match(await toast.innerText(), /详细原因已写入诊断日志/);
    assert.ok(!(await page.locator("body").innerText()).includes("PRIVATE_INSTALL_DIAGNOSTIC"));
    if (process.env.R_CODE_RTK_FAILURE_SHOT) {
      await page.screenshot({ path: process.env.R_CODE_RTK_FAILURE_SHOT, fullPage: true });
    }
    await toast.waitFor({ state: "hidden", timeout: 7000 });
  } finally {
    await page.evaluate(() => {
      delete globalThis.__rCodeBrowserMockDelayMs;
      delete globalThis.__rCodeBrowserMockFailures;
    }).catch(() => {});
    await page.close();
  }
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

test("Codex exposes only public reasoning summaries in the timeline", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const contract = await page.evaluate(async () => {
    const { applyAgentEvent, buildTimeline } = await import("/src/components/room/model.ts");
    const history = buildTimeline([
      {
        kind: "system",
        id: "reasoning-history",
        role: null,
        text: "codex_reasoning_summary",
        output_json: JSON.stringify({ text: "已定位委派入口" }),
      },
    ], [], [], new Date().toISOString());
    const live = applyAgentEvent(
      [],
      { type: "activity", phase: "requesting", detail: "Codex 思考摘要：正在核对运行树" },
      1,
      () => "reasoning-live",
    );
    const ordinaryActivity = applyAgentEvent(
      [],
      { type: "activity", phase: "requesting", detail: "private raw reasoning" },
      1,
      () => "private-live",
    );
    return { history, live, ordinaryActivity };
  });

  assert.deepEqual(
    contract.history.map((item) => [item.kind, item.label, item.detail]),
    [["context", "Codex 思考摘要", "已定位委派入口"]],
  );
  assert.deepEqual(
    contract.live.map((item) => [item.kind, item.label, item.detail]),
    [["context", "Codex 思考摘要", "正在核对运行树"]],
  );
  assert.deepEqual(contract.ordinaryActivity, []);
  await page.close();
});

test("provider reasoning is coalesced, separated from answers, and replayable", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const contract = await page.evaluate(async () => {
    const { applyAgentEvent, buildTimeline } = await import("/src/components/room/model.ts");
    const { buildLiveEntries } = await import("/src/components/room/SubagentWorkbench.tsx");
    const history = buildTimeline([
      {
        kind: "system",
        id: "provider-reasoning-history",
        role: null,
        text: "r_code_reasoning",
        output_json: JSON.stringify({ text: "先检查历史上下文" }),
      },
    ], [], [], new Date().toISOString());

    let nextId = 0;
    const nid = () => `reasoning-${nextId += 1}`;
    let live = applyAgentEvent([], { type: "reasoning", text: "先检查", delta: true }, 1, nid);
    live = applyAgentEvent(live, { type: "reasoning", text: "依赖关系", delta: true }, 1, nid);
    live = applyAgentEvent(live, { type: "message", text: "最终回答", delta: true }, 2, nid);
    live = applyAgentEvent(live, { type: "reasoning", text: "迟到思考", delta: true }, 3, nid);

    let acrossTool = applyAgentEvent([], { type: "reasoning", text: "先检查", delta: true }, 1, nid);
    acrossTool = applyAgentEvent(acrossTool, {
      type: "tool_call",
      name: "read_file",
      input: { path: "src/main.rs" },
      call_id: "reasoning-tool",
    }, 2, nid);
    acrossTool = applyAgentEvent(acrossTool, {
      type: "tool_result",
      call_id: "reasoning-tool",
      output: "ok",
      is_error: false,
    }, 3, nid);
    acrossTool = applyAgentEvent(acrossTool, { type: "reasoning", text: "再核对", delta: true }, 4, nid);
    acrossTool = applyAgentEvent(acrossTool, { type: "reasoning", text: "边界", delta: true }, 4, nid);

    const child = buildLiveEntries([
      { id: "child-r1", kind: "reasoning", label: "思考过程", detail: "检查", at: 1 },
      { id: "child-r2", kind: "reasoning", label: "思考过程", detail: "边界", at: 2 },
    ], "running");
    return { history, live, acrossTool, child };
  });

  assert.deepEqual(
    contract.history.map((item) => [item.kind, item.label, item.detail, item.collapsible]),
    [["context", "思考过程", "先检查历史上下文", true]],
  );
  assert.deepEqual(
    contract.live.map((item) => [item.kind, item.label ?? null, item.detail ?? item.text, item.streaming ?? null]),
    [
      ["context", "思考过程", "先检查依赖关系", null],
      ["agent", null, "最终回答", true],
    ],
  );
  assert.deepEqual(
    contract.child.filter((entry) => entry.kind === "reasoning").map((entry) => entry.text),
    ["检查边界"],
  );
  assert.deepEqual(
    contract.acrossTool.filter((item) => item.kind === "context" && item.label === "思考过程")
      .map((item) => item.detail),
    ["先检查", "再核对边界"],
  );
  await page.close();
});

test("user send mode markers apply to the preceding user message, not the next one", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const contract = await page.evaluate(async () => {
    const { buildTimeline, latestRunRequestText } = await import("/src/components/room/model.ts");
    const userA = { kind: "message", id: "user-a", role: "user", text: "先立即处理" };
    const userB = { kind: "message", id: "user-b", role: "user", text: "再引导当前运行" };
    const modeSendNow = {
      kind: "system",
      id: "mode-send-now",
      role: null,
      text: "r_code_user_message_mode",
      output_json: JSON.stringify({ mode: "send_now" }),
    };
    const modeSteer = {
      kind: "system",
      id: "mode-steer",
      role: null,
      text: "r_code_user_message_mode",
      output_json: JSON.stringify({ mode: "steer" }),
    };
    const messages = [userA, modeSendNow, userB, modeSteer];
    const timeline = buildTimeline(messages, [], [], new Date().toISOString());
    return {
      sendModes: timeline.filter((item) => item.kind === "you").map((item) => item.sendMode),
      latestRequest: latestRunRequestText(messages),
    };
  });

  assert.deepEqual(contract.sendModes, ["send_now", "steer"]);
  assert.equal(contract.latestRequest, "先立即处理");
  await page.close();
});

test("first-round catalog anchor rows surface from journal events and live stream", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const contract = await page.evaluate(async () => {
    const { applyAgentEvent, buildTimeline } = await import("/src/components/room/model.ts");
    const anchorSystem = (id, payload) => ({
      kind: "system",
      id,
      role: null,
      text: "r_code_catalog_anchor",
      output_json: JSON.stringify(payload),
    });
    const history = buildTimeline(
      [
        { kind: "message", id: "u1", role: "user", text: "任务目标" },
        anchorSystem("anchor-1", { phase: "narrowed", catalog: "plan_native", tool_count: 5, full_tool_count: 18 }),
        { kind: "message", id: "a1", role: "assistant", text: "先看一下再动手" },
        anchorSystem("anchor-2", { phase: "promoted", catalog: "plan_native", tool_count: 8, full_tool_count: 18 }),
      ],
      [], [], new Date().toISOString(),
    );
    const live = applyAgentEvent([], {
      type: "catalog_anchor",
      phase: "narrowed",
      catalog: "plan_native",
      tool_count: 5,
      full_tool_count: 18,
    }, 1, () => "live-anchor");
    return {
      history: history.filter((item) => item.kind === "context").map((item) => [item.label, item.detail]),
      live: live.filter((item) => item.kind === "context").map((item) => [item.label, item.detail]),
    };
  });
  assert.deepEqual(contract.history, [
    ["Plan 原生目录已收敛", "Plan 原生目录 · 仅 5 / 18 个工具"],
    ["Plan 目录晋升 resident", "此后 8 / 18 个工具（只读，不恢复完整目录）"],
  ], "journal 回放路径必须渲染 plan_native 收敛与晋升两行");
  assert.deepEqual(contract.live, [
    ["Plan 原生目录已收敛", "Plan 原生目录 · 仅 5 / 18 个工具"],
  ], "实时事件路径必须立即出现收敛行");
  await page.close();
});

test("active run duration refreshes on the shared second tick and isolates renders", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.bringToFront();
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    await useTasksStore.getState().refreshDetail("mock-task-permission");
    useAppStore.getState().openRoom("mock-task-permission");
  });

  const duration = page.locator(".run-summary.active .run-duration").first();
  await duration.waitFor({ state: "visible" });
  const initial = await duration.textContent();
  // cadence：两次文本变化应接近 1s 的共享时钟 tick（窗口保持聚焦）。
  const firstChangeAt = await page.evaluate(
    (before) => new Promise((resolve) => {
      const target = document.querySelector(".run-summary.active .run-duration");
      const start = performance.now();
      const observer = new MutationObserver(() => {
        if (target?.textContent !== before) {
          observer.disconnect();
          resolve(Math.round(performance.now() - start));
        }
      });
      observer.observe(target, { childList: true, characterData: true, subtree: true });
      setTimeout(() => {
        observer.disconnect();
        resolve(-1);
      }, 3_000);
    }),
    initial,
  );
  assert.ok(firstChangeAt > 400, `expected a ~1s tick, got ${firstChangeAt}ms`);
  assert.ok(firstChangeAt <= 2_200, `expected a ~1s tick, got ${firstChangeAt}ms`);
  assert.notEqual(await duration.textContent(), initial);
  // render isolation：duration 变化期间，其他时间线内容不应被无关更新。
  const otherContent = await page.evaluate(() => {
    const name = document.querySelector(".run-summary.active .run-name")?.textContent;
    const rows = document.querySelectorAll(".run-summary").length;
    return { name, rows };
  });
  assert.deepEqual(otherContent, { name: "处理中", rows: 1 });
  assert.equal(
    await page.locator(".timeline-process-disclosure").count(),
    0,
    "an active run must keep its live activity trace visible instead of archiving it",
  );
  await page.locator(".timeline-turn-trace.has-activity").first().waitFor({ state: "visible" });
  await page.close();
});

test("a successful turn archives its process behind the duration while keeping the final summary visible", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const { browserMockDetails, browserMockSetMessages } = await import("/src/lib/mock-data.ts");
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    const taskId = "mock-task-complete";
    const detail = browserMockDetails[taskId];
    const startedAt = "2026-08-14T01:00:00.000Z";
    const endedAt = "2026-08-14T01:01:13.000Z";
    detail.runs[0].started_at = startedAt;
    detail.runs[0].ended_at = endedAt;
    detail.runs[0].review_state = "accepted";
    detail.runs[0].summary = "过程归档交互已完成";
    browserMockSetMessages(taskId, [
      {
        id: "complete-user",
        branch_id: "main",
        kind: "message",
        role: "user",
        text: "完成时间线过程归档",
        timestamp: startedAt,
      },
      {
        id: "complete-reasoning",
        branch_id: "main",
        kind: "system",
        text: "r_code_reasoning",
        output_json: JSON.stringify({ text: "先检查完成态与运行态边界" }),
      },
      {
        id: "complete-progress",
        branch_id: "main",
        kind: "message",
        role: "assistant",
        text: "我先核对相关实现，再执行定向修改。",
      },
      {
        id: "complete-tool-call",
        branch_id: "main",
        kind: "tool_call",
        tool_name: "read_file",
        call_id: "complete-read",
        input_json: JSON.stringify({ path: "src/main.rs" }),
      },
      {
        id: "complete-tool-result",
        branch_id: "main",
        kind: "tool_result",
        call_id: "complete-read",
        output_json: JSON.stringify({ content: "读取完成" }),
        is_error: false,
      },
      {
        id: "complete-final",
        branch_id: "main",
        kind: "message",
        role: "assistant",
        text: "已完成：过程归档交互已落地。",
        timestamp: endedAt,
      },
    ]);
    await useTasksStore.getState().refreshDetail(taskId);
    useAppStore.getState().openRoom(taskId);
  });

  const toggle = page.getByRole("button", { name: "已处理 1 步 · 耗时 1m 13s" });
  await toggle.waitFor({ state: "visible" });
  assert.equal(await toggle.getAttribute("aria-expanded"), "false");
  assert.equal(await page.locator(".timeline-process-body").count(), 0, "the completed process should not remain mounted by default");
  await page.getByText("已完成：过程归档交互已落地。", { exact: true }).waitFor({ state: "visible" });

  await toggle.focus();
  await page.keyboard.press("Enter");
  assert.equal(await toggle.getAttribute("aria-expanded"), "true");
  await page.locator(".timeline-process-body").waitFor({ state: "visible" });
  await page.getByText("我先核对相关实现，再执行定向修改。", { exact: true }).waitFor({ state: "visible" });
  await page.locator(".timeline-process-body .timeline-file-row").waitFor({ state: "visible" });

  await toggle.click();
  assert.equal(await toggle.getAttribute("aria-expanded"), "false");
  assert.equal(await page.locator(".timeline-process-body").count(), 0);
  await page.getByText("已完成：过程归档交互已落地。", { exact: true }).waitFor({ state: "visible" });
  await page.close();
});

test("appearance preferences stay flat and expose only theme and companion controls", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().setThemeMode("light");
    useAppStore.getState().setSettingsPane("preferences");
  });
  await page.waitForFunction(() => document.documentElement.dataset.theme === "studio-light");

  const preferences = page.locator(".settings-preferences");
  await preferences.waitFor({ state: "visible" });
  assert.equal(await preferences.locator(".settings-sheet").count(), 0, "preferences should not sit inside a generic card");
  assert.equal(await preferences.getByText("界面缩放", { exact: true }).count(), 0);
  assert.equal(await preferences.getByText("无障碍", { exact: true }).count(), 0);
  assert.equal(await preferences.getByText("文本差异视图", { exact: true }).count(), 0);

  const themeChoices = preferences.getByRole("radio").filter({ has: page.locator(".theme-option-copy") });
  assert.equal(await themeChoices.count(), 3);
  assert.equal(await themeChoices.evaluateAll((choices) =>
    choices.filter((choice) => choice.getAttribute("aria-checked") === "true").length
  ), 1, "exactly one theme should be selected");
  assert.equal(await preferences.locator(".preference-row").count(), 4, "companion options should use four aligned rows");
  await preferences.getByRole("switch", { name: "显示小助手", exact: true }).waitFor({ state: "visible" });

  const shapeTypography = await page.evaluate(() => {
    const title = document.querySelector("#set-companion-shape-label");
    const supporting = title?.nextElementSibling;
    const control = document.querySelector(".preference-segmented .chipbtn");
    if (!(title instanceof HTMLElement) || !(supporting instanceof HTMLElement) || !(control instanceof HTMLElement)) {
      throw new Error("companion shape typography is missing");
    }
    const titleStyle = getComputedStyle(title);
    const supportingStyle = getComputedStyle(supporting);
    const controlStyle = getComputedStyle(control);
    const colorProbe = document.createElement("span");
    colorProbe.style.color = "var(--fg-muted)";
    document.body.append(colorProbe);
    const mutedColor = getComputedStyle(colorProbe).color;
    colorProbe.remove();
    return {
      titleFontFamily: titleStyle.fontFamily,
      titleFontSize: titleStyle.fontSize,
      titleFontWeight: titleStyle.fontWeight,
      supportingColor: supportingStyle.color,
      mutedColor,
      supportingFontSize: supportingStyle.fontSize,
      controlFontFamily: controlStyle.fontFamily,
      controlFontSize: controlStyle.fontSize,
    };
  });
  assert.equal(shapeTypography.titleFontSize, "14px");
  assert.equal(shapeTypography.titleFontWeight, "500");
  assert.equal(shapeTypography.supportingColor, shapeTypography.mutedColor, "light supporting copy should use the readable muted tone");
  assert.equal(shapeTypography.supportingFontSize, "12px");
  assert.equal(shapeTypography.controlFontFamily, shapeTypography.titleFontFamily, "Chinese segmented labels should use the UI font rather than monospace fallback");
  assert.equal(shapeTypography.controlFontSize, "12px");

  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().setThemeMode("dark");
  });
  await page.waitForFunction(() => document.documentElement.dataset.theme === "obsidian");
  const darkTypography = await page.evaluate(() => {
    const title = document.querySelector("#set-companion-shape-label");
    const supporting = title?.nextElementSibling;
    const control = document.querySelector(".preference-segmented .chipbtn");
    if (!(title instanceof HTMLElement) || !(supporting instanceof HTMLElement) || !(control instanceof HTMLElement)) {
      throw new Error("dark companion shape typography is missing");
    }
    const colorProbe = document.createElement("span");
    colorProbe.style.color = "var(--fg-faint)";
    document.body.append(colorProbe);
    const faintColor = getComputedStyle(colorProbe).color;
    colorProbe.remove();
    return {
      titleFontSize: getComputedStyle(title).fontSize,
      supportingColor: getComputedStyle(supporting).color,
      faintColor,
      supportingFontSize: getComputedStyle(supporting).fontSize,
      controlFontFamily: getComputedStyle(control).fontFamily,
      titleFontFamily: getComputedStyle(title).fontFamily,
    };
  });
  assert.equal(darkTypography.titleFontSize, "14px");
  assert.equal(darkTypography.supportingColor, darkTypography.faintColor, "dark supporting copy should retain the existing faint tone");
  assert.equal(darkTypography.supportingFontSize, "12px");
  assert.equal(darkTypography.controlFontFamily, darkTypography.titleFontFamily);

  const removedState = await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    const state = useAppStore.getState();
    const app = document.querySelector("#app");
    if (!(app instanceof HTMLElement)) throw new Error("#app is missing");
    return {
      hasZoom: Object.prototype.hasOwnProperty.call(state, "zoomLevel"),
      hasAccessibleDiff: Object.prototype.hasOwnProperty.call(state, "accessibleDiffMode"),
      inlineZoom: app.style.zoom,
    };
  });
  assert.equal(removedState.hasZoom, false);
  assert.equal(removedState.hasAccessibleDiff, false);
  assert.equal(removedState.inlineZoom, "");

  await page.close();
});

test("knowledge and instructions lives in Settings and keeps project-scoped navigation", async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  assert.equal(await page.locator(".sidebar-nav-item").filter({ hasText: "项目文件" }).count(), 0);
  assert.equal(await page.locator(".sidebar-nav-item").filter({ hasText: "知识与指令" }).count(), 0, "knowledge must not remain a first-level rail destination");
  await page.getByRole("button", { name: "设置", exact: true }).click();
  await page.getByRole("button", { name: "知识与指令", exact: true }).click();
  const center = page.getByRole("region", { name: "知识与指令" });
  await center.waitFor({ state: "visible" });
  await center.getByRole("tab", { name: "记忆", exact: true }).waitFor({ state: "visible" });
  await center.getByRole("tab", { name: "协作 Prompt", exact: true }).waitFor({ state: "visible" });
  await center.getByRole("tab", { name: "Skills", exact: true }).waitFor({ state: "visible" });
  await center.getByRole("button", { name: "全局", exact: true }).waitFor({ state: "visible" });
  await center.getByRole("button", { name: "r-code", exact: true }).waitFor({ state: "visible" });
  await center.getByRole("tab", { name: "Skills", exact: true }).click();
  await center.getByRole("heading", { name: "全局 Skills", exact: true }).waitFor({ state: "visible" });
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    useTasksStore.getState().setCurrentProject("D:/project/rust/api-server");
    useAppStore.getState().openKnowledge("memory");
  });
  await page.waitForFunction(() => document.querySelector('[aria-label="api-server"]')?.getAttribute("aria-pressed") === "true");
  assert.equal(await center.getByRole("tab", { name: "记忆", exact: true }).getAttribute("aria-selected"), "true");
  assert.equal(await page.locator(".file-workspace").count(), 0);

  await page.close();
});

test("project navigation opens its dashboard and project files without another chooser", async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const project = page.locator(".sidebar-project").filter({ hasText: "r-code" });
  await project.locator(".sidebar-project-head").click();
  await page.locator("#main-content > .scene-dashboard").waitFor({ state: "visible" });
  await page.getByRole("heading", { name: "r-code", exact: true }).waitFor({ state: "visible" });
  await page.getByRole("button", { name: "项目文件", exact: true }).click();
  await page.locator(".file-workspace").waitFor({ state: "visible" });
  assert.equal(await page.getByRole("region", { name: "选择项目" }).count(), 0);

  await page.close();
});

test("project file preview highlights common syntax and both modes own their scroll", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 720 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const original = await page.evaluate(async () => {
    const { browserMockFiles } = await import("/src/lib/mock-data.ts");
    const previous = { ...browserMockFiles["src/main.rs"] };
    const body = Array.from(
      { length: 180 },
      (_, index) => `    let item_${index} = Result::<usize, String>::Ok(${index});`,
    );
    browserMockFiles["src/main.rs"] = {
      revision: "editor-scroll-regression",
      content: ["fn main() {", ...body, "}"].join("\n"),
    };
    return previous;
  });

  try {
    await openProjectFiles(page);
    await page.locator(".file-tree-row").filter({ hasText: "README.md" }).click();
    await page.locator(".file-code .tok-kw").filter({ hasText: "# R-Code" }).waitFor({ state: "visible" });
    await page.locator(".file-tree-row.folder").filter({ hasText: "src" }).click();
    await page.locator(".file-tree-row").filter({ hasText: "main.rs" }).click();

    const preview = page.locator(".file-code");
    await preview.waitFor({ state: "visible" });
    assert.ok(await preview.locator(".tok-kw").count(), "Rust keywords should be syntax highlighted");
    const previewScroll = await preview.evaluate((element) => {
      element.scrollTop = element.scrollHeight;
      const rect = element.getBoundingClientRect();
      const mainRect = document.querySelector("#main-content").getBoundingClientRect();
      return {
        clientHeight: element.clientHeight,
        scrollHeight: element.scrollHeight,
        scrollTop: element.scrollTop,
        overflowY: getComputedStyle(element).overflowY,
        bottom: rect.bottom,
        mainBottom: mainRect.bottom,
      };
    });
    assert.ok(previewScroll.scrollHeight > previewScroll.clientHeight, "long read-only previews must overflow locally");
    assert.ok(previewScroll.scrollTop > 0, "read-only previews must accept vertical scrolling");
    assert.match(previewScroll.overflowY, /auto|scroll/);
    assert.ok(previewScroll.bottom <= previewScroll.mainBottom + 1, "preview must stay inside the app viewport");

    await page.locator("#main-content").getByRole("button", { name: "编辑", exact: true }).click();
    const editor = page.locator(".file-code-editor");
    await editor.waitFor({ state: "visible" });
    const editorScroll = await editor.evaluate((element) => {
      element.scrollTop = element.scrollHeight;
      return {
        clientHeight: element.clientHeight,
        scrollHeight: element.scrollHeight,
        scrollTop: element.scrollTop,
        maxScrollTop: element.scrollHeight - element.clientHeight,
        overflowY: getComputedStyle(element).overflowY,
      };
    });
    assert.ok(editorScroll.scrollHeight > editorScroll.clientHeight, "long editable files must overflow locally");
    assert.ok(editorScroll.scrollTop > 0, "the editor must scroll to lower lines");
    assert.ok(Math.abs(editorScroll.maxScrollTop - editorScroll.scrollTop) <= 1, "the final line must be reachable");
    assert.match(editorScroll.overflowY, /auto|scroll/);
  } finally {
    await page.evaluate(async (previous) => {
      const { browserMockFiles } = await import("/src/lib/mock-data.ts");
      browserMockFiles["src/main.rs"] = previous;
    }, original).catch(() => {});
    await page.close();
  }
});

test("standalone Project Files refreshes the root and expanded folders in place", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 720 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  try {
    await openProjectFiles(page);
    const sourceFolder = page.locator(".file-tree-row.folder").filter({ hasText: "src" });
    await sourceFolder.click();
    const sourceFile = page.locator(".file-tree-row").filter({ hasText: "main.rs" });
    await sourceFile.click();
    await page.locator(".file-code-preview .tok-kw").filter({ hasText: "fn" }).waitFor({ state: "visible" });

    await page.evaluate(() => {
      const originalStringify = JSON.stringify;
      globalThis.__fileListCopies = [];
      globalThis.__restoreFileListStringify = () => { JSON.stringify = originalStringify; };
      JSON.stringify = function trackedStringify(value, ...rest) {
        if (Array.isArray(value) && value.length > 0 && value.every((entry) => (
          entry && typeof entry === "object" && "path" in entry && "is_directory" in entry
        ))) {
          globalThis.__fileListCopies.push(value.map((entry) => entry.path).join("|"));
        }
        return originalStringify.call(this, value, ...rest);
      };
    });

    const refresh = page.getByRole("button", { name: "刷新文件树" });
    await refresh.click();
    await page.waitForFunction(() => globalThis.__fileListCopies?.length >= 2);
    const listings = await page.evaluate(() => [...globalThis.__fileListCopies]);
    assert.ok(listings.includes("src|assets|Cargo.toml|README.md"), "refresh must re-list the project root");
    assert.ok(listings.includes("src/main.rs|src/error.rs|src/api.rs"), "refresh must re-list an expanded directory");
    assert.equal(await sourceFile.isVisible(), true, "the expanded directory must stay open after refresh");
    assert.match(await sourceFile.getAttribute("class"), /selected/, "refresh must preserve the selected file");
    assert.ok(await page.locator(".file-code-preview .tok-kw").count(), "the shared preview must retain syntax token classes");
  } finally {
    await page.evaluate(() => globalThis.__restoreFileListStringify?.()).catch(() => {});
    await page.close();
  }
});

test("standalone Project Files exposes file-only actions and consumes task references once", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 720 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  await openProjectFiles(page);
  const sourceFolder = page.locator(".file-tree-row.folder").filter({ hasText: "src" });
  await sourceFolder.click();
  const sourceFile = page.locator(".file-tree-row").filter({ hasText: "main.rs" });

  await sourceFile.click({ button: "right" });
  let menu = page.getByRole("menu", { name: "文件操作" });
  await menu.waitFor({ state: "visible" });
  await menu.getByRole("menuitem", { name: /添加到任务/ }).waitFor({ state: "visible" });
  await menu.getByRole("menuitem", { name: "复制路径", exact: true }).waitFor({ state: "visible" });
  await menu.getByRole("menuitem", { name: /打开方式/ }).click();
  await menu.getByRole("menuitem", { name: "在文件管理器中显示", exact: true }).click();
  await page.waitForFunction(() => document.documentElement.dataset.demoRevealedPath === "D:/project/rust/r-code/src/main.rs");

  await sourceFolder.click({ button: "right" });
  assert.equal(await page.getByRole("menu", { name: "文件操作" }).count(), 0, "folder right-click must not open a custom menu");

  await sourceFile.click({ button: "right" });
  menu = page.getByRole("menu", { name: "文件操作" });
  await menu.getByRole("menuitem", { name: /添加到任务/ }).click();
  await menu.getByRole("menuitem", { name: "修复任务队列并发问题", exact: true }).click();

  await page.locator("#main-content > .scene-room").waitFor({ state: "visible" });
  const composer = page.getByRole("textbox", { name: "给 Agent 的消息" });
  await composer.waitFor({ state: "visible" });
  await page.waitForTimeout(100);
  const referenceState = await page.evaluate(async () => {
    const { browserMockMessages } = await import("/src/lib/mock-data.ts");
    const { useAppStore } = await import("/src/store/app.ts");
    return {
      draft: document.querySelector('[aria-label="给 Agent 的消息"]')?.value ?? "",
      pending: useAppStore.getState().taskFileReferences["mock-task-queue"] ?? null,
      sent: browserMockMessages("mock-task-queue").filter((message) => message.text?.includes("@src/main.rs")).length,
    };
  });
  assert.equal(referenceState.draft, "@src/main.rs", "the draft must receive exactly one reference");
  assert.equal(referenceState.pending, null, "the matching request must be acknowledged");
  assert.equal(referenceState.sent, 0, "adding a reference must not send a message");

  await openKnowledgeSettings(page);
  await page.locator(".sidebar-task-row").filter({ hasText: "修复任务队列并发问题" }).locator(".sidebar-task").click();
  await page.locator("#main-content > .scene-room").waitFor({ state: "visible" });
  assert.equal(
    await composer.inputValue(),
    "@src/main.rs",
    "the cached draft must return once without replaying the acknowledged reference",
  );

  await page.evaluate(async () => {
    const { clearComposerDraft, flushComposerDrafts } = await import("/src/lib/composer-drafts.ts");
    clearComposerDraft("mock-task-queue");
    flushComposerDrafts();
  });

  await page.close();
});

test("task Files keeps highlighted deep links, dirty drafts, and existing file workflows", async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const targetLine = 90;
  const targetColumn = 9;
  const fixture = await page.evaluate(({ line, column }) => {
    return import("/src/lib/mock-data.ts").then(({ browserMockFiles }) => {
      const previous = { ...browserMockFiles["src/main.rs"] };
      const body = Array.from(
        { length: 120 },
        (_, index) => `    let item_${index + 1}: usize = ${index + 1};`,
      );
      const content = ["fn main() {", ...body, "}"].join("\n");
      browserMockFiles["src/main.rs"] = {
        revision: "workbench-file-parity",
        content,
      };
      const lines = content.split("\n");
      const expectedOffset = lines
        .slice(0, line - 1)
        .reduce((total, value) => total + value.length + 1, 0)
        + Math.min(column - 1, lines[line - 1].length);
      return { previous, content, expectedOffset };
    });
  }, { line: targetLine, column: targetColumn });

  try {
    await page.evaluate(async ({ line, column }) => {
      const { useAppStore } = await import("/src/store/app.ts");
      const { useTasksStore } = await import("/src/store/tasks.ts");
      useTasksStore.getState().setCurrentProject("D:/project/rust/r-code");
      useAppStore.getState().openWorkbenchFile("mock-task-queue", "src/main.rs", line, column);
    }, { line: targetLine, column: targetColumn });

    const workbench = page.getByTestId("workbench-panel");
    await workbench.waitFor({ state: "visible" });
    await page.waitForFunction(() => document.querySelector('[data-testid="workbench-panel"]')?.getAttribute("data-workbench-kind") === "files");
    const preview = workbench.locator(".files-code-preview");
    await preview.waitFor({ state: "visible" });
    const activeLine = preview.locator(`.file-code-line[data-line="${targetLine}"]`);
    await activeLine.waitFor({ state: "visible" });
    assert.match(await activeLine.getAttribute("class"), /is-active/, "the deep-linked line must be highlighted");
    assert.equal(await activeLine.getAttribute("aria-current"), "location");
    assert.equal(await preview.locator('.file-code-line[data-line="1"] > i').innerText(), "1", "preview must expose line numbers");
    assert.ok(await preview.locator(".tok-kw").count(), "task preview must use the shared syntax token classes");
    const rootSourceFolder = workbench.locator(".files-tree-row").filter({ hasText: "src" });
    const rootAssetsFolder = workbench.locator(".files-tree-row").filter({ hasText: "assets" });
    const treeArrowGeometry = await Promise.all([rootSourceFolder, rootAssetsFolder].map((row) => (
      row.evaluate((element) => {
        const arrow = element.querySelector(".files-tree-arrow");
        const svg = arrow?.querySelector("svg");
        const rowRect = element.getBoundingClientRect();
        const arrowRect = arrow?.getBoundingClientRect();
        const svgRect = svg?.getBoundingClientRect();
        return {
          expanded: element.getAttribute("aria-expanded"),
          arrowLeft: arrowRect?.left,
          arrowWidth: arrowRect?.width,
          arrowCenterOffset: arrowRect ? (arrowRect.top + arrowRect.height / 2) - (rowRect.top + rowRect.height / 2) : null,
          svgWidth: svgRect?.width,
          svgHeight: svgRect?.height,
          path: svg?.querySelector("path")?.getAttribute("d"),
        };
      })
    )));
    assert.deepEqual(treeArrowGeometry.map((item) => item.expanded), ["true", "false"]);
    assert.equal(treeArrowGeometry[0].path, treeArrowGeometry[1].path, "open and closed folders must share one centered glyph");
    assert.equal(treeArrowGeometry[0].arrowLeft, treeArrowGeometry[1].arrowLeft, "same-depth folder arrows must align horizontally");
    assert.deepEqual(
      treeArrowGeometry.map(({ arrowWidth, svgWidth, svgHeight }) => ({ arrowWidth, svgWidth, svgHeight })),
      [{ arrowWidth: 12, svgWidth: 11, svgHeight: 11 }, { arrowWidth: 12, svgWidth: 11, svgHeight: 11 }],
      "open and closed folder glyphs must keep identical geometry",
    );
    assert.ok(
      treeArrowGeometry.every(({ arrowCenterOffset }) => Math.abs(arrowCenterOffset ?? Number.POSITIVE_INFINITY) <= 0.5),
      "folder arrows must stay centered on their rows",
    );
    await page.waitForFunction(() => (document.querySelector(".files-code-preview")?.scrollTop ?? 0) > 0);
    assert.equal(await workbench.locator(".files-textarea").count(), 0, "text files must start in preview mode");

    await workbench.getByRole("button", { name: "编辑", exact: true }).click();
    const editor = workbench.locator(".files-textarea");
    await editor.waitFor({ state: "visible" });
    const caret = await editor.evaluate((element) => ({
      start: element.selectionStart,
      end: element.selectionEnd,
      scrollTop: element.scrollTop,
    }));
    assert.deepEqual(caret.start, fixture.expectedOffset, "edit mode must preserve the deep-link column");
    assert.equal(caret.end, fixture.expectedOffset);
    assert.ok(caret.scrollTop > 0, "edit mode must scroll the deep-linked line into view");

    const dirtyDraft = `${fixture.content}\n// unsaved refresh sentinel`;
    await editor.fill(dirtyDraft);
    assert.equal(await workbench.getByRole("button", { name: "保存", exact: true }).isEnabled(), true);
    await page.evaluate(() => {
      const originalStringify = JSON.stringify;
      globalThis.__workbenchFileListCopies = [];
      globalThis.__restoreWorkbenchFileListStringify = () => { JSON.stringify = originalStringify; };
      JSON.stringify = function trackedStringify(value, ...rest) {
        if (Array.isArray(value) && value.length > 0 && value.every((entry) => (
          entry && typeof entry === "object" && "path" in entry && "is_directory" in entry
        ))) {
          globalThis.__workbenchFileListCopies.push(value.map((entry) => entry.path).join("|"));
        }
        return originalStringify.call(this, value, ...rest);
      };
    });

    await workbench.getByRole("button", { name: "刷新文件树" }).click();
    await page.waitForFunction(() => globalThis.__workbenchFileListCopies?.length >= 2);
    const listings = await page.evaluate(() => [...globalThis.__workbenchFileListCopies]);
    assert.ok(listings.includes("src|assets|Cargo.toml|README.md"), "refresh must re-list the task workspace root");
    assert.ok(listings.includes("src/main.rs|src/error.rs|src/api.rs"), "refresh must re-list expanded task folders");
    const sourceFolder = workbench.locator(".files-tree-row").filter({ hasText: "src" });
    const sourceFile = workbench.locator(".files-tree-row").filter({ hasText: "main.rs" });
    assert.equal(await sourceFolder.getAttribute("aria-expanded"), "true", "refresh must preserve expansion");
    assert.match(await sourceFile.getAttribute("class"), /selected/, "refresh must preserve selection");
    assert.equal(await editor.inputValue(), dirtyDraft, "refresh must not discard an unsaved draft");

    await editor.press("Control+s");
    await page.waitForFunction(async (content) => {
      const { browserMockFiles } = await import("/src/lib/mock-data.ts");
      return browserMockFiles["src/main.rs"].content === content;
    }, dirtyDraft);
    assert.equal(await workbench.getByRole("button", { name: "保存", exact: true }).isDisabled(), true, "Ctrl+S must retain the existing save flow");

    const discardedDraft = `${dirtyDraft}\n// discard me`;
    await editor.fill(discardedDraft);
    await workbench.getByRole("button", { name: "重新加载", exact: true }).click();
    await workbench.getByRole("button", { name: "确认放弃修改?", exact: true }).click();
    await preview.waitFor({ state: "visible" });
    assert.doesNotMatch(await preview.innerText(), /discard me/, "confirmed reload must discard only the later draft");
    assert.match(await preview.innerText(), /unsaved refresh sentinel/, "confirmed reload must read the last saved content");

    const assetsFolder = workbench.locator(".files-tree-row").filter({ hasText: "assets" });
    await assetsFolder.click();
    await workbench.locator(".files-tree-row").filter({ hasText: "demo-sky.png" }).click();
    await workbench.getByRole("button", { name: "预览图片：demo-sky.png" }).waitFor({ state: "visible" });
  } finally {
    await page.evaluate(async (previous) => {
      globalThis.__restoreWorkbenchFileListStringify?.();
      const { browserMockFiles } = await import("/src/lib/mock-data.ts");
      browserMockFiles["src/main.rs"] = previous;
    }, fixture.previous).catch(() => {});
    await page.close();
  }
});

test("assistant file links open the right-side Files workbench at the referenced line", async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    useTasksStore.getState().setCurrentProject("D:/project/rust/r-code");
    await useTasksStore.getState().refreshDetail("mock-task-complete");
    useAppStore.getState().openRoom("mock-task-complete");
  });

  const fileLink = page.getByRole("button", { name: "打开实现文件", exact: true });
  await fileLink.waitFor({ state: "visible" });
  await fileLink.click();

  const workbench = page.getByTestId("workbench-panel");
  await workbench.waitFor({ state: "visible" });
  await page.waitForFunction(() => (
    document.querySelector('[data-testid="workbench-panel"]')?.getAttribute("data-workbench-kind") === "files"
  ));
  const activeLine = workbench.locator('.files-code-preview .file-code-line[data-line="2"]');
  await activeLine.waitFor({ state: "visible" });
  assert.match(await activeLine.getAttribute("class"), /is-active/);
  assert.equal(await activeLine.getAttribute("aria-current"), "location");

  await page.close();
});

test("task Files exposes file-only actions and inserts one current-task reference", async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    useTasksStore.getState().setCurrentProject("D:/project/rust/r-code");
    useAppStore.setState({ taskFileReferences: {} });
    useAppStore.getState().openRoom("mock-task-queue", "files");
  });

  const workbench = page.getByTestId("workbench-panel");
  const sourceFolder = workbench.locator(".files-tree-row").filter({ hasText: "src" });
  const sourceFile = workbench.locator(".files-tree-row").filter({ hasText: "main.rs" });
  await sourceFolder.waitFor({ state: "visible" });
  await sourceFolder.click();
  await sourceFile.waitFor({ state: "visible" });
  await sourceFile.click();
  await workbench.locator(".files-code-preview").waitFor({ state: "visible" });
  await sourceFile.click({ button: "right" });
  let menu = page.getByRole("menu", { name: "文件操作" });
  await menu.waitFor({ state: "visible" });
  await menu.getByRole("menuitem", { name: /添加到任务/ }).waitFor({ state: "visible" });
  await menu.getByRole("menuitem", { name: "复制路径", exact: true }).waitFor({ state: "visible" });
  await menu.getByRole("menuitem", { name: /打开方式/ }).click();
  await menu.getByRole("menuitem", { name: "在文件管理器中显示", exact: true }).click();
  await page.waitForFunction(() => document.documentElement.dataset.demoRevealedPath === "D:/project/rust/r-code/src/main.rs");

  await sourceFolder.click({ button: "right" });
  assert.equal(await page.getByRole("menu", { name: "文件操作" }).count(), 0, "task folders must not open a custom context menu");

  await sourceFile.click({ button: "right" });
  menu = page.getByRole("menu", { name: "文件操作" });
  await menu.getByRole("menuitem", { name: /添加到任务/ }).click();
  const composer = page.getByRole("textbox", { name: "给 Agent 的消息" });
  await page.waitForFunction(() => document.querySelector('[aria-label="给 Agent 的消息"]')?.value === "@src/main.rs");
  await page.waitForTimeout(100);

  const referenceState = await page.evaluate(async () => {
    const { browserMockMessages } = await import("/src/lib/mock-data.ts");
    const { useAppStore } = await import("/src/store/app.ts");
    const draft = document.querySelector('[aria-label="给 Agent 的消息"]')?.value ?? "";
    return {
      draft,
      referenceCount: draft.split("@src/main.rs").length - 1,
      pending: useAppStore.getState().taskFileReferences["mock-task-queue"] ?? null,
      sent: browserMockMessages("mock-task-queue").filter((message) => message.text?.includes("@src/main.rs")).length,
      currentTaskId: useAppStore.getState().currentTaskId,
    };
  });
  assert.equal(await composer.inputValue(), "@src/main.rs");
  assert.equal(referenceState.referenceCount, 1, "one menu action must append one reference");
  assert.equal(referenceState.pending, null, "the current Composer must acknowledge the exact request");
  assert.equal(referenceState.sent, 0, "adding a workbench file must not send a message");
  assert.equal(referenceState.currentTaskId, "mock-task-queue", "the direct task action must stay in the current room");

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
      const mainStyle = getComputedStyle(main);
      const borderLeft = Number.parseFloat(mainStyle.borderLeftWidth) || 0;
      const borderRight = Number.parseFloat(mainStyle.borderRightWidth) || 0;
      const borderTop = Number.parseFloat(mainStyle.borderTopWidth) || 0;
      const borderBottom = Number.parseFloat(mainStyle.borderBottomWidth) || 0;
      return {
        mainContentRect: [
          mainRect.x + borderLeft,
          mainRect.y + borderTop,
          mainRect.width - borderLeft - borderRight,
          mainRect.height - borderTop - borderBottom,
        ],
        roomRect: [roomRect.x, roomRect.y, roomRect.width, roomRect.height],
        timeline: [timeline.clientHeight, timeline.scrollHeight, timeline.scrollTop],
        page: [document.documentElement.scrollWidth, document.documentElement.scrollHeight, innerWidth, innerHeight],
      };

      function assertElement(value, label) {
        if (!(value instanceof HTMLElement)) throw new Error(`${label} missing`);
      }
    });

    assert.deepEqual(layout.roomRect, layout.mainContentRect, "room must occupy the complete main content box");
    assert.ok(layout.timeline[1] > layout.timeline[0], "long conversations must overflow the timeline");
    assert.ok(layout.timeline[2] > 0, "the timeline must accept vertical scrolling");
    assert.ok(layout.page[0] <= layout.page[2] + 1, "the app must not create page-level horizontal scrolling");
    assert.ok(layout.page[1] <= layout.page[3] + 1, "the app must not create page-level vertical scrolling");
    assert.deepEqual(runtimeErrors, []);
    await page.close();
  });
}

test("new task composer remains fully reachable in a compact desktop window", async () => {
  const page = await browser.newPage({ viewport: { width: 800, height: 600 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const send = page.getByRole("button", { name: "发送", exact: true });
  const sendBox = await send.boundingBox();
  assert.ok(sendBox && sendBox.y >= 0 && sendBox.y + sendBox.height <= 600, "the primary action must not fall below the viewport");
  assert.equal(await page.evaluate(() => document.documentElement.scrollHeight), 600, "the app shell must not hide the composer behind page overflow");
  await page.close();
});

test("project conversations can be renamed from their shared actions menu", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const oldTitle = "更新依赖并修复告警";
  const newTitle = "依赖升级复盘";
  const taskRow = page.locator(".sidebar-task-row").filter({ hasText: oldTitle });
  await taskRow.hover();
  await taskRow.locator(".task-actions-trigger").click();

  const menu = page.locator('.task-actions-popover[role="menu"]');
  await menu.waitFor({ state: "visible" });
  await menu.getByRole("menuitem", { name: "重命名对话…", exact: true }).click();

  const dialog = page.getByRole("dialog", { name: "重命名对话" });
  await dialog.waitFor({ state: "visible" });
  const input = dialog.getByRole("textbox", { name: "会话名称" });
  assert.equal(await input.inputValue(), oldTitle);
  await input.fill(newTitle);
  await input.press("Enter");

  await page.getByText("对话已重命名", { exact: true }).waitFor({ state: "visible" });
  await page.locator(".sidebar-task-row").filter({ hasText: newTitle }).waitFor({ state: "visible" });
  assert.equal(await page.locator(".sidebar-task-row").filter({ hasText: oldTitle }).count(), 0);
  assert.equal(await dialog.count(), 0);

  const renamedRow = page.locator(".sidebar-task-row").filter({ hasText: newTitle });
  await renamedRow.hover();
  await renamedRow.locator(".task-actions-trigger").click();
  await page.getByRole("menu", { name: `管理对话：${newTitle}` }).waitFor({ state: "visible" });
  await page.keyboard.press("Escape");
  await page.close();
});

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
  await page.getByText("此对话已归档，只能查看历史。可在项目概览中还原，或通过右上角对话选项永久删除。").waitFor({ state: "visible" });
  assert.equal(await page.locator(".composer").count(), 0);
  await page.close();
});

test("project dashboard restores or permanently deletes archived conversations without activity noise", async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  await page.evaluate(async () => {
    const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
    await browserMockInvoke("cmd_task_archive", { taskId: "mock-task-complete" });
  });
  await page.locator(".sidebar-project-head").filter({ hasText: "r-code" }).click();

  const archived = page.locator(".dashboard-archived-row").filter({ hasText: "更新依赖并修复告警" });
  await archived.waitFor({ state: "visible" });
  assert.equal(
    await page.locator(".project-activity-item").filter({ hasText: "更新依赖并修复告警" }).count(),
    0,
    "archived conversation events must not remain in project activity",
  );
  const activityLabels = await page.locator(".project-activity-item small").allTextContents();
  assert.ok(activityLabels.length <= 5, "project activity should stay intentionally short");
  assert.equal(new Set(activityLabels.map((label) => label.split(" · ")[0])).size, activityLabels.length, "each conversation should contribute only its latest key event");

  await archived.getByRole("button", { name: "还原", exact: true }).click();
  await page.getByText("对话已还原", { exact: true }).waitFor({ state: "visible" });
  await page.locator(".dashboard-task-row").filter({ hasText: "更新依赖并修复告警" }).waitFor({ state: "visible" });
  assert.equal(await archived.count(), 0);

  await page.evaluate(async () => {
    const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    await browserMockInvoke("cmd_task_archive", { taskId: "mock-task-complete" });
    const state = useTasksStore.getState();
    await Promise.all([
      state.refreshTasks(),
      state.refreshDashboard("D:/project/rust/r-code"),
      state.refreshProjectActivity("D:/project/rust/r-code"),
    ]);
  });
  await archived.waitFor({ state: "visible" });
  await archived.getByRole("button", { name: "永久删除 更新依赖并修复告警" }).click();
  const dialog = page.getByRole("alertdialog", { name: "永久删除这段对话？" });
  await dialog.waitFor({ state: "visible" });
  await dialog.getByRole("button", { name: "永久删除", exact: true }).click();
  await page.getByText("对话已永久删除", { exact: true }).waitFor({ state: "visible" });
  assert.equal(await archived.count(), 0);
  await page.close();
});

test("desktop back and forward restore the actual visited page and project", async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const back = page.getByRole("button", { name: "后退" });
  const forward = page.getByRole("button", { name: "前进" });
  const heading = page.locator("#main-content .dashboard-header h1");

  assert.equal(await back.isDisabled(), true);
  await page.locator(".sidebar-project-head").filter({ hasText: "r-code" }).click();
  await heading.filter({ hasText: "r-code" }).waitFor({ state: "visible" });
  await page.locator(".sidebar-project-head").filter({ hasText: "api-server" }).click();
  await heading.filter({ hasText: "api-server" }).waitFor({ state: "visible" });

  await back.click();
  await heading.filter({ hasText: "r-code" }).waitFor({ state: "visible" });
  await back.click();
  await page.locator("#main-content > .scene-home").waitFor({ state: "visible" });
  await forward.click();
  await heading.filter({ hasText: "r-code" }).waitFor({ state: "visible" });
  await forward.click();
  await heading.filter({ hasText: "api-server" }).waitFor({ state: "visible" });
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

    const center = await openKnowledgeSettings(page);
    await center.getByRole("tab", { name: "记忆", exact: true }).click();

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
      await center.getByRole("button", { name: displayName, exact: true }).click();

      const notice = page.locator(".knowledge-memory-safety .legacy-memory-status");
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
          identity: workspace && { id: workspace.id, canonical_path: workspace.canonical_path },
        };
      }, scenario.path);
      assert.deepEqual(navigation.identity, contract.initialIdentity[scenario.path]);
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

    const memorySection = page.locator(".knowledge-memory-panel");
    assert.ok(await memorySection.locator("textarea").count() >= 1, "the live AppData memory ledger remains available beside the read-only legacy notice");
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

  const project = page.locator(".sidebar-project").filter({ hasText: "api-server" });
  const trigger = project.getByRole("button", { name: "api-server 项目操作" });
  await trigger.click();
  const menu = page.getByRole("menu", { name: "api-server 项目操作" });
  await menu.waitFor({ state: "visible" });
  await menu.getByRole("menuitem", { name: "项目概览", exact: true }).waitFor({ state: "visible" });
  assert.equal(await menu.getByRole("menuitem", { name: "打开项目", exact: true }).count(), 0);
  const bounds = await menu.boundingBox();
  assert.ok(bounds && bounds.x >= 0 && bounds.y >= 0);
  assert.ok(bounds.x + bounds.width <= 1200 && bounds.y + bounds.height <= 800, "project menu must stay inside the viewport");
  const remove = menu.getByRole("menuitem", { name: "从 R-Code 移除…", exact: true });
  await page.waitForFunction(
    () => {
      const item = document.querySelector('[role="menuitem"].project-remove-menu-item');
      return item != null && !item.hasAttribute("disabled");
    },
  );
  await remove.click();

  const dialog = page.getByRole("alertdialog", { name: "从 R-Code 中清除这个项目？" });
  await dialog.waitFor({ state: "visible" });
  const copy = await dialog.innerText();
  assert.match(copy, /真实文件夹及其中的文件不会被删除、移动或修改/);
  assert.match(copy, /1 段对话以及关联的运行与审计数据/);
  await dialog.getByRole("button", { name: "清除项目", exact: true }).click();

  await page.getByText("项目已从 R-Code 清除", { exact: true }).waitFor({ state: "visible" });
  assert.equal(await page.locator(".sidebar-project").filter({ hasText: "api-server" }).count(), 0);

  await page.locator(".sidebar-nav-item").filter({ hasText: "对话" }).click();
  assert.equal(await page.locator(".conversation-row").filter({ hasText: "添加请求限流中间件" }).count(), 0);
  await page.close();
});

test("unsent Composer drafts survive scene and task switches without leaking between conversations", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const storageKey = "r-code.composer.drafts.v1";
  const firstTaskId = "mock-task-queue";
  const secondTaskId = "mock-task-complete";

  try {
    await page.evaluate(async ({ storageKey, firstTaskId, secondTaskId }) => {
      window.localStorage.removeItem(storageKey);
      const { useAppStore } = await import("/src/store/app.ts");
      const { useTasksStore } = await import("/src/store/tasks.ts");
      await Promise.all([
        useTasksStore.getState().refreshDetail(firstTaskId),
        useTasksStore.getState().refreshDetail(secondTaskId),
      ]);
      useAppStore.getState().openRoom(firstTaskId);
    }, { storageKey, firstTaskId, secondTaskId });

    let composer = page.getByRole("textbox", { name: "给 Agent 的消息" });
    await composer.waitFor({ state: "visible" });
    await composer.fill("第一段未发送草稿：切到设置后仍应保留");

    await page.evaluate(async () => {
      const { useAppStore } = await import("/src/store/app.ts");
      useAppStore.getState().setScene("settings");
    });
    await page.locator("#main-content .settings-layout").waitFor({ state: "visible" });
    await page.evaluate(async (taskId) => {
      const { useAppStore } = await import("/src/store/app.ts");
      useAppStore.getState().openRoom(taskId);
    }, firstTaskId);
    composer = page.getByRole("textbox", { name: "给 Agent 的消息" });
    await composer.waitFor({ state: "visible" });
    assert.equal(await composer.inputValue(), "第一段未发送草稿：切到设置后仍应保留");

    await page.evaluate(async (taskId) => {
      const { useAppStore } = await import("/src/store/app.ts");
      useAppStore.getState().openRoom(taskId);
    }, secondTaskId);
    await page.waitForFunction((taskId) => import("/src/store/app.ts").then(
      ({ useAppStore }) => useAppStore.getState().currentTaskId === taskId,
    ), secondTaskId);
    composer = page.getByRole("textbox", { name: "给 Agent 的消息" });
    assert.equal(await composer.inputValue(), "", "a draft from another conversation must never leak here");
    await composer.fill("第二段草稿：只属于另一个对话");

    await page.evaluate(async (taskId) => {
      const { useAppStore } = await import("/src/store/app.ts");
      useAppStore.getState().openRoom(taskId);
    }, firstTaskId);
    await page.waitForFunction((taskId) => import("/src/store/app.ts").then(
      ({ useAppStore }) => useAppStore.getState().currentTaskId === taskId,
    ), firstTaskId);
    composer = page.getByRole("textbox", { name: "给 Agent 的消息" });
    assert.equal(await composer.inputValue(), "第一段未发送草稿：切到设置后仍应保留");

    await page.evaluate(async (taskId) => {
      const { useAppStore } = await import("/src/store/app.ts");
      useAppStore.getState().openRoom(taskId);
    }, secondTaskId);
    await page.waitForFunction((taskId) => import("/src/store/app.ts").then(
      ({ useAppStore }) => useAppStore.getState().currentTaskId === taskId,
    ), secondTaskId);
    composer = page.getByRole("textbox", { name: "给 Agent 的消息" });
    assert.equal(await composer.inputValue(), "第二段草稿：只属于另一个对话");

    await page.evaluate(async () => {
      const { flushComposerDrafts } = await import("/src/lib/composer-drafts.ts");
      flushComposerDrafts();
    });
    await page.reload({ waitUntil: "networkidle" });
    await page.evaluate(async (taskId) => {
      const { useAppStore } = await import("/src/store/app.ts");
      const { useTasksStore } = await import("/src/store/tasks.ts");
      await useTasksStore.getState().refreshDetail(taskId);
      useAppStore.getState().openRoom(taskId);
    }, firstTaskId);
    composer = page.getByRole("textbox", { name: "给 Agent 的消息" });
    await composer.waitFor({ state: "visible" });
    assert.equal(
      await composer.inputValue(),
      "第一段未发送草稿：切到设置后仍应保留",
      "the local-only cache should also survive a WebView reload",
    );
  } finally {
    await page.evaluate(async ({ storageKey, firstTaskId, secondTaskId }) => {
      const { clearComposerDraft, flushComposerDrafts } = await import("/src/lib/composer-drafts.ts");
      clearComposerDraft(firstTaskId);
      clearComposerDraft(secondTaskId);
      flushComposerDrafts();
      window.localStorage.removeItem(storageKey);
    }, { storageKey, firstTaskId, secondTaskId }).catch(() => {});
    await page.close();
  }
});

test("Enter uses the selected run send mode and clears the accepted draft before IPC completes", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const taskId = "mock-task-queue";
  const baseline = await page.evaluate(async (id) => {
    const {
      browserMockDetails,
      browserMockMessages,
      browserMockTasks,
    } = await import("/src/lib/mock-data.ts");
    return {
      task: structuredClone(browserMockTasks.find((item) => item.id === id)),
      detail: structuredClone(browserMockDetails[id]),
      messages: structuredClone(browserMockMessages(id)),
    };
  }, taskId);

  try {
    const taskRow = page.locator(".sidebar-task-row").filter({ hasText: "修复任务队列并发问题" });
    await taskRow.locator(".sidebar-task").click();
    await page.locator("#main-content > .scene-room").waitFor({ state: "visible" });

    await page.evaluate(async () => {
      const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
      globalThis.__rCodeSendModes = [];
      globalThis.__rCodeReleaseSend = null;
      const firstSend = new Promise((resolve) => {
        globalThis.__rCodeReleaseSend = resolve;
      });
      globalThis.__TAURI_INTERNALS__ = {
        invoke: async (command, args = {}) => {
          if (command === "cmd_agent_send") {
            globalThis.__rCodeSendModes.push(args.mode);
            if (globalThis.__rCodeFailNextSend) {
              globalThis.__rCodeFailNextSend = false;
              throw new Error("mock send rejection");
            }
            if (globalThis.__rCodeSendModes.length === 1) await firstSend;
          }
          return browserMockInvoke(command, args);
        },
      };
    });

    const composer = page.getByRole("textbox", { name: "给 Agent 的消息" });
    await composer.fill("作为下一轮发送");
    await composer.press("Enter");
    assert.equal(await composer.inputValue(), "", "accepted drafts must clear before a slow IPC resolves");
    await page.waitForFunction(() => globalThis.__rCodeSendModes?.length === 1);
    const pendingSend = page.getByRole("button", { name: "正在发送消息", exact: true });
    await pendingSend.waitFor({ state: "visible" });
    assert.equal(await pendingSend.getAttribute("aria-busy"), "true");
    assert.equal(await pendingSend.locator(".send-loading-spinner").isVisible(), true);
    assert.equal(await pendingSend.isDisabled(), true);
    assert.deepEqual(await page.evaluate(() => globalThis.__rCodeSendModes), ["queue"]);
    await page.evaluate(() => globalThis.__rCodeReleaseSend?.());
    await page.waitForFunction(() => !document.querySelector(".run-send-mode-trigger")?.hasAttribute("disabled"));
    assert.equal(
      await page.evaluate(async (id) => {
        const { readComposerDraft } = await import("/src/lib/composer-drafts.ts");
        return readComposerDraft(id);
      }, taskId),
      "",
      "a successfully accepted message must not return after navigation",
    );

    const snapshot = await page.evaluate(async (id) => {
      const { browserMockDetails } = await import("/src/lib/mock-data.ts");
      const detail = browserMockDetails[id];
      return {
        queue: structuredClone(detail.queued_messages),
        activeMainRun: detail.runs.find((run) => run.agent_kind === "main" && run.ended_at == null),
      };
    }, taskId);
    assert.equal(snapshot.queue.length, baseline.detail.queued_messages.length + 1);
    assert.equal(snapshot.queue.at(-1)?.message, "作为下一轮发送");
    assert.ok(snapshot.activeMainRun, "plain Enter must not replace or finish the active run");

    const controls = page.locator('[aria-label="运行中消息操作"]');
    assert.match(await controls.locator(".run-send-primary").innerText(), /排队\s*Enter/);

    const modeTrigger = controls.getByRole("button", { name: /选择发送方式/ });
    await modeTrigger.click();
    const modeMenu = page.getByRole("menu", { name: "选择发送方式" });
    await modeMenu.waitFor({ state: "visible" });
    assert.equal(await modeMenu.getByRole("menuitemradio").count(), 3);
    assert.equal(await modeMenu.getByText(/委派给 Codex/).count(), 0);
    await modeMenu.getByRole("menuitemradio", { name: /引导当前运行/ }).click();
    assert.match(await controls.locator(".run-send-primary").innerText(), /引导\s*Enter/);

    await modeTrigger.click();
    await modeMenu.getByRole("menuitemradio", { name: /立即发送/ }).click();
    assert.match(await controls.locator(".run-send-primary").innerText(), /立即发送\s*Enter/);
    await composer.fill("立即处理这条消息");
    await composer.press("Enter");
    await page.waitForFunction(() => globalThis.__rCodeSendModes?.length === 2);
    assert.deepEqual(await page.evaluate(() => globalThis.__rCodeSendModes), ["queue", "send_now"]);

    await page.locator(".send").waitFor({ state: "visible" });
    await page.evaluate(() => {
      globalThis.__rCodeFailNextSend = true;
    });
    await composer.fill("失败后恢复这份草稿");
    await composer.press("Enter");
    await page.getByText(/mock send rejection/).waitFor({ state: "visible" });
    assert.equal(await composer.inputValue(), "失败后恢复这份草稿");
    assert.equal(
      await page.evaluate(async (id) => {
        const { readComposerDraft } = await import("/src/lib/composer-drafts.ts");
        return readComposerDraft(id);
      }, taskId),
      "失败后恢复这份草稿",
      "a rejected send must remain in the local draft cache",
    );

    await page.evaluate(async () => {
      const { useAppStore } = await import("/src/store/app.ts");
      useAppStore.getState().setScene("settings");
    });
    await page.locator("#main-content .settings-layout").waitFor({ state: "visible" });
    await page.evaluate(async (id) => {
      const { useAppStore } = await import("/src/store/app.ts");
      useAppStore.getState().openRoom(id);
    }, taskId);
    await composer.waitFor({ state: "visible" });
    assert.equal(await composer.inputValue(), "失败后恢复这份草稿");
  } finally {
    await page.evaluate(() => {
      globalThis.__rCodeReleaseSend?.();
      delete globalThis.__rCodeReleaseSend;
      delete globalThis.__rCodeSendModes;
      delete globalThis.__rCodeFailNextSend;
      delete globalThis.__TAURI_INTERNALS__;
    }).catch(() => {});
    await page.evaluate(async ({ id, original }) => {
      const { clearComposerDraft, flushComposerDrafts } = await import("/src/lib/composer-drafts.ts");
      const {
        browserMockDetails,
        browserMockSetMessages,
        browserMockTasks,
      } = await import("/src/lib/mock-data.ts");
      clearComposerDraft(id);
      flushComposerDrafts();
      const task = browserMockTasks.find((item) => item.id === id);
      if (task && original.task) Object.assign(task, structuredClone(original.task));
      browserMockDetails[id] = structuredClone(original.detail);
      browserMockSetMessages(id, structuredClone(original.messages));
    }, { id: taskId, original: baseline }).catch(() => {});
    await page.close();
  }
});

test("plain Enter sends from the new-conversation composer while Shift+Enter keeps a newline", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().setScene("home");
  });

  const composer = page.getByRole("textbox", { name: "描述新任务" });
  await composer.waitFor({ state: "visible" });
  await composer.fill("第一行");
  await composer.press("Shift+Enter");
  await composer.type("第二行");
  assert.equal(await composer.inputValue(), "第一行\n第二行");

  await page.evaluate(async () => {
    const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
    const gate = new Promise((resolve) => {
      globalThis.__rCodeReleaseHomeSend = resolve;
    });
    globalThis.__TAURI_INTERNALS__ = {
      invoke: async (command, args = {}) => {
        if (command === "cmd_agent_send") await gate;
        return browserMockInvoke(command, args);
      },
    };
  });

  try {
    await composer.fill("Enter 直接发送");
    await composer.press("Enter");
    assert.equal(await composer.inputValue(), "", "new-conversation draft must clear before first-run IPC completes");
    const pendingSend = page.getByRole("button", { name: "正在发送新对话", exact: true });
    await pendingSend.waitFor({ state: "visible" });
    assert.equal(await pendingSend.getAttribute("aria-busy"), "true");
    assert.equal(await pendingSend.locator(".send-loading-spinner").isVisible(), true);
    assert.equal(await pendingSend.isDisabled(), true);
    await page.evaluate(() => globalThis.__rCodeReleaseHomeSend?.());
    await page.locator("#main-content > .scene-room").waitFor({ state: "visible" });
  } finally {
    await page.evaluate(() => {
      globalThis.__rCodeReleaseHomeSend?.();
      delete globalThis.__rCodeReleaseHomeSend;
      delete globalThis.__TAURI_INTERNALS__;
    }).catch(() => {});
    await page.close();
  }
});

test("sidebar plus creates and opens a durable task before background preparation finishes", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    useTasksStore.getState().setCurrentProject("D:/project/rust/r-code");
    globalThis.__rCodeNewConversationCalls = [];
    const prepareGate = new Promise((resolve) => {
      globalThis.__rCodeReleasePrepare = resolve;
    });
    globalThis.__TAURI_INTERNALS__ = {
      invoke: async (command, args = {}) => {
        if (["cmd_task_create", "cmd_project_conversation_create", "cmd_task_prepare", "cmd_agent_send"].includes(command)) {
          globalThis.__rCodeNewConversationCalls.push(command);
        }
        if (command === "cmd_task_prepare") await prepareGate;
        return browserMockInvoke(command, args);
      },
    };
  });

  let taskId;
  try {
    await page.getByRole("button", { name: "新对话", exact: true }).click();
    await page.locator("#main-content > .scene-room").waitFor({ state: "visible" });

    const created = await page.evaluate(async () => {
      const { useAppStore } = await import("/src/store/app.ts");
      const { useTasksStore } = await import("/src/store/tasks.ts");
      const taskId = useAppStore.getState().currentTaskId;
      const task = useTasksStore.getState().tasks.find((candidate) => candidate.id === taskId);
      return {
        taskId,
        task,
        workspacePath: task?.workspace_path ?? null,
        calls: [...globalThis.__rCodeNewConversationCalls],
      };
    });
    taskId = created.taskId;
    assert.ok(taskId);
    assert.equal(created.task?.title, "新对话");
    assert.equal(created.task?.state, "idle");
    assert.equal(created.workspacePath, null, "global new-conversation entry must create a floating conversation");
    assert.deepEqual(created.calls, ["cmd_task_create", "cmd_task_prepare"]);
    assert.equal(
      await page.locator(".sidebar-recent .sidebar-task").filter({ hasText: "新对话" }).count(),
      1,
      "the floating conversation should appear in the recent section",
    );
    assert.equal(
      await page.locator(".sidebar-projects .sidebar-task").filter({ hasText: "新对话" }).count(),
      0,
      "a floating conversation must not be duplicated under any project",
    );
    await page.evaluate(() => globalThis.__rCodeReleasePrepare?.());

    const composer = page.getByRole("textbox", { name: "给 Agent 的消息" });
    await composer.fill("第一次发送直接复用已创建的任务");
    await composer.press("Enter");
    await page.waitForFunction(() => globalThis.__rCodeNewConversationCalls?.includes("cmd_agent_send"));
    const callsAfterSend = await page.evaluate(() => [...globalThis.__rCodeNewConversationCalls]);
    assert.equal(callsAfterSend.filter((command) => command === "cmd_task_create").length, 1);
  } finally {
    await page.evaluate(async (id) => {
      globalThis.__rCodeReleasePrepare?.();
      delete globalThis.__rCodeReleasePrepare;
      delete globalThis.__rCodeNewConversationCalls;
      delete globalThis.__TAURI_INTERNALS__;
      if (!id) return;
      const { browserMockDetails, browserMockSetMessages, browserMockTasks } = await import("/src/lib/mock-data.ts");
      const index = browserMockTasks.findIndex((task) => task.id === id);
      if (index >= 0) browserMockTasks.splice(index, 1);
      delete browserMockDetails[id];
      browserMockSetMessages(id, []);
    }, taskId).catch(() => {});
    await page.close();
  }
});

test("project add opens project management while conversation entries persist numbered tasks", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const workspacePath = "D:/project/rust/r-code";
  const baseline = await page.evaluate(async (path) => {
    const { browserMockDetails, browserMockTasks } = await import("/src/lib/mock-data.ts");
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    const original = browserMockTasks
      .filter((task) => task.workspace_path === path)
      .map((task) => ({ id: task.id, state: task.state, updated_at: task.updated_at }));
    for (const task of browserMockTasks) {
      if (task.workspace_path !== path) continue;
      task.state = "archived";
      if (browserMockDetails[task.id]) browserMockDetails[task.id].task.state = "archived";
    }
    globalThis.__rCodeProjectCreateCalls = [];
    globalThis.__rCodePerformanceIpcProbe = (command) => {
      if (["cmd_project_conversation_create", "cmd_task_prepare"].includes(command)) {
        globalThis.__rCodeProjectCreateCalls.push(command);
      }
    };
    await useTasksStore.getState().refreshTasks();
    useTasksStore.getState().setCurrentProject(path);
    await useTasksStore.getState().refreshDashboard(path);
    useAppStore.getState().openDashboard(path);
    return original;
  }, workspacePath);

  const waitForActiveTitle = async (title) => {
    await page.locator(".sidebar-task-row.active").getByText(title, { exact: true }).waitFor({ state: "visible" });
  };

  try {
    await page.locator("#main-content > .scene-dashboard").waitFor({ state: "visible" });

    const projectAdd = page.getByRole("button", { name: "添加项目", exact: true });
    await projectAdd.click();
    await page.locator("#main-content > .scene-projects").waitFor({ state: "visible" });
    assert.equal(
      await page.evaluate(() => globalThis.__rCodeProjectCreateCalls.length),
      0,
      "the projects header add button must never create a conversation",
    );
    await page.locator(".sidebar-project-head").filter({ hasText: "r-code" }).click();
    await page.locator("#main-content > .scene-dashboard").waitFor({ state: "visible" });

    await page.getByRole("button", { name: "新建任务", exact: true }).click();
    await waitForActiveTitle("新对话");

    const createFromProjectMenu = async () => {
      await page.getByRole("button", { name: "r-code 项目操作", exact: true }).click();
      await page.getByRole("menuitem", { name: "新建对话", exact: true }).click();
    };
    await createFromProjectMenu();
    await waitForActiveTitle("新对话 2");
    await createFromProjectMenu();
    await waitForActiveTitle("新对话 3");

    await createFromProjectMenu();
    await waitForActiveTitle("新对话 4");
    await createFromProjectMenu();
    await waitForActiveTitle("新对话 5");

    await createFromProjectMenu();
    const limitToast = page.locator(".toast--warn").filter({ hasText: "已达到 5 个对话上限" });
    await limitToast.waitFor({ state: "visible" });
    assert.match(await limitToast.innerText(), /请先归档一个对话/);

    const result = await page.evaluate((original) => {
      const originalIds = new Set(original.map((task) => task.id));
      const created = globalThis.__rCodeProjectCreateCalls;
      return import("/src/lib/mock-data.ts").then(({ browserMockTasks }) => ({
        titles: browserMockTasks
          .filter((task) => task.workspace_path === "D:/project/rust/r-code" && !originalIds.has(task.id))
          .map((task) => task.title)
          .sort((left, right) => {
            const sequence = (title) => Number(title.match(/\d+$/)?.[0] ?? "1");
            return sequence(left) - sequence(right);
          }),
        createCalls: created.filter((command) => command === "cmd_project_conversation_create").length,
        prepareCalls: created.filter((command) => command === "cmd_task_prepare").length,
      }));
    }, baseline);
    assert.deepEqual(result.titles, ["新对话", "新对话 2", "新对话 3", "新对话 4", "新对话 5"]);
    assert.equal(result.createCalls, 6, "the rejected click still reaches the authoritative backend limit");
    assert.equal(result.prepareCalls, 5, "a rejected conversation must never start preparation");
  } finally {
    await page.evaluate(async ({ path, original }) => {
      const originalIds = new Set(original.map((task) => task.id));
      const { browserMockDetails, browserMockSetMessages, browserMockTasks } = await import("/src/lib/mock-data.ts");
      for (let index = browserMockTasks.length - 1; index >= 0; index -= 1) {
        const task = browserMockTasks[index];
        if (task.workspace_path !== path || originalIds.has(task.id)) continue;
        browserMockTasks.splice(index, 1);
        delete browserMockDetails[task.id];
        browserMockSetMessages(task.id, []);
      }
      for (const saved of original) {
        const task = browserMockTasks.find((candidate) => candidate.id === saved.id);
        if (!task) continue;
        task.state = saved.state;
        task.updated_at = saved.updated_at;
        if (browserMockDetails[task.id]) browserMockDetails[task.id].task = task;
      }
      delete globalThis.__rCodeProjectCreateCalls;
      delete globalThis.__rCodePerformanceIpcProbe;
    }, { path: workspacePath, original: baseline }).catch(() => {});
    await page.close();
  }
});

test("browser mock generic and project create paths share one structured limit", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const workspacePath = "D:/qa/t11/browser-mock-limit";

  try {
    const result = await page.evaluate(async (path) => {
      const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
      const { browserMockTasks } = await import("/src/lib/mock-data.ts");
      const createdIds = [];
      for (let index = 1; index <= 5; index += 1) {
        const task = await browserMockInvoke("cmd_task_create", {
          workspacePath: path,
          title: `Generic ${index}`,
          goal: "",
          mode: "edit",
          providerName: null,
          agentEngine: null,
        });
        createdIds.push(task.id);
      }

      const captureError = async (command, args) => {
        try {
          await browserMockInvoke(command, args);
          return null;
        } catch (error) {
          return {
            code: error?.code,
            message: error?.message,
            limit: error?.limit,
          };
        }
      };
      const genericError = await captureError("cmd_task_create", {
        workspacePath: path,
        title: "Generic blocked",
        goal: "",
        mode: "edit",
      });
      const projectError = await captureError("cmd_project_conversation_create", {
        workspacePath: path,
      });

      await browserMockInvoke("cmd_task_archive", { taskId: createdIds[0] });
      const replacement = await browserMockInvoke("cmd_project_conversation_create", {
        workspacePath: path,
      });
      return {
        genericError,
        projectError,
        replacementTitle: replacement.title,
        activeCount: browserMockTasks.filter(
          (task) => task.workspace_path === path && task.state !== "archived",
        ).length,
      };
    }, workspacePath);

    assert.deepEqual(result.genericError, {
      code: "PROJECT_CONVERSATION_LIMIT_REACHED",
      message: "该项目最多保留 5 个未归档对话，请先归档一个后再新建",
      limit: 5,
    });
    assert.deepEqual(result.projectError, result.genericError);
    assert.equal(result.replacementTitle, "新对话");
    assert.equal(result.activeCount, 5);
  } finally {
    await page.evaluate(async (path) => {
      const { browserMockDetails, browserMockSetMessages, browserMockTasks } = await import("/src/lib/mock-data.ts");
      for (let index = browserMockTasks.length - 1; index >= 0; index -= 1) {
        const task = browserMockTasks[index];
        if (task.workspace_path !== path) continue;
        browserMockTasks.splice(index, 1);
        delete browserMockDetails[task.id];
        browserMockSetMessages(task.id, []);
      }
    }, workspacePath).catch(() => {});
    await page.close();
  }
});

test("conversation limit UI branches on the stable code instead of localized text", async () => {
  const workspacePath = "D:/qa/t11/code-only-limit";
  const exercise = async (payload) => {
    const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
    await page.goto(baseUrl, { waitUntil: "networkidle" });
    try {
      await page.evaluate(async ({ path, rejection }) => {
        const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
        const { useAppStore } = await import("/src/store/app.ts");
        const { useTasksStore } = await import("/src/store/tasks.ts");
        globalThis.__TAURI_INTERNALS__ = {
          invoke: async (command, args = {}) => {
            if (command === "cmd_project_conversation_create") throw rejection;
            return browserMockInvoke(command, args);
          },
        };
        await useTasksStore.getState().refreshTasks();
        useTasksStore.getState().setCurrentProject(path);
        useAppStore.getState().openDashboard(path);
      }, { path: workspacePath, rejection: payload });
      await page.locator("#main-content > .scene-dashboard").waitFor({ state: "visible" });
      await page.getByRole("button", { name: "新建任务", exact: true }).click();
      await page.locator(".toast--warn, .toast--error").first().waitFor({ state: "visible" });
      return {
        warning: await page.locator(".toast--warn").allInnerTexts(),
        error: await page.locator(".toast--error").allInnerTexts(),
      };
    } finally {
      await page.evaluate(() => {
        delete globalThis.__TAURI_INTERNALS__;
      }).catch(() => {});
      await page.close();
    }
  };

  const coded = await exercise({
    code: "PROJECT_CONVERSATION_LIMIT_REACHED",
    message: "This wording is intentionally unrelated to the localized limit copy.",
    limit: 7,
  });
  assert.equal(coded.error.length, 0);
  assert.equal(coded.warning.length, 1);
  assert.match(coded.warning[0], /已达到 7 个对话上限/);

  const matchingTextOnly = await exercise({
    code: "COMMAND_FAILED",
    message: "该项目最多保留 5 个未归档对话，请先归档一个后再新建",
    limit: 5,
  });
  assert.equal(matchingTextOnly.warning.length, 0);
  assert.equal(matchingTextOnly.error.length, 1);
  assert.match(matchingTextOnly.error[0], /无法创建新对话/);
});

test("/clear empties the active task context without creating another sidebar task", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const taskId = "mock-task-complete";
  const baseline = await page.evaluate(async (id) => {
    const { browserMockDetails, browserMockMessages, browserMockTasks } = await import("/src/lib/mock-data.ts");
    return {
      task: structuredClone(browserMockTasks.find((task) => task.id === id)),
      detail: structuredClone(browserMockDetails[id]),
      messages: structuredClone(browserMockMessages(id)),
    };
  }, taskId);

  try {
    await page.evaluate(async (id) => {
      const { useAppStore } = await import("/src/store/app.ts");
      const { useTasksStore } = await import("/src/store/tasks.ts");
      globalThis.__rCodeClearCalls = [];
      globalThis.__rCodePerformanceIpcProbe = (command) => {
        if (["cmd_task_clear_context", "cmd_task_create"].includes(command)) {
          globalThis.__rCodeClearCalls.push(command);
        }
      };
      await Promise.all([
        useTasksStore.getState().refreshTasks(),
        useTasksStore.getState().refreshDetail(id),
      ]);
      useAppStore.getState().openRoom(id);
    }, taskId);
    await page.locator("#main-content > .scene-room").waitFor({ state: "visible" });
    const composer = page.getByRole("textbox", { name: "给 Agent 的消息" });
    await composer.fill("/clear");
    await composer.press("Enter");
    await page.getByText(/当前任务已切换到空白上下文/).waitFor({ state: "visible" });

    const cleared = await page.evaluate(async (id) => {
      const { browserMockDetails, browserMockMessages, browserMockTasks } = await import("/src/lib/mock-data.ts");
      const { useAppStore } = await import("/src/store/app.ts");
      const detail = browserMockDetails[id];
      return {
        currentTaskId: useAppStore.getState().currentTaskId,
        taskCount: browserMockTasks.filter((task) => task.id === id).length,
        parentBranchId: detail.active_branch.parent_branch_id,
        eventTypes: detail.events.map((event) => event.event_type),
        messages: browserMockMessages(id),
        calls: [...globalThis.__rCodeClearCalls],
      };
    }, taskId);
    assert.equal(cleared.currentTaskId, taskId);
    assert.equal(cleared.taskCount, 1);
    assert.equal(cleared.parentBranchId, baseline.detail.active_branch.id);
    assert.deepEqual(cleared.eventTypes, ["session_cleared"]);
    assert.deepEqual(cleared.messages, []);
    assert.deepEqual(cleared.calls, ["cmd_task_clear_context"]);

    const branchProjection = await page.evaluate(async (id) => {
      const { useTasksStore } = await import("/src/store/tasks.ts");
      const { browserMockDetails } = await import("/src/lib/mock-data.ts");
      return {
        store: useTasksStore.getState().details[id]?.branches.map((branch) => branch.id) ?? [],
        storeActive: useTasksStore.getState().details[id]?.active_branch.id ?? null,
        mock: browserMockDetails[id].branches.map((branch) => branch.id),
        mockActive: browserMockDetails[id].active_branch.id,
      };
    }, taskId);
    assert.equal(branchProjection.store.length, 2, JSON.stringify(branchProjection));
    assert.equal(
      await page.locator(".room-history-picker").count(),
      1,
      JSON.stringify({ branchProjection, scopebar: await page.locator(".room-scopebar").innerText() }),
    );
    const historyPicker = page.getByRole("combobox", { name: "选择对话历史分支" });
    await historyPicker.waitFor({ state: "visible" });
    const activeBranchId = await page.evaluate(async (id) => {
      const { browserMockDetails } = await import("/src/lib/mock-data.ts");
      return browserMockDetails[id].active_branch.id;
    }, taskId);
    const crossTaskError = await page.evaluate(async (branchId) => {
      const { sessionMessagesForBranch } = await import("/src/lib/ipc.ts");
      try {
        await sessionMessagesForBranch("mock-task-api", branchId);
        return null;
      } catch (cause) {
        return String(cause);
      }
    }, activeBranchId);
    assert.match(crossTaskError ?? "", /会话分支不属于当前任务/);
    const historicalTexts = await page.evaluate(async ({ id, branchId }) => {
      const { sessionMessagesForBranch } = await import("/src/lib/ipc.ts");
      return (await sessionMessagesForBranch(id, branchId))
        .filter((message) => message.kind === "message")
        .map((message) => message.text);
    }, { id: taskId, branchId: baseline.detail.active_branch.id });
    assert.deepEqual(historicalTexts, baseline.messages
      .filter((message) => message.kind === "message")
      .map((message) => message.text));

    const draft = "切到历史前尚未提交的草稿";
    await composer.fill(draft);
    const beforeBrowse = await page.evaluate(async (id) => {
      const { browserMockDetails } = await import("/src/lib/mock-data.ts");
      const detail = browserMockDetails[id];
      return {
        activeBranchId: detail.active_branch.id,
        runIds: detail.runs.map((run) => run.id),
        queuedMessageIds: detail.queued_messages.map((message) => message.id),
        taskMode: detail.task.mode,
        taskState: detail.task.state,
      };
    }, taskId);

    await historyPicker.selectOption(baseline.detail.active_branch.id);
    await page.getByText("历史分支 · 只读", { exact: true }).waitFor({ state: "visible" });
    await page.waitForTimeout(500);
    const historicalTimelineText = await page.locator(".timeline").innerText();
    assert.ok(historicalTimelineText.includes(baseline.task.goal), historicalTimelineText);
    assert.equal(await page.getByRole("textbox", { name: "给 Agent 的消息" }).count(), 0);
    assert.equal(await page.locator(".message-edit").count(), 0);

    await page.getByRole("button", { name: "返回当前对话" }).click();
    await composer.waitFor({ state: "visible" });
    assert.equal(await composer.inputValue(), draft);
    await page.waitForFunction(
      (historicalText) => !document.querySelector(".timeline")?.textContent?.includes(historicalText),
      baseline.task.goal,
    );

    // Returning to the live branch must invalidate a slower historical read. Otherwise old
    // messages can become editable after the composer has already been restored.
    await page.evaluate(() => {
      globalThis.__rCodeBrowserMockDelayMs = { cmd_session_messages_for_branch: 400 };
    });
    await historyPicker.selectOption(baseline.detail.active_branch.id);
    await page.getByText("历史分支 · 只读", { exact: true }).waitFor({ state: "visible" });
    await historyPicker.selectOption("");
    await composer.waitFor({ state: "visible" });
    await page.waitForTimeout(550);
    assert.equal(await composer.inputValue(), draft);
    assert.ok(!(await page.locator(".timeline").innerText()).includes(baseline.task.goal));

    const afterBrowse = await page.evaluate(async (id) => {
      delete globalThis.__rCodeBrowserMockDelayMs;
      const { browserMockDetails } = await import("/src/lib/mock-data.ts");
      const detail = browserMockDetails[id];
      return {
        activeBranchId: detail.active_branch.id,
        runIds: detail.runs.map((run) => run.id),
        queuedMessageIds: detail.queued_messages.map((message) => message.id),
        taskMode: detail.task.mode,
        taskState: detail.task.state,
      };
    }, taskId);
    assert.deepEqual(afterBrowse, beforeBrowse);
    await page.getByText(/当前任务已切换到空白上下文/)
      .waitFor({ state: "hidden", timeout: 7500 });
  } finally {
    await page.evaluate(async ({ id, original }) => {
      delete globalThis.__rCodeBrowserMockDelayMs;
      delete globalThis.__rCodePerformanceIpcProbe;
      delete globalThis.__rCodeClearCalls;
      const { browserMockDetails, browserMockSetMessages, browserMockTasks } = await import("/src/lib/mock-data.ts");
      const task = browserMockTasks.find((candidate) => candidate.id === id);
      if (task && original.task) Object.assign(task, structuredClone(original.task));
      browserMockDetails[id] = structuredClone(original.detail);
      browserMockSetMessages(id, structuredClone(original.messages));
    }, { id: taskId, original: baseline }).catch(() => {});
    await page.close();
  }
});

test("composer Up and Down traverse this conversation's user input history", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(() => {
    globalThis.__rCodeBrowserMockDelayMs = { cmd_session_messages: 450 };
  });

  const taskRow = page.locator(".sidebar-task-row").filter({ hasText: "修复任务队列并发问题" });
  await taskRow.locator(".sidebar-task").click();
  await page.locator("#main-content > .scene-room").waitFor({ state: "visible" });

  const composer = page.getByRole("textbox", { name: "给 Agent 的消息" });
  await composer.fill("尚未发送的草稿");
  await composer.press("ArrowUp");
  await page.waitForFunction(() => (
    document.querySelector('textarea[aria-label="给 Agent 的消息"]')?.value
      === "梳理任务队列执行路径并修复并发状态竞争。"
  ));
  assert.equal(await composer.inputValue(), "梳理任务队列执行路径并修复并发状态竞争。");
  await composer.press("ArrowUp");
  assert.equal(await composer.inputValue(), "编辑历史消息后，原分支的上下文还会保留吗？");
  await composer.press("ArrowDown");
  assert.equal(await composer.inputValue(), "梳理任务队列执行路径并修复并发状态竞争。");
  await composer.press("ArrowDown");
  assert.equal(await composer.inputValue(), "尚未发送的草稿");

  await composer.fill("第一行\n第二行");
  await composer.press("ArrowUp");
  assert.equal(await composer.inputValue(), "第一行\n第二行", "multiline caret movement must stay native");
  await page.evaluate(() => {
    delete globalThis.__rCodeBrowserMockDelayMs;
  });
  await page.close();
});

test("diagnostics uses fixed retention and a native-folder export flow", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  await page.getByRole("button", { name: "设置", exact: true }).click();
  await page.getByRole("button", { name: "诊断", exact: true }).click();
  await page.getByRole("heading", { name: "诊断日志", exact: true }).waitFor();
  await page.getByText("日志按日滚动，固定保留最近 7 天。", { exact: false }).waitFor();
  assert.equal(
    await page.getByRole("textbox", { name: "输出目录" }).count(),
    0,
    "export destination must not be a manually editable path",
  );

  await page.getByRole("button", { name: "生成预览", exact: true }).click();
  await page.getByText("警告/错误条数", { exact: true }).waitFor();
  await page.getByRole("button", { name: "选择目录并导出", exact: true }).click();
  await page.getByText("已生成：", { exact: false }).waitFor();
  await page.close();
});

test("agent coordination prompts can be edited, saved, and restored", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const original = await page.evaluate(async () => {
    const { browserMockSettings } = await import("/src/lib/mock-data.ts");
    return structuredClone(browserMockSettings.config.agent_prompts);
  });

  try {
    await page.getByRole("button", { name: "设置", exact: true }).click();
    await page.getByRole("button", { name: "Agent 编排", exact: true }).click();
    await page.getByRole("heading", { name: "委派路由", exact: true }).waitFor({ state: "visible" });
    assert.equal(await page.locator("#set-quality-reviewer").inputValue(), "r_code");
    assert.equal(await page.getByRole("textbox", { name: "主 Agent 协作 Prompt" }).count(), 0, "prompts must no longer be split across Settings");

    const center = await openKnowledgeSettings(page);
    await center.getByRole("tab", { name: "协作 Prompt", exact: true }).click();
    const mainPrompt = page.getByRole("textbox", { name: "主 Agent", exact: true });
    const childPrompt = page.getByRole("textbox", { name: "子代理", exact: true });
    await mainPrompt.fill("主代理负责统筹，只有必要时才委派。");
    await childPrompt.fill("子代理只完成边界清晰的子任务并返回摘要。");
    await page.getByRole("button", { name: "保存并应用", exact: true }).click();
    await page.getByText("全局协作 Prompt 已保存，所有项目会自动继承。", { exact: true }).waitFor({ state: "visible" });

    await center.getByRole("tab", { name: "记忆", exact: true }).click();
    await center.getByRole("tab", { name: "协作 Prompt", exact: true }).click();
    assert.equal(await mainPrompt.inputValue(), "主代理负责统筹，只有必要时才委派。");
    assert.equal(await childPrompt.inputValue(), "子代理只完成边界清晰的子任务并返回摘要。");

    await page.getByRole("button", { name: "恢复内置 Prompt" }).click();
    await page.waitForFunction(
      (element) => element.value !== "主代理负责统筹，只有必要时才委派。",
      await mainPrompt.elementHandle(),
    );
    assert.notEqual(await mainPrompt.inputValue(), "主代理负责统筹，只有必要时才委派。");
  } finally {
    await page.evaluate(async (value) => {
      const { browserMockSettings } = await import("/src/lib/mock-data.ts");
      browserMockSettings.config.agent_prompts = structuredClone(value);
    }, original).catch(() => {});
    await page.close();
  }
});

test("planning suggestion card only appears for eligible deepseek with validated evidence", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const original = await page.evaluate(async () => {
    const { browserMockSettings } = await import("/src/lib/mock-data.ts");
    return structuredClone({
      planning: browserMockSettings.config.planning,
    });
  });

  try {
    await page.getByRole("button", { name: "设置", exact: true }).click();
    await page.getByRole("button", { name: "Agent 编排", exact: true }).click();

    // 证据未通过：卡片可见但开关不可启用，提示「功能仍在验证中」。
    await page.getByRole("heading", { name: "复杂任务先建议制定计划", exact: true })
      .waitFor({ state: "visible" });
    assert.equal(await page.locator("#set-planning-suggest").isDisabled(), true,
      "evidence-pending providers cannot enable the customer switch");
    assert.match(await page.locator("#planning-suggestion-block .hint").textContent(),
      /功能仍在验证中/);

    // 证据通过后：开关可启用并持久化到 planning.suggest_complex_tasks。
    await page.evaluate(async () => {
      const { setBrowserMockPlanningStatus } = await import("/src/lib/browser-mock-runtime.ts");
      setBrowserMockPlanningStatus({
        release_state: "validated",
        emergency_off: false,
        evidence_version: "test-1",
        eligibility_profile_version: "deepseek-plan-v1",
        customer_card_visible: true,
        evidence_validated: true,
        basis: "test",
      });
    });
    await page.locator("#planning-suggestion-block .guide-link").click();
    await page.getByRole("dialog", { name: "Plan 模式与复杂任务建议" }).waitFor({ state: "visible" });
    // 指引手册四件事：没有 catalog / 工具 schema / 证据统计等内部术语。
    const guideText = await page.locator(".guide-dialog").textContent();
    for (const banned of ["catalog", "plan_ready", "bootstrap", "resident", "profile version"]) {
      assert.ok(!guideText.includes(banned), `guide copy must not leak internal term: ${banned}`);
    }
    await page.keyboard.press("Escape");
    await page.locator(".guide-dialog").waitFor({ state: "detached" });

    await page.evaluate(async () => {
      const { setBrowserMockPlanningStatus } = await import("/src/lib/browser-mock-runtime.ts");
      setBrowserMockPlanningStatus({
        release_state: "validated",
        emergency_off: false,
        evidence_version: "test-1",
        eligibility_profile_version: "deepseek-plan-v1",
        customer_card_visible: true,
        evidence_validated: true,
        basis: "test",
      });
      window.dispatchEvent(new Event("r-code:planning-status-changed"));
    });
    await page.waitForFunction(() => {
      const input = document.querySelector("#set-planning-suggest");
      return input instanceof HTMLInputElement && !input.disabled;
    });
    const enabled = await page.locator("#set-planning-suggest").isEnabled();
    assert.equal(enabled, true, "validated evidence unlocks the customer switch");
    await page.locator("#set-planning-suggest").check();
    await page.waitForFunction(async () => {
      const { settingsGet } = await import("/src/lib/ipc.ts");
      const { config } = await settingsGet(true);
      return config.planning?.suggest_complex_tasks === true;
    });
  } finally {
    await page.evaluate(async (snapshot) => {
      const { browserMockSettings } = await import("/src/lib/mock-data.ts");
      browserMockSettings.config.planning = snapshot.planning;
      const { setBrowserMockPlanningStatus } = await import("/src/lib/browser-mock-runtime.ts");
      setBrowserMockPlanningStatus({
        release_state: "off",
        emergency_off: false,
        evidence_version: "",
        eligibility_profile_version: "deepseek-plan-v1",
        customer_card_visible: true,
        evidence_validated: false,
        basis: "reset",
      });
    }, original);
    await page.close();
  }
});

test("plan entry dialog stays two-action, swaps guide without stacking, and escapes like continue", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  // 打开演示任务房间
  const firstTaskId = await page.evaluate(async () => {
    const { useTasksStore } = await import("/src/store/tasks.ts");
    const tasks = useTasksStore.getState().tasks;
    return tasks.length > 0 ? tasks[0].id : "mock-task-queue";
  });
  await page.evaluate(async (taskId) => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().openRoom(taskId);
  }, firstTaskId);
  await page.getByRole("textbox", { name: "给 Agent 的消息" }).waitFor({ state: "visible" });

  // 注入待决建议（mock 决定后自动清空）。
  await page.evaluate(async () => {
    const { setBrowserMockPlanEntryOffer } = await import("/src/lib/browser-mock-runtime.ts");
    setBrowserMockPlanEntryOffer({
      id: "offer-test-1",
      task_id: "mock-task-queue",
      revision: 1,
      state: "pending",
      customer_copy: {
        lead: "它涉及多个相互关联的改动。",
        suffix: "先制定计划可以让你确认范围和顺序，再开始修改。",
        quiet_note: "选择直接继续后，本任务不再主动弹出；你仍可随时手动选择 Plan。",
        version: 1,
      },
      notice: null,
      continuation_state: "none",
    });
  });
  const dialog = page.getByRole("dialog", { name: "这个任务适合先列个计划" });
  await dialog.waitFor({ state: "visible" });

  // 客户文案只来自固定模板：不得出现内部 signal / 工具 / 目录 / reason 词。
  const dialogText = await dialog.textContent();
  for (const banned of ["multi_subsystem", "plan_ready", "catalog", "reason", "offer", "revision"]) {
    assert.ok(!dialogText.includes(banned), `customer dialog must not leak internal term: ${banned}`);
  }
  // 初始焦点在推荐主动作「先制定计划」。
  await page.waitForFunction(() => document.activeElement instanceof HTMLButtonElement
    && document.activeElement.textContent?.trim() === "先制定计划");

  // 打开手册：替换而非叠加（同一时刻只有一个 aria-modal）。
  await dialog.getByRole("button", { name: "Plan 模式会做什么？" }).click();
  const guide = page.getByRole("dialog", { name: "Plan 模式与复杂任务建议" });
  await guide.waitFor({ state: "visible" });
  await dialog.waitFor({ state: "detached", timeout: 2000 }).catch(() => {});
  const modalCount = await page.locator('[aria-modal="true"]').count();
  assert.equal(modalCount, 1, "guide replaces the decision dialog instead of stacking");

  // 手册 Escape 只关闭手册，不代表拒绝；焦点回到「Plan 模式会做什么？」。
  await page.keyboard.press("Escape");
  await guide.waitFor({ state: "detached" });
  await dialog.waitFor({ state: "visible" });
  await page.waitForFunction(() => document.activeElement instanceof HTMLButtonElement
    && document.activeElement.textContent?.includes("Plan 模式会做什么"));

  // 决策弹窗 Escape 等价于「直接继续」：offer 清空、弹窗消失。
  await page.keyboard.press("Escape");
  await dialog.waitFor({ state: "detached" });
  const cleared = await page.evaluate(async () => {
    const { taskDetail } = await import("/src/lib/ipc.ts");
    const detail = await taskDetail("mock-task-queue").catch(() => null);
    return detail?.pending_plan_entry_offer ?? null;
  });
  assert.equal(cleared, null, "escape must decide (continue) instead of leaving an ambiguous pending state");

  await page.evaluate(async () => {
    const { setBrowserMockPlanEntryOffer } = await import("/src/lib/browser-mock-runtime.ts");
    setBrowserMockPlanEntryOffer(null);
  });
  await page.close();
});

test("help menu opens the plan guide from anywhere and closes back", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.getByRole("button", { name: "帮助", exact: true }).click();
  await page.getByRole("menuitem", { name: "Plan 模式与复杂任务建议" }).click();
  const guide = page.getByRole("dialog", { name: "Plan 模式与复杂任务建议" });
  await guide.waitFor({ state: "visible" });
  await page.keyboard.press("Escape");
  await guide.waitFor({ state: "detached" });
  await page.close();
});


test("project prompts merge explicitly and project Skills promote into inherited global Skills", async () => {
  const page = await browser.newPage({ viewport: { width: 1280, height: 860 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const workspacePath = "D:/project/rust/r-code";
  const skillName = "project-scope-check";

  try {
    await page.evaluate(async (path) => {
      const { useAppStore } = await import("/src/store/app.ts");
      const { useTasksStore } = await import("/src/store/tasks.ts");
      useTasksStore.getState().setCurrentProject(path);
      useAppStore.getState().openKnowledge("prompts");
    }, workspacePath);
    const center = page.getByRole("region", { name: "知识与指令" });
    await center.waitFor({ state: "visible" });
    assert.equal(await center.getByRole("button", { name: "r-code", exact: true }).getAttribute("aria-pressed"), "true");

    const append = center.getByRole("button", { name: /追加/ });
    const override = center.getByRole("button", { name: /覆盖/ });
    assert.equal(await append.getAttribute("aria-pressed"), "true");
    await override.click();
    await center.getByRole("textbox", { name: "主 Agent", exact: true }).fill("项目主 Agent 规则");
    await center.getByRole("textbox", { name: "子代理", exact: true }).fill("项目子代理规则");
    await center.getByRole("button", { name: "保存并应用", exact: true }).click();
    await center.getByText("r-code 的协作 Prompt 已保存并应用。", { exact: true }).waitFor();
    const effective = await page.evaluate(async (path) => {
      const { knowledgePromptsGet } = await import("/src/lib/ipc.ts");
      return knowledgePromptsGet(path);
    }, workspacePath);
    assert.equal(effective.project.mode, "override");
    assert.equal(effective.effective.main_agent, "项目主 Agent 规则");

    await center.getByRole("tab", { name: "Skills", exact: true }).click();
    await center.getByRole("button", { name: "新建项目 Skill", exact: true }).click();
    await center.getByRole("textbox", { name: "调用名", exact: true }).fill(skillName);
    await center.getByRole("textbox", { name: "简介", exact: true }).fill("验证项目 Skill 同步");
    await center.getByRole("textbox", { name: "Skill 指令", exact: true }).fill("先验证，再返回项目结果。");
    await center.getByRole("button", { name: "保存 Skill", exact: true }).click();
    await center.getByText(`项目 Skill /${skillName} 已保存，仅在 r-code 中可用。`, { exact: true }).waitFor();
    await center.getByRole("button", { name: "同步到全局", exact: true }).click();
    await center.getByText(`/${skillName} 已同步到全局；项目副本已移除，当前项目改为自动继承。`, { exact: true }).waitFor();
    assert.equal(await center.getByRole("tab", { name: /继承自全局/ }).getAttribute("aria-selected"), "true");
    await center.getByRole("button", { name: new RegExp(`/${skillName}`) }).waitFor({ state: "visible" });

    const catalog = await page.evaluate(async (path) => {
      const { workflowSkillsList } = await import("/src/lib/ipc.ts");
      return workflowSkillsList(path);
    }, workspacePath);
    const matching = catalog.filter((skill) => skill.name === skillName);
    assert.equal(matching.length, 1);
    assert.equal(matching[0].scope, "global");
    assert.equal(matching[0].inherited, true);
  } finally {
    await page.evaluate(async ({ path, name }) => {
      const { knowledgePromptsReset, workflowSkillDelete, workflowSkillsList } = await import("/src/lib/ipc.ts");
      await knowledgePromptsReset(path);
      const skill = (await workflowSkillsList()).find((item) => item.name === name);
      if (skill) await workflowSkillDelete(skill.id, "global");
    }, { path: workspacePath, name: skillName }).catch(() => {});
    await page.close();
  }
});

test("subagent trees derive depth safely and PeerMessage activity never retains message content", async () => {
  const page = await browser.newPage({ viewport: { width: 1100, height: 760 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const contract = await page.evaluate(async () => {
    const { activityTraceReducer, createActivityTraceState } = await import("/src/components/room/activity.ts");
    const { buildLiveEntries, buildSubagentForest, flattenSubagentForest } = await import("/src/components/room/SubagentWorkbench.tsx");
    const child = (id, parentRunId = null) => ({
      id,
      parentRunId,
      label: id,
      runtimeKind: "native",
      model: "test-model",
      accessMode: "read_only",
      requireApproval: false,
      routingReason: null,
      status: "running",
      phase: "requesting",
      detail: null,
      startedAt: 1,
      lastEventAt: 1,
      endedAt: null,
      events: [],
    });

    const roots = buildSubagentForest([
      child("child", "main-run"),
      child("grandchild", "child"),
    ], ["main-run"]);
    const legacy = buildSubagentForest([child("legacy-a"), child("legacy-b")]);
    const guarded = buildSubagentForest([
      child("orphan", "missing-parent"),
      child("cycle-a", "cycle-b"),
      child("cycle-b", "cycle-a"),
      child("self-cycle", "self-cycle"),
    ], ["main-run"]);

    const scope = {
      run_id: "child",
      agent_id: "child",
      parent_run_id: "main-run",
      agent_kind: "subagent",
      runtime_kind: "native",
      model: "test-model",
    };
    const queued = activityTraceReducer(createActivityTraceState(), {
      type: "event",
      at: 10,
      event: {
        type: "scoped",
        scope,
        event: {
          type: "peer_message",
          message_id: "peer-1",
          sender_agent_id: "child",
          recipient_agent_id: "grandchild",
          status: "queued",
          content_chars: 37,
          // An unknown legacy/host field must be ignored rather than copied into ActivityTraceState.
          content_preview: "SECRET_SHOULD_NEVER_REACH_UI",
        },
      },
    });
    const delivered = activityTraceReducer(queued, {
      type: "event",
      at: 11,
      event: {
        type: "scoped",
        scope,
        event: {
          type: "peer_message",
          message_id: "peer-1",
          sender_agent_id: "child",
          recipient_agent_id: "grandchild",
          status: "delivered",
          content_chars: 37,
        },
      },
    });
    const peer = delivered.subagents[0];
    return {
      tree: flattenSubagentForest(roots).map((node) => ({
        id: node.child.id,
        depth: node.depth,
        descendants: node.descendantCount,
        anomaly: node.anomaly,
      })),
      legacy: flattenSubagentForest(legacy).map((node) => ({ depth: node.depth, anomaly: node.anomaly })),
      guarded: flattenSubagentForest(guarded).map((node) => ({ id: node.child.id, depth: node.depth, anomaly: node.anomaly })),
      peer: {
        parentRunId: peer.parentRunId,
        detail: peer.detail,
        events: peer.events.map((event) => ({
          kind: event.kind,
          status: event.peerStatus,
          sender: event.peerSenderAgentId,
          recipient: event.peerRecipientAgentId,
          chars: event.peerContentChars,
          detail: event.detail,
        })),
        liveEntries: buildLiveEntries(peer.events, peer.status),
      },
      leaked: JSON.stringify(delivered).includes("SECRET_SHOULD_NEVER_REACH_UI"),
    };
  });

  assert.deepEqual(contract.tree, [
    { id: "child", depth: 1, descendants: 1, anomaly: null },
    { id: "grandchild", depth: 2, descendants: 0, anomaly: null },
  ]);
  assert.deepEqual(contract.legacy, [
    { depth: 1, anomaly: null },
    { depth: 1, anomaly: null },
  ], "legacy one-level data must remain two ordinary roots");
  assert.deepEqual(contract.guarded, [
    { id: "orphan", depth: 1, anomaly: "orphan" },
    { id: "cycle-a", depth: 1, anomaly: "cycle" },
    { id: "cycle-b", depth: 1, anomaly: "cycle" },
    { id: "self-cycle", depth: 1, anomaly: "cycle" },
  ]);
  assert.equal(contract.peer.events.length, 1, "queued/delivered updates must merge by message_id");
  assert.deepEqual(contract.peer.events[0], {
    kind: "peer_message",
    status: "delivered",
    sender: "child",
    recipient: "grandchild",
    chars: 37,
    detail: null,
  });
  assert.equal(contract.peer.parentRunId, "main-run");
  assert.match(contract.peer.detail, /child → grandchild/);
  assert.match(contract.peer.liveEntries[0].text, /发送消息（37 字符） · child → grandchild · 已送达/);
  assert.equal(contract.leaked, false);
  await page.close();
});

test("subagents open in deduplicated tabs while the overview stays available", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const taskRow = page.locator(".sidebar-task-row").filter({ hasText: "修复任务队列并发问题" });
  await taskRow.locator(".sidebar-task").click();
  await page.locator("#main-content > .scene-room").waitFor({ state: "visible" });

  const timelineSubagent = page.locator(".timeline-subagent-chip").filter({ hasText: "Codex CLI · 检查并发边界" });
  await timelineSubagent.waitFor({ state: "visible" });
  await timelineSubagent.click();

  const tablist = page.getByRole("tablist", { name: "任务工作台标签" });
  const tabs = tablist.getByRole("tab");
  const activeSubagent = page.locator(".subagent-list-row").filter({ hasText: "Codex CLI · 检查并发边界" });
  const completedSubagent = page.locator(".subagent-list-row").filter({ hasText: "Codex CLI · 核对锁顺序" });
  const nativeChild = page.locator(".subagent-list-row").filter({ hasText: "R-Code 子代理 · 规划验证" });
  const nativeGrandchild = page.locator(".subagent-list-row").filter({ hasText: "R-Code 孙代理 · 运行定向测试" });

  await page.getByTestId("subagent-detail").waitFor({ state: "visible" });
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    const state = useAppStore.getState();
    const taskId = state.currentTaskId;
    if (!taskId) throw new Error("Room task is missing");
    const workbench = state.workbenches[taskId];
    if (!workbench) throw new Error("Task workbench is missing");
    useAppStore.setState({
      workbenches: {
        ...state.workbenches,
        [taskId]: { ...workbench, openTabs: ["summary", "plan", "review"] },
      },
    });
  });
  const workbench = page.getByTestId("workbench-panel");
  const backToOverview = workbench.getByRole("button", { name: "返回运行与子代理", exact: true });
  await backToOverview.waitFor({ state: "visible" });
  await backToOverview.click();
  await page.getByTestId("subagent-list").waitFor({ state: "visible" });
  assert.equal(
    await tablist.getByRole("tab", { name: "Codex CLI · 检查并发边界", exact: true }).count(),
    1,
    "returning to the overview must keep the opened subagent tab",
  );
  await tablist.getByRole("tab", { name: "Codex CLI · 检查并发边界", exact: true }).click();
  await page.getByTestId("subagent-detail").waitFor({ state: "visible" });

  const summaryTab = workbench.getByRole("tab", { name: /^运行与子代理/ });
  await summaryTab.waitFor({ state: "visible" });
  assert.deepEqual(
    await tabs.evaluateAll((items) => items.map((item) => item.querySelector("strong")?.textContent)),
    ["运行与子代理", "计划", "审核", "Codex CLI · 检查并发边界"],
  );
  assert.equal(await tabs.count(), 4, "opening a subagent must preserve every task workbench tab");

  await summaryTab.click();
  await page.getByTestId("subagent-list").waitFor({ state: "visible" });
  const activeSectionToggle = page.getByRole("button", { name: /正在运行子代理/ });
  const completedSectionToggle = page.getByRole("button", { name: /已结束子代理/ });
  assert.equal(await activeSectionToggle.getAttribute("aria-expanded"), "true", "active subagents are expanded by default");
  assert.equal(await completedSectionToggle.getAttribute("aria-expanded"), "false", "completed subagents are collapsed by default");
  assert.equal(await activeSubagent.isVisible(), true);
  assert.equal(await completedSubagent.isVisible(), false);
  await nativeGrandchild.waitFor({ state: "visible" });
  assert.equal(await nativeChild.getAttribute("data-tree-depth"), "1");
  assert.equal(await nativeGrandchild.getAttribute("data-tree-depth"), "2");
  assert.match(await nativeChild.locator(".subagent-tree-facts").innerText(), /深度 1.*槽位 1.*R-Code.*gpt-5\.6-terra/s);
  assert.match(await nativeChild.locator(".subagent-tree-facts").innerText(), /可继续委派.*可实时消息/s);
  assert.match(await activeSubagent.locator(".subagent-tree-facts").innerText(), /叶节点/);
  assert.equal(
    await nativeChild.getByRole("button", { name: /停止R-Code 子代理 · 规划验证及其 1 个后代/ }).count(),
    1,
    "an intermediate node must expose recursive branch cancellation",
  );

  await completedSectionToggle.click();
  assert.equal(await completedSectionToggle.getAttribute("aria-expanded"), "true");
  await completedSubagent.waitFor({ state: "visible" });

  await activeSubagent.click();
  assert.equal(await tabs.count(), 4, "opening the same subagent must activate its existing tab");
  assert.equal(
    await tablist.getByRole("tab", { name: "Codex CLI · 检查并发边界", exact: true }).getAttribute("aria-selected"),
    "true",
  );

  await summaryTab.click();
  assert.equal(
    await completedSectionToggle.getAttribute("aria-expanded"),
    "true",
    "section expansion survives a detail-to-overview round trip",
  );
  await completedSubagent.click();
  assert.equal(await tabs.count(), 5, "a different subagent gets its own tab without replacing task tools");
  assert.equal(
    await tablist.getByRole("tab", { name: "Codex CLI · 核对锁顺序", exact: true }).getAttribute("aria-selected"),
    "true",
  );

  const completedTab = tablist.getByRole("tab", { name: "Codex CLI · 核对锁顺序", exact: true });
  await completedTab.focus();
  await completedTab.press("ArrowLeft");
  assert.equal(
    await tablist.getByRole("tab", { name: "Codex CLI · 检查并发边界", exact: true }).getAttribute("aria-selected"),
    "true",
    "ArrowLeft must move and activate the previous workbench tab",
  );
  await page.keyboard.press("End");
  assert.equal(await completedTab.getAttribute("aria-selected"), "true", "End must activate the final workbench tab");
  assert.equal(
    await completedTab.locator("button").count(),
    0,
    "the close control must be a sibling instead of an interactive descendant of the tab",
  );

  await workbench.getByRole("button", { name: "打开工具启动器", exact: true }).click();
  const launcher = workbench.getByRole("dialog", { name: "工作台工具启动器" });
  await launcher.waitFor({ state: "visible" });
  // Opening the launcher moves focus on the next animation frame. Wait for that accessibility
  // contract before sending Escape; otherwise a loaded CI runner can dispatch the key to the
  // just-unmounted subagent tab and turn this into a scheduler-dependent test.
  await page.waitForFunction(() => document.activeElement?.closest(".workbench-launcher") != null);
  await page.keyboard.press("Escape");
  await page.getByTestId("subagent-detail").waitFor({ state: "visible" });
  assert.equal(
    await tablist.getByRole("tab", { name: "Codex CLI · 核对锁顺序", exact: true }).getAttribute("aria-selected"),
    "true",
    "dismissing the launcher must restore the selected subagent tab",
  );

  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    const taskId = useAppStore.getState().currentTaskId;
    if (!taskId) throw new Error("Room task is missing");
    useAppStore.getState().openWorkbenchFile(taskId, "src/main.rs", 2, 3);
  });
  await page.waitForFunction(() => (
    document.querySelector('[data-testid="workbench-panel"]')?.getAttribute("data-workbench-kind") === "files"
  ));
  assert.deepEqual(
    await tabs.evaluateAll((items) => items.map((item) => item.querySelector("strong")?.textContent)),
    ["运行与子代理", "计划", "审核", "Codex CLI · 检查并发边界", "Codex CLI · 核对锁顺序", "文件"],
    "a file opened from a subagent must be appended without removing any session tab",
  );
  assert.equal(
    await tablist.getByRole("tab", { name: "文件", exact: true }).getAttribute("aria-selected"),
    "true",
  );

  await completedTab.click();
  await page.getByTestId("subagent-detail").waitFor({ state: "visible" });
  assert.equal(
    await tablist.getByRole("tab", { name: "文件", exact: true }).count(),
    1,
    "returning to the subagent must keep the file tab available",
  );

  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    const taskId = useAppStore.getState().currentTaskId;
    if (!taskId) throw new Error("Room task is missing");
    useAppStore.getState().openWorkbenchFile(taskId, "src/main.rs", 2, 3);
  });
  await page.waitForFunction(() => (
    document.querySelector('[data-testid="workbench-panel"]')?.getAttribute("data-workbench-kind") === "files"
  ));
  assert.equal(
    await tablist.getByRole("tab", { name: "文件", exact: true }).count(),
    1,
    "opening the same file again must activate the existing Files tab",
  );

  await workbench.getByRole("button", { name: "关闭文件标签页", exact: true }).click();
  await page.getByTestId("subagent-detail").waitFor({ state: "visible" });
  assert.equal(
    await completedTab.getAttribute("aria-selected"),
    "true",
    "closing the appended file tab must return to its subagent session",
  );

  await page.close();
});

test("recursive subagent cancellation stops descendants but leaves siblings running", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const taskRow = page.locator(".sidebar-task-row").filter({ hasText: "修复任务队列并发问题" });
  await taskRow.locator(".sidebar-task").click();
  await page.locator("#main-content > .scene-room").waitFor({ state: "visible" });
  await page.locator(".timeline-subagent-chip").filter({ hasText: "Codex CLI · 检查并发边界" }).click();

  const workbench = page.getByTestId("workbench-panel");
  await workbench.getByRole("tab", { name: /^运行与子代理/ }).click();
  const parent = workbench.locator(".subagent-list-row").filter({ hasText: "R-Code 子代理 · 规划验证" });
  const grandchild = workbench.locator(".subagent-list-row").filter({ hasText: "R-Code 孙代理 · 运行定向测试" });
  const sibling = workbench.locator(".subagent-list-row").filter({ hasText: "Codex CLI · 检查并发边界" });
  await parent.getByRole("button", { name: /停止R-Code 子代理 · 规划验证及其 1 个后代/ }).click();

  await page.waitForFunction(() => {
    const rows = [...document.querySelectorAll(".subagent-list-row")];
    const stopped = (label) => rows.find((row) => row.textContent?.includes(label))?.classList.contains("status-cancelled");
    return stopped("R-Code 子代理 · 规划验证") && stopped("R-Code 孙代理 · 运行定向测试");
  });
  assert.equal(await parent.evaluate((row) => row.classList.contains("status-cancelled")), true);
  assert.equal(await grandchild.evaluate((row) => row.classList.contains("status-cancelled")), true);
  assert.equal(await sibling.evaluate((row) => row.classList.contains("status-running")), true);

  const persisted = await page.evaluate(async () => {
    const { browserMockDetails } = await import("/src/lib/mock-data.ts");
    return browserMockDetails["mock-task-queue"].runs
      .filter((run) => run.id.endsWith("native-child") || run.id.endsWith("native-grandchild") || run.id.endsWith("codex-active"))
      .map((run) => ({ id: run.id, state: run.review_state, ended: run.ended_at != null }));
  });
  assert.deepEqual(persisted, [
    { id: "mock-task-queue-codex-active", state: "pending", ended: false },
    { id: "mock-task-queue-native-child", state: "aborted", ended: true },
    { id: "mock-task-queue-native-grandchild", state: "aborted", ended: true },
  ]);
  await page.close();
});

test("subagent permissions stay three-state across live events and persisted reloads", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const reducerContract = await page.evaluate(async () => {
    const { activityTraceReducer, createActivityTraceState } = await import("/src/components/room/activity.ts");
    const scope = {
      run_id: "live-approval-child",
      agent_id: "live-approval-child",
      parent_run_id: "parent",
      agent_kind: "subagent",
      access_mode: "full_access",
      require_approval: true,
    };
    const live = activityTraceReducer(createActivityTraceState(), {
      type: "event",
      at: 1,
      event: {
        type: "scoped",
        scope,
        event: { type: "subagent_lifecycle", state: "running", detail: "running" },
      },
    });
    const persisted = activityTraceReducer(createActivityTraceState(), {
      type: "snapshot",
      running: false,
      runs: [{
        id: "persisted-approval-child",
        task_id: "task",
        branch_id: "main",
        parent_run_id: "parent",
        agent_kind: "subagent",
        agent_label: "persisted",
        summary: null,
        delegated_by_tool_call_id: null,
        model: "model",
        runtime_kind: "native",
        external_session_id: null,
        review_state: "pending",
        started_at: "2026-01-01T00:00:00.000Z",
        ended_at: null,
        usage_json: null,
        access_mode: "full_access",
        require_approval: true,
        routing_reason: null,
      }],
      queuedMessages: [],
      pendingPermissions: [],
      at: 2,
    });
    return {
      live: live.subagents.map(({ accessMode, requireApproval }) => ({ accessMode, requireApproval })),
      persisted: persisted.subagents.map(({ accessMode, requireApproval }) => ({ accessMode, requireApproval })),
    };
  });
  assert.deepEqual(reducerContract.live, [{ accessMode: "full_access", requireApproval: true }]);
  assert.deepEqual(reducerContract.persisted, [{ accessMode: "full_access", requireApproval: true }]);

  const taskRow = page.locator(".sidebar-task-row").filter({ hasText: "修复任务队列并发问题" });
  await taskRow.locator(".sidebar-task").click();
  await page.locator("#main-content > .scene-room").waitFor({ state: "visible" });
  await page.locator(".timeline-subagent-chip").filter({ hasText: "Codex CLI · 检查并发边界" }).click();

  const workbench = page.getByTestId("workbench-panel");
  const permission = workbench.locator(".subagent-session-permission");
  await permission.waitFor({ state: "visible" });
  assert.equal(await permission.innerText(), "需审批");

  const summaryTab = workbench.getByRole("tab", { name: /^运行与子代理/ });
  await summaryTab.click();
  await workbench.getByRole("button", { name: /已结束子代理/ }).click();
  await workbench.locator(".subagent-list-row").filter({ hasText: "Codex CLI · 核对锁顺序" }).click();
  await permission.waitFor({ state: "visible" });
  assert.equal(await permission.innerText(), "完全访问");

  await summaryTab.click();
  await workbench.locator(".subagent-list-row").filter({ hasText: "Codex CLI · 只读复核" }).click();
  await permission.waitFor({ state: "visible" });
  assert.equal(await permission.innerText(), "只读");

  await page.close();
});

test("interrupted task toast counts down for five seconds and then releases the viewport", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const taskId = "toast-countdown-task";
  await page.evaluate(async (id) => {
    const { useTasksStore } = await import("/src/store/tasks.ts");
    const template = useTasksStore.getState().tasks[0];
    if (!template) throw new Error("browser mock must expose a task template");
    const active = {
      ...structuredClone(template),
      id,
      title: "倒计时测试",
      goal: "验证中止通知生命周期",
      state: "in_progress",
    };
    useTasksStore.setState((state) => ({ tasks: [...state.tasks, active] }));
    useTasksStore.setState((state) => ({
      tasks: state.tasks.map((task) => task.id === id ? { ...task, state: "interrupted" } : task),
    }));
  }, taskId);

  const toast = page.locator(".toast").filter({ hasText: "已中止：倒计时测试" });
  await toast.waitFor({ state: "visible" });
  assert.equal(await toast.locator(".toast-countdown").innerText(), "5s");

  const timeout = await page.evaluate(async (title) => {
    const { useToastStore } = await import("/src/store/toast.ts");
    return useToastStore.getState().toasts.find((item) => item.title === title)?.timeout;
  }, "已中止：倒计时测试");
  assert.equal(timeout, 5000, "an interrupted run is recoverable and must not inherit the permanent error timeout");

  await toast.hover();
  const pausedAt = await toast.locator(".toast-countdown").innerText();
  await page.waitForTimeout(1100);
  assert.equal(await toast.locator(".toast-countdown").innerText(), pausedAt, "hover must pause the countdown");

  await page.mouse.move(20, 20);
  await toast.waitFor({ state: "detached", timeout: 6500 });
  assert.equal(await page.locator(".toast").filter({ hasText: "已中止：倒计时测试" }).count(), 0);
  await page.close();
});

test("startup recovery opens the interrupted conversation directly without a redundant toast", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const target = await page.evaluate(async () => {
    const { browserMockRecovery } = await import("/src/lib/mock-data.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    const { useAppStore } = await import("/src/store/app.ts");
    const task = useTasksStore.getState().tasks.find((item) => item.state === "in_progress");
    if (!task) throw new Error("browser mock must expose a running recovery target");
    browserMockRecovery.interrupted_tasks.splice(0, browserMockRecovery.interrupted_tasks.length, task.id);
    browserMockRecovery.orphaned_permissions = 0;
    useAppStore.getState().openConversations();
    return { id: task.id, title: task.title };
  });

  await page.locator("#main-content > .scene-conversations").waitFor({ state: "visible" });
  await page.getByRole("button", { name: "R-Code，新建对话", exact: true }).click();
  const recoveryAction = page.getByRole("button", { name: "现在处理", exact: true });
  await recoveryAction.waitFor({ state: "visible" });
  await recoveryAction.click();

  await page.locator("#main-content > .scene-room").waitFor({ state: "visible" });
  const currentTaskId = await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    return useAppStore.getState().currentTaskId;
  });
  assert.equal(currentTaskId, target.id, "one recovery click must open the interrupted conversation");
  await page.waitForFunction(async (taskId) => {
    const { useTasksStore } = await import("/src/store/tasks.ts");
    return useTasksStore.getState().tasks.find((task) => task.id === taskId)?.state === "interrupted";
  }, target.id);
  assert.equal(
    await page.locator(".toast").filter({ hasText: `已中止：${target.title}` }).count(),
    0,
    "direct recovery navigation must not add another open-conversation card",
  );
  await page.close();
});

test("user workflow Skills are callable immediately and slash completion stays bounded", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const saved = await page.evaluate(async () => {
    const { workflowSkillSave } = await import("/src/lib/ipc.ts");
    return workflowSkillSave({
      name: "release-check",
      description: "检查发布边界、验证记录与 Git 交付状态；这段完整介绍应在悬停时可见。",
      instructions: "只检查当前任务的已归集路径，列出阻塞项并等待用户决定。",
      source: "custom",
      enabled: true,
    });
  });

  await page.getByRole("button", { name: "R-Code，新建对话", exact: true }).click();
  const composer = page.locator(".home-composer textarea");
  await composer.fill("/");

  const option = page.getByRole("option", { name: /release-check/ });
  await option.waitFor({ state: "visible", timeout: 5000 });
  const firstFour = await page.getByRole("option").evaluateAll((options) =>
    options.slice(0, 4).map((option) => option.textContent ?? ""),
  );
  assert.ok(
    firstFour.every((label) => /mcp-creator|skill-creator|review-changes|git-commit-push|release-check/.test(label)),
    `a bare slash should expose enabled Skills before static commands: ${JSON.stringify(firstFour)}`,
  );
  await option.hover();
  const detail = page.locator(".slash-menu-skill-detail");
  await detail.waitFor({ state: "visible" });
  assert.match(await detail.innerText(), /这段完整介绍应在悬停时可见/);

  const listStyle = await page.locator(".slash-menu-list").evaluate((element) => {
    const style = getComputedStyle(element);
    return {
      clientHeight: element.clientHeight,
      scrollHeight: element.scrollHeight,
      overflowY: style.overflowY,
    };
  });
  assert.ok(listStyle.clientHeight <= 224, `slash list exceeded its four-row viewport: ${JSON.stringify(listStyle)}`);
  assert.ok(listStyle.scrollHeight > listStyle.clientHeight, "the complete command catalog must remain scrollable");
  assert.equal(listStyle.overflowY, "auto");

  const invocation = await page.evaluate(async (skillId) => {
    const { workflowSkillsList, workflowSkillDelete } = await import("/src/lib/ipc.ts");
    const { parseSlashCommand, workflowPrompt } = await import("/src/lib/slash-commands.ts");
    const skills = await workflowSkillsList();
    const parsed = parseSlashCommand("/release-check 仅检查本次发布", skills);
    if (!parsed?.command) throw new Error("custom Skill was not callable");
    const prompt = workflowPrompt(parsed.command, parsed.args);
    await workflowSkillDelete(skillId);
    return { name: parsed.command.name, prompt };
  }, saved.id);
  assert.equal(invocation.name, "release-check");
  assert.match(invocation.prompt, /\[R-CODE-SKILL\]/);
  assert.match(invocation.prompt, /只检查当前任务的已归集路径/);
  assert.match(invocation.prompt, /仅检查本次发布/);

  await page.close();
});

test("review workbench exposes granular acceptance and guarded Git delivery", async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    const { browserMockDetails } = await import("/src/lib/mock-data.ts");
    globalThis.__rCodeBrowserMockExcludedReviewPaths = ["src/api.rs"];
    globalThis.__rCodeBrowserMockFailures = { cmd_review_git_status: "ignore boundary unavailable" };
    browserMockDetails["mock-task-review"].runs[0].ended_at = null;
    useTasksStore.getState().setCurrentProject("D:/project/rust/r-code");
    useAppStore.getState().openRoom("mock-task-review", "changes");
  });

  const workbench = page.getByTestId("workbench-panel");
  await workbench.waitFor({ state: "visible" });
  await workbench.getByRole("tab", { name: /变更/ }).waitFor({ state: "visible" });
  await workbench.getByText("审核范围暂不可用，请稍后重试。", { exact: true }).waitFor({ state: "visible" });
  assert.equal(await workbench.locator(".chg-row").count(), 0, "failed ignore checks must expose no actionable fallback rows");
  assert.equal(await workbench.getByRole("button", { name: "接受文件", exact: true }).count(), 0);
  await page.evaluate(() => {
    globalThis.__rCodeBrowserMockFailures = {};
    window.dispatchEvent(new Event("r-code:refresh-now"));
  });
  await workbench.getByRole("button", { name: "接受文件", exact: true }).waitFor({ state: "visible" });
  await workbench.getByRole("button", { name: "接受本轮全部", exact: true }).waitFor({ state: "visible" });
  await page.waitForFunction(
    (root) => root.querySelectorAll(".chg-row").length === 1,
    await workbench.elementHandle(),
  );
  assert.doesNotMatch(await workbench.locator(".changes-list").innerText(), /src\/api\.rs/, "paths omitted by Git status must not remain in the review list");

  await page.evaluate(() => {
    globalThis.__rCodeBrowserMockDelayMs = { cmd_review_accept_line: 350 };
  });
  const lineAccepts = workbench.locator("button.diff-line-accept");
  assert.ok(await lineAccepts.count() >= 2, "review fixture must expose multiple independently acceptable lines");
  const firstLineAccept = lineAccepts.nth(0);
  const secondLineAccept = lineAccepts.nth(1);
  await firstLineAccept.click();
  await page.waitForFunction((element) => element.disabled, await firstLineAccept.elementHandle());
  assert.equal(
    await secondLineAccept.isEnabled(),
    true,
    "accepting one line must not lock unrelated line decisions while its ledger write is pending",
  );
  await secondLineAccept.click();
  await page.waitForFunction(
    (root) => root.querySelectorAll("button.diff-line-accept").length >= 2
      && [...root.querySelectorAll("button.diff-line-accept")].filter((button) => button.textContent === "已接受").length >= 2,
    await workbench.elementHandle(),
  );
  await workbench.getByRole("button", { name: "接受文件", exact: true }).click();
  await page.waitForFunction(
    (root) => root.querySelector(".chg-accepted") !== null,
    await workbench.elementHandle(),
  );
  assert.equal(await workbench.getByRole("button", { name: "接受本轮全部", exact: true }).count(), 0, "bulk accept must disappear once every task path is resolved");
  assert.equal(await workbench.getByRole("button", { name: "拒绝并恢复本轮全部文件", exact: true }).count(), 0, "bulk reject must not stay available after acceptance");
  assert.equal(await workbench.locator(".chg-rb").count(), 0, "accepted rows must drop their reject action");
  assert.equal(await workbench.locator(".chg-accept").count(), 0, "accepted rows must drop their accept action");

  await workbench.getByRole("tab", { name: "验证与决策", exact: true }).click();
  const delivery = workbench.getByRole("region", { name: "Git 提交与推送" });
  await delivery.waitFor({ state: "visible" });
  assert.match(await delivery.innerText(), /codex\/demo/);
  assert.match(await delivery.innerText(), /origin\/codex\/demo/);

  await delivery.getByRole("button", { name: "暂存已接受文件", exact: true }).click();
  await delivery.getByText("1 个任务文件已暂存", { exact: false }).waitFor({ state: "visible" });

  await delivery.getByRole("button", { name: "自动生成", exact: true }).click();
  const message = delivery.getByPlaceholder("提交信息（可编辑）");
  await page.waitForFunction((element) => element.value.length > 0, await message.elementHandle());
  assert.equal(await message.inputValue(), "feat: update reviewed task files");

  const commit = delivery.getByRole("button", { name: "提交已暂存变更", exact: true });
  await commit.click();
  await delivery.getByRole("button", { name: "再次点击确认提交", exact: true }).click();
  await workbench.locator(".panel-note").filter({ hasText: "已提交 01234567" }).waitFor({ state: "visible" });

  await delivery.getByRole("button", { name: "推送到 upstream", exact: true }).click();
  await delivery.getByRole("button", { name: "5s 后可确认", exact: true }).waitFor({ state: "visible" });

  await page.close();
});

test("review rejection animates resolved rows away and exposes guarded bulk reject", async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    useTasksStore.getState().setCurrentProject("D:/project/rust/r-code");
    useAppStore.getState().openRoom("mock-task-review", "changes");
  });

  const workbench = page.getByTestId("workbench-panel");
  await workbench.waitFor({ state: "visible" });
  await page.waitForFunction(
    (root) => root.querySelectorAll(".chg-row").length === 2,
    await workbench.elementHandle(),
  );
  await workbench.getByRole("button", { name: "拒绝并恢复本轮全部文件", exact: true }).waitFor({ state: "visible" });

  const rejectedRow = workbench.locator(".chg-row").filter({ hasText: "src/error.rs" });
  await rejectedRow.getByRole("button", { name: "拒绝并恢复 src/error.rs", exact: true }).click();
  await rejectedRow.getByRole("button", { name: "再次确认，拒绝 src/error.rs", exact: true }).click();
  await page.waitForFunction(
    (root) => root.querySelector(".chg-row.is-exiting") !== null,
    await workbench.elementHandle(),
  );
  assert.match(
    await rejectedRow.evaluate((element) => getComputedStyle(element).animationName),
    /review-row-exit/,
  );
  await rejectedRow.waitFor({ state: "detached" });
  assert.equal(await workbench.locator(".chg-row").count(), 1, "a rejected file must not keep a placeholder row");

  await workbench.getByRole("button", { name: "拒绝并恢复本轮全部文件", exact: true }).click();
  await workbench.getByRole("button", { name: "再次确认，拒绝本轮全部文件", exact: true }).click();
  await page.waitForFunction(
    (root) => root.querySelectorAll(".chg-row").length === 0,
    await workbench.elementHandle(),
  );
  await workbench.getByText("工作区原有变更未受影响", { exact: false }).waitFor({ state: "visible" });

  await page.close();
});

test("bulk review actions disappear after accepting every task path", async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    useTasksStore.getState().setCurrentProject("D:/project/rust/r-code");
    useAppStore.getState().openRoom("mock-task-review", "changes");
  });

  const workbench = page.getByTestId("workbench-panel");
  await workbench.waitFor({ state: "visible" });
  await page.waitForFunction(
    (root) => root.querySelectorAll(".chg-row").length === 2,
    await workbench.elementHandle(),
  );
  await workbench.getByRole("button", { name: "接受本轮全部", exact: true }).click();
  await page.waitForFunction(
    (root) => root.querySelectorAll(".chg-row .chg-accepted").length === 2,
    await workbench.elementHandle(),
  );
  assert.equal(await workbench.getByRole("button", { name: "接受本轮全部", exact: true }).count(), 0, "the bulk accept button must leave after the round is accepted");
  assert.equal(await workbench.getByRole("button", { name: "拒绝并恢复本轮全部文件", exact: true }).count(), 0, "accepted rounds must not expose bulk reject anymore");
  assert.equal(await workbench.locator(".chg-rb").count(), 0, "accepted rows must not keep per-file reject actions");
  assert.equal(await workbench.locator(".chg-accept").count(), 0, "accepted rows must not keep per-file accept actions");

  await page.close();
});

test("Needs You groups projects and synchronizes granular review acceptance live", async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  await page.evaluate(async () => {
    const { browserMockDetails, browserMockTasks } = await import("/src/lib/mock-data.ts");
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    const apiTask = browserMockTasks.find((task) => task.id === "mock-task-api");
    if (!apiTask) throw new Error("missing cross-project mock task");
    apiTask.state = "review_ready";
    apiTask.updated_at = new Date().toISOString();
    browserMockDetails[apiTask.id].task = apiTask;
    browserMockDetails[apiTask.id].changes = [{
      id: "mock-task-api-change-1",
      task_id: apiTask.id,
      tool_call_id: null,
      path: "src/rate_limit.rs",
      change_type: "create",
      before_hash: null,
      after_hash: null,
      old_path: null,
      created_at: apiTask.updated_at,
    }];
    await useTasksStore.getState().refreshTasks();
    useAppStore.getState().setScene("inbox");
  });

  const inbox = page.locator(".main > .scene-inbox");
  await inbox.waitFor({ state: "visible" });
  await page.waitForFunction(() => document.querySelectorAll(".inbox-project-group").length === 2);
  assert.equal(await inbox.locator(".inbox-project-group").count(), 2);
  assert.match(await inbox.innerText(), /r-code/);
  assert.match(await inbox.innerText(), /api-server/);

  await inbox.locator('[data-task-id="mock-task-review"]').click();
  const acceptFile = inbox.getByRole("button", { name: "接受文件 src/error.rs", exact: true });
  await acceptFile.waitFor({ state: "visible" });
  await acceptFile.click();
  const reviewInspector = inbox.getByLabel("审核摘要", { exact: true });
  await reviewInspector.getByText("1 个文件待处理", { exact: true }).waitFor({ state: "visible" });
  assert.equal(await inbox.getByText("src/error.rs", { exact: true }).count(), 0);

  // Equivalent to accepting the remaining file from the task-local review workbench.
  const externallyAccepted = await page.evaluate(async () => {
    const { reviewAcceptAll, reviewGitStatus } = await import("/src/lib/ipc.ts");
    await reviewAcceptAll("mock-task-review");
    return reviewGitStatus("mock-task-review");
  });
  assert.equal(externallyAccepted.remaining_count, 0);
  await reviewInspector.getByText("审核项已全部处理", { exact: true }).first().waitFor({ state: "visible", timeout: 5000 });

  await inbox.getByRole("button", { name: "完成审核", exact: true }).click();
  await page.waitForFunction(() => !document.querySelector('[data-task-id="mock-task-review"]'));
  assert.equal(await inbox.locator('[data-task-id="mock-task-review"]').count(), 0);

  await page.close();
});

test("poll stores preserve references and coalesce concurrent list and detail reads", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const result = await page.evaluate(async () => {
    const taskId = "mock-task-complete";
    const {
      selectNeedsYou,
      selectNeedsYouTaskIds,
      selectPendingPermissions,
      selectReviewReady,
      selectRunning,
      useTasksStore,
    } = await import("/src/store/tasks.ts");
    await Promise.all([
      useTasksStore.getState().refreshTasks(),
      useTasksStore.getState().refreshWorkspaces(),
      useTasksStore.getState().refreshDetail(taskId),
    ]);
    const initial = useTasksStore.getState().details[taskId];
    const initialTasks = useTasksStore.getState().tasks;
    const initialWorkspaces = useTasksStore.getState().workspaces;
    const selectorState = useTasksStore.getState();
    const selectors = [
      selectRunning,
      selectReviewReady,
      selectPendingPermissions,
      selectNeedsYou,
      selectNeedsYouTaskIds,
    ];
    const derivedReferencesStable = selectors.every(
      (selector) => selector(selectorState) === selector(selectorState),
    );
    let detailCalls = 0;
    let taskListCalls = 0;
    let workspaceListCalls = 0;
    let referenceChanges = 0;
    globalThis.__rCodePerformanceIpcProbe = (name) => {
      if (name === "cmd_task_detail") detailCalls += 1;
      if (name === "cmd_task_list") taskListCalls += 1;
      if (name === "cmd_workspace_list") workspaceListCalls += 1;
    };
    globalThis.__rCodeBrowserMockDelayMs = {
      cmd_task_list: 40,
      cmd_workspace_list: 40,
      cmd_task_detail: 40,
    };
    const unsubscribe = useTasksStore.subscribe((state, previous) => {
      if (state.details[taskId] !== previous.details[taskId]) referenceChanges += 1;
    });

    await Promise.all([
      useTasksStore.getState().refreshTasks(),
      useTasksStore.getState().refreshTasks(),
      useTasksStore.getState().refreshTasks(),
      useTasksStore.getState().refreshWorkspaces(),
      useTasksStore.getState().refreshWorkspaces(),
      useTasksStore.getState().refreshWorkspaces(),
      useTasksStore.getState().refreshDetail(taskId),
      useTasksStore.getState().refreshDetail(taskId),
      useTasksStore.getState().refreshDetail(taskId),
    ]);
    const concurrentReferenceStable = useTasksStore.getState().details[taskId] === initial;
    const concurrentTaskReferenceStable = useTasksStore.getState().tasks === initialTasks;
    const concurrentWorkspaceReferenceStable = useTasksStore.getState().workspaces === initialWorkspaces;
    await Promise.all([
      useTasksStore.getState().refreshTasks(),
      useTasksStore.getState().refreshWorkspaces(),
      useTasksStore.getState().refreshDetail(taskId),
    ]);
    const sequentialReferenceStable = useTasksStore.getState().details[taskId] === initial;
    unsubscribe();
    delete globalThis.__rCodePerformanceIpcProbe;
    delete globalThis.__rCodeBrowserMockDelayMs;

    return {
      detailCalls,
      taskListCalls,
      workspaceListCalls,
      referenceChanges,
      concurrentReferenceStable,
      concurrentTaskReferenceStable,
      concurrentWorkspaceReferenceStable,
      derivedReferencesStable,
      sequentialReferenceStable,
    };
  });

  assert.equal(result.detailCalls, 2, "three concurrent reads should share one IPC, followed by one sequential poll");
  assert.equal(result.taskListCalls, 2, "three concurrent task-list refreshes should share one IPC");
  assert.equal(result.workspaceListCalls, 2, "three concurrent workspace-list refreshes should share one IPC");
  assert.equal(result.referenceChanges, 0, "equal JSON payloads must not replace the retained detail graph");
  assert.equal(result.concurrentReferenceStable, true);
  assert.equal(result.concurrentTaskReferenceStable, true);
  assert.equal(result.concurrentWorkspaceReferenceStable, true);
  assert.equal(result.derivedReferencesStable, true, "derived selectors must preserve references for immutable inputs");
  assert.equal(result.sequentialReferenceStable, true);
  await page.close();
});

test("opening a conversation coalesces shared history reads and reuses provider settings", async () => {
  const page = await browser.newPage({ viewport: { width: 1280, height: 840 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    globalThis.__rCodeSessionSwitchCalls = [];
    globalThis.__rCodePerformanceIpcProbe = (command, args) => {
      if ([
        "cmd_session_messages",
        "cmd_settings_get",
        "cmd_codex_integration_status",
        "cmd_codex_cli_preferences",
      ].includes(command)) {
        globalThis.__rCodeSessionSwitchCalls.push({ command, taskId: args?.taskId ?? null });
      }
    };
    globalThis.__rCodeBrowserMockDelayMs = {
      cmd_session_messages: 120,
      cmd_settings_get: 120,
      cmd_codex_integration_status: 120,
      cmd_codex_cli_preferences: 120,
    };

    await Promise.all([
      useTasksStore.getState().refreshTasks(),
      useTasksStore.getState().refreshDetail("mock-task-review"),
      useTasksStore.getState().refreshDetail("mock-task-queue"),
    ]);
    useAppStore.getState().openRoom("mock-task-review", "summary");
  });

  await page.locator('#main-content > .scene-room[data-owner-key="task:mock-task-review"]').waitFor({ state: "visible" });
  await page.waitForTimeout(250);
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().openRoom("mock-task-queue", "summary");
  });
  await page.locator('#main-content > .scene-room[data-owner-key="task:mock-task-queue"]').waitFor({ state: "visible" });
  await page.waitForTimeout(250);

  const result = await page.evaluate(() => {
    const recorded = [...globalThis.__rCodeSessionSwitchCalls];
    const visibleText = document.querySelector('#main-content > .scene-room[data-owner-key="task:mock-task-queue"]')?.textContent ?? "";
    delete globalThis.__rCodePerformanceIpcProbe;
    delete globalThis.__rCodeBrowserMockDelayMs;
    delete globalThis.__rCodeSessionSwitchCalls;
    return { recorded, visibleText };
  });
  const calls = result.recorded;
  assert.equal(
    calls.filter((call) => call.command === "cmd_session_messages" && call.taskId === "mock-task-review").length,
    1,
    "Timeline, Composer and Summary must share one JSONL projection read",
  );
  assert.equal(
    calls.filter((call) => call.command === "cmd_session_messages" && call.taskId === "mock-task-queue").length,
    1,
    "the next conversation must also read its projection only once",
  );
  assert.equal(
    calls.filter((call) => call.command === "cmd_settings_get").length,
    0,
    "switching conversations must reuse the global provider snapshot instead of rechecking credentials",
  );
  assert.equal(
    calls.filter((call) => call.command === "cmd_codex_integration_status").length,
    0,
    "switching conversations must reuse the application-level Codex readiness snapshot",
  );
  assert.equal(
    calls.filter((call) => call.command === "cmd_codex_cli_preferences").length,
    0,
    "switching conversations must reuse Codex CLI preferences instead of restarting model/login probes",
  );
  assert.doesNotMatch(
    result.visibleText,
    /正在读取模型服务|读取模型服务中|模型服务加载中/,
    "a cached provider snapshot must not put the switched conversation back into loading state",
  );
  await page.close();
});

test("provider mutations invalidate the shared provider snapshot once", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const calls = await page.evaluate(async () => {
    const { settingsSelectProvider } = await import("/src/lib/ipc.ts");
    const { useProviders } = await import("/src/lib/provider.ts");
    // The application has already hydrated the module-level snapshot during startup.
    globalThis.__rCodeProviderMutationCalls = [];
    globalThis.__rCodePerformanceIpcProbe = (command) => {
      if (command === "cmd_settings_get") globalThis.__rCodeProviderMutationCalls.push(command);
    };
    // Keep a hook consumer mounted through the real Room/Home shell; the IPC wrapper
    // announces the mutation and every subscriber must share the same forced reload.
    await settingsSelectProvider("deepseek");
    await new Promise((resolve) => setTimeout(resolve, 200));
    // Import is deliberately retained so tree-shaking cannot turn this into an IPC-only test.
    void useProviders;
    const recorded = [...globalThis.__rCodeProviderMutationCalls];
    delete globalThis.__rCodePerformanceIpcProbe;
    delete globalThis.__rCodeProviderMutationCalls;
    return recorded;
  });
  assert.equal(calls.length, 1, "a provider mutation must trigger one coalesced settings refresh");
  await page.close();
});

test("a provider mutation supersedes an in-flight stale settings snapshot", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const result = await page.evaluate(async () => {
    const { settingsGet, settingsSelectProvider } = await import("/src/lib/ipc.ts");
    await new Promise((resolve) => setTimeout(resolve, 300));
    globalThis.__rCodeProviderRaceCalls = [];
    globalThis.__rCodePerformanceIpcProbe = (command) => {
      if (command === "cmd_settings_get") globalThis.__rCodeProviderRaceCalls.push(command);
    };
    globalThis.__rCodeBrowserMockDelayMs = { cmd_settings_get: 160 };

    const stale = settingsGet();
    await new Promise((resolve) => setTimeout(resolve, 25));
    await settingsSelectProvider("deepseek");
    const fresh = settingsGet();
    await Promise.all([stale.catch(() => null), fresh]);
    await new Promise((resolve) => setTimeout(resolve, 250));

    const recorded = [...globalThis.__rCodeProviderRaceCalls];
    delete globalThis.__rCodePerformanceIpcProbe;
    delete globalThis.__rCodeBrowserMockDelayMs;
    delete globalThis.__rCodeProviderRaceCalls;
    return recorded;
  });
  assert.equal(
    result.length,
    2,
    "a mutation during an in-flight settings read must start exactly one fresh generation",
  );
  await page.close();
});

test("explicit configuration refreshes supersede in-flight application snapshots", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const calls = await page.evaluate(async () => {
    const { codexIntegrationStatus, codexStartLogin, settingsGet } = await import("/src/lib/ipc.ts");
    await new Promise((resolve) => setTimeout(resolve, 300));
    await codexStartLogin();
    globalThis.__rCodeExplicitRefreshCalls = [];
    globalThis.__rCodePerformanceIpcProbe = (command) => {
      if (command === "cmd_settings_get" || command === "cmd_codex_integration_status") {
        globalThis.__rCodeExplicitRefreshCalls.push(command);
      }
    };
    globalThis.__rCodeBrowserMockDelayMs = {
      cmd_settings_get: 160,
      cmd_codex_integration_status: 160,
    };

    const staleSettings = settingsGet();
    const staleCodex = codexIntegrationStatus();
    await new Promise((resolve) => setTimeout(resolve, 25));
    const freshSettings = settingsGet(true);
    const freshCodex = codexIntegrationStatus(true);
    const coalescedSettings = settingsGet(true);
    const coalescedCodex = codexIntegrationStatus(true);
    await Promise.all([
      staleSettings,
      staleCodex,
      freshSettings,
      freshCodex,
      coalescedSettings,
      coalescedCodex,
    ]);

    const recorded = [...globalThis.__rCodeExplicitRefreshCalls];
    delete globalThis.__rCodePerformanceIpcProbe;
    delete globalThis.__rCodeBrowserMockDelayMs;
    delete globalThis.__rCodeExplicitRefreshCalls;
    return recorded;
  });

  assert.equal(
    calls.filter((command) => command === "cmd_settings_get").length,
    2,
    "an explicit settings refresh must not reuse a request that started before the refresh boundary",
  );
  assert.equal(
    calls.filter((command) => command === "cmd_codex_integration_status").length,
    2,
    "an explicit Codex refresh must not reuse a probe that started before the refresh boundary",
  );
  await page.close();
});

test("Codex mutations supersede in-flight status and preference probes", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const result = await page.evaluate(async () => {
    const {
      codexCliPreferences,
      codexInstallCli,
      codexIntegrationStatus,
      codexSaveCliPreferences,
    } = await import("/src/lib/ipc.ts");

    globalThis.__rCodeBrowserMockDelayMs = {
      cmd_codex_integration_status: 160,
      cmd_codex_cli_preferences: 160,
    };
    const staleStatus = codexIntegrationStatus(true);
    const stalePreferences = codexCliPreferences();
    await new Promise((resolve) => setTimeout(resolve, 25));
    const install = codexInstallCli();
    const save = codexSaveCliPreferences("gpt-5.3-codex-spark", "high", "low", "full_access");
    const overlappingStatus = codexIntegrationStatus(true);
    const overlappingPreferences = codexCliPreferences();
    const [installed, saved] = await Promise.all([install, save]);
    await Promise.all([staleStatus, stalePreferences, overlappingStatus, overlappingPreferences]);

    const cachedStatus = await codexIntegrationStatus();
    const cachedPreferences = await codexCliPreferences();
    delete globalThis.__rCodeBrowserMockDelayMs;
    return { installed, saved, cachedStatus, cachedPreferences };
  });

  assert.equal(result.installed.cli_available, true);
  assert.equal(result.cachedStatus.cli_available, true, "an old CLI probe must not replace install state");
  assert.equal(result.saved.model, "gpt-5.3-codex-spark");
  assert.equal(
    result.cachedPreferences.model,
    "gpt-5.3-codex-spark",
    "an old preference probe must not replace a saved model",
  );
  await page.close();
});

test("all mounted provider consumers refresh from one settings request", async () => {
  const page = await browser.newPage({ viewport: { width: 1280, height: 840 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    await useTasksStore.getState().refreshDetail("mock-task-review");
    useAppStore.getState().openRoom("mock-task-review", "summary");
  });
  await page.locator('#main-content > .scene-room[data-owner-key="task:mock-task-review"]').waitFor({ state: "visible" });
  const result = await page.evaluate(async () => {
    const { settingsSelectProvider } = await import("/src/lib/ipc.ts");
    globalThis.__rCodeMultiProviderCalls = [];
    globalThis.__rCodePerformanceIpcProbe = (command) => {
      if (command === "cmd_settings_get") globalThis.__rCodeMultiProviderCalls.push(command);
    };
    await settingsSelectProvider("deepseek");
    await new Promise((resolve) => setTimeout(resolve, 250));
    const recorded = [...globalThis.__rCodeMultiProviderCalls];
    delete globalThis.__rCodePerformanceIpcProbe;
    delete globalThis.__rCodeMultiProviderCalls;
    return recorded;
  });
  assert.equal(
    result.length,
    1,
    "Home and Room provider hooks must each update local state while sharing one settings IPC",
  );
  await page.close();
});

test("poll hooks share one live refresh listener across the WebView", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.addInitScript(() => {
    const refreshListeners = new Set();
    const addEventListener = window.addEventListener.bind(window);
    const removeEventListener = window.removeEventListener.bind(window);
    window.addEventListener = (type, listener, options) => {
      if (type === "r-code:refresh-now") refreshListeners.add(listener);
      addEventListener(type, listener, options);
    };
    window.removeEventListener = (type, listener, options) => {
      if (type === "r-code:refresh-now") refreshListeners.delete(listener);
      removeEventListener(type, listener, options);
    };
    globalThis.__rCodeRefreshListenerCount = () => refreshListeners.size;
  });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const listenerCount = await page.evaluate(() => globalThis.__rCodeRefreshListenerCount());
  assert.equal(
    listenerCount,
    2,
    "the app startup refresher and shared poll scheduler should be the only live refresh listeners",
  );
  await page.close();
});

test("poll failures expose stale data and clear after a successful retry", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const { reportSyncFailure } = await import("/src/store/sync-health.ts");
    reportSyncFailure("startup-tasks", "会话列表", new Error("simulated offline"));
  });

  const warning = page.getByRole("alert").filter({ hasText: "数据可能已过期" });
  await warning.waitFor({ state: "visible" });
  assert.match(await warning.textContent(), /会话列表|后台数据/);

  await warning.getByRole("button", { name: "重试" }).click();
  await warning.waitFor({ state: "detached" });
  await page.close();
});

test("terminal input coalesces queued keystrokes without reordering them", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const chunks = await page.evaluate(async () => {
    const { createTerminalInputBuffer } = await import("/src/components/room/Canvas.tsx");
    const sent = [];
    let releaseFirst;
    const firstSend = new Promise((resolve) => {
      releaseFirst = resolve;
    });
    const input = createTerminalInputBuffer(async (chunk) => {
      sent.push(chunk);
      if (sent.length === 1) await firstSend;
    }, () => {});

    input.push("a");
    await new Promise((resolve) => setTimeout(resolve, 12));
    input.push("b");
    input.push("c");
    input.push("d");
    releaseFirst();
    await input.flush();
    input.dispose();
    return sent;
  });

  assert.deepEqual(chunks, ["a", "bcd"]);
  await page.close();
});

test("terminal IPC isolates terminals between conversations", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const result = await page.evaluate(async () => {
    const {
      terminalCreate,
      terminalKill,
      terminalList,
      terminalRawSnapshot,
      terminalSend,
    } = await import("/src/lib/ipc.ts");
    const firstTaskId = "mock-task-review";
    const secondTaskId = "mock-task-complete";
    const firstTerminalId = await terminalCreate(firstTaskId, "PowerShell");
    const secondTerminalId = await terminalCreate(secondTaskId, "PowerShell");
    const firstIds = (await terminalList(firstTaskId)).map((terminal) => terminal.id);
    const secondIds = (await terminalList(secondTaskId)).map((terminal) => terminal.id);
    let crossReadRejected = false;
    let crossSendRejected = false;
    let crossKillRejected = false;
    try {
      await terminalRawSnapshot(secondTaskId, firstTerminalId);
    } catch {
      crossReadRejected = true;
    }
    try {
      await terminalSend(secondTaskId, firstTerminalId, "echo crossed", true);
    } catch {
      crossSendRejected = true;
    }
    try {
      await terminalKill(secondTaskId, firstTerminalId);
    } catch {
      crossKillRejected = true;
    }
    await terminalKill(firstTaskId, firstTerminalId);
    const firstIdsAfterKill = (await terminalList(firstTaskId)).map((terminal) => terminal.id);
    const secondIdsAfterFirstKill = (await terminalList(secondTaskId)).map((terminal) => terminal.id);
    await terminalKill(secondTaskId, secondTerminalId);
    return {
      firstIds,
      secondIds,
      firstIdsAfterKill,
      secondIdsAfterFirstKill,
      firstTerminalId,
      secondTerminalId,
      crossReadRejected,
      crossSendRejected,
      crossKillRejected,
    };
  });

  assert.deepEqual(result.firstIds, [result.firstTerminalId]);
  assert.deepEqual(result.secondIds, [result.secondTerminalId]);
  assert.deepEqual(result.firstIdsAfterKill, []);
  assert.deepEqual(result.secondIdsAfterFirstKill, [result.secondTerminalId]);
  assert.equal(result.crossReadRejected, true);
  assert.equal(result.crossSendRejected, true);
  assert.equal(result.crossKillRejected, true);
  await page.close();
});

test("terminal panel switches with its conversation and restores each terminal", async () => {
  const page = await browser.newPage({ viewport: { width: 1280, height: 840 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const firstTaskId = "mock-task-review";
  const secondTaskId = "mock-task-complete";

  const ids = await page.evaluate(async ({ firstTaskId, secondTaskId }) => {
    const { terminalCreate } = await import("/src/lib/ipc.ts");
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    const [firstTerminalId, secondTerminalId] = await Promise.all([
      terminalCreate(firstTaskId, "First Session Shell"),
      terminalCreate(secondTaskId, "Second Session Shell"),
    ]);
    await Promise.all([
      useTasksStore.getState().refreshDetail(firstTaskId),
      useTasksStore.getState().refreshDetail(secondTaskId),
    ]);
    useAppStore.getState().openRoom(firstTaskId, "terminal");
    return { firstTerminalId, secondTerminalId };
  }, { firstTaskId, secondTaskId });

  const terminalList = page.getByRole("grid", { name: "终端列表" });
  await terminalList.getByText("First Session Shell", { exact: true }).waitFor({ state: "visible" });
  assert.equal(await terminalList.getByText("Second Session Shell", { exact: true }).count(), 0);

  await page.evaluate((taskId) => import("/src/store/app.ts").then(
    ({ useAppStore }) => useAppStore.getState().openRoom(taskId, "terminal"),
  ), secondTaskId);
  await terminalList.getByText("Second Session Shell", { exact: true }).waitFor({ state: "visible" });
  assert.equal(await terminalList.getByText("First Session Shell", { exact: true }).count(), 0);

  await page.evaluate((taskId) => import("/src/store/app.ts").then(
    ({ useAppStore }) => useAppStore.getState().openRoom(taskId, "terminal"),
  ), firstTaskId);
  await terminalList.getByText("First Session Shell", { exact: true }).waitFor({ state: "visible" });
  assert.equal(await terminalList.getByText("Second Session Shell", { exact: true }).count(), 0);

  await page.evaluate(async ({ firstTaskId, secondTaskId, ids }) => {
    const { terminalKill } = await import("/src/lib/ipc.ts");
    await terminalKill(firstTaskId, ids.firstTerminalId);
    await terminalKill(secondTaskId, ids.secondTerminalId);
  }, { firstTaskId, secondTaskId, ids });
  await page.close();
});
