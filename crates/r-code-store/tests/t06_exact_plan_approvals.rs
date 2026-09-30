//! T06 — durable exact-plan approval, CAS and snapshot gate contract.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

use r_code_harness_protocol::{
    services::{NetworkCeiling, WorkUnitEffectClass, WorkUnitWire},
    HarnessId, PackageRef,
};
use r_code_kernel::{
    PermissionSnapshotRef, PlanApproval, PlanApprovalActor, PlanApprovalRef, PlanApprovalState,
    PlanRevision, PlanRevisionMaterial, PlanRevisionRef, PromptSnapshotMode, PromptSnapshotRef,
    ProviderRouteKind, ProviderSnapshotRef, RunSnapshot, RunSnapshotMaterial, RunSnapshotPhase,
    WorkspaceSnapshotRef, PLAN_APPROVE_SCOPE,
};
use r_code_store::v1::plans::PlanStoreError;
use r_code_store::v1::{V1Store, V1StoreError};
use rusqlite::{params, Connection};

fn database_path(root: &Path) -> PathBuf {
    root.join("harness-v1").join("tasks.sqlite3")
}

fn plan(
    task_id: &str,
    revision: u64,
    parent_revision: Option<PlanRevisionRef>,
    marker: &str,
) -> PlanRevision {
    PlanRevision::new(PlanRevisionMaterial {
        task_id: task_id.into(),
        revision,
        parent_revision,
        current_base_hash: format!("sha256:base-{marker}"),
        workspace_baseline: "sha256:workspace".into(),
        route_digest: "sha256:route".into(),
        prompt_digest: "sha256:prompt".into(),
        permission_digest: "sha256:permission".into(),
        check_digest: "sha256:checks".into(),
        required_checks: vec!["check:test".into()],
        work_units: vec![WorkUnitWire {
            id: "implement".into(),
            description: format!("implement {marker}"),
            dependencies: Vec::new(),
            acceptance: vec!["check:test".into()],
            read_paths: vec![],
            write_paths: vec![],
            repo_exclusive: false,
            ephemeral_roots: vec![],
            effect_class: WorkUnitEffectClass::ReadOnly,
            network_ceiling: NetworkCeiling::Offline,
        }],
    })
    .expect("valid plan")
}

fn actor(actor_id: &str, session_id: &str) -> PlanApprovalActor {
    PlanApprovalActor::new(actor_id, session_id, PLAN_APPROVE_SCOPE).expect("valid actor")
}

fn approval_ref(approval: &PlanApproval) -> PlanApprovalRef {
    PlanApprovalRef {
        approval_id: approval.approval_id.clone(),
        plan_revision: approval.plan_revision.clone(),
    }
}

fn snapshot(task_id: &str, phase: RunSnapshotPhase, marker: &str) -> RunSnapshot {
    let work_unit_id = (!matches!(&phase, RunSnapshotPhase::Planning)).then(|| "implement".into());
    RunSnapshot::new(RunSnapshotMaterial {
        task_id: task_id.into(),
        task_revision: 7,
        phase,
        work_unit_id,
        provider: ProviderSnapshotRef {
            kind: ProviderRouteKind::HostProvider,
            settings_revision: 4,
            provider_id: "deepseek".into(),
            model_id: "deepseek-chat".into(),
            base_url: Some("https://api.deepseek.com".into()),
            protocol: Some("openai-compatible".into()),
            capabilities: vec!["streaming".into(), "tools".into()],
        },
        prompt: PromptSnapshotRef {
            revision: "prompt-1".into(),
            mode: PromptSnapshotMode::Default,
            content_sha256: "sha256:prompt".into(),
            resolved_system_prompt: "system prompt".into(),
        },
        workspace: WorkspaceSnapshotRef {
            canonical_root: "D:/project/r-code".into(),
            workspace_identity: "repo:one".into(),
            baseline_sha256: "sha256:workspace".into(),
        },
        permissions: PermissionSnapshotRef {
            revision: "permission-1".into(),
            profile_id: "read-only".into(),
            capabilities: vec!["host.fs.read".into()],
        },
        harness_package: PackageRef {
            id: HarnessId::new("native"),
            version: "1.0.0".parse().expect("version"),
            content_digest: "sha256:package".into(),
        },
        tool_catalog_sha256: format!("sha256:tools-{marker}"),
        inference: Some(serde_json::json!({"temperature": 0})),
    })
    .expect("valid snapshot")
}

fn row_count(path: &Path, table: &str) -> i64 {
    let connection = Connection::open(path).expect("inspect database");
    connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .expect("row count")
}

#[test]
fn typed_publication_requires_revision_one_then_exact_parent_lineage() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = V1Store::open(&database_path(temp.path())).expect("open");

    let invalid_first = plan("task-lineage", 2, None, "invalid-first");
    assert!(matches!(
        store.publish_plan_revision(&invalid_first, None),
        Err(PlanStoreError::NonMonotonicRevision {
            current: None,
            provided: 2
        })
    ));
    assert_eq!(store.current_plan_head("task-lineage").unwrap(), None);

    let first = plan("task-lineage", 1, None, "first");
    let first_ref = store
        .publish_plan_revision(&first, None)
        .expect("first revision");

    let skipped = plan("task-lineage", 3, Some(first_ref.clone()), "skipped");
    assert!(matches!(
        store.publish_plan_revision(&skipped, Some(&first_ref)),
        Err(PlanStoreError::NonMonotonicRevision {
            current: Some(1),
            provided: 3
        })
    ));

    let wrong_parent = plan(
        "task-lineage",
        2,
        Some(PlanRevisionRef::parse(format!("sha256:{}", "f".repeat(64))).unwrap()),
        "wrong-parent",
    );
    assert!(matches!(
        store.publish_plan_revision(&wrong_parent, Some(&first_ref)),
        Err(PlanStoreError::ParentMismatch)
    ));
}

#[test]
fn identical_revision_retry_is_idempotent_and_does_not_revoke_its_approval() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = database_path(temp.path());
    let store = V1Store::open(&path).expect("open");
    let first = plan("task-retry", 1, None, "first");
    let revision = store
        .publish_plan_revision(&first, None)
        .expect("first publish");
    let approval = store
        .approve_plan_revision(
            "task-retry",
            &revision,
            "approval-retry",
            actor("actor-1", "session-1"),
        )
        .expect("approve");

    assert_eq!(
        store
            .publish_plan_revision(&first, None)
            .expect("same content-addressed operation replays"),
        revision
    );
    assert_eq!(row_count(&path, "plan_revisions"), 1);
    assert_eq!(
        store
            .load_active_plan_approval("task-retry")
            .expect("load")
            .expect("active"),
        approval,
        "an idempotent retry must not supersede the approval for that same revision"
    );
}

#[test]
fn stale_publish_is_rejected_and_two_concurrent_next_revisions_have_one_winner() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = database_path(temp.path());
    let first_store = V1Store::open(&path).expect("open");
    let first = plan("task-race", 1, None, "first");
    let first_ref = first_store
        .publish_plan_revision(&first, None)
        .expect("first");
    drop(first_store);

    let left = plan("task-race", 2, Some(first_ref.clone()), "left");
    let right = plan("task-race", 2, Some(first_ref.clone()), "right");
    let barrier = Arc::new(Barrier::new(3));
    let mut handles = Vec::new();
    for candidate in [left, right] {
        let path = path.clone();
        let expected = first_ref.clone();
        let barrier = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            let store = V1Store::open(&path).expect("open contender");
            barrier.wait();
            store.publish_plan_revision(&candidate, Some(&expected))
        }));
    }
    barrier.wait();

    let results = handles
        .into_iter()
        .map(|handle| handle.join().expect("contender thread"))
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| matches!(result, Err(PlanStoreError::StaleHead { .. })))
            .count(),
        1
    );
    assert_eq!(row_count(&path, "plan_revisions"), 2);

    let store = V1Store::open(&path).expect("reopen");
    assert!(matches!(
        store.publish_plan_revision(
            &plan("task-race", 2, Some(first_ref.clone()), "late"),
            Some(&first_ref)
        ),
        Err(PlanStoreError::StaleHead { .. })
    ));
}

#[test]
fn approval_replay_conflicts_and_exact_task_revision_checks_are_explicit() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = V1Store::open(&database_path(temp.path())).expect("open");
    let plan_a = plan("task-a", 1, None, "a");
    let plan_b = plan("task-b", 1, None, "b");
    let revision_a = store.publish_plan_revision(&plan_a, None).expect("plan a");
    let revision_b = store.publish_plan_revision(&plan_b, None).expect("plan b");

    let approval = store
        .approve_plan_revision(
            "task-a",
            &revision_a,
            "approval-a",
            actor("actor-a", "session-a"),
        )
        .expect("first approval");
    assert_eq!(
        store
            .approve_plan_revision(
                "task-a",
                &revision_a,
                "approval-a",
                actor("actor-a", "session-a"),
            )
            .expect("identical replay"),
        approval
    );
    assert!(matches!(
        store.approve_plan_revision(
            "task-a",
            &revision_a,
            "approval-a",
            actor("actor-a", "different-session"),
        ),
        Err(PlanStoreError::ApprovalIdConflict)
    ));
    assert!(matches!(
        store.approve_plan_revision(
            "task-a",
            &revision_a,
            "approval-second",
            actor("actor-b", "session-b"),
        ),
        Err(PlanStoreError::ActiveApprovalExists)
    ));
    assert!(matches!(
        store.approve_plan_revision(
            "task-a",
            &revision_b,
            "approval-wrong-hash",
            actor("actor-a", "session-a"),
        ),
        Err(PlanStoreError::ApprovalNotCurrent)
    ));
    assert!(matches!(
        store.approve_plan_revision(
            "task-b",
            &revision_a,
            "approval-wrong-task",
            actor("actor-b", "session-b"),
        ),
        Err(PlanStoreError::ApprovalNotCurrent)
    ));

    assert!(matches!(
        store.validate_active_plan_approval("task-b", &approval_ref(&approval)),
        Err(PlanStoreError::ApprovalTaskMismatch)
    ));
    let wrong_revision_ref = PlanApprovalRef {
        approval_id: approval.approval_id.clone(),
        plan_revision: revision_b,
    };
    assert!(matches!(
        store.validate_active_plan_approval("task-a", &wrong_revision_ref),
        Err(PlanStoreError::ApprovalRevisionMismatch)
    ));
}

#[test]
fn material_change_gets_a_new_hash_and_atomically_supersedes_active_approval() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = V1Store::open(&database_path(temp.path())).expect("open");
    let first = plan("task-change", 1, None, "first");
    let first_ref = store
        .publish_plan_revision(&first, None)
        .expect("first plan");
    let approval = store
        .approve_plan_revision(
            "task-change",
            &first_ref,
            "approval-first",
            actor("actor-1", "session-1"),
        )
        .expect("approve first");

    let next = plan("task-change", 2, Some(first_ref.clone()), "changed");
    assert_ne!(next.reference(), &first_ref);
    let next_ref = store
        .publish_plan_revision(&next, Some(&first_ref))
        .expect("publish changed plan");
    assert_eq!(
        store.current_plan_head("task-change").unwrap(),
        Some(next_ref)
    );
    assert!(store
        .load_active_plan_approval("task-change")
        .expect("load active")
        .is_none());
    assert_eq!(
        store
            .load_plan_approval(&approval.approval_id)
            .expect("load old")
            .expect("old approval")
            .state,
        PlanApprovalState::Superseded
    );
    assert!(matches!(
        store.validate_active_plan_approval("task-change", &approval_ref(&approval)),
        Err(PlanStoreError::ApprovalSuperseded)
    ));
}

#[test]
fn plan_head_and_approval_survive_a_store_restart() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = database_path(temp.path());
    let expected_plan;
    let expected_approval;
    {
        let store = V1Store::open(&path).expect("open");
        expected_plan = plan("task-restart", 1, None, "restart");
        let revision = store
            .publish_plan_revision(&expected_plan, None)
            .expect("publish");
        expected_approval = store
            .approve_plan_revision(
                "task-restart",
                &revision,
                "approval-restart",
                actor("actor-restart", "session-restart"),
            )
            .expect("approve");
    }

    let reopened = V1Store::open(&path).expect("reopen");
    assert_eq!(
        reopened
            .current_plan_revision("task-restart")
            .expect("load plan"),
        Some(expected_plan)
    );
    assert_eq!(
        reopened
            .load_active_plan_approval("task-restart")
            .expect("load approval"),
        Some(expected_approval)
    );
}

#[test]
fn execution_and_repair_snapshot_inserts_require_current_exact_approval() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = database_path(temp.path());
    let store = V1Store::open(&path).expect("open");
    store
        .save_run_snapshot(&snapshot(
            "task-snapshot",
            RunSnapshotPhase::Planning,
            "planning",
        ))
        .expect("planning needs no approval");

    let first = plan("task-snapshot", 1, None, "first");
    let first_ref = store
        .publish_plan_revision(&first, None)
        .expect("publish first");
    let missing = PlanApprovalRef {
        approval_id: "missing-approval".into(),
        plan_revision: first_ref.clone(),
    };
    let before_invalid = row_count(&path, "run_snapshots");
    assert!(store
        .save_run_snapshot(&snapshot(
            "task-snapshot",
            RunSnapshotPhase::Execution { approval: missing },
            "missing",
        ))
        .is_err());
    assert_eq!(row_count(&path, "run_snapshots"), before_invalid);

    let approval = store
        .approve_plan_revision(
            "task-snapshot",
            &first_ref,
            "approval-snapshot",
            actor("actor-1", "session-1"),
        )
        .expect("approve");
    store
        .save_run_snapshot(&snapshot(
            "task-snapshot",
            RunSnapshotPhase::Execution {
                approval: approval_ref(&approval),
            },
            "execution",
        ))
        .expect("exact execution approval");
    store
        .save_run_snapshot(&snapshot(
            "task-snapshot",
            RunSnapshotPhase::Repair {
                approval: approval_ref(&approval),
            },
            "repair",
        ))
        .expect("exact repair approval");

    let next = plan("task-snapshot", 2, Some(first_ref.clone()), "next");
    store
        .publish_plan_revision(&next, Some(&first_ref))
        .expect("publish next");
    let before_stale = row_count(&path, "run_snapshots");
    assert!(store
        .save_run_snapshot(&snapshot(
            "task-snapshot",
            RunSnapshotPhase::Execution {
                approval: approval_ref(&approval),
            },
            "stale-after-publish",
        ))
        .is_err());
    assert_eq!(
        row_count(&path, "run_snapshots"),
        before_stale,
        "approval validation and insert must be one atomic transaction"
    );
}

#[test]
fn schema_enforces_revision_foreign_keys_uniqueness_and_one_active_approval() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = database_path(temp.path());
    let store = V1Store::open(&path).expect("open");
    let first = plan("task-schema", 1, None, "first");
    let revision = store.publish_plan_revision(&first, None).expect("publish");
    store
        .approve_plan_revision(
            "task-schema",
            &revision,
            "approval-schema",
            actor("actor-1", "session-1"),
        )
        .expect("approve");

    let connection = Connection::open(&path).expect("inspect");
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .expect("foreign keys");
    assert!(connection
        .execute(
            "INSERT INTO plan_approvals(
                approval_id, task_id, plan_revision, actor_id, session_id,
                scope, state, approval_json, created_at_ms, superseded_at_ms)
             VALUES (?1, ?2, ?3, 'actor-2', 'session-2', 'plan.approve',
                     'active', '{}', 1, NULL)",
            params!["approval-second", "task-schema", revision.as_str()],
        )
        .is_err());
    assert!(connection
        .execute(
            "INSERT INTO plan_approvals(
                approval_id, task_id, plan_revision, actor_id, session_id,
                scope, state, approval_json, created_at_ms, superseded_at_ms)
             VALUES ('approval-missing-plan', 'task-schema', ?1, 'actor', 'session',
                     'plan.approve', 'superseded', '{}', 1, 1)",
            params![format!("sha256:{}", "a".repeat(64))],
        )
        .is_err());
    assert!(connection
        .execute(
            "INSERT INTO plan_revisions(
                plan_revision, task_id, revision_number, parent_revision,
                current_base_hash, content_sha256, material_json,
                payload_json, created_at_ms)
             VALUES (?1, 'task-schema', 1, NULL, 'base', 'digest', '{}', '{}', 1)",
            params![format!("sha256:{}", "b".repeat(64))],
        )
        .is_err());

    let cross_task = plan(
        "task-schema-other",
        2,
        Some(revision.clone()),
        "cross-task-parent",
    );
    let cross_task_json = cross_task
        .canonical_json()
        .expect("canonical cross-task plan");
    assert!(
        connection
            .execute(
                "INSERT INTO plan_revisions(
                    plan_revision, task_id, revision_number, parent_revision,
                    current_base_hash, content_sha256, material_json,
                    payload_json, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7, 1)",
                params![
                    cross_task.reference().as_str(),
                    cross_task.material().task_id,
                    cross_task.material().revision,
                    revision.as_str(),
                    cross_task.material().current_base_hash,
                    cross_task
                        .reference()
                        .as_str()
                        .trim_start_matches("sha256:"),
                    cross_task_json,
                ],
            )
            .is_err(),
        "parent revision FK must preserve task ownership, not just global hash existence"
    );
}

#[test]
fn existing_v1_database_upgrades_without_losing_legacy_rows() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = database_path(temp.path());
    std::fs::create_dir_all(path.parent().unwrap()).expect("database parent");
    let connection = Connection::open(&path).expect("create old database");
    connection
        .execute_batch(
            "CREATE TABLE v2_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO v2_meta(key, value) VALUES ('schema_version', '1');
             CREATE TABLE reviews (
                 task_id TEXT PRIMARY KEY,
                 disposition TEXT NOT NULL,
                 notes TEXT,
                 updated_at_ms INTEGER NOT NULL
             );
             INSERT INTO reviews(task_id, disposition, notes, updated_at_ms)
             VALUES ('ordinary-review', 'pending', 'keep-me', 1);",
        )
        .expect("old schema");
    drop(connection);

    let store = V1Store::open(&path).expect("additive upgrade");
    let connection = Connection::open(&path).expect("inspect upgraded database");
    let note: String = connection
        .query_row(
            "SELECT notes FROM reviews WHERE task_id = 'ordinary-review'",
            [],
            |row| row.get(0),
        )
        .expect("legacy row preserved");
    assert_eq!(note, "keep-me");
    for table in ["plan_revisions", "task_plan_heads", "plan_approvals"] {
        let present: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                params![table],
                |row| row.get(0),
            )
            .expect("table lookup");
        assert_eq!(present, 1, "missing additive table {table}");
    }
    drop(store);
    drop(connection);

    let reopened = V1Store::open(&path).expect("idempotent second upgrade");
    drop(reopened);
    let connection = Connection::open(&path).expect("inspect migration ledger");
    let migration_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM v1_schema_migrations
             WHERE migration_id = 'plan-revisions-and-approvals'",
            [],
            |row| row.get(0),
        )
        .expect("migration count");
    assert_eq!(migration_count, 1);
}

#[test]
fn legacy_plan_adapter_preserves_payload_and_never_creates_review_or_approval() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = database_path(temp.path());
    let store = V1Store::open(&path).expect("open");
    let payloads = [
        (
            "legacy-json",
            35,
            " {\n  \"units\": [2, 1], \"ok\": true\n} ",
        ),
        ("legacy-text", 18, "opaque\0unicode-你好\u{1}"),
    ];
    for (task_id, revision, payload) in payloads {
        store
            .save_plan(task_id, revision, payload)
            .expect("save legacy payload");
        assert_eq!(
            store.load_plan(task_id).expect("load legacy payload"),
            Some((revision, payload.to_string()))
        );
        store
            .save_plan(task_id, revision, payload)
            .expect("identical legacy save is idempotent");
    }

    let connection = Connection::open(&path).expect("inspect");
    let review_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM reviews WHERE task_id LIKE 'plan:%'",
            [],
            |row| row.get(0),
        )
        .expect("review count");
    let approval_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM plan_approvals", [], |row| row.get(0))
        .expect("approval count");
    assert_eq!(review_count, 0);
    assert_eq!(approval_count, 0);
}

#[test]
fn lazy_review_backed_plan_migration_deletes_source_row_and_is_idempotent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = database_path(temp.path());
    let store = V1Store::open(&path).expect("open");
    let payload = " {\"legacy\":true,\"order\":[3,2,1]} ";
    let connection = Connection::open(&path).expect("seed legacy row");
    connection
        .execute(
            "INSERT INTO reviews(task_id, disposition, notes, updated_at_ms)
             VALUES ('plan:legacy-lazy', 'revision:35', ?1, 1)",
            params![payload],
        )
        .expect("legacy plan row");
    drop(connection);

    assert_eq!(
        store.load_plan("legacy-lazy").expect("first lazy load"),
        Some((35, payload.to_string()))
    );
    assert_eq!(
        store.load_plan("legacy-lazy").expect("idempotent reload"),
        Some((35, payload.to_string()))
    );
    let connection = Connection::open(&path).expect("inspect migration");
    let source_count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM reviews WHERE task_id = 'plan:legacy-lazy'",
            [],
            |row| row.get(0),
        )
        .expect("source count");
    let approval_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM plan_approvals", [], |row| row.get(0))
        .expect("approval count");
    assert_eq!(source_count, 0, "source row must be removed after commit");
    assert_eq!(approval_count, 0, "migration must not imply user approval");
}

#[test]
fn corrupted_material_hash_approval_and_state_fail_closed_without_secret_leaks() {
    for corruption in ["material", "hash", "approval-json", "state", "session"] {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = database_path(temp.path());
        let store = V1Store::open(&path).expect("open");
        let first = plan("task-corrupt", 1, None, "first");
        let revision = store.publish_plan_revision(&first, None).expect("publish");
        let approval = store
            .approve_plan_revision(
                "task-corrupt",
                &revision,
                "approval-corrupt",
                actor("actor-private", "session-private"),
            )
            .expect("approve");
        let connection = Connection::open(&path).expect("corrupt row");
        match corruption {
            "material" => {
                connection
                    .execute(
                        "UPDATE plan_revisions SET material_json=?1 WHERE plan_revision=?2",
                        params!["{\"payload-secret\":\"do-not-leak\"}", revision.as_str()],
                    )
                    .unwrap();
            }
            "hash" => {
                connection
                    .execute(
                        "UPDATE plan_revisions SET content_sha256='corrupt-hash' WHERE plan_revision=?1",
                        params![revision.as_str()],
                    )
                    .unwrap();
            }
            "approval-json" => {
                connection
                    .execute(
                        "UPDATE plan_approvals SET approval_json=?1 WHERE approval_id=?2",
                        params!["{\"payload-secret\":\"do-not-leak\"}", approval.approval_id],
                    )
                    .unwrap();
            }
            "state" => {
                connection
                    .execute(
                        "UPDATE plan_approvals SET state='superseded' WHERE approval_id=?1",
                        params![approval.approval_id],
                    )
                    .unwrap();
            }
            "session" => {
                connection
                    .execute(
                        "UPDATE plan_approvals SET session_id='session-secret-do-not-leak' WHERE approval_id=?1",
                        params![approval.approval_id],
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }
        drop(connection);

        let error = match corruption {
            "material" | "hash" => store
                .current_plan_revision("task-corrupt")
                .expect_err("corrupt plan must fail closed")
                .to_string(),
            _ => store
                .load_plan_approval("approval-corrupt")
                .expect_err("corrupt approval must fail closed")
                .to_string(),
        };
        assert!(
            error.contains("integrity validation"),
            "{corruption} returned non-generic error: {error}"
        );
        for secret in [
            "actor-private",
            "session-private",
            "session-secret-do-not-leak",
            "payload-secret",
            "do-not-leak",
        ] {
            assert!(
                !error.contains(secret),
                "{corruption} leaked {secret:?} through {error:?}"
            );
        }
    }
}

#[test]
fn corrupt_plan_cannot_authorize_a_snapshot_or_bypass_integrity_via_legacy_load() {
    for corruption in ["material", "hash"] {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = database_path(temp.path());
        let store = V1Store::open(&path).expect("open");
        let first = plan("task-gate-corrupt", 1, None, "first");
        let revision = store.publish_plan_revision(&first, None).expect("publish");
        let approval = store
            .approve_plan_revision(
                "task-gate-corrupt",
                &revision,
                "approval-gate-corrupt",
                actor("actor-private", "session-private"),
            )
            .expect("approve");
        let connection = Connection::open(&path).expect("corrupt plan row");
        match corruption {
            "material" => {
                connection
                    .execute(
                        "UPDATE plan_revisions SET material_json=?1 WHERE plan_revision=?2",
                        params!["{\"payload-secret\":\"do-not-leak\"}", revision.as_str()],
                    )
                    .unwrap();
            }
            "hash" => {
                connection
                    .execute(
                        "UPDATE plan_revisions SET content_sha256='corrupt-hash' WHERE plan_revision=?1",
                        params![revision.as_str()],
                    )
                    .unwrap();
            }
            _ => unreachable!(),
        }
        drop(connection);

        let legacy_result = store.load_plan("task-gate-corrupt");
        if let Err(error) = &legacy_result {
            let message = error.to_string();
            assert!(!message.contains("payload-secret"));
            assert!(!message.contains("do-not-leak"));
        }

        let before = row_count(&path, "run_snapshots");
        let snapshot_result = store.save_run_snapshot(&snapshot(
            "task-gate-corrupt",
            RunSnapshotPhase::Execution {
                approval: approval_ref(&approval),
            },
            corruption,
        ));
        if let Err(error) = &snapshot_result {
            let message = error.to_string();
            assert!(!message.contains("actor-private"));
            assert!(!message.contains("session-private"));
        }
        let legacy_failed = legacy_result.is_err();
        let snapshot_failed = snapshot_result.is_err();
        assert!(
            legacy_failed && snapshot_failed,
            "{corruption}: legacy_failed={legacy_failed}, snapshot_failed={snapshot_failed}"
        );
        assert_eq!(row_count(&path, "run_snapshots"), before);
    }
}

#[test]
fn invalid_approval_snapshot_error_does_not_persist_or_echo_identifiers() {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = database_path(temp.path());
    let store = V1Store::open(&path).expect("open");
    let fake = PlanApprovalRef {
        approval_id: "approval-secret-do-not-leak".into(),
        plan_revision: PlanRevisionRef::parse(format!("sha256:{}", "c".repeat(64))).unwrap(),
    };
    let error = store
        .save_run_snapshot(&snapshot(
            "task-no-plan",
            RunSnapshotPhase::Execution { approval: fake },
            "invalid",
        ))
        .expect_err("missing approval");
    assert!(matches!(error, V1StoreError::Serialization(_)));
    assert!(!error.to_string().contains("approval-secret-do-not-leak"));
    assert_eq!(row_count(&path, "run_snapshots"), 0);
}
