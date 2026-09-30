//! Unified command execution for tools and verification.
//!
//! One service routes every shell-shaped effect: common authorization first
//! (T12a), then spawn/collect on a selectable [`CommandExecutionBackend`].
//! The default backend is the existing five-tier shell resolution chain
//! (RTK-aware, Windows-dialect preserving); process trees are killed on
//! timeout/abort by the backend's kill-on-drop + kill-tree semantics.

use crate::services::authorization::{
    AuthorizationDecision, AuthorizationService, EffectivePermissions, OperationDescriptor,
    WorkspaceCapability,
};
use r_code_core::error::ProductError;
use r_code_gateway::execution_backend::{
    CollectedOutput, CommandExecutionBackend, CommandHandle, CommandSpec, LocalShellBackend,
};
use r_code_harness_protocol::services::NetworkCeiling;
use r_code_kernel::ports::ServiceError;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Errors from unified execution.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExecutionError {
    #[error("authorization denied: {0}")]
    Denied(String),
    #[error("approval required: {0}")]
    ApprovalRequired(String),
    #[error("backend failure: {0}")]
    Backend(String),
    #[error("check spawn spec refused: {0}")]
    Spec(String),
}

/// Command bytes that would let one element reinterpret the command once it
/// reaches a shell-shaped route. A check material carrying any of them is
/// refused before a backend sees it (P20.1).
const SHELL_REINTERPRETABLE: [char; 9] = [';', '&', '|', '\n', '\r', '<', '>', '`', '$'];

/// The exact, complete spawn material of one required check (P20.1).
///
/// A check never runs from a free-form command line: the host freezes the
/// executable, the verbatim entrypoint arguments, the private verify
/// directory as cwd, the declared toolchain identity, and the absolute read
/// roots the sandbox enforces. Offline is the only ceiling a check can
/// express, so it is a validated field rather than a caller-supplied option.
/// Construction is the only way to hold one — an invalid material cannot exist,
/// and its digest is what the minted evidence binds, so a changed executable,
/// argument, directory, toolchain or read root stales prior evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckSpawnSpec {
    pub executable: PathBuf,
    /// The frozen entrypoint arguments, exactly as declared, without the
    /// executable (mirrors the backend's program/arguments split).
    pub arguments: Vec<String>,
    pub cwd: PathBuf,
    pub toolchain: String,
    pub read_roots: Vec<String>,
    pub network: NetworkCeiling,
}

/// Why a check material is not spawnable. Every variant is a refusal, never a
/// default: the runner surfaces these as `Unavailable` with zero spawn.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CheckSpecError {
    #[error("check executable {0} is not a bare toolchain program")]
    Executable(String),
    #[error("check material contains {0:?}, which a shell could reinterpret")]
    Reinterpretable(String),
    #[error("check working directory {0} is not absolute")]
    CwdNotAbsolute(String),
    #[error("check declares no toolchain identity")]
    EmptyToolchain,
    #[error("check read root {0} escapes the materialized tree")]
    ReadRootEscape(String),
    #[error("check read root {0} would make the git directory visible")]
    ReadRootVisibleGit(String),
    #[error("check network ceiling must be offline")]
    NetworkNotOffline,
    #[error("dependency preparation for {0} must carry the argument {1}")]
    PrepMissingArgument(String, String),
    #[error("dependency preparation path {0} is not absolute")]
    PrepPathNotAbsolute(String),
    #[error("dependency preparation overlay {0} is not inside the working directory {1}")]
    PrepOverlayOutsideCwd(String, String),
    #[error("promoted cache {0} must live outside the run overlay {1}")]
    PrepPromotedInsideOverlay(String, String),
    #[error("promoted cache {0} would put node_modules outside the private tree")]
    PrepPromotedNodeModules(String),
    #[error("dependency preparation network ceiling host-network is unsupported in v1")]
    PrepNetworkUnsupported,
    #[error("dependency preparation path {0} would make the git directory visible")]
    PrepVisibleGit(String),
    #[error("dependency preparation environment key {0} addresses a provider credential")]
    PrepCredentialEnvironmentKey(String),
    #[error("dependency preparation program {0} is not the declared kind's toolchain")]
    PrepProgramMismatch(String),
}

impl CheckSpawnSpec {
    /// Freeze one check's material or refuse it. The executable must be a bare
    /// toolchain program (never a path the candidate controls), every argument
    /// must be verbatim and free of reinterpretation, cwd must be absolute, and
    /// each declared source root must stay inside the private tree with the
    /// git directory invisible (INV-06).
    pub fn build(
        program: &str,
        arguments: &[String],
        cwd: &Path,
        toolchain: &str,
        source_roots: &[String],
    ) -> Result<Self, CheckSpecError> {
        // A bare program name carries no path separator on any platform. Using
        // the host's own separator rules instead would make "bare" mean one
        // thing on Windows and another on Unix, letting the same definition be
        // a filename here and a path there.
        if program.is_empty() || program.contains(['/', '\\', '\0']) {
            return Err(CheckSpecError::Executable(program.to_string()));
        }
        reject_reinterpretable(program)?;
        for argument in arguments {
            reject_reinterpretable(argument)?;
        }
        if !cwd.is_absolute() {
            return Err(CheckSpecError::CwdNotAbsolute(
                cwd.to_string_lossy().to_string(),
            ));
        }
        if toolchain.trim().is_empty() {
            return Err(CheckSpecError::EmptyToolchain);
        }
        // The materialized tree is always readable; declared roots may only
        // narrow what inside it the check expects to see.
        let mut read_roots = vec![canonical(cwd)];
        for root in source_roots {
            reject_reinterpretable(root)?;
            let path = Path::new(root);
            if root.starts_with('/') || path.is_absolute() || path.components().count() == 0 {
                return Err(CheckSpecError::ReadRootEscape(root.clone()));
            }
            if path
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
            {
                return Err(CheckSpecError::ReadRootEscape(root.clone()));
            }
            let joined = canonical(&cwd.join(root));
            if visible_git(&joined) {
                return Err(CheckSpecError::ReadRootVisibleGit(joined));
            }
            read_roots.push(joined);
        }
        if read_roots.iter().any(|root| visible_git(root)) {
            return Err(CheckSpecError::ReadRootVisibleGit(canonical(cwd)));
        }
        Ok(Self {
            executable: PathBuf::from(program),
            arguments: arguments.to_vec(),
            cwd: cwd.to_path_buf(),
            toolchain: toolchain.to_string(),
            read_roots,
            network: NetworkCeiling::Offline,
        })
    }

    /// P21.3: mount one promoted dependency cache read-only for this check.
    ///
    /// The slot must be absolute, must live outside the run-owned overlay it
    /// was promoted from, and must not sit inside the check's own private tree:
    /// bytes still reachable from the overlay were never published, and an
    /// overlay dies with the run that owns it (acceptance ③). The root joins the
    /// check's read roots, so it lands inside `digest()` and a different
    /// promoted identity stales the evidence the check minted.
    pub fn with_promoted_cache(
        mut self,
        slot: &Path,
        overlay: &Path,
    ) -> Result<Self, CheckSpecError> {
        let promoted = canonical(slot);
        if !slot.is_absolute() {
            return Err(CheckSpecError::PrepPathNotAbsolute(promoted));
        }
        if visible_git(&promoted) {
            return Err(CheckSpecError::ReadRootVisibleGit(promoted));
        }
        let overlay = canonical(overlay);
        if promoted.starts_with(&overlay) || overlay.starts_with(&promoted) {
            return Err(CheckSpecError::PrepPromotedInsideOverlay(promoted, overlay));
        }
        if promoted.starts_with(&canonical(&self.cwd)) {
            return Err(CheckSpecError::PrepPromotedInsideOverlay(
                promoted,
                canonical(&self.cwd),
            ));
        }
        if !self.read_roots.contains(&promoted) {
            self.read_roots.push(promoted);
        }
        Ok(self)
    }

    /// The single command line a shell-shaped backend would carry. Quoting is
    /// only ever needed for whitespace, because construction refuses the bytes
    /// that would let an argument change the command.
    pub fn command_line(&self) -> String {
        verbatim_command_line(&self.executable, &self.arguments)
    }

    /// Canonical digest of the exact material, committed into the evidence
    /// fingerprint (P20.3): two runs sharing a digest really did share a
    /// spawnable tree, toolchain and ceiling.
    pub fn digest(&self) -> String {
        r_code_harness_protocol::canonical_input_hash(&serde_json::json!({
            "executable": self.executable.to_string_lossy(),
            "arguments": self.arguments,
            "cwd": canonical(&self.cwd),
            "toolchain": self.toolchain,
            "readRoots": self.read_roots,
            "network": self.network,
        }))
    }

    /// The spec is spawnable only under the offline ceiling it was built with;
    /// a widened ceiling is a construction error, never a runtime option.
    pub fn admits_spawn(&self) -> Result<(), CheckSpecError> {
        if self.network.is_offline() {
            Ok(())
        } else {
            Err(CheckSpecError::NetworkNotOffline)
        }
    }
}

fn reject_reinterpretable(value: &str) -> Result<(), CheckSpecError> {
    match value.chars().find(|c| SHELL_REINTERPRETABLE.contains(c)) {
        Some(found) => Err(CheckSpecError::Reinterpretable(found.to_string())),
        None if value.contains('"') => Err(CheckSpecError::Reinterpretable("\"".to_string())),
        None => Ok(()),
    }
}

fn visible_git(value: &str) -> bool {
    value
        .split(['/', '\\'])
        .any(|component| component.eq_ignore_ascii_case(".git"))
}

/// Forward-slash canonical form so one material digests identically wherever
/// the host happens to run it.
fn canonical(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// The one place frozen program + arguments become a shell-shaped string,
/// shared by check and preparation material so neither can drift into a
/// dialect the other's refusals do not cover.
fn verbatim_command_line(executable: &Path, arguments: &[String]) -> String {
    let program = executable.to_string_lossy();
    if arguments.is_empty() {
        return program.to_string();
    }
    let joined = arguments
        .iter()
        .map(|argument| {
            if argument.chars().any(char::is_whitespace) {
                format!("\"{argument}\"")
            } else {
                argument.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!("{program} {joined}")
}

/// The dependency kinds v1 prepares. Modelled rather than sniffed: the
/// mandatory-argument rule is the only thing standing between a preparation
/// run and candidate-declared lifecycle code, so it needs a kind, not a
/// substring of a command line a caller assembled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyPrepKind {
    Cargo,
    Npm,
}

impl DependencyPrepKind {
    /// The bare toolchain program this kind may run. Deriving the executable
    /// from the kind is what makes a path the candidate controls unreachable.
    pub fn executable(&self) -> &'static str {
        match self {
            Self::Cargo => "cargo",
            Self::Npm => "npm",
        }
    }

    /// Arguments the run must carry (P21.2): Cargo only fetches and honours
    /// the lock, npm only installs the locktree and never runs a script.
    /// Absent any one is a refusal, so no caller can downgrade to
    /// `cargo build` / `npm install`.
    pub fn required_arguments(&self) -> &'static [&'static str] {
        match self {
            Self::Cargo => &["fetch", "--locked"],
            Self::Npm => &["ci", "--ignore-scripts"],
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Cargo => "cargo",
            Self::Npm => "npm",
        }
    }
}

/// The three content-addressed trees one preparation run is mounted with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrepCachePaths {
    /// The read-only lower layer: an already promoted slot, or empty.
    pub lower_cache_readonly: PathBuf,
    /// The run-owned writable layer, strictly inside the working directory.
    pub overlay: PathBuf,
    /// Where validated bytes land after promotion, outside the overlay.
    pub promoted_cache: PathBuf,
}

/// The complete truth of one dependency-preparation run (P21).
///
/// Flat and total: the frozen program and verbatim arguments, the private tree
/// as cwd, the declared toolchain identity, the network ask, the read-only
/// lower cache, the run-owned overlay the process writes, the slot validated
/// bytes are promoted into, and the exact inherited-environment allowlist.
/// Construction is the only way to hold one, and its digest binds all of it,
/// so a widened field is a different run rather than the same run doing more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyPrepSpec {
    pub kind: DependencyPrepKind,
    pub executable: PathBuf,
    /// The frozen arguments, exactly as declared, without the executable.
    pub arguments: Vec<String>,
    pub cwd: PathBuf,
    pub toolchain: String,
    pub network: NetworkCeiling,
    pub lower_cache_readonly: PathBuf,
    pub overlay: PathBuf,
    pub promoted_cache: PathBuf,
    pub environment_keys: Vec<String>,
}

impl DependencyPrepSpec {
    /// Freeze one preparation run or refuse it: the check admission rules
    /// (bare program, verbatim arguments, absolute cwd, toolchain identity)
    /// plus the preparation-only ones — mandatory arguments, no provider
    /// credential in the environment, and a cache layout that keeps the
    /// promoted bytes outside the overlay that dies with the run.
    pub fn build(
        kind: DependencyPrepKind,
        arguments: &[String],
        cwd: &Path,
        toolchain: &str,
        network: NetworkCeiling,
        cache: &PrepCachePaths,
        environment_keys: &[String],
    ) -> Result<Self, CheckSpecError> {
        let spec = Self {
            kind,
            executable: PathBuf::from(kind.executable()),
            arguments: arguments.to_vec(),
            cwd: cwd.to_path_buf(),
            toolchain: toolchain.to_string(),
            network,
            lower_cache_readonly: cache.lower_cache_readonly.clone(),
            overlay: cache.overlay.clone(),
            promoted_cache: cache.promoted_cache.clone(),
            environment_keys: environment_keys.to_vec(),
        };
        spec.validate()?;
        Ok(spec)
    }

    /// The admission rules, re-derivable at the spawn boundary: every field is
    /// public, so a spec widened after construction is refused exactly like a
    /// bad build rather than quietly running wider material.
    fn validate(&self) -> Result<(), CheckSpecError> {
        let program = self.executable.to_string_lossy().to_string();
        if program != self.kind.executable() {
            return Err(CheckSpecError::PrepProgramMismatch(program));
        }
        if program.is_empty() || program.contains(['/', '\\', '\0']) {
            return Err(CheckSpecError::Executable(program));
        }
        reject_reinterpretable(&program)?;
        for argument in &self.arguments {
            reject_reinterpretable(argument)?;
        }
        for required in self.kind.required_arguments() {
            if !self.arguments.iter().any(|argument| argument == required) {
                return Err(CheckSpecError::PrepMissingArgument(
                    self.kind.as_str().to_string(),
                    (*required).to_string(),
                ));
            }
        }
        if !self.cwd.is_absolute() {
            return Err(CheckSpecError::CwdNotAbsolute(
                self.cwd.to_string_lossy().to_string(),
            ));
        }
        for path in [
            &self.lower_cache_readonly,
            &self.overlay,
            &self.promoted_cache,
        ] {
            if !path.is_absolute() {
                return Err(CheckSpecError::PrepPathNotAbsolute(
                    path.to_string_lossy().to_string(),
                ));
            }
        }
        if self.toolchain.trim().is_empty() {
            return Err(CheckSpecError::EmptyToolchain);
        }
        if matches!(self.network, NetworkCeiling::HostNetwork) {
            return Err(CheckSpecError::PrepNetworkUnsupported);
        }
        // The overlay is run-owned and lives inside the private tree; the
        // promoted slot lives outside it, because a Check may mount the slot
        // read-only and must never mount what the preparation wrote (acc. 3).
        if !self.overlay.starts_with(&self.cwd) || self.overlay == self.cwd {
            return Err(CheckSpecError::PrepOverlayOutsideCwd(
                canonical(&self.overlay),
                canonical(&self.cwd),
            ));
        }
        if self.promoted_cache.starts_with(&self.overlay) {
            return Err(CheckSpecError::PrepPromotedInsideOverlay(
                canonical(&self.promoted_cache),
                canonical(&self.overlay),
            ));
        }
        if self.promoted_cache.components().any(|component| {
            matches!(component, std::path::Component::Normal(name)
                if name.to_string_lossy().eq_ignore_ascii_case("node_modules"))
        }) {
            return Err(CheckSpecError::PrepPromotedNodeModules(canonical(
                &self.promoted_cache,
            )));
        }
        for path in [
            &self.cwd,
            &self.overlay,
            &self.lower_cache_readonly,
            &self.promoted_cache,
        ] {
            let canonical = canonical(path);
            if visible_git(&canonical) {
                return Err(CheckSpecError::PrepVisibleGit(canonical));
            }
        }
        for key in &self.environment_keys {
            if addresses_credential(key) {
                return Err(CheckSpecError::PrepCredentialEnvironmentKey(key.clone()));
            }
        }
        Ok(())
    }

    /// The command line the gated backend would carry, from the same freezing
    /// rule a check uses.
    pub fn command_line(&self) -> String {
        verbatim_command_line(&self.executable, &self.arguments)
    }

    /// Canonical digest of the exact run material: two preparations sharing a
    /// digest really did share a program, arguments, tree, ceiling, all three
    /// cache layers and the environment allowlist.
    pub fn digest(&self) -> String {
        r_code_harness_protocol::canonical_input_hash(&serde_json::json!({
            "kind": self.kind.as_str(),
            "executable": self.executable.to_string_lossy(),
            "arguments": self.arguments,
            "cwd": canonical(&self.cwd),
            "toolchain": self.toolchain,
            "network": self.network,
            "lowerCacheReadonly": canonical(&self.lower_cache_readonly),
            "overlay": canonical(&self.overlay),
            "promotedCache": canonical(&self.promoted_cache),
            "environmentKeys": self.environment_keys,
        }))
    }

    /// Preparation may only spawn under the ceiling it was frozen with, and
    /// host-network is refused outright: it is unsupported in v1 and plan
    /// validation already rejects it, so a spec carrying it is a bug, not an
    /// approval away from being legal.
    pub fn admits_spawn(&self) -> Result<(), CheckSpecError> {
        self.validate()
    }
}

/// Whether an inherited environment name is a credential a preparation run
/// must never see. Bare `KEY` is refused exactly rather than as a substring,
/// so the rule stays honest about what it blocks.
fn addresses_credential(key: &str) -> bool {
    const NEEDLES: [&str; 9] = [
        "API_KEY",
        "APIKEY",
        "TOKEN",
        "PASSWORD",
        "SECRET",
        "AUTHORIZATION",
        "PROVIDER",
        "COOKIE",
        "CREDENTIAL",
    ];
    let upper = key.to_ascii_uppercase();
    NEEDLES.iter().any(|needle| upper.contains(needle)) || upper == "KEY"
}

/// The shared execution service.
pub struct ExecutionService {
    default: Arc<dyn CommandExecutionBackend>,
    selected: tokio::sync::RwLock<Option<Arc<dyn CommandExecutionBackend>>>,
    /// Preparation never rides the shell-shaped backend chain: it needs a
    /// route that receives the whole spec. Unbound means refused.
    prep: tokio::sync::RwLock<Option<Arc<SandboxedPrepBackend>>>,
    authorization: Arc<AuthorizationService>,
    abort: Arc<AtomicBool>,
}

impl ExecutionService {
    /// Service over the default local shell backend.
    pub fn local(authorization: Arc<AuthorizationService>) -> Self {
        Self {
            default: Arc::new(LocalShellBackend::new()),
            selected: tokio::sync::RwLock::new(None),
            prep: tokio::sync::RwLock::new(None),
            authorization,
            abort: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Service over an explicit default backend (tests, docker).
    pub fn with_backend(
        default: Arc<dyn CommandExecutionBackend>,
        authorization: Arc<AuthorizationService>,
    ) -> Self {
        Self {
            default,
            selected: tokio::sync::RwLock::new(None),
            prep: tokio::sync::RwLock::new(None),
            authorization,
            abort: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Select the backend used by subsequent runs (verification prep may
    /// pin its own); None falls back to the default.
    pub async fn select_backend(&self, backend: Option<Arc<dyn CommandExecutionBackend>>) {
        *self.selected.write().await = backend;
    }

    /// Bind the gate that decides whether a dependency preparation may run.
    /// With no gate bound, `run_prep` refuses: there is no default route for a
    /// fetch, because the default would have to be the unsandboxed shell.
    pub async fn select_prep_gate(&self, gate: Option<Arc<SandboxedPrepBackend>>) {
        *self.prep.write().await = gate;
    }

    /// What a preparation would resolve to right now, for callers and tests
    /// that must prove the route is gated rather than defaulted.
    pub async fn prep_route_id(&self) -> &'static str {
        match &*self.prep.read().await {
            Some(gate) => gate.route_id(),
            None => "prep-unbound",
        }
    }

    /// The backend a run would currently use.
    async fn effective_backend(&self) -> Arc<dyn CommandExecutionBackend> {
        match &*self.selected.read().await {
            Some(selected) => selected.clone(),
            None => self.default.clone(),
        }
    }

    /// The id of the default backend a run falls back to when no backend is
    /// selected. The required-checks path exposes this so callers can prove
    /// they resolve to the sandboxed backend and never to `local`.
    pub fn default_backend_id(&self) -> &'static str {
        self.default.backend_id()
    }

    /// Request abort of in-flight and future commands (cancellation).
    pub fn abort_all(&self) {
        self.abort.store(true, Ordering::SeqCst);
    }

    fn clear_abort(&self) {
        self.abort.store(false, Ordering::SeqCst);
    }

    /// Run one authorized command. Authorization happens here; backends
    /// never re-decide semantics, they only execute.
    pub async fn run_authorized(
        &self,
        descriptor: &OperationDescriptor,
        workspace: &WorkspaceCapability,
        permissions: &EffectivePermissions,
        command: &str,
        cwd: &std::path::Path,
        timeout: Duration,
    ) -> Result<CollectedOutput, ExecutionError> {
        self.execute(
            descriptor,
            workspace,
            permissions,
            &CommandSpec {
                command: command.to_string(),
                cwd: cwd.to_path_buf(),
                timeout,
            },
        )
        .await
    }

    /// Run one required check from its frozen material (P20.1). Authorization
    /// and execution see the same exact executable + arguments the sandbox
    /// would spawn, and material that is not spawnable is refused here — before
    /// a backend is chosen, so no route can run it.
    pub async fn run_check(
        &self,
        spec: &CheckSpawnSpec,
        descriptor: &OperationDescriptor,
        workspace: &WorkspaceCapability,
        permissions: &EffectivePermissions,
        timeout: Duration,
    ) -> Result<CollectedOutput, ExecutionError> {
        spec.admits_spawn()
            .map_err(|error| ExecutionError::Spec(error.to_string()))?;
        self.execute(
            descriptor,
            workspace,
            permissions,
            &CommandSpec {
                command: spec.command_line(),
                cwd: spec.cwd.clone(),
                timeout,
            },
        )
        .await
    }

    /// Run one dependency-preparation from its frozen material (P21). The
    /// network ask is proven here, before a backend is chosen: task
    /// permissions never imply it, so an unapproved fetch cannot reach a
    /// process even through a route that re-reads the descriptor.
    pub async fn run_prep(
        &self,
        spec: &DependencyPrepSpec,
        descriptor: &OperationDescriptor,
        workspace: &WorkspaceCapability,
        permissions: &EffectivePermissions,
        timeout: Duration,
    ) -> Result<CollectedOutput, ExecutionError> {
        spec.admits_spawn()
            .map_err(|error| ExecutionError::Spec(error.to_string()))?;
        if !spec.network.is_offline()
            && (!permissions.allow_network || !descriptor.prep_network.proves(spec.network))
        {
            return Err(ExecutionError::Denied(format!(
                "dependency preparation network at {} requires an exact effect approval",
                spec.network.as_str()
            )));
        }
        self.authorize_only(descriptor, workspace, permissions)?;
        let gate = match &*self.prep.read().await {
            Some(gate) => gate.clone(),
            None => {
                return Err(ExecutionError::Backend(
                    "no dependency-preparation gate is bound; a fetch has no default route".into(),
                ))
            }
        };
        self.clear_abort();
        gate.execute(spec, Some(&self.abort), timeout)
            .await
            .map_err(|error| ExecutionError::Backend(error.to_string()))
    }

    async fn execute(
        &self,
        descriptor: &OperationDescriptor,
        workspace: &WorkspaceCapability,
        permissions: &EffectivePermissions,
        spec: &CommandSpec,
    ) -> Result<CollectedOutput, ExecutionError> {
        self.authorize_only(descriptor, workspace, permissions)?;
        let backend = self.effective_backend().await;
        self.clear_abort();
        let handle = backend
            .spawn(spec, Some(&self.abort))
            .await
            .map_err(|e| ExecutionError::Backend(e.to_string()))?;
        backend
            .collect(handle, spec, Some(&self.abort))
            .await
            .map_err(|e| ExecutionError::Backend(e.to_string()))
    }

    /// The one authorization decision every route shares, so preparation can
    /// never be authorized by a different rule than a command run.
    fn authorize_only(
        &self,
        descriptor: &OperationDescriptor,
        workspace: &WorkspaceCapability,
        permissions: &EffectivePermissions,
    ) -> Result<(), ExecutionError> {
        match self
            .authorization
            .authorize(descriptor, workspace, permissions, &Default::default())
        {
            AuthorizationDecision::Allowed => Ok(()),
            AuthorizationDecision::RequiresApproval { summary } => {
                Err(ExecutionError::ApprovalRequired(summary))
            }
            AuthorizationDecision::Denied(reason) => {
                Err(ExecutionError::Denied(reason.to_string()))
            }
        }
    }

    /// The bash-tool route: same authorization + backend, tool-shaped
    /// descriptor. Future verification preparation calls the same path.
    pub async fn run_bash(
        &self,
        workspace: &WorkspaceCapability,
        permissions: &EffectivePermissions,
        command: &str,
        cwd: &std::path::Path,
        timeout: Duration,
    ) -> Result<CollectedOutput, ExecutionError> {
        let descriptor = OperationDescriptor::tool_call("bash", vec![command.to_string()], None);
        self.run_authorized(&descriptor, workspace, permissions, command, cwd, timeout)
            .await
    }
}

impl From<ExecutionError> for ServiceError {
    fn from(error: ExecutionError) -> Self {
        ServiceError::Failure(error.to_string())
    }
}

/// Whether a required-checks backend may spawn. Derived from the platform
/// [`crate::services::sandbox`] SafetyCapabilityReport: only an Activated
/// report admits execution, and it runs on the bound native backend. Every
/// other verdict (Unsupported / SafeDisabled / NotActivated) is a hard
/// refusal — never a fallback to the unsandboxed local shell (INV-06/INV-07).
#[derive(Debug, Clone)]
pub enum CheckSandboxGate {
    /// Platform safety capability is Activated for this exact boot identity.
    Activated,
    /// Not activated: every command fails closed with this reason, which the
    /// verification runner surfaces as `CheckStatus::Unavailable`.
    Closed { reason: String },
}

/// The P20 required-checks execution backend. It refuses to spawn unless the
/// platform safety report is Activated AND a native backend is bound; with no
/// native backend this wave, required checks never start a local process and
/// always report Unavailable. There is deliberately no `local` route.
pub struct SandboxedCheckBackend {
    gate: CheckSandboxGate,
    native: Option<Arc<dyn CommandExecutionBackend>>,
}

impl SandboxedCheckBackend {
    pub fn new(gate: CheckSandboxGate, native: Option<Arc<dyn CommandExecutionBackend>>) -> Self {
        Self { gate, native }
    }

    /// A gate that refuses every spawn — the honest production default on any
    /// platform whose safety report has not reached Activated.
    pub fn closed(reason: impl Into<String>) -> Self {
        Self::new(
            CheckSandboxGate::Closed {
                reason: reason.into(),
            },
            None,
        )
    }
}

#[async_trait::async_trait]
impl CommandExecutionBackend for SandboxedCheckBackend {
    fn backend_id(&self) -> &'static str {
        match &self.gate {
            CheckSandboxGate::Activated => "sandbox-native",
            CheckSandboxGate::Closed { .. } => "sandbox-gated",
        }
    }

    async fn spawn(
        &self,
        spec: &CommandSpec,
        abort_flag: Option<&AtomicBool>,
    ) -> Result<CommandHandle, ProductError> {
        match &self.gate {
            // An activated gate runs only on the bound native backend; a
            // missing native binding is a second fail-closed layer, never a
            // fall back to the local shell.
            CheckSandboxGate::Activated => {
                // Third layer: a caller that bypassed [`ExecutionService::run_check`]
                // still cannot hand shell-reinterpretable material or a relative
                // working directory to the native backend.
                if let Some(found) = spec
                    .command
                    .chars()
                    .find(|c| SHELL_REINTERPRETABLE.contains(c))
                {
                    return Err(ProductError::Other(format!(
                        "check material contains {found:?}, which a sandbox cannot execute verbatim"
                    )));
                }
                if !spec.cwd.is_absolute() {
                    return Err(ProductError::Other(format!(
                        "check working directory {} is not absolute",
                        spec.cwd.display()
                    )));
                }
                match &self.native {
                    Some(native) => native.spawn(spec, abort_flag).await,
                    None => Err(ProductError::Other(
                        "no native sandbox backend is bound for an activated check".to_string(),
                    )),
                }
            }
            CheckSandboxGate::Closed { reason } => Err(ProductError::Other(reason.clone())),
        }
    }

    async fn collect(
        &self,
        handle: CommandHandle,
        spec: &CommandSpec,
        abort_flag: Option<&AtomicBool>,
    ) -> Result<CollectedOutput, ProductError> {
        // The gate guards collection too: a hand-assembled Closed gate that
        // also carries a native binding must still never drain output, so a
        // handle can outlive its refusal only by the caller forging one, which
        // this arm still blocks.
        if let CheckSandboxGate::Closed { reason } = &self.gate {
            return Err(ProductError::Other(reason.clone()));
        }
        match &self.native {
            Some(native) => native.collect(handle, spec, abort_flag).await,
            None => Err(ProductError::Other(
                "check backend holds no bound backend to collect from".to_string(),
            )),
        }
    }
}

/// Whether a dependency-preparation backend may spawn. Preparation fetches
/// over the network and writes bytes the host later promotes, so it is gated
/// exactly like a required check: only an Activated platform report admits it.
#[derive(Debug, Clone)]
pub enum PrepGate {
    /// Platform safety capability is Activated for this exact boot identity.
    Activated,
    /// Not activated: every preparation command fails closed with this reason.
    Closed { reason: String },
}

/// A route that can actually run one dependency preparation.
///
/// Preparation is not a shell command. The process must see the run's private
/// tree as its working directory, the content-addressed lower cache read-only,
/// its own overlay writable and the promoted slot out of reach, with an
/// environment consisting of exactly `environment_keys`. No implementation of
/// the shell-shaped [`CommandExecutionBackend`] can carry that material —
/// `CommandSpec` has fields for command, cwd and timeout only — so a route
/// handed just a command line would silently inherit the host's provider
/// credentials (acceptance ②) and write wherever it liked. Requiring the whole
/// [`DependencyPrepSpec`] at the spawn boundary makes those properties the
/// route's obligation instead of a comment about it.
#[async_trait::async_trait]
pub trait DependencyPrepRoute: Send + Sync {
    /// Stable route identity, reported in refusals and audits.
    fn route_id(&self) -> &'static str;

    /// Run one preparation to completion under exactly this material.
    async fn run(
        &self,
        spec: &DependencyPrepSpec,
        abort_flag: Option<&AtomicBool>,
        timeout: Duration,
    ) -> Result<CollectedOutput, ProductError>;
}

/// The P21 dependency-preparation gate. It mirrors [`SandboxedCheckBackend`]
/// because preparation is the more powerful effect: it runs only when the
/// platform report is Activated AND a route is bound, and it never resolves to
/// the local shell. Production binds no route, so preparation stays refused.
pub struct SandboxedPrepBackend {
    gate: PrepGate,
    route: Option<Arc<dyn DependencyPrepRoute>>,
}

impl SandboxedPrepBackend {
    pub fn new(gate: PrepGate, route: Option<Arc<dyn DependencyPrepRoute>>) -> Self {
        Self { gate, route }
    }

    /// A gate that refuses every preparation — the honest production default
    /// while no platform sandbox admits it.
    pub fn closed(reason: impl Into<String>) -> Self {
        Self::new(
            PrepGate::Closed {
                reason: reason.into(),
            },
            None,
        )
    }

    pub fn route_id(&self) -> &'static str {
        match (&self.gate, &self.route) {
            (PrepGate::Activated, Some(_)) => "sandbox-prep-native",
            (PrepGate::Activated, None) => "sandbox-prep-unbound",
            (PrepGate::Closed { .. }, _) => "sandbox-prep-gated",
        }
    }

    /// The gate decides, the route enforces. A closed gate returns its reason
    /// verbatim; an Activated gate with no bound route refuses rather than
    /// falling back to any shell, so there is no route from an approval to an
    /// unsandboxed fetch.
    pub async fn execute(
        &self,
        spec: &DependencyPrepSpec,
        abort_flag: Option<&AtomicBool>,
        timeout: Duration,
    ) -> Result<CollectedOutput, ProductError> {
        match &self.gate {
            PrepGate::Closed { reason } => Err(ProductError::Other(reason.clone())),
            PrepGate::Activated => match &self.route {
                Some(route) => {
                    spec.admits_spawn().map_err(|error| {
                        ProductError::Other(format!("preparation material refused: {error}"))
                    })?;
                    route.run(spec, abort_flag, timeout).await
                }
                None => Err(ProductError::Other(
                    "no native sandbox route is bound for an activated preparation".to_string(),
                )),
            },
        }
    }
}
