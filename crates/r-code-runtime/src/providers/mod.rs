//! Provider resolution over agent-llm.
//!
//! The host owns provider configuration and credentials; plugins resolve
//! models by opaque selection string. This module builds providers from
//! host-side configuration only.

use crate::services::models::ProviderResolver;
use agent_contract::provider::LlmProvider;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Host-owned provider registry: selection → provider instance.
pub struct ProviderRegistry {
    providers: BTreeMap<String, (Arc<dyn LlmProvider>, String)>,
    default: String,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: BTreeMap::new(),
            default: String::new(),
        }
    }

    /// Register a provider under an opaque selection (host-side config).
    pub fn register(&mut self, selection: &str, provider: Arc<dyn LlmProvider>, model: &str) {
        self.providers
            .insert(selection.to_string(), (provider, model.to_string()));
        if self.default.is_empty() {
            self.default = selection.to_string();
        }
    }

    pub fn set_default(&mut self, selection: &str) {
        self.default = selection.to_string();
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderResolver for ProviderRegistry {
    fn resolve(&self, selection: &str) -> Option<(Arc<dyn LlmProvider>, String)> {
        self.providers.get(selection).cloned()
    }

    fn default_selection(&self) -> String {
        self.default.clone()
    }
}
