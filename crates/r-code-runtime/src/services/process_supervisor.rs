//! Platform-neutral process-tree backend contract, deterministic fake, and
//! the P07 Windows job backend. Live child handles are opaque and
//! non-serializable; only P01 owner and proof records may cross a daemon
//! restart.

use crate::process_guard::{ProcessOwnerIdentity, TerminationProofKind, TerminationProofRecord};
use crate::services::artifacts::{ArtifactStore, OutputTailPolicy};
use base64::Engine as _;
use r_code_harness_protocol::{
    ArtifactRef, ProcessOutputFrame, ProcessOutputStream, ProcessReadReply, PROCESS_READ_MAX_BYTES,
    PROCESS_READ_MAX_WAIT_MS,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub type OutputFrame = ProcessOutputFrame;
pub const MAX_OUTPUT_BUFFER_BYTES: usize = 4 * 1024 * 1024;
static NEXT_BACKEND_ID: AtomicU64 = AtomicU64::new(1);

fn environment_key_allowed(name: &str) -> bool {
    matches!(
        name,
        "COMSPEC"
            | "HOME"
            | "LANG"
            | "LC_ALL"
            | "PATH"
            | "PATHEXT"
            | "SYSTEMROOT"
            | "TEMP"
            | "TERM"
            | "TMP"
            | "TMPDIR"
            | "TZ"
            | "USERPROFILE"
            | "WINDIR"
    )
}

/// Ambient inheritance is disabled; this is the complete HANDLE_LIST / FD set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum InheritedObject {
    WindowsHandle(usize),
    UnixFd(i32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnSpec {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub cwd: PathBuf,
    /// Parent environment is cleared before these entries are installed.
    pub environment: BTreeMap<String, String>,
    pub inherited_objects: Vec<InheritedObject>,
    pub output_capacity_bytes: usize,
}

impl SpawnSpec {
    pub fn validate(&self) -> Result<(), SupervisorError> {
        let inherited = self
            .inherited_objects
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let invalid_object = self.inherited_objects.iter().any(|object| {
            matches!(
                object,
                InheritedObject::WindowsHandle(0) | InheritedObject::UnixFd(..=-1)
            )
        });
        let wrong_platform = self.inherited_objects.iter().any(|object| {
            if cfg!(windows) {
                matches!(object, InheritedObject::UnixFd(_))
            } else {
                matches!(object, InheritedObject::WindowsHandle(_))
            }
        });
        let invalid_environment = self
            .environment
            .keys()
            .any(|name| name != &name.to_ascii_uppercase() || !environment_key_allowed(name));
        if self.executable.as_os_str().is_empty()
            || self.cwd.as_os_str().is_empty()
            || inherited.len() != self.inherited_objects.len()
            || invalid_object
            || wrong_platform
            || invalid_environment
            || !(1..=MAX_OUTPUT_BUFFER_BYTES).contains(&self.output_capacity_bytes)
        {
            return Err(SupervisorError::InvalidSpec("spawn policy is incomplete"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreparedChildKind {
    WindowsSuspended,
    UnixGated,
}

#[derive(Clone, PartialEq, Eq)]
pub struct PreparedLaunch {
    backend_id: u64,
    spec: SpawnSpec,
}

#[derive(Clone, PartialEq, Eq)]
pub struct PreparedChild {
    backend_id: u64,
    id: u64,
    kind: PreparedChildKind,
}

#[derive(Clone, PartialEq, Eq)]
pub struct RunningTree {
    backend_id: u64,
    id: u64,
    kind: PreparedChildKind,
}

impl PreparedChild {
    pub fn kind(&self) -> PreparedChildKind {
        self.kind
    }
}

impl RunningTree {
    pub fn kind(&self) -> PreparedChildKind {
        self.kind
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct OutputSubscription {
    backend_id: u64,
    tree_id: u64,
}

macro_rules! opaque_debug {
    ($type:ty, $name:literal) => {
        impl std::fmt::Debug for $type {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(concat!($name, "(..)"))
            }
        }
    };
}

opaque_debug!(PreparedLaunch, "PreparedLaunch");
opaque_debug!(PreparedChild, "PreparedChild");
opaque_debug!(RunningTree, "RunningTree");
opaque_debug!(OutputSubscription, "OutputSubscription");

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FaultPoint {
    Prepare,
    SpawnSuspended,
    ProbeIdentity,
    PersistIdentity,
    AbortPrepared,
    ResumeOnce,
    Terminate,
    WaitAndProve,
    SubscribeOutput,
    EmitOutput,
    Drain,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SupervisorError {
    #[error("invalid spawn specification: {0}")]
    InvalidSpec(&'static str),
    #[error("unknown or stale live handle")]
    UnknownHandle,
    #[error("invalid supervisor state: {0}")]
    InvalidState(&'static str),
    #[error("injected supervisor fault at {0:?}")]
    Injected(FaultPoint),
    #[error("output backpressure limit reached")]
    Backpressure,
    #[error("output cursor is stale")]
    StaleCursor,
    #[error("page is too small for the next frame")]
    PageTooSmall,
    #[error("process-tree termination is unverifiable")]
    Unverifiable,
    #[error("supervisor journal failure: {0}")]
    Journal(String),
    #[error("output-tail artifact failure: {0}")]
    Artifact(String),
    #[error("supervisor operation is quarantined")]
    Quarantined,
    #[error("supervisor operation timed out")]
    Timeout,
    /// The backend refused to run the spec in this environment (e.g. an
    /// incompatible enclosing job): a hard refusal, never a degraded run.
    #[error("process-tree backend cannot run this spec here: {0}")]
    Unsupported(String),
    /// Write-capable execution is safe-disabled on this platform (P11.3):
    /// the refusal was durably recorded on the operation before any spawn.
    #[error("write execution is safe-disabled on this platform: {0}")]
    SafeDisabled(String),
}

#[async_trait::async_trait]
pub trait ProcessTreeBackend: Send + Sync {
    /// Bind the durable tree identity the journal issued for the next tree
    /// this backend spawns. Backends that cannot derive the identity from
    /// the spawn itself MUST override this so their proofs name the
    /// journal's tree id and ownership epoch; the default no-op serves
    /// backends whose identities already match the journal.
    fn bind_durable_identity(&self, _tree_id: &str, _ownership_epoch: u64) {}
    async fn prepare(&self, spec: SpawnSpec) -> Result<PreparedLaunch, SupervisorError>;
    async fn spawn_suspended(
        &self,
        launch: PreparedLaunch,
    ) -> Result<PreparedChild, SupervisorError>;
    async fn probe_identity(
        &self,
        child: &PreparedChild,
    ) -> Result<ProcessOwnerIdentity, SupervisorError>;
    async fn persist_identity(
        &self,
        child: &PreparedChild,
        identity: ProcessOwnerIdentity,
    ) -> Result<(), SupervisorError>;
    async fn abort_prepared(&self, child: &PreparedChild) -> Result<(), SupervisorError>;
    async fn resume_once(&self, child: &PreparedChild) -> Result<RunningTree, SupervisorError>;
    async fn terminate(&self, tree: &RunningTree) -> Result<(), SupervisorError>;
    async fn wait_and_prove(
        &self,
        tree: &RunningTree,
        timeout: Duration,
    ) -> Result<TerminationProofRecord, SupervisorError>;
    async fn subscribe_output(
        &self,
        tree: &RunningTree,
    ) -> Result<OutputSubscription, SupervisorError>;
    async fn drain(
        &self,
        subscription: &OutputSubscription,
        cursor: u64,
        max_bytes: u32,
        wait: Duration,
    ) -> Result<ProcessReadReply, SupervisorError>;
}

#[derive(Clone)]
pub struct DeterministicFakeBackend(Arc<Mutex<FakeState>>);

struct FakeChild {
    spec: SpawnSpec,
    observed_identity: Option<ProcessOwnerIdentity>,
    identity: Option<ProcessOwnerIdentity>,
    resumed: bool,
    terminal: bool,
    frames: Vec<OutputFrame>,
    output_bytes: usize,
    stdout_eof: bool,
    stderr_eof: bool,
    exited: bool,
    proof: Option<Result<TerminationProofRecord, ()>>,
}

struct FakeState {
    backend_id: u64,
    next_id: u64,
    kind: PreparedChildKind,
    next_identity: Option<ProcessOwnerIdentity>,
    child: Option<(u64, FakeChild)>,
    faults: BTreeSet<FaultPoint>,
}

impl DeterministicFakeBackend {
    pub fn new(kind: PreparedChildKind) -> Self {
        Self(Arc::new(Mutex::new(FakeState {
            backend_id: NEXT_BACKEND_ID.fetch_add(1, Ordering::Relaxed),
            next_id: 1,
            kind,
            next_identity: None,
            child: None,
            faults: BTreeSet::new(),
        })))
    }

    pub fn fail_once(&self, point: FaultPoint) {
        self.lock().faults.insert(point);
    }

    pub fn set_next_identity(&self, identity: ProcessOwnerIdentity) {
        self.lock().next_identity = Some(identity);
    }

    pub fn push_output(
        &self,
        tree: &RunningTree,
        frame: OutputFrame,
    ) -> Result<(), SupervisorError> {
        let mut state = self.lock();
        state.require_backend(tree.backend_id)?;
        state.boundary(FaultPoint::EmitOutput)?;
        let child = state.child_mut(tree.id)?;
        if !child.resumed || child.exited {
            return Err(SupervisorError::InvalidState(
                "tree is not accepting output",
            ));
        }
        if frame.sequence() != child.frames.len() as u64 {
            return Err(SupervisorError::InvalidState(
                "output sequence is not contiguous",
            ));
        }
        let bytes = frame_bytes(&frame)?;
        match &frame {
            OutputFrame::Data { stream, .. } if stream_eof(child, *stream) => {
                return Err(SupervisorError::InvalidState("data follows stream EOF"));
            }
            OutputFrame::Eof { stream, .. } if stream_eof(child, *stream) => {
                return Err(SupervisorError::InvalidState("duplicate stream EOF"));
            }
            _ => {}
        }
        if child.output_bytes.saturating_add(bytes) > child.spec.output_capacity_bytes {
            return Err(SupervisorError::Backpressure);
        }
        child.output_bytes += bytes;
        match &frame {
            OutputFrame::Eof { stream, .. } => set_stream_eof(child, *stream),
            OutputFrame::Exit { .. } => child.exited = true,
            OutputFrame::Data { .. } => {}
        }
        child.frames.push(frame);
        Ok(())
    }

    pub fn set_termination_proof(
        &self,
        tree: &RunningTree,
        proof: Option<TerminationProofRecord>,
    ) -> Result<(), SupervisorError> {
        let mut state = self.lock();
        state.require_backend(tree.backend_id)?;
        let child = state.child_mut(tree.id)?;
        if !matches!(child.frames.last(), Some(OutputFrame::Exit { .. })) {
            return Err(SupervisorError::InvalidState(
                "termination proof requires a final exit frame",
            ));
        }
        child.terminal = true;
        child.proof = Some(proof.ok_or(()));
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, FakeState> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Default for DeterministicFakeBackend {
    fn default() -> Self {
        Self::new(if cfg!(windows) {
            PreparedChildKind::WindowsSuspended
        } else {
            PreparedChildKind::UnixGated
        })
    }
}

impl FakeState {
    fn boundary(&mut self, point: FaultPoint) -> Result<(), SupervisorError> {
        if self.faults.remove(&point) {
            Err(SupervisorError::Injected(point))
        } else {
            Ok(())
        }
    }

    fn require_backend(&self, backend_id: u64) -> Result<(), SupervisorError> {
        if self.backend_id == backend_id {
            Ok(())
        } else {
            Err(SupervisorError::UnknownHandle)
        }
    }

    fn child(&self, id: u64) -> Result<&FakeChild, SupervisorError> {
        self.child
            .as_ref()
            .filter(|(current, _)| *current == id)
            .map(|(_, child)| child)
            .ok_or(SupervisorError::UnknownHandle)
    }

    fn child_mut(&mut self, id: u64) -> Result<&mut FakeChild, SupervisorError> {
        self.child
            .as_mut()
            .filter(|(current, _)| *current == id)
            .map(|(_, child)| child)
            .ok_or(SupervisorError::UnknownHandle)
    }
}

#[async_trait::async_trait]
impl ProcessTreeBackend for DeterministicFakeBackend {
    async fn prepare(&self, spec: SpawnSpec) -> Result<PreparedLaunch, SupervisorError> {
        spec.validate()?;
        let mut state = self.lock();
        state.boundary(FaultPoint::Prepare)?;
        Ok(PreparedLaunch {
            backend_id: state.backend_id,
            spec,
        })
    }

    async fn spawn_suspended(
        &self,
        launch: PreparedLaunch,
    ) -> Result<PreparedChild, SupervisorError> {
        let mut state = self.lock();
        state.require_backend(launch.backend_id)?;
        state.boundary(FaultPoint::SpawnSuspended)?;
        if state.child.is_some() {
            return Err(SupervisorError::InvalidState("fake already owns a child"));
        }
        let id = state.next_id;
        state.next_id += 1;
        let kind = state.kind;
        let observed_identity = state.next_identity.take();
        state.child = Some((
            id,
            FakeChild {
                spec: launch.spec,
                observed_identity,
                identity: None,
                resumed: false,
                terminal: false,
                frames: Vec::new(),
                output_bytes: 0,
                stdout_eof: false,
                stderr_eof: false,
                exited: false,
                proof: None,
            },
        ));
        Ok(PreparedChild {
            backend_id: state.backend_id,
            id,
            kind,
        })
    }

    async fn probe_identity(
        &self,
        child: &PreparedChild,
    ) -> Result<ProcessOwnerIdentity, SupervisorError> {
        let mut state = self.lock();
        state.require_backend(child.backend_id)?;
        state.boundary(FaultPoint::ProbeIdentity)?;
        state
            .child(child.id)?
            .observed_identity
            .clone()
            .ok_or(SupervisorError::InvalidState(
                "owner identity is unavailable",
            ))
    }

    async fn persist_identity(
        &self,
        child: &PreparedChild,
        identity: ProcessOwnerIdentity,
    ) -> Result<(), SupervisorError> {
        let mut state = self.lock();
        state.require_backend(child.backend_id)?;
        state.boundary(FaultPoint::PersistIdentity)?;
        let child = state.child_mut(child.id)?;
        if child.observed_identity.is_none() {
            child.observed_identity = Some(identity.clone());
        }
        let valid = identity.pid != 0
            && identity.start_identity != 0
            && !identity.platform_identity.is_null()
            && identity.platform_identity_digest
                == r_code_harness_protocol::canonical_input_hash(&identity.platform_identity);
        if !valid
            || child.observed_identity.as_ref() != Some(&identity)
            || child.identity.is_some()
            || child.resumed
        {
            return Err(SupervisorError::InvalidState(
                "identity persistence is invalid",
            ));
        }
        child.identity = Some(identity);
        Ok(())
    }

    async fn abort_prepared(&self, child: &PreparedChild) -> Result<(), SupervisorError> {
        let mut state = self.lock();
        state.require_backend(child.backend_id)?;
        state.boundary(FaultPoint::AbortPrepared)?;
        let child = state.child_mut(child.id)?;
        if child.resumed {
            return Err(SupervisorError::InvalidState("prepared child was resumed"));
        }
        child.terminal = true;
        Ok(())
    }

    async fn resume_once(&self, child: &PreparedChild) -> Result<RunningTree, SupervisorError> {
        let mut state = self.lock();
        state.require_backend(child.backend_id)?;
        state.boundary(FaultPoint::ResumeOnce)?;
        let current = state.child_mut(child.id)?;
        if current.identity.is_none() || current.resumed {
            return Err(SupervisorError::InvalidState(
                "resume requires one persisted identity",
            ));
        }
        current.resumed = true;
        Ok(RunningTree {
            backend_id: child.backend_id,
            id: child.id,
            kind: child.kind,
        })
    }

    async fn terminate(&self, tree: &RunningTree) -> Result<(), SupervisorError> {
        let mut state = self.lock();
        state.require_backend(tree.backend_id)?;
        state.boundary(FaultPoint::Terminate)?;
        let child = state.child_mut(tree.id)?;
        if !child.resumed {
            return Err(SupervisorError::InvalidState("tree was not resumed"));
        }
        child.terminal = true;
        Ok(())
    }

    async fn wait_and_prove(
        &self,
        tree: &RunningTree,
        _timeout: Duration,
    ) -> Result<TerminationProofRecord, SupervisorError> {
        let mut state = self.lock();
        state.require_backend(tree.backend_id)?;
        state.boundary(FaultPoint::WaitAndProve)?;
        let child = state.child(tree.id)?;
        if !child.terminal {
            return Err(SupervisorError::InvalidState("tree is not terminal"));
        }
        child
            .proof
            .clone()
            .ok_or(SupervisorError::InvalidState("proof is not configured"))?
            .map_err(|_| SupervisorError::Unverifiable)
    }

    async fn subscribe_output(
        &self,
        tree: &RunningTree,
    ) -> Result<OutputSubscription, SupervisorError> {
        let mut state = self.lock();
        state.require_backend(tree.backend_id)?;
        state.boundary(FaultPoint::SubscribeOutput)?;
        if !state.child(tree.id)?.resumed {
            return Err(SupervisorError::InvalidState("tree was not resumed"));
        }
        Ok(OutputSubscription {
            backend_id: tree.backend_id,
            tree_id: tree.id,
        })
    }

    async fn drain(
        &self,
        subscription: &OutputSubscription,
        cursor: u64,
        max_bytes: u32,
        wait: Duration,
    ) -> Result<ProcessReadReply, SupervisorError> {
        let mut state = self.lock();
        state.require_backend(subscription.backend_id)?;
        state.boundary(FaultPoint::Drain)?;
        if max_bytes == 0
            || max_bytes > PROCESS_READ_MAX_BYTES
            || wait > Duration::from_millis(u64::from(PROCESS_READ_MAX_WAIT_MS))
        {
            return Err(SupervisorError::InvalidSpec("invalid drain bounds"));
        }
        let child = state.child(subscription.tree_id)?;
        let start = usize::try_from(cursor).map_err(|_| SupervisorError::StaleCursor)?;
        if start > child.frames.len() {
            return Err(SupervisorError::StaleCursor);
        }
        let mut used = 0usize;
        let mut frames = Vec::new();
        for frame in &child.frames[start..] {
            let bytes = frame_bytes(frame)?;
            if used + bytes > max_bytes as usize {
                if frames.is_empty() {
                    return Err(SupervisorError::PageTooSmall);
                }
                break;
            }
            used += bytes;
            frames.push(frame.clone());
        }
        let next_cursor = cursor + frames.len() as u64;
        let page_exit = frames.last().and_then(exit_code);
        let log_exit = child.frames.last().and_then(exit_code);
        let terminal = page_exit.is_some()
            || (frames.is_empty() && start == child.frames.len() && log_exit.is_some());
        Ok(ProcessReadReply {
            frames,
            next_cursor,
            terminal,
            exit_code: terminal.then(|| log_exit.flatten()).flatten(),
        })
    }
}

fn frame_bytes(frame: &OutputFrame) -> Result<usize, SupervisorError> {
    match frame {
        OutputFrame::Data { data_base64, .. } => base64::engine::general_purpose::STANDARD
            .decode(data_base64)
            .map(|bytes| bytes.len())
            .map_err(|_| SupervisorError::InvalidState("output data is not base64")),
        OutputFrame::Eof { .. } | OutputFrame::Exit { .. } => Ok(0),
    }
}

fn stream_eof(child: &FakeChild, stream: ProcessOutputStream) -> bool {
    match stream {
        ProcessOutputStream::Stdout => child.stdout_eof,
        ProcessOutputStream::Stderr => child.stderr_eof,
    }
}

fn set_stream_eof(child: &mut FakeChild, stream: ProcessOutputStream) {
    match stream {
        ProcessOutputStream::Stdout => child.stdout_eof = true,
        ProcessOutputStream::Stderr => child.stderr_eof = true,
    }
}

fn exit_code(frame: &OutputFrame) -> Option<Option<i32>> {
    match frame {
        OutputFrame::Exit { exit_code, .. } => Some(*exit_code),
        _ => None,
    }
}

// P05 durable orchestration -------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SupervisorPhase {
    Prepared,
    Suspended,
    IdentityRecorded,
    ResumePending,
    Running,
    Draining,
    Drained,
    ProofAccepted,
    Completed,
    Quarantined,
    /// Write-capable execution refused before any spawn (P11.3): terminal,
    /// durable, never activated and never retried as a spawn.
    SafeDisabled,
}

// P11.3 — write-execution SafeDisabled classification -------------------------

/// Effect profile of a supervised execution (P11.3). Only WriteCapable
/// execution requires a kernel tree-containment proof;
/// NoWorkspaceDiagnostics supervision is enumerable best-effort and is never
/// a containment claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionWriteProfile {
    NoWorkspaceDiagnostics,
    WriteCapable,
}

/// Durable SafeDisabled classification (P11.3): stored on the operation
/// record when write execution is refused. SafeDisabled is terminal — it is
/// never Activated and never becomes a spawn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteExecutionDisabled {
    pub platform: String,
    pub profile: ExecutionWriteProfile,
    pub reason: String,
}

impl ExecutionWriteProfile {
    /// Kernel containment status for the CURRENT platform (P11.3): Windows
    /// has the P07 creation-time Job proof; macOS has no kernel containment
    /// (P11 enumeration stays diagnostics); Linux's namespace proof arrives
    /// with P09. `Some((platform, reason))` ⇔ write execution stays
    /// SafeDisabled. This is static platform/proof knowledge only — it is
    /// never derived from probe results and never feeds activation
    /// (P12/P13 own that domain).
    pub fn write_safe_disabled(&self) -> Option<(&'static str, &'static str)> {
        let Self::WriteCapable = self else {
            return None;
        };
        if cfg!(windows) {
            None
        } else if cfg!(target_os = "macos") {
            Some(("macos", "macos-no-kernel-tree-containment"))
        } else {
            Some(("linux", "linux-namespace-proof-pending-p09"))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorRecord {
    pub operation_id: String,
    pub tree_id: String,
    pub task_id: String,
    pub ownership_epoch: u64,
    pub content_digest: String,
    pub phase: SupervisorPhase,
    pub revision: u64,
    pub output_cursor: u64,
    pub owner: Option<ProcessOwnerIdentity>,
    pub proof: Option<TerminationProofRecord>,
    pub output_tail: Option<ArtifactRef>,
    pub quarantine_reason: Option<String>,
    /// P11.3 SafeDisabled classification; present only in (and immutable
    /// after) the terminal SafeDisabled phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub write_disabled: Option<WriteExecutionDisabled>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrepareDisposition {
    Created(SupervisorRecord),
    Existing(SupervisorRecord),
}

pub trait SupervisorJournal: Send + Sync {
    fn load(&self, operation_id: &str) -> Result<Option<SupervisorRecord>, SupervisorError>;
    fn prepare(&self, record: SupervisorRecord) -> Result<PrepareDisposition, SupervisorError>;
    fn compare_and_swap(
        &self,
        expected: &SupervisorRecord,
        next: SupervisorRecord,
    ) -> Result<SupervisorRecord, SupervisorError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum JournalFaultPoint {
    Prepare,
    Suspended,
    IdentityRecorded,
    ResumePending,
    Running,
    Draining,
    OutputCursor,
    Drained,
    ProofAccepted,
    Completed,
    Quarantined,
    SafeDisabled,
}

/// Reopenable deterministic reference journal for tests. Production
/// durability remains an adapter seam for the later V1Store integration.
#[derive(Clone, Default)]
pub struct DeterministicSupervisorJournal {
    state: Arc<Mutex<JournalState>>,
}

#[derive(Default)]
struct JournalState {
    records: BTreeMap<String, SupervisorRecord>,
    faults: BTreeSet<JournalFaultPoint>,
    lost_acks: BTreeSet<JournalFaultPoint>,
}

impl DeterministicSupervisorJournal {
    pub fn reopen(&self) -> Self {
        self.clone()
    }

    pub fn fail_once(&self, point: JournalFaultPoint) {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .faults
            .insert(point);
    }

    pub fn lose_ack_once(&self, point: JournalFaultPoint) {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .lost_acks
            .insert(point);
    }
}

impl SupervisorJournal for DeterministicSupervisorJournal {
    fn load(&self, operation_id: &str) -> Result<Option<SupervisorRecord>, SupervisorError> {
        Ok(self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .records
            .get(operation_id)
            .cloned())
    }

    fn prepare(&self, record: SupervisorRecord) -> Result<PrepareDisposition, SupervisorError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.faults.remove(&JournalFaultPoint::Prepare) {
            return Err(SupervisorError::Journal("injected prepare fault".into()));
        }
        if record.phase != SupervisorPhase::Prepared
            || record.revision != 1
            || record.output_cursor != 0
            || record.owner.is_some()
            || record.proof.is_some()
            || record.output_tail.is_some()
            || record.quarantine_reason.is_some()
            || record.write_disabled.is_some()
            || record.operation_id.trim().is_empty()
            || record.tree_id.trim().is_empty()
            || record.task_id.trim().is_empty()
            || record.ownership_epoch == 0
            || !is_canonical_digest(&record.content_digest)
        {
            return Err(SupervisorError::Journal("invalid prepared record".into()));
        }
        if let Some(existing) = state.records.get(&record.operation_id) {
            if same_operation(existing, &record) {
                return Ok(PrepareDisposition::Existing(existing.clone()));
            }
            return Err(SupervisorError::Journal(
                "operation identity conflict".into(),
            ));
        }
        state
            .records
            .insert(record.operation_id.clone(), record.clone());
        if state.lost_acks.remove(&JournalFaultPoint::Prepare) {
            return Err(SupervisorError::Journal(
                "lost prepare acknowledgement".into(),
            ));
        }
        Ok(PrepareDisposition::Created(record))
    }

    fn compare_and_swap(
        &self,
        expected: &SupervisorRecord,
        next: SupervisorRecord,
    ) -> Result<SupervisorRecord, SupervisorError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let current = state
            .records
            .get(&expected.operation_id)
            .cloned()
            .ok_or_else(|| SupervisorError::Journal("operation is missing".into()))?;
        if &current != expected
            || !same_operation(&current, &next)
            || next.revision != current.revision + 1
            || next.output_cursor < current.output_cursor
            || !valid_phase_transition(current.phase, next.phase)
            || !valid_record_transition(&current, &next)
        {
            return Err(SupervisorError::Journal(
                "stale or invalid CAS transition".into(),
            ));
        }
        if state
            .faults
            .remove(&journal_fault_point(current.phase, next.phase))
        {
            return Err(SupervisorError::Journal("injected transition fault".into()));
        }
        let point = journal_fault_point(current.phase, next.phase);
        state
            .records
            .insert(next.operation_id.clone(), next.clone());
        if state.lost_acks.remove(&point) {
            return Err(SupervisorError::Journal(
                "lost transition acknowledgement".into(),
            ));
        }
        Ok(next)
    }
}

fn journal_fault_point(from: SupervisorPhase, to: SupervisorPhase) -> JournalFaultPoint {
    match (from, to) {
        (_, SupervisorPhase::Quarantined) => JournalFaultPoint::Quarantined,
        (_, SupervisorPhase::SafeDisabled) => JournalFaultPoint::SafeDisabled,
        (SupervisorPhase::Draining, SupervisorPhase::Draining) => JournalFaultPoint::OutputCursor,
        (_, SupervisorPhase::Suspended) => JournalFaultPoint::Suspended,
        (_, SupervisorPhase::IdentityRecorded) => JournalFaultPoint::IdentityRecorded,
        (_, SupervisorPhase::ResumePending) => JournalFaultPoint::ResumePending,
        (_, SupervisorPhase::Running) => JournalFaultPoint::Running,
        (_, SupervisorPhase::Draining) => JournalFaultPoint::Draining,
        (_, SupervisorPhase::Drained) => JournalFaultPoint::Drained,
        (_, SupervisorPhase::ProofAccepted) => JournalFaultPoint::ProofAccepted,
        (_, SupervisorPhase::Completed) => JournalFaultPoint::Completed,
        _ => JournalFaultPoint::Quarantined,
    }
}

fn same_operation(left: &SupervisorRecord, right: &SupervisorRecord) -> bool {
    left.operation_id == right.operation_id
        && left.tree_id == right.tree_id
        && left.task_id == right.task_id
        && left.ownership_epoch == right.ownership_epoch
        && left.content_digest == right.content_digest
}

fn valid_phase_transition(from: SupervisorPhase, to: SupervisorPhase) -> bool {
    (to == SupervisorPhase::Quarantined
        && !matches!(
            from,
            SupervisorPhase::Completed
                | SupervisorPhase::Quarantined
                | SupervisorPhase::SafeDisabled
        ))
        || (to == SupervisorPhase::SafeDisabled && from == SupervisorPhase::Prepared)
        || matches!(
            (from, to),
            (SupervisorPhase::Prepared, SupervisorPhase::Suspended)
                | (
                    SupervisorPhase::Suspended,
                    SupervisorPhase::IdentityRecorded
                )
                | (
                    SupervisorPhase::IdentityRecorded,
                    SupervisorPhase::ResumePending
                )
                | (SupervisorPhase::ResumePending, SupervisorPhase::Running)
                | (SupervisorPhase::Running, SupervisorPhase::Draining)
                | (SupervisorPhase::Draining, SupervisorPhase::Draining)
                | (SupervisorPhase::Draining, SupervisorPhase::Drained)
                | (SupervisorPhase::Drained, SupervisorPhase::ProofAccepted)
                | (SupervisorPhase::ProofAccepted, SupervisorPhase::Completed)
        )
}

fn valid_record_transition(current: &SupervisorRecord, next: &SupervisorRecord) -> bool {
    if current.owner.is_some() && current.owner != next.owner
        || current.proof.is_some() && current.proof != next.proof
        || current.output_tail.is_some() && current.output_tail != next.output_tail
        || current.write_disabled.is_some() && current.write_disabled != next.write_disabled
        || next.phase != SupervisorPhase::Quarantined && next.quarantine_reason.is_some()
        || next.phase != SupervisorPhase::SafeDisabled && next.write_disabled.is_some()
        || next.phase != SupervisorPhase::Completed && next.output_tail.is_some()
    {
        return false;
    }
    match next.phase {
        SupervisorPhase::Prepared | SupervisorPhase::Suspended => {
            next.owner.is_none() && next.proof.is_none()
        }
        SupervisorPhase::IdentityRecorded
        | SupervisorPhase::ResumePending
        | SupervisorPhase::Running
        | SupervisorPhase::Draining
        | SupervisorPhase::Drained => next.owner.is_some() && next.proof.is_none(),
        SupervisorPhase::ProofAccepted => next.owner.is_some() && next.proof.is_some(),
        SupervisorPhase::Completed => {
            next.owner.is_some() && next.proof.is_some() && next.output_tail.is_some()
        }
        SupervisorPhase::Quarantined => next.quarantine_reason.is_some(),
        SupervisorPhase::SafeDisabled => {
            next.write_disabled.is_some()
                && next.quarantine_reason.is_none()
                && next.owner.is_none()
                && next.proof.is_none()
                && next.output_tail.is_none()
        }
    }
}

fn is_canonical_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub struct SupervisorStart {
    pub operation_id: String,
    pub tree_id: String,
    pub task_id: String,
    pub ownership_epoch: u64,
    pub spec: SpawnSpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SupervisorLimits {
    pub run_timeout: Duration,
    pub termination_timeout: Duration,
    pub read_wait: Duration,
    pub max_read_bytes: u32,
}

#[derive(Clone, Default)]
pub struct CancellationSignal(Arc<AtomicBool>);

impl CancellationSignal {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

pub struct SupervisedRun {
    record: SupervisorRecord,
    tree: RunningTree,
    subscription: OutputSubscription,
}

impl SupervisedRun {
    pub fn tree(&self) -> &RunningTree {
        &self.tree
    }

    pub fn record(&self) -> &SupervisorRecord {
        &self.record
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SupervisorCompletion {
    pub record: SupervisorRecord,
    pub output_tail: ArtifactRef,
}

pub struct ProcessSupervisor {
    backend: Arc<dyn ProcessTreeBackend>,
    journal: Arc<dyn SupervisorJournal>,
}

impl ProcessSupervisor {
    pub fn new(backend: Arc<dyn ProcessTreeBackend>, journal: Arc<dyn SupervisorJournal>) -> Self {
        Self { backend, journal }
    }

    pub async fn start(&self, request: SupervisorStart) -> Result<SupervisedRun, SupervisorError> {
        request.spec.validate()?;
        if request.operation_id.trim().is_empty()
            || request.tree_id.trim().is_empty()
            || request.task_id.trim().is_empty()
            || request.ownership_epoch == 0
        {
            return Err(SupervisorError::InvalidSpec("supervisor identity is empty"));
        }
        let record = SupervisorRecord {
            content_digest: start_digest(&request),
            operation_id: request.operation_id.clone(),
            tree_id: request.tree_id,
            task_id: request.task_id,
            ownership_epoch: request.ownership_epoch,
            phase: SupervisorPhase::Prepared,
            revision: 1,
            output_cursor: 0,
            owner: None,
            proof: None,
            output_tail: None,
            quarantine_reason: None,
            write_disabled: None,
        };
        let mut record = match self.journal.prepare(record)? {
            PrepareDisposition::Created(record) => record,
            PrepareDisposition::Existing(record) => {
                if record.phase == SupervisorPhase::Completed {
                    return Err(SupervisorError::InvalidState("operation already completed"));
                }
                self.quarantine(&record.operation_id, "restart-or-retry-before-completion")?;
                return Err(SupervisorError::Quarantined);
            }
        };
        self.backend
            .bind_durable_identity(&record.tree_id, record.ownership_epoch);
        let launch = match self.backend.prepare(request.spec).await {
            Ok(launch) => launch,
            Err(_) => return self.fail_start(&record, None).await,
        };
        let child = match self.backend.spawn_suspended(launch).await {
            Ok(child) => child,
            Err(_) => return self.fail_start(&record, None).await,
        };
        record = match self.advance(&record, SupervisorPhase::Suspended) {
            Ok(record) => record,
            Err(_) => return self.fail_start(&record, Some(&child)).await,
        };
        let identity = match self.backend.probe_identity(&child).await {
            Ok(identity) => identity,
            Err(_) => return self.fail_start(&record, Some(&child)).await,
        };
        let mut identity_record = next_record(&record, SupervisorPhase::IdentityRecorded);
        identity_record.owner = Some(identity.clone());
        record = match self.journal.compare_and_swap(&record, identity_record) {
            Ok(record) => record,
            Err(_) => return self.fail_start(&record, Some(&child)).await,
        };
        if self
            .backend
            .persist_identity(&child, identity)
            .await
            .is_err()
        {
            return self.fail_start(&record, Some(&child)).await;
        }
        record = match self.advance(&record, SupervisorPhase::ResumePending) {
            Ok(record) => record,
            Err(_) => return self.fail_start(&record, Some(&child)).await,
        };
        let tree = match self.backend.resume_once(&child).await {
            Ok(tree) => tree,
            Err(_) => return self.fail_start(&record, Some(&child)).await,
        };
        record = match self.advance(&record, SupervisorPhase::Running) {
            Ok(record) => record,
            Err(_) => {
                let _ = tokio::time::timeout(Duration::from_secs(5), self.backend.terminate(&tree))
                    .await;
                self.quarantine(&record.operation_id, "post-resume-journal-ambiguity")?;
                return Err(SupervisorError::Quarantined);
            }
        };
        let subscription = match self.backend.subscribe_output(&tree).await {
            Ok(subscription) => subscription,
            Err(_) => {
                let _ = tokio::time::timeout(Duration::from_secs(5), self.backend.terminate(&tree))
                    .await;
                self.quarantine(&record.operation_id, "output-subscription-failed")?;
                return Err(SupervisorError::Quarantined);
            }
        };
        Ok(SupervisedRun {
            record,
            tree,
            subscription,
        })
    }

    /// P11.3: start a supervised execution under a write profile. A platform
    /// without kernel tree containment durably refuses BEFORE any spawn: the
    /// operation record is transitioned to terminal SafeDisabled carrying
    /// the platform/profile/reason, and `Err(SafeDisabled)` is returned —
    /// [`Self::recover`] hands that record back, nothing ever activates and
    /// the operation is never retried as a spawn. Platforms with a
    /// containment proof (Windows, P07 job) and NoWorkspaceDiagnostics
    /// supervision proceed through the normal [`Self::start`] path.
    pub async fn start_with_write_profile(
        &self,
        request: SupervisorStart,
        profile: ExecutionWriteProfile,
    ) -> Result<SupervisedRun, SupervisorError> {
        let Some((platform, reason)) = profile.write_safe_disabled() else {
            return self.start(request).await;
        };
        request.spec.validate()?;
        if request.operation_id.trim().is_empty()
            || request.tree_id.trim().is_empty()
            || request.task_id.trim().is_empty()
            || request.ownership_epoch == 0
        {
            return Err(SupervisorError::InvalidSpec("supervisor identity is empty"));
        }
        let record = SupervisorRecord {
            content_digest: start_digest(&request),
            operation_id: request.operation_id.clone(),
            tree_id: request.tree_id,
            task_id: request.task_id,
            ownership_epoch: request.ownership_epoch,
            phase: SupervisorPhase::Prepared,
            revision: 1,
            output_cursor: 0,
            owner: None,
            proof: None,
            output_tail: None,
            quarantine_reason: None,
            write_disabled: None,
        };
        let record = match self.journal.prepare(record)? {
            PrepareDisposition::Created(record) => record,
            // A same-identity operation already exists: refuse without side
            // effects — no spawn, no quarantine; recover() owns its fate.
            PrepareDisposition::Existing(_) => {
                return Err(SupervisorError::InvalidState(
                    "operation identity already exists",
                ));
            }
        };
        let mut disabled = next_record(&record, SupervisorPhase::SafeDisabled);
        disabled.write_disabled = Some(WriteExecutionDisabled {
            platform: platform.to_string(),
            profile,
            reason: reason.to_string(),
        });
        self.journal.compare_and_swap(&record, disabled)?;
        Err(SupervisorError::SafeDisabled(reason.to_string()))
    }

    pub async fn finish(
        &self,
        mut run: SupervisedRun,
        artifacts: &ArtifactStore,
        redaction: &OutputTailPolicy,
        limits: SupervisorLimits,
        cancellation: &CancellationSignal,
    ) -> Result<SupervisorCompletion, SupervisorError> {
        validate_limits(limits)?;
        run.record = match self.advance(&run.record, SupervisorPhase::Draining) {
            Ok(record) => record,
            Err(_) => return self.fail_finish(&run, "draining-journal-ambiguity").await,
        };
        let capture = redaction.capture_window_bytes();
        let mut stdout = RollingTail::new(capture);
        let mut stderr = RollingTail::new(capture);
        let run_deadline = match deadline_after(limits.run_timeout) {
            Ok(deadline) => deadline,
            Err(_) => return self.fail_finish(&run, "run-deadline-overflow").await,
        };
        let mut termination_deadline = None;
        loop {
            let now = Instant::now();
            if termination_deadline.is_none()
                && (cancellation.is_cancelled() || now >= run_deadline)
            {
                if !matches!(
                    tokio::time::timeout(
                        limits.termination_timeout,
                        self.backend.terminate(&run.tree),
                    )
                    .await,
                    Ok(Ok(()))
                ) {
                    return self.fail_finish(&run, "termination-failed").await;
                }
                termination_deadline = match deadline_after(limits.termination_timeout) {
                    Ok(deadline) => Some(deadline),
                    Err(_) => {
                        return self
                            .fail_finish(&run, "termination-deadline-overflow")
                            .await
                    }
                };
            }
            if termination_deadline.is_some_and(|deadline| now >= deadline) {
                self.quarantine(&run.record.operation_id, "terminal-drain-timeout")?;
                return Err(SupervisorError::Timeout);
            }
            let active_deadline = termination_deadline.unwrap_or(run_deadline);
            let remaining = active_deadline.saturating_duration_since(Instant::now());
            let read_wait = limits.read_wait.min(remaining);
            let drain = self.backend.drain(
                &run.subscription,
                run.record.output_cursor,
                limits.max_read_bytes,
                read_wait,
            );
            let page = match tokio::time::timeout(
                read_wait.saturating_add(Duration::from_millis(100)),
                drain,
            )
            .await
            {
                Ok(Ok(page)) => page,
                Err(_) => return self.fail_finish(&run, "output-drain-failed").await,
                Ok(Err(_)) => return self.fail_finish(&run, "output-drain-failed").await,
            };
            if validate_page(run.record.output_cursor, limits.max_read_bytes, &page).is_err()
                || capture_page(&page, &mut stdout, &mut stderr).is_err()
            {
                return self.fail_finish(&run, "invalid-output-page").await;
            }
            let mut cursor_record = next_record(&run.record, SupervisorPhase::Draining);
            cursor_record.output_cursor = page.next_cursor;
            run.record = match self.journal.compare_and_swap(&run.record, cursor_record) {
                Ok(record) => record,
                Err(_) => {
                    return self
                        .fail_finish(&run, "output-cursor-journal-ambiguity")
                        .await
                }
            };
            if page.terminal {
                break;
            }
            tokio::task::yield_now().await;
        }
        run.record = match self.advance(&run.record, SupervisorPhase::Drained) {
            Ok(record) => record,
            Err(_) => return self.fail_finish(&run, "drained-journal-ambiguity").await,
        };
        let proof = match tokio::time::timeout(
            limits.termination_timeout,
            self.backend
                .wait_and_prove(&run.tree, limits.termination_timeout),
        )
        .await
        {
            Ok(Ok(proof)) => proof,
            Ok(Err(_)) | Err(_) => return self.fail_finish(&run, "termination-proof-failed").await,
        };
        if !proof_matches_record(&proof, &run.record) {
            return self.fail_finish(&run, "termination-proof-mismatch").await;
        }
        let mut proof_record = next_record(&run.record, SupervisorPhase::ProofAccepted);
        proof_record.proof = Some(proof);
        run.record = match self.journal.compare_and_swap(&run.record, proof_record) {
            Ok(record) => record,
            Err(_) => return self.fail_finish(&run, "proof-journal-ambiguity").await,
        };
        if artifacts.task_id() != Some(run.record.task_id.as_str()) {
            return self.fail_finish(&run, "output-tail-owner-mismatch").await;
        }
        let raw_tail = combined_tail(stdout, stderr);
        let tail = match artifacts.put_redacted_output_tail(&raw_tail, redaction) {
            Ok(tail) => tail,
            Err(error) => {
                self.quarantine(&run.record.operation_id, "output-tail-store-failed")?;
                return Err(SupervisorError::Artifact(error.to_string()));
            }
        };
        let mut completed = next_record(&run.record, SupervisorPhase::Completed);
        completed.output_tail = Some(tail.clone());
        run.record = match self.journal.compare_and_swap(&run.record, completed) {
            Ok(record) => record,
            Err(_) => return self.fail_finish(&run, "completion-journal-ambiguity").await,
        };
        Ok(SupervisorCompletion {
            record: run.record,
            output_tail: tail,
        })
    }

    pub fn recover(&self, operation_id: &str) -> Result<SupervisorRecord, SupervisorError> {
        let record = self
            .journal
            .load(operation_id)?
            .ok_or_else(|| SupervisorError::Journal("operation is missing".into()))?;
        if matches!(
            record.phase,
            SupervisorPhase::Completed
                | SupervisorPhase::Quarantined
                | SupervisorPhase::SafeDisabled
        ) {
            return Ok(record);
        }
        self.quarantine(operation_id, "recovered-nonterminal-operation")
    }

    async fn fail_start(
        &self,
        record: &SupervisorRecord,
        child: Option<&PreparedChild>,
    ) -> Result<SupervisedRun, SupervisorError> {
        if let Some(child) = child {
            let _ =
                tokio::time::timeout(Duration::from_secs(5), self.backend.abort_prepared(child))
                    .await;
        }
        self.quarantine(&record.operation_id, "start-boundary-ambiguous")?;
        Err(SupervisorError::Quarantined)
    }

    async fn fail_finish<T>(
        &self,
        run: &SupervisedRun,
        reason: &str,
    ) -> Result<T, SupervisorError> {
        let _ =
            tokio::time::timeout(Duration::from_secs(5), self.backend.terminate(&run.tree)).await;
        self.quarantine(&run.record.operation_id, reason)?;
        Err(SupervisorError::Quarantined)
    }

    fn advance(
        &self,
        record: &SupervisorRecord,
        phase: SupervisorPhase,
    ) -> Result<SupervisorRecord, SupervisorError> {
        self.journal
            .compare_and_swap(record, next_record(record, phase))
    }

    fn quarantine(
        &self,
        operation_id: &str,
        reason: &str,
    ) -> Result<SupervisorRecord, SupervisorError> {
        let record = self
            .journal
            .load(operation_id)?
            .ok_or_else(|| SupervisorError::Journal("operation is missing".into()))?;
        if record.phase == SupervisorPhase::Quarantined {
            return Ok(record);
        }
        if matches!(
            record.phase,
            SupervisorPhase::Completed | SupervisorPhase::SafeDisabled
        ) {
            return Err(SupervisorError::Journal(
                "terminal operation cannot quarantine".into(),
            ));
        }
        let mut quarantined = next_record(&record, SupervisorPhase::Quarantined);
        quarantined.quarantine_reason = Some(reason.to_string());
        self.journal.compare_and_swap(&record, quarantined)
    }
}

fn next_record(record: &SupervisorRecord, phase: SupervisorPhase) -> SupervisorRecord {
    let mut next = record.clone();
    next.phase = phase;
    next.revision += 1;
    next
}

fn start_digest(request: &SupervisorStart) -> String {
    let inherited = request
        .spec
        .inherited_objects
        .iter()
        .map(|object| match object {
            InheritedObject::WindowsHandle(value) => serde_json::json!(["windows", value]),
            InheritedObject::UnixFd(value) => serde_json::json!(["unix", value]),
        })
        .collect::<Vec<_>>();
    r_code_harness_protocol::canonical_input_hash(&serde_json::json!({
        "operationId": request.operation_id,
        "treeId": request.tree_id,
        "taskId": request.task_id,
        "ownershipEpoch": request.ownership_epoch,
        "executable": request.spec.executable.to_string_lossy(),
        "arguments": request.spec.arguments,
        "cwd": request.spec.cwd.to_string_lossy(),
        "environment": request.spec.environment,
        "inherited": inherited,
        "outputCapacityBytes": request.spec.output_capacity_bytes,
    }))
}

fn validate_limits(limits: SupervisorLimits) -> Result<(), SupervisorError> {
    const MAX_DURATION: Duration = Duration::from_secs(24 * 60 * 60);
    if limits.run_timeout.is_zero()
        || limits.termination_timeout.is_zero()
        || limits.run_timeout > MAX_DURATION
        || limits.termination_timeout > MAX_DURATION
        || limits.read_wait > Duration::from_millis(u64::from(PROCESS_READ_MAX_WAIT_MS))
        || limits.max_read_bytes == 0
        || limits.max_read_bytes > PROCESS_READ_MAX_BYTES
    {
        return Err(SupervisorError::InvalidSpec("invalid supervisor limits"));
    }
    Ok(())
}

fn deadline_after(duration: Duration) -> Result<Instant, SupervisorError> {
    Instant::now()
        .checked_add(duration)
        .ok_or(SupervisorError::InvalidSpec("supervisor deadline overflow"))
}

fn validate_page(
    cursor: u64,
    max_bytes: u32,
    page: &ProcessReadReply,
) -> Result<(), SupervisorError> {
    let mut expected = cursor;
    let mut bytes = 0usize;
    for frame in &page.frames {
        if frame.sequence() != expected {
            return Err(SupervisorError::InvalidState(
                "output page has a sequence gap",
            ));
        }
        expected = expected
            .checked_add(1)
            .ok_or(SupervisorError::StaleCursor)?;
        bytes = bytes
            .checked_add(frame_bytes(frame)?)
            .ok_or(SupervisorError::Backpressure)?;
    }
    let final_exit = page.frames.last().and_then(exit_code);
    if page.next_cursor != expected
        || bytes > max_bytes as usize
        || page.terminal && final_exit.is_none()
        || !page.terminal && (final_exit.is_some() || page.exit_code.is_some())
        || final_exit.is_some_and(|exit| exit != page.exit_code)
    {
        return Err(SupervisorError::InvalidState(
            "output page terminal metadata is invalid",
        ));
    }
    Ok(())
}

fn proof_matches_record(proof: &TerminationProofRecord, record: &SupervisorRecord) -> bool {
    let Some(owner) = record.owner.as_ref() else {
        return false;
    };
    let Some(identity) = proof.proof_identity.as_object() else {
        return false;
    };
    proof.kind == TerminationProofKind::Exit
        && !proof.proof_id.trim().is_empty()
        && proof.tree_id == record.tree_id
        && proof.ownership_epoch == record.ownership_epoch
        && proof.observed_boot_identity == owner.boot_identity
        && proof.proof_identity_digest
            == r_code_harness_protocol::canonical_input_hash(&proof.proof_identity)
        && identity.len() == 9
        && identity.get("treeId").and_then(serde_json::Value::as_str)
            == Some(record.tree_id.as_str())
        && identity
            .get("ownershipEpoch")
            .and_then(serde_json::Value::as_u64)
            == Some(record.ownership_epoch)
        && identity.get("ownerPid").and_then(serde_json::Value::as_u64)
            == Some(u64::from(owner.pid))
        && identity
            .get("ownerStartIdentity")
            .and_then(serde_json::Value::as_u64)
            == Some(owner.start_identity)
        && identity
            .get("ownerBootIdentity")
            .and_then(serde_json::Value::as_str)
            == Some(owner.boot_identity.as_str())
        && identity
            .get("ownerPlatformIdentityDigest")
            .and_then(serde_json::Value::as_str)
            == Some(owner.platform_identity_digest.as_str())
        && identity.get("migratedObservedBootIdentity") == Some(&serde_json::Value::Null)
        && identity
            .get("observedBootIdentity")
            .and_then(serde_json::Value::as_str)
            == Some(owner.boot_identity.as_str())
        && identity
            .get("platformEvidence")
            .is_some_and(|evidence| !evidence.is_null())
}

struct RollingTail {
    bytes: Vec<u8>,
    capacity: usize,
}

impl RollingTail {
    fn new(capacity: usize) -> Self {
        Self {
            bytes: Vec::new(),
            capacity,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        self.bytes.extend_from_slice(chunk);
        if self.bytes.len() > self.capacity {
            self.bytes.drain(..self.bytes.len() - self.capacity);
        }
    }
}

fn capture_page(
    page: &ProcessReadReply,
    stdout: &mut RollingTail,
    stderr: &mut RollingTail,
) -> Result<(), SupervisorError> {
    for frame in &page.frames {
        if let OutputFrame::Data {
            stream,
            data_base64,
            ..
        } = frame
        {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data_base64)
                .map_err(|_| SupervisorError::InvalidState("output data is not base64"))?;
            match stream {
                ProcessOutputStream::Stdout => stdout.push(&bytes),
                ProcessOutputStream::Stderr => stderr.push(&bytes),
            }
        }
    }
    Ok(())
}

fn combined_tail(stdout: RollingTail, stderr: RollingTail) -> Vec<u8> {
    let mut bytes = b"[stdout]\n".to_vec();
    bytes.extend(stdout.bytes);
    bytes.extend(b"\n[stderr]\n");
    bytes.extend(stderr.bytes);
    bytes
}

// P07 Windows job backend ---------------------------------------------------

/// Real Windows implementation of [`ProcessTreeBackend`] on top of the P06
/// raw suspended spawn + P07 creation-time job assignment. The module docs
/// below state the honest scope; the inline comments after the imports
/// carry the remaining test-grade caveats.
#[cfg(windows)]
pub mod windows_backend {
    use super::*;
    use crate::process_guard::windows::{
        spawn_suspended_with_job, GuardianError, JobbedSuspendedChild, PipeRead, RawSpawnSpec,
    };
    use crate::process_guard::BootIdentity;
    use std::time::SystemTime;

    // Honest scope, part 1 — documented rather than faked:
    //
    // * `spawn_suspended` delegates to the process_guard::windows seam
    //   which performs the REAL suspended spawn with
    //   `PROC_THREAD_ATTRIBUTE_JOB_LIST`: job membership precedes any
    //   user code by construction, and an incompatible enclosing job
    //   surfaces as `Unsupported` (never a run-loose degradation).
    // * `wait_and_prove` issues the P01 nine-field proof envelope ONLY
    //   after the job's member list is proven empty (full-tree death
    //   with PID-reuse fencing) — naturally-exited trees prove the same
    //   way as terminated ones; anything less is `Unverifiable`.

    /// Poll cadence while waiting for buffered output inside `drain`.
    const DRAIN_POLL_MS: u64 = 5;

    // Honest scope, part 2:
    //
    // * Durable identities: the supervisor binds the journal's tree id
    //   and ownership epoch before prepare; a direct spawn without
    //   binding synthesizes an identity a supervisor would reject
    //   (visible, not silent). V1Store journaling is future wiring (P25).
    // * Blocking Win32 calls run inline in the async trait methods;
    //   acceptable for tests and short-lived children only.
    // * Output is a TEST-GRADE pull-based bounded log inside `drain`
    //   (no reader threads), not a production streaming design. Not
    //   wired into any production service path.

    pub struct WindowsJobBackend {
        state: Arc<Mutex<WindowsBackendState>>,
    }

    struct WindowsBackendState {
        backend_id: u64,
        next_id: u64,
        trees: BTreeMap<u64, WindowsTree>,
        bound_identity: Option<(String, u64)>,
    }

    struct WindowsTree {
        spec: SpawnSpec,
        durable_tree_id: String,
        ownership_epoch: u64,
        child: JobbedSuspendedChild,
        identity: Option<ProcessOwnerIdentity>,
        resumed: bool,
        terminated: bool,
        stdout_eof: bool,
        stderr_eof: bool,
        exit_frame: bool,
        frames: Vec<OutputFrame>,
        output_bytes: usize,
    }

    impl WindowsJobBackend {
        pub fn new() -> Self {
            Self {
                state: Arc::new(Mutex::new(WindowsBackendState {
                    backend_id: NEXT_BACKEND_ID.fetch_add(1, Ordering::Relaxed),
                    next_id: 1,
                    trees: BTreeMap::new(),
                    bound_identity: None,
                })),
            }
        }

        fn lock(&self) -> MutexGuard<'_, WindowsBackendState> {
            self.state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }
    }

    impl Default for WindowsJobBackend {
        fn default() -> Self {
            Self::new()
        }
    }

    /// Guardian failures mapped into the supervisor error vocabulary. The
    /// distinct refusal (`Unsupported`) stays distinct; everything else
    /// collapses to the same static invalid-state message the fake uses.
    fn spawn_failure(error: GuardianError) -> SupervisorError {
        match error {
            GuardianError::Unsupported(reason) => SupervisorError::Unsupported(reason),
            _ => SupervisorError::InvalidState("windows guardian operation failed"),
        }
    }

    impl WindowsBackendState {
        fn require_backend(&self, backend_id: u64) -> Result<(), SupervisorError> {
            if self.backend_id == backend_id {
                Ok(())
            } else {
                Err(SupervisorError::UnknownHandle)
            }
        }

        fn tree(&self, id: u64) -> Result<&WindowsTree, SupervisorError> {
            self.trees.get(&id).ok_or(SupervisorError::UnknownHandle)
        }

        fn tree_mut(&mut self, id: u64) -> Result<&mut WindowsTree, SupervisorError> {
            self.trees
                .get_mut(&id)
                .ok_or(SupervisorError::UnknownHandle)
        }
    }

    fn push_frame(tree: &mut WindowsTree, frame: OutputFrame) -> Result<(), SupervisorError> {
        if frame.sequence() != tree.frames.len() as u64 {
            return Err(SupervisorError::InvalidState(
                "output sequence is not contiguous",
            ));
        }
        let bytes = frame_bytes(&frame)?;
        if tree.output_bytes.saturating_add(bytes) > tree.spec.output_capacity_bytes {
            return Err(SupervisorError::Backpressure);
        }
        tree.output_bytes += bytes;
        match &frame {
            OutputFrame::Eof { stream, .. } => match stream {
                ProcessOutputStream::Stdout => tree.stdout_eof = true,
                ProcessOutputStream::Stderr => tree.stderr_eof = true,
            },
            OutputFrame::Exit { .. } => tree.exit_frame = true,
            OutputFrame::Data { .. } => {}
        }
        tree.frames.push(frame);
        Ok(())
    }

    /// Pull whatever is currently buffered on the stdio parent ends into
    /// the bounded frame log (test-grade pump: non-blocking, sequence-
    /// checked, capacity-bounded). The final Exit frame lands only once
    /// the primary is settled dead and both streams hit EOF.
    fn pump_tree_output(tree: &mut WindowsTree) -> Result<(), SupervisorError> {
        if !tree.stdout_eof {
            match tree.child.pump_stdout() {
                PipeRead::Data(bytes) => push_data_frame(tree, ProcessOutputStream::Stdout, bytes)?,
                PipeRead::Closed => push_frame(
                    tree,
                    OutputFrame::Eof {
                        sequence: tree.frames.len() as u64,
                        stream: ProcessOutputStream::Stdout,
                    },
                )?,
            }
        }
        if !tree.stderr_eof {
            match tree.child.pump_stderr() {
                PipeRead::Data(bytes) => push_data_frame(tree, ProcessOutputStream::Stderr, bytes)?,
                PipeRead::Closed => push_frame(
                    tree,
                    OutputFrame::Eof {
                        sequence: tree.frames.len() as u64,
                        stream: ProcessOutputStream::Stderr,
                    },
                )?,
            }
        }
        if tree.child.primary_exited() {
            tree.child.settle_primary();
        }
        if let Some(exit) = tree
            .child
            .settled_exit_code()
            .filter(|_| tree.stdout_eof && tree.stderr_eof && !tree.exit_frame)
        {
            push_frame(
                tree,
                OutputFrame::Exit {
                    sequence: tree.frames.len() as u64,
                    exit_code: Some(exit as i32),
                },
            )?;
        }
        Ok(())
    }

    fn push_data_frame(
        tree: &mut WindowsTree,
        stream: ProcessOutputStream,
        bytes: Vec<u8>,
    ) -> Result<(), SupervisorError> {
        if bytes.is_empty() {
            return Ok(());
        }
        let sequence = tree.frames.len() as u64;
        push_frame(
            tree,
            OutputFrame::Data {
                sequence,
                stream,
                data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            },
        )
    }

    fn assemble_page(
        tree: &WindowsTree,
        cursor: u64,
        max_bytes: u32,
    ) -> Result<ProcessReadReply, SupervisorError> {
        let start = usize::try_from(cursor).map_err(|_| SupervisorError::StaleCursor)?;
        if start > tree.frames.len() {
            return Err(SupervisorError::StaleCursor);
        }
        let mut used = 0usize;
        let mut frames = Vec::new();
        for frame in &tree.frames[start..] {
            let bytes = frame_bytes(frame)?;
            if used + bytes > max_bytes as usize {
                if frames.is_empty() {
                    return Err(SupervisorError::PageTooSmall);
                }
                break;
            }
            used += bytes;
            frames.push(frame.clone());
        }
        let next_cursor = cursor + frames.len() as u64;
        let page_exit = frames.last().and_then(exit_code);
        let log_exit = tree.frames.last().and_then(exit_code);
        let terminal = page_exit.is_some()
            || (frames.is_empty() && start == tree.frames.len() && log_exit.is_some());
        Ok(ProcessReadReply {
            frames,
            next_cursor,
            terminal,
            exit_code: terminal.then(|| log_exit.flatten()).flatten(),
        })
    }

    #[async_trait::async_trait]
    impl ProcessTreeBackend for WindowsJobBackend {
        fn bind_durable_identity(&self, tree_id: &str, ownership_epoch: u64) {
            self.lock().bound_identity = Some((tree_id.to_string(), ownership_epoch));
        }

        async fn prepare(&self, spec: SpawnSpec) -> Result<PreparedLaunch, SupervisorError> {
            spec.validate()?;
            let state = self.lock();
            Ok(PreparedLaunch {
                backend_id: state.backend_id,
                spec,
            })
        }

        async fn spawn_suspended(
            &self,
            launch: PreparedLaunch,
        ) -> Result<PreparedChild, SupervisorError> {
            let mut state = self.lock();
            state.require_backend(launch.backend_id)?;
            // The raw spawn builds exactly its own stdio pipes via the
            // handle list; ambient inherited objects cannot be honored.
            if !launch.spec.inherited_objects.is_empty() {
                return Err(SupervisorError::InvalidSpec(
                    "inherited objects are not supported by the windows backend",
                ));
            }
            let child = spawn_suspended_with_job(&RawSpawnSpec {
                executable: &launch.spec.executable,
                arguments: &launch.spec.arguments,
                cwd: Some(&launch.spec.cwd),
                environment: &launch.spec.environment,
            })
            .map_err(spawn_failure)?;
            let id = state.next_id;
            state.next_id += 1;
            // Supervisor-driven runs are always bound before prepare; a
            // direct trait spawn without binding synthesizes an identity a
            // supervisor would reject — visible, never silently accepted.
            let (durable_tree_id, ownership_epoch) = state
                .bound_identity
                .take()
                .unwrap_or_else(|| (format!("windows-job-{}-{}", state.backend_id, id), 1));
            state.trees.insert(
                id,
                WindowsTree {
                    spec: launch.spec,
                    durable_tree_id,
                    ownership_epoch,
                    child,
                    identity: None,
                    resumed: false,
                    terminated: false,
                    stdout_eof: false,
                    stderr_eof: false,
                    exit_frame: false,
                    frames: Vec::new(),
                    output_bytes: 0,
                },
            );
            Ok(PreparedChild {
                backend_id: state.backend_id,
                id,
                kind: PreparedChildKind::WindowsSuspended,
            })
        }

        async fn probe_identity(
            &self,
            child: &PreparedChild,
        ) -> Result<ProcessOwnerIdentity, SupervisorError> {
            let state = self.lock();
            state.require_backend(child.backend_id)?;
            state
                .tree(child.id)?
                .child
                .owner_identity()
                .map_err(spawn_failure)
        }

        async fn persist_identity(
            &self,
            child: &PreparedChild,
            identity: ProcessOwnerIdentity,
        ) -> Result<(), SupervisorError> {
            let mut state = self.lock();
            state.require_backend(child.backend_id)?;
            let tree = state.tree_mut(child.id)?;
            // Same discipline as the fake backend: the identity must be
            // exactly what the backend observed at spawn, persisted once,
            // before any resume.
            let observed = tree.child.owner_identity().map_err(spawn_failure)?;
            if observed != identity || tree.identity.is_some() || tree.resumed {
                return Err(SupervisorError::InvalidState(
                    "identity persistence is invalid",
                ));
            }
            tree.identity = Some(identity);
            Ok(())
        }

        async fn abort_prepared(&self, child: &PreparedChild) -> Result<(), SupervisorError> {
            let mut state = self.lock();
            state.require_backend(child.backend_id)?;
            let tree = state.tree_mut(child.id)?;
            if tree.resumed {
                return Err(SupervisorError::InvalidState("prepared child was resumed"));
            }
            // Terminates the never-resumed child before it executed
            // anything; the record stays for later probing.
            tree.child.abort();
            Ok(())
        }

        async fn resume_once(&self, child: &PreparedChild) -> Result<RunningTree, SupervisorError> {
            let mut state = self.lock();
            state.require_backend(child.backend_id)?;
            let tree = state.tree_mut(child.id)?;
            if tree.identity.is_none() || tree.resumed {
                return Err(SupervisorError::InvalidState(
                    "resume requires one persisted identity",
                ));
            }
            tree.child.resume_once().map_err(spawn_failure)?;
            tree.resumed = true;
            Ok(RunningTree {
                backend_id: child.backend_id,
                id: child.id,
                kind: PreparedChildKind::WindowsSuspended,
            })
        }

        async fn terminate(&self, tree: &RunningTree) -> Result<(), SupervisorError> {
            let mut state = self.lock();
            state.require_backend(tree.backend_id)?;
            let windows = state.tree_mut(tree.id)?;
            if !windows.resumed {
                return Err(SupervisorError::InvalidState("tree was not resumed"));
            }
            windows.child.terminate();
            windows.terminated = true;
            Ok(())
        }

        async fn wait_and_prove(
            &self,
            tree: &RunningTree,
            timeout: Duration,
        ) -> Result<TerminationProofRecord, SupervisorError> {
            let mut state = self.lock();
            state.require_backend(tree.backend_id)?;
            let windows = state.tree_mut(tree.id)?;
            if !windows.resumed {
                return Err(SupervisorError::InvalidState("tree was not resumed"));
            }
            let deadline = Instant::now()
                .checked_add(timeout)
                .ok_or(SupervisorError::InvalidSpec("supervisor deadline overflow"))?;
            // Full-tree death with PID-reuse fencing is the gate — a
            // naturally-exited tree proves the same way as a terminated
            // one; anything less is Unverifiable and blocks writes.
            if !windows.child.wait_all_members(deadline) {
                return Err(SupervisorError::Unverifiable);
            }
            let members = windows
                .child
                .member_pids()
                .map_err(|_| SupervisorError::Unverifiable)?;
            let identity = windows
                .identity
                .clone()
                .ok_or(SupervisorError::InvalidState("identity is missing"))?;
            let observed_boot = BootIdentity::current()
                .map_err(|_| SupervisorError::InvalidState("boot identity is unavailable"))?;
            let job_outcome = if windows.terminated {
                "terminated"
            } else {
                "exited"
            };
            // The exact P01 nine-field envelope (see proof_matches_record
            // for the discipline this must satisfy).
            let proof_identity = serde_json::json!({
                "treeId": windows.durable_tree_id,
                "ownershipEpoch": windows.ownership_epoch,
                "ownerPid": u64::from(identity.pid),
                "ownerStartIdentity": identity.start_identity,
                "ownerBootIdentity": identity.boot_identity.to_string(),
                "ownerPlatformIdentityDigest": identity.platform_identity_digest,
                "migratedObservedBootIdentity": serde_json::Value::Null,
                "observedBootIdentity": observed_boot.to_string(),
                "platformEvidence": { "job": job_outcome, "members": members.len() },
            });
            Ok(TerminationProofRecord {
                proof_id: format!(
                    "proof-{}-{}",
                    windows.durable_tree_id, windows.ownership_epoch
                ),
                tree_id: windows.durable_tree_id.clone(),
                ownership_epoch: windows.ownership_epoch,
                kind: TerminationProofKind::Exit,
                observed_boot_identity: observed_boot,
                proof_identity: proof_identity.clone(),
                proof_identity_digest: r_code_harness_protocol::canonical_input_hash(
                    &proof_identity,
                ),
                recorded_at_ms: SystemTime::UNIX_EPOCH
                    .elapsed()
                    .map(|elapsed| elapsed.as_millis() as i64)
                    .unwrap_or(0),
            })
        }

        async fn subscribe_output(
            &self,
            tree: &RunningTree,
        ) -> Result<OutputSubscription, SupervisorError> {
            let state = self.lock();
            state.require_backend(tree.backend_id)?;
            if !state.tree(tree.id)?.resumed {
                return Err(SupervisorError::InvalidState("tree was not resumed"));
            }
            Ok(OutputSubscription {
                backend_id: tree.backend_id,
                tree_id: tree.id,
            })
        }

        async fn drain(
            &self,
            subscription: &OutputSubscription,
            cursor: u64,
            max_bytes: u32,
            wait: Duration,
        ) -> Result<ProcessReadReply, SupervisorError> {
            if max_bytes == 0
                || max_bytes > PROCESS_READ_MAX_BYTES
                || wait > Duration::from_millis(u64::from(PROCESS_READ_MAX_WAIT_MS))
            {
                return Err(SupervisorError::InvalidSpec("invalid drain bounds"));
            }
            let deadline = Instant::now() + wait;
            loop {
                let page = {
                    let mut state = self.lock();
                    state.require_backend(subscription.backend_id)?;
                    let tree = state.tree_mut(subscription.tree_id)?;
                    pump_tree_output(tree)?;
                    assemble_page(tree, cursor, max_bytes)?
                };
                // Deliver as soon as a frame exists or the log is terminal;
                // otherwise keep pumping until the caller's wait elapses.
                if !page.frames.is_empty() || page.terminal || Instant::now() >= deadline {
                    return Ok(page);
                }
                tokio::time::sleep(Duration::from_millis(DRAIN_POLL_MS)).await;
            }
        }
    }
}

#[cfg(windows)]
pub use windows_backend::WindowsJobBackend;

// P09 — Linux bwrap PID-namespace proof capability (registration only).
//
// The proof primitives live in process_guard::unix::bwrap_proof: one
// --unshare-pid namespace whose PID1 death (observed via the outer bwrap
// pidfd plus the namespace inode's disappearance) proves EVERY descendant
// died, including setsid/double-fork escapees. This registration exposes
// the capability to supervisor consumers; migrating Checks onto the
// sandboxed supervisor path is P20's contract — nothing here activates
// execution, and the P13 discovery gate stays closed on Linux until
// P09+P17 both hold.
#[cfg(target_os = "linux")]
pub use crate::process_guard::unix::bwrap_proof::{
    launch_bwrap_tree, prove_namespace_empty, terminate_tree, wait_monitor, wait_pid1_exit,
    BwrapTreeIdentity, PidFd,
};
