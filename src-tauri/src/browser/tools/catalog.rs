use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::browser::BrowserPermissionCapability;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BrowserToolName {
    #[serde(rename = "open")]
    Open,
    #[serde(rename = "navigate")]
    Navigate,
    #[serde(rename = "snapshot")]
    Snapshot,
    #[serde(rename = "screenshot")]
    Screenshot,
    #[serde(rename = "click")]
    Click,
    #[serde(rename = "type")]
    Type,
    #[serde(rename = "select")]
    Select,
    #[serde(rename = "press")]
    Press,
    #[serde(rename = "scroll")]
    Scroll,
    #[serde(rename = "wait")]
    Wait,
    #[serde(rename = "tabs")]
    Tabs,
    #[serde(rename = "console")]
    Console,
    #[serde(rename = "network-errors")]
    NetworkErrors,
    #[serde(rename = "close")]
    Close,
}

impl BrowserToolName {
    pub const ALL: [Self; 14] = [
        Self::Open,
        Self::Navigate,
        Self::Snapshot,
        Self::Screenshot,
        Self::Click,
        Self::Type,
        Self::Select,
        Self::Press,
        Self::Scroll,
        Self::Wait,
        Self::Tabs,
        Self::Console,
        Self::NetworkErrors,
        Self::Close,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Navigate => "navigate",
            Self::Snapshot => "snapshot",
            Self::Screenshot => "screenshot",
            Self::Click => "click",
            Self::Type => "type",
            Self::Select => "select",
            Self::Press => "press",
            Self::Scroll => "scroll",
            Self::Wait => "wait",
            Self::Tabs => "tabs",
            Self::Console => "console",
            Self::NetworkErrors => "network-errors",
            Self::Close => "close",
        }
    }

    pub const fn capability(self) -> BrowserPermissionCapability {
        match self {
            Self::Open
            | Self::Navigate
            | Self::Snapshot
            | Self::Screenshot
            | Self::Tabs
            | Self::Console
            | Self::NetworkErrors
            | Self::Close => BrowserPermissionCapability::Browse,
            Self::Click | Self::Type | Self::Select | Self::Press | Self::Scroll | Self::Wait => {
                BrowserPermissionCapability::Interact
            }
        }
    }

    pub const fn is_read_only(self) -> bool {
        matches!(self.capability(), BrowserPermissionCapability::Browse)
    }
}

impl FromStr for BrowserToolName {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|name| name.as_str() == value)
            .ok_or(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrowserToolContract {
    pub name: BrowserToolName,
    pub description: String,
    pub capability: BrowserPermissionCapability,
    pub input_schema: Value,
}

impl BrowserToolContract {
    pub fn for_name(name: BrowserToolName) -> Self {
        Self {
            name,
            description: description(name).to_string(),
            capability: name.capability(),
            input_schema: input_schema(name),
        }
    }
}

pub fn browser_tool_contracts() -> Vec<BrowserToolContract> {
    BrowserToolName::ALL
        .into_iter()
        .map(BrowserToolContract::for_name)
        .collect()
}

fn description(name: BrowserToolName) -> &'static str {
    match name {
        BrowserToolName::Open => "Open the task-owned visible browser session.",
        BrowserToolName::Navigate => {
            "Navigate a task-owned tab to an allowed URL or workspace file."
        }
        BrowserToolName::Snapshot => "Read a sanitized accessibility snapshot of the active page.",
        BrowserToolName::Screenshot => "Capture the active page to task-owned local storage.",
        BrowserToolName::Click => "Click one element in a permitted page origin.",
        BrowserToolName::Type => "Type text into one element in a permitted page origin.",
        BrowserToolName::Select => "Select one or more values in a permitted page origin.",
        BrowserToolName::Press => "Press a key in a permitted page origin.",
        BrowserToolName::Scroll => "Scroll a permitted page or element.",
        BrowserToolName::Wait => "Wait up to 30 seconds for a selector, text, URL, or load state.",
        BrowserToolName::Tabs => "List tabs in the task-owned browser session.",
        BrowserToolName::Console => "Read sanitized console entries from a task-owned tab.",
        BrowserToolName::NetworkErrors => "Read sanitized network failures from a task-owned tab.",
        BrowserToolName::Close => "Close one tab or the complete task-owned browser session.",
    }
}

fn input_schema(name: BrowserToolName) -> Value {
    match name {
        BrowserToolName::Open => object(map([("target", navigation_target_schema())]), &[]),
        BrowserToolName::Navigate => {
            let mut properties = tab_properties();
            properties.insert("target".to_string(), navigation_target_schema());
            object(properties, &["session_id", "target"])
        }
        BrowserToolName::Snapshot => object(tab_properties(), &["session_id"]),
        BrowserToolName::Screenshot => {
            let mut properties = tab_properties();
            properties.insert("full_page".to_string(), json!({ "type": "boolean" }));
            object(properties, &["session_id"])
        }
        BrowserToolName::Click => action_target_schema(&[]),
        BrowserToolName::Type => action_target_schema(&[
            ("text", json!({ "type": "string" })),
            ("clear", json!({ "type": "boolean" })),
        ]),
        BrowserToolName::Select => action_target_schema(&[(
            "values",
            json!({ "type": "array", "items": { "type": "string" }, "minItems": 1 }),
        )]),
        BrowserToolName::Press => press_schema(),
        BrowserToolName::Scroll => scroll_schema(),
        BrowserToolName::Wait => wait_schema(),
        BrowserToolName::Tabs => object(session_properties(), &["session_id"]),
        BrowserToolName::Console | BrowserToolName::NetworkErrors => {
            object(tab_properties(), &["session_id"])
        }
        BrowserToolName::Close => object(tab_properties(), &["session_id"]),
    }
}

fn session_properties() -> Map<String, Value> {
    map([("session_id", json!({ "type": "string", "minLength": 1 }))])
}

fn tab_properties() -> Map<String, Value> {
    let mut properties = session_properties();
    properties.insert(
        "tab_id".to_string(),
        json!({ "type": "string", "minLength": 1 }),
    );
    properties
}

fn action_target_schema(extra: &[(&str, Value)]) -> Value {
    let mut properties = tab_properties();
    properties.insert("target".to_string(), element_target_schema());
    for (name, schema) in extra {
        properties.insert((*name).to_string(), schema.clone());
    }
    let mut required = vec!["session_id", "target"];
    required.extend(
        extra
            .iter()
            .filter_map(|(name, _)| matches!(*name, "text" | "values").then_some(*name)),
    );
    object(properties, &required)
}

fn press_schema() -> Value {
    let mut properties = tab_properties();
    properties.insert("target".to_string(), element_target_schema());
    properties.insert(
        "key".to_string(),
        json!({ "type": "string", "minLength": 1 }),
    );
    object(properties, &["session_id", "key"])
}

fn scroll_schema() -> Value {
    let mut properties = tab_properties();
    properties.insert("target".to_string(), element_target_schema());
    properties.insert("delta_x".to_string(), json!({ "type": "integer" }));
    properties.insert("delta_y".to_string(), json!({ "type": "integer" }));
    object(properties, &["session_id", "delta_x", "delta_y"])
}

fn wait_schema() -> Value {
    let mut properties = tab_properties();
    properties.insert("condition".to_string(), wait_condition_schema());
    properties.insert(
        "timeout_ms".to_string(),
        json!({ "type": "integer", "minimum": 1, "maximum": 30_000, "default": 10_000 }),
    );
    object(properties, &["session_id", "condition"])
}

fn navigation_target_schema() -> Value {
    json!({
        "oneOf": [
            object(map([
                ("kind", json!({ "const": "url" })),
                ("url", json!({ "type": "string", "minLength": 1 }))
            ]), &["kind", "url"]),
            object(map([
                ("kind", json!({ "const": "workspace_file" })),
                ("path", json!({ "type": "string", "minLength": 1 }))
            ]), &["kind", "path"])
        ]
    })
}

fn element_target_schema() -> Value {
    json!({
        "oneOf": [
            object(map([
                ("kind", json!({ "const": "css" })),
                ("selector", json!({ "type": "string", "minLength": 1 }))
            ]), &["kind", "selector"]),
            object(map([
                ("kind", json!({ "const": "text" })),
                ("text", json!({ "type": "string", "minLength": 1 })),
                ("exact", json!({ "type": "boolean" }))
            ]), &["kind", "text"]),
            object(map([
                ("kind", json!({ "const": "snapshot_ref" })),
                ("reference", json!({ "type": "string", "minLength": 1 }))
            ]), &["kind", "reference"])
        ]
    })
}

fn wait_condition_schema() -> Value {
    json!({
        "oneOf": [
            object(map([
                ("kind", json!({ "const": "selector" })),
                ("selector", json!({ "type": "string", "minLength": 1 }))
            ]), &["kind", "selector"]),
            object(map([
                ("kind", json!({ "const": "text" })),
                ("text", json!({ "type": "string", "minLength": 1 })),
                ("exact", json!({ "type": "boolean" }))
            ]), &["kind", "text"]),
            object(map([
                ("kind", json!({ "const": "url" })),
                ("url", json!({ "type": "string", "minLength": 1 }))
            ]), &["kind", "url"]),
            object(map([
                ("kind", json!({ "const": "load_state" })),
                ("state", json!({ "enum": ["dom_content_loaded", "load", "network_idle"] }))
            ]), &["kind", "state"])
        ]
    })
}

fn map<const N: usize>(entries: [(&str, Value); N]) -> Map<String, Value> {
    entries
        .into_iter()
        .map(|(name, value)| (name.to_string(), value))
        .collect()
}

fn object(properties: Map<String, Value>, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false
    })
}
