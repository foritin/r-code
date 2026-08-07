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

use std::path::{Path, PathBuf};

use r_code_core::error::ProductError;
use r_code_store::BackupManager;
use rusqlite::Connection;

/// 目标 schema 版本（由实际 store migration 作为唯一来源）。
const TARGET_VERSION: u32 = r_code_store::migrations::LATEST_SCHEMA_VERSION;
const MIGRATION_LOCK_TIMEOUT_MS: u32 = 30_000;

/// Cross-process guard for the full upgrade critical section.
///
/// Desktop and MCP can be launched independently against the same AppData directory. A separate
/// SQLite lock database gives us an OS-backed exclusive lock that is released automatically if a
/// process crashes, without keeping a stale marker file behind. The guard covers snapshot,
/// migration, verification, and any restore so a losing process can never restore an old snapshot
/// over a successful upgrade performed by the winner.
struct MigrationLock {
    _connection: Connection,
}

impl MigrationLock {
    fn acquire(db_path: &Path) -> Result<Self, ProductError> {
        Self::acquire_with_timeout(db_path, MIGRATION_LOCK_TIMEOUT_MS)
    }

    fn acquire_with_timeout(db_path: &Path, timeout_ms: u32) -> Result<Self, ProductError> {
        let lock_path = PathBuf::from(format!("{}.migration-lock", db_path.display()));
        let connection = Connection::open(&lock_path).map_err(|error| {
            ProductError::MigrationError(format!(
                "open migration coordination lock {} failed: {error}",
                lock_path.display()
            ))
        })?;
        connection
            .execute_batch(&format!("PRAGMA busy_timeout={timeout_ms}; BEGIN EXCLUSIVE;"))
            .map_err(|error| {
                ProductError::MigrationError(format!(
                    "another R-Code process is upgrading this database or the migration lock is unavailable: {error}"
                ))
            })?;
        Ok(Self {
            _connection: connection,
        })
    }
}

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
        conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")
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
        u32::try_from(version).map_err(|_| {
            ProductError::MigrationError(format!("invalid negative schema version: {version}"))
        })
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
    /// 4. Restore the pre-migration snapshot and abort startup if any step fails
    pub async fn migrate(&self) -> Result<MigrationResult, ProductError> {
        self.migrate_with_runner(r_code_store::migrations::run_migrations)
    }

    fn migrate_with_runner<F>(&self, runner: F) -> Result<MigrationResult, ProductError>
    where
        F: FnOnce(&Connection) -> Result<(), ProductError>,
    {
        if self.db_path.exists() && !self.db_path.is_file() {
            return Err(ProductError::MigrationError(format!(
                "database path is not a regular file: {}",
                self.db_path.display()
            )));
        }
        let _migration_lock = MigrationLock::acquire(&self.db_path)?;

        // Record this before `current_version` opens a fresh database file. New installations
        // have no user state yet, so they do not need a pre-migration backup.
        let database_existed = self.db_path.is_file();
        let current_version = self.current_version()?;
        if current_version > TARGET_VERSION {
            return Err(ProductError::MigrationError(format!(
                "database schema version {current_version} is newer than this application supports ({TARGET_VERSION}); refusing to open it"
            )));
        }
        self.verify_database_integrity("before migration")?;

        let pending = known_steps()
            .into_iter()
            .filter(|step| step.to_version > current_version)
            .collect::<Vec<_>>();
        if pending.is_empty() {
            return Ok(MigrationResult {
                steps_applied: vec![],
                backup_path: None,
                success: true,
                warnings: vec!["no migrations needed".to_string()],
            });
        }

        // 1. Back up existing user data using SQLite's WAL-safe snapshot API, unlike a plain
        // filesystem copy of the main .db file.
        let backup = if database_existed {
            let manager = BackupManager::new(self.backup_directory());
            let path = manager.backup_pre_migration(&self.db_path)?;
            if !manager.verify(&path)? {
                return Err(ProductError::MigrationError(format!(
                    "pre-migration backup integrity check failed: {}",
                    path.display()
                )));
            }
            Some((manager, path))
        } else {
            None
        };

        // 2. Apply migrations in order (reuses r-code-store migration runner). Drop the
        // connection before restoration: on Windows an open SQLite handle would prevent the
        // recovered snapshot from replacing the failed database file.
        let conn = self.connect()?;
        let migration_result =
            runner(&conn).and_then(|_| self.verify_connection_integrity(&conn, "after migration"));
        drop(conn);
        if let Err(error) = migration_result {
            return Err(self.restore_after_failure(error, backup.as_ref()));
        }

        let resulting_version = match self.current_version() {
            Ok(version) if version == TARGET_VERSION => version,
            Ok(version) => {
                return Err(self.restore_after_failure(
                    ProductError::MigrationError(format!(
                        "migration completed at schema version {version}, expected {TARGET_VERSION}"
                    )),
                    backup.as_ref(),
                ));
            }
            Err(error) => return Err(self.restore_after_failure(error, backup.as_ref())),
        };
        debug_assert_eq!(resulting_version, TARGET_VERSION);

        Ok(MigrationResult {
            steps_applied: pending,
            backup_path: backup.as_ref().map(|(_, path)| path.display().to_string()),
            success: true,
            warnings: vec![],
        })
    }

    fn backup_directory(&self) -> PathBuf {
        self.db_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    fn verify_database_integrity(&self, phase: &str) -> Result<(), ProductError> {
        let conn = self.connect()?;
        let result = self.verify_connection_integrity(&conn, phase);
        drop(conn);
        result
    }

    fn verify_connection_integrity(
        &self,
        conn: &Connection,
        phase: &str,
    ) -> Result<(), ProductError> {
        let integrity: String = conn
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .map_err(|e| ProductError::DatabaseError(format!("{phase} integrity check: {e}")))?;
        if integrity == "ok" {
            Ok(())
        } else {
            Err(ProductError::MigrationError(format!(
                "{phase} integrity check failed: {integrity}"
            )))
        }
    }

    fn restore_after_failure(
        &self,
        failure: ProductError,
        backup: Option<&(BackupManager, PathBuf)>,
    ) -> ProductError {
        let failure_message = failure.to_string();
        match backup {
            Some((manager, backup_path)) => match manager.restore(backup_path, &self.db_path) {
                Ok(()) => ProductError::MigrationError(format!(
                    "migration failed ({failure_message}); restored pre-migration backup {} and aborted startup",
                    backup_path.display()
                )),
                Err(restore_error) => ProductError::MigrationError(format!(
                    "migration failed ({failure_message}); automatic restore also failed ({restore_error}). \
                     The verified pre-migration backup is still available at {}",
                    backup_path.display()
                )),
            },
            None => ProductError::MigrationError(format!(
                "migration failed for a newly created database ({failure_message}); startup aborted"
            )),
        }
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
        MigrationStep {
            from_version: 21,
            to_version: 22,
            description: "Hierarchical Plan progress paths for executable feature items"
                .to_string(),
            is_reversible: false,
            dry_run_available: true,
        },
        MigrationStep {
            from_version: 22,
            to_version: 23,
            description: "Explicit Goal lifecycle state without reclassifying existing tasks"
                .to_string(),
            is_reversible: false,
            dry_run_available: true,
        },
        MigrationStep {
            from_version: 23,
            to_version: 24,
            description: "Durable user-defined execution order for queued messages".to_string(),
            is_reversible: false,
            dry_run_available: true,
        },
        MigrationStep {
            from_version: 24,
            to_version: 25,
            description: "Persisted approval clamp for delegated agent runs".to_string(),
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

    #[test]
    fn migration_lock_prevents_overlapping_upgrade_attempts() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("test.db");
        let first = MigrationLock::acquire_with_timeout(&db_path, 10).unwrap();

        let error = match MigrationLock::acquire_with_timeout(&db_path, 10) {
            Ok(_) => panic!("second migration lock acquisition must fail"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("another R-Code process"));

        drop(first);
        MigrationLock::acquire_with_timeout(&db_path, 10).unwrap();
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
        let backup_path = PathBuf::from(result.backup_path.unwrap());
        assert!(backup_path.exists());
        assert!(backup_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap()
            .contains("pre-migration"));
        assert!(
            BackupManager::new(backup_path.parent().unwrap().to_path_buf())
                .verify(&backup_path)
                .unwrap()
        );

        // After: latest version, no migration needed
        assert_eq!(mgr.current_version().unwrap(), TARGET_VERSION);
        assert!(!mgr.needs_migration().unwrap());
    }

    #[tokio::test]
    async fn failed_migration_restores_the_pre_migration_snapshot() {
        let (_dir, db_path) = setup_fresh_db();
        let mgr = MigrationManager::new(db_path.clone());
        mgr.migrate().await.unwrap();

        // Make the version ledger report a pending migration without changing the physical v25
        // schema. The test runner below then persists a marker and fails, which lets us prove
        // that the failed write is replaced with the snapshot rather than merely reported.
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "DELETE FROM schema_version WHERE version = ?",
            [TARGET_VERSION],
        )
        .unwrap();
        drop(conn);

        let error = mgr
            .migrate_with_runner(|conn| {
                conn.execute_batch("CREATE TABLE forced_migration_failure (id INTEGER);")
                    .map_err(|error| ProductError::MigrationError(error.to_string()))?;
                Err(ProductError::MigrationError(
                    "forced migration failure".to_string(),
                ))
            })
            .unwrap_err();
        assert!(error.to_string().contains("restored pre-migration backup"));

        let restored = Connection::open(&db_path).unwrap();
        let marker_exists: i64 = restored
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND name = 'forced_migration_failure'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let restored_version: i64 = restored
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(marker_exists, 0);
        assert_eq!(restored_version, i64::from(TARGET_VERSION - 1));
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
