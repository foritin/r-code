//! 版本化 migration 系统。
//!
//! 使用 `schema_version` 表 + 编号迁移文件。
//! Migration 001 覆盖全部核心表；后续迁移保持旧数据可读。
//! [doc-06 §3, §5]

use r_code_core::error::ProductError;
use rusqlite::{params, Connection, Transaction, TransactionBehavior};

/// 当前 SQLite schema 的最新版本。
///
/// `src-tauri::migration::MigrationManager` 也引用这个常量，避免产品层的迁移
/// 预检和实际 store 迁移版本发生漂移。
pub const LATEST_SCHEMA_VERSION: u32 = 18;

#[derive(Clone, Copy)]
struct MigrationSpec {
    version: i64,
    sql: &'static str,
    requires_foreign_keys_off: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MigrationFaultPoint {
    AfterSqlBeforeVersion,
    BeforeCommit,
}

type MigrationFaultHook<'a> = dyn Fn(i64, MigrationFaultPoint) -> Result<(), ProductError> + 'a;

const MIGRATIONS: &[MigrationSpec] = &[
    MigrationSpec::new(1, MIGRATION_001, false),
    MigrationSpec::new(2, MIGRATION_002, true),
    MigrationSpec::new(3, MIGRATION_003, false),
    MigrationSpec::new(4, MIGRATION_004, false),
    MigrationSpec::new(5, MIGRATION_005, false),
    MigrationSpec::new(6, MIGRATION_006, false),
    MigrationSpec::new(7, MIGRATION_007, false),
    MigrationSpec::new(8, MIGRATION_008, false),
    MigrationSpec::new(9, MIGRATION_009, false),
    MigrationSpec::new(10, MIGRATION_010, false),
    MigrationSpec::new(11, MIGRATION_011, false),
    MigrationSpec::new(12, MIGRATION_012, false),
    MigrationSpec::new(13, MIGRATION_013, false),
    MigrationSpec::new(14, MIGRATION_014, true),
    MigrationSpec::new(15, MIGRATION_015, false),
    MigrationSpec::new(16, MIGRATION_016, false),
    MigrationSpec::new(17, MIGRATION_017, false),
    MigrationSpec::new(18, MIGRATION_018, false),
];

impl MigrationSpec {
    const fn new(version: i64, sql: &'static str, requires_foreign_keys_off: bool) -> Self {
        Self {
            version,
            sql,
            requires_foreign_keys_off,
        }
    }
}

/// 运行所有待执行的 migration。
pub fn run_migrations(conn: &Connection) -> Result<(), ProductError> {
    validate_registry(MIGRATIONS)?;
    let latest = MIGRATIONS.last().map(|spec| spec.version);
    if latest != Some(i64::from(LATEST_SCHEMA_VERSION)) {
        return Err(ProductError::MigrationError(format!(
            "migration registry latest version {latest:?} does not match {LATEST_SCHEMA_VERSION}"
        )));
    }
    run_migrations_with_specs(conn, MIGRATIONS, None)
}

fn run_migrations_with_specs(
    conn: &Connection,
    migrations: &[MigrationSpec],
    fault_hook: Option<&MigrationFaultHook<'_>>,
) -> Result<(), ProductError> {
    validate_registry(migrations)?;
    initialize_schema_version(conn)?;
    for spec in migrations {
        apply_one_migration(conn, spec, fault_hook)?;
    }
    Ok(())
}

fn validate_registry(migrations: &[MigrationSpec]) -> Result<(), ProductError> {
    for (index, spec) in migrations.iter().enumerate() {
        let expected = i64::try_from(index + 1)
            .map_err(|_| ProductError::MigrationError("migration registry is too large".into()))?;
        if spec.version != expected {
            return Err(ProductError::MigrationError(format!(
                "migration registry expected version {expected}, found {}",
                spec.version
            )));
        }
    }
    Ok(())
}

fn initialize_schema_version(conn: &Connection) -> Result<(), ProductError> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map_err(|error| {
        ProductError::MigrationError(format!("begin schema_version transaction: {error}"))
    })?;
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (\n\
         version INTEGER PRIMARY KEY,\n\
         applied_at TEXT NOT NULL DEFAULT (datetime('now'))\n\
         );",
    )
    .map_err(|error| {
        ProductError::MigrationError(format!("create schema_version table: {error}"))
    })?;
    tx.commit().map_err(|error| {
        ProductError::MigrationError(format!("commit schema_version table: {error}"))
    })
}

fn apply_one_migration(
    conn: &Connection,
    spec: &MigrationSpec,
    fault_hook: Option<&MigrationFaultHook<'_>>,
) -> Result<(), ProductError> {
    if !spec.requires_foreign_keys_off {
        return apply_one_migration_transaction(conn, spec, fault_hook);
    }

    set_foreign_keys(conn, false)?;
    let migration_result = apply_one_migration_transaction(conn, spec, fault_hook);
    let restore_result = set_foreign_keys(conn, true);
    match (migration_result, restore_result) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(restore_error)) => Err(ProductError::MigrationError(format!(
            "{error}; additionally failed to restore foreign keys: {restore_error}"
        ))),
    }
}

fn apply_one_migration_transaction(
    conn: &Connection,
    spec: &MigrationSpec,
    fault_hook: Option<&MigrationFaultHook<'_>>,
) -> Result<(), ProductError> {
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map_err(|error| {
        ProductError::MigrationError(format!("begin migration {}: {error}", spec.version))
    })?;
    let current = current_version(&tx)?;
    if spec.version <= current {
        return tx.commit().map_err(|error| {
            ProductError::MigrationError(format!(
                "commit skipped migration {}: {error}",
                spec.version
            ))
        });
    }

    tracing::info!(version = spec.version, "applying migration");
    tx.execute_batch(spec.sql).map_err(|error| {
        ProductError::MigrationError(format!("migration {}: {error}", spec.version))
    })?;
    if spec.requires_foreign_keys_off {
        ensure_foreign_keys_valid(&tx, spec.version)?;
    }
    run_fault_hook(
        fault_hook,
        spec.version,
        MigrationFaultPoint::AfterSqlBeforeVersion,
    )?;
    tx.execute(
        "INSERT INTO schema_version (version) VALUES (?1)",
        params![spec.version],
    )
    .map_err(|error| {
        ProductError::MigrationError(format!("record migration {}: {error}", spec.version))
    })?;
    run_fault_hook(fault_hook, spec.version, MigrationFaultPoint::BeforeCommit)?;
    tx.commit().map_err(|error| {
        ProductError::MigrationError(format!("commit migration {}: {error}", spec.version))
    })
}

fn current_version(tx: &Transaction<'_>) -> Result<i64, ProductError> {
    tx.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_version",
        [],
        |row| row.get(0),
    )
    .map_err(|error| ProductError::MigrationError(format!("read schema_version: {error}")))
}

fn ensure_foreign_keys_valid(tx: &Transaction<'_>, version: i64) -> Result<(), ProductError> {
    let mut statement = tx.prepare("PRAGMA foreign_key_check").map_err(|error| {
        ProductError::MigrationError(format!(
            "prepare foreign key check for migration {version}: {error}"
        ))
    })?;
    let mut violations = statement.query([]).map_err(|error| {
        ProductError::MigrationError(format!(
            "run foreign key check for migration {version}: {error}"
        ))
    })?;
    if violations
        .next()
        .map_err(|error| {
            ProductError::MigrationError(format!(
                "read foreign key check for migration {version}: {error}"
            ))
        })?
        .is_some()
    {
        return Err(ProductError::MigrationError(format!(
            "migration {version} failed foreign_key_check"
        )));
    }
    Ok(())
}

fn set_foreign_keys(conn: &Connection, enabled: bool) -> Result<(), ProductError> {
    let value = if enabled { "ON" } else { "OFF" };
    conn.execute_batch(&format!("PRAGMA foreign_keys = {value};"))
        .map_err(|error| {
            ProductError::MigrationError(format!("set foreign_keys {value}: {error}"))
        })?;
    let actual: i64 = conn
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .map_err(|error| {
            ProductError::MigrationError(format!(
                "read foreign_keys after setting {value}: {error}"
            ))
        })?;
    if actual != i64::from(enabled) {
        return Err(ProductError::MigrationError(format!(
            "foreign_keys remained {actual} after setting {value}"
        )));
    }
    Ok(())
}

fn run_fault_hook(
    fault_hook: Option<&MigrationFaultHook<'_>>,
    version: i64,
    point: MigrationFaultPoint,
) -> Result<(), ProductError> {
    match fault_hook {
        Some(hook) => hook(version, point),
        None => Ok(()),
    }
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

/// Migration 014: 稳定的 workspace owner key 与项目记忆模式。
///
/// `canonical_path` 仍然唯一，但不再承担未来 memory 外键的所有者身份。重建时为
/// 每个既有 workspace 生成一次 128-bit 本地 id；本迁移不创建或启用任何记忆数据。
const MIGRATION_014: &str = r#"
CREATE TABLE workspaces_v14 (
    id TEXT NOT NULL UNIQUE,
    canonical_path TEXT NOT NULL UNIQUE,
    display_name TEXT NOT NULL,
    access_mode TEXT NOT NULL DEFAULT 'request_approval'
        CHECK (access_mode IN ('request_approval', 'risk_based', 'full_access')),
    last_opened_at TEXT NOT NULL,
    memory_mode TEXT NOT NULL DEFAULT 'inherit'
        CHECK (memory_mode IN ('inherit', 'read_only', 'off')),
    memory_generation INTEGER NOT NULL DEFAULT 1
        CHECK (memory_generation >= 1)
);

INSERT INTO workspaces_v14 (
    id,
    canonical_path,
    display_name,
    access_mode,
    last_opened_at,
    memory_mode,
    memory_generation
)
SELECT
    lower(hex(randomblob(16))),
    canonical_path,
    display_name,
    access_mode,
    last_opened_at,
    'inherit',
    1
FROM workspaces;

DROP TABLE workspaces;
ALTER TABLE workspaces_v14 RENAME TO workspaces;
CREATE INDEX idx_workspaces_last_opened
    ON workspaces(last_opened_at DESC);
"#;

/// Migration 015: 修复旧安装中通知迁移版本与实际 schema 漂移的问题。
///
/// 部分历史数据库已经记录 v10-v14，但 `notifications` 表并不存在。以新的向前
/// 迁移重放 v10 的幂等 DDL，确保正常启动即可修复，而不是在各业务查询中吞错。
const MIGRATION_015: &str = MIGRATION_010;

/// Migration 016: task-run Git snapshots and idempotent snapshot change rows.
///
/// The entry index tree and full worktree tree are kept separately so Review can later
/// distinguish agent changes from files that were already dirty before the run.
const MIGRATION_016: &str = r#"
CREATE TABLE IF NOT EXISTS run_workspace_snapshots (
    run_id TEXT PRIMARY KEY REFERENCES agent_runs(id) ON DELETE CASCADE,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    repo_root TEXT NOT NULL,
    workspace_root TEXT NOT NULL,
    entry_head_tree TEXT,
    entry_index_tree TEXT NOT NULL,
    entry_worktree_tree TEXT NOT NULL,
    exit_worktree_tree TEXT,
    captured_at TEXT NOT NULL,
    finalized_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_run_workspace_snapshots_task
    ON run_workspace_snapshots(task_id, captured_at DESC);

CREATE TABLE IF NOT EXISTS run_snapshot_changes (
    run_id TEXT NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    file_change_id TEXT NOT NULL UNIQUE REFERENCES file_changes(id) ON DELETE CASCADE,
    PRIMARY KEY (run_id, path)
);
"#;

/// Migration 017: application-owned review sessions and decisions.
///
/// Review is intentionally independent from Git's index. A decision only mutates these
/// tables; staging, committing, and pushing remain explicit delivery operations. The run-scoped
/// session key prevents an old acceptance from leaking into a later run that happens to touch
/// the same path.
const MIGRATION_017: &str = r#"
CREATE TABLE IF NOT EXISTS review_sessions (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL UNIQUE REFERENCES agent_runs(id) ON DELETE CASCADE,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    state TEXT NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'resolved', 'superseded')),
    materialized_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_review_sessions_task
    ON review_sessions(task_id, created_at DESC);

CREATE TABLE IF NOT EXISTS review_files (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES review_sessions(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    before_hash TEXT,
    after_hash TEXT,
    state TEXT NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'accepted', 'rejected', 'conflict')),
    blocker TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE (session_id, path)
);
CREATE INDEX IF NOT EXISTS idx_review_files_session_state
    ON review_files(session_id, state, path);

CREATE TABLE IF NOT EXISTS review_items (
    id TEXT PRIMARY KEY,
    review_file_id TEXT NOT NULL REFERENCES review_files(id) ON DELETE CASCADE,
    item_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'accepted', 'rejected')),
    decided_at TEXT,
    UNIQUE (review_file_id, item_id)
);
CREATE INDEX IF NOT EXISTS idx_review_items_file_state
    ON review_items(review_file_id, state, ordinal);
"#;

/// Migration 018: local, evolving memory.
///
/// Memory content is deliberately stored only in the product-owned AppData database.  Project
/// ownership uses the stable workspace id introduced in v14; no table stores a workspace path,
/// provider credential, tool output, attachment body, or hidden model reasoning.
const MIGRATION_018: &str = r#"
CREATE TABLE memory_settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
    review_provider_name TEXT,
    review_model TEXT,
    trigger_every_turns INTEGER NOT NULL DEFAULT 10 CHECK (trigger_every_turns BETWEEN 5 AND 50),
    explicit_remember_immediate INTEGER NOT NULL DEFAULT 1
        CHECK (explicit_remember_immediate IN (0, 1)),
    project_notification_mode TEXT NOT NULL DEFAULT 'on'
        CHECK (project_notification_mode IN ('off', 'on', 'verbose')),
    version INTEGER NOT NULL DEFAULT 0 CHECK (version >= 0),
    review_generation INTEGER NOT NULL DEFAULT 0 CHECK (review_generation >= 0),
    retention_time_high_watermark TEXT NOT NULL,
    physical_cleanup_pending INTEGER NOT NULL DEFAULT 0
        CHECK (physical_cleanup_pending IN (0, 1)),
    physical_cleanup_epoch INTEGER NOT NULL DEFAULT 0 CHECK (physical_cleanup_epoch >= 0),
    updated_at TEXT NOT NULL,
    CHECK (
        (enabled = 0)
        OR (
            review_provider_name IS NOT NULL AND length(trim(review_provider_name)) > 0
            AND review_model IS NOT NULL AND length(trim(review_model)) > 0
        )
    )
);

INSERT INTO memory_settings (
    id, retention_time_high_watermark, updated_at
) VALUES (1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'), strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));

CREATE TABLE memory_entries (
    id TEXT PRIMARY KEY,
    scope TEXT NOT NULL CHECK (scope IN ('global', 'project')),
    workspace_id TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('preference', 'constraint', 'convention', 'decision', 'pitfall')),
    content TEXT NOT NULL CHECK (length(content) BETWEEN 1 AND 1000),
    normalized_hash TEXT NOT NULL,
    version INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1),
    origin TEXT NOT NULL CHECK (origin IN ('manual', 'approved_candidate', 'automatic_review', 'undo')),
    pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
    source_job_id TEXT,
    source_candidate_id TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK (
        (scope = 'global' AND workspace_id IS NULL
            AND origin IN ('manual', 'approved_candidate'))
        OR
        (scope = 'project' AND workspace_id IS NOT NULL
            AND origin IN ('manual', 'automatic_review', 'undo'))
    ),
    UNIQUE (scope, workspace_id, normalized_hash)
);
CREATE UNIQUE INDEX idx_memory_entries_global_hash
    ON memory_entries(normalized_hash) WHERE scope = 'global';
CREATE INDEX idx_memory_entries_owner
    ON memory_entries(scope, workspace_id, pinned DESC, updated_at DESC, id);

CREATE TABLE memory_entry_revisions (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    entry_id TEXT NOT NULL REFERENCES memory_entries(id) ON DELETE CASCADE,
    prior_kind TEXT NOT NULL CHECK (prior_kind IN ('preference', 'constraint', 'convention', 'decision', 'pitfall')),
    prior_content TEXT NOT NULL CHECK (length(prior_content) BETWEEN 1 AND 1000),
    prior_normalized_hash TEXT NOT NULL,
    prior_version INTEGER NOT NULL CHECK (prior_version >= 1),
    prior_pinned INTEGER NOT NULL CHECK (prior_pinned IN (0, 1)),
    action TEXT NOT NULL CHECK (action IN ('edit', 'automatic_replace', 'undo')),
    source_job_id TEXT,
    source_candidate_id TEXT,
    created_at TEXT NOT NULL
);
CREATE INDEX idx_memory_revisions_entry
    ON memory_entry_revisions(entry_id, sequence DESC);

CREATE TABLE memory_review_turns (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    run_id TEXT NOT NULL UNIQUE REFERENCES agent_runs(id) ON DELETE CASCADE,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    branch_id TEXT NOT NULL,
    source_workspace_id TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    workspace_memory_generation INTEGER,
    global_generation INTEGER NOT NULL CHECK (global_generation >= 0),
    user_text TEXT,
    assistant_text TEXT,
    captured_at TEXT NOT NULL,
    scrubbed_at TEXT,
    scrub_reason TEXT CHECK (scrub_reason IS NULL OR scrub_reason IN (
        'review_succeeded', 'cancelled', 'invalidated', 'retention_expired', 'capacity_evicted'
    )),
    CHECK (
        (source_workspace_id IS NULL AND workspace_memory_generation IS NULL)
        OR (source_workspace_id IS NOT NULL AND workspace_memory_generation IS NOT NULL)
    ),
    CHECK (
        (user_text IS NOT NULL AND assistant_text IS NOT NULL
            AND scrubbed_at IS NULL AND scrub_reason IS NULL)
        OR (user_text IS NULL AND assistant_text IS NULL
            AND scrubbed_at IS NOT NULL AND scrub_reason IS NOT NULL)
    )
);
CREATE INDEX idx_memory_turns_branch
    ON memory_review_turns(task_id, branch_id, sequence);

CREATE TABLE memory_review_jobs (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    branch_id TEXT NOT NULL,
    source_run_id TEXT NOT NULL REFERENCES agent_runs(id) ON DELETE CASCADE,
    source_workspace_id TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    workspace_memory_generation INTEGER,
    review_generation INTEGER NOT NULL CHECK (review_generation >= 0),
    provider_name TEXT NOT NULL,
    model TEXT NOT NULL,
    inclusive_boundary INTEGER NOT NULL REFERENCES memory_review_turns(sequence),
    trigger TEXT NOT NULL CHECK (trigger IN ('cadence', 'manual', 'explicit_remember')),
    status TEXT NOT NULL DEFAULT 'queued'
        CHECK (status IN ('queued', 'running', 'succeeded', 'failed', 'interrupted', 'cancelled')),
    attempt INTEGER NOT NULL DEFAULT 0 CHECK (attempt >= 0),
    recovery_count INTEGER NOT NULL DEFAULT 0 CHECK (recovery_count >= 0),
    suppressed_turn_count INTEGER NOT NULL DEFAULT 0 CHECK (suppressed_turn_count >= 0),
    input_hash TEXT,
    turn_count INTEGER,
    proposal_count INTEGER,
    error_code TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK (
        (source_workspace_id IS NULL AND workspace_memory_generation IS NULL)
        OR (source_workspace_id IS NOT NULL AND workspace_memory_generation IS NOT NULL)
    )
);
CREATE UNIQUE INDEX idx_memory_job_active_branch
    ON memory_review_jobs(task_id, branch_id)
    WHERE status IN ('queued', 'running', 'failed', 'interrupted');
CREATE INDEX idx_memory_job_queue
    ON memory_review_jobs(status, sequence);

CREATE TABLE memory_candidates (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL CHECK (kind IN ('preference', 'constraint', 'convention', 'decision', 'pitfall')),
    operation TEXT NOT NULL CHECK (operation IN ('add', 'replace')),
    target_entry_id TEXT REFERENCES memory_entries(id) ON DELETE SET NULL,
    target_version INTEGER,
    source_task_id TEXT REFERENCES tasks(id) ON DELETE SET NULL,
    source_workspace_id TEXT REFERENCES workspaces(id) ON DELETE CASCADE,
    source_run_id TEXT REFERENCES agent_runs(id) ON DELETE SET NULL,
    captured_at TEXT NOT NULL,
    source_job_id TEXT REFERENCES memory_review_jobs(id) ON DELETE SET NULL,
    proposal_index INTEGER NOT NULL CHECK (proposal_index BETWEEN 0 AND 7),
    proposal_content TEXT,
    reason TEXT,
    proposal_hash TEXT NOT NULL,
    reason_hash TEXT NOT NULL,
    confidence REAL NOT NULL CHECK (confidence >= 0 AND confidence <= 1),
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK (status IN ('pending', 'approved', 'rejected', 'expired', 'superseded')),
    resolved_at TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    CHECK (
        (operation = 'add' AND target_entry_id IS NULL AND target_version IS NULL)
        OR (operation = 'replace' AND target_entry_id IS NOT NULL AND target_version IS NOT NULL)
    ),
    CHECK (
        (status = 'pending' AND proposal_content IS NOT NULL AND reason IS NOT NULL
            AND resolved_at IS NULL)
        OR (status != 'pending' AND proposal_content IS NULL AND reason IS NULL
            AND resolved_at IS NOT NULL)
    ),
    UNIQUE (source_job_id, proposal_index)
);
CREATE INDEX idx_memory_candidates_pending
    ON memory_candidates(status, sequence DESC);

CREATE TABLE memory_review_outcomes (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id TEXT NOT NULL REFERENCES memory_review_jobs(id) ON DELETE CASCADE,
    proposal_index INTEGER NOT NULL CHECK (proposal_index BETWEEN 0 AND 7),
    route TEXT NOT NULL CHECK (route IN ('global_candidate', 'project_entry', 'skipped')),
    result TEXT NOT NULL,
    entry_id TEXT REFERENCES memory_entries(id) ON DELETE SET NULL,
    candidate_id TEXT REFERENCES memory_candidates(id) ON DELETE SET NULL,
    error_code TEXT,
    created_at TEXT NOT NULL,
    UNIQUE (job_id, proposal_index)
);

CREATE TABLE memory_injections (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id TEXT NOT NULL UNIQUE REFERENCES agent_runs(id) ON DELETE CASCADE,
    engine TEXT NOT NULL CHECK (engine IN ('native', 'codex')),
    status TEXT NOT NULL CHECK (status IN ('recorded', 'aborted_before_publish')),
    snapshot_hash TEXT NOT NULL,
    global_refs_json TEXT NOT NULL,
    project_refs_json TEXT NOT NULL,
    global_chars INTEGER NOT NULL CHECK (global_chars >= 0 AND global_chars <= 4000),
    project_chars INTEGER NOT NULL CHECK (project_chars >= 0 AND project_chars <= 8000),
    created_at TEXT NOT NULL
);

ALTER TABLE notifications ADD COLUMN target_kind TEXT
    CHECK (target_kind IS NULL OR target_kind IN ('candidate', 'entry', 'job'));
ALTER TABLE notifications ADD COLUMN target_id TEXT;
ALTER TABLE notifications ADD COLUMN workspace_id TEXT REFERENCES workspaces(id) ON DELETE CASCADE;
CREATE INDEX idx_notifications_memory_target
    ON notifications(target_kind, target_id);
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier,
    };
    use std::{thread, time::Duration};

    fn assert_registry_rejected_without_mutation(migrations: &[MigrationSpec]) {
        let conn = Connection::open_in_memory().unwrap();
        assert!(run_migrations_with_specs(&conn, migrations, None).is_err());
        let object_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM sqlite_master", [], |row| row.get(0))
            .unwrap();
        assert_eq!(object_count, 0);
    }

    #[test]
    fn registry_gap_and_duplicate_fail_before_database_mutation() {
        const NOOP: &str = "SELECT 1;";
        assert_registry_rejected_without_mutation(&[
            MigrationSpec::new(1, NOOP, false),
            MigrationSpec::new(3, NOOP, false),
        ]);
        assert_registry_rejected_without_mutation(&[
            MigrationSpec::new(1, NOOP, false),
            MigrationSpec::new(1, NOOP, false),
        ]);
    }

    fn assert_fault_rolls_back_and_retries(fault_point: MigrationFaultPoint) {
        const SQL: &str = "CREATE TABLE migration_probe(value INTEGER NOT NULL);\
                           INSERT INTO migration_probe VALUES (1);\
                           UPDATE counter SET value = value + 1;";
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE counter(value INTEGER); INSERT INTO counter VALUES (0);")
            .unwrap();
        let migrations = [MigrationSpec::new(1, SQL, false)];
        let fault = |_, point| {
            if point == fault_point {
                return Err(ProductError::MigrationError("injected fault".into()));
            }
            Ok(())
        };

        assert!(run_migrations_with_specs(&conn, &migrations, Some(&fault)).is_err());
        let rolled_back: (i64, i64, i64) = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM sqlite_master WHERE name = 'migration_probe'),\
                        (SELECT value FROM counter),\
                        (SELECT COUNT(*) FROM schema_version)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(rolled_back, (0, 0, 0));

        run_migrations_with_specs(&conn, &migrations, None).unwrap();
        let committed: (i64, i64, i64) = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM migration_probe),\
                        (SELECT value FROM counter),\
                        (SELECT MAX(version) FROM schema_version)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(committed, (1, 1, 1));
    }

    #[test]
    fn migration_faults_rollback_ddl_data_and_version_then_retry() {
        for point in [
            MigrationFaultPoint::AfterSqlBeforeVersion,
            MigrationFaultPoint::BeforeCommit,
        ] {
            assert_fault_rolls_back_and_retries(point);
        }
    }

    fn v1_database_with_parent_and_children() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        run_migrations_with_specs(&conn, &MIGRATIONS[..1], None).unwrap();
        conn.execute_batch(
            "INSERT INTO tasks (id, project_id, title, goal, created_at, updated_at)\
             VALUES ('parent', '/workspace', 'Title', 'Goal', '2026-01-01', '2026-01-01');\
             INSERT INTO agent_runs (id, task_id, model, started_at)\
             VALUES ('child-run', 'parent', 'model', '2026-01-01');\
             INSERT INTO task_events (task_id, event_type, created_at)\
             VALUES ('parent', 'created', '2026-01-01');",
        )
        .unwrap();
        conn
    }

    fn schema_13_database_with_workspaces() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        run_migrations_with_specs(&conn, &MIGRATIONS[..13], None).unwrap();
        conn.execute_batch(
            "INSERT INTO workspaces \
                 (canonical_path, display_name, trust_state, last_opened_at, access_mode) \
             VALUES \
                 ('/Repo/Alpha', 'Alpha', 'untrusted', '2025-01-01T00:00:00Z', 'request_approval'), \
                 ('/repo/alpha', 'Beta', 'trusted', '2025-02-02T03:04:05Z', 'full_access'), \
                 ('/space path', 'Gamma', 'trusted', '2025-03-03T06:07:08Z', 'risk_based');",
        )
        .unwrap();
        conn
    }

    #[test]
    fn migration_v2_preserves_children_and_restores_foreign_keys() {
        let conn = v1_database_with_parent_and_children();
        let fault = |version, point| {
            if version == 2 && point == MigrationFaultPoint::BeforeCommit {
                return Err(ProductError::MigrationError("injected v2 fault".into()));
            }
            Ok(())
        };
        assert!(run_migrations_with_specs(&conn, &MIGRATIONS[..2], Some(&fault)).is_err());
        let rolled_back: (i64, i64, i64, i64) = conn
            .query_row(
                "SELECT (SELECT MAX(version) FROM schema_version),\
                        (SELECT COUNT(*) FROM pragma_table_info('tasks') WHERE name = 'project_id'),\
                        (SELECT COUNT(*) FROM agent_runs) + (SELECT COUNT(*) FROM task_events),\
                        (SELECT foreign_keys FROM pragma_foreign_keys)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(rolled_back, (1, 1, 2, 1));

        run_migrations(&conn).unwrap();
        let migrated: (String, i64, i64, i64) = conn
            .query_row(
                "SELECT workspace_path,\
                        (SELECT COUNT(*) FROM agent_runs) + (SELECT COUNT(*) FROM task_events),\
                        (SELECT COUNT(*) FROM pragma_foreign_key_check),\
                        (SELECT foreign_keys FROM pragma_foreign_keys)
                 FROM tasks WHERE id = 'parent'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(migrated, ("/workspace".into(), 2, 0, 1));
    }

    #[test]
    fn migration_v14_backfills_stable_unique_ids_and_preserves_schema_13_rows() {
        type WorkspaceRow = (String, String, String, String, String, String, i64);
        let conn = schema_13_database_with_workspaces();

        // This test owns the v13 -> v14 contract.  Keep it pinned to that boundary so
        // later feature migrations (for example v18 evolving memory) do not weaken or
        // accidentally redefine what v14 itself is required to do.
        run_migrations_with_specs(&conn, &MIGRATIONS[..14], None).unwrap();
        let rows: Vec<WorkspaceRow> = conn
            .prepare(
                "SELECT id, canonical_path, display_name, access_mode, last_opened_at, \
                        memory_mode, memory_generation \
                 FROM workspaces ORDER BY canonical_path",
            )
            .unwrap()
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();

        assert_eq!(rows.len(), 3);
        let ids: HashSet<&str> = rows.iter().map(|row| row.0.as_str()).collect();
        assert_eq!(ids.len(), rows.len());
        assert!(ids
            .iter()
            .all(|id| id.len() == 32 && id.chars().all(|c| c.is_ascii_hexdigit())));
        assert_eq!(
            rows.iter()
                .map(|row| {
                    (
                        row.1.as_str(),
                        row.2.as_str(),
                        row.3.as_str(),
                        row.4.as_str(),
                        row.5.as_str(),
                        row.6,
                    )
                })
                .collect::<Vec<_>>(),
            vec![
                (
                    "/Repo/Alpha",
                    "Alpha",
                    "request_approval",
                    "2025-01-01T00:00:00Z",
                    "inherit",
                    1,
                ),
                (
                    "/repo/alpha",
                    "Beta",
                    "full_access",
                    "2025-02-02T03:04:05Z",
                    "inherit",
                    1,
                ),
                (
                    "/space path",
                    "Gamma",
                    "risk_based",
                    "2025-03-03T06:07:08Z",
                    "inherit",
                    1,
                ),
            ]
        );
        let memory_tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type = 'table' AND (name LIKE 'memory_%' OR name LIKE '%memories%')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(memory_tables, 0);
        assert_eq!(
            conn.query_row("PRAGMA foreign_keys", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            1
        );

        let first_ids: Vec<String> = rows.into_iter().map(|row| row.0).collect();
        run_migrations(&conn).unwrap();
        let rerun_ids: Vec<String> = conn
            .prepare("SELECT id FROM workspaces ORDER BY canonical_path")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(rerun_ids, first_ids);
    }

    #[test]
    fn migration_v14_faults_restore_schema_13_and_foreign_keys_before_retry() {
        for fault_point in [
            MigrationFaultPoint::AfterSqlBeforeVersion,
            MigrationFaultPoint::BeforeCommit,
        ] {
            let conn = schema_13_database_with_workspaces();
            let fault = |version, point| {
                if version == 14 && point == fault_point {
                    return Err(ProductError::MigrationError("injected v14 fault".into()));
                }
                Ok(())
            };

            assert!(run_migrations_with_specs(&conn, MIGRATIONS, Some(&fault)).is_err());
            let rolled_back: (i64, i64, i64, i64, i64) = conn
                .query_row(
                    "SELECT (SELECT MAX(version) FROM schema_version), \
                            (SELECT COUNT(*) FROM workspaces), \
                            (SELECT COUNT(*) FROM pragma_table_info('workspaces') WHERE name = 'id'), \
                            (SELECT COUNT(*) FROM sqlite_master WHERE name = 'workspaces_v14'), \
                            (SELECT foreign_keys FROM pragma_foreign_keys)",
                    [],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    },
                )
                .unwrap();
            assert_eq!(rolled_back, (13, 3, 0, 0, 1));

            run_migrations(&conn).unwrap();
            let retried: (i64, i64, i64) = conn
                .query_row(
                    "SELECT (SELECT MAX(version) FROM schema_version), \
                            (SELECT COUNT(*) FROM workspaces), \
                            (SELECT foreign_keys FROM pragma_foreign_keys)",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(retried, (i64::from(LATEST_SCHEMA_VERSION), 3, 1));
        }
    }

    #[test]
    fn concurrent_connections_apply_non_idempotent_migration_once() {
        const BACKFILL: &str = "UPDATE counter SET value = value + 1;";
        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("concurrent.sqlite");
        Connection::open(&database)
            .unwrap()
            .execute_batch("CREATE TABLE counter(value INTEGER); INSERT INTO counter VALUES (0);")
            .unwrap();
        let barrier = Arc::new(Barrier::new(2));
        let applications = Arc::new(AtomicUsize::new(0));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let database = database.clone();
                let barrier = Arc::clone(&barrier);
                let applications = Arc::clone(&applications);
                thread::spawn(move || {
                    let conn = Connection::open(database).unwrap();
                    conn.busy_timeout(Duration::from_secs(10)).unwrap();
                    let hook = move |_, point| {
                        if point == MigrationFaultPoint::AfterSqlBeforeVersion {
                            applications.fetch_add(1, Ordering::SeqCst);
                        }
                        Ok(())
                    };
                    barrier.wait();
                    run_migrations_with_specs(
                        &conn,
                        &[MigrationSpec::new(1, BACKFILL, false)],
                        Some(&hook),
                    )
                    .unwrap();
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        let conn = Connection::open(database).unwrap();
        let committed: (i64, i64) = conn
            .query_row(
                "SELECT (SELECT value FROM counter), (SELECT COUNT(*) FROM schema_version)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(committed, (1, 1));
        assert_eq!(applications.load(Ordering::SeqCst), 1);
    }

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
            "review_sessions",
            "review_files",
            "review_items",
            "memory_settings",
            "memory_entries",
            "memory_entry_revisions",
            "memory_review_turns",
            "memory_review_jobs",
            "memory_candidates",
            "memory_review_outcomes",
            "memory_injections",
            "schema_version",
        ] {
            assert!(
                tables.contains(&expected.to_string()),
                "missing table: {expected}"
            );
        }
    }

    #[test]
    fn migration_repairs_v14_database_missing_notifications() {
        let conn = Connection::open_in_memory().unwrap();
        // Build an actual v14 database.  Rolling the version ledger backwards after
        // applying newer migrations leaves future tables behind and does not represent
        // any database an older R-Code release could have produced.
        run_migrations_with_specs(&conn, &MIGRATIONS[..14], None).unwrap();

        // Reproduce a legacy installation whose version ledger says v14 while
        // the notification schema created by v10 is physically absent.
        conn.execute_batch("DROP TABLE notifications;").unwrap();

        run_migrations(&conn).unwrap();

        let notification_objects: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE (type = 'table' AND name = 'notifications') \
                    OR (type = 'index' AND name IN ( \
                        'idx_notifications_unread', \
                        'idx_notifications_task' \
                    ))",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(notification_objects, 3);
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
