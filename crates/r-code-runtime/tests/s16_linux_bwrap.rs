//! P16 QA — the non-activating pinned bwrap launch plan under real
//! verification. Native Linux CI owns execution; on the Windows dev host
//! this file is parse-only (empty suite). Nothing here mocks bwrap: the
//! pinned-binary and plan tests run against the REAL `/usr/bin/bwrap`
//! when the host provides it and assert the fail-closed verdicts when it
//! does not, and the e2e LAUNCHES the plan argv for real inside a
//! rootless user/mount/pid namespace to prove PID-1 handoff, mount
//! polarity, `.git` absence and env-allowlist containment from inside.
//! The backend stays non-activating (P09/P17 pending) — pinned by
//! `run_probes` refusal and by source pins on the P13 gate material.

#![cfg(target_os = "linux")]

use r_code_runtime::services::sandbox::linux::{
    build_bwrap_launch_plan, verify_pinned_bwrap, BwrapLaunchPlan, BwrapMount, BwrapPlanError,
    LinuxBwrapPlanBackend, NamespaceIdentityHandoff, MINIMUM_BWRAP_VERSION, PINNED_BWRAP_PATH,
};
use r_code_runtime::services::sandbox::{
    SafetyBinaryIdentity, SandboxBackend, SandboxNetworkClass, SandboxProbeId,
    SandboxProfileMaterial,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Wall clock each real launch gets before the namespace tree is killed.
const LAUNCH_DEADLINE: Duration = Duration::from_secs(45);

fn pinned_bwrap() -> PathBuf {
    PathBuf::from(PINNED_BWRAP_PATH)
}

fn make_dir(path: &Path) {
    std::fs::create_dir_all(path).unwrap_or_else(|error| panic!("mkdir {path:?}: {error}"));
}

fn write_file(path: &Path, contents: &str) {
    make_dir(path.parent().expect("parent"));
    std::fs::write(path, contents).unwrap_or_else(|error| panic!("write {path:?}: {error}"));
}

/// A real profile whose every workspace root is a fresh tempdir directory
/// (plus, for e2e launches, read-only system toolchain roots so a real
/// `/usr/bin/sh` workload can execute inside the empty sandbox root).
fn tempdir_profile(
    base: &Path,
    toolchain_roots: Vec<String>,
    network: SandboxNetworkClass,
) -> SandboxProfileMaterial {
    for root in ["read", "write", "scratch", "cache"] {
        make_dir(&base.path().join(root));
    }
    write_file(&base.path().join("read/source.txt"), "p16-read-root");
    write_file(&base.path().join("cache/registry.bin"), "p16-cache-root");
    SandboxProfileMaterial {
        read_roots: vec![base.path().join("read").to_string_lossy().into_owned()],
        write_roots: vec![base.path().join("write").to_string_lossy().into_owned()],
        scratch_root: base.path().join("scratch").to_string_lossy().into_owned(),
        toolchain_roots,
        cache_roots: vec![base.path().join("cache").to_string_lossy().into_owned()],
        git_hidden: true,
        inherited_fds: vec![0, 1, 2],
        environment_allowlist: vec!["PATH".into(), "RC16_E2E_TOKEN".into()],
        network,
    }
}

/// The pinned binary must be verifiable, or the host honestly lacks it.
/// Returns the verified version on success; on a genuinely absent binary
/// asserts the fail-closed `BinaryMissing` shape and returns None.
fn verified_pinned_binary() -> Option<(u32, u32)> {
    let pinned = pinned_bwrap();
    match verify_pinned_bwrap(&pinned) {
        Ok(version) => {
            let metadata = std::fs::metadata(&pinned).expect("stat pinned bwrap");
            assert_eq!(
                (metadata.uid(), metadata.gid()),
                (0, 0),
                "the pinned bwrap must be root:root on the CI host"
            );
            assert_eq!(
                metadata.mode() & 0o022,
                0,
                "the pinned bwrap must not be group/world-writable"
            );
            Some(version)
        }
        Err(BwrapPlanError::BinaryMissing(detail)) => {
            assert!(
                !pinned.exists(),
                "bwrap exists yet verification reports BinaryMissing: {detail}"
            );
            eprintln!(
                "P16 s16: /usr/bin/bwrap is genuinely absent on this runner — \
                 asserting the fail-closed BinaryMissing verdict (no fake success)"
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

// P16.1 — the real pinned binary ------------------------------------------------

#[test]
fn real_pinned_bwrap_verifies_with_floor_or_plan_refuses_old_version() {
    let Some(version) = verified_pinned_binary() else {
        return;
    };
    if version >= MINIMUM_BWRAP_VERSION {
        eprintln!("P16 s16: pinned bwrap {version:?} verified (>= {MINIMUM_BWRAP_VERSION:?})");
        return;
    }
    // Present but below the floor: the plan builder must refuse it — the
    // honest fail-closed outcome, never a degraded plan.
    let base = tempfile::tempdir().expect("tempdir");
    let profile = tempdir_profile(
        base.path(),
        vec!["/usr".into()],
        SandboxNetworkClass::Offline,
    );
    assert!(matches!(
        build_bwrap_launch_plan(&profile, &pinned_bwrap(), "boot-p16"),
        Err(BwrapPlanError::VersionTooOld {
            found,
            minimum
        }) if found == version && minimum == MINIMUM_BWRAP_VERSION
    ));
}

// P16.2/P16.3 — plan shape over the REAL pinned binary ---------------------------

#[test]
fn plan_pins_one_pid_namespace_mount_polarity_and_empty_handoff() {
    let Some(version) = verified_pinned_binary() else {
        return;
    };
    assert!(
        version >= MINIMUM_BWRAP_VERSION,
        "plan-shape pin needs a usable pinned binary (found {version:?})"
    );
    let base = tempfile::tempdir().expect("tempdir");
    let profile = tempdir_profile(
        base.path(),
        vec!["/usr".into(), "/lib".into(), "/lib64".into()],
        SandboxNetworkClass::Offline,
    );
    let plan = build_bwrap_launch_plan(&profile, &pinned_bwrap(), "boot-p16-qa")
        .expect("plan builds over the verified pinned binary");

    // Exactly ONE component owns the PID namespace/PID1 (P16.2).
    assert_eq!(
        plan.argv
            .iter()
            .filter(|arg| arg.as_str() == "--unshare-pid")
            .count(),
        1,
        "argv carries --unshare-pid exactly once"
    );
    assert!(plan.unshare_pid);

    // Explicit network namespace decision (P16.3): Offline unshares net;
    // PublicInternetClient records share-net as plan material only; the
    // host stack is refused outright.
    assert_eq!(
        plan.argv
            .iter()
            .filter(|arg| arg.as_str() == "--unshare-net")
            .count(),
        1
    );
    assert!(plan.unshare_network);
    let mut public = profile.clone();
    public.network = SandboxNetworkClass::PublicInternetClient;
    let public_plan =
        build_bwrap_launch_plan(&public, &pinned_bwrap(), "boot-p16-qa").expect("public plan");
    assert!(!public_plan.unshare_network);
    assert!(!public_plan.argv.iter().any(|arg| arg == "--unshare-net"));
    let mut host = profile.clone();
    host.network = SandboxNetworkClass::HostNetwork;
    assert!(matches!(
        build_bwrap_launch_plan(&host, &pinned_bwrap(), "boot-p16-qa"),
        Err(BwrapPlanError::InvalidProfile(reason)) if reason.contains("host-network")
    ));

    // The mount table covers EXACTLY the profile roots with the right
    // polarity: read/toolchain/cache read-only, write+scratch read-write.
    let expected_ro: BTreeSet<String> = profile
        .read_roots
        .iter()
        .chain(profile.toolchain_roots.iter())
        .chain(profile.cache_roots.iter())
        .cloned()
        .collect();
    let expected_rw: BTreeSet<String> = profile
        .write_roots
        .iter()
        .chain([&profile.scratch_root])
        .cloned()
        .collect();
    let mount_ro: BTreeSet<String> = plan
        .mounts
        .iter()
        .filter_map(|mount| match mount {
            BwrapMount::ReadOnly { source, .. } => Some(source.clone()),
            _ => None,
        })
        .collect();
    let mount_rw: BTreeSet<String> = plan
        .mounts
        .iter()
        .filter_map(|mount| match mount {
            BwrapMount::ReadWrite { source, .. } => Some(source.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(mount_ro, expected_ro, "read-only mounts cover the ro roots");
    assert_eq!(
        mount_rw, expected_rw,
        "read-write mounts cover write+scratch"
    );
    assert_eq!(plan.mounts.len(), expected_ro.len() + expected_rw.len());
    for mount in &plan.mounts {
        let (source, target) = match mount {
            BwrapMount::ReadOnly { source, target } | BwrapMount::ReadWrite { source, target } => {
                (source.as_str(), target.as_str())
            }
            BwrapMount::Tmpfs { target } => ("", target.as_str()),
        };
        assert!(!target.contains(".git"), "no .git in the mount table");
        assert!(!source.contains(".git"), "no .git mount source");
    }

    // The argv mirrors the table: every mount appears as an adjacent
    // --ro-bind/--bind triple (whitelist mounting hides .git because the
    // table never names it).
    for mount in &plan.mounts {
        let (flag, source, target) = match mount {
            BwrapMount::ReadOnly { source, target } => {
                ("--ro-bind", source.as_str(), target.as_str())
            }
            BwrapMount::ReadWrite { source, target } => {
                ("--bind", source.as_str(), target.as_str())
            }
            BwrapMount::Tmpfs { target } => ("--tmpfs", "", target.as_str()),
        };
        let present = plan
            .argv
            .windows(3)
            .any(|window| window[0] == flag && window[1] == source && window[2] == target);
        assert!(present, "argv carries {flag} {source} {target}");
    }

    // Env allowlist: one --setenv per allowlisted key with the empty
    // value placeholder the launcher fills at spawn time.
    assert_eq!(
        plan.argv
            .iter()
            .filter(|arg| arg.as_str() == "--setenv")
            .count(),
        profile.environment_allowlist.len()
    );
    for key in &profile.environment_allowlist {
        let present = plan
            .argv
            .windows(3)
            .any(|window| window[0] == "--setenv" && window[1] == *key && window[2].is_empty());
        assert!(
            present,
            "argv carries --setenv {key} with the empty value slot"
        );
    }

    // The argv terminates with the workload separator.
    assert_eq!(plan.argv[plan.argv.len() - 2], "--");
    assert_eq!(plan.argv[plan.argv.len() - 1], "<workload-argv-follows>");

    // The identity handoff starts EMPTY (P16.2): nothing launched yet, so
    // no half-complete handoff can ever look complete.
    assert_eq!(
        plan.identity_handoff,
        NamespaceIdentityHandoff::expected("boot-p16-qa")
    );
    assert_eq!(plan.identity_handoff.outer_pid, None);
    assert_eq!(plan.identity_handoff.namespace_pid1, None);
    assert!(!plan.identity_handoff.is_complete());

    // Determinism: the same inputs yield the byte-identical plan.
    let again =
        build_bwrap_launch_plan(&profile, &pinned_bwrap(), "boot-p16-qa").expect("plan rebuilds");
    assert_eq!(again.argv, plan.argv);
    assert_eq!(again.mounts, plan.mounts);

    // The policy material commits table, namespace flags and allowlist
    // into the P13 digest contract.
    let material = plan.policy_material();
    assert_eq!(material["unsharePid"], serde_json::json!(true));
    assert_eq!(material["unshareNetwork"], serde_json::json!(true));
    assert_eq!(
        material["environmentAllowlist"],
        serde_json::json!(profile.environment_allowlist)
    );
    assert_eq!(
        material["mounts"].as_array().map(Vec::len),
        Some(plan.mounts.len())
    );
}

// P16.2/P16.3 — the real launch (e2e heart) --------------------------------------

/// True when stderr shows the RUNNER refusing the rootless user namespace
/// (AppArmor/seccomp/kernel.disable_userns style denials) — an environment
/// refusal, not a plan defect.
fn is_environment_namespace_refusal(stderr: &str) -> bool {
    let lowered = stderr.to_ascii_lowercase();
    [
        "user namespace",
        "uid map",
        "gid map",
        "unshare",
        "operation not permitted",
    ]
    .iter()
    .any(|needle| lowered.contains(*needle))
}

fn read_child_stderr(child: &mut std::process::Child) -> String {
    let mut text = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut text);
    }
    text
}

/// Assert the launch failure is the environment honestly refusing the
/// namespace (nothing ran — no pid/env evidence files exist) and record
/// it; a non-refusal failure is a REAL defect and panics with stderr.
fn record_launch_outcome(
    child: &mut std::process::Child,
    status: std::process::ExitStatus,
    evidence_dir: &Path,
    launch: &str,
) {
    let stderr = read_child_stderr(child);
    if is_environment_namespace_refusal(&stderr) {
        eprintln!(
            "P16 s16: {launch} refused by the runner environment (no usable rootless user \
             namespace) — fail-closed record, nothing executed:\n{stderr}"
        );
        assert!(
            !evidence_dir.join("pid").exists(),
            "no workload may run when the namespace is refused"
        );
        return;
    }
    panic!("{launch} failed without a namespace-refusal signature: {status}\nstderr: {stderr}");
}

/// Launch the plan's argv prefix with a real workload appended: drop the
/// placeholder, fill every empty `--setenv KEY ""` value with the real
/// launcher-supplied value, clear the parent environment (the plan's
/// allowlist via --setenv is then the COMPLETE sandbox environment), and
/// put the child in its own process group so the tree can be reaped.
fn launch_plan_workload(
    plan: &BwrapLaunchPlan,
    values: &BTreeMap<&str, String>,
    workload: &[&str],
    stdout: Stdio,
) -> std::process::Child {
    let mut argv = plan.argv.clone();
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
    argv.extend(workload.iter().map(|argument| argument.to_string()));
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]);
    command.env_clear();
    command.stdin(Stdio::null());
    command.stdout(stdout);
    command.stderr(Stdio::piped());
    command.process_group(0);
    command
        .spawn()
        .unwrap_or_else(|error| panic!("spawn bwrap plan argv {argv:?}: {error}"))
}

fn wait_with_deadline(child: &mut std::process::Child) -> Option<std::process::ExitStatus> {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {}
            Err(error) => panic!("try_wait bwrap child: {error}"),
        }
        if start.elapsed() >= LAUNCH_DEADLINE {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Kill the whole namespace tree (process group) and reap the leader.
fn kill_tree_and_reap(child: &mut std::process::Child) {
    let pgid = child.id() as i32;
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
    let _ = child.wait();
}

#[test]
fn real_launch_proves_pid_namespace_polarity_git_absence_and_env_allowlist() {
    let Some(version) = verified_pinned_binary() else {
        return;
    };
    assert!(
        version >= MINIMUM_BWRAP_VERSION,
        "e2e needs a usable pinned binary (found {version:?})"
    );

    let base = tempfile::tempdir().expect("tempdir");
    // A workspace .git that exists on the host OUTSIDE every mounted root:
    // whitelist mounting means it is simply never mounted inside.
    let dot_git = base.path().join("repo/.git");
    write_file(&dot_git.join("HEAD"), "ref: refs/heads/main");
    let mut toolchain_roots = vec!["/usr".to_string()];
    for candidate in ["/lib", "/lib64"] {
        if Path::new(candidate).exists() {
            toolchain_roots.push(candidate.to_string());
        }
    }
    let profile = tempdir_profile(base.path(), toolchain_roots, SandboxNetworkClass::Offline);
    let scratch = base.path().join("scratch");
    let read_root = base.path().join("read");
    let write_root = base.path().join("write");
    let plan = build_bwrap_launch_plan(&profile, &pinned_bwrap(), "boot-p16-e2e")
        .expect("e2e plan builds over the verified pinned binary");

    let values: BTreeMap<&str, String> = BTreeMap::from([
        ("PATH", "/usr/bin".to_string()),
        ("RC16_E2E_TOKEN", "p16-e2e-token".to_string()),
    ]);

    // Launch 1 — env proof with printenv as the DIRECT payload (no shell
    // in between): the sandbox environment must be EXACTLY the allowlist.
    let env_file = scratch.join("env.txt");
    let env_out = File::create(&env_file).expect("create env evidence file");
    let mut env_child =
        launch_plan_workload(&plan, &values, &["/usr/bin/printenv"], Stdio::from(env_out));
    let outer_env_pid = env_child.id();
    match wait_with_deadline(&mut env_child) {
        Some(status) if status.success() => {}
        Some(status) => {
            record_launch_outcome(&mut env_child, status, &scratch, "printenv launch");
            return;
        }
        None => {
            kill_tree_and_reap(&mut env_child);
            panic!("printenv launch exceeded the {LAUNCH_DEADLINE:?} deadline");
        }
    }
    let env_text = std::fs::read_to_string(&env_file).expect("read env evidence");
    let observed: BTreeSet<&str> = env_text
        .lines()
        .filter_map(|line| line.split_once('=').map(|(key, _)| key))
        .collect();
    assert_eq!(
        observed,
        BTreeSet::from(["PATH", "RC16_E2E_TOKEN"]),
        "only allowlisted keys may exist inside (observed: {env_text})"
    );
    assert!(env_text.contains("RC16_E2E_TOKEN=p16-e2e-token"));
    assert!(env_text.contains("PATH=/usr/bin"));

    // Launch 2 — the namespace/polarity/git/writability proof with a real
    // /usr/bin/sh workload writing its evidence into the scratch root.
    let scratch_text = scratch.to_string_lossy();
    let read_text = read_root.to_string_lossy();
    let write_text = write_root.to_string_lossy();
    let dot_git_text = dot_git.to_string_lossy();
    let script = format!(
        "echo $$ > '{scratch_text}/pid'\n\
         if touch '{read_text}/leak.txt' 2>/dev/null; then echo ro-write-ok > '{scratch_text}/ro'; else echo ro-denied > '{scratch_text}/ro'; fi\n\
         if ls '{dot_git_text}' > '{scratch_text}/git.out' 2>&1; then echo git-visible > '{scratch_text}/git'; else echo git-absent > '{scratch_text}/git'; fi\n\
         if touch '{write_text}/wrote.txt' 2>/dev/null; then echo write-ok > '{scratch_text}/write'; else echo write-denied > '{scratch_text}/write'; fi\n\
         if touch '{scratch_text}/touched.txt' 2>/dev/null; then echo scratch-ok > '{scratch_text}/scratch'; else echo scratch-denied > '{scratch_text}/scratch'; fi\n"
    );
    let mut sh_child = launch_plan_workload(
        &plan,
        &values,
        &["/usr/bin/sh", "-c", &script],
        Stdio::null(),
    );
    let outer_pid = sh_child.id();
    let outcome = wait_with_deadline(&mut sh_child);
    let Some(status) = outcome else {
        kill_tree_and_reap(&mut sh_child);
        panic!("sh launch exceeded the {LAUNCH_DEADLINE:?} deadline");
    };
    if !status.success() {
        record_launch_outcome(&mut sh_child, status, &scratch, "sh launch");
        return;
    }

    // PID-namespace proof: the workload's $$ is namespace-relative (the
    // payload IS the PID1/reaper inside --unshare-pid, typically exactly
    // 1) and differs from the OUTER bwrap pid the supervisor owns.
    let inner_pid: u32 = std::fs::read_to_string(scratch.join("pid"))
        .expect("pid evidence written by the workload")
        .trim()
        .parse()
        .expect("pid evidence is numeric");
    assert!(
        inner_pid <= 16,
        "workload pid must be namespace-small, got {inner_pid}"
    );
    assert_ne!(
        inner_pid, outer_pid,
        "inner pid must differ from the outer bwrap pid"
    );
    eprintln!(
        "P16 s16 e2e: outer bwrap pid {outer_pid} (env launch {outer_env_pid}), \
         namespace pid {inner_pid} — exactly one component owns PID1"
    );

    // Mount polarity from inside: read root is read-only, write root and
    // scratch are writable.
    assert_eq!(
        std::fs::read_to_string(scratch.join("ro"))
            .unwrap_or_default()
            .trim(),
        "ro-denied"
    );
    assert!(
        !read_root.join("leak.txt").exists(),
        "no write into the ro root"
    );
    assert_eq!(
        std::fs::read_to_string(scratch.join("write"))
            .unwrap_or_default()
            .trim(),
        "write-ok"
    );
    assert!(write_root.join("wrote.txt").exists());
    assert_eq!(
        std::fs::read_to_string(scratch.join("scratch"))
            .unwrap_or_default()
            .trim(),
        "scratch-ok"
    );
    assert!(scratch.join("touched.txt").exists());

    // .git absence from inside: the path is not in the mount table, so ls
    // fails inside the sandbox even though it exists on the host.
    assert_eq!(
        std::fs::read_to_string(scratch.join("git"))
            .unwrap_or_default()
            .trim(),
        "git-absent"
    );
    assert!(dot_git.exists(), "the host .git fixture still exists");

    // The observed handoff P09 will consume: outer bwrap pid + namespace
    // PID1 — both halves present is exactly a complete handoff, and the
    // empty plan handoff can never look like one.
    let observed = NamespaceIdentityHandoff {
        outer_pid: Some(outer_pid),
        namespace_pid1: Some(inner_pid),
        expected_boot_id: "boot-p16-e2e".into(),
    };
    assert!(observed.is_complete());
    assert!(!NamespaceIdentityHandoff::expected("boot-p16-e2e").is_complete());
}

// Non-activation pins ------------------------------------------------------------

#[tokio::test]
async fn plan_backend_is_permanently_non_activating() {
    let backend = LinuxBwrapPlanBackend::new("boot-p16");
    assert_eq!(backend.id(), "linux-bwrap-plan");

    // run_probes can NEVER produce probe evidence: P09/P17 own execution,
    // so the Linux capability stays SafeDisabled by construction.
    let base = tempfile::tempdir().expect("tempdir");
    let profile = tempdir_profile(
        base.path(),
        vec!["/usr".into()],
        SandboxNetworkClass::Offline,
    );
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
        "run_probes must refuse with the P09/P17-pending message: {refusal:?}"
    );

    // The digest path is real but honest about a missing binary.
    match backend.policy_digest(&profile) {
        Ok(first) => {
            let second = backend.policy_digest(&profile).expect("digest rebuilds");
            assert_eq!(first, second, "policy digest is deterministic");
            let mut changed = profile.clone();
            changed.read_roots.push("/opt/other".into());
            assert_ne!(
                backend
                    .policy_digest(&changed)
                    .expect("digest for changed profile"),
                first,
                "a changed profile must change the policy digest"
            );
        }
        Err(message) => {
            assert!(
                message.contains("missing or unreadable"),
                "the only honest digest failure is the fail-closed binary verdict: {message}"
            );
        }
    }
}

#[test]
fn source_pins_leave_the_activation_gate_untouched() {
    let linux_module = include_str!("../src/services/sandbox/linux.rs");
    assert!(
        !linux_module.contains("platform_activation_gate"),
        "the plan builder must never call the P13 activation gate"
    );
    assert!(
        !linux_module.contains("current_platform_material"),
        "the plan builder must never touch the current-platform material"
    );
    assert!(
        !linux_module.contains("SafetyStatus::Activated"),
        "the plan builder has no path to an Activated verdict"
    );
    assert!(
        linux_module.contains("P09/P17 pending"),
        "the permanent run_probes refusal stays pinned in source"
    );

    // The wave's gate material is unchanged: still none-this-wave and
    // honestly Unsupported — the Linux plan backend is registered as a
    // module only, never wired into the gate.
    let sandbox_module = include_str!("../src/services/sandbox.rs");
    assert!(sandbox_module.contains("\"none-this-wave\""));
    assert!(sandbox_module.contains("no native sandbox backend is available in this wave"));
    assert!(!sandbox_module.contains("LinuxBwrapPlanBackend"));
}

// Fail-closed refusals -----------------------------------------------------------

#[test]
fn profile_refusals_precede_any_binary_trust() {
    let base = tempfile::tempdir().expect("tempdir");
    let profile = tempdir_profile(
        base.path(),
        vec!["/usr".into()],
        SandboxNetworkClass::Offline,
    );
    // A binary path that does not exist: profile-level refusals fire
    // before any binary trust, and a plain build fails BinaryMissing.
    let missing = PathBuf::from("/nonexistent/p16/bwrap");

    let mut host_stack = profile.clone();
    host_stack.network = SandboxNetworkClass::HostNetwork;
    assert!(matches!(
        build_bwrap_launch_plan(&host_stack, &missing, "boot-p16"),
        Err(BwrapPlanError::InvalidProfile(reason)) if reason.contains("host-network")
    ));

    let mut git_read = profile.clone();
    git_read.read_roots = vec![base.path().join("repo/.git").to_string_lossy().into_owned()];
    assert!(matches!(
        build_bwrap_launch_plan(&git_read, &missing, "boot-p16"),
        Err(BwrapPlanError::InvalidProfile(reason)) if reason.contains(".git")
    ));

    let mut git_scratch = profile.clone();
    git_scratch.scratch_root = base.path().join("scratch/.git").to_string_lossy().into_owned();
    assert!(matches!(
        build_bwrap_launch_plan(&git_scratch, &missing, "boot-p16"),
        Err(BwrapPlanError::InvalidProfile(reason)) if reason.contains(".git")
    ));

    let mut exposed = profile.clone();
    exposed.git_hidden = false;
    assert!(matches!(
        build_bwrap_launch_plan(&exposed, &missing, "boot-p16"),
        Err(BwrapPlanError::InvalidProfile(_))
    ));

    assert!(matches!(
        build_bwrap_launch_plan(&profile, &missing, "boot-p16"),
        Err(BwrapPlanError::BinaryMissing(_))
    ));
}

/// Try to hand the stub to root via passwordless sudo (available on the
/// GitHub-hosted Linux CI); false when this host cannot.
fn try_chown_root(path: &Path) -> bool {
    run_sudo(&["chown", "0:0"], path)
}

/// Root-side chmod for a root-owned stub; false without sudo.
fn sudo_chmod(path: &Path, mode: u32) -> bool {
    let octal = format!("{mode:o}");
    run_sudo(&["chmod", octal.as_str()], path)
}

fn run_sudo(arguments: &[&str], path: &Path) -> bool {
    Command::new("sudo")
        .arg("-n")
        .args(arguments)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Replace the stub script and hand it back to root (unlinking a file in
/// a user-owned directory is allowed even when the file is root-owned).
fn replace_root_owned_stub(path: &Path, script: &str) -> bool {
    std::fs::remove_file(path).expect("unlink stub");
    write_file(path, script);
    chmod(path, 0o755);
    try_chown_root(path)
}

fn chmod(path: &Path, mode: u32) {
    let mut permissions = std::fs::metadata(path).expect("stat stub").permissions();
    permissions.set_mode(mode);
    std::fs::set_permissions(path, permissions).expect("chmod stub");
}

#[test]
fn version_floor_and_ownership_branches_fail_closed() {
    let base = tempfile::tempdir().expect("tempdir");
    let stub = base.path().join("bwrap");
    write_file(&stub, "#!/bin/sh\necho 'bubblewrap 0.7.9'\n");
    chmod(&stub, 0o755);
    let profile = tempdir_profile(
        base.path(),
        vec!["/usr".into()],
        SandboxNetworkClass::Offline,
    );

    if try_chown_root(&stub) {
        // Root-owned stub reporting 0.7.9: the floor refuses it (0,7) < (0,8).
        assert_eq!(
            build_bwrap_launch_plan(&profile, &stub, "boot-p16")
                .expect_err("old version must be refused"),
            BwrapPlanError::VersionTooOld {
                found: (0, 7),
                minimum: MINIMUM_BWRAP_VERSION
            }
        );
        // Unparseable version output is fail-closed too.
        if replace_root_owned_stub(&stub, "#!/bin/sh\necho 'banana'\n") {
            assert!(matches!(
                verify_pinned_bwrap(&stub),
                Err(BwrapPlanError::VersionUnparseable(_))
            ));
        }
        // A group-writable root-owned binary is not immutable.
        if sudo_chmod(&stub, 0o775) {
            assert!(matches!(
                verify_pinned_bwrap(&stub),
                Err(BwrapPlanError::BinaryNotImmutable(reason)) if reason.contains("group/world write")
            ));
            let _ = sudo_chmod(&stub, 0o755);
        }
    } else {
        // No root handover on this host: the ownership gate is itself the
        // proof — a user-owned binary can never be the pinned bwrap.
        assert!(matches!(
            verify_pinned_bwrap(&stub),
            Err(BwrapPlanError::BinaryNotImmutable(reason)) if reason.contains("owner")
        ));
        // The floor's shape is pinned where it cannot be exercised live.
        assert!(BwrapPlanError::VersionTooOld {
            found: (0, 7),
            minimum: MINIMUM_BWRAP_VERSION
        }
        .to_string()
        .contains("below"));
        eprintln!(
            "P16 s16: no sudo on this host — VersionTooOld covered by shape only; \
             the root:root branch runs on the native Linux CI runner"
        );
    }
}
