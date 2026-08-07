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

test("runUsageLabel keeps legacy behavior when cache fields are absent", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const out = await page.evaluate(async () => {
    const { runUsageLabel } = await import("/src/components/room/Timeline.tsx");
    return {
      empty: runUsageLabel(null),
      blank: runUsageLabel(""),
      invalid: runUsageLabel("not-json"),
      jsonNull: runUsageLabel("null"),
      emptyObject: runUsageLabel("{}"),
      both: runUsageLabel('{"input_tokens":1200,"output_tokens":420}'),
      inputOnly: runUsageLabel('{"input_tokens":7}'),
      outputOnly: runUsageLabel('{"output_tokens":7}'),
      stringValues: runUsageLabel('{"input_tokens":"1200","output_tokens":"420"}'),
    };
  });
  assert.equal(out.empty, null);
  assert.equal(out.blank, null);
  assert.equal(out.invalid, null);
  assert.equal(out.jsonNull, null);
  assert.equal(out.emptyObject, null);
  assert.equal(out.both, "输入 1,200 · 输出 420");
  assert.equal(out.inputOnly, "输入 7");
  assert.equal(out.outputOnly, "输出 7");
  assert.equal(out.stringValues, null, "non-number values must be treated as missing");
  await page.close();
});

test("runUsageLabel shows cache hit tokens and ratio when cache fields exist", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const out = await page.evaluate(async () => {
    const { runUsageLabel } = await import("/src/components/room/Timeline.tsx");
    return {
      hitAndMiss: runUsageLabel(
        '{"input_tokens":1200,"output_tokens":420,"cache_read_tokens":900,"cache_write_tokens":300}'
      ),
      hitOnly: runUsageLabel('{"input_tokens":1200,"cache_read_tokens":900}'),
      missOnly: runUsageLabel('{"input_tokens":1200,"cache_write_tokens":300}'),
      zeroBoth: runUsageLabel('{"input_tokens":10,"cache_read_tokens":0,"cache_write_tokens":0}'),
      zeroHit: runUsageLabel('{"input_tokens":10,"cache_read_tokens":0,"cache_write_tokens":10}'),
      noInOut: runUsageLabel('{"cache_read_tokens":900,"cache_write_tokens":300}'),
      rounding: runUsageLabel('{"cache_read_tokens":1,"cache_write_tokens":2}'),
      nullValues: runUsageLabel('{"input_tokens":5,"cache_read_tokens":null,"cache_write_tokens":null}'),
      huge: runUsageLabel(
        '{"input_tokens":1000000,"cache_read_tokens":999999,"cache_write_tokens":1}'
      ),
    };
  });
  assert.equal(out.hitAndMiss, "输入 1,200 · 输出 420 · 命中 900 (75%)");
  assert.equal(out.hitOnly, "输入 1,200 · 命中 900", "ratio omitted when miss count is absent");
  assert.equal(out.missOnly, "输入 1,200 · 命中 0", "missing hit count defaults to zero");
  assert.equal(out.zeroBoth, "输入 10 · 命中 0", "zero total has no ratio");
  assert.equal(out.zeroHit, "输入 10 · 命中 0 (0%)");
  assert.equal(out.noInOut, "命中 900 (75%)");
  assert.equal(out.rounding, "命中 1 (33%)", "ratio rounds to nearest percent");
  assert.equal(out.nullValues, "输入 5", "JSON null cache values keep legacy behavior");
  assert.equal(out.huge, "输入 1,000,000 · 命中 999,999 (100%)");
  await page.close();
});
