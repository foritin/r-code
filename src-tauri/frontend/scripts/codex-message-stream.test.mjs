// M1-03（PRD §10）：Codex agentMessage 前端流式呈现与历史一致回放。
//   A1：10,000 delta 单 item 单节点、最终文本完整、可见刷新 ≤10Hz（合并器）。
//   A2：live 应用序列 与 buildTimeline 历史重建 结构/顺序/phase 一致。
//   A3：commentary 轻量层（无作者头）/final 作者层 + 1280×800 无横向溢出。

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
  if (process.platform !== "win32") {
    const playwrightCache = path.join(frontendDir, "node_modules", "playwright-core", ".local-browsers");
    if (fs.existsSync(playwrightCache)) {
      const cached = fs.readdirSync(playwrightCache)
        .filter((entry) => /^chromium-\d+$/.test(entry))
        .map((entry) => {
          if (process.platform === "darwin") {
            return path.join(playwrightCache, entry, "chrome-mac", "Chromium.app", "Contents", "MacOS", "Chromium");
          }
          return path.join(playwrightCache, entry, "chrome-linux", "chrome");
        })
        .find((candidate) => fs.existsSync(candidate));
      if (cached) return cached;
    }
  }
  return [
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
      server.close((error) => (error ? reject(error) : resolve(port)));
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
      // Vite 还在启动。
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

const CODEX_DELTA = (itemId, phase, text) => ({
  type: "codex_agent_message",
  item_id: itemId,
  phase,
  text,
  delta: true,
});
const CODEX_SEAL = (itemId, phase, text) => ({
  type: "codex_agent_message",
  item_id: itemId,
  phase,
  text,
  delta: false,
});

test("m1_03_a1 ten thousand deltas keep one node, full text and ≤10Hz visible flushes", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const out = await page.evaluate(async () => {
    const { applyAgentEventInPlace, createAgentEventCoalescer } = await import("/src/components/room/model.ts");
    const TOTAL = 10_000;
    const items = [];
    let nextId = 0;
    const nid = () => `live-${(nextId += 1)}`;

    // 1) 单 item 一万增量：最终文本完整、只有一个 agent 节点。
    for (let i = 0; i < TOTAL; i += 1) {
      applyAgentEventInPlace(items, {
        type: "codex_agent_message",
        item_id: "f1",
        phase: "final_answer",
        text: "字",
        delta: true,
      }, 1, nid);
    }
    applyAgentEventInPlace(items, {
      type: "codex_agent_message",
      item_id: "f1",
      phase: "final_answer",
      text: "",
      delta: false,
    }, 1, nid);
    const single = items.filter((item) => item.kind === "agent");
    const nodeCount = single.length;
    const textLength = single[0]?.text.length ?? 0;
    const sealed = single[0]?.streaming === false;

    // 2) 合并器：假时钟驱动，10 秒内 10k 事件 → 可见应用次数 ≤ 10Hz+1，
    //    且所有事件按序进入（无丢失）。
    const pending = [];
    let applied = 0;
    let applyCount = 0;
    const clock = { now: 0, queue: [] };
    const coalescer = createAgentEventCoalescer((batch) => {
      applyCount += 1;
      applied += batch.length;
      for (const event of batch) pending.push(event.text);
    }, (flush) => {
      clock.queue.push({ at: clock.now + 100, flush });
    }, 100);
    for (let tick = 0; tick < 100; tick += 1) {
      clock.now = tick * 100;
      // 该 100ms 窗口内的所有待触发冲刷先执行。
      clock.queue = clock.queue.filter((entry) => {
        if (entry.at <= clock.now) {
          entry.flush();
          return false;
        }
        return true;
      });
      for (let j = 0; j < 100; j += 1) {
        coalescer.push({ type: "codex_agent_message", item_id: "x", phase: "commentary", text: `${tick}-${j}`, delta: true });
      }
    }
    clock.now = 10_000;
    clock.queue.forEach((entry) => entry.flush());
    clock.queue = [];
    coalescer.flush();
    return { nodeCount, textLength, sealed, applyCount, applied };
  });
  assert.equal(out.nodeCount, 1, "10k deltas must render as exactly one agent node");
  assert.equal(out.textLength, 10_000, "final text must be complete with no lost characters");
  assert.equal(out.sealed, true, "seal frame closes the stream");
  assert.equal(out.applied, 10_000, "coalescer must not drop any event");
  assert.ok(out.applyCount <= 101, `visible applies must stay ≤10Hz (+1 final flush), got ${out.applyCount}`);
  await page.close();
});

test("m1_03_a2 live sequence and history rebuild agree on structure, order and phase", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const out = await page.evaluate(async () => {
    const { applyAgentEventInPlace, buildTimeline } = await import("/src/components/room/model.ts");
    let nextId = 0;
    const nid = () => `live-${(nextId += 1)}`;

    // live：commentary → tool → final（含交错与封口）。
    const live = [];
    const feed = (event) => applyAgentEventInPlace(live, event, 1, nid);
    feed({ type: "codex_agent_message", item_id: "c1", phase: "commentary", text: "先看构建。", delta: true });
    feed({ type: "codex_agent_message", item_id: "c1", phase: "commentary", text: "入口在配置模块。", delta: true });
    feed({ type: "codex_agent_message", item_id: "c1", phase: "commentary", text: "", delta: false });
    feed({ type: "tool_call", name: "search", input: { pattern: "config" }, call_id: "call-1" });
    feed({ type: "tool_result", call_id: "call-1", output: "3 matches", is_error: false });
    feed({ type: "codex_agent_message", item_id: "f1", phase: "final_answer", text: "已修复，", delta: true });
    feed({ type: "codex_agent_message", item_id: "f1", phase: "final_answer", text: "测试全绿。", delta: true });
    feed({ type: "codex_agent_message", item_id: "f1", phase: "final_answer", text: "", delta: false });

    // history：与后端持久化等价的 SessionMessage 序列。
    const history = buildTimeline([
      { id: "s:1", branch_id: "b", kind: "message", role: "user", text: "修一下" },
      { id: "s:2", branch_id: "b", kind: "codex_commentary", role: "assistant", text: "先看构建。入口在配置模块。" },
      { id: "s:3", branch_id: "b", kind: "tool_call", tool_name: "search", call_id: "call-1", input_json: JSON.stringify({ pattern: "config" }) },
      { id: "s:4", branch_id: "b", kind: "tool_result", call_id: "call-1", output_json: JSON.stringify("3 matches"), is_error: false },
      { id: "s:5", branch_id: "b", kind: "message", role: "assistant", text: "已修复，测试全绿。" },
    ], [], [], "2026-08-25T00:00:00.000Z");

    const shape = (item) => {
      if (item.kind === "agent") return { kind: "agent", phase: item.phase ?? null, text: item.text };
      if (item.kind === "tool") return { kind: "tool", callId: item.callId, state: item.state };
      return { kind: item.kind };
    };
    const liveShape = live
      .filter((item) => item.kind === "agent" || item.kind === "tool")
      .map(shape);
    const historyShape = history
      .filter((item) => item.kind === "agent" || item.kind === "tool")
      .map(shape);
    return { liveShape, historyShape };
  });
  assert.deepEqual(
    out.liveShape.map((item) => ({ ...item, callId: item.callId ?? "call-1" })),
    out.historyShape.map((item) => ({ ...item, callId: item.callId ?? "call-1" })),
    "live and history rebuild must agree on structure, order and phase",
  );
  assert.equal(out.liveShape[0].phase, "commentary");
  assert.equal(out.liveShape[0].text, "先看构建。入口在配置模块。");
  assert.equal(out.liveShape[2].phase, null, "final keeps the default authoritative layer (history has no phase to restore)");
  assert.equal(out.liveShape[2].text, "已修复，测试全绿。");
  await page.close();
});

test("m1_03_a3 real Timeline renders layered commentary/final without horizontal overflow", async () => {
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const longCodeLine = "x".repeat(400);
  await page.evaluate(async ([longLine]) => {
    globalThis.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === "cmd_session_messages_for_branch") {
          return [
            { id: "s:1", branch_id: "b", kind: "message", role: "user", text: "梳理这段代码" },
            { id: "s:2", branch_id: "b", kind: "codex_commentary", role: "assistant", text: "先看构建入口：`build.rs` 负责前端产物校验。" },
            { id: "s:3", branch_id: "b", kind: "tool_call", tool_name: "search", call_id: "call-9", input_json: JSON.stringify({ pattern: "config" }) },
            { id: "s:4", branch_id: "b", kind: "tool_result", call_id: "call-9", output_json: JSON.stringify("3 matches"), is_error: false },
            {
              id: "s:5",
              branch_id: "b",
              kind: "message",
              role: "assistant",
              text: `结论：入口收敛在 build.rs。\n\n\`\`\`rust\nfn main() { let line = "${longLine}"; }\n\`\`\`\n\n验证：cargo check 通过。`,
            },
          ];
        }
        return null;
      },
    };
    const React = (await import("/@id/react")).default ?? (await import("/@id/react"));
    const reactDomClient = await import("/@id/react-dom/client");
    const createRoot = reactDomClient.createRoot ?? reactDomClient.default?.createRoot;
    const { Timeline } = await import("/src/components/room/Timeline.tsx");
    const container = document.createElement("div");
    container.id = "m1-03-a3-mount";
    container.style.height = "760px";
    container.style.overflow = "auto";
    document.getElementById("root").appendChild(container);
    createRoot(container).render(
      React.createElement(Timeline, {
        taskId: "task-m1-03-a3",
        branchId: "branch-m1-03-a3",
        workspacePath: null,
        cur: null,
        running: false,
        reviewing: false,
      }),
    );
  }, [longCodeLine]);
  await page.waitForSelector("#m1-03-a3-mount .agent", { timeout: 10_000 });

  const layers = await page.evaluate(() => {
    const mount = document.getElementById("m1-03-a3-mount");
    const agents = [...mount.querySelectorAll(".agent")];
    return {
      commentaryCount: agents.filter((node) => node.classList.contains("timeline-progress-update")).length,
      commentaryWithAuthor: agents
        .filter((node) => node.classList.contains("timeline-progress-update"))
        .filter((node) => node.querySelector(":scope > .who") != null).length,
      finalCount: agents.filter((node) => !node.classList.contains("timeline-progress-update")).length,
      finalWithAuthor: agents
        .filter((node) => !node.classList.contains("timeline-progress-update"))
        .filter((node) => node.querySelector(":scope > .who") != null).length,
      toolCards: mount.querySelectorAll(".timeline-activity-event").length,
      docScrollWidth: document.documentElement.scrollWidth,
      mountScrollWidth: mount.scrollWidth,
      mountClientWidth: mount.clientWidth,
    };
  });
  assert.equal(layers.commentaryCount, 1, "commentary uses the lightweight progress-update layer");
  assert.equal(layers.commentaryWithAuthor, 0, "commentary must not show the R-CODE author header");
  assert.equal(layers.finalCount, 1, "final answer renders as the single authoritative agent bubble");
  assert.ok(layers.finalWithAuthor >= 1, "final answer shows the author header");
  assert.ok(layers.toolCards > 0, "structured tool activity is visible between commentary and final");
  assert.ok(
    layers.docScrollWidth <= 1280,
    `no horizontal page overflow at 1280px, got ${layers.docScrollWidth}`,
  );
  await page.close();
});
