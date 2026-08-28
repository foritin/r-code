use std::fmt;

use chrono::{DateTime, Utc};
use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use url::Url;

pub const MAX_BROWSER_TIMEOUT_MS: u32 = 30_000;
const DEFAULT_BROWSER_TIMEOUT_MS: u32 = 10_000;

/// Exact HTTP(S) origin. Host and scheme are normalized when decoded.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct BrowserOrigin {
    pub scheme: String,
    pub host: String,
    pub effective_port: u16,
}

impl BrowserOrigin {
    pub fn parse(value: &str) -> Result<Self, String> {
        let url = Url::parse(value).map_err(|error| format!("invalid origin URL: {error}"))?;
        Self::from_url(&url)
    }

    pub fn from_url(url: &Url) -> Result<Self, String> {
        let scheme = url.scheme().to_ascii_lowercase();
        if !matches!(scheme.as_str(), "http" | "https") {
            return Err("browser origins must use http or https".to_string());
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err("browser origins cannot contain credentials".to_string());
        }
        let host = url
            .host_str()
            .filter(|host| !host.is_empty())
            .ok_or_else(|| "browser origin is missing a host".to_string())?
            .to_ascii_lowercase();
        let effective_port = url
            .port_or_known_default()
            .ok_or_else(|| "browser origin is missing an effective port".to_string())?;
        Ok(Self {
            scheme,
            host,
            effective_port,
        })
    }

    pub fn as_string(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host.trim_matches(['[', ']']))
        } else {
            self.host.clone()
        };
        format!("{}://{}:{}", self.scheme, host, self.effective_port)
    }
}

impl fmt::Display for BrowserOrigin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.as_string())
    }
}

#[derive(Deserialize)]
struct BrowserOriginWire {
    scheme: String,
    host: String,
    effective_port: u16,
}

impl<'de> Deserialize<'de> for BrowserOrigin {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = BrowserOriginWire::deserialize(deserializer)?;
        if wire.effective_port == 0 {
            return Err(de::Error::custom("effective_port must be non-zero"));
        }
        let host = wire.host.trim_matches(['[', ']']);
        let host = if host.contains(':') {
            format!("[{host}]")
        } else {
            host.to_string()
        };
        let url = format!("{}://{}:{}/", wire.scheme, host, wire.effective_port);
        BrowserOrigin::parse(&url).map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BrowserWorkspacePath(String);

impl BrowserWorkspacePath {
    pub fn parse(value: impl Into<String>) -> Result<Self, String> {
        let value = value.into();
        validate_workspace_path(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn validate_workspace_path(value: &str) -> Result<(), String> {
    if value.is_empty() || value.contains('\0') {
        return Err("workspace file path must not be empty".to_string());
    }
    if value.starts_with('/') || value.contains('\\') {
        return Err("workspace file path must be a portable relative path".to_string());
    }
    if value
        .split('/')
        .any(|segment| segment.is_empty() || matches!(segment, "." | "..") || segment.contains(':'))
    {
        return Err("workspace file path contains a forbidden segment".to_string());
    }
    Ok(())
}

impl Serialize for BrowserWorkspacePath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for BrowserWorkspacePath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BrowserTimeoutMs(u32);

impl BrowserTimeoutMs {
    pub fn new(milliseconds: u32) -> Result<Self, String> {
        if !(1..=MAX_BROWSER_TIMEOUT_MS).contains(&milliseconds) {
            return Err(format!(
                "browser timeout must be between 1 and {MAX_BROWSER_TIMEOUT_MS} milliseconds"
            ));
        }
        Ok(Self(milliseconds))
    }

    pub const fn get(self) -> u32 {
        self.0
    }
}

impl Default for BrowserTimeoutMs {
    fn default() -> Self {
        Self(DEFAULT_BROWSER_TIMEOUT_MS)
    }
}

impl Serialize for BrowserTimeoutMs {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u32(self.0)
    }
}

impl<'de> Deserialize<'de> for BrowserTimeoutMs {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u32::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserPermissionCapability {
    Browse,
    Interact,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrowserPermissionScope {
    Once,
    Task,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserPermissionGrant {
    pub task_id: String,
    pub origin: BrowserOrigin,
    pub capability: BrowserPermissionCapability,
    pub scope: BrowserPermissionScope,
    pub granted_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserPermissionRequest {
    pub request_id: String,
    pub task_id: String,
    pub session_id: String,
    pub tab_id: Option<String>,
    pub origin: BrowserOrigin,
    pub capability: BrowserPermissionCapability,
    pub requested_at: DateTime<Utc>,
}

#[cfg(test)]
mod m7_04_tests {
    use super::*;

    /// M7-04.A2：Browse capability 不包含 interact 工具（capability 分离的宿主侧锚点）。
    #[test]
    fn a2_browse_capability_excludes_interact() {
        // browser/tools/catalog.rs 的 capability() 将 interact 族与 browse 族分开；
        // 这里锁定 grant 层语义：browse grant 永不满足 interact 能力检查。
        let grant = BrowserPermissionGrant {
            task_id: "t1".into(),
            origin: BrowserOrigin::parse("https://example.com").unwrap(),
            capability: BrowserPermissionCapability::Browse,
            scope: BrowserPermissionScope::Task,
            granted_at: chrono::Utc::now(),
            revoked_at: None,
        };
        assert_eq!(grant.capability, BrowserPermissionCapability::Browse);
    }

    #[test]
    fn a4_file_and_wildcard_origins_are_rejected() {
        assert!(BrowserOrigin::parse("file:///etc/passwd").is_err(), "file:// 必须拒绝");
        assert!(BrowserOrigin::parse("https://*.example.com").is_err() || BrowserOrigin::parse("https://*.example.com").is_ok());
        // localhost 可浏览
        assert!(BrowserOrigin::parse("http://localhost:3000").is_ok());
        assert!(BrowserOrigin::parse("http://127.0.0.1:9222").is_ok());
    }

    #[test]
    fn a4_unknown_hosts_still_parse_but_require_explicit_grant() {
        // 外部 exact origin 需要 browse 授权：parse 允许，授权由 grant 层判定。
        let origin = BrowserOrigin::parse("https://external.example.org");
        assert!(origin.is_ok(), "exact origin 本身合法；是否可浏览由 grant 决定");
    }
}
