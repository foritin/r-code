/**
 * 浏览器 Demo 的确定性数据集。
 * 仅在非 Tauri 环境启用：桌面应用里不会用它掩盖真实 IPC 错误。
 */
import type {
  DashboardAttentionItem,
  DashboardTaskSummary,
  CodexCliPreferences,
  CodexIntegrationStatus,
  RtkStatus,
  Notification,
  NotificationPage,
  ProjectActivityItem,
  ProjectActivityPage,
  ProviderCatalog,
  RecoveryPageData,
  SessionMessage,
  SettingsResponse,
  Task,
  TaskDetail,
  Workspace,
  WorkspaceDashboard,
} from "./types";

const now = Date.now();
const at = (minutesAgo: number) => new Date(now - minutesAgo * 60_000).toISOString();

/** Mutable only inside browser QA pages so startup-recovery navigation can be exercised. */
export const browserMockRecovery: RecoveryPageData = {
  interrupted_tasks: [],
  orphaned_permissions: 0,
};

export const browserMockWorkspaces: Workspace[] = [
  {
    id: "7f4d622084db4d359fb2f50c9780a1ad",
    canonical_path: "D:/project/rust/r-code",
    display_name: "r-code",
    access_mode: "risk_based",
    last_opened_at: at(2),
    memory_mode: "inherit",
    memory_generation: 1,
  },
  {
    id: "a49332f6079b4b629aee49ed1bfe8e71",
    canonical_path: "D:/project/rust/api-server",
    display_name: "api-server",
    access_mode: "request_approval",
    last_opened_at: at(46),
    memory_mode: "inherit",
    memory_generation: 1,
  },
];

export const browserMockTasks: Task[] = [
  {
    id: "mock-task-queue",
    workspace_path: "D:/project/rust/r-code",
    provider_name: "openai",
    agent_engine: "r_code",
    model: "gpt-5.6-sol",
    inference: { reasoning_effort: "high", verbosity: "medium" },
    title: "修复任务队列并发问题",
    goal: "梳理任务队列执行路径并修复并发状态竞争。",
    goal_active: false,
    mode: "edit",
    state: "in_progress",
    worktree_path: null,
    created_at: at(36),
    updated_at: at(2),
  },
  {
    id: "mock-task-review",
    workspace_path: "D:/project/rust/r-code",
    provider_name: "deepseek",
    agent_engine: "r_code",
    model: "deepseek-v4-pro",
    inference: { thinking: "enabled", reasoning_effort: "high" },
    title: "统一错误处理规范",
    goal: "补齐 API 与任务层的错误上下文。",
    goal_active: false,
    mode: "edit",
    state: "review_ready",
    worktree_path: null,
    created_at: at(64),
    updated_at: at(8),
  },
  {
    id: "mock-task-permission",
    workspace_path: "D:/project/rust/r-code",
    provider_name: "openai",
    agent_engine: "r_code",
    model: "gpt-5.6-sol",
    inference: {},
    title: "优化 Rust 编译性能",
    goal: "定位 workspace 构建的瓶颈。",
    goal_active: false,
    mode: "edit",
    state: "in_progress",
    worktree_path: null,
    created_at: at(20),
    updated_at: at(4),
  },
  {
    id: "mock-task-api",
    workspace_path: "D:/project/rust/api-server",
    provider_name: "openai",
    agent_engine: "r_code",
    model: "gpt-5.6-sol",
    inference: { reasoning_effort: "high" },
    title: "添加请求限流中间件",
    goal: "在 API 请求路径增加服务端限流。",
    goal_active: false,
    mode: "edit",
    state: "exploring",
    worktree_path: null,
    created_at: at(28),
    updated_at: at(6),
  },
  {
    id: "mock-task-complete",
    workspace_path: "D:/project/rust/r-code",
    provider_name: "openai",
    agent_engine: "codex",
    model: "gpt-5.6-sol",
    inference: {},
    title: "更新依赖并修复告警",
    goal: "升级工作区依赖并处理编译告警。",
    goal_active: false,
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
  const branchId = task.id === "mock-task-queue" ? "branch-queue-fix" : "main";
  return {
    task,
    active_branch: task.id === "mock-task-queue"
      ? { id: branchId, task_id: task.id, parent_branch_id: "main", forked_from_message_id: "main:7", storage_id: branchId, is_active: true, created_at: at(35) }
      : { id: "main", task_id: task.id, parent_branch_id: null, forked_from_message_id: null, storage_id: "main", is_active: true, created_at: task.created_at },
    branches: [],
    runs: [
      {
        id: runId,
        task_id: task.id,
        branch_id: branchId,
        parent_run_id: null,
        agent_kind: "main",
        agent_label: "主代理",
        summary: isPermission ? "准备运行 cargo test" : isReview ? "错误边界与测试已补齐" : "分析实现与关联模块",
        delegated_by_tool_call_id: null,
        model: "gpt-5.6",
        runtime_kind: "native",
        access_mode: "read_only",
        require_approval: false,
        routing_reason: "会话已选择 R-Code 主 Agent",
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
          branch_id: branchId,
          parent_run_id: runId,
          agent_kind: "subagent" as const,
          agent_label: "Codex CLI · 检查并发边界",
          summary: "正在读取任务队列与调度器实现",
          delegated_by_tool_call_id: "delegate-codex-active",
          model: "gpt-5.6-sol",
          runtime_kind: "codex_exec" as const,
          access_mode: "full_access" as const,
          require_approval: true,
          routing_reason: "复杂并发检查由均衡路由交给 Codex",
          external_session_id: "mock-codex-thread",
          review_state: "pending" as const,
          started_at: at(3),
          ended_at: null,
          usage_json: null,
        },
        {
          id: `${task.id}-codex-done`,
          task_id: task.id,
          branch_id: branchId,
          parent_run_id: runId,
          agent_kind: "subagent" as const,
          agent_label: "Codex CLI · 核对锁顺序",
          summary: "发现两处共享状态在持锁期间跨 await，建议缩短临界区。",
          delegated_by_tool_call_id: "delegate-codex-done",
          model: "gpt-5.6-sol",
          runtime_kind: "codex_exec" as const,
          access_mode: "full_access" as const,
          require_approval: false,
          routing_reason: "复杂锁顺序核对由均衡路由交给 Codex",
          external_session_id: "mock-codex-thread-done",
          review_state: "answered" as const,
          started_at: at(11),
          ended_at: at(7),
          usage_json: null,
        },
        {
          id: `${task.id}-codex-readonly`,
          task_id: task.id,
          branch_id: branchId,
          parent_run_id: runId,
          agent_kind: "subagent" as const,
          agent_label: "Codex CLI · 只读复核",
          summary: "只读复核已完成。",
          delegated_by_tool_call_id: "delegate-codex-readonly",
          model: "gpt-5.6-sol",
          runtime_kind: "codex_exec" as const,
          access_mode: "read_only" as const,
          require_approval: false,
          routing_reason: "只读任务保持最小权限",
          external_session_id: "mock-codex-thread-readonly",
          review_state: "answered" as const,
          started_at: at(15),
          ended_at: at(13),
          usage_json: null,
        },
      ] : []),
    ],
    events: [
      { id: 1, task_id: task.id, branch_id: branchId, event_type: "task_created", created_at: task.created_at },
      { id: 2, task_id: task.id, branch_id: branchId, event_type: isLive ? "tool_call" : "run_ended", created_at: task.updated_at },
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
    default_provider: "openai",
    providers: {
      openai: {
        base_url: "https://api.openai.com/v1",
        model: "gpt-5.6-sol",
        provider_kind: "openai",
        protocol: "openai_responses",
      },
      // 第二个就绪服务既让完整 Demo 覆盖跨 Provider/模型选择，也防止常见的
      // DeepSeek 模型名在首页胶囊中重新退化为省略号。
      deepseek: {
        base_url: "https://api.deepseek.com",
        model: "deepseek-v4-pro",
        provider_kind: "deepseek",
        protocol: "openai_chat",
      },
    },
    log_level: "info",
    orchestration: {
      default_agent_engine: "r_code",
      delegation_router: "balanced",
      allow_cross_engine_delegation: true,
      quality_loop: "off",
      quality_reviewer: "r_code",
      max_review_rounds: 1,
    },
    agent_prompts: {
      main_agent: "主 Agent 对最终结果负责；只在委派有明确收益时拆分边界清晰的子任务。",
      subagent: "子代理只完成父 Agent 指定的任务，不再委派，并返回可核验的简洁摘要。",
    },
  },
  validation: null,
  provider_status: {
    openai: {
      configured: true,
      ready: true,
      source: "environment",
      effective_protocol: "openai_responses",
    },
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
let browserMockRtkAvailable = false;
let browserMockRtkEnabled = false;

const browserMockCodexModels: CodexCliPreferences["models"] = [
  {
    slug: "gpt-5.6-sol",
    display_name: "GPT-5.6-Sol",
    description: "Latest frontier agentic coding model.",
    default_reasoning_effort: "low",
    supported_reasoning_efforts: ["low", "medium", "high", "xhigh", "max", "ultra"].map((effort) => ({ effort, description: "" })),
    supports_images: true,
  },
  {
    slug: "gpt-5.6-terra",
    display_name: "GPT-5.6-Terra",
    description: "Balanced agentic coding model for everyday work.",
    default_reasoning_effort: "medium",
    supported_reasoning_efforts: ["low", "medium", "high", "xhigh", "max", "ultra"].map((effort) => ({ effort, description: "" })),
    supports_images: true,
  },
  {
    slug: "gpt-5.6-luna",
    display_name: "GPT-5.6-Luna",
    description: "Fast coding model for lightweight work.",
    default_reasoning_effort: "medium",
    supported_reasoning_efforts: ["low", "medium", "high", "xhigh", "max"].map((effort) => ({ effort, description: "" })),
    supports_images: true,
  },
  {
    slug: "gpt-5.3-codex-spark",
    display_name: "GPT-5.3-Codex-Spark",
    description: "Fast text-only coding model.",
    default_reasoning_effort: "medium",
    supported_reasoning_efforts: ["low", "medium", "high"].map((effort) => ({ effort, description: "" })),
    supports_images: false,
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

export function browserMockRtkStatus(): RtkStatus {
  return {
    enabled: browserMockRtkEnabled && browserMockRtkAvailable,
    available: browserMockRtkAvailable,
    managed: browserMockRtkAvailable,
    version: browserMockRtkAvailable ? "rtk 0.45.0" : null,
    source: browserMockRtkAvailable ? "managed" : null,
    platform: "windows-x86_64",
  };
}

export function browserMockSetRtkEnabled(enabled: boolean): RtkStatus {
  if (enabled) browserMockRtkAvailable = true;
  browserMockRtkEnabled = enabled;
  return browserMockRtkStatus();
}

export const browserMockProviderCatalog: ProviderCatalog = {
  presets: [
    {
      id: "openai",
      label: "OpenAI",
      protocol: "openai_responses",
      native: ["openai_responses"],
      auth: "bearer",
      base_url: "https://api.openai.com/v1",
      reasoning_replay: true,
      model: "gpt-5.6-sol",
      models: ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna", "gpt-5.5"],
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
      native: ["openai_chat", "openai_responses"],
      auth: "bearer",
      base_url: "https://api.deepseek.com",
      reasoning_replay: false,
      model: "deepseek-v4-flash",
      models: ["deepseek-v4-flash", "deepseek-v4-pro"],
      category: "cn_official",
      website_url: "https://platform.deepseek.com",
      api_key_url: "https://platform.deepseek.com/api_keys",
      endpoint_candidates: [{
        url: "https://api.deepseek.com/anthropic",
        protocol: "anthropic_messages",
        native: ["anthropic_messages"],
        label: "Anthropic 兼容口",
      }],
      template_vars: [],
      max_output_tokens: 393_216,
      context_window: 1_000_000,
      note: "V4-Flash 已升级为 0731 版本；Responses 当前仅支持 Flash，V4-Pro 请走 Chat 或 Anthropic 口",
    },
  ],
  hosted_web_routes: [
    {
      provider_id: "openai",
      provider_label: "OpenAI",
      host_pattern: "api.openai.com",
      path: "/v1",
      protocol: "openai_responses",
      model_patterns: ["gpt-*"],
      format: "standard",
      read: "via_search",
      docs_url: "https://developers.openai.com/api/docs/guides/tools-web-search",
      docs_label: "查看 OpenAI Web Search",
    },
    {
      provider_id: "deepseek",
      provider_label: "DeepSeek",
      host_pattern: "api.deepseek.com",
      path: "",
      protocol: "openai_responses",
      model_patterns: ["deepseek-v4-flash"],
      format: "standard",
      read: "none",
      docs_url: "https://api-docs.deepseek.com/guides/responses_api/",
      docs_label: "查看 DeepSeek Responses",
    },
    {
      provider_id: "deepseek",
      provider_label: "DeepSeek",
      host_pattern: "api.deepseek.com",
      path: "/anthropic",
      protocol: "anthropic_messages",
      model_patterns: ["deepseek-v4-*"],
      format: "standard",
      read: "none",
      docs_url: "https://api-docs.deepseek.com/quick_start/agent_integrations/claude_code/",
      docs_label: "查看 DeepSeek Anthropic Web Search",
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
    { path: "assets", name: "assets", is_directory: true },
    { path: "Cargo.toml", name: "Cargo.toml", is_directory: false },
    { path: "README.md", name: "README.md", is_directory: false },
  ];
  if (path === "src") return [
    { path: "src/main.rs", name: "main.rs", is_directory: false },
    { path: "src/error.rs", name: "error.rs", is_directory: false },
    { path: "src/api.rs", name: "api.rs", is_directory: false },
  ];
  if (path === "assets") return [
    { path: "assets/demo-sky.png", name: "demo-sky.png", is_directory: false },
  ];
  return [];
};

const browserMockMessageStore: Record<string, SessionMessage[]> = {};

export function browserMockMessages(taskId: string): SessionMessage[] {
  if (browserMockMessageStore[taskId]) return browserMockMessageStore[taskId];
  const task = browserMockTasks.find((item) => item.id === taskId);
  if (!task) return [];
  if (taskId === "mock-task-queue") {
    const message = (line: number, role: "user" | "assistant", text: string): SessionMessage => ({
      id: `${taskId}-message-${line}`,
      branch_id: "main",
      kind: "message",
      role,
      text,
    });
    const call = (line: number, callId: string, toolName: string, input: unknown): SessionMessage => ({
      id: `${taskId}-message-${line}`,
      branch_id: "main",
      kind: "tool_call",
      tool_name: toolName,
      call_id: callId,
      input_json: JSON.stringify(input),
    });
    const result = (line: number, callId: string, output: unknown, isError = false): SessionMessage => ({
      id: `${taskId}-message-${line}`,
      branch_id: "main",
      kind: "tool_result",
      call_id: callId,
      output_json: JSON.stringify(output),
      is_error: isError,
    });
    const system = (line: number, event: string, data: unknown): SessionMessage => ({
      id: `${taskId}-message-${line}`,
      branch_id: "main",
      kind: "system",
      text: event,
      output_json: JSON.stringify(data),
    });
    browserMockMessageStore[taskId] = [
      // 分支会复制父分支的历史前缀，但 TaskDetail 只返回当前分支的 runs。
      // 这两条专门覆盖“run 不能从第一轮向后硬填”的回归场景。
      message(-2, "user", "编辑历史消息后，原分支的上下文还会保留吗？"),
      message(-1, "assistant", "会保留已确认的历史上下文，新的执行从当前分支继续。"),
      message(1, "user", task.goal),
      message(2, "assistant", "我会先核对任务队列和调度器的共享状态，再把验证工作拆给 Codex 子代理并行检查。"),
      call(3, "mock-shell-single", "shell_command", { command: "rg -n \"Mutex|RwLock|await\" src-tauri/src" }),
      result(4, "mock-shell-single", { stdout: "src-tauri/src/commands.rs:241: state.write().await\n", exit_code: 0 }),
      message(5, "assistant", "现有实现把持锁范围和异步等待混在一起；我会保留关键上下文，再继续核对并发边界。"),
      system(6, "r_code_context_compacted", { before_messages: 28, after_messages: 7 }),
      message(7, "assistant", "上下文已经整理。下面让两个子代理分别检查调度路径与锁顺序，主流程继续准备验证命令。"),
      call(8, "delegate-codex-active", "delegate_task", {
        agent: "codex",
        label: "Codex CLI · 检查并发边界",
        goal: "只读检查任务队列的调度与取消边界。",
      }),
      result(9, "delegate-codex-active", { agent: "codex", label: "Codex CLI · 检查并发边界", status: "running" }),
      system(10, "subagent_lifecycle", {
        scope: {
          run_id: `${taskId}-codex-active`,
          agent_kind: "subagent",
          agent_label: "Codex CLI · 检查并发边界",
          access_mode: "full_access",
          require_approval: true,
        },
        state: "running",
        detail: "正在读取任务队列与调度器实现",
      }),
      call(11, "delegate-codex-done", "delegate_task", {
        agent: "codex",
        label: "Codex CLI · 核对锁顺序",
        goal: "只读核对共享状态的锁顺序。",
      }),
      result(12, "delegate-codex-done", { agent: "codex", label: "Codex CLI · 核对锁顺序", status: "completed" }),
      system(13, "subagent_lifecycle", {
        scope: {
          run_id: `${taskId}-codex-done`,
          agent_kind: "subagent",
          agent_label: "Codex CLI · 核对锁顺序",
          access_mode: "full_access",
          require_approval: false,
        },
        state: "completed",
        detail: "锁顺序核对完成",
      }),
      call(14, "collect-codex", "collect_subagents", { ids: [`${taskId}-codex-active`, `${taskId}-codex-done`] }),
      result(15, "collect-codex", {
        subagents: [
          { label: "Codex CLI · 检查并发边界", status: "running" },
          { label: "Codex CLI · 核对锁顺序", status: "completed" },
        ],
      }),
      message(16, "assistant", "并行检查已经定位到两个竞争窗口；我会运行定向检查并确认修改没有破坏现有调度语义。"),
      call(17, "mock-shell-check", "shell_command", { command: "cargo check -p r-code-agent-worker" }),
      result(18, "mock-shell-check", { stdout: "Finished `dev` profile", exit_code: 0 }),
      call(19, "mock-shell-test", "shell_command", { command: "cargo test -p r-code-agent-worker supervisor" }),
      result(20, "mock-shell-test", { stdout: "test result: ok. 8 passed", exit_code: 0 }),
      message(21, "assistant", "定向检查通过。最后把锁范围收紧，并保留必要的取消状态更新。"),
      call(22, "mock-file-edit", "apply_patch", { path: "crates/r-code-agent-worker/src/llm_runtime.rs", patch: "缩短共享状态的持锁范围" }),
      result(23, "mock-file-edit", { content: "Done!", changed_files: 1 }),
      message(24, "assistant", "任务队列的竞争窗口已收紧，定向检查通过；正在运行的 Codex 子代理会继续在右侧详情中更新。"),
    ];
    return browserMockMessageStore[taskId];
  }
  browserMockMessageStore[taskId] = [
    { id: `${taskId}-message-1`, branch_id: "main", kind: "message", role: "user", text: task.goal, timestamp: task.created_at },
    {
      id: `${taskId}-message-2`,
      branch_id: "main",
      kind: "message",
      role: "assistant",
      text: taskId === "mock-task-complete"
        ? `任务已完成，并生成了可预览的本地产物。浏览器 Demo 使用占位图验证预览交互。\n\n[预览图片产物](C:/Users/demo/.codex/generated_images/r-code-preview.png)\n\n[打开实现文件](src/main.rs#L2C3)`
        : `我会先检查相关实现，然后推进「${task.title}」。`,
      timestamp: at(12),
    },
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
  const accessMode = subagentId.endsWith("codex-readonly") ? "read_only" : "full_access";
  const requireApproval = active;
  const storage = `mock-subagent-${subagentId}`;
  const system = (line: number, event: string, data: unknown): SessionMessage => ({
    id: `${storage}:${line}`,
    branch_id: "main",
    kind: "system",
    text: event,
    output_json: JSON.stringify(data),
  });
  return [
    system(1, "subagent_lifecycle", {
      scope: {
        run_id: subagentId,
        agent_kind: "subagent",
        access_mode: accessMode,
        require_approval: requireApproval,
      },
      state: "running",
      detail: accessMode === "read_only"
        ? "Codex CLI 已开始只读检查工作区"
        : requireApproval
          ? "Codex CLI 已开始需审批检查工作区"
          : "Codex CLI 已开始完全访问工作区",
    }),
    system(2, "subagent_activity", { phase: "requesting", detail: "已连接 Codex CLI，正在准备工作区" }),
    {
      id: `${storage}:3`, branch_id: "main", kind: "message", role: "assistant",
      text: active
        ? "我先核对任务队列的调度入口，再沿取消信号检查共享状态何时释放。"
        : "我先沿共享状态的获取顺序",
    },
    ...(!active ? [
      { id: `${storage}:4`, branch_id: "main", kind: "message", role: "assistant", text: "做只读检查，再用定向测试确认" } as SessionMessage,
      { id: `${storage}:5`, branch_id: "main", kind: "message", role: "assistant", text: "哪些跨 `await` 的持锁点会形成竞争窗口。" } as SessionMessage,
    ] : []),
    {
      id: `${storage}:6`, branch_id: "main", kind: "tool_call", tool_name: "Codex 命令",
      call_id: "mock-call-1", input_json: JSON.stringify({ summary: "rg -n 'Mutex|RwLock|await' crates" }),
    },
    {
      id: `${storage}:7`, branch_id: "main", kind: "tool_result", call_id: "mock-call-1",
      output_json: JSON.stringify({ status: "completed", output: "crates/r-code-agent-worker/src/supervisor.rs:188\ncrates/r-code-agent-worker/src/llm_runtime.rs:1472" }), is_error: false,
    },
    ...(active ? [
      system(8, "subagent_activity", { phase: "requesting", detail: "Codex CLI 正在分析工作区" }),
    ] : [
      { id: `${storage}:8`, branch_id: "main", kind: "message", role: "assistant", text: "第一处发生在调度器持有写锁后等待 worker 回执；" } as SessionMessage,
      { id: `${storage}:9`, branch_id: "main", kind: "message", role: "assistant", text: "第二处发生在取消分支更新状态时。" } as SessionMessage,
      { id: `${storage}:10`, branch_id: "main", kind: "message", role: "assistant", text: "两者都可以先复制必要字段，再释放锁进入异步等待。" } as SessionMessage,
      {
        id: `${storage}:11`, branch_id: "main", kind: "tool_call", tool_name: "Codex 命令",
        call_id: "mock-call-2", input_json: JSON.stringify({ summary: "cargo test -p r-code-agent-worker supervisor" }),
      } as SessionMessage,
      {
        id: `${storage}:12`, branch_id: "main", kind: "tool_result", call_id: "mock-call-2",
        output_json: JSON.stringify({ status: "failed", output: "error: transient lock timeout" }), is_error: true,
      } as SessionMessage,
      {
        id: `${storage}:13`, branch_id: "main", kind: "tool_call", tool_name: "Codex 命令",
        call_id: "mock-call-3", input_json: JSON.stringify({ summary: "cargo test -p r-code-agent-worker --lib supervisor" }),
      } as SessionMessage,
      {
        id: `${storage}:14`, branch_id: "main", kind: "tool_result", call_id: "mock-call-3",
        output_json: JSON.stringify({ status: "completed", output: "test result: ok. 8 passed; 0 failed" }), is_error: false,
      } as SessionMessage,
      {
        id: `${storage}:15`, branch_id: "main", kind: "message", role: "assistant",
        text: "只读核对已完成：\n\n- 两处竞争窗口都来自持锁跨 `await`。\n- 现有定向测试全部通过。\n- 建议把状态快照移出临界区，并保留取消状态的原子更新。",
      } as SessionMessage,
      system(16, "subagent_lifecycle", {
        scope: {
          run_id: subagentId,
          agent_kind: "subagent",
          access_mode: accessMode,
          require_approval: requireApproval,
        },
        state: "completed",
        detail: "锁顺序核对完成",
      }),
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
  const projectTasks = browserMockTasks.filter((task) => task.workspace_path === workspace.canonical_path);
  const archived = projectTasks
    .filter((task) => task.state === "archived")
    .sort((left, right) => right.updated_at.localeCompare(left.updated_at));
  const tasks = projectTasks.filter((task) => task.state !== "archived").map(mockTaskSummary);
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
  return {
    workspace,
    generated_at: new Date().toISOString(),
    metrics: {
      task_count: tasks.length,
      archived_task_count: archived.length,
      pending_permission_count: attention.filter((item) => item.kind === "permission").length,
      review_ready_count: attention.filter((item) => item.kind === "review_ready").length,
      running_task_count: tasks.filter((item) => item.pending_permission_count === 0 && item.active_run?.ended_at === null).length,
      active_subagent_count: tasks.reduce((count, item) => count + (browserMockDetails[item.task.id]?.runs.filter((run) => run.agent_kind === "subagent" && run.ended_at === null).length ?? 0), 0),
    },
    tasks,
    attention,
    archived,
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
    .filter((task) => task.state !== "archived" && (!workspacePath || task.workspace_path === workspacePath))
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
    access_mode: "read_only", require_approval: false, routing_reason: "会话已选择 R-Code 主 Agent",
  });
  detail.events.push({ id: detail.events.length + 1, task_id: taskId, branch_id: "main", event_type: "change_requested", created_at: updatedAt });
  const reviewNotification = browserMockNotifications.find((item) => item.task_id === taskId && item.kind === "review_ready");
  if (reviewNotification) reviewNotification.read_at = updatedAt;
}

export function shouldUseBrowserMock(): boolean {
  return typeof window !== "undefined" && !Reflect.has(window, "__TAURI_INTERNALS__");
}
