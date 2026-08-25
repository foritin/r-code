// M4-01.A4（§6 性能门禁）：产出机器可读性能指标并断言阈值。
//   - 传播延迟：delta 入队 → 批量应用，p95 ≤ 250ms（真实定时器）。
//   - 更新密度：10k delta 可见应用次数 ≤ 10Hz+1；单 item 单节点、全文完整。
//   - DOM 有界：真实 Timeline 挂载长内容，节点数远小于 delta 数。
// 指标写入 artifacts/ai-tasks/verification/codex-rich-interaction/implementation/performance.json。

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
const reportPath = path.resolve(
  frontendDir,
  "..",
  "..",
  "artifacts",
  "ai-tasks",
  "verification",
  "codex-rich-interaction",
  "implementation",
  "performance.json"
);

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

function percentile(sorted, p) {
  const index = Math.min(sorted.length - 1, Math.ceil((p / 100) * sorted.length) - 1);
  return sorted[Math.max(0, index)];
}

test("m4_01_a4 §6 performance metrics exist and pass thresholds", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });

  const metrics = await page.evaluate(async () => {
    const { applyAgentEventInPlace, createAgentEventCoalescer } = await import("/src/components/room/model.ts");
    const TOTAL = 10_000;

    // -- 1) 传播延迟 + 更新密度（真实 100ms 定时器）-----------------------
    const latencies = [];
    let appliedEvents = 0;
    let visibleApplies = 0;
    let batchFirstEnqueuedAt = null;
    const coalescer = createAgentEventCoalescer((batch) => {
      visibleApplies += 1;
      appliedEvents += batch.length;
      if (batchFirstEnqueuedAt != null) {
        latencies.push(performance.now() - batchFirstEnqueuedAt);
        batchFirstEnqueuedAt = null;
      }
    }, (flush) => setTimeout(flush, 100), 100);
    for (let i = 0; i < TOTAL; i += 1) {
      if (batchFirstEnqueuedAt == null) batchFirstEnqueuedAt = performance.now();
      coalescer.push({
        type: "codex_agent_message",
        item_id: "perf",
        phase: "final_answer",
        text: "字",
        delta: true,
      });
      // 分散入队时间：模拟真实到达节奏（每 25 个一批注入）。
      if (i % 25 === 0) await new Promise((resolve) => setTimeout(resolve, 0));
    }
    await new Promise((resolve) => setTimeout(resolve, 400));
    coalescer.flush();

    // -- 2) reducer：单节点 + 全文完整 ------------------------------------
    const items = [];
    let nextId = 0;
    const nid = () => `live-${(nextId += 1)}`;
    for (let i = 0; i < TOTAL; i += 1) {
      applyAgentEventInPlace(items, {
        type: "codex_agent_message",
        item_id: "perf",
        phase: "final_answer",
        text: "字",
        delta: true,
      }, 1, nid);
    }
    applyAgentEventInPlace(items, {
      type: "codex_agent_message",
      item_id: "perf",
      phase: "final_answer",
      text: "",
      delta: false,
    }, 1, nid);
    const agentNodes = items.filter((item) => item.kind === "agent");
    const reducerNodeCount = agentNodes.length;
    const finalTextLength = agentNodes[0]?.text.length ?? 0;
    const reducerItemTotal = items.length;
    return {
      total_events: TOTAL,
      applied_events: appliedEvents,
      visible_applies: visibleApplies,
      latency_samples: latencies,
      reducer_node_count: reducerNodeCount,
      reducer_item_total: reducerItemTotal,
      final_text_length: finalTextLength,
    };
  });

  const sorted = [...metrics.latency_samples].sort((a, b) => a - b);
  const p95 = percentile(sorted, 95);
  const max = sorted[sorted.length - 1] ?? 0;
  const densityHz = metrics.visible_applies / (metrics.latency_samples.length > 0 ? 10 : 1);

  // -- 3) DOM 有界：真实 Timeline 渲染长内容 ------------------------------
  await page.evaluate(async () => {
    globalThis.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === "cmd_session_messages_for_branch") {
          return [
            { id: "s:1", branch_id: "b", kind: "message", role: "user", text: "写十万个字" },
            {
              id: "s:2",
              branch_id: "b",
              kind: "message",
              role: "assistant",
              text: `结论：\n\n${"很长的正文段落。".repeat(2_000)}`,
            },
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
    container.id = "m4-01-perf-mount";
    container.style.height = "760px";
    container.style.overflow = "auto";
    document.getElementById("root").appendChild(container);
    createRoot(container).render(
      React.createElement(Timeline, {
        taskId: "task-perf",
        branchId: "branch-perf",
        workspacePath: null,
        cur: null,
        running: false,
        reviewing: false,
      }),
    );
  });
  await page.waitForSelector("#m4-01-perf-mount .agent", { timeout: 10_000 });
  const domNodes = await page.evaluate(() =>
    document.getElementById("m4-01-perf-mount")?.querySelectorAll("*").length ?? 0
  );
  await page.close();

  const report = {
    schema_version: "codex-interaction-performance.v1",
    generated_at: new Date().toISOString(),
    platform: { node: process.version, os: process.platform },
    metrics: {
      total_delta_events: metrics.total_events,
      applied_events: metrics.applied_events,
      visible_applies: metrics.visible_applies,
      propagation_latency_ms: { p95: Math.round(p95 * 10) / 10, max: Math.round(max * 10) / 10, samples: sorted.length },
      visible_density_hz_estimate: Math.round(densityHz * 10) / 10,
      reducer_agent_node_count: metrics.reducer_node_count,
      reducer_total_items: metrics.reducer_item_total,
      final_text_length: metrics.final_text_length,
      dom_node_count_long_message: domNodes,
    },
    thresholds: {
      propagation_p95_ms_max: 250,
      visible_applies_max: 101,
      agent_node_count_max: 1,
      final_text_length_min: metrics.total_events,
      dom_nodes_max_vs_events: metrics.total_events,
    },
  };
  fs.mkdirSync(path.dirname(reportPath), { recursive: true });
  fs.writeFileSync(reportPath, `${JSON.stringify(report, null, 2)}\n`);

  assert.equal(
    metrics.applied_events,
    metrics.total_events,
    "coalescer must not drop any event"
  );
  assert.ok(
    metrics.visible_applies <= 101,
    `visible applies must stay ≤10Hz (+1 flush), got ${metrics.visible_applies}`
  );
  assert.ok(p95 <= 250, `propagation p95 ${p95}ms exceeds 250ms`);
  assert.equal(metrics.reducer_node_count, 1, "10k deltas render as one agent node");
  assert.equal(
    metrics.final_text_length,
    metrics.total_events,
    "final text must contain every delta character"
  );
  assert.ok(
    domNodes < metrics.total_events,
    `DOM node count (${domNodes}) must not scale with delta events (${metrics.total_events})`
  );
  assert.ok(fs.existsSync(reportPath), `performance report written: ${reportPath}`);
});
