//! Durable operation identity and input delivery contracts.
//!
//! JSON-RPC ids are connection-local correlation only. Effectful host methods
//! carry an attempt-scoped [`OperationKey`] whose dedup history survives
//! generation changes; input delivery is tracked with host-generated
//! message ids and checkpointed consumption positions.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Stable operation key chosen by the plugin, unique within an attempt.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(transparent)]
pub struct OperationKey(pub String);

impl OperationKey {
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }
}

/// Canonical hash of a request's normalized inputs. Two requests with the
/// same key but different hashes are a protocol violation, not a replay.
///
/// Canonicalization serializes the parsed JSON value; object keys are sorted
/// because serde_json's default map is a BTreeMap.
pub fn canonical_input_hash(params: &serde_json::Value) -> String {
    let canonical = serde_json::to_string(params).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(canonical.as_bytes());
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Host-owned reference to a pending operation that an approval request may
/// cite. Generic questions can never mint one of these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingOperationRef {
    pub operation_id: String,
    #[serde(rename = "inputHash")]
    pub input_hash: String,
}

/// How a method's side effects reconcile after a lost response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReplayClass {
    /// Pure query; retries are always safe.
    Idempotent,
    /// File-shaped effect; reconcile by comparing expected content hashes.
    FileEffect,
    /// Managed-process effect; effects beyond the host cannot be proven.
    ProcessEffect,
    /// External system acknowledgement (e.g. a CLI that applied changes);
    /// a lost ack is indeterminate, never silently retried.
    ExternalAck,
    /// Model stream; may be retried only when proven safe for replay.
    ModelStream,
}

impl ReplayClass {
    /// The replay class of every effectful host method.
    pub fn for_method(method: &str) -> Option<Self> {
        match method {
            "host.model.stream" => Some(ReplayClass::ModelStream),
            "host.tools.list" | "host.context.read" | "host.artifacts.read" => {
                Some(ReplayClass::Idempotent)
            }
            "host.tools.call" => Some(ReplayClass::FileEffect),
            "host.process.open" | "host.process.write" | "host.process.close" => {
                Some(ReplayClass::ProcessEffect)
            }
            "host.artifacts.put" => Some(ReplayClass::Idempotent),
            "host.children.spawn" | "host.children.wait" | "host.children.cancel" => {
                Some(ReplayClass::FileEffect)
            }
            "host.verification.run" => Some(ReplayClass::FileEffect),
            _ => None,
        }
    }

    pub fn is_effectful(&self) -> bool {
        !matches!(self, ReplayClass::Idempotent)
    }
}

/// Durable state of one recorded operation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperationRecord {
    pub attempt_id: String,
    pub operation_key: OperationKey,
    pub method: String,
    pub input_hash: String,
    pub state: OperationState,
    /// Generation in which the intent was first persisted.
    pub generation_recorded: u64,
    /// Generation in which the operation completed, when it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation_completed: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum OperationState {
    /// Intent persisted, outcome not yet recorded (crash window).
    Pending,
    /// Finished with a stored result; replays return the receipt.
    Completed { result: serde_json::Value },
    /// Outcome cannot be proven; surfaced, never auto-re-executed.
    Indeterminate { reason: String },
    /// Rejected before any effect (authorization, validation).
    Rejected { reason: String },
}

/// Why an outcome is indeterminate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IndeterminateReason {
    LostEffectResponse,
    ProcessTreeUncertain,
    LostExternalAcknowledgement,
    GuardianUnavailable,
}

impl std::fmt::Display for IndeterminateReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            IndeterminateReason::LostEffectResponse => "lost-effect-response",
            IndeterminateReason::ProcessTreeUncertain => "process-tree-uncertain",
            IndeterminateReason::LostExternalAcknowledgement => "lost-external-acknowledgement",
            IndeterminateReason::GuardianUnavailable => "guardian-unavailable",
        };
        f.write_str(text)
    }
}

/// What the host should do when the same operation key arrives again.
#[derive(Debug, Clone, PartialEq)]
pub enum ReplayDecision {
    /// No prior record; execute and persist the intent first.
    Fresh,
    /// Same key and hash, completed: return the stored receipt unchanged.
    ReplayReceipt { result: serde_json::Value },
    /// Same key and hash, still pending; action depends on the replay class.
    Reconcile { class: ReplayClass },
    /// Same key with a different input hash; refuse.
    ConflictingInput { recorded: String, incoming: String },
}

/// Decide how to serve a repeated call. Dedup history is keyed by
/// (attempt, operation key) and is *not* erased by generation changes.
pub fn replay_decision(
    record: Option<&OperationRecord>,
    method: &str,
    incoming_hash: &str,
) -> ReplayDecision {
    let Some(record) = record else {
        return ReplayDecision::Fresh;
    };
    if record.input_hash != incoming_hash {
        return ReplayDecision::ConflictingInput {
            recorded: record.input_hash.clone(),
            incoming: incoming_hash.to_string(),
        };
    }
    match &record.state {
        OperationState::Completed { result } => ReplayDecision::ReplayReceipt {
            result: result.clone(),
        },
        OperationState::Pending => ReplayDecision::Reconcile {
            class: ReplayClass::for_method(method).unwrap_or(ReplayClass::ExternalAck),
        },
        // Rejected operations replay their rejection exactly like completed
        // ones: the decision was durable.
        OperationState::Rejected { .. } => ReplayDecision::Reconcile {
            class: ReplayClass::Idempotent,
        },
        OperationState::Indeterminate { .. } => ReplayDecision::Reconcile {
            class: ReplayClass::ExternalAck,
        },
    }
}

/// Checkpointed consumption position over host-delivered inputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsumedInputCheckpoint {
    /// Contiguous prefix of `input_seq` values the plugin state includes.
    pub consumed_input_seq: u64,
}

/// Validate that a checkpointed consumption position is a contiguous prefix
/// of the inputs the host has delivered (`0..=last_delivered_seq`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InputAckError {
    #[error("consumed_input_seq {consumed} exceeds last delivered seq {delivered}")]
    ConsumedBeyondDelivered { consumed: u64, delivered: u64 },
    #[error("consumed_input_seq {0} is not contiguous with delivered history")]
    NotContiguous(u64),
}

pub fn validate_consumed_prefix(
    checkpoint: ConsumedInputCheckpoint,
    last_delivered_seq: u64,
) -> Result<(), InputAckError> {
    if checkpoint.consumed_input_seq > last_delivered_seq {
        return Err(InputAckError::ConsumedBeyondDelivered {
            consumed: checkpoint.consumed_input_seq,
            delivered: last_delivered_seq,
        });
    }
    Ok(())
}

/// Inputs that must be (re)delivered after restoring a checkpoint. Delivery
/// to the plugin may replay from the checkpoint; host queue acceptance is
/// exactly-once, external application never claimed exactly-once.
pub fn inputs_to_redeliver(
    checkpoint: ConsumedInputCheckpoint,
    delivered: &[crate::services::InputMessage],
) -> Vec<crate::services::InputMessage> {
    delivered
        .iter()
        .filter(|message| message.input_seq > checkpoint.consumed_input_seq)
        .cloned()
        .collect()
}

/// Outcome of an operation whose acknowledgement was lost.
pub fn lost_ack_outcome(reason: IndeterminateReason) -> OperationState {
    OperationState::Indeterminate {
        reason: reason.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_hash_is_order_insensitive_for_objects() {
        let a: serde_json::Value =
            serde_json::from_str(r#"{"b":1,"a":{"y":[1,2],"x":"z"}}"#).unwrap();
        let b: serde_json::Value =
            serde_json::from_str(r#"{"a":{"x":"z","y":[1,2]},"b":1}"#).unwrap();
        assert_eq!(canonical_input_hash(&a), canonical_input_hash(&b));
        let c: serde_json::Value =
            serde_json::from_str(r#"{"b":2,"a":{"y":[1,2],"x":"z"}}"#).unwrap();
        assert_ne!(canonical_input_hash(&a), canonical_input_hash(&c));
    }
}
