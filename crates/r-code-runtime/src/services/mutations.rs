//! Approved, single-path workspace mutations with durable reconciliation.

use crate::services::artifacts::{sha256_hex, ArtifactStore};
use crate::services::workspaces::{ResolvedWorkspacePath, TaskWorkspaceBinding};
use r_code_core::security::{PathGuard, WorkspaceFileAccess};
use r_code_harness_protocol::{
    canonical_input_hash, normalize_workspace_relative_path, ArtifactRef, OperationKey,
};
use r_code_kernel::plans::PlanRevision;
use r_code_kernel::task::WorkUnit;
use r_code_store::v1::{
    LeaseGrant, LeaseRequest, MutationFile, MutationOperation, MutationState, V1Store,
};
use serde::{Deserialize, Serialize};
use std::io::Read as _;
use std::path::Path;
use std::sync::Arc;

const WRITE_TOOLS: &[&str] = &["create_file", "edit", "apply_patch", "delete_file"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationCheckpoint {
    AfterPrepare,
    BeforeEffect,
    AfterEffect,
    AfterApplied,
}

pub trait MutationFaultHook: Send + Sync {
    fn checkpoint(
        &self,
        _point: MutationCheckpoint,
        _operation_id: &str,
    ) -> Result<(), MutationExecutionError> {
        Ok(())
    }
}

#[derive(Default)]
pub struct NoopMutationFaultHook;

impl MutationFaultHook for NoopMutationFaultHook {}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MutationExecutionError {
    #[error("execution plan and work unit do not match")]
    PlanMismatch,
    #[error("mutation is outside the approved write scope")]
    Denied,
    #[error("invalid mutation input: {0}")]
    InvalidInput(&'static str),
    #[error("workspace mutation conflicts with current content or path identity")]
    Conflict,
    #[error("durable mutation lease is unavailable")]
    Lease,
    #[error("mutation journal failure")]
    Journal,
    #[error("mutation artifact failure")]
    Artifact,
    #[error("workspace capability failure")]
    Workspace,
    #[error("injected mutation checkpoint failure")]
    Injected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MutationReply {
    pub operation_id: String,
    pub logical_path: String,
    pub state: MutationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JournalArtifactRef {
    #[serde(flatten)]
    artifact: ArtifactRef,
    /// Pre-effect identity. Recovery checks this only before applying the
    /// intended effect; an atomic replace legitimately changes target id.
    pre_effect_path: ResolvedWorkspacePath,
}

struct EffectIntent {
    logical_path: String,
    resolved: ResolvedWorkspacePath,
    before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
}

pub struct MutationExecutor {
    binding: TaskWorkspaceBinding,
    guard: PathGuard,
    store: Arc<V1Store>,
    artifacts: Arc<ArtifactStore>,
    attempt_id: String,
    lease: LeaseGrant,
    write_paths: Vec<String>,
    hook: Arc<dyn MutationFaultHook>,
}

impl MutationExecutor {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        plan: &PlanRevision,
        unit: &WorkUnit,
        binding: TaskWorkspaceBinding,
        store: Arc<V1Store>,
        artifacts: Arc<ArtifactStore>,
        attempt_id: impl Into<String>,
    ) -> Result<Self, MutationExecutionError> {
        plan.validate_identity()
            .map_err(|_| MutationExecutionError::PlanMismatch)?;
        let attempt_id = attempt_id.into();
        let wire = plan
            .material()
            .work_units
            .iter()
            .find(|candidate| candidate.id == unit.id)
            .ok_or(MutationExecutionError::PlanMismatch)?;
        if plan.material().task_id != binding.task_id
            || artifacts.task_id() != Some(binding.task_id.as_str())
            || attempt_id.trim().is_empty()
            || wire.description != unit.description
            || wire.dependencies != unit.dependencies
            || wire.acceptance != unit.acceptance
            || wire.read_paths != unit.read_paths
            || wire.write_paths != unit.write_paths
            || wire.repo_exclusive != unit.repo_exclusive
            || wire.ephemeral_roots != unit.ephemeral_roots
        {
            return Err(MutationExecutionError::PlanMismatch);
        }
        for root in &unit.ephemeral_roots {
            if binding
                .resolve_path(root)
                .map_err(|_| MutationExecutionError::Workspace)?
                .existing_target_id
                .is_some()
            {
                return Err(MutationExecutionError::Conflict);
            }
        }
        let workspace_key = format!(
            "sha256:{}",
            sha256_hex(binding.canonical_root.to_string_lossy().as_bytes())
        );
        // E07: the executor's lease is acquired as the one-member family of
        // this attempt — all-or-nothing with the attempt's identity as the
        // family key, so no lease of the attempt exists outside its family.
        let lease = store
            .acquire_lease_family(
                &attempt_id,
                &[LeaseRequest {
                    workspace_key,
                    operation_id: format!("lease:{attempt_id}:{}", unit.id),
                    owner_id: attempt_id.clone(),
                    read_paths: unit.read_paths.clone(),
                    write_paths: unit.write_paths.clone(),
                    repo_exclusive: unit.repo_exclusive,
                }],
            )
            .map_err(|_| MutationExecutionError::Lease)?
            .leases
            .into_iter()
            .next()
            .ok_or(MutationExecutionError::Lease)?;
        if !lease.active {
            return Err(MutationExecutionError::Lease);
        }
        let guard = PathGuard::new(binding.canonical_root.clone())
            .map_err(|_| MutationExecutionError::Workspace)?;
        Ok(Self {
            binding,
            guard,
            store,
            artifacts,
            attempt_id,
            lease,
            write_paths: unit.write_paths.clone(),
            hook: Arc::new(NoopMutationFaultHook),
        })
    }

    pub fn with_fault_hook(mut self, hook: Arc<dyn MutationFaultHook>) -> Self {
        self.hook = hook;
        self
    }

    pub fn supports_writes(&self) -> bool {
        self.lease.request.repo_exclusive || !self.write_paths.is_empty()
    }

    /// Release the attempt's member lease through its family (E07): a
    /// terminal family already released everything (`Ok(true)`); an open
    /// family releases this member alone — the attempt record stays in
    /// flight for the dispatcher's durable settle or restart recovery.
    pub fn release(&self) -> Result<bool, MutationExecutionError> {
        self.store
            .release_lease_family_member(
                &self.attempt_id,
                &self.lease.lease_id,
                &self.attempt_id,
                self.lease.fencing_epoch,
            )
            .map_err(|_| MutationExecutionError::Lease)
    }

    pub fn execute(
        &self,
        tool: &str,
        input: &serde_json::Value,
        operation_key: &OperationKey,
    ) -> Result<MutationReply, MutationExecutionError> {
        if !WRITE_TOOLS.contains(&tool) || operation_key.0.trim().is_empty() {
            return Err(MutationExecutionError::Denied);
        }
        let logical_path = self.approved_path(input)?;
        let operation_id = format!("{}::{}", self.attempt_id, operation_key.0);
        let input_hash = canonical_input_hash(&serde_json::json!({
            "tool": tool,
            "input": input,
        }));
        if let Some(operation) = self
            .store
            .load_mutation_operation(&operation_id)
            .map_err(|_| MutationExecutionError::Journal)?
        {
            if operation.input_hash != input_hash
                || operation.files.len() != 1
                || operation.files[0].logical_path != logical_path
            {
                return Err(MutationExecutionError::Conflict);
            }
            return self.reconcile(operation);
        }

        let intent = self.build_intent(tool, input, logical_path)?;
        let file = self.journal_file(&intent)?;
        let operation = MutationOperation {
            operation_id: operation_id.clone(),
            workspace_key: self.lease.request.workspace_key.clone(),
            lease_id: self.lease.lease_id.clone(),
            owner_id: self.attempt_id.clone(),
            fencing_epoch: self.lease.fencing_epoch,
            input_hash,
            state: MutationState::Prepared,
            files: vec![file],
        };
        let prepared = self
            .store
            .prepare_effect_operation(&operation)
            .map_err(|_| MutationExecutionError::Journal)?;
        self.hook
            .checkpoint(MutationCheckpoint::AfterPrepare, &operation_id)?;
        self.resume_prepared(prepared, Some(intent.resolved))
    }

    fn approved_path(&self, input: &serde_json::Value) -> Result<String, MutationExecutionError> {
        let object = input
            .as_object()
            .ok_or(MutationExecutionError::InvalidInput("object required"))?;
        let path = object
            .get("path")
            .and_then(|value| value.as_str())
            .ok_or(MutationExecutionError::InvalidInput("path required"))?;
        let logical =
            normalize_workspace_relative_path(path).map_err(|_| MutationExecutionError::Denied)?;
        // 大小写折叠用遮蔽而非可变绑定——unix 腿没有重赋值，mut 会告警。
        #[cfg(windows)]
        let logical = logical.to_lowercase();
        let approved = self.lease.request.repo_exclusive
            || self
                .write_paths
                .iter()
                .any(|scope| path_contains(scope, &logical));
        if approved {
            Ok(logical)
        } else {
            Err(MutationExecutionError::Denied)
        }
    }

    fn build_intent(
        &self,
        tool: &str,
        input: &serde_json::Value,
        logical_path: String,
    ) -> Result<EffectIntent, MutationExecutionError> {
        let resolved = self
            .binding
            .resolve_path(&logical_path)
            .map_err(|_| MutationExecutionError::Workspace)?;
        resolved
            .revalidate()
            .map_err(|_| MutationExecutionError::Conflict)?;
        let before = self.read_current(&logical_path, &resolved)?;
        let after = match tool {
            "create_file" => {
                if before.is_some() {
                    return Err(MutationExecutionError::Conflict);
                }
                Some(required_text(input, "content")?.as_bytes().to_vec())
            }
            "delete_file" => {
                if before.is_none() {
                    return Err(MutationExecutionError::Conflict);
                }
                None
            }
            "apply_patch" => Some(required_text(input, "content")?.as_bytes().to_vec()),
            "edit" => Some(compute_edit(input, before.as_deref())?),
            _ => return Err(MutationExecutionError::Denied),
        };
        Ok(EffectIntent {
            logical_path,
            resolved,
            before,
            after,
        })
    }

    fn journal_file(&self, intent: &EffectIntent) -> Result<MutationFile, MutationExecutionError> {
        let before = intent
            .before
            .as_deref()
            .map(|bytes| self.store_artifact(bytes, &intent.resolved))
            .transpose()?;
        let after = intent
            .after
            .as_deref()
            .map(|bytes| self.store_artifact(bytes, &intent.resolved))
            .transpose()?;
        Ok(MutationFile {
            logical_path: intent.logical_path.clone(),
            before_sha256: before.as_ref().map(|(hash, _)| hash.clone()),
            after_sha256: after.as_ref().map(|(hash, _)| hash.clone()),
            before_cas_ref: before.map(|(_, reference)| reference),
            after_cas_ref: after.map(|(_, reference)| reference),
        })
    }

    fn store_artifact(
        &self,
        bytes: &[u8],
        path: &ResolvedWorkspacePath,
    ) -> Result<(String, String), MutationExecutionError> {
        let artifact = self
            .artifacts
            .put_bytes(bytes, Some("application/octet-stream".to_string()))
            .map_err(|_| MutationExecutionError::Artifact)?;
        let journal_ref = JournalArtifactRef {
            artifact: artifact.clone(),
            pre_effect_path: path.clone(),
        };
        let encoded =
            serde_json::to_string(&journal_ref).map_err(|_| MutationExecutionError::Artifact)?;
        Ok((artifact.sha256, encoded))
    }

    fn reconcile(
        &self,
        operation: MutationOperation,
    ) -> Result<MutationReply, MutationExecutionError> {
        if operation.owner_id != self.attempt_id
            || operation.lease_id != self.lease.lease_id
            || operation.fencing_epoch != self.lease.fencing_epoch
        {
            return Err(MutationExecutionError::Lease);
        }
        match operation.state {
            MutationState::Receipted => Ok(reply(&operation)),
            MutationState::Applied => self.receipt(operation),
            MutationState::Prepared => self.resume_prepared(operation, None),
            MutationState::Conflict => Err(MutationExecutionError::Conflict),
        }
    }

    fn resume_prepared(
        &self,
        operation: MutationOperation,
        resolved: Option<ResolvedWorkspacePath>,
    ) -> Result<MutationReply, MutationExecutionError> {
        let file = operation
            .files
            .first()
            .ok_or(MutationExecutionError::Journal)?;
        self.validate_journal_file(file)?;
        let resolved = match resolved {
            Some(path) => path,
            None => self
                .binding
                .resolve_path(&file.logical_path)
                .map_err(|_| MutationExecutionError::Workspace)?,
        };
        let current = match self.current_sha(&file.logical_path, &resolved) {
            Ok(current) => current,
            Err(MutationExecutionError::Conflict) => return self.conflict(&operation.operation_id),
            Err(error) => return Err(error),
        };
        if current == file.after_sha256 {
            return self.applied_then_receipt(operation);
        }
        if current != file.before_sha256 {
            return self.conflict(&operation.operation_id);
        }
        let pre_effect = pre_effect_path(file)?;
        if pre_effect.revalidate().is_err() {
            return self.conflict(&operation.operation_id);
        }
        self.hook
            .checkpoint(MutationCheckpoint::BeforeEffect, &operation.operation_id)?;
        if resolved.revalidate().is_err() {
            return self.conflict(&operation.operation_id);
        }
        let current = match self.current_sha(&file.logical_path, &resolved) {
            Ok(current) => current,
            Err(MutationExecutionError::Conflict) => return self.conflict(&operation.operation_id),
            Err(error) => return Err(error),
        };
        if current != file.before_sha256 {
            return self.conflict(&operation.operation_id);
        }
        if let Err(error) = self.apply_effect(file) {
            return if error == MutationExecutionError::Conflict {
                self.conflict(&operation.operation_id)
            } else {
                Err(error)
            };
        }
        self.hook
            .checkpoint(MutationCheckpoint::AfterEffect, &operation.operation_id)?;
        let measured = match self.current_sha_fresh(&file.logical_path) {
            Ok(measured) => measured,
            Err(MutationExecutionError::Conflict) => return self.conflict(&operation.operation_id),
            Err(error) => return Err(error),
        };
        if measured != file.after_sha256 {
            return self.conflict(&operation.operation_id);
        }
        self.applied_then_receipt(operation)
    }

    fn apply_effect(&self, file: &MutationFile) -> Result<(), MutationExecutionError> {
        let path = Path::new(&file.logical_path);
        match (&file.before_sha256, &file.after_sha256) {
            (None, Some(_)) => {
                let bytes = self.after_bytes(file)?;
                self.guard
                    .create_new_path(path, &bytes)
                    .map_err(|_| MutationExecutionError::Conflict)?;
            }
            (Some(_), Some(_)) => {
                let bytes = self.after_bytes(file)?;
                self.guard
                    .atomic_write_path(path, &bytes)
                    .map_err(|_| MutationExecutionError::Conflict)?;
            }
            (Some(_), None) => {
                let removed = self
                    .guard
                    .remove_file_if_exists(path)
                    .map_err(|_| MutationExecutionError::Conflict)?;
                if !removed {
                    return Err(MutationExecutionError::Conflict);
                }
            }
            (None, None) => return Err(MutationExecutionError::Journal),
        }
        Ok(())
    }

    fn after_bytes(&self, file: &MutationFile) -> Result<Vec<u8>, MutationExecutionError> {
        let reference = decode_journal_ref(
            file.after_cas_ref
                .as_deref()
                .ok_or(MutationExecutionError::Journal)?,
        )?;
        let bytes = self
            .artifacts
            .read_all(&reference.artifact)
            .map_err(|_| MutationExecutionError::Artifact)?;
        if Some(sha256_hex(&bytes)) != file.after_sha256 {
            return Err(MutationExecutionError::Artifact);
        }
        Ok(bytes)
    }

    fn validate_journal_file(&self, file: &MutationFile) -> Result<(), MutationExecutionError> {
        for (hash, encoded) in [
            (&file.before_sha256, &file.before_cas_ref),
            (&file.after_sha256, &file.after_cas_ref),
        ] {
            match (hash, encoded) {
                (None, None) => {}
                (Some(expected), Some(encoded)) => {
                    let reference = decode_journal_ref(encoded)?;
                    let bytes = self
                        .artifacts
                        .read_all(&reference.artifact)
                        .map_err(|_| MutationExecutionError::Artifact)?;
                    if sha256_hex(&bytes) != *expected {
                        return Err(MutationExecutionError::Artifact);
                    }
                }
                _ => return Err(MutationExecutionError::Journal),
            }
        }
        Ok(())
    }

    fn applied_then_receipt(
        &self,
        operation: MutationOperation,
    ) -> Result<MutationReply, MutationExecutionError> {
        let applied = self
            .store
            .mark_applied(
                &operation.operation_id,
                &self.attempt_id,
                self.lease.fencing_epoch,
                operation.files.clone(),
            )
            .map_err(|_| MutationExecutionError::Journal)?;
        self.hook
            .checkpoint(MutationCheckpoint::AfterApplied, &operation.operation_id)?;
        self.receipt(applied)
    }

    fn receipt(
        &self,
        operation: MutationOperation,
    ) -> Result<MutationReply, MutationExecutionError> {
        let receipted = self
            .store
            .mark_receipted(
                &operation.operation_id,
                &self.attempt_id,
                self.lease.fencing_epoch,
            )
            .map_err(|_| MutationExecutionError::Journal)?;
        Ok(reply(&receipted))
    }

    fn conflict<T>(&self, operation_id: &str) -> Result<T, MutationExecutionError> {
        self.store
            .mark_conflict(operation_id, &self.attempt_id, self.lease.fencing_epoch)
            .map_err(|_| MutationExecutionError::Journal)?;
        Err(MutationExecutionError::Conflict)
    }

    fn current_sha(
        &self,
        logical_path: &str,
        resolved: &ResolvedWorkspacePath,
    ) -> Result<Option<String>, MutationExecutionError> {
        resolved
            .revalidate()
            .map_err(|_| MutationExecutionError::Conflict)?;
        self.read_current(logical_path, resolved)
            .map(|bytes| bytes.map(|value| sha256_hex(&value)))
    }

    fn current_sha_fresh(
        &self,
        logical_path: &str,
    ) -> Result<Option<String>, MutationExecutionError> {
        let resolved = self
            .binding
            .resolve_path(logical_path)
            .map_err(|_| MutationExecutionError::Workspace)?;
        self.current_sha(logical_path, &resolved)
    }

    fn read_current(
        &self,
        logical_path: &str,
        resolved: &ResolvedWorkspacePath,
    ) -> Result<Option<Vec<u8>>, MutationExecutionError> {
        if resolved.existing_target_id.is_none() {
            return Ok(None);
        }
        let mut file = self
            .guard
            .open_file(Path::new(logical_path), WorkspaceFileAccess::Read)
            .map_err(|_| MutationExecutionError::Conflict)?
            .into_file();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|_| MutationExecutionError::Workspace)?;
        Ok(Some(bytes))
    }
}

fn required_text<'a>(
    input: &'a serde_json::Value,
    field: &'static str,
) -> Result<&'a str, MutationExecutionError> {
    input
        .get(field)
        .and_then(|value| value.as_str())
        .ok_or(MutationExecutionError::InvalidInput(field))
}

fn compute_edit(
    input: &serde_json::Value,
    before: Option<&[u8]>,
) -> Result<Vec<u8>, MutationExecutionError> {
    let before = before.ok_or(MutationExecutionError::Conflict)?;
    let content = std::str::from_utf8(before)
        .map_err(|_| MutationExecutionError::InvalidInput("file is not UTF-8"))?;
    let old = required_text(input, "old_string")?;
    let new = required_text(input, "new_string")?;
    if old.is_empty() || old == new {
        return Err(MutationExecutionError::InvalidInput("invalid edit anchor"));
    }
    if let Some(expected) = input.get("expected_revision") {
        let expected = expected
            .as_str()
            .ok_or(MutationExecutionError::InvalidInput("expected_revision"))?;
        let actual = format!("blake3:{}", blake3::hash(before).to_hex());
        if expected != actual {
            return Err(MutationExecutionError::Conflict);
        }
    }
    let occurrences = content.match_indices(old).count();
    let replace_all = input
        .get("replace_all")
        .map(|value| {
            value
                .as_bool()
                .ok_or(MutationExecutionError::InvalidInput("replace_all"))
        })
        .transpose()?
        .unwrap_or(false);
    if occurrences == 0 || (!replace_all && occurrences != 1) {
        return Err(MutationExecutionError::Conflict);
    }
    Ok(if replace_all {
        content.replace(old, new).into_bytes()
    } else {
        content.replacen(old, new, 1).into_bytes()
    })
}

fn pre_effect_path(file: &MutationFile) -> Result<ResolvedWorkspacePath, MutationExecutionError> {
    let encoded = file
        .before_cas_ref
        .as_deref()
        .or(file.after_cas_ref.as_deref())
        .ok_or(MutationExecutionError::Journal)?;
    Ok(decode_journal_ref(encoded)?.pre_effect_path)
}

fn decode_journal_ref(value: &str) -> Result<JournalArtifactRef, MutationExecutionError> {
    serde_json::from_str(value).map_err(|_| MutationExecutionError::Artifact)
}

fn reply(operation: &MutationOperation) -> MutationReply {
    let file = &operation.files[0];
    MutationReply {
        operation_id: operation.operation_id.clone(),
        logical_path: file.logical_path.clone(),
        state: operation.state,
        before_sha256: file.before_sha256.clone(),
        after_sha256: file.after_sha256.clone(),
    }
}

fn path_contains(scope: &str, path: &str) -> bool {
    scope == path
        || path
            .strip_prefix(scope)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

// ---------------------------------------------------------------------------
// P27 — persist a measured multi-file process delta. The scanner measures;
// this journals every changed file's observed bytes as CAS delta-blobs
// under the effect operation's refcount, so the write-ingress path can
// reconcile a workspace-writing process without re-executing it.
// ---------------------------------------------------------------------------

/// CAS-persist the measured delta of one workspace-writing process: every
/// created/edited/binary entry's AFTER bytes are journaled (deleted
/// entries have none), together with the delta list itself. Returns the
/// digest over the delta list — the same digest the envelope receipts.
pub fn persist_measured_delta(
    store: &r_code_store::v1::V1Store,
    artifacts: &ArtifactStore,
    operation: crate::services::artifacts::EffectArtifactPut<'_>,
    workspace_root: &std::path::Path,
    deltas: &[crate::services::process_effects::DeltaEntry],
) -> Result<String, MutationExecutionError> {
    use crate::services::process_effects::DeltaKind;
    for entry in deltas {
        match entry.kind {
            DeltaKind::Deleted => continue,
            DeltaKind::Created | DeltaKind::Edited | DeltaKind::Binary => {}
        }
        let bytes = std::fs::read(workspace_root.join(&entry.path))
            .map_err(|_| MutationExecutionError::Workspace)?;
        artifacts
            .put_effect_bytes(store, operation, &bytes, None)
            .map_err(|_| MutationExecutionError::Artifact)?;
    }
    let delta_json = serde_json::to_vec(
        &deltas
            .iter()
            .map(|entry| {
                serde_json::json!({"path": entry.path, "kind": match entry.kind {
                    DeltaKind::Created => "created",
                    DeltaKind::Edited => "edited",
                    DeltaKind::Deleted => "deleted",
                    DeltaKind::Binary => "binary",
                }})
            })
            .collect::<Vec<_>>(),
    )
    .map_err(|_| MutationExecutionError::Journal)?;
    artifacts
        .put_effect_bytes(store, operation, &delta_json, None)
        .map_err(|_| MutationExecutionError::Artifact)?;
    Ok(crate::services::artifacts::sha256_hex(&delta_json))
}
