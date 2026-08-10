/**
 * Tauri IPC 封装 — 前端 → 后端全部命令的 typed wrapper。
 * 后端命令注册于 src-tauri/src/main.rs；参数一律 camelCase（Tauri v2 约定）。
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AgentEvent,
  AgentEventEnvelope,
  AgentRun,
  AgentSendMode,
  ChangeDiff,
  NotificationPage,
  ProjectActivityPage,
  TaskDetailBatch,
  WorkspaceDashboard,
  FileChange,
  LogEntry,
  PermissionDecision,
  PermissionRequest,
  QueuedMessage,
  RecoveryCleanupResult,
  RecoveryPageData,
  ReplayDepth,
  ReplayEntry,
  SearchMatch,
  SessionBranch,
  SessionMessage,
  AttachmentInput,
  SettingsResponse,
  SupportBundlePreview,
  Task,
  TaskAgentEngine,
  TaskDetail,
  TaskMode,
  TerminalInfo,
  TerminalRawBatch,
  TerminalRawSnapshot,
  VerificationRecord,
  ProjectAccessMode, Workspace, WorkspaceMemoryMode,
  ProviderSettingsInput,
  ProviderCatalog,
  ProviderModelsInput,
  ProviderModelsResponse,
  CodexCliPreferences,
  CodexIntegrationStatus,
  RtkStatus,
  ContextCompactionResult,
  InferenceOptions,
  LegacyMemoryStatus,
  McpCredentialStatus,
  McpLaunchPreview,
  McpManagerSnapshot,
  McpMarketInstallRequest,
  McpMarketPage,
  McpServerView,
  McpToggleResult,
  McpToolDescriptor,
  McpUpsertRequest,
  MemoryEntry,
  MemoryEntryDraft,
  MemoryEntryEdit,
  MemoryOverview,
  MemoryReviewSettingsUpdate,
  MemoryReviewSettingsView,
  AnswerPlanQuestionsInput,
  EnhancedReviewTarget,
  EnhancedReviewView,
  PlanRejectResult,
  PlanReviewDecision,
  PlanView,
  UpdatePlanItemInput,
} from "./types";
import {
  browserMockDetails,
  browserMockFileEntries,
  browserMockFiles,
  browserMockActivityList,
  browserMockAbortSubagent,
  browserMockChangeRequest,
  browserMockCodexIntegrationStatus,
  browserMockCodexCliPreferences,
  browserMockInstallCodexCli,
  browserMockSetupCodexCollaboration,
  browserMockSaveCodexCliPreferences,
  browserMockInstallCodexSkill,
  browserMockAuthenticateCodex,
  browserMockEnableCodexMcp,
  browserMockMessages,
  browserMockSubagentMessages,
  browserMockMarkAllNotificationsRead,
  browserMockMarkNotificationRead,
  browserMockNotificationList,
  browserMockProviderCatalog,
  browserMockSettings,
  browserMockWorkspaceDashboard,
  shouldUseBrowserMock,
} from "./mock-data";
import { browserMockInvoke } from "./browser-mock-runtime";

export const PROJECT_CONVERSATION_LIMIT_REACHED_CODE =
  "PROJECT_CONVERSATION_LIMIT_REACHED";

interface CommandErrorPayload {
  code: string;
  message: string;
  limit?: number;
}

export class IpcCommandError extends Error {
  readonly code: string;
  readonly limit?: number;

  constructor(payload: CommandErrorPayload) {
    super(payload.message);
    this.name = "IpcCommandError";
    this.code = payload.code;
    this.limit = payload.limit;
  }
}

function commandErrorPayload(cause: unknown): CommandErrorPayload | null {
  if (typeof cause !== "object" || cause == null) return null;
  const candidate = cause as Record<string, unknown>;
  if (typeof candidate.code !== "string" || typeof candidate.message !== "string") return null;
  return {
    code: candidate.code,
    message: candidate.message,
    ...(typeof candidate.limit === "number" ? { limit: candidate.limit } : {}),
  };
}

async function ipc<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    if (shouldUseBrowserMock()) {
      return await browserMockInvoke(command, args) as T;
    }
    return await invoke<T>(command, args);
  } catch (cause) {
    const payload = commandErrorPayload(cause);
    if (payload) throw new IpcCommandError(payload);
    throw cause;
  }
}

// ---------- 系统 ----------
export const ping = () => ipc<boolean>("ping");
export const appQuit = () => ipc<void>("cmd_app_quit");

// ---------- 任务 ----------
export const taskCreate = (
  workspacePath: string | null,
  title: string,
  goal: string,
  mode: TaskMode,
  providerName: string | null = null,
  agentEngine: TaskAgentEngine | null = null
) => ipc<Task>("cmd_task_create", { workspacePath, title, goal, mode, providerName, agentEngine });

/** Immediately persist a project-scoped empty conversation with server-assigned title/limit. */
export const projectConversationCreate = (workspacePath: string) =>
  ipc<Task>("cmd_project_conversation_create", { workspacePath });

/** Prebuild the selected runtime/session without starting a run or adding conversation content. */
export const taskPrepare = (taskId: string) =>
  ipc<void>("cmd_task_prepare", { taskId });

export const taskList = (workspacePath?: string, includeArchived = false) =>
  ipc<Task[]>("cmd_task_list", { workspacePath, includeArchived });

export const taskArchive = (taskId: string) =>
  ipc<Task>("cmd_task_archive", { taskId });

export const taskRestore = (taskId: string) =>
  ipc<Task>("cmd_task_restore", { taskId });

export const taskDelete = (taskId: string) =>
  ipc<void>("cmd_task_delete", { taskId });

export const taskSetWorkspace = (taskId: string, workspacePath: string | null) =>
  ipc<Task>("cmd_task_set_workspace", { taskId, workspacePath });

export const taskSetProvider = (taskId: string, providerName: string) =>
  ipc<Task>("cmd_task_set_provider", { taskId, providerName });

export const taskSetAgentEngine = (taskId: string, agentEngine: TaskAgentEngine) =>
  ipc<Task>("cmd_task_set_agent_engine", { taskId, agentEngine });

/** 切换会话使用的具体模型；传 null 回退到该服务的默认模型。 */
export const taskSetModel = (taskId: string, model: string | null) =>
  ipc<Task>("cmd_task_set_model", { taskId, model });

/** 修改会话的模型专属推理参数；省略字段时使用 Provider 默认值。 */
export const taskSetInference = (taskId: string, inference: InferenceOptions) =>
  ipc<Task>("cmd_task_set_inference", { taskId, inference });

export const taskRename = (taskId: string, title: string) =>
  ipc<Task>("cmd_task_rename", { taskId, title });

/** Update or clear the durable task goal used by the Plan aggregate and subsequent turns. */
export const taskUpdateGoal = (taskId: string, goal: string) =>
  ipc<Task>("cmd_task_update_goal", { taskId, goal });

/** Switch the task policy; Plan mode is enforced by the native R-Code runtime. */
export const taskSetMode = (taskId: string, mode: TaskMode) =>
  ipc<Task>("cmd_task_set_mode", { taskId, mode });

// ---------- Plan / Human in the loop ----------
export const planGet = (taskId: string) =>
  ipc<PlanView | null>("cmd_plan_get", { taskId });

export const planCreate = (taskId: string) =>
  ipc<PlanView>("cmd_plan_create", { taskId });

export const planAnswer = (taskId: string, input: AnswerPlanQuestionsInput) =>
  ipc<PlanView>("cmd_plan_answer", { taskId, input });

export const planRetryContinuation = (taskId: string, questionSetId: string) =>
  ipc<PlanView>("cmd_plan_retry_continuation", { taskId, questionSetId });

export const planApprove = (taskId: string, planId: string, expectedRevision: number) =>
  ipc<PlanView>("cmd_plan_approve", { taskId, planId, expectedRevision });

export const planRetryImplementation = (taskId: string, planId: string) =>
  ipc<PlanView>("cmd_plan_retry_implementation", { taskId, planId });

export const planCancel = (taskId: string, planId: string, expectedRevision: number) =>
  ipc<PlanView>("cmd_plan_cancel", { taskId, planId, expectedRevision });

export const planRepairProjection = (taskId: string, planId: string) =>
  ipc<PlanView>("cmd_plan_repair_projection", { taskId, planId });

export const planUpdateItem = (taskId: string, input: UpdatePlanItemInput) =>
  ipc<PlanView>("cmd_plan_update_item", { taskId, input });

export const taskForkContext = (taskId: string) =>
  ipc<SessionBranch>("cmd_task_fork_context", { taskId });

export const taskClearContext = (taskId: string) =>
  ipc<SessionBranch>("cmd_task_clear_context", { taskId });

export const taskCompactContext = (taskId: string, focus?: string) =>
  ipc<ContextCompactionResult>("cmd_task_compact_context", {
    taskId,
    focus: focus?.trim() || null,
  });

export const taskDetail = (taskId: string) =>
  ipc<TaskDetail>("cmd_task_detail", { taskId });

export const taskDetailBatch = async (taskIds: string[]) => {
  try {
    return await ipc<TaskDetailBatch>("cmd_task_detail_batch", { taskIds });
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    return { details: taskIds.map((taskId) => browserMockDetails[taskId]).filter((detail): detail is TaskDetail => Boolean(detail)) };
  }
};

// ---------- Agent ----------
export const agentSend = (
  taskId: string,
  message: string,
  mode: AgentSendMode = "auto",
  attachments: AttachmentInput[] = [],
) => ipc<void>("cmd_agent_send", { taskId, message, mode, attachments });

export const agentAbort = (taskId: string) => ipc<void>("cmd_agent_abort", { taskId });

export const agentAbortSubagent = async (taskId: string, subagentId: string) => {
  try {
    return await ipc<void>("cmd_agent_abort_subagent", { taskId, subagentId });
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    browserMockAbortSubagent(taskId, subagentId);
  }
};

/** 将当前运行中的一项任务委派给本机已登录的 Codex CLI；权限由 config.toml 决定。 */
export const agentDelegateCodex = (taskId: string, goal: string, label: string | null = null) =>
  ipc<AgentRun>("cmd_agent_delegate_codex", { taskId, goal, label });

/** 以官方 `codex mcp-server` 创建可续接的 Codex 子代理会话。 */
export const agentDelegateCodexMcp = (taskId: string, goal: string, label: string | null = null) =>
  ipc<AgentRun>("cmd_agent_delegate_codex_mcp", { taskId, goal, label });

export const agentQueueList = (taskId: string) =>
  ipc<QueuedMessage[]>("cmd_agent_queue_list", { taskId });

export const agentQueueRemove = (taskId: string, queueId: string) =>
  ipc<void>("cmd_agent_queue_remove", { taskId, queueId });

export const agentQueueReorder = (taskId: string, queueIds: string[]) =>
  ipc<void>("cmd_agent_queue_reorder", { taskId, queueIds });

export const agentQueueUpdate = (taskId: string, queueId: string, message: string) =>
  ipc<void>("cmd_agent_queue_update", { taskId, queueId, message });

export type AgentQueueSteerResult = "steered" | "queued_next" | "started";

export const agentQueueSteer = (taskId: string, queueId: string) =>
  ipc<AgentQueueSteerResult>("cmd_agent_queue_steer", { taskId, queueId });

export const agentResend = (taskId: string, messageId: string, message: string) =>
  ipc<void>("cmd_agent_resend", { taskId, messageId, message });

/** 订阅 agent 流式事件（后端 drain 循环 emit 的 "agent-event"）。 */
export const onAgentEvent = (handler: (taskId: string, event: AgentEvent) => void): Promise<UnlistenFn> => {
  if (shouldUseBrowserMock()) return Promise.resolve(() => {});
  return listen<AgentEventEnvelope>("agent-event", (e) => handler(e.payload.task_id, e.payload.event));
};

// ---------- 权限 ----------
export const permissionApprove = (requestId: string, decision: Exclude<PermissionDecision, "pending">) =>
  ipc<void>("cmd_permission_approve", { requestId, decision });

export const permissionPending = (taskId: string) =>
  ipc<PermissionRequest[]>("cmd_permission_pending", { taskId });

// ---------- 通知中心 ----------
export const notificationList = async (cursor: string | null = null, limit = 20, unreadOnly = false) => {
  try {
    return await ipc<NotificationPage>("cmd_notification_list", { cursor, limit, unreadOnly });
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    return browserMockNotificationList(unreadOnly);
  }
};

export const notificationMarkRead = async (notificationId: string) => {
  try {
    return await ipc<boolean>("cmd_notification_mark_read", { notificationId });
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    return browserMockMarkNotificationRead(notificationId);
  }
};

export const notificationMarkAllRead = async () => {
  try {
    return await ipc<number>("cmd_notification_mark_all_read");
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    return browserMockMarkAllNotificationsRead();
  }
};

// ---------- 变更 / 审查 ----------
export const changesList = (taskId: string) =>
  ipc<FileChange[]>("cmd_changes_list", { taskId });

export const rollbackFile = (taskId: string, path: string) =>
  ipc<string>("cmd_rollback_file", { taskId, path });

export const rollbackTask = (taskId: string) =>
  ipc<string[]>("cmd_rollback_task", { taskId });

export const acceptTask = (taskId: string) => ipc<void>("cmd_accept_task", { taskId });

export const reviewGitStatus = (taskId: string) =>
  ipc<import("./types").ReviewGitStatus>("cmd_review_git_status", { taskId });

export const REVIEW_STATUS_CHANGED_EVENT = "r-code:review-status-changed";

function announceReviewStatusChanged(taskId: string, result?: import("./types").ReviewAcceptResult): void {
  if (typeof window !== "undefined") {
    window.dispatchEvent(new CustomEvent(REVIEW_STATUS_CHANGED_EVENT, { detail: { taskId, result } }));
  }
}

export async function reviewAcceptLine(taskId: string, path: string, lineId: string) {
  const result = await ipc<import("./types").ReviewAcceptResult>("cmd_review_accept_line", { taskId, path, lineId });
  announceReviewStatusChanged(taskId, result);
  return result;
}

export async function reviewAcceptFile(taskId: string, path: string) {
  const result = await ipc<import("./types").ReviewAcceptResult>("cmd_review_accept_file", { taskId, path });
  announceReviewStatusChanged(taskId, result);
  return result;
}

export async function reviewAcceptAll(taskId: string) {
  const result = await ipc<import("./types").ReviewAcceptResult>("cmd_review_accept_all", { taskId });
  announceReviewStatusChanged(taskId, result);
  return result;
}

export async function reviewRejectFile(taskId: string, path: string) {
  const result = await ipc<import("./types").ReviewAcceptResult>("cmd_review_reject_file", { taskId, path });
  announceReviewStatusChanged(taskId, result);
  return result;
}

/** Current Plan revision grouped by feature ownership; independent from the Git review ledger. */
export const planReviewStatus = (taskId: string) =>
  ipc<EnhancedReviewView | null>("cmd_plan_review_status", { taskId });

export async function planReviewAcceptFile(target: EnhancedReviewTarget) {
  const result = await ipc<PlanReviewDecision>("cmd_plan_review_accept_file", { target });
  announceReviewStatusChanged(target.task_id);
  return result;
}

export async function planReviewAcceptFeature(target: EnhancedReviewTarget) {
  const result = await ipc<PlanReviewDecision>("cmd_plan_review_accept_feature", { target });
  announceReviewStatusChanged(target.task_id);
  return result;
}

export async function planReviewRejectFile(target: EnhancedReviewTarget) {
  const result = await ipc<PlanRejectResult>("cmd_plan_review_reject_file", { target });
  announceReviewStatusChanged(target.task_id);
  return result;
}

export async function planReviewRejectFeature(target: EnhancedReviewTarget) {
  const result = await ipc<PlanRejectResult>("cmd_plan_review_reject_feature", { target });
  announceReviewStatusChanged(target.task_id);
  return result;
}

export const gitDeliveryStatus = (taskId: string) =>
  ipc<import("./types").GitDeliveryStatus>("cmd_git_delivery_status", { taskId });

export const gitStageAccepted = (taskId: string) =>
  ipc<import("./types").GitDeliveryStatus>("cmd_git_stage_accepted", { taskId });

export const gitSuggestCommitMessage = (taskId: string) =>
  ipc<string>("cmd_git_suggest_commit_message", { taskId });

export const gitCommitTask = (taskId: string, message: string) =>
  ipc<import("./types").GitCommitResult>("cmd_git_commit_task", { taskId, message });

export const gitPushTask = (taskId: string) =>
  ipc<import("./types").GitPushResult>("cmd_git_push_task", { taskId });

export const workflowSkillsList = () =>
  ipc<import("./types").WorkflowSkill[]>("cmd_workflow_skills_list");

export const workflowSkillSave = (draft: import("./types").WorkflowSkillDraft) =>
  ipc<import("./types").WorkflowSkill>("cmd_workflow_skill_save", { draft });

export const workflowSkillReset = (id: string) =>
  ipc<import("./types").WorkflowSkill>("cmd_workflow_skill_reset", { id });

export const workflowSkillDelete = (id: string) =>
  ipc<void>("cmd_workflow_skill_delete", { id });

export const changeRequest = async (taskId: string, message: string) => {
  try {
    return await ipc<void>("cmd_change_request", { taskId, message });
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    browserMockChangeRequest(taskId);
  }
};

export const changeDiff = (taskId: string, path: string) =>
  ipc<ChangeDiff>("cmd_change_diff", { taskId, path });

// ---------- 验证 ----------
export const runVerification = (taskId: string, command: string) =>
  ipc<VerificationRecord>("cmd_run_verification", { taskId, command });

export const verificationList = (taskId: string) =>
  ipc<VerificationRecord[]>("cmd_verification_list", { taskId });

export const verificationOutput = (id: string) =>
  ipc<string>("cmd_verification_output", { id });

// ---------- 文件读取（Editor 只读预览） ----------
export interface FileContent {
  path: string;
  content: string;
  total_lines: number;
  truncated: boolean;
  revision: string;
  is_editable: boolean;
}

export interface FileTreeEntry {
  path: string;
  name: string;
  is_directory: boolean;
}

export interface FileTreeListing {
  entries: FileTreeEntry[];
  truncated: boolean;
}

export interface LocalFileTarget {
  scope: "workspace" | "external";
  absolute_path: string;
  relative_path: string | null;
  is_directory: boolean;
  mime_type: string | null;
  size_bytes: number | null;
  line: number | null;
  column: number | null;
}

export const fileList = async (workspacePath: string, path: string | null = null) => {
  try {
    return await ipc<FileTreeListing>("cmd_file_list", { workspacePath, path });
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    return { entries: browserMockFileEntries(path), truncated: false };
  }
};

export const fileRead = async (workspacePath: string, path: string) => {
  try {
    return await ipc<FileContent>("cmd_file_read", { workspacePath, path });
  } catch (error) {
    const file = browserMockFiles[path];
    if (!shouldUseBrowserMock() || !file) throw error;
    return { path, content: file.content, total_lines: file.content.split("\n").length, truncated: false, revision: file.revision, is_editable: true };
  }
};

export const fileWrite = (
  workspacePath: string,
  path: string,
  content: string,
  expectedRevision: string,
) => ipc<FileContent>("cmd_file_write", { workspacePath, path, content, expectedRevision });

/** Resolve a model-provided local path in the host before deciding how the UI should navigate. */
export const localFileTarget = (workspacePath: string | null, reference: string) =>
  ipc<LocalFileTarget>("cmd_local_file_target", { workspacePath, reference });

/** Binary raster preview; the caller owns any Blob URL created from this buffer. */
export const localImagePreview = async (
  workspacePath: string | null,
  reference: string,
): Promise<ArrayBuffer> => {
  if (shouldUseBrowserMock()) {
    return browserMockInvoke("cmd_local_image_preview", { workspacePath, reference }) as Promise<ArrayBuffer>;
  }
  const response = await invoke<ArrayBuffer | Uint8Array | number[]>("cmd_local_image_preview", {
    workspacePath,
    reference,
  });
  if (response instanceof ArrayBuffer) return response;
  if (response instanceof Uint8Array) {
    return Uint8Array.from(response).buffer;
  }
  return Uint8Array.from(response).buffer;
};

export const revealLocalPath = (path: string) =>
  ipc<void>("cmd_reveal_local_path", { path });

/** Idempotently make room for the right-hand workbench; browser demos simply keep their size. */
export const prepareWorkbenchWindow = async () => {
  if (shouldUseBrowserMock()) return false;
  return invoke<boolean>("cmd_prepare_workbench_window");
};

// ---------- Workspace ----------
export const workspaceList = () => ipc<Workspace[]>("cmd_workspace_list");

export const workspaceOpen = (path: string) =>
  ipc<Workspace>("cmd_workspace_open", { path });

export interface WorkspaceForgetResult {
  removed: boolean;
  removed_sessions: number;
}

/** 清除 R-Code 内部的项目与关联记录；真实工作区目录始终保留。 */
export const workspaceForget = (workspacePath: string) =>
  ipc<WorkspaceForgetResult>("cmd_workspace_forget", { workspacePath });

/** 原生系统文件夹选择器；用户取消时返回 null。 */
export const workspaceChoose = () => ipc<Workspace | null>("cmd_workspace_choose");

export const workspaceSetAccessMode = (workspacePath: string, accessMode: ProjectAccessMode) =>
  ipc<Workspace>("cmd_workspace_set_access_mode", { workspacePath, accessMode });

export const workspaceSetMemoryMode = (
  workspaceId: string,
  expectedGeneration: number,
  memoryMode: WorkspaceMemoryMode,
) => ipc<Workspace>("cmd_workspace_set_memory_mode", {
  workspaceId,
  expectedGeneration,
  memoryMode,
});

export const workspaceDashboard = async (workspacePath: string) => {
  try {
    return await ipc<WorkspaceDashboard>("cmd_workspace_dashboard", { workspacePath });
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    return browserMockWorkspaceDashboard(workspacePath);
  }
};

export const projectActivityList = async (workspacePath: string, cursor: string | null = null, limit = 30) => {
  try {
    return await ipc<ProjectActivityPage>("cmd_project_activity_list", { workspacePath, cursor, limit });
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    return browserMockActivityList(workspacePath);
  }
};

export const activityList = async (cursor: string | null = null, limit = 30) => {
  try {
    return await ipc<ProjectActivityPage>("cmd_activity_list", { cursor, limit });
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    return browserMockActivityList();
  }
};

// ---------- 搜索 ----------
export const quickOpen = async (workspacePath: string, query: string, limit = 20) => {
  try {
    return await ipc<string[]>("cmd_quick_open", { workspacePath, query, limit });
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    const needle = query.toLocaleLowerCase();
    return Object.keys(browserMockFiles).filter((path) => path.toLocaleLowerCase().includes(needle)).slice(0, limit);
  }
};

export const globalSearch = (workspacePath: string, query: string, limit = 50) =>
  ipc<SearchMatch[]>("cmd_global_search", { workspacePath, query, limit });

// ---------- 终端 ----------
export const terminalList = () => ipc<TerminalInfo[]>("cmd_terminal_list");

export const terminalCreate = (shell: string, workspacePath: string) =>
  ipc<string>("cmd_terminal_create", { shell, workspacePath });

export const terminalCreateCodex = (workspacePath: string) =>
  ipc<string>("cmd_terminal_create_codex", { workspacePath });

export const terminalSend = (id: string, text: string, pressEnter = true) =>
  ipc<void>("cmd_terminal_send", { id, text, pressEnter });

export const terminalRead = (id: string) => ipc<string>("cmd_terminal_read", { id });

export const terminalSnapshot = (id: string) => ipc<string>("cmd_terminal_snapshot", { id });

export const terminalRawSnapshot = (id: string) =>
  ipc<TerminalRawSnapshot>("cmd_terminal_raw_snapshot", { id });

export const terminalRawSince = (id: string, cursor: number) =>
  ipc<TerminalRawBatch>("cmd_terminal_raw_since", { id, cursor });

/** PTY reader 的轻量输出就绪信号；字节内容仍由 terminalRawSince 增量读取。 */
export const onTerminalOutput = (handler: (terminalId: string) => void): Promise<UnlistenFn> => {
  if (shouldUseBrowserMock()) return Promise.resolve(() => {});
  return listen<string>("terminal-output", (event) => handler(event.payload));
};

export const terminalKill = (id: string) => ipc<void>("cmd_terminal_kill", { id });

export const terminalResize = (id: string, cols: number, rows: number) =>
  ipc<void>("cmd_terminal_resize", { id, cols, rows });

// ---------- 恢复 / 支持包 ----------
export const recoveryData = async () => {
  try {
    return await ipc<RecoveryPageData>("cmd_recovery_data");
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    return { interrupted_tasks: [], orphaned_permissions: 0 };
  }
};

export const recoveryCleanup = () => ipc<RecoveryCleanupResult>("cmd_recovery_cleanup");

export const supportBundle = async (outputDir: string) => {
  try {
    return await ipc<string>("cmd_support_bundle", { outputDir });
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    return `${outputDir.replace(/[\\/]+$/, "")}/r-code-support-preview.json`;
  }
};

export const supportBundleChoose = async () => {
  try {
    return await ipc<string | null>("cmd_support_bundle_choose");
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    return "C:/Users/preview/Downloads/r-code-support-preview.json";
  }
};

export const supportPreview = async () => {
  try {
    return await ipc<SupportBundlePreview>("cmd_support_preview");
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    return {
      version: "0.1.0-preview",
      platform: navigator.platform || "browser",
      generated_at: new Date().toISOString(),
      logs: [],
      config_summary: {},
      db_stats: { task_count: 7, run_count: 12, tool_call_count: 34 },
    };
  }
};

// ---------- 回放 / 会话 ----------
export const replay = (sessionId: string, depth: ReplayDepth) =>
  ipc<ReplayEntry[]>("cmd_replay", { sessionId, depth });

export const sessionMessages = async (taskId: string) => {
  try {
    return await ipc<SessionMessage[]>("cmd_session_messages", { taskId });
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    return browserMockMessages(taskId);
  }
};

/** Read a validated historical branch without changing the task's active branch. */
export const sessionMessagesForBranch = (taskId: string, branchId: string) =>
  ipc<SessionMessage[]>("cmd_session_messages_for_branch", { taskId, branchId });

/** 读取子代理的隔离日志；其中只包含公开生命周期、工具审计和最终可见结果。 */
export const subagentSessionMessages = async (taskId: string, subagentId: string) => {
  try {
    return await ipc<SessionMessage[]>("cmd_subagent_session_messages", { taskId, subagentId });
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    return browserMockSubagentMessages(taskId, subagentId);
  }
};

// ---------- 旧版项目记忆文件风险状态 ----------
export const memoryOverview = () => ipc<MemoryOverview>("cmd_memory_overview");

export const memoryUpdateSettings = (update: MemoryReviewSettingsUpdate) =>
  ipc<MemoryReviewSettingsView>("cmd_memory_update_settings", { update });

export interface MemoryReviewScopeRequest {
  workspaceId: string | null;
  workspacePath: string | null;
}

export const memoryReviewNow = (scope: MemoryReviewScopeRequest) =>
  ipc<string | null>("cmd_memory_review_now", {
    workspaceId: scope.workspaceId,
    workspacePath: scope.workspacePath,
  });

export const memoryRetryJob = (jobId: string) =>
  ipc<void>("cmd_memory_retry_job", { jobId });

export const memoryCancelJob = (jobId: string) =>
  ipc<void>("cmd_memory_cancel_job", { jobId });

export const memoryAddEntry = (draft: MemoryEntryDraft) =>
  ipc<MemoryEntry>("cmd_memory_add_entry", { draft });

export const memoryEditEntry = (entryId: string, edit: MemoryEntryEdit) =>
  ipc<MemoryEntry>("cmd_memory_edit_entry", { entryId, edit });

export const memoryDeleteEntry = (entryId: string, expectedVersion: number) =>
  ipc<void>("cmd_memory_delete_entry", { entryId, expectedVersion });

export const memoryApproveCandidate = (candidateId: string, editedContent: string | null = null) =>
  ipc<MemoryEntry>("cmd_memory_approve_candidate", { candidateId, editedContent });

export const memoryRejectCandidate = (candidateId: string) =>
  ipc<void>("cmd_memory_reject_candidate", { candidateId });

export const memoryClearAll = () =>
  ipc<MemoryReviewSettingsView>("cmd_memory_clear_all");

// ---------- 旧版项目记忆文件风险状态 ----------
export const legacyMemoryStatus = (workspacePath: string) =>
  ipc<LegacyMemoryStatus>("cmd_legacy_memory_status", { workspacePath });

// ---------- 设置 ----------
export const settingsGet = async () => {
  try {
    return await ipc<SettingsResponse>("cmd_settings_get");
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    return browserMockSettings;
  }
};

// ---------- MCP / native web ----------
export const mcpSnapshot = () => ipc<McpManagerSnapshot>("cmd_mcp_snapshot");

export const mcpUpsert = (request: McpUpsertRequest) =>
  ipc<McpServerView>("cmd_mcp_upsert", { request });

export const mcpRemove = (serverId: string) =>
  ipc<void>("cmd_mcp_remove", { serverId });

export const mcpToggle = (serverId: string, enabled: boolean, confirmationToken: string | null = null) =>
  ipc<McpToggleResult>("cmd_mcp_toggle", { serverId, enabled, confirmationToken });

export const mcpTestConnection = (serverId: string) =>
  ipc<McpToolDescriptor[]>("cmd_mcp_test_connection", { serverId });

export const mcpCredentialStatus = (serverId: string) =>
  ipc<McpCredentialStatus[]>("cmd_mcp_credential_status", { serverId });

export const mcpSetCredential = (serverId: string, name: string, value: string) =>
  ipc<void>("cmd_mcp_set_credential", { serverId, name, value });

export const mcpDeleteCredential = (serverId: string, name: string) =>
  ipc<void>("cmd_mcp_delete_credential", { serverId, name });

export const mcpMarketSearch = (query: string | null = null, cursor: string | null = null, limit = 20) =>
  ipc<McpMarketPage>("cmd_mcp_market_search", { query, cursor, limit });

export const mcpMarketPrepareInstall = (request: McpMarketInstallRequest) =>
  ipc<McpLaunchPreview>("cmd_mcp_market_prepare_install", { request });

export const mcpMarketInstall = (request: McpMarketInstallRequest, confirmationToken: string) =>
  ipc<McpServerView>("cmd_mcp_market_install", { request, confirmationToken });

export const onMcpStatus = (
  handler: (statuses: Array<Pick<McpServerView, "id" | "state" | "tool_count" | "error_code">>) => void,
): Promise<UnlistenFn> => {
  if (shouldUseBrowserMock()) return Promise.resolve(() => {});
  return listen<Array<Pick<McpServerView, "id" | "state" | "tool_count" | "error_code">>>(
    "mcp-status",
    (event) => handler(event.payload),
  );
};

/** 内置模型服务目录。编译期常量，进程内只需拉一次。 */
export const providerCatalog = async () => {
  try {
    return await ipc<ProviderCatalog>("cmd_provider_catalog");
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    return browserMockProviderCatalog;
  }
};

/** 从当前服务实时读取模型目录；失败时由设置页保留预设和手动输入兜底。 */
export const providerModels = (request: ProviderModelsInput) =>
  ipc<ProviderModelsResponse>("cmd_provider_models", { request });

export const settingsSet = (key: string, value: unknown) =>
  ipc<void>("cmd_settings_set", { key, value });

export const settingsSaveProvider = (provider: ProviderSettingsInput) =>
  ipc<void>("cmd_settings_save_provider", { provider });

export const settingsSelectProvider = (name: string) =>
  ipc<void>("cmd_settings_select_provider", { name });

export const settingsDeleteProvider = (name: string) =>
  ipc<void>("cmd_settings_delete_provider", { name });

/** RTK is app-scoped: status never mutates system PATH or the user's global Codex files. */
export const rtkStatus = () => ipc<RtkStatus>("cmd_rtk_status");

/** Enabling may install a verified official release; disabling only renames the policy marker. */
export const rtkSetEnabled = (enabled: boolean) =>
  ipc<RtkStatus>("cmd_rtk_set_enabled", { enabled });

let codexIntegrationStatusRequest: Promise<CodexIntegrationStatus> | null = null;

const loadCodexIntegrationStatus = async () => {
  try {
    return await ipc<CodexIntegrationStatus>("cmd_codex_integration_status");
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    return browserMockCodexIntegrationStatus();
  }
};

/** Coalesce startup consumers so Home and the Codex gate do not launch the same CLI probes twice. */
export const codexIntegrationStatus = () => {
  if (codexIntegrationStatusRequest) return codexIntegrationStatusRequest;

  const request = loadCodexIntegrationStatus();
  codexIntegrationStatusRequest = request;
  const clear = () => {
    if (codexIntegrationStatusRequest === request) codexIntegrationStatusRequest = null;
  };
  void request.then(clear, clear);
  return request;
};

/** 用户在确认弹窗授权后，通过 npm 安装官方 Codex CLI，并返回最新状态。 */
export const codexInstallCli = async () => {
  try {
    return await ipc<CodexIntegrationStatus>("cmd_codex_install_cli");
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    await new Promise((resolve) => window.setTimeout(resolve, 900));
    return browserMockInstallCodexCli();
  }
};

export const codexStartLogin = async () => {
  try {
    await ipc<void>("cmd_codex_start_login");
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    browserMockAuthenticateCodex();
  }
};

export const codexStartDeviceLogin = async () => {
  try {
    await ipc<void>("cmd_codex_start_device_login");
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    browserMockAuthenticateCodex();
  }
};

export const codexInstallSkill = async () => {
  try {
    await ipc<void>("cmd_codex_install_skill");
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    browserMockInstallCodexSkill();
  }
};

/** 用户确认后将本机 R-Code stdio MCP server 注册到 Codex 配置。 */
export const codexInstallMcpServer = async () => {
  try {
    await ipc<void>("cmd_codex_install_mcp_server");
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    browserMockEnableCodexMcp();
  }
};

/** 一次更新协作 Skill 并补齐 R-Code 的 Codex MCP 配置。 */
export const codexSetupCollaboration = async () => {
  try {
    return await ipc<CodexIntegrationStatus>("cmd_codex_setup_collaboration");
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    await new Promise((resolve) => window.setTimeout(resolve, 500));
    return browserMockSetupCodexCollaboration();
  }
};

/** 读取当前 Codex CLI 实际可用模型与运行偏好。 */
export const codexCliPreferences = async () => {
  try {
    return await ipc<CodexCliPreferences>("cmd_codex_cli_preferences");
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    await new Promise((resolve) => window.setTimeout(resolve, 450));
    return browserMockCodexCliPreferences();
  }
};

/** 空模型字段会从 config.toml 移除覆盖；权限预设由 Codex 子代理启动时读取。 */
export const codexSaveCliPreferences = async (
  model: string,
  reasoningEffort: string,
  verbosity: string,
  permissionMode: string,
) => {
  const args = {
    model: model || null,
    reasoningEffort: reasoningEffort || null,
    verbosity: verbosity || null,
    permissionMode: permissionMode || null,
  };
  try {
    return await ipc<CodexCliPreferences>("cmd_codex_save_cli_preferences", args);
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    await new Promise((resolve) => window.setTimeout(resolve, 500));
    return browserMockSaveCodexCliPreferences(
      args.model,
      args.reasoningEffort,
      args.verbosity,
      args.permissionMode,
    );
  }
};

// ---------- 日志 ----------
export const logsTail = async (limit = 200, level?: string) => {
  try {
    return await ipc<LogEntry[]>("cmd_logs_tail", { limit, level: level ?? null });
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    return [];
  }
};
