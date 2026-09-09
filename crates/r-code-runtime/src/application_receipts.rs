//! Daemon-side application command deduplication.
//!
//! Wraps any [`ApplicationHandler`] with `(profile_id, client_id,
//! command_id)` receipts persisted in the v2 store. Effects execute once;
//! reconnects replay the recorded result; a reused command id with a
//! different canonical payload is refused. This layer is independent of the
//! plugin-attempt `operation_key` dedup in the RPC router.

use crate::daemon::ApplicationHandler;
use r_code_harness_protocol::application::ApplicationCommand;
use r_code_harness_protocol::{EventEnvelope, RunIdentity};
use r_code_store::v2::{CommandReceiptState, V2Store};
use std::sync::Arc;

/// Canonical hash of a command: method + canonical params. Object keys are
/// sorted by serde_json's default map, so member order never matters.
pub fn canonical_command_hash(method: &str, params: &serde_json::Value) -> String {
    let payload = serde_json::json!({"method": method, "params": params});
    r_code_harness_protocol::canonical_input_hash(&payload)
}

/// Deduplicating wrapper over an inner handler.
pub struct CommandDedup {
    profile_id: String,
    store: Arc<V2Store>,
    inner: Arc<dyn ApplicationHandler>,
}

impl CommandDedup {
    pub fn new(profile_id: &str, store: Arc<V2Store>, inner: Arc<dyn ApplicationHandler>) -> Self {
        Self {
            profile_id: profile_id.to_string(),
            store,
            inner,
        }
    }
}

#[async_trait::async_trait]
impl ApplicationHandler for CommandDedup {
    async fn execute(&self, command: ApplicationCommand) -> Result<serde_json::Value, String> {
        let hash = canonical_command_hash(&command.method, &command.params);
        let state = self
            .store
            .application_command_intent(
                &self.profile_id,
                &command.client_id,
                &command.command_id,
                &command.method,
                &hash,
            )
            .map_err(|e| e.to_string())?;
        match state {
            CommandReceiptState::Completed { result } => Ok(result),
            CommandReceiptState::Accepted => Err(
                "command with this id is already in flight; retry to observe its result".into(),
            ),
            CommandReceiptState::Conflict { recorded_hash, incoming_hash } => Err(format!(
                "command id {} was already used with a different payload ({recorded_hash} vs {incoming_hash})",
                command.command_id
            )),
            CommandReceiptState::Fresh => {
                let outcome = self.inner.execute(command.clone()).await;
                match &outcome {
                    Ok(result) => {
                        // The effect happened; the receipt must survive.
                        self.store
                            .complete_application_command(
                                &self.profile_id,
                                &command.client_id,
                                &command.command_id,
                                result,
                            )
                            .map_err(|e| e.to_string())?;
                    }
                    Err(message) => {
                        // Failures stay queryable: record a null result so a
                        // retry replays the same failure instead of
                        // re-executing a possibly-partial effect.
                        self.store
                            .complete_application_command(
                                &self.profile_id,
                                &command.client_id,
                                &command.command_id,
                                &serde_json::json!({"error": message}),
                            )
                            .map_err(|e| e.to_string())?;
                    }
                }
                outcome
            }
        }
    }

    async fn events_after(&self, after_seq: u64, limit: u32) -> Vec<EventEnvelope> {
        self.inner.events_after(after_seq, limit).await
    }
}

/// RunIdentity helper used by callers building demo/test handlers.
pub fn demo_identity() -> RunIdentity {
    RunIdentity {
        task_id: String::new(),
        branch_id: String::new(),
        run_id: String::new(),
        attempt_id: String::new(),
        generation: 0,
    }
}
