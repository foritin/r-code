//! T36 — TUI plugin catalog and selection commands.
//!
//! Headless command tests install/use/remove fixtures with machine-readable
//! outcomes; active-version constraints match the GUI (pinned removal
//! refused, enable/disable reversible), all through the same daemon surface.

use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use r_code_tui::harness_client::HarnessTuiClient;
use std::path::{Path, PathBuf};
use std::process::Command;

fn build(package: &str, bin: Option<&str>) {
    let mut args = vec!["build".to_string(), "-p".to_string(), package.to_string()];
    if let Some(bin) = bin {
        args.push("--bin".into());
        args.push(bin.into());
    }
    let output = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
        .args(&args)
        .output()
        .expect("cargo build");
    assert!(
        output.status.success(),
        "building {package} failed: {}",
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
        .join("../../target/debug")
        .join(exe);
    assert!(path.is_file(), "missing {}", path.display());
    path
}

fn stage_package(temp: &Path, name: &str, binary: &Path, harness_id: &str) -> PathBuf {
    let source = temp.join(format!("pkg-{name}"));
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
            "id": harness_id,
            "version": "1.0.0",
            "apiMajor": 1, "apiMinor": 0,
            "displayName": name,
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

fn client(temp: &Path) -> HarnessTuiClient {
    let profile = RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(temp.join("root"))
            .with_ipc_name("t36-tui"),
    )
    .expect("profile");
    HarnessTuiClient::from_profile(profile, Some(target_binary("r-code-service")))
}

#[tokio::test]
async fn plugins_lifecycle_with_machine_readable_outcomes() {
    build("r-code-runtime", Some("r-code-service"));
    build("repair-harness", None);
    let temp = tempfile::tempdir().expect("tempdir");
    let harness = client(temp.path());
    let package = stage_package(
        temp.path(),
        "repair",
        &target_binary("repair-harness"),
        "repair-harness.example",
    );

    // help: usage lines, ok.
    let help = harness.execute("help").await;
    assert!(help.ok);
    assert!(help
        .lines
        .iter()
        .any(|line| line.contains("/plugins install")));

    // list on empty catalog.
    let empty = harness.execute("list").await;
    assert!(empty.ok);
    assert!(empty.lines[0].contains("尚未安装"));

    // install from a staged directory.
    let installed = harness
        .execute(&format!("install {}", package.display()))
        .await;
    assert!(installed.ok, "{:?}", installed.lines);
    let digest = installed.detail["contentDigest"]
        .as_str()
        .unwrap()
        .to_string();

    // list shows the entry with rendered state.
    let list = harness.execute("list").await;
    assert!(list.ok);
    let entries =
        serde_json::from_value::<Vec<r_code_tui::harness_client::PluginEntry>>(list.detail)
            .expect("machine-readable entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, "repair-harness.example");
    assert!(entries[0].available && entries[0].enabled);
    assert!(list.lines.iter().any(|line| line.contains("可用")));

    // use: select the harness for a fresh task (machine-readable pin).
    // The task itself is created on the daemon first (TUI session layer
    // owns that in interactive mode; headless tests create it directly).
    {
        use r_code_client::DaemonClient;
        let info = r_code_client::ensure_daemon(
            &harness.profile().harness_v2_root(),
            &harness.profile().ipc_endpoint(),
            &harness.profile().profile_id(),
            Some(&target_binary("r-code-service")),
        )
        .await
        .expect("daemon");
        let mut client = DaemonClient::connect(
            &harness.profile().ipc_endpoint(),
            &harness.profile().profile_id(),
            &info.token,
            "t36",
        )
        .await
        .expect("connect");
        client
            .call(
                "task.create",
                serde_json::json!({"taskId": "task-tui-1", "objective": "tui fixture", "kind": "conversation"}),
            )
            .await
            .expect("create task");
    }
    let task = harness
        .execute("use task-tui-1 repair-harness.example")
        .await;
    assert!(task.ok, "{:?}", task.lines);
    assert_eq!(task.detail["id"], "repair-harness.example");

    // remove the pinned live version: refused with a machine-readable error.
    let refused = harness
        .execute(&format!("remove repair-harness.example {digest}"))
        .await;
    assert!(!refused.ok);
    assert!(refused.detail["error"].as_str().unwrap().contains("pinned"));

    // enable/disable reversible; disable then list shows the state.
    let disabled = harness
        .execute(&format!("disable repair-harness.example {digest}"))
        .await;
    assert!(disabled.ok);
    let listed = harness.execute("").await; // bare /plugins = list
    assert!(listed.ok);
    let entries =
        serde_json::from_value::<Vec<r_code_tui::harness_client::PluginEntry>>(listed.detail)
            .expect("entries");
    assert!(!entries[0].enabled);
    assert!(!entries[0].available);
    let re_enabled = harness
        .execute(&format!("enable repair-harness.example {digest}"))
        .await;
    assert!(re_enabled.ok);

    // Usage errors are machine-readable too.
    let bad = harness.execute("install").await;
    assert!(!bad.ok && bad.lines[0].contains("用法"));
    let missing = harness.execute("install Z:/definitely/not/here").await;
    assert!(!missing.ok);

    // Tests own their daemon: explicit stop releases the binary.
    {
        use r_code_client::DaemonClient;
        let info = r_code_client::ensure_daemon(
            &harness.profile().harness_v2_root(),
            &harness.profile().ipc_endpoint(),
            &harness.profile().profile_id(),
            Some(&target_binary("r-code-service")),
        )
        .await
        .expect("daemon");
        let mut client = DaemonClient::connect(
            &harness.profile().ipc_endpoint(),
            &harness.profile().profile_id(),
            &info.token,
            "t36-teardown",
        )
        .await
        .expect("connect");
        client
            .call("service.shutdown", serde_json::json!({}))
            .await
            .expect("shutdown");
    }
}

#[test]
fn slash_menu_and_dispatch_wiring_cover_plugins() {
    // The interactive surface lists /plugins and app dispatch routes it.
    let menu = r_code_tui::slash_menu::COMMANDS;
    assert!(menu.iter().any(|command| command.name == "/plugins"));

    // Rendering: availability states map to grid lines.
    let entry = r_code_tui::harness_client::PluginEntry {
        id: "example.harness".into(),
        version: "1.0.0".into(),
        display_name: "Example".into(),
        digest: "abc123".into(),
        enabled: false,
        available: false,
        unavailable_reason: Some("协议版本不兼容".into()),
    };
    let rendered = r_code_tui::harness_client::render_entry(&entry);
    assert!(rendered.contains("不可用(协议版本不兼容)"), "{rendered}");
}
