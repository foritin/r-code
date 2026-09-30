//! Plugin catalog and version lifecycle.
//!
//! Persists installed/enabled versions, granted services and configuration;
//! derives availability from platform/protocol/entrypoint checks; and is the
//! only writer of the transport's child-process requirement table (P23.1), so
//! a launch inherits what the pinned manifest declared instead of trusting an
//! in-process guess. Upgrades apply to new runs only — existing attempts keep
//! their pinned package — and packages referenced by active or recoverable
//! runs cannot be removed.

use crate::plugins::package::{InstallError, InstalledPackage, PackageInstaller};
use crate::plugins::transport::register_child_process_requirement;
use r_code_harness_protocol::{ApiVersion, HarnessManifest, HostService, PackageRef, Platform};
use r_code_store::v1::{PluginCatalogRecord, V1Store};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Host API version the catalog negotiates against. P19A advanced the minor
/// to 2 (WorkUnit effect/network fields) and P23.4 advances it to the final
/// Wave 3 minor 3 (the single-process guarantee); the major stays 1 and older
/// 1.x packages keep negotiating additively — a package only needs a newer
/// minor when it actually asks for what that minor introduced. The value comes
/// from the protocol crate's Wave 3 constant, which `rpc::HOST_API_VERSION`
/// must carry identically.
pub const HOST_API: ApiVersion = r_code_harness_protocol::manifest::WAVE3_HOST_API;

/// The minimum apiMinor a package must declare to emit WorkUnit
/// effect/network fields (P19A.4): a package carrying those fields
/// cannot declare an older minor and stay compatible.
pub const EFFECT_FIELDS_MIN_API_MINOR: u32 = 2;

/// The minimum apiMinor a package must declare to require the single-process
/// guarantee (P23.4). Re-declared here so the refusal the catalog gives and
/// the refusal the manifest model gives name the same floor.
pub const SINGLE_PROCESS_MIN_API_MINOR: u32 =
    r_code_harness_protocol::manifest::SINGLE_PROCESS_MIN_API_MINOR;

/// Why a catalog entry is not usable right now. Explicit, never guessed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub enum UnavailableReason {
    Disabled,
    IncompatibleApi {
        required_major: u32,
        required_minor: u32,
    },
    /// P23.4: the package asks for the single-process guarantee while declaring
    /// an apiMinor that predates it. Distinct from `IncompatibleApi` because it
    /// is a downgrade refusal, not a version-window mismatch: an older additive
    /// package stays available.
    SingleProcessNeedsNewerApi {
        declared_major: u32,
        declared_minor: u32,
    },
    MissingEntrypoint {
        executable: String,
    },
}

impl UnavailableReason {
    /// Stable machine-checkable token for the reason (packaging checks and the
    /// client surfaces match on this, never on the Display text).
    pub fn code(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::IncompatibleApi { .. } => "incompatible-api",
            Self::SingleProcessNeedsNewerApi { .. } => "single-process-needs-1.3",
            Self::MissingEntrypoint { .. } => "missing-entrypoint",
        }
    }
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
    /// P23.4: the package requires the single-process guarantee while declaring
    /// an apiMinor that predates it. Named, additive refusal: `code` is the
    /// machine-checkable token and never a paraphrase of `IncompatibleApi`.
    #[error("harness {id} requires the single-process guarantee but declares api {declared_major}.{declared_minor}: {code}")]
    SingleProcessDowngrade {
        id: String,
        declared_major: u32,
        declared_minor: u32,
        code: &'static str,
    },
}

/// The catalog over an installer and the v1 store.
pub struct PluginCatalog {
    installer: PackageInstaller,
    store: Arc<V1Store>,
}

impl PluginCatalog {
    pub fn new(plugins_root: impl Into<PathBuf>, store: Arc<V1Store>) -> Self {
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
        // P23.4: the install path is where the downgrade refusal lands, and it
        // lands before any registry row or launch binding exists. The refusal
        // names the package and the declaration; derive_availability keeps the
        // same verdict for rows written by an older build, so a stale store
        // cannot smuggle the package in either.
        if let Some(refusal) = single_process_refusal(&installed.manifest) {
            return Err(refusal);
        }
        bind_manifest_requirements(&installed.manifest, &installed.install_dir);
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

    /// Persist that one exact immutable package came from the trusted
    /// built-in registration path. Package ids and manifests alone are not a
    /// trust signal.
    pub fn mark_builtin(&self, package: &PackageRef) -> Result<(), CatalogError> {
        let marked = self
            .store
            .mark_plugin_builtin(
                &package.id.0,
                &package.version.to_string(),
                &package.content_digest,
            )
            .map_err(|error| CatalogError::Store(error.to_string()))?;
        if !marked {
            return Err(CatalogError::UnknownPackage {
                id: package.id.0.clone(),
                digest: package.content_digest.clone(),
            });
        }
        Ok(())
    }

    /// Check the trusted built-in marker for this exact version and digest.
    pub fn is_builtin(&self, package: &PackageRef) -> Result<bool, CatalogError> {
        self.store
            .plugin_is_builtin(
                &package.id.0,
                &package.version.to_string(),
                &package.content_digest,
            )
            .map_err(|error| CatalogError::Store(error.to_string()))
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
        // The catalog re-binds the declared child-process requirement on every
        // read, so a daemon restart that never re-installs still cannot launch a
        // declared single-process package as an ordinary contained tree.
        bind_manifest_requirements(&manifest, Path::new(&record.install_dir));
        let availability = if !record.enabled {
            // A disabled entry is disabled first: the reason a run cannot use it
            // must never be inferred from an older eligibility verdict.
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
        let removed_entry = self
            .list()?
            .into_iter()
            .find(|entry| {
                entry.manifest.id.0 == id && entry.package_ref.content_digest == content_digest
            })
            .map(|entry| (entry.manifest, entry.install_dir));
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
        // The bytes are gone, so the launch declaration that came with them must
        // go too: a stale binding could otherwise outlive the package it named.
        if let Some((manifest, install_dir)) = removed_entry {
            unbind_manifest_requirements(&manifest, &install_dir);
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
    // P23.4 first and explicitly: requiring a guarantee this host cannot honour
    // is a downgrade refusal, not a version-window miss.
    if single_process_refusal(manifest).is_some() {
        return Availability::Unavailable(UnavailableReason::SingleProcessNeedsNewerApi {
            declared_major: manifest.api_major,
            declared_minor: manifest.api_minor,
        });
    }
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

/// The P23.4 downgrade refusal for one manifest, or `None` when the package is
/// eligible. A package that never asks for the single-process guarantee — every
/// additive pre-1.3 manifest — is always eligible on this arm, which is what
/// keeps older packages installable while the guarantee itself is refused.
fn single_process_refusal(manifest: &HarnessManifest) -> Option<CatalogError> {
    let honoured = manifest.api_major == 1 && manifest.api_minor >= SINGLE_PROCESS_MIN_API_MINOR;
    if manifest.requires_single_process && !honoured {
        return Some(CatalogError::SingleProcessDowngrade {
            id: manifest.id.0.clone(),
            declared_major: manifest.api_major,
            declared_minor: manifest.api_minor,
            code: UnavailableReason::SingleProcessNeedsNewerApi {
                declared_major: manifest.api_major,
                declared_minor: manifest.api_minor,
            }
            .code(),
        });
    }
    None
}

/// Publish the manifest's child-process declaration to the transport for every
/// executable the package declares on this host. The declaration is enforced,
/// never trusted: an executable with no binding at all is treated as undeclared
/// and only ever launched inside a contained tree (and refused on macOS).
fn bind_manifest_requirements(manifest: &HarnessManifest, install_dir: &Path) {
    for entry in &manifest.supported_platforms {
        if entry.platform != Platform::current() {
            continue;
        }
        register_child_process_requirement(
            &install_dir.join(&entry.executable),
            manifest.requires_single_process,
        );
    }
}

/// Retract the bindings of a package being removed from the registry.
fn unbind_manifest_requirements(manifest: &HarnessManifest, install_dir: &Path) {
    for entry in &manifest.supported_platforms {
        if entry.platform != Platform::current() {
            continue;
        }
        register_child_process_requirement(&install_dir.join(&entry.executable), false);
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
            requires_effect_fields: false,
            requires_single_process: false,
        }
    }
}
