//! P22 — the interactive Process service runs only on the process supervisor.
//!
//! Every claim here is proven against the real `ManagedProcessService` over an
//! injected `ProcessTreeBackend`, the reference journal and a recording input
//! channel, because P22's contract is about routes: the only launch is the
//! supervisor's, it is journaled before it resumes, and its handle scopes to
//! one run/attempt/tree. Each "nothing happened" assertion carries a control
//! arm showing that the same call would have taken effect, so a vacuous
//! refusal cannot pass. The honest production activation verdict on this host
//! is `Unsupported`, so the gate-closed arm is paired with an `Activated`
//! binding instead of being stubbed away (INV-07).

use base64::Engine as _;
use r_code_harness_protocol::process_profile::{HostBindings, ProcessProfileSchema};
use r_code_harness_protocol::{
    ProcessOutputFrame, ProcessOutputStream, ProcessReadReply, PROCESS_READ_MAX_BYTES,
    PROCESS_READ_MAX_WAIT_MS,
};
use r_code_kernel::ports::GenerationToken;
use r_code_runtime::process_guard::{
    BootIdentity, ProcessOwnerIdentity, TerminationProofKind, TerminationProofRecord,
};
use r_code_runtime::services::authorization::{
    AuthorizationService, EffectivePermissions, WorkspaceCapability,
};
use r_code_runtime::services::launch_profiles::{install_profile_capability, ProfileSource};
use r_code_runtime::services::process_profiles::FrameValidator;
use r_code_runtime::services::process_profiles::ProcessProfileEffect;
use r_code_runtime::services::process_supervisor::{
    DeterministicFakeBackend, DeterministicSupervisorJournal, JournalFaultPoint,
    OutputSubscription, PrepareDisposition, PreparedChild, PreparedLaunch, ProcessSupervisor,
    ProcessTreeBackend, RunningTree, SpawnSpec, SupervisorError, SupervisorJournal,
    SupervisorPhase, SupervisorRecord, MAX_OUTPUT_BUFFER_BYTES,
};
use r_code_runtime::services::processes::{
    ManagedProcessService, ProcessError, ProcessInputChannel, ProcessSupervisorBinding,
};
use r_code_runtime::services::sandbox::{
    current_platform_material, platform_activation_gate, SafetyActivation, SafetyStatus,
};
use r_code_store::v1::V1Store;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const HELPER: &str = env!("CARGO_BIN_EXE_harness-test-helper");
const PROFILE: &str = "app-server";
const ATTEMPT: &str = "attempt-s22";
const TASK: &str = "task-s22";
const EPOCH: u64 = 6;
const CAPACITY: usize = 2048;
const CHECKOUT: &str = "D:/work";

fn token(run: &str) -> GenerationToken {
    GenerationToken {
        run_id: run.to_string(),
        generation: 1,
    }
}

fn boot() -> BootIdentity {
    BootIdentity::current().expect("authoritative boot identity on this host")
}

fn owner() -> ProcessOwnerIdentity {
    ProcessOwnerIdentity::new(43_001, 88_001, boot(), json!({"native": "s22-owner"}))
        .expect("valid owner identity")
}

fn canonical(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_string()
}

fn data(sequence: u64, bytes: &[u8]) -> ProcessOutputFrame {
    ProcessOutputFrame::Data {
        sequence,
        stream: ProcessOutputStream::Stdout,
        data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
    }
}

fn exit_frame(sequence: u64, code: Option<i32>) -> ProcessOutputFrame {
    ProcessOutputFrame::Exit {
        sequence,
        exit_code: code,
    }
}

fn frame(method: &str, params: serde_json::Value) -> Vec<u8> {
    format!(
        "{}\n",
        json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
    )
    .into_bytes()
}

/// A profile whose frame rules cannot reach a checkout, so a non-workspace
/// effect is the only honest declaration for it.
fn scratch_profile() -> ProcessProfileSchema {
    serde_json::from_value(json!({
        "name": "s22-scratch",
        "framing": "ndjson-rpc",
        "methods": [
            {"name": "harness.start", "params": []},
            {"name": "session.start", "params": [
                {"pointer": "/run", "constraint": {"kind": "bound-to", "value": "run-id"}},
                {"pointer": "/mode", "constraint": {"kind": "enum", "values": ["fast", "slow"]}},
                {"pointer": "/token", "constraint": {"kind": "must-be-absent"}}
            ]}
        ],
        "env": ["PATH"]
    }))
    .expect("scratch profile")
}

/// The same frame rules plus one method bound below the workspace root:
/// structurally the widest effect, which P22 must never hand out.
fn checkout_profile() -> ProcessProfileSchema {
    let mut schema = scratch_profile();
    schema.name = "s22-checkout".to_string();
    schema.methods.push(
        serde_json::from_value(json!({
            "name": "file.write",
            "params": [{"pointer": "/path", "constraint": {"kind": "within-workspace"}}]
        }))
        .expect("checkout method"),
    );
    schema
}

fn validator(run: &str, schema: ProcessProfileSchema) -> FrameValidator {
    FrameValidator::new(
        schema,
        HostBindings {
            workspace_root: CHECKOUT.into(),
            task_id: TASK.into(),
            run_id: run.into(),
            attempt_id: ATTEMPT.into(),
            permission_ceiling: "full".into(),
        },
    )
}

/// The launch capability covering exactly the fixture executable.
fn authorization() -> AuthorizationService {
    let mut service = AuthorizationService::new();
    install_profile_capability(
        &mut service,
        &ProfileSource {
            harness_id: "fixture.harness".into(),
            package_digest: "sha".into(),
            profile_name: PROFILE.into(),
        },
        vec![HELPER.to_string()],
        None,
        false,
        vec![],
    );
    service
}

fn activated() -> SafetyActivation {
    SafetyActivation::Activated {
        report_id: "s22-activated-stub".into(),
    }
}

/// The honest verdict of this host's production gate, with the precondition
/// that makes it a refusal rather than an accident: the platform material is
/// `Unsupported`, and `Unsupported` must never present itself as `Activated`.
fn production_verdict(store: &V1Store) -> SafetyActivation {
    let material = current_platform_material(boot().as_str());
    assert!(
        matches!(material.status, SafetyStatus::Unsupported { .. }),
        "INV-07: this wave must report the platform honestly"
    );
    platform_activation_gate(store, boot().as_str())
}

/// What the injected backend actually saw: the only evidence of a launch.
#[derive(Clone, Debug, Default)]
struct Observation {
    prepared: Vec<SpawnSpec>,
    resumed: Vec<RunningTree>,
    terminated: Vec<RunningTree>,
    proven: Vec<RunningTree>,
    at_resume: Vec<Option<SupervisorRecord>>,
    /// Tree ids the supervisor bound before each prepare, in order.
    bound: Vec<String>,
}

/// The deterministic fake plus the seams a service never exposes: captures of
/// the spawn material and of the journal state at the resume boundary.
#[derive(Clone)]
struct Backend {
    inner: DeterministicFakeBackend,
    journal: DeterministicSupervisorJournal,
    seen: Arc<Mutex<Observation>>,
    fault_resume: Arc<AtomicBool>,
    oversize_pages: Arc<AtomicBool>,
}

impl Backend {
    fn new(journal: &DeterministicSupervisorJournal) -> Self {
        Self {
            inner: DeterministicFakeBackend::default(),
            journal: journal.clone(),
            seen: Arc::new(Mutex::new(Observation::default())),
            fault_resume: Arc::new(AtomicBool::new(false)),
            oversize_pages: Arc::new(AtomicBool::new(false)),
        }
    }

    fn observe(&self) -> Observation {
        self.seen.lock().unwrap().clone()
    }

    fn arm_identity(&self, identity: ProcessOwnerIdentity) {
        self.inner.set_next_identity(identity);
    }

    fn fault_resume_once(&self) {
        self.fault_resume.store(true, Ordering::SeqCst);
    }

    /// Arm a backend that hands the service a page larger than it asked for.
    fn deliver_oversize_pages(&self) {
        self.oversize_pages.store(true, Ordering::SeqCst);
    }

    fn emit(&self, tree: &RunningTree, frames: &[ProcessOutputFrame]) {
        for frame in frames {
            self.inner.push_output(tree, frame.clone()).expect("emit");
        }
    }

    fn set_proof(&self, tree: &RunningTree, proof: Option<TerminationProofRecord>) {
        self.inner
            .set_termination_proof(tree, proof)
            .expect("proof armed");
    }
}

#[async_trait::async_trait]
impl ProcessTreeBackend for Backend {
    /// The supervisor binds the durable identity it just journaled before it
    /// prepares, so this is where the test learns which operation the record
    /// about to be resumed belongs to.
    fn bind_durable_identity(&self, tree_id: &str, ownership_epoch: u64) {
        let _ = ownership_epoch;
        self.seen.lock().unwrap().bound.push(tree_id.to_string());
    }

    async fn prepare(&self, spec: SpawnSpec) -> Result<PreparedLaunch, SupervisorError> {
        self.seen.lock().unwrap().prepared.push(spec.clone());
        self.inner.prepare(spec).await
    }

    async fn spawn_suspended(
        &self,
        launch: PreparedLaunch,
    ) -> Result<PreparedChild, SupervisorError> {
        self.inner.spawn_suspended(launch).await
    }

    async fn probe_identity(
        &self,
        child: &PreparedChild,
    ) -> Result<ProcessOwnerIdentity, SupervisorError> {
        self.inner.probe_identity(child).await
    }

    async fn persist_identity(
        &self,
        child: &PreparedChild,
        identity: ProcessOwnerIdentity,
    ) -> Result<(), SupervisorError> {
        self.inner.persist_identity(child, identity).await
    }

    async fn abort_prepared(&self, child: &PreparedChild) -> Result<(), SupervisorError> {
        self.inner.abort_prepared(child).await
    }

    async fn resume_once(&self, child: &PreparedChild) -> Result<RunningTree, SupervisorError> {
        // Snapshot the durable record at the instant the backend is asked to
        // resume: a journal write that followed resume would read `None` here.
        let bound = {
            let mut seen = self.seen.lock().unwrap();
            if seen.bound.is_empty() {
                None
            } else {
                Some(seen.bound.remove(0))
            }
        };
        let record = match bound {
            Some(tree_id) => self
                .journal
                .load(&format!("process-{tree_id}"))
                .expect("journal readable"),
            None => None,
        };
        self.seen.lock().unwrap().at_resume.push(record);
        if self.fault_resume.swap(false, Ordering::SeqCst) {
            return Err(SupervisorError::InvalidState("injected resume fault"));
        }
        let tree = self.inner.resume_once(child).await?;
        self.seen.lock().unwrap().resumed.push(tree.clone());
        Ok(tree)
    }

    async fn terminate(&self, tree: &RunningTree) -> Result<(), SupervisorError> {
        self.seen.lock().unwrap().terminated.push(tree.clone());
        self.inner.terminate(tree).await
    }

    async fn wait_and_prove(
        &self,
        tree: &RunningTree,
        timeout: Duration,
    ) -> Result<TerminationProofRecord, SupervisorError> {
        self.seen.lock().unwrap().proven.push(tree.clone());
        self.inner.wait_and_prove(tree, timeout).await
    }

    async fn subscribe_output(
        &self,
        tree: &RunningTree,
    ) -> Result<OutputSubscription, SupervisorError> {
        self.inner.subscribe_output(tree).await
    }

    async fn drain(
        &self,
        subscription: &OutputSubscription,
        cursor: u64,
        max_bytes: u32,
        wait: Duration,
    ) -> Result<ProcessReadReply, SupervisorError> {
        let mut page = self
            .inner
            .drain(subscription, cursor, max_bytes, wait)
            .await?;
        // A backend that ignores the requested bound stays sequence-coherent,
        // so only the service's own wire validation can catch the extra bytes.
        if self.oversize_pages.load(Ordering::SeqCst) {
            if let Some(ProcessOutputFrame::Data {
                sequence,
                stream,
                data_base64,
            }) = page.frames.last().cloned()
            {
                page.frames.push(ProcessOutputFrame::Data {
                    sequence: sequence + 1,
                    stream,
                    data_base64,
                });
                page.next_cursor += 1;
            }
        }
        Ok(page)
    }
}

/// One forwarded write: the tree it names and its exact bytes.
type Forwarded = (String, Vec<u8>);

/// One refused write: why it is refused, its bytes, and the refusal shape that
/// proves the frame never reached validation as a whole frame.
type Refusal = (&'static str, Vec<u8>, fn(&ProcessError) -> bool);

/// The reference journal plus the only evidence that matters for P22.1: which
/// operations were persisted, and in which order the phases landed.
#[derive(Clone)]
struct RecordingJournal {
    inner: DeterministicSupervisorJournal,
    prepared: Arc<Mutex<Vec<String>>>,
    phases: Arc<Mutex<Vec<(String, SupervisorPhase)>>>,
}

impl RecordingJournal {
    fn new(inner: DeterministicSupervisorJournal) -> Self {
        Self {
            inner,
            prepared: Arc::new(Mutex::new(Vec::new())),
            phases: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Operations the journal was asked to persist, in call order.
    fn persisted(&self) -> Vec<String> {
        self.prepared.lock().unwrap().clone()
    }

    /// Every phase transition the journal accepted, oldest first.
    fn history(&self) -> Vec<(String, SupervisorPhase)> {
        self.phases.lock().unwrap().clone()
    }
}

impl SupervisorJournal for RecordingJournal {
    fn load(&self, operation_id: &str) -> Result<Option<SupervisorRecord>, SupervisorError> {
        self.inner.load(operation_id)
    }

    fn prepare(&self, record: SupervisorRecord) -> Result<PrepareDisposition, SupervisorError> {
        let disposition = self.inner.prepare(record.clone())?;
        self.prepared.lock().unwrap().push(record.operation_id);
        Ok(disposition)
    }

    fn compare_and_swap(
        &self,
        expected: &SupervisorRecord,
        next: SupervisorRecord,
    ) -> Result<SupervisorRecord, SupervisorError> {
        let outcome = self.inner.compare_and_swap(expected, next.clone())?;
        self.phases
            .lock()
            .unwrap()
            .push((next.operation_id, next.phase));
        Ok(outcome)
    }
}

/// The only seam that can carry a validated frame toward a child in this wave.
#[derive(Clone, Default)]
struct InputSink {
    written: Arc<Mutex<Vec<Forwarded>>>,
    refuse: Arc<AtomicBool>,
}

impl InputSink {
    fn frames(&self) -> Vec<Forwarded> {
        self.written.lock().unwrap().clone()
    }

    /// A child whose standard input is already gone: a write must surface as
    /// an io failure, never as a swallowed success.
    fn refuse_writes(&self) {
        self.refuse.store(true, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl ProcessInputChannel for InputSink {
    async fn write_frame(&self, tree_id: &str, frame: &[u8]) -> Result<(), String> {
        if self.refuse.load(Ordering::SeqCst) {
            return Err("child stdin is closed".to_string());
        }
        self.written
            .lock()
            .unwrap()
            .push((tree_id.to_string(), frame.to_vec()));
        Ok(())
    }
}

/// Binding knobs a refusal arm varies, one at a time, from the launch shape
/// that is known to work.
#[derive(Clone)]
struct Options {
    activation: SafetyActivation,
    schema: Option<ProcessProfileSchema>,
    bind_input: bool,
    capacity: usize,
    attempt: String,
    epoch: u64,
    workspace: WorkspaceCapability,
}

impl Options {
    /// A supervised, admissible, writable surface: everything a launch needs.
    fn interactive() -> Self {
        Self {
            activation: activated(),
            schema: Some(scratch_profile()),
            bind_input: true,
            capacity: CAPACITY,
            attempt: ATTEMPT.into(),
            epoch: EPOCH,
            workspace: WorkspaceCapability::Unrestricted,
        }
    }
}

/// One interactive service over one bound supervisor seam.
struct Harness {
    service: ManagedProcessService,
    backend: Backend,
    journal: RecordingJournal,
    records: DeterministicSupervisorJournal,
    input: InputSink,
    scratch: PathBuf,
    _temp: Option<tempfile::TempDir>,
}

impl Harness {
    fn build(options: Options) -> Self {
        let temp = tempfile::tempdir().expect("scratch root");
        let scratch = temp.path().to_path_buf();
        Self::construct(
            options,
            DeterministicSupervisorJournal::default(),
            scratch,
            Some(temp),
        )
    }

    /// A second service over the SAME durable journal and scratch root: the
    /// shape of a host that resumed one attempt under a new ownership epoch.
    fn attach(options: Options, records: &DeterministicSupervisorJournal, scratch: &Path) -> Self {
        Self::construct(options, records.clone(), scratch.to_path_buf(), None)
    }

    fn construct(
        options: Options,
        records: DeterministicSupervisorJournal,
        scratch: PathBuf,
        temp: Option<tempfile::TempDir>,
    ) -> Self {
        let journal = RecordingJournal::new(records.clone());
        let backend = Backend::new(&records);
        let input = InputSink::default();
        let sink: Arc<dyn ProcessInputChannel> = Arc::new(input.clone());
        let root = scratch.clone();
        let binding = ProcessSupervisorBinding {
            backend: Arc::new(backend.clone()),
            journal: Arc::new(journal.clone()),
            activation: options.activation,
            task_id: TASK.into(),
            attempt_id: options.attempt,
            ownership_epoch: options.epoch,
            scratch_root: scratch,
            output_capacity_bytes: options.capacity,
            input: options.bind_input.then_some(sink),
        };
        let pinned = options.schema.map(|schema| validator("run-1", schema));
        let service = ManagedProcessService::supervised(
            Arc::new(authorization()),
            EffectivePermissions::full(),
            options.workspace,
            pinned,
            binding,
        );
        Self {
            service,
            backend,
            journal,
            records,
            input,
            scratch: root,
            _temp: temp,
        }
    }

    /// Supervised and admissible, with an input channel bound.
    async fn interactive() -> Self {
        let harness = Self::build(Options::interactive());
        harness.register(PROFILE, HELPER).await;
        harness
            .declare(ProcessProfileEffect::ScratchOnly.as_str())
            .await
            .expect("admissible declaration");
        harness
    }

    async fn register(&self, profile: &str, executable: &str) {
        self.service
            .register_profile_executable(profile, executable)
            .await;
    }

    async fn declare(&self, declaration: &str) -> Result<(), ProcessError> {
        self.service
            .register_profile_effect(PROFILE, declaration)
            .await
    }

    async fn open(&self, run: &str) -> Result<String, ProcessError> {
        self.open_at(run, None).await
    }

    async fn open_at(&self, run: &str, cwd: Option<&str>) -> Result<String, ProcessError> {
        self.backend.arm_identity(owner());
        self.service
            .open_profiled(
                &token(run),
                PROFILE,
                Path::new(HELPER),
                &["serve".to_string()],
                cwd,
            )
            .await
    }

    /// The journal record a handle names, read back through the bound journal.
    fn record(&self, handle: &str) -> SupervisorRecord {
        self.records
            .load(&Self::operation(handle))
            .expect("journal readable")
            .expect("record persisted")
    }

    fn operation(handle: &str) -> String {
        format!(
            "process-{}",
            handle.split(':').nth(2).expect("run:attempt:tree handle")
        )
    }

    /// A refusal must not even reach the journal: no operation was persisted.
    fn assert_nothing_persisted(&self) {
        assert!(
            self.journal.persisted().is_empty(),
            "a refusal persisted {:?}",
            self.journal.persisted()
        );
    }

    fn tree(&self) -> RunningTree {
        self.backend
            .observe()
            .resumed
            .first()
            .cloned()
            .expect("one resumed tree")
    }

    fn scratch_root(&self) -> String {
        canonical(&self.scratch)
    }

    /// The declared read page of one handle at an explicit bound.
    async fn read(
        &self,
        run: &str,
        handle: &str,
        cursor: u64,
        max_bytes: u32,
    ) -> Result<(ProcessReadReply, bool), ProcessError> {
        self.service
            .read_bounded(&token(run), handle, cursor, max_bytes, Some(0))
            .await
    }

    /// The exact terminal-frame sequence a confirmed close needs.
    fn set_proof(&self, proof: Option<TerminationProofRecord>) {
        self.backend.set_proof(&self.tree(), proof);
    }

    fn valid_proof(&self, handle: &str) -> TerminationProofRecord {
        let record = self.record(handle);
        proof_naming(&record, &record.tree_id, record.ownership_epoch)
    }

    fn emit(&self, frames: &[ProcessOutputFrame]) {
        self.backend.emit(&self.tree(), frames);
    }

    /// Terminal output plus a proof naming exactly this tree: a close that the
    /// backend can confirm.
    fn arm_close(&self, handle: &str, first: &[u8], code: Option<i32>) {
        self.emit(&[data(0, first), exit_frame(1, code)]);
        let record = self.record(handle);
        let proof = proof_naming(&record, &record.tree_id, record.ownership_epoch);
        self.backend.set_proof(&self.tree(), Some(proof));
    }

    async fn write(&self, run: &str, handle: &str, bytes: Vec<u8>) -> Result<(), ProcessError> {
        self.service
            .write_validated(&token(run), handle, bytes)
            .await
    }

    async fn close(&self, run: &str, handle: &str) -> Result<Option<i32>, ProcessError> {
        self.service.close_confirmed(&token(run), handle).await
    }
}

/// A proof that digests itself coherently; only the fields a caller names are
/// right, so a refusal can never be blamed on a malformed digest.
fn proof_naming(
    record: &SupervisorRecord,
    tree_id: &str,
    ownership_epoch: u64,
) -> TerminationProofRecord {
    let identity = json!({
        "treeId": tree_id,
        "ownershipEpoch": ownership_epoch,
        "nativeExit": true,
    });
    TerminationProofRecord {
        proof_id: format!("proof-{tree_id}"),
        tree_id: tree_id.to_string(),
        ownership_epoch,
        kind: TerminationProofKind::Exit,
        observed_boot_identity: record.owner.clone().expect("durable owner").boot_identity,
        proof_identity_digest: r_code_harness_protocol::canonical_input_hash(&identity),
        proof_identity: identity,
        recorded_at_ms: 1,
    }
}

/// INV-07 and acceptance ①: the honest gate refuses with zero backend calls,
/// while the identical binding carrying an `Activated` verdict does launch, so
/// the refusal is the gate's doing and not a broken harness.
#[tokio::test]
async fn closed_gate_starts_nothing_while_an_activated_binding_launches() {
    let store_root = tempfile::tempdir().expect("store tempdir");
    let store = V1Store::open(&store_root.path().join("harness-v1.db")).expect("open store");
    let verdict = production_verdict(&store);
    assert!(
        matches!(&verdict, SafetyActivation::NotActivated { reason, .. }
            if *reason == "status-unsupported"),
        "an Unsupported report must never present itself as Activated: {verdict:?}"
    );

    let mut options = Options::interactive();
    options.activation = verdict;
    let gated = Harness::build(options);
    gated.register(PROFILE, HELPER).await;
    gated
        .declare(ProcessProfileEffect::ScratchOnly.as_str())
        .await
        .expect("admissible declaration");
    let error = gated
        .open("run-1")
        .await
        .expect_err("a closed gate may not start a process");
    assert!(
        matches!(&error, ProcessError::GateClosed(reason)
            if reason.contains("status-unsupported")),
        "the refusal must name the gate verdict: {error}"
    );
    let seen = gated.backend.observe();
    assert!(
        seen.prepared.is_empty() && seen.resumed.is_empty(),
        "{seen:?}"
    );
    assert!(gated.input.frames().is_empty());
    assert!(gated.journal.persisted().is_empty());
    assert!(gated.journal.history().is_empty());

    // Same host, same material: only the verdict differs, and it launches.
    let open = Harness::interactive().await;
    let handle = open.open("run-1").await.expect("activated launch");
    assert_eq!(open.backend.observe().resumed.len(), 1);
    assert_eq!(open.journal.persisted().len(), 1);
    assert_eq!(handle, format!("run-1:{ATTEMPT}:tree-{ATTEMPT}-{EPOCH}-1"));
}

/// P22.1 and acceptance ①: a launch whose executable or working directory
/// cannot be named legally stops before the supervisor sees anything at all,
/// and the arm differing only in legality reaches it.
#[tokio::test]
async fn refused_executable_or_working_directory_launches_nothing() {
    let harness = Harness::interactive().await;
    let unregistered = harness
        .service
        .open_profiled(
            &token("run-1"),
            "no-such-profile",
            Path::new(HELPER),
            &[],
            None,
        )
        .await
        .expect_err("a profile with no host-registered executable has no material");
    assert!(
        matches!(&unregistered, ProcessError::Spec(reason)
            if reason.contains("no host-registered executable")),
        "{unregistered}"
    );

    // A caller cannot widen the pinned profile by naming another executable.
    harness
        .register(PROFILE, "definitely-not-a-real-exe.xyz")
        .await;
    let substituted = harness
        .open("run-1")
        .await
        .expect_err("the executable must be the one the host registered");
    assert!(
        matches!(&substituted, ProcessError::Spec(reason)
            if reason.contains("is not the executable registered")),
        "{substituted}"
    );
    harness.assert_nothing_persisted();
    harness.register(PROFILE, HELPER).await;

    let relative = harness
        .open_at("run-1", Some("relative/scratch"))
        .await
        .expect_err("a relative cwd is not spawn material");
    assert!(
        matches!(&relative, ProcessError::Spec(reason)
            if reason.contains("not absolute")),
        "{relative}"
    );

    let git = format!("{}/.git/hooks", harness.scratch_root());
    assert!(
        matches!(
            harness.open_at("run-1", Some(&git)).await,
            Err(ProcessError::Spec(reason)) if reason.contains("git directory visible")
        ),
        "INV-06: a .git component is never spawn material"
    );

    let foreign = harness
        .open_at("run-1", Some(CHECKOUT))
        .await
        .expect_err("a cwd outside the scratch root is not this profile's cwd");
    assert!(
        matches!(&foreign, ProcessError::Spec(reason)
            if reason.contains("escapes the scratch root")),
        "{foreign}"
    );
    assert!(harness.backend.observe().prepared.is_empty());
    harness.assert_nothing_persisted();

    // Control: the only difference in the legal arm is the material itself.
    let handle = harness.open("run-1").await.expect("legal material");
    let seen = harness.backend.observe();
    assert_eq!(seen.prepared.len(), 1);
    assert_eq!(seen.resumed.len(), 1);
    assert_eq!(harness.journal.persisted().len(), 1);
    let record = harness.record(&handle);
    assert_eq!(record.phase, SupervisorPhase::Running);
    assert!(seen.prepared[0].inherited_objects.is_empty());
    seen.prepared[0]
        .validate()
        .expect("captured spec validates");
    assert_eq!(seen.prepared[0].output_capacity_bytes, CAPACITY);
    assert_eq!(canonical(&seen.prepared[0].cwd), harness.scratch_root());
    assert_eq!(
        handle,
        format!("run-1:{ATTEMPT}:{}", record.tree_id),
        "a handle names exactly run, attempt and tree (acceptance ②)"
    );
}

/// P22.1: a binding that cannot name a capacity, an attempt identity, or a cwd
/// that is not the checkout refuses before the supervisor is constructed.
#[tokio::test]
async fn refused_capacity_or_identity_launches_nothing() {
    for (capacity, attempt, expected) in [
        (0usize, ATTEMPT.to_string(), "output capacity"),
        (
            MAX_OUTPUT_BUFFER_BYTES + 1,
            ATTEMPT.to_string(),
            "output capacity",
        ),
        (CAPACITY, " ".into(), "attempt identity is incomplete"),
    ] {
        let options = Options {
            capacity,
            attempt,
            ..Options::interactive()
        };
        let harness = Harness::build(options);
        harness.register(PROFILE, HELPER).await;
        harness
            .declare(ProcessProfileEffect::ScratchOnly.as_str())
            .await
            .expect("admissible declaration");
        let error = harness
            .open("run-1")
            .await
            .expect_err("incomplete material may not launch");
        assert!(
            matches!(&error, ProcessError::Spec(reason) if reason.contains(expected)),
            "{error}"
        );
        assert!(harness.backend.observe().prepared.is_empty());
        harness.assert_nothing_persisted();
    }

    // Acceptance ③: containment is not the only gate. A cwd below the scratch
    // root that *is* the checked-out workspace is refused too.
    let nested = tempfile::tempdir().expect("nested scratch");
    let checkout = nested.path().join("checkout");
    let journal = DeterministicSupervisorJournal::default();
    let inside = Harness::attach(
        Options {
            workspace: WorkspaceCapability::WriteWithin {
                root: canonical(&checkout),
            },
            ..Options::interactive()
        },
        &journal,
        nested.path(),
    );
    inside.register(PROFILE, HELPER).await;
    inside
        .declare(ProcessProfileEffect::ScratchOnly.as_str())
        .await
        .expect("admissible declaration");
    let refused = inside
        .open_at("run-1", Some(&canonical(&checkout)))
        .await
        .expect_err("a non-workspace profile may not run at the checkout");
    assert!(
        matches!(&refused, ProcessError::Spec(reason)
            if reason.contains("is the checked-out workspace")),
        "{refused}"
    );
    assert!(journal
        .load("process-tree-attempt-s22-1")
        .expect("journal readable")
        .is_none());
}

/// P22.1: the resolved profile is journaled as a `SpawnSpec` record before the
/// backend resumes it, and a fault inside that window leaves a durable,
/// recoverable record rather than an unmanaged process.
#[tokio::test]
async fn the_spawn_spec_is_durable_before_the_backend_resumes() {
    let harness = Harness::interactive().await;
    let handle = harness.open("run-1").await.expect("supervised open");
    let at_resume = harness
        .backend
        .observe()
        .at_resume
        .first()
        .cloned()
        .flatten()
        .expect("a record existed when resume was asked");
    assert_eq!(at_resume.phase, SupervisorPhase::ResumePending);
    assert_eq!(at_resume.owner, Some(owner()));
    assert_eq!(at_resume.revision, 4);
    assert_eq!(at_resume.ownership_epoch, EPOCH);
    assert_eq!(
        at_resume.tree_id,
        handle.split(':').nth(2).expect("tree scope")
    );
    // The snapshot is taken at the boundary, not after it: the same record has
    // moved on by the time the open returns.
    assert_eq!(harness.record(&handle).phase, SupervisorPhase::Running);
    assert_eq!(harness.record(&handle).revision, 5);
    // The phases were persisted in that order, and only then was resume asked.
    let phases: Vec<SupervisorPhase> = harness
        .journal
        .history()
        .iter()
        .map(|(_, phase)| *phase)
        .collect();
    assert_eq!(
        phases,
        vec![
            SupervisorPhase::Suspended,
            SupervisorPhase::IdentityRecorded,
            SupervisorPhase::ResumePending,
            SupervisorPhase::Running
        ]
    );

    // A refused journal write stops before resume: prepared, never resumed.
    let stalled = Harness::build(Options::interactive());
    stalled.register(PROFILE, HELPER).await;
    stalled
        .declare(ProcessProfileEffect::ScratchOnly.as_str())
        .await
        .expect("admissible declaration");
    stalled.records.fail_once(JournalFaultPoint::ResumePending);
    assert!(matches!(
        stalled.open("run-1").await,
        Err(ProcessError::Supervisor(_))
    ));
    let seen = stalled.backend.observe();
    assert_eq!(seen.prepared.len(), 1, "the spec reached the backend");
    assert!(
        seen.resumed.is_empty(),
        "nothing resumed after a refused persist"
    );

    // A backend that cannot resume also leaves a durable recoverable record.
    let crashed = Harness::build(Options::interactive());
    crashed.register(PROFILE, HELPER).await;
    crashed
        .declare(ProcessProfileEffect::ScratchOnly.as_str())
        .await
        .expect("admissible declaration");
    crashed.backend.fault_resume_once();
    assert!(!crashed.open("run-1").await.is_ok());
    let persisted = crashed.journal.persisted();
    assert_eq!(persisted.len(), 1);
    let supervisor = ProcessSupervisor::new(
        Arc::new(crashed.backend.clone()),
        Arc::new(crashed.journal.clone()),
    );
    let recovered = supervisor
        .recover(&persisted[0])
        .expect("the run is accounted for, not orphaned");
    assert_eq!(recovered.phase, SupervisorPhase::Quarantined);
    assert_eq!(recovered.owner, Some(owner()));
}

/// Acceptance ②: a handle is valid only for the run, attempt, tree and
/// generation it names, and every refusal leaves the record and the child
/// exactly as they were.
#[tokio::test]
async fn a_handle_acts_only_for_the_run_attempt_and_tree_it_names() {
    let harness = Harness::interactive().await;
    let handle = harness.open("run-1").await.expect("supervised open");
    let before = harness.record(&handle);
    assert_eq!(before.phase, SupervisorPhase::Running);

    let foreign_run = GenerationToken {
        run_id: "run-2".into(),
        generation: 1,
    };
    let stale = GenerationToken {
        run_id: "run-1".into(),
        generation: 2,
    };
    assert!(matches!(
        harness
            .service
            .write_validated(&foreign_run, &handle, frame("harness.start", json!({})))
            .await,
        Err(ProcessError::HandleScope(_))
    ));
    assert!(matches!(
        harness
            .service
            .read_bounded(&foreign_run, &handle, 0, 64, Some(0))
            .await,
        Err(ProcessError::HandleScope(_))
    ));
    assert!(matches!(
        harness.close("run-2", &handle).await,
        Err(ProcessError::HandleScope(_))
    ));
    assert!(matches!(
        harness
            .service
            .write_validated(&stale, &handle, frame("harness.start", json!({})))
            .await,
        Err(ProcessError::Fenced(_))
    ));

    // A live handle cannot restart at all, and one naming another attempt or
    // no tree at all is refused before the journal is consulted.
    assert!(matches!(
        harness
            .service
            .restart_fenced(&token("run-1"), &handle)
            .await,
        Err(ProcessError::Fenced(reason)) if reason.contains("live tree")
    ));
    let other_attempt = format!("run-1:other-attempt:tree-{ATTEMPT}-1");
    assert!(matches!(
        harness
            .service
            .restart_fenced(&token("run-1"), &other_attempt)
            .await,
        Err(ProcessError::HandleScope(_))
    ));
    assert!(matches!(
        harness
            .service
            .restart_fenced(&token("run-1"), "run-1:attempt-s22")
            .await,
        Err(ProcessError::HandleScope(reason)) if reason.contains("names no tree")
    ));
    let other_tree = format!("run-1:{ATTEMPT}:tree-{ATTEMPT}-99");
    assert!(matches!(
        harness
            .service
            .restart_fenced(&token("run-1"), &other_tree)
            .await,
        Err(ProcessError::UnknownHandle(_))
    ));

    // Nothing happened: no byte forwarded, no tree terminated, no new record.
    assert!(harness.input.frames().is_empty());
    assert!(harness.backend.observe().terminated.is_empty());
    assert_eq!(harness.record(&handle), before);

    // Control: correctly scoped calls on the same handle do take effect.
    harness
        .write("run-1", &handle, frame("harness.start", json!({})))
        .await
        .expect("the owning run may write");
    assert_eq!(harness.input.frames().len(), 1);
    harness.arm_close(&handle, b"done\n", Some(0));
    assert_eq!(
        harness.close("run-1", &handle).await.unwrap(),
        Some(0),
        "the owning run may close its own tree"
    );
    assert_eq!(
        harness.record(&handle).phase,
        SupervisorPhase::ProofAccepted
    );
    assert_eq!(harness.backend.observe().terminated.len(), 1);
}

/// The bytes a page actually delivered, decoded independently of the backend.
fn decoded(page: &ProcessReadReply) -> Vec<u8> {
    page.frames
        .iter()
        .filter_map(|frame| match frame {
            ProcessOutputFrame::Data { data_base64, .. } => Some(data_base64.clone()),
            _ => None,
        })
        .flat_map(|value| {
            base64::engine::general_purpose::STANDARD
                .decode(value)
                .expect("base64 output")
        })
        .collect()
}

/// P22.2: a read is bounded by the request and by the declared capacity,
/// reports truncation instead of growing past either, and a page is consumed
/// exactly once — a reader behind the journaled cursor is refused.
#[tokio::test]
async fn a_bounded_read_reports_truncation_and_consumes_each_page_once() {
    let harness = Harness::interactive().await;
    let handle = harness.open("run-1").await.expect("supervised open");
    harness.emit(
        &(0..6u64)
            .map(|sequence| data(sequence, &[b'x'; 20]))
            .collect::<Vec<_>>(),
    );

    let bounds = [
        (0, 0, 0),
        (0, CAPACITY as u32 + 1, 0),
        (0, PROCESS_READ_MAX_BYTES + 1, 0),
        (0, 64, PROCESS_READ_MAX_WAIT_MS + 1),
    ];
    for (cursor, max_bytes, wait_ms) in bounds {
        assert!(
            matches!(
                harness
                    .service
                    .read_bounded(&token("run-1"), &handle, cursor, max_bytes, Some(wait_ms))
                    .await,
                Err(ProcessError::Spec(_))
            ),
            "read bounds ({cursor}, {max_bytes}, {wait_ms}) were accepted"
        );
    }
    assert_eq!(harness.record(&handle).output_cursor, 0);
    assert_eq!(harness.record(&handle).phase, SupervisorPhase::Running);

    let (first, truncated) = harness.read("run-1", &handle, 0, 40).await.unwrap();
    assert_eq!(decoded(&first).len(), 40);
    assert_eq!(first.next_cursor, 2);
    assert!(truncated, "the bound cut a continuing stream short");
    assert_eq!(harness.record(&handle).output_cursor, 2);

    // Defect (reported, not fixed): truncation is only reported when the page
    // lands exactly on `max_bytes`. Frames of 30 bytes at a bound of 40 deliver
    // one frame, stop while the stream continues, and report no truncation.
    let coarser = Harness::interactive().await;
    let coarser_handle = coarser.open("run-1").await.expect("coarser open");
    coarser.emit(&[data(0, &[b'z'; 30]), data(1, &[b'z'; 30])]);
    let (page, short) = coarser
        .read("run-1", &coarser_handle, 0, 40)
        .await
        .expect("coarser read");
    assert_eq!(decoded(&page).len(), 30);
    assert_eq!(page.next_cursor, 1);
    assert!(!page.terminal);
    assert!(
        !short,
        "the service under-reports a bound-shortened page: {page:?}"
    );

    // Exact-once: the same page can never be consumed twice.
    assert!(matches!(
        harness.read("run-1", &handle, 0, 40).await,
        Err(ProcessError::HandleScope(reason)) if reason.contains("behind")
    ));
    assert_eq!(
        harness.record(&handle).output_cursor,
        2,
        "a refused replay consumed bytes"
    );
    let (second, _) = harness.read("run-1", &handle, 2, 40).await.unwrap();
    assert_eq!(decoded(&second).len(), 40);
    assert_eq!(second.next_cursor, 4);
    assert_eq!(harness.record(&handle).output_cursor, 4);

    // Tripwire: a backend handing back more than was asked is caught by the
    // service's own wire validation, and the extra bytes are never consumed.
    let lying = Harness::build(Options::interactive());
    lying.register(PROFILE, HELPER).await;
    lying
        .declare(ProcessProfileEffect::ScratchOnly.as_str())
        .await
        .expect("admissible declaration");
    lying.backend.deliver_oversize_pages();
    let lying_handle = lying.open("run-1").await.expect("oversize arm open");
    lying.emit(&[data(0, &[b'y'; 20]), data(1, &[b'y'; 20])]);
    assert!(matches!(
        lying.read("run-1", &lying_handle, 0, 20).await,
        Err(ProcessError::Supervisor(reason)) if reason.contains("maxBytes")
    ));
    assert_eq!(lying.record(&lying_handle).output_cursor, 0);
}

/// P22.2: only whole, profile-valid frames are ever forwarded. A buffer mixing
/// one valid frame with one partial frame forwards neither, so a refusal is
/// atomic instead of partially delivered.
#[tokio::test]
async fn only_whole_valid_frames_ever_reach_a_child() {
    let harness = Harness::interactive().await;
    let handle = harness.open("run-1").await.expect("supervised open");
    let tree_id = harness.record(&handle).tree_id;
    let good = frame("harness.start", json!({}));

    let refused: Vec<Refusal> = vec![
        ("empty", Vec::new(), |error| {
            matches!(error, ProcessError::Spec(_))
        }),
        ("oversize", vec![b'0'; CAPACITY + 1], |error| {
            matches!(error, ProcessError::Spec(_))
        }),
        ("malformed", b"nonsense\n".to_vec(), |error| {
            matches!(error, ProcessError::FrameRejected(_))
        }),
        ("unknown method", frame("reboot", json!({})), |error| {
            matches!(error, ProcessError::FrameRejected(reason)
                    if reason.contains("allowlist"))
        }),
        (
            "forbidden field",
            frame(
                "session.start",
                json!({"run": "run-1", "mode": "fast", "token": "x"}),
            ),
            |error| {
                matches!(error, ProcessError::FrameRejected(reason)
                    if reason.contains("must not be present"))
            },
        ),
        (
            "foreign binding",
            frame("session.start", json!({"run": "run-2", "mode": "fast"})),
            |error| {
                matches!(error, ProcessError::FrameRejected(reason)
                    if reason.contains("host-bound"))
            },
        ),
        (
            "not enumerated",
            frame("session.start", json!({"run": "run-1", "mode": "turbo"})),
            |error| {
                matches!(error, ProcessError::FrameRejected(reason)
                    if reason.contains("allowed values"))
            },
        ),
        (
            "partial frame",
            br#"{"jsonrpc":"2.0","id":1,"method":"harness.start""#.to_vec(),
            |error| {
                matches!(error, ProcessError::FrameRejected(reason)
                    if reason.contains("partial frame"))
            },
        ),
        (
            "valid head with partial tail",
            [good.as_slice(), br#"{"jsonr"#].concat(),
            |error| {
                matches!(error, ProcessError::FrameRejected(reason)
                    if reason.contains("partial frame"))
            },
        ),
    ];
    for (label, bytes, expected) in refused {
        let error = harness
            .write("run-1", &handle, bytes)
            .await
            .expect_err(label);
        assert!(expected(&error), "{label} refused as: {error}");
        assert!(
            harness.input.frames().is_empty(),
            "{label} reached the child"
        );
        assert_eq!(
            harness.record(&handle).phase,
            SupervisorPhase::Running,
            "{label}"
        );
    }

    // Control: whole valid frames arrive once, byte for byte, naming this tree.
    let second = frame("session.start", json!({"run": "run-1", "mode": "slow"}));
    harness
        .write("run-1", &handle, good.clone())
        .await
        .expect("a whole valid frame");
    let both = [good.as_slice(), second.as_slice()].concat();
    harness
        .write("run-1", &handle, both.clone())
        .await
        .expect("two whole valid frames in one buffer");
    assert_eq!(
        harness.input.frames(),
        vec![(tree_id.clone(), good), (tree_id, both)],
        "exactly the validated bytes were forwarded, once each"
    );
}

/// Disclosed v1 limit, pinned as a limit: with no `ProcessInputChannel` bound
/// nothing is forwarded even though frames are validated, while a bound channel
/// receives them exactly once and an io failure surfaces instead of being
/// swallowed.
#[tokio::test]
async fn a_write_without_a_bound_input_channel_forwards_nothing() {
    let mut unbound_options = Options::interactive();
    unbound_options.bind_input = false;
    let unbound = Harness::build(unbound_options);
    unbound.register(PROFILE, HELPER).await;
    unbound
        .declare(ProcessProfileEffect::ScratchOnly.as_str())
        .await
        .expect("admissible declaration");
    let handle = unbound.open("run-1").await.expect("supervised open");
    let tree_id = unbound.record(&handle).tree_id;
    let good = frame("harness.start", json!({}));

    let error = unbound
        .write("run-1", &handle, good.clone())
        .await
        .expect_err("no bound route to a child exists");
    assert!(
        matches!(&error, ProcessError::NoInputChannel(tree) if *tree == tree_id),
        "{error}"
    );
    assert!(unbound.input.frames().is_empty());
    // Validation still precedes the refusal: an invalid frame is refused first.
    assert!(matches!(
        unbound
            .write("run-1", &handle, b"nonsense\n".to_vec())
            .await,
        Err(ProcessError::FrameRejected(_))
    ));
    assert_eq!(unbound.record(&handle).phase, SupervisorPhase::Running);

    // Control: the identical frame on a bound channel arrives exactly once.
    let bound = Harness::interactive().await;
    let bound_handle = bound.open("run-1").await.expect("bound open");
    bound
        .write("run-1", &bound_handle, good.clone())
        .await
        .expect("a bound channel accepts a validated frame");
    assert_eq!(
        bound.input.frames(),
        vec![(bound.record(&bound_handle).tree_id, good.clone())]
    );

    // A failing channel is an io failure that forwarded nothing on the way.
    let failing = Harness::interactive().await;
    let failing_handle = failing.open("run-1").await.expect("failing open");
    failing.input.refuse_writes();
    assert!(matches!(
        failing.write("run-1", &failing_handle, good).await,
        Err(ProcessError::Io(reason)) if reason.contains("stdin")
    ));
    assert!(failing.input.frames().is_empty());
}

/// P22.3: a restart is fenced by the ownership epoch and by the record's phase.
/// It starts nothing itself, and only a proven-dead tree may be followed: the
/// relaunch that a legal fence permits names its own epoch, so it becomes a new
/// supervised tree instead of colliding with the record it fenced on.
#[tokio::test]
async fn a_restart_is_fenced_by_epoch_and_never_launches_a_second_tree() {
    let first = Harness::interactive().await;
    let handle = first.open("run-1").await.expect("supervised open");
    assert!(matches!(
        first
            .service
            .restart_fenced(&token("run-1"), &handle)
            .await,
        Err(ProcessError::Fenced(reason)) if reason.contains("live tree")
    ));
    assert!(first.backend.observe().terminated.is_empty());

    first.arm_close(&handle, b"bye\n", Some(3));
    assert_eq!(first.close("run-1", &handle).await.unwrap(), Some(3));
    let closed = first.record(&handle);
    assert_eq!(closed.phase, SupervisorPhase::ProofAccepted);

    // Same epoch: the record is not below this binding's, so restart is fenced.
    let stale = Harness::attach(Options::interactive(), &first.records, &first.scratch);
    assert!(matches!(
        stale.service.restart_fenced(&token("run-1"), &handle).await,
        Err(ProcessError::Fenced(reason)) if reason.contains("ownership epoch above")
    ));

    // Control on the same durable journal: an advanced epoch accepts, and the
    // accepted restart itself starts nothing.
    let mut advanced = Options::interactive();
    advanced.epoch = EPOCH + 1;
    let restarted = Harness::attach(advanced, &first.records, &first.scratch);
    restarted.register(PROFILE, HELPER).await;
    restarted
        .declare(ProcessProfileEffect::ScratchOnly.as_str())
        .await
        .expect("admissible declaration");
    restarted
        .service
        .restart_fenced(&token("run-1"), &handle)
        .await
        .expect("an advanced epoch may follow a proven-dead tree");
    assert!(restarted.backend.observe().prepared.is_empty());
    assert!(restarted.journal.persisted().is_empty());

    // Defect repaired: the operation name carries the ownership epoch, so the
    // restarted attempt is a new supervised tree instead of colliding with the
    // record it legitimately fenced on.
    let relaunched = restarted
        .open("run-1")
        .await
        .expect("an epoch-advancing restart relaunches");
    assert_eq!(
        relaunched,
        format!("run-1:{ATTEMPT}:tree-{ATTEMPT}-{}-1", EPOCH + 1),
        "the new tree must name the higher epoch"
    );
    assert_ne!(
        relaunched, handle,
        "a restart never reattaches the dead handle"
    );
    assert_eq!(
        restarted.journal.persisted().len(),
        1,
        "exactly one new operation was persisted"
    );
    assert_eq!(
        first.journal.persisted().len(),
        1,
        "the first harness wrote nothing further"
    );
    assert_eq!(
        restarted.record(&relaunched).phase,
        SupervisorPhase::Running,
        "the relaunched tree is supervised from its own record"
    );
    assert_eq!(restarted.backend.observe().prepared.len(), 1);
    assert_eq!(restarted.backend.observe().resumed.len(), 1);
    assert_eq!(
        restarted.record(&handle),
        closed,
        "the proven-terminal record is exactly what the restart fenced on"
    );

    // The old handle is consumed: it names a tree no service surface holds.
    assert!(matches!(
        restarted.close("run-1", &handle).await,
        Err(ProcessError::UnknownHandle(_))
    ));
    assert!(matches!(
        restarted
            .service
            .write_validated(&token("run-1"), &handle, frame("harness.start", json!({})))
            .await,
        Err(ProcessError::UnknownHandle(_))
    ));
    assert!(restarted.backend.observe().terminated.is_empty());
}

/// P22.3: a nonterminal orphan cannot be restarted into a live tree; the fence
/// recovers it first and refuses, so no stale attempt can resume or write into
/// a tree it never owned.
#[tokio::test]
async fn a_restart_recovers_a_nonterminal_orphan_instead_of_reattaching() {
    let crashed = Harness::interactive().await;
    crashed.backend.fault_resume_once();
    assert!(!crashed.open("run-1").await.is_ok());
    let persisted = crashed.journal.persisted();
    assert_eq!(persisted.len(), 1);
    let orphan = format!("run-1:{ATTEMPT}:tree-{ATTEMPT}-{EPOCH}-1");

    let mut advanced = Options::interactive();
    advanced.epoch = EPOCH + 1;
    let host = Harness::attach(advanced, &crashed.records, &crashed.scratch);
    let error = host
        .service
        .restart_fenced(&token("run-1"), &orphan)
        .await
        .expect_err("a nonterminal tree is never restartable");
    assert!(
        matches!(&error, ProcessError::Fenced(reason) if reason.contains("recovered as")),
        "{error}"
    );
    let record = host
        .records
        .load(&persisted[0])
        .expect("journal readable")
        .expect("orphan record");
    assert_eq!(record.phase, SupervisorPhase::Quarantined);
    assert!(host.backend.observe().resumed.is_empty());
    assert!(host.backend.observe().terminated.is_empty());
}

/// P22.3: a close is confirmed only by a proof naming this tree, and the
/// terminal transition lands exactly once; anything else quarantines durably
/// and never records a proof.
#[tokio::test]
async fn only_a_proof_naming_this_tree_can_confirm_a_close() {
    let confirmed = Harness::interactive().await;
    let handle = confirmed.open("run-1").await.expect("supervised open");
    confirmed.emit(&[data(0, b"tail\n"), exit_frame(1, Some(0))]);
    let proof = confirmed.valid_proof(&handle);
    confirmed.set_proof(Some(proof.clone()));
    assert_eq!(confirmed.close("run-1", &handle).await.unwrap(), Some(0));
    let closed = confirmed.record(&handle);
    assert_eq!(closed.phase, SupervisorPhase::ProofAccepted);
    assert_eq!(closed.proof, Some(proof));

    // A second terminal transition cannot re-accept the same proof.
    assert!(matches!(
        confirmed.close("run-1", &handle).await,
        Err(ProcessError::UnknownHandle(_))
    ));
    let mut repeat = closed.clone();
    repeat.revision += 1;
    assert!(matches!(
        confirmed.records.compare_and_swap(&closed, repeat),
        Err(SupervisorError::Journal(_))
    ));

    let record = {
        let foreign = Harness::interactive().await;
        let handle = foreign.open("run-1").await.expect("foreign open");
        foreign.emit(&[data(0, b"tail\n"), exit_frame(1, Some(7))]);
        let record = foreign.record(&handle);
        foreign.set_proof(Some(proof_naming(
            &record,
            "tree-another-attempt-1",
            record.ownership_epoch,
        )));
        assert!(matches!(
            foreign.close("run-1", &handle).await,
            Err(ProcessError::Unverifiable(_))
        ));
        let durable = foreign.record(&handle);
        assert_eq!(durable.phase, SupervisorPhase::Quarantined);
        assert_eq!(
            durable.quarantine_reason.as_deref(),
            Some("termination-proof-mismatch")
        );
        record
    };

    // Each forged proof names something else but digests itself coherently.
    let mut wrong_epoch = proof_naming(&record, &record.tree_id, record.ownership_epoch + 1);
    wrong_epoch.tree_id = record.tree_id.clone();
    let mut other_boot = proof_naming(&record, &record.tree_id, record.ownership_epoch);
    other_boot.observed_boot_identity =
        BootIdentity::parse("linux:0f0f0f0f-1111-2222-3333-444444444444").expect("other boot");
    let mut nameless = proof_naming(&record, &record.tree_id, record.ownership_epoch);
    nameless.proof_id = "   ".into();
    let mut tampered = proof_naming(&record, &record.tree_id, record.ownership_epoch);
    tampered.proof_identity_digest = "0".repeat(64);
    for forged in [wrong_epoch, other_boot, nameless, tampered] {
        let harness = Harness::interactive().await;
        let handle = harness.open("run-1").await.expect("forged open");
        harness.emit(&[data(0, b"tail\n"), exit_frame(1, Some(7))]);
        harness.set_proof(Some(forged.clone()));
        assert!(
            matches!(
                harness.close("run-1", &handle).await,
                Err(ProcessError::Unverifiable(_))
            ),
            "a forged proof was accepted: {forged:?}"
        );
        let durable = harness.record(&handle);
        assert_eq!(durable.phase, SupervisorPhase::Quarantined);
        assert!(durable.proof.is_none(), "a forged proof was journaled");
    }
}

/// Acceptance ③ defense in depth: whatever the router does, the service itself
/// refuses the widest effect, and no declaration lets a profile claim an effect
/// its own frame rules do not support.
#[tokio::test]
async fn the_service_refuses_every_effect_above_the_interactive_ceiling() {
    let checkout = Harness::build(Options {
        schema: Some(checkout_profile()),
        ..Options::interactive()
    });
    checkout.register(PROFILE, HELPER).await;
    checkout
        .declare(ProcessProfileEffect::CurrentCheckoutWrite.as_str())
        .await
        .expect("an honest declaration of the widest effect");
    let hidden = checkout
        .open("run-1")
        .await
        .expect_err("workspace writes stay hidden until P24B");
    assert!(
        hidden
            .to_string()
            .contains("writes into the current checkout")
            && hidden.to_string().contains("P27 effect envelope"),
        "{hidden}"
    );
    checkout.assert_nothing_persisted();
    assert!(checkout.backend.observe().prepared.is_empty());

    // A profile whose rules reach the checkout cannot declare itself narrower:
    // registration parse-checks the literal, resolution refuses the pairing.
    let smuggled = Harness::build(Options {
        schema: Some(checkout_profile()),
        ..Options::interactive()
    });
    smuggled.register(PROFILE, HELPER).await;
    smuggled
        .declare(ProcessProfileEffect::ScratchOnly.as_str())
        .await
        .expect("a known literal registers");
    let conflict = smuggled
        .open("run-1")
        .await
        .expect_err("the declaration contradicts its own frame rules");
    assert!(
        matches!(&conflict, ProcessError::Spec(reason)
            if reason.contains("constrains frames against the workspace")),
        "{conflict}"
    );
    smuggled.assert_nothing_persisted();

    // And a profile that cannot reach the checkout may not claim it either.
    let claimer = Harness::interactive().await;
    claimer
        .declare(ProcessProfileEffect::CurrentCheckoutWrite.as_str())
        .await
        .expect("a known literal registers");
    let unreachable = claimer
        .open("run-1")
        .await
        .expect_err("no frame rule of this profile can reach a checkout");
    assert!(
        matches!(&unreachable, ProcessError::Spec(reason)
            if reason.contains("no frame rule can reach the workspace")),
        "{unreachable}"
    );
    claimer.assert_nothing_persisted();
    assert!(
        matches!(
            claimer.declare("maybe-workspace").await,
            Err(ProcessError::Spec(_))
        ),
        "an unknown literal is refused at registration"
    );
    let undeclared = Harness::build(Options {
        schema: None,
        ..Options::interactive()
    });
    undeclared.register(PROFILE, HELPER).await;
    assert!(matches!(
        undeclared.open("run-1").await,
        Err(ProcessError::Spec(reason)) if reason.contains("no pinned schema")
    ));
    undeclared.assert_nothing_persisted();
}

/// P22's profile half: exactly the two effects at or below the interactive
/// ceiling launch, and `no-workspace` is confined to the scratch root itself
/// rather than anything below it.
#[tokio::test]
async fn only_effects_at_or_below_the_interactive_ceiling_launch() {
    for effect in [
        ProcessProfileEffect::NoWorkspace,
        ProcessProfileEffect::ScratchOnly,
    ] {
        let root = Harness::build(Options::interactive());
        root.register(PROFILE, HELPER).await;
        root.declare(effect.as_str())
            .await
            .expect("an admitted declaration");
        let handle = root.open("run-1").await.expect("admitted launch");
        assert_eq!(root.record(&handle).phase, SupervisorPhase::Running);

        let below = Harness::build(Options::interactive());
        below.register(PROFILE, HELPER).await;
        below
            .declare(effect.as_str())
            .await
            .expect("an admitted declaration");
        let cwd = format!("{}/scratch-sub", below.scratch_root());
        let second = below.open_at("run-1", Some(&cwd)).await;
        if effect == ProcessProfileEffect::NoWorkspace {
            assert!(
                matches!(&second, Err(ProcessError::Spec(reason))
                    if reason.contains("may only run at the scratch root")),
                "no-workspace accepted a cwd below its root: {second:?}"
            );
            below.assert_nothing_persisted();
        } else {
            assert!(
                second.is_ok(),
                "scratch-only may run below its root: {second:?}"
            );
            assert_eq!(below.journal.persisted().len(), 1);
        }
    }
}

/// Acceptance ①, behaviorally: every tree this suite observed came from the
/// injected backend, one prepare per resume, and no refusal arm added either.
#[tokio::test]
async fn every_launch_observable_in_the_suite_came_from_the_injected_backend() {
    let refused = [
        Options {
            schema: None,
            ..Options::interactive()
        },
        Options {
            schema: Some(checkout_profile()),
            ..Options::interactive()
        },
        Options {
            capacity: 0,
            ..Options::interactive()
        },
    ];
    for options in refused {
        let harness = Harness::build(options);
        harness.register(PROFILE, HELPER).await;
        let _ = harness
            .declare(ProcessProfileEffect::ScratchOnly.as_str())
            .await;
        let _ = harness.open("run-1").await;
        let seen = harness.backend.observe();
        assert!(
            seen.prepared.is_empty(),
            "acceptance ①: a launch was prepared"
        );
        assert!(seen.resumed.is_empty(), "acceptance ①: a tree was resumed");
        harness.assert_nothing_persisted();
    }

    let harness = Harness::interactive().await;
    let handle = harness.open("run-1").await.expect("supervised open");
    harness.arm_close(&handle, b"tail\n", Some(0));
    assert_eq!(harness.close("run-1", &handle).await.unwrap(), Some(0));
    let seen = harness.backend.observe();
    assert_eq!(seen.prepared.len(), 1);
    assert_eq!(seen.resumed.len(), 1);
    assert_eq!(seen.terminated, seen.resumed, "only the bound tree died");
    assert_eq!(seen.proven, seen.resumed, "only the bound tree was proven");
    assert_eq!(
        harness.journal.persisted(),
        vec![Harness::operation(&handle)],
        "the only persisted operation is the one the handle names"
    );
    let bound_at_resume = seen
        .at_resume
        .first()
        .and_then(Option::as_ref)
        .map(|record| record.tree_id.clone());
    assert_eq!(bound_at_resume, Some(harness.record(&handle).tree_id));
}
