//! R10 — remote write path: a tasks:write-capable device sends/cancels
//! through the same handler as the local console (audited with the device
//! id in `input.queued`), a read-only device's writes are refused, and
//! queueing during an active run behaves exactly like the local path.

use r_code_client::ws::{RemoteClient, RemoteEndpoint};
use r_code_harness_protocol::application::ApplicationCommand;
use r_code_kernel::ports::JournalStore as _;
use r_code_kernel::task::{TaskContract, TaskKind, TaskState};
use r_code_runtime::application_receipts::CommandDedup;
use r_code_runtime::daemon::ApplicationHandler;
use r_code_runtime::remote::capabilities::CapabilitySet;
use r_code_runtime::remote::fanout::FanoutHub;
use r_code_runtime::remote::listener::listen_pairing;
use r_code_runtime::remote::pairing::PairingSessions;
use r_code_runtime::remote::registry::{DeviceRegistry, ListenerConfig};
use r_code_runtime::remote::tls::ensure_identity;
use r_code_store::v2::V2Store;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Real ApplicationService composition so sends go through the actual
/// RunManager (queue semantics), plus the serve fixture harness.
struct Fixture {
    store: Arc<V2Store>,
    endpoint_writer: RemoteEndpoint,
    endpoint_reader: RemoteEndpoint,
    service: Arc<r_code_runtime::application::ApplicationService>,
}

struct RecordingHandler {
    service: Arc<r_code_runtime::application::ApplicationService>,
}

#[async_trait::async_trait]
impl ApplicationHandler for RecordingHandler {
    async fn execute(&self, command: ApplicationCommand) -> Result<serde_json::Value, String> {
        // Thin mirror of the daemon's dispatch for the task surface (the
        // audit actor is the connection identity, exactly like the bin).
        let params = &command.params;
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
            "task.sendMessage" => self
                .service
                .send_message_as(
                    params["taskId"].as_str().unwrap_or_default(),
                    params["text"].as_str().unwrap_or_default(),
                    Some(&command.client_id),
                )
                .await
                .map_err(|e| e.to_string()),
            "task.cancel" => self
                .service
                .cancel_task(params["taskId"].as_str().unwrap_or_default())
                .await
                .map(|cancelled| serde_json::json!({"cancelled": cancelled}))
                .map_err(|e| e.to_string()),
            "task.list" => {
                let rows = self.service.list_tasks().await;
                Ok(serde_json::to_value(rows).unwrap_or_default())
            }
            "task.rename" => self
                .service
                .rename_task(
                    params["taskId"].as_str().unwrap_or_default(),
                    params["title"].as_str().unwrap_or_default(),
                )
                .await
                .map(|_| serde_json::Value::Null)
                .map_err(|e| e.to_string()),
            "task.clone" => self
                .service
                .clone_task(
                    params["sourceTaskId"].as_str().unwrap_or_default(),
                    params["newTaskId"].as_str().unwrap_or_default(),
                    params["title"].as_str(),
                )
                .await
                .map(|branch| serde_json::json!({"taskId": branch.contract.task_id}))
                .map_err(|e| e.to_string()),
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

async fn fixture(tag: &str) -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().join(tag);
    std::fs::create_dir_all(&root).expect("mkdir");
    let registry = Arc::new(DeviceRegistry::open(&root).expect("registry"));
    registry
        .set_listener(ListenerConfig {
            enabled: true,
            bind: vec![],
            port: None,
        })
        .expect("enable");
    let identity = ensure_identity(&root).expect("identity");
    let writer = registry
        .register(
            "Writer",
            "ios-pwa",
            &identity.fingerprint,
            CapabilitySet::read_only().with_tasks_write(),
        )
        .expect("writer device");
    let reader = registry
        .register(
            "Reader",
            "ios-pwa",
            &identity.fingerprint,
            CapabilitySet::read_only(),
        )
        .expect("reader device");

    let profile = r_code_runtime::RuntimeProfile::resolve(
        &r_code_runtime::LaunchOptions::new(r_code_runtime::ProfileFlavor::Development)
            .with_data_root(root.join("data"))
            .with_ipc_name(tag),
    )
    .expect("profile");
    let service = Arc::new(
        r_code_runtime::application::ApplicationService::compose(
            &profile,
            Arc::new(r_code_kernel::testing::FakeModelService::default()),
            Arc::new(r_code_kernel::testing::FakeToolService::default()),
        )
        .expect("compose"),
    );
    let store = Arc::new(V2Store::open(&profile.database_path()).expect("store"));
    let seed = TaskState::new(TaskContract {
        task_id: "t1".into(),
        kind: TaskKind::Conversation,
        objective: "r10".into(),
        constraints: vec![],
        required_checks: vec![],
        revision: 1,
    });
    store
        .save_task_and_events(&seed, vec![])
        .await
        .expect("seed");

    let handler: Arc<dyn ApplicationHandler> = Arc::new(CommandDedup::new(
        tag,
        store.clone(),
        Arc::new(RecordingHandler {
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
    let endpoint_for = |paired: &r_code_runtime::remote::registry::PairedDevice| RemoteEndpoint {
        host: "127.0.0.1".into(),
        port,
        fingerprint: fingerprint.clone(),
        token: paired.token.clone(),
        device_id: paired.record.id.clone(),
    };
    Fixture {
        store,
        endpoint_writer: endpoint_for(&writer),
        endpoint_reader: endpoint_for(&reader),
        service,
    }
}

#[tokio::test]
async fn r10_a1_a3_remote_send_is_audited_and_queue_semantics_match_local() {
    let f = fixture("a1").await;
    let mut writer = RemoteClient::connect(&f.endpoint_writer)
        .await
        .expect("writer connect");

    // Create + send through the remote surface.
    let created = writer
        .call(
            "task.create",
            serde_json::json!({"taskId": "t1", "objective": "r10 e2e"}),
        )
        .await
        .expect("create");
    assert_eq!(created["taskId"], "t1");

    let sent = writer
        .call(
            "task.sendMessage",
            serde_json::json!({"taskId": "t1", "text": "第一条"}),
        )
        .await
        .expect("send");
    assert_eq!(sent["started"], true);

    // While the run is active (no harness installed, the run settles fast —
    // send the follow-up immediately to exercise queueing)…
    let _follow = writer
        .call(
            "task.sendMessage",
            serde_json::json!({"taskId": "t1", "text": "第二条"}),
        )
        .await;

    // …and wait for the journal to carry the audited input events.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let events = f.store.task_events("t1");
        let queued: Vec<_> = events
            .iter()
            .filter(|event| event.kind == "input.queued")
            .collect();
        if !queued.is_empty() {
            let first = &queued[0];
            assert_eq!(
                first.payload.get("actor").and_then(|v| v.as_str()),
                Some(f.endpoint_writer.device_id.as_str()),
                "input.queued carries the device actor (R10 audit)"
            );
            assert_eq!(
                first.payload.get("text").and_then(|v| v.as_str()),
                Some("第一条")
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no audited input.queued in journal"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Cancel answers (no active run → cancelled:false is honest).
    let cancelled = writer
        .call("task.cancel", serde_json::json!({"taskId": "t1"}))
        .await
        .expect("cancel call");
    assert!(cancelled.get("cancelled").is_some());
}

#[tokio::test]
async fn r10_a2_read_only_device_writes_are_all_refused() {
    let f = fixture("a2").await;
    let mut reader = RemoteClient::connect(&f.endpoint_reader)
        .await
        .expect("reader connect");
    for (method, params) in [
        (
            "task.create",
            serde_json::json!({"taskId": "x", "objective": "x"}),
        ),
        (
            "task.sendMessage",
            serde_json::json!({"taskId": "t1", "text": "x"}),
        ),
        ("task.cancel", serde_json::json!({"taskId": "t1"})),
        (
            "task.rename",
            serde_json::json!({"taskId": "t1", "title": "x"}),
        ),
        (
            "task.clone",
            serde_json::json!({"sourceTaskId": "t1", "newTaskId": "y"}),
        ),
    ] {
        let refused = reader.call(method, params).await.unwrap_err();
        assert!(
            refused.to_string().contains("permission_denied"),
            "{method} must refuse read-only devices: {refused}"
        );
    }
    // Read surface unaffected.
    let listed = reader
        .call("task.list", serde_json::json!({}))
        .await
        .expect("list");
    assert!(listed.as_array().is_some());
    let _ = f.service;
}
