//! P14 — journal discipline, crash-recovery matrix and physical/ACL CAS
//! restore over REAL Win32 ACL operations on tempdir targets plus a real
//! V1Store journal. The Windows host is the native platform for this
//! suite: every scenario below mutates only directories the test owns and
//! settles each operation (restored or conflict) before the tempdir dies.

#![cfg(windows)]

use r_code_runtime::services::sandbox::windows::{
    apply_planned_grants, build_prepared_operation, canonical_planned_delta, capture_acl,
    classify_prepared_recovery, physical_file_identity, restore_before, AclError, PlannedGrant,
    PreparedRecovery,
};
use r_code_store::v1::safety::{AclOperationRecord, AclOperationState};
use r_code_store::v1::V1Store;

/// S-1-5-18 (LOCAL_SYSTEM) — a well-known SID the test process may grant
/// onto directories it owns.
fn system_sid() -> Vec<u8> {
    vec![
        0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x12, 0x00, 0x00, 0x00,
    ]
}

/// S-1-5-20 (NETWORK SERVICE) — the first foreign actor's SID.
fn network_sid() -> Vec<u8> {
    vec![
        0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x14, 0x00, 0x00, 0x00,
    ]
}

/// S-1-5-19 (LOCAL SERVICE) — the second foreign actor's SID.
fn local_service_sid() -> Vec<u8> {
    vec![
        0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x05, 0x13, 0x00, 0x00, 0x00,
    ]
}

const FULL_ACCESS: u32 = 0x1FFFFF;

fn grants_for(sid: Vec<u8>) -> Vec<PlannedGrant> {
    vec![PlannedGrant::subtree_grant(sid, FULL_ACCESS)]
}

fn temp_target(tag: &str) -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("marker.txt"), format!("s14-{tag}")).expect("write marker");
    let path = dir.path().to_string_lossy().into_owned();
    (dir, path)
}

fn open_store(temp: &tempfile::TempDir) -> V1Store {
    V1Store::open(&temp.path().join("store.db")).expect("store opens")
}

/// The store must refuse every record that is not a genuine prepared
/// shape, with the same one-line reason.
fn assert_refused(store: &V1Store, record: AclOperationRecord, label: &str) {
    assert_eq!(
        store.prepare_acl_operation(record).unwrap_err(),
        "acl operation record is not a valid prepared record",
        "{label}"
    );
}

#[test]
fn journal_refuses_non_prepared_shapes_and_pins_the_state_machine() {
    let db = tempfile::tempdir().expect("db tempdir");
    let store = open_store(&db);
    let (_dir, path) = temp_target("journal");
    let grants = grants_for(system_sid());

    // P14.1: a genuine prepared record is journaled BEFORE any apply.
    let record = build_prepared_operation("acl-journey".to_string(), &path, &grants, 1_000)
        .expect("prepared record builds");
    assert_eq!(record.state, AclOperationState::Prepared);
    assert!(!record.before_descriptor.is_empty());
    assert_eq!(record.planned_delta, canonical_planned_delta(&grants));
    assert_eq!(record.actual_after, None);
    store
        .prepare_acl_operation(record.clone())
        .expect("prepare journals the operation");

    // Non-prepared shapes never enter the journal.
    let mut bad_prefix = record.clone();
    bad_prefix.operation_id = "op-journey".to_string();
    assert_refused(&store, bad_prefix, "operation id without the acl- prefix");

    let mut bad_after = record.clone();
    bad_after.actual_after = Some(vec![0xAA]);
    assert_refused(&store, bad_after, "actual_after preset at prepare time");

    let mut bad_empty_before = record.clone();
    bad_empty_before.before_descriptor = Vec::new();
    assert_refused(&store, bad_empty_before, "empty Before descriptor");

    let mut bad_blank_delta = record.clone();
    bad_blank_delta.planned_delta = "  ".to_string();
    assert_refused(&store, bad_blank_delta, "blank planned delta");

    let mut bad_reason = record.clone();
    bad_reason.conflict_reason = Some("pre-settled".to_string());
    assert_refused(&store, bad_reason, "conflict reason preset at prepare time");

    let mut bad_applied_at = record.clone();
    bad_applied_at.applied_at_ms = Some(1);
    assert_refused(&store, bad_applied_at, "applied timestamp preset");

    let mut bad_settled_at = record.clone();
    bad_settled_at.settled_at_ms = Some(1);
    assert_refused(&store, bad_settled_at, "settled timestamp preset");

    let mut bad_blank_path = record.clone();
    bad_blank_path.target_path = " ".to_string();
    assert_refused(&store, bad_blank_path, "blank target path");

    // Same id with identical content is idempotent; different content is
    // an identity conflict, never a silent overwrite.
    assert_eq!(store.prepare_acl_operation(record.clone()), Ok(()));
    let mut drifted_delta = record.clone();
    drifted_delta.planned_delta = canonical_planned_delta(&grants_for(network_sid()));
    assert_eq!(
        store.prepare_acl_operation(drifted_delta).unwrap_err(),
        "acl operation identity conflict"
    );
    let (_other, other_path) = temp_target("journal-other");
    let mut drifted_target = record.clone();
    drifted_target.target_path = other_path;
    drifted_target.physical_identity =
        physical_file_identity(&drifted_target.target_path).expect("other identity");
    assert_eq!(
        store.prepare_acl_operation(drifted_target).unwrap_err(),
        "acl operation identity conflict"
    );

    // mark_applied only from Prepared; settle Restored only from Applied.
    apply_planned_grants(&path, &grants).expect("apply");
    let readback = capture_acl(&path).expect("readback");
    store
        .mark_acl_applied("acl-journey", readback.self_relative.clone(), 2_000)
        .expect("mark applied");
    assert_eq!(
        store
            .mark_acl_applied("acl-journey", readback.self_relative.clone(), 2_001)
            .unwrap_err(),
        "acl operation is not in the prepared state"
    );
    assert_eq!(
        store
            .mark_acl_applied("acl-unknown", vec![0xBB], 2_002)
            .unwrap_err(),
        "acl operation is not in the prepared state"
    );

    let applied = store
        .load_acl_operation("acl-journey")
        .expect("load applied")
        .expect("applied row exists");
    assert_eq!(applied.state, AclOperationState::Applied);
    assert_eq!(applied.actual_after, Some(readback.self_relative.clone()));
    restore_before(&path, &applied)
        .expect("restore executes")
        .expect("CAS holds");
    store
        .settle_acl_operation("acl-journey", true, None, 2_500)
        .expect("settle restored");
    let restored = store
        .load_acl_operation("acl-journey")
        .expect("load restored")
        .expect("restored row exists");
    assert_eq!(restored.state, AclOperationState::Restored);
    assert_eq!(restored.settled_at_ms, Some(2_500));
    // Restored is terminal: neither settle direction leaves it.
    assert_eq!(
        store
            .settle_acl_operation("acl-journey", true, None, 2_501)
            .unwrap_err(),
        "acl operation is not settleable from its state"
    );
    assert_eq!(
        store
            .settle_acl_operation("acl-journey", false, Some("late"), 2_502)
            .unwrap_err(),
        "acl operation is not settleable from its state"
    );

    // Conflict settles from Prepared (a never-applied operation must never
    // claim a restore) and is terminal.
    let never_applied = build_prepared_operation(
        "acl-never-applied".to_string(),
        &path,
        &grants_for(network_sid()),
        3_000,
    )
    .expect("prepared");
    store
        .prepare_acl_operation(never_applied)
        .expect("prepare never-applied");
    assert_eq!(
        store
            .settle_acl_operation("acl-never-applied", true, None, 3_001)
            .unwrap_err(),
        "acl operation is not settleable from its state"
    );
    store
        .settle_acl_operation("acl-never-applied", false, Some("never-applied"), 3_500)
        .expect("settle conflict from prepared");
    assert_eq!(
        store
            .mark_acl_applied("acl-never-applied", vec![0xCC], 3_501)
            .unwrap_err(),
        "acl operation is not in the prepared state"
    );
    assert_eq!(
        store
            .settle_acl_operation("acl-never-applied", false, Some("again"), 3_502)
            .unwrap_err(),
        "acl operation is not settleable from its state"
    );

    // Conflict also settles from Applied, with the reason preserved.
    let (_dir2, path2) = temp_target("journal-conflict");
    let conflict_grants = grants_for(local_service_sid());
    let prepared = build_prepared_operation(
        "acl-applied-conflict".to_string(),
        &path2,
        &conflict_grants,
        4_000,
    )
    .expect("prepared");
    store
        .prepare_acl_operation(prepared)
        .expect("prepare applied-conflict");
    apply_planned_grants(&path2, &conflict_grants).expect("apply");
    let after = capture_acl(&path2).expect("readback");
    store
        .mark_acl_applied("acl-applied-conflict", after.self_relative.clone(), 4_100)
        .expect("mark applied");
    store
        .settle_acl_operation(
            "acl-applied-conflict",
            false,
            Some("operator-abandoned"),
            4_500,
        )
        .expect("settle conflict from applied");
    let conflicted = store
        .load_acl_operation("acl-applied-conflict")
        .expect("load conflict")
        .expect("conflict row exists");
    assert_eq!(conflicted.state, AclOperationState::Conflict);
    assert_eq!(
        conflicted.conflict_reason.as_deref(),
        Some("operator-abandoned")
    );
    assert_eq!(conflicted.settled_at_ms, Some(4_500));
}

#[test]
fn crash_matrix_recovers_through_real_store_restarts() {
    let db = tempfile::tempdir().expect("db tempdir");
    let mut store = open_store(&db);
    let grants = grants_for(system_sid());

    // Crash between prepare and apply: the target never changed, so the
    // honest settlement is a terminal conflict — the journal never claims
    // a restore that did not happen.
    let (_dir_a, path_a) = temp_target("crash-pre-apply");
    let record =
        build_prepared_operation("acl-crash-before-apply".to_string(), &path_a, &grants, 100)
            .expect("prepared");
    let before = capture_acl(&path_a).expect("before capture");
    assert_eq!(before.self_relative, record.before_descriptor);
    store
        .prepare_acl_operation(record.clone())
        .expect("prepare journals");
    drop(store);
    store = open_store(&db);
    let reloaded = store
        .load_acl_operation("acl-crash-before-apply")
        .expect("load after restart")
        .expect("row survives the restart");
    assert_eq!(reloaded.state, AclOperationState::Prepared);
    assert_eq!(reloaded.before_descriptor, record.before_descriptor);
    let current = capture_acl(&path_a).expect("current capture");
    assert_eq!(current.self_relative, record.before_descriptor);
    assert_eq!(
        classify_prepared_recovery(&before, &grants, &current),
        PreparedRecovery::NothingApplied
    );
    store
        .settle_acl_operation("acl-crash-before-apply", false, Some("never-applied"), 200)
        .expect("settle never-applied conflict");
    let settled = store
        .load_acl_operation("acl-crash-before-apply")
        .expect("load settled")
        .expect("settled row exists");
    assert_eq!(settled.state, AclOperationState::Conflict);
    assert_eq!(settled.conflict_reason.as_deref(), Some("never-applied"));
    assert_eq!(
        capture_acl(&path_a)
            .expect("post-settle capture")
            .self_relative,
        record.before_descriptor
    );

    // Crash between apply and readback: the journal is still Prepared, the
    // delta already landed — classify, mark Applied with the real bytes,
    // then restore Before byte-exactly.
    let (_dir_b, path_b) = temp_target("crash-apply-no-readback");
    let record =
        build_prepared_operation("acl-crash-after-apply".to_string(), &path_b, &grants, 300)
            .expect("prepared");
    let before = capture_acl(&path_b).expect("before capture");
    store
        .prepare_acl_operation(record.clone())
        .expect("prepare journals");
    drop(store);
    apply_planned_grants(&path_b, &grants).expect("apply before the crash");
    store = open_store(&db);
    let reloaded = store
        .load_acl_operation("acl-crash-after-apply")
        .expect("load after restart")
        .expect("row survives the restart");
    assert_eq!(reloaded.state, AclOperationState::Prepared);
    let current = capture_acl(&path_b).expect("current capture");
    assert_eq!(
        classify_prepared_recovery(&before, &grants, &current),
        PreparedRecovery::AlreadyApplied
    );
    store
        .mark_acl_applied("acl-crash-after-apply", current.self_relative.clone(), 400)
        .expect("mark applied with the read-back bytes");
    let applied = store
        .load_acl_operation("acl-crash-after-apply")
        .expect("load applied")
        .expect("applied row exists");
    assert_eq!(applied.state, AclOperationState::Applied);
    assert_eq!(
        applied.actual_after.as_deref(),
        Some(current.self_relative.as_slice())
    );
    assert_eq!(
        capture_acl(&path_b).expect("fresh capture").self_relative,
        applied
            .actual_after
            .clone()
            .expect("journaled actual-after")
    );
    restore_before(&path_b, &applied)
        .expect("restore executes")
        .expect("CAS holds");
    assert_eq!(
        capture_acl(&path_b)
            .expect("restored capture")
            .self_relative,
        applied.before_descriptor
    );
    store
        .settle_acl_operation("acl-crash-after-apply", true, None, 500)
        .expect("settle restored");

    // External edit while only Prepared: classification is Conflict, the
    // foreign ACE is preserved and restore on a never-applied record is a
    // hard refusal that writes nothing.
    let (_dir_c, path_c) = temp_target("crash-external-prepared");
    let planned = grants_for(system_sid());
    let record = build_prepared_operation(
        "acl-external-during-prepare".to_string(),
        &path_c,
        &planned,
        600,
    )
    .expect("prepared");
    let before = capture_acl(&path_c).expect("before capture");
    store
        .prepare_acl_operation(record.clone())
        .expect("prepare journals");
    apply_planned_grants(&path_c, &grants_for(network_sid())).expect("external edit");
    let current = capture_acl(&path_c).expect("current capture");
    assert_eq!(
        classify_prepared_recovery(&before, &planned, &current),
        PreparedRecovery::Conflict("external-change-during-prepare")
    );
    let prepared = store
        .load_acl_operation("acl-external-during-prepare")
        .expect("load prepared")
        .expect("prepared row exists");
    assert_eq!(
        restore_before(&path_c, &prepared),
        Err(AclError::CasMismatch("restore requires an applied record"))
    );
    store
        .settle_acl_operation(
            "acl-external-during-prepare",
            false,
            Some("external-change-during-prepare"),
            700,
        )
        .expect("settle conflict");
    let after_settle = capture_acl(&path_c).expect("post-settle capture");
    assert!(
        after_settle
            .explicit_aces
            .iter()
            .any(|ace| ace.sid == network_sid()),
        "the external ACE survives the conflict settlement"
    );

    // External edit after apply/readback: restore refuses on the ACL CAS
    // and the foreign ACE survives verbatim.
    let (_dir_d, path_d) = temp_target("crash-external-applied");
    let record = build_prepared_operation(
        "acl-external-after-apply".to_string(),
        &path_d,
        &planned,
        800,
    )
    .expect("prepared");
    store
        .prepare_acl_operation(record.clone())
        .expect("prepare journals");
    apply_planned_grants(&path_d, &planned).expect("apply");
    let readback = capture_acl(&path_d).expect("readback");
    store
        .mark_acl_applied(
            "acl-external-after-apply",
            readback.self_relative.clone(),
            900,
        )
        .expect("mark applied");
    let applied = store
        .load_acl_operation("acl-external-after-apply")
        .expect("load applied")
        .expect("applied row exists");
    apply_planned_grants(&path_d, &grants_for(local_service_sid()))
        .expect("external edit after readback");
    assert_eq!(
        restore_before(&path_d, &applied).expect("restore executes"),
        Err("acl-changed-since-apply")
    );
    let current = capture_acl(&path_d).expect("current capture");
    assert!(
        current
            .explicit_aces
            .iter()
            .any(|ace| ace.sid == local_service_sid()),
        "the external ACE survives the refused restore"
    );
    store
        .settle_acl_operation(
            "acl-external-after-apply",
            false,
            Some("acl-changed-since-apply"),
            1_000,
        )
        .expect("settle conflict from applied");
}

#[test]
fn cas_identity_inherited_ordering_and_target_history() {
    let db = tempfile::tempdir().expect("db tempdir");
    let store = open_store(&db);
    let grants = grants_for(system_sid());

    // P14.3 physical CAS: an Applied record refuses to restore onto a
    // different physical object.
    let (_dir_a, path_a) = temp_target("cas-a");
    let (_dir_b, path_b) = temp_target("cas-b");
    assert_ne!(
        physical_file_identity(&path_a).expect("identity a"),
        physical_file_identity(&path_b).expect("identity b")
    );
    let record =
        build_prepared_operation("acl-cas-cross-target".to_string(), &path_a, &grants, 100)
            .expect("prepared");
    store
        .prepare_acl_operation(record.clone())
        .expect("prepare journals");
    apply_planned_grants(&path_a, &grants).expect("apply");
    let readback = capture_acl(&path_a).expect("readback");
    store
        .mark_acl_applied("acl-cas-cross-target", readback.self_relative.clone(), 200)
        .expect("mark applied");
    let applied = store
        .load_acl_operation("acl-cas-cross-target")
        .expect("load applied")
        .expect("applied row exists");
    assert_eq!(
        restore_before(&path_b, &applied).expect("restore executes"),
        Err("physical-identity-changed")
    );

    // On the right target: capture twice around the restore — before it
    // the bytes equal the journaled ActualAfter, after it the Before.
    let pre_restore = capture_acl(&path_a).expect("pre-restore capture");
    assert_eq!(
        pre_restore.self_relative,
        applied
            .actual_after
            .clone()
            .expect("journaled actual-after")
    );
    restore_before(&path_a, &applied)
        .expect("restore executes")
        .expect("CAS holds");
    let post_restore = capture_acl(&path_a).expect("post-restore capture");
    assert_eq!(post_restore.self_relative, applied.before_descriptor);
    store
        .settle_acl_operation("acl-cas-cross-target", true, None, 300)
        .expect("settle restored");

    // Inherited/noncanonical ordering: the child of a parent carrying an
    // explicit inheritable grant starts with an inherited-only DACL. The
    // delta must not fake a conflict and the restore stays byte-exact.
    let parent = tempfile::tempdir().expect("parent tempdir");
    let parent_path = parent.path().to_string_lossy().into_owned();
    apply_planned_grants(&parent_path, &grants_for(system_sid()))
        .expect("plant an inheritable grant on the parent");
    let child_dir = parent.path().join("child");
    std::fs::create_dir(&child_dir).expect("create child");
    let child = child_dir.to_string_lossy().into_owned();
    let before = capture_acl(&child).expect("child before capture");
    assert!(
        before.explicit_aces.is_empty(),
        "a freshly created child carries inherited ACEs only"
    );
    let planned = grants_for(network_sid());
    let record =
        build_prepared_operation("acl-inherited-ordering".to_string(), &child, &planned, 400)
            .expect("prepared");
    store
        .prepare_acl_operation(record.clone())
        .expect("prepare journals");
    apply_planned_grants(&child, &planned).expect("apply");
    let after = capture_acl(&child).expect("child readback");
    assert!(
        after
            .explicit_aces
            .iter()
            .any(|ace| ace.sid == network_sid()),
        "the planned grant landed explicitly"
    );
    assert_eq!(
        classify_prepared_recovery(&before, &planned, &after),
        PreparedRecovery::AlreadyApplied
    );
    store
        .mark_acl_applied("acl-inherited-ordering", after.self_relative.clone(), 500)
        .expect("mark applied");
    let applied = store
        .load_acl_operation("acl-inherited-ordering")
        .expect("load applied")
        .expect("applied row exists");
    restore_before(&child, &applied)
        .expect("restore executes")
        .expect("inherited reordering never breaks the CAS");
    assert_eq!(
        capture_acl(&child)
            .expect("restored child capture")
            .self_relative,
        applied.before_descriptor
    );
    store
        .settle_acl_operation("acl-inherited-ordering", true, None, 600)
        .expect("settle restored");

    // History: oldest first with the pinned terminal states, and the
    // journaled ActualAfter of the live operation equals a fresh capture.
    let (_dir_h, path_h) = temp_target("history");
    let first =
        build_prepared_operation("acl-hist-1".to_string(), &path_h, &grants, 1).expect("prepared");
    store.prepare_acl_operation(first).expect("prepare hist-1");
    store
        .settle_acl_operation("acl-hist-1", false, Some("never-applied"), 2)
        .expect("settle hist-1 conflict");

    let second =
        build_prepared_operation("acl-hist-2".to_string(), &path_h, &grants, 3).expect("prepared");
    store.prepare_acl_operation(second).expect("prepare hist-2");
    apply_planned_grants(&path_h, &grants).expect("apply hist-2");
    let readback = capture_acl(&path_h).expect("readback hist-2");
    store
        .mark_acl_applied("acl-hist-2", readback.self_relative.clone(), 4)
        .expect("mark hist-2 applied");
    let applied_second = store
        .load_acl_operation("acl-hist-2")
        .expect("load hist-2")
        .expect("applied row exists");
    restore_before(&path_h, &applied_second)
        .expect("restore executes")
        .expect("CAS holds");
    store
        .settle_acl_operation("acl-hist-2", true, None, 5)
        .expect("settle hist-2 restored");

    let third = build_prepared_operation(
        "acl-hist-3".to_string(),
        &path_h,
        &grants_for(network_sid()),
        6,
    )
    .expect("prepared");
    store.prepare_acl_operation(third).expect("prepare hist-3");
    apply_planned_grants(&path_h, &grants_for(network_sid())).expect("apply hist-3");
    let readback = capture_acl(&path_h).expect("readback hist-3");
    store
        .mark_acl_applied("acl-hist-3", readback.self_relative.clone(), 7)
        .expect("mark hist-3 applied");
    let applied_third = store
        .load_acl_operation("acl-hist-3")
        .expect("load hist-3")
        .expect("applied row exists");
    assert_eq!(
        capture_acl(&path_h).expect("fresh capture").self_relative,
        applied_third
            .actual_after
            .clone()
            .expect("journaled actual-after")
    );

    let history = store
        .acl_operations_for_target(&path_h)
        .expect("history lists");
    let ids: Vec<&str> = history
        .iter()
        .map(|record| record.operation_id.as_str())
        .collect();
    assert_eq!(ids, vec!["acl-hist-1", "acl-hist-2", "acl-hist-3"]);
    assert_eq!(history[0].state, AclOperationState::Conflict);
    assert_eq!(history[1].state, AclOperationState::Restored);
    assert_eq!(history[2].state, AclOperationState::Applied);

    // Cleanup: restore the still-live operation so the tempdir dies clean.
    restore_before(&path_h, &applied_third)
        .expect("restore executes")
        .expect("CAS holds");
    store
        .settle_acl_operation("acl-hist-3", true, None, 8)
        .expect("settle hist-3 restored");
}
