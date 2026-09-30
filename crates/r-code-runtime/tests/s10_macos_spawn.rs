//! P10 — macOS gated spawn and birth identity.
//!
//! Proves on real macOS processes (native macOS CI; compiles to an empty
//! suite elsewhere): the guardian creates the workload with ONE
//! `posix_spawn` under `POSIX_SPAWN_START_SUSPENDED`, so no instruction of
//! the workload binary runs before `release_once` (a touch file is the
//! execution detector), the `PROC_PIDTBSDINFO` birth tuple (pid/ppid/start
//! seconds/useconds) is delivered, verifiable and parented BEFORE release,
//! an unreleased gate dies on Drop without ever running, release is exactly
//! once, a foreign protocol version is refused with the P08 exit code, the
//! released workload inherits exactly stdio (`POSIX_SPAWN_CLOEXEC_DEFAULT`)
//! and nothing ever reads `/proc`.
//!
//! The `serve_macos_guardian` state machine is stream-generic, so the
//! protocol-level tests drive it over test-owned `UnixStream` pairs and can
//! observe the P08 exit codes directly; the daemon-seam tests go through
//! `macos_spawn_via_guardian` (in-process guardian thread — its exit code is
//! consumed by the thread design, which the manual sessions compensate for).
//!
//! Windows parses this to an empty suite (`#![cfg(target_os = "macos")]`);
//! it runs on native macOS CI. Every wait is bounded (≤ 10 s).

#![cfg(target_os = "macos")]

use r_code_runtime::process_guard::macos::{
    macos_owner_identity, macos_process_birth_identity, macos_spawn_via_guardian,
    serve_macos_guardian, MacosBirthIdentity, MacosGatedWorkload,
};
use r_code_runtime::process_guard::unix::guardian_protocol::{
    self, FrameErrorCode, GuardianFrame, PROTOCOL_VERSION,
};
use r_code_runtime::process_guard::unix::{
    group_alive, GuardianSpawnRequest, GUARDIAN_EXIT_DAEMON_EOF, GUARDIAN_EXIT_PROTOCOL,
    GUARDIAN_EXIT_RELEASED,
};
use std::collections::BTreeMap;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

/// Hard cap for every bounded wait in this suite.
const BUDGET: Duration = Duration::from_secs(10);

/// The explicit workload environment: the workload never inherits anything,
/// and /bin/sh needs PATH to exec `touch` and `sleep`.
fn workload_environment() -> BTreeMap<String, String> {
    BTreeMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string())])
}

/// A spawn request for `/bin/sh -c <script>` with the explicit environment.
/// `/bin/sh` exists on every macOS image; the field names are the real
/// `GuardianSpawnRequest` contract from unix.rs (reused verbatim on macOS).
fn sh_request(script: &str) -> GuardianSpawnRequest {
    GuardianSpawnRequest {
        executable: "/bin/sh".to_string(),
        arguments: vec!["-c".to_string(), script.to_string()],
        cwd: None,
        environment: workload_environment(),
    }
}

/// Spawn the suspended gate through the real daemon seam.
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

/// True once the named process group is gone.
fn poll_group_gone(group_pid: u32) -> bool {
    poll_condition(BUDGET, || !group_alive(group_pid as i32))
}

// Manual guardian-driver fixtures (stream-generic serve_macos_guardian) --------

struct ManualSession {
    /// The guardian session thread; `serve_macos_guardian`'s exit code.
    guardian: std::thread::JoinHandle<i32>,
    /// Daemon → guardian commands (the guardian's read end).
    commands: UnixStream,
    /// Guardian → daemon replies (the guardian's write end).
    replies: UnixStream,
}

/// Start one guardian session over test-owned `UnixStream` pairs, so
/// protocol-level refusals, the release handshake and the P08 exit codes are
/// directly observable (the daemon seam consumes its guardian's exit code).
fn open_manual_session() -> ManualSession {
    let (commands_write, commands_read) = UnixStream::pair().expect("command stream pair");
    let (replies_write, replies_read) = UnixStream::pair().expect("reply stream pair");
    let guardian = std::thread::Builder::new()
        .name("s10-manual-macos-guardian".into())
        .spawn(move || serve_macos_guardian(&mut commands_read, &mut replies_write))
        .expect("spawn the guardian session thread");
    ManualSession {
        guardian,
        commands: commands_write,
        replies: replies_read,
    }
}

/// Bounded frame read: a guardian that cannot answer in time fails the test,
/// never hangs it.
fn read_frame_bounded(stream: &UnixStream, stage: &'static str) -> GuardianFrame {
    let reader = stream
        .try_clone()
        .expect("clone the reply stream for a bounded read");
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = reader;
        let _ = sender.send(guardian_protocol::read_frame(&mut reader));
    });
    match receiver.recv_timeout(BUDGET) {
        Ok(Ok(Some(frame))) => frame,
        Ok(Ok(None)) => panic!("{stage}: the guardian sent EOF instead of a frame"),
        Ok(Err(error)) => panic!("{stage}: the guardian frame did not decode: {error:?}"),
        Err(_) => panic!("{stage}: no guardian frame within the budget"),
    }
}

/// Bounded session join: the guardian must finish on its own (every path
/// either reaps a released workload or kills the group it created).
fn join_bounded(guardian: std::thread::JoinHandle<i32>) -> i32 {
    let deadline = Instant::now() + BUDGET;
    while !guardian.is_finished() {
        assert!(
            Instant::now() < deadline,
            "the guardian session must finish on its own within the budget"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    guardian
        .join()
        .expect("the guardian session thread must not panic")
}

// Gate semantics ---------------------------------------------------------------

#[test]
fn the_gate_blocks_all_workload_execution_until_release() {
    let temp = tempfile::tempdir().expect("temp dir");
    let marker = temp.path().join("gated-ran");
    let gate = gate_spawn(&format!("touch '{}'", marker.display()));

    // POSIX_SPAWN_START_SUSPENDED holds the task inside the posix_spawn call
    // itself: the touch file is the execution detector and must not exist.
    assert!(
        !poll_condition(Duration::from_secs(1), || marker.exists()),
        "the suspended workload must not execute before release"
    );

    let mut released = gate.release_once().expect("release the gate");
    assert!(
        poll_condition(BUDGET, || marker.exists()),
        "the workload must run after release_once"
    );
    assert!(
        poll_group_gone(released.identity().group_pid),
        "the released workload exits naturally after its command"
    );
    // Joins the in-process guardian once the workload has been reaped.
    released.wait_guardian();
}

#[test]
fn birth_identity_is_delivered_before_release_and_is_parented_by_the_test_process() {
    let gate = gate_spawn("sleep 30");
    let identity = gate.identity();
    assert_ne!(identity.outer_pid, 0, "the outer pid must be real");
    assert_ne!(
        identity.birth_start_identity, 0,
        "the birth identity must be real"
    );
    assert_eq!(
        identity.group_pid, identity.outer_pid,
        "the workload must lead its own process group"
    );
    assert!(
        group_alive(identity.group_pid as i32),
        "the suspended group must exist before release"
    );

    // The daemon re-probes PROC_PIDTBSDINFO itself; the frame must agree with
    // the kernel, and the tuple must be complete and sane.
    let birth = macos_process_birth_identity(identity.outer_pid)
        .expect("the suspended workload must expose its PROC_PIDTBSDINFO birth tuple");
    assert_eq!(birth.pid, identity.outer_pid);
    // The in-process guardian thread spawns the workload from THIS process,
    // so the workload's ppid is the test process's pid — the same parentage
    // macos.rs verifies before handing the gate out.
    assert_eq!(
        birth.ppid,
        std::process::id(),
        "the guardian process must be the workload's parent"
    );
    assert!(birth.is_complete(), "the birth tuple must be complete");
    assert!(
        birth.start_seconds > 1_600_000_000,
        "start_seconds must be a sane epoch value"
    );
    assert!(
        birth.start_useconds < 1_000_000,
        "start_useconds must be a sub-second fraction"
    );
    assert_eq!(
        identity.birth_start_identity,
        birth.start_identity(),
        "the delivered identity must equal the PROC_PIDTBSDINFO re-probe"
    );
    // Never released: the fail-closed Drop must kill the still-suspended group.
    drop(gate);
    assert!(
        poll_group_gone(identity.group_pid),
        "Drop of an unreleased gate must kill the group"
    );
}

#[test]
fn release_is_exactly_once_and_the_group_terminates_naturally() {
    let gate = gate_spawn("true");
    let identity = gate.identity();
    // release_once(mut self) CONSUMES the gate: after this call no
    // MacosGatedWorkload value exists to release again, and
    // MacosReleasedWorkload exposes no release at all — a second release is
    // unrepresentable at the type level (mirrors s08's wording).
    let mut released = gate.release_once().expect("release the gate");
    assert_eq!(
        released.identity(),
        identity,
        "the released handle carries the identity persisted before release"
    );
    assert!(
        poll_group_gone(identity.group_pid),
        "the released workload terminates naturally (sh -c true)"
    );
    released.wait_guardian();
    // The guardian's exit code is consumed by the in-process thread design;
    // the P08 released-exits-0 contract is asserted at protocol level by
    // `the_manual_released_session_exits_zero_after_reaping` below.
}

#[test]
fn dropping_the_gate_without_release_kills_the_suspended_group() {
    let temp = tempfile::tempdir().expect("temp dir");
    let marker = temp.path().join("never-ran");
    let gate = gate_spawn(&format!("touch '{}'", marker.display()));
    let group_pid = gate.identity().group_pid;
    // Fail-closed Drop: close the command end (EOF → the guardian SIGKILLs
    // the still-suspended group), close the reply end, join the thread.
    drop(gate);
    assert!(
        poll_group_gone(group_pid),
        "EOF before release must kill the still-suspended group"
    );
    assert!(
        !marker.exists(),
        "workload code must never have run before the kill"
    );
}

// Birth identity properties ------------------------------------------------------

#[test]
fn start_identity_is_injective_and_live_birth_identity_is_stable() {
    // The encoding (seconds << 32) | microseconds is injective whenever
    // usec < 2^32 (real usec counts are < 1_000_000): this is the fence that
    // distinguishes a recycled pid from its previous instance, so distinct
    // tuples must never collide.
    let tuples = [
        (1_600_000_000u64, 0u64),
        (1_600_000_000, 1),
        (1_600_000_000, 999_999),
        (1_600_000_001, 0),
        (1_600_000_001, 999_999),
        (1_700_000_000, 424_242),
    ];
    let mut encodings = std::collections::BTreeSet::new();
    for (seconds, useconds) in tuples {
        let identity = MacosBirthIdentity {
            pid: 1,
            ppid: 2,
            start_seconds: seconds,
            start_useconds: useconds,
        };
        assert_eq!(
            identity.start_identity(),
            (seconds << 32) | useconds,
            "the encoding must be (seconds << 32) | microseconds"
        );
        assert!(
            encodings.insert(identity.start_identity()),
            "distinct (seconds, useconds) tuples must encode distinctly"
        );
    }
    // A live process reads back the SAME birth tuple on every call: the
    // identity is stable, so recovery comparisons are meaningful.
    let first = macos_process_birth_identity(std::process::id())
        .expect("the test process must have a PROC_PIDTBSDINFO birth identity");
    let second = macos_process_birth_identity(std::process::id())
        .expect("the second read of the test process birth identity");
    assert_eq!(
        first, second,
        "a live pid's birth identity must be stable across calls"
    );
}

#[test]
fn owner_identity_persists_the_birth_tuple_before_release() {
    let gate = gate_spawn("sleep 30");
    let identity = gate.identity();
    // P10.3 ordering: the owner identity is taken BEFORE the gate opens.
    let owner = macos_owner_identity(identity.outer_pid).expect("owner identity before release");
    assert_eq!(owner.pid, identity.outer_pid);
    assert_eq!(
        owner.start_identity, identity.birth_start_identity,
        "the persisted start identity is the PROC_PIDTBSDINFO tuple"
    );
    assert_eq!(
        owner.platform_identity["native"].as_str(),
        Some("macos-gated"),
        "the platform tuple must name the macOS gated origin"
    );
    assert_eq!(
        owner.platform_identity["ppid"].as_u64(),
        Some(u64::from(std::process::id())),
        "the persisted parentage must pin the guardian process"
    );
    assert!(
        owner.platform_identity["startSeconds"]
            .as_u64()
            .expect("startSeconds")
            > 1_600_000_000,
        "the persisted start seconds must be a sane epoch value"
    );
    assert!(
        owner.platform_identity["startUseconds"]
            .as_u64()
            .expect("startUseconds")
            < 1_000_000,
        "the persisted microseconds must be a sub-second fraction"
    );
    drop(gate);
    assert!(
        poll_group_gone(identity.group_pid),
        "cleanup: the unreleased gate must die on drop"
    );
}

// Group ownership ----------------------------------------------------------------

#[test]
fn the_workload_leads_its_own_process_group_before_release() {
    let gate = gate_spawn("sleep 30");
    let identity = gate.identity();
    assert!(
        group_alive(identity.group_pid as i32),
        "the suspended group must exist before release"
    );
    // Independent of macos.rs's own verification: getpgid must report the
    // workload as its own group leader (POSIX_SPAWN_SETPGROUP, verified,
    // never assumed).
    // SAFETY: getpgid(2) on a pid this test created through the gate; no
    // memory is involved.
    let pgid = unsafe { libc::getpgid(identity.outer_pid as libc::pid_t) };
    assert_eq!(
        pgid, identity.outer_pid as libc::pid_t,
        "getpgid must report the workload as its own group leader"
    );
    drop(gate);
    assert!(
        poll_group_gone(identity.group_pid),
        "the unreleased gate must die on drop"
    );
}

// Protocol-level refusal, release and EOF, driven over test-owned streams --------

#[test]
fn the_macos_guardian_refuses_a_foreign_protocol_version() {
    let mut session = open_manual_session();
    // Hello is the negotiation carrier: a foreign version must be answered
    // with an explicit VersionMismatch refusal, never silence — and no
    // workload is spawned (the session ends at the refusal path).
    guardian_protocol::write_frame(
        &mut session.commands,
        &GuardianFrame::Hello {
            version: PROTOCOL_VERSION + 1,
        },
    )
    .expect("write the foreign-version hello");
    let reply = read_frame_bounded(&session.replies, "version refusal");
    assert_eq!(
        reply,
        GuardianFrame::Error {
            code: FrameErrorCode::VersionMismatch
        }
    );
    let code = join_bounded(session.guardian);
    assert_eq!(
        code, GUARDIAN_EXIT_PROTOCOL,
        "a foreign protocol version must refuse service with the P08 exit code"
    );
}

#[test]
fn the_manual_released_session_exits_zero_after_reaping() {
    let temp = tempfile::tempdir().expect("temp dir");
    let marker = temp.path().join("manual-ran");
    let mut session = open_manual_session();
    guardian_protocol::write_frame(
        &mut session.commands,
        &GuardianFrame::Hello {
            version: PROTOCOL_VERSION,
        },
    )
    .expect("write the handshake");
    let reply = read_frame_bounded(&session.replies, "guardian handshake");
    assert_eq!(
        reply,
        GuardianFrame::Hello {
            version: PROTOCOL_VERSION
        }
    );
    guardian_protocol::write_frame(
        &mut session.commands,
        &GuardianFrame::SpawnRequest {
            executable: "/bin/sh".to_string(),
            arguments: vec!["-c".to_string(), format!("touch '{}'", marker.display())],
            cwd: None,
            environment: vec![("PATH".to_string(), "/usr/bin:/bin".to_string())],
        },
    )
    .expect("write the spawn request");
    let (outer_pid, group_pid, birth) = match read_frame_bounded(&session.replies, "identity") {
        GuardianFrame::Identity {
            outer_pid,
            group_pid,
            birth_start_identity,
        } => (outer_pid, group_pid, birth_start_identity),
        other => panic!("expected Identity, got {other:?}"),
    };
    assert_ne!(outer_pid, 0);
    assert_ne!(birth, 0, "the birth identity must be real");
    assert!(
        group_alive(group_pid as i32),
        "the gated workload is suspended but alive"
    );
    assert!(!marker.exists(), "nothing may run before release");

    // Release opens the gate exactly once and is acknowledged.
    guardian_protocol::write_frame(&mut session.commands, &GuardianFrame::Release)
        .expect("write the release");
    let ack = read_frame_bounded(&session.replies, "release acknowledgement");
    assert_eq!(ack, GuardianFrame::ReleaseAck);
    assert!(
        poll_condition(BUDGET, || marker.exists()),
        "the released workload must run after the acknowledged release"
    );
    // After the ack the guardian only reaps the released workload; closing
    // both ends ends the session with the P08 released exit code.
    drop(session.commands);
    drop(session.replies);
    let code = join_bounded(session.guardian);
    assert_eq!(
        code, GUARDIAN_EXIT_RELEASED,
        "the guardian exits 0 after reaping the released workload"
    );
    assert!(
        poll_group_gone(group_pid),
        "the released workload has been reaped with its group"
    );
}

#[test]
fn daemon_eof_before_release_kills_and_exits_nonzero() {
    let temp = tempfile::tempdir().expect("temp dir");
    let marker = temp.path().join("eof-never-ran");
    let mut session = open_manual_session();
    guardian_protocol::write_frame(
        &mut session.commands,
        &GuardianFrame::Hello {
            version: PROTOCOL_VERSION,
        },
    )
    .expect("write the handshake");
    let reply = read_frame_bounded(&session.replies, "guardian handshake");
    assert_eq!(
        reply,
        GuardianFrame::Hello {
            version: PROTOCOL_VERSION
        }
    );
    guardian_protocol::write_frame(
        &mut session.commands,
        &GuardianFrame::SpawnRequest {
            executable: "/bin/sh".to_string(),
            arguments: vec!["-c".to_string(), format!("touch '{}'", marker.display())],
            cwd: None,
            environment: vec![("PATH".to_string(), "/usr/bin:/bin".to_string())],
        },
    )
    .expect("write the spawn request");
    let (outer_pid, group_pid, birth) = match read_frame_bounded(&session.replies, "identity") {
        GuardianFrame::Identity {
            outer_pid,
            group_pid,
            birth_start_identity,
        } => (outer_pid, group_pid, birth_start_identity),
        other => panic!("expected Identity, got {other:?}"),
    };
    assert_ne!(outer_pid, 0);
    assert_ne!(birth, 0, "the birth identity must be real");
    assert!(
        group_alive(group_pid as i32),
        "the gated workload is suspended but alive"
    );
    assert!(!marker.exists(), "nothing may run before release");

    // Daemon death without a Release: close the command end. The guardian
    // must SIGKILL the still-suspended group and exit with the daemon-EOF
    // code — the gate must never open on EOF.
    drop(session.commands);
    drop(session.replies);
    let code = join_bounded(session.guardian);
    assert_eq!(
        code, GUARDIAN_EXIT_DAEMON_EOF,
        "EOF before release is the daemon-EOF exit"
    );
    assert!(
        poll_group_gone(group_pid),
        "the suspended group must be dead after the guardian's EOF path"
    );
    assert!(!marker.exists(), "the workload must never have run");
}

// Descriptor hygiene on the released side ----------------------------------------

#[test]
fn the_released_workload_holds_only_stdio_descriptors() {
    let temp = tempfile::tempdir().expect("temp dir");
    let listing = temp.path().join("fd-listing");
    let gate = gate_spawn(&format!("echo /dev/fd/* > '{}'", listing.display()));
    let mut released = gate.release_once().expect("release the gate");
    assert!(
        poll_condition(BUDGET, || listing.exists()),
        "the workload must run and list its own descriptors"
    );
    let text = std::fs::read_to_string(&listing).expect("read the fd listing");
    let descriptors: Vec<u32> = text
        .split_whitespace()
        .filter_map(|entry| entry.rsplit('/').next()?.parse::<u32>().ok())
        .collect();
    assert!(
        descriptors.contains(&0) && descriptors.contains(&1) && descriptors.contains(&2),
        "stdio must be present in {text:?}"
    );
    // THE P10.3 acceptance property: POSIX_SPAWN_CLOEXEC_DEFAULT pins the
    // inherited set to exactly stdio — no control end, no daemon descriptor,
    // regardless of how many descriptors the spawner holds open.
    assert!(
        descriptors.iter().all(|descriptor| *descriptor <= 2),
        "descriptors beyond stdio leaked into the workload: {descriptors:?}"
    );
    assert!(
        poll_group_gone(released.identity().group_pid),
        "the workload exits naturally after listing"
    );
    released.wait_guardian();
}

// Structural pins ----------------------------------------------------------------

#[test]
fn the_macos_path_never_reads_proc_or_enumerates_the_kernel_table() {
    let source = include_str!("../src/process_guard/macos.rs");
    // macOS has no /proc: no path may even name one at runtime.
    assert!(
        !source.contains("/proc/"),
        "the macOS module must never read /proc"
    );
    // The PRD forbids the lazy alternative: a KERN_PROC_ALL enumeration (or a
    // raw sysctl/kinfo_proc parse) must not replace the birth tuple.
    assert!(
        !source.contains("KERN_PROC_ALL"),
        "a kernel-table enumeration must not replace the birth tuple"
    );
    assert!(
        !source.contains("libc::sysctl") && !source.contains("libc::kinfo_proc"),
        "the birth tuple must come from proc_pidinfo, not raw sysctl parsing"
    );
    // The real mechanism is present and named.
    assert!(
        source.contains("libc::proc_pidinfo") && source.contains("libc::PROC_PIDTBSDINFO"),
        "the birth tuple must be read with proc_pidinfo(PROC_PIDTBSDINFO)"
    );
}

#[test]
fn the_launch_gate_keeps_its_fail_closed_shape() {
    let source = include_str!("../src/process_guard/macos.rs");
    // The kernel holds the task before its first instruction (stronger than
    // P08's pre-exec raise(SIGSTOP)); SIGCONT is the only opener.
    assert!(
        source.contains("libc::POSIX_SPAWN_START_SUSPENDED"),
        "the gate must be POSIX_SPAWN_START_SUSPENDED"
    );
    assert!(
        source.contains("libc::POSIX_SPAWN_SETPGROUP")
            && source.contains("posix_spawnattr_setpgroup"),
        "the workload must be spawned into its own process group"
    );
    assert!(
        source.contains("libc::POSIX_SPAWN_CLOEXEC_DEFAULT"),
        "the inherited descriptor set must be pinned to stdio"
    );
    assert!(
        source.contains("libc::SIGCONT"),
        "SIGCONT must be the only gate opener"
    );
    // Exactly-once release: the gate is consumed by value; an unreleased gate
    // fails closed on Drop.
    assert!(
        source.contains("pub fn release_once(mut self)"),
        "release_once must consume the gate"
    );
    assert!(
        source.contains("impl Drop for MacosGatedWorkload"),
        "an unreleased gate must fail closed on Drop"
    );
    // The P08 exit-code contract is reused verbatim by the state machine.
    assert!(
        source.contains("GUARDIAN_EXIT_RELEASED")
            && source.contains("GUARDIAN_EXIT_DAEMON_EOF")
            && source.contains("GUARDIAN_EXIT_PROTOCOL"),
        "serve_macos_guardian must reuse the P08 exit codes"
    );
}
