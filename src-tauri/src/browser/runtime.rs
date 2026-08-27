use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Version of the Rust/TypeScript/JSON Browser contract, independent of runtime releases.
pub const BROWSER_CONTRACT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserTargetPlatform {
    Windows,
    Macos,
    Linux,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserTargetArch {
    X86_64,
    Aarch64,
}

/// Signed-app-owned description of one immutable Browser runtime artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserRuntimeManifest {
    pub schema_version: u32,
    pub runtime_version: String,
    pub target_platform: BrowserTargetPlatform,
    pub target_arch: BrowserTargetArch,
    pub wrapper_version: String,
    pub node_version: String,
    pub playwright_version: String,
    pub chromium_revision: String,
    pub asset_url: String,
    pub asset_size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserRuntimeState {
    NotInstalled,
    Installing,
    Ready,
    RepairRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserProcessState {
    NotStarted,
    Starting,
    Running,
    Stopping,
    Stopped,
    Crashed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserTabState {
    Loading,
    Ready,
    Closed,
    Crashed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserScreenshotRef {
    pub screenshot_id: String,
    pub path: String,
    pub media_type: String,
    pub width: u32,
    pub height: u32,
    pub captured_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserSession {
    pub session_id: String,
    pub task_id: String,
    pub profile_path: String,
    pub runtime_version: String,
    pub process_state: BrowserProcessState,
    pub active_tab_id: Option<String>,
    pub last_url: Option<String>,
    pub last_screenshot: Option<BrowserScreenshotRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserTab {
    pub tab_id: String,
    pub session_id: String,
    pub opener_tab_id: Option<String>,
    pub url: String,
    pub title: String,
    pub state: BrowserTabState,
    pub active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
