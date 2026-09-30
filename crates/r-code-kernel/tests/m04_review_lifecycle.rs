use r_code_kernel::task::{
    Actor, NetworkCeiling, ReviewDisposition, TaskContract, TaskExecution, TaskKind, TaskState,
    TaskVerdict, TransitionError, UnitRecord, UnitSettlement, ValidationOutcome, WorkUnit,
    WorkUnitEffectClass, WorkUnitStatus,
};
use std::collections::BTreeMap;

fn review_ready() -> TaskState {
    let mut state = TaskState::new(TaskContract {
        task_id: "task-1".into(),
        kind: TaskKind::Implementation,
        objective: "review".into(),
        constraints: vec![],
        required_checks: vec![],
        revision: 1,
    });
    state.work_units = vec![WorkUnit {
        id: "unit-1".into(),
        description: "unit".into(),
        dependencies: vec![],
        acceptance: vec![],
        read_paths: vec![],
        write_paths: vec!["src".into()],
        repo_exclusive: false,
        ephemeral_roots: vec![],
        effect_class: WorkUnitEffectClass::ReadOnly,
        network_ceiling: NetworkCeiling::Offline,
        status: WorkUnitStatus::Completed,
    }];
    // E05: the candidate digest and verification outcome live on the unit's
    // per-unit record; the task-level digest is derived from it.
    let mut unit_records = BTreeMap::new();
    unit_records.insert(
        "unit-1".to_string(),
        UnitRecord {
            attempt_id: Some("attempt-1".into()),
            candidate_digest: Some("candidate-1".into()),
            verification: ValidationOutcome::Verified {
                candidate_digest: "candidate-1".into(),
            },
            settlement: UnitSettlement::Completed,
        },
    );
    state.unit_records = unit_records;
    state.review = ReviewDisposition::Pending;
    state.execution = TaskExecution::ReviewReady {
        attempt_id: "attempt-1".into(),
    };
    assert_eq!(
        state.task_candidate_digest().as_deref(),
        Some("candidate-1")
    );
    state
}

#[test]
fn verified_accept_requires_user_or_host_exact_attempt_candidate_and_actor() {
    for (attempt, candidate, actor_id) in [
        ("wrong", "candidate-1", "user"),
        ("attempt-1", "wrong", "user"),
        ("attempt-1", "candidate-1", ""),
    ] {
        let mut state = review_ready();
        assert!(state
            .accept_verified(Actor::User, attempt, candidate, actor_id)
            .is_err());
    }
    let mut plugin = review_ready();
    assert!(matches!(
        plugin.accept_verified(Actor::Plugin, "attempt-1", "candidate-1", "plugin"),
        Err(TransitionError::HostTransitionRequired { .. })
    ));

    let mut accepted = review_ready();
    accepted
        .accept_verified(
            Actor::User,
            "attempt-1",
            "candidate-1",
            "authenticated-user",
        )
        .unwrap();
    assert_eq!(accepted.review, ReviewDisposition::Accepted);
    assert!(matches!(
        accepted.execution,
        TaskExecution::Terminal {
            verdict: TaskVerdict::VerifiedAccepted {
                ref candidate_digest,
                ref actor_id
            }
        } if candidate_digest == "candidate-1" && actor_id == "authenticated-user"
    ));
    assert!(accepted
        .accept_verified(
            Actor::User,
            "attempt-1",
            "candidate-1",
            "authenticated-user"
        )
        .is_err());
}

#[test]
fn rejection_enters_repair_and_unverified_override_is_repair_only() {
    let mut rejected = review_ready();
    rejected
        .reject_review(
            Actor::User,
            "attempt-1",
            "candidate-1",
            "needs changes".into(),
        )
        .unwrap();
    assert_eq!(rejected.review, ReviewDisposition::Rejected);
    assert!(matches!(
        rejected.execution,
        TaskExecution::RepairRequired { .. }
    ));

    let mut review_ready_override = review_ready();
    review_ready_override
        .unit_record_mut("unit-1")
        .unwrap()
        .verification = ValidationOutcome::Unverified {
        reason: "failed".into(),
    };
    assert!(review_ready_override
        .accept_unverified(
            Actor::User,
            "candidate-1",
            "user",
            "reason",
            &["check:a".into()]
        )
        .is_err());

    rejected.unit_record_mut("unit-1").unwrap().verification =
        ValidationOutcome::CheckUnavailable {
            reason: "missing".into(),
        };
    assert!(matches!(
        rejected.accept_unverified(
            Actor::Plugin,
            "candidate-1",
            "plugin",
            "reason",
            &["check:a".into()]
        ),
        Err(TransitionError::HostTransitionRequired { .. })
    ));
    rejected
        .accept_unverified(
            Actor::User,
            "candidate-1",
            "authenticated-user",
            "accept risk",
            &["check:b".into(), "check:a".into(), "check:a".into()],
        )
        .unwrap();
    assert_eq!(rejected.review, ReviewDisposition::OverrideAccepted);
    assert!(matches!(
        rejected.execution,
        TaskExecution::Terminal {
            verdict: TaskVerdict::UnverifiedAccepted {
                ref checks,
                ref reason,
                ..
            }
        } if checks == &["check:a", "check:b"] && reason == "accept risk"
    ));
}

#[test]
fn new_review_verdicts_and_dispositions_have_stable_json_roundtrips() {
    for verdict in [
        TaskVerdict::VerifiedAccepted {
            candidate_digest: "candidate".into(),
            actor_id: "actor".into(),
        },
        TaskVerdict::UnverifiedAccepted {
            candidate_digest: "candidate".into(),
            actor_id: "actor".into(),
            reason: "reason".into(),
            checks: vec!["check:a".into()],
        },
    ] {
        let json = serde_json::to_string(&verdict).unwrap();
        assert_eq!(serde_json::from_str::<TaskVerdict>(&json).unwrap(), verdict);
    }
    let disposition = ReviewDisposition::OverrideAccepted;
    let json = serde_json::to_string(&disposition).unwrap();
    assert_eq!(
        serde_json::from_str::<ReviewDisposition>(&json).unwrap(),
        disposition
    );
}
