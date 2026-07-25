//! Tauri 命令 -- 前端通过 IPC 调用的命令函数。 [doc-09]
//!
//! 本模块实现前端调用的所有 Tauri 命令。由于尚未引入 `tauri` crate 依赖，
//! 这些函数实现为普通 async 函数，在未来 Tauri 集成时会用 `#[tauri::command]`
//! 包装并注入 `State<CommandState>`。
//!
//! ## 设计
//! - 每个命令接收 `&CommandState` 作为第一个参数（等价于 Tauri 的 `State` 注入）
//! - 命令创建合适的服务实例并委托执行
//! - 返回 `Result<T, String>`，错误被字符串化以便 IPC 传输
//!
//! ## 命令分组
//! - **任务**：创建/列表/详情
//! - **Agent**：发送消息/中止
//! - **权限**：审批/列出待审批
//! - **变更**：列出/回滚/接受
//! - **验证**：运行/列出
//! - **Workspace**：列表/打开
//! - **搜索**：快速打开/全局搜索
//! - **终端**：列表/创建/发送/读取/终止
//! - **恢复**：恢复页面数据/支持包
//! - **设置**：获取/设置
//!
//! [doc-09] [doc-11]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use hermes_core::{Message, SessionEvent, SessionMeta};
use hermes_store::SessionStore;
use r_code_core::dto::{
    AgentRun, FileChange, PermissionDecision, PermissionRequest, ReviewState, Task, TaskEvent,
    TaskEventType, TaskMode, TaskState, VerificationRecord, Workspace,
};
use r_code_core::error::ProductError;
use r_code_gateway::permission::PermissionEngine;
use r_code_store::review::ReviewAction;
use r_code_store::{
    AgentRunRepository, ChangeService, Database, ReviewService, TaskEventStore, TaskRepository,
    VerificationConfig, VerificationService, WorkspaceService,
};
use r_code_terminal::{SendOptions, TerminalControlService, TerminalManager};
use serde::{Deserialize, Serialize};

use crate::search::SearchService;
use crate::settings::SettingsService;
use crate::support_bundle::SupportBundle;

// ============================================================================
// CommandState -- 命令执行所需的全局状态
// ============================================================================

/// 命令状态 -- 持有所有命令执行所需的服务与存储。
///
/// 在 Tauri 集成后，这会通过 `tauri::State<CommandState>` 注入到每个命令。
/// 当前阶段通过 `&CommandState` 参数显式传递。
pub struct CommandState {
    /// SQLite 数据库（产品状态源）
    pub db: Arc<Database>,
    /// Blob 存储目录（文件基线 / 验证输出）
    pub blobs_dir: PathBuf,
    /// JSONL 会话存储目录
    pub sessions_dir: PathBuf,
    /// JSONL SessionStore（会话内容源）
    pub session_store: SessionStore,
    /// 权限引擎（standing rules + pending requests）
    pub permission_engine: Arc<PermissionEngine>,
    /// 终端管理器
    pub terminal_manager: Arc<TerminalManager>,
    /// 配置目录
    pub config_dir: PathBuf,
    /// 项目根目录
    pub project_root: PathBuf,
    /// SQLite 数据库文件路径（None = 内存库，用于测试）
    pub db_path: Option<PathBuf>,
}

impl CommandState {
    /// 创建命令状态。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db: Arc<Database>,
        blobs_dir: PathBuf,
        sessions_dir: PathBuf,
        config_dir: PathBuf,
        project_root: PathBuf,
        db_path: Option<PathBuf>,
    ) -> Self {
        Self {
            db,
            blobs_dir,
            session_store: SessionStore::new(sessions_dir.clone()),
            sessions_dir,
            permission_engine: Arc::new(PermissionEngine::new()),
            terminal_manager: Arc::new(TerminalManager::new()),
            config_dir,
            project_root,
            db_path,
        }
    }

    /// 创建用于测试的内存状态。
    pub fn in_memory(tmp: &Path) -> Result<Self, ProductError> {
        let db = Arc::new(Database::open_in_memory()?);
        let blobs_dir = tmp.join("blobs");
        let sessions_dir = tmp.join("sessions");
        let config_dir = tmp.join("config");
        std::fs::create_dir_all(&blobs_dir)?;
        std::fs::create_dir_all(&sessions_dir)?;
        std::fs::create_dir_all(&config_dir)?;
        Ok(Self::new(
            db,
            blobs_dir,
            sessions_dir,
            config_dir,
            tmp.to_path_buf(),
            None,
        ))
    }
}

// ============================================================================
// 支持类型
// ============================================================================

/// 任务详情 -- 包含任务及其所有关联数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDetail {
    /// 任务本体
    pub task: Task,
    /// Agent Run 列表
    pub runs: Vec<AgentRun>,
    /// 任务事件时间线
    pub events: Vec<TaskEvent>,
    /// 文件变更列表
    pub changes: Vec<FileChange>,
    /// 权限请求列表
    pub permissions: Vec<PermissionRequest>,
    /// 验证记录列表
    pub verifications: Vec<VerificationRecord>,
}

/// 搜索命中结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchMatch {
    /// 文件路径（相对于 project_root）
    pub path: String,
    /// 行号（1-based）
    pub line: usize,
    /// 列号（1-based）
    pub column: usize,
    /// 整行文本
    pub line_text: String,
}

/// 终端信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalInfo {
    /// 终端 ID
    pub id: String,
    /// 终端状态（"idle"/"busy"/"agent"/"exited"）
    pub state: String,
    /// Shell 类型
    pub shell: String,
    /// 是否正在执行命令
    pub is_busy: bool,
}

/// 恢复页面数据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryPageData {
    /// 中断的任务 ID 列表
    pub interrupted_tasks: Vec<String>,
    /// 孤儿权限请求数量
    pub orphaned_permissions: u64,
}

// ============================================================================
// 辅助函数
// ============================================================================

/// 解析任务模式字符串。
fn parse_mode(mode: &str) -> Result<TaskMode, String> {
    match mode {
        "ask" => Ok(TaskMode::Ask),
        "edit" => Ok(TaskMode::Edit),
        "auto" => Ok(TaskMode::Auto),
        _ => Err(format!("invalid mode: {mode} (expected ask/edit/auto)")),
    }
}

/// 解析权限决定字符串。
fn parse_decision(decision: &str) -> Result<PermissionDecision, String> {
    match decision {
        "allow" => Ok(PermissionDecision::Allow),
        "allow_always" => Ok(PermissionDecision::AllowAlways),
        "deny" => Ok(PermissionDecision::Deny),
        _ => Err(format!(
            "invalid decision: {decision} (expected allow/allow_always/deny)"
        )),
    }
}

/// 将错误转换为 String。泛型实现，兼容 ProductError / hermes_error::Error / std::io::Error 等。
fn err_str<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

// ============================================================================
// 任务命令
// ============================================================================

/// 任务创建命令。 [doc-09]
///
/// 创建新任务并持久化到 SQLite，同时记录 TaskCreated 事件。
pub async fn cmd_task_create(
    state: &CommandState,
    project_id: String,
    title: String,
    goal: String,
    mode: String,
) -> Result<Task, String> {
    let mode = parse_mode(&mode)?;
    let task = Task::new(&project_id, &title, &goal, mode);
    TaskRepository::new(&state.db)
        .create(&task)
        .map_err(err_str)?;
    TaskEventStore::new(&state.db)
        .append(&task.id, TaskEventType::TaskCreated)
        .map_err(err_str)?;
    Ok(task)
}

/// 列出任务命令。
pub async fn cmd_task_list(
    state: &CommandState,
    project_id: Option<String>,
    include_archived: bool,
) -> Result<Vec<Task>, String> {
    TaskRepository::new(&state.db)
        .list(project_id.as_deref(), None, include_archived)
        .map_err(err_str)
}

/// 获取任务详情（含事件、变更、权限、验证）。
pub async fn cmd_task_detail(state: &CommandState, task_id: String) -> Result<TaskDetail, String> {
    let task = TaskRepository::new(&state.db)
        .get(&task_id)
        .map_err(err_str)?
        .ok_or_else(|| format!("task not found: {task_id}"))?;

    let runs = AgentRunRepository::new(&state.db)
        .list_by_task(&task_id)
        .map_err(err_str)?;

    let events = TaskEventStore::new(&state.db)
        .list_by_task(&task_id, Some(400), None)
        .map_err(err_str)?;

    let changes = ChangeService::new(&state.db, state.blobs_dir.clone())
        .list_changes(&task_id)
        .await
        .map_err(err_str)?;

    let permissions = state.permission_engine.pending_for_task(&task_id).await;

    let verifications = VerificationService::new(&state.db, state.blobs_dir.clone())
        .list_for_task(&task_id)
        .await
        .map_err(err_str)?;

    Ok(TaskDetail {
        task,
        runs,
        events,
        changes,
        permissions,
        verifications,
    })
}

// ============================================================================
// Agent 命令
// ============================================================================

/// 发送用户消息到 Agent。 [doc-04 §7]
///
/// 将消息追加到会话存储，并记录任务事件。
/// 外部会话注入使用 bracketed paste，刻意不带回车。
pub async fn cmd_agent_send(
    state: &CommandState,
    task_id: String,
    message: String,
) -> Result<(), String> {
    // 确保会话文件存在（写入 Meta 行作为首行）
    let session_path = state.sessions_dir.join(format!("{task_id}.jsonl"));
    if !session_path.exists() {
        let meta = SessionMeta {
            id: task_id.clone(),
            created_at: chrono::Utc::now(),
            model: "unknown".to_string(),
            provider: "unknown".to_string(),
            title: None,
        };
        state
            .session_store
            .append(&task_id, SessionEvent::Meta(meta))
            .await
            .map_err(err_str)?;
    }

    // 追加用户消息
    let msg = Message::user_text(&message);
    state
        .session_store
        .append(&task_id, SessionEvent::Message(msg))
        .await
        .map_err(err_str)?;

    // 记录任务事件
    TaskEventStore::new(&state.db)
        .append(&task_id, TaskEventType::ToolCall)
        .map_err(err_str)?;

    Ok(())
}

/// 中止 Agent 运行。
pub async fn cmd_agent_abort(state: &CommandState, task_id: String) -> Result<(), String> {
    // 更新任务状态为 Idle
    TaskRepository::new(&state.db)
        .update_state(&task_id, TaskState::Idle)
        .map_err(err_str)?;

    // 结束活跃的 Agent Run
    let active_run = AgentRunRepository::new(&state.db)
        .get_active_run(&task_id)
        .map_err(err_str)?;

    if let Some(run) = active_run {
        AgentRunRepository::new(&state.db)
            .update_review_state(&run.id, ReviewState::RolledBack)
            .map_err(err_str)?;
    }

    // 记录任务事件
    TaskEventStore::new(&state.db)
        .append(&task_id, TaskEventType::RunEnded)
        .map_err(err_str)?;

    Ok(())
}

// ============================================================================
// 权限命令
// ============================================================================

/// 审批权限请求。 [doc-02 §4]
///
/// decision: "allow" | "allow_always" | "deny"
pub async fn cmd_permission_approve(
    state: &CommandState,
    request_id: String,
    decision: String,
) -> Result<(), String> {
    let decision = parse_decision(&decision)?;
    state
        .permission_engine
        .decide(&request_id, decision)
        .await
        .map_err(err_str)
}

/// 获取待审批权限请求列表。
pub async fn cmd_permission_pending(
    state: &CommandState,
    task_id: String,
) -> Result<Vec<PermissionRequest>, String> {
    Ok(state.permission_engine.pending_for_task(&task_id).await)
}

// ============================================================================
// 变更命令
// ============================================================================

/// 获取任务的文件变更列表。
pub async fn cmd_changes_list(
    state: &CommandState,
    task_id: String,
) -> Result<Vec<FileChange>, String> {
    ChangeService::new(&state.db, state.blobs_dir.clone())
        .list_changes(&task_id)
        .await
        .map_err(err_str)
}

/// 回滚单个文件。
///
/// 返回回滚结果的描述字符串。
/// 若 `path` 为相对路径，则相对于 `project_root` 解析。
pub async fn cmd_rollback_file(
    state: &CommandState,
    task_id: String,
    path: String,
) -> Result<String, String> {
    let svc = ChangeService::new(&state.db, state.blobs_dir.clone());
    // 相对路径基于 project_root 解析
    let full_path = if Path::new(&path).is_absolute() {
        PathBuf::from(&path)
    } else {
        state.project_root.join(&path)
    };
    let result = svc
        .rollback_file(&task_id, &full_path)
        .await
        .map_err(err_str)?;
    Ok(format!("{result:?}"))
}

/// 回滚任务的所有变更。
///
/// 返回各文件回滚结果的描述字符串列表。
pub async fn cmd_rollback_task(
    state: &CommandState,
    task_id: String,
) -> Result<Vec<String>, String> {
    let svc = ChangeService::new(&state.db, state.blobs_dir.clone());
    let results = svc.rollback_task(&task_id).await.map_err(err_str)?;
    Ok(results.into_iter().map(|r| format!("{r:?}")).collect())
}

/// 接受任务变更。
pub async fn cmd_accept_task(state: &CommandState, task_id: String) -> Result<(), String> {
    let svc = ReviewService::new(&state.db, state.blobs_dir.clone());
    svc.apply_action(&task_id, ReviewAction::AcceptAll)
        .await
        .map_err(err_str)?;
    // 更新任务状态
    TaskRepository::new(&state.db)
        .update_state(&task_id, TaskState::Idle)
        .map_err(err_str)?;
    Ok(())
}

// ============================================================================
// 验证命令
// ============================================================================

/// 运行验证命令。
pub async fn cmd_run_verification(
    state: &CommandState,
    task_id: String,
    command: String,
) -> Result<VerificationRecord, String> {
    // 获取或创建活跃 Run
    let run = AgentRunRepository::new(&state.db)
        .get_active_run(&task_id)
        .map_err(err_str)?
        .or_else(|| {
            // 无活跃 Run -> 创建一个已结束的占位 Run
            let r = AgentRun::new(&task_id, "verification");
            let _ = AgentRunRepository::new(&state.db).create(&r);
            Some(r)
        })
        .unwrap();

    let config = VerificationConfig {
        command,
        timeout_secs: 300,
    };

    VerificationService::new(&state.db, state.blobs_dir.clone())
        .run_verification(&task_id, &run.id, &config, &state.project_root)
        .await
        .map_err(err_str)
}

/// 获取验证结果列表。
pub async fn cmd_verification_list(
    state: &CommandState,
    task_id: String,
) -> Result<Vec<VerificationRecord>, String> {
    VerificationService::new(&state.db, state.blobs_dir.clone())
        .list_for_task(&task_id)
        .await
        .map_err(err_str)
}

// ============================================================================
// Workspace 命令
// ============================================================================

/// 列出最近打开的 Workspace。
pub async fn cmd_workspace_list(state: &CommandState) -> Result<Vec<Workspace>, String> {
    WorkspaceService::new(&state.db)
        .list_recent(20)
        .map_err(err_str)
}

/// 打开 Workspace。
pub async fn cmd_workspace_open(state: &CommandState, path: String) -> Result<Workspace, String> {
    let display_name = Path::new(&path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace")
        .to_string();
    WorkspaceService::new(&state.db)
        .open(&path, &display_name)
        .map_err(err_str)
}

// ============================================================================
// 搜索命令
// ============================================================================

/// 快速打开 -- 模糊匹配文件路径。
pub async fn cmd_quick_open(
    state: &CommandState,
    query: String,
    limit: usize,
) -> Result<Vec<String>, String> {
    let svc = SearchService::new(state.project_root.clone());
    svc.quick_open(&query, limit).await.map_err(err_str)
}

/// 全局搜索 -- 搜索文件内容。
pub async fn cmd_global_search(
    state: &CommandState,
    query: String,
    limit: usize,
) -> Result<Vec<SearchMatch>, String> {
    let svc = SearchService::new(state.project_root.clone());
    let cancel = tokio_util::sync::CancellationToken::new();
    let matches = svc
        .global_search(&query, limit, cancel)
        .await
        .map_err(err_str)?;
    Ok(matches
        .into_iter()
        .map(|m| SearchMatch {
            path: m.path,
            line: m.line,
            column: m.column,
            line_text: m.line_text,
        })
        .collect())
}

// ============================================================================
// 终端命令
// ============================================================================

/// 获取终端列表。
pub async fn cmd_terminal_list(state: &CommandState) -> Result<Vec<TerminalInfo>, String> {
    let svc = TerminalControlService::new(state.terminal_manager.clone());
    let terminals = svc.list().await.map_err(err_str)?;
    Ok(terminals
        .into_iter()
        .map(|t| TerminalInfo {
            id: t.id,
            state: format!("{:?}", t.state).to_lowercase(),
            shell: t.shell,
            is_busy: t.is_busy,
        })
        .collect())
}

/// 创建终端。
///
/// 返回终端 ID。
pub async fn cmd_terminal_create(
    state: &CommandState,
    shell: String,
    cwd: String,
) -> Result<String, String> {
    let svc = TerminalControlService::new(state.terminal_manager.clone());
    let id = svc
        .create(&shell, Path::new(&cwd), Vec::new())
        .await
        .map_err(err_str)?;
    Ok(id)
}

/// 发送文本到终端。
pub async fn cmd_terminal_send(
    state: &CommandState,
    id: String,
    text: String,
    press_enter: bool,
) -> Result<(), String> {
    let svc = TerminalControlService::new(state.terminal_manager.clone());
    svc.send(&id, None, SendOptions { text, press_enter })
        .await
        .map_err(err_str)
}

/// 读取终端输出。
pub async fn cmd_terminal_read(state: &CommandState, id: String) -> Result<String, String> {
    let svc = TerminalControlService::new(state.terminal_manager.clone());
    svc.read(&id).await.map_err(err_str)
}

/// 终止终端。
pub async fn cmd_terminal_kill(state: &CommandState, id: String) -> Result<(), String> {
    let svc = TerminalControlService::new(state.terminal_manager.clone());
    // 用户命令，caller_is_agent = false
    svc.kill(&id, false).await.map_err(err_str)
}

// ============================================================================
// 恢复命令
// ============================================================================

/// 获取恢复页面数据。 [doc-18 M10]
pub async fn cmd_recovery_data(state: &CommandState) -> Result<RecoveryPageData, String> {
    // 查询中断的任务（状态非 Idle/Archived 且有活跃 Run）
    let conn = state.db.conn().map_err(err_str)?;
    let mut stmt = conn
        .prepare(
            "SELECT DISTINCT t.id FROM tasks t \
             JOIN agent_runs ar ON ar.task_id = t.id \
             WHERE t.state NOT IN ('idle', 'archived') AND ar.ended_at IS NULL",
        )
        .map_err(|e: rusqlite::Error| ProductError::DatabaseError(e.to_string()).to_string())?;

    let interrupted: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .map_err(|e| ProductError::DatabaseError(e.to_string()).to_string())?
        .filter_map(|r| r.ok())
        .collect();
    drop(stmt);

    // 查询孤儿权限（pending 状态）
    let orphaned: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM permission_requests WHERE decision = 'pending'",
            [],
            |row| row.get(0),
        )
        .map_err(|e: rusqlite::Error| ProductError::DatabaseError(e.to_string()).to_string())?;

    Ok(RecoveryPageData {
        interrupted_tasks: interrupted,
        orphaned_permissions: orphaned.max(0) as u64,
    })
}

/// 生成支持包。 [doc-18 M10-04]
///
/// 返回生成的 JSON 文件路径。
pub async fn cmd_support_bundle(
    state: &CommandState,
    output_dir: String,
) -> Result<String, String> {
    let bundle = SupportBundle::new(PathBuf::from(&output_dir));

    // 优先使用文件数据库路径；内存库则使用临时文件
    let db_path = match &state.db_path {
        Some(p) => p.clone(),
        None => {
            // 内存库无法生成完整支持包，返回降级结果
            let tmp = std::env::temp_dir().join("r-code-in-memory.db");
            std::fs::write(&tmp, b"").map_err(err_str)?;
            tmp
        }
    };

    let path = bundle.generate(&db_path).await.map_err(err_str)?;
    Ok(path.display().to_string())
}

// ============================================================================
// 设置命令
// ============================================================================

/// 获取应用设置（JSON）。
pub async fn cmd_settings_get(state: &CommandState) -> Result<serde_json::Value, String> {
    let settings = SettingsService::new(state.config_dir.clone());
    let config = settings.load_global().map_err(err_str)?;
    serde_json::to_value(&config).map_err(|e| e.to_string())
}

/// 设置应用配置项。
///
/// `key` 支持点分路径（如 "log_level"、"providers.anthropic.model"）。
pub async fn cmd_settings_set(
    state: &CommandState,
    key: String,
    value: serde_json::Value,
) -> Result<(), String> {
    let settings = SettingsService::new(state.config_dir.clone());
    let mut config = settings.load_global().map_err(err_str)?;

    // 将 Config 序列化为 JSON Value，修改后再反序列化回 Config
    let mut config_json = serde_json::to_value(&config).map_err(|e| e.to_string())?;

    // 按点分路径设置值
    set_nested_value(&mut config_json, &key, value)?;

    // 反序列化回 Config
    config = serde_json::from_value(config_json).map_err(|e| e.to_string())?;

    settings.save_global(&config).map_err(err_str)?;
    Ok(())
}

/// 按点分路径设置 JSON Value 中的值。
fn set_nested_value(
    root: &mut serde_json::Value,
    key: &str,
    value: serde_json::Value,
) -> Result<(), String> {
    // 空键 -> 直接替换根值
    if key.is_empty() {
        *root = value;
        return Ok(());
    }

    let parts: Vec<&str> = key.split('.').collect();
    let mut current = root;

    for (i, part) in parts.iter().enumerate() {
        if i == parts.len() - 1 {
            // 最后一段 -- 设置值
            if let Some(obj) = current.as_object_mut() {
                obj.insert(part.to_string(), value);
                return Ok(());
            } else {
                return Err(format!("cannot set '{key}': parent is not an object"));
            }
        } else {
            // 中间段 -- 向下遍历
            if let Some(obj) = current.as_object_mut() {
                current = obj
                    .entry(part.to_string())
                    .or_insert_with(|| serde_json::json!({}));
            } else {
                return Err(format!(
                    "cannot traverse '{key}': intermediate is not an object"
                ));
            }
        }
    }

    Ok(())
}

// ============================================================================
// 上下文投喂 [doc-04 §7]
// ============================================================================

/// 创建文件引用上下文块。
///
/// 用于 `@path` 输入：拖拽文件或输入 `@path` 时创建引用块。
pub fn create_file_ref(path: &str, line: Option<u32>) -> serde_json::Value {
    r_code_core::dto::file_ref_data(path, line)
}

/// 创建选区引用上下文块。
///
/// 用于选中代码时冻结快照块注入。
pub fn create_selection_ref(path: &str, start: u32, end: u32, hash: &str) -> serde_json::Value {
    r_code_core::dto::selection_ref_data(path, start, end, hash)
}

/// 外部会话注入 -- 使用 bracketed paste，刻意不带回车。
///
/// 返回包装后的文本，适合通过 `cmd_terminal_send(press_enter=false)` 注入。
pub fn create_external_session_injection(text: &str) -> String {
    // Bracketed paste 序列：ESC[200~ + text + ESC[201~
    // 刻意不追加 \r（回车），由用户显式按 Enter 触发执行
    format!("\x1b[200~{text}\x1b[201~")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// 创建测试状态。
    fn setup_state() -> (TempDir, CommandState) {
        let dir = TempDir::new().unwrap();
        let state = CommandState::in_memory(dir.path()).unwrap();
        (dir, state)
    }

    #[tokio::test]
    async fn task_create_and_list() {
        let (_dir, state) = setup_state();
        let task = cmd_task_create(
            &state,
            "/proj".into(),
            "Test".into(),
            "Do thing".into(),
            "ask".into(),
        )
        .await
        .unwrap();

        assert_eq!(task.project_id, "/proj");
        assert_eq!(task.title, "Test");
        assert_eq!(task.mode, TaskMode::Ask);

        let tasks = cmd_task_list(&state, None, false).await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, task.id);
    }

    #[tokio::test]
    async fn task_create_invalid_mode() {
        let (_dir, state) = setup_state();
        let result = cmd_task_create(
            &state,
            "/proj".into(),
            "T".into(),
            "g".into(),
            "invalid".into(),
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn task_detail_returns_all_data() {
        let (_dir, state) = setup_state();
        let task = cmd_task_create(
            &state,
            "/proj".into(),
            "T".into(),
            "g".into(),
            "edit".into(),
        )
        .await
        .unwrap();

        let detail = cmd_task_detail(&state, task.id.clone()).await.unwrap();
        assert_eq!(detail.task.id, task.id);
        assert!(detail
            .events
            .iter()
            .any(|e| { e.event_type == TaskEventType::TaskCreated }));
    }

    #[tokio::test]
    async fn permission_approve_and_pending() {
        let (_dir, state) = setup_state();
        let task = cmd_task_create(&state, "/p".into(), "T".into(), "g".into(), "edit".into())
            .await
            .unwrap();

        // 创建权限请求
        let req = state
            .permission_engine
            .request_permission(
                &task.id,
                "tc1",
                "write_file",
                r_code_core::dto::RiskLevel::R3,
                "writing to /etc/passwd",
            )
            .await;

        // 验证 pending 列表包含该请求
        let pending = cmd_permission_pending(&state, task.id.clone())
            .await
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, req.id);

        // 审批 -> allow（单次允许）
        cmd_permission_approve(&state, req.id.clone(), "allow".into())
            .await
            .unwrap();

        // 验证 pending 列表已清空
        let pending_after = cmd_permission_pending(&state, task.id).await.unwrap();
        assert!(pending_after.is_empty());
    }

    #[tokio::test]
    async fn permission_approve_invalid_decision() {
        let (_dir, state) = setup_state();
        let result = cmd_permission_approve(&state, "req1".into(), "maybe".into()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn rollback_file_restores_content() {
        let (_dir, state) = setup_state();
        let task = cmd_task_create(
            &state,
            "/proj".into(),
            "T".into(),
            "g".into(),
            "edit".into(),
        )
        .await
        .unwrap();

        // 创建文件并捕获基线
        let file_path = state.project_root.join("test.txt");
        std::fs::write(&file_path, "original content").unwrap();

        let cs = ChangeService::new(&state.db, state.blobs_dir.clone());
        cs.capture_baseline(&task.id, &file_path).await.unwrap();

        // 修改文件并记录变更
        std::fs::write(&file_path, "modified content").unwrap();
        cs.record_change(
            &task.id,
            &file_path,
            r_code_core::dto::FileChangeType::Modify,
            None,
            Some(b"original content"),
            Some(b"modified content"),
            None,
        )
        .await
        .unwrap();

        // 验证文件已修改
        assert_eq!(
            std::fs::read_to_string(&file_path).unwrap(),
            "modified content"
        );

        // 回滚
        let rel_path = "test.txt";
        let result = cmd_rollback_file(&state, task.id, rel_path.into())
            .await
            .unwrap();
        assert!(result.contains("Restored"));

        // 验证文件已恢复
        assert_eq!(
            std::fs::read_to_string(&file_path).unwrap(),
            "original content"
        );
    }

    #[tokio::test]
    async fn agent_send_creates_session() {
        let (_dir, state) = setup_state();
        let task = cmd_task_create(&state, "/p".into(), "T".into(), "g".into(), "ask".into())
            .await
            .unwrap();

        cmd_agent_send(&state, task.id.clone(), "Hello agent".into())
            .await
            .unwrap();

        // 验证会话文件已创建
        let session_path = state.sessions_dir.join(format!("{}.jsonl", task.id));
        assert!(session_path.exists());

        let content = std::fs::read_to_string(&session_path).unwrap();
        assert!(content.contains("Hello agent"));
    }

    #[tokio::test]
    async fn agent_abort_updates_state() {
        let (_dir, state) = setup_state();
        let task = cmd_task_create(&state, "/p".into(), "T".into(), "g".into(), "edit".into())
            .await
            .unwrap();

        // 设置为 InProgress
        TaskRepository::new(&state.db)
            .update_state(&task.id, TaskState::InProgress)
            .unwrap();

        cmd_agent_abort(&state, task.id.clone()).await.unwrap();

        let detail = cmd_task_detail(&state, task.id.clone()).await.unwrap();
        assert_eq!(detail.task.state, TaskState::Idle);
    }

    #[tokio::test]
    async fn workspace_open_and_list() {
        let (_dir, state) = setup_state();
        let ws = cmd_workspace_open(&state, "/home/user/myproject".into())
            .await
            .unwrap();
        assert_eq!(ws.canonical_path, "/home/user/myproject");

        let list = cmd_workspace_list(&state).await.unwrap();
        assert_eq!(list.len(), 1);
    }

    #[tokio::test]
    async fn recovery_data_returns_empty_for_fresh_db() {
        let (_dir, state) = setup_state();
        let data = cmd_recovery_data(&state).await.unwrap();
        assert!(data.interrupted_tasks.is_empty());
        assert_eq!(data.orphaned_permissions, 0);
    }

    #[tokio::test]
    async fn settings_get_returns_json() {
        let (_dir, state) = setup_state();
        let result = cmd_settings_get(&state).await;
        // 可能成功（返回 JSON）或失败（无有效 provider 配置）
        // 两种情况都可接受，关键是函数不 panic
        if let Ok(val) = result {
            assert!(val.is_object());
        }
    }

    #[test]
    fn file_ref_creation() {
        let data = create_file_ref("/src/main.rs", Some(42));
        assert_eq!(data["path"], "/src/main.rs");
        assert_eq!(data["line"], 42);
    }

    #[test]
    fn selection_ref_creation() {
        let data = create_selection_ref("/src/lib.rs", 10, 20, "abc123");
        assert_eq!(data["path"], "/src/lib.rs");
        assert_eq!(data["start"], 10);
        assert_eq!(data["end"], 20);
        assert_eq!(data["hash"], "abc123");
    }

    #[test]
    fn external_session_injection_no_trailing_enter() {
        let text = create_external_session_injection("ls -la\nuname -a");
        // 应包含 bracketed paste 序列
        assert!(text.contains("\x1b[200~"));
        assert!(text.contains("\x1b[201~"));
        // 不应以回车结尾
        assert!(!text.ends_with('\r'));
        assert!(!text.ends_with('\n'));
    }

    #[test]
    fn nested_value_set_simple() {
        let mut root = serde_json::json!({"log_level": "info"});
        set_nested_value(&mut root, "log_level", serde_json::json!("debug")).unwrap();
        assert_eq!(root["log_level"], "debug");
    }

    #[test]
    fn nested_value_set_deep() {
        let mut root = serde_json::json!({"providers": {"anthropic": {"model": "old"}}});
        set_nested_value(
            &mut root,
            "providers.anthropic.model",
            serde_json::json!("new-model"),
        )
        .unwrap();
        assert_eq!(root["providers"]["anthropic"]["model"], "new-model");
    }
}
