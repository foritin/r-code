// M1a-08（PRD FR-1 /context）前端投影合同：
//   - r_code_context_instructions / r_code_context_jit(_dropped) 时间线行
//     紧凑可扫描、带 /context 指引与路径明细；
//   - 斜杠命令分裂：/context 为引擎查询（requiresWorkspace），/status 保留
//     旧本地状态视图。

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
        .map((entry) =>
          process.platform === "darwin"
            ? path.join(playwrightCache, entry, "chrome-mac", "Chromium.app", "Contents", "MacOS", "Chromium")
            : path.join(playwrightCache, entry, "chrome-linux", "chrome"),
        )
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

test("m1a_08 context injection rows render compact guidance with /context pointer", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl);
  const rows = await page.evaluate(async () => {
    const { codexContextRow } = await import("/src/components/room/model.ts");
    return {
      injected: codexContextRow("r_code_context_instructions", { injected: 3, bytes: 4321 }),
      jit: codexContextRow("r_code_context_jit", { applied: 1, bytes: 90, paths: ["crates/x/AGENTS.md"] }),
      dropped: codexContextRow("r_code_context_jit_dropped", {
        reason: "jit-allowance-exceeded",
        applied: 0,
        paths: ["docs/y/AGENTS.md"],
      }),
      unknown: codexContextRow("r_code_context_instructions", {}),
    };
  });
  await page.close();

  assert.ok(rows.injected, "instructions row must render");
  assert.match(rows.injected.label, /已注入 3 条项目指令/);
  assert.match(rows.injected.detail, /\/context/);
  assert.match(rows.jit.label, /已注入子目录指令 1 条/);
  assert.match(rows.jit.detail, /crates\/x\/AGENTS\.md/);
  assert.match(rows.dropped.label, /放弃/);
  assert.match(rows.dropped.label, /JIT 额度/);
  assert.ok(rows.dropped.collapsible, "dropped batch detail stays collapsible");
});

test("m1a_08 slash split keeps /status local and /context as the engine query", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl);
  const result = await page.evaluate(async () => {
    const { SLASH_COMMANDS, parseSlashCommand } = await import("/src/lib/slash-commands.ts");
    const context = SLASH_COMMANDS.find((command) => command.name === "context");
    const status = SLASH_COMMANDS.find((command) => command.name === "status");
    return {
      context,
      status,
      parsedContext: parseSlashCommand("/context"),
      parsedStatus: parseSlashCommand("/status"),
    };
  });
  await page.close();

  assert.ok(result.context, "/context must stay registered");
  assert.equal(result.context.kind, "local");
  assert.equal(result.context.requiresWorkspace, true, "/context needs a workspace");
  assert.ok(!result.context.aliases?.includes("status"), "old status alias must be gone");
  assert.ok(result.status, "/status keeps the legacy local view");
  assert.notEqual(result.status.requiresWorkspace, true, "/status must not require a workspace");
  assert.equal(result.parsedContext?.rawName, "context");
    assert.equal(result.parsedContext?.command?.name, "context");
  assert.equal(result.parsedStatus?.rawName, "status");
    assert.equal(result.parsedStatus?.command?.name, "status");
});
