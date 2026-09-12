//! RA1 — persistent approvals: host-registered pending operations project
//! into the journal (`approval.requested`), decisions land as
//! `approval.decided`, plugin requests block until a decision (or the
//! timeout denial), and a restarted store rebuilds pending state from the
//! events alone.

use r_code_harness_protocol::rpc::{error_code, RpcId, RpcRequest};
use r_code_harness_protocol::{HostService, RunIdentity};
use r_code_kernel::ports::{JournalStore as _, RunGuard};
use r_code_kernel::task::{TaskContract, TaskKind, TaskState};
use r_code_kernel::testing::{FakeModelService, FakeProcessService, FakeToolService};
use r_code_runtime::plugins::approval_store::{DecideError, EVENT_DECIDED, EVENT_REQUESTED};
use r_code_runtime::plugins::{ApprovalStore, HostRouter, IgnoreQuestions};
use r_code_store::v2::V2Store;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

async fn seeded_store(dir: &TempDir, task_id: &str) -> Arc<V2Store> {
    let store = Arc::new(V2Store::open(&dir.path().join("journal.db")).expect("open store"));
    let task = TaskState::new(TaskContract {
        task_id: task_id.to_string(),
        kind: TaskKind::Conversation,
        objective: "approval fixture".into(),
        constraints: vec![],
        required_checks: vec![],
        revision: 1,
    });
    store.save_task_and_events(&task, vec![]).await.unwrap();
    store
}

/// Router over the real journal, sharing the given approval store.
fn approval_router(store: Arc<V2Store>, approvals: Arc<ApprovalStore>) -> Arc<HostRouter> {
    Arc::new(
        HostRouter::new(
            RunIdentity {
                task_id: "t1".into(),
                branch_id: "branch-t1".into(),
                run_id: "run-t1-1".into(),
                attempt_id: "attempt-t1-1".into(),
                generation: 1,
            },
            RunGuard::new("run-t1-1", 1),
            vec![HostService::QuestionsAsk, HostService::ApprovalsRequest],
            Arc::new(FakeToolService::default()),
            Arc::new(FakeModelService::default()),
            Arc::new(FakeProcessService::default()),
            store,
            Arc::new(IgnoreQuestions),
        )
        .with_approvals(approvals),
    )
}

fn approval_request(op_id: &str) -> RpcRequest {
    RpcRequest {
        jsonrpc: "2.0".into(),
        id: RpcId::Number(1),
        method: "host.approvals.request".into(),
        params: Some(serde_json::json!({
            "pending_operation": {"operation_id": op_id, "inputHash": "x"},
            "summary": "run tests"
        })),
    }
}

// -- RA1.A1: request persists, blocks, and unblocks on a grant -------------

#[tokio::test]
async fn ra1_a1_request_persists_blocks_and_unblocks_on_grant() {
    let dir = TempDir::new().expect("tempdir");
    let store = seeded_store(&dir, "t1").await;
    let approvals = Arc::new(ApprovalStore::new(store.clone(), Duration::from_secs(300)));
    let router = approval_router(store.clone(), approvals.clone());

    // Host registers the pending operation: journal grows the requested
    // event with the frozen payload fields, seq-ordered.
    let _rx = approvals
        .register("op-1", "run tests", "run-t1-1", "t1")
        .await;
    let events = store.task_events("t1");
    let requested = events
        .iter()
        .find(|event| event.kind == EVENT_REQUESTED)
        .expect("approval.requested journaled");
    assert_eq!(requested.payload["opId"], "op-1");
    assert_eq!(requested.payload["summary"], "run tests");
    assert_eq!(requested.payload["runId"], "run-t1-1");
    assert!(requested.payload["createdSeq"].is_u64());
    assert!(requested.payload["createdMs"].is_i64());

    // The plugin request blocks while undecided (100ms proves it parked).
    let mut request = Box::pin(router.handle_request(approval_request("op-1")));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut request)
            .await
            .is_err(),
        "request must block until a decision"
    );

    // A grant unblocks it; the decided event lands with the deciding client.
    approvals
        .decide(
            "op-1",
            r_code_harness_protocol::ApprovalDecision::Granted,
            "client-tui",
        )
        .await
        .expect("decide");
    let reply = request.await.expect("unblocked");
    assert_eq!(reply["decision"], "granted");
    let events = store.task_events("t1");
    let decided = events
        .iter()
        .find(|event| event.kind == EVENT_DECIDED)
        .expect("approval.decided journaled");
    assert_eq!(decided.payload["opId"], "op-1");
    assert_eq!(decided.payload["decision"], "granted");
    assert_eq!(decided.payload["decidedBy"], "client-tui");
    assert!(decided.payload["decidedSeq"].is_u64());
    // Journal ordering: requested precedes decided.
    let requested_seq = events
        .iter()
        .find(|event| event.kind == EVENT_REQUESTED)
        .unwrap()
        .seq;
    assert!(requested_seq < decided.seq);
}

// -- RA1.A2: pending ops rebuild from the journal after a restart ----------

#[tokio::test]
async fn ra1_a2_pending_ops_rebuild_from_journal_after_restart() {
    let dir = TempDir::new().expect("tempdir");
    {
        let store = seeded_store(&dir, "t1").await;
        let approvals = ApprovalStore::new(store.clone(), Duration::from_secs(300));
        approvals
            .register("op-2", "deploy db", "run-t1-1", "t1")
            .await;
        assert_eq!(approvals.pending().await.len(), 1);
        // "Daemon dies" before any decision: only the journal survives.
    }
    // Fresh store + fresh ApprovalStore over the same database file.
    let store = Arc::new(V2Store::open(&dir.path().join("journal.db")).expect("reopen"));
    let rebuilt = ApprovalStore::new(store.clone(), Duration::from_secs(300));
    let journal = store.read_events(0, u32::MAX).await;
    rebuilt.rebuild_from_events(&journal).await;

    let pending = rebuilt.pending().await;
    assert_eq!(pending.len(), 1, "pending rebuilt from events");
    assert_eq!(pending[0].op_id, "op-2");
    assert_eq!(pending[0].summary, "deploy db");
    assert_eq!(pending[0].task_id, "t1");

    // The rebuilt op is decidable; the decision lands in the journal.
    rebuilt
        .decide(
            "op-2",
            r_code_harness_protocol::ApprovalDecision::Granted,
            "client-b",
        )
        .await
        .expect("decide after rebuild");
    assert!(rebuilt.pending().await.is_empty());
    let decided = store
        .task_events("t1")
        .into_iter()
        .find(|event| event.kind == EVENT_DECIDED)
        .expect("decided persisted");
    assert_eq!(decided.payload["decidedBy"], "client-b");

    // Conflicting second decision is refused; the same decision replays.
    assert_eq!(
        rebuilt
            .decide(
                "op-2",
                r_code_harness_protocol::ApprovalDecision::Denied,
                "client-c",
            )
            .await
            .unwrap_err(),
        DecideError::Conflict("op-2".into())
    );
    let replay = rebuilt
        .decide(
            "op-2",
            r_code_harness_protocol::ApprovalDecision::Granted,
            "client-d",
        )
        .await
        .expect("same decision replays");
    assert_eq!(replay.decided_by, "client-b", "replay returns the original");
}

// -- RA1.A3: forged references are refused without any journal event -------

#[tokio::test]
async fn ra1_a3_forged_reference_is_refused_without_events() {
    let dir = TempDir::new().expect("tempdir");
    let store = seeded_store(&dir, "t1").await;
    let approvals = Arc::new(ApprovalStore::new(store.clone(), Duration::from_secs(300)));
    let router = approval_router(store.clone(), approvals.clone());

    let error = router
        .handle_request(approval_request("op-forged"))
        .await
        .expect_err("forged reference refused");
    assert_eq!(error.code, error_code::PROTOCOL_VIOLATION);
    assert!(
        store
            .task_events("t1")
            .iter()
            .all(|event| !event.kind.starts_with("approval.")),
        "no approval events from a forged reference"
    );
    assert!(approvals.pending().await.is_empty());
}

// -- Timeout denial (short-timeout configuration; RA3.A2 shares the rule) --

#[tokio::test]
async fn undecided_request_times_out_denied_and_journals_it() {
    let dir = TempDir::new().expect("tempdir");
    let store = seeded_store(&dir, "t1").await;
    let approvals = Arc::new(ApprovalStore::new(
        store.clone(),
        Duration::from_millis(120),
    ));
    let router = approval_router(store.clone(), approvals.clone());
    let _rx = approvals
        .register("op-3", "run tests", "run-t1-1", "t1")
        .await;

    let reply = router
        .handle_request(approval_request("op-3"))
        .await
        .expect("timeout resolves the request");
    assert_eq!(reply["decision"], "denied");
    let decided = store
        .task_events("t1")
        .into_iter()
        .find(|event| event.kind == EVENT_DECIDED)
        .expect("timeout denial journaled");
    assert_eq!(decided.payload["decidedBy"], "<timeout>");
    // The timed-out op is no longer pending.
    assert!(approvals.pending().await.is_empty());
}

// -- Repeated requests on the same op observe the same decision -----------

#[tokio::test]
async fn repeated_requests_are_idempotent_on_the_same_operation() {
    let dir = TempDir::new().expect("tempdir");
    let store = seeded_store(&dir, "t1").await;
    let approvals = Arc::new(ApprovalStore::new(store.clone(), Duration::from_secs(300)));
    let router = approval_router(store.clone(), approvals.clone());

    let _first = approvals
        .register("op-4", "run tests", "run-t1-1", "t1")
        .await;
    // A second register of the same op is idempotent (no second event).
    let _second = approvals
        .register("op-4", "run tests", "run-t1-1", "t1")
        .await;
    let requested = store
        .task_events("t1")
        .into_iter()
        .filter(|event| event.kind == EVENT_REQUESTED)
        .count();
    assert_eq!(requested, 1, "register is idempotent");

    approvals
        .decide(
            "op-4",
            r_code_harness_protocol::ApprovalDecision::Denied,
            "client-x",
        )
        .await
        .unwrap();
    // A late-arriving waiter observes the already-recorded decision.
    let reply = router
        .handle_request(approval_request("op-4"))
        .await
        .expect("decision predating the waiter is observed");
    assert_eq!(reply["decision"], "denied");
}
