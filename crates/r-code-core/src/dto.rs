//! 冻结的产品 DTO。
//!
//! 这些类型在 P0 阶段冻结，后续阶段不得破坏性修改。
//! 所有类型实现 `Serialize`/`Deserialize`，用于 SQLite 存储和 IPC 传输。

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ============================================================================
// Task DTO  [doc-06 §3.1] [doc-04 §6.1]
// ============================================================================

/// 任务 DTO —— R-Code 的核心实体。
///
/// 一个 Task 代表用户给 Agent 的一个目标。
/// 硬性不变量：一个 Task 任意时刻最多一个活跃 Agent Run。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Task {
    /// UUID v4
    pub id: String,
    /// 所属项目（workspace）的 canonical path
    pub project_id: String,
    /// 用户可见标题
    pub title: String,
    /// 用户输入的目标描述
    pub goal: String,
    /// 交互模式
    pub mode: TaskMode,
    /// 任务状态
    pub state: TaskState,
    /// Git worktree 路径（如有）
    pub worktree_path: Option<String>,
    /// 创建时间
    pub created_at: DateTime<Utc>,
    /// 最后更新时间
    pub updated_at: DateTime<Utc>,
}

impl Task {
    /// 创建新任务（状态默认 `Idle`）
    pub fn new(
        project_id: impl Into<String>,
        title: impl Into<String>,
        goal: impl Into<String>,
        mode: TaskMode,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            project_id: project_id.into(),
            title: title.into(),
            goal: goal.into(),
            mode,
            state: TaskState::Idle,
            worktree_path: None,
            created_at: now,
            updated_at: now,
        }
    }
}

/// 任务交互模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TaskMode {
    /// 只读问答模式
    #[default]
    Ask,
    /// 受控写入模式（需审批）
    Edit,
    /// 全自动模式（验证通过自动接受）
    Auto,
}

impl std::fmt::Display for TaskMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ask => write!(f, "ask"),
            Self::Edit => write!(f, "edit"),
            Self::Auto => write!(f, "auto"),
        }
    }
}

/// 任务状态。
///
/// 状态机：`Idle -> Exploring -> InProgress -> ReviewReady -> Idle (accept/rollback)`
/// 任意状态可以 `-> Archived`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// 空闲，未开始或已结束
    #[default]
    Idle,
    /// 探索中（Agent 正在读取文件、理解上下文）
    Exploring,
    /// 进行中（Agent 正在执行工具调用）
    InProgress,
    /// 待审查（Agent 完成一轮，等待用户接受/回滚）
    ReviewReady,
    /// 已归档（不可再操作）
    Archived,
}

impl std::fmt::Display for TaskState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Idle => write!(f, "idle"),
            Self::Exploring => write!(f, "exploring"),
            Self::InProgress => write!(f, "in_progress"),
            Self::ReviewReady => write!(f, "review_ready"),
            Self::Archived => write!(f, "archived"),
        }
    }
}

impl TaskState {
    /// 尝试从字符串解析
    pub fn try_from_str(s: &str) -> Option<Self> {
        match s {
            "idle" => Some(Self::Idle),
            "exploring" => Some(Self::Exploring),
            "in_progress" => Some(Self::InProgress),
            "review_ready" => Some(Self::ReviewReady),
            "archived" => Some(Self::Archived),
            _ => None,
        }
    }
}

// ============================================================================
// Agent Run DTO  [doc-06 §3.2]
// ============================================================================

/// Agent Run DTO —— 一次 Agent 执行的生命周期记录。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentRun {
    /// UUID v4
    pub id: String,
    /// 所属 Task ID
    pub task_id: String,
    /// 使用的模型名称
    pub model: String,
    /// 审查状态
    pub review_state: ReviewState,
    /// 开始时间
    pub started_at: DateTime<Utc>,
    /// 结束时间（None = 仍在运行）
    pub ended_at: Option<DateTime<Utc>>,
    /// Token 用量（JSON）
    pub usage_json: Option<String>,
}

impl AgentRun {
    /// 创建新的 Agent Run（审查状态默认 `Pending`）
    pub fn new(task_id: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            task_id: task_id.into(),
            model: model.into(),
            review_state: ReviewState::Pending,
            started_at: Utc::now(),
            ended_at: None,
            usage_json: None,
        }
    }

    /// 标记结束
    pub fn finish(&mut self, review_state: ReviewState) {
        self.review_state = review_state;
        self.ended_at = Some(Utc::now());
    }

    /// 是否仍在运行
    pub fn is_active(&self) -> bool {
        self.ended_at.is_none()
    }
}

/// Agent Run 审查状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    /// 待审查
    #[default]
    Pending,
    /// 用户已接受
    Accepted,
    /// 验证通过自动接受（Full 模式）
    AutoAccepted,
    /// 用户已回滚
    RolledBack,
    /// Ask 模式零变化轮次自动结算
    Answered,
}

impl std::fmt::Display for ReviewState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Accepted => write!(f, "accepted"),
            Self::AutoAccepted => write!(f, "auto_accepted"),
            Self::RolledBack => write!(f, "rolled_back"),
            Self::Answered => write!(f, "answered"),
        }
    }
}

impl ReviewState {
    /// 尝试从字符串解析
    pub fn try_from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "accepted" => Some(Self::Accepted),
            "auto_accepted" => Some(Self::AutoAccepted),
            "rolled_back" => Some(Self::RolledBack),
            "answered" => Some(Self::Answered),
            _ => None,
        }
    }

    /// 是否是终态（不可再转换）
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Accepted | Self::AutoAccepted | Self::RolledBack | Self::Answered
        )
    }
}

// ============================================================================
// Tool Call DTO  [doc-06 §3.3] [doc-02 §2.2]
// ============================================================================

/// Tool Call DTO —— 一次工具调用的完整审计记录。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    /// UUID v4
    pub id: String,
    /// 所属 Agent Run ID
    pub run_id: String,
    /// 所属 Task ID
    pub task_id: String,
    /// 工具名称
    pub tool_name: String,
    /// 输入参数（JSON）
    pub input_json: String,
    /// 输出结果（JSON）
    pub output_json: Option<String>,
    /// 风险等级
    pub risk_level: RiskLevel,
    /// 调用状态
    pub status: ToolCallStatus,
    /// 开始时间
    pub started_at: DateTime<Utc>,
    /// 结束时间
    pub ended_at: Option<DateTime<Utc>>,
    /// 调用者身份（task/session id 或 `terminal:<id>`）
    pub caller: Option<String>,
}

impl ToolCall {
    /// 创建新的 Tool Call 记录（状态默认 `Running`）
    pub fn new(
        run_id: impl Into<String>,
        task_id: impl Into<String>,
        tool_name: impl Into<String>,
        input_json: impl Into<String>,
        risk_level: RiskLevel,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            run_id: run_id.into(),
            task_id: task_id.into(),
            tool_name: tool_name.into(),
            input_json: input_json.into(),
            output_json: None,
            risk_level,
            status: ToolCallStatus::Running,
            started_at: Utc::now(),
            ended_at: None,
            caller: None,
        }
    }

    /// 标记成功
    pub fn succeed(&mut self, output: impl Into<String>) {
        self.output_json = Some(output.into());
        self.status = ToolCallStatus::Ok;
        self.ended_at = Some(Utc::now());
    }

    /// 标记失败
    pub fn fail(&mut self, error: impl Into<String>) {
        self.output_json = Some(serde_json::json!({ "error": error.into() }).to_string());
        self.status = ToolCallStatus::Error;
        self.ended_at = Some(Utc::now());
    }

    /// 标记被拒绝
    pub fn deny(&mut self, reason: impl Into<String>) {
        self.output_json = Some(serde_json::json!({ "denied": reason.into() }).to_string());
        self.status = ToolCallStatus::Denied;
        self.ended_at = Some(Utc::now());
    }
}

/// 风险等级 R0-R4。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "UPPERCASE")]
pub enum RiskLevel {
    /// 只读，无风险
    R0,
    /// 低风险，可能泄露信息
    R1,
    /// 中风险，可能修改状态
    R2,
    /// 高风险，不可逆操作
    R3,
    /// 前置拒绝
    #[default]
    R4,
}

impl std::fmt::Display for RiskLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::R0 => write!(f, "R0"),
            Self::R1 => write!(f, "R1"),
            Self::R2 => write!(f, "R2"),
            Self::R3 => write!(f, "R3"),
            Self::R4 => write!(f, "R4"),
        }
    }
}

impl RiskLevel {
    /// 尝试从字符串解析
    pub fn try_from_str(s: &str) -> Option<Self> {
        match s.to_uppercase().as_str() {
            "R0" => Some(Self::R0),
            "R1" => Some(Self::R1),
            "R2" => Some(Self::R2),
            "R3" => Some(Self::R3),
            "R4" => Some(Self::R4),
            _ => None,
        }
    }

    /// 是否需要用户确认
    pub fn requires_confirmation(&self) -> bool {
        matches!(self, Self::R2 | Self::R3 | Self::R4)
    }

    /// 是否可以持久化为 standing rule（R3 不持久化）
    pub fn can_persist_standing(&self) -> bool {
        !matches!(self, Self::R3 | Self::R4)
    }
}

/// Tool Call 状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolCallStatus {
    /// 正在执行
    Running,
    /// 成功
    Ok,
    /// 失败
    Error,
    /// 被拒绝（权限不足）
    Denied,
}

impl std::fmt::Display for ToolCallStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running => write!(f, "running"),
            Self::Ok => write!(f, "ok"),
            Self::Error => write!(f, "error"),
            Self::Denied => write!(f, "denied"),
        }
    }
}

impl ToolCallStatus {
    /// 尝试从字符串解析
    pub fn try_from_str(s: &str) -> Option<Self> {
        match s {
            "running" => Some(Self::Running),
            "ok" => Some(Self::Ok),
            "error" => Some(Self::Error),
            "denied" => Some(Self::Denied),
            _ => None,
        }
    }
}

// ============================================================================
// Permission Request DTO  [doc-02 §4.1] [doc-06 §3.7]
// ============================================================================

/// 权限请求 —— 高风险调用等待用户审批。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PermissionRequest {
    /// UUID v4
    pub id: String,
    /// 所属 Task ID
    pub task_id: String,
    /// 关联的 Tool Call ID
    pub tool_call_id: String,
    /// 工具名称
    pub tool_name: String,
    /// 风险等级
    pub risk_level: RiskLevel,
    /// 输入摘要（脱敏后）
    pub input_summary: String,
    /// 审批状态
    pub decision: PermissionDecision,
    /// 创建时间
    pub created_at: DateTime<Utc>,
    /// 决定时间
    pub decided_at: Option<DateTime<Utc>>,
}

impl PermissionRequest {
    /// 创建新的权限请求
    pub fn new(
        task_id: impl Into<String>,
        tool_call_id: impl Into<String>,
        tool_name: impl Into<String>,
        risk_level: RiskLevel,
        input_summary: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            task_id: task_id.into(),
            tool_call_id: tool_call_id.into(),
            tool_name: tool_name.into(),
            risk_level,
            input_summary: input_summary.into(),
            decision: PermissionDecision::Pending,
            created_at: Utc::now(),
            decided_at: None,
        }
    }

    /// 是否已决定
    pub fn is_decided(&self) -> bool {
        self.decision != PermissionDecision::Pending
    }
}

/// 权限决定。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    /// 待审批
    #[default]
    Pending,
    /// 批准（单次）
    Allow,
    /// 批准（本任务内 always allow）
    AllowAlways,
    /// 拒绝
    Deny,
}

// ============================================================================
// File Change / Baseline / Blob DTO  [doc-06 §3.4-3.6] [doc-12 §3]
// ============================================================================

/// 文件变更类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileChangeType {
    /// 新建文件
    Create,
    /// 修改文件
    Modify,
    /// 删除文件
    Delete,
    /// 重命名
    Rename,
}

impl std::fmt::Display for FileChangeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Create => write!(f, "create"),
            Self::Modify => write!(f, "modify"),
            Self::Delete => write!(f, "delete"),
            Self::Rename => write!(f, "rename"),
        }
    }
}

/// 文件变更记录。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileChange {
    /// UUID v4
    pub id: String,
    /// 所属 Task ID
    pub task_id: String,
    /// 关联的 Tool Call ID
    pub tool_call_id: Option<String>,
    /// 文件路径（相对于 workspace root）
    pub path: String,
    /// 变更类型
    pub change_type: FileChangeType,
    /// 变更前内容 hash（blake3）
    pub before_hash: Option<String>,
    /// 变更后内容 hash
    pub after_hash: Option<String>,
    /// 重命名时的旧路径
    pub old_path: Option<String>,
    /// 变更时间
    pub created_at: DateTime<Utc>,
}

impl FileChange {
    /// 创建新的文件变更记录
    pub fn new(
        task_id: impl Into<String>,
        path: impl Into<String>,
        change_type: FileChangeType,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            task_id: task_id.into(),
            tool_call_id: None,
            path: path.into(),
            change_type,
            before_hash: None,
            after_hash: None,
            old_path: None,
            created_at: Utc::now(),
        }
    }
}

/// 文件基线 —— 任务第一次可写动作前的文件快照。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileBaseline {
    /// UUID v4
    pub id: String,
    /// 所属 Task ID
    pub task_id: String,
    /// 文件路径（相对于 workspace root）
    pub path: String,
    /// 基线内容 hash（blake3）
    pub content_hash: String,
    /// Blob 存储 key（= content_hash）
    pub blob_key: String,
    /// 捕获时间
    pub captured_at: DateTime<Utc>,
}

impl FileBaseline {
    /// 创建新的文件基线
    pub fn new(
        task_id: impl Into<String>,
        path: impl Into<String>,
        content_hash: impl Into<String>,
    ) -> Self {
        let hash = content_hash.into();
        Self {
            id: Uuid::new_v4().to_string(),
            task_id: task_id.into(),
            path: path.into(),
            blob_key: hash.clone(),
            content_hash: hash,
            captured_at: Utc::now(),
        }
    }
}

/// Blob 引用计数记录。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BlobRef {
    /// 内容 hash（blake3），同时也是存储 key
    pub hash: String,
    /// 引用计数
    pub ref_count: u64,
    /// 首次写入时间
    pub created_at: DateTime<Utc>,
}

// ============================================================================
// Workspace DTO  [doc-06 §3.8]
// ============================================================================

/// Workspace 信任状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TrustState {
    /// 未信任
    #[default]
    Untrusted,
    /// 已信任
    Trusted,
}

/// Workspace 记录。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Workspace {
    /// canonical path（主键）
    pub canonical_path: String,
    /// 显示名称
    pub display_name: String,
    /// 信任状态
    pub trust_state: TrustState,
    /// 最后打开时间
    pub last_opened_at: DateTime<Utc>,
}

impl Workspace {
    /// 创建新的 workspace 记录
    pub fn new(canonical_path: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            canonical_path: canonical_path.into(),
            display_name: display_name.into(),
            trust_state: TrustState::Untrusted,
            last_opened_at: Utc::now(),
        }
    }
}

// ============================================================================
// Task Event DTO  [doc-06 §3.9] [doc-16 §2]
// ============================================================================

/// 任务事件类型（轻量投影，完整内容在 JSONL 中）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskEventType {
    /// 任务创建
    TaskCreated,
    /// 状态转换
    StateChanged,
    /// Agent Run 开始
    RunStarted,
    /// Agent Run 结束
    RunEnded,
    /// 工具调用
    ToolCall,
    /// 工具结果
    ToolResult,
    /// 权限请求
    PermissionRequested,
    /// 权限决定
    PermissionDecided,
    /// 文件变更
    FileChanged,
    /// 验证运行
    VerificationRun,
    /// 系统事件
    System,
}

impl std::fmt::Display for TaskEventType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TaskCreated => write!(f, "task_created"),
            Self::StateChanged => write!(f, "state_changed"),
            Self::RunStarted => write!(f, "run_started"),
            Self::RunEnded => write!(f, "run_ended"),
            Self::ToolCall => write!(f, "tool_call"),
            Self::ToolResult => write!(f, "tool_result"),
            Self::PermissionRequested => write!(f, "permission_requested"),
            Self::PermissionDecided => write!(f, "permission_decided"),
            Self::FileChanged => write!(f, "file_changed"),
            Self::VerificationRun => write!(f, "verification_run"),
            Self::System => write!(f, "system"),
        }
    }
}

/// 任务事件（轻量投影，只存 type + timestamp + task_id）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskEvent {
    /// 自增 ID
    pub id: i64,
    /// 所属 Task ID
    pub task_id: String,
    /// 事件类型
    pub event_type: TaskEventType,
    /// 事件时间
    pub created_at: DateTime<Utc>,
}

// ============================================================================
// ContentBlock::Custom schema  [agent-core/04 §2-3]
// ============================================================================

/// R-Code 产品专属 `ContentBlock::Custom` 类型名称。
pub mod custom_type_names {
    /// 文件引用块
    pub const FILE_REF: &str = "file_ref";
    /// 选区引用块
    pub const SELECTION_REF: &str = "selection_ref";
}

/// 创建 `file_ref` Custom 块的 data。
///
/// Schema: `{ "path": "...", "line": 42 }`
pub fn file_ref_data(path: impl Into<String>, line: Option<u32>) -> serde_json::Value {
    let mut data = serde_json::json!({ "path": path.into() });
    if let Some(line) = line {
        data["line"] = serde_json::json!(line);
    }
    data
}

/// 创建 `selection_ref` Custom 块的 data。
///
/// Schema: `{ "path": "...", "start": 10, "end": 20, "hash": "..." }`
pub fn selection_ref_data(
    path: impl Into<String>,
    start: u32,
    end: u32,
    hash: impl Into<String>,
) -> serde_json::Value {
    serde_json::json!({
        "path": path.into(),
        "start": start,
        "end": end,
        "hash": hash.into(),
    })
}

// ============================================================================
// AgentEvent  [doc-04 §6]
// ============================================================================

/// Agent 事件 —— Worker -> Main 的事件流。
///
/// 映射自 `hermes_llm::StreamEvent`，不重新定义流式事件格式。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// 文本消息（增量 delta 或完整）
    Message {
        /// 文本内容
        text: String,
        /// 是否为增量
        delta: bool,
    },
    /// 工具调用
    ToolCall {
        /// 工具名称
        name: String,
        /// 输入参数
        input: serde_json::Value,
        /// 调用 ID
        call_id: String,
    },
    /// 工具结果
    ToolResult {
        /// 调用 ID
        call_id: String,
        /// 输出结果
        output: serde_json::Value,
        /// 是否为错误
        is_error: bool,
    },
    /// Agent 计划
    Plan {
        /// 计划步骤
        steps: Vec<PlanStep>,
    },
    /// 状态变更
    State {
        /// 新状态
        state: TaskState,
    },
}

/// 计划步骤。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlanStep {
    /// 步骤描述
    pub description: String,
    /// 是否已完成
    pub completed: bool,
}

// ============================================================================
// CreateSessionInput  [doc-04 §4.1]
// ============================================================================

/// 创建会话输入。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSessionInput {
    /// 项目 ID
    pub project_id: String,
    /// 目标
    pub goal: String,
    /// 交互模式
    pub mode: TaskMode,
    /// 模型名称（可选，使用默认）
    pub model: Option<String>,
    /// 上下文引用（可选）
    pub context: Vec<ContextRef>,
}

impl Default for CreateSessionInput {
    fn default() -> Self {
        Self {
            project_id: String::new(),
            goal: String::new(),
            mode: TaskMode::Ask,
            model: None,
            context: Vec::new(),
        }
    }
}

/// 上下文引用。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContextRef {
    /// 文件引用
    FileRef {
        /// 文件路径
        path: String,
        /// 行号（可选）
        line: Option<u32>,
    },
    /// 选区引用
    SelectionRef {
        /// 文件路径
        path: String,
        /// 起始行
        start: u32,
        /// 结束行
        end: u32,
    },
}

// ============================================================================
// Verification DTO  [doc-18 M9]
// ============================================================================

/// 验证结果状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    /// 运行中
    Running,
    /// 通过
    Passed,
    /// 失败
    Failed,
    /// 被取代（新验证运行取代旧的）
    Superseded,
    /// 过期（文件已变更）
    Stale,
    /// 超时
    Timeout,
}

impl std::fmt::Display for VerificationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Running => write!(f, "running"),
            Self::Passed => write!(f, "passed"),
            Self::Failed => write!(f, "failed"),
            Self::Superseded => write!(f, "superseded"),
            Self::Stale => write!(f, "stale"),
            Self::Timeout => write!(f, "timeout"),
        }
    }
}

/// 验证记录。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VerificationRecord {
    /// UUID v4
    pub id: String,
    /// 所属 Task ID
    pub task_id: String,
    /// 所属 Run ID
    pub run_id: String,
    /// 验证命令
    pub command: String,
    /// 验证状态
    pub status: VerificationStatus,
    /// 输出 blob key（内容寻址）
    pub output_blob_key: Option<String>,
    /// 退出码
    pub exit_code: Option<i32>,
    /// 开始时间
    pub started_at: DateTime<Utc>,
    /// 结束时间
    pub ended_at: Option<DateTime<Utc>>,
}

impl VerificationRecord {
    /// 创建新的验证记录
    pub fn new(
        task_id: impl Into<String>,
        run_id: impl Into<String>,
        command: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            task_id: task_id.into(),
            run_id: run_id.into(),
            command: command.into(),
            status: VerificationStatus::Running,
            output_blob_key: None,
            exit_code: None,
            started_at: Utc::now(),
            ended_at: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_new_defaults() {
        let task = Task::new("/proj", "Test", "Do something", TaskMode::Ask);
        assert_eq!(task.state, TaskState::Idle);
        assert_eq!(task.mode, TaskMode::Ask);
        assert!(task.worktree_path.is_none());
    }

    #[test]
    fn task_state_display_roundtrip() {
        for state in [
            TaskState::Idle,
            TaskState::Exploring,
            TaskState::InProgress,
            TaskState::ReviewReady,
            TaskState::Archived,
        ] {
            let s = state.to_string();
            assert_eq!(TaskState::try_from_str(&s), Some(state));
        }
    }

    #[test]
    fn review_state_is_terminal() {
        assert!(!ReviewState::Pending.is_terminal());
        assert!(ReviewState::Accepted.is_terminal());
        assert!(ReviewState::AutoAccepted.is_terminal());
        assert!(ReviewState::RolledBack.is_terminal());
        assert!(ReviewState::Answered.is_terminal());
    }

    #[test]
    fn risk_level_requires_confirmation() {
        assert!(!RiskLevel::R0.requires_confirmation());
        assert!(!RiskLevel::R1.requires_confirmation());
        assert!(RiskLevel::R2.requires_confirmation());
        assert!(RiskLevel::R3.requires_confirmation());
        assert!(RiskLevel::R4.requires_confirmation());
    }

    #[test]
    fn risk_level_can_persist_standing() {
        assert!(RiskLevel::R0.can_persist_standing());
        assert!(RiskLevel::R1.can_persist_standing());
        assert!(RiskLevel::R2.can_persist_standing());
        assert!(!RiskLevel::R3.can_persist_standing());
        assert!(!RiskLevel::R4.can_persist_standing());
    }

    #[test]
    fn tool_call_lifecycle() {
        let mut tc = ToolCall::new("run1", "task1", "read_file", "{}", RiskLevel::R1);
        assert_eq!(tc.status, ToolCallStatus::Running);
        tc.succeed("{\"content\":\"hello\"}");
        assert_eq!(tc.status, ToolCallStatus::Ok);
        assert!(tc.output_json.is_some());
        assert!(tc.ended_at.is_some());
    }

    #[test]
    fn agent_run_lifecycle() {
        let mut run = AgentRun::new("task1", "claude-3-5-sonnet");
        assert!(run.is_active());
        assert_eq!(run.review_state, ReviewState::Pending);
        run.finish(ReviewState::Accepted);
        assert!(!run.is_active());
        assert_eq!(run.review_state, ReviewState::Accepted);
    }

    #[test]
    fn custom_block_file_ref() {
        let data = file_ref_data("/src/main.rs", Some(42));
        assert_eq!(data["path"], "/src/main.rs");
        assert_eq!(data["line"], 42);
    }

    #[test]
    fn custom_block_selection_ref() {
        let data = selection_ref_data("/src/lib.rs", 10, 20, "abc123");
        assert_eq!(data["path"], "/src/lib.rs");
        assert_eq!(data["start"], 10);
        assert_eq!(data["end"], 20);
        assert_eq!(data["hash"], "abc123");
    }
}
