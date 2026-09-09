//! T06 — task, branch, queue and lease services.
//!
//! Acceptance: queue/steer race tests show exactly one delivery and no
//! callbacks crossing task or harness identity.

use r_code_harness_protocol::{HarnessId, InputKind, OperationKey, PackageRef};
use r_code_kernel::ports::JournalStore;
use r_code_kernel::task::*;
use r_code_kernel::tasks::*;
use r_code_kernel::testing::*;
use std::sync::Arc;

fn package(id: &str, digest: &str) -> PackageRef {
    PackageRef {
        id: HarnessId::new(id),
        version: semver::Version::new(1, 0, 0),
        content_digest: digest.into(),
    }
}

fn contract(task_id: &str) -> TaskContract {
    TaskContract {
        task_id: task_id.into(),
        kind: TaskKind::Conversation,
        objective: "objective".into(),
        constraints: vec![],
        required_checks: vec![],
        revision: 1,
    }
}

fn attempt_for(task_id: &str, pkg: &PackageRef) -> Attempt {
    Attempt {
        attempt_id: format!("attempt-{task_id}"),
        task_id: task_id.into(),
        branch_id: format!("branch-{task_id}"),
        package: pkg.clone(),
        contract_revision: 1,
        config_hash: "cfg".into(),
        workspace_identity: "ws".into(),
        run_id: format!("run-{task_id}"),
    }
}

#[tokio::test]
async fn queue_acceptance_is_exactly_once_per_operation_key() {
    let journal = Arc::new(MemoryJournal::new());
    let service = TaskService::new(journal.clone());
    service
        .create_task(contract("task-1"))
        .await
        .expect("create");

    let key = OperationKey::new("submit-1");
    let first = service
        .enqueue("task-1", InputKind::User, "hello", Some(key.clone()))
        .await
        .expect("first");
    assert_eq!(first.input_seq, 1);

    // Same key, same payload: replays the original message, no new seq.
    let retry = service
        .enqueue("task-1", InputKind::User, "hello", Some(key.clone()))
        .await
        .expect("retry");
    assert_eq!(retry, first);

    // Same key, different payload: refused.
    let err = service
        .enqueue("task-1", InputKind::User, "tampered", Some(key))
        .await
        .expect_err("conflict");
    assert!(matches!(
        err,
        TaskServiceError::ConflictingOperationKey { .. }
    ));

    // Distinct submissions get dense sequence numbers.
    let second = service
        .enqueue("task-1", InputKind::User, "again", None)
        .await
        .expect("second");
    assert_eq!(second.input_seq, 2);
}

#[tokio::test]
async fn delivery_is_single_consumer_and_exactly_once() {
    let journal = Arc::new(MemoryJournal::new());
    let service = TaskService::new(journal);
    service
        .create_task(contract("task-1"))
        .await
        .expect("create");
    service
        .enqueue("task-1", InputKind::User, "one", None)
        .await
        .expect("enqueue");
    service
        .enqueue("task-1", InputKind::User, "two", None)
        .await
        .expect("enqueue");

    let first = service.poll("task-1").await.expect("message one");
    assert_eq!(first.text, "one");
    // Race: a second poll before acknowledgement gets nothing.
    assert!(service.poll("task-1").await.is_none());
    service
        .acknowledge("task-1", &first.message_id)
        .await
        .expect("ack");

    let second = service.poll("task-1").await.expect("message two");
    assert_eq!(second.text, "two");
    // Acknowledging an unknown id is refused.
    assert!(matches!(
        service.acknowledge("task-1", "bogus").await,
        Err(TaskServiceError::NotInFlight(_))
    ));
    service
        .acknowledge("task-1", &second.message_id)
        .await
        .expect("ack");
    assert!(service.poll("task-1").await.is_none());
}

#[tokio::test]
async fn queue_survives_restart_via_journal_replay() {
    let journal = Arc::new(MemoryJournal::new());
    {
        let service = TaskService::new(journal.clone());
        service
            .create_task(contract("task-1"))
            .await
            .expect("create");
        service
            .enqueue("task-1", InputKind::User, "persisted", None)
            .await
            .expect("enqueue");
    }
    // A fresh service instance over the same store rebuilds the queue.
    let service = TaskService::new(journal);
    let pending = service.reload("task-1").await.expect("reload");
    assert_eq!(pending, 1);
    let message = service.poll("task-1").await.expect("replayed");
    assert_eq!(message.text, "persisted");
}

#[tokio::test]
async fn steer_boundaries_forward_only_to_active_runs() {
    let journal = Arc::new(MemoryJournal::new());
    let service = TaskService::new(journal);
    let session = FakeHarnessSession::default();
    service
        .create_task(contract("task-1"))
        .await
        .expect("create");

    // No run yet: steer is rejected, nothing is queued.
    assert!(matches!(
        service.submit_steer("task-1", "faster", &session).await,
        Err(TaskServiceError::SteerWithoutRun(_))
    ));
    assert!(service.poll("task-1").await.is_none());

    // Start a run; steer is accepted, forwarded and marked delivered.
    let pkg = package("example.harness", "sha-a");
    service
        .pin_harness("task-1", pkg.clone())
        .await
        .expect("pin");
    let first = service
        .enqueue("task-1", InputKind::User, "go", None)
        .await
        .expect("first input");
    let input = service.poll("task-1").await.expect("poll");
    service
        .acknowledge("task-1", &input.message_id)
        .await
        .expect("ack");
    service
        .start_run(&attempt_for("task-1", &pkg), &first, &session)
        .await
        .expect("start run");

    let steer = service
        .submit_steer("task-1", "faster", &session)
        .await
        .expect("steer");
    assert_eq!(steer.kind, InputKind::Steer);
    assert_eq!(session.steered.lock().unwrap().len(), 1);
    // The steer was consumed: no lingering delivery.
    assert!(service.poll("task-1").await.is_none());

    // A queued user message waits for the current turn to end; a steer does
    // not dequeue it.
    service
        .enqueue("task-1", InputKind::User, "next turn", None)
        .await
        .expect("queued");
    let next = service
        .poll("task-1")
        .await
        .expect("queued message delivered next");
    assert_eq!(next.text, "next turn");
}

#[tokio::test]
async fn harness_selection_pins_and_switches_only_when_idle() {
    let journal = Arc::new(MemoryJournal::new());
    let service = TaskService::new(journal.clone());
    let session = FakeHarnessSession::default();
    service
        .create_task(contract("task-1"))
        .await
        .expect("create");

    let pkg_a = package("harness.a", "sha-a");
    service
        .pin_harness("task-1", pkg_a.clone())
        .await
        .expect("pin");
    let first = service
        .enqueue("task-1", InputKind::User, "go", None)
        .await
        .expect("input");
    let polled = service.poll("task-1").await.expect("poll");
    service
        .acknowledge("task-1", &polled.message_id)
        .await
        .expect("ack");
    service
        .start_run(&attempt_for("task-1", &pkg_a), &first, &session)
        .await
        .expect("start");

    // While running: switching harnesses is refused.
    assert!(matches!(
        service
            .switch_harness("task-1", package("harness.b", "sha-b"))
            .await,
        Err(TaskServiceError::TaskNotIdle(_))
    ));

    // An attempt whose package differs from the pin never starts.
    assert!(matches!(
        service
            .start_run(
                &attempt_for("task-1", &package("harness.b", "sha-b")),
                &first,
                &session
            )
            .await,
        Err(TaskServiceError::HarnessMismatch { .. })
    ));

    // Finish the run; switching is now allowed.
    let mut loaded = journal.load_task("task-1").await.expect("loaded");
    loaded
        .finalize(
            Actor::User,
            TaskVerdict::Unverified {
                reason: "done".into(),
            },
        )
        .expect("finalize");
    journal
        .save_task_and_events(
            &loaded,
            vec![r_code_kernel::ports::JournalEvent {
                seq: 0,
                task_id: "task-1".into(),
                kind: "task.finalized".into(),
                payload: serde_json::json!({}),
            }],
        )
        .await
        .expect("saved");

    let pkg_b = package("harness.b", "sha-b");
    service
        .switch_harness("task-1", pkg_b.clone())
        .await
        .expect("switch after idle");
    assert!(matches!(
        service
            .start_run(&attempt_for("task-1", &pkg_a), &first, &session)
            .await,
        Err(TaskServiceError::HarnessMismatch { .. })
    ));
}

#[tokio::test]
async fn queues_and_callbacks_never_cross_task_or_harness_identity() {
    let journal = Arc::new(MemoryJournal::new());
    let service = TaskService::new(journal.clone());
    let session = FakeHarnessSession::default();
    service
        .create_task(contract("task-a"))
        .await
        .expect("create a");
    service
        .create_task(contract("task-b"))
        .await
        .expect("create b");

    let pkg_a = package("harness.a", "sha-a");
    service
        .pin_harness("task-a", pkg_a.clone())
        .await
        .expect("pin a");
    service
        .enqueue("task-a", InputKind::User, "for-a", None)
        .await
        .expect("enqueue a");
    service
        .enqueue("task-b", InputKind::User, "for-b", None)
        .await
        .expect("enqueue b");

    // Deliveries stay per task.
    let a_message = service.poll("task-a").await.expect("a");
    let b_message = service.poll("task-b").await.expect("b");
    assert_eq!(a_message.text, "for-a");
    assert_eq!(b_message.text, "for-b");
    assert_ne!(a_message.message_id, b_message.message_id);

    // Harness identity never crosses: an attempt pinned for task-b on
    // harness-a is refused even though the same package is installed.
    assert!(matches!(
        service
            .start_run(&attempt_for("task-b", &pkg_a), &b_message, &session)
            .await,
        Err(TaskServiceError::HarnessMismatch { .. })
    ));

    // Branch creation inherits the contract and work units, not queues.
    let mut source = journal.load_task("task-a").await.expect("load a");
    source.work_units = vec![WorkUnit {
        id: "u1".into(),
        description: "unit".into(),
        dependencies: vec![],
        acceptance: vec![],
        status: WorkUnitStatus::Pending,
    }];
    journal
        .save_task_and_events(&source, vec![])
        .await
        .expect("save source");
    let branch = service
        .create_branch("task-a", "task-a2")
        .await
        .expect("branch");
    assert_eq!(branch.contract.objective, "objective");
    assert_eq!(branch.work_units.len(), 1);
    assert_eq!(service.branch_parent("task-a2").as_deref(), Some("task-a"));
    // The branch's queue starts empty: task-a's pending input did not leak.
    service.reload("task-a2").await.expect("reload branch");
    assert!(service.poll("task-a2").await.is_none());
}
