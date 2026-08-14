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
    "mcp_registry_search",
    "mcp_prepare_install",
    "mcp_prepare_enable",
    "mcp_create_draft",
    "mcp_save_draft",
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
    #[error("MCP HTTP URL must use https unless it targets an explicit local loopback host: {0}")]
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
                if !parsed.username().is_empty() || parsed.password().is_some() {
                    return Err(McpConfigError::EmbeddedCredentials);
                }
                let secure_remote = parsed.scheme() == "https" && parsed.host().is_some();
                if !secure_remote && !is_explicit_loopback_http_url(url, &parsed) {
                    return Err(McpConfigError::InsecureHttpUrl(url.clone()));
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

/// Cleartext MCP is permitted only for an explicitly written loopback authority.
///
/// `url::Url` intentionally canonicalizes legacy IPv4 spellings such as `127.1` and
/// `2130706433` to `127.0.0.1`. Those spellings are network-loopback in practice, but accepting
/// them makes the security boundary hard to audit and creates room for parser disagreement in
/// downstream clients. Check both the parsed host and the original authority so only the three
/// documented forms are accepted.
fn is_explicit_loopback_http_url(raw: &str, parsed: &url::Url) -> bool {
    if parsed.scheme() != "http" || parsed.port() == Some(0) {
        return false;
    }

    let Some((scheme, remainder)) = raw.split_once("://") else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("http") {
        return false;
    }
    let authority = remainder.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return false;
    }

    let raw_host = if authority.starts_with('[') {
        let Some(end) = authority.find(']') else {
            return false;
        };
        let suffix = &authority[end + 1..];
        if !valid_optional_port_suffix(suffix) {
            return false;
        }
        &authority[..=end]
    } else if let Some((host, port)) = authority.rsplit_once(':') {
        if host.contains(':') || port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit())
        {
            return false;
        }
        host
    } else {
        authority
    };

    match parsed.host() {
        Some(url::Host::Domain(host)) => {
            raw_host.eq_ignore_ascii_case("localhost") && host.eq_ignore_ascii_case("localhost")
        }
        Some(url::Host::Ipv4(host)) => raw_host == "127.0.0.1" && host.is_loopback(),
        Some(url::Host::Ipv6(host)) => raw_host.eq_ignore_ascii_case("[::1]") && host.is_loopback(),
        None => false,
    }
}

fn valid_optional_port_suffix(suffix: &str) -> bool {
    suffix.is_empty()
        || suffix
            .strip_prefix(':')
            .is_some_and(|port| !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpServerSource {
    Builtin,
    User,
    Generated {
        source_path: String,
        created_at: String,
    },
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

    pub fn is_generated(&self) -> bool {
        matches!(self.source, McpServerSource::Generated { .. })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_server_source_serializes_and_round_trips() {
        let source = McpServerSource::Generated {
            source_path: r"D:\projects\demo-mcp".to_string(),
            created_at: "2026-08-11T10:30:00Z".to_string(),
        };

        let encoded = serde_json::to_value(&source).unwrap();
        assert_eq!(encoded["kind"], "generated");
        assert_eq!(encoded["source_path"], r"D:\projects\demo-mcp");
        assert_eq!(
            serde_json::from_value::<McpServerSource>(encoded).unwrap(),
            source
        );
    }
}
