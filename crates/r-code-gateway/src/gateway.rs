//! Tool Gateway -- 工具注册、权限检查、审计记账。 [doc-02 §9, §1, §8]
//!
//! Tool Gateway 是 Agent 工具调用的唯一入口。所有调用经过：
//! Schema 校验 -> 权限分级 -> 执行 -> 记账。
//!
//! `ToolGateway` 实现 `hermes_core::ToolHost` trait，可无缝接入 Agent 循环。
//!
//! ## 审计策略 [doc-02 §8]
//! 所有调用（含被拒绝 / 待审批）入 `ledger`，含调用者身份。

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use hermes_core::{ToolCallOutcome, ToolHost, ToolSource, ToolSpec};
use r_code_core::dto::{RiskLevel, ToolCall};
use r_code_core::error::ProductError;
use tokio::sync::RwLock;

use crate::permission::{PermissionCheckResult, PermissionEngine};

/// 工具 trait -- 每个内置工具实现此接口。
#[async_trait]
pub trait Tool: Send + Sync {
    /// 工具名称（模型可见）。
    fn name(&self) -> &str;
    /// 工具描述（注入模型提示）。
    fn description(&self) -> &str;
    /// 默认风险等级。
    fn risk_level(&self) -> RiskLevel;
    /// JSON Schema 输入定义。
    fn input_schema(&self) -> serde_json::Value;
    /// 执行工具，返回输出文本。
    async fn execute(&self, input: serde_json::Value) -> Result<String, ProductError>;
}

/// Tool Gateway -- 管理工具注册、权限检查与审计账本。
///
/// 实现 `hermes_core::ToolHost`，可注册到 Agent 循环。
pub struct ToolGateway {
    tools: HashMap<String, Box<dyn Tool>>,
    permission_engine: Arc<PermissionEngine>,
    /// 审计账本 -- 所有工具调用记录（含被拒绝 / 待审批）。
    ledger: Arc<RwLock<Vec<ToolCall>>>,
}

impl ToolGateway {
    /// 创建新的 Tool Gateway。
    pub fn new(permission_engine: Arc<PermissionEngine>) -> Self {
        Self {
            tools: HashMap::new(),
            permission_engine,
            ledger: Arc::new(RwLock::new(Vec::new())),
        }
    }

    /// 注册一个工具。
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// 列出所有已注册工具的规格。
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.tools
            .values()
            .map(|tool| ToolSpec {
                name: tool.name().to_string(),
                description: tool.description().to_string(),
                input_schema: tool.input_schema(),
                source: ToolSource::Builtin,
                requires_confirmation: tool.risk_level().requires_confirmation(),
            })
            .collect()
    }

    /// 执行工具调用（含权限检查与审计记账）。
    ///
    /// 流程 [doc-02 §1]：
    /// 1. 查找工具（未找到 -> 错误）
    /// 2. 获取风险等级
    /// 3. 权限检查（`PermissionEngine::check`）
    /// 4. 若 `NeedsApproval` -> 记账（Denied）并返回权限错误
    /// 5. 若 `Denied` -> 记账（Denied）并返回权限错误
    /// 6. 若 `Allowed` -> 执行工具
    /// 7. 记账（Ok / Error）并返回结果
    pub async fn execute_call(
        &self,
        task_id: &str,
        run_id: &str,
        tool_name: &str,
        input: serde_json::Value,
        caller: Option<&str>,
    ) -> Result<ToolCallOutcome, ProductError> {
        // 1. 查找工具
        let tool = self
            .tools
            .get(tool_name)
            .ok_or_else(|| ProductError::PermissionError(format!("tool not found: {tool_name}")))?;

        // 2. 获取风险等级
        let risk_level = tool.risk_level();

        // 3. 从 input 中提取 target（终端工具用）
        let target = input.get("target").and_then(|v| v.as_str());

        // 4. 权限检查
        let check_result = self
            .permission_engine
            .check(task_id, tool_name, risk_level, target)
            .await;

        // 5. 创建审计记录
        let mut audit = ToolCall::new(run_id, task_id, tool_name, input.to_string(), risk_level);
        audit.caller = caller.map(|s| s.to_string());

        // 6. 根据检查结果处理
        match check_result {
            PermissionCheckResult::Allowed => {
                let outcome = tool.execute(input).await;
                match outcome {
                    Ok(content) => {
                        audit.succeed(&content);
                        self.ledger.write().await.push(audit);
                        Ok(ToolCallOutcome {
                            content,
                            is_error: false,
                            metadata: None,
                        })
                    }
                    Err(err) => {
                        audit.fail(err.to_string());
                        self.ledger.write().await.push(audit);
                        Err(err)
                    }
                }
            }
            PermissionCheckResult::Denied(reason) => {
                audit.deny(&reason);
                self.ledger.write().await.push(audit);
                Err(ProductError::PermissionError(reason))
            }
            PermissionCheckResult::NeedsApproval(req) => {
                let msg = format!(
                    "tool {tool_name} requires user approval (request {})",
                    req.id
                );
                audit.deny(&msg);
                self.ledger.write().await.push(audit);
                Err(ProductError::PermissionError(msg))
            }
        }
    }

    /// 获取审计账本（所有工具调用记录）。
    pub async fn ledger(&self) -> Vec<ToolCall> {
        self.ledger.read().await.clone()
    }

    /// 获取权限引擎引用。
    pub fn permission_engine(&self) -> &Arc<PermissionEngine> {
        &self.permission_engine
    }
}

/// 为 `ToolGateway` 实现 `hermes_core::ToolHost`。
///
/// - `list_tools`：返回所有已注册工具的 `ToolSpec`。
/// - `call`：委托给 `execute_call`（task_id / run_id 为空，表示直接调用）。
#[async_trait]
impl ToolHost for ToolGateway {
    async fn list_tools(&self) -> hermes_error::Result<Vec<ToolSpec>> {
        Ok(self.tool_specs())
    }

    async fn call(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> hermes_error::Result<ToolCallOutcome> {
        self.execute_call("", "", name, args, None)
            .await
            .map_err(Into::into)
    }
}

// ── 测试辅助工具 ──────────────────────────────────────────────

/// 用于测试的 R0 echo 工具。
#[cfg(test)]
struct EchoTool;

#[async_trait]
#[cfg(test)]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "Echo the input text"
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R0
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": { "text": { "type": "string" } },
            "required": ["text"]
        })
    }
    async fn execute(&self, input: serde_json::Value) -> Result<String, ProductError> {
        Ok(input
            .get("text")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }
}

/// 用于测试的 R2 write 工具（不实际写入，仅返回成功）。
#[cfg(test)]
struct WriteTool;

#[async_trait]
#[cfg(test)]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write_file"
    }
    fn description(&self) -> &str {
        "Write content to a file"
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R2
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" }
            },
            "required": ["path", "content"]
        })
    }
    async fn execute(&self, input: serde_json::Value) -> Result<String, ProductError> {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("");
        Ok(format!("wrote to {path}"))
    }
}

/// 用于测试的会报错的工具。
#[cfg(test)]
struct FailTool;

#[async_trait]
#[cfg(test)]
impl Tool for FailTool {
    fn name(&self) -> &str {
        "fail"
    }
    fn description(&self) -> &str {
        "Always fails"
    }
    fn risk_level(&self) -> RiskLevel {
        RiskLevel::R0
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }
    async fn execute(&self, _input: serde_json::Value) -> Result<String, ProductError> {
        Err(ProductError::Other("intentional failure".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use r_code_core::dto::{PermissionDecision, ToolCallStatus};

    fn make_gateway() -> (Arc<PermissionEngine>, ToolGateway) {
        let engine = Arc::new(PermissionEngine::new());
        let gw = ToolGateway::new(engine.clone());
        (engine, gw)
    }

    #[tokio::test]
    async fn register_and_list_tools() {
        let (_, mut gw) = make_gateway();
        gw.register(Box::new(EchoTool));
        gw.register(Box::new(WriteTool));

        let specs = gw.tool_specs();
        assert_eq!(specs.len(), 2);

        let names: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"echo"));
        assert!(names.contains(&"write_file"));

        // R0 不需要确认，R2 需要确认
        let echo_spec = specs.iter().find(|s| s.name == "echo").unwrap();
        assert!(!echo_spec.requires_confirmation);
        let write_spec = specs.iter().find(|s| s.name == "write_file").unwrap();
        assert!(write_spec.requires_confirmation);

        // 来源为 Builtin
        assert!(matches!(echo_spec.source, ToolSource::Builtin));
    }

    #[tokio::test]
    async fn list_tools_via_tool_host() {
        let (_, mut gw) = make_gateway();
        gw.register(Box::new(EchoTool));

        let specs = <ToolGateway as ToolHost>::list_tools(&gw).await.unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "echo");
    }

    #[tokio::test]
    async fn execute_r0_allowed() {
        let (_, mut gw) = make_gateway();
        gw.register(Box::new(EchoTool));

        let outcome = gw
            .execute_call(
                "t1",
                "r1",
                "echo",
                serde_json::json!({ "text": "hello" }),
                Some("caller-1"),
            )
            .await
            .unwrap();

        assert_eq!(outcome.content, "hello");
        assert!(!outcome.is_error);

        // 审计账本应记录一次成功调用
        let ledger = gw.ledger().await;
        assert_eq!(ledger.len(), 1);
        let entry = &ledger[0];
        assert_eq!(entry.tool_name, "echo");
        assert_eq!(entry.status, ToolCallStatus::Ok);
        assert_eq!(entry.task_id, "t1");
        assert_eq!(entry.run_id, "r1");
        assert_eq!(entry.caller.as_deref(), Some("caller-1"));
        assert!(entry.ended_at.is_some());
    }

    #[tokio::test]
    async fn execute_r2_needs_approval() {
        let (_, mut gw) = make_gateway();
        gw.register(Box::new(WriteTool));

        let result = gw
            .execute_call(
                "t1",
                "r1",
                "write_file",
                serde_json::json!({ "path": "foo.txt", "content": "bar" }),
                None,
            )
            .await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ProductError::PermissionError(_)));

        // 审计账本应记录一次拒绝（待审批）
        let ledger = gw.ledger().await;
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger[0].status, ToolCallStatus::Denied);
    }

    #[tokio::test]
    async fn execute_r2_with_standing_rule_allowed() {
        let (engine, mut gw) = make_gateway();
        gw.register(Box::new(WriteTool));

        // 添加 standing rule
        engine
            .add_standing_rule(
                "t1",
                "write_file",
                None,
                RiskLevel::R2,
                PermissionDecision::AllowAlways,
            )
            .await
            .unwrap();

        let outcome = gw
            .execute_call(
                "t1",
                "r1",
                "write_file",
                serde_json::json!({ "path": "foo.txt", "content": "bar" }),
                None,
            )
            .await
            .unwrap();

        assert_eq!(outcome.content, "wrote to foo.txt");
        assert!(!outcome.is_error);

        let ledger = gw.ledger().await;
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger[0].status, ToolCallStatus::Ok);
    }

    #[tokio::test]
    async fn execute_tool_not_found() {
        let (_, gw) = make_gateway();

        let result = gw
            .execute_call("t1", "r1", "nonexistent", serde_json::json!({}), None)
            .await;

        assert!(result.is_err());
        // 未找到工具不应记录审计
        assert!(gw.ledger().await.is_empty());
    }

    #[tokio::test]
    async fn execute_tool_failure_recorded() {
        let (_, mut gw) = make_gateway();
        gw.register(Box::new(FailTool));

        let result = gw
            .execute_call("t1", "r1", "fail", serde_json::json!({}), None)
            .await;

        assert!(result.is_err());

        // 失败仍应记录审计
        let ledger = gw.ledger().await;
        assert_eq!(ledger.len(), 1);
        assert_eq!(ledger[0].status, ToolCallStatus::Error);
    }

    #[tokio::test]
    async fn call_via_tool_host_trait() {
        let (_, mut gw) = make_gateway();
        gw.register(Box::new(EchoTool));

        // 通过 ToolHost trait 调用
        let outcome =
            <ToolGateway as ToolHost>::call(&gw, "echo", serde_json::json!({ "text": "via host" }))
                .await
                .unwrap();

        assert_eq!(outcome.content, "via host");
        assert!(!outcome.is_error);
    }

    #[tokio::test]
    async fn call_via_tool_host_unknown_tool() {
        let (_, gw) = make_gateway();

        let result = <ToolGateway as ToolHost>::call(&gw, "nope", serde_json::json!({})).await;
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            hermes_error::Error::PermissionDenied(_)
        ));
    }

    #[tokio::test]
    async fn ledger_records_multiple_calls() {
        let (_, mut gw) = make_gateway();
        gw.register(Box::new(EchoTool));

        gw.execute_call("t1", "r1", "echo", serde_json::json!({ "text": "a" }), None)
            .await
            .unwrap();
        gw.execute_call("t1", "r1", "echo", serde_json::json!({ "text": "b" }), None)
            .await
            .unwrap();

        let ledger = gw.ledger().await;
        assert_eq!(ledger.len(), 2);
        assert_eq!(ledger[0].tool_name, "echo");
        assert_eq!(ledger[1].tool_name, "echo");
    }

    #[tokio::test]
    async fn permission_engine_accessor() {
        let (engine, gw) = make_gateway();
        assert!(Arc::ptr_eq(&engine, gw.permission_engine()));
    }
}
