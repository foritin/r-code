//! P25 — durable process-effect operations: fenced, monotonic, recoverable.
//!
//! Proves the acceptance set on a real store: the complete launch material
//! is durable BEFORE anything resumes (a byte-identical replay converges,
//! any divergence conflicts), recovery reads the frozen command and never
//! re-runs it, a stale owner cannot advance any state, and quarantine holds
//! until the terminal receipt — with every state surviving a crash-reopen.

use r_code_store::v1::operations::PrepareProcessTree;
use r_code_store::v1::{ProcessEffectError, ProcessEffectPrepare, ProcessEffectState, V1Store};
use serde_json::{json, Value};
use std::path::PathBuf;

const BOOT_A: &str = "windows:01234567-89ab-4cde-8f01-23456789abcd";

fn database_path(temp: &tempfile::TempDir) -> PathBuf {
    temp.path().join("store.db")
}

fn digest(value: &Value) -> String {
    r_code_harness_protocol::canonical_input_hash(value)
}

fn owner(seed: u32) -> r_code_store::v1::operations::ProcessTreeOwner {
    let platform_identity = json!({
        "nativePid": 20_000 + seed,
        "startToken": format!("start-{seed}"),
    });
    r_code_store::v1::operations::ProcessTreeOwner {
        pid: 2_000 + seed,
        start_identity: 20_000 + u64::from(seed),
        boot_identity: BOOT_A.to_string(),
        platform_identity_digest: digest(&platform_identity),
        platform_identity,
    }
}

fn seed_tree(store: &V1Store, tree_id: &str, workspace_key: &str, seed: u32) {
    store
        .prepare_process_tree(&PrepareProcessTree {
            tree_id: tree_id.to_string(),
            attempt_id: format!("attempt-{tree_id}"),
            workspace_key: workspace_key.to_string(),
            profile_id: "s25".to_string(),
            owner: owner(seed),
        })
        .expect("seed process tree");
}

fn prepare_input(operation_id: &str, tree_id: &str, epoch: u64) -> ProcessEffectPrepare {
    ProcessEffectPrepare {
        operation_id: operation_id.to_string(),
        tree_id: tree_id.to_string(),
        attempt_id: format!("attempt-{tree_id}"),
        workspace_key: "workspace-s25".to_string(),
        owner_id: format!("owner-{operation_id}"),
        fencing_epoch: epoch,
        command_json: json!({
            "program": "C:/bin/tool.exe",
            "arguments": ["--check", "--locked"],
            "cwd": "C:/scratch/s25",
            "environmentKeys": ["PATH", "TEMP"]
        })
        .to_string(),
        lease_json: json!({"leaseId": format!("lease-{operation_id}"), "mode": "write"})
            .to_string(),
        before_manifest_json: json!({
            "files": [
                {"path": "src/lib.rs", "sha256": "aa", "size": 10},
                {"path": "src/main.rs", "sha256": "bb", "size": 20}
            ]
        })
        .to_string(),
        before_manifest_digest: digest(&json!({"manifest": "before-v1"})),
        scan_policy_json: json!({"roots": ["src"], "ignore": ["/target"]}).to_string(),
        ephemeral_roots_json: json!({"roots": ["C:/scratch/s25/tmp"]}).to_string(),
    }
}

#[test]
fn prepare_persists_before_resume_and_replays_converge() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = V1Store::open(&database_path(&temp)).expect("store");
    seed_tree(&store, "tree-a", "workspace-s25", 1);

    let record = store
        .prepare_process_effect(prepare_input("op-1", "tree-a", 7))
        .expect("prepare journals the launch material");
    assert_eq!(record.state, ProcessEffectState::Prepared);
    assert_eq!(record.fencing_epoch, 7);
    assert_eq!(record.state_revision, 1);
    assert!(record.receipt_digest.is_none());

    // A byte-identical replay converges on the same row.
    let replay = store
        .prepare_process_effect(prepare_input("op-1", "tree-a", 7))
        .expect("identical replay converges");
    assert_eq!(replay, record);

    // Any divergence is a conflict: the id never carries two materials.
    let mut divergent = prepare_input("op-1", "tree-a", 7);
    divergent.command_json = json!({"program": "C:/evil.exe"}).to_string();
    assert_eq!(
        store.prepare_process_effect(divergent).unwrap_err(),
        ProcessEffectError::OperationConflict
    );

    // Garbage is refused BEFORE it can become durable.
    let mut garbage = prepare_input("op-garbage", "tree-a", 7);
    garbage.scan_policy_json = "not json".to_string();
    assert!(store.prepare_process_effect(garbage).is_err());
    assert!(store
        .load_process_effect("op-garbage")
        .expect("load")
        .is_none());
}

#[test]
fn resume_requires_durable_state_and_recovery_never_reruns_the_command() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = V1Store::open(&database_path(&temp)).expect("store");
    seed_tree(&store, "tree-b", "workspace-s25", 2);

    // Nothing advances an operation that was never journaled.
    assert_eq!(
        store
            .mark_process_effect_running("op-missing", "owner-x", 1)
            .unwrap_err(),
        ProcessEffectError::OperationNotFound
    );

    let frozen = store
        .prepare_process_effect(prepare_input("op-2", "tree-b", 3))
        .expect("prepare");
    let running = store
        .mark_process_effect_running("op-2", &frozen.owner_id, 3)
        .expect("running after durable before state");
    assert_eq!(running.state, ProcessEffectState::Running);
    assert_eq!(running.state_revision, 2);

    // Running is idempotent: the replay converges, no extra revision.
    let replay = store
        .mark_process_effect_running("op-2", &frozen.owner_id, 3)
        .expect("running replay converges");
    assert_eq!(replay.state, ProcessEffectState::Running);
    assert_eq!(replay.state_revision, 2);

    // Recovery reads the frozen command verbatim from a reopened store: the
    // incomplete query is the only command source and it never changed.
    let reopened = V1Store::open(&database_path(&temp)).expect("reopen");
    let incomplete = reopened.incomplete_process_effects().expect("incomplete");
    assert_eq!(incomplete.len(), 1);
    assert_eq!(incomplete[0].operation_id, "op-2");
    assert_eq!(incomplete[0].command_json, frozen.command_json);
    assert_eq!(
        incomplete[0].before_manifest_digest,
        frozen.before_manifest_digest
    );
}

#[test]
fn stale_owner_cannot_advance_any_state() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = V1Store::open(&database_path(&temp)).expect("store");
    seed_tree(&store, "tree-c", "workspace-s25", 3);
    let record = store
        .prepare_process_effect(prepare_input("op-3", "tree-c", 5))
        .expect("prepare");

    // Wrong owner, right epoch — refused from Prepared.
    assert_eq!(
        store
            .mark_process_effect_running("op-3", "owner-imposter", 5)
            .unwrap_err(),
        ProcessEffectError::StaleOwner
    );
    // Right owner, wrong epoch — refused from Prepared.
    assert_eq!(
        store
            .mark_process_effect_running("op-3", &record.owner_id, 4)
            .unwrap_err(),
        ProcessEffectError::StaleOwner
    );
    // And the same fence holds from Running for the receipt and quarantine.
    store
        .mark_process_effect_running("op-3", &record.owner_id, 5)
        .expect("running");
    assert_eq!(
        store
            .record_process_effect_receipt("op-3", "owner-imposter", 5, "receipt-digest")
            .unwrap_err(),
        ProcessEffectError::StaleOwner
    );
    assert_eq!(
        store
            .quarantine_process_effect("op-3", "owner-imposter", 5, "sweep-unproven")
            .unwrap_err(),
        ProcessEffectError::StaleOwner
    );
    // Nothing moved: still Running.
    assert_eq!(
        store
            .load_process_effect("op-3")
            .expect("load")
            .expect("present")
            .state,
        ProcessEffectState::Running
    );
}

#[test]
fn quarantine_lasts_through_receipt() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = V1Store::open(&database_path(&temp)).expect("store");
    seed_tree(&store, "tree-d", "workspace-s25", 4);
    seed_tree(&store, "tree-empty", "workspace-s25", 5);

    // No operation at all: the quarantine stays in force.
    assert!(!store
        .tree_quarantine_lift_eligible("tree-empty")
        .expect("eligible"));

    let record = store
        .prepare_process_effect(prepare_input("op-4", "tree-d", 2))
        .expect("prepare");
    // Prepared (no receipt yet): not eligible.
    assert!(!store
        .tree_quarantine_lift_eligible("tree-d")
        .expect("eligible"));

    store
        .mark_process_effect_running("op-4", &record.owner_id, 2)
        .expect("running");
    // Running (no receipt yet): still not eligible.
    assert!(!store
        .tree_quarantine_lift_eligible("tree-d")
        .expect("eligible"));

    // The receipt pins its digest; a different digest never replaces it.
    let receipted = store
        .record_process_effect_receipt("op-4", &record.owner_id, 2, "receipt-digest-a")
        .expect("receipt");
    assert_eq!(receipted.state, ProcessEffectState::Receipted);
    assert_eq!(
        receipted.receipt_digest.as_deref(),
        Some("receipt-digest-a")
    );
    let replay = store
        .record_process_effect_receipt("op-4", &record.owner_id, 2, "receipt-digest-a")
        .expect("same-digest replay converges");
    assert_eq!(replay.state_revision, receipted.state_revision);
    assert_eq!(
        store
            .record_process_effect_receipt("op-4", &record.owner_id, 2, "receipt-digest-b")
            .unwrap_err(),
        ProcessEffectError::InvalidTransition {
            expected: ProcessEffectState::Running,
            actual: ProcessEffectState::Receipted
        }
    );
    // Receipted is terminal: no quarantine downgrade after the receipt.
    assert_eq!(
        store
            .quarantine_process_effect("op-4", &record.owner_id, 2, "late-sweep")
            .unwrap_err(),
        ProcessEffectError::InvalidTransition {
            expected: ProcessEffectState::Prepared,
            actual: ProcessEffectState::Receipted
        }
    );
    // The receipt is what lifts the quarantine.
    assert!(store
        .tree_quarantine_lift_eligible("tree-d")
        .expect("eligible"));

    // A quarantined operation never becomes receipted and never lifts.
    let record = store
        .prepare_process_effect(prepare_input("op-5", "tree-d", 6))
        .expect("prepare");
    let quarantined = store
        .quarantine_process_effect("op-5", &record.owner_id, 6, "tree-escape-unproven")
        .expect("quarantine from prepared");
    assert_eq!(quarantined.state, ProcessEffectState::Quarantined);
    assert_eq!(
        quarantined.quarantine_reason.as_deref(),
        Some("tree-escape-unproven")
    );
    assert_eq!(
        store
            .record_process_effect_receipt("op-5", &record.owner_id, 6, "digest")
            .unwrap_err(),
        ProcessEffectError::InvalidTransition {
            expected: ProcessEffectState::Running,
            actual: ProcessEffectState::Quarantined
        }
    );
    assert!(!store
        .tree_quarantine_lift_eligible("tree-d")
        .expect("eligible"));

    // Quarantine is legal from RUNNING too: a sweep that cannot prove the
    // tree while it runs quarantines mid-flight, with the same fence.
    let running = store
        .prepare_process_effect(prepare_input("op-6", "tree-d", 8))
        .expect("prepare");
    store
        .mark_process_effect_running("op-6", &running.owner_id, 8)
        .expect("running");
    let midflight = store
        .quarantine_process_effect("op-6", &running.owner_id, 8, "mid-flight-unproven")
        .expect("quarantine from running");
    assert_eq!(midflight.state, ProcessEffectState::Quarantined);
    assert_eq!(
        midflight.quarantine_reason.as_deref(),
        Some("mid-flight-unproven")
    );
    assert!(!store
        .tree_quarantine_lift_eligible("tree-d")
        .expect("eligible"));
}

/// One state to crash in, and the advance steps that reach it.
enum Step {
    Run,
    Receipt(&'static str),
    Quarantine(&'static str),
}

fn apply_steps(store: &V1Store, id: &str, owner: &str, epoch: u64, steps: &[Step]) {
    for step in steps {
        match step {
            Step::Run => {
                store
                    .mark_process_effect_running(id, owner, epoch)
                    .expect("running");
            }
            Step::Receipt(digest) => {
                store
                    .record_process_effect_receipt(id, owner, epoch, digest)
                    .expect("receipt");
            }
            Step::Quarantine(reason) => {
                store
                    .quarantine_process_effect(id, owner, epoch, reason)
                    .expect("quarantine");
            }
        }
    }
}

#[test]
fn every_state_survives_a_crash_reopen() {
    let cases: &[(&str, ProcessEffectState, &[Step])] = &[
        ("op-crash-prepared", ProcessEffectState::Prepared, &[]),
        (
            "op-crash-running",
            ProcessEffectState::Running,
            &[Step::Run],
        ),
        (
            "op-crash-receipted",
            ProcessEffectState::Receipted,
            &[Step::Run, Step::Receipt("crash-digest")],
        ),
        (
            "op-crash-quarantined",
            ProcessEffectState::Quarantined,
            &[Step::Quarantine("crash-before-proof")],
        ),
    ];
    for (operation_id, expected, steps) in cases {
        let temp = tempfile::tempdir().expect("tempdir");
        let tree_id = format!("tree-{operation_id}");
        let store = V1Store::open(&database_path(&temp)).expect("store");
        seed_tree(&store, &tree_id, "workspace-s25", 6);
        let record = store
            .prepare_process_effect(prepare_input(operation_id, &tree_id, 9))
            .expect("prepare");
        apply_steps(&store, operation_id, &record.owner_id, 9, steps);
        drop(store);

        // The crash: reopen and find the exact state and frozen material.
        let reopened = V1Store::open(&database_path(&temp)).expect("reopen");
        let survived = reopened
            .load_process_effect(operation_id)
            .expect("load")
            .expect("the row survived the crash");
        assert_eq!(&survived.state, expected, "{operation_id} state");
        assert_eq!(survived.command_json, record.command_json);
        assert_eq!(
            survived.before_manifest_digest,
            record.before_manifest_digest
        );
        assert_eq!(survived.owner_id, record.owner_id);
        // The incomplete query only ever reports prepared/running rows.
        let incomplete_ids: Vec<String> = reopened
            .incomplete_process_effects()
            .expect("incomplete")
            .into_iter()
            .map(|row| row.operation_id)
            .collect();
        let expected_incomplete = matches!(
            expected,
            ProcessEffectState::Prepared | ProcessEffectState::Running
        );
        assert_eq!(
            incomplete_ids.contains(&operation_id.to_string()),
            expected_incomplete,
            "{operation_id} incomplete membership"
        );
    }
}
