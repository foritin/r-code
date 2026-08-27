use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::browser::{
    BrowserConsoleEntry, BrowserNetworkError, BrowserPermissionGrant, BrowserPermissionRequest,
    BrowserProcessState, BrowserRuntimeState, BrowserScreenshotRef, BrowserTab, BrowserToolName,
    BROWSER_CONTRACT_SCHEMA_VERSION,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserEventEnvelope {
    #[serde(default = "browser_contract_schema_version")]
    pub schema_version: u32,
    pub event_id: String,
    pub task_id: String,
    pub session_id: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub event: BrowserEvent,
}

const fn browser_contract_schema_version() -> u32 {
    BROWSER_CONTRACT_SCHEMA_VERSION
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BrowserEvent {
    RuntimeStateChanged {
        runtime_version: Option<String>,
        state: BrowserRuntimeState,
    },
    SessionStateChanged {
        state: BrowserProcessState,
    },
    TabOpened {
        tab: BrowserTab,
    },
    TabUpdated {
        tab: BrowserTab,
    },
    TabClosed {
        tab_id: String,
    },
    ActionStarted {
        action_id: String,
        tool: BrowserToolName,
        tab_id: Option<String>,
        url: Option<String>,
    },
    ActionCompleted {
        action_id: String,
        tool: BrowserToolName,
        tab_id: Option<String>,
        url: Option<String>,
    },
    ActionFailed {
        action_id: String,
        tool: BrowserToolName,
        tab_id: Option<String>,
        url: Option<String>,
        error_code: String,
    },
    PermissionRequired {
        request: BrowserPermissionRequest,
    },
    PermissionGranted {
        grant: BrowserPermissionGrant,
    },
    PermissionRevoked {
        grant: BrowserPermissionGrant,
    },
    ScreenshotCaptured {
        screenshot: BrowserScreenshotRef,
    },
    ConsoleEntry {
        tab_id: String,
        entry: BrowserConsoleEntry,
    },
    NetworkError {
        tab_id: String,
        error: BrowserNetworkError,
    },
}
