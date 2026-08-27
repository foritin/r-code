use std::path::Path;

use r_code_core::UserFacingError;
use serde::{Deserialize, Serialize};

use super::{browser_tool_contracts, BrowserToolContract, BROWSER_CONTRACT_SCHEMA_VERSION};
use crate::feature_flags::{FeatureFlagService, ProductFeature};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BrowserAgentContract {
    pub schema_version: u32,
    pub tools: Vec<BrowserToolContract>,
}

/// Feature-gated backend entry used by future Browser control surfaces.
pub fn browser_agent_contract(config_dir: &Path) -> Result<BrowserAgentContract, UserFacingError> {
    let flags = FeatureFlagService::new(config_dir.to_path_buf())
        .load()
        .map_err(|error| {
            UserFacingError::new("browser.feature_disabled")
                .with_debug_detail(format!("cannot load Browser feature flag: {error}"))
        })?;
    flags.require(ProductFeature::Browser)?;
    Ok(BrowserAgentContract {
        schema_version: BROWSER_CONTRACT_SCHEMA_VERSION,
        tools: browser_tool_contracts(),
    })
}
