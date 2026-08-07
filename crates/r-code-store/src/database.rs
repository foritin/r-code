//! Database 结构体：rusqlite + r2d2 连接池。
//!
//! ✅ 已决策：使用 rusqlite + r2d2 + r2d2_sqlite 连接池。
//! 桌面应用低并发，WAL 多读 + 写串行化由 SQLite 保证。
//! [doc-06 §2.2, §8]

use std::path::Path;

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use r_code_core::error::ProductError;

use crate::migrations;

/// SQLite 数据库连接池。
pub struct Database {
    pool: Pool<SqliteConnectionManager>,
}

impl Database {
    /// 打开文件数据库（WAL 模式 + foreign_keys ON）。
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ProductError> {
        let path = path.as_ref();
        let db = Self::open_configured(path)?;
        db.run_migrations()?;
        Ok(db)
    }

    /// 打开已经通过产品启动迁移流程验证的文件数据库。
    ///
    /// 桌面与 MCP 启动入口必须先使用 `MigrationManager` 创建一致性备份、执行迁移并
    /// 验证完整性，再调用本方法建立连接池。它刻意不执行 migration，避免绕过失败恢复
    /// 流程；schema 不处于当前版本时会直接报错。
    pub fn open_after_migration(path: impl AsRef<Path>) -> Result<Self, ProductError> {
        let db = Self::open_configured(path.as_ref())?;
        db.ensure_current_schema()?;
        Ok(db)
    }

    fn open_configured(path: &Path) -> Result<Self, ProductError> {
        // `journal_mode` is persistent database state and may require a write lock. Configure it
        // once before r2d2 starts opening connections; doing this in `with_init` lets eager pool
        // connections race each other and produces transient `database is locked` failures.
        let bootstrap = rusqlite::Connection::open(path)
            .map_err(|e| ProductError::DatabaseError(format!("database open failed: {e}")))?;
        bootstrap
            .execute_batch(
                "PRAGMA busy_timeout=5000;\n\
                 PRAGMA journal_mode=WAL;",
            )
            .map_err(|e| ProductError::DatabaseError(format!("WAL setup failed: {e}")))?;
        drop(bootstrap);

        let manager = SqliteConnectionManager::file(path).with_init(|conn| {
            conn.execute_batch(
                "PRAGMA foreign_keys=ON;\n\
                 PRAGMA synchronous=NORMAL;\n\
                 PRAGMA busy_timeout=5000;",
            )
        });
        let pool = Pool::builder()
            .max_size(8)
            .min_idle(Some(1))
            .build(manager)
            .map_err(|e| ProductError::DatabaseError(format!("pool build failed: {e}")))?;

        let db = Self { pool };
        Ok(db)
    }

    /// 打开内存数据库（用于测试）。
    pub fn open_in_memory() -> Result<Self, ProductError> {
        let manager = SqliteConnectionManager::memory()
            .with_init(|conn| conn.execute_batch("PRAGMA foreign_keys=ON;"));
        let pool = Pool::builder().max_size(1).build(manager).map_err(|e| {
            ProductError::DatabaseError(format!("in-memory pool build failed: {e}"))
        })?;

        let db = Self { pool };
        db.run_migrations()?;
        Ok(db)
    }

    /// 执行 migration。
    fn run_migrations(&self) -> Result<(), ProductError> {
        let conn = self
            .pool
            .get()
            .map_err(|e| ProductError::DatabaseError(format!("pool get failed: {e}")))?;
        migrations::run_migrations(&conn)
    }

    fn ensure_current_schema(&self) -> Result<(), ProductError> {
        let conn = self
            .pool
            .get()
            .map_err(|e| ProductError::DatabaseError(format!("pool get failed: {e}")))?;
        let version: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                [],
                |row| row.get(0),
            )
            .map_err(|e| {
                ProductError::MigrationError(format!(
                    "read schema version after guarded migration failed: {e}"
                ))
            })?;
        if version == i64::from(migrations::LATEST_SCHEMA_VERSION) {
            Ok(())
        } else {
            Err(ProductError::MigrationError(format!(
                "database schema version {version} is not ready; expected {}",
                migrations::LATEST_SCHEMA_VERSION
            )))
        }
    }

    /// 获取连接池中的连接（用于 repository 操作）。
    pub fn conn(&self) -> Result<r2d2::PooledConnection<SqliteConnectionManager>, ProductError> {
        self.pool
            .get()
            .map_err(|e| ProductError::DatabaseError(format!("pool get failed: {e}")))
    }

    /// 获取连接池引用（用于高级场景）。
    pub fn pool(&self) -> &Pool<SqliteConnectionManager> {
        &self.pool
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_in_memory_creates_tables() {
        let db = Database::open_in_memory().unwrap();
        let conn = db.conn().unwrap();
        // 验证核心表存在
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();
        assert!(tables.contains(&"tasks".to_string()));
        assert!(tables.contains(&"agent_runs".to_string()));
        assert!(tables.contains(&"tool_calls".to_string()));
        assert!(tables.contains(&"file_changes".to_string()));
        assert!(tables.contains(&"file_baselines".to_string()));
        assert!(tables.contains(&"blobs".to_string()));
        assert!(tables.contains(&"permission_requests".to_string()));
        assert!(tables.contains(&"workspaces".to_string()));
        assert!(tables.contains(&"task_events".to_string()));
    }

    #[test]
    fn open_file_configures_every_pooled_connection() {
        let dir = tempfile::tempdir().unwrap();
        let db = Database::open(dir.path().join("r-code.db")).unwrap();

        // Hold all connections at once so the pool must grow from its single idle connection to
        // the configured maximum. Every new connection must receive the connection-local PRAGMAs
        // without trying to reconfigure the persistent journal mode.
        let connections: Vec<_> = (0..8).map(|_| db.conn().unwrap()).collect();
        for conn in connections {
            let journal_mode: String = conn
                .query_row("PRAGMA journal_mode", [], |row| row.get(0))
                .unwrap();
            let foreign_keys: i64 = conn
                .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
                .unwrap();
            let synchronous: i64 = conn
                .query_row("PRAGMA synchronous", [], |row| row.get(0))
                .unwrap();
            let busy_timeout: i64 = conn
                .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
                .unwrap();

            assert_eq!(journal_mode, "wal");
            assert_eq!(foreign_keys, 1);
            assert_eq!(synchronous, 1);
            assert_eq!(busy_timeout, 5_000);
        }
    }

    #[test]
    fn open_after_migration_rejects_unprepared_database() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("r-code.db");
        let _ = rusqlite::Connection::open(&db_path).unwrap();

        let error = match Database::open_after_migration(&db_path) {
            Ok(_) => panic!("unprepared database must be rejected"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("schema version"));
    }
}
