//! R04 — remote listener over real TLS+WebSocket on loopback (the test
//! stand-in for a chosen private NIC): authentication, capability gates,
//! forbidden-method refusal, dedup replay and the no-devices/no-listener
//! lifecycle.

use futures_util::{SinkExt, StreamExt};
use r_code_harness_protocol::application::{
    ApplicationCommand, ApplicationFrame, ApplicationResult,
};
use r_code_runtime::application_receipts::CommandDedup;
use r_code_runtime::daemon::ApplicationHandler;
use r_code_runtime::remote::listener::listen;
use r_code_runtime::remote::registry::{DeviceRegistry, ListenerConfig};
use r_code_runtime::remote::tls::{ensure_identity, pinned_client_config};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

struct EchoFacade;

#[async_trait::async_trait]
impl ApplicationHandler for EchoFacade {
    async fn execute(&self, command: ApplicationCommand) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({
            "echo": command.params,
            "by": command.client_id,
        }))
    }
    async fn events_after(
        &self,
        _after_seq: u64,
        _limit: u32,
    ) -> Vec<r_code_harness_protocol::EventEnvelope> {
        vec![]
    }
}

struct Fixture {
    registry: Arc<DeviceRegistry>,
    identity: r_code_runtime::remote::tls::Identity,
    store: Arc<r_code_store::v2::V2Store>,
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join(tag);
        std::fs::create_dir_all(&root).expect("mkdir");
        let registry = Arc::new(DeviceRegistry::open(&root).expect("registry"));
        registry
            .set_listener(ListenerConfig {
                enabled: true,
                bind: vec!["lan".into()],
                port: None,
            })
            .expect("enable listener");
        let identity = ensure_identity(&root).expect("identity");
        let store = Arc::new(r_code_store::v2::V2Store::open(&root.join("j.db")).expect("store"));
        Self {
            registry,
            identity,
            store,
            dir,
        }
    }

    async fn start_listener(&self) -> r_code_runtime::remote::ListenerHandle {
        let handler: Arc<dyn ApplicationHandler> = Arc::new(CommandDedup::new(
            "r04",
            self.store.clone(),
            Arc::new(EchoFacade),
        ));
        listen(
            "127.0.0.1".parse().unwrap(),
            0,
            self.registry.clone(),
            clone_identity(&self.identity),
            handler,
            r_code_runtime::remote::FanoutHub::new(),
            None,
        )
        .await
        .expect("listen")
    }
}

fn clone_identity(
    identity: &r_code_runtime::remote::tls::Identity,
) -> r_code_runtime::remote::tls::Identity {
    identity.duplicate()
}

type Ws = WebSocketStream<TlsStream<TcpStream>>;

async fn connect_pinned(addr: std::net::SocketAddr, fingerprint: &str) -> Result<Ws, String> {
    let tcp = TcpStream::connect(addr).await.map_err(|e| e.to_string())?;
    let connector = tokio_rustls::TlsConnector::from(Arc::new(
        pinned_client_config(fingerprint).expect("config"),
    ));
    let name = rustls::pki_types::ServerName::try_from("r-code-daemon".to_string()).unwrap();
    let tls = connector
        .connect(name, tcp)
        .await
        .map_err(|e| e.to_string())?;
    let url = format!("wss://r-code-daemon:{}/remote", addr.port());
    let (ws, _response) = tokio_tungstenite::client_async_with_config(url, tls, None)
        .await
        .map_err(|e| e.to_string())?;
    Ok(ws)
}

async fn hello(ws: &mut Ws, device_id: &str, token: &str) -> serde_json::Value {
    ws.send(Message::Text(
        serde_json::json!({
            "hello": "r-code-remote/1",
            "device_id": device_id,
            "token": token,
            "client_id": device_id,
        })
        .to_string()
        .into(),
    ))
    .await
    .expect("send hello");
    match ws.next().await {
        Some(Ok(Message::Text(text))) => serde_json::from_str(&text).expect("welcome json"),
        other => panic!("expected welcome, got {other:?}"),
    }
}

async fn send_command(ws: &mut Ws, command_id: &str, method: &str, params: serde_json::Value) {
    ws.send(Message::Text(
        serde_json::to_string(&ApplicationFrame::Command(ApplicationCommand {
            client_id: "spoofed".into(),
            command_id: command_id.into(),
            method: method.into(),
            params,
        }))
        .unwrap()
        .into(),
    ))
    .await
    .expect("send command");
}

async fn next_result(ws: &mut Ws) -> ApplicationResult {
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(text))) => {
                match serde_json::from_str::<ApplicationFrame>(&text) {
                    Ok(ApplicationFrame::Result(result)) => return result,
                    other => panic!("expected result frame, got {other:?} text={text}"),
                }
            }
            // Heartbeat pings ride the same stream; ignore them.
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            other => panic!("expected message, got {other:?}"),
        }
    }
}

// -- R04.A2: auth, dedup replay, wrong token --------------------------------

#[tokio::test]
async fn r04_a2_authenticated_commands_replay_and_bad_tokens_drop() {
    let fixture = Fixture::new("a2");
    let paired = fixture
        .registry
        .register(
            "Pixel",
            "android-pwa",
            &fixture.identity.fingerprint,
            Default::default(),
        )
        .expect("pair");
    let listener = fixture.start_listener().await;
    let addr = listener.local_addr;

    // Wrong token: refused with the structured code and closed.
    let mut bad = connect_pinned(addr, &fixture.identity.fingerprint)
        .await
        .expect("tls+ws");
    let refused = hello(&mut bad, &paired.record.id, "wrong-token").await;
    assert_eq!(refused["ok"], false);
    assert_eq!(refused["error"]["code"], "unauthorized");
    // The daemon closes the socket: the client observes a Close frame
    // followed by stream end — never another application frame.
    match bad.next().await {
        None | Some(Ok(Message::Close(_))) | Some(Err(_)) => {}
        other => panic!("expected close, got {other:?}"),
    }

    // Right token: welcome carries capabilities; commands flow and dedup.
    let mut ws = connect_pinned(addr, &fixture.identity.fingerprint)
        .await
        .expect("tls+ws");
    let welcome = hello(&mut ws, &paired.record.id, &paired.token).await;
    assert_eq!(welcome["ok"], true);
    assert_eq!(welcome["capabilities"][0], "events-read");

    send_command(&mut ws, "cmd-1", "ping", serde_json::json!({"x": 1})).await;
    let first = next_result(&mut ws).await;
    assert_eq!(first.command_id, "cmd-1");
    let echoed = first.outcome.expect("echo ok");
    assert_eq!(echoed["echo"]["x"], 1);
    // The authenticated device is the connection identity, not the wire value.
    assert_eq!(echoed["by"], paired.record.id);

    // Replayed command id: the dedup receipt, identical result.
    send_command(&mut ws, "cmd-1", "ping", serde_json::json!({"x": 1})).await;
    let replay = next_result(&mut ws).await;
    assert_eq!(replay.outcome.expect("replay ok"), echoed);
}

// -- R04.A3: capability + forbidden matrix -----------------------------------

#[tokio::test]
async fn r04_a3_capabilities_and_forbidden_methods_are_enforced() {
    let fixture = Fixture::new("a3");
    let read_only = fixture
        .registry
        .register(
            "Reader",
            "ios-pwa",
            &fixture.identity.fingerprint,
            Default::default(),
        )
        .expect("pair");
    let full = fixture
        .registry
        .register("Admin", "ios-pwa", &fixture.identity.fingerprint, {
            use r_code_runtime::remote::capabilities::CapabilitySet;
            CapabilitySet::read_only()
                .with_tasks_write()
                .with_approvals_decide()
        })
        .expect("pair full");
    let listener = fixture.start_listener().await;
    let addr = listener.local_addr;

    // Read-only device: write refused with the required capability.
    let mut ws = connect_pinned(addr, &fixture.identity.fingerprint)
        .await
        .expect("ws");
    hello(&mut ws, &read_only.record.id, &read_only.token).await;
    send_command(&mut ws, "c1", "task.sendMessage", serde_json::json!({})).await;
    let refused = next_result(&mut ws).await;
    assert_eq!(
        refused.outcome.unwrap_err(),
        "permission_denied: task.sendMessage requires tasks-write"
    );
    send_command(&mut ws, "c2", "approvals.decide", serde_json::json!({})).await;
    let refused = next_result(&mut ws).await;
    assert_eq!(
        refused.outcome.unwrap_err(),
        // The listener rewrites remote decisions with the $remote marker (R12).
        "permission_denied: approvals.decide$remote requires approvals-decide"
    );
    // Read surface works.
    send_command(&mut ws, "c3", "task.list", serde_json::json!({})).await;
    assert!(next_result(&mut ws).await.outcome.is_ok());

    // Full-capability device still cannot touch the forbidden surface.
    let mut ws = connect_pinned(addr, &fixture.identity.fingerprint)
        .await
        .expect("ws");
    hello(&mut ws, &full.record.id, &full.token).await;
    for method in [
        "settings.apply",
        "plugins.install",
        "device.revoke",
        "service.shutdown",
    ] {
        send_command(
            &mut ws,
            &format!("f-{method}"),
            method,
            serde_json::json!({}),
        )
        .await;
        let refused = next_result(&mut ws).await;
        assert_eq!(
            refused.outcome.unwrap_err(),
            format!("forbidden_remote_method: {method} is not callable from a remote transport"),
            "{method} must be forbidden even with all capabilities"
        );
    }
}

// -- R04.A1/A4: no devices / disabled / revoked-last ⇒ no listener ----------

#[tokio::test]
async fn r04_a1_a4_listener_lifecycle_follows_devices_and_switch() {
    let fixture = Fixture::new("lifecycle");
    // No devices: the listener refuses to start at all.
    let handler: Arc<dyn ApplicationHandler> = Arc::new(EchoFacade);
    let refused = listen(
        "127.0.0.1".parse().unwrap(),
        0,
        fixture.registry.clone(),
        clone_identity(&fixture.identity),
        handler.clone(),
        r_code_runtime::remote::FanoutHub::new(),
        None,
    )
    .await;
    assert!(refused.is_err(), "no paired device means no listener");

    // Pair → listen works; revoke the only device → supervision closes it.
    let paired = fixture
        .registry
        .register(
            "Solo",
            "ios-pwa",
            &fixture.identity.fingerprint,
            Default::default(),
        )
        .expect("pair");
    let listener = fixture.start_listener().await;
    let addr = listener.local_addr;
    let mut ws = connect_pinned(addr, &fixture.identity.fingerprint)
        .await
        .expect("ws");
    let welcome = hello(&mut ws, &paired.record.id, &paired.token).await;
    assert_eq!(welcome["ok"], true);

    fixture.registry.revoke(&paired.record.id).expect("revoke");
    // The supervision tick (≤2s) closes the port; a reconnect then fails.
    let deadline = std::time::Instant::now() + Duration::from_secs(6);
    loop {
        tokio::time::sleep(Duration::from_millis(200)).await;
        match TcpStream::connect(addr).await {
            Err(_) => break,
            Ok(_) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "port must close after last revoke"
                );
            }
        }
    }

    // Re-pair + re-enable: the listener accepts again… but the disabled
    // switch must keep it closed (A1's setListener(false) leg).
    let second = fixture
        .registry
        .register(
            "Next",
            "ios-pwa",
            &fixture.identity.fingerprint,
            Default::default(),
        )
        .expect("pair again");
    fixture
        .registry
        .set_listener(ListenerConfig {
            enabled: false,
            bind: vec![],
            port: None,
        })
        .expect("disable");
    let refused = listen(
        "127.0.0.1".parse().unwrap(),
        0,
        fixture.registry.clone(),
        clone_identity(&fixture.identity),
        handler,
        r_code_runtime::remote::FanoutHub::new(),
        None,
    )
    .await;
    assert!(refused.is_err(), "disabled switch refuses the listener");
    let _ = second;
    listener.stop();
    let _ = fixture.dir;
}
