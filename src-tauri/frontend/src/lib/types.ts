/**
 * 后端数据结构（serde 序列化形状）。
 * 与 crates/r-code-core/src/dto.rs、src-tauri/src/commands.rs 对齐。
 * 注意：枚举大小写各异（TaskState snake_case、risk_level 大写、ReviewState snake_case）。
 * 时间均为 RFC3339 字符串。
 */

// ---------- 任务 ----------
export type TaskMode = "ask" | "edit" | "auto";
export type TaskState =
  | "idle"
  | "exploring"
  | "in_progress"
  | "interrupted"
  | "review_ready"
  | "archived";

export interface Task {
  id: string;
  /** 未附加工作区时为 null：会话仍可聊天，但没有本地工具。 */
  workspace_path: string | null;
  /** 会话绑定的模型服务；null 代表兼容旧会话并回退全局默认。 */
  provider_name: string | null;
  /** 会话绑定的具体模型；null 表示沿用该服务在设置里配置的默认模型。 */
  model: string | null;
  title: string;
  goal: string;
  mode: TaskMode;
  state: TaskState;
  worktree_path: string | null;
  created_at: string;
  updated_at: string;
}

export type ReviewState =
  | "pending"
  | "accepted"
  | "auto_accepted"
  | "rolled_back"
  | "aborted"
  | "answered"
  | "failed";

export type AgentRunKind = "main" | "subagent";

export interface AgentRun {
  id: string;
  task_id: string;
  branch_id: string;
  parent_run_id: string | null;
  agent_kind: AgentRunKind;
  agent_label: string | null;
  summary: string | null;
  delegated_by_tool_call_id: string | null;
  model: string;
  review_state: ReviewState;
  started_at: string;
  ended_at: string | null;
  usage_json: string | null;
}

export type TaskEventType =
  | "task_created"
  | "state_changed"
  | "run_started"
  | "run_ended"
  | "user_steered"
  | "user_message_queued"
  | "queue_dispatched"
  | "run_aborted"
  | "session_branched"
  | "subagent_started"
  | "subagent_finished"
  | "tool_call"
  | "tool_result"
  | "permission_requested"
  | "permission_decided"
  | "file_changed"
  | "verification_run"
  | "system";

export interface TaskEvent {
  id: number;
  task_id: string;
  branch_id: string;
  event_type: TaskEventType;
  created_at: string;
}

export type ChangeType = "create" | "modify" | "delete" | "rename";

export interface FileChange {
  id: string;
  task_id: string;
  tool_call_id: string | null;
  path: string;
  change_type: ChangeType;
  before_hash: string | null;
  after_hash: string | null;
  old_path: string | null;
  created_at: string;
}

export type RiskLevel = "R0" | "R1" | "R2" | "R3" | "R4";
export type PermissionDecision = "pending" | "allow" | "allow_always" | "deny";

export interface PermissionRequest {
  id: string;
  task_id: string;
  tool_call_id: string;
  run_id?: string | null;
  caller?: string | null;
  tool_name: string;
  risk_level: RiskLevel;
  input_summary: string;
  decision: PermissionDecision;
  created_at: string;
  decided_at: string | null;
}

export type VerificationStatus =
  | "running"
  | "passed"
  | "failed"
  | "superseded"
  | "stale"
  | "timeout";

export interface VerificationRecord {
  id: string;
  task_id: string;
  run_id: string;
  command: string;
  status: VerificationStatus;
  output_blob_key: string | null;
  exit_code: number | null;
  started_at: string;
  ended_at: string | null;
}

export interface TaskDetail {
  task: Task;
  active_branch: SessionBranch;
  branches: SessionBranch[];
  runs: AgentRun[];
  events: TaskEvent[];
  changes: FileChange[];
  permissions: PermissionRequest[];
  verifications: VerificationRecord[];
  queued_messages: QueuedMessage[];
}

// ---------- 会话分支与运行控制 ----------
export type AgentSendMode = "auto" | "steer" | "queue" | "send_now";

export interface SessionBranch {
  id: string;
  task_id: string;
  parent_branch_id: string | null;
  forked_from_message_id: string | null;
  storage_id: string;
  is_active: boolean;
  created_at: string;
}

export type QueuedMessageState = "queued" | "dispatching" | "sent" | "cancelled" | "failed";

export interface QueuedMessage {
  id: string;
  task_id: string;
  branch_id: string;
  message: string;
  state: QueuedMessageState;
  priority: number;
  created_at: string;
  updated_at: string;
}

// ---------- Workspace ----------
export type ProjectAccessMode = "request_approval" | "risk_based" | "full_access";

export interface Workspace {
  canonical_path: string;
  display_name: string;
  access_mode: ProjectAccessMode;
  last_opened_at: string;
}

// ---------- 搜索 ----------
export interface SearchMatch {
  path: string;
  line: number;
  column: number;
  line_text: string;
}

// ---------- 终端 ----------
export type TerminalState = "idle" | "busy" | "agent" | "exited";

export interface TerminalInfo {
  id: string;
  state: TerminalState;
  shell: string;
  is_busy: boolean;
}

// ---------- 恢复 ----------
export interface RecoveryPageData {
  interrupted_tasks: string[];
  orphaned_permissions: number;
}

// ---------- 回放 ----------
export type ReplayDepth = "recap" | "explore" | "verify";
export type EvidenceLevel =
  | "verified"
  | "recorded"
  | "observed"
  | "inferred"
  | "missing";

export interface ReplayEntry {
  event_type: string;
  timestamp: string;
  summary: string;
  evidence_level: EvidenceLevel;
  details?: unknown;
}

// ---------- Agent 流式事件（serde tag="type"） ----------
/** 仅描述可观察的运行活动；不包含模型私有推理。 */
export type AgentActivityPhase =
  | "requesting"
  | "streaming"
  | "tool"
  | "waiting_permission"
  | "steer_accepted"
  | "steer_applied"
  | "finalizing";

export type SubagentState =
  | "queued"
  | "running"
  | "waiting_permission"
  | "completed"
  | "failed"
  | "cancelled";

export interface AgentEventScope {
  run_id: string;
  agent_id: string;
  parent_run_id?: string;
  agent_kind: AgentRunKind;
  agent_label?: string;
  delegated_by_tool_call_id?: string;
}

export type AgentEvent =
  | { type: "message"; text: string; delta: boolean }
  | { type: "tool_call"; name: string; input: unknown; call_id: string }
  | { type: "tool_result"; call_id: string; output: unknown; is_error: boolean }
  | { type: "plan"; steps: { description: string; completed: boolean }[] }
  | { type: "activity"; phase: AgentActivityPhase; detail?: string }
  | { type: "scoped"; scope: AgentEventScope; event: AgentEvent }
  | { type: "subagent_lifecycle"; state: SubagentState; detail?: string }
  | { type: "state"; state: TaskState };

/** "agent-event" Tauri 事件的信封（后端 drain 循环 emit）。 */
export interface AgentEventEnvelope {
  task_id: string;
  event: AgentEvent;
}

// ---------- 日志（cmd_logs_tail 返回） ----------
export interface LogEntry {
  timestamp: string;
  level: string;
  target: string;
  message: string;
}

// ---------- 会话消息（cmd_session_messages 返回，Room 时间线数据源） ----------
export interface SessionMessage {
  /** 稳定消息标识，用于编辑后从此处创建新分支。 */
  id?: string;
  /** 当前读取的会话分支。 */
  branch_id: string;
  kind: "meta" | "message" | "tool_call" | "tool_result" | "system";
  role?: "user" | "assistant" | "system";
  /** message 的文本内容 */
  text?: string;
  /** tool_call：工具名与输入 */
  tool_name?: string;
  call_id?: string;
  input_json?: string;
  /** tool_result：输出 */
  output_json?: string;
  is_error?: boolean;
  timestamp?: string;
}

// ---------- 变更 diff（cmd_change_diff 返回） ----------
export interface ChangeDiffLine {
  kind: "ctx" | "add" | "del" | "hunk";
  text: string;
  old_no?: number;
  new_no?: number;
}

export interface ChangeDiff {
  supported: boolean;
  path: string;
  change_type?: ChangeType;
  before_hash?: string | null;
  after_hash?: string | null;
  lines?: ChangeDiffLine[];
  truncated?: boolean;
}

// ---------- 设置（serde_json::Value 的已知子集） ----------
export interface ProviderConfig {
  base_url?: string;
  api_key?: string;
  model?: string;
  max_tokens?: number;
  temperature?: number;
  /** 用户显式选定的线路协议。缺省 = 升级前保存的旧配置，后端会按目录推断。 */
  protocol?: ProviderProtocol;
}

/** 线路协议。同一厂商的不同 base_url 往往是不同协议，不能按名字推断。 */
export type ProviderProtocol = "anthropic_messages" | "openai_chat" | "openai_responses";

export type ProviderCategory = "official" | "cn_official" | "cloud_provider" | "aggregator";

export interface ProviderTemplateVar {
  name: string;
  label: string;
  placeholder: string;
}

/**
 * 一条备用线路。
 *
 * 协议跟着地址走：目录里多数候选是同一厂商的**另一个协议口**（火山 `/api/coding`
 * 是 Anthropic、`/api/coding/v3` 是 OpenAI），切线路时必须把协议一起切过去。
 */
export interface ProviderEndpoint {
  url: string;
  protocol: ProviderProtocol;
  /** 该入口支持的全部协议。按地址声明，与主入口的 `native` 可以不同。 */
  native: ProviderProtocol[];
  label: string;
}

/** 后端 `cmd_provider_catalog` 下发的一条预设，字段与 provider_catalog.rs 对应。 */
export interface ProviderPreset {
  id: string;
  label: string;
  protocol: ProviderProtocol;
  native: ProviderProtocol[];
  auth: "x_api_key" | "bearer";
  base_url: string;
  reasoning_replay: boolean;
  model: string;
  models: string[];
  category: ProviderCategory;
  website_url: string;
  api_key_url: string | null;
  endpoint_candidates: ProviderEndpoint[];
  template_vars: ProviderTemplateVar[];
  max_output_tokens: number | null;
  context_window: number | null;
  note: string | null;
}

export interface ProviderCatalog {
  presets: ProviderPreset[];
}

export interface ProviderStatus {
  configured: boolean;
  ready: boolean;
  source: "keychain" | "environment" | "legacy_file" | "missing" | string;
  /**
   * 这条配置实际会用的线路协议，由后端 `resolve_effective_protocol` 算出。
   *
   * 已存 protocol 时就是它；没存过则是后端的推断结果。前端**不要**自己按预设推断——
   * 后端在地址被改写时会走启发式，前端看不到那部分逻辑，各猜一次必然对不上。
   */
  effective_protocol?: ProviderProtocol;
}

export interface ProviderSettingsInput {
  name: string;
  baseUrl: string;
  model: string;
  apiKey?: string | null;
  maxTokens?: number | null;
  temperature?: number | null;
  /** 省略 = 沿用已存的选择；从未存过则落到预设默认值。 */
  protocol?: ProviderProtocol | null;
  activate?: boolean | null;
}

export interface CodexIntegrationStatus {
  cli_available: boolean;
  cli_path?: string | null;
  config_path: string;
  config_exists: boolean;
  auth_path: string;
  authenticated: boolean;
  skill_path: string;
  skill_status: "not_installed" | "up_to_date" | "update_available" | string;
  wire_api: "responses" | string;
}

export interface AppConfig {
  default_provider?: string;
  log_level?: string;
  providers?: Record<string, ProviderConfig>;
  mcp_servers?: Record<string, unknown>;
  storage?: Record<string, unknown>;
  compaction?: Record<string, unknown>;
  tauri?: Record<string, unknown>;
  [key: string]: unknown;
}

/** cmd_settings_get 返回：宽松加载，validation 为软提示（未配置 provider 等）。 */
export interface SettingsResponse {
  config: AppConfig;
  validation: string | null;
  provider_status: Record<string, ProviderStatus>;
}

// ---------- 支持包预览 ----------
export interface SupportBundlePreview {
  version: string;
  platform: string;
  generated_at: string;
  logs: { level: string; message: string; timestamp: string }[];
  config_summary: Record<string, unknown>;
  db_stats: { task_count: number; run_count: number; tool_call_count: number };
}
