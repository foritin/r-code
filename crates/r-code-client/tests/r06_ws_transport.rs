//! R06 — the WebSocket transport client over a real listener: wss command
//! round-trips (pinned TLS, device token) and reconnect replay of the same
//! command id (F7 receipts across transports).

use r_code_client::ws::{RemoteClient, RemoteEndpoint};
use r_code_kernel::ports::JournalStore as _;
use r_code_kernel::task::{TaskContract, TaskKind, TaskState};
use r_code_runtime::application_receipts::CommandDedup;
use r_code_runtime::daemon::ApplicationHandler;
use r_code_runtime::remote::fanout::FanoutHub;
use r_code_runtime::remote::listener::listen;
use r_code_runtime::remote::registry::{DeviceRegistry, ListenerConfig};
use r_code_runtime::remote::tls::ensure_identity;
use std::sync::Arc;

struct StoreEvents(Arc<r_code_store::v2::V2Store>);

#[async_trait::async_trait]
impl ApplicationHandler for StoreEvents {
    async fn execute(
        &self,
        command: r_code_harness_protocol::application::ApplicationCommand,
    ) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({"method": command.method, "by": command.client_id}))
    }
    async fn events_after(
        &self,
        after_seq: u64,
        limit: u32,
    ) -> Vec<r_code_harness_protocol::EventEnvelope> {
        self.0
            .read_events(after_seq, limit)
            .await
            .into_iter()
            .map(r_code_runtime::run_manager::envelope_of)
            .collect()
    }
}

async fn fixture(tag: &str) -> (RemoteEndpoint, std::net::SocketAddr) {
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
    let store = Arc::new(r_code_store::v2::V2Store::open(&root.join("j.db")).expect("store"));
    let task = TaskState::new(TaskContract {
        task_id: "t1".into(),
        kind: TaskKind::Conversation,
        objective: "r06".into(),
        constraints: vec![],
        required_checks: vec![],
        revision: 1,
    });
    store
        .save_task_and_events(&task, vec![])
        .await
        .expect("seed");
    let event = r_code_kernel::ports::JournalEvent {
        seq: 0,
        task_id: "t1".into(),
        kind: "task.reopened".into(),
        payload: serde_json::json!({}),
    };
    store
        .save_task_and_events(&task, vec![event])
        .await
        .expect("event");
    let paired = registry
        .register(
            "Phone",
            "ios-pwa",
            &identity.fingerprint,
            Default::default(),
        )
        .expect("pair");
    let handler: Arc<dyn ApplicationHandler> = Arc::new(CommandDedup::new(
        "r06",
        store.clone(),
        Arc::new(StoreEvents(store.clone())),
    ));
    let handle = listen(
        "127.0.0.1".parse().unwrap(),
        0,
        registry.clone(),
        identity.duplicate(),
        handler,
        FanoutHub::new(),
        None,
    )
    .await
    .expect("listen");
    let addr = handle.local_addr;
    // Keep the listener alive for the test's lifetime.
    std::mem::forget(handle);
    let endpoint = RemoteEndpoint {
        host: "127.0.0.1".into(),
        port: addr.port(),
        fingerprint: identity.fingerprint.clone(),
        token: paired.token,
        device_id: paired.record.id.clone(),
    };
    (endpoint, addr)
}

#[tokio::test]
async fn r06_a1_wss_roundtrip_task_list_and_events() {
    let (endpoint, _addr) = fixture("a1").await;
    let mut client = RemoteClient::connect(&endpoint).await.expect("connect");

    let listed = client
        .call("task.list", serde_json::json!({}))
        .await
        .expect("task.list");
    assert_eq!(listed["method"], "task.list");
    // The daemon attributes the command to the device identity.
    assert_eq!(listed["by"], endpoint.device_id);

    let events = client.subscribe_events(0).await.expect("subscribe");
    assert_eq!(events.len(), 1, "history replayed from cursor 0");

    // A wrong fingerprint never even completes TLS (F3 pin).
    let mut bad = endpoint.clone();
    bad.fingerprint = "00".repeat(32);
    let refused = match RemoteClient::connect(&bad).await {
        Err(error) => error,
        Ok(_) => panic!("wrong fingerprint must refuse"),
    };
    assert!(matches!(
        refused,
        r_code_client::ClientError::Unreachable(_)
    ));
}

#[tokio::test]
async fn r06_a2_reconnect_replays_the_same_command_id() {
    let (endpoint, _addr) = fixture("a2").await;
    let mut client = RemoteClient::connect(&endpoint).await.expect("connect");
    let first = client
        .call_with_id("task.list", serde_json::json!({"n": 1}), "cmd-persist-1")
        .await
        .expect("first call");

    // The connection dies mid-life (network drop).
    drop(client);

    // Reconnect: the same command id returns the recorded first result —
    // not a re-execution (F7 across transports).
    let mut client = RemoteClient::connect(&endpoint).await.expect("reconnect");
    let replay = client
        .call_with_id("task.list", serde_json::json!({"n": 1}), "cmd-persist-1")
        .await
        .expect("replay");
    assert_eq!(replay, first);

    // Fresh ids keep working on the same connection.
    let fresh = client
        .call("task.detail", serde_json::json!({"taskId": "t1"}))
        .await
        .expect("fresh call");
    assert_eq!(fresh["method"], "task.detail");
}
