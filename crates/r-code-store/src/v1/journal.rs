//! V1 store: SQLite-backed implementation of the kernel `JournalStore` port
//! plus run-lease arbitration.
//!
//! Every mutation commits its aggregate and its events in one transaction;
//! a rollback leaves neither behind. The store is opened only beneath a
//! `RuntimeProfile` harness-v1 root and shares no code path with legacy
//! migrations.

use r_code_harness_protocol::PackageRef;
use r_code_kernel::plans::{PlanApproval, PlanApprovalState};
use r_code_kernel::ports::{CheckpointRecord, JournalEvent, JournalStore, ServiceError};
use r_code_kernel::task::{OperationReceipt, TaskState};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

pub use crate::v1::schema::V1_SCHEMA_VERSION;

/// Errors from the v1 store.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum V1StoreError {
    #[error("run lease for branch {branch_id} is held by run {holder}")]
    LeaseHeld { branch_id: String, holder: String },
    #[error("sqlite failure: {0}")]
    Sqlite(String),
    #[error("serialization failure: {0}")]
    Serialization(String),
    #[error("stale task revision for {task_id}: expected {expected}, actual {actual:?}")]
    StaleTaskRevision {
        task_id: String,
        expected: u64,
        actual: Option<u64>,
    },
}

impl From<V1StoreError> for ServiceError {
    fn from(error: V1StoreError) -> Self {
        ServiceError::Store(error.to_string())
    }
}

impl From<rusqlite::Error> for V1StoreError {
    fn from(error: rusqlite::Error) -> Self {
        V1StoreError::Sqlite(error.to_string())
    }
}

/// A lease acquisition result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseAcquisition {
    Acquired,
    /// Same run re-acquiring its own lease.
    AlreadyHeld,
}

/// SQLite v1 store. Thread-safe via an inner mutex; short critical sections.
pub struct V1Store {
    connection: Mutex<Connection>,
    /// Test-only fault injection: the next aggregate save rolls back.
    fail_next_save: AtomicBool,
    #[allow(dead_code)]
    database_path: PathBuf,
}

impl V1Store {
    /// Open (and migrate if needed) a v1 database at `path`. This path is
    /// always beneath `<data_root>/harness-v1`; legacy databases are opened
    /// by the legacy `Database` type, never here.
    pub fn open(path: &Path) -> Result<Self, V1StoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| V1StoreError::Sqlite(format!("create dir: {e}")))?;
        }
        let mut connection = Connection::open(path)?;
        connection.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON;")?;
        connection.execute_batch(crate::v1::schema::V1_SCHEMA_SQL)?;
        crate::v1::schema::apply_additive_migrations(&mut connection)?;
        connection.execute(
            "INSERT OR IGNORE INTO v2_meta(key, value) VALUES('schema_version', ?1)",
            params![crate::v1::schema::V1_SCHEMA_VERSION],
        )?;
        Ok(Self {
            connection: Mutex::new(connection),
            fail_next_save: AtomicBool::new(false),
            database_path: path.to_path_buf(),
        })
    }

    /// Test-only: make the next `save_task_and_events` fail after writing the
    /// events but before commit, proving the rollback leaves no split.
    pub fn debug_fail_next_save(&self) {
        self.fail_next_save.store(true, Ordering::SeqCst);
    }

    /// Acquire the run lease for a branch. A different live run on the same
    /// branch is refused.
    pub fn try_acquire_run_lease(
        &self,
        branch_id: &str,
        run_id: &str,
        attempt_id: &str,
        generation: u64,
        owner: &str,
    ) -> Result<LeaseAcquisition, V1StoreError> {
        let connection = self.connection.lock().expect("store mutex");
        let existing: Option<(String, String)> = connection
            .query_row(
                "SELECT run_id, attempt_id FROM run_leases WHERE branch_id = ?1",
                params![branch_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        if let Some((holder_run, _)) = existing {
            if holder_run == run_id {
                return Ok(LeaseAcquisition::AlreadyHeld);
            }
            return Err(V1StoreError::LeaseHeld {
                branch_id: branch_id.to_string(),
                holder: holder_run,
            });
        }
        connection.execute(
            "INSERT INTO run_leases(branch_id, run_id, attempt_id, generation, owner, acquired_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                branch_id,
                run_id,
                attempt_id,
                generation,
                owner,
                now_ms()
            ],
        )?;
        Ok(LeaseAcquisition::Acquired)
    }

    /// Release the lease held by `run_id` on `branch_id`.
    pub fn release_run_lease(&self, branch_id: &str, run_id: &str) -> Result<bool, V1StoreError> {
        let connection = self.connection.lock().expect("store mutex");
        let changed = connection.execute(
            "DELETE FROM run_leases WHERE branch_id = ?1 AND run_id = ?2",
            params![branch_id, run_id],
        )?;
        Ok(changed > 0)
    }

    /// The current lease holder for a branch, if any.
    pub fn run_lease_holder(&self, branch_id: &str) -> Option<String> {
        self.connection
            .lock()
            .expect("store mutex")
            .query_row(
                "SELECT run_id FROM run_leases WHERE branch_id = ?1",
                params![branch_id],
                |row| row.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    /// All journal events for one task, in order.
    pub fn task_events(&self, task_id: &str) -> Vec<JournalEvent> {
        self.connection
            .lock()
            .expect("store mutex")
            .prepare(
                "SELECT seq, task_id, kind, payload FROM events WHERE task_id = ?1 ORDER BY seq",
            )
            .and_then(|mut statement| {
                let mut rows = statement.query(params![task_id])?;
                let mut events = Vec::new();
                while let Some(row) = rows.next()? {
                    let payload_text: String = row.get(3)?;
                    let payload = serde_json::from_str(&payload_text).unwrap_or_default();
                    events.push(JournalEvent {
                        seq: row.get(0)?,
                        task_id: row.get(1)?,
                        kind: row.get(2)?,
                        payload,
                    });
                }
                Ok(events)
            })
            .unwrap_or_default()
    }

    /// Every persisted task with its aggregate, most recently updated first.
    pub fn list_tasks(&self) -> Vec<(TaskState, i64)> {
        self.connection
            .lock()
            .expect("store mutex")
            .prepare(
                "SELECT state_json, updated_at_ms FROM tasks
                 ORDER BY updated_at_ms DESC, task_id",
            )
            .and_then(|mut statement| {
                let mut rows = statement.query([])?;
                let mut tasks = Vec::new();
                while let Some(row) = rows.next()? {
                    let state_text: String = row.get(0)?;
                    let updated: i64 = row.get(1)?;
                    if let Ok(state) = serde_json::from_str::<TaskState>(&state_text) {
                        tasks.push((state, updated));
                    }
                }
                Ok(tasks)
            })
            .unwrap_or_default()
    }

    /// Load an aggregate together with the SQLite compare-and-swap revision.
    pub fn load_task_with_revision(
        &self,
        task_id: &str,
    ) -> Result<Option<(TaskState, u64)>, V1StoreError> {
        let row: Option<(String, i64)> = self
            .connection
            .lock()
            .expect("store mutex")
            .query_row(
                "SELECT state_json, revision FROM tasks WHERE task_id = ?1",
                params![task_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((state_json, revision)) = row else {
            return Ok(None);
        };
        let state = serde_json::from_str(&state_json)
            .map_err(|error| V1StoreError::Serialization(error.to_string()))?;
        let revision = u64::try_from(revision)
            .map_err(|error| V1StoreError::Serialization(error.to_string()))?;
        Ok(Some((state, revision)))
    }

    /// Commit one aggregate and its events only when the persisted revision
    /// still matches `expected_revision`. A stale result rolls back the whole
    /// transaction, including every event insert.
    pub fn save_task_and_events_if_revision(
        &self,
        task: &TaskState,
        events: Vec<JournalEvent>,
        expected_revision: u64,
    ) -> Result<u64, V1StoreError> {
        self.save_task_events_and_selection_pin_if_revision(task, events, expected_revision, None)
    }

    /// Create one task aggregate, its initial events and its durable Harness
    /// selection pin in the same transaction. Any failure rolls all three
    /// resources back, so a validated create never leaves a half-task.
    pub fn create_task_with_selection_pin(
        &self,
        task: &TaskState,
        events: Vec<JournalEvent>,
        package: &PackageRef,
    ) -> Result<u64, V1StoreError> {
        self.create_task_with_optional_selection_pin(task, events, Some(package))
    }

    /// Create a legacy/default-routed task without claiming a Harness
    /// selection. A later best-effort default pin or explicit selection uses
    /// the revision-fenced transaction below.
    pub fn create_task_without_selection_pin(
        &self,
        task: &TaskState,
        events: Vec<JournalEvent>,
    ) -> Result<u64, V1StoreError> {
        self.create_task_with_optional_selection_pin(task, events, None)
    }

    fn create_task_with_optional_selection_pin(
        &self,
        task: &TaskState,
        events: Vec<JournalEvent>,
        package: Option<&PackageRef>,
    ) -> Result<u64, V1StoreError> {
        let mut connection = self.connection.lock().expect("store mutex");
        let transaction = connection.transaction()?;
        if let Some(package) = package {
            ensure_selection_package(&transaction, package)?;
        }
        let state_json = serde_json::to_string(task)
            .map_err(|error| V1StoreError::Serialization(error.to_string()))?;
        transaction.execute(
            "INSERT INTO tasks(task_id, branch_id, revision, state_json, updated_at_ms)
             VALUES (?1, ?2, 1, ?3, ?4)",
            params![task.contract.task_id, "", state_json, now_ms()],
        )?;
        insert_events(&transaction, events)?;
        if let Some(package) = package {
            upsert_selection_pin(&transaction, &task.contract.task_id, package)?;
        }
        if self.fail_next_save.swap(false, Ordering::SeqCst) {
            return Err(V1StoreError::Sqlite(
                "injected fault before commit".to_string(),
            ));
        }
        transaction.commit()?;
        Ok(1)
    }

    /// CAS-update one task aggregate and its events, optionally replacing
    /// the durable task-level Harness selection pin in the same transaction.
    pub fn save_task_events_and_selection_pin_if_revision(
        &self,
        task: &TaskState,
        events: Vec<JournalEvent>,
        expected_revision: u64,
        selection_package: Option<&PackageRef>,
    ) -> Result<u64, V1StoreError> {
        self.save_task_events_selection_and_plan_invalidation_if_revision(
            task,
            events,
            expected_revision,
            selection_package,
            false,
        )
    }

    /// CAS-update a task while superseding its active plan approval in the
    /// same immediate transaction. Callers first apply the kernel's
    /// `invalidate_plan` transition to `task`; this method makes the approval,
    /// task aggregate, events and optional Harness pin indivisible.
    pub fn save_task_and_invalidate_plan_if_revision(
        &self,
        task: &TaskState,
        events: Vec<JournalEvent>,
        expected_revision: u64,
        selection_package: Option<&PackageRef>,
    ) -> Result<u64, V1StoreError> {
        self.save_task_events_selection_and_plan_invalidation_if_revision(
            task,
            events,
            expected_revision,
            selection_package,
            true,
        )
    }

    fn save_task_events_selection_and_plan_invalidation_if_revision(
        &self,
        task: &TaskState,
        events: Vec<JournalEvent>,
        expected_revision: u64,
        selection_package: Option<&PackageRef>,
        invalidate_plan: bool,
    ) -> Result<u64, V1StoreError> {
        let mut connection = self.connection.lock().expect("store mutex");
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(package) = selection_package {
            ensure_selection_package(&transaction, package)?;
        }
        if invalidate_plan {
            let active: Option<(String, String)> = transaction
                .query_row(
                    "SELECT approval_id, approval_json FROM plan_approvals
                     WHERE task_id = ?1 AND state = 'active'",
                    params![task.contract.task_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if let Some((approval_id, approval_json)) = active {
                let mut approval: PlanApproval = serde_json::from_str(&approval_json)
                    .map_err(|error| V1StoreError::Serialization(error.to_string()))?;
                approval.state = PlanApprovalState::Superseded;
                let canonical = approval
                    .canonical_json()
                    .map_err(|error| V1StoreError::Serialization(error.to_string()))?;
                transaction.execute(
                    "UPDATE plan_approvals
                     SET state = 'superseded', superseded_at_ms = ?1, approval_json = ?2
                     WHERE approval_id = ?3 AND state = 'active'",
                    params![now_ms(), canonical, approval_id],
                )?;
            }
        }
        let next_revision =
            update_task_and_events_if_revision(&transaction, task, events, expected_revision)?;
        if let Some(package) = selection_package {
            upsert_selection_pin(&transaction, &task.contract.task_id, package)?;
        }
        if self.fail_next_save.swap(false, Ordering::SeqCst) {
            return Err(V1StoreError::Sqlite(
                "injected fault before commit".to_string(),
            ));
        }
        transaction.commit()?;
        Ok(next_revision)
    }

    pub(crate) fn take_debug_fail_next_save(&self) -> bool {
        self.fail_next_save.swap(false, Ordering::SeqCst)
    }

    /// Access the connection for sibling v1 modules (branches, queues).
    pub(crate) fn connection(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.connection.lock().expect("store mutex")
    }

    fn save_task_and_events_inner(
        &self,
        task: &TaskState,
        events: Vec<JournalEvent>,
    ) -> Result<(), ServiceError> {
        let mut connection = self.connection.lock().expect("store mutex");
        let state_json = serde_json::to_string(task)
            .map_err(|e| ServiceError::Store(format!("serialize task: {e}")))?;
        let task_id = task.contract.task_id.clone();
        let branch_id = match &task.execution {
            r_code_kernel::task::TaskExecution::Running { .. } => String::new(),
            _ => String::new(),
        };
        let _ = branch_id;
        let inject_failure = self.fail_next_save.swap(false, Ordering::SeqCst);
        let transaction_result: Result<(), ServiceError> = (|| {
            let tx = connection
                .transaction()
                .map_err(|e| ServiceError::Store(e.to_string()))?;
            tx.execute(
                "INSERT INTO tasks(task_id, branch_id, revision, state_json, updated_at_ms)
                 VALUES (?1, ?2, 1, ?3, ?4)
                 ON CONFLICT(task_id) DO UPDATE SET
                    revision = revision + 1,
                    state_json = excluded.state_json,
                    updated_at_ms = excluded.updated_at_ms",
                params![task_id, "", state_json, now_ms()],
            )
            .map_err(|e| ServiceError::Store(e.to_string()))?;
            for event in events {
                tx.execute(
                    "INSERT INTO events(task_id, kind, payload) VALUES (?1, ?2, ?3)",
                    params![event.task_id, event.kind, event.payload.to_string()],
                )
                .map_err(|e| ServiceError::Store(e.to_string()))?;
            }
            if inject_failure {
                // Simulate a fault after the writes, before commit: the
                // transaction rolls back entirely.
                return Err(ServiceError::Store("injected fault before commit".into()));
            }
            tx.commit().map_err(|e| ServiceError::Store(e.to_string()))
        })();
        transaction_result
    }
}

#[async_trait::async_trait]
impl JournalStore for V1Store {
    async fn load_task(&self, task_id: &str) -> Option<TaskState> {
        self.connection
            .lock()
            .expect("store mutex")
            .query_row(
                "SELECT state_json FROM tasks WHERE task_id = ?1",
                params![task_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str(&json).ok())
    }

    async fn save_task_and_events(
        &self,
        task: &TaskState,
        events: Vec<JournalEvent>,
    ) -> Result<(), ServiceError> {
        self.save_task_and_events_inner(task, events)
    }

    async fn read_events(&self, after_seq: u64, limit: u32) -> Vec<JournalEvent> {
        self.connection
            .lock()
            .expect("store mutex")
            .prepare(
                "SELECT seq, task_id, kind, payload FROM events WHERE seq > ?1 ORDER BY seq LIMIT ?2",
            )
            .and_then(|mut statement| {
                let mut rows = statement.query(params![after_seq, limit])?;
                let mut events = Vec::new();
                while let Some(row) = rows.next()? {
                    let payload_text: String = row.get(3)?;
                    let payload = serde_json::from_str(&payload_text).unwrap_or_default();
                    events.push(JournalEvent {
                        seq: row.get(0)?,
                        task_id: row.get(1)?,
                        kind: row.get(2)?,
                        payload,
                    });
                }
                Ok(events)
            })
            .unwrap_or_default()
    }

    async fn max_event_seq(&self) -> u64 {
        self.connection
            .lock()
            .expect("store mutex")
            .query_row("SELECT COALESCE(MAX(seq), 0) FROM events", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap_or(0) as u64
    }

    async fn save_receipt(&self, receipt: OperationReceipt) -> Result<(), ServiceError> {
        let state_json = serde_json::to_string(&receipt.outcome)
            .map_err(|e| ServiceError::Store(e.to_string()))?;
        self.connection
            .lock()
            .expect("store mutex")
            .execute(
                "INSERT INTO operation_receipts(
                    attempt_id, operation_key, method, input_hash, state_json,
                    generation_recorded, generation_completed)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(attempt_id, operation_key) DO UPDATE SET
                    state_json = excluded.state_json,
                    generation_completed = excluded.generation_completed",
                params![
                    receipt.attempt_id,
                    receipt.operation_key.0,
                    receipt.method,
                    receipt.input_hash,
                    state_json,
                    0i64,
                    0i64,
                ],
            )
            .map_err(|e| ServiceError::Store(e.to_string()))?;
        Ok(())
    }

    async fn load_receipt(
        &self,
        attempt_id: &str,
        key: &r_code_harness_protocol::OperationKey,
    ) -> Option<OperationReceipt> {
        self.connection
            .lock()
            .expect("store mutex")
            .query_row(
                "SELECT method, input_hash, state_json FROM operation_receipts
                 WHERE attempt_id = ?1 AND operation_key = ?2",
                params![attempt_id, key.0],
                |row| {
                    let method: String = row.get(0)?;
                    let input_hash: String = row.get(1)?;
                    let state_json: String = row.get(2)?;
                    Ok((method, input_hash, state_json))
                },
            )
            .optional()
            .ok()
            .flatten()
            .and_then(|(method, input_hash, state_json)| {
                let outcome = serde_json::from_str(&state_json).ok()?;
                Some(OperationReceipt {
                    attempt_id: attempt_id.to_string(),
                    operation_key: r_code_harness_protocol::OperationKey(key.0.clone()),
                    method,
                    input_hash,
                    outcome,
                })
            })
    }

    async fn save_checkpoint(
        &self,
        attempt_id: &str,
        revision: u64,
        state: Vec<u8>,
        consumed_input_seq: u64,
    ) -> Result<r_code_harness_protocol::ArtifactRef, ServiceError> {
        let artifact = r_code_harness_protocol::ArtifactRef {
            schema: r_code_harness_protocol::ArtifactRef::SCHEMA,
            blob_id: format!("blob:ckpt:{attempt_id}:{revision}"),
            bytes: state.len() as u64,
            sha256: format!("sha:{:016x}", consumed_input_seq),
            media_type: Some("application/octet-stream".into()),
        };
        self.connection
            .lock()
            .expect("store mutex")
            .execute(
                "INSERT INTO checkpoints(
                    attempt_id, revision, consumed_input_seq, state, blob_id, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    attempt_id,
                    revision,
                    consumed_input_seq,
                    state,
                    artifact.blob_id,
                    now_ms()
                ],
            )
            .map_err(|e| ServiceError::Store(e.to_string()))?;
        Ok(artifact)
    }

    async fn load_latest_checkpoint(&self, attempt_id: &str) -> Option<CheckpointRecord> {
        self.connection
            .lock()
            .expect("store mutex")
            .query_row(
                "SELECT revision, consumed_input_seq, state, blob_id FROM checkpoints
                 WHERE attempt_id = ?1 ORDER BY revision DESC LIMIT 1",
                params![attempt_id],
                |row| {
                    Ok(CheckpointRecord {
                        attempt_id: attempt_id.to_string(),
                        revision: row.get(0)?,
                        consumed_input_seq: row.get(1)?,
                        state: row.get(2)?,
                        artifact: r_code_harness_protocol::ArtifactRef {
                            schema: r_code_harness_protocol::ArtifactRef::SCHEMA,
                            blob_id: row.get(3)?,
                            bytes: 0,
                            sha256: String::new(),
                            media_type: None,
                        },
                    })
                },
            )
            .optional()
            .ok()
            .flatten()
    }
}

pub(crate) fn update_task_and_events_if_revision(
    transaction: &Transaction<'_>,
    task: &TaskState,
    events: Vec<JournalEvent>,
    expected_revision: u64,
) -> Result<u64, V1StoreError> {
    let task_id = task.contract.task_id.clone();
    let actual: Option<i64> = transaction
        .query_row(
            "SELECT revision FROM tasks WHERE task_id = ?1",
            params![task_id],
            |row| row.get(0),
        )
        .optional()?;
    let actual = actual.and_then(|value| u64::try_from(value).ok());
    if actual != Some(expected_revision) {
        return Err(V1StoreError::StaleTaskRevision {
            task_id,
            expected: expected_revision,
            actual,
        });
    }
    let next_revision = expected_revision
        .checked_add(1)
        .ok_or_else(|| V1StoreError::Serialization("task revision overflow".to_string()))?;
    let state_json = serde_json::to_string(task)
        .map_err(|error| V1StoreError::Serialization(error.to_string()))?;
    let changed = transaction.execute(
        "UPDATE tasks SET revision = ?1, state_json = ?2, updated_at_ms = ?3
         WHERE task_id = ?4 AND revision = ?5",
        params![
            next_revision,
            state_json,
            now_ms(),
            task.contract.task_id,
            expected_revision,
        ],
    )?;
    if changed != 1 {
        return Err(V1StoreError::StaleTaskRevision {
            task_id: task.contract.task_id.clone(),
            expected: expected_revision,
            actual,
        });
    }
    insert_events(transaction, events)?;
    Ok(next_revision)
}

pub(crate) fn insert_events(
    transaction: &Transaction<'_>,
    events: Vec<JournalEvent>,
) -> Result<(), V1StoreError> {
    for event in events {
        transaction.execute(
            "INSERT INTO events(task_id, kind, payload) VALUES (?1, ?2, ?3)",
            params![event.task_id, event.kind, event.payload.to_string()],
        )?;
    }
    Ok(())
}

fn upsert_selection_pin(
    transaction: &rusqlite::Transaction<'_>,
    task_id: &str,
    package: &PackageRef,
) -> Result<(), V1StoreError> {
    transaction.execute(
        "INSERT OR REPLACE INTO plugin_pins(
            attempt_id, task_id, id, version, content_digest, created_at_ms)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            format!("selection-{task_id}"),
            task_id,
            package.id.0,
            package.version.to_string(),
            package.content_digest,
            now_ms(),
        ],
    )?;
    Ok(())
}

fn ensure_selection_package(
    transaction: &rusqlite::Transaction<'_>,
    package: &PackageRef,
) -> Result<(), V1StoreError> {
    let enabled: Option<i64> = transaction
        .query_row(
            "SELECT enabled FROM plugin_catalog
             WHERE id = ?1 AND version = ?2 AND content_digest = ?3",
            params![
                package.id.0,
                package.version.to_string(),
                package.content_digest,
            ],
            |row| row.get(0),
        )
        .optional()?;
    if enabled != Some(1) {
        return Err(V1StoreError::Sqlite(format!(
            "selection package {} {} is unavailable",
            package.id.0, package.version
        )));
    }
    Ok(())
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
