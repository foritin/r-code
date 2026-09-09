//! Plugin package installation.
//!
//! Local directory or ZIP sources are validated and copied into immutable
//! `plugins/<id>/<version>/<digest>` directories. Installation never executes
//! anything: no install hooks, no package managers, no repository manifests.
//! Path traversal, symlink/reparse escapes, missing entrypoints and duplicate
//! identities with different bytes are rejected before any registry entry
//! exists.

use r_code_harness_protocol::{
    HarnessManifest, ManifestError, PackageRef, Platform, PlatformEntrypoint,
    MANIFEST_SCHEMA_VERSION,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Errors during package installation.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum InstallError {
    #[error("io failure: {0}")]
    Io(String),
    #[error("zip failure: {0}")]
    Zip(String),
    #[error("harness.json is missing or unreadable")]
    MissingManifest,
    #[error("manifest rejected: {0}")]
    InvalidManifest(#[from] ManifestError),
    #[error("archive entry {0:?} escapes the package root")]
    TraversalEntry(String),
    #[error("package contains a symlink/reparse escape at {0:?}")]
    LinkEscape(String),
    #[error("no entrypoint for platform {platform:?} (manifest declares {declared:?})")]
    MissingEntrypoint {
        platform: String,
        declared: Vec<String>,
    },
    #[error(
        "identity {id} v{version} already installed with different bytes ({existing} vs {found})"
    )]
    DuplicateIdentity {
        id: String,
        version: String,
        existing: String,
        found: String,
    },
}

/// A validated, installed package.
#[derive(Debug, Clone, PartialEq)]
pub struct InstalledPackage {
    pub manifest: HarnessManifest,
    pub package_ref: PackageRef,
    pub install_dir: PathBuf,
    /// Absolute path of the platform entrypoint executable.
    pub executable: PathBuf,
    pub argv: Vec<String>,
}

/// Installer writing beneath a profile's `plugins` root.
pub struct PackageInstaller {
    plugins_root: PathBuf,
}

impl PackageInstaller {
    pub fn new(plugins_root: impl Into<PathBuf>) -> Self {
        Self {
            plugins_root: plugins_root.into(),
        }
    }

    pub fn plugins_root(&self) -> &Path {
        &self.plugins_root
    }

    /// Install from a local directory package.
    pub fn install_from_directory(&self, source: &Path) -> Result<InstalledPackage, InstallError> {
        let staging = self.staging_dir();
        self.reset_dir(&staging)?;
        let copy_result = copy_tree_no_links(source, &staging);
        if let Err(error) = copy_result {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
        self.finish_install(staging)
    }

    /// Install from a ZIP package.
    pub fn install_from_zip(&self, zip_path: &Path) -> Result<InstalledPackage, InstallError> {
        let staging = self.staging_dir();
        self.reset_dir(&staging)?;
        let extract_result = extract_zip_no_escape(zip_path, &staging);
        if let Err(error) = extract_result {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
        self.finish_install(staging)
    }

    fn staging_dir(&self) -> PathBuf {
        self.plugins_root
            .join(format!(".staging-{}", uuid::Uuid::new_v4().simple()))
    }

    fn reset_dir(&self, dir: &Path) -> Result<(), InstallError> {
        fs::create_dir_all(dir).map_err(|e| InstallError::Io(e.to_string()))?;
        Ok(())
    }

    /// Validate staged content and move it into its immutable final location.
    fn finish_install(&self, staging: PathBuf) -> Result<InstalledPackage, InstallError> {
        let result = self.finish_install_inner(&staging);
        // Staging never survives an install, successful or not.
        let _ = fs::remove_dir_all(&staging);
        result
    }

    fn finish_install_inner(&self, staging: &Path) -> Result<InstalledPackage, InstallError> {
        let manifest_path = staging.join("harness.json");
        let manifest_text =
            fs::read_to_string(&manifest_path).map_err(|_| InstallError::MissingManifest)?;
        let manifest: HarnessManifest = serde_json::from_str(&manifest_text).map_err(|e| {
            InstallError::InvalidManifest(ManifestError::InvalidVersion(e.to_string()))
        })?;
        manifest.validate()?;

        let platform = Platform::current();
        let entrypoint = resolve_entrypoint(&manifest, staging, platform)?;

        // Content digest over every regular file, sorted by path.
        let digest = content_digest(staging)?;
        let short = &digest[..16.min(digest.len())];
        let version_dir = self
            .plugins_root
            .join(&manifest.id.0)
            .join(manifest.version.to_string());
        let install_dir = version_dir.join(short);

        // Reject the same identity with different bytes.
        if version_dir.is_dir() {
            let mut entries =
                fs::read_dir(&version_dir).map_err(|e| InstallError::Io(e.to_string()))?;
            if let Some(existing) = entries.next() {
                let existing = existing.map_err(|e| InstallError::Io(e.to_string()))?;
                if existing.file_name().to_string_lossy() == *short {
                    // Same bytes: idempotent reinstall; refresh staging away.
                    return self.materialize(manifest, entrypoint, install_dir, digest);
                }
                return Err(InstallError::DuplicateIdentity {
                    id: String::new(),
                    version: String::new(),
                    existing: existing.file_name().to_string_lossy().into_owned(),
                    found: short.to_string(),
                }
                .with_identity(&manifest));
            }
        }

        fs::create_dir_all(&version_dir).map_err(|e| InstallError::Io(e.to_string()))?;
        fs::rename(staging, &install_dir).map_err(|e| InstallError::Io(e.to_string()))?;
        self.materialize(manifest, entrypoint, install_dir, digest)
    }

    fn materialize(
        &self,
        manifest: HarnessManifest,
        entrypoint: PlatformEntrypoint,
        install_dir: PathBuf,
        digest: String,
    ) -> Result<InstalledPackage, InstallError> {
        let executable = install_dir.join(&entrypoint.executable);
        make_executable(&executable)?;
        Ok(InstalledPackage {
            package_ref: PackageRef {
                id: manifest.id.clone(),
                version: manifest.version.clone(),
                content_digest: digest,
            },
            manifest,
            executable,
            argv: entrypoint.argv,
            install_dir,
        })
    }
}

impl InstallError {
    fn with_identity(self, manifest: &HarnessManifest) -> InstallError {
        match self {
            InstallError::DuplicateIdentity {
                existing, found, ..
            } => InstallError::DuplicateIdentity {
                id: manifest.id.0.clone(),
                version: manifest.version.to_string(),
                existing,
                found,
            },
            other => other,
        }
    }
}

/// Resolve the current-platform entrypoint, tolerating a missing `.exe`
/// suffix on Windows. Returns the entrypoint with the *existing* relative
/// executable path.
fn resolve_entrypoint(
    manifest: &HarnessManifest,
    root: &Path,
    platform: Platform,
) -> Result<PlatformEntrypoint, InstallError> {
    let entrypoint = manifest.entrypoint_for(platform).cloned().ok_or_else(|| {
        InstallError::MissingEntrypoint {
            platform: format!("{platform:?}"),
            declared: manifest
                .supported_platforms
                .iter()
                .map(|entry| format!("{:?}", entry.platform))
                .collect(),
        }
    })?;
    let declared = entrypoint.executable.replace('\\', "/");
    let candidates = if cfg!(windows) {
        vec![declared.clone(), format!("{declared}.exe")]
    } else {
        vec![declared.clone()]
    };
    let mut found: Option<(String, PathBuf)> = None;
    for candidate in &candidates {
        let path = root.join(candidate);
        if path.is_file() {
            found = Some((candidate.clone(), path));
            break;
        }
    }
    let (relative, executable) = found.ok_or_else(|| InstallError::MissingEntrypoint {
        platform: format!("{platform:?}"),
        declared: candidates,
    })?;
    // Final containment check: the canonicalized executable stays inside the
    // (already link-free) staging root.
    let canonical_root = root
        .canonicalize()
        .map_err(|e| InstallError::Io(e.to_string()))?;
    let canonical_executable = executable
        .canonicalize()
        .map_err(|e| InstallError::Io(e.to_string()))?;
    if !canonical_executable.starts_with(&canonical_root) {
        return Err(InstallError::LinkEscape(relative));
    }
    Ok(PlatformEntrypoint {
        platform,
        executable: relative,
        argv: entrypoint.argv,
    })
}

/// Copy a directory tree, refusing symlinks/reparse points entirely.
fn copy_tree_no_links(source: &Path, destination: &Path) -> Result<(), InstallError> {
    fs::create_dir_all(destination).map_err(|e| InstallError::Io(e.to_string()))?;
    let entries =
        fs::read_dir(source).map_err(|e| InstallError::Io(format!("read {source:?}: {e}")))?;
    for entry in entries {
        let entry = entry.map_err(|e| InstallError::Io(e.to_string()))?;
        let metadata =
            fs::symlink_metadata(entry.path()).map_err(|e| InstallError::Io(e.to_string()))?;
        if metadata.file_type().is_symlink() {
            return Err(InstallError::LinkEscape(
                entry.file_name().to_string_lossy().into_owned(),
            ));
        }
        let target = destination.join(entry.file_name());
        if metadata.is_dir() {
            copy_tree_no_links(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target).map_err(|e| InstallError::Io(e.to_string()))?;
        }
    }
    Ok(())
}

/// Extract a zip refusing escapes, absolute paths and symlink entries.
fn extract_zip_no_escape(zip_path: &Path, destination: &Path) -> Result<(), InstallError> {
    let file = fs::File::open(zip_path).map_err(|e| InstallError::Zip(e.to_string()))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| InstallError::Zip(e.to_string()))?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|e| InstallError::Zip(e.to_string()))?;
        let name = entry.name().to_string();
        if name.contains("..") || name.starts_with('/') || name.contains(':') {
            return Err(InstallError::TraversalEntry(name));
        }
        // Unix mode bits may mark the entry as a symlink: reject.
        if let Some(mode) = entry.unix_mode() {
            if mode & 0o170000 == 0o120000 {
                return Err(InstallError::LinkEscape(name));
            }
        }
        if entry.is_dir() {
            fs::create_dir_all(destination.join(&name))
                .map_err(|e| InstallError::Io(e.to_string()))?;
            continue;
        }
        let destination_path = destination.join(&name);
        if let Some(parent) = destination_path.parent() {
            fs::create_dir_all(parent).map_err(|e| InstallError::Io(e.to_string()))?;
        }
        let mut output =
            fs::File::create(&destination_path).map_err(|e| InstallError::Io(e.to_string()))?;
        std::io::copy(&mut entry, &mut output).map_err(|e| InstallError::Io(e.to_string()))?;
    }
    Ok(())
}

/// SHA-256 over the sorted (relative path, bytes) of every regular file.
fn content_digest(root: &Path) -> Result<String, InstallError> {
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    collect_files(root, root, &mut files)?;
    let mut hasher = Sha256::new();
    for (path, bytes) in &files {
        hasher.update(path.as_bytes());
        hasher.update((path.len() as u64).to_le_bytes());
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    let digest = hasher.finalize();
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

fn collect_files(
    root: &Path,
    dir: &Path,
    files: &mut BTreeMap<String, Vec<u8>>,
) -> Result<(), InstallError> {
    let entries = fs::read_dir(dir).map_err(|e| InstallError::Io(e.to_string()))?;
    for entry in entries {
        let entry = entry.map_err(|e| InstallError::Io(e.to_string()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, files)?;
        } else {
            let relative = path
                .strip_prefix(root)
                .map_err(|e| InstallError::Io(e.to_string()))?
                .to_string_lossy()
                .replace('\\', "/");
            files.insert(
                relative,
                fs::read(&path).map_err(|e| InstallError::Io(e.to_string()))?,
            );
        }
    }
    Ok(())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), InstallError> {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(metadata) = fs::metadata(path) {
        let mut permissions = metadata.permissions();
        permissions.set_mode(permissions.mode() | 0o755);
        fs::set_permissions(path, permissions).map_err(|e| InstallError::Io(e.to_string()))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), InstallError> {
    Ok(())
}

/// Manifest schema version constant re-export for installers that validate
/// before parsing.
pub const INSTALL_MANIFEST_SCHEMA: &str = MANIFEST_SCHEMA_VERSION;
