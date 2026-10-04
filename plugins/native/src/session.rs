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
        // host-owned (we only read the objective text). A06: a missing or
        // non-string objective is a typed fault — running a whole loop on an
        // empty objective wastes the budget and ends in a meaningless
        // proposal.
        let objective = params
            .contract
            .get("objective")
            .and_then(|value| value.as_str())
            .filter(|text| !text.trim().is_empty())
            .ok_or_else(|| {
                SdkError::Fault("contract.objective missing, not a string, or blank".into())
            })?
            .to_string();
        let config = self.config.lock().await.clone();
        // A08：置默认后立即放锁——run_loop 以共享锁短作用域访问状态，
        // on_steer 可在轮间落地。
        *self.state.lock().await = ConversationState::default();
        let result = crate::loop_engine::run_loop(&handle, &config, &self.state, &objective)
            .await
            .map_err(|error| SdkError::Fault(error.to_string()))?;
        // A06：遥测失败不得把已完成的 run 变失败（宿主可能重试已完成任务）。
        if let Err(error) = handle
            .emit_event(
                EventKind::Progress,
                serde_json::json!({
                    "turns": result.turns,
                    "toolCalls": result.tool_calls_executed,
                    "stopReason": result.stop_reason,
                }),
            )
            .await
        {
            eprintln!("post-run progress emit failed (ignored): {error}");
        }
        // A06：编码失败返回 Fault 而非 Ok(Null)。
        serde_json::to_value(&result)
            .map_err(|error| SdkError::Fault(format!("encode loop result: {error}")))
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
        *self.state.lock().await = restored;
        if let Some(input) = params.replay_inputs.first() {
            let result = crate::loop_engine::run_loop(&handle, &config, &self.state, &input.text)
                .await
                .map_err(|error| SdkError::Fault(error.to_string()))?;
            return serde_json::to_value(&result)
                .map_err(|error| SdkError::Fault(format!("encode loop result: {error}")));
        }
        Ok(serde_json::json!({"resumed": true, "replayed": 0}))
    }

    async fn on_steer(&self, handle: SdkHandle, params: HarnessSteerParams) {
        // Steer between turns: queue the text as the next user input and
        // persist immediately (the crash-resume point must include it).
        // A06：state 锁跨越 save_checkpoint 持有——并发 steer 必须按变更序
        // 落盘，旧快照后提交会丢输入。
        let mut state = self.state.lock().await;
        crate::request_projection::push_user(&mut state, &params.input.text);
        // A06：编码失败不得降级为空载荷落盘（会覆盖好 checkpoint）。
        let payload = match serde_json::to_vec(&*state) {
            Ok(payload) => payload,
            Err(error) => {
                eprintln!("steer checkpoint encode failed (kept in memory): {error}");
                return;
            }
        };
        let input_seq = params.input.input_seq;
        let _ = handle.save_checkpoint(&payload, input_seq).await;
        drop(state);
        let _ = handle
            .emit_event(
                EventKind::Progress,
                serde_json::json!({"queuedSteer": params.input.text}),
            )
            .await;
    }
}
