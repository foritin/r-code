mod catalog;
mod request;
mod result;

pub use catalog::{browser_tool_contracts, BrowserToolContract, BrowserToolName};
pub use request::{
    BrowserClickRequest, BrowserCloseRequest, BrowserConsoleRequest, BrowserElementTarget,
    BrowserNavigateRequest, BrowserNavigationTarget, BrowserNetworkErrorsRequest,
    BrowserOpenRequest, BrowserPressRequest, BrowserScreenshotRequest, BrowserScrollRequest,
    BrowserSelectRequest, BrowserSnapshotRequest, BrowserTabsRequest, BrowserToolRequest,
    BrowserTypeRequest, BrowserWaitCondition, BrowserWaitRequest,
};
pub use result::{
    BrowserActionMetadata, BrowserActionResult, BrowserCloseResult, BrowserConsoleEntry,
    BrowserConsoleResult, BrowserElementValue, BrowserNavigateResult, BrowserNetworkError,
    BrowserNetworkErrorsResult, BrowserOpenResult, BrowserScreenshotResult, BrowserSnapshot,
    BrowserSnapshotElement, BrowserSnapshotResult, BrowserTabResult, BrowserTabsResult,
    BrowserToolResult, BrowserWaitResult,
};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserLoadState {
    DomContentLoaded,
    Load,
    NetworkIdle,
}
