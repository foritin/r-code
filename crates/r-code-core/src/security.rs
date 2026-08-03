//! 路径边界安全：`PathGuard` 与 `ProjectContext`。
//!
//! 实现 doc-07 §4 / §11 定义的工作区路径边界模型：所有文件操作必须经
//! [`PathGuard::resolve`] 解析为 canonical 路径并校验 containment，拒绝任何
//! 逃逸尝试（符号链接、`..` 穿越、相对路径 cwd 逃逸）。
//!
//! ## 安全不变量
//! - **Fail-closed**：任何无法确证 containment 的路径一律返回
//!   [`ProductError::PathEscape`]，包括权限不足、IO 错误等情况。
//! - **TOCTOU 防护**：在 canonical 化之后重新校验 containment，从不信任输入
//!   路径的词法前缀。
//! - **符号链接解析**：`resolve` 通过 `canonicalize` 解析全部符号链接。
//!
//! [doc-07 §4, §11]: 路径边界 / Rust 实现要点

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::error::ProductError;

/// 工作区路径边界守卫 [doc-07 §4, §11]。
///
/// 持有一个已 canonical 化的 root，提供 containment 校验与安全路径解析。
/// 所有需要访问文件系统的工具（`read_file`、`write_file` 等）必须先经
/// [`PathGuard::resolve`] 解析目标路径，再使用返回的 canonical 路径执行 IO。
#[derive(Debug, Clone)]
pub struct PathGuard {
    root: PathBuf,
    // Keep the caller-visible spelling too. On macOS, temporary paths are
    // commonly exposed as `/var/...` while canonicalization returns
    // `/private/var/...`; the lexical helper must recognize both spellings.
    lexical_root: PathBuf,
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
        Ok(Self {
            root: canonical,
            lexical_root,
        })
    }

    /// 将 `path` 解析为相对 root 的 canonical 路径，确保不逃逸。
    ///
    /// - 若路径已存在：直接 `canonicalize`（解析全部符号链接与 `..`），
    ///   并在 canonical 结果上重新校验 containment（TOCTOU 防护：从不信任
    ///   输入路径的词法前缀）。
    /// - 若路径尚不存在（如即将创建的文件）：canonical 化最近的现存祖先，
    ///   重新拼接不存在的尾段并校验。
    ///
    /// 任何错误（权限不足、IO 错误等）均 fail-closed 返回
    /// [`ProductError::PathEscape`]。
    pub fn resolve(&self, path: &Path) -> Result<PathBuf, ProductError> {
        match path.canonicalize() {
            Ok(canonical) => {
                if canonical.starts_with(&self.root) {
                    Ok(canonical)
                } else {
                    Err(ProductError::PathEscape(format!(
                        "path {path:?} canonicalizes to {canonical:?} outside root {:?}",
                        self.root
                    )))
                }
            }
            Err(err) if err.kind() == ErrorKind::NotFound => self.resolve_via_ancestor(path),
            Err(err) => Err(ProductError::PathEscape(format!(
                "cannot canonicalize {path:?}: {err} (fail-closed)"
            ))),
        }
    }

    /// 与 [`resolve`](Self::resolve) 类似，但要求路径必须已存在。
    ///
    /// 用于只读工具（`list_files`、`read_file` 等）：路径不存在时直接返回
    /// [`ProductError::PathNotFound`]，而不是通过祖先目录推断 containment。
    /// 后者是写入工具（如 `create_file`）的语义——文件尚不存在但父目录在工作区内。
    pub fn resolve_existing(&self, path: &Path) -> Result<PathBuf, ProductError> {
        match path.canonicalize() {
            Ok(canonical) => {
                if canonical.starts_with(&self.root) {
                    Ok(canonical)
                } else {
                    Err(ProductError::PathEscape(format!(
                        "path {path:?} canonicalizes to {canonical:?} outside root {:?}",
                        self.root
                    )))
                }
            }
            Err(err) if err.kind() == ErrorKind::NotFound => Err(ProductError::PathNotFound(
                format!("path does not exist: {path:?}"),
            )),
            Err(err) => Err(ProductError::PathEscape(format!(
                "cannot canonicalize {path:?}: {err} (fail-closed)"
            ))),
        }
    }

    /// 为尚不存在的路径解析 containment：找到最近的现存祖先并重新拼接尾段。
    ///
    /// 尾段中若出现 `..` / `.`（无法被祖先 canonical 化消解的相对组件），
    /// 视为逃逸尝试，fail-closed 拒绝。
    fn resolve_via_ancestor(&self, path: &Path) -> Result<PathBuf, ProductError> {
        // 相对路径以 CWD 为锚，使向上查找拥有绝对基（与 canonicalize 的相对
        // 路径解析语义一致；否则无目录段的相对路径 parent 会得到空路径）。
        let anchored: PathBuf = if path.is_relative() {
            let cwd = std::env::current_dir().map_err(|err| {
                ProductError::PathEscape(format!(
                    "cannot get cwd for {path:?}: {err} (fail-closed)"
                ))
            })?;
            cwd.join(path)
        } else {
            path.to_path_buf()
        };

        let mut tail: Vec<std::ffi::OsString> = Vec::new();
        let mut current: &Path = &anchored;
        loop {
            match current.canonicalize() {
                Ok(canonical_ancestor) => {
                    if !canonical_ancestor.starts_with(&self.root) {
                        return Err(ProductError::PathEscape(format!(
                            "ancestor of {path:?} canonicalizes to {canonical_ancestor:?} outside root {:?}",
                            self.root
                        )));
                    }
                    let mut resolved = canonical_ancestor;
                    for name in tail.into_iter().rev() {
                        resolved.push(name);
                    }
                    // 防御性二次校验：拼接结果仍须落在 root 内。
                    if !resolved.starts_with(&self.root) {
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
                Err(err) => {
                    return Err(ProductError::PathEscape(format!(
                        "cannot canonicalize ancestor of {path:?}: {err} (fail-closed)"
                    )));
                }
            }
        }
    }

    /// 词法检查 `path` 是否在 root 内（不解析符号链接）。
    ///
    /// **非权威**：仅做快速前缀判断，不解析符号链接或 `..`。任何安全决策
    /// 必须使用 [`resolve`](Self::resolve)。
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
    use std::sync::Mutex;
    use tempfile::TempDir;

    // set_current_dir 是进程级全局状态，cwd 相关测试需串行化。
    static CWD_LOCK: Mutex<()> = Mutex::new(());

    /// 直接以 TempDir 作为 root。
    fn make_root() -> TempDir {
        TempDir::new().expect("create temp dir")
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
            .resolve(&file)
            .expect("path within root should resolve");
        assert_eq!(resolved, file.canonicalize().unwrap());
        assert!(resolved.starts_with(guard.root()));
    }

    #[test]
    fn path_outside_root_rejected() {
        let root = make_root();
        let guard = PathGuard::new(root.path().to_path_buf()).unwrap();
        let err = guard.resolve(Path::new("/etc/passwd")).unwrap_err();
        assert!(matches!(err, ProductError::PathEscape(_)), "got: {err:?}");
    }

    #[test]
    fn root_itself_accepted() {
        let root = make_root();
        let guard = PathGuard::new(root.path().to_path_buf()).unwrap();
        let resolved = guard.resolve(guard.root()).expect("root should resolve");
        assert_eq!(resolved, guard.root());
    }

    #[test]
    fn nonexistent_path_within_root_resolves() {
        let root = make_root();
        let guard = PathGuard::new(root.path().to_path_buf()).unwrap();
        let target = root.path().join("newfile.txt");
        assert!(!target.exists());
        let resolved = guard
            .resolve(&target)
            .expect("non-existent path within root should resolve");
        assert!(!resolved.exists());
        assert_eq!(
            resolved,
            root.path().canonicalize().unwrap().join("newfile.txt")
        );
        assert!(resolved.starts_with(guard.root()));
    }

    #[test]
    fn deeply_nonexistent_path_within_root_resolves() {
        let root = make_root();
        let guard = PathGuard::new(root.path().to_path_buf()).unwrap();
        let target = root.path().join("a/b/c/new.txt");
        let resolved = guard
            .resolve(&target)
            .expect("deeply non-existent path within root should resolve");
        assert_eq!(
            resolved,
            root.path().canonicalize().unwrap().join("a/b/c/new.txt")
        );
    }

    #[test]
    fn nonexistent_path_outside_root_rejected() {
        let root = make_root();
        let guard = PathGuard::new(root.path().to_path_buf()).unwrap();
        let target = Path::new("/etc/r_code_nonexistent_marker_xyz.txt");
        let err = guard.resolve(target).unwrap_err();
        assert!(matches!(err, ProductError::PathEscape(_)), "got: {err:?}");
    }

    #[test]
    fn resolve_existing_accepts_existing_path() {
        let root = make_root();
        let guard = PathGuard::new(root.path().to_path_buf()).unwrap();
        let file = root.path().join("hello.txt");
        fs::write(&file, "hi").unwrap();
        let resolved = guard
            .resolve_existing(&file)
            .expect("existing path should resolve");
        assert_eq!(resolved, file.canonicalize().unwrap());
    }

    #[test]
    fn resolve_existing_rejects_nonexistent_path() {
        let root = make_root();
        let guard = PathGuard::new(root.path().to_path_buf()).unwrap();
        let target = root.path().join("does_not_exist.txt");
        assert!(!target.exists());
        let err = guard.resolve_existing(&target).unwrap_err();
        assert!(matches!(err, ProductError::PathNotFound(_)), "got: {err:?}");
    }

    #[test]
    fn resolve_existing_rejects_escape() {
        // 用外层 TempDir 包裹 root，在 root 之外放一个存在的文件，
        // 验证 resolve_existing 对"存在但逃逸"的路径返回 PathEscape。
        let outer = TempDir::new().expect("create outer temp dir");
        let root = outer.path().join("root");
        fs::create_dir(&root).expect("create root dir");
        let guard = PathGuard::new(root).unwrap();
        let outside = outer.path().join("outside.txt");
        fs::write(&outside, "outside").unwrap();
        let err = guard.resolve_existing(&outside).unwrap_err();
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
        let err = guard.resolve(&escape).unwrap_err();
        assert!(matches!(err, ProductError::PathEscape(_)), "got: {err:?}");
    }

    #[test]
    fn fail_closed_on_nonexistent_root() {
        let bogus = make_root().path().join("does/not/exist");
        let err = PathGuard::new(bogus).unwrap_err();
        assert!(matches!(err, ProductError::PathEscape(_)), "got: {err:?}");
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
        let err = guard.resolve(&link).unwrap_err();
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
        let err = guard.resolve(&target).unwrap_err();
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
            .resolve(&link)
            .expect("symlink within root should resolve");
        assert_eq!(resolved, target.canonicalize().unwrap());
        assert!(resolved.starts_with(guard.root()));
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
        let resolved = ctx.path_guard().resolve(&file).unwrap();
        assert!(resolved.starts_with(ctx.mount_root.as_path()));
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
            .resolve(Path::new("../../../../etc/passwd"))
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
            .resolve(Path::new("inside.txt"))
            .expect("cwd-relative path within root should resolve");
        assert_eq!(
            resolved,
            root.path().canonicalize().unwrap().join("inside.txt")
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
