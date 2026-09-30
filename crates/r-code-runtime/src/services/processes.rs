//! Managed interactive processes (host.process.open/read/write/close).
//!
//! Launches pass common authorization against a LaunchCapability resolved
//! from a pinned process profile, and NDJSON-RPC profiles validate every
//! complete outbound frame before it reaches the child. Since P22 nothing here
//! starts a child itself: the material resolves into a `SpawnSpec`, and the
//! only route to a running tree is the supervisor over an injected
//! process-tree backend, which journals Prepared, Suspended, IdentityRecorded
//! and ResumePending *before* it resumes (P22.1). A crash in that window
//! leaves a recoverable record, never an unmanaged process, and an unbound
//! backend or a non-Activated capability is a refusal with zero spawn
//! (INV-07). Handles name run, attempt and tree.

use crate::plugins::router::validate_process_read_page;
use crate::process_guard::{
    BootIdentity, GuardedOwnerIdentity, TerminationProofKind, TerminationProofRecord,
};
use crate::services::authorization::{
    AuthorizationDecision, AuthorizationService, CredentialScope, EffectivePermissions,
    OperationDescriptor, WorkspaceCapability,
};
use crate::services::process_profiles::{
    resolve_profile_effect, FrameValidator, ProcessProfileEffect,
};
use crate::services::process_supervisor::{
    ExecutionWriteProfile, InheritedObject, OutputSubscription, ProcessSupervisor,
    ProcessTreeBackend, RunningTree, SpawnSpec, SupervisorError, SupervisorJournal,
    SupervisorPhase, SupervisorRecord, SupervisorStart, MAX_OUTPUT_BUFFER_BYTES,
};
use crate::services::sandbox::SafetyActivation;
use base64::Engine as _;
use r_code_harness_protocol::process_profile::{ConstraintViolation, ProcessProfileSchema};
use r_code_harness_protocol::services::{OutputBlock, ToolCallError, ToolCallReply};
use r_code_harness_protocol::{
    ProcessOutputFrame, ProcessReadReply, ProcessReadRequest, PROCESS_READ_MAX_BYTES,
    PROCESS_READ_MAX_WAIT_MS,
};
use r_code_kernel::ports::{GenerationToken, ProcessService, ServiceError};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Errors surfaced by the managed process service.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProcessError {
    #[error("authorization denied: {0}")]
    Denied(String),
    #[error("unknown handle {0:?}")]
    UnknownHandle(String),
    #[error("frame rejected: {0}")]
    FrameRejected(String),
    #[error("io failure: {0}")]
    Io(String),
    #[error("spawn material refused: {0}")]
    Spec(String),
    #[error("platform safety capability is not activated: {0}")]
    GateClosed(String),
    #[error("handle scope does not name the supervised record: {0}")]
    HandleScope(String),
    #[error("restart handle is fenced: {0}")]
    Fenced(String),
    #[error("process-tree termination is unverifiable: {0}")]
    Unverifiable(String),
    #[error("supervisor refused: {0}")]
    Supervisor(String),
    #[error("no input channel is bound for tree {0}")]
    NoInputChannel(String),
}

/// The input side a containing backend may expose for one supervised tree.
///
/// The process-tree backend has no write boundary in this wave, so a managed
/// process has no route to a child's standard input. Frames stay validated
/// whole before anything would be forwarded, and the validated write is then
/// refused rather than keeping an unsupervised child alive to accept it
/// (INV-07). Binding a channel is the seam that closes that gap.
#[async_trait::async_trait]
pub trait ProcessInputChannel: Send + Sync {
    async fn write_frame(&self, tree_id: &str, frame: &[u8]) -> Result<(), String>;
}

/// Host-bound identity and seams of one supervised process surface (P22.1).
/// A service built without a binding resolves nothing: no activation verdict,
/// no backend and no attempt identity means no process is ever started.
/// `input` stays `None` for every backend available in this wave.
pub struct ProcessSupervisorBinding {
    pub backend: Arc<dyn ProcessTreeBackend>,
    pub journal: Arc<dyn SupervisorJournal>,
    pub activation: SafetyActivation,
    pub task_id: String,
    pub attempt_id: String,
    pub ownership_epoch: u64,
    pub scratch_root: PathBuf,
    pub output_capacity_bytes: usize,
    pub input: Option<Arc<dyn ProcessInputChannel>>,
}

/// One supervised interactive process, fenced to the record it names.
struct ManagedProcess {
    run_id: String,
    attempt_id: String,
    tree_id: String,
    generation: u64,
    record: SupervisorRecord,
    tree: RunningTree,
    subscription: OutputSubscription,
    owner: GuardedOwnerIdentity,
}

/// The host.process service.
pub struct ManagedProcessService {
    authorization: Arc<AuthorizationService>,
    permissions: EffectivePermissions,
    workspace: WorkspaceCapability,
    validator: Option<FrameValidator>,
    processes: Mutex<HashMap<String, ManagedProcess>>,
    profile_executables: Mutex<HashMap<String, String>>,
    profile_effects: Mutex<HashMap<String, String>>,
    next_handle: AtomicU64,
    binding: Option<ProcessSupervisorBinding>,
}

impl ManagedProcessService {
    /// An unsupervised surface: it refuses every launch, because the only
    /// route to a running tree is the bound supervisor.
    pub fn new(
        authorization: Arc<AuthorizationService>,
        permissions: EffectivePermissions,
        workspace: WorkspaceCapability,
        validator: Option<FrameValidator>,
    ) -> Self {
        Self::build(authorization, permissions, workspace, validator, None)
    }

    /// The supervised surface (P22): the injected backend seam plus the
    /// platform activation verdict decide whether anything can start at all.
    pub fn supervised(
        authorization: Arc<AuthorizationService>,
        permissions: EffectivePermissions,
        workspace: WorkspaceCapability,
        validator: Option<FrameValidator>,
        binding: ProcessSupervisorBinding,
    ) -> Self {
        Self::build(
            authorization,
            permissions,
            workspace,
            validator,
            Some(binding),
        )
    }

    fn build(
        authorization: Arc<AuthorizationService>,
        permissions: EffectivePermissions,
        workspace: WorkspaceCapability,
        validator: Option<FrameValidator>,
        binding: Option<ProcessSupervisorBinding>,
    ) -> Self {
        Self {
            authorization,
            permissions,
            workspace,
            validator,
            processes: Mutex::new(HashMap::new()),
            profile_executables: Mutex::new(HashMap::new()),
            profile_effects: Mutex::new(HashMap::new()),
            next_handle: AtomicU64::new(1),
            binding,
        }
    }

    /// Register the executable a profile name resolves to (host-resolved,
    /// never plugin-supplied).
    pub async fn register_profile_executable(&self, profile: &str, executable: &str) {
        self.profile_executables
            .lock()
            .await
            .insert(profile.to_string(), executable.to_string());
    }

    /// Pin the workspace effect of a profile (host-resolved). An unknown
    /// declaration is refused here, so no launch can inherit a default.
    pub async fn register_profile_effect(
        &self,
        profile: &str,
        declaration: &str,
    ) -> Result<(), ProcessError> {
        ProcessProfileEffect::parse(declaration)
            .map_err(|error| ProcessError::Spec(error.to_string()))?;
        self.profile_effects
            .lock()
            .await
            .insert(profile.to_string(), declaration.to_string());
        Ok(())
    }

    /// The host-registered executable of one profile.
    pub async fn registered_executable(&self, profile: &str) -> Result<String, ProcessError> {
        self.profile_executables
            .lock()
            .await
            .get(profile)
            .cloned()
            .ok_or_else(|| {
                ProcessError::Spec(format!(
                    "profile {profile:?} has no host-registered executable"
                ))
            })
    }

    /// The effect a pinned profile declares, refusing an absent or
    /// contradictory declaration and a profile with no pinned schema.
    pub async fn profile_effect(
        &self,
        profile: &str,
    ) -> Result<ProcessProfileEffect, ProcessError> {
        let pinned = self.pinned_profile()?;
        let declaration = self.profile_effects.lock().await.get(profile).cloned();
        resolve_profile_effect(pinned, declaration.as_deref())
            .map_err(|error| ProcessError::Spec(error.to_string()))
    }

    /// Open a managed process from a profile-pinned executable.
    pub async fn open_profiled(
        &self,
        token: &GenerationToken,
        profile: &str,
        executable: &Path,
        argv: &[String],
        cwd: Option<&str>,
    ) -> Result<String, ProcessError> {
        let descriptor = OperationDescriptor::process_launch(
            &executable.to_string_lossy(),
            argv.to_vec(),
            cwd.map(str::to_string),
        );
        match self.authorization.authorize(
            &descriptor,
            &self.workspace,
            &self.permissions,
            &CredentialScope::default(),
        ) {
            AuthorizationDecision::Allowed => {}
            AuthorizationDecision::RequiresApproval { summary } => {
                return Err(ProcessError::Denied(format!(
                    "approval required: {summary}"
                )));
            }
            AuthorizationDecision::Denied(reason) => {
                return Err(ProcessError::Denied(reason.to_string()));
            }
        }
        let binding = self.supervised_binding()?;
        // The executable only ever comes from the host table, so a caller
        // cannot widen what the pinned profile already declared.
        let registered = self.registered_executable(profile).await?;
        if executable != Path::new(&registered) {
            return Err(ProcessError::Spec(format!(
                "{} is not the executable registered for profile {profile:?}",
                executable.to_string_lossy()
            )));
        }
        // Recovery identity is a launch precondition: resolve it before the
        // supervisor persists a record, prepares a tree or resumes anything.
        let boot_identity = BootIdentity::current()
            .map_err(|_| ProcessError::Io("stable boot identity is unavailable".to_string()))?;
        let effect = self.profile_effect(profile).await?;
        if effect.is_workspace_write() {
            // P24B: checkout-writing execution stays refused until the
            // platform report is Activated AND the launch runs inside the
            // P27 envelope (begin before the single resume, complete after
            // the tree proof). The readiness publishes the granted set —
            // "process.checkoutwrite" is deliberately NOT in it, so this
            // arm is the only path that could ever open it, and only by
            // constructing the envelope first. NoWorkspace and ScratchOnly
            // (which never touch the checkout) are the directly activatable
            // capabilities under an Activated report.
            debug_assert!({
                let applies = crate::services::process_effects::checkout_delta_applies(effect);
                !applies || effect.is_workspace_write()
            });
            return Err(ProcessError::Spec(format!(
                "{} writes into the current checkout: it launches only inside the P27 \
                 effect envelope under an Activated platform report, and no granted \
                 capability opens this arm directly",
                effect.as_str()
            )));
        }
        if !effect.interactive_process_admitted() {
            return Err(ProcessError::Spec(format!(
                "{} is above the interactive ceiling and stays hidden",
                effect.as_str()
            )));
        }
        let start = self.spawn_start(binding, &effect, executable, argv, cwd)?;
        let supervisor = ProcessSupervisor::new(binding.backend.clone(), binding.journal.clone());
        // Only a non-workspace effect reaches here; the match stays total so a
        // wider effect added later cannot silently inherit diagnostics
        // supervision instead of a durable SafeDisabled refusal.
        let write_profile = match effect {
            ProcessProfileEffect::NoWorkspace | ProcessProfileEffect::ScratchOnly => {
                ExecutionWriteProfile::NoWorkspaceDiagnostics
            }
            ProcessProfileEffect::CurrentCheckoutWrite => ExecutionWriteProfile::WriteCapable,
        };
        let run = supervisor
            .start_with_write_profile(start, write_profile)
            .await
            .map_err(supervisor_error)?;
        let record = run.record().clone();
        let subscription = binding
            .backend
            .subscribe_output(run.tree())
            .await
            .map_err(supervisor_error)?;
        let owner = record
            .owner
            .clone()
            .ok_or_else(|| ProcessError::Supervisor("record carries no owner".to_string()))?;
        if owner.boot_identity != boot_identity {
            return Err(ProcessError::Fenced(format!(
                "supervised tree {} belongs to another boot",
                record.tree_id
            )));
        }
        let tree_id = record.tree_id.clone();
        let handle = format!("{}:{}:{}", token.run_id, binding.attempt_id, tree_id);
        self.processes.lock().await.insert(
            handle.clone(),
            ManagedProcess {
                tree_id,
                attempt_id: binding.attempt_id.clone(),
                run_id: token.run_id.clone(),
                generation: token.generation,
                record,
                tree: run.tree().clone(),
                subscription,
                owner: GuardedOwnerIdentity {
                    pid: owner.pid,
                    start_identity: owner.start_identity,
                    boot_nonce: boot_identity.to_string(),
                },
            },
        );
        Ok(handle)
    }

    /// Bounded read of one supervised tree (P22.2): at most `max_bytes`, and
    /// never more than the declared output capacity, with the journaled cursor
    /// advanced exactly once per delivered page. The flag reports that the
    /// bound cut the page short while the stream is still continuing.
    pub async fn read_bounded(
        &self,
        token: &GenerationToken,
        handle: &str,
        cursor: u64,
        max_bytes: u32,
        wait_ms: Option<u32>,
    ) -> Result<(ProcessReadReply, bool), ProcessError> {
        let binding = self.supervised_binding()?;
        if max_bytes == 0
            || max_bytes > PROCESS_READ_MAX_BYTES
            || wait_ms.unwrap_or(0) > PROCESS_READ_MAX_WAIT_MS
            || max_bytes as usize > binding.output_capacity_bytes
        {
            return Err(ProcessError::Spec(
                "read bounds are outside the protocol ceiling or the capacity".to_string(),
            ));
        }
        let request = ProcessReadRequest {
            handle: handle.to_string(),
            cursor,
            max_bytes,
            wait_ms,
        };
        // Scope and drain happen under one lock: a handle cannot be swapped
        // between the fencing check and the page the cursor is read from.
        let mut guard = self.processes.lock().await;
        let process = guard
            .get_mut(&request.handle)
            .ok_or_else(|| ProcessError::UnknownHandle(request.handle.clone()))?;
        check_scope(handle, process, token, &binding.attempt_id)?;
        let page = self
            .drain_one(
                process,
                binding,
                &request,
                Duration::from_millis(u64::from(wait_ms.unwrap_or(0))),
            )
            .await?;
        let used = delivered_bytes(&page)?;
        let truncated = !page.terminal && used == max_bytes as usize;
        Ok((page, truncated))
    }

    /// Write bytes; NDJSON-RPC profiles validate complete frames first.
    pub async fn write_validated(
        &self,
        token: &GenerationToken,
        handle: &str,
        data: Vec<u8>,
    ) -> Result<(), ProcessError> {
        let binding = self.supervised_binding()?;
        let mut guard = self.processes.lock().await;
        let process = guard
            .get_mut(handle)
            .ok_or_else(|| ProcessError::UnknownHandle(handle.to_string()))?;
        check_scope(handle, process, token, &binding.attempt_id)?;
        if data.is_empty() || data.len() > binding.output_capacity_bytes {
            return Err(ProcessError::Spec(format!(
                "a write must be between 1 byte and the declared {} bytes",
                binding.output_capacity_bytes
            )));
        }
        let validator = self.validator.as_ref().ok_or_else(|| {
            ProcessError::Spec("this profile has no pinned frame validator".to_string())
        })?;
        let mut buffer = data.clone();
        validator
            .drain_buffer(&mut buffer)
            .map_err(|violation: ConstraintViolation| {
                ProcessError::FrameRejected(violation.to_string())
            })?;
        // `drain_buffer` consumed every whole frame; whatever remains is a
        // partial frame, refused instead of validated as if it were whole.
        if !buffer.is_empty() {
            return Err(ProcessError::FrameRejected(
                "a trailing partial frame is never validated or forwarded".to_string(),
            ));
        }
        match &binding.input {
            Some(channel) => channel
                .write_frame(&process.tree_id, &data)
                .await
                .map_err(ProcessError::Io),
            None => Err(ProcessError::NoInputChannel(process.tree_id.clone())),
        }
    }

    /// Close a handle: terminate the tree, drain to its terminal frame and
    /// confirm with the backend's proof before the record goes terminal.
    pub async fn close_confirmed(
        &self,
        token: &GenerationToken,
        handle: &str,
    ) -> Result<Option<i32>, ProcessError> {
        let binding = self.supervised_binding()?;
        let mut process = self
            .processes
            .lock()
            .await
            .remove(handle)
            .ok_or_else(|| ProcessError::UnknownHandle(handle.to_string()))?;
        if let Err(error) = check_scope(handle, &process, token, &binding.attempt_id) {
            // Not ours to close; return it to the run that owns it.
            self.processes
                .lock()
                .await
                .insert(handle.to_string(), process);
            return Err(error);
        }
        let journal = binding.journal.as_ref();
        let owner = process
            .record
            .owner
            .as_ref()
            .ok_or_else(|| ProcessError::Supervisor("record carries no owner".to_string()))?;
        if owner.pid != process.owner.pid || owner.start_identity != process.owner.start_identity {
            return Err(ProcessError::Fenced(
                "the supervised owner identity changed under this handle".to_string(),
            ));
        }
        advance(journal, &mut process.record, SupervisorPhase::Draining)?;
        let terminated = tokio::time::timeout(
            CLOSE_CONFIRM_TIMEOUT,
            binding.backend.terminate(&process.tree),
        )
        .await;
        if !matches!(terminated, Ok(Ok(()))) {
            let _ = quarantine(journal, &mut process.record, "termination-failed");
            return Err(ProcessError::Unverifiable(format!(
                "tree {} termination did not complete",
                process.tree_id
            )));
        }
        let request = bounded_close_request(handle, &process.record, binding.output_capacity_bytes);
        let exit = match self.close_drain(binding, &request, &mut process).await {
            Ok(exit) => exit,
            Err(error) => {
                let _ = quarantine(journal, &mut process.record, "output-drain-failed");
                return Err(error);
            }
        };
        advance(journal, &mut process.record, SupervisorPhase::Drained)?;
        let proof = tokio::time::timeout(
            CLOSE_CONFIRM_TIMEOUT,
            binding
                .backend
                .wait_and_prove(&process.tree, CLOSE_CONFIRM_TIMEOUT),
        )
        .await;
        let proof = match proof {
            Ok(Ok(proof)) => proof,
            Ok(Err(_)) | Err(_) => {
                let _ = quarantine(journal, &mut process.record, "termination-proof-failed");
                return Err(ProcessError::Unverifiable(process.tree_id.clone()));
            }
        };
        if !proof_names_this_tree(&proof, &process.record) {
            let _ = quarantine(journal, &mut process.record, "termination-proof-mismatch");
            return Err(ProcessError::Unverifiable(process.tree_id.clone()));
        }
        transition(
            journal,
            &mut process.record,
            SupervisorPhase::ProofAccepted,
            |next| {
                next.proof = Some(proof);
            },
        )?;
        Ok(exit)
    }

    /// P22.3 fence over restart handles. A restart never reattaches to a live
    /// tree: the handle must name this run and attempt, the host must have
    /// advanced the ownership epoch past the record's, and the record must
    /// already be proven terminal. Every other phase is recovered here, which
    /// quarantines a nonterminal orphan so it can never be resumed or written
    /// into by a stale attempt; nothing here starts a process — a restart
    /// re-opens through [`Self::open_profiled`] under the new epoch.
    pub async fn restart_fenced(
        &self,
        token: &GenerationToken,
        handle: &str,
    ) -> Result<(), ProcessError> {
        let binding = self.supervised_binding()?;
        if self.processes.lock().await.contains_key(handle) {
            return Err(ProcessError::Fenced(format!(
                "handle {handle:?} still owns a live tree"
            )));
        }
        let scope = scope_of(handle)
            .ok_or_else(|| ProcessError::HandleScope("handle names no tree".to_string()))?;
        if scope.0 != token.run_id || scope.1 != binding.attempt_id {
            return Err(ProcessError::HandleScope(
                "restart handle belongs to another run or attempt".to_string(),
            ));
        }
        let operation = format!("process-{}", scope.2);
        let record = binding
            .journal
            .load(&operation)
            .map_err(supervisor_error)?
            .ok_or_else(|| ProcessError::UnknownHandle(operation.clone()))?;
        if record.ownership_epoch >= binding.ownership_epoch {
            return Err(ProcessError::Fenced(format!(
                "restart needs an ownership epoch above {}",
                record.ownership_epoch
            )));
        }
        // A proven-dead tree is the only thing a restart may follow. Every
        // other phase is recovered first, which quarantines a nonterminal
        // orphan so no stale attempt can ever resume or write into it.
        if matches!(
            record.phase,
            SupervisorPhase::Completed | SupervisorPhase::ProofAccepted
        ) {
            return Ok(());
        }
        let supervisor = ProcessSupervisor::new(binding.backend.clone(), binding.journal.clone());
        let recovered = supervisor.recover(&operation).map_err(supervisor_error)?;
        Err(ProcessError::Fenced(format!(
            "operation {operation} recovered as {:?}",
            recovered.phase
        )))
    }

    /// The complete, validated spawn material of one launch (P22.1), in the
    /// frozen-material shape of the required-checks spec: a launch that cannot
    /// name its cwd, environment, handle set or output bound is refused before
    /// a supervisor sees anything, so refusal means no process is started.
    fn spawn_start(
        &self,
        binding: &ProcessSupervisorBinding,
        effect: &ProcessProfileEffect,
        executable: &Path,
        argv: &[String],
        cwd: Option<&str>,
    ) -> Result<SupervisorStart, ProcessError> {
        if !(1..=MAX_OUTPUT_BUFFER_BYTES).contains(&binding.output_capacity_bytes) {
            return Err(ProcessError::Spec(format!(
                "output capacity {} is outside 1..={MAX_OUTPUT_BUFFER_BYTES}",
                binding.output_capacity_bytes
            )));
        }
        if binding.attempt_id.trim().is_empty()
            || binding.task_id.trim().is_empty()
            || binding.ownership_epoch == 0
        {
            return Err(ProcessError::Spec(
                "the supervised attempt identity is incomplete".to_string(),
            ));
        }
        let spec = SpawnSpec {
            executable: executable.to_path_buf(),
            arguments: argv.to_vec(),
            cwd: self.scratch_cwd(effect, cwd, &binding.scratch_root)?,
            environment: self.profile_environment()?,
            // Ambient inheritance is disabled: this is the complete set, and
            // it is empty because the containing backend owns its own stdio.
            inherited_objects: Vec::<InheritedObject>::new(),
            output_capacity_bytes: binding.output_capacity_bytes,
        };
        spec.validate()
            .map_err(|error| ProcessError::Spec(error.to_string()))?;
        // The sequence is consumed only by a launch that got this far, so a
        // refusal cannot burn identities. The operation name carries the
        // ownership epoch as well as the attempt: a fenced restart runs under a
        // higher epoch, and without it the relaunched tree would collide with
        // the journal record of the tree it replaced, so no restarted process
        // could ever start.
        let sequence = self.next_handle.fetch_add(1, Ordering::SeqCst);
        let tree_id = format!(
            "tree-{}-{}-{sequence}",
            binding.attempt_id, binding.ownership_epoch
        );
        Ok(SupervisorStart {
            operation_id: format!("process-{tree_id}"),
            task_id: binding.task_id.clone(),
            ownership_epoch: binding.ownership_epoch,
            tree_id,
            spec,
        })
    }

    /// A non-workspace launch may only name a cwd inside the host's scratch
    /// root, never the checked-out workspace and never a `.git` component
    /// (INV-06).
    fn scratch_cwd(
        &self,
        effect: &ProcessProfileEffect,
        cwd: Option<&str>,
        scratch_root: &Path,
    ) -> Result<PathBuf, ProcessError> {
        if scratch_root.as_os_str().is_empty() || !scratch_root.is_absolute() {
            return Err(ProcessError::Spec(
                "no absolute scratch root is bound for a non-workspace profile".to_string(),
            ));
        }
        let requested = cwd
            .map(PathBuf::from)
            .unwrap_or_else(|| scratch_root.to_path_buf());
        if !requested.is_absolute() {
            return Err(ProcessError::Spec(format!(
                "working directory {} is not absolute",
                requested.display()
            )));
        }
        let resolved = canonical_str(&requested);
        let scratch = canonical_str(scratch_root);
        if resolved != scratch && !resolved.starts_with(&scratch) {
            return Err(ProcessError::Spec(format!(
                "working directory {resolved} escapes the scratch root {scratch}"
            )));
        }
        if *effect == ProcessProfileEffect::NoWorkspace && resolved != scratch {
            return Err(ProcessError::Spec(
                "a no-workspace profile may only run at the scratch root".to_string(),
            ));
        }
        if visible_git(&resolved) {
            return Err(ProcessError::Spec(format!(
                "working directory {resolved} would make the git directory visible"
            )));
        }
        let root = match &self.workspace {
            WorkspaceCapability::ReadOnly { root } | WorkspaceCapability::WriteWithin { root } => {
                Some(canonical_str(Path::new(root)))
            }
            WorkspaceCapability::Unrestricted => None,
        };
        if root.is_some_and(|root| resolved == root || resolved.starts_with(&root)) {
            return Err(ProcessError::Spec(format!(
                "working directory {resolved} is the checked-out workspace"
            )));
        }
        Ok(PathBuf::from(resolved))
    }

    /// The child sees a cleared parent environment plus exactly what its
    /// pinned profile declares; a declared name that is not an uppercase
    /// literal is refused instead of quietly dropped.
    fn profile_environment(&self) -> Result<BTreeMap<String, String>, ProcessError> {
        let mut environment = BTreeMap::new();
        for name in &self.pinned_profile()?.env {
            if name != &name.to_ascii_uppercase() {
                return Err(ProcessError::Spec(format!(
                    "profile environment name {name:?} is not an uppercase literal"
                )));
            }
            if let Ok(value) = std::env::var(name) {
                environment.insert(name.clone(), value);
            }
        }
        Ok(environment)
    }

    fn pinned_profile(&self) -> Result<&ProcessProfileSchema, ProcessError> {
        self.validator
            .as_ref()
            .map(FrameValidator::profile)
            .ok_or_else(|| ProcessError::Spec("this profile has no pinned schema".to_string()))
    }

    /// One bounded page of one already-fenced entry, validated by the same
    /// wire contract the router applies, with the journaled cursor advanced
    /// exactly once.
    async fn drain_one(
        &self,
        process: &mut ManagedProcess,
        binding: &ProcessSupervisorBinding,
        request: &ProcessReadRequest,
        wait: Duration,
    ) -> Result<ProcessReadReply, ProcessError> {
        if request.cursor < process.record.output_cursor {
            return Err(ProcessError::HandleScope(
                "output cursor is behind the supervised record".to_string(),
            ));
        }
        let page = binding
            .backend
            .drain(
                &process.subscription,
                request.cursor,
                request.max_bytes,
                wait,
            )
            .await
            .map_err(supervisor_error)?;
        validate_process_read_page(request, &page).map_err(ProcessError::Supervisor)?;
        if page.next_cursor > process.record.output_cursor {
            transition(
                binding.journal.as_ref(),
                &mut process.record,
                SupervisorPhase::Draining,
                |next| next.output_cursor = page.next_cursor,
            )?;
        }
        Ok(page)
    }

    /// Drain after termination until the terminal frame, bounded by the close
    /// deadline. A tree that never delivers one cannot be confirmed closed.
    async fn close_drain(
        &self,
        binding: &ProcessSupervisorBinding,
        request: &ProcessReadRequest,
        process: &mut ManagedProcess,
    ) -> Result<Option<i32>, ProcessError> {
        let deadline = Instant::now() + CLOSE_CONFIRM_TIMEOUT;
        let mut cursor = request.cursor;
        loop {
            let page = self
                .drain_one(
                    process,
                    binding,
                    &ProcessReadRequest {
                        cursor,
                        ..request.clone()
                    },
                    Duration::ZERO,
                )
                .await?;
            cursor = page.next_cursor;
            if page.terminal {
                return Ok(page.exit_code);
            }
            if Instant::now() >= deadline {
                return Err(ProcessError::Unverifiable(format!(
                    "tree {} never produced a terminal frame",
                    process.tree_id
                )));
            }
            tokio::task::yield_now().await;
        }
    }

    /// The one supervisor surface a launch may use; unbound or unactivated is
    /// a refusal with no process started.
    fn supervised_binding(&self) -> Result<&ProcessSupervisorBinding, ProcessError> {
        let binding = self.binding.as_ref().ok_or_else(|| {
            ProcessError::GateClosed("no process-tree backend is bound for this boot".to_string())
        })?;
        match &binding.activation {
            SafetyActivation::Activated { .. } => Ok(binding),
            SafetyActivation::NotActivated { reason, status } => {
                Err(ProcessError::GateClosed(format!(
                    "activation is {reason} (status {})",
                    status.clone().unwrap_or_else(|| "unknown".to_string())
                )))
            }
        }
    }
}

/// A supervised handle names run, attempt and tree (acceptance ②).
fn scope_of(handle: &str) -> Option<(&str, &str, &str)> {
    let mut parts = handle.split(':');
    Some((parts.next()?, parts.next()?, parts.next()?))
}

/// Refuse any operation whose handle does not name the record it points at.
fn check_scope(
    handle: &str,
    process: &ManagedProcess,
    token: &GenerationToken,
    attempt_id: &str,
) -> Result<(), ProcessError> {
    let scope = scope_of(handle)
        .ok_or_else(|| ProcessError::HandleScope("handle names no tree".to_string()))?;
    if scope.0 != process.run_id || scope.1 != process.attempt_id || scope.2 != process.tree_id {
        return Err(ProcessError::UnknownHandle(handle.to_string()));
    }
    if scope.0 != token.run_id || scope.1 != attempt_id {
        return Err(ProcessError::HandleScope(
            "handle names another run or attempt".to_string(),
        ));
    }
    if token.generation != process.generation {
        return Err(ProcessError::Fenced(
            "the run generation no longer matches the supervised record".to_string(),
        ));
    }
    Ok(())
}

/// The backend's proof must name exactly the tree this record owns: exit
/// kind, tree id, ownership epoch, the owner's boot identity and a
/// self-consistent identity digest. Anything else is an unverifiable close,
/// never a terminal journal transition.
fn proof_names_this_tree(proof: &TerminationProofRecord, record: &SupervisorRecord) -> bool {
    let Some(owner) = record.owner.as_ref() else {
        return false;
    };
    proof.kind == TerminationProofKind::Exit
        && !proof.proof_id.trim().is_empty()
        && proof.tree_id == record.tree_id
        && proof.ownership_epoch == record.ownership_epoch
        && proof.observed_boot_identity == owner.boot_identity
        && proof.proof_identity_digest
            == r_code_harness_protocol::canonical_input_hash(&proof.proof_identity)
}

/// The bytes a page actually delivered, for the truncation report.
fn delivered_bytes(page: &ProcessReadReply) -> Result<usize, ProcessError> {
    let mut bytes = 0usize;
    for frame in &page.frames {
        if let ProcessOutputFrame::Data { data_base64, .. } = frame {
            bytes += base64::engine::general_purpose::STANDARD
                .decode(data_base64)
                .map_err(|_| ProcessError::Supervisor("output data is not base64".to_string()))?
                .len();
        }
    }
    Ok(bytes)
}

/// The close drain reuses the read contract at the declared capacity, so a
/// confirmation never asks the backend for more than the tree may buffer.
fn bounded_close_request(
    handle: &str,
    record: &SupervisorRecord,
    capacity: usize,
) -> ProcessReadRequest {
    ProcessReadRequest {
        handle: handle.to_string(),
        cursor: record.output_cursor,
        max_bytes: u32::try_from(capacity.min(PROCESS_READ_MAX_BYTES as usize))
            .unwrap_or(PROCESS_READ_MAX_BYTES),
        wait_ms: None,
    }
}

/// One monotonic journal transition: the record is replaced only if it still
/// matches the revision this handle last observed, so each phase lands exactly
/// once and a stale handle can never rewrite history.
fn transition(
    journal: &dyn SupervisorJournal,
    record: &mut SupervisorRecord,
    phase: SupervisorPhase,
    amend: impl FnOnce(&mut SupervisorRecord),
) -> Result<(), ProcessError> {
    let mut next = record.clone();
    next.phase = phase;
    next.revision += 1;
    amend(&mut next);
    *record = journal
        .compare_and_swap(record, next)
        .map_err(supervisor_error)?;
    Ok(())
}

fn advance(
    journal: &dyn SupervisorJournal,
    record: &mut SupervisorRecord,
    phase: SupervisorPhase,
) -> Result<(), ProcessError> {
    transition(journal, record, phase, |_| {})
}

fn quarantine(
    journal: &dyn SupervisorJournal,
    record: &mut SupervisorRecord,
    reason: &str,
) -> Result<(), ProcessError> {
    transition(journal, record, SupervisorPhase::Quarantined, |next| {
        next.quarantine_reason = Some(reason.to_string())
    })
}

fn supervisor_error(error: SupervisorError) -> ProcessError {
    match error {
        SupervisorError::InvalidSpec(reason) => ProcessError::Spec(reason.to_string()),
        SupervisorError::UnknownHandle => {
            ProcessError::HandleScope("the backend does not know this tree".to_string())
        }
        SupervisorError::Unverifiable => {
            ProcessError::Unverifiable("the backend cannot prove full-tree death".to_string())
        }
        SupervisorError::SafeDisabled(reason) | SupervisorError::Unsupported(reason) => {
            ProcessError::GateClosed(reason)
        }
        other => ProcessError::Supervisor(other.to_string()),
    }
}

fn canonical_str(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_string()
}

fn visible_git(value: &str) -> bool {
    value
        .split(['/', '\\'])
        .any(|component| component.eq_ignore_ascii_case(".git"))
}

#[async_trait::async_trait]
impl ProcessService for ManagedProcessService {
    async fn open(
        &self,
        token: GenerationToken,
        profile: &str,
        arguments: Vec<String>,
        cwd: Option<String>,
    ) -> Result<String, ServiceError> {
        // The profile name resolves to an executable through the registered
        // host-resolved table; the launch itself still passes authorization.
        let executable = self.registered_executable(profile).await?;
        self.open_profiled(
            &token,
            profile,
            Path::new(&executable),
            &arguments,
            cwd.as_deref(),
        )
        .await
        .map_err(ServiceError::from)
    }

    async fn read(
        &self,
        token: GenerationToken,
        request: ProcessReadRequest,
    ) -> Result<ProcessReadReply, ServiceError> {
        // Truncation stays visible on the wire as a cursor that advanced and a
        // page that is not terminal; the bound itself is enforced inside.
        let (page, _truncated) = self
            .read_bounded(
                &token,
                &request.handle,
                request.cursor,
                request.max_bytes,
                request.wait_ms,
            )
            .await
            .map_err(ServiceError::from)?;
        Ok(page)
    }

    async fn write(
        &self,
        token: GenerationToken,
        handle: &str,
        data: Vec<u8>,
    ) -> Result<(), ServiceError> {
        self.write_validated(&token, handle, data)
            .await
            .map_err(ServiceError::from)
    }

    async fn close(
        &self,
        token: GenerationToken,
        handle: &str,
    ) -> Result<Option<i32>, ServiceError> {
        self.close_confirmed(&token, handle)
            .await
            .map_err(ServiceError::from)
    }
}

impl From<ProcessError> for ServiceError {
    fn from(error: ProcessError) -> Self {
        ServiceError::Failure(error.to_string())
    }
}

/// A ToolCallReply-shaped error for router mapping.
pub fn process_error_reply(error: &ProcessError) -> ToolCallReply {
    ToolCallReply {
        output: vec![OutputBlock::Text {
            text: String::new(),
        }],
        error: Some(ToolCallError {
            code: "process-error".into(),
            message: error.to_string(),
            denied_by: None,
        }),
    }
}

/// How long close confirmation waits before surfacing unverifiable.
pub const CLOSE_CONFIRM_TIMEOUT: Duration = Duration::from_secs(5);
