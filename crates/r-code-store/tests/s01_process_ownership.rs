//! P01 — durable process-tree ownership, proof fencing and quarantine.

use r_code_store::v1::operations::{
    NewTerminationProof, PrepareProcessTree, ProcessTreeFence, ProcessTreeOwner, ProcessTreeRecord,
    ProcessTreeState, ProcessTreeStoreError, TerminationProofKind,
};
use r_code_store::v1::{LeaseRequest, MutationError, V1Store};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Barrier};

const BOOT_A: &str = "windows:01234567-89ab-4cde-8f01-23456789abcd";
const BOOT_B: &str = "windows:01234567-89ab-4cde-8f01-23456789abce";

fn database_path(temp: &tempfile::TempDir) -> PathBuf {
    temp.path().join("store.db")
}

fn digest(value: &Value) -> String {
    r_code_harness_protocol::canonical_input_hash(value)
}

fn owner(seed: u32) -> ProcessTreeOwner {
    let platform_identity = json!({
        "nativePid": 10_000 + seed,
        "startToken": format!("start-{seed}"),
    });
    ProcessTreeOwner {
        pid: 1_000 + seed,
        start_identity: 10_000 + u64::from(seed),
        boot_identity: BOOT_A.to_string(),
        platform_identity_digest: digest(&platform_identity),
        platform_identity,
    }
}

fn prepare_input(tree_id: &str, workspace_key: &str, seed: u32) -> PrepareProcessTree {
    PrepareProcessTree {
        tree_id: tree_id.to_string(),
        attempt_id: format!("attempt-{tree_id}"),
        workspace_key: workspace_key.to_string(),
        profile_id: "offline-write".to_string(),
        owner: owner(seed),
    }
}

fn prepare_tree(
    store: &V1Store,
    tree_id: &str,
    workspace_key: &str,
    seed: u32,
) -> ProcessTreeRecord {
    store
        .prepare_process_tree(&prepare_input(tree_id, workspace_key, seed))
        .expect("prepare process tree")
}

fn transition(
    store: &V1Store,
    tree: &ProcessTreeRecord,
    next: ProcessTreeState,
    reason: Option<&str>,
    proof: Option<&NewTerminationProof>,
) -> ProcessTreeRecord {
    store
        .transition_process_tree(
            &tree.tree_id,
            &tree.fence(),
            tree.state,
            next,
            reason,
            proof,
        )
        .expect("legal process-tree transition")
}

fn running(store: &V1Store, tree: ProcessTreeRecord) -> ProcessTreeRecord {
    transition(store, &tree, ProcessTreeState::Running, None, None)
}

fn terminating(store: &V1Store, tree: ProcessTreeRecord) -> ProcessTreeRecord {
    let tree = if tree.state == ProcessTreeState::Prepared {
        running(store, tree)
    } else {
        tree
    };
    transition(store, &tree, ProcessTreeState::Terminating, None, None)
}

fn exit_proof(tree: &ProcessTreeRecord, proof_id: &str) -> NewTerminationProof {
    NewTerminationProof::bound_to_tree(
        proof_id,
        TerminationProofKind::Exit,
        tree.owner.boot_identity.clone(),
        tree,
        json!({"nativeExit": true, "pid": tree.owner.pid}),
    )
}

fn exited_tree(
    store: &V1Store,
    tree_id: &str,
    workspace_key: &str,
    seed: u32,
) -> (ProcessTreeRecord, NewTerminationProof) {
    let tree = terminating(store, prepare_tree(store, tree_id, workspace_key, seed));
    let proof = exit_proof(&tree, &format!("proof-{tree_id}"));
    let exited = transition(store, &tree, ProcessTreeState::Exited, None, Some(&proof));
    (exited, proof)
}

fn lease_request(
    workspace_key: &str,
    operation_id: &str,
    read_only: bool,
    repo_exclusive: bool,
) -> LeaseRequest {
    LeaseRequest {
        workspace_key: workspace_key.to_string(),
        operation_id: operation_id.to_string(),
        owner_id: format!("owner-{operation_id}"),
        read_paths: if read_only {
            vec!["docs/readme.md".to_string()]
        } else {
            Vec::new()
        },
        write_paths: if !read_only && !repo_exclusive {
            vec!["src/lib.rs".to_string()]
        } else {
            Vec::new()
        },
        repo_exclusive,
    }
}

fn assert_workspace_quarantined(store: &V1Store, workspace_key: &str, operation_suffix: &str) {
    for (kind, request) in [
        (
            "write",
            lease_request(
                workspace_key,
                &format!("write-{operation_suffix}"),
                false,
                false,
            ),
        ),
        (
            "repo",
            lease_request(
                workspace_key,
                &format!("repo-{operation_suffix}"),
                false,
                true,
            ),
        ),
    ] {
        assert_eq!(
            store.acquire_lease(request),
            Err(MutationError::WorkspaceQuarantined {
                workspace_key: workspace_key.to_string(),
            }),
            "{kind} lease escaped quarantine"
        );
    }
    store
        .acquire_lease(lease_request(
            workspace_key,
            &format!("read-{operation_suffix}"),
            true,
            false,
        ))
        .expect("read-only lease remains available");
}

#[test]
fn schema_upgrade_is_idempotent_indexed_constrained_and_keeps_old_rows() {
    let temp = tempfile::tempdir().unwrap();
    let path = database_path(&temp);
    let legacy = Connection::open(&path).unwrap();
    legacy
        .execute_batch(
            "CREATE TABLE writer_barriers (
                barrier_id TEXT PRIMARY KEY,
                workspace_key TEXT NOT NULL,
                owner_pid INTEGER NOT NULL,
                owner_start TEXT NOT NULL,
                reason TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL
             );
             INSERT INTO writer_barriers VALUES
                ('old-barrier', 'old-workspace', 41, '99', 'legacy', 1);",
        )
        .unwrap();
    drop(legacy);

    for _ in 0..2 {
        let store = V1Store::open(&path).expect("open and upgrade old v1 database");
        assert_eq!(store.writer_barriers("old-workspace").unwrap().len(), 1);
    }

    let raw = Connection::open(&path).unwrap();
    raw.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
    for table in [
        "workspace_ownership_epochs",
        "process_trees",
        "termination_proofs",
    ] {
        let present: i64 = raw
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                params![table],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(present, 1, "missing table {table}");
    }
    for index in [
        "idx_process_trees_workspace_state",
        "idx_termination_proofs_tree",
    ] {
        let present: i64 = raw
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name=?1",
                params![index],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(present, 1, "missing index {index}");
    }
    let migration_count: i64 = raw
        .query_row(
            "SELECT COUNT(*) FROM v1_schema_migrations
             WHERE migration_id='process-tree-ownership'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(migration_count, 1);

    let foreign_key: i64 = raw
        .query_row(
            "SELECT COUNT(*) FROM pragma_foreign_key_list('termination_proofs')
             WHERE \"table\"='process_trees' AND \"from\"='tree_id' AND \"to\"='tree_id'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(foreign_key, 1);

    let store = V1Store::open(&path).unwrap();
    let tree = prepare_tree(&store, "schema-tree", "schema-workspace", 1);
    drop(store);
    for sql in [
        "UPDATE process_trees SET ownership_epoch=0 WHERE tree_id='schema-tree'",
        "UPDATE process_trees SET state_revision=0 WHERE tree_id='schema-tree'",
        "UPDATE process_trees SET state='forged' WHERE tree_id='schema-tree'",
    ] {
        assert!(raw.execute(sql, []).is_err(), "constraint accepted {sql}");
    }
    assert!(raw
        .execute(
            "INSERT INTO termination_proofs(
                proof_id, tree_id, ownership_epoch, proof_kind,
                observed_boot_identity, proof_identity_json,
                proof_identity_digest, recorded_at_ms)
             VALUES ('orphan', 'missing-tree', 1, 'exit', ?1, '{}', 'bad', 1)",
            params![BOOT_A],
        )
        .is_err());
    assert_eq!(tree.ownership_epoch, 1);
}

#[test]
fn prepare_validates_every_identity_field_and_replays_only_exact_input() {
    let temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&database_path(&temp)).unwrap();
    let base = prepare_input("tree-exact", "workspace-exact", 2);

    let mut invalid = Vec::new();
    for field in ["tree", "attempt", "workspace", "profile"] {
        let mut input = base.clone();
        match field {
            "tree" => input.tree_id = " \t".into(),
            "attempt" => input.attempt_id.clear(),
            "workspace" => input.workspace_key = " ".into(),
            "profile" => input.profile_id.clear(),
            _ => unreachable!(),
        }
        invalid.push(input);
    }
    let mut zero_pid = base.clone();
    zero_pid.owner.pid = 0;
    invalid.push(zero_pid);
    let mut zero_start = base.clone();
    zero_start.owner.start_identity = 0;
    invalid.push(zero_start);
    for bad_boot in [
        "",
        "windows:00000000-0000-0000-0000-000000000000",
        "windows:01234567-89AB-4CDE-8F01-23456789ABCD",
        "windows:01234567-89ab-4cde-8f01-23456789abcd\n",
        "macos:0:000000",
        "macos:1:42",
        "unknown:01234567-89ab-4cde-8f01-23456789abcd",
    ] {
        let mut input = base.clone();
        input.owner.boot_identity = bad_boot.into();
        invalid.push(input);
    }
    let mut null_platform = base.clone();
    null_platform.owner.platform_identity = Value::Null;
    null_platform.owner.platform_identity_digest = digest(&Value::Null);
    invalid.push(null_platform);
    let mut stale_digest = base.clone();
    stale_digest.owner.platform_identity["nativePid"] = json!(999_999);
    invalid.push(stale_digest);

    for input in invalid {
        assert_eq!(
            store.prepare_process_tree(&input),
            Err(ProcessTreeStoreError::InvalidInput),
            "accepted invalid prepare input {input:?}"
        );
    }

    let first = store.prepare_process_tree(&base).unwrap();
    assert_eq!(first.state, ProcessTreeState::Prepared);
    assert_eq!(first.state_revision, 1);
    assert_eq!(first.ownership_epoch, 1);
    assert_eq!(store.prepare_process_tree(&base).unwrap(), first);

    let mut variants = Vec::new();
    let mut input = base.clone();
    input.attempt_id.push_str("-other");
    variants.push(input);
    let mut input = base.clone();
    input.workspace_key.push_str("-other");
    variants.push(input);
    let mut input = base.clone();
    input.profile_id.push_str("-other");
    variants.push(input);
    let mut input = base.clone();
    input.owner.pid += 1;
    variants.push(input);
    let mut input = base.clone();
    input.owner.start_identity += 1;
    variants.push(input);
    let mut input = base.clone();
    input.owner.boot_identity = BOOT_B.into();
    variants.push(input);
    let mut input = base.clone();
    input.owner.platform_identity["nativePid"] = json!(777);
    input.owner.platform_identity_digest = digest(&input.owner.platform_identity);
    variants.push(input);

    for input in variants {
        assert_eq!(
            store.prepare_process_tree(&input),
            Err(ProcessTreeStoreError::IdentityConflict)
        );
    }
    assert_eq!(store.load_process_tree(&base.tree_id).unwrap(), Some(first));
}

#[test]
fn workspace_epochs_are_monotonic_unique_and_serialized_across_connections() {
    let temp = tempfile::tempdir().unwrap();
    let path = database_path(&temp);
    let left_store = V1Store::open(&path).unwrap();
    let right_store = V1Store::open(&path).unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let mut handles = Vec::new();
    for ((tree_id, seed), store) in [
        (("tree-left", 11), left_store),
        (("tree-right", 12), right_store),
    ] {
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            store.prepare_process_tree(&prepare_input(tree_id, "workspace-race", seed))
        }));
    }
    barrier.wait();
    let mut epochs = handles
        .into_iter()
        .map(|handle| handle.join().unwrap().unwrap().ownership_epoch)
        .collect::<Vec<_>>();
    epochs.sort_unstable();
    assert_eq!(epochs, [1, 2]);

    let store = V1Store::open(&path).unwrap();
    let third = prepare_tree(&store, "tree-third", "workspace-race", 13);
    let other = prepare_tree(&store, "tree-other", "workspace-other", 14);
    assert_eq!(third.ownership_epoch, 3);
    assert_eq!(other.ownership_epoch, 1);
    let persisted = store.process_trees_for_workspace("workspace-race").unwrap();
    assert_eq!(
        persisted
            .iter()
            .map(|tree| tree.ownership_epoch)
            .collect::<Vec<_>>(),
        [1, 2, 3]
    );
}

#[test]
fn every_legal_transition_works_and_every_other_state_pair_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&database_path(&temp)).unwrap();

    let prepared_running = prepare_tree(&store, "legal-pr", "ws-pr", 20);
    assert_eq!(
        transition(
            &store,
            &prepared_running,
            ProcessTreeState::Running,
            None,
            None,
        )
        .state,
        ProcessTreeState::Running
    );
    let prepared_quarantined = prepare_tree(&store, "legal-pq", "ws-pq", 21);
    assert_eq!(
        transition(
            &store,
            &prepared_quarantined,
            ProcessTreeState::Quarantined,
            Some("prepare failed after spawn"),
            None,
        )
        .state,
        ProcessTreeState::Quarantined
    );

    let running_terminating = running(&store, prepare_tree(&store, "legal-rt", "ws-rt", 22));
    assert_eq!(
        transition(
            &store,
            &running_terminating,
            ProcessTreeState::Terminating,
            None,
            None,
        )
        .state,
        ProcessTreeState::Terminating
    );
    let running_quarantined = running(&store, prepare_tree(&store, "legal-rq", "ws-rq", 23));
    assert_eq!(
        transition(
            &store,
            &running_quarantined,
            ProcessTreeState::Quarantined,
            Some("lost process handle"),
            None,
        )
        .state,
        ProcessTreeState::Quarantined
    );

    let terminating_quarantined =
        terminating(&store, prepare_tree(&store, "legal-tq", "ws-tq", 24));
    assert_eq!(
        transition(
            &store,
            &terminating_quarantined,
            ProcessTreeState::Quarantined,
            Some("death unproved"),
            None,
        )
        .state,
        ProcessTreeState::Quarantined
    );
    let terminating_exited = terminating(&store, prepare_tree(&store, "legal-te", "ws-te", 25));
    let proof = exit_proof(&terminating_exited, "proof-legal-te");
    assert_eq!(
        transition(
            &store,
            &terminating_exited,
            ProcessTreeState::Exited,
            None,
            Some(&proof),
        )
        .state,
        ProcessTreeState::Exited
    );
    let quarantined_exited = transition(
        &store,
        &prepare_tree(&store, "legal-qe", "ws-qe", 26),
        ProcessTreeState::Quarantined,
        Some("ambiguous launch"),
        None,
    );
    let proof = exit_proof(&quarantined_exited, "proof-legal-qe");
    assert_eq!(
        transition(
            &store,
            &quarantined_exited,
            ProcessTreeState::Exited,
            None,
            Some(&proof),
        )
        .state,
        ProcessTreeState::Exited
    );

    let all = [
        ProcessTreeState::Prepared,
        ProcessTreeState::Running,
        ProcessTreeState::Terminating,
        ProcessTreeState::Exited,
        ProcessTreeState::Quarantined,
        ProcessTreeState::LegacyUnverifiable,
    ];
    let matrix = prepare_tree(&store, "transition-matrix", "ws-matrix", 27);
    for from in all {
        for to in all {
            let legal = matches!(
                (from, to),
                (ProcessTreeState::Prepared, ProcessTreeState::Running)
                    | (ProcessTreeState::Prepared, ProcessTreeState::Quarantined)
                    | (ProcessTreeState::Running, ProcessTreeState::Terminating)
                    | (ProcessTreeState::Running, ProcessTreeState::Quarantined)
                    | (ProcessTreeState::Terminating, ProcessTreeState::Exited)
                    | (ProcessTreeState::Terminating, ProcessTreeState::Quarantined)
                    | (ProcessTreeState::Quarantined, ProcessTreeState::Exited)
                    | (
                        ProcessTreeState::LegacyUnverifiable,
                        ProcessTreeState::Exited
                    )
            );
            if !legal {
                assert_eq!(
                    store.transition_process_tree(
                        &matrix.tree_id,
                        &matrix.fence(),
                        from,
                        to,
                        None,
                        None,
                    ),
                    Err(ProcessTreeStoreError::InvalidTransition),
                    "illegal transition {from:?}->{to:?} was accepted"
                );
            }
        }
    }
}

#[test]
fn stale_fence_components_and_lost_response_variants_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&database_path(&temp)).unwrap();
    let prepared = prepare_tree(&store, "tree-fence", "workspace-fence", 30);
    let fence = prepared.fence();

    let mut stale_fences = Vec::<ProcessTreeFence>::new();
    let mut stale = fence.clone();
    stale.ownership_epoch += 1;
    stale_fences.push(stale);
    let mut stale = fence.clone();
    stale.state_revision += 1;
    stale_fences.push(stale);
    let mut stale = fence.clone();
    stale.owner_pid += 1;
    stale_fences.push(stale);
    let mut stale = fence.clone();
    stale.owner_start_identity += 1;
    stale_fences.push(stale);
    let mut stale = fence.clone();
    stale.owner_boot_identity = BOOT_B.into();
    stale_fences.push(stale);
    let mut stale = fence.clone();
    stale.platform_identity_digest.push_str("-forged");
    stale_fences.push(stale);

    for stale in stale_fences {
        assert_eq!(
            store.transition_process_tree(
                &prepared.tree_id,
                &stale,
                ProcessTreeState::Prepared,
                ProcessTreeState::Running,
                None,
                None,
            ),
            Err(ProcessTreeStoreError::StaleFence)
        );
    }

    let running = store
        .transition_process_tree(
            &prepared.tree_id,
            &fence,
            ProcessTreeState::Prepared,
            ProcessTreeState::Running,
            None,
            None,
        )
        .unwrap();
    assert_eq!(
        store
            .transition_process_tree(
                &prepared.tree_id,
                &fence,
                ProcessTreeState::Prepared,
                ProcessTreeState::Running,
                None,
                None,
            )
            .unwrap(),
        running,
        "commit-lost response must replay exactly"
    );
    assert_eq!(
        store.transition_process_tree(
            &prepared.tree_id,
            &fence,
            ProcessTreeState::Prepared,
            ProcessTreeState::Quarantined,
            Some("different request"),
            None,
        ),
        Err(ProcessTreeStoreError::StaleFence)
    );

    let fresh = prepare_tree(&store, "tree-input-rules", "workspace-input-rules", 31);
    assert_eq!(
        store.transition_process_tree(
            &fresh.tree_id,
            &fresh.fence(),
            ProcessTreeState::Prepared,
            ProcessTreeState::Quarantined,
            None,
            None,
        ),
        Err(ProcessTreeStoreError::InvalidInput)
    );
    assert_eq!(
        store.transition_process_tree(
            &fresh.tree_id,
            &fresh.fence(),
            ProcessTreeState::Prepared,
            ProcessTreeState::Running,
            Some("not allowed"),
            None,
        ),
        Err(ProcessTreeStoreError::InvalidInput)
    );
    let premature_proof = exit_proof(&fresh, "premature-proof");
    assert_eq!(
        store.transition_process_tree(
            &fresh.tree_id,
            &fresh.fence(),
            ProcessTreeState::Prepared,
            ProcessTreeState::Running,
            None,
            Some(&premature_proof),
        ),
        Err(ProcessTreeStoreError::InvalidProof)
    );
    let terminating = terminating(
        &store,
        prepare_tree(&store, "tree-needs-proof", "workspace-needs-proof", 32),
    );
    assert_eq!(
        store.transition_process_tree(
            &terminating.tree_id,
            &terminating.fence(),
            ProcessTreeState::Terminating,
            ProcessTreeState::Exited,
            None,
            None,
        ),
        Err(ProcessTreeStoreError::InvalidProof)
    );
}

fn rehash_proof(proof: &mut NewTerminationProof) {
    proof.proof_identity_digest = digest(&proof.proof_identity);
}

#[test]
fn exit_proof_binds_every_tree_owner_boot_and_platform_field_exactly() {
    let temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&database_path(&temp)).unwrap();
    let tree = terminating(
        &store,
        prepare_tree(&store, "tree-proof", "workspace-proof", 40),
    );
    let valid = exit_proof(&tree, "proof-valid");

    let mut invalid = Vec::new();
    let mut proof = valid.clone();
    proof.proof_id.clear();
    invalid.push(("empty proof id", proof));
    let mut proof = valid.clone();
    proof.proof_id = "proof-null-json".into();
    proof.proof_identity = Value::Null;
    rehash_proof(&mut proof);
    invalid.push(("null proof identity", proof));
    let mut proof = valid.clone();
    proof.proof_id = "proof-stale-digest".into();
    proof.proof_identity["ownerPid"] = json!(tree.owner.pid + 1);
    invalid.push(("stale proof digest", proof));
    let mut proof = valid.clone();
    proof.proof_id = "proof-reboot-same-boot".into();
    proof.kind = TerminationProofKind::Reboot;
    invalid.push(("wrong proof kind", proof));
    let mut proof = NewTerminationProof::bound_to_tree(
        "proof-exit-wrong-boot",
        TerminationProofKind::Exit,
        BOOT_B,
        &tree,
        json!({"nativeExit": true}),
    );
    rehash_proof(&mut proof);
    invalid.push(("exit proof observed another boot", proof));

    for (name, replacement) in [
        ("treeId", json!("other-tree")),
        ("ownershipEpoch", json!(tree.ownership_epoch + 1)),
        ("ownerPid", json!(tree.owner.pid + 1)),
        ("ownerStartIdentity", json!(tree.owner.start_identity + 1)),
        ("ownerBootIdentity", json!(BOOT_B)),
        (
            "ownerPlatformIdentityDigest",
            json!(format!("{}-forged", tree.owner.platform_identity_digest)),
        ),
        ("observedBootIdentity", json!(BOOT_B)),
        ("migratedObservedBootIdentity", json!(BOOT_A)),
        ("platformEvidence", Value::Null),
    ] {
        let mut proof = valid.clone();
        proof.proof_id = format!("proof-wrong-{name}");
        proof.proof_identity[name] = replacement;
        rehash_proof(&mut proof);
        invalid.push((name, proof));
    }
    let mut missing = valid.clone();
    missing.proof_id = "proof-missing-field".into();
    missing
        .proof_identity
        .as_object_mut()
        .unwrap()
        .remove("ownerPid");
    rehash_proof(&mut missing);
    invalid.push(("missing required field", missing));
    let mut extra = valid.clone();
    extra.proof_id = "proof-extra-field".into();
    extra.proof_identity["forgedExtra"] = json!(true);
    rehash_proof(&mut extra);
    invalid.push(("extra noncanonical field", extra));

    for (name, proof) in invalid {
        assert_eq!(
            store.transition_process_tree(
                &tree.tree_id,
                &tree.fence(),
                ProcessTreeState::Terminating,
                ProcessTreeState::Exited,
                None,
                Some(&proof),
            ),
            Err(ProcessTreeStoreError::InvalidProof),
            "accepted forged proof variant {name}"
        );
        if !proof.proof_id.is_empty() {
            assert_eq!(store.load_termination_proof(&proof.proof_id).unwrap(), None);
        }
        assert_eq!(
            store
                .load_process_tree(&tree.tree_id)
                .unwrap()
                .unwrap()
                .state,
            ProcessTreeState::Terminating
        );
    }

    let exited = store
        .transition_process_tree(
            &tree.tree_id,
            &tree.fence(),
            ProcessTreeState::Terminating,
            ProcessTreeState::Exited,
            None,
            Some(&valid),
        )
        .unwrap();
    assert_eq!(exited.state, ProcessTreeState::Exited);
    assert_eq!(exited.termination_proof_id.as_deref(), Some("proof-valid"));
    let persisted = store
        .load_termination_proof("proof-valid")
        .unwrap()
        .unwrap();
    assert_eq!(persisted.tree_id, tree.tree_id);
    assert_eq!(persisted.ownership_epoch, tree.ownership_epoch);
    assert_eq!(persisted.proof_identity, valid.proof_identity);
    assert_eq!(persisted.proof_identity_digest, valid.proof_identity_digest);

    assert_eq!(
        store
            .transition_process_tree(
                &tree.tree_id,
                &tree.fence(),
                ProcessTreeState::Terminating,
                ProcessTreeState::Exited,
                None,
                Some(&valid),
            )
            .unwrap(),
        exited,
        "lost response must replay exact exit proof"
    );
    let mut variant = valid.clone();
    variant.proof_id = "proof-variant-after-commit".into();
    assert!(store
        .transition_process_tree(
            &tree.tree_id,
            &tree.fence(),
            ProcessTreeState::Terminating,
            ProcessTreeState::Exited,
            None,
            Some(&variant),
        )
        .is_err());
}

#[test]
fn one_proof_id_cannot_be_reused_for_another_tree() {
    let temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&database_path(&temp)).unwrap();
    let first = terminating(
        &store,
        prepare_tree(&store, "tree-proof-a", "workspace-proof-a", 41),
    );
    let first_proof = exit_proof(&first, "shared-proof-id");
    transition(
        &store,
        &first,
        ProcessTreeState::Exited,
        None,
        Some(&first_proof),
    );

    let second = terminating(
        &store,
        prepare_tree(&store, "tree-proof-b", "workspace-proof-b", 42),
    );
    let second_proof = exit_proof(&second, "shared-proof-id");
    assert_eq!(
        store.transition_process_tree(
            &second.tree_id,
            &second.fence(),
            ProcessTreeState::Terminating,
            ProcessTreeState::Exited,
            None,
            Some(&second_proof),
        ),
        Err(ProcessTreeStoreError::IdentityConflict)
    );
    assert_eq!(
        store
            .load_process_tree(&second.tree_id)
            .unwrap()
            .unwrap()
            .state,
        ProcessTreeState::Terminating
    );
}

fn corrupt_exited_fixture(corruption: &str) -> (tempfile::TempDir, V1Store, String, String) {
    let temp = tempfile::tempdir().unwrap();
    let path = database_path(&temp);
    let store = V1Store::open(&path).unwrap();
    let workspace = format!("workspace-corrupt-{corruption}");
    let tree_id = format!("tree-corrupt-{corruption}");
    let (tree, proof) = exited_tree(&store, &tree_id, &workspace, 50);
    let wrong_tree_target = (corruption == "wrong-tree").then(|| {
        prepare_tree(
            &store,
            "tree-valid-but-unrelated",
            "workspace-valid-but-unrelated",
            51,
        )
        .tree_id
    });
    let raw = Connection::open(&path).unwrap();
    match corruption {
        "null-proof-id" => {
            raw.execute(
                "UPDATE process_trees SET termination_proof_id=NULL WHERE tree_id=?1",
                params![tree_id],
            )
            .unwrap();
        }
        "empty-proof-id" => {
            raw.execute(
                "UPDATE process_trees SET termination_proof_id='' WHERE tree_id=?1",
                params![tree_id],
            )
            .unwrap();
        }
        "missing-proof-row" => {
            raw.execute(
                "DELETE FROM termination_proofs WHERE proof_id=?1",
                params![proof.proof_id],
            )
            .unwrap();
        }
        "wrong-tree" => {
            raw.execute(
                "UPDATE termination_proofs SET tree_id=?1 WHERE proof_id=?2",
                params![wrong_tree_target.unwrap(), proof.proof_id],
            )
            .unwrap();
        }
        "wrong-epoch" => {
            raw.execute(
                "UPDATE termination_proofs SET ownership_epoch=?1 WHERE proof_id=?2",
                params![tree.ownership_epoch + 1, proof.proof_id],
            )
            .unwrap();
        }
        "wrong-pointer-id" => {
            raw.execute(
                "UPDATE process_trees SET termination_proof_id='wrong-proof-id' WHERE tree_id=?1",
                params![tree_id],
            )
            .unwrap();
        }
        "wrong-digest" => {
            raw.execute(
                "UPDATE termination_proofs SET proof_identity_digest='forged' WHERE proof_id=?1",
                params![proof.proof_id],
            )
            .unwrap();
        }
        "invalid-json" => {
            raw.execute(
                "UPDATE termination_proofs SET proof_identity_json='{not-json' WHERE proof_id=?1",
                params![proof.proof_id],
            )
            .unwrap();
        }
        "wrong-json" => {
            raw.execute(
                "UPDATE termination_proofs SET proof_identity_json='{}' WHERE proof_id=?1",
                params![proof.proof_id],
            )
            .unwrap();
        }
        "wrong-observed-boot" => {
            raw.execute(
                "UPDATE termination_proofs SET observed_boot_identity=?1 WHERE proof_id=?2",
                params![BOOT_B, proof.proof_id],
            )
            .unwrap();
        }
        "wrong-owner-pid" => {
            raw.execute(
                "UPDATE process_trees SET owner_pid=owner_pid+1 WHERE tree_id=?1",
                params![tree_id],
            )
            .unwrap();
        }
        "wrong-owner-start" => {
            raw.execute(
                "UPDATE process_trees SET owner_start_identity='999999' WHERE tree_id=?1",
                params![tree_id],
            )
            .unwrap();
        }
        "wrong-owner-boot" => {
            raw.execute(
                "UPDATE process_trees SET owner_boot_identity=?1 WHERE tree_id=?2",
                params![BOOT_B, tree_id],
            )
            .unwrap();
        }
        "wrong-platform-digest" => {
            raw.execute(
                "UPDATE process_trees SET platform_identity_digest='forged' WHERE tree_id=?1",
                params![tree_id],
            )
            .unwrap();
        }
        _ => panic!("unknown corruption {corruption}"),
    }
    drop(raw);
    (temp, store, workspace, tree_id)
}

#[test]
fn every_incomplete_or_tampered_exited_row_quarantines_all_write_leases() {
    for corruption in [
        "null-proof-id",
        "empty-proof-id",
        "missing-proof-row",
        "wrong-tree",
        "wrong-epoch",
        "wrong-pointer-id",
        "wrong-digest",
        "invalid-json",
        "wrong-json",
        "wrong-observed-boot",
        "wrong-owner-pid",
        "wrong-owner-start",
        "wrong-owner-boot",
        "wrong-platform-digest",
    ] {
        let (_temp, store, workspace, _tree_id) = corrupt_exited_fixture(corruption);
        assert_workspace_quarantined(&store, &workspace, corruption);
    }
}

#[test]
fn every_unproved_live_state_and_unmigrated_barrier_blocks_writes_not_reads() {
    for (suffix, target) in [
        ("prepared", ProcessTreeState::Prepared),
        ("running", ProcessTreeState::Running),
        ("terminating", ProcessTreeState::Terminating),
        ("quarantined", ProcessTreeState::Quarantined),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let store = V1Store::open(&database_path(&temp)).unwrap();
        let workspace = format!("workspace-{suffix}");
        let mut tree = prepare_tree(&store, &format!("tree-{suffix}"), &workspace, 60);
        if matches!(
            target,
            ProcessTreeState::Running | ProcessTreeState::Terminating
        ) {
            tree = running(&store, tree);
        }
        if target == ProcessTreeState::Terminating {
            tree = transition(&store, &tree, ProcessTreeState::Terminating, None, None);
        }
        if target == ProcessTreeState::Quarantined {
            tree = transition(
                &store,
                &tree,
                ProcessTreeState::Quarantined,
                Some("unproved"),
                None,
            );
        }
        assert_eq!(tree.state, target);
        assert_workspace_quarantined(&store, &workspace, suffix);
    }

    let temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&database_path(&temp)).unwrap();
    store
        .save_writer_barrier("unmigrated", "workspace-legacy-raw", 77, "88", "lost")
        .unwrap();
    assert_workspace_quarantined(&store, "workspace-legacy-raw", "legacy-raw");

    let temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&database_path(&temp)).unwrap();
    exited_tree(&store, "tree-proved", "workspace-proved", 61);
    store
        .acquire_lease(lease_request(
            "workspace-proved",
            "write-after-proof",
            false,
            false,
        ))
        .expect("complete proof releases write quarantine");
}

#[allow(deprecated)]
#[test]
fn legacy_migration_is_canonical_idempotent_atomic_and_requires_changed_boot() {
    let temp = tempfile::tempdir().unwrap();
    let path = database_path(&temp);
    let store = V1Store::open(&path).unwrap();
    store
        .save_writer_barrier(
            "legacy-valid",
            "workspace-legacy-valid",
            501,
            "9001",
            "daemon disappeared",
        )
        .unwrap();
    store
        .save_writer_barrier(
            "legacy-corrupt",
            "workspace-legacy-corrupt",
            502,
            "not-a-start-identity",
            "bad legacy row",
        )
        .unwrap();

    for invalid_boot in [
        "",
        "windows:00000000-0000-0000-0000-000000000000",
        "windows:01234567-89AB-4CDE-8F01-23456789ABCD",
        "macos:0:000000",
    ] {
        assert_eq!(
            store.migrate_legacy_writer_barriers(invalid_boot),
            Err(ProcessTreeStoreError::InvalidInput)
        );
    }
    assert_eq!(store.migrate_legacy_writer_barriers(BOOT_A).unwrap(), 2);
    assert_eq!(store.migrate_legacy_writer_barriers(BOOT_A).unwrap(), 0);

    let valid = store
        .process_trees_for_workspace("workspace-legacy-valid")
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(valid.state, ProcessTreeState::LegacyUnverifiable);
    assert_eq!(valid.owner.pid, 501);
    assert_eq!(valid.owner.start_identity, 9001);
    assert_eq!(
        valid.migrated_observed_boot_identity.as_deref(),
        Some(BOOT_A)
    );
    assert_eq!(valid.legacy_barrier_id.as_deref(), Some("legacy-valid"));
    assert_eq!(
        valid.quarantine_reason.as_deref(),
        Some("legacy-writer-barrier")
    );

    let corrupt = store
        .process_trees_for_workspace("workspace-legacy-corrupt")
        .unwrap()
        .pop()
        .unwrap();
    assert_eq!(corrupt.state, ProcessTreeState::LegacyUnverifiable);
    assert_eq!(corrupt.owner.pid, 502);
    assert_eq!(corrupt.owner.start_identity, 0);
    assert_eq!(
        corrupt.quarantine_reason.as_deref(),
        Some("legacy-corrupt-record")
    );
    assert_eq!(
        corrupt.migrated_observed_boot_identity.as_deref(),
        Some(BOOT_A)
    );
    assert!(!store.clear_writer_barrier("legacy-valid").unwrap());
    assert!(!store.clear_writer_barrier("legacy-corrupt").unwrap());

    let same_boot = NewTerminationProof::bound_to_tree(
        "proof-legacy-same-boot",
        TerminationProofKind::Reboot,
        BOOT_A,
        &valid,
        json!({"bootChanged": false}),
    );
    assert_eq!(
        store.transition_process_tree(
            &valid.tree_id,
            &valid.fence(),
            ProcessTreeState::LegacyUnverifiable,
            ProcessTreeState::Exited,
            None,
            Some(&same_boot),
        ),
        Err(ProcessTreeStoreError::InvalidProof)
    );

    let changed_boot = NewTerminationProof::bound_to_tree(
        "proof-legacy-changed-boot",
        TerminationProofKind::Reboot,
        BOOT_B,
        &valid,
        json!({"bootChanged": true, "observed": BOOT_B}),
    );
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch(&format!(
        "CREATE TRIGGER fail_legacy_exit BEFORE UPDATE OF state ON process_trees
         WHEN OLD.tree_id = '{}' AND NEW.state = 'exited'
         BEGIN SELECT RAISE(ABORT, 'injected legacy exit fault'); END;",
        valid.tree_id.replace('\'', "''")
    ))
    .unwrap();
    drop(raw);
    assert!(matches!(
        store.transition_process_tree(
            &valid.tree_id,
            &valid.fence(),
            ProcessTreeState::LegacyUnverifiable,
            ProcessTreeState::Exited,
            None,
            Some(&changed_boot),
        ),
        Err(ProcessTreeStoreError::Sqlite(_))
    ));
    assert_eq!(
        store
            .load_termination_proof("proof-legacy-changed-boot")
            .unwrap(),
        None,
        "failed transaction leaked its proof row"
    );
    assert_eq!(
        store
            .writer_barriers("workspace-legacy-valid")
            .unwrap()
            .len(),
        1,
        "failed transaction deleted the legacy barrier"
    );

    let raw = Connection::open(&path).unwrap();
    raw.execute("DROP TRIGGER fail_legacy_exit", []).unwrap();
    drop(raw);
    let exited = store
        .transition_process_tree(
            &valid.tree_id,
            &valid.fence(),
            ProcessTreeState::LegacyUnverifiable,
            ProcessTreeState::Exited,
            None,
            Some(&changed_boot),
        )
        .unwrap();
    assert_eq!(exited.state, ProcessTreeState::Exited);
    assert_eq!(
        exited.termination_proof_id.as_deref(),
        Some("proof-legacy-changed-boot")
    );
    assert!(store
        .writer_barriers("workspace-legacy-valid")
        .unwrap()
        .is_empty());
    store
        .acquire_lease(lease_request(
            "workspace-legacy-valid",
            "write-after-reboot-proof",
            false,
            false,
        ))
        .expect("changed-boot proof atomically releases legacy quarantine");

    let corrupt_proof = NewTerminationProof::bound_to_tree(
        "proof-corrupt-legacy",
        TerminationProofKind::Reboot,
        BOOT_B,
        &corrupt,
        json!({"bootChanged": true}),
    );
    assert_eq!(
        store.transition_process_tree(
            &corrupt.tree_id,
            &corrupt.fence(),
            ProcessTreeState::LegacyUnverifiable,
            ProcessTreeState::Exited,
            None,
            Some(&corrupt_proof),
        ),
        Err(ProcessTreeStoreError::InvalidProof)
    );
    assert_workspace_quarantined(&store, "workspace-legacy-corrupt", "corrupt-legacy");

    let migration_rows: i64 = Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM v1_schema_migrations
             WHERE migration_id='legacy-writer-barriers-to-process-trees'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(migration_rows, 1);
}

#[test]
fn application_composition_proves_boot_and_migrates_before_service_exposure() {
    let source = include_str!("../../r-code-runtime/src/application.rs");
    let function = source
        .split_once("pub fn compose_with_policy_and_approval_timeout")
        .expect("composition function")
        .1;
    let open = function
        .find("let store = V1Store::open")
        .expect("store opens first");
    let boot = function
        .find("let boot_identity = BootIdentity::current()")
        .expect("composition requires authoritative boot identity");
    let migrate = function
        .find(".migrate_legacy_writer_barriers(boot_identity.as_str())")
        .expect("composition migrates legacy barriers");
    let expose = function
        .find("let store = Arc::new(store)")
        .expect("store service exposure boundary");
    let first_service = function
        .find("KernelTaskService::new")
        .expect("first composed service");
    assert!(open < boot && boot < migrate && migrate < expose && expose < first_service);

    let guarded_region = &function[boot..expose];
    assert!(guarded_region.contains("ApplicationError::Store"));
    assert!(guarded_region.contains("boot identity:"));
    assert!(guarded_region.contains("legacy writer quarantine migration:"));
    assert!(guarded_region.matches('?').count() >= 2);
    assert!(!guarded_region.contains("unwrap_or"));
    assert!(!guarded_region.contains("unwrap_or_else"));
}
