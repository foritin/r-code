/**
 * R-Code Frontend Application Logic [doc-09] [doc-11]
 *
 * Vanilla JS application that:
 * - Calls Tauri commands (via window.__TAURI__ or fetch fallback)
 * - Renders Home (launcher + mission control)
 * - Renders Task Room (timeline + approval + changes + verification + decision)
 * - Renders Editor (file tree + code view + terminal + settings)
 * - Handles navigation between views
 * - Implements keyboard navigation
 * - Implements window zoom (80-200%)
 * - Implements accessible diff text mode (F7/Shift+F7 + aria-live)
 * - Implements three-depth replay (Recap/Explore/Verify)
 * - Implements context injection (@path, selection ref, external session)
 */

'use strict';

// ============================================================================
// State Management
// ============================================================================
const App = {
  currentView: 'home',
  currentTaskId: null,
  currentDepth: 'recap',
  zoomLevel: 100,
  accessibleDiffMode: false,
  activeTerminalId: null,
  activeFilePath: null,
};

// ============================================================================
// IPC Bridge -- Tauri command invocation [doc-09]
// ============================================================================
const IPC = {
  /**
   * Invoke a Tauri command.
   * Uses window.__TAURI__ if available, falls back to fetch (dev mode).
   */
  async invoke(command, args = {}) {
    // Tauri v2 integration
    if (window.__TAURI__ && window.__TAURI__.core) {
      return window.__TAURI__.core.invoke(command, args);
    }
    // Tauri v1 integration
    if (window.__TAURI__ && window.__TAURI__.invoke) {
      return window.__TAURI__.invoke(command, args);
    }
    // Dev fallback -- would call a local HTTP server in development
    // For now, return empty/mock data
    console.warn(`[IPC] Tauri not available, returning mock for: ${command}`);
    return mockInvoke(command, args);
  },
};

/**
 * Mock IPC responses for development without Tauri.
 */
async function mockInvoke(command, args) {
  switch (command) {
    case 'task_create':
      return {
        id: 'mock-' + Date.now(),
        project_id: args.project_id || '/proj',
        title: args.title || 'Mock Task',
        goal: args.goal || '',
        mode: args.mode || 'ask',
        state: 'idle',
        worktree_path: null,
        created_at: new Date().toISOString(),
        updated_at: new Date().toISOString(),
      };
    case 'task_list':
      return [];
    case 'task_detail':
      return {
        task: { id: args.task_id, state: 'idle' },
        runs: [],
        events: [],
        changes: [],
        permissions: [],
        verifications: [],
      };
    case 'terminal_list':
      return [];
    case 'workspace_list':
      return [];
    case 'recovery_data':
      return { interrupted_tasks: [], orphaned_permissions: 0 };
    case 'settings_get':
      return { log_level: 'info', default_provider: 'anthropic' };
    default:
      return null;
  }
}

// ============================================================================
// Navigation [doc-09] [doc-18 M11-05 keyboard nav]
// ============================================================================
const Nav = {
  /** Switch to a view by name. */
  switchTo(view) {
    App.currentView = view;

    // Update sidebar
    document.querySelectorAll('.nav-item[data-view]').forEach((btn) => {
      btn.classList.toggle('active', btn.dataset.view === view);
      btn.setAttribute('aria-pressed', btn.dataset.view === view);
    });

    // Update views
    document.querySelectorAll('.view').forEach((v) => {
      v.classList.remove('active');
    });
    const target = document.getElementById(`view-${view}`);
    if (target) {
      target.classList.add('active');
    }

    // Load view data
    Views[view] && Views[view].load && Views[view].load();
  },

  /** Navigate to task room for a specific task. */
  openTaskRoom(taskId) {
    App.currentTaskId = taskId;
    Nav.switchTo('room');
  },
};

// ============================================================================
// Window Zoom -- 80% to 200% [doc-18 M11-05]
// ============================================================================
const Zoom = {
  min: 80,
  max: 200,
  step: 10,

  /** Set zoom level (80-200). */
  set(level) {
    App.zoomLevel = Math.max(this.min, Math.min(this.max, level));
    const app = document.getElementById('app');
    app.setAttribute('data-zoom', String(App.zoomLevel));

    // Update slider and display
    const slider = document.getElementById('zoom-slider');
    const value = document.getElementById('zoom-value');
    if (slider) slider.value = App.zoomLevel;
    if (value) value.textContent = App.zoomLevel + '%';

    announce(`缩放: ${App.zoomLevel}%`);
  },

  /** Zoom in. */
  in() {
    this.set(App.zoomLevel + this.step);
  },

  /** Zoom out. */
  out() {
    this.set(App.zoomLevel - this.step);
  },

  /** Reset to 100%. */
  reset() {
    this.set(100);
  },
};

// ============================================================================
// Accessible Diff Text Mode [doc-18 M11-05]
// ============================================================================
const DiffMode = {
  /** Toggle accessible diff text mode. */
  toggle() {
    App.accessibleDiffMode = !App.accessibleDiffMode;
    const diffPanel = document.getElementById('diff-text-mode');
    const toggleBtn = document.querySelector('[data-action="toggle-diff-mode"]');
    const settingsToggle = document.getElementById('accessible-diff-toggle');

    if (diffPanel) {
      diffPanel.hidden = !App.accessibleDiffMode;
    }
    if (toggleBtn) {
      toggleBtn.setAttribute('aria-pressed', String(App.accessibleDiffMode));
    }
    if (settingsToggle) {
      settingsToggle.checked = App.accessibleDiffMode;
    }

    announce(
      App.accessibleDiffMode
        ? '无障碍 Diff 文本模式已启用'
        : '无障碍 Diff 文本模式已关闭'
    );
  },

  /**
   * Render diff as accessible text for screen readers.
   * Uses aria-live region to announce changes.
   */
  renderTextDiff(changes) {
    if (!App.accessibleDiffMode) return;

    const panel = document.getElementById('diff-text-mode');
    if (!panel) return;

    const lines = [];
    lines.push(`共 ${changes.length} 处变更`);

    for (const change of changes) {
      const typeMap = {
        create: '新建',
        modify: '修改',
        delete: '删除',
        rename: '重命名',
      };
      const typeText = typeMap[change.change_type] || change.change_type;
      lines.push(`${typeText}: ${change.path}`);
    }

    panel.textContent = lines.join('\n');
  },

  /** Navigate to next diff (F7). */
  next() {
    announce('下一个差异');
    // Implementation would cycle through diff hunks
  },

  /** Navigate to previous diff (Shift+F7). */
  prev() {
    announce('上一个差异');
    // Implementation would cycle through diff hunks
  },
};

// ============================================================================
// Replay -- Three-depth replay [doc-01 §8]
// ============================================================================
const Replay = {
  /** Set replay depth (recap/explore/verify). */
  setDepth(depth) {
    App.currentDepth = depth;

    // Update toggle buttons
    document.querySelectorAll('.btn-toggle[data-depth]').forEach((btn) => {
      const active = btn.dataset.depth === depth;
      btn.classList.toggle('active', active);
      btn.setAttribute('aria-pressed', String(active));
    });

    // Reload timeline with new depth (playhead/filter/evidence stay stable)
    if (App.currentTaskId) {
      this.loadTimeline(App.currentTaskId, depth);
    }

    announce(`回放深度: ${depth}`);
  },

  /** Load timeline entries for a task at the specified depth. */
  async loadTimeline(taskId, depth) {
    try {
      // In real implementation, this would call a replay command
      // For now, use task_detail events
      const detail = await IPC.invoke('task_detail', { task_id: taskId });
      const events = detail.events || [];

      const timeline = document.getElementById('timeline');
      if (!timeline) return;

      if (events.length === 0) {
        timeline.innerHTML = '<p class="placeholder">无事件记录</p>';
        return;
      }

      // Recap: show summary only
      if (depth === 'recap') {
        timeline.innerHTML = `
          <div class="timeline-entry recorded">
            <span class="entry-type">recap</span>
            <div class="entry-summary">共 ${events.length} 条事件</div>
            <span class="evidence-badge recorded">Recorded</span>
          </div>
        `;
        return;
      }

      // Explore/Verify: show all events
      timeline.innerHTML = events
        .map((event) => {
          const evidenceLevel = this.evidenceForEvent(event);
          return `
            <div class="timeline-entry ${evidenceLevel}">
              <span class="entry-type">${event.event_type || 'event'}</span>
              <div class="entry-summary">${this.summarizeEvent(event)}</div>
              <span class="evidence-badge ${evidenceLevel}">${this.evidenceLabel(evidenceLevel)}</span>
              ${depth === 'verify' ? `<details><summary>证据详情</summary><pre>${JSON.stringify(event, null, 2)}</pre></details>` : ''}
            </div>
          `;
        })
        .join('');
    } catch (err) {
      console.error('Failed to load timeline:', err);
    }
  },

  /** Determine evidence level for an event. */
  evidenceForEvent(event) {
    const type = event.event_type || '';
    if (type.includes('tool_result') || type.includes('verification')) return 'verified';
    if (type.includes('message') || type.includes('task_created')) return 'recorded';
    if (type.includes('system') || type.includes('state')) return 'observed';
    if (type.includes('inferred')) return 'inferred';
    return 'missing';
  },

  /** Get human-readable evidence label. */
  evidenceLabel(level) {
    const labels = {
      verified: 'Verified',
      recorded: 'Recorded',
      observed: 'Observed',
      inferred: 'Inferred',
      missing: 'Missing',
    };
    return labels[level] || 'Missing';
  },

  /** Summarize an event for display. */
  summarizeEvent(event) {
    const type = event.event_type || 'event';
    const summaries = {
      task_created: '任务已创建',
      state_changed: '状态已变更',
      run_started: 'Agent 运行已开始',
      run_ended: 'Agent 运行已结束',
      tool_call: '工具调用',
      tool_result: '工具结果',
      permission_requested: '权限请求',
      permission_decided: '权限已决定',
      file_changed: '文件已变更',
      verification_run: '验证已运行',
      system: '系统事件',
    };
    return summaries[type] || type;
  },
};

// ============================================================================
// Context Injection [doc-04 §7]
// ============================================================================
const Context = {
  /**
   * Inject file reference (@path).
   * Creates a file_ref block and inserts into message input.
   */
  injectFileRef(path) {
    const input = document.getElementById('message-input');
    if (!input) return;

    const ref = `@${path || 'path/to/file'}`;
    const pos = input.selectionStart || input.value.length;
    input.value =
      input.value.slice(0, pos) + ref + ' ' + input.value.slice(pos);
    input.focus();

    // Update preview
    this.updatePreview();
    announce(`文件引用已注入: ${ref}`);
  },

  /**
   * Inject selection reference (frozen snapshot block).
   */
  injectSelectionRef() {
    const input = document.getElementById('message-input');
    if (!input) return;

    // In real implementation, this would use the current editor selection
    const selRef = `[selection: ${App.activeFilePath || 'current file'}]`;
    const pos = input.selectionStart || input.value.length;
    input.value =
      input.value.slice(0, pos) + selRef + ' ' + input.value.slice(pos);
    input.focus();

    this.updatePreview();
    announce('选区引用已注入');
  },

  /**
   * External session injection -- bracketed paste, no trailing Enter.
   * Uses ESC[200~ ... ESC[201~ sequence, deliberately no \r at end.
   */
  injectExternalSession() {
    const input = document.getElementById('message-input');
    if (!input) return;

    // Bracketed paste markers (visible representation)
    const text = prompt('粘贴外部会话内容:');
    if (!text) return;

    // Wrap in bracketed paste markers -- no trailing Enter
    const injected = `\x1b[200~${text}\x1b[201~`;
    const pos = input.selectionStart || input.value.length;
    input.value =
      input.value.slice(0, pos) + injected + input.value.slice(pos);
    input.focus();

    this.updatePreview();
    announce('外部会话已注入 (bracketed paste, 无回车)');
  },

  /** Update context preview area. */
  updatePreview() {
    const input = document.getElementById('message-input');
    const preview = document.getElementById('context-preview');
    if (!input || !preview) return;

    const text = input.value;
    if (!text.trim()) {
      preview.textContent = '';
      return;
    }

    // Detect @path references
    const refs = text.match(/@[\w/.-]+/g) || [];
    const lines = [];
    if (refs.length > 0) {
      lines.push(`文件引用: ${refs.join(', ')}`);
    }
    if (text.includes('\x1b[200~')) {
      lines.push('包含外部会话注入 (bracketed paste)');
    }
    if (text.includes('[selection:')) {
      lines.push('包含选区引用');
    }

    preview.textContent = lines.join('\n') || '无上下文引用';
  },
};

// ============================================================================
// Views -- Each view has a load() function
// ============================================================================
const Views = {
  home: {
    async load() {
      // Load recent tasks
      try {
        const tasks = await IPC.invoke('task_list', {
          project_id: null,
          include_archived: false,
        });
        const container = document.getElementById('recent-tasks');
        if (!container) return;

        if (!tasks || tasks.length === 0) {
          container.innerHTML = '<p class="placeholder">暂无任务</p>';
          return;
        }

        container.innerHTML = tasks
          .map(
            (task) => `
          <div class="task-card" role="listitem" tabindex="0"
               data-task-id="${task.id}"
               onclick="Nav.openTaskRoom('${task.id}')"
               onkeydown="if(event.key==='Enter')Nav.openTaskRoom('${task.id}')">
            <strong>${escapeHtml(task.title)}</strong>
            <span class="task-state">${task.state}</span>
            <p>${escapeHtml(task.goal)}</p>
          </div>
        `
          )
          .join('');
      } catch (err) {
        console.error('Failed to load tasks:', err);
      }

      // Check for recovery data
      try {
        const recovery = await IPC.invoke('recovery_data');
        const section = document.getElementById('recovery-section');
        if (
          section &&
          recovery &&
          (recovery.interrupted_tasks.length > 0 ||
            recovery.orphaned_permissions > 0)
        ) {
          section.hidden = false;
          const list = document.getElementById('recovery-list');
          const items = [];
          if (recovery.interrupted_tasks.length > 0) {
            items.push(
              `<div>${recovery.interrupted_tasks.length} 个中断的任务</div>`
            );
          }
          if (recovery.orphaned_permissions > 0) {
            items.push(
              `<div>${recovery.orphaned_permissions} 个孤儿权限请求</div>`
            );
          }
          list.innerHTML = items.join('');
        }
      } catch (err) {
        console.error('Failed to load recovery data:', err);
      }
    },
  },

  room: {
    async load() {
      if (!App.currentTaskId) return;

      try {
        const detail = await IPC.invoke('task_detail', {
          task_id: App.currentTaskId,
        });

        // Update title
        const title = document.getElementById('room-title');
        if (title && detail.task) {
          title.textContent = detail.task.title || 'Task Room';
        }

        // Load timeline
        await Replay.loadTimeline(App.currentTaskId, App.currentDepth);

        // Render approvals
        this.renderApprovals(detail.permissions || []);

        // Render changes
        this.renderChanges(detail.changes || []);

        // Render verifications
        this.renderVerifications(detail.verifications || []);
      } catch (err) {
        console.error('Failed to load task detail:', err);
      }
    },

    renderApprovals(permissions) {
      const container = document.getElementById('approval-list');
      if (!container) return;

      const pending = permissions.filter((p) => p.decision === 'pending');
      if (pending.length === 0) {
        container.innerHTML = '<p class="placeholder">无待审批请求</p>';
        return;
      }

      container.innerHTML = pending
        .map(
          (p) => `
        <div class="approval-card" role="listitem">
          <div>
            <span class="risk-badge ${p.risk_level.toLowerCase()}">${p.risk_level}</span>
            <strong>${escapeHtml(p.tool_name)}</strong>
          </div>
          <p class="input-summary">${escapeHtml(p.input_summary)}</p>
          <div class="approval-actions" role="group">
            <button class="btn btn-primary btn-sm" onclick="Actions.approve('${p.id}', 'allow')" tabindex="0">允许</button>
            <button class="btn btn-secondary btn-sm" onclick="Actions.approve('${p.id}', 'allow_always')" tabindex="0">始终允许</button>
            <button class="btn btn-danger btn-sm" onclick="Actions.approve('${p.id}', 'deny')" tabindex="0">拒绝</button>
          </div>
        </div>
      `
        )
        .join('');
    },

    renderChanges(changes) {
      const container = document.getElementById('changes-list');
      if (!container) return;

      if (changes.length === 0) {
        container.innerHTML = '<p class="placeholder">无变更</p>';
        return;
      }

      container.innerHTML = changes
        .map(
          (c) => `
        <div class="change-item" role="listitem">
          <span class="change-type ${c.change_type}">${c.change_type}</span>
          <span>${escapeHtml(c.path)}</span>
        </div>
      `
        )
        .join('');

      // Update accessible diff text mode
      DiffMode.renderTextDiff(changes);
    },

    renderVerifications(verifications) {
      const container = document.getElementById('verification-list');
      if (!container) return;

      if (verifications.length === 0) {
        container.innerHTML = '<p class="placeholder">无验证记录</p>';
        return;
      }

      container.innerHTML = verifications
        .map(
          (v) => `
        <div class="verification-item ${v.status}" role="listitem">
          <strong>${escapeHtml(v.command)}</strong>
          <span class="verification-status">${v.status}</span>
        </div>
      `
        )
        .join('');
    },
  },

  editor: {
    async load() {
      // Load terminal list
      await this.loadTerminals();
      // Load file tree
      await this.loadFileTree();
    },

    async loadTerminals() {
      try {
        const terminals = await IPC.invoke('terminal_list');
        const container = document.getElementById('terminal-list');
        if (!container) return;

        if (!terminals || terminals.length === 0) {
          container.innerHTML = '<p class="placeholder">无终端</p>';
          return;
        }

        container.innerHTML = terminals
          .map(
            (t) => `
          <div class="terminal-item" role="listitem" tabindex="0"
               data-terminal-id="${t.id}"
               onclick="Actions.selectTerminal('${t.id}')">
            <span class="terminal-state ${t.state}" aria-hidden="true"></span>
            <span>${escapeHtml(t.id)}</span>
            <span class="terminal-shell">${escapeHtml(t.shell)}</span>
          </div>
        `
          )
          .join('');
      } catch (err) {
        console.error('Failed to load terminals:', err);
      }
    },

    async loadFileTree() {
      // In real implementation, this would call quick_open or a file listing command
      const container = document.getElementById('file-tree');
      if (container) {
        container.innerHTML = '<p class="placeholder">使用快速打开搜索文件</p>';
      }
    },
  },

  settings: {
    async load() {
      // Load settings
      try {
        const settings = await IPC.invoke('settings_get');
        const container = document.getElementById('settings-json');
        if (container && settings) {
          container.innerHTML = `<pre>${escapeHtml(JSON.stringify(settings, null, 2))}</pre>`;
        }
      } catch (err) {
        const container = document.getElementById('settings-json');
        if (container) {
          container.innerHTML = '<p class="placeholder">无法加载设置</p>';
        }
      }

      // Set zoom slider
      const slider = document.getElementById('zoom-slider');
      const value = document.getElementById('zoom-value');
      if (slider) slider.value = App.zoomLevel;
      if (value) value.textContent = App.zoomLevel + '%';
    },
  },
};

// ============================================================================
// Actions -- Button handlers
// ============================================================================
const Actions = {
  /** Approve a permission request. */
  async approve(requestId, decision) {
    try {
      await IPC.invoke('permission_approve', {
        request_id: requestId,
        decision,
      });
      announce(`权限已${decision === 'deny' ? '拒绝' : '批准'}`);
      Views.room.load();
    } catch (err) {
      announce('审批失败: ' + err.message);
    }
  },

  /** Create a new task. */
  async createTask() {
    const pathInput = document.getElementById('project-path');
    const projectId = pathInput ? pathInput.value : '/proj';
    if (!projectId) {
      announce('请先选择项目路径');
      return;
    }

    const goal = prompt('输入任务目标:');
    if (!goal) return;

    try {
      const task = await IPC.invoke('task_create', {
        project_id: projectId,
        title: goal.slice(0, 50),
        goal,
        mode: 'ask',
      });
      announce('任务已创建');
      Nav.openTaskRoom(task.id);
    } catch (err) {
      announce('创建任务失败: ' + err.message);
    }
  },

  /** Send message to agent. */
  async sendMessage() {
    const input = document.getElementById('message-input');
    if (!input || !input.value.trim()) return;
    if (!App.currentTaskId) {
      announce('无活动任务');
      return;
    }

    try {
      await IPC.invoke('agent_send', {
        task_id: App.currentTaskId,
        message: input.value,
      });
      input.value = '';
      Context.updatePreview();
      announce('消息已发送');
    } catch (err) {
      announce('发送失败: ' + err.message);
    }
  },

  /** Abort agent run. */
  async abort() {
    if (!App.currentTaskId) return;
    try {
      await IPC.invoke('agent_abort', { task_id: App.currentTaskId });
      announce('Agent 已中止');
      Views.room.load();
    } catch (err) {
      announce('中止失败: ' + err.message);
    }
  },

  /** Accept task changes. */
  async acceptTask() {
    if (!App.currentTaskId) return;
    try {
      await IPC.invoke('accept_task', { task_id: App.currentTaskId });
      announce('任务变更已接受');
      Views.room.load();
    } catch (err) {
      announce('接受失败: ' + err.message);
    }
  },

  /** Rollback task changes. */
  async rollbackTask() {
    if (!App.currentTaskId) return;
    try {
      await IPC.invoke('rollback_task', { task_id: App.currentTaskId });
      announce('任务已回滚');
      Views.room.load();
    } catch (err) {
      announce('回滚失败: ' + err.message);
    }
  },

  /** Run verification. */
  async runVerification() {
    const input = document.getElementById('verification-command');
    if (!input || !input.value.trim()) return;
    if (!App.currentTaskId) return;

    try {
      await IPC.invoke('run_verification', {
        task_id: App.currentTaskId,
        command: input.value,
      });
      announce('验证已运行');
      Views.room.load();
    } catch (err) {
      announce('验证失败: ' + err.message);
    }
  },

  /** Create terminal. */
  async createTerminal() {
    const shell = 'bash';
    const cwd = '.';
    try {
      const id = await IPC.invoke('terminal_create', { shell, cwd });
      announce('终端已创建');
      App.activeTerminalId = id;
      Views.editor.loadTerminals();
    } catch (err) {
      announce('创建终端失败: ' + err.message);
    }
  },

  /** Kill terminal. */
  async killTerminal() {
    if (!App.activeTerminalId) {
      announce('无活动终端');
      return;
    }
    try {
      await IPC.invoke('terminal_kill', { id: App.activeTerminalId });
      announce('终端已终止');
      App.activeTerminalId = null;
      Views.editor.loadTerminals();
    } catch (err) {
      announce('终止失败: ' + err.message);
    }
  },

  /** Send to terminal. */
  async sendToTerminal() {
    const input = document.getElementById('terminal-input');
    if (!input || !input.value) return;
    if (!App.activeTerminalId) {
      announce('无活动终端');
      return;
    }
    try {
      await IPC.invoke('terminal_send', {
        id: App.activeTerminalId,
        text: input.value,
        press_enter: true,
      });
      input.value = '';
      // Read output
      const output = await IPC.invoke('terminal_read', {
        id: App.activeTerminalId,
      });
      const outEl = document.getElementById('terminal-output');
      if (outEl) outEl.textContent = output;
    } catch (err) {
      announce('发送失败: ' + err.message);
    }
  },

  /** Select terminal. */
  selectTerminal(id) {
    App.activeTerminalId = id;
    announce(`已选择终端: ${id}`);
  },

  /** Quick open file. */
  async quickOpen() {
    const query = prompt('输入文件名:');
    if (!query) return;
    try {
      const files = await IPC.invoke('quick_open', { query, limit: 20 });
      const tree = document.getElementById('file-tree');
      if (tree && files) {
        tree.innerHTML = files
          .map(
            (f) => `
          <div class="file-tree-item" role="treeitem" tabindex="0"
               onclick="Actions.openFile('${escapeHtml(f)}')">
            ${escapeHtml(f)}
          </div>
        `
          )
          .join('');
      }
    } catch (err) {
      announce('搜索失败: ' + err.message);
    }
  },

  /** Global search. */
  async globalSearch() {
    const query = prompt('搜索内容:');
    if (!query) return;
    try {
      const matches = await IPC.invoke('global_search', { query, limit: 50 });
      const tree = document.getElementById('file-tree');
      if (tree && matches) {
        tree.innerHTML = matches
          .map(
            (m) => `
          <div class="file-tree-item" role="treeitem" tabindex="0"
               onclick="Actions.openFile('${escapeHtml(m.path)}')">
            ${escapeHtml(m.path)}:${m.line}
          </div>
        `
          )
          .join('');
      }
    } catch (err) {
      announce('搜索失败: ' + err.message);
    }
  },

  /** Open a file. */
  async openFile(path) {
    App.activeFilePath = path;
    const activeFile = document.getElementById('active-file');
    if (activeFile) activeFile.textContent = path;

    // In real implementation, would read file content
    const codeView = document.getElementById('code-view');
    if (codeView) {
      codeView.innerHTML = `<p class="placeholder">加载 ${escapeHtml(path)}...</p>`;
    }
    announce(`已打开: ${path}`);
  },
};

// ============================================================================
// Keyboard Navigation [doc-18 M11-05] [doc-16 CH-P10]
// ============================================================================
const Keyboard = {
  /** Initialize keyboard event listeners. */
  init() {
    document.addEventListener('keydown', (e) => this.handleKeydown(e));
  },

  /** Handle keyboard shortcuts. */
  handleKeydown(e) {
    // Zoom: Cmd/Ctrl + +/-/0
    if (e.metaKey || e.ctrlKey) {
      if (e.key === '=' || e.key === '+') {
        e.preventDefault();
        Zoom.in();
        return;
      }
      if (e.key === '-' || e.key === '_') {
        e.preventDefault();
        Zoom.out();
        return;
      }
      if (e.key === '0') {
        e.preventDefault();
        Zoom.reset();
        return;
      }
    }

    // F7: Accessible diff -- next
    if (e.key === 'F7' && !e.shiftKey) {
      e.preventDefault();
      if (!App.accessibleDiffMode) {
        DiffMode.toggle();
      }
      DiffMode.next();
      return;
    }

    // Shift+F7: Accessible diff -- previous
    if (e.key === 'F7' && e.shiftKey) {
      e.preventDefault();
      if (!App.accessibleDiffMode) {
        DiffMode.toggle();
      }
      DiffMode.prev();
      return;
    }

    // Navigation: Alt+1/2/3/4
    if (e.altKey) {
      if (e.key === '1') {
        e.preventDefault();
        Nav.switchTo('home');
        return;
      }
      if (e.key === '2') {
        e.preventDefault();
        Nav.switchTo('room');
        return;
      }
      if (e.key === '3') {
        e.preventDefault();
        Nav.switchTo('editor');
        return;
      }
      if (e.key === '4') {
        e.preventDefault();
        Nav.switchTo('settings');
        return;
      }
    }

    // Escape: close dialogs / go back
    if (e.key === 'Escape') {
      const activeView = document.querySelector('.view.active');
      if (activeView && activeView.id === 'view-room') {
        Nav.switchTo('home');
      }
    }
  },
};

// ============================================================================
// Utilities
// ============================================================================

/**
 * Announce a message via the aria-live region.
 */
function announce(message) {
  const announcer = document.getElementById('aria-announcer');
  if (announcer) {
    announcer.textContent = message;
  }
  console.log('[announce]', message);
}

/**
 * Escape HTML to prevent XSS.
 */
function escapeHtml(text) {
  if (text == null) return '';
  const div = document.createElement('div');
  div.textContent = String(text);
  return div.innerHTML;
}

// ============================================================================
// Initialization
// ============================================================================
function init() {
  // Navigation
  document.querySelectorAll('.nav-item[data-view]').forEach((btn) => {
    btn.addEventListener('click', () => Nav.switchTo(btn.dataset.view));
    btn.addEventListener('keydown', (e) => {
      if (e.key === 'Enter' || e.key === ' ') {
        e.preventDefault();
        Nav.switchTo(btn.dataset.view);
      }
    });
  });

  // Zoom controls
  document.querySelectorAll('.nav-zoom-in').forEach((b) =>
    b.addEventListener('click', () => Zoom.in())
  );
  document.querySelectorAll('.nav-zoom-out').forEach((b) =>
    b.addEventListener('click', () => Zoom.out())
  );
  document.querySelectorAll('.nav-zoom-reset').forEach((b) =>
    b.addEventListener('click', () => Zoom.reset())
  );

  const zoomSlider = document.getElementById('zoom-slider');
  if (zoomSlider) {
    zoomSlider.addEventListener('input', (e) =>
      Zoom.set(parseInt(e.target.value, 10))
    );
  }

  // Settings zoom buttons
  document
    .querySelectorAll('[data-action="zoom-in"]')
    .forEach((b) => b.addEventListener('click', () => Zoom.in()));
  document
    .querySelectorAll('[data-action="zoom-out"]')
    .forEach((b) => b.addEventListener('click', () => Zoom.out()));
  document
    .querySelectorAll('[data-action="zoom-reset"]')
    .forEach((b) => b.addEventListener('click', () => Zoom.reset()));

  // Replay depth toggles
  document.querySelectorAll('.btn-toggle[data-depth]').forEach((btn) => {
    btn.addEventListener('click', () => Replay.setDepth(btn.dataset.depth));
  });

  // Diff mode toggle
  document
    .querySelectorAll('[data-action="toggle-diff-mode"]')
    .forEach((b) => b.addEventListener('click', () => DiffMode.toggle()));

  const diffToggle = document.getElementById('accessible-diff-toggle');
  if (diffToggle) {
    diffToggle.addEventListener('change', () => DiffMode.toggle());
  }

  // Quick actions
  document
    .querySelectorAll('[data-action="new-task"]')
    .forEach((b) => b.addEventListener('click', () => Actions.createTask()));

  document
    .querySelectorAll('[data-action="open-workspace"]')
    .forEach((b) =>
      b.addEventListener('click', async () => {
        const path = prompt('输入工作区路径:');
        if (path) {
          try {
            await IPC.invoke('workspace_open', { path });
            announce('工作区已打开');
            Views.home.load();
          } catch (err) {
            announce('打开失败: ' + err.message);
          }
        }
      })
    );

  // Task room actions
  document
    .querySelectorAll('[data-action="send-message"]')
    .forEach((b) => b.addEventListener('click', () => Actions.sendMessage()));
  document
    .querySelectorAll('[data-action="abort"]')
    .forEach((b) => b.addEventListener('click', () => Actions.abort()));
  document
    .querySelectorAll('[data-action="accept"]')
    .forEach((b) => b.addEventListener('click', () => Actions.acceptTask()));
  document
    .querySelectorAll('[data-action="rollback"]')
    .forEach((b) => b.addEventListener('click', () => Actions.rollbackTask()));
  document
    .querySelectorAll('[data-action="rollback-task"]')
    .forEach((b) => b.addEventListener('click', () => Actions.rollbackTask()));
  document
    .querySelectorAll('[data-action="accept-task"]')
    .forEach((b) => b.addEventListener('click', () => Actions.acceptTask()));
  document
    .querySelectorAll('[data-action="run-verification"]')
    .forEach((b) => b.addEventListener('click', () => Actions.runVerification()));
  document
    .querySelectorAll('[data-action="back-to-home"]')
    .forEach((b) => b.addEventListener('click', () => Nav.switchTo('home')));

  // Context injection
  document
    .querySelectorAll('[data-action="inject-file-ref"]')
    .forEach((b) => b.addEventListener('click', () => Context.injectFileRef()));
  document
    .querySelectorAll('[data-action="inject-selection-ref"]')
    .forEach((b) => b.addEventListener('click', () => Context.injectSelectionRef()));
  document
    .querySelectorAll('[data-action="inject-external-session"]')
    .forEach((b) =>
      b.addEventListener('click', () => Context.injectExternalSession())
    );

  // Message input -- update preview on change
  const messageInput = document.getElementById('message-input');
  if (messageInput) {
    messageInput.addEventListener('input', () => Context.updatePreview());
    // Ctrl+Enter to send
    messageInput.addEventListener('keydown', (e) => {
      if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
        e.preventDefault();
        Actions.sendMessage();
      }
    });
  }

  // Terminal actions
  document
    .querySelectorAll('[data-action="terminal-create"]')
    .forEach((b) => b.addEventListener('click', () => Actions.createTerminal()));
  document
    .querySelectorAll('[data-action="terminal-kill"]')
    .forEach((b) => b.addEventListener('click', () => Actions.killTerminal()));
  document
    .querySelectorAll('[data-action="terminal-send"]')
    .forEach((b) => b.addEventListener('click', () => Actions.sendToTerminal()));

  // Editor actions
  document
    .querySelectorAll('[data-action="quick-open"]')
    .forEach((b) => b.addEventListener('click', () => Actions.quickOpen()));
  document
    .querySelectorAll('[data-action="global-search"]')
    .forEach((b) => b.addEventListener('click', () => Actions.globalSearch()));

  // Initialize keyboard navigation
  Keyboard.init();

  // Load initial view
  Views.home.load();

  console.log('R-Code frontend initialized');
}

// Boot
if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', init);
} else {
  init();
}
