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
    let snapshot_id = RunSnapshotId::parse(format!("sha256:{}", "a".repeat(64))).unwrap();
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
    .with_run_snapshot(&snapshot_id)
}

fn snapshot_material(phase: RunSnapshotPhase) -> RunSnapshotMaterial {
    let work_unit_id = (!matches!(&phase, RunSnapshotPhase::Planning)).then(|| "unit-1".into());
    RunSnapshotMaterial {
        task_id: "task-1".into(),
        task_revision: 4,
        phase,
        work_unit_id,
        provider: ProviderSnapshotRef {
            kind: ProviderRouteKind::HostProvider,
            settings_revision: 7,
            provider_id: "deepseek".into(),
            model_id: "deepseek-chat".into(),
            base_url: Some("https://api.deepseek.com".into()),
            protocol: Some("openai-compatible".into()),
            capabilities: vec!["tools".into(), "streaming".into()],
        },
        prompt: PromptSnapshotRef {
            revision: "prompt-3".into(),
            mode: PromptSnapshotMode::Append,
            content_sha256: "sha256:prompt".into(),
            resolved_system_prompt: "system prompt".into(),
        },
        workspace: WorkspaceSnapshotRef {
            canonical_root: "D:/project/r-code".into(),
            workspace_identity: "repo:one".into(),
            baseline_sha256: "sha256:baseline".into(),
        },
        permissions: PermissionSnapshotRef {
            revision: "permission-2".into(),
            profile_id: "read-only".into(),
            capabilities: vec!["host.fs.read".into(), "host.fs.search".into()],
        },
        harness_package: PackageRef {
            id: r_code_harness_protocol::HarnessId::new("native"),
            version: semver::Version::new(1, 0, 0),
            content_digest: "sha256:package".into(),
        },
        tool_catalog_sha256: "sha256:tools".into(),
        inference: Some(serde_json::json!({"temperature": 0, "max_tokens": 4096})),
    }
}

fn snapshot_id(material: RunSnapshotMaterial) -> String {
    RunSnapshot::new(material)
        .expect("valid snapshot")
        .id()
        .to_string()
}

#[test]
fn run_snapshot_identity_is_canonical_and_covers_every_material_input() {
    let base = snapshot_material(RunSnapshotPhase::Planning);
    let expected = snapshot_id(base.clone());
    assert!(expected.starts_with("sha256:"));
    assert_eq!(expected.len(), "sha256:".len() + 64);
    assert_eq!(expected, snapshot_id(base.clone()));

    let mut reordered = base.clone();
    reordered.provider.capabilities = vec!["streaming".into(), "tools".into(), "tools".into()];
    reordered.permissions.capabilities = vec![
        "host.fs.search".into(),
        "host.fs.read".into(),
        "host.fs.read".into(),
    ];
    assert_eq!(expected, snapshot_id(reordered));

    macro_rules! assert_material_change {
        ($mutation:expr) => {{
            let mut changed = base.clone();
            $mutation(&mut changed);
            assert_ne!(
                expected,
                snapshot_id(changed),
                "material mutation did not change identity: {}",
                stringify!($mutation)
            );
        }};
    }

    assert_material_change!(|value: &mut RunSnapshotMaterial| value.task_id = "task-2".into());
    assert_material_change!(|value: &mut RunSnapshotMaterial| value.task_revision += 1);
    assert_material_change!(|value: &mut RunSnapshotMaterial| {
        value.phase = RunSnapshotPhase::Execution {
            approval: PlanApprovalRef {
                approval_id: "approval-1".into(),
                plan_revision: PlanRevisionRef("sha256:plan".into()),
            },
        };
        value.work_unit_id = Some("unit-1".into());
    });
    assert_material_change!(
        |value: &mut RunSnapshotMaterial| value.provider.kind = ProviderRouteKind::HarnessManaged
    );
    assert_material_change!(
        |value: &mut RunSnapshotMaterial| value.provider.settings_revision += 1
    );
    assert_material_change!(
        |value: &mut RunSnapshotMaterial| value.provider.provider_id = "openai".into()
    );
    assert_material_change!(
        |value: &mut RunSnapshotMaterial| value.provider.model_id = "gpt".into()
    );
    assert_material_change!(|value: &mut RunSnapshotMaterial| value.provider.base_url = None);
    assert_material_change!(|value: &mut RunSnapshotMaterial| value.provider.protocol = None);
    assert_material_change!(|value: &mut RunSnapshotMaterial| value
        .provider
        .capabilities
        .push("vision".into()));
    assert_material_change!(
        |value: &mut RunSnapshotMaterial| value.prompt.revision = "prompt-4".into()
    );
    assert_material_change!(
        |value: &mut RunSnapshotMaterial| value.prompt.mode = PromptSnapshotMode::Replace
    );
    assert_material_change!(
        |value: &mut RunSnapshotMaterial| value.prompt.content_sha256 =
            "sha256:other-prompt".into()
    );
    assert_material_change!(|value: &mut RunSnapshotMaterial| value
        .prompt
        .resolved_system_prompt =
        "other prompt".into());
    assert_material_change!(
        |value: &mut RunSnapshotMaterial| value.workspace.canonical_root = "D:/other".into()
    );
    assert_material_change!(
        |value: &mut RunSnapshotMaterial| value.workspace.workspace_identity = "repo:two".into()
    );
    assert_material_change!(
        |value: &mut RunSnapshotMaterial| value.workspace.baseline_sha256 =
            "sha256:other-baseline".into()
    );
    assert_material_change!(
        |value: &mut RunSnapshotMaterial| value.permissions.revision = "permission-3".into()
    );
    assert_material_change!(
        |value: &mut RunSnapshotMaterial| value.permissions.profile_id = "workspace-write".into()
    );
    assert_material_change!(|value: &mut RunSnapshotMaterial| value
        .permissions
        .capabilities
        .push("host.fs.write".into()));
    assert_material_change!(|value: &mut RunSnapshotMaterial| value.harness_package.id =
        r_code_harness_protocol::HarnessId::new("other"));
    assert_material_change!(
        |value: &mut RunSnapshotMaterial| value.harness_package.version =
            semver::Version::new(1, 0, 1)
    );
    assert_material_change!(|value: &mut RunSnapshotMaterial| value
        .harness_package
        .content_digest =
        "sha256:other-package".into());
    assert_material_change!(
        |value: &mut RunSnapshotMaterial| value.tool_catalog_sha256 = "sha256:other-tools".into()
    );
    assert_material_change!(|value: &mut RunSnapshotMaterial| value.inference =
        Some(serde_json::json!({"temperature": 1})));
}

#[test]
fn planning_is_planless_while_execution_and_repair_bind_exact_approval() {
    let planning = RunSnapshot::new(snapshot_material(RunSnapshotPhase::Planning)).unwrap();
    assert!(planning.phase().approval().is_none());
    assert!(planning.phase().plan_revision().is_none());
    assert!(planning.material().work_unit_id.is_none());

    let malformed_execution = serde_json::json!({"kind": "execution"});
    assert!(serde_json::from_value::<RunSnapshotPhase>(malformed_execution).is_err());

    for phase in [
        RunSnapshotPhase::Execution {
            approval: PlanApprovalRef {
                approval_id: "approval-execution".into(),
                plan_revision: PlanRevisionRef("sha256:plan-execution".into()),
            },
        },
        RunSnapshotPhase::Repair {
            approval: PlanApprovalRef {
                approval_id: "approval-repair".into(),
                plan_revision: PlanRevisionRef("sha256:plan-repair".into()),
            },
        },
    ] {
        let snapshot = RunSnapshot::new(snapshot_material(phase)).unwrap();
        let approval = snapshot.phase().approval().expect("exact approval");
        assert_eq!(snapshot.material().work_unit_id.as_deref(), Some("unit-1"));
        assert!(!approval.approval_id.is_empty());
        assert_eq!(
            snapshot.phase().plan_revision(),
            Some(&approval.plan_revision)
        );
    }

    let approval = PlanApprovalRef {
        approval_id: "approval-work-unit".into(),
        plan_revision: PlanRevisionRef("sha256:plan-work-unit".into()),
    };
    let mut missing_unit = snapshot_material(RunSnapshotPhase::Execution {
        approval: approval.clone(),
    });
    missing_unit.work_unit_id = None;
    assert!(RunSnapshot::new(missing_unit).is_err());
    let first = RunSnapshot::new(snapshot_material(RunSnapshotPhase::Execution {
        approval: approval.clone(),
    }))
    .unwrap();
    let mut changed_unit = snapshot_material(RunSnapshotPhase::Execution { approval });
    changed_unit.work_unit_id = Some("unit-2".into());
    let second = RunSnapshot::new(changed_unit).unwrap();
    assert_ne!(first.id(), second.id());

    for invalid in [
        PlanApprovalRef {
            approval_id: String::new(),
            plan_revision: PlanRevisionRef("sha256:plan".into()),
        },
        PlanApprovalRef {
            approval_id: "approval-1".into(),
            plan_revision: PlanRevisionRef(String::new()),
        },
    ] {
        assert!(
            RunSnapshot::new(snapshot_material(RunSnapshotPhase::Execution {
                approval: invalid,
            }))
            .is_err(),
            "empty approval identities cannot authorize execution"
        );
    }
}

#[test]
fn snapshot_rejects_credential_material_in_inference_metadata() {
    for inference in [
        serde_json::json!({"api_key": "sk-top-level"}),
        serde_json::json!({"provider": {"client-secret": "nested-secret"}}),
        serde_json::json!({"fallbacks": [{"authorization": "Bearer secret"}]}),
    ] {
        let mut material = snapshot_material(RunSnapshotPhase::Planning);
        material.inference = Some(inference);
        assert!(
            RunSnapshot::new(material).is_err(),
            "nested or normalized credential keys must not provide a smuggling path"
        );
    }

    let safe = snapshot_material(RunSnapshotPhase::Planning);
    assert_eq!(safe.inference.as_ref().unwrap()["max_tokens"], 4096);
    assert!(RunSnapshot::new(safe).is_ok());
}

#[test]
fn attempt_wire_format_reads_legacy_rows_and_binds_new_attempts() {
    let legacy_json = serde_json::json!({
        "attempt_id": "attempt-legacy",
        "task_id": "task-1",
        "branch_id": "branch-1",
        "package": {
            "id": "example.harness",
            "version": "1.0.0",
            "contentDigest": "sha256:aaa"
        },
        "contract_revision": 4,
        "config_hash": "legacy-config-hash",
        "workspace_identity": "ws-1",
        "run_id": "run-legacy"
    });
    let legacy: Attempt = serde_json::from_value(legacy_json).expect("legacy attempt");
    assert!(legacy.run_snapshot_id().is_none());
    assert!(serde_json::to_value(&legacy)
        .unwrap()
        .get("snapshot_id")
        .is_none());

    let snapshot = RunSnapshot::new(snapshot_material(RunSnapshotPhase::Planning)).unwrap();
    let current = Attempt::for_run_snapshot(
        "attempt-current",
        "task-1",
        "branch-1",
        snapshot.material().harness_package.clone(),
        4,
        "ws-1",
        "run-current",
        snapshot.id(),
    );
    assert_eq!(current.run_snapshot_id().as_ref(), Some(snapshot.id()));
    let current_json = serde_json::to_value(&current).unwrap();
    assert_eq!(current_json["snapshot_id"], snapshot.id().as_str());
    assert_eq!(
        current_json["config_hash"],
        format!("run-snapshot:{}", snapshot.id())
    );
    let round_trip: Attempt = serde_json::from_value(current_json.clone()).unwrap();
    assert_eq!(round_trip, current);

    let mut conflicting = current_json;
    conflicting["config_hash"] = serde_json::Value::String("legacy-config-hash".into());
    assert!(serde_json::from_value::<Attempt>(conflicting).is_err());
}

#[test]
fn starting_a_new_attempt_requires_a_snapshot_binding() {
    let mut unbound = attempt(4);
    unbound.config_hash = "legacy-config-hash".into();
    assert!(unbound.run_snapshot_id().is_none());

    let mut state = TaskState::new(contract(TaskKind::Implementation));
    assert!(
        state.start_attempt(&unbound).is_err(),
        "legacy rows may remain readable, but a newly started attempt must bind a durable snapshot"
    );
}

fn host_evidence(check_id: &str, digest: &str, passed: bool) -> EvidenceRecord {
    EvidenceRecord {
        evidence_id: format!("ev-{check_id}"),
        task_id: "task-1".into(),
        check_id: check_id.into(),
        definition_identity: format!("definition:{check_id}"),
        candidate_digest: digest.into(),
        environment: "rustc 1.88".into(),
        environment_fingerprint: "environment:rustc-1.88".into(),
        passed,
        host_output: None,
        recorded_by: Provenance::Host,
    }
}

fn plugin_evidence(check_id: &str, digest: &str) -> EvidenceRecord {
    EvidenceRecord {
        evidence_id: format!("plug-{check_id}"),
        task_id: "task-1".into(),
        check_id: check_id.into(),
        definition_identity: format!("definition:{check_id}"),
        candidate_digest: digest.into(),
        environment: "plugin-claimed".into(),
        environment_fingerprint: "environment:plugin-claimed".into(),
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
fn implementation_proposals_always_defer_to_host_verification() {
    let mut state = started_task(TaskKind::Implementation);
    // E05: candidate digests are seeded per unit.
    state
        .set_unit_candidate_digest("u1", Some("digest-a".into()))
        .unwrap();

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
    assert!(matches!(
        decision,
        ProposalDecision::Repair { feedback }
            if feedback.contains("host verification")
    ));

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
    assert!(matches!(decision, ProposalDecision::Repair { .. }));
}

#[test]
fn plugin_candidate_digest_cannot_bypass_host_verification() {
    let mut state = started_task(TaskKind::Implementation);
    state
        .set_unit_candidate_digest("u1", Some("digest-a".into()))
        .unwrap();
    state
        .record_evidence(host_evidence("check:cargo-test", "digest-a", true))
        .unwrap();

    // A proposal naming a different candidate is never trusted.
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
    assert!(matches!(decision, ProposalDecision::Repair { .. }));
    // E05: no per-unit record was verified — plugins mint no verification
    // state.
    assert!(!state
        .unit_records
        .values()
        .any(|record| matches!(record.verification, ValidationOutcome::Verified { .. })));

    // Even a matching digest and preloaded evidence cannot let the plugin
    // perform the host-only Verifying -> ReviewReady transition.
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
    assert!(matches!(decision, ProposalDecision::Repair { .. }));
    assert!(!state
        .unit_records
        .values()
        .any(|record| matches!(record.verification, ValidationOutcome::Verified { .. })));
    assert!(matches!(state.execution, TaskExecution::Running { .. }));
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
            read_paths: vec![],
            write_paths: vec![],
            repo_exclusive: false,
            ephemeral_roots: vec![],
            effect_class: WorkUnitEffectClass::ReadOnly,
            network_ceiling: NetworkCeiling::Offline,
            status: WorkUnitStatus::InProgress,
        },
        WorkUnit {
            id: "u2".into(),
            description: "second".into(),
            dependencies: vec!["u1".into()],
            acceptance: vec![],
            read_paths: vec![],
            write_paths: vec![],
            repo_exclusive: false,
            ephemeral_roots: vec![],
            effect_class: WorkUnitEffectClass::ReadOnly,
            network_ceiling: NetworkCeiling::Offline,
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

    // With evidence present the same update succeeds. E05: the unit's own
    // candidate digest gates its acceptance evidence.
    state
        .set_unit_candidate_digest("u1", Some("digest-a".into()))
        .unwrap();
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
