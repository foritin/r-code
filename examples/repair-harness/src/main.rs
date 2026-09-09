//! Example third-party harness: plan-then-repair.
//!
//! Workflow on `harness.start`:
//! 1. list the host tools (planning inputs);
//! 2. read the target file through `host.tools.call` with an attempt-stable
//!    operation key;
//! 3. emit a plan progress event;
//! 4. save a checkpoint of the plugin's opaque state;
//! 5. propose completion for host arbitration.
//!
//! The binary links only the SDK and the public protocol — no host runtime,
//! storage, gateway or Tauri code.

use r_code_harness_protocol::services::*;
use r_code_harness_protocol::{EventKind, ProposalKind};
use r_code_harness_sdk::{serve, HarnessHandlers, SdkError, SdkHandle};

struct RepairHarness;

#[async_trait::async_trait]
impl HarnessHandlers for RepairHarness {
    async fn on_initialize(&self, params: InitializeParams) -> Result<InitializeResult, SdkError> {
        Ok(InitializeResult {
            harness_id: "repair-harness.example".into(),
            harness_version: env!("CARGO_PKG_VERSION").into(),
            ready_checkpoint: None,
        })
        .inspect(|_result| {
            let _ = params.identity.run_id.len();
        })
    }

    async fn on_start(
        &self,
        handle: SdkHandle,
        params: HarnessStartParams,
    ) -> Result<serde_json::Value, SdkError> {
        // 1. Discover the host tools available to this run.
        let tools = handle.tools_list().await?;
        handle
            .emit_event(
                EventKind::Progress,
                serde_json::json!({"stage": "planning", "tools": tools.tools.len()}),
            )
            .await?;

        // 2. Inspect the target file (plan-then-repair: look before fixing).
        let target = params
            .contract
            .get("objective")
            .and_then(|objective| objective.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| "README.md".into());
        let inspected = handle
            .tools_call(
                "read_file",
                serde_json::json!({"path": target}),
                Some("inspect-target"),
            )
            .await?;
        let preview = inspected
            .output
            .iter()
            .find_map(|block| match block {
                r_code_harness_protocol::OutputBlock::Text { text } => {
                    Some(text.chars().take(64).count())
                }
                _ => None,
            })
            .unwrap_or(0);

        // 3. Publish the repair plan as progress.
        handle
            .emit_event(
                EventKind::PlanUpdated,
                serde_json::json!({
                    "plan": [
                        {"id": "inspect", "description": "inspect the target file"},
                        {"id": "repair", "description": "apply the repair"},
                    ],
                    "inspectedBytes": preview,
                }),
            )
            .await?;

        // 4. Checkpoint the plugin's opaque state.
        let state = format!("repair-plan:{}", target).into_bytes();
        let checkpoint = handle.save_checkpoint(&state, 1).await?;

        // 5. Ask the host to arbitrate completion of this reply/plan.
        let decision = handle
            .propose_completion(CompletionProposalRequest {
                kind: ProposalKind::PlanDraft,
                summary: format!("repair plan for {target} ready"),
                candidate_digest: None,
                work_unit_statuses: vec![],
            })
            .await?;

        Ok(serde_json::json!({
            "started": true,
            "checkpoint": checkpoint.checkpoint.blob_id,
            "proposalAccepted": decision.accepted,
        }))
    }

    async fn on_steer(&self, handle: SdkHandle, params: HarnessSteerParams) {
        let _ = handle
            .emit_event(
                EventKind::Progress,
                serde_json::json!({"steered": params.input.text}),
            )
            .await;
    }
}

#[tokio::main]
async fn main() -> Result<(), SdkError> {
    serve(RepairHarness).await
}
