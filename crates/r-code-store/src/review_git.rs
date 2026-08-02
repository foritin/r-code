use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use r_code_core::error::ProductError;
use rusqlite::params;
use serde::{Deserialize, Serialize};

use crate::{Database, GitService};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewPathStatus {
    pub path: String,
    pub staged: bool,
    pub remaining: bool,
    pub conflict: bool,
    pub preexisting_dirty: bool,
    pub safe_to_accept: bool,
    pub blocker: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewGitStatus {
    pub git_repository: bool,
    pub repo_root: Option<String>,
    pub paths: Vec<ReviewPathStatus>,
    pub staged_count: usize,
    pub remaining_count: usize,
    pub conflict_count: usize,
    pub can_accept_all: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReviewAcceptResult {
    pub path: Option<String>,
    pub staged_count: usize,
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

#[derive(Debug, Clone)]
struct ReviewContext {
    repo_root: PathBuf,
    paths: BTreeMap<String, PathContext>,
}

#[derive(Debug, Clone)]
struct PathContext {
    display_path: String,
    repo_path: String,
    entry_head_tree: Option<String>,
    entry_index_tree: Option<String>,
    entry_worktree_tree: Option<String>,
    legacy: bool,
    legacy_preexisting_dirty: bool,
}

#[derive(Debug, Clone)]
struct ParsedPatchLine {
    line: ReviewDiffLine,
    old_anchor: usize,
    new_anchor: usize,
}

pub struct ReviewGitService<'a> {
    db: &'a Database,
}

impl<'a> ReviewGitService<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn status(&self, task_id: &str) -> Result<ReviewGitStatus, ProductError> {
        let Some(context) = self.context(task_id)? else {
            return Ok(ReviewGitStatus {
                git_repository: false,
                repo_root: None,
                paths: Vec::new(),
                staged_count: 0,
                remaining_count: 0,
                conflict_count: 0,
                can_accept_all: false,
            });
        };
        let git = GitService::new(context.repo_root.clone());
        let mut paths = Vec::with_capacity(context.paths.len());
        for path in context.paths.values() {
            paths.push(path_status(&git, path)?);
        }
        let staged_count = paths.iter().filter(|path| path.staged).count();
        let remaining_count = paths.iter().filter(|path| path.remaining).count();
        let conflict_count = paths
            .iter()
            .filter(|path| path.conflict || path.preexisting_dirty)
            .count();
        Ok(ReviewGitStatus {
            git_repository: true,
            repo_root: Some(context.repo_root.to_string_lossy().into_owned()),
            can_accept_all: remaining_count > 0 && conflict_count == 0,
            paths,
            staged_count,
            remaining_count,
            conflict_count,
        })
    }

    pub fn diff_lines(
        &self,
        task_id: &str,
        display_path: &str,
    ) -> Result<Option<Vec<ReviewDiffLine>>, ProductError> {
        let Some(context) = self.context(task_id)? else {
            return Ok(None);
        };
        let path = allowed_path(&context, display_path)?;
        let git = GitService::new(context.repo_root.clone());
        if !git.is_indexed(&path.repo_path)? {
            return Ok(None);
        }
        let patch = git.worktree_patch(&path.repo_path)?;
        Ok(Some(
            parse_patch_lines(&patch)
                .into_iter()
                .map(|line| line.line)
                .collect(),
        ))
    }

    pub fn accept_line(
        &self,
        task_id: &str,
        display_path: &str,
        line_id: &str,
    ) -> Result<ReviewAcceptResult, ProductError> {
        let context = self.context(task_id)?.ok_or_else(|| {
            ProductError::GitError("task is not attached to a Git repository".into())
        })?;
        let path = allowed_path(&context, display_path)?;
        let git = GitService::new(context.repo_root.clone());
        ensure_safe_path(&git, path)?;
        if !git.is_indexed(&path.repo_path)? {
            git.intent_to_add(&path.repo_path)?;
        }
        let patch = git.worktree_patch(&path.repo_path)?;
        let parsed = parse_patch_lines(&patch);
        let selected = parsed
            .iter()
            .find(|line| line.line.line_id == line_id)
            .ok_or_else(|| {
                ProductError::GitError("该行已接受或差异已经变化，请刷新后重试".into())
            })?;
        if patch.contains("deleted file mode") && parsed.len() > 1 {
            return Err(ProductError::GitError(
                "整文件删除不能逐行接受，请使用“接受文件”".into(),
            ));
        }
        let partial = one_line_patch(&patch, selected)?;
        git.apply_cached_patch(partial.as_bytes())?;
        self.accept_result(task_id, Some(display_path.to_string()))
    }

    pub fn accept_file(
        &self,
        task_id: &str,
        display_path: &str,
    ) -> Result<ReviewAcceptResult, ProductError> {
        let context = self.context(task_id)?.ok_or_else(|| {
            ProductError::GitError("task is not attached to a Git repository".into())
        })?;
        let path = allowed_path(&context, display_path)?;
        let git = GitService::new(context.repo_root.clone());
        ensure_safe_path(&git, path)?;
        git.stage(&path.repo_path)?;
        self.accept_result(task_id, Some(display_path.to_string()))
    }

    pub fn accept_all(&self, task_id: &str) -> Result<ReviewAcceptResult, ProductError> {
        let context = self.context(task_id)?.ok_or_else(|| {
            ProductError::GitError("task is not attached to a Git repository".into())
        })?;
        let git = GitService::new(context.repo_root);
        for path in context.paths.values() {
            ensure_safe_path(&git, path)?;
        }
        for path in context.paths.values() {
            if git.has_worktree_change(&path.repo_path)? || !git.is_indexed(&path.repo_path)? {
                git.stage(&path.repo_path)?;
            }
        }
        self.accept_result(task_id, None)
    }

    pub fn delivery_status(&self, task_id: &str) -> Result<GitDeliveryStatus, ProductError> {
        let context = self.context(task_id)?.ok_or_else(|| {
            ProductError::GitError("task is not attached to a Git repository".into())
        })?;
        let git = GitService::new(context.repo_root);
        let branch = git.current_branch().ok();
        let upstream = git.upstream()?;
        let (ahead, behind) = git.ahead_behind()?.unwrap_or((0, 0));
        let staged = git.staged_paths()?;
        let repo_to_display: BTreeMap<&str, &str> = context
            .paths
            .values()
            .map(|path| (path.repo_path.as_str(), path.display_path.as_str()))
            .collect();
        let mut staged_task_paths = Vec::new();
        let mut staged_other_paths = Vec::new();
        for path in staged {
            match repo_to_display.get(path.as_str()) {
                Some(display) => staged_task_paths.push((*display).to_string()),
                None => staged_other_paths.push(path),
            }
        }
        let review = self.status(task_id)?;
        let mut blockers = Vec::new();
        if branch.is_none() {
            blockers.push("当前仓库处于 detached HEAD，不能由审核页提交或推送".into());
        }
        if !staged_other_paths.is_empty() {
            blockers.push(format!(
                "暂存区包含 {} 个不属于本任务的路径",
                staged_other_paths.len()
            ));
        }
        if review.conflict_count > 0 {
            blockers.push("存在冲突或任务开始前已有改动的路径".into());
        }
        let can_commit = !staged_task_paths.is_empty() && blockers.is_empty();
        let can_push = branch.is_some() && upstream.is_some() && ahead > 0;
        Ok(GitDeliveryStatus {
            branch,
            upstream,
            ahead,
            behind,
            staged_task_paths,
            staged_other_paths,
            can_commit,
            can_push,
            blockers,
        })
    }

    pub fn suggest_commit_message(&self, task_id: &str) -> Result<String, ProductError> {
        let status = self.delivery_status(task_id)?;
        if status.staged_task_paths.is_empty() {
            return Err(ProductError::GitError(
                "还没有接受到暂存区的任务变更".into(),
            ));
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
        let context = self.context(task_id)?.ok_or_else(|| {
            ProductError::GitError("task is not attached to a Git repository".into())
        })?;
        let sha = GitService::new(context.repo_root).commit(message)?;
        Ok(GitCommitResult {
            sha,
            message: message.to_string(),
        })
    }

    pub fn push_task(&self, task_id: &str) -> Result<GitPushResult, ProductError> {
        let context = self.context(task_id)?.ok_or_else(|| {
            ProductError::GitError("task is not attached to a Git repository".into())
        })?;
        let git = GitService::new(context.repo_root);
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
            staged_count: status.staged_count,
            remaining_count: status.remaining_count,
            fully_accepted: status.remaining_count == 0 && status.conflict_count == 0,
        })
    }

    fn context(&self, task_id: &str) -> Result<Option<ReviewContext>, ProductError> {
        let conn = self.db.conn()?;
        let first_snapshot = {
            let mut snapshots = conn
                .prepare(
                    "SELECT repo_root, workspace_root, entry_head_tree, entry_index_tree, \
                            entry_worktree_tree \
                     FROM run_workspace_snapshots WHERE task_id = ?1 \
                     ORDER BY captured_at ASC LIMIT 1",
                )
                .map_err(db_err)?;
            let mut rows = snapshots.query(params![task_id]).map_err(db_err)?;
            rows.next()
                .map_err(db_err)?
                .map(|row| {
                    Ok::<_, ProductError>((
                        PathBuf::from(row.get::<_, String>(0).map_err(db_err)?),
                        PathBuf::from(row.get::<_, String>(1).map_err(db_err)?),
                        row.get::<_, Option<String>>(2).map_err(db_err)?,
                        row.get::<_, String>(3).map_err(db_err)?,
                        row.get::<_, String>(4).map_err(db_err)?,
                    ))
                })
                .transpose()?
        };

        // Keep the earliest recorded before-state for conservative legacy classification.
        let mut display_paths = BTreeMap::new();
        let mut change_stmt = conn
            .prepare(
                "SELECT path, before_hash FROM file_changes \
                 WHERE task_id = ?1 ORDER BY path, created_at",
            )
            .map_err(db_err)?;
        let rows = change_stmt
            .query_map(params![task_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
            })
            .map_err(db_err)?;
        for row in rows {
            let (path, before_hash) = row.map_err(db_err)?;
            display_paths.entry(path).or_insert(before_hash.is_some());
        }
        if display_paths.is_empty() {
            return Ok(None);
        }

        let (repo_root, workspace_root, legacy) =
            if let Some((repo, workspace, _, _, _)) = &first_snapshot {
                (repo.clone(), workspace.clone(), false)
            } else {
                let project: String = conn
                    .query_row(
                        "SELECT project_id FROM tasks WHERE id = ?1",
                        params![task_id],
                        |row| row.get(0),
                    )
                    .map_err(db_err)?;
                let workspace = PathBuf::from(project);
                if !GitService::detect(&workspace)? {
                    return Ok(None);
                }
                let repo = GitService::new(workspace.clone()).repo_root()?;
                (repo, workspace, true)
            };
        // `file_changes` is an audit trail, not the live review queue. Intersect it with
        // Git's current porcelain status so ignored, restored, or otherwise clean paths do
        // not linger in Review merely because a tool recorded them earlier in the run.
        let current_git_paths: BTreeSet<String> = GitService::new(repo_root.clone())
            .status()?
            .into_iter()
            .map(|entry| entry.path.replace('\\', "/"))
            .collect();

        let mut path_snapshot_stmt = conn
            .prepare(
                "SELECT rws.entry_head_tree, rws.entry_index_tree, rws.entry_worktree_tree \
                 FROM run_snapshot_changes rsc \
                 JOIN file_changes fc ON fc.id = rsc.file_change_id \
                 JOIN run_workspace_snapshots rws ON rws.run_id = rsc.run_id \
                 WHERE fc.task_id = ?1 AND fc.path = ?2 \
                 ORDER BY rws.captured_at ASC LIMIT 1",
            )
            .map_err(db_err)?;
        let mut paths = BTreeMap::new();
        for (display_path, legacy_preexisting_dirty) in display_paths {
            let repo_path = repo_relative_path(&repo_root, &workspace_root, &display_path)?;
            if !current_git_paths.contains(&repo_path) {
                continue;
            }
            let entry_trees = if legacy {
                None
            } else {
                let mut rows = path_snapshot_stmt
                    .query(params![task_id, &display_path])
                    .map_err(db_err)?;
                rows.next()
                    .map_err(db_err)?
                    .map(|row| {
                        Ok::<_, ProductError>((
                            row.get::<_, Option<String>>(0).map_err(db_err)?,
                            row.get::<_, String>(1).map_err(db_err)?,
                            row.get::<_, String>(2).map_err(db_err)?,
                        ))
                    })
                    .transpose()?
                    .or_else(|| {
                        first_snapshot
                            .as_ref()
                            .map(|(_, _, head, index, worktree)| {
                                (head.clone(), index.clone(), worktree.clone())
                            })
                    })
            };
            paths.insert(
                display_path.clone(),
                PathContext {
                    display_path,
                    repo_path,
                    entry_head_tree: entry_trees.as_ref().and_then(|trees| trees.0.clone()),
                    entry_index_tree: entry_trees.as_ref().map(|trees| trees.1.clone()),
                    entry_worktree_tree: entry_trees.as_ref().map(|trees| trees.2.clone()),
                    legacy,
                    legacy_preexisting_dirty,
                },
            );
        }
        Ok(Some(ReviewContext { repo_root, paths }))
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

fn path_status(git: &GitService, path: &PathContext) -> Result<ReviewPathStatus, ProductError> {
    let conflict = git.has_conflict(&path.repo_path)?;
    let staged = git.has_staged_change(&path.repo_path)?;
    let indexed = git.is_indexed(&path.repo_path)?;
    let physical_exists = git.repo_root()?.join(&path.repo_path).exists();
    let remaining = if indexed {
        git.has_worktree_change(&path.repo_path)?
    } else {
        physical_exists
    };
    let preexisting_dirty = if path.legacy {
        path.legacy_preexisting_dirty
    } else {
        let index_tree = path.entry_index_tree.as_deref().unwrap_or_default();
        let worktree_tree = path.entry_worktree_tree.as_deref().unwrap_or_default();
        let entry_index = git.blob_at_tree(index_tree, &path.repo_path)?;
        let entry_worktree = git.blob_at_tree(worktree_tree, &path.repo_path)?;
        let entry_head = match path.entry_head_tree.as_deref() {
            Some(tree) => git.blob_at_tree(tree, &path.repo_path)?,
            None => None,
        };
        entry_index != entry_worktree || entry_index != entry_head
    };
    let blocker = if conflict {
        Some("文件存在 Git 冲突".to_string())
    } else if preexisting_dirty {
        Some("任务开始前该文件已有未提交改动，不能自动混入审核".to_string())
    } else {
        None
    };
    Ok(ReviewPathStatus {
        path: path.display_path.clone(),
        staged,
        remaining,
        conflict,
        preexisting_dirty,
        safe_to_accept: blocker.is_none(),
        blocker,
    })
}

fn ensure_safe_path(git: &GitService, path: &PathContext) -> Result<(), ProductError> {
    let status = path_status(git, path)?;
    if let Some(blocker) = status.blocker {
        return Err(ProductError::GitError(format!(
            "{}: {blocker}",
            path.display_path
        )));
    }
    Ok(())
}

fn allowed_path<'a>(
    context: &'a ReviewContext,
    display_path: &str,
) -> Result<&'a PathContext, ProductError> {
    context.paths.get(display_path).ok_or_else(|| {
        ProductError::GitError(format!(
            "path is not part of this task review: {display_path}"
        ))
    })
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

fn parse_patch_lines(patch: &str) -> Vec<ParsedPatchLine> {
    let mut result = Vec::new();
    let mut old_cursor = 0usize;
    let mut new_cursor = 0usize;
    let mut in_hunk = false;
    for raw in patch.lines() {
        if raw.starts_with("@@ ") {
            if let Some((old_start, new_start)) = parse_hunk_header(raw) {
                old_cursor = old_start;
                new_cursor = new_start;
                in_hunk = true;
            }
            continue;
        }
        if !in_hunk || raw.starts_with("\\ No newline") {
            continue;
        }
        let Some((prefix, text)) = raw.split_at_checked(1) else {
            continue;
        };
        match prefix {
            "+" => {
                let line = ReviewDiffLine {
                    line_id: review_line_id(ReviewDiffLineKind::Add, None, Some(new_cursor), text),
                    kind: ReviewDiffLineKind::Add,
                    text: text.to_string(),
                    old_no: None,
                    new_no: Some(new_cursor),
                };
                result.push(ParsedPatchLine {
                    line,
                    old_anchor: old_cursor,
                    new_anchor: new_cursor,
                });
                new_cursor += 1;
            }
            "-" => {
                let line = ReviewDiffLine {
                    line_id: review_line_id(ReviewDiffLineKind::Del, Some(old_cursor), None, text),
                    kind: ReviewDiffLineKind::Del,
                    text: text.to_string(),
                    old_no: Some(old_cursor),
                    new_no: None,
                };
                result.push(ParsedPatchLine {
                    line,
                    old_anchor: old_cursor,
                    new_anchor: new_cursor,
                });
                old_cursor += 1;
            }
            " " => {
                old_cursor += 1;
                new_cursor += 1;
            }
            _ => {}
        }
    }
    result
}

fn parse_hunk_header(header: &str) -> Option<(usize, usize)> {
    let body = header.strip_prefix("@@ -")?;
    let (old, rest) = body.split_once(" +")?;
    let (new, _) = rest.split_once(" @@")?;
    Some((range_start(old)?, range_start(new)?))
}

fn range_start(value: &str) -> Option<usize> {
    value.split(',').next()?.parse().ok()
}

fn one_line_patch(patch: &str, selected: &ParsedPatchLine) -> Result<String, ProductError> {
    let header_end = patch
        .lines()
        .position(|line| line.starts_with("@@ "))
        .ok_or_else(|| ProductError::GitError("Git diff did not contain a hunk".into()))?;
    let mut output = String::new();
    for line in patch.lines().take(header_end) {
        output.push_str(line);
        output.push('\n');
    }
    match selected.line.kind {
        ReviewDiffLineKind::Add => output.push_str(&format!(
            "@@ -{},0 +{},1 @@\n+{}\n",
            selected.old_anchor, selected.new_anchor, selected.line.text
        )),
        ReviewDiffLineKind::Del => output.push_str(&format!(
            "@@ -{},1 +{},0 @@\n-{}\n",
            selected.old_anchor, selected.new_anchor, selected.line.text
        )),
    }
    Ok(output)
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
