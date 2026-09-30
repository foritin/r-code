//! Insert-only persistence for immutable, content-addressed run snapshots.

use crate::v1::plans::{validate_active_approval, PlanStoreError};
use crate::v1::{V1Store, V1StoreError};
use r_code_kernel::plans::PlanRevision;
use r_code_kernel::task::{RunSnapshot, RunSnapshotPhase};
use rusqlite::{params, OptionalExtension, TransactionBehavior};

impl V1Store {
    /// Persist a snapshot exactly once. Re-inserting identical content is an
    /// idempotent success; reusing an id for different content is rejected.
    pub fn save_run_snapshot(&self, snapshot: &RunSnapshot) -> Result<(), V1StoreError> {
        snapshot
            .validate_identity()
            .map_err(|error| V1StoreError::Serialization(error.to_string()))?;
        let snapshot_json = serde_json::to_string(snapshot)
            .map_err(|error| V1StoreError::Serialization(error.to_string()))?;
        let mut connection = self.connection();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(approval) = snapshot.phase().approval() {
            validate_active_approval(&transaction, &snapshot.material().task_id, approval)
                .map_err(|error| {
                    V1StoreError::Serialization(format!(
                        "plan approval rejected: {}",
                        approval_error_code(&error)
                    ))
                })?;
            let plan_json: String = transaction
                .query_row(
                    "SELECT material_json FROM plan_revisions
                     WHERE task_id = ?1 AND plan_revision = ?2",
                    params![snapshot.material().task_id, approval.plan_revision.0],
                    |row| row.get(0),
                )
                .map_err(|_| {
                    V1StoreError::Serialization(
                        "approved plan revision could not be loaded".to_string(),
                    )
                })?;
            let plan: PlanRevision = serde_json::from_str(&plan_json)
                .map_err(|_| V1StoreError::Serialization("approved plan is corrupt".into()))?;
            plan.validate_identity()
                .map_err(|_| V1StoreError::Serialization("approved plan is corrupt".into()))?;
            let work_unit_id = snapshot
                .material()
                .work_unit_id
                .as_deref()
                .filter(|id| !id.trim().is_empty())
                .ok_or_else(|| {
                    V1StoreError::Serialization(
                        "execution snapshot has no WorkUnit identity".to_string(),
                    )
                })?;
            if plan.material().task_id != snapshot.material().task_id
                || plan.reference() != &approval.plan_revision
                || !plan
                    .material()
                    .work_units
                    .iter()
                    .any(|unit| unit.id == work_unit_id)
            {
                return Err(V1StoreError::Serialization(
                    "execution WorkUnit is not part of the approved plan".to_string(),
                ));
            }
        }
        let existing: Option<String> = transaction
            .query_row(
                "SELECT snapshot_json FROM run_snapshots WHERE snapshot_id = ?1",
                params![snapshot.id().as_str()],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(existing_json) = existing {
            let existing_snapshot: RunSnapshot = serde_json::from_str(&existing_json)
                .map_err(|error| V1StoreError::Serialization(error.to_string()))?;
            existing_snapshot
                .validate_identity()
                .map_err(|error| V1StoreError::Serialization(error.to_string()))?;
            if existing_snapshot != *snapshot {
                return Err(V1StoreError::Serialization(format!(
                    "run snapshot {} already exists with different content",
                    snapshot.id()
                )));
            }
            transaction.commit()?;
            return Ok(());
        }

        transaction.execute(
            "INSERT INTO run_snapshots(
                snapshot_id, task_id, phase, content_sha256, snapshot_json, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                snapshot.id().as_str(),
                snapshot.material().task_id,
                phase_name(snapshot.phase()),
                snapshot.id().as_str().trim_start_matches("sha256:"),
                snapshot_json,
                now_ms(),
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Load and validate a stored snapshot. Corrupt or identity-mismatched
    /// rows fail closed instead of being treated as missing configuration.
    pub fn load_run_snapshot(
        &self,
        snapshot_id: impl AsRef<str>,
    ) -> Result<Option<RunSnapshot>, V1StoreError> {
        let snapshot_id = snapshot_id.as_ref();
        let snapshot_json: Option<String> = self
            .connection()
            .query_row(
                "SELECT snapshot_json FROM run_snapshots WHERE snapshot_id = ?1",
                params![snapshot_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(snapshot_json) = snapshot_json else {
            return Ok(None);
        };
        let snapshot: RunSnapshot = serde_json::from_str(&snapshot_json)
            .map_err(|error| V1StoreError::Serialization(error.to_string()))?;
        snapshot
            .validate_identity()
            .map_err(|error| V1StoreError::Serialization(error.to_string()))?;
        if snapshot.id().as_str() != snapshot_id {
            return Err(V1StoreError::Serialization(format!(
                "run snapshot row key {snapshot_id} does not match content id {}",
                snapshot.id()
            )));
        }
        Ok(Some(snapshot))
    }
}

fn approval_error_code(error: &PlanStoreError) -> &'static str {
    match error {
        PlanStoreError::ApprovalNotFound(_) => "not-found",
        PlanStoreError::ApprovalTaskMismatch => "task-mismatch",
        PlanStoreError::ApprovalRevisionMismatch => "revision-mismatch",
        PlanStoreError::ApprovalSuperseded => "superseded",
        PlanStoreError::ApprovalNotCurrent => "not-current",
        PlanStoreError::CorruptRecord => "corrupt-record",
        PlanStoreError::Sqlite(_) => "storage-failure",
        _ => "invalid",
    }
}

fn phase_name(phase: &RunSnapshotPhase) -> &'static str {
    match phase {
        RunSnapshotPhase::Planning => "planning",
        RunSnapshotPhase::Execution { .. } => "execution",
        RunSnapshotPhase::Repair { .. } => "repair",
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
