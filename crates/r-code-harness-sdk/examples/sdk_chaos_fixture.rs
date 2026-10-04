//! A07 test fixture: a harness that blocks inside a long host_call so tests
//! can drive EOF / cancel races over real stdio.
//!
//! - `on_start` issues a 300s `host.model.stream` call (the test host never
//!   replies) — closing stdin must fail it fast via the EOF drain.
//! - `on_cancel` acknowledges.

use r_code_harness_protocol::services::{
    HarnessResumeParams, HarnessStartParams, HarnessSteerParams, InitializeParams, InitializeResult,
};
use r_code_harness_protocol::EventKind;
use r_code_harness_sdk::{HarnessHandlers, SdkError, SdkHandle};
use std::time::Duration;

pub struct ChaosFixture;

#[async_trait::async_trait]
impl HarnessHandlers for ChaosFixture {
    async fn on_initialize(&self, _params: InitializeParams) -> Result<InitializeResult, SdkError> {
        Ok(InitializeResult {
            harness_id: "chaos.a07".into(),
            harness_version: env!("CARGO_PKG_VERSION").into(),
            ready_checkpoint: None,
        })
    }

    async fn on_start(
        &self,
        handle: SdkHandle,
        _params: HarnessStartParams,
    ) -> Result<serde_json::Value, SdkError> {
        // Long host call: held until EOF drain / cancel / 300s (whichever
        // first). The reply is intentionally ignored — the observable is how
        // FAST this handler returns.
        let outcome = handle
            .host_call(
                "host.model.stream",
                serde_json::json!({"messages": []}),
                Duration::from_secs(300),
            )
            .await;
        let _ = handle
            .emit_event(
                EventKind::Progress,
                serde_json::json!({"hostCallOutcome": outcome.is_ok()}),
            )
            .await;
        Ok(serde_json::json!({"startSettled": true}))
    }

    async fn on_resume(
        &self,
        _handle: SdkHandle,
        _params: HarnessResumeParams,
    ) -> Result<serde_json::Value, SdkError> {
        Ok(serde_json::json!({"resumed": true}))
    }

    async fn on_steer(&self, _handle: SdkHandle, _params: HarnessSteerParams) {}
}

#[tokio::main]
async fn main() {
    let result = r_code_harness_sdk::serve(ChaosFixture).await;
    if let Err(error) = result {
        eprintln!("chaos fixture serve error: {error}");
        std::process::exit(1);
    }
}
