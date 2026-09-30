//! Durable per-WorkUnit execution attempts (E06.1).
//!
//! One row per (task, plan revision, work unit): the attempt id derives
//! from that triple, so a replayed dispatch converges on the existing row
//! while divergent content under the same id is refused. Every dispatch
//! writes its row before any spawn, and a row settles exactly once — late
//! or duplicate settles refuse (the resume-once vocabulary), because a
//! unit completes only from its own attempt's durable settle.

use crate::v1::journal::now_ms;
use crate::v1::V1Store;
use rusqlite::{params, OptionalExtension, TransactionBehavior};

/// Lifecycle phase of one attempt row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkUnitAttemptPhase {
    /// The dispatch row exists; the attempt has not started journaling its
    /// kernel transition yet.
    Prepared,
    /// The kernel accepted the unit start; the run may be in flight.
    Dispatched,
    /// Terminal: the unit settled Completed from exactly this attempt.
    SettledCompleted,
    /// Terminal: the unit settled failed from exactly this attempt.
    SettledFailed,
}

impl WorkUnitAttemptPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Dispatched => "dispatched",
            Self::SettledCompleted => "settled-completed",
            Self::SettledFailed => "settled-failed",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "prepared" => Some(Self::Prepared),
            "dispatched" => Some(Self::Dispatched),
            "settled-completed" => Some(Self::SettledCompleted),
            "settled-failed" => Some(Self::SettledFailed),
            _ => None,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::SettledCompleted | Self::SettledFailed)
    }
}

/// The identity one dispatch journals before any spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkUnitAttemptSeed {
    pub attempt_id: String,
    pub task_id: String,
    pub plan_revision: String,
    pub work_unit_id: String,
    /// Content digest of the frozen dispatch material (the RunSnapshot id).
    pub content_sha256: String,
}

/// One durable attempt row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkUnitAttemptRecord {
    pub attempt_id: String,
    pub task_id: String,
    pub plan_revision: String,
    pub work_unit_id: String,
    pub phase: WorkUnitAttemptPhase,
    pub content_sha256: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub settled_at_ms: Option<i64>,
}

/// Typed failures from the attempt repository.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkUnitAttemptError {
    #[error("attempt storage failed: {0}")]
    Sqlite(String),
    #[error("attempt {0} was not found")]
    UnknownAttempt(String),
    #[error("attempt id {attempt_id} already identifies a different dispatch")]
    ContentConflict { attempt_id: String },
    #[error("attempt {0} is already settled; late or duplicate settles refuse")]
    AlreadySettled(String),
    #[error("attempt row for {0} failed integrity validation")]
    CorruptRecord(String),
}

impl From<rusqlite::Error> for WorkUnitAttemptError {
    fn from(error: rusqlite::Error) -> Self {
        WorkUnitAttemptError::Sqlite(error.to_string())
    }
}

const SELECT_COLUMNS: &str = "attempt_id, task_id, plan_revision, work_unit_id, phase,
     content_sha256, created_at_ms, updated_at_ms, settled_at_ms";

impl V1Store {
    /// Journal one dispatch's identity. Idempotent: a byte-identical replay
    /// converges on the existing row (whatever phase it is in — replay never
    /// resurrects); the same attempt id carrying different content, or the
    /// same (task, plan revision, work unit) carrying a different attempt
    /// id, is a conflict. An attempt id never carries two dispatches.
    pub fn prepare_work_unit_attempt(
        &self,
        seed: &WorkUnitAttemptSeed,
    ) -> Result<WorkUnitAttemptRecord, WorkUnitAttemptError> {
        if seed.attempt_id.trim().is_empty()
            || seed.task_id.trim().is_empty()
            || seed.plan_revision.trim().is_empty()
            || seed.work_unit_id.trim().is_empty()
            || seed.content_sha256.trim().is_empty()
        {
            return Err(WorkUnitAttemptError::ContentConflict {
                attempt_id: seed.attempt_id.clone(),
            });
        }
        let mut connection = self.connection();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = load_attempt(&transaction, &seed.attempt_id)? {
            if existing.task_id == seed.task_id
                && existing.plan_revision == seed.plan_revision
                && existing.work_unit_id == seed.work_unit_id
                && existing.content_sha256 == seed.content_sha256
            {
                return Ok(existing);
            }
            return Err(WorkUnitAttemptError::ContentConflict {
                attempt_id: seed.attempt_id.clone(),
            });
        }
        let triple: Option<String> = transaction
            .query_row(
                "SELECT attempt_id FROM work_unit_attempts
                 WHERE task_id = ?1 AND plan_revision = ?2 AND work_unit_id = ?3",
                params![seed.task_id, seed.plan_revision, seed.work_unit_id],
                |row| row.get(0),
            )
            .optional()?;
        if triple.is_some() {
            return Err(WorkUnitAttemptError::ContentConflict {
                attempt_id: seed.attempt_id.clone(),
            });
        }
        let now = now_ms();
        transaction.execute(
            "INSERT INTO work_unit_attempts(
                 attempt_id, task_id, plan_revision, work_unit_id, phase,
                 content_sha256, created_at_ms, updated_at_ms, settled_at_ms)
             VALUES (?1, ?2, ?3, ?4, 'prepared', ?5, ?6, ?6, NULL)",
            params![
                seed.attempt_id,
                seed.task_id,
                seed.plan_revision,
                seed.work_unit_id,
                seed.content_sha256,
                now,
            ],
        )?;
        transaction.commit()?;
        Ok(WorkUnitAttemptRecord {
            attempt_id: seed.attempt_id.clone(),
            task_id: seed.task_id.clone(),
            plan_revision: seed.plan_revision.clone(),
            work_unit_id: seed.work_unit_id.clone(),
            phase: WorkUnitAttemptPhase::Prepared,
            content_sha256: seed.content_sha256.clone(),
            created_at_ms: now,
            updated_at_ms: now,
            settled_at_ms: None,
        })
    }

    /// Prepared -> Dispatched. Idempotent in Dispatched; a settled attempt
    /// refuses (the resume-once vocabulary: recovery may only settle).
    pub fn mark_work_unit_attempt_dispatched(
        &self,
        attempt_id: &str,
    ) -> Result<WorkUnitAttemptRecord, WorkUnitAttemptError> {
        let mut connection = self.connection();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = load_attempt(&transaction, attempt_id)?
            .ok_or_else(|| WorkUnitAttemptError::UnknownAttempt(attempt_id.to_string()))?;
        match existing.phase {
            WorkUnitAttemptPhase::Prepared => {}
            WorkUnitAttemptPhase::Dispatched => return Ok(existing),
            settled => {
                return Err(WorkUnitAttemptError::AlreadySettled(format!(
                    "{attempt_id} ({})",
                    settled.as_str()
                )))
            }
        }
        let now = now_ms();
        transaction.execute(
            "UPDATE work_unit_attempts
             SET phase = 'dispatched', updated_at_ms = ?2
             WHERE attempt_id = ?1 AND phase = 'prepared'",
            params![attempt_id, now],
        )?;
        transaction.commit()?;
        Ok(WorkUnitAttemptRecord {
            phase: WorkUnitAttemptPhase::Dispatched,
            updated_at_ms: now,
            ..existing
        })
    }

    /// Prepared/Dispatched -> settled terminal, exactly once. A second
    /// settle — even with the same outcome — refuses: a unit completes only
    /// from its own attempt's durable settle, exactly once.
    pub fn settle_work_unit_attempt(
        &self,
        attempt_id: &str,
        completed: bool,
    ) -> Result<WorkUnitAttemptRecord, WorkUnitAttemptError> {
        let mut connection = self.connection();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let existing = load_attempt(&transaction, attempt_id)?
            .ok_or_else(|| WorkUnitAttemptError::UnknownAttempt(attempt_id.to_string()))?;
        if existing.phase.is_terminal() {
            return Err(WorkUnitAttemptError::AlreadySettled(format!(
                "{attempt_id} ({})",
                existing.phase.as_str()
            )));
        }
        let phase = if completed {
            WorkUnitAttemptPhase::SettledCompleted
        } else {
            WorkUnitAttemptPhase::SettledFailed
        };
        let now = now_ms();
        let changed = transaction.execute(
            "UPDATE work_unit_attempts
             SET phase = ?2, updated_at_ms = ?3, settled_at_ms = ?3
             WHERE attempt_id = ?1 AND phase IN ('prepared', 'dispatched')",
            params![attempt_id, phase.as_str(), now],
        )?;
        if changed != 1 {
            return Err(WorkUnitAttemptError::CorruptRecord(attempt_id.to_string()));
        }
        transaction.commit()?;
        Ok(WorkUnitAttemptRecord {
            phase,
            updated_at_ms: now,
            settled_at_ms: Some(now),
            ..existing
        })
    }

    /// One attempt row by id.
    pub fn load_work_unit_attempt(
        &self,
        attempt_id: &str,
    ) -> Result<Option<WorkUnitAttemptRecord>, WorkUnitAttemptError> {
        let connection = self.connection();
        load_attempt(&connection, attempt_id)
    }

    /// Every not-yet-settled attempt of one exact plan revision, oldest
    /// first — the enumeration a dispatcher (and, later, restart recovery)
    /// reconciles against.
    pub fn list_in_flight_work_unit_attempts(
        &self,
        task_id: &str,
        plan_revision: &str,
    ) -> Result<Vec<WorkUnitAttemptRecord>, WorkUnitAttemptError> {
        let connection = self.connection();
        let sql = format!(
            "SELECT {SELECT_COLUMNS}
             FROM work_unit_attempts
             WHERE task_id = ?1 AND plan_revision = ?2 AND phase IN ('prepared', 'dispatched')
             ORDER BY created_at_ms, attempt_id"
        );
        let mut statement = connection.prepare(&sql)?;
        let rows = statement
            .query_map(params![task_id, plan_revision], attempt_from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

fn load_attempt(
    connection: &rusqlite::Connection,
    attempt_id: &str,
) -> Result<Option<WorkUnitAttemptRecord>, WorkUnitAttemptError> {
    let sql = format!("SELECT {SELECT_COLUMNS} FROM work_unit_attempts WHERE attempt_id = ?1");
    connection
        .query_row(&sql, params![attempt_id], attempt_from_row)
        .optional()
        .map_err(WorkUnitAttemptError::from)
}

fn attempt_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkUnitAttemptRecord> {
    let phase: String = row.get(4)?;
    Ok(WorkUnitAttemptRecord {
        attempt_id: row.get(0)?,
        task_id: row.get(1)?,
        plan_revision: row.get(2)?,
        work_unit_id: row.get(3)?,
        phase: WorkUnitAttemptPhase::from_str(&phase).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                Box::new(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("unknown work unit attempt phase: {phase}"),
                )),
            )
        })?,
        content_sha256: row.get(5)?,
        created_at_ms: row.get(6)?,
        updated_at_ms: row.get(7)?,
        settled_at_ms: row.get(8)?,
    })
}
