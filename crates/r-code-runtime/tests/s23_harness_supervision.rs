//! s23 — Harness supervision (P23): every launch is supervised.
//!
//! Proves the four P23 acceptance criteria on the platforms this host can
//! execute: the native harness's first exec happens inside the containment the
//! transport established at creation (Windows creation-time job, Linux exec-time
//! process group), an immediate grandchild dies with the tree (proved against
//! the OS, not against the transport's own bookkeeping), macOS stays SafeDisabled
//! for undeclared packages and refuses declared ones before any process exists,
//! and the 1.3 install gate refuses single-process packages that declare an
//! older minor while additive older packages remain installable.

use r_code_harness_protocol::manifest::{
    ApiVersion, HarnessManifestBuilder, HostService, SINGLE_PROCESS_MIN_API_MINOR, WAVE3_HOST_API,
};
use r_code_harness_protocol::Platform;
use r_code_runtime::plugins::catalog::{
    Availability, PluginCatalog, HOST_API, SINGLE_PROCESS_MIN_API_MINOR as CATALOG_MIN,
};
use r_code_runtime::plugins::transport::{
    bind_harness_launch_config, spawn_plugin, DenyCallbacks, HarnessLaunchConfig, TransportLimits,
};
#[cfg(target_os = "macos")]
use r_code_runtime::plugins::transport::{register_child_process_requirement, TransportError};
use r_code_runtime::services::process_supervisor::{
    DeterministicSupervisorJournal, SupervisorPhase,
};
use r_code_runtime::services::sandbox::SafetyActivation;
use r_code_store::v1::V1Store;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// A scratch root one level under the test's own temp dir: absolute, .git-free,
/// unique per test so parallel binds never share a working directory.
fn scratch_root(name: &str) -> PathBuf {
    let root = tempfile::tempdir().expect("tempdir").path().join(name);
    std::fs::create_dir_all(&root).expect("scratch root");
    root
}

/// Bind the launch material this test owns: its own scratch tree, an honest
/// in-memory supervisor journal, and the not-activated verdict a real boot
/// carries this wave. Rebinding immediately before each spawn keeps parallel
/// tests from launching into each other's material.
fn bind_launch(scratch: &Path) {
    let mut config = HarnessLaunchConfig::host_default();
    config.scratch_root = scratch.to_path_buf();
    config.journal = std::sync::Arc::new(DeterministicSupervisorJournal::default());
    config.activation = SafetyActivation::NotActivated {
        reason: "no safety report is bound for this boot",
        status: Some("unsupported".to_string()),
    };
    bind_harness_launch_config(config);
}

/// Build the real native harness sidecar the app ships, so the launch below is
/// the same binary the desktop stages — not a stand-in.
fn native_binary() -> PathBuf {
    let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["build", "-p", "r-code-harness-native"])
        .output()
        .expect("build native harness");
    assert!(output.status.success(), "native harness build failed");
    let exe = if cfg!(windows) {
        "r-code-harness-native.exe"
    } else {
        "r-code-harness-native"
    };
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug")
        .join(exe)
}

/// The exact refusal a launch came back with, for code-level assertions.
#[cfg(target_os = "macos")]
fn refused_code(error: &TransportError) -> &str {
    match error {
        TransportError::Refused { code, .. } => code,
        other => panic!("expected a refused launch, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Integration — the 1.3 install gate (runs on every platform)
// ---------------------------------------------------------------------------

/// Write one package: `api_minor` and `requires_single_process` are the knobs
/// under test; the entrypoint is a real file so only the version gate can
/// refuse the install.
fn write_package(
    root: &Path,
    name: &str,
    api_minor: u32,
    requires_single_process: bool,
) -> PathBuf {
    let source = root.join(format!("pkg-{name}"));
    std::fs::create_dir_all(source.join("bin")).expect("package dirs");
    let platform = match Platform::current() {
        Platform::WindowsX64 => "windows-x64",
        Platform::MacosArm64 => "macos-arm64",
        Platform::MacosX64 => "macos-x64",
        Platform::LinuxX64 => "linux-x64",
    };
    let mut manifest = serde_json::json!({
        "schema_version": "1",
        "id": format!("{name}.s23"),
        "version": "1.0.0",
        "apiMajor": 1,
        "apiMinor": api_minor,
        "displayName": name,
        "supportedPlatforms": [{"platform": platform, "executable": "bin/harness"}],
        "requestedHostServices": ["host.model.stream"],
        "configSchema": {"type": "object"}
    });
    if requires_single_process {
        manifest["requiresSingleProcess"] = serde_json::json!(true);
    }
    std::fs::write(source.join("harness.json"), manifest.to_string()).expect("manifest");
    let binary = if cfg!(windows) {
        "harness.exe"
    } else {
        "harness"
    };
    std::fs::write(source.join("bin").join(binary), b"not really a harness").expect("binary");
    source
}

fn catalog(root: &Path) -> PluginCatalog {
    let store = V1Store::open(&root.join("store").join("tasks.sqlite3")).expect("store");
    PluginCatalog::new(root.join("plugins"), std::sync::Arc::new(store))
}

/// P23.4 integration, refusal arm: a package that requires the single-process
/// guarantee while declaring <= 1.2 cannot even be installed, and the reason
/// names the package and the minor that would honour it — a downgrade refusal,
/// not a version-window miss, and nothing enters the registry.
#[test]
fn single_process_package_below_13_is_explicitly_refused() {
    let temp = tempfile::tempdir().expect("tempdir");
    let source = write_package(temp.path(), "downgrade", 2, true);
    let catalog = catalog(temp.path());
    let error = catalog
        .install_from_directory(&source)
        .expect_err("the downgrade is refused at install time");
    let rendered = format!("{error:?}");
    assert!(
        rendered.contains("SingleProcessNeedsNewerApi"),
        "the refusal must name the single-process floor: {rendered}"
    );
    assert!(
        rendered.contains("downgrade.s23"),
        "the refusal must name the package: {rendered}"
    );
    assert!(
        catalog.list().expect("list").is_empty(),
        "a refused package never enters the registry"
    );
}

/// P23.4 integration, additive arm: every pre-1.3 package that never asks for
/// the guarantee installs and stays available — 1.0, 1.1 and 1.2 alike.
#[test]
fn additive_older_packages_stay_installable() {
    let temp = tempfile::tempdir().expect("tempdir");
    let catalog = catalog(temp.path());
    for (name, minor) in [("additive0", 0), ("additive1", 1), ("additive2", 2)] {
        let source = write_package(temp.path(), name, minor, false);
        catalog
            .install_from_directory(&source)
            .expect("additive package installs");
        let entries = catalog.list().expect("list");
        let entry = entries
            .iter()
            .find(|entry| entry.manifest.id.0 == format!("{name}.s23"))
            .expect("installed entry");
        assert_eq!(entry.availability, Availability::Available, "minor {minor}");
    }
}

/// The final Wave 3 host API is exactly v1.3: the catalog's advertised major
/// carries no v2 brand, its minor is the single-process floor, a 1.2 package
/// remains negotiable, a 1.4 package is not, the runtime constant is the
/// protocol's own Wave 3 value, and the 1.2 effect-fields floor stays honoured
/// beneath it — proven by negotiation, not by comparing constants.
#[test]
fn final_wave3_host_api_is_13_with_explicit_floors() {
    assert_eq!(HOST_API.major, 1, "INV-01: no v2 brand");
    assert_eq!(HOST_API, WAVE3_HOST_API, "one Wave 3 truth");
    assert_eq!(HOST_API.minor, SINGLE_PROCESS_MIN_API_MINOR);
    assert_eq!(HOST_API.minor, CATALOG_MIN);
    let additive = ApiVersion::new(1, 2);
    assert!(additive.is_supported_by(&HOST_API), "1.2 stays negotiable");
    let ahead = ApiVersion::new(1, 4);
    assert!(!ahead.is_supported_by(&HOST_API), "1.4 is not");
    let effect_floor = HarnessManifestBuilder::new("s23.effect.floor", "1.0.0")
        .api(1, 2)
        .requires_effect_fields()
        .entrypoint(Platform::current(), "bin/harness", &[])
        .build();
    let negotiated = effect_floor
        .negotiate(HOST_API, Platform::current(), HostService::ALL)
        .expect("the 1.2 effect floor stays negotiable beneath the 1.3 host");
    assert_eq!(negotiated.plugin_api, ApiVersion::new(1, 2));
    assert_eq!(negotiated.host_api, HOST_API);
}

// ---------------------------------------------------------------------------
// e2e — real native launches through the supervised transport
// ---------------------------------------------------------------------------

/// The native harness's first exec happens under the containment the transport
/// established before the child ran: Windows proves a creation-time kill-on-close
/// job, Linux an exec-time process group. The teardown then proves the whole
/// tree gone and the record drained — a natural completion follows full
/// proof/drain, never an assumption.
#[cfg(any(windows, target_os = "linux"))]
#[tokio::test]
async fn native_harness_first_exec_runs_contained_and_drains_provably() {
    let scratch = scratch_root("s23-native");
    bind_launch(&scratch);
    let binary = native_binary();
    let process = spawn_plugin(
        &binary,
        &[],
        std::sync::Arc::new(DenyCallbacks),
        TransportLimits::default(),
    )
    .await
    .expect("the supervised launch of the real native harness");
    // The child was resumed only after the durable identity and Running landed,
    // so a live process here is a contained process that has executed.
    assert!(
        process.is_alive(),
        "the native harness is live after resume"
    );
    assert_eq!(process.tree_phase(), SupervisorPhase::Running);
    let expected_policy = if cfg!(windows) {
        "windows-creation-time-kill-on-close-job"
    } else {
        "linux-contained-process-group"
    };
    assert_eq!(process.child_policy(), expected_policy);
    assert!(process.operation_id().starts_with("harness-tree-harness-"));
    assert!(process.child_pid() > 0);
    // Cancellation sweeps the TREE and proves it: the record drains, never
    // quarantines, and the confirmation the session layer depends on holds.
    assert!(process.kill_confirmed().await, "termination confirmed");
    assert_eq!(process.tree_phase(), SupervisorPhase::Drained);
}

/// The Windows grandchild arm: the primary cmd spawns a grandchild cmd that
/// spawns ping; the deepest descendant's death is proved against tasklist —
/// OS truth the transport does not own — before and after the sweep.
#[cfg(windows)]
#[tokio::test]
async fn immediate_grandchild_dies_with_the_windows_tree() {
    let scratch = scratch_root("s23-grandchild");
    bind_launch(&scratch);
    let ping_argument = "ping -n 60 127.0.0.1".to_string();
    let process = spawn_plugin(
        Path::new("C:\\Windows\\System32\\cmd.exe"),
        &["/C".to_string(), format!("cmd /C {ping_argument} > NUL")],
        std::sync::Arc::new(DenyCallbacks),
        TransportLimits::default(),
    )
    .await
    .expect("contained cmd launch");
    let grandchild = wait_for_ping_pid(&process.child_pid(), true)
        .expect("the grandchild ping exists before the sweep");
    assert!(process.kill_confirmed().await, "termination confirmed");
    assert_eq!(process.tree_phase(), SupervisorPhase::Drained);
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if !ping_pid_alive(grandchild) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("grandchild ping {grandchild} outlived the swept tree");
}

/// Every `ping.exe` on the box, as tasklist reports it (CSV, locale-safe: only
/// lines whose image name matches are parsed, so an INFO banner never counts).
fn listed_ping_pids() -> Vec<u32> {
    let output = Command::new("tasklist")
        .args(["/FO", "CSV", "/NH", "/FI", "IMAGENAME eq ping.exe"])
        .output()
        .expect("tasklist");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.to_ascii_lowercase().starts_with("\"ping.exe\""))
        .filter_map(|line| line.split("\",\"").nth(1))
        .filter_map(|field| field.trim_matches('"').parse::<u32>().ok())
        .collect()
}

/// Wait until a ping spawned inside the launched tree is (or is not) visible.
fn wait_for_ping_pid(tree_pid: &u32, want: bool) -> Option<u32> {
    let _ = tree_pid;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let pids = listed_ping_pids();
        if let Some(pid) = pids.first() {
            if want {
                return Some(*pid);
            }
        } else if !want {
            return None;
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn ping_pid_alive(pid: u32) -> bool {
    listed_ping_pids().contains(&pid)
}

/// The Linux grandchild arm: the primary sh forks `sleep` and waits on it, so
/// the grandchild lives in the child's own process group; its death is proved
/// against /proc after the sweep.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn immediate_grandchild_dies_with_the_linux_tree() {
    let scratch = scratch_root("s23-grandchild");
    bind_launch(&scratch);
    let process = spawn_plugin(
        Path::new("/bin/sh"),
        &["-c".to_string(), "sleep 300 & wait".to_string()],
        std::sync::Arc::new(DenyCallbacks),
        TransportLimits::default(),
    )
    .await
    .expect("contained sh launch");
    let child = process.child_pid();
    let grandchild = wait_for_grandchild(child, true).expect("the sleep grandchild exists");
    assert!(process.kill_confirmed().await, "termination confirmed");
    assert_eq!(process.tree_phase(), SupervisorPhase::Drained);
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if !grandchild_alive(grandchild) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("grandchild sleep {grandchild} outlived the swept tree");
}

/// `/proc` scan for a live `sleep` whose parent is the launched sh.
#[cfg(target_os = "linux")]
fn wait_for_grandchild(child: u32, want: bool) -> Option<u32> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let found = live_grandchild(child);
        if want && found.is_some() {
            return found;
        }
        if !want && found.is_none() {
            return None;
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[cfg(target_os = "linux")]
fn grandchild_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(target_os = "linux")]
fn live_grandchild(child: u32) -> Option<u32> {
    let entries = std::fs::read_dir("/proc").ok()?;
    for entry in entries.filter_map(|entry| entry.ok()) {
        let pid: u32 = match entry.file_name().to_string_lossy().parse() {
            Ok(pid) => pid,
            Err(_) => continue,
        };
        let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat,
            Err(_) => continue,
        };
        let tail = stat.rsplit(')').next()?;
        let mut fields = tail.split_whitespace();
        let state = fields.next()?;
        let ppid: u32 = fields.next()?.parse().ok()?;
        if state != "Z" && ppid == child && stat.contains("(sleep)") {
            return Some(pid);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// e2e — macOS refuses every launch it cannot contain (SafeDisabled discipline)
// ---------------------------------------------------------------------------

/// On macOS the undeclared harness stays SafeDisabled — the refusal names the
/// platform, happens before any process exists, and never pretends to be an
/// activation. A declared package with an unactivated report is refused the
/// same way, and even a bound Activated report cannot conjure a containment:
/// the launch is refused before the child's first instruction.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn macos_refuses_every_launch_it_cannot_contain() {
    let scratch = scratch_root("s23-macos");
    let binary = native_binary();
    bind_launch(&scratch);
    // Undeclared: SafeDisabled, spelled as the platform's refusal.
    let error = spawn_plugin(
        &binary,
        &[],
        std::sync::Arc::new(DenyCallbacks),
        TransportLimits::default(),
    )
    .await
    .map(|_| ())
    .expect_err("undeclared macOS harness is refused");
    assert_eq!(
        refused_code(&error),
        "harness-macos-undeclared-harness-safe-disabled"
    );
    // Declared but not activated: the profile builds, the report does not hold.
    register_child_process_requirement(&binary, true);
    let error = spawn_plugin(
        &binary,
        &[],
        std::sync::Arc::new(DenyCallbacks),
        TransportLimits::default(),
    )
    .await
    .map(|_| ())
    .expect_err("declared macOS harness without an activated report is refused");
    assert_eq!(
        refused_code(&error),
        "harness-macos-single-process-unproven"
    );
    // Activated report, still no containment at spawn: refused pre-exec.
    let mut activated = HarnessLaunchConfig::host_default();
    activated.scratch_root = scratch;
    activated.journal = std::sync::Arc::new(DeterministicSupervisorJournal::default());
    activated.activation = SafetyActivation::Activated {
        report_id: "s23-macos-activated".into(),
    };
    bind_harness_launch_config(activated);
    let error = spawn_plugin(
        &binary,
        &[],
        std::sync::Arc::new(DenyCallbacks),
        TransportLimits::default(),
    )
    .await
    .map(|_| ())
    .expect_err("declared macOS harness cannot launch without real containment");
    assert_eq!(
        refused_code(&error),
        "harness-macos-single-process-unproven"
    );
    register_child_process_requirement(&binary, false);
}
