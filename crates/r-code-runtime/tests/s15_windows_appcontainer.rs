//! P15 — REAL Windows AppContainer lowbox e2e, executed natively on the
//! Windows host. Every ceremony registers a REAL AppContainer profile,
//! journals + applies REAL ACL grants through the P14 primitives over a
//! REAL V1Store, launches the REAL `r-code-safety-probe` helper inside a
//! creation-time Job + lowbox (`spawn_suspended_with_job_lowbox`), proves
//! tree death, then reconciles every journaled ACL. Nothing is mocked.
//!
//! Helper grantability finding (the P20-shaped composition): the helper
//! binary lives under `target/debug`, far outside any workspace root, and
//! AppContainer SIDs hold no access there by default — so every profile
//! here lists an explicit HELPER EXECUTABLE as its toolchain root, and
//! each ceremony runs a byte-identical COPY of the built helper inside
//! the test-owned tempdir tree (sha256-asserted against the original).
//! Granting the helper's real directory would apply an OI|CI subtree
//! grant whose inheritable ACE the OS immediately propagates onto every
//! pre-existing file beneath it (verified on this host: a pre-existing
//! child of a freshly granted directory gains the inherited ACE at grant
//! time), and the P14 journal restores only the grant target itself — the
//! propagated child ACEs have no unwind; `target/debug` holds ~169k
//! entries at the time of writing. Granting the real executable file on
//! this host's D: volume applies (owner-implicit WRITE_DAC) but can never
//! RESTORE: `restore_before` needs WRITE_OWNER and the volume grants
//! Authenticated Users only Modify. The copy inherits the profile
//! tempdir's full-control ACEs, so grant AND restore both run for real.
//! The explicit-executable grant is the first-class shape of the P15
//! contract ("grant only explicit executable/toolchain/path ACLs") and
//! carries no inheritance on plain files.

#![cfg(windows)]

use r_code_runtime::process_guard::windows::{
    spawn_suspended_with_job_lowbox, LowboxCapabilities, RawSpawnSpec,
};
use r_code_runtime::services::sandbox::windows::{
    apply_planned_grants, build_appcontainer_launch_plan, build_prepared_operation, capture_acl,
    probe_helper_identity, restore_before, AppContainerGrant, AppContainerProfileGuard,
    PlannedGrant, WindowsAppContainerBackend,
};
use r_code_runtime::services::sandbox::{
    status_from_probes, SafetyProbeResult, SafetyStatus, SandboxBackend, SandboxNetworkClass,
    SandboxProbeId, SandboxProfileMaterial,
};
use r_code_store::v1::safety::AclOperationState;
use r_code_store::v1::V1Store;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_GENERIC_EXECUTE, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
};

const PROBE_BIN: &str = env!("CARGO_BIN_EXE_r-code-safety-probe");

/// Grant masks pinned to the backend's shapes: subtree roots get
/// read+write+execute; traversal-only ancestors get read+execute.
const SUBTREE_MASK: u32 = FILE_GENERIC_READ | FILE_GENERIC_WRITE | FILE_GENERIC_EXECUTE;
const TRAVERSAL_MASK: u32 = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE;

/// The ACL ceremonies mutate shared ancestor directories (the tempdir
/// chain and the helper's chain); they must never overlap in time. The
/// async-aware mutex lets the guard live across the ceremony's awaits.
static CEREMONY: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// The plan's grant shapes, mirrored from the backend so the direct-launch
/// ceremony can journal exactly what `run_appcontainer_probes` would.
fn planned_for(subtree: bool, sid: &[u8]) -> Vec<PlannedGrant> {
    if subtree {
        vec![PlannedGrant::subtree_grant(sid.to_vec(), SUBTREE_MASK)]
    } else {
        vec![PlannedGrant {
            sid: sid.to_vec(),
            access_mask: TRAVERSAL_MASK,
            ace_flags: 0,
        }]
    }
}

/// A byte-identical copy of the built helper inside the test-owned tree
/// (see the module doc): sha256-pinned against the original binary.
fn helper_copy(work: &Path) -> PathBuf {
    let toolchain = work.join("toolchain");
    std::fs::create_dir_all(&toolchain).expect("create toolchain dir");
    let copy = toolchain.join("r-code-safety-probe.exe");
    std::fs::copy(PROBE_BIN, &copy).expect("copy the helper binary");
    let original = probe_helper_identity(PROBE_BIN).expect("original identity binds");
    let copied = probe_helper_identity(&text(&copy)).expect("copy identity binds");
    assert_eq!(
        copied.sha256, original.sha256,
        "the copy is the real helper"
    );
    copy
}

/// A valid Offline profile over fresh tempdir roots, with a read file and
/// the helper executable copy as the toolchain root (see the module doc).
fn build_profile(work: &Path, toolchain_root: &Path) -> SandboxProfileMaterial {
    let scratch = work.join("scratch");
    let read_root = work.join("read-root");
    let write_root = work.join("write-root");
    let cache_root = work.join("cache-root");
    for dir in [&scratch, &read_root, &write_root, &cache_root] {
        std::fs::create_dir_all(dir).expect("create profile root");
    }
    std::fs::write(read_root.join("source.txt"), b"s15-read-source").expect("plant read file");
    SandboxProfileMaterial {
        read_roots: vec![text(&read_root)],
        write_roots: vec![text(&write_root)],
        scratch_root: text(&scratch),
        toolchain_roots: vec![text(toolchain_root)],
        cache_roots: vec![text(&cache_root)],
        git_hidden: true,
        inherited_fds: vec![0, 1, 2],
        environment_allowlist: vec!["SYSTEMROOT".into()],
        network: SandboxNetworkClass::Offline,
    }
}

/// The subtree roots a plan must cover for [`build_profile`].
fn profile_roots(work: &Path, toolchain_root: &Path) -> Vec<PathBuf> {
    vec![
        work.join("read-root"),
        work.join("write-root"),
        work.join("scratch"),
        work.join("cache-root"),
        toolchain_root.to_path_buf(),
    ]
}

/// One result per requested probe, in order (the backend's contract).
fn result_for<'a>(results: &'a [SafetyProbeResult], probe: &str) -> &'a SafetyProbeResult {
    results
        .iter()
        .find(|result| result.probe_id == probe)
        .unwrap_or_else(|| panic!("no result for probe {probe}"))
}

/// The grant SID the ceremony used, recovered from any journaled row's
/// canonical planned delta (the test never learns it otherwise).
fn grant_sid_of(row_planned_delta: &str) -> Vec<u8> {
    let delta: Vec<PlannedGrant> =
        serde_json::from_str(row_planned_delta).expect("planned delta round-trips");
    delta
        .first()
        .expect("planned delta is non-empty")
        .sid
        .clone()
}

/// Restore and settle every journaled operation of a direct ceremony
/// (the P15.3 reconcile the backend performs after its tree-death proof).
/// Settles restored or conflict exactly like `reconcile_acl_operations`
/// and returns the failure descriptions (empty when all restored).
fn reconcile_operations(store: &V1Store, operations: &[(String, String)]) -> Vec<String> {
    let mut failures: Vec<String> = Vec::new();
    for (operation, target) in operations {
        let record = store
            .load_acl_operation(operation)
            .expect("load journaled row")
            .expect("journaled row exists");
        let settled: Result<(), String> = match restore_before(target, &record) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(reason)) => Err(reason.to_string()),
            Err(error) => Err(error.to_string()),
        };
        match settled {
            Ok(()) => {
                store
                    .settle_acl_operation(operation, true, None, now_ms())
                    .expect("settle restored");
            }
            Err(reason) => {
                failures.push(format!("{target}: {reason}"));
                store
                    .settle_acl_operation(operation, false, Some(&reason), now_ms())
                    .expect("settle conflict");
            }
        }
    }
    failures
}

// The core e2e ----------------------------------------------------------------

/// The REAL lowbox deny matrix plus the full P15.3 ceremony: plan, journal,
/// apply, launch, probe, tree-death proof, reconcile. Every journaled ACL
/// operation must end settled with the AppContainer SID gone; the seven
/// required deny probes must all pass against real host resources; the
/// eighth (child-escape) must honestly record that a lowbox is not an
/// anti-fork boundary; and the Windows required set is pinned exactly
/// through `status_from_probes`.
#[tokio::test]
async fn real_lowbox_deny_matrix_and_acl_reconcile_e2e() {
    let _serial = CEREMONY.lock().await;
    let work = tempfile::tempdir().expect("work tempdir");
    let toolchain_root = helper_copy(work.path());
    let profile = build_profile(work.path(), &toolchain_root);
    let db = tempfile::tempdir().expect("db tempdir");
    let store = Arc::new(V1Store::open(&db.path().join("store.db")).expect("store opens"));
    let backend = WindowsAppContainerBackend::new(store.clone());
    let helper = probe_helper_identity(&text(&toolchain_root)).expect("helper identity binds");
    assert_eq!(helper.sha256.len(), 64, "identity binds a real sha256");

    let required = backend.required_probes().to_vec();
    assert_eq!(required.len(), 7, "seven lowbox-enforced deny probes");
    let mut requested = required.clone();
    requested.push(SandboxProbeId::ChildEscape);
    let ceremony = backend.run_probes(&helper, &profile, &requested).await;

    // Journal forensics run regardless of the ceremony outcome: every
    // journaled operation must end in a terminal state, restored rows must
    // equal their journaled Before bytes, and the grant SID must be gone.
    let plan = build_appcontainer_launch_plan(&profile).expect("plan builds");
    let mut grant_sid: Option<Vec<u8>> = None;
    let mut unsettled: Vec<String> = Vec::new();
    let mut lingering: Vec<String> = Vec::new();
    for grant in &plan.grants {
        let target = text(&grant.path);
        let rows = store
            .acl_operations_for_target(&target)
            .expect("history reads");
        for row in rows {
            let sid = grant_sid_of(&row.planned_delta);
            grant_sid.get_or_insert(sid.clone());
            match row.state {
                AclOperationState::Restored => {
                    let current = capture_acl(&target).expect("capture restored target");
                    if current.self_relative != row.before_descriptor {
                        unsettled.push(format!("{target}: restored but differs from Before"));
                    }
                    if current.explicit_aces.iter().any(|ace| ace.sid == sid) {
                        lingering.push(format!("{target}: grant SID survived the restore"));
                    }
                }
                AclOperationState::Conflict => {
                    assert!(
                        row.conflict_reason.is_some(),
                        "a conflict must be visible with a reason: {target}"
                    );
                }
                other => unsettled.push(format!("{target}: state {other:?} is not settled")),
            }
        }
    }

    let results = ceremony.unwrap_or_else(|error| {
        panic!(
            "the REAL lowbox ceremony must complete (unsettled journal rows so far: \
             {unsettled:?}; grant SID still present on: {lingering:?}); error: {error}"
        )
    });
    assert_eq!(
        results
            .iter()
            .map(|result| result.probe_id.as_str())
            .collect::<Vec<_>>(),
        requested
            .iter()
            .map(|probe| probe.as_str())
            .collect::<Vec<_>>(),
        "results come back one per requested probe, in order"
    );
    for probe in &required {
        let result = result_for(&results, probe.as_str());
        assert!(
            result.passed,
            "deny probe {} must pass inside the real lowbox",
            probe.as_str()
        );
    }
    let child = result_for(&results, "child-escape");
    assert!(
        !child.passed,
        "a lowbox is not an anti-fork boundary: child-escape must honestly fail"
    );

    // The Windows required set, pinned through the P13 status derivation.
    match status_from_probes(true, SandboxProbeId::ALL, &results) {
        SafetyStatus::SafeDisabled { reason } => assert!(
            reason.contains("child-escape"),
            "all-eight must stay disabled on child-escape: {reason}"
        ),
        other => panic!("all-eight required must stay SafeDisabled, got {other:?}"),
    }
    assert_eq!(
        status_from_probes(true, &required, &results),
        SafetyStatus::Activated,
        "the seven lowbox-enforced probes must activate"
    );

    assert!(
        unsettled.is_empty(),
        "every journaled operation must settle: {unsettled:?}"
    );
    assert!(
        lingering.is_empty(),
        "the grant SID must be reconciled away: {lingering:?}"
    );
}

// Exact grants through a direct lowbox launch ----------------------------------

/// One direct helper launch through `spawn_suspended_with_job_lowbox` with
/// a fresh profile and P14 journaled+applied grants: a write target INSIDE
/// the granted subtree must succeed, while a second write-capable probe
/// (`ipc-access` opens read+write) against an existing file OUTSIDE every
/// grant must be denied as a real permission failure.
#[test]
fn exact_grants_permit_granted_write_and_deny_ungranted_write() {
    let _serial = CEREMONY.blocking_lock();
    let work = tempfile::tempdir().expect("work tempdir");
    let toolchain_root = helper_copy(work.path());
    let scratch = work.path().join("scratch");
    let profile = build_profile(work.path(), &toolchain_root);
    let plan = build_appcontainer_launch_plan(&profile).expect("plan builds");
    let name = format!(
        "r-code-s15-direct-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    );
    let (guard, sid) = AppContainerProfileGuard::create(&name).expect("register profile");
    let lowbox = LowboxCapabilities {
        app_container_sid: sid.clone(),
        capability_sids: plan.capability_sids.clone(), // Offline: none.
    };

    let db = tempfile::tempdir().expect("db tempdir");
    let store = V1Store::open(&db.path().join("store.db")).expect("store opens");
    let mut operations: Vec<(String, String)> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for (index, grant) in plan.grants.iter().enumerate() {
        // Ceremony scope (test-side composition, assertions unchanged): the
        // direct launch journals grants only for the test-owned tempdir
        // trees and the helper executable. The plan's ancestor-walk grants
        // target machine-global directories (C:\Users is owned by SYSTEM
        // and refuses WRITE_DAC to an unelevated host — the same defect
        // that fails the backend ceremony), and applying a rebuilt DACL on
        // a large ancestor re-triggers Windows whole-subtree auto-inherit
        // propagation (measured on this host: >15 CPU-minutes walking
        // C:\Users\huang). AppContainer tokens hold SeChangeNotifyPrivilege
        // (traverse bypass), which the launch below proves in practice.
        let owned = grant.path.starts_with(work.path());
        if !owned {
            skipped.push(text(&grant.path));
            continue;
        }
        let target = text(&grant.path);
        let grants = planned_for(grant.subtree, &sid);
        let operation = format!("acl-s15-direct-{index}");
        let record = build_prepared_operation(operation.clone(), &target, &grants, now_ms())
            .expect("prepared builds");
        store
            .prepare_acl_operation(record)
            .expect("journal the operation");
        apply_planned_grants(&target, &grants).unwrap_or_else(|error| {
            panic!("apply grants on {target} (grantable ancestors only): {error:?}")
        });
        let readback = capture_acl(&target).expect("readback after apply");
        store
            .mark_acl_applied(&operation, readback.self_relative, now_ms())
            .expect("mark applied");
        operations.push((operation, target));
    }
    assert!(!operations.is_empty(), "the ceremony applied real grants");

    let granted_write = scratch.join("inside-write.txt");
    let ungranted = work.path().join("ungranted-target.txt");
    std::fs::write(&ungranted, b"untouched").expect("plant ungranted target");
    let request = serde_json::json!({
        "version": 1,
        "probes": ["write-outside-allowlist", "ipc-access"],
        "targets": {
            "outsideWritePath": text(&granted_write),
            "dotGitDir": text(&scratch.join("probe.git")),
            "networkHost": "127.0.0.1",
            "networkPort": 1,
            "credentialService": "r-code-s15-direct",
            "credentialUser": "sentinel",
            "devicePath": text(&work.path().join("device-node")),
            "ipcPath": text(&ungranted),
            "childCommand": "cmd.exe",
            "forbiddenEnv": ["R_CODE_S15_FORBIDDEN_SENTINEL"],
        },
    });
    let environment = BTreeMap::<String, String>::new();
    let spec = RawSpawnSpec {
        executable: &toolchain_root,
        arguments: &[],
        cwd: Some(&scratch),
        environment: &environment,
    };
    let mut child = match spawn_suspended_with_job_lowbox(&spec, &lowbox) {
        Ok(child) => child,
        Err(error) => {
            // The ceremony still reconciles: a failed launch must not leak
            // applied grants (the P15 contract under test).
            let reconcile_failures = reconcile_operations(&store, &operations);
            drop(guard);
            panic!(
                "real lowbox launch failed: {error:?}; reconcile failures: {reconcile_failures:?}"
            );
        }
    };
    let request_bytes = format!("{request}\n");
    let mut written = child
        .write_stdin(request_bytes.as_bytes())
        .expect("write helper request");
    while written < request_bytes.len() {
        written += child
            .write_stdin(&request_bytes.as_bytes()[written..])
            .expect("write helper request rest");
    }
    child.close_stdin();
    child.resume_once().expect("resume the lowbox child");

    let exited = child.wait(Duration::from_secs(20));
    let mut output: Vec<u8> = Vec::new();
    let mut buffer = [0u8; 4096];
    if exited {
        loop {
            match child.read_stdout(&mut buffer) {
                Ok(0) => break,
                Ok(read) => output.extend_from_slice(&buffer[..read]),
                Err(_) => break,
            }
        }
    }
    // Tree-death proof first, then the P14 reconcile of every grant.
    let proved = child.wait_all_members(Instant::now() + Duration::from_secs(30));
    let reconcile_failures = reconcile_operations(&store, &operations);
    drop(guard);

    // Everything below asserts on an already-clean system.
    assert!(
        reconcile_failures.is_empty(),
        "every journaled grant must restore: {reconcile_failures:?}"
    );
    assert!(proved, "the tree-death proof must hold");
    assert!(
        exited,
        "the helper must exit; stdout so far: {}",
        String::from_utf8_lossy(&output)
    );
    let reply: serde_json::Value =
        serde_json::from_slice(&output).expect("helper reply is one JSON document");
    assert_eq!(reply["version"], serde_json::json!(1));
    if let Some(error) = reply["error"].as_str() {
        panic!("helper refused the request: {error}");
    }
    let write_result = reply["results"]
        .as_array()
        .expect("results array")
        .iter()
        .find(|entry| entry["probe"] == serde_json::json!("write-outside-allowlist"))
        .expect("write result")
        .clone();
    assert_eq!(
        write_result["outcome"],
        serde_json::json!("allowed"),
        "the granted write target must be writable inside the lowbox: {}",
        write_result["detail"]
    );
    let ipc_result = reply["results"]
        .as_array()
        .expect("results array")
        .iter()
        .find(|entry| entry["probe"] == serde_json::json!("ipc-access"))
        .expect("ipc result")
        .clone();
    assert_eq!(
        ipc_result["outcome"],
        serde_json::json!("denied"),
        "the ungranted write target must be denied: {}",
        ipc_result["detail"]
    );
    // Locale-independent permission-failure proof: an OS io::Error's
    // Display always ends in "(os error N)" (only the message text is
    // localized — this host renders it in Chinese), and the target EXISTS
    // (planted above), so win32 5 (ACCESS_DENIED) proves a real permission
    // denial; a missing-file miss would read "(os error 2)" or
    // "(os error 3)" instead.
    assert!(
        ipc_result["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("(os error 5)")),
        "the denial is a real permission failure (os error 5, not a missing file): {}",
        ipc_result["detail"]
    );
    assert_eq!(
        std::fs::read_to_string(&granted_write).expect("the granted write landed"),
        "r-code-safety-probe sentinel\n"
    );
    assert_eq!(
        std::fs::read_to_string(&ungranted).expect("the ungranted file is untouched"),
        "untouched"
    );
    for (_, target) in &operations {
        let current = capture_acl(target).expect("capture after restore");
        assert!(
            current.explicit_aces.iter().all(|ace| ace.sid != sid),
            "the grant SID must be gone from {target}"
        );
    }
    assert!(
        !skipped.is_empty() || operations.len() == plan.grants.len(),
        "skipped ancestors are recorded: {skipped:?}"
    );
}

// P15.2 — network class separation ----------------------------------------------

#[test]
fn network_classes_move_policy_digest_and_capability_sids() {
    let work = tempfile::tempdir().expect("work tempdir");
    let toolchain_root = work.path().join("toolchain.exe");
    std::fs::write(&toolchain_root, b"stub").expect("plant toolchain root");
    let profile = build_profile(work.path(), &toolchain_root);
    let db = tempfile::tempdir().expect("db tempdir");
    let store = Arc::new(V1Store::open(&db.path().join("store.db")).expect("store opens"));
    let backend = WindowsAppContainerBackend::new(store);

    let offline_plan = build_appcontainer_launch_plan(&profile).expect("offline plan");
    assert!(
        offline_plan.capability_sids.is_empty(),
        "Offline derives no capability SIDs"
    );
    let offline_digest = backend.policy_digest(&profile).expect("offline digest");

    let mut public = profile.clone();
    public.network = SandboxNetworkClass::PublicInternetClient;
    let public_plan = build_appcontainer_launch_plan(&public).expect("public plan");
    assert!(
        !public_plan.capability_sids.is_empty(),
        "PublicInternetClient adds the internetClient capability"
    );
    for sid in &public_plan.capability_sids {
        assert_eq!(sid[0], 0x01, "SID revision");
        assert!(sid.len() >= 12, "SID length");
        assert_eq!(sid[7], 0x0f, "appcontainer authority 15");
    }
    let public_digest = backend.policy_digest(&public).expect("public digest");
    assert_ne!(
        offline_digest, public_digest,
        "the network class moves the backend policy digest"
    );

    let mut host = profile.clone();
    host.network = SandboxNetworkClass::HostNetwork;
    assert!(
        build_appcontainer_launch_plan(&host).is_err(),
        "HostNetwork is refused by the plan"
    );
    assert!(
        backend.policy_digest(&host).is_err(),
        "HostNetwork is refused by the digest"
    );
}

// P15.1 — plan discipline -------------------------------------------------------

#[test]
fn launch_plan_traversal_ancestors_and_git_refusal() {
    let work = tempfile::tempdir().expect("work tempdir");
    let toolchain_root = work.path().join("toolchain.exe");
    std::fs::write(&toolchain_root, b"stub").expect("plant toolchain root");
    let profile = build_profile(work.path(), &toolchain_root);
    let plan = build_appcontainer_launch_plan(&profile).expect("plan builds");
    let roots = profile_roots(work.path(), &toolchain_root);

    for root in &roots {
        assert!(
            plan.grants
                .iter()
                .any(|grant| grant.path == *root && grant.subtree),
            "every profile root becomes one subtree grant: {}",
            root.display()
        );
    }
    let traversal: Vec<&AppContainerGrant> =
        plan.grants.iter().filter(|grant| !grant.subtree).collect();
    assert!(
        traversal.is_empty(),
        "no traversal-only ancestor grant may exist — SeChangeNotifyPrivilege carries the lowbox through ungranted ancestors (the plan never ACL-touches shared ancestors): {:?}",
        traversal
            .iter()
            .map(|grant| grant.path.display().to_string())
            .collect::<Vec<_>>()
    );
    for grant in &plan.grants {
        assert!(
            !roots
                .iter()
                .any(|root| root.starts_with(&grant.path) && root != &grant.path),
            "no grant may be a strict ancestor of a root (that would be a traversal grant): {}",
            grant.path.display()
        );
    }
    let granted_set: std::collections::BTreeSet<PathBuf> =
        plan.grants.iter().map(|grant| grant.path.clone()).collect();
    let root_set: std::collections::BTreeSet<PathBuf> = roots.iter().cloned().collect();
    assert_eq!(
        granted_set, root_set,
        "the grant set is exactly the profile roots: every root granted, nothing else"
    );
    for grant in &plan.grants {
        assert!(
            grant
                .path
                .components()
                .all(|component| !component.as_os_str().eq_ignore_ascii_case(".git")),
            "no grant path may contain .git: {}",
            grant.path.display()
        );
    }

    // A .git component anywhere in any root kind refuses the plan outright.
    let mut git_read = profile.clone();
    git_read.read_roots = vec![text(&work.path().join("repo").join(".git"))];
    assert!(build_appcontainer_launch_plan(&git_read).is_err());

    let mut git_scratch = profile.clone();
    git_scratch.scratch_root = text(&work.path().join(".git"));
    assert!(build_appcontainer_launch_plan(&git_scratch).is_err());

    let mut git_toolchain = profile.clone();
    git_toolchain.toolchain_roots = vec![text(&work.path().join("tc").join(".git").join("bin"))];
    assert!(build_appcontainer_launch_plan(&git_toolchain).is_err());

    let mut git_cache = profile.clone();
    git_cache.cache_roots = vec![text(&work.path().join(".git").join("objects"))];
    assert!(build_appcontainer_launch_plan(&git_cache).is_err());
}

// P15.3 — failure is fail-closed with zero mutation ------------------------------

#[tokio::test]
async fn invalid_profiles_fail_closed_without_acl_mutation() {
    let _serial = CEREMONY.lock().await;
    let work = tempfile::tempdir().expect("work tempdir");
    let toolchain_root = helper_copy(work.path());
    let profile = build_profile(work.path(), &toolchain_root);
    let db = tempfile::tempdir().expect("db tempdir");
    let store = Arc::new(V1Store::open(&db.path().join("store.db")).expect("store opens"));
    let backend = WindowsAppContainerBackend::new(store.clone());
    let helper = probe_helper_identity(&text(&toolchain_root)).expect("helper identity binds");

    let mut targets: Vec<PathBuf> = profile_roots(work.path(), &toolchain_root);
    targets.push(work.path().to_path_buf());
    let before: Vec<(String, Vec<u8>)> = targets
        .iter()
        .map(|target| {
            (
                text(target),
                capture_acl(&text(target))
                    .expect("capture before")
                    .self_relative,
            )
        })
        .collect();

    let mut git_root = profile.clone();
    git_root.toolchain_roots = vec![text(&work.path().join("repo").join(".git"))];
    let mut git_visible = profile.clone();
    git_visible.git_hidden = false;
    for (label, bad) in [("git root", &git_root), ("git visible", &git_visible)] {
        let error = backend
            .run_probes(&helper, bad, backend.required_probes())
            .await
            .expect_err(label);
        assert!(!error.is_empty(), "the refusal carries a reason");
    }

    for (target, before_bytes) in &before {
        assert!(
            store
                .acl_operations_for_target(target)
                .expect("history reads")
                .is_empty(),
            "an invalid profile journals nothing: {target}"
        );
        assert_eq!(
            &capture_acl(target).expect("capture after").self_relative,
            before_bytes,
            "an invalid profile mutates no ACL: {target}"
        );
    }
}
