//! 冻结的产品 DTO。
//!
//! 这些类型在 P0 阶段冻结，后续阶段不得破坏性修改。
//! 所有类型实现 `Serialize`/`Deserialize`，用于 SQLite 存储和 IPC 传输。

use chrono::{DateTime, Utc};
use hermes_core::InferenceOptions;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

fn default_branch_id() -> String {
    "main".to_string()
}

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
    /// 附加工作区的 canonical path。为空时这是一个纯聊天会话，不提供本地工具。
    #[serde(default, alias = "project_id")]
    pub workspace_path: Option<String>,
    /// 本会话绑定的模型服务配置名。为空时兼容旧会话，运行时回退到全局默认服务。
    #[serde(default)]
    pub provider_name: Option<String>,
    /// 本会话绑定的具体模型。为空时使用该服务在设置里配置的默认模型。
    #[serde(default)]
    pub model: Option<String>,
    /// 本会话绑定的模型专属推理参数；空对象表示沿用模型服务默认值。
    #[serde(default)]
    pub inference: InferenceOptions,
    /// 本会话的主 Agent 执行引擎。旧会话安全地回退到 R-Code 内置 Agent。
    #[serde(default)]
    pub agent_engine: AgentEngine,
    /// 用户可见标题
    pub title: String,
    /// 用户输入的目标描述
    pub goal: String,
    /// 用户是否显式启用了可持续执行、编辑、停止和删除的 Goal。
    ///
    /// 普通新对话也会把首条任务描述保存在 `goal` 中，供标题回退与 Plan 上下文使用；
    /// 只有该标记为 true 时，前端才展示 Goal 生命周期控件。
    #[serde(default)]
    pub goal_active: bool,
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
        workspace_path: Option<String>,
        title: impl Into<String>,
        goal: impl Into<String>,
        mode: TaskMode,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            workspace_path,
            provider_name: None,
            model: None,
            inference: InferenceOptions::default(),
            agent_engine: AgentEngine::RCode,
            title: title.into(),
            goal: goal.into(),
            goal_active: false,
            mode,
            state: TaskState::Idle,
            worktree_path: None,
            created_at: now,
            updated_at: now,
        }
    }
}

/// 会话级主 Agent 引擎。
///
/// 该选择只决定谁负责主循环；两种引擎都可通过显式、可审计的委派调用另一种
/// Agent 作为子智能体。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentEngine {
    /// R-Code 自研 provider runtime。
    #[default]
    RCode,
    /// 本机已登录的官方 Codex CLI。
    Codex,
}

impl std::fmt::Display for AgentEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RCode => write!(f, "r_code"),
            Self::Codex => write!(f, "codex"),
        }
    }
}

impl AgentEngine {
    pub fn try_from_str(value: &str) -> Option<Self> {
        match value {
            "r_code" | "native" => Some(Self::RCode),
            "codex" | "codex_cli" => Some(Self::Codex),
            _ => None,
        }
    }
}

/// 持久化的任务运行策略。产品界面把 Ask/Edit/Auto 统一呈现为 Agent，只把 Plan
/// 作为另一种交互模式；工作区权限和主 Agent 引擎分别独立配置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TaskMode {
    /// Agent 的只读策略（兼容旧会话与无写权限任务）。
    #[default]
    Ask,
    /// Agent 的受控写入策略（需审批）。
    Edit,
    /// Agent 的自动执行策略（也用于已批准 Plan 的实施）。
    Auto,
    /// 只读规划模式；计划批准后由运行时显式切回执行能力。
    Plan,
}

impl std::fmt::Display for TaskMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ask => write!(f, "ask"),
            Self::Edit => write!(f, "edit"),
            Self::Auto => write!(f, "auto"),
            Self::Plan => write!(f, "plan"),
        }
    }
}

impl TaskMode {
    /// 尝试从持久化字符串解析。
    pub fn try_from_str(value: &str) -> Option<Self> {
        match value {
            "ask" => Some(Self::Ask),
            "edit" => Some(Self::Edit),
            "auto" => Some(Self::Auto),
            "plan" => Some(Self::Plan),
            _ => None,
        }
    }
}

/// 任务状态。
///
/// 状态机：`Idle -> Exploring -> InProgress -> ReviewReady -> Idle (accept/rollback)`；
/// 用户可将运行转为 `Interrupted`，后续可直接分发排队消息。
/// 任意状态可以 `-> Archived`，归档后可由用户显式还原为 `Idle`。
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
    /// 已由用户中止，保留中止原因与运行审计记录
    Interrupted,
    /// 待审查（Agent 完成一轮，等待用户接受/回滚）
    ReviewReady,
    /// 已归档（只读；还原后可继续操作）
    Archived,
}

impl std::fmt::Display for TaskState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Idle => write!(f, "idle"),
            Self::Exploring => write!(f, "exploring"),
            Self::InProgress => write!(f, "in_progress"),
            Self::Interrupted => write!(f, "interrupted"),
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
            "interrupted" => Some(Self::Interrupted),
            "review_ready" => Some(Self::ReviewReady),
            "archived" => Some(Self::Archived),
            _ => None,
        }
    }
}

// ============================================================================
// Agent Run DTO  [doc-06 §3.2]
// ============================================================================

/// Agent Run 的角色。
///
/// 主运行直接服务于任务；子代理运行必须关联到发起它的主或子运行，
/// 以便完整重建委派树。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentKind {
    /// 任务的主 Agent 运行。
    #[default]
    Main,
    /// 由另一个 Agent 委派的子代理运行。
    Subagent,
}

impl std::fmt::Display for AgentKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Main => write!(f, "main"),
            Self::Subagent => write!(f, "subagent"),
        }
    }
}

impl AgentKind {
    /// 尝试从持久化字符串解析。
    pub fn try_from_str(value: &str) -> Option<Self> {
        match value {
            "main" => Some(Self::Main),
            "subagent" => Some(Self::Subagent),
            _ => None,
        }
    }
}

/// Agent Run 的执行驱动。
///
/// `agent_kind` 描述它在运行树中的位置（主代理 / 子代理），本枚举描述实际由谁
/// 执行。两者刻意分离：例如 Codex CLI 可以是 R-Code 主运行委派出的子代理。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunRuntimeKind {
    /// R-Code 内置 provider runtime。
    #[default]
    Native,
    /// 非交互式 Codex CLI（`codex exec --json`）。
    CodexExec,
    /// 以 MCP 会话方式运行的 Codex CLI（`codex mcp-server`）。
    CodexMcp,
}

impl std::fmt::Display for AgentRunRuntimeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Native => write!(f, "native"),
            Self::CodexExec => write!(f, "codex_exec"),
            Self::CodexMcp => write!(f, "codex_mcp"),
        }
    }
}

impl AgentRunRuntimeKind {
    /// 尝试从持久化字符串解析。
    pub fn try_from_str(value: &str) -> Option<Self> {
        match value {
            "native" => Some(Self::Native),
            "codex_exec" => Some(Self::CodexExec),
            "codex_mcp" => Some(Self::CodexMcp),
            _ => None,
        }
    }
}

/// 子智能体在一次委派中获得的工作区能力。
///
/// 委派默认只读；只有主智能体根据用户对话或明确的父任务要求传入
/// `full_access` 时，子智能体才可使用写入与命令工具。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SubagentAccessMode {
    #[default]
    ReadOnly,
    FullAccess,
}

impl std::fmt::Display for SubagentAccessMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadOnly => write!(f, "read_only"),
            Self::FullAccess => write!(f, "full_access"),
        }
    }
}

/// Agent Run DTO —— 一次 Agent 执行的生命周期记录。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentRun {
    /// UUID v4
    pub id: String,
    /// 所属 Task ID
    pub task_id: String,
    /// 所属会话分支；历史数据库迁移后缺省为 main
    #[serde(default = "default_branch_id")]
    pub branch_id: String,
    /// 父运行 ID；主运行没有父运行。
    #[serde(default)]
    pub parent_run_id: Option<String>,
    /// 运行角色；旧 JSON 缺省为主运行。
    #[serde(default)]
    pub agent_kind: AgentKind,
    /// 子代理的人类可读标签。
    #[serde(default)]
    pub agent_label: Option<String>,
    /// 子代理完成后的受限结果摘要；绝不保存或展示模型私有推理。
    #[serde(default)]
    pub summary: Option<String>,
    /// 触发这次委派的工具调用 ID。
    #[serde(default)]
    pub delegated_by_tool_call_id: Option<String>,
    /// 使用的模型名称
    pub model: String,
    /// 实际执行此运行的驱动；历史记录缺省为 R-Code 内置 runtime。
    #[serde(default)]
    pub runtime_kind: AgentRunRuntimeKind,
    /// 子智能体工作区能力；主运行保留默认值，仅用于统一 IPC 形状。
    #[serde(default)]
    pub access_mode: SubagentAccessMode,
    /// `FullAccess` 能力档位是否仍由宿主审批钳制。
    ///
    /// 旧运行缺省为 false；`ReadOnly` 运行忽略此位。
    #[serde(default)]
    pub require_approval: bool,
    /// 自动路由或显式选择该执行器的可见原因，不包含模型私有推理。
    #[serde(default)]
    pub routing_reason: Option<String>,
    /// 外部 Agent 的可恢复会话标识（例如 Codex thread ID）。
    ///
    /// 它不是凭据，也不会保存外部 Agent 的完整转录。
    #[serde(default)]
    pub external_session_id: Option<String>,
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
        Self::new_for_branch(task_id, "main", model)
    }

    /// 在指定会话分支中创建新的 Agent Run。
    pub fn new_for_branch(
        task_id: impl Into<String>,
        branch_id: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            task_id: task_id.into(),
            branch_id: branch_id.into(),
            parent_run_id: None,
            agent_kind: AgentKind::Main,
            agent_label: None,
            summary: None,
            delegated_by_tool_call_id: None,
            model: model.into(),
            runtime_kind: AgentRunRuntimeKind::Native,
            access_mode: SubagentAccessMode::ReadOnly,
            require_approval: false,
            routing_reason: None,
            external_session_id: None,
            review_state: ReviewState::Pending,
            started_at: Utc::now(),
            ended_at: None,
            usage_json: None,
        }
    }

    /// 在主分支创建子代理运行。
    pub fn new_subagent(
        task_id: impl Into<String>,
        parent_run_id: impl Into<String>,
        model: impl Into<String>,
        agent_label: Option<String>,
        delegated_by_tool_call_id: Option<String>,
    ) -> Self {
        Self::new_subagent_for_branch(
            task_id,
            "main",
            parent_run_id,
            model,
            agent_label,
            delegated_by_tool_call_id,
        )
    }

    /// 在指定会话分支创建子代理运行。
    pub fn new_subagent_for_branch(
        task_id: impl Into<String>,
        branch_id: impl Into<String>,
        parent_run_id: impl Into<String>,
        model: impl Into<String>,
        agent_label: Option<String>,
        delegated_by_tool_call_id: Option<String>,
    ) -> Self {
        let mut run = Self::new_for_branch(task_id, branch_id, model);
        run.parent_run_id = Some(parent_run_id.into());
        run.agent_kind = AgentKind::Subagent;
        run.agent_label = agent_label;
        run.delegated_by_tool_call_id = delegated_by_tool_call_id;
        run
    }

    /// 标记为由外部 Codex CLI 执行的只读子代理。
    pub fn as_codex_exec_subagent(mut self) -> Self {
        self.runtime_kind = AgentRunRuntimeKind::CodexExec;
        self
    }

    /// 标记为由 Codex MCP 会话执行的只读子代理。
    pub fn as_codex_mcp_subagent(mut self) -> Self {
        self.runtime_kind = AgentRunRuntimeKind::CodexMcp;
        self
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
    /// 用户主动中止，未把它误记为文件回滚
    Aborted,
    /// Ask 模式零变化轮次自动结算
    Answered,
    /// 子代理或 provider 在运行期间失败；与用户主动中止区分。
    Failed,
}

impl std::fmt::Display for ReviewState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Accepted => write!(f, "accepted"),
            Self::AutoAccepted => write!(f, "auto_accepted"),
            Self::RolledBack => write!(f, "rolled_back"),
            Self::Aborted => write!(f, "aborted"),
            Self::Answered => write!(f, "answered"),
            Self::Failed => write!(f, "failed"),
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
            "aborted" => Some(Self::Aborted),
            "answered" => Some(Self::Answered),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }

    /// 是否是终态（不可再转换）
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Accepted
                | Self::AutoAccepted
                | Self::RolledBack
                | Self::Aborted
                | Self::Answered
                | Self::Failed
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
    /// 发起请求的 Agent Run ID；旧记录可能缺失。
    #[serde(default)]
    pub run_id: Option<String>,
    /// 调用者身份（例如 `agent` 或 `subagent:<id>`）；旧记录可能缺失。
    #[serde(default)]
    pub caller: Option<String>,
    /// 工具名称
    pub tool_name: String,
    /// 风险等级
    pub risk_level: RiskLevel,
    /// 输入摘要（脱敏后）
    pub input_summary: String,
    /// Standing-rule 作用目标；缺失表示当前工具的任务级通配规则。
    /// 旧记录没有此字段，因此保持可选并默认缺失。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
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
            run_id: None,
            caller: None,
            tool_name: tool_name.into(),
            risk_level,
            input_summary: input_summary.into(),
            target: None,
            decision: PermissionDecision::Pending,
            created_at: Utc::now(),
            decided_at: None,
        }
    }

    /// 补充运行归属，用于审批界面解释请求来源。
    pub fn with_origin(
        mut self,
        run_id: Option<impl Into<String>>,
        caller: Option<impl Into<String>>,
    ) -> Self {
        self.run_id = run_id.map(Into::into);
        self.caller = caller.map(Into::into);
        self
    }

    /// 绑定 standing-rule 的最小授权目标，防止聚合工具的任务级通配授权。
    pub fn with_target(mut self, target: Option<impl Into<String>>) -> Self {
        self.target = target.map(Into::into);
        self
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

/// 项目级 Agent 权限模式。
///
/// 所有模式都只允许访问已附加工作区；差异仅在 Agent 调用本地工具时如何审批。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProjectAccessMode {
    /// 每次可能泄露信息、修改文件或执行命令时都请求批准。
    #[default]
    RequestApproval,
    /// 仅在风险分级判断为中高风险时请求批准。
    RiskBased,
    /// 自动批准 R0-R3；R4 和显式拒绝规则仍然生效。
    FullAccess,
}

/// 项目记忆相对全局设置的生效模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMemoryMode {
    /// 跟随全局记忆开关与策略。
    #[default]
    Inherit,
    /// 允许读取已有记忆，但不再从该项目产生新记忆。
    ReadOnly,
    /// 该项目完全不读取也不产生记忆。
    Off,
}

/// Workspace 记录。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Workspace {
    /// 不随路径变化的本地 owner key。
    pub id: String,
    /// canonical path（唯一查找键，不作为 memory owner key）
    pub canonical_path: String,
    /// 显示名称
    pub display_name: String,
    /// 项目级 Agent 权限模式
    pub access_mode: ProjectAccessMode,
    /// 最后打开时间
    pub last_opened_at: DateTime<Utc>,
    /// 项目记忆模式。
    #[serde(default)]
    pub memory_mode: WorkspaceMemoryMode,
    /// 项目记忆失效屏障；模式变化时单调递增。
    #[serde(default = "initial_memory_generation")]
    pub memory_generation: u64,
}

const fn initial_memory_generation() -> u64 {
    1
}

impl Workspace {
    /// 创建新的 workspace 记录
    pub fn new(canonical_path: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().simple().to_string(),
            canonical_path: canonical_path.into(),
            display_name: display_name.into(),
            access_mode: ProjectAccessMode::RequestApproval,
            last_opened_at: Utc::now(),
            memory_mode: WorkspaceMemoryMode::Inherit,
            memory_generation: initial_memory_generation(),
        }
    }
}

// ============================================================================
// Notification DTO
// ============================================================================

/// 顶栏通知的类别。
///
/// 通知是用户需要回看的产品级记录；它与任务事件不同，支持已读状态，且会跨越
/// 页面刷新和应用重启保留。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationKind {
    /// Agent 正在等待一项权限决定。
    PermissionRequested,
    /// 一轮任务已经完成，等待用户审查变更。
    ReviewReady,
    /// 用户已对审查结果提出修改要求。
    ChangeRequested,
    /// 全局记忆候选等待用户明确批准。
    MemoryApprovalRequired,
    /// 项目记忆已经由复盘任务自动更新。
    MemoryProjectUpdated,
}

impl std::fmt::Display for NotificationKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PermissionRequested => write!(f, "permission_requested"),
            Self::ReviewReady => write!(f, "review_ready"),
            Self::ChangeRequested => write!(f, "change_requested"),
            Self::MemoryApprovalRequired => write!(f, "memory_approval_required"),
            Self::MemoryProjectUpdated => write!(f, "memory_project_updated"),
        }
    }
}

impl NotificationKind {
    /// 从持久化字符串恢复通知类别。
    pub fn try_from_str(value: &str) -> Option<Self> {
        match value {
            "permission_requested" => Some(Self::PermissionRequested),
            "review_ready" => Some(Self::ReviewReady),
            "change_requested" => Some(Self::ChangeRequested),
            "memory_approval_required" => Some(Self::MemoryApprovalRequired),
            "memory_project_updated" => Some(Self::MemoryProjectUpdated),
            _ => None,
        }
    }
}

/// 一条可已读的用户通知。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Notification {
    /// 稳定 UUID。
    pub id: String,
    /// 通知类别。
    pub kind: NotificationKind,
    /// 短标题。
    pub title: String,
    /// 可直接显示的正文摘要。
    pub body: String,
    /// 相关任务；无任务的系统通知可为 None。
    pub task_id: Option<String>,
    /// 相关工作区；纯聊天任务可为 None。
    pub workspace_path: Option<String>,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
    /// 已读时间；None 表示未读。
    pub read_at: Option<DateTime<Utc>>,
}

impl Notification {
    /// 创建一条未读通知。
    pub fn new(
        kind: NotificationKind,
        title: impl Into<String>,
        body: impl Into<String>,
        task_id: Option<String>,
        workspace_path: Option<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            kind,
            title: title.into(),
            body: body.into(),
            task_id,
            workspace_path,
            created_at: Utc::now(),
            read_at: None,
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
    /// 运行中的补充指令已注入当前会话
    UserSteered,
    /// 用户消息已持久化到待发送队列
    UserMessageQueued,
    /// 队列消息开始分发为新的运行
    QueueDispatched,
    /// 用户主动中止了一次运行
    RunAborted,
    /// 用户消息编辑后创建了新的会话分支
    SessionBranched,
    /// 受委派子代理已创建或开始排队。
    SubagentStarted,
    /// 受委派子代理进入终态。
    SubagentFinished,
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
    /// 用户在审查阶段请求继续修改。
    ChangeRequested,
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
            Self::UserSteered => write!(f, "user_steered"),
            Self::UserMessageQueued => write!(f, "user_message_queued"),
            Self::QueueDispatched => write!(f, "queue_dispatched"),
            Self::RunAborted => write!(f, "run_aborted"),
            Self::SessionBranched => write!(f, "session_branched"),
            Self::SubagentStarted => write!(f, "subagent_started"),
            Self::SubagentFinished => write!(f, "subagent_finished"),
            Self::ToolCall => write!(f, "tool_call"),
            Self::ToolResult => write!(f, "tool_result"),
            Self::PermissionRequested => write!(f, "permission_requested"),
            Self::PermissionDecided => write!(f, "permission_decided"),
            Self::FileChanged => write!(f, "file_changed"),
            Self::VerificationRun => write!(f, "verification_run"),
            Self::ChangeRequested => write!(f, "change_requested"),
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
    /// 所属会话分支；旧审计事件缺省归入 main
    #[serde(default = "default_branch_id")]
    pub branch_id: String,
    /// 事件类型
    pub event_type: TaskEventType,
    /// 事件时间
    pub created_at: DateTime<Utc>,
}

// ============================================================================
// 会话分支与运行控制
// ============================================================================

/// 用户发消息时选择的显式动作。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AgentSendMode {
    /// 由服务端根据当前任务和运行状态选择安全默认行为。
    #[default]
    Auto,
    /// 注入当前运行，在下一次 agent 迭代前生效，不新建 AgentRun。
    Steer,
    /// 追加到当前任务的持久化待发送队列。
    Queue,
    /// 安全中止当前运行后，优先分发这条消息。
    SendNow,
}

impl AgentSendMode {
    pub fn try_from_str(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "steer" => Some(Self::Steer),
            "queue" => Some(Self::Queue),
            "send_now" => Some(Self::SendNow),
            _ => None,
        }
    }
}

/// 会话分支元数据。每个任务恰有一个活跃分支，历史分支与其 JSONL 日志保持只读。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionBranch {
    /// 分支 ID；主分支固定为 main，编辑分支为 UUID。
    pub id: String,
    /// 所属任务。
    pub task_id: String,
    /// 父分支；主分支没有父分支。
    pub parent_branch_id: Option<String>,
    /// 分叉前最后保留的用户消息 ID（JSONL 行稳定标识）。
    pub forked_from_message_id: Option<String>,
    /// JSONL 会话文件的存储 ID，不等同于运行时 session ID。
    pub storage_id: String,
    /// 是否为用户当前正在查看和继续的分支。
    pub is_active: bool,
    /// 创建时间。
    pub created_at: DateTime<Utc>,
}

impl SessionBranch {
    /// 为既有或新建任务创建主分支。
    pub fn main(task_id: impl Into<String>) -> Self {
        let task_id = task_id.into();
        Self {
            id: "main".to_string(),
            storage_id: task_id.clone(),
            task_id,
            parent_branch_id: None,
            forked_from_message_id: None,
            is_active: true,
            created_at: Utc::now(),
        }
    }

    /// 从现有消息创建新的活跃分支。
    pub fn fork(
        task_id: impl Into<String>,
        parent_branch_id: impl Into<String>,
        forked_from_message_id: impl Into<String>,
    ) -> Self {
        let task_id = task_id.into();
        let id = Uuid::new_v4().to_string();
        Self {
            storage_id: format!("{task_id}--{id}"),
            id,
            task_id,
            parent_branch_id: Some(parent_branch_id.into()),
            forked_from_message_id: Some(forked_from_message_id.into()),
            is_active: true,
            created_at: Utc::now(),
        }
    }
}

/// 待发送消息的持久化状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum QueuedMessageState {
    /// 等待当前运行结束或运行调度器空闲。
    #[default]
    Queued,
    /// 正在从队列转交给 runtime。
    Dispatching,
    /// 已交给新的 AgentRun。
    Sent,
    /// 用户明确移除或中止时取消。
    Cancelled,
    /// runtime 无法启动，保留可解释的失败记录。
    Failed,
}

impl std::fmt::Display for QueuedMessageState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Queued => write!(f, "queued"),
            Self::Dispatching => write!(f, "dispatching"),
            Self::Sent => write!(f, "sent"),
            Self::Cancelled => write!(f, "cancelled"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

impl QueuedMessageState {
    pub fn try_from_str(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "dispatching" => Some(Self::Dispatching),
            "sent" => Some(Self::Sent),
            "cancelled" => Some(Self::Cancelled),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// 任务级待发送消息。排队时尚未写入会话 JSONL，真正分发时才成为用户消息。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QueuedMessage {
    pub id: String,
    pub task_id: String,
    pub branch_id: String,
    pub message: String,
    pub state: QueuedMessageState,
    /// 数字越大越优先；“立即发送”使用更高优先级。
    pub priority: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl QueuedMessage {
    pub fn new(
        task_id: impl Into<String>,
        branch_id: impl Into<String>,
        message: impl Into<String>,
        priority: i64,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: Uuid::new_v4().to_string(),
            task_id: task_id.into(),
            branch_id: branch_id.into(),
            message: message.into(),
            state: QueuedMessageState::Queued,
            priority,
            created_at: now,
            updated_at: now,
        }
    }
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
    /// 面向用户的运行活动阶段。
    ///
    /// 仅表达请求、生成、工具和引导等可观察状态；绝不承载模型私有推理文本。
    Activity {
        /// 当前活动阶段
        phase: AgentActivityPhase,
        /// 可选的安全摘要（例如工具目标）；缺省时由前端按阶段本地化展示。
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// 由子代理产生的带运行作用域事件。主代理继续使用既有顶层载荷，因此旧 IPC
    /// 客户端仍可读取；新客户端可按 scope 将子代理活动折叠到 Working 列表。
    Scoped {
        scope: AgentEventScope,
        event: Box<AgentEvent>,
    },
    /// 子代理的结构化生命周期变化。只包含可观察状态和受限摘要，不承载思维链。
    SubagentLifecycle {
        state: SubagentState,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// 单次模型请求的 token 用量（agent loop 单轮内累计）。
    ///
    /// 键与 `hermes_core::Usage` 的 serde 输出一致（`input_tokens` /
    /// `output_tokens` / `cache_read_tokens` / `cache_write_tokens`），宿主据此写入
    /// `AgentRun.usage_json`，前端 `runUsageLabel` 直接解析；与 Codex 线路写入
    /// 同一列的 JSON 形状保持一致。仅当 provider 报告非零用量时发出；会话
    /// JSONL 与 WebView 事件流不消费此事件。
    Usage {
        /// 与 Codex 路径写入 `usage_json` 列相同形状的 JSON 文本。
        usage_json: String,
    },
}

/// 嵌套 Agent 事件的运行树作用域。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentEventScope {
    /// 此运行的稳定 ID（与 `AgentRun.id` 对齐）。
    pub run_id: String,
    /// Agent 实例 ID；当前实现与 run ID 相同，保留独立字段以兼容未来复用。
    pub agent_id: String,
    /// 父运行 ID；主运行没有父级。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_run_id: Option<String>,
    /// Agent 角色。
    #[serde(default)]
    pub agent_kind: AgentKind,
    /// 用户可见的子代理标签。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_label: Option<String>,
    /// 触发此委派的父代理工具调用 ID。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delegated_by_tool_call_id: Option<String>,
    /// 实际执行此子运行的驱动；旧事件缺省为 R-Code 内置 runtime。
    #[serde(default)]
    pub runtime_kind: AgentRunRuntimeKind,
    /// 此子运行使用的模型或执行器标签；旧事件可能没有。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// 本次委派的能力边界；旧事件安全地按只读处理。
    #[serde(default)]
    pub access_mode: SubagentAccessMode,
    /// 能力档位之外的审批钳制（M7）：`access_mode = FullAccess` 且此位为 true 时
    /// 表示"审批模式"（inherit 自 RequestApproval 等非全权父运行）。审计锚点据
    /// 此记录 effective access；旧事件缺省 false（无审批钳制）。
    #[serde(default)]
    pub require_approval: bool,
    /// 为什么选择此子智能体执行器。仅记录策略结论，不记录思维链。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing_reason: Option<String>,
}

/// 子代理的可观察生命周期状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentState {
    Queued,
    Running,
    WaitingPermission,
    Completed,
    Failed,
    Cancelled,
}

impl std::fmt::Display for SubagentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Queued => write!(f, "queued"),
            Self::Running => write!(f, "running"),
            Self::WaitingPermission => write!(f, "waiting_permission"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

/// Agent 运行中的可观察活动阶段。
///
/// 与任务状态不同：该枚举用于实时交互提示，不写入任务状态机或审计事件表。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentActivityPhase {
    /// 正在根据用户可见策略选择执行器或子智能体。
    Routing,
    /// 正在准备或发送下一次模型请求。
    Requesting,
    /// 已开始向用户流式生成可见文本。
    Streaming,
    /// 正在执行已对用户可见的工具调用。
    Tool,
    /// 工具调用正等待用户批准。
    WaitingPermission,
    /// 已接纳引导，等待当前安全点。
    SteerAccepted,
    /// 已将引导合并到下一次模型请求。
    SteerApplied,
    /// 正在进行显式配置的结果复核轮次。
    Reviewing,
    /// 正在结束本次运行并持久化已可见输出。
    Finalizing,
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
    /// 可选工作区根目录。未附加时 Agent 只能进行纯对话。
    #[serde(default, alias = "project_id")]
    pub workspace_path: Option<String>,
    /// 工作区的项目级 Agent 权限模式。未附加工作区时此字段不会授予本地能力。
    #[serde(default)]
    pub workspace_access_mode: ProjectAccessMode,
    /// 所属任务 ID（权限门/审计上下文；旧数据缺省为空串）
    #[serde(default)]
    pub task_id: String,
    /// 目标
    pub goal: String,
    /// 交互模式
    pub mode: TaskMode,
    /// 模型名称（可选，使用默认）
    pub model: Option<String>,
    /// 模型专属推理参数；空对象表示沿用 Provider 默认值。
    #[serde(default)]
    pub inference: InferenceOptions,
    /// 上下文引用（可选）
    pub context: Vec<ContextRef>,
}

impl Default for CreateSessionInput {
    fn default() -> Self {
        Self {
            workspace_path: None,
            workspace_access_mode: ProjectAccessMode::RequestApproval,
            task_id: String::new(),
            goal: String::new(),
            mode: TaskMode::Ask,
            model: None,
            inference: InferenceOptions::default(),
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
        let task = Task::new(Some("/proj".into()), "Test", "Do something", TaskMode::Ask);
        assert_eq!(task.workspace_path.as_deref(), Some("/proj"));
        assert_eq!(task.state, TaskState::Idle);
        assert_eq!(task.mode, TaskMode::Ask);
        assert!(!task.goal_active);
        assert!(task.worktree_path.is_none());

        let mut legacy = serde_json::to_value(&task).unwrap();
        legacy.as_object_mut().unwrap().remove("goal_active");
        let restored: Task = serde_json::from_value(legacy).unwrap();
        assert!(!restored.goal_active);
    }

    #[test]
    fn task_state_display_roundtrip() {
        for state in [
            TaskState::Idle,
            TaskState::Exploring,
            TaskState::InProgress,
            TaskState::Interrupted,
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
        assert!(ReviewState::Aborted.is_terminal());
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
    fn permission_request_origin_is_backward_compatible() {
        let legacy: PermissionRequest = serde_json::from_str(
            r#"{
                "id": "permission-1",
                "task_id": "task-1",
                "tool_call_id": "call-1",
                "tool_name": "write_file",
                "risk_level": "R2",
                "input_summary": "write src/lib.rs",
                "decision": "pending",
                "created_at": "2025-01-01T00:00:00Z",
                "decided_at": null
            }"#,
        )
        .unwrap();
        assert!(legacy.run_id.is_none());
        assert!(legacy.caller.is_none());
        assert!(legacy.target.is_none());

        let request = PermissionRequest::new(
            "task-1",
            "call-1",
            "write_file",
            RiskLevel::R2,
            "write src/lib.rs",
        )
        .with_origin(Some("run-1"), Some("subagent:child-1"))
        .with_target(Some("workspace/src/lib.rs"));
        assert_eq!(request.run_id.as_deref(), Some("run-1"));
        assert_eq!(request.caller.as_deref(), Some("subagent:child-1"));
        assert_eq!(request.target.as_deref(), Some("workspace/src/lib.rs"));
    }

    #[test]
    fn agent_run_lifecycle() {
        let mut run = AgentRun::new("task1", "claude-3-5-sonnet");
        assert!(run.is_active());
        assert_eq!(run.branch_id, "main");
        assert_eq!(run.agent_kind, AgentKind::Main);
        assert!(run.parent_run_id.is_none());
        assert!(run.agent_label.is_none());
        assert!(run.delegated_by_tool_call_id.is_none());
        assert_eq!(run.runtime_kind, AgentRunRuntimeKind::Native);
        assert!(run.external_session_id.is_none());
        assert_eq!(run.review_state, ReviewState::Pending);
        run.finish(ReviewState::Accepted);
        assert!(!run.is_active());
        assert_eq!(run.review_state, ReviewState::Accepted);
    }

    #[test]
    fn agent_run_legacy_json_and_subagent_constructor_are_compatible() {
        let legacy: AgentRun = serde_json::from_str(
            r#"{
                "id": "legacy-run",
                "task_id": "task-1",
                "model": "legacy-model",
                "review_state": "pending",
                "started_at": "2025-01-01T00:00:00Z",
                "ended_at": null,
                "usage_json": null
            }"#,
        )
        .unwrap();
        assert_eq!(legacy.branch_id, "main");
        assert_eq!(legacy.agent_kind, AgentKind::Main);
        assert!(legacy.parent_run_id.is_none());
        assert!(legacy.agent_label.is_none());
        assert!(legacy.delegated_by_tool_call_id.is_none());
        assert_eq!(legacy.runtime_kind, AgentRunRuntimeKind::Native);
        assert!(legacy.external_session_id.is_none());
        assert_eq!(legacy.access_mode, SubagentAccessMode::ReadOnly);
        assert!(!legacy.require_approval);

        let child = AgentRun::new_subagent_for_branch(
            "task-1",
            "branch-1",
            "parent-run",
            "child-model",
            Some("检索代理".to_string()),
            Some("tool-call-1".to_string()),
        );
        assert_eq!(child.branch_id, "branch-1");
        assert_eq!(child.agent_kind, AgentKind::Subagent);
        assert_eq!(child.parent_run_id.as_deref(), Some("parent-run"));
        assert_eq!(child.agent_label.as_deref(), Some("检索代理"));
        assert_eq!(
            child.delegated_by_tool_call_id.as_deref(),
            Some("tool-call-1")
        );

        let main_child = AgentRun::new_subagent("task-1", "parent-run", "child-model", None, None);
        assert_eq!(main_child.branch_id, "main");
        assert_eq!(main_child.agent_kind, AgentKind::Subagent);

        let codex_child = main_child.clone().as_codex_exec_subagent();
        assert_eq!(codex_child.runtime_kind, AgentRunRuntimeKind::CodexExec);
        let codex_mcp_child = main_child.clone().as_codex_mcp_subagent();
        assert_eq!(codex_mcp_child.runtime_kind, AgentRunRuntimeKind::CodexMcp);
        assert_eq!(
            AgentRunRuntimeKind::try_from_str("codex_mcp"),
            Some(AgentRunRuntimeKind::CodexMcp)
        );
        assert_eq!(main_child.parent_run_id.as_deref(), Some("parent-run"));
    }

    #[test]
    fn agent_event_scope_preserves_runtime_identity_with_legacy_defaults() {
        let legacy: AgentEventScope = serde_json::from_str(
            r#"{
                "run_id": "child-run",
                "agent_id": "child-agent",
                "agent_kind": "subagent"
            }"#,
        )
        .unwrap();
        assert_eq!(legacy.runtime_kind, AgentRunRuntimeKind::Native);
        assert!(legacy.model.is_none());
        assert_eq!(legacy.access_mode, SubagentAccessMode::ReadOnly);
        assert!(!legacy.require_approval);

        let codex = AgentEventScope {
            runtime_kind: AgentRunRuntimeKind::CodexExec,
            model: Some("codex-cli".to_string()),
            access_mode: SubagentAccessMode::FullAccess,
            ..legacy
        };
        let encoded = serde_json::to_value(codex).unwrap();
        assert_eq!(encoded["runtime_kind"], "codex_exec");
        assert_eq!(encoded["model"], "codex-cli");
        assert_eq!(encoded["access_mode"], "full_access");
    }

    #[test]
    fn session_branch_and_queue_control_have_stable_defaults() {
        let main = SessionBranch::main("task-1");
        assert_eq!(main.id, "main");
        assert_eq!(main.storage_id, "task-1");
        let branch = SessionBranch::fork("task-1", &main.id, "task-1:3");
        assert_ne!(branch.id, "main");
        assert_eq!(branch.parent_branch_id.as_deref(), Some("main"));

        assert_eq!(
            AgentSendMode::try_from_str("send_now"),
            Some(AgentSendMode::SendNow)
        );
        assert_eq!(
            QueuedMessageState::try_from_str("dispatching"),
            Some(QueuedMessageState::Dispatching)
        );
    }

    #[test]
    fn workspace_memory_mode_serde_defaults_and_identity_are_stable() {
        for (mode, encoded) in [
            (WorkspaceMemoryMode::Inherit, "inherit"),
            (WorkspaceMemoryMode::ReadOnly, "read_only"),
            (WorkspaceMemoryMode::Off, "off"),
        ] {
            assert_eq!(serde_json::to_value(mode).unwrap(), encoded);
            assert_eq!(
                serde_json::from_value::<WorkspaceMemoryMode>(encoded.into()).unwrap(),
                mode
            );
        }

        let legacy: Workspace = serde_json::from_str(
            r#"{
                "id": "00112233445566778899aabbccddeeff",
                "canonical_path": "/legacy",
                "display_name": "Legacy",
                "access_mode": "request_approval",
                "last_opened_at": "2025-01-01T00:00:00Z"
            }"#,
        )
        .unwrap();
        assert_eq!(legacy.memory_mode, WorkspaceMemoryMode::Inherit);
        assert_eq!(legacy.memory_generation, 1);

        let created = Workspace::new("/new", "New");
        let id = uuid::Uuid::parse_str(&created.id).unwrap();
        assert_eq!(created.id.len(), 32);
        assert_eq!(id.get_version(), Some(uuid::Version::Random));
        assert_eq!(created.memory_mode, WorkspaceMemoryMode::Inherit);
        assert_eq!(created.memory_generation, 1);
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
