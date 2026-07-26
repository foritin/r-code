//! Tauri 命令核心 -- 前端通过 IPC 调用的命令实现。 [doc-09]
//!
//! ## 结构
//! - 本模块（lib 侧）为可测试的核心逻辑：`pub async fn *` 接收 `&CommandState`
//! - `#[tauri::command] cmd_*` 薄包装在 **bin 侧** `tauri_commands.rs`
//!   （lib 不依赖 tauri —— 保持单元测试二进制无 GUI/comctl32 链接）
//! - 返回 `Result<T, String>`，错误被字符串化以便 IPC 传输
//!
//! ## 命令分组
//! - **任务**：创建/列表/详情
//! - **Agent**：发送消息/中止（MockAgentRuntime 驱动，drain 循环 emit `agent-event`）
//! - **权限**：审批/列出待审批
//! - **变更**：列出/回滚/接受/单文件 diff
//! - **验证**：运行/列出
//! - **Workspace**：列表/打开
//! - **搜索**：快速打开/全局搜索
//! - **终端**：列表/创建/发送/读取/终止/调整大小
//! - **恢复**：恢复页面数据/清理孤儿权限/支持包
//! - **回放**：三层深度回放/会话消息序列
//! - **项目记忆**：读取/写入
//! - **设置**：获取/设置
//!
//! [doc-09] [doc-11]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use hermes_core::{Message, SessionEvent, SessionMeta};
use hermes_store::SessionStore;
use r_code_agent_worker::{AgentRuntime, MockAgentRuntime, SteerResult};
use r_code_core::dto::{
    AgentEvent, AgentEventScope, AgentRun, AgentSendMode, CreateSessionInput, FileChange,
    FileChangeType, PermissionDecision, PermissionRequest, PlanStep, ProjectAccessMode,
    QueuedMessage, QueuedMessageState, ReviewState, RiskLevel, SessionBranch, SubagentState, Task,
    TaskEvent, TaskEventType, TaskMode, TaskState, ToolCall, VerificationRecord, Workspace,
};
use r_code_core::error::ProductError;
use r_code_core::security::PathGuard;
use r_code_gateway::permission::PermissionEngine;
use r_code_store::repositories::VERIFICATION_PLACEHOLDER_MODEL;
use r_code_store::review::ReviewAction;
use r_code_store::{
    AgentRunRepository, BlobStore, ChangeService, Database, QueuedMessageRepository, ReviewService,
    SessionBranchRepository, TaskEventStore, TaskRepository, ToolCallRepository,
    VerificationConfig, VerificationService, WorkspaceService,
};
use r_code_terminal::{SendOptions, TerminalControlService, TerminalManager};
use serde::{Deserialize, Serialize};

use crate::project_memory::ProjectMemory;
use crate::provider_catalog::{Preset as ProviderPreset, Protocol as ProviderProtocol};
use crate::recovery::RecoveryManager;
use crate::replay::{ReplayDepth, ReplayService};
use crate::search::SearchService;
use crate::settings::SettingsService;
use crate::skills::SkillManager;
use crate::support_bundle::SupportBundle;

// ============================================================================
// AgentBridge -- Mock runtime + 任务会话映射
// ============================================================================

#[derive(Debug, Clone)]
struct BridgeSession {
    runtime_session_id: String,
    branch_id: String,
    storage_id: String,
}

#[derive(Debug, Clone)]
struct ActiveRun {
    task_id: String,
    branch_id: String,
    runtime_session_id: String,
    run_id: String,
}

/// 每个运行作用域独立积累流式文本，确保并行子代理不会把增量拼到主回复。
#[derive(Default)]
struct PendingAssistantText {
    text: String,
    saw_delta: bool,
    storage_id: String,
}

/// Agent 桥接层 -- 持有 runtime（真实 provider / Mock）、任务会话映射与全局串行调度状态。
///
/// 真实模式由 `enable_real_mode` 开启（生产路径）；开启后首个 agent_send 按
/// Settings 的 provider 配置构建 LlmAgentRuntime，配置缺失/无效直接报错，不做降级。
/// Mock 路径仅用于测试与无 provider 的开发演示。
pub struct AgentBridge {
    kind: AgentRuntimeKind,
    /// task_id → 当前活跃分支的 runtime session
    sessions: HashMap<String, BridgeSession>,
    /// 物理 runtime 一次只执行一个 run，避免其全局流事件队列彼此混淆。
    active: Option<ActiveRun>,
    /// 真实模式开关
    real_mode: bool,
    /// 当前真实 runtime 的配置指纹（provider|base_url|model|api_key）
    fingerprint: Option<String>,
}

impl AgentBridge {
    fn new() -> Self {
        Self {
            kind: AgentRuntimeKind::Mock(MockAgentRuntime::new()),
            sessions: HashMap::new(),
            active: None,
            real_mode: false,
            fingerprint: None,
        }
    }
}

// ============================================================================
// CommandState -- 命令执行所需的全局状态
// ============================================================================

/// 命令状态 -- 持有所有命令执行所需的服务与存储。
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
    /// Agent 桥接（Mock runtime）
    pub agent: Arc<tokio::sync::Mutex<AgentBridge>>,
    /// Agent 事件出口（bin 侧注入，drain 循环经此转发 WebView；测试环境为 None）
    pub agent_event_sink: Mutex<Option<AgentEventSink>>,
    /// 工具门（内置工具 + 权限门 + 审计账本），真实 runtime 的 ToolHost 来源
    pub tool_gateway: Arc<r_code_gateway::ToolGateway>,
}

/// Agent 事件出口闭包（task_id, event）——由 bin 侧用 AppHandle 实现 emit。
pub type AgentEventSink = Arc<dyn Fn(&str, &AgentEvent) + Send + Sync>;

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
        let permission_engine = Arc::new(PermissionEngine::new());
        let mut gateway = r_code_gateway::ToolGateway::new(permission_engine.clone());
        // 只读（R0/R1）
        gateway.register(Box::new(r_code_gateway::ReadFileTool));
        gateway.register(Box::new(r_code_gateway::ListFilesTool));
        gateway.register(Box::new(r_code_gateway::SearchTool));
        gateway.register(Box::new(r_code_gateway::GlobTool));
        gateway.register(Box::new(r_code_gateway::GitStatusTool));
        gateway.register(Box::new(r_code_gateway::LoadSkillTool));
        // 写入（R2）
        gateway.register(Box::new(r_code_gateway::EditTool));
        gateway.register(Box::new(r_code_gateway::ApplyPatchTool));
        gateway.register(Box::new(r_code_gateway::CreateFileTool));
        gateway.register(Box::new(r_code_gateway::DeleteFileTool));
        // 命令执行（静态 R3；实际等级由 classify_shell_command 按命令内容判定）
        gateway.register(Box::new(r_code_gateway::BashTool));
        Self {
            db,
            blobs_dir,
            session_store: SessionStore::new(sessions_dir.clone()),
            sessions_dir,
            permission_engine,
            terminal_manager: Arc::new(TerminalManager::new()),
            config_dir,
            project_root,
            db_path,
            agent: Arc::new(tokio::sync::Mutex::new(AgentBridge::new())),
            agent_event_sink: Mutex::new(None),
            tool_gateway: Arc::new(gateway),
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

    /// 注入 agent 事件出口（bin 侧 setup 时调用一次）。
    pub fn set_agent_event_sink(&self, sink: AgentEventSink) {
        *self.agent_event_sink.lock().unwrap() = Some(sink);
    }

    /// 向 WebView 广播 agent 事件（未注入出口时静默跳过，如测试环境）。
    pub fn emit_agent_event(&self, task_id: &str, event: &AgentEvent) {
        let sink = self.agent_event_sink.lock().unwrap().clone();
        if let Some(sink) = sink {
            sink(task_id, event);
        }
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
    /// 当前视图对应的活跃会话分支
    pub active_branch: SessionBranch,
    /// 全部分支元数据；当前界面默认只显示 active_branch，其余供审计/回放使用。
    pub branches: Vec<SessionBranch>,
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
    /// 当前活跃分支上等待调度的消息
    pub queued_messages: Vec<QueuedMessage>,
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

/// 会话消息序列条目（Room 时间线数据源）。
///
/// 由 `{taskId}.jsonl` 的 SessionEvent 逐行转换而来。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    /// 稳定消息标识（`{storage_id}:{line}`），用于编辑后分叉。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// 本次读取的会话分支。
    pub branch_id: String,
    /// 条目类型：meta / message / tool_call / tool_result / system
    pub kind: String,
    /// message 的角色（user / assistant）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// message / system 的文本内容
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// tool_call 的工具名
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// tool_call / tool_result 的调用 ID（JSONL 不存 call_id，按顺序关联补齐）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// tool_call 的输入 JSON
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_json: Option<String>,
    /// tool_result 的输出 JSON
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_json: Option<String>,
    /// tool_result 是否错误
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    /// 时间戳（RFC3339；仅 Meta 有）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
}

/// Diff 行类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeDiffLineKind {
    /// 上下文行
    Ctx,
    /// 新增行
    Add,
    /// 删除行
    Del,
    /// hunk 分隔（省略段标记）
    Hunk,
}

/// 单文件 diff 的一行。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeDiffLine {
    /// 行类型
    pub kind: ChangeDiffLineKind,
    /// 行文本（不含换行符）
    pub text: String,
    /// 旧文件行号（ctx / del）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_no: Option<usize>,
    /// 新文件行号（ctx / add）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_no: Option<usize>,
}

/// 单文件变更 diff（cmd_change_diff 返回）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeDiff {
    /// 是否支持 diff（blob 内容齐备）；false 时前端降级显示元信息
    pub supported: bool,
    /// 文件路径
    pub path: String,
    /// 变更类型
    #[serde(skip_serializing_if = "Option::is_none")]
    pub change_type: Option<FileChangeType>,
    /// before 内容 blob hash
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_hash: Option<String>,
    /// after 内容 blob hash
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_hash: Option<String>,
    /// diff 行序列
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines: Option<Vec<ChangeDiffLine>>,
    /// 是否因行数超限被截断（降级为全删全增）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
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

/// 从已打开的 workspace 记录得到一个规范且存在的根目录。
///
/// 前端不能把任意路径当作工作区作用域传入：路径必须已经由 `workspace_open`
/// 校验并持久化。每次使用前重新 canonicalize，避免挂载点/符号链接变化后继续
/// 按旧字符串访问。
fn workspace_root(state: &CommandState, workspace_path: &str) -> Result<PathBuf, String> {
    let workspace = WorkspaceService::new(&state.db)
        .get(workspace_path)
        .map_err(err_str)?
        .ok_or_else(|| "workspace is not open; choose the folder before using it".to_string())?;
    let root = PathBuf::from(&workspace.canonical_path)
        .canonicalize()
        .map_err(|e| format!("workspace is no longer accessible: {e}"))?;
    if !root.is_dir() {
        return Err("workspace path is not a directory".to_string());
    }
    Ok(root)
}

/// 已附加工作区可供用户直接操作；所有文件路径仍在 `workspace_root` 的 PathGuard
/// 边界内。Agent 自主调用是否需要批准由项目 `access_mode` 决定。
fn attached_workspace_root(state: &CommandState, workspace_path: &str) -> Result<PathBuf, String> {
    workspace_root(state, workspace_path)
}

/// 不依赖完整 CommandState 的工作区绑定版本，供后台队列调度器复用。
fn task_workspace_binding_from_db(
    db: &Database,
    task: &Task,
) -> Result<(Option<String>, ProjectAccessMode), String> {
    let Some(workspace_path) = task.workspace_path.as_deref() else {
        return Ok((None, ProjectAccessMode::RequestApproval));
    };
    let workspace = WorkspaceService::new(db)
        .get(workspace_path)
        .map_err(err_str)?
        .ok_or_else(|| "task workspace is no longer registered".to_string())?;
    let root = PathBuf::from(&workspace.canonical_path)
        .canonicalize()
        .map_err(|e| format!("workspace is no longer accessible: {e}"))?;
    if !root.is_dir() {
        return Err("workspace path is not a directory".to_string());
    }
    Ok((Some(root.display().to_string()), workspace.access_mode))
}

fn attached_task_workspace_root(state: &CommandState, task_id: &str) -> Result<PathBuf, String> {
    let task = TaskRepository::new(&state.db)
        .get(task_id)
        .map_err(err_str)?
        .ok_or_else(|| format!("task not found: {task_id}"))?;
    let path = task.workspace_path.ok_or_else(|| {
        "this conversation has no workspace; attach a folder before using local tools".to_string()
    })?;
    attached_workspace_root(state, &path)
}

fn resolve_workspace_path(root: &Path, path: &str) -> Result<PathBuf, String> {
    let guard = PathGuard::new(root.to_path_buf()).map_err(err_str)?;
    let requested = PathBuf::from(path);
    let candidate = if requested.is_absolute() {
        requested
    } else {
        guard.root().join(requested)
    };
    guard.resolve(&candidate).map_err(err_str)
}

/// 将用户/历史路径解析成安全的物理路径，同时找回与其等价的持久化路径键。
/// Windows 上 `canonicalize` 可能添加 `\\?\\` 前缀；如果直接拿规范化路径查
/// 旧 baseline，会误判为没有基线。这里始终以 PathGuard 验证物理路径，再按
/// canonical 等价关系复用已记录的键。
async fn rollback_target(
    service: &ChangeService<'_>,
    task_id: &str,
    root: &Path,
    requested_path: &str,
) -> Result<(String, PathBuf), String> {
    let physical_path = resolve_workspace_path(root, requested_path)?;
    let guard = PathGuard::new(root.to_path_buf()).map_err(err_str)?;
    for baseline in service.list_baselines(task_id).await.map_err(err_str)? {
        let stored = PathBuf::from(&baseline.path);
        let candidate = if stored.is_absolute() {
            stored
        } else {
            root.join(stored)
        };
        if let Ok(resolved) = guard.resolve(&candidate) {
            if resolved == physical_path {
                return Ok((baseline.path, physical_path));
            }
        }
    }
    Ok((physical_path.display().to_string(), physical_path))
}

// ============================================================================
// 任务命令
// ============================================================================

pub async fn task_create(
    state: &CommandState,
    workspace_path: Option<&str>,
    title: &str,
    goal: &str,
    mode: &str,
) -> Result<Task, String> {
    task_create_with_provider(state, workspace_path, title, goal, mode, None).await
}

/// 创建任务并可选地绑定一个已就绪的模型服务。
///
/// 保留无 provider 的旧入口用于兼容既有 IPC/测试；桌面端新建会话会传入显式选择，
/// 从而不因全局默认服务随后变化而改变该会话的后续运行。
pub async fn task_create_with_provider(
    state: &CommandState,
    workspace_path: Option<&str>,
    title: &str,
    goal: &str,
    mode: &str,
    provider_name: Option<&str>,
) -> Result<Task, String> {
    let mode = parse_mode(mode)?;
    let workspace_path = workspace_path
        .filter(|path| !path.trim().is_empty())
        .map(|path| workspace_root(state, path).map(|root| root.display().to_string()))
        .transpose()?;
    let provider_name = validate_selected_provider(state, provider_name)?;
    let mut task = Task::new(workspace_path, title, goal, mode);
    task.provider_name = provider_name;
    TaskRepository::new(&state.db)
        .create(&task)
        .map_err(err_str)?;
    let branch = SessionBranchRepository::new(&state.db)
        .ensure_active(&task.id)
        .map_err(err_str)?;
    TaskEventStore::new(&state.db)
        .append_for_branch(&task.id, &branch.id, TaskEventType::TaskCreated)
        .map_err(err_str)?;
    Ok(task)
}

pub async fn task_list(
    state: &CommandState,
    workspace_path: Option<&str>,
    include_archived: bool,
) -> Result<Vec<Task>, String> {
    TaskRepository::new(&state.db)
        .list(workspace_path, None, include_archived)
        .map_err(err_str)
}

/// 归档一个已经停止的会话。运行中的会话必须先中止，避免后台运行成为孤儿。
pub async fn task_archive(state: &CommandState, task_id: &str) -> Result<Task, String> {
    let repo = TaskRepository::new(&state.db);
    let task = repo
        .get(task_id)
        .map_err(err_str)?
        .ok_or_else(|| format!("task not found: {task_id}"))?;
    if task.state == TaskState::Archived {
        return Ok(task);
    }
    if matches!(task.state, TaskState::Exploring | TaskState::InProgress) {
        return Err("会话仍在运行，请先停止后归档".to_string());
    }

    let mut bridge = state.agent.lock().await;
    if bridge
        .active
        .as_ref()
        .is_some_and(|active| active.task_id == task_id)
    {
        return Err("会话仍在运行，请先停止后归档".to_string());
    }
    repo.update_state(task_id, TaskState::Archived)
        .map_err(err_str)?;
    // 归档会话不再保留可继续运行的内存映射；持久化历史仍保留给审计与恢复。
    bridge.sessions.remove(task_id);
    drop(bridge);

    repo.get(task_id)
        .map_err(err_str)?
        .ok_or_else(|| format!("task not found after archive: {task_id}"))
}

/// 将既有会话附加到已打开的工作区，或移除其工作区作用域。
///
/// 工作区一旦附加即可公开受 PathGuard 限制的本地工具；工具调用的审批策略来自
/// 工作区持久化的项目权限模式。
pub async fn task_set_workspace(
    state: &CommandState,
    task_id: &str,
    workspace_path: Option<&str>,
) -> Result<Task, String> {
    let path = workspace_path
        .filter(|path| !path.trim().is_empty())
        .map(|path| workspace_root(state, path).map(|root| root.display().to_string()))
        .transpose()?;

    let workspace_access_mode = match &path {
        Some(path) => WorkspaceService::new(&state.db)
            .get(path)
            .map_err(err_str)?
            .map(|workspace| workspace.access_mode)
            .unwrap_or(ProjectAccessMode::RequestApproval),
        None => ProjectAccessMode::RequestApproval,
    };

    let repo = TaskRepository::new(&state.db);
    let task = repo
        .get(task_id)
        .map_err(err_str)?
        .ok_or_else(|| format!("task not found: {task_id}"))?;
    if task.state == TaskState::Archived {
        return Err("会话已归档，不能再修改工作区".to_string());
    }
    repo.set_workspace_path(task_id, path.as_deref())
        .map_err(err_str)?;
    let task = repo
        .get(task_id)
        .map_err(err_str)?
        .ok_or_else(|| format!("task not found after workspace update: {task_id}"))?;

    // 已经建立的 runtime session 保留对话历史，但其下一轮运行立即采用新作用域。
    let mut bridge = state.agent.lock().await;
    if let Some(session) = bridge.sessions.get(task_id).cloned() {
        bridge
            .kind
            .update_workspace_scope(&session.runtime_session_id, path, workspace_access_mode)
            .await
            .map_err(err_str)?;
    }
    Ok(task)
}

/// 为一个空闲会话切换绑定的模型服务。
///
/// 运行中的 provider、模型和上下文必须是同一份快照，故禁止中途切换；成功后只清除
/// 本会话的内存 runtime 映射，下一次运行将以新服务和完整持久化历史重新建立。
pub async fn task_set_provider(
    state: &CommandState,
    task_id: &str,
    provider_name: &str,
) -> Result<Task, String> {
    let provider_name = validate_selected_provider(state, Some(provider_name))?
        .ok_or_else(|| "请选择模型服务".to_string())?;
    let repo = TaskRepository::new(&state.db);
    let task = repo
        .get(task_id)
        .map_err(err_str)?
        .ok_or_else(|| format!("task not found: {task_id}"))?;
    if task.state == TaskState::Archived {
        return Err("会话已归档，不能再切换模型服务".to_string());
    }

    let mut bridge = state.agent.lock().await;
    if bridge.active.is_some() {
        return Err("当前运行尚未结束，不能在执行期间切换模型服务配置".to_string());
    }
    repo.set_provider_name(task_id, Some(&provider_name))
        .map_err(err_str)?;
    // 模型名隶属于具体服务，换服务后旧的覆盖值必然无效，一并清除。
    repo.set_model(task_id, None).map_err(err_str)?;
    bridge.sessions.remove(task_id);
    drop(bridge);

    repo.get(task_id)
        .map_err(err_str)?
        .ok_or_else(|| format!("task not found after provider update: {task_id}"))
}

/// 为一个空闲会话切换具体模型（仍属于当前绑定的模型服务）。
///
/// 与 `task_set_provider` 同样的约束：运行中不允许切换，且必须清掉内存里的
/// runtime session —— 已建立的 SessionState 会把旧模型一直用到会话结束。
/// 传入空字符串表示清除覆盖，回退到该服务在设置里配置的默认模型。
pub async fn task_set_model(
    state: &CommandState,
    task_id: &str,
    model: Option<&str>,
) -> Result<Task, String> {
    let model = model.map(str::trim).filter(|value| !value.is_empty());
    if let Some(value) = model {
        if value.len() > 200 {
            return Err("模型名过长".to_string());
        }
    }

    let repo = TaskRepository::new(&state.db);
    let task = repo
        .get(task_id)
        .map_err(err_str)?
        .ok_or_else(|| format!("task not found: {task_id}"))?;
    if task.state == TaskState::Archived {
        return Err("会话已归档，不能再切换模型".to_string());
    }

    let mut bridge = state.agent.lock().await;
    if bridge.active.is_some() {
        return Err("当前运行尚未结束，不能在执行期间切换模型".to_string());
    }
    repo.set_model(task_id, model).map_err(err_str)?;
    bridge.sessions.remove(task_id);
    drop(bridge);

    repo.get(task_id)
        .map_err(err_str)?
        .ok_or_else(|| format!("task not found after model update: {task_id}"))
}

pub async fn task_detail(state: &CommandState, task_id: &str) -> Result<TaskDetail, String> {
    let task = TaskRepository::new(&state.db)
        .get(task_id)
        .map_err(err_str)?
        .ok_or_else(|| format!("task not found: {task_id}"))?;
    let active_branch = SessionBranchRepository::new(&state.db)
        .ensure_active(task_id)
        .map_err(err_str)?;
    let branches = SessionBranchRepository::new(&state.db)
        .list_by_task(task_id)
        .map_err(err_str)?;

    let runs = AgentRunRepository::new(&state.db)
        .list_by_task_branch(task_id, &active_branch.id)
        .map_err(err_str)?;

    let events = TaskEventStore::new(&state.db)
        .list_by_task_branch(task_id, &active_branch.id, Some(400), None)
        .map_err(err_str)?;

    let changes = ChangeService::new(&state.db, state.blobs_dir.clone())
        .list_changes(task_id)
        .await
        .map_err(err_str)?;

    let permissions = state.permission_engine.pending_for_task(task_id).await;

    let verifications = VerificationService::new(&state.db, state.blobs_dir.clone())
        .list_for_task(task_id)
        .await
        .map_err(err_str)?;
    let queued_messages = QueuedMessageRepository::new(&state.db)
        .list_pending(task_id, &active_branch.id)
        .map_err(err_str)?;

    Ok(TaskDetail {
        task,
        active_branch,
        branches,
        runs,
        events,
        changes,
        permissions,
        verifications,
        queued_messages,
    })
}

// ============================================================================
// Agent 命令（真实 provider runtime；无配置直接报错，不做 mock 降级）
// ============================================================================

/// Agent runtime 种类：真实 provider 或 Mock（仅测试/开发）。
pub enum AgentRuntimeKind {
    /// 真实 provider runtime（LlmAgentRuntime）
    Real(r_code_agent_worker::LlmAgentRuntime),
    /// Mock runtime（脚本化回放；测试用）
    Mock(MockAgentRuntime),
}

#[async_trait::async_trait]
impl AgentRuntime for AgentRuntimeKind {
    async fn create_session(
        &mut self,
        input: CreateSessionInput,
    ) -> Result<hermes_core::Session, ProductError> {
        match self {
            Self::Real(r) => r.create_session(input).await,
            Self::Mock(r) => r.create_session(input).await,
        }
    }
    async fn start_run(&mut self, session_id: &str, goal: &str) -> Result<String, ProductError> {
        match self {
            Self::Real(r) => r.start_run(session_id, goal).await,
            Self::Mock(r) => r.start_run(session_id, goal).await,
        }
    }
    async fn steer(
        &mut self,
        session_id: &str,
        message: &str,
    ) -> Result<SteerResult, ProductError> {
        match self {
            Self::Real(r) => r.steer(session_id, message).await,
            Self::Mock(r) => r.steer(session_id, message).await,
        }
    }
    async fn abort(&mut self, session_id: &str) -> Result<(), ProductError> {
        match self {
            Self::Real(r) => r.abort(session_id).await,
            Self::Mock(r) => r.abort(session_id).await,
        }
    }
    async fn abort_subagent(
        &mut self,
        session_id: &str,
        subagent_id: &str,
    ) -> Result<bool, ProductError> {
        match self {
            Self::Real(r) => r.abort_subagent(session_id, subagent_id).await,
            Self::Mock(r) => r.abort_subagent(session_id, subagent_id).await,
        }
    }
    async fn replace_history(
        &mut self,
        session_id: &str,
        messages: Vec<Message>,
    ) -> Result<(), ProductError> {
        match self {
            Self::Real(r) => r.replace_history(session_id, messages).await,
            Self::Mock(r) => r.replace_history(session_id, messages).await,
        }
    }
    async fn history_snapshot(
        &mut self,
        session_id: &str,
    ) -> Result<Option<Vec<Message>>, ProductError> {
        match self {
            Self::Real(r) => r.history_snapshot(session_id).await,
            Self::Mock(r) => r.history_snapshot(session_id).await,
        }
    }
    async fn update_workspace_scope(
        &mut self,
        session_id: &str,
        workspace_path: Option<String>,
        access_mode: ProjectAccessMode,
    ) -> Result<(), ProductError> {
        match self {
            Self::Real(r) => {
                r.update_workspace_scope(session_id, workspace_path, access_mode)
                    .await
            }
            Self::Mock(r) => {
                r.update_workspace_scope(session_id, workspace_path, access_mode)
                    .await
            }
        }
    }
    async fn poll_events(&mut self) -> Result<Vec<AgentEvent>, ProductError> {
        match self {
            Self::Real(r) => r.poll_events().await,
            Self::Mock(r) => r.poll_events().await,
        }
    }
}

impl AgentBridge {
    /// 是否处于真实 provider 模式（main.rs 启动时开启；测试保持 Mock）。
    pub fn enable_real_mode(&mut self) {
        self.real_mode = true;
    }

    fn is_running(&self) -> bool {
        match &self.kind {
            AgentRuntimeKind::Real(r) => r.is_running(),
            AgentRuntimeKind::Mock(r) => r.is_running(),
        }
    }

    fn aborted(&self) -> bool {
        match &self.kind {
            AgentRuntimeKind::Real(r) => r.aborted(),
            AgentRuntimeKind::Mock(r) => r.aborted(),
        }
    }
}

/// 确保 bridge 持有与指定会话服务配置一致的真实 runtime。
///
/// 配置缺失/无效时直接报错（指引去 Settings），不做任何降级。
/// 协议分派见 [`build_provider_config`]：依据 `provider_catalog::resolve_protocol`
/// 解析出的线路协议，而不是服务名。
async fn ensure_real_runtime(
    config_dir: &Path,
    tool_gateway: &Arc<r_code_gateway::ToolGateway>,
    bridge: &mut AgentBridge,
    requested_provider: Option<&str>,
) -> Result<(), String> {
    let settings = SettingsService::new(config_dir.to_path_buf());
    // 设置页允许保留尚未完成的非默认 Provider 草稿；启动会话时只校验当前
    // 选中的 Provider，不能让无关草稿阻断已配置好的服务。
    let config = settings.load_global_unvalidated().map_err(err_str)?;
    // 旧会话没有 provider_name 时才使用全局默认；一旦任务绑定了服务，后续全局
    // 默认变更不应影响它。
    let provider_name = requested_provider
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| config.default_provider.clone());
    let pcfg = config
        .providers
        .get(&provider_name)
        .ok_or_else(|| format!("未找到默认模型服务“{provider_name}”，请前往设置完成配置"))?;
    if let Some(problem) = provider_readiness_error(&provider_name, pcfg) {
        return Err(format!(
            "模型服务“{provider_name}”尚未就绪：{problem}。请在设置中保存后重试"
        ));
    }

    // max_tokens / temperature 也是 runtime 的一部分。遗漏这两个字段会导致用户在
    // 设置页保存后，当前进程继续沿用旧的输出上限或随机性。
    // 协议也必须进指纹：只改「线路协议」而不动地址/模型时，其余字段完全没变，
    // 漏掉它会让当前进程继续用旧协议的 provider，直到重启才生效。
    // 用有效协议而非裸 `pcfg.protocol`——旧配置补上一个与推断结果相同的值时，
    // 实际行为没变，不该白白重建 runtime 并清空会话。
    let fingerprint = format!(
        "{provider_name}|{}|{}|{}|{:?}|{:?}|{}",
        pcfg.base_url,
        pcfg.model,
        pcfg.api_key,
        pcfg.max_tokens,
        pcfg.temperature,
        resolve_effective_protocol(&provider_name, pcfg).as_str()
    );
    if matches!(&bridge.kind, AgentRuntimeKind::Real(_))
        && bridge.fingerprint.as_deref() == Some(fingerprint.as_str())
    {
        return Ok(());
    }
    if bridge.active.is_some() {
        return Err("当前运行尚未结束，不能在执行期间切换模型服务配置".to_string());
    }

    let provider_config = build_provider_config(&provider_name, pcfg);
    let provider = hermes_llm::create_provider(provider_config).map_err(err_str)?;
    let max_tokens = effective_max_tokens(&provider_name, pcfg);
    let runtime = r_code_agent_worker::LlmAgentRuntime::new(
        provider,
        pcfg.model.clone(),
        tool_gateway.clone(),
        max_tokens,
        pcfg.temperature,
    );

    bridge.kind = AgentRuntimeKind::Real(runtime);
    bridge.sessions.clear(); // provider 配置变了，旧会话随旧 runtime 一起失效
    bridge.fingerprint = Some(fingerprint);
    Ok(())
}

/// Mock 演示场景：plan + 文本回复 + 一次工具调用（仅测试/开发路径使用）。
fn push_demo_scenario(runtime: &mut MockAgentRuntime, message: &str) {
    let call_id = uuid::Uuid::new_v4().to_string();
    runtime.push_scenario(vec![
        AgentEvent::Plan {
            steps: vec![
                PlanStep {
                    description: "理解需求并定位相关代码".into(),
                    completed: true,
                },
                PlanStep {
                    description: "实施改动".into(),
                    completed: false,
                },
                PlanStep {
                    description: "验证并交付审查".into(),
                    completed: false,
                },
            ],
        },
        AgentEvent::Message {
            text: format!(
                "收到。我先看相关代码再给方案。（mock runtime 脚本化回复）\n\n你的消息：{message}"
            ),
            delta: false,
        },
        AgentEvent::ToolCall {
            name: "read_file".into(),
            input: serde_json::json!({"path": "src/main.rs"}),
            call_id: call_id.clone(),
        },
        AgentEvent::ToolResult {
            call_id,
            output: serde_json::json!({"content": "// file preview (mock)"}),
            is_error: false,
        },
        AgentEvent::Message {
            text: "读完了。改动方案如下……（mock）".into(),
            delta: false,
        },
    ]);
}

fn session_file_path(sessions_dir: &Path, storage_id: &str) -> PathBuf {
    sessions_dir.join(format!("{storage_id}.jsonl"))
}

/// 子代理使用独立 JSONL，绝不把探索过程混入主 Agent 的下一轮上下文。
fn subagent_storage_id(parent_storage_id: &str, subagent_id: &str) -> String {
    format!("{parent_storage_id}--subagent-{subagent_id}")
}

/// 与内置网关工具风险分级保持一致；内部编排工具不接触工作区，按 R0 记录。
fn observed_tool_risk(tool_name: &str) -> RiskLevel {
    match tool_name {
        "read_file" => RiskLevel::R1,
        "apply_patch" | "create_file" | "delete_file" => RiskLevel::R2,
        _ => RiskLevel::R0,
    }
}

async fn ensure_session_log(
    session_store: &SessionStore,
    sessions_dir: &Path,
    storage_id: &str,
) -> Result<(), String> {
    if session_file_path(sessions_dir, storage_id).exists() {
        return Ok(());
    }
    let meta = SessionMeta {
        id: storage_id.to_string(),
        created_at: chrono::Utc::now(),
        model: "unknown".to_string(),
        provider: "unknown".to_string(),
        title: None,
    };
    session_store
        .append(storage_id, SessionEvent::Meta(meta))
        .await
        .map_err(err_str)
}

/// 解开嵌套运行作用域；旧主 Agent 事件保持没有 scope，因而对旧 runtime 完全兼容。
fn split_scoped_event(mut event: &AgentEvent) -> (Option<&AgentEventScope>, &AgentEvent) {
    let mut scope = None;
    while let AgentEvent::Scoped {
        scope: next_scope,
        event: inner,
    } = event
    {
        scope = Some(next_scope);
        event = inner;
    }
    (scope, event)
}

fn ensure_subagent_run(
    db: &Database,
    task_id: &str,
    branch_id: &str,
    scope: &AgentEventScope,
) -> Result<bool, ProductError> {
    let repo = AgentRunRepository::new(db);
    if repo.get(&scope.run_id)?.is_some() {
        return Ok(false);
    }
    let parent_run_id = scope
        .parent_run_id
        .clone()
        .ok_or_else(|| ProductError::Other(format!("子代理 {} 缺少父运行 ID", scope.run_id)))?;
    // 正常事件顺序会先落父代理的 delegate_task ToolCall；这里补一个最小审计锚点，
    // 以容忍 IPC 重放或多发送端调度导致的生命周期先到，避免外键阻断整棵运行树。
    if let Some(delegated_by_tool_call_id) = &scope.delegated_by_tool_call_id {
        let mut delegation_call = ToolCall::new(
            &parent_run_id,
            task_id,
            "delegate_task",
            serde_json::json!({ "recovered": true }).to_string(),
            RiskLevel::R0,
        );
        delegation_call.id = delegated_by_tool_call_id.clone();
        ToolCallRepository::new(db).create_if_absent(&delegation_call)?;
    }
    let mut run = AgentRun::new_subagent_for_branch(
        task_id,
        branch_id,
        parent_run_id,
        "subagent",
        scope.agent_label.clone(),
        scope.delegated_by_tool_call_id.clone(),
    );
    run.id = scope.run_id.clone();
    repo.create(&run)?;
    Ok(true)
}

async fn persist_runtime_event(
    db: &Database,
    session_store: &SessionStore,
    sessions_dir: &Path,
    task_id: &str,
    branch_id: &str,
    parent_run_id: &str,
    parent_storage_id: &str,
    event: &AgentEvent,
    pending_text: &mut HashMap<String, PendingAssistantText>,
) {
    let (scope, event) = split_scoped_event(event);
    let scope_key = scope
        .map(|value| value.run_id.clone())
        .unwrap_or_else(|| "main".to_string());
    let event_storage_id = scope
        .map(|value| subagent_storage_id(parent_storage_id, &value.run_id))
        .unwrap_or_else(|| parent_storage_id.to_string());
    let _ = ensure_session_log(session_store, sessions_dir, &event_storage_id).await;

    match event {
        AgentEvent::Message { text, delta } => {
            if *delta {
                let entry = pending_text.entry(scope_key).or_default();
                if entry.storage_id.is_empty() {
                    entry.storage_id = event_storage_id;
                }
                entry.text.push_str(text);
                entry.saw_delta = true;
            } else {
                let pending = pending_text.remove(&scope_key).unwrap_or_default();
                let storage_id = if pending.storage_id.is_empty() {
                    event_storage_id
                } else {
                    pending.storage_id
                };
                let full = pending.text + text;
                let _ = session_store
                    .append(
                        &storage_id,
                        SessionEvent::Message(Message::assistant_text(&full)),
                    )
                    .await;
            }
        }
        AgentEvent::ToolCall {
            name,
            input,
            call_id,
        } => {
            let _ = session_store
                .append(
                    &event_storage_id,
                    SessionEvent::ToolCall {
                        name: name.clone(),
                        input: input.clone(),
                    },
                )
                .await;
            let run_id = scope
                .map(|value| value.run_id.as_str())
                .unwrap_or(parent_run_id);
            let mut audit = ToolCall::new(
                run_id,
                task_id,
                name,
                input.to_string(),
                observed_tool_risk(name),
            );
            // Provider 生成的 call_id 同时用于 ToolResult、权限记录和子代理委派的外键。
            audit.id = call_id.clone();
            audit.caller = scope.map(|value| format!("subagent:{}", value.agent_id));
            let _ = ToolCallRepository::new(db).create_if_absent(&audit);
            if let Some(scope) = scope {
                let _ = session_store
                    .append(
                        &event_storage_id,
                        SessionEvent::System {
                            event: "subagent_tool_audit".into(),
                            data: serde_json::json!({
                                "caller": format!("subagent:{}", scope.agent_id),
                                "run_id": scope.run_id.as_str(),
                                "parent_run_id": scope.parent_run_id.as_deref(),
                                "tool_name": name,
                            }),
                        },
                    )
                    .await;
            }
            // 子代理工具正文只存在其独立 JSONL 中；若把它也作为主分支的工具时间
            // 锚点，会让主时间线随后工具消息的时间戳错位。
            if scope.is_none() {
                let _ = TaskEventStore::new(db).append_for_branch(
                    task_id,
                    branch_id,
                    TaskEventType::ToolCall,
                );
            }
        }
        AgentEvent::ToolResult {
            call_id,
            output,
            is_error,
        } => {
            let _ = session_store
                .append(
                    &event_storage_id,
                    SessionEvent::ToolResult {
                        call_id: call_id.clone(),
                        output: output.clone(),
                        is_error: *is_error,
                    },
                )
                .await;
            let _ = ToolCallRepository::new(db).finish(call_id, output, *is_error);
            if scope.is_none() {
                let _ = TaskEventStore::new(db).append_for_branch(
                    task_id,
                    branch_id,
                    TaskEventType::ToolResult,
                );
            }
        }
        AgentEvent::Plan { steps } => {
            let _ = session_store
                .append(
                    &event_storage_id,
                    SessionEvent::System {
                        event: "plan".into(),
                        data: serde_json::json!({ "steps": steps }),
                    },
                )
                .await;
        }
        AgentEvent::State { state } => {
            // 子运行自己的终态不能直接推动整个 Task 状态机。
            if scope.is_none() {
                let _ = TaskRepository::new(db).update_state(task_id, *state);
            }
        }
        AgentEvent::SubagentLifecycle { state, detail } => {
            let Some(scope) = scope else {
                return;
            };
            let created = match ensure_subagent_run(db, task_id, branch_id, scope) {
                Ok(created) => created,
                Err(error) => {
                    tracing::warn!(
                        task_id,
                        child_run_id = %scope.run_id,
                        "failed to persist subagent run: {error}"
                    );
                    false
                }
            };
            if created {
                let _ = TaskEventStore::new(db).append_for_branch(
                    task_id,
                    branch_id,
                    TaskEventType::SubagentStarted,
                );
            }
            if matches!(
                state,
                SubagentState::Completed | SubagentState::Failed | SubagentState::Cancelled
            ) {
                let repo = AgentRunRepository::new(db);
                let already_finished = repo
                    .get(&scope.run_id)
                    .ok()
                    .flatten()
                    .is_some_and(|run| run.ended_at.is_some());
                if !already_finished {
                    let review_state = match state {
                        SubagentState::Completed => ReviewState::Answered,
                        SubagentState::Failed => ReviewState::Failed,
                        SubagentState::Cancelled => ReviewState::Aborted,
                        _ => unreachable!("已过滤为子代理终态"),
                    };
                    let _ = repo.set_summary(&scope.run_id, detail.as_deref());
                    let _ = repo.update_review_state(&scope.run_id, review_state);
                    let _ = TaskEventStore::new(db).append_for_branch(
                        task_id,
                        branch_id,
                        TaskEventType::SubagentFinished,
                    );
                }
            }
            // 子日志保存自身生命周期，主日志保存运行树索引；两者都可独立回放。
            let lifecycle_data = serde_json::json!({
                "scope": scope,
                "state": state,
                "detail": detail,
            });
            let _ = session_store
                .append(
                    &event_storage_id,
                    SessionEvent::System {
                        event: "subagent_lifecycle".into(),
                        data: lifecycle_data.clone(),
                    },
                )
                .await;
            let _ = session_store
                .append(
                    parent_storage_id,
                    SessionEvent::System {
                        event: "subagent_lifecycle".into(),
                        data: lifecycle_data,
                    },
                )
                .await;
        }
        AgentEvent::Activity { .. } => {}
        AgentEvent::Scoped { .. } => unreachable!("split_scoped_event 已解包所有作用域"),
    }
}

/// 获取或建立当前分支的 runtime session，并从该分支 JSONL 重建可见的用户/助手消息历史。
async fn ensure_runtime_session(
    bridge: &mut AgentBridge,
    db: &Database,
    session_store: &SessionStore,
    sessions_dir: &Path,
    task: &Task,
    branch: &SessionBranch,
) -> Result<String, String> {
    if let Some(existing) = bridge.sessions.get(&task.id) {
        if existing.branch_id == branch.id && existing.storage_id == branch.storage_id {
            return Ok(existing.runtime_session_id.clone());
        }
    }

    ensure_session_log(session_store, sessions_dir, &branch.storage_id).await?;
    let (workspace_path, workspace_access_mode) = task_workspace_binding_from_db(db, task)?;
    let session = bridge
        .kind
        .create_session(CreateSessionInput {
            workspace_path,
            workspace_access_mode,
            task_id: task.id.clone(),
            goal: task.goal.clone(),
            mode: task.mode,
            model: task.model.clone(),
            context: vec![],
        })
        .await
        .map_err(err_str)?;

    let history = session_store
        .load(&branch.storage_id)
        .await
        .map_err(err_str)?;
    bridge
        .kind
        .replace_history(&session.meta.id, history.messages)
        .await
        .map_err(err_str)?;
    let runtime_session_id = session.meta.id;
    bridge.sessions.insert(
        task.id.clone(),
        BridgeSession {
            runtime_session_id: runtime_session_id.clone(),
            branch_id: branch.id.clone(),
            storage_id: branch.storage_id.clone(),
        },
    );
    Ok(runtime_session_id)
}

/// 在已持有 AgentBridge 锁的前提下启动一个 run。调用方必须在返回后尽快释放锁，
/// 再启动 drain 循环，防止不同任务的流事件互相串台。
const USER_MESSAGE_MODE_EVENT: &str = "r_code_user_message_mode";

fn send_mode_name(mode: AgentSendMode) -> &'static str {
    match mode {
        AgentSendMode::Auto => "auto",
        AgentSendMode::Steer => "steer",
        AgentSendMode::Queue => "queue",
        AgentSendMode::SendNow => "send_now",
    }
}

fn queued_dispatch_mode(message: &QueuedMessage) -> AgentSendMode {
    if message.priority >= 1_000_000 {
        AgentSendMode::SendNow
    } else {
        AgentSendMode::Queue
    }
}

async fn append_user_message_with_mode(
    session_store: &SessionStore,
    storage_id: &str,
    message: &str,
    mode: AgentSendMode,
) -> Result<(), String> {
    session_store
        .append(
            storage_id,
            SessionEvent::System {
                event: USER_MESSAGE_MODE_EVENT.into(),
                data: serde_json::json!({ "mode": send_mode_name(mode) }),
            },
        )
        .await
        .map_err(err_str)?;
    session_store
        .append(
            storage_id,
            SessionEvent::Message(Message::user_text(message)),
        )
        .await
        .map_err(err_str)
}

async fn start_run_locked(
    bridge: &mut AgentBridge,
    db: &Database,
    session_store: &SessionStore,
    sessions_dir: &Path,
    task: &Task,
    branch: &SessionBranch,
    message: &str,
    message_mode: AgentSendMode,
) -> Result<ActiveRun, String> {
    if bridge.active.is_some() {
        return Err("已有运行正在收尾，无法并发启动新的运行".to_string());
    }
    let runtime_session_id =
        ensure_runtime_session(bridge, db, session_store, sessions_dir, task, branch).await?;
    append_user_message_with_mode(session_store, &branch.storage_id, message, message_mode).await?;

    if let AgentRuntimeKind::Mock(runtime) = &mut bridge.kind {
        push_demo_scenario(runtime, message);
    }
    let runtime_run_id = bridge
        .kind
        .start_run(&runtime_session_id, message)
        .await
        .map_err(err_str)?;

    // 会话有显式模型时以它为准；否则回退到 runtime fingerprint 里的 provider 默认模型。
    // 若继续只读 fingerprint，切换过模型的会话会把运行记录写成错误的模型名。
    let run_model = task
        .model
        .clone()
        .filter(|model| !model.trim().is_empty())
        .or_else(|| {
            bridge
                .fingerprint
                .as_deref()
                .and_then(|fingerprint| fingerprint.split('|').nth(2))
                .map(str::to_string)
        })
        .unwrap_or_else(|| "mock".to_string());
    let mut run = AgentRun::new_for_branch(&task.id, &branch.id, run_model);
    // Runtime 生成的 run id 同时是工具审计和子代理父子关系的锚点；不能再另起一个
    // 仅供数据库使用的 UUID，否则实时事件与持久化运行树会失去关联。
    run.id = runtime_run_id;
    AgentRunRepository::new(db).create(&run).map_err(err_str)?;
    TaskRepository::new(db)
        .update_state(&task.id, TaskState::InProgress)
        .map_err(err_str)?;
    TaskEventStore::new(db)
        .append_for_branch(&task.id, &branch.id, TaskEventType::RunStarted)
        .map_err(err_str)?;

    let active = ActiveRun {
        task_id: task.id.clone(),
        branch_id: branch.id.clone(),
        runtime_session_id,
        run_id: run.id,
    };
    bridge.active = Some(active.clone());
    Ok(active)
}

fn enqueue_message(
    db: &Database,
    task_id: &str,
    branch_id: &str,
    message: &str,
    priority: i64,
) -> Result<QueuedMessage, String> {
    let queued = QueuedMessage::new(task_id, branch_id, message, priority);
    QueuedMessageRepository::new(db)
        .enqueue(&queued)
        .map_err(err_str)?;
    TaskEventStore::new(db)
        .append_for_branch(task_id, branch_id, TaskEventType::UserMessageQueued)
        .map_err(err_str)?;
    Ok(queued)
}

fn mark_run_aborted(db: &Database, active: &ActiveRun) -> Result<(), String> {
    let runs = AgentRunRepository::new(db);
    if let Some(run) = runs.get(&active.run_id).map_err(err_str)? {
        if run.ended_at.is_some() {
            return Ok(());
        }
        runs.update_review_state(&active.run_id, ReviewState::Aborted)
            .map_err(err_str)?;
        TaskRepository::new(db)
            .update_state(&active.task_id, TaskState::Interrupted)
            .map_err(err_str)?;
        let events = TaskEventStore::new(db);
        events
            .append_for_branch(
                &active.task_id,
                &active.branch_id,
                TaskEventType::RunAborted,
            )
            .map_err(err_str)?;
        events
            .append_for_branch(&active.task_id, &active.branch_id, TaskEventType::RunEnded)
            .map_err(err_str)?;
    }
    Ok(())
}

/// 兼容旧 IPC：未提供动作时由服务端选择安全的自动行为。
pub async fn agent_send(state: &CommandState, task_id: &str, message: &str) -> Result<(), String> {
    agent_send_with_mode(state, task_id, message, AgentSendMode::Auto).await
}

/// 发送一条用户消息，并显式处理引导、排队和立即发送三种运行控制语义。
pub async fn agent_send_with_mode(
    state: &CommandState,
    task_id: &str,
    message: &str,
    mode: AgentSendMode,
) -> Result<(), String> {
    let message = message.trim();
    if message.is_empty() {
        return Err("消息不能为空".to_string());
    }
    let task = TaskRepository::new(&state.db)
        .get(task_id)
        .map_err(err_str)?
        .ok_or_else(|| format!("task not found: {task_id}"))?;
    if task.state == TaskState::Archived {
        return Err("会话已归档，不能继续发送消息".to_string());
    }
    let branch = SessionBranchRepository::new(&state.db)
        .ensure_active(task_id)
        .map_err(err_str)?;

    let mut bridge = state.agent.lock().await;
    // 运行中的 steer / 排队不重建 provider runtime：配置变更必须等当前运行收尾，
    // 否则会丢失流事件或把消息送进错误会话。真正新开 run 时才读取最新配置。
    let had_active_run = bridge.active.is_some();
    if bridge.real_mode
        && !had_active_run
        && !matches!(mode, AgentSendMode::Queue | AgentSendMode::Steer)
    {
        ensure_real_runtime(
            &state.config_dir,
            &state.tool_gateway,
            &mut bridge,
            task.provider_name.as_deref(),
        )
        .await?;
    }
    let active = bridge.active.clone();

    match mode {
        AgentSendMode::Steer => {
            let active = active.ok_or_else(|| "当前没有可引导的运行".to_string())?;
            if active.task_id != task_id || active.branch_id != branch.id {
                return Err("只能引导当前会话的正在运行任务".to_string());
            }
            let result = bridge
                .kind
                .steer(&active.runtime_session_id, message)
                .await
                .map_err(err_str)?;
            match result {
                SteerResult::Accepted => {
                    append_user_message_with_mode(
                        &state.session_store,
                        &branch.storage_id,
                        message,
                        AgentSendMode::Steer,
                    )
                    .await?;
                    TaskEventStore::new(&state.db)
                        .append_for_branch(task_id, &branch.id, TaskEventType::UserSteered)
                        .map_err(err_str)?;
                }
                SteerResult::RunFinished => {
                    // 运行恰好在点击引导时结束：转为持久化队列，不能写进已结束 run 的历史。
                    enqueue_message(&state.db, task_id, &branch.id, message, 0)?;
                }
            }
            Ok(())
        }
        AgentSendMode::Queue => {
            enqueue_message(&state.db, task_id, &branch.id, message, 0)?;
            // 在空闲 runtime 上“排队”不应留下永远不会被消费的消息；立即交给同一
            // 分发路径，确保其按会话绑定的 provider 进行就绪检查和 runtime 重建。
            if active.is_none() {
                drop(bridge);
                // 必须在 await 前释放 std::sync::MutexGuard，否则 Tauri 命令 future
                // 无法满足 Send 约束。
                let sink = { state.agent_event_sink.lock().unwrap().clone() };
                dispatch_next_queued(
                    state.agent.clone(),
                    state.db.clone(),
                    state.sessions_dir.clone(),
                    state.config_dir.clone(),
                    state.tool_gateway.clone(),
                    sink,
                )
                .await;
            }
            Ok(())
        }
        AgentSendMode::SendNow => {
            if let Some(active) = active {
                enqueue_message(&state.db, task_id, &branch.id, message, 1_000_000)?;
                bridge
                    .kind
                    .abort(&active.runtime_session_id)
                    .await
                    .map_err(err_str)?;
                drop(bridge);
                TaskRepository::new(&state.db)
                    .update_state(&active.task_id, TaskState::Interrupted)
                    .map_err(err_str)?;
                state.emit_agent_event(
                    &active.task_id,
                    &AgentEvent::State {
                        state: TaskState::Interrupted,
                    },
                );
                Ok(())
            } else {
                let active = start_run_locked(
                    &mut bridge,
                    &state.db,
                    &state.session_store,
                    &state.sessions_dir,
                    &task,
                    &branch,
                    message,
                    AgentSendMode::SendNow,
                )
                .await?;
                drop(bridge);
                state.emit_agent_event(
                    task_id,
                    &AgentEvent::State {
                        state: TaskState::InProgress,
                    },
                );
                spawn_drain_loop(state, active);
                Ok(())
            }
        }
        AgentSendMode::Auto => {
            if let Some(active) = active {
                if active.task_id == task_id && active.branch_id == branch.id {
                    let result = bridge
                        .kind
                        .steer(&active.runtime_session_id, message)
                        .await
                        .map_err(err_str)?;
                    match result {
                        SteerResult::Accepted => {
                            append_user_message_with_mode(
                                &state.session_store,
                                &branch.storage_id,
                                message,
                                AgentSendMode::Steer,
                            )
                            .await?;
                            TaskEventStore::new(&state.db)
                                .append_for_branch(task_id, &branch.id, TaskEventType::UserSteered)
                                .map_err(err_str)?;
                        }
                        SteerResult::RunFinished => {
                            enqueue_message(&state.db, task_id, &branch.id, message, 0)?;
                        }
                    }
                    Ok(())
                } else {
                    // Runtime 的事件队列是全局的；另一个任务运行时绝不能错误地 steer
                    // 到它的 session，改为持久化排队。
                    enqueue_message(&state.db, task_id, &branch.id, message, 0)?;
                    Ok(())
                }
            } else {
                let active = start_run_locked(
                    &mut bridge,
                    &state.db,
                    &state.session_store,
                    &state.sessions_dir,
                    &task,
                    &branch,
                    message,
                    AgentSendMode::Auto,
                )
                .await?;
                drop(bridge);
                state.emit_agent_event(
                    task_id,
                    &AgentEvent::State {
                        state: TaskState::InProgress,
                    },
                );
                spawn_drain_loop(state, active);
                Ok(())
            }
        }
    }
}

/// 启动单个运行的 drain 循环。物理 runtime 保持串行，但每个任务拥有独立的
/// 运行/分支元数据；循环结束后会自动分发下一条持久化队列消息。
fn spawn_drain_loop(state: &CommandState, active: ActiveRun) {
    spawn_drain_loop_with_resources(
        state.agent.clone(),
        state.db.clone(),
        state.sessions_dir.clone(),
        state.config_dir.clone(),
        state.tool_gateway.clone(),
        state.agent_event_sink.lock().unwrap().clone(),
        active,
    );
}

fn spawn_drain_loop_with_resources(
    agent: Arc<tokio::sync::Mutex<AgentBridge>>,
    db: Arc<Database>,
    sessions_dir: PathBuf,
    config_dir: PathBuf,
    tool_gateway: Arc<r_code_gateway::ToolGateway>,
    sink: Option<AgentEventSink>,
    active: ActiveRun,
) {
    tokio::spawn(async move {
        let task_id = active.task_id.clone();
        let branch_id = active.branch_id.clone();
        let storage_id = {
            let bridge = agent.lock().await;
            bridge
                .sessions
                .get(&task_id)
                .filter(|session| session.branch_id == branch_id)
                .map(|session| session.storage_id.clone())
                .unwrap_or_else(|| task_id.clone())
        };
        let session_store = SessionStore::new(sessions_dir.clone());
        let mut empty_streak = 0u32;
        let mut pending_text: HashMap<String, PendingAssistantText> = HashMap::new();

        loop {
            let (events, running, real) = {
                let mut bridge = agent.lock().await;
                let events = bridge.kind.poll_events().await.unwrap_or_else(|error| {
                    tracing::warn!(task_id, "poll_events failed: {error}");
                    Vec::new()
                });
                (
                    events,
                    bridge.is_running(),
                    matches!(bridge.kind, AgentRuntimeKind::Real(_)),
                )
            };

            for event in &events {
                persist_runtime_event(
                    &db,
                    &session_store,
                    &sessions_dir,
                    &task_id,
                    &branch_id,
                    &active.run_id,
                    &storage_id,
                    event,
                    &mut pending_text,
                )
                .await;
                if let Some(sink) = &sink {
                    sink(&task_id, event);
                }
            }

            if events.is_empty() {
                empty_streak += 1;
            } else {
                empty_streak = 0;
            }
            let drained = if real {
                !running && events.is_empty()
            } else {
                empty_streak >= 3
            };
            if drained {
                break;
            }
            // 事件已由 runtime 实时写入通道；以约 25 FPS 排空，在流式文本与工具状态之间
            // 保持可感知的即时性，同时避免 WebView 被每个 token 的 IPC 淹没。
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        }

        // 收尾：冲刷被中止前已经接收的文本 delta，审计日志不会丢失已显示内容。
        for pending in pending_text.into_values() {
            if pending.saw_delta && !pending.text.is_empty() {
                let storage_id = if pending.storage_id.is_empty() {
                    storage_id.clone()
                } else {
                    pending.storage_id
                };
                let _ = session_store
                    .append(
                        &storage_id,
                        SessionEvent::Message(Message::assistant_text(&pending.text)),
                    )
                    .await;
            }
        }

        let (was_aborted, history_snapshot) = {
            let mut bridge = agent.lock().await;
            let was_aborted = bridge.aborted();
            let history_snapshot = match bridge
                .kind
                .history_snapshot(&active.runtime_session_id)
                .await
            {
                Ok(snapshot) => snapshot,
                Err(error) => {
                    tracing::warn!(
                        task_id,
                        run_id = %active.run_id,
                        "failed to capture runtime history snapshot: {error}"
                    );
                    None
                }
            };
            if bridge
                .active
                .as_ref()
                .is_some_and(|current| current.run_id == active.run_id)
            {
                bridge.active = None;
            }
            (was_aborted, history_snapshot)
        };

        if let Some(messages) = history_snapshot {
            let _ = session_store
                .append(&storage_id, SessionEvent::HistorySnapshot { messages })
                .await;
        }

        if was_aborted {
            // runtime 已等待所有子代理确认取消；现在再结束父 Run 并发布终态，
            // 从而保证 Stop All 不会让仍在收尾的 Working 条目提前消失。
            let _ = mark_run_aborted(&db, &active);
            if let Some(sink) = &sink {
                sink(
                    &task_id,
                    &AgentEvent::State {
                        state: TaskState::Interrupted,
                    },
                );
            }
        } else {
            // 正常结束：有变更 → review_ready；零变更 → idle（“已回答”语义）。
            let has_changes = ChangeService::new(&db, PathBuf::new())
                .list_changes(&task_id)
                .await
                .map(|changes| !changes.is_empty())
                .unwrap_or(false);
            let final_state = if has_changes {
                TaskState::ReviewReady
            } else {
                TaskState::Idle
            };
            let _ = AgentRunRepository::new(&db)
                .update_review_state(&active.run_id, ReviewState::Pending);
            let _ = TaskRepository::new(&db).update_state(&task_id, final_state);
            let _ = TaskEventStore::new(&db).append_for_branch(
                &task_id,
                &branch_id,
                TaskEventType::RunEnded,
            );
            if let Some(sink) = &sink {
                sink(&task_id, &AgentEvent::State { state: final_state });
            }
        }

        dispatch_next_queued(agent, db, sessions_dir, config_dir, tool_gateway, sink).await;
    });
}

/// 物理 runtime 空闲后，从所有任务的持久化队列中取出最高优先级消息并启动。
async fn dispatch_next_queued(
    agent: Arc<tokio::sync::Mutex<AgentBridge>>,
    db: Arc<Database>,
    sessions_dir: PathBuf,
    config_dir: PathBuf,
    tool_gateway: Arc<r_code_gateway::ToolGateway>,
    sink: Option<AgentEventSink>,
) {
    loop {
        let mut bridge = agent.lock().await;
        if bridge.active.is_some() {
            return;
        }
        let Some(queued) = QueuedMessageRepository::new(&db)
            .take_next()
            .map_err(err_str)
            .unwrap_or_else(|error| {
                tracing::warn!("cannot take queued message: {error}");
                None
            })
        else {
            return;
        };

        let branch = match SessionBranchRepository::new(&db).ensure_active(&queued.task_id) {
            Ok(branch) => branch,
            Err(error) => {
                tracing::warn!(queue_id = %queued.id, "cannot load active branch: {error}");
                let _ = QueuedMessageRepository::new(&db)
                    .set_state(&queued.id, QueuedMessageState::Failed);
                return;
            }
        };
        if branch.id != queued.branch_id {
            // 旧分支上的待发送消息不能混入新分支；保留状态为 cancelled 供审计。
            let _ = QueuedMessageRepository::new(&db)
                .set_state(&queued.id, QueuedMessageState::Cancelled);
            continue;
        }
        let task = match TaskRepository::new(&db).get(&queued.task_id) {
            Ok(Some(task)) => task,
            Ok(None) | Err(_) => {
                let _ = QueuedMessageRepository::new(&db)
                    .set_state(&queued.id, QueuedMessageState::Failed);
                return;
            }
        };
        if bridge.real_mode {
            if let Err(error) = ensure_real_runtime(
                &config_dir,
                &tool_gateway,
                &mut bridge,
                task.provider_name.as_deref(),
            )
            .await
            {
                tracing::warn!(queue_id = %queued.id, "queued message provider is unavailable: {error}");
                let _ = QueuedMessageRepository::new(&db)
                    .set_state(&queued.id, QueuedMessageState::Failed);
                return;
            }
        }
        let started = start_run_locked(
            &mut bridge,
            &db,
            &SessionStore::new(sessions_dir.clone()),
            &sessions_dir,
            &task,
            &branch,
            &queued.message,
            queued_dispatch_mode(&queued),
        )
        .await;
        match started {
            Ok(active) => {
                let _ = QueuedMessageRepository::new(&db)
                    .set_state(&queued.id, QueuedMessageState::Sent);
                let _ = TaskEventStore::new(&db).append_for_branch(
                    &task.id,
                    &branch.id,
                    TaskEventType::QueueDispatched,
                );
                drop(bridge);
                if let Some(sink) = &sink {
                    sink(
                        &task.id,
                        &AgentEvent::State {
                            state: TaskState::InProgress,
                        },
                    );
                }
                spawn_drain_loop_with_resources(
                    agent,
                    db,
                    sessions_dir,
                    config_dir,
                    tool_gateway,
                    sink,
                    active,
                );
                return;
            }
            Err(error) => {
                tracing::warn!(queue_id = %queued.id, "queued message could not start: {error}");
                let _ = QueuedMessageRepository::new(&db)
                    .set_state(&queued.id, QueuedMessageState::Failed);
                return;
            }
        }
    }
}

pub async fn agent_abort(state: &CommandState, task_id: &str) -> Result<(), String> {
    let task = TaskRepository::new(&state.db)
        .get(task_id)
        .map_err(err_str)?
        .ok_or_else(|| format!("task not found: {task_id}"))?;
    if task.state == TaskState::Archived {
        return Err("会话已归档，不能中止运行".to_string());
    }
    {
        let mut bridge = state.agent.lock().await;
        let Some(active) = bridge.active.clone() else {
            TaskRepository::new(&state.db)
                .update_state(task_id, TaskState::Interrupted)
                .map_err(err_str)?;
            state.emit_agent_event(
                task_id,
                &AgentEvent::State {
                    state: TaskState::Interrupted,
                },
            );
            return Ok(());
        };
        if active.task_id != task_id {
            return Err("当前任务没有正在执行的运行，不能中止另一个任务".to_string());
        }
        bridge
            .kind
            .abort(&active.runtime_session_id)
            .await
            .map_err(err_str)?;
    }
    // 立即给用户明确反馈，但保留 Run 为活跃状态直到监督器确认所有子代理已退出。
    TaskRepository::new(&state.db)
        .update_state(task_id, TaskState::Interrupted)
        .map_err(err_str)?;
    state.emit_agent_event(
        task_id,
        &AgentEvent::State {
            state: TaskState::Interrupted,
        },
    );
    Ok(())
}

/// 仅中止当前主运行下的一个子代理，不影响主运行或同级子代理。
pub async fn agent_abort_subagent(
    state: &CommandState,
    task_id: &str,
    subagent_id: &str,
) -> Result<(), String> {
    let mut bridge = state.agent.lock().await;
    let active = bridge
        .active
        .clone()
        .ok_or_else(|| "当前没有正在执行的任务".to_string())?;
    if active.task_id != task_id {
        return Err("当前任务没有可中止的子代理".to_string());
    }
    let stopped = bridge
        .kind
        .abort_subagent(&active.runtime_session_id, subagent_id)
        .await
        .map_err(err_str)?;
    if !stopped {
        return Err("子代理不存在或已经结束".to_string());
    }
    Ok(())
}

/// 列出当前活跃分支的持久化待发送队列。
pub async fn agent_queue_list(
    state: &CommandState,
    task_id: &str,
) -> Result<Vec<QueuedMessage>, String> {
    let branch = SessionBranchRepository::new(&state.db)
        .ensure_active(task_id)
        .map_err(err_str)?;
    QueuedMessageRepository::new(&state.db)
        .list_pending(task_id, &branch.id)
        .map_err(err_str)
}

/// 取消一条当前分支可见的排队消息。
pub async fn agent_queue_remove(
    state: &CommandState,
    task_id: &str,
    queue_id: &str,
) -> Result<(), String> {
    let branch = SessionBranchRepository::new(&state.db)
        .ensure_active(task_id)
        .map_err(err_str)?;
    QueuedMessageRepository::new(&state.db)
        .cancel_for_task_branch(queue_id, task_id, &branch.id)
        .map_err(err_str)
}

fn parse_message_line_id(storage_id: &str, message_id: &str) -> Result<usize, String> {
    let prefix = format!("{storage_id}:");
    let line = message_id
        .strip_prefix(&prefix)
        .ok_or_else(|| "消息不属于当前会话分支".to_string())?;
    let line = line
        .parse::<usize>()
        .map_err(|_| "消息标识无效".to_string())?;
    if line == 0 {
        return Err("消息标识无效".to_string());
    }
    Ok(line)
}

/// 编辑一条已发送的用户消息，并从其前缀创建新的活跃会话分支后重新运行。
///
/// 旧 JSONL、旧运行与旧分支元数据均不改写；当前视图随后只读取新分支。
pub async fn agent_resend(
    state: &CommandState,
    task_id: &str,
    message_id: &str,
    message: &str,
) -> Result<(), String> {
    let message = message.trim();
    if message.is_empty() {
        return Err("消息不能为空".to_string());
    }
    {
        let bridge = state.agent.lock().await;
        if bridge.active.is_some() {
            return Err("运行中不能编辑历史消息；请先中止或等待当前运行结束".to_string());
        }
    }

    let task = TaskRepository::new(&state.db)
        .get(task_id)
        .map_err(err_str)?
        .ok_or_else(|| format!("task not found: {task_id}"))?;
    let source_branch = SessionBranchRepository::new(&state.db)
        .ensure_active(task_id)
        .map_err(err_str)?;
    let source_path = session_file_path(&state.sessions_dir, &source_branch.storage_id);
    let source = tokio::fs::read_to_string(&source_path)
        .await
        .map_err(err_str)?;
    let line_number = parse_message_line_id(&source_branch.storage_id, message_id)?;
    let lines: Vec<&str> = source.lines().collect();
    let selected = lines
        .get(line_number - 1)
        .ok_or_else(|| "要编辑的消息已不存在".to_string())?;
    let selected_event: SessionEvent =
        serde_json::from_str(selected).map_err(|_| "要编辑的消息格式无效".to_string())?;
    if !matches!(
        selected_event,
        SessionEvent::Message(ref item) if item.role == hermes_core::Role::User
    ) {
        return Err("只能编辑自己发送的消息".to_string());
    }

    let mut prefix: Vec<SessionEvent> = lines[..line_number - 1]
        .iter()
        .map(|line| {
            serde_json::from_str::<SessionEvent>(line)
                .map_err(|_| "会话历史存在无法恢复的记录".to_string())
        })
        .collect::<Result<_, _>>()?;
    let branch = SessionBranch::fork(task_id, &source_branch.id, message_id);
    match prefix.first_mut() {
        Some(SessionEvent::Meta(meta)) => {
            meta.id = branch.storage_id.clone();
            meta.created_at = chrono::Utc::now();
        }
        _ => return Err("会话缺少元数据，无法创建分支".to_string()),
    }
    state
        .session_store
        .write_session_atomic(&branch.storage_id, &prefix)
        .await
        .map_err(err_str)?;
    SessionBranchRepository::new(&state.db)
        .create_fork(&branch)
        .map_err(err_str)?;
    TaskEventStore::new(&state.db)
        .append_for_branch(task_id, &branch.id, TaskEventType::SessionBranched)
        .map_err(err_str)?;

    // 丢弃旧分支的内存 session；下次发送会从新分支快照重建完整前缀。
    let mut bridge = state.agent.lock().await;
    bridge.sessions.remove(&task.id);
    drop(bridge);
    agent_send_with_mode(state, task_id, message, AgentSendMode::Auto).await
}

// ============================================================================
// 权限命令
// ============================================================================

pub async fn permission_approve(
    state: &CommandState,
    request_id: &str,
    decision: &str,
) -> Result<(), String> {
    let decision = parse_decision(decision)?;
    state
        .permission_engine
        .decide(request_id, decision)
        .await
        .map_err(err_str)
}

pub async fn permission_pending(
    state: &CommandState,
    task_id: &str,
) -> Result<Vec<PermissionRequest>, String> {
    Ok(state.permission_engine.pending_for_task(task_id).await)
}

// ============================================================================
// 变更命令
// ============================================================================

pub async fn changes_list(state: &CommandState, task_id: &str) -> Result<Vec<FileChange>, String> {
    ChangeService::new(&state.db, state.blobs_dir.clone())
        .list_changes(task_id)
        .await
        .map_err(err_str)
}

pub async fn rollback_file(
    state: &CommandState,
    task_id: &str,
    path: &str,
) -> Result<String, String> {
    let svc = ChangeService::new(&state.db, state.blobs_dir.clone());
    let root = attached_task_workspace_root(state, task_id)?;
    let (path_key, physical_path) = rollback_target(&svc, task_id, &root, path).await?;
    let result = svc
        .rollback_file_at(task_id, &path_key, &physical_path)
        .await
        .map_err(err_str)?;
    Ok(format!("{result:?}"))
}

pub async fn rollback_task(state: &CommandState, task_id: &str) -> Result<Vec<String>, String> {
    let svc = ChangeService::new(&state.db, state.blobs_dir.clone());
    let root = attached_task_workspace_root(state, task_id)?;
    let changes = svc.list_changes(task_id).await.map_err(err_str)?;
    let mut paths: Vec<String> = Vec::new();
    for change in changes.into_iter().rev() {
        if !paths.contains(&change.path) {
            paths.push(change.path);
        }
    }
    let mut results = Vec::with_capacity(paths.len());
    for path in paths {
        let (path_key, physical_path) = rollback_target(&svc, task_id, &root, &path).await?;
        results.push(
            svc.rollback_file_at(task_id, &path_key, &physical_path)
                .await
                .map_err(err_str)?,
        );
    }

    // 与 accept 对称：回滚后把最新主 run 标为 RolledBack，并把任务放回 idle。
    // 原先两件都没做 —— 前端 runPresentation / settledItems 的 rolled_back 分支
    // 因此永远命中不到，回滚过的任务还会卡在 review_ready。
    let runs = AgentRunRepository::new(&state.db);
    if let Some(run) = runs.get_latest_main_run(task_id).map_err(err_str)? {
        // 只改写待审查的 run。已中止 / 已接受的是终态，覆写会丢掉原有结论。
        if run.review_state == ReviewState::Pending {
            runs.update_review_state(&run.id, ReviewState::RolledBack)
                .map_err(err_str)?;
        }
    }
    let tasks = TaskRepository::new(&state.db);
    if let Some(task) = tasks.get(task_id).map_err(err_str)? {
        // 归档任务不因回滚而复活。
        if task.state != TaskState::Archived {
            tasks
                .update_state(task_id, TaskState::Idle)
                .map_err(err_str)?;
        }
    }

    Ok(results.into_iter().map(|r| format!("{r:?}")).collect())
}

pub async fn accept_task(state: &CommandState, task_id: &str) -> Result<(), String> {
    let svc = ReviewService::new(&state.db, state.blobs_dir.clone());
    svc.apply_action(task_id, ReviewAction::AcceptAll)
        .await
        .map_err(err_str)?;
    TaskRepository::new(&state.db)
        .update_state(task_id, TaskState::Idle)
        .map_err(err_str)?;
    Ok(())
}

/// 简易 unified diff：基于 LCS 的行级对比，变化块上下文 ±3 行，省略段用 hunk 行。
/// 任一侧超过 `MAX_DIFF_LINES` 行时降级为全删全增并标记 truncated。
const MAX_DIFF_LINES: usize = 800;
const DIFF_CTX: usize = 3;

fn build_diff_lines(before: &str, after: &str) -> (Vec<ChangeDiffLine>, bool) {
    let old: Vec<&str> = before.lines().collect();
    let new: Vec<&str> = after.lines().collect();

    if old.len() > MAX_DIFF_LINES || new.len() > MAX_DIFF_LINES {
        let mut lines =
            Vec::with_capacity(old.len().min(MAX_DIFF_LINES) + new.len().min(MAX_DIFF_LINES));
        for (i, l) in old.iter().take(MAX_DIFF_LINES).enumerate() {
            lines.push(ChangeDiffLine {
                kind: ChangeDiffLineKind::Del,
                text: (*l).into(),
                old_no: Some(i + 1),
                new_no: None,
            });
        }
        for (i, l) in new.iter().take(MAX_DIFF_LINES).enumerate() {
            lines.push(ChangeDiffLine {
                kind: ChangeDiffLineKind::Add,
                text: (*l).into(),
                old_no: None,
                new_no: Some(i + 1),
            });
        }
        return (lines, true);
    }

    // LCS DP
    let (n, m) = (old.len(), new.len());
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if old[i] == new[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }

    // 回溯生成操作序列
    enum Op {
        Ctx(usize, usize),
        Del(usize),
        Add(usize),
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if old[i] == new[j] {
            ops.push(Op::Ctx(i, j));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            ops.push(Op::Del(i));
            i += 1;
        } else {
            ops.push(Op::Add(j));
            j += 1;
        }
    }
    while i < n {
        ops.push(Op::Del(i));
        i += 1;
    }
    while j < m {
        ops.push(Op::Add(j));
        j += 1;
    }

    // 上下文收敛：保留变化块 ±DIFF_CTX 行，其余用 hunk 行替代
    let keep: Vec<bool> = {
        let mut k = vec![false; ops.len()];
        let changed: Vec<usize> = ops
            .iter()
            .enumerate()
            .filter_map(|(idx, op)| match op {
                Op::Ctx(..) => None,
                _ => Some(idx),
            })
            .collect();
        for &c in &changed {
            let lo = c.saturating_sub(DIFF_CTX);
            let hi = (c + DIFF_CTX + 1).min(ops.len());
            for slot in k.iter_mut().take(hi).skip(lo) {
                *slot = true;
            }
        }
        k
    };

    let mut lines = Vec::new();
    let mut in_gap = false;
    for (idx, op) in ops.iter().enumerate() {
        if !keep[idx] {
            if !in_gap {
                lines.push(ChangeDiffLine {
                    kind: ChangeDiffLineKind::Hunk,
                    text: "···".into(),
                    old_no: None,
                    new_no: None,
                });
                in_gap = true;
            }
            continue;
        }
        in_gap = false;
        match *op {
            Op::Ctx(o, nn) => lines.push(ChangeDiffLine {
                kind: ChangeDiffLineKind::Ctx,
                text: old[o].into(),
                old_no: Some(o + 1),
                new_no: Some(nn + 1),
            }),
            Op::Del(o) => lines.push(ChangeDiffLine {
                kind: ChangeDiffLineKind::Del,
                text: old[o].into(),
                old_no: Some(o + 1),
                new_no: None,
            }),
            Op::Add(nn) => lines.push(ChangeDiffLine {
                kind: ChangeDiffLineKind::Add,
                text: new[nn].into(),
                old_no: None,
                new_no: Some(nn + 1),
            }),
        }
    }
    (lines, false)
}

pub async fn change_diff(
    state: &CommandState,
    task_id: &str,
    path: &str,
) -> Result<ChangeDiff, String> {
    let changes = ChangeService::new(&state.db, state.blobs_dir.clone())
        .list_changes(task_id)
        .await
        .map_err(err_str)?;

    // 宽松匹配：绝对路径相等，或以相对路径结尾
    let change = changes
        .iter()
        .find(|c| c.path == path || c.path.ends_with(path) || path.ends_with(&c.path))
        .ok_or_else(|| format!("no change recorded for path: {path}"))?;

    let blobs = BlobStore::new(&state.db, state.blobs_dir.clone());
    let read_blob = |hash: &Option<String>| -> Result<Option<String>, String> {
        match hash {
            None => Ok(None),
            Some(h) => blobs
                .get(h)
                .map_err(err_str)?
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .map(Some)
                .map(Ok)
                .unwrap_or(Ok(None)),
        }
    };
    let before = read_blob(&change.before_hash)?;
    let after = read_blob(&change.after_hash)?;

    // 两侧内容都拿不到 → 降级为仅元信息
    if before.is_none() && after.is_none() {
        return Ok(ChangeDiff {
            supported: false,
            path: change.path.clone(),
            change_type: Some(change.change_type),
            before_hash: change.before_hash.clone(),
            after_hash: change.after_hash.clone(),
            lines: None,
            truncated: None,
        });
    }

    let (lines, truncated) = build_diff_lines(
        before.as_deref().unwrap_or(""),
        after.as_deref().unwrap_or(""),
    );
    Ok(ChangeDiff {
        supported: true,
        path: change.path.clone(),
        change_type: Some(change.change_type),
        before_hash: change.before_hash.clone(),
        after_hash: change.after_hash.clone(),
        lines: Some(lines),
        truncated: if truncated { Some(true) } else { None },
    })
}

// ============================================================================
// 验证命令
// ============================================================================

pub async fn run_verification(
    state: &CommandState,
    task_id: &str,
    command: &str,
) -> Result<VerificationRecord, String> {
    let workspace_root = attached_task_workspace_root(state, task_id)?;
    // 验证只需要一个合法的 agent_runs 外键（verifications.run_id 是
    // NOT NULL REFERENCES agent_runs(id)，且 PRAGMA foreign_keys=ON）。
    //
    // 原实现在没有活跃 run 时插一条 ended_at=NULL 的主 run，而全仓无人给它收尾：
    // 时间线永久多一条转圈的"运行中"、任务被判定为永久 running、崩溃恢复误报、
    // 还会顶替 get_active_run 让 accept 写到错误的记录上。
    //
    // 而"点验证"的典型时机恰恰是 agent 已经跑完（任务处于 review_ready / idle），
    // 此时 get_active_run 必然是 None —— 所以这是必现而非偶发。
    //
    // 改为：活跃 run → 最近一条主 run → 都没有才补一条**已结束**的占位 run。
    let runs = AgentRunRepository::new(&state.db);
    let run_id = match runs.get_active_run(task_id).map_err(err_str)? {
        Some(active) => active.id,
        None => match runs.get_latest_main_run(task_id).map_err(err_str)? {
            Some(latest) => latest.id,
            None => {
                // 任务从未跑过 agent。占位 run 建出来就是终态，不留 ended_at=NULL。
                // 复用已有的占位 run，避免每点一次验证就多一行。
                match runs
                    .get_verification_placeholder_run(task_id)
                    .map_err(err_str)?
                {
                    Some(existing) => existing.id,
                    None => {
                        let mut placeholder =
                            AgentRun::new(task_id, VERIFICATION_PLACEHOLDER_MODEL);
                        placeholder.ended_at = Some(chrono::Utc::now());
                        runs.create(&placeholder).map_err(err_str)?;
                        placeholder.id
                    }
                }
            }
        },
    };

    let config = VerificationConfig {
        command: command.to_string(),
        timeout_secs: 300,
    };

    VerificationService::new(&state.db, state.blobs_dir.clone())
        .run_verification(task_id, &run_id, &config, &workspace_root)
        .await
        .map_err(err_str)
}

pub async fn verification_list(
    state: &CommandState,
    task_id: &str,
) -> Result<Vec<VerificationRecord>, String> {
    VerificationService::new(&state.db, state.blobs_dir.clone())
        .list_for_task(task_id)
        .await
        .map_err(err_str)
}

// ============================================================================
// Workspace 命令
// ============================================================================

pub async fn workspace_list(state: &CommandState) -> Result<Vec<Workspace>, String> {
    WorkspaceService::new(&state.db)
        .list_recent(20)
        .map_err(err_str)
}

pub async fn workspace_open(state: &CommandState, path: &Path) -> Result<Workspace, String> {
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("cannot open workspace {}: {e}", path.display()))?;
    if !canonical.is_dir() {
        return Err(format!(
            "workspace must be a folder: {}",
            canonical.display()
        ));
    }
    // 构造 PathGuard 作为最后一道 fail-closed 检查；根路径不可规范化时拒绝入库。
    let guard = PathGuard::new(canonical).map_err(err_str)?;
    let canonical_path = guard.root().display().to_string();
    let display_name = guard
        .root()
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace")
        .to_string();
    WorkspaceService::new(&state.db)
        .open(&canonical_path, &display_name)
        .map_err(err_str)
}

pub async fn workspace_set_access_mode(
    state: &CommandState,
    workspace_path: &str,
    access_mode: ProjectAccessMode,
) -> Result<Workspace, String> {
    let root = workspace_root(state, workspace_path)?;
    let canonical_path = root.display().to_string();
    let service = WorkspaceService::new(&state.db);
    service
        .set_access_mode(&canonical_path, access_mode)
        .map_err(err_str)?;
    service
        .get(&canonical_path)
        .map_err(err_str)?
        .ok_or_else(|| "workspace disappeared after access mode update".to_string())
}

// ============================================================================
// 搜索命令
// ============================================================================

pub async fn quick_open(
    state: &CommandState,
    workspace_path: &str,
    query: &str,
    limit: usize,
) -> Result<Vec<String>, String> {
    let svc = SearchService::new(attached_workspace_root(state, workspace_path)?);
    svc.quick_open(query, limit).await.map_err(err_str)
}

pub async fn global_search(
    state: &CommandState,
    workspace_path: &str,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchMatch>, String> {
    let svc = SearchService::new(attached_workspace_root(state, workspace_path)?);
    let cancel = tokio_util::sync::CancellationToken::new();
    let matches = svc
        .global_search(query, limit, cancel)
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

pub async fn terminal_list(state: &CommandState) -> Result<Vec<TerminalInfo>, String> {
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

pub async fn terminal_create(
    state: &CommandState,
    shell: &str,
    workspace_path: &str,
) -> Result<String, String> {
    let svc = TerminalControlService::new(state.terminal_manager.clone());
    let root = attached_workspace_root(state, workspace_path)?;
    svc.create(shell, &root, Vec::new()).await.map_err(err_str)
}

pub async fn terminal_send(
    state: &CommandState,
    id: &str,
    text: &str,
    press_enter: bool,
) -> Result<(), String> {
    let svc = TerminalControlService::new(state.terminal_manager.clone());
    svc.send(
        id,
        None,
        SendOptions {
            text: text.to_string(),
            press_enter,
        },
    )
    .await
    .map_err(err_str)
}

pub async fn terminal_read(state: &CommandState, id: &str) -> Result<String, String> {
    let svc = TerminalControlService::new(state.terminal_manager.clone());
    svc.read(id).await.map_err(err_str)
}

/// 读取终端完整 scrollback，供 UI 在挂载或切换终端时恢复输出。
pub async fn terminal_snapshot(state: &CommandState, id: &str) -> Result<String, String> {
    let svc = TerminalControlService::new(state.terminal_manager.clone());
    svc.snapshot(id).await.map_err(err_str)
}

pub async fn terminal_kill(state: &CommandState, id: &str) -> Result<(), String> {
    let svc = TerminalControlService::new(state.terminal_manager.clone());
    svc.kill(id, false).await.map_err(err_str)
}

pub async fn terminal_resize(
    state: &CommandState,
    id: &str,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    state
        .terminal_manager
        .resize(id, cols, rows)
        .await
        .map_err(err_str)
}

// ============================================================================
// 恢复命令
// ============================================================================

pub async fn recovery_data(state: &CommandState) -> Result<RecoveryPageData, String> {
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

pub async fn recovery_cleanup(state: &CommandState) -> Result<u64, String> {
    // RecoveryManager 直接连数据库文件；内存库（测试）无文件可清，返回 0
    let Some(db_path) = &state.db_path else {
        return Ok(0);
    };
    RecoveryManager::new(db_path.clone())
        .cancel_orphaned_permissions()
        .await
        .map_err(err_str)
}

// ============================================================================
// 支持包命令
// ============================================================================

/// 支持包的数据库路径；内存库降级为空临时文件（db_stats 归零）。
fn support_db_path(state: &CommandState) -> Result<PathBuf, String> {
    match &state.db_path {
        Some(p) => Ok(p.clone()),
        None => {
            let tmp = std::env::temp_dir().join("r-code-in-memory.db");
            std::fs::write(&tmp, b"").map_err(err_str)?;
            Ok(tmp)
        }
    }
}

pub async fn support_bundle(state: &CommandState, output_dir: &str) -> Result<String, String> {
    let bundle = SupportBundle::new(PathBuf::from(output_dir));
    let db_path = support_db_path(state)?;
    let path = bundle.generate(&db_path).await.map_err(err_str)?;
    Ok(path.display().to_string())
}

pub async fn support_preview(state: &CommandState) -> Result<serde_json::Value, String> {
    // output_dir 仅用于定位 r-code.log；预览用 config_dir
    let bundle = SupportBundle::new(state.config_dir.clone());
    let db_path = support_db_path(state)?;
    let contents = bundle.preview(&db_path).await.map_err(err_str)?;
    serde_json::to_value(&contents).map_err(|e| e.to_string())
}

// ============================================================================
// 回放 / 会话消息命令
// ============================================================================

pub async fn replay(
    state: &CommandState,
    session_id: &str,
    depth: &str,
) -> Result<Vec<crate::replay::ReplayEntry>, String> {
    let depth = ReplayDepth::try_from_str(depth)
        .ok_or_else(|| format!("invalid depth: {depth} (expected recap/explore/verify)"))?;
    ReplayService::new(state.sessions_dir.clone())
        .get_replay(session_id, depth)
        .await
        .map_err(err_str)
}

pub async fn session_messages(
    state: &CommandState,
    task_id: &str,
) -> Result<Vec<SessionMessage>, String> {
    let branch = SessionBranchRepository::new(&state.db)
        .ensure_active(task_id)
        .map_err(err_str)?;
    let path = session_file_path(&state.sessions_dir, &branch.storage_id);
    let content = match tokio::fs::read_to_string(&path).await {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(err_str(e)),
    };

    let mut out: Vec<SessionMessage> = Vec::new();
    // JSONL 的 ToolCall 不存 call_id → 按顺序给 tool_call 分配占位 ID，
    // tool_result 的 call_id 映射回最近一条未配对的 tool_call
    let mut pending_calls: Vec<String> = Vec::new();

    for (line_index, line) in content.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<SessionEvent>(line) else {
            continue; // 跳过无法解析的行（崩溃恢复场景）
        };
        let message_id = Some(format!("{}:{}", branch.storage_id, line_index + 1));
        match event {
            SessionEvent::Meta(meta) => out.push(SessionMessage {
                id: message_id,
                branch_id: branch.id.clone(),
                kind: "meta".into(),
                role: None,
                text: Some(format!("{} · {}", meta.provider, meta.model)),
                tool_name: None,
                call_id: None,
                input_json: None,
                output_json: None,
                is_error: None,
                timestamp: Some(meta.created_at.to_rfc3339()),
            }),
            SessionEvent::Message(msg) => {
                let role = match msg.role {
                    hermes_core::Role::User => "user",
                    hermes_core::Role::Assistant => "assistant",
                };
                // 拼接文本块 + Custom 块（file_ref 等）的 @path 占位
                let mut text = String::new();
                for block in &msg.content {
                    match block {
                        hermes_core::ContentBlock::Text { text: t } => text.push_str(t),
                        hermes_core::ContentBlock::Custom { type_name, data } => {
                            if type_name == "file_ref" {
                                if let Some(p) = data.get("path").and_then(|v| v.as_str()) {
                                    if !text.is_empty() {
                                        text.push('\n');
                                    }
                                    text.push('@');
                                    text.push_str(p);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                out.push(SessionMessage {
                    id: message_id,
                    branch_id: branch.id.clone(),
                    kind: "message".into(),
                    role: Some(role.into()),
                    text: Some(text),
                    tool_name: None,
                    call_id: None,
                    input_json: None,
                    output_json: None,
                    is_error: None,
                    timestamp: None,
                });
            }
            SessionEvent::ToolCall { name, input } => {
                let call_id = format!("call-{}", out.len());
                pending_calls.push(call_id.clone());
                out.push(SessionMessage {
                    id: message_id,
                    branch_id: branch.id.clone(),
                    kind: "tool_call".into(),
                    role: None,
                    text: None,
                    tool_name: Some(name),
                    call_id: Some(call_id),
                    input_json: Some(input.to_string()),
                    output_json: None,
                    is_error: None,
                    timestamp: None,
                });
            }
            SessionEvent::ToolResult {
                call_id,
                output,
                is_error,
            } => {
                // call_id 对不上占位时取最近一条未配对 tool_call
                let resolved = if pending_calls.contains(&call_id) {
                    call_id
                } else {
                    pending_calls.pop().unwrap_or(call_id)
                };
                pending_calls.retain(|c| c != &resolved);
                out.push(SessionMessage {
                    id: message_id,
                    branch_id: branch.id.clone(),
                    kind: "tool_result".into(),
                    role: None,
                    text: None,
                    tool_name: None,
                    call_id: Some(resolved),
                    input_json: None,
                    output_json: Some(output.to_string()),
                    is_error: Some(is_error),
                    timestamp: None,
                });
            }
            // 运行时恢复专用的快照不应成为 UI 时间线里的第二份消息记录。
            SessionEvent::HistorySnapshot { .. } => {}
            SessionEvent::System { event, data } => out.push(SessionMessage {
                id: message_id,
                branch_id: branch.id.clone(),
                kind: "system".into(),
                role: None,
                text: Some(event),
                tool_name: None,
                call_id: None,
                input_json: None,
                output_json: Some(data.to_string()),
                is_error: None,
                timestamp: None,
            }),
            SessionEvent::Usage(_) => {}
        }
    }
    Ok(out)
}

// ============================================================================
// 项目记忆命令
// ============================================================================

pub async fn memory_get(state: &CommandState, workspace_path: &str) -> Result<String, String> {
    ProjectMemory::new(attached_workspace_root(state, workspace_path)?)
        .load()
        .map_err(err_str)
}

pub async fn memory_set(
    state: &CommandState,
    workspace_path: &str,
    content: &str,
) -> Result<(), String> {
    ProjectMemory::new(attached_workspace_root(state, workspace_path)?)
        .save(content)
        .map_err(err_str)
}

// ============================================================================
// 日志命令（环形缓冲，应用内查看）
// ============================================================================

pub async fn logs_tail(
    _state: &CommandState,
    limit: usize,
    level: Option<&str>,
) -> Result<Vec<crate::log_buffer::LogEntry>, String> {
    Ok(crate::log_buffer::tail(limit, level))
}

// ============================================================================
// 文件读取 / 验证输出命令
// ============================================================================

/// 文件内容（Editor/Files 预览）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileContent {
    /// 规范路径
    pub path: String,
    /// 文本内容（可能截断）
    pub content: String,
    /// 总行数
    pub total_lines: usize,
    /// 是否因大小上限被截断
    pub truncated: bool,
    /// 当前完整文件内容的修订标识；保存时必须回传，以防覆盖并发改动。
    pub revision: String,
    /// 是否可在内置文本编辑器中安全编辑。
    pub is_editable: bool,
}

/// 目录树的单个直接子项。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileTreeEntry {
    /// 相对于工作区根的统一斜杠路径。
    pub path: String,
    /// 当前层级的显示名称。
    pub name: String,
    /// `true` 表示可展开目录，`false` 表示普通文件。
    pub is_directory: bool,
}

/// 单层目录枚举结果。目录过大时前端可提示继续用快速打开定位。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileTreeListing {
    pub entries: Vec<FileTreeEntry>,
    pub truncated: bool,
}

/// Editor 读取大小上限（512 KiB）。
const MAX_READ_BYTES: usize = 512 * 1024;
const MAX_TREE_ENTRIES: usize = 500;

fn file_content_from_bytes(path: &Path, bytes: &[u8]) -> FileContent {
    let truncated = bytes.len() > MAX_READ_BYTES;
    let slice = if truncated {
        &bytes[..MAX_READ_BYTES]
    } else {
        bytes
    };
    let is_editable = !truncated && !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok();
    let content = String::from_utf8_lossy(slice).into_owned();
    FileContent {
        path: path.display().to_string(),
        total_lines: content.lines().count(),
        content,
        truncated,
        revision: blake3::hash(bytes).to_hex().to_string(),
        is_editable,
    }
}

fn tree_entry_is_hidden(name: &str, is_directory: bool) -> bool {
    is_directory
        && matches!(
            name,
            ".git"
                | ".hg"
                | ".svn"
                | ".cache"
                | ".turbo"
                | "__pycache__"
                | "node_modules"
                | "target"
        )
}

fn relative_workspace_path(root: &Path, path: &Path) -> Result<String, String> {
    path.strip_prefix(root)
        .map_err(|_| "file path resolved outside workspace".to_string())
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
}

/// 枚举工作区内一个目录的直接子项。所有返回路径均为相对工作区根的安全路径。
pub async fn file_list(
    state: &CommandState,
    workspace_path: &str,
    path: Option<&str>,
) -> Result<FileTreeListing, String> {
    let root = attached_workspace_root(state, workspace_path)?;
    let guard = PathGuard::new(root.clone()).map_err(err_str)?;
    let requested = path.unwrap_or("").trim();
    let directory = if requested.is_empty() {
        guard.root().to_path_buf()
    } else {
        resolve_workspace_path(guard.root(), requested)?
    };
    if !directory.is_dir() {
        return Err(format!("cannot list non-directory path: {requested}"));
    }

    let mut entries = Vec::new();
    let mut truncated = false;
    for entry in std::fs::read_dir(&directory).map_err(err_str)? {
        let Ok(entry) = entry else {
            continue;
        };
        if entry
            .file_type()
            .map(|file_type| file_type.is_symlink())
            .unwrap_or(true)
        {
            continue;
        }
        let resolved = match guard.resolve(&entry.path()) {
            Ok(path) => path,
            Err(_) => continue,
        };
        let is_directory = resolved.is_dir();
        if !is_directory && !resolved.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if tree_entry_is_hidden(&name, is_directory) {
            continue;
        }
        entries.push(FileTreeEntry {
            path: relative_workspace_path(guard.root(), &resolved)?,
            name,
            is_directory,
        });
        if entries.len() > MAX_TREE_ENTRIES {
            entries.pop();
            truncated = true;
            break;
        }
    }
    entries.sort_by(|left, right| {
        right
            .is_directory
            .cmp(&left.is_directory)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    Ok(FileTreeListing { entries, truncated })
}

pub async fn file_read(
    state: &CommandState,
    workspace_path: &str,
    path: &str,
) -> Result<FileContent, String> {
    let root = attached_workspace_root(state, workspace_path)?;
    let canon = resolve_workspace_path(&root, path)?;
    if !canon.is_file() {
        return Err(format!("cannot read non-file path: {path}"));
    }
    let bytes = std::fs::read(&canon).map_err(err_str)?;
    Ok(file_content_from_bytes(&canon, &bytes))
}

/// 保存一个已经读取的文本文件。修订标识不匹配时拒绝覆盖，要求前端先重新加载。
pub async fn file_write(
    state: &CommandState,
    workspace_path: &str,
    path: &str,
    content: &str,
    expected_revision: &str,
) -> Result<FileContent, String> {
    if content.len() > MAX_READ_BYTES {
        return Err("文件内容超过内置编辑器 512 KiB 保存上限".to_string());
    }
    let root = attached_workspace_root(state, workspace_path)?;
    let canon = resolve_workspace_path(&root, path)?;
    if !canon.is_file() {
        return Err(format!("cannot write non-file path: {path}"));
    }
    let current = std::fs::read(&canon).map_err(err_str)?;
    if current.len() > MAX_READ_BYTES
        || current.contains(&0)
        || std::str::from_utf8(&current).is_err()
    {
        return Err("二进制、非 UTF-8 或过大的文件不能在内置编辑器中保存".to_string());
    }
    let revision = blake3::hash(&current).to_hex().to_string();
    if revision != expected_revision {
        return Err("文件已在磁盘上变更，请重新加载后再保存".to_string());
    }
    let next = content.as_bytes();
    std::fs::write(&canon, next).map_err(err_str)?;
    Ok(file_content_from_bytes(&canon, next))
}

pub async fn verification_output(state: &CommandState, id: &str) -> Result<String, String> {
    // 查记录的 output_blob_key（VerificationService 无单条查询，直接 SQL）
    let blob_key: Option<String> = {
        let conn = state.db.conn().map_err(err_str)?;
        conn.query_row(
            "SELECT output_blob_key FROM verifications WHERE id = ?1",
            rusqlite::params![id],
            |row| row.get(0),
        )
        .map_err(|e| format!("verification not found: {id}: {e}"))?
    };
    let Some(key) = blob_key else {
        return Ok(String::new());
    };
    let blobs = BlobStore::new(&state.db, state.blobs_dir.clone());
    let bytes = blobs.get(&key).map_err(err_str)?;
    Ok(bytes
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default())
}

// ============================================================================
// 设置命令
// ============================================================================

/// 从设置页一次性提交的 Provider 配置。API key 仅在命令执行期间存在，随后写入
/// OS 凭据库，绝不写入 TOML 或回传 WebView。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSettingsInput {
    pub name: String,
    pub base_url: String,
    pub model: String,
    pub api_key: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    /// 用户显式选择的线路协议 slug（见 `provider_catalog::Protocol::as_str`）。
    ///
    /// 省略 = 沿用该服务已存的选择；从未存过则走
    /// [`infer_protocol_never_responses`]。旧版前端不发这个字段，此时不会有任何
    /// 配置被切到 Responses。
    pub protocol: Option<String>,
    /// 保存后是否立即设为新会话默认服务。省略时沿用旧 IPC 的行为（启用）。
    pub activate: Option<bool>,
}

/// 常用服务的保守默认值，直接取自 [`crate::provider_catalog`]。用户仍可在设置页
/// 覆盖模型或地址。
///
/// 这里曾经维护一份只有 4 条的本地表，与目录里的 29 条预设各说各话；目录才是
/// 唯一事实来源，新增服务只改 `provider_catalog::PRESETS`。
fn provider_preset(name: &str) -> Option<&'static ProviderPreset> {
    crate::provider_catalog::find(name)
}

/// 决定一条已保存的配置该用哪个协议。
///
/// 优先级：**用户存下来的 `protocol` 字段 > 目录推断**。协议是计费和能力都不同
/// 的选择（同一个 base_url 常常多协议并存），只能由用户在设置页显式选，不能替他
/// 猜——所以存了就照存的走，哪怕目录声明了别的。
///
/// 没存过（升级前的旧配置）才推断，且**推断结果永不为 Responses**：Responses 与
/// Chat 在同一地址上往往都可用但计费不同，静默切过去等于替用户改了账单。目录声明
/// Responses 的一律降级为 Chat，等用户自己去设置页选。
fn resolve_effective_protocol(
    name: &str,
    pcfg: &hermes_config::ProviderConfig,
) -> ProviderProtocol {
    pcfg.protocol
        .as_deref()
        .and_then(ProviderProtocol::parse)
        .unwrap_or_else(|| infer_protocol_never_responses(name, &pcfg.base_url))
}

/// 没存过 protocol 时的推断规则，**唯一实现**。
///
/// 保存路径和运行时路径必须共用它：任何一边多写一份，两份规则迟早漂移，结果就是
/// 设置页显示的协议和实际发出的请求对不上。
///
/// 规则 = `resolve_protocol` 的结果，但 Responses 一律降级为 Chat。Responses 与
/// Chat 常常在同一地址上都可用而计费不同，替用户选等于替他改账单。
fn infer_protocol_never_responses(name: &str, base_url: &str) -> ProviderProtocol {
    match crate::provider_catalog::resolve_protocol(name, base_url.trim()) {
        ProviderProtocol::OpenAiResponses => ProviderProtocol::OpenAiChat,
        other => other,
    }
}

/// 保存时该往 `config.toml` 写哪个协议。
///
/// 优先级：本次表单显式选的 > 该服务已存的 > 推断。抽成纯函数是为了能直接测——
/// 走 `settings_save_provider` 会写 OS keychain，而 keychain 是全局的，用真实服务名
/// （`openai` 之类）跑测试会覆盖开发者本机的密钥。
///
/// 最后那步**必须**复用 [`infer_protocol_never_responses`]。这里曾经直接取
/// `preset.protocol`，而 openai / xai 的预设协议就是 Responses——于是任何不带
/// protocol 字段的保存请求（旧版前端、脚本直接调 IPC）都会把旧配置写成 Responses，
/// 运行时再照存值执行，等于从后门绕过「不替用户切协议」这条规矩。
fn protocol_to_persist(
    name: &str,
    base_url: &str,
    requested: Option<&str>,
    stored: Option<ProviderProtocol>,
) -> Result<ProviderProtocol, String> {
    let requested = requested.map(str::trim).filter(|value| !value.is_empty());
    let Some(requested) = requested else {
        return Ok(stored.unwrap_or_else(|| infer_protocol_never_responses(name, base_url)));
    };
    let parsed =
        ProviderProtocol::parse(requested).ok_or_else(|| format!("未知的线路协议“{requested}”"))?;
    // 校验对象是**这个地址**，不是这条预设：备用线路往往是另一个协议口，拿主入口
    // 的 `native` 去卡它会把合法组合拦下。地址被改写到目录以外时不设限。
    if let Some(allowed) = crate::provider_catalog::allowed_protocols(name, base_url) {
        if !allowed.contains(&parsed) {
            let options: Vec<&str> = allowed.iter().map(|p| p.as_str()).collect();
            return Err(format!(
                "该地址不支持“{}”，可选：{}",
                parsed.as_str(),
                options.join(" / ")
            ));
        }
    }
    Ok(parsed)
}

/// 按 [`resolve_effective_protocol`] 的结果构造 Provider 配置。
///
/// 关键点：**分派依据是协议，不是服务名**。目录里 29 条预设有一多半（Kimi、
/// 智谱、MiniMax、火山 `/api/coding`、百炼……）走的是 Anthropic Messages 口，
/// 旧代码「除 anthropic / deepseek 外一律当 OpenAI Chat」会把它们全部发错协议。
fn build_provider_config(
    name: &str,
    pcfg: &hermes_config::ProviderConfig,
) -> hermes_llm::ProviderConfig {
    use crate::provider_catalog::{resolve_reasoning_replay, Protocol};

    let configured = pcfg.base_url.trim();
    // 地址留空时必须回填目录里的默认值：`ProviderConfig::Anthropic { base_url: None }`
    // 会让 AnthropicProvider 打到 api.anthropic.com，把 Kimi / 智谱这些 Anthropic 口
    // 的请求发到 Anthropic 官方去。对 id 为 `anthropic` 的服务回填是无害的——预设
    // 地址与 AnthropicProvider 的内置默认值本就是同一个。
    let base_url = if configured.is_empty() {
        provider_preset(name).map_or("", |preset| preset.base_url)
    } else {
        configured
    };
    let optional_base_url = (!base_url.is_empty()).then(|| base_url.to_string());
    match resolve_effective_protocol(name, pcfg) {
        Protocol::AnthropicMessages => hermes_llm::ProviderConfig::Anthropic {
            api_key: pcfg.api_key.clone(),
            model: pcfg.model.clone(),
            base_url: optional_base_url,
        },
        Protocol::OpenAiResponses => hermes_llm::ProviderConfig::Responses {
            api_key: pcfg.api_key.clone(),
            model: pcfg.model.clone(),
            base_url: base_url.to_string(),
            // 只有目录里明确标了 reasoning_replay 且地址未被改写的服务才打开；
            // 对不支持 `include=reasoning.encrypted_content` 的实现打开会 400。
            reasoning: if resolve_reasoning_replay(name, configured) {
                hermes_llm::ReasoningMode::EncryptedReplay
            } else {
                hermes_llm::ReasoningMode::Drop
            },
        },
        // DeepSeek 也是 OpenAI Chat，但 DeepSeekProvider 会按模型名报出正确的
        // 上下文窗口（v4 为 1M，其余 64K），压缩策略依赖这个值，故保留特例。
        Protocol::OpenAiChat if is_deepseek_chat(name, base_url) => {
            hermes_llm::ProviderConfig::DeepSeek {
                api_key: pcfg.api_key.clone(),
                model: pcfg.model.clone(),
                base_url: optional_base_url,
            }
        }
        Protocol::OpenAiChat => hermes_llm::ProviderConfig::OpenAi {
            api_key: pcfg.api_key.clone(),
            model: pcfg.model.clone(),
            base_url: base_url.to_string(),
        },
    }
}

/// 是否应走 `DeepSeekProvider`（而非通用 OpenAI 兼容实现）。
///
/// 命中条件是「id 就是 `deepseek`」**或**「地址在官方域名下」。前者保证地址留空
/// 时也能命中（此时会回填官方地址）；代价是把 id 为 `deepseek` 的服务改指到自建
/// 网关后，上下文窗口仍按模型名猜（v4 为 1M，其余 64K）——协议一样是 Chat，只影响
/// 压缩策略的估算。
fn is_deepseek_chat(name: &str, base_url: &str) -> bool {
    name == "deepseek" || base_url.contains("api.deepseek.com")
}

/// 服务端声明的单次输出上限，不等同于上下文窗口。
///
/// DeepSeek V4 当前提供 1M context，但每次 Chat Completions 的输出最多为
/// 384K（API 报错中以 393_216 表示）。把上下文窗口填入 `max_tokens` 会被服务端
/// 拒绝，因此在保存和运行旧配置时都使用同一规则保护。
///
/// 其余服务取目录里的 `max_output_tokens`（同样只是输出上限，不是 `context_window`）。
fn provider_max_output_tokens(name: &str, provider: &hermes_config::ProviderConfig) -> Option<u32> {
    let is_v4 = provider
        .model
        .trim()
        .to_ascii_lowercase()
        .starts_with("deepseek-v4-");
    if is_deepseek_chat(name, &provider.base_url) && is_v4 {
        return Some(393_216);
    }
    // 地址被改写过时目录不再可信（preset_for 会返回 None），此时不做钳制。
    crate::provider_catalog::preset_for(name, &provider.base_url)
        .and_then(|preset| preset.max_output_tokens)
}

/// 兼容已保存的旧配置：不因历史上误填的 1M/10M 输出上限而重新触发 400。
fn effective_max_tokens(name: &str, provider: &hermes_config::ProviderConfig) -> Option<u32> {
    match (
        provider.max_tokens,
        provider_max_output_tokens(name, provider),
    ) {
        (Some(requested), Some(limit)) if requested > limit => {
            tracing::warn!(
                provider = name,
                requested_max_tokens = requested,
                effective_max_tokens = limit,
                "configured output limit exceeds the provider maximum; clamping request"
            );
            Some(limit)
        }
        (configured, _) => configured,
    }
}

fn provider_env_has_key(name: &str) -> bool {
    match name {
        "anthropic" => std::env::var("ANTHROPIC_API_KEY").is_ok(),
        "openai" => std::env::var("OPENAI_API_KEY").is_ok(),
        _ => false,
    }
}

/// 只校验一个 Provider 是否足以发起请求。与 `Config::validate` 不同，它不会
/// 让其它未完成的配置草稿影响当前默认服务。
fn provider_readiness_error(
    name: &str,
    provider: &hermes_config::ProviderConfig,
) -> Option<String> {
    if provider.api_key.trim().is_empty() {
        return Some("缺少访问密钥".to_string());
    }
    if provider.model.trim().is_empty() {
        return Some("缺少默认模型".to_string());
    }
    // 地址留空只有在目录能补出默认值时才成立。
    if provider.base_url.trim().is_empty() && !has_default_base_url(name) {
        return Some("缺少接口地址".to_string());
    }
    // 带占位符的预设（Azure 的 ${RESOURCE_NAME}、Bedrock 的 ${AWS_REGION} 等）
    // 必须由用户替换后才能发请求，否则会打到一个字面量域名上。
    if provider.base_url.contains("${") {
        return Some("接口地址中的占位符尚未替换".to_string());
    }
    None
}

/// 地址留空时，目录或 runtime 能否补出一个可用的默认地址。
///
/// `anthropic` / `deepseek` 由 runtime 自带默认值（见 `DeepSeekProvider` 与
/// `AnthropicProvider`）；其余服务只有在目录里有一条不含占位符的 base_url 时才算。
fn has_default_base_url(name: &str) -> bool {
    matches!(name, "anthropic" | "deepseek")
        || provider_preset(name).is_some_and(|preset| !preset.needs_template())
}

/// 校验来自新建/切换会话界面的显式服务选择。
///
/// `None` 保持旧任务/旧 IPC 的兼容行为：任务不会被强行绑定，运行时再回退全局默认。
fn validate_selected_provider(
    state: &CommandState,
    provider_name: Option<&str>,
) -> Result<Option<String>, String> {
    let Some(name) = provider_name.map(str::trim).filter(|name| !name.is_empty()) else {
        return Ok(None);
    };
    let settings = SettingsService::new(state.config_dir.clone());
    let config = settings.load_global_unvalidated().map_err(err_str)?;
    let provider = config
        .providers
        .get(name)
        .ok_or_else(|| format!("未找到模型服务“{name}”"))?;
    if let Some(problem) = provider_readiness_error(name, provider) {
        return Err(format!("模型服务“{name}”尚未就绪：{problem}"));
    }
    Ok(Some(name.to_string()))
}

fn load_config_json_for_editing(state: &CommandState) -> Result<serde_json::Value, String> {
    let path = state.config_dir.join("config.toml");
    if path.exists() {
        let content = std::fs::read_to_string(&path).map_err(err_str)?;
        let tv: toml::Value =
            toml::from_str(&content).map_err(|e| format!("parse {}: {e}", path.display()))?;
        serde_json::to_value(tv).map_err(|e| e.to_string())
    } else {
        serde_json::to_value(&hermes_config::Config::default()).map_err(|e| e.to_string())
    }
}

/// 内置模型服务预设目录，驱动设置页的"新建服务"表单。
///
/// 纯静态数据、不碰磁盘也不碰网络，因此不需要 `state`；保持 `async` 只是为了让
/// `tauri_commands::cmd_provider_catalog` 的转发写法与其它命令一致。
pub async fn provider_catalog() -> Result<serde_json::Value, String> {
    serde_json::to_value(crate::provider_catalog::catalog_dto()).map_err(err_str)
}

pub async fn settings_get(state: &CommandState) -> Result<serde_json::Value, String> {
    // 宽松加载：未配置 provider 不是错误（由用户自行决定是否配置）。
    // runtime 视图会从 OS keychain / 环境变量填充密钥，但返回 WebView 前必须清空。
    let settings = SettingsService::new(state.config_dir.clone());
    let mut config = settings.load_global_unvalidated().map_err(err_str)?;
    let default_provider = config.default_provider.clone();
    let validation = match config.providers.get(&default_provider) {
        Some(provider) => provider_readiness_error(&default_provider, provider),
        None => Some(format!("尚未配置默认模型服务“{default_provider}”")),
    };
    let mut provider_status = serde_json::Map::new();
    for (name, provider) in &mut config.providers {
        let env_name = provider_env_has_key(name).then_some("environment");
        let source = if let Some(source) = env_name {
            source
        } else if settings.provider_secret(name).map_err(err_str)?.is_some() {
            "keychain"
        } else if !provider.api_key.trim().is_empty() {
            // 仅兼容尚未迁移的历史 config；绝不把该值返回前端。
            "legacy_file"
        } else {
            "missing"
        };
        provider_status.insert(
            name.clone(),
            serde_json::json!({
                "configured": !provider.api_key.trim().is_empty(),
                "ready": provider_readiness_error(name, provider).is_none(),
                "source": source,
                // 这条配置**实际**会用的协议。设置页必须显示它而不是自己按预设猜：
                // 前端只看得到 `preset.protocol`，看不到地址被改写后的启发式结果，
                // 两边各猜一次必然漂移（届时表单显示 A、请求发的是 B，而且用户一
                // 点保存就把错误的 A 存了下来）。
                "effective_protocol": resolve_effective_protocol(name, provider).as_str(),
            }),
        );
        provider.api_key.clear();
    }
    let config_json = serde_json::to_value(&config).map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "config": config_json,
        "validation": validation,
        "provider_status": provider_status,
    }))
}

/// 原子保存当前 Provider，并把它设为默认服务。
///
/// 旧版设置页把地址、模型和密钥拆成多次写入，主按钮没有保存密钥，任何中断都会
/// 留下一个不可用 Provider。本命令将这些字段作为一个事务性意图处理：配置文件
/// 永远无密钥，密钥始终只进入系统凭据库。
pub async fn settings_save_provider(
    state: &CommandState,
    input: ProviderSettingsInput,
) -> Result<(), String> {
    let name = input.name.trim().to_string();
    if name.is_empty() {
        return Err("请选择或填写模型服务名称".to_string());
    }

    let mut base_url = input.base_url.trim().to_string();
    let mut model = input.model.trim().to_string();
    if let Some(preset) = provider_preset(&name) {
        if base_url.is_empty() {
            base_url = preset.base_url.to_string();
        }
        if model.is_empty() {
            model = preset.model.to_string();
        }
    }

    if model.is_empty() {
        return Err("请填写默认模型".to_string());
    }
    if base_url.is_empty() && !has_default_base_url(&name) {
        return Err("请填写接口地址".to_string());
    }
    if base_url.contains("${") {
        return Err("请把接口地址里的占位符替换为实际值".to_string());
    }
    if !base_url.is_empty()
        && !(base_url.starts_with("https://") || base_url.starts_with("http://"))
    {
        return Err("接口地址需要以 http:// 或 https:// 开头".to_string());
    }
    if input.max_tokens == Some(0) {
        return Err("最大输出 Token 必须大于 0".to_string());
    }
    let output_limits = hermes_config::ProviderConfig {
        base_url: base_url.clone(),
        api_key: String::new(),
        model: model.clone(),
        max_tokens: input.max_tokens,
        temperature: input.temperature,
        protocol: None,
    };
    if let (Some(requested), Some(limit)) = (
        input.max_tokens,
        provider_max_output_tokens(&name, &output_limits),
    ) {
        if requested > limit {
            // 最常见的误填是把上下文窗口当成最大输出，所以把两者区别写进提示。
            let context_hint = provider_preset(&name)
                .and_then(|preset| preset.context_window)
                .map(|window| format!("，{window} 是上下文窗口，不是单次输出上限"))
                .unwrap_or_default();
            return Err(format!("“{name}”的最大输出为 {limit} Token{context_hint}"));
        }
    }
    if let Some(temperature) = input.temperature {
        if !temperature.is_finite() || !(0.0..=2.0).contains(&temperature) {
            return Err("随机性应为 0 到 2 之间的数字".to_string());
        }
    }

    let settings = SettingsService::new(state.config_dir.clone());
    let mut config_json = load_config_json_for_editing(state)?;
    let legacy_key = config_json
        .get("providers")
        .and_then(|providers| providers.get(&name))
        .and_then(|provider| provider.get("api_key"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string);
    let supplied_key = input
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string);
    let stored_key = settings.provider_secret(&name).map_err(err_str)?;
    if supplied_key.is_none()
        && legacy_key.is_none()
        && stored_key.as_deref().map_or(true, str::is_empty)
        && !provider_env_has_key(&name)
    {
        return Err("请填写访问密钥后再保存".to_string());
    }

    let stored_protocol = config_json
        .get("providers")
        .and_then(|providers| providers.get(&name))
        .and_then(|provider| provider.get("protocol"))
        .and_then(serde_json::Value::as_str)
        .and_then(ProviderProtocol::parse);
    let protocol =
        protocol_to_persist(&name, &base_url, input.protocol.as_deref(), stored_protocol)?;

    let root = config_json
        .as_object_mut()
        .ok_or_else(|| "配置文件根节点必须是对象".to_string())?;
    let providers = root
        .entry("providers".to_string())
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| "providers 配置必须是对象".to_string())?;
    providers.insert(
        name.clone(),
        serde_json::json!({
            "base_url": base_url,
            "api_key": "",
            "model": model,
            "max_tokens": input.max_tokens,
            "temperature": input.temperature,
            "protocol": protocol.as_str(),
        }),
    );
    if input.activate.unwrap_or(true) {
        root.insert(
            "default_provider".to_string(),
            serde_json::Value::String(name.clone()),
        );
    }

    let config: hermes_config::Config = serde_json::from_value(config_json).map_err(err_str)?;
    // 先确认序列化可行，再写入系统凭据；避免无效字段影响用户已有密钥。
    toml::to_string(&config).map_err(err_str)?;
    if let Some(secret) = supplied_key.as_deref().or(legacy_key.as_deref()) {
        settings
            .set_provider_secret(&name, secret)
            .map_err(err_str)?;
    }
    settings.save_global(&config).map_err(err_str)
}

/// 将已有的、可用的 Provider 设为新对话默认服务。
///
/// 单独的切换命令避免前端把 `default_provider` 当作普通字符串写入，从而指向一个
/// 还未填完密钥或模型的配置草稿。
pub async fn settings_select_provider(state: &CommandState, name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("请选择模型服务".to_string());
    }
    let settings = SettingsService::new(state.config_dir.clone());
    let mut config = settings.load_global_unvalidated().map_err(err_str)?;
    let provider = config
        .providers
        .get(name)
        .ok_or_else(|| format!("未找到模型服务“{name}”"))?;
    if let Some(problem) = provider_readiness_error(name, provider) {
        return Err(format!("模型服务“{name}”尚未就绪：{problem}"));
    }
    config.default_provider = name.to_string();
    settings.save_global(&config).map_err(err_str)
}

/// 删除一个已保存的 Provider 及其本地凭据。若它正被使用，会切换到另一项可用服务；
/// 没有候选项时回到未配置状态，而不是留下悬空引用。
pub async fn settings_delete_provider(state: &CommandState, name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("请选择要删除的模型服务".to_string());
    }
    let settings = SettingsService::new(state.config_dir.clone());
    let mut config = settings.load_global_unvalidated().map_err(err_str)?;
    if config.providers.remove(name).is_none() {
        return Err(format!("未找到模型服务“{name}”"));
    }

    if config.default_provider == name {
        config.default_provider = config
            .providers
            .iter()
            .find(|(candidate, provider)| provider_readiness_error(candidate, provider).is_none())
            .map(|(candidate, _)| candidate.clone())
            .unwrap_or_else(|| "anthropic".to_string());
    }

    settings.save_global(&config).map_err(err_str)?;
    settings.set_provider_secret(name, "").map_err(err_str)
}

fn codex_cli_path() -> Option<PathBuf> {
    let candidates: &[&str] = if cfg!(windows) {
        &["codex.exe", "codex"]
    } else {
        &["codex"]
    };
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .flat_map(|dir| candidates.iter().map(move |name| dir.join(name)))
            .find(|path| path.exists())
    })
}

/// 返回 Codex CLI 协作入口的状态。它不会读取、修改或回传认证令牌。
pub async fn codex_integration_status() -> Result<serde_json::Value, String> {
    let manager = SkillManager::new();
    let skill_path = manager
        .install_paths()
        .into_iter()
        .find(|path| path.to_string_lossy().contains(".codex"))
        .ok_or_else(|| "无法确定 Codex Skill 安装位置".to_string())?;
    let skill_status = match std::fs::read(&skill_path) {
        Ok(contents) if contents == SkillManager::skill_content().as_bytes() => "up_to_date",
        Ok(_) => "update_available",
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "not_installed",
        Err(error) => return Err(format!("读取 Codex Skill 状态失败：{error}")),
    };
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let codex_dir = home.join(".codex");
    let config_path = codex_dir.join("config.toml");
    let auth_path = codex_dir.join("auth.json");
    // 仅检查文件存在和大小，不解析内容，更不会把任何认证材料送到 WebView。
    let authenticated = auth_path
        .metadata()
        .map(|metadata| metadata.is_file() && metadata.len() > 0)
        .unwrap_or(false);
    Ok(serde_json::json!({
        "cli_available": codex_cli_path().is_some(),
        "cli_path": codex_cli_path(),
        "config_path": config_path,
        "config_exists": config_path.exists(),
        "auth_path": auth_path,
        "authenticated": authenticated,
        "skill_path": skill_path,
        "skill_status": skill_status,
        "wire_api": "responses",
    }))
}

/// 在用户可见的系统终端中启动 Codex 登录。它不接收任何来自 WebView 的命令文本，
/// 也不读取登录输出或 auth.json；OAuth 交互完全由 Codex CLI 处理。
pub async fn codex_start_login() -> Result<(), String> {
    if codex_cli_path().is_none() {
        return Err("未检测到 Codex CLI。请先安装或打开 Codex Desktop 后重试".to_string());
    }

    #[cfg(windows)]
    {
        Command::new("cmd")
            .args(["/C", "start", "", "cmd", "/K", "codex login"])
            .spawn()
            .map_err(|error| format!("无法启动 Codex 登录终端：{error}"))?;
    }
    #[cfg(not(windows))]
    {
        Command::new("codex")
            .arg("login")
            .spawn()
            .map_err(|error| format!("无法启动 Codex 登录：{error}"))?;
    }
    Ok(())
}

/// 用户显式请求时才把 R-Code 终端协作 Skill 安装到 Codex 的用户目录。
pub async fn codex_install_skill() -> Result<(), String> {
    SkillManager::new().install_codex().map_err(err_str)
}

pub async fn settings_set(
    state: &CommandState,
    key: &str,
    value: serde_json::Value,
) -> Result<(), String> {
    let settings = SettingsService::new(state.config_dir.clone());

    // 宽松加载：不经 validate（配置不完整/损坏时也必须能通过 UI 修复）。
    // 文件按 toml::Value 解析（允许部分字段缺失），再转 JSON 供点分写入。
    let mut config_json = load_config_json_for_editing(state)?;

    // 新 provider 预填完整条目（ProviderConfig 字段必填），保证逐键写入可反序列化
    let parts: Vec<&str> = key.split('.').collect();
    if parts.len() == 3 && parts[0] == "providers" {
        if let Some(obj) = config_json.as_object_mut() {
            let providers = obj
                .entry("providers".to_string())
                .or_insert_with(|| serde_json::json!({}));
            if let Some(pobj) = providers.as_object_mut() {
                pobj.entry(parts[1].to_string()).or_insert_with(
                    || serde_json::json!({"base_url": "", "api_key": "", "model": ""}),
                );
            }
        }
    }

    // api_key 永不落盘：先写入 OS keychain，再把配置文件中的同一字段固定为空串。
    let value = if parts.len() == 3 && parts[0] == "providers" && parts[2] == "api_key" {
        let secret = value
            .as_str()
            .ok_or_else(|| "provider api_key must be a string".to_string())?;
        settings
            .set_provider_secret(parts[1], secret)
            .map_err(err_str)?;
        serde_json::Value::String(String::new())
    } else {
        value
    };

    // 按点分路径设置值
    set_nested_value(&mut config_json, key, value)?;

    // 反序列化回 Config 并保存
    let config = serde_json::from_value(config_json).map_err(|e| e.to_string())?;
    settings.save_global(&config).map_err(err_str)?;
    Ok(())
}

/// 按点分路径设置 JSON Value 中的值。
fn set_nested_value(
    root: &mut serde_json::Value,
    key: &str,
    value: serde_json::Value,
) -> Result<(), String> {
    if key.is_empty() {
        *root = value;
        return Ok(());
    }

    let parts: Vec<&str> = key.split('.').collect();
    let mut current = root;

    for (i, part) in parts.iter().enumerate() {
        if i == parts.len() - 1 {
            if let Some(obj) = current.as_object_mut() {
                obj.insert(part.to_string(), value);
                return Ok(());
            }
            return Err(format!("cannot set '{key}': parent is not an object"));
        }
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

    Ok(())
}

// ============================================================================
// 上下文投喂 [doc-04 §7]
// ============================================================================

/// 创建文件引用上下文块。用于 `@path` 输入：拖拽文件或输入 `@path` 时创建引用块。
pub fn create_file_ref(path: &str, line: Option<u32>) -> serde_json::Value {
    r_code_core::dto::file_ref_data(path, line)
}

/// 创建选区引用上下文块。用于选中代码时冻结快照块注入。
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

    async fn scoped_test_workspace(state: &CommandState) -> String {
        let workspace = workspace_open(state, &state.project_root).await.unwrap();
        workspace_set_access_mode(
            state,
            &workspace.canonical_path,
            ProjectAccessMode::RiskBased,
        )
        .await
        .unwrap();
        workspace.canonical_path
    }

    #[tokio::test]
    async fn task_create_and_list() {
        let (_dir, state) = setup_state();
        let task = task_create(&state, None, "Test", "Do thing", "ask")
            .await
            .unwrap();

        assert!(task.workspace_path.is_none());
        assert_eq!(task.title, "Test");
        assert_eq!(task.mode, TaskMode::Ask);

        let tasks = task_list(&state, None, false).await.unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, task.id);
    }

    #[tokio::test]
    async fn task_create_invalid_mode() {
        let (_dir, state) = setup_state();
        let result = task_create(&state, None, "T", "g", "invalid").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn task_detail_returns_all_data() {
        let (_dir, state) = setup_state();
        let task = task_create(&state, None, "T", "g", "edit").await.unwrap();

        let detail = task_detail(&state, &task.id).await.unwrap();
        assert_eq!(detail.task.id, task.id);
        assert!(detail
            .events
            .iter()
            .any(|e| { e.event_type == TaskEventType::TaskCreated }));
    }

    #[tokio::test]
    async fn scoped_subagent_lifecycle_persists_run_tree_and_replay_log() {
        let (_dir, state) = setup_state();
        let task = task_create(&state, None, "T", "g", "ask").await.unwrap();
        let branch = SessionBranchRepository::new(&state.db)
            .ensure_active(&task.id)
            .unwrap();
        let parent = AgentRun::new_for_branch(&task.id, &branch.id, "test-model");
        AgentRunRepository::new(&state.db).create(&parent).unwrap();
        let scope = AgentEventScope {
            run_id: "child-run".to_string(),
            agent_id: "child-agent".to_string(),
            parent_run_id: Some(parent.id.clone()),
            agent_kind: r_code_core::dto::AgentKind::Subagent,
            agent_label: Some("只读检查".to_string()),
            delegated_by_tool_call_id: Some("delegate-call".to_string()),
        };
        let event = AgentEvent::Scoped {
            scope: scope.clone(),
            event: Box::new(AgentEvent::SubagentLifecycle {
                state: SubagentState::Queued,
                detail: Some("已加入队列".to_string()),
            }),
        };
        let delegated = AgentEvent::ToolCall {
            name: "delegate_task".to_string(),
            input: serde_json::json!({ "goal": "只读检查" }),
            call_id: "delegate-call".to_string(),
        };
        let mut pending_text = HashMap::new();
        persist_runtime_event(
            &state.db,
            &state.session_store,
            &state.sessions_dir,
            &task.id,
            &branch.id,
            &parent.id,
            &branch.storage_id,
            &delegated,
            &mut pending_text,
        )
        .await;

        let delegated_result = AgentEvent::ToolResult {
            call_id: "delegate-call".to_string(),
            output: serde_json::json!({ "subagent_id": "child-run" }),
            is_error: false,
        };
        persist_runtime_event(
            &state.db,
            &state.session_store,
            &state.sessions_dir,
            &task.id,
            &branch.id,
            &parent.id,
            &branch.storage_id,
            &delegated_result,
            &mut pending_text,
        )
        .await;

        persist_runtime_event(
            &state.db,
            &state.session_store,
            &state.sessions_dir,
            &task.id,
            &branch.id,
            &parent.id,
            &branch.storage_id,
            &event,
            &mut pending_text,
        )
        .await;
        let child_tool = AgentEvent::Scoped {
            scope: scope.clone(),
            event: Box::new(AgentEvent::ToolCall {
                name: "read_file".to_string(),
                input: serde_json::json!({ "path": "README.md" }),
                call_id: "child-read".to_string(),
            }),
        };
        persist_runtime_event(
            &state.db,
            &state.session_store,
            &state.sessions_dir,
            &task.id,
            &branch.id,
            &parent.id,
            &branch.storage_id,
            &child_tool,
            &mut pending_text,
        )
        .await;
        let child_result = AgentEvent::Scoped {
            scope: scope.clone(),
            event: Box::new(AgentEvent::ToolResult {
                call_id: "child-read".to_string(),
                output: serde_json::json!({ "content": "ok" }),
                is_error: false,
            }),
        };
        persist_runtime_event(
            &state.db,
            &state.session_store,
            &state.sessions_dir,
            &task.id,
            &branch.id,
            &parent.id,
            &branch.storage_id,
            &child_result,
            &mut pending_text,
        )
        .await;
        let child_completed = AgentEvent::Scoped {
            scope,
            event: Box::new(AgentEvent::SubagentLifecycle {
                state: SubagentState::Completed,
                detail: Some("已找到所需信息".to_string()),
            }),
        };
        persist_runtime_event(
            &state.db,
            &state.session_store,
            &state.sessions_dir,
            &task.id,
            &branch.id,
            &parent.id,
            &branch.storage_id,
            &child_completed,
            &mut pending_text,
        )
        .await;

        let child = AgentRunRepository::new(&state.db)
            .get("child-run")
            .unwrap()
            .unwrap();
        assert_eq!(child.parent_run_id.as_deref(), Some(parent.id.as_str()));
        assert_eq!(child.agent_label.as_deref(), Some("只读检查"));
        assert_eq!(child.summary.as_deref(), Some("已找到所需信息"));
        assert_eq!(
            child.delegated_by_tool_call_id.as_deref(),
            Some("delegate-call")
        );
        let conn = state.db.conn().unwrap();
        let (caller, run_id, status): (Option<String>, String, String) = conn
            .query_row(
                "SELECT caller, run_id, status FROM tool_calls WHERE id = 'child-read'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(caller.as_deref(), Some("subagent:child-agent"));
        assert_eq!(run_id, "child-run");
        assert_eq!(status, "ok");
        drop(conn);
        let child_log = session_file_path(
            &state.sessions_dir,
            &subagent_storage_id(&branch.storage_id, "child-run"),
        );
        assert!(std::fs::read_to_string(child_log)
            .unwrap()
            .contains("subagent_lifecycle"));
        let events = TaskEventStore::new(&state.db)
            .list_by_task_branch(&task.id, &branch.id, Some(100), None)
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == TaskEventType::ToolCall)
                .count(),
            1,
            "主分支只保留自身的 delegate_task 时间锚点"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.event_type == TaskEventType::ToolResult)
                .count(),
            1,
            "子代理工具结果不能污染主分支时间锚点"
        );
    }

    #[tokio::test]
    async fn permission_approve_and_pending() {
        let (_dir, state) = setup_state();
        let task = task_create(&state, None, "T", "g", "edit").await.unwrap();

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

        let pending = permission_pending(&state, &task.id).await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].id, req.id);

        permission_approve(&state, &req.id, "allow").await.unwrap();

        let pending_after = permission_pending(&state, &task.id).await.unwrap();
        assert!(pending_after.is_empty());
    }

    #[tokio::test]
    async fn permission_approve_invalid_decision() {
        let (_dir, state) = setup_state();
        let result = permission_approve(&state, "req1", "maybe").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn rollback_file_restores_content() {
        let (_dir, state) = setup_state();
        let workspace_path = scoped_test_workspace(&state).await;
        let task = task_create(&state, Some(&workspace_path), "T", "g", "edit")
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

        assert_eq!(
            std::fs::read_to_string(&file_path).unwrap(),
            "modified content"
        );

        let result = rollback_file(&state, &task.id, "test.txt").await.unwrap();
        assert!(result.contains("Restored"));

        assert_eq!(
            std::fs::read_to_string(&file_path).unwrap(),
            "original content"
        );
    }

    #[tokio::test]
    async fn agent_send_creates_session() {
        let (_dir, state) = setup_state();
        let task = task_create(&state, None, "T", "g", "ask").await.unwrap();

        agent_send(&state, &task.id, "Hello agent").await.unwrap();

        let session_path = state.sessions_dir.join(format!("{}.jsonl", task.id));
        assert!(session_path.exists());

        let content = std::fs::read_to_string(&session_path).unwrap();
        assert!(content.contains("Hello agent"));
    }

    #[tokio::test]
    async fn agent_send_mock_run_completes_zero_change_to_idle() {
        let (_dir, state) = setup_state();
        let task = task_create(&state, None, "T", "g", "ask").await.unwrap();

        agent_send(&state, &task.id, "do it").await.unwrap();

        // 等 drain 循环消费完 mock 场景（连续空 poll 退出）
        tokio::time::sleep(std::time::Duration::from_millis(900)).await;

        let detail = task_detail(&state, &task.id).await.unwrap();
        // 零变更 turn → idle（"已回答"语义）；有变更才 review_ready
        assert_eq!(detail.task.state, TaskState::Idle);
        assert!(detail.runs.iter().all(|r| r.ended_at.is_some()));

        // session JSONL 应含 assistant 消息与工具调用
        let msgs = session_messages(&state, &task.id).await.unwrap();
        assert!(msgs
            .iter()
            .any(|m| m.kind == "message" && m.role.as_deref() == Some("assistant")));
        assert!(msgs.iter().any(|m| m.kind == "tool_call"));
        assert!(msgs.iter().any(|m| m.kind == "tool_result"));
        assert!(msgs.iter().any(|m| m.kind == "system"));
    }

    #[tokio::test]
    async fn agent_abort_updates_state() {
        let (_dir, state) = setup_state();
        let task = task_create(&state, None, "T", "g", "edit").await.unwrap();

        TaskRepository::new(&state.db)
            .update_state(&task.id, TaskState::InProgress)
            .unwrap();

        agent_abort(&state, &task.id).await.unwrap();

        let detail = task_detail(&state, &task.id).await.unwrap();
        assert_eq!(detail.task.state, TaskState::Interrupted);
        assert!(
            !detail
                .events
                .iter()
                .any(|event| event.event_type == TaskEventType::RunEnded),
            "没有活跃运行时中止不得制造孤儿 run_ended 事件"
        );
    }

    #[tokio::test]
    async fn queued_message_starts_after_active_run_finishes() {
        let (_dir, state) = setup_state();
        let task = task_create(&state, None, "T", "g", "ask").await.unwrap();

        agent_send(&state, &task.id, "first").await.unwrap();
        agent_send_with_mode(&state, &task.id, "second", AgentSendMode::Queue)
            .await
            .unwrap();

        let queued = agent_queue_list(&state, &task.id).await.unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].message, "second");

        tokio::time::sleep(std::time::Duration::from_millis(1_800)).await;
        let detail = task_detail(&state, &task.id).await.unwrap();
        assert_eq!(detail.runs.len(), 2);
        assert!(detail.runs.iter().all(|run| run.ended_at.is_some()));
        assert!(detail.queued_messages.is_empty());
        let messages = session_messages(&state, &task.id).await.unwrap();
        assert!(messages.iter().any(|message| {
            message.role.as_deref() == Some("user") && message.text.as_deref() == Some("second")
        }));
        assert!(messages.iter().any(|message| {
            message.kind == "system"
                && message.text.as_deref() == Some(USER_MESSAGE_MODE_EVENT)
                && message
                    .output_json
                    .as_deref()
                    .is_some_and(|payload| payload.contains("\"mode\":\"queue\""))
        }));
    }

    #[tokio::test]
    async fn queued_message_dispatches_immediately_when_runtime_is_idle() {
        let (_dir, state) = setup_state();
        let task = task_create(&state, None, "T", "g", "ask").await.unwrap();

        agent_send_with_mode(&state, &task.id, "queued first", AgentSendMode::Queue)
            .await
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(900)).await;
        let detail = task_detail(&state, &task.id).await.unwrap();
        assert_eq!(detail.runs.len(), 1);
        assert!(detail.queued_messages.is_empty());
        let messages = session_messages(&state, &task.id).await.unwrap();
        assert!(messages.iter().any(|message| {
            message.role.as_deref() == Some("user")
                && message.text.as_deref() == Some("queued first")
        }));
    }

    #[tokio::test]
    async fn send_now_interrupts_current_run_and_prioritizes_new_message() {
        let (_dir, state) = setup_state();
        let task = task_create(&state, None, "T", "g", "ask").await.unwrap();

        agent_send(&state, &task.id, "first").await.unwrap();
        agent_send_with_mode(&state, &task.id, "urgent", AgentSendMode::SendNow)
            .await
            .unwrap();

        tokio::time::sleep(std::time::Duration::from_millis(1_800)).await;
        let detail = task_detail(&state, &task.id).await.unwrap();
        assert_eq!(detail.runs.len(), 2);
        assert!(detail
            .runs
            .iter()
            .any(|run| run.review_state == ReviewState::Aborted));
        let messages = session_messages(&state, &task.id).await.unwrap();
        assert!(messages.iter().any(|message| {
            message.role.as_deref() == Some("user") && message.text.as_deref() == Some("urgent")
        }));
        assert!(messages.iter().any(|message| {
            message.kind == "system"
                && message.text.as_deref() == Some(USER_MESSAGE_MODE_EVENT)
                && message
                    .output_json
                    .as_deref()
                    .is_some_and(|payload| payload.contains("\"mode\":\"send_now\""))
        }));
    }

    #[tokio::test]
    async fn auto_send_for_another_task_is_queued_not_steered() {
        let (_dir, state) = setup_state();
        let first = task_create(&state, None, "First", "g", "ask")
            .await
            .unwrap();
        let second = task_create(&state, None, "Second", "g", "ask")
            .await
            .unwrap();

        agent_send(&state, &first.id, "first task").await.unwrap();
        agent_send(&state, &second.id, "second task").await.unwrap();

        let first_messages = session_messages(&state, &first.id).await.unwrap();
        assert!(!first_messages.iter().any(|message| {
            message.role.as_deref() == Some("user")
                && message.text.as_deref() == Some("second task")
        }));
        let queued = agent_queue_list(&state, &second.id).await.unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].message, "second task");
    }

    #[tokio::test]
    async fn resend_creates_a_new_branch_without_rewriting_source_log() {
        let (_dir, state) = setup_state();
        let task = task_create(&state, None, "T", "g", "ask").await.unwrap();
        agent_send(&state, &task.id, "original").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(900)).await;

        let original_messages = session_messages(&state, &task.id).await.unwrap();
        let original_id = original_messages
            .iter()
            .find(|message| {
                message.role.as_deref() == Some("user")
                    && message.text.as_deref() == Some("original")
            })
            .and_then(|message| message.id.clone())
            .unwrap();
        let source_path = state.sessions_dir.join(format!("{}.jsonl", task.id));
        let source_before = std::fs::read_to_string(&source_path).unwrap();

        agent_resend(&state, &task.id, &original_id, "edited")
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(900)).await;

        let detail = task_detail(&state, &task.id).await.unwrap();
        assert_ne!(detail.active_branch.id, "main");
        let branches = SessionBranchRepository::new(&state.db)
            .list_by_task(&task.id)
            .unwrap();
        assert_eq!(branches.len(), 2);
        let current = session_messages(&state, &task.id).await.unwrap();
        assert!(current.iter().any(|message| {
            message.role.as_deref() == Some("user") && message.text.as_deref() == Some("edited")
        }));
        assert!(!current.iter().any(|message| {
            message.role.as_deref() == Some("user") && message.text.as_deref() == Some("original")
        }));
        assert_eq!(std::fs::read_to_string(source_path).unwrap(), source_before);
    }

    #[tokio::test]
    async fn workspace_open_and_list() {
        let (_dir, state) = setup_state();
        let ws = workspace_open(&state, &state.project_root).await.unwrap();
        assert_eq!(
            ws.canonical_path,
            state
                .project_root
                .canonicalize()
                .unwrap()
                .display()
                .to_string()
        );

        let list = workspace_list(&state).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].access_mode, ProjectAccessMode::RequestApproval);
    }

    #[tokio::test]
    async fn workspace_access_mode_is_persisted_per_project() {
        let (_dir, state) = setup_state();
        let ws = workspace_open(&state, &state.project_root).await.unwrap();
        let updated =
            workspace_set_access_mode(&state, &ws.canonical_path, ProjectAccessMode::FullAccess)
                .await
                .unwrap();
        assert_eq!(updated.access_mode, ProjectAccessMode::FullAccess);

        let reopened = workspace_open(&state, &state.project_root).await.unwrap();
        assert_eq!(reopened.access_mode, ProjectAccessMode::FullAccess);
    }

    #[tokio::test]
    async fn recovery_data_returns_empty_for_fresh_db() {
        let (_dir, state) = setup_state();
        let data = recovery_data(&state).await.unwrap();
        assert!(data.interrupted_tasks.is_empty());
        assert_eq!(data.orphaned_permissions, 0);
    }

    #[tokio::test]
    async fn recovery_cleanup_in_memory_is_noop() {
        let (_dir, state) = setup_state();
        // 内存库无文件路径，清理返回 0
        assert_eq!(recovery_cleanup(&state).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn settings_get_returns_json() {
        let (_dir, state) = setup_state();
        let result = settings_get(&state).await;
        // 可能成功（返回 JSON）或失败（无有效 provider 配置）
        // 两种情况都可接受，关键是函数不 panic
        if let Ok(val) = result {
            assert!(val.is_object());
        }
    }

    #[tokio::test]
    async fn settings_set_creates_new_provider_incrementally() {
        let (_dir, state) = setup_state();
        // 逐键写入一个新 provider（每次单键，最终条目必须完整可用）
        settings_set(
            &state,
            "providers.openrouter.base_url",
            serde_json::json!("https://openrouter.ai/api/v1"),
        )
        .await
        .unwrap();
        settings_set(
            &state,
            "providers.openrouter.api_key",
            serde_json::json!("sk-or-test"),
        )
        .await
        .unwrap();
        settings_set(
            &state,
            "providers.openrouter.model",
            serde_json::json!("auto"),
        )
        .await
        .unwrap();
        settings_set(&state, "default_provider", serde_json::json!("openrouter"))
            .await
            .unwrap();

        let cfg = settings_get(&state).await.unwrap();
        assert_eq!(cfg["config"]["default_provider"], "openrouter");
        assert_eq!(cfg["config"]["providers"]["openrouter"]["model"], "auto");
        assert_eq!(
            cfg["config"]["providers"]["openrouter"]["base_url"],
            "https://openrouter.ai/api/v1"
        );
        // 已配置完整 → 无校验软提示
        assert!(cfg["validation"].is_null());
    }

    #[tokio::test]
    async fn settings_save_provider_keeps_key_and_enables_chat_in_one_step() {
        let (_dir, state) = setup_state();
        let provider_name = format!("r-code-test-{}", uuid::Uuid::new_v4());

        settings_save_provider(
            &state,
            ProviderSettingsInput {
                name: provider_name.clone(),
                base_url: "https://api.example.com/v1".into(),
                model: "test-model".into(),
                api_key: Some("sk-one-step-test".into()),
                max_tokens: Some(2048),
                temperature: Some(0.2),
                protocol: None,
                activate: None,
            },
        )
        .await
        .unwrap();

        let cfg = settings_get(&state).await.unwrap();
        assert_eq!(cfg["config"]["default_provider"], provider_name);
        assert_eq!(
            cfg["config"]["providers"][provider_name.as_str()]["base_url"],
            "https://api.example.com/v1"
        );
        assert_eq!(
            cfg["config"]["providers"][provider_name.as_str()]["model"],
            "test-model"
        );
        assert_eq!(
            cfg["provider_status"][provider_name.as_str()]["configured"],
            true
        );
        assert_eq!(
            cfg["provider_status"][provider_name.as_str()]["ready"],
            true
        );
        assert!(cfg["validation"].is_null());

        let config_file = std::fs::read_to_string(state.config_dir.join("config.toml")).unwrap();
        assert!(!config_file.contains("sk-one-step-test"));

        // 留空密钥不会把刚保存的凭据清掉；运行时可直接基于默认 Provider 创建。
        settings_save_provider(
            &state,
            ProviderSettingsInput {
                name: provider_name.clone(),
                base_url: "https://api.example.com/v1".into(),
                model: "test-model".into(),
                api_key: None,
                max_tokens: None,
                temperature: None,
                protocol: None,
                activate: None,
            },
        )
        .await
        .unwrap();
        let mut bridge = AgentBridge::new();
        bridge.enable_real_mode();
        ensure_real_runtime(&state.config_dir, &state.tool_gateway, &mut bridge, None)
            .await
            .unwrap();
        assert!(matches!(bridge.kind, AgentRuntimeKind::Real(_)));
        SettingsService::new(state.config_dir.clone())
            .set_provider_secret(&provider_name, "")
            .unwrap();
    }

    #[tokio::test]
    async fn task_provider_binding_survives_default_changes_and_switches_while_idle() {
        let (_dir, state) = setup_state();
        let first = format!("session-first-{}", uuid::Uuid::new_v4());
        let second = format!("session-second-{}", uuid::Uuid::new_v4());
        for (name, activate) in [(&first, true), (&second, false)] {
            settings_save_provider(
                &state,
                ProviderSettingsInput {
                    name: name.clone(),
                    base_url: "https://api.example.com/v1".into(),
                    model: format!("{name}-model"),
                    api_key: Some(format!("sk-{name}")),
                    max_tokens: Some(2048),
                    temperature: Some(0.2),
                    protocol: None,
                    activate: Some(activate),
                },
            )
            .await
            .unwrap();
        }

        let task = task_create_with_provider(
            &state,
            None,
            "绑定服务",
            "验证会话服务绑定",
            "ask",
            Some(&first),
        )
        .await
        .unwrap();
        assert_eq!(task.provider_name.as_deref(), Some(first.as_str()));

        settings_select_provider(&state, &second).await.unwrap();
        let still_bound = TaskRepository::new(&state.db)
            .get(&task.id)
            .unwrap()
            .unwrap();
        assert_eq!(still_bound.provider_name.as_deref(), Some(first.as_str()));

        let switched = task_set_provider(&state, &task.id, &second).await.unwrap();
        assert_eq!(switched.provider_name.as_deref(), Some(second.as_str()));

        let settings = SettingsService::new(state.config_dir.clone());
        settings.set_provider_secret(&first, "").unwrap();
        settings.set_provider_secret(&second, "").unwrap();
    }

    #[test]
    fn built_in_provider_presets_have_usable_defaults() {
        let openai = provider_preset("openai").unwrap();
        assert_eq!(openai.base_url, "https://api.openai.com/v1");
        assert!(!openai.model.is_empty());

        let deepseek = provider_preset("deepseek").unwrap();
        assert_eq!(deepseek.base_url, "https://api.deepseek.com");
        assert_eq!(deepseek.model, "deepseek-v4-flash");
    }

    /// 未存过 protocol 的配置（即升级前保存的旧配置）。
    fn provider_cfg(base_url: &str, model: &str) -> hermes_config::ProviderConfig {
        hermes_config::ProviderConfig {
            base_url: base_url.into(),
            api_key: "sk-test".into(),
            model: model.into(),
            max_tokens: None,
            temperature: None,
            protocol: None,
        }
    }

    /// 用户在设置页显式选过协议的配置。
    fn provider_cfg_with(
        base_url: &str,
        model: &str,
        protocol: ProviderProtocol,
    ) -> hermes_config::ProviderConfig {
        hermes_config::ProviderConfig {
            protocol: Some(protocol.as_str().to_string()),
            ..provider_cfg(base_url, model)
        }
    }

    /// 回归：目录里一多半预设走 Anthropic Messages 口，旧的按名字分派会把它们
    /// 全部当成 OpenAI Chat 发出去。
    #[test]
    fn anthropic_style_gateways_are_not_dispatched_as_openai() {
        for id in ["kimi", "zhipu", "minimax", "ark_coding", "bailian"] {
            let preset = provider_preset(id).unwrap();
            let built = build_provider_config(id, &provider_cfg(preset.base_url, preset.model));
            assert!(
                matches!(built, hermes_llm::ProviderConfig::Anthropic { .. }),
                "{id} 应走 Anthropic Messages，实际 {built:?}"
            );
        }
    }

    /// 同一家厂商的不同入口是不同协议，必须按 base_url 区分，不能按名字前缀。
    #[test]
    fn same_vendor_different_endpoints_get_different_protocols() {
        let anthropic_port = provider_preset("ark_coding").unwrap();
        assert!(matches!(
            build_provider_config(
                "ark_coding",
                &provider_cfg(anthropic_port.base_url, anthropic_port.model)
            ),
            hermes_llm::ProviderConfig::Anthropic { .. }
        ));

        let openai_port = provider_preset("ark_coding_openai").unwrap();
        assert!(matches!(
            build_provider_config(
                "ark_coding_openai",
                &provider_cfg(openai_port.base_url, openai_port.model)
            ),
            hermes_llm::ProviderConfig::OpenAi { .. }
        ));
    }

    /// 地址被改写 = 预设不再可信，退回按地址猜；自建网关基本只有 Chat。
    #[test]
    fn rewritten_base_url_falls_back_to_chat_completions() {
        let built = build_provider_config(
            "openai",
            &provider_cfg("https://my-gateway.internal/v1", "gpt-5.6-sol"),
        );
        assert!(
            matches!(built, hermes_llm::ProviderConfig::OpenAi { .. }),
            "自建网关不应被当成 Responses，实际 {built:?}"
        );
    }

    /// 回归：**绝不替用户自动切到 Responses**。
    ///
    /// 目录声明 openai 官方走 Responses，但升级前存下的配置里没有 protocol 字段。
    /// 若此时按目录推断，用户会在毫不知情的情况下换一套 API 和计费方式。
    #[test]
    fn legacy_config_is_never_auto_upgraded_to_responses() {
        let openai = provider_preset("openai").unwrap();
        assert_eq!(openai.protocol, ProviderProtocol::OpenAiResponses);

        let built = build_provider_config("openai", &provider_cfg(openai.base_url, openai.model));
        assert!(
            matches!(built, hermes_llm::ProviderConfig::OpenAi { .. }),
            "没存过 protocol 的旧配置必须留在 Chat，实际 {built:?}"
        );
    }

    /// 存了什么就走什么——目录只提供推荐值，不覆盖用户的选择。
    #[test]
    fn stored_protocol_wins_over_the_catalog() {
        let openai = provider_preset("openai").unwrap();

        // 手动选了 Responses 才会走 Responses
        let built = build_provider_config(
            "openai",
            &provider_cfg_with(
                openai.base_url,
                openai.model,
                ProviderProtocol::OpenAiResponses,
            ),
        );
        match built {
            hermes_llm::ProviderConfig::Responses { reasoning, .. } => {
                // EncryptedReplay 只对目录里标了 reasoning_replay 的服务打开
                assert_eq!(reasoning, hermes_llm::ReasoningMode::EncryptedReplay);
            }
            other => panic!("显式选了 Responses 却没走，实际 {other:?}"),
        }

        // 反过来：目录说 Anthropic，用户偏要 Chat，也照办
        let kimi = provider_preset("kimi").unwrap();
        assert_eq!(kimi.protocol, ProviderProtocol::AnthropicMessages);
        let built = build_provider_config(
            "kimi",
            &provider_cfg_with(kimi.base_url, kimi.model, ProviderProtocol::OpenAiChat),
        );
        assert!(
            matches!(built, hermes_llm::ProviderConfig::OpenAi { .. }),
            "用户选的 Chat 被目录覆盖了，实际 {built:?}"
        );
    }

    /// 回归：保存路径同样不能把旧配置升级到 Responses。
    ///
    /// 运行时那条「推断永不为 Responses」的规则曾被保存路径绕过——`None` 分支直接
    /// 取 `preset.protocol`，而 openai 的预设协议正是 Responses。于是旧版前端（不发
    /// protocol 字段）一次保存就把配置写成 Responses，运行时再照存值执行。
    #[test]
    fn saving_without_protocol_never_writes_responses() {
        // openai 的预设协议是 Responses，正是最容易被静默升级的那条
        let preset = provider_preset("openai").unwrap();
        assert_eq!(preset.protocol, ProviderProtocol::OpenAiResponses);

        let persisted = protocol_to_persist("openai", preset.base_url, None, None).unwrap();
        assert_eq!(
            persisted,
            ProviderProtocol::OpenAiChat,
            "不带 protocol 的保存把配置升级到了 {persisted:?}"
        );
    }

    /// 用户选过之后，后续不带 protocol 的保存必须沿用他的选择，不能被预设改回去。
    #[test]
    fn saving_without_protocol_keeps_the_stored_choice() {
        let preset = provider_preset("openai").unwrap();
        for stored in [
            ProviderProtocol::OpenAiResponses,
            ProviderProtocol::OpenAiChat,
        ] {
            assert_eq!(
                protocol_to_persist("openai", preset.base_url, None, Some(stored)).unwrap(),
                stored,
                "已存的选择被后续保存冲掉了"
            );
        }
    }

    /// 目录不认的协议要被拒绝，避免存下一个必然 400 的组合。
    #[test]
    fn saving_a_protocol_the_preset_does_not_support_is_rejected() {
        let preset = provider_preset("zhipu_coding").unwrap();
        assert!(!preset.native.contains(&ProviderProtocol::OpenAiResponses));

        let error = protocol_to_persist(
            "zhipu_coding",
            preset.base_url,
            Some("openai_responses"),
            None,
        )
        .unwrap_err();
        assert!(
            error.contains("该地址不支持") && error.contains("openai_chat"),
            "实际报错：{error}"
        );

        // 但地址被改写后不设限：自建网关实现了什么我们无从知道
        let rewritten = "https://relay.example.com/v1";
        let allowed =
            protocol_to_persist("zhipu_coding", rewritten, Some("openai_responses"), None).unwrap();
        assert_eq!(allowed, ProviderProtocol::OpenAiResponses);

        // 写错的字面量要报错，而不是静默落到某个默认值
        let typo = protocol_to_persist("openai", preset.base_url, Some("grpc_whatever"), None)
            .unwrap_err();
        assert!(typo.contains("未知的线路协议"), "实际报错：{typo}");
    }

    /// 只改协议、其它字段不动时，runtime 指纹必须变，否则当前进程不会重建 provider。
    #[test]
    fn protocol_participates_in_the_runtime_fingerprint() {
        let preset = provider_preset("openai").unwrap();
        let chat = provider_cfg_with(preset.base_url, preset.model, ProviderProtocol::OpenAiChat);
        let responses = provider_cfg_with(
            preset.base_url,
            preset.model,
            ProviderProtocol::OpenAiResponses,
        );
        assert_ne!(
            resolve_effective_protocol("openai", &chat),
            resolve_effective_protocol("openai", &responses)
        );

        // 旧配置补上一个与推断结果相同的值时，有效协议没变，不该白白重建 runtime
        let legacy = provider_cfg(preset.base_url, preset.model);
        assert_eq!(
            resolve_effective_protocol("openai", &legacy),
            resolve_effective_protocol("openai", &chat)
        );
    }

    /// 无法识别的 protocol 字面量（手改配置文件写错）退回推断，不 panic。
    #[test]
    fn unknown_stored_protocol_falls_back_to_inference() {
        let kimi = provider_preset("kimi").unwrap();
        let mut cfg = provider_cfg(kimi.base_url, kimi.model);
        cfg.protocol = Some("grpc_whatever".into());
        assert!(matches!(
            build_provider_config("kimi", &cfg),
            hermes_llm::ProviderConfig::Anthropic { .. }
        ));
    }

    /// 地址留空必须回填预设默认值，否则 Anthropic 口的服务会打到 api.anthropic.com。
    #[test]
    fn blank_base_url_is_backfilled_from_the_catalog() {
        match build_provider_config("kimi", &provider_cfg("", "kimi-k2.7-code")) {
            hermes_llm::ProviderConfig::Anthropic { base_url, .. } => {
                assert_eq!(
                    base_url.as_deref(),
                    Some(provider_preset("kimi").unwrap().base_url)
                );
            }
            other => panic!("kimi 应走 Anthropic Messages，实际 {other:?}"),
        }

        // anthropic 自己回填出来的就是 AnthropicProvider 的内置默认值，等价于 None。
        match build_provider_config("anthropic", &provider_cfg("", "claude-sonnet-5")) {
            hermes_llm::ProviderConfig::Anthropic { base_url, .. } => {
                assert_eq!(base_url.as_deref(), Some("https://api.anthropic.com"));
            }
            other => panic!("anthropic 应走 Anthropic Messages，实际 {other:?}"),
        }
    }

    /// 带占位符的预设在用户替换前不算就绪，否则会打到字面量域名上。
    #[test]
    fn unresolved_template_placeholders_block_readiness() {
        let azure = provider_preset("azure_openai").unwrap();
        assert!(azure.needs_template());
        let problem =
            provider_readiness_error("azure_openai", &provider_cfg(azure.base_url, "gpt-5.5"));
        assert!(
            problem.is_some_and(|text| text.contains("占位符")),
            "未替换的 ${{RESOURCE_NAME}} 应拦下"
        );

        let resolved = provider_cfg("https://acme.openai.azure.com/openai/v1", "gpt-5.5");
        assert_eq!(provider_readiness_error("azure_openai", &resolved), None);
    }

    #[test]
    fn deepseek_v4_keeps_context_window_separate_from_output_limit() {
        let provider = hermes_config::ProviderConfig {
            base_url: "https://api.deepseek.com".into(),
            api_key: "secret".into(),
            model: "deepseek-v4-pro".into(),
            max_tokens: Some(1_000_000),
            temperature: Some(0.2),
            protocol: None,
        };
        assert_eq!(
            provider_max_output_tokens("deepseek", &provider),
            Some(393_216)
        );
        assert_eq!(effective_max_tokens("deepseek", &provider), Some(393_216));
    }

    #[tokio::test]
    async fn provider_profiles_can_be_saved_before_becoming_default() {
        let (_dir, state) = setup_state();
        let provider_name = format!("profile-test-{}", uuid::Uuid::new_v4());

        settings_save_provider(
            &state,
            ProviderSettingsInput {
                name: provider_name.clone(),
                base_url: "https://api.example.com/v1".into(),
                model: "test-model".into(),
                api_key: Some("sk-profile-test".into()),
                max_tokens: Some(2048),
                temperature: Some(0.2),
                protocol: None,
                activate: Some(false),
            },
        )
        .await
        .unwrap();

        let before = settings_get(&state).await.unwrap();
        assert_ne!(before["config"]["default_provider"], provider_name);
        assert_eq!(
            before["provider_status"][provider_name.as_str()]["ready"],
            true
        );

        settings_select_provider(&state, &provider_name)
            .await
            .unwrap();
        let after = settings_get(&state).await.unwrap();
        assert_eq!(after["config"]["default_provider"], provider_name);

        SettingsService::new(state.config_dir.clone())
            .set_provider_secret(&provider_name, "")
            .unwrap();
    }

    #[tokio::test]
    async fn saving_deepseek_context_as_output_is_rejected_before_request() {
        let (_dir, state) = setup_state();
        let error = settings_save_provider(
            &state,
            ProviderSettingsInput {
                name: "deepseek".into(),
                base_url: "https://api.deepseek.com".into(),
                model: "deepseek-v4-pro".into(),
                api_key: Some("sk-unused".into()),
                max_tokens: Some(1_000_000),
                temperature: Some(0.2),
                protocol: None,
                activate: Some(false),
            },
        )
        .await
        .unwrap_err();
        assert!(error.contains("393216"));
        assert!(error.contains("上下文窗口"));
    }

    #[tokio::test]
    async fn memory_set_then_get() {
        let (_dir, state) = setup_state();
        let workspace_path = scoped_test_workspace(&state).await;
        memory_set(&state, &workspace_path, "always run cargo test")
            .await
            .unwrap();
        let got = memory_get(&state, &workspace_path).await.unwrap();
        assert_eq!(got, "always run cargo test");
    }

    #[tokio::test]
    async fn change_diff_modify_shows_add_and_del() {
        let (_dir, state) = setup_state();
        let workspace_path = scoped_test_workspace(&state).await;
        let task = task_create(&state, Some(&workspace_path), "T", "g", "edit")
            .await
            .unwrap();

        let file_path = state.project_root.join("a.txt");
        std::fs::write(&file_path, "line1\nline2\nline3\n").unwrap();
        let cs = ChangeService::new(&state.db, state.blobs_dir.clone());
        cs.record_change(
            &task.id,
            &file_path,
            r_code_core::dto::FileChangeType::Modify,
            None,
            Some(b"line1\nline2\nline3\n"),
            Some(b"line1\nline-two\nline3\nline4\n"),
            None,
        )
        .await
        .unwrap();

        let diff = change_diff(&state, &task.id, "a.txt").await.unwrap();
        assert!(diff.supported);
        let lines = diff.lines.unwrap();
        assert!(lines
            .iter()
            .any(|l| l.kind == ChangeDiffLineKind::Del && l.text == "line2"));
        assert!(lines
            .iter()
            .any(|l| l.kind == ChangeDiffLineKind::Add && l.text == "line-two"));
        assert!(lines
            .iter()
            .any(|l| l.kind == ChangeDiffLineKind::Add && l.text == "line4"));
        assert!(lines
            .iter()
            .any(|l| l.kind == ChangeDiffLineKind::Ctx && l.text == "line1"));
    }

    #[tokio::test]
    async fn change_diff_unknown_path_errors() {
        let (_dir, state) = setup_state();
        let workspace_path = scoped_test_workspace(&state).await;
        let task = task_create(&state, Some(&workspace_path), "T", "g", "edit")
            .await
            .unwrap();
        let result = change_diff(&state, &task.id, "nope.txt").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn replay_recap_after_mock_run() {
        let (_dir, state) = setup_state();
        let task = task_create(&state, None, "T", "g", "ask").await.unwrap();
        agent_send(&state, &task.id, "hello").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(900)).await;

        let entries = replay(&state, &task.id, "recap").await.unwrap();
        assert!(!entries.is_empty());
        assert!(entries.iter().any(|e| e.event_type == "meta"));
    }

    #[tokio::test]
    async fn replay_invalid_depth_errors() {
        let (_dir, state) = setup_state();
        let result = replay(&state, "whatever", "deep").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn support_preview_returns_json() {
        let (_dir, state) = setup_state();
        let val = support_preview(&state).await.unwrap();
        assert!(val.get("version").is_some());
        assert!(val.get("db_stats").is_some());
    }

    #[tokio::test]
    async fn file_read_within_root() {
        let (_dir, state) = setup_state();
        let workspace_path = scoped_test_workspace(&state).await;
        let file_path = state.project_root.join("hello.txt");
        std::fs::write(&file_path, "line1\nline2\n").unwrap();

        let fc = file_read(&state, &workspace_path, "hello.txt")
            .await
            .unwrap();
        assert_eq!(fc.total_lines, 2);
        assert!(fc.content.contains("line2"));
        assert!(!fc.truncated);

        // 越界路径必须拒绝
        let escape = file_read(&state, &workspace_path, "../outside.txt").await;
        assert!(escape.is_err());
    }

    #[tokio::test]
    async fn file_tree_and_write_enforce_workspace_and_revision() {
        let (_dir, state) = setup_state();
        let workspace_path = scoped_test_workspace(&state).await;
        std::fs::create_dir_all(state.project_root.join("src")).unwrap();
        std::fs::create_dir_all(state.project_root.join("node_modules")).unwrap();
        std::fs::write(
            state.project_root.join("src/lib.rs"),
            "pub fn before() {}\n",
        )
        .unwrap();
        std::fs::write(state.project_root.join("node_modules/hidden.js"), "ignored").unwrap();

        let tree = file_list(&state, &workspace_path, None).await.unwrap();
        assert!(tree
            .entries
            .iter()
            .any(|entry| entry.path == "src" && entry.is_directory));
        assert!(!tree
            .entries
            .iter()
            .any(|entry| entry.path == "node_modules"));

        let original = file_read(&state, &workspace_path, "src/lib.rs")
            .await
            .unwrap();
        assert!(original.is_editable);
        let saved = file_write(
            &state,
            &workspace_path,
            "src/lib.rs",
            "pub fn after() {}\n",
            &original.revision,
        )
        .await
        .unwrap();
        assert_eq!(saved.content, "pub fn after() {}\n");
        assert_ne!(saved.revision, original.revision);
        assert_eq!(
            std::fs::read_to_string(state.project_root.join("src/lib.rs")).unwrap(),
            "pub fn after() {}\n"
        );

        let stale = file_write(
            &state,
            &workspace_path,
            "src/lib.rs",
            "pub fn stale() {}\n",
            &original.revision,
        )
        .await;
        assert!(stale.is_err());
    }

    #[tokio::test]
    async fn archive_rejects_running_tasks_and_blocks_new_messages() {
        let (_dir, state) = setup_state();
        let task = task_create(&state, None, "归档测试", "验证归档边界", "ask")
            .await
            .unwrap();

        TaskRepository::new(&state.db)
            .update_state(&task.id, TaskState::InProgress)
            .unwrap();
        assert!(task_archive(&state, &task.id).await.is_err());

        TaskRepository::new(&state.db)
            .update_state(&task.id, TaskState::Idle)
            .unwrap();
        let archived = task_archive(&state, &task.id).await.unwrap();
        assert_eq!(archived.state, TaskState::Archived);
        assert!(agent_send(&state, &task.id, "继续").await.is_err());
    }

    #[tokio::test]
    async fn verification_output_roundtrip() {
        let (_dir, state) = setup_state();
        let workspace_path = scoped_test_workspace(&state).await;
        let task = task_create(&state, Some(&workspace_path), "T", "g", "ask")
            .await
            .unwrap();

        // 跑一条真实命令（Windows: cmd /c echo）
        let rec = run_verification(&state, &task.id, "cmd /c echo hello-verify")
            .await
            .unwrap();
        let out = verification_output(&state, &rec.id).await.unwrap();
        assert!(
            out.contains("hello-verify"),
            "output should contain echoed text, got: {out}"
        );
    }

    #[test]
    fn diff_builder_context_collapse() {
        let before = (1..=20)
            .map(|i| format!("l{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut after_lines: Vec<String> = (1..=20).map(|i| format!("l{i}")).collect();
        after_lines[9] = "l10-changed".into();
        let (lines, truncated) = build_diff_lines(&before, &after_lines.join("\n"));
        assert!(!truncated);
        // 远处上下文被收敛为 hunk 行
        assert!(lines.iter().any(|l| l.kind == ChangeDiffLineKind::Hunk));
        assert!(lines
            .iter()
            .any(|l| l.kind == ChangeDiffLineKind::Del && l.text == "l10"));
        assert!(lines
            .iter()
            .any(|l| l.kind == ChangeDiffLineKind::Add && l.text == "l10-changed"));
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
        assert!(text.contains("\x1b[200~"));
        assert!(text.contains("\x1b[201~"));
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
