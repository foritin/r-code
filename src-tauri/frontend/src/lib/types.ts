/**
 * 后端数据结构（serde 序列化形状）。
 * 与 crates/r-code-core/src/dto.rs、src-tauri/src/commands.rs 对齐。
 * 注意：枚举大小写各异（TaskState snake_case、risk_level 大写、ReviewState snake_case）。
 * 时间均为 RFC3339 字符串。
 */

// ---------- 任务 ----------
export type TaskMode = "ask" | "edit" | "auto" | "plan";
export type TaskAgentEngine = "r_code" | "codex";
export interface InferenceOptions {
  /** 未设置表示沿用当前模型服务默认。 */
  thinking?: "enabled" | "disabled" | "adaptive" | string | null;
  reasoning_effort?: string | null;
  verbosity?: "low" | "medium" | "high" | string | null;
}

export type AttachmentKind = "image" | "text" | "pdf";

/** 发送给模型的内联附件；data 是不含 data: 前缀的标准 Base64。 */
export interface AttachmentInput {
  name: string;
  mediaType: string;
  data: string;
}

export interface SessionAttachmentMeta {
  name: string;
  media_type: string;
  kind: AttachmentKind;
}
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
  /** 会话级模型专属参数；空对象表示服务默认。 */
  inference: InferenceOptions;
  /** 当前会话实际使用的主 Agent；与全局默认解耦。 */
  agent_engine: TaskAgentEngine;
  title: string;
  goal: string;
  /** true only after the user explicitly starts a persistent Goal lifecycle. */
  goal_active: boolean;
  mode: TaskMode;
  state: TaskState;
  worktree_path: string | null;
  created_at: string;
  updated_at: string;
}

// ---------- Plan / Human in the loop ----------
export type PlanState =
  | "draft"
  | "awaiting_input"
  | "ready"
  | "approved"
  | "executing"
  | "completed"
  | "cancelled";

export type PlanItemState =
  | "proposed"
  | "pending"
  | "in_progress"
  | "blocked"
  | "completed"
  | "failed"
  | "cancelled";

export type PlanQuestionSetState = "pending" | "answered" | "skipped";
export type PlanContinuationState =
  | "not_requested"
  | "pending"
  | "dispatching"
  | "dispatched"
  | "failed";
export type PlanImplementationDispatchState = PlanContinuationState;

export interface Plan {
  id: string;
  task_id: string;
  revision: number;
  state: PlanState;
  approved_revision: number | null;
  projection_path: string | null;
  projection_revision: number | null;
  projection_error: string | null;
  created_at: string;
  updated_at: string;
  approved_at: string | null;
  implementation_dispatch_state: PlanImplementationDispatchState;
  implementation_dispatch_error: string | null;
  implementation_queue_message_id: string | null;
  implementation_dispatched_at: string | null;
}

export interface PlanItem {
  id: string;
  plan_id: string;
  revision: number;
  ordinal: number;
  title: string;
  description: string;
  section_path: string[];
  state: PlanItemState;
  depends_on: string[];
  created_at: string;
  updated_at: string;
  started_at: string | null;
  completed_at: string | null;
}

export interface PlanQuestionOption {
  id: string;
  label: string;
  description: string;
}

/** Persisted answer shape returned by the aggregate. */
export type PlanQuestionAnswer =
  | { kind: "option"; option_id: string }
  | { kind: "free_form"; text: string };

export interface PlanQuestion {
  id: string;
  question_set_id: string;
  ordinal: number;
  header: string;
  question: string;
  options: PlanQuestionOption[];
  answer: PlanQuestionAnswer | null;
  answered_at: string | null;
}

export interface PlanQuestionSet {
  id: string;
  plan_id: string;
  revision: number;
  state: PlanQuestionSetState;
  answer_idempotency_key: string | null;
  continuation_state: PlanContinuationState;
  continuation_error: string | null;
  questions: PlanQuestion[];
  created_at: string;
  resolved_at: string | null;
  dispatched_at: string | null;
}

/** `tasks.goal` remains the only persisted goal; this is its Plan aggregate view. */
export interface PlanGoal {
  task_id: string;
  goal: string;
  updated_at: string;
}

export interface PlanView {
  plan: Plan;
  goal: PlanGoal;
  items: PlanItem[];
  pending_question_set: PlanQuestionSet | null;
  /** Latest resolved set while its continuation still needs attention or dispatch. */
  continuation_question_set: PlanQuestionSet | null;
}

// ---------- Plan enhanced review ----------

export type PlanReviewDecisionKind = "accepted" | "rejected";
export type PlanReviewScope = "feature" | "file";

export interface EnhancedReviewEventView {
  sequence: number;
  event_id: string;
  tool_call_id: string;
  before_exists: boolean;
  after_exists: boolean;
  before_blob_hash: string | null;
  after_blob_hash: string | null;
  /** Unified UTF-8 patch. Binary changes intentionally omit this field. */
  patch: string | null;
  binary: boolean;
}

export interface EnhancedReviewFileView {
  path: string;
  decision: PlanReviewDecisionKind | null;
  first_sequence: number;
  last_sequence: number;
  events: EnhancedReviewEventView[];
}

export interface EnhancedReviewGroupView {
  item_id: string;
  ordinal: number;
  title: string;
  description: string;
  /** Authoritative Plan item state returned with the review snapshot. */
  state: PlanItemState;
  decision: PlanReviewDecisionKind | null;
  files: EnhancedReviewFileView[];
}

export interface EnhancedReviewView {
  task_id: string;
  plan_id: string;
  plan_revision: number;
  groups: EnhancedReviewGroupView[];
}

export interface EnhancedReviewTarget {
  task_id: string;
  plan_id: string;
  plan_revision: number;
  item_id: string;
  path: string | null;
}

export interface PlanReviewDecision {
  id: string;
  plan_id: string;
  plan_revision: number;
  item_id: string;
  scope: PlanReviewScope;
  path: string | null;
  decision: PlanReviewDecisionKind;
  decided_at: string;
}

export interface PlanRejectResult {
  operation_id: string;
  decision: PlanReviewDecision;
  changed_paths: string[];
  idempotent: boolean;
}

/** Exact tagged input contract accepted by `cmd_plan_answer`. */
export type PlanQuestionAnswerInput =
  | { kind: "option"; question_id: string; option_id: string }
  | { kind: "text"; question_id: string; text: string };

export interface AnswerPlanQuestionsInput {
  question_set_id: string;
  expected_revision: number;
  idempotency_key: string;
  skip_all: boolean;
  answers: PlanQuestionAnswerInput[];
}

export interface UpdatePlanItemInput {
  plan_id: string;
  item_id: string;
  expected_revision: number;
  state: PlanItemState;
}

export interface ContextCompactionResult {
  compacted: boolean;
  before_messages: number;
  after_messages: number;
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
export type AgentRunRuntimeKind = "native" | "codex_exec" | "codex_mcp";

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
  runtime_kind: AgentRunRuntimeKind;
  external_session_id: string | null;
  review_state: ReviewState;
  started_at: string;
  ended_at: string | null;
  usage_json: string | null;
  access_mode: SubagentAccessMode;
  /** 旧运行可能缺失；仅 full_access + true 表示仍需宿主审批。 */
  require_approval?: boolean;
  routing_reason: string | null;
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
  | "session_cleared"
  | "subagent_started"
  | "subagent_finished"
  | "tool_call"
  | "tool_result"
  | "permission_requested"
  | "permission_decided"
  | "file_changed"
  | "verification_run"
  | "change_requested"
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
  target?: string | null;
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

/** cmd_task_detail_batch：每项与单任务详情完全一致。 */
export interface TaskDetailBatch {
  details: TaskDetail[];
}

// ---------- 项目仪表盘 / 活动 ----------
export interface DashboardChangeSummary {
  files: number;
  created: number;
  modified: number;
  removed: number;
  renamed: number;
}

export interface DashboardTaskSummary {
  task: Task;
  activity: string;
  agent_label: string;
  pending_permission_count: number;
  active_run: AgentRun | null;
  change_summary: DashboardChangeSummary;
  latest_verification: VerificationRecord | null;
}

export type DashboardAttentionKind = "permission" | "review_ready";

export interface DashboardAttentionItem {
  kind: DashboardAttentionKind;
  task: Task;
  permission?: PermissionRequest;
  since: string;
}

export interface WorkspaceDashboardMetrics {
  task_count: number;
  archived_task_count: number;
  pending_permission_count: number;
  review_ready_count: number;
  running_task_count: number;
  active_subagent_count: number;
}

export interface WorkspaceDashboard {
  workspace: Workspace;
  generated_at: string;
  metrics: WorkspaceDashboardMetrics;
  tasks: DashboardTaskSummary[];
  attention: DashboardAttentionItem[];
  archived: Task[];
}

export interface ProjectActivityItem {
  id: string;
  at: string;
  kind: TaskEventType;
  summary: string;
  task_id: string;
  task_title: string;
  workspace_path: string | null;
  run_id?: string;
  actor?: string;
  metadata: unknown;
}

export interface ProjectActivityPage {
  items: ProjectActivityItem[];
  next_cursor?: string;
}

// ---------- 通知中心 ----------
export type NotificationKind =
  | "permission_requested"
  | "review_ready"
  | "change_requested"
  | "memory_approval_required"
  | "memory_project_updated";

export interface Notification {
  id: string;
  kind: NotificationKind;
  title: string;
  body: string;
  task_id: string | null;
  workspace_path: string | null;
  created_at: string;
  read_at: string | null;
}

export interface NotificationPage {
  notifications: Notification[];
  next_cursor?: string;
  unread_count: number;
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
export type WorkspaceMemoryMode = "inherit" | "read_only" | "off";
export type LegacyMemoryGitTracking = "tracked" | "untracked" | "unknown";

export interface LegacyMemoryStatus {
  exists: boolean;
  git_tracking: LegacyMemoryGitTracking;
}

export interface Workspace {
  id: string;
  canonical_path: string;
  display_name: string;
  access_mode: ProjectAccessMode;
  last_opened_at: string;
  memory_mode: WorkspaceMemoryMode;
  memory_generation: number;
}

// ---------- 演进记忆 ----------
export type MemoryKind = "preference" | "constraint" | "convention" | "decision" | "pitfall";
export type ProjectNotificationMode = "off" | "on" | "verbose";

export type MemoryOwner =
  | { scope: "global"; authorization: "manual" | "approved_candidate" }
  | { scope: "project"; workspace_id: string; origin: "manual" | "automatic_review" | "undo" };

export interface MemoryEntry {
  id: string;
  owner: MemoryOwner;
  kind: MemoryKind;
  content: string;
  normalized_hash: string;
  version: number;
  pinned: boolean;
  source_job_id: string | null;
  source_candidate_id: string | null;
  created_at: string;
  updated_at: string;
}

export interface ReviewerSelection {
  provider_name: string;
  model: string;
}

export interface MemoryReviewSettingsView {
  enabled: boolean;
  reviewer: ReviewerSelection | null;
  trigger_every_turns: number;
  explicit_remember_immediate: boolean;
  project_notification_mode: ProjectNotificationMode;
  version: number;
  review_generation: number;
  physical_cleanup_pending: boolean;
  updated_at: string;
}

export interface MemoryReviewSettingsUpdate {
  expected_version: number;
  enabled: boolean;
  reviewer: ReviewerSelection | null;
  trigger_every_turns: number;
  explicit_remember_immediate: boolean;
  project_notification_mode: ProjectNotificationMode;
}

export interface MemoryCandidateView {
  sequence: string;
  id: string;
  kind: MemoryKind;
  operation: "add" | "replace";
  target_entry_id: string | null;
  target_version: number | null;
  source_task_id: string | null;
  source_workspace_id: string | null;
  proposed_content: string;
  reason: string;
  confidence: number;
  created_at: string;
}

export interface MemoryReviewJobView {
  sequence: string;
  id: string;
  task_id: string;
  source_workspace_id: string | null;
  trigger: "cadence" | "manual" | "explicit_remember";
  status: "queued" | "running" | "succeeded" | "failed" | "interrupted" | "cancelled";
  provider_name: string;
  model: string;
  attempt: number;
  suppressed_turn_count: number;
  error_code: string | null;
  effect_count: number | null;
  created_at: string;
  updated_at: string;
}

export interface MemoryOverview {
  settings: MemoryReviewSettingsView;
  global_entries: MemoryEntry[];
  project_entries: MemoryEntry[];
  pending_candidates: MemoryCandidateView[];
  recent_jobs: MemoryReviewJobView[];
}

export interface MemoryEntryDraft {
  scope: "global" | "project";
  workspace_id: string | null;
  kind: MemoryKind;
  content: string;
  pinned: boolean;
}

export interface MemoryEntryEdit {
  expected_version: number;
  kind: MemoryKind;
  content: string;
  pinned: boolean;
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

/** 原始 PTY 快照，只能交给本机终端模拟器解释。 */
export interface TerminalRawSnapshot {
  output: string;
  cursor: number;
}

/** 自某个 PTY 游标以来的原始输出；reset 时需重放完整 output。 */
export interface TerminalRawBatch extends TerminalRawSnapshot {
  reset: boolean;
}

// ---------- 恢复 ----------
export interface RecoveryPageData {
  interrupted_tasks: string[];
  orphaned_permissions: number;
}

export interface RecoveryCleanupResult {
  runs_closed: number;
  tasks_interrupted: number;
  permissions_denied: number;
  tool_calls_closed: number;
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
  | "routing"
  | "requesting"
  | "streaming"
  | "tool"
  | "waiting_permission"
  | "steer_accepted"
  | "steer_applied"
  | "finalizing"
  | "reviewing";

export type SubagentState =
  | "queued"
  | "running"
  | "waiting_permission"
  | "completed"
  | "failed"
  | "cancelled";

export type SubagentAccessMode = "read_only" | "full_access";

export interface AgentEventScope {
  run_id: string;
  agent_id: string;
  parent_run_id?: string;
  agent_kind: AgentRunKind;
  agent_label?: string;
  delegated_by_tool_call_id?: string;
  runtime_kind?: AgentRunRuntimeKind;
  model?: string;
  access_mode?: SubagentAccessMode;
  /** M7：FullAccess + require_approval = 审批模式（inherit 自非全权父运行）。 */
  require_approval?: boolean;
  routing_reason?: string;
}

export type AgentEvent =
  | { type: "message"; text: string; delta: boolean }
  | { type: "reasoning"; text: string; delta: boolean }
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

// ---------- MCP / native web ----------
export type McpServerState = "disabled" | "stopped" | "starting" | "running" | "error";

export type McpServerSource =
  | { kind: "builtin" }
  | { kind: "user" }
  | { kind: "generated"; source_path: string; created_at: string }
  | {
      kind: "registry";
      registry_url: string;
      name: string;
      version: string;
      repository_url?: string;
    };

export type McpTransportView =
  | { type: "builtin" }
  | { type: "stdio"; executable: string; args: string[]; environment_names: string[] }
  | { type: "streamable_http"; url: string; header_names: string[] };

export interface McpServerView {
  id: string;
  display_name: string;
  description: string;
  enabled: boolean;
  builtin: boolean;
  source: McpServerSource;
  transport: McpTransportView;
  state: McpServerState;
  tool_count: number;
  error_code?: string;
  launch_approved: boolean;
}

export interface McpManagerSnapshot {
  servers: McpServerView[];
  settings_error?: string;
}

export type McpEditableTransport =
  | { type: "stdio"; executable: string; args: string[]; environment_names: string[] }
  | { type: "streamable_http"; url: string; header_names: string[] };

export interface McpUpsertRequest {
  id: string;
  display_name: string;
  description: string;
  transport: McpEditableTransport;
}

export type McpLaunchPreviewTransport =
  | { type: "stdio"; executable: string; args: string[]; environment_names: string[] }
  | { type: "streamable_http"; url: string; header_names: string[] };

export interface McpLaunchPreview {
  token: string;
  server_id: string;
  fingerprint: string;
  transport: McpLaunchPreviewTransport;
  expires_at: string;
}

export interface McpToggleResult {
  server: McpServerView;
  confirmation?: McpLaunchPreview;
}

export interface McpToolDescriptor {
  server_id: string;
  name: string;
  description: string;
  input_schema: unknown;
  read_only: boolean;
}

export interface McpCredentialStatus {
  name: string;
  configured: boolean;
}

export interface McpMarketEnvironmentVariable {
  name: string;
  description: string;
  required: boolean;
  secret: boolean;
  default_value?: string;
}

export type McpMarketInstallTransport =
  | {
      type: "stdio";
      package_kind: "npm" | "pypi";
      executable: string;
      args: string[];
      environment: McpMarketEnvironmentVariable[];
    }
  | { type: "streamable_http"; url: string; headers: McpMarketEnvironmentVariable[] };

export interface McpMarketInstallOption {
  id: string;
  label: string;
  transport: McpMarketInstallTransport;
}

export interface McpMarketServer {
  name: string;
  title: string;
  description: string;
  version: string;
  status: string;
  is_latest: boolean;
  suggested_id: string;
  repository_url?: string;
  install_options: McpMarketInstallOption[];
}

export interface McpMarketPage {
  servers: McpMarketServer[];
  next_cursor?: string;
  stale: boolean;
  fetched_at: string;
  registry_preview: boolean;
  registry_unreviewed: boolean;
}

export interface McpMarketInstallRequest {
  server: McpMarketServer;
  option_id: string;
  server_id: string;
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
  /** 图片正文仅返回元数据，绝不把 Base64 放进时间线 DTO。 */
  image_count?: number;
  image_media_types?: string[];
  /** 附件只返回名称、类型和分类，不返回正文或 Base64。 */
  attachments?: SessionAttachmentMeta[];
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
  line_id?: string;
  review_state?: "pending" | "accepted" | "rejected";
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
  /** Stable catalog/vendor identity; unlike the profile name and URL this is not cosmetic. */
  provider_kind?: string;
  max_tokens?: number;
  temperature?: number;
  /** 用户显式选定的线路协议。缺省 = 升级前保存的旧配置，后端会按目录推断。 */
  protocol?: ProviderProtocol;
  /** 是否在对话中展示 Provider 明确返回的思考内容/摘要；旧配置缺省为开启。 */
  show_reasoning?: boolean;
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

export type HostedWebFormat = "standard" | "dash_scope" | "open_router";
export type HostedWebRead = "none" | "via_search" | "dedicated";

/** 后端已验证并接线的一条厂商托管联网线路。 */
export interface HostedWebRoute {
  provider_id: string;
  provider_label: string;
  host_pattern: string;
  path: string;
  protocol: ProviderProtocol;
  model_patterns: string[];
  format: HostedWebFormat;
  read: HostedWebRead;
  docs_url: string;
  docs_label: string;
}

export interface ProviderCatalog {
  presets: ProviderPreset[];
  hosted_web_routes: HostedWebRoute[];
}

export interface ProviderModelsInput {
  /** 配置名，用于在 apiKey 留空时读取已保存的系统凭据。 */
  name: string;
  /** 当前预设 id；自建服务省略。 */
  preset?: string | null;
  baseUrl: string;
  apiKey?: string | null;
  protocol: ProviderProtocol;
}

export interface ProviderModelsResponse {
  models: string[];
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
  /** Empty string explicitly clears identity; omitted preserves a legacy caller's existing value. */
  providerKind?: string | null;
  baseUrl: string;
  model: string;
  apiKey?: string | null;
  maxTokens?: number | null;
  temperature?: number | null;
  /** 省略 = 沿用已存的选择；从未存过则落到预设默认值。 */
  protocol?: ProviderProtocol | null;
  /** 省略 = 沿用已存的选择；从未存过则默认展示。 */
  showReasoning?: boolean | null;
  activate?: boolean | null;
}

export interface CodexIntegrationStatus {
  cli_available: boolean;
  cli_path?: string | null;
  cli_version?: string | null;
  /** 本地命令不可用时的脱敏诊断，不含 CLI 原始输出。 */
  cli_error?: string | null;
  /** 是否找到可运行的 npm，可在用户确认后执行固定的官方安装命令。 */
  installer_available?: boolean;
  installer_command?: string;
  installer_error?: string | null;
  config_path: string;
  config_exists: boolean;
  auth_path: string;
  authenticated: boolean;
  /** 由 `codex login status` 得到；不根据 auth.json 是否存在猜测。 */
  auth_status?: "authenticated" | "not_authenticated" | "unknown" | string;
  auth_method?: "ChatGPT" | "API Key" | "访问令牌" | string | null;
  skill_path: string;
  skill_status: "not_installed" | "up_to_date" | "update_available" | string;
  /** 是否已在 Codex 中配置本机 `r-code` stdio MCP server。 */
  mcp_server_configured?: boolean;
  mcp_server_name?: string;
  /** CLI、登录、Skill 与 MCP 是否全部可以用于 R-Code 协作。 */
  integration_ready?: boolean;
  /** 后端根据真实探针归纳出的唯一下一步。 */
  setup_state?: "install_cli" | "login" | "check" | "configure" | "ready";
  wire_api: "responses" | string;
}

export interface CodexReasoningOption {
  effort: string;
  description: string;
}

export interface CodexModelOption {
  slug: string;
  display_name: string;
  description: string;
  default_reasoning_effort: string;
  supported_reasoning_efforts: CodexReasoningOption[];
  /** 来自当前 Codex CLI 模型目录的 input_modalities，而不是前端猜测。 */
  /** null 表示旧版 Codex CLI 没有提供 input_modalities。 */
  supports_images: boolean | null;
}

export interface CodexCliPreferences {
  /** null 表示不覆盖，继续使用 Codex 默认。 */
  model: string | null;
  reasoning_effort: string | null;
  verbosity: "low" | "medium" | "high" | string | null;
  /** R-Code 每次启动 Codex 子代理时读取的 config.toml 权限预设。 */
  permission_mode: "read_only" | "request_approval" | "auto_review" | "full_access" | "custom" | string;
  /** 由当前已登录 CLI 的 `codex debug models` 返回。 */
  models: CodexModelOption[];
  config_path: string;
}

export interface RtkStatus {
  /** True only when the R-Code policy is enabled and a verified RTK binary is available. */
  enabled: boolean;
  available: boolean;
  /** Managed binaries live under R-Code AppData; system binaries are preserved in place. */
  managed: boolean;
  version: string | null;
  source: "managed" | "system" | string | null;
  platform: string;
}

export interface AppConfig {
  default_provider?: string;
  log_level?: string;
  providers?: Record<string, ProviderConfig>;
  mcp_servers?: Record<string, unknown>;
  storage?: Record<string, unknown>;
  compaction?: Record<string, unknown>;
  orchestration?: OrchestrationConfig;
  /** 用户级协作提示，保存在 R-Code AppData，不进入任何项目。 */
  agent_prompts?: AgentPromptConfig;
  tauri?: Record<string, unknown>;
  [key: string]: unknown;
}

export interface ReviewPathStatus {
  path: string;
  /** Current-run changes are actionable; other Git worktree changes are context-only. */
  scope?: "task" | "workspace";
  change_type?: ChangeType | null;
  accepted: boolean;
  rejected: boolean;
  remaining: boolean;
  conflict: boolean;
  safe_to_accept: boolean;
  blocker?: string | null;
  accepted_items: number;
  rejected_items: number;
  remaining_items: number;
}

export interface ReviewStatus {
  git_repository: boolean;
  repo_root?: string | null;
  paths: ReviewPathStatus[];
  accepted_count: number;
  rejected_count: number;
  remaining_count: number;
  conflict_count: number;
  can_accept_all: boolean;
}

/** @deprecated command compatibility name; review state is application-owned, not Git state. */
export type ReviewGitStatus = ReviewStatus;

export interface ReviewAcceptResult {
  path?: string | null;
  accepted_count: number;
  rejected_count: number;
  remaining_count: number;
  fully_accepted: boolean;
}

export interface GitDeliveryStatus {
  branch?: string | null;
  upstream?: string | null;
  ahead: number;
  behind: number;
  staged_task_paths: string[];
  staged_other_paths: string[];
  can_stage: boolean;
  can_commit: boolean;
  can_push: boolean;
  blockers: string[];
}

export interface GitCommitResult {
  sha: string;
  message: string;
}

export interface GitPushResult {
  sha: string;
  branch: string;
  upstream: string;
}

export type WorkflowSkillSource = "builtin" | "custom";

export interface WorkflowSkill {
  id: string;
  name: string;
  description: string;
  instructions: string;
  source: WorkflowSkillSource;
  enabled: boolean;
  overridden: boolean;
}

export interface WorkflowSkillDraft {
  id?: string | null;
  name: string;
  description: string;
  instructions: string;
  source: WorkflowSkillSource;
  enabled: boolean;
}

export interface AgentPromptConfig {
  main_agent: string;
  subagent: string;
}

export interface OrchestrationConfig {
  default_agent_engine: TaskAgentEngine;
  delegation_router: "manual" | "balanced" | "r_code_first" | "codex_first";
  allow_cross_engine_delegation: boolean;
  quality_loop: "off" | "auto" | "always";
  quality_reviewer: "auto" | "r_code" | "codex";
  max_review_rounds: number;
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
  logs: { level: string; message: string; timestamp: string; target: string }[];
  config_summary: Record<string, unknown>;
  db_stats: { task_count: number; run_count: number; tool_call_count: number };
}
