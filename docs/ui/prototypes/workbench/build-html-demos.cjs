const fs = require("fs");
const path = require("path");
const {
  prototypeCss,
  installPrototypeDesktopChrome,
  installPrototypeActions,
  installPrototypeConversation,
  settlePrototype,
  pairedScenarios,
  activityScenarios,
  supplementalDarkScenarios,
} = require("./render-workbench.cjs");
const { sidebarExperienceCss, installPrototypeSidebarExperience } = require("./sidebar-experience.cjs");
const { activityExperienceCss, installPrototypeActivityExperience } = require("./activity-experience.cjs");
const { resilienceExperienceCss, installPrototypeResilienceExperience } = require("./resilience-experience.cjs");
const { signatureDarkCss, installSignatureDark } = require("./signature-dark.cjs");

const prototypeRoot = __dirname;

const scenarioCopy = {
  launcher: ["工作台", "任务启动器"],
  subagents: ["右侧栏", "子智能体列表"],
  terminal: ["工作台", "终端"],
  files: ["工作台", "文件"],
  review: ["审核", "审核变更"],
  "review-collapsed": ["右侧栏", "上下文简面板"],
  "subagent-detail": ["右侧栏", "子智能体详情"],
  "conversation-actions": ["会话管理", "悬停操作"],
  "conversation-pinned": ["会话管理", "会话已置顶"],
  "archived-readonly": ["会话管理", "归档后只读"],
  "compact-room": ["窗口布局", "紧凑窗口"],
  "event-running": ["运行过程", "回答与命令交替"],
  "event-complete-collapsed": ["运行过程", "完成并折叠过程"],
  "event-complete-expanded": ["运行过程", "完成并展开过程"],
  "event-multi-command-expanded": ["运行过程", "展开多个命令"],
  "event-shell-expanded": ["运行过程", "展开 Shell 输出"],
  "event-single-file-diff-expanded": ["运行过程", "展开单文件 Diff"],
  "approval-required": ["恢复与许可", "等待人工许可"],
  "command-failed": ["恢复与许可", "命令失败与重试"],
  "new-task": ["开始任务", "新任务空态"],
  "context-picker": ["开始任务", "引用上下文文件"],
};

const demoScenarios = [
  ...pairedScenarios.map((scenario) => ({
    id: scenario.slug,
    number: scenario.dark,
    query: scenario.query,
  })),
  { id: "conversation-actions", number: "14", query: "scene=room&task=complete&state=hidden", prepare: "actions" },
  { id: "conversation-pinned", number: "16", query: "scene=room&task=complete&state=hidden", prepare: "pinned" },
  { id: "archived-readonly", number: "18", query: "scene=conversations", prepare: "archived" },
  { id: "compact-room", number: "20", query: "scene=room&task=complete&state=hidden", prepare: "compact" },
  ...activityScenarios.map((scenario) => ({
    id: scenario.slug,
    number: scenario.dark,
    query: scenario.query,
  })),
  ...supplementalDarkScenarios.map((scenario) => ({
    id: scenario.slug,
    number: scenario.dark,
    query: scenario.query,
  })),
].sort((left, right) => Number(left.number) - Number(right.number))
  .map((scenario) => {
    const [group, label] = scenarioCopy[scenario.id] || ["其他", scenario.id];
    return { ...scenario, group, label };
  });

const demoChromeCss = `
  :root[data-prototype-demo-variant="signature"]:not([data-prototype-demo-ready]) body {
    visibility: hidden;
  }

  .prototype-demo-trigger {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    min-width: 92px;
    min-height: 32px;
    margin-left: auto;
    padding: 0 10px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: color-mix(in srgb, var(--bg-panel) 82%, transparent);
    color: var(--fg-muted);
    font-family: "Cascadia Mono", "JetBrains Mono", ui-monospace, monospace;
    font-size: 10px;
    font-weight: 650;
    letter-spacing: .03em;
    cursor: pointer;
  }

  .prototype-demo-trigger:hover,
  .prototype-demo-trigger:focus-visible,
  .prototype-demo-trigger[aria-expanded="true"] {
    border-color: var(--border-strong);
    color: var(--fg);
  }

  .prototype-demo-trigger strong {
    color: var(--accent);
    font: inherit;
  }

  .prototype-demo-panel {
    position: fixed;
    z-index: 5000;
    top: 46px;
    left: max(12px, calc(var(--sidebar-w, 288px) + 12px));
    width: min(390px, calc(100vw - 28px));
    max-height: min(720px, calc(100vh - 62px));
    border: 1px solid var(--border-strong);
    border-radius: 14px;
    background: color-mix(in srgb, var(--bg-panel) 97%, transparent);
    box-shadow: 0 22px 72px rgba(0, 0, 0, .34);
    overflow: hidden;
  }

  .prototype-demo-panel[hidden] {
    display: none;
  }

  .prototype-demo-panel-head {
    display: flex;
    align-items: flex-start;
    gap: 12px;
    padding: 15px 16px 13px;
    border-bottom: 1px solid var(--border);
  }

  .prototype-demo-panel-head div {
    min-width: 0;
    margin-right: auto;
  }

  .prototype-demo-panel-head strong,
  .prototype-demo-panel-head small {
    display: block;
  }

  .prototype-demo-panel-head strong {
    color: var(--fg);
    font-size: 13px;
  }

  .prototype-demo-panel-head small {
    margin-top: 3px;
    color: var(--fg-muted);
    font-size: 10px;
    line-height: 1.5;
  }

  .prototype-demo-panel-close {
    display: grid;
    place-items: center;
    width: 36px;
    height: 36px;
    border: 0;
    border-radius: 8px;
    background: transparent;
    color: var(--fg-muted);
    cursor: pointer;
  }

  .prototype-demo-panel-close:hover,
  .prototype-demo-panel-close:focus-visible {
    background: color-mix(in srgb, var(--fg) 7%, transparent);
    color: var(--fg);
  }

  .prototype-demo-groups {
    max-height: calc(min(720px, 100vh - 62px) - 112px);
    padding: 5px 8px 10px;
    overflow: auto;
  }

  .prototype-demo-group-title {
    margin: 12px 8px 4px;
    color: var(--fg-subtle, var(--fg-muted));
    font-size: 10px;
    font-weight: 680;
    letter-spacing: .08em;
  }

  .prototype-demo-scenario {
    display: grid;
    grid-template-columns: 32px minmax(0, 1fr) auto;
    align-items: center;
    gap: 9px;
    width: 100%;
    min-height: 42px;
    padding: 0 9px;
    border: 0;
    border-radius: 8px;
    background: transparent;
    color: var(--fg-muted);
    text-align: left;
    cursor: pointer;
  }

  .prototype-demo-scenario:hover,
  .prototype-demo-scenario:focus-visible {
    background: color-mix(in srgb, var(--fg) 6%, transparent);
    color: var(--fg);
  }

  .prototype-demo-scenario[aria-current="true"] {
    background: color-mix(in srgb, var(--accent) 11%, transparent);
    color: var(--fg);
  }

  .prototype-demo-number {
    color: var(--accent);
    font-family: "Cascadia Mono", "JetBrains Mono", ui-monospace, monospace;
    font-size: 10px;
    font-variant-numeric: tabular-nums;
  }

  .prototype-demo-scenario small {
    color: var(--fg-subtle, var(--fg-muted));
    font-size: 9px;
  }

  .prototype-demo-panel-foot {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    padding: 9px 16px;
    border-top: 1px solid var(--border);
    color: var(--fg-subtle, var(--fg-muted));
    font-family: "Cascadia Mono", "JetBrains Mono", ui-monospace, monospace;
    font-size: 9px;
  }

  :root[data-prototype-signature="r-code"] .prototype-demo-trigger,
  :root[data-prototype-signature="r-code"] .prototype-demo-panel {
    border-radius: 14px 5px 14px 5px;
  }

  :root[data-prototype-signature="r-code"] .prototype-demo-scenario[aria-current="true"] {
    box-shadow: inset 2px 0 #f0783b;
    background: linear-gradient(90deg, rgba(240, 120, 59, .12), transparent 72%);
  }

  :root[data-sidebar-collapsed="true"] .prototype-demo-panel,
  .sidebar-collapsed .prototype-demo-panel {
    left: 12px;
  }

  @media (max-width: 900px) {
    .prototype-demo-panel {
      left: 12px;
    }

    .prototype-demo-trigger span {
      display: none;
    }

    .prototype-demo-trigger {
      min-width: 48px;
    }
  }
`;

function applyScenarioBeforeApp(config, scenarios) {
  document.documentElement.dataset.prototypeDemoVariant = config.variant;
  const params = new URLSearchParams(window.location.search);
  const requested = params.get("demo") || config.defaultScenario;
  const scenario = scenarios.find((candidate) => candidate.id === requested) || scenarios[0];
  const controlledKeys = [
    "scene", "task", "state", "project", "file", "prototypePanel",
    "prototypeActivity", "prototypeFlow",
  ];
  controlledKeys.forEach((key) => params.delete(key));
  const scenarioParams = new URLSearchParams(scenario.query);
  scenarioParams.forEach((value, key) => params.set(key, value));
  if (!params.has("scene")) params.set("scene", "room");
  params.set("theme", "dark");
  params.set("reset", "1");
  params.set("demo", scenario.id);
  history.replaceState(null, "", `${location.pathname}?${params.toString()}${location.hash}`);
  config.scenario = scenario.id;
}

async function bootWorkbenchDemo({ config, scenarios, installers }) {
  const root = document.documentElement;
  root.dataset.prototypeHtmlDemo = "true";
  root.dataset.prototypeDemoVariant = config.variant;

  const page = {
    evaluate: async (callback, ...args) => callback(...args),
  };
  const sleep = (milliseconds) => new Promise((resolve) => window.setTimeout(resolve, milliseconds));
  const waitFor = async (predicate, timeout = 10_000) => {
    const started = performance.now();
    while (performance.now() - started < timeout) {
      const result = predicate();
      if (result) return result;
      await sleep(40);
    }
    throw new Error("Timed out waiting for the interactive prototype to become ready.");
  };
  const currentScenario = () => scenarios.find((scenario) => scenario.id === config.scenario) || scenarios[0];

  const applyInstallers = async () => {
    if (!document.querySelector("#app")) return;
    await installers.desktop(page);
    await installers.actions(page);
    await installers.conversation(page);
    await installers.activity(page);
    await installers.resilience(page);
    await installers.sidebar(page);
    if (config.variant === "signature") await installers.signature(page);
    await installers.settle(page);
  };

  const taskRow = (title) => {
    const matches = [...document.querySelectorAll(".sidebar-task-row, .conversation-row")]
      .filter((row) => row.textContent?.includes(title));
    return matches.find((row) => {
      const style = getComputedStyle(row);
      const rect = row.getBoundingClientRect();
      return style.display !== "none" && style.visibility !== "hidden" && rect.width > 0 && rect.height > 0;
    }) || matches[0];
  };

  const prepareScenario = async () => {
    const scenario = currentScenario();
    if (!scenario.prepare || root.dataset.prototypeDemoPrepared === scenario.id) return;
    root.dataset.prototypeDemoPrepared = scenario.id;

    if (scenario.prepare === "actions" || scenario.prepare === "pinned") {
      const row = taskRow("更新依赖并修复告警") || document.querySelector(".sidebar-task-row.active");
      const pin = row?.querySelector(".prototype-action-pin");
      if (scenario.prepare === "pinned" && pin?.getAttribute("aria-pressed") !== "true") pin?.click();
      pin?.focus({ preventScroll: true });
      return;
    }

    if (scenario.prepare === "compact") {
      const sidebar = document.querySelector(".app-sidebar");
      if (sidebar && sidebar.getBoundingClientRect().width > 100) {
        document.querySelector(".desktop-sidebar-toggle")?.click();
      }
      return;
    }

    if (scenario.prepare === "archived") {
      const visibleConversationRow = () => [...document.querySelectorAll(".conversation-row")]
        .find((candidate) => {
          const rect = candidate.getBoundingClientRect();
          return candidate.textContent?.includes("更新依赖并修复告警") && rect.width > 0 && rect.height > 0;
        });
      const row = visibleConversationRow();
      row?.querySelector(".prototype-action-archive")?.click();
      await waitFor(() => root.dataset.prototypeArchiveState === "committed");
      [...document.querySelectorAll("[role='tab']")]
        .find((tab) => tab.textContent?.trim() === "已归档")?.click();
      const archivedRow = await waitFor(visibleConversationRow);
      (archivedRow.querySelector(".conversation-main") || archivedRow).click();
      const note = await waitFor(() => document.querySelector(".room-archived-note"));
      note.textContent = "此对话已归档，只能查看历史。";
      document.querySelector(".toast-close")?.click();
    }
  };

  const navigateToScenario = (id) => {
    const url = new URL(window.location.href);
    url.searchParams.set("demo", id);
    window.location.assign(url.href);
  };

  const installDemoNavigator = () => {
    if (new URL(window.location.href).searchParams.get("demoControls") === "0") return;
    if (document.querySelector(".prototype-demo-trigger")) return;
    const topbar = document.querySelector(".app-topbar");
    if (!topbar) return;

    const scenario = currentScenario();
    const trigger = document.createElement("button");
    trigger.type = "button";
    trigger.className = "prototype-demo-trigger";
    trigger.setAttribute("aria-expanded", "false");
    trigger.setAttribute("aria-controls", "prototype-demo-panel");
    trigger.innerHTML = `<strong>${scenario.number}</strong><span>原型场景</span><small>${scenarios.indexOf(scenario) + 1}/${scenarios.length}</small>`;

    const groups = [...new Set(scenarios.map((item) => item.group))];
    const panel = document.createElement("aside");
    panel.id = "prototype-demo-panel";
    panel.className = "prototype-demo-panel";
    panel.hidden = true;
    panel.setAttribute("aria-label", "原型场景导航");
    panel.innerHTML = `
      <header class="prototype-demo-panel-head">
        <div><strong>${config.title}</strong><small>21 个真实状态；使用 [ 和 ] 快速切换。</small></div>
        <button type="button" class="prototype-demo-panel-close" aria-label="关闭场景导航">×</button>
      </header>
      <div class="prototype-demo-groups">
        ${groups.map((group) => `
          <section>
            <h3 class="prototype-demo-group-title">${group}</h3>
            ${scenarios.filter((item) => item.group === group).map((item) => `
              <button type="button" class="prototype-demo-scenario" data-demo-scenario="${item.id}" aria-current="${item.id === scenario.id}">
                <span class="prototype-demo-number">${item.number}</span><span>${item.label}</span><small>${item.id}</small>
              </button>`).join("")}
          </section>`).join("")}
      </div>
      <footer class="prototype-demo-panel-foot"><span>${config.variantLabel}</span><span>Esc 关闭</span></footer>`;

    const windowControls = topbar.querySelector(".app-window-controls");
    topbar.insertBefore(trigger, windowControls || null);
    document.body.append(panel);

    const setOpen = (open) => {
      panel.hidden = !open;
      trigger.setAttribute("aria-expanded", String(open));
      if (open) panel.querySelector(`[data-demo-scenario="${scenario.id}"]`)?.focus({ preventScroll: true });
    };
    trigger.addEventListener("click", () => setOpen(panel.hidden));
    panel.querySelector(".prototype-demo-panel-close")?.addEventListener("click", () => setOpen(false));
    panel.querySelectorAll("[data-demo-scenario]").forEach((button) => {
      button.addEventListener("click", () => navigateToScenario(button.dataset.demoScenario));
    });
    document.addEventListener("pointerdown", (event) => {
      if (!panel.hidden && !panel.contains(event.target) && !trigger.contains(event.target)) setOpen(false);
    });
    document.addEventListener("keydown", (event) => {
      if (event.key === "Escape" && !panel.hidden) {
        setOpen(false);
        trigger.focus();
        return;
      }
      if (event.target instanceof HTMLInputElement || event.target instanceof HTMLTextAreaElement || event.target?.isContentEditable) return;
      if (event.key !== "[" && event.key !== "]") return;
      const index = scenarios.indexOf(currentScenario());
      const direction = event.key === "]" ? 1 : -1;
      navigateToScenario(scenarios[(index + direction + scenarios.length) % scenarios.length].id);
    });
  };

  try {
    await waitFor(() => window.__ready === true && document.querySelector("#app"));
    await applyInstallers();
    installDemoNavigator();
    await prepareScenario();
    root.dataset.prototypeDemoReady = "true";

    let applyTimer = 0;
    const observer = new MutationObserver((records) => {
      if (!records.some((record) => record.type === "childList" && record.addedNodes.length)) return;
      window.clearTimeout(applyTimer);
      applyTimer = window.setTimeout(async () => {
        try {
          await applyInstallers();
          installDemoNavigator();
        } catch (error) {
          console.error("Failed to refresh prototype enhancements", error);
        }
      }, 40);
    });
    observer.observe(document.querySelector("#root") || document.body, { childList: true, subtree: true });
  } catch (error) {
    root.dataset.prototypeDemoReady = "error";
    console.error(error);
  }
}

async function installSignatureSanitizer(page) {
  await page.evaluate(() => {
    const legacyBrand = ["Co", "dex"].join("");
    const legacyCli = `${legacyBrand} CLI`;
    const legacyModel = ["gpt", "-5", ".6"].join("");
    const replaceLegacyText = (value) => String(value || "")
      .split(legacyCli).join("R-Code Agent")
      .split(legacyBrand).join("R-Code")
      .split(`${legacyModel}-sol`).join("Auto")
      .split(legacyModel).join("Auto");

    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    const textNodes = [];
    while (walker.nextNode()) textNodes.push(walker.currentNode);
    textNodes.forEach((node) => {
      const nextValue = replaceLegacyText(node.nodeValue);
      if (nextValue !== node.nodeValue) node.nodeValue = nextValue;
    });
    document.querySelectorAll("[title], [aria-label]").forEach((element) => {
      for (const attribute of ["title", "aria-label"]) {
        if (!element.hasAttribute(attribute)) continue;
        element.setAttribute(attribute, replaceLegacyText(element.getAttribute(attribute)));
      }
    });
  });
}

function renderHtml({ variant, outputPath, assetPrefix, title, variantLabel }) {
  const config = { variant, title, variantLabel, defaultScenario: "launcher" };
  const combinedCss = [prototypeCss, sidebarExperienceCss, activityExperienceCss, resilienceExperienceCss, demoChromeCss];
  if (variant === "signature") combinedCss.push(signatureDarkCss);

  const runtimeSource = [
    `const installPrototypeDesktopChrome = ${installPrototypeDesktopChrome.toString()};`,
    `const installPrototypeActions = ${installPrototypeActions.toString()};`,
    `const installPrototypeConversation = ${installPrototypeConversation.toString()};`,
    `const installPrototypeActivityExperience = ${installPrototypeActivityExperience.toString()};`,
    `const installPrototypeResilienceExperience = ${installPrototypeResilienceExperience.toString()};`,
    `const installPrototypeSidebarExperience = ${installPrototypeSidebarExperience.toString()};`,
    `const installSignatureDark = ${installSignatureDark.toString()};`,
    `const installSignatureSanitizer = ${installSignatureSanitizer.toString()};`,
    `const settlePrototype = ${settlePrototype.toString()};`,
    `const demoScenarios = ${JSON.stringify(demoScenarios)};`,
    `(${bootWorkbenchDemo.toString()})({`,
    `  config: window.__workbenchDemoConfig,`,
    `  scenarios: demoScenarios,`,
    `  installers: {`,
    `    desktop: installPrototypeDesktopChrome,`,
    `    actions: installPrototypeActions,`,
    `    conversation: installPrototypeConversation,`,
    `    activity: installPrototypeActivityExperience,`,
    `    resilience: installPrototypeResilienceExperience,`,
    `    sidebar: installPrototypeSidebarExperience,`,
    `    signature: async (page) => { await installSignatureDark(page); await installSignatureSanitizer(page); },`,
    `    settle: settlePrototype,`,
    `  },`,
    `});`,
  ].join("\n").replaceAll("</script", "<\\/script");

  let html = `<!doctype html>
<html lang="zh-CN" data-theme="obsidian">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <meta name="color-scheme" content="dark">
  <meta name="theme-color" content="${variant === "signature" ? "#10110f" : "#151311"}">
  <meta name="description" content="${title}：R-Code 工作台的完整可点击 HTML 原型。">
  <title>${title}</title>
  <link rel="stylesheet" href="${assetPrefix}/styles.css">
  <style>${combinedCss.join("\n")}</style>
  <script>
    window.__workbenchDemoConfig = ${JSON.stringify(config)};
    (${applyScenarioBeforeApp.toString()})(window.__workbenchDemoConfig, ${JSON.stringify(demoScenarios)});
  </script>
</head>
<body>
  <div id="root"></div>
  <noscript>请启用 JavaScript 以体验 R-Code 完整交互 Demo。</noscript>
  <script src="${assetPrefix}/app.js"></script>
  <script>${runtimeSource}</script>
</body>
</html>
`;

  if (variant === "signature") {
    html = html
      .replaceAll("Codex CLI", "R-Code Agent")
      .replaceAll("Codex", "R-Code")
      .replaceAll("gpt-5.6-sol", "Auto")
      .replaceAll("gpt-5.6", "Auto");
  }

  fs.writeFileSync(outputPath, html, "utf8");
  process.stdout.write(`generated ${path.relative(prototypeRoot, outputPath).replaceAll("\\", "/")}\n`);
}

renderHtml({
  variant: "dark",
  outputPath: path.join(prototypeRoot, "dark", "demo.html"),
  assetPrefix: "../../../demo",
  title: "R-Code Dark · 完整交互 Demo",
  variantLabel: "Dark baseline",
});

renderHtml({
  variant: "signature",
  outputPath: path.join(prototypeRoot, "dark", "r-code-signature", "demo.html"),
  assetPrefix: "../../../../demo",
  title: "R-Code Signature Dark · 完整交互 Demo",
  variantLabel: "R-Code Signature",
});
