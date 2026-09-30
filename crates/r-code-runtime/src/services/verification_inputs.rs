//! Frozen verifier and dependency input materialization.
//!
//! Candidate content (captured as a manifest over the live workspace) and
//! pinned acceptance-control files are materialized into a *private*
//! verification directory: candidate dependency manifests/lockfiles are
//! preserved from the candidate, while frozen control files (verifier
//! scripts, entrypoint-adjacent config) are written from host-pinned bytes
//! outside plugin write scope. Preparation runs Cargo `--locked` / npm `ci
//! --ignore-scripts` on the gated preparation route against a content-addressed
//! read-only lower cache plus a run-owned overlay, and publishes into the cache
//! only after validating what the run produced; undeclared hooks or inputs
//! yield `unavailable`, never a silent pass.

use crate::services::artifacts::sha256_hex;
use crate::services::authorization::{
    EffectivePermissions, OperationDescriptor, PrepNetworkAuthority, WorkspaceCapability,
};
use crate::services::execution::{
    DependencyPrepKind, DependencyPrepSpec, ExecutionService, PrepCachePaths,
};
use crate::services::workspaces::{CandidateManifest, TaskWorkspaceBinding};
use r_code_gateway::execution_backend::CollectedOutput;
use r_code_harness_protocol::services::{NetworkCeiling, PermissionCeiling};
use r_code_kernel::verification::{CheckDefinition, ControlFile};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Errors from materialization.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MaterializeError {
    #[error("io failure: {0}")]
    Io(String),
    #[error("the live workspace changed since capture: {0}")]
    StaleCapture(String),
    #[error("control file {0} is missing from the frozen store")]
    MissingControlFile(String),
    #[error("required input {0} was not declared and is unavailable")]
    UndeclaredInput(String),
    #[error("dependency preparation failed: {0}")]
    PreparationFailed(String),
    #[error("dependency cache root {0} is not absolute")]
    CacheRootNotAbsolute(String),
    #[error("dependency cache identity {0} is not a sha256 hex digest")]
    CacheIdentity(String),
    #[error("the dependency cache slot for {0} is poisoned: {1}")]
    CachePoisoned(String, String),
    #[error("another dependency preparation holds the cache slot for {0}")]
    CacheBusy(String),
    #[error("dependency cache promotion failed: {0}")]
    PromotionFailed(String),
}

/// Where frozen control bytes live (host-owned, outside plugin scope).
pub struct FrozenControlStore {
    root: PathBuf,
}

impl FrozenControlStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Store control bytes once (host-side ingestion).
    pub fn store(&self, control: &ControlFile, bytes: &[u8]) -> Result<(), MaterializeError> {
        let digest = sha256_hex(bytes);
        if digest != control.sha256 {
            return Err(MaterializeError::MissingControlFile(format!(
                "{} (digest mismatch)",
                control.path
            )));
        }
        let path = self.root.join(&control.sha256);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| MaterializeError::Io(e.to_string()))?;
        }
        std::fs::write(path, bytes).map_err(|e| MaterializeError::Io(e.to_string()))?;
        Ok(())
    }

    fn load(&self, control: &ControlFile) -> Result<Vec<u8>, MaterializeError> {
        let bytes = std::fs::read(self.root.join(&control.sha256))
            .map_err(|_| MaterializeError::MissingControlFile(control.path.clone()))?;
        if sha256_hex(&bytes) != control.sha256 {
            return Err(MaterializeError::MissingControlFile(control.path.clone()));
        }
        Ok(bytes)
    }
}

/// A private verification directory materialized for one candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedVerificationDir {
    pub dir: PathBuf,
    /// Candidate dependency lockfiles preserved verbatim.
    pub preserved_lockfiles: Vec<String>,
    /// The declared toolchain identity, frozen with the tree.
    pub toolchain: String,
    /// Cache identity for checksummed dependency downloads only.
    pub cache_identity: String,
    /// The run-owned overlay a preparation writes into, always inside `dir`,
    /// so its bytes can never be mounted by a Check (P21.1, acceptance 3).
    pub overlay: PathBuf,
    /// The content-addressed slot validated bytes are promoted into: outside
    /// the overlay, so a Check may mount it read-only while the overlay dies
    /// with the run (P21.3).
    pub promoted_cache: PathBuf,
}

impl PreparedVerificationDir {
    /// The slot this tree's identity owns under a caller-supplied cache root.
    /// A caller that prepares against a root other than
    /// [`default_cache_root`] mounts this, not [`Self::promoted_cache`], which
    /// is only the tree's own sibling default.
    pub fn promoted_cache_in(&self, cache_root: &Path) -> Result<PathBuf, MaterializeError> {
        promoted_path_for(cache_root, &self.cache_identity)
    }
}

/// Materialize candidate + control files into `dir`.
///
/// `candidate_root` supplies the captured bytes; the live workspace is
/// re-verified against the manifest first — concurrent edits refuse
/// materialization instead of mixing states.
pub fn materialize(
    binding: &TaskWorkspaceBinding,
    manifest: &CandidateManifest,
    control_store: &FrozenControlStore,
    definition: &CheckDefinition,
    dir: &Path,
) -> Result<PreparedVerificationDir, MaterializeError> {
    // 1. Capture/live consistency.
    manifest
        .verify_live(binding)
        .map_err(|error| MaterializeError::StaleCapture(error.to_string()))?;

    std::fs::create_dir_all(dir).map_err(|e| MaterializeError::Io(e.to_string()))?;

    // 2. Candidate bytes: from the (verified-consistent) live root.
    for relative in manifest.files.keys() {
        let source = binding.canonical_root.join(relative);
        let target = dir.join(relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| MaterializeError::Io(e.to_string()))?;
        }
        std::fs::copy(&source, &target).map_err(|e| MaterializeError::Io(e.to_string()))?;
    }

    // 3. Frozen control files override candidate-redefinable material.
    let mut control_digests = BTreeMap::new();
    for control in &definition.control_files {
        let bytes = control_store.load(control)?;
        let target = dir.join(&control.path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|e| MaterializeError::Io(e.to_string()))?;
        }
        std::fs::write(&target, &bytes).map_err(|e| MaterializeError::Io(e.to_string()))?;
        control_digests.insert(control.path.clone(), control.sha256.clone());
    }

    // 4. Declared external inputs must exist; undeclared ones can't be
    //    proven, so the check is unavailable (callers translate).
    for declared in &definition.declared_external_inputs {
        let path = binding.canonical_root.join(declared);
        if !path.exists() {
            return Err(MaterializeError::UndeclaredInput(declared.clone()));
        }
    }

    // 5. Cache identity: lockfiles + toolchain (dependency downloads only).
    let mut material = definition.toolchain.clone();
    for lock in &definition.dependency_locks {
        material.push('\u{1}');
        material.push_str(lock);
        material.push('\u{1}');
        material.push_str(
            manifest
                .files
                .get(lock)
                .map(String::as_str)
                .unwrap_or("<missing>"),
        );
    }
    let preserved_lockfiles = definition.dependency_locks.clone();
    let cache_identity = sha256_hex(material.as_bytes());
    Ok(PreparedVerificationDir {
        dir: dir.to_path_buf(),
        preserved_lockfiles,
        toolchain: definition.toolchain.clone(),
        // Composed rather than validated here: the identity is a fresh sha256
        // hex, and every path the preparation reads is re-validated when the
        // spec is built, so a private tree under a hostile location refuses
        // there instead of silently narrowing what a run may touch.
        overlay: overlay_path(dir),
        promoted_cache: promoted_path_under(&default_cache_root(dir), &cache_identity),
        cache_identity,
    })
}

/// The run-owned overlay inside a private verification tree.
pub fn overlay_path(dir: &Path) -> PathBuf {
    dir.join(".prep-overlay")
}

/// The cache root materialization assumes when the caller names none: a
/// sibling of the private tree, never inside it, so promoting bytes cannot
/// publish anything a Check reads as candidate content.
pub fn default_cache_root(dir: &Path) -> PathBuf {
    dir.parent()
        .map(|parent| parent.join("dependency-cache"))
        .unwrap_or_else(|| std::env::temp_dir().join("dependency-cache"))
}

/// The content-addressed slot for an identity already known to be a digest.
fn promoted_path_under(cache_root: &Path, cache_identity: &str) -> PathBuf {
    cache_root.join("promoted").join(cache_identity)
}

/// The validated slot path for one identity, under a caller-supplied cache
/// root. A poisoned or garbage identity must never become a path, so anything
/// that is not a sha256 hex digest is refused here rather than joined.
pub fn promoted_path_for(
    cache_root: &Path,
    cache_identity: &str,
) -> Result<PathBuf, MaterializeError> {
    validate_identity(cache_identity)?;
    Ok(promoted_path_under(cache_root, cache_identity))
}

/// The read-only lower cache for one identity: its own promoted slot once a
/// validated run filled it, otherwise an empty read-only layer. A cold identity
/// having no lower layer is exactly why preparation has to run at all.
pub fn lower_cache_root(
    cache_root: &Path,
    cache_identity: &str,
) -> Result<PathBuf, MaterializeError> {
    validate_identity(cache_identity)?;
    let slot = promoted_path_under(cache_root, cache_identity);
    if slot.is_dir() {
        return Ok(slot);
    }
    Ok(cache_root.join("lower").join(cache_identity))
}

fn validate_identity(cache_identity: &str) -> Result<(), MaterializeError> {
    if cache_identity.len() != 64 || !cache_identity.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(MaterializeError::CacheIdentity(cache_identity.to_string()));
    }
    Ok(())
}

/// Dependency kind for preparation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyKind {
    Cargo,
    Npm,
}

impl DependencyKind {
    /// The modelled preparation kind: the mandatory-argument rule lives in the
    /// spec, so it needs a kind and not a command string (P21.2).
    pub fn prep_kind(&self) -> DependencyPrepKind {
        match self {
            Self::Cargo => DependencyPrepKind::Cargo,
            Self::Npm => DependencyPrepKind::Npm,
        }
    }

    /// The lockfile a reproducible preparation of this kind must preserve.
    pub fn lockfile(&self) -> &'static str {
        match self {
            Self::Cargo => "Cargo.lock",
            Self::Npm => "package-lock.json",
        }
    }
}

/// The content-addressed dependency cache: slot, lower layer, staging area and
/// host lock, all derived from one validated identity under an absolute root.
#[derive(Debug, Clone)]
pub struct DependencyCache {
    root: PathBuf,
}

/// The paths one identity owns inside a cache.
struct CacheSlotPaths {
    slot: PathBuf,
    staging: PathBuf,
    lock: PathBuf,
}

/// The record that a promoted slot was validated: written before publication
/// and re-checked on every warm read, so bytes nobody validated can never be
/// reused just because a directory with the right name exists.
const VALID_MARKER: &str = ".preparation-valid";
/// The record that a promotion failed, carried inside the staging area.
const POISON_MARKER: &str = ".preparation-poisoned";

impl DependencyCache {
    /// A cache over an explicit absolute root.
    pub fn new(root: &Path) -> Result<Self, MaterializeError> {
        if !root.is_absolute() {
            return Err(MaterializeError::CacheRootNotAbsolute(
                root.to_string_lossy().to_string(),
            ));
        }
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    /// The cache a materialized private tree promotes into by default.
    pub fn for_directory(dir: &Path) -> Self {
        Self {
            root: default_cache_root(dir),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The promoted slot an identity owns.
    pub fn promoted(&self, cache_identity: &str) -> Result<PathBuf, MaterializeError> {
        promoted_path_for(&self.root, cache_identity)
    }

    /// The host-side staging area an overlay is validated in before promotion.
    pub fn staging(&self, cache_identity: &str) -> Result<PathBuf, MaterializeError> {
        Ok(self.paths(cache_identity)?.staging)
    }

    /// The read-only lower layer a preparation run mounts.
    pub fn lower(&self, cache_identity: &str) -> Result<PathBuf, MaterializeError> {
        lower_cache_root(&self.root, cache_identity)
    }

    fn paths(&self, cache_identity: &str) -> Result<CacheSlotPaths, MaterializeError> {
        validate_identity(cache_identity)?;
        Ok(CacheSlotPaths {
            slot: promoted_path_under(&self.root, cache_identity),
            staging: self.root.join("staging").join(cache_identity),
            lock: self
                .root
                .join("locks")
                .join(format!("{cache_identity}.lock")),
        })
    }

    /// Cold, warm or poisoned, decided from bytes on disk and nothing else.
    pub fn state(&self, cache_identity: &str) -> CacheSlotState {
        let Ok(paths) = self.paths(cache_identity) else {
            return CacheSlotState::Poisoned {
                reason: format!("{cache_identity} is not a sha256 hex digest"),
            };
        };
        if paths.staging.exists() {
            // A failed validation, or a promotion interrupted between taking
            // the overlay out of the private tree and publishing the slot:
            // those bytes were never validated, so the identity is refused.
            return CacheSlotState::Poisoned {
                reason: read_poison_reason(&paths.staging),
            };
        }
        if !paths.slot.is_dir() {
            return CacheSlotState::Cold;
        }
        match read_marker(&paths.slot.join(VALID_MARKER)) {
            Some((recorded, digest)) if recorded == cache_identity => {
                match validate_staging(&paths.slot, cache_identity) {
                    Ok(current) if current == digest => CacheSlotState::Warm,
                    Ok(_) => CacheSlotState::Poisoned {
                        reason: "the promoted tree no longer matches its digest".to_string(),
                    },
                    Err(reason) => CacheSlotState::Poisoned { reason },
                }
            }
            Some(_) => CacheSlotState::Poisoned {
                reason: "the promoted slot records another cache identity".to_string(),
            },
            None => CacheSlotState::Poisoned {
                reason: "the promoted slot carries no validation marker".to_string(),
            },
        }
    }

    /// Take the host lock over one slot with `create_new`, so two preparations
    /// for the same identity refuse instead of interleaving writes. A lock left
    /// by a killed preparation is not stolen: the identity stays refused until
    /// it is removed, which is fail-closed rather than convenient.
    pub fn try_lock(&self, cache_identity: &str) -> Result<CacheLock, MaterializeError> {
        let paths = self.paths(cache_identity)?;
        if let Some(parent) = paths.lock.parent() {
            std::fs::create_dir_all(parent).map_err(|e| MaterializeError::Io(e.to_string()))?;
        }
        std::fs::File::options()
            .create_new(true)
            .write(true)
            .open(&paths.lock)
            .map_err(|_| MaterializeError::CacheBusy(cache_identity.to_string()))?;
        Ok(CacheLock { path: paths.lock })
    }
}

/// Cold / warm / poisoned, as observable on disk (P21 test surface).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheSlotState {
    /// Nothing promoted: preparation must run.
    Cold,
    /// A validated slot whose recorded digest still describes its bytes.
    Warm,
    /// A failed, interrupted or mutated promotion: refused until cleaned.
    Poisoned { reason: String },
}

/// Where one preparation call left the slot. Poison is reported as
/// [`MaterializeError::CachePoisoned`], so a caller can never read a poisoned
/// identity as a prepared tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreparationOutcome {
    /// Prepared and promoted by this call.
    Cold {
        slot: PathBuf,
        lower_cache_readonly: PathBuf,
    },
    /// A validated slot already existed: reused with zero spawning.
    Warm {
        slot: PathBuf,
        lower_cache_readonly: PathBuf,
    },
}

/// The host lock guard over one slot; released when the holder drops it.
#[derive(Debug)]
pub struct CacheLock {
    path: PathBuf,
}

impl Drop for CacheLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// What one preparation run may do. Nothing here implies network access: the
/// ceiling and the proof that covers it travel together, an empty allowlist
/// means a cleared environment that installs nothing, and the arguments are
/// carried exactly as declared so no caller can downgrade the run.
#[derive(Debug, Clone)]
pub struct PreparationRequest {
    /// The verbatim arguments, which the spec refuses unless they carry this
    /// kind's mandatory pair (P21.2).
    pub arguments: Vec<String>,
    pub network: NetworkCeiling,
    pub authority: PrepNetworkAuthority,
    pub environment_keys: Vec<String>,
    pub timeout: Duration,
}

impl PreparationRequest {
    /// An offline preparation with no proven network authority: the only shape
    /// usable without an effect approval.
    pub fn offline(
        arguments: Vec<String>,
        environment_keys: Vec<String>,
        timeout: Duration,
    ) -> Self {
        Self {
            arguments,
            network: NetworkCeiling::Offline,
            authority: PrepNetworkAuthority::Offline,
            environment_keys,
            timeout,
        }
    }

    /// The arguments a preparation of this kind must carry: the mandatory pair
    /// itself, so a caller cannot build a request that the spec will refuse.
    pub fn required_arguments(kind: DependencyKind) -> Vec<String> {
        kind.prep_kind()
            .required_arguments()
            .iter()
            .map(|argument| (*argument).to_string())
            .collect()
    }
}

/// Prepare dependencies inside a materialized directory through the injected
/// execution service, against a content-addressed read-only lower cache and a
/// run-owned overlay, and promote into the cache only after validating what
/// the run produced.
///
/// Warm identities are reused without spawning; poisoned ones are refused
/// instead of reused; and the preparation itself runs only through the gated
/// prep route, so it never reaches the local shell. The slot the bytes were
/// promoted into comes back in [`PreparationOutcome`]: it is the path a Check
/// mounts read-only, and the overlay it came from no longer exists.
pub async fn prepare_dependencies(
    execution: &ExecutionService,
    prepared: &PreparedVerificationDir,
    kind: DependencyKind,
    cache: &DependencyCache,
    request: &PreparationRequest,
) -> Result<PreparationOutcome, MaterializeError> {
    let lockfile = kind.lockfile();
    if !prepared
        .preserved_lockfiles
        .iter()
        .any(|lock| lock == lockfile)
    {
        return Err(MaterializeError::UndeclaredInput(format!(
            "{lockfile} missing; cannot prepare reproducibly"
        )));
    }
    let identity = prepared.cache_identity.as_str();
    let slot = cache.promoted(identity)?;
    let lower = cache.lower(identity)?;
    // A validated slot answers with no spawn at all: warm identities never
    // touch the network, the gate or a lock.
    if cache.state(identity) == CacheSlotState::Warm {
        return Ok(PreparationOutcome::Warm {
            slot,
            lower_cache_readonly: lower,
        });
    }
    let _lock = cache.try_lock(identity)?;
    // Double-checked under the lock: a peer that held it may have promoted or
    // poisoned this identity while the call waited, and its answer wins.
    match cache.state(identity) {
        CacheSlotState::Warm => {
            return Ok(PreparationOutcome::Warm {
                slot,
                lower_cache_readonly: lower,
            })
        }
        CacheSlotState::Poisoned { reason } => {
            return Err(MaterializeError::CachePoisoned(
                identity.to_string(),
                reason,
            ))
        }
        CacheSlotState::Cold => {}
    }
    let paths = PrepCachePaths {
        lower_cache_readonly: lower.clone(),
        overlay: prepared.overlay.clone(),
        promoted_cache: slot.clone(),
    };
    let output = run_preparation(execution, prepared, kind, &paths, request).await?;
    if output.exit_code != Some(0) {
        let reason = format!(
            "preparation exited with {:?}: {}",
            output.exit_code, output.stderr
        );
        return Err(poison(cache, identity, &prepared.overlay, reason));
    }
    // P21.3: the run-owned overlay is renamed into host staging, validated,
    // and published by a single rename. An interrupted promotion is therefore
    // observable as staging or as nothing, never as a complete cache.
    let staging = cache.staging(identity)?;
    if let Some(parent) = staging.parent() {
        std::fs::create_dir_all(parent).map_err(|e| MaterializeError::Io(e.to_string()))?;
    }
    std::fs::rename(&prepared.overlay, &staging)
        .map_err(|e| MaterializeError::PromotionFailed(e.to_string()))?;
    let digest = match validate_staging(&staging, identity) {
        Ok(digest) => digest,
        Err(reason) => return Err(poison(cache, identity, &prepared.overlay, reason)),
    };
    std::fs::write(
        staging.join(VALID_MARKER),
        format!("{identity}\n{digest}\n"),
    )
    .map_err(|e| MaterializeError::Io(e.to_string()))?;
    // The slot's parent is host-owned and may not exist until the very first
    // publication: without it the rename fails and a validated tree is lost
    // (and then poisoned) for no reason of its own.
    if let Some(parent) = slot.parent() {
        std::fs::create_dir_all(parent).map_err(|e| MaterializeError::Io(e.to_string()))?;
    }
    std::fs::rename(&staging, &slot)
        .map_err(|e| MaterializeError::PromotionFailed(e.to_string()))?;
    Ok(PreparationOutcome::Cold {
        slot,
        lower_cache_readonly: lower,
    })
}

/// Freeze and run one preparation on the gated route. Authorization, the
/// backend and the cache layout all read this one spec, so nothing re-parses a
/// command line, and the network ask is only ever expressed as the authority
/// the runtime resolved for it.
async fn run_preparation(
    execution: &ExecutionService,
    prepared: &PreparedVerificationDir,
    kind: DependencyKind,
    paths: &PrepCachePaths,
    request: &PreparationRequest,
) -> Result<CollectedOutput, MaterializeError> {
    let spec = DependencyPrepSpec::build(
        kind.prep_kind(),
        &request.arguments,
        &prepared.dir,
        &prepared.toolchain,
        request.network,
        paths,
        &request.environment_keys,
    )
    .map_err(|error| MaterializeError::PreparationFailed(error.to_string()))?;
    std::fs::create_dir_all(&paths.overlay).map_err(|e| MaterializeError::Io(e.to_string()))?;
    std::fs::create_dir_all(&paths.lower_cache_readonly)
        .map_err(|e| MaterializeError::Io(e.to_string()))?;
    let descriptor = OperationDescriptor::dependency_preparation(
        kind.prep_kind().executable(),
        spec.arguments.clone(),
        Some(prepared.dir.to_string_lossy().to_string()),
        request.authority,
    );
    let workspace = WorkspaceCapability::WriteWithin {
        root: prepared.dir.to_string_lossy().replace('\\', "/"),
    };
    // The permissions follow the requested ceiling exactly: task authority
    // never leaks network into a fetch nobody approved (acceptance 2).
    let permissions = EffectivePermissions {
        ceiling: PermissionCeiling::Full,
        allow_processes: true,
        allow_network: !request.network.is_offline(),
    };
    execution
        .run_prep(
            &spec,
            &descriptor,
            &workspace,
            &permissions,
            request.timeout,
        )
        .await
        .map_err(|error| MaterializeError::PreparationFailed(error.to_string()))
}

/// Record a failure against a slot so the next attempt refuses it rather than
/// reusing bytes nobody validated. The marker goes into the staging area and
/// the failed bytes stay there: nothing about a poisoned identity is cleaned up
/// behind the caller's back.
fn poison(
    cache: &DependencyCache,
    cache_identity: &str,
    overlay: &Path,
    reason: String,
) -> MaterializeError {
    if let Ok(staging) = cache.staging(cache_identity) {
        if !staging.exists() {
            // A poison usually arrives before anything has created the staging
            // parent, and `rename` does not create it: without this the error
            // was swallowed, the marker had nowhere to go, and a failed
            // identity read back as Cold so the next attempt reused nothing
            // and reported nothing.
            if let Some(parent) = staging.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if overlay.exists() {
                let _ = std::fs::rename(overlay, &staging);
            }
        }
        if !staging.exists() {
            let _ = std::fs::create_dir_all(&staging);
        }
        if staging.is_dir() {
            let _ = std::fs::write(staging.join(POISON_MARKER), reason.as_bytes());
        }
    }
    MaterializeError::CachePoisoned(cache_identity.to_string(), reason)
}

/// Validate a staged tree before it can become a cache: it must exist, carry
/// bytes, keep `node_modules` and the git directory inside the private tree,
/// and digest to the identity it was staged for. The digest is what the
/// published slot records, so a warm read can prove the bytes are unchanged.
fn validate_staging(staging: &Path, cache_identity: &str) -> Result<String, String> {
    if !staging.is_dir() {
        return Err("the preparation left no staging tree to promote".to_string());
    }
    let mut files = Vec::new();
    collect_staged(staging, staging, &mut files)?;
    if files.is_empty() {
        return Err(
            "the preparation produced no bytes; promoting it would let a later run skip \
             dependencies"
                .to_string(),
        );
    }
    for (relative, _) in &files {
        if relative.split('/').any(|component| {
            component.eq_ignore_ascii_case("node_modules") || component.eq_ignore_ascii_case(".git")
        }) {
            return Err(format!(
                "staged tree carries {relative}, which must stay inside the private tree"
            ));
        }
    }
    Ok(r_code_harness_protocol::canonical_input_hash(
        &serde_json::json!({ "identity": cache_identity, "files": files }),
    ))
}

fn collect_staged(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.is_dir() {
            collect_staged(root, &path, out)?;
            continue;
        }
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default();
        if name == VALID_MARKER || name == POISON_MARKER {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
        out.push((relative, sha256_hex(&bytes)));
    }
    out.sort();
    Ok(())
}

fn read_marker(path: &Path) -> Option<(String, String)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    let identity = lines.next()?.to_string();
    let digest = lines.next()?.to_string();
    Some((identity, digest))
}

fn read_poison_reason(staging: &Path) -> String {
    std::fs::read_to_string(staging.join(POISON_MARKER)).unwrap_or_else(|_| {
        "an interrupted promotion left staged bytes that were never validated".to_string()
    })
}

/// P27: the scan policy for any checkout-writing preparation. The
/// materialization directory is the scope; the private overlay tree is
/// ephemeral by construction (recreated per run) and never reconciled
/// against the checkout.
pub fn checkout_write_policy(
    dir: &std::path::Path,
) -> crate::services::process_effects::ScanPolicy {
    crate::services::process_effects::ScanPolicy {
        root: dir.to_path_buf(),
        ignored: Vec::new(),
        ephemeral: vec!["overlay".to_string()],
    }
}
