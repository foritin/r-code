//! 备份管理 -- 创建与恢复数据库备份。
//!
//! 定期备份到 `backup_dir`（通常位于 userData 目录），含校验和验证与恢复机制。
//! 备份采用文件级拷贝：以 `{db_stem}-{timestamp}.db` 命名。
//!
//! 注意：备份前应确保数据库连接已关闭或已 checkpoint（WAL 模式下，
//! 最后一个连接关闭时 SQLite 自动 checkpoint，主 .db 文件即为一致状态）。
//!
//! [doc-06 §4.2]

use std::path::{Path, PathBuf};

use r_code_core::error::ProductError;

/// 备份管理器 -- 创建和恢复数据库备份。
pub struct BackupManager {
    backup_dir: PathBuf,
}

impl BackupManager {
    /// 创建备份管理器，`backup_dir` 为备份存放目录。
    pub fn new(backup_dir: PathBuf) -> Self {
        Self { backup_dir }
    }

    /// 备份数据库文件。
    ///
    /// 将 SQLite 文件拷贝到 `backup_dir`，文件名带时间戳。
    /// 返回备份文件路径。
    pub fn backup(&self, db_path: &Path) -> Result<PathBuf, ProductError> {
        std::fs::create_dir_all(&self.backup_dir)?;

        let stem = db_path.file_stem().and_then(|s| s.to_str()).unwrap_or("db");
        let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
        let backup_name = format!("{stem}-{timestamp}.db");
        let backup_path = self.backup_dir.join(backup_name);

        std::fs::copy(db_path, &backup_path)
            .map_err(|e| ProductError::DatabaseError(format!("backup copy failed: {e}")))?;
        Ok(backup_path)
    }

    /// 从备份恢复数据库。
    ///
    /// 用备份文件覆盖当前数据库。恢复前清除 WAL/SHM 边车文件以避免陈旧状态。
    pub fn restore(&self, backup_path: &Path, db_path: &Path) -> Result<(), ProductError> {
        if !backup_path.exists() {
            return Err(ProductError::DatabaseError(format!(
                "backup not found: {}",
                backup_path.display()
            )));
        }
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // 清除 WAL/SHM 边车文件，避免恢复后 SQLite 读取陈旧的 WAL
        for sidecar in ["-wal", "-shm"] {
            let sidecar_path = format!("{}{sidecar}", db_path.display());
            let _ = std::fs::remove_file(&sidecar_path);
        }

        std::fs::copy(backup_path, db_path)
            .map_err(|e| ProductError::DatabaseError(format!("restore copy failed: {e}")))?;
        Ok(())
    }

    /// 列出可用备份（按文件名升序，等价于时间戳顺序）。
    pub fn list_backups(&self) -> Result<Vec<PathBuf>, ProductError> {
        if !self.backup_dir.exists() {
            return Ok(Vec::new());
        }
        let mut backups: Vec<PathBuf> = std::fs::read_dir(&self.backup_dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("db"))
            .collect();
        backups.sort();
        Ok(backups)
    }

    /// 校验备份文件是否为合法 SQLite 数据库。
    ///
    /// 文件不存在或无法打开 / 校验失败返回 `Ok(false)`。
    pub fn verify(&self, backup_path: &Path) -> Result<bool, ProductError> {
        if !backup_path.exists() {
            return Ok(false);
        }
        match rusqlite::Connection::open(backup_path) {
            Ok(conn) => {
                let ok: bool = conn
                    .query_row("PRAGMA integrity_check;", [], |row| {
                        let v: String = row.get(0)?;
                        Ok(v == "ok")
                    })
                    .unwrap_or(false);
                Ok(ok)
            }
            Err(_) => Ok(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 创建一个含单行数据的 SQLite 文件。
    fn make_db(path: &Path) {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch("CREATE TABLE t (v INTEGER); INSERT INTO t VALUES (1);")
            .unwrap();
        // 连接 drop 时写入落盘
        drop(conn);
    }

    fn count_rows(path: &Path) -> i64 {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn backup_creates_file_in_backup_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("test.db");
        make_db(&db_path);

        let backup_dir = tmp.path().join("backups");
        let mgr = BackupManager::new(backup_dir.clone());
        let backup_path = mgr.backup(&db_path).unwrap();

        assert!(backup_path.exists());
        assert!(backup_path.starts_with(&backup_dir));
        assert_eq!(backup_path.extension().and_then(|s| s.to_str()), Some("db"));
    }

    #[test]
    fn backup_creates_backup_dir_if_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("test.db");
        make_db(&db_path);

        let backup_dir = tmp.path().join("nested").join("backups");
        assert!(!backup_dir.exists());

        let mgr = BackupManager::new(backup_dir.clone());
        let backup_path = mgr.backup(&db_path).unwrap();

        assert!(backup_dir.exists());
        assert!(backup_path.exists());
    }

    #[test]
    fn verify_returns_true_for_valid_db() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("test.db");
        make_db(&db_path);

        let mgr = BackupManager::new(tmp.path().join("backups"));
        let backup_path = mgr.backup(&db_path).unwrap();
        assert!(mgr.verify(&backup_path).unwrap());
    }

    #[test]
    fn verify_returns_false_for_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = BackupManager::new(tmp.path().join("backups"));
        assert!(!mgr
            .verify(Path::new("/nonexistent/path/missing.db"))
            .unwrap());
    }

    #[test]
    fn verify_returns_false_for_non_db_file() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = tmp.path().join("fake.db");
        std::fs::write(&fake, b"this is not a database").unwrap();

        let mgr = BackupManager::new(tmp.path().join("backups"));
        assert!(!mgr.verify(&fake).unwrap());
    }

    #[test]
    fn restore_overwrites_database() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("test.db");
        make_db(&db_path);

        let mgr = BackupManager::new(tmp.path().join("backups"));
        let backup_path = mgr.backup(&db_path).unwrap();

        // 修改数据库：新增两行
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute("INSERT INTO t VALUES (2);", []).unwrap();
            conn.execute("INSERT INTO t VALUES (3);", []).unwrap();
        }
        assert_eq!(count_rows(&db_path), 3);

        // 恢复
        mgr.restore(&backup_path, &db_path).unwrap();

        // 回到 1 行
        assert_eq!(count_rows(&db_path), 1);
    }

    #[test]
    fn restore_fails_for_missing_backup() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = BackupManager::new(tmp.path().join("backups"));
        let db_path = tmp.path().join("test.db");
        let result = mgr.restore(Path::new("/nonexistent/backup.db"), &db_path);
        assert!(result.is_err());
    }

    #[test]
    fn restore_creates_parent_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("test.db");
        make_db(&db_path);

        let mgr = BackupManager::new(tmp.path().join("backups"));
        let backup_path = mgr.backup(&db_path).unwrap();

        let target = tmp.path().join("subdir").join("restored.db");
        assert!(!target.parent().unwrap().exists());

        mgr.restore(&backup_path, &target).unwrap();
        assert!(target.exists());
    }

    #[test]
    fn list_backups_returns_sorted_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let backup_dir = tmp.path().join("backups");
        let mgr = BackupManager::new(backup_dir.clone());

        // 用不同 stem 创建两份备份（避免同秒时间戳碰撞）
        let db_a = tmp.path().join("a.db");
        let db_b = tmp.path().join("b.db");
        make_db(&db_a);
        make_db(&db_b);

        let bp_a = mgr.backup(&db_a).unwrap();
        let bp_b = mgr.backup(&db_b).unwrap();

        let backups = mgr.list_backups().unwrap();
        assert_eq!(backups.len(), 2);
        // "a-..." < "b-..." 字典序
        assert_eq!(backups[0], bp_a);
        assert_eq!(backups[1], bp_b);
    }

    #[test]
    fn list_backups_empty_for_missing_dir() {
        let mgr = BackupManager::new(PathBuf::from("/tmp/r-code-nonexistent-backup-dir-xyz-123"));
        let backups = mgr.list_backups().unwrap();
        assert!(backups.is_empty());
    }

    #[test]
    fn list_backups_ignores_non_db_files() {
        let tmp = tempfile::tempdir().unwrap();
        let backup_dir = tmp.path().join("backups");
        std::fs::create_dir_all(&backup_dir).unwrap();

        let db_path = tmp.path().join("test.db");
        make_db(&db_path);

        let mgr = BackupManager::new(backup_dir.clone());
        mgr.backup(&db_path).unwrap();
        // 放入非 .db 文件
        std::fs::write(backup_dir.join("notes.txt"), b"ignore me").unwrap();
        std::fs::write(backup_dir.join("readme.md"), b"# readme").unwrap();

        let backups = mgr.list_backups().unwrap();
        assert_eq!(backups.len(), 1);
        assert!(backups[0].extension().unwrap() == "db");
    }
}
