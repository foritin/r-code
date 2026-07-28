//! R-Code 的本地 stdio MCP server。
//!
//! 它让 Codex 或其他 MCP 编排器把 R-Code 自己的 Agent 当作受限子代理使用。服务
//! 只暴露「已经在 R-Code 中打开的工作区」上的只读任务；不会开放文件写入、终端、
//! 任意路径浏览或凭据读取能力。

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use r_code_core::dto::TaskState;
use r_code_store::{Database, WorkspaceService};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::sync::Mutex;
use tokio::time::{sleep, Duration, Instant};

use crate::commands::{
    agent_abort, agent_send, session_messages, task_create_with_provider, task_detail, CommandState,
};

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const MAX_RPC_LINE_BYTES: usize = 512 * 1024;
const MAX_GOAL_CHARS: usize = 12_000;
const MAX_TITLE_CHARS: usize = 96;
const MAX_RESULT_CHARS: usize = 8_000;
const DEFAULT_WAIT_SECONDS: u64 = 45;
const MAX_WAIT_SECONDS: u64 = 55;

/// 启动独立 MCP server 时使用的 R-Code 应用数据根目录。
///
/// `R_CODE_DATA_DIR` 允许部署器显式指定目录；默认路径与 Tauri identifier
/// `com.r-code.app` 一致。参数值代表 `<app-data>/r-code`，其中包含 db/config/
/// sessions/blobs 四个子目录。
pub fn default_data_dir() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("R_CODE_DATA_DIR").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let root = dirs::data_dir().ok_or_else(|| "无法确定 R-Code 应用数据目录".to_string())?;
    Ok(root.join("com.r-code.app").join("r-code"))
}

/// 运行 stdio MCP server。stdout 完全保留给 MCP JSON-RPC，诊断仅走 stderr。
pub async fn serve_stdio(data_dir: Option<PathBuf>) -> Result<(), String> {
    let service = McpService::open(data_dir.unwrap_or(default_data_dir()?)).await?;
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin).lines();
    let mut writer = BufWriter::new(stdout);

    while let Some(line) = reader.next_line().await.map_err(|e| e.to_string())? {
        let response = if line.len() > MAX_RPC_LINE_BYTES {
            Some(rpc_error(
                Value::Null,
                -32700,
                "request exceeds the MCP size limit",
            ))
        } else {
            match serde_json::from_str::<McpRequest>(&line) {
                Ok(request) => service.handle(request).await,
                Err(_) => Some(rpc_error(Value::Null, -32700, "invalid JSON-RPC request")),
            }
        };
        if let Some(response) = response {
            let payload = serde_json::to_vec(&response).map_err(|e| e.to_string())?;
            writer
                .write_all(&payload)
                .await
                .map_err(|e| e.to_string())?;
            writer.write_all(b"\n").await.map_err(|e| e.to_string())?;
            writer.flush().await.map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// 一次本地 MCP server 进程持有自己的 Agent runtime；它同时维护自己创建的任务集，
/// 防止任意外部 prompt 使用猜到的 task ID 读取或中止其他会话。
pub struct McpService {
    state: CommandState,
    owned_tasks: Mutex<HashSet<String>>,
}

impl McpService {
    async fn open(base: PathBuf) -> Result<Self, String> {
        let db_dir = base.join("db");
        let blobs_dir = base.join("blobs");
        let sessions_dir = base.join("sessions");
        let config_dir = base.join("config");
        for dir in [&db_dir, &blobs_dir, &sessions_dir, &config_dir] {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let db_path = db_dir.join("r-code.db");
        let project_root = std::env::current_dir().map_err(|e| e.to_string())?;
        let state = CommandState::new(
            Arc::new(Database::open(&db_path).map_err(|e| e.to_string())?),
            blobs_dir,
            sessions_dir,
            config_dir,
            project_root,
            Some(db_path),
        );
        state.enable_real_agent_mode().await;
        Ok(Self {
            state,
            owned_tasks: Mutex::new(HashSet::new()),
        })
    }

    #[cfg(test)]
    fn from_state(state: CommandState) -> Self {
        Self {
            state,
            owned_tasks: Mutex::new(HashSet::new()),
        }
    }

    async fn handle(&self, request: McpRequest) -> Option<Value> {
        let id = request.id.clone();
        let respond = |value| id.clone().map(|id| rpc_result(id, value));
        let respond_error = |code, message| id.clone().map(|id| rpc_error(id, code, message));

        if request.jsonrpc.as_deref() != Some("2.0") {
            return respond_error(-32600, "jsonrpc must be '2.0'");
        }
        let Some(method) = request.method.as_deref() else {
            return respond_error(-32600, "method is required");
        };

        match method {
            "initialize" => respond(json!({
                "protocolVersion": negotiated_protocol(request.params.as_ref()),
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "r-code", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "R-Code delegates only read-only work in projects already opened in R-Code. Call r_code_delegate_readonly, then r_code_wait_for_result or r_code_task_status."
            })),
            "notifications/initialized" => None,
            "ping" => respond(json!({})),
            "tools/list" => respond(json!({ "tools": tool_catalog() })),
            "tools/call" => {
                let result = self.call_tool(request.params.unwrap_or(Value::Null)).await;
                respond(result)
            }
            _ => respond_error(-32601, "method not found"),
        }
    }

    async fn call_tool(&self, params: Value) -> Value {
        let name = match params.get("name").and_then(Value::as_str) {
            Some(name) => name,
            None => return tool_error("tools/call requires a tool name"),
        };
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let result = match name {
            "r_code_delegate_readonly" => self.delegate_readonly(&arguments).await,
            "r_code_task_status" => self.task_status(&arguments).await,
            "r_code_wait_for_result" => self.wait_for_result(&arguments).await,
            "r_code_cancel_task" => self.cancel_task(&arguments).await,
            _ => Err("unknown R-Code tool"),
        };
        match result {
            Ok(value) => tool_success(value),
            Err(message) => tool_error(message),
        }
    }

    async fn delegate_readonly(&self, arguments: &Value) -> Result<Value, &'static str> {
        let goal = required_text(arguments, "goal", MAX_GOAL_CHARS)?;
        let workspace_path = required_text(arguments, "workspace_path", 4_096)?;
        let workspace = PathBuf::from(workspace_path)
            .canonicalize()
            .map_err(|_| "workspace_path is unavailable")?;
        if !workspace.is_dir() {
            return Err("workspace_path must be a directory");
        }
        let workspace = workspace.to_string_lossy().to_string();

        // A configured MCP server must not turn arbitrary filesystem paths into agent scope.
        // The user has to open the project in R-Code first; that persisted choice is our trust
        // boundary for inbound external delegation.
        if WorkspaceService::new(&self.state.db)
            .get(&workspace)
            .map_err(|_| "R-Code workspace registry is unavailable")?
            .is_none()
        {
            return Err("open this workspace in R-Code before delegating to its Agent");
        }

        let title = optional_text(arguments, "title", MAX_TITLE_CHARS)
            .unwrap_or_else(|| format!("MCP · {}", compact(&goal, 54)));
        let provider_name = optional_text(arguments, "provider_name", 100);
        // Ask mode is enforced by LlmAgentRuntime as a read-only capability policy. This is not
        // merely a prompt request: write tools, shell, and recursive delegation are unavailable.
        let task = task_create_with_provider(
            &self.state,
            Some(&workspace),
            &title,
            &goal,
            "ask",
            provider_name.as_deref(),
        )
        .await
        .map_err(|_| "R-Code could not create a read-only task")?;
        self.owned_tasks.lock().await.insert(task.id.clone());
        if let Err(error) = agent_send(&self.state, &task.id, &goal).await {
            tracing::warn!(task_id = %task.id, "MCP native delegate could not start: {error}");
            return Err(
                "R-Code Agent is not ready; configure a usable provider in R-Code Settings",
            );
        }

        let wait_seconds = wait_seconds(arguments)?;
        self.wait_task(&task.id, wait_seconds).await
    }

    async fn task_status(&self, arguments: &Value) -> Result<Value, &'static str> {
        let task_id = required_text(arguments, "task_id", 160)?;
        self.require_owned(&task_id).await?;
        self.task_status_payload(&task_id).await
    }

    async fn wait_for_result(&self, arguments: &Value) -> Result<Value, &'static str> {
        let task_id = required_text(arguments, "task_id", 160)?;
        self.require_owned(&task_id).await?;
        self.wait_task(&task_id, wait_seconds(arguments)?).await
    }

    async fn cancel_task(&self, arguments: &Value) -> Result<Value, &'static str> {
        let task_id = required_text(arguments, "task_id", 160)?;
        self.require_owned(&task_id).await?;
        agent_abort(&self.state, &task_id)
            .await
            .map_err(|_| "R-Code could not cancel this task")?;
        Ok(json!({ "task_id": task_id, "status": "cancelled" }))
    }

    async fn require_owned(&self, task_id: &str) -> Result<(), &'static str> {
        self.owned_tasks
            .lock()
            .await
            .contains(task_id)
            .then_some(())
            .ok_or("task_id is not owned by this MCP server session")
    }

    async fn wait_task(&self, task_id: &str, wait_seconds: u64) -> Result<Value, &'static str> {
        let deadline = Instant::now() + Duration::from_secs(wait_seconds);
        loop {
            let payload = self.task_status_payload(task_id).await?;
            let terminal = matches!(
                payload.get("status").and_then(Value::as_str),
                Some("completed" | "failed" | "cancelled")
            );
            if terminal || Instant::now() >= deadline {
                return Ok(payload);
            }
            sleep(Duration::from_millis(250)).await;
        }
    }

    async fn task_status_payload(&self, task_id: &str) -> Result<Value, &'static str> {
        let detail = task_detail(&self.state, task_id)
            .await
            .map_err(|_| "R-Code task is unavailable")?;
        let running = matches!(
            detail.task.state,
            TaskState::Exploring | TaskState::InProgress
        ) || detail.runs.iter().any(|run| run.ended_at.is_none());
        let messages = session_messages(&self.state, task_id)
            .await
            .map_err(|_| "R-Code task result is unavailable")?;
        let result = messages
            .iter()
            .rev()
            .find(|message| message.role.as_deref() == Some("assistant"))
            .and_then(|message| message.text.as_deref())
            .map(|text| compact(text, MAX_RESULT_CHARS));
        let cancelled = detail.task.state == TaskState::Interrupted;
        let failed = result
            .as_deref()
            .is_some_and(|text| text.trim_start().starts_with("[error]"));
        let status = if cancelled {
            "cancelled"
        } else if running {
            "running"
        } else if failed {
            "failed"
        } else {
            "completed"
        };
        let run_id = detail
            .runs
            .iter()
            .find(|run| run.ended_at.is_none())
            .or_else(|| detail.runs.first())
            .map(|run| run.id.clone());
        Ok(json!({
            "task_id": task_id,
            "run_id": run_id,
            "status": status,
            "result": result,
        }))
    }
}

#[derive(Debug, Deserialize)]
struct McpRequest {
    #[serde(default)]
    jsonrpc: Option<String>,
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    params: Option<Value>,
}

fn rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message }
    })
}

fn negotiated_protocol(params: Option<&Value>) -> &'static str {
    match params
        .and_then(|value| value.get("protocolVersion"))
        .and_then(Value::as_str)
    {
        // The current Codex client and older supported clients accept this version. Returning a
        // stable version is safer than claiming an unknown request version.
        Some("2024-11-05") | Some("2025-03-26") => MCP_PROTOCOL_VERSION,
        _ => MCP_PROTOCOL_VERSION,
    }
}

fn tool_success(structured_content: Value) -> Value {
    let text = serde_json::to_string(&structured_content).unwrap_or_else(|_| "{}".to_string());
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": structured_content,
        "isError": false,
    })
}

fn tool_error(message: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": message }],
        "isError": true,
    })
}

fn required_text(arguments: &Value, key: &str, max_chars: usize) -> Result<String, &'static str> {
    let value = arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or("a required string argument is missing")?;
    if value.contains('\0') {
        return Err("text arguments cannot contain NUL characters");
    }
    Ok(compact(value, max_chars))
}

fn optional_text(arguments: &Value, key: &str, max_chars: usize) -> Option<String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty() && !value.contains('\0'))
        .map(|value| compact(value, max_chars))
}

fn wait_seconds(arguments: &Value) -> Result<u64, &'static str> {
    match arguments.get("wait_seconds") {
        None | Some(Value::Null) => Ok(DEFAULT_WAIT_SECONDS),
        Some(value) => value
            .as_u64()
            .filter(|seconds| *seconds <= MAX_WAIT_SECONDS)
            .ok_or("wait_seconds must be an integer between 0 and 55"),
    }
}

fn compact(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let clipped = text
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    format!("{clipped}…")
}

fn tool_catalog() -> Vec<Value> {
    vec![
        json!({
            "name": "r_code_delegate_readonly",
            "description": "Start a read-only R-Code Agent task in a workspace already opened by the user in R-Code. The task cannot edit files, run shell commands, or create further agents. Returns a task id and, when it finishes within wait_seconds, its concise result.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "workspace_path": { "type": "string", "description": "Canonical path of a workspace already opened in R-Code." },
                    "goal": { "type": "string", "description": "Concrete investigation or question for the R-Code Agent." },
                    "title": { "type": "string", "description": "Optional short task label." },
                    "provider_name": { "type": "string", "description": "Optional R-Code provider profile; omit to use the task default." },
                    "wait_seconds": { "type": "integer", "minimum": 0, "maximum": 55, "description": "How long to wait for a result; defaults to 45." }
                },
                "required": ["workspace_path", "goal"]
            }
        }),
        json!({
            "name": "r_code_task_status",
            "description": "Read the status and latest visible result of an R-Code task started by this MCP server session.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": { "task_id": { "type": "string" } },
                "required": ["task_id"]
            }
        }),
        json!({
            "name": "r_code_wait_for_result",
            "description": "Wait briefly for a task started by this MCP server session, then return its status and visible result.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "task_id": { "type": "string" },
                    "wait_seconds": { "type": "integer", "minimum": 0, "maximum": 55 }
                },
                "required": ["task_id"]
            }
        }),
        json!({
            "name": "r_code_cancel_task",
            "description": "Cancel a running task started by this MCP server session.",
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": { "task_id": { "type": "string" } },
                "required": ["task_id"]
            }
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_exposes_only_the_readonly_delegation_surface() {
        let tools = tool_catalog();
        assert_eq!(tools.len(), 4);
        let delegate = tools
            .iter()
            .find(|tool| tool["name"] == "r_code_delegate_readonly")
            .unwrap();
        let required = delegate["inputSchema"]["required"].as_array().unwrap();
        assert!(required.contains(&Value::String("workspace_path".to_string())));
        assert!(required.contains(&Value::String("goal".to_string())));
        assert!(delegate["description"]
            .as_str()
            .unwrap()
            .contains("cannot edit"));
    }

    #[test]
    fn argument_limits_are_enforced_before_any_agent_starts() {
        assert!(required_text(&json!({ "goal": "" }), "goal", 20).is_err());
        assert!(required_text(&json!({ "goal": "bad\u{0000}input" }), "goal", 20).is_err());
        assert_eq!(wait_seconds(&json!({})).unwrap(), DEFAULT_WAIT_SECONDS);
        assert!(wait_seconds(&json!({ "wait_seconds": 56 })).is_err());
    }

    #[tokio::test]
    async fn task_status_rejects_ids_not_created_by_this_server_process() {
        let dir = tempfile::tempdir().unwrap();
        let service = McpService::from_state(CommandState::in_memory(dir.path()).unwrap());
        let result = service
            .task_status(&json!({ "task_id": "not-owned" }))
            .await;
        assert_eq!(
            result.unwrap_err(),
            "task_id is not owned by this MCP server session"
        );
    }

    #[tokio::test]
    async fn initialize_and_tools_list_are_standard_json_rpc_results() {
        let dir = tempfile::tempdir().unwrap();
        let service = McpService::from_state(CommandState::in_memory(dir.path()).unwrap());
        let initialize = service
            .handle(McpRequest {
                jsonrpc: Some("2.0".into()),
                id: Some(json!(1)),
                method: Some("initialize".into()),
                params: Some(json!({ "protocolVersion": "2025-03-26" })),
            })
            .await
            .unwrap();
        assert_eq!(
            initialize["result"]["protocolVersion"],
            MCP_PROTOCOL_VERSION
        );
        let tools = service
            .handle(McpRequest {
                jsonrpc: Some("2.0".into()),
                id: Some(json!(2)),
                method: Some("tools/list".into()),
                params: None,
            })
            .await
            .unwrap();
        assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 4);
    }
}
