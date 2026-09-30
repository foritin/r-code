//! Harness v1 host model settings under the profile root.
//!
//! One JSON file (`settings.json`) plus a platform credential store; the
//! daemon is the single writer. Entries reference the shared provider
//! catalog for protocol/base-url defaults, so a configured selection maps
//! deterministically to an `agent-llm` provider. API keys never appear in
//! the settings file or on any wire projection.

use crate::providers::ProviderRegistry;
use crate::services::provider_catalog::{self, Protocol};
use agent_contract::provider::LlmProvider;
use r_code_kernel::task::{ProviderRouteKind, ProviderSnapshotRef};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

/// Typed failures from the daemon-owned settings and provider route store.
/// No variant carries credential material.
#[derive(Debug, thiserror::Error)]
pub enum SettingsStoreError {
    #[error("failed to read settings at {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("settings at {path} are corrupt: {source}")]
    Corrupt {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("failed to serialize settings: {0}")]
    Serialize(#[source] serde_json::Error),
    #[error("failed to persist settings at {path}: {source}")]
    Persist {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("stale settings revision: expected {expected}, actual {actual}")]
    StaleRevision { expected: u64, actual: u64 },
    #[error("settings revision overflow at {0}")]
    RevisionOverflow(u64),
    #[error("settings mutation lock is poisoned")]
    LockPoisoned,
    #[error("provider {selection:?} does not exist in the provider catalog")]
    UnknownProvider { selection: String },
    #[error("provider {selection:?} is not configured")]
    ProviderNotConfigured { selection: String },
    #[error("provider {selection:?} has no configured credential")]
    MissingCredential { selection: String },
    #[error("provider {selection:?} has invalid protocol {protocol:?}")]
    InvalidProtocol { selection: String, protocol: String },
    #[error("credential store {operation} failed for provider {selection:?}")]
    Credential {
        operation: &'static str,
        selection: String,
    },
    #[error("provider {selection:?} could not be initialized")]
    ProviderUnavailable { selection: String },
}

/// Harness v1 credential backend: OS keychain on Windows/Linux under a service name
/// distinct from the GUI's, the encrypted file store on macOS. Mirrors the
/// GUI settings service's platform split with the same three-method surface.
enum CredentialStore {
    #[cfg(not(target_os = "macos"))]
    Keyring {
        current: r_code_core::secret::SecretStore,
        pre_v1: r_code_core::secret::SecretStore,
    },
    #[cfg(target_os = "macos")]
    File(r_code_core::secret::EncryptedFileSecretStore),
}

impl CredentialStore {
    fn new(root: &Path, current_service: &str, pre_v1_service: &str) -> Self {
        #[cfg(not(target_os = "macos"))]
        {
            let _ = root;
            Self::Keyring {
                current: r_code_core::secret::SecretStore::new(current_service),
                pre_v1: r_code_core::secret::SecretStore::new(pre_v1_service),
            }
        }
        #[cfg(target_os = "macos")]
        {
            let _ = (current_service, pre_v1_service);
            Self::File(r_code_core::secret::EncryptedFileSecretStore::new(root))
        }
    }

    fn store(&self, account: &str, value: &str) -> Result<(), SettingsStoreError> {
        #[cfg(not(target_os = "macos"))]
        {
            match self {
                Self::Keyring { current, .. } => {
                    current
                        .store(account, value)
                        .map_err(|_| SettingsStoreError::Credential {
                            operation: "write",
                            selection: account.to_string(),
                        })
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            match self {
                Self::File(store) => {
                    store
                        .store(account, value)
                        .map_err(|_| SettingsStoreError::Credential {
                            operation: "write",
                            selection: account.to_string(),
                        })
                }
            }
        }
    }

    fn get(&self, account: &str) -> Result<Option<String>, SettingsStoreError> {
        #[cfg(not(target_os = "macos"))]
        {
            match self {
                Self::Keyring { current, pre_v1 } => {
                    if let Some(value) =
                        current
                            .get(account)
                            .map_err(|_| SettingsStoreError::Credential {
                                operation: "read",
                                selection: account.to_string(),
                            })?
                    {
                        return Ok(Some(value));
                    }
                    let Some(value) =
                        pre_v1
                            .get(account)
                            .map_err(|_| SettingsStoreError::Credential {
                                operation: "migrate",
                                selection: account.to_string(),
                            })?
                    else {
                        return Ok(None);
                    };
                    current
                        .store(account, &value)
                        .map_err(|_| SettingsStoreError::Credential {
                            operation: "migrate",
                            selection: account.to_string(),
                        })?;
                    // A successful copy completes the one-time compatibility
                    // migration. Failure to clean up the old namespace must
                    // not make the newly migrated credential unavailable.
                    let _ = pre_v1.delete(account);
                    Ok(Some(value))
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            match self {
                Self::File(store) => {
                    store
                        .get(account)
                        .map_err(|_| SettingsStoreError::Credential {
                            operation: "read",
                            selection: account.to_string(),
                        })
                }
            }
        }
    }

    fn delete(&self, account: &str) -> Result<(), SettingsStoreError> {
        #[cfg(not(target_os = "macos"))]
        {
            match self {
                Self::Keyring { current, pre_v1 } => {
                    let result =
                        current
                            .delete(account)
                            .map_err(|_| SettingsStoreError::Credential {
                                operation: "delete",
                                selection: account.to_string(),
                            });
                    let _ = pre_v1.delete(account);
                    result
                }
            }
        }
        #[cfg(target_os = "macos")]
        {
            match self {
                Self::File(store) => {
                    store
                        .delete(account)
                        .map_err(|_| SettingsStoreError::Credential {
                            operation: "delete",
                            selection: account.to_string(),
                        })
                }
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
pub struct V1Settings {
    /// Monotonic compare-and-swap revision. Legacy files omit this field and
    /// therefore deserialize at revision zero.
    #[serde(default)]
    pub revision: u64,
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

/// One provider route resolved atomically for a run. Metadata is safe to
/// persist; credential material is captured only inside the provider object.
/// Cloning this value never re-reads settings or the credential backend.
#[derive(Clone)]
pub struct ResolvedProviderSnapshot {
    snapshot: ProviderSnapshotRef,
    provider: Arc<dyn LlmProvider>,
}

impl ResolvedProviderSnapshot {
    pub fn snapshot(&self) -> &ProviderSnapshotRef {
        &self.snapshot
    }

    pub fn into_parts(self) -> (ProviderSnapshotRef, Arc<dyn LlmProvider>) {
        (self.snapshot, self.provider)
    }
}

/// Result of resolving the task selection against one checked settings read.
/// An empty, valid settings document is represented explicitly so development
/// callers may substitute an injected deterministic model while production
/// callers fail before a run is started.
pub enum RunProviderResolution {
    Resolved(ResolvedProviderSnapshot),
    Unconfigured { settings_revision: u64 },
}

/// File-backed Harness v1 settings with platform credentials.
pub struct SettingsStore {
    root: PathBuf,
    credentials: CredentialStore,
    mutation_lock: Mutex<()>,
}

impl SettingsStore {
    /// Compatibility constructor for callers that do not yet carry a
    /// [`crate::profile::RuntimeProfile`]. New daemon composition must prefer
    /// [`Self::for_profile`] so development and production credentials cannot
    /// share a namespace.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_credential_services(
            root,
            crate::profile::ProfileFlavor::Production.credential_service(),
            crate::profile::ProfileFlavor::Production.pre_v1_credential_service(),
        )
    }

    /// Construct the store from the resolved profile identity. This is the
    /// production path: the current keychain namespace is supplied by
    /// `RuntimeProfile::credential_service`, never guessed by this service.
    pub fn for_profile(profile: &crate::profile::RuntimeProfile) -> Self {
        Self::with_credential_services(
            profile.harness_v1_root(),
            profile.credential_service(),
            profile.pre_v1_credential_service(),
        )
    }

    fn with_credential_services(
        root: impl Into<PathBuf>,
        current_service: &str,
        pre_v1_service: &str,
    ) -> Self {
        let root = root.into();
        let credentials = CredentialStore::new(&root, current_service, pre_v1_service);
        Self {
            root,
            credentials,
            mutation_lock: Mutex::new(()),
        }
    }

    fn settings_path(&self) -> PathBuf {
        self.root.join("settings.json")
    }

    fn mutation_guard(&self) -> Result<MutexGuard<'_, ()>, SettingsStoreError> {
        self.mutation_lock
            .lock()
            .map_err(|_| SettingsStoreError::LockPoisoned)
    }

    /// Load the settings document without losing failure information. A
    /// missing file is the only condition that produces default settings;
    /// unreadable or malformed files are explicit, fail-closed errors.
    pub fn load_checked(&self) -> Result<V1Settings, SettingsStoreError> {
        let path = self.settings_path();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Ok(V1Settings::default());
            }
            Err(source) => return Err(SettingsStoreError::Read { path, source }),
        };
        serde_json::from_str(&text).map_err(|source| SettingsStoreError::Corrupt { path, source })
    }

    /// Legacy lossy projection retained for source compatibility with older
    /// read-only clients and tests. Effectful runtime paths must use
    /// [`Self::load_checked`]; in particular this method is never used by
    /// mutation, registry construction, or route snapshot resolution.
    pub fn load(&self) -> V1Settings {
        self.load_checked().unwrap_or_default()
    }

    /// Compare-and-swap the complete settings document. `replacement.revision`
    /// is ignored; the store assigns `expected_revision + 1` after checking
    /// the on-disk revision while holding the single-writer mutex.
    pub fn compare_and_swap(
        &self,
        expected_revision: u64,
        mut replacement: V1Settings,
    ) -> Result<V1Settings, SettingsStoreError> {
        let _guard = self.mutation_guard()?;
        let current = self.load_checked()?;
        Self::check_revision(expected_revision, current.revision)?;
        replacement.revision = Self::next_revision(current.revision)?;
        self.persist_atomic(&replacement)?;
        Ok(replacement)
    }

    /// CAS update helper for callers changing only part of the document.
    pub fn update<F>(
        &self,
        expected_revision: u64,
        update: F,
    ) -> Result<V1Settings, SettingsStoreError>
    where
        F: FnOnce(&mut V1Settings),
    {
        let _guard = self.mutation_guard()?;
        let mut settings = self.load_checked()?;
        Self::check_revision(expected_revision, settings.revision)?;
        update(&mut settings);
        settings.revision = Self::next_revision(settings.revision)?;
        self.persist_atomic(&settings)?;
        Ok(settings)
    }

    /// Compatibility save with CAS semantics: the supplied document's
    /// revision is treated as the expected on-disk revision.
    pub fn save(&self, settings: &V1Settings) -> Result<(), SettingsStoreError> {
        self.compare_and_swap(settings.revision, settings.clone())
            .map(|_| ())
    }

    fn check_revision(expected: u64, actual: u64) -> Result<(), SettingsStoreError> {
        if expected == actual {
            Ok(())
        } else {
            Err(SettingsStoreError::StaleRevision { expected, actual })
        }
    }

    fn next_revision(current: u64) -> Result<u64, SettingsStoreError> {
        current
            .checked_add(1)
            .ok_or(SettingsStoreError::RevisionOverflow(current))
    }

    /// Write and sync a same-directory temporary file before atomically
    /// replacing `settings.json`. On every pre-replace failure the previous
    /// target remains untouched; this call removes only its own temporary.
    fn persist_atomic(&self, settings: &V1Settings) -> Result<(), SettingsStoreError> {
        let target = self.settings_path();
        let text = serde_json::to_vec_pretty(settings).map_err(SettingsStoreError::Serialize)?;
        std::fs::create_dir_all(&self.root).map_err(|source| SettingsStoreError::Persist {
            path: target.clone(),
            source,
        })?;
        let temporary = self.root.join(format!(
            ".settings.json.r-code-tmp-{}",
            uuid::Uuid::new_v4().simple()
        ));

        let result = (|| -> io::Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&text)?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&temporary, &target)?;
            Ok(())
        })();

        if let Err(source) = result {
            let _ = std::fs::remove_file(&temporary);
            return Err(SettingsStoreError::Persist {
                path: target,
                source,
            });
        }
        Ok(())
    }

    /// Upsert one provider entry; stores the key (when given) in the
    /// credential store under the selection id.
    pub fn apply_provider(
        &self,
        entry: ProviderEntry,
        api_key: Option<&str>,
    ) -> Result<(), SettingsStoreError> {
        self.apply_provider_inner(None, entry, api_key).map(|_| ())
    }

    /// Revision-fenced provider upsert used by the daemon mutation boundary.
    pub fn apply_provider_at_revision(
        &self,
        expected_revision: u64,
        entry: ProviderEntry,
        api_key: Option<&str>,
    ) -> Result<V1Settings, SettingsStoreError> {
        self.apply_provider_inner(Some(expected_revision), entry, api_key)
    }

    fn apply_provider_inner(
        &self,
        expected_revision: Option<u64>,
        entry: ProviderEntry,
        api_key: Option<&str>,
    ) -> Result<V1Settings, SettingsStoreError> {
        let _guard = self.mutation_guard()?;
        let mut settings = self.load_checked()?;
        if let Some(expected_revision) = expected_revision {
            Self::check_revision(expected_revision, settings.revision)?;
        }
        if let Some(key) = api_key.filter(|key| !key.is_empty()) {
            self.credentials.store(&entry.selection, key)?;
        }
        settings
            .providers
            .retain(|existing| existing.selection != entry.selection);
        settings.providers.push(entry);
        if settings.default_selection.is_none() {
            settings.default_selection = settings.providers.first().map(|p| p.selection.clone());
        }
        settings.revision = Self::next_revision(settings.revision)?;
        self.persist_atomic(&settings)?;
        Ok(settings)
    }

    /// Remove a provider entry and its credential.
    pub fn remove_provider(&self, selection: &str) -> Result<(), SettingsStoreError> {
        self.remove_provider_inner(None, selection).map(|_| ())
    }

    /// Revision-fenced provider removal used by the daemon mutation boundary.
    pub fn remove_provider_at_revision(
        &self,
        expected_revision: u64,
        selection: &str,
    ) -> Result<V1Settings, SettingsStoreError> {
        self.remove_provider_inner(Some(expected_revision), selection)
    }

    fn remove_provider_inner(
        &self,
        expected_revision: Option<u64>,
        selection: &str,
    ) -> Result<V1Settings, SettingsStoreError> {
        let _guard = self.mutation_guard()?;
        let mut settings = self.load_checked()?;
        if let Some(expected_revision) = expected_revision {
            Self::check_revision(expected_revision, settings.revision)?;
        }
        if !settings
            .providers
            .iter()
            .any(|provider| provider.selection == selection)
        {
            return Err(SettingsStoreError::ProviderNotConfigured {
                selection: selection.to_string(),
            });
        }
        settings.providers.retain(|p| p.selection != selection);
        if settings.default_selection.as_deref() == Some(selection) {
            settings.default_selection = settings.providers.first().map(|p| p.selection.clone());
        }
        settings.revision = Self::next_revision(settings.revision)?;
        self.persist_atomic(&settings)?;
        self.credentials.delete(selection)?;
        Ok(settings)
    }

    /// Set the default selection (must be configured).
    pub fn set_default(&self, selection: &str) -> Result<(), SettingsStoreError> {
        self.set_default_inner(None, selection).map(|_| ())
    }

    /// Revision-fenced default-route update used by the daemon mutation
    /// boundary.
    pub fn set_default_at_revision(
        &self,
        expected_revision: u64,
        selection: &str,
    ) -> Result<V1Settings, SettingsStoreError> {
        self.set_default_inner(Some(expected_revision), selection)
    }

    fn set_default_inner(
        &self,
        expected_revision: Option<u64>,
        selection: &str,
    ) -> Result<V1Settings, SettingsStoreError> {
        let _guard = self.mutation_guard()?;
        let mut settings = self.load_checked()?;
        if let Some(expected_revision) = expected_revision {
            Self::check_revision(expected_revision, settings.revision)?;
        }
        if !settings.providers.iter().any(|p| p.selection == selection) {
            return Err(SettingsStoreError::ProviderNotConfigured {
                selection: selection.to_string(),
            });
        }
        settings.default_selection = Some(selection.to_string());
        settings.revision = Self::next_revision(settings.revision)?;
        self.persist_atomic(&settings)?;
        Ok(settings)
    }

    /// Resolve the effective API key for an entry: credential store first,
    /// then the declared environment variable.
    fn credential_of(&self, entry: &ProviderEntry) -> Result<Option<String>, SettingsStoreError> {
        let stored = self.credentials.get(&entry.selection);
        if let Ok(Some(key)) = &stored {
            if !key.is_empty() {
                return Ok(Some(key.clone()));
            }
        }
        if let Some(var) = &entry.env_var {
            if let Ok(value) = std::env::var(var) {
                if !value.is_empty() {
                    return Ok(Some(value));
                }
            }
        }
        stored
    }

    /// The availability projection (credential-free).
    pub fn availability_checked(&self) -> Result<Vec<ProviderAvailability>, SettingsStoreError> {
        let settings = self.load_checked()?;
        let default = settings.default_selection.clone().unwrap_or_default();
        settings
            .providers
            .iter()
            .map(|entry| {
                Ok(ProviderAvailability {
                    selection: entry.selection.clone(),
                    model: entry.model.clone(),
                    has_credential: self.credential_of(entry)?.is_some(),
                    is_default: entry.selection == default,
                })
            })
            .collect()
    }

    /// Legacy read-only projection. New daemon RPCs should return the typed
    /// error from [`Self::availability_checked`] instead of collapsing it.
    pub fn availability(&self) -> Vec<ProviderAvailability> {
        self.availability_checked().unwrap_or_default()
    }

    /// Build the provider registry from current settings. Entries without a
    /// resolvable credential are skipped (providers validate keys at
    /// construction); `availability()` keeps listing them as unavailable so
    /// clients see configured-but-unauthenticated selections.
    pub fn registry_checked(&self) -> Result<ProviderRegistry, SettingsStoreError> {
        let settings = self.load_checked()?;
        let mut registry = ProviderRegistry::new();
        for entry in &settings.providers {
            let Some(api_key) = self.credential_of(entry)? else {
                continue;
            };
            let config = self.build_provider_config(entry, None, api_key)?;
            let provider = agent_llm::create_provider(config).map_err(|_| {
                SettingsStoreError::ProviderUnavailable {
                    selection: entry.selection.clone(),
                }
            })?;
            registry.register(
                &entry.selection,
                std::sync::Arc::from(provider),
                &entry.model,
            );
        }
        if let Some(default) = settings
            .default_selection
            .as_ref()
            .filter(|id| settings.providers.iter().any(|p| &p.selection == *id))
        {
            registry.set_default(default);
        }
        Ok(registry)
    }

    /// Fail-closed compatibility wrapper. A corrupt settings document yields
    /// an empty registry, so no model call can proceed; typed callers should
    /// use [`Self::registry_checked`].
    pub fn registry(&self) -> ProviderRegistry {
        self.registry_checked()
            .unwrap_or_else(|_| ProviderRegistry::new())
    }

    /// Resolve the effective provider and model from one checked settings
    /// document and one credential lookup. `selection` is the task-level
    /// provider choice; `None` uses the checked document's default. The
    /// returned provider owns its credential-bearing configuration, while the
    /// adjacent snapshot metadata is safe to persist.
    pub fn resolve_provider_for_run(
        &self,
        selection: Option<&str>,
        model_override: Option<&str>,
    ) -> Result<RunProviderResolution, SettingsStoreError> {
        let _guard = self.mutation_guard()?;
        let settings = self.load_checked()?;
        let selected = match selection {
            Some(selection) => selection,
            None => match settings.default_selection.as_deref() {
                Some(selection) => selection,
                None if settings.providers.is_empty() => {
                    return Ok(RunProviderResolution::Unconfigured {
                        settings_revision: settings.revision,
                    });
                }
                None => {
                    return Err(SettingsStoreError::ProviderNotConfigured {
                        selection: "<default>".to_string(),
                    });
                }
            },
        };
        let preset = provider_catalog::find(selected).ok_or_else(|| {
            SettingsStoreError::UnknownProvider {
                selection: selected.to_string(),
            }
        })?;
        let entry = settings
            .providers
            .iter()
            .find(|entry| entry.selection == selected)
            .ok_or_else(|| SettingsStoreError::ProviderNotConfigured {
                selection: selected.to_string(),
            })?;
        let api_key =
            self.credential_of(entry)?
                .ok_or_else(|| SettingsStoreError::MissingCredential {
                    selection: selected.to_string(),
                })?;
        let protocol = self.resolve_protocol(entry)?;
        let base_url = entry
            .base_url
            .clone()
            .unwrap_or_else(|| preset.base_url.to_string());
        let model_id = model_override
            .filter(|model| !model.trim().is_empty())
            .unwrap_or(&entry.model)
            .to_string();
        let config = self.build_provider_config(entry, Some(&model_id), api_key)?;
        let provider = agent_llm::create_provider(config).map_err(|_| {
            SettingsStoreError::ProviderUnavailable {
                selection: selected.to_string(),
            }
        })?;
        let declared = provider.capabilities();
        let mut capabilities = Vec::with_capacity(4);
        if declared.supports_streaming {
            capabilities.push("streaming".to_string());
        }
        if declared.supports_tool_use {
            capabilities.push("tools".to_string());
        }
        if declared.supports_vision {
            capabilities.push("vision".to_string());
        }
        if declared.supports_prompt_caching {
            capabilities.push("prompt-caching".to_string());
        }
        capabilities.sort();

        Ok(RunProviderResolution::Resolved(ResolvedProviderSnapshot {
            snapshot: ProviderSnapshotRef {
                kind: ProviderRouteKind::HostProvider,
                settings_revision: settings.revision,
                provider_id: entry.selection.clone(),
                model_id,
                base_url: Some(base_url),
                protocol: Some(protocol.as_str().to_string()),
                capabilities,
            },
            provider: Arc::from(provider),
        }))
    }

    /// Compatibility projection for callers that need only persistable route
    /// metadata. Runtime dispatch should retain the provider returned by
    /// [`Self::resolve_provider_for_run`] instead of resolving it again.
    pub fn resolve_provider_snapshot(
        &self,
        selection: &str,
        model_override: Option<&str>,
    ) -> Result<ProviderSnapshotRef, SettingsStoreError> {
        match self.resolve_provider_for_run(Some(selection), model_override)? {
            RunProviderResolution::Resolved(resolved) => Ok(resolved.snapshot),
            RunProviderResolution::Unconfigured { .. } => {
                Err(SettingsStoreError::ProviderNotConfigured {
                    selection: "<default>".to_string(),
                })
            }
        }
    }

    /// Map one entry onto an agent-llm provider config via the catalog:
    /// protocol follows the preset (or the explicit override), base_url
    /// falls back to the preset default, vendor identity comes from the
    /// preset id (deepseek / ark / kimi coding specializations).
    fn resolve_protocol(&self, entry: &ProviderEntry) -> Result<Protocol, SettingsStoreError> {
        let preset = provider_catalog::find(&entry.selection).ok_or_else(|| {
            SettingsStoreError::UnknownProvider {
                selection: entry.selection.clone(),
            }
        })?;
        match entry.protocol.as_deref() {
            Some(protocol) => {
                Protocol::parse(protocol).ok_or_else(|| SettingsStoreError::InvalidProtocol {
                    selection: entry.selection.clone(),
                    protocol: protocol.to_string(),
                })
            }
            None => Ok(preset.protocol),
        }
    }

    fn build_provider_config(
        &self,
        entry: &ProviderEntry,
        model_override: Option<&str>,
        api_key: String,
    ) -> Result<agent_llm::ProviderConfig, SettingsStoreError> {
        let preset = provider_catalog::find(&entry.selection).ok_or_else(|| {
            SettingsStoreError::UnknownProvider {
                selection: entry.selection.clone(),
            }
        })?;
        let protocol = self.resolve_protocol(entry)?;
        let base_url = entry
            .base_url
            .clone()
            .unwrap_or_else(|| preset.base_url.to_string());
        let model = model_override.unwrap_or(&entry.model).to_string();
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
        Ok(match protocol {
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
        .availability_checked()
        .map(|rows| rows.iter().any(|row| row.has_credential))
        .unwrap_or(false)
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
        self.store.registry_checked().ok()?.resolve(selection)
    }

    fn default_selection(&self) -> String {
        self.store
            .registry_checked()
            .map(|registry| registry.default_selection())
            .unwrap_or_default()
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
        assert_eq!(store.load(), V1Settings::default());
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
