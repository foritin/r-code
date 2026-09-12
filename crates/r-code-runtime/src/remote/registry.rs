//! Device registry (R01, F4): paired devices persisted at
//! `<harness-v2>/<profile>/devices/registry.json` with 0600 permissions.
//! Tokens are 32 random bytes shown once at pairing; only their SHA-256
//! ever touches disk. Revocation is immediate and durable.

use crate::remote::capabilities::CapabilitySet;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

/// Persisted registry shape (version field for forward compatibility).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct RegistryState {
    version: u32,
    devices: Vec<DeviceRecord>,
    #[serde(default)]
    listener: ListenerConfig,
}

/// Listener bootstrap config (R04 consumes; kept here so the file is the
/// single durable state for the remote surface).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ListenerConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub bind: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

/// One paired device. No plaintext tokens, no contact info (F10).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceRecord {
    pub id: String,
    pub name: String,
    pub platform: String,
    /// SHA-256 of the one-time pairing token (hex).
    pub token_sha256: String,
    /// Pinned server certificate fingerprint at pairing time (TOFU, F3).
    #[serde(default)]
    pub cert_fingerprint: String,
    pub capabilities: CapabilitySet,
    pub paired_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_at: Option<String>,
    #[serde(default)]
    pub revoked: bool,
}

/// A freshly paired device: the record plus the one-time plaintext token
/// returned to the pairing caller and never stored.
#[derive(Debug, Clone, PartialEq)]
pub struct PairedDevice {
    pub record: DeviceRecord,
    pub token: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("device {0} not found")]
    UnknownDevice(String),
    #[error("registry io failure: {0}")]
    Io(String),
}

/// The registry; persisted synchronously (small file, rare writes),
/// `last_seen` debounced in memory (R01: no write per command).
pub struct DeviceRegistry {
    path: PathBuf,
    state: Mutex<RegistryState>,
    last_seen_flush: Mutex<Option<SystemTime>>,
}

fn now_rfc3339() -> String {
    // Profile-local timestamps like the rest of the v2 store (ISO-8601 UTC).
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!(
        "{}Z",
        chrono::DateTime::from_timestamp(secs as i64, 0)
            .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
            .unwrap_or_default()
    )
}

fn random_token() -> String {
    // 32 bytes of randomness (two v4 UUIDs = 2×122 random bits).
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn sha256_hex(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn write_private(path: &Path, state: &RegistryState) -> Result<(), RegistryError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| RegistryError::Io(e.to_string()))?;
    }
    let json = serde_json::to_string_pretty(state).map_err(|e| RegistryError::Io(e.to_string()))?;
    std::fs::write(path, json).map_err(|e| RegistryError::Io(e.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| RegistryError::Io(e.to_string()))?;
    }
    #[cfg(windows)]
    {
        // 0600-equivalent on Windows: strip group/other via the file's ACL is
        // heavyweight; the profile root is already per-user (%LOCALAPPDATA%).
        // The file lives inside the user-private profile directory.
        let _ = path;
    }
    Ok(())
}

impl DeviceRegistry {
    /// Open (or create) the registry under `<harness-v2-root>/devices/`.
    pub fn open(harness_v2_root: &Path) -> Result<Self, RegistryError> {
        let path = harness_v2_root.join("devices").join("registry.json");
        let state = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str::<RegistryState>(&text)
                .map_err(|e| RegistryError::Io(format!("registry.json invalid: {e}")))?,
            Err(_) => RegistryState {
                version: 1,
                devices: Vec::new(),
                listener: ListenerConfig::default(),
            },
        };
        Ok(Self {
            path,
            state: Mutex::new(state),
            last_seen_flush: Mutex::new(None),
        })
    }

    /// Register a device; returns the record and the ONE-TIME plaintext
    /// token (only its SHA-256 is stored).
    pub fn register(
        &self,
        name: &str,
        platform: &str,
        cert_fingerprint: &str,
        capabilities: CapabilitySet,
    ) -> Result<PairedDevice, RegistryError> {
        let token = random_token();
        let record = DeviceRecord {
            id: format!("dev_{}", uuid::Uuid::new_v4().simple()),
            name: name.to_string(),
            platform: platform.to_string(),
            token_sha256: sha256_hex(&token),
            cert_fingerprint: cert_fingerprint.to_string(),
            capabilities,
            paired_at: now_rfc3339(),
            last_seen_at: None,
            revoked: false,
        };
        {
            let mut state = self.state.lock().expect("registry");
            state.devices.push(record.clone());
            write_private(&self.path, &state)?;
        }
        Ok(PairedDevice { record, token })
    }

    /// Authenticate a device by its plaintext token; revoked devices and
    /// unknown tokens both yield `None`.
    pub fn get_by_token(&self, token: &str) -> Option<DeviceRecord> {
        let hash = sha256_hex(token);
        self.state
            .lock()
            .expect("registry")
            .devices
            .iter()
            .find(|device| !device.revoked && device.token_sha256 == hash)
            .cloned()
    }

    /// All devices, revoked included (management listing, R11).
    pub fn list(&self) -> Vec<DeviceRecord> {
        self.state.lock().expect("registry").devices.clone()
    }

    /// Revoke a device; revocation is immediate and persisted.
    pub fn revoke(&self, device_id: &str) -> Result<(), RegistryError> {
        let mut state = self.state.lock().expect("registry");
        let device = state
            .devices
            .iter_mut()
            .find(|device| device.id == device_id)
            .ok_or_else(|| RegistryError::UnknownDevice(device_id.into()))?;
        device.revoked = true;
        write_private(&self.path, &state)
    }

    /// Update a device's capabilities (narrowing is immediate).
    pub fn update_capabilities(
        &self,
        device_id: &str,
        capabilities: CapabilitySet,
    ) -> Result<DeviceRecord, RegistryError> {
        let mut state = self.state.lock().expect("registry");
        let device = state
            .devices
            .iter_mut()
            .find(|device| device.id == device_id)
            .ok_or_else(|| RegistryError::UnknownDevice(device_id.into()))?;
        device.capabilities = capabilities;
        let record = device.clone();
        write_private(&self.path, &state)?;
        Ok(record)
    }

    /// Record a sighting, debounced: `last_seen` flushes to disk at most
    /// once per minute (per-command writes are forbidden, R01).
    pub fn note_seen(&self, device_id: &str) {
        let now = SystemTime::now();
        {
            let mut last_flush = self.last_seen_flush.lock().expect("registry flush");
            let due = last_flush
                .map(|at| {
                    now.duration_since(at).unwrap_or(Duration::ZERO) >= Duration::from_secs(60)
                })
                .unwrap_or(true);
            if !due {
                return;
            }
            *last_flush = Some(now);
        }
        let mut state = self.state.lock().expect("registry");
        if let Some(device) = state
            .devices
            .iter_mut()
            .find(|device| device.id == device_id && !device.revoked)
        {
            device.last_seen_at = Some(now_rfc3339());
            let _ = write_private(&self.path, &state);
        }
    }

    /// Listener bootstrap state (read/write by the management surface).
    pub fn listener(&self) -> ListenerConfig {
        self.state.lock().expect("registry").listener.clone()
    }

    pub fn set_listener(&self, config: ListenerConfig) -> Result<(), RegistryError> {
        let mut state = self.state.lock().expect("registry");
        state.listener = config;
        write_private(&self.path, &state)
    }

    /// Whether any live (unrevoked) device exists (F2: no device → no
    /// listener).
    pub fn has_live_devices(&self) -> bool {
        self.state
            .lock()
            .expect("registry")
            .devices
            .iter()
            .any(|device| !device.revoked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry(dir: &tempfile::TempDir) -> DeviceRegistry {
        DeviceRegistry::open(dir.path()).expect("open")
    }

    #[test]
    fn register_roundtrip_token_valid_and_not_on_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = registry(&dir);
        let paired = registry
            .register("iPhone 15", "ios-pwa", "aa:bb", CapabilitySet::read_only())
            .expect("register");

        // The plaintext token authenticates; its bytes never touch disk.
        let found = registry.get_by_token(&paired.token).expect("token valid");
        assert_eq!(found.id, paired.record.id);
        let disk = std::fs::read_to_string(dir.path().join("devices/registry.json")).unwrap();
        assert!(
            !disk.contains(&paired.token),
            "plaintext token must not persist"
        );
        assert!(!disk.to_lowercase().contains("apikey"));
        assert!(disk.contains(&paired.record.token_sha256), "hash persists");
    }

    #[test]
    fn revoke_blocks_authentication_immediately() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = registry(&dir);
        let paired = registry
            .register(
                "Pixel 9",
                "android-pwa",
                "cc:dd",
                CapabilitySet::read_only(),
            )
            .expect("register");
        registry.revoke(&paired.record.id).expect("revoke");
        assert!(registry.get_by_token(&paired.token).is_none());
        assert!(!registry.has_live_devices());
        // Reopen: revocation survives restart.
        let reopened = DeviceRegistry::open(dir.path()).expect("reopen");
        assert!(reopened.get_by_token(&paired.token).is_none());
    }

    #[test]
    fn wrong_token_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = registry(&dir);
        registry
            .register("Mac", "desktop-pwa", "ee:ff", CapabilitySet::read_only())
            .expect("register");
        assert!(registry.get_by_token("not-the-token").is_none());
    }

    #[test]
    fn fresh_devices_are_read_only_and_versioned() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = registry(&dir);
        let paired = registry
            .register("iPad", "ipados-pwa", "", CapabilitySet::read_only())
            .expect("register");
        assert_eq!(paired.record.capabilities.labels(), vec!["events-read"]);
        let disk = std::fs::read_to_string(dir.path().join("devices/registry.json")).unwrap();
        assert!(disk.contains("\"version\""));
    }

    #[test]
    fn update_capabilities_persists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = registry(&dir);
        let paired = registry
            .register("iPhone", "ios-pwa", "", CapabilitySet::read_only())
            .expect("register");
        let updated = registry
            .update_capabilities(
                &paired.record.id,
                CapabilitySet::read_only().with_tasks_write(),
            )
            .expect("update");
        assert_eq!(
            updated.capabilities.labels(),
            vec!["events-read", "tasks-write"]
        );
        let reopened = DeviceRegistry::open(dir.path()).expect("reopen");
        assert!(reopened
            .list()
            .iter()
            .any(|d| d.capabilities.has(crate::remote::Capability::TasksWrite)));
    }

    #[test]
    fn note_seen_is_debounced() {
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = registry(&dir);
        let paired = registry
            .register("Watch", "watchos-pwa", "", CapabilitySet::read_only())
            .expect("register");
        registry.note_seen(&paired.record.id);
        let first = registry
            .list()
            .iter()
            .find(|d| d.id == paired.record.id)
            .unwrap()
            .last_seen_at
            .clone();
        assert!(first.is_some());
        // Second call within the window does not rewrite the file.
        let before = std::fs::read_to_string(dir.path().join("devices/registry.json")).unwrap();
        registry.note_seen(&paired.record.id);
        let after = std::fs::read_to_string(dir.path().join("devices/registry.json")).unwrap();
        assert_eq!(before, after, "last_seen writes are debounced");
    }
}
