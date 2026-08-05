use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

#[cfg(windows)]
use std::path::Path;

use async_trait::async_trait;
use hermes_core::ToolCallOutcome;
use http::{HeaderName, HeaderValue};
use rmcp::{
    model::{
        CallToolRequest, CallToolRequestParams, ClientInfo, ClientRequest, Implementation,
        ProtocolVersion, ServerResult,
    },
    service::{Peer, PeerRequestOptions, RoleClient, RunningService},
    transport::{
        streamable_http_client::StreamableHttpClientTransportConfig, StreamableHttpClientTransport,
        TokioChildProcess,
    },
    ClientServiceExt,
};
use serde_json::{Map, Value};
use thiserror::Error;
use tokio::sync::Mutex;

use crate::{
    research::connect_research_session, BuiltinMcpServer, McpServerConfig, McpToolDescriptor,
    McpTransportConfig, SecretRef, WebClient,
};

const SESSION_CLOSE_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Error)]
pub enum McpClientError {
    #[error("MCP transport is not available for this server type")]
    UnsupportedTransport,
    #[error("MCP credential is not configured: {0}")]
    MissingSecret(String),
    #[error("invalid MCP request header name: {0}")]
    InvalidHeaderName(String),
    #[error("invalid MCP request header value")]
    InvalidHeaderValue,
    #[error("failed to start MCP process: {0}")]
    ProcessStart(String),
    #[error("unsafe Windows MCP launcher '{0}'; use a native executable instead of a shell or script shim")]
    UnsafeWindowsLauncher(String),
    #[error("failed to initialize MCP session: {0}")]
    Initialize(String),
    #[error("MCP session is closed")]
    Closed,
    #[error("MCP tool arguments must be a JSON object")]
    InvalidArguments,
    #[error("MCP tool call cancelled")]
    Cancelled,
    #[error("MCP request failed: {0}")]
    Request(String),
    #[error("MCP session shutdown failed: {0}")]
    Shutdown(String),
}

#[async_trait]
pub trait SecretResolver: Send + Sync {
    async fn resolve(&self, reference: &SecretRef) -> Result<Option<String>, McpClientError>;
}

#[derive(Default)]
pub struct EmptySecretResolver;

/// 单个 MCP 工具调用的最大时长（F2）。超过即报超时错误，避免子代理/主 agent
/// 因 MCP 服务端无响应而无限挂起——宿主取消无法打断进行中的请求。
pub const MCP_CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
const MCP_ABORT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(25);
const MCP_CANCEL_NOTIFY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

#[async_trait]
impl SecretResolver for EmptySecretResolver {
    async fn resolve(&self, _reference: &SecretRef) -> Result<Option<String>, McpClientError> {
        Ok(None)
    }
}

#[async_trait]
pub trait McpClientSession: Send + Sync {
    async fn list_tools(&self, server_id: &str) -> Result<Vec<McpToolDescriptor>, McpClientError>;
    async fn call_tool(&self, name: &str, args: Value) -> Result<ToolCallOutcome, McpClientError>;
    async fn call_tool_with_abort(
        &self,
        name: &str,
        args: Value,
        abort: Option<Arc<AtomicBool>>,
    ) -> Result<ToolCallOutcome, McpClientError> {
        let call = self.call_tool(name, args);
        tokio::pin!(call);
        loop {
            if abort
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Relaxed))
            {
                return Err(McpClientError::Cancelled);
            }
            if abort.is_none() {
                return call.await;
            }
            tokio::select! {
                result = &mut call => return result,
                _ = tokio::time::sleep(MCP_ABORT_POLL_INTERVAL) => {}
            }
        }
    }
    async fn close(&self) -> Result<(), McpClientError>;
    fn is_closed(&self) -> bool;
}

#[async_trait]
pub trait McpConnector: Send + Sync {
    async fn connect(
        &self,
        config: &McpServerConfig,
    ) -> Result<Arc<dyn McpClientSession>, McpClientError>;
}

pub struct RmcpConnector {
    secrets: Arc<dyn SecretResolver>,
    research: Option<Arc<WebClient>>,
}

impl RmcpConnector {
    pub fn new(secrets: Arc<dyn SecretResolver>) -> Self {
        Self {
            secrets,
            research: None,
        }
    }

    pub fn with_research(mut self, web: Arc<WebClient>) -> Self {
        self.research = Some(web);
        self
    }

    async fn resolve_secret(&self, reference: &SecretRef) -> Result<String, McpClientError> {
        self.secrets
            .resolve(reference)
            .await?
            .ok_or_else(|| McpClientError::MissingSecret(reference.as_str().to_string()))
    }

    fn client_info() -> ClientInfo {
        let mut info = ClientInfo::default();
        info.protocol_version = ProtocolVersion::V_2026_07_28;
        info.client_info = Implementation::new("r-code", env!("CARGO_PKG_VERSION"));
        info
    }

    pub(crate) async fn serve<T, E, A>(&self, transport: T) -> Result<RmcpSession, McpClientError>
    where
        T: rmcp::transport::IntoTransport<RoleClient, E, A>,
        E: std::error::Error + Send + Sync + 'static,
    {
        let service = Self::client_info()
            .serve_with_lifecycle(
                transport,
                rmcp::ClientLifecycleMode::Auto {
                    preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                    legacy_version: Some(ProtocolVersion::V_2025_11_25),
                },
            )
            .await
            .map_err(|error| McpClientError::Initialize(error.to_string()))?;
        Ok(RmcpSession::new(service))
    }

    async fn connect_stdio(
        &self,
        command: &str,
        args: &[String],
        env: &std::collections::BTreeMap<String, SecretRef>,
    ) -> Result<RmcpSession, McpClientError> {
        let mut process = build_stdio_process(command, args)?;
        process.kill_on_drop(true);
        for (name, reference) in env {
            process.env(name, self.resolve_secret(reference).await?);
        }
        let transport = TokioChildProcess::new(process)
            .map_err(|error| McpClientError::ProcessStart(error.to_string()))?;
        self.serve(transport).await
    }

    async fn connect_http(
        &self,
        url: &str,
        headers: &std::collections::BTreeMap<String, SecretRef>,
    ) -> Result<RmcpSession, McpClientError> {
        let mut resolved_headers = HashMap::with_capacity(headers.len());
        for (name, reference) in headers {
            let header_name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| McpClientError::InvalidHeaderName(name.clone()))?;
            let secret = self.resolve_secret(reference).await?;
            let header_value =
                HeaderValue::from_str(&secret).map_err(|_| McpClientError::InvalidHeaderValue)?;
            resolved_headers.insert(header_name, header_value);
        }
        let transport_config = StreamableHttpClientTransportConfig::with_uri(url.to_string())
            .custom_headers(resolved_headers)
            .max_sse_event_size(1024 * 1024);
        let transport = StreamableHttpClientTransport::from_config(transport_config);
        self.serve(transport).await
    }
}

fn build_stdio_process(
    command: &str,
    args: &[String],
) -> Result<tokio::process::Command, McpClientError> {
    #[cfg(windows)]
    reject_unsafe_windows_launcher(command)?;

    let mut process = tokio::process::Command::new(command);
    process.args(args);
    Ok(process)
}

#[cfg(windows)]
fn reject_unsafe_windows_launcher(command: &str) -> Result<(), McpClientError> {
    let file_name = Path::new(command)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(command)
        .to_ascii_lowercase();
    let extension = Path::new(&file_name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    let script_or_shell = matches!(extension, "bat" | "cmd" | "ps1")
        || matches!(
            file_name.as_str(),
            "cmd"
                | "cmd.exe"
                | "powershell"
                | "powershell.exe"
                | "pwsh"
                | "pwsh.exe"
                | "wscript"
                | "wscript.exe"
                | "cscript"
                | "cscript.exe"
                | "npx"
                | "npm"
        );
    if script_or_shell {
        return Err(McpClientError::UnsafeWindowsLauncher(command.to_string()));
    }
    Ok(())
}

#[async_trait]
impl McpConnector for RmcpConnector {
    async fn connect(
        &self,
        config: &McpServerConfig,
    ) -> Result<Arc<dyn McpClientSession>, McpClientError> {
        config
            .validate()
            .map_err(|error| McpClientError::Initialize(error.to_string()))?;
        let session = match &config.transport {
            McpTransportConfig::Stdio { command, args, env } => {
                self.connect_stdio(command, args, env).await?
            }
            McpTransportConfig::StreamableHttp { url, headers } => {
                self.connect_http(url, headers).await?
            }
            McpTransportConfig::Builtin {
                server: BuiltinMcpServer::Research,
            } => {
                let web = self
                    .research
                    .clone()
                    .ok_or(McpClientError::UnsupportedTransport)?;
                return connect_research_session(self, web).await;
            }
        };
        Ok(Arc::new(session))
    }
}

pub(crate) struct RmcpSession {
    peer: Peer<RoleClient>,
    service: Mutex<Option<RunningService<RoleClient, ClientInfo>>>,
}

impl RmcpSession {
    pub(crate) fn new(service: RunningService<RoleClient, ClientInfo>) -> Self {
        Self {
            peer: service.peer().clone(),
            service: Mutex::new(Some(service)),
        }
    }
}

#[async_trait]
impl McpClientSession for RmcpSession {
    async fn list_tools(&self, server_id: &str) -> Result<Vec<McpToolDescriptor>, McpClientError> {
        if self.is_closed() {
            return Err(McpClientError::Closed);
        }
        let tools = self
            .peer
            .list_all_tools()
            .await
            .map_err(|error| McpClientError::Request(error.to_string()))?;
        Ok(tools
            .into_iter()
            .map(|tool| McpToolDescriptor {
                server_id: server_id.to_string(),
                name: tool.name.into_owned(),
                description: tool
                    .description
                    .map(|value| value.into_owned())
                    .unwrap_or_default(),
                input_schema: Value::Object((*tool.input_schema).clone()),
                read_only: tool
                    .annotations
                    .as_ref()
                    .and_then(|annotations| annotations.read_only_hint)
                    .unwrap_or(false),
            })
            .collect())
    }

    async fn call_tool(&self, name: &str, args: Value) -> Result<ToolCallOutcome, McpClientError> {
        self.call_tool_with_abort(name, args, None).await
    }

    async fn call_tool_with_abort(
        &self,
        name: &str,
        args: Value,
        abort: Option<Arc<AtomicBool>>,
    ) -> Result<ToolCallOutcome, McpClientError> {
        if self.is_closed() {
            return Err(McpClientError::Closed);
        }
        let arguments: Map<String, Value> = match args {
            Value::Object(arguments) => arguments,
            Value::Null => Map::new(),
            _ => return Err(McpClientError::InvalidArguments),
        };
        if abort
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Relaxed))
        {
            return Err(McpClientError::Cancelled);
        }
        let params = CallToolRequestParams::new(name.to_string()).with_arguments(arguments);
        let request = ClientRequest::CallToolRequest(CallToolRequest::new(params));
        // Use rmcp's cancellable request handle rather than dropping `Peer::call_tool`: dropping
        // the convenience future only stops the local waiter, while the remote MCP server can
        // continue mutating state. RequestHandle::cancel emits `notifications/cancelled` with the
        // exact request id.
        let mut handle = self
            .peer
            .send_cancellable_request(request, PeerRequestOptions::no_options())
            .await
            .map_err(|error| McpClientError::Request(error.to_string()))?;
        enum Wait<T> {
            Response(T),
            Cancelled,
            TimedOut,
        }
        let deadline = tokio::time::sleep(MCP_CALL_TIMEOUT);
        tokio::pin!(deadline);
        let wait = loop {
            if abort
                .as_ref()
                .is_some_and(|flag| flag.load(Ordering::Relaxed))
            {
                break Wait::Cancelled;
            }
            break tokio::select! {
                response = &mut handle.rx => Wait::Response(response),
                _ = &mut deadline => Wait::TimedOut,
                _ = tokio::time::sleep(MCP_ABORT_POLL_INTERVAL), if abort.is_some() => continue,
            };
        };
        let response = match wait {
            Wait::Response(response) => response
                .map_err(|_| McpClientError::Request("MCP transport closed".to_string()))?
                .map_err(|error| McpClientError::Request(error.to_string()))?,
            Wait::Cancelled => {
                let _ = tokio::time::timeout(
                    MCP_CANCEL_NOTIFY_TIMEOUT,
                    handle.cancel(Some("R-Code agent run cancelled".to_string())),
                )
                .await;
                return Err(McpClientError::Cancelled);
            }
            Wait::TimedOut => {
                let _ = tokio::time::timeout(
                    MCP_CANCEL_NOTIFY_TIMEOUT,
                    handle.cancel(Some("R-Code MCP tool timeout".to_string())),
                )
                .await;
                return Err(McpClientError::Request(format!("MCP 工具 {name} 调用超时")));
            }
        };
        let result = match response {
            ServerResult::CallToolResult(result) => result,
            _ => {
                return Err(McpClientError::Request(
                    "MCP returned an unexpected response".to_string(),
                ))
            }
        };
        let text = result
            .content
            .iter()
            .filter_map(|block| block.as_text().map(|content| content.text.as_str()))
            .collect::<Vec<_>>()
            .join("\n");
        let metadata = serde_json::to_value(&result).ok();
        Ok(ToolCallOutcome {
            content: if text.is_empty() {
                result
                    .structured_content
                    .as_ref()
                    .map(Value::to_string)
                    .unwrap_or_default()
            } else {
                text
            },
            is_error: result.is_error.unwrap_or(false),
            metadata,
        })
    }

    async fn close(&self) -> Result<(), McpClientError> {
        let Some(mut service) = self.service.lock().await.take() else {
            return Ok(());
        };
        service
            .close_with_timeout(SESSION_CLOSE_TIMEOUT)
            .await
            .map_err(|error| McpClientError::Shutdown(error.to_string()))?;
        Ok(())
    }

    fn is_closed(&self) -> bool {
        self.peer.is_transport_closed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    struct PendingSession {
        started: Arc<AtomicBool>,
        dropped: Arc<AtomicBool>,
        completed: Arc<AtomicBool>,
    }

    #[async_trait]
    impl McpClientSession for PendingSession {
        async fn list_tools(
            &self,
            _server_id: &str,
        ) -> Result<Vec<McpToolDescriptor>, McpClientError> {
            Ok(Vec::new())
        }

        async fn call_tool(
            &self,
            _name: &str,
            _args: Value,
        ) -> Result<ToolCallOutcome, McpClientError> {
            let _drop = DropFlag(self.dropped.clone());
            self.started.store(true, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(30)).await;
            self.completed.store(true, Ordering::SeqCst);
            Ok(ToolCallOutcome {
                content: "unexpected completion".to_string(),
                is_error: false,
                metadata: None,
            })
        }

        async fn close(&self) -> Result<(), McpClientError> {
            Ok(())
        }

        fn is_closed(&self) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn default_abort_adapter_drops_a_non_cooperative_session_call() {
        let started = Arc::new(AtomicBool::new(false));
        let dropped = Arc::new(AtomicBool::new(false));
        let completed = Arc::new(AtomicBool::new(false));
        let session = PendingSession {
            started: started.clone(),
            dropped: dropped.clone(),
            completed: completed.clone(),
        };
        let abort = Arc::new(AtomicBool::new(false));
        let cancel_abort = abort.clone();
        let cancel = tokio::spawn(async move {
            while !started.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            cancel_abort.store(true, Ordering::SeqCst);
        });

        let result = tokio::time::timeout(
            Duration::from_secs(1),
            session.call_tool_with_abort("pending", Value::Null, Some(abort)),
        )
        .await
        .expect("MCP cancellation must be prompt");
        cancel.await.unwrap();

        assert!(matches!(result, Err(McpClientError::Cancelled)));
        assert!(dropped.load(Ordering::SeqCst));
        assert!(!completed.load(Ordering::SeqCst));
    }

    #[test]
    fn stdio_arguments_remain_separate_and_never_form_a_shell_string() {
        let args = vec![
            "--title".to_string(),
            "value with spaces".to_string(),
            "&&".to_string(),
        ];
        let process = build_stdio_process("mcp-server", &args).unwrap();
        let actual = process
            .as_std()
            .get_args()
            .map(|value| value.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert_eq!(actual, args);
    }

    #[cfg(windows)]
    #[test]
    fn windows_rejects_batch_and_shell_launchers() {
        for command in [
            "npx",
            "server.cmd",
            "server.bat",
            "server.ps1",
            "cmd.exe",
            "pwsh.exe",
        ] {
            assert!(matches!(
                build_stdio_process(command, &[]),
                Err(McpClientError::UnsafeWindowsLauncher(_))
            ));
        }
        assert!(build_stdio_process("C:\\tools\\server.exe", &[]).is_ok());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_preserves_absolute_launchers_and_literal_arguments() {
        let args = vec!["--stdio".to_string(), "value with spaces".to_string()];
        let process = build_stdio_process("/opt/homebrew/bin/uvx", &args).unwrap();
        assert_eq!(
            process.as_std().get_program().to_string_lossy(),
            "/opt/homebrew/bin/uvx"
        );
        assert_eq!(
            process
                .as_std()
                .get_args()
                .map(|value| value.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            args
        );
    }
}
