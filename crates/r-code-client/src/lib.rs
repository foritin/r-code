//! # r-code-client
//!
//! Local application RPC client for the shared r-code-service daemon.
//! Command calls carry durable `(client_id, command_id)` identity so
//! reconnects replay original results instead of duplicating effects
//! (persistence of receipts is the daemon side, T06b). Event connections
//! replay by cursor.

pub mod outbox;

pub use outbox::{Outbox, OutboxEntry};

use r_code_harness_protocol::application::{
    ApplicationCommand, ApplicationFrame, ApplicationResult, DaemonHandshake, ReadEventsRequest,
};
use r_code_harness_protocol::{EventEnvelope, IpcEndpoint};
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Client failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClientError {
    #[error("io failure: {0}")]
    Io(String),
    #[error("handshake rejected: {0}")]
    Handshake(String),
    #[error("daemon not reachable: {0}")]
    Unreachable(String),
    #[error("command failed: {0}")]
    Command(String),
    #[error("protocol fault: {0}")]
    Protocol(String),
}

/// One authenticated connection to the daemon.
pub struct DaemonClient {
    writer: tokio::io::WriteHalf<Stream>,
    reader: BufReader<tokio::io::ReadHalf<Stream>>,
    client_id: String,
    pub daemon_nonce: String,
}

// The runtime crate owns the platform stream; to keep this crate free of a
// runtime dependency we define a minimal stream pair here over the same
// transports.
/// Combined read+write object trait for the platform stream.
pub trait AsyncReadWrite: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}

impl<T> AsyncReadWrite for T where T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}

pub struct Stream {
    pub inner: Box<dyn AsyncReadWrite>,
}

impl tokio::io::AsyncRead for Stream {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut *self.inner).poll_read(cx, buf)
    }
}

impl tokio::io::AsyncWrite for Stream {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut *self.inner).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut *self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut *self.inner).poll_shutdown(cx)
    }
}
impl DaemonClient {
    /// Connect and perform the token handshake.
    pub async fn connect(
        endpoint: &IpcEndpoint,
        profile_id: &str,
        token: &str,
        client_id: &str,
    ) -> Result<Self, ClientError> {
        let stream = connect_endpoint(endpoint)
            .await
            .map_err(|e| ClientError::Unreachable(e.to_string()))?;
        let stream = Stream { inner: stream };
        let (reader, mut writer) = tokio::io::split(stream);
        let handshake = DaemonHandshake {
            protocol: "r-code-service/1".into(),
            profile_id: profile_id.to_string(),
            token: token.to_string(),
            client_id: Some(client_id.to_string()),
        };
        write_line(&mut writer, &ApplicationFrame::Handshake(handshake)).await?;
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        read_line(&mut reader, &mut line).await?;
        match serde_json::from_str::<ApplicationFrame>(line.trim()) {
            Ok(ApplicationFrame::Welcome(welcome)) => Ok(Self {
                writer,
                reader,
                client_id: client_id.to_string(),
                daemon_nonce: welcome.daemon_nonce,
            }),
            Ok(ApplicationFrame::Error { message }) => Err(ClientError::Handshake(message)),
            _ => Err(ClientError::Protocol("expected welcome".into())),
        }
    }

    /// Execute one command and await its result.
    pub async fn call(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ClientError> {
        let command_id = format!("cmd-{}", uuid::Uuid::new_v4().simple());
        self.call_with_id(method, params, &command_id).await
    }

    /// Execute with an explicit command id (retry-safe: the daemon replays
    /// the original result for a repeated id).
    pub async fn call_with_id(
        &mut self,
        method: &str,
        params: serde_json::Value,
        command_id: &str,
    ) -> Result<serde_json::Value, ClientError> {
        let command = ApplicationCommand {
            client_id: self.client_id.clone(),
            command_id: command_id.to_string(),
            method: method.to_string(),
            params,
        };
        write_line(&mut self.writer, &ApplicationFrame::Command(command)).await?;
        let mut line = String::new();
        read_line(&mut self.reader, &mut line).await?;
        match serde_json::from_str::<ApplicationFrame>(line.trim()) {
            Ok(ApplicationFrame::Result(ApplicationResult { outcome, .. })) => {
                outcome.map_err(ClientError::Command)
            }
            Ok(ApplicationFrame::Error { message }) => Err(ClientError::Protocol(message)),
            _ => Err(ClientError::Protocol("expected result".into())),
        }
    }

    /// Read events after a cursor over this (event) connection.
    pub async fn events_after(
        &mut self,
        after_seq: u64,
    ) -> Result<Vec<EventEnvelope>, ClientError> {
        let request = ReadEventsRequest {
            after_seq,
            limit: 500,
        };
        write_line(&mut self.writer, &ApplicationFrame::ReadEvents(request)).await?;
        let mut line = String::new();
        read_line(&mut self.reader, &mut line).await?;
        match serde_json::from_str::<ApplicationFrame>(line.trim()) {
            Ok(ApplicationFrame::Events(events)) => Ok(events),
            Ok(ApplicationFrame::Error { message }) => Err(ClientError::Protocol(message)),
            _ => Err(ClientError::Protocol("expected events".into())),
        }
    }
}

async fn write_line<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &ApplicationFrame,
) -> Result<(), ClientError> {
    let mut payload = serde_json::to_vec(frame).map_err(|e| ClientError::Io(e.to_string()))?;
    payload.push(b'\n');
    writer
        .write_all(&payload)
        .await
        .map_err(|e| ClientError::Io(e.to_string()))?;
    writer
        .flush()
        .await
        .map_err(|e| ClientError::Io(e.to_string()))
}

async fn read_line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    line: &mut String,
) -> Result<(), ClientError> {
    line.clear();
    let timeout = Duration::from_secs(30);
    let n = tokio::time::timeout(timeout, reader.read_line(line))
        .await
        .map_err(|_| ClientError::Io("read timeout".into()))?
        .map_err(|e| ClientError::Io(e.to_string()))?;
    if n == 0 {
        return Err(ClientError::Io("connection closed".into()));
    }
    Ok(())
}

async fn connect_endpoint(endpoint: &IpcEndpoint) -> std::io::Result<Box<dyn AsyncReadWrite>> {
    match endpoint {
        #[cfg(windows)]
        IpcEndpoint::NamedPipe { name } => {
            let client = tokio::net::windows::named_pipe::ClientOptions::new().open(name)?;
            Ok(Box::new(client))
        }
        #[cfg(unix)]
        IpcEndpoint::UnixSocket { path } => {
            let stream = tokio::net::UnixStream::connect(path).await?;
            Ok(Box::new(stream))
        }
        #[cfg(not(unix))]
        IpcEndpoint::UnixSocket { .. } => Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "unix sockets unavailable",
        )),
        #[cfg(not(windows))]
        IpcEndpoint::NamedPipe { .. } => Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "named pipes unavailable",
        )),
    }
}

/// Owner metadata needed to reach a daemon.
#[derive(Debug, Clone)]
pub struct DaemonInfo {
    pub token: String,
    pub nonce: String,
}

/// Read the daemon's owner file for token discovery.
pub fn read_owner_token(harness_v2_root: &std::path::Path) -> Option<DaemonInfo> {
    #[derive(serde::Deserialize)]
    struct OwnerIdentity {
        token: String,
        nonce: String,
    }
    let text = std::fs::read_to_string(harness_v2_root.join("owner.json")).ok()?;
    let identity: OwnerIdentity = serde_json::from_str(&text).ok()?;
    Some(DaemonInfo {
        token: identity.token,
        nonce: identity.nonce,
    })
}

/// Ensure a daemon owns the profile: try connecting; on failure spawn the
/// service binary (races converge — losers exit with an ownership error).
/// The spawned daemon is fully detached (null stdio) so it never holds the
/// caller's pipes, and receives the endpoint's ipc-name suffix so custom
/// endpoints bind identically on both sides.
pub async fn ensure_daemon(
    harness_v2_root: &std::path::Path,
    endpoint: &IpcEndpoint,
    profile_id: &str,
    service_binary: Option<&std::path::Path>,
) -> Result<DaemonInfo, ClientError> {
    let ipc_name = endpoint_suffix(endpoint);
    let mut spawn_attempts = 0u8;
    for attempt in 0..40u32 {
        if let Some(info) = read_owner_token(harness_v2_root) {
            if let Ok(client) =
                DaemonClient::connect(endpoint, profile_id, &info.token, "probe").await
            {
                drop(client);
                return Ok(info);
            }
        }
        // Spawn a candidate owner at most twice; if we lose the race it
        // exits by itself on the ownership lock.
        if let Some(binary) = service_binary {
            if attempt % 8 == 0 && spawn_attempts < 2 {
                spawn_attempts += 1;
                let mut command = std::process::Command::new(binary);
                command
                    .args(["--profile", profile_id.trim_start_matches("harness-v2/")])
                    .arg("--data-root")
                    .arg(
                        harness_v2_root
                            .parent()
                            .map(PathBuf::from)
                            .unwrap_or_default(),
                    )
                    .arg("--ipc-name")
                    .arg(&ipc_name)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt;
                    // CREATE_NO_WINDOW: no console, no inherited handles.
                    command.creation_flags(0x0800_0000);
                }
                let _ = command.spawn();
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err(ClientError::Unreachable("daemon did not come up".into()))
}

/// The ipc-name suffix a daemon must pass to bind `endpoint`. Suffixes may
/// contain hyphens, so strip the well-known prefix instead of splitting.
fn endpoint_suffix(endpoint: &IpcEndpoint) -> String {
    const PREFIX: &str = "r-code-harness-v2-";
    match endpoint {
        IpcEndpoint::NamedPipe { name } => name
            .rsplit('\\')
            .next()
            .unwrap_or(name)
            .trim_start_matches(PREFIX)
            .to_string(),
        IpcEndpoint::UnixSocket { path } => path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| PREFIX.to_string())
            .trim_end_matches(".sock")
            .trim_start_matches(PREFIX)
            .to_string(),
    }
}
