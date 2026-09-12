//! R12 — remote approval decisions: a device holding `approvals:decide`
//! decides a real plugin's pending operation through the pinned-TLS
//! transport (audit = device id); read-only devices are refused with the
//! op left pending; `require_desktop_confirm` tasks answer
//! `needs_desktop_confirm` to a remote decision and settle only through
//! the local console.

use r_code_client::ws::{RemoteClient, RemoteEndpoint};
use r_code_harness_protocol::application::ApplicationCommand;
use r_code_kernel::ports::{JournalStore as _, ToolService};
use r_code_kernel::task::{TaskContract, TaskKind, TaskState};
use r_code_kernel::testing::FakeToolService;
use r_code_runtime::application::ApplicationService;
use r_code_runtime::application_receipts::CommandDedup;
use r_code_runtime::daemon::ApplicationHandler;
use r_code_runtime::remote::capabilities::CapabilitySet;
use r_code_runtime::remote::fanout::FanoutHub;
use r_code_runtime::remote::listener::listen_pairing;
use r_code_runtime::remote::pairing::PairingSessions;
use r_code_runtime::remote::registry::{DeviceRegistry, ListenerConfig};
use r_code_runtime::remote::tls::ensure_identity;
use r_code_runtime::{LaunchOptions, ProfileFlavor, RuntimeProfile};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

const SERVICE_TIMEOUT: Duration = Duration::from_secs(30);

fn profile_for(name: &str, temp: &Path) -> RuntimeProfile {
    RuntimeProfile::resolve(
        &LaunchOptions::new(ProfileFlavor::Development)
            .with_data_root(temp.join(name))
            .with_ipc_name(name),
    )
    .expect("profile")
}

fn fixture_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_harness-test-helper"))
}

/// Stage the approval fixture harness (RA3's real plugin behaviour).
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
            "requestedHostServices": ["host.approvals.request", "host.completion.propose"],
            "configSchema": {"type": "object"}
        })
        .to_string(),
    )
    .expect("manifest");
    source
}

/// The real daemon dispatch surface (same method arms as the bin, including
/// the `$remote` approvals marker the listener injects).
struct DaemonMirror {
    service: Arc<ApplicationService>,
}

#[async_trait::async_trait]
impl ApplicationHandler for DaemonMirror {
    async fn execute(&self, command: ApplicationCommand) -> Result<serde_json::Value, String> {
        let params = command.params;
        match command.method.as_str() {
            "task.create" => self
                .service
                .create_task(
                    params["taskId"].as_str().unwrap_or("task-anon"),
                    params["objective"].as_str().unwrap_or_default(),
                    TaskKind::Conversation,
                    vec![],
                )
                .await
                .map(|state| serde_json::json!({"taskId": state.contract.task_id}))
                .map_err(|e| e.to_string()),
            "task.selectHarness" => self
                .service
                .select_harness(
                    params["taskId"].as_str().unwrap_or_default(),
                    params["harnessId"].as_str().unwrap_or_default(),
                )
                .await
                .map(|_| serde_json::Value::Null)
                .map_err(|e| e.to_string()),
            "task.sendMessage" => self
                .service
                .send_message_as(
                    params["taskId"].as_str().unwrap_or_default(),
                    params["text"].as_str().unwrap_or_default(),
                    Some(&command.client_id),
                )
                .await
                .map_err(|e| e.to_string()),
            "task.setPreferences" => {
                let preferences = r_code_kernel::task::TaskPreferences {
                    model: None,
                    inference: None,
                    mode: None,
                    require_desktop_confirm: params["requireDesktopConfirm"]
                        .as_bool()
                        .unwrap_or(false),
                };
                self.service
                    .set_task_preferences(
                        params["taskId"].as_str().unwrap_or_default(),
                        preferences,
                    )
                    .await
                    .map(|_| serde_json::Value::Null)
                    .map_err(|e| e.to_string())
            }
            "approvals.decide$remote" => self
                .service
                .approvals_decide(
                    &params,
                    &command.client_id,
                    r_code_runtime::application::CommandSource::Remote,
                )
                .await
                .map_err(|e| e.to_string()),
            "approvals.decide" => self
                .service
                .approvals_decide(
                    &params,
                    &command.client_id,
                    r_code_runtime::application::CommandSource::Local,
                )
                .await
                .map_err(|e| e.to_string()),
            "task.list" => {
                let rows = self.service.list_tasks().await;
                Ok(serde_json::to_value(rows).unwrap_or_default())
            }
            other => Err(format!("unknown method {other}")),
        }
    }
    async fn events_after(
        &self,
        after_seq: u64,
        limit: u32,
    ) -> Vec<r_code_harness_protocol::EventEnvelope> {
        self.service.events_after(after_seq, limit).await
    }
}

struct Fixture {
    service: Arc<ApplicationService>,
    store: Arc<r_code_store::v2::V2Store>,
    endpoint_decider: RemoteEndpoint,
    endpoint_reader: RemoteEndpoint,
}

async fn fixture(tag: &str) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let profile = profile_for(tag, dir.path());
    let service = Arc::new(
        ApplicationService::compose(
            &profile,
            Arc::new(r_code_kernel::testing::FakeModelService::default()),
            {
                let tools: Arc<dyn ToolService> = Arc::new(FakeToolService::default());
                tools
            },
        )
        .expect("compose"),
    );
    service
        .install_package_from_directory(&stage_approval_fixture(dir.path()))
        .expect("install fixture");
    let store = Arc::new(r_code_store::v2::V2Store::open(&profile.database_path()).expect("store"));
    let seed = TaskState::new(TaskContract {
        task_id: "t1".into(),
        kind: TaskKind::Conversation,
        objective: "seed".into(),
        constraints: vec![],
        required_checks: vec![],
        revision: 1,
    });
    store
        .save_task_and_events(&seed, vec![])
        .await
        .expect("seed");

    let root = profile.harness_v2_root();
    let registry = Arc::new(DeviceRegistry::open(&root).expect("registry"));
    registry
        .set_listener(ListenerConfig {
            enabled: true,
            bind: vec![],
            port: None,
        })
        .expect("enable");
    let identity = ensure_identity(&root).expect("identity");
    let decider = registry
        .register(
            "Decider",
            "ios-pwa",
            &identity.fingerprint,
            CapabilitySet::read_only()
                .with_tasks_write()
                .with_approvals_decide(),
        )
        .expect("decider");
    let reader = registry
        .register(
            "Reader",
            "ios-pwa",
            &identity.fingerprint,
            CapabilitySet::read_only(),
        )
        .expect("reader");

    let handler: Arc<dyn ApplicationHandler> = Arc::new(CommandDedup::new(
        tag,
        store.clone(),
        Arc::new(DaemonMirror {
            service: service.clone(),
        }),
    ));
    let handle = listen_pairing(
        "127.0.0.1".parse().unwrap(),
        0,
        registry.clone(),
        identity.duplicate(),
        handler,
        FanoutHub::new(),
        None,
        Some(Arc::new(PairingSessions::new(Duration::from_secs(120)))),
    )
    .await
    .expect("listen");
    let port = handle.local_addr.port();
    let fingerprint = identity.fingerprint.clone();
    std::mem::forget(handle);
    std::mem::forget(dir);
    let endpoint_for = |paired: &r_code_runtime::remote::registry::PairedDevice| RemoteEndpoint {
        host: "127.0.0.1".into(),
        port,
        fingerprint: fingerprint.clone(),
        token: paired.token.clone(),
        device_id: paired.record.id.clone(),
    };
    Fixture {
        service,
        store,
        endpoint_decider: endpoint_for(&decider),
        endpoint_reader: endpoint_for(&reader),
    }
}

async fn wait_until(
    store: &r_code_store::v2::V2Store,
    predicate: impl Fn(&[r_code_kernel::ports::JournalEvent]) -> bool,
    what: &str,
) -> Vec<r_code_kernel::ports::JournalEvent> {
    let deadline = Instant::now() + SERVICE_TIMEOUT;
    loop {
        let events = store.task_events("t1");
        if predicate(&events) {
            return events;
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for {what}; journal: {:?}",
                events
                    .iter()
                    .map(|event| (event.kind.as_str(), event.payload.clone()))
                    .collect::<Vec<_>>()
            );
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

#[tokio::test]
async fn r12_a1_read_only_decide_is_refused_and_op_stays_pending() {
    let f = fixture("r12a1").await;
    let mut reader = RemoteClient::connect(&f.endpoint_reader)
        .await
        .expect("connect");

    // Register an op and let a plugin-equivalent cite it (blocking branch
    // would hold the socket; use the direct decision surface instead).
    f.service
        .approvals()
        .register("op-r12-1", "run the tool", "run-t1-1", "t1")
        .await;

    // Read-only device: the listener gate refuses with the required
    // capability and the op stays pending (no decision, no event).
    let refused = reader
        .call(
            "approvals.decide",
            serde_json::json!({"operationId": "op-r12-1", "decision": "granted"}),
        )
        .await
        .unwrap_err();
    assert!(
        refused.to_string().contains("permission_denied")
            && refused.to_string().contains("approvals-decide"),
        "gate refusal: {refused}"
    );
    let pending = f.service.approvals_list().await;
    assert_eq!(pending.len(), 1, "op must remain pending after the refusal");
    assert_eq!(pending[0]["opId"], "op-r12-1");
}

#[tokio::test]
async fn r12_a2_remote_decision_flows_to_the_plugin_and_audits_the_device() {
    let f = fixture("r12a2").await;
    let mut decider = RemoteClient::connect(&f.endpoint_decider)
        .await
        .expect("connect");

    // A real task with the approval fixture harness; the host registers
    // the pending op before the run cites it.
    f.service
        .create_task("t1", "approve:op-r12-2", TaskKind::Conversation, vec![])
        .await
        .expect("create");
    f.service
        .select_harness("t1", "fixture.approval")
        .await
        .expect("pin");
    f.service
        .approvals()
        .register("op-r12-2", "run the e2e tool", "run-t1-1", "t1")
        .await;
    f.service
        .send_message("t1", "approve:op-r12-2")
        .await
        .expect("send");
    // Let the drive loop spawn the plugin and reach its blocking cite.
    tokio::time::sleep(Duration::from_secs(2)).await;

    // The decision comes from the REMOTE device; the plugin unblocks.
    let outcome = decider
        .call(
            "approvals.decide",
            serde_json::json!({"operationId": "op-r12-2", "decision": "granted"}),
        )
        .await
        .expect("remote decide");
    assert_eq!(outcome["decision"], "granted");

    let events = wait_until(
        &f.store,
        |events| events.iter().any(|event| event.kind == "run.completed"),
        "run completion",
    )
    .await;
    let decided = events
        .iter()
        .find(|event| event.kind == "approval.decided")
        .expect("decided event");
    assert_eq!(
        decided.payload["decidedBy"].as_str(),
        Some(f.endpoint_decider.device_id.as_str()),
        "audit carries the device id, never a param"
    );
    // The plugin continued after the grant.
    assert!(
        events.iter().any(|event| {
            event.kind == "harness.progress" && event.payload["payload"]["approval"] == "granted"
        }),
        "plugin observed the grant"
    );

    // The fixture proposes completion only on grant; the run completes.
    assert!(
        events.iter().any(|event| event.kind == "run.completed"),
        "granted run completes"
    );
}

#[tokio::test]
async fn r12_desktop_confirm_flag_blocks_remote_and_yields_to_local() {
    let f = fixture("r12dsk").await;
    let mut decider = RemoteClient::connect(&f.endpoint_decider)
        .await
        .expect("connect");

    f.service
        .create_task("t1", "approve:op-r12-3", TaskKind::Conversation, vec![])
        .await
        .expect("create");
    f.service
        .select_harness("t1", "fixture.approval")
        .await
        .expect("pin");
    // High-sensitivity: remote decisions must not terminate the op.
    f.service
        .set_task_preferences(
            "t1",
            r_code_kernel::task::TaskPreferences {
                model: None,
                inference: None,
                mode: None,
                require_desktop_confirm: true,
            },
        )
        .await
        .expect("preferences");
    f.service
        .approvals()
        .register("op-r12-3", "risky operation", "run-t1-1", "t1")
        .await;
    f.service
        .send_message("t1", "approve:op-r12-3")
        .await
        .expect("send");

    // Remote decision: refused with the frozen protocol code.
    let refused = decider
        .call(
            "approvals.decide",
            serde_json::json!({"operationId": "op-r12-3", "decision": "granted"}),
        )
        .await
        .unwrap_err();
    assert!(
        refused.to_string().contains("needs_desktop_confirm"),
        "frozen code: {refused}"
    );
    // The op is still pending — the remote answer never settled it.
    assert_eq!(f.service.approvals_list().await.len(), 1);

    // The desktop console decides locally; the plugin continues.
    f.service
        .approvals_decide(
            &serde_json::json!({"operationId": "op-r12-3", "decision": "granted"}),
            "console-local",
            r_code_runtime::application::CommandSource::Local,
        )
        .await
        .expect("local decide");
    wait_until(
        &f.store,
        |events| events.iter().any(|event| event.kind == "run.completed"),
        "run completion after the local decision",
    )
    .await;
}
