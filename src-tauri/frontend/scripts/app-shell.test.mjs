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
    };
  });
  assert.equal(chrome.macClass, true);
  assert.equal(chrome.customControls, 0);
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

test("sidebar uses green for active main agents and orange for finished tasks", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const colors = await page.evaluate(() => {
    const dotFor = (title) => {
      const rows = [...document.querySelectorAll(".sidebar-task-row")];
      const row = rows.find((candidate) => candidate.textContent?.includes(title));
      const dot = row?.querySelector(".task-state-dot");
      if (!(dot instanceof HTMLElement)) throw new Error(`missing sidebar state dot: ${title}`);
      return getComputedStyle(dot).backgroundColor;
    };
    return {
      running: dotFor("修复任务队列并发问题"),
      waitingWhileRunning: dotFor("优化 Rust 编译性能"),
      reviewReady: dotFor("统一错误处理规范"),
      finished: dotFor("更新依赖并修复告警"),
    };
  });

  assert.equal(colors.waitingWhileRunning, colors.running);
  assert.equal(colors.finished, colors.reviewReady);
  assert.notEqual(colors.running, colors.finished);
  await page.close();
});

test("Codex one-click setup resumes automatically after browser login", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().setSettingsPane("codex");
    useAppStore.getState().setScene("settings");
  });

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
    const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
    const { useAppStore } = await import("/src/store/app.ts");
    await browserMockInvoke("cmd_codex_install_cli");
    await browserMockInvoke("cmd_codex_start_login");
    await browserMockInvoke("cmd_codex_setup_collaboration");
    useAppStore.getState().setSettingsPane("codex");
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

test("Ctrl+= zoom keeps the app shell covering the complete webview", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  await page.keyboard.press("Control+=");
  await page.waitForFunction(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    return useAppStore.getState().zoomLevel === 110;
  });

  const layout = await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    const app = document.querySelector("#app");
    if (!(app instanceof HTMLElement)) throw new Error("#app is missing");
    const rect = app.getBoundingClientRect();
    return {
      zoomLevel: useAppStore.getState().zoomLevel,
      rect: { left: rect.left, top: rect.top, right: rect.right, bottom: rect.bottom },
      viewport: { width: innerWidth, height: innerHeight },
      inlineSize: { width: app.style.width, height: app.style.height, zoom: app.style.zoom },
    };
  });

  assert.equal(layout.zoomLevel, 110);
  assert.equal(layout.inlineSize.width, "", "CSS zoom already compensates an auto block width");
  assert.equal(layout.inlineSize.zoom, "1.1");
  assert.match(layout.inlineSize.height, /vh$/, "the explicit viewport height needs inverse vh compensation");
  assert.ok(Math.abs(layout.rect.left) <= 1 && Math.abs(layout.rect.top) <= 1, "the zoomed shell must stay anchored to the webview origin");
  assert.ok(
    Math.abs(layout.rect.right - layout.viewport.width) <= 1,
    `zoom must not leave an uncovered strip on the right: ${JSON.stringify(layout)}`,
  );
  assert.ok(
    Math.abs(layout.rect.bottom - layout.viewport.height) <= 1,
    `zoom must not leave an uncovered strip at the bottom: ${JSON.stringify(layout)}`,
  );

  for (const level of [80, 200, 100]) {
    await page.evaluate(async (nextLevel) => {
      const { useAppStore } = await import("/src/store/app.ts");
      useAppStore.getState().setZoom(nextLevel);
    }, level);
    await page.waitForFunction(async (nextLevel) => {
      const { useAppStore } = await import("/src/store/app.ts");
      return useAppStore.getState().zoomLevel === nextLevel;
    }, level);
    const bounds = await page.locator("#app").evaluate((app) => {
      const rect = app.getBoundingClientRect();
      return { right: rect.right, bottom: rect.bottom, width: innerWidth, height: innerHeight };
    });
    assert.ok(Math.abs(bounds.right - bounds.width) <= 1, `${level}% zoom must cover the webview width`);
    assert.ok(Math.abs(bounds.bottom - bounds.height) <= 1, `${level}% zoom must cover the webview height`);
  }

  await page.close();
});

test("knowledge and instructions replaces the standalone project-files module", async () => {
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  assert.equal(await page.locator(".sidebar-nav-item").filter({ hasText: "项目文件" }).count(), 0);
  await page.locator(".sidebar-nav-item").filter({ hasText: "知识与指令" }).click();
  const center = page.getByRole("region", { name: "知识与指令" });
  await center.waitFor({ state: "visible" });
  await center.getByRole("tab", { name: "记忆", exact: true }).waitFor({ state: "visible" });
  await center.getByRole("tab", { name: "协作 Prompt", exact: true }).waitFor({ state: "visible" });
  await center.getByRole("tab", { name: "Skills", exact: true }).waitFor({ state: "visible" });
  await center.getByRole("button", { name: "全局", exact: true }).waitFor({ state: "visible" });
  await center.getByRole("button", { name: "r-code", exact: true }).waitFor({ state: "visible" });
  await center.getByRole("tab", { name: "Skills", exact: true }).click();
  await center.getByRole("heading", { name: "工作流 Skills", exact: true }).waitFor({ state: "visible" });
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

  await page.locator(".sidebar-nav-item").filter({ hasText: "知识与指令" }).click();
  await page.locator("#main-content > .scene-knowledge").waitFor({ state: "visible" });
  await page.locator(".sidebar-task-row").filter({ hasText: "修复任务队列并发问题" }).locator(".sidebar-task").click();
  await page.locator("#main-content > .scene-room").waitFor({ state: "visible" });
  assert.equal(await composer.inputValue(), "", "an acknowledged reference must not replay after Composer remount");

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

    await page.locator(".sidebar-nav-item").filter({ hasText: "知识与指令" }).click();
    const center = page.getByRole("region", { name: "知识与指令" });
    await center.waitFor({ state: "visible" });
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
    assert.deepEqual(await page.evaluate(() => globalThis.__rCodeSendModes), ["queue"]);
    await page.evaluate(() => globalThis.__rCodeReleaseSend?.());
    await page.waitForFunction(() => !document.querySelector(".run-send-mode-trigger")?.hasAttribute("disabled"));

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
  } finally {
    await page.evaluate(() => {
      globalThis.__rCodeReleaseSend?.();
      delete globalThis.__rCodeReleaseSend;
      delete globalThis.__rCodeSendModes;
      delete globalThis.__rCodeFailNextSend;
      delete globalThis.__TAURI_INTERNALS__;
    }).catch(() => {});
    await page.evaluate(async ({ id, original }) => {
      const {
        browserMockDetails,
        browserMockSetMessages,
        browserMockTasks,
      } = await import("/src/lib/mock-data.ts");
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
    assert.equal(await page.getByRole("textbox", { name: "主 Agent 协作 Prompt" }).count(), 0, "prompts must no longer be split across Settings");

    await page.locator(".sidebar-nav-item").filter({ hasText: "知识与指令" }).click();
    const center = page.getByRole("region", { name: "知识与指令" });
    await center.getByRole("tab", { name: "协作 Prompt", exact: true }).click();
    const mainPrompt = page.getByRole("textbox", { name: "主 Agent 协作 Prompt" });
    const childPrompt = page.getByRole("textbox", { name: "子代理协作 Prompt" });
    await mainPrompt.fill("主代理负责统筹，只有必要时才委派。");
    await childPrompt.fill("子代理只完成边界清晰的子任务并返回摘要。");
    await page.getByRole("button", { name: "保存并应用 Prompt" }).click();
    await page.getByText("协作 Prompt 已保存并应用", { exact: true }).waitFor({ state: "visible" });

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

test("subagents open in deduplicated tabs while the overview stays available", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const taskRow = page.locator(".sidebar-task-row").filter({ hasText: "修复任务队列并发问题" });
  await taskRow.locator(".sidebar-task").click();
  await page.locator("#main-content > .scene-room").waitFor({ state: "visible" });

  const timelineSubagent = page.locator(".timeline-subagent-chip").filter({ hasText: "Codex CLI · 检查并发边界" });
  await timelineSubagent.waitFor({ state: "visible" });
  await timelineSubagent.click();

  const tablist = page.getByRole("tablist", { name: "子智能体工作台" });
  const tabs = tablist.getByRole("tab");
  const activeSubagent = page.locator(".subagent-list-row").filter({ hasText: "Codex CLI · 检查并发边界" });
  const completedSubagent = page.locator(".subagent-list-row").filter({ hasText: "Codex CLI · 核对锁顺序" });

  await page.getByTestId("subagent-detail").waitFor({ state: "visible" });
  assert.equal(await tabs.count(), 2, "opening a subagent must preserve the overview tab");

  await tablist.getByRole("tab", { name: "子智能体", exact: true }).click();
  await page.getByTestId("subagent-list").waitFor({ state: "visible" });
  await activeSubagent.click();
  assert.equal(await tabs.count(), 2, "opening the same subagent must activate its existing tab");
  assert.equal(
    await tablist.getByRole("tab", { name: "Codex CLI · 检查并发边界", exact: true }).getAttribute("aria-selected"),
    "true",
  );

  await tablist.getByRole("tab", { name: "子智能体", exact: true }).click();
  await completedSubagent.click();
  assert.equal(await tabs.count(), 3, "a different subagent gets its own tab");
  assert.equal(
    await tablist.getByRole("tab", { name: "Codex CLI · 核对锁顺序", exact: true }).getAttribute("aria-selected"),
    "true",
  );

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

  await page.getByRole("button", { name: "新对话", exact: true }).click();
  const composer = page.locator(".home-composer textarea");
  await composer.fill("/");

  const option = page.getByRole("option", { name: /release-check/ });
  await option.waitFor({ state: "visible", timeout: 5000 });
  const firstFour = await page.getByRole("option").evaluateAll((options) =>
    options.slice(0, 4).map((option) => option.textContent ?? ""),
  );
  assert.ok(
    firstFour.every((label) => /skill-creator|review-changes|git-commit-push|release-check/.test(label)),
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
    globalThis.__rCodeBrowserMockExcludedReviewPaths = ["src/api.rs"];
    useTasksStore.getState().setCurrentProject("D:/project/rust/r-code");
    useAppStore.getState().openRoom("mock-task-review", "changes");
  });

  const workbench = page.getByTestId("workbench-panel");
  await workbench.waitFor({ state: "visible" });
  await workbench.getByRole("tab", { name: /变更/ }).waitFor({ state: "visible" });
  await workbench.getByRole("button", { name: "接受文件", exact: true }).waitFor({ state: "visible" });
  await workbench.getByRole("button", { name: "接受全部", exact: true }).waitFor({ state: "visible" });
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
