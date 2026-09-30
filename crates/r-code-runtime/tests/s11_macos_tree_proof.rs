#![cfg(target_os = "macos")]
//! P11 — macOS descendant diagnostics never claim containment; write
//! execution stays durably SafeDisabled.
//!
//! Proves on real macOS processes (native macOS CI; compiles to an empty
//! suite elsewhere) the P11 acceptance criteria with adversarial fixtures
//! that go through the REAL spawn path (`macos_spawn_via_guardian` /
//! `serve_macos_guardian`, no mocks):
//!
//! * a double-forked descendant that re-parents to launchd (ppid 1) is
//!   classified `Escaped` by `macos_classify_descendant_containment` — the
//!   honest verdict, never `SnapshotReachable`-presented-as-contained;
//! * live self-reported descendants are enumerated and identity-checked
//!   (`SnapshotReachable` is diagnostics-only — no verdict value mints a
//!   `TerminationProofRecord`, and the write refusal is the durable outcome);
//! * `macos_terminate_descendants_best_effort` kills exactly the live strict
//!   descendants (each pid re-probed immediately before the signal), spares
//!   the root, and a stale snapshot signals nothing after death — a recycled
//!   foreign pid can never be signalled;
//! * `ProcessSupervisor::start_with_write_profile` with
//!   `ExecutionWriteProfile::WriteCapable` refuses on macOS with
//!   `Err(SafeDisabled)` BEFORE any spawn, persisting a terminal
//!   SafeDisabled record (reason `macos-no-kernel-tree-containment`) that
//!   `recover()` hands back, `quarantine()` refuses, and no journal CAS can
//!   move anywhere — never `Activated`;
//! * the write classifier is static platform knowledge (`cfg!`), never fed
//!   by enumeration results (P12/P13 own activation).
//!
//! The pure classifier state-machine pins on synthetic snapshots live in the
//! `macos.rs` unit tests (same CI); this suite owns the REAL-fixture proofs.
//! Every spawned workload dies before its test ends (group kills plus bounded
//! waits; every wait is bounded by `BUDGET`).

use r_code_runtime::process_guard::macos::{
    macos_classify_descendant_containment, macos_process_birth_identity, macos_snapshot_processes,
    macos_spawn_via_guardian, macos_terminate_descendants_best_effort, MacosContainmentVerdict,
    MacosGatedWorkload, MacosReleasedWorkload,
};
use r_code_runtime::process_guard::unix::{GuardianIdentity, GuardianSpawnRequest};
use r_code_runtime::process_guard::{BootIdentity, ProcessOwnerIdentity};
use r_code_runtime::services::process_supervisor::{
    DeterministicFakeBackend, DeterministicSupervisorJournal, ExecutionWriteProfile, FaultPoint,
    PreparedChildKind, ProcessSupervisor, SpawnSpec, SupervisorError, SupervisorJournal,
    SupervisorPhase, SupervisorStart,
};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Hard cap for every bounded wait in this suite (P10 value).
const BUDGET: Duration = Duration::from_secs(10);

// Real-fixture helpers ---------------------------------------------------------

/// The explicit workload environment: the workload inherits nothing beyond
/// this, and /bin/sh needs PATH to resolve sleep.
fn workload_environment() -> BTreeMap<String, String> {
    BTreeMap::from([(
        "PATH".to_string(),
        "/usr/bin:/bin:/usr/sbin:/sbin".to_string(),
    )])
}

fn sh_request(script: &str) -> GuardianSpawnRequest {
    GuardianSpawnRequest {
        executable: "/bin/sh".to_string(),
        arguments: vec!["-c".to_string(), script.to_string()],
        cwd: None,
        environment: workload_environment(),
    }
}

/// Spawn the suspended gate through the real daemon seam (no mocks).
fn gate_spawn(script: &str) -> MacosGatedWorkload {
    macos_spawn_via_guardian(sh_request(script))
        .expect("macos_spawn_via_guardian must hand back the suspended gate")
}

fn poll_condition(budget: Duration, mut check: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if check() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    check()
}

/// Liveness per the birth-identity probe: a pid whose PROC_PIDTBSDINFO tuple
/// is unreadable is dead (or at best a zombie pending reaping) — for cleanup
/// purposes it is gone once the identity is gone.
fn pid_alive(pid: u32) -> bool {
    macos_process_birth_identity(pid).is_some()
}

fn pid_gone(pid: u32) -> bool {
    !pid_alive(pid)
}

/// Kill the workload's whole process group. The pgid always comes from the
/// gate's getpgid-verified identity, mirroring macos.rs's own kill_group
/// rule: never an unverified pid.
fn kill_workload_group(group_pid: u32) {
    // SAFETY: kill(2) with a negative pid signals a process group created and
    // verified by the guardian gate; delivering the signal IS the intended
    // effect and no memory is involved.
    unsafe {
        libc::kill(-(group_pid as i32), libc::SIGKILL);
    }
}

/// Bounded read of the pid file a workload self-reports into (the API
/// contract for `known_member_pids`: strict descendants the caller knows
/// about from the workload itself).
fn read_reported_pids(report: &Path) -> Vec<u32> {
    let deadline = Instant::now() + BUDGET;
    loop {
        if let Ok(text) = std::fs::read_to_string(report) {
            let pids: Vec<u32> = text
                .split_whitespace()
                .filter_map(|value| value.parse().ok())
                .collect();
            if !pids.is_empty() {
                return pids;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the workload must self-report its descendant pids in time"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Fixture: a released root workload with two live strict descendants that
/// self-report their pids. The trailing `sleep 30` keeps the root alive after
/// the children die, so a later sweep still has a living root to spare.
fn spawn_two_child_workload(report: &Path) -> (MacosReleasedWorkload, GuardianIdentity, Vec<u32>) {
    let script = format!(
        "sleep 30 & p1=$!; sleep 30 & p2=$!; echo \"$p1 $p2\" > {}; wait; sleep 30",
        report.display()
    );
    let gate = gate_spawn(&script);
    let identity = gate.identity();
    let mut released = gate.release_once().expect("release the gate");
    let members = read_reported_pids(report);
    assert_eq!(members.len(), 2, "the fixture self-reports two descendants");
    (released, identity, members)
}

// P11.2 — the adversarial escape fixture ----------------------------------------

#[test]
fn the_double_fork_escapee_is_classified_escaped_never_contained() {
    let temp = tempfile::tempdir().expect("temp dir");
    let report = temp.path().join("escapee-pid");
    // Double-fork: the inner shell writes its own pid ($$ is the inner shell,
    // preserved across `exec sleep`), then its subshell parent exits, so the
    // escapee re-parents to launchd (ppid 1) while the root stays alive in
    // its foreground sleep — invisible to any ppid walk from the root.
    let script = format!(
        "( /bin/sh -c 'echo $$ > {}; exec sleep 60' & ) ; sleep 30",
        report.display()
    );
    let gate = gate_spawn(&script);
    let identity = gate.identity();
    let root_pid = identity.outer_pid;
    let mut released = gate.release_once().expect("release the gate");
    let escaped_pid: u32 = read_reported_pids(&report)
        .first()
        .copied()
        .expect("the escapee self-reports its pid");

    // The escape IS the re-parenting: poll until launchd owns the escapee.
    assert!(
        poll_condition(BUDGET, || {
            macos_process_birth_identity(escaped_pid).is_some_and(|birth| birth.ppid == 1)
        }),
        "the double-forked escapee must re-parent to launchd (ppid 1)"
    );
    let root_birth = macos_process_birth_identity(root_pid)
        .expect("the root workload must stay alive for the snapshot");
    let snapshot = macos_snapshot_processes().expect("process enumeration");
    let verdict =
        macos_classify_descendant_containment(Some(&snapshot), &root_birth, &[escaped_pid]);
    assert_eq!(
        verdict,
        MacosContainmentVerdict::Escaped {
            escaped_pids: vec![escaped_pid]
        },
        "an escaped known member must classify Escaped — never contained"
    );
    // The escapee IS listed in the snapshot (alive, owned by launchd):
    // Escaped means "not reachable through the root's ppid edges", which is
    // exactly the setsid/double-fork outcome the classifier must report.
    assert_eq!(
        snapshot.get(escaped_pid).expect("live escapee listed").ppid,
        1,
        "the escapee is enumerated under launchd, invisible to the root walk"
    );

    // Cleanup: the escapee stayed in the workload's process group, so the
    // verified group kill covers root and escapee together.
    kill_workload_group(identity.group_pid);
    released.wait_guardian();
    assert!(
        poll_condition(BUDGET, || pid_gone(root_pid) && pid_gone(escaped_pid)),
        "cleanup: root and escapee must both die before the test ends"
    );
}

// P11.1 — live descendants are enumerated and identity-checked ------------------

#[test]
fn live_descendants_are_enumerated_identity_checked_and_classified_reachable() {
    let temp = tempfile::tempdir().expect("temp dir");
    let report = temp.path().join("child-pids");
    let (mut released, identity, members) = spawn_two_child_workload(&report);
    let root_pid = identity.outer_pid;
    let root_birth = macos_process_birth_identity(root_pid).expect("the root workload is alive");
    let snapshot = macos_snapshot_processes().expect("process enumeration");

    for &pid in &members {
        let entry = snapshot.get(pid).expect("a live descendant is enumerated");
        assert_eq!(entry.pid, pid, "entries are keyed by their own pid");
        assert_eq!(entry.ppid, root_pid, "the descendant hangs off the root");
        assert!(entry.is_complete(), "enumerated identities are complete");
    }
    // SnapshotReachable is the DIAGNOSTICS verdict: a member count, never a
    // containment claim and never a TerminationProofRecord (pinned below).
    assert_eq!(
        macos_classify_descendant_containment(Some(&snapshot), &root_birth, &members),
        MacosContainmentVerdict::SnapshotReachable { members: 2 },
        "reachable known members classify SnapshotReachable (diagnostics only)"
    );

    kill_workload_group(identity.group_pid);
    released.wait_guardian();
    assert!(
        poll_condition(BUDGET, || pid_gone(root_pid)
            && members.iter().all(|&pid| pid_gone(pid))),
        "cleanup: the whole fixture must die before the test ends"
    );
}

// P11.1 — the best-effort sweep --------------------------------------------------

#[test]
fn the_sweep_kills_exactly_the_live_descendants_and_spares_the_root() {
    let temp = tempfile::tempdir().expect("temp dir");
    let report = temp.path().join("child-pids");
    let (mut released, identity, members) = spawn_two_child_workload(&report);
    let root_pid = identity.outer_pid;
    let root_birth = macos_process_birth_identity(root_pid).expect("the root workload is alive");
    let snapshot = macos_snapshot_processes().expect("process enumeration");

    let signalled = macos_terminate_descendants_best_effort(&snapshot, &root_birth);
    let mut killed = signalled.clone();
    killed.sort_unstable();
    let mut expected = members.clone();
    expected.sort_unstable();
    assert_eq!(
        killed, expected,
        "the sweep signals exactly the live strict descendants"
    );
    assert!(
        poll_condition(BUDGET, || members.iter().all(|&pid| pid_gone(pid))),
        "the swept descendants must die"
    );
    assert!(
        pid_alive(root_pid),
        "the root is the guardian's group contract — the sweep never signals it"
    );

    kill_workload_group(identity.group_pid);
    released.wait_guardian();
    assert!(
        poll_condition(BUDGET, || pid_gone(root_pid)),
        "cleanup: the root must die before the test ends"
    );
}

#[test]
fn a_stale_snapshot_sweep_signals_nothing_after_death() {
    let temp = tempfile::tempdir().expect("temp dir");
    let report = temp.path().join("child-pids");
    let (mut released, identity, members) = spawn_two_child_workload(&report);
    let root_pid = identity.outer_pid;
    let root_birth = macos_process_birth_identity(root_pid).expect("the root workload is alive");
    let snapshot = macos_snapshot_processes().expect("process enumeration");
    assert!(matches!(
        macos_classify_descendant_containment(Some(&snapshot), &root_birth, &members),
        MacosContainmentVerdict::SnapshotReachable { .. }
    ));

    kill_workload_group(identity.group_pid);
    released.wait_guardian();
    let mut tree = members.clone();
    tree.push(root_pid);
    assert!(
        poll_condition(BUDGET, || tree.iter().all(|&pid| pid_gone(pid))),
        "the whole tree must be dead and reaped before the stale sweep"
    );
    // The stale snapshot still lists every member. The live re-probe fence
    // must skip each one (identity vanished = dead or recycled): no signal
    // can land on whatever process, if any, now owns those pids.
    let signalled = macos_terminate_descendants_best_effort(&snapshot, &root_birth);
    assert!(
        signalled.is_empty(),
        "a stale snapshot must never produce a signal — the PID-reuse fence"
    );
}

// Unverifiable is the honest failure mode ----------------------------------------

#[test]
fn a_recycled_root_identity_classifies_unverifiable() {
    let snapshot = macos_snapshot_processes().expect("process enumeration");
    // A different start tuple under the same pid is NOT our root: a recycled
    // pid must never let the walk start (the pid-reuse fence, root side).
    let mut recycled =
        macos_process_birth_identity(std::process::id()).expect("the test process identity");
    recycled.start_seconds += 60;
    assert_eq!(
        macos_classify_descendant_containment(Some(&snapshot), &recycled, &[std::process::id()]),
        MacosContainmentVerdict::Unverifiable {
            reason: "root birth identity mismatch"
        },
        "a root identity mismatch is Unverifiable, never a guess"
    );
}

#[test]
fn a_failed_enumeration_classifies_unverifiable_never_contained() {
    let root = macos_process_birth_identity(std::process::id()).expect("test process identity");
    assert_eq!(
        macos_classify_descendant_containment(None, &root, &[std::process::id()]),
        MacosContainmentVerdict::Unverifiable {
            reason: "process enumeration failed"
        },
        "a failed enumeration can never support SnapshotReachable"
    );
    // Enumeration hygiene: the kernel (pid 0) and launchd (pid 1) are never
    // workload entries.
    let snapshot = macos_snapshot_processes().expect("process enumeration");
    assert!(snapshot.get(0).is_none(), "pid 0 is never listed");
    assert!(
        snapshot.get(1).is_none(),
        "launchd is never a workload entry"
    );
    assert!(snapshot
        .get(std::process::id())
        .expect("the test process is listed")
        .is_complete());
}

// P11.3 — the durable write refusal (supervisor surface) ------------------------

struct WriteHarness {
    supervisor: ProcessSupervisor,
    backend: Arc<DeterministicFakeBackend>,
    journal: Arc<DeterministicSupervisorJournal>,
}

fn write_harness() -> WriteHarness {
    let backend = Arc::new(DeterministicFakeBackend::new(PreparedChildKind::UnixGated));
    let journal = Arc::new(DeterministicSupervisorJournal::default());
    let supervisor = ProcessSupervisor::new(backend.clone(), journal.clone());
    WriteHarness {
        supervisor,
        backend,
        journal,
    }
}

fn write_request(operation: &str) -> SupervisorStart {
    SupervisorStart {
        operation_id: operation.into(),
        tree_id: format!("tree-{operation}"),
        task_id: format!("task-{operation}"),
        ownership_epoch: 3,
        spec: SpawnSpec {
            executable: PathBuf::from("/bin/sleep"),
            arguments: vec!["1".into()],
            cwd: PathBuf::from("/tmp"),
            environment: BTreeMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string())]),
            inherited_objects: Vec::new(),
            output_capacity_bytes: 4096,
        },
    }
}

fn fake_owner() -> ProcessOwnerIdentity {
    ProcessOwnerIdentity::new(
        31_000,
        41_000,
        BootIdentity::parse("macos:1700000000:000000").expect("canonical boot identity"),
        serde_json::json!({"native": "s11-fake"}),
    )
    .expect("complete owner identity")
}

#[tokio::test]
async fn write_capable_execution_is_durably_safe_disabled_before_any_spawn() {
    let harness = write_harness();
    // Tripwire: if any spawn were ever attempted for this operation, the
    // injected fault would turn the outcome into Quarantined — the only way
    // to observe SafeDisabled is to refuse before the spawn path entirely.
    harness.backend.fail_once(FaultPoint::SpawnSuspended);
    let refusal = match harness
        .supervisor
        .start_with_write_profile(
            write_request("op-s11-write"),
            ExecutionWriteProfile::WriteCapable,
        )
        .await
    {
        Ok(_) => panic!("write-capable execution must be refused on macOS"),
        Err(error) => error,
    };
    assert!(
        matches!(
            &refusal,
            SupervisorError::SafeDisabled(reason) if reason == "macos-no-kernel-tree-containment"
        ),
        "the refusal must name the macOS containment gap, got {refusal:?}"
    );

    // The durable outcome: recover() hands back a TERMINAL SafeDisabled
    // record — no owner, no proof, no tail, no quarantine path anywhere.
    let record = harness
        .supervisor
        .recover("op-s11-write")
        .expect("the SafeDisabled record is durable");
    assert_eq!(record.phase, SupervisorPhase::SafeDisabled);
    assert_eq!(
        record.revision, 2,
        "Prepared (rev 1) -> SafeDisabled (rev 2)"
    );
    assert!(record.owner.is_none());
    assert!(
        record.proof.is_none(),
        "SafeDisabled never carries a TerminationProofRecord"
    );
    assert!(record.output_tail.is_none());
    assert!(record.quarantine_reason.is_none());
    let disabled = record
        .write_disabled
        .as_ref()
        .expect("the refusal is persisted on the record");
    assert_eq!(disabled.platform, "macos");
    assert_eq!(disabled.reason, "macos-no-kernel-tree-containment");
    assert_eq!(disabled.profile, ExecutionWriteProfile::WriteCapable);
}

#[tokio::test]
async fn safe_disabled_records_are_terminal_and_unquarantinable() {
    let harness = write_harness();
    match harness
        .supervisor
        .start_with_write_profile(
            write_request("op-s11-terminal"),
            ExecutionWriteProfile::WriteCapable,
        )
        .await
    {
        Ok(_) => panic!("write-capable execution must be refused on macOS"),
        Err(SupervisorError::SafeDisabled(_)) => {}
        Err(other) => panic!("expected SafeDisabled, got {other:?}"),
    }
    let record = harness
        .supervisor
        .recover("op-s11-terminal")
        .expect("terminal record");

    // No journal CAS moves the record anywhere: not to a spawn phase, not to
    // quarantine (the supervisor's private quarantine() is exactly this CAS,
    // so its refusal is proven at the seam it uses), not to completion, not
    // even a revision-only self-loop.
    let journal = harness.journal.reopen();
    for target in [
        SupervisorPhase::Suspended,
        SupervisorPhase::Running,
        SupervisorPhase::Completed,
        SupervisorPhase::Quarantined,
        SupervisorPhase::SafeDisabled,
    ] {
        let mut next = record.clone();
        next.phase = target;
        next.revision += 1;
        assert!(
            journal.compare_and_swap(&record, next).is_err(),
            "SafeDisabled must never transition to {target:?}"
        );
    }
    // Tampering with the persisted classification directly (a different
    // reason under the same phase) is rejected by the same rules.
    let mut tampered = record.clone();
    tampered.revision += 1;
    if let Some(disabled) = tampered.write_disabled.as_mut() {
        disabled.reason = "pretend-containment".into();
    }
    assert!(
        journal.compare_and_swap(&record, tampered).is_err(),
        "the SafeDisabled classification is immutable"
    );
    // A retry of the same operation identity is refused without side effects
    // — never retried as a spawn.
    assert!(matches!(
        harness
            .supervisor
            .start_with_write_profile(
                write_request("op-s11-terminal"),
                ExecutionWriteProfile::WriteCapable
            )
            .await,
        Err(SupervisorError::InvalidState(_))
    ));
    // recover() stays idempotent and terminal.
    assert_eq!(
        harness
            .supervisor
            .recover("op-s11-terminal")
            .expect("still recoverable")
            .phase,
        SupervisorPhase::SafeDisabled
    );
}

#[tokio::test]
async fn diagnostics_supervision_is_not_safe_disabled_and_runs_the_start_path() {
    // The classifier distinguishes profiles: only write-capable execution
    // needs kernel containment; diagnostics supervision proceeds.
    assert_eq!(
        ExecutionWriteProfile::NoWorkspaceDiagnostics.write_safe_disabled(),
        None
    );
    let harness = write_harness();
    harness.backend.set_next_identity(fake_owner());
    let run = harness
        .supervisor
        .start_with_write_profile(
            write_request("op-s11-diag"),
            ExecutionWriteProfile::NoWorkspaceDiagnostics,
        )
        .await
        .expect("diagnostics supervision delegates into the real start path");
    assert_eq!(
        run.record().phase,
        SupervisorPhase::Running,
        "NoWorkspaceDiagnostics actually runs on macOS — it is not a write profile"
    );
    assert!(run.record().write_disabled.is_none());
}

// Discipline pins: static classifier, no activation, sweep guards ----------------

#[test]
fn the_write_classifier_is_static_platform_knowledge() {
    // cfg!-based static facts, valid on this (macOS) host.
    assert_eq!(
        ExecutionWriteProfile::WriteCapable.write_safe_disabled(),
        Some(("macos", "macos-no-kernel-tree-containment"))
    );
    let supervisor_source = include_str!("../src/services/process_supervisor.rs");
    assert!(
        supervisor_source.contains("cfg!(target_os = \"macos\")"),
        "the write decision must be static platform knowledge (cfg!)"
    );
    assert!(
        !supervisor_source.contains("macos_snapshot_processes")
            && !supervisor_source.contains("MacosContainmentVerdict"),
        "the write classifier must never be fed by enumeration results"
    );
    // And the diagnostics module stays out of the supervisor/activation
    // domain entirely: it neither decides profiles nor mints proofs.
    let macos_source = include_str!("../src/process_guard/macos.rs");
    for forbidden in [
        "ExecutionWriteProfile",
        "start_with_write_profile",
        "write_safe_disabled",
        "SupervisorPhase",
        "Activated",
        "TerminationProofRecord",
    ] {
        assert!(
            !macos_source.contains(forbidden),
            "the diagnostics module must not reference {forbidden}"
        );
    }
}

#[test]
fn the_containment_verdict_can_never_become_activated_or_a_proof() {
    // Exhaustive match WITHOUT a wildcard arm: adding an Activated (or any
    // fourth) variant breaks this compilation, so the parse gate catches it.
    fn is_diagnostic_verdict(verdict: &MacosContainmentVerdict) -> bool {
        match verdict {
            MacosContainmentVerdict::SnapshotReachable { .. }
            | MacosContainmentVerdict::Escaped { .. }
            | MacosContainmentVerdict::Unverifiable { .. } => true,
        }
    }
    let snapshot = macos_snapshot_processes().expect("process enumeration");
    let root = macos_process_birth_identity(std::process::id()).expect("test identity");
    let reachable =
        macos_classify_descendant_containment(Some(&snapshot), &root, &[std::process::id()]);
    let unverifiable = macos_classify_descendant_containment(None, &root, &[]);
    let mismatch_root = {
        let mut other = root;
        other.start_seconds += 60;
        macos_classify_descendant_containment(Some(&snapshot), &other, &[])
    };
    // The test process is not a strict descendant of itself: its own pid as
    // a known member must classify Escaped (absent from its own descendant
    // walk) — never contained.
    for verdict in [&reachable, &unverifiable, &mismatch_root] {
        assert!(
            is_diagnostic_verdict(verdict),
            "every verdict stays a diagnostics value: {verdict:?}"
        );
    }
    assert!(matches!(reachable, MacosContainmentVerdict::Escaped { .. }));
}

#[test]
fn the_sweep_guards_the_own_pid_and_reprobes_before_every_signal() {
    let source = include_str!("../src/process_guard/macos.rs");
    assert!(
        source.contains("if pid == std::process::id()"),
        "the sweep must absolutely guard the diagnosing process"
    );
    assert!(
        source.contains("macos_process_birth_identity(pid).as_ref() != Some(entry)"),
        "every candidate must be live-re-probed immediately before signalling"
    );
    assert!(
        source.contains("libc::proc_listallpids") && source.contains("libc::PROC_PIDTBSDINFO"),
        "enumeration is proc_listallpids plus per-pid PROC_PIDTBSDINFO identities"
    );
    assert!(
        !source.contains("/proc/"),
        "the macOS path must never read /proc"
    );
}
