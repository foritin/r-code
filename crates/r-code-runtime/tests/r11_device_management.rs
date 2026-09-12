//! R11 — device management (local console surface): list/revoke/capability
//! updates take effect on the wire immediately; the listener switch drops
//! all connections while device records persist.

use r_code_client::ws::{RemoteClient, RemoteEndpoint};
use r_code_harness_protocol::application::ApplicationCommand;
use r_code_runtime::application_receipts::CommandDedup;
use r_code_runtime::daemon::ApplicationHandler;
use r_code_runtime::remote::capabilities::CapabilitySet;
use r_code_runtime::remote::fanout::FanoutHub;
use r_code_runtime::remote::pairing::PairingSessions;
use r_code_runtime::remote::registry::{DeviceRegistry, ListenerConfig};
use r_code_runtime::remote::tls::ensure_identity;
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

struct NullHandler;

#[async_trait::async_trait]
impl ApplicationHandler for NullHandler {
    async fn execute(&self, _command: ApplicationCommand) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!({"ok": true}))
    }
    async fn events_after(
        &self,
        _after_seq: u64,
        _limit: u32,
    ) -> Vec<r_code_harness_protocol::EventEnvelope> {
        vec![]
    }
}

/// The management surface under test: the *same* RemoteManager the service
/// bin delegates to, wired with a live listener.
struct Surface {
    registry: Arc<DeviceRegistry>,
    identity: r_code_runtime::remote::tls::Identity,
    manager: Arc<r_code_runtime::remote::RemoteManager>,
}

async fn surface(tag: &str) -> Surface {
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
    let handler = Arc::new(CommandDedup::new(
        tag,
        Arc::new(r_code_store::v2::V2Store::open(&root.join("j.db")).expect("store")),
        Arc::new(NullHandler),
    ));
    let manager = Arc::new(r_code_runtime::remote::RemoteManager::new(
        registry.clone(),
        Arc::new(PairingSessions::new(Duration::from_secs(120))),
        identity.duplicate(),
        FanoutHub::new(),
        None,
        "127.0.0.1".parse().unwrap(),
    ));
    manager.wire_handler(handler.clone()).await;
    // Pairing intent opens the listener with zero devices (R08 semantics).
    manager.pairing_start().await.expect("pairing listener");
    std::mem::forget(dir);
    Surface {
        registry,
        identity,
        manager,
    }
}

impl Surface {
    async fn endpoint(
        &self,
        paired: &r_code_runtime::remote::registry::PairedDevice,
    ) -> RemoteEndpoint {
        RemoteEndpoint {
            host: "127.0.0.1".into(),
            port: self.manager.listening_port().await.unwrap_or_default(),
            fingerprint: self.identity.fingerprint.clone(),
            token: paired.token.clone(),
            device_id: paired.record.id.clone(),
        }
    }

    /// Console methods through the shared manager (the bin delegates the
    /// same names to the same implementation).
    async fn local(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        match method {
            "device.list" => Ok(serde_json::json!({
                "devices": self.manager.list_devices()
            })),
            "device.revoke" => self
                .manager
                .revoke(params["deviceId"].as_str().unwrap_or_default())
                .await
                .map(|_| serde_json::Value::Null)
                .map_err(|e| e.to_string()),
            "device.updateCapabilities" => {
                let labels: Vec<&str> = params["capabilities"]
                    .as_array()
                    .map(|values| values.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();
                self.manager
                    .update_capabilities(params["deviceId"].as_str().unwrap_or_default(), &labels)
                    .await
                    .map(|caps| serde_json::json!({"capabilities": caps}))
                    .map_err(|e| e.to_string())
            }
            "device.setListener" => self
                .manager
                .set_listener(params["enabled"].as_bool().unwrap_or(false))
                .await
                .map(|_| serde_json::json!({"enabled": params["enabled"]}))
                .map_err(|e| e.to_string()),
            other => Err(format!("unknown method {other}")),
        }
    }
}

#[tokio::test]
async fn r11_a1_list_carries_the_device_fields() {
    let s = surface("a1").await;
    let paired = s
        .registry
        .register(
            "iPhone 15",
            "ios-pwa",
            &s.identity.fingerprint,
            CapabilitySet::read_only(),
        )
        .expect("pair");
    let listed = s
        .local("device.list", serde_json::json!({}))
        .await
        .expect("list");
    let devices = listed["devices"].as_array().expect("array");
    assert_eq!(devices.len(), 1);
    let row = &devices[0];
    assert_eq!(row["deviceId"], paired.record.id);
    assert_eq!(row["name"], "iPhone 15");
    assert_eq!(row["platform"], "ios-pwa");
    assert_eq!(row["capabilities"][0], "events-read");
    assert_eq!(row["revoked"], false);
}

#[tokio::test]
async fn r11_a2_revocation_drops_the_live_connection_and_refuses_reconnect() {
    let s = surface("a2").await;
    let paired = s
        .registry
        .register(
            "Pixel",
            "android-pwa",
            &s.identity.fingerprint,
            CapabilitySet::read_only(),
        )
        .expect("pair");
    let mut device = RemoteClient::connect(&s.endpoint(&paired).await)
        .await
        .expect("connect");

    // The management console revokes.
    s.local(
        "device.revoke",
        serde_json::json!({"deviceId": paired.record.id}),
    )
    .await
    .expect("revoke");

    // The live socket observes the close.
    let closed = async {
        loop {
            match device_ws_next(&mut device).await {
                Some(tokio_tungstenite::tungstenite::Message::Close(_)) => return true,
                Some(_) => continue,
                None => return true,
            }
        }
    };
    assert!(
        tokio::time::timeout(Duration::from_secs(5), closed)
            .await
            .is_ok(),
        "connection must drop after revoke"
    );

    // Reconnect is refused (revoked, not merely unknown).
    let endpoint = s.endpoint(&paired).await;
    match RemoteClient::connect(&endpoint).await {
        Ok(_) => panic!("revoked device must not reconnect"),
        Err(r_code_client::ClientError::Handshake(code)) => {
            assert_eq!(code, "revoked");
        }
        Err(other) => panic!("expected a structured refusal, got: {other}"),
    }
}

/// Read the next raw message off a RemoteClient's socket (None on close).
async fn device_ws_next(client: &mut RemoteClient) -> Option<Message> {
    client.next_wire_message().await
}

#[tokio::test]
async fn r11_a3_listener_switch_drops_connections_but_keeps_devices() {
    let s = surface("a3").await;
    let paired = s
        .registry
        .register(
            "iPad",
            "ipados-pwa",
            &s.identity.fingerprint,
            CapabilitySet::read_only(),
        )
        .expect("pair");
    let mut device = RemoteClient::connect(&s.endpoint(&paired).await)
        .await
        .expect("connect");
    let _ = device
        .call("ping", serde_json::json!({}))
        .await
        .expect("alive");

    // Console disables the listener: every connection drops.
    s.local("device.setListener", serde_json::json!({"enabled": false}))
        .await
        .expect("disable");
    let closed = async {
        loop {
            match device.next_wire_message().await {
                Some(tokio_tungstenite::tungstenite::Message::Close(_)) => return true,
                Some(_) => continue,
                None => return true,
            }
        }
    };
    assert!(
        tokio::time::timeout(Duration::from_secs(5), closed)
            .await
            .is_ok(),
        "connections drop on setListener(false)"
    );
    // The device record survives.
    assert_eq!(s.registry.list().len(), 1);

    // Re-enable (with a device present): reconnect works again.
    s.local("device.setListener", serde_json::json!({"enabled": true}))
        .await
        .expect("enable");
    // The supervision loop closes the disabled listener; ensure_listener
    // restarts it on the management call. Give the port a beat.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut again = RemoteClient::connect(&s.endpoint(&paired).await).await;
    let mut attempts = 0;
    while again.is_err() && attempts < 20 {
        tokio::time::sleep(Duration::from_millis(300)).await;
        again = RemoteClient::connect(&s.endpoint(&paired).await).await;
        attempts += 1;
    }
    let mut again = again.expect("reconnect after re-enable");
    let _ = again
        .call("ping", serde_json::json!({}))
        .await
        .expect("alive again");
}
