// M4-02.A2 视觉证据：完整 commentary → 工具 → 提问(已回答) → final 的
// 真实 Timeline 渲染，亮/暗主题与 1280×800 视口，顺序与层次断言。

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

async function mountFullHistory(page) {
  await page.evaluate(async () => {
    globalThis.__TAURI_INTERNALS__ = {
      invoke: async (command) => {
        if (command === "cmd_session_messages_for_branch") {
          return [
            { id: "s:1", branch_id: "b", kind: "message", role: "user", text: "修一下失败的测试" },
            { id: "s:2", branch_id: "b", kind: "codex_commentary", role: "assistant", text: "先跑一遍测试，确认失败面。" },
            {
              id: "s:3",
              branch_id: "b",
              kind: "tool_call",
              tool_name: "Codex 命令",
              call_id: "cmd-1",
              input_json: JSON.stringify({ summary: "npm test" }),
            },
            {
              id: "s:4",
              branch_id: "b",
              kind: "tool_result",
              call_id: "cmd-1",
              output_json: JSON.stringify({ status: "failed", exit_code: 1, output: "2 failing" }),
              is_error: true,
            },
            {
              id: "s:5",
              branch_id: "b",
              kind: "codex_question",
              text: "run-1:item_q",
              output_json: JSON.stringify({
                request_key: "run-1:item_q",
                run_id: "run-1",
                questions: [
                  {
                    id: "regress",
                    header: "修复方式",
                    question: "怎么处理？",
                    is_other: false,
                    is_secret: false,
                    options: [{ label: "只修测试", description: "" }],
                  },
                ],
                state: "answered",
              }),
            },
            { id: "s:6", branch_id: "b", kind: "message", role: "assistant", text: "已按最小范围修复，测试全绿。" },
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
    container.id = "m4-02-mount";
    container.style.height = "760px";
    container.style.overflow = "auto";
    document.getElementById("root").appendChild(container);
    createRoot(container).render(
      React.createElement(Timeline, {
        taskId: "task-m4-02",
        branchId: "branch-m4-02",
        workspacePath: null,
        cur: null,
        running: false,
        reviewing: false,
      }),
    );
  });
  await page.waitForSelector("#m4-02-mount .agent", { timeout: 10_000 });
}

test("m4_02_a2 full flow timeline renders ordered layers in light and dark themes", async () => {
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await mountFullHistory(page);

  const collect = () =>
    page.evaluate(() => {
      const mount = document.getElementById("m4-02-mount");
      // 文档序联合查询：commentary/final/活动/计划/问题卡的相对顺序。
      const nodes = [
        ...mount.querySelectorAll(
          ".agent, .codex-question-card, .todo-card, .timeline-activity-event"
        ),
      ];
      const layers = nodes.map((node) => {
        const className = String(node.className);
        if (className.includes("codex-question-card")) return "question";
        if (className.includes("agent")) {
          return className.includes("timeline-progress-update") ? "commentary" : "final";
        }
        if (className.includes("todo-card")) return "plan";
        return "activity";
      });
      const agentNodes = mount.querySelectorAll(".agent").length;
      const questionCards = mount.querySelectorAll(".codex-question-card").length;
      const answeredCards = mount.querySelectorAll(".codex-question-card.state-answered").length;
      const authorHeaders = [...mount.querySelectorAll(".agent")].filter((node) =>
        node.querySelector(":scope > .who")
      ).length;
      const scrollWidth = document.documentElement.scrollWidth;
      return { layers, agentNodes, questionCards, answeredCards, authorHeaders, scrollWidth };
    });

  const light = await collect();
  // 顺序：用户 → commentary → 活动分组 → 问题卡 → final（时间线轮次折叠下，
  // 关键五类必须都在且 commentary 出现在 final 之前）。
  assert.ok(light.layers.includes("commentary"), "commentary layer present");
  assert.ok(light.layers.includes("activity") || light.layers.includes("other"), "tool activity present");
  assert.equal(light.questionCards, 1);
  assert.equal(light.answeredCards, 1, "question card is terminal read-only answered");
  assert.ok(light.layers.indexOf("commentary") < light.layers.indexOf("final") || light.layers.lastIndexOf("commentary") < light.layers.lastIndexOf("final"), "commentary precedes final");
  assert.equal(light.authorHeaders, 1, "only the final answer carries the author header");
  assert.ok(light.scrollWidth <= 1280, `light theme no overflow: ${light.scrollWidth}`);

  // 暗色主题。
  await page.evaluate(() => {
    document.documentElement.setAttribute("data-theme", "obsidian");
  });
  const dark = await collect();
  assert.equal(dark.questionCards, 1);
  assert.ok(dark.scrollWidth <= 1280, `dark theme no overflow: ${dark.scrollWidth}`);
  await page.close();
});
