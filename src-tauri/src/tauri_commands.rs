//! Tauri 命令包装层（bin 侧）——前端通过 IPC 调用的全部命令。
//!
//! 每个 `cmd_*` 都是薄包装：注入 `State<CommandState>`，委托给
//! `r_code_host::commands` 中的同名 inner（lib 侧可测核心）。
//! lib 不依赖 tauri（保持单元测试二进制无 GUI 链接）。

use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use r_code_core::dto::{
    AgentSendMode, FileChange, PermissionRequest, ProjectAccessMode, QueuedMessage, Task,
    VerificationRecord, Workspace,
};
use r_code_host::commands::{
    ChangeDiff, CommandState, RecoveryPageData, SearchMatch, SessionMessage, TaskDetail,
    TerminalInfo,
};
use r_code_host::log_buffer::LogEntry;
use r_code_host::replay::ReplayEntry;

/// 任务创建命令。 [doc-09]
#[tauri::command]
pub async fn cmd_task_create(
    state: State<'_, CommandState>,
    workspace_path: Option<String>,
    title: String,
    goal: String,
    mode: String,
    provider_name: Option<String>,
) -> Result<Task, String> {
    r_code_host::commands::task_create_with_provider(
        &state,
        workspace_path.as_deref(),
        &title,
        &goal,
        &mode,
        provider_name.as_deref(),
    )
    .await
}

/// 列出任务命令。
#[tauri::command]
pub async fn cmd_task_list(
    state: State<'_, CommandState>,
    workspace_path: Option<String>,
    include_archived: bool,
) -> Result<Vec<Task>, String> {
    r_code_host::commands::task_list(&state, workspace_path.as_deref(), include_archived).await
}

/// 归档已停止的会话。
#[tauri::command]
pub async fn cmd_task_archive(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<Task, String> {
    r_code_host::commands::task_archive(&state, &task_id).await
}

/// 为一个既有会话附加/移除工作区。未授信工作区不会开放本地工具。
#[tauri::command]
pub async fn cmd_task_set_workspace(
    state: State<'_, CommandState>,
    task_id: String,
    workspace_path: Option<String>,
) -> Result<Task, String> {
    r_code_host::commands::task_set_workspace(&state, &task_id, workspace_path.as_deref()).await
}

/// 切换空闲会话绑定的模型服务；下一次运行生效。
#[tauri::command]
pub async fn cmd_task_set_provider(
    state: State<'_, CommandState>,
    task_id: String,
    provider_name: String,
) -> Result<Task, String> {
    r_code_host::commands::task_set_provider(&state, &task_id, &provider_name).await
}

/// 获取任务详情（含事件、变更、权限、验证）。
#[tauri::command]
pub async fn cmd_task_detail(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<TaskDetail, String> {
    r_code_host::commands::task_detail(&state, &task_id).await
}

/// 发送用户消息到 Agent。 [doc-04 §7]
#[tauri::command]
pub async fn cmd_agent_send(
    state: State<'_, CommandState>,
    task_id: String,
    message: String,
    mode: Option<String>,
) -> Result<(), String> {
    let mode = match mode {
        Some(value) => AgentSendMode::try_from_str(&value)
            .ok_or_else(|| format!("invalid agent send mode: {value}"))?,
        None => AgentSendMode::Auto,
    };
    r_code_host::commands::agent_send_with_mode(&state, &task_id, &message, mode).await
}

/// 中止 Agent 运行。
#[tauri::command]
pub async fn cmd_agent_abort(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<(), String> {
    r_code_host::commands::agent_abort(&state, &task_id).await
}

/// 中止当前主运行下的一个子代理。
#[tauri::command]
pub async fn cmd_agent_abort_subagent(
    state: State<'_, CommandState>,
    task_id: String,
    subagent_id: String,
) -> Result<(), String> {
    r_code_host::commands::agent_abort_subagent(&state, &task_id, &subagent_id).await
}

/// 列出当前会话分支的待发送消息。
#[tauri::command]
pub async fn cmd_agent_queue_list(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<Vec<QueuedMessage>, String> {
    r_code_host::commands::agent_queue_list(&state, &task_id).await
}

/// 移除一条尚未分发的待发送消息。
#[tauri::command]
pub async fn cmd_agent_queue_remove(
    state: State<'_, CommandState>,
    task_id: String,
    queue_id: String,
) -> Result<(), String> {
    r_code_host::commands::agent_queue_remove(&state, &task_id, &queue_id).await
}

/// 编辑历史用户消息，并创建新分支后重跑。
#[tauri::command]
pub async fn cmd_agent_resend(
    state: State<'_, CommandState>,
    task_id: String,
    message_id: String,
    message: String,
) -> Result<(), String> {
    r_code_host::commands::agent_resend(&state, &task_id, &message_id, &message).await
}

/// 审批权限请求。 [doc-02 §4]
///
/// decision: "allow" | "allow_always" | "deny"
#[tauri::command]
pub async fn cmd_permission_approve(
    state: State<'_, CommandState>,
    request_id: String,
    decision: String,
) -> Result<(), String> {
    r_code_host::commands::permission_approve(&state, &request_id, &decision).await
}

/// 获取待审批权限请求列表。
#[tauri::command]
pub async fn cmd_permission_pending(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<Vec<PermissionRequest>, String> {
    r_code_host::commands::permission_pending(&state, &task_id).await
}

/// 获取任务的文件变更列表。
#[tauri::command]
pub async fn cmd_changes_list(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<Vec<FileChange>, String> {
    r_code_host::commands::changes_list(&state, &task_id).await
}

/// 回滚单个文件。返回回滚结果的描述字符串。
#[tauri::command]
pub async fn cmd_rollback_file(
    state: State<'_, CommandState>,
    task_id: String,
    path: String,
) -> Result<String, String> {
    r_code_host::commands::rollback_file(&state, &task_id, &path).await
}

/// 回滚任务的所有变更。
#[tauri::command]
pub async fn cmd_rollback_task(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<Vec<String>, String> {
    r_code_host::commands::rollback_task(&state, &task_id).await
}

/// 接受任务变更。
#[tauri::command]
pub async fn cmd_accept_task(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<(), String> {
    r_code_host::commands::accept_task(&state, &task_id).await
}

/// 单文件变更 diff（blob 缺失时降级返回元信息）。
#[tauri::command]
pub async fn cmd_change_diff(
    state: State<'_, CommandState>,
    task_id: String,
    path: String,
) -> Result<ChangeDiff, String> {
    r_code_host::commands::change_diff(&state, &task_id, &path).await
}

/// 运行验证命令。
#[tauri::command]
pub async fn cmd_run_verification(
    state: State<'_, CommandState>,
    task_id: String,
    command: String,
) -> Result<VerificationRecord, String> {
    r_code_host::commands::run_verification(&state, &task_id, &command).await
}

/// 获取验证结果列表。
#[tauri::command]
pub async fn cmd_verification_list(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<Vec<VerificationRecord>, String> {
    r_code_host::commands::verification_list(&state, &task_id).await
}

/// 列出最近打开的 Workspace。
#[tauri::command]
pub async fn cmd_workspace_list(state: State<'_, CommandState>) -> Result<Vec<Workspace>, String> {
    r_code_host::commands::workspace_list(&state).await
}

/// 打开 Workspace。
#[tauri::command]
pub async fn cmd_workspace_open(
    state: State<'_, CommandState>,
    path: String,
) -> Result<Workspace, String> {
    r_code_host::commands::workspace_open(&state, std::path::Path::new(&path)).await
}

/// 调用系统原生目录选择器；用户取消时返回 None，不把任何路径授予前端文件权限。
#[tauri::command]
pub async fn cmd_workspace_choose(
    app: AppHandle,
    state: State<'_, CommandState>,
) -> Result<Option<Workspace>, String> {
    let selected = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("选择工作区文件夹")
            .blocking_pick_folder()
            .map(|path| {
                path.into_path()
                    .map_err(|e| format!("invalid folder selection: {e}"))
            })
            .transpose()
    })
    .await
    .map_err(|e| format!("folder dialog worker failed: {e}"))??;

    match selected {
        Some(path) => r_code_host::commands::workspace_open(&state, &path)
            .await
            .map(Some),
        None => Ok(None),
    }
}

/// 更新项目级 Agent 权限模式。
#[tauri::command]
pub async fn cmd_workspace_set_access_mode(
    state: State<'_, CommandState>,
    workspace_path: String,
    access_mode: ProjectAccessMode,
) -> Result<Workspace, String> {
    r_code_host::commands::workspace_set_access_mode(&state, &workspace_path, access_mode).await
}

/// 快速打开 -- 模糊匹配文件路径。
#[tauri::command]
pub async fn cmd_quick_open(
    state: State<'_, CommandState>,
    workspace_path: String,
    query: String,
    limit: usize,
) -> Result<Vec<String>, String> {
    r_code_host::commands::quick_open(&state, &workspace_path, &query, limit).await
}

/// 全局搜索 -- 搜索文件内容。
#[tauri::command]
pub async fn cmd_global_search(
    state: State<'_, CommandState>,
    workspace_path: String,
    query: String,
    limit: usize,
) -> Result<Vec<SearchMatch>, String> {
    r_code_host::commands::global_search(&state, &workspace_path, &query, limit).await
}

/// 获取终端列表。
#[tauri::command]
pub async fn cmd_terminal_list(
    state: State<'_, CommandState>,
) -> Result<Vec<TerminalInfo>, String> {
    r_code_host::commands::terminal_list(&state).await
}

/// 创建终端。返回终端 ID。
#[tauri::command]
pub async fn cmd_terminal_create(
    state: State<'_, CommandState>,
    shell: String,
    workspace_path: String,
) -> Result<String, String> {
    r_code_host::commands::terminal_create(&state, &shell, &workspace_path).await
}

/// 发送文本到终端。
#[tauri::command]
pub async fn cmd_terminal_send(
    state: State<'_, CommandState>,
    id: String,
    text: String,
    press_enter: bool,
) -> Result<(), String> {
    r_code_host::commands::terminal_send(&state, &id, &text, press_enter).await
}

/// 读取终端输出。
#[tauri::command]
pub async fn cmd_terminal_read(
    state: State<'_, CommandState>,
    id: String,
) -> Result<String, String> {
    r_code_host::commands::terminal_read(&state, &id).await
}

/// 读取终端完整保留输出。
#[tauri::command]
pub async fn cmd_terminal_snapshot(
    state: State<'_, CommandState>,
    id: String,
) -> Result<String, String> {
    r_code_host::commands::terminal_snapshot(&state, &id).await
}

/// 终止终端。
#[tauri::command]
pub async fn cmd_terminal_kill(state: State<'_, CommandState>, id: String) -> Result<(), String> {
    r_code_host::commands::terminal_kill(&state, &id).await
}

/// 调整终端 PTY 大小。
#[tauri::command]
pub async fn cmd_terminal_resize(
    state: State<'_, CommandState>,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    r_code_host::commands::terminal_resize(&state, &id, cols, rows).await
}

/// 获取恢复页面数据。 [doc-18 M10]
#[tauri::command]
pub async fn cmd_recovery_data(state: State<'_, CommandState>) -> Result<RecoveryPageData, String> {
    r_code_host::commands::recovery_data(&state).await
}

/// 清理孤儿权限请求（全部 pending → deny），返回受影响行数。
#[tauri::command]
pub async fn cmd_recovery_cleanup(state: State<'_, CommandState>) -> Result<u64, String> {
    r_code_host::commands::recovery_cleanup(&state).await
}

/// 生成支持包。 [doc-18 M10-04] 返回生成的 JSON 文件路径。
#[tauri::command]
pub async fn cmd_support_bundle(
    state: State<'_, CommandState>,
    output_dir: String,
) -> Result<String, String> {
    r_code_host::commands::support_bundle(&state, &output_dir).await
}

/// 预览支持包内容（不写文件，供用户确认后再导出）。
#[tauri::command]
pub async fn cmd_support_preview(
    state: State<'_, CommandState>,
) -> Result<serde_json::Value, String> {
    r_code_host::commands::support_preview(&state).await
}

/// 获取指定深度的会话回放（recap / explore / verify）。
#[tauri::command]
pub async fn cmd_replay(
    state: State<'_, CommandState>,
    session_id: String,
    depth: String,
) -> Result<Vec<ReplayEntry>, String> {
    r_code_host::commands::replay(&state, &session_id, &depth).await
}

/// 读取会话消息序列（Room 时间线数据源）。
#[tauri::command]
pub async fn cmd_session_messages(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<Vec<SessionMessage>, String> {
    r_code_host::commands::session_messages(&state, &task_id).await
}

/// 读取项目记忆（`<project_root>/.r-code/memory.md`）。
#[tauri::command]
pub async fn cmd_memory_get(
    state: State<'_, CommandState>,
    workspace_path: String,
) -> Result<String, String> {
    r_code_host::commands::memory_get(&state, &workspace_path).await
}

/// 写入项目记忆（三投影：memory.md / CLAUDE.md / AGENTS.md 由调用方另行同步）。
#[tauri::command]
pub async fn cmd_memory_set(
    state: State<'_, CommandState>,
    workspace_path: String,
    content: String,
) -> Result<(), String> {
    r_code_host::commands::memory_set(&state, &workspace_path, &content).await
}

/// 读取最近的日志条目（环形缓冲；level 过滤如 "error"/"warn"）。
#[tauri::command]
pub async fn cmd_logs_tail(
    state: State<'_, CommandState>,
    limit: usize,
    level: Option<String>,
) -> Result<Vec<LogEntry>, String> {
    r_code_host::commands::logs_tail(&state, limit, level.as_deref()).await
}

/// 获取应用设置（JSON）。
#[tauri::command]
pub async fn cmd_settings_get(state: State<'_, CommandState>) -> Result<serde_json::Value, String> {
    r_code_host::commands::settings_get(&state).await
}

/// 设置应用配置项。`key` 支持点分路径（如 "providers.anthropic.model"）。
#[tauri::command]
pub async fn cmd_settings_set(
    state: State<'_, CommandState>,
    key: String,
    value: serde_json::Value,
) -> Result<(), String> {
    r_code_host::commands::settings_set(&state, &key, value).await
}

/// 原子保存 Provider 表单并将其设为默认服务。
#[tauri::command]
pub async fn cmd_settings_save_provider(
    state: State<'_, CommandState>,
    provider: r_code_host::commands::ProviderSettingsInput,
) -> Result<(), String> {
    r_code_host::commands::settings_save_provider(&state, provider).await
}

/// 切换新对话默认使用的 Provider（仅允许切换到已就绪的配置）。
#[tauri::command]
pub async fn cmd_settings_select_provider(
    state: State<'_, CommandState>,
    name: String,
) -> Result<(), String> {
    r_code_host::commands::settings_select_provider(&state, &name).await
}

/// 删除一个 Provider 与其系统凭据。
#[tauri::command]
pub async fn cmd_settings_delete_provider(
    state: State<'_, CommandState>,
    name: String,
) -> Result<(), String> {
    r_code_host::commands::settings_delete_provider(&state, &name).await
}

/// 获取 Codex CLI 外部协作入口状态（只读）。
#[tauri::command]
pub async fn cmd_codex_integration_status() -> Result<serde_json::Value, String> {
    r_code_host::commands::codex_integration_status().await
}

/// 在用户可见的终端中发起 Codex CLI 登录。
#[tauri::command]
pub async fn cmd_codex_start_login() -> Result<(), String> {
    r_code_host::commands::codex_start_login().await
}

/// 用户确认后，将 R-Code 的终端协作 Skill 安装到 Codex 目录。
#[tauri::command]
pub async fn cmd_codex_install_skill() -> Result<(), String> {
    r_code_host::commands::codex_install_skill().await
}

/// 列出工作区目录的直接子项（仅限受信任工作区）。
#[tauri::command]
pub async fn cmd_file_list(
    state: State<'_, CommandState>,
    workspace_path: String,
    path: Option<String>,
) -> Result<r_code_host::commands::FileTreeListing, String> {
    r_code_host::commands::file_list(&state, &workspace_path, path.as_deref()).await
}

/// 读取文件内容（限制在 project_root 内，512 KiB 截断）。
#[tauri::command]
pub async fn cmd_file_read(
    state: State<'_, CommandState>,
    workspace_path: String,
    path: String,
) -> Result<r_code_host::commands::FileContent, String> {
    r_code_host::commands::file_read(&state, &workspace_path, &path).await
}

/// 保存已经读取的文本文件；修订标识不匹配时拒绝覆盖磁盘上的新内容。
#[tauri::command]
pub async fn cmd_file_write(
    state: State<'_, CommandState>,
    workspace_path: String,
    path: String,
    content: String,
    expected_revision: String,
) -> Result<r_code_host::commands::FileContent, String> {
    r_code_host::commands::file_write(&state, &workspace_path, &path, &content, &expected_revision)
        .await
}

/// 读取验证记录的输出文本（blob 缺失时返回空串）。
#[tauri::command]
pub async fn cmd_verification_output(
    state: State<'_, CommandState>,
    id: String,
) -> Result<String, String> {
    r_code_host::commands::verification_output(&state, &id).await
}
