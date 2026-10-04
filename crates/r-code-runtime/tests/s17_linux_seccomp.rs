//! P17 QA — the compiled seccomp policy proven by REAL filtered
//! sandboxes. Native Linux CI owns execution; on the Windows dev host
//! this file is parse-only (empty suite). Nothing is mocked: the policy
//! is compiled with the production compiler, serialized through the
//! production memfd transport, attached to a REAL P16 plan over the
//! verified pinned bwrap, and handed to bwrap via the documented
//! pre_exec dup2 seam — then workloads genuinely ATTEMPT the denied
//! syscalls inside. A wrong wire format would silently not apply, so
//! the escape probes are the end-to-end format proof: every attempt
//! must die with the ENOSYS shape while the identical unfiltered
//! control launch never shows that shape. Ordinary runtimes (sh,
//! multithreaded python, cargo/node when present) must keep working —
//! clone without namespace bits and glibc's clone3-ENOSYS fallback
//! both stay live. The seccomp fd must not survive into the workload.
//! Environmental gaps (no bwrap, no python3, runner refuses the
//! rootless user namespace) are recorded honestly, never faked.

#![cfg(target_os = "linux")]

use r_code_runtime::services::sandbox::linux::{
    attach_seccomp_to_plan, build_bwrap_launch_plan, compile_namespace_policy_filter,
    verify_pinned_bwrap, write_filter_to_memfd, BwrapLaunchPlan, BwrapPlanError,
    LinuxBwrapPlanBackend, SeccompPolicyError, MINIMUM_BWRAP_VERSION, PINNED_BWRAP_PATH,
};
use r_code_runtime::services::sandbox::{
    SafetyBinaryIdentity, SandboxBackend, SandboxNetworkClass, SandboxProbeId,
    SandboxProfileMaterial,
};
use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::io::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Wall clock each real launch gets before the process group is killed.
const LAUNCH_DEADLINE: Duration = Duration::from_secs(45);
/// The child fd the launcher deliberately dup2s the memfd onto — the
/// plan's `--seccomp` argument names this number (bwrap consumes and
/// closes it before exec'ing the workload).
const SECCOMP_CHILD_FD: RawFd = 4;
/// glibc's strerror text for ENOSYS — the observable shape of a denied
/// syscall inside the sandbox.
const ENOSYS_TEXT: &str = "Function not implemented";

fn pinned_bwrap() -> PathBuf {
    PathBuf::from(PINNED_BWRAP_PATH)
}

fn make_dir(path: &Path) {
    std::fs::create_dir_all(path).unwrap_or_else(|error| panic!("mkdir {path:?}: {error}"));
}

/// A profile whose workspace roots are fresh tempdirs; the toolchain
/// roots (ro-bound /usr plus loader dirs) let a real shell/python run
/// inside the sandbox.
fn tempdir_profile(base: &Path, toolchain_roots: Vec<String>) -> SandboxProfileMaterial {
    for root in ["read", "write", "scratch", "cache"] {
        make_dir(&base.join(root));
    }
    SandboxProfileMaterial {
        read_roots: vec![base.join("read").to_string_lossy().into_owned()],
        write_roots: vec![base.join("write").to_string_lossy().into_owned()],
        scratch_root: base.join("scratch").to_string_lossy().into_owned(),
        toolchain_roots,
        cache_roots: vec![base.join("cache").to_string_lossy().into_owned()],
        git_hidden: true,
        inherited_fds: vec![0, 1, 2],
        environment_allowlist: vec!["PATH".into(), "RC17_E2E_TOKEN".into()],
        network: SandboxNetworkClass::Offline,
    }
}

/// Toolchain roots every launch uses: /usr plus the loader dirs that
/// exist on this host.
fn host_toolchain_roots() -> Vec<String> {
    let mut roots = vec!["/usr".to_string()];
    for candidate in ["/lib", "/lib64"] {
        if Path::new(candidate).exists() {
            roots.push(candidate.to_string());
        }
    }
    roots
}

fn launch_values() -> BTreeMap<&'static str, String> {
    BTreeMap::from([
        ("PATH", "/usr/bin:/bin".to_string()),
        ("RC17_E2E_TOKEN", "p17-e2e-token".to_string()),
    ])
}

/// The pinned binary must be verifiable, or the host honestly lacks it.
/// Mirrors the P16 precedent: genuine absence returns None after
/// asserting the fail-closed BinaryMissing shape.
fn verified_pinned_binary() -> Option<(u32, u32)> {
    let pinned = pinned_bwrap();
    match verify_pinned_bwrap(&pinned) {
        Ok(version) => {
            assert!(
                version >= MINIMUM_BWRAP_VERSION,
                "seccomp e2e needs a usable pinned bwrap (found {version:?})"
            );
            Some(version)
        }
        Err(BwrapPlanError::BinaryMissing(detail)) => {
            assert!(
                !pinned.exists(),
                "bwrap exists yet verification reports BinaryMissing: {detail}"
            );
            eprintln!(
                "P17 s17: /usr/bin/bwrap is genuinely absent on this runner — \
                 no filtered sandbox can launch here; asserting the fail-closed \
                 BinaryMissing verdict (no fake success)"
            );
            assert!(matches!(
                verify_pinned_bwrap(&pinned),
                Err(BwrapPlanError::BinaryMissing(_))
            ));
            None
        }
        Err(other) => panic!("pinned bwrap present but verification failed: {other:?}"),
    }
}

// Launch plumbing ----------------------------------------------------------------

/// Compose the real spawn argv: start from the P16 plan, attach the
/// seccomp fd through the PRODUCTION attach API when requested, fill
/// the `--setenv` value slots, add a fresh `/proc` (fd and namespace
/// introspection inside), and append the workload.
fn compose_argv(
    plan: &BwrapLaunchPlan,
    attach_fd: Option<RawFd>,
    values: &BTreeMap<&str, String>,
    workload: &[&str],
) -> Vec<String> {
    let effective = match attach_fd {
        Some(fd) => attach_seccomp_to_plan(plan, fd).expect("attach seccomp fd"),
        None => plan.clone(),
    };
    let mut argv = effective.argv.clone();
    argv.pop(); // "<workload-argv-follows>" placeholder
    let mut index = 0;
    while index + 2 < argv.len() {
        if argv[index] == "--setenv" && argv[index + 2].is_empty() {
            let key = argv[index + 1].clone();
            argv[index + 2] = values
                .get(key.as_str())
                .unwrap_or_else(|| panic!("no launcher value for allowlisted key {key}"))
                .clone();
            index += 3;
        } else {
            index += 1;
        }
    }
    let terminator = argv
        .iter()
        .rposition(|argument| argument == "--")
        .expect("plan argv keeps the -- terminator");
    argv.insert(terminator, "--proc".into());
    argv.insert(terminator + 1, "/proc".into());
    argv.extend(workload.iter().map(|argument| argument.to_string()));
    argv
}

#[derive(Debug)]
struct LaunchOutcome {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

/// True when stderr shows the RUNNER refusing the rootless user
/// namespace (AppArmor/seccomp/kernel.disable_userns style denials) —
/// an environment refusal, not a plan or filter defect.
fn is_environment_namespace_refusal(stderr: &str) -> bool {
    let lowered = stderr.to_ascii_lowercase();
    [
        "user namespace",
        "uid map",
        "gid map",
        "creating new namespace",
    ]
    .iter()
    .any(|needle| lowered.contains(*needle))
}

/// Spawn the argv in its own process group, optionally handing the
/// seccomp memfd to the child at `SECCOMP_CHILD_FD` via the documented
/// pre_exec dup2 seam (the fd is MFD_CLOEXEC — the dup2 is the only
/// way it crosses exec). The parent closes its copy right after spawn.
fn spawn_launch(argv: &[String], memfd: Option<RawFd>, pipe_stdout: bool) -> std::process::Child {
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]);
    command.env_clear();
    command.stdin(Stdio::null());
    command.stdout(if pipe_stdout {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    command.stderr(Stdio::piped());
    command.process_group(0);
    if let Some(fd) = memfd {
        unsafe {
            command.pre_exec(move || {
                // dup2 clears FD_CLOEXEC on the destination: the one
                // deliberate hand-off of the filter to bwrap.
                if libc::dup2(fd, SECCOMP_CHILD_FD) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let child = command
        .spawn()
        .unwrap_or_else(|error| panic!("spawn {argv:?}: {error}"));
    if let Some(fd) = memfd {
        // SAFETY: closing the parent's copy of an fd we own; the child
        // already holds its own dup at SECCOMP_CHILD_FD.
        unsafe { libc::close(fd) };
    }
    child
}

/// Run one launch to completion under the deadline, then reap the whole
/// process group so no orphan survives the test. Panics on timeout — a
/// hung sandbox is a defect, not an environment gap.
fn run_launch(argv: &[String], memfd: Option<RawFd>, pipe_stdout: bool) -> LaunchOutcome {
    let mut child = spawn_launch(argv, memfd, pipe_stdout);
    let mut child_stdout = child.stdout.take();
    let mut child_stderr = child.stderr.take();
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => panic!("try_wait bwrap child: {error}"),
        }
        if start.elapsed() >= LAUNCH_DEADLINE {
            kill_tree_and_reap(&mut child);
            panic!("launch exceeded the {LAUNCH_DEADLINE:?} deadline: {argv:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut stdout = String::new();
    let mut stderr = String::new();
    if let Some(pipe) = child_stdout.as_mut() {
        let _ = pipe.read_to_string(&mut stdout);
    }
    if let Some(pipe) = child_stderr.as_mut() {
        let _ = pipe.read_to_string(&mut stderr);
    }
    kill_tree_and_reap(&mut child);
    LaunchOutcome {
        status,
        stdout,
        stderr,
    }
}

/// Kill the whole process group and reap the leader.
fn kill_tree_and_reap(child: &mut std::process::Child) {
    let pgid = child.id() as libc::c_int;
    // SAFETY: negative-pid kill addresses the child's process group.
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
    let _ = child.wait();
}

/// A compiled filter on a fresh memfd — the transport every filtered
/// launch consumes.
fn fresh_filter_fd() -> RawFd {
    let program = compile_namespace_policy_filter()
        .unwrap_or_else(|error| panic!("policy must compile on the CI arch: {error:?}"));
    write_filter_to_memfd(&program).expect("memfd transport")
}

/// Run a launch; when the RUNNER (not the filter) refuses the rootless
/// namespace, record it honestly and return None so the caller stops
/// attributing outcomes it cannot observe.
fn launch_or_env_record(
    argv: &[String],
    memfd: Option<RawFd>,
    pipe_stdout: bool,
    label: &str,
) -> Option<LaunchOutcome> {
    let outcome = run_launch(argv, memfd, pipe_stdout);
    if !outcome.status.success() && is_environment_namespace_refusal(&outcome.stderr) {
        eprintln!(
            "P17 s17: {label} refused by the runner environment (no usable rootless \
             user namespace) — fail-closed record, nothing executed:\n{}",
            outcome.stderr
        );
        return None;
    }
    Some(outcome)
}

// P17.2 — escape attempts die with the ENOSYS shape (the wire-format proof) -------

/// One shell-level escape probe: the CONTROL launch (no filter) runs
/// the same argv and must never show the ENOSYS shape (it may succeed,
/// or fail for privilege reasons — both honest controls); the FILTERED
/// launch must fail with exactly that shape.
fn escape_probe_binary(
    plan: &BwrapLaunchPlan,
    values: &BTreeMap<&str, String>,
    label: &str,
    workload: &[&str],
) {
    let control_argv = compose_argv(plan, None, values, workload);
    let Some(control) = launch_or_env_record(&control_argv, None, false, label) else {
        return;
    };
    if control.status.success() {
        eprintln!("P17 s17: {label} control SUCCEEDED unfiltered (strongest form)");
    } else {
        assert!(
            !control.stderr.contains(ENOSYS_TEXT),
            "{label} control must never fail ENOSYS-shaped: {control:?}"
        );
        eprintln!(
            "P17 s17: {label} control failed for privilege reasons (expected for \
             mount/setns as an unprivileged uid): {}",
            control.stderr.trim()
        );
    }

    let filtered_argv = compose_argv(plan, Some(SECCOMP_CHILD_FD), values, workload);
    let Some(filtered) = launch_or_env_record(
        &filtered_argv,
        Some(fresh_filter_fd()),
        false,
        &format!("{label} (filtered)"),
    ) else {
        return;
    };
    assert!(
        !filtered.status.success(),
        "{label} MUST FAIL inside the filtered sandbox — a silent pass means the \
         filter is not applied (wire-format break): {filtered:?}"
    );
    assert!(
        filtered.stderr.contains(ENOSYS_TEXT),
        "{label} must fail with the ENOSYS shape ({ENOSYS_TEXT}); got: {filtered:?}"
    );
}

/// The raw-syscall python driver: one filtered launch attempts setns,
/// add_key, keyctl and clone with SINGLE namespace bits (the per-bit
/// trap an all-bits-at-once mask would silently miss), printing one
/// `name ret=<r> errno=<e>` line per attempt on stdout.
fn raw_syscall_python() -> String {
    format!(
        r#"
import ctypes, os
libc = ctypes.CDLL(None, use_errno=True)
libc.syscall.restype = ctypes.c_long
def attempt(name, nr, *args):
    ctypes.set_errno(0)
    ret = libc.syscall(ctypes.c_long(nr), *args)
    if name.startswith("clone") and ret == 0:
        os._exit(0)
    print("%s ret=%d errno=%d" % (name, ret, ctypes.get_errno()))
fd = os.open("/proc/self/ns/net", os.O_RDONLY)
attempt("setns_self_net", {setns_nr}, ctypes.c_int(fd), ctypes.c_int(0))
os.close(fd)
attempt("add_key", {add_key_nr}, ctypes.c_char_p(b"user"), ctypes.c_char_p(b"k"),
        ctypes.c_char_p(b"v"), ctypes.c_int(1), ctypes.c_int(-2))
attempt("keyctl_get_keyring_id", {keyctl_nr}, ctypes.c_int(0), ctypes.c_int(-2),
        ctypes.c_int(0))
for name, bit in [("clone_newuser", 0x10000000), ("clone_newnet", 0x40000000),
                  ("clone_newns", 0x00020000), ("clone_newpid", 0x20000000),
                  ("clone_newuts", 0x04000000), ("clone_newipc", 0x08000000),
                  ("clone_all_bits", 0x7E020000)]:
    attempt(name, {clone_nr}, ctypes.c_long(bit))
"#,
        setns_nr = libc::SYS_setns,
        add_key_nr = libc::SYS_add_key,
        keyctl_nr = libc::SYS_keyctl,
        clone_nr = libc::SYS_clone,
    )
    .trim()
    .to_string()
}

/// Parse `name ret=<r> errno=<e>` lines into a name -> (ret, errno) map.
fn parse_probe_lines(stdout: &str) -> BTreeMap<String, (i64, i64)> {
    let mut parsed = BTreeMap::new();
    for line in stdout.lines() {
        let mut parts = line.trim().split_whitespace();
        let (Some(name), Some(ret), Some(errno)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if let (Ok(ret), Ok(errno)) = (
            ret.trim_start_matches("ret=").parse::<i64>(),
            errno.trim_start_matches("errno=").parse::<i64>(),
        ) {
            parsed.insert(name.to_string(), (ret, errno));
        }
    }
    parsed
}

const RAW_PROBES: [&str; 10] = [
    "setns_self_net",
    "add_key",
    "keyctl_get_keyring_id",
    "clone_newuser",
    "clone_newnet",
    "clone_newns",
    "clone_newpid",
    "clone_newuts",
    "clone_newipc",
    "clone_all_bits",
];

#[test]
fn escape_attempts_fail_enosys_inside_the_filtered_sandbox() {
    if verified_pinned_binary().is_none() {
        return;
    }
    let base = tempfile::tempdir().expect("tempdir");
    make_dir(&base.path().join("scratch/mnt"));
    let profile = tempdir_profile(base.path(), host_toolchain_roots());
    let plan = build_bwrap_launch_plan(&profile, &pinned_bwrap(), "boot-p17-e2e")
        .expect("e2e plan builds over the verified pinned binary");
    let values = launch_values();
    let mount_point = base
        .path()
        .join("scratch/mnt")
        .to_string_lossy()
        .into_owned();

    // unshare(2) denials — user and network namespaces.
    escape_probe_binary(
        &plan,
        &values,
        "unshare -Ur true",
        &["/usr/bin/unshare", "-Ur", "/usr/bin/true"],
    );
    escape_probe_binary(
        &plan,
        &values,
        "unshare -n true",
        &["/usr/bin/unshare", "-n", "/usr/bin/true"],
    );
    // mount(2) denial (the control fails EPERM as an unprivileged uid;
    // the filtered run must fail ENOSYS instead — the differing errno
    // is the filter's fingerprint).
    escape_probe_binary(
        &plan,
        &values,
        "mount tmpfs",
        &[
            "/usr/bin/mount",
            "-t",
            "tmpfs",
            "none",
            mount_point.as_str(),
        ],
    );
    // setns(2) denial via util-linux nsenter against our own netns.
    if Path::new("/usr/bin/nsenter").exists() {
        escape_probe_binary(
            &plan,
            &values,
            "nsenter --net",
            &[
                "/usr/bin/nsenter",
                "--net=/proc/self/ns/net",
                "/usr/bin/true",
            ],
        );
    } else {
        eprintln!("P17 s17: /usr/bin/nsenter absent — setns still covered by the python probe");
    }

    // Raw-syscall probes (keyring + clone with SINGLE namespace bits —
    // the probe an all-bits-at-once mask silently passes).
    if !Path::new("/usr/bin/python3").exists() {
        eprintln!(
            "P17 s17: /usr/bin/python3 absent on this runner — the raw add_key/keyctl \
             and per-bit clone probes are recorded as NOT RUN (honest gap); the \
             binary probes above still prove the filter is applied end-to-end"
        );
        return;
    }
    let script = raw_syscall_python();
    let workload = ["/usr/bin/python3", "-c", script.as_str()];

    let control_argv = compose_argv(&plan, None, &values, &workload);
    let Some(control) = launch_or_env_record(&control_argv, None, true, "raw probes (control)")
    else {
        return;
    };
    assert!(
        control.status.success(),
        "raw-probe python must run unfiltered: {control:?}"
    );
    let control_errnos = parse_probe_lines(&control.stdout);

    let filtered_argv = compose_argv(&plan, Some(SECCOMP_CHILD_FD), &values, &workload);
    let Some(filtered) = launch_or_env_record(
        &filtered_argv,
        Some(fresh_filter_fd()),
        true,
        "raw probes (filtered)",
    ) else {
        return;
    };
    assert!(
        filtered.status.success(),
        "raw-probe python must exit cleanly inside the filtered sandbox (every \
         attempt is a syscall error, not a crash): {filtered:?}"
    );
    let filtered_errnos = parse_probe_lines(&filtered.stdout);
    for name in RAW_PROBES {
        let Some((ret, errno)) = filtered_errnos.get(name) else {
            panic!("filtered raw probe {name} missing from output: {filtered:?}");
        };
        assert_eq!(
            (*ret, *errno),
            (-1, libc::ENOSYS as i64),
            "filtered {name} must return -1 ENOSYS"
        );
    }
    // Control side: the same attempts may succeed or fail for privilege
    // reasons — but never with ENOSYS (the filter's fingerprint).
    for name in RAW_PROBES {
        let Some((_, errno)) = control_errnos.get(name) else {
            continue;
        };
        assert_ne!(
            *errno,
            libc::ENOSYS as i64,
            "control {name} must never be ENOSYS-shaped (the kernel has this syscall)"
        );
    }
}

// P17.3 — ordinary runtimes, thread creation and the clone3 fallback ----------------

#[test]
fn ordinary_runtimes_and_thread_creation_work_inside_the_filtered_sandbox() {
    if verified_pinned_binary().is_none() {
        return;
    }
    let base = tempfile::tempdir().expect("tempdir");
    let scratch_text = base.path().join("scratch").to_string_lossy().into_owned();
    let profile = tempdir_profile(base.path(), host_toolchain_roots());
    let plan =
        build_bwrap_launch_plan(&profile, &pinned_bwrap(), "boot-p17-run").expect("plan builds");
    let values = launch_values();

    // A plain shell round trip under the filter.
    let echo_file = format!("{scratch_text}/echo.txt");
    let echo_redirect = format!("echo ok > '{echo_file}'");
    let sh_workload = ["/usr/bin/sh", "-c", echo_redirect.as_str()];
    let argv = compose_argv(&plan, Some(SECCOMP_CHILD_FD), &values, &sh_workload);
    let Some(outcome) = launch_or_env_record(&argv, Some(fresh_filter_fd()), false, "sh echo")
    else {
        return;
    };
    assert!(
        outcome.status.success(),
        "sh -c echo must work inside the filtered sandbox: {outcome:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&echo_file)
            .expect("echo evidence")
            .trim(),
        "ok"
    );

    // Thread creation: python3's threading spawns pthreads — glibc >=
    // 2.34 tries clone3 FIRST (denied ENOSYS) and must fall back to the
    // legacy clone (allowed: no namespace bits). This single workload
    // proves both halves of the clone rule at once.
    const THREADS: i32 = 8;
    let thread_file = format!("{scratch_text}/threads.txt");
    let python_threading = format!(
        r#"
import threading
path = {thread_file:?}
lock = threading.Lock()
def work(i):
    with lock:
        with open(path, "a") as handle:
            handle.write("t%d\n" % i)
workers = [threading.Thread(target=work, args=(i,)) for i in range({THREADS})]
for worker in workers:
    worker.start()
for worker in workers:
    worker.join()
print("threads-ok")
"#
    );
    let thread_lines = || {
        std::fs::read_to_string(&thread_file)
            .expect("thread evidence")
            .lines()
            .count() as i32
    };
    if Path::new("/usr/bin/python3").exists() {
        let workload = ["/usr/bin/python3", "-c", python_threading.as_str()];
        let argv = compose_argv(&plan, Some(SECCOMP_CHILD_FD), &values, &workload);
        let Some(outcome) =
            launch_or_env_record(&argv, Some(fresh_filter_fd()), true, "python threading")
        else {
            return;
        };
        assert!(
            outcome.status.success(),
            "python threading must work under the filter (clone3 ENOSYS fallback + \
             clone-without-namespace-bits allowed): {outcome:?}"
        );
        assert!(outcome.stdout.contains("threads-ok"));
        assert_eq!(thread_lines(), THREADS, "all worker threads ran");
    } else {
        // Honest fallback: xargs -P fans out real subprocesses (clone
        // without namespace bits) when python3 is unavailable.
        eprintln!(
            "P17 s17: python3 absent — thread proof degrades to xargs -P subprocess \
             fan-out (the clone3 fallback stays unproven on this runner, \
             recorded honestly)"
        );
        let script = format!(
            "seq 1 {THREADS} | /usr/bin/xargs -P 4 -I{{}} sh -c 'echo t{{}} >> {thread_file}'"
        );
        let workload = ["/usr/bin/sh", "-c", script.as_str()];
        let argv = compose_argv(&plan, Some(SECCOMP_CHILD_FD), &values, &workload);
        let Some(outcome) =
            launch_or_env_record(&argv, Some(fresh_filter_fd()), false, "xargs -P fan-out")
        else {
            return;
        };
        assert!(
            outcome.status.success(),
            "xargs -P fan-out must work under the filter: {outcome:?}"
        );
        assert_eq!(thread_lines(), THREADS);
    }

    // Toolchain binaries of record (cargo/node) — opportunistic: when
    // present on the host they must run under the filter; the one hard
    // rule is that the filter is never the reason one dies.
    for tool in ["cargo", "node"] {
        let Some(real_path) = locate_host_binary(tool) else {
            eprintln!("P17 s17: {tool} not present on this host — recorded as not run");
            continue;
        };
        let parent = real_path.parent().expect("tool parent dir");
        let mut tool_profile = profile.clone();
        tool_profile
            .toolchain_roots
            .push(parent.to_string_lossy().into_owned());
        let tool_plan = build_bwrap_launch_plan(&tool_profile, &pinned_bwrap(), "boot-p17-tool")
            .expect("tool plan builds");
        let workload = [
            real_path.to_string_lossy().into_owned(),
            "--version".to_string(),
        ];
        let argv = compose_argv(
            &tool_plan,
            Some(SECCOMP_CHILD_FD),
            &values,
            &[workload[0].as_str(), workload[1].as_str()],
        );
        let Some(outcome) = launch_or_env_record(
            &argv,
            Some(fresh_filter_fd()),
            true,
            &format!("{tool} --version (filtered)"),
        ) else {
            continue;
        };
        if outcome.status.success() {
            eprintln!(
                "P17 s17: {tool} --version ran under the filter: {}",
                outcome.stdout.trim()
            );
        } else {
            assert!(
                !outcome.stderr.contains(ENOSYS_TEXT),
                "{tool} must never die ENOSYS-shaped under the filter: {outcome:?}"
            );
            eprintln!(
                "P17 s17: {tool} --version failed for non-filter reasons (recorded \
                 honestly — the filter is exonerated): {outcome:?}"
            );
        }
    }
}

/// Resolve a host tool to its canonical path (following rustup-style
/// proxies) or None when not installed.
fn locate_host_binary(name: &str) -> Option<PathBuf> {
    let output = Command::new("which")
        .arg(name)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        return None;
    }
    std::fs::canonicalize(text).ok()
}

// Acceptance — the workload cannot inherit the seccomp fd --------------------------

#[test]
fn seccomp_fd_is_not_inherited_by_the_workload() {
    if verified_pinned_binary().is_none() {
        return;
    }
    let base = tempfile::tempdir().expect("tempdir");
    let scratch_text = base.path().join("scratch").to_string_lossy().into_owned();
    let profile = tempdir_profile(base.path(), host_toolchain_roots());
    let plan =
        build_bwrap_launch_plan(&profile, &pinned_bwrap(), "boot-p17-fd").expect("plan builds");
    let values = launch_values();
    let names_file = format!("{scratch_text}/fd-names.txt");
    let links_file = format!("{scratch_text}/fd-links.txt");
    let script = format!(
        "ls /proc/self/fd > '{names_file}' 2>&1; ls -l /proc/self/fd > '{links_file}' 2>&1"
    );
    let workload = ["/usr/bin/sh", "-c", script.as_str()];

    let control_argv = compose_argv(&plan, None, &values, &workload);
    let Some(control) = launch_or_env_record(&control_argv, None, false, "fd listing (control)")
    else {
        return;
    };
    assert!(
        control.status.success(),
        "control fd listing must run: {control:?}"
    );

    let argv = compose_argv(&plan, Some(SECCOMP_CHILD_FD), &values, &workload);
    let Some(outcome) = launch_or_env_record(
        &argv,
        Some(fresh_filter_fd()),
        false,
        "fd listing (filtered)",
    ) else {
        return;
    };
    assert!(
        outcome.status.success(),
        "filtered fd listing must run: {outcome:?}"
    );
    let names = std::fs::read_to_string(&names_file).expect("fd names evidence");
    let links = std::fs::read_to_string(&links_file).expect("fd links evidence");
    // The transport fd number must not be open in the workload.
    assert!(
        !names
            .lines()
            .any(|line| line.trim() == SECCOMP_CHILD_FD.to_string()),
        "fd {SECCOMP_CHILD_FD} (the seccomp transport) must be closed before the \
         workload runs; observed: {names}"
    );
    // No memfd target may appear anywhere in the workload fd table.
    assert!(
        !links.contains("memfd:"),
        "no memfd may survive into the workload; observed: {links}"
    );
}

// Pure-layer pins — compile, transport, attach, rationale, non-activation ----------

#[test]
fn compile_memfd_transport_and_attach_pins() {
    // Compilation succeeds on the supported CI arch and yields the raw
    // sock_filter wire format (8 bytes per instruction — the format
    // bwrap's --seccomp consumes; the bwrap loader validates the total
    // length is a multiple of 8).
    let program = compile_namespace_policy_filter()
        .unwrap_or_else(|error| panic!("supported CI arch must compile: {error:?}"));
    assert!(!program.is_empty(), "a deny-list filter has instructions");

    // The memfd transport round-trips byte-length-exact AND keeps the
    // close-on-exec flag (the fd cannot leak across exec by accident —
    // the launcher's dup2 is the only hand-off).
    let fd = write_filter_to_memfd(&program).expect("memfd transport");
    // SAFETY: plain fcntl F_GETFD on the fd we own.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    assert!(
        flags & libc::FD_CLOEXEC != 0,
        "the seccomp memfd must be MFD_CLOEXEC (found flags {flags})"
    );
    let mut bytes = Vec::new();
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    file.read_to_end(&mut bytes).expect("read back");
    assert_eq!(bytes.len(), program.len() * 8);
    assert_eq!(bytes.len() % 8, 0, "bwrap validates an 8-byte multiple");
    // The File drop closes the fd exactly once.

    // Attach lands before the terminator and leaves the original pure
    // (re-pin of the production unit contract at the QA layer).
    if verified_pinned_binary().is_some() {
        let base = tempfile::tempdir().expect("tempdir");
        let profile = tempdir_profile(base.path(), vec!["/usr".into()]);
        let plan = build_bwrap_launch_plan(&profile, &pinned_bwrap(), "boot-p17-pin")
            .expect("plan builds");
        let attached = attach_seccomp_to_plan(&plan, SECCOMP_CHILD_FD).expect("attach");
        let seccomp_at = attached
            .argv
            .iter()
            .position(|argument| argument == "--seccomp")
            .expect("--seccomp present");
        let terminator = attached
            .argv
            .iter()
            .rposition(|argument| argument == "--")
            .expect("terminator present");
        assert!(seccomp_at < terminator);
        assert_eq!(attached.argv[seccomp_at + 1], SECCOMP_CHILD_FD.to_string());
        assert!(
            !plan.argv.iter().any(|argument| argument == "--seccomp"),
            "the original plan stays reusable without seccomp"
        );
    }
}

#[test]
fn fail_closed_arch_gate_and_error_materials_are_pinned() {
    // The UnsupportedArch material exists and speaks the contract (the
    // live branch cannot be exercised without faking
    // env::consts::ARCH — the gate itself is pinned by source below).
    let unsupported = SeccompPolicyError::UnsupportedArch { arch: "riscv64" };
    let message = unsupported.to_string();
    assert!(
        message.contains("x86_64/aarch64") && message.contains("riscv64"),
        "the fail-closed arch verdict must name the support set: {message}"
    );
    // The compile/transport materials exist for the SafeDisabled path.
    assert_eq!(
        SeccompPolicyError::Compile("detail".into()).to_string(),
        "seccomp filter compilation failed: detail"
    );
    assert_eq!(
        SeccompPolicyError::Transport("detail".into()).to_string(),
        "seccomp fd transport failed: detail"
    );

    // Source pins: the arch gate precedes any filter construction.
    let linux_source = include_str!("../src/services/sandbox/linux.rs");
    assert!(
        linux_source.contains(r#"matches!(arch, "x86_64" | "aarch64")"#),
        "the x86_64/aarch64 audit-arch gate must stay in the source"
    );
    assert!(
        linux_source.contains("UnsupportedArch"),
        "the fail-closed arch material must stay in the source"
    );
    // The unified ENOSYS rationale and the masked clone rule stay.
    assert!(
        linux_source.contains("Errno(libc::ENOSYS"),
        "the unified ENOSYS match action must stay in the source"
    );
    assert!(
        linux_source.contains("MaskedEq"),
        "the clone namespace-bit mask rule must stay in the source"
    );
    assert!(
        linux_source.contains("CLONE_NEWUSER"),
        "the namespace-bit constants must stay in the source"
    );
    // Never self-install: bwrap owns filter installation via --seccomp.
    assert!(
        !linux_source.contains("apply_filter"),
        "linux.rs must never call seccompiler::apply_filter (bwrap installs)"
    );
}

#[tokio::test]
async fn backend_stays_non_activating_until_p20() {
    let backend = LinuxBwrapPlanBackend::new("boot-p17");
    assert_eq!(backend.id(), "linux-bwrap-plan");

    // run_probes still refuses: real probe execution under bwrap+seccomp
    // is P20's migration — the Linux capability stays SafeDisabled.
    let base = tempfile::tempdir().expect("tempdir");
    let profile = tempdir_profile(base.path(), vec!["/usr".into()]);
    let helper = SafetyBinaryIdentity {
        path: "/usr/bin/r-code-safety-probe".into(),
        sha256: "0".repeat(64),
    };
    let refusal = backend
        .run_probes(&helper, &profile, &[SandboxProbeId::WriteOutsideAllowlist])
        .await;
    assert!(
        matches!(&refusal, Err(message) if message.contains("non-activating")
            && message.contains("P09")),
        "run_probes must keep the P09/P17-pending refusal: {refusal:?}"
    );

    // The P13 gate material is untouched by P17.
    let linux_source = include_str!("../src/services/sandbox/linux.rs");
    assert!(!linux_source.contains("platform_activation_gate"));
    assert!(!linux_source.contains("current_platform_material"));
    assert!(!linux_source.contains("SafetyStatus::Activated"));
    assert!(
        linux_source.contains("P09/P17 pending"),
        "the permanent refusal stays pinned in source"
    );
    let sandbox_source = include_str!("../src/services/sandbox.rs");
    assert!(sandbox_source.contains("\"none-this-wave\""));
    assert!(sandbox_source.contains("no native sandbox backend is available in this wave"));
    assert!(!sandbox_source.contains("LinuxBwrapPlanBackend"));
}
