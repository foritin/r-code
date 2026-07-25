//! DocumentStore -- 文件内容跟踪（revision/hash/dirty 模型）。
//!
//! 跟踪 workspace 内文件的修订号、内容哈希与脏状态，用于：
//! - 外部磁盘变更检测（`check_conflict`）
//! - 自写抑制（own-write suppression，避免我们自己写入触发的变更通知）
//! - 缓冲区脏状态管理
//!
//! 内容哈希使用 blake3。二进制检测：前 8KB 内是否含 null 字节。
//! 大文件上限：> 10MB 的文件不进行内容跟踪。
//! BOM 检测：识别 UTF-8 / UTF-16 LE / UTF-16 BE BOM。
//!
//! [doc-18 M3-03/M3-05/M3-07]

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use r_code_core::error::ProductError;

/// 文档跟踪条目。
#[derive(Debug, Clone)]
pub struct DocumentEntry {
    /// 文件路径（相对 workspace root；root 外的为绝对路径）
    pub path: PathBuf,
    /// 内容哈希（blake3 hex）
    pub content_hash: String,
    /// 修订号（每次内容变更递增）
    pub revision: u64,
    /// 缓冲区是否有未保存变更
    pub dirty: bool,
    /// 文件大小（字节）
    pub size: u64,
    /// 是否为二进制文件
    pub is_binary: bool,
}

/// DocumentStore -- 跟踪文件修订、内容哈希与脏状态。
pub struct DocumentStore {
    root: PathBuf,
    documents: HashMap<PathBuf, DocumentEntry>,
    /// 当前正在由我们写入的路径集合（用于自写抑制）
    own_writes: HashSet<PathBuf>,
}

/// 大文件跟踪上限（10MB）。
const MAX_TRACK_SIZE: u64 = 10 * 1024 * 1024;

/// 二进制检测窗口（前 8KB）。
const BINARY_CHECK_LEN: usize = 8 * 1024;

impl DocumentStore {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            documents: HashMap::new(),
            own_writes: HashSet::new(),
        }
    }

    /// 打开/读取磁盘上的文件，创建（或刷新）一个 `DocumentEntry`。
    ///
    /// - 检测二进制文件（前 8KB 内 null 字节）
    /// - 大文件（> 10MB）跳过内容跟踪并返回错误
    /// - 处理 BOM 与不同行尾（按原始字节哈希，保证一致性）
    /// - 若内容未变则返回既有条目（保留 dirty 状态）
    /// - 若内容已变则递增 revision
    pub fn open(&mut self, path: &Path) -> Result<DocumentEntry, ProductError> {
        let key = self.to_key(path);
        let read_path = self.to_read(&key);

        let metadata = std::fs::metadata(&read_path)?;
        if !metadata.is_file() {
            return Err(ProductError::Other(format!(
                "not a regular file: {}",
                read_path.display()
            )));
        }
        let size = metadata.len();
        if size > MAX_TRACK_SIZE {
            return Err(ProductError::Other(format!(
                "file too large to track ({} bytes > {} max): {}",
                size,
                MAX_TRACK_SIZE,
                read_path.display()
            )));
        }

        let bytes = std::fs::read(&read_path)?;
        let is_binary = detect_binary(&bytes);
        let bom = detect_bom(&bytes);
        let content_hash = blake3::hash(&bytes).to_hex().to_string();

        // 内容未变 -> 返回既有条目（保留 dirty 状态）
        if let Some(existing) = self.documents.get(&key) {
            if existing.content_hash == content_hash {
                tracing::trace!(
                    path = %read_path.display(),
                    ?bom,
                    is_binary,
                    revision = existing.revision,
                    "re-opened unchanged document"
                );
                return Ok(existing.clone());
            }
        }

        // 内容已变 -> 递增 revision；新文件 revision = 1
        let revision = self
            .documents
            .get(&key)
            .map(|e| e.revision + 1)
            .unwrap_or(1);
        let entry = DocumentEntry {
            path: key.clone(),
            content_hash,
            revision,
            dirty: false,
            size,
            is_binary,
        };
        self.documents.insert(key, entry.clone());
        tracing::trace!(
            path = %read_path.display(),
            ?bom,
            is_binary,
            revision,
            "opened document"
        );
        Ok(entry)
    }

    /// 按路径获取文档条目。
    pub fn get(&self, path: &Path) -> Option<&DocumentEntry> {
        let key = self.to_key(path);
        self.documents.get(&key)
    }

    /// 标记文档为脏（有未保存变更）。
    pub fn mark_dirty(&mut self, path: &Path) {
        let key = self.to_key(path);
        if let Some(e) = self.documents.get_mut(&key) {
            e.dirty = true;
        }
    }

    /// 标记文档为干净（已保存）。
    pub fn mark_clean(&mut self, path: &Path) {
        let key = self.to_key(path);
        if let Some(e) = self.documents.get_mut(&key) {
            e.dirty = false;
        }
    }

    /// 开始自写（抑制该路径的外部变更通知）。
    pub fn begin_own_write(&mut self, path: &Path) {
        let key = self.to_key(path);
        self.own_writes.insert(key);
    }

    /// 结束自写（恢复该路径的外部变更通知）。
    pub fn end_own_write(&mut self, path: &Path) {
        let key = self.to_key(path);
        self.own_writes.remove(&key);
    }

    /// 检查某路径是否正在由我们写入。
    pub fn is_own_write(&self, path: &Path) -> bool {
        let key = self.to_key(path);
        self.own_writes.contains(&key)
    }

    /// 检查磁盘文件自上次读取后是否已变更。
    ///
    /// - 返回 `Conflict`：磁盘哈希 != 我们的哈希，且缓冲区有未保存变更
    /// - 返回 `ChangedOnDisk`：磁盘哈希 != 我们的哈希，但缓冲区无未保存变更
    /// - 返回 `Clean`：未变更 / 未跟踪 / 正在自写（抑制）
    pub fn check_conflict(&self, path: &Path) -> Result<ConflictStatus, ProductError> {
        let key = self.to_key(path);
        let entry = match self.documents.get(&key) {
            Some(e) => e,
            None => return Ok(ConflictStatus::Clean),
        };
        // 自写抑制：我们自己的写入不应视为外部冲突
        if self.own_writes.contains(&key) {
            return Ok(ConflictStatus::Clean);
        }
        let read_path = self.to_read(&key);
        let disk_hash = hash_file(&read_path)?;
        if disk_hash == entry.content_hash {
            Ok(ConflictStatus::Clean)
        } else if entry.dirty {
            Ok(ConflictStatus::Conflict)
        } else {
            Ok(ConflictStatus::ChangedOnDisk)
        }
    }

    /// 保存后更新内容哈希。若新哈希与旧哈希不同，递增 revision；并清除 dirty。
    pub fn update_hash(&mut self, path: &Path, new_hash: String) {
        let key = self.to_key(path);
        if let Some(e) = self.documents.get_mut(&key) {
            if e.content_hash != new_hash {
                e.content_hash = new_hash;
                e.revision += 1;
            }
            e.dirty = false;
        }
    }

    /// 从跟踪中移除一个文档。
    pub fn remove(&mut self, path: &Path) {
        let key = self.to_key(path);
        self.documents.remove(&key);
        self.own_writes.remove(&key);
    }

    /// 列出所有被跟踪的文档。
    pub fn list(&self) -> Vec<&DocumentEntry> {
        self.documents.values().collect()
    }

    // ------------------------------------------------------------------------
    // 内部辅助
    // ------------------------------------------------------------------------

    /// 将任意路径归一化为跟踪键：root 内的转为相对路径，root 外的保持绝对。
    fn to_key(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.strip_prefix(&self.root)
                .map(Path::to_path_buf)
                .unwrap_or_else(|_| path.to_path_buf())
        } else {
            path.to_path_buf()
        }
    }

    /// 将跟踪键转为磁盘读取路径。
    /// `root.join(absolute)` 会替换为绝对路径，故 root 内外均正确。
    fn to_read(&self, key: &Path) -> PathBuf {
        self.root.join(key)
    }
}

/// 冲突状态（外部变更检测）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConflictStatus {
    /// 无冲突 -- 文件未变或未跟踪
    Clean,
    /// 磁盘已变更，但缓冲区无未保存变更
    ChangedOnDisk,
    /// 磁盘已变更 且 缓冲区有未保存变更 -- 冲突
    Conflict,
}

// ============================================================================
// 辅助函数
// ============================================================================

/// 检测二进制内容：前 8KB 内是否含 null 字节。
pub fn detect_binary(bytes: &[u8]) -> bool {
    let end = bytes.len().min(BINARY_CHECK_LEN);
    bytes[..end].contains(&0u8)
}

/// BOM 种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BomKind {
    /// UTF-8 (EF BB BF)
    Utf8,
    /// UTF-16 LE (FF FE)
    Utf16Le,
    /// UTF-16 BE (FE FF)
    Utf16Be,
}

/// 检测文件起始的 BOM。
pub fn detect_bom(bytes: &[u8]) -> Option<BomKind> {
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        Some(BomKind::Utf8)
    } else if bytes.starts_with(&[0xFF, 0xFE]) {
        Some(BomKind::Utf16Le)
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        Some(BomKind::Utf16Be)
    } else {
        None
    }
}

/// 增量计算文件的 blake3 哈希（hex），适用于任意大小。
fn hash_file(path: &Path) -> Result<String, ProductError> {
    let mut hasher = blake3::Hasher::new();
    let mut file = std::fs::File::open(path)?;
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    /// 创建一个带临时 root 的 DocumentStore。
    fn setup() -> (TempDir, DocumentStore) {
        let tmp = TempDir::new().unwrap();
        let store = DocumentStore::new(tmp.path().to_path_buf());
        (tmp, store)
    }

    /// 写入一个文件（相对 root），返回相对路径。
    fn write_file(root: &Path, rel: &str, content: &[u8]) -> PathBuf {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content).unwrap();
        path
    }

    // --------------------------------------------------------------------------
    // open
    // --------------------------------------------------------------------------

    #[test]
    fn test_open_text_file() {
        let (tmp, mut store) = setup();
        write_file(tmp.path(), "foo.txt", b"hello world");

        let entry = store.open(Path::new("foo.txt")).unwrap();
        assert_eq!(entry.path, Path::new("foo.txt"));
        assert_eq!(entry.size, 11);
        assert_eq!(entry.revision, 1);
        assert!(!entry.dirty);
        assert!(!entry.is_binary);
        assert!(!entry.content_hash.is_empty());
        // 与 blake3 直接计算一致
        assert_eq!(
            entry.content_hash,
            blake3::hash(b"hello world").to_hex().to_string()
        );
    }

    #[test]
    fn test_open_empty_file() {
        let (tmp, mut store) = setup();
        write_file(tmp.path(), "empty.txt", b"");

        let entry = store.open(Path::new("empty.txt")).unwrap();
        assert_eq!(entry.size, 0);
        assert!(!entry.is_binary);
        assert_eq!(entry.revision, 1);
        assert_eq!(entry.content_hash, blake3::hash(b"").to_hex().to_string());
    }

    #[test]
    fn test_open_binary_file() {
        let (tmp, mut store) = setup();
        write_file(tmp.path(), "blob.bin", b"\x00\x01\x02\x00\xff");

        let entry = store.open(Path::new("blob.bin")).unwrap();
        assert!(entry.is_binary);
    }

    #[test]
    fn test_open_binary_detection_window() {
        let (tmp, mut store) = setup();
        // null 字节在 8KB 窗口内（最后一个字节）-> 二进制
        let mut content = vec![b'a'; BINARY_CHECK_LEN];
        content[BINARY_CHECK_LEN - 1] = 0u8;
        write_file(tmp.path(), "win.bin", &content);
        let entry = store.open(Path::new("win.bin")).unwrap();
        assert!(entry.is_binary);
    }

    #[test]
    fn test_open_binary_detection_beyond_window() {
        let (tmp, mut store) = setup();
        // null 字节在第 8KB 之后 -> 不视为二进制（超出检测窗口）
        let mut content = vec![b'a'; BINARY_CHECK_LEN + 10];
        content[BINARY_CHECK_LEN + 5] = 0u8;
        write_file(tmp.path(), "beyond.bin", &content);
        let entry = store.open(Path::new("beyond.bin")).unwrap();
        assert!(!entry.is_binary);
    }

    #[test]
    fn test_open_large_file_rejected() {
        let (tmp, mut store) = setup();
        // 写入略超 10MB 的文件
        let big = vec![b'x'; (MAX_TRACK_SIZE + 1) as usize];
        write_file(tmp.path(), "big.txt", &big);

        let err = store.open(Path::new("big.txt")).unwrap_err();
        assert!(
            matches!(err, ProductError::Other(ref m) if m.contains("too large")),
            "expected too-large error, got: {err:?}"
        );
        // 未被跟踪
        assert!(store.get(Path::new("big.txt")).is_none());
    }

    #[test]
    fn test_open_at_size_cap() {
        let (tmp, mut store) = setup();
        // 恰好 10MB -> 允许跟踪
        let exact = vec![b'x'; MAX_TRACK_SIZE as usize];
        write_file(tmp.path(), "cap.txt", &exact);
        let entry = store.open(Path::new("cap.txt")).unwrap();
        assert_eq!(entry.size, MAX_TRACK_SIZE);
    }

    #[test]
    fn test_open_bom_utf8() {
        let (tmp, mut store) = setup();
        let mut content = vec![0xEF, 0xBB, 0xBF];
        content.extend_from_slice(b"bom text");
        write_file(tmp.path(), "bom8.txt", &content);

        let entry = store.open(Path::new("bom8.txt")).unwrap();
        assert!(!entry.is_binary);
        assert_eq!(detect_bom(&content), Some(BomKind::Utf8));
    }

    #[test]
    fn test_open_bom_utf16_le() {
        let (tmp, mut store) = setup();
        let content = vec![0xFF, 0xFE, b'h', 0x00, b'i', 0x00];
        write_file(tmp.path(), "bom16.txt", &content);

        let entry = store.open(Path::new("bom16.txt")).unwrap();
        // UTF-16 LE 含 null 字节 -> 二进制
        assert!(entry.is_binary);
        assert_eq!(detect_bom(&content), Some(BomKind::Utf16Le));
    }

    #[test]
    fn test_open_different_line_endings_hash_consistent() {
        let (tmp, mut store) = setup();
        write_file(tmp.path(), "lf.txt", b"line1\nline2\n");
        write_file(tmp.path(), "crlf.txt", b"line1\r\nline2\r\n");

        let lf = store.open(Path::new("lf.txt")).unwrap();
        let crlf = store.open(Path::new("crlf.txt")).unwrap();
        // 不同行尾 -> 不同原始字节 -> 不同哈希
        assert_ne!(lf.content_hash, crlf.content_hash);
        assert_eq!(
            lf.content_hash,
            blake3::hash(b"line1\nline2\n").to_hex().to_string()
        );
    }

    #[test]
    fn test_open_re_open_unchanged_preserves_dirty() {
        let (tmp, mut store) = setup();
        write_file(tmp.path(), "foo.txt", b"content");

        let entry = store.open(Path::new("foo.txt")).unwrap();
        assert_eq!(entry.revision, 1);
        assert!(!entry.dirty);

        // 标记脏后重新打开（磁盘未变）-> 保留脏状态、revision 不变
        store.mark_dirty(Path::new("foo.txt"));
        let again = store.open(Path::new("foo.txt")).unwrap();
        assert_eq!(again.revision, 1);
        assert!(again.dirty);
    }

    #[test]
    fn test_open_re_open_changed_bumps_revision() {
        let (tmp, mut store) = setup();
        write_file(tmp.path(), "foo.txt", b"v1");

        let e1 = store.open(Path::new("foo.txt")).unwrap();
        assert_eq!(e1.revision, 1);

        // 磁盘变更后重新打开 -> revision 递增、清除 dirty
        write_file(tmp.path(), "foo.txt", b"v2");
        let e2 = store.open(Path::new("foo.txt")).unwrap();
        assert_eq!(e2.revision, 2);
        assert!(!e2.dirty);
        assert_ne!(e1.content_hash, e2.content_hash);
    }

    #[test]
    fn test_open_nonexistent_returns_error() {
        let (_tmp, mut store) = setup();
        let err = store.open(Path::new("nope.txt")).unwrap_err();
        assert!(matches!(err, ProductError::Other(_)));
    }

    #[test]
    fn test_open_directory_returns_error() {
        let (tmp, mut store) = setup();
        std::fs::create_dir_all(tmp.path().join("adir")).unwrap();
        let err = store.open(Path::new("adir")).unwrap_err();
        assert!(matches!(err, ProductError::Other(_)));
    }

    // --------------------------------------------------------------------------
    // get / list / remove
    // --------------------------------------------------------------------------

    #[test]
    fn test_get_tracked_and_untracked() {
        let (tmp, mut store) = setup();
        write_file(tmp.path(), "foo.txt", b"hi");
        store.open(Path::new("foo.txt")).unwrap();

        assert!(store.get(Path::new("foo.txt")).is_some());
        assert!(store.get(Path::new("nope.txt")).is_none());
    }

    #[test]
    fn test_relative_and_absolute_path_consistency() {
        let (tmp, mut store) = setup();
        write_file(tmp.path(), "foo.txt", b"hi");

        // 用相对路径打开
        store.open(Path::new("foo.txt")).unwrap();
        // 用绝对路径获取 -> 同一条目
        let abs = tmp.path().join("foo.txt");
        assert!(store.get(&abs).is_some());
        // 用绝对路径再开也应命中同一条目（revision 不变）
        let again = store.open(&abs).unwrap();
        assert_eq!(again.revision, 1);
    }

    #[test]
    fn test_list() {
        let (tmp, mut store) = setup();
        write_file(tmp.path(), "a.txt", b"a");
        write_file(tmp.path(), "b.txt", b"b");
        write_file(tmp.path(), "c.txt", b"c");
        store.open(Path::new("a.txt")).unwrap();
        store.open(Path::new("b.txt")).unwrap();
        store.open(Path::new("c.txt")).unwrap();

        assert_eq!(store.list().len(), 3);
    }

    #[test]
    fn test_remove() {
        let (tmp, mut store) = setup();
        write_file(tmp.path(), "foo.txt", b"hi");
        store.open(Path::new("foo.txt")).unwrap();
        assert!(store.get(Path::new("foo.txt")).is_some());

        store.remove(Path::new("foo.txt"));
        assert!(store.get(Path::new("foo.txt")).is_none());
        // 移除也清除 own_write
        assert!(!store.is_own_write(Path::new("foo.txt")));
    }

    // --------------------------------------------------------------------------
    // mark_dirty / mark_clean / update_hash
    // --------------------------------------------------------------------------

    #[test]
    fn test_mark_dirty_and_clean() {
        let (tmp, mut store) = setup();
        write_file(tmp.path(), "foo.txt", b"hi");
        store.open(Path::new("foo.txt")).unwrap();

        assert!(!store.get(Path::new("foo.txt")).unwrap().dirty);
        store.mark_dirty(Path::new("foo.txt"));
        assert!(store.get(Path::new("foo.txt")).unwrap().dirty);
        store.mark_clean(Path::new("foo.txt"));
        assert!(!store.get(Path::new("foo.txt")).unwrap().dirty);
    }

    #[test]
    fn test_mark_dirty_untracked_noop() {
        let (_tmp, mut store) = setup();
        // 不存在的文档 -> 不 panic
        store.mark_dirty(Path::new("nope.txt"));
        store.mark_clean(Path::new("nope.txt"));
    }

    #[test]
    fn test_update_hash_same_hash_no_bump() {
        let (tmp, mut store) = setup();
        write_file(tmp.path(), "foo.txt", b"hi");
        let entry = store.open(Path::new("foo.txt")).unwrap();
        store.mark_dirty(Path::new("foo.txt"));

        // 相同哈希 -> 不递增 revision，但清除 dirty
        store.update_hash(Path::new("foo.txt"), entry.content_hash.clone());
        let after = store.get(Path::new("foo.txt")).unwrap();
        assert_eq!(after.revision, entry.revision);
        assert!(!after.dirty);
    }

    #[test]
    fn test_update_hash_different_hash_bumps_revision() {
        let (tmp, mut store) = setup();
        write_file(tmp.path(), "foo.txt", b"hi");
        let entry = store.open(Path::new("foo.txt")).unwrap();
        let new_hash = blake3::hash(b"new content").to_hex().to_string();

        store.update_hash(Path::new("foo.txt"), new_hash.clone());
        let after = store.get(Path::new("foo.txt")).unwrap();
        assert_eq!(after.revision, entry.revision + 1);
        assert_eq!(after.content_hash, new_hash);
        assert!(!after.dirty);
    }

    // --------------------------------------------------------------------------
    // own-write suppression
    // --------------------------------------------------------------------------

    #[test]
    fn test_own_write_lifecycle() {
        let (tmp, mut store) = setup();
        write_file(tmp.path(), "foo.txt", b"hi");
        store.open(Path::new("foo.txt")).unwrap();

        assert!(!store.is_own_write(Path::new("foo.txt")));
        store.begin_own_write(Path::new("foo.txt"));
        assert!(store.is_own_write(Path::new("foo.txt")));
        store.end_own_write(Path::new("foo.txt"));
        assert!(!store.is_own_write(Path::new("foo.txt")));
    }

    // --------------------------------------------------------------------------
    // check_conflict
    // --------------------------------------------------------------------------

    #[test]
    fn test_check_conflict_untracked_is_clean() {
        let (_tmp, store) = setup();
        assert_eq!(
            store.check_conflict(Path::new("nope.txt")).unwrap(),
            ConflictStatus::Clean
        );
    }

    #[test]
    fn test_check_conflict_clean_when_unchanged() {
        let (tmp, mut store) = setup();
        write_file(tmp.path(), "foo.txt", b"hi");
        store.open(Path::new("foo.txt")).unwrap();

        assert_eq!(
            store.check_conflict(Path::new("foo.txt")).unwrap(),
            ConflictStatus::Clean
        );
    }

    #[test]
    fn test_check_conflict_changed_on_disk() {
        let (tmp, mut store) = setup();
        write_file(tmp.path(), "foo.txt", b"v1");
        store.open(Path::new("foo.txt")).unwrap();

        // 磁盘变更，但缓冲区干净
        write_file(tmp.path(), "foo.txt", b"v2");
        assert_eq!(
            store.check_conflict(Path::new("foo.txt")).unwrap(),
            ConflictStatus::ChangedOnDisk
        );
    }

    #[test]
    fn test_check_conflict_conflict_when_dirty_and_changed() {
        let (tmp, mut store) = setup();
        write_file(tmp.path(), "foo.txt", b"v1");
        store.open(Path::new("foo.txt")).unwrap();

        // 缓冲区有未保存变更 + 磁盘也变更 -> 冲突
        store.mark_dirty(Path::new("foo.txt"));
        write_file(tmp.path(), "foo.txt", b"v2");
        assert_eq!(
            store.check_conflict(Path::new("foo.txt")).unwrap(),
            ConflictStatus::Conflict
        );
    }

    #[test]
    fn test_check_conflict_own_write_suppressed() {
        let (tmp, mut store) = setup();
        write_file(tmp.path(), "foo.txt", b"v1");
        store.open(Path::new("foo.txt")).unwrap();
        store.mark_dirty(Path::new("foo.txt"));

        // 我们正在写入 -> 即使磁盘变更也视为 Clean（抑制）
        write_file(tmp.path(), "foo.txt", b"v2");
        store.begin_own_write(Path::new("foo.txt"));
        assert_eq!(
            store.check_conflict(Path::new("foo.txt")).unwrap(),
            ConflictStatus::Clean
        );

        // 结束自写后 -> 恢复为冲突检测
        store.end_own_write(Path::new("foo.txt"));
        assert_eq!(
            store.check_conflict(Path::new("foo.txt")).unwrap(),
            ConflictStatus::Conflict
        );
    }

    #[test]
    fn test_check_conflict_deleted_file_errors() {
        let (tmp, mut store) = setup();
        write_file(tmp.path(), "foo.txt", b"hi");
        store.open(Path::new("foo.txt")).unwrap();

        std::fs::remove_file(tmp.path().join("foo.txt")).unwrap();
        let err = store.check_conflict(Path::new("foo.txt")).unwrap_err();
        assert!(matches!(err, ProductError::Other(_)));
    }

    // --------------------------------------------------------------------------
    // 辅助函数
    // --------------------------------------------------------------------------

    #[test]
    fn test_detect_binary_helper() {
        assert!(!detect_binary(b"plain text"));
        assert!(detect_binary(b"abc\x00def"));
        assert!(!detect_binary(b""));
        // null 在 8KB 之后 -> 不检测到
        let mut v = vec![b'a'; BINARY_CHECK_LEN + 4];
        v[BINARY_CHECK_LEN + 2] = 0u8;
        assert!(!detect_binary(&v));
        // null 在 8KB 之内 -> 检测到
        let mut v2 = vec![b'a'; BINARY_CHECK_LEN];
        v2[BINARY_CHECK_LEN - 1] = 0u8;
        assert!(detect_binary(&v2));
    }

    #[test]
    fn test_detect_bom_helper() {
        assert_eq!(detect_bom(&[0xEF, 0xBB, 0xBF, b'x']), Some(BomKind::Utf8));
        assert_eq!(detect_bom(&[0xFF, 0xFE, b'x']), Some(BomKind::Utf16Le));
        assert_eq!(detect_bom(&[0xFE, 0xFF, b'x']), Some(BomKind::Utf16Be));
        assert_eq!(detect_bom(b"no bom"), None);
        assert_eq!(detect_bom(b""), None);
        // 仅前两字节匹配 UTF-8 BOM 前缀不算
        assert_eq!(detect_bom(&[0xEF, 0xBB, b'x']), None);
    }

    #[test]
    fn test_hash_file_matches_blake3() {
        let (tmp, _store) = setup();
        let content = b"hash me please";
        let path = write_file(tmp.path(), "h.txt", content);

        let h = hash_file(&path).unwrap();
        assert_eq!(h, blake3::hash(content).to_hex().to_string());
    }
}
