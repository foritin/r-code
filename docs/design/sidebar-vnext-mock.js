(() => {
  const direction = document.body.dataset.direction || "a";
  const app = document.querySelector("#app");
  const toast = document.querySelector("#toast");

  const iconPaths = {
    flag: '<path d="M4 15s1-1 4-1 5 2 8 2 4-1 4-1V3s-1 1-4 1-5-2-8-2-4 1-4 1z"/><line x1="4" y1="22" x2="4" y2="15"/>',
    plus: '<path d="M5 12h14"/><path d="M12 5v14"/>',
    inbox:
      '<polyline points="22 12 16 12 14 15 10 15 8 12 2 12"/><path d="M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11z"/>',
    folder:
      '<path d="M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2Z"/>',
    search: '<circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/>',
    chevron: '<path d="m6 9 6 6 6-6"/>',
    branch:
      '<line x1="6" x2="6" y1="3" y2="15"/><circle cx="18" cy="6" r="3"/><circle cx="6" cy="18" r="3"/><path d="M18 9a9 9 0 0 1-9 9"/>',
    layout:
      '<rect width="18" height="18" x="3" y="3" rx="2"/><path d="M3 9h18"/><path d="M9 21V9"/>',
    sliders:
      '<line x1="21" x2="14" y1="4" y2="4"/><line x1="10" x2="3" y1="4" y2="4"/><line x1="21" x2="12" y1="12" y2="12"/><line x1="8" x2="3" y1="12" y2="12"/><line x1="21" x2="16" y1="20" y2="20"/><line x1="12" x2="3" y1="20" y2="20"/><line x1="14" x2="14" y1="2" y2="6"/><line x1="8" x2="8" y1="10" y2="14"/><line x1="16" x2="16" y1="18" y2="22"/>',
    file:
      '<path d="M14.5 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V7.5L14.5 2z"/><path d="M14 2v6h6"/>',
    check: '<path d="M20 6 9 17l-5-5"/>',
    alert:
      '<path d="m21.73 18-8-14a2 2 0 0 0-3.48 0l-8 14A2 2 0 0 0 4 21h16a2 2 0 0 0 1.73-3"/><path d="M12 9v4"/><path d="M12 17h.01"/>',
    x: '<path d="M18 6 6 18"/><path d="m6 6 12 12"/>',
    terminal: '<polyline points="4 17 10 11 4 5"/><line x1="12" x2="20" y1="19" y2="19"/>',
    circle: '<circle cx="12" cy="12" r="9"/>',
  };

  const icon = (name, size = 16, className = "") =>
    `<svg class="icon ${className}" width="${size}" height="${size}" viewBox="0 0 24 24" aria-hidden="true">${iconPaths[name] || ""}</svg>`;

  const sessions = [
    {
      id: "checkout",
      provider: "pi",
      providerLabel: "Pi",
      title: "把「提交订单」按钮改成成功态",
      project: "fable5",
      branch: "main",
      state: "review",
      detail: "2 files changed · ready to review",
      file: "public/index.html",
      diff: "+5 −2",
    },
    {
      id: "claude-fable",
      provider: "claude",
      providerLabel: "CC",
      title: "Claude · external session",
      project: "fable5",
      branch: "main",
      state: "live",
      detail: "Reading terminal output",
      file: "README.md",
      diff: "+0 −0",
    },
    {
      id: "moon",
      provider: "pi",
      providerLabel: "Pi",
      title: "写一个月球自传的 HTML",
      project: "fable5",
      branch: "main",
      state: "review",
      detail: "1 file changed · ready to review",
      file: "moon.html",
      diff: "+84 −0",
    },
    {
      id: "claude-scratch",
      provider: "claude",
      providerLabel: "CC",
      title: "Claude · New session",
      project: "Scratch",
      branch: "—",
      state: "live",
      detail: "PTY starting · state preserved",
      file: "terminal",
      diff: "live",
    },
    {
      id: "landing",
      provider: "pi",
      providerLabel: "Pi",
      title: "新建 public/index.html",
      project: "fable5",
      branch: "main",
      state: "accepted",
      detail: "Accepted · archived from attention",
      file: "public/index.html",
      diff: "+42 −0",
    },
  ];

  const state = {
    selected: "checkout",
    filter: "all",
    modal: false,
    modalProvider: "pi",
    activity: "sessions",
    activeProject: "fable5",
    tab: "timeline",
    groups: { fable5: true, scratch: true, history: false },
    sent: [],
  };

  const statusCopy = {
    live: "LIVE",
    review: "REVIEW",
    accepted: "DONE",
  };

  const selectedSession = () => sessions.find((session) => session.id === state.selected) || sessions[0];
  const reviewSessions = () => sessions.filter((session) => session.state === "review");
  const liveSessions = () => sessions.filter((session) => session.state === "live");

  const attentionOrder = { review: 0, live: 1, accepted: 2 };
  const byAttention = (a, b) => attentionOrder[a.state] - attentionOrder[b.state];

  function providerMark(session) {
    return `<span class="provider ${session.provider}" aria-hidden="true">${session.providerLabel}</span>`;
  }

  function sessionRow(session, { hideProject = false } = {}) {
    return `<button class="session-row ${state.selected === session.id ? "selected" : ""}" data-session="${session.id}" type="button" aria-pressed="${state.selected === session.id}">
      ${providerMark(session)}
      <span class="session-name">${session.title}</span>
      <span class="status-label ${session.state}">${statusCopy[session.state]}</span>
      <span class="session-meta">
        <span class="state-dot ${session.state}"></span>
        ${hideProject ? "" : `<span class="project-name">${session.project}</span>`}
        <span>${hideProject ? session.detail : `${session.branch} · ${session.detail}`}</span>
      </span>
    </button>`;
  }

  function brandLine(label) {
    return `<div class="brand-line">
      <span class="brand-mark">${icon("flag", 14)}</span>
      <span class="brand-name">Charter</span>
      <span class="direction-label">${label}</span>
    </div>`;
  }

  function attentionRow(hint = "Review ready") {
    return `<button class="attention-row" data-action="needs-you" type="button">
      ${icon("inbox", 14)}
      <strong>Needs you</strong>
      <span>${hint}</span>
      <span class="attention-count">${reviewSessions().length}</span>
    </button>`;
  }

  function commonFooter({ showProject = false } = {}) {
    return `<div class="rail-footer">
      ${
        showProject
          ? `<button class="footer-row" data-action="switch-project" type="button">${icon("folder", 14)}<span>fable5</span><span>switch</span></button>`
          : ""
      }
      <button class="footer-row active" data-action="open-editor" type="button">${icon("layout", 14)}<span>Editor</span><span>⌘E</span></button>
      <button class="footer-row" data-action="settings" type="button">${icon("sliders", 14)}<span>Settings</span><span></span></button>
    </div>`;
  }

  function focusRail() {
    const filtered = sessions.filter((session) => {
      if (state.filter === "running") return session.state === "live";
      if (state.filter === "review") return session.state === "review";
      return session.state !== "accepted";
    });
    return `<aside class="rail" aria-label="Focus session rail">
      <div class="rail-top">
        ${brandLine("A · Focus")}
        <div class="section-line"><strong>Sessions</strong><span class="count-label">${sessions.length}</span></div>
        <button class="new-button" data-action="new-session" type="button">${icon("plus", 14)} New session</button>
        ${attentionRow()}
        <div class="filter-strip" aria-label="Session filters">
          <button class="${state.filter === "all" ? "active" : ""}" data-filter="all" type="button">Active</button>
          <button class="${state.filter === "running" ? "active" : ""}" data-filter="running" type="button">Running ${liveSessions().length}</button>
          <button class="${state.filter === "review" ? "active" : ""}" data-filter="review" type="button">Review ${reviewSessions().length}</button>
        </div>
      </div>
      <div class="rail-scroll">
        <div class="session-list">${filtered.map((session) => sessionRow(session)).join("")}</div>
        <div class="recent-project">
          <div class="project-heading">${icon("folder", 13)}<strong>Current project</strong><button data-action="switch-project" type="button">Change</button></div>
          <button class="project-row active" data-project="fable5" type="button">${icon("folder", 13)}<strong>fable5</strong><small>4 sessions</small></button>
        </div>
      </div>
      ${commonFooter()}
    </aside>`;
  }

  function projectGroup(id, title, groupSessions, extra = "", { hideProject = true } = {}) {
    const expanded = state.groups[id];
    const needs = groupSessions.filter((session) => session.state === "review").length;
    return `<section class="group ${expanded ? "" : "collapsed"}">
      <button class="group-header" data-group="${id}" type="button" aria-expanded="${expanded}">
        ${icon("chevron", 12, "chevron")}${icon("folder", 13)}
        <strong>${title}</strong>
        ${needs ? `<span class="group-needs">${needs} need you</span>` : ""}
        <span class="group-count">${extra || groupSessions.length}</span>
      </button>
      <div class="group-items">${groupSessions.map((session) => sessionRow(session, { hideProject })).join("")}</div>
    </section>`;
  }

  function groupedRail() {
    const activeFable = sessions.filter((session) => session.project === "fable5" && session.state !== "accepted");
    const scratch = sessions.filter((session) => session.project === "Scratch");
    const history = sessions.filter((session) => session.state === "accepted");
    return `<aside class="rail" aria-label="Grouped session rail">
      <div class="rail-top">
        ${brandLine("B · Grouped")}
        <div class="section-line">
          <strong>Sessions</strong><span class="count-label">grouped by project</span>
          <button class="icon-button" data-action="new-session" type="button" aria-label="New session">${icon("plus", 14)}</button>
        </div>
        ${attentionRow()}
      </div>
      <div class="rail-scroll">
        ${projectGroup("fable5", "fable5", activeFable, "4 active")}
        ${projectGroup("scratch", "Scratch", scratch)}
        ${projectGroup("history", "History", history)}
      </div>
      ${commonFooter({ showProject: true })}
    </aside>`;
  }

  function activityButton(name, iconName, badge = "") {
    return `<button class="activity-nav-item ${state.activity === name ? "active" : ""}" data-activity="${name}" type="button" title="${name}">
      ${icon(iconName, 16)}${badge ? `<span class="mini-badge">${badge}</span>` : ""}
    </button>`;
  }

  function activityPanel() {
    if (state.activity === "inbox") {
      return `<div class="panel-heading"><strong>Needs you</strong><span>${reviewSessions().length} ready</span></div>
        <div class="context-panel-scroll">
          <div class="panel-section-title">Ready to review</div>
          <div class="panel-session-list">${reviewSessions().map((session) => sessionRow(session)).join("")}</div>
          <div class="panel-section-title">Why this is separate</div>
          <div class="context-project"><span class="provider pi">Pi</span><span class="context-project-copy"><strong>Only actionable sessions</strong><span>Completed work stays out of your attention queue.</span></span></div>
        </div>`;
    }
    if (state.activity === "projects") {
      return `<div class="panel-heading"><strong>Projects</strong><span>2 recent</span><button class="icon-button" data-action="new-session" type="button" aria-label="New project session">${icon("plus", 13)}</button></div>
        <div class="context-panel-scroll">
          <div class="panel-section-title">Working context</div>
          <div class="project-list">
            <button class="project-row active" data-project="fable5" type="button">${icon("folder", 14)}<strong>fable5</strong><small>4 sessions</small></button>
            <button class="project-row" data-project="charter" type="button">${icon("folder", 14)}<strong>charter</strong><small>0 sessions</small></button>
          </div>
          <div class="panel-section-title">Project activity</div>
          <div class="context-project">${icon("branch", 15)}<span class="context-project-copy"><strong>main</strong><span>2 live · 2 need review</span></span></div>
        </div>`;
    }
    return `<div class="panel-heading"><strong>Sessions</strong><span>${sessions.length}</span><button class="icon-button" data-action="new-session" type="button" aria-label="New session">${icon("plus", 13)}</button></div>
      <div class="context-panel-scroll">
        <div class="context-project">${icon("folder", 15)}<span class="context-project-copy"><strong>fable5</strong><span>main · current working context</span></span></div>
        <div class="panel-section-title">Needs you</div>
        <div class="panel-session-list">${reviewSessions().map((session) => sessionRow(session, { hideProject: true })).join("")}</div>
        <div class="panel-section-title">Live now</div>
        <div class="panel-session-list">${liveSessions().map((session) => sessionRow(session)).join("")}</div>
        <div class="panel-section-title">Recent</div>
        <div class="panel-session-list">${sessions.filter((session) => session.state === "accepted").map((session) => sessionRow(session)).join("")}</div>
      </div>`;
  }

  function activityRail() {
    return `<aside class="rail activity-shell" aria-label="Activity and contextual navigation">
      <nav class="activity-bar" aria-label="Primary destinations">
        <span class="activity-brand">${icon("flag", 15)}</span>
        ${activityButton("sessions", "terminal")}
        ${activityButton("inbox", "inbox", reviewSessions().length)}
        ${activityButton("projects", "folder")}
        ${activityButton("search", "search")}
        <span class="activity-spacer"></span>
        <button class="activity-nav-item" data-action="open-editor" type="button" title="Editor">${icon("layout", 16)}</button>
        <button class="activity-nav-item" data-action="settings" type="button" title="Settings">${icon("sliders", 16)}</button>
      </nav>
      <section class="context-panel">${activityPanel()}</section>
    </aside>`;
  }

  function hybridSessionsPanel() {
    const activeFable = sessions
      .filter((session) => session.project === "fable5" && session.state !== "accepted")
      .sort(byAttention);
    const scratch = sessions
      .filter((session) => session.project === "Scratch" && session.state !== "accepted")
      .sort(byAttention);
    const history = sessions.filter((session) => session.state === "accepted");
    return `<div class="panel-heading"><strong>Sessions</strong><span>grouped by project</span><button class="icon-button" data-action="new-session" type="button" aria-label="New session">${icon("plus", 13)}</button></div>
      ${reviewSessions().length ? `<div class="panel-pinned">${attentionRow("Open inbox")}</div>` : ""}
      <div class="context-panel-scroll group-scroll">
        ${projectGroup("fable5", "fable5", activeFable, `${activeFable.length} active`)}
        ${projectGroup("scratch", "Scratch", scratch)}
        ${projectGroup("history", "History", history, "", { hideProject: false })}
      </div>
      <div class="rail-footer">
        <button class="footer-row" data-action="switch-project" type="button">${icon("folder", 14)}<span>fable5 · main</span><span>Change</span></button>
      </div>`;
  }

  function hybridRail() {
    return `<aside class="rail activity-shell" aria-label="Grouped activity navigation">
      <nav class="activity-bar" aria-label="Primary destinations">
        <span class="activity-brand">${icon("flag", 15)}</span>
        <span class="direction-chip">D</span>
        ${activityButton("sessions", "terminal")}
        ${activityButton("inbox", "inbox", reviewSessions().length)}
        ${activityButton("projects", "folder")}
        ${activityButton("search", "search")}
        <span class="activity-spacer"></span>
        <button class="activity-nav-item" data-action="open-editor" type="button" title="Editor">${icon("layout", 16)}</button>
        <button class="activity-nav-item" data-action="settings" type="button" title="Settings">${icon("sliders", 16)}</button>
      </nav>
      <section class="context-panel">${state.activity === "sessions" ? hybridSessionsPanel() : activityPanel()}</section>
    </aside>`;
  }

  function renderRail() {
    if (direction === "b") return groupedRail();
    if (direction === "c") return activityRail();
    if (direction === "d") return hybridRail();
    return focusRail();
  }

  function timelineContent(session) {
    if (session.state === "live") {
      return `<div class="timeline-event">
          <span class="event-mark success">${icon("terminal", 11)}</span>
          <div class="event-copy"><strong>Preserved session is live</strong><p>The external PTY keeps its cwd, scrollback and process state while you switch elsewhere.</p><div class="tool-line"><b>PTY</b><span>${session.project === "Scratch" ? "scratch" : "~/work/fable5"} · ${session.provider === "claude" ? "claude" : "codex"}</span><span class="tool-result">connected</span></div></div>
        </div>
        <div class="timeline-event"><span class="event-mark">${icon("circle", 10)}</span><div class="event-copy"><strong>Following terminal output</strong><p>${session.detail}. New writes will appear in Changes without interrupting the native CLI workflow.</p></div></div>`;
    }

    if (session.state === "accepted") {
      return `<div class="timeline-event"><span class="event-mark success">${icon("check", 11)}</span><div class="event-copy"><strong>Session accepted</strong><p>The reviewed change is complete and no longer competes for attention.</p><div class="tool-line"><b>Done</b><span>${session.file}</span><span class="tool-result">${session.diff}</span></div></div></div>`;
    }

    return `<div class="timeline-event">
        <span class="event-mark success">${icon("check", 11)}</span>
        <div class="event-copy"><strong>Implementation complete</strong><p>Updated the success action and preserved the expired-coupon error hierarchy.</p><div class="tool-line"><b>Write</b><span>${session.file}</span><span class="tool-result">${session.diff}</span></div></div>
      </div>
      <div class="timeline-event">
        <span class="event-mark success">${icon("check", 11)}</span>
        <div class="event-copy"><strong>Verification passed</strong><p>Targeted interaction and visual checks completed with no renderer errors.</p><div class="tool-line"><b>Test</b><span>checkout success and expired coupon states</span><span class="tool-result">27 passed</span></div></div>
      </div>
      <section class="review-card">
        <div class="review-card-header">${icon("alert", 14)}<strong>Ready for your review</strong><span>${session.diff}</span></div>
        <p>The agent is waiting. Review the changed file, then accept or request another pass.</p>
        <button class="primary-button" data-action="review" type="button">Review changes</button>
      </section>`;
  }

  function renderConversation(session) {
    return `<section class="conversation">
      <div class="workbench-tabs">
        <button class="workbench-tab ${state.tab === "timeline" ? "active" : ""}" data-tab="timeline" type="button">Session</button>
        <button class="workbench-tab ${state.tab === "files" ? "active" : ""}" data-tab="files" type="button">Files</button>
        <button class="workbench-tab ${state.tab === "terminal" ? "active" : ""}" data-tab="terminal" type="button">Terminal</button>
      </div>
      <div class="timeline">
        <div class="timeline-inner">
          <header class="timeline-heading"><h1>${session.title}</h1><p>${session.provider === "pi" ? "Managed Pi session" : "Preserved external session"} · ${session.project} · ${session.branch}</p></header>
          ${timelineContent(session)}
          ${state.sent.map((message) => `<div class="timeline-event"><span class="event-mark">${providerMark({ provider: "pi", providerLabel: "You" })}</span><div class="event-copy"><strong>You</strong><p>${message}</p></div></div>`).join("")}
        </div>
      </div>
      <form class="composer" data-action="composer">
        <textarea name="message" aria-label="Steer this session" placeholder="Ask a question or steer the next step…"></textarea>
        <div class="composer-footer"><span>${session.provider === "pi" ? "Pi · balanced" : "External PTY · preserved"}</span><span class="grow"></span><span>⌘↵</span><button class="send-button" type="submit">Send</button></div>
      </form>
    </section>`;
  }

  function renderInspector(session) {
    return `<aside class="inspector" aria-label="Changed files">
      <header class="inspector-header">Changes <span class="count-label">${session.state === "live" ? "following" : "1 file"}</span></header>
      <div class="file-list">
        <div class="file-row selected">${icon("file", 13)}<span>${session.file}</span><span class="diffstat"><span class="add">${session.diff.split(" ")[0]}</span> <span class="del">${session.diff.split(" ")[1] || ""}</span></span></div>
        <div class="file-row">${icon("file", 13)}<span>README.md</span><span class="diffstat">context</span></div>
      </div>
      <div class="diff-view">
        <div class="diff-head">${icon("file", 12)}<strong>${session.file}</strong><span>${session.branch}</span></div>
        <div class="code-row"><span class="ln">120</span><span class="ln">120</span><span class="code">.submit-button {</span></div>
        <div class="code-row del"><span class="ln">121</span><span class="ln"></span><span class="code">- background: #1e88e5;</span></div>
        <div class="code-row add"><span class="ln"></span><span class="ln">121</span><span class="code">+ background: var(--success);</span></div>
        <div class="code-row"><span class="ln">122</span><span class="ln">122</span><span class="code">  color: #fff;</span></div>
        <div class="code-row"><span class="ln">123</span><span class="ln">123</span><span class="code">}</span></div>
        <div class="code-row"><span class="ln"></span><span class="ln"></span><span class="code"></span></div>
        <div class="code-row"><span class="ln">137</span><span class="ln">137</span><span class="code">.coupon-hint {</span></div>
        <div class="code-row add"><span class="ln"></span><span class="ln">138</span><span class="code">+ border-left: 4px solid var(--danger);</span></div>
        <div class="code-row"><span class="ln">138</span><span class="ln">139</span><span class="code">  color: var(--danger);</span></div>
        <div class="code-row"><span class="ln">139</span><span class="ln">140</span><span class="code">}</span></div>
      </div>
    </aside>`;
  }

  function renderWorkbench() {
    const session = selectedSession();
    return `<main class="workbench">
      <header class="workbench-topbar">
        ${providerMark(session)}
        <span class="workbench-title"><strong>${session.title}</strong><span>${session.project} · ${session.branch}</span></span>
        <span class="topbar-status ${session.state}"><span class="state-dot ${session.state}"></span>${session.state === "review" ? "Needs you" : session.state === "live" ? "Live" : "Accepted"}</span>
        <button class="small-button" data-action="split" type="button">Split</button>
        <button class="small-button" data-action="more" type="button">More</button>
      </header>
      <div class="workbench-body">${renderConversation(session)}${renderInspector(session)}</div>
      <footer class="statusbar"><span class="state-dot live"></span><strong>Charter runtime connected</strong><span>${session.project}</span><span class="grow"></span><span>${liveSessions().length} live</span><span>${reviewSessions().length} need review</span></footer>
    </main>`;
  }

  function renderModal() {
    if (!state.modal) return "";
    const providerOptions = [
      ["pi", "Pi Session", "Managed plans, tools and review."],
      ["claude", "Claude Code", "Preserved native external PTY."],
      ["codex", "Codex CLI", "Independent implementation or review."],
    ];
    return `<div class="modal-backdrop" data-action="modal-backdrop">
      <form class="modal" data-action="new-session-form">
        <header class="modal-header"><span class="modal-title"><strong>Start a new session</strong><span>Choose the runtime and bind it to a working context.</span></span><button class="icon-button" data-action="close-modal" type="button" aria-label="Close">${icon("x", 14)}</button></header>
        <div class="modal-grid">${providerOptions
          .map(
            ([value, title, detail]) => `<button class="provider-choice ${state.modalProvider === value ? "active" : ""}" data-provider="${value}" type="button">
              <span class="provider ${value}">${value === "pi" ? "Pi" : value === "claude" ? "CC" : "CX"}</span>
              <span class="provider-choice-copy"><strong>${title}</strong><span>${detail}</span></span>
            </button>`,
          )
          .join("")}</div>
        <div class="modal-fields">
          <label class="field">Project<input name="project" value="fable5" /></label>
          <label class="field">Branch<input name="branch" value="main" /></label>
          <label class="field">What should this session do?<textarea name="goal">Polish the checkout success and expired coupon states.</textarea></label>
        </div>
        <div class="modal-actions"><button class="secondary-button" data-action="close-modal" type="button">Cancel</button><button class="primary-button" type="submit">Create session</button></div>
      </form>
    </div>`;
  }

  function render() {
    app.innerHTML = `<div class="mock-shell direction-${direction}">${renderRail()}${renderWorkbench()}</div>${renderModal()}`;
    bind();
  }

  let toastTimer;
  function showToast(message) {
    clearTimeout(toastTimer);
    toast.textContent = message;
    toast.classList.add("show");
    toastTimer = setTimeout(() => toast.classList.remove("show"), 1800);
  }

  function bind() {
    document.querySelectorAll("[data-session]").forEach((button) => {
      button.addEventListener("click", () => {
        state.selected = button.dataset.session;
        render();
      });
    });

    document.querySelectorAll("[data-filter]").forEach((button) => {
      button.addEventListener("click", () => {
        state.filter = button.dataset.filter;
        render();
      });
    });

    document.querySelectorAll("[data-group]").forEach((button) => {
      button.addEventListener("click", () => {
        state.groups[button.dataset.group] = !state.groups[button.dataset.group];
        render();
      });
    });

    document.querySelectorAll("[data-activity]").forEach((button) => {
      button.addEventListener("click", () => {
        if (button.dataset.activity === "search") {
          showToast("Search opens as a command surface in this direction.");
          return;
        }
        state.activity = button.dataset.activity;
        render();
      });
    });

    document.querySelectorAll('[data-action="new-session"]').forEach((button) => {
      button.addEventListener("click", () => {
        state.modal = true;
        render();
      });
    });

    document.querySelectorAll('[data-action="close-modal"]').forEach((button) => {
      button.addEventListener("click", () => {
        state.modal = false;
        render();
      });
    });

    document.querySelectorAll("[data-provider]").forEach((button) => {
      button.addEventListener("click", () => {
        state.modalProvider = button.dataset.provider;
        render();
      });
    });

    document.querySelector('[data-action="new-session-form"]')?.addEventListener("submit", (event) => {
      event.preventDefault();
      const form = new FormData(event.currentTarget);
      const provider = state.modalProvider;
      const created = {
        id: `new-${Date.now()}`,
        provider,
        providerLabel: provider === "pi" ? "Pi" : provider === "claude" ? "CC" : "CX",
        title: provider === "pi" ? "Pi · Checkout polish" : provider === "claude" ? "Claude · Checkout polish" : "Codex · Checkout polish",
        project: form.get("project") || "fable5",
        branch: form.get("branch") || "main",
        state: "live",
        detail: "Session created · starting runtime",
        file: "No changes yet",
        diff: "+0 −0",
      };
      sessions.unshift(created);
      state.selected = created.id;
      state.modal = false;
      showToast("New session created and bound to fable5.");
      render();
    });

    document.querySelector('[data-action="composer"]')?.addEventListener("submit", (event) => {
      event.preventDefault();
      const field = event.currentTarget.elements.message;
      const message = field.value.trim();
      if (!message) {
        showToast("Write a short instruction first.");
        return;
      }
      state.sent.push(message);
      const session = selectedSession();
      session.state = "live";
      session.detail = "Working on your follow-up";
      showToast("Follow-up sent. This session is live again.");
      render();
    });

    document.querySelectorAll("[data-tab]").forEach((button) => {
      button.addEventListener("click", () => {
        state.tab = button.dataset.tab;
        showToast(`${button.textContent.trim()} stays inside the current session.`);
        render();
      });
    });

    document.querySelectorAll("[data-project]").forEach((button) => {
      button.addEventListener("click", () => {
        state.activeProject = button.dataset.project;
        showToast(`${button.dataset.project} is now the working context.`);
      });
    });

    document.querySelector('[data-action="needs-you"]')?.addEventListener("click", () => {
      if (direction === "c" || direction === "d") state.activity = "inbox";
      else state.filter = "review";
      render();
    });

    document.querySelector('[data-action="review"]')?.addEventListener("click", () => {
      showToast("Review opened in the inspector without leaving the session.");
    });

    document.querySelectorAll('[data-action="open-editor"]').forEach((button) => {
      button.addEventListener("click", () => showToast("Editor opened. The Session rail remains persistent."));
    });
    document.querySelectorAll('[data-action="settings"]').forEach((button) => {
      button.addEventListener("click", () => showToast("Settings opens as an overlay."));
    });
    document.querySelectorAll('[data-action="switch-project"]').forEach((button) => {
      button.addEventListener("click", () => showToast("Project switcher opened."));
    });
    document.querySelectorAll('[data-action="split"], [data-action="more"]').forEach((button) => {
      button.addEventListener("click", () => showToast(`${button.textContent.trim()} is available in the session toolbar.`));
    });
  }

  render();
})();
