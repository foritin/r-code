//! P12 — content-addressed SafetyCapabilityReport repository semantics:
//! put/current/history round trip, head discipline, byte-idempotent
//! regeneration, content-address refusal of foreign rows, prune semantics,
//! migration idempotence and the store-boundary tamper stance.

use r_code_store::v1::{SafetyReportRecord, SafetyReportStatus, V1Store};
use rusqlite::{params, Connection};
use std::path::PathBuf;

fn database_path(temp: &tempfile::TempDir) -> PathBuf {
    temp.path().join("store.db")
}

fn hex64(seed: char) -> String {
    std::iter::repeat_n(seed, 64).collect()
}

fn record(report_hex: &str, capability: &str, created_at_ms: i64) -> SafetyReportRecord {
    SafetyReportRecord {
        report_id: format!("safety-{report_hex}"),
        capability: capability.to_string(),
        material_digest: report_hex.to_string(),
        status: SafetyReportStatus::SafeDisabled,
        material_json: format!("{{\"capability\":\"{capability}\"}}"),
        created_at_ms,
    }
}

#[test]
fn put_current_history_round_trip_and_idempotent_regeneration() {
    let temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&database_path(&temp)).unwrap();

    let first = record(&hex64('1'), "write-execution", 1_000);
    let second = record(&hex64('2'), "write-execution", 2_000);
    store.put_safety_report(first.clone()).unwrap();
    store.put_safety_report(second.clone()).unwrap();

    assert_eq!(
        store.safety_report_capabilities().unwrap(),
        vec!["write-execution"]
    );
    // The head points at the newest (last regenerated) report.
    assert_eq!(
        store
            .current_safety_report("write-execution")
            .unwrap()
            .unwrap(),
        second.clone()
    );
    assert!(store
        .current_safety_report("other-capability")
        .unwrap()
        .is_none());

    // Audit history keeps both identities, oldest first.
    let history = store.safety_report_history("write-execution").unwrap();
    assert_eq!(history, vec![first.clone(), second.clone()]);

    // Regenerating identical material (only the wall clock drifted) is
    // byte-idempotent: same report id, the row replaces itself, no duplicate
    // history row, head unchanged.
    let mut regenerated = second.clone();
    regenerated.created_at_ms = 3_000;
    store.put_safety_report(regenerated.clone()).unwrap();
    assert_eq!(
        store
            .current_safety_report("write-execution")
            .unwrap()
            .unwrap(),
        regenerated
    );
    assert_eq!(
        store.safety_report_history("write-execution").unwrap(),
        vec![first, regenerated]
    );
}

#[test]
fn foreign_rows_are_refused_at_the_content_address_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&database_path(&temp)).unwrap();
    let base = record(&hex64('1'), "write-execution", 1_000);

    let mut variants: Vec<(&str, SafetyReportRecord)> = Vec::new();
    for (label, report_id) in [
        ("unprefixed id", format!("plain-{}", hex64('1'))),
        ("short id digest", format!("safety-{}", "1".repeat(63))),
        ("long id digest", format!("safety-{}", "1".repeat(65))),
        ("uppercase id digest", format!("safety-{}", "A".repeat(64))),
        ("non-hex id digest", format!("safety-{}", "g".repeat(64))),
    ] {
        let mut variant = base.clone();
        variant.report_id = report_id;
        variants.push((label, variant));
    }
    for (label, material_digest) in [
        ("short material digest", "1".repeat(63)),
        ("uppercase material digest", "A".repeat(64)),
        ("non-hex material digest", "g".repeat(64)),
    ] {
        let mut variant = base.clone();
        variant.material_digest = material_digest;
        variants.push((label, variant));
    }
    let mut blank_capability = base.clone();
    blank_capability.capability = "  ".to_string();
    variants.push(("blank capability", blank_capability));
    let mut blank_material = base.clone();
    blank_material.material_json = " ".to_string();
    variants.push(("blank material json", blank_material));

    for (label, variant) in &variants {
        assert!(
            store.put_safety_report(variant.clone()).is_err(),
            "{label} was accepted"
        );
        assert!(
            store
                .current_safety_report("write-execution")
                .unwrap()
                .is_none(),
            "{label} persisted anyway"
        );
        assert!(
            store
                .safety_report_history("write-execution")
                .unwrap()
                .is_empty(),
            "{label} left history rows"
        );
    }
    assert!(store.safety_report_capabilities().unwrap().is_empty());
    let error = store.put_safety_report(variants[0].1.clone()).unwrap_err();
    assert!(error.contains("content-addressed"), "{error}");

    // The boundary refuses foreign rows without wedging the store.
    store.put_safety_report(base).unwrap();
    assert_eq!(
        store.safety_report_capabilities().unwrap(),
        vec!["write-execution"]
    );
}

#[test]
fn prune_keeps_the_current_row_removes_stale_and_never_orphans_the_head() {
    let temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&database_path(&temp)).unwrap();

    let first = record(&hex64('1'), "write-execution", 1_000);
    let second = record(&hex64('2'), "write-execution", 2_000);
    store.put_safety_report(first.clone()).unwrap();
    store.put_safety_report(second.clone()).unwrap();

    // Keeping the current report removes the stale identity row.
    assert_eq!(store.prune_safety_reports(&second.report_id).unwrap(), 1);
    assert_eq!(
        store
            .current_safety_report("write-execution")
            .unwrap()
            .unwrap(),
        second
    );
    assert_eq!(
        store.safety_report_history("write-execution").unwrap(),
        vec![second.clone()]
    );

    // Keeping a row that is NOT the head re-points the head to it and still
    // leaves exactly one row for the capability.
    store.put_safety_report(first.clone()).unwrap();
    assert_eq!(store.prune_safety_reports(&second.report_id).unwrap(), 1);
    assert_eq!(
        store
            .current_safety_report("write-execution")
            .unwrap()
            .unwrap(),
        second.clone()
    );
    assert_eq!(
        store.safety_report_history("write-execution").unwrap(),
        vec![second.clone()]
    );

    // Pruning a missing report id errors and never orphans the head.
    let missing = format!("safety-{}", hex64('9'));
    let error = store.prune_safety_reports(&missing).unwrap_err();
    assert!(error.contains("does not exist"), "{error}");
    assert_eq!(
        store
            .current_safety_report("write-execution")
            .unwrap()
            .unwrap(),
        second
    );
    assert_eq!(
        store.safety_report_history("write-execution").unwrap(),
        vec![second]
    );
}

#[test]
fn migration_is_idempotent_and_reports_survive_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let path = database_path(&temp);
    let store = V1Store::open(&path).unwrap();
    let report = record(&hex64('1'), "write-execution", 1_000);
    store.put_safety_report(report.clone()).unwrap();
    drop(store);

    let store = V1Store::open(&path).unwrap();
    let raw = Connection::open(&path).unwrap();
    let ledger: i64 = raw
        .query_row(
            "SELECT COUNT(*) FROM v1_schema_migrations
             WHERE migration_id = 'safety-capability-reports'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(ledger, 1, "migration must not be applied twice");
    let tables: i64 = raw
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table'
             AND name IN ('safety_capability_reports', 'safety_report_heads')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(tables, 2);
    drop(raw);

    assert_eq!(
        store
            .current_safety_report("write-execution")
            .unwrap()
            .unwrap(),
        report
    );
    assert_eq!(
        store.safety_report_capabilities().unwrap(),
        vec!["write-execution"]
    );
}

#[test]
fn tampered_material_json_is_loaded_verbatim_by_the_store() {
    let temp = tempfile::tempdir().unwrap();
    let path = database_path(&temp);
    let store = V1Store::open(&path).unwrap();
    let report = record(&hex64('1'), "write-execution", 1_000);
    store.put_safety_report(report.clone()).unwrap();

    // The store boundary is not the tamper gate: it stores bytes and returns
    // them verbatim. The runtime digest predicate (s12_safety_api) is the
    // gate; here we prove the store does not silently rewrite or drop rows.
    let raw = Connection::open(&path).unwrap();
    raw.execute(
        "UPDATE safety_capability_reports SET material_json = '{\"tampered\":true}'
         WHERE report_id = ?1",
        params![report.report_id],
    )
    .unwrap();
    drop(raw);

    let loaded = store
        .current_safety_report("write-execution")
        .unwrap()
        .unwrap();
    assert_eq!(loaded.material_json, "{\"tampered\":true}");
    assert_eq!(loaded.material_digest, report.material_digest);
    assert_eq!(loaded.report_id, report.report_id);
}
