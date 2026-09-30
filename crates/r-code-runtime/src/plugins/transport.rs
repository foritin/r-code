//! Bounded bidirectional stdio transport for plugin processes.
//!
//! One plugin process per run. stdout carries NDJSON JSON-RPC frames only;
//! stderr is captured into a bounded diagnostic tail. An independent reader
//! task keeps serving plugin callbacks (nested host calls) while host
//! requests are pending. Frame, queue, initialize-timeout and
//! cancellation-grace limits are enforced; after the grace period the
//! process TREE is swept — a direct child alone is never enough (P23.3).
//!
//! Since P23 no launch here is direct: the P23.1 section carries the frozen
//! material, the containment policy and the record gating every child, and
//! [`spawn_plugin`] the order they land in — on Windows, resume follows `Running`.

#[cfg(windows)]
use crate::process_guard::windows::{
    spawn_suspended_with_job, GuardianError, JobbedSuspendedChild, PipeRead, RawSpawnSpec,
};
use crate::process_guard::{BootIdentity, ProcessOwnerIdentity};
use crate::services::process_profiles::ProcessProfileEffect;
use crate::services::process_supervisor::{
    DeterministicSupervisorJournal, InheritedObject, PrepareDisposition, SpawnSpec,
    SupervisorJournal, SupervisorPhase, SupervisorRecord, MAX_OUTPUT_BUFFER_BYTES,
};
use crate::services::sandbox::SafetyActivation;
use r_code_harness_protocol::rpc::{
    decode_frame, encode_frame, FrameError, RpcError, RpcId, RpcMessage, RpcNotification,
    RpcRequest, RpcResponse, CANCEL_GRACE, INITIALIZE_TIMEOUT, MAX_FRAME_BYTES, MAX_QUEUE_BYTES,
};
use r_code_harness_protocol::services::{InitializeParams, InitializeResult};
use r_code_harness_protocol::NetworkCeiling;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;
#[cfg(not(windows))]
use tokio::io::AsyncWriteExt;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
#[cfg(not(windows))]
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot, Mutex};

/// Bounded stderr tail kept for diagnostics.
pub const STDERR_TAIL_BYTES: usize = 64 * 1024;

/// Limits enforced by one transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportLimits {
    pub max_frame_bytes: usize,
    pub max_queue_bytes: usize,
    pub initialize_timeout: Duration,
    pub cancel_grace: Duration,
}

impl Default for TransportLimits {
    fn default() -> Self {
        Self {
            max_frame_bytes: MAX_FRAME_BYTES,
            max_queue_bytes: MAX_QUEUE_BYTES,
            initialize_timeout: INITIALIZE_TIMEOUT,
            cancel_grace: CANCEL_GRACE,
        }
    }
}

/// Transport-level failures.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum TransportError {
    #[error("failed to spawn plugin: {0}")]
    Spawn(String),
    #[error("protocol fault: {0}")]
    Fault(String),
    #[error("frame error: {0}")]
    Frame(#[from] FrameError),
    #[error("outbound queue overflow (> {0} bytes pending)")]
    QueueOverflow(usize),
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
    #[error("plugin connection closed while a call was pending: {0}")]
    Closed(String),
    #[error("rpc error {code}: {message}")]
    Rpc { code: i64, message: String },
    /// P23: the launch was refused. `code` is the stable, machine-checkable
    /// reason; nothing was created and nothing can be retried wider.
    #[error("harness launch refused [{code}]: {reason}")]
    Refused { code: &'static str, reason: String },
}

// ---------------------------------------------------------------------------
// P23.1 — the frozen, clean harness spawn material
//
// Every launch freezes a HarnessSpawnPlan first: cleared parent environment
// with only the supervisor's own allowlist, a non-workspace working directory,
// NetworkCeiling::Offline, and the child-process requirement the pinned
// manifest declared. The plan validates into a process_supervisor::SpawnSpec
// BEFORE anything is created; a refusal at any later step is a refusal with no
// process, and an unprovable containment is never a degraded run.
// ---------------------------------------------------------------------------

/// Directory a harness runs under. A harness never runs at a checked-out
/// workspace: `NoWorkspace` is the effect class the interactive ceiling already
/// admits (P22), so the plan refuses every other working directory.
pub const HARNESS_SCRATCH_DIR: &str = "r-code-harness-scratch";

/// The complete environment a harness child may receive. This is exactly the
/// supervisor's own allowlist, because the parent environment is cleared first:
/// what is not named here simply does not exist for the child, and a name that
/// is not in the list is refused rather than quietly inherited.
pub const HARNESS_ENVIRONMENT_ALLOWLIST: [&str; 14] = [
    "COMSPEC",
    "HOME",
    "LANG",
    "LC_ALL",
    "PATH",
    "PATHEXT",
    "SYSTEMROOT",
    "TEMP",
    "TERM",
    "TMP",
    "TMPDIR",
    "TZ",
    "USERPROFILE",
    "WINDIR",
];

/// Environment names that speak for a credential. The list and the bare-`KEY`
/// rule are P21's dependency-preparation refusal list, reused verbatim so one
/// secret vocabulary governs every child the host launches.
const CREDENTIAL_ENVIRONMENT_NEEDLES: [&str; 9] = [
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

/// Whether an environment name is a credential a harness must never receive.
fn addresses_credential(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    CREDENTIAL_ENVIRONMENT_NEEDLES
        .iter()
        .any(|needle| upper.contains(needle))
        || upper == "KEY"
}

/// A `.git` component anywhere in a path makes the repository visible, which
/// INV-06 forbids for every harness working directory.
fn visible_git(value: &str) -> bool {
    value
        .split(['/', '\\'])
        .any(|component| component.eq_ignore_ascii_case(".git"))
}

/// Forward-slash canonical form, so one path compares identically wherever the
/// host happens to run it. Windows' verbatim `\\?\` prefix is stripped: it is a
/// namespace marker, not part of the location, and leaving it in would make a
/// canonical path look like it escaped the very root it was canonicalized from.
fn canonical(value: &Path) -> String {
    let text = value.to_string_lossy().replace('\\', "/");
    let trimmed = text.strip_prefix("//?/").unwrap_or(text.as_str());
    trimmed.trim_end_matches('/').to_string()
}

/// The same location the kernel resolves a path to, when it resolves it at all,
/// with the verbatim namespace prefix taken back off. This exact text is what
/// the child is handed as its working directory, and `\\?\` is an object-manager
/// marker rather than part of the location.
fn canonicalized(value: &Path) -> PathBuf {
    strip_verbatim(std::fs::canonicalize(value).unwrap_or_else(|_| value.to_path_buf()))
}

/// Drop a `\\?\` (or `\\?\UNC\`) prefix from a path the kernel just resolved.
fn strip_verbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(remote) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{remote}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(local) => PathBuf::from(local),
        None => path,
    }
}

/// What the pinned manifest declared about the children of one executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildProcessRequirement {
    /// Declared single process (P23.1): the package never forks and never execs
    /// another image, so it asks the host to deny both.
    SingleProcess,
    /// Silent. Never read as innocence: the host enforces containment, and on
    /// macOS silence is a refusal.
    Undeclared,
}

/// The host-side table binding an installed package executable to the
/// declaration its manifest carried. Catalog registration and every catalog
/// read write it; the transport only ever reads it, keyed by the exact
/// executable it was asked to launch.
fn requirement_table() -> &'static RwLock<BTreeMap<String, ChildProcessRequirement>> {
    static TABLE: OnceLock<RwLock<BTreeMap<String, ChildProcessRequirement>>> = OnceLock::new();
    TABLE.get_or_init(Default::default)
}

fn requirement_key(executable: &Path) -> String {
    canonical(executable).to_ascii_lowercase()
}

/// Record what one package executable declared. `false` retracts the binding,
/// which is what a removed package must do: a stale single-process claim could
/// otherwise deny a replacement package its normal policy.
pub fn register_child_process_requirement(executable: &Path, requires_single_process: bool) {
    let requirement = if requires_single_process {
        ChildProcessRequirement::SingleProcess
    } else {
        ChildProcessRequirement::Undeclared
    };
    requirement_table()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(requirement_key(executable), requirement);
}

/// The declaration carried by the exact executable about to be launched.
pub fn child_process_requirement(executable: &Path) -> ChildProcessRequirement {
    *requirement_table()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&requirement_key(executable))
        .unwrap_or(&ChildProcessRequirement::Undeclared)
}

/// Host-bound launch material: where a harness may run, which safety verdict
/// the platform issued, and which journal its tree record is written to.
#[derive(Clone)]
pub struct HarnessLaunchConfig {
    pub scratch_root: PathBuf,
    pub activation: SafetyActivation,
    pub journal: Arc<dyn SupervisorJournal>,
    pub output_capacity_bytes: usize,
}

impl HarnessLaunchConfig {
    /// The composition-default: a host scratch root, no activated safety report
    /// (never invented — an unbound boot cannot claim activation) and the
    /// reference supervisor journal. The run manager replaces this with the
    /// platform gate verdict and the store-backed journal when it composes.
    pub fn host_default() -> Self {
        Self {
            scratch_root: std::env::temp_dir().join(HARNESS_SCRATCH_DIR),
            activation: SafetyActivation::NotActivated {
                reason: "no safety report is bound for this boot",
                status: Some("unsupported".to_string()),
            },
            journal: Arc::new(DeterministicSupervisorJournal::default()),
            output_capacity_bytes: MAX_OUTPUT_BUFFER_BYTES,
        }
    }
}

fn config_slot() -> &'static RwLock<Option<HarnessLaunchConfig>> {
    static SLOT: OnceLock<RwLock<Option<HarnessLaunchConfig>>> = OnceLock::new();
    SLOT.get_or_init(Default::default)
}

/// Bind the launch material the transport must use (host composition only).
pub fn bind_harness_launch_config(config: HarnessLaunchConfig) {
    *config_slot()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(config);
}

/// Publish the host's own scratch root: the only tree a harness may run in.
pub fn bind_harness_scratch_root(root: PathBuf) {
    with_config(|config| config.scratch_root = root);
}

/// Publish the platform safety verdict the host just evaluated. The verdict is
/// passed through exactly as the gate issued it: a `NotActivated` boot is never
/// rewritten into an activation, and a launch that needs the report and does not
/// have it is refused by the gate's own reason.
pub fn bind_harness_activation(activation: SafetyActivation) {
    with_config(|config| config.activation = activation);
}

/// Mutate the bound material, composing the host default on first use so a
/// partially bound boot still refuses rather than guessing.
fn with_config(amend: impl FnOnce(&mut HarnessLaunchConfig)) {
    let mut guard = config_slot()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let config = guard.get_or_insert_with(HarnessLaunchConfig::host_default);
    amend(config);
}

fn launch_config() -> HarnessLaunchConfig {
    config_slot()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .unwrap_or_else(HarnessLaunchConfig::host_default)
}

/// The complete, validated truth of one harness launch: the frozen executable
/// and arguments, the non-workspace working directory, the cleared environment,
/// the network ceiling and the declared child-process requirement. It is the
/// same frozen-material discipline the managed process service uses, and a plan
/// that cannot be validated means no process exists afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessSpawnPlan {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub cwd: PathBuf,
    pub environment: BTreeMap<String, String>,
    pub network: NetworkCeiling,
    pub requirement: ChildProcessRequirement,
}

impl HarnessSpawnPlan {
    /// Freeze the launch material or refuse it, before anything is created.
    pub fn build(
        executable: &Path,
        argv: &[String],
        config: &HarnessLaunchConfig,
    ) -> Result<Self, TransportError> {
        if executable.as_os_str().is_empty() {
            return Err(refused(
                "harness-executable-empty",
                "a launch must name an executable",
            ));
        }
        if !executable.is_absolute() {
            return Err(refused(
                "harness-executable-relative",
                format!(
                    "{} is not an absolute host-resolved path",
                    executable.display()
                ),
            ));
        }
        for argument in argv {
            if argument.contains('\0') {
                return Err(refused(
                    "harness-argument-unrepresentable",
                    "an argument cannot be carried across the spawn boundary".to_string(),
                ));
            }
        }
        let plan = Self {
            executable: executable.to_path_buf(),
            arguments: argv.to_vec(),
            cwd: scratch_cwd(&config.scratch_root)?,
            environment: clean_environment()?,
            // P23 routes Native and third-party transport through an OFFLINE
            // sandbox: a plugin reaches the network only through the host
            // services it was granted, never through its own sockets.
            network: NetworkCeiling::Offline,
            requirement: child_process_requirement(executable),
        };
        plan.validate(config)?;
        Ok(plan)
    }

    /// The supervisor-shaped spawn specification this material freezes into.
    /// Ambient inheritance is disabled, so the environment map is the complete
    /// truth and the handle set is empty: the transport owns its stdio pipes.
    pub fn to_spawn_spec(&self, config: &HarnessLaunchConfig) -> SpawnSpec {
        SpawnSpec {
            executable: self.executable.clone(),
            arguments: self.arguments.clone(),
            cwd: self.cwd.clone(),
            environment: self.environment.clone(),
            inherited_objects: Vec::<InheritedObject>::new(),
            output_capacity_bytes: config.output_capacity_bytes,
        }
    }

    /// Re-derive every rule at the boundary: a plan widened after construction
    /// is refused exactly like a bad build instead of running wider material.
    pub fn validate(&self, config: &HarnessLaunchConfig) -> Result<(), TransportError> {
        for key in self.environment.keys() {
            if !HARNESS_ENVIRONMENT_ALLOWLIST.contains(&key.as_str()) {
                return Err(refused(
                    "harness-environment-not-allowlisted",
                    format!("{key} is outside the harness environment allowlist"),
                ));
            }
            if addresses_credential(key) {
                return Err(refused(
                    "harness-environment-credential",
                    format!("{key} names a provider credential"),
                ));
            }
        }
        if !self.cwd.is_absolute() {
            return Err(refused(
                "harness-cwd-relative",
                format!("{} is not absolute", self.cwd.display()),
            ));
        }
        let scratch = canonical(&canonicalized(&config.scratch_root));
        let cwd = canonical(&self.cwd);
        if cwd != scratch && !cwd.starts_with(&format!("{scratch}/")) {
            return Err(refused(
                "harness-cwd-outside-scratch",
                format!("{cwd} escapes the host scratch root {scratch}"),
            ));
        }
        if visible_git(&cwd) || visible_git(&canonical(&self.executable)) {
            return Err(refused(
                "harness-git-visible",
                "a harness launch may not make the git directory visible".to_string(),
            ));
        }
        if self.network != NetworkCeiling::Offline {
            return Err(refused(
                "harness-network-not-offline",
                format!("{:?} is above the harness ceiling", self.network),
            ));
        }
        self.to_spawn_spec(config)
            .validate()
            .map_err(|error| refused("harness-spawn-material-invalid", error.to_string()))
    }
}

/// The cleared environment: exactly the allowlisted names the parent happens to
/// carry, and nothing else. A provider secret is therefore impossible by
/// construction, and one named in the allowlist is a refusal (see validate()).
fn clean_environment() -> Result<BTreeMap<String, String>, TransportError> {
    let mut environment = BTreeMap::new();
    for key in HARNESS_ENVIRONMENT_ALLOWLIST {
        if addresses_credential(key) {
            return Err(refused(
                "harness-environment-credential",
                format!("{key} names a provider credential"),
            ));
        }
        if let Ok(value) = std::env::var(key) {
            environment.insert(key.to_string(), value);
        }
    }
    Ok(environment)
}

/// The non-workspace working directory, created by the host so a harness can
/// never resolve a repository path relative to the daemon's own cwd.
fn scratch_cwd(root: &Path) -> Result<PathBuf, TransportError> {
    if root.as_os_str().is_empty() || !root.is_absolute() {
        return Err(refused(
            "harness-scratch-root-unbound",
            format!("{} is not an absolute scratch root", root.display()),
        ));
    }
    std::fs::create_dir_all(root).map_err(|error| {
        refused(
            "harness-scratch-root-unusable",
            format!("{}: {error}", root.display()),
        )
    })?;
    let canonical = canonicalized(root);
    if visible_git(&canonical.to_string_lossy()) {
        return Err(refused(
            "harness-git-visible",
            format!(
                "{} would make the git directory visible",
                canonical.display()
            ),
        ));
    }
    Ok(canonical)
}

fn refused(code: &'static str, reason: impl Into<String>) -> TransportError {
    TransportError::Refused {
        code,
        reason: reason.into(),
    }
}

// ---------------------------------------------------------------------------
// P23.2 / P23.3 — the declaration is enforced, never trusted
// ---------------------------------------------------------------------------

/// The containment a harness launch may run under, resolved from the
/// declaration AND the platform's own proof. A declaration alone never selects
/// a policy: an undeclared package is contained like any other, and on macOS an
/// undeclared package is refused outright.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessChildPolicy {
    /// macOS, P23.2: the declared no-child manifest, the exact executable it
    /// will run, and the P18 `NoWorkspaceSingleProcess` profile all agree, and
    /// the safety report behind that profile evaluates Activated. One initial
    /// exec, fork denied, every non-approved exec denied.
    SingleProcessNoFork {
        profile: &'static str,
        report_id: String,
    },
    /// Windows, P23.3: the child is created INSIDE a kill-on-close job — the
    /// kernel assigns it in `CreateProcessW` itself — so the child and every
    /// grandchild it creates are job members from their first instruction and
    /// die with the handle that owns the tree.
    ContainedJob { containment: &'static str },
    /// Linux, P23.3: the child execs as its own process-group leader (the group
    /// is set at exec, so there is no window) and the whole group is killed and
    /// re-verified on cancel, crash recovery and close.
    ContainedProcessGroup,
}

impl HarnessChildPolicy {
    /// Resolve the policy for one frozen plan, refusing closed when the platform
    /// cannot prove the containment the declaration asks to run under.
    pub fn resolve(
        plan: &HarnessSpawnPlan,
        config: &HarnessLaunchConfig,
    ) -> Result<Self, TransportError> {
        // The harness runs at the non-workspace effect class the shared
        // interactive ceiling already admits (P22): width is decided in that one
        // place, so the transport asks it instead of writing its own rule.
        ProcessProfileEffect::NoWorkspace
            .interactive_process_admitted()
            .then_some(())
            .ok_or_else(|| {
                refused(
                    "harness-effect-above-interactive-ceiling",
                    "the non-workspace effect class is above the interactive ceiling".to_string(),
                )
            })?;
        #[cfg(target_os = "macos")]
        return macos_policy(plan, config);
        #[cfg(not(target_os = "macos"))]
        let _ = &config;
        #[cfg(windows)]
        return windows_policy(plan);
        #[cfg(target_os = "linux")]
        return linux_policy(plan);
        #[allow(unreachable_code)]
        Err(refused(
            "harness-platform-unsupported",
            "this host platform cannot host a harness process".to_string(),
        ))
    }

    /// Stable machine-checkable code of the policy a launch runs under.
    pub fn code(&self) -> &'static str {
        match self {
            Self::SingleProcessNoFork { .. } => "macos-no-workspace-single-process",
            Self::ContainedJob { containment } => containment,
            Self::ContainedProcessGroup => "linux-contained-process-group",
        }
    }
}

#[cfg(target_os = "macos")]
fn macos_policy(
    plan: &HarnessSpawnPlan,
    config: &HarnessLaunchConfig,
) -> Result<HarnessChildPolicy, TransportError> {
    use crate::services::sandbox::macos::{MacosSeatbeltBackend, SeatbeltProfileClass};
    // Silence is not a claim of innocence: an undeclared third-party harness
    // stays SafeDisabled on macOS. It is never presented as Activated, and the
    // refusal names the platform, not the package, as the reason.
    if plan.requirement != ChildProcessRequirement::SingleProcess {
        return Err(refused(
            "harness-macos-undeclared-harness-safe-disabled",
            "a macOS harness that has not declared its child processes cannot run: no \
             no-fork/no-exec profile can be built for a package that may exec another image, \
             so the capability stays SafeDisabled"
                .to_string(),
        ));
    }
    // Exact executable/runtime policy: the literal deny-default SBPL for THIS
    // absolute harness path, or no policy at all.
    let harness_path = plan.executable.to_string_lossy().to_string();
    let profile = MacosSeatbeltBackend::new()
        .build_profile(&harness_path)
        .map_err(|error| {
            refused(
                "harness-macos-profile-refuses",
                format!("no-workspace single-process profile is unavailable: {error}"),
            )
        })?;
    match &config.activation {
        SafetyActivation::Activated { report_id } => Ok(HarnessChildPolicy::SingleProcessNoFork {
            profile: match SeatbeltProfileClass::NoWorkspaceSingleProcess {
                SeatbeltProfileClass::NoWorkspaceSingleProcess => {
                    "seatbelt:no-workspace-single-process"
                }
            },
            report_id: report_id.clone(),
        }),
        SafetyActivation::NotActivated { reason, status } => Err(refused(
            "harness-macos-single-process-unproven",
            format!(
                "the no-fork/no-exec profile is built ({} SBPL bytes) but its safety report \
                 is not activated: {reason} (status {})",
                profile.len(),
                status.clone().unwrap_or_else(|| "unknown".to_string())
            ),
        )),
    }
}

#[cfg(windows)]
fn windows_policy(_plan: &HarnessSpawnPlan) -> Result<HarnessChildPolicy, TransportError> {
    // The no-fork/no-exec denial is a Seatbelt property and does not exist here;
    // what Windows CAN prove is that the kernel put the child's tree in a
    // kill-on-close job as part of creating it, which is what an undeclared
    // package needs and the containment a declared single process runs inside.
    // The declaration never selects this on its own and is never trusted: the
    // job membership is read back from the child before it resumes, and a
    // creation that does not carry it is refused with nothing left running.
    Ok(HarnessChildPolicy::ContainedJob {
        containment: WINDOWS_CONTAINMENT,
    })
}

/// Machine-checkable code of the Windows containment: a per-launch kill-on-close
/// job the kernel assigns inside `CreateProcessW`, so membership precedes the
/// child's first instruction instead of being asserted after it.
#[cfg(windows)]
pub const WINDOWS_CONTAINMENT: &str = "windows-creation-time-kill-on-close-job";

#[cfg(target_os = "linux")]
fn linux_policy(_plan: &HarnessSpawnPlan) -> Result<HarnessChildPolicy, TransportError> {
    Ok(HarnessChildPolicy::ContainedProcessGroup)
}

// ---------------------------------------------------------------------------
// P23.3 Windows — created inside the job, resumed only after the record
//
// `process_guard::windows::spawn_suspended_with_job` puts
// `PROC_THREAD_ATTRIBUTE_JOB_LIST` in the same `STARTUPINFOEXW` attribute list
// as the stdio handle list, so the kernel assigns the kill-on-close job as part
// of `CreateProcessW`: membership is a property of creation, and reading it back
// confirms a containment that already holds. The order is the supervisor's own —
// prepare, spawn suspended, persist identity, resume — and one blocking thread
// owns the tree afterwards, because the guardian's stdio and teardown calls take
// `&mut self` and only one owner may touch a pipe.
// ---------------------------------------------------------------------------

/// Cadence of the tree thread while neither the pipes nor a command is moving.
#[cfg(windows)]
const TREE_POLL_INTERVAL: Duration = Duration::from_millis(1);

/// What the host asks the thread owning one contained tree to do. A command
/// the tree thread never answers is an unproven tree, never an assumed sweep.
#[cfg(windows)]
enum TreeCommand {
    /// Sweep the job and report the proof with the primary's exit code.
    Terminate { reply: oneshot::Sender<TreeOutcome> },
}

/// A tree command's answer: whether every job member is proven gone and the
/// exit code captured when the primary was settled.
#[cfg(windows)]
#[derive(Debug, Clone, Copy)]
struct TreeOutcome {
    swept: bool,
    exit: Option<i32>,
}

#[cfg(windows)]
impl TreeOutcome {
    /// What a caller gets when the tree thread went away before answering:
    /// nothing proven, so the record must be quarantined.
    fn unproven() -> Self {
        Self {
            swept: false,
            exit: None,
        }
    }
}

/// The tree thread's final word, recorded when the child is proven gone and
/// read by whoever asks about the tree after that (a run that finished on its
/// own is still a tree whose sweep has to be provable).
#[cfg(windows)]
type TreeOutcomeRecord = Arc<std::sync::Mutex<Option<TreeOutcome>>>;

/// Create the child inside its own kill-on-close job and leave it suspended:
/// nothing has executed, and nothing will until [`drive_tree`] resumes it.
#[cfg(windows)]
async fn create_contained(plan: &HarnessSpawnPlan) -> Result<JobbedSuspendedChild, TransportError> {
    let plan = plan.clone();
    tokio::task::spawn_blocking(move || {
        let spec = RawSpawnSpec {
            executable: &plan.executable,
            arguments: &plan.arguments,
            cwd: Some(&plan.cwd),
            environment: &plan.environment,
        };
        spawn_suspended_with_job(&spec).map_err(containment_refusal)
    })
    .await
    .map_err(|error| refused("harness-windows-tree-thread-lost", error.to_string()))?
}

/// The guardian's refusal vocabulary, kept honest: an environment that will not
/// host the creation-time job refuses the launch outright. Running the child in
/// a job nobody owns, or in no job at all, is never a fallback.
#[cfg(windows)]
fn containment_refusal(error: GuardianError) -> TransportError {
    match error {
        GuardianError::Unsupported(reason) => refused(
            "harness-windows-job-unsupported",
            format!("this environment refuses the creation-time job: {reason}"),
        ),
        other => refused("harness-windows-job-create-failed", other.to_string()),
    }
}

/// The launch-time confirmation: the kernel really did assign the job. The
/// child is still suspended here, so a `false` means it has not executed a
/// single instruction and the caller sweeps a never-started process.
#[cfg(windows)]
fn prove_created_contained(child: &JobbedSuspendedChild) -> bool {
    child.is_in_job()
}

/// Sweep the whole job and prove the member list empty, then report the
/// primary's exit code. `terminate` is `TerminateJobObject`, so the child and
/// every grandchild it created go together; `wait_all_members` fences each
/// listed pid with its start identity, so a recycled pid cannot masquerade as
/// a survivor and cannot be mistaken for a member either.
#[cfg(windows)]
fn sweep_and_settle(child: &mut JobbedSuspendedChild) -> TreeOutcome {
    child.terminate();
    let swept = child.wait_all_members(std::time::Instant::now() + TREE_PROOF_DEADLINE);
    TreeOutcome {
        swept,
        exit: child
            .settled_exit_code()
            .and_then(|code| i32::try_from(code).ok()),
    }
}

/// Prove a naturally-exited tree gone: settle the primary so the kernel can
/// retire it from the job, then wait for the member list to empty. An exit with
/// grandchildren still listed is `swept: false` and quarantines the record.
#[cfg(windows)]
fn settle_natural_exit(child: &mut JobbedSuspendedChild) -> TreeOutcome {
    child.settle_primary();
    let swept = child.wait_all_members(std::time::Instant::now() + TREE_PROOF_DEADLINE);
    TreeOutcome {
        swept,
        exit: child
            .settled_exit_code()
            .and_then(|code| i32::try_from(code).ok()),
    }
}

/// Everything the tree thread multiplexes besides the child itself: the frames
/// it writes, the two streams it pumps, the commands it answers and the
/// transport state it owns. One struct, because one loop may hold the only
/// mutable borrow of the child.
#[cfg(windows)]
struct TreeDrive {
    outbound: mpsc::UnboundedReceiver<Vec<u8>>,
    stdout: Option<mpsc::UnboundedSender<Vec<u8>>>,
    stderr: Option<mpsc::UnboundedSender<Vec<u8>>>,
    control: mpsc::UnboundedReceiver<TreeCommand>,
    outcome: TreeOutcomeRecord,
    queue_bytes: Arc<AtomicUsize>,
    alive: Arc<AtomicBool>,
    terminate: Arc<AtomicBool>,
    /// Whether this thread still holds the parent's stdin end open: dropping
    /// it is how the transport asks a plugin to stop.
    stdin_open: bool,
}

/// Start the thread that owns the tree: it resumes the suspended child, then
/// serves it until the tree is gone. The caller must only run this once the
/// durable identity and `Running` have landed, because the resume is the
/// child's first instruction, and the acknowledgement it sends back is what
/// tells the launch that the child is actually live.
#[cfg(windows)]
fn run_tree_thread(
    mut child: JobbedSuspendedChild,
    mut drive: TreeDrive,
    resume: oneshot::Sender<Result<(), String>>,
) {
    match child.resume_once() {
        Ok(()) => {
            if resume.send(Ok(())).is_err() {
                record_outcome(&drive.outcome, sweep_and_settle(&mut child));
                return;
            }
            pump_tree(&mut child, &mut drive);
        }
        // Nothing executed: sweeping a never-started child is the whole
        // teardown, and the refusal names a launch that never began.
        Err(error) => {
            let _ = resume.send(Err(error.to_string()));
            record_outcome(&drive.outcome, sweep_and_settle(&mut child));
        }
    }
    drive.alive.store(false, Ordering::SeqCst);
}

/// The tree thread's loop: answer a sweep, drain both output streams, write one
/// queued frame, and conclude the tree once the child is unreachable. The
/// streams are drained BEFORE every write, so a plugin that writes while it
/// reads never finds both pipes full at one instant. The output senders drop on
/// the way out, which is the EOF both transport tasks wait for.
#[cfg(windows)]
fn pump_tree(child: &mut JobbedSuspendedChild, drive: &mut TreeDrive) {
    loop {
        if let Ok(TreeCommand::Terminate { reply }) = drive.control.try_recv() {
            let outcome = sweep_and_settle(child);
            record_outcome(&drive.outcome, outcome);
            let _ = reply.send(outcome);
            return;
        }
        let mut moving = pump_streams(child, drive);
        match write_queued_frame(child, drive) {
            FrameWrite::Done => moving = true,
            FrameWrite::Idle | FrameWrite::Blocked => {}
            FrameWrite::Dead => break,
        }
        // Both streams at EOF means this run will never hear from the child
        // again; a closed job takes the tree with it, which is the sweep.
        if drive.stdout.is_none() && drive.stderr.is_none() {
            break;
        }
        if !moving {
            if child.primary_exited() {
                continue;
            }
            std::thread::sleep(TREE_POLL_INTERVAL);
        }
    }
    let outcome = if child.primary_exited() {
        settle_natural_exit(child)
    } else {
        // Unreachable and still running: that is a sweep, and the sweep is what
        // has to be proven before the record may settle.
        sweep_and_settle(child)
    };
    record_outcome(&drive.outcome, outcome);
}

/// What one turn of the write arm accomplished.
#[cfg(windows)]
enum FrameWrite {
    /// A frame reached the pipe.
    Done,
    /// Nothing was queued.
    Idle,
    /// A write broke because a sweep asked for exactly that.
    Blocked,
    /// The child's stdin is gone: the tree is on its way out.
    Dead,
}

/// Move one queued frame into the child's stdin. The queue is only credited back
/// once the bytes reached the pipe, so a plugin that never reads still trips the
/// host's bounded-queue refusal instead of hiding pending bytes.
#[cfg(windows)]
fn write_queued_frame(child: &mut JobbedSuspendedChild, drive: &mut TreeDrive) -> FrameWrite {
    let terminate = drive.terminate.clone();
    let Ok(frame) = drive.outbound.try_recv() else {
        if drive.stdin_open && drive.outbound.is_closed() {
            child.close_stdin();
            drive.stdin_open = false;
        }
        return FrameWrite::Idle;
    };
    if write_frame(child, &frame, &terminate) {
        drive.queue_bytes.fetch_sub(frame.len(), Ordering::SeqCst);
        return FrameWrite::Done;
    }
    drive.alive.store(false, Ordering::SeqCst);
    if terminate.load(Ordering::SeqCst) {
        return FrameWrite::Blocked;
    }
    FrameWrite::Dead
}

/// The blocking pipe write, run on a scoped worker so this thread can still
/// answer a sweep request while the child is not reading: a pending terminate
/// terminates the primary, which breaks the pipe and returns the write. Without
/// that, a plugin that never reads its stdin could wedge the only thread that
/// owns its tree.
#[cfg(windows)]
fn write_frame(child: &mut JobbedSuspendedChild, frame: &[u8], terminate: &AtomicBool) -> bool {
    let pid = child.pid();
    std::thread::scope(|scoped| {
        let writer = scoped.spawn(|| {
            let mut written = 0usize;
            while written < frame.len() {
                match child.write_stdin(&frame[written..]) {
                    Ok(0) | Err(_) => return false,
                    Ok(more) => written += more,
                }
            }
            true
        });
        loop {
            if writer.is_finished() {
                break;
            }
            if terminate.load(Ordering::SeqCst) {
                terminate_primary(pid);
            }
            std::thread::sleep(TREE_POLL_INTERVAL);
        }
        writer.join().unwrap_or(false)
    })
}

/// Terminate one process by pid: only ever the primary of a tree this thread
/// owns, and only to break a write that a sweep is already waiting on. The job
/// sweep, not this call, is what proves the tree gone.
#[cfg(windows)]
fn terminate_primary(pid: u32) {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
    unsafe {
        let process = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if process.is_null() {
            return;
        }
        TerminateProcess(process, 1);
        CloseHandle(process);
    }
}

/// Peek both output streams and hand whatever is buffered to the transport. A
/// closed stream takes the sender away, which is the EOF the reader treats as
/// the plugin's stdout going quiet.
#[cfg(windows)]
fn pump_streams(child: &JobbedSuspendedChild, drive: &mut TreeDrive) -> bool {
    let stdout = pump_stream(child.pump_stdout(), &mut drive.stdout);
    let stderr = pump_stream(child.pump_stderr(), &mut drive.stderr);
    stdout || stderr
}

#[cfg(windows)]
fn pump_stream(read: PipeRead, sender: &mut Option<mpsc::UnboundedSender<Vec<u8>>>) -> bool {
    let PipeRead::Data(chunk) = read else {
        *sender = None;
        return false;
    };
    if chunk.is_empty() {
        return false;
    }
    if sender.as_ref().is_none_or(|sink| sink.send(chunk).is_err()) {
        *sender = None;
    }
    true
}

#[cfg(windows)]
fn record_outcome(record: &TreeOutcomeRecord, outcome: TreeOutcome) {
    *record
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(outcome);
}

/// What the tree thread has concluded about its tree, if anything yet.
#[cfg(windows)]
fn recorded_outcome(record: &TreeOutcomeRecord) -> Option<TreeOutcome> {
    *record
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A stdout/stderr source the tree thread feeds: the transport reads the chunks
/// it observed rather than waiting on a pipe handle, which only the thread that
/// owns the child may touch. Dropping the sender is the child's EOF.
#[cfg(windows)]
struct ChunkStream {
    pending: Vec<u8>,
    received: mpsc::UnboundedReceiver<Vec<u8>>,
}

#[cfg(windows)]
impl ChunkStream {
    fn new(received: mpsc::UnboundedReceiver<Vec<u8>>) -> Self {
        Self {
            pending: Vec::new(),
            received,
        }
    }
}

#[cfg(windows)]
impl tokio::io::AsyncBufRead for ChunkStream {
    fn poll_fill_buf(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<&[u8]>> {
        let me = self.get_mut();
        loop {
            if !me.pending.is_empty() {
                return std::task::Poll::Ready(Ok(me.pending.as_slice()));
            }
            match me.received.poll_recv(cx) {
                std::task::Poll::Ready(Some(chunk)) => me.pending = chunk,
                std::task::Poll::Ready(None) => return std::task::Poll::Ready(Ok(&[])),
                std::task::Poll::Pending => return std::task::Poll::Pending,
            }
        }
    }

    fn consume(self: std::pin::Pin<&mut Self>, amount: usize) {
        self.get_mut().pending.drain(..amount);
    }
}

#[cfg(windows)]
impl tokio::io::AsyncRead for ChunkStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let me = self.get_mut();
        loop {
            if !me.pending.is_empty() {
                let take = me.pending.len().min(buf.remaining());
                buf.put_slice(&me.pending[..take]);
                me.pending.drain(..take);
                return std::task::Poll::Ready(Ok(()));
            }
            if buf.remaining() == 0 {
                return std::task::Poll::Ready(Ok(()));
            }
            match me.received.poll_recv(cx) {
                std::task::Poll::Ready(Some(chunk)) => me.pending = chunk,
                std::task::Poll::Ready(None) => return std::task::Poll::Ready(Ok(())),
                std::task::Poll::Pending => return std::task::Poll::Pending,
            }
        }
    }
}

/// Linux containment: the child leads its own process group, so group membership
/// is the proof that it was created contained, and an empty group is the proof
/// that the tree is gone.
#[cfg(target_os = "linux")]
fn prove_contained(_policy: &HarnessChildPolicy, pid: u32) -> bool {
    stat_field(pid, PGID_FIELD) == Some(pid as i32)
}

/// Kill the child's whole process group, then prove the group is empty. A member
/// that lingers past the proof deadline quarantines the record; it is never
/// reported as swept.
#[cfg(target_os = "linux")]
fn sweep_tree(_policy: &HarnessChildPolicy, pid: u32) -> bool {
    let group = pid as i32;
    if stat_field(pid, PGID_FIELD) != Some(group) {
        // Not the leader we promised to contain: signalling an unowned group
        // could hit processes that were never part of this tree.
        return false;
    }
    unsafe {
        libc::killpg(group, libc::SIGKILL);
    }
    let deadline = std::time::Instant::now() + TREE_PROOF_DEADLINE;
    loop {
        if process_group_members(group).is_empty() {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// `/proc/<pid>/stat`: field 5 is the process group id.
#[cfg(target_os = "linux")]
const PGID_FIELD: usize = 5;

/// One whitespace field of `/proc/<pid>/stat`, counted after the closing paren
/// so a comm containing spaces cannot shift the columns.
#[cfg(target_os = "linux")]
fn stat_field(pid: u32, field: usize) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let tail = stat.rsplit(')').next()?;
    // The tail opens with field 3 (state), so field N sits at index N - 3.
    tail.split_whitespace()
        .nth(field.checked_sub(3)?)
        .and_then(|value| value.parse().ok())
}

/// Every live (never zombie) member of one process group.
#[cfg(target_os = "linux")]
fn process_group_members(group: i32) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<u32>().ok())
        .filter(|pid| stat_field(*pid, PGID_FIELD) == Some(group))
        .filter(|pid| {
            // State is field 3, the first token of the tail.
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
            stat.rsplit(')')
                .next()
                .and_then(|tail| tail.split_whitespace().next())
                .is_some_and(|state| state != "Z")
        })
        .collect()
}

/// Every other platform refuses a launch it cannot contain: macOS is handled by
/// its own policy arm, and this is the compile-only shape for hosts the harness
/// does not target at all.
#[cfg(not(any(windows, target_os = "linux")))]
fn prove_contained(_policy: &HarnessChildPolicy, _pid: u32) -> bool {
    false
}

#[cfg(not(any(windows, target_os = "linux")))]
fn sweep_tree(_policy: &HarnessChildPolicy, _pid: u32) -> bool {
    false
}

/// Callbacks the plugin may invoke while the host is waiting.
#[async_trait::async_trait]
pub trait PluginCallbacks: Send + Sync {
    async fn handle_request(&self, request: RpcRequest) -> Result<serde_json::Value, RpcError> {
        Err(RpcError::method_not_found(&request.method))
    }

    async fn handle_notification(&self, _notification: RpcNotification) {}
}

/// Default callbacks: every request fails closed with method-not-found.
#[derive(Default)]
pub struct DenyCallbacks;

#[async_trait::async_trait]
impl PluginCallbacks for DenyCallbacks {}

/// How long a tree proof may run before the record is quarantined rather than
/// accepted as terminal.
pub const TREE_PROOF_DEADLINE: Duration = Duration::from_secs(5);

/// Bounded patience for reading the tree thread's conclusion from the async
/// side: the thread records the moment the child is proven gone, so this only
/// covers a caller that arrives while that proof is still being assembled. Past
/// it nothing is assumed — the record quarantines.
#[cfg(windows)]
const RECORD_POLL_INTERVAL: Duration = Duration::from_millis(20);

#[cfg(windows)]
const RECORD_POLL_ROUNDS: u32 = 350;

/// Sequence source for harness tree identities. The identity is issued BEFORE
/// the child exists, so a record always names the operation it was written for.
static NEXT_HARNESS_TREE: AtomicU64 = AtomicU64::new(1);

/// The durable supervisor record of one harness tree (P23.1). The record is
/// prepared before the child exists, carries the owner identity before the
/// transport sends a single frame, and names the containment that was proven.
/// Phases are exactly the supervisor's own vocabulary — no parallel state
/// machine is invented for the harness.
pub struct SupervisedTree {
    operation_id: String,
    tree_id: String,
    pid: u32,
    policy_code: &'static str,
    journal: Arc<dyn SupervisorJournal>,
    record: std::sync::Mutex<SupervisorRecord>,
}

impl SupervisedTree {
    /// Write the Prepared record for one already-validated plan. A failure here
    /// happens while no process exists, which is the point of the ordering.
    fn prepare(
        config: &HarnessLaunchConfig,
        plan: &HarnessSpawnPlan,
        policy: &HarnessChildPolicy,
    ) -> Result<Self, TransportError> {
        let sequence = NEXT_HARNESS_TREE.fetch_add(1, Ordering::SeqCst);
        let tree_id = format!("tree-harness-{sequence}");
        let name = plan
            .executable
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "plugin".to_string());
        let record = SupervisorRecord {
            operation_id: format!("harness-{tree_id}"),
            tree_id: tree_id.clone(),
            task_id: format!("harness:{name}"),
            ownership_epoch: 1,
            content_digest: r_code_harness_protocol::canonical_input_hash(&serde_json::json!({
                "executable": canonical(&plan.executable),
                "arguments": plan.arguments,
                "cwd": canonical(&plan.cwd),
                "environmentKeys": plan.environment.keys().collect::<Vec<_>>(),
                "network": plan.network,
                "requirement": format!("{:?}", plan.requirement),
                "policy": policy.code(),
            })),
            phase: SupervisorPhase::Prepared,
            revision: 1,
            output_cursor: 0,
            owner: None,
            proof: None,
            output_tail: None,
            quarantine_reason: None,
            write_disabled: None,
        };
        let journal = config.journal.clone();
        let disposition = journal
            .prepare(record.clone())
            .map_err(|error| refused("harness-journal-prepare-failed", error.to_string()))?;
        let stored = match disposition {
            PrepareDisposition::Created(record) => record,
            // A replayed identity means a record already owns this operation id;
            // reusing it would let two trees share one durable history.
            PrepareDisposition::Existing(_) => {
                return Err(refused(
                    "harness-journal-identity-conflict",
                    format!("operation for {tree_id} is already journaled"),
                ))
            }
        };
        Ok(Self {
            operation_id: stored.operation_id.clone(),
            tree_id,
            pid: 0,
            policy_code: policy.code(),
            journal,
            record: std::sync::Mutex::new(stored),
        })
    }

    /// Persist the child's owner identity and the pid every later sweep of this
    /// tree is issued against. For a harness the phases carry the supervisor's
    /// own meaning: `Suspended` is "the child exists inside the containment
    /// proven at creation and has been served nothing", `IdentityRecorded` is the
    /// durable owner, and the transport sends its first byte only after
    /// `Running` landed. A crash after it leaves a recoverable record and a crash
    /// before it leaves a Prepared one — never an unmanaged process.
    fn record_identity(&mut self, pid: u32, start_identity: u64) -> Result<(), TransportError> {
        if pid == 0 {
            return Err(refused(
                "harness-identity-unavailable",
                "the child did not report a pid".to_string(),
            ));
        }
        let boot = BootIdentity::current().map_err(|_| {
            refused(
                "harness-identity-unavailable",
                "boot identity is unavailable",
            )
        })?;
        let owner = ProcessOwnerIdentity::new(
            pid,
            start_identity,
            boot,
            serde_json::json!({
                "native": "harness-transport",
                "policy": self.policy_code,
                "pid": pid,
                "startIdentity": start_identity,
            }),
        )
        .map_err(|reason| refused("harness-identity-unavailable", reason.to_string()))?;
        self.pid = pid;
        self.advance(SupervisorPhase::Suspended, |_| {})?;
        self.advance(SupervisorPhase::IdentityRecorded, |record| {
            record.owner = Some(owner);
        })
    }

    /// The last durable steps before the first outbound frame.
    fn mark_running(&self) -> Result<(), TransportError> {
        self.advance(SupervisorPhase::ResumePending, |_| {})?;
        self.advance(SupervisorPhase::Running, |_| {})
    }

    /// Record how the tree ended: a proven sweep drains the operation, anything
    /// else quarantines it for recovery (P23.3). Idempotent — a repeat close can
    /// never downgrade a record that already carries its outcome.
    fn settle(&self, proven: bool) -> SupervisorPhase {
        let current = self.read_phase();
        if !matches!(
            current,
            SupervisorPhase::Running | SupervisorPhase::Draining
        ) {
            return current;
        }
        if !proven {
            let outcome = self.advance(SupervisorPhase::Quarantined, |record| {
                record.quarantine_reason = Some(format!("tree-{}-sweep-unproven", record.tree_id));
            });
            return match outcome {
                Ok(()) => SupervisorPhase::Quarantined,
                Err(_) => self.read_phase(),
            };
        }
        if self.advance(SupervisorPhase::Draining, |_| {}).is_err() {
            return self.read_phase();
        }
        match self.advance(SupervisorPhase::Drained, |_| {}) {
            Ok(()) => SupervisorPhase::Drained,
            Err(_) => self.read_phase(),
        }
    }

    fn advance(
        &self,
        phase: SupervisorPhase,
        amend: impl FnOnce(&mut SupervisorRecord),
    ) -> Result<(), TransportError> {
        let mut guard = self
            .record
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut next = guard.clone();
        next.phase = phase;
        next.revision += 1;
        amend(&mut next);
        let stored = self
            .journal
            .compare_and_swap(&guard, next)
            .map_err(|error| refused("harness-journal-write-failed", error.to_string()))?;
        *guard = stored;
        Ok(())
    }

    fn read_phase(&self) -> SupervisorPhase {
        self.record
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .phase
    }

    /// The durable operation identity this tree was launched under.
    pub fn operation_id(&self) -> &str {
        &self.operation_id
    }

    /// The tree this process's descendants must die with.
    pub fn tree_id(&self) -> &str {
        &self.tree_id
    }

    /// Machine-checkable code of the containment policy in force.
    pub fn policy_code(&self) -> &'static str {
        self.policy_code
    }

    /// The pid the containment was proven against.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The current durable phase of the operation record.
    pub fn phase(&self) -> SupervisorPhase {
        self.read_phase()
    }
}

/// The start-time identity that fences a pid against reuse. Windows takes it
/// from the guardian's own child record, so the identity the launch persists is
/// the same tuple recovery verifies.
#[cfg(target_os = "linux")]
fn child_start_identity(pid: u32) -> u64 {
    // Field 22 of /proc/<pid>/stat is the process start time (ticks since boot),
    // the same value the unix guardian uses to fence a reused pid.
    u64::try_from(stat_field(pid, 22).unwrap_or(0)).unwrap_or(0)
}

#[cfg(not(any(windows, target_os = "linux")))]
fn child_start_identity(_pid: u32) -> u64 {
    0
}

/// One spawned plugin process with its transport machinery. The child was
/// contained from its first instruction (P23.3): Windows hands it to the thread
/// that created it inside a kill-on-close job, Unix to the process-group leader
/// whose group was set at exec. Every teardown sweeps the TREE, never only the
/// direct child, and the record says whether that sweep was proven.
pub struct PluginProcess {
    /// What the transport may still ask of the child's tree.
    tree_handle: TreeHandle,
    tree: Arc<SupervisedTree>,
    policy: HarnessChildPolicy,
    outbound: mpsc::UnboundedSender<Vec<u8>>,
    queue_bytes: Arc<AtomicUsize>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<PendingReply>>>>,
    next_id: AtomicU64,
    limits: TransportLimits,
    stderr_tail: Arc<Mutex<Vec<u8>>>,
    alive: Arc<AtomicBool>,
}

/// What a pending call resolves to.
enum PendingReply {
    Reply(Result<serde_json::Value, RpcError>),
    Fault(String),
}

/// The platform split for one contained child: everything needed to sweep it,
/// kept apart from the stdio the transport's tasks consume.
struct TreeHandle {
    /// Windows: the thread that owns the tree answers tree commands.
    #[cfg(windows)]
    control: mpsc::UnboundedSender<TreeCommand>,
    /// Windows: set before a sweep so a stdin write that cannot complete (the
    /// plugin is not reading) is broken rather than wedging the tree thread.
    #[cfg(windows)]
    terminate: Arc<AtomicBool>,
    /// Windows: what the tree thread concluded when the child finally went.
    #[cfg(windows)]
    outcome: TreeOutcomeRecord,
    #[cfg(not(windows))]
    child: Mutex<Child>,
}

/// The two output streams and the child's stdin, per platform. Windows streams
/// arrive as the chunks the tree thread peeked off the pipes; Unix gets the
/// tokio pipes it already had.
#[cfg(windows)]
struct PluginStdio {
    stdout: ChunkStream,
    stderr: ChunkStream,
}

#[cfg(not(windows))]
struct PluginStdio {
    stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    /// The frame queue the writer task drains: on Unix the transport holds the
    /// child's stdin directly, on Windows the tree thread does.
    outbound: mpsc::UnboundedReceiver<Vec<u8>>,
}

/// The state every task of one transport shares: the frame queue, its byte
/// accounting, the bounded stderr tail, liveness and the pending-call table.
#[derive(Clone)]
struct TransportState {
    outbound: mpsc::UnboundedSender<Vec<u8>>,
    queue_bytes: Arc<AtomicUsize>,
    stderr_tail: Arc<Mutex<Vec<u8>>>,
    alive: Arc<AtomicBool>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<PendingReply>>>>,
}

/// Spawn a plugin: manifest-declared executable + argv, hidden Windows console.
///
/// The child is never created directly. The launch first freezes and validates
/// its material, resolves the containment the declaration may run under, writes
/// the Prepared record, and only then creates the process — inside that
/// containment, with the environment cleared and the offline/no-workspace cwd
/// the plan names. On Windows the child is created suspended and resumed only
/// after `Running` landed. Every failure path returns before a frame is sent.
pub async fn spawn_plugin(
    executable: &std::path::Path,
    argv: &[String],
    callbacks: Arc<dyn PluginCallbacks>,
    limits: TransportLimits,
) -> Result<PluginProcess, TransportError> {
    let config = launch_config();
    let plan = HarnessSpawnPlan::build(executable, argv, &config)?;
    let policy = HarnessChildPolicy::resolve(&plan, &config)?;
    let mut tree = SupervisedTree::prepare(&config, &plan, &policy)?;
    let (outbound_tx, outbound_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let state = TransportState {
        outbound: outbound_tx.clone(),
        queue_bytes: Arc::new(AtomicUsize::new(0)),
        stderr_tail: Arc::new(Mutex::new(Vec::new())),
        alive: Arc::new(AtomicBool::new(true)),
        pending: Arc::new(Mutex::new(HashMap::new())),
    };
    let (tree_handle, stdio) =
        launch_contained(&plan, &policy, &mut tree, &state, outbound_rx).await?;
    serve_stdio(stdio, &state, callbacks, limits.max_frame_bytes);
    Ok(PluginProcess {
        tree_handle,
        tree: Arc::new(tree),
        policy,
        outbound: outbound_tx,
        queue_bytes: state.queue_bytes,
        pending: state.pending,
        next_id: AtomicU64::new(1),
        limits,
        stderr_tail: state.stderr_tail,
        alive: state.alive,
    })
}

/// Create the child INSIDE the containment the policy names, record it durably,
/// and only then let it run: Windows resumes a jobbed suspended child, Unix
/// proves the process-group leadership that exec already established. The
/// environment is cleared and rebuilt from the plan's allowlist, so no provider
/// secret can reach the child even if the daemon carries one.
#[cfg(windows)]
async fn launch_contained(
    plan: &HarnessSpawnPlan,
    policy: &HarnessChildPolicy,
    tree: &mut SupervisedTree,
    state: &TransportState,
    outbound: mpsc::UnboundedReceiver<Vec<u8>>,
) -> Result<(TreeHandle, PluginStdio), TransportError> {
    let child = create_contained(plan).await?;
    let pid = child.pid();
    if !prove_created_contained(&child) {
        // The kernel did not give the child the containment this policy names.
        // It has executed nothing, it is swept here, and the launch is refused.
        let outcome = tokio::task::spawn_blocking(move || {
            let mut child = child;
            sweep_and_settle(&mut child)
        })
        .await
        .unwrap_or_else(|_| TreeOutcome::unproven());
        tree.settle(outcome.swept);
        return Err(refused(
            "harness-containment-unproven",
            format!(
                "pid {pid} was not created inside the {} containment",
                policy.code()
            ),
        ));
    }
    // Identity and Running land while the child is still suspended: the record
    // names a tree that exists but has run nothing.
    let recorded = tree
        .record_identity(pid, child.start_identity())
        .and_then(|()| tree.mark_running());
    if let Err(error) = recorded {
        let outcome = tokio::task::spawn_blocking(move || {
            let mut child = child;
            sweep_and_settle(&mut child)
        })
        .await
        .unwrap_or_else(|_| TreeOutcome::unproven());
        tree.settle(outcome.swept);
        return Err(error);
    }
    let (stdout_tx, stdout_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (stderr_tx, stderr_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let (control_tx, control_rx) = mpsc::unbounded_channel::<TreeCommand>();
    let (resume_tx, resumed) = oneshot::channel::<Result<(), String>>();
    let terminate = Arc::new(AtomicBool::new(false));
    let outcome = Arc::new(std::sync::Mutex::new(None));
    let drive = TreeDrive {
        outbound,
        stdout: Some(stdout_tx),
        stderr: Some(stderr_tx),
        control: control_rx,
        outcome: outcome.clone(),
        queue_bytes: state.queue_bytes.clone(),
        alive: state.alive.clone(),
        terminate: terminate.clone(),
        stdin_open: true,
    };
    let child_thread = tokio::task::spawn_blocking(move || {
        run_tree_thread(child, drive, resume_tx);
    });
    match resumed.await {
        Ok(Ok(())) => {}
        // The resume refused, or the tree thread ended before answering: either
        // way it swept the tree it owned before going, and that sweep — of a
        // child that never executed a single instruction — settles the record.
        other => {
            let _ = child_thread.await;
            let swept = recorded_outcome(&outcome).is_some_and(|entry| entry.swept);
            tree.settle(swept);
            return Err(match other {
                Ok(Err(reason)) => refused(
                    "harness-windows-resume-refused",
                    format!("the contained child could not be resumed: {reason}"),
                ),
                _ => refused(
                    "harness-windows-tree-thread-lost",
                    "the thread owning the tree ended before resuming the child",
                ),
            });
        }
    }
    Ok((
        TreeHandle {
            control: control_tx,
            terminate,
            outcome,
        },
        PluginStdio {
            stdout: ChunkStream::new(stdout_rx),
            stderr: ChunkStream::new(stderr_rx),
        },
    ))
}

/// The Unix launch: `process_group(0)` makes the child its own group leader at
/// exec, so the group exists from its first instruction and one signal reaches
/// the whole tree. macOS has no proof of a no-fork/no-exec containment here, so
/// it refuses exactly like any launch whose containment cannot be established.
#[cfg(not(windows))]
async fn launch_contained(
    plan: &HarnessSpawnPlan,
    policy: &HarnessChildPolicy,
    tree: &mut SupervisedTree,
    _state: &TransportState,
    outbound: mpsc::UnboundedReceiver<Vec<u8>>,
) -> Result<(TreeHandle, PluginStdio), TransportError> {
    // macOS: the P23.2 policy resolved, but nothing on this host yet establishes
    // a containment before the child's first instruction — the P18 profile is
    // built but not applied at spawn. Refuse before creating anything, so no
    // harness here ever executes one uncontained instruction.
    #[cfg(target_os = "macos")]
    {
        if matches!(policy, HarnessChildPolicy::SingleProcessNoFork { .. }) {
            return Err(refused(
                "harness-macos-single-process-unproven",
                format!(
                    "no containment precedes the first instruction on this platform; the \
                     declared single-process launch of {} is refused before any process \
                     exists",
                    canonical(&plan.executable)
                ),
            ));
        }
    }
    let mut command = Command::new(&plan.executable);
    command.args(&plan.arguments);
    command.current_dir(&plan.cwd);
    command.env_clear();
    command.envs(&plan.environment);
    command.stdin(std::process::Stdio::piped());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    #[cfg(target_os = "linux")]
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|error| TransportError::Spawn(error.to_string()))?;
    let pid = child.id().unwrap_or(0);
    if !prove_contained(policy, pid) {
        // The containment this policy claims is not observably in force: sweep
        // and refuse rather than keep a child whose tree nobody owns.
        let swept = sweep_policy_tree(policy, pid).await;
        let _ = child.start_kill();
        tree.settle(swept && pid_is_gone(pid));
        return Err(refused(
            "harness-containment-unproven",
            format!(
                "pid {pid} is not provably inside the {} containment",
                policy.code()
            ),
        ));
    }
    if let Err(error) = tree
        .record_identity(pid, child_start_identity(pid))
        .and_then(|()| tree.mark_running())
    {
        let swept = sweep_policy_tree(policy, pid).await;
        let _ = child.start_kill();
        tree.settle(swept && pid_is_gone(pid));
        return Err(error);
    }
    // The stdio ends are taken before the child moves into the handle: the
    // transport owns the pipes, the handle owns the process.
    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    Ok((
        TreeHandle {
            child: Mutex::new(child),
        },
        PluginStdio {
            stdin,
            stdout,
            stderr,
            outbound,
        },
    ))
}

/// Wire the two tasks that speak the protocol: the frame reader (which keeps
/// serving nested host callbacks while the host waits) and the bounded stderr
/// tail. On Unix the writer task is added here, because the transport holds the
/// child's stdin directly; on Windows the tree thread drains that queue.
fn serve_stdio(
    stdio: PluginStdio,
    state: &TransportState,
    callbacks: Arc<dyn PluginCallbacks>,
    max_frame_bytes: usize,
) {
    #[cfg(not(windows))]
    {
        let queue_bytes = state.queue_bytes.clone();
        let alive = state.alive.clone();
        let (stdin, stdout, stderr, outbound) =
            (stdio.stdin, stdio.stdout, stdio.stderr, stdio.outbound);
        tokio::spawn(write_plugin_frames(stdin, outbound, queue_bytes, alive));
        let context = ReaderContext {
            callbacks,
            pending: state.pending.clone(),
            alive: state.alive.clone(),
            outbound: state.outbound.clone(),
            queue_bytes: state.queue_bytes.clone(),
            max_frame_bytes,
        };
        tokio::spawn(read_plugin_frames(BufReader::new(stdout), context));
        tokio::spawn(capture_stderr(
            BufReader::new(stderr),
            state.stderr_tail.clone(),
        ));
    }
    #[cfg(windows)]
    {
        let (stdout, stderr) = (stdio.stdout, stdio.stderr);
        let context = ReaderContext {
            callbacks,
            pending: state.pending.clone(),
            alive: state.alive.clone(),
            outbound: state.outbound.clone(),
            queue_bytes: state.queue_bytes.clone(),
            max_frame_bytes,
        };
        tokio::spawn(read_plugin_frames(BufReader::new(stdout), context));
        tokio::spawn(capture_stderr(
            BufReader::new(stderr),
            state.stderr_tail.clone(),
        ));
    }
}

/// Outbound writer: drains the frame queue with byte accounting. NDJSON frames
/// must reach the plugin immediately, so nothing lingers in the buffer.
#[cfg(not(windows))]
async fn write_plugin_frames(
    mut stdin: tokio::process::ChildStdin,
    mut outbound: mpsc::UnboundedReceiver<Vec<u8>>,
    queue_bytes: Arc<AtomicUsize>,
    alive: Arc<AtomicBool>,
) {
    let mut writer = tokio::io::BufWriter::new(&mut stdin);
    while let Some(frame) = outbound.recv().await {
        if writer.write_all(&frame).await.is_err() || writer.flush().await.is_err() {
            alive.store(false, Ordering::SeqCst);
            break;
        }
        queue_bytes.fetch_sub(frame.len(), Ordering::SeqCst);
    }
}

/// The frame reader's world: the callback surface it serves, the calls it
/// resolves and the queue it answers nested calls with.
struct ReaderContext {
    callbacks: Arc<dyn PluginCallbacks>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<PendingReply>>>>,
    alive: Arc<AtomicBool>,
    outbound: mpsc::UnboundedSender<Vec<u8>>,
    queue_bytes: Arc<AtomicUsize>,
    max_frame_bytes: usize,
}

/// Decode frames until the plugin goes quiet: resolve pending calls, serve
/// nested host callbacks, and fault the table on a malformed or oversized frame.
async fn read_plugin_frames<R: tokio::io::AsyncBufRead + Unpin>(
    mut source: BufReader<R>,
    context: ReaderContext,
) {
    let ReaderContext {
        callbacks,
        pending,
        alive,
        outbound,
        queue_bytes,
        max_frame_bytes,
    } = context;
    loop {
        match read_frame(&mut source, max_frame_bytes).await {
            Ok(Some(line)) => {
                let message = match decode_frame(&line) {
                    Ok(message) => message,
                    Err(error) => {
                        fault_pending(&pending, format!("malformed frame: {error}")).await;
                        alive.store(false, Ordering::SeqCst);
                        return;
                    }
                };
                match message {
                    RpcMessage::Response(response) => {
                        let key = id_key(&response.id);
                        let mut guard = pending.lock().await;
                        if let Some(sender) = guard.remove(&key) {
                            let result = if let Some(error) = response.error {
                                Err(error)
                            } else {
                                Ok(response.result.unwrap_or(serde_json::Value::Null))
                            };
                            let _ = sender.send(PendingReply::Reply(result));
                        }
                    }
                    RpcMessage::Request(request) => {
                        // Nested callback: the plugin calls the host while the
                        // host awaits its own response, and this reader serves.
                        let request_id = request.id.clone();
                        let outcome = callbacks.handle_request(request).await;
                        let response = match outcome {
                            Ok(result) => RpcResponse {
                                jsonrpc: "2.0".into(),
                                id: request_id,
                                result: Some(result),
                                error: None,
                            },
                            Err(error) => RpcResponse {
                                jsonrpc: "2.0".into(),
                                id: request_id,
                                result: None,
                                error: Some(error),
                            },
                        };
                        if let Ok(frame) = encode_frame(&RpcMessage::Response(response)) {
                            if frame.len() <= max_frame_bytes {
                                let queued = queue_bytes.fetch_add(frame.len(), Ordering::SeqCst);
                                if queued + frame.len() > MAX_QUEUE_BYTES {
                                    queue_bytes.fetch_sub(frame.len(), Ordering::SeqCst);
                                } else if outbound.send(frame).is_err() {
                                    alive.store(false, Ordering::SeqCst);
                                    return;
                                }
                            }
                        }
                    }
                    RpcMessage::Notification(notification) => {
                        callbacks.handle_notification(notification).await;
                    }
                }
            }
            // Clean EOF: resolve pending calls as closed.
            Ok(None) => {
                fault_pending(&pending, "plugin stdout closed".into()).await;
                alive.store(false, Ordering::SeqCst);
                return;
            }
            Err(error) => {
                fault_pending(&pending, format!("frame limit violated: {error}")).await;
                alive.store(false, Ordering::SeqCst);
                return;
            }
        }
    }
}

/// Bounded stderr capture: the tail keeps the last STDERR_TAIL_BYTES and nothing
/// more, so a flood cannot grow host memory.
async fn capture_stderr<R: tokio::io::AsyncBufRead + Unpin>(
    mut reader: BufReader<R>,
    tail: Arc<Mutex<Vec<u8>>>,
) {
    let mut chunk = [0u8; 4096];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                let mut tail = tail.lock().await;
                tail.extend_from_slice(&chunk[..read]);
                let overflow = tail.len().saturating_sub(STDERR_TAIL_BYTES);
                if overflow > 0 {
                    tail.drain(..overflow);
                }
            }
        }
    }
}

/// Sweep the contained tree off the async runtime: the proof polls the kernel
/// and may sleep, so it never blocks a worker thread's task. Windows sweeps
/// inside the thread that owns the tree, which is the only thing allowed to
/// touch the job.
#[cfg(not(windows))]
async fn sweep_policy_tree(policy: &HarnessChildPolicy, pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let policy = policy.clone();
    tokio::task::spawn_blocking(move || sweep_tree(&policy, pid))
        .await
        .unwrap_or(false)
}

/// Whether one pid is gone, read without a handle so a swept tree cannot be
/// mistaken for a still-running member.
#[cfg(not(windows))]
fn pid_is_gone(pid: u32) -> bool {
    // /proc carries the truth on Linux; elsewhere a launch never reached this
    // point, so nothing is claimed.
    std::path::Path::new(&format!("/proc/{pid}")).exists().not()
}

fn id_key(id: &RpcId) -> String {
    match id {
        RpcId::Number(n) => format!("n:{n}"),
        RpcId::Text(t) => format!("t:{t}"),
    }
}

async fn fault_pending(
    pending: &Arc<Mutex<HashMap<String, oneshot::Sender<PendingReply>>>>,
    reason: String,
) {
    let mut guard = pending.lock().await;
    for (_, sender) in guard.drain() {
        let _ = sender.send(PendingReply::Fault(reason.clone()));
    }
}

/// Read one newline-terminated frame, enforcing the byte limit strictly
/// (a line longer than `max` fails even before its terminator arrives).
async fn read_frame<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> Result<Option<Vec<u8>>, FrameError> {
    let mut limited = reader.take(max as u64 + 1);
    let mut buf = Vec::new();
    let read = limited
        .read_until(b'\n', &mut buf)
        .await
        .map_err(|_| FrameError::InvalidUtf8)?;
    if read == 0 {
        return Ok(None);
    }
    if buf.len() > max {
        return Err(FrameError::TooLarge {
            size: buf.len(),
            max,
        });
    }
    if buf.last() == Some(&b'\n') {
        buf.pop();
        if buf.last() == Some(&b'\r') {
            buf.pop();
        }
    }
    Ok(Some(buf))
}

impl PluginProcess {
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// The durable operation identity this child was launched under (P23.1).
    pub fn operation_id(&self) -> &str {
        self.tree.operation_id()
    }

    /// The contained tree this child belongs to.
    pub fn tree_id(&self) -> &str {
        self.tree.tree_id()
    }

    /// Machine-checkable code of the containment policy in force for this child:
    /// the policy the launch resolved, which is the one the tree record names.
    pub fn child_policy(&self) -> &'static str {
        self.policy.code()
    }

    /// The pid the containment was proven against.
    pub fn child_pid(&self) -> u32 {
        self.tree.pid()
    }

    /// The current durable phase of this child's operation record.
    pub fn tree_phase(&self) -> SupervisorPhase {
        self.tree.phase()
    }

    /// Bounded stderr tail for diagnostics.
    pub async fn stderr_tail(&self) -> Vec<u8> {
        self.stderr_tail.lock().await.clone()
    }

    fn send_frame(&self, frame: Vec<u8>) -> Result<(), TransportError> {
        let bytes = frame.len();
        if bytes > self.limits.max_frame_bytes {
            return Err(FrameError::TooLarge {
                size: bytes,
                max: self.limits.max_frame_bytes,
            }
            .into());
        }
        let queued = self.queue_bytes.load(Ordering::SeqCst);
        if queued + bytes > self.limits.max_queue_bytes {
            return Err(TransportError::QueueOverflow(self.limits.max_queue_bytes));
        }
        self.queue_bytes.fetch_add(bytes, Ordering::SeqCst);
        self.outbound
            .send(frame)
            .map_err(|_| TransportError::Fault("writer task stopped".into()))
    }

    /// Perform a correlated call with a deadline.
    pub async fn request(
        &self,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value, TransportError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) as i64;
        let key = format!("n:{id}");
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(key, tx);
        let request = RpcMessage::Request(RpcRequest {
            jsonrpc: "2.0".into(),
            id: RpcId::Number(id),
            method: method.to_string(),
            params: Some(params),
        });
        let frame = encode_frame(&request)?;
        if let Err(error) = self.send_frame(frame) {
            // Remove the pending entry again; the call never left.
            self.pending.lock().await.remove(&format!("n:{id}"));
            return Err(error);
        }
        match tokio::time::timeout(timeout, rx).await {
            Err(_) => {
                self.pending.lock().await.remove(&format!("n:{id}"));
                Err(TransportError::Timeout("rpc response"))
            }
            Ok(Err(_)) => Err(TransportError::Closed(method.to_string())),
            Ok(Ok(PendingReply::Fault(reason))) => Err(TransportError::Fault(reason)),
            Ok(Ok(PendingReply::Reply(Ok(value)))) => Ok(value),
            Ok(Ok(PendingReply::Reply(Err(error)))) => Err(TransportError::Rpc {
                code: error.code,
                message: error.message,
            }),
        }
    }

    /// Fire-and-forget notification.
    pub fn notify(&self, method: &str, params: serde_json::Value) -> Result<(), TransportError> {
        let notification = RpcMessage::Notification(RpcNotification {
            jsonrpc: "2.0".into(),
            method: method.to_string(),
            params: Some(params),
        });
        self.send_frame(encode_frame(&notification)?)
    }

    /// Handshake: initialize within the configured timeout.
    pub async fn initialize(
        &self,
        params: &InitializeParams,
    ) -> Result<InitializeResult, TransportError> {
        let value = self
            .request(
                "initialize",
                serde_json::to_value(params).map_err(|e| TransportError::Fault(e.to_string()))?,
                self.limits.initialize_timeout,
            )
            .await?;
        serde_json::from_value(value)
            .map_err(|e| TransportError::Fault(format!("bad initialize result: {e}")))
    }

    /// Cancel: request `harness.cancel`, wait at most the grace period for
    /// acknowledgement, then kill the process regardless.
    pub async fn cancel(&self, reason: &str) -> bool {
        let outcome = tokio::time::timeout(
            self.limits.cancel_grace,
            self.request(
                "harness.cancel",
                serde_json::json!({"identity": {"runId": ""}, "reason": reason}),
                self.limits.cancel_grace + Duration::from_millis(250),
            ),
        )
        .await;
        let acknowledged = matches!(outcome, Ok(Ok(_)));
        self.kill().await;
        acknowledged
    }

    /// Terminate the tree and wait briefly for the child. The sweep comes first:
    /// killing only the direct child would leave the grandchildren running, which
    /// is exactly what P23.3 forbids.
    pub async fn kill(&self) -> Option<i32> {
        #[cfg(windows)]
        {
            let outcome = self.sweep_now().await;
            self.tree.settle(outcome.swept);
            outcome.exit
        }
        #[cfg(not(windows))]
        {
            let swept = sweep_policy_tree(&self.policy, self.tree.pid()).await;
            self.tree.settle(swept);
            let mut child = self.tree_handle.child.lock().await;
            let _ = child.start_kill();
            match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
                Ok(Ok(status)) => status.code(),
                _ => None,
            }
        }
    }

    /// Terminate and confirm that the main Harness process was reaped.
    /// Unlike [`Self::kill`], signal-based exits still return `true`.
    ///
    /// The confirmation keeps its historical meaning — the primary is proven
    /// dead — because the session layer's cancel path depends on it. What P23
    /// adds is that the tree was swept before that confirmation and that the
    /// proof of the sweep is durable: an unproven tree lands in Quarantined
    /// rather than being silently accepted ([`Self::tree_phase`]).
    pub async fn kill_confirmed(&self) -> bool {
        #[cfg(windows)]
        {
            let outcome = self.sweep_now().await;
            let confirmed = outcome.exit.is_some();
            self.tree.settle(outcome.swept && confirmed);
            return self.report_death(confirmed).await;
        }
        #[cfg(not(windows))]
        {
            let swept = sweep_policy_tree(&self.policy, self.tree.pid()).await;
            let confirmed = {
                let mut child = self.tree_handle.child.lock().await;
                let _ = child.start_kill();
                matches!(
                    tokio::time::timeout(Duration::from_secs(5), child.wait()).await,
                    Ok(Ok(_))
                )
            };
            self.tree.settle(swept && confirmed);
            self.report_death(confirmed).await
        }
    }

    /// Once process death is proven, liveness goes and host→plugin callers that
    /// would otherwise wait on a stdout reader blocked inside a nested host
    /// callback are woken directly.
    async fn report_death(&self, confirmed: bool) -> bool {
        if confirmed {
            self.alive.store(false, Ordering::SeqCst);
            fault_pending(&self.pending, "plugin process terminated".to_string()).await;
        }
        confirmed
    }

    /// Wait for natural exit. On Windows the answer is the tree thread's record:
    /// it holds the only handle to the child, so the exit is known where it is
    /// proven, not here.
    pub async fn wait(&self) -> Option<i32> {
        #[cfg(windows)]
        {
            self.await_record().await.exit
        }
        #[cfg(not(windows))]
        {
            let mut child = self.tree_handle.child.lock().await;
            child.wait().await.ok().and_then(|status| status.code())
        }
    }

    /// Ask the thread that owns the Windows tree to sweep it, and wait for the
    /// proof. The terminate flag goes first: a plugin that never reads its stdin
    /// must not keep the tree thread from getting to the job. A tree thread that
    /// is already concluding the tree on its own answers nothing — its record
    /// IS the same proof, so it is read rather than re-issued.
    #[cfg(windows)]
    async fn sweep_now(&self) -> TreeOutcome {
        self.tree_handle.terminate.store(true, Ordering::SeqCst);
        let (reply, answer) = oneshot::channel();
        if self
            .tree_handle
            .control
            .send(TreeCommand::Terminate { reply })
            .is_ok()
        {
            if let Ok(Ok(outcome)) =
                tokio::time::timeout(TREE_PROOF_DEADLINE + Duration::from_secs(2), answer).await
            {
                return outcome;
            }
        }
        self.await_record().await
    }

    /// The tree thread's conclusion, allowing it the time it may still be
    /// spending on the member list. Nothing after the deadline is a proof: the
    /// caller quarantines rather than assumes.
    #[cfg(windows)]
    async fn await_record(&self) -> TreeOutcome {
        for _ in 0..RECORD_POLL_ROUNDS {
            if let Some(outcome) = self.recorded_outcome() {
                return outcome;
            }
            tokio::time::sleep(RECORD_POLL_INTERVAL).await;
        }
        TreeOutcome::unproven()
    }

    /// What the tree thread concluded, if it has concluded anything yet.
    #[cfg(windows)]
    fn recorded_outcome(&self) -> Option<TreeOutcome> {
        recorded_outcome(&self.tree_handle.outcome)
    }
}
