use r_code_store::v1::{
    LeaseRequest, MutationError, MutationFile, MutationOperation, MutationState, V1Store,
};
use rusqlite::{params, Connection};
use std::path::PathBuf;
use std::sync::{Arc, Barrier};

fn database_path(temp: &tempfile::TempDir) -> PathBuf {
    temp.path().join("store.db")
}

fn request(
    operation_id: &str,
    owner_id: &str,
    read_paths: &[&str],
    write_paths: &[&str],
) -> LeaseRequest {
    LeaseRequest {
        workspace_key: "workspace-1".into(),
        operation_id: operation_id.into(),
        owner_id: owner_id.into(),
        read_paths: read_paths.iter().map(|path| (*path).into()).collect(),
        write_paths: write_paths.iter().map(|path| (*path).into()).collect(),
        repo_exclusive: false,
    }
}

fn repo_request(operation_id: &str, owner_id: &str) -> LeaseRequest {
    LeaseRequest {
        repo_exclusive: true,
        ..request(operation_id, owner_id, &[], &[])
    }
}

fn prepared(
    operation_id: &str,
    lease: &r_code_store::v1::LeaseGrant,
    paths: &[&str],
) -> MutationOperation {
    MutationOperation {
        operation_id: operation_id.into(),
        workspace_key: lease.request.workspace_key.clone(),
        lease_id: lease.lease_id.clone(),
        owner_id: lease.request.owner_id.clone(),
        fencing_epoch: lease.fencing_epoch,
        input_hash: format!("sha256:input-{operation_id}"),
        state: MutationState::Prepared,
        files: paths
            .iter()
            .map(|path| MutationFile {
                logical_path: (*path).into(),
                before_sha256: None,
                after_sha256: None,
                before_cas_ref: None,
                after_cas_ref: None,
            })
            .collect(),
    }
}

fn applied_file(path: &str, marker: char) -> MutationFile {
    let before = marker.to_string().repeat(64);
    let after_marker = if marker == 'a' { 'b' } else { 'c' };
    MutationFile {
        logical_path: path.into(),
        before_sha256: Some(format!("sha256:{before}")),
        after_sha256: Some(format!("sha256:{}", after_marker.to_string().repeat(64))),
        before_cas_ref: Some(format!("cas:before-{marker}")),
        after_cas_ref: Some(format!("cas:after-{marker}")),
    }
}

#[test]
fn canonical_lease_requests_are_sorted_deduplicated_and_idempotent() {
    let temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&database_path(&temp)).unwrap();
    let grant = store
        .acquire_lease(request(
            "lease-request-1",
            "owner-1",
            &["tests", "./src//lib.rs", r"src\lib.rs", "tests/./"],
            &["generated\\out.rs", "generated/out.rs"],
        ))
        .unwrap();
    assert_eq!(grant.request.read_paths, ["src/lib.rs", "tests"]);
    assert_eq!(grant.request.write_paths, ["generated/out.rs"]);

    let replay = store
        .acquire_lease(request(
            "lease-request-1",
            "owner-1",
            &["src/lib.rs", "tests"],
            &["generated/out.rs"],
        ))
        .unwrap();
    assert_eq!(replay, grant);

    let different = store.acquire_lease(request(
        "lease-request-1",
        "owner-1",
        &["src/other.rs"],
        &["generated/out.rs"],
    ));
    assert_eq!(different, Err(MutationError::OperationConflict));
}

#[cfg(windows)]
#[test]
fn windows_lease_paths_have_one_case_folded_identity() {
    let temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&database_path(&temp)).unwrap();
    let grant = store
        .acquire_lease(request(
            "lease-case",
            "owner",
            &[],
            &["SRC/Lib.RS", "src/lib.rs"],
        ))
        .unwrap();
    assert_eq!(grant.request.write_paths, ["src/lib.rs"]);
}

#[test]
fn read_write_overlap_is_rejected_before_persistence() {
    let temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&database_path(&temp)).unwrap();
    for (index, (read, write)) in [("src", "src"), ("src", "src/lib.rs"), ("src/lib.rs", "src")]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            store.acquire_lease(request(
                &format!("contradiction-{index}"),
                "owner",
                &[read],
                &[write],
            )),
            Err(MutationError::ConflictingScope)
        );
    }
    assert!(store.active_leases("workspace-1").unwrap().is_empty());
}

#[test]
fn lease_conflicts_follow_hierarchy_and_component_boundaries() {
    let temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&database_path(&temp)).unwrap();
    let read_a = store
        .acquire_lease(request("read-a", "reader-a", &["src"], &[]))
        .unwrap();
    let read_b = store
        .acquire_lease(request("read-b", "reader-b", &["src/lib.rs"], &[]))
        .unwrap();
    assert_ne!(read_a.lease_id, read_b.lease_id, "reads must share");

    assert!(matches!(
        store.acquire_lease(request("write-child", "writer", &[], &["src/main.rs"])),
        Err(MutationError::LeaseConflict { .. })
    ));
    assert!(matches!(
        store.acquire_lease(request("write-parent", "writer", &[], &["src"])),
        Err(MutationError::LeaseConflict { .. })
    ));

    store
        .acquire_lease(request("sibling", "writer", &[], &["src2/main.rs"]))
        .expect("src2 is not beneath src");
    store
        .acquire_lease(request("disjoint", "writer", &[], &["docs/readme.md"]))
        .expect("disjoint paths coexist");
    assert!(matches!(
        store.acquire_lease(repo_request("repo", "repo-owner")),
        Err(MutationError::LeaseConflict { .. })
    ));
}

#[test]
fn fencing_is_monotonic_owner_checked_and_restart_durable() {
    let temp = tempfile::tempdir().unwrap();
    let path = database_path(&temp);
    let first = {
        let store = V1Store::open(&path).unwrap();
        let first = store
            .acquire_lease(request("first", "owner-1", &[], &["src/a.rs"]))
            .unwrap();
        assert_eq!(
            store.release_lease(&first.lease_id, "wrong-owner", first.fencing_epoch),
            Err(MutationError::StaleLease)
        );
        assert_eq!(
            store.release_lease(&first.lease_id, "owner-1", first.fencing_epoch + 1),
            Err(MutationError::StaleLease)
        );
        first
    };

    let store = V1Store::open(&path).unwrap();
    assert_eq!(
        store.active_leases("workspace-1").unwrap(),
        std::slice::from_ref(&first)
    );
    assert!(store
        .release_lease(&first.lease_id, "owner-1", first.fencing_epoch)
        .unwrap());
    assert!(!store
        .release_lease(&first.lease_id, "owner-1", first.fencing_epoch)
        .unwrap());
    let second = store
        .acquire_lease(request("second", "owner-2", &[], &["src/a.rs"]))
        .unwrap();
    assert!(second.fencing_epoch > first.fencing_epoch);
}

#[test]
fn concurrent_immediate_transactions_have_exactly_one_legal_winner() {
    let temp = tempfile::tempdir().unwrap();
    let path = database_path(&temp);
    let left_store = V1Store::open(&path).unwrap();
    let right_store = V1Store::open(&path).unwrap();
    let barrier = Arc::new(Barrier::new(2));

    let left_barrier = Arc::clone(&barrier);
    let left = std::thread::spawn(move || {
        left_barrier.wait();
        left_store.acquire_lease(request("concurrent-left", "left", &[], &["src"]))
    });
    let right_barrier = Arc::clone(&barrier);
    let right = std::thread::spawn(move || {
        right_barrier.wait();
        right_store.acquire_lease(request("concurrent-right", "right", &[], &["src/lib.rs"]))
    });

    let outcomes = [left.join().unwrap(), right.join().unwrap()];
    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| matches!(outcome, Err(MutationError::LeaseConflict { .. })))
            .count(),
        1,
        "the serialized loser must observe the committed holder: {outcomes:?}"
    );
}

#[test]
fn mutation_prepare_requires_an_active_fenced_covering_write_lease() {
    let temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&database_path(&temp)).unwrap();
    let write = store
        .acquire_lease(request("write", "owner", &[], &["src"]))
        .unwrap();
    let covered = prepared("covered", &write, &["src/lib.rs"]);
    assert_eq!(store.prepare_operation(&covered).unwrap(), covered);

    let prefix_sibling = prepared("prefix-sibling", &write, &["src2/lib.rs"]);
    assert_eq!(
        store.prepare_operation(&prefix_sibling),
        Err(MutationError::FileNotCovered)
    );
    let read = store
        .acquire_lease(request("read", "reader", &["docs"], &[]))
        .unwrap();
    assert_eq!(
        store.prepare_operation(&prepared("read-write", &read, &["docs/readme.md"])),
        Err(MutationError::FileNotCovered)
    );

    let repo = store
        .acquire_lease(repo_request("repo-write", "repo-owner"))
        .unwrap_err();
    assert!(matches!(repo, MutationError::LeaseConflict { .. }));

    store
        .release_lease(&write.lease_id, "owner", write.fencing_epoch)
        .unwrap();
    assert_eq!(
        store.mark_applied(
            "covered",
            "owner",
            write.fencing_epoch,
            vec![applied_file("src/lib.rs", 'a')],
        ),
        Err(MutationError::LeaseInactive)
    );
    assert_eq!(
        store.prepare_operation(&prepared("inactive", &write, &["src/new.rs"])),
        Err(MutationError::LeaseInactive)
    );
}

#[test]
fn repository_lease_covers_safe_paths_and_operation_identity_is_immutable() {
    let temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&database_path(&temp)).unwrap();
    let repo = store.acquire_lease(repo_request("repo", "owner")).unwrap();
    let operation = prepared("mutation-1", &repo, &["src/lib.rs", "docs/readme.md"]);
    let first = store.prepare_operation(&operation).unwrap();
    assert_eq!(store.prepare_operation(&operation).unwrap(), first);

    let mut different_input = operation.clone();
    different_input.input_hash.push_str("-different");
    assert_eq!(
        store.prepare_operation(&different_input),
        Err(MutationError::OperationConflict)
    );
    let mut different_files = operation.clone();
    different_files.files.pop();
    assert_eq!(
        store.prepare_operation(&different_files),
        Err(MutationError::OperationConflict)
    );
}

#[test]
fn mutation_state_machine_is_idempotent_fenced_and_conflict_terminal() {
    let temp = tempfile::tempdir().unwrap();
    let path = database_path(&temp);
    let final_operation = {
        let store = V1Store::open(&path).unwrap();
        let lease = store
            .acquire_lease(request("lease", "owner", &[], &["src"]))
            .unwrap();
        store
            .prepare_operation(&prepared("operation", &lease, &["src/lib.rs"]))
            .unwrap();
        assert!(matches!(
            store.mark_receipted("operation", "owner", lease.fencing_epoch),
            Err(MutationError::InvalidTransition {
                expected: MutationState::Applied,
                actual: MutationState::Prepared
            })
        ));
        assert_eq!(
            store.mark_applied(
                "operation",
                "wrong-owner",
                lease.fencing_epoch,
                vec![applied_file("src/lib.rs", 'a')],
            ),
            Err(MutationError::StaleLease)
        );
        assert_eq!(
            store.mark_applied(
                "operation",
                "owner",
                lease.fencing_epoch + 1,
                vec![applied_file("src/lib.rs", 'a')],
            ),
            Err(MutationError::StaleLease)
        );
        let applied = store
            .mark_applied(
                "operation",
                "owner",
                lease.fencing_epoch,
                vec![applied_file("src/lib.rs", 'a')],
            )
            .unwrap();
        assert_eq!(
            store
                .mark_applied(
                    "operation",
                    "owner",
                    lease.fencing_epoch,
                    vec![applied_file("src/lib.rs", 'a')],
                )
                .unwrap(),
            applied
        );
        assert!(matches!(
            store.mark_applied(
                "operation",
                "owner",
                lease.fencing_epoch,
                vec![applied_file("src/lib.rs", 'd')],
            ),
            Err(MutationError::InvalidTransition { .. })
        ));
        let receipted = store
            .mark_receipted("operation", "owner", lease.fencing_epoch)
            .unwrap();
        assert_eq!(
            store
                .mark_receipted("operation", "owner", lease.fencing_epoch)
                .unwrap(),
            receipted
        );
        assert!(matches!(
            store.mark_conflict("operation", "owner", lease.fencing_epoch),
            Err(MutationError::InvalidTransition { .. })
        ));

        store
            .prepare_operation(&prepared("conflict", &lease, &["src/conflict.rs"]))
            .unwrap();
        let conflict = store
            .mark_conflict("conflict", "owner", lease.fencing_epoch)
            .unwrap();
        assert_eq!(
            store
                .mark_conflict("conflict", "owner", lease.fencing_epoch)
                .unwrap(),
            conflict
        );
        assert!(matches!(
            store.mark_receipted("conflict", "owner", lease.fencing_epoch),
            Err(MutationError::InvalidTransition { .. })
        ));
        assert!(matches!(
            store.mark_applied(
                "conflict",
                "owner",
                lease.fencing_epoch,
                vec![applied_file("src/conflict.rs", 'a')],
            ),
            Err(MutationError::InvalidTransition { .. })
        ));
        receipted
    };

    let reopened = V1Store::open(&path).unwrap();
    assert_eq!(
        reopened.load_mutation_operation("operation").unwrap(),
        Some(final_operation)
    );
    assert_eq!(
        reopened
            .load_mutation_operation("conflict")
            .unwrap()
            .unwrap()
            .state,
        MutationState::Conflict
    );
}

#[test]
fn invalid_hash_and_cas_pairs_never_advance_prepared_operation() {
    let temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&database_path(&temp)).unwrap();
    let lease = store
        .acquire_lease(request("lease", "owner", &[], &["src"]))
        .unwrap();
    store
        .prepare_operation(&prepared("operation", &lease, &["src/lib.rs"]))
        .unwrap();

    let invalid_files = [
        MutationFile {
            logical_path: "src/lib.rs".into(),
            before_sha256: Some("not-a-hash".into()),
            before_cas_ref: Some("cas:before".into()),
            after_sha256: None,
            after_cas_ref: None,
        },
        MutationFile {
            logical_path: "src/lib.rs".into(),
            before_sha256: Some(format!("sha256:{}", "A".repeat(64))),
            before_cas_ref: Some("cas:before".into()),
            after_sha256: None,
            after_cas_ref: None,
        },
        MutationFile {
            logical_path: "src/lib.rs".into(),
            before_sha256: Some(format!("sha256:{}", "a".repeat(64))),
            before_cas_ref: None,
            after_sha256: None,
            after_cas_ref: None,
        },
    ];
    for file in invalid_files {
        assert_eq!(
            store.mark_applied("operation", "owner", lease.fencing_epoch, vec![file],),
            Err(MutationError::InvalidFileIdentity)
        );
    }
    assert_eq!(
        store
            .load_mutation_operation("operation")
            .unwrap()
            .unwrap()
            .state,
        MutationState::Prepared
    );
}

#[test]
fn prepare_fault_rolls_back_operation_and_all_file_rows() {
    let temp = tempfile::tempdir().unwrap();
    let path = database_path(&temp);
    let store = V1Store::open(&path).unwrap();
    let lease = store
        .acquire_lease(request("lease", "owner", &[], &["src"]))
        .unwrap();
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch(
        "CREATE TRIGGER fail_second_file BEFORE INSERT ON mutation_files
         WHEN NEW.logical_path = 'src/z.rs'
         BEGIN SELECT RAISE(ABORT, 'injected'); END;",
    )
    .unwrap();

    assert!(matches!(
        store.prepare_operation(&prepared("faulted", &lease, &["src/a.rs", "src/z.rs"])),
        Err(MutationError::Sqlite(_))
    ));
    assert_eq!(store.load_mutation_operation("faulted").unwrap(), None);
    let operation_rows: i64 = raw
        .query_row(
            "SELECT COUNT(*) FROM mutation_operations WHERE operation_id = 'faulted'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let file_rows: i64 = raw
        .query_row(
            "SELECT COUNT(*) FROM mutation_files WHERE operation_id = 'faulted'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!((operation_rows, file_rows), (0, 0));
}

#[test]
fn applied_fault_rolls_back_all_file_identities_and_state() {
    let temp = tempfile::tempdir().unwrap();
    let path = database_path(&temp);
    let store = V1Store::open(&path).unwrap();
    let lease = store
        .acquire_lease(request("lease", "owner", &[], &["src"]))
        .unwrap();
    store
        .prepare_operation(&prepared("faulted", &lease, &["src/a.rs", "src/b.rs"]))
        .unwrap();
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch(
        "CREATE TRIGGER fail_applied_state BEFORE UPDATE OF state ON mutation_operations
         WHEN NEW.state = 'applied'
         BEGIN SELECT RAISE(ABORT, 'injected'); END;",
    )
    .unwrap();

    assert!(matches!(
        store.mark_applied(
            "faulted",
            "owner",
            lease.fencing_epoch,
            vec![applied_file("src/a.rs", 'a'), applied_file("src/b.rs", 'd')],
        ),
        Err(MutationError::Sqlite(_))
    ));
    let operation = store.load_mutation_operation("faulted").unwrap().unwrap();
    assert_eq!(operation.state, MutationState::Prepared);
    assert!(operation.files.iter().all(|file| {
        file.before_sha256.is_none()
            && file.after_sha256.is_none()
            && file.before_cas_ref.is_none()
            && file.after_cas_ref.is_none()
    }));
}

#[test]
fn sqlite_enforces_mutation_file_foreign_key_and_uniqueness() {
    let temp = tempfile::tempdir().unwrap();
    let path = database_path(&temp);
    let store = V1Store::open(&path).unwrap();
    let lease = store
        .acquire_lease(request("lease", "owner", &[], &["src"]))
        .unwrap();
    store
        .prepare_operation(&prepared("operation", &lease, &["src/lib.rs"]))
        .unwrap();
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch("PRAGMA foreign_keys = ON;").unwrap();

    assert!(raw
        .execute(
            "INSERT INTO mutation_files(operation_id, logical_path) VALUES (?1, ?2)",
            params!["missing-operation", "src/missing.rs"],
        )
        .is_err());
    assert!(raw
        .execute(
            "INSERT INTO mutation_files(operation_id, logical_path) VALUES (?1, ?2)",
            params!["operation", "src/lib.rs"],
        )
        .is_err());
}

#[test]
fn validation_errors_are_redacted_and_store_never_touches_workspace_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&database_path(&temp)).unwrap();
    let secret_path = "src/private-secret/../escape.rs";
    let error = store
        .acquire_lease(request("bad", "owner", &[], &[secret_path]))
        .unwrap_err();
    let MutationError::InvalidPath(reason) = &error else {
        panic!("expected redacted invalid path, got {error:?}");
    };
    assert!(!reason.contains(secret_path));
    assert!(!format!("{error:?} {error}").contains("private-secret"));

    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir(workspace.path().join("src")).unwrap();
    let file = workspace.path().join("src/lib.rs");
    std::fs::write(&file, b"user-owned bytes").unwrap();
    let before = std::fs::read(&file).unwrap();
    let lease = store
        .acquire_lease(request("safe", "owner", &[], &["src"]))
        .unwrap();
    store
        .prepare_operation(&prepared("no-effect", &lease, &["src/lib.rs"]))
        .unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), before);
}
