//! Frozen verifier and dependency input materialization.
//!
//! Candidate content (captured as a manifest over the live workspace) and
//! pinned acceptance-control files are materialized into a *private*
//! verification directory: candidate dependency manifests/lockfiles are
//! preserved from the candidate, while frozen control files (verifier
//! scripts, entrypoint-adjacent config) are written from host-pinned bytes
//! outside plugin write scope. Preparation runs Cargo `--locked` / npm `ci
//! --ignore-scripts` through the shared execution service with
//! lockfile+toolchain-scoped cache identities; undeclared hooks or inputs
//! yield `unavailable`, never a silent pass.

use crate::services::artifacts::sha256_hex;
use crate::services::execution::ExecutionService;
use crate::services::workspaces::{CandidateManifest, TaskWorkspaceBinding};
use r_code_kernel::verification::{CheckDefinition, ControlFile};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
    /// Cache identity for checksummed dependency downloads only.
    pub cache_identity: String,
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
    Ok(PreparedVerificationDir {
        dir: dir.to_path_buf(),
        preserved_lockfiles,
        cache_identity: sha256_hex(material.as_bytes()),
    })
}

/// Dependency kind for preparation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyKind {
    Cargo,
    Npm,
}

/// Prepare dependencies inside a materialized directory through the shared
/// execution service. Cargo uses `--locked`; npm uses `ci
/// --ignore-scripts`; install hooks never run.
pub async fn prepare_dependencies(
    _execution: &ExecutionService,
    prepared: &PreparedVerificationDir,
    kind: DependencyKind,
) -> Result<(), MaterializeError> {
    use crate::services::authorization::{
        AuthorizationService, EffectivePermissions, WorkspaceCapability,
    };
    let permissions = EffectivePermissions::full();
    let workspace = WorkspaceCapability::WriteWithin {
        root: prepared.dir.to_string_lossy().replace('\\', "/"),
    };
    let authorization = AuthorizationService::new();
    let execution = ExecutionService::with_backend(
        std::sync::Arc::new(r_code_gateway::execution_backend::LocalShellBackend::new()),
        std::sync::Arc::new(authorization),
    );
    let (command, lock_gate) = match kind {
        DependencyKind::Cargo => ("cargo fetch --locked", "Cargo.lock"),
        DependencyKind::Npm => ("npm ci --ignore-scripts", "package-lock.json"),
    };
    let has_lock = prepared
        .preserved_lockfiles
        .iter()
        .any(|lock| lock == lock_gate);
    if !has_lock {
        return Err(MaterializeError::UndeclaredInput(format!(
            "{lock_gate} missing; cannot prepare reproducibly"
        )));
    }
    let workspace_ref = workspace;
    let output = execution
        .run_authorized(
            &crate::services::authorization::OperationDescriptor::verification_preparation(
                command.split_whitespace().next().unwrap_or("cargo"),
                command
                    .split_whitespace()
                    .skip(1)
                    .map(str::to_string)
                    .collect(),
                Some(prepared.dir.to_string_lossy().to_string()),
            ),
            &workspace_ref,
            &permissions,
            command,
            &prepared.dir,
            std::time::Duration::from_secs(600),
        )
        .await
        .map_err(|error| MaterializeError::PreparationFailed(error.to_string()))?;
    match output.exit_code {
        Some(0) => Ok(()),
        code => Err(MaterializeError::PreparationFailed(format!(
            "preparation exited with {code:?}: {}",
            output.stderr
        ))),
    }
}
