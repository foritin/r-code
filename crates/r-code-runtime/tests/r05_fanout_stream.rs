//! R05 — live event streaming over the remote listener: subscribe with a
//! cursor, receive history then live pushes (seq-ordered, same envelope
//! projection as `task.events`), and reconnect with `after_seq` without
//! loss or duplication.

use futures_util::{SinkExt, StreamExt};
use r_code_harness_protocol::application::{ApplicationCommand, ApplicationFrame};
use r_code_kernel::ports::JournalStore as _;
use r_code_kernel::task::{TaskContract, TaskKind, TaskState};
use r_code_runtime::application_receipts::CommandDedup;
use r_code_runtime::daemon::ApplicationHandler;
use r_code_runtime::remote::fanout::CursorPublisher;
use r_code_runtime::remote::listener::listen;
use r_code_runtime::remote::registry::{DeviceRegistry, ListenerConfig};
use r_code_runtime::remote::tls::ensure_identity;
use r_code_runtime::remote::FanoutHub;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

struct StoreEvents(Arc<r_code_store::v2::V2Store>);

#[async_trait::async_trait]
impl ApplicationHandler for StoreEvents {
    async fn execute(&self, _command: ApplicationCommand) -> Result<serde_json::Value, String> {
        Err("not used".into())
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

struct Fixture {
    registry: Arc<DeviceRegistry>,
    store: Arc<r_code_store::v2::V2Store>,
    hub: Arc<FanoutHub>,
    task: TaskState,
    addr: std::net::SocketAddr,
    token: String,
    device_id: String,
    fingerprint: String,
    _dir: tempfile::TempDir,
}

impl Fixture {
    async fn new(tag: &str) -> Self {
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
            objective: "fanout".into(),
            constraints: vec![],
            required_checks: vec![],
            revision: 1,
        });
        store
            .save_task_and_events(&task, vec![])
            .await
            .expect("seed");
        let hub = FanoutHub::new();
        let paired = registry
            .register(
                "Phone",
                "ios-pwa",
                &identity.fingerprint,
                Default::default(),
            )
            .expect("pair");
        let handler: Arc<dyn ApplicationHandler> = Arc::new(CommandDedup::new(
            "r05",
            store.clone(),
            Arc::new(StoreEvents(store.clone())),
        ));
        let handle = listen(
            "127.0.0.1".parse().unwrap(),
            0,
            registry.clone(),
            identity.duplicate(),
            handler,
            hub.clone(),
            None,
        )
        .await
        .expect("listen");
        let addr = handle.local_addr;
        let token = paired.token;
        let device_id = paired.record.id;
        let fingerprint = identity.fingerprint.clone();
        // A connector used by each test leg.
        std::mem::forget(handle);
        Self {
            registry,
            store,
            hub,
            task,
            addr,
            _dir: dir,
            token,
            device_id,
            fingerprint,
        }
    }

    async fn connect(&self) -> WebSocketStream<TlsStream<TcpStream>> {
        let tcp = TcpStream::connect(self.addr).await.expect("tcp");
        let connector = tokio_rustls::TlsConnector::from(Arc::new(
            r_code_runtime::remote::tls::pinned_client_config(&self.fingerprint).expect("cfg"),
        ));
        let name = rustls::pki_types::ServerName::try_from("r-code-daemon".to_string()).unwrap();
        let tls = connector.connect(name, tcp).await.expect("tls");
        let (mut ws, _) = tokio_tungstenite::client_async_with_config(
            format!("wss://r-code-daemon:{}/remote", self.addr.port()),
            tls,
            None,
        )
        .await
        .expect("ws");
        ws.send(Message::Text(
            serde_json::json!({
                "hello": "r-code-remote/1",
                "device_id": self.device_id,
                "token": self.token,
                "client_id": self.device_id,
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("hello");
        match ws.next().await {
            Some(Ok(Message::Text(text))) => {
                let welcome: serde_json::Value = serde_json::from_str(&text).expect("welcome");
                assert_eq!(welcome["ok"], true);
            }
            other => panic!("welcome expected, got {other:?}"),
        }
        ws
    }

    /// Append N journal events; each is its own save (distinct seqs).
    async fn journal_events(&self, count: u64, label: &str) {
        for index in 0..count {
            let event = r_code_kernel::ports::JournalEvent {
                seq: 0,
                task_id: "t1".into(),
                kind: "harness.progress".into(),
                payload: serde_json::json!({"label": label, "index": index}),
            };
            self.store
                .save_task_and_events(&self.task, vec![event])
                .await
                .expect("journal");
        }
    }
}

/// Collect the next events frame from the stream (skipping pings).
async fn next_events(
    ws: &mut WebSocketStream<TlsStream<TcpStream>>,
) -> Vec<r_code_harness_protocol::EventEnvelope> {
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(text))) => {
                let frame: ApplicationFrame = serde_json::from_str(&text).expect("frame");
                match frame {
                    ApplicationFrame::Events(events) if !events.is_empty() => return events,
                    ApplicationFrame::Events(_) => continue, // empty history frame
                    other => panic!("expected events, got {other:?}"),
                }
            }
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            other => panic!("expected message, got {other:?}"),
        }
    }
}

async fn send_subscribe(ws: &mut WebSocketStream<TlsStream<TcpStream>>, after_seq: u64) {
    let payload = serde_json::to_string(&ApplicationFrame::EventsSubscribe(
        r_code_harness_protocol::application::EventsSubscribeRequest { after_seq },
    ))
    .unwrap();
    ws.send(Message::Text(payload.into()))
        .await
        .expect("subscribe send");
}

#[tokio::test]
async fn r05_a1_live_subscription_streams_new_events_by_seq() {
    let fixture = Fixture::new("a1").await;
    fixture.journal_events(2, "pre").await;
    let mut publisher = CursorPublisher::new(fixture.store.clone(), fixture.hub.clone());
    publisher.poll_once().await; // drain pre-existing events

    let mut ws = fixture.connect().await;
    send_subscribe(&mut ws, 0).await;
    let history = next_events(&mut ws).await;
    assert_eq!(history.len(), 2, "history replay after cursor 0");

    // New journal events reach the subscriber only after persistence +
    // poll (never before the journal knows them).
    fixture.journal_events(3, "live").await;
    publisher.poll_once().await;
    let mut live: Vec<r_code_harness_protocol::EventEnvelope> = Vec::new();
    while live.len() < 3 {
        live.extend(next_events(&mut ws).await);
    }
    let seqs: Vec<u64> = live.iter().map(|event| event.seq).collect();
    assert_eq!(seqs.len(), 3);
    assert!(
        seqs.windows(2).all(|pair| pair[0] < pair[1]),
        "seq monotonic"
    );
    assert!(seqs[0] > history.last().unwrap().seq, "only new events");
    // Same projection as task.events: journalKind rides the payload.
    assert_eq!(live[0].payload["journalKind"], "harness.progress");
}

#[tokio::test]
async fn r05_a2_reconnect_with_cursor_resumes_without_loss_or_duplication() {
    let fixture = Fixture::new("a2").await;
    let mut publisher = CursorPublisher::new(fixture.store.clone(), fixture.hub.clone());

    // Round 1: 10 events while connected.
    fixture.journal_events(10, "round1").await;
    let mut ws = fixture.connect().await;
    send_subscribe(&mut ws, 0).await;
    let first = next_events(&mut ws).await;
    assert_eq!(first.len(), 10);
    let mut seen: Vec<u64> = first.iter().map(|event| event.seq).collect();

    // Disconnect (drop the socket); 40 more events land while offline.
    drop(ws);
    fixture.registry.note_seen(&fixture.device_id);
    fixture.journal_events(40, "offline").await;
    publisher.poll_once().await;

    // Round 2..N: reconnect with the last seen cursor; nothing lost,
    // nothing repeated (multi-round like a flaky phone).
    let mut remaining = 40;
    let mut ws = fixture.connect().await;
    send_subscribe(&mut ws, *seen.last().unwrap()).await;
    while remaining > 0 {
        let mut batch = Vec::new();
        while batch.is_empty() {
            batch.extend(next_events(&mut ws).await);
        }
        for event in batch {
            assert!(
                event.seq > *seen.last().unwrap(),
                "no duplicates ({} <= {})",
                event.seq,
                seen.last().unwrap()
            );
            seen.push(event.seq);
            remaining -= 1;
        }
    }
    assert_eq!(seen.len(), 50, "10 + 40, no loss, no duplication");
}

#[tokio::test]
async fn r05_a3_two_subscribers_isolation_on_the_wire() {
    let fixture = Fixture::new("a3").await;
    let mut publisher = CursorPublisher::new(fixture.store.clone(), fixture.hub.clone());
    let mut steady = fixture.connect().await;
    send_subscribe(&mut steady, 0).await;
    let mut flaky = fixture.connect().await;
    send_subscribe(&mut flaky, 0).await;

    fixture.journal_events(5, "both").await;
    publisher.poll_once().await;
    let mut steady_seen = Vec::new();
    while steady_seen.len() < 5 {
        steady_seen.extend(next_events(&mut steady).await);
    }
    let mut flaky_seen = Vec::new();
    while flaky_seen.len() < 5 {
        flaky_seen.extend(next_events(&mut flaky).await);
    }
    assert_eq!(steady_seen.len(), 5);
    assert_eq!(flaky_seen.len(), 5);

    // The flaky one drops; events keep flowing to the steady one.
    drop(flaky);
    fixture.journal_events(3, "after-drop").await;
    publisher.poll_once().await;
    let mut after_drop = Vec::new();
    while after_drop.len() < 3 {
        after_drop.extend(next_events(&mut steady).await);
    }
    assert_eq!(after_drop.len(), 3, "isolation");
}
