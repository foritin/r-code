//! S26A — effect-artifact quota, dedup refs and crash-safe GC.
//!
//! Proves the acceptance set: no command starts without quota (the profile
//! ceiling refuses and journals the reservation durably), active inverse
//! data is never collected (refs live until after the terminal receipt,
//! and task attachments are outside GC entirely), and GC is idempotent
//! while recovering interrupted refcounts (a blob orphaned between the CAS
//! write and the ref journal is collected by the next sweep).

use r_code_runtime::services::artifacts::{
    available_bytes, ArtifactStore, EffectArtifactPut, EffectQuota, EffectQuotaError,
};
use r_code_store::v1::operations::PrepareProcessTree;
use r_code_store::v1::{
    EffectArtifactRef, ProcessEffectError, ProcessEffectPrepare, ProcessEffectState, V1Store,
};
use serde_json::json;
use std::path::{Path, PathBuf};

fn database_path(temp: &tempfile::TempDir) -> PathBuf {
    temp.path().join("store.db")
}

fn seed_operation(store: &V1Store, _temp: &Path, operation_id: &str, seed: u32) -> String {
    let tree_id = format!("tree-{operation_id}");
    let platform_identity = json!({"nativePid": 30_000 + seed, "startToken": format!("s{seed}")});
    store
        .prepare_process_tree(&PrepareProcessTree {
            tree_id: tree_id.clone(),
            attempt_id: format!("attempt-{operation_id}"),
            workspace_key: "workspace-s26a".to_string(),
            profile_id: "s26a".to_string(),
            owner: r_code_store::v1::operations::ProcessTreeOwner {
                pid: 3_000 + seed,
                start_identity: 30_000 + u64::from(seed),
                boot_identity: "windows:01234567-89ab-4cde-8f01-23456789abcd".to_string(),
                platform_identity_digest: r_code_harness_protocol::canonical_input_hash(
                    &platform_identity,
                ),
                platform_identity,
            },
        })
        .expect("seed tree");
    store
        .prepare_process_effect(ProcessEffectPrepare {
            operation_id: operation_id.to_string(),
            tree_id,
            attempt_id: format!("attempt-{operation_id}"),
            workspace_key: "workspace-s26a".to_string(),
            owner_id: format!("owner-{operation_id}"),
            fencing_epoch: 4,
            command_json: json!({"program": "tool"}).to_string(),
            lease_json: json!({"lease": "l"}).to_string(),
            before_manifest_json: json!({"files": []}).to_string(),
            before_manifest_digest: format!("digest-{operation_id}"),
            scan_policy_json: json!({"roots": []}).to_string(),
            ephemeral_roots_json: json!({"roots": []}).to_string(),
        })
        .expect("prepare operation");
    format!("owner-{operation_id}")
}

fn run(store: &V1Store, operation_id: &str, owner: &str) {
    store
        .mark_process_effect_running(operation_id, owner, 4)
        .expect("running");
}

#[test]
fn quota_preflight_refuses_without_headroom_and_reserves_durably() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = V1Store::open(&database_path(&temp)).expect("store");
    let blobs = temp.path().join("blobs");
    std::fs::create_dir_all(&blobs).expect("blobs dir");
    let artifacts = ArtifactStore::for_task(&blobs, "task-s26a");
    let owner = seed_operation(&store, temp.path(), "op-quota", 1);

    // Stored bytes count against the profile ceiling.
    artifacts
        .put_effect_bytes(
            &store,
            EffectArtifactPut {
                operation_id: "op-quota",
                owner_id: &owner,
                fencing_epoch: 4,
                kind: "before-blob",
            },
            &[0_u8; 600],
            None,
        )
        .expect("seed stored bytes");

    let quota = EffectQuota { limit_bytes: 1_000 };
    match quota
        .preflight_and_reserve(&store, &blobs, "op-quota", 500)
        .unwrap_err()
    {
        EffectQuotaError::QuotaExceeded {
            used,
            requested,
            limit,
        } => {
            assert_eq!(used, 600);
            assert_eq!(requested, 500);
            assert_eq!(limit, 1_000);
        }
        other => panic!("expected the quota refusal, got {other:?}"),
    }

    // Within the ceiling the reservation journals durably and survives a
    // reopen; the free-space arm is a real-volume query (the disk-exhausted
    // refusal shares the same code path after it).
    let reservation = quota
        .preflight_and_reserve(&store, &blobs, "op-quota", 300)
        .expect("reserve within the ceiling");
    assert!(reservation.starts_with("reservation-op-quota-"));
    assert_eq!(store.reserved_active_bytes().expect("reserved"), 300);
    let reopened = V1Store::open(&database_path(&temp)).expect("reopen");
    assert_eq!(reopened.reserved_active_bytes().expect("reserved"), 300);
    assert!(
        available_bytes(&blobs).expect("available") > 0,
        "the free-space probe reads a real volume"
    );

    // No command starts without quota: the refusing path is the one the
    // launch gate consumes (the ceiling counts stored + reserved bytes).
    let quota_two = EffectQuota { limit_bytes: 700 };
    assert!(matches!(
        quota_two.preflight_and_reserve(&store, &blobs, "op-quota", 1),
        Err(EffectQuotaError::QuotaExceeded { .. })
    ));
}

#[test]
fn receipt_releases_reservations_in_the_same_transaction() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = V1Store::open(&database_path(&temp)).expect("store");
    let blobs = temp.path().join("blobs");
    std::fs::create_dir_all(&blobs).expect("blobs dir");
    let owner = seed_operation(&store, temp.path(), "op-rel", 2);
    store
        .reserve_effect_disk("reservation-op-rel-1", "op-rel", 1_000)
        .expect("reserve");
    assert_eq!(store.reserved_active_bytes().expect("reserved"), 1_000);

    run(&store, "op-rel", &owner);
    store
        .record_process_effect_receipt("op-rel", &owner, 4, "receipt-digest")
        .expect("receipt");
    assert_eq!(
        store.reserved_active_bytes().expect("reserved"),
        0,
        "the receipt releases the reservation in the same transaction"
    );

    // A crash right after the receipt keeps the release (reopen sees zero).
    let reopened = V1Store::open(&database_path(&temp)).expect("reopen");
    assert_eq!(reopened.reserved_active_bytes().expect("reserved"), 0);
}

#[test]
fn put_effect_bytes_dedupes_and_journals_refs() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = V1Store::open(&database_path(&temp)).expect("store");
    let blobs = temp.path().join("blobs");
    std::fs::create_dir_all(&blobs).expect("blobs dir");
    let artifacts = ArtifactStore::for_task(&blobs, "task-s26a");
    let owner_a = seed_operation(&store, temp.path(), "op-dedup-a", 3);
    let owner_b = seed_operation(&store, temp.path(), "op-dedup-b", 4);

    let first = artifacts
        .put_effect_bytes(
            &store,
            EffectArtifactPut {
                operation_id: "op-dedup-a",
                owner_id: &owner_a,
                fencing_epoch: 4,
                kind: "delta-blob",
            },
            b"shared",
            None,
        )
        .expect("put a");
    let second = artifacts
        .put_effect_bytes(
            &store,
            EffectArtifactPut {
                operation_id: "op-dedup-b",
                owner_id: &owner_b,
                fencing_epoch: 4,
                kind: "delta-blob",
            },
            b"shared",
            None,
        )
        .expect("put b");
    assert_eq!(first, second, "identical bytes dedupe to one reference");

    // One blob pair + one marker on disk; two ref rows in the journal.
    let mut blob_count = 0;
    let mut marker_count = 0;
    for entry in std::fs::read_dir(&blobs).expect("read") {
        let name = entry
            .expect("entry")
            .file_name()
            .to_string_lossy()
            .into_owned();
        if name.ends_with(".blob") {
            blob_count += 1;
        }
        if name.ends_with(".effect") {
            marker_count += 1;
        }
    }
    assert_eq!((blob_count, marker_count), (1, 1));
    assert!(store.effect_artifact_is_live(&first.sha256).expect("live"));

    // The ref rows are the refcount: replaying the same (op, digest) row is
    // idempotent; a divergent row for the same pair conflicts.
    store
        .own_effect_artifacts(
            "op-dedup-a",
            &owner_a,
            4,
            &[EffectArtifactRef {
                digest: first.sha256.clone(),
                bytes: first.bytes,
                kind: "delta-blob".to_string(),
            }],
        )
        .expect("idempotent replay");
    assert_eq!(
        store
            .own_effect_artifacts(
                "op-dedup-a",
                &owner_a,
                4,
                &[EffectArtifactRef {
                    digest: first.sha256.clone(),
                    bytes: first.bytes,
                    kind: "output-tail".to_string(),
                }]
            )
            .unwrap_err(),
        ProcessEffectError::OperationConflict
    );
}

#[test]
fn gc_never_collects_active_inverse_data_or_task_attachments() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = V1Store::open(&database_path(&temp)).expect("store");
    let blobs = temp.path().join("blobs");
    std::fs::create_dir_all(&blobs).expect("blobs dir");
    let artifacts = ArtifactStore::for_task(&blobs, "task-s26a");
    let owner_a = seed_operation(&store, temp.path(), "op-gc-a", 5);
    let owner_b = seed_operation(&store, temp.path(), "op-gc-b", 6);
    run(&store, "op-gc-a", &owner_a);
    run(&store, "op-gc-b", &owner_b);

    let shared = artifacts
        .put_effect_bytes(
            &store,
            EffectArtifactPut {
                operation_id: "op-gc-a",
                owner_id: &owner_a,
                fencing_epoch: 4,
                kind: "before-blob",
            },
            b"inverse",
            None,
        )
        .expect("put shared");
    artifacts
        .put_effect_bytes(
            &store,
            EffectArtifactPut {
                operation_id: "op-gc-b",
                owner_id: &owner_b,
                fencing_epoch: 4,
                kind: "before-blob",
            },
            b"inverse",
            None,
        )
        .expect("put shared again");
    // A task attachment: same store, no effect marker, never GC scope.
    let attachment = artifacts
        .put_bytes(b"user-attachment", Some("text/plain".into()))
        .expect("attachment");

    // B reaches its receipt and releases its ref — A is still running and
    // holds the shared digest: nothing may be collected yet.
    store
        .record_process_effect_receipt("op-gc-b", &owner_b, 4, "receipt-b")
        .expect("receipt b");
    store
        .release_effect_artifacts("op-gc-b", &owner_b, 4, &[shared.sha256.as_str()])
        .expect("release b ref");
    let outcome = artifacts.effect_gc(&store).expect("gc");
    assert_eq!(
        outcome.collected, 0,
        "active inverse data is never collected"
    );
    assert_eq!(outcome.retained, 1);

    // A receipts and releases too: the shared blob becomes collectable —
    // and only it; the attachment stays untouched.
    store
        .record_process_effect_receipt("op-gc-a", &owner_a, 4, "receipt-a")
        .expect("receipt a");
    store
        .release_effect_artifacts("op-gc-a", &owner_a, 4, &[shared.sha256.as_str()])
        .expect("release a ref");
    let outcome = artifacts.effect_gc(&store).expect("gc");
    assert_eq!(outcome.collected, 1);
    assert_eq!(outcome.retained, 0);
    assert!(
        artifacts.read_all(&attachment).is_ok(),
        "task attachments are outside GC"
    );
    assert!(!blobs.join(format!("{}.blob", shared.sha256)).exists());
}

#[test]
fn gc_is_idempotent_and_recovers_interrupted_refcounts() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = V1Store::open(&database_path(&temp)).expect("store");
    let blobs = temp.path().join("blobs");
    std::fs::create_dir_all(&blobs).expect("blobs dir");
    let artifacts = ArtifactStore::for_task(&blobs, "task-s26a");
    let owner = seed_operation(&store, temp.path(), "op-orphan", 7);
    run(&store, "op-orphan", &owner);

    // An interrupted put: the CAS pair and marker landed, the ref journal
    // never did (the crash window inside put_effect_bytes).
    let digest = {
        let reference = artifacts.put_bytes(b"orphaned", None).expect("blob write");
        std::fs::write(
            blobs.join(format!("{}.effect", reference.sha256)),
            b"effect-owned\n",
        )
        .expect("marker");
        reference.sha256
    };
    assert!(!store.effect_artifact_is_live(&digest).expect("live"));

    // GC recovers the orphan; a rerun collects nothing (idempotent).
    let first = artifacts.effect_gc(&store).expect("gc");
    assert_eq!(first.collected, 1);
    assert!(!blobs.join(format!("{digest}.blob")).exists());
    let second = artifacts.effect_gc(&store).expect("gc rerun");
    assert_eq!(second.collected, 0);

    // Live data still survives every sweep.
    let live = artifacts
        .put_effect_bytes(
            &store,
            EffectArtifactPut {
                operation_id: "op-orphan",
                owner_id: &owner,
                fencing_epoch: 4,
                kind: "output-tail",
            },
            b"live",
            None,
        )
        .expect("live put");
    let third = artifacts.effect_gc(&store).expect("gc");
    assert_eq!(
        third,
        r_code_runtime::services::artifacts::EffectGcOutcome {
            collected: 0,
            retained: 1
        }
    );
    assert!(blobs.join(format!("{}.blob", live.sha256)).exists());
}

#[test]
fn refs_release_only_from_receipted_operations_under_the_fence() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = V1Store::open(&database_path(&temp)).expect("store");
    let blobs = temp.path().join("blobs");
    std::fs::create_dir_all(&blobs).expect("blobs dir");
    let artifacts = ArtifactStore::for_task(&blobs, "task-s26a");
    let owner = seed_operation(&store, temp.path(), "op-fence", 8);
    let reference = artifacts
        .put_effect_bytes(
            &store,
            EffectArtifactPut {
                operation_id: "op-fence",
                owner_id: &owner,
                fencing_epoch: 4,
                kind: "manifest",
            },
            b"fenced",
            None,
        )
        .expect("put");

    // Running operations keep their refs (retention through active life).
    run(&store, "op-fence", &owner);
    assert_eq!(
        store
            .release_effect_artifacts("op-fence", &owner, 4, &[reference.sha256.as_str()])
            .unwrap_err(),
        ProcessEffectError::InvalidTransition {
            expected: ProcessEffectState::Receipted,
            actual: ProcessEffectState::Running
        }
    );
    // The wrong owner cannot release even after the receipt.
    store
        .record_process_effect_receipt("op-fence", &owner, 4, "receipt-f")
        .expect("receipt");
    assert_eq!(
        store
            .release_effect_artifacts(
                "op-fence",
                "owner-imposter",
                4,
                &[reference.sha256.as_str()]
            )
            .unwrap_err(),
        ProcessEffectError::StaleOwner
    );
    // The exact owner releases; replays converge (delete is idempotent).
    assert_eq!(
        store
            .release_effect_artifacts("op-fence", &owner, 4, &[reference.sha256.as_str()])
            .expect("release"),
        1
    );
    assert_eq!(
        store
            .release_effect_artifacts("op-fence", &owner, 4, &[reference.sha256.as_str()])
            .expect("replay"),
        0
    );
}
