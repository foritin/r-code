const sidebarExperienceCss = String.raw`
  #app.scene-room .scene-room.prototype-context-mode {
    grid-template-areas: "convo" !important;
    grid-template-columns: minmax(0, 1fr) !important;
  }

  #app.scene-room .scene-room.prototype-subagent-mode {
    grid-template-areas: "convo splitter canvas" !important;
    grid-template-columns: minmax(0, 55%) 8px minmax(0, 1fr) !important;
  }

  .prototype-original-panel {
    display: none !important;
  }

  .prototype-context-mode > .prototype-sidebar-host,
  .prototype-context-mode > .prototype-sidebar-splitter,
  .prototype-subagent-mode > .prototype-context-panel,
  .prototype-context-panel[hidden] {
    display: none !important;
  }

  .prototype-context-mode .prototype-session {
    width: min(760px, calc(100% - 420px));
    margin-right: auto;
    margin-left: clamp(36px, 7.2vw, 112px);
  }

  .prototype-context-mode .composer {
    padding-right: 372px !important;
  }

  .prototype-context-panel {
    position: absolute;
    z-index: 18;
    top: 76px;
    right: 16px;
    width: 344px;
    max-height: calc(100% - 96px);
    overflow: auto;
    border: 1px solid var(--border);
    border-radius: 18px;
    background: var(--bg-card);
    box-shadow: 0 12px 34px color-mix(in srgb, #000 18%, transparent);
    color: var(--fg);
  }

  .prototype-context-section {
    padding: 14px 16px;
  }

  .prototype-context-section + .prototype-context-section {
    border-top: 1px solid var(--border);
  }

  .prototype-context-heading,
  .prototype-agent-section-heading {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin: 0 0 8px;
    color: var(--fg-muted);
    font-size: 12px;
    font-weight: 580;
    letter-spacing: .01em;
  }

  .prototype-context-count {
    color: var(--fg-faint);
    font-size: 11px;
    font-weight: 480;
  }

  .prototype-env-row {
    display: grid;
    grid-template-columns: 18px minmax(0, 1fr) auto;
    align-items: center;
    gap: 8px;
    min-height: 29px;
    color: var(--fg);
    font-size: 12px;
  }

  .prototype-env-row svg,
  .prototype-source-row svg {
    color: var(--fg-muted);
  }

  .prototype-env-value {
    color: var(--fg-muted);
  }

  .prototype-diff-positive {
    color: var(--success);
  }

  .prototype-diff-negative {
    margin-left: 4px;
    color: var(--danger);
  }

  .prototype-context-subagents-button {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    align-items: center;
    width: 100%;
    min-height: 40px;
    gap: 9px;
    padding: 6px 8px;
    border: 0;
    border-radius: 9px;
    background: var(--bg-hover);
    color: var(--fg);
    text-align: left;
  }

  .prototype-context-subagents-button:hover,
  .prototype-context-subagents-button:focus-visible {
    background: var(--bg-active);
  }

  .prototype-agent-stack {
    display: flex;
    align-items: center;
    min-width: 42px;
  }

  .prototype-agent-stack .prototype-agent-avatar + .prototype-agent-avatar {
    margin-left: -7px;
  }

  .prototype-context-subagent-copy {
    min-width: 0;
  }

  .prototype-context-subagent-copy strong,
  .prototype-context-subagent-copy small {
    display: block;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .prototype-context-subagent-copy strong {
    font-size: 12px;
    font-weight: 580;
  }

  .prototype-context-subagent-copy small {
    margin-top: 1px;
    color: var(--fg-muted);
    font-size: 11px;
  }

  .prototype-source-list {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }

  .prototype-source-row {
    display: grid;
    grid-template-columns: 24px minmax(0, 1fr);
    align-items: center;
    gap: 8px;
    min-height: 31px;
    padding: 3px 4px;
    border: 0;
    border-radius: 6px;
    background: transparent;
    color: var(--fg-muted);
    font-size: 11px;
    text-align: left;
  }

  .prototype-source-row:hover,
  .prototype-source-row:focus-visible {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .prototype-source-kind {
    display: grid;
    place-items: center;
    width: 22px;
    height: 22px;
    border: 1px solid var(--border);
    border-radius: 5px;
    background: var(--bg-inset);
    color: var(--fg-faint);
    font: 8px/1 var(--font-mono);
  }

  .prototype-source-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .prototype-sidebar-splitter {
    grid-area: splitter;
  }

  .prototype-sidebar-host {
    grid-area: canvas;
    min-width: 0;
    height: 100%;
    overflow: hidden;
    background: var(--bg-app) !important;
  }

  .prototype-agent-page {
    display: flex;
    flex-direction: column;
    min-height: 0;
    height: 100%;
  }

  .prototype-agent-page-header {
    display: flex;
    align-items: center;
    min-height: 48px;
    gap: 9px;
    padding: 0 14px;
    border-bottom: 1px solid var(--border);
    background: var(--bg-panel);
  }

  .prototype-agent-page-header strong {
    min-width: 0;
    overflow: hidden;
    font-size: 13px;
    font-weight: 620;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .prototype-agent-header-spacer {
    flex: 1;
  }

  .prototype-agent-icon-button {
    display: inline-grid;
    place-items: center;
    width: 28px;
    height: 28px;
    padding: 0;
    border: 0;
    border-radius: 6px;
    background: transparent;
    color: var(--fg-muted);
  }

  .prototype-agent-icon-button:hover,
  .prototype-agent-icon-button:focus-visible {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .prototype-agent-list-body,
  .prototype-agent-detail-body {
    min-height: 0;
    overflow: auto;
  }

  .prototype-agent-list-body {
    padding: 16px 14px 24px;
  }

  .prototype-agent-list-section + .prototype-agent-list-section {
    margin-top: 23px;
  }

  .prototype-agent-row {
    display: grid;
    grid-template-columns: 34px minmax(0, 1fr) auto;
    align-items: center;
    width: 100%;
    min-height: 64px;
    gap: 10px;
    padding: 9px 10px;
    border: 0;
    border-radius: 9px;
    background: transparent;
    color: var(--fg);
    text-align: left;
  }

  .prototype-agent-row + .prototype-agent-row {
    border-top: 1px solid var(--border);
    border-top-left-radius: 0;
    border-top-right-radius: 0;
  }

  .prototype-agent-row:hover,
  .prototype-agent-row:focus-visible,
  .prototype-agent-row.is-running {
    background: var(--bg-hover);
  }

  .prototype-agent-avatar {
    position: relative;
    display: inline-grid;
    place-items: center;
    width: 28px;
    height: 28px;
    flex: none;
    overflow: hidden;
    border: 1px solid color-mix(in srgb, var(--prototype-agent-color, var(--accent)) 72%, var(--border));
    border-radius: 50%;
    background:
      radial-gradient(circle at 42% 38%, color-mix(in srgb, var(--prototype-agent-color, var(--accent)) 72%, white) 0 22%, transparent 23%),
      conic-gradient(from 22deg, var(--prototype-agent-color, var(--accent)), transparent 44%, var(--prototype-agent-color, var(--accent)) 76%, transparent);
    box-shadow: inset 0 0 0 4px color-mix(in srgb, var(--bg-card) 88%, transparent);
  }

  .prototype-agent-avatar--mint { --prototype-agent-color: #58c7a4; }
  .prototype-agent-avatar--cyan { --prototype-agent-color: #45a8b7; }
  .prototype-agent-avatar--coral { --prototype-agent-color: #d86e68; }

  .prototype-agent-row-copy {
    min-width: 0;
  }

  .prototype-agent-row-title,
  .prototype-agent-row-description {
    display: block;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .prototype-agent-row-title {
    font-size: 13px;
    font-weight: 590;
  }

  .prototype-agent-row-description {
    margin-top: 3px;
    color: var(--fg-muted);
    font-size: 11px;
  }

  .prototype-agent-row-meta {
    display: flex;
    align-items: center;
    gap: 7px;
    margin-left: 8px;
    color: var(--fg-faint);
    font-size: 10px;
    white-space: nowrap;
  }

  .prototype-agent-spinner {
    display: inline-block;
    width: 14px;
    height: 14px;
    border: 2px solid color-mix(in srgb, var(--accent) 34%, transparent);
    border-top-color: var(--accent);
    border-radius: 50%;
  }

  @media (prefers-reduced-motion: no-preference) {
    .prototype-agent-spinner {
      animation: prototype-agent-spin .8s linear infinite;
    }
  }

  @keyframes prototype-agent-spin {
    to { transform: rotate(360deg); }
  }

  .prototype-agent-complete-mark {
    display: inline-grid;
    place-items: center;
    width: 16px;
    height: 16px;
    border: 1px solid color-mix(in srgb, var(--success) 58%, transparent);
    border-radius: 50%;
    color: var(--success);
  }

  .prototype-agent-status-chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    min-height: 24px;
    padding: 0 8px;
    border-radius: 999px;
    background: var(--bg-hover);
    color: var(--fg-muted);
    font-size: 10px;
  }

  .prototype-agent-detail-body {
    padding: 0 20px 30px;
  }

  .prototype-agent-session {
    padding: 18px 0 28px;
  }

  .prototype-agent-session-summary {
    display: flex;
    align-items: center;
    width: 100%;
    gap: 7px;
    padding: 0 0 11px;
    border: 0;
    border-bottom: 1px solid var(--border);
    background: transparent;
    color: var(--fg-muted);
    font-size: 11px;
    text-align: left;
  }

  .prototype-agent-session-summary svg {
    transition: transform var(--dur-2) var(--ease);
  }

  .prototype-agent-session-summary[aria-expanded="false"] svg {
    transform: rotate(-90deg);
  }

  .prototype-agent-session-body {
    padding-top: 18px;
  }

  .prototype-agent-copy {
    margin: 0 0 15px;
    color: var(--fg);
    font-size: 13px;
    line-height: 1.65;
    text-wrap: pretty;
  }

  .prototype-agent-command {
    display: grid;
    grid-template-columns: 20px minmax(0, 1fr);
    align-items: center;
    gap: 8px;
    margin: 16px 0;
    color: var(--prototype-command);
    font: 11px/1.5 var(--font-mono);
  }

  .prototype-agent-command-icon {
    display: grid;
    place-items: center;
    width: 17px;
    height: 17px;
    border: 1px solid color-mix(in srgb, var(--prototype-command) 64%, transparent);
    border-radius: 5px;
  }

  .prototype-agent-command code {
    overflow: hidden;
    color: inherit;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .prototype-agent-live-state {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 20px;
    padding-top: 13px;
    border-top: 1px solid var(--border);
    color: var(--fg-muted);
    font-size: 11px;
  }

  @media (max-width: 1359px) {
    .prototype-context-panel {
      width: 320px;
    }

    .prototype-context-mode .prototype-session {
      width: min(680px, calc(100% - 380px));
      margin-left: 28px;
    }

    .prototype-context-mode .composer {
      padding-right: 344px !important;
    }
  }
`;

async function installPrototypeSidebarExperience(page) {
  await page.evaluate(() => {
    const mode = new URL(window.location.href).searchParams.get("prototypePanel");
    if (!mode) return;

    const scene = document.querySelector("#main-content > .scene-room");
    if (!scene || scene.querySelector(".prototype-context-panel")) return;

    [...scene.children].forEach((child) => {
      if (
        child.classList.contains("workbench")
        || child.classList.contains("room-splitter")
        || child.classList.contains("workbench-review-rail")
        || child.classList.contains("workbench-backdrop")
      ) {
        child.classList.add("prototype-original-panel");
      }
    });

    const chevronRight = `
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="m9 6 6 6-6 6"></path>
      </svg>`;
    const chevronDown = `
      <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="m6 9 6 6 6-6"></path>
      </svg>`;
    const backIcon = `
      <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="m15 18-6-6 6-6"></path>
      </svg>`;
    const panelIcon = `
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <rect x="4" y="4" width="16" height="16" rx="2"></rect><path d="M14 4v16"></path>
      </svg>`;
    const branchIcon = `
      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <circle cx="6" cy="5" r="2"></circle><circle cx="18" cy="7" r="2"></circle>
        <circle cx="6" cy="19" r="2"></circle><path d="M6 7v10M8 12h4a6 6 0 0 0 6-3"></path>
      </svg>`;
    const folderIcon = `
      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="M3.5 7.5h6l2-2h9v13h-17z"></path>
      </svg>`;
    const changeIcon = `
      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <rect x="4" y="4" width="16" height="16" rx="2"></rect><path d="M8 9h8M8 13h5"></path>
      </svg>`;
    const terminalIcon = `
      <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="m7 8 3 4-3 4"></path><path d="M13 16h4"></path>
      </svg>`;
    const checkIcon = `
      <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="m7 12 3 3 7-7"></path>
      </svg>`;

    const avatar = (tone) => `<span class="prototype-agent-avatar prototype-agent-avatar--${tone}" aria-hidden="true"></span>`;
    const contextPanel = document.createElement("aside");
    contextPanel.className = "prototype-context-panel";
    contextPanel.setAttribute("aria-label", "当前会话上下文");
    contextPanel.innerHTML = `
      <section class="prototype-context-section" aria-labelledby="prototype-env-heading">
        <h3 class="prototype-context-heading" id="prototype-env-heading">
          <span>环境信息</span><span class="prototype-context-count">本地</span>
        </h3>
        <div class="prototype-env-row">${changeIcon}<span>变更</span><span><b class="prototype-diff-positive">+382</b><b class="prototype-diff-negative">-11</b></span></div>
        <div class="prototype-env-row">${folderIcon}<span>本地</span><span class="prototype-env-value">PowerShell</span></div>
        <div class="prototype-env-row">${branchIcon}<span>分支</span><span class="prototype-env-value">main</span></div>
      </section>
      <section class="prototype-context-section" aria-labelledby="prototype-context-agents-heading">
        <h3 class="prototype-context-heading" id="prototype-context-agents-heading">
          <span>子智能体</span><span class="prototype-context-count">3</span>
        </h3>
        <button type="button" class="prototype-context-subagents-button" aria-label="打开子智能体列表">
          <span class="prototype-agent-stack">${avatar("mint")}${avatar("cyan")}${avatar("coral")}</span>
          <span class="prototype-context-subagent-copy"><strong>1 运行中 · 2 已完成</strong><small>查看各自的运行过程</small></span>
          ${chevronRight}
        </button>
      </section>
      <section class="prototype-context-section" aria-labelledby="prototype-sources-heading">
        <h3 class="prototype-context-heading" id="prototype-sources-heading">
          <span>来源</span><span class="prototype-context-count">上下文文件</span>
        </h3>
        <div class="prototype-source-list">
          <button type="button" class="prototype-source-row" data-source="render-workbench.cjs"><span class="prototype-source-kind">JS</span><span class="prototype-source-name">render-workbench.cjs</span></button>
          <button type="button" class="prototype-source-row" data-source="02-launcher-dark.png"><span class="prototype-source-kind">PNG</span><span class="prototype-source-name">02-launcher-dark.png</span></button>
          <button type="button" class="prototype-source-row" data-source="README.md"><span class="prototype-source-kind">MD</span><span class="prototype-source-name">README.md</span></button>
        </div>
      </section>`;
    scene.append(contextPanel);

    const agents = {
      interaction: {
        title: "交互规格",
        tone: "coral",
        state: "running",
        elapsed: "1m",
        description: "正在整理右栏状态与返回路径",
        duration: "已处理 1m 12s",
        intro: "我会先核对收起态、列表态与详情态之间的入口，确保右栏切换时不会丢失当前会话上下文。",
        commandOne: "Inspect room workbench layout and panel states",
        detail: "收起态保留环境、子智能体和来源；进入列表后，未完成任务使用持续旋转的状态指示器。",
        commandTwo: "Validate context → list → detail → back",
        result: "正在检查返回路径与不同宽度下的内容层级。",
      },
      pipeline: {
        title: "原型渲染",
        tone: "mint",
        state: "complete",
        elapsed: "4m",
        description: "已生成明暗主题并通过点击验证",
        duration: "已处理 4m 08s",
        intro: "我核对了渲染入口与输出目录，确认所有状态都能按主题成对生成。",
        commandOne: "node render-workbench.cjs",
        detail: "图片尺寸和编号均已检查，未产生额外的临时文件。",
        commandTwo: "Verify output matrix and image dimensions",
        result: "原型渲染已完成。",
      },
      palette: {
        title: "配色审计",
        tone: "cyan",
        state: "complete",
        elapsed: "6m",
        description: "已核对主区、侧栏和输入层级",
        duration: "已处理 6m 21s",
        intro: "我检查了深黑主画布、浅黑侧栏与输入面的对比关系。",
        commandOne: "Audit computed surface colors",
        detail: "面板边界维持单线，内容对比和暖色侧栏光晕均符合当前原型基线。",
        commandTwo: "Compare light and dark surface hierarchy",
        result: "配色审计已完成。",
      },
    };

    let splitter;
    let host;

    const ensureDocked = () => {
      if (!splitter) {
        splitter = document.createElement("div");
        splitter.className = "room-splitter prototype-sidebar-splitter";
        splitter.setAttribute("role", "separator");
        splitter.setAttribute("aria-orientation", "vertical");
        splitter.setAttribute("aria-label", "调整子智能体侧栏宽度");
        splitter.tabIndex = 0;
        splitter.innerHTML = "<span aria-hidden=\"true\"></span>";
        scene.append(splitter);
      }
      if (!host) {
        host = document.createElement("aside");
        host.className = "canvas workbench pane pane-lit prototype-sidebar-host";
        host.setAttribute("aria-label", "子智能体侧栏");
        scene.append(host);
      }
      scene.classList.remove("workbench-collapsed", "prototype-context-mode");
      scene.classList.add("workbench-docked", "prototype-subagent-mode");
      scene.dataset.workbenchMode = "docked";
      contextPanel.hidden = true;
      return host;
    };

    const completeMark = `<span class="prototype-agent-complete-mark" aria-label="已完成">${checkIcon}</span>`;
    const runningMark = `<span class="prototype-agent-spinner" aria-hidden="true"></span><span class="sr-only">正在运行</span>`;
    const agentRow = (id) => {
      const agent = agents[id];
      const running = agent.state === "running";
      return `
        <button type="button" class="prototype-agent-row${running ? " is-running" : ""}" data-agent-id="${id}">
          ${avatar(agent.tone)}
          <span class="prototype-agent-row-copy">
            <span class="prototype-agent-row-title">${agent.title}</span>
            <span class="prototype-agent-row-description">${agent.description}</span>
          </span>
          <span class="prototype-agent-row-meta">${running ? runningMark : completeMark}<span>${agent.elapsed}</span></span>
        </button>`;
    };

    const renderContext = () => {
      scene.classList.remove("workbench-docked", "prototype-subagent-mode");
      scene.classList.add("workbench-collapsed", "prototype-context-mode");
      scene.dataset.workbenchMode = "collapsed";
      scene.dataset.prototypeSidebarState = "context";
      contextPanel.hidden = false;
    };

    const renderList = () => {
      const sidebar = ensureDocked();
      sidebar.innerHTML = `
        <div class="prototype-agent-page" data-prototype-agent-view="list">
          <header class="prototype-agent-page-header">
            ${avatar("mint")}<strong>子智能体</strong><span class="prototype-agent-header-spacer"></span>
            <button type="button" class="prototype-agent-icon-button prototype-collapse-agents" aria-label="收起右侧边栏" title="收起右侧边栏">${panelIcon}</button>
          </header>
          <div class="prototype-agent-list-body">
            <section class="prototype-agent-list-section" aria-labelledby="prototype-running-heading">
              <h3 class="prototype-agent-section-heading" id="prototype-running-heading"><span>进行中</span><span>1</span></h3>
              ${agentRow("interaction")}
            </section>
            <section class="prototype-agent-list-section" aria-labelledby="prototype-complete-heading">
              <h3 class="prototype-agent-section-heading" id="prototype-complete-heading"><span>已完成</span><span>2</span></h3>
              ${agentRow("pipeline")}${agentRow("palette")}
            </section>
          </div>
        </div>`;
      scene.dataset.prototypeSidebarState = "list";
      sidebar.querySelector(".prototype-collapse-agents")?.addEventListener("click", renderContext);
      sidebar.querySelectorAll("[data-agent-id]").forEach((row) => {
        row.addEventListener("click", () => renderDetail(row.dataset.agentId));
      });
    };

    const renderDetail = (agentId = "interaction") => {
      const agent = agents[agentId] || agents.interaction;
      const sidebar = ensureDocked();
      const isRunning = agent.state === "running";
      sidebar.innerHTML = `
        <div class="prototype-agent-page" data-prototype-agent-view="detail" data-agent-id="${agentId}">
          <header class="prototype-agent-page-header">
            <button type="button" class="prototype-agent-icon-button prototype-agent-back" aria-label="返回子智能体列表" title="返回子智能体列表">${backIcon}</button>
            ${avatar(agent.tone)}<strong>${agent.title}</strong><span class="prototype-agent-header-spacer"></span>
            <span class="prototype-agent-status-chip">${isRunning ? runningMark : completeMark}<span>${isRunning ? "进行中" : "已完成"}</span></span>
          </header>
          <div class="prototype-agent-detail-body">
            <article class="prototype-agent-session">
              <button type="button" class="prototype-agent-session-summary" aria-expanded="true" aria-controls="prototype-agent-session-body">
                <span>${agent.duration}</span>${chevronDown}
              </button>
              <div class="prototype-agent-session-body" id="prototype-agent-session-body">
                <p class="prototype-agent-copy">${agent.intro}</p>
                <div class="prototype-agent-command" aria-label="已执行操作"><span class="prototype-agent-command-icon">${terminalIcon}</span><code>${agent.commandOne}</code></div>
                <p class="prototype-agent-copy">${agent.detail}</p>
                <div class="prototype-agent-command" aria-label="已执行操作"><span class="prototype-agent-command-icon">${terminalIcon}</span><code>${agent.commandTwo}</code></div>
                <p class="prototype-agent-copy">${agent.result}</p>
                <div class="prototype-agent-live-state" role="status" aria-live="polite">
                  ${isRunning ? runningMark : completeMark}<span>${isRunning ? "正在继续运行" : "运行已完成"}</span>
                </div>
              </div>
            </article>
          </div>
        </div>`;
      scene.dataset.prototypeSidebarState = "detail";
      sidebar.querySelector(".prototype-agent-back")?.addEventListener("click", renderList);
      const summary = sidebar.querySelector(".prototype-agent-session-summary");
      const body = sidebar.querySelector(".prototype-agent-session-body");
      summary?.addEventListener("click", () => {
        const expanded = summary.getAttribute("aria-expanded") === "true";
        summary.setAttribute("aria-expanded", String(!expanded));
        if (body) body.hidden = expanded;
      });
    };

    contextPanel.querySelector(".prototype-context-subagents-button")?.addEventListener("click", renderList);
    contextPanel.querySelectorAll("[data-source]").forEach((source) => {
      source.addEventListener("click", () => {
        document.documentElement.dataset.prototypeOpenedSource = source.dataset.source || "";
      });
    });

    if (mode === "context") renderContext();
    else if (mode === "subagent-detail") renderDetail("interaction");
    else renderList();
    document.documentElement.dataset.prototypeSidebarExperience = "installed";
  });
}

module.exports = { sidebarExperienceCss, installPrototypeSidebarExperience };
