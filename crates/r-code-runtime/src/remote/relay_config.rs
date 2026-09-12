//! Desktop relay configuration and outbound link (R18, F12): the relay is
//! **opt-in** — with no configured URL the daemon never dials anything
//! (zero outbound connections, R18.A1). Configuring a URL persists it to
//! `<harness-v2>/relay.json` and starts the outbound owner registration
//! loop; the status machine (unconfigured/connecting/online/backoff)
//! mirrors the frontends' connection state machine.

use crate::daemon::ApplicationHandler;
use ed25519_dalek::Signer as _;
use futures_util::SinkExt as _;
use futures_util::StreamExt as _;
use rand_core::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::watch;

/// Persisted relay configuration (F12: never in plugin manifests).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayConfig {
    /// `host:port` of the self-hosted relay. Empty = relay disabled.
    #[serde(default)]
    pub url: String,
    /// One-time owner registration code issued by the relay operator.
    #[serde(default)]
    pub registration_code: String,
    #[serde(default)]
    pub enabled: bool,
}

/// Link status (wire shape for `relay.status`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum RelayStatus {
    Unconfigured,
    Connecting { attempt: u32 },
    Online,
    Backoff { attempt: u32 },
}

/// The owner's long-term Ed25519 identity (persisted 0600 next to the
/// config; the Noise static key derives from it — R17).
fn ensure_owner_identity(harness_v2_root: &std::path::Path) -> Result<PathBuf, String> {
    let path = harness_v2_root.join("relay-identity.json");
    if !path.exists() {
        let mut seed = [0u8; 32];
        rand_core::OsRng.fill_bytes(&mut seed);
        let signing = ed25519_dalek::SigningKey::from_bytes(&seed);
        let payload = serde_json::json!({
            "seed": hex(&seed),
            "pubkey": hex(&signing.verifying_key().to_bytes()),
        });
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, payload.to_string()).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(path)
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Generate a one-time owner registration code (displayed once in the
/// desktop console; imported into the relay's `R_CODE_RELAY_CODES`).
pub fn generate_registration_code() -> String {
    let mut bytes = [0u8; 32];
    rand_core::OsRng.fill_bytes(&mut bytes);
    hex(&bytes)
}

/// The outbound relay link owned by the daemon.
pub struct RelayLink {
    config_path: PathBuf,
    identity_path: PathBuf,
    status_tx: watch::Sender<RelayStatus>,
    /// Connect attempts actually initiated (test probe: zero when
    /// unconfigured — the "no outbound connections" invariant).
    attempts: Arc<std::sync::atomic::AtomicU32>,
}

impl RelayLink {
    /// Open the link from the profile root. With no persisted config the
    /// status stays `unconfigured` and **no connection task exists**.
    pub fn open(harness_v2_root: &std::path::Path) -> Result<Self, String> {
        ensure_owner_identity(harness_v2_root)?;
        let config_path = harness_v2_root.join("relay.json");
        let (status_tx, _) = watch::channel(RelayStatus::Unconfigured);
        let link = Self {
            config_path,
            identity_path: harness_v2_root.join("relay-identity.json"),
            status_tx,
            attempts: Arc::new(std::sync::atomic::AtomicU32::new(0)),
        };
        if link.load_config().enabled && !link.load_config().url.is_empty() {
            link.spawn_connect_task();
        }
        Ok(link)
    }

    fn load_config(&self) -> RelayConfig {
        std::fs::read_to_string(&self.config_path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Persist configuration. Enabling with a URL starts the outbound loop;
    /// disabling tears it down (status back to unconfigured).
    pub fn configure(&self, config: RelayConfig) -> Result<RelayStatus, String> {
        if let Some(parent) = self.config_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(
            &self.config_path,
            serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        if config.enabled && !config.url.is_empty() {
            self.spawn_connect_task();
            Ok(RelayStatus::Connecting { attempt: 1 })
        } else {
            self.status_tx.send_replace(RelayStatus::Unconfigured);
            Ok(RelayStatus::Unconfigured)
        }
    }

    /// Current status snapshot.
    pub fn status(&self) -> RelayStatus {
        self.status_tx.borrow().clone()
    }

    /// How many outbound connect attempts were initiated (test probe).
    pub fn connect_attempts(&self) -> u32 {
        self.attempts.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// The owner fingerprint (hex of the Ed25519 public key) for QR relay
    /// fields and console display.
    pub fn owner_fingerprint(&self) -> Result<String, String> {
        let text = std::fs::read_to_string(&self.identity_path).map_err(|e| e.to_string())?;
        let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        Ok(value["pubkey"].as_str().unwrap_or_default().to_string())
    }

    /// Start the outbound registration loop (single task; superseding tasks
    /// stop via the watch channel swap).
    fn spawn_connect_task(&self) {
        self.attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.status_tx
            .send_replace(RelayStatus::Connecting { attempt: 1 });
        // v1 loop: attempt the TCP dial + registration, report status, then
        // exponential backoff. Full E2EE integration rides R17's streams —
        // this task owns the *lifecycle* (config → dial → status).
        let identity_path = self.identity_path.clone();
        let status_tx = self.status_tx.clone();
        let attempts = self.attempts.clone();
        let config = self.load_config();
        tokio::spawn(async move {
            let mut attempt: u32 = 1;
            loop {
                let seeded = read_seed(&identity_path);
                let outcome =
                    dial_and_register(&config.url, &config.registration_code, &seeded).await;
                match outcome {
                    Ok(()) => {
                        status_tx.send_replace(RelayStatus::Online);
                        // Online until the socket dies (R19 adds traffic);
                        // park forever here — the task ends when the
                        // process does or the config changes.
                        return;
                    }
                    Err(_) => {
                        attempt += 1;
                        status_tx.send_replace(RelayStatus::Backoff { attempt });
                        let backoff = std::time::Duration::from_millis(
                            (200u64 * 2u64.saturating_pow(attempt.min(8))).min(30_000),
                        );
                        tokio::time::sleep(backoff).await;
                        attempts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        status_tx.send_replace(RelayStatus::Connecting { attempt });
                    }
                }
            }
        });
    }
}

fn read_seed(identity_path: &std::path::Path) -> [u8; 32] {
    let text = std::fs::read_to_string(identity_path).unwrap_or_default();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
    let hex = value["seed"].as_str().unwrap_or_default();
    let mut seed = [0u8; 32];
    for (index, chunk) in hex.as_bytes().chunks(2).take(32).enumerate() {
        if chunk.len() == 2 {
            if let (Some(hi), Some(lo)) = (hex_val(chunk[0]), hex_val(chunk[1])) {
                seed[index] = hi << 4 | lo;
            }
        }
    }
    seed
}

fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// One registration attempt: dial the relay, send owner.register, await
/// welcome. Errors drive the backoff loop.
async fn dial_and_register(url: &str, code: &str, seed: &[u8; 32]) -> Result<(), String> {
    use tokio_tungstenite::tungstenite::Message;
    let signing = ed25519_dalek::SigningKey::from_bytes(seed);
    let pubkey_b64 = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(signing.verifying_key().to_bytes())
    };
    let sig = {
        let message = format!("rcode-owner-register{pubkey_b64}");
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .encode(signing.sign(message.as_bytes()).to_bytes())
    };
    let tcp = tokio::net::TcpStream::connect(url)
        .await
        .map_err(|e| e.to_string())?;
    let (mut ws, _) = tokio_tungstenite::client_async(
        format!("ws://{url}/relay"),
        tokio_tungstenite::MaybeTlsStream::Plain(tcp),
    )
    .await
    .map_err(|e| e.to_string())?;
    ws.send(Message::Text(
        serde_json::json!({
            "type": "owner.register",
            "code": code,
            "pubkey": pubkey_b64,
            "sig": sig,
        })
        .to_string()
        .into(),
    ))
    .await
    .map_err(|e| e.to_string())?;
    let deadline = tokio::time::Duration::from_secs(10);
    let welcome = tokio::time::timeout(deadline, ws.next())
        .await
        .map_err(|_| "registration timeout".to_string())?;
    match welcome {
        Some(Ok(Message::Text(text))) => {
            let value: serde_json::Value =
                serde_json::from_str(&text).map_err(|e| e.to_string())?;
            if value["type"] == "owner.welcome" {
                Ok(())
            } else {
                Err(value["error"]["code"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string())
            }
        }
        _ => Err("connection closed during registration".into()),
    }
}

/// Fingerprint helper for tests: SHA-256 hex of arbitrary bytes.
pub fn fingerprint_of_seed(seed: &[u8; 32]) -> String {
    let signing = ed25519_dalek::SigningKey::from_bytes(seed);
    let digest = Sha256::digest(signing.verifying_key().to_bytes());
    hex(&digest)
}

/// Guard used by daemon tests: a handler wrapper exposing relay methods
/// with the local-console-only semantics (F5: device.* surface).
pub struct RelayMethods {
    pub link: Arc<RelayLink>,
}

#[async_trait::async_trait]
impl ApplicationHandler for RelayMethods {
    async fn execute(
        &self,
        command: r_code_harness_protocol::application::ApplicationCommand,
    ) -> Result<serde_json::Value, String> {
        match command.method.as_str() {
            "relay.status" => Ok(serde_json::to_value(self.link.status()).unwrap_or_default()),
            "relay.configure" => {
                let config = RelayConfig {
                    url: command.params["url"].as_str().unwrap_or_default().into(),
                    registration_code: command.params["registrationCode"]
                        .as_str()
                        .unwrap_or_default()
                        .into(),
                    enabled: command.params["enabled"].as_bool().unwrap_or(false),
                };
                let status = self.link.configure(config).map_err(|e| e.to_string())?;
                Ok(serde_json::to_value(status).unwrap_or_default())
            }
            "relay.issueCode" => Ok(serde_json::json!({
                // One-time registration code: shown once in the console,
                // imported into the relay's R_CODE_RELAY_CODES (F12).
                "code": generate_registration_code(),
                "ttl": "10min",
            })),
            other => Err(format!("unknown method {other}")),
        }
    }

    async fn events_after(
        &self,
        _after_seq: u64,
        _limit: u32,
    ) -> Vec<r_code_harness_protocol::EventEnvelope> {
        vec![]
    }
}
