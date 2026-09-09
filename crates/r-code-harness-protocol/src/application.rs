//! Application-level RPC between frontends (GUI/TUI/MCP) and the shared
//! r-code-service daemon. Independent of the plugin wire protocol: commands
//! carry `(profile_id, client_id, command_id)` identity for durable
//! deduplication, and events replay by cursor.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Local IPC endpoint for a profile's daemon.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum IpcEndpoint {
    /// Current-user-only Windows named pipe.
    NamedPipe { name: String },
    /// Unix domain socket with 0600 permissions.
    UnixSocket { path: PathBuf },
}

/// Handshake sent by every connection before any command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonHandshake {
    pub protocol: String,
    pub profile_id: String,
    /// Profile-private random token from the owner file.
    pub token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonWelcome {
    pub daemon_nonce: String,
    pub profile_id: String,
}

/// One application command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationCommand {
    pub client_id: String,
    pub command_id: String,
    pub method: String,
    pub params: serde_json::Value,
}

/// The daemon's answer to one command. Durable: reconnecting clients that
/// repeat a command_id receive the original result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplicationResult {
    pub client_id: String,
    pub command_id: String,
    pub outcome: Result<serde_json::Value, String>,
}

/// Read ordered events after a cursor (event connections).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadEventsRequest {
    pub after_seq: u64,
    pub limit: u32,
}

/// One line-framed message on the application connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ApplicationFrame {
    Handshake(DaemonHandshake),
    Welcome(DaemonWelcome),
    Command(ApplicationCommand),
    Result(ApplicationResult),
    ReadEvents(ReadEventsRequest),
    Events(Vec<crate::events::EventEnvelope>),
    Error { message: String },
}

/// Well-known application methods.
pub mod methods {
    pub const PING: &str = "ping";
    pub const SHUTDOWN: &str = "service.shutdown";
    pub const ECHO: &str = "echo";
}
