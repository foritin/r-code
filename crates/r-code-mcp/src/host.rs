use async_trait::async_trait;
use hermes_core::{ToolCallOutcome, ToolSource, ToolSpec};
use serde_json::{json, Value};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalToolRisk {
    /// Local catalog inspection or suggestion with no external access or mutation.
    LocalReadOnly,
    /// Public or explicitly declared read-only network access.
    ReadOnlyRemote,
    /// Unknown or state-changing external operation.
    Mutating,
}

#[derive(Debug, Error)]
#[error("external tool host error: {message}")]
pub struct ExternalToolError {
    message: String,
}

impl ExternalToolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Stable product boundary injected into every Agent session. Implementations may change their
/// live server catalog, but the model-visible schema count stays constant.
#[async_trait]
pub trait ExternalToolHost: Send + Sync + 'static {
    fn tool_specs(&self) -> Vec<ToolSpec> {
        external_tool_specs()
    }

    fn owns_tool(&self, name: &str) -> bool {
        matches!(
            name,
            "web_search" | "web_fetch" | "mcp_discover" | "mcp_call" | "suggest_mcp"
        )
    }

    async fn risk_for(&self, name: &str, args: &Value) -> ExternalToolRisk;

    async fn call(&self, name: &str, args: Value) -> Result<ToolCallOutcome, ExternalToolError>;
}

pub fn external_tool_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            name: "web_search".to_string(),
            description: "Search the public web first for current information and return bounded sources with URLs.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "minLength": 1, "maxLength": 500},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 10}
                },
                "required": ["query"],
                "additionalProperties": false
            }),
            source: ToolSource::Builtin,
            requires_confirmation: false,
        },
        ToolSpec {
            name: "web_fetch".to_string(),
            description: "Fetch one public HTTP(S) source with SSRF, redirect, MIME and size limits.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string", "minLength": 1},
                    "max_chars": {"type": "integer", "minimum": 1, "maximum": 80000}
                },
                "required": ["url"],
                "additionalProperties": false
            }),
            source: ToolSource::Builtin,
            requires_confirmation: false,
        },
        ToolSpec {
            name: "mcp_discover".to_string(),
            description: "Inspect only the locally installed MCP catalog and live status. This never searches the online Registry.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "maxLength": 200},
                    "include_disabled": {"type": "boolean"}
                },
                "additionalProperties": false
            }),
            source: ToolSource::Custom { id: "r-code-mcp-control".to_string() },
            requires_confirmation: false,
        },
        ToolSpec {
            name: "mcp_call".to_string(),
            description: "Call one tool on an already installed and enabled MCP service. Disabled or missing services return a normal result.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server_id": {"type": "string", "minLength": 1},
                    "tool": {"type": "string", "minLength": 1},
                    "arguments": {"type": "object"}
                },
                "required": ["server_id", "tool", "arguments"],
                "additionalProperties": false
            }),
            source: ToolSource::Custom { id: "r-code-mcp-control".to_string() },
            requires_confirmation: true,
        },
        ToolSpec {
            name: "suggest_mcp".to_string(),
            description: "Recommend an installed MCP service or a safe marketplace search to the user. This never enables or installs anything.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "server_id": {"type": "string"},
                    "market_query": {"type": "string", "maxLength": 200},
                    "reason": {"type": "string", "minLength": 1, "maxLength": 500}
                },
                "required": ["reason"],
                "anyOf": [
                    {"required": ["server_id"]},
                    {"required": ["market_query"]}
                ],
                "additionalProperties": false
            }),
            source: ToolSource::Custom { id: "r-code-mcp-control".to_string() },
            requires_confirmation: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_schema_count_is_constant() {
        let specs = external_tool_specs();
        assert_eq!(specs.len(), 5);
        assert_eq!(
            specs.iter().filter(|spec| spec.name == "mcp_call").count(),
            1
        );
    }
}
