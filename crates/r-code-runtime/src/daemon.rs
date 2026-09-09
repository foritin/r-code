//! The single-owner r-code-service daemon core.
//!
//! Ownership is an OS-level exclusive handle on `<harness-v2>/owner.lock`
//! (zero-byte mutex file): the handle dies with the process, so a crashed
//! owner releases automatically and a successor takes over. Identity
//! metadata (pid, boot-ish nonce, random token, endpoint) lives beside it in
//! `owner.json` for clients. Every connection must complete the token
//! handshake before sending commands.

use crate::ipc::IpcListener;
use r_code_harness_protocol::application::{
    ApplicationCommand, ApplicationFrame, ApplicationResult, DaemonHandshake, DaemonWelcome,
};
use r_code_harness_protocol::{EventEnvelope, IpcEndpoint};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Daemon errors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DaemonError {
    #[error("another live daemon owns this profile (pid {pid})")]
    AlreadyOwned { pid: u32 },
    #[error("io failure: {0}")]
    Io(String),
}

/// Owner identity written to `owner.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnerIdentity {
    pub pid: u32,
    /// Random per-daemon nonce.
    pub nonce: String,
    /// Profile-private connection token.
    pub token: String,
    pub profile_id: String,
    pub started_unix_ms: i64,
}

/// The held OS ownership lock.
pub struct ProfileLock {
    _handle: Fileish,
    lock_path: PathBuf,
}

enum Fileish {
    #[cfg(unix)]
    Flocked(std::fs::File),
    #[cfg(windows)]
    NoShare(#[allow(dead_code)] std::fs::File),
}

impl ProfileLock {
    fn open_exclusive(path: &Path) -> io::Result<Fileish> {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(path)?;
            let fd = file.as_raw_fd();
            let result = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
            if result != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Fileish::Flocked(file))
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // share_mode(0): while we hold this handle, nobody else can open
            // the file — the mutex. create_new first so a fresh profile does
            // not inherit leftovers; on collision retry a plain open, which
            // only succeeds when the previous holder is gone.
            match std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .share_mode(0)
                .open(path)
            {
                Ok(file) => Ok(Fileish::NoShare(file)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let file = std::fs::OpenOptions::new()
                        .write(true)
                        .share_mode(0)
                        .open(path)?;
                    Ok(Fileish::NoShare(file))
                }
                Err(error) => Err(error),
            }
        }
    }

    /// Acquire profile ownership. Fails with [`DaemonError::AlreadyOwned`]
    /// while a live daemon holds the lock.
    pub fn acquire(harness_v2_root: &Path, profile_id: &str) -> Result<Self, DaemonError> {
        std::fs::create_dir_all(harness_v2_root)
            .map_err(|e| DaemonError::Io(format!("create root: {e}")))?;
        let lock_path = harness_v2_root.join("owner.lock");
        let handle = Self::open_exclusive(&lock_path).map_err(|error| {
            DaemonError::AlreadyOwned {
                pid: read_owner_pid(harness_v2_root).unwrap_or(0),
            }
            .tap_error(&error)
        })?;
        let lock = Self {
            _handle: handle,
            lock_path,
        };
        // Record identity for clients.
        let identity = OwnerIdentity {
            pid: std::process::id(),
            nonce: uuid::Uuid::new_v4().simple().to_string(),
            token: format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ),
            profile_id: profile_id.to_string(),
            started_unix_ms: now_ms(),
        };
        std::fs::write(
            harness_v2_root.join("owner.json"),
            serde_json::to_string_pretty(&identity).map_err(|e| DaemonError::Io(e.to_string()))?,
        )
        .map_err(|e| DaemonError::Io(e.to_string()))?;
        Ok(lock)
    }

    pub fn identity(&self, harness_v2_root: &Path) -> Result<OwnerIdentity, DaemonError> {
        read_owner(harness_v2_root).ok_or_else(|| DaemonError::Io("owner.json unreadable".into()))
    }

    pub fn lock_path(&self) -> &Path {
        &self.lock_path
    }
}

trait TapError {
    fn tap_error(self, cause: &io::Error) -> Self;
}

impl TapError for DaemonError {
    fn tap_error(self, cause: &io::Error) -> Self {
        let _ = cause;
        self
    }
}

fn read_owner_pid(harness_v2_root: &Path) -> Option<u32> {
    read_owner(harness_v2_root).map(|owner| owner.pid)
}

fn read_owner(harness_v2_root: &Path) -> Option<OwnerIdentity> {
    let text = std::fs::read_to_string(harness_v2_root.join("owner.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// Read the owner identity (clients use this to find the token/endpoint).
pub fn owner_identity_of(harness_v2_root: &Path) -> Option<OwnerIdentity> {
    read_owner(harness_v2_root)
}

/// The application surface the daemon serves.
#[async_trait::async_trait]
pub trait ApplicationHandler: Send + Sync {
    async fn execute(&self, command: ApplicationCommand) -> Result<serde_json::Value, String>;
    async fn events_after(&self, after_seq: u64, limit: u32) -> Vec<EventEnvelope>;
}

/// Serving daemon bound to one profile endpoint.
pub struct Daemon {
    pub listener: tokio::sync::Mutex<IpcListener>,
    pub identity: OwnerIdentity,
    pub endpoint: IpcEndpoint,
    pub handler: Arc<dyn ApplicationHandler>,
}

impl Daemon {
    /// Bind the endpoint and start serving. Binding enforces single
    /// ownership of the name (first pipe instance / exclusive socket).
    pub fn start(
        endpoint: &IpcEndpoint,
        identity: OwnerIdentity,
        handler: Arc<dyn ApplicationHandler>,
    ) -> Result<Self, DaemonError> {
        let listener =
            crate::ipc::bind_endpoint(endpoint).map_err(|e| DaemonError::Io(e.to_string()))?;
        Ok(Self {
            listener: tokio::sync::Mutex::new(listener),
            identity,
            endpoint: endpoint.clone(),
            handler,
        })
    }

    /// Accept loop; each connection runs the token handshake then serves
    /// commands or event reads until EOF.
    pub async fn serve(self) -> Result<(), DaemonError> {
        let daemon = Arc::new(self);
        loop {
            let stream = match daemon.listener.lock().await.accept().await {
                Ok(stream) => stream,
                Err(error) => return Err(DaemonError::Io(error.to_string())),
            };
            let daemon = daemon.clone();
            tokio::spawn(async move {
                if let Err(error) = daemon.serve_connection(stream).await {
                    tracing_debug(&error);
                }
            });
        }
    }

    async fn serve_connection(&self, stream: crate::ipc::IpcStream) -> Result<(), String> {
        let (reader, mut writer) = tokio::io::split(stream);
        let mut lines = BufReader::new(reader);
        let mut line = String::new();
        // Handshake: token must match the profile owner's token.
        let n = lines
            .read_line(&mut line)
            .await
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("connection closed before handshake".into());
        }
        let handshake: DaemonHandshake =
            serde_json::from_str(line.trim()).map_err(|e| e.to_string())?;
        if handshake.token != self.identity.token
            || handshake.profile_id != self.identity.profile_id
        {
            let reject = ApplicationFrame::Error {
                message: "handshake rejected".into(),
            };
            let _ = write_frame(&mut writer, &reject).await;
            return Err("handshake rejected".into());
        }
        let welcome = ApplicationFrame::Welcome(DaemonWelcome {
            daemon_nonce: self.identity.nonce.clone(),
            profile_id: self.identity.profile_id.clone(),
        });
        write_frame(&mut writer, &welcome)
            .await
            .map_err(|e| e.to_string())?;

        loop {
            line.clear();
            let n = lines
                .read_line(&mut line)
                .await
                .map_err(|e| e.to_string())?;
            if n == 0 {
                return Ok(());
            }
            let frame: ApplicationFrame =
                serde_json::from_str(line.trim()).map_err(|e| e.to_string())?;
            match frame {
                ApplicationFrame::Command(command) => {
                    let outcome = self.handler.execute(command.clone()).await;
                    let result = ApplicationResult {
                        client_id: command.client_id,
                        command_id: command.command_id,
                        outcome,
                    };
                    write_frame(&mut writer, &ApplicationFrame::Result(result))
                        .await
                        .map_err(|e| e.to_string())?;
                }
                ApplicationFrame::ReadEvents(request) => {
                    let events = self
                        .handler
                        .events_after(request.after_seq, request.limit)
                        .await;
                    write_frame(&mut writer, &ApplicationFrame::Events(events))
                        .await
                        .map_err(|e| e.to_string())?;
                }
                ApplicationFrame::Handshake(_)
                | ApplicationFrame::Welcome(_)
                | ApplicationFrame::Result(_)
                | ApplicationFrame::Events(_)
                | ApplicationFrame::Error { .. } => {
                    let reject = ApplicationFrame::Error {
                        message: "unexpected frame".into(),
                    };
                    write_frame(&mut writer, &reject)
                        .await
                        .map_err(|e| e.to_string())?;
                }
            }
        }
    }
}

async fn write_frame<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &ApplicationFrame,
) -> Result<(), io::Error> {
    let mut payload = serde_json::to_vec(frame).unwrap_or_default();
    payload.push(b'\n');
    writer.write_all(&payload).await?;
    writer.flush().await
}

fn tracing_debug(error: &str) {
    let _ = error;
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// How long a client waits for the daemon handshake/welcome.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
