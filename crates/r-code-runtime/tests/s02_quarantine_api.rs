//! P02 — read-only, redacted quarantine diagnostics.

use r_code_client::{ClientError, DaemonClient};
use r_code_runtime::application::{ApplicationService, QuarantineRemediation, QuarantineView};
use r_code_runtime::process_guard::BootIdentity;
use r_code_runtime::remote::capabilities::{required_capability, Capability};
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use r_code_store::v1::operations::{
    NewTerminationProof, PrepareProcessTree, ProcessTreeOwner, ProcessTreeRecord, ProcessTreeState,
    QuarantineDiagnosticRecord, QuarantineProofStatus, QuarantineTreeState, TerminationProofKind,
};
use r_code_store::v1::{LeaseRequest, MutationError, V1Store};
use rusqlite::types::Value as SqlValue;
use rusqlite::{params, params_from_iter, Connection};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

const SERVICE: &str = env!("CARGO_BIN_EXE_r-code-service");
const BOOT_A: &str = "windows:01234567-89ab-4cde-8f01-23456789abcd";
const BOOT_B: &str = "windows:01234567-89ab-4cde-8f01-23456789abce";

fn database_path(temp: &tempfile::TempDir) -> PathBuf {
    temp.path().join("store.db")
}

fn digest(value: &Value) -> String {
    r_code_harness_protocol::canonical_input_hash(value)
}

fn opaque_text(value: &str) -> String {
    format!(
        "sha256:{}",
        digest(&serde_json::Value::String(value.to_string()))
    )
}

fn is_opaque(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn owner(seed: u32) -> ProcessTreeOwner {
    let platform_identity = json!({
        "privateHandle": format!("HANDLE-SECRET-{seed}"),
        "nativePid": 900_000 + seed,
    });
    ProcessTreeOwner {
        pid: 700_000 + seed,
        start_identity: 8_000_000_000 + u64::from(seed),
        boot_identity: BOOT_A.to_string(),
        platform_identity_digest: digest(&platform_identity),
        platform_identity,
    }
}

fn input(tree_id: &str, workspace_key: &str, profile_id: &str, seed: u32) -> PrepareProcessTree {
    PrepareProcessTree {
        tree_id: tree_id.to_string(),
        attempt_id: format!("attempt-secret-{tree_id}"),
        workspace_key: workspace_key.to_string(),
        profile_id: profile_id.to_string(),
        owner: owner(seed),
    }
}

fn prepare(
    store: &V1Store,
    tree_id: &str,
    workspace_key: &str,
    profile_id: &str,
    seed: u32,
) -> ProcessTreeRecord {
    store
        .prepare_process_tree(&input(tree_id, workspace_key, profile_id, seed))
        .unwrap()
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
        .unwrap()
}

fn running(store: &V1Store, tree: ProcessTreeRecord) -> ProcessTreeRecord {
    transition(store, &tree, ProcessTreeState::Running, None, None)
}

fn terminating(store: &V1Store, tree: ProcessTreeRecord) -> ProcessTreeRecord {
    let tree = running(store, tree);
    transition(store, &tree, ProcessTreeState::Terminating, None, None)
}

fn proved_exit(store: &V1Store, tree: ProcessTreeRecord, proof_id: &str) -> ProcessTreeRecord {
    let tree = terminating(store, tree);
    let proof = NewTerminationProof::bound_to_tree(
        proof_id,
        TerminationProofKind::Exit,
        tree.owner.boot_identity.clone(),
        &tree,
        json!({"nativeExit": true}),
    );
    transition(store, &tree, ProcessTreeState::Exited, None, Some(&proof))
}

fn find_record<'a>(
    records: &'a [QuarantineDiagnosticRecord],
    raw_tree_id: &str,
) -> &'a QuarantineDiagnosticRecord {
    let tree_ref = opaque_text(raw_tree_id);
    records
        .iter()
        .find(|record| record.tree_ref == tree_ref)
        .unwrap_or_else(|| panic!("missing diagnostic for {raw_tree_id}"))
}

fn assert_record_is_opaque(record: &QuarantineDiagnosticRecord) {
    assert!(is_opaque(&record.tree_ref));
    assert!(is_opaque(&record.workspace_ref));
    assert!(is_opaque(&record.profile_ref));
    assert!(is_opaque(&record.owner_fingerprint));
    assert!(is_opaque(&record.reason_digest));
}

#[test]
fn store_projects_every_unproved_state_filters_sorts_and_omits_only_proved_exit() {
    let temp = tempfile::tempdir().unwrap();
    let path = database_path(&temp);
    let store = V1Store::open(&path).unwrap();

    let prepared = prepare(
        &store,
        "tree-secret-prepared",
        "C:/private/workspace-prepared",
        "profile-secret-prepared",
        1,
    );
    let running = running(
        &store,
        prepare(
            &store,
            "tree-secret-running",
            "C:/private/workspace-running",
            "profile-secret-running",
            2,
        ),
    );
    let terminating = terminating(
        &store,
        prepare(
            &store,
            "tree-secret-terminating",
            "C:/private/workspace-terminating",
            "profile-secret-terminating",
            3,
        ),
    );
    let quarantined = transition(
        &store,
        &prepare(
            &store,
            "tree-secret-quarantined",
            "C:/private/workspace-quarantined",
            "profile-secret-quarantined",
            4,
        ),
        ProcessTreeState::Quarantined,
        Some("reason-secret-process-handle-lost"),
        None,
    );

    store
        .save_writer_barrier(
            "legacy-barrier-secret",
            "C:/private/workspace-legacy",
            777_777,
            "888888888888",
            "reason-secret-legacy",
        )
        .unwrap();
    store.migrate_legacy_writer_barriers(BOOT_A).unwrap();
    let legacy = store
        .process_trees_for_workspace("C:/private/workspace-legacy")
        .unwrap()
        .pop()
        .unwrap();

    let invalid_exited = proved_exit(
        &store,
        prepare(
            &store,
            "tree-secret-invalid-exited",
            "C:/private/workspace-invalid-exited",
            "profile-secret-invalid-exited",
            5,
        ),
        "proof-secret-invalid-exited",
    );
    let valid_exited = proved_exit(
        &store,
        prepare(
            &store,
            "tree-secret-proved",
            "C:/private/workspace-proved",
            "profile-secret-proved",
            6,
        ),
        "proof-secret-proved",
    );
    let raw = Connection::open(&path).unwrap();
    raw.execute(
        "UPDATE process_trees SET termination_proof_id=NULL WHERE tree_id=?1",
        params![invalid_exited.tree_id],
    )
    .unwrap();
    drop(raw);

    let strict_workspace = format!("sha256:{}", "a".repeat(64));
    let strict = prepare(
        &store,
        "tree-strict-workspace",
        &strict_workspace,
        "profile-secret-strict",
        7,
    );
    let noncanonical_workspace = format!("sha256:{}", "A".repeat(64));
    let noncanonical = prepare(
        &store,
        "tree-noncanonical-workspace",
        &noncanonical_workspace,
        "profile-secret-noncanonical",
        8,
    );

    let records = store.quarantine_diagnostics(None).unwrap();
    assert_eq!(records.len(), 8);
    assert!(records
        .windows(2)
        .all(|pair| (&pair[0].workspace_ref, &pair[0].tree_ref)
            <= (&pair[1].workspace_ref, &pair[1].tree_ref)));
    assert_eq!(store.quarantine_diagnostics(None).unwrap(), records);

    assert_eq!(
        find_record(&records, &prepared.tree_id).state,
        QuarantineTreeState::Prepared
    );
    assert_eq!(
        find_record(&records, &running.tree_id).state,
        QuarantineTreeState::Running
    );
    assert_eq!(
        find_record(&records, &terminating.tree_id).state,
        QuarantineTreeState::Terminating
    );
    let projected_quarantine = find_record(&records, &quarantined.tree_id);
    assert_eq!(projected_quarantine.state, QuarantineTreeState::Quarantined);
    assert_eq!(projected_quarantine.reason_code, "termination-unproved");
    assert_ne!(
        projected_quarantine.reason_digest,
        "reason-secret-process-handle-lost"
    );
    let projected_legacy = find_record(&records, &legacy.tree_id);
    assert_eq!(
        projected_legacy.state,
        QuarantineTreeState::LegacyUnverifiable
    );
    assert!(projected_legacy.legacy);
    let projected_exited = find_record(&records, &invalid_exited.tree_id);
    assert_eq!(projected_exited.state, QuarantineTreeState::ExitedUnproved);
    assert_eq!(
        projected_exited.proof_status,
        QuarantineProofStatus::Missing
    );
    assert!(projected_exited.corrupt);
    assert!(!records
        .iter()
        .any(|record| record.tree_ref == opaque_text(&valid_exited.tree_id)));

    assert_eq!(
        find_record(&records, &strict.tree_id)
            .workspace_identity
            .as_deref(),
        Some(strict_workspace.as_str())
    );
    assert!(find_record(&records, &noncanonical.tree_id)
        .workspace_identity
        .is_none());

    for record in &records {
        assert_record_is_opaque(record);
    }
    let debug = format!("{records:?}");
    for secret in [
        "tree-secret",
        "attempt-secret",
        "C:/private",
        "profile-secret",
        "777777",
        "888888888888",
        BOOT_A,
        "HANDLE-SECRET",
        "reason-secret",
        "proof-secret",
        "legacy-barrier-secret",
    ] {
        assert!(!debug.contains(secret), "debug leaked {secret:?}: {debug}");
    }

    let filtered = store
        .quarantine_diagnostics(Some("C:/private/workspace-running"))
        .unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].tree_ref, opaque_text(&running.tree_id));
    assert!(store
        .quarantine_diagnostics(Some("C:/private/missing"))
        .unwrap()
        .is_empty());
}

fn create_relaxed_process_tree_database(path: &Path) {
    let raw = Connection::open(path).unwrap();
    raw.execute_batch(
        "CREATE TABLE process_trees (
            tree_id,
            attempt_id,
            workspace_key,
            profile_id,
            owner_pid,
            owner_start_identity,
            owner_boot_identity,
            platform_identity_json,
            platform_identity_digest,
            ownership_epoch,
            state,
            state_revision,
            quarantine_reason,
            migrated_observed_boot_identity,
            termination_proof_id,
            created_at_ms,
            updated_at_ms,
            legacy_barrier_id
        );",
    )
    .unwrap();
}

fn raw_row(tree_id: &str) -> Vec<SqlValue> {
    let platform = json!({"private": format!("platform-secret-{tree_id}")});
    vec![
        SqlValue::Text(tree_id.to_string()),
        SqlValue::Text(format!("attempt-secret-{tree_id}")),
        SqlValue::Text(format!("C:/private/{tree_id}")),
        SqlValue::Text(format!("profile-secret-{tree_id}")),
        SqlValue::Integer(765_432),
        SqlValue::Text("987654321012345678".to_string()),
        SqlValue::Text(BOOT_A.to_string()),
        SqlValue::Text(serde_json::to_string(&platform).unwrap()),
        SqlValue::Text(digest(&platform)),
        SqlValue::Integer(1),
        SqlValue::Text("prepared".to_string()),
        SqlValue::Integer(1),
        SqlValue::Null,
        SqlValue::Null,
        SqlValue::Null,
        SqlValue::Integer(10),
        SqlValue::Integer(10),
        SqlValue::Null,
    ]
}

fn insert_raw_row(connection: &Connection, row: Vec<SqlValue>) {
    connection
        .execute(
            "INSERT INTO process_trees(
                tree_id, attempt_id, workspace_key, profile_id, owner_pid,
                owner_start_identity, owner_boot_identity,
                platform_identity_json, platform_identity_digest,
                ownership_epoch, state, state_revision, quarantine_reason,
                migrated_observed_boot_identity, termination_proof_id,
                created_at_ms, updated_at_ms, legacy_barrier_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9,
                     ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            params_from_iter(row),
        )
        .unwrap();
}

#[test]
fn malformed_sqlite_cells_downgrade_one_row_without_hiding_healthy_rows() {
    let temp = tempfile::tempdir().unwrap();
    let path = database_path(&temp);
    create_relaxed_process_tree_database(&path);
    let store = V1Store::open(&path).unwrap();
    let raw = Connection::open(&path).unwrap();
    insert_raw_row(&raw, raw_row("tree-clean"));

    let corruptions: Vec<(&str, usize, SqlValue)> = vec![
        ("tree-blob", 0, SqlValue::Blob(b"tree-secret-blob".to_vec())),
        (
            "attempt-blob",
            1,
            SqlValue::Blob(b"attempt-secret-blob".to_vec()),
        ),
        ("workspace-null", 2, SqlValue::Null),
        ("profile-integer", 3, SqlValue::Integer(42)),
        ("pid-text", 4, SqlValue::Text("pid-secret".into())),
        (
            "pid-overflow",
            4,
            SqlValue::Integer(i64::from(u32::MAX) + 1),
        ),
        ("start-blob", 5, SqlValue::Blob(b"start-secret".to_vec())),
        ("boot-blob", 6, SqlValue::Blob(BOOT_A.as_bytes().to_vec())),
        (
            "platform-json",
            7,
            SqlValue::Text("{private-bad-json".into()),
        ),
        (
            "platform-digest",
            8,
            SqlValue::Text("wrong-digest-secret".into()),
        ),
        ("epoch-real", 9, SqlValue::Real(f64::MAX)),
        (
            "state-enum",
            10,
            SqlValue::Text("forged-state-secret".into()),
        ),
        ("revision-zero", 11, SqlValue::Integer(0)),
        (
            "reason-blob",
            12,
            SqlValue::Blob(b"reason-secret-blob".to_vec()),
        ),
        (
            "migrated-blob",
            13,
            SqlValue::Blob(BOOT_B.as_bytes().to_vec()),
        ),
        (
            "proof-blob",
            14,
            SqlValue::Blob(b"proof-secret-blob".to_vec()),
        ),
        (
            "created-blob",
            15,
            SqlValue::Blob(b"created-secret".to_vec()),
        ),
        ("updated-null", 16, SqlValue::Null),
        ("legacy-blob", 17, SqlValue::Blob(b"legacy-secret".to_vec())),
    ];
    for (name, index, value) in &corruptions {
        let mut row = raw_row(&format!("tree-{name}"));
        row[*index] = value.clone();
        insert_raw_row(&raw, row);
    }
    drop(raw);

    let records = store.quarantine_diagnostics(None).unwrap();
    assert_eq!(records.len(), corruptions.len() + 1);
    let clean = find_record(&records, "tree-clean");
    assert!(!clean.corrupt);
    assert_eq!(clean.state, QuarantineTreeState::Prepared);
    for record in records
        .iter()
        .filter(|record| record.tree_ref != clean.tree_ref)
    {
        assert!(record.corrupt, "row did not downgrade: {record:?}");
        assert_record_is_opaque(record);
    }

    let debug = format!("{records:?}");
    for secret in [
        "tree-secret",
        "attempt-secret",
        "C:/private",
        "profile-secret",
        "pid-secret",
        "start-secret",
        BOOT_A,
        BOOT_B,
        "platform-secret",
        "wrong-digest-secret",
        "forged-state-secret",
        "reason-secret",
        "proof-secret",
        "legacy-secret",
    ] {
        assert!(
            !debug.contains(secret),
            "corrupt projection leaked {secret:?}"
        );
    }

    let raw = Connection::open(&path).unwrap();
    raw.execute("DROP TABLE process_trees", []).unwrap();
    drop(raw);
    assert!(matches!(
        store.quarantine_diagnostics(None),
        Err(r_code_store::v1::operations::ProcessTreeStoreError::Sqlite(
            _
        ))
    ));
}

fn write_lease(workspace_key: &str, operation_id: &str) -> LeaseRequest {
    LeaseRequest {
        workspace_key: workspace_key.to_string(),
        operation_id: operation_id.to_string(),
        owner_id: format!("owner-{operation_id}"),
        read_paths: Vec::new(),
        write_paths: vec!["src/lib.rs".to_string()],
        repo_exclusive: false,
    }
}

#[test]
fn every_tampered_proof_row_stays_visible_corrupt_and_write_quarantined() {
    for corruption in [
        "missing-id",
        "missing-row",
        "wrong-tree",
        "wrong-epoch",
        "wrong-pointer",
        "wrong-digest",
        "bad-json",
        "wrong-boot",
        "blob-pointer",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let path = database_path(&temp);
        let store = V1Store::open(&path).unwrap();
        let workspace = format!("C:/private/proof-{corruption}");
        let tree_id = format!("tree-proof-{corruption}");
        let exited = proved_exit(
            &store,
            prepare(&store, &tree_id, &workspace, "profile-secret-proof", 50),
            &format!("proof-secret-{corruption}"),
        );
        let proof_id = exited.termination_proof_id.clone().unwrap();
        let other_tree = (corruption == "wrong-tree").then(|| {
            prepare(
                &store,
                "tree-proof-unrelated",
                "C:/private/proof-unrelated",
                "profile-secret-unrelated",
                51,
            )
            .tree_id
        });
        let raw = Connection::open(&path).unwrap();
        match corruption {
            "missing-id" => {
                raw.execute(
                    "UPDATE process_trees SET termination_proof_id=NULL WHERE tree_id=?1",
                    params![tree_id],
                )
                .unwrap();
            }
            "missing-row" => {
                raw.execute(
                    "DELETE FROM termination_proofs WHERE proof_id=?1",
                    params![proof_id],
                )
                .unwrap();
            }
            "wrong-tree" => {
                raw.execute(
                    "UPDATE termination_proofs SET tree_id=?1 WHERE proof_id=?2",
                    params![other_tree.unwrap(), proof_id],
                )
                .unwrap();
            }
            "wrong-epoch" => {
                raw.execute(
                    "UPDATE termination_proofs SET ownership_epoch=ownership_epoch+1
                     WHERE proof_id=?1",
                    params![proof_id],
                )
                .unwrap();
            }
            "wrong-pointer" => {
                raw.execute(
                    "UPDATE process_trees SET termination_proof_id='proof-secret-missing'
                     WHERE tree_id=?1",
                    params![tree_id],
                )
                .unwrap();
            }
            "wrong-digest" => {
                raw.execute(
                    "UPDATE termination_proofs SET proof_identity_digest='digest-secret-forged'
                     WHERE proof_id=?1",
                    params![proof_id],
                )
                .unwrap();
            }
            "bad-json" => {
                raw.execute(
                    "UPDATE termination_proofs SET proof_identity_json='{json-secret-bad'
                     WHERE proof_id=?1",
                    params![proof_id],
                )
                .unwrap();
            }
            "wrong-boot" => {
                raw.execute(
                    "UPDATE termination_proofs SET observed_boot_identity=?1 WHERE proof_id=?2",
                    params![BOOT_B, proof_id],
                )
                .unwrap();
            }
            "blob-pointer" => {
                raw.execute(
                    "UPDATE process_trees SET termination_proof_id=?1 WHERE tree_id=?2",
                    params![b"proof-secret-blob".as_slice(), tree_id],
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        drop(raw);

        let records = store.quarantine_diagnostics(Some(&workspace)).unwrap();
        assert_eq!(records.len(), 1, "corruption={corruption}");
        let record = &records[0];
        assert!(record.corrupt, "corruption was not marked: {corruption}");
        assert!(matches!(
            record.state,
            QuarantineTreeState::ExitedUnproved | QuarantineTreeState::Corrupt
        ));
        assert!(matches!(
            record.proof_status,
            QuarantineProofStatus::Missing
                | QuarantineProofStatus::Invalid
                | QuarantineProofStatus::Corrupt
        ));
        assert_record_is_opaque(record);
        let debug = format!("{record:?}");
        for secret in [
            &tree_id,
            &workspace,
            "profile-secret",
            "proof-secret",
            "digest-secret",
            "json-secret",
            BOOT_A,
            BOOT_B,
        ] {
            assert!(!debug.contains(secret), "{corruption} leaked {secret:?}");
        }
        assert_eq!(
            store.acquire_lease(write_lease(&workspace, &format!("op-{corruption}"))),
            Err(MutationError::WorkspaceQuarantined {
                workspace_key: workspace.clone(),
            })
        );
    }

    let temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&database_path(&temp)).unwrap();
    proved_exit(
        &store,
        prepare(
            &store,
            "tree-exact-proof",
            "C:/private/exact-proof",
            "profile-exact-proof",
            52,
        ),
        "proof-exact",
    );
    assert!(store
        .quarantine_diagnostics(Some("C:/private/exact-proof"))
        .unwrap()
        .is_empty());
}

fn profile_for(name: &str, root: &Path) -> RuntimeProfile {
    RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(root.join("root"))
            .with_ipc_name(name),
    )
    .unwrap()
}

fn compose(profile: &RuntimeProfile) -> ApplicationService {
    ApplicationService::compose(
        profile,
        Arc::new(r_code_kernel::testing::FakeModelService::default()),
        Arc::new(r_code_kernel::testing::FakeToolService::default()),
    )
    .unwrap()
}

fn different_boot(current: &str) -> &'static str {
    if current == BOOT_A {
        BOOT_B
    } else {
        BOOT_A
    }
}

fn remediation_count(views: &[QuarantineView], expected: QuarantineRemediation) -> usize {
    views
        .iter()
        .filter(|view| view.remediation == expected)
        .count()
}

#[test]
fn application_remediation_is_boot_aware_manual_for_corrupt_and_never_leaks_identity() {
    let temp = tempfile::tempdir().unwrap();
    let profile = profile_for("s02-remediation", temp.path());
    let current_boot = BootIdentity::current().unwrap();
    let store = V1Store::open(&profile.database_path()).unwrap();
    prepare(
        &store,
        "tree-secret-ordinary",
        "C:/private/remediation-ordinary",
        "profile-secret-ordinary",
        60,
    );
    store
        .save_writer_barrier(
            "barrier-secret-changed",
            "C:/private/remediation-changed",
            610_001,
            "710000000001",
            "reason-secret-changed",
        )
        .unwrap();
    store
        .migrate_legacy_writer_barriers(different_boot(current_boot.as_str()))
        .unwrap();
    store
        .save_writer_barrier(
            "barrier-secret-same",
            "C:/private/remediation-same",
            610_002,
            "710000000002",
            "reason-secret-same",
        )
        .unwrap();
    store
        .save_writer_barrier(
            "barrier-secret-corrupt",
            "C:/private/remediation-corrupt",
            610_003,
            "invalid-start-secret",
            "reason-secret-corrupt",
        )
        .unwrap();
    let unknown = prepare(
        &store,
        "tree-secret-unknown",
        "C:/private/remediation-unknown",
        "profile-secret-unknown",
        61,
    );
    let raw = Connection::open(profile.database_path()).unwrap();
    raw.execute(
        "UPDATE process_trees SET platform_identity_json='{unknown-secret-json'
         WHERE tree_id=?1",
        params![unknown.tree_id],
    )
    .unwrap();
    drop(raw);
    drop(store);

    let service = compose(&profile);
    let views = service.quarantine_diagnostics(None).unwrap();
    assert_eq!(views.len(), 5);
    assert_eq!(
        remediation_count(&views, QuarantineRemediation::AwaitTerminationProof),
        1
    );
    assert_eq!(
        remediation_count(&views, QuarantineRemediation::RebootRequired),
        1
    );
    assert_eq!(
        remediation_count(&views, QuarantineRemediation::AwaitingProofRetry),
        1
    );
    assert_eq!(
        remediation_count(&views, QuarantineRemediation::ManualRecoveryRequired),
        2
    );
    let reboot = views
        .iter()
        .find(|view| view.remediation == QuarantineRemediation::RebootRequired)
        .unwrap();
    assert!(reboot.legacy_reboot_required);
    assert!(!reboot.current_boot_changed);
    let changed = views
        .iter()
        .find(|view| view.remediation == QuarantineRemediation::AwaitingProofRetry)
        .unwrap();
    assert!(!changed.legacy_reboot_required);
    assert!(changed.current_boot_changed);
    assert!(views
        .iter()
        .filter(|view| view.remediation == QuarantineRemediation::ManualRecoveryRequired)
        .all(|view| view.corrupt));

    let serialized = serde_json::to_string(&views).unwrap();
    let debug = format!("{views:?}");
    for secret in [
        "tree-secret",
        "C:/private",
        "profile-secret",
        "barrier-secret",
        "61000",
        "710000",
        current_boot.as_str(),
        BOOT_A,
        BOOT_B,
        "reason-secret",
        "invalid-start-secret",
        "unknown-secret-json",
        "HANDLE-SECRET",
    ] {
        assert!(
            !serialized.contains(secret),
            "serialized view leaked {secret:?}"
        );
        assert!(!debug.contains(secret), "debug view leaked {secret:?}");
    }
    for view in &views {
        assert!(is_opaque(&view.tree_ref));
        assert!(is_opaque(&view.workspace_ref));
        assert!(is_opaque(&view.profile_ref));
        assert!(is_opaque(&view.owner_fingerprint));
        assert!(is_opaque(&view.reason_digest));
    }
}

struct DaemonGuard(Child);

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_daemon(profile: &RuntimeProfile) -> DaemonGuard {
    let child = Command::new(SERVICE)
        .arg("--profile")
        .arg("development")
        .arg("--data-root")
        .arg(profile.data_root())
        .arg("--ipc-name")
        .arg(profile.ipc_name().unwrap())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    DaemonGuard(child)
}

fn wait_for_owner(profile: &RuntimeProfile) -> r_code_client::DaemonInfo {
    for _ in 0..100 {
        if let Some(info) = r_code_client::read_owner_token(&profile.harness_v1_root()) {
            return info;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("daemon never wrote owner token")
}

async fn connect_ready(profile: &RuntimeProfile, token: &str) -> Result<DaemonClient, ClientError> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match DaemonClient::connect(
            &profile.ipc_endpoint(),
            &profile.profile_id(),
            token,
            "s02-local-client",
        )
        .await
        {
            Err(ClientError::Unreachable(_)) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            result => return result,
        }
    }
}

fn safety_row_counts(path: &Path) -> [i64; 5] {
    let connection = Connection::open(path).unwrap();
    let mut counts = [0; 5];
    for (index, table) in [
        "process_trees",
        "termination_proofs",
        "writer_barriers",
        "tasks",
        "events",
    ]
    .into_iter()
    .enumerate()
    {
        counts[index] = connection
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
    }
    counts
}

#[tokio::test]
async fn local_rpc_is_strict_authenticated_read_only_and_remote_matrix_forbids_it() {
    let temp = tempfile::tempdir().unwrap();
    let ipc_name = format!("s02-quarantine-rpc-{}", std::process::id());
    let profile = profile_for(&ipc_name, temp.path());
    let workspace = "C:/private/rpc-workspace";
    let store = V1Store::open(&profile.database_path()).unwrap();
    prepare(
        &store,
        "tree-secret-rpc",
        workspace,
        "profile-secret-rpc",
        70,
    );
    store
        .save_writer_barrier(
            "barrier-secret-rpc",
            "C:/private/rpc-legacy",
            720_001,
            "820000000001",
            "reason-secret-rpc",
        )
        .unwrap();
    drop(store);

    let _daemon = spawn_daemon(&profile);
    let owner = wait_for_owner(&profile);
    let mut client = connect_ready(&profile, &owner.token).await.unwrap();
    let before = safety_row_counts(&profile.database_path());

    let all = client
        .call("safety.quarantine.get", json!({}))
        .await
        .expect("local diagnostic read");
    assert!(all.as_array().is_some_and(|items| items.len() == 2));
    let filtered = client
        .call("safety.quarantine.get", json!({"workspaceKey": workspace}))
        .await
        .expect("filtered local diagnostic read");
    assert!(filtered.as_array().is_some_and(|items| items.len() == 1));
    for serialized in [all.to_string(), filtered.to_string()] {
        for secret in [
            "tree-secret",
            "C:/private",
            "profile-secret",
            "barrier-secret",
            "720001",
            "820000000001",
            BOOT_A,
            BOOT_B,
            "reason-secret",
            "HANDLE-SECRET",
        ] {
            assert!(!serialized.contains(secret), "RPC leaked {secret:?}");
        }
    }

    for params in [
        Value::Null,
        json!("not-an-object"),
        json!([]),
        json!({"workspaceKey": null}),
        json!({"workspaceKey": ""}),
        json!({"workspaceKey": 42}),
        json!({"unknown": true}),
        json!({"workspaceKey": workspace, "extra": true}),
        json!({"retry": true}),
        json!({"clear": true}),
        json!({"force": true}),
    ] {
        let error = client
            .call("safety.quarantine.get", params)
            .await
            .expect_err("invalid params must be refused");
        assert!(matches!(error, ClientError::Command(_)));
    }
    for method in [
        "safety.quarantine.retry",
        "safety.quarantine.clear",
        "safety.quarantine.force",
    ] {
        let error = client
            .call(method, json!({}))
            .await
            .expect_err("mutating safety method must not exist");
        assert!(matches!(error, ClientError::Command(_)));
    }
    assert_eq!(safety_row_counts(&profile.database_path()), before);

    let forged = match DaemonClient::connect(
        &profile.ipc_endpoint(),
        &profile.profile_id(),
        "forged-token",
        "s02-forged-client",
    )
    .await
    {
        Err(error) => error,
        Ok(_) => panic!("forged local token must fail"),
    };
    assert!(matches!(forged, ClientError::Handshake(_)));

    assert_eq!(required_capability("safety.quarantine.get"), None);
    for capability in [
        Capability::EventsRead,
        Capability::TasksWrite,
        Capability::ApprovalsDecide,
    ] {
        assert_ne!(
            required_capability("safety.quarantine.get"),
            Some(capability)
        );
    }
}
