//! T05 — v2 profile and durable storage schema.
//!
//! Acceptance: rollback/fault fixtures prove no aggregate/event split and no
//! simultaneous active run for the same branch.

use r_code_harness_protocol::OperationKey;
use r_code_kernel::ports::{JournalEvent, JournalStore};
use r_code_kernel::task::{
    OperationReceipt, ReceiptOutcome, TaskContract, TaskExecution, TaskKind, TaskState,
};
use r_code_store::v2::{LeaseAcquisition, V2Store, V2StoreError};

fn task_state(task_id: &str, revision: u64) -> TaskState {
    TaskState::new(TaskContract {
        task_id: task_id.into(),
        kind: TaskKind::Conversation,
        objective: "chat".into(),
        constraints: vec![],
        required_checks: vec![],
        revision,
    })
}

fn events_for(task_id: &str, count: usize) -> Vec<JournalEvent> {
    (0..count)
        .map(|index| JournalEvent {
            seq: 0,
            task_id: task_id.into(),
            kind: "test.event".into(),
            payload: serde_json::json!({"index": index}),
        })
        .collect()
}

#[tokio::test]
async fn aggregate_and_events_commit_atomically() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = V2Store::open(&temp.path().join("harness-v2").join("tasks.sqlite3")).expect("open");

    let task = task_state("task-1", 1);
    store
        .save_task_and_events(&task, events_for("task-1", 3))
        .await
        .expect("save");

    let loaded = store.load_task("task-1").await.expect("loaded");
    assert_eq!(loaded.contract.task_id, "task-1");
    assert_eq!(loaded.contract.revision, 1);
    let events = store.read_events(0, 100).await;
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].seq, 1);
    assert_eq!(events[2].seq, 3);
    assert_eq!(events[1].payload["index"], 1);

    // A second save bumps the optimistic revision.
    let mut updated = task_state("task-1", 1);
    updated.execution = TaskExecution::Running {
        attempt_id: "attempt-1".into(),
        generation: 1,
    };
    store
        .save_task_and_events(&updated, events_for("task-1", 1))
        .await
        .expect("update");
    let loaded = store.load_task("task-1").await.expect("loaded");
    assert!(matches!(loaded.execution, TaskExecution::Running { .. }));
    assert_eq!(store.read_events(0, 100).await.len(), 4);
}

#[tokio::test]
async fn injected_fault_rolls_back_aggregate_and_events_together() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = V2Store::open(&temp.path().join("harness-v2").join("tasks.sqlite3")).expect("open");

    // Seed one committed task.
    store
        .save_task_and_events(&task_state("task-1", 1), events_for("task-1", 2))
        .await
        .expect("seed");

    // The next save fails after the writes but before commit.
    store.debug_fail_next_save();
    let result = store
        .save_task_and_events(&task_state("task-2", 1), events_for("task-2", 5))
        .await;
    assert!(result.is_err(), "injected fault must surface");

    // Neither the new aggregate nor any of its events survived: no split.
    assert!(store.load_task("task-2").await.is_none());
    let events = store.read_events(0, 100).await;
    assert_eq!(events.len(), 2, "no partial events from the failed save");
    assert!(events.iter().all(|event| event.task_id == "task-1"));
}

#[test]
fn branch_admits_exactly_one_active_run() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = V2Store::open(&temp.path().join("harness-v2").join("tasks.sqlite3")).expect("open");

    assert_eq!(
        store
            .try_acquire_run_lease("branch-1", "run-a", "attempt-1", 1, "daemon-1")
            .expect("acquire"),
        LeaseAcquisition::Acquired
    );
    // The same run re-acquiring is idempotent.
    assert_eq!(
        store
            .try_acquire_run_lease("branch-1", "run-a", "attempt-1", 1, "daemon-1")
            .expect("re-acquire"),
        LeaseAcquisition::AlreadyHeld
    );
    // A different run on the same branch is refused.
    assert!(matches!(
        store.try_acquire_run_lease("branch-1", "run-b", "attempt-2", 1, "daemon-1"),
        Err(V2StoreError::LeaseHeld { branch_id, holder }) if branch_id == "branch-1" && holder == "run-a"
    ));
    assert_eq!(store.run_lease_holder("branch-1").as_deref(), Some("run-a"));

    // Another branch is unaffected.
    assert_eq!(
        store
            .try_acquire_run_lease("branch-2", "run-c", "attempt-3", 1, "daemon-1")
            .expect("acquire other branch"),
        LeaseAcquisition::Acquired
    );

    // Release then re-acquire works for the next run.
    assert!(store
        .release_run_lease("branch-1", "run-a")
        .expect("release"));
    assert_eq!(store.run_lease_holder("branch-1"), None);
    assert_eq!(
        store
            .try_acquire_run_lease("branch-1", "run-b", "attempt-2", 1, "daemon-1")
            .expect("acquire after release"),
        LeaseAcquisition::Acquired
    );
    // Releasing someone else's lease is a no-op.
    assert!(!store.release_run_lease("branch-1", "run-z").expect("no-op"));
}

#[tokio::test]
async fn receipts_and_checkpoints_round_trip() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = V2Store::open(&temp.path().join("harness-v2").join("tasks.sqlite3")).expect("open");

    let receipt = OperationReceipt {
        attempt_id: "attempt-1".into(),
        operation_key: OperationKey::new("write-lib"),
        method: "host.tools.call".into(),
        input_hash: "abc123".into(),
        outcome: ReceiptOutcome::Completed {
            result: serde_json::json!({"ok": true}),
        },
    };
    store
        .save_receipt(receipt.clone())
        .await
        .expect("save receipt");
    let loaded = store
        .load_receipt("attempt-1", &OperationKey::new("write-lib"))
        .await
        .expect("load receipt");
    assert_eq!(loaded, receipt);
    assert!(store
        .load_receipt("attempt-1", &OperationKey::new("other"))
        .await
        .is_none());

    let artifact = store
        .save_checkpoint("attempt-1", 1, b"state-one".to_vec(), 2)
        .await
        .expect("checkpoint 1");
    assert!(artifact.blob_id.contains("attempt-1"));
    let checkpoint = store
        .load_latest_checkpoint("attempt-1")
        .await
        .expect("checkpoint");
    assert_eq!(checkpoint.state, b"state-one".to_vec());
    assert_eq!(checkpoint.consumed_input_seq, 2);

    store
        .save_checkpoint("attempt-1", 2, b"state-two".to_vec(), 4)
        .await
        .expect("checkpoint 2");
    let latest = store
        .load_latest_checkpoint("attempt-1")
        .await
        .expect("latest");
    assert_eq!(latest.revision, 2);
    assert_eq!(latest.state, b"state-two".to_vec());
    assert_eq!(latest.consumed_input_seq, 4);
}

#[test]
fn v2_schema_is_fresh_and_legacy_tables_do_not_appear() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join("harness-v2").join("tasks.sqlite3");
    {
        let _store = V2Store::open(&path).expect("open");
    }
    let connection = rusqlite::Connection::open(&path).expect("reopen");
    let mut tables = Vec::new();
    let mut statement = connection
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .expect("prepare");
    let mut rows = statement.query([]).expect("query");
    while let Some(row) = rows.next().expect("row") {
        tables.push(row.get::<_, String>(0).expect("name"));
    }
    for expected in [
        "v2_meta",
        "tasks",
        "events",
        "operation_receipts",
        "run_leases",
        "blobs",
        "checkpoints",
        "questions",
        "reviews",
        "evidence",
    ] {
        assert!(
            tables.contains(&expected.to_string()),
            "missing table {expected}"
        );
    }
    // The v2 database is created fresh: no legacy product tables sneak in.
    for absent in [
        "task",
        "runs",
        "tool_calls",
        "permission_requests",
        "file_changes",
    ] {
        assert!(
            !tables.contains(&absent.to_string()),
            "legacy table {absent} present"
        );
    }
    // Schema version is recorded.
    let version: String = connection
        .query_row(
            "SELECT value FROM v2_meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )
        .expect("version");
    assert_eq!(version, "1");
}
