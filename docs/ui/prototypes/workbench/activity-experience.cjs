const activityExperienceCss = String.raw`
  :root[data-theme="studio-light"] {
    --prototype-activity-muted: #766e66;
    --prototype-activity-active: #211e1b;
    --prototype-activity-panel: #fffdf9;
    --prototype-activity-panel-border: #d8cec4;
    --prototype-activity-add-bg: #e7f4e9;
    --prototype-activity-add-edge: #2e9a58;
    --prototype-activity-code-gutter: #f3eee8;
  }

  :root[data-theme="obsidian"] {
    --prototype-activity-muted: #8f8881;
    --prototype-activity-active: #f3eee8;
    --prototype-activity-panel: #242321;
    --prototype-activity-panel-border: #45413d;
    --prototype-activity-add-bg: #1f3124;
    --prototype-activity-add-edge: #40c977;
    --prototype-activity-code-gutter: #1a1917;
  }

  .prototype-activity-session .prototype-session-summary:disabled {
    cursor: default;
    opacity: 1;
  }

  .prototype-activity-session .prototype-session-summary:disabled:hover {
    color: var(--fg-muted);
  }

  .prototype-activity-trace {
    display: flow-root;
  }

  .prototype-activity-event {
    margin: 18px 0 21px;
    color: var(--prototype-activity-muted);
    font-size: 13px;
    line-height: 1.55;
  }

  .prototype-activity-static,
  .prototype-activity-live-command,
  .prototype-activity-toggle,
  .prototype-activity-child-command {
    display: grid;
    grid-template-columns: 20px minmax(0, 1fr) auto;
    align-items: center;
    width: 100%;
    min-width: 0;
    gap: 9px;
    color: var(--prototype-activity-muted);
    text-align: left;
  }

  .prototype-activity-static,
  .prototype-activity-live-command {
    grid-template-columns: 20px minmax(0, 1fr);
  }

  .prototype-activity-toggle,
  .prototype-activity-child-command {
    padding: 1px 0;
    border: 0;
    border-radius: 0;
    background: transparent;
    font: inherit;
  }

  .prototype-activity-toggle:hover,
  .prototype-activity-toggle:focus-visible,
  .prototype-activity-child-command:hover,
  .prototype-activity-child-command:focus-visible {
    color: var(--fg);
  }

  .prototype-activity-icon {
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    color: currentColor;
  }

  .prototype-activity-title,
  .prototype-activity-child-command code,
  .prototype-running-command-text {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .prototype-activity-title {
    font-weight: 470;
  }

  .prototype-activity-chevron {
    transition: transform var(--dur-2) var(--ease);
  }

  .prototype-activity-toggle[aria-expanded="false"] .prototype-activity-chevron,
  .prototype-activity-child-command[aria-expanded="false"] .prototype-activity-chevron {
    transform: rotate(-90deg);
  }

  .prototype-activity-details {
    margin: 10px 0 0 29px;
  }

  .prototype-activity-details[hidden] {
    display: none !important;
  }

  .prototype-activity-command-list {
    display: grid;
    gap: 9px;
  }

  .prototype-activity-child-command {
    grid-template-columns: 20px minmax(0, 1fr) auto;
    color: var(--prototype-activity-muted);
    font-family: var(--font-ui);
  }

  .prototype-activity-child-command code {
    color: inherit;
    font: inherit;
  }

  .prototype-shell-card,
  .prototype-diff-card {
    overflow: hidden;
    border: 1px solid var(--prototype-activity-panel-border);
    border-radius: 14px;
    background: var(--prototype-activity-panel);
    color: var(--fg);
  }

  .prototype-shell-label,
  .prototype-diff-head {
    display: flex;
    align-items: center;
    min-height: 38px;
    padding: 0 14px;
    border-bottom: 1px solid var(--border);
    color: var(--fg-muted);
    font-size: 12px;
  }

  .prototype-shell-card pre {
    max-height: 210px;
    overflow: auto;
    margin: 0;
    padding: 14px;
    color: var(--fg-muted);
    font: 12px/1.7 var(--font-mono);
    white-space: pre-wrap;
  }

  .prototype-shell-card pre strong {
    color: var(--fg);
    font-weight: 520;
  }

  .prototype-context-event {
    margin-block: 19px 23px;
  }

  .prototype-agent-event {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 7px;
    margin: 20px 0 23px;
  }

  .prototype-agent-chip {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    min-height: 30px;
    padding: 0 10px;
    border: 1px solid var(--border);
    border-radius: 999px;
    background: transparent;
    color: var(--prototype-activity-muted);
    font-size: 12px;
  }

  .prototype-agent-chip:hover,
  .prototype-agent-chip:focus-visible,
  .prototype-agent-chip[aria-pressed="true"] {
    background: var(--bg-chip);
    color: var(--fg);
  }

  .prototype-agent-avatar-dot {
    width: 16px;
    height: 16px;
    border: 2px solid color-mix(in srgb, var(--prototype-agent-color) 78%, var(--fg));
    border-radius: 50%;
    background:
      radial-gradient(circle at 40% 36%, color-mix(in srgb, var(--prototype-agent-color) 38%, white) 0 22%, transparent 24%),
      var(--prototype-agent-color);
    box-shadow: inset 0 0 0 3px color-mix(in srgb, var(--prototype-agent-color) 35%, transparent);
  }

  .prototype-agent-event-state {
    margin-left: 2px;
    color: var(--fg-muted);
    font-size: 12px;
  }

  .prototype-running-command-text {
    color: color-mix(in srgb, var(--prototype-activity-muted) 72%, var(--prototype-activity-active));
    font-family: var(--font-mono);
  }

  .prototype-diff-head {
    justify-content: space-between;
  }

  .prototype-diff-title {
    display: flex;
    align-items: center;
    min-width: 0;
    gap: 8px;
  }

  .prototype-diff-title strong {
    overflow: hidden;
    color: var(--fg);
    font-weight: 540;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .prototype-diff-add { color: var(--success); }
  .prototype-diff-del { color: var(--danger); }

  .prototype-copy-patch {
    display: grid;
    place-items: center;
    width: 26px;
    height: 26px;
    border-radius: 6px;
    color: var(--fg-muted);
  }

  .prototype-copy-patch:hover,
  .prototype-copy-patch:focus-visible {
    background: var(--bg-hover);
    color: var(--fg);
  }

  .prototype-diff-code {
    display: grid;
    grid-template-columns: 54px minmax(0, 1fr);
    font: 12px/1.72 var(--font-mono);
  }

  .prototype-diff-line {
    display: contents;
  }

  .prototype-diff-number,
  .prototype-diff-source {
    padding-block: 2px;
  }

  .prototype-diff-number {
    padding-right: 12px;
    background: var(--prototype-activity-code-gutter);
    color: var(--fg-faint);
    text-align: right;
    user-select: none;
  }

  .prototype-diff-source {
    overflow: hidden;
    padding-inline: 14px;
    color: var(--fg-muted);
    text-overflow: ellipsis;
    white-space: pre;
  }

  .prototype-diff-line.is-add .prototype-diff-number,
  .prototype-diff-line.is-add .prototype-diff-source {
    background: var(--prototype-activity-add-bg);
  }

  .prototype-diff-line.is-add .prototype-diff-number {
    box-shadow: inset 4px 0 0 var(--prototype-activity-add-edge);
    color: var(--prototype-activity-add-edge);
  }

  .prototype-token-keyword { color: #d071f9; }
  .prototype-token-string { color: #79c87f; }
  .prototype-token-name { color: #ef922f; }

  .prototype-final-response {
    margin-top: 24px;
    padding-top: 20px;
    border-top: 1px solid var(--border);
  }

  .prototype-activity-session .prototype-file-links {
    margin-top: 4px;
  }

`;

async function installPrototypeActivityExperience(page) {
  await page.evaluate(() => {
    const session = document.querySelector(".prototype-session");
    if (!session || session.dataset.prototypeActivityInstalled === "true") return;

    const params = new URLSearchParams(window.location.search);
    const requestedMode = params.get("prototypeActivity");
    const baseRunning = session.dataset.prototypeState === "running";
    const mode = requestedMode || (baseRunning ? "running" : "expanded");
    const running = mode === "running";
    const collapsed = mode === "collapsed";
    const complete = !running;
    const archived = session.classList.contains("is-readonly") || Boolean(document.querySelector(".room-archived-note"));
    const prompt = session.querySelector(".prototype-user-message")?.textContent?.trim()
      || "完善回答与运行交替出现的会话事件流。";
    const reviewOpen = document.querySelector('[data-testid="workbench-panel"]')?.dataset.workbenchKind === "review";

    const terminalIcon = `
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <rect x="3.5" y="4" width="17" height="16" rx="3"></rect><path d="m7.5 9 3 3-3 3"></path><path d="M13.5 15h3"></path>
      </svg>`;
    const contextIcon = `
      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="M7 4h10v12H7z"></path><path d="M4 7v12h10"></path><path d="m16 18 2 2 2-2"></path>
      </svg>`;
    const editIcon = `
      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="m4 20 4.5-1 10-10a2.1 2.1 0 0 0-3-3l-10 10z"></path><path d="m14 7 3 3"></path>
      </svg>`;
    const chevronIcon = `
      <svg class="prototype-activity-chevron" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="m6 9 6 6 6-6"></path>
      </svg>`;
    const fileIcon = `
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="M6 3.5h8l4 4v13H6z"></path><path d="M14 3.5v4h4"></path>
      </svg>`;
    const copyIcon = `
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <rect x="8" y="8" width="11" height="11" rx="2"></rect><path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2"></path>
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

    const singleOpen = mode === "single";
    const multiOpen = mode === "multi";
    const fileOpen = mode === "file";
    const traceExpanded = !collapsed;
    const singleTitle = singleOpen ? "Ran command" : "Ran rg --files docs/ui/prototypes/workbench in 2s";

    const agentChip = (id, label, color, selected = false) => `
      <button type="button" class="prototype-agent-chip" data-prototype-inline-agent="${id}"
        aria-pressed="${selected}" style="--prototype-agent-color:${color}">
        <span class="prototype-agent-avatar-dot" aria-hidden="true"></span><span>${label}</span>
      </button>`;

    const shellCard = `
      <div class="prototype-shell-card" data-prototype-shell-detail>
        <div class="prototype-shell-label">Shell</div>
        <pre><strong>$ node docs/ui/prototypes/workbench/render-workbench.cjs</strong>
rendered light/23-event-running-light.png
rendered dark/24-event-running-dark.png
rendered light/25-event-complete-collapsed-light.png
rendered dark/26-event-complete-collapsed-dark.png
rendered light/27-event-complete-expanded-light.png
rendered dark/28-event-complete-expanded-dark.png
rendered light/29-event-multi-command-expanded-light.png
rendered dark/30-event-multi-command-expanded-dark.png
rendered light/31-event-shell-expanded-light.png
rendered dark/32-event-shell-expanded-dark.png
rendered light/33-event-single-file-diff-expanded-light.png
rendered dark/34-event-single-file-diff-expanded-dark.png
rendered dark/36-approval-required-dark.png
rendered dark/38-command-failed-dark.png
rendered dark/40-new-task-dark.png
rendered dark/42-context-picker-dark.png
TOTAL=38
LIGHT=17
DARK=21
TEMP_ARTIFACTS=0</pre>
      </div>`;

    const diffCard = `
      <div class="prototype-diff-card" data-prototype-diff-detail>
        <header class="prototype-diff-head">
          <span class="prototype-diff-title"><strong>activity-experience.cjs</strong><span class="prototype-diff-add">+7</span><span class="prototype-diff-del">-0</span></span>
          <button type="button" class="prototype-copy-patch" aria-label="复制补丁">${copyIcon}</button>
        </header>
        <div class="prototype-diff-code" role="code" aria-label="文件差异">
          <span class="prototype-diff-line"><span class="prototype-diff-number">898</span><span class="prototype-diff-source">  const current = document.querySelector(".prototype-session");</span></span>
          <span class="prototype-diff-line is-add"><span class="prototype-diff-number">899</span><span class="prototype-diff-source">+ current.dataset.<span class="prototype-token-name">activityState</span> = <span class="prototype-token-string">"running"</span>;</span></span>
          <span class="prototype-diff-line is-add"><span class="prototype-diff-number">900</span><span class="prototype-diff-source">+ <span class="prototype-token-keyword">if</span> (current.matches(<span class="prototype-token-string">"[aria-busy=true]"</span>)) {</span></span>
          <span class="prototype-diff-line is-add"><span class="prototype-diff-number">901</span><span class="prototype-diff-source">+   current.classList.add(<span class="prototype-token-string">"is-streaming"</span>);</span></span>
          <span class="prototype-diff-line is-add"><span class="prototype-diff-number">902</span><span class="prototype-diff-source">+ }</span></span>
        </div>
      </div>`;

    const traceHtml = `
      <div class="prototype-activity-trace">
        <p class="prototype-assistant-copy">我会先核对现有会话结构，再按真实时间顺序整理回答、命令和系统事件。</p>

        <div class="prototype-activity-event" data-prototype-event="single-command">
          <button type="button" class="prototype-activity-toggle" aria-expanded="${singleOpen}" aria-controls="prototype-single-command-detail">
            <span class="prototype-activity-icon">${terminalIcon}</span><span class="prototype-activity-title">${singleTitle}</span>${chevronIcon}
          </button>
          <div class="prototype-activity-details" id="prototype-single-command-detail" ${singleOpen ? "" : "hidden"}>${shellCard}</div>
        </div>

        <p class="prototype-assistant-copy">现有渲染器可以继续复用，但运行事件不应该集中到独立卡片，而要留在它发生的位置。</p>

        <div class="prototype-activity-event prototype-context-event" data-prototype-event="context-compressed">
          <div class="prototype-activity-static"><span class="prototype-activity-icon">${contextIcon}</span><span class="prototype-activity-title">上下文已自动压缩</span></div>
        </div>

        <p class="prototype-assistant-copy">上下文压缩后，回答自然继续；子智能体则以紧凑状态行出现，并可跳转到右侧详情。</p>

        <div class="prototype-agent-event" data-prototype-event="subagents">
          ${agentChip("pipeline", "Prototype pipeline", "#65d6ad")}
          ${agentChip("interaction", "Interaction spec", "#ee6570", true)}
          ${agentChip("palette", "Palette audit", "#2ca9bd")}
          <span class="prototype-agent-event-state">${running ? "2 已完成 · 1 进行中" : "已完成"}</span>
        </div>

        <p class="prototype-assistant-copy">并行检查已经明确事件文案、层级和动效约束，接下来统一验证生成链路。</p>

        <div class="prototype-activity-event" data-prototype-event="multi-command">
          <button type="button" class="prototype-activity-toggle" aria-expanded="${multiOpen}" aria-controls="prototype-multi-command-detail">
            <span class="prototype-activity-icon">${terminalIcon}</span><span class="prototype-activity-title">运行了多个命令</span>${chevronIcon}
          </button>
          <div class="prototype-activity-details prototype-activity-command-list" id="prototype-multi-command-detail" ${multiOpen ? "" : "hidden"}>
            <div data-prototype-child-command>
              <button type="button" class="prototype-activity-child-command" aria-expanded="false">
                <span class="prototype-activity-icon">${terminalIcon}</span><code>Ran node --check docs/ui/prototypes/workbench/activity-experience.cjs</code>${chevronIcon}
              </button>
              <div class="prototype-activity-details" hidden>${shellCard}</div>
            </div>
            <div data-prototype-child-command>
              <button type="button" class="prototype-activity-child-command" aria-expanded="false">
                <span class="prototype-activity-icon">${terminalIcon}</span><code>Ran node docs/ui/prototypes/workbench/render-workbench.cjs</code>${chevronIcon}
              </button>
              <div class="prototype-activity-details" hidden>${shellCard}</div>
            </div>
          </div>
        </div>

        <p class="prototype-assistant-copy">命令结果通过后，我只保留必要的文件变更，并把单文件差异放到按需展开的详情中。</p>

        <div class="prototype-activity-event" data-prototype-event="file-edit">
          <button type="button" class="prototype-activity-toggle" aria-expanded="${fileOpen}" aria-controls="prototype-file-edit-detail">
            <span class="prototype-activity-icon">${editIcon}</span><span class="prototype-activity-title">已编辑的文件</span>${chevronIcon}
          </button>
          <div class="prototype-activity-details" id="prototype-file-edit-detail" ${fileOpen ? "" : "hidden"}>${diffCard}</div>
        </div>

        ${running ? `
          <p class="prototype-assistant-copy">现在正在重渲染明暗两套原型；进行中的命令只保留当前一行。</p>
          <div class="prototype-activity-event" data-prototype-event="running-command" aria-busy="true">
            <div class="prototype-activity-live-command"><span class="prototype-activity-icon">${terminalIcon}</span><span class="prototype-running-command-text">Ran node docs/ui/prototypes/workbench/render-workbench.cjs</span></div>
          </div>` : ""}
      </div>`;

    const fileHref = (() => {
      const url = new URL(window.location.href);
      url.searchParams.set("scene", "editor");
      url.searchParams.set("project", "r-code");
      url.searchParams.set("file", "docs/ui/prototypes/workbench/activity-experience.cjs");
      url.searchParams.delete("state");
      return url.href;
    })();

    const finalHtml = complete ? `
      <div class="prototype-final-response" data-prototype-final-response>
        <p class="prototype-assistant-copy prototype-result-lead">回答与运行交替出现的事件流已经完成。</p>
        <p class="prototype-assistant-copy">默认只展示安静的一行摘要；展开后可以查看多命令、Shell 输出和单文件差异。</p>
        <div class="prototype-file-links" aria-label="输出文件">
          <a class="prototype-file-link" data-prototype-file="activity-experience.cjs" href="${fileHref}">${fileIcon}<span>activity-experience.cjs</span></a>
        </div>
      </div>` : "";

    const completionHtml = complete && !archived ? `
      <div class="prototype-completion" aria-label="完成后的操作">
        <span class="prototype-completion-state">${checkIcon}<span>已完成</span></span>
        <button type="button" class="prototype-completion-action prototype-review-action" aria-pressed="${reviewOpen}" title="在右侧工作台打开审核">
          ${reviewIcon}<span>审核变更</span>
        </button>
        <button type="button" class="prototype-completion-action prototype-undo-action" title="撤销本次变更">
          ${undoIcon}<span>撤销</span>
        </button>
      </div>` : "";

    session.classList.add("prototype-activity-session");
    session.dataset.prototypeActivityInstalled = "true";
    session.dataset.prototypeActivityMode = mode;
    session.dataset.prototypeState = complete ? "complete" : "running";
    session.innerHTML = `
      <div class="prototype-user-row"><div class="prototype-user-message">${prompt}</div></div>
      <button type="button" class="prototype-session-summary" aria-expanded="${traceExpanded}"
        aria-controls="prototype-session-body" ${running ? "disabled" : ""}>
        <span>${running ? "正在处理 1m 42s" : "已处理 4m 18s"}</span>${complete ? chevronIcon : ""}
      </button>
      <div class="prototype-session-body" id="prototype-session-body" ${traceExpanded ? "" : "hidden"}>${traceHtml}</div>
      ${finalHtml}
      ${completionHtml}`;

    const bindToggle = (button, details) => {
      button?.addEventListener("click", () => {
        const expanded = button.getAttribute("aria-expanded") === "true";
        button.setAttribute("aria-expanded", String(!expanded));
        if (details) details.hidden = expanded;
      });
    };

    const summary = session.querySelector(".prototype-session-summary");
    const body = session.querySelector(".prototype-session-body");
    if (complete) {
      summary?.addEventListener("click", () => {
        const expanded = summary.getAttribute("aria-expanded") === "true";
        summary.setAttribute("aria-expanded", String(!expanded));
        if (body) body.hidden = expanded;
        document.documentElement.dataset.prototypeSummaryState = expanded ? "collapsed" : "expanded";
      });
    }

    session.querySelectorAll(".prototype-activity-event > .prototype-activity-toggle").forEach((button) => {
      bindToggle(button, button.nextElementSibling);
    });
    session.querySelectorAll("[data-prototype-child-command]").forEach((row) => {
      const button = row.querySelector(".prototype-activity-child-command");
      bindToggle(button, button?.nextElementSibling);
    });

    session.querySelectorAll(".prototype-agent-chip").forEach((chip) => {
      chip.addEventListener("click", () => {
        session.querySelectorAll(".prototype-agent-chip").forEach((candidate) => candidate.setAttribute("aria-pressed", "false"));
        chip.setAttribute("aria-pressed", "true");
        document.documentElement.dataset.prototypeInlineAgent = chip.dataset.prototypeInlineAgent || "";
        document.querySelector(".prototype-context-subagents-button")?.click();
      });
    });

    session.querySelector(".prototype-copy-patch")?.addEventListener("click", (event) => {
      event.stopPropagation();
      document.documentElement.dataset.prototypePatchCopied = "true";
    });

    const reviewButton = session.querySelector(".prototype-review-action");
    const markReviewOpen = () => {
      reviewButton?.setAttribute("aria-pressed", "true");
      document.documentElement.dataset.prototypeReviewState = "open";
      const status = document.querySelector(".room-conversation-title span");
      if (status) status.textContent = "正在审核";
    };
    reviewButton?.addEventListener("click", () => {
      if (document.querySelector('[data-testid="workbench-panel"][data-workbench-kind="review"]')) {
        markReviewOpen();
        return;
      }
      const reviewLauncher = [...document.querySelectorAll(".workbench-launcher-row")]
        .find((row) => row.textContent?.includes("审核"));
      if (reviewLauncher) reviewLauncher.click();
      else document.dispatchEvent(new KeyboardEvent("keydown", { key: "4", code: "Digit4", altKey: true, bubbles: true }));
      window.setTimeout(markReviewOpen, 0);
    });

    const undoButton = session.querySelector(".prototype-undo-action");
    undoButton?.addEventListener("click", () => {
      session.classList.add("is-undone");
      session.dataset.prototypeState = "undone";
      const state = session.querySelector(".prototype-completion-state span");
      if (state) state.textContent = "已撤销";
      const label = undoButton.querySelector("span");
      if (label) label.textContent = "已撤销";
      undoButton.disabled = true;
      if (reviewButton) reviewButton.disabled = true;
      document.documentElement.dataset.prototypeUndoState = "complete";
    });

    const status = document.querySelector(".room-conversation-title span");
    if (status) status.textContent = archived ? "已归档，只读" : (running ? "正在执行" : (reviewOpen ? "正在审核" : "已完成"));
    document.documentElement.dataset.prototypeActivityExperience = mode;
  });
}

module.exports = { activityExperienceCss, installPrototypeActivityExperience };
