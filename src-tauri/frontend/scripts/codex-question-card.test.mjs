// M3-02（PRD §10）：可访问的 Codex 问题卡。
//   A1：普通/其他/secret 输入编码正确；secret 不回显、answered 摘要不含原值。
//   A2：仅键盘完成选择/输入/提交；resolved 后同 turn 继续运行事件可见。
//   A3：1280×800 与 390×844 无横向溢出；状态可读屏识别（aria-live）。

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

const QUESTIONS = [
  {
    id: "scope",
    header: "范围",
    question: "本次处理哪一部分？",
    is_other: true,
    is_secret: false,
    options: [
      { label: "当前模块", description: "限制变更范围" },
      { label: "整个工作区", description: "" },
    ],
  },
  {
    id: "token",
    header: "凭据",
    question: "访问密钥？",
    is_other: false,
    is_secret: true,
    options: [],
  },
];

async function mountCard(page, { state = "pending", onSubmitRecorder = "recorder" } = {}) {
  await page.evaluate(async ({ questions, state, recorder }) => {
    document.body.innerHTML = "";
    const React = (await import("/@id/react")).default;
    const mod = await import("/@id/react-dom/client");
    const createRoot = mod.createRoot ?? mod.default?.createRoot;
    const { CodexQuestionCard } = await import("/src/components/room/CodexQuestionCard.tsx");
    const container = document.createElement("div");
    container.id = "m3-02-mount";
    container.style.maxWidth = "720px";
    document.body.appendChild(container);
    globalThis.__m3_02_submissions = [];
    const submit = async (answers) => {
      globalThis.__m3_02_submissions.push(answers);
      return recorder === "reject" ? "rejected" : "delivered";
    };
    createRoot(container).render(
      React.createElement(CodexQuestionCard, {
        questions,
        state,
        answerSummary: [],
        onSubmit: submit,
      }),
    );
  }, { questions: QUESTIONS, state, recorder: onSubmitRecorder });
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

test("m3_02_a1 answer encoding covers options, other text and secret without echoing secrets", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  // 1) 纯编码函数：选项 + 其他文本 + secret 全部进入 {qid: [answers]}。
  const encoded = await page.evaluate(async ({ questions }) => {
    const { encodeUserInputAnswers, summarizeAnswers } = await import("/src/components/room/CodexQuestionCard.tsx");
    const answers = encodeUserInputAnswers(questions, {
      scope: { optionLabels: ["当前模块"], text: "顺带看看测试" },
      token: { optionLabels: [], text: "sk-secret-plain" },
    });
    return { answers, summary: summarizeAnswers(questions, answers) };
  }, { questions: QUESTIONS });
  assert.deepEqual(encoded.answers.scope, ["当前模块", "顺带看看测试"]);
  assert.deepEqual(encoded.answers.token, ["sk-secret-plain"]);
  assert.ok(encoded.summary.some((line) => line.includes("已安全提交")), "secret summary never carries the value");
  assert.ok(!encoded.summary.join("\n").includes("sk-secret-plain"));

  // 2) secret 输入为密码框；answered 卡片的 DOM 不含已输入原值。
  await mountCard(page, { state: "answered" });
  const dom = await page.evaluate(() => {
    const mount = document.getElementById("m3-02-mount");
    return { text: mount?.textContent ?? "", passwordCount: mount?.querySelectorAll('input[type="password"]').length ?? 0 };
  });
  assert.equal(dom.passwordCount, 1, "secret input renders as a password field");
  await mountCard(page, { state: "pending" });
  await page.locator('input[type="password"]').fill("sk-typed-secret-value");
  const typedVisibility = await page.evaluate(() => {
    const input = document.querySelector('input[type="password"]');
    return { type: input?.getAttribute("type"), valueInDom: (document.getElementById("m3-02-mount")?.textContent ?? "").includes("sk-typed-secret-value") };
  });
  assert.equal(typedVisibility.type, "password");
  assert.equal(typedVisibility.valueInDom, false, "typed secret value must never appear as DOM text");
  await page.close();
});

test("m3_02_a2 keyboard-only flow submits answers and the turn continues visibly", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await mountCard(page, {});
  const card = page.locator("#m3-02-mount .codex-question-card");
  await card.waitFor({ state: "visible" });

  // 未答满：提交禁用。
  assert.equal(await page.locator(".codex-question-submit").isDisabled(), true);

  // 仅键盘：Tab 到第一个选项 checkbox，Space 选中；再 Tab 越过第二个
  // checkbox 到自定义回答输入框，输入其他答案。
  await page.keyboard.press("Tab"); // 第一个 fieldset 的 checkbox
  await page.keyboard.press("Space");
  await page.keyboard.press("Tab"); // 第二个 checkbox
  await page.keyboard.press("Tab"); // 自定义回答输入框
  await page.keyboard.type("顺带看看测试");
  // Tab 到 secret 密码框，输入密钥。
  await page.keyboard.press("Tab");
  await page.keyboard.type("sk-kbd-secret");
  assert.equal(await page.locator(".codex-question-submit").isDisabled(), false, "all questions answered unlocks submit");
  // Tab 到提交按钮，Enter 提交。
  await page.keyboard.press("Tab");
  await page.keyboard.press("Enter");
  await page.waitForFunction(() => globalThis.__m3_02_submissions?.length === 1);
  const submission = await page.evaluate(() => globalThis.__m3_02_submissions[0]);
  assert.deepEqual(submission.scope, ["当前模块", "顺带看看测试"]);
  assert.deepEqual(submission.token, ["sk-kbd-secret"]);

  // resolved 事件 + 同 turn 继续运行：reducer 层验证（question → answered →
  // 后续 agent 消息原位追加，即“继续运行”对用户可见）。
  const reducer = await page.evaluate(async ({ questions }) => {
    const { applyAgentEventInPlace, markQuestionAnsweredInPlace } = await import("/src/components/room/model.ts");
    const items = [];
    let nextId = 0;
    const nid = () => `live-${(nextId += 1)}`;
    applyAgentEventInPlace(items, {
      type: "codex_user_input_requested",
      run_id: "run-q",
      request_key: "run-q:item_q",
      item_id: "item_q",
      request_id: "41",
      questions,
      auto_resolution_ms: null,
    }, 1, nid);
    const marked = markQuestionAnsweredInPlace(items, "run-q:item_q", questions, {
      scope: ["当前模块"],
      token: ["sk-reducer-secret"],
    });
    const resolved = applyAgentEventInPlace(items, {
      type: "codex_user_input_resolved",
      request_key: "run-q:item_q",
      item_id: "item_q",
      outcome: "answered",
    }, 1, nid);
    applyAgentEventInPlace(items, {
      type: "codex_agent_message",
      item_id: "f1",
      phase: "final_answer",
      text: "已按当前模块继续处理。",
      delta: true,
    }, 1, nid);
    applyAgentEventInPlace(items, {
      type: "codex_agent_message",
      item_id: "f1",
      phase: "final_answer",
      text: "",
      delta: false,
    }, 1, nid);
    const card = items.find((item) => item.kind === "question");
    const final = items.find((item) => item.kind === "agent");
    return {
      markedChanged: marked.changed,
      resolvedChanged: resolved.changed,
      cardState: card?.state,
      summary: card?.answerSummary ?? [],
      finalText: final?.text,
      serializedHasSecret: JSON.stringify(items).includes("sk-reducer-secret"),
    };
  }, { questions: QUESTIONS });
  assert.equal(reducer.markedChanged, true);
  assert.equal(reducer.resolvedChanged, true, "resolved event is processed");
  assert.equal(reducer.cardState, "answered");
  assert.ok(reducer.summary.some((line) => line === "凭据：已安全提交"));
  assert.ok(!reducer.summary.join("|").includes("sk-reducer-secret"));
  assert.equal(reducer.finalText, "已按当前模块继续处理。", "turn continuation is visible after answering");
  await page.close();
});

test("m3_02_a3 question card fits 1280x800 and 390x844 with screen-reader status", async () => {
  const page = await browser.newPage({ viewport: { width: 1280, height: 800 } });
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await mountCard(page, {});
  const desktop = await page.evaluate(() => ({
    scrollWidth: document.documentElement.scrollWidth,
    liveRegions: document.querySelectorAll('[role="status"][aria-live="polite"]').length,
    labelled: document.querySelectorAll("section[aria-label]").length,
  }));
  assert.equal(desktop.liveRegions >= 1, true, "state changes announced via aria-live");
  assert.equal(desktop.labelled >= 1, true, "card carries an accessible name");
  assert.ok(desktop.scrollWidth <= 1280, `no overflow at 1280, got ${desktop.scrollWidth}`);

  await page.setViewportSize({ width: 390, height: 844 });
  const mobile = await page.evaluate(() => ({
    scrollWidth: document.documentElement.scrollWidth,
  }));
  assert.ok(mobile.scrollWidth <= 390, `no overflow at 390, got ${mobile.scrollWidth}`);

  // 暗色主题（obsidian）：同样无横向溢出。
  await page.evaluate(() => {
    document.documentElement.setAttribute("data-theme", "obsidian");
  });
  const dark = await page.evaluate(() => ({ scrollWidth: document.documentElement.scrollWidth }));
  assert.ok(dark.scrollWidth <= 390, `no overflow in dark theme, got ${dark.scrollWidth}`);
  await page.close();
});

test("m3_03_a3 history rebuild keeps live pending answerable and expired read-only", async () => {
  const page = await browser.newPage();
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  const out = await page.evaluate(async () => {
    const { buildTimeline, markQuestionStateInPlace } = await import("/src/components/room/model.ts");
    const mkMsg = (id, key, runId, state, secret) => ({
      id,
      branch_id: "b",
      kind: "codex_question",
      text: key,
      output_json: JSON.stringify({
        request_key: key,
        run_id: runId,
        questions: [
          {
            id: "q1",
            header: secret ? "凭据" : "范围",
            question: secret ? "密钥？" : "哪部分？",
            is_other: false,
            is_secret: secret,
            options: [],
          },
        ],
        state,
      }),
    });
    // 后端在 run 存活时保留 pending（可答）；run 终止后重建层把 state 改为
    // expired（模拟 session_messages_for_task 的裁决）。
    const history = buildTimeline(
      [
        { id: "s:1", branch_id: "b", kind: "message", role: "user", text: "开始" },
        mkMsg("s:2", "run-live:item_a", "run-live", "pending", false),
        mkMsg("s:3", "run-dead:item_b", "run-dead", "expired", true),
      ],
      [],
      [],
      "2026-08-25T00:00:00.000Z"
    );
    const cards = history.filter((item) => item.kind === "question");
    // 只读判定与前端卡片组件一致：state !== pending → fieldset disabled。
    const states = cards.map((card) => card.state);
    // 本地状态直写（delivered 后）在重建条目上同样幂等。
    const items = [...history];
    const mutation = markQuestionStateInPlace(items, "run-live:item_a", "answered");
    const answered = items.find((item) => item.kind === "question" && item.requestKey === "run-live:item_a");
    return {
      cardCount: cards.length,
      states,
      mutationChanged: mutation.changed,
      answeredState: answered?.state,
      secretQuestion: cards[1]?.questions[0]?.is_secret ?? null,
    };
  });
  assert.equal(out.cardCount, 2, "both question markers rebuild");
  assert.deepEqual(out.states, ["pending", "expired"]);
  assert.equal(out.mutationChanged, true);
  assert.equal(out.answeredState, "answered");
  assert.equal(out.secretQuestion, true, "secret flag survives rebuild");
  await page.close();
});
