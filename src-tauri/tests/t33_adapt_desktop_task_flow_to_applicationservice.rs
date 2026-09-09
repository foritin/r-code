//! T33 — desktop task flow adapted to the ApplicationService client.
//!
//! Desktop integration through the thin bridge (no Tauri runtime needed:
//! the plain-callable surface is what the commands delegate to) covering
//! send/queue, harness selection, message run and event replay against the
//! shared daemon with a real third-party harness package.

use r_code_host::harness_v2::HarnessV2Bridge;
use r_code_runtime::{LaunchOptions, ProfileFlavor};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

fn build(package: &str) {
    let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["build", "-p", package])
        .output()
        .expect("cargo build");
    assert!(
        output.status.success(),
        "building {package} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn build_bins() {
    // r-code-service is a bin of the runtime package, not a package.
    let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(["build", "-p", "r-code-runtime", "--bin", "r-code-service"])
        .output()
        .expect("cargo build bins");
    assert!(
        output.status.success(),
        "building r-code-service failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn target_binary(name: &str) -> PathBuf {
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/debug")
        .join(exe);
    assert!(path.is_file(), "missing {}", path.display());
    path
}

fn stage_package(temp: &Path, binary: &Path) -> PathBuf {
    let source = temp.join("repair-pkg");
    std::fs::create_dir_all(source.join("bin")).expect("dirs");
    std::fs::copy(binary, source.join("bin").join(binary.file_name().unwrap())).expect("copy");
    let platform = match r_code_harness_protocol::Platform::current() {
        r_code_harness_protocol::Platform::WindowsX64 => "windows-x64",
        r_code_harness_protocol::Platform::MacosArm64 => "macos-arm64",
        r_code_harness_protocol::Platform::MacosX64 => "macos-x64",
        r_code_harness_protocol::Platform::LinuxX64 => "linux-x64",
    };
    let executable = binary.file_name().unwrap().to_string_lossy();
    let executable = if cfg!(windows) {
        executable.trim_end_matches(".exe")
    } else {
        executable.as_ref()
    };
    std::fs::write(
        source.join("harness.json"),
        serde_json::json!({
            "schema_version": "1",
            "id": "repair-harness.example",
            "version": "1.0.0",
            "apiMajor": 1, "apiMinor": 0,
            "displayName": "repair",
            "supportedPlatforms": [{"platform": platform, "executable": format!("bin/{executable}")}],
            "requestedHostServices": ["host.model.stream", "host.tools.list", "host.tools.call",
                "host.checkpoint.save", "host.completion.propose"],
            "configSchema": {"type": "object"}
        })
        .to_string(),
    )
    .expect("manifest");
    source
}

fn bridge(temp: &Path, ipc: &str) -> HarnessV2Bridge {
    let profile = r_code_runtime::RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(temp.join("root"))
            .with_ipc_name(ipc),
    )
    .expect("profile");
    HarnessV2Bridge::new_from_profile(profile, Some(target_binary("r-code-service")))
}

#[test]
fn desktop_lifecycle_rides_the_shared_daemon() {
    build_bins();
    build("repair-harness");
    let temp = tempfile::tempdir().expect("tempdir");
    let package = stage_package(temp.path(), &target_binary("repair-harness"));
    let bridge = bridge(temp.path(), "t33-desktop");

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        // Daemon reachable (spawned by the bridge on first use).
        let pong = bridge.ping().await.expect("ping");
        assert_eq!(pong["pong"], true);

        // Plugin install + list through the daemon registry.
        let installed = bridge
            .plugins_install(package.to_string_lossy().as_ref())
            .await
            .expect("install");
        assert_eq!(installed["id"], "repair-harness.example");
        let listing = bridge.plugins_list().await.expect("list");
        assert!(listing
            .as_array()
            .expect("entries")
            .iter()
            .any(|entry| entry["manifest"]["id"] == "repair-harness.example"
                && entry["availability"] == "Available"));

        // Task create → harness select → send → events.
        let created = bridge
            .task_create("task-1", "repair the fixture", "conversation", vec![])
            .await
            .expect("create");
        assert_eq!(created["taskId"], "task-1");

        let selected = bridge
            .task_select_harness("task-1", "repair-harness.example")
            .await
            .expect("select");
        assert_eq!(selected["id"], "repair-harness.example");

        let sent = bridge
            .task_send_message("task-1", "repair README.md")
            .await
            .expect("send");
        assert_eq!(
            sent["started"], true,
            "the third-party harness ran on the daemon"
        );

        // Event replay by cursor for reconnecting frontends.
        let events = bridge.task_events(0).await.expect("events");
        let events = events.as_array().expect("envelope array");
        assert!(
            events.len() >= 3,
            "created/pinned/queued visible: {events:?}"
        );

        // Tests own their daemon: explicit stop releases the binary.
        bridge.shutdown_daemon().await.expect("shutdown");
    });
}

#[test]
fn bridge_reports_unavailable_daemons_without_panicking() {
    let temp = tempfile::tempdir().expect("tempdir");
    // No service binary and no daemon: connection fails with a typed error.
    let profile = r_code_runtime::RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(temp.path().join("root"))
            .with_ipc_name("t33-unreachable"),
    )
    .expect("profile");
    // Point at a nonexistent binary so no daemon can be spawned; the
    // default locator would otherwise find the workspace build.
    let bridge = HarnessV2Bridge::new_from_profile(
        profile,
        Some(PathBuf::from("Z:/nonexistent/r-code-service.exe")),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let result = runtime.block_on(async { bridge.ping().await });
    match result {
        Err(r_code_host::harness_v2::HarnessV2Error::Daemon(_)) => {}
        other => panic!("expected a typed daemon error, got {other:?}"),
    }
    let _ = Duration::from_secs(0);
}
