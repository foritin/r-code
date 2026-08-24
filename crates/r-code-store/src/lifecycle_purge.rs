//! Transactional cleanup for task/workspace-owned AppData.
//!
//! SQLite remains authoritative: every blob reference owned by the rows being removed is
//! counted before the cascade, decremented in the same transaction, and only zero-reference
//! files are removed after commit.  Plan projection paths stored in SQLite are deliberately
//! ignored; cleanup derives a validated UUID child from the trusted projection root.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use r_code_core::error::ProductError;
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use uuid::Uuid;

use crate::Database;

pub const PURGE_REJECT_IN_PROGRESS: &str = "审核回滚处理中，请稍后";

fn db_error(error: rusqlite::Error) -> ProductError {
    ProductError::DatabaseError(error.to_string())
}

/// Durable rows removed in one lifecycle transaction and the AppData cleanup attempted after it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LifecyclePurgeResult {
    pub workspace_removed: bool,
    pub removed_tasks: usize,
    pub released_blob_references: u64,
    pub unreferenced_blob_hashes: Vec<String>,
    pub removed_plan_ids: Vec<String>,
    /// A committed database deletion is never reported as failed solely because an AppData file
    /// was temporarily locked. Startup pruning retries these paths idempotently.
    pub cleanup_warnings: Vec<String>,
}

/// Result of a conservative AppData prune pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppDataPruneReport {
    pub removed: usize,
    pub warnings: Vec<String>,
}

/// Unified task/workspace deletion service.
pub struct LifecyclePurgeStore<'a> {
    db: &'a Database,
    blobs_dir: PathBuf,
    projection_root: PathBuf,
}

impl<'a> LifecyclePurgeStore<'a> {
    pub fn new(
        db: &'a Database,
        blobs_dir: impl Into<PathBuf>,
        projection_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            db,
            blobs_dir: blobs_dir.into(),
            projection_root: projection_root.into(),
        }
    }

    /// Delete one task and all of its SQLite-owned audit data.
    pub fn purge_task(&self, task_id: &str) -> Result<LifecyclePurgeResult, ProductError> {
        ensure_no_active_rejection_for_task(self.db, task_id)?;
        let connection = self.db.conn()?;
        // Take the SQLite writer reservation before the authoritative re-check. A rejection can
        // neither slip a prepared journal between this check and the task cascade nor wait on a
        // path lease while this transaction is held.
        let transaction = Transaction::new_unchecked(&connection, TransactionBehavior::Immediate)
            .map_err(db_error)?;
        ensure_no_active_rejection_for_task_tx(&transaction, task_id)?;
        let exists = transaction
            .query_row(
                "SELECT 1 FROM tasks WHERE id = ?1",
                params![task_id],
                |_| Ok(()),
            )
            .optional()
            .map_err(db_error)?
            .is_some();
        if !exists {
            transaction.rollback().map_err(db_error)?;
            return Ok(LifecyclePurgeResult::default());
        }

        let references = collect_task_blob_references(&transaction, task_id)?;
        let plan_ids = collect_task_plan_ids(&transaction, task_id)?;
        // notifications.task_id is ON DELETE SET NULL. Remove the task-scoped notification first
        // so permanent task deletion does not leave an unowned product record behind.
        transaction
            .execute(
                "DELETE FROM notifications WHERE task_id = ?1",
                params![task_id],
            )
            .map_err(db_error)?;
        let removed = transaction
            .execute("DELETE FROM tasks WHERE id = ?1", params![task_id])
            .map_err(db_error)?;
        let (released, unreferenced) = decrement_blob_references(&transaction, &references)?;
        transaction.commit().map_err(db_error)?;

        let mut result = LifecyclePurgeResult {
            removed_tasks: removed,
            released_blob_references: released,
            unreferenced_blob_hashes: unreferenced,
            removed_plan_ids: plan_ids,
            ..LifecyclePurgeResult::default()
        };
        self.cleanup_committed_app_data(&mut result);
        Ok(result)
    }

    /// Delete a workspace owner row and every task bound to its canonical path.
    pub fn purge_workspace(
        &self,
        canonical_path: &str,
    ) -> Result<LifecyclePurgeResult, ProductError> {
        ensure_no_active_rejection_for_workspace(self.db, canonical_path)?;
        let connection = self.db.conn()?;
        let transaction = Transaction::new_unchecked(&connection, TransactionBehavior::Immediate)
            .map_err(db_error)?;
        ensure_no_active_rejection_for_workspace_tx(&transaction, canonical_path)?;
        let workspace_exists = transaction
            .query_row(
                "SELECT 1 FROM workspaces WHERE canonical_path = ?1",
                params![canonical_path],
                |_| Ok(()),
            )
            .optional()
            .map_err(db_error)?
            .is_some();
        let task_ids = collect_workspace_task_ids(&transaction, canonical_path)?;
        if !workspace_exists && task_ids.is_empty() {
            transaction.rollback().map_err(db_error)?;
            return Ok(LifecyclePurgeResult::default());
        }

        let mut references = HashMap::new();
        let mut plan_ids = Vec::new();
        for task_id in &task_ids {
            merge_reference_counts(
                &mut references,
                collect_task_blob_references(&transaction, task_id)?,
            )?;
            plan_ids.extend(collect_task_plan_ids(&transaction, task_id)?);
        }
        plan_ids.sort();
        plan_ids.dedup();

        transaction
            .execute(
                "DELETE FROM notifications WHERE workspace_path = ?1 \
                 OR task_id IN (SELECT id FROM tasks WHERE workspace_path = ?1)",
                params![canonical_path],
            )
            .map_err(db_error)?;
        let removed_tasks = transaction
            .execute(
                "DELETE FROM tasks WHERE workspace_path = ?1",
                params![canonical_path],
            )
            .map_err(db_error)?;
        let workspace_removed = transaction
            .execute(
                "DELETE FROM workspaces WHERE canonical_path = ?1",
                params![canonical_path],
            )
            .map_err(db_error)?
            > 0;
        let (released, unreferenced) = decrement_blob_references(&transaction, &references)?;
        transaction.commit().map_err(db_error)?;

        let mut result = LifecyclePurgeResult {
            workspace_removed,
            removed_tasks,
            released_blob_references: released,
            unreferenced_blob_hashes: unreferenced,
            removed_plan_ids: plan_ids,
            ..LifecyclePurgeResult::default()
        };
        self.cleanup_committed_app_data(&mut result);
        Ok(result)
    }

    fn cleanup_committed_app_data(&self, result: &mut LifecyclePurgeResult) {
        for hash in &result.unreferenced_blob_hashes {
            if let Err(error) = remove_blob_file(&self.blobs_dir, hash) {
                result.cleanup_warnings.push(format!(
                    "failed to remove unreferenced blob {hash}: {error}"
                ));
            }
        }
        for plan_id in &result.removed_plan_ids {
            if let Err(error) = remove_plan_projection(&self.projection_root, plan_id) {
                result.cleanup_warnings.push(format!(
                    "failed to remove Plan projection {plan_id}: {error}"
                ));
            }
        }
    }
}

fn ensure_no_active_rejection_for_task(db: &Database, task_id: &str) -> Result<(), ProductError> {
    let connection = db.conn()?;
    ensure_no_active_rejection_for_task_conn(&connection, task_id)
}

fn ensure_no_active_rejection_for_workspace(
    db: &Database,
    canonical_path: &str,
) -> Result<(), ProductError> {
    let connection = db.conn()?;
    let active: bool = connection
        .query_row(
            "SELECT EXISTS(\
                 SELECT 1 FROM plan_reject_operations operation \
                 JOIN plans plan ON plan.id = operation.plan_id \
                 JOIN tasks task ON task.id = plan.task_id \
                 WHERE task.workspace_path = ?1 \
                   AND operation.state IN ('prepared', 'applying', 'rolling_back')\
             )",
            params![canonical_path],
            |row| row.get(0),
        )
        .map_err(db_error)?;
    if active {
        return Err(ProductError::StateMachineError(
            PURGE_REJECT_IN_PROGRESS.to_string(),
        ));
    }
    Ok(())
}

fn ensure_no_active_rejection_for_task_tx(
    transaction: &Transaction<'_>,
    task_id: &str,
) -> Result<(), ProductError> {
    ensure_no_active_rejection_for_task_conn(transaction, task_id)
}

fn ensure_no_active_rejection_for_workspace_tx(
    transaction: &Transaction<'_>,
    canonical_path: &str,
) -> Result<(), ProductError> {
    let active: bool = transaction
        .query_row(
            "SELECT EXISTS(\
                 SELECT 1 FROM plan_reject_operations operation \
                 JOIN plans plan ON plan.id = operation.plan_id \
                 JOIN tasks task ON task.id = plan.task_id \
                 WHERE task.workspace_path = ?1 \
                   AND operation.state IN ('prepared', 'applying', 'rolling_back')\
             )",
            params![canonical_path],
            |row| row.get(0),
        )
        .map_err(db_error)?;
    if active {
        return Err(ProductError::StateMachineError(
            PURGE_REJECT_IN_PROGRESS.to_string(),
        ));
    }
    Ok(())
}

fn ensure_no_active_rejection_for_task_conn(
    connection: &rusqlite::Connection,
    task_id: &str,
) -> Result<(), ProductError> {
    let active: bool = connection
        .query_row(
            "SELECT EXISTS(\
                 SELECT 1 FROM plan_reject_operations operation \
                 JOIN plans plan ON plan.id = operation.plan_id \
                 WHERE plan.task_id = ?1 \
                   AND operation.state IN ('prepared', 'applying', 'rolling_back')\
             )",
            params![task_id],
            |row| row.get(0),
        )
        .map_err(db_error)?;
    if active {
        return Err(ProductError::StateMachineError(
            PURGE_REJECT_IN_PROGRESS.to_string(),
        ));
    }
    Ok(())
}

fn collect_workspace_task_ids(
    transaction: &Transaction<'_>,
    canonical_path: &str,
) -> Result<Vec<String>, ProductError> {
    let mut statement = transaction
        .prepare("SELECT id FROM tasks WHERE workspace_path = ?1 ORDER BY id")
        .map_err(db_error)?;
    let rows = statement
        .query_map(params![canonical_path], |row| row.get(0))
        .map_err(db_error)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
}

fn collect_task_plan_ids(
    transaction: &Transaction<'_>,
    task_id: &str,
) -> Result<Vec<String>, ProductError> {
    let mut statement = transaction
        .prepare("SELECT id FROM plans WHERE task_id = ?1 ORDER BY id")
        .map_err(db_error)?;
    let rows = statement
        .query_map(params![task_id], |row| row.get(0))
        .map_err(db_error)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
}

fn collect_task_blob_references(
    transaction: &Transaction<'_>,
    task_id: &str,
) -> Result<HashMap<String, i64>, ProductError> {
    let mut references = HashMap::new();
    // Each column is queried separately. This is intentional: when before/after (or three reject
    // journal columns) contain the same hash, each column had its own increment and must release
    // its own reference.
    for query in [
        "SELECT before_hash, COUNT(*) FROM file_changes \
         WHERE task_id = ?1 AND before_hash IS NOT NULL GROUP BY before_hash",
        "SELECT after_hash, COUNT(*) FROM file_changes \
         WHERE task_id = ?1 AND after_hash IS NOT NULL GROUP BY after_hash",
        // content_hash is the digest while blob_key is the single independently-counted baseline
        // blob reference. Current writers store the same value in both columns but increment once.
        "SELECT blob_key, COUNT(*) FROM file_baselines \
         WHERE task_id = ?1 GROUP BY blob_key",
        "SELECT output_blob_key, COUNT(*) FROM verifications \
         WHERE task_id = ?1 AND output_blob_key IS NOT NULL GROUP BY output_blob_key",
        "SELECT before_blob_hash, COUNT(*) FROM plan_change_events \
         WHERE task_id = ?1 AND before_blob_hash IS NOT NULL GROUP BY before_blob_hash",
        "SELECT after_blob_hash, COUNT(*) FROM plan_change_events \
         WHERE task_id = ?1 AND after_blob_hash IS NOT NULL GROUP BY after_blob_hash",
        // docs/archive/implementation/multimodal-attachments-and-deepseek-plan-anchoring-implementation.md §7.5：任务删除按 attachments 逻辑引用逐个
        // 释放（同一内容多行 = 多次递减）；物理 Blob 仍被其他任务引用时保留。
        "SELECT blob_hash, COUNT(*) FROM attachments WHERE task_id = ?1 GROUP BY blob_hash",
    ] {
        add_grouped_references(transaction, query, task_id, &mut references)?;
    }
    for query in [
        "SELECT file.expected_current_hash, COUNT(*) \
         FROM plan_reject_operation_files file \
         JOIN plan_reject_operations operation ON operation.id = file.operation_id \
         JOIN plans plan ON plan.id = operation.plan_id \
         WHERE plan.task_id = ?1 AND file.expected_current_hash IS NOT NULL \
         GROUP BY file.expected_current_hash",
        "SELECT file.rollback_hash, COUNT(*) \
         FROM plan_reject_operation_files file \
         JOIN plan_reject_operations operation ON operation.id = file.operation_id \
         JOIN plans plan ON plan.id = operation.plan_id \
         WHERE plan.task_id = ?1 AND file.rollback_hash IS NOT NULL \
         GROUP BY file.rollback_hash",
        "SELECT file.desired_hash, COUNT(*) \
         FROM plan_reject_operation_files file \
         JOIN plan_reject_operations operation ON operation.id = file.operation_id \
         JOIN plans plan ON plan.id = operation.plan_id \
         WHERE plan.task_id = ?1 AND file.desired_hash IS NOT NULL \
         GROUP BY file.desired_hash",
    ] {
        add_grouped_references(transaction, query, task_id, &mut references)?;
    }

    // review_files copies hashes already owned by file_changes/run snapshots. Materialization does
    // not call increment_ref, so counting those columns here would over-decrement shared blobs.
    Ok(references)
}

fn add_grouped_references(
    transaction: &Transaction<'_>,
    query: &str,
    task_id: &str,
    references: &mut HashMap<String, i64>,
) -> Result<(), ProductError> {
    let mut statement = transaction.prepare(query).map_err(db_error)?;
    let mut rows = statement.query(params![task_id]).map_err(db_error)?;
    while let Some(row) = rows.next().map_err(db_error)? {
        let hash: String = row.get(0).map_err(db_error)?;
        let count: i64 = row.get(1).map_err(db_error)?;
        if count <= 0 {
            return Err(ProductError::DatabaseError(format!(
                "invalid blob reference count {count} for {hash}"
            )));
        }
        let entry = references.entry(hash).or_default();
        *entry = entry.checked_add(count).ok_or_else(|| {
            ProductError::DatabaseError("blob reference count overflow".to_string())
        })?;
    }
    Ok(())
}

fn merge_reference_counts(
    destination: &mut HashMap<String, i64>,
    source: HashMap<String, i64>,
) -> Result<(), ProductError> {
    for (hash, count) in source {
        let entry = destination.entry(hash).or_default();
        *entry = entry.checked_add(count).ok_or_else(|| {
            ProductError::DatabaseError("blob reference count overflow".to_string())
        })?;
    }
    Ok(())
}

fn decrement_blob_references(
    transaction: &Transaction<'_>,
    references: &HashMap<String, i64>,
) -> Result<(u64, Vec<String>), ProductError> {
    let mut released = 0_u64;
    let mut unreferenced = Vec::new();
    let mut ordered = references.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|(hash, _)| *hash);
    for (hash, decrement) in ordered {
        let current: Option<i64> = transaction
            .query_row(
                "SELECT ref_count FROM blobs WHERE hash = ?1",
                params![hash],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_error)?;
        let current = current.ok_or_else(|| {
            ProductError::DatabaseError(format!(
                "blob reference ledger is missing hash {hash}; deletion aborted"
            ))
        })?;
        if current < *decrement {
            return Err(ProductError::DatabaseError(format!(
                "blob reference ledger underflow for {hash}: stored {current}, releasing {decrement}"
            )));
        }
        if current == *decrement {
            transaction
                .execute("DELETE FROM blobs WHERE hash = ?1", params![hash])
                .map_err(db_error)?;
            unreferenced.push((*hash).clone());
        } else {
            transaction
                .execute(
                    "UPDATE blobs SET ref_count = ref_count - ?1 WHERE hash = ?2",
                    params![decrement, hash],
                )
                .map_err(db_error)?;
        }
        released = released
            .checked_add(u64::try_from(*decrement).map_err(|_| {
                ProductError::DatabaseError("negative blob reference count".to_string())
            })?)
            .ok_or_else(|| ProductError::DatabaseError("blob release count overflow".into()))?;
    }
    Ok((released, unreferenced))
}

pub(crate) fn is_safe_blob_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn remove_blob_file(root: &Path, hash: &str) -> Result<(), std::io::Error> {
    if !is_safe_blob_hash(hash) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "blob hash is not 64 lowercase hexadecimal characters",
        ));
    }
    let path = root.join(hash);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() || metadata.file_type().is_symlink() => {
            remove_file_or_symlink(&path, &metadata)
        }
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "blob path is not a file",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

pub(crate) fn is_safe_plan_id(value: &str) -> bool {
    Uuid::parse_str(value)
        .map(|parsed| parsed.to_string() == value.to_ascii_lowercase())
        .unwrap_or(false)
}

fn remove_plan_projection(root: &Path, plan_id: &str) -> Result<(), std::io::Error> {
    if !is_safe_plan_id(plan_id) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Plan id is not a canonical UUID",
        ));
    }
    remove_tree_without_following_links(&root.join(plan_id))
}

fn remove_tree_without_following_links(path: &Path) -> Result<(), std::io::Error> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() || metadata.is_file() {
        return remove_file_or_symlink(path, &metadata);
    }
    if !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "projection path is not a directory",
        ));
    }
    for entry in fs::read_dir(path)? {
        remove_tree_without_following_links(&entry?.path())?;
    }
    fs::remove_dir(path)
}

fn remove_file_or_symlink(path: &Path, metadata: &fs::Metadata) -> Result<(), std::io::Error> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(first) if metadata.file_type().is_symlink() => fs::remove_dir(path).map_err(|_| first),
        Err(error) => Err(error),
    }
}

pub(crate) fn prune_orphan_plan_directories(
    db: &Database,
    projection_root: &Path,
) -> Result<AppDataPruneReport, ProductError> {
    let mut report = AppDataPruneReport::default();
    let entries = match fs::read_dir(projection_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(error) => return Err(ProductError::Other(error.to_string())),
    };
    let connection = db.conn()?;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                report.warnings.push(error.to_string());
                continue;
            }
        };
        let Some(plan_id) = entry.file_name().to_str().map(ToOwned::to_owned) else {
            continue;
        };
        if !is_safe_plan_id(&plan_id) {
            continue;
        }
        let exists = connection
            .query_row(
                "SELECT 1 FROM plans WHERE id = ?1",
                params![plan_id],
                |_| Ok(()),
            )
            .optional()
            .map_err(db_error)?
            .is_some();
        if exists {
            continue;
        }
        match remove_plan_projection(projection_root, &plan_id) {
            Ok(()) => report.removed += 1,
            Err(error) => report.warnings.push(format!(
                "failed to prune Plan projection {plan_id}: {error}"
            )),
        }
    }
    Ok(report)
}

pub(crate) fn prune_unreferenced_blob_files(
    db: &Database,
    blobs_dir: &Path,
) -> Result<AppDataPruneReport, ProductError> {
    let mut report = AppDataPruneReport::default();
    let entries = match fs::read_dir(blobs_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(error) => return Err(ProductError::Other(error.to_string())),
    };
    let connection = db.conn()?;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                report.warnings.push(error.to_string());
                continue;
            }
        };
        let Some(hash) = entry.file_name().to_str().map(ToOwned::to_owned) else {
            continue;
        };
        if !is_safe_blob_hash(&hash) {
            continue;
        }
        let metadata = match fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) => {
                report
                    .warnings
                    .push(format!("failed to inspect blob {hash}: {error}"));
                continue;
            }
        };
        if !metadata.file_type().is_file() && !metadata.file_type().is_symlink() {
            continue;
        }
        let exists = connection
            .query_row("SELECT 1 FROM blobs WHERE hash = ?1", params![hash], |_| {
                Ok(())
            })
            .optional()
            .map_err(db_error)?
            .is_some();
        if exists {
            continue;
        }
        match remove_blob_file(blobs_dir, &hash) {
            Ok(()) => report.removed += 1,
            Err(error) => report
                .warnings
                .push(format!("failed to prune blob {hash}: {error}")),
        }
    }
    Ok(report)
}
