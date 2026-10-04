//! Workspace binding and candidate capture.
//!
//! A task binds a canonical workspace root (capability-checked for every
//! path it touches). Candidate capture copies nothing by itself: it records
//! an immutable manifest of file content hashes, preserved as a blob, and
//! verifies capture/live consistency before evidence acceptance. Foreign
//! linked worktrees are refused unless explicitly registered.

use crate::services::artifacts::sha256_hex;
use crate::workspace_locks::WorkspaceLock;
use r_code_harness_protocol::services::normalize_workspace_relative_path;
use r_code_kernel::task::WorkspaceSnapshotRef;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

const UNBOUND_READ_ONLY_ROOT: &str = "unbound://read-only";

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
    #[error("invalid workspace-relative path: {0}")]
    InvalidLogicalPath(String),
    #[error("workspace path resolves outside the bound root")]
    PathEscapesWorkspace,
    #[error("workspace path identity changed since it was resolved")]
    PathIdentityChanged,
    #[error("candidate manifest has no complete physical identity map")]
    CandidateIdentityMissing,
}

/// Stable physical identity used to detect replacement and hard-link aliasing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "platform", rename_all = "kebab-case")]
pub enum PathIdentity {
    Unix {
        device: u64,
        inode: u64,
        /// inode 变更时间（ctime，纳秒）。delete 后立即 recreate 常复用刚
        /// 释放的 inode——(device, inode) 二元组会把"新文件"误判为原文件，
        /// 身份校验失效；ctime 在每次创建/改写时必然前进，补齐区分度。
        #[serde(default)]
        change_time_ns: i64,
    },
    Windows {
        volume_serial: u32,
        file_index: u64,
    },
    Fallback {
        canonical_path: String,
        bytes: u64,
        modified_ns: Option<u64>,
    },
}

/// A logical path bound to the physical parent/target identities observed
/// during authorization. No filesystem mutation is performed by this type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedWorkspacePath {
    pub workspace_root: PathBuf,
    pub normalized_logical_path: String,
    pub normalized_logical_key: String,
    pub absolute_path: PathBuf,
    pub canonical_nearest_existing_parent: PathBuf,
    pub physical_parent_id: PathIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub existing_target_id: Option<PathIdentity>,
}

impl ResolvedWorkspacePath {
    /// Resolve the path again and reject parent/target replacement or a new
    /// link that redirects traversal outside the workspace.
    pub fn revalidate(&self) -> Result<(), WorkspaceError> {
        let current = resolve_workspace_path(&self.workspace_root, &self.normalized_logical_path)?;
        let unchanged = current.workspace_root == self.workspace_root
            && current.normalized_logical_key == self.normalized_logical_key
            && current.canonical_nearest_existing_parent == self.canonical_nearest_existing_parent
            && current.physical_parent_id == self.physical_parent_id
            && current.existing_target_id == self.existing_target_id;
        if unchanged {
            Ok(())
        } else {
            Err(WorkspaceError::PathIdentityChanged)
        }
    }
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

    /// P26: the workspace's default scan/ignored/ephemeral policy — the
    /// bound root in scope, INV-06's `.git` forever ignored, nothing
    /// ephemeral.
    pub fn scan_policy(&self) -> crate::services::process_effects::ScanPolicy {
        crate::services::process_effects::ScanPolicy::for_workspace_root(
            self.canonical_root.clone(),
        )
    }

    /// Bind a canonical logical path to its current physical identities.
    /// Missing outputs are anchored to their nearest existing parent.
    pub fn resolve_path(
        &self,
        logical_path: &str,
    ) -> Result<ResolvedWorkspacePath, WorkspaceError> {
        resolve_workspace_path(&self.canonical_root, logical_path)
    }

    /// Acquire the cross-profile write lock for this workspace.
    pub fn acquire_write_lock(&self) -> Result<WorkspaceLock, WorkspaceError> {
        WorkspaceLock::acquire(&self.canonical_root).map_err(|e| WorkspaceError::Io(e.to_string()))
    }

    /// Freeze the current checkout identity and content baseline for a run.
    ///
    /// The workspace identity depends only on the canonical physical root, so
    /// every task bound to the same checkout observes the same stable identity.
    /// The baseline is a fresh [`CandidateManifest`] digest and therefore
    /// changes whenever tracked or untracked workspace content changes.
    pub fn snapshot_ref(&self) -> Result<WorkspaceSnapshotRef, WorkspaceError> {
        let canonical_root = self.canonical_root.to_string_lossy().into_owned();
        let manifest = CandidateManifest::capture(self)?;
        Ok(WorkspaceSnapshotRef {
            workspace_identity: format!("sha256:{}", sha256_hex(canonical_root.as_bytes())),
            canonical_root,
            baseline_sha256: manifest.candidate_id,
        })
    }

    /// Explicit compatibility identity for a read-only conversation or plan
    /// that has no usable checkout. Code-changing tasks must never use this
    /// identity; the run manager enforces that distinction before dispatch.
    pub fn unbound_read_only_snapshot() -> WorkspaceSnapshotRef {
        WorkspaceSnapshotRef {
            canonical_root: UNBOUND_READ_ONLY_ROOT.to_string(),
            workspace_identity: "unbound-read-only".to_string(),
            baseline_sha256: sha256_hex(&[]),
        }
    }
}

fn resolve_workspace_path(
    workspace_root: &Path,
    logical_path: &str,
) -> Result<ResolvedWorkspacePath, WorkspaceError> {
    let root = std::fs::canonicalize(workspace_root)
        .map_err(|error| WorkspaceError::Io(format!("canonicalize workspace: {error}")))?;
    let normalized_logical_path = normalize_workspace_relative_path(logical_path)
        .map_err(|error| WorkspaceError::InvalidLogicalPath(error.to_string()))?;
    let normalized_logical_key = logical_key(&normalized_logical_path);
    let absolute_path = normalized_logical_path
        .split('/')
        .fold(root.clone(), |path, component| path.join(component));

    let existing_target = if entry_exists(&absolute_path)? {
        let canonical_target = std::fs::canonicalize(&absolute_path)
            .map_err(|error| WorkspaceError::Io(format!("canonicalize target: {error}")))?;
        ensure_within_root(&root, &canonical_target)?;
        Some(
            std::fs::metadata(&absolute_path)
                .map_err(|error| WorkspaceError::Io(format!("inspect target: {error}")))?,
        )
    } else {
        None
    };

    let mut parent = absolute_path
        .parent()
        .ok_or(WorkspaceError::PathEscapesWorkspace)?
        .to_path_buf();
    while !entry_exists(&parent)? {
        parent = parent
            .parent()
            .ok_or(WorkspaceError::PathEscapesWorkspace)?
            .to_path_buf();
    }
    let canonical_parent = std::fs::canonicalize(&parent)
        .map_err(|error| WorkspaceError::Io(format!("canonicalize parent: {error}")))?;
    ensure_within_root(&root, &canonical_parent)?;
    let parent_metadata = std::fs::metadata(&canonical_parent)
        .map_err(|error| WorkspaceError::Io(format!("inspect parent: {error}")))?;

    Ok(ResolvedWorkspacePath {
        workspace_root: root,
        normalized_logical_path,
        normalized_logical_key,
        absolute_path: absolute_path.clone(),
        canonical_nearest_existing_parent: canonical_parent.clone(),
        physical_parent_id: path_identity(&canonical_parent, &parent_metadata)?,
        existing_target_id: existing_target
            .as_ref()
            .map(|metadata| path_identity(&absolute_path, metadata))
            .transpose()?,
    })
}

fn entry_exists(path: &Path) -> Result<bool, WorkspaceError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(WorkspaceError::Io(format!("inspect path: {error}"))),
    }
}

fn ensure_within_root(root: &Path, path: &Path) -> Result<(), WorkspaceError> {
    let root_key = physical_path_key(root);
    let path_key = physical_path_key(path);
    let prefix = format!("{root_key}/");
    if path_key == root_key || path_key.starts_with(&prefix) {
        Ok(())
    } else {
        Err(WorkspaceError::PathEscapesWorkspace)
    }
}

fn logical_key(path: &str) -> String {
    #[cfg(windows)]
    {
        path.to_lowercase()
    }
    #[cfg(not(windows))]
    {
        path.to_string()
    }
}

fn physical_path_key(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    {
        value.to_lowercase().trim_end_matches('/').to_string()
    }
    #[cfg(not(windows))]
    {
        value.trim_end_matches('/').to_string()
    }
}

#[cfg(unix)]
fn path_identity(_: &Path, metadata: &std::fs::Metadata) -> Result<PathIdentity, WorkspaceError> {
    use std::os::unix::fs::MetadataExt;
    Ok(PathIdentity::Unix {
        device: metadata.dev(),
        inode: metadata.ino(),
        change_time_ns: metadata.ctime_nsec(),
    })
}

#[cfg(windows)]
fn path_identity(
    path: &Path,
    metadata: &std::fs::Metadata,
) -> Result<PathIdentity, WorkspaceError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        OPEN_EXISTING,
    };

    let mut wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    wide.push(0);
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle != INVALID_HANDLE_VALUE {
        let mut info = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::zeroed();
        let read = unsafe { GetFileInformationByHandle(handle, info.as_mut_ptr()) };
        unsafe { CloseHandle(handle) };
        if read != 0 {
            let info = unsafe { info.assume_init() };
            let file_index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
            if file_index != 0 {
                return Ok(PathIdentity::Windows {
                    volume_serial: info.dwVolumeSerialNumber,
                    file_index,
                });
            }
        }
    }
    fallback_path_identity(path, metadata)
}

#[cfg(not(any(unix, windows)))]
fn path_identity(
    path: &Path,
    metadata: &std::fs::Metadata,
) -> Result<PathIdentity, WorkspaceError> {
    fallback_path_identity(path, metadata)
}

#[cfg(not(unix))]
fn fallback_path_identity(
    path: &Path,
    metadata: &std::fs::Metadata,
) -> Result<PathIdentity, WorkspaceError> {
    let canonical = std::fs::canonicalize(path)
        .map_err(|error| WorkspaceError::Io(format!("canonicalize identity: {error}")))?;
    let modified_ns = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|duration| u64::try_from(duration.as_nanos()).ok());
    Ok(PathIdentity::Fallback {
        canonical_path: physical_path_key(&canonical),
        bytes: metadata.len(),
        modified_ns,
    })
}

/// An immutable capture of candidate content identity.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CandidateManifest {
    /// Content-addressed id: changes only when file content changes.
    pub candidate_id: String,
    /// Relative path → sha256, sorted for stable hashing.
    pub files: BTreeMap<String, String>,
    /// Relative path → stable physical file identity. Old artifacts decode,
    /// but cannot pass live verification without this complete map.
    #[serde(default)]
    pub physical_files: BTreeMap<String, PathIdentity>,
    pub captured_files: usize,
}

impl CandidateManifest {
    /// Capture the current state of the bound root: every file below the
    /// root except `.git` internals. The filesystem is never mutated.
    pub fn capture(binding: &TaskWorkspaceBinding) -> Result<Self, WorkspaceError> {
        let mut files = BTreeMap::new();
        let mut physical_files = BTreeMap::new();
        collect_files(
            &binding.canonical_root,
            &binding.canonical_root,
            &mut files,
            &mut physical_files,
        )?;
        let mut material = String::new();
        for (path, digest) in &files {
            material.push_str(path);
            material.push('\u{1}');
            material.push_str(digest);
            material.push('\u{1}');
            let identity = physical_files
                .get(path)
                .ok_or(WorkspaceError::CandidateIdentityMissing)?;
            material.push_str(
                &serde_json::to_string(identity)
                    .map_err(|error| WorkspaceError::Io(error.to_string()))?,
            );
            material.push('\u{1}');
        }
        Ok(Self {
            candidate_id: sha256_hex(material.as_bytes()),
            files,
            physical_files,
            captured_files: 0,
        })
        .map(|mut manifest| {
            manifest.captured_files = manifest.files.len();
            manifest
        })
    }

    /// Verify the live workspace still matches the capture.
    pub fn verify_live(&self, binding: &TaskWorkspaceBinding) -> Result<(), WorkspaceError> {
        if self.physical_files.len() != self.files.len()
            || self
                .files
                .keys()
                .any(|path| !self.physical_files.contains_key(path))
        {
            return Err(WorkspaceError::CandidateIdentityMissing);
        }
        let live = Self::capture(binding)?;
        if live.files != self.files || live.physical_files != self.physical_files {
            return Err(WorkspaceError::Inconsistent {
                captured: self.files.len(),
                live: live.files.len(),
            });
        }
        Ok(())
    }
}

fn collect_files(
    root: &Path,
    dir: &Path,
    files: &mut BTreeMap<String, String>,
    physical_files: &mut BTreeMap<String, PathIdentity>,
) -> Result<(), WorkspaceError> {
    for entry in std::fs::read_dir(dir).map_err(|error| WorkspaceError::Io(error.to_string()))? {
        let entry = entry.map_err(|error| WorkspaceError::Io(error.to_string()))?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|error| WorkspaceError::Io(error.to_string()))?;
        if is_link_or_reparse(&metadata) {
            return Err(WorkspaceError::Io(
                "candidate capture refuses linked or reparse entries".to_string(),
            ));
        }
        let canonical =
            std::fs::canonicalize(&path).map_err(|error| WorkspaceError::Io(error.to_string()))?;
        if !canonical.starts_with(root) {
            return Err(WorkspaceError::PathEscapesWorkspace);
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == ".git" || name == "node_modules" || name == "target" {
            continue;
        }
        if metadata.is_dir() {
            collect_files(root, &path, files, physical_files)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .expect("walk stays under root")
                .to_string_lossy()
                .replace('\\', "/");
            let bytes =
                std::fs::read(&path).map_err(|error| WorkspaceError::Io(error.to_string()))?;
            let identity = path_identity(&canonical, &metadata)?;
            files.insert(relative.clone(), sha256_hex(&bytes));
            physical_files.insert(relative, identity);
        }
    }
    Ok(())
}

fn is_link_or_reparse(metadata: &std::fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    false
}
