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
        let manager = SqliteConnectionManager::file(path).with_init(|conn| {
            conn.execute_batch(
                "PRAGMA journal_mode=WAL;\n\
                 PRAGMA foreign_keys=ON;\n\
                 PRAGMA synchronous=NORMAL;\n\
                 PRAGMA busy_timeout=5000;",
            )
        });
        let pool = Pool::builder()
            .max_size(8)
            .build(manager)
            .map_err(|e| ProductError::DatabaseError(format!("pool build failed: {e}")))?;

        let db = Self { pool };
        db.run_migrations()?;
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
}
