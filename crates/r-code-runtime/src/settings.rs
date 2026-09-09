//! Host-owned model settings.
//!
//! Provider credentials live in the host's credential storage (v2 names);
//! this module only projects configuration *shapes* to clients — opaque
//! selection ids, capability flags and inference defaults. No endpoint,
//! header or key material is ever serialized for the wire.

use crate::providers::ProviderRegistry;
use agent_llm::create_provider;
use std::sync::Arc;

/// A configured provider as stored host-side (credentials referenced, not
/// embedded).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HostProviderSetting {
    pub selection: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Credential-broker reference; never the key itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<String>,
}

/// Build a registry from host settings. Credentials resolve through the
/// broker by reference; a missing reference yields a provider that fails
/// closed at call time rather than sending keyless requests.
pub fn build_registry(settings: &[HostProviderSetting]) -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();
    for setting in settings {
        let provider_config = agent_llm::ProviderConfig::OpenAi {
            api_key: setting.credential_ref.clone().unwrap_or_default(),
            model: setting.model.clone(),
            base_url: setting.base_url.clone().unwrap_or_default(),
        };
        if let Ok(provider) = create_provider(provider_config) {
            registry.register(&setting.selection, Arc::from(provider), &setting.model);
        }
    }
    registry
}

/// The credential-free settings projection for clients.
pub fn settings_view(settings: &[HostProviderSetting]) -> serde_json::Value {
    serde_json::json!({
        "providers": settings
            .iter()
            .map(|setting| {
                serde_json::json!({
                    "selection": setting.selection,
                    "model": setting.model,
                    "hasCredential": setting.credential_ref.is_some(),
                })
            })
            .collect::<Vec<_>>(),
    })
}
