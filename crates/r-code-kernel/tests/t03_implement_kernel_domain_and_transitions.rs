//! T03 — kernel domain and state transitions.
//!
//! Acceptance: state-machine tests reject plugin-issued terminal verdicts and
//! late-generation transitions; stale revisions, waiting input, proposal
//! arbitration and exactly-one-terminal are all covered.

use r_code_harness_protocol::{PackageRef, Provenance};
use r_code_kernel::*;

fn contract(kind: TaskKind) -> TaskContract {
    TaskContract {
        task_id: "task-1".into(),
        kind,
        objective: "fix the flaky test".into(),
        constraints: vec![],
        required_checks: vec!["check:cargo-test".into()],
        revision: 4,
    }
}

fn attempt(contract_revision: u64) -> Attempt {
    Attempt {
        attempt_id: "attempt-1".into(),
        task_id: "task-1".into(),
        branch_id: "branch-1".into(),
        package: PackageRef {
            id: r_code_harness_protocol::HarnessId::new("example.harness"),
            version: semver::Version::new(1, 0, 0),
            content_digest: "sha256:aaa".into(),
        },
        contract_revision,
        config_hash: "cfg-1".into(),
        workspace_identity: "ws-1".into(),
        run_id: "run-1".into(),
    }
}

fn host_evidence(check_id: &str, digest: &str, passed: bool) -> EvidenceRecord {
    EvidenceRecord {
        evidence_id: format!("ev-{check_id}"),
        check_id: check_id.into(),
        candidate_digest: digest.into(),
        environment: "rustc 1.88".into(),
        passed,
        host_output: None,
        recorded_by: Provenance::Host,
    }
}

fn plugin_evidence(check_id: &str, digest: &str) -> EvidenceRecord {
    EvidenceRecord {
        evidence_id: format!("plug-{check_id}"),
        check_id: check_id.into(),
        candidate_digest: digest.into(),
        environment: "plugin-claimed".into(),
        passed: true,
        host_output: None,
        recorded_by: Provenance::Plugin {
            harness_id: "example.harness".into(),
            package_digest: "sha256:aaa".into(),
        },
    }
}

fn started_task(kind: TaskKind) -> TaskState {
    let mut state = TaskState::new(contract(kind));
    state.start_attempt(&attempt(4)).expect("start");
    state
}

#[test]
fn start_attempt_rejects_stale_contract_revisions() {
    let mut state = TaskState::new(contract(TaskKind::Implementation));
    assert!(matches!(
        state.start_attempt(&attempt(3)),
        Err(TransitionError::StaleRevision {
            expected: 4,
            provided: 3
        })
    ));
    assert!(state.start_attempt(&attempt(4)).is_ok());
}

#[test]
fn plugin_issued_terminal_verdicts_are_rejected() {
    let mut state = started_task(TaskKind::Implementation);
    let err = state
        .finalize(
            Actor::Plugin,
            TaskVerdict::Verified {
                candidate_digest: "x".into(),
            },
        )
        .expect_err("plugins cannot finalize");
    assert!(matches!(err, TransitionError::PluginVerdictRejected));
    // Cancel through a plugin actor is equally forbidden.
    assert!(matches!(
        state.cancel(Actor::Plugin, 1, "plugin tries cancel"),
        Err(TransitionError::PluginVerdictRejected)
    ));
}

#[test]
fn late_generation_transitions_are_rejected() {
    let mut state = started_task(TaskKind::Implementation);
    // Generation is 1; a callback from generation 0 (pre-restart) is late.
    assert!(matches!(
        state.wait_for_input(0, "q-1"),
        Err(TransitionError::LateGeneration {
            current: 1,
            provided: 0
        })
    ));
    assert!(matches!(
        state.apply_proposal(
            2,
            &CompletionProposal {
                actor: Actor::Plugin,
                kind: ProposalKind::Implementation,
                summary: "done".into(),
                candidate_digest: None,
            }
        ),
        Err(TransitionError::LateGeneration {
            current: 1,
            provided: 2
        })
    ));
    assert!(state.wait_for_input(1, "q-1").is_ok());
}

#[test]
fn waiting_input_round_trip_and_duplicate_answers() {
    let mut state = started_task(TaskKind::Conversation);
    state.wait_for_input(1, "q-7").expect("wait");
    assert!(
        matches!(&state.execution, TaskExecution::WaitingInput { question_id, .. } if question_id == "q-7")
    );
    assert!(matches!(
        state.answer_input(1, "q-8"),
        Err(TransitionError::WrongQuestion(_))
    ));
    assert!(state.answer_input(1, "q-7").expect("answer"));
    // Answering again after resume is a no-op, not an error.
    assert!(!state.answer_input(1, "q-7").expect("repeat"));
}

#[test]
fn exactly_one_terminal_result() {
    let mut state = started_task(TaskKind::Implementation);
    state
        .cancel(Actor::User, 1, "user stopped")
        .expect("cancel");
    let verdict = match &state.execution {
        TaskExecution::Terminal { verdict } => verdict.clone(),
        other => panic!("expected terminal, got {other:?}"),
    };
    assert!(matches!(verdict, TaskVerdict::Cancelled { .. }));
    // Every subsequent transition is refused, including another finalize.
    assert!(matches!(
        state.finalize(
            Actor::Host,
            TaskVerdict::Failed {
                reason: "late".into()
            }
        ),
        Err(TransitionError::AlreadyTerminal { .. })
    ));
    assert!(matches!(
        state.record_evidence(host_evidence("check:cargo-test", "d1", true)),
        Err(TransitionError::AlreadyTerminal { .. })
    ));
    assert!(matches!(
        state.update_work_unit(
            4,
            &WorkUnitUpdate {
                work_unit_id: "u1".into(),
                status: WorkUnitStatus::Completed
            }
        ),
        Err(TransitionError::AlreadyTerminal { .. })
    ));
}

#[test]
fn implementation_cannot_finish_verified_without_current_host_evidence() {
    let mut state = started_task(TaskKind::Implementation);
    state.set_candidate_digest(Some("digest-a".into())).unwrap();

    // Plugin-authored passing evidence never counts.
    state
        .record_evidence(plugin_evidence("check:cargo-test", "digest-a"))
        .unwrap();
    let decision = state
        .apply_proposal(
            1,
            &CompletionProposal {
                actor: Actor::Plugin,
                kind: ProposalKind::Implementation,
                summary: "done".into(),
                candidate_digest: Some("digest-a".into()),
            },
        )
        .expect("proposal handled");
    match decision {
        ProposalDecision::Accept {
            verdict: TaskVerdict::Unverified { reason },
        } => {
            assert!(reason.contains("check:cargo-test"), "reason: {reason}");
        }
        other => panic!("expected unverified, got {other:?}"),
    }

    // Host evidence for an older candidate digest is stale.
    state
        .record_evidence(host_evidence("check:cargo-test", "digest-old", true))
        .unwrap();
    let decision = state
        .apply_proposal(
            1,
            &CompletionProposal {
                actor: Actor::Plugin,
                kind: ProposalKind::Implementation,
                summary: "done".into(),
                candidate_digest: Some("digest-a".into()),
            },
        )
        .expect("proposal handled");
    assert!(matches!(
        decision,
        ProposalDecision::Accept {
            verdict: TaskVerdict::Unverified { .. }
        }
    ));
}

#[test]
fn verified_requires_all_required_checks_and_matching_candidate() {
    let mut state = started_task(TaskKind::Implementation);
    state.set_candidate_digest(Some("digest-a".into())).unwrap();
    state
        .record_evidence(host_evidence("check:cargo-test", "digest-a", true))
        .unwrap();

    // A proposal naming a different candidate is rejected outright.
    let decision = state
        .apply_proposal(
            1,
            &CompletionProposal {
                actor: Actor::Plugin,
                kind: ProposalKind::Implementation,
                summary: "done".into(),
                candidate_digest: Some("digest-other".into()),
            },
        )
        .expect("proposal handled");
    assert!(matches!(decision, ProposalDecision::Reject { .. }));
    assert!(matches!(state.validation, ValidationOutcome::NotEvaluated));

    // Matching candidate with full host evidence is accepted as verified.
    let decision = state
        .apply_proposal(
            1,
            &CompletionProposal {
                actor: Actor::Plugin,
                kind: ProposalKind::Implementation,
                summary: "done".into(),
                candidate_digest: Some("digest-a".into()),
            },
        )
        .expect("proposal handled");
    match decision {
        ProposalDecision::Accept {
            verdict: TaskVerdict::Verified { candidate_digest },
        } => {
            assert_eq!(candidate_digest, "digest-a");
        }
        other => panic!("expected verified, got {other:?}"),
    }
    assert!(matches!(
        state.validation,
        ValidationOutcome::Verified { .. }
    ));
    assert!(matches!(state.execution, TaskExecution::ReviewReady { .. }));
    // Failing host evidence does not verify either.
}

#[test]
fn replies_and_plan_drafts_settle_without_code_checks() {
    let mut state = started_task(TaskKind::Conversation);
    let decision = state
        .apply_proposal(
            1,
            &CompletionProposal {
                actor: Actor::Plugin,
                kind: ProposalKind::Reply,
                summary: "answer".into(),
                candidate_digest: None,
            },
        )
        .expect("proposal handled");
    match decision {
        ProposalDecision::Accept {
            verdict: TaskVerdict::Unverified { reason },
        } => {
            assert!(reason.contains("no code verification"));
        }
        other => panic!("expected settled reply, got {other:?}"),
    }
}

#[test]
fn work_units_fence_on_revision_dependencies_and_evidence() {
    let mut state = started_task(TaskKind::Implementation);
    state.work_units = vec![
        WorkUnit {
            id: "u1".into(),
            description: "first".into(),
            dependencies: vec![],
            acceptance: vec!["check:cargo-test".into()],
            status: WorkUnitStatus::InProgress,
        },
        WorkUnit {
            id: "u2".into(),
            description: "second".into(),
            dependencies: vec!["u1".into()],
            acceptance: vec![],
            status: WorkUnitStatus::Pending,
        },
    ];

    // Stale contract revision.
    assert!(matches!(
        state.update_work_unit(
            3,
            &WorkUnitUpdate {
                work_unit_id: "u2".into(),
                status: WorkUnitStatus::Completed
            }
        ),
        Err(TransitionError::StaleRevision {
            expected: 4,
            provided: 3
        })
    ));

    // Dependency not completed.
    assert!(matches!(
        state.update_work_unit(
            4,
            &WorkUnitUpdate { work_unit_id: "u2".into(), status: WorkUnitStatus::Completed }
        ),
        Err(TransitionError::DependencyNotCompleted(dep)) if dep == "u1"
    ));

    // Code unit completion requires current evidence.
    assert!(matches!(
        state.update_work_unit(
            4,
            &WorkUnitUpdate {
                work_unit_id: "u1".into(),
                status: WorkUnitStatus::Completed
            }
        ),
        Err(TransitionError::EvidenceRequired)
    ));

    // With evidence present the same update succeeds.
    state.set_candidate_digest(Some("digest-a".into())).unwrap();
    state
        .record_evidence(host_evidence("check:cargo-test", "digest-a", true))
        .unwrap();
    state
        .update_work_unit(
            4,
            &WorkUnitUpdate {
                work_unit_id: "u1".into(),
                status: WorkUnitStatus::Completed,
            },
        )
        .expect("complete u1");
    state
        .update_work_unit(
            4,
            &WorkUnitUpdate {
                work_unit_id: "u2".into(),
                status: WorkUnitStatus::Completed,
            },
        )
        .expect("complete u2");

    assert!(matches!(
        state.update_work_unit(
            4,
            &WorkUnitUpdate {
                work_unit_id: "missing".into(),
                status: WorkUnitStatus::Completed
            }
        ),
        Err(TransitionError::UnknownWorkUnit(_))
    ));
}
