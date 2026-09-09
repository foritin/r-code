//! Plugin catalog and version lifecycle.
//!
//! Persists installed/enabled versions, granted services and configuration;
//! derives availability from platform/protocol/entrypoint checks. Upgrades
//! apply to new runs only — existing attempts keep their pinned package —
//! and packages referenced by active or recoverable runs cannot be removed.

use crate::plugins::package::{InstallError, InstalledPackage, PackageInstaller};
use r_code_harness_protocol::{ApiVersion, HarnessManifest, HostService, PackageRef, Platform};
use r_code_store::v2::{PluginCatalogRecord, V2Store};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Host API version the catalog negotiates against.
pub const HOST_API: ApiVersion = ApiVersion::new(1, 0);

/// Why a catalog entry is not usable right now. Explicit, never guessed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum UnavailableReason {
    Disabled,
    IncompatibleApi {
        required_major: u32,
        required_minor: u32,
    },
    MissingEntrypoint {
        executable: String,
    },
}

/// Availability of one catalog entry.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum Availability {
    Available,
    Unavailable(UnavailableReason),
}

/// One catalog entry as seen by clients.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CatalogEntry {
    pub manifest: HarnessManifest,
    pub package_ref: PackageRef,
    pub enabled: bool,
    pub install_dir: PathBuf,
    pub availability: Availability,
}

/// Errors from catalog operations.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CatalogError {
    #[error(transparent)]
    Install(#[from] InstallError),
    #[error("store failure: {0}")]
    Store(String),
    #[error("package {id} ({digest}) is pinned by attempts {pinned_by:?} and cannot be removed")]
    RemovePinned {
        id: String,
        digest: String,
        pinned_by: Vec<String>,
    },
    #[error("package {id} ({digest}) not found")]
    UnknownPackage { id: String, digest: String },
    #[error("harness {0} has no enabled, available version")]
    NoUsableVersion(String),
}

/// The catalog over an installer and the v2 store.
pub struct PluginCatalog {
    installer: PackageInstaller,
    store: Arc<V2Store>,
}

impl PluginCatalog {
    pub fn new(plugins_root: impl Into<PathBuf>, store: Arc<V2Store>) -> Self {
        Self {
            installer: PackageInstaller::new(plugins_root),
            store,
        }
    }

    /// Install a directory package and register it.
    pub fn install_from_directory(&self, source: &Path) -> Result<InstalledPackage, CatalogError> {
        let installed = self.installer.install_from_directory(source)?;
        self.register(&installed)?;
        Ok(installed)
    }

    /// Install a zip package and register it.
    pub fn install_from_zip(&self, zip_path: &Path) -> Result<InstalledPackage, CatalogError> {
        let installed = self.installer.install_from_zip(zip_path)?;
        self.register(&installed)?;
        Ok(installed)
    }

    fn register(&self, installed: &InstalledPackage) -> Result<(), CatalogError> {
        let record = PluginCatalogRecord {
            id: installed.manifest.id.0.clone(),
            version: installed.manifest.version.to_string(),
            content_digest: installed.package_ref.content_digest.clone(),
            enabled: true,
            granted_services: installed
                .manifest
                .requested_host_services
                .iter()
                .map(|service| service.wire_name().to_string())
                .collect(),
            config: "{}".into(),
            manifest_json: serde_json::to_string(&installed.manifest)
                .map_err(|e| CatalogError::Store(e.to_string()))?,
            install_dir: installed.install_dir.to_string_lossy().into_owned(),
        };
        self.store
            .register_plugin(&record)
            .map_err(|e| CatalogError::Store(e.to_string()))?;
        Ok(())
    }

    /// List all entries with derived availability.
    pub fn list(&self) -> Result<Vec<CatalogEntry>, CatalogError> {
        let records = self
            .store
            .list_plugins()
            .map_err(|e| CatalogError::Store(e.to_string()))?;
        Ok(records
            .into_iter()
            .map(|record| self.entry_of(record))
            .collect())
    }

    fn entry_of(&self, record: PluginCatalogRecord) -> CatalogEntry {
        let manifest: HarnessManifest = serde_json::from_str(&record.manifest_json)
            .unwrap_or_else(|_| fallback_manifest(&record.id));
        let package_ref = PackageRef {
            id: r_code_harness_protocol::HarnessId::new(&record.id),
            version: record
                .version
                .parse()
                .unwrap_or(semver::Version::new(0, 0, 0)),
            content_digest: record.content_digest.clone(),
        };
        let availability = if !record.enabled {
            Availability::Unavailable(UnavailableReason::Disabled)
        } else {
            derive_availability(&manifest, Path::new(&record.install_dir))
        };
        CatalogEntry {
            manifest,
            package_ref,
            enabled: record.enabled,
            install_dir: PathBuf::from(&record.install_dir),
            availability,
        }
    }

    /// Reversibly disable/enable one installed package. Never touches run
    /// pins: already-running attempts keep their bytes.
    pub fn set_enabled(
        &self,
        id: &str,
        content_digest: &str,
        enabled: bool,
    ) -> Result<(), CatalogError> {
        let changed = self
            .store
            .set_plugin_enabled(id, content_digest, enabled)
            .map_err(|e| CatalogError::Store(e.to_string()))?;
        if !changed {
            return Err(CatalogError::UnknownPackage {
                id: id.into(),
                digest: content_digest.into(),
            });
        }
        Ok(())
    }

    /// Remove a package unless an active or recoverable run pins it.
    pub fn remove(&self, id: &str, content_digest: &str) -> Result<(), CatalogError> {
        let pins = self
            .store
            .plugin_pins_for(id, content_digest)
            .map_err(|e| CatalogError::Store(e.to_string()))?;
        if !pins.is_empty() {
            return Err(CatalogError::RemovePinned {
                id: id.into(),
                digest: content_digest.into(),
                pinned_by: pins.into_iter().map(|pin| pin.attempt_id).collect(),
            });
        }
        let changed = self
            .store
            .remove_plugin(id, content_digest)
            .map_err(|e| CatalogError::Store(e.to_string()))?;
        if !changed {
            return Err(CatalogError::UnknownPackage {
                id: id.into(),
                digest: content_digest.into(),
            });
        }
        Ok(())
    }

    /// Pin the exact package bytes for an attempt.
    pub fn pin(
        &self,
        attempt_id: &str,
        task_id: &str,
        package: &PackageRef,
    ) -> Result<(), CatalogError> {
        self.store
            .pin_plugin(
                attempt_id,
                task_id,
                &package.id.0,
                &package.version.to_string(),
                &package.content_digest,
            )
            .map_err(|e| CatalogError::Store(e.to_string()))?;
        Ok(())
    }

    /// The pinned package of an attempt (stable across upgrades).
    pub fn pinned_package(&self, attempt_id: &str) -> Result<Option<PackageRef>, CatalogError> {
        Ok(self
            .store
            .plugin_pin_for_attempt(attempt_id)
            .map_err(|e| CatalogError::Store(e.to_string()))?
            .map(|pin| PackageRef {
                id: r_code_harness_protocol::HarnessId::new(pin.id),
                version: pin.version.parse().unwrap_or(semver::Version::new(0, 0, 0)),
                content_digest: pin.content_digest,
            }))
    }

    /// The newest enabled and available version of a harness — what new runs
    /// select. Existing pins are unaffected.
    pub fn effective_package(&self, id: &str) -> Result<PackageRef, CatalogError> {
        let mut candidates: Vec<CatalogEntry> = self
            .list()?
            .into_iter()
            .filter(|entry| {
                entry.manifest.id.0 == id && entry.availability == Availability::Available
            })
            .collect();
        candidates.sort_by(|a, b| b.manifest.version.cmp(&a.manifest.version));
        candidates
            .into_iter()
            .next()
            .map(|entry| entry.package_ref)
            .ok_or_else(|| CatalogError::NoUsableVersion(id.to_string()))
    }
}

/// Availability from manifest/platform/entrypoint facts.
fn derive_availability(manifest: &HarnessManifest, install_dir: &Path) -> Availability {
    let plugin_api = ApiVersion::new(manifest.api_major, manifest.api_minor);
    if !plugin_api.is_supported_by(&HOST_API) {
        return Availability::Unavailable(UnavailableReason::IncompatibleApi {
            required_major: manifest.api_major,
            required_minor: manifest.api_minor,
        });
    }
    match manifest.entrypoint_for(Platform::current()) {
        None => Availability::Unavailable(UnavailableReason::MissingEntrypoint {
            executable: "<none for platform>".into(),
        }),
        Some(entry) => {
            let executable = install_dir.join(&entry.executable);
            let exists = executable.is_file()
                || (cfg!(windows)
                    && install_dir
                        .join(format!("{}.exe", entry.executable))
                        .is_file());
            if exists {
                Availability::Available
            } else {
                Availability::Unavailable(UnavailableReason::MissingEntrypoint {
                    executable: entry.executable.clone(),
                })
            }
        }
    }
}

fn fallback_manifest(id: &str) -> HarnessManifest {
    HarnessManifestBuilderShim::build(id)
}

struct HarnessManifestBuilderShim;

impl HarnessManifestBuilderShim {
    fn build(id: &str) -> HarnessManifest {
        HarnessManifest {
            schema_version: "1".into(),
            id: r_code_harness_protocol::HarnessId::new(id),
            version: semver::Version::new(0, 0, 0),
            api_major: 0,
            api_minor: 0,
            display_name: id.into(),
            description: None,
            supported_platforms: vec![],
            supported_features: vec![],
            requested_host_services: vec![HostService::ToolsList],
            config_schema: serde_json::json!({}),
            process_profiles: vec![],
        }
    }
}
