use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Stable tool names owned by R-Code and therefore unavailable as MCP server identifiers.
pub const RESERVED_TOOL_NAMES: &[&str] = &[
    "web_search",
    "web_fetch",
    "mcp_discover",
    "mcp_call",
    "suggest_mcp",
];

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum McpConfigError {
    #[error("MCP server id is empty")]
    EmptyServerId,
    #[error("invalid MCP server id '{0}'; use lowercase ASCII letters, digits, '-' or '_'")]
    InvalidServerId(String),
    #[error("MCP server id '{0}' is reserved by R-Code")]
    ReservedServerId(String),
    #[error("MCP command must not be empty")]
    EmptyCommand,
    #[error("MCP command or argument contains a control character")]
    CommandControlCharacter,
    #[error("MCP HTTP URL must use https: {0}")]
    InsecureHttpUrl(String),
    #[error("MCP HTTP URL must not contain embedded credentials")]
    EmbeddedCredentials,
    #[error("invalid MCP tool name '{0}'")]
    InvalidToolName(String),
    #[error("install plan server id does not match its server configuration")]
    InstallPlanServerMismatch,
}

/// A key in the operating-system credential store. It never contains the secret value itself.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretRef(String);

impl SecretRef {
    pub fn new(value: impl Into<String>) -> Result<Self, McpConfigError> {
        let value = value.into();
        if value.trim().is_empty() || value.chars().any(char::is_control) {
            return Err(McpConfigError::CommandControlCharacter);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinMcpServer {
    Research,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpTransportConfig {
    Builtin {
        server: BuiltinMcpServer,
    },
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, SecretRef>,
    },
    StreamableHttp {
        url: String,
        #[serde(default)]
        headers: BTreeMap<String, SecretRef>,
    },
}

impl McpTransportConfig {
    pub fn validate(&self) -> Result<(), McpConfigError> {
        match self {
            Self::Builtin { .. } => Ok(()),
            Self::Stdio { command, args, .. } => {
                if command.trim().is_empty() {
                    return Err(McpConfigError::EmptyCommand);
                }
                if command.chars().any(char::is_control)
                    || args
                        .iter()
                        .any(|argument| argument.chars().any(char::is_control))
                {
                    return Err(McpConfigError::CommandControlCharacter);
                }
                Ok(())
            }
            Self::StreamableHttp { url, .. } => {
                let parsed = url::Url::parse(url)
                    .map_err(|_| McpConfigError::InsecureHttpUrl(url.clone()))?;
                if parsed.scheme() != "https" {
                    return Err(McpConfigError::InsecureHttpUrl(url.clone()));
                }
                if !parsed.username().is_empty() || parsed.password().is_some() {
                    return Err(McpConfigError::EmbeddedCredentials);
                }
                Ok(())
            }
        }
    }

    pub fn launch_fingerprint_material(&self) -> Option<String> {
        match self {
            Self::Builtin { .. } => None,
            Self::Stdio { command, args, env } => Some(format!(
                "stdio\0{}\0{}\0{}",
                command,
                args.join("\0"),
                env.keys().cloned().collect::<Vec<_>>().join("\0")
            )),
            Self::StreamableHttp { url, headers } => Some(format!(
                "streamable_http\0{}\0{}",
                url,
                headers.keys().cloned().collect::<Vec<_>>().join("\0")
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpServerSource {
    Builtin,
    User,
    Registry {
        registry_url: String,
        name: String,
        version: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        repository_url: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerConfig {
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub enabled: bool,
    pub source: McpServerSource,
    pub transport: McpTransportConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_launch_fingerprint: Option<String>,
}

impl McpServerConfig {
    pub fn validate(&self) -> Result<(), McpConfigError> {
        validate_server_id(&self.id)?;
        self.transport.validate()
    }

    pub fn is_builtin(&self) -> bool {
        matches!(self.source, McpServerSource::Builtin)
    }
}

fn validate_server_id(id: &str) -> Result<(), McpConfigError> {
    if id.is_empty() {
        return Err(McpConfigError::EmptyServerId);
    }
    let valid = id.len() <= 64
        && id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'_'
        })
        && id.as_bytes()[0].is_ascii_lowercase();
    if !valid || id.contains("__") {
        return Err(McpConfigError::InvalidServerId(id.to_string()));
    }
    if RESERVED_TOOL_NAMES.contains(&id) {
        return Err(McpConfigError::ReservedServerId(id.to_string()));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpServerState {
    Disabled,
    Stopped,
    Starting,
    Running,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerStatus {
    pub id: String,
    pub state: McpServerState,
    #[serde(default)]
    pub tool_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpToolDescriptor {
    pub server_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub input_schema: serde_json::Value,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpInstallPlan {
    pub server_id: String,
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    pub source: McpServerSource,
    pub transport: McpTransportConfig,
    #[serde(default)]
    pub required_secret_names: Vec<String>,
}

impl McpInstallPlan {
    pub fn validate(&self) -> Result<(), McpConfigError> {
        validate_server_id(&self.server_id)?;
        self.transport.validate()
    }

    pub fn into_disabled_config(self) -> Result<McpServerConfig, McpConfigError> {
        self.validate()?;
        Ok(McpServerConfig {
            id: self.server_id,
            display_name: self.display_name,
            description: self.description,
            enabled: false,
            source: self.source,
            transport: self.transport,
            approved_launch_fingerprint: None,
        })
    }
}

pub fn encode_tool_name(server_id: &str, tool_name: &str) -> Result<String, McpConfigError> {
    validate_server_id(server_id)?;
    if tool_name.trim().is_empty()
        || tool_name.contains("__")
        || tool_name.chars().any(char::is_control)
    {
        return Err(McpConfigError::InvalidToolName(tool_name.to_string()));
    }
    Ok(format!("{server_id}__{tool_name}"))
}

pub fn decode_tool_name(value: &str) -> Result<(&str, &str), McpConfigError> {
    let (server, tool) = value
        .split_once("__")
        .ok_or_else(|| McpConfigError::InvalidToolName(value.to_string()))?;
    validate_server_id(server)?;
    if tool.is_empty() || tool.contains("__") || tool.chars().any(char::is_control) {
        return Err(McpConfigError::InvalidToolName(value.to_string()));
    }
    Ok((server, tool))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebSearchProvider {
    Jina,
    Brave,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebSource {
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub snippet: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
    pub retrieved_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebSearchResult {
    pub query: String,
    pub provider: WebSearchProvider,
    pub sources: Vec<WebSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebFetchResult {
    pub url: String,
    pub content_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub content: String,
    pub retrieved_at: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebLimits {
    pub timeout_ms: u64,
    pub max_bytes: usize,
    pub max_chars: usize,
    pub max_redirects: usize,
    pub max_results: usize,
}

impl Default for WebLimits {
    fn default() -> Self {
        Self {
            timeout_ms: 15_000,
            max_bytes: 2 * 1024 * 1024,
            max_chars: 80_000,
            max_redirects: 5,
            max_results: 10,
        }
    }
}
