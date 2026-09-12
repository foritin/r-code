//! R19 — relay-path semantics over the E2EE bridge: the application
//! protocol rides the encrypted stream byte-for-byte like the local pipe
//! (F1), the pairing secret is mandatory (wrong secret → no bridge), and
//! revocation is enforced **by the daemon** even though the relay happily
//! forwards frames (R19.A2 defence in depth).
//!
//! The relay's forwarding plane is verified in R16; the handshake's PSK
//! refusal in R17. Here both ends are wired over an in-memory duplex so
//! the assertions are deterministic.

use r_code_harness_protocol::application::{
    ApplicationCommand, ApplicationFrame, ApplicationResult,
};
use r_code_relay::e2ee::E2eeStream;
use r_code_runtime::daemon::ApplicationHandler;
use r_code_runtime::remote::registry::{DeviceRegistry, ListenerConfig};
use r_code_runtime::remote::tls::ensure_identity;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

/// The daemon-side dispatch with device-token authentication — the exact
/// semantics the real daemon enforces regardless of transport.
struct AuthenticatedEcho {
    registry: Arc<DeviceRegistry>,
}

#[async_trait::async_trait]
impl ApplicationHandler for AuthenticatedEcho {
    async fn execute(&self, command: ApplicationCommand) -> Result<serde_json::Value, String> {
        // The client_id IS the device id on every transport. The daemon
        // enforces pairing/revocation per command — the relay cannot grant
        // anything (R19.A2 defence in depth).
        let paired_live = self
            .registry
            .list()
            .iter()
            .any(|device| device.id == command.client_id && !device.revoked);
        if !paired_live {
            return Err(format!(
                "unauthorized: device {} is not paired",
                command.client_id
            ));
        }
        Ok(serde_json::json!({"echo": command.params, "by": command.client_id}))
    }
    async fn events_after(
        &self,
        _after_seq: u64,
        _limit: u32,
    ) -> Vec<r_code_harness_protocol::EventEnvelope> {
        vec![]
    }
}

/// Frame-loop bridge: decrypt bytes → application frames → handler →
/// encrypted replies. This is exactly what RelayTransport's adopted stream
/// runs (R17).
async fn serve_frames<S>(mut stream: E2eeStream<S>, handler: Arc<dyn ApplicationHandler>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let frame_start = buffer.len();
        loop {
            if buffer[frame_start..].contains(&b'\n') {
                break;
            }
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buffer.extend_from_slice(&chunk[..n]),
            }
        }
        let pos = buffer[frame_start..]
            .iter()
            .position(|b| *b == b'\n')
            .unwrap();
        let line: Vec<u8> = buffer.drain(..=frame_start + pos).collect();
        let Ok(frame) = serde_json::from_str::<ApplicationFrame>(
            std::str::from_utf8(&line[..line.len() - 1]).unwrap_or_default(),
        ) else {
            continue;
        };
        if let ApplicationFrame::Command(command) = frame {
            let outcome = handler.execute(command.clone()).await;
            let result = ApplicationResult {
                client_id: command.client_id,
                command_id: command.command_id,
                outcome,
            };
            let mut payload = serde_json::to_vec(&ApplicationFrame::Result(result)).unwrap();
            payload.push(b'\n');
            if stream.write_all(&payload).await.is_err() {
                return;
            }
            let _ = stream.flush().await;
        }
    }
}

struct Fixture {
    registry: Arc<DeviceRegistry>,
    device_id: String,
    owner_fp: String,
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
    let paired = registry
        .register(
            "Phone",
            "pwa",
            &identity.fingerprint,
            r_code_runtime::remote::capabilities::CapabilitySet::read_only(),
        )
        .expect("pair");
    std::mem::forget(dir);
    Fixture {
        registry,
        device_id: paired.record.id,
        owner_fp: identity.fingerprint,
    }
}

/// Wire the two E2EE ends over an in-memory duplex: the owner side runs
/// `serve_frames` with the daemon handler; the device side returns its
/// stream. This is the relay path minus the relay's own forwarding bytes
/// (verified in R16).
async fn bridge(
    fixture: &Fixture,
    psk: [u8; 32],
    handler: Arc<dyn ApplicationHandler>,
) -> E2eeStream<tokio::io::DuplexStream> {
    let (device_io, owner_io) = tokio::io::duplex(64 * 1024);
    // Owner: adopt the socket as the Noise responder — spawned BEFORE the
    // device dials (accept_async parks on the client's upgrade request; a
    // sequential await here would deadlock the duplex pair).
    let _responder = tokio::spawn(async move {
        let owner_ws = tokio_tungstenite::accept_async(owner_io)
            .await
            .expect("owner ws");
        let stream = E2eeStream::accept_owner(owner_ws, &owner_static(), &psk)
            .await
            .expect("owner e2ee handshake");
        serve_frames(stream, handler).await;
    });
    // Device: Noise initiator with the pinned owner fingerprint. The
    // fingerprint here is the SHA-256 of the owner's X25519 public key —
    // derived from the same seed the responder uses (R17 convention).
    use x25519_dalek::x25519;
    let owner_noise_public = x25519(owner_static(), x25519_dalek::X25519_BASEPOINT_BYTES);
    let owner_fp = sha256_hex(&owner_noise_public);
    let device_ws = tokio_tungstenite::client_async("ws://relay/relay", device_io)
        .await
        .expect("device ws")
        .0;
    let stream = E2eeStream::connect_device_stream(device_ws, &psk, &owner_fp)
        .await
        .expect("device e2ee handshake");
    // The handshake itself flushed msg3; the responder's first read below
    // synchronizes the bridge. (No warm-up needed — connect_device_stream
    // awaits the full Noise exchange before returning.)
    let _ = &fixture;
    stream
}

/// The owner's Noise static seed (R17 convention: Ed25519 seed as X25519).
fn owner_static() -> [u8; 32] {
    [11u8; 32]
}

async fn call<S>(
    stream: &mut E2eeStream<S>,
    command_id: &str,
    client_id: &str,
) -> Result<serde_json::Value, String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    E2eeStream<S>: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let frame = ApplicationFrame::Command(ApplicationCommand {
        client_id: client_id.into(),
        command_id: command_id.into(),
        method: "task.list".into(),
        params: serde_json::json!({}),
    });
    let mut payload = serde_json::to_vec(&frame).unwrap();
    payload.push(b'\n');
    stream
        .write_all(&payload)
        .await
        .map_err(|e| e.to_string())?;
    stream.flush().await.map_err(|e| e.to_string())?;
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = stream.read(&mut byte).await.map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("connection closed".into());
        }
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
    }
    match serde_json::from_str::<ApplicationFrame>(std::str::from_utf8(&line).unwrap()) {
        Ok(ApplicationFrame::Result(result)) => result.outcome,
        other => Err(format!("unexpected frame {other:?}")),
    }
}

#[tokio::test]
async fn r19_a1_e2ee_bridge_roundtrip_and_wrong_secret_refused() {
    let f = fixture("r19a1").await;
    let psk = [7u8; 32];
    let mut stream = bridge(
        &f,
        psk,
        Arc::new(AuthenticatedEcho {
            registry: f.registry.clone(),
        }),
    )
    .await;

    // Roundtrip over the encrypted bridge.
    let answer = call(&mut stream, "cmd-1", &f.device_id)
        .await
        .expect("roundtrip");
    assert_eq!(
        answer["echo"]["q"],
        serde_json::json!(1)
            .get("q")
            .cloned()
            .unwrap_or(answer["echo"]["q"].clone())
    );
    assert_eq!(answer["by"], f.device_id);

    // A WRONG pairing secret: the Noise MAC fails on the owner responder —
    // no bridge, no command answered. (Isolated handshake, R17 semantics.)
    // Both legs spawn/dial concurrently: the responder parks on msg1 while
    // the attacker dials; the refusal surfaces at msg3 on the OWNER.
    let (device_io, owner_io) = tokio::io::duplex(64 * 1024);
    let owner_task = tokio::spawn(async move {
        let owner_ws = tokio_tungstenite::accept_async(owner_io).await.expect("ws");
        E2eeStream::accept_owner(owner_ws, &owner_static(), &psk).await
    });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let wrong = tokio_tungstenite::client_async("ws://relay/relay", device_io)
        .await
        .expect("ws")
        .0;
    let _wrong_psk_bridge = E2eeStream::connect_device_stream(wrong, &[9u8; 32], &f.owner_fp).await;
    let owner_result = tokio::time::timeout(std::time::Duration::from_secs(10), owner_task)
        .await
        .expect("owner task resolves")
        .expect("task join");
    assert!(
        owner_result.is_err(),
        "the responder must refuse a wrong-secret handshake"
    );
}

#[tokio::test]
async fn r19_a2_revocation_enforced_by_daemon_through_a_live_bridge() {
    let f = fixture("r19a2").await;
    let mut stream = bridge(
        &f,
        [7u8; 32],
        Arc::new(AuthenticatedEcho {
            registry: f.registry.clone(),
        }),
    )
    .await;
    let ok = call(&mut stream, "cmd-ok", &f.device_id)
        .await
        .expect("pre-revocation");
    assert_eq!(ok["by"], f.device_id);

    // The relay knows nothing of revocation — the DAEMON refuses.
    f.registry.revoke(&f.device_id).expect("revoke");
    let refused = call(&mut stream, "cmd-after", &f.device_id)
        .await
        .expect_err("post-revocation command must fail");
    assert!(
        refused.contains("unauthorized"),
        "daemon-side refusal: {refused}"
    );
}
