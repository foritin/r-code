//! Checkpoints and operation reconciliation.
//!
//! Recovery for one attempt: reconcile its durable operation receipts
//! (completed effects replay, indeterminate effects surface barriers),
//! validate the pinned plugin/config/contract/workspace identity, and
//! produce an explicit plan — resume from checkpoint, restart with a new
//! attempt, or stay blocked. There is no global reset-hard path.

use crate::task::{Attempt, ReceiptOutcome};
use r_code_harness_protocol::operations::{replay_decision, ReplayClass, ReplayDecision};
use r_code_harness_protocol::{OperationKey, PackageRef};

/// The recovery plan for an attempt.
#[derive(Debug, Clone, PartialEq)]
pub enum RecoveryPlan {
    /// Same package/config/contract identity: resume from the checkpoint,
    /// replaying inputs after `consumed_input_seq`.
    ResumeFrom {
        checkpoint_revision: u64,
        consumed_input_seq: u64,
    },
    /// The pinned plugin is unavailable (removed, not installed): a
    /// user-visible restart decision, never a silent fallback.
    RestartRequired { reason: String },
    /// Indeterminate effects or unprovable termination block new writes.
    Blocked { reason: String },
}

/// Identity check for checkpoint restore: only the same package bytes,
/// configuration and contract revision may resume.
pub fn identity_matches(
    attempt: &Attempt,
    pinned_package: &PackageRef,
    config_hash: &str,
    contract_revision: u64,
    workspace_identity: &str,
) -> bool {
    attempt.package.id == pinned_package.id
        && attempt.package.content_digest == pinned_package.content_digest
        && attempt.config_hash == config_hash
        && attempt.contract_revision == contract_revision
        && attempt.workspace_identity == workspace_identity
}

/// Reconcile one effectful operation's receipt into a recovery action.
/// Fault injection before the effect leaves `Pending` (reconcile by class);
/// after the effect leaves `Completed` (replay); indeterminate effects
/// never re-execute blindly.
#[derive(Debug, Clone, PartialEq)]
pub enum ReconciledEffect {
    /// Replay the stored result (the effect happened).
    Replay { result: serde_json::Value },
    /// File-shaped effect: reconcile by comparing expected content hashes.
    ReconcileByHash { operation_key: OperationKey },
    /// Process/external effect: indeterminate — barrier, do not re-execute.
    Indeterminate {
        operation_key: OperationKey,
        reason: String,
    },
    /// Rejected before any effect: safe to re-run if the plugin retries.
    SafeToRetry { operation_key: OperationKey },
}

pub fn reconcile_receipt(
    receipt: &crate::task::OperationReceipt,
    method: &str,
    incoming_hash: &str,
) -> ReconciledEffect {
    let record = r_code_harness_protocol::OperationRecord {
        attempt_id: receipt.attempt_id.clone(),
        operation_key: receipt.operation_key.clone(),
        method: receipt.method.clone(),
        input_hash: receipt.input_hash.clone(),
        state: match &receipt.outcome {
            ReceiptOutcome::Completed { result } => {
                r_code_harness_protocol::OperationState::Completed {
                    result: result.clone(),
                }
            }
            ReceiptOutcome::Indeterminate { reason } => {
                r_code_harness_protocol::OperationState::Indeterminate {
                    reason: reason.clone(),
                }
            }
            ReceiptOutcome::Rejected { reason } => {
                r_code_harness_protocol::OperationState::Rejected {
                    reason: reason.clone(),
                }
            }
        },
        generation_recorded: 0,
        generation_completed: None,
    };
    let _ = method;
    match replay_decision(Some(&record), &receipt.method, incoming_hash) {
        ReplayDecision::ReplayReceipt { result } => ReconciledEffect::Replay { result },
        ReplayDecision::Reconcile { class } => match class {
            ReplayClass::FileEffect => ReconciledEffect::ReconcileByHash {
                operation_key: receipt.operation_key.clone(),
            },
            ReplayClass::Idempotent => ReconciledEffect::SafeToRetry {
                operation_key: receipt.operation_key.clone(),
            },
            other => ReconciledEffect::Indeterminate {
                operation_key: receipt.operation_key.clone(),
                reason: format!("{other:?} effect with unresolved outcome"),
            },
        },
        ReplayDecision::ConflictingInput { recorded, incoming } => {
            ReconciledEffect::Indeterminate {
                operation_key: receipt.operation_key.clone(),
                reason: format!("conflicting input ({recorded} vs {incoming})"),
            }
        }
        ReplayDecision::Fresh => ReconciledEffect::SafeToRetry {
            operation_key: receipt.operation_key.clone(),
        },
    }
}

/// Decide the recovery plan for an attempt given its reconciliation inputs.
#[allow(clippy::too_many_arguments)]
pub fn plan_recovery(
    attempt: &Attempt,
    pinned_package: &PackageRef,
    config_hash: &str,
    contract_revision: u64,
    workspace_identity: &str,
    pinned_plugin_installed: bool,
    checkpoint: Option<(u64, u64)>, // (revision, consumed_input_seq)
    indeterminate_effects: &[String],
) -> RecoveryPlan {
    // Pinned plugin unavailable: explicit restart decision.
    if !pinned_plugin_installed {
        return RecoveryPlan::RestartRequired {
            reason: format!(
                "pinned plugin {} @ {} is not installed",
                pinned_package.id.0,
                &pinned_package.content_digest[..12.min(pinned_package.content_digest.len())]
            ),
        };
    }
    // Identity mismatch (package/config/contract/workspace changed):
    // checkpoints of a different identity never resume.
    if !identity_matches(
        attempt,
        pinned_package,
        config_hash,
        contract_revision,
        workspace_identity,
    ) {
        return RecoveryPlan::RestartRequired {
            reason: "attempt identity no longer matches (package/config/contract/workspace)".into(),
        };
    }
    // Indeterminate effects block new writes until reconciled/proven.
    if !indeterminate_effects.is_empty() {
        return RecoveryPlan::Blocked {
            reason: format!(
                "indeterminate effects require reconciliation: {}",
                indeterminate_effects.join(", ")
            ),
        };
    }
    match checkpoint {
        Some((revision, consumed)) => RecoveryPlan::ResumeFrom {
            checkpoint_revision: revision,
            consumed_input_seq: consumed,
        },
        None => RecoveryPlan::RestartRequired {
            reason: "no checkpoint recorded for this attempt".into(),
        },
    }
}
