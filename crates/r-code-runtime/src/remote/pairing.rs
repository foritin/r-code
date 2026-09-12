//! One-shot pairing sessions (R02, F4): the host console starts a session
//! (local transport only), gets a high-entropy code valid for 120s; a
//! device exchanges that code exactly once for a durable device token.
//! Sessions live in memory only — a daemon restart has no pending pairing.

use crate::application::CommandSource;
use crate::remote::capabilities::CapabilitySet;
use crate::remote::registry::{DeviceRegistry, PairedDevice};
use sha2::{Digest, Sha256};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use thiserror::Error;

/// The frozen QR pairing payload v1 (architecture §3):
/// `rcode://pair?v=1&h=<host>&p=<port>&s=<pair_secret>&fp=<sha256-hex>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QrPairPayload {
    pub version: u8,
    pub host: String,
    pub port: u16,
    pub pair_secret: String,
    pub fingerprint: String,
}

/// Build the QR payload for an active pairing session.
pub fn qr_payload_v1(host: &str, port: u16, pair_secret: &str, fingerprint: &str) -> String {
    format!("rcode://pair?v=1&h={host}&p={port}&s={pair_secret}&fp={fingerprint}")
}

/// Parse a scanned/entered QR payload. Strict: wrong scheme, wrong
/// version, missing fields or a non-hex fingerprint are refused (fail
/// closed — a malformed payload never reaches the pairing path).
pub fn parse_qr_payload(text: &str) -> Result<QrPairPayload, PairingError> {
    let rest = text
        .strip_prefix("rcode://pair?")
        .ok_or(PairingError::Rejected)?;
    let mut version: Option<u8> = None;
    let mut host: Option<String> = None;
    let mut port: Option<u16> = None;
    let mut pair_secret: Option<String> = None;
    let mut fingerprint: Option<String> = None;
    for pair in rest.split('&') {
        let (key, value) = pair.split_once('=').ok_or(PairingError::Rejected)?;
        match key {
            "v" => {
                version = Some(value.parse().map_err(|_| PairingError::Rejected)?);
            }
            "h" => host = Some(value.to_string()),
            "p" => {
                port = Some(value.parse().map_err(|_| PairingError::Rejected)?);
            }
            "s" => pair_secret = Some(value.to_string()),
            "fp" => fingerprint = Some(value.to_string()),
            _ => return Err(PairingError::Rejected),
        }
    }
    Ok(QrPairPayload {
        version: version.filter(|v| *v == 1).ok_or(PairingError::Rejected)?,
        host: host
            .filter(|h| !h.is_empty())
            .ok_or(PairingError::Rejected)?,
        port: port.filter(|p| *p > 0).ok_or(PairingError::Rejected)?,
        pair_secret: pair_secret
            .filter(|s| !s.is_empty())
            .ok_or(PairingError::Rejected)?,
        fingerprint: fingerprint
            .filter(|fp| fp.len() == 64 && fp.chars().all(|c| c.is_ascii_hexdigit()))
            .ok_or(PairingError::Rejected)?,
    })
}

/// How long a pairing code stays valid.
pub const PAIRING_TTL: Duration = Duration::from_secs(120);

/// Structured pairing errors (wire-stable codes for clients).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PairingError {
    #[error("pairing_expired: the pairing code has expired; start a new pairing on the host")]
    Expired,
    #[error("pairing_consumed: the pairing code was already used; codes are one-shot")]
    Consumed,
    #[error("pairing_rejected: unknown pairing code")]
    Rejected,
    #[error("pairing_unavailable: no pairing session is active on this host")]
    Unavailable,
    #[error("remote_not_enabled: pairing can only be started from the local console")]
    RemoteSource,
    #[error("registry failure: {0}")]
    Registry(String),
}

/// `remote.pairingStart` reply.
#[derive(Debug, Clone, PartialEq)]
pub struct PairingStartReply {
    /// One-time high-entropy pairing code (≥128 bits).
    pub pairing_code: String,
    /// QR payload placeholder (full format frozen in R09).
    pub qr_payload: String,
    /// Candidate LAN endpoints (probe injected by the daemon wiring; the
    /// session layer never touches the network stack itself).
    pub lan_endpoints: Vec<String>,
    pub expires_at_ms: i64,
}

/// `device.pair` request.
#[derive(Debug, Clone, PartialEq)]
pub struct DevicePairRequest {
    pub pair_secret: String,
    pub device_name: String,
    pub platform: String,
}

/// `device.pair` reply — the device token appears exactly once here.
#[derive(Debug, Clone, PartialEq)]
pub struct DevicePairReply {
    pub device_id: String,
    pub token: String,
    pub capabilities: Vec<String>,
    pub server_fingerprint: String,
}

struct PairingSession {
    code_hash: String,
    fingerprint: String,
    expires_at: Instant,
    consumed: bool,
}

/// Single active pairing session per daemon (a new start supersedes the
/// previous one), memory-only.
pub struct PairingSessions {
    session: Mutex<Option<PairingSession>>,
    ttl: Duration,
}

pub(crate) fn token_sha256_hex(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let mut hex = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

fn random_code() -> String {
    // 2× UUIDv4 = 2×122 random bits ≥ 128 bits of entropy.
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl PairingSessions {
    pub fn new(ttl: Duration) -> Self {
        Self {
            session: Mutex::new(None),
            ttl,
        }
    }

    /// Start a pairing session from the host console. Local source only
    /// (F4); a new start supersedes any previous session.
    pub fn start(
        &self,
        source: CommandSource,
        fingerprint: &str,
        lan_endpoints: Vec<String>,
    ) -> Result<PairingStartReply, PairingError> {
        if source != CommandSource::Local {
            return Err(PairingError::RemoteSource);
        }
        let code = random_code();
        let reply = PairingStartReply {
            qr_payload: format!("rcode://pair?v=1&p={code}&fp={fingerprint}"),
            expires_at_ms: now_ms() + self.ttl.as_millis() as i64,
            pairing_code: code.clone(),
            lan_endpoints,
        };
        *self.session.lock().expect("pairing") = Some(PairingSession {
            code_hash: token_sha256_hex(&code),
            fingerprint: fingerprint.to_string(),
            expires_at: Instant::now() + self.ttl,
            consumed: false,
        });
        Ok(reply)
    }

    /// Exchange a one-time pairing code for a durable device token.
    /// Wrong code / expired / replayed code / no session — each a distinct
    /// structured error, and none of them ever creates a device.
    pub fn pair(
        &self,
        registry: &DeviceRegistry,
        request: DevicePairRequest,
    ) -> Result<DevicePairReply, PairingError> {
        let fingerprint = {
            let mut session = self.session.lock().expect("pairing");
            let session = session.as_mut().ok_or(PairingError::Unavailable)?;
            if session.consumed {
                return Err(PairingError::Consumed);
            }
            if Instant::now() >= session.expires_at {
                // Expired sessions are lazily invalidated.
                return Err(PairingError::Expired);
            }
            if session.code_hash != token_sha256_hex(&request.pair_secret) {
                return Err(PairingError::Rejected);
            }
            session.consumed = true;
            session.fingerprint.clone()
        };
        let PairedDevice { record, token } = registry
            .register(
                &request.device_name,
                &request.platform,
                &fingerprint,
                CapabilitySet::read_only(),
            )
            .map_err(|e| PairingError::Registry(e.to_string()))?;
        Ok(DevicePairReply {
            device_id: record.id.clone(),
            token,
            capabilities: record
                .capabilities
                .labels()
                .into_iter()
                .map(str::to_string)
                .collect(),
            server_fingerprint: fingerprint,
        })
    }

    /// Whether a session is currently live (diagnostics).
    pub fn is_active(&self) -> bool {
        self.session
            .lock()
            .expect("pairing")
            .as_ref()
            .is_some_and(|session| !session.consumed && Instant::now() < session.expires_at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry(dir: &tempfile::TempDir) -> DeviceRegistry {
        DeviceRegistry::open(dir.path()).expect("registry")
    }

    fn started(ttl: Duration) -> (PairingSessions, PairingStartReply, DeviceRegistry) {
        let dir = tempfile::tempdir().expect("tempdir");
        let sessions = PairingSessions::new(ttl);
        let reply = sessions
            .start(
                CommandSource::Local,
                "fp-aa:bb",
                vec!["192.168.1.10:0".into()],
            )
            .expect("start");
        (sessions, reply, registry(&dir))
    }

    #[test]
    fn r02_a2_successful_pairing_yields_a_device_and_one_time_token() {
        let (sessions, reply, registry) = started(PAIRING_TTL);
        assert!(reply.pairing_code.len() >= 32, "≥128-bit code");
        assert!(reply.qr_payload.starts_with("rcode://pair?v=1&p="));
        assert_eq!(reply.lan_endpoints, vec!["192.168.1.10:0".to_string()]);

        let paired = sessions
            .pair(
                &registry,
                DevicePairRequest {
                    pair_secret: reply.pairing_code.clone(),
                    device_name: "iPhone 15".into(),
                    platform: "ios-pwa".into(),
                },
            )
            .expect("pair");
        assert!(!paired.token.is_empty());
        assert_eq!(paired.capabilities, vec!["events-read"]);
        assert_eq!(paired.server_fingerprint, "fp-aa:bb");
        // The device exists and the token authenticates.
        let record = registry
            .get_by_token(&paired.token)
            .expect("device registered");
        assert_eq!(record.id, paired.device_id);
    }

    #[test]
    fn r02_a1_expired_replayed_and_wrong_codes_are_distinct_errors() {
        // Expired.
        {
            let (sessions, reply, registry) = started(Duration::from_millis(50));
            std::thread::sleep(Duration::from_millis(80));
            let error = sessions
                .pair(
                    &registry,
                    DevicePairRequest {
                        pair_secret: reply.pairing_code,
                        device_name: "d".into(),
                        platform: "p".into(),
                    },
                )
                .unwrap_err();
            assert_eq!(error, PairingError::Expired);
            assert!(registry.list().is_empty(), "no device from an expired code");
        }
        // Replayed (second consumption attempt).
        {
            let (sessions, reply, registry) = started(PAIRING_TTL);
            let request = DevicePairRequest {
                pair_secret: reply.pairing_code,
                device_name: "d".into(),
                platform: "p".into(),
            };
            sessions.pair(&registry, request.clone()).expect("first");
            let error = sessions.pair(&registry, request).unwrap_err();
            assert_eq!(error, PairingError::Consumed);
            assert_eq!(registry.list().len(), 1, "replay creates no second device");
        }
        // Wrong code.
        {
            let (sessions, _reply, registry) = started(PAIRING_TTL);
            let error = sessions
                .pair(
                    &registry,
                    DevicePairRequest {
                        pair_secret: "not-the-code".into(),
                        device_name: "d".into(),
                        platform: "p".into(),
                    },
                )
                .unwrap_err();
            assert_eq!(error, PairingError::Rejected);
            assert!(registry.list().is_empty(), "no device from a wrong code");
        }
        // No session at all.
        {
            let dir = tempfile::tempdir().expect("tempdir");
            let sessions = PairingSessions::new(PAIRING_TTL);
            let error = sessions
                .pair(
                    &registry(&dir),
                    DevicePairRequest {
                        pair_secret: "x".into(),
                        device_name: "d".into(),
                        platform: "p".into(),
                    },
                )
                .unwrap_err();
            assert_eq!(error, PairingError::Unavailable);
        }
    }

    #[test]
    fn r02_a3_remote_source_cannot_start_pairing() {
        let sessions = PairingSessions::new(PAIRING_TTL);
        let error = sessions
            .start(CommandSource::Remote, "fp", vec![])
            .unwrap_err();
        assert_eq!(error, PairingError::RemoteSource);
        assert!(!sessions.is_active(), "remote start leaves no session");
    }

    #[test]
    fn r09_a1_qr_payload_roundtrip_and_malformed_refusal() {
        let payload = qr_payload_v1("192.168.1.10", 8443, "the-secret", &"ab".repeat(32));
        let parsed = parse_qr_payload(&payload).expect("roundtrip");
        assert_eq!(
            parsed,
            QrPairPayload {
                version: 1,
                host: "192.168.1.10".into(),
                port: 8443,
                pair_secret: "the-secret".into(),
                fingerprint: "ab".repeat(32),
            }
        );
        for bad in [
            "https://example.com/pair?v=1",
            "rcode://pair?v=2&h=x&p=1&s=y&fp=00000000000000000000000000000000",
            "rcode://pair?v=1&p=1&s=y&fp=0000000000000000000000000000000000000000000000000000000000000000",
            "rcode://pair?v=1&h=x&p=0&s=y&fp=0000000000000000000000000000000000000000000000000000000000000000",
            "rcode://pair?v=1&h=x&p=1&s=y&fp=nothex",
            "rcode://pair?v=1&h=x&p=1&s=y&fp=abc&extra=1",
        ] {
            assert!(parse_qr_payload(bad).is_err(), "must refuse: {bad}");
        }
    }

    #[test]
    fn r09_a1_qr_and_manual_codes_pair_identically() {
        // QR 与手动码是同一 secret 的两种载体：同一 pair 路径、同一
        // DeviceRecord 字段（等价性，任务卡 ⑤）。
        let (sessions, reply, _registry) = started(PAIRING_TTL);
        let fingerprint = "ab".repeat(32);
        let qr = qr_payload_v1("192.168.1.10", 8443, &reply.pairing_code, &fingerprint);
        let parsed = parse_qr_payload(&qr).expect("parse");
        // The QR secret IS the manual pairing code (one session, one secret).
        assert_eq!(parsed.pair_secret, reply.pairing_code);
        // Pairing through the parsed secret yields the standard record.
        let dir = tempfile::tempdir().expect("tempdir");
        let registry_b = registry(&dir);
        let secret = parsed.pair_secret.clone();
        let via_qr = sessions
            .pair(
                &registry_b,
                DevicePairRequest {
                    pair_secret: secret.clone(),
                    device_name: "iPhone".into(),
                    platform: "ios-pwa".into(),
                },
            )
            .expect("pair via qr secret");
        assert_eq!(via_qr.capabilities, vec!["events-read".to_string()]);
        // The manual path with the same (hypothetical second) code is the
        // same shape; replaying the consumed QR secret refuses either way.
        assert_eq!(
            sessions
                .pair(
                    &registry_b,
                    DevicePairRequest {
                        pair_secret: secret,
                        device_name: "iPhone".into(),
                        platform: "ios-pwa".into(),
                    },
                )
                .unwrap_err(),
            PairingError::Consumed
        );
    }

    #[test]
    fn a_new_start_supersedes_the_previous_session() {
        let (sessions, first, _registry) = started(PAIRING_TTL);
        assert!(sessions.is_active());
        let second = sessions
            .start(CommandSource::Local, "fp-2", vec![])
            .expect("restart");
        assert_ne!(first.pairing_code, second.pairing_code);
        // The old code no longer pairs; the new one does.
        let dir = tempfile::tempdir().expect("tempdir");
        let registry = registry(&dir);
        let error = sessions
            .pair(
                &registry,
                DevicePairRequest {
                    pair_secret: first.pairing_code,
                    device_name: "d".into(),
                    platform: "p".into(),
                },
            )
            .unwrap_err();
        assert_eq!(error, PairingError::Rejected);
        sessions
            .pair(
                &registry,
                DevicePairRequest {
                    pair_secret: second.pairing_code,
                    device_name: "d".into(),
                    platform: "p".into(),
                },
            )
            .expect("new code pairs");
    }
}
