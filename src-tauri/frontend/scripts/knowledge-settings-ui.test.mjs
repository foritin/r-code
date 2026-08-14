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
const shotDir = process.env.R_CODE_KNOWLEDGE_SHOT_DIR;

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

async function newKnowledgePage({ width, height, theme, tab, project = false }) {
  const page = await browser.newPage({ viewport: { width, height } });
  await page.addInitScript((nextTheme) => {
    window.localStorage.setItem("r-code.theme.mode", nextTheme);
  }, theme);
  await page.goto(baseUrl, { waitUntil: "networkidle" });
  await page.evaluate(async ({ targetTab, useProject }) => {
    const { useAppStore } = await import("/src/store/app.ts");
    const { useTasksStore } = await import("/src/store/tasks.ts");
    await useTasksStore.getState().refreshWorkspaces();
    useTasksStore.getState().setCurrentProject(useProject ? "D:/project/rust/r-code" : null);
    useAppStore.getState().openKnowledge(targetTab);
  }, { targetTab: tab, useProject: project });
  const center = page.getByRole("region", { name: "知识与指令" });
  await center.waitFor({ state: "visible" });
  await page.evaluate(() => document.fonts.ready);
  assert.equal(await page.locator("html").getAttribute("data-theme"), theme === "light" ? "studio-light" : "obsidian");
  return { page, center };
}

async function assertResponsiveFrame(page, center) {
  const frame = await page.evaluate(() => {
    const scene = document.querySelector("#app.scene-settings .scene-scroll");
    const settings = document.querySelector(".settings-detail");
    const knowledge = document.querySelector(".knowledge-settings");
    const layout = document.querySelector(".knowledge-layout");
    const rect = knowledge?.getBoundingClientRect();
    return {
      viewportWidth: window.innerWidth,
      documentOverflow: document.documentElement.scrollWidth - document.documentElement.clientWidth,
      sceneOverflow: scene ? scene.scrollWidth - scene.clientWidth : null,
      settingsOverflow: settings ? settings.scrollWidth - settings.clientWidth : null,
      knowledgeOverflow: knowledge ? knowledge.scrollWidth - knowledge.clientWidth : null,
      layoutOverflow: layout ? layout.scrollWidth - layout.clientWidth : null,
      left: rect?.left ?? null,
      right: rect?.right ?? null,
    };
  });
  assert.ok(frame.documentOverflow <= 1, `document overflowed by ${frame.documentOverflow}px`);
  assert.ok((frame.sceneOverflow ?? 0) <= 1, `settings scene overflowed by ${frame.sceneOverflow}px`);
  assert.ok((frame.settingsOverflow ?? 0) <= 1, `settings detail overflowed by ${frame.settingsOverflow}px`);
  assert.ok((frame.knowledgeOverflow ?? 0) <= 1, `knowledge pane overflowed by ${frame.knowledgeOverflow}px`);
  assert.ok((frame.layoutOverflow ?? 0) <= 1, `knowledge layout overflowed by ${frame.layoutOverflow}px`);
  assert.ok((frame.left ?? -1) >= 0 && (frame.right ?? frame.viewportWidth + 1) <= frame.viewportWidth + 1);
  await center.getByRole("tab", { name: "记忆", exact: true }).waitFor({ state: "visible" });
  await center.getByRole("tab", { name: "协作 Prompt", exact: true }).waitFor({ state: "visible" });
  await center.getByRole("tab", { name: "Skills", exact: true }).waitFor({ state: "visible" });
}

async function screenshot(page, name) {
  if (!shotDir) return;
  fs.mkdirSync(shotDir, { recursive: true });
  await page.screenshot({ path: path.join(shotDir, `${name}.png`), fullPage: false });
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

test("knowledge settings keep navigation and memory legible across target viewports and themes", async () => {
  const cases = [
    { width: 1440, height: 900, theme: "light" },
    { width: 1440, height: 900, theme: "dark" },
    { width: 1024, height: 768, theme: "light" },
    { width: 1024, height: 768, theme: "dark" },
    { width: 800, height: 720, theme: "light" },
    { width: 800, height: 720, theme: "dark" },
  ];

  for (const item of cases) {
    const { page, center } = await newKnowledgePage({ ...item, tab: "memory", project: item.width <= 1024 });
    try {
      await assertResponsiveFrame(page, center);
      const scopeName = item.width <= 1024 ? "r-code" : "全局";
      assert.equal(await center.getByRole("button", { name: scopeName, exact: true }).getAttribute("aria-pressed"), "true");
      await center.getByRole("heading", { name: item.width <= 1024 ? "r-code 的项目记忆" : "全局记忆", exact: true }).waitFor({ state: "visible" });
      await screenshot(page, `knowledge-memory-${item.width}x${item.height}-${item.theme}`);
    } finally {
      await page.close();
    }
  }
});

test("project Prompt uses an explicit keyboard-operable append or override choice", async () => {
  const { page, center } = await newKnowledgePage({ width: 1024, height: 768, theme: "dark", tab: "prompts", project: true });
  try {
    const append = center.getByRole("button", { name: /追加/ });
    const override = center.getByRole("button", { name: /覆盖/ });
    await append.waitFor({ state: "visible" });
    assert.equal(await append.getAttribute("aria-pressed"), "true");
    await override.focus();
    await page.keyboard.press("Enter");
    assert.equal(await override.getAttribute("aria-pressed"), "true");
    assert.equal(await append.getAttribute("aria-pressed"), "false");
    await center.getByRole("textbox", { name: "主 Agent", exact: true }).fill("先读取项目约束，再决定是否委派。" );
    await center.getByRole("textbox", { name: "子代理", exact: true }).fill("只返回验证过的结论与文件清单。" );
    await assertResponsiveFrame(page, center);
    await screenshot(page, "knowledge-prompt-1024x768-dark");
  } finally {
    await page.close();
  }
});

test("project Skill editing and promotion feedback remain visible in the narrow layout", async () => {
  const skillName = "visual-scope-check";
  const workspacePath = "D:/project/rust/r-code";
  const { page, center } = await newKnowledgePage({ width: 800, height: 720, theme: "light", tab: "skills", project: true });
  try {
    await center.getByRole("button", { name: "新建项目 Skill", exact: true }).click();
    await center.getByRole("textbox", { name: "调用名", exact: true }).fill(skillName);
    await center.getByRole("textbox", { name: "简介", exact: true }).fill("验证项目 Skill 的窄窗口编辑与同步反馈");
    await center.getByRole("textbox", { name: "Skill 指令", exact: true }).fill("先验证，再输出简洁结论。" );
    await assertResponsiveFrame(page, center);
    await screenshot(page, "knowledge-skill-editor-800x720-light");

    await center.getByRole("button", { name: "保存 Skill", exact: true }).click();
    await center.getByText(`项目 Skill /${skillName} 已保存，仅在 r-code 中可用。`, { exact: true }).waitFor({ state: "visible" });
    await center.getByRole("button", { name: "同步到全局", exact: true }).click();
    await center.getByText(`/${skillName} 已同步到全局；项目副本已移除，当前项目改为自动继承。`, { exact: true }).waitFor({ state: "visible" });
    await center.getByRole("button", { name: new RegExp(`/${skillName}`) }).waitFor({ state: "visible" });
    await assertResponsiveFrame(page, center);
    await screenshot(page, "knowledge-skill-synced-800x720-light");
  } finally {
    await page.evaluate(async ({ path: currentPath, name }) => {
      const { workflowSkillDelete, workflowSkillsList } = await import("/src/lib/ipc.ts");
      const skill = (await workflowSkillsList()).find((item) => item.name === name);
      if (skill) await workflowSkillDelete(skill.id, "global", currentPath);
    }, { path: workspacePath, name: skillName }).catch(() => {});
    await page.close();
  }
});
