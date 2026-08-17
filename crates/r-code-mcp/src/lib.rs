//! R-Code product MCP layer.
//!
//! The shared `agent-contracts` submodule owns protocol-neutral contracts. This crate owns product
//! policy: persisted server metadata, secret references, lifecycle supervision, marketplace
//! installation plans, native web tools, and the bundled research server.

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
