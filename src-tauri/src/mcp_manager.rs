use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use async_trait::async_trait;
use hermes_core::{ToolCallOutcome, ToolHost};
use r_code_mcp::{
    launch_fingerprint, ExternalToolError, ExternalToolHost, ExternalToolRisk,
    LaunchApprovalService, LaunchPreview, MarketInstallOption, MarketPage, MarketServer,
    McpServerConfig, McpServerSource, McpServerState, McpServerStatus, McpSupervisor,
    McpToolDescriptor, McpTransportConfig, RegistryClient, RmcpConnector, SecretRef, WebClient,
    WebSearchConfiguration, WebToolHost,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{Mutex, RwLock};

use crate::mcp_settings::{McpSettingsService, RESEARCH_SERVER_ID};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpTransportView {
    Builtin,
    Stdio {
        executable: String,
        args: Vec<String>,
        environment_names: Vec<String>,
    },
    StreamableHttp {
        url: String,
        header_names: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerView {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub enabled: bool,
    pub builtin: bool,
    pub source: McpServerSource,
    pub transport: McpTransportView,
    pub state: McpServerState,
    pub tool_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    pub launch_approved: bool,
}

impl McpServerView {
    fn from_config(config: McpServerConfig, status: Option<&McpServerStatus>) -> Self {
        let transport = match &config.transport {
            McpTransportConfig::Builtin { .. } => McpTransportView::Builtin,
            McpTransportConfig::Stdio { command, args, env } => McpTransportView::Stdio {
                executable: command.clone(),
                args: args.clone(),
                environment_names: env.keys().cloned().collect(),
            },
            McpTransportConfig::StreamableHttp { url, headers } => {
                McpTransportView::StreamableHttp {
                    url: url.clone(),
                    header_names: headers.keys().cloned().collect(),
                }
            }
        };
        let default_state = if config.enabled {
            McpServerState::Stopped
        } else {
            McpServerState::Disabled
        };
        Self {
            id: config.id.clone(),
            display_name: config.display_name.clone(),
            description: config.description.clone(),
            enabled: config.enabled,
            builtin: config.is_builtin(),
            source: config.source.clone(),
            transport,
            state: status.map(|status| status.state).unwrap_or(default_state),
            tool_count: status.map(|status| status.tool_count).unwrap_or(0),
            error_code: status.and_then(|status| status.error_code.clone()),
            launch_approved: LaunchApprovalService::is_approved(&config),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpManagerSnapshot {
    pub servers: Vec<McpServerView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpEditableTransport {
    Stdio {
        executable: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        environment_names: Vec<String>,
    },
    StreamableHttp {
        url: String,
        #[serde(default)]
        header_names: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpUpsertRequest {
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    pub transport: McpEditableTransport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpToggleResult {
    pub server: McpServerView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation: Option<LaunchPreview>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpCredentialStatus {
    pub name: String,
    pub configured: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpMarketInstallRequest {
    pub server: MarketServer,
    pub option_id: String,
    pub server_id: String,
}

/// Desktop-owned facade. Construction is offline: no external MCP connection or Registry request
/// happens until a test/call/search action explicitly asks for one.
pub struct McpManager {
    settings: McpSettingsService,
    supervisor: Arc<McpSupervisor>,
    registry: RegistryClient,
    approvals: LaunchApprovalService,
    web: Arc<WebToolHost>,
    tool_cache: RwLock<BTreeMap<String, Vec<McpToolDescriptor>>>,
    mutation: Mutex<()>,
    settings_error: RwLock<Option<String>>,
}

impl McpManager {
    pub fn new(config_dir: PathBuf) -> Self {
        let settings = McpSettingsService::new(config_dir);
        let (configs, settings_error) = match settings.load() {
            Ok(configs) => (configs, None),
            Err(error) => (Vec::new(), Some(error.to_string())),
        };
        let web_client = Arc::new(WebClient::new(WebSearchConfiguration::jina(None)));
        let connector = Arc::new(
            RmcpConnector::new(Arc::new(settings.secret_store())).with_research(web_client.clone()),
        );
        let supervisor = Arc::new(
            McpSupervisor::new(connector, configs)
                .expect("MCP settings service returns only validated configurations"),
        );
        let registry = RegistryClient::official(settings.registry_cache_path())
            .expect("official MCP Registry endpoint is a valid static URL");
        Self {
            settings,
            supervisor,
            registry,
            approvals: LaunchApprovalService::default(),
            web: Arc::new(WebToolHost::new(web_client)),
            tool_cache: RwLock::new(BTreeMap::new()),
            mutation: Mutex::new(()),
            settings_error: RwLock::new(settings_error),
        }
    }

    pub fn subscribe_statuses(
        &self,
    ) -> tokio::sync::watch::Receiver<BTreeMap<String, McpServerStatus>> {
        self.supervisor.subscribe_statuses()
    }

    pub async fn snapshot(&self) -> McpManagerSnapshot {
        let statuses = self
            .supervisor
            .status_snapshot()
            .into_iter()
            .map(|status| (status.id.clone(), status))
            .collect::<BTreeMap<_, _>>();
        let servers = self
            .supervisor
            .config_snapshot()
            .await
            .into_iter()
            .map(|config| McpServerView::from_config(config.clone(), statuses.get(&config.id)))
            .collect();
        McpManagerSnapshot {
            servers,
            settings_error: self.settings_error.read().await.clone(),
        }
    }

    pub async fn upsert(&self, request: McpUpsertRequest) -> Result<McpServerView, String> {
        let _mutation = self.mutation.lock().await;
        if request.id == RESEARCH_SERVER_ID {
            return Err("内置 MCP 的传输配置不可编辑；可以单独关闭它".to_string());
        }
        let configs = self.supervisor.config_snapshot().await;
        let previous = configs.iter().find(|config| config.id == request.id);
        let source = previous
            .map(|config| config.source.clone())
            .unwrap_or(McpServerSource::User);
        let transport = self.editable_transport(&request.id, request.transport)?;
        let mut config = McpServerConfig {
            id: request.id,
            display_name: request.display_name,
            description: request.description,
            enabled: false,
            source,
            transport,
            approved_launch_fingerprint: None,
        };
        if let Some(previous) = previous {
            if launch_fingerprint(previous) == launch_fingerprint(&config) {
                config.enabled = previous.enabled;
                config.approved_launch_fingerprint = previous.approved_launch_fingerprint.clone();
            }
        }
        let removed_credentials = previous
            .map(|previous| {
                let next = transport_references(&config.transport)
                    .into_iter()
                    .map(|(_, reference)| reference.clone())
                    .collect::<Vec<_>>();
                transport_references(&previous.transport)
                    .into_iter()
                    .map(|(_, reference)| reference.clone())
                    .filter(|reference| !next.contains(reference))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        config.validate().map_err(|error| error.to_string())?;
        self.persist_upsert(config.clone(), configs).await?;
        for reference in removed_credentials {
            if let Err(error) = self.settings.secret_store().delete(&reference) {
                tracing::warn!(%error, "could not remove obsolete MCP credential");
            }
        }
        Ok(self.view_for(config).await)
    }

    pub async fn remove(&self, server_id: &str) -> Result<(), String> {
        let _mutation = self.mutation.lock().await;
        let configs = self.supervisor.config_snapshot().await;
        let config = configs
            .iter()
            .find(|config| config.id == server_id)
            .ok_or_else(|| format!("未找到 MCP 服务：{server_id}"))?;
        if config.is_builtin() {
            return Err("内置 MCP 不能移除，但可以关闭".to_string());
        }
        let credentials = transport_references(&config.transport)
            .into_iter()
            .map(|(_, reference)| reference.clone())
            .collect::<Vec<_>>();
        let next = configs
            .iter()
            .filter(|config| config.id != server_id)
            .cloned()
            .collect::<Vec<_>>();
        self.settings
            .save(next)
            .map_err(|error| error.to_string())?;
        if let Err(error) = self.supervisor.remove(server_id).await {
            return match self.settings.save(configs) {
                Ok(_) => Err(error.to_string()),
                Err(rollback_error) => Err(format!(
                    "{error}; restoring MCP settings also failed: {rollback_error}"
                )),
            };
        }
        self.tool_cache.write().await.remove(server_id);
        for reference in credentials {
            if let Err(error) = self.settings.secret_store().delete(&reference) {
                tracing::warn!(%error, "could not remove credential for deleted MCP server");
            }
        }
        Ok(())
    }

    pub async fn toggle(
        &self,
        server_id: &str,
        enabled: bool,
        confirmation_token: Option<&str>,
    ) -> Result<McpToggleResult, String> {
        let _mutation = self.mutation.lock().await;
        let configs = self.supervisor.config_snapshot().await;
        let mut config = configs
            .iter()
            .find(|config| config.id == server_id)
            .cloned()
            .ok_or_else(|| format!("未找到 MCP 服务：{server_id}"))?;
        if enabled && !config.is_builtin() && !LaunchApprovalService::is_approved(&config) {
            let Some(token) = confirmation_token else {
                let preview = self
                    .approvals
                    .issue(&config, chrono::Utc::now())
                    .map_err(|error| error.to_string())?;
                return Ok(McpToggleResult {
                    server: self.view_for(config).await,
                    confirmation: Some(preview),
                });
            };
            self.approvals
                .confirm(token, &mut config, chrono::Utc::now())
                .map_err(|error| error.to_string())?;
        }
        config.enabled = enabled;
        self.persist_upsert(config.clone(), configs).await?;
        Ok(McpToggleResult {
            server: self.view_for(config).await,
            confirmation: None,
        })
    }

    pub async fn test_connection(&self, server_id: &str) -> Result<Vec<McpToolDescriptor>, String> {
        let tools = self
            .supervisor
            .list_tools(server_id)
            .await
            .map_err(|error| error.to_string())?;
        self.tool_cache
            .write()
            .await
            .insert(server_id.to_string(), tools.clone());
        Ok(tools)
    }

    pub async fn credential_status(
        &self,
        server_id: &str,
    ) -> Result<Vec<McpCredentialStatus>, String> {
        let config = self.find_config(server_id).await?;
        let references = transport_references(&config.transport);
        references
            .into_iter()
            .map(|(name, reference)| {
                self.settings
                    .secret_store()
                    .configured(reference)
                    .map(|configured| McpCredentialStatus { name, configured })
                    .map_err(|error| error.to_string())
            })
            .collect()
    }

    pub async fn set_credential(
        &self,
        server_id: &str,
        name: &str,
        value: &str,
    ) -> Result<(), String> {
        let config = self.find_config(server_id).await?;
        let reference = transport_references(&config.transport)
            .into_iter()
            .find(|(candidate, _)| candidate == name)
            .map(|(_, reference)| reference.clone())
            .ok_or_else(|| format!("MCP 服务没有名为 {name} 的凭据字段"))?;
        self.settings
            .secret_store()
            .set(&reference, value)
            .map_err(|error| error.to_string())
    }

    pub async fn delete_credential(&self, server_id: &str, name: &str) -> Result<(), String> {
        self.set_credential(server_id, name, "").await
    }

    pub async fn market_search(
        &self,
        query: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<MarketPage, String> {
        self.registry
            .search(query, cursor, limit)
            .await
            .map_err(|error| error.to_string())
    }

    pub fn prepare_market_install(
        &self,
        request: &McpMarketInstallRequest,
    ) -> Result<LaunchPreview, String> {
        let config = self.market_config(request)?;
        self.approvals
            .issue(&config, chrono::Utc::now())
            .map_err(|error| error.to_string())
    }

    pub async fn install_market(
        &self,
        request: &McpMarketInstallRequest,
        confirmation_token: &str,
    ) -> Result<McpServerView, String> {
        let _mutation = self.mutation.lock().await;
        let configs = self.supervisor.config_snapshot().await;
        if configs.iter().any(|config| config.id == request.server_id) {
            return Err(format!("MCP 服务 ID 已存在：{}", request.server_id));
        }
        let mut config = self.market_config(request)?;
        self.approvals
            .confirm(confirmation_token, &mut config, chrono::Utc::now())
            .map_err(|error| error.to_string())?;
        // Add approval proves that the user reviewed the plan, but first enable is deliberately a
        // separate confirmation and therefore starts with no persisted launch approval.
        config.enabled = false;
        config.approved_launch_fingerprint = None;
        self.persist_upsert(config.clone(), configs).await?;
        Ok(self.view_for(config).await)
    }

    pub async fn shutdown(&self) -> Result<(), String> {
        self.supervisor
            .shutdown()
            .await
            .map_err(|error| error.to_string())
    }

    fn editable_transport(
        &self,
        server_id: &str,
        transport: McpEditableTransport,
    ) -> Result<McpTransportConfig, String> {
        let secrets = self.settings.secret_store();
        match transport {
            McpEditableTransport::Stdio {
                executable,
                args,
                environment_names,
            } => Ok(McpTransportConfig::Stdio {
                command: executable,
                args,
                env: environment_names
                    .into_iter()
                    .map(|name| {
                        secrets
                            .reference(server_id, "env", &name)
                            .map(|reference| (name, reference))
                            .map_err(|error| error.to_string())
                    })
                    .collect::<Result<_, _>>()?,
            }),
            McpEditableTransport::StreamableHttp { url, header_names } => {
                Ok(McpTransportConfig::StreamableHttp {
                    url,
                    headers: header_names
                        .into_iter()
                        .map(|name| {
                            secrets
                                .reference(server_id, "header", &name)
                                .map(|reference| (name, reference))
                                .map_err(|error| error.to_string())
                        })
                        .collect::<Result<_, _>>()?,
                })
            }
        }
    }

    fn market_config(&self, request: &McpMarketInstallRequest) -> Result<McpServerConfig, String> {
        let option = request
            .server
            .install_options
            .iter()
            .find(|option| option.id == request.option_id)
            .ok_or_else(|| "所选 Registry 启动方案已不存在".to_string())?;
        let references = market_credential_references(&self.settings, &request.server_id, option)?;
        let mut config = option
            .to_install_plan(&request.server, &request.server_id, &references)
            .and_then(|plan| plan.into_disabled_config().map_err(Into::into))
            .map_err(|error| error.to_string())?;
        normalize_market_transport(&mut config.transport)?;
        config.validate().map_err(|error| error.to_string())?;
        Ok(config)
    }

    async fn persist_upsert(
        &self,
        config: McpServerConfig,
        previous: Vec<McpServerConfig>,
    ) -> Result<(), String> {
        let mut next = previous.clone();
        if let Some(existing) = next.iter_mut().find(|server| server.id == config.id) {
            *existing = config.clone();
        } else {
            next.push(config.clone());
        }
        self.settings
            .save(next)
            .map_err(|error| error.to_string())?;
        if let Err(error) = self.supervisor.upsert(config).await {
            return match self.settings.save(previous) {
                Ok(_) => Err(error.to_string()),
                Err(rollback_error) => Err(format!(
                    "{error}; restoring MCP settings also failed: {rollback_error}"
                )),
            };
        }
        *self.settings_error.write().await = None;
        Ok(())
    }

    async fn find_config(&self, server_id: &str) -> Result<McpServerConfig, String> {
        self.supervisor
            .config_snapshot()
            .await
            .into_iter()
            .find(|config| config.id == server_id)
            .ok_or_else(|| format!("未找到 MCP 服务：{server_id}"))
    }

    async fn view_for(&self, config: McpServerConfig) -> McpServerView {
        let status = self
            .supervisor
            .status_snapshot()
            .into_iter()
            .find(|status| status.id == config.id);
        McpServerView::from_config(config, status.as_ref())
    }

    async fn discover_local(&self, query: Option<&str>, include_disabled: bool) -> Value {
        let needle = query.unwrap_or_default().trim().to_ascii_lowercase();
        let statuses = self
            .supervisor
            .status_snapshot()
            .into_iter()
            .map(|status| (status.id.clone(), status))
            .collect::<BTreeMap<_, _>>();
        let cache = self.tool_cache.read().await;
        let servers = self
            .supervisor
            .config_snapshot()
            .await
            .into_iter()
            .filter(|config| include_disabled || config.enabled)
            .filter(|config| {
                needle.is_empty()
                    || config.id.to_ascii_lowercase().contains(&needle)
                    || config.display_name.to_ascii_lowercase().contains(&needle)
                    || config.description.to_ascii_lowercase().contains(&needle)
            })
            .map(|config| {
                let status = statuses.get(&config.id);
                json!({
                    "id": config.id,
                    "name": config.display_name,
                    "description": config.description,
                    "enabled": config.enabled,
                    "state": status.map(|status| status.state),
                    "tools": cache.get(&config.id).cloned().unwrap_or_default(),
                })
            })
            .collect::<Vec<_>>();
        json!({
            "scope": "installed_local_only",
            "registry_searched": false,
            "servers": servers,
        })
    }
}

#[async_trait]
impl ExternalToolHost for McpManager {
    async fn risk_for(&self, name: &str, args: &Value) -> ExternalToolRisk {
        match name {
            "mcp_discover" | "suggest_mcp" => ExternalToolRisk::LocalReadOnly,
            "web_search" | "web_fetch" => ExternalToolRisk::ReadOnlyRemote,
            "mcp_call" => {
                let server_id = args.get("server_id").and_then(Value::as_str);
                let tool = args.get("tool").and_then(Value::as_str);
                let cache = self.tool_cache.read().await;
                let read_only = server_id
                    .and_then(|server_id| cache.get(server_id))
                    .into_iter()
                    .flatten()
                    .any(|descriptor| {
                        Some(descriptor.name.as_str()) == tool && descriptor.read_only
                    });
                if read_only {
                    ExternalToolRisk::ReadOnlyRemote
                } else {
                    ExternalToolRisk::Mutating
                }
            }
            _ => ExternalToolRisk::Mutating,
        }
    }

    async fn call(&self, name: &str, args: Value) -> Result<ToolCallOutcome, ExternalToolError> {
        match name {
            "web_search" | "web_fetch" => ToolHost::call(self.web.as_ref(), name, args)
                .await
                .map_err(|error| ExternalToolError::new(error.to_string())),
            "mcp_discover" => {
                let query = args.get("query").and_then(Value::as_str);
                let include_disabled = args
                    .get("include_disabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                Ok(ToolCallOutcome {
                    content: self
                        .discover_local(query, include_disabled)
                        .await
                        .to_string(),
                    is_error: false,
                    metadata: None,
                })
            }
            "mcp_call" => {
                let server_id = required_string(&args, "server_id")?;
                let tool = required_string(&args, "tool")?;
                let arguments = args
                    .get("arguments")
                    .cloned()
                    .filter(Value::is_object)
                    .ok_or_else(|| ExternalToolError::new("mcp_call requires object arguments"))?;
                let outcome = self
                    .supervisor
                    .call_tool(server_id, tool, arguments)
                    .await
                    .map_err(|error| ExternalToolError::new(error.to_string()))?;
                Ok(outcome)
            }
            "suggest_mcp" => {
                let reason = required_string(&args, "reason")?;
                let server_id = args
                    .get("server_id")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty());
                let market_query = args
                    .get("market_query")
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty());
                if server_id.is_none() && market_query.is_none() {
                    return Err(ExternalToolError::new(
                        "suggest_mcp requires server_id or market_query",
                    ));
                }
                let suggestion = json!({
                    "server_id": server_id,
                    "market_query": market_query,
                    "reason": reason,
                });
                Ok(ToolCallOutcome {
                    content: json!({
                        "status": "suggested",
                        "message": "已向用户提供 MCP 配置建议；没有安装或启用任何服务。",
                        "action": "open_mcp_settings",
                        "server_id": server_id,
                        "market_query": market_query,
                        "reason": reason,
                    })
                    .to_string(),
                    is_error: false,
                    metadata: Some(json!({"mcp_suggestion": suggestion})),
                })
            }
            _ => Err(ExternalToolError::new(format!(
                "unknown external tool: {name}"
            ))),
        }
    }
}

fn transport_references(transport: &McpTransportConfig) -> Vec<(String, &SecretRef)> {
    match transport {
        McpTransportConfig::Builtin { .. } => Vec::new(),
        McpTransportConfig::Stdio { env, .. } => env
            .iter()
            .map(|(name, reference)| (name.clone(), reference))
            .collect(),
        McpTransportConfig::StreamableHttp { headers, .. } => headers
            .iter()
            .map(|(name, reference)| (name.clone(), reference))
            .collect(),
    }
}

fn normalize_market_transport(transport: &mut McpTransportConfig) -> Result<(), String> {
    #[cfg(windows)]
    if let McpTransportConfig::Stdio { command, args, .. } = transport {
        normalize_windows_registry_npx(command, args)?;
    }

    #[cfg(not(windows))]
    let _ = transport;

    Ok(())
}

#[cfg(windows)]
fn normalize_windows_registry_npx(
    command: &mut String,
    args: &mut Vec<String>,
) -> Result<(), String> {
    let mut search_dirs = Vec::new();
    if let Some(parent) = std::path::Path::new(command)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        search_dirs.push(parent.to_path_buf());
    }
    if let Some(node_home) = std::env::var_os("NODE_HOME") {
        search_dirs.push(std::path::PathBuf::from(node_home));
    }
    if let Some(path) = std::env::var_os("PATH") {
        search_dirs.extend(std::env::split_paths(&path));
    }
    normalize_windows_registry_npx_in(command, args, &search_dirs)
}

#[cfg(windows)]
fn normalize_windows_registry_npx_in(
    command: &mut String,
    args: &mut Vec<String>,
    search_dirs: &[std::path::PathBuf],
) -> Result<(), String> {
    let file_name = std::path::Path::new(command)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or(command)
        .to_ascii_lowercase();
    if !matches!(file_name.as_str(), "npx" | "npx.cmd" | "npx.ps1") {
        return Ok(());
    }

    let node = search_dirs
        .iter()
        .map(|directory| directory.join("node.exe"))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            "Registry 的 npm MCP 需要 Node.js；未找到 node.exe。请安装 Node.js，或改用精确的原生可执行文件配置 MCP".to_string()
        })?;
    let mut npm_roots = Vec::new();
    if let Some(parent) = node.parent() {
        npm_roots.push(parent.to_path_buf());
    }
    npm_roots.extend(search_dirs.iter().cloned());
    let npx_cli = npm_roots
        .iter()
        .map(|directory| {
            directory
                .join("node_modules")
                .join("npm")
                .join("bin")
                .join("npx-cli.js")
        })
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            "已找到 Node.js，但未找到 npm 的 npx-cli.js。请修复 npm 安装后重试".to_string()
        })?;

    *command = node.to_string_lossy().into_owned();
    args.insert(0, npx_cli.to_string_lossy().into_owned());
    Ok(())
}

fn market_credential_references(
    settings: &McpSettingsService,
    server_id: &str,
    option: &MarketInstallOption,
) -> Result<BTreeMap<String, SecretRef>, String> {
    let (kind, names) = match &option.transport {
        r_code_mcp::MarketInstallTransport::Stdio { environment, .. } => (
            "env",
            environment
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
        ),
        r_code_mcp::MarketInstallTransport::StreamableHttp { headers, .. } => (
            "header",
            headers
                .iter()
                .map(|item| item.name.as_str())
                .collect::<Vec<_>>(),
        ),
    };
    names
        .into_iter()
        .map(|name| {
            settings
                .secret_store()
                .reference(server_id, kind, name)
                .map(|reference| (name.to_string(), reference))
                .map_err(|error| error.to_string())
        })
        .collect()
}

fn required_string<'a>(args: &'a Value, key: &str) -> Result<&'a str, ExternalToolError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ExternalToolError::new(format!("missing {key}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn construction_is_lazy_and_builtin_can_be_disabled_live() {
        let temp = tempfile::tempdir().unwrap();
        let manager = McpManager::new(temp.path().to_path_buf());
        let initial = manager.snapshot().await;
        assert_eq!(initial.servers.len(), 1);
        assert_eq!(initial.servers[0].state, McpServerState::Stopped);

        let disabled = manager
            .toggle(RESEARCH_SERVER_ID, false, None)
            .await
            .unwrap();
        assert!(!disabled.server.enabled);
        let outcome = manager
            .call(
                "mcp_call",
                json!({"server_id": RESEARCH_SERVER_ID, "tool": "deep_research", "arguments": {"queries": ["test"]}}),
            )
            .await
            .unwrap();
        assert!(outcome.is_error);
        assert_eq!(outcome.metadata.unwrap()["reason"], "disabled");
    }

    #[tokio::test]
    async fn local_discovery_never_uses_the_online_registry() {
        let temp = tempfile::tempdir().unwrap();
        let manager = McpManager::new(temp.path().to_path_buf());
        let outcome = manager
            .call("mcp_discover", json!({"include_disabled": true}))
            .await
            .unwrap();
        let payload: Value = serde_json::from_str(&outcome.content).unwrap();
        assert_eq!(payload["registry_searched"], false);
        assert_eq!(payload["servers"][0]["id"], RESEARCH_SERVER_ID);
    }

    #[tokio::test]
    async fn editing_launch_shape_disables_and_invalidates_approval() {
        let temp = tempfile::tempdir().unwrap();
        let manager = McpManager::new(temp.path().to_path_buf());
        manager
            .upsert(McpUpsertRequest {
                id: "sample".to_string(),
                display_name: "Sample".to_string(),
                description: String::new(),
                transport: McpEditableTransport::Stdio {
                    executable: "npx".to_string(),
                    args: vec!["sample@1".to_string()],
                    environment_names: vec!["TOKEN".to_string()],
                },
            })
            .await
            .unwrap();
        let preview = manager.toggle("sample", true, None).await.unwrap();
        assert!(preview.confirmation.is_some());
        let enabled = manager
            .toggle("sample", true, Some(&preview.confirmation.unwrap().token))
            .await
            .unwrap();
        assert!(enabled.server.enabled);
        assert!(enabled.server.launch_approved);

        let edited = manager
            .upsert(McpUpsertRequest {
                id: "sample".to_string(),
                display_name: "Sample".to_string(),
                description: String::new(),
                transport: McpEditableTransport::Stdio {
                    executable: "npx".to_string(),
                    args: vec!["sample@2".to_string()],
                    environment_names: vec!["TOKEN".to_string()],
                },
            })
            .await
            .unwrap();
        assert!(!edited.enabled);
        assert!(!edited.launch_approved);
    }

    #[tokio::test]
    async fn suggestion_is_a_safe_deep_link_and_does_not_mutate_configuration() {
        let temp = tempfile::tempdir().unwrap();
        let manager = McpManager::new(temp.path().to_path_buf());
        let before = manager.snapshot().await;

        let outcome = manager
            .call(
                "suggest_mcp",
                json!({
                    "market_query": "github",
                    "reason": "需要认证后的仓库检索"
                }),
            )
            .await
            .unwrap();
        let payload: Value = serde_json::from_str(&outcome.content).unwrap();

        assert!(!outcome.is_error);
        assert_eq!(payload["action"], "open_mcp_settings");
        assert_eq!(payload["market_query"], "github");
        assert_eq!(manager.snapshot().await, before);
    }

    #[cfg(windows)]
    #[test]
    fn registry_npx_is_rewritten_to_an_exact_native_node_launch() {
        let temp = tempfile::tempdir().unwrap();
        let npm_bin = temp.path().join("node_modules").join("npm").join("bin");
        std::fs::create_dir_all(&npm_bin).unwrap();
        std::fs::write(temp.path().join("node.exe"), b"fixture").unwrap();
        std::fs::write(npm_bin.join("npx-cli.js"), b"fixture").unwrap();
        let mut command = temp.path().join("npx.cmd").to_string_lossy().into_owned();
        let mut args = vec!["-y".to_string(), "example-mcp@1".to_string()];

        normalize_windows_registry_npx_in(&mut command, &mut args, &[temp.path().to_path_buf()])
            .unwrap();

        assert_eq!(command, temp.path().join("node.exe").to_string_lossy());
        assert_eq!(args[0], npm_bin.join("npx-cli.js").to_string_lossy());
        assert_eq!(&args[1..], &["-y", "example-mcp@1"]);
    }
}
