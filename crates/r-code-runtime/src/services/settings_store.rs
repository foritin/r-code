//! V2 host model settings under the harness-v2 profile root.
//!
//! One JSON file (`settings.json`) plus a platform credential store; the
//! daemon is the single writer. Entries reference the shared provider
//! catalog for protocol/base-url defaults, so a configured selection maps
//! deterministically to an `agent-llm` provider. API keys never appear in
//! the settings file or on any wire projection.

use crate::providers::ProviderRegistry;
use crate::services::provider_catalog::{self, Protocol};
use agent_contract::provider::LlmProvider;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// v2 credential backend: OS keychain on Windows/Linux under a service name
/// distinct from the GUI's, the encrypted file store on macOS. Mirrors the
/// GUI settings service's platform split with the same three-method surface.
enum CredentialStore {
    #[cfg(not(target_os = "macos"))]
    Keyring(r_code_core::secret::SecretStore),
    #[cfg(target_os = "macos")]
    File(r_code_core::secret::EncryptedFileSecretStore),
}

impl CredentialStore {
    fn new(root: &Path) -> Self {
        #[cfg(not(target_os = "macos"))]
        {
            let _ = root;
            Self::Keyring(r_code_core::secret::SecretStore::new("r-code-harness-v2"))
        }
        #[cfg(target_os = "macos")]
        {
            Self::File(r_code_core::secret::EncryptedFileSecretStore::new(root))
        }
    }

    fn store(&self, account: &str, value: &str) -> Result<(), String> {
        #[cfg(not(target_os = "macos"))]
        {
            match self {
                Self::Keyring(store) => store.store(account, value).map_err(|e| e.to_string()),
            }
        }
        #[cfg(target_os = "macos")]
        {
            match self {
                Self::File(store) => store.store(account, value).map_err(|e| e.to_string()),
            }
        }
    }

    fn get(&self, account: &str) -> Option<String> {
        #[cfg(not(target_os = "macos"))]
        {
            match self {
                Self::Keyring(store) => store.get(account).ok().flatten(),
            }
        }
        #[cfg(target_os = "macos")]
        {
            match self {
                Self::File(store) => store.get(account).ok().flatten(),
            }
        }
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        #[cfg(not(target_os = "macos"))]
        {
            match self {
                Self::Keyring(store) => store.delete(account).map_err(|e| e.to_string()),
            }
        }
        #[cfg(target_os = "macos")]
        {
            match self {
                Self::File(store) => store.delete(account).map_err(|e| e.to_string()),
            }
        }
    }
}

/// One configured provider (persisted shape; no key material).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderEntry {
    /// Selection id (`<preset-id>` — also the catalog key).
    pub selection: String,
    pub model: String,
    /// Overrides the catalog default when set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Wire protocol slug override (defaults to the preset protocol).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    /// Environment variable holding the key instead of the credential store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_var: Option<String>,
}

/// The persisted settings document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct V2Settings {
    #[serde(default)]
    pub providers: Vec<ProviderEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_selection: Option<String>,
}

/// Credential-free availability row for clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderAvailability {
    pub selection: String,
    pub model: String,
    pub has_credential: bool,
    pub is_default: bool,
}

/// File-backed v2 settings with platform credentials.
pub struct SettingsStore {
    root: PathBuf,
    credentials: CredentialStore,
}

impl SettingsStore {
    /// `root` is the harness-v2 profile root.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let credentials = CredentialStore::new(&root);
        Self { root, credentials }
    }

    fn settings_path(&self) -> PathBuf {
        self.root.join("settings.json")
    }

    /// Load the settings document (missing file → empty, not an error).
    pub fn load(&self) -> V2Settings {
        std::fs::read_to_string(self.settings_path())
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Persist the settings document.
    pub fn save(&self, settings: &V2Settings) -> Result<(), String> {
        std::fs::create_dir_all(&self.root).map_err(|e| format!("create root: {e}"))?;
        let text = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
        std::fs::write(self.settings_path(), text).map_err(|e| format!("write settings: {e}"))
    }

    /// Upsert one provider entry; stores the key (when given) in the
    /// credential store under the selection id.
    pub fn apply_provider(
        &self,
        entry: ProviderEntry,
        api_key: Option<&str>,
    ) -> Result<(), String> {
        if let Some(key) = api_key.filter(|key| !key.is_empty()) {
            self.credentials
                .store(&entry.selection, key)
                .map_err(|e| format!("store credential: {e}"))?;
        }
        let mut settings = self.load();
        settings
            .providers
            .retain(|existing| existing.selection != entry.selection);
        settings.providers.push(entry);
        if settings.default_selection.is_none() {
            settings.default_selection = settings.providers.first().map(|p| p.selection.clone());
        }
        self.save(&settings)
    }

    /// Remove a provider entry and its credential.
    pub fn remove_provider(&self, selection: &str) -> Result<(), String> {
        let mut settings = self.load();
        settings.providers.retain(|p| p.selection != selection);
        if settings.default_selection.as_deref() == Some(selection) {
            settings.default_selection = settings.providers.first().map(|p| p.selection.clone());
        }
        let _ = self.credentials.delete(selection);
        self.save(&settings)
    }

    /// Set the default selection (must be configured).
    pub fn set_default(&self, selection: &str) -> Result<(), String> {
        let mut settings = self.load();
        if !settings.providers.iter().any(|p| p.selection == selection) {
            return Err(format!("provider {selection} is not configured"));
        }
        settings.default_selection = Some(selection.to_string());
        self.save(&settings)
    }

    /// Resolve the effective API key for an entry: credential store first,
    /// then the declared environment variable.
    fn credential_of(&self, entry: &ProviderEntry) -> Option<String> {
        if let Some(key) = self.credentials.get(&entry.selection) {
            if !key.is_empty() {
                return Some(key);
            }
        }
        if let Some(var) = &entry.env_var {
            if let Ok(value) = std::env::var(var) {
                if !value.is_empty() {
                    return Some(value);
                }
            }
        }
        None
    }

    /// The availability projection (credential-free).
    pub fn availability(&self) -> Vec<ProviderAvailability> {
        let settings = self.load();
        let default = settings.default_selection.clone().unwrap_or_default();
        settings
            .providers
            .iter()
            .map(|entry| ProviderAvailability {
                selection: entry.selection.clone(),
                model: entry.model.clone(),
                has_credential: self.credential_of(entry).is_some(),
                is_default: entry.selection == default,
            })
            .collect()
    }

    /// Build the provider registry from current settings. Entries without a
    /// resolvable credential are skipped (providers validate keys at
    /// construction); `availability()` keeps listing them as unavailable so
    /// clients see configured-but-unauthenticated selections.
    pub fn registry(&self) -> ProviderRegistry {
        let settings = self.load();
        let mut registry = ProviderRegistry::new();
        for entry in &settings.providers {
            if self.credential_of(entry).is_none() {
                continue;
            }
            let Some(config) = self.build_provider_config(entry) else {
                continue;
            };
            match agent_llm::create_provider(config) {
                Ok(provider) => {
                    registry.register(
                        &entry.selection,
                        std::sync::Arc::from(provider),
                        &entry.model,
                    );
                }
                Err(error) => {
                    eprintln!(
                        "r-code-service: provider {} unusable: {error}",
                        entry.selection
                    );
                }
            }
        }
        if let Some(default) = settings
            .default_selection
            .as_ref()
            .filter(|id| settings.providers.iter().any(|p| &p.selection == *id))
        {
            registry.set_default(default);
        }
        registry
    }

    /// Map one entry onto an agent-llm provider config via the catalog:
    /// protocol follows the preset (or the explicit override), base_url
    /// falls back to the preset default, vendor identity comes from the
    /// preset id (deepseek / ark / kimi coding specializations).
    fn build_provider_config(&self, entry: &ProviderEntry) -> Option<agent_llm::ProviderConfig> {
        let preset = provider_catalog::find(&entry.selection)?;
        let protocol = entry
            .protocol
            .as_deref()
            .and_then(Protocol::parse)
            .unwrap_or(preset.protocol);
        let base_url = entry
            .base_url
            .clone()
            .unwrap_or_else(|| preset.base_url.to_string());
        let api_key = self.credential_of(entry).unwrap_or_default();
        let model = entry.model.clone();
        let id = preset.id;
        let is_deepseek = id.contains("deepseek");
        let ark_kind = if id.starts_with("ark") {
            Some(
                if id.contains("agent") {
                    "ark_agent"
                } else {
                    "ark_coding"
                }
                .to_string(),
            )
        } else {
            None
        };
        let kimi_coding = id.contains("kimi");
        Some(match protocol {
            Protocol::AnthropicMessages => {
                if is_deepseek {
                    agent_llm::ProviderConfig::DeepSeekAnthropic {
                        api_key,
                        model,
                        base_url: Some(base_url),
                    }
                } else if let Some(kind) = ark_kind {
                    agent_llm::ProviderConfig::ArkAnthropic {
                        api_key,
                        model,
                        base_url: Some(base_url),
                        kind,
                    }
                } else if kimi_coding {
                    agent_llm::ProviderConfig::KimiCodingAnthropic {
                        api_key,
                        model,
                        base_url: Some(base_url),
                    }
                } else {
                    agent_llm::ProviderConfig::Anthropic {
                        api_key,
                        model,
                        base_url: Some(base_url),
                    }
                }
            }
            Protocol::OpenAiResponses => {
                if is_deepseek {
                    agent_llm::ProviderConfig::DeepSeekResponses {
                        api_key,
                        model,
                        base_url,
                    }
                } else if let Some(kind) = ark_kind {
                    agent_llm::ProviderConfig::ArkResponses {
                        api_key,
                        model,
                        base_url,
                        kind,
                    }
                } else {
                    let reasoning = if preset.reasoning_replay {
                        agent_llm::ReasoningMode::EncryptedReplay
                    } else {
                        agent_llm::ReasoningMode::Drop
                    };
                    agent_llm::ProviderConfig::Responses {
                        api_key,
                        model,
                        base_url,
                        reasoning,
                    }
                }
            }
            Protocol::OpenAiChat => {
                if is_deepseek {
                    agent_llm::ProviderConfig::DeepSeek {
                        api_key,
                        model,
                        base_url: Some(base_url),
                    }
                } else if let Some(kind) = ark_kind {
                    agent_llm::ProviderConfig::ArkChat {
                        api_key,
                        model,
                        base_url,
                        kind,
                    }
                } else if kimi_coding {
                    agent_llm::ProviderConfig::KimiChat {
                        api_key,
                        model,
                        base_url,
                    }
                } else {
                    agent_llm::ProviderConfig::OpenAi {
                        api_key,
                        model,
                        base_url,
                    }
                }
            }
        })
    }
}

/// Read-only convenience for frontends deciding "is anything configured"
/// without resolving credential contents.
pub fn has_configured_provider(root: &Path) -> bool {
    SettingsStore::new(root)
        .availability()
        .iter()
        .any(|row| row.has_credential)
}

/// Live resolver over the settings store: every resolution rebuilds the
/// registry from the current settings document, so settings applied at
/// runtime take effect on the next model call without a daemon restart.
pub struct SettingsBackedResolver {
    store: SettingsStore,
}

impl SettingsBackedResolver {
    pub fn new(store: SettingsStore) -> Arc<Self> {
        Arc::new(Self { store })
    }
}

impl crate::services::models::ProviderResolver for SettingsBackedResolver {
    fn resolve(&self, selection: &str) -> Option<(std::sync::Arc<dyn LlmProvider>, String)> {
        self.store.registry().resolve(selection)
    }

    fn default_selection(&self) -> String {
        self.store.registry().default_selection()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::models::ProviderResolver as _;

    fn store() -> (tempfile::TempDir, SettingsStore) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = SettingsStore::new(dir.path().to_path_buf());
        (dir, store)
    }

    #[test]
    fn empty_root_loads_empty_settings() {
        let (_dir, store) = store();
        assert_eq!(store.load(), V2Settings::default());
        assert!(store.availability().is_empty());
    }

    #[test]
    fn apply_provider_persists_and_masks_credentials() {
        let (dir, store) = store();
        store
            .apply_provider(
                ProviderEntry {
                    selection: "openai".into(),
                    model: "gpt-5".into(),
                    base_url: None,
                    protocol: None,
                    env_var: None,
                },
                None,
            )
            .expect("apply");
        let settings = store.load();
        assert_eq!(settings.providers.len(), 1);
        assert_eq!(settings.default_selection.as_deref(), Some("openai"));
        // The settings file itself never carries key material.
        let text = std::fs::read_to_string(dir.path().join("settings.json")).unwrap();
        assert!(!text.contains("sk-"));
        // Without env var or credential the row reports unavailable.
        assert!(!store.availability()[0].has_credential);
    }

    #[test]
    fn env_var_entries_resolve_credentials_from_environment() {
        let (_dir, store) = store();
        std::env::set_var("R_CODE_TEST_PROVIDER_KEY", "env-key");
        store
            .apply_provider(
                ProviderEntry {
                    selection: "deepseek".into(),
                    model: "deepseek-chat".into(),
                    base_url: None,
                    protocol: None,
                    env_var: Some("R_CODE_TEST_PROVIDER_KEY".into()),
                },
                None,
            )
            .expect("apply");
        assert!(store.availability()[0].has_credential);
        let registry = store.registry();
        assert_eq!(registry.default_selection(), "deepseek");
        std::env::remove_var("R_CODE_TEST_PROVIDER_KEY");
    }

    #[test]
    fn registry_maps_preset_protocols_to_provider_configs() {
        let (_dir, store) = store();
        std::env::set_var("R_CODE_TEST_ANTHROPIC_KEY", "sk-ant-test");
        store
            .apply_provider(
                ProviderEntry {
                    selection: "anthropic".into(),
                    model: "claude-sonnet-4".into(),
                    base_url: None,
                    protocol: None,
                    env_var: Some("R_CODE_TEST_ANTHROPIC_KEY".into()),
                },
                None,
            )
            .expect("apply");
        let registry = store.registry();
        assert!(registry.resolve("anthropic").is_some());
        assert!(registry.resolve("nope").is_none());
        std::env::remove_var("R_CODE_TEST_ANTHROPIC_KEY");
    }
}
