//! P11R — quarantine proof retry with the complete platform-prover rules:
//! persisted-identity dispatch, the same-boot legacy refusal, the
//! post-reboot RebootProof clear, the replay-only ordinary path and the
//! local-only strict RPC surface.

use r_code_client::{ClientError, DaemonClient};
use r_code_runtime::process_guard::BootIdentity;
use r_code_runtime::remote::capabilities::{required_capability, Capability};
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use r_code_store::v1::operations::{
    NewTerminationProof, PrepareProcessTree, ProcessTreeOwner, ProcessTreeRecord, ProcessTreeState,
    ProcessTreeStoreError, QuarantineRetryOutcome, QuarantineRetryRefusal, TerminationProofKind,
};
use r_code_store::v1::{LeaseRequest, MutationError, V1Store};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const SERVICE: &str = env!("CARGO_BIN_EXE_r-code-service");
const BOOT_A: &str = "windows:01234567-89ab-4cde-8f01-23456789abcd";
const BOOT_B: &str = "windows:01234567-89ab-4cde-8f01-23456789abce";
const ACTOR: &str = "actor-secret-s11r";
const SESSION: &str = "session-secret-s11r";
const RETRIED_AT_MS: i64 = 1_777_777_777_777;

fn database_path(temp: &tempfile::TempDir) -> PathBuf {
    temp.path().join("store.db")
}

fn digest(value: &Value) -> String {
    r_code_harness_protocol::canonical_input_hash(value)
}

fn opaque_text(value: &str) -> String {
    format!("sha256:{}", digest(&Value::String(value.to_string())))
}

fn owner(seed: u32) -> ProcessTreeOwner {
    let platform_identity = json!({
        "privateHandle": format!("HANDLE-SECRET-{seed}"),
        "nativePid": 800_000 + seed,
    });
    ProcessTreeOwner {
        pid: 600_000 + seed,
        start_identity: 7_000_000_000 + u64::from(seed),
        boot_identity: BOOT_A.to_string(),
        platform_identity_digest: digest(&platform_identity),
        platform_identity,
    }
}

fn prepare_tree(
    store: &V1Store,
    tree_id: &str,
    workspace_key: &str,
    seed: u32,
) -> ProcessTreeRecord {
    let input = PrepareProcessTree {
        tree_id: tree_id.to_string(),
        attempt_id: format!("attempt-secret-{tree_id}"),
        workspace_key: workspace_key.to_string(),
        profile_id: "profile-secret-s11r".to_string(),
        owner: owner(seed),
    };
    store.prepare_process_tree(&input).expect("prepare tree")
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
        .expect("legal transition")
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

fn retry(
    store: &V1Store,
    tree_id: &str,
    current_boot: &str,
) -> Result<QuarantineRetryOutcome, ProcessTreeStoreError> {
    store.retry_quarantine(tree_id, current_boot, ACTOR, SESSION, RETRIED_AT_MS)
}

/// A migrated legacy row (P01's migration path): the observed boot the
/// migration stamped is what the retry inequality compares against.
fn legacy_tree(
    store: &V1Store,
    barrier_id: &str,
    workspace_key: &str,
    owner_pid: u32,
    owner_start: &str,
    migrate_boot: &str,
) -> ProcessTreeRecord {
    store
        .save_writer_barrier(
            barrier_id,
            workspace_key,
            owner_pid,
            owner_start,
            "reason-secret",
        )
        .expect("save legacy barrier");
    store
        .migrate_legacy_writer_barriers(migrate_boot)
        .expect("migrate legacy barrier");
    store
        .process_trees_for_workspace(workspace_key)
        .expect("load migrated tree")
        .pop()
        .expect("one migrated tree")
}

/// The crash-recovery shape the ordinary replay branch exists for: the P01
/// machinery persisted a complete Exit proof, then the state flip was lost
/// (a supervisor crash between proving and the CAS commit). Built with the
/// legal machinery — a real Quarantined->Exited transition persists the
/// proof — then the row is restored to its pre-exit quarantined state via
/// the same raw-SQL channel the P01/P02 corruption fixtures use, keeping
/// every identity field, the proof pointer and the proof row intact.
fn quarantined_with_persisted_proof(
    store: &V1Store,
    path: &Path,
    tree_id: &str,
    workspace_key: &str,
    seed: u32,
) -> (ProcessTreeRecord, NewTerminationProof) {
    let running = transition(
        store,
        &prepare_tree(store, tree_id, workspace_key, seed),
        ProcessTreeState::Running,
        None,
        None,
    );
    let quarantined = transition(
        store,
        &transition(store, &running, ProcessTreeState::Terminating, None, None),
        ProcessTreeState::Quarantined,
        Some("death unproved"),
        None,
    );
    let proof = exit_proof(&quarantined, &format!("proof-{tree_id}"));
    transition(
        store,
        &quarantined,
        ProcessTreeState::Exited,
        None,
        Some(&proof),
    );
    let raw = Connection::open(path).expect("open raw connection");
    raw.execute(
        "UPDATE process_trees SET state='quarantined', quarantine_reason='death unproved'
         WHERE tree_id = ?1",
        params![tree_id],
    )
    .expect("restore the crash shape");
    drop(raw);
    (
        store
            .load_process_tree(tree_id)
            .expect("reload")
            .expect("row"),
        proof,
    )
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

fn assert_write_blocked(store: &V1Store, workspace_key: &str, suffix: &str) {
    assert_eq!(
        store.acquire_lease(write_lease(workspace_key, &format!("write-{suffix}"))),
        Err(MutationError::WorkspaceQuarantined {
            workspace_key: workspace_key.to_string(),
        }),
        "write lease escaped quarantine ({suffix})"
    );
}

fn assert_write_grants(store: &V1Store, workspace_key: &str, suffix: &str) {
    store
        .acquire_lease(write_lease(workspace_key, &format!("write-{suffix}")))
        .expect("write lease must grant after a proved clear");
}

fn proof_rows(path: &Path) -> i64 {
    Connection::open(path)
        .expect("open store")
        .query_row("SELECT COUNT(*) FROM termination_proofs", [], |row| {
            row.get(0)
        })
        .expect("count proofs")
}

fn refused(
    outcome: Result<QuarantineRetryOutcome, ProcessTreeStoreError>,
    reason: QuarantineRetryRefusal,
) {
    assert_eq!(outcome, Ok(QuarantineRetryOutcome::Refused { reason }));
}

#[test]
fn store_legacy_same_boot_retry_refuses_without_any_state_or_lease_change() {
    let temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&database_path(&temp)).unwrap();
    let legacy = legacy_tree(
        &store,
        "barrier-secret-same",
        "C:/private/s11r-same",
        640_001,
        "740000000001",
        BOOT_A,
    );
    assert_eq!(legacy.state, ProcessTreeState::LegacyUnverifiable);

    refused(
        retry(&store, &legacy.tree_id, BOOT_A),
        QuarantineRetryRefusal::SameBootLegacy,
    );
    assert_eq!(
        store.load_process_tree(&legacy.tree_id).unwrap().unwrap(),
        legacy,
        "a refusal must not touch the row"
    );
    assert_eq!(
        store
            .load_termination_proof(&reboot_proof_id(&legacy))
            .unwrap(),
        None,
        "a refusal must not mint a proof"
    );
    assert_write_blocked(&store, "C:/private/s11r-same", "same-boot");
    assert_eq!(
        store
            .quarantine_diagnostics(Some("C:/private/s11r-same"))
            .unwrap()
            .len(),
        1
    );

    for (tree, boot, actor, session) in [
        ("", BOOT_B, ACTOR, SESSION),
        (legacy.tree_id.as_str(), "", ACTOR, SESSION),
        (legacy.tree_id.as_str(), "not-a-boot", ACTOR, SESSION),
        (
            legacy.tree_id.as_str(),
            "windows:00000000-0000-0000-0000-000000000000",
            ACTOR,
            SESSION,
        ),
        (legacy.tree_id.as_str(), BOOT_B, "", SESSION),
        (legacy.tree_id.as_str(), BOOT_B, ACTOR, ""),
        (legacy.tree_id.as_str(), BOOT_B, "  ", SESSION),
    ] {
        assert_eq!(
            store.retry_quarantine(tree, boot, actor, session, RETRIED_AT_MS),
            Err(ProcessTreeStoreError::InvalidInput),
            "accepted invalid retry input {boot:?}/{actor:?}"
        );
    }
    assert_eq!(
        retry(&store, "tree-never-existed", BOOT_B),
        Err(ProcessTreeStoreError::NotFound)
    );
    assert_eq!(
        store.load_process_tree(&legacy.tree_id).unwrap().unwrap(),
        legacy
    );
    assert_write_blocked(&store, "C:/private/s11r-same", "same-boot-after-inputs");
}

fn reboot_proof_id(tree: &ProcessTreeRecord) -> String {
    format!("reboot-proof-{}-{}", tree.tree_id, tree.ownership_epoch)
}

#[test]
fn store_legacy_retry_after_changed_boot_clears_with_reboot_proof_audit_once() {
    let temp = tempfile::tempdir().unwrap();
    let path = database_path(&temp);
    let store = V1Store::open(&path).unwrap();
    let legacy = legacy_tree(
        &store,
        "barrier-secret-reboot",
        "C:/private/s11r-reboot",
        640_002,
        "740000000002",
        BOOT_A,
    );

    let proof_id = reboot_proof_id(&legacy);
    assert_eq!(
        retry(&store, &legacy.tree_id, BOOT_B).unwrap(),
        QuarantineRetryOutcome::Cleared {
            proof_id: proof_id.clone(),
        }
    );

    let exited = store.load_process_tree(&legacy.tree_id).unwrap().unwrap();
    assert_eq!(exited.state, ProcessTreeState::Exited);
    assert_eq!(exited.state_revision, legacy.state_revision + 1);
    assert_eq!(
        exited.termination_proof_id.as_deref(),
        Some(proof_id.as_str())
    );

    let proof = store.load_termination_proof(&proof_id).unwrap().unwrap();
    assert_eq!(proof.tree_id, legacy.tree_id);
    assert_eq!(proof.ownership_epoch, legacy.ownership_epoch);
    assert_eq!(proof.kind, TerminationProofKind::Reboot);
    assert_eq!(proof.observed_boot_identity, BOOT_B);
    assert_eq!(proof.proof_identity.as_object().unwrap().len(), 9);
    let evidence = &proof.proof_identity["platformEvidence"];
    assert_eq!(evidence["actor"], json!(ACTOR));
    assert_eq!(evidence["session"], json!(SESSION));
    assert_eq!(evidence["previousBootIdentity"], json!(BOOT_A));
    assert_eq!(evidence["currentBootIdentity"], json!(BOOT_B));
    assert_eq!(evidence["retriedAtMs"], json!(RETRIED_AT_MS));

    assert!(store
        .writer_barriers("C:/private/s11r-reboot")
        .unwrap()
        .is_empty());
    assert_write_grants(&store, "C:/private/s11r-reboot", "after-reboot-proof");
    refused(
        retry(&store, &legacy.tree_id, BOOT_B),
        QuarantineRetryRefusal::NotQuarantined,
    );
    refused(
        retry(&store, &legacy.tree_id, BOOT_A),
        QuarantineRetryRefusal::NotQuarantined,
    );
    assert_eq!(proof_rows(&path), 1, "no second proof may be minted");
    assert_eq!(
        store
            .quarantine_diagnostics(Some("C:/private/s11r-reboot"))
            .unwrap()
            .len(),
        0,
        "a proved exit leaves quarantine diagnostics"
    );
}

#[test]
fn store_legacy_corrupt_row_is_refused_forever_on_every_boot() {
    let temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&database_path(&temp)).unwrap();
    let corrupt = legacy_tree(
        &store,
        "barrier-secret-corrupt",
        "C:/private/s11r-corrupt",
        640_003,
        "not-a-start-identity",
        BOOT_A,
    );
    assert_eq!(
        corrupt.quarantine_reason.as_deref(),
        Some("legacy-corrupt-record")
    );

    for boot in [BOOT_A, BOOT_B, BOOT_B] {
        refused(
            retry(&store, &corrupt.tree_id, boot),
            QuarantineRetryRefusal::LegacyCorrupt,
        );
        assert_eq!(
            store.load_process_tree(&corrupt.tree_id).unwrap().unwrap(),
            corrupt
        );
    }
    assert_eq!(
        store
            .load_termination_proof(&reboot_proof_id(&corrupt))
            .unwrap(),
        None
    );
    assert_write_blocked(&store, "C:/private/s11r-corrupt", "corrupt-forever");
}

#[test]
fn store_legacy_row_without_migrated_boot_is_corrupt_and_never_reboot_proved() {
    // A tampered legacy row with no migrated boot identity: the reboot
    // inequality is vacuous there ("changed since migration" is
    // unverifiable without the previous boot), so the strict contract
    // refuses it as a corrupt record — no RebootProof may be minted.
    let temp = tempfile::tempdir().unwrap();
    let path = database_path(&temp);
    let store = V1Store::open(&path).unwrap();
    let legacy = legacy_tree(
        &store,
        "barrier-secret-no-migrated-boot",
        "C:/private/s11r-no-migrated",
        640_005,
        "740000000005",
        BOOT_A,
    );
    let raw = Connection::open(&path).unwrap();
    raw.execute(
        "UPDATE process_trees SET migrated_observed_boot_identity = NULL WHERE tree_id = ?1",
        params![legacy.tree_id],
    )
    .unwrap();
    drop(raw);
    let tampered = store.load_process_tree(&legacy.tree_id).unwrap().unwrap();
    assert!(tampered.migrated_observed_boot_identity.is_none());

    for boot in [BOOT_A, BOOT_B] {
        refused(
            retry(&store, &tampered.tree_id, boot),
            QuarantineRetryRefusal::LegacyCorrupt,
        );
        assert_eq!(
            store.load_process_tree(&tampered.tree_id).unwrap().unwrap(),
            tampered
        );
    }
    assert_eq!(proof_rows(&path), 0);
    assert_write_blocked(&store, "C:/private/s11r-no-migrated", "no-migrated-boot");
}

#[test]
fn store_ordinary_quarantine_without_proof_stays_blocked_forever() {
    let temp = tempfile::tempdir().unwrap();
    let path = database_path(&temp);
    let store = V1Store::open(&path).unwrap();
    let quarantined = transition(
        &store,
        &transition(
            &store,
            &prepare_tree(&store, "tree-secret-noproof", "C:/private/s11r-noproof", 3),
            ProcessTreeState::Running,
            None,
            None,
        ),
        ProcessTreeState::Quarantined,
        Some("process handle lost"),
        None,
    );

    for boot in [BOOT_A, BOOT_B] {
        refused(
            retry(&store, &quarantined.tree_id, boot),
            QuarantineRetryRefusal::UnverifiableNoProof,
        );
        assert_eq!(
            store
                .load_process_tree(&quarantined.tree_id)
                .unwrap()
                .unwrap(),
            quarantined,
            "a proof-less retry must not touch the row"
        );
    }
    assert!(quarantined.termination_proof_id.is_none());
    assert_eq!(proof_rows(&path), 0);
    assert_write_blocked(&store, "C:/private/s11r-noproof", "no-proof");
    assert_eq!(
        store
            .quarantine_diagnostics(Some("C:/private/s11r-noproof"))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn store_ordinary_quarantine_replays_only_a_complete_persisted_proof() {
    let temp = tempfile::tempdir().unwrap();
    let path = database_path(&temp);
    let store = V1Store::open(&path).unwrap();
    let (crashed, proof) = quarantined_with_persisted_proof(
        &store,
        &path,
        "tree-crash-shape",
        "C:/private/s11r-replay",
        4,
    );
    assert_eq!(crashed.state, ProcessTreeState::Quarantined);
    assert_eq!(
        crashed.termination_proof_id.as_deref(),
        Some(proof.proof_id.as_str())
    );

    assert_eq!(
        retry(&store, &crashed.tree_id, BOOT_B).unwrap(),
        QuarantineRetryOutcome::Cleared {
            proof_id: proof.proof_id.clone(),
        }
    );
    let exited = store.load_process_tree(&crashed.tree_id).unwrap().unwrap();
    assert_eq!(exited.state, ProcessTreeState::Exited);
    assert_eq!(exited.state_revision, crashed.state_revision + 1);
    assert_eq!(proof_rows(&path), 1, "replay must not mint a second proof");
    assert_write_grants(&store, "C:/private/s11r-replay", "after-replay");
    refused(
        retry(&store, &crashed.tree_id, BOOT_B),
        QuarantineRetryRefusal::NotQuarantined,
    );

    for corruption in [
        "missing-row",
        "wrong-epoch",
        "wrong-pointer",
        "corrupt-json",
    ] {
        let temp = tempfile::tempdir().unwrap();
        let path = database_path(&temp);
        let store = V1Store::open(&path).unwrap();
        let (crashed, proof) = quarantined_with_persisted_proof(
            &store,
            &path,
            &format!("tree-replay-{corruption}"),
            &format!("C:/private/s11r-{corruption}"),
            5,
        );
        let raw = Connection::open(&path).unwrap();
        match corruption {
            "missing-row" => {
                raw.execute(
                    "DELETE FROM termination_proofs WHERE proof_id = ?1",
                    params![proof.proof_id],
                )
                .unwrap();
            }
            "wrong-epoch" => {
                raw.execute(
                    "UPDATE termination_proofs SET ownership_epoch = ?1 WHERE proof_id = ?2",
                    params![crashed.ownership_epoch + 1, proof.proof_id],
                )
                .unwrap();
            }
            "wrong-pointer" => {
                raw.execute(
                    "UPDATE process_trees SET termination_proof_id = 'proof-gone'
                     WHERE tree_id = ?1",
                    params![crashed.tree_id],
                )
                .unwrap();
            }
            "corrupt-json" => {
                raw.execute(
                    "UPDATE termination_proofs SET proof_identity_json = '{json-secret-bad'
                     WHERE proof_id = ?1",
                    params![proof.proof_id],
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        drop(raw);

        let outcome = retry(&store, &crashed.tree_id, BOOT_B);
        if corruption == "corrupt-json" {
            // A corrupt proof record fails loudly instead of clearing.
            assert!(matches!(outcome, Err(ProcessTreeStoreError::CorruptRecord)));
        } else {
            refused(outcome, QuarantineRetryRefusal::UnverifiableNoProof);
        }
        assert_eq!(
            store
                .load_process_tree(&crashed.tree_id)
                .unwrap()
                .unwrap()
                .state,
            ProcessTreeState::Quarantined,
            "an incomplete proof must never clear ({corruption})"
        );
        assert_write_blocked(&store, &format!("C:/private/s11r-{corruption}"), corruption);
    }
}

#[test]
fn store_retry_clear_is_atomic_and_stale_fences_never_partially_clear() {
    let temp = tempfile::tempdir().unwrap();
    let path = database_path(&temp);
    let store = V1Store::open(&path).unwrap();
    let legacy = legacy_tree(
        &store,
        "barrier-secret-atomic",
        "C:/private/s11r-atomic",
        640_004,
        "740000000004",
        BOOT_A,
    );
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch(&format!(
        "CREATE TRIGGER fail_retry_exit BEFORE UPDATE OF state ON process_trees
         WHEN OLD.tree_id = '{}' AND NEW.state = 'exited'
         BEGIN SELECT RAISE(ABORT, 'injected retry exit fault'); END;",
        legacy.tree_id.replace('\'', "''")
    ))
    .unwrap();
    drop(raw);

    assert!(matches!(
        retry(&store, &legacy.tree_id, BOOT_B),
        Err(ProcessTreeStoreError::Sqlite(_))
    ));
    assert_eq!(
        store.load_process_tree(&legacy.tree_id).unwrap().unwrap(),
        legacy,
        "a failed CAS must leave the row untouched"
    );
    assert_eq!(
        store
            .load_termination_proof(&reboot_proof_id(&legacy))
            .unwrap(),
        None,
        "a failed CAS must not leak its proof row"
    );
    let barrier_rows: i64 = Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM writer_barriers
             WHERE workspace_key = 'C:/private/s11r-atomic'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(barrier_rows, 1, "a failed CAS must keep the legacy barrier");
    assert_write_blocked(&store, "C:/private/s11r-atomic", "atomic");

    let raw = Connection::open(&path).unwrap();
    raw.execute("DROP TRIGGER fail_retry_exit", []).unwrap();
    drop(raw);
    assert_eq!(
        retry(&store, &legacy.tree_id, BOOT_B).unwrap(),
        QuarantineRetryOutcome::Cleared {
            proof_id: reboot_proof_id(&legacy),
        }
    );
    assert_write_grants(&store, "C:/private/s11r-atomic", "after-fault");

    // A stale fence (a concurrent writer committed after the retry's read)
    // is rejected by the same CAS before any write lands; a fresh retry
    // right after still clears.
    let temp = tempfile::tempdir().unwrap();
    let path = database_path(&temp);
    let store = V1Store::open(&path).unwrap();
    let (crashed, proof) = quarantined_with_persisted_proof(
        &store,
        &path,
        "tree-stale-fence",
        "C:/private/s11r-stale",
        6,
    );
    let mut stale = crashed.fence();
    stale.state_revision += 1;
    assert_eq!(
        store.transition_process_tree(
            &crashed.tree_id,
            &stale,
            ProcessTreeState::Quarantined,
            ProcessTreeState::Exited,
            None,
            Some(&proof),
        ),
        Err(ProcessTreeStoreError::StaleFence)
    );
    assert_eq!(
        store.load_process_tree(&crashed.tree_id).unwrap().unwrap(),
        crashed,
        "a stale-fenced attempt must not partially write"
    );
    assert_eq!(
        retry(&store, &crashed.tree_id, BOOT_B).unwrap(),
        QuarantineRetryOutcome::Cleared {
            proof_id: proof.proof_id.clone(),
        }
    );
    assert_eq!(proof_rows(&path), 1);
}

#[test]
fn source_pins_cas_only_writer_audit_channel_and_local_only_route() {
    let operations = include_str!("../../r-code-store/src/v1/operations.rs");
    let retry_body = operations
        .split_once("pub fn retry_quarantine")
        .and_then(|(_, rest)| rest.split_once("\n    pub fn ").map(|(body, _)| body))
        .expect("retry_quarantine source");
    assert_eq!(
        retry_body.matches("transition_process_tree").count(),
        2,
        "both retry branches must clear through the CAS transition"
    );
    for raw_write in ["UPDATE ", "INSERT INTO", "DELETE FROM"] {
        assert!(
            !retry_body.contains(raw_write),
            "the retry path writes outside the CAS: {raw_write}"
        );
    }
    assert!(retry_body.contains("\"actor\": actor"));
    assert!(retry_body.contains("\"session\": session"));
    assert_eq!(
        operations.matches("UPDATE process_trees").count(),
        1,
        "the fenced CAS in transition_process_tree must stay the only process_trees writer"
    );

    let capabilities = include_str!("../src/remote/capabilities.rs");
    assert!(
        !capabilities.contains("safety.quarantine.retry"),
        "the retry RPC must stay outside the remote capability matrix"
    );
    assert_eq!(required_capability("safety.quarantine.retry"), None);
    for capability in [
        Capability::EventsRead,
        Capability::TasksWrite,
        Capability::ApprovalsDecide,
    ] {
        assert_ne!(
            required_capability("safety.quarantine.retry"),
            Some(capability)
        );
    }

    let service = include_str!("../src/bin/r-code-service.rs");
    assert!(service.contains("\"safety.quarantine.retry\""));
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

async fn connect_ready(profile: &RuntimeProfile, token: &str) -> DaemonClient {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match DaemonClient::connect(
            &profile.ipc_endpoint(),
            &profile.profile_id(),
            token,
            "s11r-local-client",
        )
        .await
        {
            Err(ClientError::Unreachable(_)) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            result => return result.expect("daemon client connects"),
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
async fn local_rpc_retry_is_strict_redacted_e2e_and_local_only() {
    let temp = tempfile::tempdir().unwrap();
    let ipc_name = format!("s11r-quarantine-retry-{}", std::process::id());
    let profile = RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(temp.path().join("root"))
            .with_ipc_name(ipc_name),
    )
    .unwrap();
    let current_boot = BootIdentity::current().unwrap();
    let different_boot = if current_boot.as_str() == BOOT_A {
        BOOT_B
    } else {
        BOOT_A
    };
    let store = V1Store::open(&profile.database_path()).unwrap();
    store
        .save_writer_barrier(
            "barrier-secret-same",
            "C:/private/s11r-rpc-same",
            910_001,
            "910000000001",
            "reason-secret-same",
        )
        .unwrap();
    store
        .migrate_legacy_writer_barriers(current_boot.as_str())
        .unwrap();
    store
        .save_writer_barrier(
            "barrier-secret-reboot",
            "C:/private/s11r-rpc-reboot",
            910_002,
            "910000000002",
            "reason-secret-reboot",
        )
        .unwrap();
    store
        .migrate_legacy_writer_barriers(different_boot)
        .unwrap();
    drop(store);
    let tree_same = format!(
        "legacy:{}",
        digest(&Value::String("barrier-secret-same".into()))
    );
    let tree_reboot = format!(
        "legacy:{}",
        digest(&Value::String("barrier-secret-reboot".into()))
    );

    let _daemon = spawn_daemon(&profile);
    let owner = wait_for_owner(&profile);
    let mut client = connect_ready(&profile, &owner.token).await;
    let before = safety_row_counts(&profile.database_path());

    let refusal = client
        .call(
            "safety.quarantine.retry",
            json!({"treeId": tree_same, "actor": ACTOR, "session": SESSION}),
        )
        .await
        .expect("same-boot refusal is a structured outcome, not an error");
    let object = refusal.as_object().expect("refusal object");
    assert_eq!(object.len(), 3, "a refusal carries no proofRef: {refusal}");
    assert_eq!(object["treeRef"], json!(opaque_text(&tree_same)));
    assert_eq!(object["cleared"], json!(false));
    assert_eq!(object["reason"], json!("same-boot-legacy"));
    assert_eq!(safety_row_counts(&profile.database_path()), before);

    for params in [
        Value::Null,
        json!("not-an-object"),
        json!([]),
        json!({}),
        json!({"treeId": tree_reboot}),
        json!({"actor": ACTOR, "session": SESSION}),
        json!({"treeId": "", "actor": ACTOR, "session": SESSION}),
        json!({"treeId": "   ", "actor": ACTOR, "session": SESSION}),
        json!({"treeId": tree_reboot, "actor": "", "session": SESSION}),
        json!({"treeId": tree_reboot, "actor": ACTOR, "session": ""}),
        json!({"treeId": 42, "actor": ACTOR, "session": SESSION}),
        json!({"treeId": tree_reboot, "actor": ACTOR, "session": SESSION, "extra": true}),
        json!({"force": true}),
    ] {
        let error = client
            .call("safety.quarantine.retry", params.clone())
            .await
            .expect_err("invalid params must be refused");
        assert!(matches!(error, ClientError::Command(_)), "{params:?}");
    }
    assert_eq!(safety_row_counts(&profile.database_path()), before);

    let diagnostics_before = client
        .call("safety.quarantine.get", json!({}))
        .await
        .expect("diagnostics before the clear");
    assert_eq!(diagnostics_before.as_array().unwrap().len(), 2);

    let cleared = client
        .call(
            "safety.quarantine.retry",
            json!({"treeId": tree_reboot, "actor": ACTOR, "session": SESSION}),
        )
        .await
        .expect("post-reboot retry clears");
    let object = cleared.as_object().expect("cleared object");
    assert_eq!(object.len(), 4, "{cleared}");
    assert_eq!(object["treeRef"], json!(opaque_text(&tree_reboot)));
    assert_eq!(object["cleared"], json!(true));
    assert_eq!(object["reason"], json!("cleared"));
    let persisted_proof = Connection::open(profile.database_path())
        .unwrap()
        .query_row(
            "SELECT termination_proof_id FROM process_trees WHERE tree_id = ?1",
            params![tree_reboot],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    assert_eq!(object["proofRef"], json!(opaque_text(&persisted_proof)));

    for serialized in [refusal.to_string(), cleared.to_string()] {
        for secret in [
            tree_same.as_str(),
            tree_reboot.as_str(),
            persisted_proof.as_str(),
            "reboot-proof",
            ACTOR,
            SESSION,
            "barrier-secret",
            "C:/private",
            "reason-secret",
            current_boot.as_str(),
            BOOT_A,
            BOOT_B,
        ] {
            assert!(!serialized.contains(secret), "retry RPC leaked {secret:?}");
        }
    }

    let diagnostics_after = client
        .call("safety.quarantine.get", json!({}))
        .await
        .expect("diagnostics after the clear");
    let serialized = diagnostics_after.to_string();
    assert_eq!(diagnostics_after.as_array().unwrap().len(), 1);
    for secret in [
        tree_reboot.as_str(),
        "barrier-secret-reboot",
        "C:/private",
        current_boot.as_str(),
    ] {
        assert!(
            !serialized.contains(secret),
            "diagnostics leaked {secret:?}"
        );
    }

    let after = safety_row_counts(&profile.database_path());
    assert_eq!(after[0], before[0], "process_trees count");
    assert_eq!(after[1], before[1] + 1, "exactly one RebootProof row");
    assert_eq!(after[2], before[2] - 1, "the cleared legacy barrier row");
    assert_eq!(after[3], before[3], "tasks untouched");
    assert_eq!(after[4], before[4], "events untouched");

    let refusal_again = client
        .call(
            "safety.quarantine.retry",
            json!({"treeId": tree_reboot, "actor": ACTOR, "session": SESSION}),
        )
        .await
        .expect("no double clear");
    assert_eq!(refusal_again["cleared"], json!(false));
    assert_eq!(refusal_again["reason"], json!("not-quarantined"));
    assert_eq!(safety_row_counts(&profile.database_path()), after);

    let proof_row = Connection::open(profile.database_path())
        .unwrap()
        .query_row(
            "SELECT proof_kind, observed_boot_identity, proof_identity_json
             FROM termination_proofs WHERE proof_id = ?1",
            params![persisted_proof],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(proof_row.0, "reboot");
    assert_eq!(proof_row.1, current_boot.as_str());
    let identity: Value = serde_json::from_str(&proof_row.2).unwrap();
    assert_eq!(identity["platformEvidence"]["actor"], json!(ACTOR));
    assert_eq!(identity["platformEvidence"]["session"], json!(SESSION));

    let store = V1Store::open(&profile.database_path()).unwrap();
    assert_write_grants(&store, "C:/private/s11r-rpc-reboot", "after-rpc-clear");
}
