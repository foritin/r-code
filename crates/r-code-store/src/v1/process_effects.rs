//! P25 — durable process-effect operations.
//!
//! The complete launch material (verbatim command, tree, lease, before
//! manifest, scan policy, ephemeral roots) is journaled BEFORE a process
//! resumes. Every advance is fenced (exact owner id and fencing epoch),
//! monotonic and idempotent: recovery reconciles against the frozen row and
//! never re-runs the command; a replayed prepare converges on the identical
//! row or is a conflict. Quarantine is coupled to the receipt — a tree
//! under quarantine stays quarantined until its operation carries a
//! terminal receipt.

use crate::v1::journal::V1Store;
use rusqlite::{params, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::fmt;

/// The monotonic operation states. Transitions: prepared -> running ->
/// receipted, with quarantined reachable from prepared/running only — once a
/// receipt exists the operation is terminal and can never be downgraded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProcessEffectState {
    Prepared,
    Running,
    Receipted,
    Quarantined,
}

impl ProcessEffectState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Running => "running",
            Self::Receipted => "receipted",
            Self::Quarantined => "quarantined",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "prepared" => Some(Self::Prepared),
            "running" => Some(Self::Running),
            "receipted" => Some(Self::Receipted),
            "quarantined" => Some(Self::Quarantined),
            _ => None,
        }
    }
}

/// The frozen launch material one operation journals before resume. Every
/// `*_json` field must parse as a JSON value: garbage is refused before it
/// can become durable, so recovery never meets an unparseable row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessEffectPrepare {
    pub operation_id: String,
    pub tree_id: String,
    pub attempt_id: String,
    pub workspace_key: String,
    pub owner_id: String,
    pub fencing_epoch: u64,
    pub command_json: String,
    pub lease_json: String,
    pub before_manifest_json: String,
    pub before_manifest_digest: String,
    pub scan_policy_json: String,
    pub ephemeral_roots_json: String,
}

/// P26A: one artifact ref an operation owns — the refcount row that keeps
/// effect-owned inverse data alive until after the terminal receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectArtifactRef {
    pub digest: String,
    pub bytes: u64,
    /// `manifest` | `before-blob` | `delta-blob` | `output-tail`.
    pub kind: String,
}

/// One durable process-effect operation row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessEffectRecord {
    pub operation_id: String,
    pub tree_id: String,
    pub attempt_id: String,
    pub workspace_key: String,
    pub owner_id: String,
    pub fencing_epoch: u64,
    pub command_json: String,
    pub lease_json: String,
    pub before_manifest_json: String,
    pub before_manifest_digest: String,
    pub scan_policy_json: String,
    pub ephemeral_roots_json: String,
    pub state: ProcessEffectState,
    pub state_revision: u64,
    pub quarantine_reason: Option<String>,
    pub receipt_digest: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// Why a process-effect operation could not be advanced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessEffectError {
    Storage(String),
    OperationNotFound,
    /// A replayed prepare carried different frozen material.
    OperationConflict,
    /// The advancing caller is not the exact (owner, epoch) that prepared.
    StaleOwner,
    InvalidTransition {
        expected: ProcessEffectState,
        actual: ProcessEffectState,
    },
}

impl fmt::Display for ProcessEffectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(reason) => write!(f, "process-effect storage failure: {reason}"),
            Self::OperationNotFound => write!(f, "process-effect operation not found"),
            Self::OperationConflict => write!(f, "process-effect operation conflict"),
            Self::StaleOwner => write!(
                f,
                "stale owner: the exact owner and fencing epoch are required"
            ),
            Self::InvalidTransition { expected, actual } => write!(
                f,
                "invalid process-effect transition: expected {}, found {}",
                expected.as_str(),
                actual.as_str()
            ),
        }
    }
}

impl std::error::Error for ProcessEffectError {}

fn storage(error: rusqlite::Error) -> ProcessEffectError {
    ProcessEffectError::Storage(error.to_string())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// Refuse material that could not survive recovery: every JSON field must
/// parse and the before-manifest digest must be a non-empty pin.
fn validate_prepare(input: &ProcessEffectPrepare) -> Result<(), ProcessEffectError> {
    let json_fields = [
        &input.command_json,
        &input.lease_json,
        &input.before_manifest_json,
        &input.scan_policy_json,
        &input.ephemeral_roots_json,
    ];
    if json_fields
        .iter()
        .any(|text| serde_json::from_str::<serde_json::Value>(text).is_err())
    {
        return Err(ProcessEffectError::OperationConflict);
    }
    if input.before_manifest_digest.trim().is_empty()
        || input.operation_id.trim().is_empty()
        || input.tree_id.trim().is_empty()
    {
        return Err(ProcessEffectError::OperationConflict);
    }
    Ok(())
}

fn row_from(operation_id: &str, row: &rusqlite::Row<'_>) -> rusqlite::Result<ProcessEffectRecord> {
    let state: String = row.get("state")?;
    Ok(ProcessEffectRecord {
        operation_id: operation_id.to_string(),
        tree_id: row.get("tree_id")?,
        attempt_id: row.get("attempt_id")?,
        workspace_key: row.get("workspace_key")?,
        owner_id: row.get("owner_id")?,
        fencing_epoch: row.get::<_, i64>("fencing_epoch")? as u64,
        command_json: row.get("command_json")?,
        lease_json: row.get("lease_json")?,
        before_manifest_json: row.get("before_manifest_json")?,
        before_manifest_digest: row.get("before_manifest_digest")?,
        scan_policy_json: row.get("scan_policy_json")?,
        ephemeral_roots_json: row.get("ephemeral_roots_json")?,
        state: ProcessEffectState::parse(&state).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("unknown process-effect state {state:?}").into(),
            )
        })?,
        state_revision: row.get::<_, i64>("state_revision")? as u64,
        quarantine_reason: row.get("quarantine_reason")?,
        receipt_digest: row.get("receipt_digest")?,
        created_at_ms: row.get("created_at_ms")?,
        updated_at_ms: row.get("updated_at_ms")?,
    })
}

const SELECT_COLUMNS: &str = "tree_id, attempt_id, workspace_key, owner_id, \
     fencing_epoch, command_json, lease_json, before_manifest_json, \
     before_manifest_digest, scan_policy_json, ephemeral_roots_json, state, \
     state_revision, quarantine_reason, receipt_digest, created_at_ms, updated_at_ms";

fn load_from(
    transaction: &rusqlite::Transaction<'_>,
    operation_id: &str,
) -> Result<Option<ProcessEffectRecord>, ProcessEffectError> {
    let sql =
        format!("SELECT {SELECT_COLUMNS} FROM process_effect_operations WHERE operation_id = ?1");
    let mut statement = transaction.prepare(&sql).map_err(storage)?;
    let mut rows = statement.query(params![operation_id]).map_err(storage)?;
    match rows.next().map_err(storage)? {
        Some(row) => Ok(Some(row_from(operation_id, row).map_err(storage)?)),
        None => Ok(None),
    }
}

/// The frozen fields that make a replayed prepare the same operation.
fn same_frozen_material(left: &ProcessEffectRecord, input: &ProcessEffectPrepare) -> bool {
    left.tree_id == input.tree_id
        && left.attempt_id == input.attempt_id
        && left.workspace_key == input.workspace_key
        && left.owner_id == input.owner_id
        && left.fencing_epoch == input.fencing_epoch
        && left.command_json == input.command_json
        && left.lease_json == input.lease_json
        && left.before_manifest_json == input.before_manifest_json
        && left.before_manifest_digest == input.before_manifest_digest
        && left.scan_policy_json == input.scan_policy_json
        && left.ephemeral_roots_json == input.ephemeral_roots_json
}

/// The exact (owner, epoch) that prepared the operation; anyone else is
/// stale, matching the mutation-operation fence.
fn require_owner(
    record: &ProcessEffectRecord,
    owner_id: &str,
    fencing_epoch: u64,
) -> Result<(), ProcessEffectError> {
    if record.owner_id == owner_id && record.fencing_epoch == fencing_epoch {
        Ok(())
    } else {
        Err(ProcessEffectError::StaleOwner)
    }
}

impl V1Store {
    /// Journal the complete launch material before resume. A byte-identical
    /// replay converges on the existing row; any difference is a conflict —
    /// an operation id never carries two launch materials.
    pub fn prepare_process_effect(
        &self,
        input: ProcessEffectPrepare,
    ) -> Result<ProcessEffectRecord, ProcessEffectError> {
        validate_prepare(&input)?;
        let mut connection = self.connection();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        if let Some(existing) = load_from(&transaction, &input.operation_id)? {
            if same_frozen_material(&existing, &input) {
                return Ok(existing);
            }
            return Err(ProcessEffectError::OperationConflict);
        }
        let now = now_ms();
        transaction
            .execute(
                "INSERT INTO process_effect_operations (
                     operation_id, tree_id, attempt_id, workspace_key, owner_id,
                     fencing_epoch, command_json, lease_json, before_manifest_json,
                     before_manifest_digest, scan_policy_json, ephemeral_roots_json,
                     state, state_revision, created_at_ms, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                           'prepared', 1, ?13, ?13)",
                params![
                    input.operation_id,
                    input.tree_id,
                    input.attempt_id,
                    input.workspace_key,
                    input.owner_id,
                    input.fencing_epoch as i64,
                    input.command_json,
                    input.lease_json,
                    input.before_manifest_json,
                    input.before_manifest_digest,
                    input.scan_policy_json,
                    input.ephemeral_roots_json,
                    now,
                ],
            )
            .map_err(storage)?;
        transaction.commit().map_err(storage)?;
        Ok(ProcessEffectRecord {
            state: ProcessEffectState::Prepared,
            state_revision: 1,
            quarantine_reason: None,
            receipt_digest: None,
            created_at_ms: now,
            updated_at_ms: now,
            operation_id: input.operation_id,
            tree_id: input.tree_id,
            attempt_id: input.attempt_id,
            workspace_key: input.workspace_key,
            owner_id: input.owner_id,
            fencing_epoch: input.fencing_epoch,
            command_json: input.command_json,
            lease_json: input.lease_json,
            before_manifest_json: input.before_manifest_json,
            before_manifest_digest: input.before_manifest_digest,
            scan_policy_json: input.scan_policy_json,
            ephemeral_roots_json: input.ephemeral_roots_json,
        })
    }

    /// Prepared -> Running. Idempotent in Running; fenced; anything else is
    /// an invalid transition.
    pub fn mark_process_effect_running(
        &self,
        operation_id: &str,
        owner_id: &str,
        fencing_epoch: u64,
    ) -> Result<ProcessEffectRecord, ProcessEffectError> {
        self.advance(
            operation_id,
            owner_id,
            fencing_epoch,
            ProcessEffectState::Running,
            &[ProcessEffectState::Prepared],
            |record| matches!(record.state, ProcessEffectState::Running),
            "UPDATE process_effect_operations
             SET state = 'running', state_revision = state_revision + 1,
                 updated_at_ms = ?2
             WHERE operation_id = ?1",
            None,
            None,
        )
    }

    /// Running -> Receipted, pinning the receipt digest. Idempotent for the
    /// same digest; a different digest on a receipted operation is a
    /// conflict; quarantined operations can never gain a receipt.
    pub fn record_process_effect_receipt(
        &self,
        operation_id: &str,
        owner_id: &str,
        fencing_epoch: u64,
        receipt_digest: &str,
    ) -> Result<ProcessEffectRecord, ProcessEffectError> {
        if receipt_digest.trim().is_empty() {
            return Err(ProcessEffectError::OperationConflict);
        }
        self.advance(
            operation_id,
            owner_id,
            fencing_epoch,
            ProcessEffectState::Receipted,
            &[ProcessEffectState::Running],
            |record| {
                record.state == ProcessEffectState::Receipted
                    && record.receipt_digest.as_deref() == Some(receipt_digest)
            },
            "UPDATE process_effect_operations
             SET state = 'receipted', receipt_digest = ?3,
                 state_revision = state_revision + 1, updated_at_ms = ?2
             WHERE operation_id = ?1",
            Some(receipt_digest),
            // The receipt is terminal: the operation's disk reservations
            // release in the same transaction that records it.
            Some(
                "UPDATE artifact_reservations
                 SET released_at_ms = ?2
                 WHERE operation_id = ?1 AND released_at_ms IS NULL",
            ),
        )
    }

    /// Prepared/Running -> Quarantined with a reason. A receipted operation
    /// is terminal and can never be quarantined.
    pub fn quarantine_process_effect(
        &self,
        operation_id: &str,
        owner_id: &str,
        fencing_epoch: u64,
        reason: &str,
    ) -> Result<ProcessEffectRecord, ProcessEffectError> {
        if reason.trim().is_empty() {
            return Err(ProcessEffectError::OperationConflict);
        }
        self.advance(
            operation_id,
            owner_id,
            fencing_epoch,
            ProcessEffectState::Quarantined,
            &[ProcessEffectState::Prepared, ProcessEffectState::Running],
            |record| {
                matches!(record.state, ProcessEffectState::Quarantined)
                    && record.quarantine_reason.as_deref() == Some(reason)
            },
            "UPDATE process_effect_operations
             SET state = 'quarantined', quarantine_reason = ?3,
                 state_revision = state_revision + 1, updated_at_ms = ?2
             WHERE operation_id = ?1 AND state IN ('prepared', 'running')",
            Some(reason),
            None,
        )
    }

    /// One operation by id.
    pub fn load_process_effect(
        &self,
        operation_id: &str,
    ) -> Result<Option<ProcessEffectRecord>, ProcessEffectError> {
        let mut connection = self.connection();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(storage)?;
        load_from(&transaction, operation_id)
    }

    /// The startup recovery query: every operation still incomplete
    /// (prepared or running), oldest first — these are the effects a
    /// restart must reconcile, and the only source of their commands.
    pub fn incomplete_process_effects(
        &self,
    ) -> Result<Vec<ProcessEffectRecord>, ProcessEffectError> {
        let connection = self.connection();
        let sql = format!(
            "SELECT operation_id, {SELECT_COLUMNS}
             FROM process_effect_operations
             WHERE state IN ('prepared', 'running')
             ORDER BY created_at_ms, operation_id"
        );
        let mut statement = connection.prepare(&sql).map_err(storage)?;
        let records = statement
            .query_map([], |row| -> rusqlite::Result<ProcessEffectRecord> {
                let operation_id: String = row.get(0)?;
                row_from(&operation_id, row)
            })
            .map_err(storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage)?;
        Ok(records)
    }

    /// The latest operation journaled for one tree.
    pub fn process_effect_for_tree(
        &self,
        tree_id: &str,
    ) -> Result<Option<ProcessEffectRecord>, ProcessEffectError> {
        let connection = self.connection();
        let sql = format!(
            "SELECT operation_id, {SELECT_COLUMNS}
             FROM process_effect_operations
             WHERE tree_id = ?1
             ORDER BY created_at_ms DESC, operation_id DESC
             LIMIT 1"
        );
        let mut statement = connection.prepare(&sql).map_err(storage)?;
        let mut rows = statement.query(params![tree_id]).map_err(storage)?;
        match rows.next().map_err(storage)? {
            Some(row) => {
                let operation_id: String = row.get(0).map_err(storage)?;
                Ok(Some(row_from(&operation_id, row).map_err(storage)?))
            }
            None => Ok(None),
        }
    }

    /// P25.3, the quarantine/receipt coupling: a tree under quarantine may
    /// be lifted only when its journaled operation (if any) carries a
    /// terminal receipt. No operation or an unreceipted one keeps the
    /// quarantine in force.
    pub fn tree_quarantine_lift_eligible(&self, tree_id: &str) -> Result<bool, ProcessEffectError> {
        match self.process_effect_for_tree(tree_id)? {
            Some(record) => Ok(record.state == ProcessEffectState::Receipted),
            None => Ok(false),
        }
    }

    /// P26A: journal the artifact refs one operation owns (the refcount
    /// that keeps effect-owned inverse data alive). Idempotent for the
    /// same rows; a divergent row for the same (operation, digest) is a
    /// conflict. Fenced by the exact owner that prepared the operation.
    pub fn own_effect_artifacts(
        &self,
        operation_id: &str,
        owner_id: &str,
        fencing_epoch: u64,
        refs: &[EffectArtifactRef],
    ) -> Result<(), ProcessEffectError> {
        let mut connection = self.connection();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let record =
            load_from(&transaction, operation_id)?.ok_or(ProcessEffectError::OperationNotFound)?;
        require_owner(&record, owner_id, fencing_epoch)?;
        for reference in refs {
            if reference.digest.trim().is_empty() {
                return Err(ProcessEffectError::OperationConflict);
            }
            let existing: Option<(i64, String)> = transaction
                .query_row(
                    "SELECT bytes, kind FROM effect_artifact_refs
                     WHERE operation_id = ?1 AND digest = ?2",
                    params![operation_id, reference.digest],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map(Some)
                .or_else(|error| match error {
                    rusqlite::Error::QueryReturnedNoRows => Ok(None),
                    other => Err(other),
                })
                .map_err(storage)?;
            match existing {
                Some((bytes, kind)) => {
                    if bytes != reference.bytes as i64 || kind != reference.kind {
                        return Err(ProcessEffectError::OperationConflict);
                    }
                }
                None => {
                    transaction
                        .execute(
                            "INSERT INTO effect_artifact_refs
                             (operation_id, digest, bytes, kind, created_at_ms)
                             VALUES (?1, ?2, ?3, ?4, ?5)",
                            params![
                                operation_id,
                                reference.digest,
                                reference.bytes as i64,
                                reference.kind,
                                now_ms()
                            ],
                        )
                        .map_err(storage)?;
                }
            }
        }
        transaction.commit().map_err(storage)?;
        Ok(())
    }

    /// P26A: release artifact refs, fenced and ONLY from a receipted
    /// operation — active inverse data is never collectable.
    pub fn release_effect_artifacts(
        &self,
        operation_id: &str,
        owner_id: &str,
        fencing_epoch: u64,
        digests: &[&str],
    ) -> Result<usize, ProcessEffectError> {
        let mut connection = self.connection();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let record =
            load_from(&transaction, operation_id)?.ok_or(ProcessEffectError::OperationNotFound)?;
        require_owner(&record, owner_id, fencing_epoch)?;
        if record.state != ProcessEffectState::Receipted {
            return Err(ProcessEffectError::InvalidTransition {
                expected: ProcessEffectState::Receipted,
                actual: record.state,
            });
        }
        let mut released = 0;
        for digest in digests {
            released += transaction
                .execute(
                    "DELETE FROM effect_artifact_refs
                     WHERE operation_id = ?1 AND digest = ?2",
                    params![operation_id, digest],
                )
                .map_err(storage)?;
        }
        transaction.commit().map_err(storage)?;
        Ok(released)
    }

    /// Whether ANY active ref row holds one digest (the GC liveness check).
    pub fn effect_artifact_is_live(&self, digest: &str) -> Result<bool, ProcessEffectError> {
        let connection = self.connection();
        let count: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM effect_artifact_refs WHERE digest = ?1",
                params![digest],
                |row| row.get(0),
            )
            .map_err(storage)?;
        Ok(count > 0)
    }

    /// The bytes currently reserved by active (unreleased) reservations.
    pub fn reserved_active_bytes(&self) -> Result<u64, ProcessEffectError> {
        let connection = self.connection();
        let total: Option<i64> = connection
            .query_row(
                "SELECT SUM(bytes) FROM artifact_reservations
                 WHERE released_at_ms IS NULL",
                [],
                |row| row.get(0),
            )
            .map_err(storage)?;
        Ok(total.map(|value| value.max(0) as u64).unwrap_or(0))
    }

    /// P26A.1: reserve disk for one operation before resume. Idempotent for
    /// the same (reservation id, operation, bytes); anything else conflicts.
    pub fn reserve_effect_disk(
        &self,
        reservation_id: &str,
        operation_id: &str,
        bytes: u64,
    ) -> Result<(), ProcessEffectError> {
        if bytes == 0 || reservation_id.trim().is_empty() {
            return Err(ProcessEffectError::OperationConflict);
        }
        let mut connection = self.connection();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let existing: Option<(String, i64)> = transaction
            .query_row(
                "SELECT operation_id, bytes FROM artifact_reservations
                 WHERE reservation_id = ?1",
                params![reservation_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map(Some)
            .or_else(|error| match error {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })
            .map_err(storage)?;
        match existing {
            Some((operation, reserved))
                if operation == operation_id && reserved == bytes as i64 =>
            {
                return Ok(());
            }
            Some(_) => return Err(ProcessEffectError::OperationConflict),
            None => {}
        }
        transaction
            .execute(
                "INSERT INTO artifact_reservations
                 (reservation_id, operation_id, bytes, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4)",
                params![reservation_id, operation_id, bytes as i64, now_ms()],
            )
            .map_err(storage)?;
        transaction.commit().map_err(storage)?;
        Ok(())
    }

    /// The shared fenced, monotonic advance. `idempotent` decides whether the
    /// current state already satisfies the request (converge); `permissible`
    /// holds the states the transition may start from, and `target` is the
    /// state it lands in — the returned record mirrors the committed row.
    /// `extra_update` runs inside the SAME transaction after the state
    /// change (the receipt releases the operation's disk reservations).
    #[allow(clippy::too_many_arguments)]
    fn advance(
        &self,
        operation_id: &str,
        owner_id: &str,
        fencing_epoch: u64,
        target: ProcessEffectState,
        permissible: &[ProcessEffectState],
        idempotent: impl Fn(&ProcessEffectRecord) -> bool,
        update_sql: &str,
        third_parameter: Option<&str>,
        extra_update: Option<&str>,
    ) -> Result<ProcessEffectRecord, ProcessEffectError> {
        let mut connection = self.connection();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage)?;
        let record =
            load_from(&transaction, operation_id)?.ok_or(ProcessEffectError::OperationNotFound)?;
        require_owner(&record, owner_id, fencing_epoch)?;
        if idempotent(&record) {
            return Ok(record);
        }
        if !permissible.contains(&record.state) {
            return Err(ProcessEffectError::InvalidTransition {
                expected: permissible[0],
                actual: record.state,
            });
        }
        let now = now_ms();
        let changed = match third_parameter {
            Some(value) => transaction
                .execute(update_sql, params![operation_id, now, value])
                .map_err(storage)?,
            None => transaction
                .execute(update_sql, params![operation_id, now])
                .map_err(storage)?,
        };
        if changed != 1 {
            return Err(ProcessEffectError::Storage(
                "process-effect update changed no row".into(),
            ));
        }
        if let Some(extra) = extra_update {
            transaction
                .execute(extra, params![operation_id, now])
                .map_err(storage)?;
        }
        transaction.commit().map_err(storage)?;
        let mut advanced = record;
        advanced.state = target;
        advanced.state_revision += 1;
        advanced.updated_at_ms = now;
        match target {
            ProcessEffectState::Receipted => {
                advanced.receipt_digest = third_parameter.map(str::to_string);
            }
            ProcessEffectState::Quarantined => {
                advanced.quarantine_reason = third_parameter.map(str::to_string);
            }
            ProcessEffectState::Prepared | ProcessEffectState::Running => {}
        }
        Ok(advanced)
    }
}
