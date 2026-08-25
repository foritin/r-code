//! Codex App Server 丰富交互归一化层（PRD §4 机器合同的宿主实现，M0-02 冻结）。
//!
//! 职责边界：原始 JSON-RPC 帧 -> 版本化宿主事件（`CodexTimelineEventV1`）。
//! React/持久化只消费归一化事件，永不直接理解 App Server JSON（§2 决策 2）。
//!
//! 冻结语义（docs/codex-rich-interaction-prd.md §4.1/§4.5）：
//! - `scope` 至少含 run/thread/turn；不完整或不匹配的 run 事件不得进入时间线；
//! - phase 与 item kind 是显式枚举 + unknown 兼容分支，禁止裸字符串驱动 UI；
//! - 文本/数组/问题数量全部有界；secret、raw reasoning 永不进入事件或诊断正文；
//! - 未知方法/未知 kind 只产生安全诊断（方法名 + 计数），不静默转为用户消息。
//!
//! 本模块无 I/O、无 tokio 依赖：contract test 直接构造帧即可验证（M0-02
//! 决策空间允许的类型落点）。observer/持久化接线由 M1+ 复用本层完成。

use std::collections::HashMap;

use r_code_core::secret::redact_text;
use serde_json::Value;

// ---------------------------------------------------------------------------
// 有界常量（§4.1 固定要求 + §6 内存/边界门禁；M4-01 复用同一组上限）
// ---------------------------------------------------------------------------

pub const MAX_AUTHORITATIVE_TEXT_CHARS: usize = 200_000;
pub const MAX_DELTA_CHARS: usize = 16_384;
pub const MAX_TOOL_FIELD_CHARS: usize = 8_192;
pub const MAX_DIFF_CHARS: usize = 131_072;
pub const MAX_PLAN_STEPS: usize = 256;
pub const MAX_PLAN_STEP_CHARS: usize = 2_048;
pub const MAX_PLAN_EXPLANATION_CHARS: usize = 2_048;
pub const MAX_QUESTIONS: usize = 32;
pub const MAX_OPTIONS_PER_QUESTION: usize = 32;
pub const MAX_QUESTION_FIELD_CHARS: usize = 2_048;
pub const MAX_REASONING_SUMMARY_CHARS: usize = 32_768;
pub const MAX_TRACKED_ITEMS: usize = 8_192;
pub const MAX_SCOPE_ID_CHARS: usize = 160;

// ---------------------------------------------------------------------------
// 能力快照（§4.5）
// ---------------------------------------------------------------------------

/// 启动时记录的 Codex CLI 能力快照。实验字段缺省不崩溃；能力缺失走降级。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexInteractionCapabilities {
    pub cli_version: Option<String>,
    pub protocol_baseline: &'static str,
    pub supports_request_user_input: bool,
}

impl CodexInteractionCapabilities {
    /// 0.145.0 是本合同冻结基线；未知/缺失版本按最保守能力处理。
    pub fn for_cli_version(version: Option<&str>) -> Self {
        let known = version.map(semver_gte_0_145).unwrap_or(false);
        Self {
            cli_version: version.map(str::to_string),
            protocol_baseline: "codex-app-server/0.145.0",
            supports_request_user_input: known,
        }
    }
}

fn semver_gte_0_145(raw: &str) -> bool {
    let mut parts = raw.trim().split('.');
    let (major, minor, patch) = (
        parts.next().and_then(|p| p.parse::<u64>().ok()),
        parts.next().and_then(|p| p.parse::<u64>().ok()),
        parts.next().and_then(|p| p.parse::<u64>().ok()),
    );
    match (major, minor, patch) {
        (Some(major), Some(minor), Some(patch)) => {
            (major, minor, patch) >= (0, 145, 0)
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// 归一化事件（§4.1 事件 union，逐字段对齐 PRD 签名）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexAssistantPhase {
    Commentary,
    FinalAnswer,
    Unknown,
}

impl CodexAssistantPhase {
    fn from_wire(value: Option<&str>) -> Self {
        match value {
            Some("commentary") => Self::Commentary,
            Some("final_answer") => Self::FinalAnswer,
            // 0.145.0 schema：phase 可为 null；provider 可能不输出。
            // 保守归类发生在投影层（M1-01 turn 边界），归一化保留 Unknown。
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexToolKind {
    CommandExecution,
    FileChange,
    McpToolCall,
    DynamicToolCall,
    CollabAgentToolCall,
    WebSearch,
    ImageView,
    ImageGeneration,
    Sleep,
    SubAgentActivity,
    /// 未知 kind 的显式兼容分支：UI 只能落到“其他 Codex 工具”卡，
    /// 原始类型名保留在 `raw_kind_name`（有界、脱敏）供诊断。
    Unknown,
}

impl CodexToolKind {
    fn from_wire(value: &str) -> Self {
        match value {
            "commandExecution" | "command_execution" => Self::CommandExecution,
            "fileChange" | "file_change" => Self::FileChange,
            "mcpToolCall" | "mcp_tool_call" => Self::McpToolCall,
            "dynamicToolCall" | "dynamic_tool_call" => Self::DynamicToolCall,
            "collabAgentToolCall" | "collab_agent_tool_call" => Self::CollabAgentToolCall,
            "webSearch" | "web_search" => Self::WebSearch,
            "imageView" | "image_view" => Self::ImageView,
            "imageGeneration" | "image_generation" => Self::ImageGeneration,
            "sleep" => Self::Sleep,
            "subAgentActivity" | "sub_agent_activity" => Self::SubAgentActivity,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexToolStatus {
    InProgress,
    Completed,
    Failed,
    Declined,
    Unknown,
}

impl CodexToolStatus {
    fn from_wire(value: Option<&str>) -> Self {
        match value {
            Some("inProgress") => Self::InProgress,
            Some("completed") => Self::Completed,
            Some("failed") => Self::Failed,
            Some("declined") => Self::Declined,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexPlanStepStatus {
    Pending,
    InProgress,
    Completed,
    Unknown,
}

impl CodexPlanStepStatus {
    fn from_wire(value: &str) -> Self {
        match value {
            "pending" => Self::Pending,
            "inProgress" => Self::InProgress,
            "completed" => Self::Completed,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexUserInputOutcome {
    /// serverRequest/resolved：请求已被（他处/自动）解决，pending UI 应清除。
    Resolved,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexPlanStepV1 {
    pub status: CodexPlanStepStatus,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CodexTokenUsageBucketV1 {
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub cache_write_input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_output_tokens: i64,
    pub total_tokens: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CodexTokenUsageV1 {
    pub last: CodexTokenUsageBucketV1,
    pub total: CodexTokenUsageBucketV1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexUserOptionV1 {
    pub label: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexUserQuestionV1 {
    pub id: String,
    pub header: String,
    pub question: String,
    pub is_other: bool,
    pub is_secret: bool,
    pub options: Vec<CodexUserOptionV1>,
}

/// §4.1 固定要求：scope 至少包含 task/run/thread/turn。run 由宿主注入；
/// thread/turn 来自 wire 且必须匹配当前时间线，否则事件不进入当前 run。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexInteractionScopeV1 {
    pub run_id: String,
    pub thread_id: String,
    pub turn_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexSafeInputV1 {
    pub raw_kind_name: Option<String>,
    /// 人类可读的动作摘要（脱敏 + 有界），驱动工具卡折叠态标题。
    pub summary: String,
    /// 动态/MCP 工具名（rcode_delegate 检测用）。
    pub tool_name: Option<String>,
    /// 预构建的安全输入负载（rcode_delegate 的 label/goal/access 白名单对象）。
    pub input_json: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CodexTimelineEventV1 {
    AssistantStarted {
        scope: CodexInteractionScopeV1,
        item_id: String,
        phase: CodexAssistantPhase,
    },
    AssistantDelta {
        scope: CodexInteractionScopeV1,
        item_id: String,
        phase: CodexAssistantPhase,
        delta: String,
    },
    AssistantCompleted {
        scope: CodexInteractionScopeV1,
        item_id: String,
        phase: CodexAssistantPhase,
        authoritative_text: String,
    },
    ReasoningSummaryDelta {
        scope: CodexInteractionScopeV1,
        item_id: String,
        summary_index: i64,
        delta: String,
    },
    ReasoningSummaryCompleted {
        scope: CodexInteractionScopeV1,
        item_id: String,
        public_summary: String,
    },
    ToolStarted {
        scope: CodexInteractionScopeV1,
        item_id: String,
        kind: CodexToolKind,
        safe_input: CodexSafeInputV1,
    },
    ToolOutputDelta {
        scope: CodexInteractionScopeV1,
        item_id: String,
        safe_delta: String,
    },
    ToolCompleted {
        scope: CodexInteractionScopeV1,
        item_id: String,
        kind: CodexToolKind,
        status: CodexToolStatus,
        /// commandExecution 的退出码（其他 kind 为 None）。
        exit_code: Option<i64>,
        safe_output: Option<String>,
    },
    PlanUpdated {
        scope: CodexInteractionScopeV1,
        explanation: Option<String>,
        steps: Vec<CodexPlanStepV1>,
    },
    DiffUpdated {
        scope: CodexInteractionScopeV1,
        unified_diff_or_reference: String,
    },
    ContextCompacted {
        scope: CodexInteractionScopeV1,
        item_id: Option<String>,
    },
    Warning {
        scope: Option<CodexInteractionScopeV1>,
        code: Option<String>,
        safe_message: String,
    },
    UsageUpdated {
        scope: CodexInteractionScopeV1,
        safe_usage: CodexTokenUsageV1,
    },
    UserInputRequested {
        scope: CodexInteractionScopeV1,
        item_id: String,
        transport_generation: u64,
        request_id: String,
        questions: Vec<CodexUserQuestionV1>,
        auto_resolution_ms: Option<u64>,
    },
    UserInputResolved {
        scope: CodexInteractionScopeV1,
        item_id: String,
        outcome: CodexUserInputOutcome,
    },
}

// ---------------------------------------------------------------------------
// 安全诊断（R-OBS-01：记录事件名/作用域/状态/降级原因/计数，不记正文）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CodexDiagnosticCode {
    /// 未知方法：不投影、不伪装，只计数。
    UnknownMethod,
    /// item.type 无法识别：按 Unknown 工具卡兼容。
    UnknownItemKind,
    /// 0.145.0 agentMessage.phase 缺省（null）：保留 Unknown，投影层保守归类。
    PhaseMissing,
    /// threadId/turnId 与当前时间线不匹配：丢弃，不污染当前 run。
    ScopeStale,
    /// 必需 scope 缺失：丢弃。
    ScopeMissing,
    /// 文本/数组超过上限被截断（内容仍部分保留）。
    PayloadTruncated,
    /// 问题/选项超过数量上限被拒绝（超出部分丢弃）。
    PayloadRejected,
    /// 重复完成帧：幂等忽略。
    DuplicateCompleted,
    /// raw reasoning（item/reasoning/textDelta、reasoning.content）按 §2 决策 6 丢弃。
    ReasoningRawDropped,
    /// 旧字段/旧方法名（如裸 `requestUserInput`）：不再按新合同解释。
    LegacyName,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexInteractionDiagnosticV1 {
    pub code: CodexDiagnosticCode,
    pub method: Option<String>,
    pub item_id: Option<String>,
    /// 仅限不可逆元数据（长度、数量、类型名），永不包含 payload 正文。
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CodexInteractionOutcome {
    Event(CodexTimelineEventV1),
    Diagnostic(CodexInteractionDiagnosticV1),
}

// ---------------------------------------------------------------------------
// requestUserInput 解析（M3-01：归一化器与反向请求桥共用）
// ---------------------------------------------------------------------------

/// 0.145.0 `item/tool/requestUserInput` 的解析结果（问题已脱敏 + 有界）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUserInputRequest {
    pub request_id: String,
    pub item_id: String,
    pub auto_resolution_ms: Option<u64>,
    pub questions: Vec<CodexUserQuestionV1>,
}

/// 解析问题：调用方按各自合同处置（归一化器降级诊断；请求桥以协议错误
/// 拒绝并保持 fail-closed，§4.3）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserInputParseIssue {
    MissingRequestId,
    /// 0.145.0 必需字段 questions 缺失或不是数组。
    MissingQuestions,
    TooManyQuestions,
    QuestionMissingId { index: usize },
    DuplicateQuestionId { id: String },
}

pub fn parse_user_input_request(frame: &Value) -> (Option<ParsedUserInputRequest>, Vec<UserInputParseIssue>) {
    let mut issues = Vec::new();
    let params = frame.get("params").cloned().unwrap_or_default();
    let item_id = bounded_scope_id(params.get("itemId").and_then(Value::as_str));
    let request_id = wire_request_id(frame.get("id"));
    if request_id.is_empty() {
        issues.push(UserInputParseIssue::MissingRequestId);
        return (None, issues);
    }
    let auto_resolution_ms = params.get("autoResolutionMs").and_then(Value::as_u64);
    let mut questions: Vec<CodexUserQuestionV1> = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();
    match params.get("questions").and_then(Value::as_array) {
        Some(raw_questions) => {
            for (index, question) in raw_questions.iter().enumerate() {
            if index >= MAX_QUESTIONS {
                issues.push(UserInputParseIssue::TooManyQuestions);
                break;
            }
            let id = bounded_scope_id(question.get("id").and_then(Value::as_str));
            if id.is_empty() {
                issues.push(UserInputParseIssue::QuestionMissingId { index });
                continue;
            }
            if !seen_ids.insert(id.clone()) {
                issues.push(UserInputParseIssue::DuplicateQuestionId { id });
                continue;
            }
            let mut options = Vec::new();
            if let Some(raw_options) = question.get("options").and_then(Value::as_array) {
                for option in raw_options.iter().take(MAX_OPTIONS_PER_QUESTION) {
                    options.push(CodexUserOptionV1 {
                        label: safe_text(
                            option.get("label").and_then(Value::as_str).unwrap_or(""),
                            MAX_QUESTION_FIELD_CHARS,
                        ),
                        description: safe_text(
                            option.get("description").and_then(Value::as_str).unwrap_or(""),
                            MAX_QUESTION_FIELD_CHARS,
                        ),
                    });
                }
            }
            questions.push(CodexUserQuestionV1 {
                id,
                header: safe_text(
                    question.get("header").and_then(Value::as_str).unwrap_or(""),
                    MAX_QUESTION_FIELD_CHARS,
                ),
                question: safe_text(
                    question.get("question").and_then(Value::as_str).unwrap_or(""),
                    MAX_QUESTION_FIELD_CHARS,
                ),
                is_other: question
                    .get("isOther")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                is_secret: question
                    .get("isSecret")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                options,
            });
        }
        }
        None => issues.push(UserInputParseIssue::MissingQuestions),
    }
    (
        Some(ParsedUserInputRequest {
            request_id,
            item_id,
            auto_resolution_ms,
            questions,
        }),
        issues,
    )
}

// ---------------------------------------------------------------------------
// agentMessage 投影器（M1-01：§4.2 commentary 状态机的宿主实现）
// ---------------------------------------------------------------------------

/// 投影器输出。`Delta` 直接对应 `AgentEvent::Message { delta: true }`；
/// `Sealed` 携带权威全文（corrected=true 表示与累计不一致，已按 §4.2
/// 以全文修正并计入 mismatch 计数）。`streamed=true` 表示该 item 已发过
/// 增量——下游封口帧必须零长度（正文已交付）；`streamed=false` 时封口
/// 帧携带全文一次性交付（子代理/无增量兼容路径）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexMessageEmission {
    Delta {
        item_id: String,
        phase: CodexAssistantPhase,
        delta: String,
    },
    Sealed {
        item_id: String,
        phase: CodexAssistantPhase,
        authoritative_text: String,
        corrected: bool,
        streamed: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CodexProjectedItem {
    item_id: String,
    phase: CodexAssistantPhase,
    text: String,
    sealed: bool,
    streamed: bool,
}

/// 同一 run 内按 item 维护累计文本与完成状态。feed 顺序即事件顺序；
/// 重复完成幂等（正常化层已拦截重复 completed，这里再防御一次）。
pub struct CodexAgentMessageProjector {
    items: Vec<CodexProjectedItem>,
    pub mismatch_count: u64,
    pub duplicate_seal_count: u64,
    pub late_delta_count: u64,
}

impl Default for CodexAgentMessageProjector {
    fn default() -> Self {
        Self::new()
    }
}

impl CodexAgentMessageProjector {
    const MAX_TRACKED_ITEMS: usize = 1_024;

    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            mismatch_count: 0,
            duplicate_seal_count: 0,
            late_delta_count: 0,
        }
    }

    /// 消费归一化事件；只处理 agentMessage 生命周期，其余事件原样忽略
    /// （M2 的工具/上下文投影复用同一 normalizer 输出）。
    pub fn observe(&mut self, event: &CodexTimelineEventV1) -> Vec<CodexMessageEmission> {
        match event {
            CodexTimelineEventV1::AssistantStarted {
                item_id, phase, ..
            } => {
                self.start_item(item_id.clone(), *phase);
                Vec::new()
            }
            CodexTimelineEventV1::AssistantDelta {
                item_id, phase, delta, ..
            } => {
                self.push_delta(item_id, *phase, delta)
            }
            CodexTimelineEventV1::AssistantCompleted {
                item_id,
                phase,
                authoritative_text,
                ..
            } => self.seal(item_id, *phase, authoritative_text.clone()),
            _ => Vec::new(),
        }
    }

    /// turn 结束/中断：未封口 item 以累计文本封口。phase=Unknown 的残留按
    /// §4.2 保守归类为 commentary（final 只属于明确 final_answer 或 turn
    /// 交付位，交给 M1-03 的边界分类）。
    pub fn finish_turn(&mut self) -> Vec<CodexMessageEmission> {
        let mut emissions = Vec::new();
        let residuals: Vec<String> = self
            .items
            .iter()
            .filter(|item| !item.sealed)
            .map(|item| item.item_id.clone())
            .collect();
        for item_id in residuals {
            if let Some(item) = self.items.iter_mut().find(|i| i.item_id == item_id) {
                item.sealed = true;
                let text = item.text.clone();
                let streamed = item.streamed;
                let phase = match item.phase {
                    CodexAssistantPhase::FinalAnswer => CodexAssistantPhase::FinalAnswer,
                    _ => CodexAssistantPhase::Commentary,
                };
                item.phase = phase;
                emissions.push(CodexMessageEmission::Sealed {
                    item_id,
                    phase,
                    authoritative_text: text,
                    corrected: false,
                    streamed,
                });
            }
        }
        emissions
    }

    /// 最后一次封口的全文（CodexExecCompletion.summary 语义保持旧口径：
    /// 最后一条 agentMessage 文本）。
    pub fn last_sealed_text(&self) -> Option<&str> {
        self.items
            .iter()
            .rev()
            .find(|item| item.sealed && !item.text.trim().is_empty())
            .map(|item| item.text.as_str())
    }

    fn start_item(&mut self, item_id: String, phase: CodexAssistantPhase) {
        if self.items.len() >= Self::MAX_TRACKED_ITEMS && !self.items.iter().any(|i| i.item_id == item_id) {
            self.items.retain(|item| item.sealed);
            self.items.truncate(Self::MAX_TRACKED_ITEMS.saturating_sub(1));
        }
        if let Some(item) = self.items.iter_mut().find(|i| i.item_id == item_id) {
            item.phase = phase;
            item.sealed = false;
            item.streamed = false;
            item.text.clear();
            return;
        }
        self.items.push(CodexProjectedItem {
            item_id,
            phase,
            text: String::new(),
            sealed: false,
            streamed: false,
        });
    }

    fn push_delta(
        &mut self,
        item_id: &str,
        phase: CodexAssistantPhase,
        delta: &str,
    ) -> Vec<CodexMessageEmission> {
        // 终态墓碑：sealed 之后的迟到增量直接丢弃（§4.2），正文已按权威
        // 全文交付，追加会造成重复。
        if self
            .items
            .iter()
            .any(|item| item.item_id == item_id && item.sealed)
        {
            self.late_delta_count += 1;
            return Vec::new();
        }
        let known_phase = phase != CodexAssistantPhase::Unknown;
        match self.items.iter_mut().find(|i| i.item_id == item_id) {
            Some(item) => {
                if known_phase {
                    item.phase = phase;
                }
                item.text.push_str(delta);
                item.streamed = true;
            }
            None => self.items.push(CodexProjectedItem {
                item_id: item_id.to_string(),
                phase,
                text: delta.to_string(),
                sealed: false,
                streamed: true,
            }),
        }
        vec![CodexMessageEmission::Delta {
            item_id: item_id.to_string(),
            phase,
            delta: delta.to_string(),
        }]
    }

    fn seal(
        &mut self,
        item_id: &str,
        phase: CodexAssistantPhase,
        authoritative_text: String,
    ) -> Vec<CodexMessageEmission> {
        let Some(item) = self.items.iter_mut().find(|i| i.item_id == item_id) else {
            // 迟到/缺失 started 的完成帧：以权威全文一次性封口。
            self.items.push(CodexProjectedItem {
                item_id: item_id.to_string(),
                phase,
                text: authoritative_text.clone(),
                sealed: true,
                streamed: false,
            });
            return vec![CodexMessageEmission::Sealed {
                item_id: item_id.to_string(),
                phase,
                authoritative_text,
                corrected: false,
                streamed: false,
            }];
        };
        if item.sealed {
            // 幂等：重复完成帧不再产生事件（计数由 normalizer 诊断承担）。
            self.duplicate_seal_count += 1;
            return Vec::new();
        }
        let corrected = item.text != authoritative_text;
        if corrected {
            self.mismatch_count += 1;
            item.text = authoritative_text.clone();
        }
        if known(phase) {
            item.phase = phase;
        }
        item.sealed = true;
        let streamed = item.streamed;
        let sealed_phase = item.phase;
        vec![CodexMessageEmission::Sealed {
            item_id: item_id.to_string(),
            phase: sealed_phase,
            authoritative_text,
            corrected,
            streamed,
        }]
    }
}

fn known(phase: CodexAssistantPhase) -> bool {
    phase != CodexAssistantPhase::Unknown
}

/// 单个工具项的输出累计缓冲：总量超限时保留头尾并插入截断标记，
/// 进程内存有界且截断对用户可见（§6 内存/边界 + M2-01.A3）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct CodexToolOutputBuffer {
    head: String,
    tail: String,
    head_full: bool,
    dropped_chars: usize,
}

impl CodexToolOutputBuffer {
    const HEAD_CHARS: usize = 65_536;
    const TAIL_CHARS: usize = 65_536;

    fn push(&mut self, delta: &str) {
        for ch in delta.chars() {
            if !self.head_full {
                if self.head.chars().count() >= Self::HEAD_CHARS {
                    self.head_full = true;
                } else {
                    self.head.push(ch);
                    continue;
                }
            }
            self.tail.push(ch);
            if self.tail.chars().count() > Self::TAIL_CHARS {
                let overflow = self.tail.chars().count() - Self::TAIL_CHARS;
                self.dropped_chars += overflow;
                // 丢弃最旧的尾部字符，保留最近 TAIL_CHARS 个。
                let keep: String = self.tail.chars().skip(overflow).collect();
                self.tail = keep;
            }
        }
    }

    fn render(&self) -> String {
        if self.dropped_chars == 0 {
            return format!("{}{}", self.head, self.tail);
        }
        format!(
            "{}\n…[输出超出上限，中间已截断约 {} 字符]…\n{}",
            self.head, self.dropped_chars, self.tail
        )
    }
}

/// 一次 App Server run 的完整投影状态：归一化器 + agentMessage 投影器。
/// M1-01 起 host 主循环持有；M2/M3 复用同一实例扩展工具/提问投影。
pub struct CodexRunProjection {
    pub normalizer: CodexInteractionNormalizer,
    pub messages: CodexAgentMessageProjector,
    tool_outputs: HashMap<String, CodexToolOutputBuffer>,
}

impl CodexRunProjection {
    pub fn new(
        capabilities: CodexInteractionCapabilities,
        transport_generation: u64,
        run_id: impl Into<String>,
        thread_id: &str,
        turn_id: &str,
    ) -> Self {
        let mut normalizer =
            CodexInteractionNormalizer::new(capabilities, transport_generation, run_id);
        normalizer.begin_thread(thread_id);
        normalizer.begin_turn(turn_id);
        Self {
            normalizer,
            messages: CodexAgentMessageProjector::new(),
            tool_outputs: HashMap::new(),
        }
    }

    /// 累计一条工具输出增量（有界）；返回累计后的可见文本。
    pub fn accumulate_tool_output(&mut self, item_id: &str, delta: &str) -> String {
        let buffer = self.tool_outputs.entry(item_id.to_string()).or_default();
        buffer.push(delta);
        buffer.render()
    }

    /// 工具终态时取回累计输出：优先使用 item 自带的权威聚合输出，
    /// 缺失时回退累计缓冲。终态不受中途截断影响。
    pub fn take_tool_output(&mut self, item_id: &str, authoritative: Option<String>) -> Option<String> {
        let buffered = self.tool_outputs.remove(item_id).map(|buffer| buffer.render());
        match authoritative {
            Some(output) => Some(output),
            None => buffered.filter(|text| !text.is_empty()),
        }
    }

    /// 归一化一帧并把 agentMessage 生命周期投影成发射事件。
    /// 其余事件（工具/计划/提问）返回给调用方按里程碑接线。
    pub fn feed_frame(
        &mut self,
        frame: &Value,
    ) -> (
        Vec<CodexMessageEmission>,
        Vec<CodexTimelineEventV1>,
        Vec<CodexInteractionDiagnosticV1>,
    ) {
        let mut emissions = Vec::new();
        let mut events = Vec::new();
        let mut diagnostics = Vec::new();
        for outcome in self.normalizer.feed(frame) {
            match outcome {
                CodexInteractionOutcome::Event(event) => {
                    emissions.extend(self.messages.observe(&event));
                    events.push(event);
                }
                CodexInteractionOutcome::Diagnostic(diagnostic) => diagnostics.push(diagnostic),
            }
        }
        (emissions, events, diagnostics)
    }

    /// turn 结束/中断：残留 item 封口。
    pub fn finish_turn(&mut self) -> Vec<CodexMessageEmission> {
        self.messages.finish_turn()
    }
}

// ---------------------------------------------------------------------------
// 方法表（transport progress / scope / observer / dispatcher 的唯一事实源）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexScopeRequirement {
    Thread,
    ThreadAndTurn,
}

/// timeline 方法：会产生 `CodexTimelineEventV1` 的 notification。
pub(crate) fn is_timeline_notification(method: &str) -> bool {
    matches!(
        method,
        "item/started"
            | "item/completed"
            | "item/agentMessage/delta"
            | "item/reasoning/summaryTextDelta"
            | "item/reasoning/summaryPartAdded"
            | "item/reasoning/textDelta"
            | "item/commandExecution/outputDelta"
            | "item/fileChange/outputDelta"
            | "item/fileChange/patchUpdated"
            | "turn/plan/updated"
            | "turn/diff/updated"
            | "thread/compacted"
            | "thread/tokenUsage/updated"
            | "warning"
            | "error"
            | "serverRequest/resolved"
    )
}

/// 反向请求方法（带 id，需要精确回应）。
pub(crate) fn is_server_request(method: &str) -> bool {
    matches!(
        method,
        "item/tool/requestUserInput"
            | "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "item/permissions/requestApproval"
            | "item/tool/call"
    )
}

/// 心跳/进度方法：transport 启动等待循环用来刷新 deadline。
/// 与旧 `recognized_protocol_progress` 语义保持一致（M0-02 步骤 1 对齐点）。
pub(crate) fn is_recognized_protocol_progress(method: &str) -> bool {
    matches!(
        method,
        "initialized"
            | "thread/started"
            | "thread/status/changed"
            | "turn/started"
            | "turn/completed"
            | "item/commandExecution/requestApproval"
            | "item/fileChange/requestApproval"
            | "item/permissions/requestApproval"
            | "requestUserInput"
    ) || is_timeline_notification(method)
        || is_server_request(method)
}

/// scope 需求表：覆盖 timeline + request 方法。commands.rs 旧表委托到这里，
/// 新增方法（warning/error/usage/diff/compacted 等）在此扩展。
/// turn/started|completed 与 error 按各自 0.145.0 schema 必需字段从严：
/// error 的 threadId+turnId 是 required，缺失即不进入当前时间线（§4.1）。
pub(crate) fn codex_scope_requirement(method: &str) -> Option<CodexScopeRequirement> {
    match method {
        "thread/started" | "thread/status/changed" => Some(CodexScopeRequirement::Thread),
        "turn/started"
        | "turn/completed"
        | "turn/failed"
        | "error"
        | "turn/plan/updated"
        | "turn/diff/updated"
        | "thread/compacted"
        | "thread/tokenUsage/updated"
        | "item/started"
        | "item/completed"
        | "item/agentMessage/delta"
        | "item/reasoning/summaryTextDelta"
        | "item/reasoning/summaryPartAdded"
        | "item/reasoning/textDelta"
        | "item/commandExecution/outputDelta"
        | "item/fileChange/outputDelta"
        | "item/fileChange/patchUpdated"
        | "item/commandExecution/requestApproval"
        | "item/fileChange/requestApproval"
        | "item/permissions/requestApproval"
        | "item/tool/call"
        | "item/tool/requestUserInput"
        | "requestUserInput"
        | "serverRequest/resolved" => Some(CodexScopeRequirement::ThreadAndTurn),
        // warning 的 threadId 在 schema 中是可选的：全局 warning 允许无 scope。
        "warning" => None,
        _ => None,
    }
}

/// scope 门禁结果：Denied = 丢弃（已计数）；Optional = 方法无 scope 需求
/// （warning），允许无 scope 但若携带 threadId 仍须匹配。
#[derive(Debug, Clone, PartialEq, Eq)]
enum CodexScopeGate {
    Denied,
    Optional(Option<CodexInteractionScopeV1>),
    Scoped(CodexInteractionScopeV1),
}

// ---------------------------------------------------------------------------
// 归一化器
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum CodexItemState {
    AgentMessage {
        phase: CodexAssistantPhase,
        completed: bool,
    },
    Tool {
        kind: CodexToolKind,
        completed: bool,
    },
    Reasoning {
        completed: bool,
    },
    Other {
        completed: bool,
    },
}

/// 每个 run 一个实例。feed 是纯内存转换：无 I/O、无时钟依赖，
/// 帧顺序即事件顺序（§4.2 delta UI 合并、M1-03 ≤10Hz 刷新在更上层）。
pub struct CodexInteractionNormalizer {
    capabilities: CodexInteractionCapabilities,
    transport_generation: u64,
    run_id: String,
    thread_id: Option<String>,
    active_turn: Option<String>,
    items: HashMap<String, CodexItemState>,
    /// serverRequest/resolved 关联：request_id -> item_id（M3 桥接复用）。
    pending_user_inputs: HashMap<String, String>,
    /// 诊断计数：宿主周期性上报，避免逐帧洪水（R-OBS-01）。
    pub diagnostic_counts: HashMap<CodexDiagnosticCode, u64>,
}

impl CodexInteractionNormalizer {
    pub fn new(
        capabilities: CodexInteractionCapabilities,
        transport_generation: u64,
        run_id: impl Into<String>,
    ) -> Self {
        Self {
            capabilities,
            transport_generation,
            run_id: run_id.into(),
            thread_id: None,
            active_turn: None,
            items: HashMap::new(),
            pending_user_inputs: HashMap::new(),
            diagnostic_counts: HashMap::new(),
        }
    }

    pub fn capabilities(&self) -> &CodexInteractionCapabilities {
        &self.capabilities
    }

    pub fn transport_generation(&self) -> u64 {
        self.transport_generation
    }

    /// 由宿主在 thread/start、turn/start 应答后直接注入已知 scope。
    /// App Server 主流程用请求-应答拿 id，等价通知帧到达时重复注入是幂等的。
    pub fn begin_thread(&mut self, thread_id: &str) {
        let bounded = bounded_scope_id(Some(thread_id));
        if self.thread_id.is_none() {
            self.thread_id = Some(bounded);
        }
    }

    /// 进入新 turn：清掉旧 turn 的活动标记（复用同一 transport 的连续 run）。
    pub fn begin_turn(&mut self, turn_id: &str) {
        self.active_turn = Some(bounded_scope_id(Some(turn_id)));
    }

    /// 归一化一帧。返回 0..n 个结果：事件 +（可选）伴随诊断。
    /// 任何内部错误都以诊断形式返回，feed 本身永不 panic、永不阻塞。
    pub fn feed(&mut self, frame: &Value) -> Vec<CodexInteractionOutcome> {
        let Some(method) = frame.get("method").and_then(Value::as_str) else {
            return Vec::new();
        };
        match method {
            "thread/started" => return self.handle_thread_started(frame),
            "turn/started" => return self.handle_turn_started(frame),
            "turn/completed" | "turn/failed" => return self.handle_turn_finished(frame),
            _ => {}
        }
        if !is_timeline_notification(method) && !is_server_request(method) {
            if method == "requestUserInput" {
                // 旧版方法名：不按新合同解释，显式降级诊断。
                return vec![self.diagnostic(
                    CodexDiagnosticCode::LegacyName,
                    Some(method),
                    None,
                    "bare requestUserInput predates item/tool/requestUserInput",
                )];
            }
            return vec![self.diagnostic(
                CodexDiagnosticCode::UnknownMethod,
                Some(method),
                None,
                format!("len={}", method.len()),
            )];
        }
        // scope 门禁（§4.1：不完整/不匹配不进入当前时间线）。
        match self.admit(method, frame) {
            CodexScopeGate::Denied => Vec::new(),
            CodexScopeGate::Optional(optional_scope) => self.handle_warning(optional_scope, frame),
            CodexScopeGate::Scoped(scope) => self.dispatch_scoped(method, scope, frame),
        }
    }

    fn handle_warning(
        &mut self,
        scope: Option<CodexInteractionScopeV1>,
        frame: &Value,
    ) -> Vec<CodexInteractionOutcome> {
        let safe_message = safe_text(
            frame.pointer("/params/message").and_then(Value::as_str).unwrap_or(""),
            MAX_TOOL_FIELD_CHARS,
        );
        vec![CodexInteractionOutcome::Event(CodexTimelineEventV1::Warning {
            scope,
            code: None,
            safe_message,
        })]
    }

    fn dispatch_scoped(
        &mut self,
        method: &str,
        scope: CodexInteractionScopeV1,
        frame: &Value,
    ) -> Vec<CodexInteractionOutcome> {
        match method {
            "item/started" => self.handle_item_started(&scope, frame),
            "item/completed" => self.handle_item_completed(&scope, frame),
            "item/agentMessage/delta" => {
                let mut outcomes = Vec::new();
                let (item_id, delta) = match item_delta_parts(frame) {
                    Some(parts) => parts,
                    None => {
                        outcomes.push(self.diagnostic(
                            CodexDiagnosticCode::ScopeMissing,
                            Some(method),
                            None,
                            "delta frame missing itemId/delta",
                        ));
                        return outcomes;
                    }
                };
                let phase = self.agent_phase_for(&item_id);
                let (delta, truncated) = safe_text_checked(&delta, MAX_DELTA_CHARS);
                if truncated {
                    outcomes.push(self.diagnostic(
                        CodexDiagnosticCode::PayloadTruncated,
                        Some(method),
                        Some(&item_id),
                        format!("delta chars>{}", MAX_DELTA_CHARS),
                    ));
                }
                if phase == CodexAssistantPhase::Unknown {
                    outcomes.push(self.diagnostic(
                        CodexDiagnosticCode::PhaseMissing,
                        Some(method),
                        Some(&item_id),
                        "agentMessage delta without known phase",
                    ));
                }
                outcomes.push(CodexInteractionOutcome::Event(
                    CodexTimelineEventV1::AssistantDelta {
                        scope,
                        item_id,
                        phase,
                        delta,
                    },
                ));
                outcomes
            }
            "item/reasoning/summaryTextDelta" => {
                let mut outcomes = Vec::new();
                let item_id = bounded_scope_id(frame.pointer("/params/itemId").and_then(Value::as_str));
                let summary_index = frame
                    .pointer("/params/summaryIndex")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let delta_raw = frame.pointer("/params/delta").and_then(Value::as_str).unwrap_or("");
                let (delta, truncated) = safe_text_checked(delta_raw, MAX_DELTA_CHARS);
                if truncated {
                    outcomes.push(self.diagnostic(
                        CodexDiagnosticCode::PayloadTruncated,
                        Some(method),
                        Some(&item_id),
                        format!("summary delta chars>{}", MAX_DELTA_CHARS),
                    ));
                }
                outcomes.push(CodexInteractionOutcome::Event(
                    CodexTimelineEventV1::ReasoningSummaryDelta {
                        scope,
                        item_id,
                        summary_index,
                        delta,
                    },
                ));
                outcomes
            }
            "item/reasoning/summaryPartAdded" => {
                // 0.145.0：summaryPartAdded 只携带分部索引，公开全文在
                // item/completed(reasoning).summary；这里仅推进内部状态。
                Vec::new()
            }
            "item/reasoning/textDelta" => {
                let item_id = frame
                    .pointer("/params/itemId")
                    .and_then(Value::as_str)
                    .map(|id| bounded_scope_id(Some(id)));
                vec![self.diagnostic(
                    CodexDiagnosticCode::ReasoningRawDropped,
                    Some(method),
                    item_id.as_deref(),
                    "raw reasoning delta dropped by contract",
                )]
            }
            "item/commandExecution/outputDelta" | "item/fileChange/outputDelta" => {
                let mut outcomes = Vec::new();
                let item_id = bounded_scope_id(frame.pointer("/params/itemId").and_then(Value::as_str));
                let delta_raw = frame.pointer("/params/delta").and_then(Value::as_str).unwrap_or("");
                let (safe_delta, truncated) = safe_text_checked(delta_raw, MAX_DELTA_CHARS);
                if truncated {
                    outcomes.push(self.diagnostic(
                        CodexDiagnosticCode::PayloadTruncated,
                        Some(method),
                        Some(&item_id),
                        format!("tool output delta chars>{}", MAX_DELTA_CHARS),
                    ));
                }
                outcomes.push(CodexInteractionOutcome::Event(
                    CodexTimelineEventV1::ToolOutputDelta {
                        scope,
                        item_id,
                        safe_delta,
                    },
                ));
                outcomes
            }
            "item/fileChange/patchUpdated" => {
                let mut outcomes = Vec::new();
                let item_id = bounded_scope_id(frame.pointer("/params/itemId").and_then(Value::as_str));
                let mut diff = String::new();
                if let Some(changes) = frame
                    .pointer("/params/changes")
                    .and_then(Value::as_array)
                {
                    for change in changes {
                        if let Some(text) = change.get("diff").and_then(Value::as_str) {
                            diff.push_str(text);
                            diff.push('\n');
                        }
                    }
                }
                let (bounded_diff, truncated) = safe_text_checked(&diff, MAX_DIFF_CHARS);
                if truncated {
                    outcomes.push(self.diagnostic(
                        CodexDiagnosticCode::PayloadTruncated,
                        Some(method),
                        Some(&item_id),
                        format!("patch diff chars>{}", MAX_DIFF_CHARS),
                    ));
                }
                outcomes.push(CodexInteractionOutcome::Event(
                    CodexTimelineEventV1::DiffUpdated {
                        scope,
                        unified_diff_or_reference: bounded_diff,
                    },
                ));
                outcomes
            }
            "turn/plan/updated" => {
                let mut outcomes = Vec::new();
                let explanation = frame
                    .pointer("/params/explanation")
                    .and_then(Value::as_str)
                    .filter(|text| !text.trim().is_empty())
                    .map(|text| safe_text(text, MAX_PLAN_EXPLANATION_CHARS));
                let mut steps = Vec::new();
                if let Some(raw_steps) = frame.pointer("/params/plan").and_then(Value::as_array) {
                    for (index, step) in raw_steps.iter().enumerate() {
                        if index >= MAX_PLAN_STEPS {
                            outcomes.push(self.diagnostic(
                                CodexDiagnosticCode::PayloadRejected,
                                Some(method),
                                None,
                                format!("plan steps>{MAX_PLAN_STEPS}"),
                            ));
                            break;
                        }
                        let status = step
                            .get("status")
                            .and_then(Value::as_str)
                            .map(CodexPlanStepStatus::from_wire)
                            .unwrap_or(CodexPlanStepStatus::Unknown);
                        let text = safe_text(
                            step.get("step").and_then(Value::as_str).unwrap_or(""),
                            MAX_PLAN_STEP_CHARS,
                        );
                        steps.push(CodexPlanStepV1 { status, text });
                    }
                }
                outcomes.push(CodexInteractionOutcome::Event(
                    CodexTimelineEventV1::PlanUpdated {
                        scope,
                        explanation,
                        steps,
                    },
                ));
                outcomes
            }
            "turn/diff/updated" => {
                let mut outcomes = Vec::new();
                let diff_raw = frame.pointer("/params/diff").and_then(Value::as_str).unwrap_or("");
                let (bounded_diff, truncated) = safe_text_checked(diff_raw, MAX_DIFF_CHARS);
                if truncated {
                    outcomes.push(self.diagnostic(
                        CodexDiagnosticCode::PayloadTruncated,
                        Some(method),
                        None,
                        format!("turn diff chars>{}", MAX_DIFF_CHARS),
                    ));
                }
                outcomes.push(CodexInteractionOutcome::Event(
                    CodexTimelineEventV1::DiffUpdated {
                        scope,
                        unified_diff_or_reference: bounded_diff,
                    },
                ));
                outcomes
            }
            "thread/compacted" => vec![CodexInteractionOutcome::Event(
                CodexTimelineEventV1::ContextCompacted {
                    scope,
                    item_id: None,
                },
            )],
            "thread/tokenUsage/updated" => {
                let usage = frame.pointer("/params/tokenUsage");
                match usage.map(parse_token_usage) {
                    Some(safe_usage) => vec![CodexInteractionOutcome::Event(
                        CodexTimelineEventV1::UsageUpdated { scope, safe_usage },
                    )],
                    None => vec![self.diagnostic(
                        CodexDiagnosticCode::ScopeMissing,
                        Some(method),
                        None,
                        "tokenUsage missing last/total",
                    )],
                }
            }
            "error" => {
                // §4.1 无独立 Error 事件；错误按 R-ACT-02 冻结为带 code 的
                // Warning（非聊天事件），正文脱敏 + 有界。
                let safe_message = safe_text(
                    frame
                        .pointer("/params/error/message")
                        .and_then(Value::as_str)
                        .unwrap_or("Codex 报告了错误"),
                    MAX_TOOL_FIELD_CHARS,
                );
                vec![CodexInteractionOutcome::Event(CodexTimelineEventV1::Warning {
                    scope: Some(scope),
                    code: Some("codex/error".to_string()),
                    safe_message,
                })]
            }
            "serverRequest/resolved" => {
                let request_id = wire_request_id(frame.pointer("/params/requestId"));
                let item_id = self
                    .pending_user_inputs
                    .remove(&request_id)
                    .unwrap_or_else(|| format!("request:{request_id}"));
                vec![CodexInteractionOutcome::Event(
                    CodexTimelineEventV1::UserInputResolved {
                        scope,
                        item_id,
                        outcome: CodexUserInputOutcome::Resolved,
                    },
                )]
            }
            "item/tool/requestUserInput" => self.handle_user_input_request(scope, frame),
            // 审批/动态工具请求由 dispatcher 处理（M1/M3 接线），归一化层
            // 不产生时间线事件。
            _ => Vec::new(),
        }
    }

    fn handle_thread_started(&mut self, frame: &Value) -> Vec<CodexInteractionOutcome> {
        if let Some(thread_id) = frame.pointer("/params/thread/id").and_then(Value::as_str) {
            let bounded = bounded_scope_id(Some(thread_id));
            match &self.thread_id {
                None => self.thread_id = Some(bounded),
                Some(current) if *current == bounded => {}
                Some(_) => {
                    return vec![self.diagnostic(
                        CodexDiagnosticCode::ScopeStale,
                        Some("thread/started"),
                        None,
                        "second thread id on one run",
                    )]
                }
            }
        }
        Vec::new()
    }

    fn handle_turn_started(&mut self, frame: &Value) -> Vec<CodexInteractionOutcome> {
        let Some(expected_thread) = self.thread_id.clone() else {
            return vec![self.diagnostic(
                CodexDiagnosticCode::ScopeMissing,
                Some("turn/started"),
                None,
                "turn started before thread/started",
            )];
        };
        let frame_thread = frame
            .pointer("/params/threadId")
            .and_then(Value::as_str)
            .map(|value| bounded_scope_id(Some(value)));
        if frame_thread.as_deref() != Some(expected_thread.as_str()) {
            return vec![self.diagnostic(
                CodexDiagnosticCode::ScopeStale,
                Some("turn/started"),
                None,
                "turn started for a foreign thread",
            )];
        }
        if let Some(turn_id) = frame.pointer("/params/turn/id").and_then(Value::as_str) {
            self.active_turn = Some(bounded_scope_id(Some(turn_id)));
        }
        Vec::new()
    }

    fn handle_turn_finished(&mut self, frame: &Value) -> Vec<CodexInteractionOutcome> {
        let frame_turn = frame
            .pointer("/params/turn/id")
            .or_else(|| frame.pointer("/params/turnId"))
            .and_then(Value::as_str)
            .map(|value| bounded_scope_id(Some(value)));
        match (&self.active_turn, frame_turn) {
            (Some(active), Some(observed)) if *active == observed => {
                self.active_turn = None;
                Vec::new()
            }
            (None, _) => Vec::new(),
            (Some(_), observed) => vec![self.diagnostic(
                CodexDiagnosticCode::ScopeStale,
                Some("turn/completed"),
                observed.as_deref(),
                "turn completion for a non-active turn ignored",
            )],
        }
    }

    fn handle_item_started(&mut self, scope: &CodexInteractionScopeV1, frame: &Value) -> Vec<CodexInteractionOutcome> {
        let params = frame.get("params").cloned().unwrap_or_default();
        let item = params.get("item").unwrap_or(&params);
        let item_id = bounded_scope_id(item.get("id").and_then(Value::as_str));
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
        match item_type {
            "agentMessage" | "agent_message" => {
                let phase = CodexAssistantPhase::from_wire(item.get("phase").and_then(Value::as_str));
                let mut outcomes = Vec::new();
                if phase == CodexAssistantPhase::Unknown {
                    outcomes.push(self.diagnostic(
                        CodexDiagnosticCode::PhaseMissing,
                        Some("item/started"),
                        Some(&item_id),
                        "agentMessage without phase",
                    ));
                }
                self.track_item(&item_id, CodexItemState::AgentMessage { phase, completed: false });
                outcomes.push(CodexInteractionOutcome::Event(
                    CodexTimelineEventV1::AssistantStarted {
                        scope: scope.clone(),
                        item_id,
                        phase,
                    },
                ));
                outcomes
            }
            "reasoning" => {
                // raw reasoning content 只统计不投影（§2 决策 6）。
                self.track_item(&item_id, CodexItemState::Reasoning { completed: false });
                vec![self.diagnostic(
                    CodexDiagnosticCode::ReasoningRawDropped,
                    Some("item/started"),
                    Some(&item_id),
                    "raw reasoning item content dropped",
                )]
            }
            "contextCompaction" | "context_compaction" => {
                self.track_item(&item_id, CodexItemState::Other { completed: false });
                vec![CodexInteractionOutcome::Event(CodexTimelineEventV1::ContextCompacted {
                    scope: scope.clone(),
                    item_id: Some(item_id),
                })]
            }
            "userMessage" | "user_message" | "hookPrompt" | "hook_prompt" | "plan" => {
                self.track_item(&item_id, CodexItemState::Other { completed: false });
                Vec::new()
            }
            other => {
                let kind = CodexToolKind::from_wire(other);
                if kind == CodexToolKind::Unknown {
                    self.track_item(&item_id, CodexItemState::Other { completed: false });
                    return vec![self.diagnostic(
                        CodexDiagnosticCode::UnknownItemKind,
                        Some("item/started"),
                        Some(&item_id),
                        format!("item.type={} len={}", sanitize_kind_name(other), other.len()),
                    )];
                }
                self.track_item(&item_id, CodexItemState::Tool { kind, completed: false });
                vec![CodexInteractionOutcome::Event(CodexTimelineEventV1::ToolStarted {
                    scope: scope.clone(),
                    item_id,
                    kind,
                    safe_input: codex_tool_safe_input(item, kind),
                })]
            }
        }
    }

    fn handle_item_completed(
        &mut self,
        scope: &CodexInteractionScopeV1,
        frame: &Value,
    ) -> Vec<CodexInteractionOutcome> {
        let params = frame.get("params").cloned().unwrap_or_default();
        let item = params.get("item").unwrap_or(&params);
        let item_id = bounded_scope_id(item.get("id").and_then(Value::as_str));
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
        match item_type {
            "agentMessage" | "agent_message" => {
                if self.item_already_completed(&item_id) {
                    return vec![self.diagnostic(
                        CodexDiagnosticCode::DuplicateCompleted,
                        Some("item/completed"),
                        Some(&item_id),
                        "duplicate agentMessage completed frame",
                    )];
                }
                let phase = CodexAssistantPhase::from_wire(item.get("phase").and_then(Value::as_str));
                let mut outcomes = Vec::new();
                if phase == CodexAssistantPhase::Unknown {
                    outcomes.push(self.diagnostic(
                        CodexDiagnosticCode::PhaseMissing,
                        Some("item/completed"),
                        Some(&item_id),
                        "agentMessage completed without phase",
                    ));
                }
                let raw_text = item
                    .get("text")
                    .or_else(|| item.get("content"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let authoritative_text = safe_text(raw_text, MAX_AUTHORITATIVE_TEXT_CHARS);
                self.mark_completed(&item_id, CodexItemState::AgentMessage { phase, completed: true });
                outcomes.push(CodexInteractionOutcome::Event(
                    CodexTimelineEventV1::AssistantCompleted {
                        scope: scope.clone(),
                        item_id,
                        phase,
                        authoritative_text,
                    },
                ));
                outcomes
            }
            "reasoning" => {
                if self.item_already_completed(&item_id) {
                    return vec![self.diagnostic(
                        CodexDiagnosticCode::DuplicateCompleted,
                        Some("item/completed"),
                        Some(&item_id),
                        "duplicate reasoning completed frame",
                    )];
                }
                let mut parts: Vec<&str> = Vec::new();
                if let Some(summary) = item.get("summary").and_then(Value::as_array) {
                    for part in summary {
                        if let Some(text) = part.as_str() {
                            parts.push(text);
                        }
                    }
                }
                let joined = parts.join("\n\n");
                let public_summary = safe_text(&joined, MAX_REASONING_SUMMARY_CHARS);
                self.mark_completed(&item_id, CodexItemState::Reasoning { completed: true });
                vec![CodexInteractionOutcome::Event(
                    CodexTimelineEventV1::ReasoningSummaryCompleted {
                        scope: scope.clone(),
                        item_id,
                        public_summary,
                    },
                )]
            }
            "contextCompaction" | "context_compaction" => {
                self.mark_completed(&item_id, CodexItemState::Other { completed: true });
                vec![CodexInteractionOutcome::Event(CodexTimelineEventV1::ContextCompacted {
                    scope: scope.clone(),
                    item_id: Some(item_id),
                })]
            }
            "userMessage" | "user_message" | "hookPrompt" | "hook_prompt" | "plan" => {
                self.mark_completed(&item_id, CodexItemState::Other { completed: true });
                Vec::new()
            }
            other => {
                let kind = CodexToolKind::from_wire(other);
                if kind == CodexToolKind::Unknown {
                    self.mark_completed(&item_id, CodexItemState::Other { completed: true });
                    return vec![self.diagnostic(
                        CodexDiagnosticCode::UnknownItemKind,
                        Some("item/completed"),
                        Some(&item_id),
                        format!("item.type={} len={}", sanitize_kind_name(other), other.len()),
                    )];
                }
                if self.item_already_completed(&item_id) {
                    return vec![self.diagnostic(
                        CodexDiagnosticCode::DuplicateCompleted,
                        Some("item/completed"),
                        Some(&item_id),
                        "duplicate tool completed frame",
                    )];
                }
                let status = CodexToolStatus::from_wire(item.get("status").and_then(Value::as_str));
                let exit_code = item.get("exitCode").and_then(Value::as_i64);
                let safe_output = codex_tool_safe_output(item);
                self.mark_completed(&item_id, CodexItemState::Tool { kind, completed: true });
                vec![CodexInteractionOutcome::Event(
                    CodexTimelineEventV1::ToolCompleted {
                        scope: scope.clone(),
                        item_id,
                        kind,
                        status,
                        exit_code,
                        safe_output,
                    },
                )]
            }
        }
    }

    fn handle_user_input_request(
        &mut self,
        scope: CodexInteractionScopeV1,
        frame: &Value,
    ) -> Vec<CodexInteractionOutcome> {
        if !self.capabilities.supports_request_user_input {
            return vec![self.diagnostic(
                CodexDiagnosticCode::LegacyName,
                Some("item/tool/requestUserInput"),
                None,
                "requestUserInput received but capability snapshot says unsupported",
            )];
        }
        // 解析与请求桥共用（M3-01）；归一化器对可恢复问题降级为诊断，
        // 保留可投影的问题集合。
        let (parsed, issues) = parse_user_input_request(frame);
        let mut outcomes = Vec::new();
        for issue in issues {
            let detail = match &issue {
                UserInputParseIssue::MissingRequestId => {
                    "server request without usable id".to_string()
                }
                UserInputParseIssue::MissingQuestions => {
                    "requestUserInput without required questions array".to_string()
                }
                UserInputParseIssue::TooManyQuestions => format!("questions>{MAX_QUESTIONS}"),
                UserInputParseIssue::QuestionMissingId { index } => {
                    format!("question[{index}] missing id")
                }
                UserInputParseIssue::DuplicateQuestionId { id } => {
                    format!("duplicate question id len={}", id.len())
                }
            };
            let code = match issue {
                UserInputParseIssue::MissingRequestId => CodexDiagnosticCode::ScopeMissing,
                UserInputParseIssue::DuplicateQuestionId { .. } => {
                    CodexDiagnosticCode::DuplicateCompleted
                }
                _ => CodexDiagnosticCode::PayloadRejected,
            };
            outcomes.push(self.diagnostic(
                code,
                Some("item/tool/requestUserInput"),
                None,
                detail,
            ));
        }
        let Some(parsed) = parsed else {
            return outcomes;
        };
        let ParsedUserInputRequest {
            request_id,
            item_id,
            auto_resolution_ms,
            questions,
        } = parsed;
        self.pending_user_inputs
            .insert(request_id.clone(), item_id.clone());
        outcomes.push(CodexInteractionOutcome::Event(
            CodexTimelineEventV1::UserInputRequested {
                scope,
                item_id,
                transport_generation: self.transport_generation,
                request_id,
                questions,
                auto_resolution_ms,
            },
        ));
        outcomes
    }

    // -- scope 门禁 ---------------------------------------------------------

    fn admit(&mut self, method: &str, frame: &Value) -> CodexScopeGate {
        let requirement = codex_scope_requirement(method);
        let thread_present = frame
            .pointer("/params/threadId")
            .and_then(Value::as_str)
            .map(|value| bounded_scope_id(Some(value)));
        let turn_present = frame
            .pointer("/params/turnId")
            .and_then(Value::as_str)
            .map(|value| bounded_scope_id(Some(value)));

        // 无 scope 需求的方法（warning）：携带 threadId 时仍须匹配。
        let Some(requirement) = requirement else {
            return match (&self.thread_id, thread_present) {
                (Some(expected), Some(observed)) if &observed == expected => {
                    CodexScopeGate::Optional(Some(CodexInteractionScopeV1 {
                        run_id: self.run_id.clone(),
                        thread_id: expected.clone(),
                        turn_id: self.active_turn.clone(),
                    }))
                }
                (Some(_), Some(_)) => {
                    self.count(CodexDiagnosticCode::ScopeStale);
                    CodexScopeGate::Denied
                }
                (_, None) => CodexScopeGate::Optional(None),
                (None, Some(_)) => {
                    self.count(CodexDiagnosticCode::ScopeMissing);
                    CodexScopeGate::Denied
                }
            };
        };

        let Some(expected_thread) = self.thread_id.clone() else {
            // 线程尚未建立（thread/started 未到）：run 事件不允许进入。
            self.count(CodexDiagnosticCode::ScopeMissing);
            return CodexScopeGate::Denied;
        };
        match thread_present {
            Some(observed) if observed == expected_thread => {}
            Some(_) => {
                self.count(CodexDiagnosticCode::ScopeStale);
                return CodexScopeGate::Denied;
            }
            None => {
                self.count(CodexDiagnosticCode::ScopeMissing);
                return CodexScopeGate::Denied;
            }
        }
        if requirement == CodexScopeRequirement::ThreadAndTurn {
            match (&self.active_turn, turn_present) {
                (Some(active), Some(observed)) if active == &observed => {}
                (Some(_), Some(_)) => {
                    self.count(CodexDiagnosticCode::ScopeStale);
                    return CodexScopeGate::Denied;
                }
                _ => {
                    // 活动 turn 未知（兼容旧流）或帧缺 turnId：不允许进入。
                    self.count(CodexDiagnosticCode::ScopeMissing);
                    return CodexScopeGate::Denied;
                }
            }
        }
        CodexScopeGate::Scoped(CodexInteractionScopeV1 {
            run_id: self.run_id.clone(),
            thread_id: expected_thread,
            turn_id: self.active_turn.clone(),
        })
    }

    // -- item 状态 ----------------------------------------------------------

    fn track_item(&mut self, item_id: &str, state: CodexItemState) {
        if self.items.len() >= MAX_TRACKED_ITEMS && !self.items.contains_key(item_id) {
            // 有界注册表：淘汰最早完成项之外无序保证——简化为清掉已完成项。
            let completed: Vec<String> = self
                .items
                .iter()
                .filter(|(_, state)| matches!(state, CodexItemState::AgentMessage { completed, .. } if *completed)
                    || matches!(state, CodexItemState::Tool { completed, .. } if *completed)
                    || matches!(state, CodexItemState::Reasoning { completed, .. } if *completed)
                    || matches!(state, CodexItemState::Other { completed, .. } if *completed))
                .map(|(id, _)| id.clone())
                .take(self.items.len().saturating_sub(MAX_TRACKED_ITEMS) + 1)
                .collect();
            for id in completed {
                self.items.remove(&id);
            }
        }
        self.items.insert(item_id.to_string(), state);
    }

    fn item_already_completed(&self, item_id: &str) -> bool {
        matches!(
            self.items.get(item_id),
            Some(CodexItemState::AgentMessage { completed: true, .. })
                | Some(CodexItemState::Tool { completed: true, .. })
                | Some(CodexItemState::Reasoning { completed: true, .. })
                | Some(CodexItemState::Other { completed: true, .. })
        )
    }

    fn mark_completed(&mut self, item_id: &str, state: CodexItemState) {
        self.items.insert(item_id.to_string(), state);
    }

    fn agent_phase_for(&self, item_id: &str) -> CodexAssistantPhase {
        match self.items.get(item_id) {
            Some(CodexItemState::AgentMessage { phase, .. }) => *phase,
            _ => CodexAssistantPhase::Unknown,
        }
    }

    fn diagnostic(
        &mut self,
        code: CodexDiagnosticCode,
        method: Option<&str>,
        item_id: Option<&str>,
        detail: impl Into<String>,
    ) -> CodexInteractionOutcome {
        self.count(code);
        let mut detail = detail.into();
        detail = safe_text(&detail, 256);
        CodexInteractionOutcome::Diagnostic(CodexInteractionDiagnosticV1 {
            code,
            method: method.map(sanitize_kind_name),
            item_id: item_id.map(|id| bounded_scope_id(Some(id))),
            detail,
        })
    }

    fn count(&mut self, code: CodexDiagnosticCode) {
        *self.diagnostic_counts.entry(code).or_insert(0) += 1;
    }
}

// ---------------------------------------------------------------------------
// 转换辅助（纯函数）
// ---------------------------------------------------------------------------

fn item_delta_parts(frame: &Value) -> Option<(String, String)> {
    let item_id = frame.pointer("/params/itemId").and_then(Value::as_str)?;
    let delta = frame.pointer("/params/delta").and_then(Value::as_str)?;
    Some((bounded_scope_id(Some(item_id)), delta.to_string()))
}

/// JSON-RPC id / requestId 归一化（pub 供请求桥复用）：数字取十进制文本，
/// 字符串去引号原样返回。两侧（请求帧的 id 与 resolved 帧的 requestId）
/// 必须同构才能关联。
pub fn wire_request_id(value: Option<&Value>) -> String {
    match value {
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::String(text)) => bounded_scope_id(Some(text)),
        _ => String::new(),
    }
}

/// 脱敏 + 截断：先 redact（token/凭据形态），再按字符数截断。
/// 控制字符仅保留 \n\t\r，避免二进制噪声进入持久化。
pub fn safe_text(value: &str, max_chars: usize) -> String {
    let (bounded, _) = safe_text_checked(value, max_chars);
    bounded
}

fn safe_text_checked(value: &str, max_chars: usize) -> (String, bool) {
    let redacted = redact_text(value);
    let cleaned: String = redacted
        .chars()
        .map(|c| {
            if c == '\n' || c == '\t' || c == '\r' {
                c
            } else if c.is_control() {
                ' '
            } else {
                c
            }
        })
        .collect();
    let mut chars = cleaned.chars();
    let mut bounded: String = chars.by_ref().take(max_chars).collect();
    let truncated = chars.next().is_some();
    if truncated {
        bounded.push('…');
    }
    (bounded, truncated)
}

fn bounded_scope_id(value: Option<&str>) -> String {
    value
        .map(|raw| raw.trim().chars().take(MAX_SCOPE_ID_CHARS).collect::<String>())
        .unwrap_or_default()
}

fn sanitize_kind_name(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '/' || *c == '_' || *c == '-' || *c == '.')
        .take(64)
        .collect()
}

/// App Server item kind → R-Code 活动卡标题（M2-01 步骤 1 的映射表，
/// 单一事实源；与 safe_input 摘要一起驱动工具卡折叠态）。
pub fn codex_tool_display_title(kind: CodexToolKind) -> &'static str {
    match kind {
        CodexToolKind::CommandExecution => "Codex 命令",
        CodexToolKind::FileChange => "Codex 文件修改",
        CodexToolKind::McpToolCall => "Codex MCP",
        CodexToolKind::DynamicToolCall => "Codex 动态工具",
        CodexToolKind::CollabAgentToolCall => "Codex 协作工具",
        CodexToolKind::WebSearch => "Codex 搜索",
        CodexToolKind::ImageView => "Codex 图片查看",
        CodexToolKind::ImageGeneration => "Codex 图片生成",
        CodexToolKind::Sleep => "Codex 等待",
        CodexToolKind::SubAgentActivity => "Codex 子代理活动",
        CodexToolKind::Unknown => "Codex 工具",
    }
}

/// rcode_delegate 动态委派的参数白名单（与旧 codex_item_tool_input 同一
/// 边界：只透出 agent/label/goal/access 的有界文本；无 goal 时回退摘要）。
fn rcode_delegate_safe_input(item: &Value, summary: &str) -> Option<serde_json::Value> {
    let tool = item.get("tool").and_then(Value::as_str)?;
    if tool != "rcode_delegate_subagent" {
        return None;
    }
    let mut safe = serde_json::Map::new();
    safe.insert("agent".to_string(), Value::String("r_code".to_string()));
    if let Some(arguments) = item.get("arguments").and_then(Value::as_object) {
        for (key, max_chars) in [("label", 80usize), ("goal", 4_000), ("access", 40)] {
            if let Some(value) = arguments
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                safe.insert(key.to_string(), Value::String(safe_text(value, max_chars)));
            }
        }
    }
    if !safe.contains_key("goal") {
        safe.insert("summary".to_string(), Value::String(summary.to_string()));
    }
    Some(serde_json::Value::Object(safe))
}

fn codex_tool_safe_input(item: &Value, kind: CodexToolKind) -> CodexSafeInputV1 {
    let bounded = |value: &str| safe_text(value.trim(), MAX_TOOL_FIELD_CHARS);
    // 诊断保留 wire 类型名（与 0.145.0 schema 对齐），而非 Rust 枚举名。
    let raw_kind_name = match kind {
        CodexToolKind::Unknown => None,
        CodexToolKind::CommandExecution => Some("commandExecution"),
        CodexToolKind::FileChange => Some("fileChange"),
        CodexToolKind::McpToolCall => Some("mcpToolCall"),
        CodexToolKind::DynamicToolCall => Some("dynamicToolCall"),
        CodexToolKind::CollabAgentToolCall => Some("collabAgentToolCall"),
        CodexToolKind::WebSearch => Some("webSearch"),
        CodexToolKind::ImageView => Some("imageView"),
        CodexToolKind::ImageGeneration => Some("imageGeneration"),
        CodexToolKind::Sleep => Some("sleep"),
        CodexToolKind::SubAgentActivity => Some("subAgentActivity"),
    }
    .map(str::to_string);
    let (summary, tool_name, input_json) = match kind {
        CodexToolKind::CommandExecution => {
            let command = item
                .get("command")
                .and_then(Value::as_str)
                .or_else(|| item.pointer("/command/0").and_then(Value::as_str))
                .unwrap_or("读取工作区");
            (bounded(command), None, None)
        }
        CodexToolKind::FileChange => {
            let paths: Vec<&str> = item
                .get("changes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|change| change.get("path").and_then(Value::as_str))
                .take(3)
                .collect();
            let summary = if paths.is_empty() {
                "更新工作区文件".to_string()
            } else {
                format!("更新 {}", paths.join("、"))
            };
            (safe_text(&summary, MAX_TOOL_FIELD_CHARS), None, None)
        }
        CodexToolKind::McpToolCall => {
            let server = item
                .get("server")
                .or_else(|| item.get("server_name"))
                .and_then(Value::as_str)
                .unwrap_or("MCP");
            let tool = item
                .get("tool")
                .or_else(|| item.get("tool_name"))
                .and_then(Value::as_str)
                .unwrap_or("工具");
            (
                bounded(&format!("{server} · {tool}")),
                Some(bounded(tool)),
                None,
            )
        }
        CodexToolKind::DynamicToolCall => {
            let tool = item
                .get("tool")
                .and_then(Value::as_str)
                .unwrap_or("工具");
            let namespace = item
                .get("namespace")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let label = namespace
                .map(|value| format!("{value} · {tool}"))
                .unwrap_or_else(|| tool.to_string());
            if tool == "rcode_delegate_subagent" {
                let arguments = item.get("arguments").and_then(Value::as_object);
                let task_label = arguments
                    .and_then(|arguments| arguments.get("label"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty());
                let goal = arguments
                    .and_then(|arguments| arguments.get("goal"))
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty());
                let summary = match (task_label, goal) {
                    (Some(task_label), Some(goal)) => format!("{task_label} · {goal}"),
                    (Some(task_label), None) => task_label.to_string(),
                    (None, Some(goal)) => goal.to_string(),
                    (None, None) => "委派 R-Code 子智能体".to_string(),
                };
                (
                    safe_text(&summary, MAX_TOOL_FIELD_CHARS),
                    Some(bounded(tool)),
                    rcode_delegate_safe_input(item, &summary),
                )
            } else {
                (bounded(&label), Some(bounded(tool)), None)
            }
        }
        CodexToolKind::CollabAgentToolCall => {
            let tool = item.get("tool").and_then(Value::as_str).unwrap_or("协作");
            (bounded(tool), Some(bounded(tool)), None)
        }
        CodexToolKind::WebSearch => {
            let query = item.get("query").and_then(Value::as_str).unwrap_or("搜索资料");
            (bounded(query), None, None)
        }
        CodexToolKind::ImageView => {
            let path = item.get("path").and_then(Value::as_str).unwrap_or("");
            (bounded(path), None, None)
        }
        CodexToolKind::SubAgentActivity => {
            let activity = item.get("kind").and_then(Value::as_str).unwrap_or("子代理");
            (bounded(activity), None, None)
        }
        CodexToolKind::ImageGeneration | CodexToolKind::Sleep | CodexToolKind::Unknown => {
            (String::new(), None, None)
        }
    };
    CodexSafeInputV1 {
        raw_kind_name,
        summary,
        tool_name,
        input_json,
    }
}

fn codex_tool_safe_output(item: &Value) -> Option<String> {
    let raw = item
        .get("aggregatedOutput")
        .or_else(|| item.get("result"))
        .or_else(|| item.get("error"))
        .and_then(Value::as_str)?;
    Some(safe_text(raw, MAX_TOOL_FIELD_CHARS))
}

fn parse_token_usage(value: &Value) -> CodexTokenUsageV1 {
    CodexTokenUsageV1 {
        last: parse_usage_bucket(value.get("last")),
        total: parse_usage_bucket(value.get("total")),
    }
}

fn parse_usage_bucket(value: Option<&Value>) -> CodexTokenUsageBucketV1 {
    let Some(value) = value else {
        return CodexTokenUsageBucketV1::default();
    };
    let number = |key: &str| value.get(key).and_then(Value::as_i64).unwrap_or(0);
    CodexTokenUsageBucketV1 {
        input_tokens: number("inputTokens"),
        cached_input_tokens: number("cachedInputTokens"),
        cache_write_input_tokens: number("cacheWriteInputTokens"),
        output_tokens: number("outputTokens"),
        reasoning_output_tokens: number("reasoningOutputTokens"),
        total_tokens: number("totalTokens"),
    }
}

#[cfg(test)]
#[path = "codex_interaction_tests.rs"]
mod tests;
