//! Persistent review ledger plus explicitly separated Git delivery.
//!
//! Keeping a change is an application decision, not a Git staging operation. Review decisions
//! are therefore stored in SQLite and are cheap, idempotent, and safe to execute concurrently.
//! Git is only touched by the delivery methods at the bottom of this module.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};

use chrono::Utc;
use r_code_core::error::ProductError;
use r_code_core::security::PathGuard;
use rusqlite::{params, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::patch_engine::hash_content;
use crate::{BlobStore, Database, GitService};

const MAX_REVIEW_DIFF_LINES: usize = 800;
const SYNTHETIC_ITEM_PREFIX: &str = "file:";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewPathStatus {
    pub path: String,
    pub accepted: bool,
    pub rejected: bool,
    pub remaining: bool,
    pub conflict: bool,
    pub safe_to_accept: bool,
    pub blocker: Option<String>,
    pub accepted_items: usize,
    pub rejected_items: usize,
    pub remaining_items: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewStatus {
    /// Git availability is informative only. It never controls whether review decisions work.
    pub git_repository: bool,
    pub repo_root: Option<String>,
    pub paths: Vec<ReviewPathStatus>,
    pub accepted_count: usize,
    pub rejected_count: usize,
    pub remaining_count: usize,
    pub conflict_count: usize,
    pub can_accept_all: bool,
}

/// Compatibility alias for older command names. The payload is no longer Git-index state.
pub type ReviewGitStatus = ReviewStatus;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewAcceptResult {
    pub path: Option<String>,
    pub accepted_count: usize,
    pub rejected_count: usize,
    pub remaining_count: usize,
    pub fully_accepted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitDeliveryStatus {
    pub branch: Option<String>,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub staged_task_paths: Vec<String>,
    pub staged_other_paths: Vec<String>,
    pub can_stage: bool,
    pub can_commit: bool,
    pub can_push: bool,
    pub blockers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitCommitResult {
    pub sha: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GitPushResult {
    pub sha: String,
    pub branch: String,
    pub upstream: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewDiffLine {
    pub line_id: String,
    pub kind: ReviewDiffLineKind,
    pub text: String,
    pub old_no: Option<usize>,
    pub new_no: Option<usize>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDiffLineKind {
    Add,
    Del,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Pending,
    Accepted,
    Rejected,
}

impl ReviewDecision {
    fn parse(value: &str) -> Result<Self, ProductError> {
        match value {
            "pending" => Ok(Self::Pending),
            "accepted" => Ok(Self::Accepted),
            "rejected" => Ok(Self::Rejected),
            other => Err(ProductError::DatabaseError(format!(
                "invalid review decision: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewFileSnapshot {
    pub path: String,
    pub before_hash: Option<String>,
    pub after_hash: Option<String>,
}

#[derive(Debug, Clone)]
struct SessionContext {
    id: String,
    task_id: String,
    run_id: String,
    workspace_root: Option<PathBuf>,
    repo_root: Option<PathBuf>,
    materialized: bool,
    run_finished: bool,
    run_started_at: String,
}

#[derive(Debug, Clone)]
struct CandidateChange {
    path: String,
    before_hash: Option<String>,
    after_hash: Option<String>,
}

pub struct ReviewLedgerService<'a> {
    db: &'a Database,
    blobs_dir: PathBuf,
}

/// Compatibility alias retained while callers migrate from the old Git-coupled implementation.
pub type ReviewGitService<'a> = ReviewLedgerService<'a>;

impl<'a> ReviewLedgerService<'a> {
    pub fn new(db: &'a Database, blobs_dir: PathBuf) -> Self {
        Self { db, blobs_dir }
    }

    pub fn status(&self, task_id: &str) -> Result<ReviewStatus, ProductError> {
        let Some(context) = self.ensure_session(task_id)? else {
            let (git_repository, repo_root) = self.task_git_context(task_id)?;
            return Ok(empty_status(git_repository, repo_root));
        };
        self.read_status(&context)
    }

    pub fn file_snapshot(
        &self,
        task_id: &str,
        display_path: &str,
    ) -> Result<Option<ReviewFileSnapshot>, ProductError> {
        let Some(context) = self.ensure_session(task_id)? else {
            return Ok(None);
        };
        let conn = self.db.conn()?;
        conn.query_row(
            "SELECT path, before_hash, after_hash FROM review_files \
             WHERE session_id = ?1 AND path = ?2",
            params![context.id, display_path],
            |row| {
                Ok(ReviewFileSnapshot {
                    path: row.get(0)?,
                    before_hash: row.get(1)?,
                    after_hash: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(db_err)
    }

    pub fn line_decisions(
        &self,
        task_id: &str,
        display_path: &str,
    ) -> Result<BTreeMap<String, ReviewDecision>, ProductError> {
        let Some(context) = self.ensure_session(task_id)? else {
            return Ok(BTreeMap::new());
        };
        let conn = self.db.conn()?;
        let mut statement = conn
            .prepare(
                "SELECT ri.item_id, ri.state FROM review_items ri \
                 JOIN review_files rf ON rf.id = ri.review_file_id \
                 WHERE rf.session_id = ?1 AND rf.path = ?2 \
                   AND ri.item_id NOT LIKE 'file:%' ORDER BY ri.ordinal",
            )
            .map_err(db_err)?;
        let rows = statement
            .query_map(params![context.id, display_path], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(db_err)?;
        let mut decisions = BTreeMap::new();
        for row in rows {
            let (item_id, state) = row.map_err(db_err)?;
            decisions.insert(item_id, ReviewDecision::parse(&state)?);
        }
        Ok(decisions)
    }

    pub fn accept_line(
        &self,
        task_id: &str,
        display_path: &str,
        line_id: &str,
    ) -> Result<ReviewAcceptResult, ProductError> {
        let context = self.require_finished_session(task_id)?;
        let mut conn = self.db.conn()?;
        let tx = conn.transaction().map_err(db_err)?;
        let file_id = review_file_id(&tx, &context.id, display_path)?;
        let changed = tx
            .execute(
                "UPDATE review_items SET state = 'accepted', decided_at = ?1 \
                 WHERE review_file_id = ?2 AND item_id = ?3 AND state = 'pending'",
                params![Utc::now().to_rfc3339(), file_id, line_id],
            )
            .map_err(db_err)?;
        if changed == 0 {
            let existing: Option<String> = tx
                .query_row(
                    "SELECT state FROM review_items WHERE review_file_id = ?1 AND item_id = ?2",
                    params![file_id, line_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(db_err)?;
            if existing.is_none() {
                return Err(ProductError::DatabaseError(
                    "该变更点已更新，请刷新审核内容后重试".into(),
                ));
            }
        }
        recompute_file(&tx, &file_id)?;
        recompute_session(&tx, &context.id)?;
        tx.commit().map_err(db_err)?;
        drop(conn);
        self.accept_result(task_id, Some(display_path.to_string()))
    }

    pub fn accept_file(
        &self,
        task_id: &str,
        display_path: &str,
    ) -> Result<ReviewAcceptResult, ProductError> {
        let context = self.require_finished_session(task_id)?;
        let mut conn = self.db.conn()?;
        let tx = conn.transaction().map_err(db_err)?;
        let file_id = review_file_id(&tx, &context.id, display_path)?;
        tx.execute(
            "UPDATE review_items SET state = 'accepted', decided_at = ?1 \
             WHERE review_file_id = ?2 AND state = 'pending'",
            params![Utc::now().to_rfc3339(), file_id],
        )
        .map_err(db_err)?;
        recompute_file(&tx, &file_id)?;
        recompute_session(&tx, &context.id)?;
        tx.commit().map_err(db_err)?;
        drop(conn);
        self.accept_result(task_id, Some(display_path.to_string()))
    }

    pub fn accept_all(&self, task_id: &str) -> Result<ReviewAcceptResult, ProductError> {
        let context = self.require_finished_session(task_id)?;
        let mut conn = self.db.conn()?;
        let tx = conn.transaction().map_err(db_err)?;
        let now = Utc::now().to_rfc3339();
        tx.execute(
            "UPDATE review_items SET state = 'accepted', decided_at = ?1 \
             WHERE state = 'pending' AND review_file_id IN \
               (SELECT id FROM review_files WHERE session_id = ?2 AND state != 'conflict')",
            params![now, context.id],
        )
        .map_err(db_err)?;
        recompute_all_files(&tx, &context.id)?;
        recompute_session(&tx, &context.id)?;
        tx.commit().map_err(db_err)?;
        drop(conn);
        self.accept_result(task_id, None)
    }

    /// Record the result after the caller has safely restored the file on disk.
    pub fn reject_file(
        &self,
        task_id: &str,
        display_path: &str,
    ) -> Result<ReviewAcceptResult, ProductError> {
        let context = self.require_finished_session(task_id)?;
        let mut conn = self.db.conn()?;
        let tx = conn.transaction().map_err(db_err)?;
        let file_id = review_file_id(&tx, &context.id, display_path)?;
        let now = Utc::now().to_rfc3339();
        tx.execute(
            "UPDATE review_items SET state = 'rejected', decided_at = ?1 \
             WHERE review_file_id = ?2",
            params![now, file_id],
        )
        .map_err(db_err)?;
        tx.execute(
            "UPDATE review_files SET state = 'rejected', blocker = NULL, updated_at = ?1 \
             WHERE id = ?2",
            params![now, file_id],
        )
        .map_err(db_err)?;
        recompute_session(&tx, &context.id)?;
        tx.commit().map_err(db_err)?;
        drop(conn);
        self.accept_result(task_id, Some(display_path.to_string()))
    }

    pub fn mark_conflict(
        &self,
        task_id: &str,
        display_path: &str,
        blocker: &str,
    ) -> Result<(), ProductError> {
        let context = self.require_finished_session(task_id)?;
        let mut conn = self.db.conn()?;
        let tx = conn.transaction().map_err(db_err)?;
        tx.execute(
            "UPDATE review_files SET state = 'conflict', blocker = ?1, updated_at = ?2 \
             WHERE session_id = ?3 AND path = ?4",
            params![blocker, Utc::now().to_rfc3339(), context.id, display_path],
        )
        .map_err(db_err)?;
        recompute_session(&tx, &context.id)?;
        tx.commit().map_err(db_err)?;
        Ok(())
    }

    pub fn reject_all(&self, task_id: &str) -> Result<(), ProductError> {
        let context = self.require_finished_session(task_id)?;
        let mut conn = self.db.conn()?;
        let tx = conn.transaction().map_err(db_err)?;
        let now = Utc::now().to_rfc3339();
        tx.execute(
            "UPDATE review_items SET state = 'rejected', decided_at = ?1 \
             WHERE review_file_id IN (SELECT id FROM review_files WHERE session_id = ?2)",
            params![now, context.id],
        )
        .map_err(db_err)?;
        tx.execute(
            "UPDATE review_files SET state = 'rejected', blocker = NULL, updated_at = ?1 \
             WHERE session_id = ?2",
            params![now, context.id],
        )
        .map_err(db_err)?;
        recompute_session(&tx, &context.id)?;
        tx.commit().map_err(db_err)?;
        Ok(())
    }

    /// Explicitly stage resolved, kept review files. Review acceptance never calls this method.
    pub fn stage_accepted(&self, task_id: &str) -> Result<GitDeliveryStatus, ProductError> {
        let context = self.require_finished_session(task_id)?;
        let review = self.read_status(&context)?;
        if review.remaining_count > 0 || review.conflict_count > 0 {
            return Err(ProductError::GitError(
                "请先处理完所有审核项，再将已接受文件加入暂存区".into(),
            ));
        }
        let repo_root = context.repo_root.clone().ok_or_else(|| {
            ProductError::GitError("该任务未关联 Git 仓库，不能执行 Git 交付".into())
        })?;
        let workspace_root = context
            .workspace_root
            .as_ref()
            .ok_or_else(|| ProductError::GitError("任务工作区不可用，不能执行 Git 交付".into()))?;
        let snapshots = self.file_snapshots_in_state(&context, "accepted")?;
        let guard = PathGuard::new(workspace_root.clone())?;
        let mut conflicts = Vec::new();
        for snapshot in &snapshots {
            let requested = Path::new(&snapshot.path);
            let physical = if requested.is_absolute() {
                requested.to_path_buf()
            } else {
                workspace_root.join(requested)
            };
            let physical = guard.resolve(&physical)?;
            let actual = if physical.exists() {
                Some(hash_content(&std::fs::read(&physical)?))
            } else {
                None
            };
            if actual.as_deref() != snapshot.after_hash.as_deref() {
                let reason = format!(
                    "接受后文件内容发生变化（期望 {}，实际 {}）",
                    snapshot.after_hash.as_deref().unwrap_or("不存在"),
                    actual.as_deref().unwrap_or("不存在")
                );
                self.mark_conflict(task_id, &snapshot.path, &reason)?;
                conflicts.push(snapshot.path.clone());
            }
        }
        if !conflicts.is_empty() {
            return Err(ProductError::GitError(format!(
                "{} 个已接受文件在暂存前发生变化，请重新审核：{}",
                conflicts.len(),
                conflicts.join("、")
            )));
        }
        let paths: Vec<String> = snapshots
            .iter()
            .map(|snapshot| repo_relative_path(&repo_root, workspace_root, &snapshot.path))
            .collect::<Result<_, _>>()?;
        if paths.is_empty() {
            return Err(ProductError::GitError("没有已接受的文件可暂存".into()));
        }
        GitService::new(repo_root).stage_paths(&paths)?;
        self.delivery_status(task_id)
    }

    pub fn delivery_status(&self, task_id: &str) -> Result<GitDeliveryStatus, ProductError> {
        let context = self.require_finished_session(task_id)?;
        let repo_root = context.repo_root.clone().ok_or_else(|| {
            ProductError::GitError("该任务未关联 Git 仓库，不能执行 Git 交付".into())
        })?;
        let workspace_root = context
            .workspace_root
            .as_ref()
            .ok_or_else(|| ProductError::GitError("任务工作区不可用，不能执行 Git 交付".into()))?;
        let git = GitService::new(repo_root);
        let branch = git.current_branch().ok();
        let upstream = git.upstream()?;
        let (ahead, behind) = git.ahead_behind()?.unwrap_or((0, 0));
        let staged = git.staged_paths()?;
        let staged_set: BTreeSet<String> = staged.iter().cloned().collect();
        let review = self.read_status(&context)?;
        let repo_to_display: BTreeMap<String, String> = review
            .paths
            .iter()
            .filter(|path| path.accepted)
            .filter_map(|path| {
                repo_relative_path(context.repo_root.as_ref()?, workspace_root, &path.path)
                    .ok()
                    .map(|repo| (repo, path.path.clone()))
            })
            .collect();
        let mut staged_task_paths = Vec::new();
        let mut staged_other_paths = Vec::new();
        for path in staged {
            match repo_to_display.get(&path) {
                Some(display) => staged_task_paths.push(display.clone()),
                None => staged_other_paths.push(path),
            }
        }
        let mut blockers = Vec::new();
        if branch.is_none() {
            blockers.push("当前仓库处于 detached HEAD，不能由审核页提交或推送".into());
        }
        if review.remaining_count > 0 {
            blockers.push(format!(
                "还有 {} 个文件尚未完成审核",
                review.remaining_count
            ));
        }
        if review.conflict_count > 0 {
            blockers.push("存在需要手动处理的审核冲突".into());
        }
        if !staged_other_paths.is_empty() {
            blockers.push(format!(
                "暂存区包含 {} 个不属于本任务的路径",
                staged_other_paths.len()
            ));
        }
        let can_stage = review.remaining_count == 0
            && review.conflict_count == 0
            && repo_to_display
                .keys()
                .any(|path| !staged_set.contains(path.as_str()));
        let can_commit = !staged_task_paths.is_empty() && blockers.is_empty();
        let can_push = branch.is_some() && upstream.is_some() && ahead > 0;
        Ok(GitDeliveryStatus {
            branch,
            upstream,
            ahead,
            behind,
            staged_task_paths,
            staged_other_paths,
            can_stage,
            can_commit,
            can_push,
            blockers,
        })
    }

    pub fn suggest_commit_message(&self, task_id: &str) -> Result<String, ProductError> {
        let status = self.delivery_status(task_id)?;
        if status.staged_task_paths.is_empty() {
            return Err(ProductError::GitError("请先将已接受文件加入暂存区".into()));
        }
        let scope = dominant_scope(&status.staged_task_paths);
        let subject = if status.staged_task_paths.len() == 1 {
            readable_stem(&status.staged_task_paths[0])
        } else {
            format!("{} task files", status.staged_task_paths.len())
        };
        Ok(format!("{scope}: update {subject}"))
    }

    pub fn commit_task(
        &self,
        task_id: &str,
        message: &str,
    ) -> Result<GitCommitResult, ProductError> {
        let message = message.trim();
        if message.is_empty() || message.chars().count() > 500 || message.contains('\0') {
            return Err(ProductError::GitError(
                "提交信息不能为空且不能超过 500 个字符".into(),
            ));
        }
        let status = self.delivery_status(task_id)?;
        if !status.can_commit {
            return Err(ProductError::GitError(if status.blockers.is_empty() {
                "还没有可提交的本任务暂存内容".into()
            } else {
                status.blockers.join("；")
            }));
        }
        let context = self.require_finished_session(task_id)?;
        let sha = GitService::new(
            context
                .repo_root
                .ok_or_else(|| ProductError::GitError("该任务未关联 Git 仓库".into()))?,
        )
        .commit(message)?;
        Ok(GitCommitResult {
            sha,
            message: message.to_string(),
        })
    }

    pub fn push_task(&self, task_id: &str) -> Result<GitPushResult, ProductError> {
        let context = self.require_finished_session(task_id)?;
        let git = GitService::new(
            context
                .repo_root
                .ok_or_else(|| ProductError::GitError("该任务未关联 Git 仓库".into()))?,
        );
        let branch = git.current_branch()?;
        let upstream = git.upstream()?.ok_or_else(|| {
            ProductError::GitError("当前分支没有 upstream，审核页不会自动创建".into())
        })?;
        let status = self.delivery_status(task_id)?;
        if !status.can_push {
            return Err(ProductError::GitError("当前分支没有待推送提交".into()));
        }
        let sha = git.push_upstream()?;
        Ok(GitPushResult {
            sha,
            branch,
            upstream,
        })
    }

    fn accept_result(
        &self,
        task_id: &str,
        path: Option<String>,
    ) -> Result<ReviewAcceptResult, ProductError> {
        let status = self.status(task_id)?;
        Ok(ReviewAcceptResult {
            path,
            accepted_count: status.accepted_count,
            rejected_count: status.rejected_count,
            remaining_count: status.remaining_count,
            fully_accepted: status.remaining_count == 0 && status.conflict_count == 0,
        })
    }

    fn require_finished_session(&self, task_id: &str) -> Result<SessionContext, ProductError> {
        let context = self.ensure_session(task_id)?.ok_or_else(|| {
            ProductError::DatabaseError(format!("task has no review run: {task_id}"))
        })?;
        if !context.run_finished || !context.materialized {
            return Err(ProductError::DatabaseError(
                "任务仍在运行，结束后才能处理审核项".into(),
            ));
        }
        Ok(context)
    }

    fn ensure_session(&self, task_id: &str) -> Result<Option<SessionContext>, ProductError> {
        let conn = self.db.conn()?;
        let run: Option<(String, String, Option<String>)> = conn
            .query_row(
                "SELECT id, started_at, ended_at FROM agent_runs \
                 WHERE task_id = ?1 AND agent_kind = 'main' \
                 ORDER BY started_at DESC, id DESC LIMIT 1",
                params![task_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(db_err)?;
        let Some((run_id, run_started_at, ended_at)) = run else {
            return Ok(None);
        };
        let workspace_root: Option<PathBuf> = conn
            .query_row(
                "SELECT workspace_path FROM tasks WHERE id = ?1",
                params![task_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()
            .map_err(db_err)?
            .flatten()
            .map(PathBuf::from);
        let snapshot_context: Option<(String, String)> = conn
            .query_row(
                "SELECT repo_root, workspace_root FROM run_workspace_snapshots WHERE run_id = ?1",
                params![run_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(db_err)?;
        let repo_root = snapshot_context
            .as_ref()
            .map(|value| PathBuf::from(&value.0))
            .or_else(|| {
                workspace_root.as_ref().and_then(|workspace| {
                    if GitService::detect(workspace).unwrap_or(false) {
                        GitService::new(workspace.clone()).repo_root().ok()
                    } else {
                        None
                    }
                })
            });
        let workspace_root = snapshot_context
            .map(|value| PathBuf::from(value.1))
            .or(workspace_root);
        let now = Utc::now().to_rfc3339();
        let session_id = format!("review-{run_id}");
        conn.execute(
            "INSERT OR IGNORE INTO review_sessions \
             (id, run_id, task_id, state, created_at, updated_at) \
             VALUES (?1, ?2, ?3, 'pending', ?4, ?4)",
            params![session_id, run_id, task_id, now],
        )
        .map_err(db_err)?;
        conn.execute(
            "UPDATE review_sessions SET state = 'superseded', updated_at = ?1 \
             WHERE task_id = ?2 AND run_id != ?3 AND state != 'superseded'",
            params![now, task_id, run_id],
        )
        .map_err(db_err)?;
        let materialized_at: Option<String> = conn
            .query_row(
                "SELECT materialized_at FROM review_sessions WHERE id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .map_err(db_err)?;
        drop(conn);

        let mut context = SessionContext {
            id: session_id,
            task_id: task_id.to_string(),
            run_id,
            workspace_root,
            repo_root,
            materialized: materialized_at.is_some(),
            run_finished: ended_at.is_some(),
            run_started_at,
        };
        if context.run_finished && !context.materialized {
            self.materialize(&context)?;
            context.materialized = true;
        }
        Ok(Some(context))
    }

    fn materialize(&self, context: &SessionContext) -> Result<(), ProductError> {
        let mut candidates = self.candidates(context)?;
        candidates.retain(|candidate| {
            candidate.before_hash != candidate.after_hash
                && !is_generated_review_path(&candidate.path)
        });

        if let (Some(repo_root), Some(workspace_root)) =
            (context.repo_root.as_ref(), context.workspace_root.as_ref())
        {
            let mut repo_paths = Vec::new();
            let mut display_to_repo = HashMap::new();
            for candidate in &candidates {
                if let Ok(repo_path) =
                    repo_relative_path(repo_root, workspace_root, &candidate.path)
                {
                    display_to_repo.insert(candidate.path.clone(), repo_path.clone());
                    repo_paths.push(repo_path);
                }
            }
            if let Ok(ignored) = GitService::new(repo_root.clone()).ignored_paths(&repo_paths) {
                candidates.retain(|candidate| {
                    display_to_repo
                        .get(&candidate.path)
                        .map_or(true, |repo_path| !ignored.contains(repo_path))
                });
            }
        }

        let blobs = BlobStore::new(self.db, self.blobs_dir.clone());
        let prepared: Vec<(CandidateChange, Vec<String>)> = candidates
            .into_iter()
            .map(|candidate| {
                let before = read_optional_blob(&blobs, candidate.before_hash.as_deref())?;
                let after = read_optional_blob(&blobs, candidate.after_hash.as_deref())?;
                let fingerprint = change_fingerprint(
                    candidate.before_hash.as_deref(),
                    candidate.after_hash.as_deref(),
                );
                let mut items = review_unit_ids(before.as_deref(), after.as_deref());
                if items.is_empty() {
                    items.push(format!("{SYNTHETIC_ITEM_PREFIX}{fingerprint}"));
                }
                Ok((candidate, items))
            })
            .collect::<Result<_, ProductError>>()?;

        let mut conn = self.db.conn()?;
        let tx = conn.transaction().map_err(db_err)?;
        let now = Utc::now().to_rfc3339();
        for (candidate, items) in prepared {
            let file_id = Uuid::new_v4().to_string();
            tx.execute(
                "INSERT OR IGNORE INTO review_files \
                 (id, session_id, path, before_hash, after_hash, state, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?6)",
                params![
                    file_id,
                    context.id,
                    candidate.path,
                    candidate.before_hash,
                    candidate.after_hash,
                    now,
                ],
            )
            .map_err(db_err)?;
            let actual_file_id: String = tx
                .query_row(
                    "SELECT id FROM review_files WHERE session_id = ?1 AND path = ?2",
                    params![context.id, candidate.path],
                    |row| row.get(0),
                )
                .map_err(db_err)?;
            for (ordinal, item_id) in items.into_iter().enumerate() {
                tx.execute(
                    "INSERT OR IGNORE INTO review_items \
                     (id, review_file_id, item_id, ordinal, state) \
                     VALUES (?1, ?2, ?3, ?4, 'pending')",
                    params![
                        Uuid::new_v4().to_string(),
                        actual_file_id,
                        item_id,
                        ordinal as i64,
                    ],
                )
                .map_err(db_err)?;
            }
        }
        tx.execute(
            "UPDATE review_sessions SET materialized_at = ?1, updated_at = ?1 WHERE id = ?2",
            params![now, context.id],
        )
        .map_err(db_err)?;
        recompute_session(&tx, &context.id)?;
        tx.commit().map_err(db_err)?;
        Ok(())
    }

    fn candidates(&self, context: &SessionContext) -> Result<Vec<CandidateChange>, ProductError> {
        let conn = self.db.conn()?;
        let has_snapshot: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM run_workspace_snapshots WHERE run_id = ?1)",
                params![context.run_id],
                |row| row.get(0),
            )
            .map_err(db_err)?;
        if has_snapshot {
            let mut statement = conn
                .prepare(
                    "SELECT fc.path, fc.before_hash, fc.after_hash \
                     FROM run_snapshot_changes rsc \
                     JOIN file_changes fc ON fc.id = rsc.file_change_id \
                     WHERE rsc.run_id = ?1 ORDER BY fc.path",
                )
                .map_err(db_err)?;
            let rows = statement
                .query_map(params![context.run_id], |row| {
                    Ok(CandidateChange {
                        path: row.get(0)?,
                        before_hash: row.get(1)?,
                        after_hash: row.get(2)?,
                    })
                })
                .map_err(db_err)?;
            return rows.collect::<Result<Vec<_>, _>>().map_err(db_err);
        }

        // Legacy/non-Git fallback: fold only audit rows produced during this run. This avoids
        // resurrecting files from an earlier review in the same long-lived task.
        let mut statement = conn
            .prepare(
                "SELECT path, before_hash, after_hash FROM file_changes \
                 WHERE task_id = ?1 AND created_at >= ?2 ORDER BY created_at, id",
            )
            .map_err(db_err)?;
        let rows = statement
            .query_map(params![context.task_id, context.run_started_at], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(db_err)?;
        let mut folded: BTreeMap<String, CandidateChange> = BTreeMap::new();
        for row in rows {
            let (path, before_hash, after_hash) = row.map_err(db_err)?;
            folded
                .entry(path.clone())
                .and_modify(|candidate| candidate.after_hash = after_hash.clone())
                .or_insert(CandidateChange {
                    path,
                    before_hash,
                    after_hash,
                });
        }
        Ok(folded.into_values().collect())
    }

    fn read_status(&self, context: &SessionContext) -> Result<ReviewStatus, ProductError> {
        let conn = self.db.conn()?;
        let mut statement = conn
            .prepare(
                "SELECT rf.path, rf.state, rf.blocker, \
                        SUM(CASE WHEN ri.state = 'accepted' THEN 1 ELSE 0 END), \
                        SUM(CASE WHEN ri.state = 'rejected' THEN 1 ELSE 0 END), \
                        SUM(CASE WHEN ri.state = 'pending' THEN 1 ELSE 0 END) \
                 FROM review_files rf \
                 JOIN review_items ri ON ri.review_file_id = rf.id \
                 WHERE rf.session_id = ?1 \
                 GROUP BY rf.id, rf.path, rf.state, rf.blocker ORDER BY rf.path",
            )
            .map_err(db_err)?;
        let rows = statement
            .query_map(params![context.id], |row| {
                let state: String = row.get(1)?;
                let remaining_items = usize::try_from(row.get::<_, i64>(5)?).unwrap_or(0);
                Ok(ReviewPathStatus {
                    path: row.get(0)?,
                    accepted: state == "accepted",
                    rejected: state == "rejected",
                    remaining: state == "pending" || state == "conflict" || remaining_items > 0,
                    conflict: state == "conflict",
                    safe_to_accept: state != "conflict",
                    blocker: row.get(2)?,
                    accepted_items: usize::try_from(row.get::<_, i64>(3)?).unwrap_or(0),
                    rejected_items: usize::try_from(row.get::<_, i64>(4)?).unwrap_or(0),
                    remaining_items,
                })
            })
            .map_err(db_err)?;
        let paths = rows.collect::<Result<Vec<_>, _>>().map_err(db_err)?;
        let accepted_count = paths.iter().filter(|path| path.accepted).count();
        let rejected_count = paths.iter().filter(|path| path.rejected).count();
        let conflict_count = paths.iter().filter(|path| path.conflict).count();
        let remaining_count = paths.iter().filter(|path| path.remaining).count();
        Ok(ReviewStatus {
            git_repository: context.repo_root.is_some(),
            repo_root: context
                .repo_root
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned()),
            paths,
            accepted_count,
            rejected_count,
            remaining_count,
            conflict_count,
            can_accept_all: remaining_count > 0 && conflict_count == 0,
        })
    }

    fn task_git_context(&self, task_id: &str) -> Result<(bool, Option<String>), ProductError> {
        let conn = self.db.conn()?;
        let workspace: Option<String> = conn
            .query_row(
                "SELECT workspace_path FROM tasks WHERE id = ?1",
                params![task_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?
            .flatten();
        let Some(workspace) = workspace else {
            return Ok((false, None));
        };
        let path = PathBuf::from(workspace);
        if !GitService::detect(&path).unwrap_or(false) {
            return Ok((false, None));
        }
        let root = GitService::new(path).repo_root()?;
        Ok((true, Some(root.to_string_lossy().into_owned())))
    }

    fn file_snapshots_in_state(
        &self,
        context: &SessionContext,
        state: &str,
    ) -> Result<Vec<ReviewFileSnapshot>, ProductError> {
        let conn = self.db.conn()?;
        let mut statement = conn
            .prepare(
                "SELECT path, before_hash, after_hash FROM review_files \
                 WHERE session_id = ?1 AND state = ?2 ORDER BY path",
            )
            .map_err(db_err)?;
        let rows = statement
            .query_map(params![context.id, state], |row| {
                Ok(ReviewFileSnapshot {
                    path: row.get(0)?,
                    before_hash: row.get(1)?,
                    after_hash: row.get(2)?,
                })
            })
            .map_err(db_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_err)
    }
}

pub fn review_line_id(
    kind: ReviewDiffLineKind,
    old_no: Option<usize>,
    new_no: Option<usize>,
    text: &str,
) -> String {
    let marker = match kind {
        ReviewDiffLineKind::Add => "add",
        ReviewDiffLineKind::Del => "del",
    };
    let input = format!(
        "{marker}|{}|{}|{text}",
        old_no.unwrap_or(0),
        new_no.unwrap_or(0)
    );
    blake3::hash(input.as_bytes()).to_hex()[..20].to_string()
}

fn review_unit_ids(before: Option<&[u8]>, after: Option<&[u8]>) -> Vec<String> {
    let before = before.unwrap_or_default();
    let after = after.unwrap_or_default();
    let (Ok(before), Ok(after)) = (std::str::from_utf8(before), std::str::from_utf8(after)) else {
        return Vec::new();
    };
    let old: Vec<&str> = before.lines().collect();
    let new: Vec<&str> = after.lines().collect();
    if old.len() > MAX_REVIEW_DIFF_LINES || new.len() > MAX_REVIEW_DIFF_LINES {
        let mut ids = Vec::new();
        ids.extend(
            old.iter()
                .take(MAX_REVIEW_DIFF_LINES)
                .enumerate()
                .map(|(index, line)| {
                    review_line_id(ReviewDiffLineKind::Del, Some(index + 1), None, line)
                }),
        );
        ids.extend(
            new.iter()
                .take(MAX_REVIEW_DIFF_LINES)
                .enumerate()
                .map(|(index, line)| {
                    review_line_id(ReviewDiffLineKind::Add, None, Some(index + 1), line)
                }),
        );
        return ids;
    }

    let (n, m) = (old.len(), new.len());
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if old[i] == new[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut ids = Vec::new();
    while i < n && j < m {
        if old[i] == new[j] {
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            ids.push(review_line_id(
                ReviewDiffLineKind::Del,
                Some(i + 1),
                None,
                old[i],
            ));
            i += 1;
        } else {
            ids.push(review_line_id(
                ReviewDiffLineKind::Add,
                None,
                Some(j + 1),
                new[j],
            ));
            j += 1;
        }
    }
    while i < n {
        ids.push(review_line_id(
            ReviewDiffLineKind::Del,
            Some(i + 1),
            None,
            old[i],
        ));
        i += 1;
    }
    while j < m {
        ids.push(review_line_id(
            ReviewDiffLineKind::Add,
            None,
            Some(j + 1),
            new[j],
        ));
        j += 1;
    }
    ids
}

fn read_optional_blob(
    blobs: &BlobStore<'_>,
    hash: Option<&str>,
) -> Result<Option<Vec<u8>>, ProductError> {
    hash.map(|hash| blobs.get(hash))
        .transpose()
        .map(Option::flatten)
}

fn change_fingerprint(before: Option<&str>, after: Option<&str>) -> String {
    blake3::hash(format!("{}:{}", before.unwrap_or("-"), after.unwrap_or("-")).as_bytes()).to_hex()
        [..20]
        .to_string()
}

fn review_file_id(
    tx: &Transaction<'_>,
    session_id: &str,
    display_path: &str,
) -> Result<String, ProductError> {
    tx.query_row(
        "SELECT id FROM review_files WHERE session_id = ?1 AND path = ?2",
        params![session_id, display_path],
        |row| row.get(0),
    )
    .optional()
    .map_err(db_err)?
    .ok_or_else(|| {
        ProductError::DatabaseError(format!(
            "path is not part of the current task review: {display_path}"
        ))
    })
}

fn recompute_file(tx: &Transaction<'_>, file_id: &str) -> Result<(), ProductError> {
    tx.execute(
        "UPDATE review_files SET \
           state = CASE \
             WHEN state = 'conflict' THEN 'conflict' \
             WHEN EXISTS(SELECT 1 FROM review_items WHERE review_file_id = ?1 AND state = 'pending') THEN 'pending' \
             WHEN EXISTS(SELECT 1 FROM review_items WHERE review_file_id = ?1 AND state = 'accepted') THEN 'accepted' \
             ELSE 'rejected' END, \
           updated_at = ?2 \
         WHERE id = ?1",
        params![file_id, Utc::now().to_rfc3339()],
    )
    .map_err(db_err)?;
    Ok(())
}

fn recompute_all_files(tx: &Transaction<'_>, session_id: &str) -> Result<(), ProductError> {
    let mut statement = tx
        .prepare("SELECT id FROM review_files WHERE session_id = ?1")
        .map_err(db_err)?;
    let file_ids = statement
        .query_map(params![session_id], |row| row.get::<_, String>(0))
        .map_err(db_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db_err)?;
    drop(statement);
    for file_id in file_ids {
        recompute_file(tx, &file_id)?;
    }
    Ok(())
}

fn recompute_session(tx: &Transaction<'_>, session_id: &str) -> Result<(), ProductError> {
    tx.execute(
        "UPDATE review_sessions SET \
           state = CASE WHEN EXISTS(SELECT 1 FROM review_files \
                                      WHERE session_id = ?1 AND state IN ('pending', 'conflict')) \
                        THEN 'pending' ELSE 'resolved' END, \
           updated_at = ?2 \
         WHERE id = ?1 AND state != 'superseded'",
        params![session_id, Utc::now().to_rfc3339()],
    )
    .map_err(db_err)?;
    Ok(())
}

fn empty_status(git_repository: bool, repo_root: Option<String>) -> ReviewStatus {
    ReviewStatus {
        git_repository,
        repo_root,
        paths: Vec::new(),
        accepted_count: 0,
        rejected_count: 0,
        remaining_count: 0,
        conflict_count: 0,
        can_accept_all: false,
    }
}

fn is_generated_review_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/").to_ascii_lowercase();
    let segments: Vec<&str> = normalized.split('/').collect();
    const GENERATED_DIRS: &[&str] = &[
        ".git",
        ".pytest_cache",
        ".ruff_cache",
        ".mypy_cache",
        ".cache",
        "__pycache__",
        "node_modules",
        "target",
        ".venv",
        "coverage",
    ];
    if segments
        .iter()
        .any(|segment| GENERATED_DIRS.contains(segment))
    {
        return true;
    }
    let file_name = segments.last().copied().unwrap_or_default();
    const GENERATED_SUFFIXES: &[&str] = &[
        ".log", ".tmp", ".temp", ".pid", ".pyc", ".pyo", ".sqlite", ".sqlite3", ".db", ".swp",
        ".swo",
    ];
    file_name == ".ds_store"
        || file_name == "thumbs.db"
        || GENERATED_SUFFIXES
            .iter()
            .any(|suffix| file_name.ends_with(suffix))
}

fn repo_relative_path(
    repo_root: &Path,
    workspace_root: &Path,
    display_path: &str,
) -> Result<String, ProductError> {
    let display = Path::new(display_path);
    let physical = if display.is_absolute() {
        display.to_path_buf()
    } else {
        workspace_root.join(display)
    };
    let relative = physical.strip_prefix(repo_root).map_err(|_| {
        ProductError::GitError(format!("review path escapes repository: {display_path}"))
    })?;
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(ProductError::GitError(format!(
            "review path is not a normal repository path: {display_path}"
        )));
    }
    Ok(relative.to_string_lossy().replace('\\', "/"))
}

fn db_err(error: rusqlite::Error) -> ProductError {
    ProductError::DatabaseError(error.to_string())
}

fn dominant_scope(paths: &[String]) -> &'static str {
    if paths.iter().all(|path| {
        path.starts_with("docs/")
            || path.ends_with(".md")
            || path.ends_with(".mdx")
            || path.ends_with(".txt")
    }) {
        "docs"
    } else if paths.iter().all(|path| {
        path.contains("/test")
            || path.starts_with("test")
            || path.ends_with("_test.rs")
            || path.ends_with(".test.ts")
    }) {
        "test"
    } else {
        "feat"
    }
}

fn readable_stem(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("task changes")
        .replace(['_', '-'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_units_are_stable_and_line_scoped() {
        let ids = review_unit_ids(Some(b"one\ntwo\n"), Some(b"one\nthree\n"));
        assert_eq!(ids.len(), 2);
        assert_ne!(ids[0], ids[1]);
        assert_eq!(
            ids,
            review_unit_ids(Some(b"one\ntwo\n"), Some(b"one\nthree\n"))
        );
    }

    #[test]
    fn generated_artifacts_do_not_enter_review() {
        assert!(is_generated_review_path("test/tmp/run.log"));
        assert!(is_generated_review_path("target/debug/app.exe"));
        assert!(is_generated_review_path("pkg/__pycache__/module.pyc"));
        assert!(!is_generated_review_path("src/log.rs"));
        assert!(!is_generated_review_path("docs/changelog.md"));
    }
}
