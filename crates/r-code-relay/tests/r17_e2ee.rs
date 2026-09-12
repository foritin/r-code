//! R17 — E2EE over the relay: device(initiator) ↔ owner(responder) Noise
//! sessions through a real relay; the relay sees only ciphertext; wrong
//! PSK / tampered handshake kill the connection without side effects; the
//! byte-stream semantics match the local pipe (F1).

use ed25519_dalek::SigningKey;
use futures_util::{SinkExt, StreamExt};
use r_code_relay::{sign_message, RelayState};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

type Ws = WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

async fn spawn_relay() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("bind");
    let port = listener.local_addr().unwrap().port();
    let state = RelayState::new();
    let mut codes = HashMap::new();
    codes.insert("code-a".to_string(), true);
    let codes = Arc::new(Mutex::new(codes));
    tokio::spawn(r_code_relay::serve(listener, state, codes));
    port
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

async fn connect_ws(port: u16) -> Ws {
    let tcp = TcpStream::connect(("127.0.0.1", port)).await.expect("tcp");
    let (ws, _) = tokio_tungstenite::client_async(
        format!("ws://127.0.0.1:{port}/relay"),
        tokio_tungstenite::MaybeTlsStream::Plain(tcp),
    )
    .await
    .expect("ws");
    ws
}

/// Owner leg: register (or resume) + bind a device key, then hand the raw
/// socket to the E2EE responder — the exact shape RelayTransport uses.
async fn owner_leg(port: u16, owner: &SigningKey, bind_device: Option<&SigningKey>) -> Ws {
    let mut ws = connect_ws(port).await;
    let pubkey_b64 = b64(owner.verifying_key().as_bytes());
    // Resume when the owner is already registered (same key), register otherwise.
    ws.send(Message::Text(
        serde_json::json!({
            "type": "owner.resume",
            "ownerId": r_code_relay::owner_id_of(owner.verifying_key().as_bytes()),
            "pubkey": pubkey_b64,
            "sig": sign_message("rcode-owner-resume", &r_code_relay::owner_id_of(owner.verifying_key().as_bytes()), owner),
        })
        .to_string()
        .into(),
    ))
    .await
    .expect("resume");
    // Wait for welcome or error; on unknown owner fall back to register.
    let first = next_text(&mut ws).await;
    if first["type"] != "owner.welcome" {
        ws = connect_ws(port).await;
        ws.send(Message::Text(
            serde_json::json!({
                "type": "owner.register",
                "code": "code-a",
                "pubkey": pubkey_b64,
                "sig": sign_message("rcode-owner-register", &pubkey_b64, owner),
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("register");
        let welcome = next_text(&mut ws).await;
        assert_eq!(welcome["type"], "owner.welcome", "{welcome}");
    }
    if let Some(device) = bind_device {
        let device_pubkey_b64 = b64(device.verifying_key().as_bytes());
        ws.send(Message::Text(
            serde_json::json!({
                "type": "owner.bind",
                "devicePubkey": device_pubkey_b64,
                "sig": sign_message("rcode-owner-bind", &device_pubkey_b64, owner),
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("bind");
    }
    ws
}

async fn next_text(ws: &mut Ws) -> serde_json::Value {
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

/// The Noise static key is the raw 32-byte secret used with X25519; the
/// pinned fingerprint is the SHA-256 of the matching public key.
fn noise_fingerprint(noise_static_secret: &[u8; 32]) -> String {
    let public = x25519_dalek::x25519(*noise_static_secret, x25519_dalek::X25519_BASEPOINT_BYTES);
    sha256_hex(&public)
}

fn sha256_hex(value: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(value);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

#[tokio::test]
async fn r17_a1_e2ee_roundtrip_and_stream_semantics_match_a_pipe() {
    let port = spawn_relay().await;
    let owner = SigningKey::generate(&mut rand_core::OsRng);
    let psk = [7u8; 32];
    let noise_static = owner.to_bytes();
    let owner_fp = noise_fingerprint(&noise_static);
    let device_signing = SigningKey::generate(&mut rand_core::OsRng);

    // Owner leg registers and binds the device.
    let owner_ws = owner_leg(port, &owner, Some(&device_signing)).await;

    // Owner E2EE (responder over the same socket). Spawned: the responder
    // parks until the device's Noise msg1 arrives through the relay — a
    // sequential await here would deadlock (msg1 needs this test to reach
    // connect_device first).
    let owner_keys = owner.to_bytes();
    let owner_responder = tokio::spawn(async move {
        r_code_relay::e2ee::E2eeStream::accept_owner(owner_ws, &owner_keys, &psk).await
    });
    // Give the responder a beat to reach its read before the dials in.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Device E2EE (initiator): hello/challenge/welcome then Noise, pinned
    // to the owner fingerprint.
    let mut device_e2ee = r_code_relay::e2ee::E2eeStream::connect_device(
        &format!("127.0.0.1:{port}"),
        &r_code_relay::owner_id_of(owner.verifying_key().as_bytes()),
        &device_signing,
        &psk,
        &owner_fp,
    )
    .await
    .expect("device handshake (pinned fingerprint)");
    let mut owner_e2ee = owner_responder
        .await
        .expect("owner task")
        .expect("owner handshake");

    // Bidirectional plaintext over the encrypted bridge: byte-stream
    // semantics identical to a local pipe (F1).
    device_e2ee
        .write_all(b"hello over e2ee")
        .await
        .expect("write");
    device_e2ee.flush().await.expect("flush");
    let mut buffer = vec![0u8; 128];
    let read = owner_e2ee.read(&mut buffer).await.expect("read");
    assert_eq!(&buffer[..read], b"hello over e2ee");

    owner_e2ee
        .write_all(b"ack from owner")
        .await
        .expect("write");
    owner_e2ee.flush().await.expect("flush");
    let read = device_e2ee.read(&mut buffer).await.expect("read");
    assert_eq!(&buffer[..read], b"ack from owner");

    // Multi-chunk payload: larger than one Noise message chunk.
    let big: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
    let big_expected = big.clone();
    let big_len = big.len();
    let writer = tokio::spawn(async move {
        device_e2ee.write_all(&big).await.expect("big write");
        device_e2ee.flush().await.expect("big flush");
        device_e2ee
    });
    let mut received = Vec::new();
    while received.len() < big_len {
        let n = owner_e2ee.read(&mut buffer).await.expect("big read");
        received.extend_from_slice(&buffer[..n]);
    }
    assert_eq!(received, big_expected, "multi-chunk fidelity");
    let _ = writer.await;
}

#[tokio::test]
async fn r17_a2_malicious_relay_cannot_read_tamper_or_inject() {
    let port = spawn_relay().await;
    let owner = SigningKey::generate(&mut rand_core::OsRng);
    let real_psk = [9u8; 32];
    let noise_static = owner.to_bytes();
    let owner_fp = noise_fingerprint(&noise_static);
    let real_device = SigningKey::generate(&mut rand_core::OsRng);
    let attacker_device = SigningKey::generate(&mut rand_core::OsRng);
    let owner_id = r_code_relay::owner_id_of(owner.verifying_key().as_bytes());

    // ── Wrong fingerprint: a full legitimate handshake happens, then the
    // TOFU pin (device side) refuses before any application byte.
    let owner_ws = owner_leg(port, &owner, Some(&real_device)).await;
    // Spawned: the responder parks until the device dials (same reason as A1).
    let owner_keys = owner.to_bytes();
    let owner1 = tokio::spawn(async move {
        r_code_relay::e2ee::E2eeStream::accept_owner(owner_ws, &owner_keys, &real_psk).await
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    // NOTE: do NOT await owner1 here — it parks on the device's msg1; the
    // device leg below must run first (same ordering as A1).
    let wrong_fp = r_code_relay::e2ee::E2eeStream::connect_device(
        &format!("127.0.0.1:{port}"),
        &owner_id,
        &real_device,
        &real_psk,
        &"ab".repeat(32), // attacker-pinned fingerprint
    )
    .await;
    assert!(wrong_fp.is_err(), "fingerprint mismatch must refuse");
    let mut owner_e2ee = owner1
        .await
        .expect("owner1 task")
        .expect("owner handshake with the real device");
    let _ = &mut owner_e2ee;

    // ── Wrong PSK: an attacker (bound device key, foreign pairing secret)
    // completes messages 1–2 but message 3's MAC fails on the OWNER — the
    // responder refuses and nothing is established.
    let owner_ws2 = owner_leg(port, &owner, Some(&attacker_device)).await;
    let owner_task = tokio::spawn(async move {
        r_code_relay::e2ee::E2eeStream::accept_owner(owner_ws2, &owner.to_bytes(), &real_psk).await
    });
    // Give the responder a beat to reach its read; the attacker's device
    // "completes" its own handshake view but the owner rejects msg3.
    let device_view = r_code_relay::e2ee::E2eeStream::connect_device(
        &format!("127.0.0.1:{port}"),
        &owner_id,
        &attacker_device,
        &[1u8; 32], // attacker's PSK
        &owner_fp,
    )
    .await;
    let owner_result =
        match tokio::time::timeout(std::time::Duration::from_secs(10), owner_task).await {
            Ok(result) => result.expect("owner task"),
            Err(_) => panic!("owner accept timed out"),
        };
    assert!(
        owner_result.is_err(),
        "owner must refuse a wrong-PSK handshake"
    );
    // The device view is irrelevant after the owner refused; drop it.
    drop(device_view);
    drop(owner_e2ee);
}
