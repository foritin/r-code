//! P18 QA — the macOS Seatbelt single-process Harness profile under real
//! verification. Native macOS CI owns execution; on every other host this
//! file is parse-only (an empty suite). Nothing here mocks sandbox-exec:
//! the lifecycle e2e verifies the pinned `/usr/bin/sandbox-exec`
//! (root:wheel, no group/world write), builds the NoWorkspaceSingleProcess
//! profile via `MacosSeatbeltBackend::build_profile` with a REAL
//! dynamically-linked executable copied into a temp path as the literal
//! Harness, and LAUNCHES `/usr/bin/sandbox-exec -p '<profile>' <harness>`
//! for real — proving the initial literal exec, dyld over the frozen read
//! roots (the copy of `/bin/sh` cannot start unless libSystem resolves),
//! fork denial, non-approved-exec denial (with the literal self-replacement
//! still allowed), and the write/network/user-home denials, all observed
//! through stdout markers of an unmocked workload. The backend stays
//! non-activating (P20 pending) — pinned by the `run_probes` refusal, the
//! write-class `policy_digest` refusal and source pins on the P13 gate
//! material. Environmental unavailability is recorded honestly, never
//! faked: a missing sandbox-exec/curl asserts the fail-closed shape and
//! returns.

#![cfg(target_os = "macos")]

use r_code_runtime::services::sandbox::macos::{
    build_no_workspace_single_process_profile, seatbelt_policy_material,
    verify_pinned_sandbox_exec, MacosSeatbeltBackend, SeatbeltProfileClass, SeatbeltProfileError,
    BASELINE_RUNTIME_READ_ROOTS, PINNED_SANDBOX_EXEC_PATH,
};
use r_code_runtime::services::sandbox::{
    SafetyBinaryIdentity, SandboxBackend, SandboxNetworkClass, SandboxProbeId,
    SandboxProfileMaterial,
};
use std::io::Read;
use std::net::TcpListener;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Wall clock each real launch gets before the process group is killed.
const LAUNCH_DEADLINE: Duration = Duration::from_secs(30);

/// The pinned sandbox-exec must verify, or this host honestly lacks it:
/// on success the ownership/mode immutability contract is re-asserted
/// from the raw stat, on genuine absence the fail-closed `BinaryMissing`
/// verdict is asserted (never a fake success). Returns whether the
/// launch-capable path may run.
fn verified_pinned_sandbox_exec() -> bool {
    let pinned = Path::new(PINNED_SANDBOX_EXEC_PATH);
    match verify_pinned_sandbox_exec(pinned) {
        Ok(()) => {
            let metadata = std::fs::metadata(pinned).expect("stat pinned sandbox-exec");
            assert_eq!(
                (metadata.uid(), metadata.gid()),
                (0, 0),
                "the pinned sandbox-exec must be root:wheel on the CI host"
            );
            assert_eq!(
                metadata.mode() & 0o022,
                0,
                "the pinned sandbox-exec must not be group/world-writable"
            );
            true
        }
        Err(SeatbeltProfileError::BinaryMissing(detail)) => {
            assert!(
                !pinned.exists(),
                "sandbox-exec exists yet verification reports BinaryMissing: {detail}"
            );
            eprintln!(
                "P18 s18: {PINNED_SANDBOX_EXEC_PATH} is genuinely absent on this runner — \
                 asserting the fail-closed BinaryMissing verdict (no fake success)"
            );
            assert!(matches!(
                verify_pinned_sandbox_exec(pinned),
                Err(SeatbeltProfileError::BinaryMissing(_))
            ));
            false
        }
        Err(other) => panic!("pinned sandbox-exec present but verification failed: {other:?}"),
    }
}

/// Copy a real system executable into the temp base and return its
/// CANONICAL absolute path: macOS resolves `/var` → `/private/var`, and
/// the literal the profile pins must be the very string sandbox-exec
/// execs. The copy keeps the executable bit — a genuine binary, not a
/// mock, so dyld really has to resolve libSystem for it to run.
fn harness_copy(source: &str, name: &str, base: &Path) -> PathBuf {
    let source_path = Path::new(source);
    assert!(source_path.exists(), "the CI host must provide {source}");
    let copy = base.join(name);
    std::fs::copy(source_path, &copy)
        .unwrap_or_else(|error| panic!("copy {source} into the temp harness path: {error}"));
    let mut permissions = std::fs::metadata(&copy)
        .unwrap_or_else(|error| panic!("stat the harness copy: {error}"))
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&copy, permissions)
        .unwrap_or_else(|error| panic!("chmod the harness copy: {error}"));
    std::fs::canonicalize(&copy)
        .unwrap_or_else(|error| panic!("canonicalize the harness copy: {error}"))
}

/// The real single-process Harness fixture: a `/bin/sh` copy as the
/// literal, plus the production backend's profile over the verified
/// pinned binary. None when this runner honestly cannot launch.
fn sh_harness(base: &Path) -> Option<(String, PathBuf)> {
    if !verified_pinned_sandbox_exec() {
        return None;
    }
    let harness = harness_copy("/bin/sh", "p18-harness-sh", base);
    let profile = MacosSeatbeltBackend::new()
        .build_profile(&harness.to_string_lossy())
        .expect("the backend must build the profile over the verified pinned binary");
    Some((profile, harness))
}

/// Launch `/usr/bin/sandbox-exec -p <profile> <harness> [args]` for real,
/// capture stdout/stderr, and enforce the deadline by killing the whole
/// process group (defensive: sandbox-exec replaces itself rather than
/// forking, but reaping must still be guaranteed).
fn launch_under_seatbelt(
    profile: &str,
    harness: &Path,
    arguments: &[&str],
    environment: &[(&str, &str)],
) -> (std::process::ExitStatus, String, String) {
    let mut command = Command::new(PINNED_SANDBOX_EXEC_PATH);
    command.arg("-p").arg(profile).arg(harness).args(arguments);
    for (key, value) in environment {
        command.env(key, value);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = command
        .spawn()
        .unwrap_or_else(|error| panic!("spawn sandbox-exec for {}: {error}", harness.display()));
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => panic!("try_wait the sandbox-exec child: {error}"),
        }
        if start.elapsed() >= LAUNCH_DEADLINE {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.wait();
            panic!(
                "sandbox-exec launch of {} exceeded the {LAUNCH_DEADLINE:?} deadline",
                harness.display()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_string(&mut stdout);
    }
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    (status, stdout, stderr)
}

/// A read target under `/Users` the UNSANDBOXED test process creates
/// (the `/Users/Shared` world-writable directory); removed on drop so the
/// marker never outlives the test, even on assertion panic.
struct UsersSharedMarker(PathBuf);

impl Drop for UsersSharedMarker {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Create the `/Users` read target, or None when this runner refuses the
/// write — recorded honestly by the caller, never worked around.
fn users_shared_marker() -> Option<(UsersSharedMarker, String)> {
    let directory =
        PathBuf::from("/Users/Shared").join(format!("r-code-p18-qa-{}", std::process::id()));
    std::fs::create_dir_all(&directory).ok()?;
    let marker = directory.join("marker.txt");
    std::fs::write(&marker, "p18-users-marker").ok()?;
    let text = marker.to_string_lossy().into_owned();
    Some((UsersSharedMarker(directory), text))
}

// The e2e heart (P18.1/P18.2) ---------------------------------------------

/// The single-process Harness lifecycle: the pinned sandbox-exec launches
/// the literal Harness (a REAL dynamically-linked `/bin/sh` copy — the
/// process never starts unless dyld resolves libSystem through the frozen
/// read roots), the Harness runs to completion, and an explicit read
/// inside a frozen root succeeds.
#[test]
fn real_harness_lifecycle_completes_with_dyld_and_runtime_reads() {
    let base = tempfile::tempdir().expect("tempdir");
    let Some((profile, harness)) = sh_harness(base.path()) else {
        return;
    };
    let script = "echo LIFECYCLE-OK; \
                  if read -r line < /private/etc/hosts; then echo ETC-READ-OK; else echo ETC-READ-DENIED; fi";
    let (status, stdout, stderr) = launch_under_seatbelt(&profile, &harness, &["-c", script], &[]);
    assert!(
        status.success(),
        "the literal Harness must run to completion under the profile \
         (status {status}, stdout {stdout:?}, stderr {stderr:?})"
    );
    assert!(
        stdout.contains("LIFECYCLE-OK"),
        "the Harness wrote its completion marker: {stdout:?}"
    );
    assert!(
        stdout.contains("ETC-READ-OK"),
        "reads inside the frozen runtime roots must succeed: {stdout:?}"
    );
}

/// Fork never exists for the workload (P18.1): the subshell `(true)`
/// requires a fork, and the denial must be an errno the Harness SURVIVES
/// (denial, not a kill signal — single-process semantics), so the parent
/// shell reaches the DENIED branch and prints the marker itself.
#[test]
fn fork_is_denied_and_the_harness_survives_the_denial() {
    let base = tempfile::tempdir().expect("tempdir");
    let Some((profile, harness)) = sh_harness(base.path()) else {
        return;
    };
    let script = "if (true); then echo FORK-OK; else echo FORK-DENIED; fi";
    let (status, stdout, stderr) = launch_under_seatbelt(&profile, &harness, &["-c", script], &[]);
    assert!(
        stdout.contains("FORK-DENIED") && !stdout.contains("FORK-OK"),
        "the subshell fork must be denied while the parent lives \
         (status {status}, stdout {stdout:?}, stderr {stderr:?})"
    );
}

/// Exec discipline (P18.1): `exec /bin/sh` — any non-literal binary — is
/// denied (a denied exec makes the non-interactive shell exit non-zero
/// without the escaped marker), while `exec '<harness literal>'` (the
/// initial-replacement allowance pointed at the Harness's own canonical
/// path) still succeeds: the literal is the ONE exec allowance.
#[test]
fn non_approved_exec_denied_but_literal_self_replacement_succeeds() {
    let base = tempfile::tempdir().expect("tempdir");
    let Some((profile, harness)) = sh_harness(base.path()) else {
        return;
    };
    let harness_text = harness.to_string_lossy().into_owned();

    let escape = "exec /bin/sh -c 'echo ESCAPED-OK'";
    let (status, stdout, stderr) = launch_under_seatbelt(&profile, &harness, &["-c", escape], &[]);
    assert!(
        !status.success() && !stdout.contains("ESCAPED-OK"),
        "exec of a non-literal binary must fail (status {status}, stdout {stdout:?}, \
         stderr {stderr:?})"
    );

    let self_replacement = format!("exec '{harness_text}' -c 'echo SELF-REPLACEMENT-OK'");
    let (status, stdout, stderr) =
        launch_under_seatbelt(&profile, &harness, &["-c", &self_replacement], &[]);
    assert!(
        status.success() && stdout.contains("SELF-REPLACEMENT-OK"),
        "the literal Harness path is the one allowed exec replacement \
         (status {status}, stdout {stdout:?}, stderr {stderr:?})"
    );
}

/// The frozen denial classes (P18.2): file writes fail through an
/// in-process shell redirection (no fork needed), a `/Users` read fails
/// against a marker the unsandboxed test created while the same launch
/// proves frozen-root reads succeed, and an outbound connect to a
/// listener this test owns is denied — an open sandbox would have the
/// curl copy connect and fetch the marker.
#[test]
fn write_network_and_user_home_reads_are_denied() {
    let base = tempfile::tempdir().expect("tempdir");
    let Some((profile, harness)) = sh_harness(base.path()) else {
        return;
    };
    let leak = base.path().join("leak.txt");

    let script = format!(
        "if echo p18 > '{leak}'; then echo WRITE-OK; else echo WRITE-DENIED; fi; \
         if read -r line < /private/etc/hosts; then echo ETC-READ-OK; else echo ETC-READ-DENIED; fi",
    );
    let (status, stdout, stderr) = launch_under_seatbelt(&profile, &harness, &["-c", &script], &[]);
    assert!(
        stdout.contains("WRITE-DENIED") && !stdout.contains("WRITE-OK"),
        "file writes must be denied (status {status}, stdout {stdout:?}, stderr {stderr:?})"
    );
    assert!(!leak.exists(), "no write may leak outside the profile");
    assert!(
        stdout.contains("ETC-READ-OK"),
        "the positive read control must succeed: {stdout:?}"
    );

    if let Some((_guard, marker)) = users_shared_marker() {
        let script = format!(
            "if read -r line < '{marker}'; then echo USERS-READ-OK; else echo USERS-READ-DENIED; fi"
        );
        let (status, stdout, stderr) =
            launch_under_seatbelt(&profile, &harness, &["-c", &script], &[]);
        assert!(
            stdout.contains("USERS-READ-DENIED") && !stdout.contains("USERS-READ-OK"),
            "reads under /Users must be denied (status {status}, stdout {stdout:?}, \
             stderr {stderr:?})"
        );
    } else {
        eprintln!(
            "P18 s18: /Users/Shared is not writable on this runner — the /Users read \
             denial stays carried by the profile's explicit deny line"
        );
    }

    if !Path::new("/usr/bin/curl").exists() {
        eprintln!(
            "P18 s18: /usr/bin/curl is genuinely absent on this runner — the network \
             denial is not provable here (recorded, not faked)"
        );
        return;
    }
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind the network probe listener");
    let address = listener.local_addr().expect("listener address");
    let curl = harness_copy("/usr/bin/curl", "p18-harness-curl", base.path());
    let profile = MacosSeatbeltBackend::new()
        .build_profile(&curl.to_string_lossy())
        .expect("the curl Harness profile builds over the verified pinned binary");
    let url = format!("http://{address}/");
    let home = base.path().to_string_lossy().into_owned();
    let (status, stdout, stderr) = launch_under_seatbelt(
        &profile,
        &curl,
        &[
            "--silent",
            "--show-error",
            "--noproxy",
            "*",
            "--max-time",
            "5",
            &url,
        ],
        &[("HOME", home.as_str())],
    );
    listener
        .set_nonblocking(true)
        .expect("switch the probe listener to nonblocking");
    let saw_connection = listener.accept().is_ok();
    assert!(
        !status.success() && !saw_connection,
        "the outbound connect must be denied with no connection reaching the listener \
         (status {status}, connected {saw_connection}, stdout {stdout:?}, stderr {stderr:?})"
    );
}

// P18.3 — the policy identity ----------------------------------------------

/// The report-digest material binds the harness path, the harness sha256
/// and the literal profile text (any change is a different identity), and
/// the profile text itself pins the frozen policy shape: exactly ONE
/// process-exec allowance carrying the literal verbatim, an explicit
/// fork denial, frozen runtime read allowances, and NO /Users or network
/// or write allowance anywhere.
#[test]
fn policy_identity_binds_harness_and_profile_text() {
    let roots: Vec<String> = BASELINE_RUNTIME_READ_ROOTS
        .iter()
        .map(|root| (*root).to_string())
        .collect();
    let harness = "/opt/content-addressed/harness-a";
    let profile = build_no_workspace_single_process_profile(harness, &roots)
        .expect("the pure profile builder needs no host binary");

    assert!(profile.starts_with("(version 1)\n(deny default)\n"));
    assert_eq!(
        profile.matches("(allow process-exec").count(),
        1,
        "exactly ONE process-exec allowance"
    );
    assert!(profile.contains(&format!("(allow process-exec (literal \"{harness}\"))\n")));
    assert!(
        profile.contains("(deny process-fork)\n"),
        "the fork denial stays explicit in the policy text"
    );
    assert!(
        profile.contains("(deny file-read* (subpath \"/Users\"))\n"),
        "the /Users read denial stays explicit"
    );
    assert!(!profile.contains("(allow network"), "no network allowance");
    assert!(!profile.contains("(allow file-write"), "no write allowance");
    for root in &roots {
        assert!(profile.contains(&format!("(allow file-read* (subpath \"{root}\"))\n")));
    }
    for line in profile.lines() {
        if line.starts_with("(allow") {
            assert!(
                !line.contains("/Users") && !line.contains(".git"),
                "no allow rule may ever name /Users or .git: {line}"
            );
        }
    }

    // P18.3: the material digest changes with ANY identity input.
    let identity = SafetyBinaryIdentity {
        path: harness.to_string(),
        sha256: "aa".repeat(32),
    };
    let material = seatbelt_policy_material(
        SeatbeltProfileClass::NoWorkspaceSingleProcess,
        &profile,
        &identity,
    );
    assert_eq!(material["backend"], serde_json::json!("macos-seatbelt"));
    assert_eq!(
        material["class"],
        serde_json::json!("no-workspace-single-process")
    );
    assert_eq!(material["harnessPath"], serde_json::json!(harness));
    assert_eq!(
        material["harnessSha256"],
        serde_json::json!(identity.sha256)
    );
    assert_eq!(material["profile"], serde_json::json!(profile));

    let digest = r_code_harness_protocol::canonical_input_hash(&material);
    let rebuilt = r_code_harness_protocol::canonical_input_hash(&seatbelt_policy_material(
        SeatbeltProfileClass::NoWorkspaceSingleProcess,
        &profile,
        &identity,
    ));
    assert_eq!(digest, rebuilt, "the policy digest is deterministic");
    let changed_path = seatbelt_policy_material(
        SeatbeltProfileClass::NoWorkspaceSingleProcess,
        &profile,
        &SafetyBinaryIdentity {
            path: format!("{harness}-b"),
            sha256: identity.sha256.clone(),
        },
    );
    assert_ne!(
        digest,
        r_code_harness_protocol::canonical_input_hash(&changed_path),
        "a changed harness path must change the policy identity"
    );
    let changed_sha = seatbelt_policy_material(
        SeatbeltProfileClass::NoWorkspaceSingleProcess,
        &profile,
        &SafetyBinaryIdentity {
            path: identity.path.clone(),
            sha256: "bb".repeat(32),
        },
    );
    assert_ne!(
        digest,
        r_code_harness_protocol::canonical_input_hash(&changed_sha),
        "a changed harness sha256 must change the policy identity"
    );
    let other_profile = build_no_workspace_single_process_profile(&format!("{harness}-b"), &roots)
        .expect("the second profile builds");
    let changed_profile = seatbelt_policy_material(
        SeatbeltProfileClass::NoWorkspaceSingleProcess,
        &other_profile,
        &identity,
    );
    assert_ne!(
        digest,
        r_code_harness_protocol::canonical_input_hash(&changed_profile),
        "a changed profile text must change the policy identity"
    );
}

// Fail-closed input validation ---------------------------------------------

/// Relative, untrimmed and quote-bearing harness paths plus dirty runtime
/// roots (`..`, `/Users`, `.git`, relative, quotes) are refused outright,
/// and a write-capable profile NEVER gets a launchable policy digest —
/// the refusal IS the SafeDisabled reason recorded in report material.
#[test]
fn dirty_inputs_refused_and_write_profiles_stay_safe_disabled() {
    let clean_roots: Vec<String> = vec!["/usr/lib".into(), "/System/Library".into()];
    for dirty_harness in [
        "tmp/harness",
        " /tmp/harness",
        "/tmp/harness ",
        "",
        "/tmp/h\"arness",
        "/tmp/ha\\rmess",
    ] {
        assert!(
            matches!(
                build_no_workspace_single_process_profile(dirty_harness, &clean_roots),
                Err(SeatbeltProfileError::InvalidProfile(_))
            ),
            "refused harness path: {dirty_harness:?}"
        );
    }
    for dirty_root in [
        "/usr/lib/../Users",
        "/Users/shared",
        "/repo/.git",
        "usr/lib",
        "/usr/li\"b",
    ] {
        let roots = vec![dirty_root.to_string()];
        assert!(
            matches!(
                build_no_workspace_single_process_profile("/tmp/harness", &roots),
                Err(SeatbeltProfileError::InvalidProfile(_))
            ),
            "refused runtime read root: {dirty_root}"
        );
    }
    assert!(build_no_workspace_single_process_profile("/tmp/harness", &clean_roots).is_ok());

    let backend = MacosSeatbeltBackend::new();
    let write_profile = SandboxProfileMaterial {
        read_roots: vec!["/usr/lib".into()],
        write_roots: vec!["/tmp/p18-w".into()],
        scratch_root: "/tmp/p18-s".into(),
        toolchain_roots: vec!["/usr/bin/harness".into()],
        cache_roots: vec!["/tmp/p18-c".into()],
        git_hidden: true,
        inherited_fds: vec![0, 1, 2],
        environment_allowlist: vec!["PATH".into()],
        network: SandboxNetworkClass::Offline,
    };
    let refusal = backend
        .policy_digest(&write_profile)
        .expect_err("write execution must stay safe-disabled");
    assert!(
        refusal.contains("safe-disabled"),
        "the refusal IS the SafeDisabled reason: {refusal}"
    );

    // A read-only profile with NO harness path is refused too —
    // fail-closed before any binary trust.
    let no_harness = SandboxProfileMaterial {
        write_roots: vec![],
        toolchain_roots: vec![],
        ..write_profile.clone()
    };
    assert!(
        backend.policy_digest(&no_harness).is_err(),
        "an empty harness path can never yield a launchable digest"
    );
}

// Non-activation pins ------------------------------------------------------

/// The backend is registered diagnostics only: `run_probes` is
/// permanently Err (P20 owns real probe execution), the digest path is
/// real but deterministic, and the source pins prove the P18 module
/// never touches the P13 activation gate while the wave's gate material
/// stays none-this-wave.
#[tokio::test]
async fn backend_is_permanently_non_activating_until_p20() {
    let backend = MacosSeatbeltBackend::new();
    assert_eq!(backend.id(), "macos-seatbelt");

    let profile = SandboxProfileMaterial {
        read_roots: vec!["/usr/lib".into()],
        write_roots: vec![],
        scratch_root: "/tmp/p18-s".into(),
        toolchain_roots: vec!["/usr/bin/harness".into()],
        cache_roots: vec!["/tmp/p18-c".into()],
        git_hidden: true,
        inherited_fds: vec![0, 1, 2],
        environment_allowlist: vec!["PATH".into()],
        network: SandboxNetworkClass::Offline,
    };
    let helper = SafetyBinaryIdentity {
        path: "/usr/bin/r-code-safety-probe".into(),
        sha256: "0".repeat(64),
    };
    let refusal = backend
        .run_probes(&helper, &profile, &[SandboxProbeId::WriteOutsideAllowlist])
        .await;
    assert!(
        matches!(&refusal, Err(message) if message.contains("non-activating")
            && message.contains("P20")),
        "run_probes must refuse with the P20-pending message: {refusal:?}"
    );

    // The digest path is real (over the pinned binary, when present).
    if verified_pinned_sandbox_exec() {
        let first = backend
            .policy_digest(&profile)
            .expect("the digest over the real read-only profile");
        assert_eq!(
            first,
            backend
                .policy_digest(&profile)
                .expect("the digest rebuilds"),
            "the digest is deterministic"
        );
        let mut other_harness = profile.clone();
        other_harness.toolchain_roots = vec!["/usr/bin/harness-b".into()];
        assert_ne!(
            first,
            backend
                .policy_digest(&other_harness)
                .expect("the changed-harness digest"),
            "a changed harness path must change the policy digest"
        );
    }

    let macos_module = include_str!("../src/services/sandbox/macos.rs");
    assert!(
        !macos_module.contains("platform_activation_gate"),
        "the Seatbelt module must never call the P13 activation gate"
    );
    assert!(
        !macos_module.contains("current_platform_material"),
        "the Seatbelt module must never touch the current-platform material"
    );
    assert!(
        !macos_module.contains("SafetyStatus::Activated"),
        "the Seatbelt module has no path to an Activated verdict"
    );
    assert!(
        macos_module.contains("P20 pending"),
        "the permanent run_probes refusal stays pinned in source"
    );
    assert!(macos_module.contains("#![cfg(target_os = \"macos\")]"));

    let sandbox_module = include_str!("../src/services/sandbox.rs");
    assert!(
        sandbox_module.contains("\"none-this-wave\""),
        "the wave's gate material stays none-this-wave"
    );
    assert!(sandbox_module.contains("no native sandbox backend is available in this wave"));
    assert!(
        !sandbox_module.contains("MacosSeatbeltBackend"),
        "the backend is a registered module, never wired into the gate"
    );
}
