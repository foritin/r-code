//! Remote-control surface (F1: the daemon's transport extension).
//!
//! Everything remote lives behind an explicit pairing: no devices → no
//! listener (F2); capabilities are enforced daemon-side per client (F6);
//! the sensitive surface is unconditionally unreachable remotely (F5).

pub mod capabilities;
pub mod fanout;
pub mod listener;
pub mod manager;
pub mod pairing;
pub mod registry;
pub mod relay_config;
pub mod tls;

pub use capabilities::{Capability, CapabilitySet, FORBIDDEN_REMOTE_METHODS};
pub use fanout::{CursorPublisher, FanoutHub};
pub use listener::{listen, listen_pairing, validate_bind_address, ListenerError, ListenerHandle};
pub use manager::{ManagerError, RemoteManager};
pub use pairing::{
    parse_qr_payload, qr_payload_v1, DevicePairReply, DevicePairRequest, PairingError,
    PairingSessions, PairingStartReply, QrPairPayload, PAIRING_TTL,
};
pub use registry::{DeviceRecord, DeviceRegistry, ListenerConfig};
pub use tls::{ensure_identity, fingerprint_of, pinned_client_config, server_config, Identity};
