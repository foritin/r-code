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

test("subagent tool results close the matching live call and survive persisted merging", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const result = await page.evaluate(async () => {
    const { buildLiveEntries, mergeSessionEntries } = await import("/src/components/room/SubagentWorkbench.tsx");
    const { activityTraceReducer, createActivityTraceState } = await import("/src/components/room/activity.ts");
    const events = [
      { id: "event-a-start", kind: "tool_call", callId: "call-a", label: "Codex 命令", detail: "Codex 命令 · rg src", at: 1 },
      { id: "event-b-start", kind: "tool_call", callId: "call-b", label: "Codex 命令", detail: "Codex 命令 · rg src", at: 2 },
      {
        id: "event-a-result",
        kind: "tool_result",
        callId: "call-a",
        label: "工具完成",
        detail: "工具已完成",
        outputJson: JSON.stringify({ status: "completed", output: "first output" }),
        at: 3,
      },
      {
        id: "event-b-result",
        kind: "tool_result",
        callId: "call-b",
        label: "工具失败",
        detail: "工具执行失败",
        outputJson: JSON.stringify({ status: "failed", output: "second output" }),
        at: 4,
        isError: true,
      },
    ];
    const live = buildLiveEntries(events, "running").filter((entry) => entry.kind === "tool");
    const dangling = [{
      id: "dangling",
      kind: "tool_call",
      callId: "call-dangling",
      label: "Codex MCP",
      detail: "Codex MCP · inspect",
      at: 5,
    }];
    const completedDangling = buildLiveEntries(dangling, "completed")[0];
    const failedDangling = buildLiveEntries(dangling, "failed")[0];
    const merged = mergeSessionEntries(
      [{
        id: "persisted-a",
        kind: "tool",
        callId: "call-a",
        toolName: "Codex 命令",
        summary: "rg src",
        inputJson: JSON.stringify({ command: "rg src" }),
        outputJson: null,
        state: "active",
      }],
      [live[0]],
    )[0];
    const scope = {
      run_id: "child-reducer",
      agent_id: "child-reducer",
      parent_run_id: "parent",
      agent_kind: "subagent",
      runtime_kind: "codex_exec",
      access_mode: "read_only",
    };
    let trace = createActivityTraceState();
    trace = activityTraceReducer(trace, {
      type: "event",
      at: 10,
      event: {
        type: "scoped",
        scope,
        event: { type: "tool_call", call_id: "reducer-call", name: "Codex 命令", input: { summary: "rg reducer" } },
      },
    });
    trace = activityTraceReducer(trace, {
      type: "event",
      at: 11,
      event: {
        type: "scoped",
        scope,
        event: {
          type: "tool_result",
          call_id: "reducer-call",
          output: { status: "completed", output: "reducer output" },
          is_error: false,
        },
      },
    });
    const reducedEvents = trace.subagents[0].events;
    return { live, completedDangling, failedDangling, merged, reducedEvents };
  });

  assert.deepEqual(result.live.map((entry) => [entry.callId, entry.state]), [
    ["call-a", "ok"],
    ["call-b", "fail"],
  ]);
  assert.equal(JSON.parse(result.live[0].outputJson), "first output");
  assert.equal(JSON.parse(result.live[1].outputJson), "second output");
  assert.equal(result.completedDangling.state, "ok");
  assert.equal(result.failedDangling.state, "fail");
  assert.equal(result.merged.state, "ok");
  assert.equal(JSON.parse(result.merged.outputJson), "first output");
  assert.deepEqual(result.reducedEvents.map((event) => event.callId), ["reducer-call", "reducer-call"]);
  assert.equal(JSON.parse(result.reducedEvents[1].outputJson).output, "reducer output");
  await page.close();
});

test("main and child activity use semantic collapsed groups and runtime-specific identities", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const result = await page.evaluate(async () => {
    // Code executed through page.evaluate is not transformed by Vite, so bare module
    // specifiers cannot be resolved here. Import Vite's stable optimized-dependency URLs.
    const { default: React } = await import("/node_modules/.vite/deps/react.js");
    const { default: reactDomClient } = await import("/node_modules/.vite/deps/react-dom_client.js");
    const { createRoot } = reactDomClient;
    const { SubagentAvatar } = await import("/src/components/room/SubagentIdentity.tsx");
    const { groupTranscriptEntries, SubagentToolGroup } = await import("/src/components/room/SubagentWorkbench.tsx");
    const { buildTimelineTurns } = await import("/src/components/room/timeline-presentation.ts");
    const { toolActivityTitle } = await import("/src/components/room/tool-activity.ts");

    const tool = (id, toolName) => ({
      id,
      kind: "tool",
      callId: id,
      toolName,
      summary: `${toolName} target`,
      inputJson: "{}",
      outputJson: null,
      state: "ok",
    });
    const childGroups = groupTranscriptEntries([
      tool("read-a", "read_file"),
      tool("read-b", "list_files"),
      tool("command-a", "Codex 命令"),
      tool("command-b", "shell_command"),
      tool("file-a", "Codex 文件修改"),
      tool("file-b", "edit"),
    ]).map((entry) => ({
      kind: entry.kind,
      groupKind: entry.kind === "tool_group" ? entry.groupKind : null,
      count: entry.kind === "tool_group" ? entry.tools.length : 1,
    }));

    const timelineTools = [
      { kind: "tool", id: "timeline-read-a", t: 1, name: "read_file", target: "a.ts", state: "ok", summary: "a.ts", inputJson: "{}", outputJson: null },
      { kind: "tool", id: "timeline-read-b", t: 2, name: "list_files", target: "src", state: "ok", summary: "src", inputJson: "{}", outputJson: null },
      { kind: "tool", id: "timeline-command-a", t: 3, name: "shell_command", target: "npm test", state: "ok", summary: "npm test", inputJson: "{}", outputJson: null },
      { kind: "tool", id: "timeline-command-b", t: 4, name: "Codex 命令", target: "cargo test", state: "ok", summary: "cargo test", inputJson: "{}", outputJson: null },
    ];
    const mainGroups = buildTimelineTurns(timelineTools)
      .flatMap((turn) => turn.items)
      .filter((entry) => entry.kind === "tool_group")
      .map((entry) => [entry.groupKind, entry.tools.length]);
    const narratedTurn = buildTimelineTurns([
      { kind: "agent", id: "narration-a", t: 1, text: "现在修改文件。", streaming: false },
      { kind: "agent", id: "file-result", t: 1, text: "已定位 [实现文件](src/main.rs#L2C3)。", streaming: false },
      timelineTools[0],
      { kind: "agent", id: "narration-b", t: 2, text: "接着运行验证。", streaming: false },
      timelineTools[2],
      { kind: "agent", id: "final-answer", t: 3, text: "修改与验证已完成。", streaming: false },
    ])[0].items.map((entry) => ({
      kind: entry.kind,
      groupKind: entry.kind === "tool_group" ? entry.groupKind : null,
      label: entry.kind === "context" ? entry.label : null,
      detail: entry.kind === "context" ? entry.detail : null,
      text: entry.kind === "agent" ? entry.text : null,
    }));

    const host = document.createElement("div");
    document.body.append(host);
    const root = createRoot(host);
    root.render(React.createElement(React.Fragment, null,
      React.createElement(SubagentAvatar, { identity: "rcode-1", runtimeKind: "native" }),
      React.createElement(SubagentAvatar, { identity: "codex-1", runtimeKind: "codex_exec" }),
      React.createElement(SubagentToolGroup, {
        entry: {
          id: "active-lookup-group",
          kind: "tool_group",
          groupKind: "lookup",
          tools: [
            { ...tool("active-read-a", "read_file"), state: "active" },
            { ...tool("active-read-b", "list_files"), state: "active" },
          ],
        },
      }),
    ));
    await new Promise((resolve) => setTimeout(resolve, 50));
    const identities = [...host.querySelectorAll(".subagent-avatar")].map((avatar) => ({
      runtime: avatar.getAttribute("data-runtime-family"),
      glyph: avatar.querySelector("svg")?.getAttribute("data-agent-glyph"),
      className: avatar.className,
    }));
    const activeGroup = {
      expanded: host.querySelector(".subagent-tool-group-head")?.getAttribute("aria-expanded"),
      details: host.querySelectorAll(".subagent-tool-group-list").length,
    };
    root.unmount();
    host.remove();

    return {
      childGroups,
      mainGroups,
      narratedTurn,
      identities,
      activeGroup,
      titles: [
        toolActivityTitle("lookup", 38, "ok"),
        toolActivityTitle("file", 7, "ok"),
        toolActivityTitle("command", 4, "active"),
      ],
    };
  });

  assert.deepEqual(result.childGroups, [
    // read_file 现在是文件行（彩色类型图标 + 读取动词），与 list_files 分属
    // file/lookup 两种 kind；单条目不归组，保持独立的 tool 条目。
    { kind: "tool", groupKind: null, count: 1 },
    { kind: "tool", groupKind: null, count: 1 },
    { kind: "tool_group", groupKind: "command", count: 2 },
    { kind: "tool_group", groupKind: "file", count: 2 },
  ]);
  assert.deepEqual(result.mainGroups, [["file", 1], ["lookup", 1], ["command", 2]]);
  assert.deepEqual(result.narratedTurn.filter((entry) => entry.kind === "context"), []);
  assert.deepEqual(
    result.narratedTurn.map((entry) => entry.kind === "agent" ? entry.text : `${entry.kind}:${entry.groupKind}`),
    [
      "现在修改文件。",
      "已定位 [实现文件](src/main.rs#L2C3)。",
      "tool_group:file",
      "接着运行验证。",
      "tool_group:command",
      "修改与验证已完成。",
    ],
    "public progress updates must stay in chronological order around their tool activity",
  );
  assert.deepEqual(
    result.narratedTurn.filter((entry) => entry.kind === "agent").map((entry) => entry.text),
    [
      "现在修改文件。",
      "已定位 [实现文件](src/main.rs#L2C3)。",
      "接着运行验证。",
      "修改与验证已完成。",
    ],
  );
  assert.deepEqual(result.identities.map(({ runtime, glyph }) => [runtime, glyph]), [
    ["rcode", "rcode"],
    ["codex", "codex"],
  ]);
  assert.match(result.identities[0].className, /runtime-rcode/);
  assert.match(result.identities[1].className, /runtime-codex/);
  assert.deepEqual(result.activeGroup, { expanded: "false", details: 0 });
  assert.deepEqual(result.titles, ["已探索 38 项", "已处理 7 个文件", "正在执行 4 个命令"]);
  await page.close();
});
