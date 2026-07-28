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
  SettingsResponse,
  SupportBundlePreview,
  Task,
  TaskDetail,
  TaskMode,
  TerminalInfo,
  TerminalRawBatch,
  TerminalRawSnapshot,
  VerificationRecord,
  ProjectAccessMode, Workspace,
  ProviderSettingsInput,
  ProviderCatalog,
  CodexCliPreferences,
  CodexIntegrationStatus,
  ContextCompactionResult,
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

async function ipc<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  return invoke<T>(command, args);
}

// ---------- 系统 ----------
export const ping = () => ipc<boolean>("ping");

// ---------- 任务 ----------
export const taskCreate = (
  workspacePath: string | null,
  title: string,
  goal: string,
  mode: TaskMode,
  providerName: string | null = null
) => ipc<Task>("cmd_task_create", { workspacePath, title, goal, mode, providerName });

export const taskList = (workspacePath?: string, includeArchived = false) =>
  ipc<Task[]>("cmd_task_list", { workspacePath, includeArchived });

export const taskArchive = (taskId: string) =>
  ipc<Task>("cmd_task_archive", { taskId });

export const taskSetWorkspace = (taskId: string, workspacePath: string | null) =>
  ipc<Task>("cmd_task_set_workspace", { taskId, workspacePath });

export const taskSetProvider = (taskId: string, providerName: string) =>
  ipc<Task>("cmd_task_set_provider", { taskId, providerName });

/** 切换会话使用的具体模型；传 null 回退到该服务的默认模型。 */
export const taskSetModel = (taskId: string, model: string | null) =>
  ipc<Task>("cmd_task_set_model", { taskId, model });

export const taskRename = (taskId: string, title: string) =>
  ipc<Task>("cmd_task_rename", { taskId, title });

export const taskForkContext = (taskId: string) =>
  ipc<SessionBranch>("cmd_task_fork_context", { taskId });

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
export const agentSend = (taskId: string, message: string, mode: AgentSendMode = "auto") =>
  ipc<void>("cmd_agent_send", { taskId, message, mode });

export const agentAbort = (taskId: string) => ipc<void>("cmd_agent_abort", { taskId });

export const agentAbortSubagent = async (taskId: string, subagentId: string) => {
  try {
    return await ipc<void>("cmd_agent_abort_subagent", { taskId, subagentId });
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    browserMockAbortSubagent(taskId, subagentId);
  }
};

/** 将当前运行中的一项只读调查委派给本机已登录的 Codex CLI。 */
export const agentDelegateCodex = (taskId: string, goal: string, label: string | null = null) =>
  ipc<AgentRun>("cmd_agent_delegate_codex", { taskId, goal, label });

/** 以官方 `codex mcp-server` 创建可续接的只读 Codex 子代理会话。 */
export const agentDelegateCodexMcp = (taskId: string, goal: string, label: string | null = null) =>
  ipc<AgentRun>("cmd_agent_delegate_codex_mcp", { taskId, goal, label });

export const agentQueueList = (taskId: string) =>
  ipc<QueuedMessage[]>("cmd_agent_queue_list", { taskId });

export const agentQueueRemove = (taskId: string, queueId: string) =>
  ipc<void>("cmd_agent_queue_remove", { taskId, queueId });

export const agentResend = (taskId: string, messageId: string, message: string) =>
  ipc<void>("cmd_agent_resend", { taskId, messageId, message });

/** 订阅 agent 流式事件（后端 drain 循环 emit 的 "agent-event"）。 */
export const onAgentEvent = (handler: (taskId: string, event: AgentEvent) => void): Promise<UnlistenFn> =>
  listen<AgentEventEnvelope>("agent-event", (e) => handler(e.payload.task_id, e.payload.event));

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

// ---------- Workspace ----------
export const workspaceList = () => ipc<Workspace[]>("cmd_workspace_list");

export const workspaceOpen = (path: string) =>
  ipc<Workspace>("cmd_workspace_open", { path });

/** 原生系统文件夹选择器；用户取消时返回 null。 */
export const workspaceChoose = () => ipc<Workspace | null>("cmd_workspace_choose");

export const workspaceSetAccessMode = (workspacePath: string, accessMode: ProjectAccessMode) =>
  ipc<Workspace>("cmd_workspace_set_access_mode", { workspacePath, accessMode });

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

/** 读取子代理的隔离日志；其中只包含公开生命周期、工具审计和最终可见结果。 */
export const subagentSessionMessages = async (taskId: string, subagentId: string) => {
  try {
    return await ipc<SessionMessage[]>("cmd_subagent_session_messages", { taskId, subagentId });
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    return browserMockSubagentMessages(taskId, subagentId);
  }
};

// ---------- 项目记忆 ----------
export const memoryGet = (workspacePath: string) => ipc<string>("cmd_memory_get", { workspacePath });

export const memorySet = (workspacePath: string, content: string) =>
  ipc<void>("cmd_memory_set", { workspacePath, content });

// ---------- 设置 ----------
export const settingsGet = async () => {
  try {
    return await ipc<SettingsResponse>("cmd_settings_get");
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    return browserMockSettings;
  }
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

export const settingsSet = (key: string, value: unknown) =>
  ipc<void>("cmd_settings_set", { key, value });

export const settingsSaveProvider = (provider: ProviderSettingsInput) =>
  ipc<void>("cmd_settings_save_provider", { provider });

export const settingsSelectProvider = (name: string) =>
  ipc<void>("cmd_settings_select_provider", { name });

export const settingsDeleteProvider = (name: string) =>
  ipc<void>("cmd_settings_delete_provider", { name });

export const codexIntegrationStatus = async () => {
  try {
    return await ipc<CodexIntegrationStatus>("cmd_codex_integration_status");
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    return browserMockCodexIntegrationStatus();
  }
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

/** 一次更新协作 Skill 并补齐 R-Code 只读 MCP 配置。 */
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

/** 空字符串会被转换为 null，从 config.toml 移除覆盖并恢复 Codex 默认。 */
export const codexSaveCliPreferences = async (model: string, reasoningEffort: string, verbosity: string) => {
  const args = {
    model: model || null,
    reasoningEffort: reasoningEffort || null,
    verbosity: verbosity || null,
  };
  try {
    return await ipc<CodexCliPreferences>("cmd_codex_save_cli_preferences", args);
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    await new Promise((resolve) => window.setTimeout(resolve, 500));
    return browserMockSaveCodexCliPreferences(args.model, args.reasoningEffort, args.verbosity);
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
