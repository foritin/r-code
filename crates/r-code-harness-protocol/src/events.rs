//! Progress events and provenance.
//!
//! Plugin-reported observations and host-generated execution facts are two
//! different sources: [`Provenance`] travels on every envelope so downstream
//! consumers can never confuse a plugin's claim with a host fact.

use serde::{Deserialize, Serialize};

/// Where an event came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "kebab-case")]
pub enum Provenance {
    /// Fact produced by the host (execution, verification, storage).
    Host,
    /// Observation reported by a plugin process.
    Plugin {
        harness_id: String,
        #[serde(rename = "packageDigest")]
        package_digest: String,
    },
}

/// Kind of progress event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EventKind {
    Progress,
    RunState,
    ModelStream,
    ToolStarted,
    ToolFinished,
    ProcessStarted,
    ProcessExited,
    PlanUpdated,
    QuestionRaised,
    ApprovalRaised,
    CheckpointSaved,
    VerificationScheduled,
    VerificationFinished,
    CompletionProposed,
    ChildUpdate,
    Cancelled,
    Error,
}

/// `harness.event` notification params (plugin -> host).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarnessEventParams {
    pub kind: EventKind,
    pub payload: serde_json::Value,
    /// Correlates stream chunks when the event belongs to one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_id: Option<String>,
}

/// Host-side envelope persisted to the ordered event journal and broadcast to
/// clients. `seq` is assigned by the host journal, not by plugins.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub seq: u64,
    pub task_id: String,
    pub run_id: String,
    pub kind: EventKind,
    pub source: Provenance,
    pub payload: serde_json::Value,
}
