//! P05 — durable orchestration: persisted ownership before resume, drain and
//! proof before completion, quarantine at every ambiguous boundary.

use base64::Engine as _;
use r_code_harness_protocol::{
    ArtifactRef, ProcessOutputFrame, ProcessOutputStream, ProcessReadReply,
};
use r_code_runtime::process_guard::{
    BootIdentity, ProcessOwnerIdentity, TerminationProofKind, TerminationProofRecord,
};
use r_code_runtime::services::artifacts::{ArtifactStore, OutputTailPolicy, MAX_OUTPUT_TAIL_BYTES};
use r_code_runtime::services::process_supervisor::{
    CancellationSignal, DeterministicFakeBackend, DeterministicSupervisorJournal, FaultPoint,
    InheritedObject, JournalFaultPoint, OutputSubscription, PrepareDisposition, PreparedChild,
    PreparedLaunch, ProcessSupervisor, ProcessTreeBackend, RunningTree, SpawnSpec, SupervisedRun,
    SupervisorCompletion, SupervisorError, SupervisorJournal, SupervisorLimits, SupervisorPhase,
    SupervisorRecord, SupervisorStart,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BOOT: &str = "windows:01234567-89ab-4cde-8f01-23456789abcd";
const OTHER_BOOT: &str = "linux:fedcba98-7654-3210-fedc-ba9876543210";
const OP: &str = "op-s05";
const TREE: &str = "tree-s05";
const TASK: &str = "task-s05";
const EPOCH: u64 = 7;
const SECRET: &str = "s3cr3t-tail-value";

fn inherited(value: usize) -> InheritedObject {
    if cfg!(windows) {
        InheritedObject::WindowsHandle(value)
    } else {
        InheritedObject::UnixFd(i32::try_from(value).unwrap())
    }
}

fn spec(capacity: usize) -> SpawnSpec {
    SpawnSpec {
        executable: PathBuf::from("bin/fixture-child"),
        arguments: vec!["--serve".into()],
        cwd: PathBuf::from("workspace"),
        environment: BTreeMap::from([
            ("LANG".into(), "C.UTF-8".into()),
            ("PATH".into(), "bin".into()),
        ]),
        inherited_objects: vec![inherited(3), inherited(7)],
        output_capacity_bytes: capacity,
    }
}

fn owner(seed: u32) -> ProcessOwnerIdentity {
    ProcessOwnerIdentity::new(
        10_000 + seed,
        20_000 + u64::from(seed),
        BootIdentity::parse(BOOT).unwrap(),
        json!({"native": format!("owner-{seed}")}),
    )
    .unwrap()
}

/// Exact P01 nine-field envelope; the three-field P04 shape must be rejected.
fn p01_identity(owner: &ProcessOwnerIdentity) -> Value {
    json!({
        "treeId": TREE,
        "ownershipEpoch": EPOCH,
        "ownerPid": u64::from(owner.pid),
        "ownerStartIdentity": owner.start_identity,
        "ownerBootIdentity": owner.boot_identity.as_str(),
        "ownerPlatformIdentityDigest": owner.platform_identity_digest,
        "migratedObservedBootIdentity": Value::Null,
        "observedBootIdentity": owner.boot_identity.as_str(),
        "platformEvidence": json!({"native": "win-job"}),
    })
}

fn record_proof(
    identity: Value,
    tree_id: &str,
    epoch: u64,
    kind: TerminationProofKind,
    boot: BootIdentity,
) -> TerminationProofRecord {
    TerminationProofRecord {
        proof_id: format!("proof-{tree_id}"),
        proof_identity_digest: r_code_harness_protocol::canonical_input_hash(&identity),
        proof_identity: identity,
        tree_id: tree_id.into(),
        ownership_epoch: epoch,
        kind,
        observed_boot_identity: boot,
        recorded_at_ms: 42,
    }
}

fn p01_proof(owner: &ProcessOwnerIdentity) -> TerminationProofRecord {
    let boot = owner.boot_identity.clone();
    record_proof(
        p01_identity(owner),
        TREE,
        EPOCH,
        TerminationProofKind::Exit,
        boot,
    )
}

fn data(sequence: u64, bytes: &[u8]) -> ProcessOutputFrame {
    ProcessOutputFrame::Data {
        sequence,
        stream: ProcessOutputStream::Stdout,
        data_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
    }
}

fn eof(sequence: u64) -> ProcessOutputFrame {
    ProcessOutputFrame::Eof {
        sequence,
        stream: ProcessOutputStream::Stdout,
    }
}

fn exit(sequence: u64, code: Option<i32>) -> ProcessOutputFrame {
    ProcessOutputFrame::Exit {
        sequence,
        exit_code: code,
    }
}

fn limits() -> SupervisorLimits {
    SupervisorLimits {
        run_timeout: Duration::from_secs(2),
        termination_timeout: Duration::from_millis(500),
        read_wait: Duration::from_millis(10),
        max_read_bytes: 1024,
    }
}

fn tail_policy() -> OutputTailPolicy {
    OutputTailPolicy::new(
        64,
        vec![b"TOKEN=".to_vec()],
        vec![SECRET.as_bytes().to_vec()],
    )
    .unwrap()
}

fn no_cancel() -> CancellationSignal {
    CancellationSignal::default()
}

struct Harness {
    supervisor: ProcessSupervisor,
    backend: Arc<DeterministicFakeBackend>,
    journal: DeterministicSupervisorJournal,
    artifacts: ArtifactStore,
    policy: OutputTailPolicy,
    capacity: usize,
    _temp: tempfile::TempDir,
}

fn assemble(
    supervisor_backend: Arc<dyn ProcessTreeBackend>,
    raw: Arc<DeterministicFakeBackend>,
    journal: DeterministicSupervisorJournal,
    capacity: usize,
    task: &str,
) -> Harness {
    let temp = tempfile::tempdir().unwrap();
    Harness {
        supervisor: ProcessSupervisor::new(supervisor_backend, Arc::new(journal.clone())),
        backend: raw,
        journal,
        artifacts: ArtifactStore::for_task(temp.path(), task),
        policy: tail_policy(),
        capacity,
        _temp: temp,
    }
}

fn new_harness_with(capacity: usize, policy: OutputTailPolicy, task: &str) -> Harness {
    let journal = DeterministicSupervisorJournal::default();
    let backend = Arc::new(DeterministicFakeBackend::default());
    let mut harness = assemble(backend.clone(), backend, journal, capacity, task);
    harness.policy = policy;
    harness
}

fn new_harness() -> Harness {
    new_harness_with(4096, tail_policy(), TASK)
}

impl Harness {
    fn request(&self) -> SupervisorStart {
        SupervisorStart {
            operation_id: OP.into(),
            tree_id: TREE.into(),
            task_id: TASK.into(),
            ownership_epoch: EPOCH,
            spec: spec(self.capacity),
        }
    }

    async fn start_run(&self) -> SupervisedRun {
        self.backend.set_next_identity(owner(1));
        self.supervisor.start(self.request()).await.expect("start")
    }

    fn emit(&self, tree: &RunningTree, frames: &[ProcessOutputFrame]) {
        for frame in frames {
            self.backend.push_output(tree, frame.clone()).unwrap();
        }
    }

    fn push_terminal(&self, tree: &RunningTree) {
        self.emit(
            tree,
            &[data(0, b"hello TOKEN=s05 tail\n"), eof(1), exit(2, Some(0))],
        );
    }

    fn set_valid_proof(&self, tree: &RunningTree, owner: &ProcessOwnerIdentity) {
        self.backend
            .set_termination_proof(tree, Some(p01_proof(owner)))
            .unwrap();
    }

    async fn complete(&self, run: SupervisedRun) -> Result<SupervisorCompletion, SupervisorError> {
        self.complete_with(run, no_cancel()).await
    }

    async fn complete_with(
        &self,
        run: SupervisedRun,
        cancellation: CancellationSignal,
    ) -> Result<SupervisorCompletion, SupervisorError> {
        let tree = run.tree().clone();
        let owner = run.record().owner.clone().expect("owner persisted");
        self.push_terminal(&tree);
        self.set_valid_proof(&tree, &owner);
        self.finish(run, limits(), &cancellation).await
    }

    async fn finish_default(
        &self,
        run: SupervisedRun,
    ) -> Result<SupervisorCompletion, SupervisorError> {
        self.finish(run, limits(), &no_cancel()).await
    }

    async fn finish_paged(
        &self,
        run: SupervisedRun,
        limits: SupervisorLimits,
    ) -> Result<SupervisorCompletion, SupervisorError> {
        self.finish(run, limits, &no_cancel()).await
    }

    async fn finish(
        &self,
        run: SupervisedRun,
        limits: SupervisorLimits,
        cancellation: &CancellationSignal,
    ) -> Result<SupervisorCompletion, SupervisorError> {
        self.supervisor
            .finish(run, &self.artifacts, &self.policy, limits, cancellation)
            .await
    }

    fn durable(&self) -> SupervisorRecord {
        self.journal
            .reopen()
            .load(OP)
            .expect("journal alive")
            .expect("record exists")
    }
}

async fn restart(harness: &Harness) -> Result<SupervisedRun, SupervisorError> {
    ProcessSupervisor::new(harness.backend.clone(), Arc::new(harness.journal.reopen()))
        .start(harness.request())
        .await
}

async fn start_quarantines(harness: &Harness, context: &str) {
    assert_eq!(
        harness
            .supervisor
            .start(harness.request())
            .await
            .err()
            .expect(context),
        SupervisorError::Quarantined
    );
}

fn assert_quarantined(harness: &Harness, reason: &str) {
    let record = harness.durable();
    assert_eq!(record.phase, SupervisorPhase::Quarantined, "{reason}");
    assert_eq!(record.quarantine_reason.as_deref(), Some(reason));
}

// A backend wrapper that records the durable phase observed when resume_once
// is invoked, and can corrupt drain pages to probe validate_page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PageCorruption {
    StripTerminalExit,
    GapFirstSequence,
    PhantomExitMeta,
}

struct WrappedBackend {
    inner: DeterministicFakeBackend,
    corruption: Option<PageCorruption>,
    journal: DeterministicSupervisorJournal,
    resume_probe: Arc<Mutex<Option<SupervisorRecord>>>,
}

fn new_wrapped_harness(
    corruption: Option<PageCorruption>,
) -> (Harness, Arc<Mutex<Option<SupervisorRecord>>>) {
    let journal = DeterministicSupervisorJournal::default();
    let raw = DeterministicFakeBackend::default();
    let probe = Arc::new(Mutex::new(None));
    let wrapped = WrappedBackend {
        inner: raw.clone(),
        corruption,
        journal: journal.clone(),
        resume_probe: probe.clone(),
    };
    let harness = assemble(Arc::new(wrapped), Arc::new(raw), journal, 4096, TASK);
    (harness, probe)
}

fn bump_sequence(frame: &mut ProcessOutputFrame) {
    match frame {
        ProcessOutputFrame::Data { sequence, .. }
        | ProcessOutputFrame::Eof { sequence, .. }
        | ProcessOutputFrame::Exit { sequence, .. } => *sequence += 1,
    }
}

#[async_trait::async_trait]
impl ProcessTreeBackend for WrappedBackend {
    async fn prepare(&self, spec: SpawnSpec) -> Result<PreparedLaunch, SupervisorError> {
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
        *self.resume_probe.lock().unwrap() = self.journal.load(OP).unwrap();
        self.inner.resume_once(child).await
    }

    async fn terminate(&self, tree: &RunningTree) -> Result<(), SupervisorError> {
        self.inner.terminate(tree).await
    }

    async fn wait_and_prove(
        &self,
        tree: &RunningTree,
        timeout: Duration,
    ) -> Result<TerminationProofRecord, SupervisorError> {
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
        match self.corruption {
            Some(PageCorruption::StripTerminalExit) if page.terminal => {
                page.frames
                    .retain(|frame| !matches!(frame, ProcessOutputFrame::Exit { .. }));
                page.next_cursor = cursor + page.frames.len() as u64;
                page.exit_code = None;
            }
            Some(PageCorruption::GapFirstSequence) => {
                if let Some(frame) = page.frames.first_mut() {
                    bump_sequence(frame);
                }
            }
            Some(PageCorruption::PhantomExitMeta) if !page.terminal => {
                page.exit_code = Some(5);
            }
            _ => {}
        }
        Ok(page)
    }
}

#[tokio::test]
async fn owner_is_durable_before_resume_and_completion_follows_drain_and_proof() {
    let (harness, probe) = new_wrapped_harness(None);
    let run = harness.start_run().await;

    let at_resume = probe.lock().unwrap().clone().expect("resume was reached");
    assert_eq!(at_resume.phase, SupervisorPhase::ResumePending);
    assert_eq!(at_resume.owner, Some(owner(1)));

    let running = harness.durable();
    assert_eq!(running.phase, SupervisorPhase::Running);
    assert_eq!(running.owner, Some(owner(1)));
    assert!(running.proof.is_none() && running.output_tail.is_none());

    let completion = harness.complete(run).await.expect("completion");
    let record = &completion.record;
    assert_eq!(record.phase, SupervisorPhase::Completed);
    assert_eq!(record.revision, 10);
    assert_eq!(record.output_cursor, 3);
    assert!(record.quarantine_reason.is_none());
    assert_eq!(record.output_tail, Some(completion.output_tail.clone()));

    let tail = harness.artifacts.read_all(&completion.output_tail).unwrap();
    let tail_text = String::from_utf8_lossy(&tail);
    assert!(
        tail_text.contains("hello") && tail_text.contains("tail"),
        "{tail_text:?}"
    );
    assert!(
        !tail_text.contains("TOKEN="),
        "pattern leaked: {tail_text:?}"
    );
    assert!(!tail_text.contains(SECRET), "secret leaked: {tail_text:?}");
    assert!(tail.len() <= 64, "tail is unbounded: {tail_text:?}");

    let sidecar = std::fs::read_to_string(
        harness
            .artifacts
            .root()
            .join(format!("{}.owner", completion.output_tail.sha256)),
    )
    .unwrap();
    assert!(!sidecar.contains("TOKEN=") && !sidecar.contains(SECRET));

    assert_eq!(
        harness.supervisor.recover(OP).unwrap().phase,
        SupervisorPhase::Completed
    );
    assert!(matches!(
        restart(&harness).await,
        Err(SupervisorError::InvalidState(_))
    ));
}

#[tokio::test]
async fn terminal_pages_without_a_final_exit_or_with_gaps_quarantine() {
    for corruption in [
        PageCorruption::StripTerminalExit,
        PageCorruption::GapFirstSequence,
        PageCorruption::PhantomExitMeta,
    ] {
        let (harness, _) = new_wrapped_harness(Some(corruption));
        let run = harness.start_run().await;
        let tree = run.tree().clone();
        let owner = run.record().owner.clone().unwrap();
        harness.emit(&tree, &[data(0, b"hi\n"), data(1, b"yo\n")]);
        harness.emit(&tree, &[eof(2), exit(3, Some(0))]);
        harness.set_valid_proof(&tree, &owner);
        let error = harness
            .finish_paged(
                run,
                SupervisorLimits {
                    max_read_bytes: 4,
                    ..limits()
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error, SupervisorError::Quarantined, "{corruption:?}");
        assert_quarantined(&harness, "invalid-output-page");
    }
}

#[tokio::test]
async fn start_journal_faults_quarantine_durably_and_never_resume_twice() {
    let harness = new_harness();
    harness.journal.fail_once(JournalFaultPoint::Prepare);
    harness.backend.set_next_identity(owner(1));
    assert!(matches!(
        harness.supervisor.start(harness.request()).await,
        Err(SupervisorError::Journal(_))
    ));
    assert!(harness.journal.load(OP).unwrap().is_none());
    assert!(harness.supervisor.recover(OP).is_err());

    for (point, reason) in [
        (JournalFaultPoint::Suspended, "start-boundary-ambiguous"),
        (
            JournalFaultPoint::IdentityRecorded,
            "start-boundary-ambiguous",
        ),
        (JournalFaultPoint::ResumePending, "start-boundary-ambiguous"),
        (JournalFaultPoint::Running, "post-resume-journal-ambiguity"),
    ] {
        for lost_ack in [false, true] {
            let harness = new_harness();
            if lost_ack {
                harness.journal.lose_ack_once(point);
            } else {
                harness.journal.fail_once(point);
            }
            harness.backend.set_next_identity(owner(1));
            // With a lost ack the transition was still durably written, so
            // the visible outcome is the same quarantine, never a retry.
            start_quarantines(&harness, "start quarantines").await;
            assert_quarantined(&harness, reason);
            assert!(restart(&harness).await.is_err());
            assert_eq!(
                harness.supervisor.recover(OP).unwrap().phase,
                SupervisorPhase::Quarantined
            );
        }
    }

    // A lost prepare ack leaves a durable nonterminal record: recovery and
    // restart both fail closed before the backend is ever touched again.
    let harness = new_harness();
    harness.journal.lose_ack_once(JournalFaultPoint::Prepare);
    assert!(matches!(
        harness.supervisor.start(harness.request()).await,
        Err(SupervisorError::Journal(_))
    ));
    assert_eq!(harness.durable().phase, SupervisorPhase::Prepared);
    assert_eq!(
        harness.supervisor.recover(OP).unwrap().phase,
        SupervisorPhase::Quarantined
    );
    assert_quarantined(&harness, "recovered-nonterminal-operation");

    let harness = new_harness();
    let run = harness.start_run().await;
    drop(run);
    assert_eq!(
        restart(&harness).await.err().expect("retry refused"),
        SupervisorError::Quarantined
    );
    assert_quarantined(&harness, "restart-or-retry-before-completion");
}

#[tokio::test]
async fn backend_faults_in_start_quarantine_before_any_resume() {
    for point in [
        FaultPoint::Prepare,
        FaultPoint::SpawnSuspended,
        FaultPoint::ProbeIdentity,
        FaultPoint::PersistIdentity,
        FaultPoint::ResumeOnce,
    ] {
        let harness = new_harness();
        harness.backend.fail_once(point);
        harness.backend.set_next_identity(owner(1));
        start_quarantines(&harness, "start quarantines").await;
        assert_quarantined(&harness, "start-boundary-ambiguous");
        // A retry with the same operation id is refused before any respawn.
        assert_eq!(
            restart(&harness).await.err().expect("retry refused"),
            SupervisorError::Quarantined
        );
        assert_quarantined(&harness, "start-boundary-ambiguous");
    }
}

#[tokio::test]
async fn finish_fault_matrix_quarantines_every_ambiguous_boundary() {
    let journal_reasons = [
        (JournalFaultPoint::Draining, "draining-journal-ambiguity"),
        (
            JournalFaultPoint::OutputCursor,
            "output-cursor-journal-ambiguity",
        ),
        (JournalFaultPoint::Drained, "drained-journal-ambiguity"),
        (JournalFaultPoint::ProofAccepted, "proof-journal-ambiguity"),
    ];
    for (point, reason) in journal_reasons {
        for lost_ack in [false, true] {
            let harness = new_harness();
            let run = harness.start_run().await;
            if lost_ack {
                harness.journal.lose_ack_once(point);
            } else {
                harness.journal.fail_once(point);
            }
            assert_eq!(
                harness.complete(run).await.unwrap_err(),
                SupervisorError::Quarantined,
                "{point:?} lost_ack={lost_ack}"
            );
            assert_quarantined(&harness, reason);
        }
    }

    let harness = new_harness();
    let run = harness.start_run().await;
    harness.journal.fail_once(JournalFaultPoint::Completed);
    assert_eq!(
        harness.complete(run).await.unwrap_err(),
        SupervisorError::Quarantined
    );
    assert_quarantined(&harness, "completion-journal-ambiguity");

    // A lost Completed ack is the one place quarantine must refuse: the
    // record is durably Completed, finish still reports failure, and recover
    // returns the Completed record with its persisted tail.
    let harness = new_harness();
    let run = harness.start_run().await;
    harness.journal.lose_ack_once(JournalFaultPoint::Completed);
    assert!(matches!(
        harness.complete(run).await,
        Err(SupervisorError::Journal(_))
    ));
    let record = harness.durable();
    assert_eq!(record.phase, SupervisorPhase::Completed);
    assert_eq!(
        harness.supervisor.recover(OP).unwrap().phase,
        SupervisorPhase::Completed
    );
    let tail = harness
        .artifacts
        .read_all(&record.output_tail.clone().unwrap())
        .unwrap();
    let tail_text = String::from_utf8_lossy(&tail);
    assert!(tail_text.contains("hello") && !tail_text.contains("TOKEN="));

    for (point, reason) in [
        (FaultPoint::Drain, "output-drain-failed"),
        (FaultPoint::WaitAndProve, "termination-proof-failed"),
    ] {
        let harness = new_harness();
        let run = harness.start_run().await;
        harness.backend.fail_once(point);
        assert_eq!(
            harness.complete(run).await.unwrap_err(),
            SupervisorError::Quarantined,
            "{point:?}"
        );
        assert_quarantined(&harness, reason);
    }
}

#[tokio::test]
async fn forged_or_malformed_proofs_never_reach_completed() {
    let owner = owner(1);
    let other_boot = BootIdentity::parse(OTHER_BOOT).unwrap();
    let mut wrong_tree = p01_identity(&owner);
    wrong_tree["treeId"] = json!("tree-other");
    let mut wrong_epoch = p01_identity(&owner);
    wrong_epoch["ownershipEpoch"] = json!(EPOCH + 1);
    let mut wrong_boot = p01_identity(&owner);
    wrong_boot["observedBootIdentity"] = json!(other_boot.as_str());
    let mut short_envelope = p01_identity(&owner);
    short_envelope
        .as_object_mut()
        .unwrap()
        .remove("platformEvidence");
    let mut long_envelope = p01_identity(&owner);
    long_envelope["extra"] = json!(true);
    let mut tampered_pid = p01_identity(&owner);
    tampered_pid["ownerPid"] = json!(u64::from(owner.pid) + 1);
    let p04_shape = json!({
        "treeId": TREE,
        "ownershipEpoch": EPOCH,
        "nativeExit": true,
    });
    use TerminationProofKind::{Exit, Reboot};
    let variants = [
        (
            wrong_tree,
            "tree-other",
            EPOCH,
            Exit,
            owner.boot_identity.clone(),
        ),
        (
            wrong_epoch,
            TREE,
            EPOCH + 1,
            Exit,
            owner.boot_identity.clone(),
        ),
        (wrong_boot, TREE, EPOCH, Exit, other_boot),
        (
            short_envelope,
            TREE,
            EPOCH,
            Exit,
            owner.boot_identity.clone(),
        ),
        (
            long_envelope,
            TREE,
            EPOCH,
            Exit,
            owner.boot_identity.clone(),
        ),
        (tampered_pid, TREE, EPOCH, Exit, owner.boot_identity.clone()),
        (
            p01_identity(&owner),
            TREE,
            EPOCH,
            Reboot,
            owner.boot_identity.clone(),
        ),
        (p04_shape, TREE, EPOCH, Exit, owner.boot_identity.clone()),
    ];

    for (identity, tree_id, epoch, kind, boot) in variants {
        let harness = new_harness();
        let run = harness.start_run().await;
        let tree = run.tree().clone();
        harness.push_terminal(&tree);
        harness
            .backend
            .set_termination_proof(
                &tree,
                Some(record_proof(identity, tree_id, epoch, kind, boot)),
            )
            .unwrap();
        assert_eq!(
            harness.finish_default(run).await.unwrap_err(),
            SupervisorError::Quarantined
        );
        assert_quarantined(&harness, "termination-proof-mismatch");
        assert_ne!(
            harness.supervisor.recover(OP).unwrap().phase,
            SupervisorPhase::Completed
        );
    }
}

#[tokio::test]
async fn cancellation_timeouts_and_unverifiable_termination_are_bounded() {
    let harness = new_harness();
    let run = harness.start_run().await;
    let cancelled = no_cancel();
    cancelled.cancel();
    let started = Instant::now();
    let completion = harness.complete_with(run, cancelled).await.unwrap();
    assert_eq!(completion.record.phase, SupervisorPhase::Completed);
    assert!(started.elapsed() < Duration::from_secs(3));

    // No Exit frame is ever emitted: run timeout fires, terminate is issued,
    // the termination deadline expires and the operation quarantines.
    let harness = new_harness();
    let run = harness.start_run().await;
    harness
        .backend
        .push_output(run.tree(), data(0, b"partial"))
        .unwrap();
    let stalled = SupervisorLimits {
        run_timeout: Duration::from_millis(50),
        termination_timeout: Duration::from_millis(200),
        read_wait: Duration::from_millis(5),
        max_read_bytes: 1024,
    };
    let started = Instant::now();
    assert_eq!(
        harness.finish_paged(run, stalled).await.unwrap_err(),
        SupervisorError::Timeout
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_quarantined(&harness, "terminal-drain-timeout");

    let harness = new_harness();
    let run = harness.start_run().await;
    let tree = run.tree().clone();
    harness.push_terminal(&tree);
    harness.backend.set_termination_proof(&tree, None).unwrap();
    assert_eq!(
        harness.finish_default(run).await.unwrap_err(),
        SupervisorError::Quarantined
    );
    assert_quarantined(&harness, "termination-proof-failed");
}

#[tokio::test]
async fn backpressure_and_multi_page_drains_stay_bounded() {
    let harness = new_harness_with(8, tail_policy(), TASK);
    let run = harness.start_run().await;
    let tree = run.tree().clone();
    harness.emit(&tree, &[data(0, b"abcd"), data(1, b"efgh")]);
    assert_eq!(
        harness.backend.push_output(&tree, data(2, b"X")),
        Err(SupervisorError::Backpressure)
    );
    assert_eq!(
        harness.backend.push_output(&tree, data(2, b"X")),
        Err(SupervisorError::Backpressure),
        "retry must not mutate or escape the bound"
    );
    harness.emit(&tree, &[eof(2), exit(3, Some(0))]);
    let owner = run.record().owner.clone().unwrap();
    harness.set_valid_proof(&tree, &owner);
    let completion = harness.finish_default(run).await.unwrap();
    assert_eq!(completion.record.phase, SupervisorPhase::Completed);
    assert_eq!(completion.record.output_cursor, 4);

    let harness = new_harness();
    let run = harness.start_run().await;
    let tree = run.tree().clone();
    let bulk: Vec<ProcessOutputFrame> = (0..3u64).map(|s| data(s, &[b'x'; 16])).collect();
    harness.emit(&tree, &bulk);
    harness.emit(&tree, &[eof(3), exit(4, Some(0))]);
    let owner = run.record().owner.clone().unwrap();
    harness.set_valid_proof(&tree, &owner);
    let completion = harness
        .finish_paged(
            run,
            SupervisorLimits {
                max_read_bytes: 16,
                ..limits()
            },
        )
        .await
        .unwrap();
    // Draining stopped exactly once, at the single terminal page.
    assert_eq!(completion.record.phase, SupervisorPhase::Completed);
    assert_eq!(completion.record.output_cursor, 5);
}

#[tokio::test]
async fn redaction_removes_secrets_across_chunks_and_boundaries_without_leaking() {
    // A secret split across two pushed frames is re-formed in the rolling
    // window and removed before the tail is persisted.
    let policy = OutputTailPolicy::new(
        64,
        vec![b"TOKEN=".to_vec()],
        vec![b"S3cr3t-hunter2-token".to_vec()],
    )
    .unwrap();
    let harness = new_harness_with(4096, policy.clone(), TASK);
    let run = harness.start_run().await;
    let tree = run.tree().clone();
    let owner = run.record().owner.clone().unwrap();
    harness.emit(
        &tree,
        &[
            data(0, b"prefix TOKEN=abc S3cr3t-hunt"),
            data(1, b"er2-token suffix\n"),
        ],
    );
    harness.emit(&tree, &[eof(2), exit(3, Some(0))]);
    harness.set_valid_proof(&tree, &owner);
    let completion = harness.finish_default(run).await.unwrap();
    let tail = harness.artifacts.read_all(&completion.output_tail).unwrap();
    let tail_text = String::from_utf8_lossy(&tail);
    assert!(tail_text.contains("prefix") && tail_text.contains("suffix"));
    assert!(!tail_text.contains("S3cr3t-hunter2-token"), "{tail_text:?}");
    assert!(!tail_text.contains("TOKEN="), "{tail_text:?}");
    assert!(tail.len() <= 64);
    let debug = format!("{policy:?}");
    assert!(!debug.contains("S3cr3t") && !debug.contains("TOKEN="));

    let temp = tempfile::tempdir().unwrap();
    let store = ArtifactStore::for_task(temp.path(), TASK);
    // The secret straddles the tail cut: it is longer than the bytes that
    // remain before the truncation point, yet cannot survive redaction.
    let straddling =
        OutputTailPolicy::new(16, vec![], vec![b"TOPSECRET-BOUNDARY19".to_vec()]).unwrap();
    let mut window = vec![b'x'; 30];
    window.extend_from_slice(b"TOPSECRET-BOUNDARY19");
    window.extend_from_slice(b"yyy");
    let reference = store
        .put_redacted_output_tail(&window, &straddling)
        .unwrap();
    let tail = store.read_all(&reference).unwrap();
    assert!(tail.len() <= 16, "{tail:?}");
    assert!(
        !String::from_utf8_lossy(&tail).contains("TOPSECRET-BOUNDARY19"),
        "{tail:?}"
    );
    assert_eq!(straddling.capture_window_bytes(), 16 + 19);

    // Overlapping literals and secrets re-formed after replacement. The
    // matched literal is truncated whole, then one replacement byte is pushed
    // and the result is re-checked, so a rule formed across the replacement
    // ("0c" after "ab" became 0x00) is removed as well.
    let overlapping =
        OutputTailPolicy::new(32, vec![b"ABC".to_vec(), b"BCDE".to_vec()], vec![]).unwrap();
    let reference = store
        .put_redacted_output_tail(b"ABCDE", &overlapping)
        .unwrap();
    assert_eq!(store.read_all(&reference).unwrap(), [0, b'D', b'E']);
    let reforming =
        OutputTailPolicy::new(32, vec![b"ab".to_vec(), vec![0u8, b'c']], vec![]).unwrap();
    let reference = store.put_redacted_output_tail(b"abcZ", &reforming).unwrap();
    assert_eq!(store.read_all(&reference).unwrap(), [0, b'Z']);

    assert!(OutputTailPolicy::new(0, vec![], vec![]).is_err());
    assert!(OutputTailPolicy::new(MAX_OUTPUT_TAIL_BYTES + 1, vec![], vec![]).is_err());
    assert!(OutputTailPolicy::new(16, vec![Vec::new()], vec![]).is_err());
    let many = vec![b"r".to_vec(); 129];
    assert!(OutputTailPolicy::new(16, many, vec![]).is_err());
    let huge = vec![vec![b's'; 4097]];
    assert!(OutputTailPolicy::new(16, vec![], huge).is_err());
}

#[tokio::test]
async fn ownership_and_limit_negatives_fail_closed() {
    let harness = new_harness_with(4096, tail_policy(), "other-task");
    let run = harness.start_run().await;
    assert_eq!(
        harness.complete(run).await.unwrap_err(),
        SupervisorError::Quarantined
    );
    assert_quarantined(&harness, "output-tail-owner-mismatch");

    let mut harness = new_harness();
    let root = harness.artifacts.root().to_path_buf();
    let run = harness.start_run().await;
    harness.artifacts = ArtifactStore::new(root);
    assert_eq!(
        harness.complete(run).await.unwrap_err(),
        SupervisorError::Quarantined
    );
    assert_quarantined(&harness, "output-tail-owner-mismatch");

    let invalid = [
        SupervisorLimits {
            run_timeout: Duration::ZERO,
            ..limits()
        },
        SupervisorLimits {
            termination_timeout: Duration::ZERO,
            ..limits()
        },
        SupervisorLimits {
            max_read_bytes: 0,
            ..limits()
        },
        SupervisorLimits {
            read_wait: Duration::from_millis(30_001),
            ..limits()
        },
    ];
    for limits in invalid {
        let harness = new_harness();
        let run = harness.start_run().await;
        assert!(matches!(
            harness.finish_paged(run, limits).await,
            Err(SupervisorError::InvalidSpec(_))
        ));
        // Rejected before any state change: the record stays Running.
        let record = harness.durable();
        assert_eq!(record.phase, SupervisorPhase::Running);
        assert_eq!(record.revision, 5);
    }

    let harness = new_harness();
    let nameless = SupervisorStart {
        operation_id: "  ".into(),
        ..harness.request()
    };
    assert!(matches!(
        harness.supervisor.start(nameless).await,
        Err(SupervisorError::InvalidSpec(_))
    ));
    let zero_epoch = SupervisorStart {
        ownership_epoch: 0,
        ..harness.request()
    };
    assert!(matches!(
        harness.supervisor.start(zero_epoch).await,
        Err(SupervisorError::InvalidSpec(_))
    ));
    assert!(harness.journal.load(OP).unwrap().is_none());
}

fn digest() -> String {
    r_code_harness_protocol::canonical_input_hash(&json!("digest"))
}

fn tail_reference() -> ArtifactRef {
    let sha256 = digest();
    ArtifactRef {
        schema: ArtifactRef::SCHEMA,
        blob_id: format!("blob:sha256:{sha256}"),
        bytes: 4,
        sha256,
        media_type: None,
    }
}

#[test]
fn journal_cas_restricts_fields_by_phase() {
    let journal = DeterministicSupervisorJournal::default();
    let prepared = SupervisorRecord {
        operation_id: "op-cas".into(),
        tree_id: TREE.into(),
        task_id: TASK.into(),
        ownership_epoch: EPOCH,
        content_digest: digest(),
        phase: SupervisorPhase::Prepared,
        revision: 1,
        output_cursor: 0,
        owner: None,
        proof: None,
        output_tail: None,
        quarantine_reason: None,
        write_disabled: None,
    };
    let mut current = match journal.prepare(prepared.clone()).unwrap() {
        PrepareDisposition::Created(record) => record,
        PrepareDisposition::Existing(_) => panic!("fresh journal returned Existing"),
    };
    assert!(matches!(
        journal.prepare(prepared.clone()),
        Ok(PrepareDisposition::Existing(_))
    ));
    let mut ownerful = prepared.clone();
    ownerful.owner = Some(owner(3));
    assert!(journal.prepare(ownerful).is_err());
    let conflicting = SupervisorRecord {
        tree_id: "tree-conflict".into(),
        ..prepared.clone()
    };
    assert!(journal.prepare(conflicting).is_err());

    let step = |record: &SupervisorRecord, phase: SupervisorPhase| {
        let mut next = record.clone();
        next.phase = phase;
        next.revision += 1;
        next
    };

    let mut jump = step(&current, SupervisorPhase::Completed);
    jump.owner = Some(owner(3));
    jump.proof = Some(p01_proof(&owner(3)));
    jump.output_tail = Some(tail_reference());
    assert!(journal.compare_and_swap(&current, jump).is_err());

    let mut owner_early = step(&current, SupervisorPhase::Suspended);
    owner_early.owner = Some(owner(3));
    assert!(journal.compare_and_swap(&current, owner_early).is_err());

    let next = step(&current, SupervisorPhase::Suspended);
    current = journal.compare_and_swap(&current, next).unwrap();
    let mut identity = step(&current, SupervisorPhase::IdentityRecorded);
    identity.owner = Some(owner(3));
    current = journal.compare_and_swap(&current, identity).unwrap();

    let mut proof_early = step(&current, SupervisorPhase::Running);
    proof_early.proof = Some(p01_proof(&owner(3)));
    assert!(journal.compare_and_swap(&current, proof_early).is_err());
    let mut reasoned = step(&current, SupervisorPhase::Running);
    reasoned.quarantine_reason = Some("smuggled".into());
    assert!(journal.compare_and_swap(&current, reasoned).is_err());

    let next = step(&current, SupervisorPhase::ResumePending);
    current = journal.compare_and_swap(&current, next).unwrap();
    let mut tailed = step(&current, SupervisorPhase::Running);
    tailed.output_tail = Some(tail_reference());
    assert!(journal.compare_and_swap(&current, tailed).is_err());
    let next = step(&current, SupervisorPhase::Running);
    current = journal.compare_and_swap(&current, next).unwrap();

    let mut cursor_record = step(&current, SupervisorPhase::Draining);
    cursor_record.output_cursor = 2;
    current = journal.compare_and_swap(&current, cursor_record).unwrap();
    let mut regression = step(&current, SupervisorPhase::Draining);
    regression.output_cursor = 1;
    assert!(journal.compare_and_swap(&current, regression).is_err());
    let mut skip = step(&current, SupervisorPhase::Drained);
    skip.revision += 1;
    assert!(journal.compare_and_swap(&current, skip).is_err());
    let mut stale = current.clone();
    stale.revision += 1;
    let drained = step(&current, SupervisorPhase::Drained);
    assert!(journal.compare_and_swap(&stale, drained).is_err());
    let next = step(&current, SupervisorPhase::Drained);
    current = journal.compare_and_swap(&current, next).unwrap();
    let proofless = step(&current, SupervisorPhase::ProofAccepted);
    assert!(journal.compare_and_swap(&current, proofless).is_err());
    let mut quarantined = step(&current, SupervisorPhase::Quarantined);
    quarantined.quarantine_reason = Some("boundary".into());
    assert!(journal.compare_and_swap(&current, quarantined).is_ok());
}

#[test]
fn supervisor_wiring_stays_out_of_the_host_shell_and_the_reference_journal_is_not_a_store() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let supervisor = std::fs::read_to_string(root.join("services/process_supervisor.rs")).unwrap();
    for forbidden in ["r_code_store", "rusqlite", "tokio::process", "std::process"] {
        assert!(
            !supervisor.contains(forbidden),
            "reference journal must not pose as production durability: {forbidden}"
        );
    }

    // P22 moved the interactive Process service onto the supervisor, so the
    // journal it persists into is the bound one and the fault paths below are
    // reachable in production code — the reference journal is still the only
    // implementation, which is precisely why the host shell must stay out.
    let processes = std::fs::read_to_string(root.join("services/processes.rs")).unwrap();
    assert!(
        processes.contains("SupervisorJournal") && processes.contains("ProcessSupervisor::new("),
        "the interactive service must journal through the supervisor"
    );
    let router = std::fs::read_to_string(root.join("plugins/router.rs")).unwrap();
    assert!(
        router.contains("ProcessProfileEffect"),
        "the router must resolve the pinned process effect"
    );
    for path in [
        root.join("run_manager.rs"),
        root.join("bin/r-code-service.rs"),
    ] {
        let source = std::fs::read_to_string(&path).unwrap();
        assert!(
            !source.contains("ProcessSupervisor") && !source.contains("SupervisorJournal"),
            "premature supervisor wiring in {}",
            path.display()
        );
    }
}
