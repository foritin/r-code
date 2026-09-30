//! Stable machine-boot identity from authoritative platform sources.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BootIdentitySource {
    LinuxProcBootId,
    MacOsBootTime,
    WindowsBootEnvironment,
    UnsupportedPlatform,
}

impl fmt::Display for BootIdentitySource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::LinuxProcBootId => "linux-proc-boot-id",
            Self::MacOsBootTime => "macos-boot-time",
            Self::WindowsBootEnvironment => "windows-boot-environment",
            Self::UnsupportedPlatform => "unsupported-platform",
        };
        formatter.write_str(value)
    }
}

impl std::error::Error for BootIdentitySource {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum BootIdentityError {
    #[error("boot identity source is unavailable")]
    SourceUnavailable { source: BootIdentitySource },
    #[error("boot identity source returned malformed data")]
    Malformed { source: BootIdentitySource },
    #[error("boot identity source returned a zero identity")]
    Zero { source: BootIdentitySource },
    #[error("boot identity system query failed")]
    SystemQuery { source: BootIdentitySource },
    #[error("boot identity is unsupported on this platform")]
    Unsupported,
}

/// Canonical, cross-process identity of one operating-system boot.
///
/// The transparent string representation is stable across processes and
/// restarts. Construction and deserialization both reject non-canonical,
/// missing and zero values.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BootIdentity(String);

impl BootIdentity {
    pub fn current() -> Result<Self, BootIdentityError> {
        current_platform_identity()
    }

    pub fn parse(value: impl Into<String>) -> Result<Self, BootIdentityError> {
        let value = value.into();
        validate_canonical_identity(&value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Parse the exact Linux procfs payload (with at most one trailing LF).
    pub fn from_linux_boot_id(payload: &str) -> Result<Self, BootIdentityError> {
        let source = BootIdentitySource::LinuxProcBootId;
        let value = payload.strip_suffix('\n').unwrap_or(payload);
        if value.is_empty() || value.trim() != value || value.contains('\n') || value.contains('\r')
        {
            return Err(BootIdentityError::Malformed { source });
        }
        let uuid = canonical_uuid(value, source)?;
        if uuid != value {
            return Err(BootIdentityError::Malformed { source });
        }
        Self::parse(format!("linux:{uuid}"))
    }

    /// Construct the canonical macOS `kern.boottime` identity.
    pub fn from_macos_boottime(seconds: i64, microseconds: i64) -> Result<Self, BootIdentityError> {
        let source = BootIdentitySource::MacOsBootTime;
        if seconds <= 0 || !(0..1_000_000).contains(&microseconds) {
            return Err(if seconds == 0 && microseconds == 0 {
                BootIdentityError::Zero { source }
            } else {
                BootIdentityError::Malformed { source }
            });
        }
        Self::parse(format!("macos:{seconds}:{microseconds:06}"))
    }

    /// Construct a canonical Windows identity from its GUID text form.
    pub fn from_windows_boot_identifier(value: &str) -> Result<Self, BootIdentityError> {
        let source = BootIdentitySource::WindowsBootEnvironment;
        let uuid = canonical_uuid(value, source)?;
        if uuid != value {
            return Err(BootIdentityError::Malformed { source });
        }
        Self::parse(format!("windows:{uuid}"))
    }
}

impl fmt::Display for BootIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for BootIdentity {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Serialize for BootIdentity {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for BootIdentity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

fn validate_canonical_identity(value: &str) -> Result<(), BootIdentityError> {
    if let Some(uuid) = value.strip_prefix("linux:") {
        let source = BootIdentitySource::LinuxProcBootId;
        if canonical_uuid(uuid, source)? == uuid {
            return Ok(());
        }
        return Err(BootIdentityError::Malformed { source });
    }
    if let Some(uuid) = value.strip_prefix("windows:") {
        let source = BootIdentitySource::WindowsBootEnvironment;
        if canonical_uuid(uuid, source)? == uuid {
            return Ok(());
        }
        return Err(BootIdentityError::Malformed { source });
    }
    if let Some(parts) = value.strip_prefix("macos:") {
        let source = BootIdentitySource::MacOsBootTime;
        let Some((seconds, micros)) = parts.split_once(':') else {
            return Err(BootIdentityError::Malformed { source });
        };
        if micros.len() != 6
            || !seconds.bytes().all(|byte| byte.is_ascii_digit())
            || !micros.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(BootIdentityError::Malformed { source });
        }
        let seconds = seconds
            .parse::<i64>()
            .map_err(|_| BootIdentityError::Malformed { source })?;
        let micros = micros
            .parse::<i64>()
            .map_err(|_| BootIdentityError::Malformed { source })?;
        if seconds == 0 && micros == 0 {
            return Err(BootIdentityError::Zero { source });
        }
        if seconds > 0
            && (0..1_000_000).contains(&micros)
            && format!("macos:{seconds}:{micros:06}") == value
        {
            return Ok(());
        }
        return Err(BootIdentityError::Malformed { source });
    }
    Err(BootIdentityError::Malformed {
        source: BootIdentitySource::UnsupportedPlatform,
    })
}

fn canonical_uuid(value: &str, source: BootIdentitySource) -> Result<String, BootIdentityError> {
    let bytes = value.as_bytes();
    if bytes.len() != 36
        || [8, 13, 18, 23].iter().any(|index| bytes[*index] != b'-')
        || bytes
            .iter()
            .enumerate()
            .any(|(index, byte)| ![8, 13, 18, 23].contains(&index) && !byte.is_ascii_hexdigit())
    {
        return Err(BootIdentityError::Malformed { source });
    }
    let uuid = uuid::Uuid::parse_str(value).map_err(|_| BootIdentityError::Malformed { source })?;
    if uuid.is_nil() {
        return Err(BootIdentityError::Zero { source });
    }
    Ok(uuid.hyphenated().to_string())
}

#[cfg(target_os = "linux")]
fn current_platform_identity() -> Result<BootIdentity, BootIdentityError> {
    let payload = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").map_err(|_| {
        BootIdentityError::SourceUnavailable {
            source: BootIdentitySource::LinuxProcBootId,
        }
    })?;
    BootIdentity::from_linux_boot_id(&payload)
}

#[cfg(target_os = "macos")]
fn current_platform_identity() -> Result<BootIdentity, BootIdentityError> {
    let source = BootIdentitySource::MacOsBootTime;
    let mut value = std::mem::MaybeUninit::<libc::timeval>::zeroed();
    let mut length = std::mem::size_of::<libc::timeval>();
    let name = b"kern.boottime\0";
    let status = unsafe {
        libc::sysctlbyname(
            name.as_ptr().cast(),
            value.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 || length != std::mem::size_of::<libc::timeval>() {
        return Err(BootIdentityError::SystemQuery { source });
    }
    let value = unsafe { value.assume_init() };
    BootIdentity::from_macos_boottime(value.tv_sec, i64::from(value.tv_usec))
}

#[cfg(windows)]
fn current_platform_identity() -> Result<BootIdentity, BootIdentityError> {
    use windows_sys::Wdk::System::SystemInformation::NtQuerySystemInformation;

    const SYSTEM_BOOT_ENVIRONMENT_INFORMATION_CLASS: i32 = 90;
    let source = BootIdentitySource::WindowsBootEnvironment;
    let mut information = std::mem::MaybeUninit::<SystemBootEnvironmentInformation>::zeroed();
    let mut returned = 0u32;
    let expected = u32::try_from(std::mem::size_of::<SystemBootEnvironmentInformation>())
        .map_err(|_| BootIdentityError::SystemQuery { source })?;
    let status = unsafe {
        NtQuerySystemInformation(
            SYSTEM_BOOT_ENVIRONMENT_INFORMATION_CLASS,
            information.as_mut_ptr().cast(),
            expected,
            &mut returned,
        )
    };
    if status < 0 || returned < expected {
        return Err(BootIdentityError::SystemQuery { source });
    }
    let information = unsafe { information.assume_init() };
    BootIdentity::from_windows_boot_identifier(&format_guid(&information.boot_identifier))
}

#[cfg(windows)]
#[repr(C)]
struct SystemBootEnvironmentInformation {
    boot_identifier: NativeGuid,
    firmware_type: i32,
    boot_flags: u64,
}

#[cfg(windows)]
#[repr(C)]
struct NativeGuid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[cfg(windows)]
fn format_guid(guid: &NativeGuid) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        guid.data1,
        guid.data2,
        guid.data3,
        guid.data4[0],
        guid.data4[1],
        guid.data4[2],
        guid.data4[3],
        guid.data4[4],
        guid.data4[5],
        guid.data4[6],
        guid.data4[7]
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn current_platform_identity() -> Result<BootIdentity, BootIdentityError> {
    Err(BootIdentityError::Unsupported)
}
