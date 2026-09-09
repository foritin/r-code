//! T20 — completion arbitration.
//!
//! A scripted Harness cannot finish verified after omitting a check, citing
//! another run, or changing files after a passing check; failed checks
//! return repair feedback; unavailable environments block.

use r_code_harness_protocol::Provenance;
use r_code_kernel::completion::*;
use r_code_kernel::task::*;
use r_code_kernel::verification::*;

fn contract(checks: &[&str]) -> TaskContract {
    TaskContract {
        task_id: "task-1".into(),
        kind: TaskKind::Implementation,
        objective: "o".into(),
        constraints: vec![],
        required_checks: checks.iter().map(|check| check.to_string()).collect(),
        revision: 1,
    }
}

fn definition(check_id: &str) -> CheckDefinition {
    CheckDefinition {
        check_id: check_id.into(),
        entrypoint: CheckEntrypoint::Command {
            program: "node".into(),
            argv: vec!["verify.js".into()],
        },
        control_files: vec![],
        source_roots: vec![],
        dependency_locks: vec![],
        toolchain: "node test".into(),
        declared_external_inputs: vec![],
        entrypoint_bytes: None,
    }
}

fn evidence(check_id: &str, digest: &str, passed: bool) -> EvidenceRecord {
    EvidenceRecord {
        evidence_id: format!("ev-{check_id}-{digest}"),
        check_id: check_id.into(),
        candidate_digest: digest.into(),
        environment: "node test".into(),
        passed,
        host_output: None,
        recorded_by: Provenance::Host,
    }
}

fn inputs<'a>(
    contract: &'a TaskContract,
    digest: Option<&'a str>,
    evidence: &'a [EvidenceRecord],
    definitions: &'a [CheckDefinition],
) -> ArbitrationInputs<'a> {
    ArbitrationInputs {
        contract,
        candidate_digest: digest,
        evidence,
        definitions,
        unavailable: Vec::new(),
    }
}

fn proposal(digest: Option<&str>) -> CompletionProposal {
    CompletionProposal {
        actor: Actor::Plugin,
        kind: ProposalKind::Implementation,
        summary: "done".into(),
        candidate_digest: digest.map(str::to_string),
    }
}

#[test]
fn omitting_a_check_cannot_finish_verified() {
    let contract = contract(&["check:a", "check:b"]);
    let definitions = [definition("check:a"), definition("check:b")];
    // Evidence only for check:a — check:b omitted entirely.
    let evidence = [evidence("check:a", "digest-1", true)];
    let verdict = arbitrate(
        &inputs(&contract, Some("digest-1"), &evidence, &definitions),
        &proposal(Some("digest-1")),
    );
    match verdict {
        Arbitration::Final(TaskVerdict::Unverified { reason }) => {
            assert!(reason.contains("check:b"), "{reason}");
        }
        other => panic!("expected unverified, got {other:?}"),
    }
}

#[test]
fn citing_another_runs_evidence_cannot_finish_verified() {
    let contract = contract(&["check:a"]);
    let definitions = [definition("check:a")];
    // Passing evidence exists — but for a different candidate (another run
    // or files changed after that run's check).
    let evidence = [evidence("check:a", "digest-old", true)];
    let verdict = arbitrate(
        &inputs(&contract, Some("digest-new"), &evidence, &definitions),
        &proposal(Some("digest-new")),
    );
    match verdict {
        Arbitration::Final(TaskVerdict::Unverified { reason }) => {
            assert!(
                reason.contains("foreign") || reason.contains("missing"),
                "{reason}"
            );
        }
        other => panic!("expected unverified, got {other:?}"),
    }
}

#[test]
fn changing_files_after_a_passing_check_invalidates_it() {
    let contract = contract(&["check:a"]);
    let definitions = [definition("check:a")];
    // The check passed for digest-1; the candidate then changed (digest-2).
    let evidence = [evidence("check:a", "digest-1", true)];
    let verdict = arbitrate(
        &inputs(&contract, Some("digest-2"), &evidence, &definitions),
        &proposal(Some("digest-2")),
    );
    assert!(matches!(
        verdict,
        Arbitration::Final(TaskVerdict::Unverified { .. })
    ));
    // A proposal still naming the old digest is refused outright.
    let verdict = arbitrate(
        &inputs(&contract, Some("digest-2"), &evidence, &definitions),
        &proposal(Some("digest-1")),
    );
    assert!(matches!(
        verdict,
        Arbitration::Final(TaskVerdict::Unverified { reason }) if reason.contains("does not match")
    ));
}

#[test]
fn failed_checks_return_repair_feedback() {
    let contract = contract(&["check:a", "check:b"]);
    let definitions = [definition("check:a"), definition("check:b")];
    let evidence = [
        evidence("check:a", "digest-1", true),
        evidence("check:b", "digest-1", false),
    ];
    let verdict = arbitrate(
        &inputs(&contract, Some("digest-1"), &evidence, &definitions),
        &proposal(Some("digest-1")),
    );
    match verdict {
        Arbitration::RepairRequired { feedback } => {
            assert!(feedback.contains("check:b failed"), "{feedback}");
            assert!(
                feedback.contains("ev-check:b-digest-1"),
                "feedback cites evidence: {feedback}"
            );
        }
        other => panic!("expected repair, got {other:?}"),
    }
}

#[test]
fn unavailable_environments_block_instead_of_failing() {
    let contract = contract(&["check:a"]);
    let definitions = [definition("check:a")];
    let evidence: [EvidenceRecord; 0] = [];
    let mut case = inputs(&contract, Some("digest-1"), &evidence, &definitions);
    case.unavailable = vec![("check:a".into(), "node 18 missing on this machine".into())];
    let verdict = arbitrate(&case, &proposal(Some("digest-1")));
    match verdict {
        Arbitration::Final(TaskVerdict::Blocked { reason }) => {
            assert!(reason.contains("node 18 missing"), "{reason}");
        }
        other => panic!("expected blocked, got {other:?}"),
    }
}

#[test]
fn full_fresh_host_evidence_verifies() {
    let contract = contract(&["check:a", "check:b"]);
    let definitions = [definition("check:a"), definition("check:b")];
    let evidence = [
        evidence("check:a", "digest-1", true),
        evidence("check:b", "digest-1", true),
    ];
    let verdict = arbitrate(
        &inputs(&contract, Some("digest-1"), &evidence, &definitions),
        &proposal(Some("digest-1")),
    );
    assert_eq!(
        verdict,
        Arbitration::Final(TaskVerdict::Verified {
            candidate_digest: "digest-1".into()
        })
    );
    // Review disposition stays independent of the verdict.
    assert!(review_disposition_is_independent(&verdict));
}

#[test]
fn plugin_authored_evidence_never_verifies() {
    let contract = contract(&["check:a"]);
    let definitions = [definition("check:a")];
    let mut claimed = evidence("check:a", "digest-1", true);
    claimed.recorded_by = Provenance::Plugin {
        harness_id: "sneaky".into(),
        package_digest: "sha".into(),
    };
    let verdict = arbitrate(
        &inputs(&contract, Some("digest-1"), &[claimed], &definitions),
        &proposal(Some("digest-1")),
    );
    assert!(matches!(
        verdict,
        Arbitration::Final(TaskVerdict::Unverified { .. })
    ));
}

#[test]
fn replies_and_plan_drafts_settle_without_checks() {
    let contract = contract(&["check:a"]);
    let definitions = [definition("check:a")];
    let evidence: [EvidenceRecord; 0] = [];
    let mut reply = proposal(None);
    reply.kind = ProposalKind::Reply;
    let verdict = arbitrate(&inputs(&contract, None, &evidence, &definitions), &reply);
    assert!(matches!(
        verdict,
        Arbitration::Final(TaskVerdict::Unverified { reason }) if reason.contains("no code verification")
    ));
}
