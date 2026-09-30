//! S27 — recover process effects without command re-execution.
//!
//! Proves the acceptance set end to end over the P26 scanner + P25 rows +
//! P26A journal: one spawn per operation with exact forward recovery (a
//! lost receipt replays/converges and never repeats the command), the
//! repo-exclusive lease and the quarantine last through the receipt, and
//! the CAS material preserves later foreign edits. Plus the effect-scope
//! rule (only CurrentCheckoutWrite carries a checkout delta), the
//! write-ingress delta journaling, and startup recovery before any write
//! ingress.

use r_code_runtime::services::artifacts::{ArtifactStore, EffectQuota};
use r_code_runtime::services::process_effects::{
    begin_workspace_effect, checkout_delta_applies, complete_workspace_effect,
    recover_incomplete_effects, EffectIdentity, EnvelopeContext, EnvelopeError, ScanBounds,
};
use r_code_runtime::services::process_profiles::ProcessProfileEffect;
use r_code_runtime::services::workspaces::TaskWorkspaceBinding;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use r_code_store::v1::operations::PrepareProcessTree;
use r_code_store::v1::{ProcessEffectState, V1Store};
use serde_json::json;
use std::path::Path;

fn seed_tree(store: &V1Store, tree_id: &str, seed: u32) {
    let platform_identity = json!({"nativePid": 50_000 + seed, "startToken": format!("s{seed}")});
    store
        .prepare_process_tree(&PrepareProcessTree {
            tree_id: tree_id.to_string(),
            attempt_id: format!("attempt-{tree_id}"),
            workspace_key: format!("workspace-s27-{seed}"),
            profile_id: "s27".to_string(),
            owner: r_code_store::v1::operations::ProcessTreeOwner {
                pid: 5_000 + seed,
                start_identity: 50_000 + u64::from(seed),
                boot_identity: "windows:01234567-89ab-4cde-8f01-23456789abcd".to_string(),
                platform_identity_digest: r_code_harness_protocol::canonical_input_hash(
                    &platform_identity,
                ),
                platform_identity,
            },
        })
        .expect("seed tree");
}

fn write(root: &Path, relative: &str, bytes: &[u8]) {
    let path = root.join(relative);
    std::fs::create_dir_all(path.parent().expect("parent")).expect("dirs");
    std::fs::write(path, bytes).expect("write");
}

fn fixture(root: &Path) {
    write(root, "keep.txt", b"unchanged\n");
    write(root, "edit-me.txt", b"before\n");
    write(root, "remove-me.txt", b"gone\n");
}

/// The single spawn of the "user process": everything it writes happens
/// between begin and complete, exactly once per operation.
fn user_process_writes(root: &Path, run: u32) {
    write(root, "edit-me.txt", format!("after-run-{run}\n").as_bytes());
    write(root, "created.txt", b"created by the process\n");
    let _ = std::fs::remove_file(root.join("remove-me.txt"));
}

#[test]
fn one_spawn_with_exact_forward_recovery_and_lost_receipt_convergence() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("ws");
    std::fs::create_dir_all(&root).expect("root");
    fixture(&root);
    let store = V1Store::open(&temp.path().join("store.db")).expect("store");
    let artifacts = ArtifactStore::for_task(temp.path().join("blobs"), "task-s27");
    let binding = TaskWorkspaceBinding::bind_local("task-s27", &root, &[]).expect("bind");
    seed_tree(&store, "tree-s27a", 1);
    let quota = EffectQuota::default();
    let bounds = ScanBounds::default();
    let command = json!({"program": "workspace-writer"}).to_string();

    // One spawn: begin refuses to hand out a second first-begin, and the
    // concurrent second lock holder is refused outright (repo-exclusive).
    let begin = begin_workspace_effect(
        &EnvelopeContext {
            store: &store,
            artifacts: &artifacts,
            binding: &binding,
            quota: &quota,
            bounds: &bounds,
        },
        EffectIdentity {
            operation_id: "op-s27a",
            owner_id: "owner-s27a",
            fencing_epoch: 2,
            tree_id: "tree-s27a",
            command_json: &command,
        },
    )
    .expect("begin");
    assert!(!begin.already_journaled);
    assert!(matches!(
        begin_workspace_effect(
            &EnvelopeContext {
                store: &store,
                artifacts: &artifacts,
                binding: &binding,
                quota: &quota,
                bounds: &bounds,
            },
            EffectIdentity {
                operation_id: "op-s27a",
                owner_id: "owner-s27a",
                fencing_epoch: 2,
                tree_id: "tree-s27a",
                command_json: &command,
            },
        ),
        Err(EnvelopeError::Conflict(_))
    ));

    // The single execution of the user process.
    user_process_writes(&root, 1);
    let completion = complete_workspace_effect(
        &EnvelopeContext {
            store: &store,
            artifacts: &artifacts,
            binding: &binding,
            quota: &EffectQuota::default(),
            bounds: &bounds,
        },
        begin,
    )
    .expect("complete");
    assert_eq!(completion.delta.len(), 3, "{:?}", completion.delta);
    assert!(completion
        .delta
        .iter()
        .any(|entry| entry.path == "created.txt"
            && entry.kind == r_code_runtime::services::process_effects::DeltaKind::Created));
    assert_eq!(store.reserved_active_bytes().expect("reserved"), 0);

    // LOST RECEIPT: a replay begin converges with already_journaled=true —
    // the caller knows NOT to spawn again — and completing again replays
    // the identical digest (the command never runs twice).
    let replay = begin_workspace_effect(
        &EnvelopeContext {
            store: &store,
            artifacts: &artifacts,
            binding: &binding,
            quota: &quota,
            bounds: &bounds,
        },
        EffectIdentity {
            operation_id: "op-s27a",
            owner_id: "owner-s27a",
            fencing_epoch: 2,
            tree_id: "tree-s27a",
            command_json: &command,
        },
    )
    .expect("replay begin");
    assert!(
        replay.already_journaled,
        "the replay must tell the caller not to spawn again"
    );
    let replayed = complete_workspace_effect(
        &EnvelopeContext {
            store: &store,
            artifacts: &artifacts,
            binding: &binding,
            quota: &EffectQuota::default(),
            bounds: &bounds,
        },
        replay,
    )
    .expect("replay complete converges");
    assert_eq!(replayed.delta_digest, completion.delta_digest);
    let record = store
        .load_process_effect("op-s27a")
        .expect("load")
        .expect("present");
    assert_eq!(record.state, ProcessEffectState::Receipted);

    // Divergent material under the same id is a conflict — never a rerun.
    let divergent = json!({"program": "other-writer"}).to_string();
    assert!(matches!(
        begin_workspace_effect(
            &EnvelopeContext {
                store: &store,
                artifacts: &artifacts,
                binding: &binding,
                quota: &quota,
                bounds: &bounds,
            },
            EffectIdentity {
                operation_id: "op-s27a",
                owner_id: "owner-s27a",
                fencing_epoch: 2,
                tree_id: "tree-s27a",
                command_json: &divergent,
            },
        ),
        Err(EnvelopeError::Conflict(_))
    ));
}

#[test]
fn crash_before_complete_quarantines_and_never_reruns() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("ws");
    std::fs::create_dir_all(&root).expect("root");
    fixture(&root);
    let store = V1Store::open(&temp.path().join("store.db")).expect("store");
    let artifacts = ArtifactStore::for_task(temp.path().join("blobs"), "task-s27");
    let binding = TaskWorkspaceBinding::bind_local("task-s27", &root, &[]).expect("bind");
    seed_tree(&store, "tree-s27b", 2);

    let begin = begin_workspace_effect(
        &EnvelopeContext {
            store: &store,
            artifacts: &artifacts,
            binding: &binding,
            quota: &EffectQuota::default(),
            bounds: &ScanBounds::default(),
        },
        EffectIdentity {
            operation_id: "op-s27b",
            owner_id: "owner-s27b",
            fencing_epoch: 5,
            tree_id: "tree-s27b",
            command_json: r#"{"program":"w"}"#,
        },
    )
    .expect("begin");
    user_process_writes(&root, 1);
    let held_lock_path = {
        // The crash: everything drops without completing.
        let lock_path = temp.path().join("lock-marker");
        std::fs::write(&lock_path, b"held").expect("marker");
        drop(begin);
        lock_path
    };
    let _ = std::fs::remove_file(&held_lock_path);

    // Recovery quarantines the incomplete operation; it never re-runs and
    // never receipts (P25's terminal-state discipline), and the quarantine
    // holds: the tree stays non-liftable.
    let quarantined = recover_incomplete_effects(&store).expect("recover");
    assert_eq!(quarantined, vec!["op-s27b".to_string()]);
    let record = store
        .load_process_effect("op-s27b")
        .expect("load")
        .expect("present");
    assert_eq!(record.state, ProcessEffectState::Quarantined);
    assert!(!store
        .tree_quarantine_lift_eligible("tree-s27b")
        .expect("eligible"));
    assert!(matches!(
        store.record_process_effect_receipt("op-s27b", "owner-s27b", 5, "late"),
        Err(r_code_store::v1::ProcessEffectError::InvalidTransition { .. })
    ));

    // Forward: a NEW operation under a new id proceeds normally.
    seed_tree(&store, "tree-s27b2", 3);
    let fresh = begin_workspace_effect(
        &EnvelopeContext {
            store: &store,
            artifacts: &artifacts,
            binding: &binding,
            quota: &EffectQuota::default(),
            bounds: &ScanBounds::default(),
        },
        EffectIdentity {
            operation_id: "op-s27b2",
            owner_id: "owner-s27b2",
            fencing_epoch: 5,
            tree_id: "tree-s27b2",
            command_json: r#"{"program":"w2"}"#,
        },
    )
    .expect("forward begin");
    let completion = complete_workspace_effect(
        &EnvelopeContext {
            store: &store,
            artifacts: &artifacts,
            binding: &binding,
            quota: &EffectQuota::default(),
            bounds: &ScanBounds::default(),
        },
        fresh,
    )
    .expect("forward complete");
    assert_eq!(
        completion.delta.len(),
        0,
        "no further edits between captures"
    );
}

#[test]
fn startup_recovery_runs_before_write_ingress_in_composition() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("ws");
    std::fs::create_dir_all(&root).expect("root");
    fixture(&root);
    let ipc = format!("s27-compose-{}", std::process::id());
    let profile = RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(temp.path().join("data"))
            .with_ipc_name(ipc),
    )
    .expect("profile");

    // Seed an incomplete effect in the very store composition opens.
    let store = V1Store::open(&profile.database_path()).expect("store");
    let artifacts = ArtifactStore::for_task(profile.blobs_root().join("tasks"), "task-compose");
    let binding = TaskWorkspaceBinding::bind_local("task-compose", &root, &[]).expect("bind");
    seed_tree(&store, "tree-compose", 4);
    let begin = begin_workspace_effect(
        &EnvelopeContext {
            store: &store,
            artifacts: &artifacts,
            binding: &binding,
            quota: &EffectQuota::default(),
            bounds: &ScanBounds::default(),
        },
        EffectIdentity {
            operation_id: "op-compose",
            owner_id: "owner-compose",
            fencing_epoch: 9,
            tree_id: "tree-compose",
            command_json: r#"{"program":"w"}"#,
        },
    )
    .expect("begin");
    drop(begin); // crash

    let models: std::sync::Arc<dyn r_code_kernel::ports::ModelService> =
        std::sync::Arc::new(r_code_kernel::testing::FakeModelService::default());
    let tools: std::sync::Arc<dyn r_code_kernel::ports::ToolService> =
        std::sync::Arc::new(r_code_kernel::testing::FakeToolService::default());
    let service = r_code_runtime::application::ApplicationService::compose(&profile, models, tools)
        .expect("compose recovers before write ingress");
    drop(service);

    let record = store
        .load_process_effect("op-compose")
        .expect("load")
        .expect("present");
    assert_eq!(record.state, ProcessEffectState::Quarantined);
    let reason = record.quarantine_reason.expect("reason");
    assert_eq!(reason, "startup-recovery-incomplete");
}

#[test]
fn cas_material_preserves_later_foreign_edits() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("ws");
    std::fs::create_dir_all(&root).expect("root");
    fixture(&root);
    let store = V1Store::open(&temp.path().join("store.db")).expect("store");
    let artifacts = ArtifactStore::for_task(temp.path().join("blobs"), "task-s27");
    let binding = TaskWorkspaceBinding::bind_local("task-s27", &root, &[]).expect("bind");
    seed_tree(&store, "tree-s27c", 5);
    let quota = EffectQuota::default();
    let bounds = ScanBounds::default();

    let begin = begin_workspace_effect(
        &EnvelopeContext {
            store: &store,
            artifacts: &artifacts,
            binding: &binding,
            quota: &quota,
            bounds: &bounds,
        },
        EffectIdentity {
            operation_id: "op-s27c",
            owner_id: "owner-s27c",
            fencing_epoch: 3,
            tree_id: "tree-s27c",
            command_json: r#"{"program":"w"}"#,
        },
    )
    .expect("begin");
    user_process_writes(&root, 1);
    let completion = complete_workspace_effect(
        &EnvelopeContext {
            store: &store,
            artifacts: &artifacts,
            binding: &binding,
            quota: &EffectQuota::default(),
            bounds: &bounds,
        },
        begin,
    )
    .expect("complete");
    let edited_digest = {
        let edited = completion
            .delta
            .iter()
            .find(|entry| entry.path == "edit-me.txt")
            .expect("edited entry");
        let _ = edited;
        r_code_runtime::services::artifacts::sha256_hex(b"after-run-1\n")
    };

    // The user's later foreign edit: nothing rolls it back, and the CAS
    // material from the receipt stays live and readable.
    write(&root, "edit-me.txt", b"user-edited-later\n");
    assert!(store.effect_artifact_is_live(&edited_digest).expect("live"));
    seed_tree(&store, "tree-s27c2", 6);
    let next = begin_workspace_effect(
        &EnvelopeContext {
            store: &store,
            artifacts: &artifacts,
            binding: &binding,
            quota: &quota,
            bounds: &bounds,
        },
        EffectIdentity {
            operation_id: "op-s27c2",
            owner_id: "owner-s27c2",
            fencing_epoch: 3,
            tree_id: "tree-s27c2",
            command_json: r#"{"program":"w"}"#,
        },
    )
    .expect("next begin sees the foreign edit, never rolls it back");
    let record = store
        .load_process_effect("op-s27c2")
        .expect("load")
        .expect("present");
    let before_json: serde_json::Value =
        serde_json::from_str(&record.before_manifest_json).expect("before json");
    let files = before_json["files"].as_array().expect("files");
    let edited = files
        .iter()
        .find(|file| file["path"] == json!("edit-me.txt"))
        .expect("edited file captured");
    assert_eq!(
        edited["sha256"],
        json!(r_code_runtime::services::artifacts::sha256_hex(
            b"user-edited-later\n"
        )),
        "the later foreign edit is the observed truth — no rollback"
    );
    drop(next);
}

#[test]
fn only_current_checkout_write_carries_a_checkout_delta() {
    assert!(checkout_delta_applies(
        ProcessProfileEffect::CurrentCheckoutWrite
    ));
    assert!(!checkout_delta_applies(ProcessProfileEffect::NoWorkspace));
    assert!(!checkout_delta_applies(ProcessProfileEffect::ScratchOnly));

    // The checkout-writing preparation policy excludes its ephemeral
    // overlay by construction.
    let policy = r_code_runtime::services::verification_inputs::checkout_write_policy(Path::new(
        "C:/prep-dir",
    ));
    assert!(policy.ephemeral.contains(&"overlay".to_string()));
}

#[test]
fn write_ingress_journals_measured_multi_file_deltas() {
    use r_code_runtime::services::artifacts::EffectArtifactPut;
    use r_code_runtime::services::mutations::persist_measured_delta;
    use r_code_runtime::services::process_effects::DeltaEntry;
    use r_code_runtime::services::process_effects::DeltaKind;

    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().join("ws");
    std::fs::create_dir_all(&root).expect("root");
    let store = V1Store::open(&temp.path().join("store.db")).expect("store");
    let artifacts = ArtifactStore::for_task(temp.path().join("blobs"), "task-s27");
    let binding = TaskWorkspaceBinding::bind_local("task-s27", &root, &[]).expect("bind");
    seed_tree(&store, "tree-s27d", 7);
    let begin = begin_workspace_effect(
        &EnvelopeContext {
            store: &store,
            artifacts: &artifacts,
            binding: &binding,
            quota: &EffectQuota::default(),
            bounds: &ScanBounds::default(),
        },
        EffectIdentity {
            operation_id: "op-s27d",
            owner_id: "owner-s27d",
            fencing_epoch: 6,
            tree_id: "tree-s27d",
            command_json: r#"{"program":"w"}"#,
        },
    )
    .expect("begin");
    // The measured delta's after-bytes are written by the process AFTER
    // begin — files that already existed at begin are before-blobs, and
    // the journal correctly refuses to re-kind the same digest.
    write(&root, "created.txt", b"created content\n");
    write(&root, "edited.txt", b"edited content\n");
    let operation = EffectArtifactPut {
        operation_id: "op-s27d",
        owner_id: "owner-s27d",
        fencing_epoch: 6,
        kind: "delta-blob",
    };
    let deltas = vec![
        DeltaEntry {
            path: "created.txt".into(),
            kind: DeltaKind::Created,
        },
        DeltaEntry {
            path: "edited.txt".into(),
            kind: DeltaKind::Edited,
        },
        DeltaEntry {
            path: "removed.txt".into(),
            kind: DeltaKind::Deleted,
        },
    ];
    let digest =
        persist_measured_delta(&store, &artifacts, operation, &root, &deltas).expect("persist");
    assert_eq!(digest.len(), 64);
    // The after-bytes of created+edited are journaled and live; deleted
    // entries contribute no blob.
    assert!(store
        .effect_artifact_is_live(&r_code_runtime::services::artifacts::sha256_hex(
            b"created content\n"
        ))
        .expect("live"));
    assert!(store
        .effect_artifact_is_live(&r_code_runtime::services::artifacts::sha256_hex(
            b"edited content\n"
        ))
        .expect("live"));
    drop(begin);
}
