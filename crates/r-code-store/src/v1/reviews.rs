//! Immutable unverified-override audit records (E09-R).
//!
//! One row per (task, override id), carrying the exact failed check ids —
//! never a summary. The table is the single query source for overrides: the
//! review journal event is its derived projection. A row commits in the
//! SAME immediate transaction as the UnverifiedAccepted verdict it audits,
//! so there is no verdict without its row and no row without its verdict;
//! an identical replay converges on the first row while a different actor
//! on the same id conflicts.

use crate::v1::journal::{now_ms, update_task_and_events_if_revision};
use crate::v1::V1Store;
use r_code_kernel::ports::JournalEvent;
use r_code_kernel::task::TaskState;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};

/// The identity and material one override row freezes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnverifiedOverrideSeed {
    pub override_id: String,
    pub task_id: String,
    pub candidate_digest: String,
    pub actor_id: String,
    pub session_id: String,
    pub reason: String,
    /// The exact failed/missing check ids, sorted and deduplicated.
    pub checks: Vec<String>,
}

/// One immutable override row (the canonical camelCase projection).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnverifiedOverrideRecord {
    pub override_id: String,
    pub task_id: String,
    pub candidate_digest: String,
    pub actor_id: String,
    pub session_id: String,
    pub reason: String,
    pub checks: Vec<String>,
    pub created_at_ms: i64,
}

/// Typed failures from the override repository.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UnverifiedOverrideError {
    #[error("override storage failed: {0}")]
    Sqlite(String),
    #[error("required override field {0} is empty")]
    EmptyField(&'static str),
    #[error("override must name at least one failed check")]
    NoChecks,
    #[error("override {override_id} on task {task_id} already belongs to actor {actor}")]
    ActorConflict {
        override_id: String,
        task_id: String,
        actor: String,
    },
    #[error("override row for {task_id}/{override_id} failed integrity validation")]
    CorruptRecord {
        task_id: String,
        override_id: String,
    },
}

impl From<rusqlite::Error> for UnverifiedOverrideError {
    fn from(error: rusqlite::Error) -> Self {
        UnverifiedOverrideError::Sqlite(error.to_string())
    }
}

/// The combined verdict-plus-row commit's failures: the store arm keeps its
/// own variants (`StaleTaskRevision` drives replay) instead of flattening.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OverrideCommitError {
    #[error("{0}")]
    Store(crate::v1::V1StoreError),
    #[error("{0}")]
    Override(UnverifiedOverrideError),
}

impl From<crate::v1::V1StoreError> for OverrideCommitError {
    fn from(error: crate::v1::V1StoreError) -> Self {
        OverrideCommitError::Store(error)
    }
}

impl From<UnverifiedOverrideError> for OverrideCommitError {
    fn from(error: UnverifiedOverrideError) -> Self {
        OverrideCommitError::Override(error)
    }
}

impl From<rusqlite::Error> for OverrideCommitError {
    fn from(error: rusqlite::Error) -> Self {
        OverrideCommitError::Override(UnverifiedOverrideError::Sqlite(error.to_string()))
    }
}

const SELECT_COLUMNS: &str = "override_id, task_id, candidate_digest, actor_id,
     session_id, reason, checks_json, created_at_ms";

impl V1Store {
    /// Record one override row in the SAME immediate transaction as the
    /// UnverifiedAccepted verdict: the task CAS update, its journal events
    /// and the audit row commit together or not at all (E09-R.2).
    pub fn save_task_events_and_unverified_override_if_revision(
        &self,
        task: &TaskState,
        events: Vec<JournalEvent>,
        expected_revision: u64,
        seed: &UnverifiedOverrideSeed,
    ) -> Result<(u64, UnverifiedOverrideRecord), OverrideCommitError> {
        let mut connection = self.connection();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let next_revision =
            update_task_and_events_if_revision(&transaction, task, events, expected_revision)?;
        let record = insert_override(&transaction, seed, now_ms())?;
        transaction.commit()?;
        Ok((next_revision, record))
    }

    /// Record one override row alone (idempotent, replay-converging) — the
    /// recovery path for a row whose verdict committed in an earlier build.
    pub fn record_unverified_override(
        &self,
        seed: &UnverifiedOverrideSeed,
    ) -> Result<UnverifiedOverrideRecord, UnverifiedOverrideError> {
        let mut connection = self.connection();
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let record = insert_override(&transaction, seed, now_ms())?;
        transaction.commit()?;
        Ok(record)
    }

    /// One override row by (task, override id).
    pub fn load_unverified_override(
        &self,
        task_id: &str,
        override_id: &str,
    ) -> Result<Option<UnverifiedOverrideRecord>, UnverifiedOverrideError> {
        let connection = self.connection();
        let sql = format!(
            "SELECT {SELECT_COLUMNS} FROM unverified_overrides
             WHERE task_id = ?1 AND override_id = ?2"
        );
        connection
            .query_row(&sql, params![task_id, override_id], override_from_row)
            .optional()
            .map_err(UnverifiedOverrideError::from)
    }

    /// Every override row, newest first; `task_id` scopes the query when
    /// given. This table is the single query source (the journal is derived).
    pub fn list_unverified_overrides(
        &self,
        task_id: Option<&str>,
    ) -> Result<Vec<UnverifiedOverrideRecord>, UnverifiedOverrideError> {
        let connection = self.connection();
        let sql = match task_id {
            Some(_) => format!(
                "SELECT {SELECT_COLUMNS} FROM unverified_overrides
                 WHERE task_id = ?1 ORDER BY created_at_ms DESC, override_id"
            ),
            None => format!(
                "SELECT {SELECT_COLUMNS} FROM unverified_overrides
                 ORDER BY created_at_ms DESC, override_id"
            ),
        };
        let mut statement = connection.prepare(&sql)?;
        let rows = match task_id {
            Some(task_id) => statement
                .query_map(params![task_id], override_from_row)?
                .collect::<Result<Vec<_>, _>>()?,
            None => statement
                .query_map(params![], override_from_row)?
                .collect::<Result<Vec<_>, _>>()?,
        };
        Ok(rows)
    }
}

/// Insert one override row inside an open transaction: an identical replay
/// converges on the first row; a different actor on the same
/// (task, override id) conflicts.
fn insert_override(
    transaction: &Connection,
    seed: &UnverifiedOverrideSeed,
    created_at_ms: i64,
) -> Result<UnverifiedOverrideRecord, UnverifiedOverrideError> {
    if seed.override_id.trim().is_empty() || seed.task_id.trim().is_empty() {
        return Err(UnverifiedOverrideError::EmptyField("override_id"));
    }
    if seed.actor_id.trim().is_empty() {
        return Err(UnverifiedOverrideError::EmptyField("actor_id"));
    }
    if seed.session_id.trim().is_empty() {
        return Err(UnverifiedOverrideError::EmptyField("session_id"));
    }
    if seed.candidate_digest.trim().is_empty() {
        return Err(UnverifiedOverrideError::EmptyField("candidate_digest"));
    }
    if seed.reason.trim().is_empty() || seed.checks.is_empty() {
        return Err(UnverifiedOverrideError::NoChecks);
    }
    let mut checks = seed.checks.clone();
    checks.sort();
    checks.dedup();
    if let Some(existing) = load_override(transaction, &seed.task_id, &seed.override_id)? {
        if existing.actor_id == seed.actor_id
            && existing.session_id == seed.session_id
            && existing.candidate_digest == seed.candidate_digest
            && existing.reason == seed.reason
            && existing.checks == checks
        {
            return Ok(existing);
        }
        return Err(UnverifiedOverrideError::ActorConflict {
            override_id: seed.override_id.clone(),
            task_id: seed.task_id.clone(),
            actor: existing.actor_id,
        });
    }
    let checks_json = serde_json::to_string(&checks)
        .map_err(|error| UnverifiedOverrideError::Sqlite(error.to_string()))?;
    transaction.execute(
        "INSERT INTO unverified_overrides(override_id, task_id, candidate_digest, actor_id,
         session_id, reason, checks_json, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            seed.override_id,
            seed.task_id,
            seed.candidate_digest,
            seed.actor_id,
            seed.session_id,
            seed.reason,
            checks_json,
            created_at_ms
        ],
    )?;
    Ok(UnverifiedOverrideRecord {
        override_id: seed.override_id.clone(),
        task_id: seed.task_id.clone(),
        candidate_digest: seed.candidate_digest.clone(),
        actor_id: seed.actor_id.clone(),
        session_id: seed.session_id.clone(),
        reason: seed.reason.clone(),
        checks,
        created_at_ms,
    })
}

fn load_override(
    connection: &Connection,
    task_id: &str,
    override_id: &str,
) -> Result<Option<UnverifiedOverrideRecord>, UnverifiedOverrideError> {
    let sql = format!(
        "SELECT {SELECT_COLUMNS} FROM unverified_overrides
         WHERE task_id = ?1 AND override_id = ?2"
    );
    connection
        .query_row(&sql, params![task_id, override_id], override_from_row)
        .optional()
        .map_err(UnverifiedOverrideError::from)
}

fn override_from_row(row: &rusqlite::Row<'_>) -> Result<UnverifiedOverrideRecord, rusqlite::Error> {
    let checks_json: String = row.get(6)?;
    let checks: Vec<String> = serde_json::from_str(&checks_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            6,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("override checks are not canonical JSON: {error}"),
            )),
        )
    })?;
    if checks.is_empty() || checks.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            6,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "override checks must be a sorted non-empty list",
            )),
        ));
    }
    Ok(UnverifiedOverrideRecord {
        override_id: row.get(0)?,
        task_id: row.get(1)?,
        candidate_digest: row.get(2)?,
        actor_id: row.get(3)?,
        session_id: row.get(4)?,
        reason: row.get(5)?,
        checks,
        created_at_ms: row.get(7)?,
    })
}
