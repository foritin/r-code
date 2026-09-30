//! P12 — persistent, content-addressed SafetyCapabilityReports. The store
//! owns rows, heads and pruning; canonical hashing and the activation
//! predicate live in `r_code_runtime::services::sandbox` (the runtime is
//! the only consumer that can regenerate report material).

use crate::v1::V1Store;
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};

/// Coarse persisted status of a report. The rich material (reason strings,
/// probes) lives inside `material_json`; this column exists so stale or
/// disabled reports are visible without parsing JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SafetyReportStatus {
    Unsupported,
    SafeDisabled,
    Activated,
}

impl SafetyReportStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::SafeDisabled => "safe-disabled",
            Self::Activated => "activated",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "unsupported" => Some(Self::Unsupported),
            "safe-disabled" => Some(Self::SafeDisabled),
            "activated" => Some(Self::Activated),
            _ => None,
        }
    }
}

/// One persisted report row (P12.1). `report_id` is derived from the
/// material digest by the runtime; the store refuses ids that do not carry
/// the `safety-` prefix and a 64-hex digest so a foreign row can never pose
/// as a content-addressed report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyReportRecord {
    pub report_id: String,
    pub capability: String,
    pub material_digest: String,
    pub status: SafetyReportStatus,
    pub material_json: String,
    pub created_at_ms: i64,
}

fn is_hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_record(record: &SafetyReportRecord) -> Result<(), String> {
    if !record.report_id.starts_with("safety-")
        || !is_hex_digest(&record.report_id["safety-".len()..])
        || !is_hex_digest(&record.material_digest)
        || record.capability.trim().is_empty()
        || record.material_json.trim().is_empty()
    {
        return Err("safety report record is not content-addressed".to_string());
    }
    Ok(())
}

impl V1Store {
    /// Persist one report and point the capability head at it, atomically
    /// (P12.2). Regenerating identical material is idempotent: the row is
    /// replaced with itself and the head already matches — no duplicate
    /// rows, no history churn.
    pub fn put_safety_report(&self, record: SafetyReportRecord) -> Result<(), String> {
        validate_record(&record)?;
        let mut connection = self.connection();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        put_safety_report_tx(&transaction, &record)?;
        transaction.commit().map_err(|error| error.to_string())
    }

    /// The capability's current head report, if any (reload path).
    pub fn current_safety_report(
        &self,
        capability: &str,
    ) -> Result<Option<SafetyReportRecord>, String> {
        let connection = self.connection();
        connection
            .query_row(
                "SELECT r.report_id, r.capability, r.material_digest, r.status,
                        r.material_json, r.created_at_ms
                 FROM safety_report_heads h
                 JOIN safety_capability_reports r ON r.report_id = h.report_id
                 WHERE h.capability = ?1",
                params![capability],
                parse_record,
            )
            .optional()
            .map_err(|error| error.to_string())
    }

    /// Every capability that currently has a head report (diagnostics
    /// listing; sorted for determinism).
    pub fn safety_report_capabilities(&self) -> Result<Vec<String>, String> {
        let connection = self.connection();
        let mut statement = connection
            .prepare("SELECT capability FROM safety_report_heads ORDER BY capability")
            .map_err(|error| error.to_string())?;
        let capabilities = statement
            .query_map([], |row| row.get(0))
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<String>, _>>()
            .map_err(|error: rusqlite::Error| error.to_string())?;
        Ok(capabilities)
    }

    /// Every persisted report for a capability, oldest first (audit view).
    pub fn safety_report_history(
        &self,
        capability: &str,
    ) -> Result<Vec<SafetyReportRecord>, String> {
        let connection = self.connection();
        let mut statement = connection
            .prepare(
                "SELECT report_id, capability, material_digest, status, material_json,
                        created_at_ms
                 FROM safety_capability_reports
                 WHERE capability = ?1
                 ORDER BY created_at_ms, report_id",
            )
            .map_err(|error| error.to_string())?;
        let records = statement
            .query_map(params![capability], parse_record)
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error: rusqlite::Error| error.to_string())?;
        Ok(records)
    }

    /// Invalidate stale rows (P12.2): re-point the capability head at the
    /// kept report, THEN delete every other report of the capability (the
    /// head upsert must precede the delete — with foreign keys on, deleting
    /// a row the head still references fails the FK constraint). Returns
    /// the number of rows removed. A missing `keep` row is an error —
    /// pruning must never leave a head without its report.
    pub fn prune_safety_reports(&self, keep_report_id: &str) -> Result<usize, String> {
        let mut connection = self.connection();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let capability: String = transaction
            .query_row(
                "SELECT capability FROM safety_capability_reports WHERE report_id = ?1",
                params![keep_report_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error: rusqlite::Error| error.to_string())?
            .ok_or_else(|| "report to keep does not exist".to_string())?;
        transaction
            .execute(
                "INSERT INTO safety_report_heads(capability, report_id) VALUES (?1, ?2)
                 ON CONFLICT(capability) DO UPDATE SET report_id = excluded.report_id",
                params![capability, keep_report_id],
            )
            .map_err(|error| error.to_string())?;
        let removed = transaction
            .execute(
                "DELETE FROM safety_capability_reports
                 WHERE capability = ?1 AND report_id <> ?2",
                params![capability, keep_report_id],
            )
            .map_err(|error| error.to_string())?;
        transaction.commit().map_err(|error| error.to_string())?;
        Ok(removed)
    }
}

fn put_safety_report_tx(
    transaction: &Transaction,
    record: &SafetyReportRecord,
) -> Result<(), String> {
    transaction
        .execute(
            "INSERT INTO safety_capability_reports(
                 report_id, capability, material_digest, status, material_json, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(report_id) DO UPDATE SET
                 capability = excluded.capability,
                 material_digest = excluded.material_digest,
                 status = excluded.status,
                 material_json = excluded.material_json,
                 created_at_ms = excluded.created_at_ms",
            params![
                record.report_id,
                record.capability,
                record.material_digest,
                record.status.as_str(),
                record.material_json,
                record.created_at_ms
            ],
        )
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "INSERT INTO safety_report_heads(capability, report_id) VALUES (?1, ?2)
             ON CONFLICT(capability) DO UPDATE SET report_id = excluded.report_id",
            params![record.capability, record.report_id],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn parse_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<SafetyReportRecord> {
    let status: String = row.get(3)?;
    Ok(SafetyReportRecord {
        report_id: row.get(0)?,
        capability: row.get(1)?,
        material_digest: row.get(2)?,
        status: SafetyReportStatus::parse(&status).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                "unknown safety report status".into(),
            )
        })?,
        material_json: row.get(4)?,
        created_at_ms: row.get(5)?,
    })
}

// P14 — journaled ACL deltas ---------------------------------------------

/// Lifecycle of one ACL operation. `Conflict` is terminal: an external
/// edit is preserved verbatim and never overwritten.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AclOperationState {
    Prepared,
    Applied,
    Restored,
    Conflict,
}

impl AclOperationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Applied => "applied",
            Self::Restored => "restored",
            Self::Conflict => "conflict",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "prepared" => Some(Self::Prepared),
            "applied" => Some(Self::Applied),
            "restored" => Some(Self::Restored),
            "conflict" => Some(Self::Conflict),
            _ => None,
        }
    }
}

/// One journaled ACL delta (P14.1): physical identity, the Before
/// self-relative descriptor bytes and the canonical PlannedDelta are
/// persisted BEFORE any mutation; ActualAfter is read back after apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AclOperationRecord {
    pub operation_id: String,
    pub target_path: String,
    pub physical_identity: String,
    pub state: AclOperationState,
    pub before_descriptor: Vec<u8>,
    pub planned_delta: String,
    pub actual_after: Option<Vec<u8>>,
    pub conflict_reason: Option<String>,
    pub prepared_at_ms: i64,
    pub applied_at_ms: Option<i64>,
    pub settled_at_ms: Option<i64>,
}

fn validate_acl_record(record: &AclOperationRecord) -> Result<(), String> {
    let valid_prepared = record.state == AclOperationState::Prepared
        && record.actual_after.is_none()
        && record.conflict_reason.is_none()
        && record.applied_at_ms.is_none()
        && record.settled_at_ms.is_none();
    if !record.operation_id.starts_with("acl-")
        || record.target_path.trim().is_empty()
        || record.physical_identity.trim().is_empty()
        || record.before_descriptor.is_empty()
        || record.planned_delta.trim().is_empty()
        || !valid_prepared
    {
        return Err("acl operation record is not a valid prepared record".to_string());
    }
    Ok(())
}

impl V1Store {
    /// Persist the operation BEFORE any ACL mutation (P14.1). A
    /// same-operation-id retry with different content is an error; with
    /// identical content it is idempotent.
    pub fn prepare_acl_operation(&self, record: AclOperationRecord) -> Result<(), String> {
        validate_acl_record(&record)?;
        let mut connection = self.connection();
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| error.to_string())?;
        let existing = transaction
            .query_row(
                "SELECT before_descriptor, planned_delta, physical_identity
                 FROM acl_operations WHERE operation_id = ?1",
                params![record.operation_id],
                |row| {
                    Ok((
                        row.get::<_, Vec<u8>>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|error: rusqlite::Error| error.to_string())?;
        if let Some((before, delta, identity)) = existing {
            if before != record.before_descriptor
                || delta != record.planned_delta
                || identity != record.physical_identity
                || record.target_path
                    != transaction
                        .query_row(
                            "SELECT target_path FROM acl_operations WHERE operation_id = ?1",
                            params![record.operation_id],
                            |row| row.get::<_, String>(0),
                        )
                        .map_err(|error: rusqlite::Error| error.to_string())?
            {
                return Err("acl operation identity conflict".to_string());
            }
            return Ok(());
        }
        transaction
            .execute(
                "INSERT INTO acl_operations(
                     operation_id, target_path, physical_identity, state,
                     before_descriptor, planned_delta, actual_after, conflict_reason,
                     prepared_at_ms, applied_at_ms, settled_at_ms)
                 VALUES (?1, ?2, ?3, 'prepared', ?4, ?5, NULL, NULL, ?6, NULL, NULL)",
                params![
                    record.operation_id,
                    record.target_path,
                    record.physical_identity,
                    record.before_descriptor,
                    record.planned_delta,
                    record.prepared_at_ms
                ],
            )
            .map_err(|error| error.to_string())?;
        transaction.commit().map_err(|error| error.to_string())
    }

    /// Record the read-back ActualAfter (P14.2): only from Prepared.
    pub fn mark_acl_applied(
        &self,
        operation_id: &str,
        actual_after: Vec<u8>,
        applied_at_ms: i64,
    ) -> Result<(), String> {
        let connection = self.connection();
        let changed = connection
            .execute(
                "UPDATE acl_operations
                 SET state = 'applied', actual_after = ?2, applied_at_ms = ?3
                 WHERE operation_id = ?1 AND state = 'prepared'",
                params![operation_id, actual_after, applied_at_ms],
            )
            .map_err(|error| error.to_string())?;
        if changed == 0 {
            return Err("acl operation is not in the prepared state".to_string());
        }
        Ok(())
    }

    /// Settle one operation: Restored (only from Applied — the CAS rule
    /// was satisfied and Before was reinstalled) or Conflict (from
    /// Prepared or Applied; the reason is preserved verbatim).
    pub fn settle_acl_operation(
        &self,
        operation_id: &str,
        restored: bool,
        reason: Option<&str>,
        settled_at_ms: i64,
    ) -> Result<(), String> {
        let connection = self.connection();
        let from_state = if restored {
            "applied"
        } else {
            "prepared', 'applied"
        };
        let changed = connection
            .execute(
                &format!(
                    "UPDATE acl_operations
                     SET state = ?2, conflict_reason = ?3, settled_at_ms = ?4
                     WHERE operation_id = ?1 AND state IN ('{from_state}')"
                ),
                params![
                    operation_id,
                    if restored { "restored" } else { "conflict" },
                    reason,
                    settled_at_ms
                ],
            )
            .map_err(|error| error.to_string())?;
        if changed == 0 {
            return Err("acl operation is not settleable from its state".to_string());
        }
        Ok(())
    }

    pub fn load_acl_operation(
        &self,
        operation_id: &str,
    ) -> Result<Option<AclOperationRecord>, String> {
        let connection = self.connection();
        connection
            .query_row(
                "SELECT operation_id, target_path, physical_identity, state,
                        before_descriptor, planned_delta, actual_after, conflict_reason,
                        prepared_at_ms, applied_at_ms, settled_at_ms
                 FROM acl_operations WHERE operation_id = ?1",
                params![operation_id],
                parse_acl_record,
            )
            .optional()
            .map_err(|error: rusqlite::Error| error.to_string())
    }

    /// Every operation for a target, oldest first (recovery/diagnostics).
    pub fn acl_operations_for_target(
        &self,
        target_path: &str,
    ) -> Result<Vec<AclOperationRecord>, String> {
        let connection = self.connection();
        let mut statement = connection
            .prepare(
                "SELECT operation_id, target_path, physical_identity, state,
                        before_descriptor, planned_delta, actual_after, conflict_reason,
                        prepared_at_ms, applied_at_ms, settled_at_ms
                 FROM acl_operations WHERE target_path = ?1
                 ORDER BY prepared_at_ms, operation_id",
            )
            .map_err(|error| error.to_string())?;
        let records = statement
            .query_map(params![target_path], parse_acl_record)
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error: rusqlite::Error| error.to_string())?;
        Ok(records)
    }
}

fn parse_acl_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<AclOperationRecord> {
    let state: String = row.get(3)?;
    Ok(AclOperationRecord {
        operation_id: row.get(0)?,
        target_path: row.get(1)?,
        physical_identity: row.get(2)?,
        state: AclOperationState::parse(&state).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                3,
                rusqlite::types::Type::Text,
                "unknown acl operation state".into(),
            )
        })?,
        before_descriptor: row.get(4)?,
        planned_delta: row.get(5)?,
        actual_after: row.get(6)?,
        conflict_reason: row.get(7)?,
        prepared_at_ms: row.get(8)?,
        applied_at_ms: row.get(9)?,
        settled_at_ms: row.get(10)?,
    })
}
