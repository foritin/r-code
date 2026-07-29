/**
 * 浏览器 Demo 的确定性数据集。
 * 仅在非 Tauri 环境启用：桌面应用里不会用它掩盖真实 IPC 错误。
 */
import type {
  DashboardAttentionItem,
  DashboardTaskSummary,
  CodexCliPreferences,
  CodexIntegrationStatus,
  Notification,
  NotificationPage,
  ProjectActivityItem,
  ProjectActivityPage,
  ProviderCatalog,
  SessionMessage,
  SettingsResponse,
  Task,
  TaskDetail,
  Workspace,
  WorkspaceDashboard,
} from "./types";

const now = Date.now();
const at = (minutesAgo: number) => new Date(now - minutesAgo * 60_000).toISOString();

export const browserMockWorkspaces: Workspace[] = [
  {
    canonical_path: "D:/project/rust/r-code",
    display_name: "r-code",
    access_mode: "risk_based",
    last_opened_at: at(2),
  },
  {
    canonical_path: "D:/project/rust/api-server",
    display_name: "api-server",
    access_mode: "request_approval",
    last_opened_at: at(46),
  },
];

export const browserMockTasks: Task[] = [
  {
    id: "mock-task-queue",
    workspace_path: "D:/project/rust/r-code",
    provider_name: "codex",
    model: "gpt-5.6",
    title: "修复任务队列并发问题",
    goal: "梳理任务队列执行路径并修复并发状态竞争。",
    mode: "edit",
    state: "in_progress",
    worktree_path: null,
    created_at: at(36),
    updated_at: at(2),
  },
  {
    id: "mock-task-review",
    workspace_path: "D:/project/rust/r-code",
    provider_name: "codex",
    model: "gpt-5.6",
    title: "统一错误处理规范",
    goal: "补齐 API 与任务层的错误上下文。",
    mode: "edit",
    state: "review_ready",
    worktree_path: null,
    created_at: at(64),
    updated_at: at(8),
  },
  {
    id: "mock-task-permission",
    workspace_path: "D:/project/rust/r-code",
    provider_name: "codex",
    model: "gpt-5.6",
    title: "优化 Rust 编译性能",
    goal: "定位 workspace 构建的瓶颈。",
    mode: "edit",
    state: "in_progress",
    worktree_path: null,
    created_at: at(20),
    updated_at: at(4),
  },
  {
    id: "mock-task-api",
    workspace_path: "D:/project/rust/api-server",
    provider_name: "codex",
    model: "gpt-5.6",
    title: "添加请求限流中间件",
    goal: "在 API 请求路径增加服务端限流。",
    mode: "edit",
    state: "exploring",
    worktree_path: null,
    created_at: at(28),
    updated_at: at(6),
  },
  {
    id: "mock-task-complete",
    workspace_path: "D:/project/rust/r-code",
    provider_name: "codex",
    model: "gpt-5.6",
    title: "更新依赖并修复告警",
    goal: "升级工作区依赖并处理编译告警。",
    mode: "edit",
    state: "idle",
    worktree_path: null,
    created_at: at(120),
    updated_at: at(26),
  },
];

function detail(task: Task): TaskDetail {
  const isPermission = task.id === "mock-task-permission";
  const isReview = task.id === "mock-task-review";
  const isLive = task.state === "in_progress" || task.state === "exploring";
  const runId = `${task.id}-run`;
  return {
    task,
    active_branch: { id: "main", task_id: task.id, parent_branch_id: null, forked_from_message_id: null, storage_id: "main", is_active: true, created_at: task.created_at },
    branches: [],
    runs: [
      {
        id: runId,
        task_id: task.id,
        branch_id: "main",
        parent_run_id: null,
        agent_kind: "main",
        agent_label: "主代理",
        summary: isPermission ? "准备运行 cargo test" : isReview ? "错误边界与测试已补齐" : "分析实现与关联模块",
        delegated_by_tool_call_id: null,
        model: "gpt-5.6",
        runtime_kind: "native",
        external_session_id: null,
        review_state: isReview ? "pending" : isLive ? "answered" : "accepted",
        started_at: task.created_at,
        ended_at: isLive ? null : task.updated_at,
        usage_json: null,
      },
      ...(task.id === "mock-task-queue" ? [
        {
          id: `${task.id}-codex-active`,
          task_id: task.id,
          branch_id: "main",
          parent_run_id: runId,
          agent_kind: "subagent" as const,
          agent_label: "Codex CLI · 检查并发边界",
          summary: "正在读取任务队列与调度器实现",
          delegated_by_tool_call_id: "delegate-codex-active",
          model: "gpt-5.6-sol",
          runtime_kind: "codex_exec" as const,
          external_session_id: "mock-codex-thread",
          review_state: "pending" as const,
          started_at: at(3),
          ended_at: null,
          usage_json: null,
        },
        {
          id: `${task.id}-codex-done`,
          task_id: task.id,
          branch_id: "main",
          parent_run_id: runId,
          agent_kind: "subagent" as const,
          agent_label: "Codex CLI · 核对锁顺序",
          summary: "发现两处共享状态在持锁期间跨 await，建议缩短临界区。",
          delegated_by_tool_call_id: "delegate-codex-done",
          model: "gpt-5.6-sol",
          runtime_kind: "codex_exec" as const,
          external_session_id: "mock-codex-thread-done",
          review_state: "answered" as const,
          started_at: at(11),
          ended_at: at(7),
          usage_json: null,
        },
      ] : []),
    ],
    events: [
      { id: 1, task_id: task.id, branch_id: "main", event_type: "task_created", created_at: task.created_at },
      { id: 2, task_id: task.id, branch_id: "main", event_type: isLive ? "tool_call" : "run_ended", created_at: task.updated_at },
    ],
    changes: isReview || task.id === "mock-task-queue"
      ? [
          { id: `${task.id}-change-1`, task_id: task.id, tool_call_id: null, path: "src/error.rs", change_type: "modify", before_hash: null, after_hash: null, old_path: null, created_at: at(10) },
          { id: `${task.id}-change-2`, task_id: task.id, tool_call_id: null, path: "src/api.rs", change_type: "modify", before_hash: null, after_hash: null, old_path: null, created_at: at(9) },
        ]
      : [],
    permissions: isPermission
      ? [{ id: `${task.id}-permission`, task_id: task.id, tool_call_id: "tool-1", run_id: runId, caller: "agent", tool_name: "cargo test", risk_level: "R2", input_summary: "运行项目测试以验证本次修改。", decision: "pending", created_at: at(4), decided_at: null }]
      : [],
    verifications: isReview
      ? [{ id: `${task.id}-verification`, task_id: task.id, run_id: runId, command: "cargo test", status: "passed", output_blob_key: null, exit_code: 0, started_at: at(8), ended_at: at(7) }]
      : [],
    queued_messages: [],
  };
}

export const browserMockDetails: Record<string, TaskDetail> = Object.fromEntries(
  browserMockTasks.map((task) => [task.id, detail(task)]),
);

export const browserMockSettings: SettingsResponse = {
  config: {
    default_provider: "codex",
    providers: {
      codex: { base_url: "https://api.openai.com/v1", model: "gpt-5.6" },
      // 第二个就绪服务既让完整 Demo 覆盖跨 Provider/模型选择，也防止常见的
      // DeepSeek 模型名在首页胶囊中重新退化为省略号。
      deepseek: {
        base_url: "https://api.deepseek.com",
        model: "deepseek-v4-pro",
        protocol: "openai_chat",
      },
    },
    log_level: "info",
  },
  validation: null,
  provider_status: {
    codex: { configured: true, ready: true, source: "environment" },
    deepseek: {
      configured: true,
      ready: true,
      source: "environment",
      effective_protocol: "openai_chat",
    },
  },
};

let browserMockCodexInstalled = false;
let browserMockCodexAuthenticated = false;
let browserMockCodexMcpEnabled = false;
let browserMockCodexSkillInstalled = false;
let browserMockCodexModel = "gpt-5.6-sol";
let browserMockCodexReasoning = "max";
let browserMockCodexVerbosity = "medium";
let browserMockCodexPermissionMode: CodexCliPreferences["permission_mode"] = "read_only";

const browserMockCodexModels: CodexCliPreferences["models"] = [
  {
    slug: "gpt-5.6-sol",
    display_name: "GPT-5.6-Sol",
    description: "Latest frontier agentic coding model.",
    default_reasoning_effort: "low",
    supported_reasoning_efforts: ["low", "medium", "high", "xhigh", "max", "ultra"].map((effort) => ({ effort, description: "" })),
  },
  {
    slug: "gpt-5.6-terra",
    display_name: "GPT-5.6-Terra",
    description: "Balanced agentic coding model for everyday work.",
    default_reasoning_effort: "medium",
    supported_reasoning_efforts: ["low", "medium", "high", "xhigh", "max", "ultra"].map((effort) => ({ effort, description: "" })),
  },
  {
    slug: "gpt-5.6-luna",
    display_name: "GPT-5.6-Luna",
    description: "Fast coding model for lightweight work.",
    default_reasoning_effort: "medium",
    supported_reasoning_efforts: ["low", "medium", "high", "xhigh", "max"].map((effort) => ({ effort, description: "" })),
  },
];

export function browserMockCodexIntegrationStatus(): CodexIntegrationStatus {
  const setupState: NonNullable<CodexIntegrationStatus["setup_state"]> = !browserMockCodexInstalled
    ? "install_cli"
    : !browserMockCodexAuthenticated
      ? "login"
      : !browserMockCodexSkillInstalled || !browserMockCodexMcpEnabled
        ? "configure"
        : "ready";
  return {
    cli_available: browserMockCodexInstalled,
    cli_path: browserMockCodexInstalled ? "C:/Users/demo/AppData/Roaming/npm/codex.cmd" : null,
    cli_version: browserMockCodexInstalled ? "codex-cli 0.145.0" : null,
    cli_error: browserMockCodexInstalled ? null : "未检测到可运行的 Codex CLI。",
    installer_available: true,
    installer_command: "npm install -g @openai/codex",
    installer_error: null,
    config_path: "C:/Users/demo/.codex/config.toml",
    config_exists: browserMockCodexInstalled,
    auth_path: "C:/Users/demo/.codex/auth.json",
    authenticated: browserMockCodexAuthenticated,
    auth_status: browserMockCodexAuthenticated ? "authenticated" : "not_authenticated",
    auth_method: browserMockCodexAuthenticated ? "ChatGPT" : null,
    skill_path: "C:/Users/demo/.codex/skills/r-code-terminal/SKILL.md",
    skill_status: browserMockCodexSkillInstalled ? "up_to_date" : "not_installed",
    mcp_server_configured: browserMockCodexMcpEnabled,
    mcp_server_name: "r-code",
    integration_ready: setupState === "ready",
    setup_state: setupState,
    wire_api: "responses",
  };
}

export function browserMockInstallCodexCli(): CodexIntegrationStatus {
  browserMockCodexInstalled = true;
  return browserMockCodexIntegrationStatus();
}

export function browserMockAuthenticateCodex(): void {
  browserMockCodexInstalled = true;
  browserMockCodexAuthenticated = true;
}

export function browserMockEnableCodexMcp(): void {
  browserMockCodexMcpEnabled = true;
}

export function browserMockInstallCodexSkill(): void {
  browserMockCodexSkillInstalled = true;
}

export function browserMockSetupCodexCollaboration(): CodexIntegrationStatus {
  browserMockCodexSkillInstalled = true;
  browserMockCodexMcpEnabled = true;
  return browserMockCodexIntegrationStatus();
}

export function browserMockCodexCliPreferences(): CodexCliPreferences {
  return {
    model: browserMockCodexModel || null,
    reasoning_effort: browserMockCodexReasoning || null,
    verbosity: browserMockCodexVerbosity || null,
    permission_mode: browserMockCodexPermissionMode,
    models: browserMockCodexModels,
    config_path: "C:/Users/demo/.codex/config.toml",
  };
}

export function browserMockSaveCodexCliPreferences(
  model: string | null,
  reasoningEffort: string | null,
  verbosity: string | null,
  permissionMode: string | null,
): CodexCliPreferences {
  browserMockCodexModel = model ?? "";
  browserMockCodexReasoning = reasoningEffort ?? "";
  browserMockCodexVerbosity = verbosity ?? "";
  if (permissionMode) browserMockCodexPermissionMode = permissionMode;
  return browserMockCodexCliPreferences();
}

export const browserMockProviderCatalog: ProviderCatalog = {
  presets: [
    {
      id: "codex",
      label: "Codex",
      protocol: "openai_responses",
      native: ["openai_responses"],
      auth: "bearer",
      base_url: "https://api.openai.com/v1",
      reasoning_replay: true,
      model: "gpt-5.6",
      models: ["gpt-5.6"],
      category: "official",
      website_url: "https://openai.com",
      api_key_url: null,
      endpoint_candidates: [],
      template_vars: [],
      max_output_tokens: null,
      context_window: null,
      note: null,
    },
    {
      id: "deepseek",
      label: "DeepSeek",
      protocol: "openai_chat",
      native: ["openai_chat"],
      auth: "bearer",
      base_url: "https://api.deepseek.com",
      reasoning_replay: false,
      model: "deepseek-v4-pro",
      models: ["deepseek-v4-pro", "deepseek-v4-flash"],
      category: "cn_official",
      website_url: "https://platform.deepseek.com",
      api_key_url: "https://platform.deepseek.com/api_keys",
      endpoint_candidates: [],
      template_vars: [],
      max_output_tokens: 8192,
      context_window: 1_000_000,
      note: null,
    },
  ],
};

export const browserMockFiles: Record<string, { content: string; revision: string }> = {
  "Cargo.toml": { revision: "mock-cargo-1", content: "[workspace]\nresolver = \"2\"\nmembers = [\"crates/r-code-core\", \"src-tauri\"]\n" },
  "README.md": { revision: "mock-readme-1", content: "# R-Code\n\n本地优先的编码 agent 驾驶舱。\n" },
  "src/main.rs": { revision: "mock-main-1", content: "fn main() {\n    r_code::run();\n}\n" },
  "src/error.rs": { revision: "mock-error-1", content: "use thiserror::Error;\n\n#[derive(Debug, Error)]\npub enum AppError {\n    #[error(\"operation failed: {0}\")]\n    Operation(String),\n}\n" },
  "src/api.rs": { revision: "mock-api-1", content: "pub async fn health() -> &'static str {\n    \"ok\"\n}\n" },
};

export const browserMockFileEntries = (path: string | null) => {
  if (!path) return [
    { path: "src", name: "src", is_directory: true },
    { path: "Cargo.toml", name: "Cargo.toml", is_directory: false },
    { path: "README.md", name: "README.md", is_directory: false },
  ];
  if (path === "src") return [
    { path: "src/main.rs", name: "main.rs", is_directory: false },
    { path: "src/error.rs", name: "error.rs", is_directory: false },
    { path: "src/api.rs", name: "api.rs", is_directory: false },
  ];
  return [];
};

const browserMockMessageStore: Record<string, SessionMessage[]> = {};

export function browserMockMessages(taskId: string): SessionMessage[] {
  if (browserMockMessageStore[taskId]) return browserMockMessageStore[taskId];
  const task = browserMockTasks.find((item) => item.id === taskId);
  if (!task) return [];
  browserMockMessageStore[taskId] = [
    { id: `${taskId}-message-1`, branch_id: "main", kind: "message", role: "user", text: task.goal, timestamp: task.created_at },
    { id: `${taskId}-message-2`, branch_id: "main", kind: "message", role: "assistant", text: `我会先检查相关实现，然后推进「${task.title}」。`, timestamp: at(12) },
    { id: `${taskId}-message-3`, branch_id: "main", kind: "tool_call", tool_name: "read_file", call_id: "mock-read", input_json: '{"path":"src/error.rs"}', timestamp: task.updated_at },
  ];
  return browserMockMessageStore[taskId];
}

export function browserMockSetMessages(taskId: string, messages: SessionMessage[]): void {
  browserMockMessageStore[taskId] = messages;
}

export function browserMockSubagentMessages(taskId: string, subagentId: string): SessionMessage[] {
  if (taskId !== "mock-task-queue" || !subagentId.includes("codex-")) return [];
  const active = subagentId.endsWith("codex-active");
  const storage = `mock-subagent-${subagentId}`;
  const system = (line: number, event: string, data: unknown): SessionMessage => ({
    id: `${storage}:${line}`,
    branch_id: "main",
    kind: "system",
    text: event,
    output_json: JSON.stringify(data),
  });
  return [
    system(1, "subagent_lifecycle", { state: "running", detail: "Codex CLI 已开始处理工作区" }),
    system(2, "subagent_activity", { phase: "requesting", detail: "已连接 Codex CLI，正在准备工作区" }),
    {
      id: `${storage}:3`, branch_id: "main", kind: "tool_call", tool_name: "Codex 命令",
      call_id: "mock-call-1", input_json: JSON.stringify({ summary: "rg -n 'Mutex|RwLock|await' crates" }),
    },
    {
      id: `${storage}:4`, branch_id: "main", kind: "tool_result", call_id: "mock-call-1",
      output_json: JSON.stringify({ status: "completed" }), is_error: false,
    },
    ...(active ? [
      system(5, "subagent_activity", { phase: "requesting", detail: "Codex CLI 正在分析工作区" }),
    ] : [
      { id: `${storage}:5`, branch_id: "main", kind: "message", role: "assistant", text: "发现两处共享状态在持锁期间跨 await，建议缩短临界区。" } as SessionMessage,
      system(6, "subagent_lifecycle", { state: "completed", detail: "锁顺序核对完成" }),
    ]),
  ];
}

/** 浏览器原型中的停止操作也要改变运行树，避免“按钮可点但没有结果”的假交互。 */
export function browserMockAbortSubagent(taskId: string, subagentId: string): void {
  const detail = browserMockDetails[taskId];
  const run = detail?.runs.find((item) => item.id === subagentId && item.agent_kind === "subagent");
  if (!run || run.ended_at) throw new Error("子代理不存在或已经结束");
  const stoppedAt = new Date().toISOString();
  browserMockDetails[taskId] = {
    ...detail,
    runs: detail.runs.map((item) => item.id === subagentId ? {
      ...item,
      ended_at: stoppedAt,
      review_state: "aborted",
      summary: "已由用户停止",
    } : item),
    events: [...detail.events, {
      id: detail.events.length + 1,
      task_id: taskId,
      branch_id: run.branch_id,
      event_type: "subagent_finished",
      created_at: stoppedAt,
    }],
  };
}

function mockChangeSummary(task: Task) {
  const changes = browserMockDetails[task.id]?.changes ?? [];
  return changes.reduce(
    (summary, change) => {
      summary.files += 1;
      if (change.change_type === "create") summary.created += 1;
      else if (change.change_type === "delete") summary.removed += 1;
      else if (change.change_type === "rename") summary.renamed += 1;
      else summary.modified += 1;
      return summary;
    },
    { files: 0, created: 0, modified: 0, removed: 0, renamed: 0 },
  );
}

function mockTaskSummary(task: Task): DashboardTaskSummary {
  const detail = browserMockDetails[task.id];
  const activeRun = detail?.runs.find((run) => run.ended_at === null) ?? detail?.runs.find((run) => run.agent_kind === "main") ?? null;
  const permission = detail?.permissions.find((item) => item.decision === "pending");
  const activity = permission
    ? `等待授权 · ${permission.tool_name}`
    : activeRun?.summary || (task.state === "review_ready" ? "变更已准备好审查" : task.state === "idle" ? "任务已完成" : "正在推进任务");
  return {
    task,
    activity,
    agent_label: activeRun?.agent_label || (activeRun?.agent_kind === "subagent" ? "子代理" : "主代理"),
    pending_permission_count: detail?.permissions.filter((item) => item.decision === "pending").length ?? 0,
    active_run: activeRun,
    change_summary: mockChangeSummary(task),
    latest_verification: detail?.verifications[detail.verifications.length - 1] ?? null,
  };
}

/** 与真实 cmd_workspace_dashboard 同形状的浏览器预览数据。 */
export function browserMockWorkspaceDashboard(workspacePath: string): WorkspaceDashboard {
  const workspace = browserMockWorkspaces.find((item) => item.canonical_path === workspacePath) ?? browserMockWorkspaces[0];
  const tasks = browserMockTasks.filter((task) => task.workspace_path === workspace.canonical_path).map(mockTaskSummary);
  const attention: DashboardAttentionItem[] = [];
  for (const summary of tasks) {
    const detail = browserMockDetails[summary.task.id];
    for (const permission of detail?.permissions.filter((item) => item.decision === "pending") ?? []) {
      attention.push({ kind: "permission", task: summary.task, permission, since: permission.created_at });
    }
    if (summary.task.state === "review_ready") attention.push({ kind: "review_ready", task: summary.task, since: summary.task.updated_at });
  }
  tasks.sort((left, right) => {
    const rank = (item: DashboardTaskSummary) => item.pending_permission_count ? 0 : item.task.state === "review_ready" ? 1 : item.active_run?.ended_at === null ? 2 : item.task.state === "idle" ? 4 : 3;
    return rank(left) - rank(right) || right.task.updated_at.localeCompare(left.task.updated_at);
  });
  const hourAgo = Date.now() - 60 * 60_000;
  const completed = tasks.filter((item) => item.task.state === "idle" && Date.parse(item.task.updated_at) >= hourAgo);
  return {
    workspace,
    generated_at: new Date().toISOString(),
    metrics: {
      task_count: tasks.length,
      pending_permission_count: attention.filter((item) => item.kind === "permission").length,
      review_ready_count: attention.filter((item) => item.kind === "review_ready").length,
      running_task_count: tasks.filter((item) => item.pending_permission_count === 0 && item.active_run?.ended_at === null).length,
      active_subagent_count: tasks.reduce((count, item) => count + (browserMockDetails[item.task.id]?.runs.filter((run) => run.agent_kind === "subagent" && run.ended_at === null).length ?? 0), 0),
      completed_last_hour_count: completed.length,
    },
    tasks,
    attention,
    completed,
  };
}

function mockEventLabel(kind: ProjectActivityItem["kind"]): string {
  const labels: Partial<Record<ProjectActivityItem["kind"], string>> = {
    task_created: "创建了任务",
    run_started: "开始执行",
    run_ended: "完成了一次执行",
    tool_call: "调用了工具",
    tool_result: "收到了工具结果",
    permission_requested: "请求了权限",
    permission_decided: "完成了权限裁决",
    change_requested: "请求继续修改",
  };
  return labels[kind] ?? "更新了任务";
}

export function browserMockActivityList(workspacePath?: string | null): ProjectActivityPage {
  const items: ProjectActivityItem[] = browserMockTasks
    .filter((task) => !workspacePath || task.workspace_path === workspacePath)
    .flatMap((task) => (browserMockDetails[task.id]?.events ?? []).map((event) => ({
      id: `${task.id}:${event.id}`,
      at: event.created_at,
      kind: event.event_type,
      summary: mockEventLabel(event.event_type),
      task_id: task.id,
      task_title: task.title,
      workspace_path: task.workspace_path,
      run_id: browserMockDetails[task.id]?.runs[0]?.id,
      actor: "主代理",
      metadata: { event_id: event.id, branch_id: event.branch_id },
    })))
    .sort((left, right) => right.at.localeCompare(left.at));
  return { items, next_cursor: undefined };
}

export const browserMockNotifications: Notification[] = [
  {
    id: "mock-notification-review",
    kind: "review_ready",
    title: "等待审核：统一错误处理规范",
    body: "本轮变更已准备好验收。",
    task_id: "mock-task-review",
    workspace_path: "D:/project/rust/r-code",
    created_at: at(8),
    read_at: null,
  },
  {
    id: "mock-notification-permission",
    kind: "permission_requested",
    title: "需要授权：优化 Rust 编译性能",
    body: "运行项目测试以验证本次修改。",
    task_id: "mock-task-permission",
    workspace_path: "D:/project/rust/r-code",
    created_at: at(4),
    read_at: null,
  },
];

export function browserMockNotificationList(unreadOnly = false): NotificationPage {
  const notifications = browserMockNotifications
    .filter((notification) => !unreadOnly || notification.read_at === null)
    .sort((left, right) => right.created_at.localeCompare(left.created_at));
  return {
    notifications: notifications.map((notification) => ({ ...notification })),
    next_cursor: undefined,
    unread_count: browserMockNotifications.filter((notification) => notification.read_at === null).length,
  };
}

export function browserMockMarkNotificationRead(notificationId: string): boolean {
  const notification = browserMockNotifications.find((item) => item.id === notificationId);
  if (!notification) return false;
  notification.read_at ??= new Date().toISOString();
  return true;
}

export function browserMockMarkAllNotificationsRead(): number {
  const now = new Date().toISOString();
  let count = 0;
  for (const notification of browserMockNotifications) {
    if (notification.read_at === null) {
      notification.read_at = now;
      count += 1;
    }
  }
  return count;
}

/** 让浏览器 demo 的“请求修改”也有可见状态变化。 */
export function browserMockChangeRequest(taskId: string): void {
  const task = browserMockTasks.find((item) => item.id === taskId);
  const detail = browserMockDetails[taskId];
  if (!task || !detail) return;
  const updatedAt = new Date().toISOString();
  task.state = "in_progress";
  task.updated_at = updatedAt;
  detail.task = task;
  detail.runs.unshift({
    id: `${taskId}-revision-run`, task_id: taskId, branch_id: "main", parent_run_id: null,
    agent_kind: "main", agent_label: "主代理", summary: "根据审核反馈继续修改", delegated_by_tool_call_id: null,
    model: "gpt-5.6", runtime_kind: "native", external_session_id: null, review_state: "pending", started_at: updatedAt, ended_at: null, usage_json: null,
  });
  detail.events.push({ id: detail.events.length + 1, task_id: taskId, branch_id: "main", event_type: "change_requested", created_at: updatedAt });
  const reviewNotification = browserMockNotifications.find((item) => item.task_id === taskId && item.kind === "review_ready");
  if (reviewNotification) reviewNotification.read_at = updatedAt;
}

export function shouldUseBrowserMock(): boolean {
  return typeof window !== "undefined" && !Reflect.has(window, "__TAURI_INTERNALS__");
}
