//! Read-only task status projection for list, dashboard, and detail surfaces.
//!
//! [`TaskState`](crate::dto::TaskState) remains the persisted lifecycle state. This module
//! combines that durable state with live run, queue, approval, question, verification, and
//! workspace observations into a display-only view. Keeping the projection pure prevents UI
//! surfaces from growing subtly different precedence rules.

use serde::{Deserialize, Serialize};

use crate::dto::{
    AgentKind, AgentRun, PermissionDecision, PermissionRequest, QueuedMessage, QueuedMessageState,
    ReviewState, Task, TaskState, VerificationRecord, VerificationStatus,
};

/// The single state a task surface should render after applying the public precedence contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskDisplayState {
    /// Archived tasks stay archived even if stale live observations remain after recovery.
    Archived,
    /// A tool invocation is waiting for an approval decision.
    WaitingForApproval,
    /// The agent is waiting for structured user input.
    WaitingForQuestion,
    /// The current run, queue dispatch, or current verification failed.
    Failed,
    /// The persisted task lifecycle was interrupted.
    Interrupted,
    /// The persisted workspace binding can no longer be resolved safely.
    WorkspaceBindingInvalid,
    /// The current result is ready for user review.
    ReviewReady,
    /// The latest verification was invalidated and must be run again.
    VerificationRequired,
    /// A verification command is currently running.
    Verifying,
    /// A task or one of its runs is active.
    Running,
    /// Work exists in the task queue but is not otherwise active.
    Queued,
    /// No higher-priority observation applies.
    Idle,
}

/// The actionable reason, if any, that accompanies a task display state.
///
/// `unread_count` is intentionally not represented here: unread activity is additive metadata,
/// not a reason to change task state or manufacture a user action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskAttention {
    ApprovalRequired,
    UserQuestion,
    WorkspaceBindingInvalid,
    RunFailed,
    VerificationRequired,
    ReviewRequired,
}

/// Stable, display-only task status returned alongside the persisted [`Task`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskStatusView {
    pub task_id: String,
    /// A copy of the durable lifecycle state; the projector never rewrites it.
    pub persisted_state: TaskState,
    pub display_state: TaskDisplayState,
    /// All current attention signals in stable contract order. Display precedence never removes
    /// a lower-priority coexisting signal from this list.
    #[serde(default)]
    pub attention: Vec<TaskAttention>,
    /// The authoritative active main run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_run_id: Option<String>,
    /// Number of queued or dispatching messages. Failed/terminal queue rows are excluded.
    pub queue_depth: u32,
    /// Unread notifications for this task. This never participates in display precedence.
    pub unread_count: u64,
}

/// Existing observations consumed by [`project_task_status`].
///
/// The host owns I/O and supplies the observations; the projector performs no storage, runtime,
/// or filesystem access and is therefore reusable by list, dashboard, and detail projections.
#[derive(Debug, Clone, Copy)]
pub struct TaskStatusProjectionInput<'a> {
    pub task: &'a Task,
    pub runs: &'a [AgentRun],
    pub queued_messages: &'a [QueuedMessage],
    pub approvals: &'a [PermissionRequest],
    /// Number of unresolved question cards from any supported runtime or Plan source.
    pub pending_question_count: u32,
    /// Most recent verification for the task, if one exists.
    pub latest_verification: Option<&'a VerificationRecord>,
    /// Set by the host when the persisted workspace binding cannot be resolved fail-closed.
    pub workspace_binding_invalid: bool,
    /// Additive notification metadata; it must not influence the selected display state.
    pub unread_count: u64,
}

/// Project existing task observations into one deterministic public status view.
///
/// Precedence is fixed and intentionally spelled out in control-flow order:
/// archived → approval/question → failed/interrupted/workspace binding invalid →
/// review/verification → running → queued → idle. Within a slash-delimited tier, the written
/// left-to-right order is deterministic as well.
pub fn project_task_status(input: TaskStatusProjectionInput<'_>) -> TaskStatusView {
    let pending_approval = input
        .approvals
        .iter()
        .any(|request| request.decision == PermissionDecision::Pending);
    let latest_main_run = input
        .runs
        .iter()
        .filter(|run| run.agent_kind == AgentKind::Main)
        .max_by_key(|run| run.started_at);
    let active_run = input
        .runs
        .iter()
        .filter(|run| run.agent_kind == AgentKind::Main && run.is_active())
        .max_by_key(|run| run.started_at);
    let relevant_verification = input.latest_verification.filter(|verification| {
        latest_main_run.is_none_or(|run| {
            verification.run_id == run.id || verification.started_at >= run.started_at
        })
    });
    let verification_failed = relevant_verification.is_some_and(|verification| {
        matches!(
            verification.status,
            VerificationStatus::Failed | VerificationStatus::Timeout
        )
    });
    let run_failed = latest_main_run.is_some_and(|run| run.review_state == ReviewState::Failed)
        || input
            .queued_messages
            .iter()
            .any(|message| message.state == QueuedMessageState::Failed)
        || verification_failed;
    let verification_required = relevant_verification.is_some_and(|verification| {
        matches!(
            verification.status,
            VerificationStatus::Superseded | VerificationStatus::Stale
        )
    });
    let verifying = relevant_verification
        .is_some_and(|verification| verification.status == VerificationStatus::Running);
    let running = matches!(
        input.task.state,
        TaskState::Exploring | TaskState::InProgress
    ) || input.runs.iter().any(AgentRun::is_active);
    let queue_depth = input
        .queued_messages
        .iter()
        .filter(|message| {
            matches!(
                message.state,
                QueuedMessageState::Queued | QueuedMessageState::Dispatching
            )
        })
        .count();
    let queue_depth = u32::try_from(queue_depth).unwrap_or(u32::MAX);
    let queued = queue_depth > 0;

    let mut attention = Vec::new();
    if pending_approval {
        attention.push(TaskAttention::ApprovalRequired);
    }
    if input.pending_question_count > 0 {
        attention.push(TaskAttention::UserQuestion);
    }
    if input.workspace_binding_invalid {
        attention.push(TaskAttention::WorkspaceBindingInvalid);
    }
    if run_failed {
        attention.push(TaskAttention::RunFailed);
    }
    if verification_required {
        attention.push(TaskAttention::VerificationRequired);
    }
    if input.task.state == TaskState::ReviewReady {
        attention.push(TaskAttention::ReviewRequired);
    }

    let display_state = if input.task.state == TaskState::Archived {
        TaskDisplayState::Archived
    } else if pending_approval {
        TaskDisplayState::WaitingForApproval
    } else if input.pending_question_count > 0 {
        TaskDisplayState::WaitingForQuestion
    } else if run_failed {
        TaskDisplayState::Failed
    } else if input.task.state == TaskState::Interrupted {
        TaskDisplayState::Interrupted
    } else if input.workspace_binding_invalid {
        TaskDisplayState::WorkspaceBindingInvalid
    } else if input.task.state == TaskState::ReviewReady {
        TaskDisplayState::ReviewReady
    } else if verification_required {
        TaskDisplayState::VerificationRequired
    } else if verifying {
        TaskDisplayState::Verifying
    } else if running {
        TaskDisplayState::Running
    } else if queued {
        TaskDisplayState::Queued
    } else {
        TaskDisplayState::Idle
    };

    TaskStatusView {
        task_id: input.task.id.clone(),
        persisted_state: input.task.state,
        display_state,
        attention,
        active_run_id: active_run.map(|run| run.id.clone()),
        queue_depth,
        unread_count: input.unread_count,
    }
}
