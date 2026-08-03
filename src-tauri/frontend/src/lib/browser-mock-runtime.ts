/**
 * 完整浏览器 Demo 的内存后端。
 *
 * 正式桌面端始终走 Tauri IPC；只有普通浏览器预览会进入这里。命令名与 IPC
 * 保持一一对应，使 demo 复用真实 React 页面、状态管理和交互逻辑，而不是另造
 * 一套容易过期的静态产品壳。
 */
import type {
  AgentRun,
  ChangeDiff,
  AttachmentInput,
  InferenceOptions,
  LegacyMemoryStatus,
  LogEntry,
  McpCredentialStatus,
  McpLaunchPreview,
  McpManagerSnapshot,
  McpMarketInstallRequest,
  McpMarketPage,
  McpServerView,
  McpUpsertRequest,
  MemoryEntryDraft,
  MemoryEntryEdit,
  MemoryOverview,
  MemoryReviewSettingsUpdate,
  AnswerPlanQuestionsInput,
  EnhancedReviewFileView,
  EnhancedReviewTarget,
  EnhancedReviewView,
  PlanRejectResult,
  PlanReviewDecision,
  PlanQuestionSet,
  PlanView,
  ProjectAccessMode,
  ProviderModelsInput,
  ProviderModelsResponse,
  ProviderSettingsInput,
  SearchMatch,
  SessionBranch,
  SessionMessage,
  Task,
  TaskDetail,
  TaskAgentEngine,
  TaskMode,
  TerminalInfo,
  VerificationRecord,
  UpdatePlanItemInput,
  Workspace,
  WorkspaceMemoryMode,
  WorkflowSkill,
  WorkflowSkillDraft,
} from "./types";
import {
  browserMockAbortSubagent,
  browserMockActivityList,
  browserMockChangeRequest,
  browserMockCodexCliPreferences as browserMockCliPreferences,
  browserMockCodexIntegrationStatus,
  browserMockDetails,
  browserMockFileEntries,
  browserMockFiles,
  browserMockInstallCodexCli as browserMockInstallCli,
  browserMockEnableCodexMcp as browserMockInstallMcp,
  browserMockInstallCodexSkill as browserMockInstallSkill,
  browserMockAuthenticateCodex as browserMockLogin,
  browserMockMarkAllNotificationsRead,
  browserMockMarkNotificationRead,
  browserMockMessages,
  browserMockNotifications,
  browserMockNotificationList,
  browserMockProviderCatalog,
  browserMockSaveCodexCliPreferences as browserMockSaveCliPreferences,
  browserMockSetMessages,
  browserMockSettings,
  browserMockSetupCodexCollaboration as browserMockSetupCollaboration,
  browserMockSubagentMessages,
  browserMockTasks,
  browserMockWorkspaces,
  browserMockWorkspaceDashboard,
} from "./mock-data";

type MockArgs = Record<string, unknown>;

let sequence = 0;
let mockMemoryOverview: MemoryOverview = {
  settings: {
    enabled: false,
    reviewer: null,
    trigger_every_turns: 10,
    explicit_remember_immediate: true,
    project_notification_mode: "on",
    version: 1,
    review_generation: 1,
    physical_cleanup_pending: false,
    updated_at: new Date().toISOString(),
  },
  global_entries: [],
  project_entries: [],
  pending_candidates: [],
  recent_jobs: [],
};
let mockMcpServers: McpServerView[] = [
  {
    id: "r-code-research",
    display_name: "R-Code 深度调研",
    description: "内置多来源证据收集；需要完整调研时由主 Agent 选择。",
    enabled: true,
    builtin: true,
    source: { kind: "builtin" },
    transport: { type: "builtin" },
    state: "stopped",
    tool_count: 3,
    launch_approved: true,
  },
];
const mockMcpCredentials = new Map<string, Set<string>>();

const mockMcpMarket: McpMarketPage = {
  servers: [{
    name: "io.example/demo-search",
    title: "Demo Search MCP",
    description: "浏览器 Demo 中的 Registry 条目。",
    version: "1.0.0",
    status: "active",
    is_latest: true,
    suggested_id: "demo-search",
    install_options: [{
      id: "npm:demo-search@1.0.0",
      label: "npm · demo-search@1.0.0",
      transport: {
        type: "stdio",
        package_kind: "npm",
        executable: "npx",
        args: ["-y", "demo-search@1.0.0"],
        environment: [{ name: "DEMO_TOKEN", description: "访问令牌", required: true, secret: true }],
      },
    }],
  }],
  stale: false,
  fetched_at: new Date().toISOString(),
  registry_preview: true,
  registry_unreviewed: true,
};
const defaultWorkflowSkills: WorkflowSkill[] = [
  {
    id: "builtin:skill-creator",
    name: "skill-creator",
    description: "创建并注册 R-Code 自定义 Skill。",
    instructions: "设计 Skill 后必须调用 save_skill 工具保存。",
    source: "builtin",
    enabled: true,
    overridden: false,
  },
  {
    id: "builtin:review-changes",
    name: "review-changes",
    description: "安全审核并接受任务变更。",
    instructions: "只审核当前任务路径。",
    source: "builtin",
    enabled: true,
    overridden: false,
  },
  {
    id: "builtin:git-commit-push",
    name: "git-commit-push",
    description: "提交并推送已接受的任务变更。",
    instructions: "不得 force push。",
    source: "builtin",
    enabled: true,
    overridden: false,
  },
];
let workflowSkills = defaultWorkflowSkills.map((skill) => ({ ...skill }));
const legacyMemoryStatusByWorkspace = new Map<string, LegacyMemoryStatus>([
  ["D:/project/rust/r-code", { exists: true, git_tracking: "tracked" }],
  ["D:/project/rust/api-server", { exists: true, git_tracking: "untracked" }],
  ["D:/project/rust/legacy-unknown", { exists: true, git_tracking: "unknown" }],
  ["D:/project/rust/legacy-deleted-tracked", { exists: false, git_tracking: "tracked" }],
  ["D:/project/rust/legacy-absent", { exists: false, git_tracking: "untracked" }],
]);
const verificationOutputs = new Map<string, string>();
const terminalOutputs = new Map<string, string>();
const terminalInputs = new Map<string, string>();
/** Browser QA mirrors the desktop's application-owned review ledger. */
const acceptedReviewPaths = new Map<string, Set<string>>();
const partiallyAcceptedReviewPaths = new Map<string, Set<string>>();
const rejectedReviewPaths = new Map<string, Set<string>>();
/** Git staging is an explicit delivery step and never changes review decisions. */
const stagedReviewPaths = new Map<string, Set<string>>();
/** Durable Plan state used by browser-only product demos and deterministic UI tests. */
const mockPlans = new Map<string, PlanView>();
const mockPlanQuestionSets = new Map<string, PlanQuestionSet>();
const mockPlanAnswerPayloads = new Map<string, string>();
const mockPlanReviewDecisions = new Map<string, PlanReviewDecision>();
let mockPlanSequence = 0;
const terminals: TerminalInfo[] = [
  { id: "demo-terminal-main", state: "idle", shell: "PowerShell", is_busy: false },
];

terminalOutputs.set(
  "demo-terminal-main",
  "R-Code Demo Terminal\r\nPowerShell 7 · D:\\project\\rust\\r-code\r\n\r\nPS D:\\project\\rust\\r-code> ",
);
terminalInputs.set("demo-terminal-main", "");

function copy<T>(value: T): T {
  if (value == null) return value;
  return JSON.parse(JSON.stringify(value)) as T;
}

function nowIso(): string {
  return new Date().toISOString();
}

function nextId(prefix: string): string {
  sequence += 1;
  return `demo-${prefix}-${Date.now().toString(36)}-${sequence}`;
}

function newWorkspaceId(): string {
  return globalThis.crypto.randomUUID().replaceAll("-", "");
}

function isWorkspaceMemoryMode(value: unknown): value is Workspace["memory_mode"] {
  return value === "inherit" || value === "read_only" || value === "off";
}

function normalizeWorkspace(workspace: Workspace): Workspace {
  const legacy = workspace as unknown as {
    id?: unknown;
    memory_mode?: unknown;
    memory_generation?: unknown;
  };

  if (legacy.id === undefined) workspace.id = newWorkspaceId();
  else if (typeof legacy.id !== "string" || legacy.id.length === 0) throw new Error("Demo workspace id 无效");

  if (legacy.memory_mode === undefined) workspace.memory_mode = "inherit";
  else if (!isWorkspaceMemoryMode(legacy.memory_mode)) {
    throw new Error(`Demo workspace memory_mode 无效: ${String(legacy.memory_mode)}`);
  }

  if (legacy.memory_generation === undefined) workspace.memory_generation = 1;
  else if (
    typeof legacy.memory_generation !== "number"
    || !Number.isSafeInteger(legacy.memory_generation)
    || legacy.memory_generation < 1
  ) {
    throw new Error("Demo workspace memory_generation 无效");
  }

  return workspace;
}

function stringArg(args: MockArgs, key: string): string {
  const value = args[key];
  if (typeof value !== "string") throw new Error(`Demo 参数 ${key} 无效`);
  return value;
}

function optionalStringArg(args: MockArgs, key: string): string | null {
  const value = args[key];
  return typeof value === "string" && value.length > 0 ? value : null;
}

function taskById(taskId: string): Task {
  const task = browserMockTasks.find((item) => item.id === taskId);
  if (!task) throw new Error(`Demo 中不存在任务 ${taskId}`);
  return task;
}

function detailById(taskId: string): TaskDetail {
  const detail = browserMockDetails[taskId];
  if (!detail) throw new Error(`Demo 中不存在任务详情 ${taskId}`);
  return detail;
}

function workspaceByPath(path: string): Workspace {
  const workspace = browserMockWorkspaces.find((item) => item.canonical_path === path);
  if (!workspace) throw new Error(`Demo 中不存在项目 ${path}`);
  return normalizeWorkspace(workspace);
}

function addEvent(detail: TaskDetail, eventType: TaskDetail["events"][number]["event_type"]): void {
  detail.events.push({
    id: Math.max(0, ...detail.events.map((event) => event.id)) + 1,
    task_id: detail.task.id,
    branch_id: detail.active_branch.id,
    event_type: eventType,
    created_at: nowIso(),
  });
}

function touchTask(task: Task): void {
  task.updated_at = nowIso();
}

function markTaskNotificationsRead(taskId: string): void {
  const notifications = browserMockNotificationList(false).notifications;
  for (const notification of notifications) {
    if (notification.task_id === taskId) browserMockMarkNotificationRead(notification.id);
  }
}

function createTask(args: MockArgs): Task {
  const createdAt = nowIso();
  const workspacePath = optionalStringArg(args, "workspacePath");
  const providerName = optionalStringArg(args, "providerName") ?? browserMockSettings.config.default_provider ?? "openai";
  const provider = browserMockSettings.config.providers?.[providerName];
  const agentEngine = (optionalStringArg(args, "agentEngine") ??
    browserMockSettings.config.orchestration?.default_agent_engine ??
    "r_code") as TaskAgentEngine;
  const task: Task = {
    id: nextId("task"),
    workspace_path: workspacePath,
    provider_name: providerName,
    agent_engine: agentEngine,
    model: provider?.model ?? null,
    inference: {},
    title: stringArg(args, "title") || "新对话",
    goal: stringArg(args, "goal"),
    mode: (args.mode as TaskMode | undefined) ?? (workspacePath ? "edit" : "ask"),
    state: "exploring",
    worktree_path: null,
    created_at: createdAt,
    updated_at: createdAt,
  };
  const branch: SessionBranch = {
    id: "main",
    task_id: task.id,
    parent_branch_id: null,
    forked_from_message_id: null,
    storage_id: "main",
    is_active: true,
    created_at: createdAt,
  };
  browserMockTasks.unshift(task);
  browserMockDetails[task.id] = {
    task,
    active_branch: branch,
    branches: [],
    runs: [],
    events: [{ id: 1, task_id: task.id, branch_id: "main", event_type: "task_created", created_at: createdAt }],
    changes: [],
    permissions: [],
    verifications: [],
    queued_messages: [],
  };
  browserMockSetMessages(task.id, []);
  return task;
}

function sendMessage(args: MockArgs): void {
  const taskId = stringArg(args, "taskId");
  const message = stringArg(args, "message").trim();
  const attachments = Array.isArray(args.attachments)
    ? args.attachments.filter((item): item is AttachmentInput => Boolean(
        item && typeof item === "object" && typeof (item as AttachmentInput).mediaType === "string",
      ))
    : [];
  const mode = typeof args.mode === "string" ? args.mode : "auto";
  if (!message && attachments.length === 0) return;
  const task = taskById(taskId);
  const detail = detailById(taskId);
  const timestamp = nowIso();
  const messages = browserMockMessages(taskId);

  if (mode === "queue") {
    detail.queued_messages.push({
      id: nextId("queue"),
      task_id: taskId,
      branch_id: detail.active_branch.id,
      message,
      state: "queued",
      priority: 0,
      created_at: timestamp,
      updated_at: timestamp,
    });
    addEvent(detail, "user_message_queued");
    touchTask(task);
    return;
  }

  if (task.mode === "plan") requestMockPlanQuestions(taskId);
  const planNeedsInput = currentMockPlan(taskId)?.plan.state === "awaiting_input";

  for (const run of detail.runs) {
    if (run.ended_at == null) {
      run.ended_at = timestamp;
      run.review_state = mode === "send_now" ? "aborted" : "answered";
    }
  }

  const run: AgentRun = {
    id: nextId("run"),
    task_id: taskId,
    branch_id: detail.active_branch.id,
    parent_run_id: null,
    agent_kind: "main",
    agent_label: "主代理",
    summary: task.mode === "plan"
      ? planNeedsInput ? "计划需要补充信息" : "计划草案已准备好，等待用户确认"
      : task.workspace_path ? "已完成代码检查并准备变更" : "已完成本轮回答",
    delegated_by_tool_call_id: null,
    model: task.model ?? "gpt-5.6",
    runtime_kind: task.agent_engine === "codex" ? "codex_exec" : "native",
    access_mode: "read_only",
    routing_reason: task.agent_engine === "codex"
      ? "该会话已选择 Codex 主 Agent"
      : "该会话已选择 R-Code 主 Agent",
    external_session_id: null,
    review_state: task.mode === "plan" ? "answered" : task.workspace_path ? "pending" : "answered",
    started_at: timestamp,
    ended_at: timestamp,
    usage_json: JSON.stringify({ input_tokens: 860, output_tokens: 420 }),
  };
  detail.runs.unshift(run);
  messages.push(
    {
      id: nextId("message"),
      branch_id: detail.active_branch.id,
      kind: "message",
      role: "user",
      text: message || undefined,
      image_count: attachments.filter((attachment) => attachment.mediaType.startsWith("image/")).length,
      image_media_types: attachments
        .filter((attachment) => attachment.mediaType.startsWith("image/"))
        .map((attachment) => attachment.mediaType),
      attachments: attachments.map((attachment) => ({
        name: attachment.name,
        media_type: attachment.mediaType,
        kind: attachment.mediaType.startsWith("image/")
          ? "image"
          : attachment.mediaType === "application/pdf"
            ? "pdf"
            : "text",
      })),
      timestamp,
    },
    {
      id: nextId("message"),
      branch_id: detail.active_branch.id,
      kind: "message",
      role: "assistant",
      text: task.mode === "plan"
        ? planNeedsInput
          ? "在整理计划前还需要你确认两项边界；回答下方问题后我会继续生成计划。"
          : "计划草案已整理为可独立验收的功能项。确认后即可按依赖顺序实施。"
        : task.workspace_path
        ? "已完成这轮演示任务：我检查了相关文件、整理了修改，并把结果放到右侧 Changes 与 Review 中供你继续操作。"
        : "这是浏览器 Demo 的完整会话回复。你可以继续追问、使用斜杠命令，或从左侧切换到其他产品场景。",
      timestamp,
    },
  );
  if (task.mode !== "plan" && task.workspace_path && detail.changes.length === 0) {
    detail.changes.push({
      id: nextId("change"),
      task_id: taskId,
      tool_call_id: null,
      path: "README.md",
      change_type: "modify",
      before_hash: "demo-before",
      after_hash: "demo-after",
      old_path: null,
      created_at: timestamp,
    });
  }
  task.state = task.mode === "plan" ? "idle" : task.workspace_path ? "review_ready" : "idle";
  touchTask(task);
  addEvent(detail, "run_started");
  addEvent(detail, "run_ended");
}

function setTaskField(args: MockArgs, field: "workspace_path" | "provider_name" | "agent_engine" | "model" | "title"): Task {
  const task = taskById(stringArg(args, "taskId"));
  if (field === "workspace_path") task.workspace_path = optionalStringArg(args, "workspacePath");
  if (field === "provider_name") task.provider_name = stringArg(args, "providerName");
  if (field === "agent_engine") task.agent_engine = stringArg(args, "agentEngine") as TaskAgentEngine;
  if (field === "model") task.model = optionalStringArg(args, "model");
  if (field === "title") task.title = stringArg(args, "title").trim() || task.title;
  touchTask(task);
  return task;
}

function integerArg(args: MockArgs, key: string): number {
  const value = args[key];
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
    throw new Error(`Demo 参数 ${key} 无效`);
  }
  return value;
}

function updateTaskGoal(args: MockArgs): Task {
  const task = taskById(stringArg(args, "taskId"));
  task.goal = stringArg(args, "goal").trim();
  touchTask(task);
  const view = mockPlans.get(task.id);
  if (view) {
    view.goal.goal = task.goal;
    view.goal.updated_at = task.updated_at;
  }
  return task;
}

function setTaskMode(args: MockArgs): Task {
  const task = taskById(stringArg(args, "taskId"));
  const mode = stringArg(args, "mode") as TaskMode;
  if (!(["ask", "edit", "auto", "plan"] satisfies TaskMode[]).includes(mode)) {
    throw new Error(`Demo 任务模式无效: ${mode}`);
  }
  if (mode === "plan" && task.agent_engine !== "r_code") {
    throw new Error("计划模式需要使用 R-Code 主 Agent");
  }
  task.mode = mode;
  touchTask(task);
  return task;
}

function syncMockPlanGoal(view: PlanView): PlanView {
  const task = taskById(view.plan.task_id);
  view.goal = {
    task_id: task.id,
    goal: task.goal,
    updated_at: task.updated_at,
  };
  return view;
}

function currentMockPlan(taskId: string): PlanView | null {
  const view = mockPlans.get(taskId);
  return view ? syncMockPlanGoal(view) : null;
}

function createMockPlan(taskId: string): PlanView {
  const existing = mockPlans.get(taskId);
  if (existing && !["completed", "cancelled"].includes(existing.plan.state)) {
    throw new Error("该会话已经有一个进行中的计划");
  }
  const task = taskById(taskId);
  const timestamp = nowIso();
  mockPlanSequence += 1;
  const planId = `demo-plan-${taskId}-${mockPlanSequence}`;
  const view: PlanView = {
    plan: {
      id: planId,
      task_id: taskId,
      revision: 1,
      state: "draft",
      approved_revision: null,
      projection_path: `R-Code/Plans/${planId}/plan.md`,
      projection_revision: 1,
      projection_error: null,
      created_at: timestamp,
      updated_at: timestamp,
      approved_at: null,
      implementation_dispatch_state: "not_requested",
      implementation_dispatch_error: null,
      implementation_queue_message_id: null,
      implementation_dispatched_at: null,
    },
    goal: { task_id: taskId, goal: task.goal, updated_at: task.updated_at },
    items: [],
    pending_question_set: null,
    continuation_question_set: null,
  };
  mockPlans.set(taskId, view);
  return view;
}

function requestMockPlanQuestions(taskId: string): void {
  const view = currentMockPlan(taskId);
  if (!view || view.plan.state !== "draft" || view.items.length > 0 || view.pending_question_set) return;
  const timestamp = nowIso();
  const revision = view.plan.revision + 1;
  const setId = `${view.plan.id}-questions-${revision}`;
  const set: PlanQuestionSet = {
    id: setId,
    plan_id: view.plan.id,
    revision,
    state: "pending",
    answer_idempotency_key: null,
    continuation_state: "not_requested",
    continuation_error: null,
    questions: [
      {
        id: `${setId}-scope`,
        question_set_id: setId,
        ordinal: 1,
        header: "实现范围",
        question: "这轮计划应优先覆盖哪个范围？",
        options: [
          { id: "focused", label: "聚焦核心流程", description: "先交付主路径与必要验证。" },
          { id: "complete", label: "完整交付", description: "同时覆盖边界、迁移与文档。" },
        ],
        answer: null,
        answered_at: null,
      },
      {
        id: `${setId}-validation`,
        question_set_id: setId,
        ordinal: 2,
        header: "验收方式",
        question: "你希望以什么作为主要验收依据？",
        options: [
          { id: "automated", label: "自动化测试", description: "优先使用可重复运行的测试。" },
          { id: "manual", label: "交互验收", description: "优先验证实际界面与操作流程。" },
          { id: "both", label: "两者结合", description: "自动化回归与人工体验同时覆盖。" },
        ],
        answer: null,
        answered_at: null,
      },
    ],
    created_at: timestamp,
    resolved_at: null,
    dispatched_at: null,
  };
  view.plan.revision = revision;
  view.plan.state = "awaiting_input";
  view.plan.updated_at = timestamp;
  view.plan.projection_revision = revision;
  view.pending_question_set = set;
  mockPlanQuestionSets.set(setId, set);
}

/** Browser chat simulates the trusted runtime publishing a small feature Plan. */
function publishMockPlan(taskId: string, request: string): void {
  const view = currentMockPlan(taskId);
  if (!view || view.plan.state !== "draft" || view.items.length > 0) return;
  const timestamp = nowIso();
  const revision = view.plan.revision + 1;
  const firstId = `${view.plan.id}-feature-1`;
  const secondId = `${view.plan.id}-feature-2`;
  view.plan.revision = revision;
  view.plan.state = "ready";
  view.plan.projection_revision = revision;
  view.plan.updated_at = timestamp;
  view.items = [
    {
      id: firstId,
      plan_id: view.plan.id,
      revision,
      ordinal: 1,
      title: "明确实现边界",
      description: request.trim() || "确认目标、约束与验收标准。",
      state: "proposed",
      depends_on: [],
      created_at: timestamp,
      updated_at: timestamp,
      started_at: null,
      completed_at: null,
    },
    {
      id: secondId,
      plan_id: view.plan.id,
      revision,
      ordinal: 2,
      title: "实现并验证功能",
      description: "完成实现、测试与交付检查。",
      state: "proposed",
      depends_on: [firstId],
      created_at: timestamp,
      updated_at: timestamp,
      started_at: null,
      completed_at: null,
    },
  ];
}

function dispatchMockPlanContinuation(taskId: string, questionSet: PlanQuestionSet): void {
  const view = currentMockPlan(taskId);
  if (!view || view.continuation_question_set?.id !== questionSet.id) return;
  questionSet.continuation_state = "dispatching";
  questionSet.continuation_error = null;
  window.setTimeout(() => {
    const current = currentMockPlan(taskId);
    if (!current || current.continuation_question_set?.id !== questionSet.id) return;
    questionSet.continuation_state = "dispatched";
    questionSet.dispatched_at = nowIso();
    current.continuation_question_set = null;
    publishMockPlan(taskId, taskById(taskId).goal || "根据已确认的边界生成实现计划");
  }, 240);
}

function answerMockPlan(taskId: string, input: AnswerPlanQuestionsInput): PlanView {
  const view = currentMockPlan(taskId);
  if (!view) throw new Error("该会话没有进行中的计划");
  const set = mockPlanQuestionSets.get(input.question_set_id);
  if (!set || set.plan_id !== view.plan.id) throw new Error("问题集不存在或不属于该会话");
  const serialized = JSON.stringify(input);
  if (set.state !== "pending") {
    if (mockPlanAnswerPayloads.get(set.id) === serialized) return view;
    throw new Error("问题集已经由另一份回答处理");
  }
  if (view.plan.revision !== input.expected_revision || set.revision !== input.expected_revision) {
    throw new Error("计划已经更新，请刷新后重试");
  }
  if (input.skip_all) {
    if (input.answers.length !== 0) throw new Error("跳过整个问题集时不能同时提交回答");
    set.state = "skipped";
  } else {
    const answers = new Map(input.answers.map((answer) => [answer.question_id, answer]));
    if (answers.size !== set.questions.length) throw new Error("必须完整回答当前问题集");
    for (const question of set.questions) {
      const answer = answers.get(question.id);
      if (!answer) throw new Error("必须完整回答当前问题集");
      if (answer.kind === "option") {
        if (!question.options.some((option) => option.id === answer.option_id)) {
          throw new Error(`选项不属于问题 ${question.id}`);
        }
        question.answer = { kind: "option", option_id: answer.option_id };
      } else {
        if (!answer.text.trim()) throw new Error("自定义回答不能为空");
        question.answer = { kind: "free_form", text: answer.text.trim() };
      }
      question.answered_at = nowIso();
    }
    set.state = "answered";
  }
  const timestamp = nowIso();
  set.answer_idempotency_key = input.idempotency_key;
  set.continuation_state = "pending";
  set.resolved_at = timestamp;
  mockPlanAnswerPayloads.set(set.id, serialized);
  view.pending_question_set = null;
  view.continuation_question_set = set;
  view.plan.revision += 1;
  view.plan.state = "draft";
  view.plan.projection_revision = view.plan.revision;
  view.plan.updated_at = timestamp;
  dispatchMockPlanContinuation(taskId, set);
  return view;
}

function retryMockPlanContinuation(taskId: string, questionSetId: string): PlanView {
  const view = currentMockPlan(taskId);
  if (!view) throw new Error("该会话没有进行中的计划");
  const set = mockPlanQuestionSets.get(questionSetId);
  if (!set || set.plan_id !== view.plan.id || set.state === "pending") {
    throw new Error("没有可重试的计划续接");
  }
  if (set.continuation_state === "failed" || set.continuation_state === "pending") {
    view.continuation_question_set = set;
    set.continuation_state = "dispatching";
    set.continuation_error = null;
    dispatchMockPlanContinuation(taskId, set);
  }
  return view;
}

function approveMockPlan(taskId: string, planId: string, expectedRevision: number): PlanView {
  const view = currentMockPlan(taskId);
  if (!view || view.plan.id !== planId) throw new Error("计划不存在或不属于该会话");
  if (
    view.plan.approved_revision === expectedRevision
    && ["approved", "executing", "completed"].includes(view.plan.state)
  ) return view;
  if (view.plan.revision !== expectedRevision) throw new Error("计划已经更新，请刷新后重试");
  if (view.plan.state !== "ready" || view.items.length === 0) throw new Error("计划尚未准备好");
  const timestamp = nowIso();
  view.plan.approved_revision = expectedRevision;
  view.plan.revision += 1;
  view.plan.state = "executing";
  view.plan.approved_at = timestamp;
  view.plan.updated_at = timestamp;
  view.plan.projection_revision = view.plan.revision;
  view.plan.implementation_dispatch_state = "dispatched";
  view.plan.implementation_dispatch_error = null;
  view.plan.implementation_queue_message_id = `plan-implementation:${planId}:${expectedRevision}`;
  view.plan.implementation_dispatched_at = timestamp;
  for (const item of view.items) {
    item.state = item.depends_on.length === 0 ? "in_progress" : "pending";
    item.updated_at = timestamp;
    item.started_at = item.state === "in_progress" ? timestamp : null;
  }
  return view;
}

function retryMockPlanImplementation(taskId: string, planId: string): PlanView {
  const view = currentMockPlan(taskId);
  if (!view || view.plan.id !== planId || view.plan.state !== "executing") {
    throw new Error("没有可重试的计划实施请求");
  }
  if (view.plan.implementation_dispatch_state === "failed") {
    view.plan.implementation_dispatch_state = "dispatched";
    view.plan.implementation_dispatch_error = null;
    view.plan.implementation_dispatched_at = nowIso();
  }
  return view;
}

function cancelMockPlan(taskId: string, planId: string, expectedRevision: number): PlanView {
  const view = currentMockPlan(taskId);
  if (!view || view.plan.id !== planId) throw new Error("计划不存在或不属于该会话");
  if (view.plan.state === "cancelled") return view;
  if (view.plan.revision !== expectedRevision) throw new Error("计划已经更新，请刷新后重试");
  if (view.plan.state === "completed") throw new Error("已完成的计划不能取消");
  const timestamp = nowIso();
  view.plan.revision += 1;
  view.plan.state = "cancelled";
  view.plan.updated_at = timestamp;
  view.plan.implementation_dispatch_state = "not_requested";
  view.plan.implementation_dispatch_error = null;
  view.plan.implementation_dispatched_at = null;
  for (const item of view.items) {
    if (!["completed", "failed", "cancelled"].includes(item.state)) {
      item.state = "cancelled";
      item.completed_at = timestamp;
      item.updated_at = timestamp;
    }
  }
  const task = taskById(taskId);
  task.mode = task.workspace_path ? "edit" : "ask";
  touchTask(task);
  return view;
}

function updateMockPlanItem(taskId: string, input: UpdatePlanItemInput): PlanView {
  const view = currentMockPlan(taskId);
  if (!view || view.plan.id !== input.plan_id) throw new Error("计划不存在或不属于该会话");
  if (view.plan.revision !== input.expected_revision) throw new Error("计划已经更新，请刷新后重试");
  if (view.plan.state !== "executing") throw new Error("计划当前没有在执行");
  const item = view.items.find((candidate) => candidate.id === input.item_id);
  if (!item) throw new Error("计划功能项不存在");
  const timestamp = nowIso();
  item.state = input.state;
  item.updated_at = timestamp;
  if (input.state === "in_progress" && !item.started_at) item.started_at = timestamp;
  if (input.state === "completed") item.completed_at = timestamp;
  if (input.state === "completed") {
    const completed = new Set(view.items.filter((candidate) => candidate.state === "completed").map((candidate) => candidate.id));
    const next = view.items.find((candidate) =>
      candidate.state === "pending" && candidate.depends_on.every((dependency) => completed.has(dependency))
    );
    if (next) {
      next.state = "in_progress";
      next.started_at = timestamp;
      next.updated_at = timestamp;
    }
  }
  view.plan.revision += 1;
  view.plan.state = view.items.every((candidate) => candidate.state === "completed") ? "completed" : "executing";
  view.plan.updated_at = timestamp;
  view.plan.projection_revision = view.plan.revision;
  return view;
}

function repairMockPlanProjection(taskId: string, planId: string): PlanView {
  const view = currentMockPlan(taskId);
  if (!view || view.plan.id !== planId) throw new Error("计划不存在或不属于该会话");
  view.plan.projection_error = null;
  view.plan.projection_revision = view.plan.revision;
  view.plan.updated_at = nowIso();
  return view;
}

function mockPlanReviewDecisionKey(planId: string, itemId: string, path: string | null): string {
  return `${planId}:${itemId}:${path ?? "@feature"}`;
}

function mockEnhancedReviewFiles(
  view: PlanView,
  itemId: string,
  ordinal: number,
): EnhancedReviewFileView[] {
  const firstPath = ordinal === 1 ? "src/plan-mode.ts" : "src/shared-plan.ts";
  const sharedPath = "src/shared-plan.ts";
  const textPatch = ordinal === 1
    ? "diff --git a/src/plan-mode.ts b/src/plan-mode.ts\n@@ -0,0 +1,3 @@\n+export const planMode = true;\n+export const askHuman = true;\n+export const planRevision = 1;"
    : "diff --git a/src/shared-plan.ts b/src/shared-plan.ts\n@@ -2,3 +2,4 @@\n export const owner = 'plan';\n+export const enhancedReview = true;\n export const stable = true;";
  const sharedPatch = ordinal === 1
    ? "diff --git a/src/shared-plan.ts b/src/shared-plan.ts\n@@ -0,0 +1,2 @@\n+export const owner = 'plan';\n+export const stable = true;"
    : textPatch;
  const createFile = (path: string, sequence: number, patch: string | null, binary = false): EnhancedReviewFileView => ({
    path,
    decision: mockPlanReviewDecisions.get(mockPlanReviewDecisionKey(view.plan.id, itemId, path))?.decision ?? null,
    first_sequence: sequence,
    last_sequence: sequence,
    events: [{
      sequence,
      event_id: `${itemId}-event-${sequence}`,
      tool_call_id: `${itemId}-tool-${sequence}`,
      before_exists: ordinal !== 1 || path === sharedPath,
      after_exists: true,
      before_blob_hash: binary ? "demo-binary-before" : "demo-text-before",
      after_blob_hash: binary ? "demo-binary-after" : "demo-text-after",
      patch,
      binary,
    }],
  });
  const files = [createFile(firstPath, ordinal * 10, textPatch)];
  if (firstPath !== sharedPath) files.push(createFile(sharedPath, ordinal * 10 + 1, sharedPatch));
  if (ordinal === 2) files.push(createFile("assets/plan-preview.bin", ordinal * 10 + 2, null, true));
  return files;
}

function currentMockEnhancedReview(taskId: string): EnhancedReviewView | null {
  const view = currentMockPlan(taskId);
  if (!view || view.items.length === 0) return null;
  return {
    task_id: taskId,
    plan_id: view.plan.id,
    plan_revision: view.plan.revision,
    groups: view.items.map((item) => ({
      item_id: item.id,
      ordinal: item.ordinal,
      title: item.title,
      description: item.description,
      state: item.state,
      decision: mockPlanReviewDecisions.get(mockPlanReviewDecisionKey(view.plan.id, item.id, null))?.decision ?? null,
      files: mockEnhancedReviewFiles(view, item.id, item.ordinal),
    })),
  };
}

function mockPlanReviewTarget(args: MockArgs): EnhancedReviewTarget {
  const target = args.target as EnhancedReviewTarget | undefined;
  if (!target?.task_id || !target.plan_id || !target.item_id) throw new Error("增强审核目标不完整");
  return target;
}

function decideMockPlanReview(
  target: EnhancedReviewTarget,
  decision: "accepted" | "rejected",
  scope: "feature" | "file",
): PlanReviewDecision {
  const view = currentMockEnhancedReview(target.task_id);
  if (!view || view.plan_id !== target.plan_id) throw new Error("计划审核版本已经失效");
  const group = view.groups.find((candidate) => candidate.item_id === target.item_id);
  if (!group) throw new Error("计划功能项不存在");
  if (!(["blocked", "completed", "failed", "cancelled"] as const).includes(group.state as never)) {
    throw new Error("功能仍在实施，暂时不能接受或拒绝");
  }
  if (scope === "file" && (!target.path || !group.files.some((file) => file.path === target.path))) {
    throw new Error("计划审核文件不存在");
  }
  const key = mockPlanReviewDecisionKey(target.plan_id, target.item_id, scope === "file" ? target.path : null);
  const existing = mockPlanReviewDecisions.get(key);
  if (existing?.decision === decision) return existing;
  const result: PlanReviewDecision = {
    id: nextId("plan-review"),
    plan_id: target.plan_id,
    plan_revision: view.plan_revision,
    item_id: target.item_id,
    scope,
    path: scope === "file" ? target.path : null,
    decision,
    decided_at: nowIso(),
  };
  mockPlanReviewDecisions.set(key, result);
  if (scope === "feature") {
    for (const file of group.files) {
      mockPlanReviewDecisions.set(mockPlanReviewDecisionKey(target.plan_id, target.item_id, file.path), {
        ...result,
        id: nextId("plan-review-file"),
        scope: "file",
        path: file.path,
      });
    }
  }
  return result;
}

function rejectMockPlanReview(target: EnhancedReviewTarget, scope: "feature" | "file"): PlanRejectResult {
  const decision = decideMockPlanReview(target, "rejected", scope);
  const view = currentMockEnhancedReview(target.task_id);
  const group = view?.groups.find((candidate) => candidate.item_id === target.item_id);
  return {
    operation_id: nextId("plan-reject"),
    decision,
    changed_paths: scope === "file" ? [target.path!].filter(Boolean) : group?.files.map((file) => file.path) ?? [],
    idempotent: false,
  };
}

function forkTask(taskId: string, messageId: string | null = null): SessionBranch {
  const detail = detailById(taskId);
  detail.active_branch.is_active = false;
  const branch: SessionBranch = {
    id: nextId("branch"),
    task_id: taskId,
    parent_branch_id: detail.active_branch.id,
    forked_from_message_id: messageId,
    storage_id: nextId("storage"),
    is_active: true,
    created_at: nowIso(),
  };
  detail.branches = [...detail.branches.filter((item) => item.id !== detail.active_branch.id), detail.active_branch, branch];
  detail.active_branch = branch;
  addEvent(detail, "session_branched");
  return branch;
}

function abortTask(taskId: string): void {
  const task = taskById(taskId);
  const detail = detailById(taskId);
  const endedAt = nowIso();
  for (const run of detail.runs) {
    if (run.ended_at == null) {
      run.ended_at = endedAt;
      run.review_state = "aborted";
      run.summary = "已由用户停止";
    }
  }
  task.state = "interrupted";
  touchTask(task);
  addEvent(detail, "run_aborted");
}

function delegateTask(args: MockArgs, runtime: AgentRun["runtime_kind"]): AgentRun {
  const taskId = stringArg(args, "taskId");
  const detail = detailById(taskId);
  const timestamp = nowIso();
  const run: AgentRun = {
    id: nextId("subagent"),
    task_id: taskId,
    branch_id: detail.active_branch.id,
    parent_run_id: detail.runs.find((item) => item.agent_kind === "main")?.id ?? null,
    agent_kind: "subagent",
    agent_label: optionalStringArg(args, "label") ?? "Codex 调查",
    summary: `已完成 Codex 调查：${stringArg(args, "goal")}`,
    delegated_by_tool_call_id: nextId("delegate"),
    model: "gpt-5.6-sol",
    runtime_kind: runtime,
    access_mode: "read_only",
    routing_reason: "浏览器 Demo 中显式委派给 Codex",
    external_session_id: nextId("session"),
    review_state: "answered",
    started_at: timestamp,
    ended_at: timestamp,
    usage_json: null,
  };
  detail.runs.push(run);
  addEvent(detail, "subagent_started");
  addEvent(detail, "subagent_finished");
  return run;
}

function approvePermission(args: MockArgs): void {
  const requestId = stringArg(args, "requestId");
  for (const detail of Object.values(browserMockDetails)) {
    const permission = detail.permissions.find((item) => item.id === requestId);
    if (!permission) continue;
    permission.decision = args.decision as typeof permission.decision;
    permission.decided_at = nowIso();
    touchTask(detail.task);
    addEvent(detail, "permission_decided");
    markTaskNotificationsRead(detail.task.id);
    return;
  }
  throw new Error(`Demo 中不存在权限请求 ${requestId}`);
}

function diffFor(path: string): ChangeDiff {
  const current = browserMockFiles[path]?.content ?? "pub fn demo() {\n    println!(\"R-Code\");\n}\n";
  const first = current.split("\n")[0] || "";
  return {
    supported: true,
    path,
    change_type: "modify",
    before_hash: "demo-before",
    after_hash: "demo-after",
    lines: [
      { kind: "hunk", text: "@@ -1,3 +1,4 @@" },
      { kind: "ctx", text: first, old_no: 1, new_no: 1 },
      { kind: "del", text: "// previous demo implementation", old_no: 2, line_id: "del:2:demo-previous" },
      { kind: "add", text: "// complete interactive demo implementation", new_no: 2, line_id: "add:2:demo-complete" },
      { kind: "add", text: "// shares the production React UI", new_no: 3, line_id: "add:3:demo-shared" },
    ],
    truncated: false,
  };
}

function reviewChanges(taskId: string) {
  const excluded = new Set(
    (globalThis as { __rCodeBrowserMockExcludedReviewPaths?: string[] })
      .__rCodeBrowserMockExcludedReviewPaths ?? [],
  );
  return detailById(taskId).changes.filter((change) => !excluded.has(change.path));
}

function runVerification(args: MockArgs): VerificationRecord {
  const taskId = stringArg(args, "taskId");
  const detail = detailById(taskId);
  const timestamp = nowIso();
  const record: VerificationRecord = {
    id: nextId("verification"),
    task_id: taskId,
    run_id: detail.runs[0]?.id ?? "demo-run",
    command: stringArg(args, "command"),
    status: "passed",
    output_blob_key: nextId("verification-output"),
    exit_code: 0,
    started_at: timestamp,
    ended_at: timestamp,
  };
  detail.verifications.push(record);
  verificationOutputs.set(record.id, `> ${record.command}\n\n✓ Demo verification passed\n✓ 43 tests passed\n`);
  addEvent(detail, "verification_run");
  return record;
}

function writeFile(args: MockArgs) {
  const path = stringArg(args, "path");
  const content = stringArg(args, "content");
  const revision = nextId("revision");
  browserMockFiles[path] = { content, revision };
  return {
    path,
    content,
    total_lines: content.split("\n").length,
    truncated: false,
    revision,
    is_editable: true,
  };
}

function searchFiles(query: string, limit: number): SearchMatch[] {
  const needle = query.toLocaleLowerCase();
  const matches: SearchMatch[] = [];
  for (const [path, file] of Object.entries(browserMockFiles)) {
    for (const [index, line] of file.content.split("\n").entries()) {
      const column = line.toLocaleLowerCase().indexOf(needle);
      if (column >= 0) matches.push({ path, line: index + 1, column: column + 1, line_text: line });
      if (matches.length >= limit) return matches;
    }
  }
  return matches;
}

function createTerminal(shell: string): string {
  const id = nextId("terminal");
  terminals.unshift({ id, state: shell === "Codex CLI" ? "agent" : "idle", shell, is_busy: false });
  terminalOutputs.set(id, `${shell}\r\nR-Code browser demo session\r\n\r\nPS D:\\project\\rust\\r-code> `);
  terminalInputs.set(id, "");
  return id;
}

function sendTerminalInput(id: string, text: string): void {
  const terminal = terminals.find((item) => item.id === id);
  if (!terminal || terminal.state === "exited") throw new Error("终端已经结束");
  let output = terminalOutputs.get(id) ?? "";
  let input = terminalInputs.get(id) ?? "";
  for (const character of text) {
    if (character === "\r" || character === "\n") {
      const command = input.trim();
      output += "\r\n";
      if (command === "pwd") output += "D:\\project\\rust\\r-code\r\n";
      else if (command === "clear" || command === "cls") output = "";
      else if (command.includes("test")) output += "✓ 43 tests passed in 1.12s\r\n";
      else if (command) output += `Demo executed: ${command}\r\n`;
      output += "PS D:\\project\\rust\\r-code> ";
      input = "";
    } else if (character === "\u007f" || character === "\b") {
      if (input.length > 0) {
        input = input.slice(0, -1);
        output = output.slice(0, -1);
      }
    } else if (character >= " ") {
      input += character;
      output += character;
    }
  }
  terminalOutputs.set(id, output);
  terminalInputs.set(id, input);
}

function setConfigValue(key: string, value: unknown): void {
  if (key === "agent_prompts" && value == null) {
    browserMockSettings.config.agent_prompts = {
      main_agent: "主 Agent 对最终结果负责；只在委派有明确收益时拆分边界清晰的子任务。",
      subagent: "子代理只完成父 Agent 指定的任务，不再委派，并返回可核验的简洁摘要。",
    };
    return;
  }
  const parts = key.split(".").filter(Boolean);
  let target = browserMockSettings.config as Record<string, unknown>;
  while (parts.length > 1) {
    const part = parts.shift()!;
    const next = target[part];
    if (!next || typeof next !== "object" || Array.isArray(next)) target[part] = {};
    target = target[part] as Record<string, unknown>;
  }
  if (parts[0]) target[parts[0]] = value;
}

function saveProvider(provider: ProviderSettingsInput): void {
  browserMockSettings.config.providers ??= {};
  browserMockSettings.config.providers[provider.name] = {
    base_url: provider.baseUrl,
    model: provider.model,
    max_tokens: provider.maxTokens ?? undefined,
    temperature: provider.temperature ?? undefined,
    protocol: provider.protocol ?? undefined,
  };
  browserMockSettings.provider_status[provider.name] = {
    configured: true,
    ready: true,
    source: provider.apiKey ? "keychain" : "environment",
    effective_protocol: provider.protocol ?? "openai_responses",
  };
  if (provider.activate) browserMockSettings.config.default_provider = provider.name;
}

function providerModels(request: ProviderModelsInput): ProviderModelsResponse {
  const preset = browserMockProviderCatalog.presets.find((item) => item.id === request.preset);
  const configured = browserMockSettings.config.providers?.[request.name]?.model;
  return {
    models: Array.from(new Set([configured, ...(preset?.models ?? [])].filter(Boolean))) as string[],
  };
}

async function browserMockImagePreview(): Promise<ArrayBuffer> {
  const canvas = document.createElement("canvas");
  canvas.width = 960;
  canvas.height = 540;
  const context = canvas.getContext("2d");
  if (!context) throw new Error("浏览器无法创建图片预览占位图");

  const sky = context.createLinearGradient(0, 0, 0, canvas.height);
  sky.addColorStop(0, "#4f9fe8");
  sky.addColorStop(0.58, "#91caef");
  sky.addColorStop(1, "#e8f2ef");
  context.fillStyle = sky;
  context.fillRect(0, 0, canvas.width, canvas.height);

  const drawCloud = (x: number, y: number, scale: number) => {
    context.save();
    context.translate(x, y);
    context.scale(scale, scale);
    context.fillStyle = "rgba(255, 255, 255, .9)";
    context.beginPath();
    context.arc(0, 24, 36, 0, Math.PI * 2);
    context.arc(44, 0, 54, 0, Math.PI * 2);
    context.arc(103, 23, 40, 0, Math.PI * 2);
    context.rect(0, 22, 103, 42);
    context.fill();
    context.restore();
  };
  drawCloud(155, 140, 1);
  drawCloud(650, 245, 0.72);
  drawCloud(410, 82, 0.5);

  context.fillStyle = "rgba(255, 255, 255, .88)";
  context.font = "600 22px system-ui, sans-serif";
  context.fillText("浏览器 Demo · 图片预览占位", 34, 500);

  const blob = await new Promise<Blob>((resolve, reject) => {
    canvas.toBlob((value) => value ? resolve(value) : reject(new Error("无法编码图片预览占位图")), "image/png");
  });
  return blob.arrayBuffer();
}

function mockMcpPreview(server: McpServerView): McpLaunchPreview {
  const transport = server.transport.type === "stdio"
    ? {
        type: "stdio" as const,
        executable: server.transport.executable,
        args: server.transport.args,
        environment_names: server.transport.environment_names,
      }
    : server.transport.type === "streamable_http"
      ? {
          type: "streamable_http" as const,
          url: server.transport.url,
          header_names: server.transport.header_names,
        }
      : { type: "stdio" as const, executable: "builtin", args: [], environment_names: [] };
  return {
    token: `demo-approval-${server.id}`,
    server_id: server.id,
    fingerprint: `demo-${server.id}`,
    transport,
    expires_at: new Date(Date.now() + 5 * 60_000).toISOString(),
  };
}

function mockMcpViewFromRequest(request: McpUpsertRequest): McpServerView {
  return {
    id: request.id,
    display_name: request.display_name,
    description: request.description,
    enabled: false,
    builtin: false,
    source: { kind: "user" },
    transport: request.transport,
    state: "disabled",
    tool_count: 0,
    launch_approved: false,
  };
}

/** 执行一条浏览器 Demo IPC，并返回与正式后端同形状的数据。 */
export async function browserMockInvoke(command: string, args: MockArgs = {}): Promise<unknown> {
  (globalThis as { __rCodePerformanceIpcProbe?: (name: string, args: MockArgs) => void })
    .__rCodePerformanceIpcProbe?.(command, args);
  const delayMs = (globalThis as { __rCodeBrowserMockDelayMs?: Record<string, number> })
    .__rCodeBrowserMockDelayMs?.[command] ?? 0;
  if (delayMs > 0) {
    await new Promise((resolve) => globalThis.setTimeout(resolve, delayMs));
  }
  const forcedFailure = (globalThis as { __rCodeBrowserMockFailures?: Record<string, string> })
    .__rCodeBrowserMockFailures?.[command];
  if (forcedFailure) throw new Error(forcedFailure);
  switch (command) {
    case "ping": return true;

    case "cmd_task_create": return copy(createTask(args));
    case "cmd_task_list": {
      const workspacePath = optionalStringArg(args, "workspacePath");
      const includeArchived = args.includeArchived === true;
      return copy(browserMockTasks.filter((task) =>
        (!workspacePath || task.workspace_path === workspacePath) && (includeArchived || task.state !== "archived")
      ));
    }
    case "cmd_task_archive": {
      const task = taskById(stringArg(args, "taskId"));
      if (task.state === "exploring" || task.state === "in_progress") {
        throw new Error("会话仍在运行，请先停止后归档");
      }
      task.state = "archived";
      touchTask(task);
      if (browserMockDetails[task.id]) browserMockDetails[task.id].task = copy(task);
      return copy(task);
    }
    case "cmd_task_restore": {
      const task = taskById(stringArg(args, "taskId"));
      if (task.state === "archived") {
        task.state = "idle";
        touchTask(task);
        if (browserMockDetails[task.id]) browserMockDetails[task.id].task = copy(task);
      }
      return copy(task);
    }
    case "cmd_task_delete": {
      const taskId = stringArg(args, "taskId");
      const task = taskById(taskId);
      if (task.state === "exploring" || task.state === "in_progress") {
        throw new Error("会话仍在运行，请先停止后删除");
      }
      const index = browserMockTasks.findIndex((candidate) => candidate.id === taskId);
      if (index >= 0) browserMockTasks.splice(index, 1);
      delete browserMockDetails[taskId];
      return undefined;
    }
    case "cmd_task_set_workspace": return copy(setTaskField(args, "workspace_path"));
    case "cmd_task_set_agent_engine": return copy(setTaskField(args, "agent_engine"));
    case "cmd_task_set_provider": return copy(setTaskField(args, "provider_name"));
    case "cmd_task_set_model": return copy(setTaskField(args, "model"));
    case "cmd_task_set_inference": {
      const task = taskById(stringArg(args, "taskId"));
      task.inference = copy((args.inference as InferenceOptions | undefined) ?? {});
      touchTask(task);
      return copy(task);
    }
    case "cmd_task_rename": return copy(setTaskField(args, "title"));
    case "cmd_task_update_goal": return copy(updateTaskGoal(args));
    case "cmd_task_set_mode": return copy(setTaskMode(args));
    case "cmd_plan_get": return copy(currentMockPlan(stringArg(args, "taskId")));
    case "cmd_plan_create": return copy(createMockPlan(stringArg(args, "taskId")));
    case "cmd_plan_answer": return copy(answerMockPlan(
      stringArg(args, "taskId"),
      args.input as AnswerPlanQuestionsInput,
    ));
    case "cmd_plan_retry_continuation": return copy(retryMockPlanContinuation(
      stringArg(args, "taskId"),
      stringArg(args, "questionSetId"),
    ));
    case "cmd_plan_approve": return copy(approveMockPlan(
      stringArg(args, "taskId"),
      stringArg(args, "planId"),
      integerArg(args, "expectedRevision"),
    ));
    case "cmd_plan_retry_implementation": return copy(retryMockPlanImplementation(
      stringArg(args, "taskId"),
      stringArg(args, "planId"),
    ));
    case "cmd_plan_cancel": return copy(cancelMockPlan(
      stringArg(args, "taskId"),
      stringArg(args, "planId"),
      integerArg(args, "expectedRevision"),
    ));
    case "cmd_plan_repair_projection": return copy(repairMockPlanProjection(
      stringArg(args, "taskId"),
      stringArg(args, "planId"),
    ));
    case "cmd_plan_update_item": return copy(updateMockPlanItem(
      stringArg(args, "taskId"),
      args.input as UpdatePlanItemInput,
    ));
    case "cmd_plan_review_status": return copy(currentMockEnhancedReview(stringArg(args, "taskId")));
    case "cmd_plan_review_accept_file": return copy(decideMockPlanReview(
      mockPlanReviewTarget(args),
      "accepted",
      "file",
    ));
    case "cmd_plan_review_accept_feature": return copy(decideMockPlanReview(
      mockPlanReviewTarget(args),
      "accepted",
      "feature",
    ));
    case "cmd_plan_review_reject_file": return copy(rejectMockPlanReview(mockPlanReviewTarget(args), "file"));
    case "cmd_plan_review_reject_feature": return copy(rejectMockPlanReview(mockPlanReviewTarget(args), "feature"));
    case "cmd_task_fork_context": return copy(forkTask(stringArg(args, "taskId")));
    case "cmd_task_compact_context": {
      const count = browserMockMessages(stringArg(args, "taskId")).filter((item) => item.kind === "message").length;
      return { compacted: count > 4, before_messages: count, after_messages: count > 4 ? 3 : count };
    }
    case "cmd_task_detail": return copy(detailById(stringArg(args, "taskId")));
    case "cmd_task_detail_batch": return {
      details: copy(((args.taskIds as string[] | undefined) ?? []).map((id) => browserMockDetails[id]).filter(Boolean)),
    };

    case "cmd_agent_send": sendMessage(args); return undefined;
    case "cmd_agent_abort": abortTask(stringArg(args, "taskId")); return undefined;
    case "cmd_agent_abort_subagent": browserMockAbortSubagent(stringArg(args, "taskId"), stringArg(args, "subagentId")); return undefined;
    case "cmd_agent_delegate_codex": return copy(delegateTask(args, "codex_exec"));
    case "cmd_agent_delegate_codex_mcp": return copy(delegateTask(args, "codex_mcp"));
    case "cmd_agent_queue_list": return copy(detailById(stringArg(args, "taskId")).queued_messages);
    case "cmd_agent_queue_remove": {
      const detail = detailById(stringArg(args, "taskId"));
      detail.queued_messages = detail.queued_messages.filter((item) => item.id !== stringArg(args, "queueId"));
      return undefined;
    }
    case "cmd_agent_resend": {
      const taskId = stringArg(args, "taskId");
      forkTask(taskId, stringArg(args, "messageId"));
      sendMessage({ taskId, message: stringArg(args, "message"), mode: "auto" });
      return undefined;
    }

    case "cmd_permission_approve": approvePermission(args); return undefined;
    case "cmd_permission_pending": return copy(detailById(stringArg(args, "taskId")).permissions.filter((item) => item.decision === "pending"));
    case "cmd_notification_list": return copy(browserMockNotificationList(args.unreadOnly === true));
    case "cmd_notification_mark_read": return browserMockMarkNotificationRead(stringArg(args, "notificationId"));
    case "cmd_notification_mark_all_read": return browserMockMarkAllNotificationsRead();

    case "cmd_changes_list": return copy(detailById(stringArg(args, "taskId")).changes);
    case "cmd_review_git_status": {
      const taskId = stringArg(args, "taskId");
      const changes = reviewChanges(taskId);
      const accepted = acceptedReviewPaths.get(taskId) ?? new Set<string>();
      const partiallyAccepted = partiallyAcceptedReviewPaths.get(taskId) ?? new Set<string>();
      const rejected = rejectedReviewPaths.get(taskId) ?? new Set<string>();
      const remainingCount = changes.filter((change) => !accepted.has(change.path) && !rejected.has(change.path)).length;
      return {
        git_repository: true,
        repo_root: "D:/demo/r-code",
        paths: changes.map((change) => ({
          path: change.path,
          scope: "task",
          change_type: change.change_type,
          accepted: accepted.has(change.path),
          rejected: rejected.has(change.path),
          remaining: !accepted.has(change.path) && !rejected.has(change.path),
          conflict: false,
          safe_to_accept: true,
          blocker: null,
          accepted_items: accepted.has(change.path) ? 1 : partiallyAccepted.has(change.path) ? 1 : 0,
          rejected_items: rejected.has(change.path) ? 1 : 0,
          remaining_items: accepted.has(change.path) || rejected.has(change.path) ? 0 : 1,
        })),
        accepted_count: changes.filter((change) => accepted.has(change.path)).length,
        rejected_count: changes.filter((change) => rejected.has(change.path)).length,
        remaining_count: remainingCount,
        conflict_count: 0,
        can_accept_all: remainingCount > 0,
      };
    }
    case "cmd_review_accept_line":
    case "cmd_review_accept_file":
    case "cmd_review_accept_all":
    case "cmd_review_reject_file": {
      const taskId = stringArg(args, "taskId");
      const changes = reviewChanges(taskId);
      const accepted = acceptedReviewPaths.get(taskId) ?? new Set<string>();
      const partiallyAccepted = partiallyAcceptedReviewPaths.get(taskId) ?? new Set<string>();
      const rejected = rejectedReviewPaths.get(taskId) ?? new Set<string>();
      if (command === "cmd_review_accept_all") {
        for (const change of changes) {
          accepted.add(change.path);
          partiallyAccepted.delete(change.path);
          rejected.delete(change.path);
        }
      } else if (command === "cmd_review_accept_file" && typeof args.path === "string") {
        accepted.add(args.path);
        partiallyAccepted.delete(args.path);
        rejected.delete(args.path);
      } else if (command === "cmd_review_accept_line" && typeof args.path === "string") {
        partiallyAccepted.add(args.path);
      } else if (command === "cmd_review_reject_file" && typeof args.path === "string") {
        rejected.add(args.path);
        accepted.delete(args.path);
        partiallyAccepted.delete(args.path);
      }
      acceptedReviewPaths.set(taskId, accepted);
      partiallyAcceptedReviewPaths.set(taskId, partiallyAccepted);
      rejectedReviewPaths.set(taskId, rejected);
      const remainingCount = changes.filter((change) => !accepted.has(change.path) && !rejected.has(change.path)).length;
      return {
        path: typeof args.path === "string" ? args.path : null,
        accepted_count: changes.filter((change) => accepted.has(change.path)).length,
        rejected_count: changes.filter((change) => rejected.has(change.path)).length,
        remaining_count: remainingCount,
        fully_accepted: remainingCount === 0,
      };
    }
    case "cmd_git_delivery_status": {
      const taskId = stringArg(args, "taskId");
      const changes = reviewChanges(taskId);
      const accepted = acceptedReviewPaths.get(taskId) ?? new Set<string>();
      const rejected = rejectedReviewPaths.get(taskId) ?? new Set<string>();
      const staged = stagedReviewPaths.get(taskId) ?? new Set<string>();
      const unresolved = changes.filter((change) => !accepted.has(change.path) && !rejected.has(change.path));
      const acceptedPaths = changes.filter((change) => accepted.has(change.path)).map((change) => change.path);
      return {
        branch: "codex/demo",
        upstream: "origin/codex/demo",
        ahead: 1,
        behind: 0,
        staged_task_paths: [...staged],
        staged_other_paths: [],
        can_stage: unresolved.length === 0 && acceptedPaths.some((path) => !staged.has(path)),
        can_commit: staged.size > 0,
        can_push: true,
        blockers: unresolved.length > 0 ? ["请先处理完所有审核项，再将已接受文件加入暂存区"] : [],
      };
    }
    case "cmd_git_stage_accepted": {
      const taskId = stringArg(args, "taskId");
      const changes = reviewChanges(taskId);
      const accepted = acceptedReviewPaths.get(taskId) ?? new Set<string>();
      const rejected = rejectedReviewPaths.get(taskId) ?? new Set<string>();
      const unresolved = changes.filter((change) => !accepted.has(change.path) && !rejected.has(change.path));
      if (unresolved.length > 0) throw new Error("请先处理完所有审核项，再将已接受文件加入暂存区");
      const staged = new Set(changes.filter((change) => accepted.has(change.path)).map((change) => change.path));
      stagedReviewPaths.set(taskId, staged);
      return {
        branch: "codex/demo",
        upstream: "origin/codex/demo",
        ahead: 1,
        behind: 0,
        staged_task_paths: [...staged],
        staged_other_paths: [],
        can_stage: false,
        can_commit: staged.size > 0,
        can_push: true,
        blockers: [],
      };
    }
    case "cmd_git_suggest_commit_message": return "feat: update reviewed task files";
    case "cmd_git_commit_task": return {
      sha: "0123456789abcdef0123456789abcdef01234567",
      message: stringArg(args, "message"),
    };
    case "cmd_git_push_task": return {
      sha: "0123456789abcdef0123456789abcdef01234567",
      branch: "codex/demo",
      upstream: "origin/codex/demo",
    };
    case "cmd_workflow_skills_list": return copy(workflowSkills);
    case "cmd_workflow_skill_save": {
      const draft = args.draft as WorkflowSkillDraft;
      const id = draft.id ?? `custom:${globalThis.crypto.randomUUID()}`;
      const saved: WorkflowSkill = {
        ...draft,
        id,
        overridden: draft.source === "builtin",
      };
      const index = workflowSkills.findIndex((skill) => skill.id === id);
      if (index >= 0) workflowSkills[index] = saved;
      else workflowSkills.push(saved);
      return copy(saved);
    }
    case "cmd_workflow_skill_reset": {
      const id = stringArg(args, "id");
      const restored = defaultWorkflowSkills.find((skill) => skill.id === id);
      if (!restored) throw new Error(`Unknown built-in Skill: ${id}`);
      const index = workflowSkills.findIndex((skill) => skill.id === id);
      if (index >= 0) workflowSkills[index] = { ...restored };
      else workflowSkills.push({ ...restored });
      return copy(restored);
    }
    case "cmd_workflow_skill_delete": {
      const id = stringArg(args, "id");
      workflowSkills = workflowSkills.filter((skill) => skill.id !== id || skill.source === "builtin");
      return null;
    }
    case "cmd_rollback_file": {
      const detail = detailById(stringArg(args, "taskId"));
      const path = stringArg(args, "path");
      detail.changes = detail.changes.filter((change) => change.path !== path);
      addEvent(detail, "file_changed");
      return path;
    }
    case "cmd_rollback_task": {
      const taskId = stringArg(args, "taskId");
      const detail = detailById(taskId);
      const paths = detail.changes.map((change) => change.path);
      detail.changes = [];
      detail.task.state = "idle";
      touchTask(detail.task);
      markTaskNotificationsRead(taskId);
      return paths;
    }
    case "cmd_accept_task": {
      const taskId = stringArg(args, "taskId");
      const detail = detailById(taskId);
      detail.task.state = "idle";
      for (const run of detail.runs) if (run.review_state === "pending") run.review_state = "accepted";
      touchTask(detail.task);
      markTaskNotificationsRead(taskId);
      return undefined;
    }
    case "cmd_change_request": browserMockChangeRequest(stringArg(args, "taskId")); return undefined;
    case "cmd_change_diff": return copy(diffFor(stringArg(args, "path")));
    case "cmd_run_verification": return copy(runVerification(args));
    case "cmd_verification_list": return copy(detailById(stringArg(args, "taskId")).verifications);
    case "cmd_verification_output": return verificationOutputs.get(stringArg(args, "id")) ?? "✓ Demo verification passed\n";

    case "cmd_file_list": return { entries: copy(browserMockFileEntries(optionalStringArg(args, "path"))), truncated: false };
    case "cmd_file_read": {
      const path = stringArg(args, "path");
      const file = browserMockFiles[path];
      if (!file) throw new Error(`Demo 文件不存在：${path}`);
      return { path, content: file.content, total_lines: file.content.split("\n").length, truncated: false, revision: file.revision, is_editable: true };
    }
    case "cmd_file_write": return copy(writeFile(args));
    case "cmd_local_file_target": {
      const rawReference = stringArg(args, "reference");
      const hashLocation = rawReference.match(/#L(\d+)(?:C(\d+))?$/i);
      const reference = rawReference.replace(/#L\d+(?:C\d+)?$/i, "");
      const workspacePath = optionalStringArg(args, "workspacePath");
      const relative = !/^(?:file:|[A-Za-z]:[\\/]|\/)/i.test(reference)
        ? reference.replace(/\\/g, "/")
        : null;
      const image = /\.(?:png|jpe?g|gif|webp|bmp|avif)$/i.test(reference);
      return {
        scope: relative && workspacePath ? "workspace" : "external",
        absolute_path: relative && workspacePath ? `${workspacePath}/${relative}` : reference,
        relative_path: relative && workspacePath ? relative : null,
        is_directory: false,
        mime_type: image ? "image/png" : null,
        size_bytes: image ? 68 : null,
        line: hashLocation ? Number(hashLocation[1]) : null,
        column: hashLocation?.[2] ? Number(hashLocation[2]) : null,
      };
    }
    case "cmd_local_image_preview": {
      const reference = stringArg(args, "reference");
      const isExternal = /^(?:file:|[A-Za-z]:[\\/]|\/)/i.test(reference);
      if (isExternal && !/[\\/]\.codex[\\/]generated_images[\\/]/i.test(reference)) {
        throw new Error("外部图片预览仅限 Codex generated_images 产物");
      }
      return browserMockImagePreview();
    }
    case "cmd_reveal_local_path": {
      document.documentElement.dataset.demoRevealedPath = stringArg(args, "path");
      return undefined;
    }
    case "cmd_prepare_workbench_window": return false;

    case "cmd_workspace_list": return copy(browserMockWorkspaces.map(normalizeWorkspace));
    case "cmd_workspace_open": {
      const path = stringArg(args, "path");
      const existing = browserMockWorkspaces.find((item) => item.canonical_path === path);
      if (existing) return copy(normalizeWorkspace(existing));
      const workspace: Workspace = {
        id: newWorkspaceId(),
        canonical_path: path,
        display_name: path.split(/[\\/]/).filter(Boolean).pop() ?? path,
        access_mode: "request_approval",
        last_opened_at: nowIso(),
        memory_mode: "inherit",
        memory_generation: 1,
      };
      browserMockWorkspaces.unshift(workspace);
      return copy(workspace);
    }
    case "cmd_workspace_forget": {
      const workspacePath = stringArg(args, "workspacePath");
      const live = browserMockTasks.some((task) =>
        task.workspace_path === workspacePath && (task.state === "exploring" || task.state === "in_progress")
      );
      if (live) throw new Error("项目仍有会话正在运行，请先停止后再清除项目");
      const removedTaskIds = browserMockTasks
        .filter((task) => task.workspace_path === workspacePath)
        .map((task) => task.id);
      for (const taskId of removedTaskIds) {
        const taskIndex = browserMockTasks.findIndex((task) => task.id === taskId);
        if (taskIndex >= 0) browserMockTasks.splice(taskIndex, 1);
        delete browserMockDetails[taskId];
      }
      for (let notificationIndex = browserMockNotifications.length - 1; notificationIndex >= 0; notificationIndex -= 1) {
        const notification = browserMockNotifications[notificationIndex];
        if (notification.workspace_path === workspacePath || (notification.task_id && removedTaskIds.includes(notification.task_id))) {
          browserMockNotifications.splice(notificationIndex, 1);
        }
      }
      const index = browserMockWorkspaces.findIndex((item) => item.canonical_path === workspacePath);
      if (index >= 0) browserMockWorkspaces.splice(index, 1);
      return { removed: index >= 0, removed_sessions: removedTaskIds.length };
    }
    case "cmd_workspace_choose": {
      const workspace = normalizeWorkspace(browserMockWorkspaces[0]);
      workspace.last_opened_at = nowIso();
      return copy(workspace);
    }
    case "cmd_workspace_set_access_mode": {
      const workspace = workspaceByPath(stringArg(args, "workspacePath"));
      workspace.access_mode = args.accessMode as ProjectAccessMode;
      workspace.last_opened_at = nowIso();
      return copy(workspace);
    }
    case "cmd_workspace_set_memory_mode": {
      const workspace = browserMockWorkspaces.find((item) => item.id === stringArg(args, "workspaceId"));
      if (!workspace) throw new Error("项目不存在");
      const expected = typeof args.expectedGeneration === "number" ? args.expectedGeneration : 0;
      if (workspace.memory_generation !== expected) throw new Error("项目记忆设置已在其他位置更新");
      workspace.memory_mode = args.memoryMode as WorkspaceMemoryMode;
      workspace.memory_generation += 1;
      return copy(workspace);
    }
    case "cmd_workspace_dashboard": return copy(browserMockWorkspaceDashboard(stringArg(args, "workspacePath")));
    case "cmd_project_activity_list": return copy(browserMockActivityList(stringArg(args, "workspacePath")));
    case "cmd_activity_list": return copy(browserMockActivityList());

    case "cmd_quick_open": {
      const needle = stringArg(args, "query").toLocaleLowerCase();
      const limit = typeof args.limit === "number" ? args.limit : 20;
      return Object.keys(browserMockFiles).filter((path) => path.toLocaleLowerCase().includes(needle)).slice(0, limit);
    }
    case "cmd_global_search": return copy(searchFiles(stringArg(args, "query"), typeof args.limit === "number" ? args.limit : 50));

    case "cmd_terminal_list": return copy(terminals);
    case "cmd_terminal_create": return createTerminal(stringArg(args, "shell"));
    case "cmd_terminal_create_codex": return createTerminal("Codex CLI");
    case "cmd_terminal_send": sendTerminalInput(stringArg(args, "id"), stringArg(args, "text")); return undefined;
    case "cmd_terminal_read":
    case "cmd_terminal_snapshot": return terminalOutputs.get(stringArg(args, "id")) ?? "";
    case "cmd_terminal_raw_snapshot": {
      const output = terminalOutputs.get(stringArg(args, "id")) ?? "";
      return { output, cursor: output.length };
    }
    case "cmd_terminal_raw_since": {
      const output = terminalOutputs.get(stringArg(args, "id")) ?? "";
      const cursor = typeof args.cursor === "number" ? args.cursor : 0;
      const reset = cursor > output.length;
      return { output: reset ? output : output.slice(cursor), cursor: output.length, reset };
    }
    case "cmd_terminal_kill": {
      const terminal = terminals.find((item) => item.id === stringArg(args, "id"));
      if (terminal) {
        terminal.state = "exited";
        terminal.is_busy = false;
      }
      return undefined;
    }
    case "cmd_terminal_resize": return undefined;

    case "cmd_recovery_data": return { interrupted_tasks: [], orphaned_permissions: 0 };
    case "cmd_recovery_cleanup": return { runs_closed: 0, tasks_interrupted: 0, permissions_denied: 0, tool_calls_closed: 0 };
    case "cmd_support_bundle": {
      const outputDir = stringArg(args, "outputDir").replace(/[\\/]+$/, "");
      return `${outputDir || "Downloads"}/r-code-support-demo.json`;
    }
    case "cmd_support_preview": return {
      version: "0.1.0-demo",
      platform: navigator.platform || "browser",
      generated_at: nowIso(),
      logs: [],
      config_summary: { demo: true },
      db_stats: { task_count: browserMockTasks.length, run_count: Object.values(browserMockDetails).reduce((sum, detail) => sum + detail.runs.length, 0), tool_call_count: 12 },
    };
    case "cmd_replay": return copy([
      { event_type: "task_created", timestamp: nowIso(), summary: "任务已创建", evidence_level: "recorded" },
      { event_type: "run_ended", timestamp: nowIso(), summary: "运行已完成", evidence_level: "verified" },
    ]);
    case "cmd_session_messages": return copy(browserMockMessages(stringArg(args, "taskId")));
    case "cmd_subagent_session_messages": return copy(browserMockSubagentMessages(stringArg(args, "taskId"), stringArg(args, "subagentId")));

    case "cmd_memory_overview": return copy(mockMemoryOverview);
    case "cmd_memory_update_settings": {
      const update = args.update as MemoryReviewSettingsUpdate;
      if (update.expected_version !== mockMemoryOverview.settings.version) throw new Error("记忆设置已在其他位置更新");
      const selectionChanged = mockMemoryOverview.settings.enabled !== update.enabled
        || JSON.stringify(mockMemoryOverview.settings.reviewer) !== JSON.stringify(update.reviewer);
      mockMemoryOverview.settings = {
        ...mockMemoryOverview.settings,
        ...update,
        version: mockMemoryOverview.settings.version + 1,
        review_generation: mockMemoryOverview.settings.review_generation + (selectionChanged ? 1 : 0),
        updated_at: nowIso(),
      };
      return copy(mockMemoryOverview.settings);
    }
    case "cmd_memory_review_now": {
      if (!mockMemoryOverview.settings.enabled || !mockMemoryOverview.settings.reviewer) return null;
      const id = `memory-job-${++sequence}`;
      mockMemoryOverview.recent_jobs.unshift({
        sequence: String(sequence), id, task_id: stringArg(args, "taskId"), source_workspace_id: null,
        trigger: "manual", status: "queued", provider_name: mockMemoryOverview.settings.reviewer.provider_name,
        model: mockMemoryOverview.settings.reviewer.model, attempt: 0, suppressed_turn_count: 0,
        error_code: null, created_at: nowIso(), updated_at: nowIso(),
      });
      return id;
    }
    case "cmd_memory_retry_job": {
      const job = mockMemoryOverview.recent_jobs.find((item) => item.id === stringArg(args, "jobId"));
      if (job) { job.status = "queued"; job.error_code = null; job.updated_at = nowIso(); }
      return undefined;
    }
    case "cmd_memory_cancel_job": {
      const job = mockMemoryOverview.recent_jobs.find((item) => item.id === stringArg(args, "jobId"));
      if (job) { job.status = "cancelled"; job.updated_at = nowIso(); }
      return undefined;
    }
    case "cmd_memory_add_entry": {
      const draft = args.draft as MemoryEntryDraft;
      const id = `memory-entry-${++sequence}`;
      const entry = {
        id,
        owner: draft.scope === "global"
          ? { scope: "global" as const, authorization: "manual" as const }
          : { scope: "project" as const, workspace_id: draft.workspace_id ?? "", origin: "manual" as const },
        kind: draft.kind,
        content: draft.content,
        normalized_hash: `demo-${sequence}`,
        version: 1,
        pinned: draft.pinned,
        source_job_id: null,
        source_candidate_id: null,
        created_at: nowIso(),
        updated_at: nowIso(),
      };
      (draft.scope === "global" ? mockMemoryOverview.global_entries : mockMemoryOverview.project_entries).unshift(entry);
      return copy(entry);
    }
    case "cmd_memory_edit_entry": {
      const edit = args.edit as MemoryEntryEdit;
      const entry = [...mockMemoryOverview.global_entries, ...mockMemoryOverview.project_entries]
        .find((item) => item.id === stringArg(args, "entryId"));
      if (!entry) throw new Error("记忆不存在");
      if (entry.version !== edit.expected_version) throw new Error("记忆已在其他位置更新");
      Object.assign(entry, { kind: edit.kind, content: edit.content, pinned: edit.pinned, version: entry.version + 1, updated_at: nowIso() });
      return copy(entry);
    }
    case "cmd_memory_delete_entry": {
      const id = stringArg(args, "entryId");
      mockMemoryOverview.global_entries = mockMemoryOverview.global_entries.filter((entry) => entry.id !== id);
      mockMemoryOverview.project_entries = mockMemoryOverview.project_entries.filter((entry) => entry.id !== id);
      return undefined;
    }
    case "cmd_memory_approve_candidate": {
      const id = stringArg(args, "candidateId");
      const candidate = mockMemoryOverview.pending_candidates.find((item) => item.id === id);
      if (!candidate) throw new Error("候选不存在");
      mockMemoryOverview.pending_candidates = mockMemoryOverview.pending_candidates.filter((item) => item.id !== id);
      const content = optionalStringArg(args, "editedContent") ?? candidate.proposed_content;
      const entry = {
        id: `memory-entry-${++sequence}`, owner: { scope: "global" as const, authorization: "approved_candidate" as const },
        kind: candidate.kind, content, normalized_hash: `demo-${sequence}`, version: 1, pinned: false,
        source_job_id: null, source_candidate_id: candidate.id, created_at: nowIso(), updated_at: nowIso(),
      };
      mockMemoryOverview.global_entries.unshift(entry);
      return copy(entry);
    }
    case "cmd_memory_reject_candidate": {
      const id = stringArg(args, "candidateId");
      mockMemoryOverview.pending_candidates = mockMemoryOverview.pending_candidates.filter((item) => item.id !== id);
      return undefined;
    }
    case "cmd_memory_clear_all": {
      mockMemoryOverview = {
        ...mockMemoryOverview,
        settings: { ...mockMemoryOverview.settings, enabled: false, reviewer: null, version: mockMemoryOverview.settings.version + 1, review_generation: mockMemoryOverview.settings.review_generation + 1, updated_at: nowIso() },
        global_entries: [], project_entries: [], pending_candidates: [], recent_jobs: [],
      };
      return copy(mockMemoryOverview.settings);
    }
    case "cmd_legacy_memory_status": return copy(
      legacyMemoryStatusByWorkspace.get(stringArg(args, "workspacePath"))
        ?? { exists: false, git_tracking: "unknown" },
    );
    case "cmd_settings_get": return copy(browserMockSettings);
    case "cmd_mcp_snapshot": return copy({ servers: mockMcpServers } satisfies McpManagerSnapshot);
    case "cmd_mcp_upsert": {
      const request = args.request as McpUpsertRequest;
      const server = mockMcpViewFromRequest(request);
      mockMcpServers = [...mockMcpServers.filter((item) => item.id !== server.id), server];
      return copy(server);
    }
    case "cmd_mcp_remove": {
      const id = stringArg(args, "serverId");
      mockMcpServers = mockMcpServers.filter((server) => server.id !== id || server.builtin);
      return undefined;
    }
    case "cmd_mcp_toggle": {
      const id = stringArg(args, "serverId");
      const server = mockMcpServers.find((item) => item.id === id);
      if (!server) throw new Error(`未找到 MCP 服务：${id}`);
      const enabled = args.enabled === true;
      const confirmation = optionalStringArg(args, "confirmationToken");
      if (enabled && !server.builtin && !server.launch_approved && !confirmation) {
        return copy({ server, confirmation: mockMcpPreview(server) });
      }
      server.enabled = enabled;
      server.state = enabled ? "stopped" : "disabled";
      if (confirmation) server.launch_approved = true;
      return copy({ server });
    }
    case "cmd_mcp_test_connection": {
      const id = stringArg(args, "serverId");
      const server = mockMcpServers.find((item) => item.id === id);
      if (!server?.enabled) throw new Error("请先启用该 MCP 服务");
      server.state = "running";
      server.tool_count = Math.max(1, server.tool_count);
      return [{ server_id: id, name: "demo_tool", description: "Demo tool", input_schema: { type: "object" }, read_only: true }];
    }
    case "cmd_mcp_credential_status": {
      const id = stringArg(args, "serverId");
      const server = mockMcpServers.find((item) => item.id === id);
      if (!server) throw new Error(`未找到 MCP 服务：${id}`);
      const names = server.transport.type === "stdio"
        ? server.transport.environment_names
        : server.transport.type === "streamable_http" ? server.transport.header_names : [];
      const configured = mockMcpCredentials.get(id) ?? new Set<string>();
      return copy(names.map((name): McpCredentialStatus => ({ name, configured: configured.has(name) })));
    }
    case "cmd_mcp_set_credential": {
      const id = stringArg(args, "serverId");
      const configured = mockMcpCredentials.get(id) ?? new Set<string>();
      if (stringArg(args, "value")) configured.add(stringArg(args, "name"));
      mockMcpCredentials.set(id, configured);
      return undefined;
    }
    case "cmd_mcp_delete_credential": {
      mockMcpCredentials.get(stringArg(args, "serverId"))?.delete(stringArg(args, "name"));
      return undefined;
    }
    case "cmd_mcp_market_search": return copy(mockMcpMarket);
    case "cmd_mcp_market_prepare_install": {
      const request = args.request as McpMarketInstallRequest;
      const option = request.server.install_options.find((item) => item.id === request.option_id);
      if (!option) throw new Error("所选启动方案不存在");
      const transport = option.transport.type === "stdio"
        ? { type: "stdio" as const, executable: option.transport.executable, args: option.transport.args, environment_names: option.transport.environment.map((item) => item.name) }
        : { type: "streamable_http" as const, url: option.transport.url, header_names: option.transport.headers.map((item) => item.name) };
      return copy({ token: `demo-install-${request.server_id}`, server_id: request.server_id, fingerprint: `demo-${request.server_id}`, transport, expires_at: new Date(Date.now() + 300_000).toISOString() });
    }
    case "cmd_mcp_market_install": {
      const request = args.request as McpMarketInstallRequest;
      const option = request.server.install_options.find((item) => item.id === request.option_id);
      if (!option) throw new Error("所选启动方案不存在");
      const transport = option.transport.type === "stdio"
        ? { type: "stdio" as const, executable: option.transport.executable, args: option.transport.args, environment_names: option.transport.environment.map((item) => item.name) }
        : { type: "streamable_http" as const, url: option.transport.url, header_names: option.transport.headers.map((item) => item.name) };
      const server: McpServerView = { id: request.server_id, display_name: request.server.title, description: request.server.description, enabled: false, builtin: false, source: { kind: "registry", registry_url: "https://registry.modelcontextprotocol.io/v0.1/servers", name: request.server.name, version: request.server.version }, transport, state: "disabled", tool_count: 0, launch_approved: false };
      mockMcpServers = [...mockMcpServers.filter((item) => item.id !== server.id), server];
      return copy(server);
    }
    case "cmd_provider_catalog": return copy(browserMockProviderCatalog);
    case "cmd_provider_models": return copy(providerModels(args.request as ProviderModelsInput));
    case "cmd_settings_set": setConfigValue(stringArg(args, "key"), args.value); return undefined;
    case "cmd_settings_save_provider": saveProvider(args.provider as ProviderSettingsInput); return undefined;
    case "cmd_settings_select_provider": browserMockSettings.config.default_provider = stringArg(args, "name"); return undefined;
    case "cmd_settings_delete_provider": {
      const name = stringArg(args, "name");
      delete browserMockSettings.config.providers?.[name];
      delete browserMockSettings.provider_status[name];
      if (browserMockSettings.config.default_provider === name) browserMockSettings.config.default_provider = undefined;
      return undefined;
    }
    case "cmd_codex_integration_status": return copy(browserMockCodexIntegrationStatus());
    case "cmd_codex_install_cli": return copy(browserMockInstallCli());
    case "cmd_codex_start_login":
    case "cmd_codex_start_device_login": browserMockLogin(); return undefined;
    case "cmd_codex_install_skill": browserMockInstallSkill(); return undefined;
    case "cmd_codex_install_mcp_server": browserMockInstallMcp(); return undefined;
    case "cmd_codex_setup_collaboration": return copy(browserMockSetupCollaboration());
    case "cmd_codex_cli_preferences": return copy(browserMockCliPreferences());
    case "cmd_codex_save_cli_preferences": return copy(browserMockSaveCliPreferences(
      optionalStringArg(args, "model"),
      optionalStringArg(args, "reasoningEffort"),
      optionalStringArg(args, "verbosity"),
      optionalStringArg(args, "permissionMode"),
    ));
    case "cmd_logs_tail": {
      const logs: LogEntry[] = [
        { timestamp: nowIso(), level: "INFO", target: "r_code::demo", message: "完整浏览器 Demo 已就绪" },
        { timestamp: nowIso(), level: "DEBUG", target: "r_code::gateway", message: "所有数据均保存在当前页面内存中" },
      ];
      return copy(logs);
    }
    default:
      throw new Error(`浏览器 Demo 尚未实现命令：${command}`);
  }
}
