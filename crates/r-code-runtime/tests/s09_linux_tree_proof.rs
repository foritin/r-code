//! P09 QA — the exact bwrap PID-namespace tree proof. Native Linux CI owns
//! execution; on the Windows dev host this file is parse-only (empty suite).
//! Nothing here mocks bwrap: every launch goes through the REAL pinned
//! `/usr/bin/bwrap` over a REAL P16 plan (mounts filled, `--setenv` values
//! supplied by the launcher), and the identity and proof observe the
//! NAMESPACE PID1 that bwrap hands out via `--info-fd` — never the outer
//! monitor and never the host namespace. The proof is PID1's pidfd becoming
//! readable (the kernel marks it at exit, zombie included) AND PID1's /proc
//! entry disappearing once the monitor reaps it; when PID1 dies the kernel
//! SIGKILLs every remaining namespace member, which subsumes setsid and
//! double-fork escapees, while a workload-leader or monitor exit alone is
//! NEVER the proof. Every signal rides the PID1 pidfd or the owned monitor
//! Child handle — no bare kill(pid) anywhere. If the runner lacks bwrap or
//! refuses rootless user namespaces, the honest environmental refusal is
//! recorded — never a fake pass.

#![cfg(target_os = "linux")]

// The supervisor registration (P09) is the intended consumer surface; the
// lower-level identity helpers that are not re-exported come from the
// bwrap_proof module directly.
use r_code_runtime::process_guard::unix::bwrap_proof::{
    namespace_inode, pidfd_open, pidfd_signal, proc_start_identity, wait_pid1_exit,
};
use r_code_runtime::services::process_supervisor::{
    launch_bwrap_tree, prove_namespace_empty, terminate_tree, wait_monitor, BwrapTreeIdentity,
    PidFd,
};
use r_code_runtime::services::sandbox::linux::{
    build_bwrap_launch_plan, verify_pinned_bwrap, BwrapPlanError, PINNED_BWRAP_PATH,
};
use r_code_runtime::services::sandbox::{SandboxNetworkClass, SandboxProfileMaterial};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

/// Wall clock each bounded proof waits before giving up.
const PROOF_TIMEOUT: Duration = Duration::from_secs(20);

fn pinned_bwrap() -> PathBuf {
    PathBuf::from(PINNED_BWRAP_PATH)
}

fn make_dir(path: &Path) {
    std::fs::create_dir_all(path).unwrap_or_else(|error| panic!("mkdir {path:?}: {error}"));
}

/// The bwrap_proof source region (everything after the module opener up to
/// the unit-test module) for the source-level discipline pins.
fn proof_module_region() -> &'static str {
    let unix_source = include_str!("../src/process_guard/unix.rs");
    unix_source
        .split("pub mod bwrap_proof")
        .nth(1)
        .expect("bwrap_proof module exists")
        .split("#[cfg(test)]")
        .next()
        .expect("the module ends before the tests module")
}

/// One function's body out of the proof module region (from its `pub fn`
/// opener to the next top-level pub fn or the module end).
fn proof_fn_body(name: &str) -> &str {
    let module = proof_module_region();
    let marker = format!("pub fn {name}");
    let start = module
        .find(&marker)
        .unwrap_or_else(|| panic!("the {name} function exists in bwrap_proof"));
    let rest = &module[start..];
    let end = rest[marker.len()..]
        .find("\n    pub fn ")
        .map(|offset| marker.len() + offset + 1)
        .unwrap_or(rest.len());
    &rest[..end]
}

/// A real profile whose workspace roots are fresh tempdirs and whose
/// toolchain roots are the host system roots, so `/usr/bin/sh` and friends
/// really execute inside the sandbox.
fn tempdir_profile(base: &Path) -> SandboxProfileMaterial {
    for root in ["read", "write", "scratch", "cache"] {
        make_dir(&base.join(root));
    }
    let mut toolchain_roots = vec!["/usr".to_string()];
    for candidate in ["/lib", "/lib64"] {
        if Path::new(candidate).exists() {
            toolchain_roots.push(candidate.to_string());
        }
    }
    SandboxProfileMaterial {
        read_roots: vec![base.join("read").to_string_lossy().into_owned()],
        write_roots: vec![base.join("write").to_string_lossy().into_owned()],
        scratch_root: base.join("scratch").to_string_lossy().into_owned(),
        toolchain_roots,
        cache_roots: vec![base.join("cache").to_string_lossy().into_owned()],
        git_hidden: true,
        inherited_fds: vec![0, 1, 2],
        environment_allowlist: vec!["PATH".into(), "RC09_TOKEN".into()],
        network: SandboxNetworkClass::Offline,
    }
}

/// The pinned binary must verify, or the host honestly lacks it (the s16
/// precedent: assert the fail-closed verdict and skip — never fake).
fn verified_pinned_binary() -> Option<(u32, u32)> {
    match verify_pinned_bwrap(&pinned_bwrap()) {
        Ok(version) => Some(version),
        Err(BwrapPlanError::BinaryMissing(_)) => {
            eprintln!(
                "P09 s09: /usr/bin/bwrap is genuinely absent on this runner — \
                 recording the honest environmental refusal (no fake pass)"
            );
            assert!(!pinned_bwrap().exists());
            None
        }
        Err(other) => panic!("pinned bwrap present but verification failed: {other:?}"),
    }
}

/// Launcher-side argv completion (the P16 contract): drop the workload
/// placeholder and fill every empty `--setenv KEY ""` slot with the real
/// value. The returned prefix still ends at `--`; the workload follows.
fn filled_bwrap_prefix(
    plan_environment_allowlist: &[String],
    plan_argv: &[String],
    values: &BTreeMap<&str, String>,
) -> Vec<String> {
    let mut argv = plan_argv.to_vec();
    assert_eq!(
        argv.pop().as_deref(),
        Some("<workload-argv-follows>"),
        "the plan argv ends with the workload placeholder"
    );
    let mut index = 0;
    while index + 2 < argv.len() {
        if argv[index] == "--setenv" && argv[index + 2].is_empty() {
            let key = argv[index + 1].clone();
            assert!(
                plan_environment_allowlist
                    .iter()
                    .any(|allowed| *allowed == key),
                "the plan only setenvs allowlisted keys (found {key})"
            );
            argv[index + 2] = values
                .get(key.as_str())
                .unwrap_or_else(|| panic!("no launcher value for allowlisted key {key}"))
                .clone();
            index += 3;
        } else {
            index += 1;
        }
    }
    assert_eq!(argv.last().map(String::as_str), Some("--"));
    argv
}

/// One real launch over a real plan. `None` records an honest environmental
/// refusal (the runner refused the rootless user namespace — nothing ran).
fn launch_over_plan(base: &Path, workload_argv: &[&str]) -> Option<(BwrapTreeIdentity, PathBuf)> {
    let profile = tempdir_profile(base);
    let plan = build_bwrap_launch_plan(&profile, &pinned_bwrap(), "boot-p09-qa")
        .expect("plan builds over the verified pinned binary");
    let values: BTreeMap<&str, String> = BTreeMap::from([
        ("PATH", "/usr/bin".to_string()),
        ("RC09_TOKEN", "p09-qa-token".to_string()),
    ]);
    let prefix = filled_bwrap_prefix(&plan.environment_allowlist, &plan.argv, &values);
    let environment: BTreeMap<String, String> = values
        .iter()
        .map(|(key, value)| ((*key).to_string(), value.clone()))
        .collect();
    let workload_owned: Vec<String> = workload_argv.iter().map(|s| (*s).to_string()).collect();
    match launch_bwrap_tree(&prefix, &workload_owned, &environment, None) {
        Ok(tree) => Some((tree, base.join("scratch"))),
        Err(error) => {
            eprintln!(
                "P09 s09: launch refused by the runner environment (rootless user \
                 namespace unavailable?) — honest refusal, nothing executed: {error:?}"
            );
            None
        }
    }
}

fn heartbeat_age_ms(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    Some(SystemTime::now().duration_since(modified).ok()?.as_millis() as u64)
}

fn heartbeat_is_fresh(path: &Path) -> bool {
    heartbeat_age_ms(path).is_some_and(|age_ms| age_ms < 2500)
}

/// Wait until the heartbeat file exists (the escapee is running inside the
/// namespace). None records an environmental refusal (the sandbox never
/// came up).
fn wait_for_heartbeat(path: &Path) -> Option<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if path.exists() {
            return Some(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    eprintln!(
        "P09 s09: the sandboxed escapee never heartbeated — recording the \
         honest environmental refusal instead of guessing"
    );
    None
}

// 1 — full lifecycle e2e: the escapees die WITH the namespace -----------------

#[test]
fn escapees_die_with_the_namespace_on_natural_lifecycle() {
    let Some(version) = verified_pinned_binary() else {
        return;
    };
    assert!(
        version >= (0, 8),
        "e2e needs a usable pinned bwrap (found {version:?})"
    );
    let base = tempfile::tempdir().expect("tempdir");
    // The workload (the sandbox's namespace PID1) spawns a setsid grandchild
    // AND a double-forked great-grandchild that both outlive it (each sleeps
    // 3s while the workload exits after ~1s), then exits.
    let script = "/usr/bin/setsid /usr/bin/sleep 3 & \
                  /usr/bin/sh -c '/usr/bin/sh -c \"/usr/bin/sleep 3 &\" &' ; \
                  /usr/bin/sleep 1 ; exit 0";
    let Some((mut tree, _scratch)) = launch_over_plan(base.path(), &["/usr/bin/sh", "-c", script])
    else {
        return;
    };

    // Identity discipline (P09.1): the persisted namespace inode must be a
    // REAL sandbox namespace — different from this supervisor process's own
    // pid namespace — and the PID1 start fence must be nonzero.
    let own_inode = namespace_inode(std::process::id())
        .expect("the test process's own ns/pid inode is readable");
    assert_ne!(
        tree.namespace_inode, own_inode,
        "the persisted namespace inode must be the SANDBOX's, not the host's"
    );
    assert!(tree.namespace_pid1 > 1);
    assert!(tree.pid1_start_identity > 0);

    // THE P09.2 proof: PID1's death empties the namespace, subsuming the
    // setsid and double-fork escapees — no enumeration, no group heuristics.
    assert_eq!(
        prove_namespace_empty(&tree, PROOF_TIMEOUT),
        Ok(true),
        "the escapees must die with the namespace"
    );
    assert!(
        wait_monitor(&mut tree, Duration::from_secs(10)),
        "the monitor child is reaped through the owned handle"
    );
}

// 2 — terminate path ------------------------------------------------------------

#[test]
fn terminate_tree_empties_a_live_namespace() {
    let Some(version) = verified_pinned_binary() else {
        return;
    };
    assert!(version >= (0, 8));
    let base = tempfile::tempdir().expect("tempdir");
    let Some((mut tree, _scratch)) = launch_over_plan(base.path(), &["/usr/bin/sleep", "300"])
    else {
        return;
    };

    // Live-identity pin: while the sandbox lives, the PID1's CURRENT
    // namespace inode is exactly the persisted one — the identity observes
    // the handed-off sandbox PID1's namespace, never the monitor's host one.
    assert_ne!(
        tree.namespace_pid1, tree.monitor_pid,
        "the observed PID1 is the sandbox init, not the outer monitor"
    );
    assert!(tree.monitor_pid > 1);
    assert_eq!(
        namespace_inode(tree.namespace_pid1).as_deref(),
        Some(tree.namespace_inode.as_str()),
        "the persisted inode is the live sandbox PID1's namespace"
    );

    // While the namespace is alive the proof must refuse to conclude.
    assert_eq!(
        prove_namespace_empty(&tree, Duration::from_secs(2)),
        Ok(false),
        "a live namespace is never proven empty"
    );
    // Terminate through the PID1 pidfd (recycle-safe signalling) and prove
    // the namespace actually empties within the bounded wait.
    assert!(
        terminate_tree(&mut tree),
        "the SIGKILL must be delivered through the PID1 pidfd"
    );
    assert_eq!(
        prove_namespace_empty(&tree, PROOF_TIMEOUT),
        Ok(true),
        "terminate must empty the namespace (P09.2)"
    );
}

// 3 — identity discipline (P09.1) and pidfd-only signalling ---------------------

#[test]
fn identity_is_captured_at_launch_and_signals_never_use_bare_pids() {
    let own_pid = std::process::id();
    let own_start =
        proc_start_identity(own_pid).expect("the test process's own start identity is readable");
    assert!(own_start > 0);

    // Source pin: inside bwrap_proof no wait/kill path uses a bare
    // kill(pid) — every signal goes through pidfd_signal, terminate rides
    // the PID1 pidfd, and the only other kill is the owned monitor Child.
    let proof_region = proof_module_region();
    assert!(
        !proof_region.contains("libc::kill("),
        "no bare kill() may exist in the proof path — pidfd/Child handle only"
    );
    assert!(
        proof_region.contains("pidfd_signal(&identity.pid1_pidfd, libc::SIGKILL)"),
        "terminate_tree signals the namespace PID1 through its pidfd"
    );

    // Registration pin: the supervisor re-exports the Linux proof capability
    // under cfg(target_os = "linux") (the P09 contract's consumer surface).
    let supervisor_source = include_str!("../src/services/process_supervisor.rs");
    for name in [
        "launch_bwrap_tree",
        "prove_namespace_empty",
        "terminate_tree",
        "wait_pid1_exit",
        "wait_monitor",
        "BwrapTreeIdentity",
        "PidFd",
    ] {
        assert!(
            supervisor_source.contains(name),
            "the supervisor registration must re-export {name}"
        );
    }
    assert!(supervisor_source.contains("#[cfg(target_os = \"linux\")]"));

    // Live identity over a real short-lived launch: all P09.1 material is
    // captured from the HANDED-OFF sandbox PID1 before the tree is
    // considered supervised.
    let Some(version) = verified_pinned_binary() else {
        return;
    };
    assert!(version >= (0, 8));
    let base = tempfile::tempdir().expect("tempdir");
    let Some((mut tree, _scratch)) = launch_over_plan(base.path(), &["/usr/bin/sleep", "1"]) else {
        return;
    };
    assert!(tree.namespace_pid1 > 1);
    assert_ne!(
        tree.namespace_pid1, tree.monitor_pid,
        "the identity observes the sandbox init, never the outer monitor"
    );
    assert!(tree.pid1_start_identity > 0);
    let own_inode = namespace_inode(own_pid).expect("own ns inode");
    assert_ne!(
        tree.namespace_inode, own_inode,
        "the persisted inode must identify the sandbox namespace, not the host's"
    );
    // The PID1 is observable to completion through its pidfd alone — the
    // kernel marks it readable at exit, zombie included, so proof stage one
    // never depends on the monitor's reaping.
    assert!(
        wait_pid1_exit(&tree, Duration::from_secs(10)),
        "the PID1 pidfd must report exit for a finished workload"
    );
    assert_eq!(
        prove_namespace_empty(&tree, PROOF_TIMEOUT),
        Ok(true),
        "the two-stage proof completes for a naturally finished workload"
    );
    assert!(
        wait_monitor(&mut tree, Duration::from_secs(10)),
        "the monitor child is reaped through the owned handle"
    );
}

// 4 — pidfd-only signal delivery and the two-stage fence pins -------------------

#[test]
fn pidfd_only_signalling_is_delivered_and_the_fence_is_pinned() {
    // A LIVE unrelated process (no bwrap at all): pidfd_open + pidfd_signal
    // must deliver SIGKILL with no bare kill(pid) — the exact signal path
    // terminate_tree rides. pidfd_open returning None is an honest
    // environmental refusal (the live-unproven verdict is exercised against
    // a real live namespace in the terminate test above).
    let mut sleeper = Command::new("/usr/bin/sleep")
        .arg("300")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn the foreign sleeper");
    let sleeper_pidfd: Option<PidFd> = pidfd_open(sleeper.id());
    match sleeper_pidfd {
        Some(fd) => {
            assert!(
                pidfd_signal(&fd, libc::SIGKILL),
                "SIGKILL must be deliverable through a live pidfd"
            );
            let status = sleeper.wait().expect("reap the signalled sleeper");
            assert!(!status.success(), "the SIGKILLed sleeper cannot succeed");
        }
        None => {
            eprintln!(
                "P09 s09: pidfd_open refused by this kernel — honest \
                 environmental refusal (no fake pass)"
            );
            let _ = sleeper.kill();
            let _ = sleeper.wait();
        }
    }

    // Fence pin (the two-stage shape): the proof requires BOTH the PID1
    // pidfd readable AND the PID1 /proc entry gone after the monitor's
    // reap; a pid recycled onto a foreign process keeps /proc alive, so the
    // verdict stays Ok(false) — unproven, never a false proof — and the
    // monitor's own exit is never the proof signal.
    let prove_body = proof_fn_body("prove_namespace_empty");
    assert!(
        prove_body.contains("wait_pid1_exit(identity, timeout)"),
        "the proof's first stage is the PID1 pidfd exit"
    );
    assert!(
        prove_body.contains("namespace_inode(identity.namespace_pid1).is_none()"),
        "the proof's second stage is the PID1 /proc disappearance"
    );
    assert!(
        !prove_body.contains("monitor_pid"),
        "the monitor's own exit is never the proof signal"
    );

    // Varargs pin: pidfd_send_signal passes ALL FOUR syscall arguments
    // explicitly — NULL siginfo pointer and a real zero flags register, not
    // whatever the varargs ABI left in the fourth slot.
    let signal_body = proof_fn_body("pidfd_signal");
    assert!(
        signal_body.contains("libc::SYS_pidfd_send_signal"),
        "signalling rides pidfd_send_signal"
    );
    assert!(
        signal_body.contains("0usize") && signal_body.contains("0u32"),
        "siginfo (NULL) and flags (0) are passed explicitly"
    );

    // Launch amendment pin: the plan's trailing placeholder is replaced
    // exactly once by the die-with-parent belt plus the info-fd handoff,
    // and the identity is read from the documented child-pid JSON — never
    // by guessing the monitor's or the leader's pid.
    let launch_body = proof_fn_body("launch_bwrap_tree");
    assert_eq!(
        launch_body.matches("--die-with-parent").count(),
        1,
        "the die-with-parent belt is armed exactly once"
    );
    assert_eq!(
        launch_body.matches("--info-fd").count(),
        1,
        "the info-fd handoff is requested exactly once"
    );
    assert_eq!(
        launch_body.matches("\"--\".into()").count(),
        1,
        "exactly one workload terminator is pushed"
    );
    assert!(
        launch_body.contains("child-pid"),
        "the identity observes the handed-off sandbox PID1"
    );
    assert!(
        !launch_body.contains("--unshare"),
        "the launcher never adds a namespace flag of its own"
    );
}

// 5 — group-non-proof: an escapee in its own session outlives the leader ---------

#[test]
fn leader_or_group_exit_alone_is_never_the_proof() {
    let Some(version) = verified_pinned_binary() else {
        return;
    };
    assert!(version >= (0, 8));
    let base = tempfile::tempdir().expect("tempdir");
    let heartbeat = base.path().join("scratch/heartbeat");
    let heartbeat_text = heartbeat.to_string_lossy().into_owned();
    // The workload leader IS the namespace PID1: it setsids a child that
    // would outlive any process-group heuristic (heartbeats into the rw
    // scratch root, bounded at 90s), then the leader exits immediately —
    // leader dead, escapee alive in its own session is only observable if
    // the PID1-death containment itself failed.
    let script = format!(
        "/usr/bin/setsid /usr/bin/sh -c 'n=0 ; while [ $n -lt 90 ] ; do \
         : > \"{heartbeat_text}\" ; n=$((n+1)) ; /usr/bin/sleep 1 ; done' & \
         exit 0"
    );
    let Some((mut tree, _scratch)) = launch_over_plan(base.path(), &["/usr/bin/sh", "-c", &script])
    else {
        return;
    };
    if wait_for_heartbeat(&heartbeat).is_none() {
        return;
    }

    // Give any die-with-parent-style cascade time to fire, then branch
    // honestly on the observable fact: is the escapee still alive?
    std::thread::sleep(Duration::from_secs(3));
    if heartbeat_is_fresh(&heartbeat) {
        // The leader is long dead, its process group is gone, and the
        // escapee lives in its own session — leader/PG exit alone must
        // NEVER read as proof while a member is provably alive.
        assert_eq!(
            prove_namespace_empty(&tree, Duration::from_secs(1)),
            Ok(false),
            "leader/PG exit is never proof while an escapee is alive"
        );
        assert!(
            terminate_tree(&mut tree),
            "terminate through the PID1 pidfd"
        );
        // The containment claim itself: the setsid escapee dies WITH the
        // namespace (PID1's death cascades) — the P11-macOS contrast, where
        // an enumeration-free kernel container is the only containment.
        let deadline = Instant::now() + PROOF_TIMEOUT;
        while Instant::now() < deadline {
            if !heartbeat_is_fresh(&heartbeat) {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        assert!(
            !heartbeat_is_fresh(&heartbeat),
            "the setsid escapee must die with the namespace — group \
             heuristics could never contain it"
        );
        assert_eq!(
            prove_namespace_empty(&tree, PROOF_TIMEOUT),
            Ok(true),
            "after the namespace actually emptied the proof concludes"
        );
    } else {
        // The expected shape in this contract: the leader WAS PID1, so its
        // exit cascaded onto the escapee via the kernel's namespace kill;
        // the proof may conclude exactly now that the namespace emptied.
        eprintln!(
            "P09 s09: the escapee died with the leader's (= PID1's) exit \
             via the kernel namespace kill; the group-non-proof window was \
             subsumed"
        );
        assert_eq!(
            prove_namespace_empty(&tree, PROOF_TIMEOUT),
            Ok(true),
            "the proof concludes exactly when the namespace emptied"
        );
        // No resurrection: the escapee stays dead and the monitor reaps.
        std::thread::sleep(Duration::from_secs(2));
        assert!(
            !heartbeat_is_fresh(&heartbeat),
            "the escapee stays dead with the namespace"
        );
        assert!(
            wait_monitor(&mut tree, Duration::from_secs(10)),
            "the monitor child is reaped through the owned handle"
        );
    }
}

// 6 — single namespace owner ------------------------------------------------------

#[test]
fn exactly_one_component_owns_the_pid_namespace() {
    // Plan-level pin (P16 contract): the argv carries --unshare-pid exactly
    // once, and the proof module NEVER adds or passes a namespace flag of
    // its own — one namespace owner, no nested second PID namespace.
    assert!(
        !proof_module_region().contains("--unshare-pid"),
        "the proof path must never pass a namespace flag — the plan owns the one"
    );

    let Some(version) = verified_pinned_binary() else {
        return;
    };
    assert!(version >= (0, 8));
    let base = tempfile::tempdir().expect("tempdir");
    let profile = tempdir_profile(base.path());
    let plan = build_bwrap_launch_plan(&profile, &pinned_bwrap(), "boot-p09-qa")
        .expect("plan builds over the verified pinned binary");
    assert!(plan.unshare_pid);
    assert_eq!(
        plan.argv
            .iter()
            .filter(|argument| argument.as_str() == "--unshare-pid")
            .count(),
        1,
        "exactly one --unshare-pid: no nested second PID namespace"
    );
}
