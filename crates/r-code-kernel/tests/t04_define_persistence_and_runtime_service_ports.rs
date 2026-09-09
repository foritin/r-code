//! T04 — persistence and runtime service ports.
//!
//! Acceptance: the kernel Cargo dependency graph excludes Tauri, SQLite,
//! Gateway, provider implementations and harness binaries; fakes implement
//! every port and honour generation/cancellation contracts.

use r_code_harness_protocol::{
    ContentBlock, InputKind, InputMessage, ModelMessage, ModelRole, ModelStreamRequest,
    OperationKey,
};
use r_code_kernel::ports::*;
use r_code_kernel::task::*;
use r_code_kernel::testing::*;

fn contract() -> TaskContract {
    TaskContract {
        task_id: "task-1".into(),
        kind: TaskKind::Conversation,
        objective: "chat".into(),
        constraints: vec![],
        required_checks: vec![],
        revision: 1,
    }
}

#[tokio::test]
async fn journal_round_trips_tasks_events_and_receipts_atomically() {
    let journal = MemoryJournal::new();
    let mut state = TaskState::new(contract());
    state.execution = TaskExecution::Running {
        attempt_id: "attempt-1".into(),
        generation: 1,
    };
    journal
        .save_task_and_events(
            &state,
            vec![
                JournalEvent {
                    seq: 0,
                    task_id: "task-1".into(),
                    kind: "task.created".into(),
                    payload: serde_json::json!({}),
                },
                JournalEvent {
                    seq: 0,
                    task_id: "task-1".into(),
                    kind: "run.started".into(),
                    payload: serde_json::json!({"attempt": "attempt-1"}),
                },
            ],
        )
        .await
        .expect("atomic save");

    let loaded = journal.load_task("task-1").await.expect("task persisted");
    assert_eq!(loaded.execution, state.execution);
    assert_eq!(journal.event_count(), 2);
    let events = journal.read_events(0, 10).await;
    assert_eq!(events.len(), 2);
    // Sequences are assigned densely starting at 1 (after_seq=0 replays all).
    assert_eq!((events[0].seq, events[1].seq), (1, 2));
    assert_eq!(events[0].kind, "task.created");
    assert!(journal.read_events(1, 10).await.len() == 1);

    let receipt = OperationReceipt {
        attempt_id: "attempt-1".into(),
        operation_key: OperationKey::new("write-lib"),
        method: "host.tools.call".into(),
        input_hash: "abc".into(),
        outcome: ReceiptOutcome::Completed {
            result: serde_json::json!({"ok": true}),
        },
    };
    journal
        .save_receipt(receipt.clone())
        .await
        .expect("receipt saved");
    let loaded_receipt = journal
        .load_receipt("attempt-1", &OperationKey::new("write-lib"))
        .await
        .expect("receipt loaded");
    assert_eq!(loaded_receipt, receipt);
}

#[tokio::test]
async fn checkpoints_round_trip_with_consumed_positions() {
    let journal = MemoryJournal::new();
    let artifact = journal
        .save_checkpoint("attempt-1", 1, b"opaque-state".to_vec(), 3)
        .await
        .expect("save");
    assert_eq!(artifact.bytes, 12);
    let record = journal
        .load_latest_checkpoint("attempt-1")
        .await
        .expect("load");
    assert_eq!(record.consumed_input_seq, 3);
    assert_eq!(record.state, b"opaque-state".to_vec());
    assert_eq!(record.artifact, artifact);

    journal
        .save_checkpoint("attempt-1", 2, b"newer-state".to_vec(), 5)
        .await
        .expect("save newer");
    let latest = journal
        .load_latest_checkpoint("attempt-1")
        .await
        .expect("latest");
    assert_eq!(latest.revision, 2);
    assert_eq!(latest.consumed_input_seq, 5);
}

#[tokio::test]
async fn generation_guard_rejects_stale_and_cancelled_calls() {
    let guard = RunGuard::new("run-1", 1);
    let token = guard.token();
    assert_eq!(token.generation, 1);
    guard.check(&token).expect("live token accepted");

    guard.rotate();
    assert!(matches!(
        guard.check(&token),
        Err(ServiceError::StaleGeneration {
            current: 2,
            provided: 1,
            ..
        })
    ));

    let fresh = guard.token();
    guard.check(&fresh).expect("fresh token accepted");
    guard.revoke();
    assert!(matches!(guard.check(&fresh), Err(ServiceError::Cancelled)));
}

#[tokio::test]
async fn fakes_implement_every_port_with_expected_shapes() {
    let guard = RunGuard::new("run-1", 1);
    let token = guard.token();

    let models = FakeModelService::default();
    let mut sink = CollectSink::new();
    let outcome = models
        .stream(
            token.clone(),
            ModelStreamRequest {
                selection: Some("provider/model".into()),
                messages: vec![
                    ModelMessage {
                        role: ModelRole::User,
                        content: vec![ContentBlock::Text { text: "a".into() }],
                    },
                    ModelMessage {
                        role: ModelRole::Assistant,
                        content: vec![ContentBlock::Text { text: "b".into() }],
                    },
                ],
                tools: vec![],
                inference: None,
                deadline_ms: None,
            },
            &mut sink,
        )
        .await
        .expect("stream");
    assert_eq!(outcome.finish_reason.as_deref(), Some("stop"));
    assert_eq!(sink.events.len(), 3);
    assert_eq!(sink.events.last().unwrap().done, Some(true));

    let tools = FakeToolService::default();
    let listed = tools.list(token.clone()).await.expect("list");
    assert_eq!(listed.len(), 1);
    let reply = tools
        .call(
            token.clone(),
            r_code_harness_protocol::ToolCallRequest {
                tool: "read_file".into(),
                input: serde_json::json!({"path": "a.txt"}),
            },
        )
        .await
        .expect("call");
    assert!(reply.error.is_none());
    assert_eq!(tools.calls.lock().unwrap().len(), 1);

    let processes = FakeProcessService::default();
    let handle = processes
        .open(token.clone(), "app-server", vec![], None)
        .await
        .expect("open");
    processes
        .write(token.clone(), &handle, b"hello".to_vec())
        .await
        .expect("write");
    let exit = processes
        .close(token.clone(), &handle)
        .await
        .expect("close");
    assert_eq!(exit, Some(0));

    let session = FakeHarnessSession::default();
    session
        .start(
            &Attempt {
                attempt_id: "attempt-1".into(),
                task_id: "task-1".into(),
                branch_id: "branch-1".into(),
                package: r_code_harness_protocol::PackageRef {
                    id: r_code_harness_protocol::HarnessId::new("example.harness"),
                    version: semver::Version::new(1, 0, 0),
                    content_digest: "sha256:aaa".into(),
                },
                contract_revision: 1,
                config_hash: "cfg".into(),
                workspace_identity: "ws".into(),
                run_id: "run-1".into(),
            },
            &contract(),
            &InputMessage {
                message_id: "m1".into(),
                input_seq: 1,
                kind: InputKind::User,
                text: "go".into(),
            },
        )
        .await
        .expect("start");
    assert_eq!(session.started.lock().unwrap().len(), 1);
}

#[test]
fn kernel_dependency_graph_excludes_forbidden_crates() {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let kernel_cargo = std::path::Path::new(manifest_dir).join("Cargo.toml");
    let protocol_cargo =
        std::path::Path::new(manifest_dir).join("../r-code-harness-protocol/Cargo.toml");
    let forbidden = [
        "tauri",
        "rusqlite",
        "r-code-gateway",
        "r-code-store",
        "r-code-runtime",
        "r-code-agent-worker",
        "r-code-host",
        "agent-llm",
        "r-code-harness-native",
        "r-code-harness-codex",
    ];
    for path in [kernel_cargo, protocol_cargo] {
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
        // Only the [dependencies] section is the link-time surface;
        // dev-dependencies serve in-crate tests (e.g. the v2 store
        // round-trip fixture) without shipping in the kernel.
        let lower = text
            .split("[dependencies]")
            .nth(1)
            .map(|section| section.split('[').next().unwrap_or(section))
            .unwrap_or("")
            .to_ascii_lowercase();
        for crate_name in forbidden {
            assert!(
                !lower.contains(crate_name),
                "{:?} must not depend on {crate_name}",
                path.file_name().unwrap()
            );
        }
    }
}
