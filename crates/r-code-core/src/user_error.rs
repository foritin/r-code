use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserFacingError {
    pub code: String,
    #[serde(default)]
    pub args: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debug_detail: Option<String>,
}

impl UserFacingError {
    pub fn new(code: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            args: BTreeMap::new(),
            debug_detail: None,
        }
    }

    pub fn with_arg(mut self, name: impl Into<String>, value: impl Into<Value>) -> Self {
        self.args.insert(name.into(), value.into());
        self
    }

    pub fn with_debug_detail(mut self, detail: impl Into<String>) -> Self {
        self.debug_detail = Some(detail.into());
        self
    }
}

impl fmt::Display for UserFacingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.code)
    }
}

impl std::error::Error for UserFacingError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_stable_code_and_arguments() {
        let error = UserFacingError::new("browser.runtime_hash_mismatch")
            .with_arg("expected", "abc123")
            .with_arg("actual", "def456");

        let value = serde_json::to_value(error).expect("serialize user-facing error");

        assert_eq!(value["code"], "browser.runtime_hash_mismatch");
        assert_eq!(value["args"]["expected"], "abc123");
        assert_eq!(value["args"]["actual"], "def456");
        assert!(value.get("debug_detail").is_none());
    }

    #[test]
    fn technical_detail_is_explicit_and_round_trips() {
        let error = UserFacingError::new("updater.signature_invalid")
            .with_debug_detail("signature verification returned exit status 17");

        let json = serde_json::to_string(&error).expect("serialize user-facing error");
        let decoded: UserFacingError =
            serde_json::from_str(&json).expect("deserialize user-facing error");

        assert_eq!(decoded, error);
        assert_eq!(decoded.to_string(), "updater.signature_invalid");
        assert_eq!(
            serde_json::to_value(decoded).unwrap()["args"],
            serde_json::json!({})
        );
    }

    #[test]
    fn internal_agent_conversion_never_exposes_debug_detail() {
        let product_error = crate::error::ProductError::from(
            UserFacingError::new("browser.origin_permission_required")
                .with_arg("origin", "https://example.com")
                .with_debug_detail("authorization=secret-token"),
        );

        let agent_error = agent_error::Error::from(product_error).to_string();

        assert!(agent_error.contains("browser.origin_permission_required"));
        assert!(!agent_error.contains("secret-token"));
    }
}
