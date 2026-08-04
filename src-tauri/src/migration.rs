//! Migration strategy -- 版本升级迁移 [doc-19 §6]。
//!
//! Flow: parallel read -> shadow run -> optional migration -> controlled switch.
//! Never put existing user data directly into new version writes.
//! Old database retains full backup and version tag.
//! Irreversible schema changes provide export and dry-run first.
//!
//! 本模块复用 `r_code_store::migrations::run_migrations` 执行实际 schema 迁移，
//! 在其之上提供：版本检查、dry-run、备份、完整性校验、JSON 导出。
//!
//! [doc-19 §6]

use std::path::PathBuf;

use r_code_core::error::ProductError;
use rusqlite::Connection;

/// 目标 schema 版本（由实际 store migration 作为唯一来源）。
const TARGET_VERSION: u32 = r_code_store::migrations::LATEST_SCHEMA_VERSION;

/// Migration strategy for upgrading from previous versions.
/// [doc-19 §6]
///
/// Flow: parallel read -> shadow run -> optional migration -> controlled switch.
/// Never put existing user data directly into new version writes.
/// Old database retains full backup and version tag.
/// Irreversible schema changes provide export and dry-run first.
pub struct MigrationManager {
    db_path: PathBuf,
}

/// Migration step.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MigrationStep {
    /// 起始版本
    pub from_version: u32,
    /// 目标版本
    pub to_version: u32,
    /// 描述
    pub description: String,
    /// 是否可逆
    pub is_reversible: bool,
    /// 是否支持 dry-run
    pub dry_run_available: bool,
}

/// Migration result.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MigrationResult {
    /// 已应用的步骤
    pub steps_applied: Vec<MigrationStep>,
    /// 备份文件路径（如有）
    pub backup_path: Option<String>,
    /// 是否成功
    pub success: bool,
    /// 警告信息
    pub warnings: Vec<String>,
}

impl MigrationManager {
    pub fn new(db_path: PathBuf) -> Self {
        Self { db_path }
    }

    /// 打开一个直接连接（非连接池），设置 busy_timeout 以容忍并发读。
    fn connect(&self) -> Result<Connection, ProductError> {
        let conn = Connection::open(&self.db_path)
            .map_err(|e| ProductError::DatabaseError(format!("migration open failed: {e}")))?;
        conn.execute_batch("PRAGMA busy_timeout=5000;")
            .map_err(|e| ProductError::DatabaseError(format!("set busy_timeout: {e}")))?;
        Ok(conn)
    }

    /// Check if migration is needed.
    pub fn needs_migration(&self) -> Result<bool, ProductError> {
        Ok(self.current_version()? < TARGET_VERSION)
    }

    /// Get the current schema version.
    ///
    /// Reads from the `schema_version` table. Returns 0 if the table
    /// doesn't exist (fresh database).
    pub fn current_version(&self) -> Result<u32, ProductError> {
        let conn = self.connect()?;
        // Check if schema_version table exists
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| ProductError::DatabaseError(format!("check schema_version: {e}")))?;
        if exists == 0 {
            return Ok(0);
        }
        let version: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version",
                [],
                |row| row.get(0),
            )
            .map_err(|e| ProductError::DatabaseError(format!("read schema_version: {e}")))?;
        Ok(version as u32)
    }

    /// List pending migration steps.
    pub fn pending_steps(&self) -> Result<Vec<MigrationStep>, ProductError> {
        let current = self.current_version()?;
        Ok(known_steps()
            .into_iter()
            .filter(|s| s.to_version > current)
            .collect())
    }

    /// Dry-run migration (no changes applied).
    ///
    /// Returns the steps that would be applied, without modifying the database.
    pub async fn dry_run(&self) -> Result<MigrationResult, ProductError> {
        let steps = self.pending_steps()?;
        let warnings = steps
            .iter()
            .map(|s| {
                format!(
                    "would apply: v{} -> v{}: {}",
                    s.from_version, s.to_version, s.description
                )
            })
            .collect();
        Ok(MigrationResult {
            steps_applied: steps,
            backup_path: None,
            success: true,
            warnings,
        })
    }

    /// Execute migration with backup.
    /// 1. Backup current database
    /// 2. Apply migrations in order
    /// 3. Verify integrity
    pub async fn migrate(&self) -> Result<MigrationResult, ProductError> {
        let pending = self.pending_steps()?;
        if pending.is_empty() {
            return Ok(MigrationResult {
                steps_applied: vec![],
                backup_path: None,
                success: true,
                warnings: vec!["no migrations needed".to_string()],
            });
        }

        // 1. Backup current database
        let backup_path = self.backup()?;

        // 2. Apply migrations in order (reuses r-code-store migration runner)
        let conn = self.connect()?;
        r_code_store::migrations::run_migrations(&conn)?;

        // 3. Verify integrity
        let integrity: String = conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(|e| ProductError::DatabaseError(format!("integrity check: {e}")))?;
        if integrity != "ok" {
            return Ok(MigrationResult {
                steps_applied: pending,
                backup_path: backup_path.map(|p| p.display().to_string()),
                success: false,
                warnings: vec![format!("integrity check failed: {integrity}")],
            });
        }

        Ok(MigrationResult {
            steps_applied: pending,
            backup_path: backup_path.map(|p| p.display().to_string()),
            success: true,
            warnings: vec![],
        })
    }

    /// Export database contents to JSON (for user backup before migration).
    ///
    /// Reads all user tables and serializes each row as a JSON object.
    /// Blob values are rendered as `<blob N bytes>` placeholders.
    pub async fn export_json(&self) -> Result<String, ProductError> {
        let conn = self.connect()?;

        // Get all user tables (exclude sqlite_internal tables)
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master \
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            )
            .map_err(|e| ProductError::DatabaseError(format!("list tables: {e}")))?;
        let table_names: Vec<String> = stmt
            .query_map([], |row| row.get(0))
            .map_err(|e| ProductError::DatabaseError(format!("query tables: {e}")))?
            .filter_map(|r| r.ok())
            .collect();
        drop(stmt);

        let mut db_json = serde_json::Map::new();
        for table in table_names {
            let rows = export_table(&conn, &table)?;
            db_json.insert(table, serde_json::Value::Array(rows));
        }

        Ok(serde_json::to_string_pretty(&serde_json::Value::Object(
            db_json,
        ))?)
    }

    /// Backup current database to a timestamped file in the same directory.
    /// Returns None if the database file doesn't exist yet.
    fn backup(&self) -> Result<Option<PathBuf>, ProductError> {
        if !self.db_path.exists() {
            return Ok(None);
        }
        let parent = self
            .db_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let stem = self
            .db_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("db");
        let timestamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
        let backup_name = format!("{stem}-pre-migration-{timestamp}.db");
        let backup_path = parent.join(backup_name);
        std::fs::copy(&self.db_path, &backup_path)?;
        Ok(Some(backup_path))
    }
}

/// 已知的 migration 步骤（与 r-code-store::migrations 保持同步）。
fn known_steps() -> Vec<MigrationStep> {
    vec![
        MigrationStep {
            from_version: 0,
            to_version: 1,
            description: "Initial schema: tasks, agent_runs, tool_calls, file_changes, \
                          file_baselines, blobs, permission_requests, workspaces"
                .to_string(),
            is_reversible: false,
            dry_run_available: true,
        },
        MigrationStep {
            from_version: 1,
            to_version: 2,
            description: "Optional workspace scope for pure-chat tasks".to_string(),
            is_reversible: false,
            dry_run_available: true,
        },
        MigrationStep {
            from_version: 2,
            to_version: 3,
            description: "Session branches and durable queued messages".to_string(),
            is_reversible: false,
            dry_run_available: true,
        },
        MigrationStep {
            from_version: 3,
            to_version: 4,
            description: "Agent run tree for delegated read-only subagents".to_string(),
            is_reversible: false,
            dry_run_available: true,
        },
        MigrationStep {
            from_version: 4,
            to_version: 5,
            description: "Persistent safe summaries for completed subagents".to_string(),
            is_reversible: false,
            dry_run_available: true,
        },
        MigrationStep {
            from_version: 5,
            to_version: 6,
            description: "Session-bound model service selection".to_string(),
            is_reversible: false,
            dry_run_available: true,
        },
        MigrationStep {
            from_version: 6,
            to_version: 7,
            description: "Project-level Agent access modes".to_string(),
            is_reversible: false,
            dry_run_available: true,
        },
        MigrationStep {
            from_version: 7,
            to_version: 8,
            description: "Session-bound concrete model override".to_string(),
            is_reversible: false,
            dry_run_available: true,
        },
        MigrationStep {
            from_version: 8,
            to_version: 9,
            description: "Close legacy verification placeholder runs".to_string(),
            is_reversible: false,
            dry_run_available: true,
        },
        MigrationStep {
            from_version: 9,
            to_version: 10,
            description: "Persistent notification center with read state".to_string(),
            is_reversible: false,
            dry_run_available: true,
        },
        MigrationStep {
            from_version: 10,
            to_version: 11,
            description: "External agent runtime kind and resumable session identifiers"
                .to_string(),
            is_reversible: false,
            dry_run_available: true,
        },
        MigrationStep {
            from_version: 11,
            to_version: 12,
            description: "Task agent engine and observable delegation routing metadata".to_string(),
            is_reversible: false,
            dry_run_available: true,
        },
        MigrationStep {
            from_version: 12,
            to_version: 13,
            description: "Task-scoped model inference configuration".to_string(),
            is_reversible: false,
            dry_run_available: true,
        },
        MigrationStep {
            from_version: 13,
            to_version: 14,
            description: "Stable workspace identity and local memory mode".to_string(),
            is_reversible: false,
            dry_run_available: true,
        },
        MigrationStep {
            from_version: 14,
            to_version: 15,
            description: "Repair missing notification schema in legacy databases".to_string(),
            is_reversible: false,
            dry_run_available: true,
        },
        MigrationStep {
            from_version: 15,
            to_version: 16,
            description: "Run workspace snapshots and idempotent review change materialization"
                .to_string(),
            is_reversible: false,
            dry_run_available: true,
        },
        MigrationStep {
            from_version: 16,
            to_version: 17,
            description: "Persistent application review sessions and decisions".to_string(),
            is_reversible: false,
            dry_run_available: true,
        },
        MigrationStep {
            from_version: 17,
            to_version: 18,
            description:
                "Scoped evolving memory, review jobs, approvals and frozen injection ledger"
                    .to_string(),
            is_reversible: false,
            dry_run_available: true,
        },
        MigrationStep {
            from_version: 18,
            to_version: 19,
            description: "Durable Plan/HITL state and feature-scoped review journals".to_string(),
            is_reversible: false,
            dry_run_available: true,
        },
        MigrationStep {
            from_version: 19,
            to_version: 20,
            description: "Durable approved-Plan implementation handoff".to_string(),
            is_reversible: false,
            dry_run_available: true,
        },
        MigrationStep {
            from_version: 20,
            to_version: 21,
            description: "Atomic enhanced-review scope and rejection claims".to_string(),
            is_reversible: false,
            dry_run_available: true,
        },
    ]
}

/// 导出单张表的所有行为 JSON 数组。
fn export_table(conn: &Connection, table: &str) -> Result<Vec<serde_json::Value>, ProductError> {
    let sql = format!("SELECT * FROM {table}");
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| ProductError::DatabaseError(format!("prepare {table}: {e}")))?;
    let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let mut rows = stmt
        .query([])
        .map_err(|e| ProductError::DatabaseError(format!("query {table}: {e}")))?;
    let mut result = Vec::new();
    while let Some(row) = rows
        .next()
        .map_err(|e| ProductError::DatabaseError(format!("read {table}: {e}")))?
    {
        let mut obj = serde_json::Map::new();
        for (i, col_name) in col_names.iter().enumerate() {
            let val: rusqlite::types::Value = row
                .get(i)
                .map_err(|e| ProductError::DatabaseError(format!("read col {i}: {e}")))?;
            obj.insert(col_name.clone(), sqlite_value_to_json(val));
        }
        result.push(serde_json::Value::Object(obj));
    }
    Ok(result)
}

/// 将 SQLite 值转换为 JSON 值。
fn sqlite_value_to_json(val: rusqlite::types::Value) -> serde_json::Value {
    match val {
        rusqlite::types::Value::Null => serde_json::Value::Null,
        rusqlite::types::Value::Integer(i) => serde_json::json!(i),
        rusqlite::types::Value::Real(f) => serde_json::json!(f),
        rusqlite::types::Value::Text(s) => serde_json::json!(s),
        rusqlite::types::Value::Blob(b) => {
            serde_json::json!(format!("<blob {} bytes>", b.len()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// 构造一个空 SQLite 文件（无 schema）。
    fn setup_fresh_db() -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let _ = Connection::open(&db_path).unwrap();
        (dir, db_path)
    }

    // ── current_version / needs_migration ─────────────────────────

    #[test]
    fn fresh_db_version_is_zero() {
        let (_dir, db_path) = setup_fresh_db();
        let mgr = MigrationManager::new(db_path);
        assert_eq!(mgr.current_version().unwrap(), 0);
    }

    #[test]
    fn fresh_db_needs_migration() {
        let (_dir, db_path) = setup_fresh_db();
        let mgr = MigrationManager::new(db_path);
        assert!(mgr.needs_migration().unwrap());
    }

    // ── pending_steps ─────────────────────────────────────────────

    #[test]
    fn fresh_db_has_pending_step() {
        let (_dir, db_path) = setup_fresh_db();
        let mgr = MigrationManager::new(db_path);
        let steps = mgr.pending_steps().unwrap();
        assert_eq!(steps.len(), TARGET_VERSION as usize);
        assert_eq!(steps[0].from_version, 0);
        assert_eq!(steps[0].to_version, 1);
        assert!(!steps[0].is_reversible);
        assert!(steps[0].dry_run_available);
        assert_eq!(steps[1].from_version, 1);
        assert_eq!(steps[1].to_version, 2);
        assert_eq!(steps[2].from_version, 2);
        assert_eq!(steps[2].to_version, 3);
        assert_eq!(steps[3].from_version, 3);
        assert_eq!(steps[3].to_version, 4);
        assert_eq!(steps[4].to_version, 5);
        assert_eq!(steps[6].to_version, 7);
        assert_eq!(steps[7].to_version, 8);
        assert_eq!(steps[8].to_version, 9);
        assert_eq!(steps.last().unwrap().to_version, TARGET_VERSION);
    }

    // ── dry_run ───────────────────────────────────────────────────

    #[tokio::test]
    async fn dry_run_does_not_modify() {
        let (_dir, db_path) = setup_fresh_db();
        let mgr = MigrationManager::new(db_path);
        let result = mgr.dry_run().await.unwrap();
        assert!(result.success);
        assert!(!result.steps_applied.is_empty());
        assert!(result.backup_path.is_none());
        // Version should still be 0
        assert_eq!(mgr.current_version().unwrap(), 0);
    }

    // ── migrate ───────────────────────────────────────────────────

    #[tokio::test]
    async fn migrate_applies_all_current_migrations() {
        let (_dir, db_path) = setup_fresh_db();
        let mgr = MigrationManager::new(db_path);

        // Before: version 0, needs migration
        assert_eq!(mgr.current_version().unwrap(), 0);
        assert!(mgr.needs_migration().unwrap());

        // Migrate
        let result = mgr.migrate().await.unwrap();
        assert!(result.success);
        assert!(!result.steps_applied.is_empty());

        // After: latest version, no migration needed
        assert_eq!(mgr.current_version().unwrap(), TARGET_VERSION);
        assert!(!mgr.needs_migration().unwrap());
    }

    #[tokio::test]
    async fn migrate_on_already_migrated_is_noop() {
        let (_dir, db_path) = setup_fresh_db();
        let mgr = MigrationManager::new(db_path);

        // First migration
        mgr.migrate().await.unwrap();

        // Second migration should be noop
        let result = mgr.migrate().await.unwrap();
        assert!(result.success);
        assert!(result.steps_applied.is_empty());
    }

    #[tokio::test]
    async fn pending_steps_migrated_db_empty() {
        let (_dir, db_path) = setup_fresh_db();
        let mgr = MigrationManager::new(db_path);
        mgr.migrate().await.unwrap();
        let steps = mgr.pending_steps().unwrap();
        assert!(steps.is_empty());
    }

    // ── export_json ───────────────────────────────────────────────

    #[tokio::test]
    async fn export_json_returns_valid_json() {
        let (_dir, db_path) = setup_fresh_db();
        let mgr = MigrationManager::new(db_path);
        mgr.migrate().await.unwrap();

        let json = mgr.export_json().await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed.is_object());
        // Should contain the tasks table (created by migration 001)
        assert!(parsed.get("tasks").is_some());
    }

    #[tokio::test]
    async fn export_json_empty_db_returns_empty_object() {
        let (_dir, db_path) = setup_fresh_db();
        let mgr = MigrationManager::new(db_path);

        let json = mgr.export_json().await.unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed.is_object());
        // Fresh db has no user tables
        assert!(parsed.as_object().unwrap().is_empty());
    }
}
