//! Codex MCP client bridge.
//!
//! R-Code keeps this bridge deliberately small: it starts the official
//! `codex mcp-server` process, performs the MCP handshake, and only retains
//! the public thread identifier plus the final visible response.  Stderr,
//! model reasoning, tool transcripts, and credentials never cross this
//! boundary or enter the R-Code database.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};
use tokio_util::sync::CancellationToken;

use crate::codex_permissions::CodexDelegationPermissions;

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const MCP_START_TIMEOUT: Duration = Duration::from_secs(12);
const MCP_CALL_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const MAX_MCP_LINE_BYTES: usize = 512 * 1024;

/// 一个完成的 Codex MCP 调用中可以安全投影到产品状态的字段。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CodexMcpResponse {
    /// Codex 的公开 thread ID；可用于后续 `codex-reply`，但不是认证凭据。
    pub thread_id: Option<String>,
    /// Agent 明确返回给调用方的可见文本，不包含推理或工具输出。
    pub text: Option<String>,
}

/// 调用结果区分「已经取消」和「MCP 调用失败」。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexMcpCallOutcome {
    Completed(CodexMcpResponse),
    Cancelled,
}

/// 长生命周期的 Codex MCP 连接注册表。
///
/// 一个连接一次只处理一个 MCP request；这样一项取消可以安全地回收子进程，后续
/// 请求会按需重新建立干净连接，不会把不同 run 的响应串到一起。
#[derive(Default)]
pub struct CodexMcpRegistry {
    connection: Mutex<Option<CodexMcpConnection>>,
}

impl CodexMcpRegistry {
    /// 在指定工作区内启动一轮采用已验证权限快照的 Codex MCP 会话。
    pub(crate) async fn run(
        &self,
        cli_path: Option<PathBuf>,
        workspace: &Path,
        prompt: &str,
        permissions: CodexDelegationPermissions,
        cancellation: CancellationToken,
    ) -> Result<CodexMcpCallOutcome, CodexMcpError> {
        if cancellation.is_cancelled() {
            return Ok(CodexMcpCallOutcome::Cancelled);
        }

        // MCP 协议不会把 Codex 的 on-request 审批回传为 R-Code 的可交互卡片。
        // 上层会改走 App Server；这里显式拒绝，避免静默卡在无 stdin 的子进程中。
        if permissions.requests_r_code_approval() {
            return Err(CodexMcpError::ApprovalBridgeRequired);
        }

        let mut slot = tokio::select! {
            _ = cancellation.cancelled() => return Ok(CodexMcpCallOutcome::Cancelled),
            slot = self.connection.lock() => slot,
        };
        if slot.is_none() {
            let connection = timeout(
                MCP_START_TIMEOUT,
                CodexMcpConnection::spawn(cli_path, workspace),
            )
            .await
            .map_err(|_| CodexMcpError::StartupTimeout)??;
            *slot = Some(connection);
        }

        let request = {
            let connection = slot
                .as_mut()
                .expect("Codex MCP connection must exist after successful startup");
            tokio::select! {
                _ = cancellation.cancelled() => None,
                result = timeout(MCP_CALL_TIMEOUT, connection.call_codex(prompt, workspace, permissions)) => {
                    Some(result.unwrap_or(Err(CodexMcpError::CallTimeout)))
                },
            }
        };

        match request {
            None => {
                if let Some(connection) = slot.take() {
                    connection.shutdown().await;
                }
                Ok(CodexMcpCallOutcome::Cancelled)
            }
            Some(Ok(response)) => Ok(CodexMcpCallOutcome::Completed(response)),
            Some(Err(error)) => {
                // 断开的 stdio 会话不能可靠复用。关闭并让下一次显式调用重连。
                if let Some(connection) = slot.take() {
                    connection.shutdown().await;
                }
                Err(error)
            }
        }
    }

    /// 继续已保存的 Codex thread。当前 UI 尚未暴露续接入口，但保留完整 MCP
    /// 合同，供后续「继续外部会话」和自动恢复调用。
    pub async fn reply(
        &self,
        cli_path: Option<PathBuf>,
        workspace: &Path,
        thread_id: &str,
        prompt: &str,
        cancellation: CancellationToken,
    ) -> Result<CodexMcpCallOutcome, CodexMcpError> {
        if thread_id.trim().is_empty() || prompt.contains('\0') {
            return Err(CodexMcpError::Protocol);
        }
        if cancellation.is_cancelled() {
            return Ok(CodexMcpCallOutcome::Cancelled);
        }

        let mut slot = tokio::select! {
            _ = cancellation.cancelled() => return Ok(CodexMcpCallOutcome::Cancelled),
            slot = self.connection.lock() => slot,
        };
        if slot.is_none() {
            let connection = timeout(
                MCP_START_TIMEOUT,
                CodexMcpConnection::spawn(cli_path, workspace),
            )
            .await
            .map_err(|_| CodexMcpError::StartupTimeout)??;
            *slot = Some(connection);
        }
        let request = {
            let connection = slot
                .as_mut()
                .expect("Codex MCP connection must exist after successful startup");
            tokio::select! {
                _ = cancellation.cancelled() => None,
                result = timeout(MCP_CALL_TIMEOUT, connection.reply(thread_id, prompt)) => {
                    Some(result.unwrap_or(Err(CodexMcpError::CallTimeout)))
                },
            }
        };
        match request {
            None => {
                if let Some(connection) = slot.take() {
                    connection.shutdown().await;
                }
                Ok(CodexMcpCallOutcome::Cancelled)
            }
            Some(Ok(response)) => Ok(CodexMcpCallOutcome::Completed(response)),
            Some(Err(error)) => {
                if let Some(connection) = slot.take() {
                    connection.shutdown().await;
                }
                Err(error)
            }
        }
    }
}

/// MCP 客户端错误的脱敏分类。Display 文本可直接给普通用户，不包含 CLI 原始输出。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexMcpError {
    Launch,
    StartupTimeout,
    CallTimeout,
    ApprovalBridgeRequired,
    Disconnected,
    Protocol,
    RemoteFailure,
}

impl std::fmt::Display for CodexMcpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::Launch => "无法启动 Codex MCP 服务。",
            Self::StartupTimeout => "Codex MCP 服务启动超时。",
            Self::CallTimeout => "Codex MCP 连续 15 分钟没有完成本次委派，R-Code 已停止该会话。",
            Self::ApprovalBridgeRequired => {
                "“请求批准”模式需要 R-Code 的 Codex 审批桥，不能通过 MCP 直连运行。"
            }
            Self::Disconnected => "Codex MCP 服务已断开。",
            Self::Protocol => "Codex MCP 服务返回了无法识别的协议数据。",
            Self::RemoteFailure => "Codex MCP 服务未能完成本次委派。",
        };
        f.write_str(message)
    }
}

impl std::error::Error for CodexMcpError {}

struct CodexMcpConnection {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    stderr_drain: Option<tokio::task::JoinHandle<()>>,
}

impl CodexMcpConnection {
    async fn spawn(cli_path: Option<PathBuf>, workspace: &Path) -> Result<Self, CodexMcpError> {
        let mut command = codex_mcp_server_command(cli_path, workspace)?;
        let mut child = command.spawn().map_err(|error| {
            tracing::warn!(kind = ?error.kind(), "failed to launch Codex MCP server");
            CodexMcpError::Launch
        })?;
        let stdin = child.stdin.take().ok_or(CodexMcpError::Launch)?;
        let stdout = child.stdout.take().ok_or(CodexMcpError::Launch)?;
        let stderr_drain = child.stderr.take().map(|mut stderr| {
            tokio::spawn(async move {
                // Codex progress/errors may include local paths or sensitive prompt context. Drain
                // the pipe to prevent backpressure but never persist, log, or relay it.
                let _ = tokio::io::copy(&mut stderr, &mut tokio::io::sink()).await;
            })
        });
        let mut connection = Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
            stderr_drain,
        };
        if let Err(error) = connection.initialize().await {
            connection.shutdown().await;
            return Err(error);
        }
        Ok(connection)
    }

    async fn initialize(&mut self) -> Result<(), CodexMcpError> {
        let initialized = self
            .request(
                "initialize",
                Some(json!({
                    "protocolVersion": MCP_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {
                        "name": "r-code",
                        "version": env!("CARGO_PKG_VERSION"),
                    }
                })),
            )
            .await?;
        if !initialized.is_object() {
            return Err(CodexMcpError::Protocol);
        }
        self.notify("notifications/initialized", None).await?;
        let tools = self.request("tools/list", None).await?;
        let available = tools
            .get("tools")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("name").and_then(Value::as_str))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if !available.contains(&"codex") || !available.contains(&"codex-reply") {
            return Err(CodexMcpError::Protocol);
        }
        Ok(())
    }

    async fn call_codex(
        &mut self,
        prompt: &str,
        workspace: &Path,
        permissions: CodexDelegationPermissions,
    ) -> Result<CodexMcpResponse, CodexMcpError> {
        let workspace = workspace.to_str().ok_or(CodexMcpError::Protocol)?;
        let mut arguments = serde_json::Map::new();
        arguments.insert("prompt".into(), Value::String(prompt.to_string()));
        arguments.insert("cwd".into(), Value::String(workspace.to_string()));
        arguments.insert(
            "sandbox".into(),
            Value::String(permissions.sandbox().as_str().to_string()),
        );
        arguments.insert(
            "approval-policy".into(),
            Value::String(permissions.approval_policy().as_str().to_string()),
        );
        arguments.insert(
            "config".into(),
            json!({ "approvals_reviewer": permissions.approvals_reviewer().as_str() }),
        );
        let result = self
            .request(
                "tools/call",
                Some(json!({
                    "name": "codex",
                    "arguments": arguments,
                })),
            )
            .await?;
        extract_codex_response(&result)
    }

    async fn reply(
        &mut self,
        thread_id: &str,
        prompt: &str,
    ) -> Result<CodexMcpResponse, CodexMcpError> {
        let result = self
            .request(
                "tools/call",
                Some(json!({
                    "name": "codex-reply",
                    "arguments": {
                        "threadId": thread_id,
                        "prompt": prompt,
                    }
                })),
            )
            .await?;
        extract_codex_response(&result)
    }

    async fn notify(&mut self, method: &str, params: Option<Value>) -> Result<(), CodexMcpError> {
        let mut payload = serde_json::Map::new();
        payload.insert("jsonrpc".into(), Value::String("2.0".into()));
        payload.insert("method".into(), Value::String(method.to_string()));
        if let Some(params) = params {
            payload.insert("params".into(), params);
        }
        self.write_value(Value::Object(payload)).await
    }

    async fn request(
        &mut self,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, CodexMcpError> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let mut payload = serde_json::Map::new();
        payload.insert("jsonrpc".into(), Value::String("2.0".into()));
        payload.insert("id".into(), Value::from(id));
        payload.insert("method".into(), Value::String(method.to_string()));
        if let Some(params) = params {
            payload.insert("params".into(), params);
        }
        self.write_value(Value::Object(payload)).await?;

        loop {
            let mut line = String::new();
            let bytes = self
                .stdout
                .read_line(&mut line)
                .await
                .map_err(|_| CodexMcpError::Disconnected)?;
            if bytes == 0 {
                return Err(CodexMcpError::Disconnected);
            }
            if bytes > MAX_MCP_LINE_BYTES {
                return Err(CodexMcpError::Protocol);
            }
            let value: Value =
                serde_json::from_str(line.trim()).map_err(|_| CodexMcpError::Protocol)?;
            if value.get("id").and_then(Value::as_u64) != Some(id) {
                // MCP notifications and stale responses are not part of this request.  The bridge
                // intentionally makes only one request at a time, so it is safe to ignore them.
                continue;
            }
            if value.get("error").is_some() {
                return Err(CodexMcpError::RemoteFailure);
            }
            return value.get("result").cloned().ok_or(CodexMcpError::Protocol);
        }
    }

    async fn write_value(&mut self, value: Value) -> Result<(), CodexMcpError> {
        let payload = serde_json::to_vec(&value).map_err(|_| CodexMcpError::Protocol)?;
        self.stdin
            .write_all(&payload)
            .await
            .map_err(|_| CodexMcpError::Disconnected)?;
        self.stdin
            .write_all(b"\n")
            .await
            .map_err(|_| CodexMcpError::Disconnected)?;
        self.stdin
            .flush()
            .await
            .map_err(|_| CodexMcpError::Disconnected)
    }

    async fn shutdown(mut self) {
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
        if let Some(drain) = self.stderr_drain.take() {
            let _ = drain.await;
        }
    }
}

/// 构造 Codex MCP 子进程，不接收任何来自 WebView/MCP caller 的命令片段。
fn codex_mcp_server_command(
    cli_path: Option<PathBuf>,
    workspace: &Path,
) -> Result<Command, CodexMcpError> {
    #[cfg(windows)]
    let mut command = match cli_path {
        Some(path)
            if path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("exe")) =>
        {
            let mut command = Command::new(path);
            command.arg("mcp-server");
            command
        }
        path => {
            // npm 常以 .cmd shim 安装 Codex。只把经过校验的 CLI 路径和固定参数交给
            // cmd；工作区与任务文本均不会进入 shell 命令串。
            let executable = path.unwrap_or_else(|| PathBuf::from("codex"));
            windows_cmd_path(&executable)?;
            let mut command = Command::new("cmd.exe");
            command
                .args(["/D", "/S", "/C", "call"])
                .arg(executable)
                .arg("mcp-server");
            command
        }
    };
    #[cfg(not(windows))]
    let mut command = {
        let mut command = Command::new(cli_path.unwrap_or_else(|| PathBuf::from("codex")));
        command.arg("mcp-server");
        command
    };

    command
        .current_dir(workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    Ok(command)
}

#[cfg(windows)]
fn windows_cmd_path(path: &Path) -> Result<(), CodexMcpError> {
    let text = path.to_str().ok_or(CodexMcpError::Launch)?;
    if text.chars().any(|character| {
        matches!(
            character,
            '\0' | '\r' | '\n' | '"' | '&' | '|' | '<' | '>' | '^' | '%' | '!'
        )
    }) {
        return Err(CodexMcpError::Launch);
    }
    Ok(())
}

fn extract_codex_response(result: &Value) -> Result<CodexMcpResponse, CodexMcpError> {
    let structured = result.get("structuredContent").unwrap_or(result);
    let thread_id = structured
        .get("threadId")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned);
    let text = structured
        .get("content")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            result
                .get("content")
                .and_then(Value::as_array)
                .and_then(|items| {
                    items.iter().find_map(|item| {
                        (item.get("type").and_then(Value::as_str) == Some("text"))
                            .then(|| item.get("text").and_then(Value::as_str))
                            .flatten()
                            .map(ToOwned::to_owned)
                    })
                })
        });
    if thread_id.is_none() && text.is_none() {
        return Err(CodexMcpError::Protocol);
    }
    Ok(CodexMcpResponse { thread_id, text })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_structured_codex_response_without_tool_trace() {
        let result = json!({
            "structuredContent": {
                "threadId": "thread-123",
                "content": "调查已完成"
            },
            "content": [{"type": "text", "text": "legacy copy"}],
            "internalTrace": "must not be read"
        });
        assert_eq!(
            extract_codex_response(&result).unwrap(),
            CodexMcpResponse {
                thread_id: Some("thread-123".to_string()),
                text: Some("调查已完成".to_string()),
            }
        );
    }

    #[test]
    fn legacy_content_is_supported_when_structured_content_is_absent() {
        let result = json!({
            "content": [{"type": "text", "text": "legacy answer"}]
        });
        assert_eq!(
            extract_codex_response(&result).unwrap().text.as_deref(),
            Some("legacy answer")
        );
    }

    #[test]
    fn empty_codex_response_is_protocol_error() {
        assert_eq!(
            extract_codex_response(&json!({})),
            Err(CodexMcpError::Protocol)
        );
    }

    #[cfg(windows)]
    #[test]
    fn mcp_server_uses_the_exact_cmd_shim() {
        let command = codex_mcp_server_command(
            Some(PathBuf::from(r"C:\Program Files\Codex\codex.cmd")),
            Path::new(r"C:\repo"),
        )
        .unwrap();
        let command = command.as_std();
        assert_eq!(command.get_program(), "cmd.exe");
        let args = command
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(args[0..4], ["/D", "/S", "/C", "call"]);
        assert_eq!(args[4], r"C:\Program Files\Codex\codex.cmd");
        assert_eq!(args[5], "mcp-server");
    }

    #[cfg(windows)]
    #[test]
    fn mcp_server_exe_receives_the_subcommand() {
        let command = codex_mcp_server_command(
            Some(PathBuf::from(r"C:\Codex\codex.exe")),
            Path::new(r"C:\repo"),
        )
        .unwrap();
        let command = command.as_std();
        assert_eq!(command.get_program(), r"C:\Codex\codex.exe");
        assert_eq!(
            command
                .get_args()
                .map(|value| value.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            ["mcp-server"]
        );
    }
}
