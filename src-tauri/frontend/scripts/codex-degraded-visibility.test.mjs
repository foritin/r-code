// M4 子代理可靠性：降级标签解析 + run 行接线断言。
// 说明：usage_json→标签的语义由标签函数全覆盖；run 行的 JSX 接线
// （计算、徽章类名、aria status）以模块源码合同锁定，防止静默回归。

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

test("m4_degraded_label maps all reasons and rejects junk", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const out = await page.evaluate(async () => {
    const { runDegradedLabel } = await import("/src/components/room/Timeline.tsx");
    return {
      budget: runDegradedLabel('{"degraded_reason":"tool_budget"}'),
      loop: runDegradedLabel('{"degraded_reason":"loop_guard"}'),
      unknown: runDegradedLabel('{"degraded_reason":"future_reason"}'),
      none: runDegradedLabel('{"input_tokens":5}'),
      null: runDegradedLabel(null),
      invalid: runDegradedLabel("not-json"),
    };
  });
  assert.equal(out.budget, "工具预算耗尽");
  assert.equal(out.loop, "循环护栏触发");
  assert.equal(out.unknown, "降级运行");
  assert.equal(out.none, null);
  assert.equal(out.null, null);
  assert.equal(out.invalid, null);
  await page.close();
});

test("m4_run_row_wires_degraded_badge_into_the_run_row", () => {
  // 渲染接线：runDegradedLabel 计算 + 徽章类名 + aria status 三点锁定
  // （直接对源文件断言，不经 vite 转译）。
  const source = fs.readFileSync(
    path.join(frontendDir, "src", "components", "room", "Timeline.tsx"),
    "utf8",
  );
  assert.equal(
    source.includes("const degraded = runDegradedLabel(it.usageJson)"),
    true,
    "run row computes the degraded label",
  );
  assert.equal(source.includes("run-status-degraded"), true, "badge uses the degraded status class");
  assert.equal(
    source.includes('role="status" title="完成但质量受损，结论需复核'),
    true,
    "badge is an aria status region",
  );
  assert.equal(source.includes("export function runDegradedLabel"), true);
});
