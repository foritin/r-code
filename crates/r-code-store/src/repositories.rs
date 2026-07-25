//! Repository 实现：对 SQLite 表的 CRUD 操作。
//!
//! 每个 repository 持有 `&Database` 引用，通过 r2d2 连接池执行查询。
//! DateTime 字段以 ISO 8601 / RFC 3339 字符串 (TEXT) 存储。
//! [doc-06 §3-8]

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use r_code_core::dto::{
    AgentRun, ReviewState, Task, TaskEvent, TaskEventType, TaskMode, TaskState, TrustState,
    Workspace,
};
use r_code_core::error::ProductError;
use rusqlite::params;

use crate::Database;

// ============================================================================
// 辅助函数
// ============================================================================

/// 将 rusqlite 错误转换为 ProductError。
fn db_err(e: rusqlite::Error) -> ProductError {
    ProductError::DatabaseError(e.to_string())
}

/// 解析 RFC 3339 时间戳为 `DateTime<Utc>`。
fn parse_ts(s: &str) -> Result<DateTime<Utc>, ProductError> {
    DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|e| ProductError::DatabaseError(format!("timestamp parse error: {e}")))
}

/// 解析 `TaskMode` 字符串。
fn parse_task_mode(s: &str) -> Result<TaskMode, ProductError> {
    match s {
        "ask" => Ok(TaskMode::Ask),
        "edit" => Ok(TaskMode::Edit),
        "auto" => Ok(TaskMode::Auto),
        _ => Err(ProductError::DatabaseError(format!(
            "invalid task mode: {s}"
        ))),
    }
}

/// 解析 `TaskEventType` 字符串。
fn parse_task_event_type(s: &str) -> Result<TaskEventType, ProductError> {
    match s {
        "task_created" => Ok(TaskEventType::TaskCreated),
        "state_changed" => Ok(TaskEventType::StateChanged),
        "run_started" => Ok(TaskEventType::RunStarted),
        "run_ended" => Ok(TaskEventType::RunEnded),
        "tool_call" => Ok(TaskEventType::ToolCall),
        "tool_result" => Ok(TaskEventType::ToolResult),
        "permission_requested" => Ok(TaskEventType::PermissionRequested),
        "permission_decided" => Ok(TaskEventType::PermissionDecided),
        "file_changed" => Ok(TaskEventType::FileChanged),
        "verification_run" => Ok(TaskEventType::VerificationRun),
        "system" => Ok(TaskEventType::System),
        _ => Err(ProductError::DatabaseError(format!(
            "invalid task event type: {s}"
        ))),
    }
}

/// 将 `TrustState` 转换为存储字符串。
fn trust_state_str(t: TrustState) -> &'static str {
    match t {
        TrustState::Untrusted => "untrusted",
        TrustState::Trusted => "trusted",
    }
}

/// 解析 `TrustState` 字符串。
fn parse_trust_state(s: &str) -> Result<TrustState, ProductError> {
    match s {
        "untrusted" => Ok(TrustState::Untrusted),
        "trusted" => Ok(TrustState::Trusted),
        _ => Err(ProductError::DatabaseError(format!(
            "invalid trust state: {s}"
        ))),
    }
}

/// 将数据库行映射为 `Task`。
///
/// 列顺序：id, project_id, title, goal, mode, state, worktree_path, created_at, updated_at
fn row_to_task(row: &rusqlite::Row<'_>) -> Result<Task, ProductError> {
    let mode_str: String = row.get(4).map_err(db_err)?;
    let mode = parse_task_mode(&mode_str)?;
    let state_str: String = row.get(5).map_err(db_err)?;
    let state = TaskState::try_from_str(&state_str)
        .ok_or_else(|| ProductError::DatabaseError(format!("invalid task state: {state_str}")))?;
    let created_str: String = row.get(7).map_err(db_err)?;
    let created_at = parse_ts(&created_str)?;
    let updated_str: String = row.get(8).map_err(db_err)?;
    let updated_at = parse_ts(&updated_str)?;

    Ok(Task {
        id: row.get(0).map_err(db_err)?,
        project_id: row.get(1).map_err(db_err)?,
        title: row.get(2).map_err(db_err)?,
        goal: row.get(3).map_err(db_err)?,
        mode,
        state,
        worktree_path: row.get(6).map_err(db_err)?,
        created_at,
        updated_at,
    })
}

/// 将数据库行映射为 `AgentRun`。
///
/// 列顺序：id, task_id, model, review_state, started_at, ended_at, usage_json
fn row_to_agent_run(row: &rusqlite::Row<'_>) -> Result<AgentRun, ProductError> {
    let review_state_str: String = row.get(3).map_err(db_err)?;
    let review_state = ReviewState::try_from_str(&review_state_str).ok_or_else(|| {
        ProductError::DatabaseError(format!("invalid review state: {review_state_str}"))
    })?;
    let started_str: String = row.get(4).map_err(db_err)?;
    let started_at = parse_ts(&started_str)?;
    let ended_str: Option<String> = row.get(5).map_err(db_err)?;
    let ended_at = match ended_str {
        Some(s) => Some(parse_ts(&s)?),
        None => None,
    };

    Ok(AgentRun {
        id: row.get(0).map_err(db_err)?,
        task_id: row.get(1).map_err(db_err)?,
        model: row.get(2).map_err(db_err)?,
        review_state,
        started_at,
        ended_at,
        usage_json: row.get(6).map_err(db_err)?,
    })
}

/// 将数据库行映射为 `Workspace`。
///
/// 列顺序：canonical_path, display_name, trust_state, last_opened_at
fn row_to_workspace(row: &rusqlite::Row<'_>) -> Result<Workspace, ProductError> {
    let trust_str: String = row.get(2).map_err(db_err)?;
    let trust_state = parse_trust_state(&trust_str)?;
    let last_opened_str: String = row.get(3).map_err(db_err)?;
    let last_opened_at = parse_ts(&last_opened_str)?;

    Ok(Workspace {
        canonical_path: row.get(0).map_err(db_err)?,
        display_name: row.get(1).map_err(db_err)?,
        trust_state,
        last_opened_at,
    })
}

// ============================================================================
// TaskRepository
// ============================================================================

/// 任务仓库 -- `tasks` 表的 CRUD。
pub struct TaskRepository<'a> {
    db: &'a Database,
}

impl<'a> TaskRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// 创建任务。
    pub fn create(&self, task: &Task) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT INTO tasks (id, project_id, title, goal, mode, state, worktree_path, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                task.id,
                task.project_id,
                task.title,
                task.goal,
                task.mode.to_string(),
                task.state.to_string(),
                task.worktree_path,
                task.created_at.to_rfc3339(),
                task.updated_at.to_rfc3339(),
            ],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 按 ID 获取任务。
    pub fn get(&self, id: &str) -> Result<Option<Task>, ProductError> {
        let conn = self.db.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, project_id, title, goal, mode, state, worktree_path, created_at, updated_at \
                 FROM tasks WHERE id = ?1",
            )
            .map_err(db_err)?;
        let mut rows = stmt.query(params![id]).map_err(db_err)?;
        match rows.next().map_err(db_err)? {
            Some(row) => Ok(Some(row_to_task(row)?)),
            None => Ok(None),
        }
    }

    /// 列出任务，支持按项目/状态过滤。
    pub fn list(
        &self,
        project_id: Option<&str>,
        state: Option<TaskState>,
        include_archived: bool,
    ) -> Result<Vec<Task>, ProductError> {
        let conn = self.db.conn()?;
        let mut sql = String::from(
            "SELECT id, project_id, title, goal, mode, state, worktree_path, created_at, updated_at \
             FROM tasks WHERE 1=1",
        );
        let mut param_values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(pid) = project_id {
            sql.push_str(" AND project_id = ?");
            param_values.push(Box::new(pid.to_string()));
        }
        if let Some(st) = state {
            sql.push_str(" AND state = ?");
            param_values.push(Box::new(st.to_string()));
        }
        if !include_archived {
            sql.push_str(" AND state != 'archived'");
        }
        sql.push_str(" ORDER BY created_at DESC");

        let params_refs: Vec<&dyn rusqlite::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();
        let mut stmt = conn.prepare(&sql).map_err(db_err)?;
        let mut rows = stmt.query(params_refs.as_slice()).map_err(db_err)?;
        let mut tasks = Vec::new();
        while let Some(row) = rows.next().map_err(db_err)? {
            tasks.push(row_to_task(row)?);
        }
        Ok(tasks)
    }

    /// 更新任务状态（同时更新 `updated_at`）。
    pub fn update_state(&self, id: &str, new_state: TaskState) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE tasks SET state = ?1, updated_at = ?2 WHERE id = ?3",
            params![new_state.to_string(), Utc::now().to_rfc3339(), id],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 部分更新任务字段（`None` 表示不修改该字段）。
    pub fn update(
        &self,
        id: &str,
        title: Option<&str>,
        goal: Option<&str>,
        worktree_path: Option<&str>,
    ) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        let mut sets: Vec<&str> = Vec::new();
        let mut param_values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(t) = title {
            sets.push("title = ?");
            param_values.push(Box::new(t.to_string()));
        }
        if let Some(g) = goal {
            sets.push("goal = ?");
            param_values.push(Box::new(g.to_string()));
        }
        if let Some(w) = worktree_path {
            sets.push("worktree_path = ?");
            param_values.push(Box::new(w.to_string()));
        }

        if sets.is_empty() {
            return Ok(());
        }

        sets.push("updated_at = ?");
        param_values.push(Box::new(Utc::now().to_rfc3339()));
        param_values.push(Box::new(id.to_string()));

        let sql = format!("UPDATE tasks SET {} WHERE id = ?", sets.join(", "));
        let params_refs: Vec<&dyn rusqlite::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();
        conn.execute(&sql, params_refs.as_slice()).map_err(db_err)?;
        Ok(())
    }
}

// ============================================================================
// AgentRunRepository
// ============================================================================

/// Agent Run 仓库 -- `agent_runs` 表的 CRUD。
pub struct AgentRunRepository<'a> {
    db: &'a Database,
}

impl<'a> AgentRunRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// 创建 Agent Run 记录。
    pub fn create(&self, run: &AgentRun) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT INTO agent_runs (id, task_id, model, review_state, started_at, ended_at, usage_json) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                run.id,
                run.task_id,
                run.model,
                run.review_state.to_string(),
                run.started_at.to_rfc3339(),
                run.ended_at.map(|dt| dt.to_rfc3339()),
                run.usage_json,
            ],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 按 ID 获取 Agent Run。
    pub fn get(&self, id: &str) -> Result<Option<AgentRun>, ProductError> {
        let conn = self.db.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, model, review_state, started_at, ended_at, usage_json \
                 FROM agent_runs WHERE id = ?1",
            )
            .map_err(db_err)?;
        let mut rows = stmt.query(params![id]).map_err(db_err)?;
        match rows.next().map_err(db_err)? {
            Some(row) => Ok(Some(row_to_agent_run(row)?)),
            None => Ok(None),
        }
    }

    /// 列出某 Task 的所有 Agent Run（按开始时间降序）。
    pub fn list_by_task(&self, task_id: &str) -> Result<Vec<AgentRun>, ProductError> {
        let conn = self.db.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, model, review_state, started_at, ended_at, usage_json \
                 FROM agent_runs WHERE task_id = ?1 ORDER BY started_at DESC",
            )
            .map_err(db_err)?;
        let mut rows = stmt.query(params![task_id]).map_err(db_err)?;
        let mut runs = Vec::new();
        while let Some(row) = rows.next().map_err(db_err)? {
            runs.push(row_to_agent_run(row)?);
        }
        Ok(runs)
    }

    /// 获取 Task 的活跃 Run（`ended_at IS NULL`）。
    pub fn get_active_run(&self, task_id: &str) -> Result<Option<AgentRun>, ProductError> {
        let conn = self.db.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, model, review_state, started_at, ended_at, usage_json \
                 FROM agent_runs WHERE task_id = ?1 AND ended_at IS NULL \
                 ORDER BY started_at DESC LIMIT 1",
            )
            .map_err(db_err)?;
        let mut rows = stmt.query(params![task_id]).map_err(db_err)?;
        match rows.next().map_err(db_err)? {
            Some(row) => Ok(Some(row_to_agent_run(row)?)),
            None => Ok(None),
        }
    }

    /// 更新审查状态并设置 `ended_at`（标记 Run 结束）。
    pub fn update_review_state(
        &self,
        id: &str,
        review_state: ReviewState,
    ) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE agent_runs SET review_state = ?1, ended_at = ?2 WHERE id = ?3",
            params![review_state.to_string(), Utc::now().to_rfc3339(), id],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 设置 Token 用量 JSON。
    pub fn set_usage(&self, id: &str, usage_json: &str) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE agent_runs SET usage_json = ?1 WHERE id = ?2",
            params![usage_json, id],
        )
        .map_err(db_err)?;
        Ok(())
    }
}

// ============================================================================
// TaskEventStore
// ============================================================================

/// 任务事件存储 -- `task_events` 表的轻量投影。
pub struct TaskEventStore<'a> {
    db: &'a Database,
}

impl<'a> TaskEventStore<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// 追加事件，返回自增 ID。
    pub fn append(&self, task_id: &str, event_type: TaskEventType) -> Result<i64, ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT INTO task_events (task_id, event_type, created_at) VALUES (?1, ?2, ?3)",
            params![task_id, event_type.to_string(), Utc::now().to_rfc3339()],
        )
        .map_err(db_err)?;
        Ok(conn.last_insert_rowid())
    }

    /// 列出某 Task 的事件（按 `created_at` ASC，同时间按 `id` ASC）。
    pub fn list_by_task(
        &self,
        task_id: &str,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<Vec<TaskEvent>, ProductError> {
        let conn = self.db.conn()?;
        let limit_val = limit.unwrap_or(100) as i64;
        let offset_val = offset.unwrap_or(0) as i64;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, event_type, created_at FROM task_events \
                 WHERE task_id = ?1 ORDER BY created_at ASC, id ASC LIMIT ?2 OFFSET ?3",
            )
            .map_err(db_err)?;
        let mut rows = stmt
            .query(params![task_id, limit_val, offset_val])
            .map_err(db_err)?;
        let mut events = Vec::new();
        while let Some(row) = rows.next().map_err(db_err)? {
            let event_type_str: String = row.get(2).map_err(db_err)?;
            let event_type = parse_task_event_type(&event_type_str)?;
            let created_str: String = row.get(3).map_err(db_err)?;
            let created_at = parse_ts(&created_str)?;
            events.push(TaskEvent {
                id: row.get(0).map_err(db_err)?,
                task_id: row.get(1).map_err(db_err)?,
                event_type,
                created_at,
            });
        }
        Ok(events)
    }
}

// ============================================================================
// BlobStore
// ============================================================================

/// Blob 存储 -- 内容寻址去重。
///
/// 文件存储在磁盘 `blobs_dir`，引用计数在 SQLite `blobs` 表。
pub struct BlobStore<'a> {
    db: &'a Database,
    blobs_dir: PathBuf,
}

impl<'a> BlobStore<'a> {
    pub fn new(db: &'a Database, blobs_dir: PathBuf) -> Self {
        Self { db, blobs_dir }
    }

    /// 写入内容，返回 blake3 哈希。相同内容去重（不覆盖已存在的文件）。
    pub fn put(&self, content: &[u8]) -> Result<String, ProductError> {
        let hash = blake3::hash(content).to_hex().to_string();
        let blob_path = self.blobs_dir.join(&hash);
        if !blob_path.exists() {
            std::fs::create_dir_all(&self.blobs_dir)?;
            std::fs::write(&blob_path, content)?;
        }
        Ok(hash)
    }

    /// 读取 blob 内容。文件不存在返回 `None`。
    pub fn get(&self, hash: &str) -> Result<Option<Vec<u8>>, ProductError> {
        let blob_path = self.blobs_dir.join(hash);
        if !blob_path.exists() {
            return Ok(None);
        }
        let content = std::fs::read(&blob_path)?;
        Ok(Some(content))
    }

    /// 在 `blobs` 表中递增引用计数（不存在则插入）。
    pub fn increment_ref(&self, hash: &str) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT INTO blobs (hash, ref_count, created_at) VALUES (?1, 1, ?2) \
             ON CONFLICT(hash) DO UPDATE SET ref_count = ref_count + 1",
            params![hash, Utc::now().to_rfc3339()],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 递减引用计数。当计数降为 0 时删除 DB 记录和磁盘文件。
    pub fn decrement_ref(&self, hash: &str) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        let ref_count: Option<i64> = conn
            .query_row(
                "SELECT ref_count FROM blobs WHERE hash = ?1",
                params![hash],
                |row| row.get(0),
            )
            .ok();

        match ref_count {
            Some(count) if count <= 1 => {
                conn.execute("DELETE FROM blobs WHERE hash = ?1", params![hash])
                    .map_err(db_err)?;
                let blob_path = self.blobs_dir.join(hash);
                if blob_path.exists() {
                    let _ = std::fs::remove_file(&blob_path);
                }
            }
            Some(_) => {
                conn.execute(
                    "UPDATE blobs SET ref_count = ref_count - 1 WHERE hash = ?1",
                    params![hash],
                )
                .map_err(db_err)?;
            }
            None => {
                // blob 不在 DB 中，无需操作
            }
        }
        Ok(())
    }
}

// ============================================================================
// WorkspaceRepository
// ============================================================================

/// Workspace 仓库 -- `workspaces` 表的管理。
pub struct WorkspaceRepository<'a> {
    db: &'a Database,
}

impl<'a> WorkspaceRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// 插入或替换 Workspace。
    pub fn upsert(&self, ws: &Workspace) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO workspaces (canonical_path, display_name, trust_state, last_opened_at) \
             VALUES (?1, ?2, ?3, ?4)",
            params![
                ws.canonical_path,
                ws.display_name,
                trust_state_str(ws.trust_state),
                ws.last_opened_at.to_rfc3339(),
            ],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 按 canonical path 获取 Workspace。
    pub fn get(&self, canonical_path: &str) -> Result<Option<Workspace>, ProductError> {
        let conn = self.db.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT canonical_path, display_name, trust_state, last_opened_at \
                 FROM workspaces WHERE canonical_path = ?1",
            )
            .map_err(db_err)?;
        let mut rows = stmt.query(params![canonical_path]).map_err(db_err)?;
        match rows.next().map_err(db_err)? {
            Some(row) => Ok(Some(row_to_workspace(row)?)),
            None => Ok(None),
        }
    }

    /// 列出最近打开的 Workspace（按 `last_opened_at` 降序）。
    pub fn list_recent(&self, limit: u32) -> Result<Vec<Workspace>, ProductError> {
        let conn = self.db.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT canonical_path, display_name, trust_state, last_opened_at \
                 FROM workspaces ORDER BY last_opened_at DESC LIMIT ?1",
            )
            .map_err(db_err)?;
        let mut rows = stmt.query(params![limit as i64]).map_err(db_err)?;
        let mut workspaces = Vec::new();
        while let Some(row) = rows.next().map_err(db_err)? {
            workspaces.push(row_to_workspace(row)?);
        }
        Ok(workspaces)
    }

    /// 更新信任状态。
    pub fn update_trust(
        &self,
        canonical_path: &str,
        trust: TrustState,
    ) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE workspaces SET trust_state = ?1 WHERE canonical_path = ?2",
            params![trust_state_str(trust), canonical_path],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 更新最后打开时间。
    pub fn touch(&self, canonical_path: &str) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE workspaces SET last_opened_at = ?1 WHERE canonical_path = ?2",
            params![Utc::now().to_rfc3339(), canonical_path],
        )
        .map_err(db_err)?;
        Ok(())
    }
}

// ============================================================================
// 测试
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use r_code_core::dto::{ReviewState, TaskEventType, TaskMode, TaskState, TrustState};
    use tempfile::TempDir;

    /// 创建内存数据库用于测试。
    fn setup_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    /// 创建测试任务并持久化。
    fn create_test_task(db: &Database, project_id: &str, title: &str) -> Task {
        let repo = TaskRepository::new(db);
        let task = Task::new(project_id, title, "test goal", TaskMode::Edit);
        repo.create(&task).unwrap();
        task
    }

    // --------------------------------------------------------------------------
    // TaskRepository 测试
    // --------------------------------------------------------------------------

    #[test]
    fn test_task_create_and_get() {
        let db = setup_db();
        let repo = TaskRepository::new(&db);
        let task = Task::new("/proj", "My Task", "Do stuff", TaskMode::Auto);
        repo.create(&task).unwrap();

        let fetched = repo.get(&task.id).unwrap().unwrap();
        assert_eq!(fetched.id, task.id);
        assert_eq!(fetched.project_id, "/proj");
        assert_eq!(fetched.title, "My Task");
        assert_eq!(fetched.goal, "Do stuff");
        assert_eq!(fetched.mode, TaskMode::Auto);
        assert_eq!(fetched.state, TaskState::Idle);
        assert!(fetched.worktree_path.is_none());
        assert_eq!(fetched.created_at, task.created_at);
        assert_eq!(fetched.updated_at, task.updated_at);
    }

    #[test]
    fn test_task_get_not_found() {
        let db = setup_db();
        let repo = TaskRepository::new(&db);
        assert!(repo.get("nonexistent").unwrap().is_none());
    }

    #[test]
    fn test_task_list_by_project() {
        let db = setup_db();
        let repo = TaskRepository::new(&db);

        create_test_task(&db, "/proj-a", "Task A1");
        create_test_task(&db, "/proj-a", "Task A2");
        create_test_task(&db, "/proj-b", "Task B1");

        let proj_a = repo.list(Some("/proj-a"), None, true).unwrap();
        assert_eq!(proj_a.len(), 2);
        assert!(proj_a.iter().all(|t| t.project_id == "/proj-a"));

        let proj_b = repo.list(Some("/proj-b"), None, true).unwrap();
        assert_eq!(proj_b.len(), 1);

        let all = repo.list(None, None, true).unwrap();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn test_task_list_by_state() {
        let db = setup_db();
        let repo = TaskRepository::new(&db);

        let t1 = create_test_task(&db, "/proj", "T1");
        let t2 = create_test_task(&db, "/proj", "T2");
        repo.update_state(&t2.id, TaskState::InProgress).unwrap();

        let idle = repo.list(None, Some(TaskState::Idle), true).unwrap();
        assert_eq!(idle.len(), 1);
        assert_eq!(idle[0].id, t1.id);

        let in_progress = repo.list(None, Some(TaskState::InProgress), true).unwrap();
        assert_eq!(in_progress.len(), 1);
        assert_eq!(in_progress[0].id, t2.id);
    }

    #[test]
    fn test_task_list_exclude_archived() {
        let db = setup_db();
        let repo = TaskRepository::new(&db);

        let t1 = create_test_task(&db, "/proj", "Active");
        let t2 = create_test_task(&db, "/proj", "Archived");
        repo.update_state(&t2.id, TaskState::Archived).unwrap();

        let visible = repo.list(None, None, false).unwrap();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, t1.id);

        let all = repo.list(None, None, true).unwrap();
        assert_eq!(all.len(), 2);
    }

    #[test]
    fn test_task_update_state() {
        let db = setup_db();
        let repo = TaskRepository::new(&db);
        let task = create_test_task(&db, "/proj", "State Test");

        repo.update_state(&task.id, TaskState::Exploring).unwrap();
        let fetched = repo.get(&task.id).unwrap().unwrap();
        assert_eq!(fetched.state, TaskState::Exploring);
        assert!(fetched.updated_at >= task.updated_at);
    }

    #[test]
    fn test_task_update_fields() {
        let db = setup_db();
        let repo = TaskRepository::new(&db);
        let task = create_test_task(&db, "/proj", "Original");

        repo.update(
            &task.id,
            Some("Updated Title"),
            Some("Updated Goal"),
            Some("/worktree/path"),
        )
        .unwrap();
        let fetched = repo.get(&task.id).unwrap().unwrap();
        assert_eq!(fetched.title, "Updated Title");
        assert_eq!(fetched.goal, "Updated Goal");
        assert_eq!(fetched.worktree_path.as_deref(), Some("/worktree/path"));
    }

    #[test]
    fn test_task_update_partial() {
        let db = setup_db();
        let repo = TaskRepository::new(&db);
        let task = create_test_task(&db, "/proj", "Partial");

        repo.update(&task.id, Some("New Title"), None, None)
            .unwrap();
        let fetched = repo.get(&task.id).unwrap().unwrap();
        assert_eq!(fetched.title, "New Title");
        assert_eq!(fetched.goal, "test goal");
    }

    #[test]
    fn test_task_update_nothing() {
        let db = setup_db();
        let repo = TaskRepository::new(&db);
        let task = create_test_task(&db, "/proj", "No Change");

        repo.update(&task.id, None, None, None).unwrap();
        let fetched = repo.get(&task.id).unwrap().unwrap();
        assert_eq!(fetched.title, "No Change");
    }

    // --------------------------------------------------------------------------
    // AgentRunRepository 测试
    // --------------------------------------------------------------------------

    #[test]
    fn test_agent_run_create_and_get() {
        let db = setup_db();
        let task = create_test_task(&db, "/proj", "Run Task");
        let repo = AgentRunRepository::new(&db);

        let run = AgentRun::new(&task.id, "gpt-4");
        repo.create(&run).unwrap();

        let fetched = repo.get(&run.id).unwrap().unwrap();
        assert_eq!(fetched.id, run.id);
        assert_eq!(fetched.task_id, task.id);
        assert_eq!(fetched.model, "gpt-4");
        assert_eq!(fetched.review_state, ReviewState::Pending);
        assert!(fetched.ended_at.is_none());
        assert!(fetched.usage_json.is_none());
        assert_eq!(fetched.started_at, run.started_at);
    }

    #[test]
    fn test_agent_run_get_not_found() {
        let db = setup_db();
        let repo = AgentRunRepository::new(&db);
        assert!(repo.get("nonexistent").unwrap().is_none());
    }

    #[test]
    fn test_agent_run_list_by_task() {
        let db = setup_db();
        let task = create_test_task(&db, "/proj", "Multi Run");
        let repo = AgentRunRepository::new(&db);

        let run1 = AgentRun::new(&task.id, "gpt-4");
        repo.create(&run1).unwrap();
        let run2 = AgentRun::new(&task.id, "claude-3");
        repo.create(&run2).unwrap();

        let runs = repo.list_by_task(&task.id).unwrap();
        assert_eq!(runs.len(), 2);
    }

    #[test]
    fn test_agent_run_list_by_task_empty() {
        let db = setup_db();
        let task = create_test_task(&db, "/proj", "Empty Runs");
        let repo = AgentRunRepository::new(&db);
        let runs = repo.list_by_task(&task.id).unwrap();
        assert!(runs.is_empty());
    }

    #[test]
    fn test_agent_run_get_active() {
        let db = setup_db();
        let task = create_test_task(&db, "/proj", "Active Run");
        let repo = AgentRunRepository::new(&db);

        let active = AgentRun::new(&task.id, "gpt-4");
        repo.create(&active).unwrap();

        let found = repo.get_active_run(&task.id).unwrap().unwrap();
        assert_eq!(found.id, active.id);
        assert!(found.is_active());
    }

    #[test]
    fn test_agent_run_get_active_none_when_finished() {
        let db = setup_db();
        let task = create_test_task(&db, "/proj", "Finished Run");
        let repo = AgentRunRepository::new(&db);

        let run = AgentRun::new(&task.id, "gpt-4");
        repo.create(&run).unwrap();
        repo.update_review_state(&run.id, ReviewState::Accepted)
            .unwrap();

        assert!(repo.get_active_run(&task.id).unwrap().is_none());
    }

    #[test]
    fn test_agent_run_update_review_state() {
        let db = setup_db();
        let task = create_test_task(&db, "/proj", "Review Test");
        let repo = AgentRunRepository::new(&db);

        let run = AgentRun::new(&task.id, "gpt-4");
        repo.create(&run).unwrap();

        repo.update_review_state(&run.id, ReviewState::Accepted)
            .unwrap();
        let fetched = repo.get(&run.id).unwrap().unwrap();
        assert_eq!(fetched.review_state, ReviewState::Accepted);
        assert!(fetched.ended_at.is_some());
    }

    #[test]
    fn test_agent_run_set_usage() {
        let db = setup_db();
        let task = create_test_task(&db, "/proj", "Usage Test");
        let repo = AgentRunRepository::new(&db);

        let run = AgentRun::new(&task.id, "gpt-4");
        repo.create(&run).unwrap();

        let usage = r#"{"input_tokens":100,"output_tokens":50}"#;
        repo.set_usage(&run.id, usage).unwrap();
        let fetched = repo.get(&run.id).unwrap().unwrap();
        assert_eq!(fetched.usage_json.as_deref(), Some(usage));
    }

    // --------------------------------------------------------------------------
    // TaskEventStore 测试
    // --------------------------------------------------------------------------

    #[test]
    fn test_event_append_and_list() {
        let db = setup_db();
        let task = create_test_task(&db, "/proj", "Event Task");
        let store = TaskEventStore::new(&db);

        let id1 = store.append(&task.id, TaskEventType::TaskCreated).unwrap();
        let id2 = store.append(&task.id, TaskEventType::RunStarted).unwrap();
        let id3 = store.append(&task.id, TaskEventType::RunEnded).unwrap();

        assert!(id1 < id2);
        assert!(id2 < id3);

        let events = store.list_by_task(&task.id, None, None).unwrap();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].event_type, TaskEventType::TaskCreated);
        assert_eq!(events[1].event_type, TaskEventType::RunStarted);
        assert_eq!(events[2].event_type, TaskEventType::RunEnded);
    }

    #[test]
    fn test_event_list_with_limit_offset() {
        let db = setup_db();
        let task = create_test_task(&db, "/proj", "Paginated Events");
        let store = TaskEventStore::new(&db);

        for _ in 0..5 {
            store.append(&task.id, TaskEventType::ToolCall).unwrap();
        }

        let page1 = store.list_by_task(&task.id, Some(2), Some(0)).unwrap();
        assert_eq!(page1.len(), 2);

        let page2 = store.list_by_task(&task.id, Some(2), Some(2)).unwrap();
        assert_eq!(page2.len(), 2);

        let page3 = store.list_by_task(&task.id, Some(2), Some(4)).unwrap();
        assert_eq!(page3.len(), 1);

        // 确保分页顺序正确（id 递增）
        assert!(page1[0].id < page1[1].id);
        assert!(page1[1].id < page2[0].id);
        assert!(page2[1].id < page3[0].id);
    }

    #[test]
    fn test_event_list_empty() {
        let db = setup_db();
        let task = create_test_task(&db, "/proj", "No Events");
        let store = TaskEventStore::new(&db);
        let events = store.list_by_task(&task.id, None, None).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn test_event_all_types_roundtrip() {
        let db = setup_db();
        let task = create_test_task(&db, "/proj", "All Event Types");
        let store = TaskEventStore::new(&db);

        let all_types = [
            TaskEventType::TaskCreated,
            TaskEventType::StateChanged,
            TaskEventType::RunStarted,
            TaskEventType::RunEnded,
            TaskEventType::ToolCall,
            TaskEventType::ToolResult,
            TaskEventType::PermissionRequested,
            TaskEventType::PermissionDecided,
            TaskEventType::FileChanged,
            TaskEventType::VerificationRun,
            TaskEventType::System,
        ];

        for et in all_types {
            store.append(&task.id, et).unwrap();
        }

        let events = store.list_by_task(&task.id, None, None).unwrap();
        assert_eq!(events.len(), all_types.len());
        for (i, et) in all_types.iter().enumerate() {
            assert_eq!(events[i].event_type, *et);
        }
    }

    // --------------------------------------------------------------------------
    // BlobStore 测试
    // --------------------------------------------------------------------------

    #[test]
    fn test_blob_put_and_get() {
        let db = setup_db();
        let tmp = TempDir::new().unwrap();
        let store = BlobStore::new(&db, tmp.path().to_path_buf());

        let content = b"hello world";
        let hash = store.put(content).unwrap();
        assert!(!hash.is_empty());

        let fetched = store.get(&hash).unwrap().unwrap();
        assert_eq!(fetched, content);
    }

    #[test]
    fn test_blob_dedup() {
        let db = setup_db();
        let tmp = TempDir::new().unwrap();
        let store = BlobStore::new(&db, tmp.path().to_path_buf());

        let content = b"dedup content";
        let hash1 = store.put(content).unwrap();
        let hash2 = store.put(content).unwrap();
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_blob_get_not_found() {
        let db = setup_db();
        let tmp = TempDir::new().unwrap();
        let store = BlobStore::new(&db, tmp.path().to_path_buf());

        assert!(store.get("nonexistent_hash").unwrap().is_none());
    }

    #[test]
    fn test_blob_different_content_different_hash() {
        let db = setup_db();
        let tmp = TempDir::new().unwrap();
        let store = BlobStore::new(&db, tmp.path().to_path_buf());

        let hash1 = store.put(b"content A").unwrap();
        let hash2 = store.put(b"content B").unwrap();
        assert_ne!(hash1, hash2);
    }

    #[test]
    fn test_blob_increment_ref() {
        let db = setup_db();
        let tmp = TempDir::new().unwrap();
        let store = BlobStore::new(&db, tmp.path().to_path_buf());

        let hash = store.put(b"ref test").unwrap();

        store.increment_ref(&hash).unwrap();
        store.increment_ref(&hash).unwrap();

        // 验证 ref_count = 2（通过递减逻辑间接验证）
        let conn = db.conn().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT ref_count FROM blobs WHERE hash = ?1",
                params![hash],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn test_blob_decrement_ref_keeps_when_above_zero() {
        let db = setup_db();
        let tmp = TempDir::new().unwrap();
        let store = BlobStore::new(&db, tmp.path().to_path_buf());

        let hash = store.put(b"keep me").unwrap();
        store.increment_ref(&hash).unwrap();
        store.increment_ref(&hash).unwrap();

        // ref_count = 2，递减后应为 1，文件仍存在
        store.decrement_ref(&hash).unwrap();
        assert!(store.get(&hash).unwrap().is_some());

        let conn = db.conn().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT ref_count FROM blobs WHERE hash = ?1",
                params![hash],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn test_blob_decrement_to_zero_deletes() {
        let db = setup_db();
        let tmp = TempDir::new().unwrap();
        let store = BlobStore::new(&db, tmp.path().to_path_buf());

        let hash = store.put(b"delete me").unwrap();
        store.increment_ref(&hash).unwrap();

        // ref_count = 1，递减后为 0，应删除文件和 DB 记录
        store.decrement_ref(&hash).unwrap();
        assert!(store.get(&hash).unwrap().is_none());

        let conn = db.conn().unwrap();
        let exists: bool = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM blobs WHERE hash = ?1)",
                params![hash],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!exists);
    }

    #[test]
    fn test_blob_decrement_nonexistent_is_noop() {
        let db = setup_db();
        let tmp = TempDir::new().unwrap();
        let store = BlobStore::new(&db, tmp.path().to_path_buf());

        // 不存在的 hash，递减应成功但不做任何事
        store.decrement_ref("nonexistent").unwrap();
    }

    // --------------------------------------------------------------------------
    // WorkspaceRepository 测试
    // --------------------------------------------------------------------------

    #[test]
    fn test_workspace_upsert_and_get() {
        let db = setup_db();
        let repo = WorkspaceRepository::new(&db);

        let ws = Workspace::new("/home/user/project", "My Project");
        repo.upsert(&ws).unwrap();

        let fetched = repo.get("/home/user/project").unwrap().unwrap();
        assert_eq!(fetched.canonical_path, "/home/user/project");
        assert_eq!(fetched.display_name, "My Project");
        assert_eq!(fetched.trust_state, TrustState::Untrusted);
        assert_eq!(fetched.last_opened_at, ws.last_opened_at);
    }

    #[test]
    fn test_workspace_upsert_replaces() {
        let db = setup_db();
        let repo = WorkspaceRepository::new(&db);

        let ws = Workspace::new("/proj", "Original");
        repo.upsert(&ws).unwrap();

        let updated = Workspace {
            canonical_path: "/proj".to_string(),
            display_name: "Updated Name".to_string(),
            trust_state: TrustState::Trusted,
            last_opened_at: Utc::now(),
        };
        repo.upsert(&updated).unwrap();

        let fetched = repo.get("/proj").unwrap().unwrap();
        assert_eq!(fetched.display_name, "Updated Name");
        assert_eq!(fetched.trust_state, TrustState::Trusted);
    }

    #[test]
    fn test_workspace_get_not_found() {
        let db = setup_db();
        let repo = WorkspaceRepository::new(&db);
        assert!(repo.get("/nonexistent").unwrap().is_none());
    }

    #[test]
    fn test_workspace_list_recent() {
        let db = setup_db();
        let repo = WorkspaceRepository::new(&db);

        let ws1 = Workspace::new("/proj1", "Project 1");
        repo.upsert(&ws1).unwrap();
        let ws2 = Workspace::new("/proj2", "Project 2");
        repo.upsert(&ws2).unwrap();
        let ws3 = Workspace::new("/proj3", "Project 3");
        repo.upsert(&ws3).unwrap();

        let recent = repo.list_recent(10).unwrap();
        assert_eq!(recent.len(), 3);
    }

    #[test]
    fn test_workspace_list_recent_limit() {
        let db = setup_db();
        let repo = WorkspaceRepository::new(&db);

        for i in 0..5 {
            let ws = Workspace::new(format!("/proj{i}"), format!("Project {i}"));
            repo.upsert(&ws).unwrap();
        }

        let recent = repo.list_recent(3).unwrap();
        assert_eq!(recent.len(), 3);
    }

    #[test]
    fn test_workspace_update_trust() {
        let db = setup_db();
        let repo = WorkspaceRepository::new(&db);

        let ws = Workspace::new("/proj", "Trust Test");
        repo.upsert(&ws).unwrap();

        repo.update_trust("/proj", TrustState::Trusted).unwrap();
        let fetched = repo.get("/proj").unwrap().unwrap();
        assert_eq!(fetched.trust_state, TrustState::Trusted);
    }

    #[test]
    fn test_workspace_touch() {
        let db = setup_db();
        let repo = WorkspaceRepository::new(&db);

        let ws = Workspace::new("/proj", "Touch Test");
        repo.upsert(&ws).unwrap();

        let original = repo.get("/proj").unwrap().unwrap();
        repo.touch("/proj").unwrap();
        let fetched = repo.get("/proj").unwrap().unwrap();
        assert!(fetched.last_opened_at >= original.last_opened_at);
    }

    #[test]
    fn test_workspace_list_recent_ordered_by_last_opened() {
        let db = setup_db();
        let repo = WorkspaceRepository::new(&db);

        let ws1 = Workspace::new("/old", "Old");
        repo.upsert(&ws1).unwrap();
        let ws2 = Workspace::new("/new", "New");
        repo.upsert(&ws2).unwrap();

        // touch /old 使其 last_opened_at 更新
        repo.touch("/old").unwrap();

        let recent = repo.list_recent(10).unwrap();
        assert_eq!(recent.len(), 2);
        // /old 被最后 touch，应排在第一位
        assert_eq!(recent[0].canonical_path, "/old");
    }
}
