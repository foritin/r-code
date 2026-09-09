//! Typed host-service request/reply envelopes.
//!
//! These DTOs are the *only* shapes that cross the plugin wire. They are
//! provider-neutral: model selections are opaque strings, credentials never
//! appear (the host resolves them internally), and large content travels via
//! versioned [`ArtifactRef`]s instead of inline frames.

use serde::{Deserialize, Serialize};

/// Versioned, content-addressed reference to large or binary content stored
/// by the host. `schema` versions the reference shape itself so old refs stay
/// readable across protocol growth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub schema: u16,
    pub blob_id: String,
    pub bytes: u64,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
}

impl ArtifactRef {
    pub const SCHEMA: u16 = 1;
}

/// Identity of the run a connection is bound to. Handles, streams and
/// artifacts are scoped to this identity and must never cross it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RunIdentity {
    pub task_id: String,
    pub branch_id: String,
    pub run_id: String,
    pub attempt_id: String,
    /// Monotonic generation; cancellation revokes it and late callbacks that
    /// carry a stale generation are rejected.
    pub generation: u64,
}

// ---------------------------------------------------------------------------
// Host -> plugin lifecycle params
// ---------------------------------------------------------------------------

/// Transport-level limits negotiated during initialize.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolLimits {
    pub max_frame_bytes: usize,
    pub max_queue_bytes: usize,
    pub initialize_timeout_ms: u64,
    pub cancel_grace_ms: u64,
}

impl Default for ProtocolLimits {
    fn default() -> Self {
        Self {
            max_frame_bytes: crate::rpc::MAX_FRAME_BYTES,
            max_queue_bytes: crate::rpc::MAX_QUEUE_BYTES,
            initialize_timeout_ms: crate::rpc::INITIALIZE_TIMEOUT.as_millis() as u64,
            cancel_grace_ms: crate::rpc::CANCEL_GRACE.as_millis() as u64,
        }
    }
}

/// `initialize` params: the host introduces itself and the run identity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitializeParams {
    pub protocol: String,
    pub host_api: crate::manifest::ApiVersion,
    pub identity: RunIdentity,
    pub granted_services: Vec<crate::manifest::HostService>,
    #[serde(default)]
    pub harness_config: serde_json::Value,
    pub limits: ProtocolLimits,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InitializeResult {
    #[serde(rename = "harnessId")]
    pub harness_id: String,
    #[serde(rename = "harnessVersion")]
    pub harness_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_checkpoint: Option<ArtifactRef>,
}

/// `harness.start` params: begin a fresh attempt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarnessStartParams {
    pub identity: RunIdentity,
    /// TaskContract wire form; owned by the kernel domain model.
    pub contract: serde_json::Value,
    /// Canonical transcript state to resume from, as context references.
    #[serde(default)]
    pub transcript_cursor: u64,
}

/// `harness.resume` params: continue from a host-stored checkpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarnessResumeParams {
    pub identity: RunIdentity,
    /// Opaque plugin state previously saved via `host.checkpoint.save`.
    pub checkpoint: ArtifactRef,
    /// The checkpoint's raw bytes, embedded by the host when within the
    /// frame budget (checkpoints are quota-bounded).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state_base64: Option<String>,
    /// Inputs delivered after the checkpointed `consumed_input_seq`.
    #[serde(default)]
    pub replay_inputs: Vec<InputMessage>,
}

/// A user input with host-generated identity and ordering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputMessage {
    pub message_id: String,
    pub input_seq: u64,
    pub kind: InputKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InputKind {
    User,
    Steer,
    Answer,
    Continuation,
}

/// `harness.steer` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessSteerParams {
    pub identity: RunIdentity,
    pub input: InputMessage,
}

/// `harness.cancel` params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessCancelParams {
    pub identity: RunIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessCancelResult {
    /// True when the plugin acknowledged within the grace period.
    pub acknowledged: bool,
}

// ---------------------------------------------------------------------------
// Model streaming (host.model.stream + stream.event notifications)
// ---------------------------------------------------------------------------

/// Role of a model message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelRole {
    System,
    User,
    Assistant,
    Tool,
}

/// One block of message content.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    /// Images and other binary content travel as artifact references.
    Image {
        artifact: ArtifactRef,
    },
    ToolCall {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    ToolResult {
        call_id: String,
        output: Vec<OutputBlock>,
    },
}

/// Output-only blocks (never contain tool calls).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum OutputBlock {
    Text { text: String },
    Image { artifact: ArtifactRef },
    Json { value: serde_json::Value },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelMessage {
    pub role: ModelRole,
    pub content: Vec<ContentBlock>,
}

/// A tool as advertised to models and plugins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// Token/cost accounting; missing facts stay `None` (unavailable, never
/// fabricated).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ModelUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_micros: Option<u128>,
}

/// `host.model.stream` request. `selection` is an opaque provider/model id
/// resolved host-side; credentials never cross the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelStreamRequest {
    /// Opaque model selection (provider id + model name resolved by host).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selection: Option<String>,
    pub messages: Vec<ModelMessage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDescriptor>,
    /// Provider capability/inference settings (temperature, limits...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inference: Option<serde_json::Value>,
    /// Deadline for the whole stream; cancellation is separate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelStreamReply {
    pub stream_id: String,
}

/// Correlated notification chunk for model and process streams.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamEvent {
    pub stream_id: String,
    pub sequence: u64,
    #[serde(flatten)]
    pub payload: StreamPayload,
    /// Present (and true) on the final chunk.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum StreamPayload {
    TextDelta {
        text: String,
    },
    ToolCallDelta {
        id: String,
        name: String,
        partial_input: String,
    },
    Usage {
        usage: ModelUsage,
    },
    Finish {
        reason: String,
        usage: ModelUsage,
    },
    ProcessStdout {
        data_base64: String,
    },
    ProcessStderr {
        data_base64: String,
    },
    ProcessExit {
        code: Option<i32>,
    },
    Failed {
        message: String,
    },
}

// ---------------------------------------------------------------------------
// Tools (host.tools.list / call)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolsListRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolsListReply {
    pub tools: Vec<ToolDescriptor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallRequest {
    pub tool: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCallReply {
    pub output: Vec<OutputBlock>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ToolCallError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denied_by: Option<String>,
}

// ---------------------------------------------------------------------------
// Managed processes (host.process.open / write / close)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessOpenRequest {
    /// Name of a process profile pinned by the plugin package.
    pub profile: String,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Non-secret environment only; secrets resolve host-side by reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<std::collections::BTreeMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessOpenReply {
    /// Run-scoped handle; rejected when used on another run.
    pub handle: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessWriteRequest {
    pub handle: String,
    pub data_base64: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessCloseRequest {
    pub handle: String,
    /// Wait for exit confirmation before returning.
    #[serde(default)]
    pub wait: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessCloseReply {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

// ---------------------------------------------------------------------------
// Context and artifacts
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextReadRequest {
    /// Which projection to read (e.g. "transcript", "instructions", "skills").
    pub projection: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextPage {
    pub entries: Vec<TranscriptEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<u64>,
}

/// One canonical transcript entry. Complete tool pairs are preserved: a
/// tool-call entry is always followed by its result entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscriptEntry {
    pub seq: u64,
    pub role: ModelRole,
    #[serde(default)]
    pub blocks: Vec<ContentBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactsPutRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// Base64 content; must fit the frame budget. Larger content is chunked
    /// by the SDK or referenced by the host.
    pub data_base64: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactsReadRequest {
    pub artifact: ArtifactRef,
    #[serde(default)]
    pub offset: u64,
    #[serde(default)]
    pub length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactsReadReply {
    pub data_base64: String,
    pub total_bytes: u64,
}

// ---------------------------------------------------------------------------
// Plan, questions, approvals, children, verification, completion
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkUnitWire {
    pub id: String,
    pub description: String,
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// Check-definition ids this unit is accepted by.
    #[serde(default)]
    pub acceptance: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanPublishRequest {
    pub revision: u64,
    pub work_units: Vec<WorkUnitWire>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanUpdateRequest {
    pub revision: u64,
    pub updates: Vec<WorkUnitStatusUpdate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkUnitStatusUpdate {
    pub work_unit_id: String,
    pub status: WorkUnitWireStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WorkUnitWireStatus {
    Pending,
    InProgress,
    Completed,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionsAskRequest {
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// Blocking questions suspend the run until answered.
    #[serde(default)]
    pub blocking: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionsAskReply {
    pub question_id: String,
}

/// Approval requests may only reference a pending operation the host itself
/// created; generic questions cannot mint permission grants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalsRequest {
    /// Host-owned pending-operation reference (see [`crate::operations`]).
    pub pending_operation: crate::operations::PendingOperationRef,
    #[serde(default)]
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalDecision {
    Granted,
    Denied,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalsReply {
    pub decision: ApprovalDecision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildrenSpawnRequest {
    pub objective: String,
    /// Optional different harness for the child.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub harness: Option<String>,
    /// Permission ceiling, bounded by the parent's own permissions.
    pub permissions: PermissionCeiling,
    /// Budget share requested from the parent's root budget.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget_share: Option<serde_json::Value>,
}

/// Child permission ceilings; the host intersects with parent permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionCeiling {
    ReadOnly,
    ApprovalRequired,
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildrenSpawnReply {
    pub child_task_id: String,
    pub child_run_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildrenWaitRequest {
    pub child_task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChildReport {
    pub child_task_id: String,
    pub outcome: String,
    #[serde(default)]
    pub verified: Vec<String>,
    #[serde(default)]
    pub inferred: Vec<String>,
    #[serde(default)]
    pub unverifiable: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildrenCancelRequest {
    pub child_task_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationRunRequest {
    /// Check-definition ids to run, or all required checks when empty.
    #[serde(default)]
    pub check_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationRunReply {
    pub verification_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointSaveRequest {
    /// Opaque plugin state (bounded by the frame budget).
    pub state_base64: String,
    /// Contiguous prefix of delivered inputs the state already includes.
    pub consumed_input_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointSaveReply {
    pub checkpoint: ArtifactRef,
    pub revision: u64,
}

/// Completion proposal kinds. Replies and plans may finish without code
/// checks; implementation proposals require host-owned evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProposalKind {
    Implementation,
    Reply,
    PlanDraft,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompletionProposalRequest {
    pub kind: ProposalKind,
    pub summary: String,
    /// Required for implementation proposals; the host verifies freshness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_digest: Option<String>,
    #[serde(default)]
    pub work_unit_statuses: Vec<WorkUnitStatusUpdate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletionProposalReply {
    /// Host decision; the plugin never sets the authoritative terminal state.
    pub accepted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repair_feedback: Option<String>,
}
