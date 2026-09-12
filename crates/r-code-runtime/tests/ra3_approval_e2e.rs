//! RA3 — approval wiring end to end: a real fixture harness process cites a
//! host-registered pending operation during `harness.start`, blocks on the
//! host decision, and the run settles by that decision — granted continues
//! to completion, denied ends without a proposal, and an undecided request
//! denies by timeout (short-timeout configuration) without hanging the run.

use r_code_kernel::ports::ToolService;
use r_code_kernel::testing::FakeToolService;
use r_code_runtime::application::ApplicationService;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn profile_for(name: &str, temp: &Path) -> RuntimeProfile {
    RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(temp.join(name))
            .with_ipc_name(name),
    )
    .expect("profile")
}

fn fixture_binary() -> PathBuf {
    // The test-only harness fixture binary (CARGO_BIN_EXE covers bins of
    // this crate, including this test's own dependencies' bin targets).
    PathBuf::from(env!("CARGO_BIN_EXE_harness-test-helper"))
}

/// Stage the approval fixture as an installable harness package.
fn stage_approval_fixture(temp: &Path) -> PathBuf {
    let binary = fixture_binary();
    let source = temp.join("pkg-approval");
    let bin_dir = source.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("dirs");
    std::fs::copy(&binary, bin_dir.join("harness-test-helper")).expect("copy fixture");
    let platform = match r_code_harness_protocol::Platform::current() {
        r_code_harness_protocol::Platform::WindowsX64 => "windows-x64",
        r_code_harness_protocol::Platform::MacosArm64 => "macos-arm64",
        r_code_harness_protocol::Platform::MacosX64 => "macos-x64",
        r_code_harness_protocol::Platform::LinuxX64 => "linux-x64",
    };
    std::fs::write(
        source.join("harness.json"),
        serde_json::json!({
            "schema_version": "1",
            "id": "fixture.approval",
            "version": "1.0.0",
            "apiMajor": 1,
            "apiMinor": 0,
            "displayName": "Approval Fixture",
            "supportedPlatforms": [{"platform": platform, "executable": "bin/harness-test-helper", "argv": ["approval"]}],
            "requestedHostServices": [
                "host.approvals.request",
                "host.completion.propose"
            ],
            "configSchema": {"type": "object"}
        })
        .to_string(),
    )
    .expect("manifest");
    source
}

/// Poll the daemon's event view until a `journalKind` appears.
async fn wait_for_kind(
    service: &ApplicationService,
    kind: &str,
    timeout: Duration,
) -> Vec<r_code_harness_protocol::EventEnvelope> {
    let deadline = Instant::now() + timeout;
    loop {
        let events = service.events_after(0, 500).await;
        if events
            .iter()
            .any(|event| event.payload.get("journalKind") == Some(&serde_json::json!(kind)))
        {
            return events;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {kind}; journal: {:?}",
            events
                .iter()
                .map(|event| event.payload.get("journalKind"))
                .collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Wait until the run settles (completed / failed) for the task.
async fn wait_for_run_settled(
    service: &ApplicationService,
    task_id: &str,
    timeout: Duration,
) -> Vec<r_code_harness_protocol::EventEnvelope> {
    let deadline = Instant::now() + timeout;
    loop {
        let events = service.events_after(0, 500).await;
        let settled = events
            .iter()
            .filter(|event| event.task_id == task_id)
            .any(|event| {
                matches!(
                    event.payload.get("journalKind").and_then(|k| k.as_str()),
                    Some("run.completed") | Some("run.failed") | Some("run.cancelled")
                )
            });
        if settled {
            return events;
        }
        assert!(
            Instant::now() < deadline,
            "run never settled; journal: {:?}",
            events
                .iter()
                .map(|event| event.payload.get("journalKind"))
                .collect::<Vec<_>>()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

// -- RA3.A1: grant through the real process chain --------------------------

#[tokio::test]
async fn ra3_a1_grant_unblocks_the_plugin_and_completes_the_run() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile_for("ra3grant", temp.path());
    let tools: Arc<dyn ToolService> = Arc::new(FakeToolService::default());
    let service = ApplicationService::compose(
        &profile,
        Arc::new(r_code_kernel::testing::FakeModelService::default()),
        tools,
    )
    .expect("compose");
    service
        .install_package_from_directory(&stage_approval_fixture(temp.path()))
        .expect("install fixture");
    service
        .create_task(
            "t1",
            "approve:op-e2e-grant",
            r_code_kernel::task::TaskKind::Conversation,
            vec![],
        )
        .await
        .expect("task");
    service
        .select_harness("t1", "fixture.approval")
        .await
        .expect("pin fixture harness");

    // The host registers the pending operation before the plugin cites it.
    service
        .approvals()
        .register("op-e2e-grant", "run the e2e tool", "run-t1-1", "t1")
        .await;

    service
        .send_message("t1", "approve:op-e2e-grant")
        .await
        .expect("send");

    // The requested event surfaces on the daemon event channel…
    wait_for_kind(&service, "approval.requested", Duration::from_secs(30)).await;
    // …the client decides through the daemon decision surface…
    service
        .approvals_decide(
            &serde_json::json!({"operationId": "op-e2e-grant", "decision": "granted"}),
            "client-tui",
            r_code_runtime::application::CommandSource::Local,
        )
        .await
        .expect("decide");

    // …and the plugin continues to completion.
    let events = wait_for_run_settled(&service, "t1", Duration::from_secs(30)).await;
    let decided = events
        .iter()
        .find(|event| {
            event.payload.get("journalKind") == Some(&serde_json::json!("approval.decided"))
        })
        .expect("decided event journaled");
    assert_eq!(decided.payload["decidedBy"], "client-tui");
    assert_eq!(decided.payload["decision"], "granted");
    // The fixture only emits its progress note after the decision lands.
    assert!(
        events.iter().any(|event| {
            event.payload.get("journalKind") == Some(&serde_json::json!("harness.progress"))
                && event.payload["payload"]["approval"] == "granted"
        }),
        "plugin continued after the grant"
    );
    let settled = events
        .iter()
        .find(|event| {
            matches!(
                event.payload.get("journalKind").and_then(|k| k.as_str()),
                Some("run.completed") | Some("run.failed")
            )
        })
        .expect("run settled");
    assert_eq!(
        settled.payload.get("journalKind"),
        Some(&serde_json::json!("run.completed")),
        "granted run completes (not fails)"
    );
}

// -- RA3.A2: denial ends the run; undecided denies by timeout ---------------

#[tokio::test]
async fn ra3_a2_deny_settles_and_timeout_denies_without_hanging() {
    let temp = tempfile::tempdir().expect("tempdir");
    let profile = profile_for("ra3deny", temp.path());
    let tools: Arc<dyn ToolService> = Arc::new(FakeToolService::default());
    let service = ApplicationService::compose(
        &profile,
        Arc::new(r_code_kernel::testing::FakeModelService::default()),
        tools,
    )
    .expect("compose");
    service
        .install_package_from_directory(&stage_approval_fixture(temp.path()))
        .expect("install fixture");
    service
        .create_task(
            "t1",
            "approve:op-e2e-deny",
            r_code_kernel::task::TaskKind::Conversation,
            vec![],
        )
        .await
        .expect("task");
    service
        .select_harness("t1", "fixture.approval")
        .await
        .expect("pin fixture harness");
    service
        .approvals()
        .register("op-e2e-deny", "run the e2e tool", "run-t1-1", "t1")
        .await;
    service
        .send_message("t1", "approve:op-e2e-deny")
        .await
        .expect("send");
    wait_for_kind(&service, "approval.requested", Duration::from_secs(30)).await;

    service
        .approvals_decide(
            &serde_json::json!({"operationId": "op-e2e-deny", "decision": "denied"}),
            "client-tui",
            r_code_runtime::application::CommandSource::Local,
        )
        .await
        .expect("decide deny");

    // The plugin observes the denial and ends (no completion proposal —
    // the run still settles, it must not hang).
    let events = wait_for_run_settled(&service, "t1", Duration::from_secs(30)).await;
    assert!(
        events.iter().any(|event| {
            event.payload.get("journalKind") == Some(&serde_json::json!("harness.progress"))
                && event.payload["payload"]["approval"] == "denied"
        }),
        "plugin observed the denial"
    );

    // Timeout path: a second task whose op nobody decides. Short timeout
    // configuration (not the 300s default) proves the denial fires.
    let slow = ApplicationService::compose_with_approval_timeout(
        &profile,
        Arc::new(r_code_kernel::testing::FakeModelService::default()),
        {
            let tools: Arc<dyn ToolService> = Arc::new(FakeToolService::default());
            tools
        },
        Duration::from_millis(400),
    )
    .expect("compose short-timeout");
    slow.create_task(
        "t2",
        "approve:op-e2e-timeout",
        r_code_kernel::task::TaskKind::Conversation,
        vec![],
    )
    .await
    .expect("task");
    slow.select_harness("t2", "fixture.approval")
        .await
        .expect("pin fixture harness");
    slow.approvals()
        .register("op-e2e-timeout", "nobody will decide", "run-t2-1", "t2")
        .await;
    slow.send_message("t2", "approve:op-e2e-timeout")
        .await
        .expect("send");

    let events = wait_for_run_settled(&slow, "t2", Duration::from_secs(30)).await;
    let timeout_decision = events
        .iter()
        .find(|event| {
            event.payload.get("journalKind") == Some(&serde_json::json!("approval.decided"))
                && event.payload["decidedBy"] == "<timeout>"
        })
        .expect("timeout denial journaled");
    assert_eq!(timeout_decision.payload["decision"], "denied");
    assert!(
        events.iter().any(|event| {
            event.payload.get("journalKind") == Some(&serde_json::json!("harness.progress"))
                && event.payload["payload"]["approval"] == "denied"
        }),
        "plugin unblocked by the timeout denial"
    );
    // The run settled rather than hanging on the undecided approval.
    assert!(
        events.iter().any(|event| {
            matches!(
                event.payload.get("journalKind").and_then(|k| k.as_str()),
                Some("run.completed") | Some("run.failed")
            )
        }),
        "timed-out run settles"
    );
}
