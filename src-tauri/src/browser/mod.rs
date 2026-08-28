//! Browser automation public contracts.
//!
//! This module deliberately contains no Playwright, download, process, or persistence state
//! machine. It freezes the cross-layer shapes and the single ToolGateway registration boundary
//! consumed by both native R-Code runs and Codex App Server dynamic tools.

pub mod asset_manifest;
pub mod installer;
mod commands;
mod events;
mod runtime;
mod scope;
mod tool_gateway;
mod tools;

pub use commands::{browser_agent_contract, BrowserAgentContract};
pub use events::{BrowserEvent, BrowserEventEnvelope};
pub use runtime::{
    BrowserProcessState, BrowserRuntimeManifest, BrowserRuntimeState, BrowserScreenshotRef,
    BrowserSession, BrowserTab, BrowserTabState, BrowserTargetArch, BrowserTargetPlatform,
    BROWSER_CONTRACT_SCHEMA_VERSION,
};
pub use scope::{
    BrowserOrigin, BrowserPermissionCapability, BrowserPermissionGrant, BrowserPermissionRequest,
    BrowserPermissionScope, BrowserTimeoutMs, BrowserWorkspacePath, MAX_BROWSER_TIMEOUT_MS,
};
pub use tool_gateway::{
    codex_dynamic_browser_tools, execute_codex_browser_tool, register_browser_agent_tools,
    BrowserCodexExecution, BrowserToolExecutor,
};
pub use tools::{
    browser_tool_contracts, BrowserActionMetadata, BrowserActionResult, BrowserClickRequest,
    BrowserCloseRequest, BrowserCloseResult, BrowserConsoleEntry, BrowserConsoleRequest,
    BrowserConsoleResult, BrowserElementTarget, BrowserElementValue, BrowserLoadState,
    BrowserNavigateRequest, BrowserNavigateResult, BrowserNavigationTarget, BrowserNetworkError,
    BrowserNetworkErrorsRequest, BrowserNetworkErrorsResult, BrowserOpenRequest, BrowserOpenResult,
    BrowserPressRequest, BrowserScreenshotRequest, BrowserScreenshotResult, BrowserScrollRequest,
    BrowserSelectRequest, BrowserSnapshot, BrowserSnapshotElement, BrowserSnapshotRequest,
    BrowserSnapshotResult, BrowserTabResult, BrowserTabsRequest, BrowserTabsResult,
    BrowserToolContract, BrowserToolName, BrowserToolRequest, BrowserToolResult,
    BrowserTypeRequest, BrowserWaitCondition, BrowserWaitRequest, BrowserWaitResult,
};
