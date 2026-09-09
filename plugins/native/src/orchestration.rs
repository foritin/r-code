//! Native delegation, review and strategy budgets (T28).
//!
//! Delegation routes sub-investigations to child harnesses through
//! `host.children.*`; report collection keeps verified / inferred /
//! unverifiable distinct; the reviewer packet cites host evidence only.
//! Strategy defaults live in [`LoopConfig`] (plugin-owned), while root
//! budgets stay host-owned — the plugin never writes terminal state or
//! checkpoints of other tasks.

use r_code_harness_protocol::services::{ChildReport, PermissionCeiling};
use r_code_harness_sdk::{SdkError, SdkHandle};

/// A delegation request shaped by the Native strategy.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DelegationRequest {
    pub objective: String,
    /// A different harness for the child (cross-harness delegation).
    pub harness: Option<String>,
    /// Bounded retries on repair feedback.
    pub max_retries: u32,
}

impl Default for DelegationRequest {
    fn default() -> Self {
        Self {
            objective: String::new(),
            harness: None,
            max_retries: 2,
        }
    }
}

/// Delegate one child investigation and collect its report.
pub async fn delegate(
    handle: &SdkHandle,
    request: &DelegationRequest,
) -> Result<ChildReport, SdkError> {
    let spawn = handle
        .spawn_child(
            &request.objective,
            request.harness.as_deref(),
            PermissionCeiling::ApprovalRequired,
        )
        .await?;
    // Poll-wait with bounded retries on transient protocol errors.
    let mut attempts = 0;
    loop {
        match handle.wait_child(&spawn.child_task_id).await {
            Ok(report) => return Ok(report),
            Err(error) => {
                attempts += 1;
                if attempts > request.max_retries {
                    let _ = handle.cancel_child(&spawn.child_task_id).await;
                    return Err(error);
                }
            }
        }
    }
}

/// Build the reviewer packet from child reports: verified facts and
/// inferred/unverifiable statements stay separated; no invented evidence.
pub fn reviewer_packet(reports: &[ChildReport]) -> serde_json::Value {
    let verified: Vec<&str> = reports
        .iter()
        .flat_map(|report| report.verified.iter().map(String::as_str))
        .collect();
    let inferred: Vec<&str> = reports
        .iter()
        .flat_map(|report| report.inferred.iter().map(String::as_str))
        .collect();
    let unverifiable: Vec<&str> = reports
        .iter()
        .flat_map(|report| report.unverifiable.iter().map(String::as_str))
        .collect();
    serde_json::json!({
        "verified": verified,
        "inferred": inferred,
        "unverifiable": unverifiable,
        "reportCount": reports.len(),
    })
}
