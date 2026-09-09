//! Codex delegation and resume (T31).
//!
//! In-session dynamic delegation maps onto host.children; App Server
//! thread references persist in versioned checkpoints and resume only the
//! same harness package/config identity. External thread unavailability
//! surfaces as a visible restart-required outcome.

use r_code_harness_protocol::services::{ChildReport, PermissionCeiling};
use r_code_harness_sdk::{SdkError, SdkHandle};

/// One delegated Codex child (possibly onto another harness).
pub async fn delegate(
    handle: &SdkHandle,
    objective: &str,
    harness: Option<&str>,
) -> Result<ChildReport, SdkError> {
    let spawn = handle
        .spawn_child(objective, harness, PermissionCeiling::ApprovalRequired)
        .await?;
    match handle.wait_child(&spawn.child_task_id).await {
        Ok(report) => Ok(report),
        Err(error) => {
            let _ = handle.cancel_child(&spawn.child_task_id).await;
            Err(error)
        }
    }
}

/// The persisted resume reference: App Server thread + package identity.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ThreadResumeRef {
    pub harness_id: String,
    pub package_digest: String,
    pub config_hash: String,
    pub thread_id: String,
}

/// Checkpoint payload for the Codex plugin (opaque to the host).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CodexCheckpoint {
    pub resume: Option<ThreadResumeRef>,
    pub consumed_input_seq: u64,
}

/// Resume validation: only the same package/config identity may continue;
/// a missing external thread is a visible restart-required outcome.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "outcome", rename_all = "kebab-case")]
pub enum ResumeDecision {
    Resume { thread_id: String },
    RestartRequired { reason: String },
}

pub fn decide_resume(
    checkpoint: &CodexCheckpoint,
    current_harness_id: &str,
    current_package_digest: &str,
    current_config_hash: &str,
    external_thread_available: bool,
) -> ResumeDecision {
    let Some(resume) = &checkpoint.resume else {
        return ResumeDecision::RestartRequired {
            reason: "checkpoint carries no thread reference".into(),
        };
    };
    if resume.harness_id != current_harness_id
        || resume.package_digest != current_package_digest
        || resume.config_hash != current_config_hash
    {
        return ResumeDecision::RestartRequired {
            reason: "harness package/config identity changed".into(),
        };
    }
    if !external_thread_available {
        return ResumeDecision::RestartRequired {
            reason: format!("external thread {} unavailable", resume.thread_id),
        };
    }
    ResumeDecision::Resume {
        thread_id: resume.thread_id.clone(),
    }
}
