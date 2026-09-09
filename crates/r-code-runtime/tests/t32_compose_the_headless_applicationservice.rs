//! T32 — headless ApplicationService composition.
//!
//! A headless end-to-end fixture runs Native, Codex-mock and third-party
//! Harness with identical task lifecycle APIs.

use r_code_kernel::ports::{ModelService, ToolService};
use r_code_kernel::task::TaskKind;
use r_code_runtime::application::ApplicationService;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn profile_for(name: &str, temp: &Path) -> RuntimeProfile {
    RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(temp.join(name))
            .with_ipc_name(name),
    )
    .expect("profile")
}

fn repair_harness_binary() -> PathBuf {
    let output =
        std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args(["build", "-p", "repair-harness"])
            .output()
            .expect("build repair-harness");
    assert!(output.status.success());
    let exe = if cfg!(windows) {
        "repair-harness.exe"
    } else {
        "repair-harness"
    };
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug")
        .join(exe)
}

fn service_helper_binary() -> PathBuf {
    // The Codex-mock stand-in: a real process speaking the plugin protocol.
    PathBuf::from(env!("CARGO_BIN_EXE_harness-test-helper"))
}

/// Stage a minimal installable package around a binary.
fn stage_package(temp: &Path, name: &str, binary: &Path, id: &str) -> PathBuf {
    let source = temp.join(format!("pkg-{name}"));
    let bin_dir = source.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("dirs");
    std::fs::copy(binary, bin_dir.join(binary.file_name().unwrap())).expect("copy");
    let platform = match r_code_harness_protocol::Platform::current() {
        r_code_harness_protocol::Platform::WindowsX64 => "windows-x64",
        r_code_harness_protocol::Platform::MacosArm64 => "macos-arm64",
        r_code_harness_protocol::Platform::MacosX64 => "macos-x64",
        r_code_harness_protocol::Platform::LinuxX64 => "linux-x64",
    };
    let executable = binary.file_name().unwrap().to_string_lossy();
    let executable = if cfg!(windows) {
        executable.trim_end_matches(".exe").to_string()
    } else {
        executable.to_string()
    };
    std::fs::write(
        source.join("harness.json"),
        serde_json::json!({
            "schema_version": "1",
            "id": id,
            "version": "1.0.0",
            "apiMajor": 1,
            "apiMinor": 0,
            "displayName": name,
            "supportedPlatforms": [{"platform": platform, "executable": format!("bin/{executable}")}],
            "requestedHostServices": ["host.model.stream", "host.tools.list", "host.tools.call", "host.checkpoint.save", "host.completion.propose"],
            "configSchema": {"type": "object"}
        })
        .to_string(),
    )
    .expect("manifest");
    source
}

async fn run_one_harness(
    temp: &Path,
    name: &str,
    package_source: &Path,
    harness_id: &str,
) -> serde_json::Value {
    let models: Arc<dyn ModelService> =
        Arc::new(r_code_kernel::testing::FakeModelService::default());
    let tools: Arc<dyn ToolService> = Arc::new(r_code_kernel::testing::FakeToolService::default());
    let profile = profile_for(name, temp);
    let service = ApplicationService::compose(&profile, models, tools).expect("compose");

    // Identical lifecycle API for every harness:
    let installed = service
        .install_package_from_directory(package_source)
        .expect("install");
    assert_eq!(installed.manifest.id.0, harness_id);
    assert!(service
        .list_plugins()
        .expect("list")
        .iter()
        .any(|entry| entry.manifest.id.0 == harness_id
            && entry.availability == r_code_runtime::plugins::Availability::Available));

    service
        .create_task(
            "task-1",
            "fix the flaky test",
            TaskKind::Conversation,
            vec![],
        )
        .await
        .expect("create task");
    service
        .select_harness("task-1", harness_id)
        .await
        .expect("select harness");

    let result = service
        .send_message("task-1", "repair README.md")
        .await
        .expect("send message");
    assert_eq!(result["started"], true);

    // The durable journal carries the lifecycle for clients.
    let events = service.events_after(0, 100).await;
    assert!(
        events.len() >= 3,
        "created+pinned+queued events: {}",
        events.len()
    );
    assert!(events
        .iter()
        .any(|event| event.kind == r_code_harness_protocol::EventKind::RunState));
    result
}

#[tokio::test]
async fn third_party_and_codex_mock_share_one_lifecycle_api() {
    let temp = tempfile::tempdir().expect("tempdir");

    // Third-party harness (the independent example package).
    let third_party = stage_package(
        temp.path(),
        "third-party",
        &repair_harness_binary(),
        "repair-harness.example",
    );
    let third = run_one_harness(temp.path(), "third", &third_party, "repair-harness.example").await;
    assert_eq!(third["started"], true);

    // Codex-mock: a real fixture process through the same API.
    let codex_mock = stage_package(
        temp.path(),
        "codex-mock",
        &service_helper_binary(),
        "codex-mock.fixture",
    );
    let mock = run_one_harness(temp.path(), "codexmock", &codex_mock, "codex-mock.fixture").await;
    assert_eq!(mock["started"], true);

    // Native rides the identical path (T26 exercises the session layer in
    // depth; here the lifecycle API parity is the contract).
    let native_pkg_source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/native");
    // Stage the native binary as a package around the built executable.
    let native_binary = repair_native_binary();
    let native_pkg = stage_package(temp.path(), "native", &native_binary, "native.r-code");
    let _ = native_pkg_source;
    let native = run_one_harness(temp.path(), "nativelife", &native_pkg, "native.r-code").await;
    assert_eq!(native["started"], true);
}

fn repair_native_binary() -> PathBuf {
    let output =
        std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args(["build", "-p", "r-code-harness-native"])
            .output()
            .expect("build native");
    assert!(output.status.success());
    let exe = if cfg!(windows) {
        "r-code-harness-native.exe"
    } else {
        "r-code-harness-native"
    };
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug")
        .join(exe)
}
