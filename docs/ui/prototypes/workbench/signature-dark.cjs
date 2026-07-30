const signatureDarkCss = String.raw`
  :root[data-prototype-signature="r-code"] {
    color-scheme: dark;
    --prototype-workspace-radius: 28px;
    --font-ui: "Segoe UI Variable Text", "Microsoft YaHei UI", "PingFang SC", sans-serif;
    --font-display: Bahnschrift, "Segoe UI Variable Display", "Microsoft YaHei UI", sans-serif;
    --font-mono: "Cascadia Code", "SFMono-Regular", Consolas, monospace;

    --bg-app: #10110f;
    --bg-sidebar: #1c1918;
    --bg-panel: #151614;
    --bg-card: #1b1c19;
    --bg-chip: #25241f;
    --bg-inset: #0b0c0a;
    --bg-hover: #20211e;
    --bg-active: #29261f;
    --border: #2b2b27;
    --border-strong: #4b463d;
    --fg: #f1efe9;
    --fg-muted: #a49f95;
    --fg-faint: #817b72;
    --accent: #f4742b;
    --accent-2: #c49a61;
    --accent-fg: #17100b;
    --warning: #e5a15b;
    --success: #69c798;
    --danger: #df6b62;

    --prototype-sidebar: #1c1918;
    --prototype-sidebar-glow: rgba(244, 116, 43, .045);
    --prototype-link: #f0a262;
    --prototype-command: #92928b;
    --prototype-user: #20211e;
    --signature-accent-soft: color-mix(in srgb, var(--accent) 14%, transparent);
    --signature-accent-faint: color-mix(in srgb, var(--accent) 7%, transparent);
    --signature-keyline: color-mix(in srgb, var(--accent) 34%, var(--border));
  }

  :root[data-prototype-signature="r-code"] #app.app-shell {
    background-color: var(--prototype-sidebar) !important;
    background-image:
      linear-gradient(90deg, rgba(244, 116, 43, .018), transparent 72%),
      radial-gradient(72% 76% at 2% 100%, rgba(196, 74, 35, .065), transparent 67%) !important;
  }

  :root[data-prototype-signature="r-code"] #app.app-shell .main {
    border-color: #32312c !important;
    border-radius: var(--prototype-workspace-radius) 0 0 0;
    background: var(--bg-app);
    box-shadow: -18px -18px 46px rgba(0, 0, 0, .12);
  }

  :root[data-prototype-signature="r-code"] #app.app-shell .main::before,
  :root[data-prototype-signature="r-code"] #app.app-shell .main::after {
    content: "";
    position: absolute;
    z-index: 30;
    pointer-events: none;
    background: linear-gradient(90deg, rgba(244, 116, 43, .72), rgba(244, 116, 43, 0));
  }

  :root[data-prototype-signature="r-code"] #app.app-shell .main::before {
    top: -1px;
    left: 31px;
    width: 54px;
    height: 1px;
  }

  :root[data-prototype-signature="r-code"] #app.app-shell .main::after {
    top: 31px;
    left: -1px;
    width: 1px;
    height: 42px;
    background: linear-gradient(180deg, rgba(244, 116, 43, .56), rgba(244, 116, 43, 0));
  }

  :root[data-prototype-signature="r-code"] #app .app-sidebar,
  :root[data-prototype-signature="r-code"] #app.app-shell .app-topbar {
    background: transparent !important;
  }

  :root[data-prototype-signature="r-code"] #app.app-shell .app-topbar {
    padding-inline: 12px 9px;
  }

  :root[data-prototype-signature="r-code"] .prototype-desktop-nav,
  :root[data-prototype-signature="r-code"] .desktop-navigation {
    gap: 10px;
  }

  :root[data-prototype-signature="r-code"] .prototype-history-actions,
  :root[data-prototype-signature="r-code"] .desktop-history-actions {
    gap: 4px;
    padding-right: 9px;
    border-right: 1px solid rgba(255, 255, 255, .055);
  }

  :root[data-prototype-signature="r-code"] .prototype-desktop-button,
  :root[data-prototype-signature="r-code"] .desktop-nav-button {
    height: 27px;
    padding-inline: 7px;
    border-radius: 4px;
    color: #aaa49b;
    font-family: var(--font-display);
    font-size: 11px;
    font-weight: 500;
    letter-spacing: .055em;
  }

  :root[data-prototype-signature="r-code"] .prototype-history-button,
  :root[data-prototype-signature="r-code"] .desktop-history-button {
    width: 27px;
    padding: 0;
  }

  :root[data-prototype-signature="r-code"] .prototype-desktop-button:hover,
  :root[data-prototype-signature="r-code"] .prototype-desktop-button:focus-visible,
  :root[data-prototype-signature="r-code"] .prototype-desktop-button[aria-expanded="true"],
  :root[data-prototype-signature="r-code"] .desktop-nav-button:hover,
  :root[data-prototype-signature="r-code"] .desktop-nav-button:focus-visible,
  :root[data-prototype-signature="r-code"] .desktop-nav-button[aria-expanded="true"] {
    background: rgba(244, 116, 43, .08);
    color: var(--fg);
  }

  :root[data-prototype-signature="r-code"] .sidebar-brand-row {
    padding-bottom: 5px;
  }

  :root[data-prototype-signature="r-code"] .sidebar-brand {
    gap: 10px;
    font-family: var(--font-display);
    letter-spacing: .02em;
  }

  :root[data-prototype-signature="r-code"] .sidebar-brand-mark {
    width: 27px;
    height: 27px;
    border: 0;
    border-radius: 9px 9px 3px 9px;
    background: var(--accent);
    box-shadow:
      0 0 0 1px rgba(255, 154, 91, .24),
      0 8px 22px rgba(116, 45, 13, .22);
    color: #1b110a;
    font-family: var(--font-display);
    font-size: 13px;
    font-weight: 720;
  }

  :root[data-prototype-signature="r-code"] .sidebar-new,
  :root[data-prototype-signature="r-code"] .sidebar-collapse,
  :root[data-prototype-signature="r-code"] .sidebar-search,
  :root[data-prototype-signature="r-code"] .sidebar-settings,
  :root[data-prototype-signature="r-code"] .sidebar-project-manage {
    border-radius: 5px;
  }

  :root[data-prototype-signature="r-code"] .sidebar-new {
    font-family: var(--font-display);
    font-weight: 560;
  }

  :root[data-prototype-signature="r-code"] .sidebar-nav {
    gap: 2px;
    padding-block: 5px 12px;
    border-bottom: 1px solid rgba(255, 255, 255, .06);
  }

  :root[data-prototype-signature="r-code"] .sidebar-nav-item {
    position: relative;
    min-height: 34px;
    border-radius: 3px 9px 9px 3px;
    color: #aaa59c;
  }

  :root[data-prototype-signature="r-code"] .sidebar-nav-item:hover {
    background: rgba(255, 255, 255, .025);
    color: var(--fg);
  }

  :root[data-prototype-signature="r-code"] .sidebar-nav-item.active {
    background: linear-gradient(90deg, rgba(244, 116, 43, .13), rgba(244, 116, 43, .025) 62%, transparent);
    box-shadow: inset 2px 0 0 var(--accent);
    color: var(--fg);
  }

  :root[data-prototype-signature="r-code"] .sidebar-nav-item.active svg {
    color: #ff9b5d;
  }

  :root[data-prototype-signature="r-code"] .sidebar-section-head {
    min-height: 28px;
    color: #817b72;
    font-family: var(--font-display);
    font-size: 10px;
    font-weight: 600;
    letter-spacing: .13em;
  }

  :root[data-prototype-signature="r-code"] .sidebar-project-head {
    border-radius: 5px;
    color: #d8d4cc;
    font-family: var(--font-display);
  }

  :root[data-prototype-signature="r-code"] .sidebar-task-row,
  :root[data-prototype-signature="r-code"] .sidebar-task {
    border-radius: 3px 8px 8px 3px;
  }

  :root[data-prototype-signature="r-code"] .sidebar-task-row.active,
  :root[data-prototype-signature="r-code"] .sidebar-task.active {
    background: linear-gradient(90deg, rgba(244, 116, 43, .105), rgba(244, 116, 43, .02) 70%, transparent);
    box-shadow: inset 2px 0 0 var(--accent);
  }

  :root[data-prototype-signature="r-code"] .sidebar-task-row.prototype-pinned,
  :root[data-prototype-signature="r-code"] .conversation-row.prototype-pinned {
    box-shadow: inset 2px 0 0 var(--accent);
  }

  :root[data-prototype-signature="r-code"] .task-state-dot,
  :root[data-prototype-signature="r-code"] .sidebar-live {
    width: 3px;
    height: 10px;
    border-radius: 2px;
  }

  :root[data-prototype-signature="r-code"] .prototype-row-actions {
    gap: 3px;
    padding: 2px;
    border: 1px solid rgba(255, 255, 255, .065);
    border-radius: 6px;
    background: #22211e;
    box-shadow: 0 8px 18px rgba(0, 0, 0, .28);
  }

  :root[data-prototype-signature="r-code"] .prototype-action {
    border-radius: 4px;
  }

  :root[data-prototype-signature="r-code"] .prototype-action:hover,
  :root[data-prototype-signature="r-code"] .prototype-action:focus-visible,
  :root[data-prototype-signature="r-code"] .prototype-action[aria-pressed="true"] {
    background: var(--signature-accent-soft);
    color: #ffab73;
  }

  :root[data-prototype-signature="r-code"] .room-conversation-head {
    min-height: 48px;
    padding-inline: 18px 14px;
    border-bottom-color: rgba(255, 255, 255, .07);
  }

  :root[data-prototype-signature="r-code"] .room-conversation-head > svg {
    color: #7f7b73;
  }

  :root[data-prototype-signature="r-code"] .room-conversation-title {
    position: relative;
    padding-left: 12px;
  }

  :root[data-prototype-signature="r-code"] .room-conversation-title::before {
    content: "";
    position: absolute;
    top: 3px;
    bottom: 3px;
    left: 0;
    width: 2px;
    border-radius: 2px;
    background: linear-gradient(180deg, var(--accent), rgba(244, 116, 43, .18));
  }

  :root[data-prototype-signature="r-code"] .room-conversation-title strong {
    font-family: var(--font-display);
    font-weight: 620;
    letter-spacing: .015em;
  }

  :root[data-prototype-signature="r-code"] .timeline,
  :root[data-prototype-signature="r-code"] .convo,
  :root[data-prototype-signature="r-code"] .workbench,
  :root[data-prototype-signature="r-code"] .prototype-sidebar-host {
    background: var(--bg-app);
  }

  :root[data-prototype-signature="r-code"] .prototype-session {
    width: min(92%, 930px);
  }

  :root[data-prototype-signature="r-code"] .prototype-user-message {
    border: 1px solid rgba(255, 255, 255, .055);
    border-radius: 14px 14px 4px 14px;
    background: var(--prototype-user);
    box-shadow: 0 9px 24px rgba(0, 0, 0, .12);
  }

  :root[data-prototype-signature="r-code"] .prototype-session-summary {
    color: #928d84;
    font-family: var(--font-display);
    letter-spacing: .035em;
  }

  :root[data-prototype-signature="r-code"] .prototype-session-summary::before {
    content: "";
    width: 18px;
    height: 1px;
    margin-right: 8px;
    background: linear-gradient(90deg, var(--accent), rgba(244, 116, 43, .06));
  }

  :root[data-prototype-signature="r-code"] .prototype-activity-trace {
    position: relative;
    padding-left: 28px;
  }

  :root[data-prototype-signature="r-code"] .prototype-activity-trace::before {
    content: "";
    position: absolute;
    top: 7px;
    bottom: 8px;
    left: 7px;
    width: 1px;
    background: linear-gradient(180deg, rgba(244, 116, 43, .48), #35342f 18%, #292925 84%, rgba(244, 116, 43, .12));
  }

  :root[data-prototype-signature="r-code"] .prototype-activity-event,
  :root[data-prototype-signature="r-code"] .prototype-context-event,
  :root[data-prototype-signature="r-code"] .prototype-agent-event {
    position: relative;
  }

  :root[data-prototype-signature="r-code"] .prototype-activity-event::before,
  :root[data-prototype-signature="r-code"] .prototype-context-event::before,
  :root[data-prototype-signature="r-code"] .prototype-agent-event::before {
    content: "";
    position: absolute;
    top: 9px;
    left: -25px;
    width: 7px;
    height: 7px;
    border: 1px solid #625b50;
    border-radius: 2px;
    background: var(--bg-app);
    transform: rotate(45deg);
  }

  :root[data-prototype-signature="r-code"] [data-prototype-event="running-command"]::before {
    border-color: var(--accent);
    background: var(--accent);
    box-shadow: 0 0 0 4px rgba(244, 116, 43, .08);
  }

  :root[data-prototype-signature="r-code"] .prototype-activity-toggle,
  :root[data-prototype-signature="r-code"] .prototype-activity-static,
  :root[data-prototype-signature="r-code"] .prototype-activity-live-command,
  :root[data-prototype-signature="r-code"] .prototype-agent-event {
    min-height: 28px;
  }

  :root[data-prototype-signature="r-code"] .prototype-activity-icon,
  :root[data-prototype-signature="r-code"] .prototype-command-icon,
  :root[data-prototype-signature="r-code"] .prototype-agent-command-icon {
    border-radius: 3px;
    color: #8e8a82;
  }

  :root[data-prototype-signature="r-code"] .prototype-agent-chip,
  :root[data-prototype-signature="r-code"] .prototype-agent-status-chip,
  :root[data-prototype-signature="r-code"] .chip {
    border-radius: 5px;
  }

  :root[data-prototype-signature="r-code"] .prototype-agent-chip[aria-pressed="true"] {
    border-color: color-mix(in srgb, var(--accent) 28%, var(--border));
    background: rgba(244, 116, 43, .07);
  }

  :root[data-prototype-signature="r-code"] .prototype-shell-card,
  :root[data-prototype-signature="r-code"] .prototype-diff-card,
  :root[data-prototype-signature="r-code"] .review-files,
  :root[data-prototype-signature="r-code"] .review-diff,
  :root[data-prototype-signature="r-code"] .terminal-panel {
    border-radius: 10px 10px 4px 10px;
    border-color: #37362f;
    background: #171815;
    box-shadow: none;
  }

  :root[data-prototype-signature="r-code"] .prototype-diff-head,
  :root[data-prototype-signature="r-code"] .prototype-shell-label {
    font-family: var(--font-display);
  }

  :root[data-prototype-signature="r-code"] .prototype-completion {
    border-top-color: rgba(255, 255, 255, .07);
  }

  :root[data-prototype-signature="r-code"] .prototype-completion-state {
    color: var(--success);
  }

  :root[data-prototype-signature="r-code"] .prototype-completion-action {
    border-radius: 4px;
  }

  :root[data-prototype-signature="r-code"] .prototype-file-link {
    color: var(--prototype-link);
    text-decoration-color: rgba(240, 162, 98, .4);
  }

  :root[data-prototype-signature="r-code"] .composer {
    padding-inline: 12px;
  }

  :root[data-prototype-signature="r-code"] .comp-box {
    position: relative;
    border: 1px solid #36352f;
    border-radius: 16px 16px 5px 16px;
    background: var(--bg-card);
    box-shadow:
      0 -10px 34px rgba(0, 0, 0, .12),
      0 10px 24px rgba(0, 0, 0, .16);
  }

  :root[data-prototype-signature="r-code"] .comp-box::before {
    content: "";
    position: absolute;
    top: -1px;
    left: 24px;
    width: 58px;
    height: 1px;
    background: linear-gradient(90deg, var(--accent), rgba(244, 116, 43, 0));
    pointer-events: none;
  }

  :root[data-prototype-signature="r-code"] .comp-box textarea {
    color: var(--fg);
  }

  :root[data-prototype-signature="r-code"] .provider-pill,
  :root[data-prototype-signature="r-code"] .project-access-trigger {
    border-radius: 5px;
    background: #23231f;
  }

  :root[data-prototype-signature="r-code"] .provider-pill > span {
    color: #ffa268;
    font-family: var(--font-display);
    letter-spacing: .035em;
  }

  :root[data-prototype-signature="r-code"] .send {
    border-radius: 9px 9px 3px 9px;
    background: var(--accent);
    color: #1a1009;
  }

  :root[data-prototype-signature="r-code"] .send:disabled {
    background: #82502f;
    color: #1c1713;
  }

  :root[data-prototype-signature="r-code"] .room-splitter::before {
    background: #32312c;
  }

  :root[data-prototype-signature="r-code"] .workbench-head,
  :root[data-prototype-signature="r-code"] .prototype-agent-page-header {
    border-bottom-color: rgba(255, 255, 255, .07);
  }

  :root[data-prototype-signature="r-code"] .workbench-tab,
  :root[data-prototype-signature="r-code"] .wb-tab,
  :root[data-prototype-signature="r-code"] [role="tab"] {
    font-family: var(--font-display);
    letter-spacing: .035em;
  }

  :root[data-prototype-signature="r-code"] .workbench-tab[aria-selected="true"],
  :root[data-prototype-signature="r-code"] .wb-tab.active,
  :root[data-prototype-signature="r-code"] [role="tab"][aria-selected="true"] {
    color: #ffac73;
  }

  :root[data-prototype-signature="r-code"] .prototype-context-panel {
    padding: 4px 20px 12px;
    border: 0;
    border-radius: 0;
    background: transparent;
    box-shadow: none;
  }

  :root[data-prototype-signature="r-code"] .prototype-context-section {
    padding-block: 17px;
  }

  :root[data-prototype-signature="r-code"] .prototype-context-section + .prototype-context-section {
    border-top: 1px solid rgba(255, 255, 255, .07);
  }

  :root[data-prototype-signature="r-code"] .prototype-context-heading,
  :root[data-prototype-signature="r-code"] .prototype-agent-section-heading {
    font-family: var(--font-display);
    letter-spacing: .05em;
  }

  :root[data-prototype-signature="r-code"] .prototype-context-subagents-button,
  :root[data-prototype-signature="r-code"] .prototype-source-row,
  :root[data-prototype-signature="r-code"] .prototype-agent-row {
    border-radius: 6px 6px 2px 6px;
  }

  :root[data-prototype-signature="r-code"] .prototype-context-subagents-button {
    border-color: rgba(255, 255, 255, .065);
    background: rgba(255, 255, 255, .018);
  }

  :root[data-prototype-signature="r-code"] .prototype-agent-row {
    background: transparent;
  }

  :root[data-prototype-signature="r-code"] .prototype-agent-row.is-running {
    background: transparent;
  }

  :root[data-prototype-signature="r-code"] .prototype-agent-row:hover,
  :root[data-prototype-signature="r-code"] .prototype-agent-row:focus-visible {
    background: linear-gradient(90deg, rgba(244, 116, 43, .075), transparent 72%);
  }

  :root[data-prototype-signature="r-code"] .prototype-agent-avatar {
    border-radius: 5px 9px 5px 9px;
    background:
      linear-gradient(135deg, color-mix(in srgb, var(--prototype-agent-color, var(--accent)) 62%, #171815), #171815 64%);
    box-shadow: inset 0 0 0 3px rgba(255, 255, 255, .035);
  }

  :root[data-prototype-signature="r-code"] .prototype-agent-spinner,
  :root[data-prototype-signature="r-code"] .prototype-agent-complete-mark {
    border-radius: 3px;
  }

  :root[data-prototype-signature="r-code"] .btn,
  :root[data-prototype-signature="r-code"] .iconbtn,
  :root[data-prototype-signature="r-code"] .input,
  :root[data-prototype-signature="r-code"] .menu-item,
  :root[data-prototype-signature="r-code"] .popover {
    border-radius: 6px;
  }

  @media (max-width: 1260px) {
    :root[data-prototype-signature="r-code"] .prototype-session {
      width: min(94%, 860px);
    }

    :root[data-prototype-signature="r-code"] .prototype-activity-trace {
      padding-left: 24px;
    }

    :root[data-prototype-signature="r-code"] .prototype-activity-trace::before {
      left: 6px;
    }

    :root[data-prototype-signature="r-code"] .prototype-activity-event::before,
    :root[data-prototype-signature="r-code"] .prototype-context-event::before,
    :root[data-prototype-signature="r-code"] .prototype-agent-event::before {
      left: -22px;
    }
  }
`;

async function installSignatureDark(page) {
  await page.evaluate(() => {
    document.documentElement.dataset.prototypeSignature = "r-code";

    const replaceBrandText = (value) => String(value || "")
      .replace(/Codex CLI/g, "R-Code Agent")
      .replace(/Codex/g, "R-Code")
      .replace(/gpt-5\.6(?:-sol)?/g, "Auto");

    const provider = document.querySelector(".provider-pill");
    if (provider) {
      const providerName = provider.querySelector(":scope > span");
      const modelName = provider.querySelector(":scope > small");
      if (providerName) providerName.textContent = "R-Code";
      if (modelName) modelName.textContent = "Auto";
      provider.title = "本会话使用：R-Code / 自动";
    }

    const walker = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
    const textNodes = [];
    while (walker.nextNode()) textNodes.push(walker.currentNode);
    textNodes.forEach((node) => {
      const nextValue = replaceBrandText(node.nodeValue);
      if (nextValue !== node.nodeValue) node.nodeValue = nextValue;
    });

    document.querySelectorAll("[title], [aria-label]").forEach((element) => {
      for (const attribute of ["title", "aria-label"]) {
        if (!element.hasAttribute(attribute)) continue;
        element.setAttribute(attribute, replaceBrandText(element.getAttribute(attribute)));
      }
    });

    document.documentElement.dataset.prototypeSignatureReady = "true";
  });
}

module.exports = { signatureDarkCss, installSignatureDark };
