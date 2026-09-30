use r_code_harness_protocol::{HarnessId, PackageRef, Provenance};
use r_code_kernel::task::{
    Actor, Attempt, EvidenceRecord, EvidenceRequirement, NetworkCeiling, PlanApprovalRef,
    PlanRevisionRef, ReviewDisposition, RunSnapshotId, TaskContract, TaskExecution, TaskKind,
    TaskState, TransitionError, UnitSettlement, ValidationOutcome, WorkUnit, WorkUnitEffectClass,
    WorkUnitStatus,
};

fn approval() -> PlanApprovalRef {
    PlanApprovalRef {
        approval_id: "approval-1".into(),
        plan_revision: PlanRevisionRef(format!("sha256:{}", "a".repeat(64))),
    }
}

fn attempt(id: &str) -> Attempt {
    let snapshot = RunSnapshotId::parse(format!("sha256:{}", "b".repeat(64))).unwrap();
    Attempt {
        attempt_id: id.into(),
        task_id: "task-1".into(),
        branch_id: "branch-1".into(),
        package: PackageRef {
            id: HarnessId::new("native.r-code"),
            version: semver::Version::new(1, 0, 0),
            content_digest: "sha256:package".into(),
        },
        contract_revision: 1,
        config_hash: "legacy".into(),
        workspace_identity: "workspace-1".into(),
        run_id: format!("run-{id}"),
    }
    .with_run_snapshot(&snapshot)
}

fn unit(id: &str, dependencies: &[&str], writable: bool) -> WorkUnit {
    WorkUnit {
        id: id.into(),
        description: format!("unit {id}"),
        dependencies: dependencies.iter().map(|value| (*value).into()).collect(),
        acceptance: vec!["check:test".into()],
        read_paths: vec![],
        write_paths: writable.then(|| format!("src/{id}")).into_iter().collect(),
        repo_exclusive: false,
        ephemeral_roots: vec![],
        effect_class: WorkUnitEffectClass::ReadOnly,
        network_ceiling: NetworkCeiling::Offline,
        status: WorkUnitStatus::Pending,
    }
}

fn ready_state() -> (TaskState, PlanApprovalRef) {
    let mut state = TaskState::new(TaskContract {
        task_id: "task-1".into(),
        kind: TaskKind::Implementation,
        objective: "implement".into(),
        constraints: vec![],
        required_checks: vec!["check:test".into()],
        revision: 1,
    });
    state.start_attempt(&attempt("planning")).unwrap();
    let approval = approval();
    state
        .await_plan_approval(Actor::Host, "planning", 1, approval.plan_revision.clone())
        .unwrap();
    state
        .mark_plan_ready(Actor::Host, approval.clone())
        .unwrap();
    (state, approval)
}

fn evidence(
    id: &str,
    task_id: &str,
    definition: &str,
    environment: &str,
    passed: bool,
    provenance: Provenance,
) -> EvidenceRecord {
    EvidenceRecord {
        evidence_id: id.into(),
        task_id: task_id.into(),
        check_id: "check:test".into(),
        definition_identity: definition.into(),
        candidate_digest: "candidate-1".into(),
        environment: "test environment".into(),
        environment_fingerprint: environment.into(),
        passed,
        host_output: None,
        recorded_by: provenance,
    }
}

#[test]
fn execution_start_requires_host_exact_approval_ready_writable_unit_and_dependencies() {
    let (state, exact) = ready_state();
    let writable = unit("write", &[], true);

    let mut plugin = state.clone();
    assert!(matches!(
        plugin.start_execution_attempt(
            Actor::Plugin,
            &attempt("plugin"),
            &exact,
            vec![writable.clone()],
            "write"
        ),
        Err(TransitionError::HostTransitionRequired { .. })
    ));

    let mut wrong_approval = state.clone();
    let mut foreign = exact.clone();
    foreign.approval_id = "approval-other".into();
    assert!(wrong_approval
        .start_execution_attempt(
            Actor::Host,
            &attempt("wrong-approval"),
            &foreign,
            vec![writable.clone()],
            "write"
        )
        .is_err());

    let mut wrong_unit = state.clone();
    assert!(matches!(
        wrong_unit.start_execution_attempt(
            Actor::Host,
            &attempt("wrong-unit"),
            &exact,
            vec![writable.clone()],
            "missing"
        ),
        Err(TransitionError::UnknownWorkUnit(_))
    ));

    let mut empty_write = state.clone();
    assert!(empty_write
        .start_execution_attempt(
            Actor::Host,
            &attempt("empty-write"),
            &exact,
            vec![unit("read-only", &[], false)],
            "read-only"
        )
        .is_err());

    let mut completed_unit = unit("done", &[], true);
    completed_unit.status = WorkUnitStatus::Completed;
    let mut completed = state.clone();
    assert!(completed
        .start_execution_attempt(
            Actor::Host,
            &attempt("completed"),
            &exact,
            vec![completed_unit],
            "done"
        )
        .is_err());

    let mut blocked = state.clone();
    // A dependency that still needs an attempt (writable, Pending) blocks
    // its dependent. E05.3 note: a READ-ONLY dependency no longer blocks
    // forever — the state machine completes it without an attempt (pinned
    // right below) — so the gating pin uses a writable dependency.
    assert!(matches!(
        blocked.start_execution_attempt(
            Actor::Host,
            &attempt("blocked"),
            &exact,
            vec![unit("first", &[], true), unit("second", &["first"], true)],
            "second"
        ),
        Err(TransitionError::DependencyNotCompleted(dependency)) if dependency == "first"
    ));

    // E05.3: the same DAG with a read-only dependency starts the dependent —
    // the read-only unit settles Completed with a no-effect per-unit record
    // (no attempt, no child) and never holds an in-progress slot.
    let mut swept = state.clone();
    swept
        .start_execution_attempt(
            Actor::Host,
            &attempt("swept"),
            &exact,
            vec![unit("first", &[], false), unit("second", &["first"], true)],
            "second",
        )
        .unwrap();
    assert_eq!(swept.work_units[0].status, WorkUnitStatus::Completed);
    let swept_record = swept.unit_record("first").expect("no-effect record");
    assert_eq!(swept_record.attempt_id, None);
    assert_eq!(
        swept_record.settlement,
        UnitSettlement::CompletedWithoutEffect
    );
    assert_eq!(swept.work_units[1].status, WorkUnitStatus::InProgress);
    assert!(matches!(
        swept.execution,
        TaskExecution::Running { ref attempt_id, generation: 1 } if attempt_id == "swept"
    ));

    let mut valid = state;
    valid
        .start_execution_attempt(
            Actor::Host,
            &attempt("valid"),
            &exact,
            vec![writable],
            "write",
        )
        .unwrap();
    assert!(matches!(
        valid.execution,
        TaskExecution::Running { ref attempt_id, generation: 1 } if attempt_id == "valid"
    ));
    assert_eq!(valid.work_units[0].status, WorkUnitStatus::InProgress);
}

#[test]
fn review_ready_requires_exact_host_evidence_binding_and_sets_pending_review() {
    let (mut state, exact) = ready_state();
    state
        .start_execution_attempt(
            Actor::Host,
            &attempt("execution"),
            &exact,
            vec![unit("write", &[], true)],
            "write",
        )
        .unwrap();
    assert!(state
        .begin_verification(Actor::Plugin, "execution", "write", "candidate-1".into())
        .is_err());
    state
        .begin_verification(Actor::Host, "execution", "write", "candidate-1".into())
        .unwrap();
    let requirement = EvidenceRequirement {
        check_id: "check:test".into(),
        definition_identity: "definition-1".into(),
        environment_fingerprint: "environment-1".into(),
    };
    for record in [
        evidence(
            "foreign-task",
            "task-other",
            "definition-1",
            "environment-1",
            true,
            Provenance::Host,
        ),
        evidence(
            "wrong-definition",
            "task-1",
            "definition-other",
            "environment-1",
            true,
            Provenance::Host,
        ),
        evidence(
            "wrong-environment",
            "task-1",
            "definition-1",
            "environment-other",
            true,
            Provenance::Host,
        ),
        evidence(
            "plugin",
            "task-1",
            "definition-1",
            "environment-1",
            true,
            Provenance::Plugin {
                harness_id: "untrusted".into(),
                package_digest: "sha256:untrusted".into(),
            },
        ),
        evidence(
            "failed",
            "task-1",
            "definition-1",
            "environment-1",
            false,
            Provenance::Host,
        ),
    ] {
        state.record_evidence(record).unwrap();
    }
    assert_eq!(
        state.finish_verification(
            Actor::Host,
            "execution",
            "write",
            std::slice::from_ref(&requirement)
        ),
        Err(TransitionError::EvidenceRequired)
    );
    state
        .record_evidence(evidence(
            "valid",
            "task-1",
            "definition-1",
            "environment-1",
            true,
            Provenance::Host,
        ))
        .unwrap();
    state
        .finish_verification(Actor::Host, "execution", "write", &[requirement])
        .unwrap();
    assert!(matches!(
        state.execution,
        TaskExecution::ReviewReady { ref attempt_id } if attempt_id == "execution"
    ));
    assert_eq!(state.review, ReviewDisposition::Pending);
    // E05: the verification outcome lives on the unit's per-unit record.
    let record = state.unit_record("write").expect("per-unit record");
    assert_eq!(
        record.verification,
        ValidationOutcome::Verified {
            candidate_digest: "candidate-1".into()
        }
    );
    assert_eq!(record.settlement, UnitSettlement::Completed);
    assert_eq!(
        state.task_candidate_digest().as_deref(),
        Some("candidate-1")
    );
    assert_eq!(state.work_units[0].status, WorkUnitStatus::Completed);
}

#[test]
fn no_check_candidate_can_finish_but_failed_or_unavailable_paths_require_repair() {
    let (state, exact) = ready_state();
    let start_verifying = || {
        let mut current = state.clone();
        current
            .start_execution_attempt(
                Actor::Host,
                &attempt("execution"),
                &exact,
                vec![unit("write", &[], true)],
                "write",
            )
            .unwrap();
        current
            .begin_verification(Actor::Host, "execution", "write", "candidate-1".into())
            .unwrap();
        current
    };

    let mut no_checks = start_verifying();
    no_checks
        .finish_verification(Actor::Host, "execution", "write", &[])
        .unwrap();
    assert!(matches!(
        no_checks.execution,
        TaskExecution::ReviewReady { .. }
    ));

    let mut failed = start_verifying();
    failed
        .require_repair(
            Actor::Host,
            Some("execution".into()),
            Some("write".into()),
            "required check failed".into(),
            false,
        )
        .unwrap();
    assert!(matches!(
        failed.execution,
        TaskExecution::RepairRequired { .. }
    ));
    // E05: the failure outcome and reason live on the unit's per-unit
    // record; the task-level verdict names the failed unit.
    let record = failed.unit_record("write").expect("per-unit record");
    assert!(matches!(
        &record.verification,
        ValidationOutcome::Unverified { reason } if reason == "required check failed"
    ));
    assert_eq!(record.settlement, UnitSettlement::Failed);
    assert!(matches!(
        &failed.execution,
        TaskExecution::RepairRequired { ref work_unit_id, .. } if work_unit_id.as_deref() == Some("write")
    ));

    let mut unavailable = start_verifying();
    unavailable
        .require_repair(
            Actor::Host,
            Some("execution".into()),
            Some("write".into()),
            "required check unavailable".into(),
            true,
        )
        .unwrap();
    let record = unavailable.unit_record("write").expect("per-unit record");
    assert!(matches!(
        &record.verification,
        ValidationOutcome::CheckUnavailable { reason } if reason == "required check unavailable"
    ));
    assert_eq!(record.settlement, UnitSettlement::Failed);
}
