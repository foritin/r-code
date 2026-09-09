//! T18 — verification profiles and evidence store.
//!
//! Tests cover absent checks, changed check definitions, stale evidence and
//! refusal of plugin-authored pass assertions.

use r_code_harness_protocol::{ArtifactRef, HarnessId, PackageRef, Provenance};
use r_code_kernel::task::*;
use r_code_kernel::verification::*;

fn check(check_id: &str) -> CheckDefinition {
    CheckDefinition {
        check_id: check_id.into(),
        entrypoint: CheckEntrypoint::Command {
            program: "cargo".into(),
            argv: vec!["test".into(), "--locked".into()],
        },
        control_files: vec![ControlFile {
            path: "acceptance/rust-toolchain.toml".into(),
            sha256: "a".repeat(64),
        }],
        source_roots: vec!["src".into(), "crates".into()],
        dependency_locks: vec!["Cargo.lock".into()],
        toolchain: "rustc 1.88".into(),
        declared_external_inputs: vec![],
        entrypoint_bytes: Some(ArtifactRef {
            schema: 1,
            blob_id: "blob:sha256:entry".into(),
            bytes: 128,
            sha256: "entry".into(),
            media_type: None,
        }),
    }
}

fn host_evidence(check_id: &str, digest: &str, env: &str, passed: bool) -> EvidenceRecord {
    EvidenceRecord {
        evidence_id: format!("ev-{check_id}-{digest}"),
        check_id: check_id.into(),
        candidate_digest: digest.into(),
        environment: env.into(),
        passed,
        host_output: None,
        recorded_by: Provenance::Host,
    }
}

#[test]
fn definitions_require_direct_entrypoints_and_pinned_controls() {
    assert!(check("check:cargo-test").validate().is_ok());

    let mut empty_entry = check("check:bad");
    empty_entry.entrypoint = CheckEntrypoint::Command {
        program: String::new(),
        argv: vec![],
    };
    assert!(matches!(
        empty_entry.validate(),
        Err(CheckDefinitionError::NoEntrypoint(_))
    ));

    let mut unpinned = check("check:bad");
    unpinned.control_files = vec![ControlFile {
        path: "x".into(),
        sha256: "short".into(),
    }];
    assert!(matches!(
        unpinned.validate(),
        Err(CheckDefinitionError::UnpinnedControlFile(_))
    ));

    // Candidate npm-script aliases can never stand in for a frozen check.
    assert!(!check("check:any").is_aliasable_by_candidate_scripts());
}

#[test]
fn changed_definitions_invalidate_prior_evidence() {
    let base = check("check:cargo-test");
    let identity = base.identity();

    // An identical rebuild keeps the identity.
    assert_eq!(check("check:cargo-test").identity(), identity);

    // Any weakening/strengthening change (toolchain, argv, controls) is a
    // new identity → old evidence no longer binds.
    let mut changed_toolchain = check("check:cargo-test");
    changed_toolchain.toolchain = "rustc 1.89".into();
    assert_ne!(changed_toolchain.identity(), identity);

    let mut changed_argv = check("check:cargo-test");
    let CheckEntrypoint::Command { argv, .. } = &mut changed_argv.entrypoint;
    argv.push("--release".into());
    assert_ne!(changed_argv.identity(), identity);

    let mut changed_control = check("check:cargo-test");
    changed_control.control_files[0].sha256 = "b".repeat(64);
    assert_ne!(changed_control.identity(), identity);

    // Evidence validity is keyed to the definition identity via the
    // acceptance profile digest.
    let profile = build_acceptance_profile(1, std::slice::from_ref(&base)).expect("profile");
    let mut other = build_acceptance_profile(2, &[changed_toolchain]).expect("profile");
    other.revision = 1;
    assert!(EvidenceValidity::is_weaker_than(&other, &profile));
}

#[test]
fn stale_and_foreign_evidence_is_rejected() {
    let definition = check("check:cargo-test");
    let passing = host_evidence("check:cargo-test", "digest-a", "rustc 1.88", true);
    assert!(EvidenceValidity::is_valid(
        &passing,
        &definition,
        "digest-a",
        "rustc 1.88"
    ));

    // Different candidate: foreign.
    assert!(!EvidenceValidity::is_valid(
        &passing,
        &definition,
        "digest-b",
        "rustc 1.88"
    ));
    // Different environment: stale.
    assert!(!EvidenceValidity::is_valid(
        &passing,
        &definition,
        "digest-a",
        "rustc 1.87"
    ));
    // Failing evidence never counts.
    let failing = host_evidence("check:cargo-test", "digest-a", "rustc 1.88", false);
    assert!(!EvidenceValidity::is_valid(
        &failing,
        &definition,
        "digest-a",
        "rustc 1.88"
    ));
    // Different check: foreign.
    let other_check = host_evidence("check:fmt", "digest-a", "rustc 1.88", true);
    assert!(!EvidenceValidity::is_valid(
        &other_check,
        &definition,
        "digest-a",
        "rustc 1.88"
    ));

    // Plugin-authored pass assertions never count, regardless of shape.
    let mut plugin_claimed = host_evidence("check:cargo-test", "digest-a", "rustc 1.88", true);
    plugin_claimed.recorded_by = Provenance::Plugin {
        harness_id: "sneaky.harness".into(),
        package_digest: "sha:1234".into(),
    };
    assert!(!EvidenceValidity::is_valid(
        &plugin_claimed,
        &definition,
        "digest-a",
        "rustc 1.88"
    ));
}

#[test]
fn absent_checks_yield_unverified_not_verified() {
    // A task whose contract requires two checks but has evidence for one
    // can only finish unverified (the kernel's arbitration, T03/T20).
    let mut state = TaskState::new(TaskContract {
        task_id: "task-1".into(),
        kind: TaskKind::Implementation,
        objective: "o".into(),
        constraints: vec![],
        required_checks: vec!["check:cargo-test".into(), "check:clippy".into()],
        revision: 1,
    });
    state
        .start_attempt(&Attempt {
            attempt_id: "a1".into(),
            task_id: "task-1".into(),
            branch_id: "b".into(),
            package: PackageRef {
                id: HarnessId::new("h"),
                version: semver::Version::new(1, 0, 0),
                content_digest: "s".into(),
            },
            contract_revision: 1,
            config_hash: "c".into(),
            workspace_identity: "w".into(),
            run_id: "r".into(),
        })
        .expect("start");
    state.set_candidate_digest(Some("digest-a".into())).unwrap();
    // Evidence present only for one of the two required checks.
    state
        .record_evidence(host_evidence(
            "check:cargo-test",
            "digest-a",
            "rustc 1.88",
            true,
        ))
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
        .expect("proposal");
    match decision {
        ProposalDecision::Accept {
            verdict: TaskVerdict::Unverified { reason },
        } => {
            assert!(
                reason.contains("check:clippy"),
                "missing check named: {reason}"
            );
        }
        other => panic!("expected unverified, got {other:?}"),
    }
}

#[test]
fn acceptance_profiles_are_immutable_and_user_authorized_on_weakening() {
    let definitions = [check("check:cargo-test"), check("check:clippy")];
    let profile = build_acceptance_profile(1, &definitions).expect("profile");

    // Dropping a check, lowering revision or changing definitions is weaker.
    let mut dropped = definitions.to_vec();
    dropped.pop();
    let dropped_profile = build_acceptance_profile(1, &dropped).expect("dropped");
    assert!(EvidenceValidity::is_weaker_than(&dropped_profile, &profile));

    let stale_revision = build_acceptance_profile(0, &definitions).expect("stale");
    assert!(EvidenceValidity::is_weaker_than(&stale_revision, &profile));

    let same = build_acceptance_profile(2, &definitions).expect("same content, new revision");
    assert!(!EvidenceValidity::is_weaker_than(&same, &profile));
}

#[test]
fn store_round_trips_definitions_and_candidate_evidence() {
    let temp = tempfile::tempdir().expect("tempdir");
    let store = r_code_store::v2::V2Store::open(&temp.path().join("tasks.sqlite3")).expect("open");

    let definition = check("check:cargo-test");
    store
        .save_check_definition(&definition)
        .expect("save definition");

    store
        .save_evidence(&host_evidence(
            "check:cargo-test",
            "digest-a",
            "rustc 1.88",
            true,
        ))
        .expect("save evidence");
    store
        .save_evidence(&host_evidence(
            "check:clippy",
            "digest-a",
            "rustc 1.88",
            true,
        ))
        .expect("save evidence 2");

    let records = store.evidence_for_candidate("digest-a").expect("query");
    assert_eq!(records.len(), 2);
    assert!(records
        .iter()
        .all(|record| matches!(record.recorded_by, Provenance::Host)));

    // Other candidates see nothing.
    assert!(store
        .evidence_for_candidate("digest-b")
        .expect("query")
        .is_empty());
}
