use r_code_harness_protocol::services::{NetworkCeiling, WorkUnitEffectClass, WorkUnitWire};
use r_code_harness_protocol::{ArtifactRef, HarnessId, PackageRef, Provenance};
use r_code_kernel::plans::{
    PlanApprovalActor, PlanRevision, PlanRevisionMaterial, PLAN_APPROVE_SCOPE,
};
use r_code_kernel::task::{
    EvidenceRecord, EvidenceRequirement, PermissionSnapshotRef, PlanApprovalRef,
    PromptSnapshotMode, PromptSnapshotRef, ProviderRouteKind, ProviderSnapshotRef, RunSnapshot,
    RunSnapshotMaterial, RunSnapshotPhase, WorkspaceSnapshotRef,
};
use r_code_kernel::verification::{CheckDefinition, CheckEntrypoint, ControlFile};
use r_code_store::v1::verification::VerificationStoreError;
use r_code_store::v1::V1Store;
use rusqlite::{params, Connection};

fn definition(check_id: &str) -> CheckDefinition {
    CheckDefinition {
        check_id: check_id.into(),
        entrypoint: CheckEntrypoint::Command {
            program: "node".into(),
            argv: vec!["verify.js".into()],
        },
        control_files: vec![ControlFile {
            path: "verify.js".into(),
            sha256: "a".repeat(64),
        }],
        source_roots: vec!["src".into()],
        dependency_locks: vec!["Cargo.lock".into()],
        toolchain: "node-20".into(),
        declared_external_inputs: vec![],
        entrypoint_bytes: None,
    }
}

fn evidence(evidence_id: &str, definition: &CheckDefinition, candidate: &str) -> EvidenceRecord {
    EvidenceRecord {
        evidence_id: evidence_id.into(),
        task_id: "task-1".into(),
        check_id: definition.check_id.clone(),
        definition_identity: definition.identity(),
        candidate_digest: candidate.into(),
        environment: "node-20 windows-x64".into(),
        environment_fingerprint: "b".repeat(64),
        passed: true,
        host_output: Some(ArtifactRef {
            schema: ArtifactRef::SCHEMA,
            blob_id: format!("blob:sha256:{}", "c".repeat(64)),
            bytes: 12,
            sha256: "c".repeat(64),
            media_type: Some("text/plain".into()),
        }),
        recorded_by: Provenance::Host,
    }
}

#[test]
fn definitions_and_evidence_are_separate_immutable_and_idempotent() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("store.db");
    let store = V1Store::open(&path).unwrap();
    let definition = definition("check:test");
    store.save_check_definition(&definition).unwrap();
    store.save_check_definition(&definition).unwrap();
    assert_eq!(
        store.load_check_definition("check:test").unwrap(),
        Some(definition.clone())
    );

    let mut changed_definition = definition.clone();
    changed_definition.toolchain = "node-22".into();
    assert!(matches!(
        store.save_check_definition(&changed_definition),
        Err(VerificationStoreError::Conflict)
    ));

    let record = evidence("evidence-1", &definition, "candidate-1");
    store.save_evidence(&record).unwrap();
    store.save_evidence(&record).unwrap();
    let mut changed_record = record.clone();
    changed_record.environment_fingerprint = "d".repeat(64);
    assert!(matches!(
        store.save_evidence(&changed_record),
        Err(VerificationStoreError::Conflict)
    ));
    let mut duplicate_identity = record.clone();
    duplicate_identity.evidence_id = "evidence-2".into();
    assert!(matches!(
        store.save_evidence(&duplicate_identity),
        Err(VerificationStoreError::Conflict)
    ));

    let raw = Connection::open(&path).unwrap();
    let definitions: i64 = raw
        .query_row("SELECT COUNT(*) FROM check_definitions", [], |row| {
            row.get(0)
        })
        .unwrap();
    let evidence_rows: i64 = raw
        .query_row("SELECT COUNT(*) FROM evidence", [], |row| row.get(0))
        .unwrap();
    assert_eq!((definitions, evidence_rows), (1, 1));
    assert_eq!(
        store.evidence_for_candidate("candidate-1").unwrap(),
        [record]
    );
}

#[test]
fn incomplete_foreign_definition_or_plugin_evidence_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&temp.path().join("store.db")).unwrap();
    let definition = definition("check:test");
    store.save_check_definition(&definition).unwrap();

    let mut wrong_definition = evidence("wrong-definition", &definition, "candidate-1");
    wrong_definition.definition_identity = "wrong".into();
    assert!(matches!(
        store.save_evidence(&wrong_definition),
        Err(VerificationStoreError::InvalidEvidence)
    ));

    let mut incomplete = evidence("incomplete", &definition, "candidate-1");
    incomplete.environment_fingerprint.clear();
    assert!(matches!(
        store.save_evidence(&incomplete),
        Err(VerificationStoreError::InvalidEvidence)
    ));

    let mut plugin = evidence("plugin", &definition, "candidate-1");
    plugin.recorded_by = Provenance::Plugin {
        harness_id: "untrusted".into(),
        package_digest: "sha256:untrusted".into(),
    };
    assert!(matches!(
        store.save_evidence(&plugin),
        Err(VerificationStoreError::InvalidEvidence)
    ));

    let mut forged_artifact = evidence("forged-artifact", &definition, "candidate-forged");
    forged_artifact.host_output = Some(ArtifactRef {
        schema: 99,
        blob_id: "blob:forged".into(),
        bytes: 1,
        sha256: "not-a-sha256".into(),
        media_type: None,
    });
    assert!(matches!(
        store.save_evidence(&forged_artifact),
        Err(VerificationStoreError::InvalidEvidence)
    ));

    let requirement = EvidenceRequirement {
        check_id: definition.check_id.clone(),
        definition_identity: definition.identity(),
        environment_fingerprint: "b".repeat(64),
    };
    let mut foreign_task = evidence("foreign-task", &definition, "candidate-1");
    foreign_task.task_id = "task-other".into();
    assert!(!foreign_task.is_valid_for_binding(&requirement, "task-1", "candidate-1"));
    let mut failed = evidence("failed", &definition, "candidate-1");
    failed.passed = false;
    assert!(!failed.is_valid_for_binding(&requirement, "task-1", "candidate-1"));
}

fn execution_snapshot(approval: PlanApprovalRef, work_unit_id: &str) -> RunSnapshot {
    RunSnapshot::new(RunSnapshotMaterial {
        task_id: "task-snapshot".into(),
        task_revision: 1,
        phase: RunSnapshotPhase::Execution { approval },
        work_unit_id: Some(work_unit_id.into()),
        provider: ProviderSnapshotRef {
            kind: ProviderRouteKind::HostProvider,
            settings_revision: 1,
            provider_id: "provider".into(),
            model_id: "model".into(),
            base_url: None,
            protocol: None,
            capabilities: vec![],
        },
        prompt: PromptSnapshotRef {
            revision: "prompt-1".into(),
            mode: PromptSnapshotMode::Default,
            content_sha256: "sha256:prompt".into(),
            resolved_system_prompt: "system".into(),
        },
        workspace: WorkspaceSnapshotRef {
            canonical_root: "D:/workspace".into(),
            workspace_identity: "workspace-1".into(),
            baseline_sha256: "sha256:baseline".into(),
        },
        permissions: PermissionSnapshotRef {
            revision: "permissions-1".into(),
            profile_id: "workspace-write".into(),
            capabilities: vec!["host.fs.write".into()],
        },
        harness_package: PackageRef {
            id: HarnessId::new("native.r-code"),
            version: "1.0.0".parse().unwrap(),
            content_digest: "sha256:package".into(),
        },
        tool_catalog_sha256: "sha256:tools".into(),
        inference: None,
        instructions: Default::default(),
    })
    .unwrap()
}

#[test]
fn execution_snapshot_persistence_rejects_work_unit_outside_approved_plan() {
    let temp = tempfile::tempdir().unwrap();
    let store = V1Store::open(&temp.path().join("snapshot.db")).unwrap();
    let plan = PlanRevision::new(PlanRevisionMaterial {
        task_id: "task-snapshot".into(),
        revision: 1,
        parent_revision: None,
        current_base_hash: "sha256:base".into(),
        workspace_baseline: "sha256:baseline".into(),
        route_digest: "sha256:route".into(),
        prompt_digest: "sha256:prompt".into(),
        permission_digest: "sha256:permissions".into(),
        check_digest: "sha256:checks".into(),
        required_checks: vec![],
        work_units: vec![WorkUnitWire {
            id: "known-unit".into(),
            description: "known".into(),
            dependencies: vec![],
            acceptance: vec![],
            read_paths: vec![],
            write_paths: vec!["src".into()],
            repo_exclusive: false,
            ephemeral_roots: vec![],
            effect_class: WorkUnitEffectClass::ReadOnly,
            network_ceiling: NetworkCeiling::Offline,
        }],
    })
    .unwrap();
    store.publish_plan_revision(&plan, None).unwrap();
    let approval = store
        .approve_plan_revision(
            "task-snapshot",
            plan.reference(),
            "approval-snapshot",
            PlanApprovalActor::new("actor", "session", PLAN_APPROVE_SCOPE).unwrap(),
        )
        .unwrap();
    let approval_ref = PlanApprovalRef {
        approval_id: approval.approval_id,
        plan_revision: approval.plan_revision,
    };
    assert!(store
        .save_run_snapshot(&execution_snapshot(approval_ref, "missing-unit"))
        .is_err());
}

#[test]
fn old_evidence_schema_migrates_transactionally_and_reopens_idempotently() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("legacy.db");
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch(
        "CREATE TABLE evidence (
            evidence_id TEXT PRIMARY KEY,
            task_id TEXT NOT NULL,
            check_id TEXT NOT NULL,
            candidate_digest TEXT NOT NULL,
            environment TEXT NOT NULL,
            passed INTEGER NOT NULL,
            host_output_ref TEXT,
            provenance_json TEXT NOT NULL,
            created_at_ms INTEGER NOT NULL
         );",
    )
    .unwrap();
    raw.execute(
        "INSERT INTO evidence(evidence_id, task_id, check_id, candidate_digest,
          environment, passed, host_output_ref, provenance_json, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, 1, NULL, ?6, 1)",
        params![
            "legacy",
            "task-1",
            "check:test",
            "candidate-legacy",
            "legacy-env",
            serde_json::to_string(&Provenance::Host).unwrap()
        ],
    )
    .unwrap();
    drop(raw);

    for _ in 0..3 {
        drop(V1Store::open(&path).unwrap());
    }
    let raw = Connection::open(&path).unwrap();
    let columns = raw
        .prepare("PRAGMA table_info(evidence)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(columns.contains(&"definition_identity".to_string()));
    assert!(columns.contains(&"environment_fingerprint".to_string()));
    let migrations: i64 = raw
        .query_row(
            "SELECT COUNT(*) FROM v1_schema_migrations
             WHERE migration_id = 'check-definitions-and-evidence-identity'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(migrations, 1);
    let legacy: (String, String) = raw
        .query_row(
            "SELECT definition_identity, environment_fingerprint
             FROM evidence WHERE evidence_id = 'legacy'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(legacy, (String::new(), String::new()));
}

fn corrupt_and_query(
    mutation: impl FnOnce(&Connection),
) -> Result<Vec<EvidenceRecord>, VerificationStoreError> {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("corrupt.db");
    let store = V1Store::open(&path).unwrap();
    let definition = definition("check:test");
    store.save_check_definition(&definition).unwrap();
    store
        .save_evidence(&evidence("evidence-1", &definition, "candidate-1"))
        .unwrap();
    let raw = Connection::open(&path).unwrap();
    mutation(&raw);
    store.evidence_for_candidate("candidate-1")
}

#[test]
fn corrupt_definition_artifact_and_provenance_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("definition.db");
    let store = V1Store::open(&path).unwrap();
    let definition = definition("check:test");
    store.save_check_definition(&definition).unwrap();
    Connection::open(&path)
        .unwrap()
        .execute(
            "UPDATE check_definitions SET identity = 'forged' WHERE check_id = 'check:test'",
            [],
        )
        .unwrap();
    assert!(store.load_check_definition("check:test").is_err());

    assert!(corrupt_and_query(|raw| {
        raw.execute(
            "UPDATE evidence SET host_output_ref = '{\"schema\":1,\"blob_id\":\"forged\",\"bytes\":1,\"sha256\":\"bad\"}'",
            [],
        )
        .unwrap();
    })
    .is_err());
    assert!(corrupt_and_query(|raw| {
        raw.execute("UPDATE evidence SET provenance_json = '{not-json}'", [])
            .unwrap();
    })
    .is_err());
}
