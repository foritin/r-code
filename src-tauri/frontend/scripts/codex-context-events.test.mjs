// M2-02（PRD §10）：计划/diff/压缩/warning/usage 的前端投影。
//   A2：重复 plan 更新幂等；live 序列与历史重建保留最终计划、diff 引用
//       与压缩位置（结构一致）。
//   A3：上下文行紧凑可扫描、长 diff 截断可见且不撑破时间线。

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

const PLAN = (steps) => ({ type: "plan", steps });
const CONTEXT = (event, data) => ({ type: "codex_context_event", event, data });

test("m2_02_a2 duplicate plan updates stay idempotent across live and history rebuild", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const out = await page.evaluate(async () => {
    const { applyAgentEventInPlace, buildTimeline } = await import("/src/components/room/model.ts");
    let nextId = 0;
    const nid = () => `live-${(nextId += 1)}`;
    const items = [];
    const feed = (event) => applyAgentEventInPlace(items, event, 1, nid);

    // live：重复 plan 更新只保留最新一张卡。
    feed({ type: "plan", steps: [{ description: "扫描", completed: true }] });
    feed({ type: "plan", steps: [
      { description: "扫描", completed: true },
      { description: "修复", completed: false },
    ] });
    feed({ type: "codex_context_event", event: "codex_diff", data: { chars: 12_000, truncated: true, preview: "diff head…" } });
    feed({ type: "codex_context_event", event: "r_code_context_compacted", data: {} });
    const livePlans = items.filter((item) => item.kind === "plan");
    const livePlanSteps = livePlans.length === 1 ? livePlans[0].steps : null;
    const liveContextKinds = items.filter((item) => item.kind === "context").map((item) => item.label);

    // history：与后端持久化等价的 SessionMessage 序列（plan 落两条 system）。
    const history = buildTimeline([
      { id: "s:1", branch_id: "b", kind: "message", role: "user", text: "开始" },
      { id: "s:2", branch_id: "b", kind: "system", text: "plan", output_json: JSON.stringify({ steps: [{ description: "扫描", completed: true }] }) },
      { id: "s:3", branch_id: "b", kind: "system", text: "plan", output_json: JSON.stringify({ steps: [
        { description: "扫描", completed: true },
        { description: "修复", completed: false },
      ] }) },
      { id: "s:4", branch_id: "b", kind: "system", text: "codex_diff", output_json: JSON.stringify({ chars: 12_000, truncated: true, preview: "diff head…" }) },
      { id: "s:5", branch_id: "b", kind: "system", text: "r_code_context_compacted", output_json: "{}" },
      { id: "s:6", branch_id: "b", kind: "message", role: "assistant", text: "完成" },
    ], [], [], "2026-08-25T00:00:00.000Z");
    const historyPlans = history.filter((item) => item.kind === "plan");
    const historyPlanSteps = historyPlans.length === 1 ? historyPlans[0].steps : null;
    const historyContextKinds = history.filter((item) => item.kind === "context").map((item) => item.label);
    return {
      livePlanCount: livePlans.length,
      livePlanSteps,
      liveContextKinds,
      historyPlanCount: historyPlans.length,
      historyPlanSteps,
      historyContextKinds,
    };
  });
  assert.equal(out.livePlanCount, 1, "duplicate live plan updates replace the card");
  assert.equal(out.historyPlanCount, 1, "history rebuild keeps only the final plan");
  assert.deepEqual(out.livePlanSteps, out.historyPlanSteps, "final plan identical live vs history");
  assert.deepEqual(out.liveContextKinds, ["Codex 变更摘要", "上下文已自动压缩"]);
  assert.deepEqual(out.historyContextKinds, out.liveContextKinds, "context rows identical live vs history");
  await page.close();
});

test("m2_02_a3 context rows stay compact and long diffs never overflow the timeline", async () => {
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const longPreview = "x".repeat(800);
  await page.evaluate(async ([preview]) => {
    globalThis.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === "cmd_session_messages_for_branch") {
          return [
            { id: "s:1", branch_id: "b", kind: "message", role: "user", text: "改一下" },
            { id: "s:2", branch_id: "b", kind: "codex_commentary", role: "assistant", text: "先看构建。" },
            { id: "s:3", branch_id: "b", kind: "system", text: "plan", output_json: JSON.stringify({ steps: [
              { description: "扫描", completed: true },
              { description: "修复", completed: false },
            ] }) },
            { id: "s:4", branch_id: "b", kind: "system", text: "codex_diff", output_json: JSON.stringify({ chars: 40_000, truncated: true, preview }) },
            { id: "s:5", branch_id: "b", kind: "system", text: "codex_warning", output_json: JSON.stringify({ message: "磁盘空间偏低", code: null }) },
            { id: "s:6", branch_id: "b", kind: "message", role: "assistant", text: "完成。" },
          ];
        }
        return null;
      },
    };
    const React = (await import("/@id/react")).default;
    const mod = await import("/@id/react-dom/client");
    const createRoot = mod.createRoot ?? mod.default?.createRoot;
    const { Timeline } = await import("/src/components/room/Timeline.tsx");
    const container = document.createElement("div");
    container.id = "m2-02-a3-mount";
    container.style.height = "760px";
    container.style.overflow = "auto";
    document.getElementById("root").appendChild(container);
    createRoot(container).render(
      React.createElement(Timeline, {
        taskId: "task-m2-02-a3",
        branchId: "branch-m2-02-a3",
        workspacePath: null,
        cur: null,
        running: false,
        reviewing: false,
      }),
    );
  }, [longPreview]);
  await page.waitForSelector("#m2-02-a3-mount .agent", { timeout: 10_000 });

  const layers = await page.evaluate(() => {
    const mount = document.getElementById("m2-02-a3-mount");
    const contextRows = [...mount.querySelectorAll(".timeline-context-row, [class*='context']")];
    const textOf = (node) => (node.textContent ?? "").length;
    const planCards = mount.querySelectorAll(".todo-card").length;
    return {
      contextRowCount: contextRows.length,
      maxContextText: Math.max(0, ...contextRows.map(textOf)),
      planCards,
      docScrollWidth: document.documentElement.scrollWidth,
    };
  });
  assert.ok(layers.contextRowCount >= 2, "compact diff and warning rows are present");
  assert.ok(layers.planCards >= 1, "plan card visible");
  // 紧凑：diff 行含截断说明但绝不渲染 40k 全文；行文本总量有界。
  assert.ok(layers.maxContextText < 2_000, `context rows stay compact (max ${layers.maxContextText} chars)`);
  assert.ok(layers.docScrollWidth <= 1280, `no horizontal overflow, got ${layers.docScrollWidth}`);
  await page.close();
});
