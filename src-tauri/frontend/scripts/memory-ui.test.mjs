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
      .flatMap((entry) => [
        path.join(cache, entry, "chrome-win64", "chrome.exe"),
        path.join(cache, entry, "chrome-linux", "chrome"),
        path.join(cache, entry, "chrome-mac", "Chromium.app", "Contents", "MacOS", "Chromium"),
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

test("manual review UI and IPC expose only an explicit memory scope", () => {
  const panel = fs.readFileSync(path.join(frontendDir, "src/components/scenes/MemoryPanel.tsx"), "utf8");
  const ipc = fs.readFileSync(path.join(frontendDir, "src/lib/ipc.ts"), "utf8");
  const panelReviewStart = panel.indexOf("const reviewLatest");
  const panelReview = panel.slice(panelReviewStart, panel.indexOf("const providerNames", panelReviewStart));
  const ipcReview = ipc.slice(ipc.indexOf("export interface MemoryReviewScopeRequest"), ipc.indexOf("export const memoryRetryJob"));

  assert.doesNotMatch(panel, /\blatestTask\b/);
  assert.doesNotMatch(panelReview, /\btaskId\b/);
  assert.match(panelReview, /workspaceId:\s*workspace\?\.id\s*\?\?\s*null/);
  assert.match(panelReview, /workspacePath:\s*workspace\?\.canonical_path\s*\?\?\s*null/);
  assert.doesNotMatch(ipcReview, /\btaskId\b/);
  assert.match(ipcReview, /workspaceId:\s*scope\.workspaceId/);
  assert.match(ipcReview, /workspacePath:\s*scope\.workspacePath/);
});

test("memory makes its privacy default, reviewer choice, and enable action self-evident", async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const { memoryClearAll } = await import("/src/lib/ipc.ts");
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    await memoryClearAll();
    useTasksStore.getState().setCurrentProject(null);
    useAppStore.getState().openKnowledge("memory");
  });

  const panel = page.locator(".knowledge-memory-panel");
  await panel.waitFor({ state: "visible" });
  await panel.getByText("记忆已关闭", { exact: true }).waitFor({ state: "visible" });
  await panel.getByText(/默认关闭/).waitFor({ state: "visible" });

  const provider = panel.getByLabel("模型服务", { exact: true });
  const model = panel.getByLabel("复盘模型", { exact: true });
  assert.equal(await provider.inputValue(), "openai");
  assert.equal(await model.inputValue(), "gpt-5.6-sol", "default provider model must not be cleared after settings load");
  assert.equal(await page.locator(".knowledge-tabs button span").count(), 0, "tabs should not repeat explanatory copy");

  if (process.env.R_CODE_MEMORY_SHOT_BEFORE) {
    await page.screenshot({ path: process.env.R_CODE_MEMORY_SHOT_BEFORE, fullPage: true });
  }

  await panel.getByRole("button", { name: "启用记忆", exact: true }).click();
  await panel.getByText("记忆已开启", { exact: true }).waitFor({ state: "visible" });
  const settings = await page.evaluate(async () => (await import("/src/lib/ipc.ts")).memoryOverview());
  assert.equal(settings.settings.enabled, true);
  assert.deepEqual(settings.settings.reviewer, { provider_name: "openai", model: "gpt-5.6-sol" });

  if (process.env.R_CODE_MEMORY_SHOT) {
    await page.screenshot({ path: process.env.R_CODE_MEMORY_SHOT, fullPage: true });
  }
  await page.close();
});

test("browser memory review scope matches the native IPC contract", async () => {
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const outcomes = await page.evaluate(async () => {
    const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
    const { browserMockWorkspaces } = await import("/src/lib/mock-data.ts");
    const workspace = browserMockWorkspaces[0];
    const overview = await browserMockInvoke("cmd_memory_overview");
    await browserMockInvoke("cmd_memory_update_settings", {
      update: {
        expected_version: overview.settings.version,
        enabled: true,
        reviewer: { provider_name: "openai", model: "gpt-5.6-sol" },
        trigger_every_turns: overview.settings.trigger_every_turns,
        explicit_remember_immediate: overview.settings.explicit_remember_immediate,
        project_notification_mode: overview.settings.project_notification_mode,
      },
    });
    const invoke = async (args) => {
      try {
        await browserMockInvoke("cmd_memory_review_now", args);
        return "accepted";
      } catch {
        return "rejected";
      }
    };
    return {
      partialId: await invoke({ workspaceId: workspace.id, workspacePath: null }),
      partialPath: await invoke({ workspaceId: null, workspacePath: workspace.canonical_path }),
      mismatched: await invoke({ workspaceId: workspace.id, workspacePath: "D:/wrong/project" }),
      blank: await invoke({ workspaceId: "", workspacePath: "" }),
      whitespace: await invoke({ workspaceId: "  ", workspacePath: "\t" }),
      nonString: await invoke({ workspaceId: 42, workspacePath: workspace.canonical_path }),
      missing: await invoke({}),
      global: await invoke({ workspaceId: null, workspacePath: null }),
      project: await invoke({ workspaceId: workspace.id, workspacePath: workspace.canonical_path }),
    };
  });
  assert.deepEqual(outcomes, {
    partialId: "rejected",
    partialPath: "rejected",
    mismatched: "rejected",
    blank: "rejected",
    whitespace: "rejected",
    nonString: "rejected",
    missing: "accepted",
    global: "accepted",
    project: "accepted",
  });
  await page.close();
});

test("manual review reports an empty backlog honestly and exposes queued work immediately", async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const { memoryClearAll, memoryOverview, memoryUpdateSettings } = await import("/src/lib/ipc.ts");
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    await memoryClearAll();
    const overview = await memoryOverview();
    await memoryUpdateSettings({
      expected_version: overview.settings.version,
      enabled: true,
      reviewer: { provider_name: "openai", model: "gpt-5.6-sol" },
      trigger_every_turns: overview.settings.trigger_every_turns,
      explicit_remember_immediate: overview.settings.explicit_remember_immediate,
      project_notification_mode: overview.settings.project_notification_mode,
    });
    globalThis.__rCodeBrowserMockMemoryReviewResult = null;
    globalThis.__rCodeMemoryReviewScopes = [];
    globalThis.__rCodePerformanceIpcProbe = (command, args) => {
      if (command === "cmd_memory_review_now") {
        globalThis.__rCodeMemoryReviewScopes.push({
          workspaceId: args.workspaceId,
          workspacePath: args.workspacePath,
        });
      }
    };
    useTasksStore.getState().setCurrentProject("D:/project/rust/r-code");
    useAppStore.getState().openKnowledge("memory");
  });

  try {
    const panel = page.locator(".knowledge-memory-panel");
    const review = panel.getByRole("button", { name: "立即复盘", exact: true });
    await review.waitFor({ state: "visible" });
    await review.click();
    await panel.getByText("最近完成的会话没有新的可复盘内容", { exact: true }).waitFor({ state: "visible" });
    assert.equal(await panel.getByText("已提交一次手动复盘", { exact: true }).count(), 0);
    assert.equal(await panel.getByRole("heading", { name: "最近复盘", exact: true }).count(), 0);
    await panel.getByText("最近完成的会话没有新的可复盘内容", { exact: true })
      .waitFor({ state: "hidden", timeout: 4_500 });

    await page.evaluate(() => delete globalThis.__rCodeBrowserMockMemoryReviewResult);
    await review.click();
    await panel.getByText("已加入复盘队列，可在下方查看进度", { exact: true }).waitFor({ state: "visible" });
    await panel.getByRole("heading", { name: "最近复盘", exact: true }).waitFor({ state: "visible" });
    await panel.getByText("排队中", { exact: true }).waitFor({ state: "visible" });
    const scopeState = await page.evaluate(async () => {
      const { browserMockWorkspaces } = await import("/src/lib/mock-data.ts");
      const workspace = browserMockWorkspaces.find((item) => item.canonical_path === "D:/project/rust/r-code");
      return { scopes: globalThis.__rCodeMemoryReviewScopes, workspace };
    });
    assert.deepEqual(scopeState.scopes, [
      { workspaceId: scopeState.workspace.id, workspacePath: scopeState.workspace.canonical_path },
      { workspaceId: scopeState.workspace.id, workspacePath: scopeState.workspace.canonical_path },
    ]);

    await page.evaluate(() => {
      globalThis.__rCodeBrowserMockFailures = {
        cmd_memory_review_now: "internal database detail must stay out of the UI",
      };
    });
    await review.click();
    const safeFailure = panel.getByText("暂时无法提交复盘，请稍后重试", { exact: true });
    await safeFailure.waitFor({ state: "visible" });
    assert.equal(await panel.getByText("internal database detail must stay out of the UI", { exact: true }).count(), 0);
    await safeFailure.waitFor({ state: "hidden", timeout: 4_500 });
  } finally {
    await page.evaluate(() => {
      delete globalThis.__rCodeBrowserMockMemoryReviewResult;
      delete globalThis.__rCodeMemoryReviewScopes;
      delete globalThis.__rCodePerformanceIpcProbe;
      delete globalThis.__rCodeBrowserMockFailures;
    }).catch(() => {});
    await page.close();
  }
});

test("project overview opens memory management in the current project scope", async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    const workspacePath = "D:/project/rust/r-code";
    useTasksStore.getState().setCurrentProject(workspacePath);
    await useTasksStore.getState().refreshDashboard(workspacePath);
    useAppStore.getState().openDashboard(workspacePath);
  });

  await page.getByRole("button", { name: "项目记忆", exact: true }).click();
  const knowledge = page.getByRole("region", { name: "知识与指令" });
  await knowledge.waitFor({ state: "visible" });
  await knowledge.getByRole("heading", { name: "r-code 的项目记忆", exact: true }).waitFor({ state: "visible" });
  await knowledge.getByText("这里管理项目专属记忆；启用时同时自动继承全局记忆。", { exact: true }).waitFor({ state: "visible" });
  assert.equal(
    await knowledge.getByRole("button", { name: "r-code", exact: true }).getAttribute("aria-pressed"),
    "true",
  );

  await page.close();
});
