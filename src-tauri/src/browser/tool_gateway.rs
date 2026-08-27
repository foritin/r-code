use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use agent_contract::ToolCallOutcome;
use async_trait::async_trait;
use r_code_core::dto::RiskLevel;
use r_code_core::error::ProductError;
use r_code_core::security::PathGuard;
use r_code_core::UserFacingError;
use r_code_gateway::{PathBinding, Tool, ToolExecutionContext, ToolExecutionResult, ToolGateway};
use r_code_store::{Database, TaskRepository};
use tokio_util::sync::CancellationToken;

use super::{BrowserToolContract, BrowserToolName, BrowserToolRequest, BrowserToolResult};
use crate::feature_flags::{ProductFeature, ProductFeatureFlags};
use crate::task_workspace_binding::resolve_task_workspace_binding;

const NO_PATH_BINDINGS: &[PathBinding] = &[];

/// Runtime-owned implementation injected at B3. F6 never supplies a placeholder executor.
#[async_trait]
pub trait BrowserToolExecutor: Send + Sync {
    async fn execute(
        &self,
        request: BrowserToolRequest,
        context: &ToolExecutionContext,
        workspace_guard: &PathGuard,
        abort_flag: Option<&AtomicBool>,
    ) -> Result<BrowserToolResult, ProductError>;
}

/// The only registration entry for Browser tools.
///
/// Native R-Code runs already read this gateway directly. Codex App Server descriptors are also
/// projected from this same registered gateway by [`codex_dynamic_browser_tools`].
pub fn register_browser_agent_tools(
    flags: ProductFeatureFlags,
    gateway: &mut ToolGateway,
    executor: Arc<dyn BrowserToolExecutor>,
) -> Result<(), UserFacingError> {
    flags.require(ProductFeature::Browser)?;
    for name in BrowserToolName::ALL {
        gateway.register(Box::new(BrowserGatewayTool {
            contract: BrowserToolContract::for_name(name),
            executor: executor.clone(),
        }));
    }
    Ok(())
}

struct BrowserGatewayTool {
    contract: BrowserToolContract,
    executor: Arc<dyn BrowserToolExecutor>,
}

impl BrowserGatewayTool {
    async fn run(
        &self,
        input: serde_json::Value,
        context: &ToolExecutionContext,
        abort_flag: Option<&AtomicBool>,
        workspace_guard: Option<&PathGuard>,
    ) -> Result<ToolExecutionResult, ProductError> {
        let workspace_guard = workspace_guard.ok_or_else(workspace_scope_required)?;
        if context.task_id.is_empty() || context.run_id.is_empty() {
            return Err(workspace_scope_required());
        }
        let request = BrowserToolRequest::from_input(self.contract.name, input)
            .map_err(|error| invalid_input(self.contract.name, error))?;
        let result = self
            .executor
            .execute(request, context, workspace_guard, abort_flag)
            .await?;
        if result.tool_name() != self.contract.name {
            return Err(result_contract_mismatch(self.contract.name));
        }
        let content = serde_json::to_string(&result).map_err(ProductError::from)?;
        Ok(
            ToolExecutionResult::success(content).with_metadata(serde_json::json!({
                "browser_result": result,
            })),
        )
    }
}

#[async_trait]
impl Tool for BrowserGatewayTool {
    fn name(&self) -> &str {
        self.contract.name.as_str()
    }

    fn description(&self) -> &str {
        &self.contract.description
    }

    fn risk_level(&self) -> RiskLevel {
        if self.contract.name.is_read_only() {
            RiskLevel::R1
        } else {
            RiskLevel::R2
        }
    }

    fn path_bindings(&self) -> &'static [PathBinding] {
        NO_PATH_BINDINGS
    }

    fn input_schema(&self) -> serde_json::Value {
        self.contract.input_schema.clone()
    }

    async fn execute(&self, _input: serde_json::Value) -> Result<String, ProductError> {
        Err(workspace_scope_required())
    }

    async fn execute_with_context_and_abort_with_workspace(
        &self,
        input: serde_json::Value,
        context: &ToolExecutionContext,
        abort_flag: Option<&AtomicBool>,
        workspace_guard: Option<&PathGuard>,
    ) -> Result<ToolExecutionResult, ProductError> {
        self.run(input, context, abort_flag, workspace_guard).await
    }
}

fn workspace_scope_required() -> ProductError {
    UserFacingError::new("workspace.binding_invalid")
        .with_debug_detail("browser tool execution requires a trusted task workspace binding")
        .into()
}

fn invalid_input(name: BrowserToolName, error: serde_json::Error) -> ProductError {
    ProductError::RecoverableToolError {
        tool: name.as_str().to_string(),
        code: "browser.invalid_tool_input".to_string(),
        message: "Browser tool input does not match the registered contract".to_string(),
        details: serde_json::json!({ "reason": error.to_string() }),
    }
}

fn result_contract_mismatch(name: BrowserToolName) -> ProductError {
    ProductError::RecoverableToolError {
        tool: name.as_str().to_string(),
        code: "browser.result_contract_mismatch".to_string(),
        message: "Browser runtime returned a result for a different tool".to_string(),
        details: serde_json::json!({}),
    }
}

/// Codex App Server's dynamic-tool wire shape, projected only from actually registered tools.
pub fn codex_dynamic_browser_tools(gateway: &ToolGateway) -> Vec<serde_json::Value> {
    let specs = gateway.tool_specs();
    BrowserToolName::ALL
        .into_iter()
        .filter_map(|name| {
            specs
                .iter()
                .find(|spec| spec.name == name.as_str())
                .map(|spec| {
                    serde_json::json!({
                        "type": "function",
                        "name": spec.name,
                        "description": spec.description,
                        "inputSchema": spec.input_schema,
                    })
                })
        })
        .collect()
}

pub enum BrowserCodexExecution {
    Completed(ToolCallOutcome),
    Cancelled,
}

/// Execute a Codex dynamic Browser call through the same ToolGateway audit and permission path.
#[allow(clippy::too_many_arguments)]
pub async fn execute_codex_browser_tool(
    db: &Database,
    gateway: &ToolGateway,
    task_id: &str,
    run_id: &str,
    caller: &str,
    call_id: Option<&str>,
    tool_name: &str,
    input: serde_json::Value,
    cancellation: CancellationToken,
) -> Result<BrowserCodexExecution, ProductError> {
    BrowserToolName::from_str(tool_name)
        .map_err(|_| ProductError::PermissionError("browser tool not registered".to_string()))?;
    if !gateway.owns_tool(tool_name) {
        return Err(ProductError::PermissionError(
            "browser tool not registered".to_string(),
        ));
    }
    let task = TaskRepository::new(db)
        .get(task_id)?
        .ok_or_else(|| ProductError::Other("browser task does not exist".to_string()))?;
    let binding = resolve_task_workspace_binding(db, &task)?;
    let access_mode = binding.access_mode();
    let workspace_guard = PathGuard::new(binding.root().to_path_buf())?;
    let abort_flag = Arc::new(AtomicBool::new(false));
    let monitor_flag = abort_flag.clone();
    let monitor_cancellation = cancellation.clone();
    let monitor = tokio::spawn(async move {
        monitor_cancellation.cancelled().await;
        monitor_flag.store(true, Ordering::Release);
    });
    let result = gateway
        .execute_with_wait_with_access_mode_and_workspace_guard(
            task_id,
            run_id,
            call_id,
            tool_name,
            input,
            Some(caller),
            browser_input_summary(tool_name),
            Some(abort_flag),
            access_mode,
            Some(&workspace_guard),
        )
        .await;
    monitor.abort();
    if cancellation.is_cancelled() {
        return Ok(BrowserCodexExecution::Cancelled);
    }
    result.map(BrowserCodexExecution::Completed)
}

fn browser_input_summary(tool_name: &str) -> &'static str {
    match BrowserToolName::from_str(tool_name) {
        Ok(BrowserToolName::Type) => "type redacted text into a browser element",
        Ok(BrowserToolName::Select) => "select browser element values",
        Ok(BrowserToolName::Press) => "press a browser key",
        Ok(BrowserToolName::Navigate | BrowserToolName::Open) => "navigate the task browser",
        Ok(BrowserToolName::Click) => "click a browser element",
        Ok(BrowserToolName::Scroll) => "scroll the task browser",
        Ok(BrowserToolName::Wait) => "wait for a browser condition",
        Ok(_) => "read or control the task browser",
        Err(_) => "unknown browser action",
    }
}
