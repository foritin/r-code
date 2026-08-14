//! Repository 实现：对 SQLite 表的 CRUD 操作。
//!
//! 每个 repository 持有 `&Database` 引用，通过 r2d2 连接池执行查询。
//! DateTime 字段以 ISO 8601 / RFC 3339 字符串 (TEXT) 存储。
//! [doc-06 §3-8]

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use r_code_core::dto::{
    AgentEngine, AgentKind, AgentRun, AgentRunRuntimeKind, Notification, NotificationKind,
    ProjectAccessMode, QueuedMessage, QueuedMessageState, ReviewState, SessionBranch,
    SubagentAccessMode, Task, TaskEvent, TaskEventType, TaskMode, TaskState, ToolCall,
    ToolCallStatus, Workspace, WorkspaceMemoryMode,
};
use r_code_core::error::ProductError;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use uuid::Uuid;

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
    TaskMode::try_from_str(s)
        .ok_or_else(|| ProductError::DatabaseError(format!("invalid task mode: {s}")))
}

/// 解析 Agent Run 角色字符串。
fn parse_agent_kind(s: &str) -> Result<AgentKind, ProductError> {
    AgentKind::try_from_str(s)
        .ok_or_else(|| ProductError::DatabaseError(format!("invalid agent kind: {s}")))
}

/// 解析 `TaskEventType` 字符串。
fn parse_task_event_type(s: &str) -> Result<TaskEventType, ProductError> {
    match s {
        "task_created" => Ok(TaskEventType::TaskCreated),
        "state_changed" => Ok(TaskEventType::StateChanged),
        "run_started" => Ok(TaskEventType::RunStarted),
        "run_ended" => Ok(TaskEventType::RunEnded),
        "user_steered" => Ok(TaskEventType::UserSteered),
        "user_message_queued" => Ok(TaskEventType::UserMessageQueued),
        "queue_dispatched" => Ok(TaskEventType::QueueDispatched),
        "run_aborted" => Ok(TaskEventType::RunAborted),
        "session_branched" => Ok(TaskEventType::SessionBranched),
        "session_cleared" => Ok(TaskEventType::SessionCleared),
        "subagent_started" => Ok(TaskEventType::SubagentStarted),
        "subagent_finished" => Ok(TaskEventType::SubagentFinished),
        "tool_call" => Ok(TaskEventType::ToolCall),
        "tool_result" => Ok(TaskEventType::ToolResult),
        "permission_requested" => Ok(TaskEventType::PermissionRequested),
        "permission_decided" => Ok(TaskEventType::PermissionDecided),
        "file_changed" => Ok(TaskEventType::FileChanged),
        "verification_run" => Ok(TaskEventType::VerificationRun),
        "change_requested" => Ok(TaskEventType::ChangeRequested),
        "system" => Ok(TaskEventType::System),
        _ => Err(ProductError::DatabaseError(format!(
            "invalid task event type: {s}"
        ))),
    }
}

/// 解析通知类别字符串。
fn parse_notification_kind(s: &str) -> Result<NotificationKind, ProductError> {
    NotificationKind::try_from_str(s)
        .ok_or_else(|| ProductError::DatabaseError(format!("invalid notification kind: {s}")))
}

/// 解析待发送消息状态字符串。
fn parse_queued_message_state(s: &str) -> Result<QueuedMessageState, ProductError> {
    QueuedMessageState::try_from_str(s)
        .ok_or_else(|| ProductError::DatabaseError(format!("invalid queued message state: {s}")))
}

/// 将 `ProjectAccessMode` 转换为存储字符串。
fn access_mode_str(mode: ProjectAccessMode) -> &'static str {
    match mode {
        ProjectAccessMode::RequestApproval => "request_approval",
        ProjectAccessMode::RiskBased => "risk_based",
        ProjectAccessMode::FullAccess => "full_access",
    }
}

/// 解析 `ProjectAccessMode` 字符串。
fn parse_access_mode(s: &str) -> Result<ProjectAccessMode, ProductError> {
    match s {
        "request_approval" => Ok(ProjectAccessMode::RequestApproval),
        "risk_based" => Ok(ProjectAccessMode::RiskBased),
        "full_access" => Ok(ProjectAccessMode::FullAccess),
        _ => Err(ProductError::DatabaseError(format!(
            "invalid project access mode: {s}"
        ))),
    }
}

/// 将 `WorkspaceMemoryMode` 转换为存储字符串。
fn workspace_memory_mode_str(mode: WorkspaceMemoryMode) -> &'static str {
    match mode {
        WorkspaceMemoryMode::Inherit => "inherit",
        WorkspaceMemoryMode::ReadOnly => "read_only",
        WorkspaceMemoryMode::Off => "off",
    }
}

/// 解析 `WorkspaceMemoryMode` 字符串。
fn parse_workspace_memory_mode(value: &str) -> Result<WorkspaceMemoryMode, ProductError> {
    match value {
        "inherit" => Ok(WorkspaceMemoryMode::Inherit),
        "read_only" => Ok(WorkspaceMemoryMode::ReadOnly),
        "off" => Ok(WorkspaceMemoryMode::Off),
        _ => Err(ProductError::DatabaseError(format!(
            "invalid workspace memory mode: {value}"
        ))),
    }
}

/// 将数据库行映射为 `Task`。
///
/// 列顺序：id, workspace_path, provider_name, title, goal, mode, state, worktree_path,
/// created_at, updated_at, model, agent_engine, inference_json, goal_active。
fn row_to_task(row: &rusqlite::Row<'_>) -> Result<Task, ProductError> {
    let mode_str: String = row.get(5).map_err(db_err)?;
    let mode = parse_task_mode(&mode_str)?;
    let state_str: String = row.get(6).map_err(db_err)?;
    let state = TaskState::try_from_str(&state_str)
        .ok_or_else(|| ProductError::DatabaseError(format!("invalid task state: {state_str}")))?;
    let created_str: String = row.get(8).map_err(db_err)?;
    let created_at = parse_ts(&created_str)?;
    let updated_str: String = row.get(9).map_err(db_err)?;
    let updated_at = parse_ts(&updated_str)?;
    let engine_str: String = row.get(11).map_err(db_err)?;
    let agent_engine = AgentEngine::try_from_str(&engine_str).ok_or_else(|| {
        ProductError::DatabaseError(format!("invalid task agent engine: {engine_str}"))
    })?;
    let inference_json: String = row.get(12).map_err(db_err)?;
    let inference = serde_json::from_str(&inference_json).map_err(|error| {
        ProductError::DatabaseError(format!("invalid task inference_json: {error}"))
    })?;

    Ok(Task {
        id: row.get(0).map_err(db_err)?,
        workspace_path: row.get(1).map_err(db_err)?,
        provider_name: row.get(2).map_err(db_err)?,
        title: row.get(3).map_err(db_err)?,
        goal: row.get(4).map_err(db_err)?,
        goal_active: row.get::<_, i64>(13).map_err(db_err)? != 0,
        mode,
        state,
        worktree_path: row.get(7).map_err(db_err)?,
        created_at,
        updated_at,
        model: row.get(10).map_err(db_err)?,
        agent_engine,
        inference,
    })
}

/// 将数据库行映射为 `AgentRun`。
///
/// 列顺序：id, task_id, branch_id, parent_run_id, agent_kind, agent_label,
/// delegated_by_tool_call_id, model, runtime_kind, external_session_id, review_state,
/// started_at, ended_at, usage_json, summary, access_mode, routing_reason,
/// require_approval
fn row_to_agent_run(row: &rusqlite::Row<'_>) -> Result<AgentRun, ProductError> {
    let agent_kind_str: String = row.get(4).map_err(db_err)?;
    let agent_kind = parse_agent_kind(&agent_kind_str)?;
    let runtime_kind_str: String = row.get(8).map_err(db_err)?;
    let runtime_kind = AgentRunRuntimeKind::try_from_str(&runtime_kind_str).ok_or_else(|| {
        ProductError::DatabaseError(format!(
            "invalid agent run runtime kind: {runtime_kind_str}"
        ))
    })?;
    let review_state_str: String = row.get(10).map_err(db_err)?;
    let review_state = ReviewState::try_from_str(&review_state_str).ok_or_else(|| {
        ProductError::DatabaseError(format!("invalid review state: {review_state_str}"))
    })?;
    let started_str: String = row.get(11).map_err(db_err)?;
    let started_at = parse_ts(&started_str)?;
    let ended_str: Option<String> = row.get(12).map_err(db_err)?;
    let ended_at = match ended_str {
        Some(s) => Some(parse_ts(&s)?),
        None => None,
    };
    let access_mode_str: String = row.get(15).map_err(db_err)?;
    let access_mode = match access_mode_str.as_str() {
        "read_only" => SubagentAccessMode::ReadOnly,
        "full_access" => SubagentAccessMode::FullAccess,
        value => {
            return Err(ProductError::DatabaseError(format!(
                "invalid subagent access mode: {value}"
            )))
        }
    };

    Ok(AgentRun {
        id: row.get(0).map_err(db_err)?,
        task_id: row.get(1).map_err(db_err)?,
        branch_id: row.get(2).map_err(db_err)?,
        parent_run_id: row.get(3).map_err(db_err)?,
        agent_kind,
        agent_label: row.get(5).map_err(db_err)?,
        delegated_by_tool_call_id: row.get(6).map_err(db_err)?,
        model: row.get(7).map_err(db_err)?,
        runtime_kind,
        access_mode,
        routing_reason: row.get(16).map_err(db_err)?,
        require_approval: row.get::<_, i64>(17).map_err(db_err)? != 0,
        external_session_id: row.get(9).map_err(db_err)?,
        review_state,
        started_at,
        ended_at,
        usage_json: row.get(13).map_err(db_err)?,
        summary: row.get(14).map_err(db_err)?,
    })
}

/// 列顺序：id, task_id, parent_branch_id, forked_from_message_id, storage_id, is_active, created_at
fn row_to_session_branch(row: &rusqlite::Row<'_>) -> Result<SessionBranch, ProductError> {
    let created_at: String = row.get(6).map_err(db_err)?;
    Ok(SessionBranch {
        id: row.get(0).map_err(db_err)?,
        task_id: row.get(1).map_err(db_err)?,
        parent_branch_id: row.get(2).map_err(db_err)?,
        forked_from_message_id: row.get(3).map_err(db_err)?,
        storage_id: row.get(4).map_err(db_err)?,
        is_active: row.get::<_, i64>(5).map_err(db_err)? != 0,
        created_at: parse_ts(&created_at)?,
    })
}

/// 列顺序：id, task_id, branch_id, message, state, priority, attachments_json, created_at, updated_at
fn row_to_queued_message(row: &rusqlite::Row<'_>) -> Result<QueuedMessage, ProductError> {
    let state: String = row.get(4).map_err(db_err)?;
    let created_at: String = row.get(7).map_err(db_err)?;
    let updated_at: String = row.get(8).map_err(db_err)?;
    Ok(QueuedMessage {
        id: row.get(0).map_err(db_err)?,
        task_id: row.get(1).map_err(db_err)?,
        branch_id: row.get(2).map_err(db_err)?,
        message: row.get(3).map_err(db_err)?,
        state: parse_queued_message_state(&state)?,
        priority: row.get(5).map_err(db_err)?,
        attachments_json: row.get(6).map_err(db_err)?,
        created_at: parse_ts(&created_at)?,
        updated_at: parse_ts(&updated_at)?,
    })
}

/// 将数据库行映射为 `Workspace`。
///
/// 列顺序：id, canonical_path, display_name, access_mode, last_opened_at,
/// memory_mode, memory_generation。
fn row_to_workspace(row: &rusqlite::Row<'_>) -> Result<Workspace, ProductError> {
    let access_mode_str: String = row.get(3).map_err(db_err)?;
    let access_mode = parse_access_mode(&access_mode_str)?;
    let last_opened_str: String = row.get(4).map_err(db_err)?;
    let last_opened_at = parse_ts(&last_opened_str)?;
    let memory_mode_str: String = row.get(5).map_err(db_err)?;
    let memory_mode = parse_workspace_memory_mode(&memory_mode_str)?;
    let memory_generation: i64 = row.get(6).map_err(db_err)?;
    let memory_generation = u64::try_from(memory_generation).map_err(|_| {
        ProductError::DatabaseError("workspace memory_generation must be non-negative".into())
    })?;

    Ok(Workspace {
        id: row.get(0).map_err(db_err)?,
        canonical_path: row.get(1).map_err(db_err)?,
        display_name: row.get(2).map_err(db_err)?,
        access_mode,
        last_opened_at,
        memory_mode,
        memory_generation,
    })
}

/// 将数据库行映射为 `(sequence, Notification)`。
///
/// 列顺序：sequence, id, kind, title, body, task_id, workspace_path, created_at, read_at
fn row_to_notification(row: &rusqlite::Row<'_>) -> Result<(i64, Notification), ProductError> {
    let kind: String = row.get(2).map_err(db_err)?;
    let created_at: String = row.get(7).map_err(db_err)?;
    let read_at: Option<String> = row.get(8).map_err(db_err)?;
    Ok((
        row.get(0).map_err(db_err)?,
        Notification {
            id: row.get(1).map_err(db_err)?,
            kind: parse_notification_kind(&kind)?,
            title: row.get(3).map_err(db_err)?,
            body: row.get(4).map_err(db_err)?,
            task_id: row.get(5).map_err(db_err)?,
            workspace_path: row.get(6).map_err(db_err)?,
            created_at: parse_ts(&created_at)?,
            read_at: read_at.as_deref().map(parse_ts).transpose()?,
        },
    ))
}

// ============================================================================
// TaskRepository
// ============================================================================

/// A project keeps a deliberately small set of active conversations. Archived conversations do
/// not consume a slot and remain available for audit/history.
pub const MAX_PROJECT_CONVERSATIONS: usize = 5;

/// Human-readable compatibility message; callers should branch on the typed ProductError variant.
pub const PROJECT_CONVERSATION_LIMIT_REACHED: &str =
    "该项目最多保留 5 个未归档对话，请先归档一个后再新建";

const PROJECT_CONVERSATION_TITLE: &str = "新对话";

fn insert_task(conn: &Connection, task: &Task) -> Result<(), ProductError> {
    conn.execute(
        "INSERT INTO tasks (id, workspace_path, provider_name, title, goal, mode, state, worktree_path, created_at, updated_at, model, agent_engine, inference_json, goal_active) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            task.id,
            task.workspace_path,
            task.provider_name,
            task.title,
            task.goal,
            task.mode.to_string(),
            task.state.to_string(),
            task.worktree_path,
            task.created_at.to_rfc3339(),
            task.updated_at.to_rfc3339(),
            task.model,
            task.agent_engine.to_string(),
            serde_json::to_string(&task.inference).map_err(|error| {
                ProductError::DatabaseError(format!("serialize task inference: {error}"))
            })?,
            task.goal_active,
        ],
    )
    .map_err(db_err)?;
    Ok(())
}

fn ensure_project_conversation_capacity(
    conn: &Connection,
    workspace_path: &str,
) -> Result<(), ProductError> {
    let active_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE workspace_path = ?1 AND state != 'archived'",
            params![workspace_path],
            |row| row.get(0),
        )
        .map_err(db_err)?;
    if active_count >= MAX_PROJECT_CONVERSATIONS as i64 {
        return Err(ProductError::ProjectConversationLimitReached {
            limit: MAX_PROJECT_CONVERSATIONS,
        });
    }
    Ok(())
}

fn project_conversation_sequence(title: &str) -> Option<u64> {
    let suffix = title.strip_prefix(PROJECT_CONVERSATION_TITLE)?.trim();
    if suffix.is_empty() {
        return Some(1);
    }
    if !suffix.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    suffix.parse::<u64>().ok().filter(|sequence| *sequence >= 2)
}

fn project_conversation_title(sequence: u64) -> String {
    if sequence == 1 {
        PROJECT_CONVERSATION_TITLE.to_string()
    } else {
        format!("{PROJECT_CONVERSATION_TITLE} {sequence}")
    }
}

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
        let Some(workspace_path) = task.workspace_path.as_deref() else {
            let conn = self.db.conn()?;
            return insert_task(&conn, task);
        };

        let mut conn = self.db.conn()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_err)?;
        ensure_project_conversation_capacity(&tx, workspace_path)?;
        insert_task(&tx, task)?;
        tx.commit().map_err(db_err)
    }

    /// Atomically allocate the next project-scoped placeholder title, enforce the active
    /// conversation limit, and persist the task's initial branch/event. An IMMEDIATE transaction
    /// serializes concurrent plus-button clicks before either the count or title is observed.
    pub fn create_project_conversation(
        &self,
        task: &mut Task,
    ) -> Result<SessionBranch, ProductError> {
        let workspace_path = task.workspace_path.as_deref().ok_or_else(|| {
            ProductError::DatabaseError(
                "project conversation requires a workspace path".to_string(),
            )
        })?;
        let mut conn = self.db.conn()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_err)?;

        ensure_project_conversation_capacity(&tx, workspace_path)?;

        let next_sequence = {
            let mut statement = tx
                .prepare("SELECT title FROM tasks WHERE workspace_path = ?1")
                .map_err(db_err)?;
            let titles = statement
                .query_map(params![workspace_path], |row| row.get::<_, String>(0))
                .map_err(db_err)?;
            let mut highest = 0_u64;
            for title in titles {
                if let Some(sequence) = project_conversation_sequence(&title.map_err(db_err)?) {
                    highest = highest.max(sequence);
                }
            }
            highest.checked_add(1).ok_or_else(|| {
                ProductError::DatabaseError(
                    "project conversation title sequence overflowed".to_string(),
                )
            })?
        };
        task.title = project_conversation_title(next_sequence);

        insert_task(&tx, task)?;
        let branch = SessionBranch::main(&task.id);
        tx.execute(
            "INSERT INTO session_branches \
             (id, task_id, parent_branch_id, forked_from_message_id, storage_id, is_active, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)",
            params![
                branch.id,
                branch.task_id,
                branch.parent_branch_id,
                branch.forked_from_message_id,
                branch.storage_id,
                branch.created_at.to_rfc3339(),
            ],
        )
        .map_err(db_err)?;
        tx.execute(
            "INSERT INTO task_events (task_id, branch_id, event_type, created_at) \
             VALUES (?1, ?2, ?3, ?4)",
            params![
                task.id,
                branch.id,
                TaskEventType::TaskCreated.to_string(),
                branch.created_at.to_rfc3339(),
            ],
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)?;
        Ok(branch)
    }

    /// 按 ID 获取任务。
    pub fn get(&self, id: &str) -> Result<Option<Task>, ProductError> {
        let conn = self.db.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, workspace_path, provider_name, title, goal, mode, state, worktree_path, created_at, updated_at, model, agent_engine, inference_json, goal_active \
                 FROM tasks WHERE id = ?1",
            )
            .map_err(db_err)?;
        let mut rows = stmt.query(params![id]).map_err(db_err)?;
        match rows.next().map_err(db_err)? {
            Some(row) => Ok(Some(row_to_task(row)?)),
            None => Ok(None),
        }
    }

    /// 列出任务，支持按工作区/状态过滤。
    pub fn list(
        &self,
        workspace_path: Option<&str>,
        state: Option<TaskState>,
        include_archived: bool,
    ) -> Result<Vec<Task>, ProductError> {
        let conn = self.db.conn()?;
        let mut sql = String::from(
            "SELECT id, workspace_path, provider_name, title, goal, mode, state, worktree_path, created_at, updated_at, model, agent_engine, inference_json, goal_active \
             FROM tasks WHERE 1=1",
        );
        let mut param_values: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

        if let Some(path) = workspace_path {
            sql.push_str(" AND workspace_path = ?");
            param_values.push(Box::new(path.to_string()));
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

    /// 显式更新任务交互模式（同时更新 `updated_at`）。
    pub fn set_mode(&self, id: &str, mode: TaskMode) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE tasks SET mode = ?1, updated_at = ?2 WHERE id = ?3",
            params![mode.to_string(), Utc::now().to_rfc3339(), id],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// Update or clear the explicitly activated Goal consumed by Plan and subsequent model turns.
    pub fn set_goal(&self, id: &str, goal: &str) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        let goal = goal.trim();
        conn.execute(
            "UPDATE tasks SET goal = ?1, goal_active = ?2, updated_at = ?3 WHERE id = ?4",
            params![goal, !goal.is_empty(), Utc::now().to_rfc3339(), id],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 更新任务的可选工作区绑定（`None` 表示纯聊天会话）。
    pub fn set_workspace_path(
        &self,
        id: &str,
        workspace_path: Option<&str>,
    ) -> Result<DateTime<Utc>, ProductError> {
        let conn = self.db.conn()?;
        let updated_at = Utc::now();
        let changed = conn
            .execute(
                "UPDATE tasks SET workspace_path = ?1, updated_at = ?2 \
                 WHERE id = ?3 AND (workspace_path IS NULL OR workspace_path = ?1)",
                params![workspace_path, updated_at.to_rfc3339(), id],
            )
            .map_err(db_err)?;
        if changed != 1 {
            return Err(ProductError::StateMachineError(
                "task workspace is immutable after its first binding".to_string(),
            ));
        }
        Ok(updated_at)
    }

    /// 绑定会话在后续运行中使用的具体模型。`None` 表示沿用该服务的默认模型。
    pub fn set_model(&self, id: &str, model: Option<&str>) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE tasks SET model = ?1, updated_at = ?2 WHERE id = ?3",
            params![model, Utc::now().to_rfc3339(), id],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 保存会话级模型推理参数。空对象表示完全沿用服务默认值。
    pub fn set_inference(
        &self,
        id: &str,
        inference: &hermes_core::InferenceOptions,
    ) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        let inference_json = serde_json::to_string(inference).map_err(|error| {
            ProductError::DatabaseError(format!("serialize task inference: {error}"))
        })?;
        conn.execute(
            "UPDATE tasks SET inference_json = ?1, updated_at = ?2 WHERE id = ?3",
            params![inference_json, Utc::now().to_rfc3339(), id],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 绑定会话在后续运行中使用的模型服务。`None` 仅用于兼容旧会话回退全局默认。
    pub fn set_provider_name(
        &self,
        id: &str,
        provider_name: Option<&str>,
    ) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE tasks SET provider_name = ?1, updated_at = ?2 WHERE id = ?3",
            params![provider_name, Utc::now().to_rfc3339(), id],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 切换会话的主 Agent 引擎；调用方负责确保任务当前没有活跃运行。
    pub fn set_agent_engine(
        &self,
        id: &str,
        agent_engine: AgentEngine,
    ) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE tasks SET agent_engine = ?1, updated_at = ?2 WHERE id = ?3",
            params![agent_engine.to_string(), Utc::now().to_rfc3339(), id],
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
            sets.push("goal_active = ?");
            param_values.push(Box::new(!g.trim().is_empty()));
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

    /// 永久删除任务。所有以 task_id 关联的产品记录、Blob 引用与 Plan 投影由统一
    /// 生命周期事务清理。
    /// 返回 false 表示任务在执行前已经不存在。
    pub fn delete(
        &self,
        id: &str,
        blobs_dir: &Path,
        projection_root: &Path,
    ) -> Result<bool, ProductError> {
        let result =
            crate::lifecycle_purge::LifecyclePurgeStore::new(self.db, blobs_dir, projection_root)
                .purge_task(id)?;
        for warning in result.cleanup_warnings {
            tracing::warn!(task_id = id, %warning, "task AppData cleanup will retry at startup");
        }
        Ok(result.removed_tasks > 0)
    }
}

// ============================================================================
// AgentRunRepository
// ============================================================================

/// 验证占位 Run 的 model 值。
///
/// 它不是真实模型名，只是一个哨兵：任务从未跑过 agent 时，验证记录需要一条
/// `agent_runs` 行来满足 `verifications.run_id` 外键。MIGRATION_009 也用同一个
/// 指纹识别历史遗留数据。
pub const VERIFICATION_PLACEHOLDER_MODEL: &str = "verification";

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
            "INSERT INTO agent_runs \
             (id, task_id, branch_id, parent_run_id, agent_kind, agent_label, summary, delegated_by_tool_call_id, \
              model, runtime_kind, external_session_id, review_state, started_at, ended_at, usage_json, access_mode, routing_reason, require_approval) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            params![
                run.id,
                run.task_id,
                run.branch_id,
                run.parent_run_id,
                run.agent_kind.to_string(),
                run.agent_label,
                run.summary,
                run.delegated_by_tool_call_id,
                run.model,
                run.runtime_kind.to_string(),
                run.external_session_id,
                run.review_state.to_string(),
                run.started_at.to_rfc3339(),
                run.ended_at.map(|dt| dt.to_rfc3339()),
                run.usage_json,
                run.access_mode.to_string(),
                run.routing_reason,
                i64::from(run.require_approval),
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
                "SELECT id, task_id, branch_id, parent_run_id, agent_kind, agent_label, delegated_by_tool_call_id, \
                        model, runtime_kind, external_session_id, review_state, started_at, ended_at, usage_json, summary, access_mode, routing_reason, require_approval \
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
                "SELECT id, task_id, branch_id, parent_run_id, agent_kind, agent_label, delegated_by_tool_call_id, \
                        model, runtime_kind, external_session_id, review_state, started_at, ended_at, usage_json, summary, access_mode, routing_reason, require_approval \
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

    /// 列出某次运行直接委派的子代理（按开始时间升序）。
    pub fn list_by_parent_run_id(
        &self,
        parent_run_id: &str,
    ) -> Result<Vec<AgentRun>, ProductError> {
        let conn = self.db.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, branch_id, parent_run_id, agent_kind, agent_label, delegated_by_tool_call_id, \
                        model, runtime_kind, external_session_id, review_state, started_at, ended_at, usage_json, summary, access_mode, routing_reason, require_approval \
                 FROM agent_runs WHERE parent_run_id = ?1 ORDER BY started_at ASC",
            )
            .map_err(db_err)?;
        let mut rows = stmt.query(params![parent_run_id]).map_err(db_err)?;
        let mut runs = Vec::new();
        while let Some(row) = rows.next().map_err(db_err)? {
            runs.push(row_to_agent_run(row)?);
        }
        Ok(runs)
    }

    /// 列出某个会话分支的运行记录（按开始时间降序）。
    pub fn list_by_task_branch(
        &self,
        task_id: &str,
        branch_id: &str,
    ) -> Result<Vec<AgentRun>, ProductError> {
        let conn = self.db.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, branch_id, parent_run_id, agent_kind, agent_label, delegated_by_tool_call_id, \
                        model, runtime_kind, external_session_id, review_state, started_at, ended_at, usage_json, summary, access_mode, routing_reason, require_approval \
                 FROM agent_runs WHERE task_id = ?1 AND branch_id = ?2 ORDER BY started_at DESC",
            )
            .map_err(db_err)?;
        let mut rows = stmt.query(params![task_id, branch_id]).map_err(db_err)?;
        let mut runs = Vec::new();
        while let Some(row) = rows.next().map_err(db_err)? {
            runs.push(row_to_agent_run(row)?);
        }
        Ok(runs)
    }

    /// 获取 Task 的活跃主 Run（`ended_at IS NULL`）。
    pub fn get_active_run(&self, task_id: &str) -> Result<Option<AgentRun>, ProductError> {
        let conn = self.db.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, branch_id, parent_run_id, agent_kind, agent_label, delegated_by_tool_call_id, \
                        model, runtime_kind, external_session_id, review_state, started_at, ended_at, usage_json, summary, access_mode, routing_reason, require_approval \
                 FROM agent_runs WHERE task_id = ?1 AND agent_kind = 'main' AND ended_at IS NULL \
                 ORDER BY started_at DESC LIMIT 1",
            )
            .map_err(db_err)?;
        let mut rows = stmt.query(params![task_id]).map_err(db_err)?;
        match rows.next().map_err(db_err)? {
            Some(row) => Ok(Some(row_to_agent_run(row)?)),
            None => Ok(None),
        }
    }

    /// 获取一个分支的活跃主运行。
    pub fn get_active_run_for_branch(
        &self,
        task_id: &str,
        branch_id: &str,
    ) -> Result<Option<AgentRun>, ProductError> {
        let conn = self.db.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, branch_id, parent_run_id, agent_kind, agent_label, delegated_by_tool_call_id, \
                        model, runtime_kind, external_session_id, review_state, started_at, ended_at, usage_json, summary, access_mode, routing_reason, require_approval \
                 FROM agent_runs WHERE task_id = ?1 AND branch_id = ?2 \
                   AND agent_kind = 'main' AND ended_at IS NULL \
                 ORDER BY started_at DESC LIMIT 1",
            )
            .map_err(db_err)?;
        let mut rows = stmt.query(params![task_id, branch_id]).map_err(db_err)?;
        match rows.next().map_err(db_err)? {
            Some(row) => Ok(Some(row_to_agent_run(row)?)),
            None => Ok(None),
        }
    }

    /// 获取 Task 最近一条**可审阅**的主 Run（不要求仍活跃）。
    ///
    /// 与 [`Self::get_active_run`] 的分工：
    /// - `get_active_run` 回答"现在有没有在跑"，因此过滤 `ended_at IS NULL`；
    /// - 本方法回答"用户正在审阅的是哪条"。
    ///
    /// 审查（接受/回滚）作用的那条 run 按定义已经结束（drain loop 收尾时写入
    /// `ended_at`），所以不能用 `get_active_run` 定位。
    ///
    /// 排除验证占位 Run：它由 `run_verification` 在任务从未跑过 agent 时创建，
    /// `started_at` 是"用户点验证的时刻"，必然晚于真实 run —— 不排除的话
    /// 存量库里审查动作会写到占位 run 上，真实 run 永远停在 pending。
    pub fn get_latest_main_run(&self, task_id: &str) -> Result<Option<AgentRun>, ProductError> {
        let conn = self.db.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, branch_id, parent_run_id, agent_kind, agent_label, delegated_by_tool_call_id, \
                        model, runtime_kind, external_session_id, review_state, started_at, ended_at, usage_json, summary, access_mode, routing_reason, require_approval \
                 FROM agent_runs WHERE task_id = ?1 AND agent_kind = 'main' \
                   AND model <> ?2 \
                 ORDER BY started_at DESC, id DESC LIMIT 1",
            )
            .map_err(db_err)?;
        let mut rows = stmt
            .query(params![task_id, VERIFICATION_PLACEHOLDER_MODEL])
            .map_err(db_err)?;
        match rows.next().map_err(db_err)? {
            Some(row) => Ok(Some(row_to_agent_run(row)?)),
            None => Ok(None),
        }
    }

    /// 获取该 Task 的验证占位 Run。
    ///
    /// `verifications.run_id` 是 `NOT NULL REFERENCES agent_runs(id)`，从未跑过
    /// agent 的任务需要一条占位记录来满足外键。复用它，避免每点一次验证多一行。
    pub fn get_verification_placeholder_run(
        &self,
        task_id: &str,
    ) -> Result<Option<AgentRun>, ProductError> {
        let conn = self.db.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, branch_id, parent_run_id, agent_kind, agent_label, delegated_by_tool_call_id, \
                        model, runtime_kind, external_session_id, review_state, started_at, ended_at, usage_json, summary, access_mode, routing_reason, require_approval \
                 FROM agent_runs WHERE task_id = ?1 AND agent_kind = 'main' AND model = ?2 \
                 ORDER BY started_at DESC, id DESC LIMIT 1",
            )
            .map_err(db_err)?;
        let mut rows = stmt
            .query(params![task_id, VERIFICATION_PLACEHOLDER_MODEL])
            .map_err(db_err)?;
        match rows.next().map_err(db_err)? {
            Some(row) => Ok(Some(row_to_agent_run(row)?)),
            None => Ok(None),
        }
    }

    /// 更新审查状态；`ended_at` 仅在尚未设置时填入。
    pub fn update_review_state(
        &self,
        id: &str,
        review_state: ReviewState,
    ) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            // COALESCE：首次结束时刻不可变。接受/回滚发生在 run 结束之后，
            // 若无条件覆写会把审计里的"运行结束时间"改成"用户点按钮的时间"。
            "UPDATE agent_runs SET review_state = ?1, ended_at = COALESCE(ended_at, ?2) WHERE id = ?3",
            params![review_state.to_string(), Utc::now().to_rfc3339(), id],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// Atomically finish a still-active run and preserve any summary that was already recorded.
    ///
    /// Cleanup paths use the affected-row result to decide whether they own the terminal event.
    /// This prevents a late provider lifecycle event and a host timeout fallback from both
    /// appending `SubagentFinished` for the same run.
    pub fn finish_if_active(
        &self,
        id: &str,
        review_state: ReviewState,
        fallback_summary: Option<&str>,
    ) -> Result<bool, ProductError> {
        let conn = self.db.conn()?;
        let updated = conn
            .execute(
                "UPDATE agent_runs \
                 SET review_state = ?1, ended_at = ?2, summary = COALESCE(summary, ?3) \
                 WHERE id = ?4 AND ended_at IS NULL",
                params![
                    review_state.to_string(),
                    Utc::now().to_rfc3339(),
                    fallback_summary,
                    id,
                ],
            )
            .map_err(db_err)?;
        Ok(updated > 0)
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

    /// 保存外部 Agent 的会话标识。该值用于后续续接/诊断，绝不是认证凭据。
    pub fn set_external_session_id(
        &self,
        id: &str,
        external_session_id: Option<&str>,
    ) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE agent_runs SET external_session_id = ?1 WHERE id = ?2",
            params![external_session_id, id],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 保存子代理的受限完成摘要。
    pub fn set_summary(&self, id: &str, summary: Option<&str>) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE agent_runs SET summary = ?1 WHERE id = ?2",
            params![summary, id],
        )
        .map_err(db_err)?;
        Ok(())
    }
}

// ============================================================================
// ToolCallRepository
// ============================================================================

/// 工具调用审计仓储。AgentEvent 的 `call_id` 是运行树和权限记录的关联锚点，
/// 因而按调用 ID 幂等写入，而不是另行生成数据库 ID。
pub struct ToolCallRepository<'a> {
    db: &'a Database,
}

impl<'a> ToolCallRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// 在工具调用开始时建立审计记录；重复事件不会覆盖先前结果。
    pub fn create_if_absent(&self, call: &ToolCall) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT OR IGNORE INTO tool_calls \
             (id, run_id, task_id, tool_name, input_json, output_json, risk_level, status, \
              caller, started_at, ended_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                call.id,
                call.run_id,
                call.task_id,
                call.tool_name,
                call.input_json,
                call.output_json,
                call.risk_level.to_string(),
                call.status.to_string(),
                call.caller,
                call.started_at.to_rfc3339(),
                call.ended_at.map(|dt| dt.to_rfc3339()),
            ],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 将对应调用标记为完成；未知调用保持幂等，兼容旧的事件流。
    pub fn finish(
        &self,
        id: &str,
        output: &serde_json::Value,
        is_error: bool,
    ) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        let status = if is_error {
            ToolCallStatus::Error
        } else {
            ToolCallStatus::Ok
        };
        conn.execute(
            "UPDATE tool_calls SET output_json = ?1, status = ?2, ended_at = ?3 WHERE id = ?4",
            params![
                output.to_string(),
                status.to_string(),
                Utc::now().to_rfc3339(),
                id,
            ],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 子运行已经进入终态、但外部事件流没有为部分工具发出结果时，立即关闭这些
    /// 残留审计行。它们不能继续显示为“运行中”，也不能被误记为成功。
    pub fn finish_running_for_run_as_error(
        &self,
        run_id: &str,
        output: &serde_json::Value,
    ) -> Result<u64, ProductError> {
        let conn = self.db.conn()?;
        let updated = conn
            .execute(
                "UPDATE tool_calls \
                 SET output_json = COALESCE(output_json, ?1), status = ?2, ended_at = ?3 \
                 WHERE run_id = ?4 AND status = ?5",
                params![
                    output.to_string(),
                    ToolCallStatus::Error.to_string(),
                    Utc::now().to_rfc3339(),
                    run_id,
                    ToolCallStatus::Running.to_string(),
                ],
            )
            .map_err(db_err)?;
        Ok(updated as u64)
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
        self.append_for_branch(task_id, "main", event_type)
    }

    /// 追加指定会话分支的事件，返回自增 ID。
    pub fn append_for_branch(
        &self,
        task_id: &str,
        branch_id: &str,
        event_type: TaskEventType,
    ) -> Result<i64, ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT INTO task_events (task_id, branch_id, event_type, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![
                task_id,
                branch_id,
                event_type.to_string(),
                Utc::now().to_rfc3339()
            ],
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
                "SELECT id, task_id, branch_id, event_type, created_at FROM task_events \
                 WHERE task_id = ?1 ORDER BY created_at ASC, id ASC LIMIT ?2 OFFSET ?3",
            )
            .map_err(db_err)?;
        let mut rows = stmt
            .query(params![task_id, limit_val, offset_val])
            .map_err(db_err)?;
        let mut events = Vec::new();
        while let Some(row) = rows.next().map_err(db_err)? {
            let event_type_str: String = row.get(3).map_err(db_err)?;
            let event_type = parse_task_event_type(&event_type_str)?;
            let created_str: String = row.get(4).map_err(db_err)?;
            let created_at = parse_ts(&created_str)?;
            events.push(TaskEvent {
                id: row.get(0).map_err(db_err)?,
                task_id: row.get(1).map_err(db_err)?,
                branch_id: row.get(2).map_err(db_err)?,
                event_type,
                created_at,
            });
        }
        Ok(events)
    }

    /// 列出某个会话分支的事件。
    pub fn list_by_task_branch(
        &self,
        task_id: &str,
        branch_id: &str,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<Vec<TaskEvent>, ProductError> {
        let conn = self.db.conn()?;
        let limit_val = limit.unwrap_or(100) as i64;
        let offset_val = offset.unwrap_or(0) as i64;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, branch_id, event_type, created_at FROM task_events \
                 WHERE task_id = ?1 AND branch_id = ?2 \
                 ORDER BY created_at ASC, id ASC LIMIT ?3 OFFSET ?4",
            )
            .map_err(db_err)?;
        let mut rows = stmt
            .query(params![task_id, branch_id, limit_val, offset_val])
            .map_err(db_err)?;
        let mut events = Vec::new();
        while let Some(row) = rows.next().map_err(db_err)? {
            let event_type_str: String = row.get(3).map_err(db_err)?;
            events.push(TaskEvent {
                id: row.get(0).map_err(db_err)?,
                task_id: row.get(1).map_err(db_err)?,
                branch_id: row.get(2).map_err(db_err)?,
                event_type: parse_task_event_type(&event_type_str)?,
                created_at: parse_ts(&row.get::<_, String>(4).map_err(db_err)?)?,
            });
        }
        Ok(events)
    }

    /// 按事件 ID 倒序列出未归档会话的全局近期事件。
    ///
    /// `before_event_id` 是不透明分页游标的内部值；前端只需把响应中的
    /// `next_cursor` 原样带回。自增 ID 比时间字符串更稳定，也能避免同一毫秒的
    /// 多条事件在翻页时重叠或遗漏。
    pub fn list_recent(
        &self,
        before_event_id: Option<i64>,
        limit: u32,
    ) -> Result<Vec<TaskEvent>, ProductError> {
        let conn = self.db.conn()?;
        let before = before_event_id.unwrap_or(i64::MAX);
        let limit = i64::from(limit.clamp(1, 100));
        let mut stmt = conn
            .prepare(
                "SELECT event.id, event.task_id, event.branch_id, event.event_type, event.created_at \
                 FROM task_events AS event \
                 INNER JOIN tasks AS task ON task.id = event.task_id \
                 WHERE task.state != 'archived' AND event.id < ?1 \
                 ORDER BY event.id DESC LIMIT ?2",
            )
            .map_err(db_err)?;
        let mut rows = stmt.query(params![before, limit]).map_err(db_err)?;
        let mut events = Vec::new();
        while let Some(row) = rows.next().map_err(db_err)? {
            let event_type: String = row.get(3).map_err(db_err)?;
            events.push(TaskEvent {
                id: row.get(0).map_err(db_err)?,
                task_id: row.get(1).map_err(db_err)?,
                branch_id: row.get(2).map_err(db_err)?,
                event_type: parse_task_event_type(&event_type)?,
                created_at: parse_ts(&row.get::<_, String>(4).map_err(db_err)?)?,
            });
        }
        Ok(events)
    }

    /// 按事件 ID 倒序列出某个工作区的近期事件。
    pub fn list_by_workspace_recent(
        &self,
        workspace_path: &str,
        before_event_id: Option<i64>,
        limit: u32,
    ) -> Result<Vec<TaskEvent>, ProductError> {
        let conn = self.db.conn()?;
        let before = before_event_id.unwrap_or(i64::MAX);
        let limit = i64::from(limit.clamp(1, 100));
        let mut stmt = conn
            .prepare(
                "SELECT event.id, event.task_id, event.branch_id, event.event_type, event.created_at \
                 FROM task_events AS event \
                 INNER JOIN tasks AS task ON task.id = event.task_id \
                 WHERE task.workspace_path = ?1 AND task.state != 'archived' AND event.id < ?2 \
                 ORDER BY event.id DESC LIMIT ?3",
            )
            .map_err(db_err)?;
        let mut rows = stmt
            .query(params![workspace_path, before, limit])
            .map_err(db_err)?;
        let mut events = Vec::new();
        while let Some(row) = rows.next().map_err(db_err)? {
            let event_type: String = row.get(3).map_err(db_err)?;
            events.push(TaskEvent {
                id: row.get(0).map_err(db_err)?,
                task_id: row.get(1).map_err(db_err)?,
                branch_id: row.get(2).map_err(db_err)?,
                event_type: parse_task_event_type(&event_type)?,
                created_at: parse_ts(&row.get::<_, String>(4).map_err(db_err)?)?,
            });
        }
        Ok(events)
    }
}

// ============================================================================
// NotificationRepository
// ============================================================================

/// 可追溯的用户通知仓库。
///
/// `source_key` 是内部去重键（例如 `permission:<request-id>`）；同一待处理项
/// 在轮询同步时只会刷新展示文案，不会重复制造未读通知。
pub struct NotificationRepository<'a> {
    db: &'a Database,
}

impl<'a> NotificationRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// 新建或刷新一条通知，同时保留既有的创建时间和已读状态。
    pub fn upsert(
        &self,
        source_key: &str,
        notification: &Notification,
    ) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT INTO notifications \
             (id, source_key, kind, title, body, task_id, workspace_path, created_at, read_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
             ON CONFLICT(source_key) DO UPDATE SET \
               kind = excluded.kind, title = excluded.title, body = excluded.body, \
               task_id = excluded.task_id, workspace_path = excluded.workspace_path",
            params![
                notification.id,
                source_key,
                notification.kind.to_string(),
                notification.title,
                notification.body,
                notification.task_id,
                notification.workspace_path,
                notification.created_at.to_rfc3339(),
                notification.read_at.map(|time| time.to_rfc3339()),
            ],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 列出通知及其内部顺序号（用于构造下一页游标）。
    pub fn list(
        &self,
        before_sequence: Option<i64>,
        limit: u32,
        unread_only: bool,
    ) -> Result<Vec<(i64, Notification)>, ProductError> {
        let conn = self.db.conn()?;
        let before = before_sequence.unwrap_or(i64::MAX);
        let limit = i64::from(limit.clamp(1, 100));
        let sql = if unread_only {
            "SELECT sequence, id, kind, title, body, task_id, workspace_path, created_at, read_at \
             FROM notifications WHERE sequence < ?1 AND read_at IS NULL \
             ORDER BY sequence DESC LIMIT ?2"
        } else {
            "SELECT sequence, id, kind, title, body, task_id, workspace_path, created_at, read_at \
             FROM notifications WHERE sequence < ?1 \
             ORDER BY sequence DESC LIMIT ?2"
        };
        let mut stmt = conn.prepare(sql).map_err(db_err)?;
        let mut rows = stmt.query(params![before, limit]).map_err(db_err)?;
        let mut items = Vec::new();
        while let Some(row) = rows.next().map_err(db_err)? {
            items.push(row_to_notification(row)?);
        }
        Ok(items)
    }

    /// 返回准确的未读数量，不受当前分页大小影响。
    pub fn unread_count(&self) -> Result<u64, ProductError> {
        let conn = self.db.conn()?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM notifications WHERE read_at IS NULL",
                [],
                |row| row.get(0),
            )
            .map_err(db_err)?;
        Ok(count.max(0) as u64)
    }

    /// 标记一条通知已读；不存在时返回 false。
    pub fn mark_read(&self, id: &str) -> Result<bool, ProductError> {
        let conn = self.db.conn()?;
        let changed = conn
            .execute(
                "UPDATE notifications SET read_at = COALESCE(read_at, ?1) WHERE id = ?2",
                params![Utc::now().to_rfc3339(), id],
            )
            .map_err(db_err)?;
        Ok(changed > 0)
    }

    /// 处理完一个待办源后，将其关联通知置为已读。
    pub fn mark_source_read(&self, source_key: &str) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE notifications SET read_at = COALESCE(read_at, ?1) WHERE source_key = ?2",
            params![Utc::now().to_rfc3339(), source_key],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 将同一任务下某类来源的旧通知置为已读；`keep_source_key` 存在时保留该条。
    pub fn mark_task_source_prefix_read(
        &self,
        task_id: &str,
        source_prefix: &str,
        keep_source_key: Option<&str>,
    ) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        let pattern = format!("{source_prefix}%");
        let now = Utc::now().to_rfc3339();
        match keep_source_key {
            Some(keep) => {
                conn.execute(
                    "UPDATE notifications SET read_at = COALESCE(read_at, ?1) \
                     WHERE task_id = ?2 AND source_key LIKE ?3 AND source_key != ?4",
                    params![now, task_id, pattern, keep],
                )
                .map_err(db_err)?;
            }
            None => {
                conn.execute(
                    "UPDATE notifications SET read_at = COALESCE(read_at, ?1) \
                     WHERE task_id = ?2 AND source_key LIKE ?3",
                    params![now, task_id, pattern],
                )
                .map_err(db_err)?;
            }
        }
        Ok(())
    }

    /// 将所有当前未读通知标记为已读，返回受影响数量。
    pub fn mark_all_read(&self) -> Result<u64, ProductError> {
        let conn = self.db.conn()?;
        let changed = conn
            .execute(
                "UPDATE notifications SET read_at = ?1 WHERE read_at IS NULL",
                params![Utc::now().to_rfc3339()],
            )
            .map_err(db_err)?;
        Ok(changed as u64)
    }
}

// ============================================================================
// SessionBranchRepository
// ============================================================================

/// 会话分支元数据仓库。JSONL 内容仍由 `SessionStore` 管理，本仓库只记录分支关系与活跃视图。
pub struct SessionBranchRepository<'a> {
    db: &'a Database,
}

impl<'a> SessionBranchRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// 返回当前活跃分支；旧任务首次访问时惰性建立主分支。
    pub fn ensure_active(&self, task_id: &str) -> Result<SessionBranch, ProductError> {
        if let Some(branch) = self.active(task_id)? {
            return Ok(branch);
        }
        let branch = SessionBranch::main(task_id);
        let conn = self.db.conn()?;
        conn.execute(
            "INSERT OR IGNORE INTO session_branches \
             (id, task_id, parent_branch_id, forked_from_message_id, storage_id, is_active, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6)",
            params![
                branch.id,
                branch.task_id,
                branch.parent_branch_id,
                branch.forked_from_message_id,
                branch.storage_id,
                branch.created_at.to_rfc3339(),
            ],
        )
        .map_err(db_err)?;
        drop(conn);
        self.active(task_id)?.ok_or_else(|| {
            ProductError::DatabaseError(format!(
                "failed to create active branch for task: {task_id}"
            ))
        })
    }

    /// 查询任务当前活跃分支。
    pub fn active(&self, task_id: &str) -> Result<Option<SessionBranch>, ProductError> {
        let conn = self.db.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, parent_branch_id, forked_from_message_id, storage_id, is_active, created_at \
                 FROM session_branches WHERE task_id = ?1 AND is_active = 1 LIMIT 1",
            )
            .map_err(db_err)?;
        let mut rows = stmt.query(params![task_id]).map_err(db_err)?;
        match rows.next().map_err(db_err)? {
            Some(row) => Ok(Some(row_to_session_branch(row)?)),
            None => Ok(None),
        }
    }

    /// 列出全部分支（用于审计与回放，不影响当前 UI）。
    pub fn list_by_task(&self, task_id: &str) -> Result<Vec<SessionBranch>, ProductError> {
        let conn = self.db.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, parent_branch_id, forked_from_message_id, storage_id, is_active, created_at \
                 FROM session_branches WHERE task_id = ?1 ORDER BY created_at DESC",
            )
            .map_err(db_err)?;
        let mut rows = stmt.query(params![task_id]).map_err(db_err)?;
        let mut branches = Vec::new();
        while let Some(row) = rows.next().map_err(db_err)? {
            branches.push(row_to_session_branch(row)?);
        }
        Ok(branches)
    }

    /// 原子地将新分支设为活跃分支，旧分支元数据和 JSONL 都不改写。
    pub fn create_fork(&self, branch: &SessionBranch) -> Result<(), ProductError> {
        let mut conn = self.db.conn()?;
        let tx = conn.transaction().map_err(db_err)?;
        tx.execute(
            "UPDATE session_branches SET is_active = 0 WHERE task_id = ?1 AND is_active = 1",
            params![branch.task_id],
        )
        .map_err(db_err)?;
        tx.execute(
            "INSERT INTO session_branches \
             (id, task_id, parent_branch_id, forked_from_message_id, storage_id, is_active, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                branch.id,
                branch.task_id,
                branch.parent_branch_id,
                branch.forked_from_message_id,
                branch.storage_id,
                if branch.is_active { 1_i64 } else { 0_i64 },
                branch.created_at.to_rfc3339(),
            ],
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)
    }

    /// Atomically activate an empty context branch and retire every pending message from its
    /// source branch. Ordinary first-turn goals can be cleared with the same transaction, while
    /// an explicitly managed persistent Goal remains attached to the task.
    pub fn reset_context(
        &self,
        branch: &SessionBranch,
        source_branch_id: &str,
        clear_ordinary_goal: bool,
    ) -> Result<(), ProductError> {
        let mut conn = self.db.conn()?;
        let tx = conn.transaction().map_err(db_err)?;
        let deactivated = tx
            .execute(
                "UPDATE session_branches SET is_active = 0 \
                 WHERE task_id = ?1 AND id = ?2 AND is_active = 1",
                params![branch.task_id, source_branch_id],
            )
            .map_err(db_err)?;
        if deactivated != 1 {
            return Err(ProductError::DatabaseError(format!(
                "active branch changed while clearing task: {}",
                branch.task_id
            )));
        }
        tx.execute(
            "INSERT INTO session_branches \
             (id, task_id, parent_branch_id, forked_from_message_id, storage_id, is_active, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                branch.id,
                branch.task_id,
                branch.parent_branch_id,
                branch.forked_from_message_id,
                branch.storage_id,
                if branch.is_active { 1_i64 } else { 0_i64 },
                branch.created_at.to_rfc3339(),
            ],
        )
        .map_err(db_err)?;
        let now = Utc::now().to_rfc3339();
        tx.execute(
            "UPDATE queued_messages SET state = 'cancelled', updated_at = ?1 \
             WHERE task_id = ?2 AND branch_id = ?3 \
               AND state IN ('queued', 'dispatching', 'failed')",
            params![now, branch.task_id, source_branch_id],
        )
        .map_err(db_err)?;
        let task_updated = if clear_ordinary_goal {
            tx.execute(
                "UPDATE tasks SET goal = '', goal_active = 0, updated_at = ?1 WHERE id = ?2",
                params![now, branch.task_id],
            )
        } else {
            tx.execute(
                "UPDATE tasks SET updated_at = ?1 WHERE id = ?2",
                params![now, branch.task_id],
            )
        }
        .map_err(db_err)?;
        if task_updated != 1 {
            return Err(ProductError::DatabaseError(format!(
                "task disappeared while clearing context: {}",
                branch.task_id
            )));
        }
        tx.execute(
            "INSERT INTO task_events (task_id, branch_id, event_type, created_at) \
             VALUES (?1, ?2, ?3, ?4)",
            params![
                branch.task_id,
                branch.id,
                TaskEventType::SessionCleared.to_string(),
                now,
            ],
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)
    }
}

// ============================================================================
// QueuedMessageRepository
// ============================================================================

/// 任务级待发送队列。消息在真正分发到运行时前不会写入会话 JSONL。
pub struct QueuedMessageRepository<'a> {
    db: &'a Database,
}

impl<'a> QueuedMessageRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn enqueue(&self, message: &QueuedMessage) -> Result<(), ProductError> {
        let mut conn = self.db.conn()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_err)?;
        let sort_order: i64 = if message.priority > 0 {
            tx.query_row(
                "SELECT COALESCE(MIN(sort_order), 0) - 1 FROM queued_messages \
                 WHERE task_id = ?1 AND branch_id = ?2 \
                   AND state IN ('queued', 'dispatching', 'failed')",
                params![message.task_id, message.branch_id],
                |row| row.get(0),
            )
            .map_err(db_err)?
        } else {
            tx.query_row(
                "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM queued_messages \
                 WHERE task_id = ?1 AND branch_id = ?2 \
                   AND state IN ('queued', 'dispatching', 'failed')",
                params![message.task_id, message.branch_id],
                |row| row.get(0),
            )
            .map_err(db_err)?
        };
        tx.execute(
            "INSERT INTO queued_messages \
             (id, task_id, branch_id, message, state, priority, attachments_json, sort_order, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                message.id,
                message.task_id,
                message.branch_id,
                message.message,
                message.state.to_string(),
                message.priority,
                message.attachments_json,
                sort_order,
                message.created_at.to_rfc3339(),
                message.updated_at.to_rfc3339(),
            ],
        )
        .map_err(db_err)?;
        tx.commit().map_err(db_err)
    }

    /// 只列出当前用户仍可操作或需要看到的排队项目。
    pub fn list_pending(
        &self,
        task_id: &str,
        branch_id: &str,
    ) -> Result<Vec<QueuedMessage>, ProductError> {
        let conn = self.db.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, task_id, branch_id, message, state, priority, attachments_json, created_at, updated_at \
                 FROM queued_messages \
                 WHERE task_id = ?1 AND branch_id = ?2 \
                   AND state IN ('queued', 'dispatching', 'failed') \
                 ORDER BY sort_order ASC, priority DESC, created_at ASC, id ASC",
            )
            .map_err(db_err)?;
        let mut rows = stmt.query(params![task_id, branch_id]).map_err(db_err)?;
        let mut messages = Vec::new();
        while let Some(row) = rows.next().map_err(db_err)? {
            messages.push(row_to_queued_message(row)?);
        }
        Ok(messages)
    }

    /// Lists tasks that still have durable queued work. Startup uses this projection to resume
    /// delivery without loading every task or competing with task-local dispatchers.
    pub fn list_queued_task_ids(&self) -> Result<Vec<String>, ProductError> {
        let conn = self.db.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT DISTINCT task_id FROM queued_messages WHERE state = 'queued' \
                 ORDER BY task_id ASC",
            )
            .map_err(db_err)?;
        let mut rows = stmt.query([]).map_err(db_err)?;
        let mut task_ids = Vec::new();
        while let Some(row) = rows.next().map_err(db_err)? {
            task_ids.push(row.get(0).map_err(db_err)?);
        }
        Ok(task_ids)
    }

    /// 取得全局调度器下一条消息，并把它原子地标记为 dispatching。
    pub fn take_next(&self) -> Result<Option<QueuedMessage>, ProductError> {
        self.take_next_matching(None)
    }

    /// 取得指定任务的下一条消息，并把它原子地标记为 dispatching。
    ///
    /// Native runtime 按任务隔离后，每个完成回调只能推进自己的队列；否则一个
    /// 快任务可能抢走仍在运行任务的消息，造成同一会话并发和上下文串线。
    pub fn take_next_for_task(&self, task_id: &str) -> Result<Option<QueuedMessage>, ProductError> {
        self.take_next_matching(Some(task_id))
    }

    fn take_next_matching(
        &self,
        task_id: Option<&str>,
    ) -> Result<Option<QueuedMessage>, ProductError> {
        let mut conn = self.db.conn()?;
        // Acquire the writer reservation before selecting a candidate. A deferred transaction
        // can read a snapshot, race a queue reorder commit, and then fail its write upgrade with
        // SQLITE_BUSY_SNAPSHOT, leaving an idle task with work that is never dispatched.
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_err)?;
        let candidate = if let Some(task_id) = task_id {
            let mut stmt = tx
                .prepare(
                    "SELECT id, task_id, branch_id, message, state, priority, attachments_json, created_at, updated_at \
                     FROM queued_messages WHERE state = 'queued' AND task_id = ?1 \
                     ORDER BY sort_order ASC, priority DESC, created_at ASC, id ASC LIMIT 1",
                )
                .map_err(db_err)?;
            let mut rows = stmt.query(params![task_id]).map_err(db_err)?;
            match rows.next().map_err(db_err)? {
                Some(row) => Some(row_to_queued_message(row)?),
                None => None,
            }
        } else {
            let mut stmt = tx
                .prepare(
                    "SELECT id, task_id, branch_id, message, state, priority, attachments_json, created_at, updated_at \
                     FROM queued_messages WHERE state = 'queued' \
                     ORDER BY priority DESC, created_at ASC, id ASC LIMIT 1",
                )
                .map_err(db_err)?;
            let mut rows = stmt.query([]).map_err(db_err)?;
            match rows.next().map_err(db_err)? {
                Some(row) => Some(row_to_queued_message(row)?),
                None => None,
            }
        };
        let Some(mut message) = candidate else {
            tx.commit().map_err(db_err)?;
            return Ok(None);
        };
        let changed = tx
            .execute(
                "UPDATE queued_messages SET state = 'dispatching', updated_at = ?1 \
                 WHERE id = ?2 AND state = 'queued'",
                params![Utc::now().to_rfc3339(), message.id],
            )
            .map_err(db_err)?;
        if changed == 0 {
            tx.commit().map_err(db_err)?;
            return Ok(None);
        }
        tx.commit().map_err(db_err)?;
        message.state = QueuedMessageState::Dispatching;
        message.updated_at = Utc::now();
        Ok(Some(message))
    }

    /// Replace the exact pending order for one visible task branch.
    ///
    /// The complete queued ID set is required so a drag made against a stale UI cannot
    /// silently overwrite a message that arrived or started dispatching in the meantime.
    pub fn reorder_pending(
        &self,
        task_id: &str,
        branch_id: &str,
        ordered_ids: &[String],
    ) -> Result<(), ProductError> {
        let mut conn = self.db.conn()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_err)?;
        let current_ids = {
            let mut statement = tx
                .prepare(
                    "SELECT id FROM queued_messages \
                     WHERE task_id = ?1 AND branch_id = ?2 AND state = 'queued' \
                     ORDER BY sort_order ASC, priority DESC, created_at ASC, id ASC",
                )
                .map_err(db_err)?;
            let ids = statement
                .query_map(params![task_id, branch_id], |row| row.get::<_, String>(0))
                .map_err(db_err)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(db_err)?;
            ids
        };
        let mut expected = current_ids;
        let mut requested = ordered_ids.to_vec();
        expected.sort();
        requested.sort();
        if expected != requested || requested.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(ProductError::Other(
                "待发送队列已经变化，请刷新后重试排序".to_string(),
            ));
        }

        let now = Utc::now().to_rfc3339();
        for (index, id) in ordered_ids.iter().enumerate() {
            let sort_order = i64::try_from(index)
                .map_err(|_| ProductError::Other("待发送队列过长，无法排序".to_string()))?;
            let changed = tx
                .execute(
                    "UPDATE queued_messages SET sort_order = ?1, updated_at = ?2 \
                     WHERE id = ?3 AND task_id = ?4 AND branch_id = ?5 AND state = 'queued'",
                    params![sort_order, now, id, task_id, branch_id],
                )
                .map_err(db_err)?;
            if changed != 1 {
                return Err(ProductError::Other(
                    "待发送队列已经变化，请刷新后重试排序".to_string(),
                ));
            }
        }
        tx.commit().map_err(db_err)
    }

    /// 编辑一条仍可操作的队列消息。失败项在用户修正后重新进入等待状态；已经开始
    /// 分发的消息不会被改写，避免界面编辑与调度器竞争时出现“显示新文本、执行旧文本”。
    pub fn update_pending_message(
        &self,
        id: &str,
        task_id: &str,
        branch_id: &str,
        message: &str,
    ) -> Result<(), ProductError> {
        let message = message.trim();
        if message.is_empty() {
            return Err(ProductError::Other("队列消息不能为空".to_string()));
        }
        let conn = self.db.conn()?;
        let changed = conn
            .execute(
                "UPDATE queued_messages \
                 SET message = ?1, state = 'queued', updated_at = ?2 \
                 WHERE id = ?3 AND task_id = ?4 AND branch_id = ?5 \
                   AND state IN ('queued', 'failed')",
                params![message, Utc::now().to_rfc3339(), id, task_id, branch_id,],
            )
            .map_err(db_err)?;
        if changed != 1 {
            return Err(ProductError::Other(
                "这条消息已经开始处理或不在当前队列中".to_string(),
            ));
        }
        Ok(())
    }

    /// 原子认领用户点选的队列项，用于将指定消息引导进当前运行。
    ///
    /// 与 `take_next_for_task` 不同，这里只认领给定 ID，并同时校验任务、分支和状态；
    /// 因此不会误取队首，也不会把旧分支的文本注入当前会话。
    pub fn claim_pending_for_task_branch(
        &self,
        id: &str,
        task_id: &str,
        branch_id: &str,
    ) -> Result<Option<QueuedMessage>, ProductError> {
        let mut conn = self.db.conn()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_err)?;
        let candidate = {
            let mut statement = tx
                .prepare(
                    "SELECT id, task_id, branch_id, message, state, priority, attachments_json, created_at, updated_at \
                     FROM queued_messages \
                     WHERE id = ?1 AND task_id = ?2 AND branch_id = ?3 AND state = 'queued'",
                )
                .map_err(db_err)?;
            let mut rows = statement
                .query(params![id, task_id, branch_id])
                .map_err(db_err)?;
            match rows.next().map_err(db_err)? {
                Some(row) => Some(row_to_queued_message(row)?),
                None => None,
            }
        };
        let Some(mut message) = candidate else {
            tx.commit().map_err(db_err)?;
            return Ok(None);
        };
        let now = Utc::now();
        let changed = tx
            .execute(
                "UPDATE queued_messages SET state = 'dispatching', updated_at = ?1 \
                 WHERE id = ?2 AND task_id = ?3 AND branch_id = ?4 AND state = 'queued'",
                params![now.to_rfc3339(), id, task_id, branch_id],
            )
            .map_err(db_err)?;
        if changed != 1 {
            tx.commit().map_err(db_err)?;
            return Ok(None);
        }
        tx.commit().map_err(db_err)?;
        message.state = QueuedMessageState::Dispatching;
        message.updated_at = now;
        Ok(Some(message))
    }

    /// 当前运行无法接纳引导时，把已经认领的消息还原到队首。只移动该消息，其他
    /// 队列项的 `sort_order` 完全不变，从而保留用户此前的拖拽顺序。
    pub fn restore_claim_to_front(
        &self,
        id: &str,
        task_id: &str,
        branch_id: &str,
    ) -> Result<(), ProductError> {
        let mut conn = self.db.conn()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_err)?;
        let sort_order: i64 = tx
            .query_row(
                "SELECT COALESCE(MIN(sort_order), 0) - 1 FROM queued_messages \
                 WHERE task_id = ?1 AND branch_id = ?2 AND id <> ?3 \
                   AND state IN ('queued', 'dispatching', 'failed')",
                params![task_id, branch_id, id],
                |row| row.get(0),
            )
            .map_err(db_err)?;
        let changed = tx
            .execute(
                "UPDATE queued_messages \
                 SET state = 'queued', sort_order = ?1, updated_at = ?2 \
                 WHERE id = ?3 AND task_id = ?4 AND branch_id = ?5 AND state = 'dispatching'",
                params![sort_order, Utc::now().to_rfc3339(), id, task_id, branch_id,],
            )
            .map_err(db_err)?;
        if changed != 1 {
            return Err(ProductError::Other(
                "这条消息的队列状态已经变化".to_string(),
            ));
        }
        tx.commit().map_err(db_err)
    }

    /// 将成功引导进当前运行的认领项移出可见队列。
    pub fn complete_claim_for_task_branch(
        &self,
        id: &str,
        task_id: &str,
        branch_id: &str,
    ) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        let changed = conn
            .execute(
                "UPDATE queued_messages SET state = 'sent', updated_at = ?1 \
                 WHERE id = ?2 AND task_id = ?3 AND branch_id = ?4 AND state = 'dispatching'",
                params![Utc::now().to_rfc3339(), id, task_id, branch_id],
            )
            .map_err(db_err)?;
        if changed != 1 {
            return Err(ProductError::Other(
                "这条消息的队列状态已经变化".to_string(),
            ));
        }
        Ok(())
    }

    pub fn set_state(&self, id: &str, state: QueuedMessageState) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE queued_messages SET state = ?1, updated_at = ?2 WHERE id = ?3",
            params![state.to_string(), Utc::now().to_rfc3339(), id],
        )
        .map_err(db_err)?;
        Ok(())
    }

    pub fn cancel(&self, id: &str) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE queued_messages SET state = 'cancelled', updated_at = ?1 \
             WHERE id = ?2 AND state IN ('queued', 'dispatching', 'failed')",
            params![Utc::now().to_rfc3339(), id],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 仅允许取消当前任务和分支可见的队列项，避免跨会话误操作。
    pub fn cancel_for_task_branch(
        &self,
        id: &str,
        task_id: &str,
        branch_id: &str,
    ) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE queued_messages SET state = 'cancelled', updated_at = ?1 \
             WHERE id = ?2 AND task_id = ?3 AND branch_id = ?4 \
               AND state IN ('queued', 'dispatching', 'failed')",
            params![Utc::now().to_rfc3339(), id, task_id, branch_id],
        )
        .map_err(db_err)?;
        Ok(())
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

    /// Retry files left after a committed deletion whose best-effort disk cleanup was blocked.
    /// Only exact lowercase BLAKE3 filenames without a matching ledger row are touched.
    pub fn prune_unreferenced_files(
        &self,
    ) -> Result<crate::lifecycle_purge::AppDataPruneReport, ProductError> {
        crate::lifecycle_purge::prune_unreferenced_blob_files(self.db, &self.blobs_dir)
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

    /// 按 canonical path 插入或更新 Workspace，并返回数据库中的最终所有者记录。
    ///
    /// 冲突分支不会写 `id`、`memory_mode` 或 `memory_generation`，因此并发首次打开
    /// 的失败竞争者也会拿到赢家已经持久化的稳定 id。
    pub fn upsert_and_get(
        &self,
        candidate_id: &str,
        canonical_path: &str,
        display_name: &str,
        access_mode: ProjectAccessMode,
        last_opened_at: DateTime<Utc>,
    ) -> Result<Workspace, ProductError> {
        let conn = self.db.conn()?;
        let mut statement = conn
            .prepare(
                "INSERT INTO workspaces (id, canonical_path, display_name, access_mode, last_opened_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(canonical_path) DO UPDATE SET \
                     display_name = excluded.display_name, \
                     access_mode = excluded.access_mode, \
                     last_opened_at = excluded.last_opened_at \
                 RETURNING id, canonical_path, display_name, access_mode, last_opened_at, \
                           memory_mode, memory_generation",
            )
            .map_err(db_err)?;
        let mut rows = statement
            .query(params![
                candidate_id,
                canonical_path,
                display_name,
                access_mode_str(access_mode),
                last_opened_at.to_rfc3339(),
            ])
            .map_err(db_err)?;
        match rows.next().map_err(db_err)? {
            Some(row) => row_to_workspace(row),
            None => Err(ProductError::DatabaseError(
                "workspace upsert returned no row".into(),
            )),
        }
    }

    /// Atomically register a workspace and bind it to an as-yet unscoped task.
    /// If the task update fails, the workspace upsert rolls back with it, so callers never
    /// observe a project in the rail without the conversation that selected it being attached.
    pub fn upsert_and_attach_task_once(
        &self,
        task_id: &str,
        canonical_path: &str,
        display_name: &str,
    ) -> Result<(Workspace, DateTime<Utc>), ProductError> {
        let mut conn = self.db.conn()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(db_err)?;
        let now = Utc::now();
        let candidate_id = Uuid::new_v4().simple().to_string();
        let workspace = {
            let mut statement = tx
                .prepare(
                    "INSERT INTO workspaces \
                     (id, canonical_path, display_name, access_mode, last_opened_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5) \
                     ON CONFLICT(canonical_path) DO UPDATE SET \
                         display_name = excluded.display_name, \
                         last_opened_at = excluded.last_opened_at \
                     RETURNING id, canonical_path, display_name, access_mode, last_opened_at, \
                               memory_mode, memory_generation",
                )
                .map_err(db_err)?;
            let mut rows = statement
                .query(params![
                    candidate_id,
                    canonical_path,
                    display_name,
                    access_mode_str(ProjectAccessMode::RequestApproval),
                    now.to_rfc3339(),
                ])
                .map_err(db_err)?;
            match rows.next().map_err(db_err)? {
                Some(row) => row_to_workspace(row)?,
                None => {
                    return Err(ProductError::DatabaseError(
                        "workspace attach upsert returned no row".into(),
                    ));
                }
            }
        };
        let changed = tx
            .execute(
                "UPDATE tasks SET workspace_path = ?1, updated_at = ?2 \
                 WHERE id = ?3 AND workspace_path IS NULL",
                params![canonical_path, now.to_rfc3339(), task_id],
            )
            .map_err(db_err)?;
        if changed != 1 {
            return Err(ProductError::StateMachineError(
                "task disappeared or already has a workspace while attaching".to_string(),
            ));
        }
        tx.commit().map_err(db_err)?;
        Ok((workspace, now))
    }

    /// 兼容需要提交完整 DTO 的调用方；稳定字段仍由数据库冲突分支保护。
    pub fn upsert(&self, ws: &Workspace) -> Result<(), ProductError> {
        self.upsert_and_get(
            &ws.id,
            &ws.canonical_path,
            &ws.display_name,
            ws.access_mode,
            ws.last_opened_at,
        )?;
        Ok(())
    }

    /// 按 canonical path 获取 Workspace。
    pub fn get(&self, canonical_path: &str) -> Result<Option<Workspace>, ProductError> {
        let conn = self.db.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, canonical_path, display_name, access_mode, last_opened_at, \
                        memory_mode, memory_generation \
                 FROM workspaces WHERE canonical_path = ?1",
            )
            .map_err(db_err)?;
        let mut rows = stmt.query(params![canonical_path]).map_err(db_err)?;
        match rows.next().map_err(db_err)? {
            Some(row) => Ok(Some(row_to_workspace(row)?)),
            None => Ok(None),
        }
    }

    /// 按稳定 owner id 获取 Workspace。
    pub fn get_by_id(&self, id: &str) -> Result<Option<Workspace>, ProductError> {
        let conn = self.db.conn()?;
        let mut statement = conn
            .prepare(
                "SELECT id, canonical_path, display_name, access_mode, last_opened_at, \
                        memory_mode, memory_generation \
                 FROM workspaces WHERE id = ?1",
            )
            .map_err(db_err)?;
        let mut rows = statement.query(params![id]).map_err(db_err)?;
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
                "SELECT id, canonical_path, display_name, access_mode, last_opened_at, \
                        memory_mode, memory_generation \
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

    /// 从 R-Code 中清除一个 Workspace 及其关联产品记录。
    ///
    /// `tasks` 的关联审计表、Blob 引用与 Plan 投影由统一生命周期服务清理；通知按
    /// `workspace_path` 显式清理。工作区磁盘目录永远不进入删除路径。
    /// 返回 `(是否存在并清除了 Workspace, 清除的会话数)`。
    pub fn remove(
        &self,
        canonical_path: &str,
        blobs_dir: &Path,
        projection_root: &Path,
    ) -> Result<(bool, usize), ProductError> {
        let result =
            crate::lifecycle_purge::LifecyclePurgeStore::new(self.db, blobs_dir, projection_root)
                .purge_workspace(canonical_path)?;
        for warning in result.cleanup_warnings {
            tracing::warn!(workspace_path = canonical_path, %warning, "workspace AppData cleanup will retry at startup");
        }
        Ok((result.workspace_removed, result.removed_tasks))
    }

    /// 更新项目级 Agent 权限模式。
    pub fn update_access_mode(
        &self,
        canonical_path: &str,
        access_mode: ProjectAccessMode,
    ) -> Result<(), ProductError> {
        let conn = self.db.conn()?;
        conn.execute(
            "UPDATE workspaces SET access_mode = ?1 WHERE canonical_path = ?2",
            params![access_mode_str(access_mode), canonical_path],
        )
        .map_err(db_err)?;
        Ok(())
    }

    /// 以 generation CAS 更新项目记忆模式，并返回递增后的 generation。
    pub fn update_memory_mode(
        &self,
        id: &str,
        expected_generation: u64,
        memory_mode: WorkspaceMemoryMode,
    ) -> Result<u64, ProductError> {
        let expected_generation = i64::try_from(expected_generation).map_err(|_| {
            ProductError::DatabaseError("workspace memory_generation exceeds SQLite range".into())
        })?;
        let conn = self.db.conn()?;
        let next: Option<i64> = conn
            .query_row(
                "UPDATE workspaces \
                 SET memory_mode = ?1, memory_generation = memory_generation + 1 \
                 WHERE id = ?2 AND memory_generation = ?3 \
                 RETURNING memory_generation",
                params![
                    workspace_memory_mode_str(memory_mode),
                    id,
                    expected_generation
                ],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_err)?;
        let next = next.ok_or_else(|| {
            ProductError::DatabaseError(format!(
                "workspace memory mode conflict for id {id} at generation {expected_generation}"
            ))
        })?;
        u64::try_from(next).map_err(|_| {
            ProductError::DatabaseError("workspace memory_generation must be non-negative".into())
        })
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
    use r_code_core::dto::{
        ProjectAccessMode, ReviewState, TaskEventType, TaskMode, TaskState, WorkspaceMemoryMode,
    };
    use std::collections::BTreeSet;
    use std::sync::{Arc, Barrier};
    use tempfile::TempDir;

    /// 创建内存数据库用于测试。
    fn setup_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    /// 创建测试任务并持久化。
    fn create_test_task(db: &Database, workspace_path: &str, title: &str) -> Task {
        let repo = TaskRepository::new(db);
        let task = Task::new(
            Some(workspace_path.to_string()),
            title,
            "test goal",
            TaskMode::Edit,
        );
        repo.create(&task).unwrap();
        task
    }

    fn assert_project_conversation_limit(error: &ProductError) {
        match error {
            ProductError::ProjectConversationLimitReached { limit } => {
                assert_eq!(*limit, MAX_PROJECT_CONVERSATIONS);
            }
            other => panic!("expected project conversation limit error, got {other:?}"),
        }
        assert_eq!(error.to_string(), PROJECT_CONVERSATION_LIMIT_REACHED);
    }

    // --------------------------------------------------------------------------
    // TaskRepository 测试
    // --------------------------------------------------------------------------

    #[test]
    fn test_task_create_and_get() {
        let db = setup_db();
        let repo = TaskRepository::new(&db);
        let task = Task::new(Some("/proj".into()), "My Task", "Do stuff", TaskMode::Auto);
        repo.create(&task).unwrap();

        let fetched = repo.get(&task.id).unwrap().unwrap();
        assert_eq!(fetched.id, task.id);
        assert_eq!(fetched.workspace_path.as_deref(), Some("/proj"));
        assert_eq!(fetched.title, "My Task");
        assert_eq!(fetched.goal, "Do stuff");
        assert!(!fetched.goal_active);
        assert_eq!(fetched.mode, TaskMode::Auto);
        assert_eq!(fetched.state, TaskState::Idle);
        assert!(fetched.worktree_path.is_none());
        assert_eq!(fetched.created_at, task.created_at);
        assert_eq!(fetched.updated_at, task.updated_at);
    }

    #[test]
    fn project_conversations_are_named_capped_and_reopen_after_archive() {
        let db = setup_db();
        let repo = TaskRepository::new(&db);
        let mut created = Vec::new();

        for expected in ["新对话", "新对话 2", "新对话 3", "新对话 4", "新对话 5"] {
            let mut task = Task::new(Some("/proj".into()), "ignored", "", TaskMode::Edit);
            let branch = repo.create_project_conversation(&mut task).unwrap();
            assert_eq!(task.title, expected);
            assert_eq!(branch.task_id, task.id);
            assert_eq!(branch.id, "main");
            created.push(task);
        }

        let mut blocked = Task::new(Some("/proj".into()), "ignored", "", TaskMode::Edit);
        let error = repo.create_project_conversation(&mut blocked).unwrap_err();
        assert_project_conversation_limit(&error);

        repo.update_state(&created[0].id, TaskState::Archived)
            .unwrap();
        let mut replacement = Task::new(Some("/proj".into()), "ignored", "", TaskMode::Edit);
        repo.create_project_conversation(&mut replacement).unwrap();
        assert_eq!(replacement.title, "新对话 6");
        assert_eq!(repo.list(Some("/proj"), None, false).unwrap().len(), 5);

        let events = TaskEventStore::new(&db)
            .list_by_task(&replacement.id, None, None)
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, TaskEventType::TaskCreated);
    }

    #[test]
    fn generic_create_caps_each_workspace_without_limiting_standalone_tasks() {
        let db = setup_db();
        let repo = TaskRepository::new(&db);
        let mut project_a = Vec::new();

        for index in 1..=MAX_PROJECT_CONVERSATIONS {
            let task = Task::new(
                Some("/proj-a".into()),
                format!("Project A {index}"),
                "",
                TaskMode::Edit,
            );
            repo.create(&task).unwrap();
            project_a.push(task);
        }
        let blocked_a = Task::new(
            Some("/proj-a".into()),
            "Project A blocked",
            "",
            TaskMode::Edit,
        );
        assert_project_conversation_limit(&repo.create(&blocked_a).unwrap_err());

        for index in 1..=MAX_PROJECT_CONVERSATIONS {
            let task = Task::new(
                Some("/proj-b".into()),
                format!("Project B {index}"),
                "",
                TaskMode::Edit,
            );
            repo.create(&task).unwrap();
        }
        let blocked_b = Task::new(
            Some("/proj-b".into()),
            "Project B blocked",
            "",
            TaskMode::Edit,
        );
        assert_project_conversation_limit(&repo.create(&blocked_b).unwrap_err());

        for index in 1..=(MAX_PROJECT_CONVERSATIONS + 2) {
            let task = Task::new(None, format!("Standalone {index}"), "", TaskMode::Ask);
            repo.create(&task).unwrap();
        }

        repo.update_state(&project_a[0].id, TaskState::Archived)
            .unwrap();
        let replacement = Task::new(
            Some("/proj-a".into()),
            "Project A replacement",
            "",
            TaskMode::Edit,
        );
        repo.create(&replacement).unwrap();
        assert_eq!(
            repo.list(Some("/proj-a"), None, false).unwrap().len(),
            MAX_PROJECT_CONVERSATIONS,
        );
        assert_eq!(
            repo.list(Some("/proj-b"), None, false).unwrap().len(),
            MAX_PROJECT_CONVERSATIONS,
        );
    }

    #[test]
    fn concurrent_generic_creation_never_exceeds_project_limit() {
        let directory = TempDir::new().unwrap();
        let db = Arc::new(Database::open(directory.path().join("generic-create.db")).unwrap());
        let workers = MAX_PROJECT_CONVERSATIONS * 3;
        let barrier = Arc::new(Barrier::new(workers));
        let handles: Vec<_> = (0..workers)
            .map(|index| {
                let db = db.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let task = Task::new(
                        Some("/proj".into()),
                        format!("Generic {index}"),
                        "",
                        TaskMode::Edit,
                    );
                    barrier.wait();
                    TaskRepository::new(db.as_ref()).create(&task).map(|_| task)
                })
            })
            .collect();

        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert_eq!(
            results.iter().filter(|result| result.is_ok()).count(),
            MAX_PROJECT_CONVERSATIONS,
        );
        assert_eq!(
            results.iter().filter(|result| result.is_err()).count(),
            workers - MAX_PROJECT_CONVERSATIONS,
        );
        for error in results.iter().filter_map(|result| result.as_ref().err()) {
            assert_project_conversation_limit(error);
        }
        assert_eq!(
            TaskRepository::new(db.as_ref())
                .list(Some("/proj"), None, false)
                .unwrap()
                .len(),
            MAX_PROJECT_CONVERSATIONS,
        );
    }

    #[test]
    fn generic_and_specialized_creation_share_the_last_atomic_slot() {
        let directory = TempDir::new().unwrap();
        let db = Arc::new(Database::open(directory.path().join("mixed-create.db")).unwrap());
        let repo = TaskRepository::new(db.as_ref());
        for index in 1..MAX_PROJECT_CONVERSATIONS {
            let task = Task::new(
                Some("/proj".into()),
                format!("Existing {index}"),
                "",
                TaskMode::Edit,
            );
            repo.create(&task).unwrap();
        }

        let barrier = Arc::new(Barrier::new(2));
        let generic = {
            let db = db.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let task = Task::new(
                    Some("/proj".into()),
                    "Generic contender",
                    "",
                    TaskMode::Edit,
                );
                barrier.wait();
                TaskRepository::new(db.as_ref())
                    .create(&task)
                    .map(|_| "generic")
            })
        };
        let specialized = {
            let db = db.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let mut task = Task::new(Some("/proj".into()), "ignored", "", TaskMode::Edit);
                barrier.wait();
                TaskRepository::new(db.as_ref())
                    .create_project_conversation(&mut task)
                    .map(|_| "specialized")
            })
        };

        let results = [generic.join().unwrap(), specialized.join().unwrap()];
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let error = results
            .iter()
            .find_map(|result| result.as_ref().err())
            .expect("one contender must lose the final slot");
        assert_project_conversation_limit(error);
        assert_eq!(
            TaskRepository::new(db.as_ref())
                .list(Some("/proj"), None, false)
                .unwrap()
                .len(),
            MAX_PROJECT_CONVERSATIONS,
        );
    }

    #[test]
    fn concurrent_project_conversation_creation_never_exceeds_limit_or_reuses_titles() {
        let directory = TempDir::new().unwrap();
        let db = Arc::new(Database::open(directory.path().join("conversations.db")).unwrap());
        let workers = MAX_PROJECT_CONVERSATIONS * 3;
        let barrier = Arc::new(Barrier::new(workers));
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                let db = db.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let mut task = Task::new(Some("/proj".into()), "ignored", "", TaskMode::Edit);
                    barrier.wait();
                    TaskRepository::new(db.as_ref())
                        .create_project_conversation(&mut task)
                        .map(|_| task)
                })
            })
            .collect();

        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        let titles: BTreeSet<_> = results
            .iter()
            .filter_map(|result| result.as_ref().ok().map(|task| task.title.clone()))
            .collect();
        assert_eq!(titles.len(), MAX_PROJECT_CONVERSATIONS);
        assert_eq!(
            titles,
            ["新对话", "新对话 2", "新对话 3", "新对话 4", "新对话 5"]
                .into_iter()
                .map(str::to_string)
                .collect(),
        );
        for error in results.iter().filter_map(|result| result.as_ref().err()) {
            assert_project_conversation_limit(error);
        }

        let visible = TaskRepository::new(db.as_ref())
            .list(Some("/proj"), None, false)
            .unwrap();
        assert_eq!(visible.len(), MAX_PROJECT_CONVERSATIONS);
        let conn = db.conn().unwrap();
        let branch_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM session_branches", [], |row| {
                row.get(0)
            })
            .unwrap();
        let event_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM task_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(branch_count, MAX_PROJECT_CONVERSATIONS as i64);
        assert_eq!(event_count, MAX_PROJECT_CONVERSATIONS as i64);
    }

    #[test]
    fn test_task_inference_round_trip() {
        let db = setup_db();
        let repo = TaskRepository::new(&db);
        let task = Task::new(Some("/proj".into()), "Inference", "Run", TaskMode::Auto);
        repo.create(&task).unwrap();
        let inference = hermes_core::InferenceOptions {
            thinking: Some("enabled".into()),
            reasoning_effort: Some("high".into()),
            verbosity: Some("low".into()),
        };

        repo.set_inference(&task.id, &inference).unwrap();

        let fetched = repo.get(&task.id).unwrap().unwrap();
        assert_eq!(fetched.inference, inference);
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
        assert!(proj_a
            .iter()
            .all(|t| t.workspace_path.as_deref() == Some("/proj-a")));

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
        assert!(fetched.goal_active);
        assert_eq!(fetched.worktree_path.as_deref(), Some("/worktree/path"));
    }

    #[test]
    fn test_task_goal_activation_and_clear_are_atomic() {
        let db = setup_db();
        let repo = TaskRepository::new(&db);
        let task = create_test_task(&db, "/proj", "Goal lifecycle");

        assert!(!repo.get(&task.id).unwrap().unwrap().goal_active);

        repo.set_goal(&task.id, "  Explicit goal  ").unwrap();
        let active = repo.get(&task.id).unwrap().unwrap();
        assert_eq!(active.goal, "Explicit goal");
        assert!(active.goal_active);

        repo.set_goal(&task.id, "   ").unwrap();
        let cleared = repo.get(&task.id).unwrap().unwrap();
        assert_eq!(cleared.goal, "");
        assert!(!cleared.goal_active);
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
        assert_eq!(fetched.branch_id, "main");
        assert_eq!(fetched.agent_kind, AgentKind::Main);
        assert!(fetched.parent_run_id.is_none());
        assert!(fetched.agent_label.is_none());
        assert!(fetched.delegated_by_tool_call_id.is_none());
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
    fn test_subagent_run_roundtrip_and_list_by_parent() {
        let db = setup_db();
        let task = create_test_task(&db, "/proj", "Delegation Tree");
        let repo = AgentRunRepository::new(&db);
        let parent = AgentRun::new(&task.id, "parent-model");
        repo.create(&parent).unwrap();

        let delegated_by_tool_call_id = "delegate-call";
        let conn = db.conn().unwrap();
        conn.execute(
            "INSERT INTO tool_calls \
             (id, run_id, task_id, tool_name, input_json, risk_level, started_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                delegated_by_tool_call_id,
                parent.id,
                task.id,
                "delegate",
                "{}",
                "R0",
                Utc::now().to_rfc3339(),
            ],
        )
        .unwrap();
        drop(conn);

        let mut child = AgentRun::new_subagent_for_branch(
            &task.id,
            "branch-1",
            &parent.id,
            "child-model",
            Some("检索代理".to_string()),
            Some(delegated_by_tool_call_id.to_string()),
        );
        child.access_mode = SubagentAccessMode::FullAccess;
        child.require_approval = true;
        child.routing_reason = Some("父运行要求写操作逐项审批".to_string());
        repo.create(&child).unwrap();

        let fetched = repo.get(&child.id).unwrap().unwrap();
        assert_eq!(fetched, child);

        let children = repo.list_by_parent_run_id(&parent.id).unwrap();
        assert_eq!(children, vec![child]);
        assert!(repo
            .list_by_parent_run_id("unknown-parent")
            .unwrap()
            .is_empty());

        let active = repo.get_active_run(&task.id).unwrap().unwrap();
        assert_eq!(active.id, parent.id);
        let active_main_branch = repo
            .get_active_run_for_branch(&task.id, "main")
            .unwrap()
            .unwrap();
        assert_eq!(active_main_branch.id, parent.id);
    }

    #[test]
    fn test_external_agent_run_roundtrip_preserves_runtime_and_session_id() {
        let db = setup_db();
        let task = create_test_task(&db, "/proj", "External Delegation");
        let repo = AgentRunRepository::new(&db);
        let parent = AgentRun::new(&task.id, "parent-model");
        repo.create(&parent).unwrap();

        let child = AgentRun::new_subagent_for_branch(
            &task.id,
            "main",
            &parent.id,
            "codex-cli",
            Some("Codex CLI · 只读调查".to_string()),
            None,
        )
        .as_codex_exec_subagent();
        repo.create(&child).unwrap();
        repo.set_external_session_id(&child.id, Some("thread-123"))
            .unwrap();

        let fetched = repo.get(&child.id).unwrap().unwrap();
        assert_eq!(
            fetched.runtime_kind,
            r_code_core::dto::AgentRunRuntimeKind::CodexExec
        );
        assert_eq!(fetched.external_session_id.as_deref(), Some("thread-123"));
        assert_eq!(fetched.agent_kind, AgentKind::Subagent);
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

    /// 回归：首次结束时刻不可变。审查动作发生在 run 结束之后，
    /// 若二次覆写会把审计里的"运行结束时间"改成"用户点按钮的时间"。
    #[test]
    fn test_update_review_state_preserves_first_ended_at() {
        let db = setup_db();
        let task = create_test_task(&db, "/proj", "Ended At Guard");
        let repo = AgentRunRepository::new(&db);

        let run = AgentRun::new(&task.id, "gpt-4");
        repo.create(&run).unwrap();

        // 模拟 drain loop 收尾
        repo.update_review_state(&run.id, ReviewState::Pending)
            .unwrap();
        let first_ended = repo.get(&run.id).unwrap().unwrap().ended_at.unwrap();

        // 稍后用户点接受
        std::thread::sleep(std::time::Duration::from_millis(20));
        repo.update_review_state(&run.id, ReviewState::Accepted)
            .unwrap();

        let after = repo.get(&run.id).unwrap().unwrap();
        assert_eq!(after.review_state, ReviewState::Accepted);
        assert_eq!(after.ended_at.unwrap(), first_ended);
    }

    #[test]
    fn finish_if_active_claims_the_terminal_transition_once() {
        let db = setup_db();
        let task = create_test_task(&db, "/proj", "Atomic Run Finish");
        let repo = AgentRunRepository::new(&db);
        let run = AgentRun::new(&task.id, "gpt-4");
        repo.create(&run).unwrap();

        assert!(repo
            .finish_if_active(&run.id, ReviewState::Aborted, Some("cleanup fallback"))
            .unwrap());
        assert!(!repo
            .finish_if_active(&run.id, ReviewState::Failed, Some("late duplicate"))
            .unwrap());

        let finished = repo.get(&run.id).unwrap().unwrap();
        assert_eq!(finished.review_state, ReviewState::Aborted);
        assert_eq!(finished.summary.as_deref(), Some("cleanup fallback"));
        assert!(finished.ended_at.is_some());
    }

    /// 回归：run 结束后 get_active_run 是 None，但 get_latest_main_run 仍能找到它。
    /// 这正是 accept_task 曾经必然失败的原因。
    #[test]
    fn test_get_latest_main_run_finds_ended_run() {
        let db = setup_db();
        let task = create_test_task(&db, "/proj", "Latest Main Run");
        let repo = AgentRunRepository::new(&db);

        assert!(repo.get_latest_main_run(&task.id).unwrap().is_none());

        let run = AgentRun::new(&task.id, "gpt-4");
        repo.create(&run).unwrap();
        repo.update_review_state(&run.id, ReviewState::Pending)
            .unwrap();

        assert!(repo.get_active_run(&task.id).unwrap().is_none());
        let latest = repo.get_latest_main_run(&task.id).unwrap().unwrap();
        assert_eq!(latest.id, run.id);
        assert!(latest.ended_at.is_some());
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
    fn test_recent_events_hide_archived_tasks_until_restored() {
        let db = setup_db();
        let task = create_test_task(&db, "/proj", "Archived activity");
        let store = TaskEventStore::new(&db);
        store.append(&task.id, TaskEventType::RunEnded).unwrap();

        assert_eq!(store.list_recent(None, 20).unwrap().len(), 1);
        TaskRepository::new(&db)
            .update_state(&task.id, TaskState::Archived)
            .unwrap();
        assert!(store.list_recent(None, 20).unwrap().is_empty());

        TaskRepository::new(&db)
            .update_state(&task.id, TaskState::Idle)
            .unwrap();
        assert_eq!(store.list_recent(None, 20).unwrap().len(), 1);
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
            TaskEventType::UserSteered,
            TaskEventType::UserMessageQueued,
            TaskEventType::QueueDispatched,
            TaskEventType::RunAborted,
            TaskEventType::SessionBranched,
            TaskEventType::SessionCleared,
            TaskEventType::SubagentStarted,
            TaskEventType::SubagentFinished,
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
    // SessionBranchRepository / QueuedMessageRepository 测试
    // --------------------------------------------------------------------------

    #[test]
    fn session_branch_fork_switches_active_branch_without_deleting_main() {
        let db = setup_db();
        let task = create_test_task(&db, "/proj", "branch");
        let repo = SessionBranchRepository::new(&db);
        let main = repo.ensure_active(&task.id).unwrap();
        assert_eq!(main.id, "main");

        let fork = SessionBranch::fork(&task.id, &main.id, "task:2");
        repo.create_fork(&fork).unwrap();

        let active = repo.ensure_active(&task.id).unwrap();
        assert_eq!(active.id, fork.id);
        let branches = repo.list_by_task(&task.id).unwrap();
        assert_eq!(branches.len(), 2);
        assert!(branches.iter().any(|branch| branch.id == "main"));
        assert_eq!(branches.iter().filter(|branch| branch.is_active).count(), 1);
    }

    #[test]
    fn queued_messages_dispatch_in_priority_then_creation_order() {
        let db = setup_db();
        let task = create_test_task(&db, "/proj", "queue");
        let branch = SessionBranchRepository::new(&db)
            .ensure_active(&task.id)
            .unwrap();
        let repo = QueuedMessageRepository::new(&db);
        let normal = QueuedMessage::new(&task.id, &branch.id, "normal", 0);
        let urgent = QueuedMessage::new(&task.id, &branch.id, "urgent", 100);
        repo.enqueue(&normal).unwrap();
        repo.enqueue(&urgent).unwrap();

        let next = repo.take_next().unwrap().unwrap();
        assert_eq!(next.id, urgent.id);
        assert_eq!(next.state, QueuedMessageState::Dispatching);
        repo.set_state(&next.id, QueuedMessageState::Sent).unwrap();

        let pending = repo.list_pending(&task.id, &branch.id).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, normal.id);
    }

    #[test]
    fn queued_message_reorder_is_exact_and_controls_task_dispatch_order() {
        let db = setup_db();
        let task = create_test_task(&db, "/proj", "reorder queue");
        let branch = SessionBranchRepository::new(&db)
            .ensure_active(&task.id)
            .unwrap();
        let repo = QueuedMessageRepository::new(&db);
        let first = QueuedMessage::new(&task.id, &branch.id, "first", 0);
        let second = QueuedMessage::new(&task.id, &branch.id, "second", 0);
        let third = QueuedMessage::new(&task.id, &branch.id, "third", 0);
        repo.enqueue(&first).unwrap();
        repo.enqueue(&second).unwrap();
        repo.enqueue(&third).unwrap();

        assert!(repo
            .reorder_pending(
                &task.id,
                &branch.id,
                &[third.id.clone(), third.id.clone(), first.id.clone()],
            )
            .is_err());
        assert_eq!(
            repo.list_pending(&task.id, &branch.id)
                .unwrap()
                .iter()
                .map(|message| message.id.as_str())
                .collect::<Vec<_>>(),
            vec![first.id.as_str(), second.id.as_str(), third.id.as_str()]
        );

        repo.reorder_pending(
            &task.id,
            &branch.id,
            &[third.id.clone(), first.id.clone(), second.id.clone()],
        )
        .unwrap();
        assert_eq!(
            repo.list_pending(&task.id, &branch.id)
                .unwrap()
                .iter()
                .map(|message| message.id.as_str())
                .collect::<Vec<_>>(),
            vec![third.id.as_str(), first.id.as_str(), second.id.as_str()]
        );

        let claimed = repo.take_next_for_task(&task.id).unwrap().unwrap();
        assert_eq!(claimed.id, third.id);
        repo.set_state(&claimed.id, QueuedMessageState::Sent)
            .unwrap();
        assert_eq!(
            repo.take_next_for_task(&task.id).unwrap().unwrap().id,
            first.id
        );
    }

    #[test]
    fn queue_claim_waits_for_a_concurrent_reorder_writer_then_dispatches() {
        let dir = TempDir::new().unwrap();
        let db = std::sync::Arc::new(Database::open(dir.path().join("queue.db")).unwrap());
        let task = create_test_task(&db, "/proj", "concurrent reorder");
        let branch = SessionBranchRepository::new(&db)
            .ensure_active(&task.id)
            .unwrap();
        let queued = QueuedMessage::new(&task.id, &branch.id, "dispatch me", 0);
        QueuedMessageRepository::new(&db).enqueue(&queued).unwrap();

        let mut reorder_conn = db.conn().unwrap();
        let reorder = reorder_conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        reorder
            .execute(
                "UPDATE queued_messages SET sort_order = sort_order + 1 WHERE id = ?1",
                params![queued.id],
            )
            .unwrap();

        let dispatch_db = db.clone();
        let task_id = task.id.clone();
        let dispatch = std::thread::spawn(move || {
            QueuedMessageRepository::new(&dispatch_db).take_next_for_task(&task_id)
        });
        std::thread::sleep(std::time::Duration::from_millis(75));
        reorder.commit().unwrap();

        let claimed = dispatch.join().unwrap().unwrap().unwrap();
        assert_eq!(claimed.id, queued.id);
        assert_eq!(claimed.state, QueuedMessageState::Dispatching);
    }

    #[test]
    fn queued_message_edit_and_selected_claim_leave_the_other_order_unchanged() {
        let db = setup_db();
        let task = create_test_task(&db, "/proj", "queue actions");
        let branch = SessionBranchRepository::new(&db)
            .ensure_active(&task.id)
            .unwrap();
        let repo = QueuedMessageRepository::new(&db);
        let first = QueuedMessage::new(&task.id, &branch.id, "first", 0);
        let second = QueuedMessage::new(&task.id, &branch.id, "second", 0);
        let third = QueuedMessage::new(&task.id, &branch.id, "third", 0);
        repo.enqueue(&first).unwrap();
        repo.enqueue(&second).unwrap();
        repo.enqueue(&third).unwrap();

        repo.update_pending_message(&second.id, &task.id, &branch.id, "edited second")
            .unwrap();
        let claimed = repo
            .claim_pending_for_task_branch(&third.id, &task.id, &branch.id)
            .unwrap()
            .unwrap();
        assert_eq!(claimed.message, "third");
        assert_eq!(claimed.state, QueuedMessageState::Dispatching);
        repo.complete_claim_for_task_branch(&third.id, &task.id, &branch.id)
            .unwrap();

        let remaining = repo.list_pending(&task.id, &branch.id).unwrap();
        assert_eq!(
            remaining
                .iter()
                .map(|message| message.message.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "edited second"]
        );
        assert!(repo
            .update_pending_message(&third.id, &task.id, &branch.id, "too late")
            .is_err());
    }

    #[test]
    fn restoring_a_selected_claim_promotes_only_that_message() {
        let db = setup_db();
        let task = create_test_task(&db, "/proj", "queue claim fallback");
        let branch = SessionBranchRepository::new(&db)
            .ensure_active(&task.id)
            .unwrap();
        let repo = QueuedMessageRepository::new(&db);
        let first = QueuedMessage::new(&task.id, &branch.id, "first", 0);
        let second = QueuedMessage::new(&task.id, &branch.id, "second", 0);
        let third = QueuedMessage::new(&task.id, &branch.id, "third", 0);
        repo.enqueue(&first).unwrap();
        repo.enqueue(&second).unwrap();
        repo.enqueue(&third).unwrap();

        repo.claim_pending_for_task_branch(&third.id, &task.id, &branch.id)
            .unwrap()
            .unwrap();
        repo.restore_claim_to_front(&third.id, &task.id, &branch.id)
            .unwrap();

        assert_eq!(
            repo.list_pending(&task.id, &branch.id)
                .unwrap()
                .iter()
                .map(|message| message.message.as_str())
                .collect::<Vec<_>>(),
            vec!["third", "first", "second"]
        );
    }

    #[test]
    fn queued_messages_dispatch_is_scoped_to_the_requested_task() {
        let db = setup_db();
        let first_task = create_test_task(&db, "/proj-a", "first queue");
        let second_task = create_test_task(&db, "/proj-b", "second queue");
        let branches = SessionBranchRepository::new(&db);
        let first_branch = branches.ensure_active(&first_task.id).unwrap();
        let second_branch = branches.ensure_active(&second_task.id).unwrap();
        let repo = QueuedMessageRepository::new(&db);
        let first = QueuedMessage::new(&first_task.id, &first_branch.id, "first", 0);
        let second = QueuedMessage::new(&second_task.id, &second_branch.id, "second", 1_000);
        repo.enqueue(&first).unwrap();
        repo.enqueue(&second).unwrap();

        let claimed_first = repo.take_next_for_task(&first_task.id).unwrap().unwrap();
        assert_eq!(claimed_first.id, first.id);
        let claimed_second = repo.take_next_for_task(&second_task.id).unwrap().unwrap();
        assert_eq!(claimed_second.id, second.id);
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
        assert_eq!(fetched.id, ws.id);
        assert_eq!(fetched.canonical_path, "/home/user/project");
        assert_eq!(fetched.display_name, "My Project");
        assert_eq!(fetched.access_mode, ProjectAccessMode::RequestApproval);
        assert_eq!(fetched.last_opened_at, ws.last_opened_at);
        assert_eq!(fetched.memory_mode, WorkspaceMemoryMode::Inherit);
        assert_eq!(fetched.memory_generation, 1);
    }

    #[test]
    fn test_workspace_upsert_updates_mutable_fields_without_replacing_memory_owner() {
        let db = setup_db();
        let repo = WorkspaceRepository::new(&db);

        let ws = Workspace::new("/proj", "Original");
        repo.upsert(&ws).unwrap();
        assert_eq!(
            repo.update_memory_mode(&ws.id, 1, WorkspaceMemoryMode::ReadOnly)
                .unwrap(),
            2
        );

        let mut updated = Workspace::new("/proj", "Updated Name");
        let losing_candidate_id = updated.id.clone();
        updated.access_mode = ProjectAccessMode::FullAccess;
        updated.memory_mode = WorkspaceMemoryMode::Off;
        updated.memory_generation = 99;
        repo.upsert(&updated).unwrap();

        let fetched = repo.get("/proj").unwrap().unwrap();
        assert_eq!(fetched.id, ws.id);
        assert_eq!(fetched.display_name, "Updated Name");
        assert_eq!(fetched.access_mode, ProjectAccessMode::FullAccess);
        assert_eq!(fetched.memory_mode, WorkspaceMemoryMode::ReadOnly);
        assert_eq!(fetched.memory_generation, 2);
        assert_eq!(repo.get_by_id(&ws.id).unwrap(), Some(fetched));
        assert!(repo.get_by_id(&losing_candidate_id).unwrap().is_none());
    }

    #[test]
    fn test_workspace_memory_mode_generation_cas_and_reads() {
        let db = setup_db();
        let repo = WorkspaceRepository::new(&db);
        let ws = Workspace::new("/cas", "CAS");
        repo.upsert(&ws).unwrap();

        let before = repo.get_by_id(&ws.id).unwrap().unwrap();
        assert_eq!(before.memory_generation, 1);
        assert_eq!(
            repo.update_memory_mode(&ws.id, 1, WorkspaceMemoryMode::ReadOnly)
                .unwrap(),
            2
        );

        assert!(repo
            .update_memory_mode(&ws.id, 1, WorkspaceMemoryMode::Off)
            .is_err());
        assert!(repo
            .update_memory_mode("unknown-owner", 1, WorkspaceMemoryMode::Off)
            .is_err());
        let after_conflicts = repo.get("/cas").unwrap().unwrap();
        assert_eq!(after_conflicts.memory_mode, WorkspaceMemoryMode::ReadOnly);
        assert_eq!(after_conflicts.memory_generation, 2);

        assert_eq!(repo.list_recent(1).unwrap()[0].memory_generation, 2);
        assert_eq!(
            repo.get_by_id(&ws.id).unwrap().unwrap().memory_generation,
            2
        );
        assert_eq!(
            repo.update_memory_mode(&ws.id, 2, WorkspaceMemoryMode::Off)
                .unwrap(),
            3
        );
        let final_state = repo.get_by_id(&ws.id).unwrap().unwrap();
        assert_eq!(final_state.memory_mode, WorkspaceMemoryMode::Off);
        assert_eq!(final_state.memory_generation, 3);
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
    fn test_workspace_update_access_mode() {
        let db = setup_db();
        let repo = WorkspaceRepository::new(&db);

        let ws = Workspace::new("/proj", "Access Test");
        repo.upsert(&ws).unwrap();

        repo.update_access_mode("/proj", ProjectAccessMode::RiskBased)
            .unwrap();
        let fetched = repo.get("/proj").unwrap().unwrap();
        assert_eq!(fetched.access_mode, ProjectAccessMode::RiskBased);
    }

    #[test]
    fn test_workspace_touch() {
        let db = setup_db();
        let repo = WorkspaceRepository::new(&db);

        let ws = Workspace::new("/proj", "Touch Test");
        repo.upsert(&ws).unwrap();
        repo.update_memory_mode(&ws.id, 1, WorkspaceMemoryMode::ReadOnly)
            .unwrap();

        let original = repo.get("/proj").unwrap().unwrap();
        repo.touch("/proj").unwrap();
        let fetched = repo.get("/proj").unwrap().unwrap();
        assert!(fetched.last_opened_at >= original.last_opened_at);
        assert_eq!(fetched.id, original.id);
        assert_eq!(fetched.memory_mode, WorkspaceMemoryMode::ReadOnly);
        assert_eq!(fetched.memory_generation, 2);
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
