//! Runtime flavor and application-owned storage boundaries.
//!
//! `cargo tauri dev` is compiled without the crate's `custom-protocol` feature,
//! while packaged builds enable it. That Tauri-native boundary selects a fully
//! isolated Development or Production identity before logging, SQLite, MCP,
//! Codex, credentials, or the single-instance plugin can touch persistent state.

use std::io;
use std::path::{Path, PathBuf};

pub const DEFAULT_BUNDLE_IDENTIFIER: &str = "com.r-code.app";
pub const DEFAULT_DEV_BUNDLE_IDENTIFIER: &str = "com.r-code.app.dev";
pub const MACOS_BUNDLE_IDENTIFIER: &str = "com.rcode.desktop";
pub const MACOS_DEV_BUNDLE_IDENTIFIER: &str = "com.rcode.desktop.dev";

pub const PRODUCTION_PRODUCT_NAME: &str = "R-Code";
pub const DEVELOPMENT_PRODUCT_NAME: &str = "R-Code Dev";

pub const PRODUCTION_CREDENTIAL_SERVICE: &str = "r-code";
pub const DEVELOPMENT_CREDENTIAL_SERVICE: &str = "r-code-dev";

pub const PRODUCTION_UPDATE_ENDPOINT: &str =
    "https://github.com/foritin/r-code/releases/latest/download/latest.json";
pub const DEVELOPMENT_UPDATE_ENDPOINT: &str =
    "https://github.com/foritin/r-code/releases/latest/download/dev-latest.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppFlavor {
    Production,
    Development,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevelopmentEnvironment {
    pub data_dir: PathBuf,
    pub codex_home: PathBuf,
    pub npm_prefix: PathBuf,
}

impl AppFlavor {
    /// Tauri disables the default custom protocol for every `tauri dev` run,
    /// including release-profile dev runs. Packaged builds retain the feature.
    pub const fn current() -> Self {
        if cfg!(feature = "custom-protocol") {
            Self::Production
        } else {
            Self::Development
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Production => "production",
            Self::Development => "development",
        }
    }

    pub const fn product_name(self) -> &'static str {
        match self {
            Self::Production => PRODUCTION_PRODUCT_NAME,
            Self::Development => DEVELOPMENT_PRODUCT_NAME,
        }
    }

    pub const fn bundle_identifier(self) -> &'static str {
        match (self, cfg!(target_os = "macos")) {
            (Self::Production, true) => MACOS_BUNDLE_IDENTIFIER,
            (Self::Development, true) => MACOS_DEV_BUNDLE_IDENTIFIER,
            (Self::Production, false) => DEFAULT_BUNDLE_IDENTIFIER,
            (Self::Development, false) => DEFAULT_DEV_BUNDLE_IDENTIFIER,
        }
    }

    pub const fn credential_service_name(self) -> &'static str {
        match self {
            Self::Production => PRODUCTION_CREDENTIAL_SERVICE,
            Self::Development => DEVELOPMENT_CREDENTIAL_SERVICE,
        }
    }

    pub const fn updater_endpoint(self) -> &'static str {
        match self {
            Self::Production => PRODUCTION_UPDATE_ENDPOINT,
            Self::Development => DEVELOPMENT_UPDATE_ENDPOINT,
        }
    }

    pub fn data_dir_under(self, root: &Path) -> PathBuf {
        root.join(self.bundle_identifier()).join("r-code")
    }

    pub fn default_data_dir(self) -> Option<PathBuf> {
        dirs::data_dir().map(|root| self.data_dir_under(&root))
    }

    pub fn development_environment(self) -> Option<DevelopmentEnvironment> {
        if self != Self::Development {
            return None;
        }
        let data_dir = self.default_data_dir()?;
        let integrations = data_dir.join("integrations");
        Some(DevelopmentEnvironment {
            data_dir,
            codex_home: integrations.join("codex"),
            npm_prefix: integrations.join("npm"),
        })
    }

    /// Apply the selected identity to the generated Tauri context before any
    /// plugin initializes. The Windows single-instance mutex and WebView data
    /// directory both derive from this identifier.
    pub fn apply_to_tauri_config(self, config: &mut tauri::Config) -> Result<(), String> {
        if self == Self::Development {
            config.product_name = Some(self.product_name().to_string());
            config.identifier = self.bundle_identifier().to_string();
            for window in &mut config.app.windows {
                if window.label == "main" {
                    window.title = self.product_name().to_string();
                }
            }

            let updater = config
                .plugins
                .0
                .entry("updater".to_string())
                .or_insert_with(|| serde_json::json!({}));
            if !updater.is_object() {
                *updater = serde_json::json!({});
            }
            let updater = updater
                .as_object_mut()
                .expect("updater config was normalized to an object");
            updater.insert("active".to_string(), serde_json::Value::Bool(true));
            updater.insert(
                "endpoints".to_string(),
                serde_json::json!([self.updater_endpoint()]),
            );
        }

        if config.identifier != self.bundle_identifier() {
            return Err(format!(
                "{} runtime refused mismatched Tauri identifier '{}' (expected '{}')",
                self.product_name(),
                config.identifier,
                self.bundle_identifier()
            ));
        }
        Ok(())
    }

    /// Keep every external CLI state mutation made by R-Code Dev inside its own
    /// AppData tree. Environment changes are process-local and are inherited only
    /// by terminals and CLI children launched from the development process.
    pub fn prepare_process_environment(self) -> io::Result<()> {
        let Some(environment) = self.development_environment() else {
            return Ok(());
        };
        for directory in [
            &environment.data_dir,
            &environment.codex_home,
            &environment.npm_prefix,
        ] {
            std::fs::create_dir_all(directory)?;
        }
        std::env::set_var("R_CODE_APP_FLAVOR", self.as_str());
        std::env::set_var("R_CODE_DATA_DIR", &environment.data_dir);
        std::env::set_var("CODEX_HOME", &environment.codex_home);
        std::env::set_var("NPM_CONFIG_PREFIX", &environment.npm_prefix);
        Ok(())
    }
}

pub const fn bundle_identifier() -> &'static str {
    AppFlavor::current().bundle_identifier()
}

pub const fn production_bundle_identifier() -> &'static str {
    AppFlavor::Production.bundle_identifier()
}

pub const fn product_name() -> &'static str {
    AppFlavor::current().product_name()
}

pub const fn credential_service_name() -> &'static str {
    AppFlavor::current().credential_service_name()
}

/// Same default root as `app.path().app_data_dir()?.join("r-code")` for the
/// currently compiled flavor.
pub fn default_data_dir() -> Option<PathBuf> {
    AppFlavor::current().default_data_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn production_identifiers_match_tauri_configs() {
        let common: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let macos: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.macos.conf.json")).unwrap();

        assert_eq!(
            common["identifier"].as_str(),
            Some(DEFAULT_BUNDLE_IDENTIFIER)
        );
        assert_eq!(macos["identifier"].as_str(), Some(MACOS_BUNDLE_IDENTIFIER));
    }

    #[test]
    fn production_and_development_namespaces_are_disjoint() {
        let root = Path::new("app-data");
        assert_ne!(
            AppFlavor::Production.bundle_identifier(),
            AppFlavor::Development.bundle_identifier()
        );
        assert_ne!(
            AppFlavor::Production.credential_service_name(),
            AppFlavor::Development.credential_service_name()
        );
        assert_ne!(
            AppFlavor::Production.updater_endpoint(),
            AppFlavor::Development.updater_endpoint()
        );
        assert_ne!(
            AppFlavor::Production.data_dir_under(root),
            AppFlavor::Development.data_dir_under(root)
        );
    }

    #[test]
    fn development_runtime_rewrites_every_tauri_owned_identity() {
        let mut config: tauri::Config =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        AppFlavor::Development
            .apply_to_tauri_config(&mut config)
            .unwrap();

        assert_eq!(
            config.product_name.as_deref(),
            Some(DEVELOPMENT_PRODUCT_NAME)
        );
        assert_eq!(
            config.identifier,
            AppFlavor::Development.bundle_identifier()
        );
        assert_eq!(config.app.windows[0].title, DEVELOPMENT_PRODUCT_NAME);
        let updater = &config.plugins.0["updater"];
        assert_eq!(updater["active"], true);
        assert_eq!(
            updater["endpoints"][0],
            AppFlavor::Development.updater_endpoint()
        );
    }

    #[test]
    fn production_runtime_rejects_a_development_identifier() {
        let mut config: tauri::Config =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        config.identifier = AppFlavor::Development.bundle_identifier().to_string();
        assert!(AppFlavor::Production
            .apply_to_tauri_config(&mut config)
            .is_err());
    }

    #[test]
    fn current_flavor_follows_tauri_custom_protocol_boundary() {
        let expected = if cfg!(feature = "custom-protocol") {
            AppFlavor::Production
        } else {
            AppFlavor::Development
        };
        assert_eq!(AppFlavor::current(), expected);
    }

    #[test]
    fn default_data_dir_uses_the_current_flavor_identifier() {
        let path = default_data_dir().expect("the test platform should expose a data directory");
        let rendered = path.to_string_lossy().replace('\\', "/");
        assert!(rendered.ends_with(&format!("{}/r-code", bundle_identifier())));
    }

    #[test]
    fn development_external_cli_state_stays_below_development_app_data() {
        let environment = AppFlavor::Development
            .development_environment()
            .expect("the test platform should expose a data directory");
        assert!(environment.codex_home.starts_with(&environment.data_dir));
        assert!(environment.npm_prefix.starts_with(&environment.data_dir));
        assert_ne!(
            Some(environment.data_dir),
            AppFlavor::Production.default_data_dir()
        );
    }
}
