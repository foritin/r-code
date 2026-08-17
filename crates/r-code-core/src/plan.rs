//! Durable Plan, human-in-the-loop, and enhanced-review contracts.
//!
//! Persisted enum spellings in this module are part of the SQLite and IPC contract. Keep their
//! `as_str`, parser, and serde representations in sync.

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::dto::TaskMode;

macro_rules! stable_string_enum {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $value,)+
                }
            }

            pub fn try_from_str(value: &str) -> Option<Self> {
                match value {
                    $($value => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

/// Lifecycle of one durable plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanState {
    #[default]
    Draft,
    AwaitingInput,
    Ready,
    Approved,
    Executing,
    Completed,
    Cancelled,
}

stable_string_enum!(PlanState {
    Draft => "draft",
    AwaitingInput => "awaiting_input",
    Ready => "ready",
    Approved => "approved",
    Executing => "executing",
    Completed => "completed",
    Cancelled => "cancelled",
});

/// Lifecycle of a feature item. Proposed items become pending when their revision is approved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanItemState {
    #[default]
    Proposed,
    Pending,
    InProgress,
    Blocked,
    Completed,
    Failed,
    Cancelled,
}

stable_string_enum!(PlanItemState {
    Proposed => "proposed",
    Pending => "pending",
    InProgress => "in_progress",
    Blocked => "blocked",
    Completed => "completed",
    Failed => "failed",
    Cancelled => "cancelled",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanQuestionSetState {
    #[default]
    Pending,
    Answered,
    Skipped,
}

stable_string_enum!(PlanQuestionSetState {
    Pending => "pending",
    Answered => "answered",
    Skipped => "skipped",
});

/// Distinguishes ordinary Plan clarification from a scope decision raised while the task is
/// still in Agent mode. Scope decisions remember the Agent mode to restore after the user answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanQuestionSetKind {
    #[default]
    Plan,
    ScopeDecision,
}

stable_string_enum!(PlanQuestionSetKind {
    Plan => "plan",
    ScopeDecision => "scope_decision",
});

/// Durable dispatch state makes answering/skipping idempotent across process restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanContinuationState {
    #[default]
    NotRequested,
    Pending,
    Dispatching,
    Dispatched,
    Failed,
}

stable_string_enum!(PlanContinuationState {
    NotRequested => "not_requested",
    Pending => "pending",
    Dispatching => "dispatching",
    Dispatched => "dispatched",
    Failed => "failed",
});

/// Durable handoff state for the first implementation message of an approved Plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanImplementationDispatchState {
    #[default]
    NotRequested,
    Pending,
    Dispatching,
    Dispatched,
    Failed,
}

stable_string_enum!(PlanImplementationDispatchState {
    NotRequested => "not_requested",
    Pending => "pending",
    Dispatching => "dispatching",
    Dispatched => "dispatched",
    Failed => "failed",
});

/// Runtime safety state for an approved Plan. `paused` is fail-closed: an executing Plan without
/// an active feature may not mutate the workspace until the blocked feature is explicitly resumed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanExecutionStatus {
    #[default]
    NoExecutingPlan,
    ActiveFeature,
    Paused,
}

stable_string_enum!(PlanExecutionStatus {
    NoExecutingPlan => "no_executing_plan",
    ActiveFeature => "active_feature",
    Paused => "paused",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanChangeEventState {
    #[default]
    Pending,
    Captured,
    Failed,
}

stable_string_enum!(PlanChangeEventState {
    Pending => "pending",
    Captured => "captured",
    Failed => "failed",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanReviewScope {
    Feature,
    File,
}

stable_string_enum!(PlanReviewScope {
    Feature => "feature",
    File => "file",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanReviewDecisionKind {
    Accepted,
    Rejected,
}

stable_string_enum!(PlanReviewDecisionKind {
    Accepted => "accepted",
    Rejected => "rejected",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanRejectOperationState {
    #[default]
    Prepared,
    Applying,
    Committed,
    RollingBack,
    RolledBack,
    Conflict,
    Failed,
}

stable_string_enum!(PlanRejectOperationState {
    Prepared => "prepared",
    Applying => "applying",
    Committed => "committed",
    RollingBack => "rolling_back",
    RolledBack => "rolled_back",
    Conflict => "conflict",
    Failed => "failed",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanRejectFileState {
    #[default]
    Pending,
    Applied,
    RolledBack,
    Conflict,
}

stable_string_enum!(PlanRejectFileState {
    Pending => "pending",
    Applied => "applied",
    RolledBack => "rolled_back",
    Conflict => "conflict",
});

/// Stable Plan identity and optimistic-revision metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub id: String,
    pub task_id: String,
    pub revision: u64,
    pub state: PlanState,
    pub approved_revision: Option<u64>,
    pub projection_path: Option<String>,
    pub projection_revision: Option<u64>,
    pub projection_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub approved_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub implementation_dispatch_state: PlanImplementationDispatchState,
    pub implementation_dispatch_error: Option<String>,
    pub implementation_queue_message_id: Option<String>,
    pub implementation_dispatched_at: Option<DateTime<Utc>>,
}

/// A revisioned feature todo. Dependencies contain stable item IDs from the same Plan revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanItem {
    pub id: String,
    pub plan_id: String,
    pub revision: u64,
    pub ordinal: u32,
    pub title: String,
    pub description: String,
    /// Optional presentation hierarchy. Only leaf items participate in execution and review.
    #[serde(default)]
    pub section_path: Vec<String>,
    pub state: PlanItemState,
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanQuestionOption {
    pub id: String,
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanQuestionAnswer {
    Option { option_id: String },
    FreeForm { text: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanQuestion {
    pub id: String,
    pub question_set_id: String,
    pub ordinal: u32,
    pub header: String,
    pub question: String,
    #[serde(default)]
    pub options: Vec<PlanQuestionOption>,
    pub answer: Option<PlanQuestionAnswer>,
    pub answered_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanQuestionSet {
    pub id: String,
    pub plan_id: String,
    pub revision: u64,
    pub state: PlanQuestionSetState,
    /// Distinguishes ordinary Plan clarification from a scope decision raised in Agent mode.
    #[serde(default)]
    pub kind: PlanQuestionSetKind,
    /// For scope decisions, the Agent mode to restore after the user answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore_mode: Option<TaskMode>,
    /// Stable client submission identity used to make answer/skip retry-safe across restarts.
    pub answer_idempotency_key: Option<String>,
    pub continuation_state: PlanContinuationState,
    pub continuation_error: Option<String>,
    #[serde(default)]
    pub questions: Vec<PlanQuestion>,
    pub created_at: DateTime<Utc>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub dispatched_at: Option<DateTime<Utc>>,
}

/// The Plan workflow deliberately reads and updates `tasks.goal`; this is only its view DTO.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanGoal {
    pub task_id: String,
    pub goal: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanView {
    pub plan: Plan,
    pub goal: PlanGoal,
    #[serde(default)]
    pub items: Vec<PlanItem>,
    pub pending_question_set: Option<PlanQuestionSet>,
    /// Latest resolved question set whose continuation still needs dispatch/retry handling.
    /// Dispatched sets are omitted so this remains a compact actionable projection.
    #[serde(default)]
    pub continuation_question_set: Option<PlanQuestionSet>,
}

/// Small, stable projection consumed by host prompts and write guards.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanExecutionContext {
    pub status: PlanExecutionStatus,
    pub plan_id: Option<String>,
    pub plan_revision: Option<u64>,
    pub active_feature: Option<PlanItem>,
    pub blocked_feature: Option<PlanItem>,
}

impl PlanExecutionContext {
    pub fn from_view(view: Option<&PlanView>) -> Self {
        let Some(view) = view.filter(|view| view.plan.state == PlanState::Executing) else {
            return Self {
                status: PlanExecutionStatus::NoExecutingPlan,
                plan_id: None,
                plan_revision: None,
                active_feature: None,
                blocked_feature: None,
            };
        };
        let active_feature = view
            .items
            .iter()
            .find(|item| item.state == PlanItemState::InProgress)
            .cloned();
        let blocked_feature = view
            .items
            .iter()
            .find(|item| item.state == PlanItemState::Blocked)
            .cloned();
        Self {
            status: if active_feature.is_some() {
                PlanExecutionStatus::ActiveFeature
            } else {
                PlanExecutionStatus::Paused
            },
            plan_id: Some(view.plan.id.clone()),
            plan_revision: Some(view.plan.revision),
            active_feature,
            blocked_feature,
        }
    }
}

/// Creates one durable Plan for a task. The store generates the Plan identity and projection path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatePlanInput {
    pub task_id: String,
}

/// One feature proposed by a Plan publication. IDs remain stable while the draft is revised.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanItemDraft {
    pub id: String,
    pub title: String,
    pub description: String,
    /// Optional hierarchy labels such as `["Backend", "Validation"]`.
    #[serde(default)]
    pub section_path: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
}

/// Replaces the current draft feature set using an optimistic Plan revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishPlanInput {
    pub plan_id: String,
    pub expected_revision: u64,
    pub items: Vec<PlanItemDraft>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanQuestionOptionDraft {
    pub id: String,
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanQuestionDraft {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<PlanQuestionOptionDraft>,
}

/// Persists one all-or-nothing human-in-the-loop question set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestPlanQuestionsInput {
    pub plan_id: String,
    pub expected_revision: u64,
    pub questions: Vec<PlanQuestionDraft>,
}

/// Persists a scope decision raised while the task is still in Agent mode. The store creates the
/// owning Plan aggregate, records the Agent mode to restore, and switches the task to Plan mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestScopeDecisionInput {
    pub questions: Vec<PlanQuestionDraft>,
}

/// Exactly one response for one question. `text` is the free-form "other" path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanQuestionAnswerInput {
    Option {
        question_id: String,
        option_id: String,
    },
    Text {
        question_id: String,
        text: String,
    },
}

impl PlanQuestionAnswerInput {
    pub fn question_id(&self) -> &str {
        match self {
            Self::Option { question_id, .. } | Self::Text { question_id, .. } => question_id,
        }
    }
}

/// Resolves a question set once. A whole-set skip cannot carry individual answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerPlanQuestionsInput {
    pub question_set_id: String,
    pub expected_revision: u64,
    pub idempotency_key: String,
    pub skip_all: bool,
    #[serde(default)]
    pub answers: Vec<PlanQuestionAnswerInput>,
}

/// Pins and starts exactly one published Plan revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovePlanInput {
    pub plan_id: String,
    pub expected_revision: u64,
}

/// Cancels one non-terminal Plan using optimistic concurrency. Cancellation preserves completed
/// work and enhanced-review history; it only terminates the remaining workflow state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelPlanInput {
    pub plan_id: String,
    pub expected_revision: u64,
}

/// Advances one approved feature while guarding against a stale Plan aggregate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatePlanItemInput {
    pub plan_id: String,
    pub item_id: String,
    pub expected_revision: u64,
    pub state: PlanItemState,
}

/// One trusted write-tool observation owned by an approved feature item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanChangeEvent {
    pub sequence: i64,
    pub id: String,
    pub plan_id: String,
    pub plan_revision: u64,
    pub item_id: String,
    pub task_id: String,
    pub run_id: String,
    pub tool_call_id: String,
    pub path: String,
    pub before_blob_hash: Option<String>,
    pub before_exists: bool,
    pub after_blob_hash: Option<String>,
    pub after_exists: Option<bool>,
    pub state: PlanChangeEventState,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub finalized_at: Option<DateTime<Utc>>,
}

/// A ledger-only review decision. Files use `path`; feature decisions leave it empty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanReviewDecision {
    pub id: String,
    pub plan_id: String,
    pub plan_revision: u64,
    pub item_id: String,
    pub scope: PlanReviewScope,
    pub path: Option<String>,
    pub decision: PlanReviewDecisionKind,
    pub decided_at: DateTime<Utc>,
}

/// Durable rejection journal header. Non-terminal rows are recovered on startup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanRejectOperation {
    pub id: String,
    pub plan_id: String,
    pub plan_revision: u64,
    pub item_id: String,
    pub scope: PlanReviewScope,
    pub path: Option<String>,
    pub state: PlanRejectOperationState,
    pub recovery_count: u32,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

/// Per-path rollback and desired snapshots written before a rejection starts mutating files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanRejectOperationFile {
    pub operation_id: String,
    pub ordinal: u32,
    pub path: String,
    pub expected_current_hash: Option<String>,
    pub expected_exists: bool,
    pub rollback_hash: Option<String>,
    pub rollback_exists: bool,
    pub desired_hash: Option<String>,
    pub desired_exists: bool,
    pub state: PlanRejectFileState,
    pub error: Option<String>,
    pub applied_at: Option<DateTime<Utc>>,
    pub rolled_back_at: Option<DateTime<Utc>>,
}
