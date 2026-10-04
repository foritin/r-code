//! Content-addressed artifact store.
//!
//! Blobs live under the profile's blobs root keyed by sha256; `put` returns
//! versioned [`ArtifactRef`]s and enforces frame-sized inline input. Large
//! content arrives in chunks. Attachments record owning task ids — ownership
//! metadata travels with the reference, never the bytes.

use r_code_harness_protocol::services::{
    ArtifactRef, ArtifactsPutRequest, ArtifactsReadReply, ArtifactsReadRequest,
};
use sha2::{Digest as _, Sha256};
use std::io;
use std::path::{Path, PathBuf};

const MAX_INLINE_ARTIFACT_FRAME: usize = r_code_harness_protocol::rpc::MAX_FRAME_BYTES;
pub const MAX_OUTPUT_TAIL_BYTES: usize = 64 * 1024;
const MAX_REDACTION_RULES: usize = 128;
const MAX_REDACTION_VALUE_BYTES: usize = 4 * 1024;

/// Literal patterns and secret values removed before a bounded tail is cut.
#[derive(Clone, PartialEq, Eq)]
pub struct OutputTailPolicy {
    max_bytes: usize,
    rules: Vec<Vec<u8>>,
}

impl std::fmt::Debug for OutputTailPolicy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OutputTailPolicy")
            .field("max_bytes", &self.max_bytes)
            .field("rules", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("output-tail redaction policy is invalid")]
pub struct OutputTailPolicyError;

impl OutputTailPolicy {
    pub fn new(
        max_bytes: usize,
        patterns: Vec<Vec<u8>>,
        secret_values: Vec<Vec<u8>>,
    ) -> Result<Self, OutputTailPolicyError> {
        let mut rules = patterns;
        rules.extend(secret_values);
        if !(1..=MAX_OUTPUT_TAIL_BYTES).contains(&max_bytes)
            || rules.len() > MAX_REDACTION_RULES
            || rules
                .iter()
                .any(|rule| rule.is_empty() || rule.len() > MAX_REDACTION_VALUE_BYTES)
        {
            return Err(OutputTailPolicyError);
        }
        rules.sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
        rules.dedup();
        Ok(Self { max_bytes, rules })
    }

    /// Raw rolling-window capacity sufficient to redact a value straddling
    /// the eventual tail boundary before truncation.
    pub fn capture_window_bytes(&self) -> usize {
        self.max_bytes
            + self
                .rules
                .first()
                .map_or(0, |rule| rule.len().saturating_sub(1))
    }
}

/// Errors from the artifact store.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArtifactError {
    #[error("io failure: {0}")]
    Io(String),
    #[error("artifact not found")]
    NotFound,
    #[error("invalid base64 artifact payload")]
    Base64,
    #[error("artifact frame exceeds the inline frame limit")]
    FrameTooLarge,
    #[error("artifact reference failed identity validation")]
    InvalidReference,
    #[error("artifact byte range is invalid")]
    InvalidRange,
    #[error("artifact store belongs to a different task")]
    TaskMismatch,
}

/// Filesystem blob store scoped to one task.
pub struct ArtifactStore {
    root: PathBuf,
    task_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ArtifactMetadata {
    owner_task: String,
    reference: ArtifactRef,
}

impl ArtifactStore {
    /// Compatibility constructor for callers that pass the owner to `put`.
    /// Production routing uses [`Self::for_task`] instead.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            task_id: None,
        }
    }

    pub fn for_task(root: impl Into<PathBuf>, task_id: impl Into<String>) -> Self {
        Self {
            root: root.into(),
            task_id: Some(task_id.into()),
        }
    }

    fn blob_path(&self, digest: &str) -> PathBuf {
        self.root.join(format!("{digest}.blob"))
    }

    fn owner_path(&self, digest: &str) -> PathBuf {
        self.root.join(format!("{digest}.owner"))
    }

    /// Store bytes; returns the versioned reference.
    pub fn put(
        &self,
        request: &ArtifactsPutRequest,
        owner_task: &str,
    ) -> Result<ArtifactRef, ArtifactError> {
        if self
            .task_id
            .as_deref()
            .is_some_and(|task_id| task_id != owner_task)
        {
            return Err(ArtifactError::TaskMismatch);
        }
        if request.data_base64.len() > MAX_INLINE_ARTIFACT_FRAME {
            return Err(ArtifactError::FrameTooLarge);
        }
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&request.data_base64)
            .map_err(|_| ArtifactError::Base64)?;
        self.put_bytes_for_owner(&bytes, request.media_type.clone(), owner_task)
    }

    /// Store host-local bytes without the JSON-RPC inline frame limit.
    pub fn put_bytes(
        &self,
        bytes: &[u8],
        media_type: Option<String>,
    ) -> Result<ArtifactRef, ArtifactError> {
        let owner_task = self.task_id.as_deref().ok_or(ArtifactError::TaskMismatch)?;
        self.put_bytes_for_owner(bytes, media_type, owner_task)
    }

    /// Redact literals before cutting and persisting a deterministic tail.
    pub fn put_redacted_output_tail(
        &self,
        raw_window: &[u8],
        policy: &OutputTailPolicy,
    ) -> Result<ArtifactRef, ArtifactError> {
        let owner_task = self.task_id.as_deref().ok_or(ArtifactError::TaskMismatch)?;
        let redacted = redact_literals(raw_window, &policy.rules);
        let start = redacted.len().saturating_sub(policy.max_bytes);
        self.put_bytes_for_owner(
            &redacted[start..],
            Some("application/vnd.r-code.process-tail".into()),
            owner_task,
        )
    }

    fn put_bytes_for_owner(
        &self,
        bytes: &[u8],
        media_type: Option<String>,
        owner_task: &str,
    ) -> Result<ArtifactRef, ArtifactError> {
        let digest = sha256_hex(bytes);
        let path = self.blob_path(&digest);
        std::fs::create_dir_all(&self.root).map_err(|e| ArtifactError::Io(e.to_string()))?;
        let reference = ArtifactRef {
            schema: ArtifactRef::SCHEMA,
            blob_id: format!("blob:sha256:{digest}"),
            bytes: bytes.len() as u64,
            sha256: digest.clone(),
            media_type,
        };
        if path.exists() {
            let stored_bytes =
                std::fs::read(&path).map_err(|e| ArtifactError::Io(e.to_string()))?;
            if stored_bytes != bytes {
                return Err(ArtifactError::InvalidReference);
            }
            let metadata = self.read_metadata(&digest)?;
            if self.task_id.is_some()
                && (metadata.owner_task != owner_task || metadata.reference != reference)
            {
                return Err(ArtifactError::InvalidReference);
            }
        } else {
            std::fs::write(&path, bytes).map_err(|e| ArtifactError::Io(e.to_string()))?;
        }
        self.write_metadata(
            &digest,
            &ArtifactMetadata {
                owner_task: owner_task.to_string(),
                reference: reference.clone(),
            },
        )?;
        Ok(reference)
    }

    /// Resolve and verify a complete task-owned CAS reference.
    pub fn read_all(&self, reference: &ArtifactRef) -> Result<Vec<u8>, ArtifactError> {
        let digest = validate_reference_shape(reference)?;
        let path = self.blob_path(&digest);
        if !path.is_file() {
            return Err(ArtifactError::NotFound);
        }
        let metadata = self.read_metadata(&digest)?;
        if metadata.reference != *reference
            || self
                .task_id
                .as_deref()
                .is_some_and(|task_id| metadata.owner_task != task_id)
        {
            return Err(ArtifactError::InvalidReference);
        }
        let bytes = std::fs::read(path).map_err(|error| ArtifactError::Io(error.to_string()))?;
        if bytes.len() as u64 != reference.bytes || sha256_hex(&bytes) != digest {
            return Err(ArtifactError::InvalidReference);
        }
        Ok(bytes)
    }

    /// Read a byte range of an artifact.
    pub fn read(
        &self,
        request: &ArtifactsReadRequest,
    ) -> Result<ArtifactsReadReply, ArtifactError> {
        let bytes = self.read_all(&request.artifact)?;
        let total = bytes.len() as u64;
        let start = usize::try_from(request.offset).map_err(|_| ArtifactError::InvalidRange)?;
        if start > bytes.len() {
            return Err(ArtifactError::InvalidRange);
        }
        let end = if request.length == 0 {
            bytes.len()
        } else {
            let length =
                usize::try_from(request.length).map_err(|_| ArtifactError::InvalidRange)?;
            start
                .checked_add(length)
                .ok_or(ArtifactError::InvalidRange)?
                .min(bytes.len())
        };
        use base64::Engine as _;
        let data_base64 = base64::engine::general_purpose::STANDARD.encode(&bytes[start..end]);
        if data_base64.len() > MAX_INLINE_ARTIFACT_FRAME {
            return Err(ArtifactError::FrameTooLarge);
        }
        Ok(ArtifactsReadReply {
            data_base64,
            total_bytes: total,
        })
    }

    /// The owning task of a blob reference (attachment ownership).
    pub fn owner_of(&self, blob_id: &str) -> Option<String> {
        let digest = blob_id.strip_prefix("blob:sha256:")?;
        if !is_sha256(digest) || !self.blob_path(digest).is_file() {
            return None;
        }
        self.read_metadata(digest)
            .ok()
            .map(|metadata| metadata.owner_task)
    }

    /// The store root (for profile layout tests).
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn task_id(&self) -> Option<&str> {
        self.task_id.as_deref()
    }

    fn read_metadata(&self, digest: &str) -> Result<ArtifactMetadata, ArtifactError> {
        let payload =
            std::fs::read(self.owner_path(digest)).map_err(|_| ArtifactError::InvalidReference)?;
        serde_json::from_slice(&payload).map_err(|_| ArtifactError::InvalidReference)
    }

    fn write_metadata(
        &self,
        digest: &str,
        metadata: &ArtifactMetadata,
    ) -> Result<(), ArtifactError> {
        let payload = serde_json::to_vec(metadata).map_err(|_| ArtifactError::InvalidReference)?;
        std::fs::write(self.owner_path(digest), payload)
            .map_err(|e| ArtifactError::Io(e.to_string()))
    }
}

fn redact_literals(input: &[u8], rules: &[Vec<u8>]) -> Vec<u8> {
    let replacement = (0u8..=u8::MAX)
        .find(|byte| !rules.iter().any(|rule| rule.as_slice() == [*byte]))
        .expect("rule limit leaves a replacement byte");
    let mut output = Vec::with_capacity(input.len());
    for byte in input {
        output.push(*byte);
        while let Some(rule) = rules.iter().find(|rule| output.ends_with(rule)) {
            output.truncate(output.len() - rule.len());
            output.push(replacement);
        }
    }
    output
}

fn validate_reference_shape(reference: &ArtifactRef) -> Result<String, ArtifactError> {
    if reference.schema != ArtifactRef::SCHEMA || !is_sha256(&reference.sha256) {
        return Err(ArtifactError::InvalidReference);
    }
    let expected_blob_id = format!("blob:sha256:{}", reference.sha256);
    if reference.blob_id != expected_blob_id {
        return Err(ArtifactError::InvalidReference);
    }
    Ok(reference.sha256.clone())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

impl From<io::Error> for ArtifactError {
    fn from(error: io::Error) -> Self {
        ArtifactError::Io(error.to_string())
    }
}

// ---------------------------------------------------------------------------
// P26A — effect-artifact quota, dedup refs and crash-safe GC.
//
// An effect-owned blob carries a `<digest>.effect` marker beside the CAS
// pair; the marker is what makes it collectable. Task attachments and any
// blob without a marker are never touched by GC. The durable refcount
// lives in the store (`effect_artifact_refs`): a marker whose digest has no
// live ref row is garbage — which also self-heals a put interrupted
// between the blob write and the ref journal (the next GC collects the
// orphan instead of leaving it counted).
// ---------------------------------------------------------------------------

/// The profile-wide ceiling on effect artifacts (stored bytes + active
/// reservations). Deliberately generous; the point is the refusal shape,
/// not the number.
pub const DEFAULT_EFFECT_QUOTA_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Why a launch could not reserve artifact quota.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EffectQuotaError {
    #[error("profile quota exceeded: {used} used + {requested} requested > {limit}")]
    QuotaExceeded {
        used: u64,
        requested: u64,
        limit: u64,
    },
    #[error("disk exhausted: {requested} requested, {available} available")]
    DiskExhausted { requested: u64, available: u64 },
    #[error("io failure: {0}")]
    Io(String),
    #[error("store failure: {0}")]
    Store(String),
}

impl From<r_code_store::v1::ProcessEffectError> for EffectQuotaError {
    fn from(error: r_code_store::v1::ProcessEffectError) -> Self {
        EffectQuotaError::Store(error.to_string())
    }
}

/// Free bytes on the volume holding `root`.
pub fn available_bytes(root: &Path) -> Result<u64, EffectQuotaError> {
    let path = root
        .to_str()
        .ok_or_else(|| EffectQuotaError::Io("root is not UTF-8".into()))?;
    #[cfg(windows)]
    {
        use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let mut available: u64 = 0;
        let ok = unsafe {
            GetDiskFreeSpaceExW(
                wide.as_ptr(),
                &mut available as *mut u64 as *mut _,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(EffectQuotaError::Io(format!(
                "GetDiskFreeSpaceExW failed for {path}"
            )));
        }
        Ok(available)
    }
    #[cfg(not(windows))]
    {
        // statvfs 需要 NUL 终止的 C 路径——&str 的指针不带终止符。
        let c_path = std::ffi::CString::new(path)
            .map_err(|_| EffectQuotaError::Io("root contains NUL".into()))?;
        let mut buffer = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        let code = unsafe { libc::statvfs(c_path.as_ptr(), buffer.as_mut_ptr()) };
        if code != 0 {
            return Err(EffectQuotaError::Io(format!("statvfs failed for {path}")));
        }
        let stats = unsafe { buffer.assume_init() };
        // statvfs 字段宽度平台各异（Linux 皆 u64，macOS f_bavail 是 u32）——
        // 任何统一写法都会在其中一侧触发 unnecessary_cast/useless_conversion，
        // 就地豁免并以 u64 目标明示宽度。
        #[allow(clippy::unnecessary_cast, clippy::useless_conversion)]
        let available = stats.f_bavail as u64 * stats.f_frsize as u64;
        Ok(available)
    }
}

/// The fenced operation identity one effect-artifact put belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectArtifactPut<'a> {
    pub operation_id: &'a str,
    pub owner_id: &'a str,
    pub fencing_epoch: u64,
    /// `manifest` | `before-blob` | `delta-blob` | `output-tail`.
    pub kind: &'a str,
}

/// The profile quota an effect launch must satisfy before it may resume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectQuota {
    pub limit_bytes: u64,
}

impl Default for EffectQuota {
    fn default() -> Self {
        Self {
            limit_bytes: DEFAULT_EFFECT_QUOTA_BYTES,
        }
    }
}

impl EffectQuota {
    /// P26A.1: no command starts without quota. Checks the profile ceiling
    /// (stored effect bytes + actively reserved bytes + the request) and
    /// the volume's free space, then journals the reservation durably.
    pub fn preflight_and_reserve(
        &self,
        store: &r_code_store::v1::V1Store,
        root: &Path,
        operation_id: &str,
        bytes: u64,
    ) -> Result<String, EffectQuotaError> {
        let stored =
            stored_blob_bytes(root).map_err(|error| EffectQuotaError::Io(error.to_string()))?;
        let reserved = store.reserved_active_bytes()?;
        let used = stored.saturating_add(reserved);
        if used.saturating_add(bytes) > self.limit_bytes {
            return Err(EffectQuotaError::QuotaExceeded {
                used,
                requested: bytes,
                limit: self.limit_bytes,
            });
        }
        let available = available_bytes(root)?;
        if available < bytes {
            return Err(EffectQuotaError::DiskExhausted {
                requested: bytes,
                available,
            });
        }
        let reservation_id = format!("reservation-{operation_id}-{bytes}");
        store.reserve_effect_disk(&reservation_id, operation_id, bytes)?;
        Ok(reservation_id)
    }
}

fn stored_blob_bytes(root: &Path) -> io::Result<u64> {
    let mut total = 0u64;
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".blob") {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

/// One GC sweep's outcome: nothing is collected while a ref row is live,
/// and rerunning the sweep changes nothing (idempotent).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectGcOutcome {
    pub collected: usize,
    pub retained: usize,
}

impl ArtifactStore {
    fn effect_marker(&self, digest: &str) -> PathBuf {
        self.root.join(format!("{digest}.effect"))
    }

    /// P26A.2: store bytes for an effect, deduped by digest, and journal
    /// the durable ref that keeps the blob alive. The `.effect` marker is
    /// written after the CAS pair: a crash between the blob write and the
    /// ref journal leaves an orphan the next GC collects (interrupted
    /// refcount recovery), never uncounted live data.
    pub fn put_effect_bytes(
        &self,
        store: &r_code_store::v1::V1Store,
        operation: EffectArtifactPut<'_>,
        bytes: &[u8],
        media_type: Option<String>,
    ) -> Result<ArtifactRef, ArtifactError> {
        let owner_task = self.task_id.as_deref().ok_or(ArtifactError::TaskMismatch)?;
        let reference = self.put_bytes_for_owner(bytes, media_type, owner_task)?;
        let marker = self.effect_marker(&reference.sha256);
        std::fs::write(&marker, b"effect-owned\n").map_err(|e| ArtifactError::Io(e.to_string()))?;
        store
            .own_effect_artifacts(
                operation.operation_id,
                operation.owner_id,
                operation.fencing_epoch,
                &[r_code_store::v1::EffectArtifactRef {
                    digest: reference.sha256.clone(),
                    bytes: reference.bytes,
                    kind: operation.kind.to_string(),
                }],
            )
            .map_err(|error| ArtifactError::Io(format!("durable ref journal: {error}")))?;
        Ok(reference)
    }

    /// P26A.3: collect effect-owned blobs whose digests have no live ref
    /// row. Blobs without an `.effect` marker (task attachments, active
    /// inverse data while any ref lives) are never touched; the sweep is
    /// idempotent — a rerun over an already-swept root collects nothing.
    pub fn effect_gc(
        &self,
        store: &r_code_store::v1::V1Store,
    ) -> Result<EffectGcOutcome, ArtifactError> {
        let mut outcome = EffectGcOutcome {
            collected: 0,
            retained: 0,
        };
        let entries =
            std::fs::read_dir(&self.root).map_err(|e| ArtifactError::Io(e.to_string()))?;
        for entry in entries {
            let entry = entry.map_err(|e| ArtifactError::Io(e.to_string()))?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Some(digest) = name.strip_suffix(".effect") else {
                continue;
            };
            if !is_sha256(digest) {
                continue;
            }
            let live = store
                .effect_artifact_is_live(digest)
                .map_err(|error| ArtifactError::Io(format!("durable ref journal: {error}")))?;
            if live {
                outcome.retained += 1;
                continue;
            }
            let _ = std::fs::remove_file(self.blob_path(digest));
            let _ = std::fs::remove_file(self.owner_path(digest));
            let _ = std::fs::remove_file(entry.path());
            outcome.collected += 1;
        }
        Ok(outcome)
    }
}
