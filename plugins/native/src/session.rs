//! Session persistence for the Native plugin: only plugin-specific opaque
//! state (the conversation projection and loop position). Host ids,
//! transcript and task completion stay authoritative host-side.

use crate::loop_engine::LoopConfig;
use crate::request_projection::ConversationState;
use r_code_harness_protocol::services::{
    HarnessResumeParams, HarnessStartParams, HarnessSteerParams, InitializeParams, InitializeResult,
};
use r_code_harness_protocol::EventKind;
use r_code_harness_sdk::{HarnessHandlers, SdkError, SdkHandle};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Shared session state (steerable between turns). The loop config is
/// seeded by the host through `initialize` (per-task model selection,
/// inference knobs and task mode) and stays fixed for the process.
pub struct NativeSession {
    config: Mutex<LoopConfig>,
    state: Mutex<ConversationState>,
}

impl NativeSession {
    pub fn new(config: LoopConfig) -> Arc<Self> {
        Arc::new(Self {
            config: Mutex::new(config),
            state: Mutex::new(ConversationState::default()),
        })
    }
}

#[async_trait::async_trait]
impl HarnessHandlers for NativeSession {
    async fn on_initialize(&self, params: InitializeParams) -> Result<InitializeResult, SdkError> {
        *self.config.lock().await = LoopConfig::from_harness_config(&params.harness_config);
        Ok(InitializeResult {
            harness_id: "native.r-code".into(),
            harness_version: env!("CARGO_PKG_VERSION").into(),
            ready_checkpoint: None,
        })
    }

    async fn on_start(
        &self,
        handle: SdkHandle,
        params: HarnessStartParams,
    ) -> Result<serde_json::Value, SdkError> {
        // The objective seeds the first user turn; the contract stays
        // host-owned (we only read the objective text).
        let objective = params
            .contract
            .get("objective")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let config = self.config.lock().await.clone();
        let mut state = self.state.lock().await;
        *state = ConversationState::default();
        let result = crate::loop_engine::run_loop(&handle, &config, &mut state, &objective)
            .await
            .map_err(|error| SdkError::Fault(error.to_string()))?;
        handle
            .emit_event(
                EventKind::Progress,
                serde_json::json!({
                    "turns": result.turns,
                    "toolCalls": result.tool_calls_executed,
                }),
            )
            .await?;
        Ok(serde_json::to_value(&result).unwrap_or_default())
    }

    async fn on_resume(
        &self,
        handle: SdkHandle,
        params: HarnessResumeParams,
    ) -> Result<serde_json::Value, SdkError> {
        // Restore the opaque conversation state from the checkpoint bytes
        // the host embedded, then continue with any replayed inputs.
        use base64::Engine as _;
        let payload = params
            .state_base64
            .ok_or_else(|| SdkError::Fault("checkpoint bytes missing from resume params".into()))?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&payload)
            .map_err(|e| SdkError::Fault(format!("bad checkpoint: {e}")))?;
        let restored: ConversationState = serde_json::from_slice(&bytes)
            .map_err(|e| SdkError::Fault(format!("bad checkpoint state: {e}")))?;
        let config = self.config.lock().await.clone();
        let mut state = self.state.lock().await;
        *state = restored;
        if let Some(input) = params.replay_inputs.first() {
            let result = crate::loop_engine::run_loop(&handle, &config, &mut state, &input.text)
                .await
                .map_err(|error| SdkError::Fault(error.to_string()))?;
            return Ok(serde_json::to_value(&result).unwrap_or_default());
        }
        Ok(serde_json::json!({"resumed": true, "replayed": 0}))
    }

    async fn on_steer(&self, handle: SdkHandle, params: HarnessSteerParams) {
        // Steer between turns: queue the text as the next user input and
        // persist immediately (the crash-resume point must include it).
        let mut state = self.state.lock().await;
        crate::request_projection::push_user(&mut state, &params.input.text);
        let payload = serde_json::to_vec(&*state).unwrap_or_default();
        let input_seq = params.input.input_seq;
        drop(state);
        let _ = handle.save_checkpoint(&payload, input_seq).await;
        let _ = handle
            .emit_event(
                EventKind::Progress,
                serde_json::json!({"queuedSteer": params.input.text}),
            )
            .await;
    }
}
