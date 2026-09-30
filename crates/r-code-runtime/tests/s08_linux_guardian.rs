//! P08 — guardian-as-spawner launch gate (Linux).
//!
//! Proves on real Linux processes: the guardian binary exists FIRST and forks
//! the workload behind a release gate — no workload instruction runs before
//! `release_once` (a touch file is the execution detector; stdio is null at
//! this layer), the identity is delivered and verifiable BEFORE release, an
//! unreleased gate dies on Drop or daemon EOF without ever running, release
//! is exactly once, a foreign protocol version is refused with a nonzero
//! guardian exit, the released workload holds only stdio (fd set ⊆ {0,1,2}),
//! and the post-release group kill sweeps grandchildren while the guardian
//! reaps and exits 0.
//!
//! Windows compiles this to an empty suite (`#![cfg(target_os = "linux")]`);
//! it runs on native Linux CI. Tests spawning through the daemon seam
//! serialize on one lock, so any `--test-threads` setting is safe.

#![cfg(target_os = "linux")]

use r_code_runtime::process_guard::unix::guardian_protocol::{
    self, FrameErrorCode, GuardianFrame, GUARDIAN_CONTROL_FD, GUARDIAN_REPLY_FD, PROTOCOL_VERSION,
};
use r_code_runtime::process_guard::unix::{
    group_alive, process_start_identity, spawn_via_guardian, GatedWorkload, GuardianError,
    GuardianSpawnRequest, ReleasedWorkload, GUARDIAN_BIN_ENV, GUARDIAN_EXIT_DAEMON_EOF,
    GUARDIAN_EXIT_PROTOCOL, GUARDIAN_EXIT_RELEASED,
};
use std::collections::BTreeMap;
use std::os::fd::FromRawFd;
use std::os::unix::process::CommandExt;
use std::time::{Duration, Instant};

/// Hard cap for every bounded wait in this suite.
const BUDGET: Duration = Duration::from_secs(10);

/// The QA-built guardian binary, handed to the daemon-side seam through
/// `R_CODE_GUARDIAN_BIN` (the seam reads the variable at spawn time).
const GUARDIAN_BIN: &str = env!("CARGO_BIN_EXE_r-code-process-guardian");

/// Serializes every test that spawns through the daemon seam: the seam reads
/// `R_CODE_GUARDIAN_BIN` at spawn time, and the negative override test below
/// swaps it temporarily.
static GUARDIAN_BIN_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Hold for the whole body of any test that spawns through the seam.
fn spawn_env() -> std::sync::MutexGuard<'static, ()> {
    let guard = GUARDIAN_BIN_LOCK.lock().expect("guardian bin env lock");
    static SET: std::sync::Once = std::sync::Once::new();
    SET.call_once(|| {
        std::env::set_var(GUARDIAN_BIN_ENV, GUARDIAN_BIN);
    });
    guard
}

/// The explicit workload environment: the workload never inherits the
/// guardian's environment, and /bin/sh needs PATH to exec `sleep` and `touch`.
fn workload_environment() -> BTreeMap<String, String> {
    BTreeMap::from([("PATH".to_string(), "/usr/bin:/bin".to_string())])
}

/// A spawn request for `/bin/sh -c <script>` with the explicit environment.
fn sh_request(script: &str) -> GuardianSpawnRequest {
    GuardianSpawnRequest {
        executable: "/bin/sh".to_string(),
        arguments: vec!["-c".to_string(), script.to_string()],
        cwd: None,
        environment: workload_environment(),
    }
}

/// Spawn the stopped gate through the real daemon seam. The caller must hold
/// [`spawn_env`] for the whole body of the test.
fn gate_spawn(script: &str) -> GatedWorkload {
    spawn_via_guardian(sh_request(script))
        .expect("spawn_via_guardian must hand back the stopped gate")
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

/// Group membership by /proc scan: every pid whose pgrp equals `group_pid`.
/// kill(-pgid) reaches every member, so the scan doubles as the containment
/// oracle. comm may contain spaces and parentheses, so the fixed fields are
/// parsed after the closing parenthesis (state is field 3, pgrp field 5).
fn group_members(group_pid: u32) -> Vec<u32> {
    let mut members = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return members;
    };
    for entry in entries.flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        let Some(after_comm) = stat.rsplit(')').next() else {
            continue;
        };
        let fields: Vec<&str> = after_comm.split_whitespace().collect();
        if fields.get(2).and_then(|value| value.parse::<u32>().ok()) == Some(group_pid) {
            members.push(pid);
        }
    }
    members
}

/// The member list once it holds `at_least` pids (None on timeout).
fn poll_group_members(group_pid: u32, at_least: usize) -> Option<Vec<u32>> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let members = group_members(group_pid);
        if members.len() >= at_least {
            return Some(members);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Bounded guardian reap through the released handle (which may only reap,
/// never kill): the status once the guardian has exited.
fn reap_released_guardian(released: &mut ReleasedWorkload) -> std::process::ExitStatus {
    let deadline = Instant::now() + BUDGET;
    loop {
        if let Some(status) = released.reap_guardian() {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "the guardian must exit once its workload is gone"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

// Manual guardian-driver fixtures ---------------------------------------------

struct ManualSession {
    guardian: std::process::Child,
    /// Daemon → guardian commands (the guardian's fixed fd 3).
    commands: std::fs::File,
    /// Guardian → daemon replies (the guardian's fixed fd 4).
    replies: std::fs::File,
}

fn cloexec_pipe() -> (i32, i32) {
    let mut fds = [0i32; 2];
    // SAFETY: pipe2 fills the two out-slots; O_CLOEXEC keeps every stray end
    // out of the guardian's exec image.
    let created = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    assert_eq!(created, 0, "pipe2 for the manual guardian session");
    (fds[0], fds[1])
}

/// RAII close for the guardian-side pipe ends held in the parent.
struct RawFdGuard(i32);

impl Drop for RawFdGuard {
    fn drop(&mut self) {
        // SAFETY: the descriptor came from cloexec_pipe and is closed once.
        unsafe {
            libc::close(self.0);
        }
    }
}

/// Point one fixed control descriptor at a pipe end inside the pre-exec
/// callback (dup2(fd, fd) is a no-op that does NOT clear O_CLOEXEC — a pipe
/// that already landed on the fixed number must clear its own flag instead).
fn place_on_fixed_fd(source_fd: i32, fixed_fd: i32) -> std::io::Result<()> {
    // SAFETY: fcntl/dup2 on descriptors from cloexec_pipe; the callback runs
    // in the forked child before exec, single-threaded.
    unsafe {
        if source_fd == fixed_fd {
            if libc::fcntl(source_fd, libc::F_SETFD, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
        } else if libc::dup2(source_fd, fixed_fd) == -1 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Start the guardian binary over pipes the TEST owns (the same fixed-fd
/// contract `spawn_via_guardian` sets up), so protocol-level refusals and the
/// EOF path are observable down to the guardian's exit status.
fn open_manual_session() -> ManualSession {
    let (command_read, command_write) = cloexec_pipe();
    let (reply_read, reply_write) = cloexec_pipe();
    let command_read = RawFdGuard(command_read);
    let reply_write = RawFdGuard(reply_write);

    let mut command = std::process::Command::new(GUARDIAN_BIN);
    command
        .arg("--control-fd")
        .arg(GUARDIAN_CONTROL_FD.to_string())
        .arg("--reply-fd")
        .arg(GUARDIAN_REPLY_FD.to_string());
    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::null());
    command.stderr(std::process::Stdio::null());
    command.process_group(0);
    let control_source = command_read.0;
    let reply_source = reply_write.0;
    // SAFETY: the callback runs in the forked child before exec and installs
    // exactly the two control ends on the fixed descriptors; every other
    // descriptor is O_CLOEXEC and vanishes at exec.
    unsafe {
        command.pre_exec(move || {
            place_on_fixed_fd(control_source, GUARDIAN_CONTROL_FD)?;
            place_on_fixed_fd(reply_source, GUARDIAN_REPLY_FD)
        });
    }
    let guardian = command.spawn().expect("spawn the guardian binary manually");
    // The dup2'd copies live in the guardian now; drop the originals so its
    // exit is observable as EOF on the reply pipe.
    drop(command_read);
    drop(reply_write);
    ManualSession {
        guardian,
        commands: unsafe { std::fs::File::from_raw_fd(command_write) },
        replies: unsafe { std::fs::File::from_raw_fd(reply_read) },
    }
}

/// Bounded exit: the guardian must exit on its own within the budget.
fn wait_exit_bounded(guardian: &mut std::process::Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + BUDGET;
    loop {
        if let Some(status) = guardian.try_wait().expect("try_wait the guardian") {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "the guardian must exit on its own within the budget"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

// Gate semantics ---------------------------------------------------------------

#[test]
fn the_gate_blocks_all_workload_execution_until_release() {
    let _env = spawn_env();
    let temp = tempfile::tempdir().expect("temp dir");
    let marker = temp.path().join("gated-ran");
    let gate = gate_spawn(&format!("touch '{}'", marker.display()));

    // No stdio crosses this layer, so the touch file is the execution
    // detector: the SIGSTOPped workload must not create it before release.
    assert!(
        !poll_condition(Duration::from_secs(1), || marker.exists()),
        "the stopped workload must not execute before release"
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
    let status = reap_released_guardian(&mut released);
    assert_eq!(
        status.code(),
        Some(GUARDIAN_EXIT_RELEASED),
        "the guardian exits cleanly after reaping the released workload"
    );
}

#[test]
fn identity_is_delivered_before_release_and_is_verifiable() {
    let _env = spawn_env();
    let gate = gate_spawn("sleep 30");
    let identity = gate.identity();
    assert_ne!(identity.outer_pid, 0, "the outer pid must be real");
    assert_ne!(
        identity.birth_start_identity, 0,
        "the birth identity must be real"
    );
    // fork_stopped_workload verifies pgid == pid before delivering identity.
    assert_eq!(
        identity.group_pid, identity.outer_pid,
        "the workload must lead its own process group"
    );
    assert!(
        group_alive(identity.group_pid as i32),
        "the stopped group must exist before release"
    );
    // The birth identity is the real /proc start time of the stopped
    // workload, not a fabrication.
    assert_eq!(
        process_start_identity(identity.outer_pid),
        identity.birth_start_identity,
        "the delivered birth identity must match /proc"
    );
    // Never released: the fail-closed Drop must kill the still-stopped group.
    let group_pid = identity.group_pid;
    drop(gate);
    assert!(
        poll_group_gone(group_pid),
        "Drop of an unreleased gate must kill the group"
    );
}

#[test]
fn release_is_exactly_once_and_the_group_terminates_naturally() {
    let _env = spawn_env();
    let gate = gate_spawn("true");
    let identity = gate.identity();
    // release_once(self) CONSUMES the gate, so a second release is
    // unrepresentable at the type level: after this call no GatedWorkload
    // value exists to release again, and ReleasedWorkload exposes no release
    // at all — the exactly-once property needs no runtime flag.
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
    let status = reap_released_guardian(&mut released);
    assert_eq!(status.code(), Some(GUARDIAN_EXIT_RELEASED));
}

#[test]
fn dropping_the_gate_without_release_kills_the_stopped_group() {
    let _env = spawn_env();
    let temp = tempfile::tempdir().expect("temp dir");
    let marker = temp.path().join("never-ran");
    let gate = gate_spawn(&format!("touch '{}'", marker.display()));
    let group_pid = gate.identity().group_pid;
    // Fail-closed Drop: close the command end (EOF → the guardian SIGKILLs
    // the stopped group), close the reply end, reap the guardian with a
    // bounded window. The guardian's exit status is consumed by the teardown;
    // the observable nonzero EOF exit is asserted directly in
    // `daemon_eof_before_release_kills_and_exits_nonzero` below.
    drop(gate);
    assert!(
        poll_group_gone(group_pid),
        "EOF before release must kill the still-stopped group"
    );
    assert!(
        !marker.exists(),
        "workload code must never have run before the kill"
    );
}

#[test]
fn a_gate_that_never_reaches_persistence_never_executes() {
    let _env = spawn_env();
    let temp = tempfile::tempdir().expect("temp dir");
    let marker = temp.path().join("crash-never-ran");
    let gate = gate_spawn(&format!("touch '{}'", marker.display()));
    let group_pid = gate.identity().group_pid;
    // Simulate the daemon dying WITHOUT any destructor running: leak the gate
    // with mem::forget, so neither the EOF drop path nor any cleanup executes
    // on the daemon side. The invariant under test: without an explicit
    // Release the gate holds by itself — the workload stays SIGSTOPped and no
    // instruction of it runs. (The EOF-driven kill for an actually-dead
    // daemon is proven by the drop test above and the manual EOF test below;
    // this leaked session is reclaimed when the test binary exits and every
    // pipe end closes.)
    std::mem::forget(gate);
    assert!(
        !poll_condition(Duration::from_secs(2), || marker.exists()),
        "no instruction of the workload may run without a release"
    );
    assert!(
        group_alive(group_pid as i32),
        "the unreleased session must stay parked, not silently killed"
    );
}

#[test]
fn a_missing_guardian_binary_refuses_instead_of_running_unguarded() {
    let _env = spawn_env();
    let previous = std::env::var(GUARDIAN_BIN_ENV).ok();
    std::env::set_var(GUARDIAN_BIN_ENV, "/nonexistent/r-code-process-guardian");
    let outcome = spawn_via_guardian(sh_request("true"));
    // Restore before asserting so a failure cannot poison the other tests.
    match previous {
        Some(value) => std::env::set_var(GUARDIAN_BIN_ENV, value),
        None => std::env::remove_var(GUARDIAN_BIN_ENV),
    }
    assert!(
        matches!(outcome, Err(GuardianError::Unsupported(_))),
        "a missing guardian binary must refuse, never run unguarded: {outcome:?}"
    );
}

// Protocol-level refusal and EOF, driven over test-owned pipes -----------------

#[test]
fn the_guardian_refuses_a_foreign_protocol_version() {
    let mut session = open_manual_session();
    // Hello is the negotiation carrier: a foreign version must be answered
    // with an explicit VersionMismatch refusal, never silence, and service
    // must be refused.
    guardian_protocol::write_frame(
        &mut session.commands,
        &GuardianFrame::Hello {
            version: PROTOCOL_VERSION + 1,
        },
    )
    .expect("write the foreign-version hello");
    let reply = guardian_protocol::read_frame(&mut session.replies)
        .expect("read the guardian reply")
        .expect("a refusal frame, not EOF");
    assert_eq!(
        reply,
        GuardianFrame::Error {
            code: FrameErrorCode::VersionMismatch
        }
    );
    let status = wait_exit_bounded(&mut session.guardian);
    assert_eq!(
        status.code(),
        Some(GUARDIAN_EXIT_PROTOCOL),
        "a foreign protocol version must refuse service"
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
    let reply = guardian_protocol::read_frame(&mut session.replies)
        .expect("read the guardian handshake")
        .expect("a hello, not EOF");
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
    let (outer_pid, group_pid, birth) = match guardian_protocol::read_frame(&mut session.replies)
        .expect("read the identity")
        .expect("an identity frame, not EOF")
    {
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
        "the gated workload is stopped but alive"
    );
    assert!(!marker.exists(), "nothing may run before release");

    // Daemon death without a Release: close the command end. The guardian
    // must SIGKILL the still-stopped group and exit with the daemon-EOF code.
    drop(session.commands);
    drop(session.replies);
    let status = wait_exit_bounded(&mut session.guardian);
    assert_eq!(
        status.code(),
        Some(GUARDIAN_EXIT_DAEMON_EOF),
        "EOF before release is the daemon-EOF exit"
    );
    assert!(
        poll_group_gone(group_pid),
        "the stopped group must be dead after the guardian's EOF path"
    );
    assert!(!marker.exists(), "the workload must never have run");
}

// Descriptor hygiene and containment on the released side ----------------------

#[test]
fn the_released_workload_holds_no_control_descriptors() {
    let _env = spawn_env();
    let temp = tempfile::tempdir().expect("temp dir");
    let listing = temp.path().join("fd-listing");
    let gate = gate_spawn(&format!("echo /proc/self/fd/* > '{}'", listing.display()));
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
    // THE acceptance property: the workload inherited exactly /dev/null stdio
    // — no control descriptor 3/4, no daemon pipe end, nothing else.
    assert!(
        descriptors.iter().all(|descriptor| *descriptor <= 2),
        "control descriptors leaked into the workload: {descriptors:?}"
    );
    assert!(
        poll_group_gone(released.identity().group_pid),
        "the workload exits naturally after listing"
    );
    let status = reap_released_guardian(&mut released);
    assert_eq!(status.code(), Some(GUARDIAN_EXIT_RELEASED));
}

#[test]
fn the_released_group_kill_sweeps_grandchildren_and_the_guardian_reaps() {
    let _env = spawn_env();
    let gate = gate_spawn("sleep 30 & wait");
    let identity = gate.identity();
    let mut released = gate.release_once().expect("release the gate");

    // After the SIGCONT the shell forks a grandchild into the same group.
    let members = poll_group_members(identity.group_pid, 2)
        .expect("the shell and its sleeping grandchild must share the group");
    assert!(
        members.contains(&identity.outer_pid),
        "the shell must be in {members:?}"
    );

    // The daemon's own termination primitive is the group signal: kill(-pgid)
    // must reach every member, including the grandchild.
    // SAFETY: kill(2) on a group the guardian created and verified; signal
    // delivery IS the mechanism under test.
    unsafe {
        libc::kill(-(identity.group_pid as i32), libc::SIGKILL);
    }
    assert!(
        poll_group_gone(identity.group_pid),
        "kill(-pgid) must sweep the shell and its grandchild"
    );
    let status = reap_released_guardian(&mut released);
    assert_eq!(
        status.code(),
        Some(GUARDIAN_EXIT_RELEASED),
        "the guardian exits 0 after reaping the terminated released workload"
    );
}

// Structural pins ---------------------------------------------------------------

#[test]
fn the_launch_gate_keeps_its_fail_closed_shape() {
    let source = include_str!("../src/process_guard/unix.rs");
    // P08.1: both control pipes are O_CLOEXEC from creation.
    assert!(
        source.contains("libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC)"),
        "the control pipes must be created O_CLOEXEC"
    );
    // The workload's pre-exec closes the fixed control descriptors.
    assert!(
        source.contains("let _ = libc::close(fd);"),
        "the workload pre-exec must close the control descriptors"
    );
    // The kernel kills the workload if the guardian dies first.
    assert!(
        source.contains("libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0)"),
        "the workload must die with the guardian (PR_SET_PDEATHSIG)"
    );
    // Stop-gate ordering: setpgid and the control-fd close happen BEFORE
    // raise(SIGSTOP) — stopping earlier would freeze the setup.
    let setpgid = source
        .find("libc::setpgid(0, 0)")
        .expect("the child setpgid exists");
    let raise = source
        .find("libc::raise(libc::SIGSTOP)")
        .expect("the stop gate exists");
    assert!(
        setpgid < raise,
        "the stop gate must be the LAST pre-exec step"
    );
    // Exactly-once release: the gate is consumed by value.
    assert!(
        source.contains("pub fn release_once(mut self)"),
        "release_once must consume the gate"
    );
    assert!(
        source.contains("impl Drop for GatedWorkload"),
        "an unreleased gate must fail closed on Drop"
    );
}
