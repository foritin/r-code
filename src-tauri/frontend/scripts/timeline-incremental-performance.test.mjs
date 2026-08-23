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
      server.close((error) => error ? reject(error) : resolve(address.port));
    });
  });
}

async function waitForServer(url, handle) {
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    if (handle.exitCode != null) throw new Error(`Vite exited with ${handle.exitCode}`);
    try {
      if ((await fetch(url)).ok) return;
    } catch {
      // Still starting.
    }
    await new Promise((resolve) => setTimeout(resolve, 80));
  }
  throw new Error("Timed out waiting for Vite");
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

test("streaming tokens preserve completed turns and only rebuild the active tail", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const result = await page.evaluate(async () => {
    const { applyAgentEventInPlace } = await import("/src/components/room/model.ts");
    const { TimelinePresentationCache } = await import("/src/components/room/timeline-presentation.ts");
    const items = [];
    for (let index = 0; index < 5_000; index += 1) {
      items.push({
        kind: "you",
        id: `user-${index}`,
        t: index,
        text: `question ${index}`,
        imageCount: 0,
        imageMediaTypes: [],
        attachments: [],
        sendMode: "auto",
      });
      items.push({ kind: "agent", id: `agent-${index}`, t: index, text: `answer ${index}`, streaming: false });
    }

    const cache = new TimelinePresentationCache();
    cache.reset(items);
    const before = cache.window(80);
    const stableTurns = before.turns.slice(0, -1);
    let nextId = 0;
    let worstMutation = 0;
    const started = performance.now();
    for (let index = 0; index < 1_000; index += 1) {
      const tokenStart = performance.now();
      const mutation = applyAgentEventInPlace(
        items,
        { type: "message", text: "x", delta: true },
        5_001,
        () => `live-${nextId += 1}`,
      );
      cache.update(items, mutation.startIndex);
      cache.window(80);
      worstMutation = Math.max(worstMutation, performance.now() - tokenStart);
    }
    const elapsed = performance.now() - started;
    const after = cache.window(80);
    return {
      elapsed,
      worstMutation,
      stableTurnCount: after.turns.slice(0, -1).filter((turn, index) => turn === stableTurns[index]).length,
      totalTurns: after.totalTurns,
      mountedTurns: after.turns.length,
      tailLength: after.turns.at(-1).items.at(-1).text.length,
    };
  });

  assert.equal(result.stableTurnCount, 79, "all mounted completed turns must survive token updates");
  assert.equal(result.totalTurns, 5_000);
  assert.equal(result.mountedTurns, 80);
  assert.equal(result.tailLength, 1_000);
  assert.ok(result.elapsed < 1_000, `1,000 token updates took ${result.elapsed.toFixed(1)}ms`);
  assert.ok(result.worstMutation < 100, `one token update took ${result.worstMutation.toFixed(1)}ms`);
  await page.close();
});

test("incremental presentation remains output-equivalent across tail transitions", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const result = await page.evaluate(async () => {
    const { applyAgentEventInPlace } = await import("/src/components/room/model.ts");
    const { buildTimelineTurns, TimelinePresentationCache } = await import("/src/components/room/timeline-presentation.ts");
    const items = [{
      kind: "you", id: "user", t: 0, text: "go", imageCount: 0,
      imageMediaTypes: [], attachments: [], sendMode: "auto",
    }];
    const cache = new TimelinePresentationCache();
    cache.reset(items);
    let nextId = 0;
    const events = [
      { type: "message", text: "draft", delta: true },
      { type: "tool_call", name: "read_file", input: { path: "a.ts" }, call_id: "call-a" },
      { type: "tool_result", call_id: "call-a", output: "ok", is_error: false },
      { type: "message", text: "done", delta: true },
      { type: "plan", steps: [{ description: "ship", completed: true }] },
    ];
    const snapshots = [];
    for (const event of events) {
      const mutation = applyAgentEventInPlace(items, event, 1, () => `live-${nextId += 1}`);
      cache.update(items, mutation.startIndex);
      snapshots.push(JSON.stringify(cache.window(100).turns) === JSON.stringify(buildTimelineTurns(items)));
    }
    const planReplacement = applyAgentEventInPlace(
      items,
      { type: "plan", steps: [{ description: "ship", completed: false }] },
      1,
      () => `live-${nextId += 1}`,
    );
    cache.update(items, planReplacement.startIndex);
    snapshots.push(JSON.stringify(cache.window(100).turns) === JSON.stringify(buildTimelineTurns(items)));
    const beforeNewTurn = cache.window(100);
    const startIndex = items.length;
    items.push({
      kind: "you", id: "next-user", t: 2, text: "next", imageCount: 0,
      imageMediaTypes: [], attachments: [], sendMode: "auto",
    });
    cache.update(items, startIndex);
    const afterNewTurn = cache.window(100);

    const queued = [{
      kind: "you", id: "queued-only", t: 3, text: "later", imageCount: 0,
      imageMediaTypes: [], attachments: [], sendMode: "queue", queuedState: "queued",
    }];
    const queuedCache = new TimelinePresentationCache();
    queuedCache.reset(queued);
    const acceptedSteer = [{
      kind: "you", id: "accepted-steer", t: 4, text: "改为重新生成", imageCount: 0,
      imageMediaTypes: [], attachments: [], sendMode: "steer",
    }];
    return {
      snapshots,
      beforeCount: beforeNewTurn.totalTurns,
      afterCount: afterNewTurn.totalTurns,
      lastUser: afterNewTurn.turns.at(-1).user?.id,
      newTurnEquivalent: JSON.stringify(afterNewTurn.turns) === JSON.stringify(buildTimelineTurns(items)),
      queuedCache: queuedCache.window(100),
      queuedFull: buildTimelineTurns(queued),
      acceptedSteer: buildTimelineTurns(acceptedSteer),
    };
  });

  assert.deepEqual(result.snapshots, [true, true, true, true, true, true]);
  assert.equal(result.afterCount, result.beforeCount + 1);
  assert.equal(result.lastUser, "next-user");
  assert.equal(result.newTurnEquivalent, true);
  assert.deepEqual(result.queuedCache, { turns: [], totalTurns: 0 });
  assert.deepEqual(result.queuedFull, []);
  assert.equal(
    result.acceptedSteer.at(-1)?.user?.id,
    "accepted-steer",
    "an accepted in-flight guidance message must remain visible as a user action",
  );
  await page.close();
});
