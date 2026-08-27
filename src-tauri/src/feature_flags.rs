use std::path::PathBuf;

use r_code_core::error::ProductError;
use r_code_core::UserFacingError;
use serde::{Deserialize, Serialize};

const FEATURE_FLAGS_FILE: &str = "features.toml";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProductFeature {
    Browser,
    Automation,
}

impl ProductFeature {
    pub const fn key(self) -> &'static str {
        match self {
            Self::Browser => "browser",
            Self::Automation => "automation",
        }
    }

    pub const fn disabled_error_code(self) -> &'static str {
        match self {
            Self::Browser => "browser.feature_disabled",
            Self::Automation => "automation.feature_disabled",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductFeatureFlags {
    #[serde(default)]
    pub browser_enabled: bool,
    #[serde(default)]
    pub automation_enabled: bool,
}

impl ProductFeatureFlags {
    pub const fn is_enabled(self, feature: ProductFeature) -> bool {
        match feature {
            ProductFeature::Browser => self.browser_enabled,
            ProductFeature::Automation => self.automation_enabled,
        }
    }

    /// Server-side gate used by every feature command. Hiding a frontend route is never treated
    /// as an authorization boundary.
    pub fn require(self, feature: ProductFeature) -> Result<(), UserFacingError> {
        if self.is_enabled(feature) {
            Ok(())
        } else {
            Err(UserFacingError::new(feature.disabled_error_code()))
        }
    }
}

#[derive(Debug, Clone)]
pub struct FeatureFlagService {
    config_dir: PathBuf,
}

impl FeatureFlagService {
    pub fn new(config_dir: PathBuf) -> Self {
        Self { config_dir }
    }

    pub fn path(&self) -> PathBuf {
        self.config_dir.join(FEATURE_FLAGS_FILE)
    }

    pub fn load(&self) -> Result<ProductFeatureFlags, ProductError> {
        let path = self.path();
        if !path.exists() {
            return Ok(ProductFeatureFlags::default());
        }
        let content = std::fs::read_to_string(&path).map_err(|error| {
            ProductError::ConfigError(format!("read {}: {error}", path.display()))
        })?;
        toml::from_str(&content).map_err(|error| {
            ProductError::ConfigError(format!("parse {}: {error}", path.display()))
        })
    }

    pub fn save(&self, flags: ProductFeatureFlags) -> Result<(), ProductError> {
        std::fs::create_dir_all(&self.config_dir)?;
        let content = toml::to_string_pretty(&flags).map_err(|error| {
            ProductError::ConfigError(format!("serialize feature flags: {error}"))
        })?;
        std::fs::write(self.path(), content)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_config_dir(label: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(&format!("r-code-feature-flags-{label}-"))
            .tempdir()
            .expect("create feature flag temp directory")
    }

    #[test]
    fn defaults_keep_unfinished_features_disabled() {
        let temp = temp_config_dir("defaults");
        let service = FeatureFlagService::new(temp.path().to_path_buf());

        let flags = service.load().expect("load absent feature flags");

        assert_eq!(flags, ProductFeatureFlags::default());
        assert!(!flags.is_enabled(ProductFeature::Browser));
        assert!(!flags.is_enabled(ProductFeature::Automation));
    }

    #[test]
    fn persisted_flags_round_trip() {
        let temp = temp_config_dir("round-trip");
        let service = FeatureFlagService::new(temp.path().to_path_buf());
        let expected = ProductFeatureFlags {
            browser_enabled: true,
            automation_enabled: false,
        };

        service.save(expected).expect("save feature flags");

        assert_eq!(service.load().expect("reload feature flags"), expected);
        assert!(expected.require(ProductFeature::Browser).is_ok());
        assert_eq!(
            expected
                .require(ProductFeature::Automation)
                .expect_err("automation remains disabled")
                .code,
            "automation.feature_disabled"
        );
    }

    #[test]
    fn missing_fields_remain_disabled_for_forward_compatible_configs() {
        let temp = temp_config_dir("missing-fields");
        let service = FeatureFlagService::new(temp.path().to_path_buf());
        std::fs::write(service.path(), "browser_enabled = true\n")
            .expect("write partial feature flag config");

        let flags = service.load().expect("load partial feature flags");

        assert!(flags.browser_enabled);
        assert!(!flags.automation_enabled);
    }
}
