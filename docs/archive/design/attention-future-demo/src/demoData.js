export const DEMO_DURATION = 35;

export const DIRECTIONS = [
  {
    id: 'command',
    number: 'A',
    name: 'Home + Attention',
    label: 'Home 注意力中枢',
    summary: '从 Chat Session 发起，再回到 Home 处理跨项目 Attention。',
    accent: '#1267e8',
  },
  {
    id: 'rooms',
    number: 'B',
    name: 'Durable Rooms',
    label: '持久任务房间',
    summary: '从 Home 发起，Task Room 保存对话、恢复现场与审计记录。',
    accent: '#6d4bc3',
  },
  {
    id: 'terminal',
    number: 'C',
    name: 'Terminal Workbench',
    label: 'Terminal 工作台',
    summary: '从 Home 发起，然后进入编辑器与语义 Terminal 主舞台。',
    accent: '#0a7c4f',
  },
];

export const SCENES = [
  {
    start: 0,
    end: 6,
    title: '从 Home 发起 Chat Session',
    short: '发起',
    caption: 'Home 保留主入口：输入目标、选项目与 Agent 模式，然后创建可恢复的 Chat Session。',
  },
  {
    start: 6,
    end: 13,
    title: '进入会话与精确现场',
    short: '会话',
    caption: '会话携带 cwd、布局、scrollback 与原生 session ID；⌘⇧] 可直达需要用户的精确 prompt。',
  },
  {
    start: 13,
    end: 21,
    title: '子 Agent 协同',
    short: '协同',
    caption: 'Planner、Test generator 与 Bench runner 投射为原生状态，不再藏在外部 TUI 里。',
  },
  {
    start: 21,
    end: 29,
    title: '边界内审批',
    short: '审批',
    caption: '权限请求带 exact command、cwd 与 Gateway policy；Approve once 不会扩张成 read-screen/send-key。',
  },
  {
    start: 29,
    end: 35,
    title: '语义 Terminal 收尾',
    short: '收尾',
    caption: 'Cmd+F 搜索、prompt 跳转、整段输出选择、长命令通知和可搜索历史汇到同一个完成现场。',
  },
];

export const attentionRows = [
  {
    id: 'permission',
    time: '10:42',
    title: 'Allow npm test in /compiler-lab?',
    subtitle: 'External Claude session · via Charter Gateway',
    project: 'compiler-lab',
    session: 'claude-session-9f3a',
    status: 'needs_permission',
  },
  {
    id: 'input',
    time: '10:28',
    title: 'Which API version should we target?',
    subtitle: 'Waiting for your input',
    project: 'api-gateway',
    session: 'native-8d1b',
    status: 'needs_input',
  },
  {
    id: 'unread',
    time: '10:17',
    title: 'PR #124 opened',
    subtitle: 'Add error codes and update docs',
    project: 'docs-site',
    session: 'native-7a2c',
    status: 'unread',
  },
];

export const workingRows = [
  {
    id: 'parser',
    time: '10:45',
    title: 'Implement parser recovery',
    subtitle: 'Working · 14m elapsed',
    project: 'compiler-lab',
    session: 'native-c3d4',
    status: 'working',
  },
  {
    id: 'auth',
    time: '10:41',
    title: 'Refactor auth middleware',
    subtitle: 'Working · 22m elapsed',
    project: 'api-gateway',
    session: 'native-8d1b',
    status: 'working',
  },
];

export const agents = [
  { name: 'Planner', detail: 'Root cause isolated', status: 'completed', time: '10:44' },
  { name: 'Test generator', detail: '6 integration tests added', status: 'completed', time: '10:47' },
  { name: 'Bench runner', detail: 'Running recovery benchmarks', status: 'working', time: '10:49' },
];
