//! Shared DTOs and the non-sensitive health receipt cache for subagent providers.
//!
//! Connectivity probes deliberately live outside this foundation. This module owns only bounded,
//! redacted health metadata plus the keyed configuration identity used to validate that metadata.

use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use agent_config::{Config, SubagentPoolConfig, SubagentProviderSource};
use base64::{engine::general_purpose::STANDARD_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use r_code_core::error::ProductError;
use ring::{
    hmac,
    rand::{SecureRandom as _, SystemRandom},
};
use serde::{Deserialize, Serialize};

use crate::settings::SettingsService;

pub const SUBAGENT_HEALTH_RECEIPT_VERSION: u32 = 1;
const SUBAGENT_HEALTH_RECEIPTS_FILE: &str = "subagent-provider-health.json";
const MAX_RECEIPT_FILE_BYTES: u64 = 1024 * 1024;
const MAX_RECEIPTS: usize = 256;
const MAX_RECEIPT_KEY_CHARS: usize = 256;
const MAX_FINGERPRINT_CHARS: usize = 256;
const MAX_EXECUTABLE_FINGERPRINT_BYTES: u64 = 128 * 1024 * 1024;
const FINGERPRINT_PEPPER_BYTES: usize = 32;
const FINGERPRINT_DOMAIN: &[u8] = b"r-code/subagent-provider-fingerprint/v1";
const FINGERPRINT_PREFIX: &str = "blake3-keyed-v1:";
const RECEIPT_ATTESTATION_PREFIX: &str = "hmac-sha256-v1:";
const RECEIPT_ATTESTATION_DOMAIN: &[u8] = b"r-code/subagent-health-receipt-attestation/v1";
const FINGERPRINT_PEPPER_SECRET_KEY: &str = "subagent-provider-fingerprint-pepper-v1";

static FINGERPRINT_PEPPER_LOCK: Mutex<()> = Mutex::new(());

/// Process-memory view of the machine-local pepper. It intentionally implements neither `Debug`
/// nor serde traits and clears its bytes when dropped.
pub struct SubagentFingerprintPepper {
    key: [u8; FINGERPRINT_PEPPER_BYTES],
}

impl Drop for SubagentFingerprintPepper {
    fn drop(&mut self) {
        self.key.fill(0);
    }
}

/// Configuration material for one selectable source. This type intentionally implements neither
/// `Debug` nor serde traits because `credential` may contain a live API secret.
pub struct SubagentProviderFingerprintInput<'a> {
    pub source: &'a SubagentProviderSource,
    pub model: &'a str,
    pub base_url: Option<&'a str>,
    pub protocol: Option<&'a str>,
    pub provider_kind: Option<&'a str>,
    pub credential: Option<&'a str>,
    pub permission_profile: &'a str,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub show_reasoning: bool,
    /// Ordered canonical launch chain. npm-backed CLIs can bind the shim, runtime and entrypoint;
    /// native integrations normally bind one executable.
    pub executable_trust_chain: &'a [PathBuf],
}

/// Opaque keyed identity. Only its encoded receipt value can cross into the non-sensitive JSON.
#[derive(Clone, PartialEq, Eq)]
pub struct SubagentProviderFingerprint(blake3::Hash);

impl SubagentProviderFingerprint {
    pub fn receipt_value(&self) -> String {
        format!("{FINGERPRINT_PREFIX}{}", self.0.to_hex())
    }
}

/// Host-verified launch identity. npm-backed runners must name every security-relevant layer; a
/// caller cannot represent a `.cmd` shim as a complete npm chain by supplying only one path.
/// Paths are never serialized or included in renderer DTOs.
pub enum VerifiedExecutableTrustChain {
    Native {
        binary: PathBuf,
    },
    Npm {
        shim: PathBuf,
        runtime: PathBuf,
        js_entrypoint: PathBuf,
        platform_binary: PathBuf,
        additional_binaries: Vec<PathBuf>,
    },
}

impl VerifiedExecutableTrustChain {
    fn complete_paths(&self) -> Option<Vec<PathBuf>> {
        let paths = match self {
            Self::Native { binary } => {
                if path_is_script(binary) {
                    return None;
                }
                vec![binary.clone()]
            }
            Self::Npm {
                shim,
                runtime,
                js_entrypoint,
                platform_binary,
                additional_binaries,
            } => {
                let mut paths = vec![
                    shim.clone(),
                    runtime.clone(),
                    js_entrypoint.clone(),
                    platform_binary.clone(),
                ];
                paths.extend(additional_binaries.iter().cloned());
                if paths
                    .iter()
                    .enumerate()
                    .any(|(index, path)| paths[..index].contains(path))
                {
                    return None;
                }
                paths
            }
        };
        paths
            .iter()
            .all(|path| !path.as_os_str().is_empty())
            .then_some(paths)
    }
}

fn path_is_script(path: &Path) -> bool {
    let script_extension = path
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "cmd" | "bat" | "ps1" | "js" | "mjs" | "cjs" | "sh"
            )
        });
    if script_extension {
        return true;
    }

    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut prefix = [0_u8; 2];
    file.read_exact(&mut prefix).is_ok() && prefix == *b"#!"
}

/// Host-supplied Codex state. Detection and login checks happen before catalog construction; this
/// value contains no command output or authentication material.
pub struct CodexCliCatalogInput {
    pub configured: bool,
    pub installed: bool,
    pub auth_ready: bool,
    pub adapter_ready: bool,
    pub model: String,
    pub permission_profile: String,
    pub trust_chain: Option<VerifiedExecutableTrustChain>,
}

/// All catalog inputs are Host-owned and already verified. In particular `global_config` must come
/// from the global Settings loader, never from a workspace overlay.
pub struct CatalogBuildInput<'a> {
    pub global_config: &'a Config,
    pub native_permission_profile: &'a str,
    pub codex: &'a CodexCliCatalogInput,
    pub receipts: &'a SubagentHealthReceiptDocument,
    pub pepper: &'a SubagentFingerprintPepper,
    pub now: DateTime<Utc>,
}

/// Load the existing machine pepper, or create it once with the OS CSPRNG and persist it through
/// `SettingsService`'s platform credential backend. Invalid stored material is never regenerated
/// silently: doing so would hide credential-store corruption and invalidate every receipt.
pub fn load_or_create_fingerprint_pepper(
    settings: &SettingsService,
) -> Result<SubagentFingerprintPepper, ProductError> {
    let _guard = FINGERPRINT_PEPPER_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(encoded) = settings.internal_secret(FINGERPRINT_PEPPER_SECRET_KEY)? {
        return decode_fingerprint_pepper(&encoded);
    }

    let mut generated = [0_u8; FINGERPRINT_PEPPER_BYTES];
    SystemRandom::new().fill(&mut generated).map_err(|_| {
        ProductError::SecretError("failed to generate provider fingerprint pepper".into())
    })?;
    let encoded = STANDARD_NO_PAD.encode(generated);
    settings.set_internal_secret(FINGERPRINT_PEPPER_SECRET_KEY, &encoded)?;

    let confirmed = settings
        .internal_secret(FINGERPRINT_PEPPER_SECRET_KEY)?
        .ok_or_else(|| {
            ProductError::SecretError(
                "provider fingerprint pepper was not readable after storage".into(),
            )
        })?;
    let pepper = decode_fingerprint_pepper(&confirmed)?;
    if pepper.key != generated {
        generated.fill(0);
        return Err(ProductError::SecretError(
            "provider fingerprint pepper verification failed".into(),
        ));
    }
    generated.fill(0);
    Ok(pepper)
}

fn decode_fingerprint_pepper(encoded: &str) -> Result<SubagentFingerprintPepper, ProductError> {
    let mut decoded = STANDARD_NO_PAD.decode(encoded).map_err(|_| {
        ProductError::SecretError("stored provider fingerprint pepper is invalid".into())
    })?;
    if decoded.len() != FINGERPRINT_PEPPER_BYTES || STANDARD_NO_PAD.encode(&decoded) != encoded {
        decoded.fill(0);
        return Err(ProductError::SecretError(
            "stored provider fingerprint pepper is invalid".into(),
        ));
    }
    let mut key = [0_u8; FINGERPRINT_PEPPER_BYTES];
    key.copy_from_slice(&decoded);
    decoded.fill(0);
    Ok(SubagentFingerprintPepper { key })
}

/// Compute a keyed, domain-separated identity over every configuration field that can change
/// provider behavior. Executable paths must already be canonical; every member of the ordered
/// launch chain is read in full through the Host-owned executable trust primitive.
pub fn compute_subagent_provider_fingerprint(
    pepper: &SubagentFingerprintPepper,
    input: &SubagentProviderFingerprintInput<'_>,
) -> Result<SubagentProviderFingerprint, ProductError> {
    let mut executable_trust = Vec::with_capacity(input.executable_trust_chain.len());
    for path in input.executable_trust_chain {
        let content_fingerprint = executable_content_fingerprint(path)?;
        executable_trust.push((path_identity_bytes(path), content_fingerprint));
    }

    let (source_kind, source_id) = source_identity(input.source);
    let mut material = Vec::new();
    append_fingerprint_field(&mut material, b"domain", Some(FINGERPRINT_DOMAIN));
    append_fingerprint_field(&mut material, b"source_kind", Some(source_kind.as_bytes()));
    append_fingerprint_field(&mut material, b"source_id", source_id.map(str::as_bytes));
    append_fingerprint_field(&mut material, b"model", Some(input.model.as_bytes()));
    append_fingerprint_field(
        &mut material,
        b"base_url",
        input.base_url.map(str::as_bytes),
    );
    append_fingerprint_field(
        &mut material,
        b"protocol",
        input.protocol.map(str::as_bytes),
    );
    append_fingerprint_field(
        &mut material,
        b"provider_kind",
        input.provider_kind.map(str::as_bytes),
    );
    append_fingerprint_field(
        &mut material,
        b"permission_profile",
        Some(input.permission_profile.as_bytes()),
    );
    let max_tokens = input.max_tokens.map(u32::to_le_bytes);
    append_fingerprint_field(
        &mut material,
        b"max_tokens",
        max_tokens.as_ref().map(<[u8; 4]>::as_slice),
    );
    let temperature = input.temperature.map(|value| value.to_bits().to_le_bytes());
    append_fingerprint_field(
        &mut material,
        b"temperature_bits",
        temperature.as_ref().map(<[u8; 4]>::as_slice),
    );
    append_fingerprint_field(
        &mut material,
        b"show_reasoning",
        Some(&[u8::from(input.show_reasoning)]),
    );
    append_fingerprint_field(
        &mut material,
        b"executable_count",
        Some(&(executable_trust.len() as u64).to_le_bytes()),
    );
    for (path, content_fingerprint) in &executable_trust {
        append_fingerprint_field(&mut material, b"executable_path", Some(path));
        append_fingerprint_field(
            &mut material,
            b"executable_content_fingerprint",
            Some(content_fingerprint.as_bytes()),
        );
    }
    // Append credentials last so every fallible filesystem operation completes before secret
    // bytes enter the temporary buffer. The buffer is cleared immediately after keyed hashing.
    append_fingerprint_field(
        &mut material,
        b"credential",
        input.credential.map(str::as_bytes),
    );
    let fingerprint = blake3::keyed_hash(&pepper.key, &material);
    material.fill(0);
    Ok(SubagentProviderFingerprint(fingerprint))
}

fn executable_content_fingerprint(path: &Path) -> Result<String, ProductError> {
    let canonical = std::fs::canonicalize(path)
        .map_err(|_| ProductError::PermissionError("trusted executable no longer exists".into()))?;
    if canonical != path {
        return Err(ProductError::PermissionError(
            "trusted executable path is not canonical".into(),
        ));
    }
    let metadata = std::fs::metadata(&canonical).map_err(|_| {
        ProductError::PermissionError("unable to inspect trusted executable".into())
    })?;
    if !metadata.is_file() || metadata.len() > MAX_EXECUTABLE_FINGERPRINT_BYTES {
        return Err(ProductError::PermissionError(
            "trusted executable type or size is not allowed".into(),
        ));
    }
    let mut file = std::fs::File::open(&canonical)
        .map_err(|_| ProductError::PermissionError("unable to read trusted executable".into()))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|_| {
            ProductError::PermissionError("unable to fingerprint trusted executable".into())
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("blake3:{}", hasher.finalize().to_hex()))
}

fn source_identity(source: &SubagentProviderSource) -> (&'static str, Option<&str>) {
    match source {
        SubagentProviderSource::ApiProvider { provider_id } => {
            ("api_provider", Some(provider_id.as_str()))
        }
        SubagentProviderSource::CodexCli => ("codex_cli", None),
    }
}

fn append_fingerprint_field(material: &mut Vec<u8>, name: &[u8], value: Option<&[u8]>) {
    material.extend_from_slice(&(name.len() as u64).to_le_bytes());
    material.extend_from_slice(name);
    match value {
        Some(value) => {
            material.push(1);
            material.extend_from_slice(&(value.len() as u64).to_le_bytes());
            material.extend_from_slice(value);
        }
        None => material.push(0),
    }
}

#[cfg(windows)]
fn path_identity_bytes(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt as _;

    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(unix)]
fn path_identity_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt as _;

    path.as_os_str().as_bytes().to_vec()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentProviderHealthState {
    #[default]
    Untested,
    Connected,
    Failed,
    Stale,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationLevel {
    Inference,
    RemoteCatalog,
}

/// Persistable failure categories. Probe output and arbitrary error text are intentionally absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentHealthErrorCode {
    Timeout,
    AuthenticationFailed,
    NetworkUnavailable,
    ProtocolViolation,
    ModelUnavailable,
    PermissionDenied,
    ExecutableUnavailable,
    Unsupported,
    Unknown,
}

/// Runtime abilities are fail-closed. External one-shot runners use `Default` and therefore never
/// claim that they can receive live messages or create descendants through the host.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentProviderCapabilities {
    #[serde(default)]
    pub supports_host_delegation: bool,
    #[serde(default)]
    pub supports_live_messages: bool,
    #[serde(default)]
    pub supports_full_access: bool,
}

impl SubagentProviderCapabilities {
    pub const fn external() -> Self {
        Self {
            supports_host_delegation: false,
            supports_live_messages: false,
            supports_full_access: false,
        }
    }

    pub const fn native() -> Self {
        Self {
            supports_host_delegation: true,
            supports_live_messages: true,
            supports_full_access: true,
        }
    }
}

/// Renderer-facing projection. It intentionally contains no credential, executable arguments,
/// probe prompt/response, stdout or stderr.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentProviderCatalogEntry {
    pub source: SubagentProviderSource,
    pub display_name: String,
    pub model: String,
    pub configured: bool,
    pub ready: bool,
    pub connected: bool,
    pub selectable: bool,
    pub supported: bool,
    pub availability: SubagentProviderAvailability,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    #[serde(default)]
    pub capabilities: SubagentProviderCapabilities,
    pub health: HealthView,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentProviderAvailability {
    Ready,
    NeedsConfiguration,
    NotInstalled,
    LoginRequired,
    TrustRequired,
    Unsupported,
}

/// Renderer-facing health metadata. The keyed fingerprint remains Host-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthView {
    pub state: SubagentProviderHealthState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_level: Option<VerificationLevel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<SubagentHealthErrorCode>,
}

impl Default for HealthView {
    fn default() -> Self {
        Self {
            state: SubagentProviderHealthState::Untested,
            verification_level: None,
            checked_at: None,
            expires_at: None,
            latency_ms: None,
            error: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogSnapshot {
    pub generated_at: DateTime<Utc>,
    pub entries: Vec<SubagentProviderCatalogEntry>,
}

/// Host-recomputed health for one persisted slot. Unlike the catalog, this projection always uses
/// the slot's own model override when resolving its keyed fingerprint and receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentPoolSlotHealth {
    pub slot_id: String,
    pub source: SubagentProviderSource,
    pub model: String,
    pub selectable: bool,
    pub availability: SubagentProviderAvailability,
    pub capabilities: SubagentProviderCapabilities,
    pub health: HealthView,
}

/// Global-only atomic view used by the settings page. `revision` is an opaque keyed CAS token;
/// callers must never derive it from renderer timestamps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentPoolSnapshot {
    pub revision: String,
    pub pool: SubagentPoolConfig,
    pub catalog: CatalogSnapshot,
    pub slot_health: Vec<SubagentPoolSlotHealth>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentProviderProbeRequest {
    pub source: SubagentProviderSource,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentProviderProbeResponse {
    pub result: SubagentProviderCatalogEntry,
    pub snapshot: SubagentPoolSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubagentProviderProbeBatchResponse {
    pub results: Vec<SubagentProviderCatalogEntry>,
    pub snapshot: SubagentPoolSnapshot,
}

/// Persisted receipt metadata. `fingerprint` is an opaque keyed value supplied by the later
/// fingerprint layer; this type never has access to the credential or pepper that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubagentHealthReceipt {
    pub fingerprint: String,
    #[serde(default)]
    pub attestation: String,
    pub status: SubagentProviderHealthState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_level: Option<VerificationLevel>,
    pub checked_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<SubagentHealthErrorCode>,
}

/// Seal every mutable persisted receipt field, including its map key. The resulting attestation is
/// safe to persist but cannot be forged without the machine-local pepper.
pub fn attest_subagent_health_receipt(
    pepper: &SubagentFingerprintPepper,
    receipt_key: &str,
    receipt: &mut SubagentHealthReceipt,
) {
    let material = receipt_attestation_material(receipt_key, receipt);
    let key = hmac::Key::new(hmac::HMAC_SHA256, &pepper.key);
    let tag = hmac::sign(&key, &material);
    receipt.attestation = format!(
        "{RECEIPT_ATTESTATION_PREFIX}{}",
        encode_lower_hex(tag.as_ref())
    );
}

/// Constant-time attestation verification. Malformed or legacy receipts without an attestation
/// fail closed and can never become connected.
pub fn verify_subagent_health_receipt_attestation(
    pepper: &SubagentFingerprintPepper,
    receipt_key: &str,
    receipt: &SubagentHealthReceipt,
) -> bool {
    let Some(tag) = decode_receipt_attestation(&receipt.attestation) else {
        return false;
    };
    let material = receipt_attestation_material(receipt_key, receipt);
    let key = hmac::Key::new(hmac::HMAC_SHA256, &pepper.key);
    hmac::verify(&key, &material, &tag).is_ok()
}

fn receipt_attestation_material(receipt_key: &str, receipt: &SubagentHealthReceipt) -> Vec<u8> {
    let checked_at = receipt
        .checked_at
        .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
    let expires_at = receipt
        .expires_at
        .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true);
    let latency = receipt.latency_ms.map(u64::to_le_bytes);
    let mut material = Vec::new();
    append_fingerprint_field(&mut material, b"domain", Some(RECEIPT_ATTESTATION_DOMAIN));
    append_fingerprint_field(&mut material, b"receipt_key", Some(receipt_key.as_bytes()));
    append_fingerprint_field(
        &mut material,
        b"fingerprint",
        Some(receipt.fingerprint.as_bytes()),
    );
    append_fingerprint_field(
        &mut material,
        b"status",
        Some(health_state_name(receipt.status).as_bytes()),
    );
    append_fingerprint_field(
        &mut material,
        b"verification_level",
        receipt
            .verification_level
            .map(verification_level_name)
            .map(str::as_bytes),
    );
    append_fingerprint_field(&mut material, b"checked_at", Some(checked_at.as_bytes()));
    append_fingerprint_field(&mut material, b"expires_at", Some(expires_at.as_bytes()));
    append_fingerprint_field(
        &mut material,
        b"latency_ms",
        latency.as_ref().map(<[u8; 8]>::as_slice),
    );
    append_fingerprint_field(
        &mut material,
        b"error",
        receipt.error.map(health_error_name).map(str::as_bytes),
    );
    material
}

fn health_state_name(state: SubagentProviderHealthState) -> &'static str {
    match state {
        SubagentProviderHealthState::Untested => "untested",
        SubagentProviderHealthState::Connected => "connected",
        SubagentProviderHealthState::Failed => "failed",
        SubagentProviderHealthState::Stale => "stale",
    }
}

fn verification_level_name(level: VerificationLevel) -> &'static str {
    match level {
        VerificationLevel::Inference => "inference",
        VerificationLevel::RemoteCatalog => "remote_catalog",
    }
}

fn health_error_name(error: SubagentHealthErrorCode) -> &'static str {
    match error {
        SubagentHealthErrorCode::Timeout => "timeout",
        SubagentHealthErrorCode::AuthenticationFailed => "authentication_failed",
        SubagentHealthErrorCode::NetworkUnavailable => "network_unavailable",
        SubagentHealthErrorCode::ProtocolViolation => "protocol_violation",
        SubagentHealthErrorCode::ModelUnavailable => "model_unavailable",
        SubagentHealthErrorCode::PermissionDenied => "permission_denied",
        SubagentHealthErrorCode::ExecutableUnavailable => "executable_unavailable",
        SubagentHealthErrorCode::Unsupported => "unsupported",
        SubagentHealthErrorCode::Unknown => "unknown",
    }
}

fn encode_lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        encoded.push(HEX[usize::from(byte >> 4)] as char);
        encoded.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    encoded
}

fn decode_receipt_attestation(encoded: &str) -> Option<[u8; 32]> {
    let hex = encoded.strip_prefix(RECEIPT_ATTESTATION_PREFIX)?;
    if hex.len() != 64 {
        return None;
    }
    let mut decoded = [0_u8; 32];
    for (index, pair) in hex.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        decoded[index] =
            (decode_lower_hex_nibble(pair[0])? << 4) | decode_lower_hex_nibble(pair[1])?;
    }
    Some(decoded)
}

fn decode_lower_hex_nibble(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        _ => None,
    }
}

/// Resolve persisted health against the current configuration identity. A prior success is never
/// reusable after configuration drift or expiry, and a matching failure remains an explicit
/// failure instead of being mistaken for an untested source.
pub fn evaluate_subagent_provider_health(
    receipt: Option<&SubagentHealthReceipt>,
    receipt_key: &str,
    current_fingerprint: &SubagentProviderFingerprint,
    required_verification_level: VerificationLevel,
    now: DateTime<Utc>,
    pepper: &SubagentFingerprintPepper,
) -> SubagentProviderHealthState {
    let Some(receipt) = receipt else {
        return SubagentProviderHealthState::Untested;
    };
    if !verify_subagent_health_receipt_attestation(pepper, receipt_key, receipt)
        || receipt.fingerprint != current_fingerprint.receipt_value()
        || receipt.expires_at <= now
        || receipt.verification_level != Some(required_verification_level)
    {
        return SubagentProviderHealthState::Stale;
    }
    match receipt.status {
        SubagentProviderHealthState::Connected => SubagentProviderHealthState::Connected,
        SubagentProviderHealthState::Failed => SubagentProviderHealthState::Failed,
        SubagentProviderHealthState::Stale | SubagentProviderHealthState::Untested => {
            SubagentProviderHealthState::Stale
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubagentHealthReceiptDocument {
    pub version: u32,
    #[serde(default)]
    pub receipts: BTreeMap<String, SubagentHealthReceipt>,
}

impl Default for SubagentHealthReceiptDocument {
    fn default() -> Self {
        Self {
            version: SUBAGENT_HEALTH_RECEIPT_VERSION,
            receipts: BTreeMap::new(),
        }
    }
}

/// Stable non-secret lookup key for a source/model pair. Credentials and executable data are not
/// part of the key; they remain bound by the keyed receipt fingerprint itself.
pub fn subagent_health_receipt_key(source: &SubagentProviderSource, model: &str) -> String {
    let (source_kind, source_id) = source_identity(source);
    let mut material = Vec::new();
    append_fingerprint_field(
        &mut material,
        b"domain",
        Some(b"r-code/subagent-health-receipt-key/v1"),
    );
    append_fingerprint_field(&mut material, b"source_kind", Some(source_kind.as_bytes()));
    append_fingerprint_field(&mut material, b"source_id", source_id.map(str::as_bytes));
    append_fingerprint_field(&mut material, b"model", Some(model.as_bytes()));
    format!("source-model-v1:{}", blake3::hash(&material).to_hex())
}

struct CatalogEntryDraft {
    source: SubagentProviderSource,
    display_name: String,
    model: String,
    configured: bool,
    ready: bool,
    supported: bool,
    availability: SubagentProviderAvailability,
    protocol: Option<String>,
    capabilities: SubagentProviderCapabilities,
    fingerprint: Option<SubagentProviderFingerprint>,
    required_verification_level: VerificationLevel,
}

/// Host-only material needed to seal a probe result. The keyed fingerprint is intentionally not
/// serializable and therefore cannot cross the IPC boundary.
pub struct SubagentProviderProbeIdentity {
    pub fingerprint: SubagentProviderFingerprint,
    pub verification_level: VerificationLevel,
}

fn catalog_entry_draft(
    input: &CatalogBuildInput<'_>,
    source: &SubagentProviderSource,
    model: &str,
) -> Option<CatalogEntryDraft> {
    match source {
        SubagentProviderSource::ApiProvider { provider_id } => {
            let provider = input.global_config.providers.get(provider_id)?;
            let configuration_ready = !provider.base_url.trim().is_empty()
                && !model.trim().is_empty()
                && !provider.api_key.trim().is_empty()
                && !input.native_permission_profile.trim().is_empty();
            let fingerprint = compute_subagent_provider_fingerprint(
                input.pepper,
                &SubagentProviderFingerprintInput {
                    source,
                    model,
                    base_url: Some(&provider.base_url),
                    protocol: provider.protocol.as_deref(),
                    provider_kind: provider.provider_kind.as_deref(),
                    credential: Some(&provider.api_key),
                    permission_profile: input.native_permission_profile,
                    max_tokens: provider.max_tokens,
                    temperature: provider.temperature,
                    show_reasoning: provider.show_reasoning,
                    executable_trust_chain: &[],
                },
            )
            .ok();
            let ready = configuration_ready && fingerprint.is_some();
            Some(CatalogEntryDraft {
                source: source.clone(),
                display_name: provider_id.clone(),
                model: model.to_string(),
                configured: true,
                ready,
                supported: true,
                availability: if ready {
                    SubagentProviderAvailability::Ready
                } else {
                    SubagentProviderAvailability::NeedsConfiguration
                },
                protocol: provider.protocol.clone(),
                capabilities: SubagentProviderCapabilities::native(),
                fingerprint,
                required_verification_level: VerificationLevel::Inference,
            })
        }
        SubagentProviderSource::CodexCli => {
            let codex_paths = input
                .codex
                .trust_chain
                .as_ref()
                .and_then(VerifiedExecutableTrustChain::complete_paths);
            let fingerprint = codex_paths.as_ref().and_then(|paths| {
                compute_subagent_provider_fingerprint(
                    input.pepper,
                    &SubagentProviderFingerprintInput {
                        source,
                        model,
                        base_url: None,
                        protocol: Some("codex_cli"),
                        provider_kind: Some("codex"),
                        credential: None,
                        permission_profile: &input.codex.permission_profile,
                        max_tokens: None,
                        temperature: None,
                        show_reasoning: false,
                        executable_trust_chain: paths,
                    },
                )
                .ok()
            });
            let ready = input.codex.configured
                && input.codex.installed
                && input.codex.auth_ready
                && input.codex.adapter_ready
                && !model.trim().is_empty()
                && !input.codex.permission_profile.trim().is_empty()
                && fingerprint.is_some();
            let availability = if !input.codex.installed {
                SubagentProviderAvailability::NotInstalled
            } else if !input.codex.configured || input.codex.permission_profile.trim().is_empty() {
                SubagentProviderAvailability::NeedsConfiguration
            } else if !input.codex.auth_ready {
                SubagentProviderAvailability::LoginRequired
            } else if model.trim().is_empty() {
                SubagentProviderAvailability::NeedsConfiguration
            } else if !input.codex.adapter_ready || fingerprint.is_none() {
                SubagentProviderAvailability::TrustRequired
            } else {
                SubagentProviderAvailability::Ready
            };
            Some(CatalogEntryDraft {
                source: source.clone(),
                display_name: "Codex CLI".to_string(),
                model: model.to_string(),
                configured: input.codex.configured,
                ready,
                supported: true,
                availability,
                protocol: Some("codex_cli".to_string()),
                capabilities: SubagentProviderCapabilities::external(),
                fingerprint,
                required_verification_level: VerificationLevel::RemoteCatalog,
            })
        }
    }
}

/// Resolve a renderer-safe entry for an explicit `(source, model)` pair. Pool slot health and
/// probe results use this path so a slot-level model override never inherits the default model's
/// receipt by accident.
pub fn resolve_subagent_provider_catalog_entry(
    input: &CatalogBuildInput<'_>,
    source: &SubagentProviderSource,
    model: &str,
) -> Option<SubagentProviderCatalogEntry> {
    let draft = catalog_entry_draft(input, source, model)?;
    Some(finish_catalog_entry(
        draft,
        input.receipts,
        input.pepper,
        input.now,
    ))
}

pub fn resolve_subagent_provider_probe_identity(
    input: &CatalogBuildInput<'_>,
    source: &SubagentProviderSource,
    model: &str,
) -> Result<SubagentProviderProbeIdentity, ProductError> {
    let draft = catalog_entry_draft(input, source, model).ok_or_else(|| {
        ProductError::ConfigError("subagent provider source does not exist".into())
    })?;
    Ok(SubagentProviderProbeIdentity {
        fingerprint: draft.fingerprint.ok_or_else(|| {
            ProductError::PermissionError("subagent provider identity is unavailable".into())
        })?,
        verification_level: draft.required_verification_level,
    })
}

/// Build a deterministic global Provider catalog without scanning or executing any CLI. All CLI
/// state is supplied by Host-owned hardened resolvers.
pub fn build_subagent_provider_catalog(input: CatalogBuildInput<'_>) -> CatalogSnapshot {
    let mut entries = Vec::new();
    let mut api_profiles = input.global_config.providers.iter().collect::<Vec<_>>();
    api_profiles.sort_by_key(|(provider_id, _)| *provider_id);
    for (provider_id, provider) in api_profiles {
        let source = SubagentProviderSource::ApiProvider {
            provider_id: provider_id.clone(),
        };
        if let Some(entry) =
            resolve_subagent_provider_catalog_entry(&input, &source, &provider.model)
        {
            entries.push(entry);
        }
    }

    let codex_source = SubagentProviderSource::CodexCli;
    if let Some(entry) =
        resolve_subagent_provider_catalog_entry(&input, &codex_source, &input.codex.model)
    {
        entries.push(entry);
    }

    CatalogSnapshot {
        generated_at: input.now,
        entries,
    }
}

/// Build an opaque CAS token over every Host-owned input that can affect selection. Timestamps
/// generated for a renderer view are intentionally excluded; the token changes only when the
/// pool, a signed receipt, or a current Provider/Codex identity changes.
pub fn compute_subagent_pool_revision(
    input: &CatalogBuildInput<'_>,
    pool: &SubagentPoolConfig,
) -> Result<String, ProductError> {
    let pool_json = serde_json::to_vec(pool).map_err(ProductError::from)?;
    let receipts_json = serde_json::to_vec(input.receipts).map_err(ProductError::from)?;
    let mut material = Vec::new();
    append_fingerprint_field(
        &mut material,
        b"domain",
        Some(b"r-code/subagent-pool-revision/v1"),
    );
    append_fingerprint_field(&mut material, b"pool", Some(&pool_json));
    append_fingerprint_field(&mut material, b"receipts", Some(&receipts_json));
    append_fingerprint_field(
        &mut material,
        b"native_permission_profile",
        Some(input.native_permission_profile.as_bytes()),
    );
    append_fingerprint_field(
        &mut material,
        b"codex_configured",
        Some(&[u8::from(input.codex.configured)]),
    );
    append_fingerprint_field(
        &mut material,
        b"codex_installed",
        Some(&[u8::from(input.codex.installed)]),
    );
    append_fingerprint_field(
        &mut material,
        b"codex_auth_ready",
        Some(&[u8::from(input.codex.auth_ready)]),
    );
    append_fingerprint_field(
        &mut material,
        b"codex_adapter_ready",
        Some(&[u8::from(input.codex.adapter_ready)]),
    );

    let mut identities = input
        .global_config
        .providers
        .iter()
        .map(|(provider_id, provider)| {
            (
                SubagentProviderSource::ApiProvider {
                    provider_id: provider_id.clone(),
                },
                provider.model.clone(),
            )
        })
        .chain(std::iter::once((
            SubagentProviderSource::CodexCli,
            input.codex.model.clone(),
        )))
        .chain(
            pool.slots
                .iter()
                .map(|slot| (slot.source.clone(), slot.model.clone())),
        )
        .collect::<Vec<_>>();
    identities.sort_by(|(left_source, left_model), (right_source, right_model)| {
        source_sort_key(left_source)
            .cmp(&source_sort_key(right_source))
            .then_with(|| left_model.cmp(right_model))
    });
    identities.dedup();
    for (source, model) in identities {
        let (source_kind, source_id) = source_identity(&source);
        append_fingerprint_field(
            &mut material,
            b"identity_source_kind",
            Some(source_kind.as_bytes()),
        );
        append_fingerprint_field(
            &mut material,
            b"identity_source_id",
            source_id.map(str::as_bytes),
        );
        append_fingerprint_field(&mut material, b"identity_model", Some(model.as_bytes()));
        let fingerprint = catalog_entry_draft(input, &source, &model)
            .and_then(|draft| draft.fingerprint)
            .map(|fingerprint| fingerprint.receipt_value());
        append_fingerprint_field(
            &mut material,
            b"identity_fingerprint",
            fingerprint.as_deref().map(str::as_bytes),
        );
    }
    let revision = blake3::keyed_hash(&input.pepper.key, &material);
    material.fill(0);
    Ok(format!("subagent-pool-revision-v1:{}", revision.to_hex()))
}

fn source_sort_key(source: &SubagentProviderSource) -> (&'static str, &str) {
    match source {
        SubagentProviderSource::ApiProvider { provider_id } => ("api_provider", provider_id),
        SubagentProviderSource::CodexCli => ("codex_cli", ""),
    }
}

fn finish_catalog_entry(
    draft: CatalogEntryDraft,
    receipts: &SubagentHealthReceiptDocument,
    pepper: &SubagentFingerprintPepper,
    now: DateTime<Utc>,
) -> SubagentProviderCatalogEntry {
    let receipt_key = subagent_health_receipt_key(&draft.source, &draft.model);
    let receipt = receipts.receipts.get(&receipt_key);
    let health = health_view_for_receipt(
        receipt,
        &receipt_key,
        draft.fingerprint.as_ref(),
        draft.required_verification_level,
        now,
        pepper,
    );
    let connected = health.state == SubagentProviderHealthState::Connected;
    SubagentProviderCatalogEntry {
        source: draft.source,
        display_name: draft.display_name,
        model: draft.model,
        configured: draft.configured,
        ready: draft.ready,
        connected,
        selectable: draft.ready && draft.supported && connected,
        supported: draft.supported,
        availability: draft.availability,
        protocol: draft.protocol,
        capabilities: draft.capabilities,
        health,
    }
}

fn health_view_for_receipt(
    receipt: Option<&SubagentHealthReceipt>,
    receipt_key: &str,
    fingerprint: Option<&SubagentProviderFingerprint>,
    required_verification_level: VerificationLevel,
    now: DateTime<Utc>,
    pepper: &SubagentFingerprintPepper,
) -> HealthView {
    if receipt.is_some_and(|receipt| {
        !verify_subagent_health_receipt_attestation(pepper, receipt_key, receipt)
    }) {
        return HealthView {
            state: SubagentProviderHealthState::Stale,
            ..HealthView::default()
        };
    }
    let state = match fingerprint {
        Some(fingerprint) => evaluate_subagent_provider_health(
            receipt,
            receipt_key,
            fingerprint,
            required_verification_level,
            now,
            pepper,
        ),
        None if receipt.is_some() => SubagentProviderHealthState::Stale,
        None => SubagentProviderHealthState::Untested,
    };
    HealthView {
        state,
        verification_level: receipt.and_then(|receipt| receipt.verification_level),
        checked_at: receipt.map(|receipt| receipt.checked_at),
        expires_at: receipt.map(|receipt| receipt.expires_at),
        latency_ms: receipt.and_then(|receipt| receipt.latency_ms),
        error: (state == SubagentProviderHealthState::Failed)
            .then(|| receipt.and_then(|receipt| receipt.error))
            .flatten(),
    }
}

/// AppData-backed receipt storage. Invalid, oversized, symlinked or unknown-version files are
/// treated as an empty cache so no corrupt disk state can manufacture a connected provider.
#[derive(Debug, Clone)]
pub struct SubagentHealthReceiptStore {
    path: PathBuf,
}

impl SubagentHealthReceiptStore {
    pub fn new(config_dir: impl Into<PathBuf>) -> Self {
        Self {
            path: config_dir.into().join(SUBAGENT_HEALTH_RECEIPTS_FILE),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> SubagentHealthReceiptDocument {
        self.try_load().unwrap_or_default()
    }

    fn try_load(&self) -> Option<SubagentHealthReceiptDocument> {
        let metadata = std::fs::symlink_metadata(&self.path).ok()?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > MAX_RECEIPT_FILE_BYTES
        {
            return None;
        }
        let encoded = std::fs::read(&self.path).ok()?;
        let document: SubagentHealthReceiptDocument = serde_json::from_slice(&encoded).ok()?;
        if document.version != SUBAGENT_HEALTH_RECEIPT_VERSION || !validate_document(&document) {
            return None;
        }
        Some(document)
    }

    /// Atomically replaces the receipt document. The caller owns cross-request serialization;
    /// batch probes can therefore collect results first and commit exactly once.
    pub fn save(&self, document: &SubagentHealthReceiptDocument) -> Result<(), ProductError> {
        let mut sanitized = document.clone();
        sanitized.version = SUBAGENT_HEALTH_RECEIPT_VERSION;
        if sanitized.receipts.len() > MAX_RECEIPTS {
            return Err(ProductError::ConfigError(format!(
                "subagent health receipt cache exceeds {MAX_RECEIPTS} entries"
            )));
        }
        for (key, receipt) in &sanitized.receipts {
            validate_receipt_key(key)?;
            validate_receipt(receipt)?;
        }

        let parent = self.path.parent().ok_or_else(|| {
            ProductError::ConfigError("subagent health receipt path has no parent".into())
        })?;
        std::fs::create_dir_all(parent)?;
        let encoded = serde_json::to_vec_pretty(&sanitized).map_err(ProductError::from)?;
        if encoded.len() as u64 > MAX_RECEIPT_FILE_BYTES {
            return Err(ProductError::ConfigError(
                "subagent health receipt cache exceeds the file-size limit".into(),
            ));
        }

        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(&encoded)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(&self.path)
            .map_err(|error| ProductError::from(error.error))?;
        Ok(())
    }
}

fn validate_document(document: &SubagentHealthReceiptDocument) -> bool {
    document.receipts.len() <= MAX_RECEIPTS
        && document.receipts.iter().all(|(key, receipt)| {
            validate_receipt_key(key).is_ok() && validate_receipt(receipt).is_ok()
        })
}

fn validate_receipt_key(key: &str) -> Result<(), ProductError> {
    if key.is_empty()
        || key.chars().count() > MAX_RECEIPT_KEY_CHARS
        || key.chars().any(char::is_control)
    {
        return Err(ProductError::ConfigError(
            "invalid subagent health receipt key".into(),
        ));
    }
    Ok(())
}

fn validate_receipt(receipt: &SubagentHealthReceipt) -> Result<(), ProductError> {
    if !valid_fingerprint_encoding(&receipt.fingerprint)
        || decode_receipt_attestation(&receipt.attestation).is_none()
        || receipt.fingerprint.chars().count() > MAX_FINGERPRINT_CHARS
        || receipt.fingerprint.chars().any(char::is_control)
        || receipt.expires_at < receipt.checked_at
    {
        return Err(ProductError::ConfigError(
            "invalid subagent health receipt".into(),
        ));
    }
    Ok(())
}

fn valid_fingerprint_encoding(fingerprint: &str) -> bool {
    fingerprint
        .strip_prefix(FINGERPRINT_PREFIX)
        .is_some_and(|value| {
            value.len() == blake3::OUT_LEN * 2
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    use agent_config::ProviderConfig;
    use chrono::Duration as ChronoDuration;
    use std::fs::{File, FileTimes};
    use tempfile::TempDir;

    const NATIVE_PERMISSION_PROFILE: &str = "request_approval";

    fn test_now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-15T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn test_pepper(fill: u8) -> SubagentFingerprintPepper {
        SubagentFingerprintPepper {
            key: [fill; FINGERPRINT_PEPPER_BYTES],
        }
    }

    fn provider_config(api_key: &str, model: &str) -> ProviderConfig {
        ProviderConfig {
            base_url: "https://api.example.test/v1".to_string(),
            api_key: api_key.to_string(),
            model: model.to_string(),
            provider_kind: Some("openai".to_string()),
            max_tokens: Some(4_096),
            temperature: Some(0.2),
            protocol: Some("openai_responses".to_string()),
            show_reasoning: true,
        }
    }

    fn api_fingerprint(
        pepper: &SubagentFingerprintPepper,
        provider_id: &str,
        provider: &ProviderConfig,
    ) -> SubagentProviderFingerprint {
        let source = SubagentProviderSource::ApiProvider {
            provider_id: provider_id.to_string(),
        };
        compute_subagent_provider_fingerprint(
            pepper,
            &SubagentProviderFingerprintInput {
                source: &source,
                model: &provider.model,
                base_url: Some(&provider.base_url),
                protocol: provider.protocol.as_deref(),
                provider_kind: provider.provider_kind.as_deref(),
                credential: Some(&provider.api_key),
                permission_profile: NATIVE_PERMISSION_PROFILE,
                max_tokens: provider.max_tokens,
                temperature: provider.temperature,
                show_reasoning: provider.show_reasoning,
                executable_trust_chain: &[],
            },
        )
        .unwrap()
    }

    fn signed_receipt(
        pepper: &SubagentFingerprintPepper,
        receipt_key: &str,
        fingerprint: &SubagentProviderFingerprint,
        status: SubagentProviderHealthState,
        verification_level: VerificationLevel,
        error: Option<SubagentHealthErrorCode>,
    ) -> SubagentHealthReceipt {
        let now = test_now();
        let mut receipt = SubagentHealthReceipt {
            fingerprint: fingerprint.receipt_value(),
            attestation: String::new(),
            status,
            verification_level: Some(verification_level),
            checked_at: now,
            expires_at: now + ChronoDuration::minutes(10),
            latency_ms: Some(37),
            error,
        };
        attest_subagent_health_receipt(pepper, receipt_key, &mut receipt);
        receipt
    }

    fn assert_receipt_mutation_is_stale(
        pepper: &SubagentFingerprintPepper,
        receipt_key: &str,
        fingerprint: &SubagentProviderFingerprint,
        receipt: &SubagentHealthReceipt,
        mutate: impl FnOnce(&mut SubagentHealthReceipt),
    ) {
        let mut mutated = receipt.clone();
        mutate(&mut mutated);
        assert!(!verify_subagent_health_receipt_attestation(
            pepper,
            receipt_key,
            &mutated
        ));
        assert_eq!(
            evaluate_subagent_provider_health(
                Some(&mutated),
                receipt_key,
                fingerprint,
                VerificationLevel::Inference,
                test_now(),
                pepper,
            ),
            SubagentProviderHealthState::Stale
        );
    }

    fn write_canonical(directory: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let path = directory.join(name);
        std::fs::write(&path, bytes).unwrap();
        std::fs::canonicalize(path).unwrap()
    }

    fn codex_catalog_input(
        trust_chain: Option<VerifiedExecutableTrustChain>,
    ) -> CodexCliCatalogInput {
        CodexCliCatalogInput {
            configured: true,
            installed: true,
            auth_ready: true,
            adapter_ready: true,
            model: "gpt-5-codex".to_string(),
            permission_profile: "read_only".to_string(),
            trust_chain,
        }
    }

    fn build_catalog(
        config: &Config,
        codex: &CodexCliCatalogInput,
        receipts: &SubagentHealthReceiptDocument,
        pepper: &SubagentFingerprintPepper,
    ) -> CatalogSnapshot {
        build_subagent_provider_catalog(CatalogBuildInput {
            global_config: config,
            native_permission_profile: NATIVE_PERMISSION_PROFILE,
            codex,
            receipts,
            pepper,
            now: test_now(),
        })
    }

    #[test]
    fn receipt_hmac_rejects_every_field_map_key_and_fingerprint_tamper() {
        let pepper = test_pepper(0x11);
        let provider = provider_config("sk-receipt-test", "model-a");
        let source = SubagentProviderSource::ApiProvider {
            provider_id: "provider-a".to_string(),
        };
        let fingerprint = api_fingerprint(&pepper, "provider-a", &provider);
        let receipt_key = subagent_health_receipt_key(&source, &provider.model);
        let receipt = signed_receipt(
            &pepper,
            &receipt_key,
            &fingerprint,
            SubagentProviderHealthState::Connected,
            VerificationLevel::Inference,
            None,
        );
        assert!(verify_subagent_health_receipt_attestation(
            &pepper,
            &receipt_key,
            &receipt
        ));
        assert!(!verify_subagent_health_receipt_attestation(
            &test_pepper(0x12),
            &receipt_key,
            &receipt
        ));

        assert!(!verify_subagent_health_receipt_attestation(
            &pepper,
            "different-map-key",
            &receipt
        ));
        assert_eq!(
            evaluate_subagent_provider_health(
                Some(&receipt),
                "different-map-key",
                &fingerprint,
                VerificationLevel::Inference,
                test_now(),
                &pepper,
            ),
            SubagentProviderHealthState::Stale
        );

        let drifted_provider = provider_config("sk-other-secret", "model-a");
        let drifted_fingerprint = api_fingerprint(&pepper, "provider-a", &drifted_provider);
        assert_receipt_mutation_is_stale(&pepper, &receipt_key, &fingerprint, &receipt, |value| {
            value.fingerprint = drifted_fingerprint.receipt_value()
        });
        assert_receipt_mutation_is_stale(&pepper, &receipt_key, &fingerprint, &receipt, |value| {
            value.status = SubagentProviderHealthState::Failed
        });
        assert_receipt_mutation_is_stale(&pepper, &receipt_key, &fingerprint, &receipt, |value| {
            value.verification_level = Some(VerificationLevel::RemoteCatalog)
        });
        assert_receipt_mutation_is_stale(&pepper, &receipt_key, &fingerprint, &receipt, |value| {
            value.checked_at += ChronoDuration::seconds(1)
        });
        assert_receipt_mutation_is_stale(&pepper, &receipt_key, &fingerprint, &receipt, |value| {
            value.expires_at += ChronoDuration::seconds(1)
        });
        assert_receipt_mutation_is_stale(&pepper, &receipt_key, &fingerprint, &receipt, |value| {
            value.latency_ms = Some(999)
        });
        assert_receipt_mutation_is_stale(&pepper, &receipt_key, &fingerprint, &receipt, |value| {
            value.error = Some(SubagentHealthErrorCode::Timeout)
        });
        assert_receipt_mutation_is_stale(&pepper, &receipt_key, &fingerprint, &receipt, |value| {
            let last = value.attestation.pop().unwrap();
            value.attestation.push(if last == '0' { '1' } else { '0' });
        });
    }

    #[test]
    fn expired_wrong_level_and_configuration_drift_are_stale() {
        let pepper = test_pepper(0x22);
        let provider = provider_config("sk-current", "model-a");
        let source = SubagentProviderSource::ApiProvider {
            provider_id: "provider-a".to_string(),
        };
        let fingerprint = api_fingerprint(&pepper, "provider-a", &provider);
        let receipt_key = subagent_health_receipt_key(&source, &provider.model);
        let receipt = signed_receipt(
            &pepper,
            &receipt_key,
            &fingerprint,
            SubagentProviderHealthState::Connected,
            VerificationLevel::Inference,
            None,
        );
        assert_eq!(
            evaluate_subagent_provider_health(
                Some(&receipt),
                &receipt_key,
                &fingerprint,
                VerificationLevel::Inference,
                test_now(),
                &pepper,
            ),
            SubagentProviderHealthState::Connected
        );
        assert_eq!(
            evaluate_subagent_provider_health(
                Some(&receipt),
                &receipt_key,
                &fingerprint,
                VerificationLevel::Inference,
                receipt.expires_at,
                &pepper,
            ),
            SubagentProviderHealthState::Stale
        );
        assert_eq!(
            evaluate_subagent_provider_health(
                Some(&receipt),
                &receipt_key,
                &fingerprint,
                VerificationLevel::RemoteCatalog,
                test_now(),
                &pepper,
            ),
            SubagentProviderHealthState::Stale
        );

        let drifted_provider = provider_config("sk-rotated", "model-a");
        let drifted_fingerprint = api_fingerprint(&pepper, "provider-a", &drifted_provider);
        assert_eq!(
            evaluate_subagent_provider_health(
                Some(&receipt),
                &receipt_key,
                &drifted_fingerprint,
                VerificationLevel::Inference,
                test_now(),
                &pepper,
            ),
            SubagentProviderHealthState::Stale
        );
    }

    #[test]
    fn executable_fingerprint_changes_after_same_size_content_rewrite_with_restored_mtime() {
        let directory = TempDir::new().unwrap();
        let executable = write_canonical(directory.path(), "codex.exe", b"AAAA");
        let original_metadata = std::fs::metadata(&executable).unwrap();
        let original_modified = original_metadata.modified().unwrap();
        let pepper = test_pepper(0x33);
        let source = SubagentProviderSource::CodexCli;
        let fingerprint = |path: &Path| {
            compute_subagent_provider_fingerprint(
                &pepper,
                &SubagentProviderFingerprintInput {
                    source: &source,
                    model: "gpt-5-codex",
                    base_url: None,
                    protocol: Some("codex_cli"),
                    provider_kind: Some("codex"),
                    credential: None,
                    permission_profile: "read_only",
                    max_tokens: None,
                    temperature: None,
                    show_reasoning: false,
                    executable_trust_chain: &[path.to_path_buf()],
                },
            )
            .unwrap()
        };
        let before = fingerprint(&executable);

        std::fs::write(&executable, b"BBBB").unwrap();
        File::options()
            .write(true)
            .open(&executable)
            .unwrap()
            .set_times(FileTimes::new().set_modified(original_modified))
            .unwrap();
        let rewritten_metadata = std::fs::metadata(&executable).unwrap();
        assert_eq!(rewritten_metadata.len(), original_metadata.len());
        assert_eq!(rewritten_metadata.modified().unwrap(), original_modified);

        let after = fingerprint(&executable);
        assert!(before != after);
    }

    #[test]
    fn native_scripts_and_incomplete_or_shim_only_npm_chains_are_rejected() {
        let directory = TempDir::new().unwrap();
        let cmd_script = write_canonical(directory.path(), "codex.cmd", b"@echo off\r\n");
        let shebang_script = write_canonical(directory.path(), "codex", b"#!/bin/sh\n");
        let native_binary = write_canonical(directory.path(), "codex.exe", b"native");
        assert!(VerifiedExecutableTrustChain::Native { binary: cmd_script }
            .complete_paths()
            .is_none());
        assert!(VerifiedExecutableTrustChain::Native {
            binary: shebang_script,
        }
        .complete_paths()
        .is_none());
        assert!(VerifiedExecutableTrustChain::Native {
            binary: native_binary,
        }
        .complete_paths()
        .is_some());

        let shim = write_canonical(directory.path(), "npm.cmd", b"@echo off\r\n");
        let runtime = write_canonical(directory.path(), "node.exe", b"node");
        let entrypoint = write_canonical(directory.path(), "cli.js", b"main();\n");
        let platform = write_canonical(directory.path(), "codex-platform.exe", b"platform");
        assert!(VerifiedExecutableTrustChain::Npm {
            shim: shim.clone(),
            runtime: PathBuf::new(),
            js_entrypoint: entrypoint.clone(),
            platform_binary: platform.clone(),
            additional_binaries: Vec::new(),
        }
        .complete_paths()
        .is_none());
        assert!(VerifiedExecutableTrustChain::Npm {
            shim: shim.clone(),
            runtime: shim.clone(),
            js_entrypoint: shim.clone(),
            platform_binary: shim.clone(),
            additional_binaries: Vec::new(),
        }
        .complete_paths()
        .is_none());
        assert_eq!(
            VerifiedExecutableTrustChain::Npm {
                shim,
                runtime,
                js_entrypoint: entrypoint,
                platform_binary: platform,
                additional_binaries: Vec::new(),
            }
            .complete_paths()
            .unwrap()
            .len(),
            4
        );
    }

    #[test]
    fn untrusted_codex_chains_fail_closed_in_the_catalog() {
        let directory = TempDir::new().unwrap();
        let script = write_canonical(directory.path(), "codex.cmd", b"@echo off\r\n");
        let config = Config::default();
        let receipts = SubagentHealthReceiptDocument::default();
        let pepper = test_pepper(0x44);

        let native_script = codex_catalog_input(Some(VerifiedExecutableTrustChain::Native {
            binary: script.clone(),
        }));
        let snapshot = build_catalog(&config, &native_script, &receipts, &pepper);
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(
            snapshot.entries[0].availability,
            SubagentProviderAvailability::TrustRequired
        );
        assert!(!snapshot.entries[0].ready);
        assert!(!snapshot.entries[0].selectable);

        let shim_only = codex_catalog_input(Some(VerifiedExecutableTrustChain::Npm {
            shim: script.clone(),
            runtime: script.clone(),
            js_entrypoint: script.clone(),
            platform_binary: script,
            additional_binaries: Vec::new(),
        }));
        let snapshot = build_catalog(&config, &shim_only, &receipts, &pepper);
        assert_eq!(
            snapshot.entries[0].availability,
            SubagentProviderAvailability::TrustRequired
        );
        assert!(!snapshot.entries[0].ready);
        assert!(!snapshot.entries[0].selectable);
    }

    #[test]
    fn codex_login_requirement_is_not_masked_by_an_unresolved_default_model() {
        let config = Config::default();
        let receipts = SubagentHealthReceiptDocument::default();
        let pepper = test_pepper(0x45);
        let mut codex = codex_catalog_input(None);
        codex.auth_ready = false;
        codex.model.clear();

        let snapshot = build_catalog(&config, &codex, &receipts, &pepper);
        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(
            snapshot.entries[0].availability,
            SubagentProviderAvailability::LoginRequired
        );
        assert!(!snapshot.entries[0].ready);
    }

    #[test]
    fn catalog_contains_only_api_profiles_and_codex_and_disconnected_entries_are_not_selectable() {
        let directory = TempDir::new().unwrap();
        let codex_binary = write_canonical(directory.path(), "codex.exe", b"native-codex");
        let mut config = Config::default();
        config.providers.insert(
            "provider-b".to_string(),
            provider_config("sk-provider-b", "model-b"),
        );
        config.providers.insert(
            "provider-a".to_string(),
            provider_config("sk-provider-a", "model-a"),
        );
        let codex = codex_catalog_input(Some(VerifiedExecutableTrustChain::Native {
            binary: codex_binary,
        }));
        let snapshot = build_catalog(
            &config,
            &codex,
            &SubagentHealthReceiptDocument::default(),
            &test_pepper(0x55),
        );

        assert_eq!(snapshot.entries.len(), 3);
        assert!(matches!(
            &snapshot.entries[0].source,
            SubagentProviderSource::ApiProvider { provider_id } if provider_id == "provider-a"
        ));
        assert!(matches!(
            &snapshot.entries[1].source,
            SubagentProviderSource::ApiProvider { provider_id } if provider_id == "provider-b"
        ));
        assert_eq!(snapshot.entries[2].source, SubagentProviderSource::CodexCli);
        assert!(snapshot.entries.iter().all(|entry| entry.ready));
        assert!(snapshot.entries.iter().all(|entry| !entry.connected));
        assert!(snapshot.entries.iter().all(|entry| !entry.selectable));

        let encoded = serde_json::to_string(&snapshot)
            .unwrap()
            .to_ascii_lowercase();
        for forbidden in [
            "reasonix",
            "claude_jsonl",
            "opencode_jsonl",
            "gemini_jsonl",
            "ast_grep",
        ] {
            assert!(!encoded.contains(forbidden));
        }
    }

    #[test]
    fn valid_failed_receipt_remains_failed_and_not_selectable() {
        let pepper = test_pepper(0x66);
        let provider = provider_config("sk-failed", "model-a");
        let source = SubagentProviderSource::ApiProvider {
            provider_id: "provider-a".to_string(),
        };
        let fingerprint = api_fingerprint(&pepper, "provider-a", &provider);
        let receipt_key = subagent_health_receipt_key(&source, &provider.model);
        let receipt = signed_receipt(
            &pepper,
            &receipt_key,
            &fingerprint,
            SubagentProviderHealthState::Failed,
            VerificationLevel::Inference,
            Some(SubagentHealthErrorCode::AuthenticationFailed),
        );
        let mut receipts = SubagentHealthReceiptDocument::default();
        receipts.receipts.insert(receipt_key, receipt);
        let mut config = Config::default();
        config.providers.insert("provider-a".to_string(), provider);
        let codex = CodexCliCatalogInput {
            configured: false,
            installed: false,
            auth_ready: false,
            adapter_ready: false,
            model: String::new(),
            permission_profile: String::new(),
            trust_chain: None,
        };

        let snapshot = build_catalog(&config, &codex, &receipts, &pepper);
        let entry = snapshot
            .entries
            .iter()
            .find(|entry| entry.source == source)
            .unwrap();
        assert_eq!(entry.health.state, SubagentProviderHealthState::Failed);
        assert_eq!(
            entry.health.error,
            Some(SubagentHealthErrorCode::AuthenticationFailed)
        );
        assert!(!entry.connected);
        assert!(!entry.selectable);
    }

    #[test]
    fn corrupt_unknown_version_and_oversized_receipt_json_degrade_to_empty() {
        let directory = TempDir::new().unwrap();
        let store = SubagentHealthReceiptStore::new(directory.path());

        std::fs::write(store.path(), b"{not-json").unwrap();
        assert_eq!(store.load(), SubagentHealthReceiptDocument::default());

        std::fs::write(store.path(), br#"{"version":999,"receipts":{}}"#).unwrap();
        assert_eq!(store.load(), SubagentHealthReceiptDocument::default());

        std::fs::write(
            store.path(),
            br#"{"version":1,"receipts":{},"unknown":true}"#,
        )
        .unwrap();
        assert_eq!(store.load(), SubagentHealthReceiptDocument::default());

        let padding = "x".repeat(MAX_RECEIPT_FILE_BYTES as usize);
        let oversized = format!(r#"{{"version":1,"receipts":{{}},"padding":"{padding}"}}"#);
        assert!(oversized.len() as u64 > MAX_RECEIPT_FILE_BYTES);
        std::fs::write(store.path(), oversized).unwrap();
        assert_eq!(store.load(), SubagentHealthReceiptDocument::default());

        let pepper = test_pepper(0x77);
        let provider = provider_config("sk-limit", "model-a");
        let fingerprint = api_fingerprint(&pepper, "provider-a", &provider);
        let mut too_many = SubagentHealthReceiptDocument::default();
        for index in 0..=MAX_RECEIPTS {
            let key = format!("receipt-{index}");
            let receipt = signed_receipt(
                &pepper,
                &key,
                &fingerprint,
                SubagentProviderHealthState::Connected,
                VerificationLevel::Inference,
                None,
            );
            too_many.receipts.insert(key, receipt);
        }
        std::fs::write(store.path(), serde_json::to_vec(&too_many).unwrap()).unwrap();
        assert_eq!(store.load(), SubagentHealthReceiptDocument::default());
        assert!(store.save(&too_many).is_err());
    }

    #[test]
    fn receipts_are_isolated_by_exact_source_and_model() {
        let pepper = test_pepper(0x77);
        let provider_a = provider_config("sk-model-isolation", "model-a");
        let mut provider_b = provider_a.clone();
        provider_b.model = "model-b".to_string();
        let source = SubagentProviderSource::ApiProvider {
            provider_id: "provider-a".to_string(),
        };
        let key_a = subagent_health_receipt_key(&source, "model-a");
        let key_b = subagent_health_receipt_key(&source, "model-b");
        assert_ne!(key_a, key_b);

        let fingerprint_a = api_fingerprint(&pepper, "provider-a", &provider_a);
        let mut receipts = SubagentHealthReceiptDocument::default();
        receipts.receipts.insert(
            key_a,
            signed_receipt(
                &pepper,
                &subagent_health_receipt_key(&source, "model-a"),
                &fingerprint_a,
                SubagentProviderHealthState::Connected,
                VerificationLevel::Inference,
                None,
            ),
        );
        let mut config = Config::default();
        config
            .providers
            .insert("provider-a".to_string(), provider_a);
        let codex = codex_catalog_input(None);
        let input = CatalogBuildInput {
            global_config: &config,
            native_permission_profile: NATIVE_PERMISSION_PROFILE,
            codex: &codex,
            receipts: &receipts,
            pepper: &pepper,
            now: test_now(),
        };

        let model_a = resolve_subagent_provider_catalog_entry(&input, &source, "model-a").unwrap();
        let model_b = resolve_subagent_provider_catalog_entry(&input, &source, "model-b").unwrap();
        assert!(model_a.connected && model_a.selectable);
        assert!(!model_b.connected && !model_b.selectable);
        assert_eq!(model_b.health.state, SubagentProviderHealthState::Untested);
        assert_ne!(
            api_fingerprint(&pepper, "provider-a", &provider_b).receipt_value(),
            fingerprint_a.receipt_value()
        );
    }

    #[test]
    fn pool_revision_tracks_pool_receipts_and_provider_identity_but_not_view_time() {
        let pepper = test_pepper(0x88);
        let provider = provider_config("sk-revision", "model-a");
        let source = SubagentProviderSource::ApiProvider {
            provider_id: "provider-a".to_string(),
        };
        let fingerprint = api_fingerprint(&pepper, "provider-a", &provider);
        let receipt_key = subagent_health_receipt_key(&source, "model-a");
        let mut receipts = SubagentHealthReceiptDocument::default();
        receipts.receipts.insert(
            receipt_key.clone(),
            signed_receipt(
                &pepper,
                &receipt_key,
                &fingerprint,
                SubagentProviderHealthState::Connected,
                VerificationLevel::Inference,
                None,
            ),
        );
        let mut config = Config::default();
        config
            .providers
            .insert("provider-a".to_string(), provider.clone());
        let codex = codex_catalog_input(None);
        let pool = SubagentPoolConfig {
            slots: vec![agent_config::SubagentProviderSlot {
                slot_id: "primary".to_string(),
                source: source.clone(),
                model: "model-a".to_string(),
                weight: 100,
                prompt_template_id: None,
                prompt: "Implement the delegated task.".to_string(),
            }],
        };
        let revision_at = |config: &Config,
                           receipts: &SubagentHealthReceiptDocument,
                           pool: &SubagentPoolConfig,
                           now| {
            compute_subagent_pool_revision(
                &CatalogBuildInput {
                    global_config: config,
                    native_permission_profile: NATIVE_PERMISSION_PROFILE,
                    codex: &codex,
                    receipts,
                    pepper: &pepper,
                    now,
                },
                pool,
            )
            .unwrap()
        };
        let baseline = revision_at(&config, &receipts, &pool, test_now());
        assert_eq!(
            baseline,
            revision_at(
                &config,
                &receipts,
                &pool,
                test_now() + ChronoDuration::hours(6)
            ),
            "renderer snapshot time must not cause a false CAS conflict"
        );

        let mut changed_pool = pool.clone();
        changed_pool.slots[0].prompt = "Review the delegated task.".to_string();
        assert_ne!(
            baseline,
            revision_at(&config, &receipts, &changed_pool, test_now())
        );

        let mut changed_receipts = receipts.clone();
        changed_receipts.receipts.insert(
            receipt_key.clone(),
            signed_receipt(
                &pepper,
                &receipt_key,
                &fingerprint,
                SubagentProviderHealthState::Failed,
                VerificationLevel::Inference,
                Some(SubagentHealthErrorCode::Timeout),
            ),
        );
        assert_ne!(
            baseline,
            revision_at(&config, &changed_receipts, &pool, test_now())
        );

        let mut changed_config = config.clone();
        changed_config
            .providers
            .get_mut("provider-a")
            .unwrap()
            .max_tokens = Some(8_192);
        assert_ne!(
            baseline,
            revision_at(&changed_config, &receipts, &pool, test_now())
        );
    }

    #[test]
    fn probe_identity_uses_inference_for_api_and_remote_catalog_for_codex() {
        let directory = TempDir::new().unwrap();
        let executable = write_canonical(directory.path(), "codex-native.bin", b"trusted-cli");
        let pepper = test_pepper(0x99);
        let mut config = Config::default();
        config.providers.insert(
            "provider-a".to_string(),
            provider_config("sk-level", "model-a"),
        );
        let codex = codex_catalog_input(Some(VerifiedExecutableTrustChain::Native {
            binary: executable,
        }));
        let receipts = SubagentHealthReceiptDocument::default();
        let input = CatalogBuildInput {
            global_config: &config,
            native_permission_profile: NATIVE_PERMISSION_PROFILE,
            codex: &codex,
            receipts: &receipts,
            pepper: &pepper,
            now: test_now(),
        };
        let api = resolve_subagent_provider_probe_identity(
            &input,
            &SubagentProviderSource::ApiProvider {
                provider_id: "provider-a".to_string(),
            },
            "model-a",
        )
        .unwrap();
        let codex = resolve_subagent_provider_probe_identity(
            &input,
            &SubagentProviderSource::CodexCli,
            "gpt-5-codex",
        )
        .unwrap();
        assert_eq!(api.verification_level, VerificationLevel::Inference);
        assert_eq!(codex.verification_level, VerificationLevel::RemoteCatalog);
    }

    #[test]
    fn renderer_dto_and_receipt_do_not_disclose_secret_pepper_or_raw_fingerprint_material() {
        let api_secret = "sk-super-secret-never-render";
        let pepper = test_pepper(0xa5);
        let provider = provider_config(api_secret, "private-model-name");
        let source = SubagentProviderSource::ApiProvider {
            provider_id: "provider-a".to_string(),
        };
        let fingerprint = api_fingerprint(&pepper, "provider-a", &provider);
        let receipt_key = subagent_health_receipt_key(&source, &provider.model);
        let receipt = signed_receipt(
            &pepper,
            &receipt_key,
            &fingerprint,
            SubagentProviderHealthState::Connected,
            VerificationLevel::Inference,
            None,
        );
        let mut receipts = SubagentHealthReceiptDocument::default();
        receipts.receipts.insert(receipt_key, receipt);
        let mut config = Config::default();
        config.providers.insert("provider-a".to_string(), provider);
        let codex = CodexCliCatalogInput {
            configured: false,
            installed: false,
            auth_ready: false,
            adapter_ready: false,
            model: String::new(),
            permission_profile: String::new(),
            trust_chain: None,
        };
        let snapshot = build_catalog(&config, &codex, &receipts, &pepper);

        let renderer_json = serde_json::to_string(&snapshot).unwrap();
        let receipt_json = serde_json::to_string(&receipts).unwrap();
        let pepper_base64 = STANDARD_NO_PAD.encode(pepper.key);
        let pepper_hex = encode_lower_hex(&pepper.key);
        let opaque_fingerprint = fingerprint.receipt_value();
        for sensitive in [
            api_secret,
            "https://api.example.test/v1",
            NATIVE_PERMISSION_PROFILE,
            &pepper_base64,
            &pepper_hex,
            &opaque_fingerprint,
        ] {
            assert!(!renderer_json.contains(sensitive));
        }
        assert!(!renderer_json.contains("fingerprint"));
        assert!(!renderer_json.contains("attestation"));
        for sensitive in [
            api_secret,
            "https://api.example.test/v1",
            "private-model-name",
            NATIVE_PERMISSION_PROFILE,
            &pepper_base64,
            &pepper_hex,
            "credential",
            "pepper",
        ] {
            assert!(!receipt_json.contains(sensitive));
        }
    }
}
