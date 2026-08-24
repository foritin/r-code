//! Host-owned Plan lifecycle tools exposed to the native main agent.
//!
//! These tools never trust task/run/tool-call ownership from model JSON. The gateway binds that
//! identity in [`ToolExecutionContext`], and the store re-checks every Plan resource against the
//! context task before mutating SQLite.

use std::sync::Arc;

use async_trait::async_trait;
use r_code_core::dto::{RiskLevel, TaskMode};
use r_code_core::error::ProductError;
use r_code_core::plan::{
    PlanItemDraft, PlanItemState, PlanQuestionDraft, PlanView, PublishPlanInput,
    RequestPlanQuestionsInput, RequestScopeDecisionInput, UpdatePlanItemInput,
};
use r_code_core::plan_entry::{PlanComplexitySignal, ResolvedPlanRuntimeProfile};
use r_code_gateway::{PathBinding, Tool, ToolExecutionContext, ToolExecutionResult};
use r_code_store::{Database, PlanEntryStore, PlanStore, SessionBranchRepository, TaskRepository};
use serde::Deserialize;

use crate::plan_policy::{
    customer_copy_template, provider_route_snapshot, sanitize_reason_for_audit,
    ArmedPlanSuggestion, PlanSuggestionGate, PROVIDER_PROFILE_VERSION,
};

fn invalid(message: impl Into<String>) -> ProductError {
    ProductError::StateMachineError(message.into())
}

fn decode<T: for<'de> Deserialize<'de>>(input: serde_json::Value) -> Result<T, ProductError> {
    serde_json::from_value(input)
        .map_err(|error| invalid(format!("invalid Plan tool input: {error}")))
}

fn require_task_mode(
    db: &Database,
    task_id: &str,
    allowed: &[TaskMode],
    operation: &str,
) -> Result<(), ProductError> {
    let task = TaskRepository::new(db)
        .get(task_id)?
        .ok_or_else(|| invalid(format!("task does not exist: {task_id}")))?;
    if allowed.contains(&task.mode) {
        Ok(())
    } else {
        Err(invalid(format!(
            "{operation} is unavailable while task mode is {}",
            task.mode
        )))
    }
}

fn context_required() -> ProductError {
    invalid("this host Plan tool requires gateway-owned execution context")
}

fn authoritative_plan_metadata(view: &PlanView) -> serde_json::Value {
    serde_json::json!({ "r_code_authoritative_plan_view": view })
}

fn plan_item_execution_result(content: String, view: &PlanView) -> ToolExecutionResult {
    let active = view
        .items
        .iter()
        .any(|item| item.state == PlanItemState::InProgress);
    let result = if active {
        ToolExecutionResult::require_agent_continuation(content)
    } else {
        ToolExecutionResult::allow_agent_completion(content)
    };
    result.with_metadata(authoritative_plan_metadata(view))
}

fn recoverable_plan_update_conflict(error: &ProductError) -> bool {
    matches!(
        error,
        ProductError::StateMachineError(message)
            if message.starts_with("stale Plan revision:")
                || message.starts_with("invalid Plan feature transition:")
                || message == "another Plan feature is already in progress"
                || message == "Plan changed while its feature was being updated"
                || message.starts_with("cannot update a feature while Plan is ")
    )
}

const ENTER_PLAN_CONTINUATION: &str = "[system] R-Code safely changed this task from Agent mode to Plan mode. Continue the user's existing request as a structured Plan. Investigate read-only as needed, ask only blocking questions with request_user_input, then publish the complete functional Plan with plan_publish. Do not edit files or execute implementation before the user approves the Plan.";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnterPlanModeArgs {
    reason: String,
}

/// 宿主解析的冻结 Plan profile 来源（docs §14.2）：按任务绑定的 Provider route
/// 现场解析；工具与存储层都不读取 Provider 设置或证据文件。
pub type TaskPlanProfileResolver = Arc<dyn Fn(&str) -> ResolvedPlanRuntimeProfile + Send + Sync>;

#[derive(Clone)]
pub struct EnterPlanModeTool {
    db: Arc<Database>,
    plans: Arc<PlanStore>,
    profile_resolver: TaskPlanProfileResolver,
}

impl EnterPlanModeTool {
    pub fn new(
        db: Arc<Database>,
        plans: Arc<PlanStore>,
        profile_resolver: TaskPlanProfileResolver,
    ) -> Self {
        Self {
            db,
            plans,
            profile_resolver,
        }
    }
}

#[async_trait]
impl Tool for EnterPlanModeTool {
    fn name(&self) -> &str {
        "enter_plan_mode"
    }

    fn description(&self) -> &str {
        "Safely change the current main task from Agent mode to Plan mode. Call this only after the user explicitly chose Plan mode or explicitly asked for a structured plan first — explicit consent needs no second confirmation. Never use this for automatic complexity routing; when planning should merely be suggested to the user, call propose_plan_mode instead. Do not use it for a single, isolated, immediately verifiable change. This ends the current Agent run and the host resumes the same request with read-only Plan tools. Returning to Agent mode requires explicit user approval; never use this to restart an already approved/executing Plan."
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R0
    }

    fn path_bindings(&self) -> &'static [PathBinding] {
        &[]
    }

    fn requires_existing_path(&self) -> bool {
        false
    }

    fn requires_workspace_scope(&self) -> bool {
        false
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "reason": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 2000,
                    "description": "Short user-visible reason why planning is needed before implementation."
                }
            },
            "required": ["reason"]
        })
    }

    async fn execute(&self, _input: serde_json::Value) -> Result<String, ProductError> {
        Err(context_required())
    }

    async fn execute_with_context(
        &self,
        input: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolExecutionResult, ProductError> {
        require_task_mode(
            &self.db,
            &context.task_id,
            &[TaskMode::Ask, TaskMode::Edit, TaskMode::Auto],
            "enter_plan_mode",
        )?;
        let args: EnterPlanModeArgs = decode(input)?;
        if args.reason.trim().is_empty() {
            return Err(invalid("enter_plan_mode reason cannot be blank"));
        }
        let branch = SessionBranchRepository::new(&self.db).ensure_active(&context.task_id)?;
        let profile = (self.profile_resolver)(&context.task_id);
        let view = self
            .plans
            .enter_plan_mode_and_stage_continuation_with_profile(
                &context.task_id,
                &branch.id,
                ENTER_PLAN_CONTINUATION,
                &profile,
                context.origin_request_key.as_deref(),
            )?;
        let content = serde_json::to_string(&serde_json::json!({
            "entered_plan_mode": true,
            "reason": args.reason.trim(),
            "plan": &view,
            "instruction": "Stop this Agent run. The host has queued a fresh Plan-mode continuation for the same request.",
        }))
        .map_err(|error| invalid(format!("serialize enter_plan_mode result: {error}")))?;
        Ok(ToolExecutionResult::suspend_for_user(content)
            .with_metadata(authoritative_plan_metadata(&view)))
    }
}

// ---------------------------------------------------------------------------
// Plan 入口建议（docs/archive/implementation/plan-mode-dual-track-gate.md §9）。模型只能建议「先制定
// 计划」，不能替客户切换模式：建议是 pending offer，任务保持原 Agent 模式。
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProposePlanModeArgs {
    reason: String,
    signals: Vec<PlanComplexitySignal>,
}

#[derive(Clone)]
pub struct ProposePlanModeTool {
    db: Arc<Database>,
    offers: Arc<PlanEntryStore>,
    gate: Arc<PlanSuggestionGate>,
}

impl ProposePlanModeTool {
    pub fn new(
        db: Arc<Database>,
        offers: Arc<PlanEntryStore>,
        gate: Arc<PlanSuggestionGate>,
    ) -> Self {
        Self { db, offers, gate }
    }
}

#[async_trait]
impl Tool for ProposePlanModeTool {
    fn name(&self) -> &str {
        "propose_plan_mode"
    }

    fn description(&self) -> &str {
        "Suggest to the user that this request should start with a structured plan, without switching modes yourself. Call this once when the request is genuinely complex: it spans multiple interdependent subsystems, involves data migration / protocol / persistence compatibility, needs a design or product decision the user must approve, a wrong attempt is expensive to roll back, or it cannot be verified safely in one pass. Submit 1-5 matching signals and a short internal reason. The host shows the user a one-time dialog and this run stops to wait for their decision. Do not call this for a single isolated fix, explanations, reviews, read-only checks, after the user asked to work directly, or twice for the same task."
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R0
    }

    fn path_bindings(&self) -> &'static [PathBinding] {
        &[]
    }

    fn requires_existing_path(&self) -> bool {
        false
    }

    fn requires_workspace_scope(&self) -> bool {
        false
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "reason": {
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 1000,
                    "description": "Short internal reason for the suggestion. Audited locally only; never shown to the user."
                },
                "signals": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 5,
                    "uniqueItems": true,
                    "items": {
                        "type": "string",
                        "enum": PlanComplexitySignal::ALL.iter().map(|signal| signal.as_str()).collect::<Vec<_>>(),
                        "description": "Controlled complexity signals. Pick only what truly applies."
                    }
                }
            },
            "required": ["reason", "signals"]
        })
    }

    async fn execute(&self, _input: serde_json::Value) -> Result<String, ProductError> {
        Err(context_required())
    }

    async fn execute_with_context(
        &self,
        input: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolExecutionResult, ProductError> {
        // 身份三重绑定：宿主 request key、宿主武装登记、任务模式。模型不能传入
        // task_id / run_id / request_key / 目标模式（deny_unknown_fields 兜底）。
        let Some(request_key) = context.origin_request_key.as_deref() else {
            return Err(invalid(
                "propose_plan_mode requires the host-bound origin request key",
            ));
        };
        let Some(armed) = self.gate.armed(&context.run_id) else {
            return Err(invalid(
                "propose_plan_mode is not available for this run (not suggestion-eligible)",
            ));
        };
        require_task_mode(
            &self.db,
            &context.task_id,
            &[TaskMode::Ask, TaskMode::Edit, TaskMode::Auto],
            "propose_plan_mode",
        )?;
        let args: ProposePlanModeArgs = decode(input)?;
        let reason = sanitize_reason_for_audit(&args.reason);
        if reason.is_empty() {
            return Err(invalid("propose_plan_mode reason cannot be blank"));
        }
        if args.signals.is_empty() || args.signals.len() > 5 {
            return Err(invalid("propose_plan_mode requires 1-5 signals"));
        }
        let mut unique = std::collections::HashSet::new();
        if !args.signals.iter().all(|signal| unique.insert(*signal)) {
            return Err(invalid("propose_plan_mode signals must be unique"));
        }
        let Some(primary_signal) = PlanComplexitySignal::primary_of(&args.signals) else {
            return Err(invalid("propose_plan_mode signals could not be ranked"));
        };
        let copy = customer_copy_template(primary_signal);
        let branch = SessionBranchRepository::new(&self.db).ensure_active(&context.task_id)?;
        let task = TaskRepository::new(&self.db)
            .get(&context.task_id)?
            .ok_or_else(|| invalid(format!("task does not exist: {}", context.task_id)))?;
        let snapshot = provider_route_snapshot(&armed.route, PROVIDER_PROFILE_VERSION);
        let offer = self
            .offers
            .create_offer(&r_code_store::CreatePlanEntryOfferInput {
                task_id: context.task_id.clone(),
                branch_id: branch.id,
                source_run_id: context.run_id.clone(),
                request_key: request_key.to_string(),
                original_mode: task.mode.to_string(),
                reason_audit: reason,
                signals: args.signals.clone(),
                primary_signal,
                customer_copy_key: copy.key.to_string(),
                customer_copy_version: copy.version,
                provider: snapshot,
                eligibility_profile_version: armed.control.eligibility_profile_version.clone(),
                evidence_version: armed.control.evidence_version.clone(),
                resolved_plan_runtime_profile: armed.profile.clone(),
            })?;
        let content = serde_json::to_string(&serde_json::json!({
            "proposed": true,
            "primary_signal": primary_signal.as_str(),
            "instruction": "Stop this Agent run and wait. The host is asking the user whether to start with a structured plan; do not continue editing while the suggestion is pending.",
        }))
        .map_err(|error| invalid(format!("serialize propose_plan_mode result: {error}")))?;
        Ok(
            ToolExecutionResult::suspend_for_user(content).with_metadata(serde_json::json!({
                "r_code_plan_entry_offer": &offer,
                "plan_entry_offer_id": &offer.id,
            })),
        )
    }
}

/// 预留：宿主武装登记的只读探针（诊断与测试用）。
impl ProposePlanModeTool {
    pub fn gate_armed(&self, run_id: &str) -> Option<ArmedPlanSuggestion> {
        self.gate.armed(run_id)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishArgs {
    plan_id: String,
    expected_revision: u64,
    items: Vec<PlanItemDraft>,
}

#[derive(Clone)]
pub struct PlanPublishTool {
    db: Arc<Database>,
    plans: Arc<PlanStore>,
}

impl PlanPublishTool {
    pub fn new(db: Arc<Database>, plans: Arc<PlanStore>) -> Self {
        Self { db, plans }
    }
}

#[async_trait]
impl Tool for PlanPublishTool {
    fn name(&self) -> &str {
        "plan_publish"
    }

    fn description(&self) -> &str {
        "Publish the current Plan draft as 1-100 independently verifiable executable leaf items. Use section_path for optional hierarchy (for example phase -> area) instead of creating non-executable parent items. Each section_path label must be descriptive text, never a bare number; numbering is rendered automatically. Each leaf must represent one coherent implementation outcome with explicit acceptance criteria and only real dependencies."
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R0
    }

    fn path_bindings(&self) -> &'static [PathBinding] {
        &[]
    }

    fn requires_existing_path(&self) -> bool {
        false
    }

    fn requires_workspace_scope(&self) -> bool {
        false
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "plan_id": { "type": "string", "minLength": 1 },
                "expected_revision": { "type": "integer", "minimum": 1 },
                "items": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 100,
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "id": { "type": "string", "minLength": 1, "maxLength": 256 },
                            "title": { "type": "string", "minLength": 1, "maxLength": 200 },
                            "description": {
                                "type": "string",
                                "maxLength": 20000,
                                "description": "Markdown-supported implementation and acceptance details. Keep simple items concise. For multi-part items, use short sections and lists with paths or commands in inline code instead of one semicolon-packed paragraph."
                            },
                            "section_path": {
                                "type": "array",
                                "maxItems": 4,
                                "description": "Optional presentation hierarchy. Parent labels are not executable tasks; this item remains the tracked leaf. Labels must be descriptive text; bare numbers are rejected because numbering is added automatically.",
                                "items": { "type": "string", "minLength": 1, "maxLength": 120 }
                            },
                            "depends_on": {
                                "type": "array",
                                "items": { "type": "string", "minLength": 1, "maxLength": 256 },
                                "uniqueItems": true
                            }
                        },
                        "required": ["id", "title", "description"]
                    }
                }
            },
            "required": ["plan_id", "expected_revision", "items"]
        })
    }

    async fn execute(&self, _input: serde_json::Value) -> Result<String, ProductError> {
        Err(context_required())
    }

    async fn execute_with_context(
        &self,
        input: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolExecutionResult, ProductError> {
        require_task_mode(
            &self.db,
            &context.task_id,
            &[TaskMode::Plan],
            "plan_publish",
        )?;
        let args: PublishArgs = decode(input)?;
        let view = self.plans.publish_plan(
            &context.task_id,
            &PublishPlanInput {
                plan_id: args.plan_id,
                expected_revision: args.expected_revision,
                items: args.items,
            },
        )?;
        let content = serde_json::to_string(&serde_json::json!({
            "published": true,
            "plan": &view,
        }))
        .map_err(|error| invalid(format!("serialize plan_publish result: {error}")))?;
        Ok(ToolExecutionResult::success(content).with_metadata(authoritative_plan_metadata(&view)))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestUserInputArgs {
    plan_id: String,
    expected_revision: u64,
    questions: Vec<PlanQuestionDraft>,
}

#[derive(Clone)]
pub struct RequestUserInputTool {
    db: Arc<Database>,
    plans: Arc<PlanStore>,
}

impl RequestUserInputTool {
    pub fn new(db: Arc<Database>, plans: Arc<PlanStore>) -> Self {
        Self { db, plans }
    }
}

#[async_trait]
impl Tool for RequestUserInputTool {
    fn name(&self) -> &str {
        "request_user_input"
    }

    fn description(&self) -> &str {
        "Ask the user 1-3 short blocking Plan questions and suspend the run. Each question must provide 2-3 mutually exclusive choices; put the recommended choice first and explain its tradeoff. The UI also provides a free-form response path."
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R0
    }

    fn path_bindings(&self) -> &'static [PathBinding] {
        &[]
    }

    fn requires_existing_path(&self) -> bool {
        false
    }

    fn requires_workspace_scope(&self) -> bool {
        false
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "plan_id": { "type": "string", "minLength": 1 },
                "expected_revision": { "type": "integer", "minimum": 1 },
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 3,
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "id": { "type": "string", "minLength": 1, "maxLength": 256 },
                            "header": { "type": "string", "minLength": 1, "maxLength": 64 },
                            "question": { "type": "string", "minLength": 1, "maxLength": 2000 },
                            "options": {
                                "type": "array",
                                "minItems": 2,
                                "maxItems": 3,
                                "items": {
                                    "type": "object",
                                    "additionalProperties": false,
                                    "properties": {
                                        "id": { "type": "string", "minLength": 1, "maxLength": 256 },
                                        "label": { "type": "string", "minLength": 1, "maxLength": 120 },
                                        "description": { "type": "string", "maxLength": 1000 }
                                    },
                                    "required": ["id", "label", "description"]
                                }
                            }
                        },
                        "required": ["id", "header", "question", "options"]
                    }
                }
            },
            "required": ["plan_id", "expected_revision", "questions"]
        })
    }

    async fn execute(&self, _input: serde_json::Value) -> Result<String, ProductError> {
        Err(context_required())
    }

    async fn execute_with_context(
        &self,
        input: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolExecutionResult, ProductError> {
        require_task_mode(
            &self.db,
            &context.task_id,
            &[TaskMode::Plan],
            "request_user_input",
        )?;
        let args: RequestUserInputArgs = decode(input)?;
        let view = self.plans.request_questions(
            &context.task_id,
            &RequestPlanQuestionsInput {
                plan_id: args.plan_id,
                expected_revision: args.expected_revision,
                questions: args.questions,
            },
        )?;
        let question_set = view
            .pending_question_set
            .clone()
            .ok_or_else(|| invalid("Plan store did not return the pending question set"))?;
        let content = serde_json::to_string(&serde_json::json!({
            "status": "awaiting_user_input",
            "question_set": &question_set,
            "plan": &view,
        }))
        .map_err(|error| invalid(format!("serialize request_user_input result: {error}")))?;
        Ok(
            ToolExecutionResult::suspend_for_user(content).with_metadata(serde_json::json!({
                "question_set": &question_set,
                "plan": &view,
                "r_code_authoritative_plan_view": &view,
            })),
        )
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestScopeDecisionArgs {
    questions: Vec<PlanQuestionDraft>,
}

#[derive(Clone)]
pub struct RequestScopeDecisionTool {
    db: Arc<Database>,
    plans: Arc<PlanStore>,
}

impl RequestScopeDecisionTool {
    pub fn new(db: Arc<Database>, plans: Arc<PlanStore>) -> Self {
        Self { db, plans }
    }
}

#[async_trait]
impl Tool for RequestScopeDecisionTool {
    fn name(&self) -> &str {
        "request_scope_decision"
    }

    fn description(&self) -> &str {
        "Raise a blocking scope decision while the task is still in Agent mode. Use this when an attached image, OCR, or multi-part request is ambiguous about what the user actually wants, and proceeding without confirmation risks doing the wrong work. Ask 1-3 short questions with 2-3 mutually exclusive choices each and put the recommended choice first; the host switches the task to Plan mode, shows the questions, and resumes the original Agent task after the user answers. Do not use this from Plan mode."
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R0
    }

    fn path_bindings(&self) -> &'static [PathBinding] {
        &[]
    }

    fn requires_existing_path(&self) -> bool {
        false
    }

    fn requires_workspace_scope(&self) -> bool {
        false
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 3,
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "id": { "type": "string", "minLength": 1, "maxLength": 256 },
                            "header": { "type": "string", "minLength": 1, "maxLength": 64 },
                            "question": { "type": "string", "minLength": 1, "maxLength": 2000 },
                            "options": {
                                "type": "array",
                                "minItems": 2,
                                "maxItems": 3,
                                "items": {
                                    "type": "object",
                                    "additionalProperties": false,
                                    "properties": {
                                        "id": { "type": "string", "minLength": 1, "maxLength": 256 },
                                        "label": { "type": "string", "minLength": 1, "maxLength": 120 },
                                        "description": { "type": "string", "maxLength": 1000 }
                                    },
                                    "required": ["id", "label", "description"]
                                }
                            }
                        },
                        "required": ["id", "header", "question", "options"]
                    }
                }
            },
            "required": ["questions"]
        })
    }

    async fn execute(&self, _input: serde_json::Value) -> Result<String, ProductError> {
        Err(context_required())
    }

    async fn execute_with_context(
        &self,
        input: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolExecutionResult, ProductError> {
        require_task_mode(
            &self.db,
            &context.task_id,
            &[TaskMode::Ask, TaskMode::Edit, TaskMode::Auto],
            "request_scope_decision",
        )?;
        let args: RequestScopeDecisionArgs = decode(input)?;
        let view = self.plans.request_scope_decision(
            &context.task_id,
            &RequestScopeDecisionInput {
                questions: args.questions,
            },
        )?;
        let question_set = view
            .pending_question_set
            .clone()
            .ok_or_else(|| invalid("Plan store did not return the pending scope decision"))?;
        let content = serde_json::to_string(&serde_json::json!({
            "status": "awaiting_user_input",
            "question_set": &question_set,
            "plan": &view,
        }))
        .map_err(|error| invalid(format!("serialize request_scope_decision result: {error}")))?;
        Ok(
            ToolExecutionResult::suspend_for_user(content).with_metadata(serde_json::json!({
                "question_set": &question_set,
                "plan": &view,
                "r_code_authoritative_plan_view": &view,
            })),
        )
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateItemArgs {
    plan_id: String,
    item_id: String,
    expected_revision: u64,
    state: PlanItemState,
}

#[derive(Clone)]
pub struct PlanItemUpdateTool {
    db: Arc<Database>,
    plans: Arc<PlanStore>,
}

impl PlanItemUpdateTool {
    pub fn new(db: Arc<Database>, plans: Arc<PlanStore>) -> Self {
        Self { db, plans }
    }
}

#[async_trait]
impl Tool for PlanItemUpdateTool {
    fn name(&self) -> &str {
        "plan_item_update"
    }

    fn description(&self) -> &str {
        "Advance only the active feature of an approved/executing Plan. Implement and attribute workspace writes only to that feature; never skip dependencies or work ahead on another feature. Mark the current in_progress feature completed only after its acceptance criteria are satisfied, which activates the next dependency-ready feature; use blocked when user input or an external dependency prevents progress. Resume that same blocked feature with state=in_progress only after the blocker is resolved; the host permits this only when its dependencies remain completed and no other feature is in progress. The successful result returns the complete newest Plan and active_feature; it immediately supersedes the host context snapshot for subsequent work in the same run."
    }

    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R0
    }

    fn path_bindings(&self) -> &'static [PathBinding] {
        &[]
    }

    fn requires_existing_path(&self) -> bool {
        false
    }

    fn requires_workspace_scope(&self) -> bool {
        false
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "plan_id": { "type": "string", "minLength": 1 },
                "item_id": { "type": "string", "minLength": 1 },
                "expected_revision": { "type": "integer", "minimum": 1 },
                "state": {
                    "type": "string",
                    "enum": ["in_progress", "completed", "blocked"],
                    "description": "Use completed or blocked for the current active feature. Use in_progress only to resume that same blocked feature after its blocker is resolved."
                }
            },
            "required": ["plan_id", "item_id", "expected_revision", "state"]
        })
    }

    async fn execute(&self, _input: serde_json::Value) -> Result<String, ProductError> {
        Err(context_required())
    }

    async fn execute_with_context(
        &self,
        input: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolExecutionResult, ProductError> {
        require_task_mode(
            &self.db,
            &context.task_id,
            &[TaskMode::Auto, TaskMode::Edit],
            "plan_item_update",
        )?;
        let args: UpdateItemArgs = decode(input)?;
        let update = UpdatePlanItemInput {
            plan_id: args.plan_id,
            item_id: args.item_id,
            expected_revision: args.expected_revision,
            state: args.state,
        };
        let request_json = serde_json::to_string(&update)
            .map_err(|error| invalid(format!("serialize plan_item_update request: {error}")))?;
        let outcome = match self.plans.update_plan_item_idempotent(
            &context.task_id,
            &update,
            &context.run_id,
            &context.tool_call_id,
            &request_json,
        ) {
            Ok(outcome) => outcome,
            Err(error) if recoverable_plan_update_conflict(&error) => {
                tracing::warn!(
                    task_id = %context.task_id,
                    run_id = %context.run_id,
                    tool_call_id = %context.tool_call_id,
                    plan_id = %update.plan_id,
                    item_id = %update.item_id,
                    error = %error,
                    "Plan update conflict recovered from authoritative store state"
                );
                let Some(view) = self.plans.current_for_task(&context.task_id)? else {
                    return Err(error);
                };
                if view.plan.id != update.plan_id {
                    return Err(error);
                }
                let active_feature = view
                    .items
                    .iter()
                    .find(|item| item.state == PlanItemState::InProgress);
                let content = serde_json::to_string(&serde_json::json!({
                    "updated": false,
                    "status": "plan_state_synchronized",
                    "plan": &view,
                    "active_feature": active_feature,
                    "instruction": "The requested transition was not applied because the Plan had already advanced. This complete Plan is authoritative; continue only with active_feature, or stop if it is null.",
                }))
                .map_err(|serialize_error| invalid(format!(
                    "serialize synchronized plan_item_update result: {serialize_error}"
                )))?;
                return Ok(plan_item_execution_result(content, &view));
            }
            Err(error) => return Err(error),
        };
        let replayed = outcome.replayed;
        let view = outcome.view;
        let active_feature = view
            .items
            .iter()
            .find(|item| item.state == PlanItemState::InProgress);
        let content = serde_json::to_string(&serde_json::json!({
            "updated": !replayed,
            "replayed": replayed,
            "plan": &view,
            "active_feature": active_feature,
            "instruction": "This complete Plan is now authoritative. Continue only with active_feature; if it is null, stop implementation.",
        }))
        .map_err(|error| invalid(format!("serialize plan_item_update result: {error}")))?;
        Ok(plan_item_execution_result(content, &view))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use r_code_core::dto::{AgentEngine, ProjectAccessMode, Task};
    use r_code_core::plan::{ApprovePlanInput, CreatePlanInput, PlanItemDraft, PublishPlanInput};
    use r_code_store::QueuedMessageRepository;

    fn context(task_id: &str) -> ToolExecutionContext {
        context_with_call(task_id, "call-host-owned")
    }

    fn context_with_call(task_id: &str, tool_call_id: &str) -> ToolExecutionContext {
        ToolExecutionContext {
            task_id: task_id.to_string(),
            run_id: "run-host-owned".to_string(),
            tool_call_id: tool_call_id.to_string(),
            caller: Some("agent".to_string()),
            access_mode: ProjectAccessMode::RiskBased,
            origin_request_key: None,
        }
    }

    #[tokio::test]
    async fn agent_can_enter_plan_mode_and_stage_a_fresh_plan_run() {
        let temp = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open_in_memory().unwrap());
        let task = Task::new(None, "Agent", "Plan this safely", TaskMode::Edit);
        TaskRepository::new(&db).create(&task).unwrap();
        let branch = SessionBranchRepository::new(&db)
            .ensure_active(&task.id)
            .unwrap();
        let plans = Arc::new(PlanStore::new(db.clone(), temp.path().join("plans")));
        let tool = EnterPlanModeTool::new(
            db.clone(),
            plans.clone(),
            std::sync::Arc::new(|_task_id| {
                r_code_core::plan_entry::ResolvedPlanRuntimeProfile::baseline()
            }),
        );

        let result = tool
            .execute_with_context(
                serde_json::json!({"reason": "The requested change spans several dependent features."}),
                &context(&task.id),
            )
            .await
            .unwrap();

        assert_eq!(
            result.directive,
            Some(r_code_gateway::ToolExecutionDirective::SuspendForUser)
        );
        assert_eq!(
            TaskRepository::new(&db)
                .get(&task.id)
                .unwrap()
                .unwrap()
                .mode,
            TaskMode::Plan
        );
        let current = plans.current_for_task(&task.id).unwrap().unwrap();
        assert_eq!(current.plan.state, r_code_core::plan::PlanState::Draft);
        let queued = QueuedMessageRepository::new(&db)
            .list_pending(&task.id, &branch.id)
            .unwrap();
        assert_eq!(queued.len(), 1);
        assert!(queued[0].id.starts_with("plan-entry:"));
        assert!(queued[0].message.contains("structured Plan"));
    }

    #[tokio::test]
    async fn request_input_uses_context_task_and_returns_typed_suspension() {
        let temp = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open_in_memory().unwrap());
        let task = Task::new(None, "Plan", "Choose safely", TaskMode::Plan);
        TaskRepository::new(&db).create(&task).unwrap();
        let plans = Arc::new(PlanStore::new(db.clone(), temp.path().join("plans")));
        let plan = plans
            .create_plan(&CreatePlanInput {
                task_id: task.id.clone(),
            })
            .unwrap();
        let tool = RequestUserInputTool::new(db, plans);
        let result = tool
            .execute_with_context(
                serde_json::json!({
                    "plan_id": plan.plan.id,
                    "expected_revision": plan.plan.revision,
                    "questions": [{
                        "id": "scope",
                        "header": "Scope",
                        "question": "Which scope should the Plan use?",
                        "options": [
                            {"id": "safe", "label": "Safe (Recommended)", "description": "Keep the smallest coherent scope."},
                            {"id": "wide", "label": "Wider", "description": "Include adjacent cleanup."}
                        ]
                    }]
                }),
                &context(&task.id),
            )
            .await
            .unwrap();
        assert_eq!(
            result.directive,
            Some(r_code_gateway::ToolExecutionDirective::SuspendForUser)
        );
        assert!(result.metadata.unwrap().get("question_set").is_some());
    }

    #[tokio::test]
    async fn model_cannot_supply_a_spoofed_task_identity() {
        let temp = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open_in_memory().unwrap());
        let task = Task::new(None, "Plan", "Goal", TaskMode::Plan);
        TaskRepository::new(&db).create(&task).unwrap();
        let plans = Arc::new(PlanStore::new(db.clone(), temp.path().join("plans")));
        let plan = plans
            .create_plan(&CreatePlanInput {
                task_id: task.id.clone(),
            })
            .unwrap();
        let tool = PlanPublishTool::new(db, plans);
        let error = tool
            .execute_with_context(
                serde_json::json!({
                    "task_id": "spoofed",
                    "plan_id": plan.plan.id,
                    "expected_revision": plan.plan.revision,
                    "items": [{"id": "one", "title": "One", "description": "Do one thing"}]
                }),
                &context(&task.id),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("unknown field"));
    }

    #[tokio::test]
    async fn item_update_tool_resumes_blocked_feature_before_advancing_its_dependent() {
        let temp = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open_in_memory().unwrap());
        let task = Task::new(None, "Plan", "Resume safely", TaskMode::Plan);
        let tasks = TaskRepository::new(&db);
        tasks.create(&task).unwrap();
        let plans = Arc::new(PlanStore::new(db.clone(), temp.path().join("plans")));
        let created = plans
            .create_plan(&CreatePlanInput {
                task_id: task.id.clone(),
            })
            .unwrap();
        let ready = plans
            .publish_plan(
                &task.id,
                &PublishPlanInput {
                    plan_id: created.plan.id.clone(),
                    expected_revision: created.plan.revision,
                    items: vec![
                        PlanItemDraft {
                            id: "first".to_string(),
                            title: "First".to_string(),
                            description: "Complete the first acceptance criterion".to_string(),
                            section_path: vec![],
                            depends_on: vec![],
                        },
                        PlanItemDraft {
                            id: "second".to_string(),
                            title: "Second".to_string(),
                            description: "Verify the dependent outcome".to_string(),
                            section_path: vec![],
                            depends_on: vec!["first".to_string()],
                        },
                    ],
                },
            )
            .unwrap();
        let approved = plans
            .approve_plan(
                &task.id,
                &ApprovePlanInput {
                    plan_id: ready.plan.id.clone(),
                    expected_revision: ready.plan.revision,
                },
            )
            .unwrap();
        tasks.set_mode(&task.id, TaskMode::Auto).unwrap();
        let tool = PlanItemUpdateTool::new(db, plans);

        let blocked = tool
            .execute_with_context(
                serde_json::json!({
                    "plan_id": ready.plan.id,
                    "item_id": "first",
                    "expected_revision": approved.plan.revision,
                    "state": "blocked"
                }),
                &context_with_call(&task.id, "call-block-first"),
            )
            .await
            .unwrap();
        assert_eq!(
            blocked.directive,
            Some(r_code_gateway::ToolExecutionDirective::AllowAgentCompletion)
        );
        let blocked: serde_json::Value = serde_json::from_str(&blocked.content).unwrap();
        assert!(blocked["active_feature"].is_null());

        let resumed = tool
            .execute_with_context(
                serde_json::json!({
                    "plan_id": created.plan.id,
                    "item_id": "first",
                    "expected_revision": blocked["plan"]["plan"]["revision"],
                    "state": "in_progress"
                }),
                &context_with_call(&task.id, "call-resume-first"),
            )
            .await
            .unwrap();
        assert_eq!(
            resumed.directive,
            Some(r_code_gateway::ToolExecutionDirective::RequireAgentContinuation)
        );
        let resumed: serde_json::Value = serde_json::from_str(&resumed.content).unwrap();
        assert_eq!(resumed["active_feature"]["id"], "first");

        let advance_input = serde_json::json!({
            "plan_id": created.plan.id,
            "item_id": "first",
            "expected_revision": resumed["plan"]["plan"]["revision"],
            "state": "completed"
        });
        let advanced = tool
            .execute_with_context(
                advance_input.clone(),
                &context_with_call(&task.id, "call-complete-first"),
            )
            .await
            .unwrap();
        assert_eq!(
            advanced.directive,
            Some(r_code_gateway::ToolExecutionDirective::RequireAgentContinuation)
        );
        let advanced: serde_json::Value = serde_json::from_str(&advanced.content).unwrap();
        assert_eq!(advanced["active_feature"]["id"], "second");

        // Exact provider replay is idempotent and returns the current authoritative view.
        let replayed = tool
            .execute_with_context(
                advance_input.clone(),
                &context_with_call(&task.id, "call-complete-first"),
            )
            .await
            .unwrap();
        let replayed: serde_json::Value = serde_json::from_str(&replayed.content).unwrap();
        assert_eq!(replayed["replayed"], true);
        assert_eq!(
            replayed["plan"]["plan"]["revision"],
            advanced["plan"]["plan"]["revision"]
        );
        assert_eq!(replayed["active_feature"]["id"], "second");

        // A different call id carrying the old revision is stale intent, not an idempotent replay.
        // The tool does not mutate again; it returns a typed synchronization result instead of a
        // raw state-machine error.
        let stale = tool
            .execute_with_context(
                advance_input,
                &context_with_call(&task.id, "call-stale-complete-first"),
            )
            .await
            .unwrap();
        let stale_metadata = stale.metadata.clone().unwrap();
        let stale: serde_json::Value = serde_json::from_str(&stale.content).unwrap();
        assert_eq!(stale["updated"], false);
        assert_eq!(stale["status"], "plan_state_synchronized");
        assert_eq!(stale["active_feature"]["id"], "second");
        assert_eq!(
            stale_metadata["r_code_authoritative_plan_view"]["plan"]["revision"],
            advanced["plan"]["plan"]["revision"]
        );

        let duplicate_completed = tool
            .execute_with_context(
                serde_json::json!({
                    "plan_id": created.plan.id,
                    "item_id": "first",
                    "expected_revision": advanced["plan"]["plan"]["revision"],
                    "state": "completed"
                }),
                &context_with_call(&task.id, "call-duplicate-completed-first"),
            )
            .await
            .unwrap();
        let duplicate_completed: serde_json::Value =
            serde_json::from_str(&duplicate_completed.content).unwrap();
        assert_eq!(duplicate_completed["status"], "plan_state_synchronized");
        assert_eq!(duplicate_completed["active_feature"]["id"], "second");
        assert!(tool.description().contains("state=in_progress"));
        assert!(tool.input_schema()["properties"]["state"]["description"]
            .as_str()
            .unwrap()
            .contains("resume that same blocked feature"));
    }

    #[tokio::test]
    async fn scope_decision_switches_agent_to_plan_and_suspends() {
        let temp = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open_in_memory().unwrap());
        let task = Task::new(None, "Agent", "Choose a scope", TaskMode::Edit);
        TaskRepository::new(&db).create(&task).unwrap();
        let plans = Arc::new(PlanStore::new(db.clone(), temp.path().join("plans")));
        let tool = RequestScopeDecisionTool::new(db.clone(), plans.clone());

        let result = tool
            .execute_with_context(
                serde_json::json!({
                    "questions": [{
                        "id": "scope",
                        "header": "Scope",
                        "question": "Which parts should I implement?",
                        "options": [
                            {"id": "narrow", "label": "Narrow (Recommended)", "description": "Only the explicit request."},
                            {"id": "all", "label": "Everything in the image", "description": "Also apply the OCR suggestions."}
                        ]
                    }]
                }),
                &context(&task.id),
            )
            .await
            .unwrap();

        assert_eq!(
            result.directive,
            Some(r_code_gateway::ToolExecutionDirective::SuspendForUser)
        );
        assert_eq!(
            TaskRepository::new(&db)
                .get(&task.id)
                .unwrap()
                .unwrap()
                .mode,
            TaskMode::Plan
        );
        let current = plans.current_for_task(&task.id).unwrap().unwrap();
        let question_set = current.pending_question_set.unwrap();
        assert_eq!(
            question_set.kind,
            r_code_core::plan::PlanQuestionSetKind::ScopeDecision
        );
        assert_eq!(question_set.restore_mode, Some(TaskMode::Edit));
        assert!(result.metadata.unwrap().get("question_set").is_some());
    }

    #[tokio::test]
    async fn scope_decision_rejects_plan_mode() {
        let temp = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open_in_memory().unwrap());
        let task = Task::new(None, "Plan", "Already planning", TaskMode::Plan);
        TaskRepository::new(&db).create(&task).unwrap();
        let plans = Arc::new(PlanStore::new(db.clone(), temp.path().join("plans")));
        let tool = RequestScopeDecisionTool::new(db, plans);

        let error = tool
            .execute_with_context(
                serde_json::json!({
                    "questions": [{
                        "id": "scope",
                        "header": "Scope",
                        "question": "Which scope?",
                        "options": [
                            {"id": "a", "label": "A", "description": "First."},
                            {"id": "b", "label": "B", "description": "Second."}
                        ]
                    }]
                }),
                &context(&task.id),
            )
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("unavailable while task mode is plan"));
    }

    #[tokio::test]
    async fn scope_decision_rejects_codex_main_agent() {
        let temp = tempfile::tempdir().unwrap();
        let db = Arc::new(Database::open_in_memory().unwrap());
        let task = Task::new(None, "Agent", "Choose a scope", TaskMode::Auto);
        TaskRepository::new(&db).create(&task).unwrap();
        TaskRepository::new(&db)
            .set_agent_engine(&task.id, AgentEngine::Codex)
            .unwrap();
        let plans = Arc::new(PlanStore::new(db.clone(), temp.path().join("plans")));
        let tool = RequestScopeDecisionTool::new(db, plans);

        let error = tool
            .execute_with_context(
                serde_json::json!({
                    "questions": [{
                        "id": "scope",
                        "header": "Scope",
                        "question": "Which scope?",
                        "options": [
                            {"id": "a", "label": "A", "description": "First."},
                            {"id": "b", "label": "B", "description": "Second."}
                        ]
                    }]
                }),
                &context(&task.id),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("R-Code main Agent"));
    }
}
