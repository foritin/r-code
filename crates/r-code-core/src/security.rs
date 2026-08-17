//! 路径边界安全：`PathGuard` 与 `ProjectContext`。
//!
//! 实现 doc-07 §4 / §11 定义的工作区路径边界模型：路径解析通过
//! [`PathGuard::resolve_path`] / [`PathGuard::resolve_existing_path`] 做
//! containment 校验；实际的既有文件 I/O 必须通过
//! [`PathGuard::open_file`] / [`PathGuard::list_directory`] /
//! [`PathGuard::atomic_write_path`] / [`PathGuard::create_new_path`] /
//! [`PathGuard::remove_file_if_exists`] 取得受工作区目录 capability 限制的句柄。
//! 这会拒绝任何逃逸尝试（符号链接、`..` 穿越、相对路径 cwd 逃逸）。
//!
//! ## 安全不变量
//! - **Fail-closed**：任何无法确证 containment 的路径一律返回
//!   [`ProductError::PathEscape`]，包括权限不足、IO 错误等情况。
//! - **Capability-only 解析与 I/O**：路径解析与文件读写都通过创建时固定的
//!   工作区目录 capability（`root_dir`）完成，不依赖 ambient
//!   `std::path::Path::canonicalize`。
//! - **抗竞态打开**：`open_file` 从创建时固定的工作区目录 capability 相对打开
//!   文件，不能在校验后因符号链接替换而重新按环境路径逃逸。
//! - **类型化句柄**：[`ResolvedWorkspacePath`]、[`WorkspaceFile`]、
//!   [`WorkspaceDirectory`] 把 capability 相对路径封装在内部，调用方只拿到
//!   展示路径或已打开的文件句柄，无法拿回可被 `std::fs` 按环境路径重新打开
//!   的裸 `PathBuf`。
//! - **Windows junction/reparse**：与 Unix 符号链接逃逸一样，junction 指向
//!   工作区外时所有入口均 fail-closed 拒绝。
//! - **崩溃一致性**：[`PathGuard::atomic_write_path`] 在 `rename` 成功后对父
//!   目录执行 `fsync`（不支持的平台依赖操作系统元数据日志，不伪同步）。
//!
//! [doc-07 §4, §11]: 路径边界 / Rust 实现要点

use std::io::{ErrorKind, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use uuid::Uuid;

use crate::error::ProductError;

/// Render an OS path for people and model-visible diagnostics without leaking Windows' internal
/// verbatim prefix. The underlying [`PathBuf`] must still be retained for filesystem operations:
/// removing `\\?\` here is a presentation concern, not a canonicalization step.
pub fn path_for_display(path: impl AsRef<Path>) -> String {
    let rendered = path.as_ref().as_os_str().to_string_lossy();
    #[cfg(windows)]
    {
        if let Some(rest) = rendered.strip_prefix(r"\\?\UNC\") {
            return format!(r"\\{rest}");
        }
        if let Some(rest) = rendered.strip_prefix(r"\\?\") {
            let bytes = rest.as_bytes();
            if bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && matches!(bytes[2], b'\\' | b'/')
            {
                return rest.to_string();
            }
        }
    }
    rendered.into_owned()
}

/// 工作区路径边界守卫 [doc-07 §4, §11]。
///
/// 持有一个已 canonical 化的 root 与固定的目录 capability，提供 containment
/// 校验和抗符号链接竞态的既有文件打开。
#[derive(Debug, Clone)]
pub struct PathGuard {
    root: PathBuf,
    // Keep the caller-visible spelling too. On macOS, temporary paths are
    // commonly exposed as `/var/...` while canonicalization returns
    // `/private/var/...`; the lexical helper must recognize both spellings.
    lexical_root: PathBuf,
    // Keep an open directory handle instead of reopening `root` by path for each
    // file operation. `cap-std` resolves child paths inside this capability on all
    // supported desktop platforms, including when a symlink is swapped after a
    // logical path check.
    root_dir: Arc<Dir>,
}

/// 既有工作区文件的访问模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceFileAccess {
    /// 只读访问。
    Read,
    /// 在同一个已验证句柄上先读取再写入，避免 revision 校验与写入之间重新按路径打开。
    ReadWrite,
}

/// 工作区目录内的一个非符号链接条目。
///
/// 由 [`PathGuard::list_directory`] 从目录 capability 枚举而来；调用方
/// 不应根据普通环境路径重新打开它。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceDirectoryEntry {
    /// 该条目的 canonical 绝对路径，仅用于显示或作为后续 `PathGuard` 调用的输入。
    pub path: PathBuf,
    /// 文件名，不含父目录。
    pub name: std::ffi::OsString,
    /// 是否为目录。
    pub is_directory: bool,
    /// 是否为普通文件。
    pub is_file: bool,
}

/// 类型化工作区路径：capability 相对路径 + 仅供展示的显示路径。
///
/// 它由 [`PathGuard::resolve_path`] / [`PathGuard::resolve_existing_path`] 产生，
/// 只能作为后续类型化 I/O 方法的输入；capability 相对路径从不对调用方暴露为
/// 可被 `std::fs` 重新按环境路径打开的值。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedWorkspacePath {
    /// 相对工作区 capability 的 canonical 路径。仅 [`PathGuard`] 的类型化 I/O
    /// 方法可消费，绝不对调用方暴露。
    #[allow(dead_code)]
    relative: PathBuf,
    /// 供身份比较、持久化键与展示使用的 canonical 绝对路径，不是文件系统 I/O 输入。
    canonical: PathBuf,
    /// 供展示/日志使用的绝对路径，不是文件系统 I/O 的输入。
    display_path: String,
}

impl ResolvedWorkspacePath {
    fn new(root: &Path, relative: PathBuf) -> Self {
        let canonical = root.join(&relative);
        let display_path = path_for_display(&canonical);
        Self {
            canonical,
            display_path,
            relative,
        }
    }

    /// 由已 canonical 的绝对路径构造（要求路径仍在 root 内）。
    fn from_canonical(root: &Path, canonical: PathBuf) -> Result<Self, ProductError> {
        let relative = canonical
            .strip_prefix(root)
            .map_err(|_| {
                ProductError::PathEscape(format!(
                    "path {canonical:?} is outside workspace root {root:?} (fail-closed)"
                ))
            })?
            .to_path_buf();
        Ok(Self::new(root, relative))
    }

    /// 展示路径（绝不可用于 `std::fs` 按路径重新打开）。
    pub fn display(&self) -> &str {
        &self.display_path
    }

    /// canonical 绝对路径，仅用于身份比较、持久化键、日志等非 I/O 场景。
    ///
    /// 绝不把它交给 `std::fs::File::open` / `read` / `write` 等按环境路径执行的
    /// 操作；真正的文件 I/O 必须通过 [`PathGuard`] 的类型化句柄方法完成。
    pub fn canonical(&self) -> &Path {
        &self.canonical
    }
}

/// 工作区内已打开的普通文件句柄。
///
/// 它持有真实文件句柄；调用方通过 [`file`](Self::file) /
/// [`file_mut`](Self::file_mut) 读写，而不会拿回一个可重新按环境路径打开的
/// 裸 `PathBuf`。
#[derive(Debug)]
pub struct WorkspaceFile {
    /// canonical 绝对路径，仅用于身份比较、持久化键与日志等非 I/O 场景。
    canonical: PathBuf,
    display_path: String,
    file: std::fs::File,
}

impl WorkspaceFile {
    fn new(canonical: PathBuf, file: std::fs::File) -> Self {
        Self {
            display_path: path_for_display(&canonical),
            canonical,
            file,
        }
    }

    /// 展示路径（绝不可用于 `std::fs` 按路径重新打开）。
    pub fn display(&self) -> &str {
        &self.display_path
    }

    /// 只读访问底层文件句柄。
    pub fn file(&self) -> &std::fs::File {
        &self.file
    }

    /// 可写访问底层文件句柄。
    pub fn file_mut(&mut self) -> &mut std::fs::File {
        &mut self.file
    }

    /// 取出底层文件句柄（不会泄漏任何裸路径）。
    pub fn into_file(self) -> std::fs::File {
        self.file
    }

    /// canonical 绝对路径，仅用于身份比较、持久化键、日志等非 I/O 场景。
    ///
    /// 绝不把它交给 `std::fs::File::open` / `read` / `write` 等按环境路径执行的
    /// 操作；真正的文件 I/O 必须通过 [`PathGuard`] 的类型化句柄方法完成。
    pub fn canonical(&self) -> &Path {
        &self.canonical
    }
}

/// 工作区目录的类型化枚举结果。
#[derive(Debug)]
pub struct WorkspaceDirectory {
    /// canonical 绝对路径，仅用于身份比较、持久化键与日志等非 I/O 场景。
    canonical: PathBuf,
    display_path: String,
    entries: Vec<WorkspaceDirectoryEntry>,
}

impl WorkspaceDirectory {
    fn new(canonical: PathBuf, entries: Vec<WorkspaceDirectoryEntry>) -> Self {
        Self {
            display_path: path_for_display(&canonical),
            canonical,
            entries,
        }
    }

    /// 展示路径（绝不可用于 `std::fs` 按路径重新打开）。
    pub fn display(&self) -> &str {
        &self.display_path
    }

    /// 目录直接子项（不含符号链接条目）。
    pub fn entries(&self) -> &[WorkspaceDirectoryEntry] {
        &self.entries
    }

    /// 取出目录子项。
    pub fn into_entries(self) -> Vec<WorkspaceDirectoryEntry> {
        self.entries
    }

    /// canonical ��对路径，仅用于身份比较、持久化键、日志等非 I/O 场景。
    ///
    /// 绝不把它交给 `std::fs::File::open` / `read` / `write` 等按环境路径执行的
    /// 操作；真正的文件 I/O 必须通过 [`PathGuard`] 的类型化句柄方法完成。
    pub fn canonical(&self) -> &Path {
        &self.canonical
    }
}

impl PathGuard {
    /// 创建以 `root` 为根的 `PathGuard`。
    ///
    /// `root` 在创建时即被 canonical 化；若 canonical 化失败（不存在、无权限），
    /// 返回 [`ProductError::PathEscape`]（fail-closed）。
    pub fn new(root: PathBuf) -> Result<Self, ProductError> {
        let lexical_root = if root.is_absolute() {
            root.clone()
        } else {
            std::env::current_dir()
                .map_err(|err| {
                    ProductError::PathEscape(format!(
                        "cannot anchor root {root:?}: {err} (fail-closed)"
                    ))
                })?
                .join(&root)
        };
        let canonical = root.canonicalize().map_err(|err| {
            ProductError::PathEscape(format!(
                "failed to canonicalize root {root:?}: {err} (fail-closed)"
            ))
        })?;
        let root_dir = Dir::open_ambient_dir(&canonical, ambient_authority()).map_err(|err| {
            ProductError::PathEscape(format!(
                "failed to open workspace root {canonical:?}: {err} (fail-closed)"
            ))
        })?;
        Ok(Self {
            root: canonical,
            lexical_root,
            root_dir: Arc::new(root_dir),
        })
    }

    /// 将 `path` 解析为工作区 capability 相对 canonical 路径，确保不逃逸。
    ///
    /// - 若路径已存在：通过创建时持有的目录 capability `canonicalize`（解析全部
    ///   符号链接与 `..`），并校验 canonical 结果仍落在 root 内。
    /// - 若路径尚不存在（如即将创建的文件）：在同一 capability 上解析最近的现存
    ///   祖先，重新拼接不存在的尾段并校验。
    ///
    /// 返回相对工作区 capability 的路径（root 本身为空路径），由公开 API 决定
    /// 是包装为 [`ResolvedWorkspacePath`] 还是拼接回绝对路径。
    ///
    /// 任何错误（权限不足、IO 错误等）均 fail-closed 返回
    /// [`ProductError::PathEscape`]。
    fn resolve_relative(&self, path: &Path) -> Result<PathBuf, ProductError> {
        let relative = self.anchor_workspace_relative(path)?;
        if relative.as_os_str().is_empty() {
            return Ok(PathBuf::new());
        }
        match self.root_dir.canonicalize(&relative) {
            Ok(canonical_relative) => {
                if Self::relative_is_contained(&canonical_relative) {
                    Ok(canonical_relative)
                } else {
                    Err(ProductError::PathEscape(format!(
                        "path {path:?} canonicalizes to {canonical_relative:?} outside root {:?}",
                        self.root
                    )))
                }
            }
            Err(err) if err.kind() == ErrorKind::NotFound => {
                self.resolve_via_ancestor_relative(path, &relative)
            }
            Err(err) => Err(self.resolve_canonicalize_error(path, err)),
        }
    }

    /// 与 [`resolve_relative`](Self::resolve_relative) 类似，但要求路径必须已存在。
    ///
    /// 用于只读工具（`list_files`、`read_file` 等）：路径不存在时直接返回
    /// [`ProductError::PathNotFound`]，而不是通过祖先目录推断 containment。
    /// 后者是写入工具（如 `create_file`）的语义——文件尚不存在但父目录在工作区内。
    fn resolve_relative_existing(&self, path: &Path) -> Result<PathBuf, ProductError> {
        let relative = self.anchor_workspace_relative(path)?;
        if relative.as_os_str().is_empty() {
            return Ok(PathBuf::new());
        }
        match self.root_dir.canonicalize(&relative) {
            Ok(canonical_relative) => {
                if Self::relative_is_contained(&canonical_relative) {
                    Ok(canonical_relative)
                } else {
                    Err(ProductError::PathEscape(format!(
                        "path {path:?} canonicalizes to {canonical_relative:?} outside root {:?}",
                        self.root
                    )))
                }
            }
            Err(err) if err.kind() == ErrorKind::NotFound => Err(ProductError::PathNotFound(
                format!("path does not exist: {path:?}"),
            )),
            Err(err) => Err(self.resolve_canonicalize_error(path, err)),
        }
    }

    /// 解析为类型化工作区路径（推荐）。返回的 [`ResolvedWorkspacePath`] 不暴露可被
    /// `std::fs` 按路径重新打开的裸 `PathBuf`。
    pub fn resolve_path(&self, path: &Path) -> Result<ResolvedWorkspacePath, ProductError> {
        let relative = self.resolve_relative(path)?;
        Ok(ResolvedWorkspacePath::new(&self.root, relative))
    }

    /// 解析已存在的类型化工作区路径（推荐）。
    pub fn resolve_existing_path(
        &self,
        path: &Path,
    ) -> Result<ResolvedWorkspacePath, ProductError> {
        let relative = self.resolve_relative_existing(path)?;
        Ok(ResolvedWorkspacePath::new(&self.root, relative))
    }

    /// 将输入路径锚定并转换为相对工作区 capability 的路径。
    ///
    /// 相对路径以进程 CWD 为锚（与 `std::fs::canonicalize` 一致），绝对路径直接
    /// 使用。词法上不在 root 内的绝对路径立即拒绝；残余的 `..` 组件由后续
    /// capability canonicalize 权威校验，因此 `sub/../file` 这类仍落在 root 内
    /// 的路径可以正常解析。
    fn anchor_workspace_relative(&self, path: &Path) -> Result<PathBuf, ProductError> {
        let anchored = if path.is_absolute() {
            path.to_path_buf()
        } else {
            let cwd = std::env::current_dir().map_err(|err| {
                ProductError::PathEscape(format!(
                    "cannot get cwd for {path:?}: {err} (fail-closed)"
                ))
            })?;
            cwd.join(path)
        };
        self.strip_workspace_root(&anchored).ok_or_else(|| {
            ProductError::PathEscape(format!(
                "absolute path {anchored:?} is outside workspace root {:?}",
                self.root
            ))
        })
    }

    fn resolve_canonicalize_error(&self, path: &Path, err: std::io::Error) -> ProductError {
        ProductError::PathEscape(format!(
            "cannot canonicalize {path:?} within workspace {:?}: {err} (fail-closed)",
            self.root
        ))
    }

    /// 判断一个相对 capability 的路径是否仍落在工作区内。
    ///
    /// capability canonicalize 正常不会产生越界路径，这里保留词法防御：一旦
    /// 首组件是 `..`、根或前缀，视为逃逸。
    fn relative_is_contained(path: &Path) -> bool {
        !matches!(
            path.components().next(),
            Some(Component::ParentDir | Component::RootDir | Component::Prefix(_))
        )
    }

    /// 在工作区目录 capability 内安全打开一个已存在的普通文件（推荐）。
    ///
    /// 返回类型化 [`WorkspaceFile`]：通过 `file()` / `file_mut()` 读写，
    /// `display()` 仅用于展示，不会把可被 `std::fs` 按路径重新打开的裸
    /// `PathBuf` 交还给调用方。
    ///
    /// 相对路径以工作区 root 为基；绝对路径必须以 root 的 canonical 或 caller
    /// 可见拼写为前缀。任何 `..`、绝对逃逸、符号链接逃逸或打开错误都 fail-closed。
    pub fn open_file(
        &self,
        path: &Path,
        access: WorkspaceFileAccess,
    ) -> Result<WorkspaceFile, ProductError> {
        let (canonical, file) = self.open_existing_file_inner(path, access)?;
        Ok(WorkspaceFile::new(canonical, file))
    }

    fn open_existing_file_inner(
        &self,
        path: &Path,
        access: WorkspaceFileAccess,
    ) -> Result<(PathBuf, std::fs::File), ProductError> {
        let relative = self.relative_workspace_path(path)?;
        let canonical_relative = self
            .root_dir
            .canonicalize(&relative)
            .map_err(|err| self.workspace_open_error(path, err))?;
        let metadata = self
            .root_dir
            .metadata(&canonical_relative)
            .map_err(|err| self.workspace_open_error(path, err))?;
        if !metadata.is_file() {
            return Err(ProductError::PathEscape(format!(
                "path {path:?} is not a regular file within workspace (fail-closed)"
            )));
        }

        let mut options = OpenOptions::new();
        options.read(true);
        if access == WorkspaceFileAccess::ReadWrite {
            options.write(true);
        }
        let file = self
            .root_dir
            .open_with(&canonical_relative, &options)
            .map_err(|err| self.workspace_open_error(path, err))?;
        let opened_metadata = file
            .metadata()
            .map_err(|err| self.workspace_open_error(path, err))?;
        if !opened_metadata.is_file() {
            return Err(ProductError::PathEscape(format!(
                "path {path:?} is not a regular file within workspace (fail-closed)"
            )));
        }

        Ok((self.root.join(canonical_relative), file.into_std()))
    }

    /// 从目录 capability 枚举一个已存在目录的直接子项（推荐）。
    ///
    /// 返回类型化 [`WorkspaceDirectory`]；`display()` 仅供展示，`entries()`
    /// 返回不含符号链接的直接子项。与先 `resolve_path` 后调用 `std::fs::read_dir`
    /// 不同，此方法始终以创建 `PathGuard` 时固定的目录句柄打开目标目录。
    pub fn list_directory(&self, path: &Path) -> Result<WorkspaceDirectory, ProductError> {
        let (canonical, entries) = self.list_existing_directory_inner(path)?;
        Ok(WorkspaceDirectory::new(canonical, entries))
    }

    fn list_existing_directory_inner(
        &self,
        path: &Path,
    ) -> Result<(PathBuf, Vec<WorkspaceDirectoryEntry>), ProductError> {
        let relative = self.relative_workspace_directory_path(path)?;
        let canonical_relative = if relative.as_os_str().is_empty() {
            PathBuf::new()
        } else {
            self.root_dir
                .canonicalize(&relative)
                .map_err(|err| self.workspace_open_error(path, err))?
        };
        let directory = if canonical_relative.as_os_str().is_empty() {
            self.root_dir
                .try_clone()
                .map_err(|err| self.workspace_open_error(path, err))?
        } else {
            self.root_dir
                .open_dir(&canonical_relative)
                .map_err(|err| self.workspace_open_error(path, err))?
        };
        let canonical = self.root.join(&canonical_relative);
        let entries = directory
            .entries()
            .map_err(|err| self.workspace_open_error(path, err))?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let file_type = entry.file_type().ok()?;
                if file_type.is_symlink() {
                    return None;
                }
                let name = entry.file_name();
                Some(WorkspaceDirectoryEntry {
                    path: canonical.join(&name),
                    name,
                    is_directory: file_type.is_dir(),
                    is_file: file_type.is_file(),
                })
            })
            .collect();
        Ok((canonical, entries))
    }

    /// 以 capability-relative 原子替换写入工作区文件（推荐）。
    ///
    /// 返回类型化 [`ResolvedWorkspacePath`]，`display()` 仅供展示；不会把可被
    /// `std::fs` 按路径重新打开的裸 `PathBuf` 交还给调用方。
    ///
    /// 目标目录和临时文件都从固定工作区句柄打开。这样即使攻击者在逻辑路径校验后
    /// 替换符号链接，也无法把写入重定向到工作区外。父目录必须已经存在，避免让
    /// `apply_patch` 等原本只写文件的调用隐式创建目录；已有普通文件的权限会保留。
    pub fn atomic_write_path(
        &self,
        path: &Path,
        content: &[u8],
    ) -> Result<ResolvedWorkspacePath, ProductError> {
        let canonical = self.atomic_write_file_inner(path, content)?;
        ResolvedWorkspacePath::from_canonical(&self.root, canonical)
    }

    fn atomic_write_file_inner(
        &self,
        path: &Path,
        content: &[u8],
    ) -> Result<PathBuf, ProductError> {
        let relative = self.relative_workspace_path(path)?;
        let canonical_relative = match self.root_dir.canonicalize(&relative) {
            Ok(canonical) => canonical,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                let parent = relative.parent().unwrap_or_else(|| Path::new(""));
                let canonical_parent = if parent.as_os_str().is_empty() {
                    PathBuf::new()
                } else {
                    self.root_dir
                        .canonicalize(parent)
                        .map_err(|err| self.workspace_open_error(path, err))?
                };
                let file_name = relative.file_name().ok_or_else(|| {
                    ProductError::PathEscape(format!(
                        "workspace path {path:?} does not name a file (fail-closed)"
                    ))
                })?;
                canonical_parent.join(file_name)
            }
            Err(error) => return Err(self.workspace_open_error(path, error)),
        };
        let parent = canonical_relative.parent().unwrap_or_else(|| Path::new(""));
        let file_name = canonical_relative.file_name().ok_or_else(|| {
            ProductError::PathEscape(format!(
                "workspace path {path:?} does not name a file (fail-closed)"
            ))
        })?;
        let parent_dir = if parent.as_os_str().is_empty() {
            self.root_dir
                .try_clone()
                .map_err(|err| self.workspace_open_error(path, err))?
        } else {
            self.root_dir
                .open_dir(parent)
                .map_err(|err| self.workspace_open_error(path, err))?
        };
        let prior_permissions = match parent_dir.metadata(file_name) {
            Ok(metadata) if metadata.is_file() => Some(metadata.permissions()),
            Ok(_) => {
                return Err(ProductError::PathEscape(format!(
                    "path {path:?} is not a regular file within workspace (fail-closed)"
                )));
            }
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => return Err(self.workspace_open_error(path, error)),
        };

        for _ in 0..16 {
            let temporary = PathBuf::from(format!(".r-code-write-{}.tmp", Uuid::new_v4()));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            let mut file = match parent_dir.open_with(&temporary, &options) {
                Ok(file) => file,
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(self.workspace_open_error(path, error)),
            };
            let write_result = (|| -> std::io::Result<()> {
                file.write_all(content)?;
                file.flush()?;
                file.sync_all()?;
                if let Some(permissions) = prior_permissions.as_ref() {
                    file.set_permissions(permissions.clone())?;
                }
                Ok(())
            })();
            drop(file);
            if let Err(error) = write_result {
                let _ = parent_dir.remove_file(&temporary);
                return Err(self.workspace_open_error(path, error));
            }
            match parent_dir.rename(&temporary, &parent_dir, file_name) {
                Ok(()) => {
                    self.sync_parent_directory(parent_dir, path)?;
                    return Ok(self.root.join(canonical_relative));
                }
                Err(error) => {
                    let _ = parent_dir.remove_file(&temporary);
                    return Err(self.workspace_open_error(path, error));
                }
            }
        }
        Err(ProductError::PathEscape(format!(
            "could not allocate a secure temporary file for {path:?} (fail-closed)"
        )))
    }

    /// rename 成功后同步父目录，使目录项在崩溃后仍可见。
    ///
    /// 目录 fsync 是平台相关的：Linux 等 Unix 支持对已打开目录 fd 调用
    /// `fsync`，失败即 fail-closed；Windows 没有用户态目录同步原语
    /// （`FlushFileBuffers` 对目录句柄固定返回 `ERROR_ACCESS_DENIED`），
    /// macOS 对目录 fd 的 `fsync` 返回 `EINVAL`。这两类平台的目录项持久性
    /// 由操作系统自身的日志/缓存策略保证，此处不做伪同步，也绝不把它当成
    /// 写入失败。
    fn sync_parent_directory(
        &self,
        parent_dir: cap_std::fs::Dir,
        path: &Path,
    ) -> Result<(), ProductError> {
        #[cfg(all(unix, not(target_os = "macos")))]
        {
            let dir_file = parent_dir.into_std_file();
            dir_file.sync_all().map_err(|err| {
                ProductError::PathEscape(format!(
                    "cannot sync parent directory for {path:?} within workspace {:?}: {err} (fail-closed)",
                    self.root
                ))
            })?;
        }
        #[cfg(any(windows, target_os = "macos"))]
        {
            let _ = parent_dir;
            tracing::debug!(
                "atomic_write_path: skipping parent-directory fsync for {path:?} (unsupported on this platform; relying on OS metadata journaling)"
            );
        }
        Ok(())
    }

    /// 在已存在的工作区父目录中创建一个新的普通文件，绝不覆盖已有条目（推荐）。
    ///
    /// 返回类型化 [`ResolvedWorkspacePath`]，`display()` 仅供展示；不会把可被
    /// `std::fs` 按路径重新打开的裸 `PathBuf` 交还给调用方。
    ///
    /// `create_new` 在同一 capability 目录句柄上完成存在性检查与创建，因此即使
    /// 调用前后存在同名文件或符号链接竞态，也不会写到工作区外或覆盖已有文件。
    /// 父目录必须已经存在，与 `std::fs::OpenOptions::create_new` 的历史语义一致。
    pub fn create_new_path(
        &self,
        path: &Path,
        content: &[u8],
    ) -> Result<ResolvedWorkspacePath, ProductError> {
        let canonical = self.create_new_file_inner(path, content)?;
        ResolvedWorkspacePath::from_canonical(&self.root, canonical)
    }

    fn create_new_file_inner(&self, path: &Path, content: &[u8]) -> Result<PathBuf, ProductError> {
        let relative = self.relative_workspace_path(path)?;
        let parent = relative.parent().unwrap_or_else(|| Path::new(""));
        let file_name = relative.file_name().ok_or_else(|| {
            ProductError::PathEscape(format!(
                "workspace path {path:?} does not name a file (fail-closed)"
            ))
        })?;
        let parent_dir = if parent.as_os_str().is_empty() {
            self.root_dir
                .try_clone()
                .map_err(|err| self.workspace_open_error(path, err))?
        } else {
            self.root_dir
                .open_dir(parent)
                .map_err(|err| self.workspace_open_error(path, err))?
        };

        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = parent_dir
            .open_with(file_name, &options)
            .map_err(|err| self.workspace_open_error(path, err))?;
        file.write_all(content)
            .and_then(|()| file.flush())
            .and_then(|()| file.sync_all())
            .map_err(|err| self.workspace_open_error(path, err))?;

        Ok(self.root.join(relative))
    }

    /// 删除工作区中的普通文件。不存在的文件返回 `false`，其余路径错误 fail-closed。
    pub fn remove_file_if_exists(&self, path: &Path) -> Result<bool, ProductError> {
        let relative = self.relative_workspace_path(path)?;
        let canonical_relative = match self.root_dir.canonicalize(&relative) {
            Ok(canonical) => canonical,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(self.workspace_open_error(path, error)),
        };
        let metadata = self
            .root_dir
            .metadata(&canonical_relative)
            .map_err(|err| self.workspace_open_error(path, err))?;
        if !metadata.is_file() {
            return Err(ProductError::PathEscape(format!(
                "path {path:?} is not a regular file within workspace (fail-closed)"
            )));
        }
        match self.root_dir.remove_file(&canonical_relative) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Err(error) => Err(self.workspace_open_error(path, error)),
        }
    }

    /// 将一个调用方路径转为相对于固定工作区 capability 的安全路径。
    fn relative_workspace_path(&self, path: &Path) -> Result<PathBuf, ProductError> {
        let normalized = self.normalized_workspace_path(path)?;
        if normalized.as_os_str().is_empty() {
            return Err(ProductError::PathEscape(format!(
                "workspace path {path:?} does not name a file (fail-closed)"
            )));
        }
        Ok(normalized)
    }

    /// 与 [`relative_workspace_path`](Self::relative_workspace_path) 相同，但允许
    /// 工作区根本身作为目录。
    fn relative_workspace_directory_path(&self, path: &Path) -> Result<PathBuf, ProductError> {
        self.normalized_workspace_path(path)
    }

    fn normalized_workspace_path(&self, path: &Path) -> Result<PathBuf, ProductError> {
        let relative = if path.is_absolute() {
            self.strip_workspace_root(path).ok_or_else(|| {
                ProductError::PathEscape(format!(
                    "absolute path {path:?} is outside workspace root {:?}",
                    self.root
                ))
            })?
        } else {
            path.to_path_buf()
        };

        let mut normalized = PathBuf::new();
        for component in relative.components() {
            match component {
                Component::Normal(name) => normalized.push(name),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(ProductError::PathEscape(format!(
                        "workspace path {path:?} contains disallowed component {component:?} (fail-closed)"
                    )));
                }
            }
        }
        Ok(normalized)
    }

    fn strip_workspace_root(&self, path: &Path) -> Option<PathBuf> {
        path.strip_prefix(&self.root)
            .or(path.strip_prefix(&self.lexical_root))
            .map(Path::to_path_buf)
            .ok()
            .or({
                // Windows canonical paths commonly add `\\?\` while user-facing paths do
                // not. Compare the equivalent normal spellings as a final lexical step.
                #[cfg(windows)]
                {
                    fn strip_verbatim(path: &Path) -> PathBuf {
                        let value = path.as_os_str().to_string_lossy();
                        value
                            .strip_prefix(r"\\?\")
                            .map(PathBuf::from)
                            .unwrap_or_else(|| path.to_path_buf())
                    }
                    let path = strip_verbatim(path);
                    let root = strip_verbatim(&self.root);
                    let lexical_root = strip_verbatim(&self.lexical_root);
                    path.strip_prefix(root)
                        .or(path.strip_prefix(lexical_root))
                        .map(Path::to_path_buf)
                        .ok()
                }
                #[cfg(not(windows))]
                {
                    None
                }
            })
    }

    fn workspace_open_error(&self, path: &Path, err: std::io::Error) -> ProductError {
        if err.kind() == ErrorKind::NotFound {
            ProductError::PathNotFound(format!("path does not exist: {path:?}"))
        } else {
            ProductError::PathEscape(format!(
                "cannot safely open {path:?} within workspace {:?}: {err} (fail-closed)",
                self.root
            ))
        }
    }

    /// 为尚不存在的路径解析 containment：找到最近的现存祖先并重新拼接尾段。
    ///
    /// `relative` 已由 [`anchor_workspace_relative`](Self::anchor_workspace_relative)
    /// 相对工作区 capability 锚定。祖先查找全部通过 `root_dir` 完成，尾段中若出现
    /// `..` / `.`（无法被祖先 canonical 化消解的相对组件），视为逃逸尝试。
    ///
    /// 返回相对工作区 capability 的路径，由调用方决定是否拼接绝对路径。
    fn resolve_via_ancestor_relative(
        &self,
        path: &Path,
        relative: &Path,
    ) -> Result<PathBuf, ProductError> {
        let mut tail: Vec<std::ffi::OsString> = Vec::new();
        let mut current: &Path = relative;
        loop {
            if current.as_os_str().is_empty() {
                // 工作区根即最近的现存祖先。
                let mut resolved = PathBuf::new();
                for name in tail.into_iter().rev() {
                    resolved.push(name);
                }
                if !Self::relative_is_contained(&resolved) {
                    return Err(ProductError::PathEscape(format!(
                        "resolved path {resolved:?} escapes root {:?}",
                        self.root
                    )));
                }
                return Ok(resolved);
            }
            match self.root_dir.canonicalize(current) {
                Ok(canonical_ancestor) => {
                    let mut resolved = canonical_ancestor;
                    for name in tail.into_iter().rev() {
                        resolved.push(name);
                    }
                    if !Self::relative_is_contained(&resolved) {
                        return Err(ProductError::PathEscape(format!(
                            "resolved path {resolved:?} escapes root {:?}",
                            self.root
                        )));
                    }
                    return Ok(resolved);
                }
                Err(err) if err.kind() == ErrorKind::NotFound => match current.file_name() {
                    Some(name)
                        if name == std::ffi::OsStr::new("..")
                            || name == std::ffi::OsStr::new(".") =>
                    {
                        return Err(ProductError::PathEscape(format!(
                            "non-existent portion of {path:?} contains relative component {name:?} (fail-closed)"
                        )));
                    }
                    Some(name) => {
                        tail.push(name.to_os_string());
                        current = match current.parent() {
                            Some(p) => p,
                            None => {
                                return Err(ProductError::PathEscape(format!(
                                    "no existing ancestor for {path:?} (reached path root, fail-closed)"
                                )));
                            }
                        };
                    }
                    None => {
                        return Err(ProductError::PathEscape(format!(
                            "no existing ancestor for {path:?} (reached path root, fail-closed)"
                        )));
                    }
                },
                Err(err) => return Err(self.resolve_canonicalize_error(path, err)),
            }
        }
    }

    /// 词法检查 `path` 是否在 root 内（不解析符号链接）。
    ///
    /// **非权威**：仅做快速前缀判断，不解析符号链接或 `..`。任何安全决策
    /// 必须使用 [`resolve_path`](Self::resolve_path) /
    /// [`resolve_existing_path`](Self::resolve_existing_path)；进行实际文件
    /// I/O 时必须使用 [`open_file`](Self::open_file) 等类型化句柄方法。
    pub fn contains(&self, path: &Path) -> bool {
        if path.starts_with(&self.root) || path.starts_with(&self.lexical_root) {
            return true;
        }
        // Windows：root 经 canonicalize 带 `\\?\` verbatim 前缀，而未 canonical
        // 的探测路径没有该前缀，词法比较会假阴性。去前缀后再比一次。
        #[cfg(windows)]
        {
            fn strip_verbatim(p: &Path) -> PathBuf {
                let s = p.as_os_str().to_string_lossy();
                match s.strip_prefix(r"\\?\") {
                    Some(rest) => PathBuf::from(rest),
                    None => p.to_path_buf(),
                }
            }
            let path = strip_verbatim(path);
            path.starts_with(strip_verbatim(&self.root))
                || path.starts_with(strip_verbatim(&self.lexical_root))
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    /// 返回 root 路径（已 canonical 化）。
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// 项目上下文 [doc-01 §3.1]。
///
/// 以 mount root 为键，聚合该项目的路径边界守卫。后续将扩展为包含
/// DocumentStore、ChangeService、BlobStore、ToolGateway、PermissionEngine、
/// VerificationService 的完整上下文，独立于焦点编辑器 workspace 存活。
#[derive(Debug, Clone)]
pub struct ProjectContext {
    /// 项目挂载根（canonical 化）。
    pub mount_root: PathBuf,
    /// 路径边界守卫。
    pub path_guard: PathGuard,
}

impl ProjectContext {
    /// 以 `mount_root` 创建项目上下文，构造对应的 `PathGuard`。
    pub fn new(mount_root: PathBuf) -> Result<Self, ProductError> {
        let path_guard = PathGuard::new(mount_root)?;
        let mount_root = path_guard.root().to_path_buf();
        Ok(Self {
            mount_root,
            path_guard,
        })
    }

    /// 返回路径边界守卫的引用。
    pub fn path_guard(&self) -> &PathGuard {
        &self.path_guard
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::io::Read;
    use std::sync::Mutex;
    use tempfile::TempDir;

    // set_current_dir 是进程级全局状态，cwd 相关测试需串行化。
    static CWD_LOCK: Mutex<()> = Mutex::new(());

    /// 直接以 TempDir 作为 root。
    fn make_root() -> TempDir {
        TempDir::new().expect("create temp dir")
    }

    #[cfg(windows)]
    #[test]
    fn display_path_hides_windows_verbatim_prefixes() {
        assert_eq!(
            path_for_display(r"\\?\D:\project\r-code\src\main.rs"),
            r"D:\project\r-code\src\main.rs"
        );
        assert_eq!(
            path_for_display(r"\\?\UNC\server\share\file.rs"),
            r"\\server\share\file.rs"
        );
        assert_eq!(
            path_for_display(r"D:\project\r-code\src\main.rs"),
            r"D:\project\r-code\src\main.rs"
        );
        assert_eq!(
            path_for_display(r"\\?\Volume{1234}\project\main.rs"),
            r"\\?\Volume{1234}\project\main.rs"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn display_path_preserves_literal_unix_names_that_look_verbatim() {
        assert_eq!(
            path_for_display(r"\\?\D:\project\r-code\src\main.rs"),
            r"\\?\D:\project\r-code\src\main.rs"
        );
    }

    /// 创建一个外层 TempDir，并在其中建 `root` 子目录，便于放置 root 外部
    /// 的符号链接目标（仍在外层 TempDir 内、唯一隔离）。
    ///
    /// 只有 `#[cfg(unix)]` 的符号链接测试用得到它——Windows 建符号链接需要额外
    /// 权限，那几个测试不编译。不加这个 gate，Windows 上会报 dead_code；CI 跑在
    /// ubuntu 上看不见，本地 `cargo check` 才会暴露。
    #[cfg(unix)]
    fn make_outer_with_root() -> (TempDir, PathBuf) {
        let outer = TempDir::new().expect("create outer temp dir");
        let root = outer.path().join("root");
        fs::create_dir(&root).expect("create root dir");
        (outer, root)
    }

    #[test]
    fn working_path_within_root_passes() {
        let root = make_root();
        let guard = PathGuard::new(root.path().to_path_buf()).unwrap();
        let file = root.path().join("hello.txt");
        fs::write(&file, "hi").unwrap();
        let resolved = guard
            .resolve_path(&file)
            .expect("path within root should resolve");
        assert_eq!(resolved.canonical(), file.canonicalize().unwrap());
        assert!(resolved.canonical().starts_with(guard.root()));
    }

    #[test]
    fn path_outside_root_rejected() {
        let root = make_root();
        let guard = PathGuard::new(root.path().to_path_buf()).unwrap();
        let err = guard.resolve_path(Path::new("/etc/passwd")).unwrap_err();
        assert!(matches!(err, ProductError::PathEscape(_)), "got: {err:?}");
    }

    #[test]
    fn root_itself_accepted() {
        let root = make_root();
        let guard = PathGuard::new(root.path().to_path_buf()).unwrap();
        let resolved = guard
            .resolve_path(guard.root())
            .expect("root should resolve");
        assert_eq!(resolved.canonical(), guard.root());
    }

    #[test]
    fn nonexistent_path_within_root_resolves() {
        let root = make_root();
        let guard = PathGuard::new(root.path().to_path_buf()).unwrap();
        let target = root.path().join("newfile.txt");
        assert!(!target.exists());
        let resolved = guard
            .resolve_path(&target)
            .expect("non-existent path within root should resolve");
        assert!(!resolved.canonical().exists());
        assert_eq!(
            resolved.canonical(),
            root.path().canonicalize().unwrap().join("newfile.txt")
        );
        assert!(resolved.canonical().starts_with(guard.root()));
    }

    #[test]
    fn deeply_nonexistent_path_within_root_resolves() {
        let root = make_root();
        let guard = PathGuard::new(root.path().to_path_buf()).unwrap();
        let target = root.path().join("a/b/c/new.txt");
        let resolved = guard
            .resolve_path(&target)
            .expect("deeply non-existent path within root should resolve");
        assert_eq!(
            resolved.canonical(),
            root.path().canonicalize().unwrap().join("a/b/c/new.txt")
        );
    }

    #[test]
    fn nonexistent_path_outside_root_rejected() {
        let root = make_root();
        let guard = PathGuard::new(root.path().to_path_buf()).unwrap();
        let target = Path::new("/etc/r_code_nonexistent_marker_xyz.txt");
        let err = guard.resolve_path(target).unwrap_err();
        assert!(matches!(err, ProductError::PathEscape(_)), "got: {err:?}");
    }

    #[test]
    fn resolve_existing_accepts_existing_path() {
        let root = make_root();
        let guard = PathGuard::new(root.path().to_path_buf()).unwrap();
        let file = root.path().join("hello.txt");
        fs::write(&file, "hi").unwrap();
        let resolved = guard
            .resolve_existing_path(&file)
            .expect("existing path should resolve");
        assert_eq!(resolved.canonical(), file.canonicalize().unwrap());
    }

    #[test]
    fn resolve_existing_rejects_nonexistent_path() {
        let root = make_root();
        let guard = PathGuard::new(root.path().to_path_buf()).unwrap();
        let target = root.path().join("does_not_exist.txt");
        assert!(!target.exists());
        let err = guard.resolve_existing_path(&target).unwrap_err();
        assert!(matches!(err, ProductError::PathNotFound(_)), "got: {err:?}");
    }

    #[test]
    fn resolve_existing_rejects_escape() {
        // 用外层 TempDir 包裹 root，在 root 之外放一个存在的文件，
        // 验证 resolve_existing_path 对"存在但逃逸"的路径返回 PathEscape。
        let outer = TempDir::new().expect("create outer temp dir");
        let root = outer.path().join("root");
        fs::create_dir(&root).expect("create root dir");
        let guard = PathGuard::new(root).unwrap();
        let outside = outer.path().join("outside.txt");
        fs::write(&outside, "outside").unwrap();
        let err = guard.resolve_existing_path(&outside).unwrap_err();
        assert!(matches!(err, ProductError::PathEscape(_)), "got: {err:?}");
    }

    #[test]
    fn lexical_prefix_but_canonical_escape_rejected() {
        // 输入路径词法上以 root 开头，但 canonical 化后逃逸到 root 之外：
        // 验证 TOCTOU 防护——校验发生在 canonical 化之后。
        let root = make_root();
        let guard = PathGuard::new(root.path().to_path_buf()).unwrap();
        fs::create_dir(root.path().join("sub")).unwrap();
        let escape = root.path().join("sub").join("../../../etc/passwd");
        // 词法前缀确实落在 root 内（迷惑性），但 canonical 化后逃逸。
        assert!(escape.starts_with(root.path()));
        let err = guard.resolve_path(&escape).unwrap_err();
        assert!(matches!(err, ProductError::PathEscape(_)), "got: {err:?}");
    }

    #[test]
    fn parent_component_within_root_resolves_via_capability() {
        // capability canonicalize 必须仍能规范化仍落在 root 内的 `..` 组件，
        // 不能把 `sub/../file` 误判为逃逸。
        let root = make_root();
        let guard = PathGuard::new(root.path().to_path_buf()).unwrap();
        fs::create_dir(root.path().join("sub")).unwrap();
        let file = root.path().join("file.txt");
        fs::write(&file, "data").unwrap();

        let resolved = guard
            .resolve_path(&root.path().join("sub").join("../file.txt"))
            .expect("in-workspace parent traversal should resolve");
        assert_eq!(resolved.canonical(), file.canonicalize().unwrap());
        assert!(resolved.canonical().starts_with(guard.root()));
    }

    #[test]
    fn fail_closed_on_nonexistent_root() {
        let bogus = make_root().path().join("does/not/exist");
        let err = PathGuard::new(bogus).unwrap_err();
        assert!(matches!(err, ProductError::PathEscape(_)), "got: {err:?}");
    }

    #[cfg(windows)]
    #[test]
    fn junction_escape_rejected_by_all_capability_paths() {
        // Windows junction（目录 reparse point）不需要管理员权限，是最常见的
        // 逃逸向量。从工作区子目录指向工作区外的目录，四个 capability 入口
        // 都必须 fail-closed 拒绝。创建 junction 失败（权限/文件系统不支持）时
        // 显式 skip，不使 CI 失败。
        let outer = TempDir::new().expect("create outer temp dir");
        let root = outer.path().join("root");
        let outside = outer.path().join("outside");
        fs::create_dir(&root).expect("create workspace root");
        fs::create_dir(&outside).expect("create outside dir");
        fs::write(outside.join("secret.txt"), "secret").expect("write outside file");

        let link = root.join("link");
        match std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&outside)
            .status()
        {
            Ok(status) if status.success() => {}
            Ok(status) => {
                eprintln!("[windows-junction-test] skipped: mklink exited {status}");
                return;
            }
            Err(err) => {
                eprintln!("[windows-junction-test] skipped: cannot create junction: {err}");
                return;
            }
        }
        assert!(
            link.exists(),
            "junction must be created for the escape test"
        );

        let guard = PathGuard::new(root).expect("create PathGuard");

        let err = guard.resolve_path(&link).unwrap_err();
        assert!(
            matches!(err, ProductError::PathEscape(_)),
            "resolve_path should reject junction escape, got: {err:?}"
        );

        let err = guard.resolve_existing_path(&link).unwrap_err();
        assert!(
            matches!(err, ProductError::PathEscape(_)),
            "resolve_existing_path should reject junction escape, got: {err:?}"
        );

        let err = guard
            .open_file(&link, WorkspaceFileAccess::Read)
            .unwrap_err();
        assert!(
            matches!(err, ProductError::PathEscape(_)),
            "open_file should reject junction escape, got: {err:?}"
        );

        let err = guard.list_directory(&link).unwrap_err();
        assert!(
            matches!(err, ProductError::PathEscape(_)),
            "list_directory should reject junction escape, got: {err:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_rejected() {
        use std::os::unix::fs::symlink;
        let (_outer, root) = make_outer_with_root();
        let guard = PathGuard::new(root.clone()).unwrap();
        let outside = root.join("../outside_target");
        fs::write(&outside, "secret").unwrap();
        let link = root.join("evil");
        symlink("../outside_target", &link).unwrap();
        let err = guard.resolve_path(&link).unwrap_err();
        assert!(matches!(err, ProductError::PathEscape(_)), "got: {err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn directory_symlink_escape_rejected() {
        use std::os::unix::fs::symlink;
        let (_outer, root) = make_outer_with_root();
        let guard = PathGuard::new(root.clone()).unwrap();
        let outside_dir = root.join("../outside_dir");
        fs::create_dir(&outside_dir).unwrap();
        fs::write(outside_dir.join("file.txt"), "secret").unwrap();
        let link = root.join("evildir");
        symlink("../outside_dir", &link).unwrap();
        let target = link.join("file.txt");
        let err = guard.resolve_path(&target).unwrap_err();
        assert!(matches!(err, ProductError::PathEscape(_)), "got: {err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_within_root_accepted() {
        use std::os::unix::fs::symlink;
        let (_outer, root) = make_outer_with_root();
        let guard = PathGuard::new(root.clone()).unwrap();
        let target = root.join("real.txt");
        fs::write(&target, "data").unwrap();
        let link = root.join("link");
        symlink("real.txt", &link).unwrap();
        let resolved = guard
            .resolve_path(&link)
            .expect("symlink within root should resolve");
        assert_eq!(resolved.canonical(), target.canonicalize().unwrap());
        assert!(resolved.canonical().starts_with(guard.root()));
    }

    #[cfg(unix)]
    #[test]
    fn capability_open_rejects_symlink_escape_created_after_guard() {
        use std::os::unix::fs::symlink;

        let (_outer, root) = make_outer_with_root();
        let outside = root.join("../outside.txt");
        fs::write(&outside, "secret").unwrap();
        let guard = PathGuard::new(root.clone()).unwrap();
        let link = root.join("escape.txt");
        symlink(&outside, &link).unwrap();

        let err = guard
            .open_file(&link, WorkspaceFileAccess::Read)
            .unwrap_err();
        assert!(matches!(err, ProductError::PathEscape(_)), "got: {err:?}");
    }

    #[cfg(unix)]
    #[test]
    fn capability_handle_rejects_symlink_swap_after_logical_check() {
        use std::os::unix::fs::symlink;

        let (_outer, root) = make_outer_with_root();
        let outside = root.join("../outside.txt");
        fs::write(&outside, "secret").unwrap();
        let safe = root.join("safe.txt");
        fs::write(&safe, "safe").unwrap();
        let guard = PathGuard::new(root).unwrap();

        // This mirrors the formerly vulnerable sequence: validate a regular
        // in-workspace file, then have another process replace it before the
        // actual open. The held capability must reject the later escape.
        guard.root_dir.canonicalize("safe.txt").unwrap();
        fs::remove_file(&safe).unwrap();
        symlink(&outside, &safe).unwrap();
        assert!(guard.root_dir.open("safe.txt").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn capability_open_stays_bound_to_original_root_after_path_replacement() {
        let outer = TempDir::new().expect("create outer temp dir");
        let root = outer.path().join("root");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("keep.txt"), "inside").unwrap();
        let guard = PathGuard::new(root.clone()).unwrap();

        // Simulate an attacker replacing the workspace path after validation. The
        // capability held by `guard` must still resolve relative names in the
        // directory that was originally opened, rather than in this replacement.
        let original_root = outer.path().join("original-root");
        fs::rename(&root, &original_root).unwrap();
        fs::create_dir(&root).unwrap();
        fs::write(root.join("keep.txt"), "replacement").unwrap();

        let mut handle = guard
            .open_file(Path::new("keep.txt"), WorkspaceFileAccess::Read)
            .expect("capability open should use original root handle");
        let mut content = String::new();
        handle.file_mut().read_to_string(&mut content).unwrap();
        assert_eq!(content, "inside");
    }

    #[test]
    fn capability_open_rejects_parent_traversal() {
        let root = make_root();
        let guard = PathGuard::new(root.path().to_path_buf()).unwrap();
        let err = guard
            .open_file(Path::new("../outside.txt"), WorkspaceFileAccess::Read)
            .unwrap_err();
        assert!(matches!(err, ProductError::PathEscape(_)), "got: {err:?}");
    }

    #[test]
    fn capability_atomic_write_does_not_create_missing_parent_directories() {
        let root = make_root();
        let guard = PathGuard::new(root.path().to_path_buf()).unwrap();
        let target = root.path().join("missing").join("file.txt");

        let err = guard.atomic_write_path(&target, b"content").unwrap_err();

        assert!(matches!(err, ProductError::PathNotFound(_)), "got: {err:?}");
        assert!(!root.path().join("missing").exists());
    }

    #[test]
    fn contains_lexical_check() {
        let root = make_root();
        let guard = PathGuard::new(root.path().to_path_buf()).unwrap();
        assert!(guard.contains(&root.path().join("foo.txt")));
        assert!(!guard.contains(Path::new("/etc/passwd")));
    }

    #[test]
    fn project_context_path_guard() {
        let root = make_root();
        let ctx = ProjectContext::new(root.path().to_path_buf()).unwrap();
        assert_eq!(ctx.mount_root, ctx.path_guard().root());
        let file = root.path().join("doc.md");
        fs::write(&file, "x").unwrap();
        let resolved = ctx.path_guard().resolve_path(&file).unwrap();
        assert!(resolved.canonical().starts_with(ctx.mount_root.as_path()));
    }

    #[test]
    fn cwd_relative_escape_rejected() {
        // cwd-escape：相对路径经 CWD 解析后逃逸出 root。
        let _lock = CWD_LOCK.lock().unwrap();
        let root = make_root();
        let _restore = RestoreCwd::new();
        std::env::set_current_dir(root.path()).unwrap();
        let guard = PathGuard::new(root.path().to_path_buf()).unwrap();
        let err = guard
            .resolve_path(Path::new("../../../../etc/passwd"))
            .unwrap_err();
        assert!(matches!(err, ProductError::PathEscape(_)), "got: {err:?}");
    }

    #[test]
    fn cwd_relative_inside_resolves() {
        let _lock = CWD_LOCK.lock().unwrap();
        let root = make_root();
        let _restore = RestoreCwd::new();
        std::env::set_current_dir(root.path()).unwrap();
        let guard = PathGuard::new(root.path().to_path_buf()).unwrap();
        let resolved = guard
            .resolve_path(Path::new("inside.txt"))
            .expect("cwd-relative path within root should resolve");
        assert_eq!(
            resolved.canonical(),
            root.path().canonicalize().unwrap().join("inside.txt")
        );
    }

    #[test]
    fn typed_resolve_path_exposes_display_path_only() {
        let root = make_root();
        let guard = PathGuard::new(root.path().to_path_buf()).unwrap();
        let dir = root.path().join("sub");
        fs::create_dir(&dir).unwrap();
        let file = dir.join("file.txt");
        fs::write(&file, "data").unwrap();

        let resolved = guard.resolve_path(&file).unwrap();
        assert_eq!(
            resolved.display(),
            path_for_display(file.canonicalize().unwrap())
        );

        let existing = guard.resolve_existing_path(&file).unwrap();
        assert_eq!(
            existing.display(),
            path_for_display(file.canonicalize().unwrap())
        );
    }

    #[test]
    fn typed_resolve_existing_path_rejects_missing() {
        let root = make_root();
        let guard = PathGuard::new(root.path().to_path_buf()).unwrap();
        let missing = root.path().join("does_not_exist.txt");
        let err = guard.resolve_existing_path(&missing).unwrap_err();
        assert!(matches!(err, ProductError::PathNotFound(_)), "got: {err:?}");
    }

    #[test]
    fn typed_handles_reject_escape() {
        let outer = TempDir::new().expect("create outer temp dir");
        let root = outer.path().join("root");
        fs::create_dir(&root).expect("create root dir");
        let guard = PathGuard::new(root).unwrap();
        let outside = outer.path().join("outside.txt");
        fs::write(&outside, "secret").unwrap();

        let err = guard.resolve_path(&outside).unwrap_err();
        assert!(matches!(err, ProductError::PathEscape(_)), "got: {err:?}");

        let err = guard
            .open_file(&outside, WorkspaceFileAccess::Read)
            .unwrap_err();
        assert!(matches!(err, ProductError::PathEscape(_)), "got: {err:?}");

        let err = guard.list_directory(&outside).unwrap_err();
        assert!(matches!(err, ProductError::PathEscape(_)), "got: {err:?}");

        let err = guard.atomic_write_path(&outside, b"x").unwrap_err();
        assert!(matches!(err, ProductError::PathEscape(_)), "got: {err:?}");
    }

    #[test]
    fn typed_open_and_list_handles_expose_display_paths() {
        let root = make_root();
        let guard = PathGuard::new(root.path().to_path_buf()).unwrap();
        let dir = root.path().join("sub");
        fs::create_dir(&dir).unwrap();
        let file = dir.join("a.txt");
        fs::write(&file, "data").unwrap();

        let opened = guard.open_file(&file, WorkspaceFileAccess::Read).unwrap();
        assert_eq!(
            opened.display(),
            path_for_display(file.canonicalize().unwrap())
        );

        let listed = guard.list_directory(&dir).unwrap();
        assert_eq!(
            listed.display(),
            path_for_display(dir.canonicalize().unwrap())
        );
        assert!(listed.entries().iter().any(|entry| entry.name == "a.txt"));
    }

    #[test]
    fn typed_write_handles_return_display_paths() {
        let root = make_root();
        let guard = PathGuard::new(root.path().to_path_buf()).unwrap();

        let target = root.path().join("new.txt");
        let written = guard.atomic_write_path(&target, b"content").unwrap();
        assert_eq!(
            written.display(),
            path_for_display(target.canonicalize().unwrap())
        );

        let fresh = root.path().join("fresh.txt");
        let created = guard.create_new_path(&fresh, b"x").unwrap();
        assert_eq!(
            created.display(),
            path_for_display(fresh.canonicalize().unwrap())
        );
    }

    /// RAII 守卫：drop 时恢复进程 CWD。
    struct RestoreCwd {
        original: PathBuf,
    }

    impl RestoreCwd {
        fn new() -> Self {
            Self {
                original: std::env::current_dir().expect("get current dir"),
            }
        }
    }

    impl Drop for RestoreCwd {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.original);
        }
    }
}
