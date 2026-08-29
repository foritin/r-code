/**
 * Tauri IPC 封装 — 前端 → 后端全部命令的 typed wrapper。
 * 后端命令注册于 src-tauri/src/main.rs；参数一律 camelCase（Tauri v2 约定）。
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { announceRuntimeSettingsChanged } from "./onboarding";
import type {
  AgentEvent,
  AgentEventEnvelope,
  AgentRun,
  AgentSendMode,
  ChangeDiff,
  NotificationPage,
  NativeNotificationEvent,
  NativeNotificationOpenPayload,
  NativeNotificationPermissionState,
  PlanEntryOfferView,
  PlanningStatusView,
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
  SubagentSessionMessagePage,
  SubagentSessionMessagePageRequest,
  AttachmentInput,
  AttachmentPreviewPayload,
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
  ProviderBalanceInput,
  ProviderBalanceResponse,
  SubagentPoolConfig,
  SubagentPoolSnapshot,
  SubagentProviderCatalogSnapshot,
  SubagentProviderProbeBatchResponse,
  SubagentProviderProbeRequest,
  SubagentProviderProbeResponse,
  CodexCliPreferences,
  CodexCliSyncResult,
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
  PlatformCapabilities,
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
  browserMockSyncCodexCli,
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
import { commandErrorPayload, IpcCommandError, toUserFacingIpcError } from "./ipc-error";
import type { UpdaterSnapshot } from "./updater-contract";
import { APPLICATION_UPDATER_STATE_EVENT } from "./updater-contract";

export { IpcCommandError, UserFacingIpcError, type UserFacingErrorPayload } from "./ipc-error";

export const PROJECT_CONVERSATION_LIMIT_REACHED_CODE =
  "PROJECT_CONVERSATION_LIMIT_REACHED";

/** RTK 安装后二进制被安全软件（通常为 Windows Defender）拦截或隔离。 */
export const RTK_BLOCKED_BY_SECURITY_SOFTWARE =
  "RTK_BLOCKED_BY_SECURITY_SOFTWARE";

const LEGACY_COMPANION_ENSURE_MISSING_ERROR =
  "Command cmd_companion_ensure not found";

function isLegacyCompanionEnsureMissing(cause: unknown): boolean {
  if (typeof cause === "string") return cause === LEGACY_COMPANION_ENSURE_MISSING_ERROR;
  if (cause instanceof Error) return cause.message === LEGACY_COMPANION_ENSURE_MISSING_ERROR;
  if (typeof cause !== "object" || cause == null) return false;
  return (cause as Record<string, unknown>).message === LEGACY_COMPANION_ENSURE_MISSING_ERROR;
}

async function ipc<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  try {
    if (shouldUseBrowserMock()) {
      return await browserMockInvoke(command, args) as T;
    }
    return await invoke<T>(command, args);
  } catch (cause) {
    const userFacingError = toUserFacingIpcError(cause);
    if (userFacingError) throw userFacingError;
    const payload = commandErrorPayload(cause);
    if (payload) throw new IpcCommandError(payload);
    throw cause;
  }
}

// ---------- 系统 ----------
export const ping = () => ipc<boolean>("ping");
export const appQuit = () => ipc<void>("cmd_app_quit");
export const companionEnsure = async (): Promise<boolean> => {
  try {
    return await ipc<boolean>("cmd_companion_ensure");
  } catch (cause) {
    // Vite can hot-reload this frontend while the already-running native host still predates the
    // ensure command. That host created the companion during startup, so preserve the existing
    // window until the next normal application restart. Never mask any other IPC/window failure.
    if (!isLegacyCompanionEnsureMissing(cause)) throw cause;
    console.info("Companion recovery command is unavailable in the running legacy host; using its startup-created window.");
    return true;
  }
};
export const platformCapabilities = () => ipc<PlatformCapabilities>("cmd_platform_capabilities");

// ---------- Application Updater ----------
export const updaterStatus = () => ipc<UpdaterSnapshot>("cmd_updater_status");
export const updaterCheck = (force = true) =>
  ipc<UpdaterSnapshot>("cmd_updater_check", { force });
export const updaterDownload = () => ipc<UpdaterSnapshot>("cmd_updater_download");
export const updaterInstall = () => ipc<UpdaterSnapshot>("cmd_updater_install");
export const updaterRestart = () => ipc<void>("cmd_updater_restart");
export const onUpdaterState = (
  handler: (snapshot: UpdaterSnapshot) => void,
): Promise<UnlistenFn> => {
  if (shouldUseBrowserMock()) return Promise.resolve(() => {});
  return listen<UpdaterSnapshot>(APPLICATION_UPDATER_STATE_EVENT, (event) => {
    handler(event.payload);
  });
};

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

/** Native folder picker plus one-time task binding; cancellation returns null without side effects. */
export const taskChooseWorkspace = (taskId: string) =>
  ipc<Task | null>("cmd_task_choose_workspace", { taskId });

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

export const taskForkContext = async (taskId: string) => {
  const branch = await ipc<SessionBranch>("cmd_task_fork_context", { taskId });
  invalidateSessionMessages(taskId);
  return branch;
};

export const taskClearContext = async (taskId: string) => {
  const branch = await ipc<SessionBranch>("cmd_task_clear_context", { taskId });
  invalidateSessionMessages(taskId);
  return branch;
};

export const taskCompactContext = async (taskId: string, focus?: string) => {
  const result = await ipc<ContextCompactionResult>("cmd_task_compact_context", {
    taskId,
    focus: focus?.trim() || null,
  });
  if (result.compacted) invalidateSessionMessages(taskId);
  return result;
};

export const planEntryDecide = (input: {
  offerId: string;
  expectedRevision: number;
  decision: "accept" | "continue" | "close" | "escape";
  idempotencyKey: string;
}) =>
  ipc<PlanEntryOfferView>("cmd_plan_entry_decide", {
    input: {
      offer_id: input.offerId,
      expected_revision: input.expectedRevision,
      decision: input.decision,
      idempotency_key: input.idempotencyKey,
    },
  });

export const planEntryRetryContinuation = (offerId: string) =>
  ipc<PlanEntryOfferView>("cmd_plan_entry_retry_continuation", { offerId });

export const planningStatus = () => ipc<PlanningStatusView>("cmd_planning_status");

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
  attachmentIds: string[] = [],
) => ipc<void>("cmd_agent_send", {
  taskId,
  message,
  mode,
  attachments,
  attachmentIds,
}).then((result) => {
  invalidateSessionMessages(taskId);
  return result;
});

/** 附件 staging（docs/support/archive/implementation/multimodal-attachments-and-deepseek-plan-anchoring-implementation.md §2.2 边界 1）：一次性 Base64 → Blob 引用。 */
export const attachmentStage = (taskId: string, attachment: AttachmentInput) =>
  ipc<import("./types").AttachmentRefDto>("cmd_attachment_stage", { taskId, attachment });

/** 删除草稿附件：立即释放 staged 引用与 Blob 计数。 */
export const attachmentDiscard = (taskId: string, attachmentId: string) =>
  ipc<void>("cmd_attachment_discard", { taskId, attachmentId });

export const agentAbort = (taskId: string) => ipc<void>("cmd_agent_abort", { taskId });

/** 按引用取回时间线图片附件预览（OCR 落盘原图或会话内联 Image 块）。 */
export const agentAttachmentPreview = (taskId: string, reference: string) =>
  ipc<AttachmentPreviewPayload>("cmd_agent_attachment_preview", { taskId, reference });

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
  ipc<AgentQueueSteerResult>("cmd_agent_queue_steer", { taskId, queueId }).then((result) => {
    // `steered` and `started` durably append a user event; invalidating all successful outcomes
    // keeps the wrapper correct if the backend promotes `queued_next` in the same transaction.
    invalidateSessionMessages(taskId);
    return result;
  });

export const agentResend = (taskId: string, messageId: string, message: string) =>
  ipc<void>("cmd_agent_resend", { taskId, messageId, message }).then((result) => {
    invalidateSessionMessages(taskId);
    return result;
  });

/** 订阅 agent 流式事件（后端 drain 循环 emit 的 "agent-event"）。 */
export const onAgentEvent = (handler: (taskId: string, event: AgentEvent) => void): Promise<UnlistenFn> => {
  if (shouldUseBrowserMock()) return Promise.resolve(() => {});
  return listen<AgentEventEnvelope>("agent-event", (e) => {
    // Agent events are persisted to the session log before they reach the UI.
    // Do not let the short coalescing window hide a just-written message/tool event.
    invalidateSessionMessages(e.payload.task_id);
    handler(e.payload.task_id, e.payload.event);
  });
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

export const NATIVE_NOTIFICATION_EVENT = "r-code:native-notification";
export const NATIVE_NOTIFICATION_OPEN_EVENT = "r-code:native-notification-open";

export const nativeNotificationPermissionState = async (): Promise<NativeNotificationPermissionState> => {
  try {
    return await ipc<NativeNotificationPermissionState>("cmd_native_notification_permission_state");
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    return "unavailable";
  }
};

export const nativeNotificationRequestPermission = async (): Promise<NativeNotificationPermissionState> => {
  try {
    return await ipc<NativeNotificationPermissionState>("cmd_native_notification_request_permission");
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    return "unavailable";
  }
};

export const nativeNotificationSetLocale = async (locale: string): Promise<void> => {
  try {
    await ipc<void>("cmd_native_notification_set_locale", { locale });
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
  }
};

export const onNativeNotification = (
  handler: (event: NativeNotificationEvent) => void,
): Promise<UnlistenFn> => {
  if (shouldUseBrowserMock()) return Promise.resolve(() => {});
  return listen<NativeNotificationEvent>(NATIVE_NOTIFICATION_EVENT, (event) => handler(event.payload));
};

export const onNativeNotificationOpen = (
  handler: (payload: NativeNotificationOpenPayload) => void,
): Promise<UnlistenFn> => {
  if (shouldUseBrowserMock()) return Promise.resolve(() => {});
  return listen<NativeNotificationOpenPayload>(NATIVE_NOTIFICATION_OPEN_EVENT, (event) => handler(event.payload));
};

// ---------- 变更 / 审查 ----------
export const changesList = (taskId: string) =>
  ipc<FileChange[]>("cmd_changes_list", { taskId });

export const rollbackFile = (taskId: string, path: string) =>
  ipc<string>("cmd_rollback_file", { taskId, path });

export const rollbackTask = (taskId: string) =>
  ipc<string[]>("cmd_rollback_task", { taskId });

export const rollbackTaskToCheckpoint = (taskId: string) =>
  ipc<string[]>("cmd_rollback_task_to_checkpoint", { taskId });

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

export const workflowSkillsList = (workspacePath?: string | null) =>
  ipc<import("./types").WorkflowSkill[]>("cmd_workflow_skills_list", { workspacePath: workspacePath ?? null });

export const workflowSkillSave = (draft: import("./types").WorkflowSkillDraft, workspacePath?: string | null) =>
  ipc<import("./types").WorkflowSkill>("cmd_workflow_skill_save", { draft, workspacePath: workspacePath ?? null });

export const workflowSkillReset = (id: string) =>
  ipc<import("./types").WorkflowSkill>("cmd_workflow_skill_reset", { id });

export const workflowSkillDelete = (id: string, scope: import("./types").WorkflowSkillScope, workspacePath?: string | null) =>
  ipc<void>("cmd_workflow_skill_delete", { id, scope, workspacePath: workspacePath ?? null });

export const workflowSkillSyncToGlobal = (id: string, workspacePath: string) =>
  ipc<import("./types").WorkflowSkill>("cmd_workflow_skill_sync_to_global", { id, workspacePath });

export const knowledgePromptsGet = (workspacePath?: string | null) =>
  ipc<import("./types").KnowledgePromptSnapshot>("cmd_knowledge_prompts_get", { workspacePath: workspacePath ?? null });

export const knowledgePromptsSave = (
  workspacePath: string | null,
  mode: import("./types").ProjectPromptMode,
  mainAgent: string,
  subagent: string,
) => ipc<import("./types").KnowledgePromptSnapshot>("cmd_knowledge_prompts_save", {
  workspacePath,
  mode,
  mainAgent,
  subagent,
});

export const knowledgePromptsReset = (workspacePath?: string | null) =>
  ipc<import("./types").KnowledgePromptSnapshot>("cmd_knowledge_prompts_reset", { workspacePath: workspacePath ?? null });

export const changeRequest = async (taskId: string, message: string) => {
  try {
    return await ipc<void>("cmd_change_request", { taskId, message });
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    browserMockChangeRequest(taskId);
  }
};

export const changeDiff = (taskId: string, path: string, runId?: string | null) =>
  ipc<ChangeDiff>("cmd_change_diff", { taskId, path, runId: runId ?? null });

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
export const terminalList = (taskId: string) =>
  ipc<TerminalInfo[]>("cmd_terminal_list", { taskId });

export const terminalCreate = (taskId: string, shell: string) =>
  ipc<string>("cmd_terminal_create", { taskId, shell });

export const terminalCreateCodex = (taskId: string) =>
  ipc<string>("cmd_terminal_create_codex", { taskId });

export const terminalSend = (taskId: string, id: string, text: string, pressEnter = true) =>
  ipc<void>("cmd_terminal_send", { taskId, id, text, pressEnter });

export const terminalRead = (taskId: string, id: string) =>
  ipc<string>("cmd_terminal_read", { taskId, id });

export const terminalSnapshot = (taskId: string, id: string) =>
  ipc<string>("cmd_terminal_snapshot", { taskId, id });

export const terminalRawSnapshot = (taskId: string, id: string) =>
  ipc<TerminalRawSnapshot>("cmd_terminal_raw_snapshot", { taskId, id });

export const terminalRawSince = (taskId: string, id: string, cursor: number) =>
  ipc<TerminalRawBatch>("cmd_terminal_raw_since", { taskId, id, cursor });

/** PTY reader 的轻量输出就绪信号；字节内容仍由 terminalRawSince 增量读取。 */
export const onTerminalOutput = (handler: (terminalId: string) => void): Promise<UnlistenFn> => {
  if (shouldUseBrowserMock()) return Promise.resolve(() => {});
  return listen<string>("terminal-output", (event) => handler(event.payload));
};

export const terminalKill = (taskId: string, id: string) =>
  ipc<void>("cmd_terminal_kill", { taskId, id });

export const terminalResize = (taskId: string, id: string, cols: number, rows: number) =>
  ipc<void>("cmd_terminal_resize", { taskId, id, cols, rows });

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

interface SessionMessagesRequest {
  generation: number;
  promise: Promise<SessionMessage[]>;
  settled: boolean;
  stale: boolean;
}

/**
 * Room 的 Timeline、Composer 输入历史和 Summary 审计都消费同一份会话投影。
 * 会话切换时它们会在同一个 React 提交中挂载；共享这一趟 IPC，避免把同一个
 * JSONL 文件在 Rust 端读取 / 解析三次、再跨 WebView 序列化三次。
 *
 * 仅复用当前正在进行的请求；完成后若没有失效，就保留一个很短的热窗口，让
 * 相邻 effect 仍可命中，同时不会把运行中新落盘的消息长期藏在缓存后面。
 */
const SESSION_MESSAGES_HOT_MS = 250;
const sessionMessageRequests = new Map<string, SessionMessagesRequest>();
const sessionMessageGenerations = new Map<string, number>();

function requestSessionMessages(taskId: string): Promise<SessionMessage[]> {
  const generation = sessionMessageGenerations.get(taskId) ?? 0;
  const cached = sessionMessageRequests.get(taskId);
  if (cached && cached.generation === generation && !cached.stale) return cached.promise;

  const request: SessionMessagesRequest = {
    generation,
    promise: Promise.resolve([]),
    settled: false,
    stale: false,
  };
  const promise = ipc<SessionMessage[]>("cmd_session_messages", { taskId })
    .catch((error) => {
      if (!shouldUseBrowserMock()) throw error;
      return browserMockMessages(taskId);
    })
    .finally(() => {
      request.settled = true;
      if (request.stale) {
        if (sessionMessageRequests.get(taskId) === request) sessionMessageRequests.delete(taskId);
        return;
      }
      window.setTimeout(() => {
        if (sessionMessageRequests.get(taskId) === request) sessionMessageRequests.delete(taskId);
      }, SESSION_MESSAGES_HOT_MS);
    });
  request.promise = promise;
  sessionMessageRequests.set(taskId, request);
  return promise;
}

/** Mark the current projection stale after a command that can append or replace session JSONL. */
export function invalidateSessionMessages(taskId: string): void {
  sessionMessageGenerations.set(taskId, (sessionMessageGenerations.get(taskId) ?? 0) + 1);
  const request = sessionMessageRequests.get(taskId);
  if (!request) return;
  request.stale = true;
  if (sessionMessageRequests.get(taskId) === request) sessionMessageRequests.delete(taskId);
}

export const sessionMessages = async (taskId: string) => {
  return await requestSessionMessages(taskId);
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

/**
 * Read only the latest/added/preceding window of a subagent log. Pollers pass `next_cursor` back as
 * `after_cursor`, so an idle poll avoids rereading, parsing and serializing the complete JSONL file.
 */
export const subagentSessionMessagePage = (
  taskId: string,
  subagentId: string,
  request: SubagentSessionMessagePageRequest = {},
) => ipc<SubagentSessionMessagePage>("cmd_subagent_session_message_page", {
  taskId,
  subagentId,
  request,
});

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
interface SettingsRequest {
  generation: number;
  promise: Promise<SettingsResponse>;
  forced: boolean;
  settled: boolean;
  stale: boolean;
}

const SETTINGS_HOT_MS = 250;
let settingsGeneration = 0;
let settingsRequest: SettingsRequest | null = null;

function invalidateSettingsRequest(): void {
  settingsGeneration += 1;
  const request = settingsRequest;
  if (!request) return;
  request.stale = true;
  if (settingsRequest === request) settingsRequest = null;
}

export const settingsGet = (force = false) => {
  if (
    force
    && settingsRequest?.forced
    && !settingsRequest.settled
    && !settingsRequest.stale
  ) return settingsRequest.promise;
  if (force) invalidateSettingsRequest();
  if (
    settingsRequest
    && settingsRequest.generation === settingsGeneration
    && !settingsRequest.stale
  ) return settingsRequest.promise;
  const request: SettingsRequest = {
    generation: settingsGeneration,
    promise: Promise.resolve(browserMockSettings),
    forced: force,
    settled: false,
    stale: false,
  };
  const promise = ipc<SettingsResponse>("cmd_settings_get")
    .catch((error) => {
      if (!shouldUseBrowserMock()) throw error;
      return browserMockSettings;
    })
    .finally(() => {
      request.settled = true;
      if (request.stale) {
        if (settingsRequest === request) settingsRequest = null;
        return;
      }
      window.setTimeout(() => {
        if (settingsRequest === request) settingsRequest = null;
      }, SETTINGS_HOT_MS);
    });
  request.promise = promise;
  settingsRequest = request;
  return promise;
};

const afterSettingsMutation = <T>(result: T): T => {
  invalidateSettingsRequest();
  return result;
};

const afterProviderMutation = <T>(result: T): T => {
  afterSettingsMutation(result);
  announceRuntimeSettingsChanged();
  return result;
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

/** 模型胶囊悬停时按需读取 DeepSeek 官方账户余额。 */
export const providerBalance = (request: ProviderBalanceInput) =>
  ipc<ProviderBalanceResponse>("cmd_provider_balance", { request });

export type ExecutionEnvProbe = {
  dialect: string;
  program: string;
  git_bash_detected: boolean;
  /** 已保存的 execution.bash_shell_path（null=自动探测；空串=强制回落）。 */
  configured_override?: string | null;
};

/** R-OPS-01 执行环境探测（当前 shell 解析档/路径/是否检出 Git Bash）。 */
export const executionEnvProbe = (): Promise<ExecutionEnvProbe> =>
  ipc<ExecutionEnvProbe>("cmd_execution_env_probe");

export const settingsSet = (key: string, value: unknown) =>
  ipc<void>("cmd_settings_set", { key, value }).then(afterSettingsMutation);

export const settingsSaveProvider = (provider: ProviderSettingsInput) =>
  ipc<void>("cmd_settings_save_provider", { provider }).then(afterProviderMutation);

export const settingsSelectProvider = (name: string) =>
  ipc<void>("cmd_settings_select_provider", { name }).then(afterProviderMutation);

export const settingsDeleteProvider = (name: string) =>
  ipc<void>("cmd_settings_delete_provider", { name }).then(afterProviderMutation);

// ---------- 子代理 Provider 候选池 ----------

/** 只返回已配置的全局 API Provider 与受信任 Codex CLI，不触发联网探测。 */
export const subagentProviderCatalog = () =>
  ipc<SubagentProviderCatalogSnapshot>("cmd_subagent_provider_catalog");

/** 用户显式触发的单来源最小连通测试；Host 不接受前端伪造的健康布尔值。 */
export const subagentProviderTest = (request: SubagentProviderProbeRequest) =>
  ipc<SubagentProviderProbeResponse>("cmd_subagent_provider_test", { request });

/** 有界批测，逐项返回成功或脱敏失败，不因单项失败取消其他来源。 */
export const subagentProviderTestBatch = (requests: SubagentProviderProbeRequest[]) =>
  ipc<SubagentProviderProbeBatchResponse>("cmd_subagent_provider_test_batch", { requests });

/** global-only、带 revision 的候选池与健康快照。 */
export const subagentPoolSnapshot = () =>
  ipc<SubagentPoolSnapshot>("cmd_subagent_pool_snapshot");

/** 按旧 revision 原子替换整个候选池；冲突或任一无效槽位会整次拒绝。 */
export const subagentPoolSave = (revision: string, pool: SubagentPoolConfig) =>
  ipc<SubagentPoolSnapshot>("cmd_subagent_pool_save", { revision, pool });

/** RTK is app-scoped: status never mutates system PATH or the user's global Codex files. */
export const rtkStatus = () => ipc<RtkStatus>("cmd_rtk_status");

/** Enabling may install a verified official release; disabling only renames the policy marker. */
export const rtkSetEnabled = (enabled: boolean) =>
  ipc<RtkStatus>("cmd_rtk_set_enabled", { enabled });

/** 打开 Windows 安全中心「排除项」页，用于在被 Defender 隔离后由用户手动放行 RTK。 */
export const rtkOpenSecurityExclusions = () =>
  ipc<void>("cmd_rtk_open_security_exclusions");

let codexIntegrationStatusRequest: Promise<CodexIntegrationStatus> | null = null;
let codexIntegrationStatusRequestForced = false;
let codexIntegrationStatusSnapshot: CodexIntegrationStatus | null = null;
let codexIntegrationStatusGeneration = 0;

function invalidateCodexIntegrationStatus(): number {
  codexIntegrationStatusGeneration += 1;
  codexIntegrationStatusSnapshot = null;
  codexIntegrationStatusRequest = null;
  codexIntegrationStatusRequestForced = false;
  return codexIntegrationStatusGeneration;
}

const loadCodexIntegrationStatus = async () => {
  try {
    return await ipc<CodexIntegrationStatus>("cmd_codex_integration_status");
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    return browserMockCodexIntegrationStatus();
  }
};

/**
 * Codex 安装 / 登录 / MCP 状态属于应用级状态，而非会话状态。成功探测后复用快照，
 * 避免每次 Room 挂载都在 Windows 上重新启动 CLI/version/login 子进程。
 * 所有会改变该状态的操作都会更新或清除此快照。
 */
export const codexIntegrationStatus = (force = false) => {
  if (force && codexIntegrationStatusRequest && codexIntegrationStatusRequestForced) {
    return codexIntegrationStatusRequest;
  }
  if (force) invalidateCodexIntegrationStatus();
  if (!force && codexIntegrationStatusSnapshot) {
    return Promise.resolve(codexIntegrationStatusSnapshot);
  }
  if (codexIntegrationStatusRequest) return codexIntegrationStatusRequest;

  const generation = codexIntegrationStatusGeneration;
  const request = loadCodexIntegrationStatus().then((status) => {
    if (generation === codexIntegrationStatusGeneration) {
      codexIntegrationStatusSnapshot = status;
    }
    return status;
  });
  codexIntegrationStatusRequest = request;
  codexIntegrationStatusRequestForced = force;
  const clear = () => {
    if (codexIntegrationStatusRequest === request) {
      codexIntegrationStatusRequest = null;
      codexIntegrationStatusRequestForced = false;
    }
  };
  void request.then(clear, clear);
  return request;
};

/** 用户在确认弹窗授权后，通过 npm 安装官方 Codex CLI，并返回最新状态。 */
export const codexInstallCli = async () => {
  invalidateCodexIntegrationStatus();
  try {
    const status = await ipc<CodexIntegrationStatus>("cmd_codex_install_cli");
    invalidateCodexIntegrationStatus();
    codexIntegrationStatusSnapshot = status;
    return status;
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    await new Promise((resolve) => window.setTimeout(resolve, 900));
    const status = browserMockInstallCodexCli();
    invalidateCodexIntegrationStatus();
    codexIntegrationStatusSnapshot = status;
    return status;
  }
};

/** 进入 Codex 运行时设置时检查更新，并在官方 CLI 报告有新版时自动升级。 */
export const codexSyncCli = async () => {
  invalidateCodexIntegrationStatus();
  try {
    const result = await ipc<CodexCliSyncResult>("cmd_codex_sync_cli");
    invalidateCodexIntegrationStatus();
    codexIntegrationStatusSnapshot = result.status;
    return result;
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    await new Promise((resolve) => window.setTimeout(resolve, 250));
    const result = browserMockSyncCodexCli();
    invalidateCodexIntegrationStatus();
    codexIntegrationStatusSnapshot = result.status;
    return result;
  }
};

export const codexStartLogin = async () => {
  invalidateCodexIntegrationStatus();
  try {
    await ipc<void>("cmd_codex_start_login");
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    browserMockAuthenticateCodex();
  }
  invalidateCodexIntegrationStatus();
};

export const codexStartDeviceLogin = async () => {
  invalidateCodexIntegrationStatus();
  try {
    await ipc<void>("cmd_codex_start_device_login");
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    browserMockAuthenticateCodex();
  }
  invalidateCodexIntegrationStatus();
};

export const codexInstallSkill = async () => {
  invalidateCodexIntegrationStatus();
  try {
    await ipc<void>("cmd_codex_install_skill");
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    browserMockInstallCodexSkill();
  }
  invalidateCodexIntegrationStatus();
};

/** 用户确认后将本机 R-Code stdio MCP server 注册到 Codex 配置。 */
export const codexInstallMcpServer = async () => {
  invalidateCodexIntegrationStatus();
  try {
    await ipc<void>("cmd_codex_install_mcp_server");
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    browserMockEnableCodexMcp();
  }
  invalidateCodexIntegrationStatus();
};

/** 一次更新协作 Skill 并补齐 R-Code 的 Codex MCP 配置。 */
export const codexSetupCollaboration = async () => {
  invalidateCodexIntegrationStatus();
  try {
    const status = await ipc<CodexIntegrationStatus>("cmd_codex_setup_collaboration");
    invalidateCodexIntegrationStatus();
    codexIntegrationStatusSnapshot = status;
    return status;
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    await new Promise((resolve) => window.setTimeout(resolve, 500));
    const status = browserMockSetupCodexCollaboration();
    invalidateCodexIntegrationStatus();
    codexIntegrationStatusSnapshot = status;
    return status;
  }
};

/** 读取当前 Codex CLI 实际可用模型与运行偏好。 */
let codexCliPreferencesSnapshot: CodexCliPreferences | null = null;
let codexCliPreferencesRequest: Promise<CodexCliPreferences> | null = null;
let codexCliPreferencesGeneration = 0;

function invalidateCodexCliPreferences(): number {
  codexCliPreferencesGeneration += 1;
  codexCliPreferencesSnapshot = null;
  codexCliPreferencesRequest = null;
  return codexCliPreferencesGeneration;
}

const loadCodexCliPreferences = async () => {
  try {
    return await ipc<CodexCliPreferences>("cmd_codex_cli_preferences");
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    await new Promise((resolve) => window.setTimeout(resolve, 450));
    return browserMockCodexCliPreferences();
  }
};

export const codexCliPreferences = () => {
  if (codexCliPreferencesSnapshot) return Promise.resolve(codexCliPreferencesSnapshot);
  if (codexCliPreferencesRequest) return codexCliPreferencesRequest;
  const generation = codexCliPreferencesGeneration;
  const request = loadCodexCliPreferences().then((preferences) => {
    if (generation === codexCliPreferencesGeneration) {
      codexCliPreferencesSnapshot = preferences;
    }
    return preferences;
  });
  codexCliPreferencesRequest = request;
  const clear = () => {
    if (codexCliPreferencesRequest === request) codexCliPreferencesRequest = null;
  };
  void request.then(clear, clear);
  return request;
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
  invalidateCodexCliPreferences();
  try {
    const preferences = await ipc<CodexCliPreferences>("cmd_codex_save_cli_preferences", args);
    invalidateCodexCliPreferences();
    codexCliPreferencesSnapshot = preferences;
    return preferences;
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    await new Promise((resolve) => window.setTimeout(resolve, 500));
    const preferences = browserMockSaveCodexCliPreferences(
      args.model,
      args.reasoningEffort,
      args.verbosity,
      args.permissionMode,
    );
    invalidateCodexCliPreferences();
    codexCliPreferencesSnapshot = preferences;
    return preferences;
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

/** M3-02：提交 Codex requestUserInput 答案；answers 为 null 表示取消。 */
export async function codexSubmitUserInput(
  taskId: string,
  runId: string,
  requestKey: string,
  answers: Record<string, string[]> | null
): Promise<"delivered" | "rejected"> {
  const encoded = answers
    ? Object.fromEntries(
        Object.entries(answers).map(([id, values]) => [id, { answers: values }])
      )
    : null;
  const outcome = await ipc<string>("cmd_codex_submit_user_input", {
    taskId,
    runId,
    requestKey,
    answers: encoded,
  });
  return outcome === "delivered" ? "delivered" : "rejected";
}

// ---- M3-01 关闭状态机 ----
export const CLOSE_PROMPT_REQUEST_EVENT = "close-prompt-request";
export const closeBehaviorGet = () => ipc<string>("cmd_close_behavior_get");
export const closeBehaviorSet = (behavior: string) =>
  ipc<void>("cmd_close_behavior_set", { behavior });
export const closePromptDecision = (epoch: number, decision: string, remember: boolean) =>
  ipc<boolean>("cmd_close_prompt_decision", { epoch, decision, remember });

export const lifecycleExplicitQuit = () =>
  ipc<boolean>("cmd_lifecycle_explicit_quit");
