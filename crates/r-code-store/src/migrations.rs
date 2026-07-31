//! 版本化 migration 系统。
//!
//! 使用 `schema_version` 表 + 编号迁移文件。
//! Migration 001 覆盖全部核心表；后续迁移保持旧数据可读。
//! [doc-06 §3, §5]

use r_code_core::error::ProductError;
use rusqlite::Connection;

/// 当前 SQLite schema 的最新版本。
///
/// `src-tauri::migration::MigrationManager` 也引用这个常量，避免产品层的迁移
/// 预检和实际 store 迁移版本发生漂移。
pub const LATEST_SCHEMA_VERSION: u32 = 13;

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
    let migrations: &[(i64, &str)] = &[
        (1, MIGRATION_001),
        (2, MIGRATION_002),
        (3, MIGRATION_003),
        (4, MIGRATION_004),
        (5, MIGRATION_005),
        (6, MIGRATION_006),
        (7, MIGRATION_007),
        (8, MIGRATION_008),
        (9, MIGRATION_009),
        (10, MIGRATION_010),
        (11, MIGRATION_011),
        (12, MIGRATION_012),
        (13, MIGRATION_013),
    ];

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

/// Migration 002: `project_id` 改为可选的 `workspace_path`。
///
/// 纯聊天会话没有本地工作区；SQLite 不能直接移除 NOT NULL 约束，因此以表重建
/// 完成列改名与可空化。历史任务的 project_id 原样迁移为 workspace_path。
const MIGRATION_002: &str = r#"
PRAGMA foreign_keys = OFF;

CREATE TABLE tasks_v2 (
    id TEXT PRIMARY KEY,
    workspace_path TEXT,
    title TEXT NOT NULL,
    goal TEXT NOT NULL,
    mode TEXT NOT NULL DEFAULT 'ask',
    state TEXT NOT NULL DEFAULT 'idle',
    worktree_path TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

INSERT INTO tasks_v2 (id, workspace_path, title, goal, mode, state, worktree_path, created_at, updated_at)
SELECT id, NULLIF(project_id, ''), title, goal, mode, state, worktree_path, created_at, updated_at
FROM tasks;

DROP TABLE tasks;
ALTER TABLE tasks_v2 RENAME TO tasks;

CREATE INDEX IF NOT EXISTS idx_tasks_workspace ON tasks(workspace_path);
CREATE INDEX IF NOT EXISTS idx_tasks_state ON tasks(state);

PRAGMA foreign_keys = ON;
"#;

/// Migration 003: 会话分支与任务级待发送队列。
///
/// 已有运行和事件默认归入 `main`，不重写旧 JSONL；编辑后的会话在新文件中形成
/// 分支快照，因此旧审计记录始终可读。
const MIGRATION_003: &str = r#"
ALTER TABLE agent_runs ADD COLUMN branch_id TEXT NOT NULL DEFAULT 'main';
CREATE INDEX IF NOT EXISTS idx_agent_runs_task_branch ON agent_runs(task_id, branch_id, started_at);

ALTER TABLE task_events ADD COLUMN branch_id TEXT NOT NULL DEFAULT 'main';
CREATE INDEX IF NOT EXISTS idx_task_events_task_branch ON task_events(task_id, branch_id, created_at);

CREATE TABLE IF NOT EXISTS session_branches (
    id TEXT NOT NULL,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    parent_branch_id TEXT,
    forked_from_message_id TEXT,
    storage_id TEXT NOT NULL,
    is_active INTEGER NOT NULL DEFAULT 0 CHECK (is_active IN (0, 1)),
    created_at TEXT NOT NULL,
    PRIMARY KEY (task_id, id)
);
CREATE INDEX IF NOT EXISTS idx_session_branches_task ON session_branches(task_id, created_at DESC);
CREATE UNIQUE INDEX IF NOT EXISTS idx_session_branches_one_active
    ON session_branches(task_id) WHERE is_active = 1;

CREATE TABLE IF NOT EXISTS queued_messages (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    branch_id TEXT NOT NULL,
    message TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'queued',
    priority INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_queued_messages_pending
    ON queued_messages(state, priority DESC, created_at ASC);
CREATE INDEX IF NOT EXISTS idx_queued_messages_task_branch
    ON queued_messages(task_id, branch_id, state, priority DESC, created_at ASC);
"#;

/// Migration 004: Agent Run 委派树。
///
/// 旧运行保持主运行语义；新列均可由默认值或 NULL 表示，以便已有数据库原地升级。
const MIGRATION_004: &str = r#"
ALTER TABLE agent_runs ADD COLUMN parent_run_id TEXT
    REFERENCES agent_runs(id) ON DELETE SET NULL;
ALTER TABLE agent_runs ADD COLUMN agent_kind TEXT NOT NULL DEFAULT 'main';
ALTER TABLE agent_runs ADD COLUMN agent_label TEXT;
ALTER TABLE agent_runs ADD COLUMN delegated_by_tool_call_id TEXT
    REFERENCES tool_calls(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS idx_agent_runs_parent
    ON agent_runs(parent_run_id, started_at);
CREATE INDEX IF NOT EXISTS idx_agent_runs_delegated_by_tool_call
    ON agent_runs(delegated_by_tool_call_id);
"#;

/// Migration 005: 子代理完成摘要。
const MIGRATION_005: &str = r#"
ALTER TABLE agent_runs ADD COLUMN summary TEXT;
"#;

/// Migration 006: 会话级模型服务绑定。
///
/// 旧会话保留 NULL，并在首次运行时回退到全局默认服务；新会话和显式切换会写入
/// 已选择的配置名，避免全局默认变更意外改变既有会话。
const MIGRATION_006: &str = r#"
ALTER TABLE tasks ADD COLUMN provider_name TEXT;
CREATE INDEX IF NOT EXISTS idx_tasks_provider_name ON tasks(provider_name);
"#;

/// Migration 007: 项目级 Agent 权限模式。
///
/// 保留旧 `trust_state` 仅为向后兼容已有数据库；运行时和新代码统一以
/// `access_mode` 为准。旧的未授信项目以最保守的“请求批准”迁移，
/// 已授信项目则保持原有风险分级体验。
const MIGRATION_007: &str = r#"
ALTER TABLE workspaces ADD COLUMN access_mode TEXT NOT NULL DEFAULT 'request_approval';
UPDATE workspaces
SET access_mode = CASE trust_state
    WHEN 'trusted' THEN 'risk_based'
    ELSE 'request_approval'
END;
"#;

/// Migration 008: 会话级具体模型绑定。
///
/// NULL 语义与 `provider_name` 对称：沿用该服务在设置里配置的默认模型。
/// 运行时（LlmAgentRuntime）本就支持 per-session 覆盖，这里只是补上持久化。
const MIGRATION_008: &str = r#"
ALTER TABLE tasks ADD COLUMN model TEXT;
"#;

/// Migration 009: 修复历史遗留的"验证占位 Run"。
///
/// `run_verification` 曾在无活跃 Run 时插入 `model = 'verification'` 的主 Run
/// 且从不写 `ended_at`，导致时间线永久转圈、任务被判定为永久运行中、
/// 崩溃恢复误报，并顶替 `get_active_run` 让接受动作写到错误的记录上。
///
/// 这里只补 `ended_at` 而**不删除**这些 Run：`verifications.run_id` 是
/// `ON DELETE CASCADE`，删 Run 会连带丢掉用户的验证历史。
///
/// 四道守卫确保不会误伤真实的运行中记录：
///   - `model = 'verification'` 是唯一指纹（真实 run 的 model 来自 provider 配置）
///   - `review_state = 'pending'` 排除已被任何流程改写过状态的 run
///   - 没有任何 tool_call 挂在它下面（真实 run 一开跑就会产生工具调用）
///   - 没有子代理以它为父
///
/// 结束时刻取该 run 对应验证的最晚 ended_at，取不到则退回 started_at
/// （时长为 0 好过显示"跑了三个月"）。`ended_at IS NULL` 使其可重复执行。
const MIGRATION_009: &str = r#"
UPDATE agent_runs
SET ended_at = COALESCE(
        (SELECT MAX(v.ended_at) FROM verifications v
          WHERE v.run_id = agent_runs.id AND v.ended_at IS NOT NULL),
        started_at
    )
WHERE model = 'verification'
  AND agent_kind = 'main'
  AND ended_at IS NULL
  AND review_state = 'pending'
  AND parent_run_id IS NULL
  AND NOT EXISTS (SELECT 1 FROM tool_calls tc WHERE tc.run_id = agent_runs.id)
  AND NOT EXISTS (SELECT 1 FROM agent_runs child WHERE child.parent_run_id = agent_runs.id);
"#;

/// Migration 010: 用户通知中心。
///
/// `source_key` 是可重放的去重键：轮询同步同一个权限请求或同一轮审查时，只刷新
/// 文案，不会每次都制造新的未读记录。`sequence` 仅用于稳定游标，不暴露给业务 UI。
const MIGRATION_010: &str = r#"
CREATE TABLE IF NOT EXISTS notifications (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    source_key TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    task_id TEXT REFERENCES tasks(id) ON DELETE SET NULL,
    workspace_path TEXT,
    created_at TEXT NOT NULL,
    read_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_notifications_unread
    ON notifications(read_at, sequence DESC);
CREATE INDEX IF NOT EXISTS idx_notifications_task
    ON notifications(task_id, sequence DESC);
"#;

/// Migration 011: 外部 Agent 执行驱动与可恢复会话标识。
///
/// 历史记录默认 `native`，不改变原有 R-Code provider runtime 的语义。外部会话 ID
/// 仅保存用于续接和诊断的公开标识，绝不保存认证令牌或完整外部转录。
const MIGRATION_011: &str = r#"
ALTER TABLE agent_runs ADD COLUMN runtime_kind TEXT NOT NULL DEFAULT 'native';
ALTER TABLE agent_runs ADD COLUMN external_session_id TEXT;
CREATE INDEX IF NOT EXISTS idx_agent_runs_runtime_kind
    ON agent_runs(runtime_kind, started_at DESC);
CREATE INDEX IF NOT EXISTS idx_agent_runs_external_session
    ON agent_runs(external_session_id)
    WHERE external_session_id IS NOT NULL;
"#;

/// Migration 012: 会话级主 Agent 引擎与可审计的委派策略元数据。
///
/// 旧会话仍由 R-Code 执行；旧子运行按最小权限显示为只读。路由理由只保存策略
/// 结论（例如“复杂任务优先 Codex”），不保存任何模型私有推理。
const MIGRATION_012: &str = r#"
ALTER TABLE tasks ADD COLUMN agent_engine TEXT NOT NULL DEFAULT 'r_code';
CREATE INDEX IF NOT EXISTS idx_tasks_agent_engine
    ON tasks(agent_engine, updated_at DESC);

ALTER TABLE agent_runs ADD COLUMN access_mode TEXT NOT NULL DEFAULT 'read_only';
ALTER TABLE agent_runs ADD COLUMN routing_reason TEXT;
"#;

/// Migration 013: 会话级模型推理参数。
///
/// JSON 只保存用户显式覆盖的 thinking / reasoning_effort / verbosity；空对象表示
/// 完全沿用模型服务默认值，确保旧会话与未知兼容网关不会收到额外参数。
const MIGRATION_013: &str = r#"
ALTER TABLE tasks ADD COLUMN inference_json TEXT NOT NULL DEFAULT '{}';
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归：v9 只结束"验证占位 run"，不碰真实的运行中记录，也不删任何行。
    #[test]
    fn migration_v9_ends_verification_placeholder_runs_only() {
        // 先跑全量迁移建好表结构，再直接重放 009。不能只删 schema_version=9
        // 后调用 run_migrations：一旦后续已有 v10/v11，MAX(version) 仍会跳过 v9。
        let conn = Connection::open_in_memory().unwrap();
        run_migrations(&conn).unwrap();

        conn.execute_batch(
            "INSERT INTO tasks (id, workspace_path, title, goal, mode, state, created_at, updated_at)
             VALUES ('t1', '/p', 'T', 'g', 'edit', 'review_ready', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z');
             -- 验证占位 run：应被结束
             INSERT INTO agent_runs (id, task_id, model, review_state, started_at, agent_kind, branch_id)
             VALUES ('fake', 't1', 'verification', 'pending', '2026-01-01T00:01:00Z', 'main', 'main');
             -- 真实运行中的 run：必须原封不动
             INSERT INTO agent_runs (id, task_id, model, review_state, started_at, agent_kind, branch_id)
             VALUES ('real', 't1', 'deepseek-v4-pro', 'pending', '2026-01-01T00:02:00Z', 'main', 'main');",
        )
        .unwrap();

        conn.execute_batch(MIGRATION_009).unwrap();

        let ended_of = |id: &str| -> Option<String> {
            conn.query_row(
                "SELECT ended_at FROM agent_runs WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .unwrap()
        };
        let fake_ended = ended_of("fake");
        let real_ended = ended_of("real");
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM agent_runs", [], |row| row.get(0))
            .unwrap();

        assert_eq!(fake_ended.as_deref(), Some("2026-01-01T00:01:00Z"));
        assert!(real_ended.is_none(), "真实运行中的 run 不能被结束");
        assert_eq!(count, 2, "迁移只能 UPDATE，不能删除任何行");
    }

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
            "session_branches",
            "queued_messages",
            "notifications",
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
        assert_eq!(version, i64::from(LATEST_SCHEMA_VERSION));
    }

    #[test]
    fn migration_v7_maps_legacy_workspace_trust_to_access_mode() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE schema_version (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL DEFAULT (datetime('now')));
            INSERT INTO schema_version (version) VALUES (6);
            CREATE TABLE IF NOT EXISTS verifications (
                id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL,
                run_id TEXT NOT NULL,
                command TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'running',
                started_at TEXT NOT NULL,
                ended_at TEXT
            );
            CREATE TABLE workspaces (
                canonical_path TEXT PRIMARY KEY,
                display_name TEXT NOT NULL,
                trust_state TEXT NOT NULL DEFAULT 'untrusted',
                last_opened_at TEXT NOT NULL
            );
            INSERT INTO workspaces (canonical_path, display_name, trust_state, last_opened_at)
            VALUES
                ('/legacy-untrusted', 'Legacy Untrusted', 'untrusted', '2025-01-01T00:00:00Z'),
                ('/legacy-trusted', 'Legacy Trusted', 'trusted', '2025-01-01T00:00:00Z');
            -- 后续迁移的 SQL 会引用下列表/列：SQLite 在 prepare 阶段就解析表名，
            -- 与表里有没有数据无关。真实旧库在 v1/v3/v4 已建好它们，
            -- 这里补齐骨架以复现真实的升级路径（已存在的会被 IF NOT EXISTS 跳过）。
            CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                goal TEXT NOT NULL,
                mode TEXT NOT NULL DEFAULT 'ask',
                state TEXT NOT NULL DEFAULT 'idle',
                worktree_path TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS agent_runs (
                id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL,
                model TEXT NOT NULL,
                review_state TEXT NOT NULL DEFAULT 'pending',
                started_at TEXT NOT NULL,
                ended_at TEXT,
                usage_json TEXT,
                branch_id TEXT NOT NULL DEFAULT 'main',
                agent_kind TEXT NOT NULL DEFAULT 'main',
                parent_run_id TEXT
            );
            CREATE TABLE IF NOT EXISTS tool_calls (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                task_id TEXT NOT NULL,
                tool_name TEXT NOT NULL,
                input_json TEXT NOT NULL,
                risk_level TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'running',
                started_at TEXT NOT NULL,
                ended_at TEXT
            );
            "#,
        )
        .unwrap();

        run_migrations(&conn).unwrap();

        let untrusted: String = conn
            .query_row(
                "SELECT access_mode FROM workspaces WHERE canonical_path = '/legacy-untrusted'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let trusted: String = conn
            .query_row(
                "SELECT access_mode FROM workspaces WHERE canonical_path = '/legacy-trusted'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(untrusted, "request_approval");
        assert_eq!(trusted, "risk_based");
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

    #[test]
    fn migration_v2_preserves_legacy_project_and_allows_pure_chat() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE schema_version (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL DEFAULT (datetime('now')));
            INSERT INTO schema_version (version) VALUES (1);
            CREATE TABLE IF NOT EXISTS verifications (
                id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL,
                run_id TEXT NOT NULL,
                command TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'running',
                started_at TEXT NOT NULL,
                ended_at TEXT
            );
            CREATE TABLE tasks (
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
            CREATE TABLE workspaces (
                canonical_path TEXT PRIMARY KEY,
                display_name TEXT NOT NULL,
                trust_state TEXT NOT NULL DEFAULT 'untrusted',
                last_opened_at TEXT NOT NULL
            );
            CREATE TABLE agent_runs (
                id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                model TEXT NOT NULL,
                review_state TEXT NOT NULL DEFAULT 'pending',
                started_at TEXT NOT NULL,
                ended_at TEXT,
                usage_json TEXT
            );
            CREATE TABLE task_events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                event_type TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            INSERT INTO tasks (id, project_id, title, goal, mode, state, created_at, updated_at)
            VALUES ('legacy', '/legacy/workspace', 'Legacy', 'Goal', 'ask', 'idle', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z');
            -- 后续迁移的 SQL 会引用下列表/列：SQLite 在 prepare 阶段就解析表名，
            -- 与表里有没有数据无关。真实旧库在 v1/v3/v4 已建好它们，
            -- 这里补齐骨架以复现真实的升级路径（已存在的会被 IF NOT EXISTS 跳过）。
            CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                goal TEXT NOT NULL,
                mode TEXT NOT NULL DEFAULT 'ask',
                state TEXT NOT NULL DEFAULT 'idle',
                worktree_path TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS agent_runs (
                id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL,
                model TEXT NOT NULL,
                review_state TEXT NOT NULL DEFAULT 'pending',
                started_at TEXT NOT NULL,
                ended_at TEXT,
                usage_json TEXT,
                branch_id TEXT NOT NULL DEFAULT 'main',
                agent_kind TEXT NOT NULL DEFAULT 'main',
                parent_run_id TEXT
            );
            CREATE TABLE IF NOT EXISTS tool_calls (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                task_id TEXT NOT NULL,
                tool_name TEXT NOT NULL,
                input_json TEXT NOT NULL,
                risk_level TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'running',
                started_at TEXT NOT NULL,
                ended_at TEXT
            );
            "#,
        )
        .unwrap();

        run_migrations(&conn).unwrap();

        let migrated: Option<String> = conn
            .query_row(
                "SELECT workspace_path FROM tasks WHERE id = 'legacy'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(migrated.as_deref(), Some("/legacy/workspace"));

        conn.execute(
            "INSERT INTO tasks (id, workspace_path, title, goal, mode, state, created_at, updated_at) \
             VALUES ('chat', NULL, 'Chat', 'No workspace', 'ask', 'idle', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        let empty_scope: Option<String> = conn
            .query_row(
                "SELECT workspace_path FROM tasks WHERE id = 'chat'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(empty_scope.is_none());
    }

    #[test]
    fn migration_v4_preserves_legacy_runs_and_adds_tree_columns() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;
            CREATE TABLE schema_version (version INTEGER PRIMARY KEY, applied_at TEXT NOT NULL DEFAULT (datetime('now')));
            INSERT INTO schema_version (version) VALUES (3);
            CREATE TABLE IF NOT EXISTS verifications (
                id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL,
                run_id TEXT NOT NULL,
                command TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'running',
                started_at TEXT NOT NULL,
                ended_at TEXT
            );
            CREATE TABLE tasks (
                id TEXT PRIMARY KEY,
                workspace_path TEXT,
                title TEXT NOT NULL,
                goal TEXT NOT NULL,
                mode TEXT NOT NULL DEFAULT 'ask',
                state TEXT NOT NULL DEFAULT 'idle',
                worktree_path TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE workspaces (
                canonical_path TEXT PRIMARY KEY,
                display_name TEXT NOT NULL,
                trust_state TEXT NOT NULL DEFAULT 'untrusted',
                last_opened_at TEXT NOT NULL
            );
            CREATE TABLE agent_runs (
                id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                model TEXT NOT NULL,
                review_state TEXT NOT NULL DEFAULT 'pending',
                started_at TEXT NOT NULL,
                ended_at TEXT,
                usage_json TEXT,
                branch_id TEXT NOT NULL DEFAULT 'main'
            );
            CREATE TABLE tool_calls (
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
            INSERT INTO tasks (id, workspace_path, title, goal, mode, state, created_at, updated_at)
            VALUES ('task-1', '/legacy', 'Legacy', 'Goal', 'ask', 'idle', '2025-01-01T00:00:00Z', '2025-01-01T00:00:00Z');
            INSERT INTO agent_runs (id, task_id, model, review_state, started_at, branch_id)
            VALUES ('run-1', 'task-1', 'legacy-model', 'pending', '2025-01-01T00:00:00Z', 'main');
            -- 后续迁移的 SQL 会引用下列表/列：SQLite 在 prepare 阶段就解析表名，
            -- 与表里有没有数据无关。真实旧库在 v1/v3/v4 已建好它们，
            -- 这里补齐骨架以复现真实的升级路径（已存在的会被 IF NOT EXISTS 跳过）。
            CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                goal TEXT NOT NULL,
                mode TEXT NOT NULL DEFAULT 'ask',
                state TEXT NOT NULL DEFAULT 'idle',
                worktree_path TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS agent_runs (
                id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL,
                model TEXT NOT NULL,
                review_state TEXT NOT NULL DEFAULT 'pending',
                started_at TEXT NOT NULL,
                ended_at TEXT,
                usage_json TEXT,
                branch_id TEXT NOT NULL DEFAULT 'main',
                agent_kind TEXT NOT NULL DEFAULT 'main',
                parent_run_id TEXT
            );
            CREATE TABLE IF NOT EXISTS tool_calls (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL,
                task_id TEXT NOT NULL,
                tool_name TEXT NOT NULL,
                input_json TEXT NOT NULL,
                risk_level TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'running',
                started_at TEXT NOT NULL,
                ended_at TEXT
            );
            "#,
        )
        .unwrap();

        run_migrations(&conn).unwrap();

        type LegacyValues = (
            String,
            Option<String>,
            String,
            Option<String>,
            Option<String>,
            String,
            Option<String>,
        );
        let legacy_values: LegacyValues = conn
            .query_row(
                "SELECT branch_id, parent_run_id, agent_kind, agent_label, delegated_by_tool_call_id, \
                        runtime_kind, external_session_id \
                 FROM agent_runs WHERE id = 'run-1'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(legacy_values.0, "main");
        assert!(legacy_values.1.is_none());
        assert_eq!(legacy_values.2, "main");
        assert!(legacy_values.3.is_none());
        assert!(legacy_values.4.is_none());
        assert_eq!(legacy_values.5, "native");
        assert!(legacy_values.6.is_none());

        let columns: Vec<String> = conn
            .prepare("PRAGMA table_info(agent_runs)")
            .unwrap()
            .query_map([], |row| row.get(1))
            .unwrap()
            .filter_map(|row| row.ok())
            .collect();
        for expected in [
            "parent_run_id",
            "agent_kind",
            "agent_label",
            "summary",
            "delegated_by_tool_call_id",
            "runtime_kind",
            "external_session_id",
        ] {
            assert!(
                columns.contains(&expected.to_string()),
                "missing agent_runs column: {expected}"
            );
        }

        let indexes: Vec<String> = conn
            .prepare(
                "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'agent_runs'",
            )
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|row| row.ok())
            .collect();
        assert!(indexes.contains(&"idx_agent_runs_parent".to_string()));
        assert!(indexes.contains(&"idx_agent_runs_delegated_by_tool_call".to_string()));
        assert!(indexes.contains(&"idx_agent_runs_runtime_kind".to_string()));
        assert!(indexes.contains(&"idx_agent_runs_external_session".to_string()));
    }
}
