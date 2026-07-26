/**
 * Tauri IPC 封装 — 前端 → 后端全部命令的 typed wrapper。
 * 后端命令注册于 src-tauri/src/main.rs；参数一律 camelCase（Tauri v2 约定）。
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AgentEvent,
  AgentEventEnvelope,
  AgentSendMode,
  ChangeDiff,
  FileChange,
  LogEntry,
  PermissionDecision,
  PermissionRequest,
  QueuedMessage,
  RecoveryPageData,
  ReplayDepth,
  ReplayEntry,
  SearchMatch,
  SessionMessage,
  SettingsResponse,
  SupportBundlePreview,
  Task,
  TaskDetail,
  TaskMode,
  TerminalInfo,
  VerificationRecord,
  ProjectAccessMode, Workspace,
  ProviderSettingsInput,
  ProviderCatalog,
  CodexIntegrationStatus,
} from "./types";

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

export const taskDetail = (taskId: string) =>
  ipc<TaskDetail>("cmd_task_detail", { taskId });

// ---------- Agent ----------
export const agentSend = (taskId: string, message: string, mode: AgentSendMode = "auto") =>
  ipc<void>("cmd_agent_send", { taskId, message, mode });

export const agentAbort = (taskId: string) => ipc<void>("cmd_agent_abort", { taskId });

export const agentAbortSubagent = (taskId: string, subagentId: string) =>
  ipc<void>("cmd_agent_abort_subagent", { taskId, subagentId });

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

// ---------- 变更 / 审查 ----------
export const changesList = (taskId: string) =>
  ipc<FileChange[]>("cmd_changes_list", { taskId });

export const rollbackFile = (taskId: string, path: string) =>
  ipc<string>("cmd_rollback_file", { taskId, path });

export const rollbackTask = (taskId: string) =>
  ipc<string[]>("cmd_rollback_task", { taskId });

export const acceptTask = (taskId: string) => ipc<void>("cmd_accept_task", { taskId });

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

export const fileList = (workspacePath: string, path: string | null = null) =>
  ipc<FileTreeListing>("cmd_file_list", { workspacePath, path });

export const fileRead = (workspacePath: string, path: string) =>
  ipc<FileContent>("cmd_file_read", { workspacePath, path });

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

// ---------- 搜索 ----------
export const quickOpen = (workspacePath: string, query: string, limit = 20) =>
  ipc<string[]>("cmd_quick_open", { workspacePath, query, limit });

export const globalSearch = (workspacePath: string, query: string, limit = 50) =>
  ipc<SearchMatch[]>("cmd_global_search", { workspacePath, query, limit });

// ---------- 终端 ----------
export const terminalList = () => ipc<TerminalInfo[]>("cmd_terminal_list");

export const terminalCreate = (shell: string, workspacePath: string) =>
  ipc<string>("cmd_terminal_create", { shell, workspacePath });

export const terminalSend = (id: string, text: string, pressEnter = true) =>
  ipc<void>("cmd_terminal_send", { id, text, pressEnter });

export const terminalRead = (id: string) => ipc<string>("cmd_terminal_read", { id });

export const terminalSnapshot = (id: string) => ipc<string>("cmd_terminal_snapshot", { id });

export const terminalKill = (id: string) => ipc<void>("cmd_terminal_kill", { id });

export const terminalResize = (id: string, cols: number, rows: number) =>
  ipc<void>("cmd_terminal_resize", { id, cols, rows });

// ---------- 恢复 / 支持包 ----------
export const recoveryData = () => ipc<RecoveryPageData>("cmd_recovery_data");

export const recoveryCleanup = () => ipc<number>("cmd_recovery_cleanup");

export const supportBundle = (outputDir: string) =>
  ipc<string>("cmd_support_bundle", { outputDir });

export const supportPreview = () => ipc<SupportBundlePreview>("cmd_support_preview");

// ---------- 回放 / 会话 ----------
export const replay = (sessionId: string, depth: ReplayDepth) =>
  ipc<ReplayEntry[]>("cmd_replay", { sessionId, depth });

export const sessionMessages = (taskId: string) =>
  ipc<SessionMessage[]>("cmd_session_messages", { taskId });

// ---------- 项目记忆 ----------
export const memoryGet = (workspacePath: string) => ipc<string>("cmd_memory_get", { workspacePath });

export const memorySet = (workspacePath: string, content: string) =>
  ipc<void>("cmd_memory_set", { workspacePath, content });

// ---------- 设置 ----------
export const settingsGet = () => ipc<SettingsResponse>("cmd_settings_get");

/** 内置模型服务目录。编译期常量，进程内只需拉一次。 */
export const providerCatalog = () => ipc<ProviderCatalog>("cmd_provider_catalog");

export const settingsSet = (key: string, value: unknown) =>
  ipc<void>("cmd_settings_set", { key, value });

export const settingsSaveProvider = (provider: ProviderSettingsInput) =>
  ipc<void>("cmd_settings_save_provider", { provider });

export const settingsSelectProvider = (name: string) =>
  ipc<void>("cmd_settings_select_provider", { name });

export const settingsDeleteProvider = (name: string) =>
  ipc<void>("cmd_settings_delete_provider", { name });

export const codexIntegrationStatus = () =>
  ipc<CodexIntegrationStatus>("cmd_codex_integration_status");

export const codexStartLogin = () => ipc<void>("cmd_codex_start_login");

export const codexInstallSkill = () => ipc<void>("cmd_codex_install_skill");

// ---------- 日志 ----------
export const logsTail = (limit = 200, level?: string) =>
  ipc<LogEntry[]>("cmd_logs_tail", { limit, level: level ?? null });
