//! Review and scoped rollback (v1).
//!
//! Changes are WorkUnit-owned with recorded before/after content hashes and
//! before-bytes snapshots. Rejection restores before content *only* where
//! the file still matches its recorded after state — later user edits are
//! preserved by never clobbering, and multi-file rejections are atomic (all
//! preconditions checked before any write). An interruption journal makes
//! recovery idempotent. Unassigned external changes stay in the ordinary
//! review set.

use crate::services::artifacts::{sha256_hex, ArtifactStore};
use crate::services::workspaces::{CandidateManifest, TaskWorkspaceBinding};
use r_code_core::security::PathGuard;
use r_code_harness_protocol::{canonical_input_hash, ArtifactRef};
use r_code_kernel::task::{TaskExecution, TaskState, WorkUnitStatus};
use r_code_store::v1::{LeaseRequest, MutationFile, MutationOperation, MutationState, V1Store};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Errors from review operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReviewError {
    #[error("io failure: {0}")]
    Io(String),
    #[error("file {0} changed after the recorded state (later edits preserved; refusing)")]
    Conflict(String),
    #[error("rejection {0} is already complete")]
    AlreadyComplete(String),
    #[error("review is unavailable for the current task state")]
    InvalidState,
    #[error("review candidate or mutation journal is stale")]
    Stale,
    #[error("durable review store failure")]
    Store,
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
    sha256_hex(bytes)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DurableReviewView {
    pub task_id: String,
    pub task_revision: u64,
    pub attempt_id: String,
    pub work_unit_id: String,
    pub candidate_digest: String,
    pub stale: bool,
    pub conflict: bool,
    pub changes: Vec<DurableReviewChange>,
    pub checks: Vec<DurableReviewCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DurableReviewChange {
    pub operation_id: String,
    pub path: String,
    pub before_sha256: Option<String>,
    pub after_sha256: Option<String>,
    pub current_sha256: Option<String>,
    pub current_matches_after: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DurableReviewCheck {
    pub check_id: String,
    pub passed: bool,
    pub definition_identity: Option<String>,
    pub environment_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RollbackOutcome {
    pub restored_paths: Vec<String>,
    pub already_reverted_paths: Vec<String>,
}

pub struct DurableReviewService {
    store: Arc<V1Store>,
    binding: TaskWorkspaceBinding,
    guard: PathGuard,
    artifacts: Arc<ArtifactStore>,
    task_id: String,
    attempt_id: String,
    work_unit_id: String,
    candidate_digest: String,
}

impl DurableReviewService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Arc<V1Store>,
        binding: TaskWorkspaceBinding,
        artifacts: Arc<ArtifactStore>,
        task_id: impl Into<String>,
        attempt_id: impl Into<String>,
        work_unit_id: impl Into<String>,
        candidate_digest: impl Into<String>,
    ) -> Result<Self, ReviewError> {
        let task_id = task_id.into();
        if binding.task_id != task_id || artifacts.task_id() != Some(task_id.as_str()) {
            return Err(ReviewError::InvalidState);
        }
        let guard = PathGuard::new(binding.canonical_root.clone())
            .map_err(|_| ReviewError::InvalidState)?;
        Ok(Self {
            store,
            binding,
            guard,
            artifacts,
            task_id,
            attempt_id: attempt_id.into(),
            work_unit_id: work_unit_id.into(),
            candidate_digest: candidate_digest.into(),
        })
    }

    pub fn project(
        &self,
        state: &TaskState,
        task_revision: u64,
    ) -> Result<DurableReviewView, ReviewError> {
        self.validate_state(state)?;
        let operations = self.operations()?;
        let mut chains = BTreeMap::<String, DurableReviewChange>::new();
        let mut conflict = false;
        for operation in operations {
            if operation.state != MutationState::Receipted {
                conflict = true;
            }
            for file in operation.files {
                chains
                    .entry(file.logical_path.clone())
                    .and_modify(|change| {
                        change.operation_id = operation.operation_id.clone();
                        change.after_sha256 = file.after_sha256.clone();
                    })
                    .or_insert(DurableReviewChange {
                        operation_id: operation.operation_id.clone(),
                        path: file.logical_path,
                        before_sha256: file.before_sha256,
                        after_sha256: file.after_sha256,
                        current_sha256: None,
                        current_matches_after: false,
                    });
            }
        }
        let mut changes = chains.into_values().collect::<Vec<_>>();
        for change in &mut changes {
            change.current_sha256 = self.current_sha(&change.path)?;
            change.current_matches_after = change.current_sha256 == change.after_sha256;
            conflict |= !change.current_matches_after;
        }
        let candidate_current = CandidateManifest::capture(&self.binding)
            .map(|manifest| manifest.candidate_id == self.candidate_digest)
            .unwrap_or(false);
        let required = required_checks(state, &self.work_unit_id);
        let checks = required
            .into_iter()
            .map(|check_id| {
                let evidence = state.evidence.iter().find(|record| {
                    record.task_id == self.task_id
                        && record.check_id == check_id
                        && record.candidate_digest == self.candidate_digest
                        && record.passed
                        && matches!(
                            record.recorded_by,
                            r_code_harness_protocol::Provenance::Host
                        )
                        && !record.definition_identity.is_empty()
                        && !record.environment_fingerprint.is_empty()
                });
                DurableReviewCheck {
                    check_id,
                    passed: evidence.is_some(),
                    definition_identity: evidence.map(|record| record.definition_identity.clone()),
                    environment_fingerprint: evidence
                        .map(|record| record.environment_fingerprint.clone()),
                }
            })
            .collect();
        Ok(DurableReviewView {
            task_id: self.task_id.clone(),
            task_revision,
            attempt_id: self.attempt_id.clone(),
            work_unit_id: self.work_unit_id.clone(),
            candidate_digest: self.candidate_digest.clone(),
            stale: !candidate_current || conflict,
            conflict,
            changes,
            checks,
        })
    }

    pub fn candidate_is_current(&self) -> bool {
        CandidateManifest::capture(&self.binding)
            .map(|manifest| manifest.candidate_id == self.candidate_digest)
            .unwrap_or(false)
    }

    pub fn rollback(&self, action_id: &str) -> Result<RollbackOutcome, ReviewError> {
        if action_id.trim().is_empty() {
            return Err(ReviewError::InvalidState);
        }
        let operations = self.operations()?;
        let mut virtual_state = BTreeMap::<String, Option<String>>::new();
        let mut initial_state = BTreeMap::<String, Option<String>>::new();
        for operation in &operations {
            for file in &operation.files {
                initial_state
                    .entry(file.logical_path.clone())
                    .or_insert_with(|| file.before_sha256.clone());
            }
        }
        let mut fully_reverted = BTreeSet::new();
        for (path, initial) in &initial_state {
            if &self.current_sha(path)? == initial {
                fully_reverted.insert(path.clone());
            }
        }
        let mut steps = Vec::<(String, MutationFile, bool)>::new();
        for operation in operations.iter().rev() {
            if operation.state != MutationState::Receipted {
                return Err(ReviewError::Stale);
            }
            for file in operation.files.iter().rev() {
                let current = match virtual_state.get(&file.logical_path) {
                    Some(current) => current.clone(),
                    None => self.current_sha(&file.logical_path)?,
                };
                let already =
                    fully_reverted.contains(&file.logical_path) || current == file.before_sha256;
                if !already && current != file.after_sha256 {
                    return Err(ReviewError::Conflict(file.logical_path.clone()));
                }
                virtual_state.insert(file.logical_path.clone(), file.before_sha256.clone());
                steps.push((operation.operation_id.clone(), file.clone(), already));
            }
        }
        let paths = steps
            .iter()
            .map(|(_, file, _)| file.logical_path.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let physical_preflight = paths
            .iter()
            .map(|path| {
                self.binding
                    .resolve_path(path)
                    .map(|resolved| (path.clone(), resolved))
                    .map_err(|_| ReviewError::Conflict(path.clone()))
            })
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        let already_reverted_paths = steps
            .iter()
            .filter(|(_, _, already)| *already)
            .map(|(_, file, _)| file.logical_path.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let workspace_key = format!(
            "sha256:{}",
            sha256_hex(self.binding.canonical_root.to_string_lossy().as_bytes())
        );
        let owner = format!("review:{}:{action_id}", self.task_id);
        let lease = self
            .store
            .acquire_lease(LeaseRequest {
                workspace_key: workspace_key.clone(),
                operation_id: format!("review-rollback:{action_id}"),
                owner_id: owner.clone(),
                read_paths: Vec::new(),
                write_paths: paths,
                repo_exclusive: false,
            })
            .map_err(|_| ReviewError::Store)?;
        if !lease.active {
            if steps.iter().all(|(_, _, already)| *already)
                && self.inverse_operations_receipted(action_id, &steps)?
            {
                return Ok(RollbackOutcome {
                    restored_paths: Vec::new(),
                    already_reverted_paths,
                });
            }
            return Err(ReviewError::Store);
        }
        if is_sha256_digest(&self.candidate_digest) && !self.candidate_is_current() {
            let _ = self
                .store
                .release_lease(&lease.lease_id, &owner, lease.fencing_epoch);
            return Err(ReviewError::Conflict(
                "candidate identity changed before rollback".to_string(),
            ));
        }
        for (path, resolved) in &physical_preflight {
            if resolved.revalidate().is_err() {
                let _ = self
                    .store
                    .release_lease(&lease.lease_id, &owner, lease.fencing_epoch);
                return Err(ReviewError::Conflict(path.clone()));
            }
        }
        // The first preflight happened before lease acquisition. Re-run the
        // complete virtual chain under the acquired path set before applying
        // the first inverse effect, so an external writer cannot cause a
        // late-path conflict after earlier paths were already restored.
        let mut post_virtual = BTreeMap::<String, Option<String>>::new();
        let mut post_fully_reverted = BTreeSet::new();
        for (path, initial) in &initial_state {
            if &self.current_sha(path)? == initial {
                post_fully_reverted.insert(path.clone());
            }
        }
        for (_, file, already) in &mut steps {
            let current = match post_virtual.get(&file.logical_path) {
                Some(current) => current.clone(),
                None => self.current_sha(&file.logical_path)?,
            };
            *already =
                post_fully_reverted.contains(&file.logical_path) || current == file.before_sha256;
            if !*already && current != file.after_sha256 {
                let _ = self
                    .store
                    .release_lease(&lease.lease_id, &owner, lease.fencing_epoch);
                return Err(ReviewError::Conflict(file.logical_path.clone()));
            }
            post_virtual.insert(file.logical_path.clone(), file.before_sha256.clone());
        }
        let already_reverted_paths = steps
            .iter()
            .filter(|(_, _, already)| *already)
            .map(|(_, file, _)| file.logical_path.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if steps.iter().all(|(_, _, already)| *already) {
            self.reconcile_inverse_operations(action_id, &steps, &owner, &lease)?;
            let released = self
                .store
                .release_lease(&lease.lease_id, &owner, lease.fencing_epoch)
                .map_err(|_| ReviewError::Store)?;
            if !released {
                return Err(ReviewError::Store);
            }
            return Ok(RollbackOutcome {
                restored_paths: Vec::new(),
                already_reverted_paths,
            });
        }
        let result = (|| {
            let mut restored_paths = Vec::new();
            for (index, (forward_id, file, already)) in steps.into_iter().enumerate() {
                if already {
                    continue;
                }
                let logical_path = file.logical_path.clone();
                self.rollback_file(
                    action_id,
                    index,
                    &forward_id,
                    file,
                    &workspace_key,
                    &owner,
                    &lease,
                )?;
                restored_paths.push(logical_path);
            }
            Ok(RollbackOutcome {
                restored_paths,
                already_reverted_paths,
            })
        })();
        if matches!(
            &result,
            Ok(_) | Err(ReviewError::Conflict(_)) | Err(ReviewError::Stale)
        ) {
            let released = self
                .store
                .release_lease(&lease.lease_id, &owner, lease.fencing_epoch)
                .map_err(|_| ReviewError::Store)?;
            if !released {
                return Err(ReviewError::Store);
            }
        }
        result
    }

    fn reconcile_inverse_operations(
        &self,
        action_id: &str,
        steps: &[(String, MutationFile, bool)],
        owner: &str,
        lease: &r_code_store::v1::LeaseGrant,
    ) -> Result<(), ReviewError> {
        for (index, (forward_id, file, _)) in steps.iter().enumerate() {
            self.rollback_file(
                action_id,
                index,
                forward_id,
                file.clone(),
                &lease.request.workspace_key,
                owner,
                lease,
            )?;
        }
        Ok(())
    }

    fn inverse_operations_receipted(
        &self,
        action_id: &str,
        steps: &[(String, MutationFile, bool)],
    ) -> Result<bool, ReviewError> {
        for (index, (forward_id, _, _)) in steps.iter().enumerate() {
            let operation_id = inverse_operation_id(action_id, index, forward_id);
            let operation = self
                .store
                .load_mutation_operation(&operation_id)
                .map_err(|_| ReviewError::Store)?;
            if !operation.is_some_and(|operation| operation.state == MutationState::Receipted) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    #[allow(clippy::too_many_arguments)]
    fn rollback_file(
        &self,
        action_id: &str,
        index: usize,
        forward_id: &str,
        file: MutationFile,
        workspace_key: &str,
        owner: &str,
        lease: &r_code_store::v1::LeaseGrant,
    ) -> Result<(), ReviewError> {
        let inverse = MutationFile {
            logical_path: file.logical_path.clone(),
            before_sha256: file.after_sha256.clone(),
            after_sha256: file.before_sha256.clone(),
            before_cas_ref: file.after_cas_ref.clone(),
            after_cas_ref: file.before_cas_ref.clone(),
        };
        let operation_id = inverse_operation_id(action_id, index, forward_id);
        let operation = MutationOperation {
            operation_id: operation_id.clone(),
            workspace_key: workspace_key.to_string(),
            lease_id: lease.lease_id.clone(),
            owner_id: owner.to_string(),
            fencing_epoch: lease.fencing_epoch,
            input_hash: canonical_input_hash(&serde_json::json!({
                "forward": forward_id,
                "path": inverse.logical_path,
                "before": inverse.before_sha256,
                "after": inverse.after_sha256,
            })),
            state: MutationState::Prepared,
            files: vec![inverse.clone()],
        };
        let prepared = self
            .store
            .prepare_effect_operation(&operation)
            .map_err(|_| ReviewError::Store)?;
        match prepared.state {
            MutationState::Receipted => return Ok(()),
            MutationState::Applied => {
                self.store
                    .mark_receipted(&operation_id, owner, lease.fencing_epoch)
                    .map_err(|_| ReviewError::Store)?;
                return Ok(());
            }
            MutationState::Conflict => {
                return Err(ReviewError::Conflict(inverse.logical_path));
            }
            MutationState::Prepared => {}
        }
        let resolved = match self.binding.resolve_path(&inverse.logical_path) {
            Ok(resolved) => resolved,
            Err(_) => {
                return self.inverse_conflict(
                    &operation_id,
                    owner,
                    lease.fencing_epoch,
                    &inverse.logical_path,
                )
            }
        };
        if resolved.revalidate().is_err() {
            return self.inverse_conflict(
                &operation_id,
                owner,
                lease.fencing_epoch,
                &inverse.logical_path,
            );
        }
        let current = match self.current_sha(&inverse.logical_path) {
            Ok(current) => current,
            Err(_) => {
                return self.inverse_conflict(
                    &operation_id,
                    owner,
                    lease.fencing_epoch,
                    &inverse.logical_path,
                )
            }
        };
        if current == inverse.after_sha256 {
            self.store
                .mark_applied(&operation_id, owner, lease.fencing_epoch, vec![inverse])
                .and_then(|_| {
                    self.store
                        .mark_receipted(&operation_id, owner, lease.fencing_epoch)
                })
                .map_err(|_| ReviewError::Store)?;
            return Ok(());
        }
        if current != inverse.before_sha256 {
            return self.inverse_conflict(
                &operation_id,
                owner,
                lease.fencing_epoch,
                &inverse.logical_path,
            );
        }
        let path = Path::new(&inverse.logical_path);
        match (&inverse.before_sha256, &inverse.after_sha256) {
            (None, Some(_)) => {
                let bytes = match self.cas_bytes(inverse.after_cas_ref.as_deref()) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        return self.inverse_conflict(
                            &operation_id,
                            owner,
                            lease.fencing_epoch,
                            &inverse.logical_path,
                        )
                    }
                };
                if self.guard.create_new_path(path, &bytes).is_err() {
                    return self.inverse_conflict(
                        &operation_id,
                        owner,
                        lease.fencing_epoch,
                        &inverse.logical_path,
                    );
                }
            }
            (Some(_), Some(_)) => {
                let bytes = match self.cas_bytes(inverse.after_cas_ref.as_deref()) {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        return self.inverse_conflict(
                            &operation_id,
                            owner,
                            lease.fencing_epoch,
                            &inverse.logical_path,
                        )
                    }
                };
                if self.guard.atomic_write_path(path, &bytes).is_err() {
                    return self.inverse_conflict(
                        &operation_id,
                        owner,
                        lease.fencing_epoch,
                        &inverse.logical_path,
                    );
                }
            }
            (Some(_), None) => {
                if !self.guard.remove_file_if_exists(path).unwrap_or(false) {
                    return self.inverse_conflict(
                        &operation_id,
                        owner,
                        lease.fencing_epoch,
                        &inverse.logical_path,
                    );
                }
            }
            (None, None) => {
                return self.inverse_conflict(
                    &operation_id,
                    owner,
                    lease.fencing_epoch,
                    &inverse.logical_path,
                )
            }
        }
        if self.current_sha(&inverse.logical_path).ok() != Some(inverse.after_sha256.clone()) {
            return self.inverse_conflict(
                &operation_id,
                owner,
                lease.fencing_epoch,
                &inverse.logical_path,
            );
        }
        self.store
            .mark_applied(&operation_id, owner, lease.fencing_epoch, vec![inverse])
            .and_then(|_| {
                self.store
                    .mark_receipted(&operation_id, owner, lease.fencing_epoch)
            })
            .map_err(|_| ReviewError::Store)?;
        Ok(())
    }

    fn inverse_conflict<T>(
        &self,
        operation_id: &str,
        owner: &str,
        fencing_epoch: u64,
        logical_path: &str,
    ) -> Result<T, ReviewError> {
        self.store
            .mark_conflict(operation_id, owner, fencing_epoch)
            .map_err(|_| ReviewError::Store)?;
        let _ = logical_path;
        // A conflict after an inverse was durably Prepared is not terminal:
        // retain the complete review lease for explicit recovery/audit.
        Err(ReviewError::Store)
    }

    fn operations(&self) -> Result<Vec<MutationOperation>, ReviewError> {
        self.store
            .mutation_operations_for_owner(&self.attempt_id)
            .map_err(|_| ReviewError::Store)
    }

    fn validate_state(&self, state: &TaskState) -> Result<(), ReviewError> {
        // E05: the task-level candidate digest is DERIVED from the per-unit
        // records — the same aggregate the wire's candidateDigest carries.
        if state.contract.task_id != self.task_id
            || state.task_candidate_digest().as_deref() != Some(self.candidate_digest.as_str())
            || !matches!(
                &state.execution,
                TaskExecution::ReviewReady { attempt_id } if attempt_id == &self.attempt_id
            )
            || !state.work_units.iter().any(|unit| {
                unit.id == self.work_unit_id && unit.status == WorkUnitStatus::Completed
            })
        {
            return Err(ReviewError::InvalidState);
        }
        Ok(())
    }

    fn current_sha(&self, logical_path: &str) -> Result<Option<String>, ReviewError> {
        let resolved = self
            .binding
            .resolve_path(logical_path)
            .map_err(|_| ReviewError::Conflict(logical_path.to_string()))?;
        resolved
            .revalidate()
            .map_err(|_| ReviewError::Conflict(logical_path.to_string()))?;
        if resolved.existing_target_id.is_none() {
            return Ok(None);
        }
        let mut file = self
            .guard
            .open_file(
                Path::new(logical_path),
                r_code_core::security::WorkspaceFileAccess::Read,
            )
            .map_err(|_| ReviewError::Conflict(logical_path.to_string()))?
            .into_file();
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut file, &mut bytes)
            .map_err(|_| ReviewError::Io("read failed".to_string()))?;
        Ok(Some(sha256_hex(&bytes)))
    }

    fn cas_bytes(&self, encoded: Option<&str>) -> Result<Vec<u8>, ReviewError> {
        let encoded = encoded.ok_or(ReviewError::Stale)?;
        let artifact: ArtifactRef =
            serde_json::from_str(encoded).map_err(|_| ReviewError::Stale)?;
        self.artifacts
            .read_all(&artifact)
            .map_err(|_| ReviewError::Stale)
    }
}

fn required_checks(state: &TaskState, work_unit_id: &str) -> Vec<String> {
    let acceptance = state
        .work_units
        .iter()
        .find(|unit| unit.id == work_unit_id)
        .map(|unit| unit.acceptance.as_slice())
        .unwrap_or_default();
    state
        .contract
        .required_checks
        .iter()
        .chain(
            acceptance
                .iter()
                .filter(|check| check.starts_with("check:")),
        )
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn inverse_operation_id(action_id: &str, index: usize, forward_id: &str) -> String {
    format!("review:{action_id}:{index}:{forward_id}")
}

fn is_sha256_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
