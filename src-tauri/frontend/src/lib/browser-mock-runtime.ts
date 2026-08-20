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
  RtkStatus,
  SearchMatch,
  SessionBranch,
  SessionMessage,
  SubagentPoolConfig,
  SubagentPoolSnapshot,
  SubagentProviderCatalogEntry,
  SubagentProviderCatalogSnapshot,
  SubagentProviderHealthView,
  SubagentProviderProbeBatchResponse,
  SubagentProviderProbeRequest,
  SubagentProviderProbeResponse,
  SubagentProviderSource,
  SubagentSessionMessagePage,
  SubagentSessionMessagePageRequest,
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
  ProjectAgentPromptConfig,
  PlanEntryOfferView,
  PlanningStatusView,
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
  browserMockRecovery,
  browserMockSaveCodexCliPreferences as browserMockSaveCliPreferences,
  browserMockSetMessages,
  browserMockSettings,
  browserMockSetupCodexCollaboration as browserMockSetupCollaboration,
  browserMockRtkStatus,
  browserMockSetRtkEnabled,
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
    id: "generated-demo",
    display_name: "项目搜索 MCP",
    description: "由 mcp-creator 生成，等待用户审核启动方案。",
    enabled: false,
    builtin: false,
    source: {
      kind: "generated",
      source_path: "D:/project/rust/r-code/examples/generated-demo",
      created_at: new Date().toISOString(),
    },
    transport: {
      type: "stdio",
      executable: "D:/project/rust/r-code/examples/generated-demo/generated-demo.exe",
      args: [],
      environment_names: [],
    },
    state: "disabled",
    tool_count: 0,
    launch_approved: false,
  },
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
    id: "builtin:mcp-creator",
    name: "mcp-creator",
    description: "创建 MCP 服务源码并保存为待用户审核的禁用草稿。",
    instructions: "帮用户创建全局 MCP 草稿：只声明凭据变量名，不填值；不得启动或启用服务；验证后调用 mcp_create_draft。",
    source: "builtin",
    enabled: true,
    overridden: false,
    scope: "global",
    inherited: false,
  },
  {
    id: "builtin:skill-creator",
    name: "skill-creator",
    description: "创建并注册 R-Code 自定义 Skill。",
    instructions: "设计 Skill 后必须调用 save_skill 工具保存。",
    source: "builtin",
    enabled: true,
    overridden: false,
    scope: "global",
    inherited: false,
  },
  {
    id: "builtin:review-changes",
    name: "review-changes",
    description: "安全审核并接受任务变更。",
    instructions: "只审核当前任务路径。",
    source: "builtin",
    enabled: true,
    overridden: false,
    scope: "global",
    inherited: false,
  },
  {
    id: "builtin:git-commit-push",
    name: "git-commit-push",
    description: "提交并推送已接受的任务变更。",
    instructions: "不得 force push。",
    source: "builtin",
    enabled: true,
    overridden: false,
    scope: "global",
    inherited: false,
  },
];
let workflowSkills = defaultWorkflowSkills.map((skill) => ({ ...skill }));
const projectWorkflowSkills = new Map<string, WorkflowSkill[]>();
const projectPromptConfigs = new Map<string, ProjectAgentPromptConfig>();
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
const terminalOwners = new Map<string, string>([
  ["demo-terminal-main", "mock-task-queue"],
]);

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

const MOCK_NATIVE_SUBAGENT_CAPABILITIES = {
  supports_host_delegation: true,
  supports_live_messages: true,
  supports_full_access: true,
} as const;
const MOCK_CODEX_SUBAGENT_CAPABILITIES = {
  supports_host_delegation: false,
  supports_live_messages: false,
  supports_full_access: false,
} as const;

let mockSubagentPoolRevision = 1;
let mockSubagentPool: SubagentPoolConfig = { slots: [] };
const mockSubagentHealth = new Map<string, SubagentProviderHealthView>();

function mockSubagentSourceKey(source: SubagentProviderSource): string {
  return source.kind === "api_provider" ? `api:${source.provider_id}` : "codex_cli";
}

function mockSubagentCandidateKey(source: SubagentProviderSource, model: string): string {
  return `${mockSubagentSourceKey(source)}\u0000${model}`;
}

function mockSubagentRevisionToken(): string {
  return `mock-subagent-revision-${mockSubagentPoolRevision}`;
}

function sameMockSubagentSource(left: SubagentProviderSource, right: SubagentProviderSource): boolean {
  return mockSubagentSourceKey(left) === mockSubagentSourceKey(right);
}

function mockInitialSubagentHealth(
  source: SubagentProviderSource,
  model: string,
): SubagentProviderHealthView {
  if (source.kind === "api_provider" && source.provider_id === "openai"
    && model === browserMockSettings.config.providers?.openai?.model) {
    return {
      state: "connected",
      verification_level: "inference",
      checked_at: nowIso(),
      expires_at: new Date(Date.now() + 30 * 60_000).toISOString(),
      latency_ms: 168,
    };
  }
  if (source.kind === "api_provider" && source.provider_id === "deepseek"
    && model === browserMockSettings.config.providers?.deepseek?.model) {
    return {
      state: "failed",
      verification_level: "inference",
      checked_at: nowIso(),
      expires_at: new Date(Date.now() + 5 * 60_000).toISOString(),
      latency_ms: 920,
      error: "network_unavailable",
    };
  }
  return { state: "untested" };
}

function mockSubagentHealthFor(
  source: SubagentProviderSource,
  model: string,
): SubagentProviderHealthView {
  const key = mockSubagentCandidateKey(source, model);
  const existing = mockSubagentHealth.get(key);
  if (existing) {
    if (existing.state === "connected" && existing.expires_at && Date.parse(existing.expires_at) <= Date.now()) {
      const stale = { ...existing, state: "stale" as const };
      mockSubagentHealth.set(key, stale);
      mockSubagentPoolRevision += 1;
      return stale;
    }
    return existing;
  }
  const initial = mockInitialSubagentHealth(source, model);
  mockSubagentHealth.set(key, initial);
  return initial;
}

function markMockSubagentSourceStale(source: SubagentProviderSource): void {
  const prefix = `${mockSubagentSourceKey(source)}\u0000`;
  for (const [key, health] of mockSubagentHealth) {
    if (key.startsWith(prefix) && health.state === "connected") {
      mockSubagentHealth.set(key, { ...health, state: "stale" });
    }
  }
  mockSubagentPoolRevision += 1;
}

function validMockSubagentIdentifier(value: string, maxChars: number): boolean {
  return value.length > 0
    && value.trim() === value
    && [...value].length <= maxChars
    && !/[\u0000-\u001f\u007f-\u009f]/.test(value);
}

function mockSubagentCatalog(): SubagentProviderCatalogSnapshot {
  const entries: SubagentProviderCatalogEntry[] = [];
  const providers = Object.entries(browserMockSettings.config.providers ?? {}).sort(([left], [right]) => left.localeCompare(right));
  for (const [providerId, provider] of providers) {
    const status = browserMockSettings.provider_status[providerId];
    if (!status?.configured) continue;
    const source: SubagentProviderSource = { kind: "api_provider", provider_id: providerId };
    const model = provider.model?.trim() ?? "";
    const ready = status.ready && model.length > 0;
    const health = mockSubagentHealthFor(source, model);
    entries.push({
      source,
      display_name: providerId === "openai" ? "OpenAI API" : providerId === "deepseek" ? "DeepSeek API" : providerId,
      model,
      configured: true,
      ready,
      connected: health.state === "connected",
      selectable: ready && health.state === "connected",
      supported: true,
      availability: ready ? "ready" : "needs_configuration",
      protocol: provider.protocol ?? null,
      capabilities: { ...MOCK_NATIVE_SUBAGENT_CAPABILITIES },
      health: { ...health },
    });
  }

  const codexStatus = browserMockCodexIntegrationStatus();
  const codexSource: SubagentProviderSource = { kind: "codex_cli" };
  const codexModel = browserMockCliPreferences().model ?? "gpt-5.6-sol";
  const codexReady = codexStatus.integration_ready === true;
  const codexHealth = mockSubagentHealthFor(codexSource, codexModel);
  const codexAvailability = !codexStatus.cli_available
    ? "not_installed"
    : codexStatus.auth_status !== "authenticated"
      ? "login_required"
      : codexReady
        ? "ready"
        : "trust_required";
  entries.push({
    source: codexSource,
    display_name: "Codex CLI",
    model: codexModel,
    configured: codexStatus.config_exists,
    ready: codexReady,
    connected: codexHealth.state === "connected",
    selectable: codexReady && codexHealth.state === "connected",
    supported: true,
    availability: codexAvailability,
    protocol: "codex_cli",
    capabilities: { ...MOCK_CODEX_SUBAGENT_CAPABILITIES },
    health: { ...codexHealth },
  });

  return { generated_at: nowIso(), entries };
}

function mockSubagentEntryFor(
  source: SubagentProviderSource,
  model: string,
  catalog = mockSubagentCatalog(),
): SubagentProviderCatalogEntry | null {
  const base = catalog.entries.find((entry) => sameMockSubagentSource(entry.source, source));
  if (!base) return null;
  const health = mockSubagentHealthFor(source, model);
  return {
    ...base,
    source: copy(source),
    model,
    connected: health.state === "connected",
    selectable: base.ready && base.supported && health.state === "connected",
    health: { ...health },
  };
}

function mockSubagentPoolSnapshot(): SubagentPoolSnapshot {
  const catalog = mockSubagentCatalog();
  return {
    revision: mockSubagentRevisionToken(),
    pool: copy(mockSubagentPool),
    catalog,
    slot_health: mockSubagentPool.slots.map((slot) => {
      const entry = mockSubagentEntryFor(slot.source, slot.model, catalog);
      return {
        slot_id: slot.slot_id,
        source: copy(slot.source),
        model: slot.model,
        selectable: entry?.selectable ?? false,
        availability: entry?.availability ?? "unsupported",
        capabilities: entry?.capabilities ?? { ...MOCK_CODEX_SUBAGENT_CAPABILITIES },
        health: entry?.health ?? { state: "untested" },
      };
    }),
  };
}

function mockProbeSubagentProvider(request: SubagentProviderProbeRequest): SubagentProviderCatalogEntry {
  const model = request.model.trim();
  if (!model) throw new Error("子代理模型不能为空");
  const catalog = mockSubagentCatalog();
  const base = catalog.entries.find((entry) => sameMockSubagentSource(entry.source, request.source));
  if (!base) throw new Error("子代理来源不存在或已被删除");

  const connected = base.ready && !(request.source.kind === "api_provider" && request.source.provider_id === "deepseek");
  const health: SubagentProviderHealthView = connected
    ? {
        state: "connected",
        verification_level: request.source.kind === "codex_cli" ? "remote_catalog" : "inference",
        checked_at: nowIso(),
        expires_at: new Date(Date.now() + 30 * 60_000).toISOString(),
        latency_ms: request.source.kind === "codex_cli" ? 242 : 184,
      }
    : {
        state: "failed",
        verification_level: request.source.kind === "codex_cli" ? "remote_catalog" : "inference",
        checked_at: nowIso(),
        expires_at: new Date(Date.now() + 5 * 60_000).toISOString(),
        latency_ms: 900,
        error: base.ready ? "network_unavailable" : "authentication_failed",
      };
  mockSubagentHealth.set(mockSubagentCandidateKey(request.source, model), health);
  return {
    ...base,
    source: copy(request.source),
    model,
    connected,
    selectable: connected,
    health,
  };
}

function mockSubagentProviderTest(request: SubagentProviderProbeRequest): SubagentProviderProbeResponse {
  const result = mockProbeSubagentProvider(request);
  mockSubagentPoolRevision += 1;
  return { result, snapshot: mockSubagentPoolSnapshot() };
}

function mockSubagentProviderTestBatch(requests: SubagentProviderProbeRequest[]): SubagentProviderProbeBatchResponse {
  const results = requests.map(mockProbeSubagentProvider);
  mockSubagentPoolRevision += 1;
  return { results, snapshot: mockSubagentPoolSnapshot() };
}

function mockSaveSubagentPool(revision: string, pool: SubagentPoolConfig): SubagentPoolSnapshot {
  if (revision !== mockSubagentRevisionToken()) {
    throw new Error("子代理配置已在其他窗口更新，请重新加载后再保存");
  }
  if (!pool || !Array.isArray(pool.slots)) throw new Error("子代理候选池格式无效");
  if (pool.slots.length > 3) throw new Error("子代理候选池最多支持 3 个槽位");

  const catalog = mockSubagentCatalog();
  const ids = new Set<string>();
  let weightTotal = 0;
  for (const slot of pool.slots) {
    if (!validMockSubagentIdentifier(slot.slot_id, 80) || ids.has(slot.slot_id)) {
      throw new Error("子代理槽位 ID 必须非空、无控制字符、最多 80 字符且唯一");
    }
    ids.add(slot.slot_id);
    if (!Number.isInteger(slot.weight) || slot.weight < 1 || slot.weight > 100) {
      throw new Error("每个子代理权重必须是 1 到 100 的整数");
    }
    weightTotal += slot.weight;
    if (!validMockSubagentIdentifier(slot.model, 320)) throw new Error("子代理模型必须填写、无控制字符且最多 320 字符");
    if (slot.source.kind === "api_provider" && !validMockSubagentIdentifier(slot.source.provider_id, 160)) {
      throw new Error("API Provider 标识无效或超过 160 字符");
    }
    if (slot.prompt_template_id != null && !validMockSubagentIdentifier(slot.prompt_template_id, 80)) {
      throw new Error("Prompt 模板标识无效或超过 80 字符");
    }
    if (!slot.prompt.trim() || slot.prompt.includes("\u0000") || [...slot.prompt].length > 12_000) {
      throw new Error("子代理 Prompt 必须非空且不超过 12000 字符");
    }
    const entry = mockSubagentEntryFor(slot.source, slot.model, catalog);
    if (!entry?.selectable || entry.health.state !== "connected") {
      throw new Error("只有当前配置指纹下连通测试通过的来源与模型才能保存");
    }
  }
  if (pool.slots.length > 0 && weightTotal !== 100) {
    throw new Error(`子代理权重合计必须为 100，当前为 ${weightTotal}`);
  }

  mockSubagentPool = copy(pool);
  browserMockSettings.config.orchestration ??= {
    default_agent_engine: "r_code",
    delegation_router: "balanced",
    allow_cross_engine_delegation: true,
    quality_loop: "off",
    quality_reviewer: "r_code",
    max_review_rounds: 1,
  };
  browserMockSettings.config.orchestration.subagent_pool = copy(pool);
  mockSubagentPoolRevision += 1;
  return mockSubagentPoolSnapshot();
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

function memoryReviewScopeArg(args: MockArgs, key: "workspaceId" | "workspacePath"): string | null {
  if (!Object.prototype.hasOwnProperty.call(args, key) || args[key] === null) return null;
  const value = args[key];
  if (typeof value !== "string" || value.trim().length === 0) {
    throw new Error("复盘范围无效");
  }
  return value;
}

function hasReviewableMemoryExchange(taskId: string): boolean {
  let hasUserMessage = false;
  for (const message of browserMockMessages(taskId)) {
    if (message.kind !== "message" || !message.text?.trim()) continue;
    if (message.role === "user") {
      hasUserMessage = true;
      continue;
    }
    if (message.role === "assistant" && hasUserMessage && !message.text.trimStart().startsWith("[error]")) {
      return true;
    }
  }
  return false;
}

function selectMemoryReviewTask(args: MockArgs): { task: Task; workspaceId: string | null } | null {
  const workspaceId = memoryReviewScopeArg(args, "workspaceId");
  const workspacePath = memoryReviewScopeArg(args, "workspacePath");
  if ((workspaceId == null) !== (workspacePath == null)) {
    throw new Error("复盘范围无效");
  }
  if (workspaceId && workspacePath) {
    const workspace = browserMockWorkspaces.find(
      (item) => item.id === workspaceId && item.canonical_path === workspacePath,
    );
    if (!workspace) throw new Error("复盘范围无效");
    if (workspace.memory_mode !== "inherit") throw new Error("当前项目记忆不可写");
  }

  const task = [...browserMockTasks]
    .filter((item) => item.workspace_path === workspacePath)
    .filter((item) => item.state === "idle" || item.state === "review_ready")
    .filter((item) => hasReviewableMemoryExchange(item.id))
    .filter((item) => !mockMemoryOverview.recent_jobs.some((job) =>
      job.task_id === item.id && ["queued", "running", "failed", "interrupted"].includes(job.status)
    ))
    .sort((left, right) => right.updated_at.localeCompare(left.updated_at))[0] ?? null;
  return task ? { task, workspaceId } : null;
}

function performanceProbeArgs(command: string, args: MockArgs): MockArgs {
  if (command !== "cmd_memory_review_now") return args;
  const selection = selectMemoryReviewTask(args);
  return selection ? { ...args, taskId: selection.task.id } : args;
}

function taskById(taskId: string): Task {
  const task = browserMockTasks.find((item) => item.id === taskId);
  if (!task) throw new Error(`Demo 中不存在任务 ${taskId}`);
  return task;
}

/** 浏览器回归钩子：当前模拟的待决 Plan 入口建议（默认无）。 */
let browserMockPlanEntryOffer: PlanEntryOfferView | null = null;

export function setBrowserMockPlanEntryOffer(offer: PlanEntryOfferView | null): void {
  browserMockPlanEntryOffer = offer;
}

/** 浏览器回归钩子：模拟的规划发布状态（默认 off，证据未通过）。 */
let browserMockPlanningStatus: PlanningStatusView = {
  release_state: "off",
  emergency_off: false,
  evidence_version: "",
  eligibility_profile_version: "deepseek-plan-v1",
  customer_card_visible: true,
  evidence_validated: false,
  basis: "browser mock: no validated evidence manifest embedded",
};

export function setBrowserMockPlanningStatus(status: PlanningStatusView): void {
  browserMockPlanningStatus = status;
}

function detailById(taskId: string): TaskDetail {
  const detail = browserMockDetails[taskId];
  if (!detail) throw new Error(`Demo 中不存在任务详情 ${taskId}`);
  return detail;
}

/** Browser-regression hook: settle a mock main run the same way the desktop host would. */
export function browserMockFinishTask(
  taskId: string,
  state: Extract<Task["state"], "interrupted" | "review_ready">,
): void {
  const task = taskById(taskId);
  const detail = detailById(taskId);
  const timestamp = nowIso();
  task.state = state;
  touchTask(task);
  detail.task = copy(task);
  for (const run of detail.runs) {
    if (run.agent_kind !== "main" || run.ended_at != null) continue;
    run.ended_at = timestamp;
    run.review_state = state === "interrupted" ? "aborted" : "pending";
  }
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

const MAX_PROJECT_CONVERSATIONS = 5;
const PROJECT_CONVERSATION_LIMIT_ERROR = {
  code: "PROJECT_CONVERSATION_LIMIT_REACHED",
  message: "该项目最多保留 5 个未归档对话，请先归档一个后再新建",
  limit: MAX_PROJECT_CONVERSATIONS,
} as const;

function assertProjectConversationCapacity(workspacePath: string | null): void {
  if (!workspacePath) return;
  const activeCount = browserMockTasks.filter(
    (task) => task.workspace_path === workspacePath && task.state !== "archived",
  ).length;
  if (activeCount >= MAX_PROJECT_CONVERSATIONS) {
    throw { ...PROJECT_CONVERSATION_LIMIT_ERROR };
  }
}

function createTask(args: MockArgs): Task {
  const createdAt = nowIso();
  const workspacePath = optionalStringArg(args, "workspacePath");
  assertProjectConversationCapacity(workspacePath);
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
    goal_active: false,
    mode: (args.mode as TaskMode | undefined) ?? (workspacePath ? "edit" : "ask"),
    state: "idle",
    worktree_path: null,
    created_at: createdAt,
    updated_at: createdAt,
  };
  const branch: SessionBranch = {
    id: "main",
    task_id: task.id,
    parent_branch_id: null,
    forked_from_message_id: null,
    storage_id: task.id,
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

function projectConversationSequence(title: string): number | null {
  if (!title.startsWith("新对话")) return null;
  const suffix = title.slice("新对话".length).trim();
  if (!suffix) return 1;
  if (!/^\d+$/.test(suffix)) return null;
  const sequence = Number(suffix);
  return Number.isSafeInteger(sequence) && sequence >= 2 ? sequence : null;
}

function createProjectConversation(args: MockArgs): Task {
  const workspacePath = stringArg(args, "workspacePath");
  assertProjectConversationCapacity(workspacePath);

  const highestSequence = browserMockTasks
    .filter((task) => task.workspace_path === workspacePath)
    .reduce((highest, task) => Math.max(highest, projectConversationSequence(task.title) ?? 0), 0);
  const nextSequence = highestSequence + 1;
  return createTask({
    ...args,
    workspacePath,
    title: nextSequence === 1 ? "新对话" : `新对话 ${nextSequence}`,
    goal: "",
    mode: "edit",
  });
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

function renameTask(args: MockArgs): Task {
  const task = taskById(stringArg(args, "taskId"));
  const title = stringArg(args, "title").trim();
  if (!title) throw new Error("请输入新的会话名称");
  if (Array.from(title).length > 96) throw new Error("会话名称不能超过 96 个字符");
  if (task.state === "archived") throw new Error("会话已归档，不能再重命名");
  task.title = title;
  touchTask(task);
  return task;
}

function setTaskWorkspace(args: MockArgs): Task {
  const task = taskById(stringArg(args, "taskId"));
  const requested = optionalStringArg(args, "workspacePath");
  const detail = browserMockDetails[task.id];
  if (task.state === "archived") throw new Error("会话已归档，不能再修改工作区");
  if (
    task.state === "exploring" ||
    task.state === "in_progress" ||
    detail?.runs.some((run) => run.ended_at == null)
  ) {
    throw new Error("当前运行尚未结束，不能在执行期间附加工作区");
  }
  if (task.workspace_path && requested !== task.workspace_path) {
    throw new Error("此会话已绑定项目；如需使用其他项目，请新建对话");
  }
  if (!task.workspace_path && requested) task.workspace_path = requested;
  touchTask(task);
  if (detail) detail.task = copy(task);
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
  task.goal_active = task.goal.length > 0;
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
    kind: "plan",
    restore_mode: null,
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
      section_path: ["阶段 1 · 边界"],
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
      section_path: ["阶段 2 · 交付"],
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
  taskById(taskId).mode = "auto";
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
  const task = taskById(taskId);
  const detail = detailById(taskId);
  if (detail.runs.some((run) => run.agent_kind === "main" && run.ended_at == null)) {
    throw new Error("当前运行尚未结束，无需重复启动 Plan 实施");
  }
  if (view.plan.implementation_dispatch_state === "failed") {
    view.plan.implementation_dispatch_state = "dispatched";
    view.plan.implementation_dispatch_error = null;
    view.plan.implementation_dispatched_at = nowIso();
  } else if (
    view.plan.implementation_dispatch_state !== "dispatched" ||
    !["interrupted", "review_ready"].includes(task.state)
  ) {
    throw new Error("只有已中断或已有部分成果待审查的 Plan 才能续接当前功能");
  }
  const timestamp = nowIso();
  task.state = "in_progress";
  touchTask(task);
  detail.task = copy(task);
  detail.runs.unshift({
    id: nextId("plan-continuation-run"),
    task_id: taskId,
    branch_id: detail.active_branch.id,
    parent_run_id: null,
    agent_kind: "main",
    agent_label: "主代理",
    summary: "继续实施当前 Plan 功能事项",
    delegated_by_tool_call_id: null,
    model: task.model ?? "gpt-5.6",
    runtime_kind: "native",
    external_session_id: null,
    review_state: "pending",
    started_at: timestamp,
    ended_at: null,
    usage_json: null,
    access_mode: "full_access",
    routing_reason: "沿用当前 Plan 与功能进度",
  });
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

function clearTaskContext(taskId: string): SessionBranch {
  const task = taskById(taskId);
  // The browser preview store may still hold the previous detail object. Mutate a copy so the
  // subsequent cmd_task_detail response has a new identity and React observes the branch change.
  const detail = copy(detailById(taskId));
  if (task.state === "exploring" || task.state === "in_progress") {
    throw new Error("当前运行尚未结束，请先停止或等待完成后再清空上下文");
  }
  historicalBranchMessages.set(
    `${taskId}:${detail.active_branch.id}`,
    copy(browserMockMessages(taskId)),
  );
  detail.active_branch.is_active = false;
  const branch: SessionBranch = {
    id: nextId("branch"),
    task_id: taskId,
    parent_branch_id: detail.active_branch.id,
    forked_from_message_id: null,
    storage_id: nextId("storage"),
    is_active: true,
    created_at: nowIso(),
  };
  detail.branches = [
    ...detail.branches.filter((item) => item.id !== detail.active_branch.id),
    detail.active_branch,
    branch,
  ];
  detail.active_branch = branch;
  detail.runs = [];
  detail.events = [];
  detail.permissions = [];
  detail.queued_messages = [];
  if (!task.goal_active) task.goal = "";
  touchTask(task);
  detail.task = copy(task);
  addEvent(detail, "session_cleared");
  browserMockSetMessages(taskId, []);
  browserMockDetails[taskId] = detail;
  return branch;
}

const historicalBranchMessages = new Map<string, SessionMessage[]>();

function messagesForBranch(taskId: string, branchId: string): SessionMessage[] {
  const detail = detailById(taskId);
  const branch = detail.branches.find((item) => item.id === branchId);
  if (!branch || branch.task_id !== taskId) {
    throw new Error("会话分支不属于当前任务");
  }
  if (detail.active_branch.id === branchId) return browserMockMessages(taskId);
  return historicalBranchMessages.get(`${taskId}:${branchId}`) ?? [];
}

function abortTask(taskId: string): void {
  const task = taskById(taskId);
  const detail = detailById(taskId);
  if (task.state === "archived") throw new Error("会话已归档，不能中止运行");
  const hasActiveMainRun = detail.runs.some(
    (run) => run.agent_kind === "main" && run.ended_at == null,
  );
  if (
    task.state !== "exploring"
    && task.state !== "in_progress"
    && !hasActiveMainRun
  ) {
    // Mirror the native command: a stale Stop that loses to run finalization is a no-op and must
    // not overwrite review-ready or idle state in browser-backed product tests.
    return;
  }
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

function requireOwnedTerminal(taskId: string, id: string): TerminalInfo {
  const terminal = terminals.find((item) => item.id === id);
  if (!terminal || terminalOwners.get(id) !== taskId) {
    throw new Error(`terminal not found for this conversation: ${id}`);
  }
  return terminal;
}

function subagentPageRequestArg(args: MockArgs): SubagentSessionMessagePageRequest {
  const value = args.request;
  if (value == null) return {};
  if (typeof value !== "object" || Array.isArray(value)) throw new Error("Demo 子代理分页参数无效");
  const request = value as Record<string, unknown>;
  return {
    ...(typeof request.after_cursor === "string" ? { after_cursor: request.after_cursor } : {}),
    ...(typeof request.before_cursor === "string" ? { before_cursor: request.before_cursor } : {}),
    ...(typeof request.limit === "number" ? { limit: request.limit } : {}),
  };
}

function mockSubagentMessagePage(
  taskId: string,
  subagentId: string,
  request: SubagentSessionMessagePageRequest,
): SubagentSessionMessagePage {
  if (request.after_cursor && request.before_cursor) throw new Error("Demo 子代理游标不能同时向前和向后读取");
  const messages = browserMockSubagentMessages(taskId, subagentId);
  const limit = Math.max(1, Math.min(250, Math.floor(request.limit ?? 80)));
  const parseCursor = (value: string | undefined): { start: number; end: number } | null => {
    if (!value) return null;
    const match = /^mock:window:(\d+):(\d+)$/.exec(value);
    if (!match) return null;
    const start = Number.parseInt(match[1], 10);
    const end = Number.parseInt(match[2], 10);
    return start <= end && end <= messages.length ? { start, end } : null;
  };
  const requestedCursor = request.after_cursor ?? request.before_cursor;
  const cursor = parseCursor(requestedCursor);
  const reset = Boolean(requestedCursor && cursor == null);

  let pageStart: number;
  let pageEnd: number;
  let windowStart: number;
  let windowEnd: number;
  if (!requestedCursor || reset) {
    pageEnd = messages.length;
    pageStart = Math.max(0, pageEnd - limit);
    windowStart = pageStart;
    windowEnd = pageEnd;
  } else if (request.after_cursor) {
    pageStart = cursor!.end;
    pageEnd = Math.min(messages.length, pageStart + limit);
    windowStart = cursor!.start;
    windowEnd = pageEnd;
  } else {
    pageEnd = cursor!.start;
    pageStart = Math.max(0, pageEnd - limit);
    windowStart = pageStart;
    windowEnd = cursor!.end;
  }

  const pageMessages = messages.slice(pageStart, pageEnd);
  const windowCursor = `mock:window:${windowStart}:${windowEnd}`;
  return {
    messages: copy(pageMessages),
    call_id_updates: [],
    // Like the host cursor, both directions carry the complete locally loaded [start, end]
    // window. An idle append poll therefore cannot forget history loaded via before_cursor.
    next_cursor: windowCursor,
    previous_cursor: windowCursor,
    has_more_before: windowStart > 0,
    reset,
    unchanged: Boolean(request.after_cursor && !reset && pageMessages.length === 0),
  };
}

function createTerminal(taskId: string, shell: string): string {
  taskById(taskId);
  const id = nextId("terminal");
  terminals.unshift({ id, state: shell === "Codex CLI" ? "agent" : "idle", shell, is_busy: false });
  terminalOwners.set(id, taskId);
  terminalOutputs.set(id, `${shell}\r\nR-Code browser demo session\r\n\r\nPS D:\\project\\rust\\r-code> `);
  terminalInputs.set(id, "");
  return id;
}

function sendTerminalInput(taskId: string, id: string, text: string): void {
  const terminal = requireOwnedTerminal(taskId, id);
  if (terminal.state === "exited") throw new Error("终端已经结束");
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

function browserKnowledgePromptSnapshot(workspacePath: string | null) {
  const global = browserMockSettings.config.agent_prompts ?? { main_agent: "", subagent: "" };
  const project = workspacePath ? projectPromptConfigs.get(workspacePath) ?? null : null;
  const append = (base: string, addition: string) => {
    const scoped = addition.trim();
    if (!scoped) return base;
    return base.trim()
      ? `${base.trimEnd()}\n\nProject-specific collaboration guidance:\n${scoped}`
      : scoped;
  };
  const effective = !project
    ? { ...global }
    : project.mode === "override"
      ? { main_agent: project.main_agent, subagent: project.subagent }
      : {
          main_agent: append(global.main_agent, project.main_agent),
          subagent: append(global.subagent, project.subagent),
        };
  return {
    global: { ...global },
    project: project ? { ...project } : workspacePath ? { mode: "append", main_agent: "", subagent: "" } : null,
    project_configured: Boolean(project),
    effective,
  };
}

function saveProvider(provider: ProviderSettingsInput): void {
  browserMockSettings.config.providers ??= {};
  const existing = browserMockSettings.config.providers[provider.name];
  browserMockSettings.config.providers[provider.name] = {
    base_url: provider.baseUrl,
    model: provider.model,
    provider_kind: provider.providerKind == null
      ? existing?.provider_kind
      : provider.providerKind.trim() || undefined,
    max_tokens: provider.maxTokens ?? undefined,
    temperature: provider.temperature ?? undefined,
    protocol: provider.protocol ?? undefined,
    show_reasoning: provider.showReasoning ?? existing?.show_reasoning ?? true,
  };
  browserMockSettings.provider_status[provider.name] = {
    configured: true,
    ready: true,
    source: provider.apiKey ? "encrypted_file" : "environment",
    effective_protocol: provider.protocol ?? "openai_responses",
  };
  markMockSubagentSourceStale({ kind: "api_provider", provider_id: provider.name });
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
  const previous = mockMcpServers.find((server) => server.id === request.id);
  const launchUnchanged = Boolean(previous
    && JSON.stringify(previous.transport) === JSON.stringify(request.transport));
  const enabled = launchUnchanged ? Boolean(previous?.enabled) : false;
  return {
    id: request.id,
    display_name: request.display_name,
    description: request.description,
    enabled,
    builtin: false,
    source: previous?.source ?? { kind: "user" },
    transport: request.transport,
    state: enabled ? "stopped" : "disabled",
    tool_count: launchUnchanged ? previous?.tool_count ?? 0 : 0,
    launch_approved: launchUnchanged ? Boolean(previous?.launch_approved) : false,
  };
}

/** 执行一条浏览器 Demo IPC，并返回与正式后端同形状的数据。 */
export async function browserMockInvoke(command: string, args: MockArgs = {}): Promise<unknown> {
  (globalThis as { __rCodePerformanceIpcProbe?: (name: string, args: MockArgs) => void })
    .__rCodePerformanceIpcProbe?.(command, performanceProbeArgs(command, args));
  const delayMs = (globalThis as { __rCodeBrowserMockDelayMs?: Record<string, number> })
    .__rCodeBrowserMockDelayMs?.[command] ?? 0;
  if (delayMs > 0) {
    await new Promise((resolve) => globalThis.setTimeout(resolve, delayMs));
  }
  const forcedFailure = (globalThis as { __rCodeBrowserMockFailures?: Record<string, unknown> })
    .__rCodeBrowserMockFailures?.[command];
  if (typeof forcedFailure === "string") throw new Error(forcedFailure);
  if (forcedFailure) throw forcedFailure;
  switch (command) {
    case "ping": return true;
    case "cmd_app_quit": return null;
    case "cmd_platform_capabilities": {
      const hint = navigator.platform || "";
      const explicitMac = /mac|darwin/i.test(hint);
      const explicitWindows = /win/i.test(hint);
      const explicitOther = /linux|x11|cros|android|iphone|ipad|ipod/i.test(hint);
      const macOS = explicitMac || (!explicitWindows && !explicitOther
        && /macintosh|mac os x/i.test(navigator.userAgent || ""));
      const nativeOcr = macOS || explicitWindows;
      return {
        platform: macOS ? "macos" : explicitWindows ? "windows" : /linux|x11/i.test(hint) ? "linux" : "other",
        nativeOcr,
        nativeOcrFormats: nativeOcr ? ["image/png", "image/jpeg"] : [],
      };
    }

    case "cmd_task_create": return copy(createTask(args));
    case "cmd_project_conversation_create": return copy(createProjectConversation(args));
    case "cmd_task_prepare": return undefined;
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
      for (let terminalIndex = terminals.length - 1; terminalIndex >= 0; terminalIndex -= 1) {
        const terminalId = terminals[terminalIndex].id;
        if (terminalOwners.get(terminalId) !== taskId) continue;
        terminals.splice(terminalIndex, 1);
        terminalOwners.delete(terminalId);
        terminalOutputs.delete(terminalId);
        terminalInputs.delete(terminalId);
      }
      return undefined;
    }
    case "cmd_task_set_workspace": return copy(setTaskWorkspace(args));
    case "cmd_task_choose_workspace": {
      const workspace = normalizeWorkspace(browserMockWorkspaces[0]);
      return copy(setTaskWorkspace({ ...args, workspacePath: workspace.canonical_path }));
    }
    case "cmd_task_set_agent_engine": return copy(setTaskField(args, "agent_engine"));
    case "cmd_task_set_provider": return copy(setTaskField(args, "provider_name"));
    case "cmd_task_set_model": return copy(setTaskField(args, "model"));
    case "cmd_task_set_inference": {
      const task = taskById(stringArg(args, "taskId"));
      task.inference = copy((args.inference as InferenceOptions | undefined) ?? {});
      touchTask(task);
      return copy(task);
    }
    case "cmd_task_rename": return copy(renameTask(args));
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
    case "cmd_task_clear_context": return copy(clearTaskContext(stringArg(args, "taskId")));
    case "cmd_task_compact_context": {
      const count = browserMockMessages(stringArg(args, "taskId")).filter((item) => item.kind === "message").length;
      return { compacted: count > 4, before_messages: count, after_messages: count > 4 ? 3 : count };
    }
    case "cmd_task_detail": {
      const detail = copy(detailById(stringArg(args, "taskId")));
      detail.pending_plan_entry_offer = browserMockPlanEntryOffer
        ? copy({ ...browserMockPlanEntryOffer, task_id: stringArg(args, "taskId") })
        : null;
      return detail;
    }
    case "cmd_plan_entry_offer_get": return null;
    case "cmd_plan_entry_decide": {
      const offer = browserMockPlanEntryOffer;
      if (!offer) throw new Error("没有待决的 Plan 入口建议");
      const decision = String(
        (args as { input?: { decision?: string } }).input?.decision ?? "continue",
      );
      browserMockPlanEntryOffer = null;
      return copy({
        ...offer,
        state: decision === "accept" ? "accepted" : "declined",
        continuation_state: "sent",
      });
    }
    case "cmd_plan_entry_retry_continuation": {
      return null;
    }
    case "cmd_planning_status": return copy(browserMockPlanningStatus);
    case "cmd_task_detail_batch": return {
      details: copy(((args.taskIds as string[] | undefined) ?? []).map((id) => browserMockDetails[id]).filter(Boolean)),
    };

    case "cmd_agent_send": sendMessage(args); return undefined;
    case "cmd_agent_attachment_preview": {
      // 与正式后端同形状：media_type + 标准 Base64（1×1 透明 PNG）。
      return {
        media_type: "image/png",
        data: "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=",
      };
    }
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
    case "cmd_agent_queue_reorder": {
      const detail = detailById(stringArg(args, "taskId"));
      const queueIds = Array.isArray(args.queueIds)
        ? args.queueIds.filter((id): id is string => typeof id === "string")
        : [];
      const pending = detail.queued_messages.filter((item) => item.state === "queued");
      const expected = pending.map((item) => item.id).sort();
      const requested = [...queueIds].sort();
      if (
        expected.length !== requested.length
        || expected.some((id, index) => id !== requested[index])
        || new Set(queueIds).size !== queueIds.length
      ) {
        throw new Error("待发送队列已经变化，请刷新后重试排序");
      }
      const byId = new Map(pending.map((item) => [item.id, item]));
      const reordered = queueIds.map((id) => byId.get(id)!);
      let queuedIndex = 0;
      detail.queued_messages = detail.queued_messages.map((item) =>
        item.state === "queued" ? reordered[queuedIndex++] : item
      );
      return undefined;
    }
    case "cmd_agent_queue_update": {
      const detail = detailById(stringArg(args, "taskId"));
      const queueId = stringArg(args, "queueId");
      const message = stringArg(args, "message").trim();
      if (!message) throw new Error("队列消息不能为空");
      const queued = detail.queued_messages.find((item) => item.id === queueId);
      if (!queued || (queued.state !== "queued" && queued.state !== "failed")) {
        throw new Error("这条消息已经开始处理或不在当前队列中");
      }
      queued.message = message;
      queued.state = "queued";
      queued.updated_at = nowIso();
      return undefined;
    }
    case "cmd_agent_queue_steer": {
      const taskId = stringArg(args, "taskId");
      const queueId = stringArg(args, "queueId");
      const task = taskById(taskId);
      const detail = detailById(taskId);
      const index = detail.queued_messages.findIndex(
        (item) => item.id === queueId && item.state === "queued",
      );
      if (index < 0) throw new Error("这条消息已经开始处理或不在当前队列中");
      detail.queued_messages.splice(index, 1);
      addEvent(detail, "user_steered");
      touchTask(task);
      return "steered";
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
    case "cmd_workflow_skills_list": {
      const workspacePath = typeof args.workspacePath === "string" ? args.workspacePath : null;
      const global = workflowSkills.map((skill) => ({ ...skill, inherited: Boolean(workspacePath) }));
      const project = workspacePath ? projectWorkflowSkills.get(workspacePath) ?? [] : [];
      return copy([...global, ...project]);
    }
    case "cmd_workflow_skill_save": {
      const draft = args.draft as WorkflowSkillDraft;
      const id = draft.id ?? `custom:${globalThis.crypto.randomUUID()}`;
      const saved: WorkflowSkill = {
        ...draft,
        id,
        overridden: draft.source === "builtin",
        inherited: false,
      };
      const workspacePath = typeof args.workspacePath === "string" ? args.workspacePath : null;
      if (draft.scope === "project") {
        if (!workspacePath) throw new Error("保存项目 Skill 需要项目作用域");
        if (workflowSkills.some((skill) => skill.name === draft.name)) {
          throw new Error(`Skill 调用名 /${draft.name} 已被全局 Skill 使用，请换一个名称`);
        }
        const project = projectWorkflowSkills.get(workspacePath) ?? [];
        const duplicate = project.find((skill) => skill.name === draft.name && skill.id !== id);
        if (duplicate) throw new Error(`Skill 调用名 /${draft.name} 已存在`);
        const index = project.findIndex((skill) => skill.id === id);
        if (index >= 0) project[index] = saved;
        else project.push(saved);
        projectWorkflowSkills.set(workspacePath, project);
      } else {
        const index = workflowSkills.findIndex((skill) => skill.id === id);
        if (index >= 0) workflowSkills[index] = saved;
        else workflowSkills.push(saved);
      }
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
      if (args.scope === "project" && typeof args.workspacePath === "string") {
        projectWorkflowSkills.set(
          args.workspacePath,
          (projectWorkflowSkills.get(args.workspacePath) ?? []).filter((skill) => skill.id !== id),
        );
      } else {
        workflowSkills = workflowSkills.filter((skill) => skill.id !== id || skill.source === "builtin");
      }
      return null;
    }
    case "cmd_workflow_skill_sync_to_global": {
      const id = stringArg(args, "id");
      const workspacePath = stringArg(args, "workspacePath");
      const project = projectWorkflowSkills.get(workspacePath) ?? [];
      const index = project.findIndex((skill) => skill.id === id);
      if (index < 0) throw new Error(`Project Skill not found: ${id}`);
      const [skill] = project.splice(index, 1);
      if (workflowSkills.some((item) => item.name === skill.name)) {
        project.splice(index, 0, skill);
        throw new Error(`Skill 调用名 /${skill.name} 已被全局 Skill 使用`);
      }
      projectWorkflowSkills.set(workspacePath, project);
      const global = { ...skill, scope: "global" as const, inherited: false };
      workflowSkills.push(global);
      return copy(global);
    }
    case "cmd_knowledge_prompts_get": {
      const workspacePath = typeof args.workspacePath === "string" ? args.workspacePath : null;
      return copy(browserKnowledgePromptSnapshot(workspacePath));
    }
    case "cmd_knowledge_prompts_save": {
      const workspacePath = typeof args.workspacePath === "string" ? args.workspacePath : null;
      const mainAgent = stringArg(args, "mainAgent");
      const subagent = stringArg(args, "subagent");
      if (workspacePath) {
        projectPromptConfigs.set(workspacePath, {
          mode: args.mode === "override" ? "override" : "append",
          main_agent: mainAgent,
          subagent,
        });
      } else {
        browserMockSettings.config.agent_prompts = { main_agent: mainAgent, subagent };
      }
      return copy(browserKnowledgePromptSnapshot(workspacePath));
    }
    case "cmd_knowledge_prompts_reset": {
      const workspacePath = typeof args.workspacePath === "string" ? args.workspacePath : null;
      if (workspacePath) projectPromptConfigs.delete(workspacePath);
      else setConfigValue("agent_prompts", null);
      return copy(browserKnowledgePromptSnapshot(workspacePath));
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
      const accepted = acceptedReviewPaths.get(taskId) ?? new Set<string>();
      const rejected = rejectedReviewPaths.get(taskId) ?? new Set<string>();
      const paths = detail.changes
        .filter((change) => !accepted.has(change.path) && !rejected.has(change.path))
        .map((change) => change.path);
      detail.changes = detail.changes.filter((change) => accepted.has(change.path));
      detail.task.state = "idle";
      touchTask(detail.task);
      markTaskNotificationsRead(taskId);
      return paths;
    }
    case "cmd_rollback_task_to_checkpoint": {
      const taskId = stringArg(args, "taskId");
      const detail = detailById(taskId);
      const accepted = acceptedReviewPaths.get(taskId) ?? new Set<string>();
      const rejected = rejectedReviewPaths.get(taskId) ?? new Set<string>();
      const paths = detail.changes
        .filter((change) => !accepted.has(change.path) && !rejected.has(change.path))
        .map((change) => change.path);
      detail.changes = detail.changes.filter((change) => accepted.has(change.path));
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
    case "cmd_companion_ensure": return true;

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
      for (let terminalIndex = terminals.length - 1; terminalIndex >= 0; terminalIndex -= 1) {
        const terminalId = terminals[terminalIndex].id;
        if (!removedTaskIds.includes(terminalOwners.get(terminalId) ?? "")) continue;
        terminals.splice(terminalIndex, 1);
        terminalOwners.delete(terminalId);
        terminalOutputs.delete(terminalId);
        terminalInputs.delete(terminalId);
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

    case "cmd_terminal_list": {
      const taskId = stringArg(args, "taskId");
      taskById(taskId);
      return copy(terminals.filter((terminal) => terminalOwners.get(terminal.id) === taskId));
    }
    case "cmd_terminal_create": return createTerminal(stringArg(args, "taskId"), stringArg(args, "shell"));
    case "cmd_terminal_create_codex": return createTerminal(stringArg(args, "taskId"), "Codex CLI");
    case "cmd_terminal_send": {
      sendTerminalInput(stringArg(args, "taskId"), stringArg(args, "id"), stringArg(args, "text"));
      return undefined;
    }
    case "cmd_terminal_read":
    case "cmd_terminal_snapshot": {
      const id = stringArg(args, "id");
      requireOwnedTerminal(stringArg(args, "taskId"), id);
      return terminalOutputs.get(id) ?? "";
    }
    case "cmd_terminal_raw_snapshot": {
      const id = stringArg(args, "id");
      requireOwnedTerminal(stringArg(args, "taskId"), id);
      const output = terminalOutputs.get(id) ?? "";
      return { output, cursor: output.length };
    }
    case "cmd_terminal_raw_since": {
      const id = stringArg(args, "id");
      requireOwnedTerminal(stringArg(args, "taskId"), id);
      const output = terminalOutputs.get(id) ?? "";
      const cursor = typeof args.cursor === "number" ? args.cursor : 0;
      const reset = cursor > output.length;
      return { output: reset ? output : output.slice(cursor), cursor: output.length, reset };
    }
    case "cmd_terminal_kill": {
      const id = stringArg(args, "id");
      const terminal = requireOwnedTerminal(stringArg(args, "taskId"), id);
      const index = terminals.indexOf(terminal);
      if (index >= 0) terminals.splice(index, 1);
      terminalOwners.delete(id);
      terminalOutputs.delete(id);
      terminalInputs.delete(id);
      return undefined;
    }
    case "cmd_terminal_resize": {
      requireOwnedTerminal(stringArg(args, "taskId"), stringArg(args, "id"));
      return undefined;
    }

    case "cmd_recovery_data": return copy(browserMockRecovery);
    case "cmd_recovery_cleanup": {
      const interruptedTaskIds = new Set(browserMockRecovery.interrupted_tasks);
      const recoveredAt = nowIso();
      let tasksInterrupted = 0;
      let runsClosed = 0;
      for (const task of browserMockTasks) {
        if (!interruptedTaskIds.has(task.id)) continue;
        if (task.state === "in_progress" || task.state === "exploring") {
          task.state = "interrupted";
          task.updated_at = recoveredAt;
          tasksInterrupted += 1;
        }
        const detail = browserMockDetails[task.id];
        if (!detail) continue;
        for (const run of detail.runs) {
          if (run.ended_at != null) continue;
          run.ended_at = recoveredAt;
          run.review_state = "aborted";
          run.summary ??= "应用退出前的遗留运行已安全收束。";
          runsClosed += 1;
        }
      }
      const permissionsDenied = browserMockRecovery.orphaned_permissions;
      browserMockRecovery.interrupted_tasks.splice(0);
      browserMockRecovery.orphaned_permissions = 0;
      return {
        runs_closed: runsClosed,
        tasks_interrupted: tasksInterrupted,
        permissions_denied: permissionsDenied,
        tool_calls_closed: 0,
      };
    }
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
    case "cmd_session_messages_for_branch": return copy(messagesForBranch(
      stringArg(args, "taskId"),
      stringArg(args, "branchId"),
    ));
    case "cmd_subagent_session_messages": return copy(browserMockSubagentMessages(stringArg(args, "taskId"), stringArg(args, "subagentId")));
    case "cmd_subagent_session_message_page": return mockSubagentMessagePage(
      stringArg(args, "taskId"),
      stringArg(args, "subagentId"),
      subagentPageRequestArg(args),
    );

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
      const forcedResult = (globalThis as { __rCodeBrowserMockMemoryReviewResult?: string | null })
        .__rCodeBrowserMockMemoryReviewResult;
      if (forcedResult !== undefined) return forcedResult;
      if (!mockMemoryOverview.settings.enabled || !mockMemoryOverview.settings.reviewer) return null;
      const selection = selectMemoryReviewTask(args);
      if (!selection) return null;
      const id = `memory-job-${++sequence}`;
      mockMemoryOverview.recent_jobs.unshift({
        sequence: String(sequence), id, task_id: selection.task.id, source_workspace_id: selection.workspaceId,
        trigger: "manual", status: "queued", provider_name: mockMemoryOverview.settings.reviewer.provider_name,
        model: mockMemoryOverview.settings.reviewer.model, attempt: 0, suppressed_turn_count: 0,
        error_code: null, effect_count: null, created_at: nowIso(), updated_at: nowIso(),
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
      if (mockSubagentPool.slots.some((slot) => slot.source.kind === "api_provider" && slot.source.provider_id === name)) {
        throw new Error("该 API Provider 正被子代理候选池引用，请先移除对应槽位");
      }
      delete browserMockSettings.config.providers?.[name];
      delete browserMockSettings.provider_status[name];
      markMockSubagentSourceStale({ kind: "api_provider", provider_id: name });
      if (browserMockSettings.config.default_provider === name) browserMockSettings.config.default_provider = undefined;
      return undefined;
    }
    case "cmd_subagent_provider_catalog": return copy(mockSubagentCatalog());
    case "cmd_subagent_provider_test":
      return copy(mockSubagentProviderTest(args.request as SubagentProviderProbeRequest));
    case "cmd_subagent_provider_test_batch": {
      if (!Array.isArray(args.requests)) throw new Error("子代理批量测试参数无效");
      return copy(mockSubagentProviderTestBatch(args.requests as SubagentProviderProbeRequest[]));
    }
    case "cmd_subagent_pool_snapshot": return copy(mockSubagentPoolSnapshot());
    case "cmd_subagent_pool_save":
      return copy(mockSaveSubagentPool(stringArg(args, "revision"), args.pool as SubagentPoolConfig));
    case "cmd_rtk_status": return copy(browserMockRtkStatus() satisfies RtkStatus);
    case "cmd_rtk_set_enabled": return copy(browserMockSetRtkEnabled(args.enabled === true));
    case "cmd_rtk_open_security_exclusions": return undefined;
    case "cmd_codex_integration_status": return copy(browserMockCodexIntegrationStatus());
    case "cmd_codex_install_cli": return copy(browserMockInstallCli());
    case "cmd_codex_start_login":
    case "cmd_codex_start_device_login": browserMockLogin(); return undefined;
    case "cmd_codex_install_skill": browserMockInstallSkill(); return undefined;
    case "cmd_codex_install_mcp_server": browserMockInstallMcp(); return undefined;
    case "cmd_codex_setup_collaboration": return copy(browserMockSetupCollaboration());
    case "cmd_codex_cli_preferences": return copy(browserMockCliPreferences());
    case "cmd_codex_save_cli_preferences": {
      const result = browserMockSaveCliPreferences(
        optionalStringArg(args, "model"),
        optionalStringArg(args, "reasoningEffort"),
        optionalStringArg(args, "verbosity"),
        optionalStringArg(args, "permissionMode"),
      );
      markMockSubagentSourceStale({ kind: "codex_cli" });
      return copy(result);
    }
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
