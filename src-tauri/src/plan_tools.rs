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
    PlanItemDraft, PlanItemState, PlanQuestionDraft, PublishPlanInput, RequestPlanQuestionsInput,
    UpdatePlanItemInput,
};
use r_code_gateway::{PathBinding, Tool, ToolExecutionContext, ToolExecutionResult};
use r_code_store::{Database, PlanStore, SessionBranchRepository, TaskRepository};
use serde::Deserialize;

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

const ENTER_PLAN_CONTINUATION: &str = "[system] R-Code safely changed this task from Agent mode to Plan mode. Continue the user's existing request as a structured Plan. Investigate read-only as needed, ask only blocking questions with request_user_input, then publish the complete functional Plan with plan_publish. Do not edit files or execute implementation before the user approves the Plan.";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnterPlanModeArgs {
    reason: String,
}

#[derive(Clone)]
pub struct EnterPlanModeTool {
    db: Arc<Database>,
    plans: Arc<PlanStore>,
}

impl EnterPlanModeTool {
    pub fn new(db: Arc<Database>, plans: Arc<PlanStore>) -> Self {
        Self { db, plans }
    }
}

#[async_trait]
impl Tool for EnterPlanModeTool {
    fn name(&self) -> &str {
        "enter_plan_mode"
    }

    fn description(&self) -> &str {
        "Safely change the current main task from Agent mode to Plan mode before making writes. Use when the user explicitly requests a structured plan, or when ambiguity, risk, or scope makes direct implementation unsafe. This ends the current Agent run and the host resumes the same request with read-only Plan tools. Returning to Agent mode requires explicit user approval; never use this to restart an already approved/executing Plan."
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

    fn allows_transient_retry(&self) -> bool {
        true
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
        let view = self.plans.enter_plan_mode_and_stage_continuation(
            &context.task_id,
            &branch.id,
            ENTER_PLAN_CONTINUATION,
        )?;
        let content = serde_json::to_string(&serde_json::json!({
            "entered_plan_mode": true,
            "reason": args.reason.trim(),
            "plan": view,
            "instruction": "Stop this Agent run. The host has queued a fresh Plan-mode continuation for the same request.",
        }))
        .map_err(|error| invalid(format!("serialize enter_plan_mode result: {error}")))?;
        Ok(ToolExecutionResult::suspend_for_user(content))
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
        "Publish the current Plan draft as 1-100 independently verifiable executable leaf items. Use section_path for optional hierarchy (for example phase -> area) instead of creating non-executable parent items. Each leaf must represent one coherent implementation outcome with explicit acceptance criteria and only real dependencies."
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

    fn allows_transient_retry(&self) -> bool {
        true
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
                            "description": { "type": "string", "maxLength": 20000 },
                            "section_path": {
                                "type": "array",
                                "maxItems": 4,
                                "description": "Optional presentation hierarchy. Parent labels are not executable tasks; this item remains the tracked leaf.",
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
            "plan": view,
        }))
        .map_err(|error| invalid(format!("serialize plan_publish result: {error}")))?;
        Ok(ToolExecutionResult::success(content))
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

    fn allows_transient_retry(&self) -> bool {
        true
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
            "question_set": question_set,
            "plan": view,
        }))
        .map_err(|error| invalid(format!("serialize request_user_input result: {error}")))?;
        Ok(
            ToolExecutionResult::suspend_for_user(content).with_metadata(serde_json::json!({
                "question_set": question_set,
                "plan": view,
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

    fn allows_transient_retry(&self) -> bool {
        true
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
        let view = self.plans.update_plan_item(
            &context.task_id,
            &UpdatePlanItemInput {
                plan_id: args.plan_id,
                item_id: args.item_id,
                expected_revision: args.expected_revision,
                state: args.state,
            },
        )?;
        let active_feature = view
            .items
            .iter()
            .find(|item| item.state == PlanItemState::InProgress);
        let content = serde_json::to_string(&serde_json::json!({
            "updated": true,
            "plan": &view,
            "active_feature": active_feature,
            "instruction": "This complete Plan is now authoritative. Continue only with active_feature; if it is null, stop implementation.",
        }))
        .map_err(|error| invalid(format!("serialize plan_item_update result: {error}")))?;
        if active_feature.is_some() {
            Ok(ToolExecutionResult::require_agent_continuation(content))
        } else {
            Ok(ToolExecutionResult::allow_agent_completion(content))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use r_code_core::dto::{ProjectAccessMode, Task};
    use r_code_core::plan::{ApprovePlanInput, CreatePlanInput, PlanItemDraft, PublishPlanInput};
    use r_code_store::QueuedMessageRepository;

    fn context(task_id: &str) -> ToolExecutionContext {
        ToolExecutionContext {
            task_id: task_id.to_string(),
            run_id: "run-host-owned".to_string(),
            tool_call_id: "call-host-owned".to_string(),
            caller: Some("agent".to_string()),
            access_mode: ProjectAccessMode::RiskBased,
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
        let tool = EnterPlanModeTool::new(db.clone(), plans.clone());

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
                &context(&task.id),
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
                &context(&task.id),
            )
            .await
            .unwrap();
        assert_eq!(
            resumed.directive,
            Some(r_code_gateway::ToolExecutionDirective::RequireAgentContinuation)
        );
        let resumed: serde_json::Value = serde_json::from_str(&resumed.content).unwrap();
        assert_eq!(resumed["active_feature"]["id"], "first");

        let advanced = tool
            .execute_with_context(
                serde_json::json!({
                    "plan_id": created.plan.id,
                    "item_id": "first",
                    "expected_revision": resumed["plan"]["plan"]["revision"],
                    "state": "completed"
                }),
                &context(&task.id),
            )
            .await
            .unwrap();
        assert_eq!(
            advanced.directive,
            Some(r_code_gateway::ToolExecutionDirective::RequireAgentContinuation)
        );
        let advanced: serde_json::Value = serde_json::from_str(&advanced.content).unwrap();
        assert_eq!(advanced["active_feature"]["id"], "second");
        assert!(tool.description().contains("state=in_progress"));
        assert!(tool.input_schema()["properties"]["state"]["description"]
            .as_str()
            .unwrap()
            .contains("resume that same blocked feature"));
    }
}
