//! Feature-scoped Plan review and crash-safe rejection.
//!
//! This module is intentionally separate from Git review. Enhanced review only projects trusted
//! write events attributed to the current Plan revision; accepting is a ledger operation, while
//! rejecting removes exactly one feature's contribution with an inverse three-way merge.

use std::collections::HashMap;
use std::io::{self, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use chrono::{DateTime, Utc};
use r_code_core::error::ProductError;
use r_code_core::plan::{
    PlanChangeEvent, PlanChangeEventState, PlanExecutionStatus, PlanItemState, PlanReviewDecision,
    PlanReviewDecisionKind, PlanReviewScope,
};
use r_code_core::security::{PathGuard, WorkspaceFileAccess};
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use uuid::Uuid;

use crate::{BlobStore, Database};

/// A stable reference carried by trusted tool execution context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanFeatureRef {
    pub plan_id: String,
    pub plan_revision: u64,
    pub item_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanRevisionRef {
    pub plan_id: String,
    pub plan_revision: u64,
}

pub const PLAN_REVIEW_FEATURE_NOT_TERMINAL: &str = "plan_review_feature_not_terminal";
pub const PLAN_REVIEW_SCOPE_CONFLICT: &str = "plan_review_scope_conflict";

/// Metadata supplied after a trusted write completes while [`PlanWriteGuard`] is still held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishPlanWriteInput {
    pub tool_call_id: String,
}

/// Owns the path lease from the before-read until the event has been durably recorded.
pub struct PlanWriteGuard {
    task_id: String,
    run_id: String,
    feature: Option<PlanFeatureRef>,
    resolved: ResolvedWorkspacePath,
    before: Snapshot,
    _lease: PathLease,
}

/// Owns the same path lease for a trusted write that has no active Plan feature.
///
/// Ordinary writes deliberately carry no snapshots or ownership metadata, but still coordinate
/// with rejection so a completed feature cannot roll back over a later direct edit.
pub struct CoordinatedWriteGuard {
    resolved: ResolvedWorkspacePath,
    _lease: PathLease,
}

impl CoordinatedWriteGuard {
    pub fn path(&self) -> &Path {
        &self.resolved.absolute
    }
}

impl PlanWriteGuard {
    pub fn path(&self) -> &Path {
        &self.resolved.absolute
    }

    pub fn relative_path(&self) -> &str {
        &self.resolved.relative
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RecordPlanWriteOutcome {
    Captured { event: PlanChangeEvent },
    Duplicate { event: PlanChangeEvent },
    Unassigned { path: String },
    Unchanged { path: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnhancedReviewEventView {
    pub sequence: i64,
    pub event_id: String,
    pub tool_call_id: String,
    pub before_exists: bool,
    pub after_exists: bool,
    pub before_blob_hash: Option<String>,
    pub after_blob_hash: Option<String>,
    /// Unified patch for UTF-8 text. Binary snapshots deliberately omit inline content.
    pub patch: Option<String>,
    pub binary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnhancedReviewFileView {
    pub path: String,
    pub decision: Option<PlanReviewDecisionKind>,
    pub first_sequence: i64,
    pub last_sequence: i64,
    pub events: Vec<EnhancedReviewEventView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnhancedReviewGroupView {
    pub item_id: String,
    pub ordinal: u32,
    pub title: String,
    pub description: String,
    pub state: PlanItemState,
    pub decision: Option<PlanReviewDecisionKind>,
    pub files: Vec<EnhancedReviewFileView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnhancedReviewView {
    pub task_id: String,
    pub plan_id: String,
    pub plan_revision: u64,
    pub groups: Vec<EnhancedReviewGroupView>,
}

/// Optimistic target identity prevents a stale UI from deciding a newer Plan revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnhancedReviewTarget {
    pub task_id: String,
    pub plan_id: String,
    pub plan_revision: u64,
    pub item_id: String,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanRejectResult {
    pub operation_id: String,
    pub decision: PlanReviewDecision,
    pub changed_paths: Vec<String>,
    pub idempotent: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanRejectRecoveryReport {
    pub recovered_operation_ids: Vec<String>,
    pub conflicted_operation_ids: Vec<String>,
    pub retryable_operation_ids: Vec<String>,
}

/// Process-wide per-path locking. Locks are always acquired in normalized lexical order.
#[derive(Clone, Default)]
pub struct PathCoordinator {
    locks: Arc<Mutex<HashMap<String, Weak<AsyncMutex<()>>>>>,
}

pub struct PathLease {
    paths: Vec<PathBuf>,
    _guards: Vec<OwnedMutexGuard<()>>,
}

impl PathLease {
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }
}

impl PathCoordinator {
    pub fn shared() -> Self {
        static SHARED: OnceLock<PathCoordinator> = OnceLock::new();
        SHARED.get_or_init(Self::default).clone()
    }

    pub async fn acquire<I>(&self, paths: I) -> Result<PathLease, ProductError>
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let mut paths: Vec<PathBuf> = paths.into_iter().collect();
        paths.sort_by_key(|path| path_sort_key(path));
        paths.dedup_by(|left, right| path_sort_key(left) == path_sort_key(right));

        let mutexes = {
            let mut registry = self
                .locks
                .lock()
                .map_err(|_| ProductError::Other("path lock registry is poisoned".into()))?;
            registry.retain(|_, lock| lock.strong_count() > 0);
            paths
                .iter()
                .map(|path| {
                    let key = path_sort_key(path);
                    if let Some(lock) = registry.get(&key).and_then(Weak::upgrade) {
                        lock
                    } else {
                        let lock = Arc::new(AsyncMutex::new(()));
                        registry.insert(key, Arc::downgrade(&lock));
                        lock
                    }
                })
                .collect::<Vec<_>>()
        };

        let mut guards = Vec::with_capacity(mutexes.len());
        for lock in mutexes {
            guards.push(lock.lock_owned().await);
        }
        Ok(PathLease {
            paths,
            _guards: guards,
        })
    }
}

/// File operations are injectable so atomic failure and recovery can be verified deterministically.
pub trait PlanReviewFileSystem: Send + Sync {
    fn read_snapshot(&self, guard: &PathGuard, path: &Path) -> io::Result<Option<Vec<u8>>>;
    fn write_snapshot(
        &self,
        guard: &PathGuard,
        path: &Path,
        content: Option<&[u8]>,
    ) -> io::Result<()>;
}

#[derive(Debug, Default)]
pub struct OsPlanReviewFileSystem;

impl PlanReviewFileSystem for OsPlanReviewFileSystem {
    fn read_snapshot(&self, guard: &PathGuard, path: &Path) -> io::Result<Option<Vec<u8>>> {
        let (_, mut file) = match guard.open_existing_file(path, WorkspaceFileAccess::Read) {
            Ok(file) => file,
            Err(ProductError::PathNotFound(_)) => return Ok(None),
            Err(error) => return Err(product_error_to_io(error)),
        };
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        Ok(Some(bytes))
    }

    fn write_snapshot(
        &self,
        guard: &PathGuard,
        path: &Path,
        content: Option<&[u8]>,
    ) -> io::Result<()> {
        match content {
            None => guard
                .remove_file_if_exists(path)
                .map(|_| ())
                .map_err(product_error_to_io),
            Some(content) => guard
                .atomic_write_file(path, content)
                .map(|_| ())
                .map_err(product_error_to_io),
        }
    }
}

pub struct PlanReviewStore<'a> {
    db: &'a Database,
    blobs_dir: PathBuf,
    coordinator: PathCoordinator,
    file_system: Arc<dyn PlanReviewFileSystem>,
}

impl<'a> PlanReviewStore<'a> {
    pub fn new(db: &'a Database, blobs_dir: PathBuf) -> Self {
        Self::with_dependencies(
            db,
            blobs_dir,
            PathCoordinator::shared(),
            Arc::new(OsPlanReviewFileSystem),
        )
    }

    pub fn with_dependencies(
        db: &'a Database,
        blobs_dir: PathBuf,
        coordinator: PathCoordinator,
        file_system: Arc<dyn PlanReviewFileSystem>,
    ) -> Self {
        Self {
            db,
            blobs_dir,
            coordinator,
            file_system,
        }
    }

    pub fn coordinator(&self) -> &PathCoordinator {
        &self.coordinator
    }

    /// Acquires a verified path lease without reading snapshots or creating an enhanced event.
    pub async fn begin_coordinated_write(
        &self,
        workspace_root: &Path,
        task_id: &str,
        path: &Path,
    ) -> Result<CoordinatedWriteGuard, ProductError> {
        let resolved = self.resolve_task_path(task_id, workspace_root, path)?;
        let lease = self
            .coordinator
            .acquire([resolved.absolute.clone()])
            .await?;
        let verified = self.resolve_task_path(task_id, workspace_root, path)?;
        if verified.absolute != resolved.absolute {
            return Err(ProductError::PathEscape(format!(
                "path changed while acquiring write lease: {path:?}"
            )));
        }
        Ok(CoordinatedWriteGuard {
            resolved,
            _lease: lease,
        })
    }

    /// Starts the trusted write critical section before the tool reads or mutates the target.
    pub async fn begin_feature_write(
        &self,
        workspace_root: &Path,
        task_id: &str,
        run_id: &str,
        path: &Path,
    ) -> Result<PlanWriteGuard, ProductError> {
        let feature = self.active_feature_for_run(task_id, run_id)?;
        let resolved = self.resolve_task_path(task_id, workspace_root, path)?;
        let lease = self
            .coordinator
            .acquire([resolved.absolute.clone()])
            .await?;
        // Resolve again after acquiring the lease. A symlink swap between the first resolution and
        // acquisition must not redirect the tool to a different path key.
        let verified = self.resolve_task_path(task_id, workspace_root, path)?;
        if verified.absolute != resolved.absolute {
            return Err(ProductError::PathEscape(format!(
                "path changed while acquiring write lease: {path:?}"
            )));
        }
        let before = Snapshot::from_option(
            self.file_system
                .read_snapshot(&resolved.guard, &resolved.absolute)
                .map_err(io_error)?,
        );
        Ok(PlanWriteGuard {
            task_id: task_id.to_string(),
            run_id: run_id.to_string(),
            feature,
            resolved,
            before,
            _lease: lease,
        })
    }

    /// Finishes the trusted write while consuming (and therefore still holding) its path lease.
    pub fn finish_feature_write(
        &self,
        guard: PlanWriteGuard,
        input: FinishPlanWriteInput,
    ) -> Result<RecordPlanWriteOutcome, ProductError> {
        let after = Snapshot::from_option(
            self.file_system
                .read_snapshot(&guard.resolved.guard, &guard.resolved.absolute)
                .map_err(io_error)?,
        );
        self.persist_write_observation(
            &guard.task_id,
            &guard.resolved,
            guard.before,
            after,
            CapturedOwnership {
                run_id: guard.run_id,
                tool_call_id: input.tool_call_id,
                feature: guard.feature,
            },
        )
    }

    /// Resolves ownership from durable host state. Model-provided IDs are never accepted.
    pub fn active_feature_for_run(
        &self,
        task_id: &str,
        run_id: &str,
    ) -> Result<Option<PlanFeatureRef>, ProductError> {
        let conn = self.db.conn()?;
        let run_valid: bool = conn
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM agent_runs
                   WHERE id = ?1 AND task_id = ?2 AND ended_at IS NULL
                 )",
                params![run_id, task_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if !run_valid {
            return Err(ProductError::StateMachineError(format!(
                "run {run_id} does not belong to task {task_id}"
            )));
        }
        let mut statement = conn
            .prepare(
                "SELECT plan.id, plan.approved_revision, item.id
                 FROM plans plan
                 JOIN plan_items item
                   ON item.plan_id = plan.id AND item.revision = plan.approved_revision
                 WHERE plan.task_id = ?1
                   AND plan.state IN ('approved', 'executing')
                   AND item.state = 'in_progress'
                 ORDER BY plan.updated_at DESC, item.ordinal, item.id LIMIT 2",
            )
            .map_err(db_error)?;
        let rows = statement
            .query_map(params![task_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        match rows.as_slice() {
            [] => Ok(None),
            [(plan_id, revision, item_id)] => Ok(Some(PlanFeatureRef {
                plan_id: plan_id.clone(),
                plan_revision: i64_to_u64(*revision, "plan revision")?,
                item_id: item_id.clone(),
            })),
            _ => Err(ProductError::StateMachineError(format!(
                "task {task_id} has multiple in-progress Plan features"
            ))),
        }
    }

    /// Resolve the three-state Plan execution policy from trusted task/run identity.
    ///
    /// An executing Plan without exactly one active feature is deliberately `Paused`: callers
    /// must not silently downgrade such writes to ordinary Git review.
    pub fn execution_status_for_run(
        &self,
        task_id: &str,
        run_id: &str,
    ) -> Result<PlanExecutionStatus, ProductError> {
        let conn = self.db.conn()?;
        let run_valid: bool = conn
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM agent_runs
                   WHERE id = ?1 AND task_id = ?2 AND ended_at IS NULL
                 )",
                params![run_id, task_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if !run_valid {
            return Err(ProductError::StateMachineError(format!(
                "run {run_id} does not belong to task {task_id}"
            )));
        }
        let executing_plans: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM plans WHERE task_id = ?1 AND state = 'executing'",
                params![task_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if executing_plans == 0 {
            return Ok(PlanExecutionStatus::NoExecutingPlan);
        }
        if executing_plans != 1 {
            return Err(ProductError::StateMachineError(format!(
                "task {task_id} has multiple executing Plans"
            )));
        }
        let active_features: i64 = conn
            .query_row(
                "SELECT COUNT(*)
                 FROM plans plan
                 JOIN plan_items item
                   ON item.plan_id = plan.id AND item.revision = plan.approved_revision
                 WHERE plan.task_id = ?1 AND plan.state = 'executing'
                   AND item.state = 'in_progress'",
                params![task_id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        match active_features {
            0 => Ok(PlanExecutionStatus::Paused),
            1 => Ok(PlanExecutionStatus::ActiveFeature),
            _ => Err(ProductError::StateMachineError(format!(
                "task {task_id} has multiple in-progress Plan features"
            ))),
        }
    }

    pub fn current_plan_for_task(
        &self,
        task_id: &str,
    ) -> Result<Option<PlanRevisionRef>, ProductError> {
        self.current_plan_identity(task_id).map(|current| {
            current.map(|(plan_id, plan_revision)| PlanRevisionRef {
                plan_id,
                plan_revision,
            })
        })
    }

    fn persist_write_observation(
        &self,
        task_id: &str,
        resolved: &ResolvedWorkspacePath,
        observed_before: Snapshot,
        observed_after: Snapshot,
        ownership: CapturedOwnership,
    ) -> Result<RecordPlanWriteOutcome, ProductError> {
        if observed_before == observed_after {
            return Ok(RecordPlanWriteOutcome::Unchanged {
                path: resolved.relative.clone(),
            });
        }
        let Some(feature) = ownership.feature else {
            return Ok(RecordPlanWriteOutcome::Unassigned {
                path: resolved.relative.clone(),
            });
        };

        self.validate_feature_context(task_id, &ownership.run_id, &feature)?;
        if let Some(existing) =
            self.event_by_tool_path(&ownership.tool_call_id, &resolved.relative)?
        {
            if existing.plan_id != feature.plan_id
                || existing.plan_revision != feature.plan_revision
                || existing.item_id != feature.item_id
            {
                return Err(ProductError::StateMachineError(format!(
                    "tool call {} already owns a different Plan change event for {}",
                    ownership.tool_call_id, resolved.relative
                )));
            }
            return Ok(RecordPlanWriteOutcome::Duplicate { event: existing });
        }

        let blob_store = self.blob_store();
        let before_hash = observed_before
            .bytes
            .as_deref()
            .map(|bytes| blob_store.put(bytes))
            .transpose()?;
        let after_hash = observed_after
            .bytes
            .as_deref()
            .map(|bytes| blob_store.put(bytes))
            .transpose()?;
        let event_id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let mut conn = self.db.conn()?;
        let tx = conn.transaction().map_err(db_error)?;
        increment_optional_ref(&tx, before_hash.as_deref(), &now)?;
        increment_optional_ref(&tx, after_hash.as_deref(), &now)?;
        tx.execute(
            "INSERT INTO plan_change_events (
                 id, plan_id, plan_revision, item_id, task_id, run_id, tool_call_id, path,
                 before_blob_hash, before_exists, after_blob_hash, after_exists, state,
                 created_at, finalized_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                       'captured', ?13, ?13)",
            params![
                event_id,
                feature.plan_id,
                to_i64(feature.plan_revision, "plan revision")?,
                feature.item_id,
                task_id,
                ownership.run_id,
                ownership.tool_call_id,
                resolved.relative,
                before_hash,
                bool_i64(observed_before.exists()),
                after_hash,
                bool_i64(observed_after.exists()),
                now,
            ],
        )
        .map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        drop(conn);

        let event = self
            .event_by_id(&event_id)?
            .ok_or_else(|| ProductError::DatabaseError("inserted Plan event is missing".into()))?;
        Ok(RecordPlanWriteOutcome::Captured { event })
    }

    /// Lists only the current Plan aggregate. Git changes and unassigned shell writes never enter it.
    pub fn list_current(&self, task_id: &str) -> Result<Option<EnhancedReviewView>, ProductError> {
        let Some((plan_id, revision)) = self.current_plan_identity(task_id)? else {
            return Ok(None);
        };
        let conn = self.db.conn()?;
        let decisions = load_decisions(&conn, &plan_id, revision)?;
        let mut statement = conn
            .prepare(
                "SELECT item.id, item.ordinal, item.title, item.description, item.state
                 FROM plan_items item
                 WHERE item.plan_id = ?1 AND item.revision = ?2
                   AND EXISTS (
                     SELECT 1 FROM plan_change_events event
                     WHERE event.plan_id = item.plan_id
                       AND event.plan_revision = item.revision
                       AND event.item_id = item.id AND event.state = 'captured'
                   )
                 ORDER BY item.ordinal, item.id",
            )
            .map_err(db_error)?;
        let mut rows = statement
            .query(params![plan_id, to_i64(revision, "plan revision")?])
            .map_err(db_error)?;
        let mut groups = Vec::new();
        while let Some(row) = rows.next().map_err(db_error)? {
            let item_id: String = row.get(0).map_err(db_error)?;
            let state: String = row.get(4).map_err(db_error)?;
            groups.push(EnhancedReviewGroupView {
                ordinal: i64_to_u32(row.get(1).map_err(db_error)?, "item ordinal")?,
                title: row.get(2).map_err(db_error)?,
                description: row.get(3).map_err(db_error)?,
                state: PlanItemState::try_from_str(&state).ok_or_else(|| {
                    ProductError::DatabaseError(format!("invalid Plan item state {state}"))
                })?,
                decision: decisions.get(&(item_id.clone(), None)).copied(),
                files: self.load_group_files(&conn, &plan_id, revision, &item_id, &decisions)?,
                item_id,
            });
        }
        Ok(Some(EnhancedReviewView {
            task_id: task_id.to_string(),
            plan_id,
            plan_revision: revision,
            groups,
        }))
    }

    pub fn accept_feature(
        &self,
        target: &EnhancedReviewTarget,
    ) -> Result<PlanReviewDecision, ProductError> {
        if target.path.is_some() {
            return Err(invalid("feature acceptance cannot carry a path"));
        }
        self.validate_review_target(target, PlanReviewScope::Feature)?;
        self.ensure_group_action_is_available(target, PlanReviewDecisionKind::Accepted)?;
        self.persist_decision(
            target,
            PlanReviewScope::Feature,
            PlanReviewDecisionKind::Accepted,
        )
    }

    pub fn accept_file(
        &self,
        target: &EnhancedReviewTarget,
    ) -> Result<PlanReviewDecision, ProductError> {
        self.validate_review_target(target, PlanReviewScope::File)?;
        self.ensure_file_action_is_available(target, PlanReviewDecisionKind::Accepted)?;
        self.persist_decision(
            target,
            PlanReviewScope::File,
            PlanReviewDecisionKind::Accepted,
        )
    }

    pub async fn reject_file(
        &self,
        workspace_root: &Path,
        target: &EnhancedReviewTarget,
    ) -> Result<PlanRejectResult, ProductError> {
        self.validate_review_target(target, PlanReviewScope::File)?;
        self.ensure_file_action_is_available(target, PlanReviewDecisionKind::Rejected)?;
        let path = target
            .path
            .as_ref()
            .ok_or_else(|| invalid("file rejection requires a path"))?;
        self.reject_paths(
            workspace_root,
            target,
            PlanReviewScope::File,
            vec![path.clone()],
        )
        .await
    }

    pub async fn reject_feature(
        &self,
        workspace_root: &Path,
        target: &EnhancedReviewTarget,
    ) -> Result<PlanRejectResult, ProductError> {
        if target.path.is_some() {
            return Err(invalid("feature rejection cannot carry a path"));
        }
        self.validate_review_target(target, PlanReviewScope::Feature)?;
        self.ensure_group_action_is_available(target, PlanReviewDecisionKind::Rejected)?;
        // Keep rusqlite guards in a lexical block. They are intentionally dropped before the
        // first await so the command future remains Send for Tauri's async IPC executor.
        let paths = {
            let conn = self.db.conn()?;
            let mut statement = conn
                .prepare(
                    "SELECT DISTINCT path FROM plan_change_events
                     WHERE plan_id = ?1 AND plan_revision = ?2 AND item_id = ?3
                       AND state = 'captured' ORDER BY path",
                )
                .map_err(db_error)?;
            let paths = statement
                .query_map(
                    params![
                        target.plan_id,
                        to_i64(target.plan_revision, "plan revision")?,
                        target.item_id
                    ],
                    |row| row.get::<_, String>(0),
                )
                .map_err(db_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(db_error)?;
            paths
        };
        self.reject_paths(workspace_root, target, PlanReviewScope::Feature, paths)
            .await
    }

    /// Restores every non-terminal rejection to its pre-operation snapshots.
    pub async fn recover_pending(&self) -> Result<PlanRejectRecoveryReport, ProductError> {
        let operations = self.load_recoverable_operations()?;
        let mut report = PlanRejectRecoveryReport::default();
        for operation in operations {
            match self.recover_operation(&operation).await {
                Ok(RecoveryOutcome::Recovered) => report.recovered_operation_ids.push(operation.id),
                Ok(RecoveryOutcome::Conflict) => report.conflicted_operation_ids.push(operation.id),
                Ok(RecoveryOutcome::Retryable) => report.retryable_operation_ids.push(operation.id),
                Err(error) => {
                    self.mark_operation_retryable(&operation.id, &error.to_string())?;
                    report.retryable_operation_ids.push(operation.id);
                }
            }
        }
        Ok(report)
    }

    async fn reject_paths(
        &self,
        workspace_root: &Path,
        target: &EnhancedReviewTarget,
        scope: PlanReviewScope,
        mut paths: Vec<String>,
    ) -> Result<PlanRejectResult, ProductError> {
        if let Some(existing) = self.existing_decision(target, scope)? {
            if existing.decision == PlanReviewDecisionKind::Rejected {
                return Ok(PlanRejectResult {
                    operation_id: String::new(),
                    decision: existing,
                    changed_paths: Vec::new(),
                    idempotent: true,
                });
            }
        }
        paths.sort();
        paths.dedup();
        if paths.is_empty() {
            let decision =
                self.persist_decision(target, scope, PlanReviewDecisionKind::Rejected)?;
            return Ok(PlanRejectResult {
                operation_id: String::new(),
                decision,
                changed_paths: Vec::new(),
                idempotent: false,
            });
        }

        let mut resolved = Vec::with_capacity(paths.len());
        for path in &paths {
            resolved.push(self.resolve_task_path(
                &target.task_id,
                workspace_root,
                Path::new(path),
            )?);
        }
        resolved.sort_by(|left, right| {
            path_sort_key(&left.absolute).cmp(&path_sort_key(&right.absolute))
        });
        let _lease = self
            .coordinator
            .acquire(resolved.iter().map(|path| path.absolute.clone()))
            .await?;

        let mut prepared = Vec::with_capacity(resolved.len());
        for path in resolved {
            let rollback = Snapshot::from_option(
                self.file_system
                    .read_snapshot(&path.guard, &path.absolute)
                    .map_err(io_error)?,
            );
            let events = self.load_feature_events_for_path(target, &path.relative)?;
            if events.is_empty() {
                return Err(invalid(format!(
                    "feature {} has no captured event for {}",
                    target.item_id, path.relative
                )));
            }
            match self.compute_rejection(&rollback, &events) {
                Ok(desired) => prepared.push(PreparedFile {
                    resolved: path,
                    rollback,
                    desired,
                }),
                Err(error) => {
                    self.record_conflict_operation(target, scope, Some(&error.to_string()))?;
                    return Err(error);
                }
            }
        }

        let operation_id = self.prepare_operation(target, scope, &prepared)?;
        self.update_operation_state(&operation_id, "applying", None, false)?;
        for file in &prepared {
            if let Err(error) = self.apply_prepared_file(&operation_id, file) {
                self.update_operation_state(
                    &operation_id,
                    "rolling_back",
                    Some(&error.to_string()),
                    false,
                )?;
                let rollback_result = self.rollback_prepared_files(&operation_id, &prepared);
                return match rollback_result {
                    Ok(()) => Err(error),
                    Err(rollback_error) => Err(ProductError::RollbackError(format!(
                        "rejection failed: {error}; rollback remains pending: {rollback_error}"
                    ))),
                };
            }
        }

        let decision = match self.commit_rejection(target, scope, &operation_id) {
            Ok(decision) => decision,
            Err(error) => {
                self.update_operation_state(
                    &operation_id,
                    "rolling_back",
                    Some(&error.to_string()),
                    false,
                )?;
                let rollback_result = self.rollback_prepared_files(&operation_id, &prepared);
                return match rollback_result {
                    Ok(()) => Err(error),
                    Err(rollback_error) => Err(ProductError::RollbackError(format!(
                        "decision commit failed: {error}; rollback remains pending: {rollback_error}"
                    ))),
                };
            }
        };
        Ok(PlanRejectResult {
            operation_id,
            decision,
            changed_paths: prepared
                .into_iter()
                .map(|file| file.resolved.relative)
                .collect(),
            idempotent: false,
        })
    }

    fn apply_prepared_file(
        &self,
        operation_id: &str,
        file: &PreparedFile,
    ) -> Result<(), ProductError> {
        let current = Snapshot::from_option(
            self.file_system
                .read_snapshot(&file.resolved.guard, &file.resolved.absolute)
                .map_err(io_error)?,
        );
        if current != file.rollback {
            self.mark_operation_file_conflict(
                operation_id,
                &file.resolved.relative,
                "file changed after rejection was prepared",
            )?;
            return Err(ProductError::RollbackError(format!(
                "{} changed while rejection was being prepared",
                file.resolved.relative
            )));
        }
        self.file_system
            .write_snapshot(
                &file.resolved.guard,
                &file.resolved.absolute,
                file.desired.bytes.as_deref(),
            )
            .map_err(io_error)?;
        let now = Utc::now().to_rfc3339();
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE plan_reject_operation_files
             SET state = 'applied', applied_at = ?1, error = NULL
             WHERE operation_id = ?2 AND path = ?3",
            params![now, operation_id, file.resolved.relative],
        )
        .map_err(db_error)?;
        Ok(())
    }

    fn rollback_prepared_files(
        &self,
        operation_id: &str,
        files: &[PreparedFile],
    ) -> Result<(), ProductError> {
        let mut first_error = None;
        for file in files.iter().rev() {
            let result = self.restore_if_operation_owned(
                operation_id,
                &file.resolved,
                &file.rollback,
                &file.desired,
            );
            if let Err(error) = result {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        if let Some(error) = first_error {
            self.update_operation_state(
                operation_id,
                "rolling_back",
                Some(&error.to_string()),
                false,
            )?;
            return Err(error);
        }
        self.update_operation_state(operation_id, "rolled_back", None, true)?;
        Ok(())
    }

    fn restore_if_operation_owned(
        &self,
        operation_id: &str,
        path: &ResolvedWorkspacePath,
        rollback: &Snapshot,
        desired: &Snapshot,
    ) -> Result<(), ProductError> {
        let current = Snapshot::from_option(
            self.file_system
                .read_snapshot(&path.guard, &path.absolute)
                .map_err(io_error)?,
        );
        if current == *rollback {
            self.mark_operation_file_rolled_back(operation_id, &path.relative)?;
            return Ok(());
        }
        if current != *desired {
            self.mark_operation_file_conflict(
                operation_id,
                &path.relative,
                "external change prevents rollback",
            )?;
            return Err(ProductError::RollbackError(format!(
                "{} changed externally during rejection rollback",
                path.relative
            )));
        }
        self.file_system
            .write_snapshot(&path.guard, &path.absolute, rollback.bytes.as_deref())
            .map_err(io_error)?;
        self.mark_operation_file_rolled_back(operation_id, &path.relative)
    }

    fn compute_rejection(
        &self,
        current: &Snapshot,
        events: &[PlanChangeEvent],
    ) -> Result<Snapshot, ProductError> {
        let mut desired = current.clone();
        for event in events.iter().rev() {
            let before =
                self.snapshot_from_blob(event.before_exists, event.before_blob_hash.as_deref())?;
            let after = self.snapshot_from_blob(
                event.after_exists.ok_or_else(|| {
                    ProductError::DatabaseError(format!(
                        "captured Plan event {} has no after snapshot",
                        event.id
                    ))
                })?,
                event.after_blob_hash.as_deref(),
            )?;
            desired = inverse_merge(&after, &desired, &before).map_err(|_| {
                ProductError::RollbackError(format!(
                    "feature rejection conflicts at event {}",
                    event.id
                ))
            })?;
        }
        Ok(desired)
    }

    fn prepare_operation(
        &self,
        target: &EnhancedReviewTarget,
        scope: PlanReviewScope,
        files: &[PreparedFile],
    ) -> Result<String, ProductError> {
        let operation_id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let blob_store = self.blob_store();
        let mut stored = Vec::with_capacity(files.len());
        for file in files {
            stored.push(StoredJournalFile {
                path: file.resolved.relative.clone(),
                expected_hash: file
                    .rollback
                    .bytes
                    .as_deref()
                    .map(|bytes| blob_store.put(bytes))
                    .transpose()?,
                rollback_hash: file
                    .rollback
                    .bytes
                    .as_deref()
                    .map(|bytes| blob_store.put(bytes))
                    .transpose()?,
                desired_hash: file
                    .desired
                    .bytes
                    .as_deref()
                    .map(|bytes| blob_store.put(bytes))
                    .transpose()?,
                expected_exists: file.rollback.exists(),
                rollback_exists: file.rollback.exists(),
                desired_exists: file.desired.exists(),
            });
        }

        let mut conn = self.db.conn()?;
        let tx = conn.transaction().map_err(db_error)?;
        tx.execute(
            "INSERT INTO plan_reject_operations (
                 id, plan_id, plan_revision, item_id, scope, path, state,
                 recovery_count, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'prepared', 0, ?7, ?7)",
            params![
                operation_id,
                target.plan_id,
                to_i64(target.plan_revision, "plan revision")?,
                target.item_id,
                scope.as_str(),
                target.path,
                now,
            ],
        )
        .map_err(db_error)?;
        for (ordinal, file) in stored.iter().enumerate() {
            increment_optional_ref(&tx, file.expected_hash.as_deref(), &now)?;
            increment_optional_ref(&tx, file.rollback_hash.as_deref(), &now)?;
            increment_optional_ref(&tx, file.desired_hash.as_deref(), &now)?;
            tx.execute(
                "INSERT INTO plan_reject_operation_files (
                     operation_id, ordinal, path, expected_current_hash, expected_exists,
                     rollback_hash, rollback_exists, desired_hash, desired_exists, state
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending')",
                params![
                    operation_id,
                    i64::try_from(ordinal)
                        .map_err(|_| invalid("rejection file ordinal overflow"))?,
                    file.path,
                    file.expected_hash,
                    bool_i64(file.expected_exists),
                    file.rollback_hash,
                    bool_i64(file.rollback_exists),
                    file.desired_hash,
                    bool_i64(file.desired_exists),
                ],
            )
            .map_err(db_error)?;
        }
        tx.commit().map_err(db_error)?;
        Ok(operation_id)
    }

    fn commit_rejection(
        &self,
        target: &EnhancedReviewTarget,
        scope: PlanReviewScope,
        operation_id: &str,
    ) -> Result<PlanReviewDecision, ProductError> {
        let decision = decision_from_target(target, scope, PlanReviewDecisionKind::Rejected);
        let now = decision.decided_at.to_rfc3339();
        let mut conn = self.db.conn()?;
        let tx = conn.transaction().map_err(db_error)?;
        // Release this operation's active database claim inside the same write transaction before
        // inserting its decision. The schema guard can then reject every *other* incompatible
        // claim without mistaking this operation for a competitor. No other writer can observe the
        // intermediate state because SQLite holds the transaction's write lock until commit.
        let updated = tx
            .execute(
                "UPDATE plan_reject_operations
             SET state = 'committed', error = NULL, updated_at = ?1, completed_at = ?1
             WHERE id = ?2 AND state = 'applying'",
                params![now, operation_id],
            )
            .map_err(db_error)?;
        if updated != 1 {
            return Err(invalid(format!(
                "rejection operation {operation_id} is no longer applying"
            )));
        }
        insert_decision(&tx, &decision)?;
        tx.commit().map_err(db_error)?;
        Ok(decision)
    }

    fn record_conflict_operation(
        &self,
        target: &EnhancedReviewTarget,
        scope: PlanReviewScope,
        error: Option<&str>,
    ) -> Result<String, ProductError> {
        let operation_id = Uuid::new_v4().to_string();
        let now = Utc::now().to_rfc3339();
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT INTO plan_reject_operations (
                 id, plan_id, plan_revision, item_id, scope, path, state, recovery_count,
                 error, created_at, updated_at, completed_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'conflict', 0, ?7, ?8, ?8, ?8)",
            params![
                operation_id,
                target.plan_id,
                to_i64(target.plan_revision, "plan revision")?,
                target.item_id,
                scope.as_str(),
                target.path,
                error,
                now,
            ],
        )
        .map_err(db_error)?;
        Ok(operation_id)
    }

    async fn recover_operation(
        &self,
        operation: &RecoverableOperation,
    ) -> Result<RecoveryOutcome, ProductError> {
        let workspace_root = self.task_workspace_root(&operation.task_id)?;
        let mut files = Vec::with_capacity(operation.files.len());
        for journal in &operation.files {
            let resolved = self.resolve_task_path(
                &operation.task_id,
                &workspace_root,
                Path::new(&journal.path),
            )?;
            files.push((resolved, journal.clone()));
        }
        files.sort_by(|left, right| {
            path_sort_key(&left.0.absolute).cmp(&path_sort_key(&right.0.absolute))
        });
        let _lease = self
            .coordinator
            .acquire(files.iter().map(|file| file.0.absolute.clone()))
            .await?;
        self.bump_recovery_count(&operation.id)?;

        let mut found_conflict = false;
        let mut retryable_error = None;
        for (path, journal) in &files {
            let rollback =
                self.snapshot_from_blob(journal.rollback_exists, journal.rollback_hash.as_deref())?;
            let desired =
                self.snapshot_from_blob(journal.desired_exists, journal.desired_hash.as_deref())?;
            let expected =
                self.snapshot_from_blob(journal.expected_exists, journal.expected_hash.as_deref())?;
            let current = Snapshot::from_option(
                self.file_system
                    .read_snapshot(&path.guard, &path.absolute)
                    .map_err(io_error)?,
            );
            if current == rollback || current == expected {
                self.mark_operation_file_rolled_back(&operation.id, &path.relative)?;
                continue;
            }
            if current != desired {
                self.mark_operation_file_conflict(
                    &operation.id,
                    &path.relative,
                    "recovery found an external change",
                )?;
                found_conflict = true;
                continue;
            }
            if let Err(error) = self.file_system.write_snapshot(
                &path.guard,
                &path.absolute,
                rollback.bytes.as_deref(),
            ) {
                retryable_error.get_or_insert_with(|| error.to_string());
                continue;
            }
            self.mark_operation_file_rolled_back(&operation.id, &path.relative)?;
        }
        if let Some(error) = retryable_error {
            self.update_operation_state(&operation.id, "rolling_back", Some(&error), false)?;
            return Ok(RecoveryOutcome::Retryable);
        }
        if found_conflict {
            self.update_operation_state(
                &operation.id,
                "conflict",
                Some("recovery found an external change"),
                true,
            )?;
            return Ok(RecoveryOutcome::Conflict);
        }
        self.update_operation_state(&operation.id, "rolled_back", None, true)?;
        Ok(RecoveryOutcome::Recovered)
    }

    fn load_recoverable_operations(&self) -> Result<Vec<RecoverableOperation>, ProductError> {
        let conn = self.db.conn()?;
        let mut statement = conn
            .prepare(
                "SELECT operation.id, plan.task_id
                 FROM plan_reject_operations operation
                 JOIN plans plan ON plan.id = operation.plan_id
                 WHERE operation.state IN ('prepared', 'applying', 'rolling_back')
                 ORDER BY operation.created_at, operation.id",
            )
            .map_err(db_error)?;
        let headers = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        drop(statement);
        let mut operations = Vec::with_capacity(headers.len());
        for (id, task_id) in headers {
            let mut files_statement = conn
                .prepare(
                    "SELECT path, expected_current_hash, expected_exists, rollback_hash,
                            rollback_exists, desired_hash, desired_exists
                     FROM plan_reject_operation_files
                     WHERE operation_id = ?1 ORDER BY ordinal",
                )
                .map_err(db_error)?;
            let files = files_statement
                .query_map(params![id], |row| {
                    Ok(RecoveryFile {
                        path: row.get(0)?,
                        expected_hash: row.get(1)?,
                        expected_exists: row.get::<_, i64>(2)? != 0,
                        rollback_hash: row.get(3)?,
                        rollback_exists: row.get::<_, i64>(4)? != 0,
                        desired_hash: row.get(5)?,
                        desired_exists: row.get::<_, i64>(6)? != 0,
                    })
                })
                .map_err(db_error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(db_error)?;
            operations.push(RecoverableOperation { id, task_id, files });
        }
        Ok(operations)
    }

    fn validate_feature_context(
        &self,
        task_id: &str,
        run_id: &str,
        feature: &PlanFeatureRef,
    ) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        let valid: bool = conn
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM plans plan
                   JOIN plan_items item
                     ON item.plan_id = plan.id AND item.revision = ?2 AND item.id = ?3
                   JOIN agent_runs run ON run.id = ?5 AND run.task_id = plan.task_id
                   WHERE plan.id = ?1 AND plan.task_id = ?4
                     AND plan.approved_revision = ?2
                     AND plan.state IN ('approved', 'executing', 'completed')
                 )",
                params![
                    feature.plan_id,
                    to_i64(feature.plan_revision, "plan revision")?,
                    feature.item_id,
                    task_id,
                    run_id,
                ],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if !valid {
            return Err(ProductError::StateMachineError(format!(
                "invalid or stale feature attribution {}@{}:{} for task {task_id}",
                feature.plan_id, feature.plan_revision, feature.item_id
            )));
        }
        Ok(())
    }

    fn validate_review_target(
        &self,
        target: &EnhancedReviewTarget,
        scope: PlanReviewScope,
    ) -> Result<(), ProductError> {
        let current = self
            .current_plan_identity(&target.task_id)?
            .ok_or_else(|| invalid("task has no current Plan"))?;
        if current != (target.plan_id.clone(), target.plan_revision) {
            return Err(ProductError::StateMachineError(format!(
                "stale enhanced-review target {}@{}; current is {}@{}",
                target.plan_id, target.plan_revision, current.0, current.1
            )));
        }
        match scope {
            PlanReviewScope::Feature if target.path.is_some() => {
                return Err(invalid("feature target cannot carry a path"));
            }
            PlanReviewScope::File if target.path.as_deref().unwrap_or("").trim().is_empty() => {
                return Err(invalid("file target requires a non-empty path"));
            }
            _ => {}
        }
        let conn = self.db.conn()?;
        let event_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM plan_change_events
                 WHERE plan_id = ?1 AND plan_revision = ?2 AND item_id = ?3
                   AND state = 'captured'
                   AND (?4 IS NULL OR path = ?4)",
                params![
                    target.plan_id,
                    to_i64(target.plan_revision, "plan revision")?,
                    target.item_id,
                    target.path,
                ],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if event_count == 0 {
            return Err(invalid(format!(
                "enhanced-review target has no captured changes: {}",
                target.item_id
            )));
        }
        let item_state: String = conn
            .query_row(
                "SELECT state FROM plan_items
                 WHERE plan_id = ?1 AND revision = ?2 AND id = ?3",
                params![
                    target.plan_id,
                    to_i64(target.plan_revision, "plan revision")?,
                    target.item_id,
                ],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if !matches!(item_state.as_str(), "completed" | "failed" | "cancelled") {
            return Err(ProductError::StateMachineError(format!(
                "{PLAN_REVIEW_FEATURE_NOT_TERMINAL}: feature {} is {item_state}",
                target.item_id
            )));
        }
        Ok(())
    }

    fn ensure_file_action_is_available(
        &self,
        target: &EnhancedReviewTarget,
        requested: PlanReviewDecisionKind,
    ) -> Result<(), ProductError> {
        let feature_target = EnhancedReviewTarget {
            path: None,
            ..target.clone()
        };
        if let Some(group) = self.existing_decision(&feature_target, PlanReviewScope::Feature)? {
            return Err(invalid(format!(
                "feature {} was already {} as a group",
                target.item_id, group.decision
            )));
        }
        if let Some(existing) = self.existing_decision(target, PlanReviewScope::File)? {
            if existing.decision != requested {
                return Err(invalid(format!(
                    "file {} already has final decision {}",
                    target.path.as_deref().unwrap_or_default(),
                    existing.decision
                )));
            }
        }
        Ok(())
    }

    fn ensure_group_action_is_available(
        &self,
        target: &EnhancedReviewTarget,
        requested: PlanReviewDecisionKind,
    ) -> Result<(), ProductError> {
        if let Some(existing) = self.existing_decision(target, PlanReviewScope::Feature)? {
            if existing.decision != requested {
                return Err(invalid(format!(
                    "feature {} already has final decision {}",
                    target.item_id, existing.decision
                )));
            }
            return Ok(());
        }
        let conn = self.db.conn()?;
        let file_decisions: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM plan_review_decisions
                 WHERE plan_id = ?1 AND plan_revision = ?2 AND item_id = ?3 AND scope = 'file'",
                params![
                    target.plan_id,
                    to_i64(target.plan_revision, "plan revision")?,
                    target.item_id,
                ],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if file_decisions > 0 {
            return Err(invalid(format!(
                "feature {} already has file-level decisions",
                target.item_id
            )));
        }
        Ok(())
    }

    fn existing_decision(
        &self,
        target: &EnhancedReviewTarget,
        scope: PlanReviewScope,
    ) -> Result<Option<PlanReviewDecision>, ProductError> {
        let conn = self.db.conn()?;
        conn.query_row(
            "SELECT id, plan_id, plan_revision, item_id, scope, path, decision, decided_at
             FROM plan_review_decisions
             WHERE plan_id = ?1 AND plan_revision = ?2 AND item_id = ?3
               AND scope = ?4
               AND ((?5 IS NULL AND path IS NULL) OR path = ?5)",
            params![
                target.plan_id,
                to_i64(target.plan_revision, "plan revision")?,
                target.item_id,
                scope.as_str(),
                target.path,
            ],
            map_decision,
        )
        .optional()
        .map_err(db_error)
    }

    fn persist_decision(
        &self,
        target: &EnhancedReviewTarget,
        scope: PlanReviewScope,
        kind: PlanReviewDecisionKind,
    ) -> Result<PlanReviewDecision, ProductError> {
        if let Some(existing) = self.existing_decision(target, scope)? {
            if existing.decision == kind {
                return Ok(existing);
            }
            return Err(invalid("review decisions are final and cannot be reversed"));
        }
        let decision = decision_from_target(target, scope, kind);
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT INTO plan_review_decisions (
                 id, plan_id, plan_revision, item_id, scope, path, decision, decided_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                decision.id,
                decision.plan_id,
                to_i64(decision.plan_revision, "plan revision")?,
                decision.item_id,
                decision.scope.as_str(),
                decision.path,
                decision.decision.as_str(),
                decision.decided_at.to_rfc3339(),
            ],
        )
        .map_err(db_error)?;
        Ok(decision)
    }

    fn current_plan_identity(&self, task_id: &str) -> Result<Option<(String, u64)>, ProductError> {
        let conn = self.db.conn()?;
        let row: Option<(String, i64)> = conn
            .query_row(
                "SELECT id, COALESCE(approved_revision, revision)
                 FROM plans WHERE task_id = ?1
                 ORDER BY CASE WHEN state NOT IN ('completed', 'cancelled') THEN 0 ELSE 1 END,
                          updated_at DESC, id DESC LIMIT 1",
                params![task_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(db_error)?;
        row.map(|(id, revision)| Ok((id, i64_to_u64(revision, "plan revision")?)))
            .transpose()
    }

    fn load_group_files(
        &self,
        conn: &rusqlite::Connection,
        plan_id: &str,
        revision: u64,
        item_id: &str,
        decisions: &HashMap<(String, Option<String>), PlanReviewDecisionKind>,
    ) -> Result<Vec<EnhancedReviewFileView>, ProductError> {
        let mut statement = conn
            .prepare(
                "SELECT path, MIN(sequence), MAX(sequence)
                 FROM plan_change_events
                 WHERE plan_id = ?1 AND plan_revision = ?2 AND item_id = ?3
                   AND state = 'captured'
                 GROUP BY path ORDER BY MIN(sequence), path",
            )
            .map_err(db_error)?;
        let rows = statement
            .query_map(
                params![plan_id, to_i64(revision, "plan revision")?, item_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        let group_decision = decisions.get(&(item_id.to_string(), None)).copied();
        let mut files = Vec::with_capacity(rows.len());
        for (path, first_sequence, last_sequence) in rows {
            files.push(EnhancedReviewFileView {
                decision: decisions
                    .get(&(item_id.to_string(), Some(path.clone())))
                    .copied()
                    .or(group_decision),
                events: self.load_event_views(conn, plan_id, revision, item_id, &path)?,
                path,
                first_sequence,
                last_sequence,
            });
        }
        Ok(files)
    }

    fn load_event_views(
        &self,
        conn: &rusqlite::Connection,
        plan_id: &str,
        revision: u64,
        item_id: &str,
        path: &str,
    ) -> Result<Vec<EnhancedReviewEventView>, ProductError> {
        let events = load_events(conn, plan_id, revision, item_id, path)?;
        events
            .into_iter()
            .map(|event| {
                let before = self
                    .snapshot_from_blob(event.before_exists, event.before_blob_hash.as_deref())?;
                let after_exists = event.after_exists.ok_or_else(|| {
                    ProductError::DatabaseError(format!(
                        "captured Plan event {} has no after snapshot",
                        event.id
                    ))
                })?;
                let after =
                    self.snapshot_from_blob(after_exists, event.after_blob_hash.as_deref())?;
                let (patch, binary) = event_patch(&before, &after);
                Ok(EnhancedReviewEventView {
                    sequence: event.sequence,
                    event_id: event.id,
                    tool_call_id: event.tool_call_id,
                    before_exists: event.before_exists,
                    after_exists,
                    before_blob_hash: event.before_blob_hash,
                    after_blob_hash: event.after_blob_hash,
                    patch,
                    binary,
                })
            })
            .collect()
    }

    fn load_feature_events_for_path(
        &self,
        target: &EnhancedReviewTarget,
        path: &str,
    ) -> Result<Vec<PlanChangeEvent>, ProductError> {
        let conn = self.db.conn()?;
        load_events(
            &conn,
            &target.plan_id,
            target.plan_revision,
            &target.item_id,
            path,
        )
    }

    fn event_by_tool_path(
        &self,
        tool_call_id: &str,
        path: &str,
    ) -> Result<Option<PlanChangeEvent>, ProductError> {
        let conn = self.db.conn()?;
        conn.query_row(
            &format!(
                "SELECT {EVENT_COLUMNS} FROM plan_change_events
                 WHERE tool_call_id = ?1 AND path = ?2"
            ),
            params![tool_call_id, path],
            map_event,
        )
        .optional()
        .map_err(db_error)
    }

    fn event_by_id(&self, id: &str) -> Result<Option<PlanChangeEvent>, ProductError> {
        let conn = self.db.conn()?;
        conn.query_row(
            &format!("SELECT {EVENT_COLUMNS} FROM plan_change_events WHERE id = ?1"),
            params![id],
            map_event,
        )
        .optional()
        .map_err(db_error)
    }

    fn snapshot_from_blob(
        &self,
        exists: bool,
        hash: Option<&str>,
    ) -> Result<Snapshot, ProductError> {
        if !exists {
            if hash.is_some() {
                return Err(ProductError::DatabaseError(
                    "absent snapshot unexpectedly references a blob".into(),
                ));
            }
            return Ok(Snapshot::absent());
        }
        let hash = hash.ok_or_else(|| {
            ProductError::DatabaseError("existing snapshot has no blob hash".into())
        })?;
        let bytes = self
            .blob_store()
            .get(hash)?
            .ok_or_else(|| ProductError::BlobError(format!("missing referenced blob {hash}")))?;
        if blake3::hash(&bytes).to_hex().as_str() != hash {
            return Err(ProductError::BlobError(format!(
                "referenced blob {hash} failed content verification"
            )));
        }
        Ok(Snapshot::present(bytes))
    }

    fn resolve_task_path(
        &self,
        task_id: &str,
        workspace_root: &Path,
        path: &Path,
    ) -> Result<ResolvedWorkspacePath, ProductError> {
        let configured_root = self.task_workspace_root(task_id)?;
        let configured_guard = PathGuard::new(configured_root)?;
        let supplied_guard = PathGuard::new(workspace_root.to_path_buf())?;
        if configured_guard.root() != supplied_guard.root() {
            return Err(ProductError::PathEscape(format!(
                "workspace root {:?} does not match task {task_id} root {:?}",
                supplied_guard.root(),
                configured_guard.root()
            )));
        }
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            configured_guard.root().join(path)
        };
        let absolute = configured_guard.resolve(&candidate)?;
        let relative_path = absolute
            .strip_prefix(configured_guard.root())
            .map_err(|_| {
                ProductError::PathEscape(format!(
                    "resolved path {absolute:?} is outside task workspace"
                ))
            })?;
        if relative_path.as_os_str().is_empty() {
            return Err(ProductError::PathEscape(
                "workspace root cannot be reviewed as a file".into(),
            ));
        }
        let relative = portable_relative_path(relative_path)?;
        Ok(ResolvedWorkspacePath {
            absolute,
            relative,
            guard: configured_guard,
        })
    }

    fn task_workspace_root(&self, task_id: &str) -> Result<PathBuf, ProductError> {
        let conn = self.db.conn()?;
        let root: Option<String> = conn
            .query_row(
                "SELECT workspace_path FROM tasks WHERE id = ?1",
                params![task_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_error)?
            .flatten();
        root.filter(|path| !path.trim().is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| invalid(format!("task {task_id} has no workspace")))
    }

    fn blob_store(&self) -> BlobStore<'_> {
        BlobStore::new(self.db, self.blobs_dir.clone())
    }

    fn update_operation_state(
        &self,
        operation_id: &str,
        state: &str,
        error: Option<&str>,
        terminal: bool,
    ) -> Result<(), ProductError> {
        let now = Utc::now().to_rfc3339();
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE plan_reject_operations
             SET state = ?1, error = ?2, updated_at = ?3,
                 completed_at = CASE WHEN ?4 = 1 THEN ?3 ELSE NULL END
             WHERE id = ?5",
            params![state, error, now, bool_i64(terminal), operation_id],
        )
        .map_err(db_error)?;
        Ok(())
    }

    fn mark_operation_file_rolled_back(
        &self,
        operation_id: &str,
        path: &str,
    ) -> Result<(), ProductError> {
        let now = Utc::now().to_rfc3339();
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE plan_reject_operation_files
             SET state = 'rolled_back', error = NULL,
                 applied_at = COALESCE(applied_at, ?1), rolled_back_at = ?1
             WHERE operation_id = ?2 AND path = ?3",
            params![now, operation_id, path],
        )
        .map_err(db_error)?;
        Ok(())
    }

    fn mark_operation_file_conflict(
        &self,
        operation_id: &str,
        path: &str,
        error: &str,
    ) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE plan_reject_operation_files
             SET state = 'conflict', error = ?1
             WHERE operation_id = ?2 AND path = ?3",
            params![error, operation_id, path],
        )
        .map_err(db_error)?;
        Ok(())
    }

    fn bump_recovery_count(&self, operation_id: &str) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE plan_reject_operations
             SET recovery_count = recovery_count + 1, updated_at = ?1
             WHERE id = ?2",
            params![Utc::now().to_rfc3339(), operation_id],
        )
        .map_err(db_error)?;
        Ok(())
    }

    fn mark_operation_retryable(
        &self,
        operation_id: &str,
        error: &str,
    ) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE plan_reject_operations
             SET state = 'rolling_back', recovery_count = recovery_count + 1,
                 error = ?1, updated_at = ?2, completed_at = NULL
             WHERE id = ?3",
            params![error, Utc::now().to_rfc3339(), operation_id],
        )
        .map_err(db_error)?;
        Ok(())
    }
}

const EVENT_COLUMNS: &str = "sequence, id, plan_id, plan_revision, item_id, task_id, run_id, \
    tool_call_id, path, before_blob_hash, before_exists, after_blob_hash, after_exists, state, \
    error, created_at, finalized_at";

#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    bytes: Option<Vec<u8>>,
}

impl Snapshot {
    fn from_option(bytes: Option<Vec<u8>>) -> Self {
        Self { bytes }
    }

    fn present(bytes: Vec<u8>) -> Self {
        Self { bytes: Some(bytes) }
    }

    fn absent() -> Self {
        Self { bytes: None }
    }

    fn exists(&self) -> bool {
        self.bytes.is_some()
    }
}

#[derive(Debug, Clone)]
struct ResolvedWorkspacePath {
    absolute: PathBuf,
    relative: String,
    /// Absolute paths are retained for locking and display only. Filesystem I/O uses this
    /// directory capability, which remains bound to the task workspace after path resolution.
    guard: PathGuard,
}

struct CapturedOwnership {
    run_id: String,
    tool_call_id: String,
    feature: Option<PlanFeatureRef>,
}

#[derive(Debug)]
struct PreparedFile {
    resolved: ResolvedWorkspacePath,
    rollback: Snapshot,
    desired: Snapshot,
}

struct StoredJournalFile {
    path: String,
    expected_hash: Option<String>,
    expected_exists: bool,
    rollback_hash: Option<String>,
    rollback_exists: bool,
    desired_hash: Option<String>,
    desired_exists: bool,
}

struct RecoverableOperation {
    id: String,
    task_id: String,
    files: Vec<RecoveryFile>,
}

#[derive(Clone)]
struct RecoveryFile {
    path: String,
    expected_hash: Option<String>,
    expected_exists: bool,
    rollback_hash: Option<String>,
    rollback_exists: bool,
    desired_hash: Option<String>,
    desired_exists: bool,
}

enum RecoveryOutcome {
    Recovered,
    Conflict,
    Retryable,
}

fn inverse_merge(
    after: &Snapshot,
    current: &Snapshot,
    before: &Snapshot,
) -> Result<Snapshot, Vec<u8>> {
    match (&after.bytes, &current.bytes, &before.bytes) {
        (None, None, _) => Ok(before.clone()),
        (Some(_), None, _) => {
            // A later writer deleted the file. Removing the target feature must preserve that
            // later deletion rather than resurrecting older content.
            Ok(Snapshot::absent())
        }
        (None, Some(current), None) => Ok(Snapshot::present(current.clone())),
        (None, Some(current), Some(before)) if current == before => {
            Ok(Snapshot::present(current.clone()))
        }
        (None, Some(_), Some(_)) => Err(b"file was recreated after the target deletion".to_vec()),
        (Some(after), Some(current), before) => {
            let before_bytes = before.as_deref().unwrap_or_default();
            let merged = match diffy::merge_bytes(after, current, before_bytes) {
                Ok(merged) => merged,
                Err(conflict) => {
                    aligned_line_inverse_merge(after, current, before_bytes).ok_or(conflict)?
                }
            };
            let preserve_later_existence = current != after;
            let exists = before.is_some() || preserve_later_existence || !merged.is_empty();
            if exists {
                Ok(Snapshot::present(merged))
            } else {
                Ok(Snapshot::absent())
            }
        }
    }
}

/// Diff3 intentionally groups adjacent edits into one conflict hunk. For source review, ownership
/// can still be proven when all three snapshots retain line alignment: only lines unchanged from
/// `after` are reverted, lines the target feature never changed are preserved, and any same-line
/// competing edit remains a hard conflict.
fn aligned_line_inverse_merge(after: &[u8], current: &[u8], before: &[u8]) -> Option<Vec<u8>> {
    fn lines(bytes: &[u8]) -> Vec<&[u8]> {
        bytes.split_inclusive(|byte| *byte == b'\n').collect()
    }

    let after = lines(after);
    let current = lines(current);
    let before = lines(before);
    if after.len() != current.len() || after.len() != before.len() {
        return None;
    }
    let mut merged = Vec::new();
    for ((after, current), before) in after.into_iter().zip(current).zip(before) {
        if current == after {
            merged.extend_from_slice(before);
        } else if before == after || current == before {
            merged.extend_from_slice(current);
        } else {
            return None;
        }
    }
    Some(merged)
}

fn event_patch(before: &Snapshot, after: &Snapshot) -> (Option<String>, bool) {
    let before = before.bytes.as_deref().unwrap_or_default();
    let after = after.bytes.as_deref().unwrap_or_default();
    match (std::str::from_utf8(before), std::str::from_utf8(after)) {
        (Ok(before), Ok(after)) => (Some(diffy::create_patch(before, after).to_string()), false),
        _ => (None, true),
    }
}

fn load_events(
    conn: &rusqlite::Connection,
    plan_id: &str,
    revision: u64,
    item_id: &str,
    path: &str,
) -> Result<Vec<PlanChangeEvent>, ProductError> {
    let mut statement = conn
        .prepare(&format!(
            "SELECT {EVENT_COLUMNS} FROM plan_change_events
             WHERE plan_id = ?1 AND plan_revision = ?2 AND item_id = ?3 AND path = ?4
               AND state = 'captured' ORDER BY sequence"
        ))
        .map_err(db_error)?;
    let events = statement
        .query_map(
            params![plan_id, to_i64(revision, "plan revision")?, item_id, path],
            map_event,
        )
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    Ok(events)
}

fn map_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlanChangeEvent> {
    let state: String = row.get(13)?;
    let state = PlanChangeEventState::try_from_str(&state).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            13,
            rusqlite::types::Type::Text,
            format!("invalid Plan change event state {state}").into(),
        )
    })?;
    Ok(PlanChangeEvent {
        sequence: row.get(0)?,
        id: row.get(1)?,
        plan_id: row.get(2)?,
        plan_revision: sql_u64(row.get(3)?, 3)?,
        item_id: row.get(4)?,
        task_id: row.get(5)?,
        run_id: row.get(6)?,
        tool_call_id: row.get(7)?,
        path: row.get(8)?,
        before_blob_hash: row.get(9)?,
        before_exists: row.get::<_, i64>(10)? != 0,
        after_blob_hash: row.get(11)?,
        after_exists: row.get::<_, Option<i64>>(12)?.map(|value| value != 0),
        state,
        error: row.get(14)?,
        created_at: sql_date(row.get(15)?, 15)?,
        finalized_at: row
            .get::<_, Option<String>>(16)?
            .map(|value| sql_date(value, 16))
            .transpose()?,
    })
}

fn load_decisions(
    conn: &rusqlite::Connection,
    plan_id: &str,
    revision: u64,
) -> Result<HashMap<(String, Option<String>), PlanReviewDecisionKind>, ProductError> {
    let mut statement = conn
        .prepare(
            "SELECT item_id, path, decision FROM plan_review_decisions
             WHERE plan_id = ?1 AND plan_revision = ?2",
        )
        .map_err(db_error)?;
    let entries = statement
        .query_map(
            params![plan_id, to_i64(revision, "plan revision")?],
            |row| {
                let item_id: String = row.get(0)?;
                let path: Option<String> = row.get(1)?;
                let decision: String = row.get(2)?;
                let decision =
                    PlanReviewDecisionKind::try_from_str(&decision).ok_or_else(|| {
                        rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Text,
                            format!("invalid Plan review decision {decision}").into(),
                        )
                    })?;
                Ok(((item_id, path), decision))
            },
        )
        .map_err(db_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_error)?;
    Ok(entries.into_iter().collect())
}

fn map_decision(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlanReviewDecision> {
    let scope: String = row.get(4)?;
    let decision: String = row.get(6)?;
    Ok(PlanReviewDecision {
        id: row.get(0)?,
        plan_id: row.get(1)?,
        plan_revision: sql_u64(row.get(2)?, 2)?,
        item_id: row.get(3)?,
        scope: PlanReviewScope::try_from_str(&scope).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                4,
                rusqlite::types::Type::Text,
                format!("invalid Plan review scope {scope}").into(),
            )
        })?,
        path: row.get(5)?,
        decision: PlanReviewDecisionKind::try_from_str(&decision).ok_or_else(|| {
            rusqlite::Error::FromSqlConversionFailure(
                6,
                rusqlite::types::Type::Text,
                format!("invalid Plan review decision {decision}").into(),
            )
        })?,
        decided_at: sql_date(row.get(7)?, 7)?,
    })
}

fn decision_from_target(
    target: &EnhancedReviewTarget,
    scope: PlanReviewScope,
    kind: PlanReviewDecisionKind,
) -> PlanReviewDecision {
    PlanReviewDecision {
        id: Uuid::new_v4().to_string(),
        plan_id: target.plan_id.clone(),
        plan_revision: target.plan_revision,
        item_id: target.item_id.clone(),
        scope,
        path: target.path.clone(),
        decision: kind,
        decided_at: Utc::now(),
    }
}

fn insert_decision(
    tx: &Transaction<'_>,
    decision: &PlanReviewDecision,
) -> Result<(), ProductError> {
    tx.execute(
        "INSERT INTO plan_review_decisions (
             id, plan_id, plan_revision, item_id, scope, path, decision, decided_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            decision.id,
            decision.plan_id,
            to_i64(decision.plan_revision, "plan revision")?,
            decision.item_id,
            decision.scope.as_str(),
            decision.path,
            decision.decision.as_str(),
            decision.decided_at.to_rfc3339(),
        ],
    )
    .map_err(db_error)?;
    Ok(())
}

fn increment_optional_ref(
    tx: &Transaction<'_>,
    hash: Option<&str>,
    now: &str,
) -> Result<(), ProductError> {
    let Some(hash) = hash else {
        return Ok(());
    };
    tx.execute(
        "INSERT INTO blobs (hash, ref_count, created_at) VALUES (?1, 1, ?2)
         ON CONFLICT(hash) DO UPDATE SET ref_count = ref_count + 1",
        params![hash, now],
    )
    .map_err(db_error)?;
    Ok(())
}

fn product_error_to_io(error: ProductError) -> io::Error {
    let kind = match &error {
        ProductError::PathNotFound(_) => ErrorKind::NotFound,
        ProductError::PathEscape(_) | ProductError::PermissionError(_) => {
            ErrorKind::PermissionDenied
        }
        _ => ErrorKind::Other,
    };
    io::Error::new(kind, error.to_string())
}

fn portable_relative_path(path: &Path) -> Result<String, ProductError> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => {
                let part = part.to_str().ok_or_else(|| {
                    ProductError::PathEscape(format!("non-UTF-8 workspace path: {path:?}"))
                })?;
                if part.is_empty() {
                    return Err(ProductError::PathEscape(format!(
                        "empty workspace path component: {path:?}"
                    )));
                }
                parts.push(part);
            }
            _ => {
                return Err(ProductError::PathEscape(format!(
                    "non-relative workspace path component: {path:?}"
                )));
            }
        }
    }
    if parts.is_empty() {
        return Err(ProductError::PathEscape(
            "empty workspace-relative path".into(),
        ));
    }
    Ok(parts.join("/"))
}

fn path_sort_key(path: &Path) -> String {
    let key = path.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    {
        key.to_lowercase()
    }
    #[cfg(not(windows))]
    {
        key
    }
}

fn bool_i64(value: bool) -> i64 {
    i64::from(value)
}

fn to_i64(value: u64, label: &str) -> Result<i64, ProductError> {
    i64::try_from(value).map_err(|_| invalid(format!("{label} is too large")))
}

fn i64_to_u64(value: i64, label: &str) -> Result<u64, ProductError> {
    u64::try_from(value).map_err(|_| ProductError::DatabaseError(format!("invalid {label}")))
}

fn i64_to_u32(value: i64, label: &str) -> Result<u32, ProductError> {
    u32::try_from(value).map_err(|_| ProductError::DatabaseError(format!("invalid {label}")))
}

fn sql_u64(value: i64, column: usize) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

fn sql_date(value: String, column: usize) -> rusqlite::Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(&value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                column,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })
}

fn invalid(message: impl Into<String>) -> ProductError {
    ProductError::StateMachineError(message.into())
}

fn db_error(error: rusqlite::Error) -> ProductError {
    let message = error.to_string();
    if message.contains(PLAN_REVIEW_SCOPE_CONFLICT) {
        return invalid(format!(
            "{PLAN_REVIEW_SCOPE_CONFLICT}: another incompatible review action is already final or in progress"
        ));
    }
    ProductError::DatabaseError(message)
}

fn io_error(error: io::Error) -> ProductError {
    ProductError::RollbackError(error.to_string())
}
