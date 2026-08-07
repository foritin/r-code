//! 备份管理 -- 创建与恢复数据库备份。
//!
//! 定期备份到 `backup_dir`（通常位于 userData 目录），含校验和验证与恢复机制。
//! 备份通过 SQLite `VACUUM INTO` 创建，而不是直接复制主数据库文件；因此即使源库
//! 仍处于 WAL 模式，备份也会包含尚未 checkpoint 的数据。
//!
//! [doc-06 §4.2]

use std::path::{Path, PathBuf};

use r_code_core::error::ProductError;
use rusqlite::{Connection, OpenFlags};
use uuid::Uuid;

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
    /// 使用 SQLite `VACUUM INTO` 创建一个一致性快照，文件名带时间戳。
    /// 该命令会读取 WAL 中尚未 checkpoint 的页，因此无需关闭正在使用的源数据库。
    /// 返回备份文件路径。
    pub fn backup(&self, db_path: &Path) -> Result<PathBuf, ProductError> {
        self.backup_with_label(db_path, "backup")
    }

    /// 创建一次升级前备份。
    ///
    /// 迁移调用方使用单独的标签，方便用户和支持人员识别可用于恢复的升级前快照。
    pub fn backup_pre_migration(&self, db_path: &Path) -> Result<PathBuf, ProductError> {
        self.backup_with_label(db_path, "pre-migration")
    }

    fn backup_with_label(&self, db_path: &Path, label: &str) -> Result<PathBuf, ProductError> {
        if !db_path.is_file() {
            return Err(ProductError::DatabaseError(format!(
                "backup source is not a database file: {}",
                db_path.display()
            )));
        }
        std::fs::create_dir_all(&self.backup_dir)?;

        let stem = db_path.file_stem().and_then(|s| s.to_str()).unwrap_or("db");
        let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S%.3fZ");
        // UUID avoids silently overwriting an earlier snapshot when two processes start in the
        // same clock tick (for example a desktop launch racing an MCP launch).
        let backup_name = format!("{stem}-{label}-{timestamp}-{}.db", Uuid::new_v4());
        let backup_path = self.backup_dir.join(backup_name);
        let partial_path = backup_path.with_extension("db.partial");

        let result = (|| {
            let source = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|e| {
                    ProductError::DatabaseError(format!("backup source open failed: {e}"))
                })?;
            source
                .execute_batch("PRAGMA busy_timeout=5000;")
                .map_err(|e| {
                    ProductError::DatabaseError(format!("backup source setup failed: {e}"))
                })?;

            let partial_path_text = partial_path.to_string_lossy();
            source
                .execute("VACUUM INTO ?1", [&partial_path_text])
                .map_err(|e| ProductError::DatabaseError(format!("backup snapshot failed: {e}")))?;
            drop(source);

            if !self.verify(&partial_path)? {
                return Err(ProductError::DatabaseError(
                    "backup integrity check failed".to_string(),
                ));
            }
            std::fs::rename(&partial_path, &backup_path)
                .map_err(|e| ProductError::DatabaseError(format!("finalize backup failed: {e}")))?;
            Ok(backup_path.clone())
        })();

        if result.is_err() {
            let _ = std::fs::remove_file(&partial_path);
        }
        result
    }

    /// 从备份恢复数据库。
    ///
    /// 用备份文件原子覆盖当前数据库，并在替换后清除旧 WAL/SHM 边车文件。
    ///
    /// 调用方必须先关闭目标数据库的所有连接；恢复后本方法会重新打开文件进行
    /// `integrity_check`，任何失败都会显式返回给调用方。
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

        if !self.verify(backup_path)? {
            return Err(ProductError::DatabaseError(format!(
                "backup integrity check failed: {}",
                backup_path.display()
            )));
        }

        // Stage the complete backup before touching the live database. This prevents a failed
        // copy from replacing a recoverable database with a truncated file.
        let restore_staging = db_path.with_extension(format!("db.restore-{}", Uuid::new_v4()));
        let restore_result = (|| {
            std::fs::copy(backup_path, &restore_staging)
                .map_err(|e| ProductError::DatabaseError(format!("restore copy failed: {e}")))?;
            std::fs::OpenOptions::new()
                .write(true)
                .open(&restore_staging)
                .and_then(|file| file.sync_all())
                .map_err(|e| {
                    ProductError::DatabaseError(format!("flush staged restore failed: {e}"))
                })?;
            if !self.verify(&restore_staging)? {
                return Err(ProductError::DatabaseError(
                    "staged restore integrity check failed".to_string(),
                ));
            }

            // `rename` replaces an existing file atomically when source and destination are in
            // the same directory (POSIX rename / Windows MoveFileEx with replace-existing).
            // Never delete the live primary database first: a crash in that gap would leave the
            // next startup with no database at all. Callers close all SQLite handles before
            // restore, so an open-handle failure leaves the previous database intact.
            std::fs::rename(&restore_staging, db_path).map_err(|e| {
                ProductError::DatabaseError(format!("activate restored database failed: {e}"))
            })?;

            // Clear WAL/SHM only after the primary database is safely replaced. A crash before
            // this cleanup leaves stale sidecars beside the new database; SQLite rejects those
            // by their database/WAL identity and the next startup can remove them. Cleaning them
            // first would instead risk discarding uncheckpointed data if activation failed.
            for sidecar in ["-wal", "-shm"] {
                let sidecar_path = PathBuf::from(format!("{}{sidecar}", db_path.display()));
                match std::fs::remove_file(&sidecar_path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(ProductError::DatabaseError(format!(
                            "remove stale SQLite sidecar {} failed: {error}",
                            sidecar_path.display()
                        )));
                    }
                }
            }

            if !self.verify(db_path)? {
                return Err(ProductError::DatabaseError(
                    "restored database integrity check failed".to_string(),
                ));
            }
            Ok(())
        })();

        if restore_result.is_err() {
            let _ = std::fs::remove_file(&restore_staging);
        }
        restore_result
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
        // Read-only open prevents a raced-away backup path from being recreated as an empty
        // SQLite file and incorrectly passing integrity_check.
        match rusqlite::Connection::open_with_flags(backup_path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
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
    fn backup_includes_uncheckpointed_wal_content() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("wal-source.db");
        let source = Connection::open(&db_path).unwrap();
        source
            .execute_batch(
                "PRAGMA journal_mode=WAL;\
                 PRAGMA wal_autocheckpoint=0;\
                 CREATE TABLE t (v INTEGER);\
                 INSERT INTO t VALUES (1);",
            )
            .unwrap();
        assert!(PathBuf::from(format!("{}-wal", db_path.display())).exists());

        let mgr = BackupManager::new(tmp.path().join("backups"));
        let backup_path = mgr.backup(&db_path).unwrap();

        // Keep the source connection open so the write remains in the WAL. A raw copy of only
        // the main .db file would not be a valid snapshot here; VACUUM INTO is.
        assert_eq!(count_rows(&backup_path), 1);
        drop(source);
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
