//! E05 contract tests: the kernel task state machine generalized to N
//! concurrent in-progress work units with per-unit outcomes.
//!
//! Arms (one per declared test in o-gate.tasks.json task E05):
//! 1. `two_units_run_concurrently` — both units InProgress with their own
//!    records, independent settle, task-level ReviewReady only after BOTH
//!    are terminal.
//! 2. `failed_unit_blocks_only_its_dependents` — a failed unit blocks
//!    exactly its transitive dependents; the started sibling completes; the
//!    aggregate lands RepairRequired naming the failed unit.
//! 3. `read_only_unit_completes_without_attempt` — a read-only unit with
//!    satisfied dependencies reaches CompletedWithoutEffect with no attempt
//!    and never occupies an in-progress slot.
//! 4. `accept_refused_while_any_unit_unsettled` — accept_verified /
//!    accept_unverified refuse while any started unit is unsettled; once
//!    settled, the exact attempt/candidate/actor checks still bite.
//! 5. `legacy_single_slot_row_folds_into_one_unit_record` — pre-E05
//!    persisted rows (top-level validation/candidate_digest, Verifying
//!    carrying work_unit_id) decode and fold into one per-unit record;
//!    new-shape serialization emits no legacy field.

use r_code_harness_protocol::{HarnessId, PackageRef};
use r_code_kernel::task::{
    Actor, Attempt, NetworkCeiling, PlanApprovalRef, PlanRevisionRef, ReviewDisposition,
    RunSnapshotId, TaskContract, TaskExecution, TaskKind, TaskState, TaskVerdict, UnitRecord,
    UnitSettlement, ValidationOutcome, WorkUnit, WorkUnitEffectClass, WorkUnitStatus,
};
use std::collections::BTreeMap;

// -- shared fixtures (m03/m04 idioms) ----------------------------------------

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

fn task_contract() -> TaskContract {
    TaskContract {
        task_id: "task-1".into(),
        kind: TaskKind::Implementation,
        objective: "concurrent units".into(),
        constraints: vec![],
        required_checks: vec![],
        revision: 1,
    }
}

/// A task that finished planning and sits in Ready with the exact approval.
fn ready_state() -> (TaskState, PlanApprovalRef) {
    let mut state = TaskState::new(task_contract());
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

fn status_of(state: &TaskState, id: &str) -> WorkUnitStatus {
    state
        .work_units
        .iter()
        .find(|unit| unit.id == id)
        .unwrap_or_else(|| panic!("work unit {id} missing"))
        .status
}

// -- arm 1: concurrent units, aggregate ReviewReady --------------------------

#[test]
fn two_units_run_concurrently() {
    let (mut state, exact) = ready_state();
    let plan = vec![unit("alpha", &[], true), unit("beta", &[], true)];

    // First unit of the wave starts from Ready.
    state
        .start_execution_attempt(
            Actor::Host,
            &attempt("attempt-alpha"),
            &exact,
            plan.clone(),
            "alpha",
        )
        .unwrap();
    // Second unit arms the SAME approval and starts while alpha is still
    // in progress: N concurrent InProgress units.
    state
        .start_execution_attempt(
            Actor::Host,
            &attempt("attempt-beta"),
            &exact,
            plan.clone(),
            "beta",
        )
        .unwrap();

    assert_eq!(status_of(&state, "alpha"), WorkUnitStatus::InProgress);
    assert_eq!(status_of(&state, "beta"), WorkUnitStatus::InProgress);
    assert_eq!(state.unit_records.len(), 2);
    let alpha = state.unit_record("alpha").expect("alpha per-unit record");
    assert_eq!(alpha.attempt_id.as_deref(), Some("attempt-alpha"));
    assert_eq!(alpha.candidate_digest, None);
    assert_eq!(alpha.verification, ValidationOutcome::NotEvaluated);
    assert_eq!(alpha.settlement, UnitSettlement::InFlight);
    let beta = state.unit_record("beta").expect("beta per-unit record");
    assert_eq!(beta.attempt_id.as_deref(), Some("attempt-beta"));
    assert_eq!(beta.settlement, UnitSettlement::InFlight);
    assert!(state.has_unsettled_units());

    // alpha settles independently while beta keeps executing.
    state
        .begin_verification(
            Actor::Host,
            "attempt-alpha",
            "alpha",
            "candidate-alpha".into(),
        )
        .unwrap();
    // The verifying unit is derived from the per-unit records (E05).
    assert_eq!(state.verifying_work_unit_id().as_deref(), Some("alpha"));
    state
        .finish_verification(Actor::Host, "attempt-alpha", "alpha", &[])
        .unwrap();

    let alpha = state.unit_record("alpha").expect("alpha per-unit record");
    assert_eq!(
        alpha.verification,
        ValidationOutcome::Verified {
            candidate_digest: "candidate-alpha".into()
        }
    );
    assert_eq!(alpha.settlement, UnitSettlement::Completed);
    assert_eq!(status_of(&state, "alpha"), WorkUnitStatus::Completed);

    // ReviewReady must NOT fire after the first settle: beta is unsettled.
    assert!(matches!(state.execution, TaskExecution::Running { .. }));
    assert!(!matches!(
        state.execution,
        TaskExecution::ReviewReady { .. }
    ));
    assert_eq!(state.review, ReviewDisposition::NotRequired);
    assert_eq!(status_of(&state, "beta"), WorkUnitStatus::InProgress);
    assert_eq!(
        state
            .unit_record("beta")
            .expect("beta per-unit record")
            .settlement,
        UnitSettlement::InFlight
    );
    assert!(state.has_unsettled_units());

    // beta settles: NOW the task-level aggregate fires.
    state
        .begin_verification(Actor::Host, "attempt-beta", "beta", "candidate-beta".into())
        .unwrap();
    state
        .finish_verification(Actor::Host, "attempt-beta", "beta", &[])
        .unwrap();

    assert!(matches!(
        &state.execution,
        TaskExecution::ReviewReady { attempt_id } if attempt_id == "attempt-beta"
    ));
    assert_eq!(state.review, ReviewDisposition::Pending);
    assert!(!state.has_unsettled_units());
    assert!(state.active_approval.is_none());
    assert_eq!(status_of(&state, "alpha"), WorkUnitStatus::Completed);
    assert_eq!(status_of(&state, "beta"), WorkUnitStatus::Completed);
    // The task-level digest is derived from the anchored per-unit record.
    assert_eq!(
        state.task_candidate_digest().as_deref(),
        Some("candidate-beta")
    );
}

// -- arm 2: a failed unit blocks exactly its dependents ----------------------

#[test]
fn failed_unit_blocks_only_its_dependents() {
    let (mut state, exact) = ready_state();
    let plan = vec![
        unit("base", &[], true),
        unit("child", &["base"], true),
        unit("grandchild", &["child"], true),
        unit("sibling", &[], true),
        unit("unrelated", &[], true),
    ];
    state
        .start_execution_attempt(
            Actor::Host,
            &attempt("attempt-base"),
            &exact,
            plan.clone(),
            "base",
        )
        .unwrap();
    state
        .start_execution_attempt(
            Actor::Host,
            &attempt("attempt-sibling"),
            &exact,
            plan.clone(),
            "sibling",
        )
        .unwrap();

    state
        .begin_verification(Actor::Host, "attempt-base", "base", "candidate-base".into())
        .unwrap();
    state
        .require_repair(
            Actor::Host,
            Some("attempt-base".into()),
            Some("base".into()),
            "required check failed".into(),
            false,
        )
        .unwrap();

    // The per-unit failure reason lives on base's own record.
    let base = state.unit_record("base").expect("base per-unit record");
    assert_eq!(base.settlement, UnitSettlement::Failed);
    assert_eq!(
        base.verification,
        ValidationOutcome::Unverified {
            reason: "required check failed".into()
        }
    );

    // Exactly base's transitive dependents are blocked (dependency-aware).
    assert_eq!(status_of(&state, "base"), WorkUnitStatus::Blocked);
    assert_eq!(status_of(&state, "child"), WorkUnitStatus::Blocked);
    assert_eq!(status_of(&state, "grandchild"), WorkUnitStatus::Blocked);
    // The started sibling keeps executing; a never-started unrelated unit
    // is untouched.
    assert_eq!(status_of(&state, "sibling"), WorkUnitStatus::InProgress);
    assert_eq!(status_of(&state, "unrelated"), WorkUnitStatus::Pending);
    // The task-level verdict waits for the in-flight sibling.
    assert!(matches!(state.execution, TaskExecution::Running { .. }));
    assert!(!matches!(
        state.execution,
        TaskExecution::RepairRequired { .. }
    ));

    // The sibling completes; the aggregate lands RepairRequired NAMING base.
    state
        .begin_verification(
            Actor::Host,
            "attempt-sibling",
            "sibling",
            "candidate-sibling".into(),
        )
        .unwrap();
    state
        .finish_verification(Actor::Host, "attempt-sibling", "sibling", &[])
        .unwrap();

    assert!(matches!(
        &state.execution,
        TaskExecution::RepairRequired { ref work_unit_id, .. }
            if work_unit_id.as_deref() == Some("base")
    ));
    assert_eq!(state.review, ReviewDisposition::NotRequired);
    assert_eq!(status_of(&state, "sibling"), WorkUnitStatus::Completed);
    let sibling = state
        .unit_record("sibling")
        .expect("sibling per-unit record");
    assert_eq!(sibling.settlement, UnitSettlement::Completed);
    assert_eq!(
        sibling.verification,
        ValidationOutcome::Verified {
            candidate_digest: "candidate-sibling".into()
        }
    );
    // The failed unit's dependents stay blocked; nothing else regressed.
    assert_eq!(status_of(&state, "child"), WorkUnitStatus::Blocked);
    assert_eq!(status_of(&state, "grandchild"), WorkUnitStatus::Blocked);
    assert_eq!(status_of(&state, "unrelated"), WorkUnitStatus::Pending);
}

// -- arm 3: read-only units complete without an attempt ----------------------

#[test]
fn read_only_unit_completes_without_attempt() {
    let mut state = TaskState::new(task_contract());
    let mut exclusive = unit("exclusive", &[], false);
    exclusive.repo_exclusive = true;
    state.work_units = vec![
        unit("reader", &[], false),
        // A read-only chain: the fixpoint sweeps the dependent once its
        // read-only dependency settles.
        unit("chained-reader", &["reader"], false),
        // Read-only but its writable dependency is not Completed yet.
        unit("waiting-reader", &["writer"], false),
        unit("writer", &[], true),
        // Repo-exclusive units are never swept even without write scope.
        exclusive,
    ];

    let swept = state.complete_read_only_units();
    assert_eq!(swept, 2);

    for id in ["reader", "chained-reader"] {
        assert_eq!(status_of(&state, id), WorkUnitStatus::Completed, "{id}");
        let record = state
            .unit_record(id)
            .unwrap_or_else(|| panic!("{id} per-unit record"));
        assert_eq!(record.attempt_id, None, "{id} completed without an attempt");
        assert_eq!(record.candidate_digest, None, "{id}");
        assert_eq!(record.verification, ValidationOutcome::NotEvaluated, "{id}");
        assert_eq!(
            record.settlement,
            UnitSettlement::CompletedWithoutEffect,
            "{id}"
        );
    }

    // A swept read-only unit never occupies an in-progress slot...
    assert!(!state
        .work_units
        .iter()
        .any(|unit| unit.status == WorkUnitStatus::InProgress));
    // ...and its no-effect settlement counts as terminal for the aggregate.
    assert!(!state.has_unsettled_units());

    // Nothing else was swept: unsatisfied dependency, writable, and
    // repo-exclusive units stay Pending with no per-unit record.
    for id in ["waiting-reader", "writer", "exclusive"] {
        assert_eq!(status_of(&state, id), WorkUnitStatus::Pending, "{id}");
        assert!(state.unit_record(id).is_none(), "{id}");
    }

    // The sweep is idempotent.
    assert_eq!(state.complete_read_only_units(), 0);
}

// -- arm 4: accept refuses while any started unit is unsettled ---------------

#[test]
fn accept_refused_while_any_unit_unsettled() {
    // Half-settled wave via real transitions: alpha settled, beta in flight.
    let (mut state, exact) = ready_state();
    let plan = vec![unit("alpha", &[], true), unit("beta", &[], true)];
    state
        .start_execution_attempt(
            Actor::Host,
            &attempt("attempt-alpha"),
            &exact,
            plan.clone(),
            "alpha",
        )
        .unwrap();
    state
        .start_execution_attempt(
            Actor::Host,
            &attempt("attempt-beta"),
            &exact,
            plan.clone(),
            "beta",
        )
        .unwrap();
    state
        .begin_verification(
            Actor::Host,
            "attempt-alpha",
            "alpha",
            "candidate-alpha".into(),
        )
        .unwrap();
    state
        .finish_verification(Actor::Host, "attempt-alpha", "alpha", &[])
        .unwrap();
    // beta is still unsettled: neither accept is possible on the strength
    // of alpha's settle alone.
    assert!(state
        .accept_verified(
            Actor::User,
            "attempt-alpha",
            "candidate-alpha",
            "authenticated-user"
        )
        .is_err());
    let checks = vec!["check:a".to_string()];
    assert!(state
        .accept_unverified(
            Actor::User,
            "candidate-alpha",
            "authenticated-user",
            "reason",
            &checks
        )
        .is_err());

    // Settle the wave: the exact attempt/candidate/actor checks still bite.
    state
        .begin_verification(Actor::Host, "attempt-beta", "beta", "candidate-beta".into())
        .unwrap();
    state
        .finish_verification(Actor::Host, "attempt-beta", "beta", &[])
        .unwrap();
    assert!(matches!(
        &state.execution,
        TaskExecution::ReviewReady { attempt_id } if attempt_id == "attempt-beta"
    ));

    // Wrong attempt.
    assert!(state
        .accept_verified(
            Actor::User,
            "attempt-alpha",
            "candidate-beta",
            "authenticated-user"
        )
        .is_err());
    // Wrong candidate.
    assert!(state
        .accept_verified(
            Actor::User,
            "attempt-beta",
            "candidate-alpha",
            "authenticated-user"
        )
        .is_err());
    // Empty actor.
    assert!(state
        .accept_verified(Actor::User, "attempt-beta", "candidate-beta", "")
        .is_err());
    // Single-actor happy path accepted.
    state
        .accept_verified(
            Actor::User,
            "attempt-beta",
            "candidate-beta",
            "authenticated-user",
        )
        .unwrap();
    assert_eq!(state.review, ReviewDisposition::Accepted);
    assert!(matches!(
        &state.execution,
        TaskExecution::Terminal {
            verdict: TaskVerdict::VerifiedAccepted {
                ref candidate_digest,
                ref actor_id
            }
        } if candidate_digest == "candidate-beta" && actor_id == "authenticated-user"
    ));

    // The un-settled guard itself, pinned directly: a ReviewReady-shaped
    // task that still carries a started-but-unsettled unit refuses the
    // exact attempt/candidate/actor triple.
    let mut review_ready_unsettled = review_ready_with_unsettled_sibling();
    assert!(review_ready_unsettled
        .accept_verified(
            Actor::User,
            "attempt-1",
            "candidate-1",
            "authenticated-user"
        )
        .is_err());
    assert!(review_ready_unsettled
        .accept_unverified(
            Actor::User,
            "candidate-1",
            "authenticated-user",
            "accept risk",
            &checks
        )
        .is_err());

    // Same guard for the override path: RepairRequired with an unsettled
    // sibling refuses; once the sibling settles, the override is accepted.
    let mut repair_unsettled = repair_required_with_unsettled_sibling();
    assert!(repair_unsettled
        .accept_unverified(
            Actor::User,
            "candidate-1",
            "authenticated-user",
            "accept risk",
            &checks
        )
        .is_err());
    repair_unsettled
        .work_units
        .iter_mut()
        .find(|unit| unit.id == "beta")
        .expect("beta unit")
        .status = WorkUnitStatus::Completed;
    repair_unsettled
        .unit_record_mut("beta")
        .expect("beta per-unit record")
        .settlement = UnitSettlement::Completed;
    repair_unsettled
        .accept_unverified(
            Actor::User,
            "candidate-1",
            "authenticated-user",
            "accept risk",
            &checks,
        )
        .unwrap();
    assert!(matches!(
        &repair_unsettled.execution,
        TaskExecution::Terminal {
            verdict: TaskVerdict::UnverifiedAccepted { .. }
        }
    ));
}

/// ReviewReady-shaped state whose wave still holds a started, unsettled unit
/// (beta under attempt-2) — the aggregate would not legally produce this,
/// which is exactly what the accept guard must defend against.
fn review_ready_with_unsettled_sibling() -> TaskState {
    let mut state = TaskState::new(task_contract());
    state.work_units = vec![unit("alpha", &[], true), unit("beta", &[], true)];
    state.work_units[0].status = WorkUnitStatus::Completed;
    state.work_units[1].status = WorkUnitStatus::InProgress;
    let mut records = BTreeMap::new();
    records.insert(
        "alpha".to_string(),
        UnitRecord {
            attempt_id: Some("attempt-1".into()),
            candidate_digest: Some("candidate-1".into()),
            verification: ValidationOutcome::Verified {
                candidate_digest: "candidate-1".into(),
            },
            settlement: UnitSettlement::Completed,
        },
    );
    records.insert(
        "beta".to_string(),
        UnitRecord {
            attempt_id: Some("attempt-2".into()),
            candidate_digest: None,
            verification: ValidationOutcome::NotEvaluated,
            settlement: UnitSettlement::InFlight,
        },
    );
    state.unit_records = records;
    state.review = ReviewDisposition::Pending;
    state.execution = TaskExecution::ReviewReady {
        attempt_id: "attempt-1".into(),
    };
    state
}

/// RepairRequired-shaped state whose wave still holds a started, unsettled
/// sibling (beta under attempt-2) while alpha already failed under attempt-1.
fn repair_required_with_unsettled_sibling() -> TaskState {
    let mut state = TaskState::new(task_contract());
    state.work_units = vec![unit("alpha", &[], true), unit("beta", &[], true)];
    state.work_units[0].status = WorkUnitStatus::Blocked;
    state.work_units[1].status = WorkUnitStatus::InProgress;
    let mut records = BTreeMap::new();
    records.insert(
        "alpha".to_string(),
        UnitRecord {
            attempt_id: Some("attempt-1".into()),
            candidate_digest: Some("candidate-1".into()),
            verification: ValidationOutcome::Unverified {
                reason: "required check failed".into(),
            },
            settlement: UnitSettlement::Failed,
        },
    );
    records.insert(
        "beta".to_string(),
        UnitRecord {
            attempt_id: Some("attempt-2".into()),
            candidate_digest: None,
            verification: ValidationOutcome::NotEvaluated,
            settlement: UnitSettlement::InFlight,
        },
    );
    state.unit_records = records;
    state.execution = TaskExecution::RepairRequired {
        attempt_id: Some("attempt-1".into()),
        work_unit_id: Some("alpha".into()),
        reason: "required check failed".into(),
    };
    state
}

// -- arm 5: legacy single-slot rows fold into one per-unit record ------------

#[test]
fn legacy_single_slot_row_folds_into_one_unit_record() {
    // Pre-E05 persisted shape: the verification material lived in top-level
    // single slots and the Verifying phase carried the verifying unit's id.
    // The store's load_task decodes with a silent .ok(), so this row must
    // decode AND fold — never silently vanish.
    let legacy_verifying = r#"{
        "contract": {
            "task_id": "task-legacy",
            "kind": "implementation",
            "objective": "legacy row",
            "constraints": [],
            "required_checks": ["check:test"],
            "revision": 1
        },
        "work_units": [
            {
                "id": "unit-a",
                "description": "legacy unit",
                "dependencies": [],
                "acceptance": ["check:test"],
                "write_paths": ["src/a"],
                "status": "in-progress"
            }
        ],
        "execution": {
            "phase": "verifying",
            "attempt_id": "attempt-legacy",
            "work_unit_id": "unit-a"
        },
        "review": "not-required",
        "evidence": [],
        "validation": { "state": "in-progress" },
        "candidate_digest": "candidate-legacy"
    }"#;
    let state: TaskState = serde_json::from_str(legacy_verifying)
        .expect("legacy Verifying row must decode instead of being silently dropped");
    assert_eq!(state.unit_records.len(), 1, "exactly one folded record");
    let record = state
        .unit_record("unit-a")
        .expect("the fold targets the unit the legacy Verifying phase named");
    assert_eq!(record.attempt_id.as_deref(), Some("attempt-legacy"));
    assert_eq!(record.candidate_digest.as_deref(), Some("candidate-legacy"));
    assert_eq!(record.verification, ValidationOutcome::InProgress);
    assert_eq!(record.settlement, UnitSettlement::InFlight);
    assert!(matches!(
        &state.execution,
        TaskExecution::Verifying { attempt_id } if attempt_id == "attempt-legacy"
    ));
    assert_new_shape_round_trips(&state);

    // A legacy ReviewReady row: the single-slot Verified outcome and digest
    // fold onto the one Completed unit.
    let legacy_review_ready = r#"{
        "contract": {
            "task_id": "task-legacy",
            "kind": "implementation",
            "objective": "legacy row",
            "constraints": [],
            "required_checks": ["check:test"],
            "revision": 1
        },
        "work_units": [
            {
                "id": "unit-a",
                "description": "legacy unit",
                "dependencies": [],
                "acceptance": ["check:test"],
                "write_paths": ["src/a"],
                "status": "completed"
            }
        ],
        "execution": {
            "phase": "review-ready",
            "attempt_id": "attempt-legacy"
        },
        "review": "pending",
        "evidence": [],
        "validation": { "state": "verified", "candidate_digest": "candidate-legacy" },
        "candidate_digest": "candidate-legacy"
    }"#;
    let state: TaskState = serde_json::from_str(legacy_review_ready)
        .expect("legacy ReviewReady row must decode instead of being silently dropped");
    assert_eq!(state.unit_records.len(), 1, "exactly one folded record");
    let record = state
        .unit_record("unit-a")
        .expect("the fold targets the single Completed unit");
    assert_eq!(record.attempt_id.as_deref(), Some("attempt-legacy"));
    assert_eq!(record.candidate_digest.as_deref(), Some("candidate-legacy"));
    assert_eq!(
        record.verification,
        ValidationOutcome::Verified {
            candidate_digest: "candidate-legacy".into()
        }
    );
    assert_eq!(record.settlement, UnitSettlement::Completed);
    assert_eq!(state.review, ReviewDisposition::Pending);
    // The task-level digest is re-derived from the folded per-unit record.
    assert_eq!(
        state.task_candidate_digest().as_deref(),
        Some("candidate-legacy")
    );
    assert_new_shape_round_trips(&state);
}

/// A new-shape serialization carries the per-unit material only: no legacy
/// top-level slots, no legacy Verifying payload, and it round-trips.
fn assert_new_shape_round_trips(state: &TaskState) {
    let serialized = serde_json::to_string(state).unwrap();
    let value: serde_json::Value = serde_json::from_str(&serialized).unwrap();
    assert!(
        value.get("validation").is_none(),
        "new-shape row must not emit legacy top-level validation: {serialized}"
    );
    assert!(
        value.get("candidate_digest").is_none(),
        "new-shape row must not emit legacy top-level candidate_digest: {serialized}"
    );
    assert!(
        value
            .get("execution")
            .and_then(|execution| execution.get("work_unit_id"))
            .is_none(),
        "new-shape row must not emit the legacy Verifying work_unit_id: {serialized}"
    );
    let round_tripped: TaskState = serde_json::from_str(&serialized)
        .expect("new-shape row round-trips through the same decoder");
    assert_eq!(&round_tripped, state);
}
