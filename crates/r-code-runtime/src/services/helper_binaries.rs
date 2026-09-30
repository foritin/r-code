//! P24H — host-owned helper binary resolution (guardian / safety-probe).
//!
//! The runtime resolves its privileged helper binaries from exactly two
//! places, in order: an explicit absolute directory the host passed at
//! launch (`--helper-dir`, carried by [`RuntimeProfile`]), otherwise the
//! verified sibling directory of the running daemon executable. PATH is
//! never searched (fail-closed, INV-07).
//!
//! Verification at resolve time: regular file, above the packaging size
//! floor, native executable magic — tampered binaries are refused. Full
//! signature/owner pinning is the release gate's job (P32); the digest
//! helper lets callers pin bytes.

use std::path::{Path, PathBuf};

/// Helper binary names the host ships (target-triple suffixes are stripped
/// by the packaging scripts before staging beside the daemon).
pub const GUARDIAN_HELPER: &str = "r-code-process-guardian";
pub const SAFETY_PROBE_HELPER: &str = "r-code-safety-probe";

/// The same floor the packaging validation enforces: a real helper is never
/// a placeholder-sized file.
pub const MIN_HELPER_BYTES: u64 = 64 * 1024;

/// Why a helper could not be resolved. Every refusal is SafeDisabled-shaped:
/// the caller declines the capability, never falls back to PATH.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum HelperBinaryError {
    #[error("helper {name:?} was not found in {directory:?}")]
    Missing { name: String, directory: PathBuf },
    #[error("helper {path} failed verification: {reason}")]
    Tampered { path: PathBuf, reason: String },
    #[error("no helper directory is bound and the daemon directory is unavailable")]
    Unbound,
}

/// Resolves helper binaries for one running daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperBinaryResolver {
    /// The explicit `--helper-dir` the host passed, when it passed one.
    explicit: Option<PathBuf>,
    /// The directory of the running daemon executable (the packaged layout
    /// ships helpers beside it; a dev layout finds them in target/debug).
    daemon_directory: Option<PathBuf>,
}

impl HelperBinaryResolver {
    /// Build the resolver from the profile's explicit binding (if any) and
    /// the running executable's own directory.
    pub fn from_profile(profile: &crate::profile::RuntimeProfile) -> Self {
        Self {
            explicit: profile.helper_dir().map(Path::to_path_buf),
            daemon_directory: std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(Path::to_path_buf)),
        }
    }

    /// Build a resolver over explicit directories (tests, custom layouts).
    pub fn new(explicit: Option<PathBuf>, daemon_directory: Option<PathBuf>) -> Self {
        Self {
            explicit,
            daemon_directory,
        }
    }

    /// Resolve one helper by bare name. The explicit directory is consulted
    /// first and is REQUIRED to hold a verified helper when bound; the
    /// sibling directory is the fallback. PATH is never consulted.
    pub fn resolve(&self, name: &str) -> Result<PathBuf, HelperBinaryError> {
        if let Some(explicit) = &self.explicit {
            let candidate = helper_path(explicit, name);
            return verify_helper(&candidate, name);
        }
        match &self.daemon_directory {
            Some(directory) => {
                let candidate = helper_path(directory, name);
                verify_helper(&candidate, name)
            }
            None => Err(HelperBinaryError::Unbound),
        }
    }

    /// The safety probe helper, verified.
    pub fn safety_probe(&self) -> Result<PathBuf, HelperBinaryError> {
        self.resolve(SAFETY_PROBE_HELPER)
    }

    /// The process guardian helper, verified.
    pub fn guardian(&self) -> Result<PathBuf, HelperBinaryError> {
        self.resolve(GUARDIAN_HELPER)
    }
}

fn helper_path(directory: &Path, name: &str) -> PathBuf {
    let file = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    directory.join(file)
}

/// Verify one candidate helper: regular file, above the size floor, and
/// carrying this platform's executable magic.
fn verify_helper(path: &Path, name: &str) -> Result<PathBuf, HelperBinaryError> {
    let metadata = std::fs::metadata(path).map_err(|_| HelperBinaryError::Missing {
        name: name.to_string(),
        directory: path.parent().map(Path::to_path_buf).unwrap_or_default(),
    })?;
    if !metadata.is_file() {
        return Err(HelperBinaryError::Tampered {
            path: path.to_path_buf(),
            reason: "not a regular file".into(),
        });
    }
    if metadata.len() < MIN_HELPER_BYTES {
        return Err(HelperBinaryError::Tampered {
            path: path.to_path_buf(),
            reason: format!(
                "only {} bytes; below the {}-byte helper floor",
                metadata.len(),
                MIN_HELPER_BYTES
            ),
        });
    }
    let mut prefix = [0_u8; 4];
    std::fs::File::open(path)
        .and_then(|mut file| std::io::Read::read_exact(&mut file, &mut prefix))
        .map_err(|error| HelperBinaryError::Tampered {
            path: path.to_path_buf(),
            reason: format!("unreadable: {error}"),
        })?;
    let valid = if cfg!(windows) {
        prefix.starts_with(b"MZ")
    } else if cfg!(target_os = "linux") {
        prefix == [0x7f, b'E', b'L', b'F']
    } else if cfg!(target_os = "macos") {
        matches!(
            prefix,
            [0xfe, 0xed, 0xfa, 0xce]
                | [0xce, 0xfa, 0xed, 0xfe]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xcf, 0xfa, 0xed, 0xfe]
                | [0xca, 0xfe, 0xba, 0xbe]
                | [0xbe, 0xba, 0xfe, 0xca]
                | [0xca, 0xfe, 0xba, 0xbf]
                | [0xbf, 0xba, 0xfe, 0xca]
        )
    } else {
        false
    };
    if !valid {
        return Err(HelperBinaryError::Tampered {
            path: path.to_path_buf(),
            reason: format!("magic {:02x?} is not a native executable", prefix),
        });
    }
    Ok(path.to_path_buf())
}

/// The content digest of one helper binary, for callers that pin the
/// packaged bytes (the release gate and the safety report material).
pub fn helper_digest(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(crate::services::artifacts::sha256_hex(&bytes))
}
