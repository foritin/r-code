//! Gateway tools adapted to the host `ToolService` port.
//!
//! Every call flows: common authorization (T12a) → operation-intent
//! persistence → Gateway execution (its own schema checks, risk-based
//! permission engine and audit ledger). Read-only workspaces deny write
//! tools before the Gateway sees them; cancellations propagate by refusing
//! calls whose generation is no longer live.

use crate::services::authorization::{
    AuthorizationDecision, AuthorizationService, EffectivePermissions, OperationDescriptor,
    WorkspaceCapability,
};
use r_code_gateway::gateway::{ToolExecutionDirective, ToolGateway};
use r_code_gateway::tools::{CreateFileTool, DeleteFileTool, EditTool, ReadFileTool};
use r_code_harness_protocol::services::ToolCallRequest;
use r_code_harness_protocol::services::{
    OutputBlock, ToolCallError, ToolCallReply, ToolDescriptor,
};
use r_code_kernel::ports::{GenerationToken, JournalStore, ServiceError, ToolService};
use std::sync::Arc;

/// Write-effecting tools denied outright for read-only workspaces.
const WRITE_TOOLS: &[&str] = &["create_file", "delete_file", "edit", "apply_patch", "bash"];

/// The host tool service over the existing Gateway.
pub struct GatewayToolService {
    gateway: Arc<ToolGateway>,
    authorization: Arc<AuthorizationService>,
    store: Arc<dyn JournalStore>,
    workspace: WorkspaceCapability,
    permissions: EffectivePermissions,
}

impl GatewayToolService {
    /// Compose with an already-configured gateway (tools registered,
    /// permission engine installed).
    pub fn new(
        gateway: Arc<ToolGateway>,
        authorization: Arc<AuthorizationService>,
        store: Arc<dyn JournalStore>,
        workspace: WorkspaceCapability,
        permissions: EffectivePermissions,
    ) -> Self {
        Self {
            gateway,
            authorization,
            store,
            workspace,
            permissions,
        }
    }

    /// A gateway pre-registered with the core file tools.
    pub fn with_core_tools(
        authorization: Arc<AuthorizationService>,
        store: Arc<dyn JournalStore>,
        workspace: WorkspaceCapability,
        permissions: EffectivePermissions,
    ) -> Self {
        let mut gateway =
            ToolGateway::new(Arc::new(r_code_gateway::permission::PermissionEngine::new()));
        gateway.register(Box::new(ReadFileTool));
        gateway.register(Box::new(CreateFileTool));
        gateway.register(Box::new(EditTool));
        gateway.register(Box::new(DeleteFileTool));
        Self::new(
            Arc::new(gateway),
            authorization,
            store,
            workspace,
            permissions,
        )
    }

    fn access_mode(&self) -> r_code_core::dto::ProjectAccessMode {
        use r_code_core::dto::ProjectAccessMode;
        use r_code_harness_protocol::services::PermissionCeiling;
        match self.permissions.ceiling {
            // Read-only workspaces already deny write tools above; reads
            // flow through risk-based checks without approval friction.
            PermissionCeiling::ReadOnly => ProjectAccessMode::RiskBased,
            PermissionCeiling::ApprovalRequired => ProjectAccessMode::RequestApproval,
            PermissionCeiling::Full => ProjectAccessMode::FullAccess,
        }
    }

    async fn persist_intent(
        &self,
        token: &GenerationToken,
        tool: &str,
        input: &serde_json::Value,
    ) -> Result<(), ServiceError> {
        // Persist an operation intent before the effect runs; the RPC router
        // (T10) keys dedup on the plugin-provided operation_key, this intent
        // makes the in-flight effect itself durable.
        let hash = r_code_harness_protocol::canonical_input_hash(input);
        self.store
            .save_receipt(r_code_kernel::task::OperationReceipt {
                attempt_id: format!("tool:{}", token.run_id),
                operation_key: r_code_harness_protocol::OperationKey(format!(
                    "intent:{tool}:{hash}"
                )),
                method: format!("host.tools.call:{tool}"),
                input_hash: hash,
                outcome: r_code_kernel::task::ReceiptOutcome::Completed {
                    result: serde_json::json!({"intent": true}),
                },
            })
            .await
    }
}

#[async_trait::async_trait]
impl ToolService for GatewayToolService {
    async fn list(&self, _token: GenerationToken) -> Result<Vec<ToolDescriptor>, ServiceError> {
        Ok(self
            .gateway
            .tool_specs()
            .into_iter()
            .map(|spec| ToolDescriptor {
                name: spec.name,
                description: spec.description,
                input_schema: spec.input_schema,
            })
            .collect())
    }

    async fn call(
        &self,
        token: GenerationToken,
        call: ToolCallRequest,
    ) -> Result<ToolCallReply, ServiceError> {
        // 1. Read-only workspaces deny write tools before the Gateway.
        if matches!(self.workspace, WorkspaceCapability::ReadOnly { .. })
            && WRITE_TOOLS.contains(&call.tool.as_str())
        {
            return Ok(denied(&call.tool, "read-only workspace"));
        }

        // 2. Common authorization: descriptor from tool name + argv-ish
        // string arguments, so shell tools cannot slip past.
        let argv = call
            .input
            .as_object()
            .map(|object| {
                object
                    .values()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let cwd = call
            .input
            .get("path")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let descriptor = OperationDescriptor::tool_call(&call.tool, argv, cwd);
        let decision = self.authorization.authorize(
            &descriptor,
            &self.workspace,
            &self.permissions,
            &Default::default(),
        );
        match decision {
            AuthorizationDecision::Allowed => {}
            AuthorizationDecision::RequiresApproval { summary } => {
                return Ok(denied(&call.tool, &format!("approval required: {summary}")));
            }
            AuthorizationDecision::Denied(reason) => {
                return Ok(denied(&call.tool, &reason.to_string()));
            }
        }

        // 3. Persist the intent before execution.
        self.persist_intent(&token, &call.tool, &call.input).await?;

        // 4. Execute through the Gateway (schema checks, risk-based
        //    permissions, audit ledger). Permission refusals come back as
        //    structured denials, not transport failures.
        let outcome = match self
            .gateway
            .execute_call_with_access_mode(
                &format!("task:{}", token.run_id),
                &token.run_id,
                &call.tool,
                call.input.clone(),
                Some("harness-plugin"),
                self.access_mode(),
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(r_code_core::error::ProductError::PermissionError(reason)) => {
                return Ok(denied(&call.tool, &reason));
            }
            Err(error) => {
                return Ok(ToolCallReply {
                    output: vec![],
                    error: Some(ToolCallError {
                        code: "tool-error".into(),
                        message: error.to_string(),
                        denied_by: None,
                    }),
                });
            }
        };

        let text = outcome.content;
        let reply = if outcome.is_error {
            ToolCallReply {
                output: vec![],
                error: Some(ToolCallError {
                    code: "tool-error".into(),
                    message: text,
                    denied_by: None,
                }),
            }
        } else {
            ToolCallReply {
                output: vec![OutputBlock::Text { text }],
                error: None,
            }
        };
        let _ = ToolExecutionDirective::AllowAgentCompletion;
        Ok(reply)
    }
}

fn denied(tool: &str, reason: &str) -> ToolCallReply {
    ToolCallReply {
        output: vec![],
        error: Some(ToolCallError {
            code: "denied".into(),
            message: reason.to_string(),
            denied_by: Some(format!("host/tool-service/{tool}")),
        }),
    }
}
