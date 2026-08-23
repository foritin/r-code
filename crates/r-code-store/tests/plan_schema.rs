use std::collections::BTreeSet;

use r_code_core::{
    dto::{AgentRun, Task, TaskMode},
    plan::{
        PlanChangeEventState, PlanContinuationState, PlanExecutionStatus,
        PlanImplementationDispatchState, PlanItemState, PlanQuestionAnswer, PlanQuestionSetState,
        PlanRejectFileState, PlanRejectOperationState, PlanReviewDecisionKind, PlanReviewScope,
        PlanState,
    },
};
use r_code_store::{
    migrations::{run_migrations, LATEST_SCHEMA_VERSION},
    AgentRunRepository, Database, TaskRepository,
};
use rusqlite::{params, Connection};

const PLAN_TABLES: &[&str] = &[
    "plans",
    "origin_requests",
    "plan_entry_offers",
    "plan_suggestion_branch_states",
    "plan_items",
    "plan_item_dependencies",
    "plan_question_sets",
    "plan_questions",
    "plan_question_options",
    "plan_change_events",
    "plan_tool_receipts",
    "plan_review_decisions",
    "plan_reject_operations",
    "plan_reject_operation_files",
];

const PLAN_INDEXES: &[&str] = &[
    "idx_plans_current_task",
    "idx_plans_task_updated",
    "idx_plan_items_revision_state",
    "idx_plan_item_dependencies_ready",
    "idx_plan_question_sets_pending",
    "idx_plan_question_sets_revision",
    "idx_plan_change_events_feature",
    "idx_plan_change_events_path",
    "idx_plan_change_events_pending",
    "idx_plan_tool_receipts_task_plan",
    "idx_plan_review_decisions_feature",
    "idx_plan_review_decisions_file",
    "idx_plan_review_decisions_read",
    "idx_plan_reject_operations_recovery",
    "idx_plan_reject_operations_feature",
    "idx_plan_reject_operation_files_order",
    "idx_plans_implementation_dispatch",
];

const PLAN_TRIGGERS: &[&str] = &[
    "trg_plan_review_decisions_scope_guard",
    "trg_plan_reject_operations_scope_guard",
];

fn schema_version(conn: &Connection) -> i64 {
    conn.query_row("SELECT MAX(version) FROM schema_version", [], |row| {
        row.get(0)
    })
    .unwrap()
}

fn schema_objects(conn: &Connection, kind: &str) -> BTreeSet<String> {
    let mut statement = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = ?1")
        .unwrap();
    statement
        .query_map([kind], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn normalized_table_sql(conn: &Connection, table: &str) -> String {
    let sql: String = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |row| row.get(0),
        )
        .unwrap();
    sql.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> bool {
    let mut statement = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .unwrap();
    let found = statement
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .map(Result::unwrap)
        .any(|name| name == column);
    found
}

fn foreign_keys(conn: &Connection, table: &str) -> Vec<(String, String, String, String)> {
    let mut statement = conn
        .prepare(&format!("PRAGMA foreign_key_list({table})"))
        .unwrap();
    statement
        .query_map([], |row| {
            Ok((row.get(3)?, row.get(2)?, row.get(4)?, row.get(6)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn seed_task_and_run(db: &Database) -> (Task, AgentRun) {
    let task = Task::new(None, "Plan schema", "Exercise schema 19", TaskMode::Plan);
    let run = AgentRun::new(&task.id, "test-model");
    TaskRepository::new(db).create(&task).unwrap();
    AgentRunRepository::new(db).create(&run).unwrap();
    (task, run)
}

fn seed_plan_revision(conn: &Connection, task_id: &str) {
    conn.execute(
        "INSERT INTO plans
         (id, task_id, revision, state, created_at, updated_at)
         VALUES ('plan-1', ?1, 2, 'draft', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [task_id],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO plan_items
         (id, plan_id, revision, ordinal, title, description, state, created_at, updated_at)
         VALUES ('item-1', 'plan-1', 2, 0, 'Feature', 'Description', 'proposed',
                 '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        [],
    )
    .unwrap();
}

#[test]
fn clean_database_and_schema_18_upgrade_reach_latest_complete_schema() {
    let clean = Database::open_in_memory().unwrap();
    let clean_conn = clean.conn().unwrap();
    assert_eq!(
        schema_version(&clean_conn),
        i64::from(LATEST_SCHEMA_VERSION)
    );
    let tables = schema_objects(&clean_conn, "table");
    let indexes = schema_objects(&clean_conn, "index");
    let triggers = schema_objects(&clean_conn, "trigger");
    for table in PLAN_TABLES {
        assert!(
            tables.contains(*table),
            "clean schema is missing table {table}"
        );
    }
    for index in PLAN_INDEXES {
        assert!(
            indexes.contains(*index),
            "clean schema is missing index {index}"
        );
    }
    for trigger in PLAN_TRIGGERS {
        assert!(
            triggers.contains(*trigger),
            "clean schema is missing trigger {trigger}"
        );
    }
    assert!(table_has_column(
        &clean_conn,
        "queued_messages",
        "sort_order"
    ));
    assert!(table_has_column(
        &clean_conn,
        "agent_runs",
        "require_approval"
    ));
    assert!(table_has_column(
        &clean_conn,
        "memory_review_turns",
        "explicit_remember"
    ));
    drop(clean_conn);
    drop(clean);

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("schema-18.db");
    let v18 = Database::open(&path).unwrap();
    let task = Task::new(None, "Existing task", "Must survive upgrade", TaskMode::Ask);
    TaskRepository::new(&v18).create(&task).unwrap();
    let run = AgentRun::new(&task.id, "test-model");
    AgentRunRepository::new(&v18).create(&run).unwrap();
    {
        let conn = v18.conn().unwrap();
        conn.execute_batch(
            "DROP TABLE plan_entry_offers;
             DROP TABLE plan_suggestion_branch_states;
             DROP TABLE origin_requests;
             DROP INDEX idx_queued_messages_request_key;
             ALTER TABLE queued_messages DROP COLUMN request_key;
             DROP TABLE plan_tool_receipts;
             DROP TABLE plan_reject_operation_files;
             DROP TABLE plan_reject_operations;
             DROP TABLE plan_review_decisions;
             DROP TABLE plan_change_events;
             DROP TABLE plan_question_options;
             DROP TABLE plan_questions;
             DROP TABLE plan_question_sets;
             DROP TABLE plan_item_dependencies;
             DROP TABLE plan_items;
             DROP TABLE plans;
             DROP INDEX idx_queued_messages_task_dispatch;
             DROP INDEX idx_queued_messages_task_branch;
             ALTER TABLE queued_messages DROP COLUMN sort_order;
             ALTER TABLE queued_messages DROP COLUMN attachments_json;
             CREATE INDEX idx_queued_messages_task_branch
                 ON queued_messages(task_id, branch_id, state, priority DESC, created_at ASC);
             ALTER TABLE tasks DROP COLUMN goal_active;
             ALTER TABLE agent_runs DROP COLUMN require_approval;
             ALTER TABLE agent_runs DROP COLUMN guard_trip;
             ALTER TABLE agent_runs DROP COLUMN checkpoint_sha;
             ALTER TABLE agent_runs DROP COLUMN checkpoint_base_head;
             ALTER TABLE memory_review_turns DROP COLUMN explicit_remember;
             DELETE FROM schema_version WHERE version IN (19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO memory_review_turns (
                 id, run_id, task_id, branch_id, global_generation,
                 user_text, assistant_text, captured_at
             ) VALUES ('legacy-turn', ?1, ?2, 'legacy-branch', 0,
                 'legacy user text', 'legacy assistant text', '2026-01-01T00:00:00Z')",
            params![run.id, task.id],
        )
        .unwrap();
        assert_eq!(schema_version(&conn), 18);
    }
    drop(v18);

    let upgraded = Database::open(&path).unwrap();
    let conn = upgraded.conn().unwrap();
    assert_eq!(schema_version(&conn), i64::from(LATEST_SCHEMA_VERSION));
    let upgraded_task = TaskRepository::new(&upgraded)
        .get(&task.id)
        .unwrap()
        .unwrap();
    assert_eq!(upgraded_task.goal, "Must survive upgrade");
    assert!(!upgraded_task.goal_active);
    let tables = schema_objects(&conn, "table");
    let indexes = schema_objects(&conn, "index");
    let triggers = schema_objects(&conn, "trigger");
    for table in PLAN_TABLES {
        assert!(tables.contains(*table), "upgrade is missing table {table}");
    }
    for index in PLAN_INDEXES {
        assert!(indexes.contains(*index), "upgrade is missing index {index}");
    }
    for trigger in PLAN_TRIGGERS {
        assert!(
            triggers.contains(*trigger),
            "upgrade is missing trigger {trigger}"
        );
    }
    assert!(table_has_column(&conn, "queued_messages", "sort_order"));
    assert!(table_has_column(
        &conn,
        "queued_messages",
        "attachments_json"
    ));
    assert!(table_has_column(&conn, "plan_question_sets", "kind"));
    assert!(table_has_column(
        &conn,
        "plan_question_sets",
        "restore_mode"
    ));
    assert!(table_has_column(&conn, "agent_runs", "require_approval"));
    assert!(table_has_column(
        &conn,
        "memory_review_turns",
        "explicit_remember"
    ));
    let legacy_explicit: i64 = conn
        .query_row(
            "SELECT explicit_remember FROM memory_review_turns WHERE id = 'legacy-turn'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        legacy_explicit, 0,
        "migration 26 must not retroactively authorize legacy evidence"
    );
    assert!(conn
        .execute(
            "UPDATE memory_review_turns SET explicit_remember = 2 WHERE id = 'legacy-turn'",
            [],
        )
        .is_err());
}

#[test]
fn latest_schema_declares_every_check_and_foreign_key_contract() {
    let db = Database::open_in_memory().unwrap();
    let conn = db.conn().unwrap();

    let checks: &[(&str, &[&str])] = &[
        (
            "plans",
            &[
                "revision >= 1",
                "state in (",
                "approved_revision is null",
                "projection_revision is null",
                "projection_path is null",
                "implementation_dispatch_state in (",
            ],
        ),
        (
            "plan_items",
            &[
                "revision >= 1",
                "ordinal >= 0",
                "state in (",
                "completed_at is null",
            ],
        ),
        (
            "plan_item_dependencies",
            &["revision >= 1", "item_id <> depends_on_item_id"],
        ),
        (
            "plan_question_sets",
            &[
                "state in ('pending', 'answered', 'skipped')",
                "answer_idempotency_key is null",
                "continuation_state = 'dispatched'",
            ],
        ),
        (
            "plan_questions",
            &[
                "ordinal between 0 and 2",
                "answer_kind is null or answer_kind in ('option', 'free_form')",
                "answered_at is null",
            ],
        ),
        (
            "plan_question_options",
            &["ordinal >= 0", "length(trim(label)) > 0"],
        ),
        (
            "plan_change_events",
            &[
                "plan_revision >= 1",
                "before_exists in (0, 1)",
                "after_exists is null or after_exists in (0, 1)",
                "state = 'captured'",
            ],
        ),
        (
            "plan_review_decisions",
            &[
                "scope in ('feature', 'file')",
                "decision in ('accepted', 'rejected')",
                "scope = 'feature' and path is null",
            ],
        ),
        (
            "plan_reject_operations",
            &[
                "recovery_count >= 0",
                "state in (",
                "scope = 'feature' and path is null",
                "state in ('committed', 'rolled_back', 'conflict', 'failed')",
            ],
        ),
        (
            "plan_reject_operation_files",
            &[
                "ordinal >= 0",
                "expected_exists in (0, 1)",
                "rollback_exists in (0, 1)",
                "desired_exists in (0, 1)",
                "state = 'rolled_back'",
            ],
        ),
    ];
    for (table, fragments) in checks {
        let sql = normalized_table_sql(&conn, table);
        for fragment in *fragments {
            assert!(
                sql.contains(fragment),
                "{table} is missing CHECK contract: {fragment}\n{sql}"
            );
        }
    }
    let expected_check_counts = [
        ("plans", 9),
        ("plan_items", 6),
        ("plan_item_dependencies", 2),
        ("plan_question_sets", 8),
        ("plan_questions", 6),
        ("plan_question_options", 3),
        ("plan_change_events", 9),
        ("plan_review_decisions", 5),
        ("plan_reject_operations", 7),
        ("plan_reject_operation_files", 10),
    ];
    for (table, expected_count) in expected_check_counts {
        let sql = normalized_table_sql(&conn, table);
        assert_eq!(
            sql.matches("check (").count(),
            expected_count,
            "unexpected CHECK set for {table}: {sql}"
        );
    }
    let receipts_sql = normalized_table_sql(&conn, "plan_tool_receipts");
    assert!(
        receipts_sql.contains("primary key (task_id, run_id, tool_call_id)"),
        "Plan tool receipt idempotency scope drifted: {receipts_sql}"
    );

    let expected_fk_counts = [
        ("plans", 1),
        ("plan_items", 1),
        ("plan_item_dependencies", 6),
        ("plan_question_sets", 1),
        ("plan_questions", 1),
        ("plan_question_options", 2),
        ("plan_change_events", 8),
        ("plan_tool_receipts", 2),
        ("plan_review_decisions", 4),
        ("plan_reject_operations", 4),
        ("plan_reject_operation_files", 4),
    ];
    for (table, expected_count) in expected_fk_counts {
        let keys = foreign_keys(&conn, table);
        assert_eq!(
            keys.len(),
            expected_count,
            "unexpected FK set for {table}: {keys:?}"
        );
        assert!(keys
            .iter()
            .all(|(_, _, _, on_delete)| on_delete == "CASCADE" || on_delete == "NO ACTION"));
    }

    assert!(foreign_keys(&conn, "plans").contains(&(
        "task_id".into(),
        "tasks".into(),
        "id".into(),
        "CASCADE".into(),
    )));
    assert!(foreign_keys(&conn, "plan_questions").contains(&(
        "question_set_id".into(),
        "plan_question_sets".into(),
        "id".into(),
        "CASCADE".into(),
    )));
    assert!(foreign_keys(&conn, "plan_question_options").contains(&(
        "question_id".into(),
        "plan_questions".into(),
        "id".into(),
        "CASCADE".into(),
    )));
    assert!(foreign_keys(&conn, "plan_tool_receipts").contains(&(
        "task_id".into(),
        "tasks".into(),
        "id".into(),
        "CASCADE".into(),
    )));
    assert!(foreign_keys(&conn, "plan_tool_receipts").contains(&(
        "plan_id".into(),
        "plans".into(),
        "id".into(),
        "CASCADE".into(),
    )));
    assert!(
        foreign_keys(&conn, "plan_reject_operation_files").contains(&(
            "operation_id".into(),
            "plan_reject_operations".into(),
            "id".into(),
            "CASCADE".into(),
        ))
    );
    assert!(PLAN_TABLES.iter().all(|table| foreign_keys(&conn, table)
        .iter()
        .all(|(from, _, _, _)| from != "tool_call_id")));
    assert!(conn
        .prepare("PRAGMA foreign_key_check")
        .unwrap()
        .query([])
        .unwrap()
        .next()
        .unwrap()
        .is_none());
}

#[test]
fn question_skip_is_whole_set_idempotent_and_change_event_does_not_wait_for_tool_audit() {
    let db = Database::open_in_memory().unwrap();
    let (task, run) = seed_task_and_run(&db);
    let conn = db.conn().unwrap();
    seed_plan_revision(&conn, &task.id);

    conn.execute(
        "INSERT INTO plan_question_sets
         (id, plan_id, revision, state, answer_idempotency_key, continuation_state, created_at, resolved_at)
         VALUES ('questions-1', 'plan-1', 2, 'skipped', 'answer-key-1', 'pending',
                 '2026-01-01T00:00:00Z', '2026-01-01T00:01:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO plan_questions
         (id, question_set_id, ordinal, header, question)
         VALUES ('question-1', 'questions-1', 0, 'Scope', 'Which scope?')",
        [],
    )
    .unwrap();

    let skipped_answer = conn.execute(
        "UPDATE plan_questions
         SET answer_kind = 'skipped', answer_value = 'skipped', answered_at = '2026-01-01T00:01:00Z'
         WHERE id = 'question-1'",
        [],
    );
    assert!(
        skipped_answer.is_err(),
        "skip must not be represented per question"
    );
    conn.execute(
        "INSERT INTO plan_question_sets
         (id, plan_id, revision, state, answer_idempotency_key, continuation_state, created_at, resolved_at)
         VALUES ('questions-2', 'plan-1', 2, 'answered', 'answer-key-1', 'pending',
                 '2026-01-01T00:02:00Z', '2026-01-01T00:03:00Z')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO plan_change_events
         (id, plan_id, plan_revision, item_id, task_id, run_id, tool_call_id, path,
          before_exists, state, created_at)
         VALUES ('event-1', 'plan-1', 2, 'item-1', ?1, ?2,
                 'tool-audit-arrives-later', 'src/lib.rs', 0, 'pending', '2026-01-01T00:00:00Z')",
        params![task.id, run.id],
    )
    .unwrap();
    assert!(
        conn.execute(
            "INSERT INTO plan_change_events
         (id, plan_id, plan_revision, item_id, task_id, run_id, tool_call_id, path,
          before_exists, state, created_at)
         VALUES ('event-stale', 'plan-1', 1, 'item-1', ?1, ?2,
                 'tool-stale', 'src/stale.rs', 0, 'pending', '2026-01-01T00:00:00Z')",
            params![task.id, run.id],
        )
        .is_err(),
        "an item cannot be attributed through a stale plan revision"
    );
}

#[test]
fn resolved_question_set_requires_a_non_null_answer_idempotency_key() {
    let db = Database::open_in_memory().unwrap();
    let (task, _) = seed_task_and_run(&db);
    let conn = db.conn().unwrap();
    seed_plan_revision(&conn, &task.id);

    assert!(conn.execute(
        "INSERT INTO plan_question_sets
         (id, plan_id, revision, state, answer_idempotency_key, continuation_state, created_at, resolved_at)
         VALUES ('questions-without-key', 'plan-1', 2, 'skipped', NULL, 'pending',
                 '2026-01-01T00:04:00Z', '2026-01-01T00:05:00Z')",
        [],
    ).is_err(), "a resolved whole-set skip requires its idempotency key");
}

#[test]
fn task_mode_plan_round_trips_on_create_and_set_mode() {
    let db = Database::open_in_memory().unwrap();
    let repository = TaskRepository::new(&db);
    let task = Task::new(None, "Plan task", "Plan it", TaskMode::Plan);
    repository.create(&task).unwrap();
    assert_eq!(
        repository.get(&task.id).unwrap().unwrap().mode,
        TaskMode::Plan
    );

    repository.set_mode(&task.id, TaskMode::Edit).unwrap();
    assert_eq!(
        repository.get(&task.id).unwrap().unwrap().mode,
        TaskMode::Edit
    );
    repository.set_mode(&task.id, TaskMode::Plan).unwrap();
    assert_eq!(
        repository.get(&task.id).unwrap().unwrap().mode,
        TaskMode::Plan
    );
    assert_eq!(TaskMode::try_from_str("plan"), Some(TaskMode::Plan));
    assert_eq!(TaskMode::Plan.to_string(), "plan");
}

#[test]
fn plan_state_strings_and_question_answer_kinds_are_stable() {
    macro_rules! assert_stable_strings {
        ($type:ty, [$($variant:expr => $text:literal),+ $(,)?]) => {
            $(
                assert_eq!($variant.as_str(), $text);
                assert_eq!(<$type>::try_from_str($text), Some($variant));
                assert_eq!(serde_json::to_string(&$variant).unwrap(), concat!("\"", $text, "\""));
                assert_eq!(serde_json::from_str::<$type>(concat!("\"", $text, "\"")).unwrap(), $variant);
            )+
        };
    }

    assert_stable_strings!(PlanState, [
        PlanState::Draft => "draft", PlanState::AwaitingInput => "awaiting_input",
        PlanState::Ready => "ready", PlanState::Approved => "approved",
        PlanState::Executing => "executing", PlanState::Completed => "completed",
        PlanState::Cancelled => "cancelled",
    ]);
    assert_stable_strings!(PlanItemState, [
        PlanItemState::Proposed => "proposed", PlanItemState::Pending => "pending",
        PlanItemState::InProgress => "in_progress", PlanItemState::Blocked => "blocked",
        PlanItemState::Completed => "completed", PlanItemState::Failed => "failed",
        PlanItemState::Cancelled => "cancelled",
    ]);
    assert_stable_strings!(PlanQuestionSetState, [
        PlanQuestionSetState::Pending => "pending", PlanQuestionSetState::Answered => "answered",
        PlanQuestionSetState::Skipped => "skipped",
    ]);
    assert_stable_strings!(PlanContinuationState, [
        PlanContinuationState::NotRequested => "not_requested", PlanContinuationState::Pending => "pending",
        PlanContinuationState::Dispatching => "dispatching", PlanContinuationState::Dispatched => "dispatched",
        PlanContinuationState::Failed => "failed",
    ]);
    assert_stable_strings!(PlanImplementationDispatchState, [
        PlanImplementationDispatchState::NotRequested => "not_requested",
        PlanImplementationDispatchState::Pending => "pending",
        PlanImplementationDispatchState::Dispatching => "dispatching",
        PlanImplementationDispatchState::Dispatched => "dispatched",
        PlanImplementationDispatchState::Failed => "failed",
    ]);
    assert_stable_strings!(PlanExecutionStatus, [
        PlanExecutionStatus::NoExecutingPlan => "no_executing_plan",
        PlanExecutionStatus::ActiveFeature => "active_feature",
        PlanExecutionStatus::Paused => "paused",
    ]);
    assert_stable_strings!(PlanChangeEventState, [
        PlanChangeEventState::Pending => "pending", PlanChangeEventState::Captured => "captured",
        PlanChangeEventState::Failed => "failed",
    ]);
    assert_stable_strings!(PlanReviewScope, [
        PlanReviewScope::Feature => "feature", PlanReviewScope::File => "file",
    ]);
    assert_stable_strings!(PlanReviewDecisionKind, [
        PlanReviewDecisionKind::Accepted => "accepted", PlanReviewDecisionKind::Rejected => "rejected",
    ]);
    assert_stable_strings!(PlanRejectOperationState, [
        PlanRejectOperationState::Prepared => "prepared", PlanRejectOperationState::Applying => "applying",
        PlanRejectOperationState::Committed => "committed", PlanRejectOperationState::RollingBack => "rolling_back",
        PlanRejectOperationState::RolledBack => "rolled_back", PlanRejectOperationState::Conflict => "conflict",
        PlanRejectOperationState::Failed => "failed",
    ]);
    assert_stable_strings!(PlanRejectFileState, [
        PlanRejectFileState::Pending => "pending", PlanRejectFileState::Applied => "applied",
        PlanRejectFileState::RolledBack => "rolled_back", PlanRejectFileState::Conflict => "conflict",
    ]);

    let option = PlanQuestionAnswer::Option {
        option_id: "option-1".into(),
    };
    let free_form = PlanQuestionAnswer::FreeForm {
        text: "Custom".into(),
    };
    assert_eq!(
        serde_json::from_str::<PlanQuestionAnswer>(&serde_json::to_string(&option).unwrap())
            .unwrap(),
        option
    );
    assert_eq!(
        serde_json::from_str::<PlanQuestionAnswer>(&serde_json::to_string(&free_form).unwrap())
            .unwrap(),
        free_form
    );
    assert!(serde_json::from_str::<PlanQuestionAnswer>(r#"{"kind":"skipped"}"#).is_err());
}

#[test]
fn direct_schema_18_migration_entrypoint_is_idempotent() {
    let conn = Connection::open_in_memory().unwrap();
    run_migrations(&conn).unwrap();
    run_migrations(&conn).unwrap();
    assert_eq!(schema_version(&conn), i64::from(LATEST_SCHEMA_VERSION));
}
