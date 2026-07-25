//! Patch Engine -- 原子化补丁应用与内容哈希。 [doc-18 M5-04]
//!
//! 提供基于内容哈希的乐观并发控制：
//! - [`apply_patch`]：校验 base hash 后原子写入新内容
//! - [`hash_content`]：blake3 内容哈希
//!
//! ## 原子写入策略
//! 写入目标同目录的临时文件，再 `rename` 覆盖目标（POSIX 原子语义）。
//! 临时文件名带 UUID 后缀，避免并发冲突。
//!
//! [doc-06 §3.4] [doc-18 M5-04]

use std::path::Path;

use r_code_core::error::ProductError;

/// 原子化应用补丁。
///
/// 1. 读取当前文件内容
/// 2. 计算当前哈希，与 `base_hash` 比对
/// 3. 不匹配 -> [`PatchError::VersionConflict`]
/// 4. 匹配 -> 原子写入 `new_content`（临时文件 + rename）
pub async fn apply_patch(
    file_path: &Path,
    base_hash: &str,
    new_content: &[u8],
) -> Result<(), PatchError> {
    // 1. 读取当前文件内容
    let current_content = std::fs::read(file_path)
        .map_err(|e| PatchError::IoError(format!("failed to read {}: {e}", file_path.display())))?;

    // 2. 计算并校验哈希
    let current_hash = hash_content(&current_content);
    if current_hash != base_hash {
        return Err(PatchError::VersionConflict {
            expected: base_hash.to_string(),
            actual: current_hash,
        });
    }

    // 3. 原子写入新内容
    atomic_write(file_path, new_content).map_err(|e| {
        PatchError::IoError(format!("failed to write {}: {e}", file_path.display()))
    })?;

    Ok(())
}

/// 计算内容的 blake3 哈希（十六进制字符串）。
pub fn hash_content(content: &[u8]) -> String {
    blake3::hash(content).to_hex().to_string()
}

/// 原子写入：先写同目录临时文件，再 rename 覆盖目标。
fn atomic_write(path: &Path, content: &[u8]) -> Result<(), std::io::Error> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let tmp_name = format!(".{file_name}.r-code-tmp-{}", uuid::Uuid::new_v4());
    let tmp_path = dir.join(tmp_name);

    // 写入临时文件
    std::fs::write(&tmp_path, content)?;

    // 原子 rename（POSIX 上 rename 是原子的）
    std::fs::rename(&tmp_path, path)?;

    Ok(())
}

/// 补丁应用错误。
#[derive(Debug, Clone)]
pub enum PatchError {
    /// Base hash 与当前文件内容不匹配（版本冲突）
    VersionConflict {
        /// 期望的哈希值
        expected: String,
        /// 实际的哈希值
        actual: String,
    },
    /// IO 错误
    IoError(String),
    /// ProductError 包装
    Product(ProductError),
}

impl PartialEq for PatchError {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::VersionConflict {
                    expected: e1,
                    actual: a1,
                },
                Self::VersionConflict {
                    expected: e2,
                    actual: a2,
                },
            ) => e1 == e2 && a1 == a2,
            (Self::IoError(m1), Self::IoError(m2)) => m1 == m2,
            (Self::Product(e1), Self::Product(e2)) => e1.to_string() == e2.to_string(),
            _ => false,
        }
    }
}

impl Eq for PatchError {}

impl PatchError {
    /// 是否为版本冲突错误。
    pub fn is_version_conflict(&self) -> bool {
        matches!(self, Self::VersionConflict { .. })
    }
}

impl std::fmt::Display for PatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::VersionConflict { expected, actual } => {
                write!(f, "VERSION_CONFLICT: expected={expected}, actual={actual}")
            }
            Self::IoError(msg) => write!(f, "IO error: {msg}"),
            Self::Product(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for PatchError {}

impl From<PatchError> for ProductError {
    fn from(e: PatchError) -> Self {
        match e {
            PatchError::VersionConflict { .. } => Self::BaselineError(format!("{e}")),
            PatchError::IoError(msg) => Self::Other(msg),
            PatchError::Product(e) => e,
        }
    }
}

impl From<ProductError> for PatchError {
    fn from(e: ProductError) -> Self {
        Self::Product(e)
    }
}

impl From<std::io::Error> for PatchError {
    fn from(e: std::io::Error) -> Self {
        Self::IoError(e.to_string())
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn apply_patch_success() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.txt");
        let original = b"hello world";
        fs::write(&file, original).unwrap();

        let base_hash = hash_content(original);
        let new_content = b"hello rust";

        apply_patch(&file, &base_hash, new_content).await.unwrap();

        let result = fs::read_to_string(&file).unwrap();
        assert_eq!(result, "hello rust");
    }

    #[tokio::test]
    async fn apply_patch_version_conflict() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.txt");
        fs::write(&file, b"actual content").unwrap();

        // 传入错误的 base_hash
        let wrong_hash = hash_content(b"different content");
        let result = apply_patch(&file, &wrong_hash, b"new content").await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, PatchError::VersionConflict { .. }));
        assert!(err.is_version_conflict());

        // 文件内容不应被修改
        let content = fs::read_to_string(&file).unwrap();
        assert_eq!(content, "actual content");
    }

    #[tokio::test]
    async fn apply_patch_nonexistent_file() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("nonexistent.txt");

        let result = apply_patch(&file, "some_hash", b"content").await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), PatchError::IoError(_)));
    }

    #[tokio::test]
    async fn apply_patch_no_temp_left_behind() {
        // 确保成功后临时文件被清理（renamed away）
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("clean.txt");
        fs::write(&file, b"original").unwrap();

        let base_hash = hash_content(b"original");
        apply_patch(&file, &base_hash, b"updated").await.unwrap();

        // 不应有 .r-code-tmp 文件残留
        let entries: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(entries, vec!["clean.txt".to_string()]);
    }

    #[test]
    fn hash_content_consistent() {
        let content = b"test content";
        let hash1 = hash_content(content);
        let hash2 = hash_content(content);

        assert_eq!(hash1, hash2);
        // blake3 hex 是 64 字符
        assert_eq!(hash1.len(), 64);
    }

    #[test]
    fn hash_content_different_inputs() {
        let h1 = hash_content(b"foo");
        let h2 = hash_content(b"bar");
        assert_ne!(h1, h2);
    }

    #[test]
    fn hash_content_empty() {
        let hash = hash_content(b"");
        assert_eq!(hash.len(), 64);
        // blake3 of empty input
        assert_eq!(
            hash,
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
        );
    }

    #[test]
    fn patch_error_display() {
        let e = PatchError::VersionConflict {
            expected: "abc".to_string(),
            actual: "def".to_string(),
        };
        let s = format!("{e}");
        assert!(s.contains("VERSION_CONFLICT"));
        assert!(s.contains("abc"));
        assert!(s.contains("def"));

        let e2 = PatchError::IoError("disk full".to_string());
        assert!(format!("{e2}").contains("IO error"));
        assert!(format!("{e2}").contains("disk full"));
    }

    #[test]
    fn patch_error_to_product_error() {
        let e = PatchError::VersionConflict {
            expected: "a".to_string(),
            actual: "b".to_string(),
        };
        let p: ProductError = e.into();
        assert!(matches!(p, ProductError::BaselineError(_)));

        let e2 = PatchError::IoError("fail".to_string());
        let p2: ProductError = e2.into();
        assert!(matches!(p2, ProductError::Other(_)));

        let e3 = PatchError::Product(ProductError::RollbackError("x".to_string()));
        let p3: ProductError = e3.into();
        assert!(matches!(p3, ProductError::RollbackError(_)));
    }

    #[test]
    fn patch_error_from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let patch_err: PatchError = io_err.into();
        assert!(matches!(patch_err, PatchError::IoError(_)));
    }
}
