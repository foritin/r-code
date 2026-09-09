//! Platform process guardians for owned child trees.
//!
//! Windows uses kill-on-close Job Objects (T14a); Unix uses a daemon-EOF
//! guardian owning managed process groups (T14b). Guardians record owner and
//! process-start identity and never reason from PID alone.

#[cfg(unix)]
pub mod unix;
#[cfg(windows)]
pub mod windows;

use serde::{Deserialize, Serialize};

/// Owner/start identity persisted with every guarded tree. Recovery compares
/// the full identity, never a bare PID (PIDs get reused).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GuardedOwnerIdentity {
    pub pid: u32,
    pub start_identity: u64,
    pub boot_nonce: String,
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
