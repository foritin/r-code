//! Transparent write tracking and command-facing services for Plan enhanced review.
//!
//! This module deliberately contains no Tauri registration. The authoritative desktop setup
//! wires it into the gateway and invokes startup recovery; MCP sibling processes must not run
//! recovery against a live desktop database.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use r_code_core::dto::RiskLevel;
use r_code_core::error::ProductError;
use r_code_core::plan::PlanExecutionStatus;
use r_code_gateway::{
    PathBinding, Tool, ToolExecutionContext, ToolExecutionResult, ToolPolicyGuard,
};
use r_code_store::{
    Database, EnhancedReviewTarget, EnhancedReviewView, FinishPlanWriteInput,
    OsPlanReviewFileSystem, PathCoordinator, PlanRejectRecoveryReport, PlanRejectResult,
    PlanReviewFileSystem, PlanReviewStore, RecordPlanWriteOutcome, TaskRepository,
};

const ATTRIBUTION_FAILURE_WARNING: &str = "工作区可能已改变但增强归属失败，普通审核仍可用";
const PLAN_EXECUTION_PAUSED: &str =
    "Plan execution is paused; resume the blocked feature with plan_item_update before writing";

/// Shared dependencies used by tracked write tools and enhanced-review commands.
///
/// Every production instance uses the same process-wide [`PathCoordinator`], so a write and a
/// feature rejection can never mutate the same path concurrently.
#[derive(Clone)]
pub struct PlanReviewServices {
    db: Arc<Database>,
    blobs_dir: PathBuf,
    coordinator: PathCoordinator,
    file_system: Arc<dyn PlanReviewFileSystem>,
}

impl PlanReviewServices {
    pub fn new(db: Arc<Database>, blobs_dir: PathBuf) -> Self {
        Self {
            db,
            blobs_dir,
            coordinator: PathCoordinator::shared(),
            file_system: Arc::new(OsPlanReviewFileSystem),
        }
    }

    /// Dependency-injected construction is useful for deterministic host integration tests.
    pub fn with_dependencies(
        db: Arc<Database>,
        blobs_dir: PathBuf,
        coordinator: PathCoordinator,
        file_system: Arc<dyn PlanReviewFileSystem>,
    ) -> Self {
        Self {
            db,
            blobs_dir,
            coordinator,
            file_system,
        }
    }

    pub fn store(&self) -> PlanReviewStore<'_> {
        PlanReviewStore::with_dependencies(
            &self.db,
            self.blobs_dir.clone(),
            self.coordinator.clone(),
            self.file_system.clone(),
        )
    }

    pub fn tracked(&self, inner: Box<dyn Tool>) -> TrackedWriteTool {
        TrackedWriteTool::new(inner, self.clone())
    }

    fn workspace_root(&self, task_id: &str) -> Result<PathBuf, ProductError> {
        let task = TaskRepository::new(&self.db).get(task_id)?.ok_or_else(|| {
            ProductError::StateMachineError(format!("task does not exist: {task_id}"))
        })?;
        task.workspace_path
            .filter(|path| !path.trim().is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| {
                ProductError::StateMachineError(format!("task {task_id} has no workspace"))
            })
    }
}

/// Host policy shared by built-in, shell and dynamic MCP tools.
///
/// R0/R1 reads remain available for diagnosis. Every state-changing operation is rejected while
/// an executing Plan has no active feature, including classifications supplied by MCP adapters.
pub struct PlanExecutionToolGuard {
    db: Arc<Database>,
}

impl PlanExecutionToolGuard {
    pub fn new(db: Arc<Database>) -> Self {
        Self { db }
    }
}

impl ToolPolicyGuard for PlanExecutionToolGuard {
    fn check(
        &self,
        context: &ToolExecutionContext,
        _tool_name: &str,
        risk_level: RiskLevel,
    ) -> Result<(), ProductError> {
        if matches!(risk_level, RiskLevel::R0 | RiskLevel::R1) {
            return Ok(());
        }
        let conn = self.db.conn()?;
        let paused: bool = conn
            .query_row(
                "SELECT EXISTS(
                   SELECT 1 FROM plans plan
                   WHERE plan.task_id = ?1 AND plan.state = 'executing'
                     AND NOT EXISTS (
                       SELECT 1 FROM plan_items item
                       WHERE item.plan_id = plan.id
                         AND item.revision = plan.approved_revision
                         AND item.state = 'in_progress'
                     )
                 )",
                rusqlite::params![context.task_id],
                |row| row.get(0),
            )
            .map_err(|error| ProductError::DatabaseError(error.to_string()))?;
        if paused {
            return Err(ProductError::PermissionError(
                PLAN_EXECUTION_PAUSED.to_string(),
            ));
        }
        Ok(())
    }
}

/// Wraps one of the built-in single-path write tools without changing its model-visible contract.
pub struct TrackedWriteTool {
    inner: Box<dyn Tool>,
    services: PlanReviewServices,
}

impl TrackedWriteTool {
    pub fn new(inner: Box<dyn Tool>, services: PlanReviewServices) -> Self {
        debug_assert!(matches!(
            inner.name(),
            "edit" | "apply_patch" | "create_file" | "delete_file"
        ));
        Self { inner, services }
    }

    fn tracked_path<'a>(
        &self,
        input: &'a serde_json::Value,
    ) -> Result<(&'static str, &'a Path), ProductError> {
        let binding = self.inner.path_bindings().first().ok_or_else(|| {
            ProductError::StateMachineError(format!(
                "tracked write tool {} does not declare a path binding",
                self.inner.name()
            ))
        })?;
        let value = input
            .get(binding.key)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                ProductError::Other(format!(
                    "missing '{}' parameter for tracked write tool {}",
                    binding.key,
                    self.inner.name()
                ))
            })?;
        Ok((binding.key, Path::new(value)))
    }

    fn bind_guard_path(
        input: &mut serde_json::Value,
        key: &str,
        path: &Path,
    ) -> Result<(), ProductError> {
        let object = input.as_object_mut().ok_or_else(|| {
            ProductError::Other("tracked write input must be a JSON object".into())
        })?;
        object.insert(
            key.to_string(),
            serde_json::Value::String(path.to_string_lossy().into_owned()),
        );
        Ok(())
    }

    fn combined_capture_error(
        original: Result<ToolExecutionResult, ProductError>,
        capture: ProductError,
    ) -> ProductError {
        match original {
            Ok(_) => ProductError::Other(format!("{ATTRIBUTION_FAILURE_WARNING}: {capture}")),
            Err(original) => ProductError::Other(format!(
                "{original}; {ATTRIBUTION_FAILURE_WARNING}: {capture}"
            )),
        }
    }
}

#[async_trait]
impl Tool for TrackedWriteTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn risk_level(&self) -> RiskLevel {
        self.inner.risk_level()
    }

    fn risk_for(&self, input: &serde_json::Value) -> RiskLevel {
        self.inner.risk_for(input)
    }

    fn path_bindings(&self) -> &'static [PathBinding] {
        self.inner.path_bindings()
    }

    fn requires_existing_path(&self) -> bool {
        self.inner.requires_existing_path()
    }

    fn requires_workspace_scope(&self) -> bool {
        self.inner.requires_workspace_scope()
    }

    fn input_schema(&self) -> serde_json::Value {
        self.inner.input_schema()
    }

    async fn execute(&self, input: serde_json::Value) -> Result<String, ProductError> {
        self.inner.execute(input).await
    }

    async fn execute_with_context(
        &self,
        mut input: serde_json::Value,
        context: &ToolExecutionContext,
    ) -> Result<ToolExecutionResult, ProductError> {
        let store = self.services.store();
        let execution_status = store.execution_status_for_run(&context.task_id, &context.run_id)?;
        let workspace_root = self.services.workspace_root(&context.task_id)?;
        let (path_key, requested_path) = self.tracked_path(&input)?;
        if execution_status == PlanExecutionStatus::Paused {
            return Err(ProductError::PermissionError(
                PLAN_EXECUTION_PAUSED.to_string(),
            ));
        }
        if execution_status == PlanExecutionStatus::NoExecutingPlan {
            // Ordinary writes share the rejection lock but perform no before/after reads and
            // create no enhanced-review event.
            let guard = store
                .begin_coordinated_write(&workspace_root, &context.task_id, requested_path)
                .await?;
            Self::bind_guard_path(&mut input, path_key, guard.path())?;
            let result = self.inner.execute_with_context(input, context).await;
            drop(guard);
            return result;
        }

        let guard = store
            .begin_feature_write(
                &workspace_root,
                &context.task_id,
                &context.run_id,
                requested_path,
            )
            .await
            .map_err(|error| {
                ProductError::Other(format!("无法建立增强审核归属，写入未执行: {error}"))
            })?;

        // The inner tool must mutate exactly the verified path covered by the lease.
        Self::bind_guard_path(&mut input, path_key, guard.path())?;
        let original = self.inner.execute_with_context(input, context).await;
        let captured = store.finish_feature_write(
            guard,
            FinishPlanWriteInput {
                tool_call_id: context.tool_call_id.clone(),
            },
        );
        match captured {
            Ok(
                RecordPlanWriteOutcome::Captured { .. }
                | RecordPlanWriteOutcome::Duplicate { .. }
                | RecordPlanWriteOutcome::Unassigned { .. }
                | RecordPlanWriteOutcome::Unchanged { .. },
            ) => original,
            Err(capture) => Err(Self::combined_capture_error(original, capture)),
        }
    }
}

/// Read the current Plan-only enhanced review aggregate (`{ taskId }`).
pub fn plan_review_status(
    services: &PlanReviewServices,
    task_id: &str,
) -> Result<Option<EnhancedReviewView>, ProductError> {
    services.store().list_current(task_id)
}

/// Record a ledger-only file acceptance (`{ target }`).
pub fn plan_review_accept_file(
    services: &PlanReviewServices,
    target: &EnhancedReviewTarget,
) -> Result<r_code_core::plan::PlanReviewDecision, ProductError> {
    services.store().accept_file(target)
}

/// Record a ledger-only feature acceptance (`{ target }`).
pub fn plan_review_accept_feature(
    services: &PlanReviewServices,
    target: &EnhancedReviewTarget,
) -> Result<r_code_core::plan::PlanReviewDecision, ProductError> {
    services.store().accept_feature(target)
}

/// Reject one feature-owned file contribution (`{ target }`).
pub async fn plan_review_reject_file(
    services: &PlanReviewServices,
    target: &EnhancedReviewTarget,
) -> Result<PlanRejectResult, ProductError> {
    let workspace_root = services.workspace_root(&target.task_id)?;
    services.store().reject_file(&workspace_root, target).await
}

/// Reject every file contribution owned by one feature (`{ target }`).
pub async fn plan_review_reject_feature(
    services: &PlanReviewServices,
    target: &EnhancedReviewTarget,
) -> Result<PlanRejectResult, ProductError> {
    let workspace_root = services.workspace_root(&target.task_id)?;
    services
        .store()
        .reject_feature(&workspace_root, target)
        .await
}

/// Recover incomplete Plan rejection journals.
///
/// Call this only from the authoritative desktop `main.setup`; never from `CommandState::new`,
/// because MCP sibling processes also construct command state.
pub async fn recover_plan_review_rejections(
    services: &PlanReviewServices,
) -> Result<PlanRejectRecoveryReport, ProductError> {
    services.store().recover_pending().await
}
