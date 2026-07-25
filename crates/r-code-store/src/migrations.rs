//! 版本化 migration 系统。
//!
//! 使用 `schema_version` 表 + 编号迁移文件。
//! Migration 001 覆盖全部核心表。
//! [doc-06 §3, §5]

use r_code_core::error::ProductError;
use rusqlite::Connection;

/// 运行所有待执行的 migration。
pub fn run_migrations(conn: &Connection) -> Result<(), ProductError> {
    // 创建 schema_version 表（如果不存在）
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (\n\
         version INTEGER PRIMARY KEY,\n\
         applied_at TEXT NOT NULL DEFAULT (datetime('now'))\n\
         );",
    )
    .map_err(|e| ProductError::MigrationError(format!("create schema_version table: {e}")))?;

    // 获取当前版本
    let current: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version",
            [],
            |row| row.get(0),
        )
        .map_err(|e| ProductError::MigrationError(format!("read schema_version: {e}")))?;

    // 按序执行 migration
    let migrations: &[(i64, &str)] = &[(1, MIGRATION_001)];

    for (version, sql) in migrations {
        if *version > current {
            tracing::info!(version, "applying migration");
            conn.execute_batch(sql)
                .map_err(|e| ProductError::MigrationError(format!("migration {version}: {e}")))?;
            conn.execute(
                "INSERT INTO schema_version (version) VALUES (?1)",
                rusqlite::params![version],
            )
            .map_err(|e| {
                ProductError::MigrationError(format!("record migration {version}: {e}"))
            })?;
        }
    }

    Ok(())
}

/// Migration 001: 初始 schema -- 全部核心表。
const MIGRATION_001: &str = r#"
-- ============================================================================
-- tasks 表 [doc-06 §3.1]
-- ============================================================================
CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    title TEXT NOT NULL,
    goal TEXT NOT NULL,
    mode TEXT NOT NULL DEFAULT 'ask',
    state TEXT NOT NULL DEFAULT 'idle',
    worktree_path TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_tasks_project ON tasks(project_id);
CREATE INDEX IF NOT EXISTS idx_tasks_state ON tasks(state);

-- ============================================================================
-- agent_runs 表 [doc-06 §3.2]
-- ============================================================================
CREATE TABLE IF NOT EXISTS agent_runs (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    model TEXT NOT NULL,
    review_state TEXT NOT NULL DEFAULT 'pending',
    started_at TEXT NOT NULL,
    ended_at TEXT,
    usage_json TEXT
);
CREATE INDEX IF NOT EXISTS idx_agent_runs_task ON agent_runs(task_id);
CREATE INDEX IF NOT EXISTS idx_agent_runs_active ON agent_runs(task_id) WHERE ended_at IS NULL;

-- ============================================================================
-- tool_calls 表 [doc-06 §3.3] [doc-02 §8]
-- ============================================================================
CREATE TABLE IF NOT EXISTS tool_calls (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    tool_name TEXT NOT NULL,
    input_json TEXT NOT NULL,
    output_json TEXT,
    risk_level TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'running',
    caller TEXT,
    started_at TEXT NOT NULL,
    ended_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_tool_calls_run ON tool_calls(run_id);
CREATE INDEX IF NOT EXISTS idx_tool_calls_task ON tool_calls(task_id);
CREATE INDEX IF NOT EXISTS idx_tool_calls_status ON tool_calls(status);

-- ============================================================================
-- file_changes 表 [doc-06 §3.4]
-- ============================================================================
CREATE TABLE IF NOT EXISTS file_changes (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    tool_call_id TEXT REFERENCES tool_calls(id) ON DELETE SET NULL,
    path TEXT NOT NULL,
    change_type TEXT NOT NULL,
    before_hash TEXT,
    after_hash TEXT,
    old_path TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_file_changes_task ON file_changes(task_id);
CREATE INDEX IF NOT EXISTS idx_file_changes_path ON file_changes(path);

-- ============================================================================
-- file_baselines 表 [doc-06 §3.5]
-- ============================================================================
CREATE TABLE IF NOT EXISTS file_baselines (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    blob_key TEXT NOT NULL,
    captured_at TEXT NOT NULL,
    UNIQUE(task_id, path)
);
CREATE INDEX IF NOT EXISTS idx_file_baselines_task ON file_baselines(task_id);

-- ============================================================================
-- blobs 表 [doc-06 §3.6] [doc-12 §3.5]
-- ============================================================================
CREATE TABLE IF NOT EXISTS blobs (
    hash TEXT PRIMARY KEY,
    ref_count INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL
);

-- ============================================================================
-- permission_requests 表 [doc-06 §3.7]
-- ============================================================================
CREATE TABLE IF NOT EXISTS permission_requests (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    tool_call_id TEXT NOT NULL REFERENCES tool_calls(id) ON DELETE CASCADE,
    tool_name TEXT NOT NULL,
    risk_level TEXT NOT NULL,
    input_summary TEXT NOT NULL,
    decision TEXT NOT NULL DEFAULT 'pending',
    created_at TEXT NOT NULL,
    decided_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_permission_requests_task ON permission_requests(task_id);
CREATE INDEX IF NOT EXISTS idx_permission_requests_pending ON permission_requests(task_id) WHERE decision = 'pending';

-- ============================================================================
-- workspaces 表 [doc-06 §3.8]
-- ============================================================================
CREATE TABLE IF NOT EXISTS workspaces (
    canonical_path TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    trust_state TEXT NOT NULL DEFAULT 'untrusted',
    last_opened_at TEXT NOT NULL
);

-- ============================================================================
-- task_events 表 [doc-06 §3.9] -- JSONL 轻量投影
-- ============================================================================
CREATE TABLE IF NOT EXISTS task_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_task_events_task ON task_events(task_id, created_at);

-- ============================================================================
-- verifications 表 [doc-18 M9]
-- ============================================================================
CREATE TABLE IF NOT EXISTS verifications (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    run_id TEXT NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
    command TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'running',
    output_blob_key TEXT,
    exit_code INTEGER,
    started_at TEXT NOT NULL,
    ended_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_verifications_task ON verifications(task_id);
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_creates_all_tables() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();

        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        for expected in [
            "tasks",
            "agent_runs",
            "tool_calls",
            "file_changes",
            "file_baselines",
            "blobs",
            "permission_requests",
            "workspaces",
            "task_events",
            "verifications",
            "schema_version",
        ] {
            assert!(
                tables.contains(&expected.to_string()),
                "missing table: {expected}"
            );
        }
    }

    #[test]
    fn migration_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        // 再次运行不应出错
        run_migrations(&conn).unwrap();
    }

    #[test]
    fn schema_version_recorded() {
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();
        let version: i64 = conn
            .query_row("SELECT MAX(version) FROM schema_version", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(version, 1);
    }

    #[test]
    fn foreign_keys_enforced() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        run_migrations(&conn).unwrap();
        // 插入 agent_run 引用不存在的 task 应失败
        let result = conn.execute(
            "INSERT INTO agent_runs (id, task_id, model, started_at) VALUES ('r1', 'nonexistent', 'test', '2024-01-01T00:00:00Z')",
            [],
        );
        assert!(result.is_err());
    }
}
