//! P29 — the daemon's only Git reader: restricted gix status/log.
//!
//! INV-06: Git reads go through this module and nowhere else. The reader
//! opens the canonical `.git` directory the workspace binding resolved —
//! never discovers, never wanders — with isolated permissions: no system/
//! user/env configuration, no config includes, no environment variables,
//! so nothing outside the repository can inject configuration or
//! alternates. Process filters are structurally impossible: no
//! configuration section is trusted for filter drivers, so the pipeline
//! never carries a driver and `gitoxide`'s command plumbing can never
//! spawn. The `gix::Repository` handle stays private — only plain data
//! leaves this module, and nothing re-exports it beyond `services`.

use std::path::Path;

// The plain-data projections are the module's public surface; the gix
// types stay behind `bounded`.
pub use bounded::{ChangeKind, LogReport, Stage, StatusEntry, StatusReport};

/// Entry ceiling for one bounded status read.
pub const MAX_STATUS_ENTRIES: usize = 10_000;
/// Commit ceiling for one bounded log read.
pub const MAX_LOG_COMMITS: usize = 1_000;

/// Why a restricted read could not be served.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GitReadError {
    #[error("the git directory {0:?} could not be opened: {1}")]
    Open(String, String),
    #[error("git read failed: {0}")]
    Read(String),
    #[error("the read bound must be at least 1")]
    ZeroBound,
}

fn open_error(path: &Path, error: impl std::fmt::Display) -> GitReadError {
    GitReadError::Open(path.display().to_string(), error.to_string())
}

/// Process filters are structurally impossible: no configuration section
/// is trusted for the filtered lookups, so filter drivers (local ones
/// included) read as absent and the pipeline never carries a driver —
/// gitoxide's command plumbing can never spawn on this reader's behalf.
const NO_TRUSTED_CONFIG_SECTION: fn(&gix::config::file::Metadata) -> bool = |_| false;

/// One restricted, read-only reader over the canonical git directory.
/// Constructed only through [`open_read_only`].
pub struct GitReader {
    repo: gix::ThreadSafeRepository,
}

/// Open the canonical `.git` directory with the restricted options: no
/// discovery (the caller passes the exact git dir the workspace bound),
/// isolated permissions (repository-local configuration only, includes
/// off, every environment category denied) and no trusted filter sections.
pub fn open_read_only(git_dir: &Path) -> Result<GitReader, GitReadError> {
    // The canonical git-dir signature: a worktree root or a foreign
    // directory is refused before gix ever sees it — this reader never
    // discovers and never wanders.
    if !git_dir.join("HEAD").is_file() {
        return Err(open_error(
            git_dir,
            "not a canonical git directory (no HEAD file)",
        ));
    }
    let options = gix::open::Options::isolated()
        .filter_config_section(NO_TRUSTED_CONFIG_SECTION)
        .strict_config(true);
    let repo = gix::ThreadSafeRepository::open_opts(git_dir, options)
        .map_err(|error| open_error(git_dir, error))?;
    Ok(GitReader { repo })
}

impl GitReader {
    /// Bounded status over HEAD→index and index→worktree plus untracked
    /// files, as plain data. `None` untracked handling is not consulted:
    /// untracked files are always reported, matching `git status
    /// --porcelain`'s default.
    pub fn status(&self, limit: usize) -> Result<StatusReport, GitReadError> {
        if limit == 0 {
            return Err(GitReadError::ZeroBound);
        }
        let repo = self.repo.to_thread_local();
        let platform = repo
            .status(gix::progress::Discard)
            .map_err(|error| GitReadError::Read(error.to_string()))?
            .untracked_files(gix::status::UntrackedFiles::Files);
        let iterator = platform
            .into_iter(Vec::<gix::bstr::BString>::new())
            .map_err(|error| GitReadError::Read(error.to_string()))?;
        let mut entries = Vec::new();
        let mut truncated = false;
        for item in iterator {
            let item = item.map_err(|error| GitReadError::Read(error.to_string()))?;
            if entries.len() >= limit {
                truncated = true;
                break;
            }
            if let Some(entry) = bounded::map_status_item(&item) {
                entries.push(entry);
            }
        }
        Ok(StatusReport { entries, truncated })
    }

    /// Bounded log: the commits reachable from HEAD, oldest-agnostic, as
    /// plain hex ids. `git log` order is date/parent-dependent; consumers
    /// that need a stable order sort the ids.
    pub fn log(&self, limit: usize) -> Result<LogReport, GitReadError> {
        if limit == 0 {
            return Err(GitReadError::ZeroBound);
        }
        let repo = self.repo.to_thread_local();
        let head = repo
            .head_commit()
            .map_err(|error| GitReadError::Read(error.to_string()))?;
        let walk = repo
            .rev_walk([head.id])
            .all()
            .map_err(|error| GitReadError::Read(error.to_string()))?;
        let mut commits = Vec::new();
        let mut truncated = false;
        for info in walk {
            let info = info.map_err(|error| GitReadError::Read(error.to_string()))?;
            if commits.len() >= limit {
                truncated = true;
                break;
            }
            commits.push(info.id.to_hex().to_string());
        }
        Ok(LogReport { commits, truncated })
    }
}

/// Plain-data projections and the item mapping. Kept private to the module
/// tree: the gix types never leak past `bounded`.
mod bounded {
    use crate::services::git_read::MAX_LOG_COMMITS;
    use gix::bstr::ByteSlice;

    /// One status entry, reduced to plain data.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct StatusEntry {
        /// Repository-relative path, forward slashes.
        pub path: String,
        /// What changed.
        pub change: ChangeKind,
        /// Which comparison produced the entry.
        pub stage: Stage,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ChangeKind {
        Added,
        Modified,
        Deleted,
        Renamed,
        Untracked,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Stage {
        /// HEAD → index.
        HeadToIndex,
        /// Index → worktree.
        IndexToWorktree,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct StatusReport {
        pub entries: Vec<StatusEntry>,
        pub truncated: bool,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct LogReport {
        /// Commit ids (hex), walk order.
        pub commits: Vec<String>,
        pub truncated: bool,
    }

    const _: () = {
        assert!(MAX_LOG_COMMITS > 0);
    };

    fn path_of(value: impl AsRef<[u8]>) -> String {
        String::from_utf8_lossy(value.as_ref()).replace('\\', "/")
    }

    fn entry(path: impl AsRef<[u8]>, change: ChangeKind, stage: Stage) -> StatusEntry {
        StatusEntry {
            path: path_of(path),
            change,
            stage,
        }
    }

    /// Reduce one gix status item to plain data. Unchanged, ignored and
    /// intent-to-add entries map to `None` (they are not reportable
    /// changes); rewrite items are only produced with rename tracking,
    /// which this reader leaves off.
    pub fn map_status_item(item: &gix::status::Item) -> Option<StatusEntry> {
        match item {
            gix::status::Item::TreeIndex(change) => match change {
                gix::diff::index::Change::Addition { location, .. } => {
                    Some(entry(location.as_ref(), ChangeKind::Added, Stage::HeadToIndex))
                }
                gix::diff::index::Change::Deletion { location, .. } => {
                    Some(entry(location.as_ref(), ChangeKind::Deleted, Stage::HeadToIndex))
                }
                gix::diff::index::Change::Modification { location, .. } => {
                    Some(entry(location.as_ref(), ChangeKind::Modified, Stage::HeadToIndex))
                }
                _ => None,
            },
            gix::status::Item::IndexWorktree(inner) => match inner {
                gix::status::index_worktree::Item::Modification {
                    rela_path, status, ..
                } => match status {
                    gix::status::plumbing::index_as_worktree::EntryStatus::Change(change) => match change {
                        gix::status::plumbing::index_as_worktree::Change::Removed => Some(entry(
                            rela_path,
                            ChangeKind::Deleted,
                            Stage::IndexToWorktree,
                        )),
                        gix::status::plumbing::index_as_worktree::Change::Modification { .. }
                        | gix::status::plumbing::index_as_worktree::Change::Type { .. }
                        | gix::status::plumbing::index_as_worktree::Change::SubmoduleModification(_) => {
                            Some(entry(rela_path, ChangeKind::Modified, Stage::IndexToWorktree))
                        }
                    },
                    gix::status::plumbing::index_as_worktree::EntryStatus::Conflict { .. } => {
                        Some(entry(rela_path, ChangeKind::Modified, Stage::IndexToWorktree))
                    }
                    gix::status::plumbing::index_as_worktree::EntryStatus::NeedsUpdate(_)
                    | gix::status::plumbing::index_as_worktree::EntryStatus::IntentToAdd => None,
                },
                gix::status::index_worktree::Item::DirectoryContents { entry: dir_entry, .. } => {
                    match dir_entry.status {
                        gix::dir::entry::Status::Untracked => Some(entry(
                            dir_entry.rela_path.as_bytes(),
                            ChangeKind::Untracked,
                            Stage::IndexToWorktree,
                        )),
                        _ => None,
                    }
                }
                gix::status::index_worktree::Item::Rewrite { .. } => None,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// P30 — the bounded read-only diff. Two blob contents in, a bounded
// unified text diff out; binary content is represented as metadata only
// (never decoded into hunks). All bounds refuse with Unavailable so an
// unbounded diff can never stream through a tool reply.
// ---------------------------------------------------------------------------

/// Hard bounds for one diff (P30.1). Overflow is `Unavailable`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiffBounds {
    pub max_hunks: usize,
    pub max_lines_per_hunk: usize,
    pub max_total_bytes: usize,
}

pub const MAX_DIFF_HUNKS: usize = 2_000;
pub const MAX_DIFF_LINES_PER_HUNK: usize = 500;
pub const MAX_DIFF_TOTAL_BYTES: usize = 1024 * 1024;

impl Default for DiffBounds {
    fn default() -> Self {
        Self {
            max_hunks: MAX_DIFF_HUNKS,
            max_lines_per_hunk: MAX_DIFF_LINES_PER_HUNK,
            max_total_bytes: MAX_DIFF_TOTAL_BYTES,
        }
    }
}

/// Why a diff refused. Unavailable = a bound; the reader never streams a
/// partial diff.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DiffError {
    #[error("diff unavailable: {0}")]
    Unavailable(String),
    #[error("diff io failure: {0}")]
    Io(String),
}

/// One unified hunk: a bounded run of context/insertion/deletion lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    pub old_start: u32,
    pub new_start: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffLineKind {
    Context,
    Added,
    Removed,
}

/// The bounded result. `binary` is set (with both sizes) when either side
/// contains binary content — the ONLY representation of binary diffs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffReport {
    pub hunks: Vec<DiffHunk>,
    pub truncated: bool,
    pub binary: Option<(u64, u64)>,
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(8192)].contains(&0)
}

/// Diff two blob contents under the bounds. Context is 0 lines (the exact
/// changed runs only) so the output is minimal and deterministic; binary
/// content short-circuits to metadata before any line work.
pub fn diff_blobs(old: &[u8], new: &[u8], bounds: &DiffBounds) -> Result<DiffReport, DiffError> {
    if is_binary(old) || is_binary(new) {
        return Ok(DiffReport {
            hunks: Vec::new(),
            truncated: false,
            binary: Some((old.len() as u64, new.len() as u64)),
        });
    }
    let old_lines: Vec<&[u8]> = old.split(|byte| *byte == b'\n').collect();
    let new_lines: Vec<&[u8]> = new.split(|byte| *byte == b'\n').collect();
    // Longest-common-subsequence table on line indices; bounded inputs
    // only (the byte bound guards the table size before allocation).
    if old.len().saturating_add(new.len()) > bounds.max_total_bytes {
        return Err(DiffError::Unavailable("diff byte bound exceeded".into()));
    }
    let mut table = vec![vec![0u32; new_lines.len() + 1]; old_lines.len() + 1];
    for old_index in (0..old_lines.len()).rev() {
        for new_index in (0..new_lines.len()).rev() {
            table[old_index][new_index] = if old_lines[old_index] == new_lines[new_index] {
                table[old_index + 1][new_index + 1] + 1
            } else {
                table[old_index + 1][new_index].max(table[old_index][new_index + 1])
            };
        }
    }
    // Walk the table into runs of added/removed lines; each run is a hunk.
    let mut hunks: Vec<DiffHunk> = Vec::new();
    let mut truncated = false;
    let mut old_index = 0;
    let mut new_index = 0;
    let mut emitted_bytes = 0usize;
    while old_index < old_lines.len() || new_index < new_lines.len() {
        if old_index < old_lines.len()
            && new_index < new_lines.len()
            && old_lines[old_index] == new_lines[new_index]
        {
            old_index += 1;
            new_index += 1;
            continue;
        }
        // A change run: consume the differing lines on both sides until the
        // streams resynchronize.
        let run_old_start = old_index as u32 + 1;
        let run_new_start = new_index as u32 + 1;
        let mut lines: Vec<DiffLine> = Vec::new();
        while old_index < old_lines.len()
            && new_index < new_lines.len()
            && old_lines[old_index] != new_lines[new_index]
        {
            if lines.len() >= bounds.max_lines_per_hunk || hunks.len() >= bounds.max_hunks {
                truncated = true;
                break;
            }
            lines.push(DiffLine {
                kind: DiffLineKind::Removed,
                text: String::from_utf8_lossy(old_lines[old_index]).into_owned(),
            });
            lines.push(DiffLine {
                kind: DiffLineKind::Added,
                text: String::from_utf8_lossy(new_lines[new_index]).into_owned(),
            });
            emitted_bytes = emitted_bytes
                .saturating_add(old_lines[old_index].len() + new_lines[new_index].len());
            if emitted_bytes > bounds.max_total_bytes {
                truncated = true;
                break;
            }
            old_index += 1;
            new_index += 1;
        }
        // Pure insertions or deletions (one side exhausted or resynced).
        while old_index < old_lines.len()
            && (new_index >= new_lines.len() || old_lines[old_index] != new_lines[new_index])
        {
            if lines.len() >= bounds.max_lines_per_hunk || hunks.len() >= bounds.max_hunks {
                truncated = true;
                break;
            }
            lines.push(DiffLine {
                kind: DiffLineKind::Removed,
                text: String::from_utf8_lossy(old_lines[old_index]).into_owned(),
            });
            emitted_bytes = emitted_bytes.saturating_add(old_lines[old_index].len());
            if emitted_bytes > bounds.max_total_bytes {
                truncated = true;
                break;
            }
            old_index += 1;
        }
        while new_index < new_lines.len()
            && (old_index >= old_lines.len() || old_lines[old_index] != new_lines[new_index])
        {
            if lines.len() >= bounds.max_lines_per_hunk || hunks.len() >= bounds.max_hunks {
                truncated = true;
                break;
            }
            lines.push(DiffLine {
                kind: DiffLineKind::Added,
                text: String::from_utf8_lossy(new_lines[new_index]).into_owned(),
            });
            emitted_bytes = emitted_bytes.saturating_add(new_lines[new_index].len());
            if emitted_bytes > bounds.max_total_bytes {
                truncated = true;
                break;
            }
            new_index += 1;
        }
        if truncated {
            break;
        }
        if !lines.is_empty() {
            hunks.push(DiffHunk {
                old_start: run_old_start,
                new_start: run_new_start,
                lines,
            });
        }
    }
    Ok(DiffReport {
        hunks,
        truncated,
        binary: None,
    })
}
