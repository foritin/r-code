//! R-Code 专属 IPC 方法处理器。
//!
//! 基于 `agent-ipc` 的 `IpcHandler` trait（JSON-RPC 2.0）。
//! 本模块注册 R-Code 专属 method handler，并定义产品层错误码映射。
//!
//! - 健康检查（`ping`）
//! - 任务创建（`task.create`）[doc-08 §3]
//! - 错误码：JSON-RPC 标准码 (-32xxx) + 应用码 (1xxx) [doc-08 §6]
//! - 请求校验 [doc-08 §5]
//!
//! [doc-08] [agent-core/12 §6]

use async_trait::async_trait;
use agent_ipc::{IpcHandler, JsonRpcError, JsonRpcRequest};
use r_code_core::dto::{Task, TaskMode};
use r_code_core::error::ProductError;
use serde_json::Value;

/// Ping handler -- 健康检查。 [doc-08 §3]
///
/// 方法名：`ping`。返回 `{ "pong": true }`。
pub struct PingHandler;

#[async_trait]
impl IpcHandler for PingHandler {
    async fn handle(&self, _params: Value) -> agent_error::Result<Value> {
        Ok(serde_json::json!({ "pong": true }))
    }
}

/// Task create handler -- 创建任务（stub）。 [doc-08 §3.1]
///
/// 方法名：`task.create`。
/// 参数：`{ workspacePath?, goal, mode? }`。工作区是可选的；保留 `projectId`
/// 作为旧客户端兼容别名。
/// 当前为 stub：仅校验参数结构并构造 Task，不持久化。
/// 持久化由 `TaskRepository` 在上层完成（见 doc-06 §3.1）。
pub struct TaskCreateHandler;

#[async_trait]
impl IpcHandler for TaskCreateHandler {
    async fn handle(&self, params: Value) -> agent_error::Result<Value> {
        let workspace_path = match params
            .get("workspacePath")
            .or_else(|| params.get("projectId"))
        {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_str()
                    .ok_or_else(|| {
                        agent_error::Error::Ipc("workspacePath must be a string".into())
                    })?
                    .to_string(),
            ),
        };
        let goal = params
            .get("goal")
            .and_then(|v| v.as_str())
            .ok_or_else(|| agent_error::Error::Ipc("missing goal".into()))?;

        let mode = params.get("mode").and_then(|v| v.as_str()).unwrap_or("ask");
        let mode = match mode {
            "ask" => TaskMode::Ask,
            "edit" => TaskMode::Edit,
            "auto" => TaskMode::Auto,
            _ => return Err(agent_error::Error::Ipc("invalid mode".into())),
        };

        // stub：goal 同时作为 title（上层可更新 title）
        let task = Task::new(workspace_path, goal, goal, mode);
        Ok(serde_json::to_value(&task)?)
    }
}

/// 错误码常量。 [doc-08 §6]
///
/// JSON-RPC 标准码（-32xxx）+ R-Code 应用码（1xxx）。
pub mod error_codes {
    // JSON-RPC 2.0 标准错误码
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;

    // R-Code 应用错误码 [doc-08 §6.2]
    pub const TASK_NOT_FOUND: i64 = 1001;
    pub const RUN_NOT_FOUND: i64 = 1002;
    pub const PERMISSION_DENIED: i64 = 1003;
    pub const PATH_ESCAPE: i64 = 1004;
    pub const STATE_TRANSITION_ERROR: i64 = 1005;
    pub const VALIDATION_ERROR: i64 = 1006;
}

/// 将 `ProductError` 映射为 JSON-RPC 错误响应。 [doc-08 §6.2]
///
/// 产品专属错误映射到应用码（1xxx），其余降级为 `INTERNAL_ERROR`。
pub fn error_to_rpc_response(err: &ProductError) -> JsonRpcError {
    let (code, message) = match err {
        ProductError::PathEscape(_) => (error_codes::PATH_ESCAPE, err.to_string()),
        ProductError::PermissionError(_) => (error_codes::PERMISSION_DENIED, err.to_string()),
        ProductError::StateMachineError(_) => {
            (error_codes::STATE_TRANSITION_ERROR, err.to_string())
        }
        ProductError::ConfigError(_) => (error_codes::VALIDATION_ERROR, err.to_string()),
        ProductError::DatabaseError(_) => (error_codes::INTERNAL_ERROR, err.to_string()),
        _ => (error_codes::INTERNAL_ERROR, err.to_string()),
    };
    JsonRpcError { code, message }
}

/// 校验 JSON-RPC 2.0 请求必要字段。 [doc-08 §5, §6.1]
///
/// 校验 `jsonrpc == "2.0"` 且 `method` 非空。
/// 不合规返回 `INVALID_REQUEST` 错误。
pub fn validate_request(req: &JsonRpcRequest) -> Result<(), JsonRpcError> {
    if req.jsonrpc != "2.0" {
        return Err(JsonRpcError {
            code: error_codes::INVALID_REQUEST,
            message: "jsonrpc must be '2.0'".into(),
        });
    }
    if req.method.is_empty() {
        return Err(JsonRpcError {
            code: error_codes::INVALID_REQUEST,
            message: "method is required".into(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── PingHandler ──────────────────────────────────────────────

    #[tokio::test]
    async fn ping_handler_returns_pong() {
        let handler = PingHandler;
        let result = handler.handle(serde_json::json!({})).await.unwrap();
        assert_eq!(result["pong"], true);
    }

    #[tokio::test]
    async fn ping_handler_ignores_params() {
        let handler = PingHandler;
        let result = handler
            .handle(serde_json::json!({ "anything": "ignored" }))
            .await
            .unwrap();
        assert_eq!(result["pong"], true);
    }

    // ── TaskCreateHandler ────────────────────────────────────────

    #[tokio::test]
    async fn task_create_handler_creates_task() {
        let handler = TaskCreateHandler;
        let params = serde_json::json!({
            "projectId": "/workspace/myproject",
            "goal": "Fix the login bug",
            "mode": "edit"
        });
        let result = handler.handle(params).await.unwrap();
        assert_eq!(result["workspace_path"], "/workspace/myproject");
        assert_eq!(result["goal"], "Fix the login bug");
        assert_eq!(result["title"], "Fix the login bug");
        assert_eq!(result["mode"], "edit");
        assert_eq!(result["state"], "idle");
        assert!(result["id"].is_string());
        assert!(!result["id"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn task_create_handler_defaults_mode_to_ask() {
        let handler = TaskCreateHandler;
        let params = serde_json::json!({
            "projectId": "/ws",
            "goal": "do something"
        });
        let result = handler.handle(params).await.unwrap();
        assert_eq!(result["mode"], "ask");
    }

    #[tokio::test]
    async fn task_create_handler_accepts_all_modes() {
        let handler = TaskCreateHandler;
        for mode in ["ask", "edit", "auto"] {
            let params = serde_json::json!({
                "projectId": "/ws",
                "goal": "g",
                "mode": mode
            });
            let result = handler.handle(params).await.unwrap();
            assert_eq!(result["mode"], mode);
        }
    }

    #[tokio::test]
    async fn task_create_handler_allows_pure_chat_without_workspace() {
        let handler = TaskCreateHandler;
        let result = handler
            .handle(serde_json::json!({ "goal": "x" }))
            .await
            .unwrap();
        assert!(result["workspace_path"].is_null());
    }

    #[tokio::test]
    async fn task_create_handler_rejects_missing_goal() {
        let handler = TaskCreateHandler;
        let result = handler
            .handle(serde_json::json!({ "projectId": "/ws" }))
            .await;
        let err = result.unwrap_err();
        assert!(err.to_string().contains("goal"));
    }

    #[tokio::test]
    async fn task_create_handler_rejects_invalid_mode() {
        let handler = TaskCreateHandler;
        let params = serde_json::json!({
            "projectId": "/ws",
            "goal": "x",
            "mode": "invalid"
        });
        let result = handler.handle(params).await;
        let err = result.unwrap_err();
        assert!(err.to_string().contains("invalid mode"));
    }

    #[tokio::test]
    async fn task_create_handler_rejects_non_string_workspace_path() {
        let handler = TaskCreateHandler;
        let params = serde_json::json!({ "workspacePath": 123, "goal": "x" });
        assert!(handler.handle(params).await.is_err());
    }

    // ── error_codes ──────────────────────────────────────────────

    #[test]
    fn error_codes_match_json_rpc_standard() {
        assert_eq!(error_codes::PARSE_ERROR, -32700);
        assert_eq!(error_codes::INVALID_REQUEST, -32600);
        assert_eq!(error_codes::METHOD_NOT_FOUND, -32601);
        assert_eq!(error_codes::INVALID_PARAMS, -32602);
        assert_eq!(error_codes::INTERNAL_ERROR, -32603);
    }

    #[test]
    fn error_codes_match_application_codes() {
        assert_eq!(error_codes::TASK_NOT_FOUND, 1001);
        assert_eq!(error_codes::RUN_NOT_FOUND, 1002);
        assert_eq!(error_codes::PERMISSION_DENIED, 1003);
        assert_eq!(error_codes::PATH_ESCAPE, 1004);
        assert_eq!(error_codes::STATE_TRANSITION_ERROR, 1005);
        assert_eq!(error_codes::VALIDATION_ERROR, 1006);
    }

    // ── error_to_rpc_response ────────────────────────────────────

    #[test]
    fn error_to_rpc_response_maps_path_escape() {
        let err = ProductError::PathEscape("/etc/passwd".into());
        let rpc = error_to_rpc_response(&err);
        assert_eq!(rpc.code, error_codes::PATH_ESCAPE);
        assert!(rpc.message.contains("path escape"));
    }

    #[test]
    fn error_to_rpc_response_maps_permission_error() {
        let err = ProductError::PermissionError("denied".into());
        let rpc = error_to_rpc_response(&err);
        assert_eq!(rpc.code, error_codes::PERMISSION_DENIED);
        assert!(rpc.message.contains("permission"));
    }

    #[test]
    fn error_to_rpc_response_maps_state_machine_error() {
        let err = ProductError::StateMachineError("bad transition".into());
        let rpc = error_to_rpc_response(&err);
        assert_eq!(rpc.code, error_codes::STATE_TRANSITION_ERROR);
    }

    #[test]
    fn error_to_rpc_response_maps_config_error() {
        let err = ProductError::ConfigError("bad config".into());
        let rpc = error_to_rpc_response(&err);
        assert_eq!(rpc.code, error_codes::VALIDATION_ERROR);
    }

    #[test]
    fn error_to_rpc_response_maps_database_error_to_internal() {
        let err = ProductError::DatabaseError("conn failed".into());
        let rpc = error_to_rpc_response(&err);
        assert_eq!(rpc.code, error_codes::INTERNAL_ERROR);
    }

    #[test]
    fn error_to_rpc_response_maps_unknown_to_internal() {
        let err = ProductError::Other("something else".into());
        let rpc = error_to_rpc_response(&err);
        assert_eq!(rpc.code, error_codes::INTERNAL_ERROR);
    }

    // ── validate_request ─────────────────────────────────────────

    #[test]
    fn validate_request_accepts_valid_request() {
        let req = JsonRpcRequest::new("ping", "1", None);
        assert!(validate_request(&req).is_ok());
    }

    #[test]
    fn validate_request_rejects_wrong_jsonrpc_version() {
        let mut req = JsonRpcRequest::new("ping", "1", None);
        req.jsonrpc = "1.0".into();
        let err = validate_request(&req).unwrap_err();
        assert_eq!(err.code, error_codes::INVALID_REQUEST);
        assert!(err.message.contains("2.0"));
    }

    #[test]
    fn validate_request_rejects_empty_method() {
        let mut req = JsonRpcRequest::new("ping", "1", None);
        req.method = String::new();
        let err = validate_request(&req).unwrap_err();
        assert_eq!(err.code, error_codes::INVALID_REQUEST);
        assert!(err.message.contains("method"));
    }
}
