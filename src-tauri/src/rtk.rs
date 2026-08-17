//! R-Code 托管的 RTK（Rust Token Killer）安装与启停策略。
//!
//! 二进制放在应用数据根的 `bin/`，只通过 R-Code 启动的 Codex 子进程 PATH 暴露；
//! 不修改系统 PATH，也不写用户的 `~/.codex/AGENTS.md`。启停由同一个策略文件在
//! `rtk-policy.md` / `rtk-policy.md.disabled` 之间改名完成，因此关闭不会卸载 RTK。

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;

use futures::StreamExt;
use r_code_core::process::hide_background_console;
use reqwest::header::{ACCEPT, USER_AGENT};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::process::Command;
use tokio::time::{timeout, Duration};

const RTK_REPOSITORY: &str = "rtk-ai/rtk";
const RTK_RELEASE_PREFIX: &str = "https://github.com/rtk-ai/rtk/releases/download/";
const RTK_RELEASE_TAG: &str = "v0.45.0";
const POLICY_FILE: &str = "rtk-policy.md";
const DISABLED_POLICY_FILE: &str = "rtk-policy.md.disabled";
const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;
const MAX_BINARY_BYTES: u64 = 64 * 1024 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(90);

/// Stable error code surfaced to the WebView when the freshly installed RTK binary cannot be
/// executed. On Windows this is almost always Windows Defender quarantining the unsigned binary
/// (`Behavior:Win32/DefenseEvasion.A!ml`), which the user must resolve through the Windows
/// Security exclusions UI — elevation alone cannot bypass tamper protection.
pub const RTK_BLOCKED_BY_SECURITY_SOFTWARE: &str = "RTK_BLOCKED_BY_SECURITY_SOFTWARE";

/// Deep link that opens the Windows Security exclusions page directly. The Security app handles
/// this custom protocol even when tamper protection blocks scripted exclusion changes.
pub const WINDOWS_SECURITY_EXCLUSIONS_URL: &str = "windowsdefender://threat/exclusions";

pub const COMMAND_HINT: &str =
    "RTK (Rust Token Killer) is enabled by the user in R-Code. For supported non-interactive shell work, \
you must prefer its token-optimized wrappers, for example `rtk rg`, `rtk read`, `rtk git`, \
`rtk cargo`, `rtk npm`, and `rtk test`. Use the native command only when RTK does not support it, \
RTK returns an error, exact unfiltered output is required, or the command is interactive. Do not \
run the same successful operation again natively just to obtain longer output.";

const POLICY_CONTENT: &str = "# R-Code RTK command policy\n\n\
Enabled by the RTK switch in R-Code Settings. New R-Code-hosted Codex runs prefer RTK wrappers \
for supported non-interactive shell commands. Rename this file to `rtk-policy.md.disabled` to \
disable the policy without uninstalling RTK.\n";

fn append_command_hint(target: &mut String, hint: &str) {
    target.push_str(
        "

RTK command policy:
",
    );
    target.push_str(hint);
}

static RTK_CHANGE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RtkStatus {
    pub enabled: bool,
    pub available: bool,
    pub managed: bool,
    pub version: Option<String>,
    pub source: Option<&'static str>,
    pub platform: String,
    /// R-Code 托管二进制所在目录；被安全软件拦截时，用户应把该目录加入排除项。
    pub bin_dir: Option<String>,
}

#[derive(Debug, Clone)]
struct RtkProbe {
    version: String,
    managed: bool,
}

/// Typed failure for RTK enable/install. Carries a stable `code` so the WebView can render
/// actionable guidance (for example opening the Windows Security exclusions page) without
/// parsing diagnostic prose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtkError {
    code: Option<&'static str>,
    message: String,
}

impl RtkError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self {
            code: None,
            message: message.into(),
        }
    }

    pub(crate) fn security_block(message: impl Into<String>) -> Self {
        Self {
            code: Some(RTK_BLOCKED_BY_SECURITY_SOFTWARE),
            message: message.into(),
        }
    }

    pub fn code(&self) -> Option<&'static str> {
        self.code
    }
}

impl std::fmt::Display for RtkError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RtkError {}

impl From<String> for RtkError {
    fn from(message: String) -> Self {
        Self::new(message)
    }
}

#[derive(Debug, Clone, Copy)]
struct RtkReleaseAsset {
    name: &'static str,
    sha256: &'static str,
}

#[derive(Debug, Clone)]
pub struct RtkManager {
    data_dir: PathBuf,
    config_dir: PathBuf,
}

impl RtkManager {
    pub fn from_config_dir(config_dir: impl Into<PathBuf>) -> Self {
        let config_dir = config_dir.into();
        let data_dir = config_dir
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| config_dir.clone());
        Self {
            data_dir,
            config_dir,
        }
    }

    pub fn from_data_dir(data_dir: impl Into<PathBuf>) -> Self {
        let data_dir = data_dir.into();
        Self {
            config_dir: data_dir.join("config"),
            data_dir,
        }
    }

    pub fn managed_bin_dir(&self) -> PathBuf {
        self.data_dir.join("bin")
    }

    pub fn managed_executable(&self) -> PathBuf {
        self.managed_bin_dir().join(rtk_executable_name())
    }

    pub fn policy_path(&self) -> PathBuf {
        self.config_dir.join(POLICY_FILE)
    }

    pub fn disabled_policy_path(&self) -> PathBuf {
        self.config_dir.join(DISABLED_POLICY_FILE)
    }

    pub fn policy_enabled(&self) -> bool {
        self.policy_path().is_file()
    }

    pub fn command_hint(&self) -> &'static str {
        if self.policy_enabled() && self.has_candidate_binary() {
            COMMAND_HINT
        } else {
            ""
        }
    }

    /// RTK 是全局开关：开启后所有模型的会话都优先使用 RTK 包装命令。
    /// 这里把策略追加进原生运行时的主/子代理提示词；Codex 路径仍由
    /// `command_hint()` 在各自 prompt 组装点单独注入，两者互不重叠。
    pub fn apply_command_hint(&self, main_agent: &mut String, subagent: &mut String) {
        let hint = self.command_hint();
        if hint.is_empty() {
            return;
        }
        append_command_hint(main_agent, hint);
        append_command_hint(subagent, hint);
    }

    pub async fn status(&self) -> RtkStatus {
        let probe = self.probe().await;
        let available = probe.is_some();
        let configured = self.policy_enabled();
        RtkStatus {
            enabled: configured && available,
            available,
            managed: probe.as_ref().is_some_and(|probe| probe.managed),
            version: probe.as_ref().map(|probe| probe.version.clone()),
            source: probe
                .as_ref()
                .map(|probe| if probe.managed { "managed" } else { "system" }),
            platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            bin_dir: Some(self.managed_bin_dir().to_string_lossy().into_owned()),
        }
    }

    pub async fn set_enabled(&self, enabled: bool) -> Result<RtkStatus, RtkError> {
        let _guard = RTK_CHANGE_LOCK.lock().await;
        if !enabled {
            self.disable_policy()?;
            return Ok(self.status().await);
        }

        let result = self.enable_inner().await;
        if result.is_err() {
            // A failed enable must never leave the next Codex run partially configured.
            if let Err(rollback_error) = self.disable_policy() {
                tracing::error!(%rollback_error, "failed to roll back RTK policy after enable error");
            }
        }
        result
    }

    async fn enable_inner(&self) -> Result<RtkStatus, RtkError> {
        let probe = match self.probe().await {
            Some(probe) => probe,
            None => self.install_managed().await?,
        };
        self.enable_policy()?;
        let status = RtkStatus {
            enabled: true,
            available: true,
            managed: probe.managed,
            version: Some(probe.version),
            source: Some(if probe.managed { "managed" } else { "system" }),
            platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            bin_dir: Some(self.managed_bin_dir().to_string_lossy().into_owned()),
        };
        tracing::info!(
            source = status.source.unwrap_or("unknown"),
            version = status.version.as_deref().unwrap_or("unknown"),
            "RTK command policy enabled"
        );
        Ok(status)
    }

    async fn probe(&self) -> Option<RtkProbe> {
        for (path, managed) in self.candidate_paths().into_iter().take(16) {
            if let Some(version) = probe_rtk(&path).await {
                return Some(RtkProbe { version, managed });
            }
        }
        None
    }

    fn candidate_paths(&self) -> Vec<(PathBuf, bool)> {
        let managed = self.managed_executable();
        let mut candidates = Vec::new();
        let mut seen = HashSet::<OsString>::new();
        if managed.is_file() {
            seen.insert(normalized_path_key(&managed));
            candidates.push((managed, true));
        }
        if let Some(path) = std::env::var_os("PATH") {
            for directory in std::env::split_paths(&path) {
                for name in rtk_executable_names() {
                    let candidate = directory.join(name);
                    let key = normalized_path_key(&candidate);
                    if candidate.is_file() && seen.insert(key) {
                        candidates.push((candidate, false));
                    }
                }
            }
        }
        candidates
    }

    fn has_candidate_binary(&self) -> bool {
        self.managed_executable().is_file()
            || std::env::var_os("PATH").is_some_and(|path| {
                std::env::split_paths(&path).any(|directory| {
                    rtk_executable_names()
                        .iter()
                        .any(|name| directory.join(name).is_file())
                })
            })
    }

    async fn install_managed(&self) -> Result<RtkProbe, RtkError> {
        let asset = release_asset(std::env::consts::OS, std::env::consts::ARCH)?;
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(12))
            .timeout(DOWNLOAD_TIMEOUT)
            .build()
            .map_err(|error| format!("build RTK download client: {error}"))?;
        let archive_url = format!("{RTK_RELEASE_PREFIX}{RTK_RELEASE_TAG}/{}", asset.name);
        let archive_bytes = download_bounded(
            &client,
            &archive_url,
            MAX_ARCHIVE_BYTES,
            "application/octet-stream",
        )
        .await?;
        let actual = format!("{:x}", Sha256::digest(&archive_bytes));
        if actual != asset.sha256 {
            return Err(format!(
                "RTK archive checksum mismatch for {}: expected {}, got {actual}",
                asset.name, asset.sha256
            )
            .into());
        }

        let binary = extract_rtk_binary(&archive_bytes)?;
        let staging = self.stage_managed_binary(&binary)?;
        let target = self.managed_executable();
        let backup = self.activate_staged_binary(staging.as_ref())?;
        let verification = match probe_rtk_outcome(&target).await {
            ProbeOutcome::Version(version)
                if rtk_version_matches_release(&version, RTK_RELEASE_TAG) =>
            {
                Ok(version)
            }
            ProbeOutcome::Version(version) => Err(RtkError::new(format!(
                "installed RTK version {version:?} does not match release {RTK_RELEASE_TAG}"
            ))),
            ProbeOutcome::Blocked(detail) => Err(security_block_error(&target, &detail)),
            ProbeOutcome::Unavailable => {
                if target.is_file() {
                    Err(RtkError::new(format!(
                        "installed RTK binary failed verification at {}",
                        target.display()
                    )))
                } else {
                    Err(security_block_error(
                        &target,
                        "the binary was removed after installation",
                    ))
                }
            }
        };
        let version = match verification {
            Ok(version) => version,
            Err(verification_error) => {
                return match self.rollback_managed_binary(backup.as_deref()) {
                    Ok(()) => Err(verification_error),
                    Err(rollback_error) => Err(RtkError::new(format!(
                        "{verification_error}; failed to roll back RTK binary: {rollback_error}"
                    ))),
                };
            }
        };
        if let Some(backup_path) = backup.as_deref() {
            if let Err(cleanup_error) = std::fs::remove_file(backup_path) {
                if cleanup_error.kind() != std::io::ErrorKind::NotFound {
                    return match self.rollback_managed_binary(Some(backup_path)) {
                        Ok(()) => Err(RtkError::new(format!(
                            "remove verified RTK backup {}: {cleanup_error}",
                            backup_path.display()
                        ))),
                        Err(rollback_error) => Err(RtkError::new(format!(
                            "remove verified RTK backup {}: {cleanup_error}; failed to roll back RTK binary: {rollback_error}",
                            backup_path.display()
                        ))),
                    };
                }
            }
        }
        tracing::info!(
            repository = RTK_REPOSITORY,
            release = RTK_RELEASE_TAG,
            asset = asset.name,
            path = %target.display(),
            "installed verified RTK release"
        );
        Ok(RtkProbe {
            version,
            managed: true,
        })
    }

    fn stage_managed_binary(&self, binary: &[u8]) -> Result<tempfile::TempPath, String> {
        if binary.is_empty() || binary.len() as u64 > MAX_BINARY_BYTES {
            return Err("extracted RTK binary has an invalid size".to_string());
        }
        let bin_dir = self.managed_bin_dir();
        std::fs::create_dir_all(&bin_dir)
            .map_err(|error| format!("create RTK bin directory {}: {error}", bin_dir.display()))?;
        let mut temporary = tempfile::Builder::new()
            .prefix(".rtk-staging-")
            .tempfile_in(&bin_dir)
            .map_err(|error| format!("create RTK temporary file: {error}"))?;
        temporary
            .write_all(binary)
            .and_then(|_| temporary.as_file().sync_all())
            .map_err(|error| format!("write RTK temporary file: {error}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(temporary.path(), std::fs::Permissions::from_mode(0o755))
                .map_err(|error| format!("mark RTK executable: {error}"))?;
        }
        Ok(temporary.into_temp_path())
    }

    fn activate_staged_binary(&self, staging: &Path) -> Result<Option<PathBuf>, String> {
        let target = self.managed_executable();
        let target_exists = match std::fs::symlink_metadata(&target) {
            Ok(metadata) if metadata.file_type().is_file() => true,
            Ok(_) => {
                return Err(format!(
                    "refuse to replace non-file RTK target {}",
                    target.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(format!("inspect RTK target {}: {error}", target.display()));
            }
        };
        let backup = if target_exists {
            let placeholder = tempfile::Builder::new()
                .prefix(".rtk-backup-")
                .tempfile_in(self.managed_bin_dir())
                .map_err(|error| format!("reserve RTK backup path: {error}"))?;
            let backup_path = placeholder.path().to_path_buf();
            placeholder
                .close()
                .map_err(|error| format!("prepare RTK backup path: {error}"))?;
            std::fs::rename(&target, &backup_path).map_err(|error| {
                format!(
                    "back up existing RTK {} to {}: {error}",
                    target.display(),
                    backup_path.display()
                )
            })?;
            Some(backup_path)
        } else {
            None
        };

        if let Err(install_error) = std::fs::rename(staging, &target) {
            if let Some(backup_path) = backup.as_deref() {
                return match std::fs::rename(backup_path, &target) {
                    Ok(()) => Err(format!(
                        "install staged RTK at {}: {install_error}",
                        target.display()
                    )),
                    Err(rollback_error) => Err(format!(
                        "install staged RTK at {}: {install_error}; restore backup {}: {rollback_error}",
                        target.display(),
                        backup_path.display()
                    )),
                };
            }
            return Err(format!(
                "install staged RTK at {}: {install_error}",
                target.display()
            ));
        }
        Ok(backup)
    }

    fn rollback_managed_binary(&self, backup: Option<&Path>) -> Result<(), String> {
        let target = self.managed_executable();
        match std::fs::remove_file(&target) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "remove failed RTK binary {}: {error}",
                    target.display()
                ));
            }
        }
        if let Some(backup_path) = backup {
            std::fs::rename(backup_path, &target).map_err(|error| {
                format!(
                    "restore RTK backup {} to {}: {error}",
                    backup_path.display(),
                    target.display()
                )
            })?;
        }
        Ok(())
    }

    fn enable_policy(&self) -> Result<(), String> {
        std::fs::create_dir_all(&self.config_dir)
            .map_err(|error| format!("create RTK policy directory: {error}"))?;
        let active = self.policy_path();
        if active.is_file() {
            return Ok(());
        }
        let disabled = self.disabled_policy_path();
        if !disabled.is_file() {
            atomic_write(&disabled, POLICY_CONTENT.as_bytes())?;
        }
        std::fs::rename(&disabled, &active).map_err(|error| {
            format!(
                "enable RTK policy {} -> {}: {error}",
                disabled.display(),
                active.display()
            )
        })
    }

    fn disable_policy(&self) -> Result<(), String> {
        let active = self.policy_path();
        if !active.is_file() {
            return Ok(());
        }
        let disabled = self.disabled_policy_path();
        if disabled.exists() {
            std::fs::remove_file(&disabled)
                .map_err(|error| format!("replace stale RTK policy backup: {error}"))?;
        }
        std::fs::rename(&active, &disabled).map_err(|error| {
            format!(
                "disable RTK policy {} -> {}: {error}",
                active.display(),
                disabled.display()
            )
        })?;
        tracing::info!("RTK command policy disabled; installed binary was preserved");
        Ok(())
    }

    fn prepend_managed_bin(&self, command: &mut Command) {
        if !self.policy_enabled() || !self.managed_executable().is_file() {
            return;
        }
        let bin_dir = self.managed_bin_dir();
        let current = std::env::var_os("PATH").unwrap_or_default();
        let paths = std::iter::once(bin_dir).chain(std::env::split_paths(&current));
        if let Ok(path) = std::env::join_paths(paths) {
            command.env("PATH", path);
        }
    }
}

/// Apply the managed RTK directory only to Codex children spawned by R-Code.
pub fn configure_codex_child(command: &mut Command) {
    if let Some(data_dir) = crate::app_paths::default_data_dir() {
        RtkManager::from_data_dir(data_dir).prepend_managed_bin(command);
    }
}

fn rtk_executable_name() -> &'static str {
    if cfg!(windows) {
        "rtk.exe"
    } else {
        "rtk"
    }
}

fn rtk_executable_names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["rtk.exe"]
    } else {
        &["rtk"]
    }
}

fn normalized_path_key(path: &Path) -> OsString {
    #[cfg(windows)]
    {
        OsString::from(path.to_string_lossy().to_lowercase())
    }
    #[cfg(not(windows))]
    {
        path.as_os_str().to_os_string()
    }
}

async fn probe_rtk(path: &Path) -> Option<String> {
    match probe_rtk_outcome(path).await {
        ProbeOutcome::Version(version) => Some(version),
        ProbeOutcome::Blocked(_) | ProbeOutcome::Unavailable => None,
    }
}

#[derive(Debug, Clone)]
enum ProbeOutcome {
    Version(String),
    /// The process could not be started: Windows Defender (or another security product)
    /// blocked or quarantined the binary before it could run.
    Blocked(String),
    Unavailable,
}

#[derive(Debug, Clone)]
enum ProbeRunError {
    /// Only constructed on Windows, where Defender maps ERROR_VIRUS_INFECTED to it.
    #[cfg_attr(not(windows), allow(dead_code))]
    Blocked(String),
    Failed(String),
}

async fn probe_rtk_outcome(path: &Path) -> ProbeOutcome {
    let version_output = match run_probe_outcome(path, &["--version"]).await {
        Ok(output) => output,
        Err(ProbeRunError::Blocked(detail)) => return ProbeOutcome::Blocked(detail),
        Err(ProbeRunError::Failed(detail)) => {
            tracing::debug!(path = ?path, detail = %detail, "RTK probe could not run the binary");
            return ProbeOutcome::Unavailable;
        }
    };
    let version = match first_non_empty_line(&version_output) {
        Some(version) if version.starts_with("rtk ") && version.len() <= 80 => version,
        _ => return ProbeOutcome::Unavailable,
    };
    // The RTK README explicitly warns about a crates.io name collision. `gain --help`
    // distinguishes Rust Token Killer without changing telemetry or analytics state.
    match run_probe_outcome(path, &["gain", "--help"]).await {
        Ok(gain_help) => {
            let normalized = gain_help.to_ascii_lowercase();
            if normalized.contains("token") && normalized.contains("saving") {
                ProbeOutcome::Version(version.to_string())
            } else {
                ProbeOutcome::Unavailable
            }
        }
        Err(ProbeRunError::Blocked(detail)) => ProbeOutcome::Blocked(detail),
        Err(ProbeRunError::Failed(detail)) => {
            tracing::debug!(path = ?path, detail = %detail, "RTK `gain --help` probe failed");
            ProbeOutcome::Unavailable
        }
    }
}

async fn run_probe_outcome(path: &Path, args: &[&str]) -> Result<String, ProbeRunError> {
    let mut command = Command::new(path);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    hide_background_console(command.as_std_mut());
    let output = match timeout(PROBE_TIMEOUT, command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => return Err(spawn_probe_error(&error)),
        Err(_) => return Err(ProbeRunError::Failed("probe timed out".to_string())),
    };
    if !output.status.success() {
        return Err(ProbeRunError::Failed(format!(
            "probe exited with {}",
            output.status
        )));
    }
    let bytes = if output.stdout.is_empty() {
        output.stderr
    } else {
        output.stdout
    };
    String::from_utf8(bytes).map_err(|error| ProbeRunError::Failed(error.to_string()))
}

fn spawn_probe_error(error: &std::io::Error) -> ProbeRunError {
    #[cfg(windows)]
    {
        // ERROR_VIRUS_INFECTED (225) is the Win32 code Defender returns when real-time
        // protection blocks an unsigned binary before CreateProcess completes.
        const ERROR_VIRUS_INFECTED: i32 = 225;
        if error.raw_os_error() == Some(ERROR_VIRUS_INFECTED) {
            return ProbeRunError::Blocked(error.to_string());
        }
    }
    ProbeRunError::Failed(error.to_string())
}

fn security_block_error(target: &Path, detail: &str) -> RtkError {
    RtkError::security_block(format!(
        "installed RTK binary failed verification at {}: {detail}; \
         security software (for example Windows Defender) likely blocked or quarantined the unsigned binary",
        target.display()
    ))
}

fn first_non_empty_line(value: &str) -> Option<&str> {
    value.lines().map(str::trim).find(|line| !line.is_empty())
}

async fn download_bounded(
    client: &reqwest::Client,
    url: &str,
    limit: usize,
    accept: &str,
) -> Result<Vec<u8>, String> {
    let response = client
        .get(url)
        .header(USER_AGENT, format!("R-Code/{}", env!("CARGO_PKG_VERSION")))
        .header(ACCEPT, accept)
        .send()
        .await
        .map_err(|error| format!("download {url}: {error}"))?
        .error_for_status()
        .map_err(|error| format!("download {url}: {error}"))?;
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(format!("download from {url} exceeds {limit} bytes"));
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| format!("read download from {url}: {error}"))?;
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(format!("download from {url} exceeds {limit} bytes"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn rtk_version_matches_release(version: &str, tag: &str) -> bool {
    let expected = tag.strip_prefix('v').unwrap_or(tag);
    version.starts_with("rtk ") && version.split_whitespace().nth(1) == Some(expected)
}

fn release_asset(os: &str, arch: &str) -> Result<RtkReleaseAsset, String> {
    match (os, arch) {
        ("windows", "x86_64") => Ok(RtkReleaseAsset {
            name: "rtk-x86_64-pc-windows-msvc.zip",
            sha256: "34cea9009a8099acdaf85147b971d95f65efabfa63fb3aea7d3e2b73e6f517c3",
        }),
        ("macos", "x86_64") => Ok(RtkReleaseAsset {
            name: "rtk-x86_64-apple-darwin.tar.gz",
            sha256: "9ea02f889d5a2779e4fb700df4587824303c5a57cda22e903e30058079fca0ef",
        }),
        ("macos", "aarch64") => Ok(RtkReleaseAsset {
            name: "rtk-aarch64-apple-darwin.tar.gz",
            sha256: "064151cfc2d50b24d810b06a0af2e41b9c945e83534e4c438c3d3eae607fc3f4",
        }),
        ("linux", "x86_64") => Ok(RtkReleaseAsset {
            name: "rtk-x86_64-unknown-linux-musl.tar.gz",
            sha256: "c4c036fbf181fc55ef329786c8c17e0d427972b053b825944d968a6aafef1ba4",
        }),
        ("linux", "aarch64") => Ok(RtkReleaseAsset {
            name: "rtk-aarch64-unknown-linux-gnu.tar.gz",
            sha256: "80a746dd305ef944ff50ef011ae4ce3878dd5ba88dfe35d859d05498191637c3",
        }),
        _ => Err(format!(
            "RTK has no supported prebuilt release for {os}-{arch}"
        )),
    }
}

#[cfg(windows)]
fn extract_rtk_binary(archive: &[u8]) -> Result<Vec<u8>, String> {
    let reader = Cursor::new(archive);
    let mut zip = zip::ZipArchive::new(reader)
        .map_err(|error| format!("open RTK Windows archive: {error}"))?;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|error| format!("read RTK Windows archive entry: {error}"))?;
        let is_rtk = Path::new(entry.name())
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("rtk.exe"));
        if !entry.is_dir() && is_rtk {
            let size = entry.size();
            return read_bounded_binary(&mut entry, size);
        }
    }
    Err("RTK Windows archive does not contain rtk.exe".to_string())
}

#[cfg(unix)]
fn extract_rtk_binary(archive: &[u8]) -> Result<Vec<u8>, String> {
    let decoder = flate2::read::GzDecoder::new(Cursor::new(archive));
    let mut tar = tar::Archive::new(decoder);
    let entries = tar
        .entries()
        .map_err(|error| format!("open RTK archive: {error}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|error| format!("read RTK archive entry: {error}"))?;
        let is_file = entry.header().entry_type().is_file();
        let is_rtk = entry
            .path()
            .ok()
            .and_then(|path| path.file_name().map(|name| name == "rtk"))
            .unwrap_or(false);
        if is_file && is_rtk {
            let size = entry.size();
            return read_bounded_binary(&mut entry, size);
        }
    }
    Err("RTK archive does not contain the rtk binary".to_string())
}

fn read_bounded_binary(reader: &mut impl Read, declared_size: u64) -> Result<Vec<u8>, String> {
    if declared_size == 0 || declared_size > MAX_BINARY_BYTES {
        return Err("RTK archive entry has an invalid size".to_string());
    }
    let mut bytes = Vec::with_capacity(declared_size as usize);
    reader
        .take(MAX_BINARY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("extract RTK binary: {error}"))?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_BINARY_BYTES {
        return Err("RTK extracted binary exceeds the size limit".to_string());
    }
    Ok(bytes)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "RTK policy path has no parent".to_string())?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("create RTK policy directory: {error}"))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|error| format!("create RTK policy temporary file: {error}"))?;
    temporary
        .write_all(bytes)
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|error| format!("write RTK policy: {error}"))?;
    temporary
        .persist(path)
        .map_err(|error| format!("persist RTK policy: {}", error.error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const PINNED_RELEASE_ASSETS: [(&str, &str, &str, &str); 5] = [
        (
            "windows",
            "x86_64",
            "rtk-x86_64-pc-windows-msvc.zip",
            "34cea9009a8099acdaf85147b971d95f65efabfa63fb3aea7d3e2b73e6f517c3",
        ),
        (
            "macos",
            "x86_64",
            "rtk-x86_64-apple-darwin.tar.gz",
            "9ea02f889d5a2779e4fb700df4587824303c5a57cda22e903e30058079fca0ef",
        ),
        (
            "macos",
            "aarch64",
            "rtk-aarch64-apple-darwin.tar.gz",
            "064151cfc2d50b24d810b06a0af2e41b9c945e83534e4c438c3d3eae607fc3f4",
        ),
        (
            "linux",
            "x86_64",
            "rtk-x86_64-unknown-linux-musl.tar.gz",
            "c4c036fbf181fc55ef329786c8c17e0d427972b053b825944d968a6aafef1ba4",
        ),
        (
            "linux",
            "aarch64",
            "rtk-aarch64-unknown-linux-gnu.tar.gz",
            "80a746dd305ef944ff50ef011ae4ce3878dd5ba88dfe35d859d05498191637c3",
        ),
    ];

    fn verify_fixture_archive_digest(asset: RtkReleaseAsset, archive: &[u8]) -> Result<(), String> {
        let actual = format!("{:x}", Sha256::digest(archive));
        if actual == asset.sha256 {
            Ok(())
        } else {
            Err(format!(
                "RTK archive checksum mismatch for {}: expected {}, got {actual}",
                asset.name, asset.sha256
            ))
        }
    }

    fn transaction_artifacts(manager: &RtkManager) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(manager.managed_bin_dir()) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.starts_with(".rtk-staging-") || name.starts_with(".rtk-backup-")
                    })
            })
            .collect()
    }

    fn compile_fake_rtk(directory: &Path, probe_guard_dir: Option<&Path>) -> Vec<u8> {
        let source_path = directory.join("fake_rtk.rs");
        let binary_path = directory.join(if cfg!(windows) {
            "fake-rtk.exe"
        } else {
            "fake-rtk"
        });
        let guard = probe_guard_dir
            .map(|path| {
                let literal = format!("{:?}", path.to_string_lossy().as_ref());
                r#"
    let state = std::path::Path::new(__STATE__);
    std::fs::create_dir_all(state).unwrap();
    let active = state.join("active-probe");
    let owns_probe = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&active)
        .is_ok();
    if !owns_probe {
        std::fs::write(state.join("overlap"), b"overlap").unwrap();
    }
    let mut calls = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(state.join("calls"))
        .unwrap();
    writeln!(calls, "{}", std::env::args().skip(1).collect::<Vec<_>>().join(" ")).unwrap();
    drop(calls);
    std::thread::sleep(std::time::Duration::from_millis(40));
    if owns_probe {
        let _ = std::fs::remove_file(active);
    }
"#
                .replace("__STATE__", &literal)
            })
            .unwrap_or_default();
        let source = [
            "use std::io::Write;\n\nfn main() {\n",
            guard.as_str(),
            r#"
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.len() == 1 && args[0] == "--version" {
        println!("rtk 0.45.0");
    } else if args.len() == 2 && args[0] == "gain" && args[1] == "--help" {
        println!("Token saving help");
    } else {
        std::process::exit(2);
    }
}
"#,
        ]
        .concat();
        std::fs::write(&source_path, source).unwrap();

        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| OsString::from("rustc"));
        let output = std::process::Command::new(rustc)
            .arg("--edition=2021")
            .arg("-C")
            .arg("debuginfo=0")
            .arg(&source_path)
            .arg("-o")
            .arg(&binary_path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "compile fake RTK: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        std::fs::read(binary_path).unwrap()
    }

    #[test]
    fn release_assets_and_urls_are_fully_pinned_to_v0_45_0() {
        assert_eq!(RTK_RELEASE_TAG, "v0.45.0");
        assert_eq!(
            RTK_RELEASE_PREFIX,
            "https://github.com/rtk-ai/rtk/releases/download/"
        );
        for (os, arch, expected_name, expected_digest) in PINNED_RELEASE_ASSETS {
            let asset = release_asset(os, arch).unwrap();
            assert_eq!(asset.name, expected_name, "asset for {os}-{arch}");
            assert_eq!(asset.sha256, expected_digest, "digest for {os}-{arch}");
            assert_eq!(asset.sha256.len(), 64);
            assert!(asset.sha256.chars().all(|ch| ch.is_ascii_hexdigit()));

            let url = format!("{RTK_RELEASE_PREFIX}{RTK_RELEASE_TAG}/{}", asset.name);
            assert_eq!(
                url,
                format!("https://github.com/rtk-ai/rtk/releases/download/v0.45.0/{expected_name}")
            );
            assert!(!url.contains("/latest/"));
            assert!(!url.contains("checksums"));
        }
        assert!(release_asset("windows", "aarch64").is_err());
    }

    #[test]
    fn incorrect_archive_digest_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let manager = RtkManager::from_data_dir(directory.path());
        std::fs::create_dir_all(manager.managed_bin_dir()).unwrap();
        std::fs::write(manager.managed_executable(), b"existing target").unwrap();
        let asset = release_asset("windows", "x86_64").unwrap();
        let error = verify_fixture_archive_digest(asset, b"tampered archive").unwrap_err();
        assert!(error.contains("checksum mismatch"));
        assert!(error.contains(asset.name));
        assert!(error.contains(asset.sha256));
        assert_eq!(
            std::fs::read(manager.managed_executable()).unwrap(),
            b"existing target"
        );
        assert!(transaction_artifacts(&manager).is_empty());
    }

    #[test]
    fn policy_toggle_renames_one_persistent_file_and_preserves_binary() {
        let directory = tempfile::tempdir().unwrap();
        let manager = RtkManager::from_data_dir(directory.path());
        std::fs::create_dir_all(manager.managed_bin_dir()).unwrap();
        std::fs::write(manager.managed_executable(), b"installed-binary").unwrap();

        manager.enable_policy().unwrap();
        assert!(manager.policy_path().is_file());
        assert!(!manager.disabled_policy_path().exists());
        assert_eq!(manager.command_hint(), COMMAND_HINT);

        manager.disable_policy().unwrap();
        assert!(!manager.policy_path().exists());
        assert!(manager.disabled_policy_path().is_file());
        assert!(manager.managed_executable().is_file());
        assert_eq!(manager.command_hint(), "");

        manager.enable_policy().unwrap();
        assert!(manager.policy_path().is_file());
        assert!(!manager.disabled_policy_path().exists());
    }

    #[test]
    fn enabled_managed_binary_is_prepended_only_to_the_child_path() {
        let directory = tempfile::tempdir().unwrap();
        let manager = RtkManager::from_data_dir(directory.path());
        std::fs::create_dir_all(manager.managed_bin_dir()).unwrap();
        std::fs::write(manager.managed_executable(), b"installed-binary").unwrap();
        manager.enable_policy().unwrap();

        let mut command = Command::new("codex");
        manager.prepend_managed_bin(&mut command);
        let configured_path = command
            .as_std()
            .get_envs()
            .find_map(|(key, value)| {
                key.eq_ignore_ascii_case("PATH")
                    .then(|| value.map(OsString::from))
                    .flatten()
            })
            .expect("child PATH should be overridden");
        assert_eq!(
            std::env::split_paths(&configured_path).next().as_deref(),
            Some(manager.managed_bin_dir().as_path())
        );

        manager.disable_policy().unwrap();
        let mut disabled_command = Command::new("codex");
        manager.prepend_managed_bin(&mut disabled_command);
        assert!(disabled_command.as_std().get_envs().next().is_none());
    }

    #[cfg(windows)]
    #[test]
    fn windows_archive_extracts_only_the_rtk_executable() {
        use zip::write::SimpleFileOptions;

        let writer = Cursor::new(Vec::new());
        let mut archive = zip::ZipWriter::new(writer);
        archive
            .start_file("release/rtk.exe", SimpleFileOptions::default())
            .unwrap();
        archive.write_all(b"verified-rtk-binary").unwrap();
        archive
            .start_file("release/README.md", SimpleFileOptions::default())
            .unwrap();
        archive.write_all(b"not executable").unwrap();
        let bytes = archive.finish().unwrap().into_inner();

        assert_eq!(extract_rtk_binary(&bytes).unwrap(), b"verified-rtk-binary");
    }

    #[tokio::test]
    async fn damaged_target_is_replaced_and_success_cleans_transaction_files() {
        let directory = tempfile::tempdir().unwrap();
        let manager = RtkManager::from_data_dir(directory.path());
        std::fs::create_dir_all(manager.managed_bin_dir()).unwrap();
        std::fs::write(manager.managed_executable(), b"damaged target").unwrap();
        let fixture = compile_fake_rtk(directory.path(), None);

        let staging = manager.stage_managed_binary(&fixture).unwrap();
        let staging_path = staging.to_path_buf();
        let backup = manager
            .activate_staged_binary(staging.as_ref())
            .unwrap()
            .expect("damaged target should be backed up");
        assert!(!staging_path.exists());
        assert_eq!(std::fs::read(&backup).unwrap(), b"damaged target");
        assert_eq!(
            std::fs::read(manager.managed_executable()).unwrap(),
            fixture
        );
        assert_eq!(
            probe_rtk(&manager.managed_executable()).await.as_deref(),
            Some("rtk 0.45.0")
        );

        std::fs::remove_file(&backup).unwrap();
        drop(staging);
        assert!(!backup.exists());
        assert!(transaction_artifacts(&manager).is_empty());
    }

    #[tokio::test]
    async fn failed_probe_restores_the_exact_original_target() {
        let directory = tempfile::tempdir().unwrap();
        let manager = RtkManager::from_data_dir(directory.path());
        std::fs::create_dir_all(manager.managed_bin_dir()).unwrap();
        let original = b"original target bytes, preserved exactly";
        std::fs::write(manager.managed_executable(), original).unwrap();

        let staging = manager.stage_managed_binary(b"not an executable").unwrap();
        let backup = manager
            .activate_staged_binary(staging.as_ref())
            .unwrap()
            .expect("original target should be backed up");
        assert!(probe_rtk(&manager.managed_executable()).await.is_none());

        manager
            .rollback_managed_binary(Some(backup.as_path()))
            .unwrap();
        drop(staging);
        assert_eq!(
            std::fs::read(manager.managed_executable()).unwrap(),
            original
        );
        assert!(!backup.exists());
        assert!(transaction_artifacts(&manager).is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_enable_is_single_flight_without_partial_artifacts() {
        const CALLERS: usize = 6;
        let directory = tempfile::tempdir().unwrap();
        let manager = RtkManager::from_data_dir(directory.path());
        let probe_state = directory.path().join("probe-state");
        let fixture = compile_fake_rtk(directory.path(), Some(&probe_state));
        let staging = manager.stage_managed_binary(&fixture).unwrap();
        assert!(manager
            .activate_staged_binary(staging.as_ref())
            .unwrap()
            .is_none());
        drop(staging);

        let barrier = Arc::new(tokio::sync::Barrier::new(CALLERS + 1));
        let mut tasks = Vec::new();
        for _ in 0..CALLERS {
            let manager = manager.clone();
            let barrier = barrier.clone();
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                manager.set_enabled(true).await
            }));
        }
        barrier.wait().await;

        for task in tasks {
            let status = task.await.unwrap().unwrap();
            assert!(status.enabled);
            assert!(status.available);
            assert!(status.managed);
            assert_eq!(status.version.as_deref(), Some("rtk 0.45.0"));
        }
        let calls = std::fs::read_to_string(probe_state.join("calls")).unwrap();
        assert_eq!(calls.lines().count(), CALLERS * 2);
        assert!(
            !probe_state.join("overlap").exists(),
            "enable probes overlapped despite the single-flight lock"
        );
        assert!(manager.policy_path().is_file());
        assert!(!manager.disabled_policy_path().exists());
        assert_eq!(
            std::fs::read(manager.managed_executable()).unwrap(),
            fixture
        );
        assert!(transaction_artifacts(&manager).is_empty());
    }

    #[test]
    fn installed_version_must_match_the_selected_release() {
        assert!(rtk_version_matches_release("rtk 0.45.0", "v0.45.0"));
        assert!(!rtk_version_matches_release("rtk 0.44.2", "v0.45.0"));
        assert!(!rtk_version_matches_release("other 0.45.0", "v0.45.0"));
    }

    #[test]
    fn command_hint_appends_to_native_agent_prompts() {
        let mut main = String::from("base main prompt");
        let mut subagent = String::from("base subagent prompt");
        append_command_hint(&mut main, COMMAND_HINT);
        append_command_hint(&mut subagent, COMMAND_HINT);
        assert!(main.starts_with("base main prompt"));
        assert!(main.contains("RTK command policy:"));
        assert!(main.contains("must prefer its token-optimized wrappers"));
        assert!(subagent.contains("`rtk cargo`"));
    }

    #[test]
    fn apply_command_hint_is_inert_while_disabled() {
        let manager =
            RtkManager::from_config_dir(std::path::PathBuf::from("D:/nonexistent-rtk-home"));
        let mut main = String::from("base");
        let mut subagent = String::from("base");
        manager.apply_command_hint(&mut main, &mut subagent);
        assert_eq!(main, "base");
        assert_eq!(subagent, "base");
    }
}
