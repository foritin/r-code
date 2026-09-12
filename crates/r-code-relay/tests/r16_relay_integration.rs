//! R16 — relay integration: one-time owner registration, device binding,
//! byte-level E2EE forwarding (opaque ciphertext passes through untouched),
//! rate limiting and a content-free audit trail.

use ed25519_dalek::SigningKey;
use futures_util::{SinkExt, StreamExt};
use r_code_relay::{sign_message, RelayState};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

async fn connect(port: u16) -> WebSocketStream<TcpStream> {
    let (ws, _) = tokio_tungstenite::client_async(
        format!("ws://127.0.0.1:{port}/relay"),
        TcpStream::connect(("127.0.0.1", port)).await.expect("tcp"),
    )
    .await
    .expect("ws");
    ws
}

async fn send_json(ws: &mut WebSocketStream<TcpStream>, value: serde_json::Value) {
    ws.send(Message::Text(value.to_string().into()))
        .await
        .expect("send");
}

async fn next_text(ws: &mut WebSocketStream<TcpStream>) -> serde_json::Value {
    loop {
        match ws.next().await {
            Some(Ok(Message::Text(text))) => {
                return serde_json::from_str(&text).expect("json");
            }
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            other => panic!("expected text frame, got {other:?}"),
        }
    }
}

struct OwnerPeer {
    signing: SigningKey,
    pubkey_b64: String,
    owner_id: String,
}

impl OwnerPeer {
    fn new() -> Self {
        let signing = SigningKey::generate(&mut rand_core::OsRng);
        let pubkey_b64 = b64(signing.verifying_key().as_bytes());
        let owner_id = {
            use sha2::{Digest, Sha256};
            let digest = Sha256::digest(signing.verifying_key().as_bytes());
            let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
            hex
        };
        Self {
            signing,
            pubkey_b64,
            owner_id,
        }
    }

    async fn register(&self, ws: &mut WebSocketStream<TcpStream>, code: &str) {
        send_json(
            ws,
            serde_json::json!({
                "type": "owner.register",
                "code": code,
                "pubkey": self.pubkey_b64,
                "sig": sign_message("rcode-owner-register", &self.pubkey_b64, &self.signing),
            }),
        )
        .await;
    }
}

async fn spawn_relay(
    port_requested: bool,
) -> (u16, Arc<RelayState>, Arc<Mutex<HashMap<String, bool>>>) {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind");
    let port = listener.local_addr().unwrap().port();
    let _requested = port_requested;
    let state = RelayState::new();
    let codes = Arc::new(Mutex::new(HashMap::new()));
    codes.lock().unwrap().insert("test-code-1".into(), true);
    codes.lock().unwrap().insert("test-code-2".into(), true);
    tokio::spawn(r_code_relay::serve(listener, state.clone(), codes.clone()));
    (port, state, codes)
}

#[tokio::test]
async fn r16_a1_register_bind_reject_forward_paths() {
    let (port, _state, _codes) = spawn_relay(true).await;
    let owner = OwnerPeer::new();

    // ── Register (one-time code) → welcome with the derived owner id.
    let mut owner_ws = connect(port).await;
    owner.register(&mut owner_ws, "test-code-1").await;
    let welcome = next_text(&mut owner_ws).await;
    assert_eq!(welcome["type"], "owner.welcome");
    assert_eq!(
        welcome["ownerId"], owner.owner_id,
        "owner id = hash(pubkey)"
    );

    // ── The same code can never register again.
    let mut second = connect(port).await;
    let second_owner = OwnerPeer::new();
    second_owner.register(&mut second, "test-code-1").await;
    let refused = next_text(&mut second).await;
    assert_eq!(refused["type"], "error");
    assert_eq!(refused["code"], "owner_code_used");

    // ── An unknown code is invalid.
    let third_owner = OwnerPeer::new();
    let mut third = connect(port).await;
    third_owner.register(&mut third, "no-such-code").await;
    let refused = next_text(&mut third).await;
    assert_eq!(refused["code"], "owner_code_invalid");
    drop(second);
    drop(third);

    // ── Device binding: the owner proves the binding with its key.
    let device_signing = SigningKey::generate(&mut rand_core::OsRng);
    let device_pubkey_b64 = b64(device_signing.verifying_key().as_bytes());
    send_json(
        &mut owner_ws,
        serde_json::json!({
            "type": "owner.bind",
            "devicePubkey": device_pubkey_b64,
            "sig": sign_message("rcode-owner-bind", &device_pubkey_b64, &owner.signing),
        }),
    )
    .await;

    // ── Unbound device (different key) → device_unbound.
    let rogue = SigningKey::generate(&mut rand_core::OsRng);
    let mut rogue_ws = connect(port).await;
    send_json(
        &mut rogue_ws,
        serde_json::json!({
            "type": "device.hello",
            "ownerId": owner.owner_id,
            "devicePubkey": b64(rogue.verifying_key().as_bytes()),
            "nonce": "n0",
        }),
    )
    .await;
    let refused = next_text(&mut rogue_ws).await;
    assert_eq!(refused["code"], "device_unbound");

    // ── Bound device: challenge → answer → welcome → bidirectional
    // forwarding with byte-for-byte ciphertext fidelity.
    let mut device_ws = connect(port).await;
    send_json(
        &mut device_ws,
        serde_json::json!({
            "type": "device.hello",
            "ownerId": owner.owner_id,
            "devicePubkey": device_pubkey_b64,
            "nonce": "n1",
        }),
    )
    .await;
    let challenge = next_text(&mut device_ws).await;
    assert_eq!(challenge["type"], "device.challenge");
    let answer = sign_message(
        "rcode-device-challenge",
        challenge["challenge"].as_str().unwrap_or_default(),
        &device_signing,
    );
    send_json(
        &mut device_ws,
        serde_json::json!({"type": "device.answer", "sig": answer}),
    )
    .await;
    let welcome = next_text(&mut device_ws).await;
    assert_eq!(welcome["type"], "device.welcome");

    // Device → owner: opaque binary ciphertext, byte-for-byte.
    let ciphertext: Vec<u8> = (0..64u8).collect();
    device_ws
        .send(Message::Binary(ciphertext.clone().into()))
        .await
        .expect("send up");
    let up = tokio::time::timeout(std::time::Duration::from_secs(5), owner_ws.next())
        .await
        .expect("owner receives within 5s");
    match up {
        Some(Ok(Message::Binary(received))) => assert_eq!(received, ciphertext, "bytes identical"),
        other => panic!("expected binary frame, got {other:?}"),
    }

    // Owner → device (downlink through the active bridge).
    let down_payload: Vec<u8> = (200..255u8).collect();
    owner_ws
        .send(Message::Binary(down_payload.clone().into()))
        .await
        .expect("send down");
    let down = tokio::time::timeout(std::time::Duration::from_secs(5), device_ws.next())
        .await
        .expect("device receives within 5s");
    match down {
        Some(Ok(Message::Binary(received))) => {
            assert_eq!(received, down_payload, "bytes identical")
        }
        other => panic!("expected binary frame, got {other:?}"),
    }

    // ── Owner offline: a fresh device gets the explicit code.
    drop(owner_ws);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let mut orphan = connect(port).await;
    send_json(
        &mut orphan,
        serde_json::json!({
            "type": "device.hello",
            "ownerId": owner.owner_id,
            "devicePubkey": device_pubkey_b64,
            "nonce": "n2",
        }),
    )
    .await;
    let refused = next_text(&mut orphan).await;
    assert_eq!(refused["code"], "owner_offline");
}

#[tokio::test]
async fn r16_a2_audit_is_content_free_and_rate_limit_enforced() {
    let (port, state, _codes) = spawn_relay(false).await;
    let owner = OwnerPeer::new();
    let mut owner_ws = connect(port).await;
    owner.register(&mut owner_ws, "test-code-2").await;
    let _welcome = next_text(&mut owner_ws).await;
    let device_signing = SigningKey::generate(&mut rand_core::OsRng);
    let device_pubkey_b64 = b64(device_signing.verifying_key().as_bytes());
    send_json(
        &mut owner_ws,
        serde_json::json!({
            "type": "owner.bind",
            "devicePubkey": device_pubkey_b64,
            "sig": sign_message("rcode-owner-bind", &device_pubkey_b64, &owner.signing),
        }),
    )
    .await;
    let mut device_ws = connect(port).await;
    send_json(
        &mut device_ws,
        serde_json::json!({
            "type": "device.hello",
            "ownerId": owner.owner_id,
            "devicePubkey": device_pubkey_b64,
            "nonce": "n",
        }),
    )
    .await;
    let challenge = next_text(&mut device_ws).await;
    send_json(
        &mut device_ws,
        serde_json::json!({
            "type": "device.answer",
            "sig": sign_message(
                "rcode-device-challenge",
                challenge["challenge"].as_str().unwrap_or_default(),
                &device_signing,
            ),
        }),
    )
    .await;
    let _welcome = next_text(&mut device_ws).await;

    // Push a known random plaintext through the relay; the audit trail must
    // never contain it (content-free metadata only).
    let known_plaintext = b"SECRET-PLAINTEXT-MUST-NOT-LEAK-0123456789".to_vec();
    for _ in 0..3 {
        device_ws
            .send(Message::Binary(known_plaintext.clone().into()))
            .await
            .expect("send");
    }
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let audit_dump = {
        let audit = state.audit.lock().unwrap();
        format!("{audit:?}")
    };
    assert!(
        !audit_dump.contains("SECRET-PLAINTEXT"),
        "audit must not contain forwarded content"
    );
    // Entries are metadata (direction/identity/byte-count) only — the
    // content-free property was asserted above on the serialized dump.
    assert!(!state.audit.lock().unwrap().is_empty());

    // Rate limit: the sustained cap is 20 frames/s (10 sustained + 10
    // burst). Sending 40 frames back-to-back must trip it — the bridge
    // closes and the audit records the rate disconnect.
    for index in 0..40u8 {
        let payload = vec![index; 32];
        if device_ws
            .send(Message::Binary(payload.into()))
            .await
            .is_err()
        {
            break;
        }
    }
    let closed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match device_ws.next().await {
                Some(Ok(Message::Close(_))) | None => return true,
                Some(Ok(_)) => continue,
                Some(Err(_)) => return true,
            }
        }
    })
    .await;
    assert!(
        matches!(closed, Ok(true)),
        "bridge must close on sustained over-rate"
    );
    let audit_dump = format!("{:?}", state.audit.lock().unwrap());
    assert!(
        audit_dump.contains("device-disconnected-rate"),
        "rate disconnect audited"
    );
}
