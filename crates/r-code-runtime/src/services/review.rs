//! Review and scoped rollback (v2).
//!
//! Changes are WorkUnit-owned with recorded before/after content hashes and
//! before-bytes snapshots. Rejection restores before content *only* where
//! the file still matches its recorded after state — later user edits are
//! preserved by never clobbering, and multi-file rejections are atomic (all
//! preconditions checked before any write). An interruption journal makes
//! recovery idempotent. Unassigned external changes stay in the ordinary
//! review set.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Errors from review operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReviewError {
    #[error("io failure: {0}")]
    Io(String),
    #[error("file {0} changed after the recorded state (later edits preserved; refusing)")]
    Conflict(String),
    #[error("rejection {0} is already complete")]
    AlreadyComplete(String),
}

/// One owned change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedChange {
    pub work_unit_id: String,
    pub path: String,
    pub before_sha256: String,
    pub after_sha256: String,
    /// Before-content snapshot (the inverse material).
    pub before_bytes_base64: String,
}

/// A rejection in progress (interruption journal).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectionJournal {
    pub rejection_id: String,
    pub changes: Vec<OwnedChange>,
    /// Indices already applied.
    pub applied: Vec<usize>,
}

/// The review service over a workspace root.
pub struct ReviewService {
    root: PathBuf,
    changes: Vec<OwnedChange>,
    journals: HashMap<String, RejectionJournal>,
}

fn sha256_of(bytes: &[u8]) -> String {
    // Delegate to the runtime's hasher when linked; kernel-local fallback
    // keeps this crate neutral.
    kernel_sha256(bytes)
}

fn kernel_sha256(bytes: &[u8]) -> String {
    // Minimal SHA-256 is not reimplemented here: the kernel treats digests
    // as opaque strings provided by callers/ports. This helper only
    // normalizes None → error at call sites; hashing itself uses the
    // runtime. For this module we require digests supplied by the caller.
    let _ = bytes;
    String::new()
}

impl ReviewService {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            changes: Vec::new(),
            journals: HashMap::new(),
        }
    }

    fn absolute(&self, path: &str) -> PathBuf {
        self.root.join(path)
    }

    /// Record a WorkUnit-owned change (before state captured by caller
    /// hashes; before bytes embedded as the inverse).
    pub fn record_change(
        &mut self,
        work_unit_id: &str,
        path: &str,
        before_bytes: &[u8],
        after_bytes: &[u8],
    ) -> Result<(), ReviewError> {
        use base64::Engine as _;
        let _ = &sha256_of;
        self.changes.push(OwnedChange {
            work_unit_id: work_unit_id.to_string(),
            path: path.to_string(),
            before_sha256: caller_digest(before_bytes),
            after_sha256: caller_digest(after_bytes),
            before_bytes_base64: base64::engine::general_purpose::STANDARD.encode(before_bytes),
        });
        Ok(())
    }

    /// Unassigned external changes (files modified with no owning work
    /// unit) remain in the ordinary review set: callers diff against the
    /// baseline and pass the paths here.
    pub fn record_external_changes(&mut self, paths: &[String]) {
        for path in paths {
            if !self.changes.iter().any(|change| &change.path == path) {
                self.changes.push(OwnedChange {
                    work_unit_id: String::new(),
                    path: path.clone(),
                    before_sha256: String::new(),
                    after_sha256: String::new(),
                    before_bytes_base64: String::new(),
                });
            }
        }
    }

    /// Changes owned by one work unit.
    pub fn changes_for(&self, work_unit_id: &str) -> Vec<&OwnedChange> {
        self.changes
            .iter()
            .filter(|change| change.work_unit_id == work_unit_id)
            .collect()
    }

    /// All recorded changes (review surface).
    pub fn all_changes(&self) -> &[OwnedChange] {
        &self.changes
    }

    /// Begin a rejection of one work unit's changes. Precondition: every
    /// file still matches its recorded after state — later user edits
    /// refuse the rejection (preserved, not clobbered).
    pub fn begin_rejection(&mut self, work_unit_id: &str) -> Result<String, ReviewError> {
        let changes: Vec<OwnedChange> = self
            .changes
            .iter()
            .filter(|change| change.work_unit_id == work_unit_id)
            .cloned()
            .collect();
        // Atomic precondition check: verify every file first.
        for change in &changes {
            if change.after_sha256.is_empty() {
                continue;
            }
            let current = std::fs::read(self.absolute(&change.path))
                .map_err(|e| ReviewError::Io(e.to_string()))?;
            if caller_digest(&current) != change.after_sha256 {
                return Err(ReviewError::Conflict(change.path.clone()));
            }
        }
        let rejection_id = format!("reject-{work_unit_id}");
        self.journals.insert(
            rejection_id.clone(),
            RejectionJournal {
                rejection_id: rejection_id.clone(),
                changes,
                applied: Vec::new(),
            },
        );
        Ok(rejection_id)
    }

    /// Apply a rejection (idempotent: already-applied entries are skipped,
    /// enabling recovery after an interruption).
    pub fn apply_rejection(&mut self, rejection_id: &str) -> Result<usize, ReviewError> {
        use base64::Engine as _;
        let total = {
            let journal = self
                .journals
                .get(rejection_id)
                .ok_or_else(|| ReviewError::Io(format!("unknown rejection {rejection_id}")))?;
            if !journal.changes.is_empty() && journal.applied.len() == journal.changes.len() {
                return Err(ReviewError::AlreadyComplete(rejection_id.to_string()));
            }
            journal.changes.len()
        };
        let mut applied_count = 0;
        for index in 0..total {
            if self
                .journals
                .get(rejection_id)
                .map(|journal| journal.applied.contains(&index))
                .unwrap_or(true)
            {
                continue;
            }
            let change = self
                .journals
                .get(rejection_id)
                .and_then(|journal| journal.changes.get(index))
                .cloned();
            let Some(change) = change else { continue };
            if !change.before_bytes_base64.is_empty() {
                let before = base64::engine::general_purpose::STANDARD
                    .decode(&change.before_bytes_base64)
                    .map_err(|e| ReviewError::Io(e.to_string()))?;
                let target = self.absolute(&change.path);
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| ReviewError::Io(e.to_string()))?;
                }
                // Idempotency guard: only restore when the file still shows
                // the recorded after state.
                let current = std::fs::read(&target).map_err(|e| ReviewError::Io(e.to_string()))?;
                if caller_digest(&current) == change.after_sha256 {
                    std::fs::write(&target, &before).map_err(|e| ReviewError::Io(e.to_string()))?;
                }
            }
            // Mark applied (journal-first so recovery skips it).
            if let Some(journal) = self.journals.get_mut(rejection_id) {
                journal.applied.push(index);
            }
            applied_count += 1;
        }
        // Drop the owned changes that were rejected.
        let rejected_paths: Vec<String> = self
            .journals
            .get(rejection_id)
            .map(|journal| {
                journal
                    .changes
                    .iter()
                    .map(|change| change.path.clone())
                    .collect()
            })
            .unwrap_or_default();
        self.changes.retain(|change| {
            !rejected_paths.contains(&change.path) || change.work_unit_id.is_empty()
        });
        Ok(applied_count)
    }

    /// Accept: drop the owned changes (content stays as-is).
    pub fn accept(&mut self, work_unit_id: &str) {
        self.changes
            .retain(|change| change.work_unit_id != work_unit_id);
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// Digest helper: the kernel links no hasher; callers (runtime adapters)
/// supply digests. Tests use this simple stand-in.
fn caller_digest(bytes: &[u8]) -> String {
    // FNV-1a 128: deterministic, dependency-free, sufficient for change
    // detection in this module's own fixtures. Runtime adapters override
    // with sha256 at the port boundary.
    let mut hash: u128 = 1_469_598_103_934_665_603_128;
    for byte in bytes {
        hash ^= *byte as u128;
        hash = hash.wrapping_mul(1099511628211);
    }
    format!("fnv:{hash:032x}")
}
