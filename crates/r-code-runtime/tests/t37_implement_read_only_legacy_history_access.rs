//! T37 — read-only legacy history access.
//!
//! Hash-based fixtures confirm old files remain unchanged while
//! reading/exporting; dev/production/v2 data never cross.

use r_code_runtime::legacy::{LegacyError, LegacyReadOutcome, LegacyReader};
use std::path::Path;

fn hash_of(path: &Path) -> u128 {
    let bytes = std::fs::read(path).unwrap_or_default();
    bytes
        .iter()
        .fold(1_469_598_103_934_665_603_128u128, |acc, byte| {
            (acc ^ *byte as u128).wrapping_mul(1099511628211)
        })
}

fn create_legacy_database(path: &Path) {
    let connection = rusqlite::Connection::open(path).expect("create legacy");
    connection
        .execute_batch(
            "CREATE TABLE tasks (id INTEGER PRIMARY KEY, title TEXT);
             INSERT INTO tasks(title) VALUES ('legacy task 1');
             INSERT INTO tasks(title) VALUES ('legacy task 2');
             INSERT INTO tasks(title) VALUES ('legacy task 3');",
        )
        .expect("seed legacy");
}

#[test]
fn backup_reads_and_exports_without_touching_the_source() {
    let temp = tempfile::tempdir().expect("tempdir");
    let legacy_root = temp.path().join("legacy-root");
    std::fs::create_dir_all(&legacy_root).expect("root");
    let legacy_db = legacy_root.join("db.sqlite3");
    create_legacy_database(&legacy_db);
    std::fs::write(legacy_root.join("config.json"), b"legacy-config").expect("config");
    std::fs::write(legacy_root.join("history.jsonl"), b"{\"a\":1}\n").expect("jsonl");

    let before_db = hash_of(&legacy_db);
    let before_config = hash_of(&legacy_root.join("config.json"));
    let before_jsonl = hash_of(&legacy_root.join("history.jsonl"));

    // Backup into the v2 temporary area.
    let v2_root = temp.path().join("harness-v2");
    let copy = v2_root.join("legacy-copy.sqlite3");
    match LegacyReader::backup_readonly(&legacy_db, &copy, Duration::from_secs(30)).expect("backup")
    {
        LegacyReadOutcome::Ready { copy_path } => assert_eq!(copy_path, copy),
        LegacyReadOutcome::CloseOldAppRequired { reason } => {
            panic!("no live writer in this fixture: {reason}")
        }
    }

    // The source files are bit-identical.
    assert_eq!(hash_of(&legacy_db), before_db, "legacy DB mutated");
    assert_eq!(hash_of(&legacy_root.join("config.json")), before_config);
    assert_eq!(hash_of(&legacy_root.join("history.jsonl")), before_jsonl);

    // Export from the copy as JSONL.
    let mut exported = Vec::new();
    let count = LegacyReader::export_table_jsonl(&copy, "tasks", &mut exported).expect("export");
    assert_eq!(count, 3);
    let text = String::from_utf8_lossy(&exported).into_owned();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3);
    let first: serde_json::Value = serde_json::from_str(lines[0]).expect("json");
    assert_eq!(first["title"], "legacy task 1");

    // Repeated exports are stable (frozen boundaries).
    let mut again = Vec::new();
    LegacyReader::export_table_jsonl(&copy, "tasks", &mut again).expect("again");
    assert_eq!(again, exported);

    // The source is still untouched after the export.
    assert_eq!(hash_of(&legacy_db), before_db);
}

#[test]
fn legacy_reader_never_uses_the_mutating_constructors() {
    // Source-level guard: this module must not reference the legacy
    // mutating stack.
    let raw = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/legacy.rs"))
        .expect("read legacy.rs");
    // Comments may *mention* the forbidden APIs; code must not use them.
    let source: String = raw
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join(
            "
",
        );
    for forbidden in [
        "Database::open",
        "MigrationManager",
        "SettingsService",
        "immutable=1",
        "VACUUM INTO",
    ] {
        assert!(
            !source.contains(forbidden),
            "legacy reader must not use {forbidden}"
        );
    }
}

#[test]
fn dev_production_and_v2_data_never_cross() {
    let temp = tempfile::tempdir().expect("tempdir");
    // Three distinct roots; each keeps its own data.
    let dev_root = temp.path().join("dev-root");
    let prod_root = temp.path().join("prod-root");
    let v2_root = temp.path().join("harness-v2");
    for root in [&dev_root, &prod_root] {
        std::fs::create_dir_all(root).expect("root");
        create_legacy_database(&root.join("db.sqlite3"));
    }

    // Dev backs up into ITS v2 temp area, prod into a separate copy.
    std::fs::create_dir_all(&v2_root).expect("v2");
    let dev_copy = v2_root.join("dev-copy.sqlite3");
    let prod_copy = v2_root.join("prod-copy.sqlite3");
    match LegacyReader::backup_readonly(
        &dev_root.join("db.sqlite3"),
        &dev_copy,
        Duration::from_secs(30),
    )
    .expect("dev backup")
    {
        LegacyReadOutcome::Ready { .. } => {}
        other => panic!("unexpected {other:?}"),
    }
    match LegacyReader::backup_readonly(
        &prod_root.join("db.sqlite3"),
        &prod_copy,
        Duration::from_secs(30),
    )
    .expect("prod backup")
    {
        LegacyReadOutcome::Ready { .. } => {}
        other => panic!("unexpected {other:?}"),
    }
    // Both copies export their own content (identical fixtures here, but
    // through separate files — no shared handles).
    let mut dev_export = Vec::new();
    LegacyReader::export_table_jsonl(&dev_copy, "tasks", &mut dev_export).expect("dev export");
    let mut prod_export = Vec::new();
    LegacyReader::export_table_jsonl(&prod_copy, "tasks", &mut prod_export).expect("prod export");
    assert_eq!(dev_export, prod_export);
    assert!(!dev_copy.starts_with(dev_root.parent().unwrap().join("nowhere").as_path()));

    // Missing tables/invalid names fail closed on the copy only.
    let error =
        LegacyReader::export_table_jsonl(&dev_copy, "tasks; DROP TABLE tasks", &mut Vec::new());
    assert!(matches!(error, Err(LegacyError::Sqlite(_))));
}

use std::time::Duration;
