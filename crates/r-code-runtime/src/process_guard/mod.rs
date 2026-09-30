//! Platform process guardians for owned child trees.
//!
//! Windows uses kill-on-close Job Objects (T14a); Unix uses a daemon-EOF
//! guardian owning managed process groups (T14b). Guardians record owner and
//! process-start identity and never reason from PID alone.

pub mod boot;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(unix)]
pub mod unix;
#[cfg(windows)]
pub mod windows;

pub use boot::{BootIdentity, BootIdentityError, BootIdentitySource};

use serde::{Deserialize, Serialize};

/// Owner/start identity persisted with every guarded tree. Recovery compares
/// the full identity, never a bare PID (PIDs get reused).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuardedOwnerIdentity {
    pub pid: u32,
    pub start_identity: u64,
    pub boot_nonce: String,
}

/// Portable owner identity persisted for recovery and fencing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessOwnerIdentity {
    pub pid: u32,
    pub start_identity: u64,
    pub boot_identity: BootIdentity,
    pub platform_identity: serde_json::Value,
    pub platform_identity_digest: String,
}

impl ProcessOwnerIdentity {
    pub fn new(
        pid: u32,
        start_identity: u64,
        boot_identity: BootIdentity,
        platform_identity: serde_json::Value,
    ) -> Result<Self, &'static str> {
        if pid == 0 || start_identity == 0 || platform_identity.is_null() {
            return Err("process owner identity is incomplete");
        }
        let platform_identity_digest =
            r_code_harness_protocol::canonical_input_hash(&platform_identity);
        Ok(Self {
            pid,
            start_identity,
            boot_identity,
            platform_identity,
            platform_identity_digest,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProcessTreeState {
    Prepared,
    Running,
    Terminating,
    Exited,
    Quarantined,
    LegacyUnverifiable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessTreeRecord {
    pub tree_id: String,
    pub attempt_id: String,
    pub workspace_key: String,
    pub profile_id: String,
    pub owner: ProcessOwnerIdentity,
    pub ownership_epoch: u64,
    pub state: ProcessTreeState,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migrated_observed_boot_identity: Option<BootIdentity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TerminationProofKind {
    Exit,
    Reboot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminationProofRecord {
    pub proof_id: String,
    pub tree_id: String,
    pub ownership_epoch: u64,
    pub kind: TerminationProofKind,
    pub observed_boot_identity: BootIdentity,
    pub proof_identity: serde_json::Value,
    pub proof_identity_digest: String,
    pub recorded_at_ms: i64,
}

/// Termination proof outcome for write-barrier decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminationProof {
    /// All known processes verifiably exited.
    Confirmed,
    /// Could not prove termination: block writes, surface as blocked.
    Unverifiable,
}

impl From<bool> for TerminationProof {
    fn from(confirmed: bool) -> Self {
        if confirmed {
            TerminationProof::Confirmed
        } else {
            TerminationProof::Unverifiable
        }
    }
}
