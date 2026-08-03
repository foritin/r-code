use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use hermes_config::{Config, ServerSpec};
use r_code_core::{error::ProductError, secret::SecretStore};
use r_code_mcp::{
    BuiltinMcpServer, McpClientError, McpServerConfig, McpServerSource, McpTransportConfig,
    SecretRef, SecretResolver,
};
use serde::{Deserialize, Serialize};

use crate::security_config::mcp_credential_account;

const SETTINGS_FILE: &str = "mcp-servers.toml";
const REGISTRY_CACHE_FILE: &str = "mcp-registry-cache.json";
const SETTINGS_SCHEMA_VERSION: u32 = 1;
const SECRET_SERVICE: &str = "r-code";
pub const RESEARCH_SERVER_ID: &str = "r-code-research";
type MigratedMcpServer = (McpServerConfig, Vec<(SecretRef, String)>);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpSettingsFile {
    schema_version: u32,
    #[serde(default)]
    legacy_migrated: bool,
    #[serde(default)]
    servers: Vec<McpServerConfig>,
}

impl Default for McpSettingsFile {
    fn default() -> Self {
        Self {
            schema_version: SETTINGS_SCHEMA_VERSION,
            legacy_migrated: false,
            servers: Vec::new(),
        }
    }
}

pub trait McpCredentialBackend: Send + Sync + 'static {
    fn set(&self, account: &str, value: &str) -> Result<(), ProductError>;
    fn get(&self, account: &str) -> Result<Option<String>, ProductError>;
    fn delete(&self, account: &str) -> Result<(), ProductError>;
}

#[derive(Default)]
struct OsCredentialBackend;

impl McpCredentialBackend for OsCredentialBackend {
    fn set(&self, account: &str, value: &str) -> Result<(), ProductError> {
        SecretStore::new(SECRET_SERVICE).store(account, value)
    }

    fn get(&self, account: &str) -> Result<Option<String>, ProductError> {
        SecretStore::new(SECRET_SERVICE).get(account)
    }

    fn delete(&self, account: &str) -> Result<(), ProductError> {
        SecretStore::new(SECRET_SERVICE).delete(account)
    }
}

#[derive(Clone)]
pub struct McpSecretStore {
    backend: Arc<dyn McpCredentialBackend>,
}

impl McpSecretStore {
    fn new(backend: Arc<dyn McpCredentialBackend>) -> Self {
        Self { backend }
    }

    pub fn reference(
        &self,
        server_id: &str,
        kind: &str,
        name: &str,
    ) -> Result<SecretRef, ProductError> {
        let account = mcp_credential_account(server_id, kind, name).ok_or_else(|| {
            ProductError::ConfigError("invalid MCP credential account components".to_string())
        })?;
        SecretRef::new(account).map_err(|error| ProductError::ConfigError(error.to_string()))
    }

    pub fn set(&self, reference: &SecretRef, value: &str) -> Result<(), ProductError> {
        if value.is_empty() {
            self.backend.delete(reference.as_str())
        } else {
            self.backend.set(reference.as_str(), value)
        }
    }

    pub fn delete(&self, reference: &SecretRef) -> Result<(), ProductError> {
        self.backend.delete(reference.as_str())
    }

    pub fn configured(&self, reference: &SecretRef) -> Result<bool, ProductError> {
        Ok(self.backend.get(reference.as_str())?.is_some())
    }
}

#[async_trait]
impl SecretResolver for McpSecretStore {
    async fn resolve(&self, reference: &SecretRef) -> Result<Option<String>, McpClientError> {
        let account = reference.as_str().to_string();
        let backend = self.backend.clone();
        tokio::task::spawn_blocking(move || backend.get(&account))
            .await
            .map_err(|_| McpClientError::Request("credential lookup task failed".to_string()))?
            .map_err(|_| McpClientError::Request("credential store is unavailable".to_string()))
    }
}

pub struct McpSettingsService {
    config_dir: PathBuf,
    secrets: McpSecretStore,
}

impl McpSettingsService {
    pub fn new(config_dir: PathBuf) -> Self {
        Self::with_credentials(config_dir, Arc::new(OsCredentialBackend))
    }

    pub fn with_credentials(
        config_dir: PathBuf,
        credentials: Arc<dyn McpCredentialBackend>,
    ) -> Self {
        Self {
            config_dir,
            secrets: McpSecretStore::new(credentials),
        }
    }

    pub fn path(&self) -> PathBuf {
        self.config_dir.join(SETTINGS_FILE)
    }

    /// Cache is intentionally separate from settings so Registry corruption never blocks startup.
    pub fn registry_cache_path(&self) -> PathBuf {
        self.config_dir.join(REGISTRY_CACHE_FILE)
    }

    pub fn secret_store(&self) -> McpSecretStore {
        self.secrets.clone()
    }

    pub fn load(&self) -> Result<Vec<McpServerConfig>, ProductError> {
        let path = self.path();
        let existed = path.exists();
        let mut file = self.read_file()?;
        let changed = reconcile_builtin(&mut file.servers);
        validate_servers(&file.servers)?;
        if !existed || changed {
            self.write_file(&file)?;
        }
        Ok(file.servers)
    }

    pub fn save(
        &self,
        servers: Vec<McpServerConfig>,
    ) -> Result<Vec<McpServerConfig>, ProductError> {
        let mut file = self.read_file()?;
        file.servers = servers;
        reconcile_builtin(&mut file.servers);
        validate_servers(&file.servers)?;
        self.write_file(&file)?;
        Ok(file.servers)
    }

    pub fn migrate_legacy(&self, config: &Config) -> Result<usize, ProductError> {
        let mut file = self.read_file()?;
        if file.legacy_migrated {
            return Ok(0);
        }
        reconcile_builtin(&mut file.servers);
        let existing = file
            .servers
            .iter()
            .map(|server| server.id.clone())
            .collect::<BTreeSet<_>>();
        let mut migrated = Vec::new();
        for (server_id, server) in &config.mcp_servers {
            if existing.contains(server_id) || server_id == RESEARCH_SERVER_ID {
                continue;
            }
            if let Some((converted, credentials)) = self.convert_legacy(server_id, server)? {
                for (reference, value) in credentials {
                    self.secrets.set(&reference, &value)?;
                }
                migrated.push(converted);
            }
        }
        let count = migrated.len();
        file.servers.extend(migrated);
        file.legacy_migrated = true;
        validate_servers(&file.servers)?;
        self.write_file(&file)?;
        Ok(count)
    }

    fn convert_legacy(
        &self,
        server_id: &str,
        legacy: &ServerSpec,
    ) -> Result<Option<MigratedMcpServer>, ProductError> {
        let mut credential_values = Vec::new();
        let transport = match legacy {
            ServerSpec::Stdio { command, args, env } => {
                let mut refs = BTreeMap::new();
                for (name, value) in env {
                    let reference = self.secrets.reference(server_id, "env", name)?;
                    refs.insert(name.clone(), reference.clone());
                    credential_values.push((reference, value.clone()));
                }
                McpTransportConfig::Stdio {
                    command: command.clone(),
                    args: args.clone(),
                    env: refs,
                }
            }
            ServerSpec::Http { url, headers } => {
                let mut refs = BTreeMap::new();
                for (name, value) in headers {
                    let reference = self.secrets.reference(server_id, "header", name)?;
                    refs.insert(name.clone(), reference.clone());
                    credential_values.push((reference, value.clone()));
                }
                McpTransportConfig::StreamableHttp {
                    url: url.clone(),
                    headers: refs,
                }
            }
        };
        let config = McpServerConfig {
            id: server_id.to_string(),
            display_name: server_id.to_string(),
            description: "Migrated from the legacy R-Code configuration".to_string(),
            enabled: false,
            source: McpServerSource::User,
            transport,
            approved_launch_fingerprint: None,
        };
        if config.validate().is_err() {
            return Ok(None);
        }
        Ok(Some((config, credential_values)))
    }

    fn read_file(&self) -> Result<McpSettingsFile, ProductError> {
        let path = self.path();
        if !path.exists() {
            return Ok(McpSettingsFile::default());
        }
        let content = std::fs::read_to_string(&path).map_err(|error| {
            ProductError::ConfigError(format!("read {}: {error}", path.display()))
        })?;
        let file: McpSettingsFile = toml::from_str(&content).map_err(|error| {
            ProductError::ConfigError(format!("parse {}: {error}", path.display()))
        })?;
        if file.schema_version != SETTINGS_SCHEMA_VERSION {
            return Err(ProductError::ConfigError(format!(
                "unsupported MCP settings schema version: {}",
                file.schema_version
            )));
        }
        Ok(file)
    }

    fn write_file(&self, file: &McpSettingsFile) -> Result<(), ProductError> {
        let content = toml::to_string_pretty(file).map_err(|error| {
            ProductError::ConfigError(format!("serialize MCP settings: {error}"))
        })?;
        atomic_write(&self.path(), content.as_bytes())
    }
}

fn builtin_research() -> McpServerConfig {
    McpServerConfig {
        id: RESEARCH_SERVER_ID.to_string(),
        display_name: "R-Code Research".to_string(),
        description: "Built-in multi-source research service".to_string(),
        enabled: true,
        source: McpServerSource::Builtin,
        transport: McpTransportConfig::Builtin {
            server: BuiltinMcpServer::Research,
        },
        approved_launch_fingerprint: None,
    }
}

fn reconcile_builtin(servers: &mut Vec<McpServerConfig>) -> bool {
    let canonical = builtin_research();
    if let Some(existing) = servers
        .iter_mut()
        .find(|server| server.id == RESEARCH_SERVER_ID)
    {
        let enabled = existing.enabled;
        let mut replacement = canonical;
        replacement.enabled = enabled;
        if *existing == replacement {
            return false;
        }
        *existing = replacement;
        return true;
    }
    servers.push(canonical);
    true
}

fn validate_servers(servers: &[McpServerConfig]) -> Result<(), ProductError> {
    let mut ids = BTreeSet::new();
    for server in servers {
        server
            .validate()
            .map_err(|error| ProductError::ConfigError(error.to_string()))?;
        if !ids.insert(server.id.as_str()) {
            return Err(ProductError::ConfigError(format!(
                "duplicate MCP server id: {}",
                server.id
            )));
        }
    }
    Ok(())
}

fn atomic_write(path: &Path, content: &[u8]) -> Result<(), ProductError> {
    let parent = path
        .parent()
        .ok_or_else(|| ProductError::ConfigError("MCP settings path has no parent".to_string()))?;
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(content)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| ProductError::from(error.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Mutex};

    use super::*;

    #[derive(Default)]
    struct MemoryCredentials {
        values: Mutex<HashMap<String, String>>,
    }

    impl McpCredentialBackend for MemoryCredentials {
        fn set(&self, account: &str, value: &str) -> Result<(), ProductError> {
            self.values
                .lock()
                .unwrap()
                .insert(account.to_string(), value.to_string());
            Ok(())
        }

        fn get(&self, account: &str) -> Result<Option<String>, ProductError> {
            Ok(self.values.lock().unwrap().get(account).cloned())
        }

        fn delete(&self, account: &str) -> Result<(), ProductError> {
            self.values.lock().unwrap().remove(account);
            Ok(())
        }
    }

    #[test]
    fn builtin_is_seeded_and_user_disable_survives_reload() {
        let temp = tempfile::tempdir().unwrap();
        let service = McpSettingsService::with_credentials(
            temp.path().to_path_buf(),
            Arc::new(MemoryCredentials::default()),
        );

        let mut servers = service.load().unwrap();
        assert!(servers[0].enabled);
        servers[0].enabled = false;
        service.save(servers).unwrap();

        let restored = service.load().unwrap();
        assert!(!restored[0].enabled);
        assert!(restored[0].is_builtin());
        assert!(service.path().starts_with(temp.path()));
    }

    #[test]
    fn corruption_is_reported_without_overwriting_original_file() {
        let temp = tempfile::tempdir().unwrap();
        let service = McpSettingsService::with_credentials(
            temp.path().to_path_buf(),
            Arc::new(MemoryCredentials::default()),
        );
        std::fs::create_dir_all(temp.path()).unwrap();
        std::fs::write(service.path(), "this is not = valid toml = [").unwrap();

        assert!(service.load().is_err());
        assert_eq!(
            std::fs::read_to_string(service.path()).unwrap(),
            "this is not = valid toml = ["
        );
    }

    #[test]
    fn legacy_secrets_move_to_credentials_and_never_reach_toml() {
        let temp = tempfile::tempdir().unwrap();
        let credentials = Arc::new(MemoryCredentials::default());
        let service =
            McpSettingsService::with_credentials(temp.path().to_path_buf(), credentials.clone());
        let mut legacy = Config::default();
        legacy.mcp_servers.insert(
            "legacy".to_string(),
            ServerSpec::Stdio {
                command: "node".to_string(),
                args: vec!["server.js".to_string()],
                env: HashMap::from([(
                    "API_TOKEN".to_string(),
                    "sentinel-legacy-secret".to_string(),
                )]),
            },
        );

        assert_eq!(service.migrate_legacy(&legacy).unwrap(), 1);
        assert_eq!(service.migrate_legacy(&legacy).unwrap(), 0);
        let servers = service.load().unwrap();
        let migrated = servers.iter().find(|server| server.id == "legacy").unwrap();
        assert!(!migrated.enabled);
        let saved = std::fs::read_to_string(service.path()).unwrap();
        assert!(!saved.contains("sentinel-legacy-secret"));
        assert!(saved.contains("mcp:legacy:env:API_TOKEN"));
        assert!(credentials
            .values
            .lock()
            .unwrap()
            .values()
            .any(|value| value == "sentinel-legacy-secret"));
    }

    #[tokio::test]
    async fn secret_resolver_returns_values_without_exposing_them_in_reference() {
        let temp = tempfile::tempdir().unwrap();
        let service = McpSettingsService::with_credentials(
            temp.path().to_path_buf(),
            Arc::new(MemoryCredentials::default()),
        );
        let secrets = service.secret_store();
        let reference = secrets
            .reference("server", "header", "Authorization")
            .unwrap();
        secrets.set(&reference, "Bearer sentinel").unwrap();

        assert!(secrets.configured(&reference).unwrap());
        assert_eq!(
            secrets.resolve(&reference).await.unwrap().as_deref(),
            Some("Bearer sentinel")
        );
        assert!(!reference.as_str().contains("sentinel"));
        secrets.delete(&reference).unwrap();
        assert!(!secrets.configured(&reference).unwrap());
    }
}
