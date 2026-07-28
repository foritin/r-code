(() => {
  'use strict';

  const params = new URLSearchParams(window.location.search);
  const captureState = params.get('state');
  const captureTheme = params.get('theme');
  const allowedKinds = ['launcher', 'run', 'terminal', 'files', 'review'];
  const initialKind = allowedKinds.includes(captureState) ? captureState : 'launcher';
  const initialMode = captureState === 'review-collapsed' ? 'collapsed' : 'docked';
  const savedTheme = localStorage.getItem('r-code-demo-theme');
  const initialTheme = captureTheme === 'dark' || captureTheme === 'light'
    ? captureTheme
    : savedTheme === 'dark' ? 'dark' : 'light';
  const isMac = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);
  const modLabel = isMac ? '⌘' : 'Ctrl';

  const paths = {
    PanelLeft: '<rect x="3" y="3" width="18" height="18" rx="2"/><path d="M9 3v18"/>',
    PanelRight: '<rect x="3" y="3" width="18" height="18" rx="2"/><path d="M15 3v18"/>',
    ArrowLeft: '<path d="m15 18-6-6 6-6"/><path d="M9 12h10"/>',
    ArrowRight: '<path d="m9 18 6-6-6-6"/><path d="M5 12h10"/>',
    Search: '<circle cx="11" cy="11" r="7"/><path d="m20 20-3.5-3.5"/>',
    Plus: '<path d="M12 5v14M5 12h14"/>',
    Minus: '<path d="M5 12h14"/>',
    Square: '<rect x="6" y="6" width="12" height="12" rx="1"/>',
    X: '<path d="m6 6 12 12M18 6 6 18"/>',
    Message: '<path d="M21 15a4 4 0 0 1-4 4H8l-5 3V7a4 4 0 0 1 4-4h10a4 4 0 0 1 4 4z"/>',
    Inbox: '<path d="M4 5h16l2 9h-5l-2 3H9l-2-3H2z"/><path d="M4 5 2 14"/>',
    Activity: '<path d="M3 12h4l2-7 4 14 2-7h6"/>',
    Folder: '<path d="M3 6h6l2 2h10v11H3z"/>',
    FolderOpen: '<path d="M3 7h6l2 2h10l-2 10H3z"/>',
    File: '<path d="M6 2h8l4 4v16H6z"/><path d="M14 2v5h5"/>',
    Settings: '<circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.9l.1.1-2.8 2.8-.1-.1A1.7 1.7 0 0 0 15 19.4a1.7 1.7 0 0 0-1 .6l-.1.2h-4l-.1-.2a1.7 1.7 0 0 0-1-.6 1.7 1.7 0 0 0-1.9.3l-.1.1L4 17l.1-.1A1.7 1.7 0 0 0 4.4 15a1.7 1.7 0 0 0-.6-1l-.2-.1v-4l.2-.1a1.7 1.7 0 0 0 .6-1A1.7 1.7 0 0 0 4.1 7L4 6.9 6.8 4l.1.1a1.7 1.7 0 0 0 1.9.3 1.7 1.7 0 0 0 1-.6l.1-.2h4l.1.2a1.7 1.7 0 0 0 1 .6 1.7 1.7 0 0 0 1.9-.3l.1-.1L20 6.9l-.1.1a1.7 1.7 0 0 0-.3 1.9 1.7 1.7 0 0 0 .6 1l.2.1v4l-.2.1a1.7 1.7 0 0 0-.8.9z"/>',
    Help: '<circle cx="12" cy="12" r="9"/><path d="M9.5 9a2.7 2.7 0 1 1 4.7 1.8c-1 .8-2.2 1.2-2.2 2.7"/><path d="M12 17h.01"/>',
    ChevronDown: '<path d="m7 10 5 5 5-5"/>',
    ChevronRight: '<path d="m10 7 5 5-5 5"/>',
    More: '<circle cx="5" cy="12" r="1"/><circle cx="12" cy="12" r="1"/><circle cx="19" cy="12" r="1"/>',
    Users: '<path d="M16 21v-2a4 4 0 0 0-4-4H6a4 4 0 0 0-4 4v2"/><circle cx="9" cy="7" r="4"/><path d="M22 21v-2a4 4 0 0 0-3-3.9M16 3.1a4 4 0 0 1 0 7.8"/>',
    Terminal: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="m7 9 3 3-3 3M13 15h4"/>',
    GitDiff: '<circle cx="6" cy="5" r="2"/><circle cx="6" cy="19" r="2"/><path d="M6 7v10M13 6h3a2 2 0 0 1 2 2v9M15 14l3 3 3-3"/>',
    Shield: '<path d="M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z"/><path d="m9 12 2 2 4-4"/>',
    Check: '<path d="m5 12 4 4L19 6"/>',
    Circle: '<circle cx="12" cy="12" r="9"/>',
    Loader: '<path d="M21 12a9 9 0 1 1-6.2-8.6"/>',
    Stop: '<rect x="7" y="7" width="10" height="10" rx="1"/>',
    Expand: '<path d="M8 3H3v5M16 3h5v5M8 21H3v-5M16 21h5v-5"/>',
    External: '<path d="M14 3h7v7M10 14 21 3"/><path d="M21 14v6a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1h6"/>',
    Copy: '<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V5a1 1 0 0 0-1-1H5a1 1 0 0 0-1 1v10a1 1 0 0 0 1 1h3"/>',
    Filter: '<path d="M4 5h16M7 12h10M10 19h4"/>',
    Send: '<path d="m22 2-7 20-4-9-9-4z"/><path d="M22 2 11 13"/>',
    Sun: '<circle cx="12" cy="12" r="4"/><path d="M12 2v2M12 20v2M4.93 4.93l1.42 1.42M17.66 17.66l1.41 1.41M2 12h2M20 12h2M4.93 19.07l1.42-1.42M17.66 6.34l1.41-1.41"/>',
    Moon: '<path d="M21 12.8A9 9 0 1 1 11.2 3 7 7 0 0 0 21 12.8z"/>',
    Maximize: '<path d="M4 14v6h6M20 10V4h-6M14 20h6v-6M10 4H4v6"/>',
    Trash: '<path d="M3 6h18M8 6V4h8v2M6 6l1 15h10l1-15M10 11v6M14 11v6"/>',
    Refresh: '<path d="M20 6v5h-5"/><path d="M4 18v-5h5"/><path d="M6.1 8A7 7 0 0 1 18.7 7L20 11M4 13l1.3 4A7 7 0 0 0 17.9 16"/>'
  };

  const icon = (name, size = 16, className = '') => `
    <svg class="${className}" width="${size}" height="${size}" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      ${paths[name] || paths.Circle}
    </svg>`;
  const escapeHtml = (value) => String(value).replace(/[&<>'"]/g, (char) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', "'": '&#39;', '"': '&quot;' })[char]);
  const statusDot = (kind = '') => `<span class="status-dot ${kind ? `is-${kind}` : ''}" aria-hidden="true"></span>`;

  const tasks = {
    workbench: { title: '设计右侧工作台', project: 'r-code', time: '刚刚', running: true },
    api: { title: '梳理 API 风险', project: 'CryptoPlatform', time: '1d', running: false },
    agents: { title: '调用子代理查看项目', project: 'CryptoPlatform', time: '1d', running: false }
  };
  const makeWorkbench = (kind = 'launcher', mode = 'closed') => ({ kind, lastKind: kind === 'launcher' ? 'run' : kind, mode, previousKind: 'run' });

  const state = {
    theme: initialTheme,
    activeTask: 'workbench',
    workbenches: {
      workbench: makeWorkbench(initialMode === 'collapsed' ? 'review' : initialKind, initialMode),
      api: makeWorkbench('review', 'closed'),
      agents: makeWorkbench('run', 'closed')
    },
    focus: false,
    launcherIndex: 0,
    agents: [
      { id: 'visual', title: '视觉审查', summary: '正在核对工作台比例与主题令牌', status: 'live', elapsed: '01:37', goal: '检查工作台如何适配 R-Code 的双主题与窄屏布局' },
      { id: 'flow', title: '交互结构', summary: '梳理打开、切换与恢复规则', status: 'live', elapsed: '01:12', goal: '验证单一工作台的状态流转与键盘操作' },
      { id: 'impl', title: '实现盘点', summary: '已确认可复用的终端与 diff', status: 'success', elapsed: '42s', goal: '盘点现有实现并给出最小迁移边界' }
    ],
    selectedAgent: 'visual',
    terminal: {
      session: 1,
      alive: true,
      lines: [
        ['muted', 'PowerShell 7.6.4'],
        ['prompt', 'PS D:\\project\\rust\\r-code> npm run build'],
        ['', '> r-code-frontend@0.1.0 build\n> tsc && vite build'],
        ['ok', '✓ 107 modules transformed.\n✓ built in 1.56s'],
        ['prompt', 'PS D:\\project\\rust\\r-code> cargo check -p r-code-host'],
        ['', '    Checking r-code-host v0.1.0'],
        ['ok', '    Finished dev profile in 5.84s']
      ]
    },
    fileQuery: '',
    selectedFile: 'Workbench.tsx',
    reviewFile: 'Composer.tsx',
    reviewStatus: 'pending',
    composer: '',
    modal: null,
    toast: ''
  };

  const cloneTerminal = (label) => ({ session: 1, alive: true, lines: [['muted', `PowerShell 7.6.4 · ${label}`]] });
  state.taskPayloads = {
    workbench: {
      terminal: state.terminal,
      selectedFile: state.selectedFile,
      reviewFile: state.reviewFile,
      reviewStatus: state.reviewStatus,
      fileQuery: state.fileQuery,
      selectedAgent: state.selectedAgent
    },
    api: {
      terminal: cloneTerminal('CryptoPlatform'),
      selectedFile: 'commands.rs',
      reviewFile: 'commands.rs',
      reviewStatus: 'pending',
      fileQuery: '',
      selectedAgent: 'impl'
    },
    agents: {
      terminal: cloneTerminal('CryptoPlatform'),
      selectedFile: 'Composer.tsx',
      reviewFile: 'Composer.tsx',
      reviewStatus: 'pending',
      fileQuery: '',
      selectedAgent: 'flow'
    }
  };

  const switchTask = (nextTask) => {
    if (!tasks[nextTask] || nextTask === state.activeTask) return;
    const current = state.taskPayloads[state.activeTask];
    current.terminal = state.terminal;
    current.selectedFile = state.selectedFile;
    current.reviewFile = state.reviewFile;
    current.reviewStatus = state.reviewStatus;
    current.fileQuery = state.fileQuery;
    current.selectedAgent = state.selectedAgent;
    const next = state.taskPayloads[nextTask];
    state.activeTask = nextTask;
    state.terminal = next.terminal;
    state.selectedFile = next.selectedFile;
    state.reviewFile = next.reviewFile;
    state.reviewStatus = next.reviewStatus;
    state.fileQuery = next.fileQuery;
    state.selectedAgent = next.selectedAgent;
    state.focus = false;
  };

  const files = {
    'Workbench.tsx': [
      ['31', '<span class="syntax-keyword">type</span> WorkbenchKind = <span class="syntax-string">"terminal"</span> | <span class="syntax-string">"files"</span> | <span class="syntax-string">"review"</span>;'],
      ['32', ''], ['33', '<span class="syntax-keyword">type</span> <span class="syntax-type">WorkbenchState</span> = {'],
      ['34', '  ownerKey: <span class="syntax-string">`task:${string}`</span>;'],
      ['35', '  kind: <span class="syntax-type">WorkbenchKind</span> | <span class="syntax-keyword">null</span>;'],
      ['36', '  mode: <span class="syntax-string">"closed"</span> | <span class="syntax-string">"docked"</span> | <span class="syntax-string">"focus"</span>;'],
      ['37', '  entityId?: <span class="syntax-keyword">string</span>;'], ['38', '};'], ['39', ''],
      ['40', '<span class="syntax-keyword">export const</span> <span class="syntax-accent">openWorkbench</span> = (kind: <span class="syntax-type">WorkbenchKind</span>) =&gt; {'],
      ['41', '  set((state) =&gt; ({'], ['42', '    workbench: { ...state.workbench, kind, mode: <span class="syntax-string">"docked"</span> }'],
      ['43', '  }));'], ['44', '};']
    ],
    'Composer.tsx': [['18', '<span class="syntax-keyword">export function</span> <span class="syntax-accent">Composer</span>() {'], ['19', '  <span class="syntax-keyword">const</span> [message, setMessage] = useState(<span class="syntax-string">""</span>);'], ['20', '  <span class="syntax-keyword">return</span> &lt;MessageInput value={message} /&gt;;'], ['21', '}']],
    'Timeline.tsx': [['12', '<span class="syntax-keyword">export const</span> timeline = events.map(formatEvent);'], ['13', '<span class="syntax-keyword">export default</span> timeline;']],
    'commands.rs': [['88', '<span class="syntax-keyword">pub async fn</span> open_workbench(kind: WorkbenchKind) {'], ['89', '    state.show(kind).await;'], ['90', '}']]
  };
  const reviewFiles = [
    { name: 'Composer.tsx', add: 409, remove: 13 },
    { name: 'Workbench.tsx', add: 186, remove: 0 },
    { name: 'commands.rs', add: 121, remove: 9 },
    { name: 'room.css', add: 88, remove: 34 }
  ];

  const currentWorkbench = () => state.workbenches[state.activeTask];
  const setTheme = (theme, persist = true) => {
    state.theme = theme;
    document.documentElement.dataset.theme = theme;
    document.querySelector('meta[name="theme-color"]').content = theme === 'dark' ? '#151311' : '#fbfaf7';
    if (persist) localStorage.setItem('r-code-demo-theme', theme);
  };
  setTheme(state.theme, false);

  const desktopBar = () => `
    <header class="desktop-bar">
      <div class="desktop-menus">
        <div class="desktop-menu-cluster"><span class="chrome-icon">${icon('PanelLeft', 18)}</span><span class="chrome-icon">${icon('ArrowLeft', 18)}</span><span class="chrome-icon">${icon('ArrowRight', 18)}</span></div>
        <span class="desktop-menu-label">文件</span><span class="desktop-menu-label">编辑</span><span class="desktop-menu-label">视图</span><span class="desktop-menu-label">帮助</span>
      </div>
      <div class="window-controls"><span class="window-control">${icon('Minus', 15)}</span><span class="window-control">${icon('Square', 13)}</span><span class="window-control is-close">${icon('X', 16)}</span></div>
    </header>`;

  const taskButton = (id) => {
    const task = tasks[id];
    return `<button type="button" class="project-task ${state.activeTask === id ? 'is-current' : ''} ${task.running ? 'is-running' : ''}" data-action="select-task" data-task="${id}" aria-current="${state.activeTask === id ? 'page' : 'false'}">
      <span class="project-task-dot"></span><span>${task.title}</span><time>${task.time}</time>
    </button>`;
  };

  const sidebar = () => `
    <aside class="sidebar">
      <div class="sidebar-brand"><span class="brand-mark">R</span><span class="brand-name">R-Code</span><button type="button" class="icon-button" aria-label="搜索">${icon('Search', 17)}</button></div>
      <button type="button" class="new-task" data-action="new-task">${icon('Plus', 17)}<span>新对话</span></button>
      <nav class="sidebar-nav" aria-label="全局导航">
        <button type="button" class="nav-item is-current" aria-current="page">${icon('Message', 17)}<span>对话</span></button>
        <button type="button" class="nav-item" data-action="notice" data-message="待处理视图不在本轮工作台原型范围内">${icon('Inbox', 17)}<span>待处理</span><span class="sidebar-count">3</span></button>
        <button type="button" class="nav-item" data-action="notice" data-message="活动会在项目内按任务呈现">${icon('Activity', 17)}<span>活动</span></button>
        <button type="button" class="nav-item" data-action="open-tool" data-kind="files">${icon('Folder', 17)}<span>项目文件</span></button>
      </nav>
      <div class="project-list">
        <div class="project-list-title"><span>项目</span><span>${state.agents.filter((agent) => agent.status === 'live').length} 运行中</span></div>
        <section class="project-group"><div class="project-head">${icon('Folder', 16)}<span>CryptoPlatform</span>${icon('ChevronRight', 14)}</div>${taskButton('agents')}${taskButton('api')}</section>
        <section class="project-group"><div class="project-head">${icon('FolderOpen', 16)}<span>r-code</span><span class="project-live"></span></div>${taskButton('workbench')}</section>
      </div>
      <div class="sidebar-foot">
        <button type="button" data-action="notice" data-message="设置页将在产品实现中承载完整外观偏好">${icon('Settings', 17)}<span>设置</span></button>
        <button type="button" data-action="toggle-theme" aria-label="切换为${state.theme === 'light' ? '暗色' : '亮色'}主题">${icon(state.theme === 'light' ? 'Moon' : 'Sun', 17)}<span>${state.theme === 'light' ? '切换暗色' : '切换亮色'}</span></button>
      </div>
    </aside>`;

  const conversationContent = () => {
    const task = tasks[state.activeTask];
    if (state.activeTask !== 'workbench') return `
      <div class="conversation-copy">
        <div class="user-message">${state.activeTask === 'api' ? '梳理一下当前 API 的边界风险。' : '调用子代理检查这个项目。'}</div>
        <article class="assistant-block"><div class="assistant-kicker"><span>R-CODE</span><span>任务记录</span></div><p>这是另一个任务的独立上下文。右侧工作台不会继承「设计右侧工作台」中的文件、审核或终端状态。</p></article>
      </div>`;
    return `
      <div class="conversation-copy">
        <div class="user-message">看看最新设计的 demo，右边工作台参照 Codex 的模式；保留最终一套，并补齐亮色与暗色。</div>
        <article class="assistant-block">
          <div class="assistant-kicker"><span>R-CODE</span><span>刚刚更新</span></div>
          <p>右侧已经从固定检查器改成按需打开的任务工作台。主对话保持稳定，终端、文件、审核和子代理在同一个位置切换，不再跳到另一套页面。</p>
          <div class="run-summary">
            <button type="button" class="run-summary-row" data-action="open-tool" data-kind="run">${statusDot('success')}<span class="run-summary-copy"><strong>完成结构审计</strong><small>移除重复路由，确定单一工作台槽位</small></span><span class="run-summary-meta">18s</span></button>
            <button type="button" class="run-summary-row" data-action="open-tool" data-kind="run">${statusDot('live')}<span class="run-summary-copy"><strong>${state.agents.filter((agent) => agent.status === 'live').length} 个子代理正在协作</strong><small>视觉规格 · 交互状态 · 现有实现盘点</small></span><span class="run-summary-meta">02:14</span></button>
          </div>
          <button type="button" class="change-strip" data-action="open-tool" data-kind="review"><span class="change-icon">${icon('GitDiff', 15)}</span><span class="change-copy"><strong>准备了 5 个打开状态 + 1 个收起状态</strong><small><span class="add-text">+2,053</span> <span class="remove-text">−146</span></small></span>${icon('ChevronRight', 15)}</button>
        </article>
      </div>`;
  };

  const conversation = () => {
    const task = tasks[state.activeTask];
    const wb = currentWorkbench();
    return `<section class="conversation">
      <header class="conversation-head">${icon('Folder', 16)}<div class="conversation-title"><strong>${task.title}</strong><span>${task.project} · 主代理</span></div><span class="scope-badge">完全访问权限</span><button type="button" class="icon-button workbench-reopen" data-action="open-last" aria-label="${wb.mode === 'closed' ? '打开工作台' : '工作台已打开'}" aria-expanded="${wb.mode !== 'closed'}">${icon('PanelRight', 17)}</button><button type="button" class="icon-button" data-action="notice" data-message="任务菜单">${icon('More', 17)}</button></header>
      <div class="conversation-stream">${conversationContent()}</div>
      <div class="composer-wrap"><div class="composer"><textarea id="composer-input" aria-label="消息" placeholder="继续调整，或描述下一步…">${escapeHtml(state.composer)}</textarea><div class="composer-foot"><button type="button" class="composer-tool" aria-label="添加上下文">${icon('Plus', 17)}</button><span class="composer-access">${icon('Shield', 14)} 完全访问</span><span class="composer-model">GPT-5.6 Sol · 最高</span><button type="button" class="composer-tool" aria-label="语音输入">${icon('Activity', 16)}</button><button type="button" class="send-button" data-action="send-message" aria-label="发送消息">${icon('Send', 16)}</button></div></div></div>
    </section>`;
  };

  const toolMeta = {
    launcher: ['PanelRight', '工作台'], run: ['Users', '运行'], terminal: ['Terminal', '终端'], files: ['File', '文件'], review: ['Shield', '审核']
  };
  const workbenchHead = (kind) => {
    const [glyph, label] = toolMeta[kind] || toolMeta.launcher;
    return `<header class="workbench-head"><div class="workbench-tab is-active" role="tab" aria-selected="true">${icon(glyph, 15)}<strong>${label}</strong><button type="button" class="tab-close" data-testid="workbench-close" data-action="close-tab" aria-label="关闭${label}">${icon('X', 13)}</button></div><button type="button" class="tab-add icon-button" data-action="show-launcher" aria-label="打开其他工具">${icon('Plus', 16)}</button><span class="workbench-head-spacer"></span><button type="button" class="icon-button" data-action="toggle-focus" aria-label="${state.focus ? '退出专注' : '专注工作台'}">${icon(state.focus ? 'ArrowLeft' : 'Expand', 15)}</button><button type="button" class="icon-button" data-action="hide-workbench" aria-label="隐藏工作台">${icon('PanelRight', 16)}</button></header>`;
  };

  const launcherItems = [
    ['run', 'Users', '运行与子代理', `${modLabel}+Shift+R`], ['review', 'Shield', '审核', `${modLabel}+Shift+G`], ['terminal', 'Terminal', '终端', `${modLabel}+\``], ['files', 'File', '文件', `${modLabel}+P`]
  ];
  const launcher = () => `<section class="launcher" role="dialog" data-testid="launcher-dialog" aria-label="工作台启动器"><div class="launcher-intro"><span>任务工具</span><strong>在一个位置继续工作</strong><p>工具之间切换不会丢失当前任务状态。</p></div><div class="launcher-dock" role="listbox" aria-label="选择工具">${launcherItems.map(([kind, glyph, label, shortcut], index) => `<button type="button" class="launcher-row ${index === state.launcherIndex ? 'is-keyboard' : ''}" data-action="open-tool" data-kind="${kind}" role="option" aria-selected="${index === state.launcherIndex}">${icon(glyph, 16)}<strong>${label}</strong><span class="shortcut">${shortcut}</span></button>`).join('')}</div></section>`;

  const agentTimeline = (agent) => {
    const running = agent.status === 'live';
    return `<div class="agent-timeline">
      <div class="timeline-row is-done">${icon('Check', 15)}<div class="timeline-copy"><strong>读取参考与当前实现</strong><small>公开操作记录已同步</small></div><time>12s</time></div>
      <div class="timeline-row is-done">${icon('Check', 15)}<div class="timeline-copy"><strong>提取结构与视觉约束</strong><small>工作台宽度、层级和状态规则</small></div><time>9s</time></div>
      <div class="timeline-row ${running ? 'is-current' : 'is-done'}">${icon(running ? 'Loader' : 'Check', 15)}<div class="timeline-copy"><strong>${running ? '核对当前任务' : '输出结论'}</strong><small>${agent.summary}</small></div><time>${running ? '进行中' : '完成'}</time></div>
      <div class="timeline-row ${running ? '' : 'is-done'}">${icon(running ? 'Circle' : 'Check', 14)}<div class="timeline-copy"><strong>汇总到主对话</strong><small>${running ? '完成后自动保留结果' : '已保留可检查的结果'}</small></div><time>${running ? '—' : '1s'}</time></div>
    </div>`;
  };
  const runWorkbench = () => {
    const selected = state.agents.find((agent) => agent.id === state.selectedAgent) || state.agents[0];
    const liveCount = state.agents.filter((agent) => agent.status === 'live').length;
    return `<section class="tool-surface"><header class="tool-context"><span class="tool-pill ${liveCount ? 'is-live' : 'is-success'}">${statusDot(liveCount ? 'live' : 'success')} ${liveCount ? '运行中' : '已完成'}</span><strong>3 个子代理</strong><span class="tool-context-spacer"></span><small>主运行 02:14</small><button type="button" class="icon-button" data-action="notice" data-message="运行菜单">${icon('More', 16)}</button></header>
      <div class="run-workbench"><aside class="agent-list"><div class="section-caption"><span>子代理</span><span>${liveCount} 运行 · ${3 - liveCount} 完成</span></div>${state.agents.map((agent) => `<button type="button" class="agent-row ${agent.id === selected.id ? 'is-selected' : ''}" data-action="select-agent" data-agent="${agent.id}" aria-pressed="${agent.id === selected.id}">${statusDot(agent.status)}<span class="agent-row-copy"><strong>${agent.title}</strong><small>${agent.summary}</small></span></button>`).join('')}</aside>
      <article class="agent-detail"><header class="agent-detail-head"><div class="agent-state-line">${statusDot(selected.status)}<span>${selected.status === 'live' ? '工作中' : selected.status === 'success' ? '已完成' : '已停止'}</span><span class="mono">${selected.elapsed}</span></div><h2>${selected.goal}</h2><p>这里只展示公开操作、进度与结果；不显示私有推理或内部事件协议。</p></header>${agentTimeline(selected)}<footer class="agent-detail-foot"><span>结果会保留在当前任务中</span>${selected.status === 'live' ? `<button type="button" class="danger-button" data-action="stop-agent" data-agent="${selected.id}">${icon('Stop', 13)} 停止子代理</button>` : '<span class="completion-label">已归档</span>'}</footer></article></div></section>`;
  };

  const terminalWorkbench = () => {
    const terminalText = state.terminal.lines.map(([tone, value]) => `<span class="${tone}">${escapeHtml(value)}</span>`).join('\n\n');
    return `<section class="tool-surface"><header class="tool-context"><span class="tool-pill ${state.terminal.alive ? 'is-success' : ''}">${icon(state.terminal.alive ? 'Check' : 'Circle', 13)} ${state.terminal.alive ? '已连接' : '已结束'}</span><strong>PowerShell ${state.terminal.session}</strong><span class="mono">D:\\project\\rust\\r-code</span><span class="tool-context-spacer"></span><button type="button" class="icon-button" data-action="new-terminal" aria-label="新建终端">${icon('Plus', 15)}</button><button type="button" class="icon-button" data-action="end-terminal" aria-label="结束终端">${icon('Trash', 15)}</button></header>
      <div class="terminal-workbench" role="log" aria-label="终端输出"><div class="terminal-output">${terminalText}</div><label class="terminal-prompt"><span>PS D:\\project\\rust\\r-code&gt;</span><input id="terminal-input" autocomplete="off" spellcheck="false" aria-label="终端命令" ${state.terminal.alive ? '' : 'disabled'}><i class="cursor"></i></label></div></section>`;
  };

  const visibleFileNames = () => Object.keys(files).filter((name) => name.toLowerCase().includes(state.fileQuery.toLowerCase()));
  const fileTree = (review = false) => {
    const selected = review ? state.reviewFile : state.selectedFile;
    const names = review ? reviewFiles.map((file) => file.name).filter((name) => name !== 'room.css') : visibleFileNames();
    return `<aside class="${review ? 'review-tree' : 'file-tree'}"><label class="tree-search">${icon('Search', 14)}<input ${review ? 'disabled' : ''} data-role="file-filter" value="${escapeHtml(state.fileQuery)}" placeholder="筛选文件…" aria-label="筛选文件"></label><div class="tree-root">${icon('ChevronDown', 14)}${icon('FolderOpen', 14)}<span>r-code</span></div><div class="tree-row is-folder" style="--depth:0">${icon('ChevronDown', 13)}${icon('FolderOpen', 14)}<span>src-tauri / frontend / src</span><span class="tree-change">M</span></div>${names.map((name, index) => `<button type="button" class="tree-row ${name === selected ? 'is-selected' : ''}" style="--depth:${index < 3 ? 2 : 1}" data-action="select-file" data-file="${name}" data-review="${review}"><span></span>${icon('File', 14)}<span>${name}</span><span class="tree-change ${name === 'Workbench.tsx' ? 'is-new' : ''}">${name === 'Workbench.tsx' ? '+' : 'M'}</span></button>`).join('') || '<div class="tree-empty">没有匹配文件</div>'}</aside>`;
  };
  const codeRows = (name) => (files[name] || files['Workbench.tsx']).map(([number, code]) => `<div class="code-row"><span class="ln">${number}</span><code>${code}</code></div>`).join('');
  const filesWorkbench = () => `<section class="tool-surface"><header class="tool-context"><strong>${state.selectedFile}</strong><span class="mono">src-tauri/frontend/src/components</span><span class="tool-context-spacer"></span><button type="button" class="icon-button" data-action="notice" data-message="已在独立编辑器中打开">${icon('External', 14)}</button><button type="button" class="icon-button" data-action="copy-file" aria-label="复制文件内容">${icon('Copy', 14)}</button></header><div class="files-workbench"><article class="file-editor"><header class="file-editor-title">${icon('File', 14)}<span>${state.selectedFile}</span><span class="file-modified"></span></header><div class="code-sheet">${codeRows(state.selectedFile)}</div></article>${fileTree(false)}</div></section>`;

  const diffRows = (name) => `<div class="diff-fold">${icon('ChevronDown', 13)}<span>${name === 'Composer.tsx' ? '28' : '16'} unmodified lines</span></div><div class="diff-row"><span class="ln">31</span><span class="ln">31</span><code>  onChanged: () =&gt; void;</code></div><div class="diff-row is-add"><span class="ln"></span><span class="ln">32</span><code>+ openRequest?: number;</code></div><div class="diff-row"><span class="ln">32</span><span class="ln">33</span><code>}</code></div><div class="diff-fold">${icon('ChevronDown', 13)}<span>16 unmodified lines</span></div><div class="diff-row"><span class="ln">48</span><span class="ln">49</span><code>  variant = "bar",</code></div><div class="diff-row is-add"><span class="ln"></span><span class="ln">50</span><code>+ openRequest,</code></div><div class="diff-row is-remove"><span class="ln">109</span><span class="ln"></span><code>- menuClassName="model-menu old"</code></div><div class="diff-row is-add"><span class="ln"></span><span class="ln">112</span><code>+ menuClassName="model-menu"</code></div>`;
  const reviewWorkbench = () => {
    const resolved = state.reviewStatus !== 'pending';
    return `<section class="tool-surface"><header class="tool-context"><strong>本轮变更</strong><span class="add-text">+2,053</span><span class="remove-text">−146</span><span class="tool-context-spacer"></span><span class="tool-pill is-success">${icon('Check', 13)} ${resolved ? (state.reviewStatus === 'accepted' ? '已接受' : '已回滚') : '验证通过'}</span><button type="button" class="icon-button" data-action="notice" data-message="审核筛选器">${icon('Filter', 15)}</button></header><div class="review-workbench"><aside class="review-file-list"><div class="section-caption"><span>23 个文件</span><span>本轮</span></div>${reviewFiles.map((file) => `<button type="button" class="review-file-row ${state.reviewFile === file.name ? 'is-selected' : ''}" data-action="select-review-file" data-file="${file.name}"><strong>${file.name}</strong><small><span class="add-text">+${file.add}</span> <span class="remove-text">−${file.remove}</span></small></button>`).join('')}</aside><article class="diff-view"><header class="diff-title">${icon('File', 13)}<span>${state.reviewFile}</span><span class="diff-stat"><span class="add-text">+409</span> <span class="remove-text">−13</span></span></header>${diffRows(state.reviewFile)}</article>${fileTree(true)}<footer class="review-actions"><div class="review-actions-copy"><strong>${resolved ? '本轮审核已经完成' : '验证通过，可以进入人工审核'}</strong><small>TypeScript · cargo check · 3 项定向测试</small></div><button type="button" class="quiet-button" data-action="request-changes" ${resolved ? 'disabled' : ''}>请求修改</button><button type="button" class="danger-button" data-action="rollback" ${resolved ? 'disabled' : ''}>回滚</button><button type="button" class="primary-button" data-action="accept-review" ${resolved ? 'disabled' : ''}>接受变更</button></footer></div></section>`;
  };

  const workbenchBody = (kind) => kind === 'run' ? runWorkbench() : kind === 'terminal' ? terminalWorkbench() : kind === 'files' ? filesWorkbench() : kind === 'review' ? reviewWorkbench() : launcher();
  const workbench = (kind) => `<aside class="workbench" data-testid="workbench-panel" aria-label="任务工作台">${workbenchHead(kind)}<div class="workbench-body">${workbenchBody(kind)}</div></aside>`;
  const reviewRail = () => `<aside class="review-rail" data-testid="review-collapsed" aria-label="收起的审核摘要"><button type="button" class="review-rail-button" data-action="expand-review" aria-label="展开审核摘要" aria-expanded="false"><span class="review-rail-icon">${icon('Shield', 19)}<b>1</b></span><span>审核</span></button><span class="review-rail-spacer"></span><span class="review-rail-status">${statusDot(state.reviewStatus === 'pending' ? 'live' : 'success')}</span></aside>`;
  const modal = () => !state.modal ? '' : `<div class="modal-backdrop" data-action="dismiss-modal"><section class="modal" role="alertdialog" data-testid="confirm-dialog" aria-modal="true" aria-labelledby="modal-title" data-modal-panel><div class="modal-mark">${icon(state.modal.intent === 'danger' ? 'Stop' : 'Shield', 18)}</div><h2 id="modal-title">${state.modal.title}</h2><p>${state.modal.body}</p><div class="modal-actions"><button type="button" class="quiet-button" data-action="dismiss-modal">取消</button><button type="button" class="${state.modal.intent === 'danger' ? 'danger-button' : 'primary-button'}" data-action="confirm-modal">${state.modal.confirm}</button></div></section></div>`;

  const render = (focusSelector = '') => {
    const wb = currentWorkbench();
    const classes = [wb.mode === 'closed' ? 'is-workbench-closed' : '', wb.mode === 'collapsed' ? 'is-review-collapsed' : '', state.focus ? 'is-workbench-focus' : ''].filter(Boolean).join(' ');
    const layout = state.focus ? 'focus' : innerWidth <= 1359 ? 'overlay' : innerWidth < 1600 ? 'compact' : 'wide';
    const kind = wb.mode === 'closed' ? 'none' : wb.kind === 'run' ? 'subagents' : wb.kind;
    document.querySelector('#stage').innerHTML = `<div class="prototype-app ${classes}" data-testid="workbench-root" data-workbench-kind="${kind}" data-workbench-mode="${state.focus ? 'focus' : wb.mode}" data-workbench-layout="${layout}" data-owner-key="task:${state.activeTask}" data-demo-state="${wb.mode === 'collapsed' ? 'review-collapsed' : wb.kind}" data-demo-theme="${state.theme}">${desktopBar()}<div class="app-shell">${sidebar()}${conversation()}${wb.mode === 'docked' ? '<button type="button" class="workbench-backdrop" data-action="hide-workbench" aria-label="关闭工作台"></button>' : ''}${wb.mode === 'docked' ? workbench(wb.kind) : wb.mode === 'collapsed' ? reviewRail() : ''}</div>${modal()}${state.toast ? `<div class="toast" role="status">${escapeHtml(state.toast)}</div>` : ''}</div>`;
    if (focusSelector) requestAnimationFrame(() => document.querySelector(focusSelector)?.focus());
    window.__ready = true;
  };

  let toastTimer;
  const notify = (message) => {
    state.toast = message;
    document.querySelector('#live-region').textContent = message;
    clearTimeout(toastTimer);
    render();
    toastTimer = setTimeout(() => { state.toast = ''; render(); }, 2200);
  };
  const openTool = (kind) => {
    const wb = currentWorkbench();
    if (!allowedKinds.includes(kind) || kind === 'launcher') return;
    wb.previousKind = wb.kind === 'launcher' ? wb.previousKind : wb.kind;
    wb.kind = kind;
    wb.lastKind = kind;
    wb.mode = 'docked';
    state.focus = false;
    render(kind === 'terminal' ? '#terminal-input' : '');
  };
  const hideWorkbench = () => {
    const wb = currentWorkbench();
    state.focus = false;
    wb.mode = wb.kind === 'review' && state.reviewStatus === 'pending' ? 'collapsed' : 'closed';
    render('.workbench-reopen');
  };
  const showLauncher = () => {
    const wb = currentWorkbench();
    if (wb.kind !== 'launcher') wb.previousKind = wb.kind;
    wb.kind = 'launcher';
    wb.mode = 'docked';
    state.launcherIndex = 0;
    render('.launcher-row');
  };
  const openModal = (kind, title, body, confirm, intent = 'danger', payload = {}) => {
    state.modal = { kind, title, body, confirm, intent, ...payload };
    render('[data-action="confirm-modal"]');
  };
  const confirmModal = () => {
    const action = state.modal;
    if (!action) return;
    if (action.kind === 'stop-agent') {
      const agent = state.agents.find((item) => item.id === action.agent);
      if (agent) { agent.status = 'stopped'; agent.summary = '已由主代理停止，已有记录仍保留'; }
      state.toast = '子代理已停止，已有结果仍保留';
    }
    if (action.kind === 'end-terminal') {
      state.terminal.alive = false;
      state.terminal.lines.push(['warn', '终端会话已结束（隐藏工作台不会触发此操作）']);
      state.toast = '终端进程已结束';
    }
    if (action.kind === 'accept-review') { state.reviewStatus = 'accepted'; currentWorkbench().mode = 'closed'; state.toast = '本轮变更已接受'; }
    if (action.kind === 'rollback') { state.reviewStatus = 'rolled-back'; currentWorkbench().mode = 'closed'; state.toast = '已模拟回滚，本地文件未被修改'; }
    if (action.kind === 'request-changes') { state.toast = '修改请求已加入当前任务'; }
    state.modal = null;
    render();
  };

  document.querySelector('#stage').addEventListener('click', (event) => {
    const control = event.target.closest('[data-action]');
    if (!control) return;
    const action = control.dataset.action;
    if (action === 'open-tool') openTool(control.dataset.kind);
    if (action === 'show-launcher') showLauncher();
    if (action === 'hide-workbench' || action === 'close-tab') hideWorkbench();
    if (action === 'open-last') { const wb = currentWorkbench(); wb.kind = wb.lastKind || 'run'; wb.mode = 'docked'; render(); }
    if (action === 'expand-review') { const wb = currentWorkbench(); wb.kind = 'review'; wb.mode = 'docked'; render(); }
    if (action === 'toggle-focus') { state.focus = !state.focus; render(); }
    if (action === 'toggle-theme') { setTheme(state.theme === 'light' ? 'dark' : 'light'); render(); }
    if (action === 'select-agent') { state.selectedAgent = control.dataset.agent; render(); }
    if (action === 'stop-agent') openModal('stop-agent', '停止这个子代理？', '停止后不会丢失已经完成的步骤和公开结果。', '停止子代理', 'danger', { agent: control.dataset.agent });
    if (action === 'new-terminal') { state.terminal.session += 1; state.terminal.alive = true; state.terminal.lines = [['muted', `PowerShell 7.6.4 · 会话 ${state.terminal.session}`]]; render('#terminal-input'); }
    if (action === 'end-terminal') openModal('end-terminal', '结束终端会话？', '这会停止当前进程。只想腾出空间时，请使用右上角的隐藏按钮。', '结束终端', 'danger');
    if (action === 'select-file') { if (control.dataset.review === 'true') state.reviewFile = control.dataset.file; else state.selectedFile = control.dataset.file; render(); }
    if (action === 'select-review-file') { state.reviewFile = control.dataset.file; render(); }
    if (action === 'copy-file') notify(`${state.selectedFile} 内容已复制（Demo）`);
    if (action === 'request-changes') openModal('request-changes', '提交修改请求？', '请求会留在当前审核中，工作台仍保持打开。', '提交请求', 'normal');
    if (action === 'rollback') openModal('rollback', '回滚本轮变更？', '这是 Demo，不会修改真实文件；产品实现中会再次列出受影响文件。', '确认回滚', 'danger');
    if (action === 'accept-review') openModal('accept-review', '接受本轮变更？', '验证已经通过，接受后本轮审核将标记为完成。', '接受变更', 'normal');
    if (action === 'dismiss-modal' && !event.target.closest('[data-modal-panel]')) { state.modal = null; render(); }
    if (action === 'dismiss-modal' && control.matches('button')) { state.modal = null; render(); }
    if (action === 'confirm-modal') confirmModal();
    if (action === 'notice') notify(control.dataset.message || '该操作为 Demo 占位');
    if (action === 'select-task') { switchTask(control.dataset.task); render(); }
    if (action === 'new-task') { notify('新对话入口已响应；完整创建流程由主产品页面承载'); }
    if (action === 'send-message') { const input = document.querySelector('#composer-input'); state.composer = input?.value || ''; if (state.composer.trim()) { state.composer = ''; notify('消息已加入队列（Demo）'); } }
  });

  document.querySelector('#stage').addEventListener('input', (event) => {
    if (event.target.matches('[data-role="file-filter"]')) { state.fileQuery = event.target.value; render('[data-role="file-filter"]'); const input = document.querySelector('[data-role="file-filter"]'); input?.setSelectionRange(state.fileQuery.length, state.fileQuery.length); }
    if (event.target.matches('#composer-input')) state.composer = event.target.value;
  });

  document.querySelector('#stage').addEventListener('keydown', (event) => {
    if (event.target.matches('#terminal-input') && event.key === 'Enter') {
      event.preventDefault();
      const command = event.target.value.trim();
      if (!command) return;
      state.terminal.lines.push(['prompt', `PS D:\\project\\rust\\r-code> ${command}`]);
      const output = command === 'clear' || command === 'cls' ? '' : command.includes('test') ? '✓ 43 tests passed in 1.12s' : command === 'pwd' ? 'D:\\project\\rust\\r-code' : `模拟执行完成：${command}`;
      if (command === 'clear' || command === 'cls') state.terminal.lines = [];
      else state.terminal.lines.push(['ok', output]);
      render('#terminal-input');
    }
    if (event.target.closest('.launcher') && ['ArrowDown', 'ArrowUp', 'Enter', 'Escape'].includes(event.key)) {
      event.preventDefault();
      if (event.key === 'ArrowDown') state.launcherIndex = (state.launcherIndex + 1) % launcherItems.length;
      if (event.key === 'ArrowUp') state.launcherIndex = (state.launcherIndex - 1 + launcherItems.length) % launcherItems.length;
      if (event.key === 'Enter') return openTool(launcherItems[state.launcherIndex][0]);
      if (event.key === 'Escape') { const wb = currentWorkbench(); wb.kind = wb.previousKind || 'run'; wb.mode = 'docked'; return render(); }
      render(`.launcher-row:nth-child(${state.launcherIndex + 1})`);
    }
  });

  window.addEventListener('keydown', (event) => {
    if (state.modal && event.key === 'Escape') { state.modal = null; render(); return; }
    if (event.key === 'Escape' && state.focus) { state.focus = false; render(); return; }
    if (!(event.ctrlKey || event.metaKey)) return;
    if (event.key.toLowerCase() === 'k') { event.preventDefault(); showLauncher(); }
    if (event.shiftKey && event.key.toLowerCase() === 'r') { event.preventDefault(); openTool('run'); }
    if (event.shiftKey && event.key.toLowerCase() === 'g') { event.preventDefault(); openTool('review'); }
    if (event.key === '`') { event.preventDefault(); openTool('terminal'); }
    if (event.key.toLowerCase() === 'p') { event.preventDefault(); openTool('files'); }
  });

  document.fonts.ready.then(() => render());
})();
