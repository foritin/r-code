//! GitService -- Git 仓库操作封装。 [doc-12 §1]
//!
//! 使用 `git` CLI（而非 `git2`/libgit2 绑定）实现，避免原生依赖，提升可移植性。
//!
//! ## 核心职责
//! - **detect**：检测路径是否位于 git 仓库内
//! - **status**：获取工作区状态（porcelain v2 格式，解析为 `GitFileStatus`）
//! - **diff**：获取文件差异（`git diff HEAD`，含已暂存与未暂存）
//! - **stage/unstage/discard**：暂存 / 取消暂存 / 丢弃更改
//! - **commit**：提交暂存的更改
//! - **branch**：分支创建 / 当前分支查询
//! - **worktree**：worktree 管理（每任务独立 worktree）
//! - **entry_snapshot**：临时索引 + `git write-tree` 生成快照 tree SHA
//!
//! 所有操作通过 `std::process::Command` 同步调用 `git` CLI。
//! 如需在异步上下文中使用，应通过 `tokio::task::spawn_blocking` 包装。
//!
//! [doc-12 §1] [doc-06 §3.7]

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use r_code_core::error::ProductError;
use r_code_core::process::hide_background_console;

/// GitService -- 封装 `git` CLI 进行仓库操作。
pub struct GitService {
    repo_path: PathBuf,
}

/// Git 状态条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitFileStatus {
    /// 文件路径（相对于仓库根）
    pub path: String,
    /// 状态种类
    pub status: GitStatusKind,
}

/// Git 状态种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitStatusKind {
    /// 未跟踪
    Untracked,
    /// 已修改
    Modified,
    /// 已暂存新增
    Added,
    /// 已删除
    Deleted,
    /// 已重命名
    Renamed,
    /// 冲突（未合并）
    Conflicted,
}

/// Git diff 结果。
#[derive(Debug, Clone)]
pub struct GitDiffResult {
    /// 文件路径
    pub path: String,
    /// diff 文本
    pub diff: String,
}

/// A path changed between two Git tree objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitTreeChange {
    pub path: String,
    pub kind: GitTreeChangeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitTreeChangeKind {
    Added,
    Modified,
    Deleted,
}

impl GitService {
    /// 创建 GitService，绑定到指定仓库路径。
    pub fn new(repo_path: PathBuf) -> Self {
        Self { repo_path }
    }

    /// 检测路径是否位于 git 仓库内。
    ///
    /// 使用 `git rev-parse --git-dir`：退出码 0 表示在仓库内，
    /// 非零表示不在仓库内。若 `git` 未安装或路径不存在，返回错误 / `false`。
    pub fn detect(path: &Path) -> Result<bool, ProductError> {
        if !path.exists() {
            return Ok(false);
        }
        let mut command = Command::new("git");
        command
            .current_dir(path)
            .args(["rev-parse", "--git-dir"])
            .env("GIT_TERMINAL_PROMPT", "0");
        hide_background_console(&mut command);
        let output = command
            .output()
            .map_err(|e| ProductError::GitError(format!("failed to execute git: {e}")))?;
        Ok(output.status.success())
    }

    /// 获取仓库根路径（`git rev-parse --show-toplevel`）。
    pub fn repo_root(&self) -> Result<PathBuf, ProductError> {
        let out = self.run_git(&["rev-parse", "--show-toplevel"])?;
        Ok(PathBuf::from(out.trim()))
    }

    /// 获取工作区状态（porcelain v2 格式，解析为 `GitFileStatus` 列表）。
    ///
    /// 包含未跟踪文件（`--untracked-files=all`）。
    /// 禁用重命名检测（`--no-renames`），重命名以 delete+add 形式呈现，
    /// 简化解析。
    pub fn status(&self) -> Result<Vec<GitFileStatus>, ProductError> {
        let bytes = self.run_git_bytes(&[
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=all",
            "--no-renames",
        ])?;
        Ok(parse_porcelain_v2(&bytes))
    }

    /// 获取文件差异。
    ///
    /// - `path = Some(p)`：仅返回指定文件的 diff。
    /// - `path = None`：返回所有已更改文件的 diff。
    ///
    /// 使用 `git diff HEAD`，显示所有未提交的更改（含已暂存和未暂存）。
    pub fn diff(&self, path: Option<&str>) -> Result<Vec<GitDiffResult>, ProductError> {
        match path {
            Some(p) => {
                let diff_text = self.run_git(&["diff", "HEAD", "--", p])?;
                if diff_text.trim().is_empty() {
                    Ok(Vec::new())
                } else {
                    Ok(vec![GitDiffResult {
                        path: p.to_string(),
                        diff: diff_text,
                    }])
                }
            }
            None => {
                // 先获取已更改文件列表（-z 处理含空格路径），再逐文件获取 diff
                let names_bytes = self.run_git_bytes(&["diff", "HEAD", "--name-only", "-z"])?;
                let names: Vec<String> = names_bytes
                    .split(|&b| b == 0)
                    .filter(|s| !s.is_empty())
                    .map(|s| String::from_utf8_lossy(s).into_owned())
                    .collect();
                let mut results = Vec::with_capacity(names.len());
                for name in &names {
                    let diff_text = self.run_git(&["diff", "HEAD", "--", name])?;
                    if !diff_text.trim().is_empty() {
                        results.push(GitDiffResult {
                            path: name.clone(),
                            diff: diff_text,
                        });
                    }
                }
                Ok(results)
            }
        }
    }

    /// 暂存文件（`git add -- <path>`）。
    pub fn stage(&self, path: &str) -> Result<(), ProductError> {
        self.run_git(&["add", "--", path])?;
        Ok(())
    }

    /// Stage several explicitly selected paths in one Git process.
    pub fn stage_paths(&self, paths: &[String]) -> Result<(), ProductError> {
        if paths.is_empty() {
            return Ok(());
        }
        // Passing every path as a process argument hits Windows' command-line length limit on
        // larger reviews. NUL-delimited stdin is one Git process, preserves odd filenames, and
        // keeps pathspec interpretation disabled.
        let mut input = Vec::new();
        for path in paths {
            input.extend_from_slice(path.as_bytes());
            input.push(0);
        }
        let args = [
            "--literal-pathspecs",
            "add",
            "--pathspec-from-file=-",
            "--pathspec-file-nul",
        ];
        let mut command = self.git_command(&args);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| ProductError::GitError(format!("failed to execute git: {error}")))?;
        child
            .stdin
            .take()
            .ok_or_else(|| ProductError::GitError("git stdin was not available".into()))?
            .write_all(&input)
            .map_err(|error| {
                ProductError::GitError(format!("failed to write git stdin: {error}"))
            })?;
        let output = child
            .wait_with_output()
            .map_err(|error| ProductError::GitError(format!("failed to wait for git: {error}")))?;
        check_git_success(&args, &output)?;
        Ok(())
    }

    /// Resolve repository-relative paths ignored by Git in one process.
    ///
    /// `--no-index` intentionally also applies ignore rules to an accidentally tracked
    /// generated file. Review uses this only as an exclusion filter; it never changes Git state.
    pub fn ignored_paths(&self, paths: &[String]) -> Result<BTreeSet<String>, ProductError> {
        if paths.is_empty() {
            return Ok(BTreeSet::new());
        }
        let mut input = Vec::new();
        for path in paths {
            input.extend_from_slice(path.as_bytes());
            input.push(0);
        }
        let mut command = self.git_command(&["check-ignore", "--no-index", "-z", "--stdin"]);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| ProductError::GitError(format!("failed to execute git: {error}")))?;
        child
            .stdin
            .take()
            .ok_or_else(|| ProductError::GitError("git stdin was not available".into()))?
            .write_all(&input)
            .map_err(|error| {
                ProductError::GitError(format!("failed to write git stdin: {error}"))
            })?;
        let output = child
            .wait_with_output()
            .map_err(|error| ProductError::GitError(format!("failed to wait for git: {error}")))?;
        match output.status.code() {
            Some(0) | Some(1) => Ok(output
                .stdout
                .split(|byte| *byte == 0)
                .filter(|path| !path.is_empty())
                .map(|path| String::from_utf8_lossy(path).replace('\\', "/"))
                .collect()),
            _ => {
                check_git_success(&["check-ignore", "--no-index", "-z", "--stdin"], &output)?;
                Ok(BTreeSet::new())
            }
        }
    }

    /// Add an intent-to-add index entry so an untracked text file can be partially staged.
    pub fn intent_to_add(&self, path: &str) -> Result<(), ProductError> {
        self.run_git(&["add", "-N", "--", path])?;
        Ok(())
    }

    /// Return the current index-to-worktree patch for one path.
    pub fn worktree_patch(&self, path: &str) -> Result<String, ProductError> {
        self.run_git(&[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--unified=0",
            "--",
            path,
        ])
    }

    /// Apply a patch to the real index without changing the worktree.
    pub fn apply_cached_patch(&self, patch: &[u8]) -> Result<(), ProductError> {
        self.run_git_with_stdin(
            &[
                "apply",
                "--cached",
                "--unidiff-zero",
                "--whitespace=nowarn",
                "-",
            ],
            patch,
        )?;
        Ok(())
    }

    /// Whether a path currently has staged content relative to HEAD.
    pub fn has_staged_change(&self, path: &str) -> Result<bool, ProductError> {
        Ok(!self.git_exit_success(&["diff", "--cached", "--quiet", "--", path])?)
    }

    /// Whether a tracked or intent-to-add path differs between index and worktree.
    pub fn has_worktree_change(&self, path: &str) -> Result<bool, ProductError> {
        Ok(!self.git_exit_success(&["diff", "--quiet", "--", path])?)
    }

    /// Whether a path has unresolved index stages.
    pub fn has_conflict(&self, path: &str) -> Result<bool, ProductError> {
        Ok(!self
            .run_git_bytes(&["ls-files", "--unmerged", "-z", "--", path])?
            .is_empty())
    }

    /// Whether a path has any entry in the index (including intent-to-add).
    pub fn is_indexed(&self, path: &str) -> Result<bool, ProductError> {
        Ok(!self
            .run_git_bytes(&["ls-files", "--stage", "-z", "--", path])?
            .is_empty())
    }

    /// 取消暂存文件。
    ///
    /// 若 HEAD 存在，使用 `git reset HEAD -- <path>`；
    /// 若仓库尚无提交，使用 `git rm --cached -- <path>`。
    pub fn unstage(&self, path: &str) -> Result<(), ProductError> {
        let has_head = self
            .run_git(&["rev-parse", "--verify", "-q", "HEAD"])
            .is_ok();
        if has_head {
            self.run_git(&["reset", "-q", "HEAD", "--", path])?;
        } else {
            self.run_git(&["rm", "--cached", "-q", "--", path])?;
        }
        Ok(())
    }

    /// 丢弃文件更改（从 HEAD 检出，`git checkout HEAD -- <path>`）。
    ///
    /// 同时重置暂存区和工作区到 HEAD 状态。仅适用于已跟踪文件；
    /// 未跟踪文件需调用方自行删除。
    pub fn discard(&self, path: &str) -> Result<(), ProductError> {
        self.run_git(&["checkout", "HEAD", "--", path])?;
        Ok(())
    }

    /// 提交暂存的更改。
    ///
    /// 返回新提交的 SHA。
    pub fn commit(&self, message: &str) -> Result<String, ProductError> {
        self.run_git(&["commit", "-m", message])?;
        let sha = self.run_git(&["rev-parse", "HEAD"])?;
        Ok(sha.trim().to_string())
    }

    /// Paths currently staged relative to HEAD.
    pub fn staged_paths(&self) -> Result<Vec<String>, ProductError> {
        let bytes = self.run_git_bytes(&["diff", "--cached", "--name-only", "-z", "--"])?;
        Ok(bytes
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| String::from_utf8_lossy(path).into_owned())
            .collect())
    }

    /// The configured upstream of the current branch, if one exists.
    pub fn upstream(&self) -> Result<Option<String>, ProductError> {
        match self.run_git(&[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ]) {
            Ok(value) => Ok(Some(value.trim().to_string())),
            Err(ProductError::GitError(_)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Return (ahead, behind) relative to the configured upstream.
    pub fn ahead_behind(&self) -> Result<Option<(usize, usize)>, ProductError> {
        if self.upstream()?.is_none() {
            return Ok(None);
        }
        let value = self.run_git(&["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])?;
        let mut counts = value.split_whitespace();
        let ahead = counts
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        let behind = counts
            .next()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        Ok(Some((ahead, behind)))
    }

    /// Push the current branch to its already configured upstream. No refspec or force flag
    /// is supplied, so Git's normal branch protection and non-fast-forward checks remain active.
    pub fn push_upstream(&self) -> Result<String, ProductError> {
        if self.upstream()?.is_none() {
            return Err(ProductError::GitError(
                "当前分支没有 upstream，请先在终端显式设置远端跟踪分支".into(),
            ));
        }
        self.run_git(&["push"])?;
        Ok(self.run_git(&["rev-parse", "HEAD"])?.trim().to_string())
    }

    /// 创建新分支（`git branch <name>`）。不切换到该分支。
    pub fn create_branch(&self, name: &str) -> Result<(), ProductError> {
        self.run_git(&["branch", name])?;
        Ok(())
    }

    /// 获取当前分支名（`git symbolic-ref --short HEAD`）。
    ///
    /// 在 detached HEAD 状态下返回错误。
    pub fn current_branch(&self) -> Result<String, ProductError> {
        let out = self.run_git(&["symbolic-ref", "--short", "HEAD"])?;
        Ok(out.trim().to_string())
    }

    pub fn head_tree(&self) -> Result<Option<String>, ProductError> {
        if !Self::detect(&self.repo_path)? {
            return Ok(None);
        }
        match self.run_git(&["rev-parse", "--verify", "HEAD^{tree}"]) {
            Ok(tree) => Ok(Some(tree.trim().to_string())),
            Err(ProductError::GitError(_)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// 解析任意 revision 为完整 SHA（例如 `HEAD` 或 `<checkpoint>^`）。
    pub fn rev_parse(&self, spec: &str) -> Result<String, ProductError> {
        let out = self.run_git(&["rev-parse", "--verify", spec])?;
        let sha = out.trim();
        if sha.is_empty() {
            return Err(ProductError::GitError(format!(
                "git rev-parse {spec} returned an empty value"
            )));
        }
        Ok(sha.to_string())
    }

    /// 把仓库硬重置到指定 commit。调用方必须已经完成外部 HEAD 移动校验；
    /// untracked 文件不受影响。
    pub fn reset_hard(&self, target: &str) -> Result<(), ProductError> {
        self.run_git(&["reset", "--hard", target]).map(|_| ())
    }

    /// Snapshot the real Git index without reading or changing the worktree.
    pub fn index_snapshot(&self) -> Result<Option<String>, ProductError> {
        if !Self::detect(&self.repo_path)? {
            return Ok(None);
        }
        let tree = self.run_git(&["write-tree"])?;
        Ok(Some(tree.trim().to_string()))
    }

    /// List net path changes between two tree objects. Rename detection is intentionally
    /// disabled so downstream review only needs create/modify/delete semantics.
    pub fn tree_changes(
        &self,
        before_tree: &str,
        after_tree: &str,
    ) -> Result<Vec<GitTreeChange>, ProductError> {
        let output = self.run_git_bytes(&[
            "diff",
            "--name-status",
            "-z",
            "--no-renames",
            before_tree,
            after_tree,
            "--",
        ])?;
        let fields: Vec<&[u8]> = output
            .split(|byte| *byte == 0)
            .filter(|field| !field.is_empty())
            .collect();
        let mut changes = Vec::with_capacity(fields.len() / 2);
        for pair in fields.as_chunks::<2>().0 {
            let status = pair[0].first().copied();
            let kind = match status {
                Some(b'A') => GitTreeChangeKind::Added,
                Some(b'D') => GitTreeChangeKind::Deleted,
                Some(b'M') | Some(b'T') => GitTreeChangeKind::Modified,
                _ => continue,
            };
            changes.push(GitTreeChange {
                path: String::from_utf8_lossy(pair[1]).into_owned(),
                kind,
            });
        }
        Ok(changes)
    }

    /// Read a regular-file blob from a tree. A missing path (or a directory entry) is `None`.
    pub fn blob_at_tree(&self, tree: &str, path: &str) -> Result<Option<Vec<u8>>, ProductError> {
        let listing = self.run_git_bytes(&["ls-tree", "-z", tree, "--", path])?;
        let Some(record) = listing.split(|byte| *byte == 0).find(|row| !row.is_empty()) else {
            return Ok(None);
        };
        let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
            return Ok(None);
        };
        let header = String::from_utf8_lossy(&record[..tab]);
        let mut parts = header.split_whitespace();
        let _mode = parts.next();
        if parts.next() != Some("blob") {
            return Ok(None);
        }
        let Some(object_id) = parts.next() else {
            return Ok(None);
        };
        self.run_git_bytes(&["cat-file", "blob", object_id])
            .map(Some)
    }

    /// 为任务创建 git worktree。
    ///
    /// 执行 `git worktree add -b r-code/<task_id> <worktree_path> HEAD`。
    /// 需要仓库至少有一个提交。
    pub fn create_worktree(&self, task_id: &str, worktree_path: &Path) -> Result<(), ProductError> {
        let branch = format!("r-code/{task_id}");
        let path_str = worktree_path.to_string_lossy();
        self.run_git(&["worktree", "add", "-b", &branch, &path_str, "HEAD"])?;
        Ok(())
    }

    /// 移除 worktree（`git worktree remove <path>`）。
    pub fn remove_worktree(&self, worktree_path: &Path) -> Result<(), ProductError> {
        let path_str = worktree_path.to_string_lossy();
        self.run_git(&["worktree", "remove", &path_str])?;
        Ok(())
    }

    /// 使用临时索引 + `git write-tree` 创建入口快照。
    ///
    /// 流程：`GIT_INDEX_FILE=$(tempfile) git read-tree HEAD; git add -A; git write-tree`。
    /// 返回 tree SHA。
    ///
    /// 对于非 git 项目，返回 `None`（降级模式）。
    pub fn entry_snapshot(&self) -> Result<Option<String>, ProductError> {
        // 非 git 项目 -> 降级模式
        if !Self::detect(&self.repo_path)? {
            return Ok(None);
        }

        // 创建临时索引文件（NamedTempFile 在 drop 时自动清理）
        let tmp = tempfile::NamedTempFile::new().map_err(|e| {
            ProductError::GitError(format!("failed to create temp index file: {e}"))
        })?;
        let index_path = tmp.path().to_string_lossy().into_owned();
        let env = [("GIT_INDEX_FILE", index_path.as_str())];

        // 若 HEAD 存在，先读入 HEAD tree（使快照能正确反映删除）
        let has_head = self
            .run_git(&["rev-parse", "--verify", "-q", "HEAD"])
            .is_ok();
        if has_head {
            self.run_git_with_env(&["read-tree", "HEAD"], &env)?;
        }

        // 暂存所有工作区文件
        self.run_git_with_env(&["add", "-A"], &env)?;

        // 写入 tree 并返回 SHA
        let tree_sha = self.run_git_with_env(&["write-tree"], &env)?;
        Ok(Some(tree_sha.trim().to_string()))
    }

    // ========================================================================
    // 内部辅助
    // ========================================================================

    /// 构建 git 命令（设置 current_dir 和通用环境变量）。
    fn git_command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("git");
        cmd.current_dir(&self.repo_path);
        cmd.args(args);
        cmd.env("GIT_TERMINAL_PROMPT", "0");
        hide_background_console(&mut cmd);
        cmd
    }

    /// 运行 git 命令，返回 stdout 字符串。
    fn run_git(&self, args: &[&str]) -> Result<String, ProductError> {
        let output = self
            .git_command(args)
            .output()
            .map_err(|e| ProductError::GitError(format!("failed to execute git: {e}")))?;
        check_git_success(args, &output)?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// 运行 git 命令，返回 stdout 原始字节（用于 -z 输出）。
    fn run_git_bytes(&self, args: &[&str]) -> Result<Vec<u8>, ProductError> {
        let output = self
            .git_command(args)
            .output()
            .map_err(|e| ProductError::GitError(format!("failed to execute git: {e}")))?;
        check_git_success(args, &output)?;
        Ok(output.stdout)
    }

    /// 运行 git 命令（带额外环境变量），返回 stdout 字符串。
    fn run_git_with_env(
        &self,
        args: &[&str],
        env: &[(&str, &str)],
    ) -> Result<String, ProductError> {
        let mut cmd = self.git_command(args);
        for &(k, v) in env {
            cmd.env(k, v);
        }
        let output = cmd
            .output()
            .map_err(|e| ProductError::GitError(format!("failed to execute git: {e}")))?;
        check_git_success(args, &output)?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn git_exit_success(&self, args: &[&str]) -> Result<bool, ProductError> {
        let output = self
            .git_command(args)
            .output()
            .map_err(|e| ProductError::GitError(format!("failed to execute git: {e}")))?;
        match output.status.code() {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => {
                check_git_success(args, &output)?;
                Ok(true)
            }
        }
    }

    fn run_git_with_stdin(&self, args: &[&str], input: &[u8]) -> Result<(), ProductError> {
        let mut command = self.git_command(args);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|e| ProductError::GitError(format!("failed to execute git: {e}")))?;
        child
            .stdin
            .take()
            .ok_or_else(|| ProductError::GitError("git stdin was not available".into()))?
            .write_all(input)
            .map_err(|e| ProductError::GitError(format!("failed to write git stdin: {e}")))?;
        let output = child
            .wait_with_output()
            .map_err(|e| ProductError::GitError(format!("failed to wait for git: {e}")))?;
        check_git_success(args, &output)
    }
}

// ============================================================================
// porcelain v2 解析
// ============================================================================

/// 检查 git 退出码，非零则返回包含 stderr 的错误。
fn check_git_success(args: &[&str], output: &std::process::Output) -> Result<(), ProductError> {
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(ProductError::GitError(format!(
            "git {} failed: {}",
            args.join(" "),
            stderr.trim()
        )))
    } else {
        Ok(())
    }
}

/// 解析 `git status --porcelain=v2 -z` 输出。
///
/// 条目类型：
/// - `1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>` - 普通已跟踪
/// - `2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path>` + NUL `<origPath>` - 重命名
/// - `? <path>` - 未跟踪
/// - `u <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>` - 未合并
/// - `# branch.*` - 分支头信息（跳过）
///
/// XY 固定 2 字符（可能含空格），通过字节位置 `line[2..4]` 提取以避免
/// 空格分割歧义。path 是最后一个字段，可能含空格（用 `join(" ")` 还原）。
fn parse_porcelain_v2(output: &[u8]) -> Vec<GitFileStatus> {
    let mut results = Vec::new();
    let pieces: Vec<&[u8]> = output.split(|&b| b == 0).collect();
    let mut i = 0;
    while i < pieces.len() {
        let piece = pieces[i];
        if piece.is_empty() {
            i += 1;
            continue;
        }
        let line = String::from_utf8_lossy(piece);

        // 跳过分支头信息
        if line.starts_with('#') {
            i += 1;
            continue;
        }
        if line.len() < 3 {
            i += 1;
            continue;
        }

        match piece[0] {
            // 普通已跟踪条目
            b'1' => {
                // "1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>"
                // line[0]='1', line[1]=' ', line[2..4]=XY, line[4]=' ', line[5..]=rest
                if line.len() < 5 {
                    i += 1;
                    continue;
                }
                let xy = &line[2..4];
                let rest = &line[5..];
                // rest = "sub mH mI mW hH hI path" -> path 是第 7 个字段（索引 6）
                let parts: Vec<&str> = rest.split(' ').collect();
                if parts.len() >= 7 {
                    let path = parts[6..].join(" ");
                    results.push(GitFileStatus {
                        path,
                        status: map_xy(xy),
                    });
                }
                i += 1;
            }
            // 重命名/拷贝条目（--no-renames 时不会出现，但防御性处理）
            b'2' => {
                // "2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path>"
                // 随后 NUL 分隔的 <origPath> 也占一个 piece
                if line.len() >= 5 {
                    let xy = &line[2..4];
                    let rest = &line[5..];
                    // rest = "sub mH mI mW hH hI Xscore path" -> path 是第 8 个字段（索引 7）
                    let parts: Vec<&str> = rest.split(' ').collect();
                    if parts.len() >= 8 {
                        let path = parts[7..].join(" ");
                        results.push(GitFileStatus {
                            path,
                            status: map_xy(xy),
                        });
                    }
                }
                i += 2; // 跳过 origPath piece
            }
            // 未跟踪条目
            b'?' => {
                // "? <path>"
                let path = &line[2..];
                results.push(GitFileStatus {
                    path: path.to_string(),
                    status: GitStatusKind::Untracked,
                });
                i += 1;
            }
            // 未合并条目
            b'u' => {
                // "u <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>"
                if line.len() < 5 {
                    i += 1;
                    continue;
                }
                let rest = &line[5..];
                // rest = "sub m1 m2 m3 mW h1 h2 h3 path" -> path 是第 9 个字段（索引 8）
                let parts: Vec<&str> = rest.split(' ').collect();
                if parts.len() >= 9 {
                    let path = parts[8..].join(" ");
                    results.push(GitFileStatus {
                        path,
                        status: GitStatusKind::Conflicted,
                    });
                }
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }
    results
}

/// 将 porcelain 的 XY 状态码映射为 `GitStatusKind`。
///
/// X = 暂存区状态（index vs HEAD），Y = 工作区状态（workdir vs index）。
/// 优先报告暂存区状态（X），其次报告工作区状态（Y）。
fn map_xy(xy: &str) -> GitStatusKind {
    let mut chars = xy.chars();
    let x = chars.next().unwrap_or(' ');
    let y = chars.next().unwrap_or(' ');

    // 未跟踪（不应出现在 type 1 条目中，但防御性处理）
    if x == '?' || y == '?' {
        return GitStatusKind::Untracked;
    }

    // 未合并（冲突）
    if x == 'U' || y == 'U' || (x == 'A' && y == 'A') || (x == 'D' && y == 'D') {
        return GitStatusKind::Conflicted;
    }

    // 优先报告暂存区状态（X）
    match x {
        'A' => return GitStatusKind::Added,
        'D' => return GitStatusKind::Deleted,
        'R' | 'C' => return GitStatusKind::Renamed,
        'M' => return GitStatusKind::Modified,
        _ => {}
    }

    // 其次报告工作区状态（Y）
    match y {
        'M' => GitStatusKind::Modified,
        'D' => GitStatusKind::Deleted,
        'A' => GitStatusKind::Added,
        _ => GitStatusKind::Modified,
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::{GitDiffResult, GitFileStatus, GitService, GitStatusKind};
    use std::collections::HashMap;
    use std::path::Path;
    use tempfile::TempDir;

    /// 创建测试 git 仓库（含 local config，可立即提交）。
    fn create_test_repo() -> TempDir {
        let tmp = TempDir::new().unwrap();
        let repo_path = tmp.path();

        git_run(repo_path, &["init", "-b", "main"]);
        git_run(repo_path, &["config", "user.email", "test@r-code.dev"]);
        git_run(repo_path, &["config", "user.name", "Test User"]);
        git_run(repo_path, &["config", "commit.gpgsign", "false"]);
        // 关闭 CRLF 转换：Windows 上全局 core.autocrlf=true 会把检出的 \n 转成 \r\n，
        // 测试断言按 \n 写的内容必须原样读回
        git_run(repo_path, &["config", "core.autocrlf", "false"]);

        tmp
    }

    /// 在指定目录运行 git（测试辅助）。
    fn git_run(repo_path: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .current_dir(repo_path)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .unwrap_or_else(|e| panic!("failed to run git: {e}"));
        if !output.status.success() {
            panic!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// 将状态列表转为 path -> kind 的 map。
    fn status_map(statuses: &[GitFileStatus]) -> HashMap<String, GitStatusKind> {
        statuses
            .iter()
            .map(|s| (s.path.clone(), s.status))
            .collect()
    }

    #[test]
    fn test_detect_git_repo() {
        let tmp = create_test_repo();
        assert!(GitService::detect(tmp.path()).unwrap());
    }

    #[test]
    fn test_detect_non_git() {
        let tmp = TempDir::new().unwrap();
        // 写入一个文件确保目录存在但非 git
        std::fs::write(tmp.path().join("file.txt"), "hi").unwrap();
        assert!(!GitService::detect(tmp.path()).unwrap());
    }

    #[test]
    fn test_detect_nonexistent_path() {
        assert!(!GitService::detect(Path::new("/nonexistent/path/xyz")).unwrap());
    }

    #[test]
    fn test_repo_root() {
        let tmp = create_test_repo();
        let svc = GitService::new(tmp.path().to_path_buf());
        let root = svc.repo_root().unwrap();
        // Windows 上 git 输出 mingw 风格路径（C:/...），canonicalize 后与 tempdir 的
        // verbatim 前缀（\\?\C:\...）对齐再比较
        assert_eq!(
            root.canonicalize().unwrap(),
            tmp.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn test_current_branch_no_commits() {
        let tmp = create_test_repo();
        let svc = GitService::new(tmp.path().to_path_buf());
        let branch = svc.current_branch().unwrap();
        assert_eq!(branch, "main");
    }

    #[test]
    fn test_status_empty_repo() {
        let tmp = create_test_repo();
        let svc = GitService::new(tmp.path().to_path_buf());
        let statuses = svc.status().unwrap();
        assert!(
            statuses.is_empty(),
            "expected empty status, got {statuses:?}"
        );
    }

    #[test]
    fn test_status_modified_untracked_staged() {
        let tmp = create_test_repo();
        let repo_path = tmp.path();

        // 初始提交
        std::fs::write(repo_path.join("committed.txt"), "initial\n").unwrap();
        git_run(repo_path, &["add", "committed.txt"]);
        git_run(repo_path, &["commit", "-m", "initial"]);

        // 修改已跟踪文件（未暂存）
        std::fs::write(repo_path.join("committed.txt"), "modified\n").unwrap();

        // 创建未跟踪文件
        std::fs::write(repo_path.join("untracked.txt"), "new\n").unwrap();

        // 创建并暂存新文件
        std::fs::write(repo_path.join("staged.txt"), "staged\n").unwrap();
        git_run(repo_path, &["add", "staged.txt"]);

        let svc = GitService::new(repo_path.to_path_buf());
        let map = status_map(&svc.status().unwrap());

        assert_eq!(map.get("committed.txt"), Some(&GitStatusKind::Modified));
        assert_eq!(map.get("untracked.txt"), Some(&GitStatusKind::Untracked));
        assert_eq!(map.get("staged.txt"), Some(&GitStatusKind::Added));
    }

    #[test]
    fn test_status_deleted() {
        let tmp = create_test_repo();
        let repo_path = tmp.path();

        std::fs::write(repo_path.join("file.txt"), "content\n").unwrap();
        git_run(repo_path, &["add", "file.txt"]);
        git_run(repo_path, &["commit", "-m", "initial"]);

        // 删除文件（未暂存）
        std::fs::remove_file(repo_path.join("file.txt")).unwrap();

        let svc = GitService::new(repo_path.to_path_buf());
        let map = status_map(&svc.status().unwrap());
        assert_eq!(map.get("file.txt"), Some(&GitStatusKind::Deleted));
    }

    #[test]
    fn test_stage_and_unstage_new_file() {
        let tmp = create_test_repo();
        let repo_path = tmp.path();

        // 初始提交（unstage 需要 HEAD）
        std::fs::write(repo_path.join("init.txt"), "init\n").unwrap();
        git_run(repo_path, &["add", "init.txt"]);
        git_run(repo_path, &["commit", "-m", "init"]);

        // 新文件（未跟踪）
        std::fs::write(repo_path.join("new.txt"), "new\n").unwrap();

        let svc = GitService::new(repo_path.to_path_buf());

        // 初始：未跟踪
        let map = status_map(&svc.status().unwrap());
        assert_eq!(map.get("new.txt"), Some(&GitStatusKind::Untracked));

        // 暂存 -> Added
        svc.stage("new.txt").unwrap();
        let map = status_map(&svc.status().unwrap());
        assert_eq!(map.get("new.txt"), Some(&GitStatusKind::Added));

        // 取消暂存 -> Untracked
        svc.unstage("new.txt").unwrap();
        let map = status_map(&svc.status().unwrap());
        assert_eq!(map.get("new.txt"), Some(&GitStatusKind::Untracked));
    }

    #[test]
    fn test_stage_and_unstage_no_head() {
        // 仓库尚无提交时 unstage 应使用 rm --cached
        let tmp = create_test_repo();
        let repo_path = tmp.path();

        std::fs::write(repo_path.join("file.txt"), "content\n").unwrap();

        let svc = GitService::new(repo_path.to_path_buf());

        // 暂存
        svc.stage("file.txt").unwrap();
        let map = status_map(&svc.status().unwrap());
        assert_eq!(map.get("file.txt"), Some(&GitStatusKind::Added));

        // 取消暂存（无 HEAD -> git rm --cached）
        svc.unstage("file.txt").unwrap();
        let map = status_map(&svc.status().unwrap());
        assert_eq!(map.get("file.txt"), Some(&GitStatusKind::Untracked));
    }

    #[test]
    fn test_commit() {
        let tmp = create_test_repo();
        let repo_path = tmp.path();

        std::fs::write(repo_path.join("file.txt"), "content\n").unwrap();

        let svc = GitService::new(repo_path.to_path_buf());
        svc.stage("file.txt").unwrap();

        let sha = svc.commit("test commit").unwrap();
        assert_eq!(sha.len(), 40, "SHA-1 commit hash should be 40 chars");

        // 验证提交信息
        let log = git_run(repo_path, &["log", "-1", "--pretty=%s"]);
        assert_eq!(log.trim(), "test commit");

        // 验证状态干净
        let statuses = svc.status().unwrap();
        assert!(
            statuses.is_empty(),
            "expected clean status, got {statuses:?}"
        );
    }

    #[test]
    fn test_create_branch() {
        let tmp = create_test_repo();
        let repo_path = tmp.path();

        // 分支创建需要至少一个提交
        std::fs::write(repo_path.join("file.txt"), "content\n").unwrap();
        git_run(repo_path, &["add", "file.txt"]);
        git_run(repo_path, &["commit", "-m", "initial"]);

        let svc = GitService::new(repo_path.to_path_buf());
        svc.create_branch("feature-branch").unwrap();

        // 验证分支存在
        let branches = git_run(repo_path, &["branch", "--list"]);
        assert!(
            branches.contains("feature-branch"),
            "expected feature-branch in {branches}"
        );
    }

    #[test]
    fn test_diff_single_file() {
        let tmp = create_test_repo();
        let repo_path = tmp.path();

        // 初始提交
        std::fs::write(repo_path.join("file.txt"), "line1\nline2\nline3\n").unwrap();
        git_run(repo_path, &["add", "file.txt"]);
        git_run(repo_path, &["commit", "-m", "initial"]);

        // 修改文件
        std::fs::write(repo_path.join("file.txt"), "line1\nmodified\nline3\n").unwrap();

        let svc = GitService::new(repo_path.to_path_buf());

        // 指定文件 diff
        let diffs = svc.diff(Some("file.txt")).unwrap();
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].path, "file.txt");
        assert!(
            diffs[0].diff.contains("-line2"),
            "diff should contain -line2: {}",
            diffs[0].diff
        );
        assert!(
            diffs[0].diff.contains("+modified"),
            "diff should contain +modified: {}",
            diffs[0].diff
        );
    }

    #[test]
    fn test_diff_all_files() {
        let tmp = create_test_repo();
        let repo_path = tmp.path();

        // 初始提交
        std::fs::write(repo_path.join("a.txt"), "aaa\n").unwrap();
        std::fs::write(repo_path.join("b.txt"), "bbb\n").unwrap();
        git_run(repo_path, &["add", "-A"]);
        git_run(repo_path, &["commit", "-m", "initial"]);

        // 修改两个文件
        std::fs::write(repo_path.join("a.txt"), "AAA\n").unwrap();
        std::fs::write(repo_path.join("b.txt"), "BBB\n").unwrap();

        let svc = GitService::new(repo_path.to_path_buf());
        let diffs: Vec<GitDiffResult> = svc.diff(None).unwrap();
        assert_eq!(diffs.len(), 2);

        let paths: Vec<&str> = diffs.iter().map(|d| d.path.as_str()).collect();
        assert!(paths.contains(&"a.txt"), "expected a.txt in {paths:?}");
        assert!(paths.contains(&"b.txt"), "expected b.txt in {paths:?}");
    }

    #[test]
    fn test_diff_no_changes() {
        let tmp = create_test_repo();
        let repo_path = tmp.path();

        std::fs::write(repo_path.join("file.txt"), "content\n").unwrap();
        git_run(repo_path, &["add", "file.txt"]);
        git_run(repo_path, &["commit", "-m", "initial"]);

        let svc = GitService::new(repo_path.to_path_buf());
        let diffs = svc.diff(None).unwrap();
        assert!(diffs.is_empty());

        let diffs = svc.diff(Some("file.txt")).unwrap();
        assert!(diffs.is_empty());
    }

    #[test]
    fn test_discard() {
        let tmp = create_test_repo();
        let repo_path = tmp.path();

        // 初始提交
        std::fs::write(repo_path.join("file.txt"), "original\n").unwrap();
        git_run(repo_path, &["add", "file.txt"]);
        git_run(repo_path, &["commit", "-m", "initial"]);

        // 修改文件
        std::fs::write(repo_path.join("file.txt"), "modified\n").unwrap();

        let svc = GitService::new(repo_path.to_path_buf());

        // 丢弃更改
        svc.discard("file.txt").unwrap();

        // 验证文件恢复到原始内容
        let content = std::fs::read_to_string(repo_path.join("file.txt")).unwrap();
        assert_eq!(content, "original\n");

        // 验证状态干净
        let statuses = svc.status().unwrap();
        assert!(
            statuses.is_empty(),
            "expected clean status after discard, got {statuses:?}"
        );
    }

    #[test]
    fn test_create_and_remove_worktree() {
        let tmp = create_test_repo();
        let repo_path = tmp.path();

        // worktree 需要至少一个提交
        std::fs::write(repo_path.join("file.txt"), "content\n").unwrap();
        git_run(repo_path, &["add", "file.txt"]);
        git_run(repo_path, &["commit", "-m", "initial"]);

        let svc = GitService::new(repo_path.to_path_buf());

        // 创建 worktree
        let worktree_path = tmp.path().join("wt-task123");
        svc.create_worktree("task123", &worktree_path).unwrap();

        // 验证 worktree 目录存在
        assert!(worktree_path.exists(), "worktree dir should exist");
        assert!(
            worktree_path.join(".git").exists(),
            "worktree .git should exist"
        );

        // 验证分支已创建
        let branches = git_run(repo_path, &["branch", "--list"]);
        assert!(
            branches.contains("r-code/task123"),
            "expected r-code/task123 branch in {branches}"
        );

        // 验证 worktree 内文件可访问
        let content = std::fs::read_to_string(worktree_path.join("file.txt")).unwrap();
        assert_eq!(content, "content\n");

        // 移除 worktree
        svc.remove_worktree(&worktree_path).unwrap();
        assert!(!worktree_path.exists(), "worktree dir should be removed");
    }

    #[test]
    fn test_entry_snapshot_clean() {
        let tmp = create_test_repo();
        let repo_path = tmp.path();

        // 创建并提交文件
        std::fs::write(repo_path.join("file1.txt"), "content1\n").unwrap();
        std::fs::write(repo_path.join("file2.txt"), "content2\n").unwrap();
        git_run(repo_path, &["add", "-A"]);
        git_run(repo_path, &["commit", "-m", "initial"]);

        let svc = GitService::new(repo_path.to_path_buf());
        let snapshot = svc.entry_snapshot().unwrap();
        let sha = snapshot.expect("expected Some(tree sha) for git repo");

        assert_eq!(sha.len(), 40, "tree SHA should be 40 chars");

        // 干净工作区的快照应与 HEAD tree 一致
        let head_tree = git_run(repo_path, &["rev-parse", "HEAD^{tree}"]);
        assert_eq!(
            sha,
            head_tree.trim(),
            "clean snapshot should match HEAD tree"
        );
    }

    #[test]
    fn test_entry_snapshot_with_changes() {
        let tmp = create_test_repo();
        let repo_path = tmp.path();

        // 初始提交
        std::fs::write(repo_path.join("file1.txt"), "content1\n").unwrap();
        git_run(repo_path, &["add", "-A"]);
        git_run(repo_path, &["commit", "-m", "initial"]);

        // 创建未提交的新文件
        std::fs::write(repo_path.join("file2.txt"), "new\n").unwrap();

        let svc = GitService::new(repo_path.to_path_buf());
        let snapshot = svc.entry_snapshot().unwrap();
        let sha = snapshot.expect("expected Some(tree sha)");

        // 快照应不同于 HEAD tree（包含新文件）
        let head_tree = git_run(repo_path, &["rev-parse", "HEAD^{tree}"]);
        assert_ne!(
            sha,
            head_tree.trim(),
            "snapshot with changes should differ from HEAD tree"
        );

        // 验证快照 tree 包含新文件
        let files = git_run(repo_path, &["ls-tree", "-r", "--name-only", &sha]);
        assert!(
            files.contains("file2.txt"),
            "snapshot tree should contain file2.txt: {files}"
        );
    }

    #[test]
    fn test_entry_snapshot_non_git() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("file.txt"), "hi").unwrap();

        let svc = GitService::new(tmp.path().to_path_buf());
        let snapshot = svc.entry_snapshot().unwrap();
        assert_eq!(snapshot, None, "non-git repo should return None");
    }
}
