const resilienceExperienceCss = `
  .prototype-resilience-session,
  .prototype-empty-task {
    width: min(820px, calc(100% - 48px));
    margin: 0 auto;
    padding: 28px 0 120px;
    color: var(--fg);
  }

  .prototype-resilience-session .prototype-user-row {
    margin-bottom: 38px;
  }

  .prototype-resilience-status {
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 34px;
    margin-bottom: 26px;
    padding-bottom: 12px;
    border-bottom: 1px solid var(--border);
    color: var(--fg-muted);
    font-size: 13px;
    font-variant-numeric: tabular-nums;
  }

  .prototype-resilience-status svg {
    flex: 0 0 auto;
  }

  .prototype-resilience-copy {
    margin: 0 0 20px;
    color: var(--fg);
    font-size: 15px;
    font-weight: 520;
    line-height: 1.78;
    text-wrap: pretty;
  }

  .prototype-resilience-command {
    display: flex;
    align-items: center;
    gap: 10px;
    min-height: 34px;
    margin: 6px 0 22px;
    color: var(--prototype-command);
    font-size: 13px;
  }

  .prototype-resilience-command svg {
    flex: 0 0 auto;
  }

  .prototype-resilience-command code {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .prototype-resilience-command.is-running {
    color: color-mix(in srgb, var(--prototype-command) 84%, var(--fg));
  }

  :root[data-prototype-html-demo="true"] {
    --prototype-demo-shimmer: enabled;
  }

  :root[data-prototype-html-demo="true"] .prototype-resilience-command.is-running code,
  :root[data-prototype-html-demo="true"] .prototype-activity-event[data-prototype-event="running-command"] .prototype-running-command-text {
    color: transparent;
    background-image: linear-gradient(
      96deg,
      var(--prototype-command) 0%,
      var(--prototype-command) 34%,
      color-mix(in srgb, var(--fg) 94%, transparent) 49%,
      var(--prototype-command) 64%,
      var(--prototype-command) 100%
    );
    background-size: 230% 100%;
    background-position: 100% 0;
    background-clip: text;
    -webkit-background-clip: text;
    animation: prototype-command-sweep 2.35s ease-in-out infinite;
  }

  @keyframes prototype-command-sweep {
    0% { background-position: 100% 0; }
    64%, 100% { background-position: -95% 0; }
  }

  .prototype-decision-panel,
  .prototype-failure-detail {
    margin: 24px 0 28px;
    border: 1px solid var(--border-strong);
    border-radius: 14px;
    background: color-mix(in srgb, var(--bg-card) 94%, transparent);
    overflow: hidden;
  }

  .prototype-decision-head,
  .prototype-failure-head {
    display: grid;
    grid-template-columns: 36px minmax(0, 1fr);
    gap: 12px;
    padding: 18px 18px 16px;
  }

  .prototype-decision-icon,
  .prototype-failure-icon {
    display: grid;
    place-items: center;
    width: 36px;
    height: 36px;
    border: 1px solid color-mix(in srgb, var(--warning) 48%, var(--border));
    border-radius: 10px;
    color: var(--warning);
  }

  .prototype-failure-icon {
    border-color: color-mix(in srgb, var(--danger) 48%, var(--border));
    color: var(--danger);
  }

  .prototype-decision-copy,
  .prototype-failure-copy {
    min-width: 0;
  }

  .prototype-decision-kicker,
  .prototype-failure-kicker {
    display: block;
    margin-bottom: 4px;
    color: var(--fg-muted);
    font-size: 11px;
    font-weight: 650;
    letter-spacing: .05em;
  }

  .prototype-decision-copy strong,
  .prototype-failure-copy strong {
    display: block;
    color: var(--fg);
    font-size: 15px;
    line-height: 1.45;
  }

  .prototype-decision-copy p,
  .prototype-failure-copy p {
    margin: 7px 0 0;
    color: var(--fg-muted);
    font-size: 12px;
    line-height: 1.65;
  }

  .prototype-decision-command {
    display: flex;
    align-items: center;
    gap: 9px;
    min-height: 44px;
    margin: 0 18px;
    padding: 0 12px;
    border-top: 1px solid var(--border);
    border-bottom: 1px solid var(--border);
    background: color-mix(in srgb, var(--bg-inset) 72%, transparent);
    color: var(--fg);
    font-family: "Cascadia Mono", "JetBrains Mono", ui-monospace, monospace;
    font-size: 12px;
    overflow: hidden;
  }

  .prototype-decision-command code {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .prototype-decision-actions,
  .prototype-failure-actions {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: 8px;
    padding: 14px 18px 16px;
  }

  .prototype-flow-button {
    min-height: 36px;
    padding: 0 12px;
    border: 1px solid var(--border);
    border-radius: 8px;
    background: transparent;
    color: var(--fg-muted);
    font-size: 12px;
    font-weight: 620;
    cursor: pointer;
  }

  .prototype-flow-button:hover,
  .prototype-flow-button:focus-visible {
    border-color: var(--border-strong);
    background: color-mix(in srgb, var(--fg) 6%, transparent);
    color: var(--fg);
  }

  .prototype-flow-button--primary {
    border-color: color-mix(in srgb, var(--accent) 64%, var(--border));
    background: color-mix(in srgb, var(--accent) 88%, black);
    color: #fff;
  }

  .prototype-flow-button--danger:hover,
  .prototype-flow-button--danger:focus-visible {
    border-color: color-mix(in srgb, var(--danger) 56%, var(--border));
    color: color-mix(in srgb, var(--danger) 76%, var(--fg));
  }

  .prototype-flow-result {
    display: none;
    align-items: flex-start;
    gap: 10px;
    margin-top: 16px;
    padding: 13px 14px;
    border-top: 1px solid var(--border);
    color: var(--fg-muted);
    font-size: 12px;
    line-height: 1.6;
  }

  .prototype-decision-panel.is-resolved .prototype-decision-actions {
    display: none;
  }

  .prototype-decision-panel.is-resolved .prototype-flow-result {
    display: flex;
  }

  .prototype-error-output {
    margin: 0 18px;
    padding: 13px 14px;
    border-top: 1px solid var(--border);
    border-bottom: 1px solid var(--border);
    background: color-mix(in srgb, var(--bg-inset) 78%, transparent);
    color: color-mix(in srgb, var(--danger) 62%, var(--fg-muted));
    font-family: "Cascadia Mono", "JetBrains Mono", ui-monospace, monospace;
    font-size: 12px;
    line-height: 1.7;
    white-space: pre-wrap;
  }

  .prototype-empty-task {
    display: flex;
    min-height: 100%;
    flex-direction: column;
    justify-content: center;
    padding-bottom: 190px;
  }

  .prototype-empty-task-label {
    margin-bottom: 10px;
    color: var(--accent);
    font-family: "Cascadia Mono", "JetBrains Mono", ui-monospace, monospace;
    font-size: 11px;
    font-weight: 700;
    letter-spacing: .12em;
    text-transform: uppercase;
  }

  .prototype-empty-task h2 {
    max-width: 620px;
    margin: 0;
    color: var(--fg);
    font-size: clamp(25px, 2.5vw, 34px);
    font-weight: 650;
    line-height: 1.24;
    text-wrap: balance;
  }

  .prototype-empty-task > p {
    max-width: 620px;
    margin: 14px 0 26px;
    color: var(--fg-muted);
    font-size: 14px;
    line-height: 1.75;
  }

  .prototype-empty-prompts {
    display: grid;
    max-width: 680px;
    border-top: 1px solid var(--border);
  }

  .prototype-empty-prompt {
    display: grid;
    grid-template-columns: 24px minmax(0, 1fr) auto;
    align-items: center;
    gap: 10px;
    min-height: 52px;
    padding: 0 4px;
    border: 0;
    border-bottom: 1px solid var(--border);
    background: transparent;
    color: var(--fg-muted);
    text-align: left;
    cursor: pointer;
  }

  .prototype-empty-prompt:hover,
  .prototype-empty-prompt:focus-visible {
    color: var(--fg);
    background: color-mix(in srgb, var(--fg) 4%, transparent);
  }

  .prototype-empty-prompt svg:last-child {
    opacity: .55;
  }

  .prototype-context-picker {
    position: absolute;
    z-index: 35;
    right: 0;
    bottom: calc(100% + 10px);
    width: min(560px, 100%);
    border: 1px solid var(--border-strong);
    border-radius: 14px;
    background: var(--bg-card);
    box-shadow: 0 18px 60px rgba(0, 0, 0, .28);
    overflow: hidden;
  }

  .prototype-context-picker-head {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 12px 14px;
    border-bottom: 1px solid var(--border);
  }

  .prototype-context-picker-head strong {
    margin-right: auto;
    color: var(--fg);
    font-size: 13px;
  }

  .prototype-context-close,
  .prototype-context-trigger {
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

  .prototype-context-close:hover,
  .prototype-context-close:focus-visible,
  .prototype-context-trigger:hover,
  .prototype-context-trigger:focus-visible {
    background: color-mix(in srgb, var(--fg) 7%, transparent);
    color: var(--fg);
  }

  .prototype-context-search {
    display: flex;
    align-items: center;
    gap: 9px;
    margin: 12px 14px 8px;
    padding: 0 10px;
    border: 1px solid var(--border);
    border-radius: 9px;
    background: var(--bg-inset);
  }

  .prototype-context-search input {
    width: 100%;
    min-height: 38px;
    border: 0;
    outline: 0;
    background: transparent;
    color: var(--fg);
    font: inherit;
    font-size: 13px;
  }

  .prototype-context-search input::placeholder {
    color: var(--fg-subtle, var(--fg-muted));
  }

  .prototype-context-list {
    display: grid;
    padding: 4px 8px 10px;
  }

  .prototype-context-row {
    display: grid;
    grid-template-columns: 30px minmax(0, 1fr) 18px;
    align-items: center;
    gap: 9px;
    min-height: 48px;
    padding: 0 8px;
    border: 0;
    border-radius: 8px;
    background: transparent;
    color: var(--fg-muted);
    text-align: left;
    cursor: pointer;
  }

  .prototype-context-row:hover,
  .prototype-context-row:focus-visible {
    background: color-mix(in srgb, var(--fg) 6%, transparent);
    color: var(--fg);
  }

  .prototype-context-kind {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    border: 1px solid var(--border);
    border-radius: 7px;
    font-family: "Cascadia Mono", "JetBrains Mono", ui-monospace, monospace;
    font-size: 10px;
  }

  .prototype-context-row-copy {
    min-width: 0;
  }

  .prototype-context-row-copy strong,
  .prototype-context-row-copy small {
    display: block;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .prototype-context-row-copy strong {
    color: inherit;
    font-size: 12px;
    font-weight: 590;
  }

  .prototype-context-row-copy small {
    margin-top: 2px;
    color: var(--fg-subtle, var(--fg-muted));
    font-size: 10px;
  }

  .prototype-context-check {
    color: var(--success);
    opacity: 0;
  }

  .prototype-context-row[aria-pressed="true"] .prototype-context-check {
    opacity: 1;
  }

  .prototype-context-chip-row {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    padding: 10px 10px 0;
  }

  .prototype-context-chip {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    max-width: 240px;
    min-height: 26px;
    padding: 0 8px;
    border: 1px solid var(--border);
    border-radius: 7px;
    background: color-mix(in srgb, var(--bg-chip) 84%, transparent);
    color: var(--fg-muted);
    font-size: 11px;
  }

  .prototype-context-chip span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .composer.prototype-context-composer {
    position: relative !important;
  }

  :root[data-prototype-signature="r-code"] .prototype-decision-panel,
  :root[data-prototype-signature="r-code"] .prototype-failure-detail,
  :root[data-prototype-signature="r-code"] .prototype-context-picker {
    border-radius: 18px 8px 18px 8px;
  }

  :root[data-prototype-signature="r-code"] .prototype-flow-button,
  :root[data-prototype-signature="r-code"] .prototype-context-search,
  :root[data-prototype-signature="r-code"] .prototype-context-row {
    border-radius: 8px 3px 8px 3px;
  }

  :root[data-prototype-signature="r-code"] .prototype-empty-task-label,
  :root[data-prototype-signature="r-code"] .prototype-decision-kicker,
  :root[data-prototype-signature="r-code"] .prototype-failure-kicker {
    color: #f08a52;
  }

  @media (max-width: 1359px) {
    .prototype-resilience-session,
    .prototype-empty-task {
      width: min(760px, calc(100% - 40px));
    }
  }

  @media (prefers-reduced-motion: reduce) {
    :root[data-prototype-html-demo="true"] .prototype-resilience-command.is-running code,
    :root[data-prototype-html-demo="true"] .prototype-activity-event[data-prototype-event="running-command"] .prototype-running-command-text {
      color: var(--prototype-command);
      background: none;
      animation: none;
    }
  }
`;

async function installPrototypeResilienceExperience(page) {
  await page.evaluate(() => {
    const mode = new URL(window.location.href).searchParams.get("prototypeFlow");
    if (!mode) return;

    const timeline = document.querySelector(".timeline");
    const composer = document.querySelector(".composer");
    if (!timeline || !composer) return;
    if (document.querySelector(`[data-prototype-flow="${mode}"]`)) return;

    const terminalIcon = `
      <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <rect x="3.5" y="4.5" width="17" height="15" rx="2"></rect><path d="m7 9 3 3-3 3"></path><path d="M13 15h4"></path>
      </svg>`;
    const shieldIcon = `
      <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="M12 3 5 6v5c0 4.6 2.7 8 7 10 4.3-2 7-5.4 7-10V6z"></path><path d="M12 8v4"></path><path d="M12 16h.01"></path>
      </svg>`;
    const errorIcon = `
      <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <circle cx="12" cy="12" r="9"></circle><path d="m9 9 6 6M15 9l-6 6"></path>
      </svg>`;
    const checkIcon = `
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m6 12 4 4 8-8"></path></svg>`;
    const searchIcon = `
      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="11" cy="11" r="6"></circle><path d="m16 16 4 4"></path></svg>`;
    const closeIcon = `
      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.8" stroke-linecap="round" aria-hidden="true"><path d="m7 7 10 10M17 7 7 17"></path></svg>`;
    const arrowIcon = `
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M5 12h14M14 7l5 5-5 5"></path></svg>`;
    const paperclipIcon = `
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor"
        stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m9 12 5.6-5.6a3 3 0 0 1 4.2 4.2l-8.1 8.1a5 5 0 0 1-7.1-7.1l8-8"></path></svg>`;

    const setRoomHeading = (title, status) => {
      const heading = document.querySelector(".room-conversation-title strong");
      const state = document.querySelector(".room-conversation-title span");
      if (heading && title) heading.textContent = title;
      if (state) state.textContent = status;
    };

    if (mode === "context") {
      composer.classList.add("prototype-context-composer");
      const picker = document.createElement("section");
      picker.className = "prototype-context-picker";
      picker.dataset.prototypeFlow = "context";
      picker.setAttribute("role", "dialog");
      picker.setAttribute("aria-label", "引用上下文文件");
      picker.innerHTML = `
        <header class="prototype-context-picker-head">
          ${paperclipIcon}<strong>引用上下文</strong>
          <button type="button" class="prototype-context-close" aria-label="关闭上下文选择器">${closeIcon}</button>
        </header>
        <label class="prototype-context-search">
          ${searchIcon}<span class="sr-only">筛选工作区文件</span>
          <input type="search" placeholder="筛选工作区文件…" autocomplete="off">
        </label>
        <div class="prototype-context-list">
          <button type="button" class="prototype-context-row" data-context-file="src-tauri/frontend/src/components/room/Composer.tsx" aria-pressed="true">
            <span class="prototype-context-kind">TSX</span><span class="prototype-context-row-copy"><strong>Composer.tsx</strong><small>src-tauri/frontend/src/components/room</small></span><span class="prototype-context-check">${checkIcon}</span>
          </button>
          <button type="button" class="prototype-context-row" data-context-file="docs/ui/prototypes/workbench/README.md" aria-pressed="true">
            <span class="prototype-context-kind">MD</span><span class="prototype-context-row-copy"><strong>README.md</strong><small>docs/ui/prototypes/workbench</small></span><span class="prototype-context-check">${checkIcon}</span>
          </button>
          <button type="button" class="prototype-context-row" data-context-file="docs/ui/prototypes/workbench/render-workbench.cjs" aria-pressed="false">
            <span class="prototype-context-kind">JS</span><span class="prototype-context-row-copy"><strong>render-workbench.cjs</strong><small>docs/ui/prototypes/workbench</small></span><span class="prototype-context-check">${checkIcon}</span>
          </button>
        </div>`;
      composer.append(picker);

      const compBox = composer.querySelector(".comp-box") || composer.firstElementChild;
      let chips = compBox?.querySelector(".prototype-context-chip-row");
      if (!chips && compBox) {
        chips = document.createElement("div");
        chips.className = "prototype-context-chip-row";
        compBox.prepend(chips);
      }
      const syncChips = () => {
        if (!chips) return;
        chips.innerHTML = [...picker.querySelectorAll("[data-context-file][aria-pressed='true']")].map((row) => {
          const file = row.dataset.contextFile || "";
          return `<span class="prototype-context-chip">${paperclipIcon}<span>${file.split("/").pop()}</span></span>`;
        }).join("");
      };
      syncChips();

      picker.querySelectorAll("[data-context-file]").forEach((row) => {
        row.addEventListener("click", () => {
          row.setAttribute("aria-pressed", String(row.getAttribute("aria-pressed") !== "true"));
          syncChips();
        });
      });
      picker.querySelector("input")?.addEventListener("input", (event) => {
        const value = String(event.currentTarget.value || "").trim().toLowerCase();
        picker.querySelectorAll("[data-context-file]").forEach((row) => {
          row.hidden = Boolean(value) && !String(row.dataset.contextFile || "").toLowerCase().includes(value);
        });
      });
      picker.querySelector(".prototype-context-close")?.addEventListener("click", () => {
        picker.hidden = true;
        trigger.hidden = false;
      });

      const trigger = document.createElement("button");
      trigger.type = "button";
      trigger.className = "prototype-context-trigger";
      trigger.setAttribute("aria-label", "引用上下文文件");
      trigger.title = "引用上下文文件";
      trigger.hidden = true;
      trigger.innerHTML = paperclipIcon;
      trigger.addEventListener("click", () => {
        picker.hidden = false;
        trigger.hidden = true;
        picker.querySelector("input")?.focus();
      });
      composer.append(trigger);
      document.documentElement.dataset.prototypeFlowInstalled = mode;
      return;
    }

    if (mode === "empty") {
      const empty = document.createElement("article");
      empty.className = "prototype-empty-task";
      empty.dataset.prototypeFlow = "empty";
      empty.innerHTML = `
        <span class="prototype-empty-task-label">New task</span>
        <h2>从结果开始，而不是从工具开始。</h2>
        <p>描述你要完成的事情。R-Code 会读取当前工作区、按需执行命令，并把每一处改动留给你审核。</p>
        <div class="prototype-empty-prompts" aria-label="常用任务示例">
          <button type="button" class="prototype-empty-prompt" data-prompt="定位失败的测试，说明根因并修复。">${terminalIcon}<span>定位失败的测试，说明根因并修复</span>${arrowIcon}</button>
          <button type="button" class="prototype-empty-prompt" data-prompt="解释当前模块的调用路径，并标出关键文件。">${paperclipIcon}<span>解释模块调用路径并标出关键文件</span>${arrowIcon}</button>
          <button type="button" class="prototype-empty-prompt" data-prompt="审核未提交的变更，优先指出行为回归。">${shieldIcon}<span>审核未提交变更并指出行为回归</span>${arrowIcon}</button>
        </div>`;
      timeline.replaceChildren(empty);
      timeline.scrollTop = 0;
      setRoomHeading("新任务", "准备就绪");

      empty.querySelectorAll("[data-prompt]").forEach((button) => {
        button.addEventListener("click", () => {
          const input = composer.querySelector("textarea, input:not([type]), [contenteditable='true']");
          if (!input) return;
          if (input instanceof HTMLInputElement || input instanceof HTMLTextAreaElement) input.value = button.dataset.prompt || "";
          else input.textContent = button.dataset.prompt || "";
          input.dispatchEvent(new InputEvent("input", { bubbles: true, inputType: "insertText", data: button.dataset.prompt || "" }));
          input.focus();
          document.documentElement.dataset.prototypeFlowState = "prompt-selected";
        });
      });
      document.documentElement.dataset.prototypeFlowInstalled = mode;
      return;
    }

    const session = document.createElement("article");
    session.className = "prototype-resilience-session";
    session.dataset.prototypeFlow = mode;

    if (mode === "approval") {
      session.innerHTML = `
        <div class="prototype-user-row"><div class="prototype-user-message">运行完整测试，并修复阻塞合并的问题。</div></div>
        <div class="prototype-resilience-status">${shieldIcon}<span>等待许可 · 1 项</span></div>
        <p class="prototype-resilience-copy">测试需要在工作区写入构建缓存。命令本身不访问网络，也不会修改源文件；我会先等你决定。</p>
        <div class="prototype-resilience-command is-pending">${terminalIcon}<code>Run cargo test --workspace --all-targets</code></div>
        <section class="prototype-decision-panel" aria-labelledby="prototype-approval-title">
          <div class="prototype-decision-head">
            <span class="prototype-decision-icon">${shieldIcon}</span>
            <div class="prototype-decision-copy"><span class="prototype-decision-kicker">需要你的许可</span><strong id="prototype-approval-title">允许执行工作区测试？</strong><p>影响范围：读取工作区，并向 <code>target/</code> 写入构建缓存。</p></div>
          </div>
          <div class="prototype-decision-command">${terminalIcon}<code>cargo test --workspace --all-targets</code></div>
          <div class="prototype-decision-actions">
            <button type="button" class="prototype-flow-button prototype-flow-button--danger" data-decision="deny">拒绝</button>
            <button type="button" class="prototype-flow-button" data-decision="always">始终允许此命令</button>
            <button type="button" class="prototype-flow-button prototype-flow-button--primary" data-decision="once">允许一次</button>
          </div>
          <div class="prototype-flow-result" role="status" aria-live="polite">${checkIcon}<span></span></div>
        </section>`;
      setRoomHeading(null, "等待许可");
      const panel = session.querySelector(".prototype-decision-panel");
      const statusCopy = session.querySelector(".prototype-resilience-status span");
      const command = session.querySelector(".prototype-resilience-command");
      panel?.querySelectorAll("[data-decision]").forEach((button) => {
        button.addEventListener("click", () => {
          const decision = button.dataset.decision;
          panel.classList.add("is-resolved");
          command?.classList.remove("is-pending");
          const result = panel.querySelector(".prototype-flow-result span");
          if (decision === "deny") {
            document.documentElement.dataset.prototypeFlowState = "denied";
            if (statusCopy) statusCopy.textContent = "已拒绝 · 等待新方案";
            if (result) result.textContent = "已拒绝。你可以要求改用只读检查，或调整权限后重试。";
            setRoomHeading(null, "已暂停");
          } else {
            document.documentElement.dataset.prototypeFlowState = decision === "always" ? "allowed-always" : "allowed-once";
            if (statusCopy) statusCopy.textContent = decision === "always" ? "已允许此命令 · 正在执行" : "已允许一次 · 正在执行";
            if (result) result.textContent = decision === "always" ? "规则已保存，命令正在执行。" : "本次已允许，命令正在执行。";
            command?.classList.add("is-running");
            setRoomHeading(null, "正在执行");
          }
        });
      });
    } else if (mode === "failure") {
      session.innerHTML = `
        <div class="prototype-user-row"><div class="prototype-user-message">运行完整测试，并修复阻塞合并的问题。</div></div>
        <div class="prototype-resilience-status">${errorIcon}<span>已处理 42s · 命令失败</span></div>
        <p class="prototype-resilience-copy">测试没有跑完。失败来自一个可恢复的本地端口占用，源文件尚未改动。</p>
        <section class="prototype-failure-detail" aria-labelledby="prototype-failure-title">
          <div class="prototype-failure-head">
            <span class="prototype-failure-icon">${errorIcon}</span>
            <div class="prototype-failure-copy"><span class="prototype-failure-kicker">命令失败 · exit 101</span><strong id="prototype-failure-title">测试服务无法绑定 127.0.0.1:4317</strong><p>关闭占用进程后可直接重试，无需重新开始任务。</p></div>
          </div>
          <pre class="prototype-error-output">error: address already in use (os error 10048)\nhelp: stop PID 18472 or choose an available loopback port</pre>
          <div class="prototype-failure-actions">
            <button type="button" class="prototype-flow-button" data-failure-action="terminal">打开终端</button>
            <button type="button" class="prototype-flow-button prototype-flow-button--primary" data-failure-action="retry">重试命令</button>
          </div>
        </section>
        <p class="prototype-resilience-copy">我保留了失败上下文；重试后会继续从这里向下记录，不会重复前面的输出。</p>`;
      setRoomHeading(null, "需要处理");
      const statusCopy = session.querySelector(".prototype-resilience-status span");
      session.querySelector("[data-failure-action='retry']")?.addEventListener("click", () => {
        document.documentElement.dataset.prototypeFlowState = "retrying";
        if (statusCopy) statusCopy.textContent = "正在重试 · 已保留失败上下文";
        const detail = session.querySelector(".prototype-failure-detail");
        detail?.classList.add("is-retrying");
        detail?.querySelectorAll("button").forEach((button) => { button.disabled = true; });
        setRoomHeading(null, "正在执行");
      });
      session.querySelector("[data-failure-action='terminal']")?.addEventListener("click", () => {
        document.documentElement.dataset.prototypeFlowState = "terminal-requested";
        document.dispatchEvent(new KeyboardEvent("keydown", { key: "2", code: "Digit2", altKey: true, bubbles: true }));
      });
    } else {
      return;
    }

    timeline.replaceChildren(session);
    timeline.scrollTop = 0;
    timeline.setAttribute("aria-label", "任务运行记录");
    document.documentElement.dataset.prototypeFlowInstalled = mode;
  });
}

module.exports = { resilienceExperienceCss, installPrototypeResilienceExperience };
