//! R-Code product MCP layer.
//!
//! The shared `agent-contracts` submodule owns protocol-neutral contracts. This crate owns product
//! policy: persisted server metadata, secret references, lifecycle supervision, marketplace
//! installation plans, native web tools, and the bundled research server.

// clippy 1.99 对 async_trait 展开的 boxing 方法报 double_must_use（方法与其返回
// 的 BoxFuture 同时标 must_use）——宏输出不可控，crate 级豁免。
#![allow(clippy::double_must_use)]

pub mod client;
pub mod host;
pub mod installer;
pub mod model;
pub mod registry;
pub mod research;
pub mod runtime;
pub mod web;

pub use client::{
    EmptySecretResolver, McpClientError, McpClientSession, McpConnector, RmcpConnector,
    SecretResolver,
};
pub use host::{external_tool_specs, ExternalToolError, ExternalToolHost, ExternalToolRisk};
pub use installer::{
    launch_fingerprint, LaunchApprovalError, LaunchApprovalService, LaunchPreview,
    LaunchPreviewTransport, DEFAULT_APPROVAL_TTL,
};

pub use model::{
    decode_tool_name, encode_tool_name, BuiltinMcpServer, McpConfigError, McpInstallPlan,
    McpServerConfig, McpServerSource, McpServerState, McpServerStatus, McpToolDescriptor,
    McpTransportConfig, SecretRef, WebFetchResult, WebLimits, WebSearchProvider, WebSearchResult,
    WebSource, RESERVED_TOOL_NAMES,
};
pub use registry::{
    MarketEnvironmentVariable, MarketInstallOption, MarketInstallTransport, MarketPackageKind,
    MarketPage, MarketServer, RegistryClient, RegistryError, RegistryHttpAdapter,
    ReqwestRegistryHttpAdapter, OFFICIAL_REGISTRY_ENDPOINT,
};
pub use research::ResearchServer;
pub use runtime::{McpRuntimeError, McpSupervisor};
pub use web::{
    is_blocked_ip, DnsResolver, ReqwestWebHttpAdapter, SystemDnsResolver, WebClient, WebError,
    WebHttpAdapter, WebHttpRequest, WebHttpResponse, WebSearchConfiguration, WebToolHost,
};

/// E09-C：unverified-override 审计的 MCP 工具面——`review.overrides.list`
/// 同一 RPC 的 canonical camelCase 投影，只读。键序四端一致，顺序只在这
/// 里排一次（桌面 Permissions / TUI 覆盖层 / 远端投影逐字对齐）。
pub mod override_audit {
    use serde::{Deserialize, Serialize};

    /// MCP 工具名（编码后的产品工具标识）。
    pub const OVERRIDE_LIST_TOOL: &str = "review_overrides_list";

    /// 该工具转发的 daemon RPC。
    pub const OVERRIDE_LIST_RPC: &str = "review.overrides.list";

    /// 审计行的 canonical 键序。
    pub const CANONICAL_FIELDS: [&str; 8] = [
        "overrideId",
        "taskId",
        "candidateDigest",
        "actorId",
        "sessionId",
        "reason",
        "checks",
        "createdAtMs",
    ];

    /// 一条不可变审计行（逐字段等于 store 记录）。
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct UnverifiedOverrideView {
        pub override_id: String,
        pub task_id: String,
        pub candidate_digest: String,
        pub actor_id: String,
        pub session_id: String,
        pub reason: String,
        /// 精确失败检查 id，升序。
        pub checks: Vec<String>,
        pub created_at_ms: i64,
    }

    /// 工具描述符：可选 `taskId` 过滤，输出即 canonical 行数组。
    pub fn tool_descriptor() -> serde_json::Value {
        serde_json::json!({
            "name": OVERRIDE_LIST_TOOL,
            "description": "List immutable unverified-override audit rows (the exact failed checks per accepted override).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "taskId": { "type": "string", "description": "scope the rows to one task" }
                }
            }
        })
    }

    /// 该工具调用时发给 daemon 的参数。
    pub fn call_params(task_id: Option<&str>) -> serde_json::Value {
        serde_json::json!({"taskId": task_id})
    }
}
