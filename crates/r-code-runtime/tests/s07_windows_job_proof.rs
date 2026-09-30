//! P07 — creation-time Job composition, containment, and full-tree proof.
//!
//! Proves on real Windows processes: the job assignment happens inside
//! CreateProcessW (the suspended primary is a member before any user code),
//! immediate grandchildren and a job-creating nested-job descendant stay
//! contained, daemon death (job handle close) sweeps the whole tree,
//! wait_all_members only certifies an empty member list, and the
//! WindowsJobBackend drives the P05 supervisor end-to-end with the P01
//! nine-field termination proof. Refusals stay refusals: an environment the
//! seam cannot guard surfaces as Unsupported, and an unprovable termination
//! blocks completion instead of guessing.

#![cfg(windows)]

use r_code_harness_protocol::{
    canonical_input_hash, PROCESS_READ_MAX_BYTES, PROCESS_READ_MAX_WAIT_MS,
};
use r_code_runtime::process_guard::windows::{
    current_process_in_job, is_process_alive, spawn_suspended_with_job, GuardianError, PipeRead,
    RawSpawnSpec,
};
use r_code_runtime::process_guard::TerminationProofKind;
use r_code_runtime::services::artifacts::{ArtifactStore, OutputTailPolicy};
use r_code_runtime::services::process_supervisor::{
    CancellationSignal, DeterministicSupervisorJournal, InheritedObject, PreparedChildKind,
    ProcessSupervisor, ProcessTreeBackend, SpawnSpec, SupervisorError, SupervisorJournal,
    SupervisorLimits, SupervisorPhase, SupervisorRecord, SupervisorStart, WindowsJobBackend,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const HELPER: &str = env!("CARGO_BIN_EXE_process-tree-helper");
const MARKER: &[u8] = b"resumed\n";
const BUDGET: Duration = Duration::from_secs(5);
/// Hard cap for any single full-tree death proof.
const WAIT_ALL: Duration = Duration::from_secs(10);
const OP: &str = "op-s07";
const TASK: &str = "task-s07";

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn spawn_jobbed(
    arguments: &[String],
    environment: &BTreeMap<String, String>,
) -> r_code_runtime::process_guard::windows::JobbedSuspendedChild {
    let spec = RawSpawnSpec {
        executable: Path::new(HELPER),
        arguments,
        cwd: None,
        environment,
    };
    spawn_suspended_with_job(&spec).expect("spawn the helper suspended inside its job")
}

/// True once `at_least` bytes are buffered on the child's stdout.
fn wait_for_available(
    child: &r_code_runtime::process_guard::windows::JobbedSuspendedChild,
    at_least: u32,
    budget: Duration,
) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if child.stdout_available_bytes() >= at_least {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.stdout_available_bytes() >= at_least
}

/// The member list once it holds `at_least` pids (None on timeout).
fn wait_for_members(
    child: &mut r_code_runtime::process_guard::windows::JobbedSuspendedChild,
    at_least: usize,
    budget: Duration,
) -> Option<Vec<u32>> {
    let deadline = Instant::now() + budget;
    loop {
        let members = child.member_pids().expect("member list query");
        if members.len() >= at_least {
            return Some(members);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// True once the pid is gone (a reused pid counts as alive; the poll window
/// is seconds, so reuse inside it is not a realistic hazard).
fn poll_dead(pid: u32, deadline: Instant) -> bool {
    while Instant::now() < deadline && is_process_alive(pid) {
        std::thread::sleep(Duration::from_millis(20));
    }
    !is_process_alive(pid)
}

// Containment fixtures -------------------------------------------------------

#[test]
fn job_precedes_user_code_and_contains_immediate_grandchildren() {
    let environment = BTreeMap::new();
    let arguments = args(&["--grandchild", "3", "--sleep", "2500"]);
    let mut child = spawn_jobbed(&arguments, &environment);

    // Assignment precedes user code: the still-suspended primary is already
    // the job's only member, and its durable identity is recordable pre-resume.
    let (job, pid, start) = child.job_identity();
    assert_ne!(job, 0, "job handle identity must be real");
    assert_ne!(pid, 0);
    assert_ne!(start, 0, "start-time identity must be real");
    assert!(
        child.is_in_job(),
        "the creation-time assignment must be observable before resume"
    );
    assert_eq!(
        child.member_pids().expect("member list"),
        vec![pid],
        "a fresh job lists exactly the suspended primary"
    );
    assert_eq!(
        child.stdout_available_bytes(),
        0,
        "no user code may run before resume"
    );

    child.resume_once().expect("resume the helper");
    assert!(
        wait_for_available(&child, (4 * MARKER.len()) as u32, BUDGET),
        "the helper and each of its three grandchildren must mark resume"
    );

    let members = wait_for_members(&mut child, 4, BUDGET)
        .expect("helper + three grandchildren must be job members");
    assert!(members.contains(&pid), "primary {pid} in {members:?}");

    child.terminate();
    assert!(
        child.wait_all_members(Instant::now() + WAIT_ALL),
        "termination of the tree must be proven, not assumed"
    );
    assert!(
        child.member_pids().expect("member list").is_empty(),
        "an empty member list is the only accepted death proof"
    );
    // Release the job (Drop closes its handle) before probing pids: a
    // terminated member's process object stays openable while the job
    // object exists, so bare OpenProcess success is not death evidence.
    drop(child);
    let deadline = Instant::now() + BUDGET;
    for member in members {
        assert!(
            poll_dead(member, deadline),
            "member {member} survived the job"
        );
    }
}

#[test]
fn nested_job_descendants_stay_contained_in_the_outer_job() {
    let environment = BTreeMap::new();
    let arguments = args(&["--nested-job", "--sleep", "2500"]);
    let mut child = spawn_jobbed(&arguments, &environment);
    child.resume_once().expect("resume the helper");

    assert!(
        wait_for_available(&child, (2 * MARKER.len()) as u32, BUDGET),
        "the helper and its nested-job child must both mark resume"
    );
    let members = wait_for_members(&mut child, 2, BUDGET)
        .expect("a job-creating descendant and its child must stay members");

    child.terminate();
    assert!(
        child.wait_all_members(Instant::now() + WAIT_ALL),
        "the outer job must prove death over its nested-job subtree"
    );
    assert!(child.member_pids().expect("member list").is_empty());
    // Drop before probing: the job pins terminated members' objects open
    // until its own handle closes (see the grandchild test).
    drop(child);
    let deadline = Instant::now() + BUDGET;
    for member in members {
        assert!(
            poll_dead(member, deadline),
            "member {member} survived the job"
        );
    }
}

#[test]
fn daemon_death_kill_on_close_terminates_the_whole_tree() {
    let environment = BTreeMap::new();
    let arguments = args(&["--grandchild", "2", "--sleep", "10000"]);
    let mut child = spawn_jobbed(&arguments, &environment);
    child.resume_once().expect("resume the helper");

    let members = wait_for_members(&mut child, 3, BUDGET)
        .expect("the helper and its two sleeping grandchildren must be members");

    // Daemon death: drop the wrapper WITHOUT calling terminate. For a
    // resumed child the wrapper's Drop merely closes handles — the job
    // handle close is what must sweep the tree (the kernel's
    // kill-on-close limit is the mechanism under test).
    drop(child);

    let deadline = Instant::now() + WAIT_ALL;
    for member in &members {
        assert!(
            poll_dead(*member, deadline),
            "member {member} survived the daemon's death"
        );
    }
}

// Proof and identity ---------------------------------------------------------

#[test]
fn wait_all_members_proves_death_with_a_fenced_identity() {
    let environment = BTreeMap::new();
    let arguments = args(&["--exit", "0"]);
    let mut child = spawn_jobbed(&arguments, &environment);

    // The durable identity persisted before resume carries the job tuple.
    let (job, pid, start) = child.job_identity();
    let identity = child.owner_identity().expect("owner identity");
    assert_eq!(identity.pid, pid);
    assert_eq!(identity.start_identity, start);
    assert_eq!(
        identity.platform_identity["native"].as_str(),
        Some("windows-jobbed-suspended")
    );
    assert_eq!(identity.platform_identity["job"].as_u64(), Some(job as u64));
    assert_eq!(
        identity.platform_identity["pid"].as_u64(),
        Some(u64::from(pid))
    );
    assert_eq!(
        identity.platform_identity["startIdentity"].as_u64(),
        Some(start)
    );

    child.resume_once().expect("resume the helper");
    assert!(
        matches!(child.resume_once(), Err(GuardianError::State(_))),
        "the jobbed wrapper keeps the exactly-once resume discipline"
    );

    assert!(child.wait(BUDGET), "the helper exits on its own");
    // Non-blocking observation: buffered bytes drain as Data first; only a
    // fully drained pipe whose child side is gone reads as broken.
    assert_eq!(child.pump_stdout(), PipeRead::Data(MARKER.to_vec()));
    assert_eq!(child.pump_stderr(), PipeRead::Closed);
    assert_eq!(child.pump_stdout(), PipeRead::Closed);
    // Settling releases the primary's handles so it can leave the member
    // list; only then can the full-tree proof see an empty list.
    assert_eq!(child.settle_primary(), Some(0));
    assert!(
        child.wait_all_members(Instant::now() + WAIT_ALL),
        "a dead, settled, single-member tree must be provable"
    );
    assert_eq!(child.settled_exit_code(), Some(0));
    assert!(child.member_pids().expect("member list").is_empty());
}

#[test]
fn unsupported_refusal_is_fail_closed_not_run_loose() {
    // The cheap enclosing-job probe must answer without panicking either
    // way (CI hosts may legitimately run this test binary inside a job).
    let _ = current_process_in_job();

    let source = include_str!("../src/process_guard/windows.rs");
    assert!(
        source.contains("ERROR_ACCESS_DENIED"),
        "the seam must classify creation-time denial explicitly"
    );
    // The denial maps to Unsupported before any generic Api failure —
    // a refused spawn is an error, never a jobless run.
    let created_failure = source.split("if created == 0").nth(1).unwrap_or_default();
    assert!(
        created_failure.contains("if code == ERROR_ACCESS_DENIED")
            && created_failure.contains("GuardianError::Unsupported"),
        "CreateProcessW denial must surface as Unsupported, not a fallback"
    );
    let supervisor = include_str!("../src/services/process_supervisor.rs");
    assert!(
        supervisor
            .contains("GuardianError::Unsupported(reason) => SupervisorError::Unsupported(reason)"),
        "the backend must keep the distinct refusal distinct"
    );
    // An in-test genuine incompatible enclosing job is impractical to build
    // portably (it needs a pre-Windows-8-style nesting refusal or a silo);
    // the source pins above hold the mapping, and every spawn failure in
    // this suite returns Err before the child is ever resumed.
}

// Supervisor end-to-end ------------------------------------------------------

struct Harness {
    supervisor: ProcessSupervisor,
    journal: DeterministicSupervisorJournal,
    artifacts: ArtifactStore,
    policy: OutputTailPolicy,
    cwd: PathBuf,
    _temp: tempfile::TempDir,
}

fn harness_for(arguments: &[&str]) -> (Harness, SupervisorStart) {
    let temp = tempfile::tempdir().expect("temp dir");
    let cwd = temp.path().to_path_buf();
    let backend = Arc::new(WindowsJobBackend::new());
    let journal = DeterministicSupervisorJournal::default();
    let harness = Harness {
        supervisor: ProcessSupervisor::new(backend, Arc::new(journal.clone())),
        journal,
        artifacts: ArtifactStore::for_task(temp.path(), TASK),
        policy: OutputTailPolicy::new(4096, Vec::new(), Vec::new()).expect("tail policy"),
        cwd,
        _temp: temp,
    };
    let request = SupervisorStart {
        operation_id: OP.into(),
        tree_id: "tree-s07".into(),
        task_id: TASK.into(),
        ownership_epoch: 1,
        spec: SpawnSpec {
            executable: PathBuf::from(HELPER),
            arguments: args(arguments),
            cwd: harness.cwd.clone(),
            environment: BTreeMap::new(),
            inherited_objects: Vec::new(),
            output_capacity_bytes: 4096,
        },
    };
    (harness, request)
}

fn limits() -> SupervisorLimits {
    SupervisorLimits {
        run_timeout: Duration::from_secs(10),
        termination_timeout: Duration::from_secs(5),
        read_wait: Duration::from_millis(20),
        max_read_bytes: 4096,
    }
}

/// Let the resumed helper write its marker into the still-undrained pipe
/// before finish() starts, so pre-termination output is observable.
async fn settle_output() {
    tokio::time::sleep(Duration::from_millis(200)).await;
}

fn durable_record(harness: &Harness) -> SupervisorRecord {
    harness
        .journal
        .reopen()
        .load(OP)
        .expect("journal alive")
        .expect("record exists")
}

/// Pins the FIXED supervisor integration: the journal's tree id and
/// ownership epoch are bound into the backend before prepare, so the
/// assembled nine-field proof names the journal's tree, the record guard
/// accepts it, and the run COMPLETES end-to-end (real jobbed spawn,
/// identity persisted pre-resume, cancellation-order termination,
/// full-tree death proof, tail stored through the task's store). The
/// negative guard lives in `backend_discipline_negatives_fail_closed`:
/// a backend spawned WITHOUT binding synthesizes a different tree id, so
/// the mismatch discipline stays covered.
#[tokio::test]
async fn supervisor_completes_windows_backend_runs_with_journal_tree_ids() {
    let (harness, request) = harness_for(&["--exit", "0"]);
    let run = harness.supervisor.start(request).await.expect("start");

    settle_output().await;
    let cancellation = CancellationSignal::default();
    cancellation.cancel();
    let completion = harness
        .supervisor
        .finish(
            run,
            &harness.artifacts,
            &harness.policy,
            limits(),
            &cancellation,
        )
        .await
        .expect("a journal-bound proof completes the run");
    let record = &completion.record;
    assert_eq!(record.phase, SupervisorPhase::Completed);
    assert_eq!(
        record.quarantine_reason, None,
        "a completing run never carries a quarantine reason"
    );
    assert!(
        record.revision >= 10,
        "the journal advanced through every phase, not skipped ahead"
    );
    assert_eq!(durable_record(&harness), *record);

    let proof = record.proof.as_ref().expect("proof attached at completion");
    assert_eq!(proof.kind, TerminationProofKind::Exit);
    assert_eq!(
        proof.tree_id, "tree-s07",
        "the proof names the journal tree, not a backend-synthesized one"
    );
    assert_eq!(proof.ownership_epoch, 1);
    assert_eq!(proof.proof_id, "proof-tree-s07-1");
    assert_eq!(
        proof.observed_boot_identity,
        record
            .owner
            .as_ref()
            .expect("owner persisted")
            .boot_identity,
        "the fencing identity stays coherent within one boot"
    );
    let identity = proof.proof_identity.as_object().expect("identity object");
    assert_eq!(identity.len(), 9, "the exact P01 nine-field envelope");
    assert_eq!(identity["treeId"].as_str(), Some("tree-s07"));
    assert_eq!(identity["ownershipEpoch"].as_u64(), Some(1));
    assert_eq!(
        identity["platformEvidence"]["members"].as_u64(),
        Some(0),
        "members are counted after wait_all_members certified them dead"
    );
    assert_eq!(
        proof.proof_identity_digest,
        canonical_input_hash(&proof.proof_identity),
        "the digest must commit to the exact envelope"
    );

    assert_eq!(
        record.output_tail.as_ref(),
        Some(&completion.output_tail),
        "the record and the completion name the same tail"
    );
    let tail = harness
        .artifacts
        .read_all(&completion.output_tail)
        .expect("the tail is readable through the owning task store");
    assert!(
        tail.windows(MARKER.len()).any(|window| window == MARKER),
        "the pre-termination marker must survive into the stored tail"
    );
}

/// The cancellation path, fixed: a long-running tree is terminated through
/// the job, full-tree death is proven (the member list is certified to
/// zero), and the nine-field proof carries `job == "terminated"` under the
/// journal's tree id — the run completes, never a loose write.
#[tokio::test]
async fn cancelled_long_runs_terminate_and_prove_the_full_tree() {
    let (harness, request) = harness_for(&["--sleep", "10000"]);
    let run = harness.supervisor.start(request).await.expect("start");

    settle_output().await;
    let cancellation = CancellationSignal::default();
    cancellation.cancel();
    let completion = harness
        .supervisor
        .finish(
            run,
            &harness.artifacts,
            &harness.policy,
            limits(),
            &cancellation,
        )
        .await
        .expect("cancellation completes with a certified death proof");
    assert_eq!(completion.record.phase, SupervisorPhase::Completed);
    assert_eq!(completion.record.quarantine_reason, None);
    let proof = completion
        .record
        .proof
        .as_ref()
        .expect("proof attached at completion");
    assert_eq!(proof.tree_id, "tree-s07");
    let identity = proof.proof_identity.as_object().expect("identity object");
    assert_eq!(
        identity["platformEvidence"]["job"].as_str(),
        Some("terminated"),
        "an explicitly terminated tree labels its evidence"
    );
    assert_eq!(
        identity["platformEvidence"]["members"].as_u64(),
        Some(0),
        "the whole tree is certified dead, not just the primary"
    );
    assert_eq!(durable_record(&harness).phase, SupervisorPhase::Completed);
}

/// The natural-exit path, fixed: a tree that exits on its own (no cancel,
/// no timeout) proves through the same certified-empty member list and
/// carries `job == "exited"` — completion, not quarantine.
#[tokio::test]
async fn natural_completion_proves_without_termination() {
    let (harness, request) = harness_for(&["--exit", "0"]);
    let run = harness.supervisor.start(request).await.expect("start");

    let completion = harness
        .supervisor
        .finish(
            run,
            &harness.artifacts,
            &harness.policy,
            limits(),
            &CancellationSignal::default(),
        )
        .await
        .expect("a natural exit proves without termination");
    assert_eq!(completion.record.phase, SupervisorPhase::Completed);
    assert_eq!(completion.record.quarantine_reason, None);
    let proof = completion
        .record
        .proof
        .as_ref()
        .expect("proof attached at completion");
    assert_eq!(proof.tree_id, "tree-s07");
    let identity = proof.proof_identity.as_object().expect("identity object");
    assert_eq!(
        identity["platformEvidence"]["job"].as_str(),
        Some("exited"),
        "a naturally-exited tree distinguishes its evidence from termination"
    );
    assert_eq!(
        identity["platformEvidence"]["members"].as_u64(),
        Some(0),
        "even a natural exit must prove the whole tree dead"
    );
    assert_eq!(durable_record(&harness).phase, SupervisorPhase::Completed);
}

// Backend discipline negatives -----------------------------------------------

#[tokio::test]
async fn backend_discipline_negatives_fail_closed() {
    let backend = Arc::new(WindowsJobBackend::new());

    // prepare validates the spec, but the raw seam cannot honor ambient
    // inherited objects — spawn_suspended refuses them explicitly.
    let inherited_spec = SpawnSpec {
        executable: PathBuf::from(HELPER),
        arguments: args(&[]),
        cwd: std::env::temp_dir(),
        environment: BTreeMap::new(),
        inherited_objects: vec![InheritedObject::WindowsHandle(3)],
        output_capacity_bytes: 4096,
    };
    let launch = backend
        .prepare(inherited_spec)
        .await
        .expect("prepare validates what spawn will refuse");
    assert!(
        matches!(
            backend.spawn_suspended(launch).await,
            Err(SupervisorError::InvalidSpec(_))
        ),
        "the windows backend must refuse inherited objects it cannot honor"
    );

    // Live gating on a real suspended helper: resume without a persisted
    // identity is refused (the child stays suspended, never leaked). The
    // helper sleeps so the resumed tree is genuinely alive below.
    let live_spec = SpawnSpec {
        executable: PathBuf::from(HELPER),
        arguments: args(&["--sleep", "10000"]),
        cwd: std::env::temp_dir(),
        environment: BTreeMap::new(),
        inherited_objects: Vec::new(),
        output_capacity_bytes: 4096,
    };
    let launch = backend.prepare(live_spec).await.expect("prepare");
    let child = backend.spawn_suspended(launch).await.expect("real spawn");
    assert_eq!(child.kind(), PreparedChildKind::WindowsSuspended);
    assert!(matches!(
        backend.resume_once(&child).await,
        Err(SupervisorError::InvalidState(_))
    ));
    let identity = backend
        .probe_identity(&child)
        .await
        .expect("observed identity");
    backend
        .persist_identity(&child, identity)
        .await
        .expect("persist the observed identity");
    let tree = backend.resume_once(&child).await.expect("resume");
    let subscription = backend.subscribe_output(&tree).await.expect("subscribe");

    // A resumed, still-alive tree cannot be certified within a short
    // timeout: the empty-member-list gate refuses to guess (Unverifiable),
    // which blocks supervisor writes instead of completing loosely.
    assert!(matches!(
        backend
            .wait_and_prove(&tree, Duration::from_millis(50))
            .await,
        Err(SupervisorError::Unverifiable)
    ));
    // A never-resumed tree is refused BEFORE any wait-and-certify. A
    // RunningTree handle only exists after a successful resume (which sets
    // the flag first), so this ordering is pinned in the source rather
    // than exercised live — same discipline as the unsupported-refusal pin.
    let source = include_str!("../src/services/process_supervisor.rs");
    let windows = source
        .split("impl ProcessTreeBackend for WindowsJobBackend")
        .nth(1)
        .unwrap_or_default();
    let prove = windows
        .split("async fn wait_and_prove")
        .nth(1)
        .unwrap_or_default();
    let prove = prove
        .split("async fn subscribe_output")
        .next()
        .unwrap_or_default();
    let resume_gate = prove
        .find("\"tree was not resumed\"")
        .expect("the never-resumed refusal exists");
    let certify = prove
        .find("wait_all_members")
        .expect("the certify wait exists");
    assert!(
        resume_gate < certify,
        "the never-resumed refusal must fire before any certify wait"
    );

    // Drain bounds are enforced per the PROCESS_READ protocol limits.
    for (max_bytes, wait) in [
        (0u32, Duration::ZERO),
        (PROCESS_READ_MAX_BYTES + 1, Duration::ZERO),
        (
            1,
            Duration::from_millis(u64::from(PROCESS_READ_MAX_WAIT_MS) + 1),
        ),
    ] {
        assert!(
            matches!(
                backend.drain(&subscription, 0, max_bytes, wait).await,
                Err(SupervisorError::InvalidSpec(_))
            ),
            "drain bounds ({max_bytes} bytes, {wait:?}) must be rejected"
        );
    }
    assert_eq!(
        backend.drain(&subscription, 5, 64, Duration::ZERO).await,
        Err(SupervisorError::StaleCursor)
    );
    // Once the marker is buffered, a page too small for one frame is a
    // hard error rather than a silently truncated frame.
    assert_eq!(
        backend
            .drain(&subscription, 0, 1, Duration::from_millis(500))
            .await,
        Err(SupervisorError::PageTooSmall)
    );

    // Live handles never cross backend instances.
    let other = WindowsJobBackend::new();
    assert!(matches!(
        other.subscribe_output(&tree).await,
        Err(SupervisorError::UnknownHandle)
    ));

    // Cleanup doubles as the positive proof path: terminate, then prove.
    backend.terminate(&tree).await.expect("terminate");
    let proof = backend
        .wait_and_prove(&tree, Duration::from_secs(5))
        .await
        .expect("proof after explicit termination");
    assert_eq!(proof.kind, TerminationProofKind::Exit);
    // The exact P01 nine-field envelope, gated on a certified-empty
    // member list.
    let identity = proof.proof_identity.as_object().expect("identity object");
    assert_eq!(identity.len(), 9, "the exact P01 nine-field envelope");
    assert_eq!(
        identity["platformEvidence"]["job"].as_str(),
        Some("terminated")
    );
    assert_eq!(
        identity["platformEvidence"]["members"].as_u64(),
        Some(0),
        "members are counted after wait_all_members certified them dead"
    );
    // NEGATIVE guard for the supervisor completion test: this backend was
    // spawned WITHOUT a bound identity (direct trait use), so its proof
    // names a synthesized tree a supervisor journal would refuse.
    assert_ne!(
        proof.tree_id, "tree-s07",
        "an unbound direct spawn must not name a journal tree"
    );
    assert!(
        proof.tree_id.starts_with("windows-job-"),
        "the synthesized identity is namespaced to the backend: {}",
        proof.tree_id
    );
    assert_eq!(identity["treeId"].as_str(), Some(proof.tree_id.as_str()));
    assert_eq!(
        identity["ownershipEpoch"].as_u64(),
        Some(1),
        "a synthesized identity defaults to epoch 1"
    );
}

#[test]
fn no_premature_production_wiring() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for path in [
        root.join("services/processes.rs"),
        root.join("plugins/router.rs"),
        root.join("run_manager.rs"),
        root.join("bin/r-code-service.rs"),
    ] {
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        assert!(
            !source.contains("WindowsJobBackend") && !source.contains("windows_backend"),
            "the P07 backend must not be wired into production yet: {}",
            path.display()
        );
    }
}
