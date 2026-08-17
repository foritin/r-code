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
    const candidate = net.createServer();
    candidate.once("error", reject);
    candidate.listen(0, "127.0.0.1", () => {
      const address = candidate.address();
      const port = typeof address === "object" && address ? address.port : 0;
      candidate.close((error) => error ? reject(error) : resolve(port));
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

test("运行护栏设置按字段往返保存", async () => {
  const page = await browser.newPage({ viewport: { width: 1200, height: 900 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const { useAppStore } = await import("/src/store/app.ts");
    useAppStore.getState().setScene("settings");
    useAppStore.getState().setSettingsPane("agents");
  });
  const sheet = page.locator(".settings-sheet");
  await sheet.getByRole("heading", { name: "运行护栏" }).waitFor({ state: "visible" });

  const rounds = page.locator("#set-budget-rounds");
  assert.equal(await rounds.inputValue(), "60");
  await rounds.fill("42");
  const replay = page.locator("#set-budget-replay");
  assert.equal(await replay.isChecked(), true);
  await replay.uncheck();

  const saved = await page.evaluate(async () => {
    const { settingsGet } = await import("/src/lib/ipc.ts");
    const response = await settingsGet(true);
    return {
      max_tool_rounds: response.config.orchestration?.run_budget?.max_tool_rounds,
      replay_detection: response.config.orchestration?.run_budget?.replay_detection,
    };
  });
  assert.equal(saved.max_tool_rounds, 42);
  assert.equal(saved.replay_detection, false);
  await page.close();
});

test("护栏与检查点事件进入时间线上下文", async () => {
  const page = await browser.newPage({ viewport: { width: 1000, height: 700 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const items = await page.evaluate(async () => {
    const { applyAgentEvent } = await import("/src/components/room/model.ts");
    const afterTrip = applyAgentEvent(
      [],
      { type: "guard_trip", reason: "no_progress", detail: "连续 24 个工具轮没有成功变更" },
      1,
      () => "g1",
    );
    const afterCheckpoint = applyAgentEvent(
      afterTrip,
      { type: "checkpoint", sha: "0123456789abcdef" },
      2,
      () => "c1",
    );
    return afterCheckpoint.map((item) => ({
      kind: item.kind,
      label: item.kind === "context" ? item.label : null,
      detail: item.kind === "context" ? item.detail : null,
    }));
  });
  assert.equal(items.length, 2);
  assert.equal(items[0].label, "护栏触发 · 持续调用但无进展");
  assert.equal(items[0].detail, "连续 24 个工具轮没有成功变更");
  assert.equal(items[1].label, "已保存绿灯检查点");
  assert.match(items[1].detail ?? "", /01234567/);
  await page.close();
});
