//! T11 — Rust SDK and independent Harness fixture.
//!
//! Acceptance: install and execute the independently built repair-harness
//! through the actual registry/transport without modifying host source or
//! selecting a built-in engine enum.

use r_code_harness_protocol::{
    ApiVersion, HostService, NegotiatedCapabilities, PackageRef, RunIdentity,
};
use r_code_kernel::ports::{HarnessSession, JournalStore, RunGuard};
use r_code_kernel::task::{Attempt, TaskContract, TaskKind};
use r_code_kernel::testing::{FakeModelService, FakeProcessService, FakeToolService};
use r_code_runtime::plugins::*;
use r_code_store::v2::V2Store;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Locate (building if needed) the repair-harness example binary. The build
/// is always refreshed so the fixture matches the current SDK sources.
fn repair_harness_binary() -> PathBuf {
    if let Ok(path) = std::env::var("R_CODE_REPAIR_HARNESS_BIN") {
        return PathBuf::from(path);
    }
    let output =
        std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
            .args(["build", "-p", "repair-harness"])
            .output()
            .expect("cargo build repair-harness");
    assert!(
        output.status.success(),
        "building repair-harness failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let exe = if cfg!(windows) {
        "repair-harness.exe"
    } else {
        "repair-harness"
    };
    let target = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/debug")
        .join(exe);
    assert!(target.is_file(), "binary not found at {}", target.display());
    target
}

/// Stage an installable directory package for the fixture.
fn stage_package(temp: &Path, binary: &Path) -> PathBuf {
    let source = temp.join("repair-pkg");
    let bin_dir = source.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("bin dir");
    std::fs::copy(binary, bin_dir.join(binary.file_name().expect("name"))).expect("copy binary");
    let manifest = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/repair-harness/harness.json"),
    )
    .expect("read manifest");
    // The staged binary keeps its platform suffix; the manifest's executable
    // entry omits it, which the installer resolves on Windows.
    std::fs::write(source.join("harness.json"), manifest).expect("write manifest");
    source
}

fn identity() -> RunIdentity {
    RunIdentity {
        task_id: "task-1".into(),
        branch_id: "branch-1".into(),
        run_id: "run-1".into(),
        attempt_id: "attempt-1".into(),
        generation: 1,
    }
}

#[tokio::test]
async fn third_party_fixture_installs_and_runs_through_public_paths() {
    let temp = tempfile::tempdir().expect("tempdir");
    let binary = repair_harness_binary();
    let source = stage_package(temp.path(), &binary);

    // Install through the real installer + catalog + v2 store.
    let store = Arc::new(
        V2Store::open(&temp.path().join("harness-v2").join("tasks.sqlite3")).expect("store"),
    );
    let catalog = PluginCatalog::new(temp.path().join("plugins"), store.clone());
    let installed = catalog
        .install_from_directory(&source)
        .expect("install third-party package");
    assert_eq!(installed.manifest.id.0, "repair-harness.example");

    // Selection is a registry lookup — no built-in engine enum anywhere.
    let effective: PackageRef = catalog
        .effective_package("repair-harness.example")
        .expect("effective");
    assert_eq!(
        effective.content_digest,
        installed.package_ref.content_digest
    );
    catalog.pin("attempt-1", "task-1", &effective).expect("pin");

    // Run through the real transport + router with fake host services.
    let guard = RunGuard::new("run-1", 1);
    let tools = Arc::new(FakeToolService::default());
    let router = Arc::new(HostRouter::new(
        identity(),
        guard.clone(),
        vec![
            HostService::ToolsList,
            HostService::ToolsCall,
            HostService::QuestionsAsk,
            HostService::CheckpointSave,
            HostService::CompletionPropose,
        ],
        tools.clone(),
        Arc::new(FakeModelService::default()),
        Arc::new(FakeProcessService::default()),
        store.clone(),
        Arc::new(IgnoreQuestions),
    ));

    let capabilities = NegotiatedCapabilities {
        plugin_api: ApiVersion::new(1, 0),
        host_api: ApiVersion::new(1, 0),
        granted_services: vec![
            HostService::ToolsList,
            HostService::ToolsCall,
            HostService::QuestionsAsk,
            HostService::CheckpointSave,
            HostService::CompletionPropose,
        ],
    };
    let session = PluginSession::start(
        &installed.executable,
        &installed.argv,
        identity(),
        capabilities,
        guard,
        router.clone(),
        serde_json::json!({"maxRepairSteps": 4}),
        TransportLimits::default(),
    )
    .await
    .expect("session handshake");

    let attempt = Attempt {
        attempt_id: "attempt-1".into(),
        task_id: "task-1".into(),
        branch_id: "branch-1".into(),
        package: effective.clone(),
        contract_revision: 1,
        config_hash: "cfg".into(),
        workspace_identity: "ws".into(),
        run_id: "run-1".into(),
    };
    let contract = TaskContract {
        task_id: "task-1".into(),
        kind: TaskKind::Conversation,
        objective: "fix the flaky test".into(),
        constraints: vec![],
        required_checks: vec![],
        revision: 1,
    };
    let input = r_code_harness_protocol::InputMessage {
        message_id: "m1".into(),
        input_seq: 1,
        kind: r_code_harness_protocol::InputKind::User,
        text: "repair README.md".into(),
    };
    session
        .start(&attempt, &contract, &input)
        .await
        .expect("fixture run");

    // The fixture's workflow ran through public host services:
    tokio::time::sleep(Duration::from_millis(300)).await;

    // 1. It inspected the target through host.tools.call (with operation key).
    let (call_count, call_name) = {
        let tool_calls = tools.calls.lock().unwrap();
        (tool_calls.len(), tool_calls[0].1.clone())
    };
    assert_eq!(call_count, 1);
    assert_eq!(call_name, "read_file");

    // 2. It checkpointed its opaque state with the host.
    let checkpoint = store
        .load_latest_checkpoint("attempt-1")
        .await
        .expect("checkpoint stored");
    assert!(checkpoint.state.starts_with(b"repair-plan:"));
    assert_eq!(checkpoint.consumed_input_seq, 1);

    // 3. It proposed completion and the host recorded the proposal.
    let proposals = router.recorded_proposals.lock().expect("proposals").clone();
    assert_eq!(proposals.len(), 1);
    assert_eq!(
        proposals[0].kind,
        r_code_harness_protocol::services::ProposalKind::PlanDraft
    );
    assert!(proposals[0].summary.contains("repair plan"));

    // 4. Progress events flowed through harness.event.
    let events = router.observed_events.lock().expect("events").clone();
    assert!(
        events.len() >= 2,
        "expected planning + plan events, got {}",
        events.len()
    );

    session.cancel("test complete").await.expect("cancel");
}

#[test]
fn sdk_fixture_depends_only_on_sdk_and_protocol() {
    // Dependency guard: the example crate's [dependencies] must not reference
    // host implementation crates (dev-dependencies of this test crate are
    // irrelevant to what ships inside a plugin binary).
    let manifest = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/repair-harness/Cargo.toml"),
    )
    .expect("read example manifest");
    let dependencies = manifest
        .split("[dependencies]")
        .nth(1)
        .map(|section| section.split('[').next().unwrap_or(section))
        .unwrap_or("");
    for forbidden in [
        "r-code-runtime",
        "r-code-store",
        "r-code-gateway",
        "r-code-client",
        "r-code-host",
        "tauri",
        "r-code-agent-worker",
        "r-code-kernel",
    ] {
        assert!(
            !dependencies.contains(forbidden),
            "repair-harness must not depend on {forbidden}"
        );
    }
    // And the SDK's own [dependencies] stay protocol-pure.
    let sdk_manifest =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("read sdk manifest");
    let sdk_dependencies = sdk_manifest
        .split("[dependencies]")
        .nth(1)
        .map(|section| section.split('[').next().unwrap_or(section))
        .unwrap_or("");
    for forbidden in [
        "r-code-runtime",
        "r-code-store",
        "r-code-gateway",
        "tauri",
        "r-code-host",
        "r-code-kernel",
    ] {
        assert!(
            !sdk_dependencies.contains(forbidden),
            "the SDK must not depend on {forbidden}"
        );
    }
}

#[test]
fn sdk_serves_initialize_and_cancel_over_real_stdio() {
    // Drive the fixture binary directly over its stdio protocol: the SDK
    // must answer initialize and acknowledge a cooperative cancel.
    use std::io::{BufRead, BufReader, Write};

    let binary = repair_harness_binary();
    let mut child = std::process::Command::new(&binary)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn fixture");

    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut reader = BufReader::new(stdout);

    let initialize = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocol": "r-code-harness/1",
            "host_api": {"major": 1, "minor": 0},
            "identity": {"task_id": "t", "branch_id": "b", "run_id": "r",
                          "attempt_id": "a", "generation": 1},
            "granted_services": [],
            "harness_config": {},
            "limits": {"max_frame_bytes": 1048576, "max_queue_bytes": 16777216,
                        "initialize_timeout_ms": 10000, "cancel_grace_ms": 5000}
        }
    });
    writeln!(stdin, "{initialize}").expect("write initialize");
    stdin.flush().expect("flush");

    let mut line = String::new();
    reader.read_line(&mut line).expect("read initialize reply");
    let reply: serde_json::Value = serde_json::from_str(line.trim()).expect("parse reply");
    assert_eq!(reply["id"], 1);
    assert_eq!(reply["result"]["harnessId"], "repair-harness.example");

    let cancel = serde_json::json!({
        "jsonrpc": "2.0", "id": 2, "method": "harness.cancel",
        "params": {"identity": {"task_id": "t", "branch_id": "b", "run_id": "r",
                                 "attempt_id": "a", "generation": 1},
                    "reason": "test"}
    });
    writeln!(stdin, "{cancel}").expect("write cancel");
    stdin.flush().expect("flush");

    let mut line = String::new();
    reader.read_line(&mut line).expect("read cancel reply");
    let reply: serde_json::Value = serde_json::from_str(line.trim()).expect("parse cancel reply");
    assert_eq!(reply["id"], 2);
    assert_eq!(reply["result"]["acknowledged"], true);

    drop(stdin);
    let status = child.wait().expect("wait");
    assert!(
        status.success() || status.code() == Some(0),
        "clean exit, got {status:?}"
    );
}
