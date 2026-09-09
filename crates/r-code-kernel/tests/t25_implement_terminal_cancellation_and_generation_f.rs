//! T25 — terminal cancellation and generation fencing.
//!
//! Race tests cover late callbacks, plugin ignoring cancellation, queued
//! next message and exactly one final outcome; unproven termination keeps
//! the lease held.

use r_code_harness_protocol::services::{ChildrenSpawnRequest, PermissionCeiling};
use r_code_kernel::cancellation::*;
use r_code_kernel::children::ChildrenSupervisor;
use r_code_kernel::ports::{RunGuard, ServiceError};
use r_code_kernel::task::*;

struct Proven(bool);

impl TerminationProver for Proven {
    fn proven(&self, _run_id: &str) -> Result<bool, String> {
        Ok(self.0)
    }
}

fn supervisor_with_children(count: usize) -> ChildrenSupervisor {
    let mut supervisor = ChildrenSupervisor::new();
    for _ in 0..count {
        supervisor
            .spawn(
                PermissionCeiling::Full,
                &ChildrenSpawnRequest {
                    objective: "o".into(),
                    harness: None,
                    permissions: PermissionCeiling::ReadOnly,
                    budget_share: None,
                },
            )
            .expect("spawn");
    }
    supervisor
}

#[test]
fn cancellation_runs_in_strict_fence_order() {
    let guard = RunGuard::new("run-1", 1);
    let mut coordinator = CancellationCoordinator::new(guard.clone())
        .with_children(supervisor_with_children(2))
        .with_prover(Box::new(Proven(true)));

    let steps = coordinator.cancel("run-1", "user asked").expect("cancel");
    assert_eq!(
        steps,
        vec![
            CancellationStep::GenerationRevoked {
                run_id: "run-1".into(),
                generation: 1
            },
            CancellationStep::ChildrenCancelled { count: 2 },
            CancellationStep::WorkStopped,
            CancellationStep::TerminationConfirmed,
            CancellationStep::LeaseReleasable,
        ]
    );
    // Children were cancelled by the cascade.
    assert!(coordinator
        .supervisor
        .as_ref()
        .expect("supervisor")
        .live_children()
        .is_empty());
    // The generation is revoked: late callbacks fail closed.
    assert!(matches!(
        guard.check(&r_code_kernel::ports::GenerationToken {
            run_id: "run-1".into(),
            generation: 1
        }),
        Err(ServiceError::Cancelled)
    ));
}

#[test]
fn unproven_termination_keeps_the_lease_held() {
    let guard = RunGuard::new("run-1", 1);
    let mut coordinator = CancellationCoordinator::new(guard).with_prover(Box::new(Proven(false)));
    let error = coordinator
        .cancel("run-1", "kill daemon")
        .expect_err("unproven");
    match error {
        CancellationError::UnprovenTermination(reason) => {
            assert!(reason.contains("cannot be proven terminated"), "{reason}");
            assert!(reason.contains("lease stays held"), "{reason}");
        }
        other => panic!("expected unproven, got {other:?}"),
    }
    // The coordinator never reached LeaseReleasable; ownership uncertainty
    // stays blocked.
}

#[test]
fn plugins_ignoring_cancellation_still_end_terminal() {
    // The transport-level kill after the grace period (T09) covers
    // ignoring plugins; here the kernel side: finalization is forced
    // host-side even when the plugin never acknowledged.
    let mut state = TaskState::new(TaskContract {
        task_id: "task-1".into(),
        kind: TaskKind::Conversation,
        objective: "o".into(),
        constraints: vec![],
        required_checks: vec![],
        revision: 1,
    });
    state
        .start_attempt(&Attempt {
            attempt_id: "a1".into(),
            task_id: "task-1".into(),
            branch_id: "b".into(),
            package: r_code_harness_protocol::PackageRef {
                id: r_code_harness_protocol::HarnessId::new("h"),
                version: semver::Version::new(1, 0, 0),
                content_digest: "s".into(),
            },
            contract_revision: 1,
            config_hash: "c".into(),
            workspace_identity: "w".into(),
            run_id: "r".into(),
        })
        .expect("start");

    // The plugin tries to finalize afterwards: refused (plugins never can).
    assert!(matches!(
        state.finalize(
            Actor::Plugin,
            TaskVerdict::Verified {
                candidate_digest: "x".into()
            }
        ),
        Err(TransitionError::PluginVerdictRejected)
    ));

    // Host finalization produces exactly one terminal outcome; further
    // transitions (including another finalize) are refused.
    let verdict = finalize_cancelled(&mut state, 1, "plugin ignored cancel").expect("final");
    assert!(matches!(verdict, TaskVerdict::Cancelled { .. }));
    assert!(matches!(
        finalize_cancelled(&mut state, 1, "second attempt"),
        Err(TransitionError::AlreadyTerminal { .. })
    ));
    assert!(matches!(
        state.record_evidence(r_code_kernel::task::EvidenceRecord {
            evidence_id: "late".into(),
            check_id: "c".into(),
            candidate_digest: "d".into(),
            environment: "e".into(),
            passed: true,
            host_output: None,
            recorded_by: r_code_harness_protocol::Provenance::Host,
        }),
        Err(TransitionError::AlreadyTerminal { .. })
    ));
}

#[test]
fn queued_messages_survive_for_the_next_run() {
    // A message queued during cancellation never reaches the revoked
    // generation, but is held for the successor run.
    assert_eq!(
        classify_queued_message(true),
        QueuedMessageDisposition::HeldForNextRun
    );
    assert_eq!(
        classify_queued_message(false),
        QueuedMessageDisposition::DroppedForRevokedGeneration
    );

    // The queue service itself keeps undelivered messages across the
    // generation boundary (T06 semantics): simulate a queued message and a
    // cancelled run.
    let journal = std::sync::Arc::new(r_code_kernel::testing::MemoryJournal::new());
    let service = r_code_kernel::tasks::TaskService::new(journal);
    futures_block(async {
        service
            .create_task(TaskContract {
                task_id: "task-1".into(),
                kind: TaskKind::Conversation,
                objective: "o".into(),
                constraints: vec![],
                required_checks: vec![],
                revision: 1,
            })
            .await
            .expect("create");
        let queued = service
            .enqueue(
                "task-1",
                r_code_harness_protocol::InputKind::User,
                "next turn",
                None,
            )
            .await
            .expect("queue");
        // The run's generation is revoked (cancellation) — the message is
        // still deliverable to the successor.
        let pending = service.reload("task-1").await.expect("reload");
        assert_eq!(pending, 1);
        let delivered = service.poll("task-1").await.expect("held");
        assert_eq!(delivered.message_id, queued.message_id);
    });
}

fn futures_block<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(future)
}
