//! Tauri 命令包装层（bin 侧）——前端通过 IPC 调用的全部命令。
//!
//! 每个 `cmd_*` 都是薄包装：注入 `State<CommandState>`，委托给
//! `r_code_host::commands` 中的同名 inner（lib 侧可测核心）。
//! lib 不依赖 tauri（保持单元测试二进制无 GUI 链接）。

use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize, State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;
use tauri_plugin_opener::OpenerExt;

use agent_contract::InferenceOptions;
use r_code_core::dto::{
    AgentRun, AgentSendMode, FileChange, PermissionRequest, ProjectAccessMode, QueuedMessage,
    SessionBranch, Task, TaskMode, VerificationRecord, Workspace, WorkspaceMemoryMode,
};
use r_code_core::error::{ProductError, PROJECT_CONVERSATION_LIMIT_REACHED_CODE};
use r_code_core::plan::{
    AnswerPlanQuestionsInput, PlanReviewDecision, PlanView, UpdatePlanItemInput,
};
use r_code_core::plan_entry::PlanEntryDecisionInput;
use r_code_host::commands::{
    ChangeDiff, CodexCliPreferences, CommandState, NotificationPage, ProjectActivityPage,
    RecoveryCleanupResult, RecoveryPageData, SearchMatch, SessionMessage,
    SubagentSessionMessagePage, SubagentSessionMessagePageRequest, TaskDetail, TaskDetailBatch,
    TerminalInfo, TerminalRawBatch, TerminalRawSnapshot, WorkspaceDashboard, WorkspaceForgetResult,
};
use r_code_host::log_buffer::LogEntry;
use r_code_host::plan_entry_commands::{PlanEntryOfferView, PlanningStatusView};
use r_code_host::replay::ReplayEntry;
use r_code_store::{EnhancedReviewTarget, EnhancedReviewView, PlanRejectResult};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct CommandError {
    code: &'static str,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
}

impl From<ProductError> for CommandError {
    fn from(error: ProductError) -> Self {
        let (code, limit) = match &error {
            ProductError::ProjectConversationLimitReached { limit } => {
                (PROJECT_CONVERSATION_LIMIT_REACHED_CODE, Some(*limit))
            }
            _ => ("COMMAND_FAILED", None),
        };
        Self {
            code,
            message: error.to_string(),
            limit,
        }
    }
}

impl From<r_code_host::rtk::RtkError> for CommandError {
    fn from(error: r_code_host::rtk::RtkError) -> Self {
        Self {
            code: error.code().unwrap_or("COMMAND_FAILED"),
            message: error.to_string(),
            limit: None,
        }
    }
}

/// Explicit application exit. Window close is intentionally different on Windows: it hides the
/// main window to the notification area so active agents and terminals can continue running.
#[tauri::command]
pub fn cmd_app_quit(app: AppHandle) {
    app.exit(0);
}

/// Ensure the independently rendered companion WebView still exists before Settings enables it.
/// Window creation can fail transiently (for example while WebView2 is recovering); surfacing the
/// error lets the toggle roll back instead of persisting an impossible "enabled but absent" state.
#[tauri::command]
pub fn cmd_companion_ensure(app: AppHandle) -> Result<bool, String> {
    if let Err(error) = crate::setup_companion_window(&app) {
        tracing::warn!(%error, "failed to ensure the native companion window");
        return Err("native companion window is unavailable".to_string());
    }
    if app
        .get_webview_window(crate::COMPANION_WINDOW_LABEL)
        .is_none()
    {
        tracing::warn!(
            window_label = crate::COMPANION_WINDOW_LABEL,
            "native companion window is missing after a successful setup attempt"
        );
        return Err("native companion window is unavailable".to_string());
    }
    Ok(true)
}

#[tauri::command]
pub fn cmd_platform_capabilities() -> r_code_host::commands::PlatformCapabilities {
    r_code_host::commands::platform_capabilities()
}

/// 任务创建命令。 [doc-09]
#[tauri::command]
pub async fn cmd_task_create(
    state: State<'_, CommandState>,
    workspace_path: Option<String>,
    title: String,
    goal: String,
    mode: String,
    provider_name: Option<String>,
    agent_engine: Option<String>,
) -> Result<Task, CommandError> {
    r_code_host::commands::task_create_with_agent_typed(
        &state,
        workspace_path.as_deref(),
        &title,
        &goal,
        &mode,
        provider_name.as_deref(),
        agent_engine.as_deref(),
    )
    .await
    .map_err(CommandError::from)
}

/// 项目“+”专用：立即持久化一个按项目递增命名的空白对话。
#[tauri::command]
pub async fn cmd_project_conversation_create(
    state: State<'_, CommandState>,
    workspace_path: String,
) -> Result<Task, CommandError> {
    r_code_host::commands::project_conversation_create_typed(&state, &workspace_path)
        .await
        .map_err(CommandError::from)
}

/// 提前准备空白会话的 runtime/session；不会创建 Agent Run 或写入用户消息。
#[tauri::command]
pub async fn cmd_task_prepare(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<(), String> {
    r_code_host::commands::task_prepare(&state, &task_id).await
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

/// 将归档会话还原到项目任务列表。
#[tauri::command]
pub async fn cmd_task_restore(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<Task, String> {
    r_code_host::commands::task_restore(&state, &task_id).await
}

/// 切换空闲会话的主 Agent；下一次运行使用 R-Code provider 或 Codex CLI。
#[tauri::command]
pub async fn cmd_task_set_agent_engine(
    state: State<'_, CommandState>,
    task_id: String,
    agent_engine: String,
) -> Result<Task, String> {
    r_code_host::commands::task_set_agent_engine(&state, &task_id, &agent_engine).await
}

/// 永久删除已停止的会话；项目目录和工作区文件不在删除范围内。
#[tauri::command]
pub async fn cmd_task_delete(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<(), String> {
    r_code_host::commands::task_delete(&state, &task_id).await
}

/// 为一个既有会话附加已登记工作区；绑定后仅允许同路径幂等刷新。
#[tauri::command]
pub async fn cmd_task_set_workspace(
    state: State<'_, CommandState>,
    task_id: String,
    workspace_path: Option<String>,
) -> Result<Task, String> {
    r_code_host::commands::task_set_workspace(&state, &task_id, workspace_path.as_deref()).await
}

/// 为尚未绑定项目的会话选择并一次性附加本地工作区。
/// 用户取消返回 None；任务状态或唯一绑定校验失败时不登记 workspace。
#[tauri::command]
pub async fn cmd_task_choose_workspace(
    app: AppHandle,
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<Option<Task>, String> {
    let Some(path) = pick_workspace_folder(app).await? else {
        return Ok(None);
    };
    r_code_host::commands::task_attach_workspace(&state, &task_id, &path)
        .await
        .map(Some)
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

/// 切换空闲会话使用的具体模型；下一次运行生效。传 null 表示回退服务默认模型。
#[tauri::command]
pub async fn cmd_task_set_model(
    state: State<'_, CommandState>,
    task_id: String,
    model: Option<String>,
) -> Result<Task, String> {
    r_code_host::commands::task_set_model(&state, &task_id, model.as_deref()).await
}

/// 修改空闲会话的模型专属推理参数；空字段沿用服务默认值。
#[tauri::command]
pub async fn cmd_task_set_inference(
    state: State<'_, CommandState>,
    task_id: String,
    inference: InferenceOptions,
) -> Result<Task, String> {
    r_code_host::commands::task_set_inference(&state, &task_id, inference).await
}

/// 修改会话在列表中显示的名称。
#[tauri::command]
pub async fn cmd_task_rename(
    state: State<'_, CommandState>,
    task_id: String,
    title: String,
) -> Result<Task, String> {
    r_code_host::commands::task_rename(&state, &task_id, &title).await
}

/// Update or clear the durable goal used by Plan and subsequent turns.
#[tauri::command]
pub async fn cmd_task_update_goal(
    state: State<'_, CommandState>,
    task_id: String,
    goal: String,
) -> Result<Task, String> {
    r_code_host::commands::task_update_goal(&state, &task_id, &goal).await
}

/// Switch the task policy without rebuilding its native conversation history.
#[tauri::command]
pub async fn cmd_task_set_mode(
    state: State<'_, CommandState>,
    task_id: String,
    mode: String,
) -> Result<Task, String> {
    let mode =
        TaskMode::try_from_str(mode.trim()).ok_or_else(|| format!("invalid task mode: {mode}"))?;
    r_code_host::commands::task_set_mode(&state, &task_id, mode).await
}

#[tauri::command]
pub async fn cmd_plan_entry_offer_get(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<Option<PlanEntryOfferView>, String> {
    r_code_host::plan_entry_commands::plan_entry_offer_get(&state, &task_id).await
}

#[tauri::command]
pub async fn cmd_plan_entry_decide(
    state: State<'_, CommandState>,
    input: PlanEntryDecisionInput,
) -> Result<PlanEntryOfferView, String> {
    r_code_host::plan_entry_commands::plan_entry_decide(&state, &input).await
}

#[tauri::command]
pub async fn cmd_plan_entry_retry_continuation(
    state: State<'_, CommandState>,
    offer_id: String,
) -> Result<PlanEntryOfferView, String> {
    r_code_host::plan_entry_commands::plan_entry_retry_continuation(&state, &offer_id).await
}

#[tauri::command]
pub async fn cmd_planning_status(
    state: State<'_, CommandState>,
) -> Result<PlanningStatusView, String> {
    r_code_host::plan_entry_commands::planning_status(&state).await
}

#[tauri::command]
pub async fn cmd_plan_get(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<Option<PlanView>, String> {
    r_code_host::commands::plan_get(&state, &task_id).await
}

#[tauri::command]
pub async fn cmd_plan_create(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<PlanView, String> {
    r_code_host::commands::plan_create(&state, &task_id).await
}

#[tauri::command]
pub async fn cmd_plan_answer(
    state: State<'_, CommandState>,
    task_id: String,
    input: AnswerPlanQuestionsInput,
) -> Result<PlanView, String> {
    r_code_host::commands::plan_answer(&state, &task_id, input).await
}

#[tauri::command]
pub async fn cmd_plan_retry_continuation(
    state: State<'_, CommandState>,
    task_id: String,
    question_set_id: String,
) -> Result<PlanView, String> {
    r_code_host::commands::plan_retry_continuation(&state, &task_id, &question_set_id).await
}

#[tauri::command]
pub async fn cmd_plan_approve(
    state: State<'_, CommandState>,
    task_id: String,
    plan_id: String,
    expected_revision: u64,
) -> Result<PlanView, String> {
    r_code_host::commands::plan_approve(&state, &task_id, &plan_id, expected_revision).await
}

#[tauri::command]
pub async fn cmd_plan_retry_implementation(
    state: State<'_, CommandState>,
    task_id: String,
    plan_id: String,
) -> Result<PlanView, String> {
    r_code_host::commands::plan_retry_implementation(&state, &task_id, &plan_id).await
}

#[tauri::command]
pub async fn cmd_plan_cancel(
    state: State<'_, CommandState>,
    task_id: String,
    plan_id: String,
    expected_revision: u64,
) -> Result<PlanView, String> {
    r_code_host::commands::plan_cancel(&state, &task_id, &plan_id, expected_revision).await
}

#[tauri::command]
pub async fn cmd_plan_repair_projection(
    state: State<'_, CommandState>,
    task_id: String,
    plan_id: String,
) -> Result<PlanView, String> {
    r_code_host::commands::plan_repair_projection(&state, &task_id, &plan_id).await
}

#[tauri::command]
pub async fn cmd_plan_update_item(
    state: State<'_, CommandState>,
    task_id: String,
    input: UpdatePlanItemInput,
) -> Result<PlanView, String> {
    r_code_host::commands::plan_update_item(&state, &task_id, input).await
}

#[tauri::command]
pub async fn cmd_plan_review_status(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<Option<EnhancedReviewView>, String> {
    r_code_host::commands::plan_review_status(&state, &task_id)
}

#[tauri::command]
pub async fn cmd_plan_review_accept_file(
    state: State<'_, CommandState>,
    target: EnhancedReviewTarget,
) -> Result<PlanReviewDecision, String> {
    r_code_host::commands::plan_review_accept_file(&state, &target)
}

#[tauri::command]
pub async fn cmd_plan_review_accept_feature(
    state: State<'_, CommandState>,
    target: EnhancedReviewTarget,
) -> Result<PlanReviewDecision, String> {
    r_code_host::commands::plan_review_accept_feature(&state, &target)
}

#[tauri::command]
pub async fn cmd_plan_review_reject_file(
    state: State<'_, CommandState>,
    target: EnhancedReviewTarget,
) -> Result<PlanRejectResult, String> {
    r_code_host::commands::plan_review_reject_file(&state, &target).await
}

#[tauri::command]
pub async fn cmd_plan_review_reject_feature(
    state: State<'_, CommandState>,
    target: EnhancedReviewTarget,
) -> Result<PlanRejectResult, String> {
    r_code_host::commands::plan_review_reject_feature(&state, &target).await
}

/// 从当前上下文末端创建新的活跃会话分支。
#[tauri::command]
pub async fn cmd_task_fork_context(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<SessionBranch, String> {
    r_code_host::commands::task_fork_context(&state, &task_id).await
}

/// 在同一任务中切换到空白上下文；旧分支与 JSONL 只读保留。
#[tauri::command]
pub async fn cmd_task_clear_context(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<SessionBranch, String> {
    r_code_host::commands::task_clear_context(&state, &task_id).await
}

/// 手动压缩当前分支的模型上下文；完整聊天记录继续保留用于回看与审计。
#[tauri::command]
pub async fn cmd_task_compact_context(
    state: State<'_, CommandState>,
    task_id: String,
    focus: Option<String>,
) -> Result<r_code_host::commands::ContextCompactionResult, String> {
    r_code_host::commands::task_compact_context(&state, &task_id, focus.as_deref()).await
}

/// 获取任务详情（含事件、变更、权限、验证）。
#[tauri::command]
pub async fn cmd_task_detail(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<TaskDetail, String> {
    r_code_host::commands::task_detail(&state, &task_id).await
}

/// 批量获取任务详情，避免项目 / 活动页产生 IPC N+1。
#[tauri::command]
pub async fn cmd_task_detail_batch(
    state: State<'_, CommandState>,
    task_ids: Vec<String>,
) -> Result<TaskDetailBatch, String> {
    r_code_host::commands::task_detail_batch(&state, &task_ids).await
}

/// 发送用户消息到 Agent。 [doc-04 §7]
#[tauri::command]
pub async fn cmd_agent_send(
    state: State<'_, CommandState>,
    task_id: String,
    message: String,
    mode: Option<String>,
    attachments: Option<Vec<r_code_host::commands::AttachmentInput>>,
    attachment_ids: Option<Vec<String>>,
) -> Result<(), String> {
    let mode = match mode {
        Some(value) => AgentSendMode::try_from_str(&value)
            .ok_or_else(|| format!("invalid agent send mode: {value}"))?,
        None => AgentSendMode::Auto,
    };
    // 新引用链路（docs/archive/implementation/multimodal-attachments-and-deepseek-plan-anchoring-implementation.md §4.4）：只传 attachment id；
    // 旧 Base64 载荷仅继续服务尚未迁移的调用方。
    if let Some(ids) = attachment_ids.as_deref() {
        if !ids.is_empty() {
            return r_code_host::commands::agent_send_with_attachment_refs(
                &state, &task_id, &message, mode, ids,
            )
            .await;
        }
    }
    r_code_host::commands::agent_send_with_mode_and_attachments(
        &state,
        &task_id,
        &message,
        mode,
        attachments.as_deref().unwrap_or_default(),
    )
    .await
}

/// 附件 staging（docs §2.2 边界 1）：一次性 IPC Base64 → Blob 引用。
#[tauri::command]
pub async fn cmd_attachment_stage(
    state: State<'_, CommandState>,
    task_id: String,
    attachment: r_code_host::commands::AttachmentInput,
) -> Result<r_code_host::commands::AttachmentRefDto, String> {
    r_code_host::commands::attachment_stage(&state, &task_id, attachment).await
}

/// 删除草稿附件：立即释放 staged 引用与 Blob 计数。
#[tauri::command]
pub async fn cmd_attachment_discard(
    state: State<'_, CommandState>,
    task_id: String,
    attachment_id: String,
) -> Result<(), String> {
    r_code_host::commands::attachment_discard(&state, &task_id, &attachment_id).await
}

/// 按引用取回时间线图片附件预览（OCR 落盘原图或会话内联 Image 块）。
#[tauri::command]
pub async fn cmd_agent_attachment_preview(
    state: State<'_, CommandState>,
    task_id: String,
    reference: String,
) -> Result<r_code_host::commands::AttachmentPreviewPayload, String> {
    r_code_host::commands::agent_attachment_preview(&state, &task_id, &reference).await
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

/// 以只读 `codex exec --json` 委派当前主运行的一项子任务。
#[tauri::command]
pub async fn cmd_agent_delegate_codex(
    state: State<'_, CommandState>,
    task_id: String,
    goal: String,
    label: Option<String>,
) -> Result<AgentRun, String> {
    r_code_host::commands::agent_delegate_codex(&state, &task_id, &goal, label.as_deref()).await
}

/// 以持久 MCP 会话委派 Codex；完成后可保留外部 thread ID 供后续续接。
#[tauri::command]
pub async fn cmd_agent_delegate_codex_mcp(
    state: State<'_, CommandState>,
    task_id: String,
    goal: String,
    label: Option<String>,
) -> Result<AgentRun, String> {
    r_code_host::commands::agent_delegate_codex_mcp(&state, &task_id, &goal, label.as_deref()).await
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

/// 按界面从上到下的顺序重排当前分支尚未分发的消息。
#[tauri::command]
pub async fn cmd_agent_queue_reorder(
    state: State<'_, CommandState>,
    task_id: String,
    queue_ids: Vec<String>,
) -> Result<(), String> {
    r_code_host::commands::agent_queue_reorder(&state, &task_id, &queue_ids).await
}

/// 编辑一条尚未分发的队列消息。
#[tauri::command]
pub async fn cmd_agent_queue_update(
    state: State<'_, CommandState>,
    task_id: String,
    queue_id: String,
    message: String,
) -> Result<(), String> {
    r_code_host::commands::agent_queue_update(&state, &task_id, &queue_id, &message).await
}

/// 将点选的队列消息优先引导进当前运行；无法安全注入时保留在队首。
#[tauri::command]
pub async fn cmd_agent_queue_steer(
    state: State<'_, CommandState>,
    task_id: String,
    queue_id: String,
) -> Result<String, String> {
    r_code_host::commands::agent_queue_steer(&state, &task_id, &queue_id).await
}

/// M3-01：提交 Codex `requestUserInput` 的用户答案。answers 形如
/// `{ "<questionId>": ["答案"] }`；传 None 表示取消（编码为空答案集）。
/// 返回 "delivered" 或 "rejected"（迟到/未知/重复提交）。
#[tauri::command]
pub async fn cmd_codex_submit_user_input(
    state: State<'_, CommandState>,
    task_id: String,
    run_id: String,
    request_key: String,
    answers: Option<serde_json::Value>,
) -> Result<String, String> {
    let external_agents = state.external_agents.clone();
    let outcome = r_code_host::commands::submit_codex_user_input_for_task(
        &external_agents,
        &task_id,
        &run_id,
        &request_key,
        answers,
    )
    .await;
    Ok(match outcome {
        r_code_host::commands::ExternalUserInputOutcome::Delivered => "delivered".to_string(),
        r_code_host::commands::ExternalUserInputOutcome::Rejected => "rejected".to_string(),
    })
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

/// 列出可已读的顶栏通知。
#[tauri::command]
pub async fn cmd_notification_list(
    state: State<'_, CommandState>,
    cursor: Option<String>,
    limit: u32,
    unread_only: bool,
) -> Result<NotificationPage, String> {
    r_code_host::commands::notification_list(&state, cursor.as_deref(), limit, unread_only).await
}

/// 将一条通知标记为已读。
#[tauri::command]
pub async fn cmd_notification_mark_read(
    state: State<'_, CommandState>,
    notification_id: String,
) -> Result<bool, String> {
    r_code_host::commands::notification_mark_read(&state, &notification_id).await
}

/// 将全部通知标记为已读，返回受影响数量。
#[tauri::command]
pub async fn cmd_notification_mark_all_read(state: State<'_, CommandState>) -> Result<u64, String> {
    r_code_host::commands::notification_mark_all_read(&state).await
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

/// 回滚到本次运行最近一次绿灯 git checkpoint。
#[tauri::command]
pub async fn cmd_rollback_task_to_checkpoint(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<Vec<String>, String> {
    r_code_host::commands::rollback_task_to_checkpoint(&state, &task_id).await
}

/// 接受任务变更。
#[tauri::command]
pub async fn cmd_accept_task(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<(), String> {
    r_code_host::commands::accept_task(&state, &task_id).await
}

/// 获取应用内持久化审核状态（与 Git 暂存区无关）。
#[tauri::command]
pub async fn cmd_review_git_status(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<r_code_store::ReviewGitStatus, String> {
    r_code_host::commands::review_git_status(&state, &task_id)
}

/// 接受一条新增或删除行；仅写审核账本。
#[tauri::command]
pub async fn cmd_review_accept_line(
    state: State<'_, CommandState>,
    task_id: String,
    path: String,
    line_id: String,
) -> Result<r_code_store::ReviewAcceptResult, String> {
    r_code_host::commands::review_accept_line(&state, &task_id, &path, &line_id)
}

/// 接受一个任务文件；仅写审核账本。
#[tauri::command]
pub async fn cmd_review_accept_file(
    state: State<'_, CommandState>,
    task_id: String,
    path: String,
) -> Result<r_code_store::ReviewAcceptResult, String> {
    r_code_host::commands::review_accept_file(&state, &task_id, &path)
}

/// 接受该任务的全部待审核路径；仅写审核账本。
#[tauri::command]
pub async fn cmd_review_accept_all(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<r_code_store::ReviewAcceptResult, String> {
    r_code_host::commands::review_accept_all(&state, &task_id)
}

/// 拒绝一个文件并安全恢复其任务前内容。
#[tauri::command]
pub async fn cmd_review_reject_file(
    state: State<'_, CommandState>,
    task_id: String,
    path: String,
) -> Result<r_code_store::ReviewAcceptResult, String> {
    r_code_host::commands::review_reject_file(&state, &task_id, &path).await
}

/// 获取审核页 Git 提交与推送状态。
#[tauri::command]
pub async fn cmd_git_delivery_status(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<r_code_store::GitDeliveryStatus, String> {
    r_code_host::commands::git_delivery_status(&state, &task_id)
}

/// 显式将审核中保留的文件加入 Git 暂存区。
#[tauri::command]
pub async fn cmd_git_stage_accepted(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<r_code_store::GitDeliveryStatus, String> {
    r_code_host::commands::git_stage_accepted(&state, &task_id)
}

/// 生成可编辑的提交信息建议。
#[tauri::command]
pub async fn cmd_git_suggest_commit_message(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<String, String> {
    r_code_host::commands::git_suggest_commit_message(&state, &task_id)
}

/// 提交用户已显式加入暂存区的任务内容。
#[tauri::command]
pub async fn cmd_git_commit_task(
    state: State<'_, CommandState>,
    task_id: String,
    message: String,
) -> Result<r_code_store::GitCommitResult, String> {
    r_code_host::commands::git_commit_task(&state, &task_id, &message)
}

/// 将当前分支普通推送到已有 upstream。
#[tauri::command]
pub async fn cmd_git_push_task(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<r_code_store::GitPushResult, String> {
    r_code_host::commands::git_push_task(&state, &task_id)
}

/// 列出可调用的内置与自定义工作流 Skill。
#[tauri::command]
pub async fn cmd_workflow_skills_list(
    state: State<'_, CommandState>,
    workspace_path: Option<String>,
) -> Result<Vec<r_code_host::ScopedWorkflowSkill>, String> {
    r_code_host::commands::workflow_skills_list(&state, workspace_path.as_deref())
}

/// 新建或保存一条 Skill；内置 Skill 保存为用户级覆盖。
#[tauri::command]
pub async fn cmd_workflow_skill_save(
    state: State<'_, CommandState>,
    workspace_path: Option<String>,
    draft: r_code_host::ScopedWorkflowSkillDraft,
) -> Result<r_code_host::ScopedWorkflowSkill, String> {
    r_code_host::commands::workflow_skill_save(&state, workspace_path.as_deref(), draft)
}

/// 将内置 Skill 恢复为随应用发布的默认内容。
#[tauri::command]
pub async fn cmd_workflow_skill_reset(
    state: State<'_, CommandState>,
    id: String,
) -> Result<r_code_host::ScopedWorkflowSkill, String> {
    r_code_host::commands::workflow_skill_reset(&state, &id)
}

/// 删除自定义 Skill。内置 Skill 会被拒绝。
#[tauri::command]
pub async fn cmd_workflow_skill_delete(
    state: State<'_, CommandState>,
    workspace_path: Option<String>,
    scope: r_code_host::WorkflowSkillScope,
    id: String,
) -> Result<(), String> {
    r_code_host::commands::workflow_skill_delete(&state, workspace_path.as_deref(), scope, &id)
}

/// Atomically promote one project Skill into the global AppData catalog.
#[tauri::command]
pub async fn cmd_workflow_skill_sync_to_global(
    state: State<'_, CommandState>,
    workspace_path: String,
    id: String,
) -> Result<r_code_host::ScopedWorkflowSkill, String> {
    r_code_host::commands::workflow_skill_sync_to_global(&state, &workspace_path, &id)
}

#[tauri::command]
pub async fn cmd_knowledge_prompts_get(
    state: State<'_, CommandState>,
    workspace_path: Option<String>,
) -> Result<serde_json::Value, String> {
    r_code_host::commands::knowledge_prompts_get(&state, workspace_path.as_deref())
}

#[tauri::command]
pub async fn cmd_knowledge_prompts_save(
    state: State<'_, CommandState>,
    workspace_path: Option<String>,
    mode: r_code_host::settings::ProjectPromptMode,
    main_agent: String,
    subagent: String,
) -> Result<serde_json::Value, String> {
    r_code_host::commands::knowledge_prompts_save(
        &state,
        workspace_path.as_deref(),
        mode,
        &main_agent,
        &subagent,
    )
}

#[tauri::command]
pub async fn cmd_knowledge_prompts_reset(
    state: State<'_, CommandState>,
    workspace_path: Option<String>,
) -> Result<serde_json::Value, String> {
    r_code_host::commands::knowledge_prompts_reset(&state, workspace_path.as_deref())
}

/// 在审核阶段提出修改请求，启动下一轮 Agent 运行。
#[tauri::command]
pub async fn cmd_change_request(
    state: State<'_, CommandState>,
    task_id: String,
    message: String,
) -> Result<(), String> {
    r_code_host::commands::change_request(&state, &task_id, &message).await
}

/// 单文件变更 diff（blob 缺失时降级返回元信息）。
#[tauri::command]
pub async fn cmd_change_diff(
    state: State<'_, CommandState>,
    task_id: String,
    path: String,
    run_id: Option<String>,
) -> Result<ChangeDiff, String> {
    match run_id.as_deref() {
        Some(run_id) => {
            r_code_host::commands::change_diff_for_run(&state, &task_id, run_id, &path).await
        }
        None => r_code_host::commands::change_diff(&state, &task_id, &path).await,
    }
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

/// 清除 R-Code 内的项目及关联记录；不删除、移动或修改真实工作区目录。
#[tauri::command]
pub async fn cmd_workspace_forget(
    state: State<'_, CommandState>,
    workspace_path: String,
) -> Result<WorkspaceForgetResult, String> {
    r_code_host::commands::workspace_forget(&state, &workspace_path).await
}

/// 调用系统原生目录选择器；用户取消时返回 None，不把任何路径授予前端文件权限。
#[tauri::command]
pub async fn cmd_workspace_choose(
    app: AppHandle,
    state: State<'_, CommandState>,
) -> Result<Option<Workspace>, String> {
    let selected = pick_workspace_folder(app).await?;

    match selected {
        Some(path) => r_code_host::commands::workspace_open(&state, &path)
            .await
            .map(Some),
        None => Ok(None),
    }
}

async fn pick_workspace_folder(app: AppHandle) -> Result<Option<std::path::PathBuf>, String> {
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
    Ok(selected)
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

/// 以 generation CAS 更新项目记忆模式，并使未完成的旧快照失效。
#[tauri::command]
pub async fn cmd_workspace_set_memory_mode(
    state: State<'_, CommandState>,
    workspace_id: String,
    expected_generation: u64,
    memory_mode: WorkspaceMemoryMode,
) -> Result<Workspace, String> {
    r_code_host::commands::workspace_set_memory_mode(
        &state,
        &workspace_id,
        expected_generation,
        memory_mode,
    )
    .await
}

/// 获取项目仪表盘聚合数据。
#[tauri::command]
pub async fn cmd_workspace_dashboard(
    state: State<'_, CommandState>,
    workspace_path: String,
) -> Result<WorkspaceDashboard, String> {
    r_code_host::commands::workspace_dashboard(&state, &workspace_path).await
}

/// 获取项目级活动流。
#[tauri::command]
pub async fn cmd_project_activity_list(
    state: State<'_, CommandState>,
    workspace_path: String,
    cursor: Option<String>,
    limit: u32,
) -> Result<ProjectActivityPage, String> {
    r_code_host::commands::project_activity_list(&state, &workspace_path, cursor.as_deref(), limit)
        .await
}

/// 获取跨项目活动流。
#[tauri::command]
pub async fn cmd_activity_list(
    state: State<'_, CommandState>,
    cursor: Option<String>,
    limit: u32,
) -> Result<ProjectActivityPage, String> {
    r_code_host::commands::activity_list(&state, cursor.as_deref(), limit).await
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
    task_id: String,
) -> Result<Vec<TerminalInfo>, String> {
    r_code_host::commands::terminal_list(&state, &task_id).await
}

/// 创建终端。返回终端 ID。
#[tauri::command]
pub async fn cmd_terminal_create(
    state: State<'_, CommandState>,
    task_id: String,
    shell: String,
) -> Result<String, String> {
    r_code_host::commands::terminal_create(&state, &task_id, &shell).await
}

/// 使用已探测到的真实 CLI 路径创建交互式 Codex 终端。
#[tauri::command]
pub async fn cmd_terminal_create_codex(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<String, String> {
    r_code_host::commands::terminal_create_codex(&state, &task_id).await
}

/// 发送文本到终端。
#[tauri::command]
pub async fn cmd_terminal_send(
    state: State<'_, CommandState>,
    task_id: String,
    id: String,
    text: String,
    press_enter: bool,
) -> Result<(), String> {
    r_code_host::commands::terminal_send(&state, &task_id, &id, &text, press_enter).await
}

/// 读取终端输出。
#[tauri::command]
pub async fn cmd_terminal_read(
    state: State<'_, CommandState>,
    task_id: String,
    id: String,
) -> Result<String, String> {
    r_code_host::commands::terminal_read(&state, &task_id, &id).await
}

/// 读取终端完整保留输出。
#[tauri::command]
pub async fn cmd_terminal_snapshot(
    state: State<'_, CommandState>,
    task_id: String,
    id: String,
) -> Result<String, String> {
    r_code_host::commands::terminal_snapshot(&state, &task_id, &id).await
}

/// 读取终端原始快照，仅交由桌面终端模拟器渲染。
#[tauri::command]
pub async fn cmd_terminal_raw_snapshot(
    state: State<'_, CommandState>,
    task_id: String,
    id: String,
) -> Result<TerminalRawSnapshot, String> {
    r_code_host::commands::terminal_raw_snapshot(&state, &task_id, &id).await
}

/// 读取自指定游标以来的原始终端输出。
#[tauri::command]
pub async fn cmd_terminal_raw_since(
    state: State<'_, CommandState>,
    task_id: String,
    id: String,
    cursor: u64,
) -> Result<TerminalRawBatch, String> {
    r_code_host::commands::terminal_raw_since(&state, &task_id, &id, cursor).await
}

/// 终止终端。
#[tauri::command]
pub async fn cmd_terminal_kill(
    state: State<'_, CommandState>,
    task_id: String,
    id: String,
) -> Result<(), String> {
    r_code_host::commands::terminal_kill(&state, &task_id, &id).await
}

/// 调整终端 PTY 大小。
#[tauri::command]
pub async fn cmd_terminal_resize(
    state: State<'_, CommandState>,
    task_id: String,
    id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    r_code_host::commands::terminal_resize(&state, &task_id, &id, cols, rows).await
}

/// 获取恢复页面数据。 [doc-18 M10]
#[tauri::command]
pub async fn cmd_recovery_data(state: State<'_, CommandState>) -> Result<RecoveryPageData, String> {
    r_code_host::commands::recovery_data(&state).await
}

/// 收束启动前遗留的运行、工具调用与权限请求。
#[tauri::command]
pub async fn cmd_recovery_cleanup(
    state: State<'_, CommandState>,
) -> Result<RecoveryCleanupResult, String> {
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

/// 通过系统原生目录选择器导出支持包；取消选择属于正常操作，返回 `None`。
#[tauri::command]
pub async fn cmd_support_bundle_choose(
    app: AppHandle,
    state: State<'_, CommandState>,
) -> Result<Option<String>, String> {
    let selected = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("选择诊断支持包导出目录")
            .blocking_pick_folder()
            .map(|path| {
                path.into_path()
                    .map_err(|error| format!("invalid folder selection: {error}"))
            })
            .transpose()
    })
    .await
    .map_err(|error| format!("folder dialog worker failed: {error}"))??;

    match selected {
        Some(path) => r_code_host::commands::support_bundle(&state, &path.to_string_lossy())
            .await
            .map(Some),
        None => Ok(None),
    }
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

/// Read a historical branch without changing the active conversation.
#[tauri::command]
pub async fn cmd_session_messages_for_branch(
    state: State<'_, CommandState>,
    task_id: String,
    branch_id: String,
) -> Result<Vec<SessionMessage>, String> {
    r_code_host::commands::session_messages_for_branch(&state, &task_id, &branch_id).await
}

/// 读取子代理独立会话日志（详情面板数据源）。
#[tauri::command]
pub async fn cmd_subagent_session_messages(
    state: State<'_, CommandState>,
    task_id: String,
    subagent_id: String,
) -> Result<Vec<SessionMessage>, String> {
    r_code_host::commands::subagent_session_messages(&state, &task_id, &subagent_id).await
}

/// 增量读取子代理独立会话日志；支持最新窗口、追加轮询和向前加载历史。
#[tauri::command]
pub async fn cmd_subagent_session_message_page(
    state: State<'_, CommandState>,
    task_id: String,
    subagent_id: String,
    request: SubagentSessionMessagePageRequest,
) -> Result<SubagentSessionMessagePage, String> {
    r_code_host::commands::subagent_session_message_page(&state, &task_id, &subagent_id, request)
        .await
}

/// 检查旧版项目记忆文件是否存在以及是否被 Git 跟踪。
#[tauri::command]
pub async fn cmd_memory_overview(
    state: State<'_, CommandState>,
) -> Result<r_code_store::MemoryOverview, String> {
    r_code_host::commands::memory_overview(&state).await
}

#[tauri::command]
pub async fn cmd_memory_update_settings(
    state: State<'_, CommandState>,
    update: r_code_core::MemoryReviewSettingsUpdate,
) -> Result<r_code_core::MemoryReviewSettingsView, String> {
    r_code_host::commands::memory_update_settings(&state, update).await
}

#[tauri::command]
pub async fn cmd_memory_review_now(
    state: State<'_, CommandState>,
    workspace_id: Option<String>,
    workspace_path: Option<String>,
) -> Result<Option<String>, String> {
    r_code_host::commands::memory_review_now(
        &state,
        workspace_id.as_deref(),
        workspace_path.as_deref(),
    )
    .await
}

#[tauri::command]
pub async fn cmd_memory_retry_job(
    state: State<'_, CommandState>,
    job_id: String,
) -> Result<(), String> {
    r_code_host::commands::memory_retry_job(&state, &job_id).await
}

#[tauri::command]
pub async fn cmd_memory_cancel_job(
    state: State<'_, CommandState>,
    job_id: String,
) -> Result<(), String> {
    r_code_host::commands::memory_cancel_job(&state, &job_id).await
}

#[tauri::command]
pub async fn cmd_memory_add_entry(
    state: State<'_, CommandState>,
    draft: r_code_store::MemoryEntryDraft,
) -> Result<r_code_core::MemoryEntry, String> {
    r_code_host::commands::memory_add_entry(&state, draft).await
}

#[tauri::command]
pub async fn cmd_memory_edit_entry(
    state: State<'_, CommandState>,
    entry_id: String,
    edit: r_code_store::MemoryEntryEdit,
) -> Result<r_code_core::MemoryEntry, String> {
    r_code_host::commands::memory_edit_entry(&state, &entry_id, edit).await
}

#[tauri::command]
pub async fn cmd_memory_delete_entry(
    state: State<'_, CommandState>,
    entry_id: String,
    expected_version: u64,
) -> Result<(), String> {
    r_code_host::commands::memory_delete_entry(&state, &entry_id, expected_version).await
}

#[tauri::command]
pub async fn cmd_memory_approve_candidate(
    state: State<'_, CommandState>,
    candidate_id: String,
    edited_content: Option<String>,
) -> Result<r_code_core::MemoryEntry, String> {
    r_code_host::commands::memory_approve_candidate(
        &state,
        &candidate_id,
        edited_content.as_deref(),
    )
    .await
}

#[tauri::command]
pub async fn cmd_memory_reject_candidate(
    state: State<'_, CommandState>,
    candidate_id: String,
) -> Result<(), String> {
    r_code_host::commands::memory_reject_candidate(&state, &candidate_id).await
}

#[tauri::command]
pub async fn cmd_memory_clear_all(
    state: State<'_, CommandState>,
) -> Result<r_code_core::MemoryReviewSettingsView, String> {
    r_code_host::commands::memory_clear_all(&state).await
}

/// 检查旧版项目记忆文件是否存在以及是否被 Git 跟踪。
#[tauri::command]
pub async fn cmd_legacy_memory_status(
    state: State<'_, CommandState>,
    workspace_path: String,
) -> Result<r_code_host::LegacyMemoryStatus, String> {
    r_code_host::commands::legacy_memory_status(&state, &workspace_path).await
}

/// 读取最近的诊断日志条目（启动时已水合近七天尾部；level 如 "error"/"warn"）。
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

/// 读取经过脱敏的 MCP 配置与实时状态；不会因查看设置页而启动第三方进程。
#[tauri::command]
pub async fn cmd_mcp_snapshot(
    state: State<'_, CommandState>,
) -> Result<r_code_host::mcp_manager::McpManagerSnapshot, String> {
    r_code_host::commands::mcp_snapshot(&state).await
}

/// 新建或编辑自定义 MCP。新增及启动形态变化后保持关闭，等待单独确认。
#[tauri::command]
pub async fn cmd_mcp_upsert(
    state: State<'_, CommandState>,
    request: r_code_host::mcp_manager::McpUpsertRequest,
) -> Result<r_code_host::mcp_manager::McpServerView, String> {
    r_code_host::commands::mcp_upsert(&state, request).await
}

#[tauri::command]
pub async fn cmd_mcp_remove(
    state: State<'_, CommandState>,
    server_id: String,
) -> Result<(), String> {
    r_code_host::commands::mcp_remove(&state, &server_id).await
}

/// 开关 MCP；第三方首次开启分两步，第一次返回逐字段预览和一次性令牌。
#[tauri::command]
pub async fn cmd_mcp_toggle(
    state: State<'_, CommandState>,
    server_id: String,
    enabled: bool,
    confirmation_token: Option<String>,
) -> Result<r_code_host::mcp_manager::McpToggleResult, String> {
    r_code_host::commands::mcp_toggle(&state, &server_id, enabled, confirmation_token.as_deref())
        .await
}

#[tauri::command]
pub async fn cmd_mcp_test_connection(
    state: State<'_, CommandState>,
    server_id: String,
) -> Result<Vec<r_code_mcp::McpToolDescriptor>, String> {
    r_code_host::commands::mcp_test_connection(&state, &server_id).await
}

#[tauri::command]
pub async fn cmd_mcp_credential_status(
    state: State<'_, CommandState>,
    server_id: String,
) -> Result<Vec<r_code_host::mcp_manager::McpCredentialStatus>, String> {
    r_code_host::commands::mcp_credential_status(&state, &server_id).await
}

#[tauri::command]
pub async fn cmd_mcp_set_credential(
    state: State<'_, CommandState>,
    server_id: String,
    name: String,
    value: String,
) -> Result<(), String> {
    r_code_host::commands::mcp_set_credential(&state, &server_id, &name, &value).await
}

#[tauri::command]
pub async fn cmd_mcp_delete_credential(
    state: State<'_, CommandState>,
    server_id: String,
    name: String,
) -> Result<(), String> {
    r_code_host::commands::mcp_delete_credential(&state, &server_id, &name).await
}

#[tauri::command]
pub async fn cmd_mcp_market_search(
    state: State<'_, CommandState>,
    query: Option<String>,
    cursor: Option<String>,
    limit: usize,
) -> Result<r_code_mcp::MarketPage, String> {
    r_code_host::commands::mcp_market_search(&state, query.as_deref(), cursor.as_deref(), limit)
        .await
}

#[tauri::command]
pub async fn cmd_mcp_market_prepare_install(
    state: State<'_, CommandState>,
    request: r_code_host::mcp_manager::McpMarketInstallRequest,
) -> Result<r_code_mcp::LaunchPreview, String> {
    r_code_host::commands::mcp_market_prepare_install(&state, &request)
}

#[tauri::command]
pub async fn cmd_mcp_market_install(
    state: State<'_, CommandState>,
    request: r_code_host::mcp_manager::McpMarketInstallRequest,
    confirmation_token: String,
) -> Result<r_code_host::mcp_manager::McpServerView, String> {
    r_code_host::commands::mcp_market_install(&state, &request, &confirmation_token).await
}

/// 获取内置模型服务目录，驱动设置页的"新建服务"表单。
#[tauri::command]
pub async fn cmd_provider_catalog() -> Result<serde_json::Value, String> {
    r_code_host::commands::provider_catalog().await
}

#[tauri::command]
pub async fn cmd_subagent_provider_catalog(
    state: State<'_, CommandState>,
) -> Result<r_code_host::subagent_providers::CatalogSnapshot, String> {
    r_code_host::commands::subagent_provider_catalog(&state).await
}

#[tauri::command]
pub async fn cmd_subagent_provider_test(
    state: State<'_, CommandState>,
    request: r_code_host::subagent_providers::SubagentProviderProbeRequest,
) -> Result<r_code_host::subagent_providers::SubagentProviderProbeResponse, String> {
    r_code_host::commands::subagent_provider_test(&state, request).await
}

#[tauri::command]
pub async fn cmd_subagent_provider_test_batch(
    state: State<'_, CommandState>,
    requests: Vec<r_code_host::subagent_providers::SubagentProviderProbeRequest>,
) -> Result<r_code_host::subagent_providers::SubagentProviderProbeBatchResponse, String> {
    r_code_host::commands::subagent_provider_test_batch(&state, requests).await
}

#[tauri::command]
pub async fn cmd_subagent_pool_snapshot(
    state: State<'_, CommandState>,
) -> Result<r_code_host::subagent_providers::SubagentPoolSnapshot, String> {
    r_code_host::commands::subagent_pool_snapshot(&state).await
}

#[tauri::command]
pub async fn cmd_subagent_pool_save(
    state: State<'_, CommandState>,
    revision: String,
    pool: agent_config::SubagentPoolConfig,
) -> Result<r_code_host::subagent_providers::SubagentPoolSnapshot, String> {
    r_code_host::commands::subagent_pool_save(&state, &revision, pool).await
}

/// 模型胶囊悬停时按需读取 DeepSeek 官方账户余额。
#[tauri::command]
pub async fn cmd_provider_balance(
    state: State<'_, CommandState>,
    request: r_code_host::commands::ProviderBalanceInput,
) -> Result<serde_json::Value, String> {
    r_code_host::commands::provider_balance(&state, request).await
}

/// 从当前 Provider 的模型目录端点读取可用模型。
#[tauri::command]
pub async fn cmd_provider_models(
    state: State<'_, CommandState>,
    request: r_code_host::commands::ProviderModelsInput,
) -> Result<serde_json::Value, String> {
    r_code_host::commands::provider_models(&state, request).await
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

/// 删除一个 Provider 与其本机凭据。
#[tauri::command]
pub async fn cmd_settings_delete_provider(
    state: State<'_, CommandState>,
    name: String,
) -> Result<(), String> {
    r_code_host::commands::settings_delete_provider(&state, &name).await
}

/// Detect the verified RTK binary and R-Code-owned enable marker.
#[tauri::command]
pub async fn cmd_rtk_status(
    state: State<'_, CommandState>,
) -> Result<r_code_host::rtk::RtkStatus, String> {
    r_code_host::commands::rtk_status(&state).await
}

/// A4：请求信封审计自检计数（headers_appended, mismatches）。Real runtime
/// 不在场时返回 None；soak 期间经 devtools 消费，不进设置 UI。
#[tauri::command]
pub async fn cmd_request_audit_counters(
    state: State<'_, CommandState>,
    task_id: String,
) -> Result<Option<(usize, usize)>, String> {
    r_code_host::commands::request_audit_counters(&state, &task_id).await
}

/// Install RTK from its verified official release when needed, then atomically toggle policy.
#[tauri::command]
pub async fn cmd_rtk_set_enabled(
    state: State<'_, CommandState>,
    enabled: bool,
) -> Result<r_code_host::rtk::RtkStatus, CommandError> {
    r_code_host::commands::rtk_set_enabled(&state, enabled)
        .await
        .map_err(CommandError::from)
}

/// Open the Windows Security exclusions page so the user can allow the RTK binary after Windows
/// Defender quarantined it. The custom protocol is handled by the Windows Security app, which is
/// the only trusted channel when tamper protection blocks scripted exclusion changes.
#[tauri::command]
pub fn cmd_rtk_open_security_exclusions(app: AppHandle) -> Result<(), String> {
    app.opener()
        .open_url(
            r_code_host::rtk::WINDOWS_SECURITY_EXCLUSIONS_URL,
            None::<&str>,
        )
        .map_err(|error| format!("open Windows Security exclusions page: {error}"))
}

/// 获取 Codex CLI 外部协作入口状态（只读）。
#[tauri::command]
pub async fn cmd_codex_integration_status() -> Result<serde_json::Value, String> {
    r_code_host::commands::codex_integration_status().await
}

/// 前端展示固定命令并获得用户明确确认后，安装官方 Codex CLI npm 包。
#[tauri::command]
pub async fn cmd_codex_install_cli(
    state: State<'_, CommandState>,
) -> Result<serde_json::Value, String> {
    r_code_host::commands::codex_install_cli(&state).await
}

/// 进入 Codex 运行时设置时检查更新；已安装且有新版时由官方 CLI 自动升级。
#[tauri::command]
pub async fn cmd_codex_sync_cli(
    state: State<'_, CommandState>,
) -> Result<serde_json::Value, String> {
    r_code_host::commands::codex_sync_cli(&state).await
}

/// 在用户可见的终端中发起 Codex CLI 登录。
#[tauri::command]
pub async fn cmd_codex_start_login() -> Result<(), String> {
    r_code_host::commands::codex_start_login().await
}

/// 在用户可见的终端中发起 Codex CLI 设备码登录。
#[tauri::command]
pub async fn cmd_codex_start_device_login() -> Result<(), String> {
    r_code_host::commands::codex_start_device_login().await
}

/// 用户确认后，将 R-Code 的终端协作 Skill 安装到 Codex 目录。
#[tauri::command]
pub async fn cmd_codex_install_skill() -> Result<(), String> {
    r_code_host::commands::codex_install_skill().await
}

/// 用户确认后向 Codex 注册本机 R-Code MCP server。
#[tauri::command]
pub async fn cmd_codex_install_mcp_server(state: State<'_, CommandState>) -> Result<(), String> {
    r_code_host::commands::codex_install_mcp_server(&state).await
}

/// 用户一次确认后，更新协作 Skill 并补齐 R-Code MCP 配置。
#[tauri::command]
pub async fn cmd_codex_setup_collaboration(
    state: State<'_, CommandState>,
) -> Result<serde_json::Value, String> {
    r_code_host::commands::codex_setup_collaboration(&state).await
}

/// 仅在 Codex 已登录后读取 CLI 实际可用模型与当前运行偏好。
#[tauri::command]
pub async fn cmd_codex_cli_preferences() -> Result<CodexCliPreferences, String> {
    r_code_host::commands::codex_cli_preferences().await
}

/// 保存 Codex CLI 的模型、推理强度、回复详细度与子代理权限；空模型字段恢复默认。
#[tauri::command]
pub async fn cmd_codex_save_cli_preferences(
    state: State<'_, CommandState>,
    model: Option<String>,
    reasoning_effort: Option<String>,
    verbosity: Option<String>,
    permission_mode: Option<String>,
) -> Result<CodexCliPreferences, String> {
    r_code_host::commands::codex_save_cli_preferences(
        &state,
        model.as_deref(),
        reasoning_effort.as_deref(),
        verbosity.as_deref(),
        permission_mode.as_deref(),
    )
    .await
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

/// Resolve a local Markdown/artifact target and classify it against the attached workspace.
#[tauri::command]
pub async fn cmd_local_file_target(
    state: State<'_, CommandState>,
    workspace_path: Option<String>,
    reference: String,
) -> Result<r_code_host::commands::LocalFileTarget, String> {
    r_code_host::commands::resolve_local_file_target(&state, workspace_path.as_deref(), &reference)
}

/// Return bounded image bytes through Tauri's binary IPC response (not JSON/base64).
#[tauri::command]
pub async fn cmd_local_image_preview(
    state: State<'_, CommandState>,
    workspace_path: Option<String>,
    reference: String,
) -> Result<tauri::ipc::Response, String> {
    let (_, bytes) =
        r_code_host::commands::local_image_preview(&state, workspace_path.as_deref(), &reference)?;
    Ok(tauri::ipc::Response::new(bytes))
}

/// Reveal an already resolved local path using the platform file manager.
#[tauri::command]
pub async fn cmd_reveal_local_path(path: String) -> Result<(), String> {
    r_code_host::system_integration::reveal_in_file_manager(std::path::Path::new(&path))
}

/// Make room for the docked workbench without granting window-mutation permissions to JS.
#[tauri::command]
pub async fn cmd_prepare_workbench_window(window: WebviewWindow) -> Result<bool, String> {
    if window.is_maximized().map_err(|error| error.to_string())?
        || window.is_fullscreen().map_err(|error| error.to_string())?
    {
        return Ok(false);
    }
    let Some(monitor) = window
        .current_monitor()
        .map_err(|error| error.to_string())?
    else {
        return Ok(false);
    };
    let position = window.outer_position().map_err(|error| error.to_string())?;
    let size = window.outer_size().map_err(|error| error.to_string())?;
    let work_area = monitor.work_area();
    let current = r_code_host::system_integration::DesktopRect {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
    };
    let available = r_code_host::system_integration::DesktopRect {
        x: work_area.position.x,
        y: work_area.position.y,
        width: work_area.size.width,
        height: work_area.size.height,
    };
    let Some(target) = r_code_host::system_integration::workbench_window_rect(
        current,
        available,
        monitor.scale_factor(),
    ) else {
        return Ok(false);
    };

    if target.x != current.x || target.y != current.y {
        window
            .set_position(PhysicalPosition::new(target.x, target.y))
            .map_err(|error| error.to_string())?;
    }
    window
        .set_size(PhysicalSize::new(target.width, target.height))
        .map_err(|error| error.to_string())?;
    Ok(true)
}

/// 读取验证记录的输出文本（blob 缺失时返回空串）。
#[tauri::command]
pub async fn cmd_verification_output(
    state: State<'_, CommandState>,
    id: String,
) -> Result<String, String> {
    r_code_host::commands::verification_output(&state, &id).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn project_limit_error_serializes_the_stable_tauri_contract() {
        let payload = serde_json::to_value(CommandError::from(
            ProductError::ProjectConversationLimitReached { limit: 5 },
        ))
        .unwrap();

        assert_eq!(
            payload,
            json!({
                "code": "PROJECT_CONVERSATION_LIMIT_REACHED",
                "message": "该项目最多保留 5 个未归档对话，请先归档一个后再新建",
                "limit": 5,
            })
        );
    }

    #[test]
    fn unrelated_command_error_does_not_impersonate_the_limit_contract() {
        let payload = serde_json::to_value(CommandError::from(ProductError::Other(
            "该项目最多保留 5 个未归档对话，请先归档一个后再新建".to_string(),
        )))
        .unwrap();

        assert_eq!(
            payload,
            json!({
                "code": "COMMAND_FAILED",
                "message": "该项目最多保留 5 个未归档对话，请先归档一个后再新建",
            })
        );
    }
}
