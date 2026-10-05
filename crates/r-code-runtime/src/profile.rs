//! Explicit runtime profiles and harness-v1 path isolation.
//!
//! A `RuntimeProfile` is constructed *before* any store, settings, plugin or
//! provider service initializes. The flavor is always explicit: the GUI passes
//! its build flavor, TUI/service take `--profile`, and the runtime itself
//! never infers identity from Tauri features.
//!
//! All Harness v1 data lives under `<data_root>/harness-v1`; legacy databases,
//! configuration and JSONL history in the parent root are never read for
//! writes and never modified.

use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

pub use r_code_harness_protocol::IpcEndpoint;

const PRE_V1_HARNESS_DIR: &str = "harness-v2";

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

    /// Default credential service name for the Harness v1 credential service. These are distinct
    /// from the pre-Harness application service names.
    pub fn credential_service(self) -> &'static str {
        match self {
            ProfileFlavor::Development => "r-code-harness-v1-dev",
            ProfileFlavor::Production => "r-code-harness-v1",
        }
    }

    /// Credential service used by the temporary pre-v1 Harness build. This is
    /// exposed only inside the runtime so the settings store can perform its
    /// one-time, lazy credential migration without making the old namespace a
    /// supported public configuration surface.
    pub(crate) fn pre_v1_credential_service(self) -> &'static str {
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
    /// Host-owned directory holding the guardian/probe helper binaries
    /// (P24H). None falls back to the daemon executable's own directory —
    /// PATH is never searched.
    pub helper_dir: Option<PathBuf>,
}

impl LaunchOptions {
    pub fn new(flavor: ProfileFlavor) -> Self {
        Self {
            flavor,
            data_root: None,
            ipc_name: None,
            helper_dir: None,
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

    /// Bind the host-owned helper directory (P24H).
    pub fn with_helper_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.helper_dir = Some(dir.into());
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
        let mut helper_dir: Option<PathBuf> = None;
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
                "--helper-dir" => {
                    let value = args
                        .get(index + 1)
                        .ok_or(ProfileError::MissingValue("--helper-dir"))?;
                    helper_dir = Some(PathBuf::from(value));
                    index += 2;
                }
                other => return Err(ProfileError::UnexpectedArgument(other.to_string())),
            }
        }
        Ok(Self {
            flavor: flavor.ok_or(ProfileError::AmbiguousFlavor)?,
            data_root,
            ipc_name,
            helper_dir,
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
    helper_dir: Option<PathBuf>,
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
            helper_dir: options.helper_dir.clone(),
        })
    }

    pub fn flavor(&self) -> ProfileFlavor {
        self.flavor
    }

    /// Endpoint-name override, when one was supplied (tests, custom layouts).
    pub fn ipc_name(&self) -> Option<&str> {
        self.ipc_name.as_deref()
    }

    /// The host-owned helper directory bound at launch, when one was
    /// supplied (P24H). None means the helper resolver falls back to the
    /// daemon executable's own directory.
    pub fn helper_dir(&self) -> Option<&Path> {
        self.helper_dir.as_deref()
    }

    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    /// Stable identity string; clients use it to discover the same daemon.
    pub fn profile_id(&self) -> String {
        format!("harness-v1/{}", self.flavor.as_str())
    }

    /// Root of all Harness v1 state: `<data_root>/harness-v1`.
    pub fn harness_v1_root(&self) -> PathBuf {
        self.data_root.join("harness-v1")
    }

    fn pre_v1_harness_root(&self) -> PathBuf {
        self.data_root.join(PRE_V1_HARNESS_DIR)
    }

    pub fn database_path(&self) -> PathBuf {
        self.harness_v1_root().join("tasks.sqlite3")
    }

    pub fn plugins_root(&self) -> PathBuf {
        self.harness_v1_root().join("plugins")
    }

    pub fn blobs_root(&self) -> PathBuf {
        self.harness_v1_root().join("blobs")
    }

    pub fn workspaces_root(&self) -> PathBuf {
        self.harness_v1_root().join("workspaces")
    }

    pub fn checkpoints_root(&self) -> PathBuf {
        self.harness_v1_root().join("checkpoints")
    }

    pub fn credential_service(&self) -> &'static str {
        self.flavor.credential_service()
    }

    pub(crate) fn pre_v1_credential_service(&self) -> &'static str {
        self.flavor.pre_v1_credential_service()
    }

    /// OS-local IPC endpoint for the profile daemon.
    pub fn ipc_endpoint(&self) -> IpcEndpoint {
        let suffix = self
            .ipc_name
            .clone()
            .unwrap_or_else(|| self.flavor.as_str().to_string());
        if cfg!(windows) {
            IpcEndpoint::NamedPipe {
                name: format!(r"\\.\pipe\r-code-harness-v1-{suffix}"),
            }
        } else {
            let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|_| std::env::temp_dir());
            let path = runtime_dir.join(format!("r-code-harness-v1-{suffix}.sock"));
            // macOS 的 sun_path 上限 104 字节：长 ipc 名组合会超限，bind
            // 直接报 "path must be shorter than SUN_LEN"（daemon 起不来，
            // CI 深临时目录稳定触发）。超限时回落到按全名散列的短路径
            // （同一 profile 稳定映射，防碰撞）。
            #[cfg(target_os = "macos")]
            let path = shorten_for_sun_len(path, &suffix);
            IpcEndpoint::UnixSocket { path }
        }
    }

    /// Create the Harness v1 directory layout. A pre-v1 development build used the temporary
    /// pre-v1 directory label for this same schema; when v1 does not exist yet, rename that
    /// complete tree in place before opening any database so tasks, settings, plugins and remote
    /// identity remain intact.
    pub fn ensure_layout(&self) -> io::Result<()> {
        let root = self.harness_v1_root();
        let pre_v1_root = self.pre_v1_harness_root();
        if !root.exists() && pre_v1_root.exists() {
            std::fs::rename(&pre_v1_root, &root).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "migrate {} to {}: {error}",
                        pre_v1_root.display(),
                        root.display()
                    ),
                )
            })?;
        }
        for dir in [
            root,
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

/// macOS 的 sockaddr_un.sun_path 上限是 104 字节：超出时 bind 会以
/// "path must be longer than SUN_LEN" 形式失败。超长路径回落为按
/// 全名（ipc 名）散列的短文件名，同一 profile 稳定映射。
#[cfg(target_os = "macos")]
fn shorten_for_sun_len(path: PathBuf, suffix: &str) -> PathBuf {
    const SUN_LEN_LIMIT: usize = 100;
    if path.as_os_str().len() <= SUN_LEN_LIMIT {
        return path;
    }
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(format!("r-code-ipc:{suffix}").as_bytes());
    let hex: String = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    std::env::temp_dir().join(format!("r-code-{hex}.sock"))
}
