use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{BrowserLoadState, BrowserToolName};
use crate::browser::{BrowserProcessState, BrowserScreenshotRef, BrowserSession, BrowserTab};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserActionMetadata {
    pub session_id: String,
    pub tab_id: Option<String>,
    pub url: Option<String>,
    pub action_id: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BrowserElementValue {
    Missing,
    Visible { text: String },
    Redacted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserSnapshotElement {
    pub reference: String,
    pub role: String,
    pub name: String,
    pub value: BrowserElementValue,
    pub disabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserSnapshot {
    pub snapshot_id: String,
    pub text: String,
    pub elements: Vec<BrowserSnapshotElement>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserConsoleEntry {
    pub level: String,
    pub text: String,
    pub timestamp: DateTime<Utc>,
    pub redacted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserNetworkError {
    pub method: String,
    pub url: String,
    pub error_text: String,
    pub timestamp: DateTime<Utc>,
    pub redacted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserOpenResult {
    #[serde(flatten)]
    pub action: BrowserActionMetadata,
    pub session: BrowserSession,
    pub tab: BrowserTab,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserNavigateResult {
    #[serde(flatten)]
    pub action: BrowserActionMetadata,
    pub tab: BrowserTab,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserSnapshotResult {
    #[serde(flatten)]
    pub action: BrowserActionMetadata,
    pub snapshot: BrowserSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserScreenshotResult {
    #[serde(flatten)]
    pub action: BrowserActionMetadata,
    pub screenshot: BrowserScreenshotRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserActionResult {
    #[serde(flatten)]
    pub action: BrowserActionMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserWaitResult {
    #[serde(flatten)]
    pub action: BrowserActionMetadata,
    pub satisfied: bool,
    pub load_state: Option<BrowserLoadState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserTabsResult {
    #[serde(flatten)]
    pub action: BrowserActionMetadata,
    pub tabs: Vec<BrowserTab>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserTabResult {
    #[serde(flatten)]
    pub action: BrowserActionMetadata,
    pub tab: BrowserTab,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserConsoleResult {
    #[serde(flatten)]
    pub action: BrowserActionMetadata,
    pub entries: Vec<BrowserConsoleEntry>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserNetworkErrorsResult {
    #[serde(flatten)]
    pub action: BrowserActionMetadata,
    pub errors: Vec<BrowserNetworkError>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserCloseResult {
    #[serde(flatten)]
    pub action: BrowserActionMetadata,
    pub process_state: BrowserProcessState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "tool", content = "output")]
pub enum BrowserToolResult {
    #[serde(rename = "open")]
    Open(Box<BrowserOpenResult>),
    #[serde(rename = "navigate")]
    Navigate(Box<BrowserNavigateResult>),
    #[serde(rename = "snapshot")]
    Snapshot(Box<BrowserSnapshotResult>),
    #[serde(rename = "screenshot")]
    Screenshot(Box<BrowserScreenshotResult>),
    #[serde(rename = "click")]
    Click(BrowserActionResult),
    #[serde(rename = "type")]
    Type(BrowserActionResult),
    #[serde(rename = "select")]
    Select(BrowserActionResult),
    #[serde(rename = "press")]
    Press(BrowserActionResult),
    #[serde(rename = "scroll")]
    Scroll(BrowserActionResult),
    #[serde(rename = "wait")]
    Wait(Box<BrowserWaitResult>),
    #[serde(rename = "tabs")]
    Tabs(Box<BrowserTabsResult>),
    #[serde(rename = "console")]
    Console(Box<BrowserConsoleResult>),
    #[serde(rename = "network-errors")]
    NetworkErrors(Box<BrowserNetworkErrorsResult>),
    #[serde(rename = "close")]
    Close(Box<BrowserCloseResult>),
}

impl BrowserToolResult {
    pub const fn tool_name(&self) -> BrowserToolName {
        match self {
            Self::Open(_) => BrowserToolName::Open,
            Self::Navigate(_) => BrowserToolName::Navigate,
            Self::Snapshot(_) => BrowserToolName::Snapshot,
            Self::Screenshot(_) => BrowserToolName::Screenshot,
            Self::Click(_) => BrowserToolName::Click,
            Self::Type(_) => BrowserToolName::Type,
            Self::Select(_) => BrowserToolName::Select,
            Self::Press(_) => BrowserToolName::Press,
            Self::Scroll(_) => BrowserToolName::Scroll,
            Self::Wait(_) => BrowserToolName::Wait,
            Self::Tabs(_) => BrowserToolName::Tabs,
            Self::Console(_) => BrowserToolName::Console,
            Self::NetworkErrors(_) => BrowserToolName::NetworkErrors,
            Self::Close(_) => BrowserToolName::Close,
        }
    }
}
