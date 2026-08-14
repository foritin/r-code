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

async function openTaskWithAssistantText(page, text) {
  const taskId = "mock-task-complete";
  await page.evaluate(async ({ id, assistantText }) => {
    const { invalidateSessionMessages } = await import("/src/lib/ipc.ts");
    const { browserMockSetMessages } = await import("/src/lib/mock-data.ts");
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    browserMockSetMessages(id, [{
      id: `${id}-long-assistant`,
      branch_id: "main",
      kind: "message",
      role: "assistant",
      text: assistantText,
      timestamp: new Date().toISOString(),
    }]);
    invalidateSessionMessages(id);
    useTasksStore.getState().setCurrentProject("D:/project/rust/r-code");
    await useTasksStore.getState().refreshDetail(id);
    useAppStore.getState().openRoom(id);
  }, { id: taskId, assistantText: text });
  await page.locator("#main-content > .scene-room").waitFor({ state: "visible" });
  return page.locator(".timeline .agent .md").first();
}

test("500 Markdown blocks stay layout-contained with a linear DOM budget", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const paragraphCount = 500;
  const markdown = Array.from(
    { length: paragraphCount },
    (_, index) => `性能段落 ${String(index + 1).padStart(4, "0")} · LONG_MARKDOWN_SENTINEL`,
  ).join("\n\n");

  try {
    const rendered = await openTaskWithAssistantText(page, markdown);
    const blocks = rendered.locator(":scope > .md-block");
    await blocks.nth(paragraphCount - 1).waitFor({ state: "attached" });
    assert.equal(await blocks.count(), paragraphCount);

    const containment = await blocks.evaluateAll((items) => ({
      contentVisibility: [...new Set(items.map((item) => getComputedStyle(item).contentVisibility))],
      intrinsicSizes: [...new Set(items.map((item) => getComputedStyle(item).containIntrinsicSize))],
    }));
    assert.deepEqual(containment.contentVisibility, ["auto"]);
    assert.ok(
      containment.intrinsicSizes.every((value) => value.includes("42px")),
      `every Markdown block needs a 42px intrinsic placeholder, got ${containment.intrinsicSizes.join(", ")}`,
    );

    const descendantCount = await rendered.locator("*").count();
    assert.ok(
      descendantCount <= paragraphCount * 2 + 20,
      `500 plain paragraphs should stay close to two DOM nodes each, got ${descendantCount}`,
    );
  } finally {
    await page.close();
  }
});

test("a collapsed 1000-line code block mounts only its 16-line preview", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const lineCount = 1000;
  const lines = Array.from(
    { length: lineCount },
    (_, index) => `const long_code_line_${String(index + 1).padStart(4, "0")} = ${index + 1};`,
  );
  const markdown = `\`\`\`ts\n${lines.join("\n")}\n\`\`\``;

  try {
    const rendered = await openTaskWithAssistantText(page, markdown);
    const code = rendered.locator(".md-code code");
    await code.waitFor({ state: "attached" });
    const collapsedText = await code.textContent();
    const collapsedLines = collapsedText?.split("\n") ?? [];
    assert.equal(collapsedLines.length, 16);
    assert.ok(collapsedText?.includes("long_code_line_0016"));
    assert.ok(!collapsedText?.includes("long_code_line_0017"));
    assert.ok(!collapsedText?.includes("long_code_line_1000"));
    const collapsedDescendants = await rendered.locator("*").count();

    const toggle = rendered.locator(".md-code-toggle");
    assert.equal(await toggle.getAttribute("aria-expanded"), "false");
    assert.equal((await toggle.innerText()).trim(), "展开全部 · 1000 行");
    await toggle.click();
    await page.waitForFunction(
      (selector) => document.querySelector(selector)?.textContent?.includes("long_code_line_1000"),
      ".timeline .agent .md .md-code code",
    );

    const expandedText = await code.textContent();
    assert.equal(expandedText?.split("\n").length, lineCount);
    assert.ok(expandedText?.includes("long_code_line_1000"));
    assert.equal(await toggle.getAttribute("aria-expanded"), "true");
    assert.ok(
      (expandedText?.length ?? 0) > (collapsedText?.length ?? 0) * 20,
      "expansion must replace the preview with the complete source",
    );
    assert.ok(
      await rendered.locator("*").count() > collapsedDescendants,
      "full syntax highlighting should mount more DOM than the collapsed preview",
    );
  } finally {
    await page.close();
  }
});

test("an expanded code block stays open while streaming appends content", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const initialLines = Array.from(
    { length: 20 },
    (_, index) => `streaming_code_line_${String(index + 1).padStart(2, "0")}`,
  );

  try {
    await page.evaluate(async (lines) => {
      const ReactModule = await import("/node_modules/.vite/deps/react.js");
      const ReactDomModule = await import("/node_modules/.vite/deps/react-dom_client.js");
      const createElement = ReactModule.createElement ?? ReactModule.default?.createElement;
      const createRoot = ReactDomModule.createRoot ?? ReactDomModule.default?.createRoot;
      const { Markdown } = await import("/src/components/room/Markdown.tsx");
      const host = document.createElement("div");
      host.id = "streaming-code-fixture";
      document.body.append(host);
      const root = createRoot(host);
      globalThis.__rCodeStreamingMarkdownRoot = root;
      globalThis.__rCodeRenderStreamingMarkdown = (nextLines) => {
        root.render(createElement(Markdown, {
          text: `\`\`\`python\n${nextLines.join("\n")}\n\`\`\``,
          streaming: true,
        }));
      };
      globalThis.__rCodeRenderStreamingMarkdown(lines);
    }, initialLines);

    const fixture = page.locator("#streaming-code-fixture");
    const toggle = fixture.locator(".md-code-toggle");
    await toggle.waitFor({ state: "visible" });
    assert.match((await toggle.innerText()).trim(), /^展开全部 · 2[01] 行$/);
    await toggle.click();
    assert.equal(await toggle.getAttribute("aria-expanded"), "true");

    await page.evaluate((lines) => {
      globalThis.__rCodeRenderStreamingMarkdown([...lines, "streaming_code_line_21"]);
    }, initialLines);
    await page.waitForFunction(
      () => document.querySelector("#streaming-code-fixture .md-code code")?.textContent?.includes("streaming_code_line_21"),
    );
    assert.equal(
      await fixture.locator(".md-code-toggle").getAttribute("aria-expanded"),
      "true",
      "streaming growth must preserve the user's expanded state",
    );
  } finally {
    await page.evaluate(() => {
      globalThis.__rCodeStreamingMarkdownRoot?.unmount();
      delete globalThis.__rCodeStreamingMarkdownRoot;
      delete globalThis.__rCodeRenderStreamingMarkdown;
    }).catch(() => {});
    await page.close();
  }
});

test("an expanded code block resets when its source is replaced", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const originalLines = Array.from(
    { length: 20 },
    (_, index) => `original_code_line_${String(index + 1).padStart(2, "0")}`,
  );
  const replacementLines = Array.from(
    { length: 20 },
    (_, index) => `replacement_code_line_${String(index + 1).padStart(2, "0")}`,
  );

  try {
    await page.evaluate(async (lines) => {
      const ReactModule = await import("/node_modules/.vite/deps/react.js");
      const ReactDomModule = await import("/node_modules/.vite/deps/react-dom_client.js");
      const createElement = ReactModule.createElement ?? ReactModule.default?.createElement;
      const createRoot = ReactDomModule.createRoot ?? ReactDomModule.default?.createRoot;
      const { Markdown } = await import("/src/components/room/Markdown.tsx");
      const host = document.createElement("div");
      host.id = "replacement-code-fixture";
      document.body.append(host);
      const root = createRoot(host);
      globalThis.__rCodeReplacementMarkdownRoot = root;
      globalThis.__rCodeRenderReplacementMarkdown = (nextLines) => {
        root.render(createElement(Markdown, {
          text: `\`\`\`python\n${nextLines.join("\n")}\n\`\`\``,
        }));
      };
      globalThis.__rCodeRenderReplacementMarkdown(lines);
    }, originalLines);

    const fixture = page.locator("#replacement-code-fixture");
    const toggle = fixture.locator(".md-code-toggle");
    await toggle.waitFor({ state: "visible" });
    await toggle.click();
    assert.equal(await toggle.getAttribute("aria-expanded"), "true");

    await page.evaluate((lines) => globalThis.__rCodeRenderReplacementMarkdown(lines), replacementLines);
    await page.waitForFunction(
      () => document.querySelector("#replacement-code-fixture .md-code code")?.textContent?.includes("replacement_code_line_01"),
    );
    assert.equal(
      await fixture.locator(".md-code-toggle").getAttribute("aria-expanded"),
      "false",
      "a different source replacing the same block position should return to the safe collapsed default",
    );
  } finally {
    await page.evaluate(() => {
      globalThis.__rCodeReplacementMarkdownRoot?.unmount();
      delete globalThis.__rCodeReplacementMarkdownRoot;
      delete globalThis.__rCodeRenderReplacementMarkdown;
    }).catch(() => {});
    await page.close();
  }
});

test("a collapsed 1000-line tool payload also mounts only its preview", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const output = Array.from(
    { length: 1000 },
    (_, index) => `tool_payload_line_${String(index + 1).padStart(4, "0")}`,
  ).join("\n");

  try {
    await page.evaluate(async (toolOutput) => {
      const ReactModule = await import("/node_modules/.vite/deps/react.js");
      const ReactDomModule = await import("/node_modules/.vite/deps/react-dom_client.js");
      const createElement = ReactModule.createElement ?? ReactModule.default?.createElement;
      const createRoot = ReactDomModule.createRoot ?? ReactDomModule.default?.createRoot;
      const { ToolCard } = await import("/src/components/room/ToolCard.tsx");
      const host = document.createElement("div");
      host.id = "tool-payload-fixture";
      document.body.append(host);
      const root = createRoot(host);
      root.render(createElement(ToolCard, {
        name: "shell_command",
        target: "print many lines",
        state: "ok",
        summary: "completed",
        inputJson: JSON.stringify({ command: "print many lines" }),
        outputJson: JSON.stringify({ output: toolOutput }),
        t: 0,
      }));
      globalThis.__rCodeToolPayloadRoot = root;
    }, output);
    const card = page.locator("#tool-payload-fixture .tcard");
    await card.waitFor({ state: "visible" });
    await card.locator(".tcard-head").click();
    const code = card.locator(".tcard-payload .md-code code").last();
    await code.waitFor({ state: "attached" });
    const collapsedText = await code.textContent();
    assert.equal(collapsedText?.split("\n").length, 16);
    assert.ok(collapsedText?.includes("tool_payload_line_0016"));
    assert.ok(!collapsedText?.includes("tool_payload_line_0017"));
    assert.ok(!collapsedText?.includes("tool_payload_line_1000"));

    const toggle = card.locator(".tcard-payload .md-code-toggle").last();
    await toggle.click();
    const expandedText = await code.textContent();
    // Tool payloads are safety-clamped before render; expansion must still expose the complete
    // retained payload rather than merely unmasking a CSS-clipped, pre-mounted token tree.
    assert.ok((expandedText?.split("\n").length ?? 0) > 16);
    assert.ok((expandedText?.length ?? 0) > (collapsedText?.length ?? 0) * 10);
  } finally {
    await page.evaluate(() => {
      globalThis.__rCodeToolPayloadRoot?.unmount();
      delete globalThis.__rCodeToolPayloadRoot;
      document.querySelector("#tool-payload-fixture")?.remove();
    }).catch(() => {});
    await page.close();
  }
});

test("long subagent transcripts are contained and unchanged polls preserve their DOM", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  await page.evaluate(async () => {
    const messages = [];
    for (let index = 1; index <= 25; index += 1) {
      const callId = `long-call-${index}`;
      messages.push({
        id: `long-message-${index}`,
        branch_id: "main",
        kind: "message",
        role: "assistant",
        text: `长记录第 ${index} 段，保留足够多的公开推理依据。`,
      });
      messages.push({
        id: `long-tool-call-${index}`,
        branch_id: "main",
        kind: "tool_call",
        tool_name: "Codex 命令",
        call_id: callId,
        input_json: JSON.stringify({ summary: `rg -n long-record-${index} src` }),
      });
      messages.push({
        id: `long-tool-result-${index}`,
        branch_id: "main",
        kind: "tool_result",
        call_id: callId,
        output_json: JSON.stringify({ status: "completed", output: `match-${index}` }),
        is_error: false,
      });
    }
    messages.push({
      id: "long-message-completeness",
      branch_id: "main",
      kind: "message",
      role: "assistant",
      text: `${"完整正文依据。".repeat(4000)}\n\nSUBAGENT_MESSAGE_TAIL_SENTINEL`,
    });
    messages.push({
      id: "long-reasoning-lazy",
      branch_id: "main",
      kind: "system",
      text: "r_code_reasoning",
      output_json: JSON.stringify({
        text: `${"逐步核对推理依据。".repeat(4000)}\n\nSUBAGENT_REASONING_TAIL_SENTINEL`,
      }),
    });
    globalThis.__rCodeLongSubagentMessages = messages;
    globalThis.__rCodeLongSubagentPolls = 0;
  });

  await page.evaluate(async () => {
    // `ipc.ts` is already loaded by the app. Install the override through the Tauri bridge and
    // then import a fresh URL so this test exercises the same async IPC boundary as desktop.
    const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
    globalThis.__TAURI_INTERNALS__ = {
      invoke: async (command, args = {}) => {
        if (command === "cmd_subagent_session_message_page") {
          globalThis.__rCodeLongSubagentPolls += 1;
          const request = args.request ?? {};
          const cursor = typeof request.after_cursor === "string"
            ? Number.parseInt(request.after_cursor.replace(/^test:/, ""), 10)
            : null;
          const start = Number.isFinite(cursor)
            ? Math.min(globalThis.__rCodeLongSubagentMessages.length, cursor)
            : 0;
          const messages = globalThis.__rCodeLongSubagentMessages.slice(start);
          return {
            messages: structuredClone(messages),
            call_id_updates: [],
            next_cursor: `test:${globalThis.__rCodeLongSubagentMessages.length}`,
            has_more_before: false,
            reset: false,
            unchanged: cursor !== null && messages.length === 0,
          };
        }
        return browserMockInvoke(command, args);
      },
    };
  });

  try {
    await page.evaluate(async () => {
      const { useAppStore } = await import("/src/store/app.ts");
      const { useTasksStore } = await import("/src/store/tasks.ts");
      useTasksStore.getState().setCurrentProject("D:/project/rust/r-code");
      await useTasksStore.getState().refreshDetail("mock-task-queue");
      useAppStore.getState().openRoom("mock-task-queue");
    });
    await page.locator("#main-content > .scene-room").waitFor({ state: "visible" });
    const chip = page.locator(".timeline-subagent-chip").filter({ hasText: "Codex CLI · 检查并发边界" });
    await chip.waitFor({ state: "visible" });
    await chip.click();
    await page.getByTestId("subagent-detail").waitFor({ state: "visible" });

    const transcript = page.locator(".subagent-transcript.is-long");
    await transcript.waitFor({ state: "visible" });
    const blockCount = await transcript.locator(":scope > *").count();
    assert.ok(blockCount >= 24, `long transcript marker requires at least 24 blocks, got ${blockCount}`);
    const containment = await transcript.locator(":scope > *").evaluateAll((items) => items.map((item) => {
      const style = getComputedStyle(item);
      return { contentVisibility: style.contentVisibility, intrinsicSize: style.containIntrinsicSize };
    }));
    assert.ok(containment.slice(0, -1).every((item) => item.contentVisibility === "auto"));
    assert.ok(containment.slice(0, -1).every((item) => item.intrinsicSize.includes("160px")));
    assert.equal(containment.at(-1)?.contentVisibility, "visible");
    assert.equal(containment.at(-1)?.intrinsicSize, "none");

    const completeMessage = transcript.locator(".subagent-transcript-message", {
      hasText: "SUBAGENT_MESSAGE_TAIL_SENTINEL",
    });
    await completeMessage.waitFor({ state: "attached" });
    assert.ok(
      (await completeMessage.textContent())?.includes("SUBAGENT_MESSAGE_TAIL_SENTINEL"),
      "assistant output beyond the former 20k boundary must remain available",
    );

    const reasoning = transcript.locator(".subagent-reasoning-event").last();
    const reasoningSummary = reasoning.locator("summary small");
    assert.equal(await reasoning.locator(".subagent-reasoning-detail").count(), 0);
    assert.ok((await reasoningSummary.textContent())?.length <= 321);
    assert.ok(!(await reasoningSummary.textContent())?.includes("SUBAGENT_REASONING_TAIL_SENTINEL"));
    await reasoning.locator("summary").click();
    const reasoningDetail = reasoning.locator(".subagent-reasoning-detail .md");
    await reasoningDetail.waitFor({ state: "attached" });
    assert.ok(
      (await reasoningDetail.textContent())?.includes("SUBAGENT_REASONING_TAIL_SENTINEL"),
      "expanding reasoning must mount its complete content",
    );

    const baselinePolls = await page.evaluate(() => {
      const node = document.querySelector(".subagent-transcript.is-long");
      if (!node || !node.firstElementChild) throw new Error("long transcript is missing");
      globalThis.__rCodeLongTranscriptNode = node;
      globalThis.__rCodeLongTranscriptFirst = node.firstElementChild;
      globalThis.__rCodeLongTranscriptMutations = [];
      globalThis.__rCodeLongTranscriptObserver = new MutationObserver((records) => {
        globalThis.__rCodeLongTranscriptMutations.push(...records.map((record) => record.type));
      });
      globalThis.__rCodeLongTranscriptObserver.observe(node, {
        attributes: true,
        characterData: true,
        childList: true,
        subtree: true,
      });
      return globalThis.__rCodeLongSubagentPolls;
    });

    await page.waitForFunction(
      (baseline) => globalThis.__rCodeLongSubagentPolls > baseline,
      baselinePolls,
      { timeout: 5000 },
    );
    const stable = await page.evaluate(() => ({
      sameTranscript: globalThis.__rCodeLongTranscriptNode === document.querySelector(".subagent-transcript.is-long"),
      sameFirstBlock: globalThis.__rCodeLongTranscriptFirst === document.querySelector(".subagent-transcript.is-long")?.firstElementChild,
      mutations: [...globalThis.__rCodeLongTranscriptMutations],
      polls: globalThis.__rCodeLongSubagentPolls,
    }));
    assert.equal(stable.sameTranscript, true);
    assert.equal(stable.sameFirstBlock, true);
    assert.deepEqual(stable.mutations, []);
    assert.ok(stable.polls > baselinePolls);
  } finally {
    await page.evaluate(() => {
      globalThis.__rCodeLongTranscriptObserver?.disconnect();
      delete globalThis.__rCodeLongTranscriptObserver;
      delete globalThis.__rCodeLongTranscriptMutations;
      delete globalThis.__rCodeLongTranscriptFirst;
      delete globalThis.__rCodeLongTranscriptNode;
      delete globalThis.__rCodeLongSubagentPolls;
      delete globalThis.__rCodeLongSubagentMessages;
      delete globalThis.__TAURI_INTERNALS__;
    }).catch(() => {});
    await page.close();
  }
});

test("browser mock cursors preserve the loaded history window across idle polls", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  try {
    const result = await page.evaluate(async () => {
      const { browserMockInvoke } = await import("/src/lib/browser-mock-runtime.ts");
      const args = {
        taskId: "mock-task-queue",
        subagentId: "mock-task-queue-codex-active",
      };
      const initial = await browserMockInvoke("cmd_subagent_session_message_page", {
        ...args,
        request: { limit: 2 },
      });
      const firstHistory = await browserMockInvoke("cmd_subagent_session_message_page", {
        ...args,
        request: { before_cursor: initial.previous_cursor, limit: 2 },
      });
      const idle = await browserMockInvoke("cmd_subagent_session_message_page", {
        ...args,
        request: { after_cursor: firstHistory.next_cursor, limit: 2 },
      });
      const secondHistory = await browserMockInvoke("cmd_subagent_session_message_page", {
        ...args,
        request: { before_cursor: idle.previous_cursor, limit: 2 },
      });
      const reset = await browserMockInvoke("cmd_subagent_session_message_page", {
        ...args,
        request: { after_cursor: "not-a-valid-window", limit: 2 },
      });
      return { initial, firstHistory, idle, secondHistory, reset };
    });

    assert.match(result.initial.next_cursor, /^mock:window:\d+:\d+$/);
    assert.equal(result.firstHistory.next_cursor, result.firstHistory.previous_cursor);
    assert.equal(result.idle.next_cursor, result.firstHistory.next_cursor);
    assert.equal(result.idle.previous_cursor, result.firstHistory.previous_cursor);
    assert.equal(result.idle.unchanged, true);
    assert.deepEqual(result.idle.messages, []);
    const firstIds = new Set(result.firstHistory.messages.map((message) => message.id));
    assert.ok(result.secondHistory.messages.every((message) => !firstIds.has(message.id)));
    assert.equal(result.reset.reset, true);
    assert.match(result.reset.previous_cursor, /^mock:window:\d+:\d+$/);
    assert.equal(result.reset.has_more_before, true);
  } finally {
    await page.close();
  }
});
