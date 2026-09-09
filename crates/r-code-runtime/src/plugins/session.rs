//! A plugin session: one spawned process bound to one run identity.
//!
//! The session wires the transport's callback surface into the
//! [`HostRouter`], forwards lifecycle requests (start/resume/steer/cancel)
//! and enforces generation fencing on every outbound call. It implements the
//! kernel's `HarnessSession` port.

use crate::plugins::router::HostRouter;
use crate::plugins::transport::{spawn_plugin, PluginCallbacks, PluginProcess, TransportLimits};
use r_code_harness_protocol::rpc::{RpcError, RpcNotification, RpcRequest};
use r_code_harness_protocol::services::{
    HarnessCancelResult, HarnessResumeParams, HarnessStartParams, HarnessSteerParams,
    InitializeParams, InitializeResult,
};
use r_code_harness_protocol::{InputMessage, NegotiatedCapabilities, RunIdentity};
use r_code_kernel::ports::{HarnessSession, RunGuard, ServiceError};
use r_code_kernel::task::{Attempt, TaskContract};
use std::sync::Arc;
use std::time::Duration;

/// Errors from session operation.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SessionError {
    #[error("transport failure: {0}")]
    Transport(String),
    #[error("service failure: {0}")]
    Service(String),
}

impl From<r_code_kernel::ports::ServiceError> for SessionError {
    fn from(error: r_code_kernel::ports::ServiceError) -> Self {
        SessionError::Service(error.to_string())
    }
}

/// Callbacks adapter binding plugin calls to one router.
struct SessionCallbacks {
    router: Arc<HostRouter>,
}

#[async_trait::async_trait]
impl PluginCallbacks for SessionCallbacks {
    async fn handle_request(&self, request: RpcRequest) -> Result<serde_json::Value, RpcError> {
        self.router.handle_request(request).await
    }

    async fn handle_notification(&self, notification: RpcNotification) {
        self.router.handle_notification(notification).await;
    }
}

/// One live plugin process for one run.
pub struct PluginSession {
    identity: RunIdentity,
    capabilities: NegotiatedCapabilities,
    guard: Arc<RunGuard>,
    router: Arc<HostRouter>,
    process: Arc<PluginProcess>,
    store_view: Arc<dyn r_code_kernel::ports::JournalStore>,
}

impl PluginSession {
    /// Spawn the plugin and perform the initialize handshake.
    #[allow(clippy::too_many_arguments)] // mirrors the wire handshake's full parameter set
    pub async fn start(
        executable: &std::path::Path,
        argv: &[String],
        identity: RunIdentity,
        capabilities: NegotiatedCapabilities,
        guard: Arc<RunGuard>,
        router: Arc<HostRouter>,
        harness_config: serde_json::Value,
        limits: TransportLimits,
    ) -> Result<Self, SessionError> {
        let callbacks = Arc::new(SessionCallbacks {
            router: router.clone(),
        });
        let process = spawn_plugin(executable, argv, callbacks, limits)
            .await
            .map_err(|e| SessionError::Transport(e.to_string()))?;
        let process = Arc::new(process);
        let initialize = InitializeParams {
            protocol: "r-code-harness/1".into(),
            host_api: capabilities.host_api,
            identity: identity.clone(),
            granted_services: capabilities.granted_services.clone(),
            harness_config,
            limits: r_code_harness_protocol::services::ProtocolLimits {
                max_frame_bytes: limits.max_frame_bytes,
                max_queue_bytes: limits.max_queue_bytes,
                initialize_timeout_ms: limits.initialize_timeout.as_millis() as u64,
                cancel_grace_ms: limits.cancel_grace.as_millis() as u64,
            },
        };
        let result: InitializeResult = process
            .initialize(&initialize)
            .await
            .map_err(|e| SessionError::Transport(e.to_string()))?;
        let _ = result; // identity recorded in the manifest/catalog at spawn time
        Ok(Self {
            identity,
            capabilities,
            guard,
            router: router.clone(),
            process,
            store_view: router.store.clone(),
        })
    }

    pub fn identity(&self) -> &RunIdentity {
        &self.identity
    }

    pub fn capabilities(&self) -> &NegotiatedCapabilities {
        &self.capabilities
    }

    pub fn router(&self) -> &Arc<HostRouter> {
        &self.router
    }

    pub fn process(&self) -> &Arc<PluginProcess> {
        &self.process
    }

    fn ensure_live(&self) -> Result<(), ServiceError> {
        self.guard.check(&r_code_kernel::ports::GenerationToken {
            run_id: self.identity.run_id.clone(),
            generation: self.identity.generation,
        })
    }

    async fn lifecycle_request(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, SessionError> {
        self.ensure_live()?;
        let result = self
            .process
            .request(method, params, SERVICE_TIMEOUT)
            .await
            .map_err(|e| SessionError::Transport(e.to_string()))?;
        Ok(result)
    }
}

const SERVICE_TIMEOUT: Duration = Duration::from_secs(120);

#[async_trait::async_trait]
impl HarnessSession for PluginSession {
    async fn start(
        &self,
        attempt: &Attempt,
        contract: &TaskContract,
        first_input: &InputMessage,
    ) -> Result<(), ServiceError> {
        let params = HarnessStartParams {
            identity: self.identity.clone(),
            contract: serde_json::to_value(contract)
                .map_err(|e| ServiceError::Failure(e.to_string()))?,
            transcript_cursor: 0,
        };
        let _ = self
            .lifecycle_request(
                "harness.start",
                serde_json::to_value(&params).unwrap_or_default(),
            )
            .await
            .map_err(|e| ServiceError::Failure(e.to_string()))?;
        let _ = (attempt, first_input);
        Ok(())
    }

    async fn resume(
        &self,
        attempt: &Attempt,
        checkpoint: &r_code_harness_protocol::ArtifactRef,
        replay_inputs: &[InputMessage],
    ) -> Result<(), ServiceError> {
        // Embed the stored checkpoint bytes for the plugin (quota-bounded).
        let state_base64 = self
            .store_view
            .load_latest_checkpoint(&attempt.attempt_id)
            .await
            .map(|record| {
                use base64::Engine as _;
                base64::engine::general_purpose::STANDARD.encode(record.state)
            });
        let params = HarnessResumeParams {
            identity: self.identity.clone(),
            checkpoint: checkpoint.clone(),
            state_base64,
            replay_inputs: replay_inputs.to_vec(),
        };
        self.lifecycle_request(
            "harness.resume",
            serde_json::to_value(&params).unwrap_or_default(),
        )
        .await
        .map_err(|e| ServiceError::Failure(e.to_string()))?;
        Ok(())
    }

    async fn steer(&self, input: &InputMessage) -> Result<(), ServiceError> {
        self.ensure_live()?;
        let params = HarnessSteerParams {
            identity: self.identity.clone(),
            input: input.clone(),
        };
        self.process
            .notify(
                "harness.steer",
                serde_json::to_value(&params).unwrap_or_default(),
            )
            .map_err(|e| ServiceError::Failure(e.to_string()))?;
        Ok(())
    }

    async fn cancel(&self, reason: &str) -> Result<(), ServiceError> {
        let _ = HarnessCancelResult {
            acknowledged: self.process.cancel(reason).await,
        };
        Ok(())
    }
}
