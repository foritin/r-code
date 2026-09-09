//! Explicit runtime profiles and harness-v2 path isolation.
//!
//! A `RuntimeProfile` is constructed *before* any store, settings, plugin or
//! provider service initializes. The flavor is always explicit: the GUI passes
//! its build flavor, TUI/service take `--profile`, and the runtime itself
//! never infers identity from Tauri features.
//!
//! All v2 data lives under `<data_root>/harness-v2`; legacy databases,
//! configuration and JSONL history in the parent root are never read for
//! writes and never modified.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

pub use r_code_harness_protocol::IpcEndpoint;

/// Development or production identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProfileFlavor {
    Development,
    Production,
}

impl ProfileFlavor {
    pub fn as_str(self) -> &'static str {
        match self {
            ProfileFlavor::Development => "development",
            ProfileFlavor::Production => "production",
        }
    }

    /// Parse a `--profile` value. Accepts the long and short spellings.
    pub fn parse(value: &str) -> Option<Self> {
        match value.to_ascii_lowercase().as_str() {
            "development" | "dev" => Some(ProfileFlavor::Development),
            "production" | "prod" => Some(ProfileFlavor::Production),
            _ => None,
        }
    }

    /// Default credential service name for the v2 credential service. These
    /// are distinct from the legacy v1 service names.
    pub fn credential_service(self) -> &'static str {
        match self {
            ProfileFlavor::Development => "r-code-harness-v2-dev",
            ProfileFlavor::Production => "r-code-harness-v2",
        }
    }

    /// Bundle identifier used to derive the default data root, mirroring the
    /// desktop flavor table (macOS uses its own identifiers).
    fn bundle_identifier(self) -> &'static str {
        match (self, cfg!(target_os = "macos")) {
            (ProfileFlavor::Production, true) => "com.rcode.desktop",
            (ProfileFlavor::Development, true) => "com.rcode.desktop.dev",
            (ProfileFlavor::Production, false) => "com.r-code.app",
            (ProfileFlavor::Development, false) => "com.r-code.app.dev",
        }
    }

    /// Default data root: `<data_dir>/<bundle_identifier>/r-code`, matching
    /// the desktop app's app_data_dir layout for the same flavor.
    pub fn default_data_root(self) -> Option<PathBuf> {
        dirs::data_dir().map(|root| self.data_root_under(&root))
    }

    pub fn data_root_under(self, root: &Path) -> PathBuf {
        root.join(self.bundle_identifier()).join("r-code")
    }
}

/// Errors constructing a profile.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProfileError {
    #[error("profile flavor must be passed explicitly (--profile development|production); it is never inferred")]
    AmbiguousFlavor,
    #[error("unknown profile flavor {0:?} (expected development or production)")]
    UnknownFlavor(String),
    #[error("unexpected argument {0:?}")]
    UnexpectedArgument(String),
    #[error("missing value for {0}")]
    MissingValue(&'static str),
}

/// Launch options consumed by GUI/TUI/service entrypoints before any service
/// initialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchOptions {
    pub flavor: ProfileFlavor,
    /// Explicit data-root override (tests, portable installs). Defaults to
    /// the flavor's platform data directory.
    pub data_root: Option<PathBuf>,
    /// Endpoint-name override for the daemon IPC (tests isolating parallel
    /// profiles on one machine). None derives from the flavor.
    pub ipc_name: Option<String>,
}

impl LaunchOptions {
    pub fn new(flavor: ProfileFlavor) -> Self {
        Self {
            flavor,
            data_root: None,
            ipc_name: None,
        }
    }

    pub fn with_data_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.data_root = Some(root.into());
        self
    }

    pub fn with_ipc_name(mut self, name: impl Into<String>) -> Self {
        self.ipc_name = Some(name.into());
        self
    }

    /// Parse `--profile <flavor>` and `--data-root <path>` from an argument
    /// list. When `default` is None the flavor must be explicit.
    pub fn parse_args_with_default(
        args: &[String],
        default: Option<ProfileFlavor>,
    ) -> Result<Self, ProfileError> {
        let mut flavor = default;
        let mut data_root: Option<PathBuf> = None;
        let mut ipc_name: Option<String> = None;
        let mut index = 0;
        while index < args.len() {
            let arg = args[index].as_str();
            match arg {
                "--profile" => {
                    let value = args
                        .get(index + 1)
                        .ok_or(ProfileError::MissingValue("--profile"))?;
                    flavor = Some(
                        ProfileFlavor::parse(value)
                            .ok_or_else(|| ProfileError::UnknownFlavor(value.clone()))?,
                    );
                    index += 2;
                }
                "--data-root" => {
                    let value = args
                        .get(index + 1)
                        .ok_or(ProfileError::MissingValue("--data-root"))?;
                    data_root = Some(PathBuf::from(value));
                    index += 2;
                }
                "--ipc-name" => {
                    let value = args
                        .get(index + 1)
                        .ok_or(ProfileError::MissingValue("--ipc-name"))?;
                    ipc_name = Some(value.clone());
                    index += 2;
                }
                other => return Err(ProfileError::UnexpectedArgument(other.to_string())),
            }
        }
        Ok(Self {
            flavor: flavor.ok_or(ProfileError::AmbiguousFlavor)?,
            data_root,
            ipc_name,
        })
    }

    /// Strict parse: the flavor must always be explicit.
    pub fn parse_args(args: &[String]) -> Result<Self, ProfileError> {
        Self::parse_args_with_default(args, None)
    }
}

/// The resolved, immutable profile identity for one running process.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RuntimeProfile {
    flavor: ProfileFlavor,
    data_root: PathBuf,
    ipc_name: Option<String>,
}

impl RuntimeProfile {
    /// Resolve a profile from launch options. Pure: touches no filesystem.
    pub fn resolve(options: &LaunchOptions) -> Result<Self, ProfileError> {
        let data_root = match &options.data_root {
            Some(root) => root.clone(),
            None => options
                .flavor
                .default_data_root()
                .ok_or(ProfileError::AmbiguousFlavor)?,
        };
        Ok(Self {
            flavor: options.flavor,
            data_root,
            ipc_name: options.ipc_name.clone(),
        })
    }

    pub fn flavor(&self) -> ProfileFlavor {
        self.flavor
    }

    /// Endpoint-name override, when one was supplied (tests, custom layouts).
    pub fn ipc_name(&self) -> Option<&str> {
        self.ipc_name.as_deref()
    }

    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    /// Stable identity string; clients use it to discover the same daemon.
    pub fn profile_id(&self) -> String {
        format!("harness-v2/{}", self.flavor.as_str())
    }

    /// Root of all v2 state: `<data_root>/harness-v2`. Legacy data in
    /// `data_root` is untouched.
    pub fn harness_v2_root(&self) -> PathBuf {
        self.data_root.join("harness-v2")
    }

    pub fn database_path(&self) -> PathBuf {
        self.harness_v2_root().join("tasks.sqlite3")
    }

    pub fn plugins_root(&self) -> PathBuf {
        self.harness_v2_root().join("plugins")
    }

    pub fn blobs_root(&self) -> PathBuf {
        self.harness_v2_root().join("blobs")
    }

    pub fn workspaces_root(&self) -> PathBuf {
        self.harness_v2_root().join("workspaces")
    }

    pub fn checkpoints_root(&self) -> PathBuf {
        self.harness_v2_root().join("checkpoints")
    }

    pub fn credential_service(&self) -> &'static str {
        self.flavor.credential_service()
    }

    /// OS-local IPC endpoint for the profile daemon.
    pub fn ipc_endpoint(&self) -> IpcEndpoint {
        let suffix = self
            .ipc_name
            .clone()
            .unwrap_or_else(|| self.flavor.as_str().to_string());
        if cfg!(windows) {
            IpcEndpoint::NamedPipe {
                name: format!(r"\\.\pipe\r-code-harness-v2-{suffix}"),
            }
        } else {
            let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| std::env::temp_dir());
            IpcEndpoint::UnixSocket {
                path: runtime_dir.join(format!("r-code-harness-v2-{suffix}.sock")),
            }
        }
    }

    /// Create the v2 directory layout. Only ever writes below
    /// `harness-v2`; idempotent.
    pub fn ensure_layout(&self) -> io::Result<()> {
        for dir in [
            self.harness_v2_root(),
            self.plugins_root(),
            self.blobs_root(),
            self.workspaces_root(),
            self.checkpoints_root(),
        ] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}
