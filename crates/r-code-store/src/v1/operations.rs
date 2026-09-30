//! V1 persistence for operation receipts (attempt view) and writer barriers.

use crate::v1::V1Store;
use r_code_kernel::task::{OperationReceipt, ReceiptOutcome};
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};

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

impl ProcessTreeState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Running => "running",
            Self::Terminating => "terminating",
            Self::Exited => "exited",
            Self::Quarantined => "quarantined",
            Self::LegacyUnverifiable => "legacy-unverifiable",
        }
    }

    fn parse(value: &str) -> Result<Self, ProcessTreeStoreError> {
        match value {
            "prepared" => Ok(Self::Prepared),
            "running" => Ok(Self::Running),
            "terminating" => Ok(Self::Terminating),
            "exited" => Ok(Self::Exited),
            "quarantined" => Ok(Self::Quarantined),
            "legacy-unverifiable" => Ok(Self::LegacyUnverifiable),
            _ => Err(ProcessTreeStoreError::CorruptRecord),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessTreeOwner {
    pub pid: u32,
    pub start_identity: u64,
    pub boot_identity: String,
    pub platform_identity: serde_json::Value,
    pub platform_identity_digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrepareProcessTree {
    pub tree_id: String,
    pub attempt_id: String,
    pub workspace_key: String,
    pub profile_id: String,
    pub owner: ProcessTreeOwner,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessTreeRecord {
    pub tree_id: String,
    pub attempt_id: String,
    pub workspace_key: String,
    pub profile_id: String,
    pub owner: ProcessTreeOwner,
    pub ownership_epoch: u64,
    pub state: ProcessTreeState,
    pub state_revision: u64,
    pub quarantine_reason: Option<String>,
    pub migrated_observed_boot_identity: Option<String>,
    pub termination_proof_id: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub legacy_barrier_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessTreeFence {
    pub ownership_epoch: u64,
    pub state_revision: u64,
    pub owner_pid: u32,
    pub owner_start_identity: u64,
    pub owner_boot_identity: String,
    pub platform_identity_digest: String,
}

impl ProcessTreeRecord {
    pub fn fence(&self) -> ProcessTreeFence {
        ProcessTreeFence {
            ownership_epoch: self.ownership_epoch,
            state_revision: self.state_revision,
            owner_pid: self.owner.pid,
            owner_start_identity: self.owner.start_identity,
            owner_boot_identity: self.owner.boot_identity.clone(),
            platform_identity_digest: self.owner.platform_identity_digest.clone(),
        }
    }

    /// Canonical envelope that binds platform evidence to this exact tree.
    pub fn proof_identity(
        &self,
        observed_boot_identity: &str,
        platform_evidence: serde_json::Value,
    ) -> serde_json::Value {
        serde_json::json!({
            "treeId": self.tree_id,
            "ownershipEpoch": self.ownership_epoch,
            "ownerPid": self.owner.pid,
            "ownerStartIdentity": self.owner.start_identity,
            "ownerBootIdentity": self.owner.boot_identity,
            "ownerPlatformIdentityDigest": self.owner.platform_identity_digest,
            "migratedObservedBootIdentity": self.migrated_observed_boot_identity,
            "observedBootIdentity": observed_boot_identity,
            "platformEvidence": platform_evidence,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TerminationProofKind {
    Exit,
    Reboot,
}

impl TerminationProofKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Exit => "exit",
            Self::Reboot => "reboot",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewTerminationProof {
    pub proof_id: String,
    pub kind: TerminationProofKind,
    pub observed_boot_identity: String,
    pub proof_identity: serde_json::Value,
    pub proof_identity_digest: String,
}

impl NewTerminationProof {
    pub fn bound_to_tree(
        proof_id: impl Into<String>,
        kind: TerminationProofKind,
        observed_boot_identity: impl Into<String>,
        tree: &ProcessTreeRecord,
        platform_evidence: serde_json::Value,
    ) -> Self {
        let observed_boot_identity = observed_boot_identity.into();
        let proof_identity = tree.proof_identity(&observed_boot_identity, platform_evidence);
        let proof_identity_digest = r_code_harness_protocol::canonical_input_hash(&proof_identity);
        Self {
            proof_id: proof_id.into(),
            kind,
            observed_boot_identity,
            proof_identity,
            proof_identity_digest,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminationProofRecord {
    pub proof_id: String,
    pub tree_id: String,
    pub ownership_epoch: u64,
    pub kind: TerminationProofKind,
    pub observed_boot_identity: String,
    pub proof_identity: serde_json::Value,
    pub proof_identity_digest: String,
    pub recorded_at_ms: i64,
}

/// The outcome of one quarantine retry (P11R): what happened, and why
/// the blocked kinds stayed blocked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuarantineRetryOutcome {
    /// The tree transitioned to Exited carrying the proof.
    Cleared { proof_id: String },
    /// The retry was refused — the reason is caller-facing.
    Refused { reason: QuarantineRetryRefusal },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarantineRetryRefusal {
    /// The tree is not quarantined (already exited, running, missing).
    NotQuarantined,
    /// A legacy row on the SAME boot: only a different stable
    /// BootIdentity can ever prove it (P11R.2).
    SameBootLegacy,
    /// A corrupted legacy row can never be proven.
    LegacyCorrupt,
    /// An ordinary quarantined tree without a complete persisted
    /// proof: no live prover can re-issue one after a restart, so it
    /// stays blocked (the acceptance criterion).
    UnverifiableNoProof,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QuarantineTreeState {
    Prepared,
    Running,
    Terminating,
    Quarantined,
    LegacyUnverifiable,
    ExitedUnproved,
    Corrupt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QuarantineProofStatus {
    Missing,
    Invalid,
    Corrupt,
}

/// Store-owned, already-redacted projection of one writer quarantine.
#[derive(Clone, PartialEq, Eq)]
pub struct QuarantineDiagnosticRecord {
    pub tree_ref: String,
    pub workspace_ref: String,
    pub workspace_identity: Option<String>,
    pub profile_ref: String,
    pub state: QuarantineTreeState,
    pub ownership_epoch: Option<u64>,
    pub owner_fingerprint: String,
    pub reason_code: String,
    pub reason_digest: String,
    pub proof_status: QuarantineProofStatus,
    pub legacy: bool,
    pub legacy_corrupt: bool,
    pub corrupt: bool,
    pub created_at_ms: Option<i64>,
    pub updated_at_ms: Option<i64>,
    migrated_observed_boot_identity: Option<String>,
}

impl std::fmt::Debug for QuarantineDiagnosticRecord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("QuarantineDiagnosticRecord")
            .field("tree_ref", &self.tree_ref)
            .field("workspace_ref", &self.workspace_ref)
            .field("workspace_identity", &self.workspace_identity)
            .field("profile_ref", &self.profile_ref)
            .field("state", &self.state)
            .field("ownership_epoch", &self.ownership_epoch)
            .field("owner_fingerprint", &self.owner_fingerprint)
            .field("reason_code", &self.reason_code)
            .field("reason_digest", &self.reason_digest)
            .field("proof_status", &self.proof_status)
            .field("legacy", &self.legacy)
            .field("legacy_corrupt", &self.legacy_corrupt)
            .field("corrupt", &self.corrupt)
            .field("created_at_ms", &self.created_at_ms)
            .field("updated_at_ms", &self.updated_at_ms)
            .finish()
    }
}

impl QuarantineDiagnosticRecord {
    pub fn current_boot_changed(&self, current_boot_identity: &str) -> Option<bool> {
        if !self.legacy {
            return None;
        }
        self.migrated_observed_boot_identity
            .as_deref()
            .map(|observed| observed != current_boot_identity)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProcessTreeStoreError {
    #[error("process tree input is invalid")]
    InvalidInput,
    #[error("process tree already exists with different identity")]
    IdentityConflict,
    #[error("process tree was not found")]
    NotFound,
    #[error("process tree ownership fence is stale")]
    StaleFence,
    #[error("process tree transition is invalid")]
    InvalidTransition,
    #[error("termination proof is required or invalid")]
    InvalidProof,
    #[error("process tree record is corrupt")]
    CorruptRecord,
    #[error("sqlite failure: {0}")]
    Sqlite(String),
}

impl From<rusqlite::Error> for ProcessTreeStoreError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error.to_string())
    }
}

impl V1Store {
    /// All receipts for one attempt (dedup history; generations never erase it).
    pub fn receipts_for_attempt(
        &self,
        attempt_id: &str,
    ) -> Result<Vec<OperationReceipt>, rusqlite::Error> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT operation_key, method, input_hash, state_json FROM operation_receipts
             WHERE attempt_id = ?1",
        )?;
        let rows = statement.query_map(params![attempt_id], |row| {
            let operation_key: String = row.get(0)?;
            let method: String = row.get(1)?;
            let input_hash: String = row.get(2)?;
            let state_json: String = row.get(3)?;
            Ok((operation_key, method, input_hash, state_json))
        })?;
        let mut receipts = Vec::new();
        for row in rows {
            let (operation_key, method, input_hash, state_json) = row?;
            let outcome: ReceiptOutcome =
                serde_json::from_str(&state_json).unwrap_or(ReceiptOutcome::Rejected {
                    reason: "unreadable state".into(),
                });
            receipts.push(OperationReceipt {
                attempt_id: attempt_id.to_string(),
                operation_key: r_code_harness_protocol::OperationKey(operation_key),
                method,
                input_hash,
                outcome,
            });
        }
        Ok(receipts)
    }

    pub fn prepare_process_tree(
        &self,
        input: &PrepareProcessTree,
    ) -> Result<ProcessTreeRecord, ProcessTreeStoreError> {
        validate_prepare(input)?;
        let mut connection = self.connection();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = load_process_tree_from(&transaction, &input.tree_id)? {
            if existing.attempt_id == input.attempt_id
                && existing.workspace_key == input.workspace_key
                && existing.profile_id == input.profile_id
                && existing.owner == input.owner
            {
                transaction.commit()?;
                return Ok(existing);
            }
            return Err(ProcessTreeStoreError::IdentityConflict);
        }
        let epoch = next_workspace_epoch(&transaction, &input.workspace_key)?;
        let timestamp = now_ms();
        transaction.execute(
            "INSERT INTO process_trees(
                tree_id, attempt_id, workspace_key, profile_id, owner_pid,
                owner_start_identity, owner_boot_identity, platform_identity_json,
                platform_identity_digest, ownership_epoch, state, state_revision,
                quarantine_reason, migrated_observed_boot_identity,
                termination_proof_id, created_at_ms, updated_at_ms, legacy_barrier_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                     'prepared', 1, NULL, NULL, NULL, ?11, ?11, NULL)",
            params![
                input.tree_id,
                input.attempt_id,
                input.workspace_key,
                input.profile_id,
                input.owner.pid,
                input.owner.start_identity.to_string(),
                input.owner.boot_identity,
                serde_json::to_string(&input.owner.platform_identity)
                    .map_err(|_| ProcessTreeStoreError::InvalidInput)?,
                input.owner.platform_identity_digest,
                epoch,
                timestamp,
            ],
        )?;
        let record = load_process_tree_from(&transaction, &input.tree_id)?
            .ok_or(ProcessTreeStoreError::CorruptRecord)?;
        transaction.commit()?;
        Ok(record)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn transition_process_tree(
        &self,
        tree_id: &str,
        fence: &ProcessTreeFence,
        expected_state: ProcessTreeState,
        next_state: ProcessTreeState,
        quarantine_reason: Option<&str>,
        proof: Option<&NewTerminationProof>,
    ) -> Result<ProcessTreeRecord, ProcessTreeStoreError> {
        if tree_id.trim().is_empty() || !valid_transition(expected_state, next_state) {
            return Err(ProcessTreeStoreError::InvalidTransition);
        }
        let mut connection = self.connection();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current = load_process_tree_from(&transaction, tree_id)?
            .ok_or(ProcessTreeStoreError::NotFound)?;
        if transition_replay_matches(
            &transaction,
            &current,
            fence,
            expected_state,
            next_state,
            quarantine_reason,
            proof,
        )? {
            transaction.commit()?;
            return Ok(current);
        }
        if current.state != expected_state || current.fence() != *fence {
            return Err(ProcessTreeStoreError::StaleFence);
        }
        let proof_id = if next_state == ProcessTreeState::Exited {
            let proof = proof.ok_or(ProcessTreeStoreError::InvalidProof)?;
            validate_proof(&current, proof)?;
            persist_proof(&transaction, &current, proof)?;
            Some(proof.proof_id.as_str())
        } else {
            if proof.is_some() {
                return Err(ProcessTreeStoreError::InvalidProof);
            }
            None
        };
        match (next_state, quarantine_reason) {
            (ProcessTreeState::Quarantined, Some(reason)) if !reason.trim().is_empty() => {}
            (ProcessTreeState::Quarantined, _) | (_, Some(_)) => {
                return Err(ProcessTreeStoreError::InvalidInput);
            }
            (_, None) => {}
        }
        let next_revision = current
            .state_revision
            .checked_add(1)
            .ok_or(ProcessTreeStoreError::InvalidTransition)?;
        let changed = transaction.execute(
            "UPDATE process_trees SET state = ?1, state_revision = ?2,
                    quarantine_reason = ?3, termination_proof_id = ?4, updated_at_ms = ?5
             WHERE tree_id = ?6 AND ownership_epoch = ?7 AND state = ?8
               AND state_revision = ?9 AND owner_pid = ?10
               AND owner_start_identity = ?11 AND owner_boot_identity = ?12
               AND platform_identity_digest = ?13",
            params![
                next_state.as_str(),
                next_revision,
                quarantine_reason,
                proof_id,
                now_ms(),
                tree_id,
                fence.ownership_epoch,
                expected_state.as_str(),
                fence.state_revision,
                fence.owner_pid,
                fence.owner_start_identity.to_string(),
                fence.owner_boot_identity,
                fence.platform_identity_digest,
            ],
        )?;
        if changed != 1 {
            return Err(ProcessTreeStoreError::StaleFence);
        }
        if next_state == ProcessTreeState::Exited {
            if let Some(barrier_id) = current.legacy_barrier_id.as_deref() {
                transaction.execute(
                    "DELETE FROM writer_barriers WHERE barrier_id = ?1",
                    params![barrier_id],
                )?;
            }
        }
        let record = load_process_tree_from(&transaction, tree_id)?
            .ok_or(ProcessTreeStoreError::CorruptRecord)?;
        transaction.commit()?;
        Ok(record)
    }

    pub fn load_process_tree(
        &self,
        tree_id: &str,
    ) -> Result<Option<ProcessTreeRecord>, ProcessTreeStoreError> {
        load_process_tree_from(&self.connection(), tree_id)
    }

    pub fn process_trees_for_workspace(
        &self,
        workspace_key: &str,
    ) -> Result<Vec<ProcessTreeRecord>, ProcessTreeStoreError> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT tree_id FROM process_trees WHERE workspace_key = ?1
             ORDER BY ownership_epoch, tree_id",
        )?;
        let tree_ids = statement
            .query_map(params![workspace_key], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        tree_ids
            .into_iter()
            .map(|tree_id| {
                load_process_tree_from(&connection, &tree_id)?
                    .ok_or(ProcessTreeStoreError::CorruptRecord)
            })
            .collect()
    }

    /// P11R retry: re-evaluate ONE quarantined tree by its persisted
    /// identity (tree id + fence — never a bare pid) and clear it only
    /// through the existing proof-validated CAS path:
    /// * LegacyUnverifiable — mint a RebootProof bound to the tree when
    ///   the current stable BootIdentity DIFFERS from the migrated
    ///   observed boot; the actor/session audit travels inside the
    ///   proof's platform evidence. Same boot, a NULL migrated boot
    ///   (tampered/corrupt row) or corrupt rows are refused.
    /// * ordinary Quarantined — only a COMPLETE persisted termination
    ///   proof may be replayed into the Exited transition; a tree with
    ///   no proof stays blocked (no prover fabricates one post-restart).
    pub fn retry_quarantine(
        &self,
        tree_id: &str,
        current_boot_identity: &str,
        actor: &str,
        session: &str,
        now_ms: i64,
    ) -> Result<QuarantineRetryOutcome, ProcessTreeStoreError> {
        if tree_id.trim().is_empty()
            || current_boot_identity.trim().is_empty()
            || !valid_boot_identity(current_boot_identity)
            || actor.trim().is_empty()
            || session.trim().is_empty()
        {
            return Err(ProcessTreeStoreError::InvalidInput);
        }
        let tree = load_process_tree_from(&self.connection(), tree_id)?
            .ok_or(ProcessTreeStoreError::NotFound)?;
        match tree.state {
            ProcessTreeState::LegacyUnverifiable => {
                if tree.quarantine_reason.as_deref() == Some("legacy-corrupt-record") {
                    return Ok(QuarantineRetryOutcome::Refused {
                        reason: QuarantineRetryRefusal::LegacyCorrupt,
                    });
                }
                // A NULL migrated boot is a tampered/corrupt row: the
                // same-boot comparison would pass vacuously and mint a
                // meaningless RebootProof, so it refuses like any other
                // corrupt legacy record.
                match tree.migrated_observed_boot_identity.as_deref() {
                    Some(migrated) if migrated == current_boot_identity => {
                        Ok(QuarantineRetryOutcome::Refused {
                            reason: QuarantineRetryRefusal::SameBootLegacy,
                        })
                    }
                    Some(migrated) if valid_boot_identity(migrated) => {
                        // Different boot: mint the proof with this as the
                        // previous boot.
                        let proof_id =
                            format!("reboot-proof-{}-{}", tree.tree_id, tree.ownership_epoch);
                        let evidence = serde_json::json!({
                            "previousBootIdentity": migrated,
                            "currentBootIdentity": current_boot_identity,
                            "actor": actor,
                            "session": session,
                            "retriedAtMs": now_ms,
                        });
                        let proof = NewTerminationProof::bound_to_tree(
                            &proof_id,
                            TerminationProofKind::Reboot,
                            current_boot_identity,
                            &tree,
                            evidence,
                        );
                        let cleared = self.transition_process_tree(
                            tree_id,
                            &tree.fence(),
                            ProcessTreeState::LegacyUnverifiable,
                            ProcessTreeState::Exited,
                            None,
                            Some(&proof),
                        )?;
                        Ok(QuarantineRetryOutcome::Cleared {
                            proof_id: cleared.termination_proof_id.unwrap_or(proof_id),
                        })
                    }
                    _ => Ok(QuarantineRetryOutcome::Refused {
                        reason: QuarantineRetryRefusal::LegacyCorrupt,
                    }),
                }
            }
            ProcessTreeState::Quarantined => {
                // Replay path: only a COMPLETE persisted termination
                // proof may clear the tree through the same CAS — nothing
                // mints a new Exit proof here (a dead prover cannot
                // prove, so a proof-less tree stays blocked).
                let refusal = QuarantineRetryOutcome::Refused {
                    reason: QuarantineRetryRefusal::UnverifiableNoProof,
                };
                let Some(proof_id) = tree.termination_proof_id.clone() else {
                    return Ok(refusal);
                };
                if proof_id.trim().is_empty() {
                    return Ok(refusal);
                }
                let Some(record) = load_termination_proof_from(&self.connection(), &proof_id)?
                else {
                    return Ok(refusal);
                };
                let candidate = NewTerminationProof {
                    proof_id: record.proof_id.clone(),
                    kind: record.kind,
                    observed_boot_identity: record.observed_boot_identity.clone(),
                    proof_identity: record.proof_identity.clone(),
                    proof_identity_digest: record.proof_identity_digest.clone(),
                };
                if !(record.matches(&tree, &candidate)
                    && proof_identity_matches_tree(&tree, &candidate)
                    && validate_proof(&tree, &candidate).is_ok())
                {
                    return Ok(refusal);
                }
                let cleared = self.transition_process_tree(
                    tree_id,
                    &tree.fence(),
                    ProcessTreeState::Quarantined,
                    ProcessTreeState::Exited,
                    None,
                    Some(&candidate),
                )?;
                Ok(QuarantineRetryOutcome::Cleared {
                    proof_id: cleared.termination_proof_id.unwrap_or(proof_id),
                })
            }
            _ => Ok(QuarantineRetryOutcome::Refused {
                reason: QuarantineRetryRefusal::NotQuarantined,
            }),
        }
    }

    /// Read-only, redacted writer-quarantine diagnostics. A single malformed
    /// row becomes a `Corrupt` projection instead of hiding other records.
    pub fn quarantine_diagnostics(
        &self,
        workspace_key: Option<&str>,
    ) -> Result<Vec<QuarantineDiagnosticRecord>, ProcessTreeStoreError> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT tree_id, workspace_key, profile_id, owner_pid,
                    owner_start_identity, owner_boot_identity,
                    platform_identity_json, platform_identity_digest,
                    state, state_revision, quarantine_reason,
                    migrated_observed_boot_identity, termination_proof_id,
                    ownership_epoch, created_at_ms, updated_at_ms,
                    legacy_barrier_id
             FROM process_trees
             WHERE (?1 IS NULL OR workspace_key = ?1)",
        )?;
        let mut rows = statement.query(params![workspace_key])?;
        let mut diagnostics = Vec::new();
        while let Some(row) = rows.next()? {
            let raw = RawQuarantineRow::from_row(row)?;
            let Some(tree_id) = raw.tree_id.text.as_deref() else {
                diagnostics.push(raw.corrupt_projection());
                continue;
            };
            if !raw.can_load() {
                diagnostics.push(raw.corrupt_projection());
                continue;
            };
            let tree = match load_process_tree_from(&connection, tree_id) {
                Ok(Some(tree)) => tree,
                Ok(None) | Err(ProcessTreeStoreError::CorruptRecord) => {
                    diagnostics.push(raw.corrupt_projection());
                    continue;
                }
                Err(error) => return Err(error),
            };
            match has_complete_termination_proof(&connection, &tree) {
                Ok(true) => continue,
                Ok(false) | Err(ProcessTreeStoreError::CorruptRecord) => {
                    diagnostics.push(raw.projection(&tree));
                }
                Err(error) => return Err(error),
            }
        }
        diagnostics.sort_by(|left, right| {
            (&left.workspace_ref, &left.tree_ref).cmp(&(&right.workspace_ref, &right.tree_ref))
        });
        Ok(diagnostics)
    }

    pub fn load_termination_proof(
        &self,
        proof_id: &str,
    ) -> Result<Option<TerminationProofRecord>, ProcessTreeStoreError> {
        load_termination_proof_from(&self.connection(), proof_id)
    }

    pub fn migrate_legacy_writer_barriers(
        &self,
        observed_boot_identity: &str,
    ) -> Result<usize, ProcessTreeStoreError> {
        if !valid_boot_identity(observed_boot_identity) {
            return Err(ProcessTreeStoreError::InvalidInput);
        }
        let mut connection = self.connection();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let barriers = {
            let mut statement = transaction.prepare(
                "SELECT barrier_id, workspace_key, owner_pid, owner_start, reason, created_at_ms
                 FROM writer_barriers ORDER BY created_at_ms, barrier_id",
            )?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, i64>(5)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        let mut migrated = 0usize;
        for (barrier_id, workspace_key, owner_pid, owner_start, reason, created_at) in barriers {
            let existing: Option<String> = transaction
                .query_row(
                    "SELECT tree_id FROM process_trees WHERE legacy_barrier_id = ?1",
                    params![barrier_id],
                    |row| row.get(0),
                )
                .optional()?;
            if existing.is_some() {
                continue;
            }
            let parsed_owner_pid = u32::try_from(owner_pid).ok().filter(|pid| *pid > 0);
            let parsed_owner_start = owner_start.parse::<u64>().ok().filter(|start| *start > 0);
            let corrupt = barrier_id.trim().is_empty()
                || workspace_key.trim().is_empty()
                || parsed_owner_pid.is_none()
                || parsed_owner_start.is_none();
            let safe_workspace = if workspace_key.trim().is_empty() {
                format!("legacy-corrupt:{}", digest_text(&barrier_id))
            } else {
                workspace_key.clone()
            };
            let epoch = next_workspace_epoch(&transaction, &safe_workspace)?;
            let platform_identity = serde_json::json!({
                "legacyBarrierId": barrier_id,
                "ownerStart": owner_start,
                "reasonDigest": digest_text(&reason),
            });
            let platform_json = serde_json::to_string(&platform_identity)
                .map_err(|_| ProcessTreeStoreError::InvalidInput)?;
            let tree_id = format!("legacy:{}", digest_text(&barrier_id));
            transaction.execute(
                "INSERT INTO process_trees(
                    tree_id, attempt_id, workspace_key, profile_id, owner_pid,
                    owner_start_identity, owner_boot_identity, platform_identity_json,
                    platform_identity_digest, ownership_epoch, state, state_revision,
                    quarantine_reason, migrated_observed_boot_identity,
                    termination_proof_id, created_at_ms, updated_at_ms, legacy_barrier_id)
                 VALUES (?1, '<legacy-unavailable>', ?2, '<legacy>', ?3, ?4,
                         '<legacy-unavailable>', ?5, ?6, ?7, 'legacy-unverifiable',
                         1, ?8, ?9, NULL, ?10, ?10, ?11)",
                params![
                    tree_id,
                    safe_workspace,
                    parsed_owner_pid.unwrap_or(0),
                    parsed_owner_start.unwrap_or(0).to_string(),
                    platform_json,
                    r_code_harness_protocol::canonical_input_hash(&platform_identity),
                    epoch,
                    if corrupt {
                        "legacy-corrupt-record"
                    } else {
                        "legacy-writer-barrier"
                    },
                    observed_boot_identity,
                    created_at.max(0),
                    barrier_id,
                ],
            )?;
            migrated += 1;
        }
        transaction.execute(
            "INSERT OR IGNORE INTO v1_schema_migrations(migration_id, applied_at_ms)
             VALUES ('legacy-writer-barriers-to-process-trees', ?1)",
            params![now_ms()],
        )?;
        transaction.commit()?;
        Ok(migrated)
    }

    /// Persist a writer barrier for indeterminate effects.
    pub fn save_writer_barrier(
        &self,
        barrier_id: &str,
        workspace_key: &str,
        owner_pid: u32,
        owner_start: &str,
        reason: &str,
    ) -> Result<(), rusqlite::Error> {
        self.connection().execute(
            "INSERT OR REPLACE INTO writer_barriers(
                barrier_id, workspace_key, owner_pid, owner_start, reason, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                barrier_id,
                workspace_key,
                owner_pid,
                owner_start,
                reason,
                now_ms()
            ],
        )?;
        Ok(())
    }

    /// Active barriers for a workspace.
    pub fn writer_barriers(
        &self,
        workspace_key: &str,
    ) -> Result<Vec<(String, u32, String, String)>, rusqlite::Error> {
        let connection = self.connection();
        let mut statement = connection.prepare(
            "SELECT barrier_id, owner_pid, owner_start, reason FROM writer_barriers
             WHERE workspace_key = ?1",
        )?;
        let rows = statement.query_map(params![workspace_key], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })?;
        rows.collect()
    }

    /// Legacy force-clear is permanently disabled. Callers must persist an
    /// exact fenced termination proof against the migrated process tree.
    #[deprecated(note = "writer barriers require fenced process-tree proof")]
    pub fn clear_writer_barrier(&self, barrier_id: &str) -> Result<bool, rusqlite::Error> {
        let _ = barrier_id;
        Ok(false)
    }

    /// Whether the attempt has a pinned plugin available (join with the
    /// catalog by digest).
    pub fn attempt_plugin_available(
        &self,
        id: &str,
        content_digest: &str,
    ) -> Result<bool, rusqlite::Error> {
        let found: Option<i64> = self
            .connection()
            .query_row(
                "SELECT 1 FROM plugin_catalog WHERE id = ?1 AND content_digest = ?2",
                params![id, content_digest],
                |row| row.get(0),
            )
            .optional()?;
        Ok(found.is_some())
    }
}

fn validate_prepare(input: &PrepareProcessTree) -> Result<(), ProcessTreeStoreError> {
    if input.tree_id.trim().is_empty()
        || input.attempt_id.trim().is_empty()
        || input.workspace_key.trim().is_empty()
        || input.profile_id.trim().is_empty()
        || input.owner.pid == 0
        || input.owner.start_identity == 0
        || !valid_boot_identity(&input.owner.boot_identity)
        || input.owner.platform_identity.is_null()
        || input.owner.platform_identity_digest
            != r_code_harness_protocol::canonical_input_hash(&input.owner.platform_identity)
    {
        return Err(ProcessTreeStoreError::InvalidInput);
    }
    Ok(())
}

fn valid_transition(from: ProcessTreeState, to: ProcessTreeState) -> bool {
    matches!(
        (from, to),
        (ProcessTreeState::Prepared, ProcessTreeState::Running)
            | (ProcessTreeState::Prepared, ProcessTreeState::Quarantined)
            | (ProcessTreeState::Running, ProcessTreeState::Terminating)
            | (ProcessTreeState::Running, ProcessTreeState::Quarantined)
            | (ProcessTreeState::Terminating, ProcessTreeState::Exited)
            | (ProcessTreeState::Terminating, ProcessTreeState::Quarantined)
            | (ProcessTreeState::Quarantined, ProcessTreeState::Exited)
            | (
                ProcessTreeState::LegacyUnverifiable,
                ProcessTreeState::Exited
            )
    )
}

#[allow(clippy::too_many_arguments)]
fn transition_replay_matches(
    transaction: &Transaction<'_>,
    current: &ProcessTreeRecord,
    fence: &ProcessTreeFence,
    expected_state: ProcessTreeState,
    next_state: ProcessTreeState,
    quarantine_reason: Option<&str>,
    proof: Option<&NewTerminationProof>,
) -> Result<bool, ProcessTreeStoreError> {
    let Some(expected_revision) = fence.state_revision.checked_add(1) else {
        return Ok(false);
    };
    if current.state != next_state
        || current.state_revision != expected_revision
        || !same_owner_fence(current, fence)
        || current.quarantine_reason.as_deref() != quarantine_reason
    {
        return Ok(false);
    }
    if next_state != ProcessTreeState::Exited {
        return Ok(proof.is_none() && current.termination_proof_id.is_none());
    }
    let Some(proof) = proof else {
        return Ok(false);
    };
    let mut prior = current.clone();
    prior.state = expected_state;
    prior.state_revision = fence.state_revision;
    validate_proof(&prior, proof)?;
    if current.termination_proof_id.as_deref() != Some(proof.proof_id.as_str()) {
        return Ok(false);
    }
    Ok(load_termination_proof_from(transaction, &proof.proof_id)?
        .is_some_and(|persisted| persisted.matches(&prior, proof)))
}

fn same_owner_fence(tree: &ProcessTreeRecord, fence: &ProcessTreeFence) -> bool {
    tree.ownership_epoch == fence.ownership_epoch
        && tree.owner.pid == fence.owner_pid
        && tree.owner.start_identity == fence.owner_start_identity
        && tree.owner.boot_identity == fence.owner_boot_identity
        && tree.owner.platform_identity_digest == fence.platform_identity_digest
}

fn validate_proof(
    tree: &ProcessTreeRecord,
    proof: &NewTerminationProof,
) -> Result<(), ProcessTreeStoreError> {
    if proof.proof_id.trim().is_empty()
        || !valid_boot_identity(&proof.observed_boot_identity)
        || proof.proof_identity.is_null()
        || proof.proof_identity_digest
            != r_code_harness_protocol::canonical_input_hash(&proof.proof_identity)
        || !proof_identity_matches_tree(tree, proof)
    {
        return Err(ProcessTreeStoreError::InvalidProof);
    }
    match tree.state {
        ProcessTreeState::LegacyUnverifiable => {
            if tree.quarantine_reason.as_deref() == Some("legacy-corrupt-record")
                || tree.owner.pid == 0
                || tree.owner.start_identity == 0
                || proof.kind != TerminationProofKind::Reboot
                || tree.migrated_observed_boot_identity.as_deref()
                    == Some(proof.observed_boot_identity.as_str())
            {
                return Err(ProcessTreeStoreError::InvalidProof);
            }
        }
        ProcessTreeState::Quarantined if proof.kind == TerminationProofKind::Reboot => {
            if proof.observed_boot_identity == tree.owner.boot_identity {
                return Err(ProcessTreeStoreError::InvalidProof);
            }
        }
        _ => {
            if proof.kind != TerminationProofKind::Exit
                || proof.observed_boot_identity != tree.owner.boot_identity
            {
                return Err(ProcessTreeStoreError::InvalidProof);
            }
        }
    }
    Ok(())
}

fn proof_identity_matches_tree(tree: &ProcessTreeRecord, proof: &NewTerminationProof) -> bool {
    let Some(identity) = proof.proof_identity.as_object() else {
        return false;
    };
    if identity.len() != 9 {
        return false;
    }
    let expected_migrated_boot = tree
        .migrated_observed_boot_identity
        .as_ref()
        .map_or(serde_json::Value::Null, |boot| boot.clone().into());
    identity.get("treeId").and_then(serde_json::Value::as_str) == Some(tree.tree_id.as_str())
        && identity
            .get("ownershipEpoch")
            .and_then(serde_json::Value::as_u64)
            == Some(tree.ownership_epoch)
        && identity.get("ownerPid").and_then(serde_json::Value::as_u64)
            == Some(u64::from(tree.owner.pid))
        && identity
            .get("ownerStartIdentity")
            .and_then(serde_json::Value::as_u64)
            == Some(tree.owner.start_identity)
        && identity
            .get("ownerBootIdentity")
            .and_then(serde_json::Value::as_str)
            == Some(tree.owner.boot_identity.as_str())
        && identity
            .get("ownerPlatformIdentityDigest")
            .and_then(serde_json::Value::as_str)
            == Some(tree.owner.platform_identity_digest.as_str())
        && identity.get("migratedObservedBootIdentity") == Some(&expected_migrated_boot)
        && identity
            .get("observedBootIdentity")
            .and_then(serde_json::Value::as_str)
            == Some(proof.observed_boot_identity.as_str())
        && identity
            .get("platformEvidence")
            .is_some_and(|evidence| !evidence.is_null())
}

fn persist_proof(
    transaction: &Transaction<'_>,
    tree: &ProcessTreeRecord,
    proof: &NewTerminationProof,
) -> Result<(), ProcessTreeStoreError> {
    let proof_json = serde_json::to_string(&proof.proof_identity)
        .map_err(|_| ProcessTreeStoreError::InvalidProof)?;
    if let Some(existing) = load_termination_proof_from(transaction, &proof.proof_id)? {
        if existing.matches(tree, proof) {
            return Ok(());
        }
        return Err(ProcessTreeStoreError::IdentityConflict);
    }
    transaction.execute(
        "INSERT INTO termination_proofs(
            proof_id, tree_id, ownership_epoch, proof_kind,
            observed_boot_identity, proof_identity_json,
            proof_identity_digest, recorded_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            proof.proof_id,
            tree.tree_id,
            tree.ownership_epoch,
            proof.kind.as_str(),
            proof.observed_boot_identity,
            proof_json,
            proof.proof_identity_digest,
            now_ms(),
        ],
    )?;
    Ok(())
}

impl TerminationProofRecord {
    fn matches(&self, tree: &ProcessTreeRecord, proof: &NewTerminationProof) -> bool {
        self.tree_id == tree.tree_id
            && self.ownership_epoch == tree.ownership_epoch
            && self.kind == proof.kind
            && self.observed_boot_identity == proof.observed_boot_identity
            && self.proof_identity == proof.proof_identity
            && self.proof_identity_digest == proof.proof_identity_digest
    }
}

fn load_termination_proof_from(
    connection: &rusqlite::Connection,
    proof_id: &str,
) -> Result<Option<TerminationProofRecord>, ProcessTreeStoreError> {
    type Row = (String, i64, String, String, String, String, i64);
    let row: Option<Row> = connection
        .query_row(
            "SELECT tree_id, ownership_epoch, proof_kind, observed_boot_identity,
                    proof_identity_json, proof_identity_digest, recorded_at_ms
             FROM termination_proofs WHERE proof_id = ?1",
            params![proof_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()
        .map_err(classify_row_error)?;
    let Some((tree_id, epoch, kind, boot, identity_json, digest, recorded_at_ms)) = row else {
        return Ok(None);
    };
    let proof_identity: serde_json::Value =
        serde_json::from_str(&identity_json).map_err(|_| ProcessTreeStoreError::CorruptRecord)?;
    if digest != r_code_harness_protocol::canonical_input_hash(&proof_identity) {
        return Err(ProcessTreeStoreError::CorruptRecord);
    }
    let kind = match kind.as_str() {
        "exit" => TerminationProofKind::Exit,
        "reboot" => TerminationProofKind::Reboot,
        _ => return Err(ProcessTreeStoreError::CorruptRecord),
    };
    Ok(Some(TerminationProofRecord {
        proof_id: proof_id.to_string(),
        tree_id,
        ownership_epoch: u64::try_from(epoch).map_err(|_| ProcessTreeStoreError::CorruptRecord)?,
        kind,
        observed_boot_identity: boot,
        proof_identity,
        proof_identity_digest: digest,
        recorded_at_ms,
    }))
}

pub(crate) fn workspace_has_unproved_process_tree(
    connection: &rusqlite::Connection,
    workspace_key: &str,
) -> Result<bool, ProcessTreeStoreError> {
    let mut statement = connection.prepare(
        "SELECT tree_id FROM process_trees
         WHERE workspace_key = ?1 ORDER BY ownership_epoch, tree_id",
    )?;
    let tree_ids = statement
        .query_map(params![workspace_key], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for tree_id in tree_ids {
        let tree = match load_process_tree_from(connection, &tree_id) {
            Ok(Some(tree)) => tree,
            Ok(None) | Err(ProcessTreeStoreError::CorruptRecord) => return Ok(true),
            Err(error) => return Err(error),
        };
        if !has_complete_termination_proof(connection, &tree)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn has_complete_termination_proof(
    connection: &rusqlite::Connection,
    tree: &ProcessTreeRecord,
) -> Result<bool, ProcessTreeStoreError> {
    if tree.state != ProcessTreeState::Exited {
        return Ok(false);
    }
    if tree.legacy_barrier_id.is_some() {
        if tree.quarantine_reason.as_deref() == Some("legacy-corrupt-record")
            || tree.owner.pid == 0
            || tree.owner.start_identity == 0
            || !tree
                .migrated_observed_boot_identity
                .as_deref()
                .is_some_and(valid_boot_identity)
        {
            return Ok(false);
        }
    } else if tree.tree_id.trim().is_empty()
        || tree.attempt_id.trim().is_empty()
        || tree.workspace_key.trim().is_empty()
        || tree.profile_id.trim().is_empty()
        || tree.owner.pid == 0
        || tree.owner.start_identity == 0
        || !valid_boot_identity(&tree.owner.boot_identity)
    {
        return Ok(false);
    }
    let Some(proof_id) = tree.termination_proof_id.as_deref() else {
        return Ok(false);
    };
    if proof_id.trim().is_empty() {
        return Ok(false);
    }
    let proof = match load_termination_proof_from(connection, proof_id) {
        Ok(Some(proof)) => proof,
        Ok(None) | Err(ProcessTreeStoreError::CorruptRecord) => return Ok(false),
        Err(error) => return Err(error),
    };
    let candidate = NewTerminationProof {
        proof_id: proof.proof_id.clone(),
        kind: proof.kind,
        observed_boot_identity: proof.observed_boot_identity.clone(),
        proof_identity: proof.proof_identity.clone(),
        proof_identity_digest: proof.proof_identity_digest.clone(),
    };
    if !proof.matches(tree, &candidate) || !proof_identity_matches_tree(tree, &candidate) {
        return Ok(false);
    }
    let valid_boot_relationship = if let Some(migrated_boot) =
        tree.migrated_observed_boot_identity.as_deref()
    {
        proof.kind == TerminationProofKind::Reboot && proof.observed_boot_identity != migrated_boot
    } else {
        match proof.kind {
            TerminationProofKind::Exit => proof.observed_boot_identity == tree.owner.boot_identity,
            TerminationProofKind::Reboot => {
                proof.observed_boot_identity != tree.owner.boot_identity
            }
        }
    };
    Ok(valid_boot_relationship && valid_boot_identity(&proof.observed_boot_identity))
}

#[derive(Clone)]
struct RawCell {
    text: Option<String>,
    integer: Option<i64>,
    digest: String,
    is_null: bool,
}

impl RawCell {
    fn from_row(row: &rusqlite::Row<'_>, index: usize) -> Result<Self, rusqlite::Error> {
        use rusqlite::types::ValueRef;

        let value = row.get_ref(index)?;
        let (text, integer, identity, is_null) = match value {
            ValueRef::Null => (None, None, serde_json::json!({"type": "null"}), true),
            ValueRef::Integer(value) => (
                None,
                Some(value),
                serde_json::json!({"type": "integer", "value": value}),
                false,
            ),
            ValueRef::Real(value) => (
                None,
                None,
                serde_json::json!({"type": "real", "bits": format!("{:016x}", value.to_bits())}),
                false,
            ),
            ValueRef::Text(bytes) => match std::str::from_utf8(bytes) {
                Ok(value) => (
                    Some(value.to_string()),
                    None,
                    serde_json::Value::String(value.to_string()),
                    false,
                ),
                Err(_) => (
                    None,
                    None,
                    serde_json::json!({
                        "type": "invalid-text",
                        "blake3": blake3::hash(bytes).to_hex().to_string(),
                    }),
                    false,
                ),
            },
            ValueRef::Blob(bytes) => (
                None,
                None,
                serde_json::json!({
                    "type": "blob",
                    "blake3": blake3::hash(bytes).to_hex().to_string(),
                }),
                false,
            ),
        };
        Ok(Self {
            text,
            integer,
            digest: opaque_value_ref(&identity),
            is_null,
        })
    }

    fn positive_u64(&self) -> Option<u64> {
        self.integer
            .and_then(|value| u64::try_from(value).ok())
            .filter(|value| *value > 0)
    }

    fn non_negative_i64(&self) -> Option<i64> {
        self.integer.filter(|value| *value >= 0)
    }
}

struct RawQuarantineRow {
    tree_id: RawCell,
    workspace_key: RawCell,
    profile_id: RawCell,
    owner_pid: RawCell,
    owner_start: RawCell,
    owner_boot: RawCell,
    platform_identity: RawCell,
    platform_digest: RawCell,
    state: RawCell,
    state_revision: RawCell,
    reason: RawCell,
    migrated_boot: RawCell,
    proof_id: RawCell,
    ownership_epoch: RawCell,
    created_at: RawCell,
    updated_at: RawCell,
    legacy_barrier_id: RawCell,
}

impl RawQuarantineRow {
    fn from_row(row: &rusqlite::Row<'_>) -> Result<Self, rusqlite::Error> {
        Ok(Self {
            tree_id: RawCell::from_row(row, 0)?,
            workspace_key: RawCell::from_row(row, 1)?,
            profile_id: RawCell::from_row(row, 2)?,
            owner_pid: RawCell::from_row(row, 3)?,
            owner_start: RawCell::from_row(row, 4)?,
            owner_boot: RawCell::from_row(row, 5)?,
            platform_identity: RawCell::from_row(row, 6)?,
            platform_digest: RawCell::from_row(row, 7)?,
            state: RawCell::from_row(row, 8)?,
            state_revision: RawCell::from_row(row, 9)?,
            reason: RawCell::from_row(row, 10)?,
            migrated_boot: RawCell::from_row(row, 11)?,
            proof_id: RawCell::from_row(row, 12)?,
            ownership_epoch: RawCell::from_row(row, 13)?,
            created_at: RawCell::from_row(row, 14)?,
            updated_at: RawCell::from_row(row, 15)?,
            legacy_barrier_id: RawCell::from_row(row, 16)?,
        })
    }

    fn projection(&self, tree: &ProcessTreeRecord) -> QuarantineDiagnosticRecord {
        let legacy =
            tree.legacy_barrier_id.is_some() || tree.state == ProcessTreeState::LegacyUnverifiable;
        let legacy_corrupt = legacy
            && (tree.quarantine_reason.as_deref() == Some("legacy-corrupt-record")
                || tree.owner.pid == 0
                || tree.owner.start_identity == 0
                || !tree
                    .migrated_observed_boot_identity
                    .as_deref()
                    .is_some_and(valid_boot_identity));
        let unexpected_proof =
            tree.state != ProcessTreeState::Exited && tree.termination_proof_id.is_some();
        let corrupt = legacy_corrupt
            || tree.tree_id.trim().is_empty()
            || tree.workspace_key.trim().is_empty()
            || tree.profile_id.trim().is_empty()
            || tree.ownership_epoch == 0
            || tree.state_revision == 0
            || tree.created_at_ms < 0
            || tree.updated_at_ms < 0
            || tree.state == ProcessTreeState::Exited
            || unexpected_proof
            || (!legacy
                && (tree.owner.pid == 0
                    || tree.owner.start_identity == 0
                    || !valid_boot_identity(&tree.owner.boot_identity)));
        let state = match tree.state {
            ProcessTreeState::Prepared => QuarantineTreeState::Prepared,
            ProcessTreeState::Running => QuarantineTreeState::Running,
            ProcessTreeState::Terminating => QuarantineTreeState::Terminating,
            ProcessTreeState::Quarantined => QuarantineTreeState::Quarantined,
            ProcessTreeState::LegacyUnverifiable => QuarantineTreeState::LegacyUnverifiable,
            ProcessTreeState::Exited => QuarantineTreeState::ExitedUnproved,
        };
        let proof_status = if tree.state == ProcessTreeState::Exited {
            if tree.termination_proof_id.is_some() {
                QuarantineProofStatus::Invalid
            } else {
                QuarantineProofStatus::Missing
            }
        } else if unexpected_proof {
            QuarantineProofStatus::Invalid
        } else {
            QuarantineProofStatus::Missing
        };
        QuarantineDiagnosticRecord {
            tree_ref: self.tree_id.digest.clone(),
            workspace_ref: self.workspace_key.digest.clone(),
            workspace_identity: self
                .workspace_key
                .text
                .as_deref()
                .filter(|value| strict_sha256_identity(value))
                .map(str::to_string),
            profile_ref: self.profile_id.digest.clone(),
            state,
            ownership_epoch: Some(tree.ownership_epoch),
            owner_fingerprint: self.owner_fingerprint(),
            reason_code: reason_code(tree.state, corrupt, legacy_corrupt).to_string(),
            reason_digest: self.reason.digest.clone(),
            proof_status,
            legacy,
            legacy_corrupt,
            corrupt,
            created_at_ms: Some(tree.created_at_ms),
            updated_at_ms: Some(tree.updated_at_ms),
            migrated_observed_boot_identity: tree
                .migrated_observed_boot_identity
                .as_deref()
                .filter(|value| valid_boot_identity(value))
                .map(str::to_string),
        }
    }

    fn can_load(&self) -> bool {
        self.tree_id.text.is_some()
            && self.workspace_key.text.is_some()
            && self.profile_id.text.is_some()
            && self.owner_pid.integer.is_some()
            && self.owner_start.text.is_some()
            && self.owner_boot.text.is_some()
            && self.platform_identity.text.is_some()
            && self.platform_digest.text.is_some()
            && self.state.text.is_some()
            && self.state_revision.integer.is_some()
            && (self.reason.is_null || self.reason.text.is_some())
            && (self.migrated_boot.is_null || self.migrated_boot.text.is_some())
            && (self.proof_id.is_null || self.proof_id.text.is_some())
            && self.ownership_epoch.integer.is_some()
            && self.created_at.integer.is_some()
            && self.updated_at.integer.is_some()
            && (self.legacy_barrier_id.is_null || self.legacy_barrier_id.text.is_some())
    }

    fn corrupt_projection(&self) -> QuarantineDiagnosticRecord {
        let legacy = !self.legacy_barrier_id.is_null
            || self.state.text.as_deref() == Some("legacy-unverifiable")
            || self.profile_id.text.as_deref() == Some("<legacy>");
        QuarantineDiagnosticRecord {
            tree_ref: self.tree_id.digest.clone(),
            workspace_ref: self.workspace_key.digest.clone(),
            workspace_identity: self
                .workspace_key
                .text
                .as_deref()
                .filter(|value| strict_sha256_identity(value))
                .map(str::to_string),
            profile_ref: self.profile_id.digest.clone(),
            state: QuarantineTreeState::Corrupt,
            ownership_epoch: self.ownership_epoch.positive_u64(),
            owner_fingerprint: self.owner_fingerprint(),
            reason_code: "corrupt-record".to_string(),
            reason_digest: self.reason.digest.clone(),
            proof_status: QuarantineProofStatus::Corrupt,
            legacy,
            legacy_corrupt: legacy,
            corrupt: true,
            created_at_ms: self.created_at.non_negative_i64(),
            updated_at_ms: self.updated_at.non_negative_i64(),
            migrated_observed_boot_identity: self
                .migrated_boot
                .text
                .as_deref()
                .filter(|value| valid_boot_identity(value))
                .map(str::to_string),
        }
    }

    fn owner_fingerprint(&self) -> String {
        opaque_value_ref(&serde_json::json!({
            "pid": self.owner_pid.digest,
            "start": self.owner_start.digest,
            "boot": self.owner_boot.digest,
            "platform": self.platform_identity.digest,
            "platformDigest": self.platform_digest.digest,
        }))
    }
}

fn reason_code(state: ProcessTreeState, corrupt: bool, legacy_corrupt: bool) -> &'static str {
    if legacy_corrupt || corrupt {
        return "corrupt-record";
    }
    match state {
        ProcessTreeState::Prepared | ProcessTreeState::Running | ProcessTreeState::Terminating => {
            "process-tree-active"
        }
        ProcessTreeState::Quarantined => "termination-unproved",
        ProcessTreeState::LegacyUnverifiable => "legacy-writer-barrier",
        ProcessTreeState::Exited => "termination-proof-invalid",
    }
}

fn strict_sha256_identity(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn opaque_value_ref(value: &serde_json::Value) -> String {
    format!(
        "sha256:{}",
        r_code_harness_protocol::canonical_input_hash(value)
    )
}

fn load_process_tree_from(
    connection: &rusqlite::Connection,
    tree_id: &str,
) -> Result<Option<ProcessTreeRecord>, ProcessTreeStoreError> {
    type Row = (
        String,
        String,
        String,
        i64,
        String,
        String,
        String,
        String,
        i64,
        String,
        i64,
        Option<String>,
        Option<String>,
        Option<String>,
        i64,
        i64,
        Option<String>,
    );
    let row: Option<Row> = connection
        .query_row(
            "SELECT attempt_id, workspace_key, profile_id, owner_pid,
                    owner_start_identity, owner_boot_identity,
                    platform_identity_json, platform_identity_digest,
                    ownership_epoch, state, state_revision, quarantine_reason,
                    migrated_observed_boot_identity, termination_proof_id,
                    created_at_ms, updated_at_ms, legacy_barrier_id
             FROM process_trees WHERE tree_id = ?1",
            params![tree_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                    row.get(10)?,
                    row.get(11)?,
                    row.get(12)?,
                    row.get(13)?,
                    row.get(14)?,
                    row.get(15)?,
                    row.get(16)?,
                ))
            },
        )
        .optional()
        .map_err(classify_row_error)?;
    let Some((
        attempt_id,
        workspace_key,
        profile_id,
        owner_pid,
        owner_start,
        owner_boot,
        platform_json,
        platform_digest,
        epoch,
        state,
        state_revision,
        quarantine_reason,
        migrated_boot,
        proof_id,
        created_at,
        updated_at,
        legacy_barrier_id,
    )) = row
    else {
        return Ok(None);
    };
    let platform_identity: serde_json::Value =
        serde_json::from_str(&platform_json).map_err(|_| ProcessTreeStoreError::CorruptRecord)?;
    if platform_digest != r_code_harness_protocol::canonical_input_hash(&platform_identity) {
        return Err(ProcessTreeStoreError::CorruptRecord);
    }
    Ok(Some(ProcessTreeRecord {
        tree_id: tree_id.to_string(),
        attempt_id,
        workspace_key,
        profile_id,
        owner: ProcessTreeOwner {
            pid: u32::try_from(owner_pid).map_err(|_| ProcessTreeStoreError::CorruptRecord)?,
            start_identity: owner_start
                .parse()
                .map_err(|_| ProcessTreeStoreError::CorruptRecord)?,
            boot_identity: owner_boot,
            platform_identity,
            platform_identity_digest: platform_digest,
        },
        ownership_epoch: u64::try_from(epoch).map_err(|_| ProcessTreeStoreError::CorruptRecord)?,
        state: ProcessTreeState::parse(&state)?,
        state_revision: u64::try_from(state_revision)
            .map_err(|_| ProcessTreeStoreError::CorruptRecord)?,
        quarantine_reason,
        migrated_observed_boot_identity: migrated_boot,
        termination_proof_id: proof_id,
        created_at_ms: created_at,
        updated_at_ms: updated_at,
        legacy_barrier_id,
    }))
}

fn next_workspace_epoch(
    transaction: &Transaction<'_>,
    workspace_key: &str,
) -> Result<u64, ProcessTreeStoreError> {
    let current: Option<i64> = transaction
        .query_row(
            "SELECT last_epoch FROM workspace_ownership_epochs WHERE workspace_key = ?1",
            params![workspace_key],
            |row| row.get(0),
        )
        .optional()?;
    let next = current
        .unwrap_or(0)
        .checked_add(1)
        .ok_or(ProcessTreeStoreError::InvalidTransition)?;
    transaction.execute(
        "INSERT INTO workspace_ownership_epochs(workspace_key, last_epoch) VALUES (?1, ?2)
         ON CONFLICT(workspace_key) DO UPDATE SET last_epoch = excluded.last_epoch",
        params![workspace_key, next],
    )?;
    u64::try_from(next).map_err(|_| ProcessTreeStoreError::InvalidTransition)
}

fn classify_row_error(error: rusqlite::Error) -> ProcessTreeStoreError {
    match error {
        rusqlite::Error::FromSqlConversionFailure(..)
        | rusqlite::Error::IntegralValueOutOfRange(..)
        | rusqlite::Error::Utf8Error(..)
        | rusqlite::Error::InvalidColumnType(..) => ProcessTreeStoreError::CorruptRecord,
        error => ProcessTreeStoreError::from(error),
    }
}

fn valid_boot_identity(value: &str) -> bool {
    if let Some(uuid) = value
        .strip_prefix("linux:")
        .or_else(|| value.strip_prefix("windows:"))
    {
        let Ok(parsed) = uuid::Uuid::parse_str(uuid) else {
            return false;
        };
        return !parsed.is_nil() && parsed.hyphenated().to_string() == uuid;
    }
    if let Some(value) = value.strip_prefix("macos:") {
        let Some((seconds, micros)) = value.split_once(':') else {
            return false;
        };
        if micros.len() != 6
            || !seconds.bytes().all(|byte| byte.is_ascii_digit())
            || !micros.bytes().all(|byte| byte.is_ascii_digit())
        {
            return false;
        }
        let (Ok(seconds_value), Ok(micros_value)) = (seconds.parse::<i64>(), micros.parse::<i64>())
        else {
            return false;
        };
        return seconds_value > 0
            && (0..1_000_000).contains(&micros_value)
            && seconds_value.to_string() == seconds
            && format!("{micros_value:06}") == micros;
    }
    false
}

fn digest_text(value: &str) -> String {
    r_code_harness_protocol::canonical_input_hash(&serde_json::Value::String(value.to_string()))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
