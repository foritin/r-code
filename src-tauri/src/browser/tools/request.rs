use serde::{de::Error as _, Deserialize, Serialize};
use url::Url;

use super::{BrowserLoadState, BrowserToolName};
use crate::browser::{BrowserTimeoutMs, BrowserWorkspacePath};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrowserNavigationTarget {
    Url { url: String },
    WorkspaceFile { path: BrowserWorkspacePath },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrowserElementTarget {
    Css {
        selector: String,
    },
    Text {
        text: String,
        #[serde(default)]
        exact: bool,
    },
    SnapshotRef {
        reference: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BrowserWaitCondition {
    Selector {
        selector: String,
    },
    Text {
        text: String,
        #[serde(default)]
        exact: bool,
    },
    Url {
        url: String,
    },
    LoadState {
        state: BrowserLoadState,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserOpenRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<BrowserNavigationTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserNavigateRequest {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    pub target: BrowserNavigationTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserSnapshotRequest {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserScreenshotRequest {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default)]
    pub full_page: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserClickRequest {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    pub target: BrowserElementTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserTypeRequest {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    pub target: BrowserElementTarget,
    pub text: String,
    #[serde(default)]
    pub clear: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserSelectRequest {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    pub target: BrowserElementTarget,
    pub values: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserPressRequest {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<BrowserElementTarget>,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserScrollRequest {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<BrowserElementTarget>,
    pub delta_x: i32,
    pub delta_y: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserWaitRequest {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    pub condition: BrowserWaitCondition,
    #[serde(default)]
    pub timeout_ms: BrowserTimeoutMs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserTabsRequest {
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserConsoleRequest {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserNetworkErrorsRequest {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserCloseRequest {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "tool", content = "input", deny_unknown_fields)]
pub enum BrowserToolRequest {
    #[serde(rename = "open")]
    Open(BrowserOpenRequest),
    #[serde(rename = "navigate")]
    Navigate(BrowserNavigateRequest),
    #[serde(rename = "snapshot")]
    Snapshot(BrowserSnapshotRequest),
    #[serde(rename = "screenshot")]
    Screenshot(BrowserScreenshotRequest),
    #[serde(rename = "click")]
    Click(BrowserClickRequest),
    #[serde(rename = "type")]
    Type(BrowserTypeRequest),
    #[serde(rename = "select")]
    Select(BrowserSelectRequest),
    #[serde(rename = "press")]
    Press(BrowserPressRequest),
    #[serde(rename = "scroll")]
    Scroll(BrowserScrollRequest),
    #[serde(rename = "wait")]
    Wait(BrowserWaitRequest),
    #[serde(rename = "tabs")]
    Tabs(BrowserTabsRequest),
    #[serde(rename = "console")]
    Console(BrowserConsoleRequest),
    #[serde(rename = "network-errors")]
    NetworkErrors(BrowserNetworkErrorsRequest),
    #[serde(rename = "close")]
    Close(BrowserCloseRequest),
}

impl BrowserToolRequest {
    pub fn from_input(name: BrowserToolName, input: serde_json::Value) -> serde_json::Result<Self> {
        let request = match name {
            BrowserToolName::Open => decode(input).map(Self::Open),
            BrowserToolName::Navigate => decode(input).map(Self::Navigate),
            BrowserToolName::Snapshot => decode(input).map(Self::Snapshot),
            BrowserToolName::Screenshot => decode(input).map(Self::Screenshot),
            BrowserToolName::Click => decode(input).map(Self::Click),
            BrowserToolName::Type => decode(input).map(Self::Type),
            BrowserToolName::Select => decode(input).map(Self::Select),
            BrowserToolName::Press => decode(input).map(Self::Press),
            BrowserToolName::Scroll => decode(input).map(Self::Scroll),
            BrowserToolName::Wait => decode(input).map(Self::Wait),
            BrowserToolName::Tabs => decode(input).map(Self::Tabs),
            BrowserToolName::Console => decode(input).map(Self::Console),
            BrowserToolName::NetworkErrors => decode(input).map(Self::NetworkErrors),
            BrowserToolName::Close => decode(input).map(Self::Close),
        }?;
        request.validate().map_err(serde_json::Error::custom)?;
        Ok(request)
    }

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

    fn validate(&self) -> Result<(), String> {
        match self {
            Self::Open(request) => validate_optional_target(request.target.as_ref()),
            Self::Navigate(request) => {
                validate_session_tab(&request.session_id, request.tab_id.as_deref())?;
                validate_navigation_target(&request.target)
            }
            Self::Snapshot(request) => {
                validate_session_tab(&request.session_id, request.tab_id.as_deref())
            }
            Self::Screenshot(request) => {
                validate_session_tab(&request.session_id, request.tab_id.as_deref())
            }
            Self::Click(request) => validate_action_target(
                &request.session_id,
                request.tab_id.as_deref(),
                &request.target,
            ),
            Self::Type(request) => {
                validate_action_target(
                    &request.session_id,
                    request.tab_id.as_deref(),
                    &request.target,
                )?;
                validate_no_nul("text", &request.text)
            }
            Self::Select(request) => validate_select(request),
            Self::Press(request) => validate_press(request),
            Self::Scroll(request) => validate_scroll(request),
            Self::Wait(request) => validate_wait(request),
            Self::Tabs(request) => validate_nonempty("session_id", &request.session_id),
            Self::Console(request) => {
                validate_session_tab(&request.session_id, request.tab_id.as_deref())
            }
            Self::NetworkErrors(request) => {
                validate_session_tab(&request.session_id, request.tab_id.as_deref())
            }
            Self::Close(request) => {
                validate_session_tab(&request.session_id, request.tab_id.as_deref())
            }
        }
    }
}

fn validate_optional_target(target: Option<&BrowserNavigationTarget>) -> Result<(), String> {
    target.map_or(Ok(()), validate_navigation_target)
}

fn validate_navigation_target(target: &BrowserNavigationTarget) -> Result<(), String> {
    let BrowserNavigationTarget::Url { url } = target else {
        return Ok(());
    };
    let parsed = Url::parse(url).map_err(|error| format!("invalid browser URL: {error}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(
            "browser URLs must use http or https; use workspace_file for local files".into(),
        );
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("browser URLs cannot contain credentials".to_string());
    }
    if parsed.host_str().is_none() {
        return Err("browser URL is missing a host".to_string());
    }
    Ok(())
}

fn validate_session_tab(session_id: &str, tab_id: Option<&str>) -> Result<(), String> {
    validate_nonempty("session_id", session_id)?;
    if let Some(tab_id) = tab_id {
        validate_nonempty("tab_id", tab_id)?;
    }
    Ok(())
}

fn validate_action_target(
    session_id: &str,
    tab_id: Option<&str>,
    target: &BrowserElementTarget,
) -> Result<(), String> {
    validate_session_tab(session_id, tab_id)?;
    validate_element_target(target)
}

fn validate_element_target(target: &BrowserElementTarget) -> Result<(), String> {
    match target {
        BrowserElementTarget::Css { selector } => validate_nonempty("selector", selector),
        BrowserElementTarget::Text { text, .. } => validate_nonempty("text", text),
        BrowserElementTarget::SnapshotRef { reference } => {
            validate_nonempty("reference", reference)
        }
    }
}

fn validate_select(request: &BrowserSelectRequest) -> Result<(), String> {
    validate_action_target(
        &request.session_id,
        request.tab_id.as_deref(),
        &request.target,
    )?;
    if request.values.is_empty() {
        return Err("values must contain at least one selection".to_string());
    }
    request
        .values
        .iter()
        .try_for_each(|value| validate_no_nul("value", value))
}

fn validate_press(request: &BrowserPressRequest) -> Result<(), String> {
    validate_session_tab(&request.session_id, request.tab_id.as_deref())?;
    if let Some(target) = request.target.as_ref() {
        validate_element_target(target)?;
    }
    validate_nonempty("key", &request.key)
}

fn validate_scroll(request: &BrowserScrollRequest) -> Result<(), String> {
    validate_session_tab(&request.session_id, request.tab_id.as_deref())?;
    if let Some(target) = request.target.as_ref() {
        validate_element_target(target)?;
    }
    if request.delta_x == 0 && request.delta_y == 0 {
        return Err("scroll delta must not be zero on both axes".to_string());
    }
    Ok(())
}

fn validate_wait(request: &BrowserWaitRequest) -> Result<(), String> {
    validate_session_tab(&request.session_id, request.tab_id.as_deref())?;
    match &request.condition {
        BrowserWaitCondition::Selector { selector } => validate_nonempty("selector", selector),
        BrowserWaitCondition::Text { text, .. } => validate_nonempty("text", text),
        BrowserWaitCondition::Url { url } => validate_nonempty("url", url),
        BrowserWaitCondition::LoadState { .. } => Ok(()),
    }
}

fn validate_nonempty(label: &str, value: &str) -> Result<(), String> {
    if value.trim().is_empty() || value.chars().any(char::is_control) {
        Err(format!("{label} must contain non-control text"))
    } else {
        Ok(())
    }
}

fn validate_no_nul(label: &str, value: &str) -> Result<(), String> {
    if value.contains('\0') {
        Err(format!("{label} must not contain a null character"))
    } else {
        Ok(())
    }
}

fn decode<T: for<'de> Deserialize<'de>>(input: serde_json::Value) -> serde_json::Result<T> {
    serde_json::from_value(input)
}
