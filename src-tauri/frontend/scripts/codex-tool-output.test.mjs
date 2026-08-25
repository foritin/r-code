// M2-01（PRD §10）：工具输出增量的前端呈现。
//   A2：输出增量按序原位追加到活动工具卡，终态 ToolResult 权威覆盖。
//   A3：迟到输出（无活动卡片）被丢弃，不产生新节点。

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

test("m2_01_a2 tool card accumulates ordered output deltas then final replaces", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const out = await page.evaluate(async () => {
    const { applyAgentEventInPlace } = await import("/src/components/room/model.ts");
    const items = [];
    let nextId = 0;
    const nid = () => `live-${(nextId += 1)}`;
    const feed = (event) => applyAgentEventInPlace(items, event, 1, nid);

    feed({ type: "tool_call", name: "Codex 命令", input: { summary: "cargo test" }, call_id: "cmd-1" });
    feed({ type: "tool_output_delta", call_id: "cmd-1", safe_delta: "running 2 tests\n" });
    feed({ type: "tool_output_delta", call_id: "cmd-1", safe_delta: "test result: FAILED\n" });
    const mid = items.find((item) => item.kind === "tool");
    const midOutput = mid && mid.kind === "tool" ? mid.outputJson : null;
    const midState = mid && mid.kind === "tool" ? mid.state : null;

    feed({
      type: "tool_result",
      call_id: "cmd-1",
      output: { status: "failed", exit_code: 101, output: "running 2 tests\ntest result: FAILED\n" },
      is_error: true,
    });
    const finalItem = items.find((item) => item.kind === "tool");
    return {
      toolCount: items.filter((item) => item.kind === "tool").length,
      midOutput,
      midState,
      finalOutput: finalItem && finalItem.kind === "tool" ? finalItem.outputJson : null,
      finalState: finalItem && finalItem.kind === "tool" ? finalItem.state : null,
    };
  });
  assert.equal(out.toolCount, 1, "one tool card for the whole lifecycle");
  const mid = JSON.parse(out.midOutput);
  assert.equal(mid.status, "streaming");
  assert.equal(mid.output, "running 2 tests\ntest result: FAILED\n", "deltas append in order");
  assert.equal(out.midState, "active", "card stays active while streaming");
  const finalPayload = JSON.parse(out.finalOutput);
  assert.equal(finalPayload.status, "failed");
  assert.equal(finalPayload.exit_code, 101);
  assert.equal(finalPayload.output, "running 2 tests\ntest result: FAILED\n", "final result replaces streamed text");
  assert.equal(out.finalState, "fail", "failure state matches exit code");
  await page.close();
});

test("m2_01_a3 late output deltas without an active card are dropped", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const out = await page.evaluate(async () => {
    const { applyAgentEventInPlace } = await import("/src/components/room/model.ts");
    const items = [];
    let nextId = 0;
    const nid = () => `live-${(nextId += 1)}`;
    const feed = (event) => applyAgentEventInPlace(items, event, 1, nid);

    feed({ type: "tool_call", name: "Codex 命令", input: { summary: "cargo test" }, call_id: "cmd-9" });
    feed({
      type: "tool_result",
      call_id: "cmd-9",
      output: { status: "completed", output: "ok" },
      is_error: false,
    });
    // 终态之后的迟到增量：不得改动已封口卡片，也不得新建节点。
    const late = feed({ type: "tool_output_delta", call_id: "cmd-9", safe_delta: "late" });
    const lateUnknown = feed({ type: "tool_output_delta", call_id: "ghost", safe_delta: "ghost" });
    const card = items.find((item) => item.kind === "tool");
    return {
      toolCount: items.filter((item) => item.kind === "tool").length,
      lateChanged: late.changed,
      lateUnknownChanged: lateUnknown.changed,
      output: card && card.kind === "tool" ? card.outputJson : null,
    };
  });
  assert.equal(out.toolCount, 1);
  assert.equal(out.lateChanged, false, "late delta after terminal state is dropped");
  assert.equal(out.lateUnknownChanged, false, "delta without any card is dropped");
  assert.equal(JSON.parse(out.output).output, "ok", "terminal output is untouched");
  await page.close();
});
