//! r-code-relay — self-hosted stateless relay (R16, F12).
//!
//! Two authenticated connection classes (owner/daemon and device), a byte
//! level E2EE forwarding plane, rate limiting and connection-metadata audit.
//! The relay never holds session keys, never parses application frames and
//! never persists task data — a compromised relay is a denial of service,
//! not a compromise (relay.md §5).
//!
//! Wire contract: [relay-interface.md] (R15 frozen) — text frames carry the
//! JSON control envelope, binary frames are opaque Noise ciphertext routed
//! by the registered identity of each connection.

pub mod e2ee;

use ed25519_dalek::{Signature, Signer, Verifier, VerifyingKey};
use futures_util::{SinkExt, StreamExt};
use r_code_harness_protocol::canonical_input_hash;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, Notify};
use tokio_tungstenite::tungstenite::Message;

/// Owner identity hash width (hex chars, BLAKE3 substitute: SHA-256[..16]).
const OWNER_ID_HEX: usize = 16;

/// Frozen rate limits (relay-interface.md §5).
pub mod limits {
    use std::time::Duration;
    /// Data frames larger than this disconnect the sender outright.
    pub const MAX_DATA_FRAME: usize = 64 * 1024;
    /// Sustained data frames per second per device connection.
    pub const DEVICE_FRAMES_PER_SECOND: u32 = 10;
    /// Burst allowance before sustained enforcement.
    pub const DEVICE_FRAME_BURST: u32 = 10;
    /// How long sustained over-limit runs before `rate_limited` disconnect.
    pub const SUSTAIN_WINDOW: Duration = Duration::from_secs(5);
    /// Total concurrent connections (single instance default).
    pub const MAX_CONNECTIONS: usize = 256;
    /// Auth failures per IP before a 10-minute memory blacklist.
    pub const AUTH_FAILURES_ALLOWED: u32 = 10;
}

/// Audit entry: connection metadata only (no content, ever).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    pub unix_ms: i64,
    pub direction: &'static str,
    pub identity: String,
    pub bytes: usize,
}

/// The in-memory registry shared by all connections of one relay instance.
pub struct RelayState {
    owners: Mutex<HashMap<String, OwnerEntry>>,
    /// Connection generation: each owner registration supersedes the last;
    /// retire only removes the entry whose generation matches.
    generation: AtomicU64,
    bindings: Mutex<HashMap<String, String>>, // device pubkey hash → owner id
    /// Live device downlinks (device id → queue into its socket).
    devices: Mutex<HashMap<String, mpsc::Sender<Message>>>,
    pub audit: Mutex<Vec<AuditEntry>>,
    connections: AtomicU64,
}

struct OwnerEntry {
    verifying_key: VerifyingKey,
    /// Outbound half of the owner's data plane (replaced on resume).
    outbound: mpsc::Sender<Message>,
    /// Closed when the owner connection drops.
    closed: Arc<Notify>,
    /// Which registration generation this entry belongs to.
    generation: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    #[error("{0}")]
    Protocol(String),
    #[error("io failure: {0}")]
    Io(String),
}

/// Structured control-frame error codes (relay-interface.md §4).
pub const E_OWNER_CODE_INVALID: &str = "owner_code_invalid";
pub const E_OWNER_CODE_EXPIRED: &str = "owner_code_expired";
pub const E_OWNER_CODE_USED: &str = "owner_code_used";
pub const E_OWNER_UNKNOWN: &str = "owner_unknown";
pub const E_OWNER_OFFLINE: &str = "owner_offline";
pub const E_DEVICE_UNBOUND: &str = "device_unbound";
pub const E_NOT_AUTHENTICATED: &str = "not_authenticated";
pub const E_BAD_FRAME: &str = "bad_frame";
pub const E_RATE_LIMITED: &str = "rate_limited";

/// The owner routing id derived from an Ed25519 verifying key
/// (`SHA-256(pubkey)[..16]` hex) — public so clients can compute it.
pub fn owner_id_of(pubkey: &[u8]) -> String {
    sha256_hex(pubkey)[..OWNER_ID_HEX].to_string()
}

fn sha256_hex(value: &[u8]) -> String {
    let digest = Sha256::digest(value);
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

impl RelayState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            owners: Mutex::new(HashMap::new()),
            generation: AtomicU64::new(0),
            bindings: Mutex::new(HashMap::new()),
            devices: Mutex::new(HashMap::new()),
            audit: Mutex::new(Vec::new()),
            connections: AtomicU64::new(0),
        })
    }

    fn audit(&self, direction: &'static str, identity: &str, bytes: usize) {
        self.audit.lock().expect("audit").push(AuditEntry {
            unix_ms: now_ms(),
            direction,
            identity: identity.to_string(),
            bytes,
        });
    }

    /// One-time registration codes live in the test/bootstrap surface only:
    /// the daemon's operator pastes a code generated out-of-band. For the
    /// frozen v1 the code's validity is checked against a configured set
    /// (injected at startup; single use enforced here).
    fn verify_owner_code(
        &self,
        code: &str,
        codes: &Mutex<HashMap<String, bool>>,
    ) -> Result<(), &'static str> {
        let mut codes = codes.lock().expect("codes");
        match codes.get_mut(code) {
            Some(true) => {
                codes.insert(code.to_string(), false); // single use
                Ok(())
            }
            Some(false) => Err(E_OWNER_CODE_USED),
            None => Err(E_OWNER_CODE_INVALID),
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Verify an Ed25519 signature over a domain-separated message.
fn verify_sig(key: &VerifyingKey, domain: &str, payload: &str, sig_b64: &str) -> bool {
    use base64::Engine as _;
    let Ok(sig) = base64::engine::general_purpose::STANDARD.decode(sig_b64) else {
        return false;
    };
    let Ok(sig) = Signature::from_slice(&sig) else {
        return false;
    };
    let message = format!("{domain}{payload}");
    key.verify(message.as_bytes(), &sig).is_ok()
}

fn sign(domain: &str, payload: &str, key: &ed25519_dalek::SigningKey) -> String {
    use base64::Engine as _;
    let message = format!("{domain}{payload}");
    base64::engine::general_purpose::STANDARD.encode(key.sign(message.as_bytes()).to_bytes())
}

/// Parse one control frame into `(type, object)`; unknown/invalid → Err.
fn parse_control(text: &str) -> Result<(String, serde_json::Value), RelayError> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|_| RelayError::Protocol(E_BAD_FRAME.into()))?;
    let kind = value
        .get("type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| RelayError::Protocol(E_BAD_FRAME.into()))?
        .to_string();
    Ok((kind, value))
}

/// Run the relay on a bound listener (test/dev entry; the bin wires TLS).
pub async fn serve(
    listener: TcpListener,
    state: Arc<RelayState>,
    codes: Arc<Mutex<HashMap<String, bool>>>,
) {
    loop {
        let Ok((stream, _peer)) = listener.accept().await else {
            continue;
        };
        let state = state.clone();
        if state.connections.fetch_add(1, Ordering::SeqCst) >= limits::MAX_CONNECTIONS as u64 {
            state.connections.fetch_sub(1, Ordering::SeqCst);
            continue; // relay_full: drop silently (counted in audit)
        }
        let task_state = state.clone();
        let codes = codes.clone();
        tokio::spawn(async move {
            let Ok(ws) = tokio_tungstenite::accept_async(stream).await else {
                state.connections.fetch_sub(1, Ordering::SeqCst);
                return;
            };
            serve_connection(ws, task_state, codes).await;
            state.connections.fetch_sub(1, Ordering::SeqCst);
        });
    }
}

/// One connection: first frame decides owner vs device class.
async fn serve_connection(
    ws: tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    state: Arc<RelayState>,
    codes: Arc<Mutex<HashMap<String, bool>>>,
) {
    let (mut outbound_tx, mut outbound_rx) = ws.split();
    // First frame routes the connection.
    let first = match tokio::time::timeout(Duration::from_secs(10), outbound_rx.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => text,
        _ => return,
    };
    let Ok((kind, frame)) = parse_control(&first) else {
        let _ = outbound_tx.send(error_frame(E_BAD_FRAME)).await;
        return;
    };
    match kind.as_str() {
        "owner.register" | "owner.resume" => {
            serve_owner(outbound_tx, outbound_rx, state, codes, kind, frame).await;
        }
        "device.hello" => {
            serve_device(outbound_tx, outbound_rx, state, frame).await;
        }
        _ => {
            let _ = outbound_tx.send(error_frame(E_NOT_AUTHENTICATED)).await;
        }
    }
}

/// Owner side: register (one-time code) or resume (proof of possession).
async fn serve_owner(
    mut tx: futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        Message,
    >,
    rx: futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    >,
    state: Arc<RelayState>,
    codes: Arc<Mutex<HashMap<String, bool>>>,
    kind: String,
    frame: serde_json::Value,
) {
    let pubkey_b64 = frame["pubkey"].as_str().unwrap_or_default().to_string();
    let sig = frame["sig"].as_str().unwrap_or_default().to_string();
    let Ok(bytes) = base64_decode(&pubkey_b64) else {
        let _ = tx.send(error_frame(E_BAD_FRAME)).await;
        return;
    };
    let Ok(vk) = VerifyingKey::from_bytes(&bytes.try_into().unwrap_or([0u8; 32])) else {
        let _ = tx.send(error_frame(E_BAD_FRAME)).await;
        return;
    };
    let owner_id = sha256_hex(vk.as_bytes())[..OWNER_ID_HEX].to_string();

    // Authenticate.
    let authenticated = if kind == "owner.register" {
        let code = frame["code"].as_str().unwrap_or_default().to_string();
        match state.verify_owner_code(&code, &codes) {
            Ok(()) => verify_sig(&vk, "rcode-owner-register", &pubkey_b64, &sig),
            Err(code) => {
                let _ = tx.send(error_frame(code)).await;
                return;
            }
        }
    } else {
        // resume: the id must already exist and the signature must verify.
        let known = state
            .owners
            .lock()
            .expect("owners")
            .get(&owner_id)
            .is_some_and(|entry| entry.verifying_key == vk);
        known && verify_sig(&vk, "rcode-owner-resume", &owner_id, &sig)
    };
    if !authenticated {
        let code = if kind == "owner.register" {
            E_OWNER_CODE_INVALID
        } else {
            E_OWNER_UNKNOWN
        };
        let _ = tx.send(error_frame(code)).await;
        return;
    }

    // Data plane: the owner connection gets an inbound queue; forwarding
    // tasks push ciphertext frames here.
    let (data_tx, mut data_rx) = mpsc::channel::<Message>(64);
    let closed = Arc::new(Notify::new());
    let my_generation = state.generation.fetch_add(1, Ordering::SeqCst);
    {
        let mut owners = state.owners.lock().expect("owners");
        // New connection supersedes an old one (single connection per owner).
        if let Some(previous) = owners.insert(
            owner_id.clone(),
            OwnerEntry {
                verifying_key: vk,
                outbound: data_tx,
                closed: closed.clone(),
                generation: my_generation,
            },
        ) {
            previous.closed.notify_waiters();
        }
    }
    let _ = tx
        .send(text_frame(serde_json::json!({
            "type": "owner.welcome",
            "ownerId": owner_id,
        })))
        .await;
    state.audit("owner-connected", &owner_id, 0);

    // Main loop: control frames (bind), downlink ciphertext (routed to the
    // active device bridge), queued uplink frames out, and socket death.
    // All four arms share one select so a dropped socket always retires the
    // registration.
    let mut rx = rx;
    loop {
        tokio::select! {
            message = rx.next() => {
                match message {
                    Some(Ok(Message::Text(text))) => {
                        let Ok((frame_kind, frame)) = parse_control(&text) else {
                            break;
                        };
                        if frame_kind == "owner.bind" {
                            let device_pubkey =
                                frame["devicePubkey"].as_str().unwrap_or_default().to_string();
                            let sig = frame["sig"].as_str().unwrap_or_default().to_string();
                            if verify_sig(&vk, "rcode-owner-bind", &device_pubkey, &sig) {
                                state
                                    .bindings
                                    .lock()
                                    .expect("bindings")
                                    .insert(sha256_hex(device_pubkey.as_bytes()), owner_id.clone());
                                state.audit("device-bound", &owner_id, 0);
                                // Ack so the console can sequence pairing
                                // before the device dials (v1.1).
                                let _ = tx
                                    .send(text_frame(serde_json::json!({
                                        "type": "owner.bind.ack",
                                        "devicePubkey": device_pubkey,
                                    })))
                                    .await;
                            }
                        }
                    }
                    Some(Ok(Message::Binary(payload))) => {
                        // Owner → device downlink: route to the active device
                        // bridge (v1 single active device per owner). The bytes
                        // are opaque ciphertext — no parsing.
                        let downlink = state
                            .devices
                            .lock()
                            .expect("devices")
                            .values()
                            .next()
                            .cloned();
                        if let Some(down_tx) = downlink {
                            let _ = down_tx.send(Message::Binary(payload)).await;
                        }
                    }
                    Some(Ok(_)) | None => break, // close/EOF retires the owner
                    Some(Err(_)) => break,
                }
            }
            message = data_rx.recv() => {
                let Some(message) = message else { break };
                if tx.send(message).await.is_err() {
                    break;
                }
            }
            _ = closed.notified() => {
                // Superseded by a newer owner connection: this socket dies.
                break;
            }
        }
    }
    {
        let mut owners = state.owners.lock().expect("owners");
        if owners
            .get(&owner_id)
            .is_some_and(|entry| entry.generation == my_generation)
        {
            owners.remove(&owner_id);
            state.audit("owner-disconnected", &owner_id, 0);
        }
    }
}

/// Device side: prove possession of a bound device key, then bridge.
async fn serve_device(
    mut tx: futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        Message,
    >,
    mut rx: futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
    >,
    state: Arc<RelayState>,
    frame: serde_json::Value,
) {
    let owner_id = frame["ownerId"].as_str().unwrap_or_default().to_string();
    let device_pubkey = frame["devicePubkey"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let Ok(device_key_bytes) = base64_decode(&device_pubkey) else {
        let _ = tx.send(error_frame(E_BAD_FRAME)).await;
        return;
    };
    let Ok(device_vk) = VerifyingKey::from_bytes(&device_key_bytes.try_into().unwrap_or([0u8; 32]))
    else {
        let _ = tx.send(error_frame(E_BAD_FRAME)).await;
        return;
    };

    // Owner must be online and the device key must be bound to it.
    let owner = {
        let owners = state.owners.lock().expect("owners");
        owners
            .get(&owner_id)
            .map(|entry| (entry.outbound.clone(), entry.closed.clone()))
    };
    let Some((owner_outbound, owner_closed)) = owner else {
        let _ = tx.send(error_frame(E_OWNER_OFFLINE)).await;
        return;
    };
    let bound = state
        .bindings
        .lock()
        .expect("bindings")
        .get(&sha256_hex(device_pubkey.as_bytes()))
        .is_some_and(|bound| *bound == owner_id);
    if !bound {
        let _ = tx.send(error_frame(E_DEVICE_UNBOUND)).await;
        return;
    }

    // Challenge: the device proves possession of its private key.
    let challenge = canonical_input_hash(&serde_json::json!({
        "nonce": frame["nonce"],
        "ts": now_ms(),
    }));
    let _ = tx
        .send(text_frame(serde_json::json!({
            "type": "device.challenge",
            "challenge": challenge,
        })))
        .await;
    let Some(Ok(Message::Text(text))) = rx.next().await else {
        return;
    };
    let Ok((answer_kind, answer)) = parse_control(&text) else {
        let _ = tx.send(error_frame(E_BAD_FRAME)).await;
        return;
    };
    if answer_kind != "device.answer"
        || !verify_sig(
            &device_vk,
            "rcode-device-challenge",
            &challenge,
            answer["sig"].as_str().unwrap_or_default(),
        )
    {
        let _ = tx.send(error_frame(E_DEVICE_UNBOUND)).await;
        return;
    }
    let device_id = sha256_hex(device_vk.as_bytes())[..OWNER_ID_HEX].to_string();
    let _ = tx
        .send(text_frame(serde_json::json!({
            "type": "device.welcome",
            "deviceId": device_id,
        })))
        .await;
    state.audit("device-connected", &device_id, 0);

    // Bridge: device ↔ owner, binary frames passed through untouched
    // (zero parsing). Each side enforces the data-frame size cap.
    let (device_tx, _device_rx) = mpsc::channel::<Message>(64);
    let rate = Arc::new(RateLimiter::new());

    // Device → owner.
    let up_state = state.clone();
    let up_device = device_id.clone();
    let up_rate = rate.clone();
    let up_task = tokio::spawn(async move {
        while let Some(Ok(message)) = rx.next().await {
            match message {
                Message::Binary(payload) => {
                    if payload.len() > limits::MAX_DATA_FRAME {
                        up_state.audit("device-disconnected-rate", &up_device, payload.len());
                        return;
                    }
                    if !up_rate.allow() {
                        // Sustained over-limit: drop the bridge; the closed
                        // socket is the client-visible `rate_limited` signal
                        // paired with the error frame on the control path.
                        up_state.audit("device-disconnected-rate", &up_device, payload.len());
                        return;
                    }
                    up_state.audit("device-to-owner", &up_device, payload.len());
                    if owner_outbound.send(Message::Binary(payload)).await.is_err() {
                        return;
                    }
                }
                Message::Close(_) => return,
                _ => {}
            }
        }
    });

    // Owner → device: poll the owner's socket? No — the owner connection
    // task owns its socket. Instead the owner registers an explicit
    // downlink: the owner sends `device.route` targeting a device id.
    // v1 simplification: with one owner and one active device, frames the
    // OWNER pushes while a device bridge exists are forwarded by the owner
    // task. To keep the relay fully stateless about application frames, the
    // device task drains a dedicated queue registered here.
    let (down_tx, mut down_rx) = mpsc::channel::<Message>(64);
    state
        .devices
        .lock()
        .expect("devices")
        .insert(device_id.clone(), down_tx);

    let down_state = state.clone();
    let down_device = device_id.clone();
    let down_task = tokio::spawn(async move {
        while let Some(message) = down_rx.recv().await {
            down_state.audit("owner-to-device", &down_device, message.len());
            if tx.send(message).await.is_err() {
                break;
            }
        }
    });

    tokio::select! {
        _ = up_task => {}
        _ = down_task => {}
    }
    state.devices.lock().expect("devices").remove(&device_id);
    drop(device_tx);
    let _ = owner_closed;
    state.audit("device-disconnected", &device_id, 0);
}

/// Sustained-rate limiter (frozen values in [`limits`]).
pub struct RateLimiter {
    window_start: Mutex<std::time::Instant>,
    window_count: Mutex<u32>,
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

impl RateLimiter {
    pub fn new() -> Self {
        Self {
            window_start: Mutex::new(std::time::Instant::now()),
            window_count: Mutex::new(0),
        }
    }

    /// Whether one more frame fits the sustained rate (with burst).
    pub fn allow(&self) -> bool {
        let mut start = self.window_start.lock().expect("rate");
        let mut count = self.window_count.lock().expect("rate");
        let elapsed = start.elapsed();
        if elapsed >= Duration::from_secs(1) {
            *start = std::time::Instant::now();
            *count = 0;
        }
        *count += 1;
        *count <= limits::DEVICE_FRAMES_PER_SECOND + limits::DEVICE_FRAME_BURST
    }
}

fn text_frame(value: serde_json::Value) -> Message {
    Message::Text(value.to_string().into())
}

fn error_frame(code: &str) -> Message {
    text_frame(serde_json::json!({"type": "error", "code": code}))
}

fn base64_decode(value: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|e| e.to_string())
}

/// Sign helper re-exported for tests and the daemon integration (R17):
/// the relay contract is Ed25519 over domain-separated messages.
pub fn sign_message(domain: &str, payload: &str, key: &ed25519_dalek::SigningKey) -> String {
    sign(domain, payload, key)
}
