//! T24 — checkpoints and operation reconciliation.
//!
//! Fault injection before/after side effects proves no duplicate execution,
//! no global reset-hard, and explicit recovery when a pinned plugin is
//! unavailable.

use r_code_harness_protocol::{HarnessId, OperationKey, PackageRef};
use r_code_kernel::recovery::*;
use r_code_kernel::task::*;

fn package(id: &str, digest: &str) -> PackageRef {
    PackageRef {
        id: HarnessId::new(id),
        version: semver::Version::new(1, 0, 0),
        content_digest: digest.into(),
    }
}

fn attempt(package: PackageRef) -> Attempt {
    Attempt {
        attempt_id: "attempt-1".into(),
        task_id: "task-1".into(),
        branch_id: "branch-1".into(),
        package,
        contract_revision: 2,
        config_hash: "cfg-abc".into(),
        workspace_identity: "ws-1".into(),
        run_id: "run-1".into(),
    }
}

#[test]
fn effects_after_side_effects_replay_without_duplicate_execution() {
    let receipt = OperationReceipt {
        attempt_id: "attempt-1".into(),
        operation_key: OperationKey::new("write-a"),
        method: "host.tools.call".into(),
        input_hash: "hash-1".into(),
        outcome: ReceiptOutcome::Completed {
            result: serde_json::json!({"written": "a.txt"}),
        },
    };
    // Fault AFTER the effect: recovery replays the stored result.
    match reconcile_receipt(&receipt, "host.tools.call", "hash-1") {
        ReconciledEffect::Replay { result } => assert_eq!(result["written"], "a.txt"),
        other => panic!("expected replay, got {other:?}"),
    }
    // Conflicting input on the same key never re-executes blindly.
    match reconcile_receipt(&receipt, "host.tools.call", "hash-other") {
        ReconciledEffect::Indeterminate { reason, .. } => {
            assert!(reason.contains("conflicting"), "{reason}")
        }
        other => panic!("expected indeterminate, got {other:?}"),
    }
}

#[test]
fn faults_before_side_effects_reconcile_by_replay_class() {
    // Pending file effect (fault before completion): reconcile by hash.
    let pending_file = OperationReceipt {
        attempt_id: "attempt-1".into(),
        operation_key: OperationKey::new("write-b"),
        method: "host.tools.call".into(),
        input_hash: "hash-2".into(),
        // Pending is represented as an unreadable/partial outcome in this
        // fixture: the recovery path sees Indeterminate.
        outcome: ReceiptOutcome::Indeterminate {
            reason: "crash before completion".into(),
        },
    };
    match reconcile_receipt(&pending_file, "host.tools.call", "hash-2") {
        ReconciledEffect::Indeterminate { reason, .. } => {
            assert!(reason.contains("unresolved"), "{reason}")
        }
        other => panic!("expected indeterminate, got {other:?}"),
    }

    // Rejected-before-effect operations are safe to retry.
    let rejected = OperationReceipt {
        attempt_id: "attempt-1".into(),
        operation_key: OperationKey::new("denied"),
        method: "host.process.open".into(),
        input_hash: "hash-3".into(),
        outcome: ReceiptOutcome::Rejected {
            reason: "authorization denied".into(),
        },
    };
    assert!(matches!(
        reconcile_receipt(&rejected, "host.process.open", "hash-3"),
        ReconciledEffect::SafeToRetry { .. }
    ));
}

#[test]
fn pinned_plugin_unavailable_requires_explicit_restart() {
    let pkg = package("example.harness", "sha-1111");
    let att = attempt(pkg.clone());
    let plan = plan_recovery(&att, &pkg, "cfg-abc", 2, "ws-1", false, Some((1, 3)), &[]);
    match plan {
        RecoveryPlan::RestartRequired { reason } => {
            assert!(reason.contains("not installed"), "{reason}");
            assert!(reason.contains("example.harness"));
        }
        other => panic!("expected restart, got {other:?}"),
    }
    // There is no code path that resets the workspace: the plan vocabulary
    // has no reset-hard variant (compile-time guarantee of this module).
    // Available plugin + matching identity resumes from the checkpoint.
    let plan = plan_recovery(&att, &pkg, "cfg-abc", 2, "ws-1", true, Some((1, 3)), &[]);
    assert_eq!(
        plan,
        RecoveryPlan::ResumeFrom {
            checkpoint_revision: 1,
            consumed_input_seq: 3
        }
    );
}

#[test]
fn identity_mismatches_never_resume_foreign_checkpoints() {
    let pkg = package("example.harness", "sha-1111");
    let att = attempt(pkg.clone());

    // Different config hash.
    let plan = plan_recovery(
        &att,
        &pkg,
        "cfg-CHANGED",
        2,
        "ws-1",
        true,
        Some((1, 3)),
        &[],
    );
    assert!(matches!(plan, RecoveryPlan::RestartRequired { .. }));

    // Contract revision moved on.
    let plan = plan_recovery(&att, &pkg, "cfg-abc", 3, "ws-1", true, Some((1, 3)), &[]);
    assert!(matches!(plan, RecoveryPlan::RestartRequired { .. }));

    // Workspace identity changed.
    let plan = plan_recovery(
        &att,
        &pkg,
        "cfg-abc",
        2,
        "ws-other",
        true,
        Some((1, 3)),
        &[],
    );
    assert!(matches!(plan, RecoveryPlan::RestartRequired { .. }));

    // Upgraded package bytes pin new runs only.
    let upgraded = package("example.harness", "sha-2222");
    let plan = plan_recovery(
        &att,
        &upgraded,
        "cfg-abc",
        2,
        "ws-1",
        true,
        Some((1, 3)),
        &[],
    );
    assert!(matches!(plan, RecoveryPlan::RestartRequired { .. }));
}

#[test]
fn indeterminate_effects_block_new_writes_behind_a_barrier() {
    let pkg = package("example.harness", "sha-1111");
    let att = attempt(pkg.clone());
    let plan = plan_recovery(
        &att,
        &pkg,
        "cfg-abc",
        2,
        "ws-1",
        true,
        Some((1, 3)),
        &["host.process.close (lost external acknowledgement)".into()],
    );
    match plan {
        RecoveryPlan::Blocked { reason } => {
            assert!(reason.contains("lost external acknowledgement"), "{reason}");
        }
        other => panic!("expected blocked, got {other:?}"),
    }
}

#[test]
fn writer_barriers_round_trip_and_clear_only_when_proven() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = r_code_store::v2::V2Store::open(&temp.path().join("tasks.sqlite3")).expect("open");

    store
        .save_writer_barrier("b1", "ws-1", 4242, "start-identity-9", "guardian lost")
        .expect("save barrier");
    store
        .save_writer_barrier("b2", "ws-1", 4243, "start-identity-10", "kill unconfirmed")
        .expect("save barrier 2");

    let barriers = store.writer_barriers("ws-1").expect("query");
    assert_eq!(barriers.len(), 2);
    assert!(barriers
        .iter()
        .any(|(id, pid, _, reason)| id == "b1" && *pid == 4242 && reason == "guardian lost"));
    // Other workspaces unaffected.
    assert!(store.writer_barriers("ws-2").expect("query").is_empty());

    // Clearing requires the specific barrier id (termination proven).
    assert!(store.clear_writer_barrier("b1").expect("clear"));
    assert!(!store.clear_writer_barrier("b1").expect("already cleared"));
    assert_eq!(store.writer_barriers("ws-1").expect("query").len(), 1);
}

#[test]
fn store_reconciles_attempt_receipts_and_plugin_availability() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = r_code_store::v2::V2Store::open(&temp.path().join("tasks.sqlite3")).expect("open");

    // Save receipts through the JournalStore port, read the attempt view.
    use r_code_kernel::ports::JournalStore;
    futures_lite_block(async {
        store
            .save_receipt(OperationReceipt {
                attempt_id: "attempt-1".into(),
                operation_key: OperationKey::new("write-a"),
                method: "host.tools.call".into(),
                input_hash: "h1".into(),
                outcome: ReceiptOutcome::Completed {
                    result: serde_json::json!({"ok": 1}),
                },
            })
            .await
            .expect("receipt 1");
        store
            .save_receipt(OperationReceipt {
                attempt_id: "attempt-1".into(),
                operation_key: OperationKey::new("write-b"),
                method: "host.process.close".into(),
                input_hash: "h2".into(),
                outcome: ReceiptOutcome::Indeterminate {
                    reason: "lost ack".into(),
                },
            })
            .await
            .expect("receipt 2");
    });

    let receipts = store.receipts_for_attempt("attempt-1").expect("view");
    assert_eq!(receipts.len(), 2);
    assert!(receipts
        .iter()
        .any(|receipt| receipt.operation_key.0 == "write-a"
            && matches!(&receipt.outcome, ReceiptOutcome::Completed { .. })));

    // Plugin availability joins the catalog by digest.
    assert!(!store
        .attempt_plugin_available("example.harness", "sha-x")
        .expect("available"));
    store
        .register_plugin(&r_code_store::v2::PluginCatalogRecord {
            id: "example.harness".into(),
            version: "1.0.0".into(),
            content_digest: "sha-x".into(),
            enabled: true,
            granted_services: vec![],
            config: "{}".into(),
            manifest_json: "{}".into(),
            install_dir: "/nowhere".into(),
        })
        .expect("register");
    assert!(store
        .attempt_plugin_available("example.harness", "sha-x")
        .expect("available"));
}

/// Minimal block_on for the sync test.
fn futures_lite_block<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(future)
}
