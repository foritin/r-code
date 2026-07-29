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
  LogEntry,
  ProjectAccessMode,
  ProviderModelsInput,
  ProviderModelsResponse,
  ProviderSettingsInput,
  SearchMatch,
  SessionBranch,
  SessionMessage,
  Task,
  TaskDetail,
  TaskMode,
  TerminalInfo,
  VerificationRecord,
  Workspace,
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
const memoryByWorkspace = new Map<string, string>([
  ["D:/project/rust/r-code", "# 项目记忆\n\n- Rust + Tauri v2 桌面应用\n- 前端位于 `src-tauri/frontend`\n- 提交前运行前端构建与 Rust 测试\n"],
  ["D:/project/rust/api-server", "# 项目记忆\n\n- API 服务优先保持向后兼容\n- 新增中间件必须包含边界测试\n"],
]);
const verificationOutputs = new Map<string, string>();
const terminalOutputs = new Map<string, string>();
const terminalInputs = new Map<string, string>();
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
  return workspace;
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
  const providerName = optionalStringArg(args, "providerName") ?? browserMockSettings.config.default_provider ?? "codex";
  const provider = browserMockSettings.config.providers?.[providerName];
  const task: Task = {
    id: nextId("task"),
    workspace_path: workspacePath,
    provider_name: providerName,
    model: provider?.model ?? null,
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
  const mode = typeof args.mode === "string" ? args.mode : "auto";
  if (!message) return;
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
    summary: task.workspace_path ? "已完成代码检查并准备变更" : "已完成本轮回答",
    delegated_by_tool_call_id: null,
    model: task.model ?? "gpt-5.6",
    runtime_kind: "native",
    external_session_id: null,
    review_state: task.workspace_path ? "pending" : "answered",
    started_at: timestamp,
    ended_at: timestamp,
    usage_json: JSON.stringify({ input_tokens: 860, output_tokens: 420 }),
  };
  detail.runs.unshift(run);
  messages.push(
    { id: nextId("message"), branch_id: detail.active_branch.id, kind: "message", role: "user", text: message, timestamp },
    {
      id: nextId("message"),
      branch_id: detail.active_branch.id,
      kind: "message",
      role: "assistant",
      text: task.workspace_path
        ? "已完成这轮演示任务：我检查了相关文件、整理了修改，并把结果放到右侧 Changes 与 Review 中供你继续操作。"
        : "这是浏览器 Demo 的完整会话回复。你可以继续追问、使用斜杠命令，或从左侧切换到其他产品场景。",
      timestamp,
    },
  );
  if (task.workspace_path && detail.changes.length === 0) {
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
  task.state = task.workspace_path ? "review_ready" : "idle";
  touchTask(task);
  addEvent(detail, "run_started");
  addEvent(detail, "run_ended");
}

function setTaskField(args: MockArgs, field: "workspace_path" | "provider_name" | "model" | "title"): Task {
  const task = taskById(stringArg(args, "taskId"));
  if (field === "workspace_path") task.workspace_path = optionalStringArg(args, "workspacePath");
  if (field === "provider_name") task.provider_name = stringArg(args, "providerName");
  if (field === "model") task.model = optionalStringArg(args, "model");
  if (field === "title") task.title = stringArg(args, "title").trim() || task.title;
  touchTask(task);
  return task;
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
    agent_label: optionalStringArg(args, "label") ?? "只读调查",
    summary: `已完成只读调查：${stringArg(args, "goal")}`,
    delegated_by_tool_call_id: nextId("delegate"),
    model: "gpt-5.6-sol",
    runtime_kind: runtime,
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
      { kind: "del", text: "// previous demo implementation", old_no: 2 },
      { kind: "add", text: "// complete interactive demo implementation", new_no: 2 },
      { kind: "add", text: "// shares the production React UI", new_no: 3 },
    ],
    truncated: false,
  };
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

/** 执行一条浏览器 Demo IPC，并返回与正式后端同形状的数据。 */
export async function browserMockInvoke(command: string, args: MockArgs = {}): Promise<unknown> {
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
      task.state = "archived";
      touchTask(task);
      return copy(task);
    }
    case "cmd_task_set_workspace": return copy(setTaskField(args, "workspace_path"));
    case "cmd_task_set_provider": return copy(setTaskField(args, "provider_name"));
    case "cmd_task_set_model": return copy(setTaskField(args, "model"));
    case "cmd_task_rename": return copy(setTaskField(args, "title"));
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

    case "cmd_workspace_list": return copy(browserMockWorkspaces);
    case "cmd_workspace_open": {
      const path = stringArg(args, "path");
      const existing = browserMockWorkspaces.find((item) => item.canonical_path === path);
      if (existing) return copy(existing);
      const workspace: Workspace = { canonical_path: path, display_name: path.split(/[\\/]/).filter(Boolean).pop() ?? path, access_mode: "request_approval", last_opened_at: nowIso() };
      browserMockWorkspaces.unshift(workspace);
      return copy(workspace);
    }
    case "cmd_workspace_choose": {
      const workspace = browserMockWorkspaces[0];
      workspace.last_opened_at = nowIso();
      return copy(workspace);
    }
    case "cmd_workspace_set_access_mode": {
      const workspace = workspaceByPath(stringArg(args, "workspacePath"));
      workspace.access_mode = args.accessMode as ProjectAccessMode;
      workspace.last_opened_at = nowIso();
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
    case "cmd_support_bundle": return `${stringArg(args, "outputDir").replace(/[\\/]+$/, "")}/r-code-support-demo.json`;
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

    case "cmd_memory_get": return memoryByWorkspace.get(stringArg(args, "workspacePath")) ?? "";
    case "cmd_memory_set": memoryByWorkspace.set(stringArg(args, "workspacePath"), stringArg(args, "content")); return undefined;
    case "cmd_settings_get": return copy(browserMockSettings);
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
    case "cmd_codex_save_cli_preferences": return copy(browserMockSaveCliPreferences(optionalStringArg(args, "model"), optionalStringArg(args, "reasoningEffort"), optionalStringArg(args, "verbosity")));
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
