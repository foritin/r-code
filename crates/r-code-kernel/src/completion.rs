//! Completion arbitration.
//!
//! Handles [`CompletionProposal`]s by looking up host-owned required
//! evidence for the current contract and candidate: verified only when
//! every required check has fresh passing host evidence; failed checks
//! return repair feedback; unavailable environments block; anything else
//! finishes unverified. Review disposition stays independent.

use crate::task::{
    Actor, CompletionProposal, EvidenceRecord, ProposalKind, TaskContract, TaskKind, TaskVerdict,
};
use crate::verification::{CheckDefinition, EvidenceValidity};

/// Inputs the arbiter needs (host-owned).
pub struct ArbitrationInputs<'a> {
    pub contract: &'a TaskContract,
    pub candidate_digest: Option<&'a str>,
    pub evidence: &'a [EvidenceRecord],
    pub definitions: &'a [CheckDefinition],
    /// Checks whose environment was unavailable this run.
    pub unavailable: Vec<(String, String)>,
}

/// The full verdict including repair-shaped outcomes.
#[derive(Debug, Clone, PartialEq)]
pub enum Arbitration {
    /// Ready to finalize with this verdict.
    Final(TaskVerdict),
    /// Check failures with actionable feedback: the run continues.
    RepairRequired { feedback: String },
}

/// Arbitrate a proposal. The kernel alone decides; plugins only propose.
pub fn arbitrate(inputs: &ArbitrationInputs<'_>, proposal: &CompletionProposal) -> Arbitration {
    // Non-plugin proposals are a programming error upstream.
    debug_assert_eq!(proposal.actor, Actor::Plugin);

    // Replies and plan drafts settle without code checks.
    if !inputs.contract.kind.requires_code_evidence() || proposal.kind == ProposalKind::Reply {
        return Arbitration::Final(TaskVerdict::Unverified {
            reason: "no code verification required for this task kind".into(),
        });
    }

    // The proposal must name the current candidate (or none, letting the
    // current one apply); a stale digest is rejected outright.
    let digest = match (inputs.candidate_digest, &proposal.candidate_digest) {
        (Some(current), Some(claimed)) if current == claimed => current,
        (Some(current), None) => current,
        _ => {
            return Arbitration::Final(TaskVerdict::Unverified {
                reason: "proposal does not match the current candidate content".into(),
            })
        }
    };

    // Classify each required check.
    let mut missing: Vec<String> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    for check_id in &inputs.contract.required_checks {
        // Unavailable environment → blocked (budgets/infrastructure).
        if let Some((_, reason)) = inputs
            .unavailable
            .iter()
            .find(|(unavailable_id, _)| unavailable_id == check_id)
        {
            return Arbitration::Final(TaskVerdict::Blocked {
                reason: format!("check {check_id} unavailable: {reason}"),
            });
        }
        let definition = inputs
            .definitions
            .iter()
            .find(|def| &def.check_id == check_id);
        let matching: Vec<&EvidenceRecord> = inputs
            .evidence
            .iter()
            .filter(|record| &record.check_id == check_id)
            .collect();
        if matching.is_empty() {
            missing.push(check_id.clone());
            continue;
        }
        // Evidence for other candidates/runs never counts: same check,
        // different digest → stale.
        let current_run: Vec<&&EvidenceRecord> = matching
            .iter()
            .filter(|record| record.candidate_digest == digest)
            .collect();
        if current_run.is_empty() {
            missing.push(format!("{check_id} (only foreign/stale evidence)"));
            continue;
        }
        // A real failure means a host record for this candidate that ran and
        // did not pass. Plugin-claimed passes or never-run checks are
        // missing evidence, not failures.
        let has_host_failure = current_run.iter().any(|record| {
            !record.passed
                && matches!(
                    record.recorded_by,
                    r_code_harness_protocol::Provenance::Host
                )
        });
        let any_passing = current_run.iter().any(|record| match definition {
            Some(def) => {
                let environment_ok =
                    record.environment == def.toolchain || def.identity().is_empty();
                EvidenceValidity::is_valid(record, def, digest, &record.environment)
                    && environment_ok
            }
            None => {
                record.passed
                    && matches!(
                        record.recorded_by,
                        r_code_harness_protocol::Provenance::Host
                    )
            }
        });
        if has_host_failure {
            let feedback = current_run
                .iter()
                .find(|record| !record.passed)
                .map(|record| format!("check {check_id} failed (evidence {})", record.evidence_id))
                .unwrap_or_else(|| format!("check {check_id} failed"));
            failures.push(feedback);
        } else if !any_passing {
            missing.push(format!("{check_id} (no valid host evidence)"));
        }
    }

    if !failures.is_empty() {
        return Arbitration::RepairRequired {
            feedback: failures.join("; "),
        };
    }
    if !missing.is_empty() {
        return Arbitration::Final(TaskVerdict::Unverified {
            reason: format!(
                "missing required evidence for checks: {}",
                missing.join(", ")
            ),
        });
    }
    Arbitration::Final(TaskVerdict::Verified {
        candidate_digest: digest.to_string(),
    })
}

/// Convenience: the review disposition stays orthogonal — this helper only
/// exists so callers remember it is *not* part of the verdict.
pub fn review_disposition_is_independent(_verdict: &Arbitration) -> bool {
    true
}

/// Wire into [`crate::task::TaskState::apply_proposal`] outcomes: map a
/// repair decision back into the proposal decision type used by the state
/// machine (kept separate so the arbiter stays pure).
pub fn task_kind_requires_evidence(kind: TaskKind) -> bool {
    kind.requires_code_evidence()
}
