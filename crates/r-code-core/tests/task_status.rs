use chrono::{DateTime, Utc};
use r_code_core::dto::{
    AgentKind, AgentRun, PermissionRequest, QueuedMessage, QueuedMessageState, ReviewState,
    RiskLevel, Task, TaskMode, TaskState, VerificationRecord, VerificationStatus,
};
use r_code_core::{
    project_task_status, TaskAttention, TaskDisplayState, TaskStatusProjectionInput, TaskStatusView,
};

fn timestamp(seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(seconds, 0).expect("valid test timestamp")
}

fn task(state: TaskState) -> Task {
    let mut task = Task::new(None, "status", "status", TaskMode::Edit);
    task.id = "status-task".to_string();
    task.state = state;
    task
}

fn run(
    id: &str,
    kind: AgentKind,
    started_at: i64,
    active: bool,
    review_state: ReviewState,
) -> AgentRun {
    let mut run = AgentRun::new("status-task", "test-model");
    run.id = id.to_string();
    run.agent_kind = kind;
    run.started_at = timestamp(started_at);
    run.review_state = review_state;
    run.ended_at = (!active).then(|| timestamp(started_at + 1));
    run
}

fn queued(state: QueuedMessageState) -> QueuedMessage {
    let mut message = QueuedMessage::new("status-task", "main", "queued", 0);
    message.state = state;
    message
}

fn approval() -> PermissionRequest {
    PermissionRequest::new(
        "status-task",
        "tool-call",
        "write_file",
        RiskLevel::R2,
        "write fixture",
    )
}

fn verification(run_id: &str, status: VerificationStatus, started_at: i64) -> VerificationRecord {
    let mut verification = VerificationRecord::new("status-task", run_id, "cargo test");
    verification.status = status;
    verification.started_at = timestamp(started_at);
    verification
}

struct DisplayCase {
    name: &'static str,
    task_state: TaskState,
    pending_approval: bool,
    pending_questions: u32,
    failed_run: bool,
    binding_invalid: bool,
    verification: Option<VerificationStatus>,
    active_run: bool,
    queued: bool,
    expected: TaskDisplayState,
}

#[test]
fn display_state_follows_the_frozen_precedence_table() {
    let cases = [
        DisplayCase {
            name: "archived overrides stale live observations",
            task_state: TaskState::Archived,
            pending_approval: true,
            pending_questions: 1,
            failed_run: true,
            binding_invalid: true,
            verification: Some(VerificationStatus::Stale),
            active_run: false,
            queued: true,
            expected: TaskDisplayState::Archived,
        },
        DisplayCase {
            name: "approval precedes question",
            task_state: TaskState::Idle,
            pending_approval: true,
            pending_questions: 1,
            failed_run: false,
            binding_invalid: false,
            verification: None,
            active_run: true,
            queued: true,
            expected: TaskDisplayState::WaitingForApproval,
        },
        DisplayCase {
            name: "question precedes failure",
            task_state: TaskState::Idle,
            pending_approval: false,
            pending_questions: 1,
            failed_run: true,
            binding_invalid: false,
            verification: None,
            active_run: false,
            queued: true,
            expected: TaskDisplayState::WaitingForQuestion,
        },
        DisplayCase {
            name: "failure precedes interrupted",
            task_state: TaskState::Interrupted,
            pending_approval: false,
            pending_questions: 0,
            failed_run: true,
            binding_invalid: true,
            verification: None,
            active_run: false,
            queued: false,
            expected: TaskDisplayState::Failed,
        },
        DisplayCase {
            name: "interrupted precedes invalid binding",
            task_state: TaskState::Interrupted,
            pending_approval: false,
            pending_questions: 0,
            failed_run: false,
            binding_invalid: true,
            verification: Some(VerificationStatus::Stale),
            active_run: false,
            queued: false,
            expected: TaskDisplayState::Interrupted,
        },
        DisplayCase {
            name: "invalid binding precedes review",
            task_state: TaskState::ReviewReady,
            pending_approval: false,
            pending_questions: 0,
            failed_run: false,
            binding_invalid: true,
            verification: Some(VerificationStatus::Stale),
            active_run: false,
            queued: false,
            expected: TaskDisplayState::WorkspaceBindingInvalid,
        },
        DisplayCase {
            name: "review precedes verification",
            task_state: TaskState::ReviewReady,
            pending_approval: false,
            pending_questions: 0,
            failed_run: false,
            binding_invalid: false,
            verification: Some(VerificationStatus::Stale),
            active_run: false,
            queued: false,
            expected: TaskDisplayState::ReviewReady,
        },
        DisplayCase {
            name: "stale verification requires verification",
            task_state: TaskState::Idle,
            pending_approval: false,
            pending_questions: 0,
            failed_run: false,
            binding_invalid: false,
            verification: Some(VerificationStatus::Stale),
            active_run: false,
            queued: true,
            expected: TaskDisplayState::VerificationRequired,
        },
        DisplayCase {
            name: "running verification precedes active run",
            task_state: TaskState::Idle,
            pending_approval: false,
            pending_questions: 0,
            failed_run: false,
            binding_invalid: false,
            verification: Some(VerificationStatus::Running),
            active_run: true,
            queued: true,
            expected: TaskDisplayState::Verifying,
        },
        DisplayCase {
            name: "active run precedes queue",
            task_state: TaskState::Idle,
            pending_approval: false,
            pending_questions: 0,
            failed_run: false,
            binding_invalid: false,
            verification: None,
            active_run: true,
            queued: true,
            expected: TaskDisplayState::Running,
        },
        DisplayCase {
            name: "persisted exploring state is running",
            task_state: TaskState::Exploring,
            pending_approval: false,
            pending_questions: 0,
            failed_run: false,
            binding_invalid: false,
            verification: None,
            active_run: false,
            queued: false,
            expected: TaskDisplayState::Running,
        },
        DisplayCase {
            name: "persisted in-progress state is running",
            task_state: TaskState::InProgress,
            pending_approval: false,
            pending_questions: 0,
            failed_run: false,
            binding_invalid: false,
            verification: None,
            active_run: false,
            queued: false,
            expected: TaskDisplayState::Running,
        },
        DisplayCase {
            name: "queue is visible when otherwise idle",
            task_state: TaskState::Idle,
            pending_approval: false,
            pending_questions: 0,
            failed_run: false,
            binding_invalid: false,
            verification: None,
            active_run: false,
            queued: true,
            expected: TaskDisplayState::Queued,
        },
        DisplayCase {
            name: "idle is the final fallback",
            task_state: TaskState::Idle,
            pending_approval: false,
            pending_questions: 0,
            failed_run: false,
            binding_invalid: false,
            verification: None,
            active_run: false,
            queued: false,
            expected: TaskDisplayState::Idle,
        },
    ];

    for case in cases {
        let task = task(case.task_state);
        let runs = if case.failed_run {
            vec![run(
                "main-run",
                AgentKind::Main,
                20,
                false,
                ReviewState::Failed,
            )]
        } else if case.active_run {
            vec![run(
                "main-run",
                AgentKind::Main,
                20,
                true,
                ReviewState::Pending,
            )]
        } else {
            Vec::new()
        };
        let queued_messages = case.queued.then(|| queued(QueuedMessageState::Queued));
        let queued_messages = queued_messages.into_iter().collect::<Vec<_>>();
        let approvals = case.pending_approval.then(approval);
        let approvals = approvals.into_iter().collect::<Vec<_>>();
        let verification = case.verification.map(|status| {
            verification(
                runs.first()
                    .map_or("verification-run", |run| run.id.as_str()),
                status,
                21,
            )
        });

        let projected = project_task_status(TaskStatusProjectionInput {
            task: &task,
            runs: &runs,
            queued_messages: &queued_messages,
            approvals: &approvals,
            pending_question_count: case.pending_questions,
            latest_verification: verification.as_ref(),
            workspace_binding_invalid: case.binding_invalid,
            unread_count: 0,
        });

        assert_eq!(projected.display_state, case.expected, "{}", case.name);
        assert_eq!(projected.persisted_state, case.task_state, "{}", case.name);
    }
}

#[test]
fn coexisting_attention_is_complete_and_stably_ordered() {
    let task = task(TaskState::ReviewReady);
    let runs = vec![run(
        "failed-main",
        AgentKind::Main,
        20,
        false,
        ReviewState::Failed,
    )];
    let queued_messages = vec![
        queued(QueuedMessageState::Queued),
        queued(QueuedMessageState::Dispatching),
    ];
    let approvals = vec![approval()];
    let verification = verification("failed-main", VerificationStatus::Stale, 21);

    let projected = project_task_status(TaskStatusProjectionInput {
        task: &task,
        runs: &runs,
        queued_messages: &queued_messages,
        approvals: &approvals,
        pending_question_count: 2,
        latest_verification: Some(&verification),
        workspace_binding_invalid: true,
        unread_count: 9,
    });

    assert_eq!(
        projected.display_state,
        TaskDisplayState::WaitingForApproval
    );
    assert_eq!(
        projected.attention,
        [
            TaskAttention::ApprovalRequired,
            TaskAttention::UserQuestion,
            TaskAttention::WorkspaceBindingInvalid,
            TaskAttention::RunFailed,
            TaskAttention::VerificationRequired,
            TaskAttention::ReviewRequired,
        ]
    );
    assert_eq!(projected.queue_depth, 2);
    assert_eq!(projected.unread_count, 9);
}

#[test]
fn active_run_id_is_reserved_for_the_newest_active_main_run() {
    let task = task(TaskState::Idle);
    let main = run("main-run", AgentKind::Main, 10, true, ReviewState::Pending);
    let subagent = run(
        "newer-subagent",
        AgentKind::Subagent,
        20,
        true,
        ReviewState::Pending,
    );

    let with_main = project_task_status(TaskStatusProjectionInput {
        task: &task,
        runs: &[main, subagent.clone()],
        queued_messages: &[],
        approvals: &[],
        pending_question_count: 0,
        latest_verification: None,
        workspace_binding_invalid: false,
        unread_count: 0,
    });
    assert_eq!(with_main.active_run_id.as_deref(), Some("main-run"));

    let subagent_only = project_task_status(TaskStatusProjectionInput {
        task: &task,
        runs: &[subagent],
        queued_messages: &[],
        approvals: &[],
        pending_question_count: 0,
        latest_verification: None,
        workspace_binding_invalid: false,
        unread_count: 0,
    });
    assert_eq!(subagent_only.display_state, TaskDisplayState::Running);
    assert_eq!(subagent_only.active_run_id, None);
}

#[test]
fn queue_depth_counts_only_queued_and_dispatching_rows() {
    let task = task(TaskState::Archived);
    let queued_messages = [
        queued(QueuedMessageState::Queued),
        queued(QueuedMessageState::Dispatching),
        queued(QueuedMessageState::Sent),
        queued(QueuedMessageState::Cancelled),
        queued(QueuedMessageState::Failed),
    ];

    let projected = project_task_status(TaskStatusProjectionInput {
        task: &task,
        runs: &[],
        queued_messages: &queued_messages,
        approvals: &[],
        pending_question_count: 0,
        latest_verification: None,
        workspace_binding_invalid: false,
        unread_count: 0,
    });

    assert_eq!(projected.display_state, TaskDisplayState::Archived);
    assert_eq!(projected.queue_depth, 2);
    assert_eq!(projected.attention, [TaskAttention::RunFailed]);
}

#[test]
fn unread_metadata_never_overrides_an_idle_display_state() {
    let task = task(TaskState::Idle);
    let projected = project_task_status(TaskStatusProjectionInput {
        task: &task,
        runs: &[],
        queued_messages: &[],
        approvals: &[],
        pending_question_count: 0,
        latest_verification: None,
        workspace_binding_invalid: false,
        unread_count: 42,
    });

    assert_eq!(projected.display_state, TaskDisplayState::Idle);
    assert!(projected.attention.is_empty());
    assert_eq!(projected.unread_count, 42);
}

#[test]
fn stale_verification_from_an_older_run_cannot_override_the_new_main_run() {
    let task = task(TaskState::Idle);
    let runs = [run(
        "new-main",
        AgentKind::Main,
        20,
        true,
        ReviewState::Pending,
    )];
    let old_verification = verification("old-main", VerificationStatus::Stale, 10);

    let projected = project_task_status(TaskStatusProjectionInput {
        task: &task,
        runs: &runs,
        queued_messages: &[],
        approvals: &[],
        pending_question_count: 0,
        latest_verification: Some(&old_verification),
        workspace_binding_invalid: false,
        unread_count: 0,
    });

    assert_eq!(projected.display_state, TaskDisplayState::Running);
    assert_eq!(projected.active_run_id.as_deref(), Some("new-main"));
    assert!(projected.attention.is_empty());
}

#[test]
fn serde_values_are_stable_and_optional_active_run_is_omitted() {
    let display_states = [
        (TaskDisplayState::Archived, "archived"),
        (TaskDisplayState::WaitingForApproval, "waiting_for_approval"),
        (TaskDisplayState::WaitingForQuestion, "waiting_for_question"),
        (TaskDisplayState::Failed, "failed"),
        (TaskDisplayState::Interrupted, "interrupted"),
        (
            TaskDisplayState::WorkspaceBindingInvalid,
            "workspace_binding_invalid",
        ),
        (TaskDisplayState::ReviewReady, "review_ready"),
        (
            TaskDisplayState::VerificationRequired,
            "verification_required",
        ),
        (TaskDisplayState::Verifying, "verifying"),
        (TaskDisplayState::Running, "running"),
        (TaskDisplayState::Queued, "queued"),
        (TaskDisplayState::Idle, "idle"),
    ];
    for (state, serialized) in display_states {
        assert_eq!(serde_json::to_value(state).unwrap(), serialized);
        assert_eq!(
            serde_json::from_value::<TaskDisplayState>(serialized.into()).unwrap(),
            state
        );
    }

    let attention_values = [
        (TaskAttention::ApprovalRequired, "approval_required"),
        (TaskAttention::UserQuestion, "user_question"),
        (
            TaskAttention::WorkspaceBindingInvalid,
            "workspace_binding_invalid",
        ),
        (TaskAttention::RunFailed, "run_failed"),
        (TaskAttention::VerificationRequired, "verification_required"),
        (TaskAttention::ReviewRequired, "review_required"),
    ];
    for (attention, serialized) in attention_values {
        assert_eq!(serde_json::to_value(attention).unwrap(), serialized);
        assert_eq!(
            serde_json::from_value::<TaskAttention>(serialized.into()).unwrap(),
            attention
        );
    }

    let view = TaskStatusView {
        task_id: "status-task".to_string(),
        persisted_state: TaskState::Idle,
        display_state: TaskDisplayState::Idle,
        attention: Vec::new(),
        active_run_id: None,
        queue_depth: 0,
        unread_count: 3,
    };
    let serialized = serde_json::to_value(&view).unwrap();
    assert_eq!(serialized["persisted_state"], "idle");
    assert_eq!(serialized["display_state"], "idle");
    assert_eq!(serialized["attention"], serde_json::json!([]));
    assert!(serialized.get("active_run_id").is_none());
    assert_eq!(
        serde_json::from_value::<TaskStatusView>(serialized).unwrap(),
        view
    );

    let without_optional_fields = serde_json::json!({
        "task_id": "legacy-status-task",
        "persisted_state": "idle",
        "display_state": "idle",
        "queue_depth": 0,
        "unread_count": 0
    });
    let restored: TaskStatusView = serde_json::from_value(without_optional_fields).unwrap();
    assert!(restored.attention.is_empty());
    assert_eq!(restored.active_run_id, None);
}
