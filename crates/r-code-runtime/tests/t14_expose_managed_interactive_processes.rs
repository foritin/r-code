//! T14 — managed interactive processes.
//!
//! Since P22 the interactive path never spawns: `open` resolves the pinned
//! profile into a `SpawnSpec`, journals it and only then resumes a tree over
//! the injected backend. This suite therefore drives the real service against
//! a bound process-tree backend, the reference journal and a recording input
//! channel, and proves each intent from that evidence — journaled phases,
//! captured spawn material, terminated trees and forwarded bytes. The
//! production activation verdict on this host is honestly `Unsupported`, so
//! the fail-closed case is proven separately instead of being stubbed away.

use base64::Engine as _;
use r_code_harness_protocol::process_profile::{HostBindings, ProcessProfileSchema};
use r_code_harness_protocol::rpc::{error_code, RpcId, RpcRequest};
use r_code_harness_protocol::{
    HostService, ProcessOutputFrame, ProcessOutputStream, ProcessReadReply, ProcessReadRequest,
    RunIdentity,
};
use r_code_kernel::ports::{GenerationToken, ProcessService, RunGuard, ServiceError};
use r_code_kernel::testing::{FakeModelService, FakeToolService, MemoryJournal};
use r_code_runtime::plugins::{HostRouter, IgnoreQuestions};
use r_code_runtime::process_guard::{
    BootIdentity, ProcessOwnerIdentity, TerminationProofKind, TerminationProofRecord,
};
use r_code_runtime::services::authorization::*;
use r_code_runtime::services::launch_profiles::*;
use r_code_runtime::services::process_profiles::{FrameValidator, ProcessProfileEffect};
use r_code_runtime::services::process_supervisor::{
    DeterministicFakeBackend, DeterministicSupervisorJournal, OutputSubscription, PreparedChild,
    PreparedLaunch, ProcessTreeBackend, RunningTree, SpawnSpec, SupervisorError, SupervisorJournal,
    SupervisorPhase, SupervisorRecord,
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
use std::sync::{Arc, Mutex};
use std::time::Duration;

const HELPER: &str = env!("CARGO_BIN_EXE_harness-test-helper");
const PROFILE: &str = "app-server";
const ATTEMPT: &str = "attempt-t14";
const TASK: &str = "task-t14";
const EPOCH: u64 = 4;
const CAPACITY: usize = 4096;
const WORKSPACE: &str = "D:/work";

fn token(run: &str) -> GenerationToken {
    GenerationToken {
        run_id: run.into(),
        generation: 1,
    }
}

fn boot() -> BootIdentity {
    BootIdentity::current().expect("authoritative boot identity on this host")
}

fn owner() -> ProcessOwnerIdentity {
    ProcessOwnerIdentity::new(42_001, 77_001, boot(), json!({"native": "t14-job-handle"}))
        .expect("valid owner identity")
}

fn frame(method: &str, params: serde_json::Value) -> Vec<u8> {
    format!(
        "{}\n",
        json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
    )
    .into_bytes()
}

/// A profile whose frame rules cannot reach a checkout, so `scratch-only` is
/// the only effect it can honestly declare.
fn scratch_only_profile() -> ProcessProfileSchema {
    serde_json::from_value(serde_json::json!({
        "name": "app-server-fixture",
        "framing": "ndjson-rpc",
        "methods": [
            {"name": "session.start", "params": [
                {"pointer": "/run", "constraint": {"kind": "bound-to", "value": "run-id"}},
                {"pointer": "/mode", "constraint": {"kind": "enum", "values": ["fast", "slow"]}},
                {"pointer": "/token", "constraint": {"kind": "must-be-absent"}}
            ]},
            {"name": "harness.start", "params": []}
        ],
        "env": ["PATH"]
    }))
    .expect("profile")
}

/// The one profile shape whose frame rules do reach a checkout, which is what
/// P22 must keep undiscoverable.
fn workspace_profile() -> ProcessProfileSchema {
    serde_json::from_value(serde_json::json!({
        "name": "checkout-writer-fixture",
        "framing": "ndjson-rpc",
        "methods": [
            {"name": "file.write", "params": [
                {"pointer": "/path", "constraint": {"kind": "within-workspace"}}
            ]}
        ]
    }))
    .expect("profile")
}

fn validator(run: &str, profile: ProcessProfileSchema) -> FrameValidator {
    FrameValidator::new(
        profile,
        HostBindings {
            workspace_root: WORKSPACE.into(),
            task_id: TASK.into(),
            run_id: run.into(),
            attempt_id: ATTEMPT.into(),
            permission_ceiling: "full".into(),
        },
    )
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

fn exit(sequence: u64, code: Option<i32>) -> ProcessOutputFrame {
    ProcessOutputFrame::Exit {
        sequence,
        exit_code: code,
    }
}

fn proof_for(record: &SupervisorRecord) -> TerminationProofRecord {
    let owner = record.owner.clone().expect("durable owner");
    let proof_identity = json!({
        "treeId": record.tree_id,
        "ownershipEpoch": record.ownership_epoch,
        "nativeExit": true,
    });
    TerminationProofRecord {
        proof_id: format!("proof-{}", record.tree_id),
        tree_id: record.tree_id.clone(),
        ownership_epoch: record.ownership_epoch,
        kind: TerminationProofKind::Exit,
        observed_boot_identity: owner.boot_identity,
        proof_identity_digest: r_code_harness_protocol::canonical_input_hash(&proof_identity),
        proof_identity,
        recorded_at_ms: 1,
    }
}

/// Coherently self-digesting, but naming a different tree: only the tree
/// identity is wrong, so the refusal cannot be attributed to anything else.
fn foreign_proof(record: &SupervisorRecord) -> TerminationProofRecord {
    let tree_id = "tree-another-attempt-1".to_string();
    let proof_identity = json!({
        "treeId": tree_id,
        "ownershipEpoch": record.ownership_epoch,
        "nativeExit": true,
    });
    TerminationProofRecord {
        proof_id: format!("proof-{tree_id}"),
        ownership_epoch: record.ownership_epoch,
        kind: TerminationProofKind::Exit,
        observed_boot_identity: record.owner.clone().expect("durable owner").boot_identity,
        proof_identity_digest: r_code_harness_protocol::canonical_input_hash(&proof_identity),
        proof_identity,
        tree_id,
        recorded_at_ms: 1,
    }
}

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

/// Every launch and termination the supervised service asks of a backend.
#[derive(Clone, Default)]
struct LaunchLog {
    prepared: Vec<SpawnSpec>,
    resumed: Vec<RunningTree>,
    terminated: Vec<RunningTree>,
    proven: Vec<RunningTree>,
}

/// The deterministic fake backend plus the capture seams a service never
/// exposes: the resumed tree handle and the terminations it observed.
#[derive(Clone)]
struct InteractiveBackend {
    inner: DeterministicFakeBackend,
    log: Arc<Mutex<LaunchLog>>,
}

impl InteractiveBackend {
    fn new() -> Self {
        Self {
            inner: DeterministicFakeBackend::default(),
            log: Arc::new(Mutex::new(LaunchLog::default())),
        }
    }

    fn snapshot(&self) -> LaunchLog {
        self.log.lock().unwrap().clone()
    }

    fn arm_next_identity(&self, owner: ProcessOwnerIdentity) {
        self.inner.set_next_identity(owner);
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
impl ProcessTreeBackend for InteractiveBackend {
    async fn prepare(&self, spec: SpawnSpec) -> Result<PreparedLaunch, SupervisorError> {
        self.log.lock().unwrap().prepared.push(spec.clone());
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
        let tree = self.inner.resume_once(child).await?;
        self.log.lock().unwrap().resumed.push(tree.clone());
        Ok(tree)
    }

    async fn terminate(&self, tree: &RunningTree) -> Result<(), SupervisorError> {
        self.log.lock().unwrap().terminated.push(tree.clone());
        self.inner.terminate(tree).await
    }

    async fn wait_and_prove(
        &self,
        tree: &RunningTree,
        timeout: Duration,
    ) -> Result<TerminationProofRecord, SupervisorError> {
        self.log.lock().unwrap().proven.push(tree.clone());
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
        self.inner
            .drain(subscription, cursor, max_bytes, wait)
            .await
    }
}

/// One forwarded frame: the tree it was written to and its exact bytes.
type Forwarded = (String, Vec<u8>);

/// The only seam that can carry a validated frame to a child in this wave.
#[derive(Clone, Default)]
struct InputSink {
    written: Arc<Mutex<Vec<Forwarded>>>,
}

impl InputSink {
    fn frames(&self) -> Vec<Forwarded> {
        self.written.lock().unwrap().clone()
    }
}

#[async_trait::async_trait]
impl ProcessInputChannel for InputSink {
    async fn write_frame(&self, tree_id: &str, frame: &[u8]) -> Result<(), String> {
        self.written
            .lock()
            .unwrap()
            .push((tree_id.to_string(), frame.to_vec()));
        Ok(())
    }
}

/// The host-side launch capability covering exactly the fixture executable.
fn profile_authorization() -> AuthorizationService {
    let mut authorization = AuthorizationService::new();
    install_profile_capability(
        &mut authorization,
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
    authorization
}

fn activated() -> SafetyActivation {
    SafetyActivation::Activated {
        report_id: "t14-activated-stub".into(),
    }
}

/// The honest host verdict, from the production gate over a real store: this
/// wave's platform material is Unsupported, so the gate must refuse instead of
/// presenting itself as activated (INV-07).
fn production_verdict(store: &V1Store) -> SafetyActivation {
    let material = current_platform_material(boot().as_str());
    assert!(
        matches!(material.status, SafetyStatus::Unsupported { .. }),
        "this wave must report the platform honestly, never as Activated"
    );
    platform_activation_gate(store, boot().as_str())
}

/// One interactive service over a bound supervisor seam.
struct Harness {
    service: ManagedProcessService,
    backend: InteractiveBackend,
    journal: DeterministicSupervisorJournal,
    input: InputSink,
    scratch: tempfile::TempDir,
    bound_input: bool,
}

impl Harness {
    fn build(
        activation: SafetyActivation,
        profile: Option<ProcessProfileSchema>,
        bind_input: bool,
    ) -> Self {
        let scratch = tempfile::tempdir().expect("scratch root");
        let backend = InteractiveBackend::new();
        let journal = DeterministicSupervisorJournal::default();
        let sink = InputSink::default();
        let input: Option<Arc<dyn ProcessInputChannel>> =
            bind_input.then(|| Arc::new(sink.clone()) as Arc<dyn ProcessInputChannel>);
        let binding = ProcessSupervisorBinding {
            backend: Arc::new(backend.clone()),
            journal: Arc::new(journal.clone()),
            activation,
            task_id: TASK.into(),
            attempt_id: ATTEMPT.into(),
            ownership_epoch: EPOCH,
            scratch_root: scratch.path().to_path_buf(),
            output_capacity_bytes: CAPACITY,
            input,
        };
        let authorization = profile_authorization();
        let service = ManagedProcessService::supervised(
            Arc::new(authorization),
            EffectivePermissions::full(),
            WorkspaceCapability::WriteWithin {
                root: WORKSPACE.into(),
            },
            profile.map(|schema| validator("run-1", schema)),
            binding,
        );
        Self {
            service,
            backend,
            journal,
            input: sink,
            scratch,
            bound_input: bind_input,
        }
    }

    /// Supervised, admissible profile, with an input channel bound.
    async fn interactive() -> Self {
        Self::build(activated(), Some(scratch_only_profile()), true)
            .registered(ProcessProfileEffect::ScratchOnly)
            .await
    }

    /// Supervised and admissible, but with the backend's real v1 limitation:
    /// no `ProcessInputChannel` is bound.
    async fn without_input_channel() -> Self {
        Self::build(activated(), Some(scratch_only_profile()), false)
            .registered(ProcessProfileEffect::ScratchOnly)
            .await
    }

    /// No package-pinned schema, so no effect can be resolved at all.
    async fn without_pinned_schema() -> Self {
        Self::build(activated(), None, true)
            .registered(ProcessProfileEffect::ScratchOnly)
            .await
    }

    /// The profile P22 must never let this path resolve.
    async fn workspace_writer() -> Self {
        Self::build(activated(), Some(workspace_profile()), true)
            .registered(ProcessProfileEffect::CurrentCheckoutWrite)
            .await
    }

    async fn registered(self, effect: ProcessProfileEffect) -> Self {
        self.service
            .register_profile_executable(PROFILE, HELPER)
            .await;
        self.service
            .register_profile_effect(PROFILE, effect.as_str())
            .await
            .expect("pinned declaration");
        self
    }

    async fn open(&self, run: &str) -> String {
        self.backend.arm_next_identity(owner());
        self.service
            .open(token(run), PROFILE, vec!["serve".into()], None)
            .await
            .expect("supervised open")
    }

    fn tree(&self) -> RunningTree {
        self.backend
            .snapshot()
            .resumed
            .first()
            .cloned()
            .expect("one resumed tree")
    }

    fn record(&self, sequence: u64) -> SupervisorRecord {
        self.journal
            .load(&format!("process-tree-{ATTEMPT}-{EPOCH}-{sequence}"))
            .expect("journal alive")
            .expect("record persisted")
    }

    fn emit(&self, frames: &[ProcessOutputFrame]) {
        self.backend.emit(&self.tree(), frames);
    }

    fn set_proof(&self, proof: Option<TerminationProofRecord>) {
        self.backend.set_proof(&self.tree(), proof);
    }

    /// The tree's whole terminal output plus the proof naming exactly it.
    fn arm_close(&self, first: &[u8], exit_code: Option<i32>) {
        self.emit(&[data(0, first), exit(1, exit_code)]);
        let record = self.record(1);
        self.set_proof(Some(proof_for(&record)));
    }

    async fn write(&self, run: &str, handle: &str, data: Vec<u8>) -> Result<(), ProcessError> {
        self.service
            .write_validated(&token(run), handle, data)
            .await
    }

    async fn read(&self, run: &str, handle: &str, cursor: u64) -> ProcessReadReply {
        self.service
            .read(
                token(run),
                ProcessReadRequest {
                    handle: handle.into(),
                    cursor,
                    max_bytes: 1024,
                    wait_ms: Some(0),
                },
            )
            .await
            .expect("bounded read")
    }

    async fn close(&self, run: &str, handle: &str) -> Result<Option<i32>, ProcessError> {
        self.service.close_confirmed(&token(run), handle).await
    }

    fn handle_tree(handle: &str) -> &str {
        handle.split(':').nth(2).expect("a run:attempt:tree handle")
    }

    fn scratch_root(&self) -> String {
        canonical(self.scratch.path())
    }
}

/// Outbound and inbound IO on the supervised handle: the resolved spawn
/// material is journaled before the tree runs, one validated whole frame
/// reaches the bound child, and the tree's bytes come back through the same
/// handle until a proven close.
#[tokio::test]
async fn bidirectional_io_flows_through_the_managed_handle() {
    let harness = Harness::interactive().await;
    assert!(harness.bound_input);
    let handle = harness.open("run-1").await;
    assert_eq!(
        handle,
        format!("run-1:{ATTEMPT}:tree-{ATTEMPT}-{EPOCH}-1"),
        "a supervised handle names its attempt, epoch and sequence"
    );

    let record = harness.record(1);
    assert_eq!(record.phase, SupervisorPhase::Running);
    assert_eq!(record.tree_id, Harness::handle_tree(&handle));
    assert_eq!(record.ownership_epoch, EPOCH);
    assert_eq!(record.task_id, TASK);
    assert_eq!(record.owner.clone().expect("owner").boot_identity, boot());

    let log = harness.backend.snapshot();
    assert_eq!(log.prepared.len(), 1);
    assert_eq!(log.resumed.len(), 1);
    let spec = &log.prepared[0];
    assert_eq!(spec.executable, PathBuf::from(HELPER));
    assert_eq!(spec.arguments, vec!["serve".to_string()]);
    assert_eq!(canonical(&spec.cwd), harness.scratch_root());
    assert!(spec.inherited_objects.is_empty());
    assert_eq!(spec.output_capacity_bytes, CAPACITY);

    let out = frame("harness.start", json!({}));
    harness.write("run-1", &handle, out.clone()).await.unwrap();
    assert_eq!(harness.input.frames(), vec![(record.tree_id, out)]);

    let child_line = b"{\"ok\":true}\n";
    harness.emit(&[data(0, child_line)]);
    let page = harness.read("run-1", &handle, 0).await;
    assert_eq!(decoded(&page), child_line);
    assert_eq!(page.next_cursor, 1);
    assert!(!page.terminal);
    let read_back = harness.record(1);
    assert_eq!(read_back.output_cursor, 1);
    assert_eq!(read_back.phase, SupervisorPhase::Draining);

    harness.emit(&[exit(1, Some(0))]);
    harness.set_proof(Some(proof_for(&harness.record(1))));
    assert_eq!(harness.close("run-1", &handle).await.unwrap(), Some(0));
    let closed = harness.record(1);
    assert_eq!(closed.phase, SupervisorPhase::ProofAccepted);
    assert_eq!(closed.proof.clone().expect("proof").tree_id, closed.tree_id);
    let log = harness.backend.snapshot();
    assert_eq!(log.terminated, log.resumed);
    assert_eq!(log.proven, log.resumed);
}

/// A handle is valid only for the run, attempt and tree it names and for the
/// generation that opened it: a foreign caller forwards nothing, terminates
/// nothing and cannot even consume the record's phase.
#[tokio::test]
async fn cross_run_handles_are_rejected() {
    let harness = Harness::interactive().await;
    let handle = harness.open("run-1").await;
    let out = frame("harness.start", json!({}));

    let foreign = harness
        .write("run-2", &handle, out.clone())
        .await
        .expect_err("another run cannot write this handle");
    assert!(
        matches!(
            foreign,
            ProcessError::HandleScope(ref reason) if reason.contains("another run or attempt")
        ),
        "a cross-run handle must be refused by its scope: {foreign}"
    );
    assert!(matches!(
        harness.close("run-2", &handle).await,
        Err(ProcessError::HandleScope(_))
    ));

    let stale = GenerationToken {
        run_id: "run-1".into(),
        generation: 2,
    };
    assert!(matches!(
        harness
            .service
            .write_validated(&stale, &handle, out.clone())
            .await,
        Err(ProcessError::Fenced(_))
    ));
    assert!(matches!(
        harness.service.close_confirmed(&stale, &handle).await,
        Err(ProcessError::Fenced(_))
    ));
    assert!(matches!(
        harness
            .service
            .close_confirmed(&token("run-1"), "run-1:999")
            .await,
        Err(ProcessError::UnknownHandle(_))
    ));

    assert!(harness.input.frames().is_empty());
    assert_eq!(harness.record(1).phase, SupervisorPhase::Running);
    assert!(harness.backend.snapshot().terminated.is_empty());

    harness.write("run-1", &handle, out.clone()).await.unwrap();
    let tree_id = harness.record(1).tree_id;
    assert_eq!(harness.input.frames(), vec![(tree_id, out)]);
    harness.arm_close(b"bye\n", Some(0));
    assert_eq!(harness.close("run-1", &handle).await.unwrap(), Some(0));
    assert_eq!(harness.record(1).phase, SupervisorPhase::ProofAccepted);
    assert_eq!(harness.backend.snapshot().terminated.len(), 1);
}

/// NDJSON-RPC profiles validate every whole frame before anything is
/// forwarded, and the refusal is atomic: a buffer whose head is valid and
/// whose tail is partial forwards nothing at all.
#[tokio::test]
async fn ndjson_profiles_validate_frames_before_they_reach_the_child() {
    let harness = Harness::interactive().await;
    let handle = harness.open("run-1").await;
    let tree_id = harness.record(1).tree_id;

    let good = frame("harness.start", json!({}));
    harness.write("run-1", &handle, good.clone()).await.unwrap();

    let bound = frame("session.start", json!({"run": "run-999", "mode": "fast"}));
    let error = harness
        .write("run-1", &handle, bound)
        .await
        .expect_err("a host-bound field must be refused");
    assert!(
        matches!(&error, ProcessError::FrameRejected(reason) if reason.contains("host-bound")),
        "{error}"
    );

    assert!(matches!(
        harness
            .write("run-1", &handle, frame("reboot", json!({})))
            .await,
        Err(ProcessError::FrameRejected(reason)) if reason.contains("allowlist")
    ));

    let forbidden = frame(
        "session.start",
        json!({"run": "run-1", "mode": "fast", "token": "secret"}),
    );
    assert!(matches!(
        harness.write("run-1", &handle, forbidden).await,
        Err(ProcessError::FrameRejected(reason)) if reason.contains("must not be present")
    ));

    let mut smuggled = good.clone();
    smuggled.extend_from_slice(br#"{"jsonrpc":"2.0","id":9,"method":"har"#);
    assert!(matches!(
        harness.write("run-1", &handle, smuggled).await,
        Err(ProcessError::FrameRejected(reason)) if reason.contains("partial frame")
    ));

    let second = frame("session.start", json!({"run": "run-1", "mode": "slow"}));
    let both = [good.as_slice(), second.as_slice()].concat();
    harness.write("run-1", &handle, both.clone()).await.unwrap();

    let forwarded = harness.input.frames();
    assert_eq!(forwarded.len(), 2, "refused frames never reached the child");
    assert_eq!(forwarded[0], (tree_id, good));
    assert_eq!(forwarded[1].1, both);
    assert_eq!(harness.record(1).phase, SupervisorPhase::Running);

    harness.arm_close(b"{}\n", Some(0));
    assert_eq!(harness.close("run-1", &handle).await.unwrap(), Some(0));
}

/// Close reports a clean termination only when the backend's proof names this
/// tree; a missing or foreign proof terminates the tree, then refuses the
/// close and quarantines the record durably instead of claiming a kill.
#[tokio::test]
async fn closing_terminates_descendants_with_the_job() {
    let confirmed = Harness::interactive().await;
    let handle = confirmed.open("run-1").await;
    confirmed.arm_close(b"tail\n", Some(0));
    assert_eq!(confirmed.close("run-1", &handle).await.unwrap(), Some(0));
    assert_eq!(confirmed.record(1).phase, SupervisorPhase::ProofAccepted);

    let proofless = Harness::interactive().await;
    let handle = proofless.open("run-1").await;
    let record = proofless.record(1);
    let tree = proofless.tree();
    proofless.emit(&[data(0, b"tail\n"), exit(1, Some(7))]);
    proofless.set_proof(None);
    let error = proofless
        .close("run-1", &handle)
        .await
        .expect_err("an unprovable tree is never a clean close");
    assert!(
        matches!(&error, ProcessError::Unverifiable(tree) if *tree == record.tree_id),
        "{error}"
    );
    let durable = proofless.record(1);
    assert_eq!(durable.phase, SupervisorPhase::Quarantined);
    assert_eq!(
        durable.quarantine_reason.as_deref(),
        Some("termination-proof-failed")
    );
    assert_eq!(proofless.backend.snapshot().terminated, vec![tree.clone()]);
    assert_eq!(proofless.backend.snapshot().proven, vec![tree]);
    assert!(matches!(
        proofless.close("run-1", &handle).await,
        Err(ProcessError::UnknownHandle(_))
    ));

    let mismatched = Harness::interactive().await;
    let handle = mismatched.open("run-1").await;
    let record = mismatched.record(1);
    mismatched.emit(&[data(0, b"tail\n"), exit(1, Some(7))]);
    mismatched.set_proof(Some(foreign_proof(&record)));
    assert!(matches!(
        mismatched.close("run-1", &handle).await,
        Err(ProcessError::Unverifiable(_))
    ));
    let durable = mismatched.record(1);
    assert_eq!(durable.phase, SupervisorPhase::Quarantined);
    assert_eq!(
        durable.quarantine_reason.as_deref(),
        Some("termination-proof-mismatch")
    );
    assert!(
        durable.proof.is_none(),
        "a mismatched proof is never journaled"
    );
}

/// The disclosed v1 boundary as a proof: the process-tree backend has no write
/// seam, so a validated frame is refused with `NoInputChannel` instead of
/// being handed to anything, and whole-frame validation still precedes that
/// refusal.
#[tokio::test]
async fn validated_frames_refuse_when_no_input_channel_is_bound() {
    let unbound = Harness::without_input_channel().await;
    assert!(!unbound.bound_input);
    let handle = unbound.open("run-1").await;
    let tree_id = unbound.record(1).tree_id;

    let good = frame("harness.start", json!({}));
    let error = unbound
        .write("run-1", &handle, good.clone())
        .await
        .expect_err("nothing can accept a frame without an input channel");
    assert!(
        matches!(&error, ProcessError::NoInputChannel(tree) if *tree == tree_id),
        "{error}"
    );
    assert!(unbound.input.frames().is_empty());
    assert!(matches!(
        unbound
            .write("run-1", &handle, frame("reboot", json!({})))
            .await,
        Err(ProcessError::FrameRejected(_))
    ));
    assert_eq!(unbound.record(1).phase, SupervisorPhase::Running);
    assert!(unbound.input.frames().is_empty());

    let bound = Harness::interactive().await;
    let bound_handle = bound.open("run-1").await;
    bound
        .write("run-1", &bound_handle, good.clone())
        .await
        .expect("a bound channel accepts the validated frame");
    assert_eq!(
        bound.input.frames(),
        vec![(bound.record(1).tree_id, good)],
        "the bound channel is the only route a frame may take"
    );
}

/// Whatever the interactive path cannot prove is refused before it acts: an
/// unpinned profile, an unauthorized executable, a workspace-writing effect
/// and this host's honest gate verdict all start nothing, and an unbound
/// service cannot launch either.
#[tokio::test]
async fn launch_failures_and_unauthorized_profiles_fail_closed() {
    let harness = Harness::interactive().await;
    let missing = harness
        .service
        .open(token("run-1"), "missing-profile", vec![], None)
        .await
        .expect_err("an unpinned profile has no executable");
    assert!(missing
        .to_string()
        .contains("has no host-registered executable"));

    harness
        .service
        .register_profile_executable(PROFILE, "definitely-not-a-real-exe.xyz")
        .await;
    let denied = harness
        .service
        .open(token("run-1"), PROFILE, vec![], None)
        .await
        .expect_err("an executable outside every launch capability must not launch");
    assert!(denied.to_string().contains("authorization denied"));
    let log = harness.backend.snapshot();
    assert!(
        log.prepared.is_empty() && log.resumed.is_empty(),
        "INV-07: a refusal may not prepare or resume a tree"
    );

    let writer = Harness::workspace_writer().await;
    let hidden = writer
        .service
        .open(token("run-1"), PROFILE, vec![], None)
        .await
        .expect_err("workspace writes stay hidden until P24B");
    assert!(
        hidden.to_string().contains("P27 effect envelope"),
        "{hidden}"
    );
    assert!(writer.backend.snapshot().prepared.is_empty());

    // P22 narrowed the surface: with no package-pinned schema there is no
    // effect to resolve, so the launch boundary is never reached.
    let unpinned = Harness::without_pinned_schema().await;
    let unschema = unpinned
        .service
        .open(token("run-1"), PROFILE, vec![], None)
        .await
        .expect_err("an effect cannot be resolved without a pinned profile");
    assert!(unschema.to_string().contains("no pinned schema"));
    assert!(unpinned.backend.snapshot().prepared.is_empty());

    let gate_root = tempfile::tempdir().expect("tempdir");
    let store = V1Store::open(&gate_root.path().join("harness-v1.db")).expect("open store");
    let closed = production_verdict(&store);
    assert!(
        matches!(&closed, SafetyActivation::NotActivated { reason, .. }
            if *reason == "status-unsupported"),
        "Unsupported must never present itself as Activated: {closed:?}"
    );
    let gated = Harness::build(closed, Some(scratch_only_profile()), true)
        .registered(ProcessProfileEffect::ScratchOnly)
        .await;
    let refused = gated
        .service
        .open(token("run-1"), PROFILE, vec![], None)
        .await
        .expect_err("a closed activation gate starts nothing");
    let named_reason = "platform safety capability is not activated: \
                        activation is status-unsupported (status unsupported)";
    assert!(
        refused.to_string().contains(named_reason),
        "the refusal must name the gate reason: {refused}"
    );
    assert!(gated.backend.snapshot().prepared.is_empty());
    assert!(gated.input.frames().is_empty());

    let unbound = ManagedProcessService::new(
        Arc::new(profile_authorization()),
        EffectivePermissions::full(),
        WorkspaceCapability::WriteWithin {
            root: WORKSPACE.into(),
        },
        Some(validator("run-1", scratch_only_profile())),
    );
    unbound.register_profile_executable(PROFILE, HELPER).await;
    unbound
        .register_profile_effect(PROFILE, "scratch-only")
        .await
        .unwrap();
    let unbound_refusal = unbound
        .open(token("run-1"), PROFILE, vec![], None)
        .await
        .expect_err("no bound backend means no process");
    assert!(unbound_refusal
        .to_string()
        .contains("no process-tree backend is bound"));
}

#[derive(Default)]
struct ProbedProcess {
    opens: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl ProcessService for ProbedProcess {
    async fn open(
        &self,
        _token: GenerationToken,
        profile: &str,
        _arguments: Vec<String>,
        _cwd: Option<String>,
    ) -> Result<String, ServiceError> {
        self.opens.lock().unwrap().push(profile.to_string());
        Err(ServiceError::Failure(
            "must never resolve a hidden profile".into(),
        ))
    }

    async fn read(
        &self,
        _token: GenerationToken,
        _request: ProcessReadRequest,
    ) -> Result<ProcessReadReply, ServiceError> {
        Err(ServiceError::Unsupported("read".into()))
    }

    async fn write(
        &self,
        _token: GenerationToken,
        _handle: &str,
        _data: Vec<u8>,
    ) -> Result<(), ServiceError> {
        Err(ServiceError::Unsupported("write".into()))
    }

    async fn close(
        &self,
        _token: GenerationToken,
        _handle: &str,
    ) -> Result<Option<i32>, ServiceError> {
        Err(ServiceError::Unsupported("close".into()))
    }
}

fn router(processes: Arc<ProbedProcess>, pins: &[(&str, ProcessProfileEffect)]) -> HostRouter {
    let identity = RunIdentity {
        task_id: TASK.into(),
        branch_id: "branch-t14".into(),
        run_id: "run-t14".into(),
        attempt_id: ATTEMPT.into(),
        generation: 1,
    };
    let grants = vec![
        HostService::ProcessOpen,
        HostService::ProcessRead,
        HostService::ProcessWrite,
        HostService::ProcessClose,
    ];
    let router = HostRouter::new(
        identity,
        RunGuard::new("run-t14", 1),
        grants,
        Arc::new(FakeToolService::default()),
        Arc::new(FakeModelService::default()),
        processes,
        Arc::new(MemoryJournal::new()),
        Arc::new(IgnoreQuestions),
    );
    pins.iter().fold(router, |router, (profile, effect)| {
        router.pin_process_profile_effect(profile, *effect)
    })
}

fn open_request(profile: &str) -> RpcRequest {
    RpcRequest {
        jsonrpc: "2.0".into(),
        id: RpcId::Number(1),
        method: "host.process.open".into(),
        params: Some(json!({"profile": profile, "arguments": []})),
    }
}

/// Acceptance ③: a profile pinned above the interactive ceiling cannot be probed
/// into existence, and hiding it cannot cost an admissible profile its own
/// capability. Grants name services rather than profiles, so the request-time
/// hide is the authoritative layer; a run whose only pin is above the ceiling
/// additionally loses the Process surface outright.
#[tokio::test]
async fn workspace_write_profiles_stay_undiscoverable_through_the_router() {
    let unknown = {
        let probe = Arc::new(ProbedProcess::default());
        router(probe, &[("app-server", ProcessProfileEffect::ScratchOnly)])
            .handle_request(RpcRequest {
                jsonrpc: "2.0".into(),
                id: RpcId::Number(2),
                method: "host.process.nope".into(),
                params: None,
            })
            .await
            .expect_err("an unknown method is the only comparable refusal")
    };

    let mixed = Arc::new(ProbedProcess::default());
    let mixed_router = router(
        mixed.clone(),
        &[
            ("app-server", ProcessProfileEffect::ScratchOnly),
            (
                "checkout-writer",
                ProcessProfileEffect::CurrentCheckoutWrite,
            ),
        ],
    );
    let hidden = mixed_router
        .handle_request(open_request("checkout-writer"))
        .await
        .expect_err("a workspace-writing profile is never reachable");
    assert_eq!(
        hidden.code, unknown.code,
        "a hidden profile must answer exactly like a method that does not exist"
    );
    assert!(
        !hidden.message.contains("checkout-writer"),
        "the refusal must not name the hidden profile: {}",
        hidden.message
    );
    assert!(hidden.data.is_none(), "{hidden:?}");

    let reached = mixed_router
        .handle_request(open_request("app-server"))
        .await
        .expect_err("the stubbed service refuses");
    assert_eq!(
        reached.code,
        error_code::INTERNAL,
        "hiding one pin must not strip a capability the run was granted"
    );
    assert_eq!(
        *mixed.opens.lock().unwrap(),
        vec!["app-server".to_string()],
        "only the admissible profile may ever reach the service"
    );

    let alone = Arc::new(ProbedProcess::default());
    let alone_router = router(
        alone.clone(),
        &[(
            "checkout-writer",
            ProcessProfileEffect::CurrentCheckoutWrite,
        )],
    );
    let refused = alone_router
        .handle_request(open_request("checkout-writer"))
        .await
        .expect_err("a run with no admissible profile has no Process surface");
    assert_eq!(refused.code, unknown.code, "{refused:?}");
    assert!(
        alone.opens.lock().unwrap().is_empty(),
        "the service was never asked to resolve a hidden profile"
    );
}
