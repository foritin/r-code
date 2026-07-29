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

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use hermes_core::{CompletionRequest, Message, Role, SessionEvent, SessionMeta};
use hermes_store::SessionStore;
use r_code_agent_worker::{
    AgentRuntime, CodexSubagentEventSink, CodexSubagentOutcome, CodexSubagentRequest,
    CodexSubagentRunner, MockAgentRuntime, SteerResult,
};
use r_code_core::dto::{
    AgentActivityPhase, AgentEvent, AgentEventScope, AgentKind, AgentRun, AgentRunRuntimeKind,
    AgentSendMode, CreateSessionInput, FileChange, FileChangeType, Notification, NotificationKind,
    PermissionDecision, PermissionRequest, PlanStep, ProjectAccessMode, QueuedMessage,
    QueuedMessageState, ReviewState, RiskLevel, SessionBranch, SubagentState, Task, TaskEvent,
    TaskEventType, TaskMode, TaskState, ToolCall, VerificationRecord, Workspace,
};
use r_code_core::error::ProductError;
use r_code_core::secret::redact_text;
use r_code_core::security::PathGuard;
use r_code_gateway::permission::{PermissionCheckResult, PermissionEngine};
use r_code_store::repositories::VERIFICATION_PLACEHOLDER_MODEL;
use r_code_store::review::ReviewAction;
use r_code_store::{
    AgentRunRepository, BlobStore, ChangeService, Database, NotificationRepository,
    QueuedMessageRepository, ReviewService, SessionBranchRepository, TaskEventStore,
    TaskRepository, ToolCallRepository, VerificationConfig, VerificationService, WorkspaceService,
};
use r_code_terminal::{SendOptions, TerminalControlService, TerminalManager};
pub use r_code_terminal::{TerminalRawBatch, TerminalRawSnapshot};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::process::Command as TokioCommand;
use tokio::time::{timeout, Duration};
use tokio_util::sync::CancellationToken;

use crate::codex_mcp::{CodexMcpCallOutcome, CodexMcpRegistry};
use crate::codex_permissions::{CodexDelegationPermissions, CodexPermissionMode};
use crate::project_memory::ProjectMemory;
use crate::provider_catalog::{Preset as ProviderPreset, Protocol as ProviderProtocol};
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

/// 应用本次启动前遗留的活动记录。
///
/// 不能每次打开首页都扫描 `ended_at IS NULL`：那会把本进程刚启动的真实运行
/// 误报成“崩溃恢复”。因此只在 `CommandState` 建立时拍一张快照，后续恢复操作
/// 也只处理这份快照中的 run / 权限请求。
#[derive(Debug, Clone, Default)]
struct StartupRecoverySnapshot {
    runs: Vec<StartupRecoveryRun>,
    pending_permission_ids: Vec<String>,
}

#[derive(Debug, Clone)]
struct StartupRecoveryRun {
    run_id: String,
    task_id: String,
    branch_id: String,
}

/// 外部 CLI 子代理在当前进程内的取消注册表。
///
/// SQLite 是运行记录的真源；注册表只保留不可持久化的进程取消句柄。它从不包含
/// 命令行、提示词、认证信息或外部 Agent 的原始输出。
#[derive(Default)]
pub struct ExternalAgentRegistry {
    runs: tokio::sync::Mutex<HashMap<String, ExternalAgentHandle>>,
}

struct ExternalAgentHandle {
    task_id: String,
    parent_run_id: String,
    cancellation: CancellationToken,
}

impl ExternalAgentRegistry {
    const MAX_CODEX_EXEC_SUBAGENTS_PER_TASK: usize = 3;

    async fn reserve(
        &self,
        task_id: &str,
        parent_run_id: &str,
        run_id: &str,
    ) -> Result<CancellationToken, String> {
        let mut runs = self.runs.lock().await;
        let active_for_task = runs
            .values()
            .filter(|handle| handle.task_id == task_id)
            .count();
        if active_for_task >= Self::MAX_CODEX_EXEC_SUBAGENTS_PER_TASK {
            return Err(format!(
                "当前任务最多同时运行 {} 个 Codex 子代理",
                Self::MAX_CODEX_EXEC_SUBAGENTS_PER_TASK
            ));
        }
        let cancellation = CancellationToken::new();
        runs.insert(
            run_id.to_string(),
            ExternalAgentHandle {
                task_id: task_id.to_string(),
                parent_run_id: parent_run_id.to_string(),
                cancellation: cancellation.clone(),
            },
        );
        Ok(cancellation)
    }

    async fn remove(&self, run_id: &str) {
        self.runs.lock().await.remove(run_id);
    }

    async fn cancel_run_for_task(&self, task_id: &str, run_id: &str) -> bool {
        let token = self
            .runs
            .lock()
            .await
            .get(run_id)
            .filter(|handle| handle.task_id == task_id)
            .map(|handle| handle.cancellation.clone());
        if let Some(token) = token {
            token.cancel();
            true
        } else {
            false
        }
    }

    async fn cancel_task(&self, task_id: &str) -> usize {
        let tokens: Vec<CancellationToken> = self
            .runs
            .lock()
            .await
            .values()
            .filter(|handle| handle.task_id == task_id)
            .map(|handle| handle.cancellation.clone())
            .collect();
        for token in &tokens {
            token.cancel();
        }
        tokens.len()
    }

    async fn has_for_parent_run(&self, parent_run_id: &str) -> bool {
        self.runs
            .lock()
            .await
            .values()
            .any(|handle| handle.parent_run_id == parent_run_id)
    }
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
    /// 外部 CLI 子代理的可取消进程注册表。
    pub external_agents: Arc<ExternalAgentRegistry>,
    /// R-Code 作为 MCP client 连接 Codex 的长生命周期会话注册表。
    pub codex_mcp: Arc<CodexMcpRegistry>,
    /// 本次应用启动前遗留的 run / pending permission 快照。
    startup_recovery: Arc<Mutex<StartupRecoverySnapshot>>,
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
        // 在任何新的 Agent / MCP sidecar 有机会写入之前记录遗留项。查询失败不应
        // 阻断桌面启动，恢复页会安全地显示为空并保留数据库原状。
        let startup_recovery = capture_startup_recovery(&db).unwrap_or_default();
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
            external_agents: Arc::new(ExternalAgentRegistry::default()),
            codex_mcp: Arc::new(CodexMcpRegistry::default()),
            startup_recovery: Arc::new(Mutex::new(startup_recovery)),
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

    /// 为非 Tauri 宿主（MCP stdio server 等）启用真实 provider runtime。
    ///
    /// 调用方仍需自行确保所选 Provider 已在本机设置完成；这里绝不创建 mock
    /// 降级路径，避免外部编排器误以为得到了真实答复。
    pub async fn enable_real_agent_mode(&self) {
        self.agent.lock().await.enable_real_mode();
    }

    /// 向 WebView 广播 agent 事件（未注入出口时静默跳过，如测试环境）。
    pub fn emit_agent_event(&self, task_id: &str, event: &AgentEvent) {
        let sink = self.agent_event_sink.lock().unwrap().clone();
        if let Some(sink) = sink {
            sink(task_id, event);
        }
    }
}

fn capture_startup_recovery(db: &Database) -> Result<StartupRecoverySnapshot, ProductError> {
    let conn = db.conn()?;
    let mut run_stmt = conn
        .prepare(
            "SELECT ar.id, ar.task_id, ar.branch_id \
         FROM agent_runs ar \
         INNER JOIN tasks t ON t.id = ar.task_id \
         WHERE ar.ended_at IS NULL AND t.state NOT IN ('idle', 'archived') \
         ORDER BY ar.started_at ASC, ar.id ASC",
        )
        .map_err(|error| ProductError::DatabaseError(error.to_string()))?;
    let runs = run_stmt
        .query_map([], |row| {
            Ok(StartupRecoveryRun {
                run_id: row.get(0)?,
                task_id: row.get(1)?,
                branch_id: row.get(2)?,
            })
        })
        .map_err(|error| ProductError::DatabaseError(error.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| ProductError::DatabaseError(error.to_string()))?;
    drop(run_stmt);

    let mut permission_stmt = conn.prepare(
        "SELECT id FROM permission_requests WHERE decision = 'pending' ORDER BY created_at ASC, id ASC",
    )
    .map_err(|error| ProductError::DatabaseError(error.to_string()))?;
    let pending_permission_ids = permission_stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| ProductError::DatabaseError(error.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| ProductError::DatabaseError(error.to_string()))?;

    Ok(StartupRecoverySnapshot {
        runs,
        pending_permission_ids,
    })
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

/// 批量任务详情响应。
///
/// 保持每项与 `cmd_task_detail` 完全相同的形状，前端可以直接写入现有缓存；
/// 批量边界主要消除 WebView IPC 的 N+1 往返。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskDetailBatch {
    pub details: Vec<TaskDetail>,
}

/// 仪表盘中的文件变更摘要。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DashboardChangeSummary {
    pub files: u32,
    pub created: u32,
    pub modified: u32,
    pub removed: u32,
    pub renamed: u32,
}

/// 单个任务在项目仪表盘中的聚合视图。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardTaskSummary {
    pub task: Task,
    pub activity: String,
    pub agent_label: String,
    pub pending_permission_count: u32,
    pub active_run: Option<AgentRun>,
    pub change_summary: DashboardChangeSummary,
    pub latest_verification: Option<VerificationRecord>,
}

/// 项目级待处理项目的类型。
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DashboardAttentionKind {
    Permission,
    ReviewReady,
}

/// 项目仪表盘上直接可操作的一项待处理记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardAttentionItem {
    pub kind: DashboardAttentionKind,
    pub task: Task,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission: Option<PermissionRequest>,
    pub since: chrono::DateTime<chrono::Utc>,
}

/// 项目仪表盘的稳定统计口径。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WorkspaceDashboardMetrics {
    pub task_count: u32,
    pub pending_permission_count: u32,
    pub review_ready_count: u32,
    pub running_task_count: u32,
    pub active_subagent_count: u32,
    pub completed_last_hour_count: u32,
}

/// 一个项目的完整仪表盘数据源。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceDashboard {
    pub workspace: Workspace,
    pub generated_at: chrono::DateTime<chrono::Utc>,
    pub metrics: WorkspaceDashboardMetrics,
    pub tasks: Vec<DashboardTaskSummary>,
    pub attention: Vec<DashboardAttentionItem>,
    pub completed: Vec<DashboardTaskSummary>,
}

/// 可分页的项目 / 全局活动项。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectActivityItem {
    pub id: String,
    pub at: chrono::DateTime<chrono::Utc>,
    pub kind: TaskEventType,
    pub summary: String,
    pub task_id: String,
    pub task_title: String,
    pub workspace_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    pub metadata: serde_json::Value,
}

/// 活动流分页响应。`next_cursor` 是不透明字符串，调用方不得自行解析。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectActivityPage {
    pub items: Vec<ProjectActivityItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// 顶栏通知中心分页响应。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationPage {
    pub notifications: Vec<Notification>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub unread_count: u64,
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

/// 用户确认处理启动前遗留项后的结果。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RecoveryCleanupResult {
    /// 被以“中止”收尾的遗留 Agent Run 数量。
    pub runs_closed: u64,
    /// 因所有遗留运行均已收尾而标记为 Interrupted 的任务数量。
    pub tasks_interrupted: u64,
    /// 被拒绝的遗留权限请求数量。
    pub permissions_denied: u64,
    /// 被标记为 error 的遗留中的工具调用数量。
    pub tool_calls_closed: u64,
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

/// 手动上下文压缩结果。可见聊天记录不删除；`before_messages` / `after_messages`
/// 描述的是下一轮模型使用的工作集。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextCompactionResult {
    pub compacted: bool,
    pub before_messages: usize,
    pub after_messages: usize,
}

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

/// 修改会话显示名称。标题不参与模型上下文，因此无需重建 runtime。
pub async fn task_rename(state: &CommandState, task_id: &str, title: &str) -> Result<Task, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("请输入新的会话名称".to_string());
    }
    if title.chars().count() > 96 {
        return Err("会话名称不能超过 96 个字符".to_string());
    }
    let repo = TaskRepository::new(&state.db);
    let task = repo
        .get(task_id)
        .map_err(err_str)?
        .ok_or_else(|| format!("task not found: {task_id}"))?;
    if task.state == TaskState::Archived {
        return Err("会话已归档，不能再重命名".to_string());
    }
    repo.update(task_id, Some(title), None, None)
        .map_err(err_str)?;
    repo.get(task_id)
        .map_err(err_str)?
        .ok_or_else(|| format!("task not found after rename: {task_id}"))
}

/// 从当前分支末端创建一条新的活跃分支；源分支和完整 JSONL 保持只读。
pub async fn task_fork_context(
    state: &CommandState,
    task_id: &str,
) -> Result<SessionBranch, String> {
    let task = TaskRepository::new(&state.db)
        .get(task_id)
        .map_err(err_str)?
        .ok_or_else(|| format!("task not found: {task_id}"))?;
    if task.state == TaskState::Archived {
        return Err("会话已归档，不能创建分支".to_string());
    }
    {
        let bridge = state.agent.lock().await;
        if bridge
            .active
            .as_ref()
            .is_some_and(|active| active.task_id == task_id)
        {
            return Err("当前运行尚未结束，请先停止或等待完成后再创建分支".to_string());
        }
    }

    let source_branch = SessionBranchRepository::new(&state.db)
        .ensure_active(task_id)
        .map_err(err_str)?;
    ensure_session_log(
        &state.session_store,
        &state.sessions_dir,
        &source_branch.storage_id,
    )
    .await?;
    let source_path = session_file_path(&state.sessions_dir, &source_branch.storage_id);
    let source = tokio::fs::read_to_string(source_path)
        .await
        .map_err(err_str)?;
    let mut events = Vec::new();
    let mut forked_from_message_id = None;
    for (line_index, line) in source.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event = serde_json::from_str::<SessionEvent>(line)
            .map_err(|_| "会话历史存在无法恢复的记录".to_string())?;
        if matches!(event, SessionEvent::Message(_)) {
            forked_from_message_id =
                Some(format!("{}:{}", source_branch.storage_id, line_index + 1));
        }
        events.push(event);
    }
    let forked_from_message_id =
        forked_from_message_id.ok_or_else(|| "当前会话还没有可分支的消息".to_string())?;
    let branch = SessionBranch::fork(task_id, &source_branch.id, &forked_from_message_id);
    match events.first_mut() {
        Some(SessionEvent::Meta(meta)) => {
            meta.id = branch.storage_id.clone();
            meta.created_at = chrono::Utc::now();
        }
        _ => return Err("会话缺少元数据，无法创建分支".to_string()),
    }
    state
        .session_store
        .write_session_atomic(&branch.storage_id, &events)
        .await
        .map_err(err_str)?;
    SessionBranchRepository::new(&state.db)
        .create_fork(&branch)
        .map_err(err_str)?;
    TaskEventStore::new(&state.db)
        .append_for_branch(task_id, &branch.id, TaskEventType::SessionBranched)
        .map_err(err_str)?;

    let mut bridge = state.agent.lock().await;
    bridge.sessions.remove(task_id);
    Ok(branch)
}

const COMPACTION_KEEP_FIRST: usize = 1;
const COMPACTION_KEEP_RECENT: usize = 10;
const COMPACTION_MIN_MESSAGES: usize = COMPACTION_KEEP_FIRST + COMPACTION_KEEP_RECENT + 3;
const COMPACTION_SOURCE_CHARS: usize = 120_000;

fn trim_chars(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn compaction_source(messages: &[Message]) -> String {
    let mut source = String::new();
    for message in messages {
        let role = match message.role {
            Role::User => "USER",
            Role::Assistant => "ASSISTANT",
        };
        source.push_str(role);
        source.push_str(":\n");
        source.push_str(&message.text_content());
        if message
            .content
            .iter()
            .any(|block| block.is_tool_use() || block.is_tool_result())
        {
            source.push_str("\nSTRUCTURED_CONTENT: ");
            source.push_str(
                &serde_json::to_string(&message.content)
                    .unwrap_or_else(|_| "[无法序列化的工具内容]".to_string()),
            );
        }
        source.push_str("\n\n");
    }
    if source.chars().count() <= COMPACTION_SOURCE_CHARS {
        return source;
    }

    // 极长会话同时保留开头约定与靠近切分点的最新事实；按 char 截取避免切断 UTF-8。
    let head_chars = COMPACTION_SOURCE_CHARS / 3;
    let tail_chars = COMPACTION_SOURCE_CHARS - head_chars;
    let head = trim_chars(&source, head_chars);
    let tail = source
        .chars()
        .rev()
        .take(tail_chars)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("{head}\n\n[中间内容因长度受限已省略]\n\n{tail}")
}

fn compacted_working_set(history: &[Message], summary: &str) -> Vec<Message> {
    if history.len() < COMPACTION_MIN_MESSAGES {
        return history.to_vec();
    }
    let recent_start = history.len().saturating_sub(COMPACTION_KEEP_RECENT);
    let mut result = history[..COMPACTION_KEEP_FIRST].to_vec();
    result.push(Message::system_text(format!(
        "[R-Code 上下文摘要]\n{}",
        summary.trim()
    )));
    result.extend_from_slice(&history[recent_start..]);
    result
}

/// 压缩当前分支的模型工作集，同时保留完整可见聊天与审计记录。
pub async fn task_compact_context(
    state: &CommandState,
    task_id: &str,
    focus: Option<&str>,
) -> Result<ContextCompactionResult, String> {
    let task = TaskRepository::new(&state.db)
        .get(task_id)
        .map_err(err_str)?
        .ok_or_else(|| format!("task not found: {task_id}"))?;
    if task.state == TaskState::Archived {
        return Err("会话已归档，不能压缩上下文".to_string());
    }

    {
        let bridge = state.agent.lock().await;
        if bridge
            .active
            .as_ref()
            .is_some_and(|active| active.task_id == task_id)
        {
            return Err("当前运行尚未结束，请先停止或等待完成后再压缩上下文".to_string());
        }
    }

    let branch = SessionBranchRepository::new(&state.db)
        .ensure_active(task_id)
        .map_err(err_str)?;
    ensure_session_log(
        &state.session_store,
        &state.sessions_dir,
        &branch.storage_id,
    )
    .await?;
    let history = state
        .session_store
        .load(&branch.storage_id)
        .await
        .map_err(err_str)?;
    let before_messages = history.messages.len();
    if before_messages < COMPACTION_MIN_MESSAGES {
        return Ok(ContextCompactionResult {
            compacted: false,
            before_messages,
            after_messages: before_messages,
        });
    }

    let settings = SettingsService::new(state.config_dir.clone());
    let config = settings.load_global_unvalidated().map_err(err_str)?;
    let provider_name = task
        .provider_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(config.default_provider.as_str());
    let provider_config = config
        .providers
        .get(provider_name)
        .ok_or_else(|| format!("未找到模型服务“{provider_name}”，请前往设置完成配置"))?;
    if let Some(problem) = provider_readiness_error(provider_name, provider_config) {
        return Err(format!("模型服务“{provider_name}”尚未就绪：{problem}"));
    }
    let provider =
        hermes_llm::create_provider(build_provider_config(provider_name, provider_config))
            .map_err(err_str)?;

    let recent_start = before_messages.saturating_sub(COMPACTION_KEEP_RECENT);
    let to_summarize = &history.messages[COMPACTION_KEEP_FIRST..recent_start];
    let focus = focus
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| trim_chars(value, 2_000));
    let focus_line = focus
        .as_deref()
        .map(|value| format!("\n用户特别要求保留：{value}"))
        .unwrap_or_default();
    let prompt = format!(
        "把下面较早的会话压缩成一份能让另一个编码 Agent 无缝继续工作的中文摘要。\
必须保留：用户目标与约束、已经确认的决定、重要文件/符号、已做修改、命令与验证结果、错误及根因、未完成事项。\
删除寒暄、重复过程和无结论的尝试。不要编造，不要写开场白。{focus_line}\n\n{}",
        compaction_source(to_summarize)
    );
    let model = task
        .model
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(provider_config.model.as_str())
        .to_string();
    let response = provider
        .complete(CompletionRequest {
            model,
            system: Some("你是精确的会话上下文压缩器。只返回结构化摘要。".to_string()),
            messages: vec![Message::user_text(prompt)],
            tools: vec![],
            max_tokens: 4_096,
            temperature: Some(0.1),
            enable_caching: false,
        })
        .await
        .map_err(|error| format!("生成上下文摘要失败：{}", err_str(error)))?;
    let summary = response.text();
    if summary.trim().is_empty() {
        return Err("模型没有返回可用的上下文摘要".to_string());
    }
    let compacted = compacted_working_set(&history.messages, &summary);

    // 摘要生成期间可能有另一入口启动了运行；写快照前再次检查并持锁，避免新消息
    // 落在快照之后却被旧摘要覆盖。
    let mut bridge = state.agent.lock().await;
    if bridge
        .active
        .as_ref()
        .is_some_and(|active| active.task_id == task_id)
    {
        return Err("摘要生成期间会话开始了新的运行，本次未应用压缩".to_string());
    }
    let current_branch = SessionBranchRepository::new(&state.db)
        .ensure_active(task_id)
        .map_err(err_str)?;
    if current_branch.id != branch.id || current_branch.storage_id != branch.storage_id {
        return Err("摘要生成期间会话分支已变化，本次未应用压缩".to_string());
    }
    state
        .session_store
        .append(
            &branch.storage_id,
            SessionEvent::HistorySnapshot {
                messages: compacted.clone(),
            },
        )
        .await
        .map_err(err_str)?;
    state
        .session_store
        .append(
            &branch.storage_id,
            SessionEvent::System {
                event: "r_code_context_compacted".to_string(),
                data: serde_json::json!({
                    "before_messages": before_messages,
                    "after_messages": compacted.len(),
                }),
            },
        )
        .await
        .map_err(err_str)?;
    bridge.sessions.remove(task_id);
    drop(bridge);
    TaskEventStore::new(&state.db)
        .append_for_branch(task_id, &branch.id, TaskEventType::System)
        .map_err(err_str)?;

    Ok(ContextCompactionResult {
        compacted: true,
        before_messages,
        after_messages: compacted.len(),
    })
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

/// 永久删除一个已经停止的会话及其 R-Code 审计数据。
///
/// 工作区和其中的文件不属于会话存储，永远不会在这里删除。运行中的会话必须先停止，
/// 防止后台进程继续向已经被级联清理的记录写入。
pub async fn task_delete(state: &CommandState, task_id: &str) -> Result<(), String> {
    let repo = TaskRepository::new(&state.db);
    let task = repo
        .get(task_id)
        .map_err(err_str)?
        .ok_or_else(|| format!("task not found: {task_id}"))?;
    if matches!(task.state, TaskState::Exploring | TaskState::InProgress) {
        return Err("会话仍在运行，请先停止后删除".to_string());
    }

    let storage_ids = SessionBranchRepository::new(&state.db)
        .list_by_task(task_id)
        .map_err(err_str)?
        .into_iter()
        .map(|branch| branch.storage_id)
        .collect::<HashSet<_>>();

    let mut bridge = state.agent.lock().await;
    if bridge
        .active
        .as_ref()
        .is_some_and(|active| active.task_id == task_id)
    {
        return Err("会话仍在运行，请先停止后删除".to_string());
    }
    if !repo.delete(task_id).map_err(err_str)? {
        return Err(format!("task not found: {task_id}"));
    }
    bridge.sessions.remove(task_id);
    drop(bridge);

    // JSONL 不在 SQLite 事务中；数据库删除成功后做幂等的最佳努力清理。
    // 同时按 task 前缀覆盖旧主分支和外部子代理日志，绝不触碰工作区目录。
    if let Ok(entries) = std::fs::read_dir(&state.sessions_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = entry.file_name();
            let Some(file_name) = file_name.to_str() else {
                continue;
            };
            let storage_match = storage_ids
                .iter()
                .any(|storage_id| file_name == format!("{storage_id}.jsonl"));
            let task_prefix_match = file_name.ends_with(".jsonl")
                && (file_name == format!("{task_id}.jsonl")
                    || file_name.starts_with(&format!("{task_id}--")));
            if (storage_match || task_prefix_match) && path.is_file() {
                if let Err(error) = std::fs::remove_file(&path) {
                    tracing::warn!(task_id, file = %path.display(), %error, "failed to remove deleted task session log");
                }
            }
        }
    }
    Ok(())
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

/// 单次读取多个任务详情，供项目 / 活动页避免为每个任务走一次 WebView IPC。
pub async fn task_detail_batch(
    state: &CommandState,
    task_ids: &[String],
) -> Result<TaskDetailBatch, String> {
    const MAX_BATCH_SIZE: usize = 80;
    let mut unique_ids = Vec::new();
    for task_id in task_ids {
        if task_id.trim().is_empty() || unique_ids.iter().any(|known| known == task_id) {
            continue;
        }
        unique_ids.push(task_id.clone());
    }
    if unique_ids.len() > MAX_BATCH_SIZE {
        return Err(format!("一次最多读取 {MAX_BATCH_SIZE} 个任务详情"));
    }

    let mut details = Vec::with_capacity(unique_ids.len());
    for task_id in unique_ids {
        details.push(task_detail(state, &task_id).await?);
    }
    Ok(TaskDetailBatch { details })
}

fn display_task_title(task: &Task) -> String {
    let title = task.title.trim();
    if !title.is_empty() {
        return title.to_string();
    }
    let goal = task.goal.trim();
    if !goal.is_empty() {
        return goal.to_string();
    }
    "未命名任务".to_string()
}

fn summarize_changes(changes: &[FileChange]) -> DashboardChangeSummary {
    let mut summary = DashboardChangeSummary {
        files: changes.len() as u32,
        ..DashboardChangeSummary::default()
    };
    for change in changes {
        match change.change_type {
            FileChangeType::Create => summary.created += 1,
            FileChangeType::Modify => summary.modified += 1,
            FileChangeType::Delete => summary.removed += 1,
            FileChangeType::Rename => summary.renamed += 1,
        }
    }
    summary
}

fn is_live_dashboard_task(task: &Task, runs: &[AgentRun]) -> bool {
    matches!(task.state, TaskState::Exploring | TaskState::InProgress)
        || runs
            .iter()
            .any(|run| run.ended_at.is_none() && run.model != VERIFICATION_PLACEHOLDER_MODEL)
}

fn current_dashboard_run(runs: &[AgentRun]) -> Option<AgentRun> {
    runs.iter()
        .find(|run| run.ended_at.is_none() && run.model != VERIFICATION_PLACEHOLDER_MODEL)
        .cloned()
        .or_else(|| {
            runs.iter()
                .find(|run| {
                    run.agent_kind == AgentKind::Main && run.model != VERIFICATION_PLACEHOLDER_MODEL
                })
                .cloned()
        })
        .or_else(|| runs.first().cloned())
}

fn dashboard_agent_label(run: Option<&AgentRun>) -> String {
    match run {
        Some(run)
            if run
                .agent_label
                .as_deref()
                .is_some_and(|label| !label.trim().is_empty()) =>
        {
            run.agent_label.clone().unwrap_or_default()
        }
        Some(run) if run.agent_kind == AgentKind::Subagent => "子代理".to_string(),
        _ => "主代理".to_string(),
    }
}

fn dashboard_activity(
    task: &Task,
    permissions: &[PermissionRequest],
    run: Option<&AgentRun>,
) -> String {
    if let Some(permission) = permissions.first() {
        return format!("等待授权 · {}", permission.tool_name);
    }
    if let Some(summary) = run.and_then(|item| item.summary.as_deref()) {
        if !summary.trim().is_empty() {
            return summary.trim().to_string();
        }
    }
    match task.state {
        TaskState::Exploring => "梳理代码与执行路径".to_string(),
        TaskState::InProgress => "正在推进任务".to_string(),
        TaskState::ReviewReady => "变更已准备好审查".to_string(),
        TaskState::Interrupted => "任务已停止".to_string(),
        TaskState::Idle => "任务已完成".to_string(),
        TaskState::Archived => "会话已归档".to_string(),
    }
}

fn dashboard_task_rank(summary: &DashboardTaskSummary) -> u8 {
    if summary.pending_permission_count > 0 {
        0
    } else if summary.task.state == TaskState::ReviewReady {
        1
    } else if summary
        .active_run
        .as_ref()
        .is_some_and(|run| run.ended_at.is_none())
        || matches!(
            summary.task.state,
            TaskState::Exploring | TaskState::InProgress
        )
    {
        2
    } else if summary.task.state == TaskState::Interrupted {
        3
    } else if summary.task.state == TaskState::Idle {
        4
    } else {
        5
    }
}

/// 获取一个工作区的仪表盘聚合数据。
///
/// 所有统计口径在同一后端时刻计算，前端不再根据多轮轮询的详情缓存拼装数量。
pub async fn workspace_dashboard(
    state: &CommandState,
    workspace_path: &str,
) -> Result<WorkspaceDashboard, String> {
    let workspace = WorkspaceService::new(&state.db)
        .get(workspace_path)
        .map_err(err_str)?
        .ok_or_else(|| {
            "workspace is not open; choose the folder before viewing its dashboard".to_string()
        })?;
    let tasks = TaskRepository::new(&state.db)
        .list(Some(&workspace.canonical_path), None, false)
        .map_err(err_str)?;

    let now = chrono::Utc::now();
    let completed_since = now - chrono::Duration::hours(1);
    let runs = AgentRunRepository::new(&state.db);
    let change_service = ChangeService::new(&state.db, state.blobs_dir.clone());
    let verification_service = VerificationService::new(&state.db, state.blobs_dir.clone());
    let mut metrics = WorkspaceDashboardMetrics::default();
    let mut summaries = Vec::with_capacity(tasks.len());
    let mut attention = Vec::new();
    let mut completed = Vec::new();

    for task in tasks {
        metrics.task_count += 1;
        let task_runs = runs.list_by_task(&task.id).map_err(err_str)?;
        let active_run = current_dashboard_run(&task_runs);
        let pending_permissions = state.permission_engine.pending_for_task(&task.id).await;
        let changes = change_service
            .list_changes(&task.id)
            .await
            .map_err(err_str)?;
        let verifications = verification_service
            .list_for_task(&task.id)
            .await
            .map_err(err_str)?;
        let latest_verification = verifications
            .iter()
            .max_by_key(|record| record.ended_at.as_ref().unwrap_or(&record.started_at))
            .cloned();
        let live = is_live_dashboard_task(&task, &task_runs);

        metrics.pending_permission_count += pending_permissions.len() as u32;
        if task.state == TaskState::ReviewReady {
            metrics.review_ready_count += 1;
        }
        // 与旧 UI 保持一致：正在等用户授权的任务优先归入“待处理”，不重复记到运行中。
        if live && pending_permissions.is_empty() {
            metrics.running_task_count += 1;
        }
        metrics.active_subagent_count += task_runs
            .iter()
            .filter(|run| run.agent_kind == AgentKind::Subagent && run.ended_at.is_none())
            .count() as u32;
        if task.state == TaskState::Idle && task.updated_at >= completed_since {
            metrics.completed_last_hour_count += 1;
        }

        for permission in &pending_permissions {
            attention.push(DashboardAttentionItem {
                kind: DashboardAttentionKind::Permission,
                task: task.clone(),
                permission: Some(permission.clone()),
                since: permission.created_at,
            });
        }
        if task.state == TaskState::ReviewReady {
            attention.push(DashboardAttentionItem {
                kind: DashboardAttentionKind::ReviewReady,
                task: task.clone(),
                permission: None,
                since: task.updated_at,
            });
        }

        let summary = DashboardTaskSummary {
            activity: dashboard_activity(&task, &pending_permissions, active_run.as_ref()),
            agent_label: dashboard_agent_label(active_run.as_ref()),
            pending_permission_count: pending_permissions.len() as u32,
            change_summary: summarize_changes(&changes),
            latest_verification,
            active_run,
            task,
        };
        if summary.task.state == TaskState::Idle && summary.task.updated_at >= completed_since {
            completed.push(summary.clone());
        }
        summaries.push(summary);
    }

    summaries.sort_by(|left, right| {
        dashboard_task_rank(left)
            .cmp(&dashboard_task_rank(right))
            .then_with(|| right.task.updated_at.cmp(&left.task.updated_at))
    });
    attention.sort_by(|left, right| left.since.cmp(&right.since));
    completed.sort_by(|left, right| right.task.updated_at.cmp(&left.task.updated_at));
    completed.truncate(6);

    Ok(WorkspaceDashboard {
        workspace,
        generated_at: now,
        metrics,
        tasks: summaries,
        attention,
        completed,
    })
}

fn parse_page_cursor(cursor: Option<&str>, label: &str) -> Result<Option<i64>, String> {
    let Some(cursor) = cursor.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let parsed = cursor
        .parse::<i64>()
        .map_err(|_| format!("invalid {label} cursor"))?;
    if parsed < 0 {
        return Err(format!("invalid {label} cursor"));
    }
    Ok(Some(parsed))
}

fn run_for_activity_event(runs: &[AgentRun], event: &TaskEvent) -> Option<AgentRun> {
    runs.iter()
        .filter(|run| run.branch_id == event.branch_id && run.started_at <= event.created_at)
        .max_by_key(|run| run.started_at)
        .cloned()
        .or_else(|| {
            runs.iter()
                .find(|run| run.branch_id == event.branch_id)
                .cloned()
        })
        .or_else(|| runs.first().cloned())
}

fn activity_presentation(event: &TaskEvent, run: Option<&AgentRun>) -> (String, String) {
    let agent = dashboard_agent_label(run);
    match event.event_type {
        TaskEventType::TaskCreated => ("创建了任务".to_string(), "你".to_string()),
        TaskEventType::StateChanged => ("更新了任务状态".to_string(), agent),
        TaskEventType::RunStarted => ("开始执行".to_string(), agent),
        TaskEventType::RunEnded => ("完成了一次执行".to_string(), agent),
        TaskEventType::UserSteered => ("补充了执行指令".to_string(), "你".to_string()),
        TaskEventType::UserMessageQueued => ("将消息加入队列".to_string(), "你".to_string()),
        TaskEventType::QueueDispatched => ("发送了队列消息".to_string(), "系统".to_string()),
        TaskEventType::RunAborted => ("停止了执行".to_string(), "你".to_string()),
        TaskEventType::SessionBranched => ("创建了会话分支".to_string(), "你".to_string()),
        TaskEventType::SubagentStarted => ("启动了子代理".to_string(), "子代理".to_string()),
        TaskEventType::SubagentFinished => ("子代理已完成".to_string(), "子代理".to_string()),
        TaskEventType::ToolCall => ("调用了工具".to_string(), agent),
        TaskEventType::ToolResult => ("收到了工具结果".to_string(), agent),
        TaskEventType::PermissionRequested => ("请求了权限".to_string(), agent),
        TaskEventType::PermissionDecided => ("完成了权限裁决".to_string(), "你".to_string()),
        TaskEventType::FileChanged => ("修改了文件".to_string(), agent),
        TaskEventType::VerificationRun => ("运行了验证".to_string(), agent),
        TaskEventType::ChangeRequested => ("请求继续修改".to_string(), "你".to_string()),
        TaskEventType::System => ("更新了系统记录".to_string(), "系统".to_string()),
    }
}

fn build_activity_page(
    state: &CommandState,
    events: Vec<TaskEvent>,
) -> Result<ProjectActivityPage, String> {
    let next_cursor = events.last().map(|event| event.id.to_string());
    let tasks = TaskRepository::new(&state.db);
    let runs = AgentRunRepository::new(&state.db);
    let mut task_cache: HashMap<String, Task> = HashMap::new();
    let mut run_cache: HashMap<String, Vec<AgentRun>> = HashMap::new();
    let mut items = Vec::with_capacity(events.len());

    for event in events {
        let task = if let Some(task) = task_cache.get(&event.task_id) {
            task.clone()
        } else {
            let task = tasks
                .get(&event.task_id)
                .map_err(err_str)?
                .ok_or_else(|| format!("task missing for activity event: {}", event.task_id))?;
            task_cache.insert(event.task_id.clone(), task.clone());
            task
        };
        let task_runs = if let Some(cached) = run_cache.get(&event.task_id) {
            cached.clone()
        } else {
            let records = runs.list_by_task(&event.task_id).map_err(err_str)?;
            run_cache.insert(event.task_id.clone(), records.clone());
            records
        };
        let run = run_for_activity_event(&task_runs, &event);
        let (summary, actor) = activity_presentation(&event, run.as_ref());
        items.push(ProjectActivityItem {
            id: event.id.to_string(),
            at: event.created_at,
            kind: event.event_type,
            summary,
            task_id: task.id.clone(),
            task_title: display_task_title(&task),
            workspace_path: task.workspace_path.clone(),
            run_id: run.as_ref().map(|item| item.id.clone()),
            actor: Some(actor),
            metadata: serde_json::json!({
                "event_id": event.id,
                "branch_id": event.branch_id,
                "task_state": task.state.to_string(),
                "run_id": run.as_ref().map(|item| item.id.clone()),
            }),
        });
    }

    Ok(ProjectActivityPage { items, next_cursor })
}

/// 读取一个项目的真实、可分页活动流。
pub async fn project_activity_list(
    state: &CommandState,
    workspace_path: &str,
    cursor: Option<&str>,
    limit: u32,
) -> Result<ProjectActivityPage, String> {
    let workspace = WorkspaceService::new(&state.db)
        .get(workspace_path)
        .map_err(err_str)?
        .ok_or_else(|| {
            "workspace is not open; choose the folder before viewing its activity".to_string()
        })?;
    let cursor = parse_page_cursor(cursor, "project activity")?;
    let events = TaskEventStore::new(&state.db)
        .list_by_workspace_recent(&workspace.canonical_path, cursor, limit)
        .map_err(err_str)?;
    build_activity_page(state, events)
}

/// 读取跨项目的真实、可分页活动流。
pub async fn activity_list(
    state: &CommandState,
    cursor: Option<&str>,
    limit: u32,
) -> Result<ProjectActivityPage, String> {
    let cursor = parse_page_cursor(cursor, "activity")?;
    let events = TaskEventStore::new(&state.db)
        .list_recent(cursor, limit)
        .map_err(err_str)?;
    build_activity_page(state, events)
}

fn review_notification_source_key(task_id: &str, run: Option<&AgentRun>) -> String {
    format!(
        "review:{task_id}:{}",
        run.map(|record| record.id.as_str()).unwrap_or("task")
    )
}

async fn sync_notifications(state: &CommandState) -> Result<(), String> {
    let tasks = TaskRepository::new(&state.db)
        .list(None, None, false)
        .map_err(err_str)?;
    let notifications = NotificationRepository::new(&state.db);
    let runs = AgentRunRepository::new(&state.db);

    for task in tasks {
        for permission in state.permission_engine.pending_for_task(&task.id).await {
            let body = if permission.input_summary.trim().is_empty() {
                format!("{} 需要你的批准才能继续。", permission.tool_name)
            } else {
                permission.input_summary.clone()
            };
            let notification = Notification::new(
                NotificationKind::PermissionRequested,
                format!("需要授权：{}", display_task_title(&task)),
                body,
                Some(task.id.clone()),
                task.workspace_path.clone(),
            );
            notifications
                .upsert(&format!("permission:{}", permission.id), &notification)
                .map_err(err_str)?;
        }

        if task.state == TaskState::ReviewReady {
            let latest_run = runs.get_latest_main_run(&task.id).map_err(err_str)?;
            let source_key = review_notification_source_key(&task.id, latest_run.as_ref());
            notifications
                .mark_task_source_prefix_read(&task.id, "review:", Some(&source_key))
                .map_err(err_str)?;
            let notification = Notification::new(
                NotificationKind::ReviewReady,
                format!("等待审核：{}", display_task_title(&task)),
                "本轮变更已准备好验收。".to_string(),
                Some(task.id.clone()),
                task.workspace_path.clone(),
            );
            notifications
                .upsert(&source_key, &notification)
                .map_err(err_str)?;
        } else {
            notifications
                .mark_task_source_prefix_read(&task.id, "review:", None)
                .map_err(err_str)?;
        }
    }
    Ok(())
}

fn mark_current_review_notification_read(
    state: &CommandState,
    task_id: &str,
) -> Result<(), String> {
    let run = AgentRunRepository::new(&state.db)
        .get_latest_main_run(task_id)
        .map_err(err_str)?;
    NotificationRepository::new(&state.db)
        .mark_source_read(&review_notification_source_key(task_id, run.as_ref()))
        .map_err(err_str)
}

/// 读取通知中心；每次读取会先把当前待授权 / 待审查状态同步成幂等通知。
pub async fn notification_list(
    state: &CommandState,
    cursor: Option<&str>,
    limit: u32,
    unread_only: bool,
) -> Result<NotificationPage, String> {
    sync_notifications(state).await?;
    let cursor = parse_page_cursor(cursor, "notification")?;
    let notifications = NotificationRepository::new(&state.db);
    let rows = notifications
        .list(cursor, limit, unread_only)
        .map_err(err_str)?;
    let next_cursor = rows.last().map(|(sequence, _)| sequence.to_string());
    let records = rows
        .into_iter()
        .map(|(_, notification)| notification)
        .collect();
    Ok(NotificationPage {
        notifications: records,
        next_cursor,
        unread_count: notifications.unread_count().map_err(err_str)?,
    })
}

/// 标记一条通知已读。不存在的通知返回 false，便于前端处理已被清理的旧链接。
pub async fn notification_mark_read(
    state: &CommandState,
    notification_id: &str,
) -> Result<bool, String> {
    NotificationRepository::new(&state.db)
        .mark_read(notification_id)
        .map_err(err_str)
}

/// 标记全部通知已读，返回受影响数量。
pub async fn notification_mark_all_read(state: &CommandState) -> Result<u64, String> {
    NotificationRepository::new(&state.db)
        .mark_all_read()
        .map_err(err_str)
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
    )
    .with_codex_subagent_runner(Arc::new(RCodeCodexSubagentRunner {
        permission_engine: tool_gateway.permission_engine().clone(),
    }));

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
        scope
            .model
            .clone()
            .unwrap_or_else(|| "subagent".to_string()),
        scope.agent_label.clone(),
        scope.delegated_by_tool_call_id.clone(),
    );
    run.id = scope.run_id.clone();
    run.runtime_kind = scope.runtime_kind;
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
        AgentEvent::Activity { phase, detail } => {
            // 主运行活动是易失的 UI 状态；子代理活动需要进入其独立日志，才能在完成后
            // 仍然打开查看。这里只保存宿主已分类的阶段和受限说明，不保存推理正文。
            if let Some(scope) = scope {
                let _ = session_store
                    .append(
                        &event_storage_id,
                        SessionEvent::System {
                            event: "subagent_activity".into(),
                            data: serde_json::json!({
                                "run_id": scope.run_id.as_str(),
                                "phase": phase,
                                "detail": detail,
                            }),
                        },
                    )
                    .await;
            }
        }
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
                    state.external_agents.clone(),
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
        state.external_agents.clone(),
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
    external_agents: Arc<ExternalAgentRegistry>,
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
            // MCP stdio can run in a sibling process so that Codex owns its server lifetime.
            // The desktop process therefore cannot hold that runtime's cancellation token.  A
            // persisted Interrupted state is the cross-process cancellation handshake: the
            // owning drain loop observes it and aborts its own provider run promptly.
            let externally_interrupted = TaskRepository::new(&db)
                .get(&task_id)
                .ok()
                .flatten()
                .is_some_and(|task| task.state == TaskState::Interrupted);
            if externally_interrupted {
                let mut bridge = agent.lock().await;
                if bridge
                    .active
                    .as_ref()
                    .is_some_and(|current| current.run_id == active.run_id)
                    && !bridge.aborted()
                {
                    let _ = bridge.kind.abort(&active.runtime_session_id).await;
                }
            }
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
            if drained && !external_agents.has_for_parent_run(&active.run_id).await {
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

        dispatch_next_queued(
            agent,
            external_agents,
            db,
            sessions_dir,
            config_dir,
            tool_gateway,
            sink,
        )
        .await;
    });
}

/// 物理 runtime 空闲后，从所有任务的持久化队列中取出最高优先级消息并启动。
async fn dispatch_next_queued(
    agent: Arc<tokio::sync::Mutex<AgentBridge>>,
    external_agents: Arc<ExternalAgentRegistry>,
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
                    external_agents,
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
    // 外部 CLI 子代理不归内置 runtime 的 supervisor 管理，先向其发送取消信号。
    // 进程收尾仍由对应任务负责，避免过早把持久化运行记为结束。
    let _ = state.external_agents.cancel_task(task_id).await;
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
    if state
        .external_agents
        .cancel_run_for_task(task_id, subagent_id)
        .await
    {
        return Ok(());
    }
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
    let request = state.permission_engine.pending_by_id(request_id).await;
    state
        .permission_engine
        .decide(request_id, decision)
        .await
        .map_err(err_str)?;
    if let Some(request) = request {
        let branch = SessionBranchRepository::new(&state.db)
            .ensure_active(&request.task_id)
            .map_err(err_str)?;
        TaskEventStore::new(&state.db)
            .append_for_branch(
                &request.task_id,
                &branch.id,
                TaskEventType::PermissionDecided,
            )
            .map_err(err_str)?;
        NotificationRepository::new(&state.db)
            .mark_source_read(&format!("permission:{}", request.id))
            .map_err(err_str)?;
    }
    Ok(())
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

    mark_current_review_notification_read(state, task_id)?;

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
    mark_current_review_notification_read(state, task_id)?;
    Ok(())
}

/// 将审核反馈作为一个新的用户指令发送给任务，并保留明确的审计事件。
///
/// 该动作只允许发生在 `review_ready`：它不会悄悄接受或回滚现有改动，而是启动
/// 下一轮 Agent 运行，让用户的反馈和后续修改都留在同一会话历史中。
pub async fn change_request(
    state: &CommandState,
    task_id: &str,
    message: &str,
) -> Result<(), String> {
    const MAX_REVIEW_FEEDBACK_CHARS: usize = 8_000;
    let feedback = message.trim();
    if feedback.is_empty() {
        return Err("请说明希望修改的内容".to_string());
    }
    if feedback.chars().count() > MAX_REVIEW_FEEDBACK_CHARS {
        return Err(format!(
            "审核反馈不能超过 {MAX_REVIEW_FEEDBACK_CHARS} 个字符"
        ));
    }
    let task = TaskRepository::new(&state.db)
        .get(task_id)
        .map_err(err_str)?
        .ok_or_else(|| format!("task not found: {task_id}"))?;
    if task.state != TaskState::ReviewReady {
        return Err("只有等待审核的任务可以请求修改".to_string());
    }
    let branch = SessionBranchRepository::new(&state.db)
        .ensure_active(task_id)
        .map_err(err_str)?;
    let reviewed_run = AgentRunRepository::new(&state.db)
        .get_latest_main_run(task_id)
        .map_err(err_str)?;
    let reviewed_source = review_notification_source_key(task_id, reviewed_run.as_ref());
    let instruction =
        format!("审核反馈：\n{feedback}\n\n请根据以上反馈继续修改；完成后说明变更内容与验证结果。");

    agent_send_with_mode(state, task_id, &instruction, AgentSendMode::Auto).await?;
    TaskEventStore::new(&state.db)
        .append_for_branch(task_id, &branch.id, TaskEventType::ChangeRequested)
        .map_err(err_str)?;
    NotificationRepository::new(&state.db)
        .mark_source_read(&reviewed_source)
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

/// 在真实 PTY 中打开交互式 Codex CLI。CLI 路径由后端探测并作为进程参数传递，
/// 避免 Windows Store 同名别名抢占 npm CLI，也不让 WebView 拼接 shell 命令。
pub async fn terminal_create_codex(
    state: &CommandState,
    workspace_path: &str,
) -> Result<String, String> {
    let root = attached_workspace_root(state, workspace_path)?;
    let cli = probe_codex_cli().await;
    if !cli.available {
        return Err(cli
            .error
            .unwrap_or("未检测到可运行的 Codex CLI。")
            .to_string());
    }
    let executable = cli
        .path
        .ok_or_else(|| "无法定位 Codex CLI 可执行文件。".to_string())?;

    #[cfg(windows)]
    {
        let executable = executable
            .to_str()
            .ok_or_else(|| "Codex CLI 路径不是有效的 Unicode 文本。".to_string())?;
        state
            .terminal_manager
            .create_with_args(
                "cmd.exe",
                &root,
                Vec::new(),
                vec![
                    "/D".to_string(),
                    "/K".to_string(),
                    "call".to_string(),
                    executable.to_string(),
                ],
            )
            .await
            .map_err(err_str)
    }
    #[cfg(not(windows))]
    {
        state
            .terminal_manager
            .create(
                executable
                    .to_str()
                    .ok_or_else(|| "Codex CLI 路径不是有效的 Unicode 文本。".to_string())?,
                &root,
                Vec::new(),
            )
            .await
            .map_err(err_str)
    }
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

/// 读取原始终端快照，仅供桌面终端模拟器恢复 ANSI/cursor 状态。
/// Agent 工具必须继续使用 `terminal_read` 的 ANSI-free 文本结果。
pub async fn terminal_raw_snapshot(
    state: &CommandState,
    id: &str,
) -> Result<TerminalRawSnapshot, String> {
    let svc = TerminalControlService::new(state.terminal_manager.clone());
    svc.raw_snapshot(id).await.map_err(err_str)
}

/// 读取自前端游标以来的原始终端输出。
pub async fn terminal_raw_since(
    state: &CommandState,
    id: &str,
    cursor: u64,
) -> Result<TerminalRawBatch, String> {
    let svc = TerminalControlService::new(state.terminal_manager.clone());
    svc.raw_since(id, cursor).await.map_err(err_str)
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

/// 返回仍然存在的启动前遗留项，并把已经被正常收尾的记录从快照中移除。
///
/// 这里绝不重新扫描所有活跃记录：启动之后新建的运行不属于崩溃恢复范围。
fn startup_recovery_items(state: &CommandState) -> Result<StartupRecoverySnapshot, String> {
    let recorded = state
        .startup_recovery
        .lock()
        .map_err(|_| "恢复状态不可用".to_string())?
        .clone();
    if recorded.runs.is_empty() && recorded.pending_permission_ids.is_empty() {
        return Ok(recorded);
    }

    let conn = state.db.conn().map_err(err_str)?;
    let mut run_stmt = conn
        .prepare("SELECT EXISTS(SELECT 1 FROM agent_runs WHERE id = ?1 AND ended_at IS NULL)")
        .map_err(err_str)?;
    let mut runs = Vec::new();
    for run in recorded.runs {
        let is_still_active: i64 = run_stmt
            .query_row([&run.run_id], |row| row.get(0))
            .map_err(err_str)?;
        if is_still_active != 0 {
            runs.push(run);
        }
    }
    drop(run_stmt);

    let mut permission_stmt = conn
        .prepare("SELECT EXISTS(SELECT 1 FROM permission_requests WHERE id = ?1 AND decision = 'pending')")
        .map_err(err_str)?;
    let mut pending_permission_ids = Vec::new();
    for permission_id in recorded.pending_permission_ids {
        let is_still_pending: i64 = permission_stmt
            .query_row([&permission_id], |row| row.get(0))
            .map_err(err_str)?;
        if is_still_pending != 0 {
            pending_permission_ids.push(permission_id);
        }
    }

    let snapshot = StartupRecoverySnapshot {
        runs,
        pending_permission_ids,
    };
    *state
        .startup_recovery
        .lock()
        .map_err(|_| "恢复状态不可用".to_string())? = snapshot.clone();
    Ok(snapshot)
}

pub async fn recovery_data(state: &CommandState) -> Result<RecoveryPageData, String> {
    let snapshot = startup_recovery_items(state)?;
    let mut task_ids = HashSet::new();
    let interrupted_tasks = snapshot
        .runs
        .into_iter()
        .filter_map(|run| task_ids.insert(run.task_id.clone()).then_some(run.task_id))
        .collect();

    Ok(RecoveryPageData {
        interrupted_tasks,
        orphaned_permissions: snapshot.pending_permission_ids.len() as u64,
    })
}

/// 收束本次启动前遗留的执行，不触碰本进程新建的运行或新的权限请求。
pub async fn recovery_cleanup(state: &CommandState) -> Result<RecoveryCleanupResult, String> {
    let snapshot = startup_recovery_items(state)?;
    if snapshot.runs.is_empty() && snapshot.pending_permission_ids.is_empty() {
        return Ok(RecoveryCleanupResult::default());
    }

    let now = chrono::Utc::now().to_rfc3339();
    let run_summary = "应用在这次运行结束前已退出；已在下次启动时安全收束。";
    let tool_error = serde_json::json!({
        "error": "应用在工具调用完成前退出，已在下次启动时结束该调用。"
    })
    .to_string();
    let mut conn = state.db.conn().map_err(err_str)?;
    let tx = conn.transaction().map_err(err_str)?;
    let mut result = RecoveryCleanupResult::default();
    let mut closed_runs = Vec::new();

    for run in &snapshot.runs {
        let closed = tx
            .execute(
                "UPDATE agent_runs
                 SET review_state = CASE WHEN review_state = 'pending' THEN 'aborted' ELSE review_state END,
                     ended_at = COALESCE(ended_at, ?1),
                     summary = COALESCE(summary, ?2)
                 WHERE id = ?3 AND ended_at IS NULL",
                rusqlite::params![&now, run_summary, &run.run_id],
            )
            .map_err(err_str)?;
        if closed == 0 {
            continue;
        }
        result.runs_closed += closed as u64;
        closed_runs.push(run.clone());
        result.tool_calls_closed += tx
            .execute(
                "UPDATE tool_calls
                 SET status = 'error',
                     output_json = COALESCE(output_json, ?1),
                     ended_at = COALESCE(ended_at, ?2)
                 WHERE run_id = ?3 AND status = 'running'",
                rusqlite::params![&tool_error, &now, &run.run_id],
            )
            .map_err(err_str)? as u64;
    }

    let interrupted_state = TaskState::Interrupted.to_string();
    let mut interrupted_task_ids = HashSet::new();
    for task_id in closed_runs
        .iter()
        .map(|run| run.task_id.as_str())
        .collect::<HashSet<_>>()
    {
        let updated = tx
            .execute(
                "UPDATE tasks
                 SET state = ?1, updated_at = ?2
                 WHERE id = ?3
                   AND state NOT IN ('idle', 'archived', 'interrupted')
                   AND NOT EXISTS (
                       SELECT 1 FROM agent_runs AS live
                       WHERE live.task_id = tasks.id AND live.ended_at IS NULL
                   )",
                rusqlite::params![&interrupted_state, &now, task_id],
            )
            .map_err(err_str)?;
        if updated != 0 {
            result.tasks_interrupted += updated as u64;
            interrupted_task_ids.insert(task_id.to_string());
        }
    }

    let mut event_branches = HashSet::new();
    for run in &closed_runs {
        if interrupted_task_ids.contains(&run.task_id) {
            event_branches.insert((run.task_id.clone(), run.branch_id.clone()));
        }
    }
    for (task_id, branch_id) in event_branches {
        tx.execute(
            "INSERT INTO task_events (task_id, branch_id, event_type, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                task_id,
                branch_id,
                TaskEventType::RunAborted.to_string(),
                &now
            ],
        )
        .map_err(err_str)?;
        tx.execute(
            "INSERT INTO task_events (task_id, branch_id, event_type, created_at)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                task_id,
                branch_id,
                TaskEventType::RunEnded.to_string(),
                &now
            ],
        )
        .map_err(err_str)?;
    }

    for permission_id in &snapshot.pending_permission_ids {
        result.permissions_denied += tx
            .execute(
                "UPDATE permission_requests
                 SET decision = 'deny', decided_at = COALESCE(decided_at, ?1)
                 WHERE id = ?2 AND decision = 'pending'",
                rusqlite::params![&now, permission_id],
            )
            .map_err(err_str)? as u64;
    }

    tx.commit().map_err(err_str)?;
    *state
        .startup_recovery
        .lock()
        .map_err(|_| "恢复状态不可用".to_string())? = StartupRecoverySnapshot::default();
    Ok(result)
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
    Ok(parse_session_messages(
        &content,
        &branch.id,
        &branch.storage_id,
    ))
}

fn parse_session_messages(content: &str, branch_id: &str, storage_id: &str) -> Vec<SessionMessage> {
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
        let message_id = Some(format!("{}:{}", storage_id, line_index + 1));
        match event {
            SessionEvent::Meta(meta) => out.push(SessionMessage {
                id: message_id,
                branch_id: branch_id.to_string(),
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
                    branch_id: branch_id.to_string(),
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
                    branch_id: branch_id.to_string(),
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
                    branch_id: branch_id.to_string(),
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
                branch_id: branch_id.to_string(),
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
    out
}

/// 读取某个子代理的独立日志。先校验运行归属，避免使用任意 ID 探测其他任务文件。
pub async fn subagent_session_messages(
    state: &CommandState,
    task_id: &str,
    subagent_id: &str,
) -> Result<Vec<SessionMessage>, String> {
    let run = AgentRunRepository::new(&state.db)
        .get(subagent_id)
        .map_err(err_str)?
        .ok_or_else(|| "子代理运行不存在".to_string())?;
    if run.task_id != task_id || run.agent_kind != AgentKind::Subagent {
        return Err("子代理运行不属于当前任务".to_string());
    }
    let branch = SessionBranchRepository::new(&state.db)
        .list_by_task(task_id)
        .map_err(err_str)?
        .into_iter()
        .find(|branch| branch.id == run.branch_id)
        .ok_or_else(|| "子代理所属会话分支不存在".to_string())?;
    if branch.task_id != task_id {
        return Err("子代理所属会话分支不属于当前任务".to_string());
    }
    let storage_id = subagent_storage_id(&branch.storage_id, subagent_id);
    let path = session_file_path(&state.sessions_dir, &storage_id);
    let content = match tokio::fs::read_to_string(path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(err_str(error)),
    };
    Ok(parse_session_messages(&content, &branch.id, &storage_id))
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

/// 设置页发起的模型目录读取。`api_key` 只在本次请求内存中使用；留空时尝试读取
/// `name` 对应的已保存凭据或环境变量。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModelsInput {
    pub name: String,
    pub preset: Option<String>,
    pub base_url: String,
    pub api_key: Option<String>,
    pub protocol: String,
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

/// 读取服务端实时模型列表。显式传入的新密钥优先；留空时只从运行时设置视图读取
/// 该配置已有的 keychain / 环境变量密钥。响应和错误都不会包含密钥或响应正文。
pub async fn provider_models(
    state: &CommandState,
    input: ProviderModelsInput,
) -> Result<serde_json::Value, String> {
    use crate::provider_catalog::{AuthStyle, Protocol};

    let name = input.name.trim();
    let preset = input
        .preset
        .as_deref()
        .and_then(provider_preset)
        .or_else(|| provider_preset(name));
    let base_url = if input.base_url.trim().is_empty() {
        preset.map_or("", |item| item.base_url)
    } else {
        input.base_url.trim()
    };
    let protocol = Protocol::parse(&input.protocol)
        .ok_or_else(|| format!("未知的线路协议“{}”", input.protocol.trim()))?;
    let auth = preset.map_or_else(
        || {
            if protocol == Protocol::AnthropicMessages {
                AuthStyle::XApiKey
            } else {
                AuthStyle::Bearer
            }
        },
        |item| item.auth,
    );

    let supplied_key = input
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string);
    let api_key = if supplied_key.is_some() || name.is_empty() {
        supplied_key
    } else {
        let settings = SettingsService::new(state.config_dir.clone());
        settings
            .load_global_unvalidated()
            .map_err(err_str)?
            .providers
            .get(name)
            .map(|provider| provider.api_key.trim())
            .filter(|key| !key.is_empty())
            .map(str::to_string)
    };

    let response =
        crate::provider_models::discover_models(base_url, api_key.as_deref(), protocol, auth)
            .await?;
    serde_json::to_value(response).map_err(err_str)
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

const CODEX_CLI_PROBE_TIMEOUT: Duration = Duration::from_secs(4);
const CODEX_CLI_CATALOG_TIMEOUT: Duration = Duration::from_secs(15);
const CODEX_CLI_INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
const CODEX_CLI_INSTALL_COMMAND: &str = "npm install -g @openai/codex";
const CODEX_CLI_INSTALL_ARGS: &[&str] = &["install", "-g", "@openai/codex"];
static CODEX_CLI_INSTALL_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static CODEX_COLLAB_SETUP_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
static CODEX_PREFERENCES_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Codex CLI 的可用性。不要把 PATH 上一个同名文件的存在误认为 CLI 可运行：
/// Windows App 的受保护安装目录、陈旧 shim 和损坏的 npm 安装都会命中这种误判。
#[derive(Debug, Clone)]
struct CodexCliProbe {
    available: bool,
    path: Option<PathBuf>,
    version: Option<String>,
    error: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexAuthState {
    Authenticated,
    NotAuthenticated,
    Unknown,
}

impl CodexAuthState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Authenticated => "authenticated",
            Self::NotAuthenticated => "not_authenticated",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexSetupState {
    InstallCli,
    Login,
    Check,
    Configure,
    Ready,
}

impl CodexSetupState {
    fn from_components(
        cli_available: bool,
        auth_state: CodexAuthState,
        skill_status: &str,
        mcp_server_configured: bool,
    ) -> Self {
        if !cli_available {
            Self::InstallCli
        } else if auth_state == CodexAuthState::NotAuthenticated {
            Self::Login
        } else if auth_state == CodexAuthState::Unknown {
            Self::Check
        } else if skill_status != "up_to_date" || !mcp_server_configured {
            Self::Configure
        } else {
            Self::Ready
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::InstallCli => "install_cli",
            Self::Login => "login",
            Self::Check => "check",
            Self::Configure => "configure",
            Self::Ready => "ready",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CodexAuthProbe {
    state: CodexAuthState,
    method: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CodexReasoningOption {
    pub effort: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CodexModelOption {
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub default_reasoning_effort: String,
    pub supported_reasoning_efforts: Vec<CodexReasoningOption>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct CodexCliPreferences {
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub verbosity: Option<String>,
    /// 由 Codex config.toml 解析出的子代理权限预设。
    pub permission_mode: CodexPermissionMode,
    pub models: Vec<CodexModelOption>,
    pub config_path: String,
}

#[derive(Debug, Deserialize)]
struct CodexModelCatalogWire {
    models: Vec<CodexModelWire>,
}

#[derive(Debug, Deserialize)]
struct CodexModelWire {
    slug: String,
    display_name: String,
    description: String,
    default_reasoning_level: String,
    #[serde(default)]
    supported_reasoning_levels: Vec<CodexReasoningWire>,
    visibility: Option<String>,
    priority: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct CodexReasoningWire {
    effort: String,
    description: String,
}

#[derive(Debug)]
enum CodexCommandError {
    Launch(std::io::ErrorKind),
    Timeout,
}

fn executable_paths(candidates: &[&str]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Some(paths) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&paths) {
            push_executable_candidates(&mut found, &directory, candidates);
        }
    }
    found
}

fn push_executable_candidates(found: &mut Vec<PathBuf>, directory: &Path, names: &[&str]) {
    for name in names {
        let path = directory.join(name);
        if path.is_file() && !found.iter().any(|candidate| candidate == &path) {
            found.push(path);
        }
    }
}

fn codex_cli_names() -> &'static [&'static str] {
    if cfg!(windows) {
        // npm 安装通常留下 .cmd shim；只找 exe 会把正常的 npm CLI 误判为未安装。
        &["codex.exe", "codex.cmd", "codex.bat", "codex"]
    } else {
        &["codex"]
    }
}

fn codex_cli_paths() -> Vec<PathBuf> {
    let mut found = executable_paths(codex_cli_names());
    #[cfg(windows)]
    if let Some(app_data) = std::env::var_os("APPDATA") {
        // GUI 应用可能继承了尚未刷新的 PATH；npm 的默认用户级 prefix 仍可直接探测。
        let npm_prefix = Path::new(&app_data).join("npm");
        push_executable_candidates(&mut found, &npm_prefix, codex_cli_names());
    }
    found
}

fn npm_cli_paths() -> Vec<PathBuf> {
    if cfg!(windows) {
        executable_paths(&["npm.exe", "npm.cmd", "npm.bat", "npm"])
    } else {
        executable_paths(&["npm"])
    }
}

fn codex_home_dir_from(configured: Option<PathBuf>, home: Option<PathBuf>) -> PathBuf {
    configured.unwrap_or_else(|| home.unwrap_or_else(|| PathBuf::from(".")).join(".codex"))
}

fn codex_home_dir() -> PathBuf {
    let configured = std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    codex_home_dir_from(configured, dirs::home_dir())
}

/// 只运行由本模块声明的固定 Codex 参数，绝不把 WebView 文字拼到 shell 命令中。
async fn run_codex_cli_at(
    cli_path: &Path,
    args: &[&str],
) -> Result<std::process::Output, CodexCommandError> {
    run_codex_cli_at_with_timeout(cli_path, args, CODEX_CLI_PROBE_TIMEOUT).await
}

async fn run_codex_cli_at_with_timeout(
    cli_path: &Path,
    args: &[&str],
    deadline: Duration,
) -> Result<std::process::Output, CodexCommandError> {
    #[cfg(windows)]
    let mut command = if cli_path.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
    }) {
        // npm 安装通常是 .cmd shim。命令路径来自 PATH 且先拒绝 cmd 元字符，参数只
        // 来自本模块字面量；这样既能绕开 Windows Store 别名，也不接受 WebView 文本。
        debug_assert!(args.iter().all(|arg| {
            arg.chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        }));
        windows_cmd_safe_path(cli_path)
            .map_err(|_| CodexCommandError::Launch(std::io::ErrorKind::InvalidInput))?;
        let mut command = TokioCommand::new("cmd.exe");
        command
            .args(["/D", "/S", "/C", "call"])
            .arg(cli_path)
            .args(args);
        command
    } else {
        let mut command = TokioCommand::new(cli_path);
        command.args(args);
        command
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut command = TokioCommand::new(cli_path);
        command.args(args);
        command
    };

    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    match timeout(deadline, command.output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(CodexCommandError::Launch(error.kind())),
        Err(_) => Err(CodexCommandError::Timeout),
    }
}

/// 构造 npm 命令。调用方只能传入本模块声明的固定参数；WebView 不能提供包名、
/// registry、脚本或任意 shell 文本。
fn npm_command_at(npm_path: &Path, args: &[&str]) -> Result<TokioCommand, String> {
    #[cfg(windows)]
    if npm_path.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
    }) {
        windows_cmd_safe_path(npm_path)?;
        let mut command = TokioCommand::new("cmd.exe");
        command
            .args(["/D", "/S", "/C", "call"])
            .arg(npm_path)
            .args(args);
        return Ok(command);
    }

    let mut command = TokioCommand::new(npm_path);
    command.args(args);
    Ok(command)
}

async fn run_npm_at(
    npm_path: &Path,
    args: &[&str],
    deadline: Duration,
) -> Result<std::process::Output, CodexCommandError> {
    let mut command = npm_command_at(npm_path, args)
        .map_err(|_| CodexCommandError::Launch(std::io::ErrorKind::InvalidInput))?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    match timeout(deadline, command.output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(error)) => Err(CodexCommandError::Launch(error.kind())),
        Err(_) => Err(CodexCommandError::Timeout),
    }
}

fn first_nonempty_line(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(120).collect())
}

async fn probe_npm_cli() -> Option<PathBuf> {
    for path in npm_cli_paths() {
        if matches!(
            run_npm_at(&path, &["--version"], CODEX_CLI_PROBE_TIMEOUT).await,
            Ok(output) if output.status.success()
        ) {
            return Some(path);
        }
    }
    None
}

async fn npm_global_prefix(npm_path: &Path) -> Option<PathBuf> {
    let output = run_npm_at(npm_path, &["prefix", "-g"], CODEX_CLI_PROBE_TIMEOUT)
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let prefix = PathBuf::from(first_nonempty_line(&output.stdout)?);
    prefix.is_absolute().then_some(prefix)
}

async fn codex_cli_paths_with_npm_prefix() -> (Vec<PathBuf>, Option<PathBuf>) {
    let mut paths = codex_cli_paths();
    let npm_path = probe_npm_cli().await;
    if let Some(npm_path) = npm_path.as_deref() {
        if let Some(prefix) = npm_global_prefix(npm_path).await {
            let bin = if cfg!(windows) {
                prefix
            } else {
                prefix.join("bin")
            };
            push_executable_candidates(&mut paths, &bin, codex_cli_names());
        }
    }
    (paths, npm_path)
}

async fn probe_codex_cli() -> CodexCliProbe {
    let (paths, _) = codex_cli_paths_with_npm_prefix().await;
    if paths.is_empty() {
        return CodexCliProbe {
            available: false,
            path: None,
            version: None,
            error: Some("未检测到可运行的 Codex CLI。请先独立安装 Codex CLI。"),
        };
    }

    let mut permission_denied = false;
    let mut timed_out = false;
    for path in paths {
        match run_codex_cli_at(&path, &["--version"]).await {
            Ok(output) if output.status.success() => {
                return CodexCliProbe {
                    available: true,
                    path: Some(path),
                    version: first_nonempty_line(&output.stdout),
                    error: None,
                };
            }
            Ok(_) => {}
            Err(CodexCommandError::Launch(std::io::ErrorKind::PermissionDenied)) => {
                permission_denied = true;
            }
            Err(CodexCommandError::Timeout) => timed_out = true,
            Err(CodexCommandError::Launch(_)) => {}
        }
    }

    let error = if permission_denied {
        "只检测到无法从命令行启动的 Codex Desktop 受保护程序。请独立安装 Codex CLI；安装后刷新状态。"
    } else if timed_out {
        "Codex CLI 启动超时。请在系统终端运行 `codex doctor` 排查。"
    } else {
        "检测到 Codex 命令，但无法正常启动。请在系统终端运行 `codex --version` 排查。"
    };
    CodexCliProbe {
        available: false,
        path: None,
        version: None,
        error: Some(error),
    }
}

/// 解析 `codex login status` 的公开、人类可读状态。只归纳状态和登录方式，绝不把
/// stdout/stderr、账户名或凭据传回前端或写入日志。
fn parse_codex_login_status(success: bool, stdout: &[u8], stderr: &[u8]) -> CodexAuthProbe {
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(stdout),
        String::from_utf8_lossy(stderr)
    )
    .to_ascii_lowercase();
    let method = if text.contains("chatgpt") {
        Some("ChatGPT")
    } else if text.contains("api key") || text.contains("api-key") {
        Some("API Key")
    } else if text.contains("access token") {
        Some("访问令牌")
    } else {
        None
    };
    let explicitly_signed_out = [
        "not logged in",
        "not authenticated",
        "no active authentication",
        "no active login",
        "signed out",
    ]
    .iter()
    .any(|needle| text.contains(needle));
    let state = if explicitly_signed_out {
        CodexAuthState::NotAuthenticated
    } else if success {
        // `codex login status` 的公开契约是：存在有效登录时退出码为 0。
        // 输出文本用于识别认证方式，不应反过来把成功退出误判为 unknown。
        CodexAuthState::Authenticated
    } else {
        CodexAuthState::Unknown
    };
    CodexAuthProbe { state, method }
}

async fn probe_codex_login(cli_path: Option<&Path>) -> CodexAuthProbe {
    let Some(cli_path) = cli_path else {
        return CodexAuthProbe {
            state: CodexAuthState::Unknown,
            method: None,
        };
    };
    match run_codex_cli_at(cli_path, &["login", "status"]).await {
        Ok(output) => {
            parse_codex_login_status(output.status.success(), &output.stdout, &output.stderr)
        }
        Err(_) => CodexAuthProbe {
            state: CodexAuthState::Unknown,
            method: None,
        },
    }
}

/// 返回 Codex CLI 协作入口的状态。它不会读取、修改或回传认证令牌，也不把
/// `auth.json` 是否存在当作登录结论，因为 Codex 可将凭据保存到系统密钥库。
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
    let codex_dir = codex_home_dir();
    let config_path = codex_dir.join("config.toml");
    let auth_path = codex_dir.join("auth.json");
    let cli = probe_codex_cli().await;
    let npm_path = if cli.available {
        None
    } else {
        probe_npm_cli().await
    };
    let auth = if cli.available {
        probe_codex_login(cli.path.as_deref()).await
    } else {
        CodexAuthProbe {
            state: CodexAuthState::Unknown,
            method: None,
        }
    };
    let mcp_server_configured = if cli.available {
        codex_mcp_server_configured(cli.path.as_deref()).await
    } else {
        false
    };
    let setup_state = CodexSetupState::from_components(
        cli.available,
        auth.state,
        skill_status,
        mcp_server_configured,
    );
    Ok(serde_json::json!({
        "cli_available": cli.available,
        "cli_path": cli.path,
        "cli_version": cli.version,
        "cli_error": cli.error,
        "installer_available": npm_path.is_some(),
        "installer_command": CODEX_CLI_INSTALL_COMMAND,
        "installer_error": if npm_path.is_some() {
            None
        } else {
            Some("未检测到 npm。请先安装 Node.js，或在系统终端手动安装 Codex CLI。")
        },
        "config_path": config_path,
        "config_exists": config_path.exists(),
        "auth_path": auth_path,
        "authenticated": auth.state == CodexAuthState::Authenticated,
        "auth_status": auth.state.as_str(),
        "auth_method": auth.method,
        "skill_path": skill_path,
        "skill_status": skill_status,
        "mcp_server_configured": mcp_server_configured,
        "mcp_server_name": "r-code",
        "integration_ready": setup_state == CodexSetupState::Ready,
        "setup_state": setup_state.as_str(),
        "wire_api": "responses",
    }))
}

fn parse_codex_model_catalog(stdout: &[u8]) -> Result<Vec<CodexModelOption>, String> {
    const MAX_CATALOG_BYTES: usize = 4 * 1024 * 1024;
    if stdout.len() > MAX_CATALOG_BYTES {
        return Err("Codex 返回的模型目录过大，已停止读取。".to_string());
    }
    let wire: CodexModelCatalogWire =
        serde_json::from_slice(stdout).map_err(|_| "Codex 模型目录格式无法识别。".to_string())?;
    let mut models = wire
        .models
        .into_iter()
        .filter(|model| model.visibility.as_deref() == Some("list"))
        .filter_map(|model| {
            let slug = model.slug.trim().to_string();
            if slug.is_empty() {
                return None;
            }
            let mut seen = HashSet::new();
            let supported_reasoning_efforts = model
                .supported_reasoning_levels
                .into_iter()
                .filter_map(|option| {
                    let effort = option.effort.trim().to_string();
                    if effort.is_empty() || !seen.insert(effort.clone()) {
                        return None;
                    }
                    Some(CodexReasoningOption {
                        effort,
                        description: option.description.trim().to_string(),
                    })
                })
                .collect::<Vec<_>>();
            Some((
                model.priority.unwrap_or(u32::MAX),
                CodexModelOption {
                    display_name: if model.display_name.trim().is_empty() {
                        slug.clone()
                    } else {
                        model.display_name.trim().to_string()
                    },
                    slug,
                    description: model.description.trim().to_string(),
                    default_reasoning_effort: model.default_reasoning_level.trim().to_string(),
                    supported_reasoning_efforts,
                },
            ))
        })
        .collect::<Vec<_>>();
    models.sort_by(|(left_priority, left), (right_priority, right)| {
        left_priority
            .cmp(right_priority)
            .then_with(|| left.display_name.cmp(&right.display_name))
    });
    let models = models
        .into_iter()
        .map(|(_, model)| model)
        .take(32)
        .collect::<Vec<_>>();
    if models.is_empty() {
        return Err("Codex 当前没有返回可选模型。".to_string());
    }
    Ok(models)
}

async fn require_authenticated_codex_cli() -> Result<CodexCliProbe, String> {
    let cli = probe_codex_cli().await;
    if !cli.available {
        return Err(cli
            .error
            .unwrap_or("未检测到可运行的 Codex CLI。")
            .to_string());
    }
    match probe_codex_login(cli.path.as_deref()).await.state {
        CodexAuthState::Authenticated => Ok(cli),
        CodexAuthState::NotAuthenticated => {
            Err("Codex CLI 尚未登录，暂时不能读取运行偏好。".to_string())
        }
        CodexAuthState::Unknown => Err("暂时无法确认 Codex 登录状态。".to_string()),
    }
}

async fn load_codex_model_catalog(cli_path: &Path) -> Result<Vec<CodexModelOption>, String> {
    let output =
        run_codex_cli_at_with_timeout(cli_path, &["debug", "models"], CODEX_CLI_CATALOG_TIMEOUT)
            .await
            .map_err(|error| match error {
                CodexCommandError::Timeout => "读取 Codex 模型目录超时，请稍后重试。".to_string(),
                CodexCommandError::Launch(_) => "无法启动 Codex CLI 读取模型目录。".to_string(),
            })?;
    if !output.status.success() {
        return Err("Codex 未能读取模型目录，请确认登录状态后重试。".to_string());
    }
    parse_codex_model_catalog(&output.stdout)
}

fn codex_config_string(
    document: &toml_edit::DocumentMut,
    key: &str,
) -> Result<Option<String>, String> {
    match document.get(key) {
        None => Ok(None),
        Some(item) => item
            .as_str()
            .map(|value| Some(value.to_string()))
            .ok_or_else(|| format!("Codex 配置项 `{key}` 不是字符串，无法安全修改。")),
    }
}

fn read_codex_preference_values(
    config_path: &Path,
) -> Result<(String, Option<String>, Option<String>, Option<String>), String> {
    let source = match std::fs::read_to_string(config_path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("读取 Codex 配置失败：{error}")),
    };
    let document = source
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| format!("Codex config.toml 格式有误：{error}"))?;
    let model = codex_config_string(&document, "model")?;
    let reasoning_effort = codex_config_string(&document, "model_reasoning_effort")?;
    let verbosity = codex_config_string(&document, "model_verbosity")?;
    Ok((source, model, reasoning_effort, verbosity))
}

/// 读取当前 Codex 子代理权限。这里故意只接受 Codex 已知枚举；手写的未知值不会
/// 被拼进子进程参数，而会作为 `custom` 展示并安全降级为只读。
fn read_codex_delegation_permissions(
    config_path: &Path,
) -> Result<CodexDelegationPermissions, String> {
    let source = match std::fs::read_to_string(config_path) {
        Ok(source) => source,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("读取 Codex 配置失败：{error}")),
    };
    read_codex_delegation_permissions_from_source(&source)
}

fn read_codex_delegation_permissions_from_source(
    source: &str,
) -> Result<CodexDelegationPermissions, String> {
    let document = source
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| format!("Codex config.toml 格式有误：{error}"))?;
    let sandbox = codex_config_string(&document, "sandbox_mode")?;
    let approval_policy = codex_config_string(&document, "approval_policy")?;
    let approvals_reviewer = codex_config_string(&document, "approvals_reviewer")?;
    let permissions = CodexDelegationPermissions::from_config(
        sandbox.as_deref(),
        approval_policy.as_deref(),
        approvals_reviewer.as_deref(),
    );
    // Codex profile selection can resolve permission fields outside the top level. We neither
    // flatten nor overwrite it; show it as custom and retain our safe executable fallback.
    if document.get("default_permissions").is_some() {
        Ok(permissions.as_custom())
    } else {
        Ok(permissions)
    }
}

fn codex_preferences_payload(
    config_path: &Path,
    model: Option<String>,
    reasoning_effort: Option<String>,
    verbosity: Option<String>,
    permission_mode: CodexPermissionMode,
    models: Vec<CodexModelOption>,
) -> CodexCliPreferences {
    CodexCliPreferences {
        model,
        reasoning_effort,
        verbosity,
        permission_mode,
        models,
        config_path: config_path.to_string_lossy().to_string(),
    }
}

pub async fn codex_cli_preferences() -> Result<CodexCliPreferences, String> {
    let cli = require_authenticated_codex_cli().await?;
    let cli_path = cli
        .path
        .as_deref()
        .ok_or_else(|| "无法定位 Codex CLI。".to_string())?;
    let models = load_codex_model_catalog(cli_path).await?;
    let config_path = codex_home_dir().join("config.toml");
    let (_, model, reasoning_effort, verbosity) = read_codex_preference_values(&config_path)?;
    let permission_mode = read_codex_delegation_permissions(&config_path)?.mode();
    Ok(codex_preferences_payload(
        &config_path,
        model,
        reasoning_effort,
        verbosity,
        permission_mode,
        models,
    ))
}

fn normalize_codex_preference(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn validate_codex_preferences(
    model: Option<&str>,
    reasoning_effort: Option<&str>,
    verbosity: Option<&str>,
    current_model: Option<&str>,
    models: &[CodexModelOption],
) -> Result<(), String> {
    let selected_model = model.and_then(|slug| models.iter().find(|item| item.slug == slug));
    if let Some(model) = model {
        if selected_model.is_none() && Some(model) != current_model {
            return Err("所选模型不在 Codex 当前可用目录中，请刷新后重试。".to_string());
        }
    }
    if let Some(effort) = reasoning_effort {
        let supported = match selected_model {
            Some(model) => model
                .supported_reasoning_efforts
                .iter()
                .any(|option| option.effort == effort),
            None if model.is_none() => models.iter().any(|model| {
                model
                    .supported_reasoning_efforts
                    .iter()
                    .any(|option| option.effort == effort)
            }),
            None => matches!(
                effort,
                "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra"
            ),
        };
        if !supported {
            return Err("当前模型不支持所选推理强度。".to_string());
        }
    }
    if let Some(verbosity) = verbosity {
        if !matches!(verbosity, "low" | "medium" | "high") {
            return Err("回复详细度必须是 low、medium 或 high。".to_string());
        }
    }
    Ok(())
}

fn render_codex_preferences(
    source: &str,
    model: Option<&str>,
    reasoning_effort: Option<&str>,
    verbosity: Option<&str>,
) -> Result<String, String> {
    let mut document = source
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| format!("Codex config.toml 格式有误：{error}"))?;
    for (key, value) in [
        ("model", model),
        ("model_reasoning_effort", reasoning_effort),
        ("model_verbosity", verbosity),
    ] {
        if let Some(value) = value {
            document[key] = toml_edit::value(value);
        } else {
            document.remove(key);
        }
    }
    Ok(document.to_string())
}

/// 把一个设置页预设写入 Codex 的全局 config.toml。
///
/// `default_permissions` 是 Codex 的另一套 profile 选择机制，不能和直接的 sandbox
/// 设置混用。遇到该项时拒绝修改，而不是悄悄删除用户的自定义 profile。
fn render_codex_permission_mode(source: &str, mode: CodexPermissionMode) -> Result<String, String> {
    if mode == CodexPermissionMode::Custom {
        // 当前 UI 仅把它作为“保持手写 config.toml”的展示态；不能反向生成未知组合。
        return Ok(source.to_string());
    }
    let profile = CodexDelegationPermissions::from_mode(mode).ok_or_else(|| {
        "“自定义 config.toml”由你直接维护；请选择一个预设后再保存，或在 Codex 配置中修改。"
            .to_string()
    })?;
    let mut document = source
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| format!("Codex config.toml 格式有误：{error}"))?;
    if document.get("default_permissions").is_some() {
        return Err(
            "当前 Codex config.toml 使用了 default_permissions profile；请在该文件中调整权限，R-Code 不会覆盖它。"
                .to_string(),
        );
    }
    document["sandbox_mode"] = toml_edit::value(profile.sandbox().as_str());
    document["approval_policy"] = toml_edit::value(profile.approval_policy().as_str());
    document["approvals_reviewer"] = toml_edit::value(profile.approvals_reviewer().as_str());
    Ok(document.to_string())
}

pub async fn codex_save_cli_preferences(
    model: Option<&str>,
    reasoning_effort: Option<&str>,
    verbosity: Option<&str>,
    permission_mode: Option<&str>,
) -> Result<CodexCliPreferences, String> {
    let _guard = CODEX_PREFERENCES_LOCK.lock().await;
    let cli = require_authenticated_codex_cli().await?;
    let cli_path = cli
        .path
        .as_deref()
        .ok_or_else(|| "无法定位 Codex CLI。".to_string())?;
    let models = load_codex_model_catalog(cli_path).await?;
    let config_path = codex_home_dir().join("config.toml");
    let (source, current_model, _, _) = read_codex_preference_values(&config_path)?;
    let model = normalize_codex_preference(model);
    let reasoning_effort = normalize_codex_preference(reasoning_effort);
    let verbosity = normalize_codex_preference(verbosity);
    validate_codex_preferences(
        model.as_deref(),
        reasoning_effort.as_deref(),
        verbosity.as_deref(),
        current_model.as_deref(),
        &models,
    )?;
    let rendered = render_codex_preferences(
        &source,
        model.as_deref(),
        reasoning_effort.as_deref(),
        verbosity.as_deref(),
    )?;
    let permission_mode = match permission_mode {
        Some(value) => Some(
            CodexPermissionMode::parse(value)
                .ok_or_else(|| "Codex 子代理权限不是受支持的预设。".to_string())?,
        ),
        None => None,
    };
    let rendered = match permission_mode {
        Some(mode) => render_codex_permission_mode(&rendered, mode)?,
        None => rendered,
    };
    let parent = config_path
        .parent()
        .ok_or_else(|| "无法定位 Codex 配置目录。".to_string())?;
    std::fs::create_dir_all(parent).map_err(|error| format!("创建 Codex 配置目录失败：{error}"))?;
    std::fs::write(&config_path, rendered)
        .map_err(|error| format!("保存 Codex 配置失败：{error}"))?;
    let effective_permission_mode = read_codex_delegation_permissions(&config_path)?.mode();
    Ok(codex_preferences_payload(
        &config_path,
        model,
        reasoning_effort,
        verbosity,
        effective_permission_mode,
        models,
    ))
}

fn npm_install_failure_message(stderr: &[u8]) -> &'static str {
    let text = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    if ["eacces", "eperm", "permission denied", "access is denied"]
        .iter()
        .any(|needle| text.contains(needle))
    {
        "npm 没有写入全局安装目录的权限。R-Code 不会自动请求管理员权限；请在系统终端按你的 Node.js 安装方式处理权限后重试。"
    } else if [
        "enotfound",
        "econnreset",
        "etimedout",
        "network",
        "unable to get local issuer certificate",
    ]
    .iter()
    .any(|needle| text.contains(needle))
    {
        "npm 无法连接软件源。请检查网络、代理或 npm registry 后重试。"
    } else {
        "npm 未能安装 Codex CLI。请在系统终端运行 `npm install -g @openai/codex` 查看完整诊断。"
    }
}

/// 用户在前端确认后安装官方 Codex CLI npm 包。
///
/// 命令形状完全固定，不接收 WebView 参数，不自动提权，也不读取 npm/Codex 凭据。
/// 安装完成后重新执行真实 CLI 探测，并返回与设置页相同的脱敏状态。
pub async fn codex_install_cli() -> Result<serde_json::Value, String> {
    let _guard = CODEX_CLI_INSTALL_LOCK.lock().await;
    if probe_codex_cli().await.available {
        return codex_integration_status().await;
    }

    let npm_path = probe_npm_cli().await.ok_or_else(|| {
        "未检测到可运行的 npm。请先安装 Node.js，再运行 `npm install -g @openai/codex`。"
            .to_string()
    })?;
    let output = match run_npm_at(&npm_path, CODEX_CLI_INSTALL_ARGS, CODEX_CLI_INSTALL_TIMEOUT)
        .await
    {
        Ok(output) => output,
        Err(CodexCommandError::Timeout) => {
            return Err("安装超过 5 分钟仍未完成，已停止 npm 进程。请检查网络后重试。".to_string())
        }
        Err(CodexCommandError::Launch(std::io::ErrorKind::PermissionDenied)) => {
            return Err("系统拒绝启动 npm。请检查 Node.js 安装与当前用户权限。".to_string())
        }
        Err(CodexCommandError::Launch(_)) => {
            return Err("无法启动 npm。请确认 Node.js 安装完整后重试。".to_string())
        }
    };
    if !output.status.success() {
        return Err(npm_install_failure_message(&output.stderr).to_string());
    }

    let installed = probe_codex_cli().await;
    if !installed.available {
        return Err(
            "npm 已完成安装，但 R-Code 仍未找到可运行的 Codex CLI。请重启 R-Code；若仍不可用，请在系统终端运行 `codex --version`。"
                .to_string(),
        );
    }
    codex_integration_status().await
}

const CODEX_MCP_CONFIG_TIMEOUT: Duration = Duration::from_secs(12);
const CODEX_MCP_GET_MAX_BYTES: usize = 64 * 1024;
const CODEX_MCP_SERVER_NAME: &str = "r-code";
const CODEX_MCP_HOST_DIR: &str = "mcp-host";
const CODEX_MCP_HOST_PREFIX: &str = "r-code-mcp-host-";

#[derive(Debug, Deserialize)]
struct CodexMcpRegistrationWire {
    name: String,
    enabled: bool,
    transport: CodexMcpTransportWire,
}

#[derive(Debug, Deserialize)]
struct CodexMcpTransportWire {
    #[serde(rename = "type")]
    transport_type: String,
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CodexMcpRegistration {
    enabled: bool,
    command: PathBuf,
    args: Vec<String>,
}

impl CodexMcpRegistration {
    fn data_dir(&self) -> Option<PathBuf> {
        match self.args.as_slice() {
            [subcommand, flag, data_dir] if subcommand == "mcp-server" && flag == "--data-dir" => {
                Some(PathBuf::from(data_dir))
            }
            _ => None,
        }
    }

    fn is_managed_for(&self, data_dir: &Path) -> bool {
        let Some(file_name) = self.command.file_name().and_then(|value| value.to_str()) else {
            return false;
        };
        let Some(parent) = self.command.parent() else {
            return false;
        };
        file_name.starts_with(CODEX_MCP_HOST_PREFIX)
            && paths_equal(parent, &data_dir.join(CODEX_MCP_HOST_DIR))
    }
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    if cfg!(windows) {
        left.to_string_lossy()
            .replace('/', "\\")
            .eq_ignore_ascii_case(&right.to_string_lossy().replace('/', "\\"))
    } else {
        left == right
    }
}

fn parse_codex_mcp_registration(stdout: &[u8]) -> Result<CodexMcpRegistration, String> {
    if stdout.len() > CODEX_MCP_GET_MAX_BYTES {
        return Err("Codex MCP 配置响应过大，已停止读取。".to_string());
    }
    let wire: CodexMcpRegistrationWire =
        serde_json::from_slice(stdout).map_err(|_| "Codex MCP 配置格式无法识别。".to_string())?;
    if wire.name != CODEX_MCP_SERVER_NAME || wire.transport.transport_type != "stdio" {
        return Err("Codex 中的 r-code 条目不是受支持的本地 MCP 配置。".to_string());
    }
    let command = wire
        .transport
        .command
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| "Codex MCP 配置缺少启动命令。".to_string())?;
    Ok(CodexMcpRegistration {
        enabled: wire.enabled,
        command,
        args: wire.transport.args,
    })
}

async fn codex_mcp_registration(cli_path: &Path) -> Result<Option<CodexMcpRegistration>, String> {
    let output = run_codex_cli_at_with_timeout(
        cli_path,
        &["mcp", "get", CODEX_MCP_SERVER_NAME, "--json"],
        CODEX_MCP_CONFIG_TIMEOUT,
    )
    .await
    .map_err(|error| match error {
        CodexCommandError::Timeout => "读取 Codex MCP 配置超时。".to_string(),
        CodexCommandError::Launch(_) => "无法启动 Codex CLI 读取 MCP 配置。".to_string(),
    })?;
    if !output.status.success() {
        return Ok(None);
    }
    parse_codex_mcp_registration(&output.stdout).map(Some)
}

fn file_fingerprint(path: &Path) -> Result<String, String> {
    let mut file =
        std::fs::File::open(path).map_err(|error| format!("读取 R-Code MCP 主机失败：{error}"))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("读取 R-Code MCP 主机失败：{error}"))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn managed_codex_mcp_file_name(source: &Path, fingerprint: &str) -> String {
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{value}"))
        .unwrap_or_default();
    format!("{CODEX_MCP_HOST_PREFIX}{fingerprint}{extension}")
}

fn deploy_codex_mcp_host(source: &Path, data_dir: &Path) -> Result<PathBuf, String> {
    let fingerprint = file_fingerprint(source)?;
    let host_dir = data_dir.join(CODEX_MCP_HOST_DIR);
    std::fs::create_dir_all(&host_dir)
        .map_err(|error| format!("创建 R-Code MCP 主机目录失败：{error}"))?;
    let target = host_dir.join(managed_codex_mcp_file_name(source, &fingerprint));
    if target.is_file() {
        if file_fingerprint(&target)? == fingerprint {
            return Ok(target);
        }
        return Err("R-Code MCP 主机副本校验失败，请删除损坏的副本后重试。".to_string());
    }

    let temporary = host_dir.join(format!(
        ".{}-{}.tmp",
        target
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("r-code-mcp-host"),
        std::process::id()
    ));
    std::fs::copy(source, &temporary)
        .map_err(|error| format!("部署 R-Code MCP 主机失败：{error}"))?;
    if let Err(error) = std::fs::rename(&temporary, &target) {
        let _ = std::fs::remove_file(&temporary);
        if target.is_file() && file_fingerprint(&target)? == fingerprint {
            return Ok(target);
        }
        return Err(format!("启用 R-Code MCP 主机失败：{error}"));
    }
    Ok(target)
}

fn registration_uses_current_host(
    registration: &CodexMcpRegistration,
    current_executable: &Path,
) -> bool {
    let Some(data_dir) = registration.data_dir() else {
        return false;
    };
    if !registration.enabled
        || !registration.is_managed_for(&data_dir)
        || !registration.command.is_file()
    {
        return false;
    }
    let Ok(fingerprint) = file_fingerprint(current_executable) else {
        return false;
    };
    let expected = managed_codex_mcp_file_name(current_executable, &fingerprint);
    if registration
        .command
        .file_name()
        .and_then(|value| value.to_str())
        == Some(expected.as_str())
    {
        return true;
    }
    file_fingerprint(&registration.command).is_ok_and(|configured| configured == fingerprint)
}

fn registration_is_owned_by_r_code(
    registration: &CodexMcpRegistration,
    current_executable: &Path,
    data_dir: &Path,
) -> bool {
    registration
        .data_dir()
        .is_some_and(|configured| paths_equal(&configured, data_dir))
        && (registration.is_managed_for(data_dir)
            || paths_equal(&registration.command, current_executable)
            || registration
                .command
                .file_stem()
                .and_then(|value| value.to_str())
                .is_some_and(|value| value.eq_ignore_ascii_case("r-code-host")))
}

fn cleanup_old_codex_mcp_hosts(data_dir: &Path, current: &Path) {
    let host_dir = data_dir.join(CODEX_MCP_HOST_DIR);
    let Ok(entries) = std::fs::read_dir(host_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let managed = path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.starts_with(CODEX_MCP_HOST_PREFIX));
        if managed && !paths_equal(&path, current) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// 只有内容与当前 R-Code 构建一致、且位于应用数据目录中的独立副本才算就绪。
/// 旧版直接指向 `target/debug/r-code-host.exe` 的配置会被判定为待迁移，避免 Codex
/// 长驻进程锁住 Cargo 下一次需要覆盖的热编译产物。
async fn codex_mcp_server_configured(cli_path: Option<&Path>) -> bool {
    let Some(cli_path) = cli_path else {
        return false;
    };
    let Ok(Some(registration)) = codex_mcp_registration(cli_path).await else {
        return false;
    };
    let Ok(current_executable) = std::env::current_exe() else {
        return false;
    };
    registration_uses_current_host(&registration, &current_executable)
}

/// 在用户从设置页明确点击后，将本机 R-Code MCP server 加入 Codex 配置。
///
/// 这是一个可见、可逆的外部配置写入：它只写入 `r-code` MCP 条目，不会读取/复制
/// Codex 凭据，也不会替换用户其它 MCP server。Codex 运行应用数据目录中的内容寻址
/// 副本，不直接运行 Cargo / 安装器需要覆盖的主程序，因此长驻连接不会阻塞重编译或更新。
pub async fn codex_install_mcp_server(state: &CommandState) -> Result<(), String> {
    let cli = probe_codex_cli().await;
    if !cli.available {
        return Err(cli
            .error
            .unwrap_or("未检测到可运行的 Codex CLI。")
            .to_string());
    }
    let current_executable = std::env::current_exe()
        .map_err(|_| "无法定位当前 R-Code 可执行文件，无法配置 MCP 服务。".to_string())?;
    let data_dir = state
        .config_dir
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "无法定位 R-Code 应用数据目录，无法配置 MCP 服务。".to_string())?;
    let cli_path = cli
        .path
        .ok_or_else(|| "无法定位 Codex CLI，无法配置 MCP 服务。".to_string())?;
    let executable = deploy_codex_mcp_host(&current_executable, &data_dir)?;
    if let Some(existing) = codex_mcp_registration(&cli_path).await? {
        if registration_uses_current_host(&existing, &current_executable) {
            return Ok(());
        }
        if !registration_is_owned_by_r_code(&existing, &current_executable, &data_dir) {
            return Err(
                "Codex 中已存在一个不是由当前 R-Code 管理的 r-code MCP 条目；为避免覆盖，请先手动检查该配置。"
                    .to_string(),
            );
        }
        let removed = run_codex_mcp_remove(&cli_path).await?;
        if !removed.status.success() {
            return Err("Codex 未能移除旧的 R-Code MCP 配置，请稍后重试。".to_string());
        }
    }
    let output = run_codex_mcp_add(Some(cli_path.clone()), &executable, &data_dir).await?;
    if !output.status.success() {
        return Err(
            "Codex 未能写入 R-Code MCP 配置。请在系统终端运行 `codex mcp add` 后重试。".to_string(),
        );
    }
    let refreshed = codex_mcp_registration(&cli_path).await?;
    if !refreshed.as_ref().is_some_and(|registration| {
        registration_uses_current_host(registration, &current_executable)
    }) {
        return Err("Codex 已写入配置，但校验未通过；请重新配置 MCP。".to_string());
    }
    cleanup_old_codex_mcp_hosts(&data_dir, &executable);
    Ok(())
}

async fn run_codex_mcp_remove(cli_path: &Path) -> Result<std::process::Output, String> {
    run_codex_cli_at_with_timeout(
        cli_path,
        &["mcp", "remove", CODEX_MCP_SERVER_NAME],
        CODEX_MCP_CONFIG_TIMEOUT,
    )
    .await
    .map_err(|error| match error {
        CodexCommandError::Timeout => "移除旧的 Codex MCP 配置超时。".to_string(),
        CodexCommandError::Launch(_) => "无法启动 Codex CLI 更新 MCP 配置。".to_string(),
    })
}

/// 运行固定形状的 `codex mcp add`。动态路径仅来自本进程的 `current_exe` 和已创建的
/// 应用数据目录；Windows .cmd shim 需要 cmd 解析时，会先拒绝 cmd 元字符，避免把路径
/// 变成 shell 语法。
async fn run_codex_mcp_add(
    cli_path: Option<PathBuf>,
    executable: &Path,
    data_dir: &Path,
) -> Result<std::process::Output, String> {
    #[cfg(windows)]
    let mut command = match cli_path {
        Some(path)
            if path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("exe")) =>
        {
            let mut command = TokioCommand::new(path);
            command
                .args(["mcp", "add", "r-code", "--"])
                .arg(executable)
                .args(["mcp-server", "--data-dir"])
                .arg(data_dir);
            command
        }
        path => {
            let cli = path.unwrap_or_else(|| PathBuf::from("codex"));
            windows_cmd_safe_path(&cli)?;
            windows_cmd_safe_path(executable)?;
            windows_cmd_safe_path(data_dir)?;
            let mut command = TokioCommand::new("cmd.exe");
            command
                .args(["/D", "/S", "/C", "call"])
                .arg(cli)
                .args(["mcp", "add", "r-code", "--"])
                .arg(executable)
                .args(["mcp-server", "--data-dir"])
                .arg(data_dir);
            command
        }
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut command = TokioCommand::new(cli_path.unwrap_or_else(|| PathBuf::from("codex")));
        command
            .args(["mcp", "add", "r-code", "--"])
            .arg(executable)
            .args(["mcp-server", "--data-dir"])
            .arg(data_dir);
        command
    };

    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    match timeout(CODEX_MCP_CONFIG_TIMEOUT, command.output()).await {
        Ok(Ok(output)) => Ok(output),
        Ok(Err(_)) => Err("无法启动 Codex MCP 配置命令。".to_string()),
        Err(_) => Err("Codex MCP 配置命令超时。".to_string()),
    }
}

#[cfg(windows)]
fn windows_cmd_safe_path(path: &Path) -> Result<String, String> {
    let text = path
        .to_str()
        .ok_or_else(|| "命令路径不是有效的 Unicode 文本。".to_string())?;
    if text.chars().any(|character| {
        matches!(
            character,
            '\0' | '\r' | '\n' | '"' | '&' | '|' | '<' | '>' | '^' | '%' | '!'
        )
    }) {
        return Err("命令路径包含 cmd 不支持的字符；请改用不含特殊字符的安装目录。".to_string());
    }
    Ok(format!("\"{text}\""))
}

#[derive(Debug, Clone, Copy)]
enum CodexLoginMode {
    Browser,
    DeviceCode,
}

impl CodexLoginMode {
    fn args(self) -> &'static [&'static str] {
        match self {
            Self::Browser => &["login"],
            Self::DeviceCode => &["login", "--device-auth"],
        }
    }
}

/// 在用户可见的系统终端中启动 Codex 登录。它不接收任何来自 WebView 的命令文本，
/// 也不读取登录输出或 auth.json；OAuth 交互完全由 Codex CLI 处理。
async fn codex_start_login_with_mode(mode: CodexLoginMode) -> Result<(), String> {
    let cli = probe_codex_cli().await;
    if !cli.available {
        return Err(cli
            .error
            .unwrap_or("未检测到可运行的 Codex CLI。")
            .to_string());
    }

    #[cfg(windows)]
    {
        let executable = cli.path.unwrap_or_else(|| PathBuf::from("codex"));
        windows_cmd_safe_path(&executable)?;
        // `start` 显式创建可见窗口，避免 Tauri 的 GUI 子进程吞掉交互式登录提示。
        let mut command = Command::new("cmd.exe");
        command
            .args(["/D", "/C", "start", "", "cmd.exe", "/D", "/K", "call"])
            .arg(executable)
            .args(mode.args());
        command
            .spawn()
            .map_err(|_| "无法启动 Codex 登录终端。请在系统终端运行 `codex login`。".to_string())?;
    }
    #[cfg(not(windows))]
    {
        let executable = cli.path.unwrap_or_else(|| PathBuf::from("codex"));
        let mut command = Command::new(executable);
        command.args(mode.args());
        command
            .spawn()
            .map_err(|_| "无法启动 Codex 登录。请在系统终端运行 `codex login`。".to_string())?;
    }
    Ok(())
}

pub async fn codex_start_login() -> Result<(), String> {
    codex_start_login_with_mode(CodexLoginMode::Browser).await
}

/// 适合远程桌面、无浏览器回调或 localhost callback 被拦截的设备码登录。
pub async fn codex_start_device_login() -> Result<(), String> {
    codex_start_login_with_mode(CodexLoginMode::DeviceCode).await
}

const CODEX_EXEC_MAX_GOAL_CHARS: usize = 12_000;
const CODEX_EXEC_MAX_SUMMARY_CHARS: usize = 8_000;
const CODEX_EXEC_MAX_LIFECYCLE_DETAIL_CHARS: usize = 320;
const CODEX_EXEC_IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const CODEX_EXEC_HARD_TIMEOUT: Duration = Duration::from_secs(30 * 60);

#[derive(Debug, Clone, Copy)]
struct CodexExecLimits {
    idle_timeout: Duration,
    hard_timeout: Duration,
}

impl Default for CodexExecLimits {
    fn default() -> Self {
        Self {
            idle_timeout: CODEX_EXEC_IDLE_TIMEOUT,
            hard_timeout: CODEX_EXEC_HARD_TIMEOUT,
        }
    }
}

/// Codex `--json` 输出里可安全进入 R-Code 状态机的事件。
///
/// reasoning 只映射为阶段，不保留正文；命令/MCP 只保留经脱敏、截断后的动作标签，
/// 原始输出永不进入 UI 或持久化存储。
#[derive(Debug, Clone, PartialEq, Eq)]
enum CodexExecJsonEvent {
    ThreadStarted(String),
    Activity {
        phase: AgentActivityPhase,
        detail: String,
    },
    ToolStarted {
        call_id: String,
        name: String,
        summary: String,
    },
    ToolCompleted {
        call_id: String,
        is_error: bool,
    },
    AssistantMessage(String),
    Usage(String),
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexExecFailure {
    Launch,
    ApprovalBridge,
    Stream,
    Reported,
    IdleTimeout,
    Deadline,
    ExitStatus,
}

#[derive(Debug, Default)]
struct CodexExecCompletion {
    cancelled: bool,
    succeeded: bool,
    thread_id: Option<String>,
    summary: Option<String>,
    usage_json: Option<String>,
    failure: Option<CodexExecFailure>,
}

/// Codex CLI backend exposed to the native R-Code agent's `delegate_task` tool.
///
/// Tool orchestration and lifecycle events stay in `r-code-agent-worker`; this adapter only owns
/// official CLI discovery, authentication gating and the configured-permission child process.
struct RCodeCodexSubagentRunner {
    permission_engine: Arc<PermissionEngine>,
}

#[async_trait::async_trait]
impl CodexSubagentRunner for RCodeCodexSubagentRunner {
    async fn run(
        &self,
        request: CodexSubagentRequest,
    ) -> Result<CodexSubagentOutcome, ProductError> {
        let CodexSubagentRequest {
            workspace,
            goal,
            task_id,
            run_id,
            caller,
            abort,
            event_sink,
        } = request;
        let goal = bounded_text(&goal, CODEX_EXEC_MAX_GOAL_CHARS);
        if goal.is_empty() || goal.contains('\0') {
            return Err(ProductError::Other(
                "Codex 子代理需要一项有效的委派任务".to_string(),
            ));
        }
        let workspace = PathGuard::new(workspace)?.root().to_path_buf();
        let permissions = read_codex_delegation_permissions(&codex_home_dir().join("config.toml"))
            .map_err(ProductError::Other)?;
        let cli = probe_codex_cli().await;
        if !cli.available {
            return Err(ProductError::Other(
                cli.error
                    .unwrap_or("未检测到可运行的 Codex CLI。请先在设置中完成安装。")
                    .to_string(),
            ));
        }
        match probe_codex_login(cli.path.as_deref()).await.state {
            CodexAuthState::Authenticated => {}
            CodexAuthState::NotAuthenticated => {
                return Err(ProductError::Other(
                    "Codex CLI 尚未登录。请先在设置中完成浏览器登录或设备码登录。".to_string(),
                ))
            }
            CodexAuthState::Unknown => {
                return Err(ProductError::Other(
                    "暂时无法确认 Codex CLI 登录状态，请在设置中刷新状态后重试。".to_string(),
                ))
            }
        }

        let cancellation = CancellationToken::new();
        let cancellation_monitor = {
            let cancellation = cancellation.clone();
            tokio::spawn(async move {
                while !abort.load(Ordering::Relaxed) {
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                cancellation.cancel();
            })
        };
        let completion = run_codex_delegation_process(
            &workspace,
            &build_codex_delegation_prompt(&goal, permissions),
            cli.path,
            cancellation,
            None,
            Some(&event_sink),
            permissions,
            CodexAppServerApprovalContext {
                permission_engine: self.permission_engine.clone(),
                task_id,
                run_id,
                caller,
            },
        )
        .await;
        cancellation_monitor.abort();

        if completion.cancelled {
            return Ok(CodexSubagentOutcome::Cancelled);
        }
        if !completion.succeeded {
            return Err(ProductError::Other(codex_exec_failure_message(
                completion.failure,
            )));
        }
        Ok(CodexSubagentOutcome::Completed(
            completion
                .summary
                .unwrap_or_else(|| "Codex CLI 已完成，但没有返回可显示的摘要。".to_string()),
        ))
    }
}

fn bounded_text(value: &str, max_chars: usize) -> String {
    let mut chars = value.trim().chars();
    let mut bounded: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        bounded.push('…');
    }
    bounded
}

fn sanitize_codex_usage(usage: &serde_json::Value) -> Option<String> {
    let source = usage.as_object()?;
    let mut safe = serde_json::Map::new();
    for key in [
        "input_tokens",
        "cached_input_tokens",
        "output_tokens",
        "reasoning_output_tokens",
    ] {
        if let Some(value) = source.get(key).and_then(serde_json::Value::as_u64) {
            safe.insert(key.to_string(), serde_json::Value::from(value));
        }
    }
    (!safe.is_empty()).then(|| serde_json::Value::Object(safe).to_string())
}

fn safe_codex_action(value: &str, fallback: &str) -> String {
    let redacted = redact_text(value);
    let normalized = redacted.split_whitespace().collect::<Vec<_>>().join(" ");
    let bounded = bounded_text(&normalized, 180);
    if bounded.is_empty() {
        fallback.to_string()
    } else {
        bounded
    }
}

fn codex_item_tool(item: &serde_json::Value) -> Option<(String, String, String)> {
    let item_type = item.get("type")?.as_str()?;
    let call_id = item
        .get("id")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(|value| bounded_text(value, 160))?;
    let (name, summary) = match item_type {
        "command_execution" => {
            let command = item
                .get("command")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("读取工作区");
            ("Codex 命令", safe_codex_action(command, "读取工作区"))
        }
        "mcp_tool_call" => {
            let server = item
                .get("server")
                .or_else(|| item.get("server_name"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("MCP");
            let tool = item
                .get("tool")
                .or_else(|| item.get("tool_name"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("工具");
            (
                "Codex MCP",
                safe_codex_action(&format!("{server} · {tool}"), "MCP 工具"),
            )
        }
        "web_search" => {
            let query = item
                .get("query")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("搜索资料");
            ("Codex 搜索", safe_codex_action(query, "搜索资料"))
        }
        _ => return None,
    };
    Some((call_id, name.to_string(), summary))
}

fn parse_codex_exec_json_line(line: &str) -> Option<CodexExecJsonEvent> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    match value.get("type")?.as_str()? {
        "thread.started" => value
            .get("thread_id")
            .and_then(serde_json::Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map(|id| CodexExecJsonEvent::ThreadStarted(bounded_text(id, 160))),
        "turn.started" => Some(CodexExecJsonEvent::Activity {
            phase: AgentActivityPhase::Requesting,
            detail: "Codex CLI 正在分析任务".to_string(),
        }),
        "item.started" => {
            let item = value.get("item")?;
            if item.get("type").and_then(serde_json::Value::as_str) == Some("reasoning") {
                return Some(CodexExecJsonEvent::Activity {
                    phase: AgentActivityPhase::Requesting,
                    detail: "Codex CLI 正在分析工作区".to_string(),
                });
            }
            codex_item_tool(item).map(|(call_id, name, summary)| CodexExecJsonEvent::ToolStarted {
                call_id,
                name,
                summary,
            })
        }
        "item.completed" => {
            let item = value.get("item")?;
            match item.get("type")?.as_str()? {
                "agent_message" => item
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .map(|text| {
                        CodexExecJsonEvent::AssistantMessage(bounded_text(
                            text,
                            CODEX_EXEC_MAX_SUMMARY_CHARS,
                        ))
                    })
                    .filter(|event| {
                        !matches!(event, CodexExecJsonEvent::AssistantMessage(text) if text.is_empty())
                    }),
                "reasoning" => Some(CodexExecJsonEvent::Activity {
                    phase: AgentActivityPhase::Finalizing,
                    detail: "Codex CLI 已完成一轮分析".to_string(),
                }),
                _ => codex_item_tool(item).map(|(call_id, _, _)| {
                    let is_error = item
                        .get("status")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|status| {
                            matches!(status, "failed" | "error" | "cancelled")
                        });
                    CodexExecJsonEvent::ToolCompleted { call_id, is_error }
                }),
            }
        }
        "turn.completed" => value
            .get("usage")
            .and_then(sanitize_codex_usage)
            .map(CodexExecJsonEvent::Usage),
        "turn.failed" | "error" => Some(CodexExecJsonEvent::Failed),
        _ => None,
    }
}

fn codex_exec_failure_message(failure: Option<CodexExecFailure>) -> String {
    match failure {
        Some(CodexExecFailure::IdleTimeout) => {
            "Codex CLI 连续 5 分钟没有返回任何进度，R-Code 已自动停止该子代理。请缩小任务范围后重试。"
                .to_string()
        }
        Some(CodexExecFailure::Deadline) => {
            "Codex CLI 已达到 30 分钟运行上限，R-Code 已自动停止该子代理。请拆分任务后重试。"
                .to_string()
        }
        Some(CodexExecFailure::Launch) => {
            "Codex CLI 子代理未能启动。请在设置中刷新安装与登录状态后重试。".to_string()
        }
        Some(CodexExecFailure::ApprovalBridge) => {
            "Codex 的审批桥未能建立。请升级本机 Codex CLI，或在设置中改用“替我审批”或“完全访问权限”。"
                .to_string()
        }
        Some(CodexExecFailure::Stream) => {
            "Codex CLI 的进度通道意外中断，请重试。".to_string()
        }
        Some(CodexExecFailure::Reported | CodexExecFailure::ExitStatus) | None => {
            "Codex CLI 子代理未能完成。请在设置中刷新 Codex 状态后重试。".to_string()
        }
    }
}

fn build_codex_delegation_prompt(goal: &str, permissions: CodexDelegationPermissions) -> String {
    let capability = match permissions.mode() {
        CodexPermissionMode::ReadOnly => {
            "Your configured permission profile is read-only. Do not edit files, do not create commits, and do not change configuration."
        }
        CodexPermissionMode::RequestApproval => {
            "Your configured permission profile can edit the attached workspace after required approvals. Request only the minimum additional permissions needed for the assignment."
        }
        CodexPermissionMode::AutoReview => {
            "Your configured permission profile can edit the attached workspace and uses Codex auto-review for additional permissions. Use only the minimum access needed for the assignment."
        }
        CodexPermissionMode::FullAccess => {
            "Your configured permission profile has full access. Stay within the attached workspace unless the assignment explicitly requires otherwise, and use only the minimum access needed."
        }
        CodexPermissionMode::Custom => {
            "Your configured permission profile comes from the user's custom Codex config.toml. Respect its sandbox and approval policy, and use only the minimum access needed."
        }
    };
    format!(
        "You are a delegated subagent inside R-Code. {capability} \
Do not create commits, do not alter global Codex or R-Code configuration, and do not start more agents. \
Return a concise factual summary for the parent agent. Do not expose private chain-of-thought.\n\nAssignment:\n{goal}"
    )
}

fn external_agent_scope(run: &AgentRun) -> AgentEventScope {
    AgentEventScope {
        run_id: run.id.clone(),
        agent_id: run.id.clone(),
        parent_run_id: run.parent_run_id.clone(),
        agent_kind: run.agent_kind,
        agent_label: run.agent_label.clone(),
        delegated_by_tool_call_id: run.delegated_by_tool_call_id.clone(),
        runtime_kind: run.runtime_kind,
        model: Some(run.model.clone()),
    }
}

fn scoped_external_event(run: &AgentRun, event: AgentEvent) -> AgentEvent {
    AgentEvent::Scoped {
        scope: external_agent_scope(run),
        event: Box::new(event),
    }
}

struct CodexExecObserver<'a> {
    db: &'a Database,
    session_store: &'a SessionStore,
    sessions_dir: &'a Path,
    parent_storage_id: &'a str,
    run: &'a AgentRun,
    sink: &'a Option<AgentEventSink>,
}

async fn persist_and_emit_external_event(
    db: &Database,
    session_store: &SessionStore,
    sessions_dir: &Path,
    parent_storage_id: &str,
    run: &AgentRun,
    event: AgentEvent,
    sink: &Option<AgentEventSink>,
) {
    let mut pending_text = HashMap::new();
    let parent_run_id = run.parent_run_id.as_deref().unwrap_or(&run.id);
    persist_runtime_event(
        db,
        session_store,
        sessions_dir,
        &run.task_id,
        &run.branch_id,
        parent_run_id,
        parent_storage_id,
        &event,
        &mut pending_text,
    )
    .await;
    if let Some(sink) = sink {
        sink(&run.task_id, &event);
    }
}

/// 将已探测到的 Codex CLI 转为可安全承载位置参数的子进程。
///
/// Windows 的 npm 安装暴露的是 `codex.cmd`。Tokio 向 `codex exec -` 关闭 stdin 时，
/// Node CLI 有时不会收到 EOF，因而会在没有任何 JSONL 进度的状态下永久等待。对于
/// 该受信任的 npm shim，直接执行其固定的 `node_modules/@openai/codex/bin/codex.js`
/// 入口，既绕开 cmd 的输入管道问题，也能把任务作为普通 argv 传入而不是 shell 文本。
#[cfg(windows)]
fn codex_npm_node_command(cli_path: &Path) -> Result<TokioCommand, String> {
    let Some(directory) = cli_path.parent() else {
        return Err("Codex npm 命令路径无效，无法定位运行时。".to_string());
    };
    let entrypoint = directory
        .join("node_modules")
        .join("@openai")
        .join("codex")
        .join("bin")
        .join("codex.js");
    if !entrypoint.is_file() {
        return Err(
            "检测到 Codex .cmd 命令，但未找到其 npm 运行入口。请在设置中重新安装 Codex CLI。"
                .to_string(),
        );
    }
    let local_node = directory.join("node.exe");
    let node = if local_node.is_file() {
        local_node
    } else {
        executable_paths(&["node.exe"])
            .into_iter()
            .next()
            .ok_or_else(|| {
                "检测到 Codex npm 命令，但未找到可执行的 Node.js。请安装 Node.js 后重试。"
                    .to_string()
            })?
    };
    let mut command = TokioCommand::new(node);
    command.arg(entrypoint);
    Ok(command)
}

fn codex_child_command(cli_path: Option<PathBuf>) -> Result<TokioCommand, String> {
    #[cfg(windows)]
    {
        return match cli_path {
            Some(path)
                if path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("exe")) =>
            {
                Ok(TokioCommand::new(path))
            }
            Some(path)
                if path.extension().is_some_and(|extension| {
                    extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
                }) =>
            {
                codex_npm_node_command(&path)
            }
            Some(_) => Err("R-Code 只能启动已验证的 Codex .exe 或 npm .cmd 命令。".to_string()),
            // 正常委派始终先经 `probe_codex_cli`，因此这里不允许把任务文本交给 PATH
            // 中未知的 cmd shim。这样即使探测结果意外丢失，也不会退回 shell 解析。
            None => Ok(TokioCommand::new("codex.exe")),
        };
    }
    #[cfg(not(windows))]
    {
        Ok(TokioCommand::new(
            cli_path.unwrap_or_else(|| PathBuf::from("codex")),
        ))
    }
}

/// 构造 Codex 非交互进程。任务文本作为位置参数直接传给已验证的 CLI，绝不进入
/// shell 命令串。权限参数只会来自 [`CodexDelegationPermissions`] 的受限枚举，绝不
/// 接受 WebView 或 config.toml 的原始命令片段。
fn codex_exec_command_with_permissions(
    cli_path: Option<PathBuf>,
    workspace: &Path,
    permissions: CodexDelegationPermissions,
    prompt: &str,
) -> Result<TokioCommand, String> {
    if prompt.contains('\0') {
        return Err("Codex 委派任务不能包含 NUL 字符。".to_string());
    }
    // R-Code already treats the user-selected, PathGuard-validated workspace as the
    // delegation boundary. Codex otherwise refuses a perfectly valid non-Git folder
    // before it can emit JSONL, which makes folder-based projects fail instantly.
    // This is a fixed CLI flag, never derived from WebView input or config.toml.
    let exec_args = [
        "exec",
        "--json",
        "--skip-git-repo-check",
        "--sandbox",
        permissions.sandbox().as_str(),
        "-c",
        permissions.approval_policy().config_override(),
        "-c",
        permissions.approvals_reviewer().config_override(),
    ];
    let mut command = codex_child_command(cli_path)?;
    command.args(exec_args).arg(prompt);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        // Codex 本身可能继续派生 shell、Node 或工具进程。让 wrapper 成为独立进程组
        // 的组长，取消与超时时即可只回收这一棵子树，不影响 R-Code 或其他任务。
        command.as_std_mut().process_group(0);
    }
    command
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    Ok(command)
}

async fn emit_codex_observable_event(
    observer: Option<&CodexExecObserver<'_>>,
    event_sink: Option<&CodexSubagentEventSink>,
    event: AgentEvent,
) {
    if let Some(event_sink) = event_sink {
        event_sink(event.clone());
    }
    if let Some(observer) = observer {
        persist_and_emit_external_event(
            observer.db,
            observer.session_store,
            observer.sessions_dir,
            observer.parent_storage_id,
            observer.run,
            scoped_external_event(observer.run, event),
            observer.sink,
        )
        .await;
    }
}

/// 终止 Codex 的整棵进程树。
///
/// Windows 通过 `taskkill /T` 回收 `.cmd` wrapper 与 Node 后代；Unix/macOS 在启动时
/// 已为 Codex 建立独立进程组，这里向该组发送 SIGKILL。最后再杀直接子进程作为兜底。
async fn terminate_codex_child(child: &mut tokio::process::Child) {
    #[cfg(windows)]
    if let Some(pid) = child.id() {
        let pid = pid.to_string();
        let mut terminate_tree = TokioCommand::new("taskkill");
        terminate_tree
            .args(["/PID", pid.as_str(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let _ = timeout(Duration::from_secs(5), terminate_tree.status()).await;
    }
    #[cfg(unix)]
    if let Some(process_group) = child.id().and_then(|pid| i32::try_from(pid).ok()) {
        // SAFETY: process_group 来自刚由本进程启动、且通过 process_group(0) 隔离的
        // Codex 子进程。负 PID 只命中这一进程组；返回值仅用于 best-effort 清理。
        let _ = unsafe { libc::kill(-process_group, libc::SIGKILL) };
    }
    let _ = child.kill().await;
}

#[cfg_attr(not(test), allow(dead_code))]
async fn run_codex_exec_process(
    workspace: &Path,
    prompt: &str,
    cli_path: Option<PathBuf>,
    cancellation: CancellationToken,
    observer: Option<CodexExecObserver<'_>>,
) -> CodexExecCompletion {
    run_codex_exec_process_with_options(
        workspace,
        prompt,
        cli_path,
        cancellation,
        observer,
        None,
        CodexExecLimits::default(),
    )
    .await
}

#[cfg_attr(not(test), allow(dead_code))]
async fn run_codex_exec_process_with_options(
    workspace: &Path,
    prompt: &str,
    cli_path: Option<PathBuf>,
    cancellation: CancellationToken,
    observer: Option<CodexExecObserver<'_>>,
    event_sink: Option<&CodexSubagentEventSink>,
    limits: CodexExecLimits,
) -> CodexExecCompletion {
    run_codex_exec_process_with_options_and_permissions(
        workspace,
        prompt,
        cli_path,
        cancellation,
        observer,
        event_sink,
        CodexDelegationPermissions::read_only(),
        limits,
    )
    .await
}

async fn run_codex_exec_process_with_options_and_permissions(
    workspace: &Path,
    prompt: &str,
    cli_path: Option<PathBuf>,
    cancellation: CancellationToken,
    observer: Option<CodexExecObserver<'_>>,
    event_sink: Option<&CodexSubagentEventSink>,
    permissions: CodexDelegationPermissions,
    limits: CodexExecLimits,
) -> CodexExecCompletion {
    use tokio::io::{AsyncBufReadExt, BufReader};

    if cancellation.is_cancelled() {
        return CodexExecCompletion {
            cancelled: true,
            ..Default::default()
        };
    }

    let mut child =
        match codex_exec_command_with_permissions(cli_path, workspace, permissions, prompt)
            .and_then(|mut command| command.spawn().map_err(|error| error.to_string()))
        {
            Ok(child) => child,
            Err(error) => {
                let run_id = observer
                    .as_ref()
                    .map(|value| value.run.id.as_str())
                    .unwrap_or("agent-tool");
                tracing::warn!(run_id, error = %error, "failed to launch Codex exec child");
                return CodexExecCompletion {
                    failure: Some(CodexExecFailure::Launch),
                    ..Default::default()
                };
            }
        };
    emit_codex_observable_event(
        observer.as_ref(),
        event_sink,
        AgentEvent::Activity {
            phase: AgentActivityPhase::Requesting,
            detail: Some("Codex CLI 进程已启动".to_string()),
        },
    )
    .await;

    let Some(stdout) = child.stdout.take() else {
        terminate_codex_child(&mut child).await;
        return CodexExecCompletion {
            failure: Some(CodexExecFailure::Stream),
            ..Default::default()
        };
    };
    let stderr_task = child.stderr.take().map(|mut stderr| {
        tokio::spawn(async move {
            // Codex 的补充诊断会写入 stderr。持续排空防止管道回压，但绝不把原始输出
            // 写入日志、数据库或 WebView（它可能含本地路径或敏感上下文）。
            let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
        })
    });

    let mut lines = BufReader::new(stdout).lines();
    let mut cancelled = false;
    let mut failure = None;
    let mut thread_id = None;
    let mut summary = None;
    let mut usage_json = None;
    let idle_timer = tokio::time::sleep(limits.idle_timeout);
    let deadline_timer = tokio::time::sleep(limits.hard_timeout);
    tokio::pin!(idle_timer);
    tokio::pin!(deadline_timer);

    loop {
        tokio::select! {
            _ = cancellation.cancelled() => {
                cancelled = true;
                terminate_codex_child(&mut child).await;
                break;
            }
            _ = &mut idle_timer => {
                failure = Some(CodexExecFailure::IdleTimeout);
                emit_codex_observable_event(
                    observer.as_ref(),
                    event_sink,
                    AgentEvent::Activity {
                        phase: AgentActivityPhase::Finalizing,
                        detail: Some("连续 5 分钟没有进度，正在自动停止 Codex CLI".to_string()),
                    },
                ).await;
                terminate_codex_child(&mut child).await;
                break;
            }
            _ = &mut deadline_timer => {
                failure = Some(CodexExecFailure::Deadline);
                emit_codex_observable_event(
                    observer.as_ref(),
                    event_sink,
                    AgentEvent::Activity {
                        phase: AgentActivityPhase::Finalizing,
                        detail: Some("已达到 30 分钟运行上限，正在自动停止 Codex CLI".to_string()),
                    },
                ).await;
                terminate_codex_child(&mut child).await;
                break;
            }
            next = lines.next_line() => match next {
                Ok(Some(line)) => {
                    // 任何 JSONL 行都证明进程仍在推进；即便该事件因隐私策略未映射，也重置空闲计时。
                    idle_timer.as_mut().reset(tokio::time::Instant::now() + limits.idle_timeout);
                    if let Some(event) = parse_codex_exec_json_line(&line) {
                        match event {
                            CodexExecJsonEvent::ThreadStarted(external_thread_id) => {
                                thread_id = Some(external_thread_id.clone());
                                if let Some(observer) = observer.as_ref() {
                                    let _ = AgentRunRepository::new(observer.db)
                                        .set_external_session_id(&observer.run.id, Some(&external_thread_id));
                                }
                                emit_codex_observable_event(
                                    observer.as_ref(),
                                    event_sink,
                                    AgentEvent::Activity {
                                        phase: AgentActivityPhase::Requesting,
                                        detail: Some("已连接 Codex CLI，正在准备工作区".to_string()),
                                    },
                                ).await;
                            }
                            CodexExecJsonEvent::Activity { phase, detail } => {
                                emit_codex_observable_event(
                                    observer.as_ref(),
                                    event_sink,
                                    AgentEvent::Activity { phase, detail: Some(detail) },
                                ).await;
                            }
                            CodexExecJsonEvent::ToolStarted { call_id, name, summary: action } => {
                                emit_codex_observable_event(
                                    observer.as_ref(),
                                    event_sink,
                                    AgentEvent::ToolCall {
                                        name,
                                        input: serde_json::json!({ "summary": action }),
                                        call_id,
                                    },
                                ).await;
                            }
                            CodexExecJsonEvent::ToolCompleted { call_id, is_error } => {
                                emit_codex_observable_event(
                                    observer.as_ref(),
                                    event_sink,
                                    AgentEvent::ToolResult {
                                        call_id,
                                        output: serde_json::json!({
                                            "status": if is_error { "failed" } else { "completed" }
                                        }),
                                        is_error,
                                    },
                                ).await;
                            }
                            CodexExecJsonEvent::AssistantMessage(text) => {
                                summary = Some(text.clone());
                                emit_codex_observable_event(
                                    observer.as_ref(),
                                    event_sink,
                                    AgentEvent::Message { text, delta: false },
                                ).await;
                            }
                            CodexExecJsonEvent::Usage(value) => {
                                usage_json = Some(value.clone());
                                if let Some(observer) = observer.as_ref() {
                                    let _ = AgentRunRepository::new(observer.db)
                                        .set_usage(&observer.run.id, &value);
                                }
                            }
                            CodexExecJsonEvent::Failed => {
                                failure = Some(CodexExecFailure::Reported);
                                emit_codex_observable_event(
                                    observer.as_ref(),
                                    event_sink,
                                    AgentEvent::Activity {
                                        phase: AgentActivityPhase::Finalizing,
                                        detail: Some("Codex CLI 报告执行失败".to_string()),
                                    },
                                ).await;
                            }
                        }
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    let run_id = observer
                        .as_ref()
                        .map(|value| value.run.id.as_str())
                        .unwrap_or("agent-tool");
                    tracing::warn!(run_id, kind = ?error.kind(), "Codex exec JSONL stream failed");
                    failure = Some(CodexExecFailure::Stream);
                    terminate_codex_child(&mut child).await;
                    break;
                }
            }
        }
    }

    let child_was_terminated = cancelled
        || matches!(
            failure,
            Some(
                CodexExecFailure::Stream
                    | CodexExecFailure::IdleTimeout
                    | CodexExecFailure::Deadline
            )
        );
    let status = if child_was_terminated {
        child.wait().await
    } else {
        tokio::select! {
            status = child.wait() => status,
            _ = cancellation.cancelled() => {
                cancelled = true;
                terminate_codex_child(&mut child).await;
                child.wait().await
            }
            _ = &mut idle_timer => {
                failure = Some(CodexExecFailure::IdleTimeout);
                emit_codex_observable_event(
                    observer.as_ref(),
                    event_sink,
                    AgentEvent::Activity {
                        phase: AgentActivityPhase::Finalizing,
                        detail: Some("连续 5 分钟没有进度，正在自动停止 Codex CLI".to_string()),
                    },
                ).await;
                terminate_codex_child(&mut child).await;
                child.wait().await
            }
            _ = &mut deadline_timer => {
                failure = Some(CodexExecFailure::Deadline);
                emit_codex_observable_event(
                    observer.as_ref(),
                    event_sink,
                    AgentEvent::Activity {
                        phase: AgentActivityPhase::Finalizing,
                        detail: Some("已达到 30 分钟运行上限，正在自动停止 Codex CLI".to_string()),
                    },
                ).await;
                terminate_codex_child(&mut child).await;
                child.wait().await
            }
        }
    };
    if let Some(stderr_task) = stderr_task {
        let _ = timeout(Duration::from_secs(2), stderr_task).await;
    }
    if !cancelled && failure.is_none() && !status.as_ref().is_ok_and(|value| value.success()) {
        failure = Some(CodexExecFailure::ExitStatus);
    }

    CodexExecCompletion {
        cancelled,
        succeeded: !cancelled && failure.is_none(),
        thread_id,
        summary,
        usage_json,
        failure,
    }
}

const CODEX_APP_SERVER_START_TIMEOUT: Duration = Duration::from_secs(15);
const CODEX_APP_SERVER_MAX_LINE_BYTES: usize = 512 * 1024;
const CODEX_APP_SERVER_APPROVAL_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// R-Code 对 Codex App Server 审批请求的归属。它只引用既有权限引擎，因此审批卡
/// 会自然出现在当前任务的 Room / Inbox，而不是让 `codex exec` 在无终端环境中等待。
#[derive(Clone)]
struct CodexAppServerApprovalContext {
    permission_engine: Arc<PermissionEngine>,
    task_id: String,
    run_id: String,
    caller: String,
}

enum CodexAppServerRequestHandling {
    Ignored,
    Handled,
    Cancelled,
    Failed,
}

/// 启动官方 Codex App Server。该协议是用于 `on-request` 审批的双向 JSON-RPC
/// 通道：R-Code 始终作为客户端，任务文本不会进入 shell 命令串。
fn codex_app_server_command(
    cli_path: Option<PathBuf>,
    workspace: &Path,
) -> Result<TokioCommand, String> {
    #[cfg(windows)]
    let mut command = match cli_path {
        Some(path)
            if path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("exe")) =>
        {
            let mut command = TokioCommand::new(path);
            command.arg("app-server");
            command
        }
        path => {
            let executable = path.unwrap_or_else(|| PathBuf::from("codex"));
            windows_cmd_safe_path(&executable)?;
            let mut command = TokioCommand::new("cmd.exe");
            command
                .args(["/D", "/S", "/C", "call"])
                .arg(executable)
                .arg("app-server");
            command
        }
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut command = TokioCommand::new(cli_path.unwrap_or_else(|| PathBuf::from("codex")));
        command.arg("app-server");
        command
    };
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;

        command.as_std_mut().process_group(0);
    }
    command
        .current_dir(workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    Ok(command)
}

async fn write_codex_app_server_value(
    stdin: &mut tokio::process::ChildStdin,
    value: &serde_json::Value,
) -> Result<(), ()> {
    let payload = serde_json::to_vec(value).map_err(|_| ())?;
    stdin.write_all(&payload).await.map_err(|_| ())?;
    stdin.write_all(b"\n").await.map_err(|_| ())?;
    stdin.flush().await.map_err(|_| ())
}

async fn wait_for_codex_app_server_response(
    lines: &mut tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
    expected_id: u64,
) -> Result<serde_json::Value, CodexExecFailure> {
    loop {
        let next = timeout(CODEX_APP_SERVER_START_TIMEOUT, lines.next_line())
            .await
            .map_err(|_| CodexExecFailure::ApprovalBridge)?
            .map_err(|_| CodexExecFailure::ApprovalBridge)?;
        let Some(line) = next else {
            return Err(CodexExecFailure::ApprovalBridge);
        };
        if line.len() > CODEX_APP_SERVER_MAX_LINE_BYTES {
            return Err(CodexExecFailure::ApprovalBridge);
        }
        let value: serde_json::Value =
            serde_json::from_str(line.trim()).map_err(|_| CodexExecFailure::ApprovalBridge)?;
        if value.get("id").and_then(serde_json::Value::as_u64) != Some(expected_id) {
            // 初始化期间的通知不携带用户可见内容；后续主循环会处理运行期事件。
            continue;
        }
        if value.get("error").is_some() {
            return Err(CodexExecFailure::ApprovalBridge);
        }
        return value
            .get("result")
            .cloned()
            .ok_or(CodexExecFailure::ApprovalBridge);
    }
}

fn codex_app_server_thread_id(result: &serde_json::Value) -> Option<String> {
    result
        .pointer("/thread/id")
        .or_else(|| result.get("threadId"))
        .or_else(|| result.get("id"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| bounded_text(value, 160))
}

fn codex_app_server_approval_summary(method: &str, params: &serde_json::Value) -> (String, String) {
    let detail = match method {
        "item/commandExecution/requestApproval" => params
            .get("command")
            .and_then(serde_json::Value::as_str)
            .or_else(|| params.get("reason").and_then(serde_json::Value::as_str))
            .unwrap_or("Codex 请求执行一条命令"),
        "item/fileChange/requestApproval" => params
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .or_else(|| params.get("fileChange").and_then(serde_json::Value::as_str))
            .unwrap_or("Codex 请求修改工作区文件"),
        "item/permissions/requestApproval" => params
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Codex 请求扩大访问范围"),
        _ => "Codex 请求额外权限",
    };
    let tool_name = match method {
        "item/commandExecution/requestApproval" => "Codex 命令",
        "item/fileChange/requestApproval" => "Codex 文件修改",
        "item/permissions/requestApproval" => "Codex 访问权限",
        _ => "Codex 权限请求",
    };
    (
        tool_name.to_string(),
        safe_codex_action(detail, "Codex 请求额外权限"),
    )
}

async fn handle_codex_app_server_request(
    value: &serde_json::Value,
    stdin: &mut tokio::process::ChildStdin,
    approval: &CodexAppServerApprovalContext,
    cancellation: &CancellationToken,
    observer: Option<&CodexExecObserver<'_>>,
    event_sink: Option<&CodexSubagentEventSink>,
) -> CodexAppServerRequestHandling {
    let Some(method) = value.get("method").and_then(serde_json::Value::as_str) else {
        return CodexAppServerRequestHandling::Ignored;
    };
    if !matches!(
        method,
        "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "item/permissions/requestApproval"
    ) {
        return CodexAppServerRequestHandling::Ignored;
    }
    let Some(request_id) = value.get("id") else {
        return CodexAppServerRequestHandling::Failed;
    };
    let params = value.get("params").cloned().unwrap_or_default();
    let item_id = params
        .get("itemId")
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(|value| bounded_text(value, 160))
        .unwrap_or_else(|| {
            let source = request_id
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| request_id.to_string());
            format!("codex-approval-{}", bounded_text(&source, 80))
        });
    let (tool_name, summary) = codex_app_server_approval_summary(method, &params);
    let permission = match approval
        .permission_engine
        .check_detailed_with_access_mode(
            &approval.task_id,
            &item_id,
            Some(&approval.run_id),
            Some(&approval.caller),
            &tool_name,
            RiskLevel::R2,
            &summary,
            None,
            ProjectAccessMode::RequestApproval,
        )
        .await
    {
        PermissionCheckResult::NeedsApproval(request) => Some(request),
        PermissionCheckResult::Allowed => None,
        PermissionCheckResult::Denied(_) => {
            if write_codex_app_server_value(
                stdin,
                &serde_json::json!({ "id": request_id, "result": { "decision": "decline" } }),
            )
            .await
            .is_err()
            {
                return CodexAppServerRequestHandling::Failed;
            }
            return CodexAppServerRequestHandling::Handled;
        }
    };

    let decision = if let Some(permission) = permission {
        emit_codex_observable_event(
            observer,
            event_sink,
            AgentEvent::Activity {
                phase: AgentActivityPhase::Tool,
                detail: Some("Codex 正在等待你的权限批准".to_string()),
            },
        )
        .await;
        tokio::select! {
            _ = cancellation.cancelled() => {
                let _ = approval
                    .permission_engine
                    .decide(&permission.id, PermissionDecision::Deny)
                    .await;
                ("cancel", true)
            }
            result = approval.permission_engine.wait_decision(&permission.id, CODEX_APP_SERVER_APPROVAL_TIMEOUT) => {
                match result {
                    Some(PermissionDecision::Allow) => ("accept", false),
                    Some(PermissionDecision::AllowAlways) => ("acceptForSession", false),
                    Some(PermissionDecision::Deny) | Some(PermissionDecision::Pending) => ("decline", false),
                    None => {
                        let _ = approval
                            .permission_engine
                            .decide(&permission.id, PermissionDecision::Deny)
                            .await;
                        ("cancel", false)
                    }
                }
            }
        }
    } else {
        ("accept", false)
    };

    if write_codex_app_server_value(
        stdin,
        &serde_json::json!({ "id": request_id, "result": { "decision": decision.0 } }),
    )
    .await
    .is_err()
    {
        return CodexAppServerRequestHandling::Failed;
    }
    if decision.1 {
        return CodexAppServerRequestHandling::Cancelled;
    }
    if decision.0 == "accept" || decision.0 == "acceptForSession" {
        emit_codex_observable_event(
            observer,
            event_sink,
            AgentEvent::Activity {
                phase: AgentActivityPhase::Tool,
                detail: Some("已将权限决定发送给 Codex".to_string()),
            },
        )
        .await;
    }
    CodexAppServerRequestHandling::Handled
}

async fn observe_codex_app_server_event(
    value: &serde_json::Value,
    observer: Option<&CodexExecObserver<'_>>,
    event_sink: Option<&CodexSubagentEventSink>,
    summary: &mut Option<String>,
) -> Option<CodexExecFailure> {
    let Some(method) = value.get("method").and_then(serde_json::Value::as_str) else {
        return None;
    };
    let params = value.get("params").cloned().unwrap_or_default();
    match method {
        "item/started" => {
            emit_codex_observable_event(
                observer,
                event_sink,
                AgentEvent::Activity {
                    phase: AgentActivityPhase::Requesting,
                    detail: Some("Codex 正在处理委派任务".to_string()),
                },
            )
            .await;
        }
        "item/completed" => {
            let item = params.get("item").unwrap_or(&params);
            let item_type = item.get("type").and_then(serde_json::Value::as_str);
            if matches!(item_type, Some("agentMessage") | Some("agent_message")) {
                if let Some(text) = item
                    .get("text")
                    .or_else(|| item.get("content"))
                    .and_then(serde_json::Value::as_str)
                    .map(|text| bounded_text(text, CODEX_EXEC_MAX_SUMMARY_CHARS))
                    .filter(|text| !text.trim().is_empty())
                {
                    *summary = Some(text.clone());
                    emit_codex_observable_event(
                        observer,
                        event_sink,
                        AgentEvent::Message { text, delta: false },
                    )
                    .await;
                }
            }
        }
        "turn/completed" => {
            let turn = params.get("turn").unwrap_or(&params);
            if turn
                .get("status")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|status| matches!(status, "failed" | "error" | "cancelled"))
            {
                return Some(CodexExecFailure::Reported);
            }
        }
        "turn/failed" | "error" => return Some(CodexExecFailure::Reported),
        _ => {}
    }
    None
}

/// 用 App Server 执行一轮 Codex 子代理。只在 `请求批准` 预设下使用；其他预设
/// 继续走轻量的 `codex exec --json` 路径。
async fn run_codex_app_server_process(
    workspace: &Path,
    prompt: &str,
    cli_path: Option<PathBuf>,
    permissions: CodexDelegationPermissions,
    cancellation: CancellationToken,
    observer: Option<CodexExecObserver<'_>>,
    event_sink: Option<&CodexSubagentEventSink>,
    approval: CodexAppServerApprovalContext,
    limits: CodexExecLimits,
) -> CodexExecCompletion {
    use tokio::io::{AsyncBufReadExt, BufReader};

    if cancellation.is_cancelled() {
        return CodexExecCompletion {
            cancelled: true,
            ..Default::default()
        };
    }
    let mut child = match codex_app_server_command(cli_path, workspace)
        .and_then(|mut command| command.spawn().map_err(|error| error.to_string()))
    {
        Ok(child) => child,
        Err(error) => {
            tracing::warn!(error = %error, "failed to launch Codex App Server");
            return CodexExecCompletion {
                failure: Some(CodexExecFailure::ApprovalBridge),
                ..Default::default()
            };
        }
    };
    let Some(mut stdin) = child.stdin.take() else {
        terminate_codex_child(&mut child).await;
        return CodexExecCompletion {
            failure: Some(CodexExecFailure::ApprovalBridge),
            ..Default::default()
        };
    };
    let Some(stdout) = child.stdout.take() else {
        terminate_codex_child(&mut child).await;
        return CodexExecCompletion {
            failure: Some(CodexExecFailure::ApprovalBridge),
            ..Default::default()
        };
    };
    let stderr_task = child.stderr.take().map(|mut stderr| {
        tokio::spawn(async move {
            let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
        })
    });
    let mut lines = BufReader::new(stdout).lines();

    let setup = async {
        write_codex_app_server_value(
            &mut stdin,
            &serde_json::json!({
                "id": 0,
                "method": "initialize",
                "params": { "clientInfo": { "name": "r-code", "version": env!("CARGO_PKG_VERSION") } }
            }),
        )
        .await
        .map_err(|_| CodexExecFailure::ApprovalBridge)?;
        let _ = wait_for_codex_app_server_response(&mut lines, 0).await?;
        write_codex_app_server_value(
            &mut stdin,
            &serde_json::json!({ "method": "initialized", "params": {} }),
        )
        .await
        .map_err(|_| CodexExecFailure::ApprovalBridge)?;
        let cwd = workspace.to_str().ok_or(CodexExecFailure::ApprovalBridge)?;
        write_codex_app_server_value(
            &mut stdin,
            &serde_json::json!({
                "id": 1,
                "method": "thread/start",
                "params": {
                    "cwd": cwd,
                    "sandbox": permissions.sandbox().as_str(),
                    "approvalPolicy": permissions.approval_policy().as_str(),
                    "approvalsReviewer": permissions.approvals_reviewer().as_str(),
                }
            }),
        )
        .await
        .map_err(|_| CodexExecFailure::ApprovalBridge)?;
        let thread = wait_for_codex_app_server_response(&mut lines, 1).await?;
        let thread_id = codex_app_server_thread_id(&thread).ok_or(CodexExecFailure::ApprovalBridge)?;
        write_codex_app_server_value(
            &mut stdin,
            &serde_json::json!({
                "id": 2,
                "method": "turn/start",
                "params": {
                    "threadId": thread_id,
                    "input": [{ "type": "text", "text": prompt }],
                }
            }),
        )
        .await
        .map_err(|_| CodexExecFailure::ApprovalBridge)?;
        let _ = wait_for_codex_app_server_response(&mut lines, 2).await?;
        Ok::<String, CodexExecFailure>(thread_id)
    }
    .await;

    let thread_id = match setup {
        Ok(thread_id) => thread_id,
        Err(failure) => {
            terminate_codex_child(&mut child).await;
            if let Some(stderr_task) = stderr_task {
                let _ = timeout(Duration::from_secs(2), stderr_task).await;
            }
            return CodexExecCompletion {
                failure: Some(failure),
                ..Default::default()
            };
        }
    };
    if let Some(observer) = observer.as_ref() {
        let _ = AgentRunRepository::new(observer.db)
            .set_external_session_id(&observer.run.id, Some(&thread_id));
    }
    emit_codex_observable_event(
        observer.as_ref(),
        event_sink,
        AgentEvent::Activity {
            phase: AgentActivityPhase::Requesting,
            detail: Some("已连接 Codex 审批桥，正在准备工作区".to_string()),
        },
    )
    .await;

    let mut cancelled = false;
    let mut failure = None;
    let mut summary = None;
    let mut completed = false;
    let idle_timer = tokio::time::sleep(limits.idle_timeout);
    let deadline_timer = tokio::time::sleep(limits.hard_timeout);
    tokio::pin!(idle_timer);
    tokio::pin!(deadline_timer);
    loop {
        tokio::select! {
            _ = cancellation.cancelled() => {
                cancelled = true;
                break;
            }
            _ = &mut idle_timer => {
                failure = Some(CodexExecFailure::IdleTimeout);
                emit_codex_observable_event(
                    observer.as_ref(),
                    event_sink,
                    AgentEvent::Activity {
                        phase: AgentActivityPhase::Finalizing,
                        detail: Some("连续 5 分钟没有进度，正在自动停止 Codex".to_string()),
                    },
                ).await;
                break;
            }
            _ = &mut deadline_timer => {
                failure = Some(CodexExecFailure::Deadline);
                emit_codex_observable_event(
                    observer.as_ref(),
                    event_sink,
                    AgentEvent::Activity {
                        phase: AgentActivityPhase::Finalizing,
                        detail: Some("已达到 30 分钟运行上限，正在自动停止 Codex".to_string()),
                    },
                ).await;
                break;
            }
            next = lines.next_line() => match next {
                Ok(Some(line)) => {
                    if line.len() > CODEX_APP_SERVER_MAX_LINE_BYTES {
                        failure = Some(CodexExecFailure::ApprovalBridge);
                        break;
                    }
                    idle_timer.as_mut().reset(tokio::time::Instant::now() + limits.idle_timeout);
                    let value: serde_json::Value = match serde_json::from_str(line.trim()) {
                        Ok(value) => value,
                        Err(_) => {
                            failure = Some(CodexExecFailure::ApprovalBridge);
                            break;
                        }
                    };
                    match handle_codex_app_server_request(
                        &value,
                        &mut stdin,
                        &approval,
                        &cancellation,
                        observer.as_ref(),
                        event_sink,
                    ).await {
                        CodexAppServerRequestHandling::Cancelled => {
                            cancelled = true;
                            break;
                        }
                        CodexAppServerRequestHandling::Failed => {
                            failure = Some(CodexExecFailure::ApprovalBridge);
                            break;
                        }
                        CodexAppServerRequestHandling::Handled | CodexAppServerRequestHandling::Ignored => {}
                    }
                    if let Some(event_failure) = observe_codex_app_server_event(
                        &value,
                        observer.as_ref(),
                        event_sink,
                        &mut summary,
                    ).await {
                        failure = Some(event_failure);
                        break;
                    }
                    if value.get("method").and_then(serde_json::Value::as_str) == Some("turn/completed") {
                        completed = failure.is_none();
                        break;
                    }
                }
                Ok(None) => {
                    failure = Some(CodexExecFailure::Stream);
                    break;
                }
                Err(_) => {
                    failure = Some(CodexExecFailure::Stream);
                    break;
                }
            }
        }
    }
    terminate_codex_child(&mut child).await;
    let _ = child.wait().await;
    if let Some(stderr_task) = stderr_task {
        let _ = timeout(Duration::from_secs(2), stderr_task).await;
    }
    CodexExecCompletion {
        cancelled,
        succeeded: completed && !cancelled && failure.is_none(),
        thread_id: Some(thread_id),
        summary,
        usage_json: None,
        failure,
    }
}

async fn run_codex_exec_subagent(
    db: &Database,
    session_store: &SessionStore,
    sessions_dir: &Path,
    parent_storage_id: &str,
    run: &AgentRun,
    workspace: &Path,
    prompt: &str,
    cli_path: Option<PathBuf>,
    permissions: CodexDelegationPermissions,
    permission_engine: Arc<PermissionEngine>,
    cancellation: CancellationToken,
    sink: &Option<AgentEventSink>,
) -> CodexExecCompletion {
    run_codex_delegation_process(
        workspace,
        prompt,
        cli_path,
        cancellation,
        Some(CodexExecObserver {
            db,
            session_store,
            sessions_dir,
            parent_storage_id,
            run,
            sink,
        }),
        None,
        permissions,
        CodexAppServerApprovalContext {
            permission_engine,
            task_id: run.task_id.clone(),
            run_id: run.id.clone(),
            caller: format!("subagent:{}", run.id),
        },
    )
    .await
}

/// 根据每次启动时读取到的 profile 选择轻量 exec 或可交互 App Server。调用方传入
/// 的 `permissions` 已经是安全解析后的快照，运行中不受后续配置修改影响。
async fn run_codex_delegation_process(
    workspace: &Path,
    prompt: &str,
    cli_path: Option<PathBuf>,
    cancellation: CancellationToken,
    observer: Option<CodexExecObserver<'_>>,
    event_sink: Option<&CodexSubagentEventSink>,
    permissions: CodexDelegationPermissions,
    approval: CodexAppServerApprovalContext,
) -> CodexExecCompletion {
    if permissions.requests_r_code_approval() {
        run_codex_app_server_process(
            workspace,
            prompt,
            cli_path,
            permissions,
            cancellation,
            observer,
            event_sink,
            approval,
            CodexExecLimits::default(),
        )
        .await
    } else {
        run_codex_exec_process_with_options_and_permissions(
            workspace,
            prompt,
            cli_path,
            cancellation,
            observer,
            event_sink,
            permissions,
            CodexExecLimits::default(),
        )
        .await
    }
}

fn spawn_codex_exec_subagent(
    external_agents: Arc<ExternalAgentRegistry>,
    db: Arc<Database>,
    sessions_dir: PathBuf,
    parent_storage_id: String,
    run: AgentRun,
    workspace: PathBuf,
    prompt: String,
    cli_path: Option<PathBuf>,
    permissions: CodexDelegationPermissions,
    permission_engine: Arc<PermissionEngine>,
    cancellation: CancellationToken,
    sink: Option<AgentEventSink>,
) {
    tokio::spawn(async move {
        let session_store = SessionStore::new(sessions_dir.clone());
        persist_and_emit_external_event(
            &db,
            &session_store,
            &sessions_dir,
            &parent_storage_id,
            &run,
            scoped_external_event(
                &run,
                AgentEvent::SubagentLifecycle {
                    state: SubagentState::Running,
                    detail: Some(format!(
                        "Codex CLI 正在以{}模式处理工作区",
                        permissions.mode().display_name()
                    )),
                },
            ),
            &sink,
        )
        .await;

        let completion = run_codex_exec_subagent(
            &db,
            &session_store,
            &sessions_dir,
            &parent_storage_id,
            &run,
            &workspace,
            &prompt,
            cli_path,
            permissions,
            permission_engine,
            cancellation,
            &sink,
        )
        .await;

        let (state, review_state, summary) = if completion.cancelled {
            (
                SubagentState::Cancelled,
                ReviewState::Aborted,
                "Codex CLI 子代理已取消。".to_string(),
            )
        } else if completion.succeeded {
            (
                SubagentState::Completed,
                ReviewState::Answered,
                completion.summary.unwrap_or_else(|| {
                    "Codex CLI 子代理已完成，但没有返回可显示的摘要。".to_string()
                }),
            )
        } else {
            (
                SubagentState::Failed,
                ReviewState::Failed,
                codex_exec_failure_message(completion.failure),
            )
        };
        let summary = bounded_text(&summary, CODEX_EXEC_MAX_SUMMARY_CHARS);
        let lifecycle_detail = bounded_text(&summary, CODEX_EXEC_MAX_LIFECYCLE_DETAIL_CHARS);

        let repository = AgentRunRepository::new(&db);
        if let Some(thread_id) = completion.thread_id.as_deref() {
            let _ = repository.set_external_session_id(&run.id, Some(thread_id));
        }
        if let Some(usage_json) = completion.usage_json.as_deref() {
            let _ = repository.set_usage(&run.id, usage_json);
        }
        let _ = repository.set_summary(&run.id, Some(&summary));
        let _ = repository.update_review_state(&run.id, review_state);
        let _ = TaskEventStore::new(&db).append_for_branch(
            &run.task_id,
            &run.branch_id,
            TaskEventType::SubagentFinished,
        );

        persist_and_emit_external_event(
            &db,
            &session_store,
            &sessions_dir,
            &parent_storage_id,
            &run,
            scoped_external_event(
                &run,
                AgentEvent::SubagentLifecycle {
                    state,
                    detail: Some(lifecycle_detail),
                },
            ),
            &sink,
        )
        .await;
        external_agents.remove(&run.id).await;
    });
}

/// 启动由官方 `codex mcp-server` 驱动的、采用已配置权限的子代理。
///
/// 与 `codex exec` 路径共享同一份运行树、取消注册和安全投影规则；差异仅在于
/// MCP 会话返回的 thread ID 会被保存，以便后续可显式续接。
fn spawn_codex_mcp_subagent(
    external_agents: Arc<ExternalAgentRegistry>,
    codex_mcp: Arc<CodexMcpRegistry>,
    db: Arc<Database>,
    sessions_dir: PathBuf,
    parent_storage_id: String,
    run: AgentRun,
    workspace: PathBuf,
    prompt: String,
    cli_path: Option<PathBuf>,
    permissions: CodexDelegationPermissions,
    cancellation: CancellationToken,
    sink: Option<AgentEventSink>,
) {
    tokio::spawn(async move {
        let session_store = SessionStore::new(sessions_dir.clone());
        persist_and_emit_external_event(
            &db,
            &session_store,
            &sessions_dir,
            &parent_storage_id,
            &run,
            scoped_external_event(
                &run,
                AgentEvent::SubagentLifecycle {
                    state: SubagentState::Running,
                    detail: Some(format!(
                        "Codex MCP 会话正在以{}模式处理工作区",
                        permissions.mode().display_name()
                    )),
                },
            ),
            &sink,
        )
        .await;

        let outcome = codex_mcp
            .run(cli_path, &workspace, &prompt, permissions, cancellation)
            .await;
        let (state, review_state, summary) = match outcome {
            Ok(CodexMcpCallOutcome::Cancelled) => (
                SubagentState::Cancelled,
                ReviewState::Aborted,
                "Codex MCP 子代理已取消。".to_string(),
            ),
            Ok(CodexMcpCallOutcome::Completed(response)) => {
                if let Some(thread_id) = response.thread_id.as_deref() {
                    let _ = AgentRunRepository::new(&db)
                        .set_external_session_id(&run.id, Some(thread_id));
                }
                if let Some(text) = response
                    .text
                    .as_deref()
                    .map(|text| bounded_text(text, CODEX_EXEC_MAX_SUMMARY_CHARS))
                    .filter(|text| !text.trim().is_empty())
                {
                    persist_and_emit_external_event(
                        &db,
                        &session_store,
                        &sessions_dir,
                        &parent_storage_id,
                        &run,
                        scoped_external_event(
                            &run,
                            AgentEvent::Message {
                                text: text.clone(),
                                delta: false,
                            },
                        ),
                        &sink,
                    )
                    .await;
                    (SubagentState::Completed, ReviewState::Answered, text)
                } else {
                    (
                        SubagentState::Completed,
                        ReviewState::Answered,
                        "Codex MCP 子代理已完成，但没有返回可显示的摘要。".to_string(),
                    )
                }
            }
            Err(error) => {
                tracing::warn!(run_id = %run.id, error = ?error, "Codex MCP subagent failed");
                (
                    SubagentState::Failed,
                    ReviewState::Failed,
                    format!("Codex MCP 子代理未能完成：{error}"),
                )
            }
        };
        let summary = bounded_text(&summary, CODEX_EXEC_MAX_SUMMARY_CHARS);
        let lifecycle_detail = bounded_text(&summary, CODEX_EXEC_MAX_LIFECYCLE_DETAIL_CHARS);

        let repository = AgentRunRepository::new(&db);
        let _ = repository.set_summary(&run.id, Some(&summary));
        let _ = repository.update_review_state(&run.id, review_state);
        let _ = TaskEventStore::new(&db).append_for_branch(
            &run.task_id,
            &run.branch_id,
            TaskEventType::SubagentFinished,
        );
        persist_and_emit_external_event(
            &db,
            &session_store,
            &sessions_dir,
            &parent_storage_id,
            &run,
            scoped_external_event(
                &run,
                AgentEvent::SubagentLifecycle {
                    state,
                    detail: Some(lifecycle_detail),
                },
            ),
            &sink,
        )
        .await;
        external_agents.remove(&run.id).await;
    });
}

/// 将一项工作委派给本机已登录的 Codex CLI。
///
/// 每次启动都会读取 Codex config.toml 的权限预设；外部工具活动仍不会伪装成
/// R-Code 网关审计，只有 App Server 公开的审批事件会投影为 R-Code 权限卡。
pub async fn agent_delegate_codex(
    state: &CommandState,
    task_id: &str,
    goal: &str,
    label: Option<&str>,
) -> Result<AgentRun, String> {
    let goal = goal.trim();
    if goal.is_empty() {
        return Err("Codex 子代理需要一项明确的委派任务".to_string());
    }
    if goal.contains('\0') {
        return Err("Codex 子代理任务不能包含空字符".to_string());
    }
    let goal = bounded_text(goal, CODEX_EXEC_MAX_GOAL_CHARS);
    let task = TaskRepository::new(&state.db)
        .get(task_id)
        .map_err(err_str)?
        .ok_or_else(|| format!("task not found: {task_id}"))?;
    if task.state == TaskState::Archived {
        return Err("会话已归档，不能委派 Codex 子代理".to_string());
    }
    let workspace_path = task
        .workspace_path
        .as_deref()
        .ok_or_else(|| "请先为会话附加一个本地 Git 工作区，再委派 Codex 子代理".to_string())?;
    let workspace = PathGuard::new(PathBuf::from(workspace_path))
        .map_err(err_str)?
        .root()
        .to_path_buf();
    let branch = SessionBranchRepository::new(&state.db)
        .ensure_active(task_id)
        .map_err(err_str)?;
    let parent = AgentRunRepository::new(&state.db)
        .get_active_run_for_branch(task_id, &branch.id)
        .map_err(err_str)?
        .ok_or_else(|| "请先启动当前 R-Code 会话，再委派 Codex 子代理".to_string())?;
    if parent.runtime_kind != AgentRunRuntimeKind::Native {
        return Err("当前外部运行暂不能继续委派其他 Agent".to_string());
    }

    let cli = probe_codex_cli().await;
    if !cli.available {
        return Err(cli
            .error
            .unwrap_or("未检测到可运行的 Codex CLI。")
            .to_string());
    }
    if probe_codex_login(cli.path.as_deref()).await.state == CodexAuthState::NotAuthenticated {
        return Err("Codex CLI 尚未登录。请在设置中完成浏览器登录或设备码登录后重试。".to_string());
    }
    let permissions = read_codex_delegation_permissions(&codex_home_dir().join("config.toml"))?;

    let user_label = label
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| bounded_text(value, 100));
    let agent_label = user_label
        .map(|value| format!("Codex CLI · {value}"))
        .unwrap_or_else(|| format!("Codex CLI · {}任务", permissions.mode().display_name()));
    let run = AgentRun::new_subagent_for_branch(
        task_id,
        &branch.id,
        &parent.id,
        "codex-cli",
        Some(agent_label),
        None,
    )
    .as_codex_exec_subagent();
    let cancellation = state
        .external_agents
        .reserve(task_id, &parent.id, &run.id)
        .await?;
    if let Err(error) = AgentRunRepository::new(&state.db)
        .create(&run)
        .map_err(err_str)
    {
        state.external_agents.remove(&run.id).await;
        return Err(error);
    }
    if let Err(error) = TaskEventStore::new(&state.db)
        .append_for_branch(task_id, &branch.id, TaskEventType::SubagentStarted)
        .map_err(err_str)
    {
        let _ =
            AgentRunRepository::new(&state.db).update_review_state(&run.id, ReviewState::Failed);
        state.external_agents.remove(&run.id).await;
        return Err(error);
    }

    let sink = state.agent_event_sink.lock().unwrap().clone();
    persist_and_emit_external_event(
        &state.db,
        &state.session_store,
        &state.sessions_dir,
        &branch.storage_id,
        &run,
        scoped_external_event(
            &run,
            AgentEvent::SubagentLifecycle {
                state: SubagentState::Queued,
                detail: Some(format!(
                    "已加入 Codex CLI {}子代理队列",
                    permissions.mode().display_name()
                )),
            },
        ),
        &sink,
    )
    .await;

    spawn_codex_exec_subagent(
        state.external_agents.clone(),
        state.db.clone(),
        state.sessions_dir.clone(),
        branch.storage_id,
        run.clone(),
        workspace,
        build_codex_delegation_prompt(&goal, permissions),
        cli.path,
        permissions,
        state.permission_engine.clone(),
        cancellation,
        sink,
    );
    Ok(run)
}

/// 以可续接的官方 `codex mcp-server` 会话委派一项任务。
///
/// 这条路径与 `agent_delegate_codex` 的非交互式 `exec` 保持并存：前者更适合一次
/// 性快速任务，MCP 版本则保留 Codex thread ID，供 R-Code 及未来的外部编排器继续
/// 对话。`请求批准` 必须使用 App Server 才能回传审批卡，因此该预设会自动转到同
/// 一条 App Server 委派路径。两条路径都只能从正在运行的原生 R-Code 主 Agent 发起。
pub async fn agent_delegate_codex_mcp(
    state: &CommandState,
    task_id: &str,
    goal: &str,
    label: Option<&str>,
) -> Result<AgentRun, String> {
    let goal = goal.trim();
    if goal.is_empty() {
        return Err("Codex MCP 子代理需要一项明确的委派任务".to_string());
    }
    if goal.contains('\0') {
        return Err("Codex MCP 子代理任务不能包含空字符".to_string());
    }
    let goal = bounded_text(goal, CODEX_EXEC_MAX_GOAL_CHARS);
    let task = TaskRepository::new(&state.db)
        .get(task_id)
        .map_err(err_str)?
        .ok_or_else(|| format!("task not found: {task_id}"))?;
    if task.state == TaskState::Archived {
        return Err("会话已归档，不能委派 Codex MCP 子代理".to_string());
    }
    let workspace_path = task
        .workspace_path
        .as_deref()
        .ok_or_else(|| "请先为会话附加一个本地 Git 工作区，再委派 Codex MCP 子代理".to_string())?;
    let workspace = PathGuard::new(PathBuf::from(workspace_path))
        .map_err(err_str)?
        .root()
        .to_path_buf();
    let branch = SessionBranchRepository::new(&state.db)
        .ensure_active(task_id)
        .map_err(err_str)?;
    let parent = AgentRunRepository::new(&state.db)
        .get_active_run_for_branch(task_id, &branch.id)
        .map_err(err_str)?
        .ok_or_else(|| "请先启动当前 R-Code 会话，再委派 Codex MCP 子代理".to_string())?;
    if parent.runtime_kind != AgentRunRuntimeKind::Native {
        return Err("当前外部运行暂不能继续委派其他 Agent".to_string());
    }

    let cli = probe_codex_cli().await;
    if !cli.available {
        return Err(cli
            .error
            .unwrap_or("未检测到可运行的 Codex CLI。")
            .to_string());
    }
    if probe_codex_login(cli.path.as_deref()).await.state == CodexAuthState::NotAuthenticated {
        return Err("Codex CLI 尚未登录。请在设置中完成浏览器登录或设备码登录后重试。".to_string());
    }
    let permissions = read_codex_delegation_permissions(&codex_home_dir().join("config.toml"))?;
    if permissions.requests_r_code_approval() {
        // `codex mcp-server` 不会向 MCP client 暴露可答复的批准请求；复用 exec
        // 委派的 App Server 桥，确保“请求批准”不会表现为无进度卡住。
        return agent_delegate_codex(state, task_id, &goal, label).await;
    }

    let user_label = label
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| bounded_text(value, 100));
    let agent_label = user_label
        .map(|value| format!("Codex MCP · {value}"))
        .unwrap_or_else(|| format!("Codex MCP · {}任务", permissions.mode().display_name()));
    let run = AgentRun::new_subagent_for_branch(
        task_id,
        &branch.id,
        &parent.id,
        "codex-cli",
        Some(agent_label),
        None,
    )
    .as_codex_mcp_subagent();
    let cancellation = state
        .external_agents
        .reserve(task_id, &parent.id, &run.id)
        .await?;
    if let Err(error) = AgentRunRepository::new(&state.db)
        .create(&run)
        .map_err(err_str)
    {
        state.external_agents.remove(&run.id).await;
        return Err(error);
    }
    if let Err(error) = TaskEventStore::new(&state.db)
        .append_for_branch(task_id, &branch.id, TaskEventType::SubagentStarted)
        .map_err(err_str)
    {
        let _ =
            AgentRunRepository::new(&state.db).update_review_state(&run.id, ReviewState::Failed);
        state.external_agents.remove(&run.id).await;
        return Err(error);
    }

    let sink = state.agent_event_sink.lock().unwrap().clone();
    persist_and_emit_external_event(
        &state.db,
        &state.session_store,
        &state.sessions_dir,
        &branch.storage_id,
        &run,
        scoped_external_event(
            &run,
            AgentEvent::SubagentLifecycle {
                state: SubagentState::Queued,
                detail: Some(format!(
                    "已加入 Codex MCP {}子代理队列",
                    permissions.mode().display_name()
                )),
            },
        ),
        &sink,
    )
    .await;
    spawn_codex_mcp_subagent(
        state.external_agents.clone(),
        state.codex_mcp.clone(),
        state.db.clone(),
        state.sessions_dir.clone(),
        branch.storage_id,
        run.clone(),
        workspace,
        build_codex_delegation_prompt(&goal, permissions),
        cli.path,
        permissions,
        cancellation,
        sink,
    );
    Ok(run)
}

/// 用户显式请求时才把 R-Code 终端协作 Skill 安装到 Codex 的用户目录。
pub async fn codex_install_skill() -> Result<(), String> {
    SkillManager::new().install_codex().map_err(err_str)
}

/// 一次完成 R-Code 与 Codex 的协作配置。
///
/// 调用方必须已通过安装和登录门禁。本命令会更新 R-Code 协作 Skill，并注册固定名称
/// 的 Codex MCP server；旧版直连构建产物的配置会自动迁移，当前配置不会重复写入。
pub async fn codex_setup_collaboration(state: &CommandState) -> Result<serde_json::Value, String> {
    let _guard = CODEX_COLLAB_SETUP_LOCK.lock().await;
    let cli = probe_codex_cli().await;
    if !cli.available {
        return Err(cli
            .error
            .unwrap_or("未检测到可运行的 Codex CLI。")
            .to_string());
    }

    match probe_codex_login(cli.path.as_deref()).await.state {
        CodexAuthState::Authenticated => {}
        CodexAuthState::NotAuthenticated => {
            return Err("Codex CLI 尚未登录。请先完成 Codex 官方登录流程。".to_string())
        }
        CodexAuthState::Unknown => {
            return Err("暂时无法确认 Codex 登录状态，请重新检测后再试。".to_string())
        }
    }

    codex_install_skill().await?;
    if !codex_mcp_server_configured(cli.path.as_deref()).await {
        codex_install_mcp_server(state).await?;
    }
    codex_integration_status().await
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

    /// 构造一个“上次进程退出前仍在运行”的文件数据库。
    ///
    /// `CommandState::new` 在此之后建立，因而它记录到的项目才是恢复范围；
    /// 测试可再往同一数据库写入新运行，验证它不会被误收束。
    fn setup_persisted_recovery_state() -> (TempDir, CommandState, Task, AgentRun, ToolCall, String)
    {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("r-code.db");
        let db = Arc::new(Database::open(&db_path).unwrap());
        let task = Task::new(None, "Interrupted", "recover me", TaskMode::Edit);
        TaskRepository::new(db.as_ref()).create(&task).unwrap();
        TaskRepository::new(db.as_ref())
            .update_state(&task.id, TaskState::InProgress)
            .unwrap();
        let branch = SessionBranchRepository::new(db.as_ref())
            .ensure_active(&task.id)
            .unwrap();
        let run = AgentRun::new_for_branch(&task.id, &branch.id, "test-model");
        AgentRunRepository::new(db.as_ref()).create(&run).unwrap();
        let tool_call = ToolCall::new(&run.id, &task.id, "shell", "{}", RiskLevel::R2);
        ToolCallRepository::new(db.as_ref())
            .create_if_absent(&tool_call)
            .unwrap();
        let permission_id = uuid::Uuid::new_v4().to_string();
        let conn = db.conn().unwrap();
        conn.execute(
            "INSERT INTO permission_requests \
             (id, task_id, tool_call_id, tool_name, risk_level, input_summary, decision, created_at) \
             VALUES (?1, ?2, ?3, 'shell', 'R2', 'cargo test', 'pending', ?4)",
            rusqlite::params![
                &permission_id,
                &task.id,
                &tool_call.id,
                chrono::Utc::now().to_rfc3339(),
            ],
        )
        .unwrap();
        drop(conn);

        let blobs_dir = dir.path().join("blobs");
        let sessions_dir = dir.path().join("sessions");
        let config_dir = dir.path().join("config");
        std::fs::create_dir_all(&blobs_dir).unwrap();
        std::fs::create_dir_all(&sessions_dir).unwrap();
        std::fs::create_dir_all(&config_dir).unwrap();
        let state = CommandState::new(
            db,
            blobs_dir,
            sessions_dir,
            config_dir,
            dir.path().to_path_buf(),
            Some(db_path),
        );
        (dir, state, task, run, tool_call, permission_id)
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
    async fn task_rename_preserves_session_and_rejects_blank_title() {
        let (_dir, state) = setup_state();
        let task = task_create(&state, None, "Before", "Do thing", "ask")
            .await
            .unwrap();

        let renamed = task_rename(&state, &task.id, "  After  ").await.unwrap();
        assert_eq!(renamed.id, task.id);
        assert_eq!(renamed.title, "After");
        assert!(task_rename(&state, &task.id, "   ").await.is_err());
    }

    #[tokio::test]
    async fn task_fork_context_preserves_source_and_switches_active_branch() {
        let (_dir, state) = setup_state();
        let task = task_create(&state, None, "Fork", "Try another direction", "ask")
            .await
            .unwrap();
        let source = SessionBranchRepository::new(&state.db)
            .ensure_active(&task.id)
            .unwrap();
        ensure_session_log(
            &state.session_store,
            &state.sessions_dir,
            &source.storage_id,
        )
        .await
        .unwrap();
        state
            .session_store
            .append(
                &source.storage_id,
                SessionEvent::Message(Message::user_text("first")),
            )
            .await
            .unwrap();
        state
            .session_store
            .append(
                &source.storage_id,
                SessionEvent::Message(Message::assistant_text("answer")),
            )
            .await
            .unwrap();

        let fork = task_fork_context(&state, &task.id).await.unwrap();
        let repo = SessionBranchRepository::new(&state.db);
        let active = repo.ensure_active(&task.id).unwrap();
        let source_history = state.session_store.load(&source.storage_id).await.unwrap();
        let fork_history = state.session_store.load(&fork.storage_id).await.unwrap();

        assert_eq!(fork.parent_branch_id.as_deref(), Some(source.id.as_str()));
        assert_eq!(active.id, fork.id);
        assert_eq!(repo.list_by_task(&task.id).unwrap().len(), 2);
        assert_eq!(source_history.messages.len(), 2);
        assert_eq!(fork_history.messages.len(), 2);
    }

    #[test]
    fn compacted_working_set_keeps_opening_context_and_recent_messages() {
        let history = (0..18)
            .map(|index| {
                if index % 2 == 0 {
                    Message::user_text(format!("message-{index}"))
                } else {
                    Message::assistant_text(format!("message-{index}"))
                }
            })
            .collect::<Vec<_>>();

        let compacted = compacted_working_set(&history, "decisions and pending work");

        assert_eq!(
            compacted.len(),
            COMPACTION_KEEP_FIRST + 1 + COMPACTION_KEEP_RECENT
        );
        assert_eq!(compacted[0].text_content(), "message-0");
        assert!(compacted[1]
            .text_content()
            .contains("decisions and pending work"));
        assert_eq!(compacted[2].text_content(), "message-8");
        assert_eq!(compacted.last().unwrap().text_content(), "message-17");
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
    async fn task_detail_batch_deduplicates_ids_and_keeps_detail_shape() {
        let (_dir, state) = setup_state();
        let first = task_create(&state, None, "First", "g", "edit")
            .await
            .unwrap();
        let second = task_create(&state, None, "Second", "g", "edit")
            .await
            .unwrap();

        let batch = task_detail_batch(
            &state,
            &[first.id.clone(), second.id.clone(), first.id.clone()],
        )
        .await
        .unwrap();
        assert_eq!(batch.details.len(), 2);
        assert!(batch
            .details
            .iter()
            .any(|detail| detail.task.id == first.id));
        assert!(batch
            .details
            .iter()
            .any(|detail| detail.task.id == second.id));
    }

    #[tokio::test]
    async fn workspace_dashboard_and_activity_use_workspace_scoped_records() {
        let (_dir, state) = setup_state();
        let workspace = scoped_test_workspace(&state).await;
        let task = task_create(&state, Some(&workspace), "Dashboard", "g", "edit")
            .await
            .unwrap();
        TaskRepository::new(&state.db)
            .update_state(&task.id, TaskState::ReviewReady)
            .unwrap();
        let branch = SessionBranchRepository::new(&state.db)
            .ensure_active(&task.id)
            .unwrap();
        TaskEventStore::new(&state.db)
            .append_for_branch(&task.id, &branch.id, TaskEventType::RunEnded)
            .unwrap();

        let dashboard = workspace_dashboard(&state, &workspace).await.unwrap();
        assert_eq!(dashboard.metrics.task_count, 1);
        assert_eq!(dashboard.metrics.review_ready_count, 1);
        assert_eq!(dashboard.attention.len(), 1);
        assert_eq!(dashboard.tasks[0].task.id, task.id);

        let activity = project_activity_list(&state, &workspace, None, 20)
            .await
            .unwrap();
        assert!(activity.items.iter().any(|item| item.task_id == task.id));
        assert!(activity.next_cursor.is_some());
    }

    #[tokio::test]
    async fn notification_center_persists_read_state_for_review_ready_tasks() {
        let (_dir, state) = setup_state();
        let task = task_create(&state, None, "Review", "g", "edit")
            .await
            .unwrap();
        TaskRepository::new(&state.db)
            .update_state(&task.id, TaskState::ReviewReady)
            .unwrap();

        let first_page = notification_list(&state, None, 20, false).await.unwrap();
        let notification = first_page
            .notifications
            .iter()
            .find(|item| item.task_id.as_deref() == Some(task.id.as_str()))
            .unwrap();
        assert_eq!(notification.kind, NotificationKind::ReviewReady);
        assert_eq!(first_page.unread_count, 1);

        assert!(notification_mark_read(&state, &notification.id)
            .await
            .unwrap());
        let second_page = notification_list(&state, None, 20, false).await.unwrap();
        assert_eq!(second_page.unread_count, 0);
        assert!(second_page.notifications[0].read_at.is_some());
    }

    #[tokio::test]
    async fn change_request_records_an_audit_event_and_starts_follow_up() {
        let (_dir, state) = setup_state();
        let task = task_create(&state, None, "Review", "g", "edit")
            .await
            .unwrap();
        TaskRepository::new(&state.db)
            .update_state(&task.id, TaskState::ReviewReady)
            .unwrap();

        change_request(&state, &task.id, "请补充错误分支测试")
            .await
            .unwrap();
        let events = TaskEventStore::new(&state.db)
            .list_by_task(&task.id, None, None)
            .unwrap();
        assert!(events
            .iter()
            .any(|event| event.event_type == TaskEventType::ChangeRequested));
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
            runtime_kind: AgentRunRuntimeKind::Native,
            model: Some("test-model".to_string()),
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
        let child_activity = AgentEvent::Scoped {
            scope: scope.clone(),
            event: Box::new(AgentEvent::Activity {
                phase: AgentActivityPhase::Tool,
                detail: Some("正在读取 README.md".to_string()),
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
            &child_activity,
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
        assert_eq!(child.runtime_kind, AgentRunRuntimeKind::Native);
        assert_eq!(child.model, "test-model");
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
        let child_messages = subagent_session_messages(&state, &task.id, "child-run")
            .await
            .unwrap();
        assert!(child_messages.iter().any(|message| {
            message.kind == "system"
                && message.text.as_deref() == Some("subagent_activity")
                && message
                    .output_json
                    .as_deref()
                    .is_some_and(|value| value.contains("正在读取 README.md"))
        }));
        assert!(
            subagent_session_messages(&state, "another-task", "child-run")
                .await
                .is_err()
        );
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
    async fn codex_scoped_lifecycle_persists_external_runtime_identity() {
        let (_dir, state) = setup_state();
        let task = task_create(&state, None, "Codex", "delegate", "ask")
            .await
            .unwrap();
        let branch = SessionBranchRepository::new(&state.db)
            .ensure_active(&task.id)
            .unwrap();
        let parent = AgentRun::new_for_branch(&task.id, &branch.id, "test-model");
        AgentRunRepository::new(&state.db).create(&parent).unwrap();
        let event = AgentEvent::Scoped {
            scope: AgentEventScope {
                run_id: "codex-child".to_string(),
                agent_id: "codex-child".to_string(),
                parent_run_id: Some(parent.id.clone()),
                agent_kind: AgentKind::Subagent,
                agent_label: Some("Codex CLI · 检查边界".to_string()),
                delegated_by_tool_call_id: None,
                runtime_kind: AgentRunRuntimeKind::CodexExec,
                model: Some("codex-cli".to_string()),
            },
            event: Box::new(AgentEvent::SubagentLifecycle {
                state: SubagentState::Queued,
                detail: Some("已加入 Codex CLI 子代理队列".to_string()),
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
            &event,
            &mut HashMap::new(),
        )
        .await;

        let child = AgentRunRepository::new(&state.db)
            .get("codex-child")
            .unwrap()
            .unwrap();
        assert_eq!(child.runtime_kind, AgentRunRuntimeKind::CodexExec);
        assert_eq!(child.model, "codex-cli");
        assert_eq!(child.agent_label.as_deref(), Some("Codex CLI · 检查边界"));
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
        let result = recovery_cleanup(&state).await.unwrap();
        assert_eq!(result.runs_closed, 0);
        assert_eq!(result.tasks_interrupted, 0);
        assert_eq!(result.permissions_denied, 0);
        assert_eq!(result.tool_calls_closed, 0);
    }

    #[tokio::test]
    async fn recovery_only_closes_runs_captured_at_startup() {
        let (_dir, state, stale_task, stale_run, stale_call, permission_id) =
            setup_persisted_recovery_state();

        // 这条运行在 CommandState 已建立后才开始，绝不能被当作崩溃遗留项。
        let fresh_task = Task::new(None, "Fresh", "keep running", TaskMode::Edit);
        TaskRepository::new(&state.db).create(&fresh_task).unwrap();
        TaskRepository::new(&state.db)
            .update_state(&fresh_task.id, TaskState::InProgress)
            .unwrap();
        let fresh_branch = SessionBranchRepository::new(&state.db)
            .ensure_active(&fresh_task.id)
            .unwrap();
        let fresh_run = AgentRun::new_for_branch(&fresh_task.id, &fresh_branch.id, "test-model");
        AgentRunRepository::new(&state.db)
            .create(&fresh_run)
            .unwrap();
        let fresh_call = ToolCall::new(&fresh_run.id, &fresh_task.id, "shell", "{}", RiskLevel::R2);
        ToolCallRepository::new(&state.db)
            .create_if_absent(&fresh_call)
            .unwrap();
        let fresh_permission_id = uuid::Uuid::new_v4().to_string();
        let conn = state.db.conn().unwrap();
        conn.execute(
            "INSERT INTO permission_requests \
             (id, task_id, tool_call_id, tool_name, risk_level, input_summary, decision, created_at) \
             VALUES (?1, ?2, ?3, 'shell', 'R2', 'fresh command', 'pending', ?4)",
            rusqlite::params![
                &fresh_permission_id,
                &fresh_task.id,
                &fresh_call.id,
                chrono::Utc::now().to_rfc3339(),
            ],
        )
        .unwrap();
        drop(conn);

        let page = recovery_data(&state).await.unwrap();
        assert_eq!(page.interrupted_tasks, vec![stale_task.id.clone()]);
        assert_eq!(page.orphaned_permissions, 1);

        let result = recovery_cleanup(&state).await.unwrap();
        assert_eq!(result.runs_closed, 1);
        assert_eq!(result.tasks_interrupted, 1);
        assert_eq!(result.tool_calls_closed, 1);
        assert_eq!(result.permissions_denied, 1);
        assert!(recovery_data(&state)
            .await
            .unwrap()
            .interrupted_tasks
            .is_empty());

        let closed_run = AgentRunRepository::new(&state.db)
            .get(&stale_run.id)
            .unwrap()
            .unwrap();
        assert_eq!(closed_run.review_state, ReviewState::Aborted);
        assert!(closed_run.ended_at.is_some());
        assert_eq!(
            TaskRepository::new(&state.db)
                .get(&stale_task.id)
                .unwrap()
                .unwrap()
                .state,
            TaskState::Interrupted
        );

        let conn = state.db.conn().unwrap();
        let tool_status: String = conn
            .query_row(
                "SELECT status FROM tool_calls WHERE id = ?1",
                [&stale_call.id],
                |row| row.get(0),
            )
            .unwrap();
        let permission_decision: String = conn
            .query_row(
                "SELECT decision FROM permission_requests WHERE id = ?1",
                [&permission_id],
                |row| row.get(0),
            )
            .unwrap();
        let fresh_permission_decision: String = conn
            .query_row(
                "SELECT decision FROM permission_requests WHERE id = ?1",
                [&fresh_permission_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tool_status, "error");
        assert_eq!(permission_decision, "deny");
        assert_eq!(fresh_permission_decision, "pending");

        assert!(AgentRunRepository::new(&state.db)
            .get(&fresh_run.id)
            .unwrap()
            .unwrap()
            .is_active());
        assert_eq!(
            TaskRepository::new(&state.db)
                .get(&fresh_task.id)
                .unwrap()
                .unwrap()
                .state,
            TaskState::InProgress
        );
        let events = TaskEventStore::new(&state.db)
            .list_by_task(&stale_task.id, None, None)
            .unwrap();
        assert!(events
            .iter()
            .any(|event| event.event_type == TaskEventType::RunAborted));
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
    async fn delete_rejects_running_tasks_then_removes_records_and_session_logs() {
        let (_dir, state) = setup_state();
        let task = task_create(&state, None, "删除测试", "验证永久删除边界", "ask")
            .await
            .unwrap();
        let branch = SessionBranchRepository::new(&state.db)
            .ensure_active(&task.id)
            .unwrap();
        let session_path = state
            .sessions_dir
            .join(format!("{}.jsonl", branch.storage_id));
        std::fs::write(&session_path, "{}\n").unwrap();

        TaskRepository::new(&state.db)
            .update_state(&task.id, TaskState::InProgress)
            .unwrap();
        assert!(task_delete(&state, &task.id).await.is_err());
        assert!(TaskRepository::new(&state.db)
            .get(&task.id)
            .unwrap()
            .is_some());

        TaskRepository::new(&state.db)
            .update_state(&task.id, TaskState::Idle)
            .unwrap();
        task_delete(&state, &task.id).await.unwrap();
        assert!(TaskRepository::new(&state.db)
            .get(&task.id)
            .unwrap()
            .is_none());
        assert!(!session_path.exists());
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
    fn codex_login_status_uses_official_exit_code_contract() {
        let status = parse_codex_login_status(true, b"Logged in using ChatGPT\n", b"");
        assert_eq!(status.state, CodexAuthState::Authenticated);
        assert_eq!(status.method, Some("ChatGPT"));

        let status = parse_codex_login_status(false, b"", b"Not logged in");
        assert_eq!(status.state, CodexAuthState::NotAuthenticated);
        assert_eq!(status.method, None);

        let status = parse_codex_login_status(true, b"status unavailable", b"");
        assert_eq!(status.state, CodexAuthState::Authenticated);
        assert_eq!(status.method, None);

        let status = parse_codex_login_status(false, b"", b"temporary transport error");
        assert_eq!(status.state, CodexAuthState::Unknown);
    }

    #[test]
    fn codex_model_catalog_only_exposes_listed_models_and_efforts() {
        let catalog = parse_codex_model_catalog(
            br#"{
              "models": [
                {
                  "slug": "gpt-5.6-terra",
                  "display_name": "GPT-5.6-Terra",
                  "description": "Balanced model",
                  "default_reasoning_level": "medium",
                  "supported_reasoning_levels": [
                    {"effort": "low", "description": "Fast"},
                    {"effort": "medium", "description": "Balanced"}
                  ],
                  "visibility": "list",
                  "priority": 2
                },
                {
                  "slug": "hidden-model",
                  "display_name": "Hidden",
                  "description": "Internal",
                  "default_reasoning_level": "high",
                  "supported_reasoning_levels": [],
                  "visibility": "hidden",
                  "priority": 1
                }
              ]
            }"#,
        )
        .unwrap();
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].slug, "gpt-5.6-terra");
        assert_eq!(catalog[0].default_reasoning_effort, "medium");
        assert_eq!(catalog[0].supported_reasoning_efforts.len(), 2);
    }

    #[test]
    fn legacy_codex_mcp_registration_is_owned_but_not_build_safe() {
        let registration = parse_codex_mcp_registration(
            br#"{
              "name": "r-code",
              "enabled": true,
              "transport": {
                "type": "stdio",
                "command": "D:\\project\\r-code\\target\\debug\\r-code-host.exe",
                "args": [
                  "mcp-server",
                  "--data-dir",
                  "C:\\Users\\demo\\AppData\\Roaming\\com.r-code.app\\r-code"
                ]
              }
            }"#,
        )
        .unwrap();
        let current = Path::new(r"D:\project\r-code\target\debug\r-code-host.exe");
        let data_dir = Path::new(r"C:\Users\demo\AppData\Roaming\com.r-code.app\r-code");
        assert!(registration_is_owned_by_r_code(
            &registration,
            current,
            data_dir
        ));
        assert!(!registration_uses_current_host(&registration, current));
    }

    #[test]
    fn codex_mcp_host_uses_a_content_addressed_copy_outside_cargo_target() {
        let directory = TempDir::new().unwrap();
        let source = directory.path().join(if cfg!(windows) {
            "target/debug/r-code-host.exe"
        } else {
            "target/debug/r-code-host"
        });
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, b"first R-Code build").unwrap();
        let data_dir = directory.path().join("app-data/r-code");

        let first = deploy_codex_mcp_host(&source, &data_dir).unwrap();
        assert_ne!(first, source);
        assert_eq!(
            first.parent(),
            Some(data_dir.join(CODEX_MCP_HOST_DIR).as_path())
        );
        assert_eq!(std::fs::read(&first).unwrap(), b"first R-Code build");
        let registration = CodexMcpRegistration {
            enabled: true,
            command: first.clone(),
            args: vec![
                "mcp-server".to_string(),
                "--data-dir".to_string(),
                data_dir.to_string_lossy().to_string(),
            ],
        };
        assert!(registration_uses_current_host(&registration, &source));
        let bootstrap = first.parent().unwrap().join(if cfg!(windows) {
            "r-code-mcp-host-bootstrap.exe"
        } else {
            "r-code-mcp-host-bootstrap"
        });
        std::fs::copy(&first, &bootstrap).unwrap();
        let bootstrap_registration = CodexMcpRegistration {
            command: bootstrap,
            ..registration.clone()
        };
        assert!(registration_uses_current_host(
            &bootstrap_registration,
            &source
        ));

        std::fs::write(&source, b"second R-Code build").unwrap();
        assert!(!registration_uses_current_host(&registration, &source));
        let second = deploy_codex_mcp_host(&source, &data_dir).unwrap();
        assert_ne!(first, second);
        assert_eq!(std::fs::read(second).unwrap(), b"second R-Code build");
    }

    #[test]
    fn codex_preference_edit_preserves_comments_and_mcp_sections() {
        let source = r#"# keep this note
model = "old-model"
model_reasoning_effort = "low"

[mcp_servers.r-code]
command = "r-code-host"
"#;
        let rendered =
            render_codex_preferences(source, Some("gpt-5.6-sol"), Some("high"), Some("medium"))
                .unwrap();
        assert!(rendered.contains("# keep this note"));
        assert!(rendered.contains("[mcp_servers.r-code]"));
        assert!(rendered.contains("command = \"r-code-host\""));
        let document = rendered.parse::<toml_edit::DocumentMut>().unwrap();
        assert_eq!(document["model"].as_str(), Some("gpt-5.6-sol"));
        assert_eq!(document["model_reasoning_effort"].as_str(), Some("high"));
        assert_eq!(document["model_verbosity"].as_str(), Some("medium"));
    }

    #[test]
    fn codex_preference_edit_can_restore_cli_defaults() {
        let rendered = render_codex_preferences(
            "model = \"gpt-5.6-sol\"\nmodel_reasoning_effort = \"max\"\nmodel_verbosity = \"high\"\n[features]\nfast_mode = true\n",
            None,
            None,
            None,
        )
        .unwrap();
        let document = rendered.parse::<toml_edit::DocumentMut>().unwrap();
        assert!(document.get("model").is_none());
        assert!(document.get("model_reasoning_effort").is_none());
        assert!(document.get("model_verbosity").is_none());
        assert_eq!(document["features"]["fast_mode"].as_bool(), Some(true));
    }

    #[test]
    fn codex_permission_preset_rendering_preserves_unrelated_configuration() {
        let rendered = render_codex_permission_mode(
            "# keep this note\n[mcp_servers.r-code]\ncommand = \"r-code-host\"\n",
            CodexPermissionMode::AutoReview,
        )
        .unwrap();
        assert!(rendered.contains("# keep this note"));
        assert!(rendered.contains("[mcp_servers.r-code]"));
        let document = rendered.parse::<toml_edit::DocumentMut>().unwrap();
        assert_eq!(document["sandbox_mode"].as_str(), Some("workspace-write"));
        assert_eq!(document["approval_policy"].as_str(), Some("on-request"));
        assert_eq!(document["approvals_reviewer"].as_str(), Some("auto_review"));
        assert_eq!(
            read_codex_delegation_permissions_from_source(&rendered)
                .unwrap()
                .mode(),
            CodexPermissionMode::AutoReview
        );
    }

    #[test]
    fn codex_permission_preset_refuses_to_overwrite_a_profile_selector() {
        let error = render_codex_permission_mode(
            "default_permissions = \"safe\"\n[permissions.safe]\nsandbox_mode = \"read-only\"\n",
            CodexPermissionMode::FullAccess,
        )
        .unwrap_err();
        assert!(error.contains("default_permissions"));
    }

    #[test]
    fn codex_setup_state_follows_prerequisite_order() {
        assert_eq!(
            CodexSetupState::from_components(
                false,
                CodexAuthState::Unknown,
                "not_installed",
                false,
            ),
            CodexSetupState::InstallCli
        );
        assert_eq!(
            CodexSetupState::from_components(
                true,
                CodexAuthState::NotAuthenticated,
                "up_to_date",
                true,
            ),
            CodexSetupState::Login
        );
        assert_eq!(
            CodexSetupState::from_components(true, CodexAuthState::Unknown, "up_to_date", true,),
            CodexSetupState::Check
        );
        assert_eq!(
            CodexSetupState::from_components(
                true,
                CodexAuthState::Authenticated,
                "update_available",
                true,
            ),
            CodexSetupState::Configure
        );
        assert_eq!(
            CodexSetupState::from_components(
                true,
                CodexAuthState::Authenticated,
                "up_to_date",
                true,
            ),
            CodexSetupState::Ready
        );
    }

    #[test]
    fn codex_login_status_detects_supported_auth_methods() {
        let api = parse_codex_login_status(true, b"Authenticated with API Key", b"");
        assert_eq!(api.state, CodexAuthState::Authenticated);
        assert_eq!(api.method, Some("API Key"));

        let token = parse_codex_login_status(true, b"Signed in with access token", b"");
        assert_eq!(token.state, CodexAuthState::Authenticated);
        assert_eq!(token.method, Some("访问令牌"));
    }

    #[test]
    fn codex_home_prefers_explicit_codex_home() {
        let home = codex_home_dir_from(
            Some(PathBuf::from("D:/isolated/codex-home")),
            Some(PathBuf::from("C:/Users/example")),
        );
        assert_eq!(home, PathBuf::from("D:/isolated/codex-home"));

        let default = codex_home_dir_from(None, Some(PathBuf::from("C:/Users/example")));
        assert_eq!(default, PathBuf::from("C:/Users/example/.codex"));
    }

    #[test]
    fn codex_login_modes_are_fixed_commands() {
        assert_eq!(CodexLoginMode::Browser.args(), ["login"]);
        assert_eq!(
            CodexLoginMode::DeviceCode.args(),
            ["login", "--device-auth"]
        );
    }

    #[test]
    fn npm_installer_only_targets_the_official_codex_package() {
        let npm_path = if cfg!(windows) {
            Path::new(r"C:\Program Files\nodejs\npm.exe")
        } else {
            Path::new("/usr/local/bin/npm")
        };
        let command = npm_command_at(npm_path, CODEX_CLI_INSTALL_ARGS).unwrap();
        let command = command.as_std();
        assert_eq!(command.get_program(), npm_path.as_os_str());
        assert_eq!(
            command
                .get_args()
                .map(|value| value.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            ["install", "-g", "@openai/codex"]
        );
    }

    #[test]
    fn npm_install_errors_are_actionable_without_leaking_output() {
        assert!(npm_install_failure_message(b"npm ERR! code EACCES").contains("权限"));
        assert!(npm_install_failure_message(b"npm ERR! code ENOTFOUND").contains("网络"));
        assert!(npm_install_failure_message(b"secret registry output").contains("系统终端"));
        assert!(!npm_install_failure_message(b"secret registry output").contains("secret"));
    }

    #[test]
    fn codex_exec_json_parser_keeps_safe_progress_without_private_reasoning() {
        assert_eq!(
            parse_codex_exec_json_line(
                r#"{"type":"thread.started","thread_id":"thread-123","secret":"ignore"}"#
            ),
            Some(CodexExecJsonEvent::ThreadStarted("thread-123".to_string()))
        );
        assert_eq!(
            parse_codex_exec_json_line(
                r#"{"type":"item.completed","item":{"type":"agent_message","text":"调查完成"}}"#
            ),
            Some(CodexExecJsonEvent::AssistantMessage("调查完成".to_string()))
        );
        assert_eq!(
            parse_codex_exec_json_line(
                r#"{"type":"turn.completed","usage":{"input_tokens":12,"output_tokens":5,"ignored":"secret"}}"#
            ),
            Some(CodexExecJsonEvent::Usage(
                r#"{"input_tokens":12,"output_tokens":5}"#.to_string()
            ))
        );
        assert_eq!(
            parse_codex_exec_json_line(
                r#"{"type":"item.completed","item":{"type":"reasoning","text":"private"}}"#
            ),
            Some(CodexExecJsonEvent::Activity {
                phase: AgentActivityPhase::Finalizing,
                detail: "Codex CLI 已完成一轮分析".to_string(),
            })
        );
        let command = parse_codex_exec_json_line(
            r#"{"type":"item.started","item":{"id":"item-7","type":"command_execution","command":"curl -H 'Authorization: Bearer secret-token' https://example.test","status":"in_progress"}}"#,
        );
        assert!(matches!(
            command,
            Some(CodexExecJsonEvent::ToolStarted { call_id, name, summary })
                if call_id == "item-7"
                    && name == "Codex 命令"
                    && summary.contains("Authorization: ***")
                    && !summary.contains("secret-token")
        ));
    }

    #[tokio::test]
    async fn external_agent_registry_caps_and_cancels_by_run() {
        let registry = ExternalAgentRegistry::default();
        let first = registry.reserve("task", "parent", "one").await.unwrap();
        registry.reserve("task", "parent", "two").await.unwrap();
        registry.reserve("task", "parent", "three").await.unwrap();
        assert!(registry.reserve("task", "parent", "four").await.is_err());
        assert!(registry.has_for_parent_run("parent").await);
        assert!(!registry.cancel_run_for_task("other-task", "one").await);
        assert!(registry.cancel_run_for_task("task", "one").await);
        assert!(first.is_cancelled());
        registry.remove("one").await;
        assert!(registry.reserve("task", "parent", "four").await.is_ok());
    }

    #[tokio::test]
    async fn codex_delegation_requires_an_active_native_parent_run() {
        let (_dir, state) = setup_state();
        let workspace = scoped_test_workspace(&state).await;
        let task = task_create(&state, Some(&workspace), "Codex", "inspect", "ask")
            .await
            .unwrap();

        let error = agent_delegate_codex(&state, &task.id, "检查入口", None)
            .await
            .unwrap_err();
        assert!(error.contains("启动当前 R-Code 会话"));
    }

    #[tokio::test]
    async fn codex_mcp_delegation_requires_an_active_native_parent_run() {
        let (_dir, state) = setup_state();
        let workspace = scoped_test_workspace(&state).await;
        let task = task_create(&state, Some(&workspace), "Codex MCP", "inspect", "ask")
            .await
            .unwrap();

        let error = agent_delegate_codex_mcp(&state, &task.id, "检查入口", None)
            .await
            .unwrap_err();
        assert!(error.contains("启动当前 R-Code 会话"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_mcp_config_rejects_cmd_metacharacters_in_paths() {
        assert!(windows_cmd_safe_path(Path::new(r"C:\Program Files\R-Code\R-Code.exe")).is_ok());
        assert!(windows_cmd_safe_path(Path::new(r"C:\bad&path\R-Code.exe")).is_err());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn codex_probe_runner_uses_an_exact_cmd_shim_with_spaces() {
        let directory = TempDir::new().unwrap();
        let shim_dir = directory.path().join("Codex Test");
        std::fs::create_dir_all(&shim_dir).unwrap();
        let shim = shim_dir.join("codex.cmd");
        std::fs::write(&shim, "@echo off\r\necho codex-test 1.0\r\n").unwrap();

        let output = run_codex_cli_at(&shim, &["--version"]).await.unwrap();
        assert!(
            output.status.success(),
            "stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            first_nonempty_line(&output.stdout).as_deref(),
            Some("codex-test 1.0")
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn codex_subagent_process_collects_a_safe_summary_from_the_cli_stream() {
        if executable_paths(&["node.exe"]).is_empty() {
            return;
        }
        let directory = TempDir::new().unwrap();
        let shim = directory.path().join("codex.cmd");
        let entrypoint = directory
            .path()
            .join("node_modules")
            .join("@openai")
            .join("codex")
            .join("bin")
            .join("codex.js");
        std::fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
        std::fs::write(&shim, "@echo off\r\n").unwrap();
        std::fs::write(
            &entrypoint,
            r#"process.stdout.write('{"type":"thread.started","thread_id":"thread-test"}\n');
process.stdout.write('{"type":"item.completed","item":{"type":"agent_message","text":"Codex child summary"}}\n');
process.stdout.write('{"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":2}}\n');"#,
        )
        .unwrap();

        let completion = run_codex_exec_process(
            directory.path(),
            "inspect only",
            Some(shim),
            CancellationToken::new(),
            None,
        )
        .await;

        assert!(completion.succeeded);
        assert!(!completion.cancelled);
        assert_eq!(completion.summary.as_deref(), Some("Codex child summary"));
        assert_eq!(
            completion.usage_json.as_deref(),
            Some(r#"{"input_tokens":1,"output_tokens":2}"#)
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn codex_app_server_process_returns_a_visible_summary_without_an_interactive_terminal() {
        let directory = TempDir::new().unwrap();
        let shim = directory.path().join("codex-app-server.cmd");
        std::fs::write(
            &shim,
            "@echo off\r\n\
setlocal\r\n\
set /p line=\r\n\
echo {\"id\":0,\"result\":{}}\r\n\
set /p line=\r\n\
set /p line=\r\n\
echo {\"id\":1,\"result\":{\"thread\":{\"id\":\"thread-app-server\"}}}\r\n\
set /p line=\r\n\
echo {\"id\":2,\"result\":{\"turn\":{\"id\":\"turn-app-server\"}}}\r\n\
echo {\"method\":\"item/completed\",\"params\":{\"item\":{\"type\":\"agentMessage\",\"text\":\"App Server child summary\"}}}\r\n\
echo {\"method\":\"turn/completed\",\"params\":{\"turn\":{\"status\":\"completed\"}}}\r\n\
exit /b 0\r\n",
        )
        .unwrap();
        let permissions =
            CodexDelegationPermissions::from_mode(CodexPermissionMode::RequestApproval)
                .expect("request-approval must be a built-in preset");
        let completion = run_codex_app_server_process(
            directory.path(),
            "inspect only",
            Some(shim),
            permissions,
            CancellationToken::new(),
            None,
            None,
            CodexAppServerApprovalContext {
                permission_engine: Arc::new(PermissionEngine::new()),
                task_id: "task-app-server".to_string(),
                run_id: "run-app-server".to_string(),
                caller: "subagent:run-app-server".to_string(),
            },
            CodexExecLimits {
                idle_timeout: Duration::from_secs(2),
                hard_timeout: Duration::from_secs(5),
            },
        )
        .await;
        assert!(completion.succeeded);
        assert!(!completion.cancelled);
        assert_eq!(completion.thread_id.as_deref(), Some("thread-app-server"));
        assert_eq!(
            completion.summary.as_deref(),
            Some("App Server child summary")
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn codex_app_server_turn_waits_for_an_r_code_permission_decision() {
        let directory = TempDir::new().unwrap();
        let shim = directory.path().join("codex-app-server-approval.cmd");
        std::fs::write(
            &shim,
            "@echo off\r\n\
setlocal\r\n\
set /p line=\r\n\
echo {\"id\":0,\"result\":{}}\r\n\
set /p line=\r\n\
set /p line=\r\n\
echo {\"id\":1,\"result\":{\"thread\":{\"id\":\"thread-approval\"}}}\r\n\
set /p line=\r\n\
echo {\"id\":2,\"result\":{\"turn\":{\"id\":\"turn-approval\"}}}\r\n\
echo {\"id\":3,\"method\":\"item/fileChange/requestApproval\",\"params\":{\"itemId\":\"change-approval\",\"reason\":\"modify approval.txt\"}}\r\n\
set /p line=\r\n\
echo {\"method\":\"item/completed\",\"params\":{\"item\":{\"type\":\"agentMessage\",\"text\":\"Approved change completed\"}}}\r\n\
echo {\"method\":\"turn/completed\",\"params\":{\"turn\":{\"status\":\"completed\"}}}\r\n\
exit /b 0\r\n",
        )
        .unwrap();
        let permission_engine = Arc::new(PermissionEngine::new());
        let engine_for_run = permission_engine.clone();
        let permissions =
            CodexDelegationPermissions::from_mode(CodexPermissionMode::RequestApproval)
                .expect("request-approval must be a built-in preset");
        let workspace = directory.path().to_path_buf();
        let run = tokio::spawn(async move {
            run_codex_app_server_process(
                &workspace,
                "make one approved change",
                Some(shim),
                permissions,
                CancellationToken::new(),
                None,
                None,
                CodexAppServerApprovalContext {
                    permission_engine: engine_for_run,
                    task_id: "task-approval".to_string(),
                    run_id: "run-approval".to_string(),
                    caller: "subagent:run-approval".to_string(),
                },
                CodexExecLimits {
                    idle_timeout: Duration::from_secs(2),
                    hard_timeout: Duration::from_secs(5),
                },
            )
            .await
        });
        let request = timeout(Duration::from_secs(2), async {
            loop {
                if let Some(request) = permission_engine
                    .pending_for_task("task-approval")
                    .await
                    .into_iter()
                    .next()
                {
                    break request;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("Codex App Server must surface an approval request");
        assert_eq!(request.run_id.as_deref(), Some("run-approval"));
        assert_eq!(request.tool_name, "Codex 文件修改");
        permission_engine
            .decide(&request.id, PermissionDecision::Allow)
            .await
            .unwrap();
        let completion = timeout(Duration::from_secs(5), run)
            .await
            .expect("Codex App Server turn must finish after approval")
            .unwrap();
        assert!(completion.succeeded);
        assert_eq!(
            completion.summary.as_deref(),
            Some("Approved change completed")
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn codex_subagent_process_stops_an_idle_process_tree() {
        if executable_paths(&["node.exe"]).is_empty() {
            return;
        }
        let directory = TempDir::new().unwrap();
        let shim = directory.path().join("codex.cmd");
        let entrypoint = directory
            .path()
            .join("node_modules")
            .join("@openai")
            .join("codex")
            .join("bin")
            .join("codex.js");
        std::fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
        std::fs::write(&shim, "@echo off\r\n").unwrap();
        std::fs::write(&entrypoint, "setInterval(() => {}, 1000);\n").unwrap();
        let observed = Arc::new(Mutex::new(Vec::<AgentEvent>::new()));
        let observed_for_sink = observed.clone();
        let event_sink: CodexSubagentEventSink = Arc::new(move |event| {
            observed_for_sink.lock().unwrap().push(event);
        });
        let started = std::time::Instant::now();

        let completion = run_codex_exec_process_with_options(
            directory.path(),
            "inspect only",
            Some(shim),
            CancellationToken::new(),
            None,
            Some(&event_sink),
            CodexExecLimits {
                idle_timeout: Duration::from_millis(80),
                hard_timeout: Duration::from_secs(5),
            },
        )
        .await;

        assert_eq!(completion.failure, Some(CodexExecFailure::IdleTimeout));
        assert!(!completion.succeeded);
        assert!(!completion.cancelled);
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(observed.lock().unwrap().iter().any(|event| matches!(
            event,
            AgentEvent::Activity { detail: Some(detail), .. }
                if detail.contains("自动停止")
        )));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn codex_subagent_process_stops_an_idle_unix_process_group() {
        use std::os::unix::fs::PermissionsExt;

        let directory = TempDir::new().unwrap();
        let shim = directory.path().join("codex-idle");
        let descendant_pid_path = directory.path().join("codex-descendant.pid");
        std::fs::write(
            &shim,
            format!(
                "#!/bin/sh\nsleep 30 &\necho $! > '{}'\nwait\n",
                descendant_pid_path.display()
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&shim).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&shim, permissions).unwrap();

        let completion = run_codex_exec_process_with_options(
            directory.path(),
            "inspect only",
            Some(shim),
            CancellationToken::new(),
            None,
            None,
            CodexExecLimits {
                idle_timeout: Duration::from_millis(250),
                hard_timeout: Duration::from_secs(5),
            },
        )
        .await;

        assert_eq!(completion.failure, Some(CodexExecFailure::IdleTimeout));
        let descendant_pid = std::fs::read_to_string(&descendant_pid_path)
            .unwrap()
            .trim()
            .parse::<i32>()
            .unwrap();
        let mut descendant_gone = false;
        for _ in 0..40 {
            // SAFETY: signal 0 performs an existence check only and does not mutate the process.
            let exists = unsafe { libc::kill(descendant_pid, 0) } == 0;
            if !exists && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
                descendant_gone = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(
            descendant_gone,
            "Codex descendant process survived group termination"
        );
    }

    #[cfg(windows)]
    #[test]
    fn npm_installer_routes_cmd_shim_without_shell_text() {
        let command = npm_command_at(
            Path::new(r"C:\Program Files\nodejs\npm.cmd"),
            CODEX_CLI_INSTALL_ARGS,
        )
        .unwrap();
        let command = command.as_std();
        assert_eq!(command.get_program(), "cmd.exe");
        let args = command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(args[0..4], ["/D", "/S", "/C", "call"]);
        assert_eq!(args[4], r"C:\Program Files\nodejs\npm.cmd");
        assert_eq!(args[5..], ["install", "-g", "@openai/codex"]);
    }

    #[cfg(windows)]
    #[test]
    fn codex_exec_routes_npm_cmd_shim_through_node_and_permits_non_git_workspace() {
        let directory = TempDir::new().unwrap();
        let npm_dir = directory.path().join("npm");
        let shim = npm_dir.join("codex.cmd");
        let node = npm_dir.join("node.exe");
        let entrypoint = npm_dir
            .join("node_modules")
            .join("@openai")
            .join("codex")
            .join("bin")
            .join("codex.js");
        std::fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
        std::fs::write(&shim, "@echo off\r\n").unwrap();
        std::fs::write(&node, []).unwrap();
        std::fs::write(&entrypoint, "// test entrypoint\n").unwrap();

        let prompt = "inspect only";
        let command = codex_exec_command_with_permissions(
            Some(shim),
            directory.path(),
            CodexDelegationPermissions::read_only(),
            prompt,
        )
        .unwrap();
        let command = command.as_std();
        assert_eq!(command.get_program(), node.as_os_str());
        let args = command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(args[0], entrypoint.to_string_lossy());
        assert_eq!(
            args[1..],
            [
                "exec",
                "--json",
                "--skip-git-repo-check",
                "--sandbox",
                "read-only",
                "-c",
                "approval_policy=\"never\"",
                "-c",
                "approvals_reviewer=\"user\"",
                prompt,
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn codex_exec_uses_only_validated_permission_preset_arguments() {
        let permissions = CodexDelegationPermissions::from_mode(CodexPermissionMode::AutoReview)
            .expect("auto review must be a built-in preset");
        let command = codex_exec_command_with_permissions(
            Some(PathBuf::from(r"C:\Program Files\Codex\codex.exe")),
            Path::new(r"C:\repo"),
            permissions,
            "validated prompt",
        )
        .unwrap();
        assert_eq!(
            command.as_std().get_program(),
            Path::new(r"C:\Program Files\Codex\codex.exe")
        );
        let args = command
            .as_std()
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            [
                "exec",
                "--json",
                "--skip-git-repo-check",
                "--sandbox",
                "workspace-write",
                "-c",
                "approval_policy=\"on-request\"",
                "-c",
                "approvals_reviewer=\"auto_review\"",
                "validated prompt",
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn codex_exec_rejects_a_cmd_shim_without_the_expected_npm_entrypoint() {
        let directory = TempDir::new().unwrap();
        let shim = directory.path().join("codex.cmd");
        std::fs::write(&shim, "@echo off\r\n").unwrap();

        let error = codex_exec_command_with_permissions(
            Some(shim),
            directory.path(),
            CodexDelegationPermissions::read_only(),
            "inspect only",
        )
        .expect_err("an unknown cmd shim must not receive a delegated prompt");
        assert!(error.contains("npm"));
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
