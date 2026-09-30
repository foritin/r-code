//! P06 — raw suspended Windows spawn.
//!
//! Proves on a real helper process: nothing is observable before resume
//! (CREATE_SUSPENDED), the marker is the first stdout after resume, resume
//! is exactly-once, the child environment is exactly the explicit map, only
//! the three stdio handles cross the spawn boundary (PROC_THREAD_ATTRIBUTE_
//! HANDLE_LIST filters even an inheritable canary), teardown is fail-closed
//! (abort/Drop terminate, never leak a suspended process), and the primitive
//! stays raw (no tokio, no premature supervisor wiring).

#![cfg(windows)]

use r_code_runtime::process_guard::windows::{
    is_process_alive, spawn_suspended, GuardianError, RawSpawnSpec, RawSuspendedChild,
};
use r_code_runtime::process_guard::BootIdentity;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    CloseHandle, SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT,
};
use windows_sys::Win32::System::Pipes::{CreatePipe, PeekNamedPipe};

const HELPER: &str = env!("CARGO_BIN_EXE_process-tree-helper");
const MARKER: &[u8] = b"resumed\n";
const BUDGET: Duration = Duration::from_secs(5);

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn spawn_helper(arguments: &[String], environment: &BTreeMap<String, String>) -> RawSuspendedChild {
    let spec = RawSpawnSpec {
        executable: Path::new(HELPER),
        arguments,
        cwd: None,
        environment,
    };
    spawn_suspended(&spec).expect("spawn the helper suspended")
}

/// Drain stdout after the child has exited: buffered bytes first, then the
/// pipe breaks (write end closed), which ends the loop.
fn read_all_stdout(child: &mut RawSuspendedChild) -> Vec<u8> {
    let mut collected = Vec::new();
    let mut buffer = [0u8; 512];
    loop {
        match child.read_stdout(&mut buffer) {
            Ok(0) => break,
            Ok(n) => collected.extend_from_slice(&buffer[..n]),
            Err(_) => break,
        }
    }
    collected
}

fn wait_for_available(child: &RawSuspendedChild, at_least: u32) -> bool {
    let deadline = Instant::now() + BUDGET;
    while Instant::now() < deadline {
        if child.stdout_available_bytes() >= at_least {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.stdout_available_bytes() >= at_least
}

fn peek_stderr_available(child: &RawSuspendedChild) -> u32 {
    let mut available: u32 = 0;
    // SAFETY: valid out-pointer; the handle is owned by the child wrapper.
    let ok = unsafe {
        PeekNamedPipe(
            child.stderr_read_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut available,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        0
    } else {
        available
    }
}

/// True once the pid is gone within the budget.
fn poll_until_dead(pid: u32) -> bool {
    let deadline = Instant::now() + BUDGET;
    while Instant::now() < deadline && is_process_alive(pid) {
        std::thread::sleep(Duration::from_millis(20));
    }
    !is_process_alive(pid)
}

/// An extra pipe marked INHERITABLE in the test process and deliberately
/// absent from the spawn spec: with bInheritHandles=1 only the handle list
/// can keep it out of the child, so `absent` proves the list filters.
fn create_inheritable_canary() -> (HANDLE, HANDLE) {
    let mut read: HANDLE = std::ptr::null_mut();
    let mut write: HANDLE = std::ptr::null_mut();
    // SAFETY: valid out-pointers; the caller closes both ends.
    let created = unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), 0) };
    assert_eq!(created, 1, "CreatePipe for the canary probe");
    // SAFETY: `read` came from CreatePipe above.
    let flagged = unsafe { SetHandleInformation(read, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
    assert_eq!(
        flagged, 1,
        "the canary must be inheritable to prove filtering"
    );
    (read, write)
}

#[test]
fn child_writes_nothing_until_resumed_and_marks_resume_first() {
    let environment = BTreeMap::new();
    let mut child = spawn_helper(&args(&[]), &environment);

    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        child.stdout_available_bytes(),
        0,
        "stdout must be empty while the child is suspended"
    );
    assert_eq!(
        peek_stderr_available(&child),
        0,
        "stderr must be empty while the child is suspended"
    );

    child.resume_once().expect("resume the helper");
    assert!(
        wait_for_available(&child, MARKER.len() as u32),
        "the marker must appear after resume"
    );
    let mut buffer = [0u8; 16];
    let read = child
        .read_stdout(&mut buffer)
        .expect("blocking marker read");
    assert_eq!(&buffer[..read], MARKER, "the marker is the first stdout");

    assert!(child.wait(BUDGET), "the helper exits on its own");
    assert_eq!(child.exit_code(), Some(0));
    let mut leftover = [0u8; 4];
    assert!(
        child.read_stdout(&mut leftover).is_err(),
        "stdout must break once the child and its write end are gone"
    );
}

#[test]
fn resume_is_exactly_once() {
    let environment = BTreeMap::new();
    let mut child = spawn_helper(&args(&[]), &environment);
    child.resume_once().expect("the first resume succeeds");
    assert!(
        matches!(child.resume_once(), Err(GuardianError::State(_))),
        "a second resume must be a state error"
    );
    assert!(child.wait(BUDGET));
}

#[test]
fn child_environment_is_exactly_the_explicit_map() {
    let secret = format!("P06_PARENT_ONLY_{}", std::process::id());
    std::env::set_var(&secret, "parent-secret");
    let environment = BTreeMap::from([("P06MARK".to_string(), "yes".to_string())]);
    let arguments = args(&["--env", "P06MARK", "--env", &secret]);

    let spec = RawSpawnSpec {
        executable: Path::new(HELPER),
        arguments: &arguments,
        cwd: None,
        environment: &environment,
    };
    let mut child = spawn_suspended(&spec).unwrap_or_else(|error| {
        panic!(
            "a non-empty explicit environment must spawn: {error:?} — a UTF-16 \
             lpEnvironment without CREATE_UNICODE_ENVIRONMENT is parsed as ANSI \
             and Windows rejects it with ERROR_INVALID_PARAMETER (87)"
        )
    });
    child.resume_once().expect("resume the helper");
    assert!(child.wait(BUDGET), "the helper exits on its own");

    let text = String::from_utf8(read_all_stdout(&mut child)).expect("ascii helper output");
    assert!(text.starts_with("resumed\n"), "marker first: {text:?}");
    assert!(
        text.contains("env:P06MARK=yes\n"),
        "explicit entry missing: {text:?}"
    );
    assert!(
        text.contains(&format!("env:{secret}=absent\n")),
        "the parent environment leaked into the child: {text:?}"
    );
}

#[test]
fn only_the_stdio_handles_cross_the_spawn_boundary() {
    let (canary_read, canary_write) = create_inheritable_canary();
    let canary_value = canary_read as usize;
    let environment = BTreeMap::new();
    let arguments = args(&["--canary", &canary_value.to_string(), "--canary-self"]);

    let mut child = spawn_helper(&arguments, &environment);
    child.resume_once().expect("resume the helper");
    assert!(child.wait(BUDGET), "the helper exits on its own");

    let text = String::from_utf8(read_all_stdout(&mut child)).expect("ascii helper output");
    assert!(
        text.contains("canary=absent\n"),
        "an inheritable non-stdio handle leaked into the child: {text:?}"
    );
    assert!(
        text.contains("canary-self=PRESENT\n"),
        "the canary probe lost discrimination (positive control): {text:?}"
    );
    // SAFETY: both ends came from create_inheritable_canary above.
    unsafe {
        CloseHandle(canary_read);
        CloseHandle(canary_write);
    }
}

#[test]
fn stdin_roundtrip_and_exit_code_track_the_child() {
    let environment = BTreeMap::new();
    let arguments = args(&["--echo-stdin", "4", "--exit", "7"]);
    let mut child = spawn_helper(&arguments, &environment);

    child.resume_once().expect("resume the helper");
    assert_eq!(
        child.exit_code(),
        None,
        "the helper cannot exit before its stdin read completes"
    );
    assert_eq!(child.write_stdin(b"abcd").expect("write to child stdin"), 4);
    assert!(child.wait(BUDGET), "the helper exits after the stdin echo");

    let output = read_all_stdout(&mut child);
    assert!(output.starts_with(MARKER), "marker first: {output:?}");
    assert!(output.ends_with(b"stdin:abcd\n"), "stdin echo: {output:?}");
    assert_eq!(child.exit_code(), Some(7));
}

#[test]
fn abort_before_resume_kills_the_child_and_is_idempotent() {
    let environment = BTreeMap::new();
    let mut child = spawn_helper(&args(&[]), &environment);
    let pid = child.pid();
    assert!(is_process_alive(pid), "a suspended child is alive");

    child.abort();
    assert!(
        poll_until_dead(pid),
        "an aborted never-resumed child must be gone"
    );
    assert_eq!(
        child.stdout_available_bytes(),
        0,
        "no output may appear from an aborted child"
    );
    child.abort(); // handles are already closed: this must be a no-op
}

#[test]
fn dropping_a_never_resumed_child_terminates_it() {
    let environment = BTreeMap::new();
    let child = spawn_helper(&args(&[]), &environment);
    let pid = child.pid();
    assert!(is_process_alive(pid), "a suspended child is alive");
    drop(child);
    assert!(
        poll_until_dead(pid),
        "Drop must terminate (not leak) a never-resumed child"
    );
}

#[test]
fn terminate_after_resume_kills_a_running_child() {
    let environment = BTreeMap::new();
    let mut child = spawn_helper(&args(&["--echo-stdin", "4"]), &environment);
    let pid = child.pid();
    child.resume_once().expect("resume the helper");
    child.terminate();
    assert!(
        poll_until_dead(pid),
        "a terminated running child must be gone"
    );
}

#[test]
fn exit_code_reports_the_child_code_and_terminate_cleanup_is_idempotent() {
    let environment = BTreeMap::new();
    let mut child = spawn_helper(&args(&["--exit", "7"]), &environment);
    child.resume_once().expect("resume the helper");
    assert!(child.wait(BUDGET), "the helper with --exit 7 exits");
    assert_eq!(child.exit_code(), Some(7));
    child.terminate(); // cleanup after natural exit: idempotent, no panic
}

#[test]
fn identities_are_populated_and_durable() {
    let environment = BTreeMap::new();
    let child = spawn_helper(&args(&[]), &environment);
    let pid = child.pid();
    assert_ne!(pid, 0);
    let start_identity = child.start_identity();
    assert_ne!(start_identity, 0, "the child start identity must be real");

    let owner = child.owner_identity().expect("owner identity");
    assert_eq!(owner.pid, pid);
    assert_eq!(owner.start_identity, start_identity);
    assert!(owner.boot_identity.as_str().starts_with("windows:"));
    assert_eq!(
        BootIdentity::parse(owner.boot_identity.to_string()).unwrap(),
        owner.boot_identity
    );
    assert!(!owner.platform_identity.is_null());
    assert_eq!(
        owner.platform_identity["native"].as_str(),
        Some("windows-raw-suspended")
    );
    assert!(!owner.platform_identity_digest.is_empty());
}

#[test]
fn environment_spec_rejects_reserved_bytes_without_spawning() {
    let environment_cases: [(&str, &str); 3] = [
        ("BAD\u{0}KEY", "value"),
        ("BAD=KEY", "value"),
        ("OK_KEY", "va\u{0}lue"),
    ];
    for (key, value) in environment_cases {
        let environment = BTreeMap::from([(key.to_string(), value.to_string())]);
        let spec = RawSpawnSpec {
            executable: Path::new(HELPER),
            arguments: &[],
            cwd: None,
            environment: &environment,
        };
        assert!(
            matches!(spawn_suspended(&spec), Err(GuardianError::Api(_))),
            "reserved byte in {key:?}={value:?} must fail cleanly"
        );
    }
}

#[test]
fn the_primitive_stays_raw_flags_explicit_and_unwired() {
    let source = include_str!("../src/process_guard/windows.rs");
    for needle in [
        "CREATE_SUSPENDED | CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT",
        // The environment block is UTF-16: without CREATE_UNICODE_ENVIRONMENT
        // Windows parses lpEnvironment as ANSI and rejects every non-empty
        // explicit environment with ERROR_INVALID_PARAMETER.
        "CREATE_UNICODE_ENVIRONMENT",
        "PROC_THREAD_ATTRIBUTE_HANDLE_LIST",
        "STARTF_USESTDHANDLES",
        "size_of::<[HANDLE; 3]>",
        "SetHandleInformation(parent, HANDLE_FLAG_INHERIT, 0)",
    ] {
        assert!(source.contains(needle), "the primitive lost {needle}");
    }
    assert!(
        !source.contains("process_supervisor") && !source.contains("SupervisorJournal"),
        "P06 must not be wired into the supervisor before P07"
    );
    let primitive = source.split("pub fn spawn_suspended").nth(1).unwrap();
    assert!(
        !primitive.contains("tokio"),
        "the raw spawn primitive must not touch async process machinery"
    );
}
