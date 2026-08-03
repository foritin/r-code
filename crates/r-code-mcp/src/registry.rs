use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use url::Url;

use crate::{McpConfigError, McpInstallPlan, McpServerSource, McpTransportConfig, SecretRef};

pub const OFFICIAL_REGISTRY_ENDPOINT: &str =
    "https://registry.modelcontextprotocol.io/v0.1/servers";
const CACHE_TTL: Duration = Duration::from_secs(60 * 60);
const MAX_CACHE_ENTRIES: usize = 64;
const MAX_PAGE_SIZE: usize = 50;

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("invalid Registry request: {0}")]
    InvalidRequest(String),
    #[error("Registry request failed: {0}")]
    Request(String),
    #[error("Registry returned an invalid response: {0}")]
    InvalidResponse(String),
    #[error("Registry cache failed: {0}")]
    Cache(String),
    #[error(transparent)]
    Config(#[from] McpConfigError),
    #[error("missing credential reference for {0}")]
    MissingCredentialReference(String),
}

#[async_trait]
pub trait RegistryHttpAdapter: Send + Sync + 'static {
    async fn get_json(&self, url: Url) -> Result<Value, RegistryError>;
}

pub struct ReqwestRegistryHttpAdapter {
    client: reqwest::Client,
}

impl ReqwestRegistryHttpAdapter {
    pub fn new() -> Result<Self, RegistryError> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::limited(3))
            .user_agent("R-Code MCP Registry client")
            .build()
            .map_err(|error| RegistryError::Request(error.to_string()))?;
        Ok(Self { client })
    }
}

#[async_trait]
impl RegistryHttpAdapter for ReqwestRegistryHttpAdapter {
    async fn get_json(&self, url: Url) -> Result<Value, RegistryError> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|error| RegistryError::Request(error.to_string()))?
            .error_for_status()
            .map_err(|error| RegistryError::Request(error.to_string()))?;
        response
            .json()
            .await
            .map_err(|error| RegistryError::InvalidResponse(error.to_string()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketPackageKind {
    Npm,
    Pypi,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketEnvironmentVariable {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub secret: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum MarketInstallTransport {
    Stdio {
        package_kind: MarketPackageKind,
        executable: String,
        args: Vec<String>,
        #[serde(default)]
        environment: Vec<MarketEnvironmentVariable>,
    },
    StreamableHttp {
        url: String,
        #[serde(default)]
        headers: Vec<MarketEnvironmentVariable>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketInstallOption {
    pub id: String,
    pub label: String,
    pub transport: MarketInstallTransport,
}

impl MarketInstallOption {
    pub fn to_install_plan(
        &self,
        server: &MarketServer,
        server_id: &str,
        credential_references: &BTreeMap<String, SecretRef>,
    ) -> Result<McpInstallPlan, RegistryError> {
        let (transport, required_secret_names) = match &self.transport {
            MarketInstallTransport::Stdio {
                executable,
                args,
                environment,
                ..
            } => {
                let env = credential_map(environment, credential_references)?;
                let required = environment
                    .iter()
                    .filter(|item| item.required)
                    .map(|item| item.name.clone())
                    .collect();
                (
                    McpTransportConfig::Stdio {
                        command: executable.clone(),
                        args: args.clone(),
                        env,
                    },
                    required,
                )
            }
            MarketInstallTransport::StreamableHttp { url, headers } => {
                let headers_map = credential_map(headers, credential_references)?;
                let required = headers
                    .iter()
                    .filter(|item| item.required)
                    .map(|item| item.name.clone())
                    .collect();
                (
                    McpTransportConfig::StreamableHttp {
                        url: url.clone(),
                        headers: headers_map,
                    },
                    required,
                )
            }
        };
        let plan = McpInstallPlan {
            server_id: server_id.to_string(),
            display_name: server.title.clone(),
            description: server.description.clone(),
            source: McpServerSource::Registry {
                registry_url: OFFICIAL_REGISTRY_ENDPOINT.to_string(),
                name: server.name.clone(),
                version: server.version.clone(),
                repository_url: server.repository_url.clone(),
            },
            transport,
            required_secret_names,
        };
        plan.validate()?;
        Ok(plan)
    }
}

fn credential_map(
    variables: &[MarketEnvironmentVariable],
    credential_references: &BTreeMap<String, SecretRef>,
) -> Result<BTreeMap<String, SecretRef>, RegistryError> {
    variables
        .iter()
        .map(|variable| {
            credential_references
                .get(&variable.name)
                .cloned()
                .map(|reference| (variable.name.clone(), reference))
                .ok_or_else(|| RegistryError::MissingCredentialReference(variable.name.clone()))
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketServer {
    pub name: String,
    pub title: String,
    pub description: String,
    pub version: String,
    pub status: String,
    pub is_latest: bool,
    pub suggested_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_url: Option<String>,
    #[serde(default)]
    pub install_options: Vec<MarketInstallOption>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketPage {
    pub servers: Vec<MarketServer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub stale: bool,
    pub fetched_at: DateTime<Utc>,
    /// The official Registry is still a preview and does not review listed servers.
    pub registry_preview: bool,
    pub registry_unreviewed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct RegistryCacheFile {
    #[serde(default)]
    entries: BTreeMap<String, CachedPage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedPage {
    fetched_at_epoch: i64,
    page: MarketPage,
}

pub struct RegistryClient {
    endpoint: Url,
    cache_path: PathBuf,
    http: Arc<dyn RegistryHttpAdapter>,
    cache_ttl: Duration,
}

impl RegistryClient {
    pub fn official(cache_path: PathBuf) -> Result<Self, RegistryError> {
        Ok(Self::new(
            Url::parse(OFFICIAL_REGISTRY_ENDPOINT)
                .map_err(|error| RegistryError::InvalidRequest(error.to_string()))?,
            cache_path,
            Arc::new(ReqwestRegistryHttpAdapter::new()?),
        ))
    }

    pub fn new(endpoint: Url, cache_path: PathBuf, http: Arc<dyn RegistryHttpAdapter>) -> Self {
        Self {
            endpoint,
            cache_path,
            http,
            cache_ttl: CACHE_TTL,
        }
    }

    #[cfg(test)]
    fn with_cache_ttl(mut self, ttl: Duration) -> Self {
        self.cache_ttl = ttl;
        self
    }

    pub async fn search(
        &self,
        query: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<MarketPage, RegistryError> {
        self.search_at(query, cursor, limit, Utc::now()).await
    }

    async fn search_at(
        &self,
        query: Option<&str>,
        cursor: Option<&str>,
        limit: usize,
        now: DateTime<Utc>,
    ) -> Result<MarketPage, RegistryError> {
        let limit = limit.clamp(1, MAX_PAGE_SIZE);
        let key = cache_key(query, cursor, limit);
        let cache = read_cache(&self.cache_path).unwrap_or_default();
        if let Some(cached) = cache.entries.get(&key) {
            let age = now.timestamp().saturating_sub(cached.fetched_at_epoch);
            if age >= 0 && age < self.cache_ttl.as_secs() as i64 {
                let mut page = cached.page.clone();
                page.stale = false;
                return Ok(page);
            }
        }

        let mut url = self.endpoint.clone();
        {
            let mut pairs = url.query_pairs_mut();
            if let Some(query) = query.filter(|value| !value.trim().is_empty()) {
                pairs.append_pair("search", query.trim());
            }
            if let Some(cursor) = cursor.filter(|value| !value.trim().is_empty()) {
                pairs.append_pair("cursor", cursor);
            }
            pairs.append_pair("limit", &limit.to_string());
        }
        match self
            .http
            .get_json(url)
            .await
            .and_then(|json| normalize_page(json, now))
        {
            Ok(page) => {
                let mut updated = cache;
                updated.entries.insert(
                    key,
                    CachedPage {
                        fetched_at_epoch: now.timestamp(),
                        page: page.clone(),
                    },
                );
                while updated.entries.len() > MAX_CACHE_ENTRIES {
                    let oldest = updated
                        .entries
                        .iter()
                        .min_by_key(|(_, entry)| entry.fetched_at_epoch)
                        .map(|(key, _)| key.clone());
                    if let Some(oldest) = oldest {
                        updated.entries.remove(&oldest);
                    }
                }
                let _ = write_cache(&self.cache_path, &updated);
                Ok(page)
            }
            Err(error) => {
                if let Some(cached) = cache.entries.get(&key) {
                    let mut page = cached.page.clone();
                    page.stale = true;
                    return Ok(page);
                }
                Err(error)
            }
        }
    }
}

fn cache_key(query: Option<&str>, cursor: Option<&str>, limit: usize) -> String {
    format!(
        "{}\0{}\0{limit}",
        query.unwrap_or_default().trim().to_ascii_lowercase(),
        cursor.unwrap_or_default()
    )
}

fn read_cache(path: &Path) -> Result<RegistryCacheFile, RegistryError> {
    if !path.exists() {
        return Ok(RegistryCacheFile::default());
    }
    let bytes = std::fs::read(path).map_err(|error| RegistryError::Cache(error.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|error| RegistryError::Cache(error.to_string()))
}

fn write_cache(path: &Path, cache: &RegistryCacheFile) -> Result<(), RegistryError> {
    let parent = path
        .parent()
        .ok_or_else(|| RegistryError::Cache("cache path has no parent".to_string()))?;
    std::fs::create_dir_all(parent).map_err(|error| RegistryError::Cache(error.to_string()))?;
    let bytes =
        serde_json::to_vec(cache).map_err(|error| RegistryError::Cache(error.to_string()))?;
    std::fs::write(path, bytes).map_err(|error| RegistryError::Cache(error.to_string()))
}

fn normalize_page(json: Value, now: DateTime<Utc>) -> Result<MarketPage, RegistryError> {
    let records = json
        .get("servers")
        .and_then(Value::as_array)
        .ok_or_else(|| RegistryError::InvalidResponse("missing servers array".to_string()))?;
    let mut seen = HashSet::new();
    let mut servers = Vec::new();
    for record in records {
        if let Some(server) = normalize_server(record) {
            let identity = format!("{}\0{}", server.name, server.version);
            if seen.insert(identity) {
                servers.push(server);
            }
        }
    }
    let next_cursor = json
        .pointer("/metadata/nextCursor")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    Ok(MarketPage {
        servers,
        next_cursor,
        stale: false,
        fetched_at: now,
        registry_preview: true,
        registry_unreviewed: true,
    })
}

fn normalize_server(record: &Value) -> Option<MarketServer> {
    let raw = record.get("server")?;
    let name = raw.get("name")?.as_str()?.trim();
    let version = raw.get("version")?.as_str()?.trim();
    if name.is_empty() || version.is_empty() {
        return None;
    }
    let official = record.pointer("/_meta/io.modelcontextprotocol.registry~1official");
    let status = official
        .and_then(|value| value.get("status"))
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_ascii_lowercase();
    if matches!(status.as_str(), "deleted" | "deprecated") {
        return None;
    }
    let is_latest = official
        .and_then(|value| value.get("isLatest"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let title = raw
        .get("title")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(name)
        .to_string();
    let description = raw
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let repository_url = raw
        .pointer("/repository/url")
        .and_then(Value::as_str)
        .map(str::to_string);
    let suggested_id = suggested_server_id(name);
    let mut install_options = Vec::new();
    if let Some(packages) = raw.get("packages").and_then(Value::as_array) {
        for (index, package) in packages.iter().enumerate() {
            if let Some(option) = normalize_package(package, index) {
                install_options.push(option);
            }
        }
    }
    if let Some(remotes) = raw.get("remotes").and_then(Value::as_array) {
        for (index, remote) in remotes.iter().enumerate() {
            if let Some(option) = normalize_remote(remote, index) {
                install_options.push(option);
            }
        }
    }
    Some(MarketServer {
        name: name.to_string(),
        title,
        description,
        version: version.to_string(),
        status,
        is_latest,
        suggested_id,
        repository_url,
        install_options,
    })
}

fn normalize_package(package: &Value, index: usize) -> Option<MarketInstallOption> {
    let registry_type = package.get("registryType")?.as_str()?;
    let identifier = package.get("identifier")?.as_str()?.trim();
    if identifier.is_empty() {
        return None;
    }
    let version = package
        .get("version")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    let (package_kind, default_executable, package_spec) = match registry_type {
        "npm" => (
            MarketPackageKind::Npm,
            "npx",
            version
                .map(|version| format!("{identifier}@{version}"))
                .unwrap_or_else(|| identifier.to_string()),
        ),
        "pypi" => (
            MarketPackageKind::Pypi,
            "uvx",
            version
                .map(|version| format!("{identifier}=={version}"))
                .unwrap_or_else(|| identifier.to_string()),
        ),
        _ => return None,
    };
    let executable = package
        .get("runtimeHint")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(default_executable)
        .to_string();
    let mut args = argument_values(package.get("runtimeArguments"));
    if registry_type == "npm" && executable == "npx" && args.is_empty() {
        args.push("-y".to_string());
    }
    args.push(package_spec);
    args.extend(argument_values(package.get("packageArguments")));
    let environment = normalize_variables(package.get("environmentVariables"));
    Some(MarketInstallOption {
        id: format!("package-{registry_type}-{index}"),
        label: format!("{} · {}", registry_type.to_ascii_uppercase(), identifier),
        transport: MarketInstallTransport::Stdio {
            package_kind,
            executable,
            args,
            environment,
        },
    })
}

fn normalize_remote(remote: &Value, index: usize) -> Option<MarketInstallOption> {
    if remote.get("type")?.as_str()? != "streamable-http" {
        return None;
    }
    let url = remote.get("url")?.as_str()?.trim();
    let parsed = Url::parse(url).ok()?;
    if parsed.scheme() != "https" || !parsed.username().is_empty() || parsed.password().is_some() {
        return None;
    }
    Some(MarketInstallOption {
        id: format!("remote-{index}"),
        label: parsed
            .host_str()
            .map(|host| format!("Remote · {host}"))
            .unwrap_or_else(|| "Remote".to_string()),
        transport: MarketInstallTransport::StreamableHttp {
            url: url.to_string(),
            headers: normalize_variables(remote.get("headers")),
        },
    })
}

fn normalize_variables(value: Option<&Value>) -> Vec<MarketEnvironmentVariable> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let name = item.get("name")?.as_str()?.trim();
            if name.is_empty() || name.chars().any(char::is_control) {
                return None;
            }
            Some(MarketEnvironmentVariable {
                name: name.to_string(),
                description: item
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                required: item
                    .get("isRequired")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                secret: item
                    .get("isSecret")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                default_value: item
                    .get("default")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        })
        .collect()
}

fn argument_values(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|argument| match argument {
            Value::String(value) => Some(value.clone()),
            Value::Object(object) => object
                .get("value")
                .and_then(Value::as_str)
                .map(str::to_string),
            _ => None,
        })
        .filter(|value| !value.chars().any(char::is_control))
        .collect()
}

fn suggested_server_id(name: &str) -> String {
    let mut id = String::with_capacity(name.len());
    let mut previous_dash = false;
    for byte in name.bytes() {
        let lowered = byte.to_ascii_lowercase();
        if lowered.is_ascii_lowercase() || lowered.is_ascii_digit() {
            id.push(lowered as char);
            previous_dash = false;
        } else if !previous_dash && !id.is_empty() {
            id.push('-');
            previous_dash = true;
        }
    }
    while id.ends_with('-') {
        id.pop();
    }
    if id
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_digit())
    {
        id.insert_str(0, "mcp-");
    }
    if id.is_empty() {
        id.push_str("mcp-server");
    }
    if id.len() > 55 {
        id.truncate(55);
        while id.ends_with('-') {
            id.pop();
        }
    }
    let digest = blake3::hash(name.as_bytes()).to_hex();
    let suffix = &digest.as_str()[..8];
    format!("{id}-{suffix}")
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Mutex};

    use chrono::TimeZone;

    use super::*;

    struct FakeHttp {
        responses: Mutex<VecDeque<Result<Value, String>>>,
    }

    #[async_trait]
    impl RegistryHttpAdapter for FakeHttp {
        async fn get_json(&self, _url: Url) -> Result<Value, RegistryError> {
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| Err("no response".to_string()))
                .map_err(RegistryError::Request)
        }
    }

    fn fixture() -> Value {
        serde_json::json!({
            "servers": [
                {
                    "server": {
                        "name": "io.example/files",
                        "title": "Example Files",
                        "description": "Example",
                        "version": "1.2.3",
                        "repository": {"url": "https://github.com/example/files"},
                        "packages": [{
                            "registryType": "npm",
                            "identifier": "@example/files",
                            "version": "1.2.3",
                            "transport": {"type": "stdio"},
                            "runtimeArguments": [{"value": "--yes"}],
                            "environmentVariables": [{"name": "TOKEN", "isRequired": true, "isSecret": true}]
                        }],
                        "remotes": [{"type": "streamable-http", "url": "https://example.com/mcp"}]
                    },
                    "_meta": {"io.modelcontextprotocol.registry/official": {"status": "active", "isLatest": true}}
                },
                {
                    "server": {"name": "gone", "version": "1", "packages": []},
                    "_meta": {"io.modelcontextprotocol.registry/official": {"status": "deleted", "isLatest": true}}
                },
                {"server": {"description": "malformed"}}
            ],
            "metadata": {"nextCursor": "next"}
        })
    }

    #[test]
    fn normalizes_supported_records_and_filters_deleted_or_malformed() {
        let page = normalize_page(fixture(), Utc::now()).unwrap();
        assert_eq!(page.servers.len(), 1);
        assert_eq!(page.next_cursor.as_deref(), Some("next"));
        let server = &page.servers[0];
        assert_eq!(server.install_options.len(), 2);
        assert!(server.is_latest);
        match &server.install_options[0].transport {
            MarketInstallTransport::Stdio {
                executable,
                args,
                environment,
                ..
            } => {
                assert_eq!(executable, "npx");
                assert_eq!(args, &["--yes", "@example/files@1.2.3"]);
                assert_eq!(environment[0].name, "TOKEN");
            }
            _ => panic!("expected stdio"),
        }
    }

    #[test]
    fn install_plan_is_disabled_and_preserves_exact_launch_shape() {
        let page = normalize_page(fixture(), Utc::now()).unwrap();
        let server = &page.servers[0];
        let refs = BTreeMap::from([(
            "TOKEN".to_string(),
            SecretRef::new("mcp/example/env/TOKEN").unwrap(),
        )]);
        let plan = server.install_options[0]
            .to_install_plan(server, "example-files", &refs)
            .unwrap();
        let config = plan.into_disabled_config().unwrap();
        assert!(!config.enabled);
        assert!(config.approved_launch_fingerprint.is_none());
        match config.transport {
            McpTransportConfig::Stdio { command, args, .. } => {
                assert_eq!(command, "npx");
                assert_eq!(args, vec!["--yes", "@example/files@1.2.3"]);
            }
            _ => panic!("expected stdio"),
        }
    }

    #[tokio::test]
    async fn fresh_cache_then_stale_fallback_avoids_losing_market() {
        let temp = tempfile::tempdir().unwrap();
        let now = Utc.timestamp_opt(1_800_000_000, 0).unwrap();
        let http = Arc::new(FakeHttp {
            responses: Mutex::new(VecDeque::from([Ok(fixture()), Err("offline".to_string())])),
        });
        let client = RegistryClient::new(
            Url::parse(OFFICIAL_REGISTRY_ENDPOINT).unwrap(),
            temp.path().join("registry-cache.json"),
            http,
        )
        .with_cache_ttl(Duration::from_secs(1));
        let first = client
            .search_at(Some("files"), None, 20, now)
            .await
            .unwrap();
        assert!(!first.stale);
        let fresh = client
            .search_at(Some("files"), None, 20, now)
            .await
            .unwrap();
        assert!(!fresh.stale);
        let stale = client
            .search_at(Some("files"), None, 20, now + chrono::Duration::seconds(2))
            .await
            .unwrap();
        assert!(stale.stale);
        assert_eq!(stale.servers.len(), 1);
    }

    #[test]
    fn malformed_root_is_rejected() {
        assert!(normalize_page(serde_json::json!({}), Utc::now()).is_err());
    }
}
