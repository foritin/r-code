//! P26 — the bounded, stable before/delta scanner.
//!
//! Captures content+physical identities for every in-scope file (links,
//! out-of-scope paths, pre-existing ignored paths and ephemeral roots are
//! rejected from the manifest), under hard bounds — entries, logical bytes
//! and a wall-clock deadline; any overflow is `Unavailable`, so an
//! unbounded or ambiguous scan can never receipt a launch. Deltas between
//! two manifests are deterministic (path-sorted create/edit/delete with a
//! binary distinction), every captured file form is CAS-able through the
//! P26A effect-artifact journal, and a concurrent edit during the initial
//! scan aborts before any user code runs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::services::artifacts::{ArtifactStore, EffectArtifactPut};
use r_code_store::v1::V1Store;

/// Hard bounds (P26.1). Overflow is `Unavailable`, never a partial scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanBounds {
    pub max_entries: usize,
    pub max_logical_bytes: u64,
    pub max_duration: Duration,
}

pub const MAX_SCAN_ENTRIES: usize = 200_000;
pub const MAX_SCAN_LOGICAL_BYTES: u64 = 20 * 1024 * 1024 * 1024;
pub const MAX_SCAN_DURATION: Duration = Duration::from_secs(120);

impl Default for ScanBounds {
    fn default() -> Self {
        Self {
            max_entries: MAX_SCAN_ENTRIES,
            max_logical_bytes: MAX_SCAN_LOGICAL_BYTES,
            max_duration: MAX_SCAN_DURATION,
        }
    }
}

/// What the scanner captures and what it rejects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanPolicy {
    /// The single scope root; nothing outside it is ever in the manifest.
    pub root: PathBuf,
    /// Repository-relative prefixes excluded from capture (pre-existing
    /// ignored paths: their bytes are foreign and always preserved).
    pub ignored: Vec<String>,
    /// Roots recreated per run: excluded from the before capture.
    pub ephemeral: Vec<String>,
}

impl ScanPolicy {
    /// The workspace's default policy: the bound root, INV-06's `.git`
    /// forever ignored, nothing ephemeral.
    pub fn for_workspace_root(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            ignored: vec![".git".to_string()],
            ephemeral: Vec::new(),
        }
    }

    fn rejects(&self, relative: &str) -> bool {
        let normalized = relative.replace('\\', "/");
        self.ignored
            .iter()
            .chain(self.ephemeral.iter())
            .any(|prefix| normalized == *prefix || normalized.starts_with(&format!("{prefix}/")))
    }
}

/// Content+physical identity of one file. Physical identity is the stat
/// pair; content identity is the sha256 of the bytes observed between two
/// identical stats (a change in flight aborts instead of guessing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileIdentity {
    /// Root-relative, forward slashes.
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    /// Millisecond-resolution modified time.
    pub modified_ms: i64,
    /// NUL byte seen in the observed bytes.
    pub is_binary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanManifest {
    /// Path-sorted at construction: manifests are deterministic.
    pub files: Vec<FileIdentity>,
    pub logical_bytes: u64,
}

/// Why a scan or revalidation failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScanError {
    #[error("scan unavailable: {0}")]
    Unavailable(String),
    #[error("symlink rejected: {0}")]
    Link(String),
    #[error("concurrent edit aborted the scan: {0}")]
    ConcurrentEdit(String),
    #[error("io failure: {0}")]
    Io(String),
}

fn io_error(error: std::io::Error) -> ScanError {
    ScanError::Io(error.to_string())
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn modified_ms(metadata: &std::fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|moment| moment.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn classify(bytes: &[u8]) -> bool {
    bytes.len() <= 8192 && bytes.contains(&0) || bytes[..bytes.len().min(8192)].contains(&0)
}

/// Capture the manifest under the policy and bounds. Symlinks are rejected
/// anywhere in scope; ignored and ephemeral prefixes are excluded (their
/// bytes are never read, never written); a file whose stat pair changes
/// between the two stats bracketing its read aborts the whole scan —
/// before any user code consumes the result.
pub fn capture_manifest(
    policy: &ScanPolicy,
    bounds: &ScanBounds,
) -> Result<ScanManifest, ScanError> {
    let deadline = Instant::now() + bounds.max_duration;
    let mut files = Vec::new();
    let mut logical_bytes = 0u64;
    let mut stack = vec![policy.root.clone()];
    while let Some(directory) = stack.pop() {
        if Instant::now() >= deadline {
            return Err(ScanError::Unavailable("scan deadline exceeded".into()));
        }
        let mut entries: Vec<_> = std::fs::read_dir(&directory)
            .map_err(io_error)?
            .collect::<Result<_, _>>()
            .map_err(io_error)?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let relative = relative_path(&policy.root, &path);
            if policy.rejects(&relative) {
                continue;
            }
            let metadata = entry.metadata().map_err(io_error)?;
            if metadata.is_symlink() {
                return Err(ScanError::Link(relative));
            }
            if metadata.is_dir() {
                stack.push(path);
                continue;
            }
            if files.len() >= bounds.max_entries {
                return Err(ScanError::Unavailable(format!(
                    "entry bound exceeded ({} files)",
                    bounds.max_entries
                )));
            }
            if logical_bytes.saturating_add(metadata.len()) > bounds.max_logical_bytes {
                return Err(ScanError::Unavailable("logical byte bound exceeded".into()));
            }
            let first = metadata.clone();
            let bytes = std::fs::read(&path).map_err(io_error)?;
            let second = std::fs::metadata(&path).map_err(io_error)?;
            let stable = second.len() == first.len() && modified_ms(&second) == modified_ms(&first);
            if !stable {
                return Err(ScanError::ConcurrentEdit(relative));
            }
            logical_bytes += bytes.len() as u64;
            files.push(FileIdentity {
                path: relative,
                sha256: crate::services::artifacts::sha256_hex(&bytes),
                bytes: bytes.len() as u64,
                modified_ms: modified_ms(&first),
                is_binary: classify(&bytes),
            });
        }
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(ScanManifest {
        files,
        logical_bytes,
    })
}

/// One deterministic delta entry between two manifests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeltaEntry {
    pub path: String,
    pub kind: DeltaKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaKind {
    Created,
    Edited,
    Deleted,
    Binary,
}

/// Path-sorted deterministic delta: created (after only), deleted (before
/// only), edited (content differs) — with binary content on either side
/// reported as `Binary` so appliers never run text logic over opaque bytes.
pub fn delta(before: &ScanManifest, after: &ScanManifest) -> Vec<DeltaEntry> {
    let before_map: BTreeMap<&str, &FileIdentity> = before
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    let after_map: BTreeMap<&str, &FileIdentity> = after
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    let mut paths: Vec<&str> = before_map.keys().chain(after_map.keys()).copied().collect();
    paths.sort_unstable();
    paths.dedup();
    let mut entries = Vec::new();
    for path in paths {
        let kind = match (before_map.get(path), after_map.get(path)) {
            (None, Some(_)) => DeltaKind::Created,
            (Some(_), None) => DeltaKind::Deleted,
            (Some(from), Some(to)) => {
                if from.sha256 == to.sha256 {
                    continue;
                }
                if from.is_binary || to.is_binary {
                    DeltaKind::Binary
                } else {
                    DeltaKind::Edited
                }
            }
            (None, None) => continue,
        };
        entries.push(DeltaEntry {
            path: path.to_string(),
            kind,
        });
    }
    entries
}

/// P26.3: revalidate every identity immediately before a launch release.
/// Any drift — a missing file, a changed stat pair, changed content —
/// refuses; a stale manifest may never release a launch.
pub fn revalidate(manifest: &ScanManifest, policy: &ScanPolicy) -> Result<(), ScanError> {
    for file in &manifest.files {
        let path = policy.root.join(&file.path);
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => return Err(ScanError::ConcurrentEdit(file.path.clone())),
        };
        if metadata.is_symlink()
            || metadata.len() != file.bytes
            || modified_ms(&metadata) != file.modified_ms
        {
            return Err(ScanError::ConcurrentEdit(file.path.clone()));
        }
        let bytes = std::fs::read(&path).map_err(io_error)?;
        if crate::services::artifacts::sha256_hex(&bytes) != file.sha256 {
            return Err(ScanError::ConcurrentEdit(file.path.clone()));
        }
    }
    Ok(())
}

/// P26.4: capture twice after exit — any nonempty delta between the two
/// captures is post-exit drift the reconciler must treat as foreign.
pub fn post_exit_drift(first: &ScanManifest, second: &ScanManifest) -> Option<Vec<DeltaEntry>> {
    let entries = delta(first, second);
    if entries.is_empty() {
        None
    } else {
        Some(entries)
    }
}

/// P26.2: CAS every captured form — the manifest itself and each file's
/// observed bytes — into the effect-artifact journal (deduped by digest,
/// refcounted by the P26A store rows). A file whose bytes changed since
/// the capture aborts the CAS pass (the journal never records stale
/// content).
pub fn cas_manifest(
    store: &V1Store,
    artifacts: &ArtifactStore,
    operation: EffectArtifactPut<'_>,
    policy: &ScanPolicy,
    manifest: &ScanManifest,
) -> Result<(), ScanError> {
    let manifest_json = serde_json::to_vec(&manifest_to_owned(manifest))
        .map_err(|error| ScanError::Io(error.to_string()))?;
    artifacts
        .put_effect_bytes(store, operation, &manifest_json, None)
        .map_err(|error| ScanError::Io(error.to_string()))?;
    for file in &manifest.files {
        let bytes = std::fs::read(policy.root.join(&file.path)).map_err(io_error)?;
        if crate::services::artifacts::sha256_hex(&bytes) != file.sha256 {
            return Err(ScanError::ConcurrentEdit(file.path.clone()));
        }
        artifacts
            .put_effect_bytes(store, operation, &bytes, None)
            .map_err(|error| ScanError::Io(error.to_string()))?;
    }
    Ok(())
}

/// The manifest's wire form (paths, digests, identities — no bytes). The
/// owned mirror round-trips: recovery reconstructs the frozen before state
/// from the journaled row without ever re-reading the (drifted) tree.
#[derive(serde::Serialize, serde::Deserialize)]
struct OwnedManifest {
    files: Vec<OwnedFile>,
    logical_bytes: u64,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct OwnedFile {
    path: String,
    sha256: String,
    bytes: u64,
    modified_ms: i64,
    is_binary: bool,
}

fn manifest_to_owned(manifest: &ScanManifest) -> OwnedManifest {
    OwnedManifest {
        files: manifest
            .files
            .iter()
            .map(|file| OwnedFile {
                path: file.path.clone(),
                sha256: file.sha256.clone(),
                bytes: file.bytes,
                modified_ms: file.modified_ms,
                is_binary: file.is_binary,
            })
            .collect(),
        logical_bytes: manifest.logical_bytes,
    }
}

fn manifest_from_owned(owned: &OwnedManifest) -> ScanManifest {
    let mut files: Vec<FileIdentity> = owned
        .files
        .iter()
        .map(|file| FileIdentity {
            path: file.path.clone(),
            sha256: file.sha256.clone(),
            bytes: file.bytes,
            modified_ms: file.modified_ms,
            is_binary: file.is_binary,
        })
        .collect();
    files.sort_by(|left, right| left.path.cmp(&right.path));
    ScanManifest {
        files,
        logical_bytes: owned.logical_bytes,
    }
}

// ---------------------------------------------------------------------------
// P27 — the execute/recover/reconcile envelope for workspace-writing
// processes. Recovery NEVER re-executes the command: the frozen before
// state is the only input, the after scan measures once, and the delta
// digest is the receipt. The repo-exclusive workspace lock is held from
// begin to complete; a lost receipt replays (converges) on the same digest
// or conflicts — it never spawns again.
// ---------------------------------------------------------------------------

/// Why the envelope refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvelopeError {
    #[error("scan failure: {0}")]
    Scan(#[from] ScanError),
    #[error("store failure: {0}")]
    Store(#[from] r_code_store::v1::ProcessEffectError),
    #[error("quota failure: {0}")]
    Quota(#[from] crate::services::artifacts::EffectQuotaError),
    #[error("artifact failure: {0}")]
    Artifacts(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("io failure: {0}")]
    Io(String),
}

/// The live begin guard: the repo-exclusive lock, held until
/// [`complete_workspace_effect`] consumes it. Dropping it without
/// completing leaves the operation incomplete — the next boot's recovery
/// quarantines it (INV-08), never re-runs it.
pub struct WorkspaceEffectBegin {
    pub operation_id: String,
    pub owner_id: String,
    pub fencing_epoch: u64,
    /// True when the operation was already journaled by a previous begin
    /// with identical material: the caller must NOT spawn again (resume
    /// once); it may only complete.
    pub already_journaled: bool,
    #[allow(dead_code)]
    lock: crate::workspace_locks::WorkspaceLock,
}

/// The fixed dependencies one envelope call runs with.
pub struct EnvelopeContext<'a> {
    pub store: &'a V1Store,
    pub artifacts: &'a ArtifactStore,
    pub binding: &'a crate::services::workspaces::TaskWorkspaceBinding,
    pub quota: &'a crate::services::artifacts::EffectQuota,
    pub bounds: &'a ScanBounds,
}

/// The fenced identity and frozen command one operation runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectIdentity<'a> {
    pub operation_id: &'a str,
    pub owner_id: &'a str,
    pub fencing_epoch: u64,
    pub tree_id: &'a str,
    pub command_json: &'a str,
}

/// P27.1: acquire repo-exclusive ownership, persist the before state
/// (manifest + CAS forms + durable operation row + disk reservation) and
/// revalidate every identity immediately before the caller resumes — the
/// single resume happens only after this returns Ok.
pub fn begin_workspace_effect(
    context: &EnvelopeContext<'_>,
    identity: EffectIdentity<'_>,
) -> Result<WorkspaceEffectBegin, EnvelopeError> {
    let EnvelopeContext {
        store,
        artifacts,
        binding,
        quota,
        bounds,
    } = context;
    let EffectIdentity {
        operation_id,
        owner_id,
        fencing_epoch,
        tree_id,
        command_json,
    } = identity;
    let lock = binding
        .acquire_write_lock()
        .map_err(|error| EnvelopeError::Conflict(error.to_string()))?;
    let policy = binding.scan_policy();

    // A replayed begin with identical frozen material converges and must
    // not spawn again; anything else is a conflict — the command never
    // runs twice under one operation id.
    if let Some(existing) = store.load_process_effect(operation_id)? {
        let same = existing.tree_id == tree_id
            && existing.owner_id == owner_id
            && existing.fencing_epoch == fencing_epoch
            && existing.command_json == command_json;
        if !same {
            return Err(EnvelopeError::Conflict(format!(
                "operation {operation_id} already carries different material"
            )));
        }
        return Ok(WorkspaceEffectBegin {
            operation_id: operation_id.to_string(),
            owner_id: owner_id.to_string(),
            fencing_epoch,
            already_journaled: true,
            lock,
        });
    }

    let before = capture_manifest(&policy, bounds)?;
    let before_owned = manifest_to_owned(&before);
    let before_manifest_json = serde_json::to_string(&before_owned)
        .map_err(|error| EnvelopeError::Io(error.to_string()))?;
    let before_manifest_digest =
        crate::services::artifacts::sha256_hex(before_manifest_json.as_bytes());
    store.prepare_process_effect(r_code_store::v1::ProcessEffectPrepare {
        operation_id: operation_id.to_string(),
        tree_id: tree_id.to_string(),
        attempt_id: format!("attempt-{operation_id}"),
        workspace_key: format!("workspace:{operation_id}"),
        owner_id: owner_id.to_string(),
        fencing_epoch,
        command_json: command_json.to_string(),
        lease_json: format!(
            "{{\"repoExclusive\":true,\"workspace\":\"{}\"}}",
            binding.task_id
        ),
        before_manifest_json,
        before_manifest_digest,
        scan_policy_json: serde_json::to_string(&serde_json::json!({
            "root": binding.canonical_root.to_string_lossy(),
            "ignored": policy.ignored,
        }))
        .map_err(|error| EnvelopeError::Io(error.to_string()))?,
        ephemeral_roots_json: serde_json::to_string(&policy.ephemeral)
            .map_err(|error| EnvelopeError::Io(error.to_string()))?,
    })?;
    let operation = EffectArtifactPut {
        operation_id,
        owner_id,
        fencing_epoch,
        kind: "before-blob",
    };
    cas_manifest(store, artifacts, operation, &policy, &before)
        .map_err(|error| EnvelopeError::Artifacts(error.to_string()))?;
    quota.preflight_and_reserve(
        store,
        artifacts.root(),
        operation_id,
        before.logical_bytes.max(1),
    )?;
    // P26.3: every identity revalidated immediately before the release —
    // the single resume happens only over a verified tree.
    revalidate(&before, &policy)?;
    Ok(WorkspaceEffectBegin {
        operation_id: operation_id.to_string(),
        owner_id: owner_id.to_string(),
        fencing_epoch,
        already_journaled: false,
        lock,
    })
}

/// P27.2's measured outcome: the delta once, and its digest as the receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceEffectCompletion {
    pub delta: Vec<DeltaEntry>,
    pub delta_digest: String,
}

/// P27.2: prove-and-settle — scan once, measure the delta against the
/// FROZEN before state (reconstructed from the durable row, never from the
/// possibly-drifted tree), journal the measured delta as a CAS delta-blob
/// and receipt the operation with its digest (which releases the
/// reservation in the same transaction and makes the artifact refs
/// releasable). A replay converges on the same digest (lost receipt); a
/// different digest is a conflict — the command is never re-executed to
/// "fix" it.
pub fn complete_workspace_effect(
    context: &EnvelopeContext<'_>,
    begin: WorkspaceEffectBegin,
) -> Result<WorkspaceEffectCompletion, EnvelopeError> {
    let EnvelopeContext {
        store,
        artifacts,
        binding,
        quota: _,
        bounds,
    } = context;
    let record = store
        .load_process_effect(&begin.operation_id)?
        .ok_or_else(|| EnvelopeError::Conflict("operation vanished".into()))?;
    // The resume is recorded at settle time: a Prepared row means the
    // single spawn happened after begin's revalidation, so the tree proof
    // (the caller's settle) is what promotes it to Running before the
    // receipt. A Receipted replay needs no transition at all.
    if record.state == r_code_store::v1::ProcessEffectState::Prepared {
        store.mark_process_effect_running(
            &begin.operation_id,
            &begin.owner_id,
            begin.fencing_epoch,
        )?;
    }
    let owned: OwnedManifest = serde_json::from_str(&record.before_manifest_json)
        .map_err(|error| EnvelopeError::Io(error.to_string()))?;
    let before = manifest_from_owned(&owned);

    let policy = binding.scan_policy();
    let after = capture_manifest(&policy, bounds)?;
    let measured = delta(&before, &after);
    let delta_digest = delta_digest_of(&measured);

    let operation = EffectArtifactPut {
        operation_id: &begin.operation_id,
        owner_id: &begin.owner_id,
        fencing_epoch: begin.fencing_epoch,
        kind: "delta-blob",
    };
    // Every changed file's observed AFTER bytes are journaled as delta
    // blobs (the CAS rollback material), then the delta list itself.
    for entry in &measured {
        if entry.kind == DeltaKind::Deleted {
            continue;
        }
        let bytes = std::fs::read(policy.root.join(&entry.path))
            .map_err(|error| EnvelopeError::Io(error.to_string()))?;
        artifacts
            .put_effect_bytes(store, operation, &bytes, None)
            .map_err(|error| EnvelopeError::Artifacts(error.to_string()))?;
    }
    artifacts
        .put_effect_bytes(store, operation, delta_json_of(&measured).as_bytes(), None)
        .map_err(|error| EnvelopeError::Artifacts(error.to_string()))?;

    store.record_process_effect_receipt(
        &begin.operation_id,
        &begin.owner_id,
        begin.fencing_epoch,
        &delta_digest,
    )?;
    drop(begin); // releases the repo-exclusive lock with the receipt durable
    Ok(WorkspaceEffectCompletion {
        delta: measured,
        delta_digest,
    })
}

fn delta_json_of(entries: &[DeltaEntry]) -> String {
    serde_json::to_string(
        &entries
            .iter()
            .map(|entry| serde_json::json!({"path": entry.path, "kind": delta_kind_name(entry.kind)}))
            .collect::<Vec<_>>(),
    )
    .unwrap_or_default()
}

fn delta_digest_of(entries: &[DeltaEntry]) -> String {
    crate::services::artifacts::sha256_hex(delta_json_of(entries).as_bytes())
}

fn delta_kind_name(kind: DeltaKind) -> &'static str {
    match kind {
        DeltaKind::Created => "created",
        DeltaKind::Edited => "edited",
        DeltaKind::Deleted => "deleted",
        DeltaKind::Binary => "binary",
    }
}

/// P27.4: only the CurrentCheckoutWrite effect carries a checkout delta;
/// NoWorkspace and ScratchOnly explicitly skip it (their writes live
/// outside the checkout and are never reconciled against its tree).
pub fn checkout_delta_applies(
    effect: crate::services::process_profiles::ProcessProfileEffect,
) -> bool {
    matches!(
        effect,
        crate::services::process_profiles::ProcessProfileEffect::CurrentCheckoutWrite
    )
}

/// P27 startup recovery: every incomplete effect operation left by a crash
/// is quarantined — never re-executed. Called before any write ingress.
pub fn recover_incomplete_effects(store: &V1Store) -> Result<Vec<String>, EnvelopeError> {
    let mut quarantined = Vec::new();
    for record in store.incomplete_process_effects()? {
        store.quarantine_process_effect(
            &record.operation_id,
            &record.owner_id,
            record.fencing_epoch,
            "startup-recovery-incomplete",
        )?;
        quarantined.push(record.operation_id);
    }
    Ok(quarantined)
}
