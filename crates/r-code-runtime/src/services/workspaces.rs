//! Workspace binding and candidate capture.
//!
//! A task binds a canonical workspace root (capability-checked for every
//! path it touches). Candidate capture copies nothing by itself: it records
//! an immutable manifest of file content hashes, preserved as a blob, and
//! verifies capture/live consistency before evidence acceptance. Foreign
//! linked worktrees are refused unless explicitly registered.

use crate::services::artifacts::sha256_hex;
use crate::workspace_locks::WorkspaceLock;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

/// Errors from workspace services.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkspaceError {
    #[error("io failure: {0}")]
    Io(String),
    #[error("workspace root {0:?} is a linked worktree and was not registered for this task")]
    ForeignWorktree(String),
    #[error("workspace root {0:?} is not a directory")]
    NotADirectory(String),
    #[error("the live workspace changed since capture ({captured} files captured, {live} now)")]
    Inconsistent { captured: usize, live: usize },
}

/// The binding between a task and a physical workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskWorkspaceBinding {
    pub task_id: String,
    pub canonical_root: PathBuf,
    registered_worktrees: Vec<PathBuf>,
}

impl TaskWorkspaceBinding {
    /// Bind a local workspace. `.git` being a *file* means a linked
    /// worktree; only pre-registered roots are admitted.
    pub fn bind_local(
        task_id: &str,
        root: &Path,
        registered_worktrees: &[PathBuf],
    ) -> Result<Self, WorkspaceError> {
        let canonical = std::fs::canonicalize(root)
            .map_err(|e| WorkspaceError::Io(format!("canonicalize: {e}")))?;
        if !canonical.is_dir() {
            return Err(WorkspaceError::NotADirectory(
                canonical.to_string_lossy().into(),
            ));
        }
        let git_path = canonical.join(".git");
        if git_path.is_file() {
            let registered = registered_worktrees.iter().any(|worktree| {
                std::fs::canonicalize(worktree)
                    .map(|path| path == canonical)
                    .unwrap_or(false)
            });
            if !registered {
                return Err(WorkspaceError::ForeignWorktree(
                    canonical.to_string_lossy().into(),
                ));
            }
        }
        Ok(Self {
            task_id: task_id.to_string(),
            canonical_root: canonical,
            registered_worktrees: registered_worktrees.to_vec(),
        })
    }

    /// Capability check: a path stays within the bound root.
    pub fn contains(&self, path: &Path) -> bool {
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        canonical.starts_with(&self.canonical_root)
    }

    /// Acquire the cross-profile write lock for this workspace.
    pub fn acquire_write_lock(&self) -> Result<WorkspaceLock, WorkspaceError> {
        WorkspaceLock::acquire(&self.canonical_root).map_err(|e| WorkspaceError::Io(e.to_string()))
    }
}

/// An immutable capture of candidate content identity.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CandidateManifest {
    /// Content-addressed id: changes only when file content changes.
    pub candidate_id: String,
    /// Relative path → sha256, sorted for stable hashing.
    pub files: BTreeMap<String, String>,
    pub captured_files: usize,
}

impl CandidateManifest {
    /// Capture the current state of the bound root: every file below the
    /// root except `.git` internals. The filesystem is never mutated.
    pub fn capture(binding: &TaskWorkspaceBinding) -> Result<Self, WorkspaceError> {
        let mut files = BTreeMap::new();
        collect_files(&binding.canonical_root, &binding.canonical_root, &mut files)
            .map_err(|e| WorkspaceError::Io(e.to_string()))?;
        let mut material = String::new();
        for (path, digest) in &files {
            material.push_str(path);
            material.push('\u{1}');
            material.push_str(digest);
            material.push('\u{1}');
        }
        Ok(Self {
            candidate_id: sha256_hex(material.as_bytes()),
            files,
            captured_files: 0,
        })
        .map(|mut manifest| {
            manifest.captured_files = manifest.files.len();
            manifest
        })
    }

    /// Verify the live workspace still matches the capture.
    pub fn verify_live(&self, binding: &TaskWorkspaceBinding) -> Result<(), WorkspaceError> {
        let live = Self::capture(binding)?;
        if live.files != self.files {
            return Err(WorkspaceError::Inconsistent {
                captured: self.files.len(),
                live: live.files.len(),
            });
        }
        Ok(())
    }
}

fn collect_files(root: &Path, dir: &Path, files: &mut BTreeMap<String, String>) -> io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == ".git" || name == "node_modules" || name == "target" {
            continue;
        }
        if path.is_dir() {
            collect_files(root, &path, files)?;
        } else if path.is_file() {
            let relative = path
                .strip_prefix(root)
                .expect("walk stays under root")
                .to_string_lossy()
                .replace('\\', "/");
            let bytes = std::fs::read(&path)?;
            files.insert(relative, sha256_hex(&bytes));
        }
    }
    Ok(())
}
