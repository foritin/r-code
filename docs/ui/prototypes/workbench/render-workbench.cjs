const fs = require("fs");
const crypto = require("crypto");
const path = require("path");
const { createRequire } = require("module");
const { pathToFileURL } = require("url");
const { sidebarExperienceCss, installPrototypeSidebarExperience } = require("./sidebar-experience.cjs");
const { activityExperienceCss, installPrototypeActivityExperience } = require("./activity-experience.cjs");
const { resilienceExperienceCss, installPrototypeResilienceExperience } = require("./resilience-experience.cjs");
const { signatureDarkCss, installSignatureDark } = require("./signature-dark.cjs");

const prototypeRoot = __dirname;
const repositoryRoot = path.resolve(prototypeRoot, "..", "..", "..", "..");
const frontendRequire = createRequire(path.join(repositoryRoot, "src-tauri", "frontend", "package.json"));
const { chromium } = frontendRequire("playwright-core");
const demoUrl = pathToFileURL(path.join(repositoryRoot, "docs", "ui", "demo", "index.html")).href;

const invokedDirectly = require.main === module;
const args = invokedDirectly ? process.argv.slice(2) : [];
const signatureDarkMode = args.length === 1 && args[0] === "--signature-dark";
if (args.length && !signatureDarkMode) throw new Error(`Unknown arguments: ${args.join(" ")}`);

const lightTheme = { key: "light", query: "light", rootTheme: "studio-light" };
const darkTheme = { key: "dark", query: "dark", rootTheme: "obsidian" };
const themes = signatureDarkMode ? [darkTheme] : [lightTheme, darkTheme];
const signatureRoot = path.join(prototypeRoot, "dark");
const signatureOutputDir = path.join(signatureRoot, "r-code-signature");
const signatureStagingDir = path.join(signatureRoot, `.r-code-signature-render-${process.pid}`);

const pairedScenarios = [
  { slug: "launcher", query: "task=review&state=launcher", light: "01", dark: "02" },
  { slug: "subagents", query: "task=queue&state=run&prototypePanel=subagents", light: "03", dark: "04" },
  { slug: "terminal", query: "task=queue&state=terminal", light: "05", dark: "06" },
  { slug: "files", query: "task=queue&state=files", light: "07", dark: "08" },
  { slug: "review", query: "task=review&state=review", light: "09", dark: "10" },
  { slug: "review-collapsed", query: "task=review&state=review-collapsed&prototypePanel=context", light: "11", dark: "12" },
  { slug: "subagent-detail", query: "task=queue&state=run&prototypePanel=subagent-detail", light: "21", dark: "22" },
  { slug: "workbench-multi-tabs", query: "task=review&state=files&prototypePanel=workbench-tabs", light: "35", dark: "44" },
  { slug: "workbench-tab-fallback", query: "task=review&state=files&prototypePanel=workbench-tab-fallback", light: "37", dark: "46" },
  { slug: "workbench-launcher-restored", query: "task=review&state=files&prototypePanel=workbench-launcher-restored", light: "39", dark: "48" },
  { slug: "model-configuration", query: "task=review&state=hidden", light: "41", dark: "50", prepare: "model-config" },
  { slug: "codex-configuration", query: "task=complete&state=hidden", light: "43", dark: "52", prepare: "model-config" },
];

const activityScenarios = [
  {
    slug: "event-running",
    mode: "running",
    query: "task=queue&state=run&prototypeActivity=running",
    light: "23",
    dark: "24",
  },
  {
    slug: "event-complete-collapsed",
    mode: "collapsed",
    query: "task=complete&state=hidden&prototypeActivity=collapsed",
    light: "25",
    dark: "26",
  },
  {
    slug: "event-complete-expanded",
    mode: "expanded",
    query: "task=complete&state=hidden&prototypeActivity=expanded",
    light: "27",
    dark: "28",
  },
  {
    slug: "event-multi-command-expanded",
    mode: "multi",
    query: "task=complete&state=hidden&prototypeActivity=multi",
    light: "29",
    dark: "30",
  },
  {
    slug: "event-shell-expanded",
    mode: "single",
    query: "task=complete&state=hidden&prototypeActivity=single",
    light: "31",
    dark: "32",
  },
  {
    slug: "event-single-file-diff-expanded",
    mode: "file",
    query: "task=complete&state=hidden&prototypeActivity=file",
    light: "33",
    dark: "34",
  },
];

const supplementalDarkScenarios = [
  {
    slug: "approval-required",
    mode: "approval",
    query: "task=queue&state=run&prototypeFlow=approval",
    dark: "36",
  },
  {
    slug: "command-failed",
    mode: "failure",
    query: "task=queue&state=run&prototypeFlow=failure",
    dark: "38",
  },
  {
    slug: "new-task",
    mode: "empty",
    query: "task=queue&state=launcher&prototypeFlow=empty",
    dark: "40",
  },
  {
    slug: "context-picker",
    mode: "context",
    query: "task=complete&state=hidden&prototypeFlow=context",
    dark: "42",
  },
];

const prototypeCss = `
  :root[data-theme="studio-light"] {
    --prototype-workspace-radius: 20px;
    --bg-app: #fbfaf7;
    --bg-sidebar: #f5f0ea;
    --bg-panel: #fffefc;
    --bg-card: #fffdf9;
    --bg-chip: #f3ede7;
    --bg-inset: #f6f1eb;
    --bg-hover: #f5eee7;
    --bg-active: #f0e7de;
    --border: #e7ded4;
    --border-strong: #d5c9bc;
    --prototype-sidebar: #f5f0ea;
    --prototype-sidebar-glow: rgba(191, 91, 73, .05);
    --prototype-link: #286a9f;
    --prototype-command: #8d837a;
    --prototype-user: #f1ece6;
  }

  :root[data-theme="obsidian"] {
    --prototype-workspace-radius: 20px;
    --bg-app: #151311;
    --bg-sidebar: #211d1e;
    --bg-panel: #1b1917;
    --bg-card: #211f1c;
    --bg-chip: #2b2725;
    --bg-inset: #100f0e;
    --bg-hover: #292622;
    --bg-active: #302c28;
    --border: #302e2a;
    --border-strong: #48423d;
    --prototype-sidebar: #211d1e;
    --prototype-sidebar-glow: rgba(106, 34, 50, .08);
    --prototype-link: #8bc3ff;
    --prototype-command: #8d8984;
    --prototype-user: #242321;
  }

  /* One continuous underlay is exposed by the sidebar, topbar, and workspace corner cutout. */
  #app.app-shell {
    background-color: var(--prototype-sidebar) !important;
    background-image: radial-gradient(
      48% 110% at 16% 10%,
      var(--prototype-sidebar-glow) 0%,
      transparent 72%
    ) !important;
  }

  #app .app-sidebar {
    background: transparent !important;
    border-right: 0 !important;
  }

  #app.app-shell .app-topbar {
    gap: 2px;
    padding-inline: 10px;
    border-bottom: 0 !important;
    background: transparent !important;
  }

  #app.app-shell .compact-search-toggle,
  #app.app-shell .top-action-help {
    display: none !important;
  }

  #app.app-shell .main {
    position: relative;
    z-index: 1;
    overflow: hidden;
    isolation: isolate;
    border-left: 1px solid var(--border) !important;
    border-top: 1px solid var(--border) !important;
    border-radius: var(--prototype-workspace-radius) 0 0 0;
    background: var(--bg-app);
  }

  .prototype-desktop-nav,
  .prototype-history-actions,
  .prototype-app-menus {
    display: flex;
    align-items: center;
  }

  .prototype-desktop-nav {
    align-self: stretch;
    gap: 8px;
    margin-left: 2px;
  }

  .prototype-history-actions,
  .prototype-app-menus {
    gap: 1px;
  }

  .prototype-desktop-button {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    height: 30px;
    padding: 0 8px;
    border: 0;
    border-radius: 6px;
    background: transparent;
    color: var(--fg-muted);
    font-size: 12px;
    line-height: 1;
    white-space: nowrap;
  }

  .prototype-desktop-button:hover,
  .prototype-desktop-button:focus-visible,
  .prototype-desktop-button[aria-expanded="true"] {
    background: color-mix(in srgb, var(--fg) 7%, transparent);
    color: var(--fg);
  }

  .prototype-desktop-button:disabled {
    opacity: .38;
  }

  .prototype-history-button {
    width: 30px;
    padding: 0;
  }

  .prototype-desktop-menu-popover {
    position: fixed;
    z-index: 520;
    min-width: 168px;
    padding: 5px;
    border: 1px solid var(--border-strong);
    border-radius: 9px;
    background: var(--bg-panel);
    box-shadow: var(--shadow-popover);
  }

  .prototype-desktop-menu-item {
    display: flex;
    align-items: center;
    width: 100%;
    min-height: 29px;
    padding: 0 9px;
    border: 0;
    border-radius: 5px;
    background: transparent;
    color: var(--fg);
    font-size: 12px;
    text-align: left;
  }

  .prototype-desktop-menu-item:hover,
  .prototype-desktop-menu-item:focus-visible {
    background: var(--bg-hover);
  }

  #app.scene-room .room-scopebar {
    display: none !important;
  }

  #app.scene-room .convo,
  #app.scene-room .workbench {
    border-right: 0 !important;
    border-left: 0 !important;
  }

  #app.scene-room .room-splitter {
    background: transparent !important;
  }

  #app.scene-room .room-splitter::before,
  #app.scene-room .room-splitter:hover::before,
  #app.scene-room .room-splitter:focus-visible::before {
    top: 0 !important;
    bottom: 0 !important;
    width: 1px !important;
    background: var(--border) !important;
  }

  #app.scene-room .room-splitter:hover::before,
  #app.scene-room .room-splitter:focus-visible::before {
    background: var(--accent) !important;
  }

  #app.scene-room .room-splitter > span {
    display: none !important;
  }

  #app.scene-room .scene-room,
  #app.scene-room .convo,
  #app.scene-room .canvas,
  #app.scene-room .canvas-body,
  #app.scene-room .timeline,
  #app.scene-room .workbench,
  #app.scene-room .workbench-body,
  #app.scene-room .file-preview,
  #app.scene-room .file-code,
  #app.scene-room .file-code-editor {
    background: var(--bg-app) !important;
  }

  #app.scene-room .room-conversation-head,
  #app.scene-room .room-scopebar,
  #app.scene-room .canvas-tabs,
  #app.scene-room .workbench-head,
  #app.scene-room .chat-composer,
  #app.scene-room .room-archived-note {
    background: var(--bg-panel) !important;
  }

  #app.scene-room .comp-box,
  #app.scene-room .workbench-launcher-list,
  #app.scene-room .workbench-launcher-glyph,
  #app.scene-room .workbench-review-panel,
  #app.scene-room .workbench-review-rail-icon {
    background: var(--bg-card) !important;
  }

  .sidebar-task-actions,
  .conversation-row-actions > .menu-root,
  .room-task-actions,
  .task-actions-popover {
    display: none !important;
  }

  .sidebar-task-row.active:not(:hover):not(:focus-within) .sidebar-task time {
    opacity: 1 !important;
  }

  .prototype-row-actions {
    display: inline-flex;
    align-items: center;
    justify-content: flex-end;
    gap: 1px;
    opacity: 0;
    pointer-events: none;
    transition: opacity var(--dur-1) var(--ease);
  }

  .sidebar-task-row > .prototype-row-actions {
    position: absolute;
    z-index: 2;
    top: 1px;
    right: 2px;
    min-width: 58px;
  }

  .sidebar-task-row:hover > .prototype-row-actions,
  .sidebar-task-row:focus-within > .prototype-row-actions,
  .conversation-row:hover .prototype-row-actions,
  .conversation-row:focus-within .prototype-row-actions {
    opacity: 1;
    pointer-events: auto;
  }

  .sidebar-task-row:hover .sidebar-task time,
  .sidebar-task-row:focus-within .sidebar-task time {
    opacity: 0;
  }

  .prototype-action {
    display: inline-grid;
    place-items: center;
    width: 28px;
    height: 28px;
    padding: 0;
    border: 0;
    border-radius: 5px;
    background: transparent;
    color: var(--fg-muted);
  }

  .prototype-action:hover,
  .prototype-action:focus-visible {
    background: color-mix(in srgb, var(--fg) 8%, transparent);
    color: var(--fg);
  }

  .prototype-action[aria-pressed="true"] {
    color: var(--accent);
  }

  .prototype-pinned > .sidebar-task,
  .conversation-row.prototype-pinned {
    background: var(--bg-hover);
  }

  .conversation-row-actions .prototype-row-actions {
    min-width: 58px;
  }

  /* Main session stream: one reading column, not a stack of cards. */
  #app.scene-room .timeline {
    padding: 0 !important;
    scrollbar-gutter: stable;
  }

  #app.scene-room .convo > .activity-strip,
  #app.scene-room .convo > .subagent-panel,
  #app.scene-room .composer > .statusbar {
    display: none !important;
  }

  #app.scene-room .chat-composer {
    border-top: 0 !important;
    background: var(--bg-app) !important;
  }

  #app.scene-room .composer {
    padding: 8px 12px 12px;
  }

  #app.scene-room .comp-box {
    border-color: var(--border) !important;
    box-shadow: none !important;
  }

  .prototype-session {
    width: min(820px, calc(100% - 56px));
    margin: 0 auto;
    padding: 24px 0 36px;
    color: var(--fg);
  }

  .prototype-user-row {
    display: flex;
    justify-content: flex-end;
    margin-bottom: 32px;
  }

  .prototype-user-message {
    max-width: min(72%, 620px);
    padding: 11px 16px 12px;
    border: 0;
    border-radius: 18px;
    background: var(--prototype-user);
    color: var(--fg);
    font-size: 14px;
    line-height: 1.55;
    text-wrap: pretty;
  }

  .prototype-session-summary {
    display: flex;
    align-items: center;
    width: 100%;
    gap: 7px;
    padding: 0 0 12px;
    border: 0;
    border-bottom: 1px solid var(--border);
    border-radius: 0;
    background: transparent;
    color: var(--fg-muted);
    font-size: 13px;
    font-weight: 520;
    text-align: left;
  }

  .prototype-session-summary:hover {
    color: var(--fg);
  }

  .prototype-session-summary svg {
    transition: transform var(--dur-2) var(--ease);
  }

  .prototype-session-summary[aria-expanded="false"] svg {
    transform: rotate(-90deg);
  }

  .prototype-session-body {
    padding-top: 22px;
  }

  .prototype-assistant-copy {
    margin: 0 0 17px;
    color: var(--fg);
    font-size: 15px;
    font-weight: 440;
    line-height: 1.72;
    letter-spacing: .002em;
    text-wrap: pretty;
  }

  .prototype-command {
    display: grid;
    grid-template-columns: 22px minmax(0, 1fr);
    align-items: center;
    gap: 10px;
    min-width: 0;
    margin: 19px 0 20px;
    color: var(--prototype-command);
    font: 12px/1.55 var(--font-mono);
  }

  .prototype-command-icon {
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    border: 1px solid color-mix(in srgb, var(--prototype-command) 72%, transparent);
    border-radius: 5px;
  }

  .prototype-command code {
    min-width: 0;
    overflow: hidden;
    color: inherit;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .prototype-result-lead {
    margin-top: 24px;
    font-weight: 600;
  }

  .prototype-file-links {
    display: flex;
    align-items: flex-start;
    flex-direction: column;
    gap: 5px;
    margin: 10px 0 0;
  }

  .prototype-file-link {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    max-width: 100%;
    padding: 3px 0;
    color: var(--prototype-link);
    font-size: 13px;
    line-height: 1.45;
    text-decoration: none;
  }

  .prototype-file-link:hover,
  .prototype-file-link:focus-visible {
    color: color-mix(in srgb, var(--prototype-link) 82%, var(--fg));
    text-decoration: underline;
    text-underline-offset: 3px;
  }

  .prototype-file-link span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .prototype-completion {
    display: flex;
    align-items: center;
    gap: 4px;
    margin-top: 22px;
    padding-top: 13px;
    border-top: 1px solid var(--border);
  }

  .prototype-completion-state {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    margin-right: auto;
    color: var(--success);
    font-size: 12px;
    font-weight: 560;
  }

  .prototype-completion-action {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-height: 30px;
    padding: 0 8px;
    border: 0;
    border-radius: 6px;
    background: transparent;
    color: var(--fg-muted);
    font-size: 12px;
    font-weight: 540;
  }

  .prototype-completion-action:hover,
  .prototype-completion-action:focus-visible {
    background: color-mix(in srgb, var(--fg) 7%, transparent);
    color: var(--fg);
  }

  .prototype-review-action,
  .prototype-review-action[aria-pressed="true"] {
    color: var(--prototype-link);
  }

  .prototype-session.is-undone .prototype-completion-state {
    color: var(--fg-muted);
  }

  .prototype-session.is-readonly .prototype-completion {
    display: none;
  }

  @media (max-width: 1359px) {
    .prototype-desktop-nav {
      gap: 4px;
    }

    .prototype-desktop-button {
      padding-inline: 6px;
    }

    .prototype-session {
      width: min(760px, calc(100% - 40px));
      padding-top: 20px;
    }

    .prototype-assistant-copy {
      font-size: 14px;
    }
  }
`;

function findBrowserExecutable() {
  const localAppData = process.env.LOCALAPPDATA || "";
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
    path.join(process.env.PROGRAMFILES || "", "Google", "Chrome", "Application", "chrome.exe"),
    path.join(process.env.PROGRAMFILES || "", "Microsoft", "Edge", "Application", "msedge.exe"),
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    "/usr/bin/google-chrome",
    "/usr/bin/chromium",
    "/usr/bin/chromium-browser",
  ].find((candidate) => candidate && fs.existsSync(candidate));
}

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function sha256(filePath) {
  return crypto.createHash("sha256").update(fs.readFileSync(filePath)).digest("hex");
}

function snapshotTopLevelDarkPngs() {
  if (!fs.existsSync(signatureRoot)) return {};
  return Object.fromEntries(
    fs.readdirSync(signatureRoot, { withFileTypes: true })
      .filter((entry) => entry.isFile() && entry.name.toLowerCase().endsWith(".png"))
      .sort((left, right) => left.name.localeCompare(right.name))
      .map((entry) => [entry.name, sha256(path.join(signatureRoot, entry.name))]),
  );
}

function assertSignatureChild(targetPath, label) {
  const resolvedRoot = path.resolve(signatureRoot);
  const resolvedTarget = path.resolve(targetPath);
  assert(path.dirname(resolvedTarget) === resolvedRoot, `${label} escaped the dark prototype directory`);
}

function prepareSignatureRender() {
  assertSignatureChild(signatureStagingDir, "signature staging directory");
  assert(!fs.existsSync(signatureStagingDir), `stale signature staging directory: ${signatureStagingDir}`);
  fs.mkdirSync(signatureStagingDir, { recursive: true });
  const existingDemo = path.join(signatureOutputDir, "demo.html");
  if (fs.existsSync(existingDemo)) fs.copyFileSync(existingDemo, path.join(signatureStagingDir, "demo.html"));
  return snapshotTopLevelDarkPngs();
}

function signaturePngNames(directory) {
  if (!fs.existsSync(directory)) return [];
  const entries = fs.readdirSync(directory, { withFileTypes: true });
  assert(entries.every((entry) => entry.isFile()
      && (entry.name.toLowerCase().endsWith(".png") || entry.name === "demo.html")),
    `signature output contains an unexpected entry: ${directory}`);
  return entries.filter((entry) => entry.name.toLowerCase().endsWith(".png")).map((entry) => entry.name).sort();
}

function publishSignatureRender(baselineDarkPngs) {
  assertSignatureChild(signatureOutputDir, "signature output directory");
  assertSignatureChild(signatureStagingDir, "signature staging directory");

  const stagedNames = signaturePngNames(signatureStagingDir);
  const expectedCount = pairedScenarios.length + activityScenarios.length + supplementalDarkScenarios.length + 4;
  assert(stagedNames.length === expectedCount,
    `signature render produced ${stagedNames.length} PNGs instead of ${expectedCount}`);
  assert(new Set(stagedNames).size === stagedNames.length, "signature render produced duplicate filenames");

  const currentDarkPngs = snapshotTopLevelDarkPngs();
  assert(JSON.stringify(currentDarkPngs) === JSON.stringify(baselineDarkPngs),
    "top-level dark prototypes changed during signature rendering");

  if (fs.existsSync(signatureOutputDir)) {
    const existingNames = signaturePngNames(signatureOutputDir);
    assert(existingNames.every((name) => stagedNames.includes(name)),
      "existing signature directory contains unexpected files and will not be replaced");
    fs.rmSync(signatureOutputDir, { recursive: true });
  }

  fs.renameSync(signatureStagingDir, signatureOutputDir);
  process.stdout.write(`published dark/r-code-signature (${stagedNames.length} PNGs)\n`);
}

function cleanupSignatureRender() {
  assertSignatureChild(signatureStagingDir, "signature staging directory");
  if (fs.existsSync(signatureStagingDir)) fs.rmSync(signatureStagingDir, { recursive: true });
}

async function waitReady(page, selector) {
  await page.waitForFunction(() => window.__ready === true, undefined, { timeout: 10_000 });
  await page.waitForSelector(selector, { state: "visible" });
  await page.evaluate(() => document.fonts?.ready ?? true);
}

async function installPrototypeDesktopChrome(page) {
  await page.evaluate(() => {
    const topbar = document.querySelector(".app-topbar");
    const sidebarToggle = topbar?.querySelector(".desktop-sidebar-toggle");
    if (!topbar || !sidebarToggle) return;

    const nativeNav = topbar.querySelector(".desktop-navigation");
    if (nativeNav) {
      topbar.querySelector(".prototype-desktop-nav")?.remove();
      document.documentElement.dataset.prototypeDesktopChrome = "native";
      return;
    }
    if (topbar.querySelector(".prototype-desktop-nav")) return;

    const backIcon = `
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="m15 18-6-6 6-6"></path>
      </svg>`;
    const forwardIcon = `
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="m9 18 6-6-6-6"></path>
      </svg>`;
    const menuItems = {
      "文件": ["新建任务", "打开文件夹…", "关闭窗口"],
      "编辑": ["撤销", "重做", "查找"],
      "视图": ["切换左侧边栏", "打开审核面板", "重置缩放"],
      "帮助": ["快捷键", "查看文档", "关于 R-Code"],
    };

    const nav = document.createElement("nav");
    nav.className = "prototype-desktop-nav";
    nav.setAttribute("aria-label", "桌面导航");
    nav.innerHTML = `
      <div class="prototype-history-actions" aria-label="浏览历史">
        <button type="button" class="prototype-desktop-button prototype-history-button"
          data-prototype-history="back" aria-label="后退" title="后退">${backIcon}</button>
        <button type="button" class="prototype-desktop-button prototype-history-button"
          data-prototype-history="forward" aria-label="前进" title="前进" disabled>${forwardIcon}</button>
      </div>
      <div class="prototype-app-menus">
        ${Object.keys(menuItems).map((label) => `
          <button type="button" class="prototype-desktop-button prototype-menu-button"
            data-prototype-menu="${label}" aria-haspopup="menu" aria-expanded="false">${label}</button>
        `).join("")}
      </div>`;
    sidebarToggle.insertAdjacentElement("afterend", nav);

    const closeMenu = () => {
      document.querySelector(".prototype-desktop-menu-popover")?.remove();
      nav.querySelectorAll("[data-prototype-menu]").forEach((button) => {
        button.setAttribute("aria-expanded", "false");
      });
    };

    nav.querySelector('[data-prototype-history="back"]')?.addEventListener("click", () => {
      document.documentElement.dataset.prototypeHistoryAction = "back";
    });

    nav.querySelectorAll("[data-prototype-menu]").forEach((button) => {
      button.addEventListener("click", (event) => {
        event.stopPropagation();
        const label = button.dataset.prototypeMenu || "";
        const wasOpen = button.getAttribute("aria-expanded") === "true";
        closeMenu();
        if (wasOpen) return;

        const popover = document.createElement("div");
        popover.className = "prototype-desktop-menu-popover";
        popover.setAttribute("role", "menu");
        popover.setAttribute("aria-label", `${label}菜单`);
        popover.innerHTML = (menuItems[label] || []).map((item) => `
          <button type="button" role="menuitem" class="prototype-desktop-menu-item">${item}</button>
        `).join("");
        const rect = button.getBoundingClientRect();
        popover.style.left = `${Math.round(rect.left)}px`;
        popover.style.top = `${Math.round(rect.bottom + 4)}px`;
        document.body.append(popover);
        button.setAttribute("aria-expanded", "true");
        popover.querySelectorAll("[role=menuitem]").forEach((item) => {
          item.addEventListener("click", closeMenu);
        });
      });
    });

    if (window.__prototypeDesktopOutsideHandler) {
      document.removeEventListener("pointerdown", window.__prototypeDesktopOutsideHandler);
    }
    window.__prototypeDesktopOutsideHandler = (event) => {
      if (!event.target.closest?.(".prototype-desktop-nav, .prototype-desktop-menu-popover")) closeMenu();
    };
    document.addEventListener("pointerdown", window.__prototypeDesktopOutsideHandler);

    if (window.__prototypeDesktopKeyHandler) {
      document.removeEventListener("keydown", window.__prototypeDesktopKeyHandler);
    }
    window.__prototypeDesktopKeyHandler = (event) => {
      if (event.key === "Escape") closeMenu();
    };
    document.addEventListener("keydown", window.__prototypeDesktopKeyHandler);
    document.documentElement.dataset.prototypeDesktopChrome = "true";
  });
}

async function installPrototypeActions(page) {
  await page.evaluate(() => {
    const pinSvg = `
      <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="M12 17v5"></path><path d="M5 17h14"></path><path d="M6 3h12"></path>
        <path d="M8 3v7a2 2 0 0 1-.6 1.4L5 14h14l-2.4-2.6A2 2 0 0 1 16 10V3"></path>
      </svg>`;
    const archiveSvg = `
      <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="M4 7h16"></path><path d="M5 7v12h14V7"></path><path d="M9 11h6"></path>
        <path d="M4 3h16v4H4z"></path>
      </svg>`;

    const rows = [...document.querySelectorAll(".sidebar-task-row, .conversation-row")];
    rows.forEach((row, index) => {
      if (row.querySelector(":scope > .prototype-row-actions, .conversation-row-actions > .prototype-row-actions")) return;
      row.dataset.prototypeOrder = row.dataset.prototypeOrder || String(index);

      const title = row.querySelector(".sidebar-task .rail-label, .conversation-main strong")?.textContent?.trim() || "当前对话";
      const originalTrigger = row.querySelector(".task-actions-trigger");
      if (!originalTrigger) return;

      const actions = document.createElement("span");
      actions.className = "prototype-row-actions";
      actions.setAttribute("aria-label", `对话操作：${title}`);

      const pin = document.createElement("button");
      pin.type = "button";
      pin.className = "prototype-action prototype-action-pin";
      pin.title = "置顶";
      pin.setAttribute("aria-label", `置顶：${title}`);
      pin.setAttribute("aria-pressed", "false");
      pin.innerHTML = pinSvg;
      pin.addEventListener("click", (event) => {
        event.preventDefault();
        event.stopPropagation();
        const isPinned = !row.classList.contains("prototype-pinned");
        row.classList.toggle("prototype-pinned", isPinned);
        pin.setAttribute("aria-pressed", String(isPinned));
        pin.setAttribute("aria-label", `${isPinned ? "取消置顶" : "置顶"}：${title}`);
        pin.title = isPinned ? "取消置顶" : "置顶";

        const parent = row.parentElement;
        if (!parent) return;
        if (isPinned) {
          parent.prepend(row);
        } else {
          const originalOrder = Number(row.dataset.prototypeOrder || 0);
          const next = [...parent.children].find((candidate) => {
            if (candidate === row) return false;
            return Number(candidate.dataset.prototypeOrder || Number.MAX_SAFE_INTEGER) > originalOrder;
          });
          parent.insertBefore(row, next || null);
        }
      });

      const archive = document.createElement("button");
      archive.type = "button";
      archive.className = "prototype-action prototype-action-archive";
      archive.title = "归档";
      archive.setAttribute("aria-label", `归档：${title}`);
      archive.innerHTML = archiveSvg;
      archive.addEventListener("click", (event) => {
        event.preventDefault();
        event.stopPropagation();
        document.documentElement.dataset.prototypeArchiveState = "requested";
        originalTrigger.click();
        window.setTimeout(() => {
          const archiveItem = [...document.querySelectorAll('[role="menuitem"]')]
            .find((item) => item.textContent?.includes("归档对话"));
          if (!archiveItem) {
            document.documentElement.dataset.prototypeArchiveState = "missing-menu-action";
            return;
          }
          archiveItem.click();
          document.documentElement.dataset.prototypeArchiveState = "committed";
        }, 0);
      });

      actions.append(pin, archive);
      const conversationHost = row.querySelector(".conversation-row-actions");
      (conversationHost || row).append(actions);
    });

    document.documentElement.dataset.prototypeActionsInstalled = "true";
  });
}

async function installPrototypeConversation(page) {
  await page.evaluate(() => {
    const timeline = document.querySelector(".timeline");
    if (!timeline || timeline.querySelector(".prototype-session")) return;

    const title = document.querySelector(".room-conversation-title strong")?.textContent?.trim() || "当前任务";
    const profiles = {
      "更新依赖并修复告警": {
        prompt: "升级工作区依赖并处理编译告警。",
        duration: "已处理 1m 46s",
        intro: "我会先核对依赖清单和编译输出，确认升级范围，再逐项消除告警。",
        firstCommand: "cargo update --workspace && cargo check --workspace --all-targets",
        detail: "依赖已经更新。剩余告警来自未使用的导入和旧版 API，我会按实际调用关系收敛修改，不做无关重构。",
        secondCommand: "cargo fmt --all -- --check && cargo test --workspace",
        result: "依赖升级与告警清理已完成，工作区检查和测试均已通过。",
        files: ["Cargo.toml", "Cargo.lock"],
        complete: true,
      },
      "统一错误处理规范": {
        prompt: "统一错误处理规范，并补齐错误边界的回归测试。",
        duration: "已处理 2m 06s",
        intro: "我会先梳理错误类型的传播路径，再统一边界转换和用户可见文案。",
        firstCommand: "rg \"anyhow!|unwrap\\(|expect\\(\" src tests",
        detail: "目前有两处边界把底层错误直接暴露给界面。我会保留可诊断上下文，同时把用户提示收敛到统一结构。",
        secondCommand: "cargo test -p r-code-host error_boundary -- --nocapture",
        result: "错误类型、边界转换和回归测试已经统一，可以进入变更审核。",
        files: ["src/error.rs", "tests/error_boundary.rs"],
        complete: true,
      },
      "修复任务队列并发问题": {
        prompt: "检查任务队列的并发问题，避免同一任务被重复执行。",
        duration: "已处理 1m 18s",
        intro: "我会先核对任务领取、状态写入和取消流程，确认是否存在重复消费的时间窗口。",
        firstCommand: "cargo test -p r-code-host task_queue -- --nocapture",
        detail: "已经定位到状态写入晚于任务派发：多个 worker 会在同一窗口读到 pending。我正在把领取动作收敛成一次原子更新。",
        secondCommand: "cargo test -p r-code-host task_queue::concurrency -- --nocapture",
        result: "并发回归测试正在运行；完成后会给出变更文件和审核入口。",
        files: [],
        complete: false,
      },
    };
    const profile = profiles[title] || profiles["更新依赖并修复告警"];
    const archived = Boolean(document.querySelector(".room-archived-note"));
    const reviewOpen = document.querySelector('[data-testid="workbench-panel"]')?.dataset.workbenchKind === "review";

    const terminalIcon = `
      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="m7 8 3 4-3 4"></path><path d="M13 16h4"></path>
      </svg>`;
    const fileIcon = `
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="M6 3.5h8l4 4v13H6z"></path><path d="M14 3.5v4h4"></path>
      </svg>`;
    const reviewIcon = `
      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="M4 5h16v14H4z"></path><path d="m8 12 2.5 2.5L16 9"></path>
      </svg>`;
    const undoIcon = `
      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="M9 7 4 12l5 5"></path><path d="M5 12h8a6 6 0 0 1 6 6"></path>
      </svg>`;
    const checkIcon = `
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <circle cx="12" cy="12" r="9"></circle><path d="m8 12 2.5 2.5L16 9"></path>
      </svg>`;
    const chevronIcon = `
      <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="m6 9 6 6 6-6"></path>
      </svg>`;

    const fileLinks = profile.files.map((file) => `
      <a class="prototype-file-link" data-prototype-file="${file}" href="#">
        ${fileIcon}<span>${file}</span>
      </a>`).join("");

    const completion = profile.complete ? `
      <div class="prototype-completion" aria-label="完成后的操作">
        <span class="prototype-completion-state">${checkIcon}<span>已完成</span></span>
        <button type="button" class="prototype-completion-action prototype-review-action"
          aria-pressed="${reviewOpen}" title="在右侧工作台打开审核">
          ${reviewIcon}<span>审核变更</span>
        </button>
        <button type="button" class="prototype-completion-action prototype-undo-action" title="撤销本次变更">
          ${undoIcon}<span>撤销</span>
        </button>
      </div>` : "";

    const session = document.createElement("article");
    session.className = `prototype-session${archived ? " is-readonly" : ""}`;
    session.dataset.prototypeState = profile.complete ? "complete" : "running";
    session.innerHTML = `
      <div class="prototype-user-row">
        <div class="prototype-user-message">${profile.prompt}</div>
      </div>
      <button type="button" class="prototype-session-summary" aria-expanded="true"
        aria-controls="prototype-session-body">
        <span>${profile.duration}</span>${chevronIcon}
      </button>
      <div class="prototype-session-body" id="prototype-session-body">
        <p class="prototype-assistant-copy">${profile.intro}</p>
        <div class="prototype-command" aria-label="已执行命令">
          <span class="prototype-command-icon">${terminalIcon}</span>
          <code>Ran ${profile.firstCommand}</code>
        </div>
        <p class="prototype-assistant-copy">${profile.detail}</p>
        <div class="prototype-command" aria-label="已执行命令">
          <span class="prototype-command-icon">${terminalIcon}</span>
          <code>Ran ${profile.secondCommand}</code>
        </div>
        <p class="prototype-assistant-copy${profile.complete ? " prototype-result-lead" : ""}">${profile.result}</p>
        ${fileLinks ? `<div class="prototype-file-links" aria-label="输出文件">${fileLinks}</div>` : ""}
        ${completion}
      </div>`;

    timeline.replaceChildren(session);
    timeline.scrollTop = 0;
    timeline.setAttribute("aria-label", "Session 记录");

    const status = document.querySelector(".room-conversation-title span");
    if (status) {
      status.textContent = archived
        ? "已归档，只读"
        : profile.complete
          ? (reviewOpen ? "正在审核" : "已完成")
          : "正在执行";
    }

    document.querySelectorAll(".scoped").forEach((badge) => {
      if (!badge.textContent?.includes("替我审批")) return;
      badge.textContent = badge.textContent.split("·")[0].trim();
      badge.title = badge.title.replace(/\n项目权限：替我审批/g, "");
      badge.dataset.prototypeDeduped = "true";
    });
    document.querySelectorAll(".sum-scope").forEach((scope) => {
      const project = scope.firstElementChild;
      if (project?.textContent?.trim().toLowerCase() !== "r-code") return;
      project.remove();
      scope.dataset.prototypeProjectDeduped = "true";
    });

    session.querySelectorAll(".prototype-file-link").forEach((link) => {
      const url = new URL(window.location.href);
      url.searchParams.set("scene", "editor");
      url.searchParams.set("project", "r-code");
      url.searchParams.set("file", link.dataset.prototypeFile || "");
      url.searchParams.delete("state");
      link.href = url.href;
    });

    const summary = session.querySelector(".prototype-session-summary");
    const body = session.querySelector(".prototype-session-body");
    summary?.addEventListener("click", () => {
      const expanded = summary.getAttribute("aria-expanded") === "true";
      summary.setAttribute("aria-expanded", String(!expanded));
      if (body) body.hidden = expanded;
      document.documentElement.dataset.prototypeSummaryState = expanded ? "collapsed" : "expanded";
    });

    const reviewButton = session.querySelector(".prototype-review-action");
    const markReviewOpen = () => {
      reviewButton?.setAttribute("aria-pressed", "true");
      document.documentElement.dataset.prototypeReviewState = "open";
      const currentStatus = document.querySelector(".room-conversation-title span");
      if (currentStatus) currentStatus.textContent = "正在审核";
    };
    reviewButton?.addEventListener("click", () => {
      if (document.querySelector('[data-testid="workbench-panel"][data-workbench-kind="review"]')) {
        markReviewOpen();
        return;
      }
      const reviewLauncher = [...document.querySelectorAll(".workbench-launcher-row")]
        .find((row) => row.textContent?.includes("审核"));
      if (reviewLauncher) {
        reviewLauncher.click();
        window.setTimeout(markReviewOpen, 0);
        return;
      }
      document.dispatchEvent(new KeyboardEvent("keydown", {
        key: "4",
        code: "Digit4",
        altKey: true,
        bubbles: true,
      }));
      window.setTimeout(markReviewOpen, 0);
    });

    const undoButton = session.querySelector(".prototype-undo-action");
    undoButton?.addEventListener("click", () => {
      session.classList.add("is-undone");
      session.dataset.prototypeState = "undone";
      const stateLabel = session.querySelector(".prototype-completion-state span");
      if (stateLabel) stateLabel.textContent = "已撤销";
      const label = undoButton.querySelector("span");
      if (label) label.textContent = "已撤销";
      undoButton.disabled = true;
      if (reviewButton) reviewButton.disabled = true;
      document.documentElement.dataset.prototypeUndoState = "complete";
    });

    document.documentElement.dataset.prototypeConversationInstalled = "true";
  });
}

async function settlePrototype(page) {
  await page.evaluate(() => new Promise((resolve) => {
    requestAnimationFrame(() => requestAnimationFrame(resolve));
  }));
}

async function openPage(browser, theme, viewport, query, selector, label, browserErrors) {
  const page = await browser.newPage({ viewport, deviceScaleFactor: 1, reducedMotion: "reduce" });
  page.setDefaultTimeout(10_000);
  page.on("pageerror", (error) => browserErrors.push(`[${label}] ${String(error)}`));
  page.on("console", (message) => {
    if (message.type() === "error") browserErrors.push(`[${label}] ${message.text()}`);
  });

  await page.goto(`${demoUrl}?${query}&theme=${theme.query}&reset=1`, { waitUntil: "load" });
  await waitReady(page, selector);
  await page.addStyleTag({ content: prototypeCss });
  await page.addStyleTag({ content: sidebarExperienceCss });
  await page.addStyleTag({ content: activityExperienceCss });
  await page.addStyleTag({ content: resilienceExperienceCss });
  await installPrototypeDesktopChrome(page);
  await installPrototypeActions(page);
  await installPrototypeConversation(page);
  await installPrototypeActivityExperience(page);
  await installPrototypeResilienceExperience(page);
  await installPrototypeSidebarExperience(page);
  if (signatureDarkMode) {
    await page.addStyleTag({ content: signatureDarkCss });
    await installSignatureDark(page);
  }
  await settlePrototype(page);

  const actualTheme = await page.evaluate(() => document.documentElement.dataset.theme);
  assert(actualTheme === theme.rootTheme, `${label}: expected ${theme.rootTheme}, received ${actualTheme}`);
  return page;
}

async function capture(page, theme, filename) {
  if (signatureDarkMode) {
    await installSignatureDark(page);
    await settlePrototype(page);
  }
  const outputDir = signatureDarkMode ? signatureStagingDir : path.join(prototypeRoot, theme.key);
  fs.mkdirSync(outputDir, { recursive: true });
  await page.screenshot({
    path: path.join(outputDir, filename),
    animations: "disabled",
  });
  const relativeOutput = path.relative(prototypeRoot, path.join(outputDir, filename)).replaceAll("\\", "/");
  process.stdout.write(`rendered ${relativeOutput}\n`);
}

async function assertDefaultActionsHidden(page, label) {
  const row = page.locator(".sidebar-task-row.active").first();
  if (!await row.count()) return;
  const state = await row.evaluate((element) => {
    const actions = element.querySelector(":scope > .prototype-row-actions");
    const original = element.querySelector(".sidebar-task-actions");
    return {
      buttons: actions?.querySelectorAll("button").length || 0,
      opacity: actions ? Number(getComputedStyle(actions).opacity) : -1,
      pointerEvents: actions ? getComputedStyle(actions).pointerEvents : "missing",
      originalDisplay: original ? getComputedStyle(original).display : "missing",
    };
  });
  assert(state.buttons === 2, `${label}: expected exactly two prototype actions`);
  assert(state.opacity === 0 && state.pointerEvents === "none", `${label}: actions visible without hover`);
  assert(state.originalDisplay === "none", `${label}: legacy three-dot action is still visible`);
}

async function assertPaletteHierarchy(page, theme, label) {
  const colors = await page.evaluate(() => {
    const background = (selector) => {
      const element = document.querySelector(selector);
      return element ? getComputedStyle(element).backgroundColor : null;
    };
    const shell = document.querySelector("#app");
    return {
      main: background(".timeline"),
      sidebar: background("#app"),
      composer: background(".comp-box"),
      underlayImage: shell ? getComputedStyle(shell).backgroundImage : "none",
    };
  });

  const expected = signatureDarkMode
    ? { main: "rgb(16, 17, 15)", sidebar: "rgb(28, 25, 24)", composer: "rgb(27, 28, 25)" }
    : theme.key === "dark"
      ? { main: "rgb(21, 19, 17)", sidebar: "rgb(33, 29, 30)", composer: "rgb(33, 31, 28)" }
      : { main: "rgb(251, 250, 247)", sidebar: "rgb(245, 240, 234)", composer: "rgb(255, 253, 249)" };

  assert(colors.main === expected.main, `${label}: main canvas color ${colors.main}`);
  assert(colors.sidebar === expected.sidebar, `${label}: sidebar color ${colors.sidebar}`);
  assert(colors.composer === expected.composer, `${label}: composer color ${colors.composer}`);
  assert(colors.underlayImage && colors.underlayImage !== "none", `${label}: shared underlay glow missing`);
}

async function assertDesktopShell(page, label) {
  const audit = await page.evaluate(() => {
    const visibleElement = (selector) => [...document.querySelectorAll(selector)]
      .find((element) => getComputedStyle(element).display !== "none");
    const main = document.querySelector("#main-content");
    const topbar = document.querySelector(".app-topbar");
    const sidebar = document.querySelector(".app-sidebar");
    const scopebar = document.querySelector(".room-scopebar");
    const status = document.querySelector(".room-conversation-title span");
    const convo = document.querySelector(".convo");
    const workbench = visibleElement(".workbench");
    const collapsedRail = visibleElement(".workbench-review-rail");
    const splitter = visibleElement(".room-splitter");
    const visibleText = document.body.innerText;
    const mainRect = main?.getBoundingClientRect();
    const topbarRect = topbar?.getBoundingClientRect();
    const sidebarRect = sidebar?.getBoundingClientRect();
    const mainStyle = main ? getComputedStyle(main) : null;
    const shellStyle = getComputedStyle(document.querySelector("#app"));
    const sidebarStyle = sidebar ? getComputedStyle(sidebar) : null;
    const topbarStyle = topbar ? getComputedStyle(topbar) : null;
    return {
      historyLabels: [...document.querySelectorAll(".desktop-history-button, .prototype-history-button")]
        .map((button) => button.getAttribute("aria-label")),
      menuLabels: [...document.querySelectorAll(".desktop-menu-trigger, .prototype-menu-button")]
        .map((button) => button.textContent?.trim()),
      mainRadius: mainStyle?.borderTopLeftRadius || null,
      mainOtherRadii: mainStyle
        ? [mainStyle.borderTopRightRadius, mainStyle.borderBottomRightRadius, mainStyle.borderBottomLeftRadius]
        : [],
      mainBorderLeft: mainStyle?.borderLeftWidth || null,
      mainBorderTop: mainStyle?.borderTopWidth || null,
      mainMeetsSidebar: mainRect && sidebarRect
        ? Math.abs(mainRect.left - sidebarRect.right) <= 1
        : false,
      mainStartsBelowTopbar: mainRect && topbarRect
        ? Math.abs(mainRect.top - topbarRect.bottom) <= 1
        : false,
      shellUnderlayColor: shellStyle.backgroundColor,
      shellUnderlayImage: shellStyle.backgroundImage,
      sidebarOwnBackground: sidebarStyle
        ? [sidebarStyle.backgroundColor, sidebarStyle.backgroundImage]
        : [],
      topbarOwnBackground: topbarStyle
        ? [topbarStyle.backgroundColor, topbarStyle.backgroundImage]
        : [],
      workspaceContainsWorkbench: workbench ? main?.contains(workbench) : true,
      topbarBorderBottom: topbar ? getComputedStyle(topbar).borderBottomWidth : null,
      sidebarBorderRight: sidebar ? getComputedStyle(sidebar).borderRightWidth : null,
      scopeDisplay: scopebar ? getComputedStyle(scopebar).display : null,
      status: status?.textContent?.trim() || "",
      permissionMentions: visibleText.match(/替我审批/g)?.length || 0,
      redundantProjectScopes: [...document.querySelectorAll(".sum-scope")]
        .filter((scope) => /(^|\s)r-code(\s|$)/i.test(scope.innerText)).length,
      convoBorderRight: convo ? getComputedStyle(convo).borderRightWidth : null,
      workbenchExists: Boolean(workbench),
      workbenchBorderLeft: workbench ? getComputedStyle(workbench).borderLeftWidth : null,
      collapsedRailExists: Boolean(collapsedRail),
      collapsedRailBorderLeft: collapsedRail ? getComputedStyle(collapsedRail).borderLeftWidth : null,
      splitterLineWidth: splitter ? getComputedStyle(splitter, "::before").width : null,
    };
  });

  assert(JSON.stringify(audit.historyLabels) === JSON.stringify(["后退", "前进"]), `${label}: back/forward controls missing`);
  assert(JSON.stringify(audit.menuLabels) === JSON.stringify(["文件", "编辑", "视图", "帮助"]), `${label}: desktop menus incomplete`);
  const expectedWorkspaceRadius = signatureDarkMode ? "28px" : "20px";
  assert(audit.mainRadius === expectedWorkspaceRadius,
    `${label}: outer workspace corner is missing (${audit.mainRadius})`);
  assert(audit.mainOtherRadii.every((radius) => radius === "0px"), `${label}: unrelated workspace corners are rounded`);
  assert(audit.mainMeetsSidebar && audit.mainStartsBelowTopbar, `${label}: radius is not at the sidebar/topbar junction`);
  assert(audit.shellUnderlayImage !== "none", `${label}: shared sidebar/topbar underlay glow is missing`);
  assert(
    audit.sidebarOwnBackground[0] === "rgba(0, 0, 0, 0)" && audit.sidebarOwnBackground[1] === "none",
    `${label}: sidebar paints a separate background instead of exposing the shared underlay`,
  );
  assert(
    audit.topbarOwnBackground[0] === "rgba(0, 0, 0, 0)" && audit.topbarOwnBackground[1] === "none",
    `${label}: topbar paints a separate background instead of exposing the shared underlay`,
  );
  assert(audit.workspaceContainsWorkbench, `${label}: right workbench is not part of the foreground workspace layer`);
  assert(audit.mainBorderTop === "1px" && audit.topbarBorderBottom === "0px", `${label}: rounded top edge is doubled or invisible`);
  assert(audit.mainBorderLeft === "1px" && audit.sidebarBorderRight === "0px", `${label}: sidebar/main edge is not a single line`);
  assert(audit.scopeDisplay === "none", `${label}: duplicate room scope remains visible`);
  assert(!audit.status.includes("r-code"), `${label}: project name is repeated in the session status`);
  assert(audit.redundantProjectScopes === 0, `${label}: project name is repeated in workbench scope metadata`);
  if (audit.status.includes("已归档")) {
    assert(audit.permissionMentions <= 1, `${label}: permission copy appears ${audit.permissionMentions} times`);
  } else {
    assert(audit.permissionMentions === 1, `${label}: permission copy appears ${audit.permissionMentions} times`);
  }
  assert(audit.convoBorderRight === "0px", `${label}: conversation panel keeps a duplicate right border`);
  if (audit.workbenchExists) {
    assert(audit.workbenchBorderLeft === "0px", `${label}: workbench keeps a duplicate left border`);
    assert(audit.splitterLineWidth === "1px", `${label}: workbench divider is ${audit.splitterLineWidth}`);
  } else if (audit.collapsedRailExists) {
    assert(audit.collapsedRailBorderLeft === "1px", `${label}: collapsed workbench edge is not a single line`);
  }
}

async function auditSidebarExperience(page) {
  return page.evaluate(() => {
    const scene = document.querySelector("#main-content > .scene-room");
    const context = document.querySelector(".prototype-context-panel");
    const list = document.querySelector('[data-prototype-agent-view="list"]');
    const detail = document.querySelector('[data-prototype-agent-view="detail"]');
    const spinner = document.querySelector(".prototype-agent-spinner");
    const spinnerStyle = spinner ? getComputedStyle(spinner) : null;
    return {
      state: scene?.dataset.prototypeSidebarState || "",
      contextVisible: Boolean(context && getComputedStyle(context).display !== "none"),
      contextHeadings: context ? [...context.querySelectorAll(".prototype-context-heading > span:first-child")]
        .map((heading) => heading.textContent?.trim()) : [],
      environmentRows: context?.querySelectorAll(".prototype-env-row").length || 0,
      sourceRows: context?.querySelectorAll("[data-source]").length || 0,
      listVisible: Boolean(list),
      listHeadings: list ? [...list.querySelectorAll(".prototype-agent-section-heading")]
        .map((heading) => heading.textContent?.trim()) : [],
      listRows: list?.querySelectorAll(".prototype-agent-row").length || 0,
      runningRows: list?.querySelectorAll(".prototype-agent-row.is-running").length || 0,
      completedRows: list?.querySelectorAll('[data-agent-state="complete"]').length || 0,
      listSpinnerCount: list?.querySelectorAll(".prototype-agent-spinner").length || 0,
      detailVisible: Boolean(detail),
      detailBackButtons: detail?.querySelectorAll(".prototype-agent-back").length || 0,
      detailCommands: detail?.querySelectorAll(".prototype-agent-command").length || 0,
      detailCopyBlocks: detail?.querySelectorAll(".prototype-agent-copy").length || 0,
      detailSummaryExpanded: detail?.querySelector(".prototype-agent-session-summary")?.getAttribute("aria-expanded") || null,
      spinnerCount: document.querySelectorAll(".prototype-agent-spinner").length,
      spinnerHasOpenRing: spinnerStyle ? spinnerStyle.borderTopColor !== spinnerStyle.borderRightColor : false,
    };
  });
}

async function assertSidebarView(page, expected, label) {
  const audit = await auditSidebarExperience(page);
  assert(audit.state === expected, `${label}: expected sidebar state ${expected}, received ${audit.state}`);
  if (expected === "context") {
    assert(audit.contextVisible, `${label}: compact context panel is not visible`);
    assert(
      JSON.stringify(audit.contextHeadings) === JSON.stringify(["环境信息", "子智能体", "来源"]),
      `${label}: compact context sections are incomplete`,
    );
    assert(audit.environmentRows >= 3, `${label}: environment information is incomplete`);
    assert(audit.sourceRows === 3, `${label}: context sources are not represented as files`);
  }
  if (expected === "list") {
    assert(audit.listVisible, `${label}: subagent list is not visible`);
    assert(audit.listRows === 3 && audit.runningRows === 1 && audit.completedRows === 2, `${label}: subagent state groups are incorrect`);
    assert(
      JSON.stringify(audit.listHeadings) === JSON.stringify(["进行中 · 01", "已完成 · 02"]),
      `${label}: subagent group headings are incorrect`,
    );
    assert(audit.listSpinnerCount === 0, `${label}: list duplicated status beside relative time`);
  }
  if (expected === "detail") {
    assert(audit.detailVisible, `${label}: subagent detail is not visible`);
    assert(audit.detailBackButtons === 1, `${label}: detail back button is missing`);
    assert(audit.detailCommands === 2 && audit.detailCopyBlocks >= 3, `${label}: detail process stream is incomplete`);
    assert(audit.detailSummaryExpanded === "true", `${label}: detail session summary is not expanded`);
    assert(audit.spinnerCount >= 1 && audit.spinnerHasOpenRing, `${label}: running detail state is missing`);
  }
}

async function assertRunningIndicatorAnimates(page, label) {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await settlePrototype(page);
  const animationName = await page.locator(".prototype-agent-spinner").first()
    .evaluate((element) => getComputedStyle(element).animationName);
  assert(animationName === "prototype-agent-spin", `${label}: running indicator is not animated`);
}

async function assertRunningAvatarAnimates(page, label) {
  await page.emulateMedia({ reducedMotion: "no-preference" });
  await settlePrototype(page);
  const animationName = await page.locator(".prototype-agent-row .prototype-agent-avatar.is-running").first()
    .evaluate((element) => getComputedStyle(element, "::after").animationName);
  assert(animationName === "prototype-agent-spin", `${label}: running avatar indicator is not animated`);
}

async function verifySidebarExperience(page, scenario, label) {
  if (scenario.slug === "review-collapsed") {
    await assertSidebarView(page, "context", label);
    await page.locator(".prototype-context-subagents-button").click();
    await page.waitForFunction(() => (
      document.querySelector("#main-content > .scene-room")?.dataset.prototypeSidebarState === "list"
    ));
    await assertSidebarView(page, "list", label);
    await assertRunningAvatarAnimates(page, label);
    await page.locator('[data-agent-id="interaction"]').click();
    await assertSidebarView(page, "detail", label);
    await page.locator(".prototype-agent-back").click();
    await assertSidebarView(page, "list", label);
    await page.locator(".prototype-collapse-agents").click();
    await assertSidebarView(page, "context", label);
  }

  if (scenario.slug === "subagents") {
    await assertSidebarView(page, "list", label);
    await assertRunningAvatarAnimates(page, label);
    await page.locator('[data-agent-id="interaction"]').click();
    await assertSidebarView(page, "detail", label);
    await page.locator(".prototype-agent-back").click();
    await assertSidebarView(page, "list", label);
  }

  if (scenario.slug === "subagent-detail") {
    await assertSidebarView(page, "detail", label);
    await assertRunningIndicatorAnimates(page, label);
    assert((await page.locator(".prototype-agent-permission").innerText()).trim() === "完全访问", `${label}: explicit subagent elevation is missing`);
    const toolGroup = page.locator(".prototype-agent-tool-group-head");
    assert((await toolGroup.innerText()).includes("运行了 2 项操作"), `${label}: grouped subagent tools are missing`);
    await toolGroup.click();
    assert(await page.locator(".prototype-agent-tool-group-list").isHidden(), `${label}: grouped subagent tools did not collapse`);
    await toolGroup.click();
    assert(await page.locator(".prototype-agent-tool-group-list").isVisible(), `${label}: grouped subagent tools did not expand`);
    const summary = page.locator(".prototype-agent-session-summary");
    const body = page.locator(".prototype-agent-session-body");
    await summary.click();
    assert(await body.isHidden(), `${label}: detail session summary did not collapse`);
    await summary.click();
    assert(await body.isVisible(), `${label}: detail session summary did not expand`);
    await page.locator(".prototype-agent-back").click();
    await assertSidebarView(page, "list", label);
  }
}

async function assertWorkbenchTabState(page, expected, label) {
  const audit = await page.evaluate(() => {
    const root = document.querySelector('[data-testid="workbench-root"]');
    const panel = document.querySelector('[data-testid="workbench-panel"]');
    const tabs = [...document.querySelectorAll(".workbench-tab")];
    return {
      mode: root?.dataset.workbenchMode || null,
      kind: panel?.dataset.workbenchKind || null,
      tabs: tabs.map((tab) => tab.querySelector("strong")?.textContent?.trim() || ""),
      active: document.querySelector(".workbench-active-tab strong")?.textContent?.trim() || null,
      closeButtons: tabs.filter((tab) => tab.querySelector('.workbench-tab-close[aria-label^="关闭"]')).length,
      launcherRows: document.querySelectorAll(".workbench-launcher-row").length,
      shortcuts: [...document.querySelectorAll(".workbench-launcher-row kbd")]
        .map((kbd) => kbd.textContent?.trim() || ""),
    };
  });

  if (expected === "tabs") {
    assert(audit.mode === "docked" && audit.kind === "review", `${label}: multi-tab workbench is not docked on review`);
    assert(JSON.stringify(audit.tabs) === JSON.stringify(["文件", "审核"]), `${label}: multi-tab order is incorrect`);
    assert(audit.active === "审核" && audit.closeButtons === 2, `${label}: active review tab or close controls are missing`);
    return;
  }
  if (expected === "fallback") {
    assert(audit.mode === "docked" && audit.kind === "files", `${label}: closing review did not keep the workbench docked on files`);
    assert(JSON.stringify(audit.tabs) === JSON.stringify(["文件"]) && audit.active === "文件",
      `${label}: previous tab did not occupy the workbench`);
    return;
  }
  if (expected === "launcher") {
    assert(audit.mode === "docked" && audit.kind === "launcher", `${label}: empty workbench did not reopen at the launcher`);
    assert(audit.tabs.length === 0, `${label}: launcher revived a closed tool tab`);
    assert(audit.launcherRows === 4, `${label}: launcher tool choices are incomplete`);
    for (const shortcut of ["Ctrl+Alt+S", "Ctrl+`", "Ctrl+P", "Ctrl+Shift+G"]) {
      assert(audit.shortcuts.includes(shortcut), `${label}: launcher shortcut ${shortcut} is missing`);
    }
  }
}

async function verifyWorkbenchTabExperience(page, scenario, label) {
  const expected = scenario.slug === "workbench-multi-tabs"
    ? "tabs"
    : scenario.slug === "workbench-tab-fallback"
      ? "fallback"
      : "launcher";
  await assertWorkbenchTabState(page, expected, label);
  if (scenario.slug !== "workbench-multi-tabs") return;

  await page.getByTestId("workbench-close").click();
  await assertWorkbenchTabState(page, "fallback", label);
  await page.getByTestId("workbench-close").click();
  assert(await page.getByTestId("workbench-root").getAttribute("data-workbench-mode") === "hidden",
    `${label}: closing the final tab did not hide the workbench`);
  await page.locator(".room-workbench-toggle").click();
  await assertWorkbenchTabState(page, "launcher", label);

  await page.keyboard.press("Control+P");
  assert(await page.getByTestId("workbench-panel").getAttribute("data-workbench-kind") === "files", `${label}: Ctrl+P failed`);
  await page.keyboard.press("Control+Shift+G");
  assert(await page.getByTestId("workbench-panel").getAttribute("data-workbench-kind") === "review", `${label}: Ctrl+Shift+G failed`);
  await page.keyboard.press("Control+Alt+S");
  assert(await page.getByTestId("workbench-panel").getAttribute("data-workbench-kind") === "summary", `${label}: Ctrl+Alt+S failed`);
  await page.keyboard.press("Control+Backquote");
  assert(await page.getByTestId("workbench-panel").getAttribute("data-workbench-kind") === "terminal", `${label}: Ctrl+\` failed`);
}

async function setSidebarCollapsed(page, collapsed, label) {
  const app = page.locator("#app");
  const currentlyCollapsed = await app.evaluate((element) => element.classList.contains("rail-is-collapsed"));
  if (currentlyCollapsed !== collapsed) {
    await page.locator(".desktop-sidebar-toggle").click();
    await page.waitForFunction((expected) => (
      document.querySelector("#app")?.classList.contains("rail-is-collapsed") === expected
    ), collapsed);
  }
  await installPrototypeDesktopChrome(page);
  await settlePrototype(page);
  const width = await page.locator(".app-sidebar").evaluate((element) => element.getBoundingClientRect().width);
  if (collapsed) {
    assert(width <= 72, `${label}: collapsed sidebar is still ${width}px wide`);
  } else {
    assert(width >= 200, `${label}: expanded sidebar is only ${width}px wide`);
  }
  return width;
}

async function verifyDesktopChromeInteractions(page, label) {
  const nativeNav = page.locator(".desktop-navigation");
  if (await nativeNav.count()) {
    assert(await nativeNav.locator(".desktop-history-button").count() === 2, `${label}: native history controls are incomplete`);
    const labels = await nativeNav.locator(".desktop-menu-trigger").allTextContents();
    assert(JSON.stringify(labels.map((value) => value.trim())) === JSON.stringify(["文件", "编辑", "视图", "帮助"]),
      `${label}: native desktop menus are incomplete`);
    await nativeNav.locator(".desktop-menu-trigger", { hasText: "文件" }).click();
    const menu = page.locator('.desktop-menu-popover[role="menu"]');
    await menu.waitFor({ state: "visible" });
    assert(await menu.getByRole("menuitem").count() >= 3, `${label}: native file menu did not open`);
    await page.keyboard.press("Escape");
    await menu.waitFor({ state: "hidden" });
  } else {
    await page.locator('[data-prototype-history="back"]').click();
    assert(
      await page.evaluate(() => document.documentElement.dataset.prototypeHistoryAction) === "back",
      `${label}: back control is inert`,
    );

    await page.locator('[data-prototype-menu="文件"]').click();
    const menu = page.locator('.prototype-desktop-menu-popover[aria-label="文件菜单"]');
    await menu.waitFor({ state: "visible" });
    assert(await menu.getByRole("menuitem").count() === 3, `${label}: file menu did not open`);
    await page.keyboard.press("Escape");
    assert(await page.locator(".prototype-desktop-menu-popover").count() === 0, `${label}: desktop menu did not close`);
  }

  const expandedWidth = await setSidebarCollapsed(page, false, label);
  const collapsedWidth = await setSidebarCollapsed(page, true, label);
  assert(collapsedWidth < expandedWidth, `${label}: sidebar collapse did not reduce its width`);
  await setSidebarCollapsed(page, false, label);
}

async function assertConversationStructure(page, label) {
  const session = page.locator(".prototype-session");
  if (!await session.count()) return;
  const audit = await session.evaluate((element) => {
    const summary = element.querySelector(".prototype-session-summary");
    const body = element.querySelector(".prototype-session-body");
    const assistant = element.querySelector(".prototype-assistant-copy");
    const event = element.querySelector(".prototype-activity-event");
    const finalResponse = element.querySelector("[data-prototype-final-response]");
    const files = [...element.querySelectorAll(".prototype-file-link")];
    const complete = element.dataset.prototypeState === "complete";
    return {
      installed: element.dataset.prototypeActivityInstalled,
      mode: element.dataset.prototypeActivityMode,
      summaries: element.querySelectorAll(".prototype-session-summary").length,
      assistantBlocks: element.querySelectorAll(".prototype-assistant-copy").length,
      events: element.querySelectorAll(".prototype-activity-event").length,
      contextEvents: element.querySelectorAll('[data-prototype-event="context-compressed"]').length,
      subagentEvents: element.querySelectorAll('[data-prototype-event="subagents"]').length,
      runningEvents: element.querySelectorAll('[data-prototype-event="running-command"][aria-busy="true"]').length,
      files: files.length,
      fileIcons: files.filter((link) => link.querySelector("svg")).length,
      editorLinks: files.filter((link) => link.href.includes("scene=editor") && link.href.includes("file=")).length,
      completionActions: element.querySelectorAll(".prototype-completion-action").length,
      finalResponses: element.querySelectorAll("[data-prototype-final-response]").length,
      finalOutsideTrace: Boolean(finalResponse && body && !body.contains(finalResponse)),
      summaryExpanded: summary?.getAttribute("aria-expanded"),
      summaryDisabled: Boolean(summary?.disabled),
      summaryText: summary?.textContent?.trim() || "",
      bodyHidden: Boolean(body?.hidden),
      bodyBackground: body ? getComputedStyle(body).backgroundColor : null,
      assistantColor: assistant ? getComputedStyle(assistant).color : null,
      eventColor: event ? getComputedStyle(event).color : null,
      complete,
      readonly: element.classList.contains("is-readonly"),
    };
  });

  assert(audit.installed === "true" && audit.mode, `${label}: activity experience is not installed`);
  assert(audit.summaries === 1, `${label}: session summary missing`);
  assert(audit.assistantBlocks >= 6, `${label}: assistant narrative is incomplete`);
  assert(audit.events >= 4, `${label}: chronological tool events are incomplete`);
  assert(audit.contextEvents === 1, `${label}: context compression event is missing or duplicated`);
  assert(audit.subagentEvents === 1, `${label}: subagent event is missing or duplicated`);
  assert(audit.bodyBackground === "rgba(0, 0, 0, 0)", `${label}: session body became another card`);
  assert(audit.assistantColor !== audit.eventColor, `${label}: tool events are not visually de-emphasized`);
  if (audit.complete) {
    assert(audit.summaryText.startsWith("已处理"), `${label}: completion summary copy is incorrect`);
    assert(!audit.summaryDisabled, `${label}: completion summary cannot be expanded`);
    assert(audit.runningEvents === 0, `${label}: completion still exposes a live command`);
    assert(audit.finalResponses === 1 && audit.finalOutsideTrace, `${label}: final response is not independent from trace details`);
    assert(audit.files > 0 && audit.fileIcons === audit.files && audit.editorLinks === audit.files, `${label}: output file links are incomplete`);
    assert(audit.readonly ? audit.completionActions === 0 : audit.completionActions === 2, `${label}: review/undo actions are inconsistent`);
    if (audit.mode === "collapsed") {
      assert(audit.summaryExpanded === "false" && audit.bodyHidden, `${label}: completed trace did not start collapsed`);
    } else {
      assert(audit.summaryExpanded === "true" && !audit.bodyHidden, `${label}: completed trace did not start expanded`);
    }
  } else {
    assert(audit.summaryText.startsWith("正在处理"), `${label}: running summary copy is incorrect`);
    assert(audit.summaryDisabled && audit.summaryExpanded === "true", `${label}: running summary should stay open`);
    assert(audit.runningEvents === 1, `${label}: exactly one live command must be visible`);
    assert(audit.files === 0 && audit.finalResponses === 0 && audit.completionActions === 0, `${label}: running session exposes completion output early`);
  }
}

async function verifyCompletionInteractions(page, label) {
  const summary = page.locator(".prototype-session-summary");
  const body = page.locator(".prototype-session-body");
  const finalResponse = page.locator("[data-prototype-final-response]");
  await summary.click();
  assert(await body.isHidden(), `${label}: session summary did not collapse`);
  assert(await finalResponse.isVisible(), `${label}: collapsing trace hid the final response`);
  await summary.click();
  assert(await body.isVisible(), `${label}: session summary did not expand`);
  assert(await finalResponse.isVisible(), `${label}: final response disappeared after re-expanding trace`);

  await page.locator(".prototype-review-action").click();
  await page.waitForFunction(() => (
    document.querySelector('[data-testid="workbench-panel"]')?.dataset.workbenchKind === "review"
  ));
  await page.waitForFunction(() => (
    document.querySelector(".prototype-review-action")?.getAttribute("aria-pressed") === "true"
  ));
  assert(await page.locator(".prototype-review-action").getAttribute("aria-pressed") === "true", `${label}: review action did not become active`);

  await page.locator(".prototype-undo-action").click();
  const undoState = await page.locator(".prototype-session").getAttribute("data-prototype-state");
  assert(undoState === "undone", `${label}: undo did not apply directly`);
}

async function assertActivityScenario(page, scenario, label) {
  const session = page.locator(".prototype-session");
  const body = session.locator(".prototype-session-body");
  const summary = session.locator(".prototype-session-summary");
  const finalResponse = session.locator("[data-prototype-final-response]");
  assert(await session.getAttribute("data-prototype-activity-mode") === scenario.mode, `${label}: activity mode mismatch`);

  const common = await session.evaluate((element) => {
    const bodyElement = element.querySelector(".prototype-session-body");
    const context = element.querySelector('[data-prototype-event="context-compressed"]');
    const subagents = element.querySelector('[data-prototype-event="subagents"]');
    const events = [...element.querySelectorAll(".prototype-activity-event")];
    return {
      contextInTrace: Boolean(context && bodyElement?.contains(context)),
      subagentsInTrace: Boolean(subagents && bodyElement?.contains(subagents)),
      contextText: context?.textContent?.trim() || "",
      subagentText: subagents?.textContent?.replace(/\s+/g, " ").trim() || "",
      flatEvents: events.every((event) => (
        getComputedStyle(event).backgroundColor === "rgba(0, 0, 0, 0)"
        && getComputedStyle(event).borderTopWidth === "0px"
      )),
    };
  });
  assert(common.contextInTrace && common.contextText === "上下文已自动压缩", `${label}: context compression is not a chronological event`);
  assert(common.subagentsInTrace && common.subagentText.includes("Prototype pipeline") && common.subagentText.includes("已完成"), `${label}: subagent event lacks visible status`);
  assert(common.flatEvents, `${label}: collapsed events gained unnecessary card surfaces`);

  if (scenario.mode === "running") {
    const live = session.locator('[data-prototype-event="running-command"][aria-busy="true"]');
    assert(await live.count() === 1 && await live.isVisible(), `${label}: current command is not the only live row`);
    assert((await live.innerText()).trim().startsWith("Ran "), `${label}: live command summary is malformed`);
    const runningText = live.locator(".prototype-running-command-text");
    const staticState = await runningText.evaluate((element) => ({
      animation: getComputedStyle(element).animationName,
      color: getComputedStyle(element).color,
    }));
    assert(staticState.animation === "none", `${label}: static prototype unexpectedly animates the current command`);
    assert(staticState.color !== "rgba(0, 0, 0, 0)", `${label}: static running command is not legible`);
    return;
  }

  assert(await finalResponse.isVisible(), `${label}: completed state hides the final answer`);
  if (scenario.mode === "collapsed") {
    assert(await summary.getAttribute("aria-expanded") === "false" && await body.isHidden(), `${label}: trace is not collapsed`);
    await summary.click();
    assert(await body.isVisible() && await finalResponse.isVisible(), `${label}: collapsed trace did not reopen cleanly`);
    await summary.click();
    assert(await body.isHidden() && await finalResponse.isVisible(), `${label}: final answer was coupled to trace visibility`);
    return;
  }

  assert(await body.isVisible(), `${label}: completed trace is unexpectedly hidden`);
  if (scenario.mode === "expanded") {
    const openDetails = await session.locator(".prototype-activity-details").evaluateAll((elements) => elements
      .filter((element) => {
        const rect = element.getBoundingClientRect();
        return rect.width > 0 && rect.height > 0;
      })
      .map((element) => ({ id: element.id, hidden: element.hidden, parent: element.parentElement?.dataset.prototypeEvent || "child" })));
    assert(openDetails.length === 0, `${label}: a detail card opened without user intent (${JSON.stringify(openDetails)})`);
    await summary.click();
    assert(await body.isHidden() && await finalResponse.isVisible(), `${label}: completed trace did not collapse`);
    await summary.click();
    assert(await body.isVisible(), `${label}: completed trace did not expand again`);
    return;
  }

  if (scenario.mode === "multi") {
    const event = session.locator('[data-prototype-event="multi-command"]');
    const details = event.locator(":scope > .prototype-activity-details");
    const children = details.locator("[data-prototype-child-command]");
    assert(await details.isVisible() && await children.count() === 8, `${label}: multiple-command list is incomplete`);
    const overflow = await details.evaluate((element) => ({
      clientHeight: element.clientHeight,
      scrollHeight: element.scrollHeight,
      overflowY: getComputedStyle(element).overflowY,
    }));
    assert(overflow.scrollHeight > overflow.clientHeight && overflow.overflowY === "auto",
      `${label}: long command list does not use bounded internal scrolling`);
    assert(await session.locator(".prototype-shell-card:visible").count() === 0, `${label}: nested Shell opened before selection`);
    const first = children.first().locator(".prototype-activity-child-command");
    await first.click();
    assert(await session.locator(".prototype-shell-card:visible").count() === 1, `${label}: child command could not reveal its Shell output`);
    await first.click();
    assert(await session.locator(".prototype-shell-card:visible").count() === 0, `${label}: child Shell output did not collapse`);
    return;
  }

  if (scenario.mode === "single") {
    const event = session.locator('[data-prototype-event="single-command"]');
    assert(await event.locator(":scope > .prototype-activity-details").isVisible(), `${label}: single command detail is hidden`);
    assert(await session.locator(".prototype-shell-card:visible").count() === 1, `${label}: single Shell output is missing or duplicated`);
    assert(await session.locator('[data-prototype-event="multi-command"] > .prototype-activity-details').isHidden(), `${label}: unrelated multiple-command detail is open`);
    return;
  }

  if (scenario.mode === "file") {
    const event = session.locator('[data-prototype-event="file-edit"]');
    const diff = event.locator(".prototype-diff-card");
    assert(await diff.isVisible() && await session.locator(".prototype-diff-card:visible").count() === 1, `${label}: single-file diff is missing or duplicated`);
    assert(await diff.locator(".prototype-diff-line.is-add").count() >= 1, `${label}: diff has no semantic added lines`);
    await diff.locator(".prototype-copy-patch").click();
    assert(await page.evaluate(() => document.documentElement.dataset.prototypePatchCopied) === "true", `${label}: diff copy action is inert`);
  }
}

async function positionActivityScenario(page, mode) {
  await page.evaluate((activityMode) => {
    if (document.activeElement instanceof HTMLElement) document.activeElement.blur();
    const timeline = document.querySelector(".timeline");
    document.documentElement.scrollTop = 0;
    document.body.scrollTop = 0;
    if (!(timeline instanceof HTMLElement)) return;
    timeline.scrollTop = 0;
    const selectors = {
      running: '[data-prototype-event="running-command"]',
      multi: '[data-prototype-event="multi-command"]',
      single: '[data-prototype-event="single-command"]',
      file: '[data-prototype-event="file-edit"]',
    };
    const selector = selectors[activityMode];
    if (!selector) return;
    const target = document.querySelector(selector);
    if (!(target instanceof HTMLElement)) return;
    const timelineRect = timeline.getBoundingClientRect();
    const targetRect = target.getBoundingClientRect();
    const centeredTop = targetRect.top - timelineRect.top - ((timeline.clientHeight - targetRect.height) / 2);
    timeline.scrollTop = Math.max(0, Math.min(timeline.scrollHeight - timeline.clientHeight, timeline.scrollTop + centeredTop));
  }, mode);
  await settlePrototype(page);
}

function taskRow(page, title) {
  return page.locator(".sidebar-task-row").filter({ hasText: title }).first();
}

async function revealInlineActions(page, row, label) {
  await row.hover();
  await page.waitForFunction((title) => {
    const rows = [...document.querySelectorAll(".sidebar-task-row")];
    const target = rows.find((candidate) => candidate.textContent?.includes(title));
    const actions = target?.querySelector(":scope > .prototype-row-actions");
    return actions
      && actions.querySelectorAll("button").length === 2
      && Number(getComputedStyle(actions).opacity) === 1
      && getComputedStyle(actions).pointerEvents === "auto";
  }, label);
  assert(await page.locator(".task-actions-popover").count() === 0, `${label}: hover opened a menu`);
}

async function renderPairedScenario(browser, theme, scenario, browserErrors) {
  const label = `${scenario.slug}/${theme.key}`;
  const page = await openPage(
    browser,
    theme,
    { width: 1600, height: 1000 },
    scenario.query,
    ".scene-room",
    label,
    browserErrors,
  );
  try {
    await assertDefaultActionsHidden(page, label);
    await assertPaletteHierarchy(page, theme, label);
    await assertConversationStructure(page, label);
    await assertDesktopShell(page, label);
    const expectedSidebarState = scenario.slug === "review-collapsed"
      ? "context"
      : scenario.slug === "subagents"
        ? "list"
        : scenario.slug === "subagent-detail"
          ? "detail"
          : null;
    const isWorkbenchTabScenario = scenario.slug.startsWith("workbench-");
    if (expectedSidebarState) await assertSidebarView(page, expectedSidebarState, label);
    if (isWorkbenchTabScenario) {
      const expected = scenario.slug === "workbench-multi-tabs"
        ? "tabs"
        : scenario.slug === "workbench-tab-fallback"
          ? "fallback"
          : "launcher";
      await assertWorkbenchTabState(page, expected, label);
    }
    if (scenario.slug === "model-configuration" || scenario.slug === "codex-configuration") {
      await page.locator(".model-config-trigger").click();
      await page.locator(".model-config-menu").waitFor({ state: "visible" });
      const labels = await page.locator(".model-config-row > span:first-child").allTextContents();
      const expectedRows = scenario.slug === "model-configuration"
        ? ["模型", "思考模式", "推理强度"]
        : ["模型", "推理强度", "输出详略"];
      for (const expected of expectedRows) {
        assert(labels.map((value) => value.trim()).includes(expected), `${label}: ${expected} config is missing`);
      }
    }
    await capture(page, theme, `${scenario[theme.key]}-${scenario.slug}-${theme.key}.png`);
    if (expectedSidebarState) await verifySidebarExperience(page, scenario, label);
    if (isWorkbenchTabScenario) await verifyWorkbenchTabExperience(page, scenario, label);
    if (scenario.slug === "launcher") {
      await verifyCompletionInteractions(page, label);
      await verifyDesktopChromeInteractions(page, label);
    }
  } finally {
    await page.close();
  }
}

async function renderActivityScenario(browser, theme, scenario, browserErrors) {
  const label = `${scenario.slug}/${theme.key}`;
  const page = await openPage(
    browser,
    theme,
    { width: 1600, height: 1000 },
    scenario.query,
    ".scene-room",
    label,
    browserErrors,
  );
  try {
    await assertDefaultActionsHidden(page, label);
    await assertPaletteHierarchy(page, theme, label);
    await assertConversationStructure(page, label);
    await assertDesktopShell(page, label);
    await assertActivityScenario(page, scenario, label);
    await positionActivityScenario(page, scenario.mode);
    assert(await page.locator(".composer").isVisible(), `${label}: composer disappeared from the interaction flow`);
    const viewportAudit = await page.evaluate(() => {
      const rect = (selector) => {
        const box = document.querySelector(selector)?.getBoundingClientRect();
        return box ? [box.x, box.y, box.width, box.height] : null;
      };
      const strayScroll = [...document.querySelectorAll("*")]
        .filter((element) => element !== document.querySelector(".timeline") && (element.scrollTop > 0 || element.scrollLeft > 0))
        .map((element) => `${element.tagName.toLowerCase()}.${element.className || ""}:${element.scrollLeft},${element.scrollTop}`);
      return {
        overflow: Math.max(0, document.documentElement.scrollWidth - innerWidth),
        app: rect("#app"),
        topbar: rect(".app-topbar"),
        sidebar: rect(".app-sidebar"),
        main: rect("#main-content"),
        strayScroll,
      };
    });
    assert(viewportAudit.overflow <= 1, `${label}: activity detail caused ${viewportAudit.overflow}px horizontal page overflow`);
    assert(viewportAudit.strayScroll.length === 0, `${label}: activity interaction scrolled the app shell (${viewportAudit.strayScroll.join(" | ")})`);
    assert(
      viewportAudit.app?.[1] === 0 && viewportAudit.topbar?.[1] === 0 && viewportAudit.sidebar?.[1] >= 40 && viewportAudit.main?.[1] >= 40,
      `${label}: app shell moved out of frame (${JSON.stringify(viewportAudit)})`,
    );
    await capture(page, theme, `${scenario[theme.key]}-${scenario.slug}-${theme.key}.png`);
  } finally {
    await page.close();
  }
}

async function renderConversationActions(browser, theme, browserErrors) {
  const label = `conversation-actions/${theme.key}`;
  const page = await openPage(
    browser,
    theme,
    { width: 1600, height: 1000 },
    "scene=room&task=complete&state=hidden",
    ".scene-room",
    label,
    browserErrors,
  );
  try {
    await assertDefaultActionsHidden(page, label);
    await assertPaletteHierarchy(page, theme, label);
    await assertConversationStructure(page, label);
    await assertDesktopShell(page, label);
    const row = taskRow(page, "更新依赖并修复告警");
    await revealInlineActions(page, row, "更新依赖并修复告警");
    const number = theme.key === "light" ? "13" : "14";
    await capture(page, theme, `${number}-conversation-actions-${theme.key}.png`);
  } finally {
    await page.close();
  }
}

async function renderPinnedConversation(browser, theme, browserErrors) {
  const label = `conversation-pinned/${theme.key}`;
  const page = await openPage(
    browser,
    theme,
    { width: 1600, height: 1000 },
    "scene=room&task=complete&state=hidden",
    ".scene-room",
    label,
    browserErrors,
  );
  try {
    await assertConversationStructure(page, label);
    await assertDesktopShell(page, label);
    const row = taskRow(page, "更新依赖并修复告警");
    await revealInlineActions(page, row, "更新依赖并修复告警");
    await row.locator(".prototype-action-pin").click();
    await revealInlineActions(page, row, "更新依赖并修复告警");

    const pinnedState = await row.evaluate((element) => ({
      pinned: element.classList.contains("prototype-pinned"),
      pressed: element.querySelector(".prototype-action-pin")?.getAttribute("aria-pressed"),
      first: element.parentElement?.firstElementChild === element,
    }));
    assert(pinnedState.pinned && pinnedState.pressed === "true" && pinnedState.first, `${label}: pin did not apply directly`);

    const number = theme.key === "light" ? "15" : "16";
    await capture(page, theme, `${number}-conversation-pinned-${theme.key}.png`);
  } finally {
    await page.close();
  }
}

async function renderArchivedReadOnly(browser, theme, browserErrors) {
  const label = `archived-readonly/${theme.key}`;
  const page = await openPage(
    browser,
    theme,
    { width: 1600, height: 1000 },
    "scene=conversations",
    ".scene-conversations",
    label,
    browserErrors,
  );
  try {
    const row = page.locator(".conversation-row").filter({ hasText: "更新依赖并修复告警" }).first();
    await row.hover();
    await row.locator(".prototype-action-archive").click();
    await page.getByText("对话已归档", { exact: true }).waitFor({ state: "visible" });
    assert(await page.getByRole("alertdialog").count() === 0, `${label}: archive unexpectedly requested confirmation`);

    await page.getByRole("tab", { name: "已归档" }).click();
    const archivedRow = page.locator(".conversation-row").filter({ hasText: "更新依赖并修复告警" }).first();
    await archivedRow.waitFor({ state: "visible" });
    await archivedRow.locator(".conversation-main").click();
    const note = page.locator(".room-archived-note");
    await note.waitFor({ state: "visible" });
    await note.evaluate((element) => {
      element.textContent = "此对话已归档，只能查看历史。";
    });

    const toastClose = page.locator(".toast-close");
    if (await toastClose.count()) await toastClose.click();
    await installPrototypeActions(page);
    await installPrototypeConversation(page);
    await installPrototypeActivityExperience(page);
    await installPrototypeDesktopChrome(page);
    await settlePrototype(page);
    await assertDefaultActionsHidden(page, label);
    await assertConversationStructure(page, label);
    await assertDesktopShell(page, label);
    assert(!(await page.locator("body").innerText()).includes("永久删除"), `${label}: obsolete permanent-delete copy remains`);

    const number = theme.key === "light" ? "17" : "18";
    await capture(page, theme, `${number}-archived-readonly-${theme.key}.png`);
  } finally {
    await page.close();
  }
}

async function renderCompactRoom(browser, theme, browserErrors) {
  const label = `compact-room/${theme.key}`;
  const page = await openPage(
    browser,
    theme,
    { width: 1200, height: 800 },
    "scene=room&task=complete&state=hidden",
    ".scene-room",
    label,
    browserErrors,
  );
  try {
    await assertDefaultActionsHidden(page, label);
    await assertPaletteHierarchy(page, theme, label);
    await assertConversationStructure(page, label);
    await setSidebarCollapsed(page, true, label);
    await assertDesktopShell(page, label);
    const layout = await page.evaluate(() => {
      const main = document.querySelector("#main-content");
      const room = document.querySelector("#main-content > .scene-room");
      if (!(main instanceof HTMLElement) || !(room instanceof HTMLElement)) throw new Error("room layout missing");
      const mainRect = main.getBoundingClientRect();
      const roomRect = room.getBoundingClientRect();
      return {
        main: [mainRect.x, mainRect.y, mainRect.width, mainRect.height],
        room: [roomRect.x, roomRect.y, roomRect.width, roomRect.height],
        overflow: [
          Math.max(0, document.documentElement.scrollWidth - innerWidth),
          Math.max(0, document.documentElement.scrollHeight - innerHeight),
        ],
      };
    });
    assert(
      layout.main.every((value, index) => Math.abs(value - layout.room[index]) <= 1),
      `${label}: room does not fill main (${layout.main.join(",")} vs ${layout.room.join(",")})`,
    );
    assert(layout.overflow.every((value) => value <= 1), `${label}: page overflow ${layout.overflow.join(",")}`);

    const number = theme.key === "light" ? "19" : "20";
    await capture(page, theme, `${number}-compact-room-${theme.key}.png`);
  } finally {
    await page.close();
  }
}

async function renderSupplementalDarkScenario(browser, theme, scenario, browserErrors) {
  const label = `${scenario.slug}/${theme.key}`;
  const page = await openPage(
    browser,
    theme,
    { width: 1600, height: 1000 },
    scenario.query,
    ".scene-room",
    label,
    browserErrors,
  );
  try {
    await assertDefaultActionsHidden(page, label);
    await assertPaletteHierarchy(page, theme, label);
    await assertDesktopShell(page, label);
    const flow = page.locator(`[data-prototype-flow="${scenario.mode}"]`);
    await flow.waitFor({ state: "visible" });

    const structure = await flow.evaluate((element, mode) => ({
      mode: element.getAttribute("data-prototype-flow"),
      actions: element.querySelectorAll("button").length,
      inputs: element.querySelectorAll("input").length,
      shimmer: getComputedStyle(document.documentElement).getPropertyValue("--prototype-demo-shimmer").trim(),
      pageOverflow: Math.max(0, document.documentElement.scrollWidth - innerWidth),
    }), scenario.mode);
    assert(structure.mode === scenario.mode, `${label}: supplemental state mismatch`);
    assert(structure.pageOverflow <= 1, `${label}: supplemental state caused horizontal overflow`);
    assert(structure.shimmer === "", `${label}: static prototype unexpectedly enabled command shimmer`);
    if (scenario.mode === "approval") assert(structure.actions === 3, `${label}: approval must expose three decisions`);
    if (scenario.mode === "failure") assert(structure.actions === 2, `${label}: failure must expose two recovery actions`);
    if (scenario.mode === "empty") assert(structure.actions === 3, `${label}: empty task must expose three starting prompts`);
    if (scenario.mode === "context") assert(structure.inputs === 1, `${label}: context picker search is missing`);

    await capture(page, theme, `${scenario.dark}-${scenario.slug}-dark.png`);

    if (scenario.mode === "approval") {
      await flow.getByRole("button", { name: "允许一次" }).click();
      assert(await page.locator("html[data-prototype-flow-state='allowed-once']").count() === 1,
        `${label}: allow-once decision did not apply`);
    }
    if (scenario.mode === "failure") {
      await flow.getByRole("button", { name: "重试命令" }).click();
      assert(await page.locator("html[data-prototype-flow-state='retrying']").count() === 1,
        `${label}: retry action did not apply`);
    }
  } finally {
    await page.close();
  }
}

async function renderPrototypeImages() {
  const baselineDarkPngs = signatureDarkMode ? prepareSignatureRender() : null;
  const executablePath = findBrowserExecutable();
  let browser = null;
  const browserErrors = [];
  let published = !signatureDarkMode;
  try {
    browser = await chromium.launch({ headless: true, ...(executablePath ? { executablePath } : {}) });
    for (const theme of themes) {
      for (const scenario of pairedScenarios) {
        await renderPairedScenario(browser, theme, scenario, browserErrors);
      }
      await renderConversationActions(browser, theme, browserErrors);
      await renderPinnedConversation(browser, theme, browserErrors);
      await renderArchivedReadOnly(browser, theme, browserErrors);
      await renderCompactRoom(browser, theme, browserErrors);
      for (const scenario of activityScenarios) {
        await renderActivityScenario(browser, theme, scenario, browserErrors);
      }
      if (theme.key === "dark") {
        for (const scenario of supplementalDarkScenarios) {
          await renderSupplementalDarkScenario(browser, theme, scenario, browserErrors);
        }
      }
    }
    if (browserErrors.length) throw new Error(`browser errors:\n${browserErrors.join("\n")}`);
    if (signatureDarkMode) {
      publishSignatureRender(baselineDarkPngs);
      published = true;
    }
  } finally {
    if (browser) await browser.close();
    if (signatureDarkMode && !published) cleanupSignatureRender();
  }
}

if (invokedDirectly) {
  renderPrototypeImages().catch((error) => {
    console.error(error);
    process.exitCode = 1;
  });
}

module.exports = {
  prototypeCss,
  installPrototypeDesktopChrome,
  installPrototypeActions,
  installPrototypeConversation,
  settlePrototype,
  pairedScenarios,
  activityScenarios,
  supplementalDarkScenarios,
  renderPrototypeImages,
};
