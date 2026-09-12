//! Async service ports the kernel depends on.
//!
//! The kernel never links persistence or runtime implementations; it talks to
//! these traits. Concrete adapters live in `r-code-runtime` / `r-code-store`.
//! All effectful calls carry a [`GenerationToken`] so stale work from revoked
//! generations is rejected before reaching a service.

use crate::task::{Attempt, OperationReceipt, TaskContract, TaskState};
use async_trait::async_trait;
use r_code_harness_protocol::{
    ArtifactRef, InputMessage, ModelStreamRequest, OperationKey, StreamEvent, ToolCallReply,
    ToolCallRequest, ToolDescriptor,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// One ordered journal event. Durable mutations commit their aggregate and
/// events in one transaction (see T05).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalEvent {
    pub seq: u64,
    pub task_id: String,
    pub kind: String,
    pub payload: serde_json::Value,
}

/// Errors shared by the ports.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ServiceError {
    #[error("stale generation {provided} for run {run_id} (current {current})")]
    StaleGeneration {
        run_id: String,
        provided: u64,
        current: u64,
    },
    #[error("operation was cancelled")]
    Cancelled,
    #[error("service failure: {0}")]
    Failure(String),
    #[error("store failure: {0}")]
    Store(String),
}

/// Token binding a call to a live run generation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GenerationToken {
    pub run_id: String,
    pub generation: u64,
}

impl GenerationToken {
    pub fn new(run_id: impl Into<String>, generation: u64) -> Self {
        Self {
            run_id: run_id.into(),
            generation,
        }
    }
}

/// Shared cancellation/generation state for one run. Cancellation revokes the
/// generation first; late callbacks observe a mismatch and fail closed.
#[derive(Debug)]
pub struct RunGuard {
    run_id: String,
    cancelled: AtomicBool,
    generation: AtomicU64,
}

impl RunGuard {
    pub fn new(run_id: impl Into<String>, generation: u64) -> Arc<Self> {
        Arc::new(Self {
            run_id: run_id.into(),
            cancelled: AtomicBool::new(false),
            generation: AtomicU64::new(generation),
        })
    }

    pub fn token(&self) -> GenerationToken {
        GenerationToken {
            run_id: self.run_id.clone(),
            generation: self.generation.load(Ordering::SeqCst),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Revoke the current generation (cancellation step 1).
    pub fn revoke(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Bump the generation after a restart; tokens from before are stale.
    pub fn rotate(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Validate a caller-supplied token against current state.
    pub fn check(&self, token: &GenerationToken) -> Result<(), ServiceError> {
        if self.is_cancelled() {
            return Err(ServiceError::Cancelled);
        }
        let current = self.generation.load(Ordering::SeqCst);
        if token.generation != current {
            return Err(ServiceError::StaleGeneration {
                run_id: self.run_id.clone(),
                provided: token.generation,
                current,
            });
        }
        Ok(())
    }
}

/// Durable persistence port. `save_task_and_events` is atomic: either the
/// aggregate and its events both commit or neither does.
#[async_trait]
pub trait JournalStore: Send + Sync {
    async fn load_task(&self, task_id: &str) -> Option<TaskState>;
    async fn save_task_and_events(
        &self,
        task: &TaskState,
        events: Vec<JournalEvent>,
    ) -> Result<(), ServiceError>;
    async fn read_events(&self, after_seq: u64, limit: u32) -> Vec<JournalEvent>;
    /// Highest allocated journal seq (0 when the journal is empty) — the
    /// water level callers can stamp into payloads written *after* the
    /// probe. Stores with direct SQL access override the full-scan default.
    async fn max_event_seq(&self) -> u64 {
        self.read_events(0, u32::MAX)
            .await
            .last()
            .map(|event| event.seq)
            .unwrap_or(0)
    }
    async fn save_receipt(&self, receipt: OperationReceipt) -> Result<(), ServiceError>;
    async fn load_receipt(&self, attempt_id: &str, key: &OperationKey) -> Option<OperationReceipt>;
    async fn save_checkpoint(
        &self,
        attempt_id: &str,
        revision: u64,
        state: Vec<u8>,
        consumed_input_seq: u64,
    ) -> Result<ArtifactRef, ServiceError>;
    async fn load_latest_checkpoint(&self, attempt_id: &str) -> Option<CheckpointRecord>;
}

/// A stored plugin checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointRecord {
    pub attempt_id: String,
    pub revision: u64,
    pub state: Vec<u8>,
    pub consumed_input_seq: u64,
    pub artifact: ArtifactRef,
}

/// Receiver side of streaming output (model deltas, process output).
#[async_trait]
pub trait StreamSink: Send {
    async fn send(&mut self, event: StreamEvent) -> Result<(), ServiceError>;
}

/// Final outcome of a model stream.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelStreamOutcome {
    pub stream_id: String,
    pub finish_reason: Option<String>,
    pub usage: r_code_harness_protocol::ModelUsage,
}

/// Model provider access, host-mediated; credentials never leave the host.
#[async_trait]
pub trait ModelService: Send + Sync {
    async fn stream(
        &self,
        token: GenerationToken,
        request: ModelStreamRequest,
        sink: &mut dyn StreamSink,
    ) -> Result<ModelStreamOutcome, ServiceError>;
}

/// Tool execution (Gateway-backed in the runtime).
#[async_trait]
pub trait ToolService: Send + Sync {
    async fn list(&self, token: GenerationToken) -> Result<Vec<ToolDescriptor>, ServiceError>;
    async fn call(
        &self,
        token: GenerationToken,
        call: ToolCallRequest,
    ) -> Result<ToolCallReply, ServiceError>;
}

/// Managed interactive processes with run-scoped handles.
#[async_trait]
pub trait ProcessService: Send + Sync {
    async fn open(
        &self,
        token: GenerationToken,
        profile: &str,
        arguments: Vec<String>,
        cwd: Option<String>,
    ) -> Result<String, ServiceError>;
    async fn write(
        &self,
        token: GenerationToken,
        handle: &str,
        data: Vec<u8>,
    ) -> Result<(), ServiceError>;
    async fn close(
        &self,
        token: GenerationToken,
        handle: &str,
    ) -> Result<Option<i32>, ServiceError>;
}

/// Workspace candidate capture and file access.
#[async_trait]
pub trait WorkspaceService: Send + Sync {
    async fn capture_candidate(&self, task_id: &str) -> Result<CandidateManifest, ServiceError>;
    async fn read_file(&self, path: &str) -> Result<Vec<u8>, ServiceError>;
    async fn write_file(
        &self,
        token: GenerationToken,
        path: &str,
        content: Vec<u8>,
    ) -> Result<(), ServiceError>;
}

/// Immutable snapshot of candidate content identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateManifest {
    pub digest: String,
    pub file_count: u64,
}

/// One harness plugin session bound to a run.
#[async_trait]
pub trait HarnessSession: Send + Sync {
    async fn start(
        &self,
        attempt: &Attempt,
        contract: &TaskContract,
        first_input: &InputMessage,
    ) -> Result<(), ServiceError>;
    async fn resume(
        &self,
        attempt: &Attempt,
        checkpoint: &ArtifactRef,
        replay_inputs: &[InputMessage],
    ) -> Result<(), ServiceError>;
    async fn steer(&self, input: &InputMessage) -> Result<(), ServiceError>;
    async fn cancel(&self, reason: &str) -> Result<(), ServiceError>;
}
