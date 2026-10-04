//! Unix daemon-EOF guardian.
//!
//! Each managed child runs in its own process group. A small guardian
//! process holds the read end of a pipe whose write end the daemon owns;
//! when the daemon dies the pipe reaches EOF and the guardian sends TERM to
//! the group, escalating to KILL after a grace period. Recovery never acts
//! on PID alone: group membership and start identity are verified first.
//!
//! P08 guardian-as-spawner: the guardian binary exists FIRST and creates
//! the workload behind a release gate — see [`spawn_via_guardian`] and
//! [`serve_guardian`].

#![cfg(unix)]

use std::io;
use std::time::Duration;

// guardian_protocol 定义帧类型（GuardianFrame），顶层守护流程直接使用其
// 简名；协议编解码内部引用顶层的 GuardianError。
use guardian_protocol::GuardianFrame;

/// Environment variable selecting guardian mode in a host binary.
pub const GUARDIAN_PIPE_ENV: &str = "R_CODE_GUARDIAN_PIPE";
/// Environment variable carrying the guarded process-group id.
pub const GUARDIAN_PGID_ENV: &str = "R_CODE_GUARDIAN_PGID";

/// Errors from the Unix guardian.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GuardianError {
    #[error("io failure: {0}")]
    Io(String),
    #[error("guardian process failed to start")]
    GuardianSpawn,
    /// The control protocol was violated (foreign version, bad frame,
    /// unexpected reply). Additive P08 variant.
    #[error("guardian protocol violation: {0}")]
    Protocol(&'static str),
    /// The environment cannot host the guardian (binary missing, no /proc).
    /// Running unguarded is never a fallback. Additive P08 variant.
    #[error("this environment cannot host the guardian: {0}")]
    Unsupported(String),
    /// The guardian did not answer within the bounded wait named by the
    /// payload. Additive P08 variant.
    #[error("guardian did not answer in time during {0}")]
    Timeout(&'static str),
    /// The guardian exited before confirming the session (clean EOF on a
    /// control pipe). Additive P08 variant.
    #[error("guardian exited before confirming the session (EOF)")]
    GuardianExited,
}

/// The guardian loop: block on the daemon pipe, then TERM → grace → KILL.
/// Host binaries call this (via [`maybe_run_guardian`]) before anything
/// else when invoked in guardian mode; it never returns.
pub fn run_guardian(read_fd: i32, pgid: i32) -> ! {
    use std::io::Read;
    // Wait for EOF (daemon death closes the write end).
    let mut file = unsafe { fd_file(read_fd) };
    let mut buffer = [0u8; 16];
    loop {
        match file.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(_) => continue,
        }
    }
    // Graceful termination first.
    unsafe {
        libc::kill(-pgid, libc::SIGTERM);
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if unsafe { libc::kill(-pgid, 0) } != 0 {
            // Group is gone: contained.
            std::process::exit(0);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // Escalate.
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
    std::process::exit(0);
}

/// If the current process was invoked as a guardian, run it (never returns).
pub fn maybe_run_guardian() {
    let pipe = match std::env::var(GUARDIAN_PIPE_ENV) {
        Ok(value) => value.parse::<i32>().expect("guardian pipe fd"),
        Err(_) => return,
    };
    let pgid = std::env::var(GUARDIAN_PGID_ENV)
        .expect("guardian pgid")
        .parse::<i32>()
        .expect("guardian pgid numeric");
    run_guardian(pipe, pgid);
}

unsafe fn fd_file(fd: i32) -> std::fs::File {
    use std::os::fd::FromRawFd;
    std::fs::File::from_raw_fd(fd)
}

/// A guarded managed child: own process group with a watching guardian.
pub struct GuardedChild {
    pub child: tokio::process::Child,
    pub pgid: i32,
    _daemon_write: std::fs::File,
    _guardian: std::process::Child,
}

/// Spawn `executable` in its own process group with a daemon-EOF guardian
/// (a re-invocation of the current executable in guardian mode).
pub async fn spawn_guarded(
    executable: &std::path::Path,
    argv: &[String],
) -> Result<GuardedChild, GuardianError> {
    use std::os::unix::process::CommandExt;

    let (read_fd, write_fd) = {
        let mut pipes = [0i32; 2];
        if unsafe { libc::pipe(pipes.as_mut_ptr()) } != 0 {
            return Err(GuardianError::Io("pipe creation failed".into()));
        }
        (pipes[0], pipes[1])
    };

    // Spawn the managed child in a fresh process group.
    let mut command = tokio::process::Command::new(executable);
    command.args(argv);
    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    command.process_group(0);
    let child = command
        .spawn()
        .map_err(|e| GuardianError::Io(format!("spawn failed: {e}")))?;
    let pgid = child.id().expect("child pid") as i32;

    // Spawn the guardian: current executable re-invoked in guardian mode
    // with the pipe read end and the group id.
    let mut guardian = std::process::Command::new(std::env::current_exe().unwrap_or_default());
    guardian.env(GUARDIAN_PIPE_ENV, read_fd.to_string());
    guardian.env(GUARDIAN_PGID_ENV, pgid.to_string());
    guardian.stdin(std::process::Stdio::null());
    guardian.stdout(std::process::Stdio::null());
    guardian.stderr(std::process::Stdio::null());
    // Guardians must outlive nothing in particular but never join the
    // managed group themselves.
    guardian.process_group(0);
    let guardian = guardian.spawn().map_err(|_| GuardianError::GuardianSpawn)?;

    // The daemon keeps the write end; closing it (drop or death) triggers
    // the guardian's EOF path.
    let daemon_write = unsafe { fd_file(write_fd) };
    // The read fd is now owned by the guardian process; close ours.
    unsafe {
        libc::close(read_fd);
    }

    Ok(GuardedChild {
        child,
        pgid,
        _daemon_write: daemon_write,
        _guardian: guardian,
    })
}

impl GuardedChild {
    /// Graceful cancellation: TERM to the group, wait, then KILL.
    pub async fn cancel_tree(&mut self) {
        unsafe {
            libc::kill(-self.pgid, libc::SIGTERM);
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                _ => {}
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        unsafe {
            libc::kill(-self.pgid, libc::SIGKILL);
        }
        let _ = self.child.wait().await;
    }
}

/// Wait for a process group to be gone; used by recovery to prove
/// termination before releasing writer barriers.
pub async fn confirm_termination(pgid: i32, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if unsafe { libc::kill(-pgid, 0) } != 0 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

/// Whether a process group leader is alive.
pub fn group_alive(pgid: i32) -> bool {
    unsafe { libc::kill(-pgid, 0) == 0 }
}

/// Start-time identity of a pid via /proc (0 = unverifiable).
pub fn process_start_identity(pid: u32) -> u64 {
    std::fs::read_to_string(format!("/proc/{pid}/stat"))
        .ok()
        .and_then(|stat| {
            // Field 22 (1-indexed) is starttime; comm may contain spaces so
            // split after the closing parenthesis.
            let after_comm = stat.rsplit(')').next()?.trim_start();
            let fields: Vec<&str> = after_comm.split_whitespace().collect();
            // after_comm starts at field 3 (state); starttime is field 22 →
            // index 22 - 3 = 19.
            fields.get(19).and_then(|value| value.parse::<u64>().ok())
        })
        .unwrap_or(0)
}

#[allow(dead_code)]
fn unused_io_marker(_: io::Error) {}

// P08 — guardian-as-spawner launch gate. The daemon spawns the GUARDIAN
// first; the guardian forks the workload behind a release gate and reports
// its identity before the daemon acknowledges persistence. Two O_CLOEXEC
// pipes carry the versioned frames; the guardian inherits exactly fds
// 0/1/2 (/dev/null) plus 3 (commands) and 4 (replies), and the workload's
// pre-exec closes 3+4, so its inherited descriptor set is empty. Release
// is exactly once (the gate is consumed); EOF before release kills the
// still-stopped group; on Linux PR_SET_PDEATHSIG additionally kills the
// workload if the guardian itself dies first.

/// Environment override pointing at the guardian binary; without it the
/// daemon looks for `r-code-process-guardian` next to the current
/// executable.
pub const GUARDIAN_BIN_ENV: &str = "R_CODE_GUARDIAN_BIN";

/// Exit codes of the r-code-process-guardian binary (its observable
/// contract besides the control protocol itself).
pub const GUARDIAN_EXIT_RELEASED: i32 = 0;
pub const GUARDIAN_EXIT_DAEMON_EOF: i32 = 1;
pub const GUARDIAN_EXIT_PROTOCOL: i32 = 2;

/// Bounded waits: a guardian that cannot answer in time is treated as
/// failed, and every failure path tears the session down fail-closed.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const IDENTITY_TIMEOUT: Duration = Duration::from_secs(10);
const RELEASE_ACK_TIMEOUT: Duration = Duration::from_secs(10);
/// Reap window before escalating to SIGKILL on the guardian itself.
const GUARDIAN_REAP_TIMEOUT: Duration = Duration::from_secs(2);

/// Versioned length-prefixed control-protocol frames (P08). Pure codec:
/// every function here is free of unsafe and operates on bytes, so the
/// wire contract is verifiable by reading it.
///
/// Wire format — `[kind: u8][version: u32 LE][body_len: u32 LE][body]`:
/// every frame names its protocol version. Decoding enforces our version
/// on every frame EXCEPT `Hello`, which carries the peer's negotiated
/// version for the explicit refusal path (a foreign version can only be
/// reported, never silently dropped, before the handshake exists).
pub mod guardian_protocol {
    use super::GuardianError;
    use std::io::{self, Read, Write};

    /// Wire protocol version. A peer speaking a different version must
    /// refuse service; the other side fails closed.
    pub const PROTOCOL_VERSION: u32 = 1;
    /// Fixed guardian control descriptors: 3 = commands (daemon →
    /// guardian), 4 = replies (guardian → daemon). The workload's pre-exec
    /// closes both.
    pub const GUARDIAN_CONTROL_FD: i32 = 3;
    pub const GUARDIAN_REPLY_FD: i32 = 4;
    /// Hard cap on one frame body: a SpawnRequest (path, argv, explicit
    /// environment) fits comfortably; anything larger is a bug or an
    /// attack.
    pub const MAX_FRAME_BODY_BYTES: usize = 64 * 1024;
    /// Header size: kind byte + version + body length.
    const FRAME_HEADER_BYTES: usize = 9;

    const KIND_HELLO: u8 = 1;
    const KIND_SPAWN_REQUEST: u8 = 2;
    const KIND_IDENTITY: u8 = 3;
    const KIND_RELEASE: u8 = 4;
    const KIND_RELEASE_ACK: u8 = 5;
    const KIND_ERROR: u8 = 6;

    /// Error codes carried by [`GuardianFrame::Error`].
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum FrameErrorCode {
        Generic = 1,
        VersionMismatch = 2,
        SpawnFailed = 3,
        ProtocolViolation = 4,
    }

    impl FrameErrorCode {
        pub fn from_u32(code: u32) -> Option<Self> {
            match code {
                1 => Some(Self::Generic),
                2 => Some(Self::VersionMismatch),
                3 => Some(Self::SpawnFailed),
                4 => Some(Self::ProtocolViolation),
                _ => None,
            }
        }
    }

    /// One control-protocol frame.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum GuardianFrame {
        /// Handshake open: names the sender's protocol version.
        Hello { version: u32 },
        /// Daemon → guardian: create the gated workload.
        SpawnRequest {
            executable: String,
            arguments: Vec<String>,
            cwd: Option<String>,
            environment: Vec<(String, String)>,
        },
        /// Guardian → daemon: outer/group/birth identity of the stopped
        /// workload, delivered BEFORE the release gate.
        Identity {
            outer_pid: u32,
            group_pid: u32,
            birth_start_identity: u64,
        },
        /// Daemon → guardian: open the gate (SIGCONT the group once).
        Release,
        /// Guardian → daemon: the release happened.
        ReleaseAck,
        /// Guardian → daemon: refusal or violation; the session is over.
        Error { code: FrameErrorCode },
    }

    fn put_u32(out: &mut Vec<u8>, value: u32) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    fn put_u64(out: &mut Vec<u8>, value: u64) {
        out.extend_from_slice(&value.to_le_bytes());
    }

    /// Strings travel length-prefixed and must be NUL-free: they become
    /// execve argv/envp members on the guardian side, where interior NULs
    /// are unrepresentable.
    fn put_str(out: &mut Vec<u8>, value: &str) -> Result<(), GuardianError> {
        if value.contains('\0') {
            return Err(GuardianError::Protocol(
                "control-protocol strings must not contain NUL",
            ));
        }
        put_u32(out, value.len() as u32);
        out.extend_from_slice(value.as_bytes());
        Ok(())
    }

    /// Encode one frame. Pure: returns the exact wire bytes or a protocol
    /// error (NUL in strings, '=' in environment keys, oversized body).
    pub fn encode_frame(frame: &GuardianFrame) -> Result<Vec<u8>, GuardianError> {
        let (kind, version, mut body) = match frame {
            GuardianFrame::Hello { version } => (KIND_HELLO, *version, Vec::new()),
            GuardianFrame::SpawnRequest {
                executable,
                arguments,
                cwd,
                environment,
            } => {
                let mut body = Vec::new();
                put_str(&mut body, executable)?;
                let count = u32::try_from(arguments.len())
                    .map_err(|_| GuardianError::Protocol("too many arguments"))?;
                put_u32(&mut body, count);
                for argument in arguments {
                    put_str(&mut body, argument)?;
                }
                match cwd {
                    Some(cwd) => {
                        body.push(1);
                        put_str(&mut body, cwd)?;
                    }
                    None => body.push(0),
                }
                let count = u32::try_from(environment.len())
                    .map_err(|_| GuardianError::Protocol("too many environment entries"))?;
                put_u32(&mut body, count);
                for (key, value) in environment {
                    if key.is_empty() || key.contains('=') {
                        return Err(GuardianError::Protocol(
                            "environment keys must be non-empty and free of '='",
                        ));
                    }
                    put_str(&mut body, key)?;
                    put_str(&mut body, value)?;
                }
                (KIND_SPAWN_REQUEST, PROTOCOL_VERSION, body)
            }
            GuardianFrame::Identity {
                outer_pid,
                group_pid,
                birth_start_identity,
            } => {
                let mut body = Vec::with_capacity(16);
                put_u32(&mut body, *outer_pid);
                put_u32(&mut body, *group_pid);
                put_u64(&mut body, *birth_start_identity);
                (KIND_IDENTITY, PROTOCOL_VERSION, body)
            }
            GuardianFrame::Release => (KIND_RELEASE, PROTOCOL_VERSION, Vec::new()),
            GuardianFrame::ReleaseAck => (KIND_RELEASE_ACK, PROTOCOL_VERSION, Vec::new()),
            GuardianFrame::Error { code } => {
                let mut body = Vec::with_capacity(4);
                put_u32(&mut body, *code as u32);
                (KIND_ERROR, PROTOCOL_VERSION, body)
            }
        };
        if body.len() > MAX_FRAME_BODY_BYTES {
            return Err(GuardianError::Protocol(
                "frame body exceeds the hard length cap",
            ));
        }
        let mut out = Vec::with_capacity(FRAME_HEADER_BYTES + body.len());
        out.push(kind);
        put_u32(&mut out, version);
        put_u32(&mut out, body.len() as u32);
        out.append(&mut body);
        Ok(out)
    }

    /// Consume one complete frame from the FRONT of `buffer` (pure).
    /// Ok(None) means "need more bytes"; Err means the bytes already
    /// violate the protocol (wrong version, oversized body, unknown kind,
    /// malformed body).
    pub fn decode_frame(buffer: &mut &[u8]) -> Result<Option<GuardianFrame>, GuardianError> {
        if buffer.len() < FRAME_HEADER_BYTES {
            return Ok(None);
        }
        let kind = buffer[0];
        let version = u32::from_le_bytes(buffer[1..5].try_into().expect("4 header bytes"));
        let body_len =
            u32::from_le_bytes(buffer[5..9].try_into().expect("4 header bytes")) as usize;
        validate_header(kind, version, body_len)?;
        if buffer.len() < FRAME_HEADER_BYTES + body_len {
            return Ok(None);
        }
        let frame = parse_body(
            kind,
            version,
            &buffer[FRAME_HEADER_BYTES..FRAME_HEADER_BYTES + body_len],
        )?;
        *buffer = &buffer[FRAME_HEADER_BYTES + body_len..];
        Ok(Some(frame))
    }

    /// Version policy: only `Hello` may carry a foreign version (it IS the
    /// negotiation); every other frame must speak ours or be rejected.
    fn validate_header(kind: u8, version: u32, body_len: usize) -> Result<(), GuardianError> {
        if kind != KIND_HELLO && version != PROTOCOL_VERSION {
            return Err(GuardianError::Protocol("control frame version mismatch"));
        }
        if body_len > MAX_FRAME_BODY_BYTES {
            return Err(GuardianError::Protocol(
                "frame body exceeds the hard length cap",
            ));
        }
        Ok(())
    }

    fn parse_body(kind: u8, version: u32, body: &[u8]) -> Result<GuardianFrame, GuardianError> {
        let mut cursor = body;
        let frame = match kind {
            KIND_HELLO => GuardianFrame::Hello { version },
            KIND_SPAWN_REQUEST => decode_spawn_request(&mut cursor)?,
            KIND_IDENTITY => GuardianFrame::Identity {
                outer_pid: take_u32(&mut cursor)?,
                group_pid: take_u32(&mut cursor)?,
                birth_start_identity: take_u64(&mut cursor)?,
            },
            KIND_RELEASE => {
                ensure_empty(&mut cursor)?;
                GuardianFrame::Release
            }
            KIND_RELEASE_ACK => {
                ensure_empty(&mut cursor)?;
                GuardianFrame::ReleaseAck
            }
            KIND_ERROR => GuardianFrame::Error {
                code: FrameErrorCode::from_u32(take_u32(&mut cursor)?).ok_or(
                    GuardianError::Protocol("unknown control-protocol error code"),
                )?,
            },
            _ => return Err(GuardianError::Protocol("unknown control frame kind")),
        };
        if !cursor.is_empty() {
            return Err(GuardianError::Protocol("frame body has trailing bytes"));
        }
        Ok(frame)
    }

    fn ensure_empty(cursor: &mut &[u8]) -> Result<(), GuardianError> {
        if !cursor.is_empty() {
            return Err(GuardianError::Protocol("frame body must be empty"));
        }
        Ok(())
    }

    fn decode_spawn_request(cursor: &mut &[u8]) -> Result<GuardianFrame, GuardianError> {
        let executable = take_str(cursor)?;
        let argument_count = take_u32(cursor)? as usize;
        // Bound the declared counts before any allocation: a hostile length
        // must not turn into a huge with_capacity (each argument costs at
        // least one length prefix, each environment entry at least two).
        if argument_count > MAX_FRAME_BODY_BYTES / 4 {
            return Err(GuardianError::Protocol(
                "spawn request argument count exceeds the frame cap",
            ));
        }
        let mut arguments = Vec::with_capacity(argument_count);
        for _ in 0..argument_count {
            arguments.push(take_str(cursor)?);
        }
        let cwd = match take_bytes(cursor, 1)? {
            [0] => None,
            [1] => Some(take_str(cursor)?),
            _ => return Err(GuardianError::Protocol("invalid cwd presence byte")),
        };
        let environment_count = take_u32(cursor)? as usize;
        if environment_count > MAX_FRAME_BODY_BYTES / 8 {
            return Err(GuardianError::Protocol(
                "spawn request environment count exceeds the frame cap",
            ));
        }
        let mut environment = Vec::with_capacity(environment_count);
        for _ in 0..environment_count {
            let key = take_str(cursor)?;
            if key.is_empty() || key.contains('=') {
                return Err(GuardianError::Protocol(
                    "environment keys must be non-empty and free of '='",
                ));
            }
            let value = take_str(cursor)?;
            environment.push((key, value));
        }
        Ok(GuardianFrame::SpawnRequest {
            executable,
            arguments,
            cwd,
            environment,
        })
    }

    fn take_bytes<'a>(cursor: &mut &'a [u8], wanted: usize) -> Result<&'a [u8], GuardianError> {
        if cursor.len() < wanted {
            return Err(GuardianError::Protocol("truncated frame body"));
        }
        let (head, tail) = cursor.split_at(wanted);
        *cursor = tail;
        Ok(head)
    }

    fn take_u32(cursor: &mut &[u8]) -> Result<u32, GuardianError> {
        let raw = take_bytes(cursor, 4)?;
        Ok(u32::from_le_bytes(raw.try_into().expect("4 bytes")))
    }

    fn take_u64(cursor: &mut &[u8]) -> Result<u64, GuardianError> {
        let raw = take_bytes(cursor, 8)?;
        Ok(u64::from_le_bytes(raw.try_into().expect("8 bytes")))
    }

    fn take_str(cursor: &mut &[u8]) -> Result<String, GuardianError> {
        let len = take_u32(cursor)? as usize;
        let raw = take_bytes(cursor, len)?;
        let text = std::str::from_utf8(raw)
            .map_err(|_| GuardianError::Protocol("frame string is not UTF-8"))?;
        if text.contains('\0') {
            return Err(GuardianError::Protocol(
                "control-protocol strings must not contain NUL",
            ));
        }
        Ok(text.to_owned())
    }

    /// Blocking write of one frame. A broken pipe means the peer is gone
    /// (mapped to [`GuardianError::GuardianExited`]); other errors are Io.
    pub fn write_frame<W: Write>(
        writer: &mut W,
        frame: &GuardianFrame,
    ) -> Result<(), GuardianError> {
        let bytes = encode_frame(frame)?;
        writer.write_all(&bytes).map_err(|error| {
            if error.kind() == io::ErrorKind::BrokenPipe {
                GuardianError::GuardianExited
            } else {
                GuardianError::Io(error.to_string())
            }
        })
    }

    /// Blocking read of exactly one frame. Ok(None) = clean EOF at a frame
    /// boundary (peer closed); Err covers mid-frame truncation and every
    /// codec violation.
    pub fn read_frame<R: Read>(reader: &mut R) -> Result<Option<GuardianFrame>, GuardianError> {
        let mut header = [0u8; FRAME_HEADER_BYTES];
        if !read_exact_or_eof(reader, &mut header)? {
            return Ok(None);
        }
        let kind = header[0];
        let version = u32::from_le_bytes(header[1..5].try_into().expect("4 header bytes"));
        let body_len =
            u32::from_le_bytes(header[5..9].try_into().expect("4 header bytes")) as usize;
        validate_header(kind, version, body_len)?;
        let mut body = vec![0u8; body_len];
        if !read_exact_or_eof(reader, &mut body)? {
            return Err(GuardianError::Protocol("control stream ended mid-frame"));
        }
        Ok(Some(parse_body(kind, version, &body)?))
    }

    /// Fill `out` completely; Ok(false) means the stream hit EOF with ZERO
    /// bytes consumed (a clean frame boundary), any partial fill is a
    /// truncation. Handles EINTR by retrying.
    fn read_exact_or_eof<R: Read>(reader: &mut R, out: &mut [u8]) -> Result<bool, GuardianError> {
        let mut filled = 0usize;
        while filled < out.len() {
            match reader.read(&mut out[filled..]) {
                Ok(0) => {
                    return if filled == 0 {
                        Ok(false)
                    } else {
                        Err(GuardianError::Protocol("control stream ended mid-frame"))
                    };
                }
                Ok(n) => filled += n,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(GuardianError::Io(error.to_string())),
            }
        }
        Ok(true)
    }
}

/// Outer/group/birth identity of the gated workload (P08.2): delivered by
/// the guardian BEFORE the release gate so the daemon can persist first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GuardianIdentity {
    pub outer_pid: u32,
    pub group_pid: u32,
    pub birth_start_identity: u64,
}

/// Explicit spawn request handed to the guardian (P08): the workload
/// receives exactly this argv/cwd/environment — never the daemon's or the
/// guardian's inherited environment (mirror of windows.rs RawSpawnSpec).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuardianSpawnRequest {
    pub executable: String,
    pub arguments: Vec<String>,
    pub cwd: Option<String>,
    pub environment: std::collections::BTreeMap<String, String>,
}

/// A workload created BEHIND the guardian's release gate (P08): at handout
/// it exists only as a SIGSTOPped image in its own process group — no
/// instruction of its binary has run and it holds no control descriptors.
/// The daemon persists [`GatedWorkload::identity`] first, then opens the
/// gate with [`GatedWorkload::release_once`] exactly once.
///
/// Dropping the gate WITHOUT releasing is fail-closed by construction: the
/// command end closes, the guardian reads EOF and SIGKILLs the group (and
/// on Linux PR_SET_PDEATHSIG kills the workload even if the guardian dies
/// first).
pub struct GatedWorkload {
    /// Daemon → guardian commands; taken (None) by `release_once`.
    control_write: Option<std::fs::File>,
    /// Guardian → daemon replies, held for the release acknowledgement.
    reply_read: Option<std::fs::File>,
    identity: GuardianIdentity,
    /// The guardian process itself; reaped on every path.
    guardian: Option<std::process::Child>,
}

impl std::fmt::Debug for GatedWorkload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Descriptors and handles are never printed; durable identity only.
        f.debug_struct("GatedWorkload")
            .field("identity", &self.identity)
            .finish()
    }
}

impl GatedWorkload {
    /// Identity to persist BEFORE releasing the gate.
    pub fn identity(&self) -> GuardianIdentity {
        self.identity
    }

    /// Open the release gate exactly once (P08.3): consume the gate, send
    /// `Release`, and require `ReleaseAck`. The gate is consumed, so a
    /// second release cannot be expressed. EOF before the ack means the
    /// guardian is gone — the workload died with it (guardian EOF path or
    /// Linux PDEATHSIG) — and no live handle is ever returned: every
    /// failure path kills the group and reaps the guardian first. An
    /// unconfirmed release is treated as failed even if the gate may have
    /// opened: fail-closed means dead, never running-unowned.
    pub fn release_once(mut self) -> Result<ReleasedWorkload, GuardianError> {
        let mut control_write = self
            .control_write
            .take()
            .expect("gate fields are present until release_once takes them");
        let reply_read = self
            .reply_read
            .take()
            .expect("gate fields are present until release_once takes them");
        let mut guardian = self
            .guardian
            .take()
            .expect("gate fields are present until release_once takes them");
        let identity = self.identity;
        guardian_protocol::write_frame(&mut control_write, &GuardianFrame::Release)?;
        // The reply CLONE moves into the bounded-read helper thread; on
        // timeout the clone stays with the thread until the guardian's EOF
        // (see read_frame_bounded) — the original reply end is ours.
        match read_frame_bounded(
            reply_read
                .try_clone()
                .map_err(|error| GuardianError::Io(error.to_string()))?,
            RELEASE_ACK_TIMEOUT,
            "release acknowledgement",
        ) {
            Ok(GuardianFrame::ReleaseAck) => {
                // Gate open; both control ends close (the guardian is
                // finishing its reap/exit and needs nothing further).
                drop(control_write);
                drop(reply_read);
                Ok(ReleasedWorkload { identity, guardian })
            }
            outcome => {
                // Anything else (Error frame, foreign frame, EOF, timeout)
                // means the release was never confirmed: fail closed — kill
                // the group, then reap the guardian — and surface the error.
                drop(control_write);
                drop(reply_read);
                kill_group(identity.group_pid as i32, libc::SIGKILL);
                reap_guardian_bounded(&mut guardian);
                Err(match outcome {
                    Ok(_) => GuardianError::Protocol("guardian did not acknowledge the release"),
                    Err(error) => error,
                })
            }
        }
    }
}

impl Drop for GatedWorkload {
    fn drop(&mut self) {
        // Fail-closed teardown of an UNRELEASED gate, in strict order: close
        // the command end (EOF → the guardian SIGKILLs the still-stopped
        // group), close the reply end, then reap the guardian with a
        // bounded window. Taken fields are None: a consumed gate never
        // double-closes and never kills a released workload.
        drop(self.control_write.take());
        drop(self.reply_read.take());
        if let Some(mut guardian) = self.guardian.take() {
            reap_guardian_bounded(&mut guardian);
        }
    }
}

/// A released workload (post-[`GatedWorkload::release_once`]): the gate is
/// open — the group was SIGCONTed exactly once — and the identity was
/// already persisted by the daemon. Owns the guardian process for reaping
/// only; the workload's lifecycle belongs to the daemon through its group.
#[derive(Debug)]
pub struct ReleasedWorkload {
    identity: GuardianIdentity,
    guardian: std::process::Child,
}

impl ReleasedWorkload {
    /// Identity of the released workload (as persisted before the release).
    pub fn identity(&self) -> GuardianIdentity {
        self.identity
    }

    /// Reap the guardian when it has exited (it exits right after the ack,
    /// once it has reaped the workload). None while it is still running.
    pub fn reap_guardian(&mut self) -> Option<std::process::ExitStatus> {
        self.guardian.try_wait().ok().flatten()
    }

    /// Block until the guardian exits — which is exactly when the released
    /// workload (and its group) has been reaped by the guardian.
    pub fn wait_guardian(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.guardian.wait()
    }
}

impl Drop for ReleasedWorkload {
    fn drop(&mut self) {
        // Post-release the guardian only reaps the (running) workload; it is
        // NEVER killed here — a SIGKILL would cascade to the workload via
        // PR_SET_PDEATHSIG. Best-effort reap of an already-exited guardian;
        // a still-running one is reaped by wait_guardian/reap_guardian or
        // reparented to init when the daemon exits.
        let _ = self.guardian.try_wait();
    }
}

/// Spawn the workload BEHIND the guardian's release gate (P08): the
/// guardian binary is located and started FIRST (P08.1: it inherits only
/// /dev/null stdio plus the two control descriptors dup2'd onto 3 and 4),
/// the protocol handshake runs, then `SpawnRequest` makes the guardian
/// create the SIGSTOPped workload and report its identity (P08.2). The
/// returned gate blocks all workload execution until
/// [`GatedWorkload::release_once`] — persist the identity first. Fully
/// synchronous (std only); the supervisor wires async later.
pub fn spawn_via_guardian(request: GuardianSpawnRequest) -> Result<GatedWorkload, GuardianError> {
    use std::os::unix::process::CommandExt;

    // P08.1: both control pipes are O_CLOEXEC; only the dup2'd copies on
    // the fixed descriptors survive exec, and the workload closes those —
    // no control descriptor can reach the workload.
    let (command_read, command_write) = create_cloexec_pipe()?;
    let (reply_read, reply_write) = create_cloexec_pipe()?;
    let command_write = unsafe { fd_file(command_write) };
    let reply_read = unsafe { fd_file(reply_read) };
    let command_read = RawFdClose(command_read);
    let reply_write = RawFdClose(reply_write);

    let guardian_path = locate_guardian_binary()?;

    let mut command = std::process::Command::new(&guardian_path);
    command
        .arg("--control-fd")
        .arg(guardian_protocol::GUARDIAN_CONTROL_FD.to_string())
        .arg("--reply-fd")
        .arg(guardian_protocol::GUARDIAN_REPLY_FD.to_string());
    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::null());
    command.stderr(std::process::Stdio::null());
    // The guardian leads its own group, detached from the daemon's.
    command.process_group(0);
    let control_fd = command_read.0;
    let reply_fd = reply_write.0;
    unsafe {
        // SAFETY: this callback runs in the forked guardian child before
        // exec, single-threaded, with async-signal-safe calls only. Its
        // invariant: the guardian inherits EXACTLY fds 0/1/2 (each nulled
        // to /dev/null) plus the two control ends on 3 and 4 — every other
        // descriptor the daemon owns is O_CLOEXEC and vanishes at exec.
        command.pre_exec(move || {
            place_control_fd(control_fd, guardian_protocol::GUARDIAN_CONTROL_FD)?;
            place_control_fd(reply_fd, guardian_protocol::GUARDIAN_REPLY_FD)?;
            Ok(())
        });
    }
    // The guardian is created BEFORE any workload exists (P08 ordering).
    let mut guardian = command
        .spawn()
        .map_err(|error| GuardianError::Io(format!("guardian spawn failed: {error}")))?;
    // The dup2'd copies live in the guardian now; drop the originals so its
    // exit is observable as EOF on the reply pipe.
    drop(command_read);
    drop(reply_write);

    let (identity, command_write, reply_read, guardian) =
        negotiate_guardian_spawn(command_write, reply_read, guardian, &request)?;
    Ok(GatedWorkload {
        control_write: Some(command_write),
        reply_read: Some(reply_read),
        identity,
        guardian: Some(guardian),
    })
}

/// Handshake + gated spawn over an already-started guardian session. Owns
/// the session ends so EVERY failure path tears them down fail-closed:
/// close the command end (EOF → the guardian's kill path), then reap the
/// guardian. Returns the identity plus the owned session ends on success.
fn negotiate_guardian_spawn(
    mut command_write: std::fs::File,
    reply_read: std::fs::File,
    mut guardian: std::process::Child,
    request: &GuardianSpawnRequest,
) -> Result<
    (
        GuardianIdentity,
        std::fs::File,
        std::fs::File,
        std::process::Child,
    ),
    GuardianError,
> {
    let negotiated = (|| -> Result<GuardianIdentity, GuardianError> {
        guardian_protocol::write_frame(
            &mut command_write,
            &GuardianFrame::Hello {
                version: guardian_protocol::PROTOCOL_VERSION,
            },
        )?;
        match read_frame_bounded(
            reply_read
                .try_clone()
                .map_err(|error| GuardianError::Io(error.to_string()))?,
            HANDSHAKE_TIMEOUT,
            "guardian handshake",
        ) {
            Ok(GuardianFrame::Hello {
                version: guardian_protocol::PROTOCOL_VERSION,
            }) => {}
            Ok(GuardianFrame::Error {
                code: guardian_protocol::FrameErrorCode::VersionMismatch,
            }) => {
                return Err(GuardianError::Protocol(
                    "guardian rejected our protocol version",
                ));
            }
            Ok(GuardianFrame::Hello { .. }) => {
                return Err(GuardianError::Protocol(
                    "guardian speaks a different protocol version",
                ));
            }
            Ok(_) => {
                return Err(GuardianError::Protocol(
                    "guardian handshake produced an unexpected frame",
                ));
            }
            Err(error) => return Err(error),
        }
        guardian_protocol::write_frame(
            &mut command_write,
            &GuardianFrame::SpawnRequest {
                executable: request.executable.clone(),
                arguments: request.arguments.clone(),
                cwd: request.cwd.clone(),
                environment: request
                    .environment
                    .iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            },
        )?;
        match read_frame_bounded(
            reply_read
                .try_clone()
                .map_err(|error| GuardianError::Io(error.to_string()))?,
            IDENTITY_TIMEOUT,
            "workload identity",
        ) {
            Ok(GuardianFrame::Identity {
                outer_pid,
                group_pid,
                birth_start_identity,
            }) => {
                let identity = GuardianIdentity {
                    outer_pid,
                    group_pid,
                    birth_start_identity,
                };
                // Fail closed on an unverifiable identity: the gate must
                // not open for a tree we cannot describe.
                if identity.birth_start_identity == 0 || !group_alive(identity.group_pid as i32) {
                    kill_group(identity.group_pid as i32, libc::SIGKILL);
                    return Err(GuardianError::Protocol(
                        "guardian returned an unverifiable workload identity",
                    ));
                }
                Ok(identity)
            }
            Ok(GuardianFrame::Error {
                code: guardian_protocol::FrameErrorCode::SpawnFailed,
            }) => Err(GuardianError::Protocol(
                "guardian could not create the gated workload",
            )),
            Ok(_) => Err(GuardianError::Protocol(
                "guardian sent an unexpected frame instead of the workload identity",
            )),
            Err(error) => Err(error),
        }
    })();
    if let Err(error) = negotiated {
        // Close the command end FIRST (EOF is the guardian's own fail-closed
        // trigger), then reap the guardian with a bounded window; a wedged
        // guardian is SIGKILLed (on Linux its PDEATHSIG-armed stopped
        // workload dies with it).
        drop(command_write);
        reap_guardian_bounded(&mut guardian);
        return Err(error);
    }
    let identity = negotiated.expect("negotiation result checked above");
    Ok((identity, command_write, reply_read, guardian))
}

/// Locate the guardian binary: `R_CODE_GUARDIAN_BIN` override, else
/// `r-code-process-guardian` next to the current executable (no `.exe`
/// suffix — this path is unix-only). Missing means Unsupported, never a
/// fallback to running unguarded.
fn locate_guardian_binary() -> Result<std::path::PathBuf, GuardianError> {
    if let Ok(path) = std::env::var(GUARDIAN_BIN_ENV) {
        let path = std::path::PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(GuardianError::Unsupported(format!(
            "{GUARDIAN_BIN_ENV} does not point at a guardian binary"
        )));
    }
    let path = match std::env::current_exe()
        .ok()
        .as_deref()
        .and_then(std::path::Path::parent)
    {
        Some(directory) => directory.join("r-code-process-guardian"),
        None => return Err(GuardianError::GuardianSpawn),
    };
    if !path.is_file() {
        return Err(GuardianError::Unsupported(format!(
            "guardian binary not found at {}: build r-code-process-guardian or set {GUARDIAN_BIN_ENV}",
            path.display()
        )));
    }
    Ok(path)
}

/// Point one fixed control descriptor at the pipe end inside the pre-exec
/// callback. dup2(fd, fd) is a documented NO-OP that does not clear
/// O_CLOEXEC, so a pipe that already landed on the fixed number must clear
/// its own close-on-exec flag instead of being dup2'd onto itself.
fn place_control_fd(source_fd: i32, fixed_fd: i32) -> io::Result<()> {
    // SAFETY: fcntl/dup2 on descriptors created by create_cloexec_pipe; the
    // F_SETFD write is per-descriptor and dup2 atomically installs the copy.
    unsafe {
        if source_fd == fixed_fd {
            if libc::fcntl(source_fd, libc::F_SETFD, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
        } else if libc::dup2(source_fd, fixed_fd) == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// `pipe2(O_CLOEXEC)` on Linux; `pipe` + explicit FD_CLOEXEC elsewhere.
/// P08.1: every control descriptor must vanish at exec unless explicitly
/// dup2'd onto a fixed descriptor.
fn create_cloexec_pipe() -> Result<(i32, i32), GuardianError> {
    let mut fds = [0i32; 2];
    #[cfg(target_os = "linux")]
    {
        // SAFETY: pipe2 fills the two out-slots; O_CLOEXEC makes both ends
        // vanish at exec so only explicit dup2 copies can be inherited.
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(GuardianError::Io(
                "pipe2 for the guardian control channel failed".into(),
            ));
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        // SAFETY: pipe fills the two out-slots.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(GuardianError::Io(
                "pipe for the guardian control channel failed".into(),
            ));
        }
        for fd in fds {
            // SAFETY: F_SETFD on a descriptor created immediately above.
            if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
                let error = io::Error::last_os_error();
                // SAFETY: both descriptors were created above; the failure
                // path closes both exactly once before returning.
                unsafe {
                    libc::close(fds[0]);
                    libc::close(fds[1]);
                }
                return Err(GuardianError::Io(format!(
                    "fcntl(FD_CLOEXEC) failed: {error}"
                )));
            }
        }
    }
    Ok((fds[0], fds[1]))
}

/// RAII owner of a raw descriptor that exists only between pipe2 and the
/// guardian spawn (the guardian-side pipe ends, in the daemon).
struct RawFdClose(i32);

impl Drop for RawFdClose {
    fn drop(&mut self) {
        // SAFETY: the descriptor came from create_cloexec_pipe, was never
        // wrapped in an owning type, and is closed exactly once here.
        unsafe { libc::close(self.0) };
    }
}

/// One bounded control-frame read. Choice documented (P08): std pipes have
/// no portable timed read, so the blocking read runs on a detached helper
/// thread and the answer races `recv_timeout`. On timeout the helper stays
/// blocked on the CLONE and self-terminates at EOF — every timeout path
/// closes the command end and reaps (or kills) the guardian, which closes
/// the reply pipe and wakes it; the clone then closes exactly once.
fn read_frame_bounded(
    reader: std::fs::File,
    timeout: Duration,
    stage: &'static str,
) -> Result<GuardianFrame, GuardianError> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let outcome = match guardian_protocol::read_frame(&mut reader) {
            Ok(Some(frame)) => Ok(frame),
            // EOF before any frame: the peer is gone.
            Ok(None) => Err(GuardianError::GuardianExited),
            Err(error) => Err(error),
        };
        let _ = sender.send(outcome);
        // `reader` drops here (or at EOF after an abandoned timeout).
    });
    receiver
        .recv_timeout(timeout)
        .unwrap_or_else(|_| Err(GuardianError::Timeout(stage)))
}

/// Reap the guardian within a bounded window; escalate to SIGKILL if it
/// will not exit. Never blocks unbounded, so Drop paths stay responsive.
fn reap_guardian_bounded(guardian: &mut std::process::Child) {
    let deadline = std::time::Instant::now() + GUARDIAN_REAP_TIMEOUT;
    while std::time::Instant::now() < deadline {
        match guardian.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => return,
        }
    }
    let _ = guardian.kill();
    let _ = guardian.wait();
}

/// Signal a whole process group. The pgid always comes from a group the
/// guardian created and verified (`getpgid == pid`), never from an
/// unverified pid.
fn kill_group(pgid: i32, signal: i32) {
    // SAFETY: kill(2) with a negative pid signals the group; delivering the
    // signal IS the intended effect and no memory is involved.
    unsafe {
        libc::kill(-pgid, signal);
    }
}

// The stop-gate race argument: the workload's SIGSTOP is raised inside the
// pre_exec callback, which runs in the forked child BEFORE execve replaces
// the image — between fork and raise the child executes only guardian-image
// code, so no workload instruction can precede SIGCONT regardless of
// scheduling. For the same reason raise(SIGSTOP) is the LAST pre-exec step:
// stopping earlier would freeze the remaining setup (pdeathsig, setpgid,
// control-fd close) until a SIGCONT only the guardian may deliver.

/// Guardian binary entry point (P08): run ONE control session over the two
/// fixed descriptors and exit with the [`GUARDIAN_EXIT_*`] code. Every
/// abnormal path kills the group — the session never leaves a live gate
/// behind.
pub fn serve_guardian(commands: std::fs::File, replies: std::fs::File) -> i32 {
    let mut commands = commands;
    let mut replies = replies;
    guardian_session(&mut commands, &mut replies)
}

/// The guardian's control session state machine: handshake → gated spawn →
/// release-or-EOF. Pure over the two streams; the only unsafe lives in
/// [`fork_stopped_workload`] and [`kill_group`].
fn guardian_session<R: std::io::Read, W: std::io::Write>(commands: &mut R, replies: &mut W) -> i32 {
    use guardian_protocol::{
        read_frame, write_frame, FrameErrorCode, GuardianFrame, PROTOCOL_VERSION,
    };

    // Handshake: refuse foreign versions explicitly (versioned refusal).
    let daemon_version = match read_frame(commands) {
        Ok(Some(GuardianFrame::Hello { version })) => version,
        Ok(Some(_)) => {
            let _ = write_frame(
                replies,
                &GuardianFrame::Error {
                    code: FrameErrorCode::ProtocolViolation,
                },
            );
            return GUARDIAN_EXIT_PROTOCOL;
        }
        // Daemon died before the handshake: nothing is gated.
        Ok(None) => return GUARDIAN_EXIT_DAEMON_EOF,
        Err(_) => return GUARDIAN_EXIT_PROTOCOL,
    };
    if daemon_version != PROTOCOL_VERSION {
        let _ = write_frame(
            replies,
            &GuardianFrame::Error {
                code: FrameErrorCode::VersionMismatch,
            },
        );
        return GUARDIAN_EXIT_PROTOCOL;
    }
    if write_frame(
        replies,
        &GuardianFrame::Hello {
            version: PROTOCOL_VERSION,
        },
    )
    .is_err()
    {
        return GUARDIAN_EXIT_PROTOCOL;
    }

    // Spawn request: the gated workload is created HERE, stopped.
    let (executable, arguments, cwd, environment) = match read_frame(commands) {
        Ok(Some(GuardianFrame::SpawnRequest {
            executable,
            arguments,
            cwd,
            environment,
        })) => (executable, arguments, cwd, environment),
        Ok(Some(_)) => {
            let _ = write_frame(
                replies,
                &GuardianFrame::Error {
                    code: FrameErrorCode::ProtocolViolation,
                },
            );
            return GUARDIAN_EXIT_PROTOCOL;
        }
        Ok(None) => return GUARDIAN_EXIT_DAEMON_EOF,
        Err(_) => {
            let _ = write_frame(
                replies,
                &GuardianFrame::Error {
                    code: FrameErrorCode::ProtocolViolation,
                },
            );
            return GUARDIAN_EXIT_PROTOCOL;
        }
    };
    let (mut workload, identity) =
        match fork_stopped_workload(&executable, &arguments, cwd.as_deref(), &environment) {
            Ok(created) => created,
            Err(_) => {
                // Nothing is gated; report and drain until the daemon closes.
                let _ = write_frame(
                    replies,
                    &GuardianFrame::Error {
                        code: FrameErrorCode::SpawnFailed,
                    },
                );
                return drain_to_eof(commands);
            }
        };
    if write_frame(
        replies,
        &GuardianFrame::Identity {
            outer_pid: identity.outer_pid,
            group_pid: identity.group_pid,
            birth_start_identity: identity.birth_start_identity,
        },
    )
    .is_err()
    {
        // The daemon never learned the identity: fail closed.
        kill_group(identity.group_pid as i32, libc::SIGKILL);
        let _ = workload.wait();
        return GUARDIAN_EXIT_PROTOCOL;
    }

    // Release-or-EOF loop: the group stays stopped until `Release`.
    loop {
        match read_frame(commands) {
            Ok(Some(GuardianFrame::Release)) => {
                // Open the gate exactly once.
                kill_group(identity.group_pid as i32, libc::SIGCONT);
                if write_frame(replies, &GuardianFrame::ReleaseAck).is_err() {
                    // An undeliverable ack means the release was never
                    // confirmed: the daemon must not be left owning a live
                    // tree it did not acknowledge.
                    kill_group(identity.group_pid as i32, libc::SIGKILL);
                    let _ = workload.wait();
                    return GUARDIAN_EXIT_PROTOCOL;
                }
                // Reaping the released workload is intended to block: the
                // guardian's lifetime doubles as zombie hygiene.
                let _ = workload.wait();
                return GUARDIAN_EXIT_RELEASED;
            }
            // A repeat Release/SpawnRequest or any other frame before the
            // ack is a protocol violation: fail closed.
            Ok(Some(_)) => {
                let _ = write_frame(
                    replies,
                    &GuardianFrame::Error {
                        code: FrameErrorCode::ProtocolViolation,
                    },
                );
                kill_group(identity.group_pid as i32, libc::SIGKILL);
                let _ = workload.wait();
                return GUARDIAN_EXIT_PROTOCOL;
            }
            // EOF BEFORE release (daemon death or dropped gate): the gate
            // must never open — kill the still-stopped group.
            Ok(None) => {
                kill_group(identity.group_pid as i32, libc::SIGKILL);
                let _ = workload.wait();
                return GUARDIAN_EXIT_DAEMON_EOF;
            }
            Err(_) => {
                kill_group(identity.group_pid as i32, libc::SIGKILL);
                let _ = workload.wait();
                return GUARDIAN_EXIT_PROTOCOL;
            }
        }
    }
}

/// Fork the workload STOPPED (P08.2). Pre-exec, in order: arm
/// PR_SET_PDEATHSIG (Linux — the kernel kills the workload if the guardian
/// dies, even while stopped), `setpgid(0, 0)` (fresh group, double-sided
/// with the parent call below), close the control descriptors 3/4 (their
/// dup2 copies lost O_CLOEXEC — this is what keeps the workload's
/// inheritance empty), and FINALLY `raise(SIGSTOP)` (the gate; see the
/// race argument above). The parent then pins and verifies the group,
/// reads the birth identity from /proc (fail-closed: an unreadable
/// identity is not delivered), and hands back child + identity.
fn fork_stopped_workload(
    executable: &str,
    arguments: &[String],
    cwd: Option<&str>,
    environment: &[(String, String)],
) -> Result<(std::process::Child, GuardianIdentity), GuardianError> {
    use std::os::unix::process::CommandExt;

    let mut command = std::process::Command::new(executable);
    command.args(arguments);
    // The workload's stdio comes from the guardian (which the daemon runs
    // with /dev/null), keeping the workload silent in production while
    // observable when the guardian is driven manually in tests.
    command.stdin(std::process::Stdio::inherit());
    command.stdout(std::process::Stdio::inherit());
    command.stderr(std::process::Stdio::inherit());
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    // Exactly the explicit environment — never inherited.
    command.env_clear();
    for (key, value) in environment {
        command.env(key, value);
    }
    unsafe {
        // SAFETY: runs in the forked child before exec, single-threaded,
        // async-signal-safe calls only. Invariant at exec time: the child
        // dies with the guardian (PDEATHSIG), leads its own group, holds NO
        // control descriptor (3/4 closed; everything else is O_CLOEXEC),
        // and is SIGSTOPped so nothing runs before the guardian's SIGCONT.
        command.pre_exec(|| {
            #[cfg(target_os = "linux")]
            {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            if libc::setpgid(0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            for fd in [
                guardian_protocol::GUARDIAN_CONTROL_FD,
                guardian_protocol::GUARDIAN_REPLY_FD,
            ] {
                // EBADF (never opened) is fine to ignore: nothing to leak.
                let _ = libc::close(fd);
            }
            if libc::raise(libc::SIGSTOP) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut workload = command
        .spawn()
        .map_err(|error| GuardianError::Io(format!("gated workload spawn failed: {error}")))?;
    let pid = workload.id();

    // Double-sided setpgid: the parent pins the group too, so the group
    // exists even if the child's own call raced with exec (EACCES then —
    // impossible before SIGCONT, but tolerated defensively). The group id
    // is verified via getpgid, never assumed.
    let pgid = unsafe {
        // SAFETY: setpgid/getpgid on our direct child; no memory involved.
        if libc::setpgid(pid as i32, pid as i32) != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EACCES)
                && error.raw_os_error() != Some(libc::ESRCH)
            {
                let _ = workload.kill();
                let _ = workload.wait();
                return Err(GuardianError::Io(format!(
                    "setpgid for the gated workload failed: {error}"
                )));
            }
        }
        let observed = libc::getpgid(pid as i32);
        if observed < 0 {
            let _ = workload.kill();
            let _ = workload.wait();
            return Err(GuardianError::Io(
                "getpgid for the gated workload failed".into(),
            ));
        }
        observed
    };
    if pgid != pid as i32 {
        let _ = workload.kill();
        let _ = workload.wait();
        return Err(GuardianError::Io(
            "the gated workload did not become its own group leader".into(),
        ));
    }
    // Birth identity from /proc, fail-closed: P08.2 delivers an identity;
    // an unreadable one is not delivered (the gate stays shut).
    let birth_start_identity = process_start_identity(pid);
    if birth_start_identity == 0 {
        let _ = workload.kill();
        let _ = workload.wait();
        return Err(GuardianError::Io(
            "the gated workload birth identity is unreadable via /proc".into(),
        ));
    }
    Ok((
        workload,
        GuardianIdentity {
            outer_pid: pid,
            group_pid: pgid as u32,
            birth_start_identity,
        },
    ))
}

/// Drain the command stream to EOF (spawn-failure aftermath): no workload
/// exists, so a belated Release must not be acknowledged and an EOF must
/// simply end the session.
fn drain_to_eof<R: std::io::Read>(commands: &mut R) -> i32 {
    let mut sink = [0u8; 512];
    loop {
        match commands.read(&mut sink) {
            Ok(0) | Err(_) => return GUARDIAN_EXIT_DAEMON_EOF,
            Ok(_) => continue,
        }
    }
}

/// P09 — bwrap PID-namespace tree ownership and proof (Linux only).
/// Consumes the P16 launch plan's identity contract: ONE --unshare-pid
/// namespace whose PID1 is the bwrap-forked sandbox init. THE PROOF is
/// the death of THAT PID1 — observed through its own pidfd (obtained via
/// bwrap's --info-fd handoff) plus the disappearance of its /proc entry
/// after the monitor reaps it. When PID1 dies the kernel SIGKILLs every
/// remaining namespace member, subsuming setsid/double-fork escapees; a
/// process-group or workload-leader exit is NEVER the proof (the monitor
/// outlives the leader, and the leader exiting first is normal bwrap).
/// Every signal and wait goes through pidfds or our own child handle, so
/// a recycled foreign pid is never signalled or mistaken for the tree;
/// --die-with-parent arms the reverse (monitor death kills the sandbox).
#[cfg(target_os = "linux")]
pub mod bwrap_proof {
    use super::GuardianError;
    use std::collections::BTreeMap;
    use std::io::Read;
    use std::os::unix::io::AsRawFd;
    use std::os::unix::io::FromRawFd;
    use std::os::unix::process::CommandExt;

    /// RAII owner of a pidfd.
    pub struct PidFd(std::os::unix::io::RawFd);

    impl PidFd {
        pub fn raw(&self) -> std::os::unix::io::RawFd {
            self.0
        }
    }

    impl std::fmt::Debug for PidFd {
        fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            // 只暴露描述符编号——pidfd 本身不可读。
            formatter.debug_tuple("PidFd").field(&self.0).finish()
        }
    }

    impl Drop for PidFd {
        fn drop(&mut self) {
            // SAFETY: the fd was created by pidfd_open and is closed once.
            unsafe { libc::close(self.0) };
        }
    }

    /// The identity material persisted BEFORE release (P09.1): the
    /// namespace PID1 (the bwrap sandbox init, handed out via --info-fd)
    /// with its pidfd, start-time fence and namespace inode, plus the
    /// outer monitor child we own and must reap.
    #[derive(Debug)]
    pub struct BwrapTreeIdentity {
        /// Host-side pid of the sandbox namespace PID1.
        pub namespace_pid1: u32,
        /// /proc start-time fence for the PID1 pid (reuse guard).
        pub pid1_start_identity: u64,
        /// Inode identifier of the sandbox PID namespace (ns/pid symlink
        /// of PID1 — NOT the monitor's host namespace).
        pub namespace_inode: String,
        /// pidfd of PID1: the proof signal.
        pub pid1_pidfd: PidFd,
        /// The outer bwrap monitor: our own child; reaped by wait().
        monitor_child: Option<std::process::Child>,
        /// Host-side pid of the monitor (diagnostics; fencing is not
        /// needed — the Child handle owns its identity).
        pub monitor_pid: u32,
    }

    /// Open a pidfd for `pid`. None on failure (vanishing race, ENOSYS).
    pub fn pidfd_open(pid: u32) -> Option<PidFd> {
        // SAFETY: syscall with plain value arguments; the return is an fd
        // or -1 with errno set.
        let fd =
            unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0u32) as libc::c_int };
        if fd < 0 {
            return None;
        }
        Some(PidFd(fd))
    }

    /// Signal a process THROUGH its pidfd (PID-reuse safe). All four
    /// syscall arguments are passed explicitly — the flags register must
    /// be a real zero, not whatever the varargs ABI left there.
    pub fn pidfd_signal(fd: &PidFd, signal: libc::c_int) -> bool {
        // SAFETY: syscall with plain value arguments; the siginfo pointer
        // and flags are both explicitly zero.
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                fd.raw(),
                signal,
                0usize, // siginfo pointer: NULL for plain signals
                0u32,   // flags
            )
        };
        result == 0
    }

    /// The PID-namespace identifier of `pid` (the `ns/pid` symlink text).
    pub fn namespace_inode(pid: u32) -> Option<String> {
        std::fs::read_link(format!("/proc/{pid}/ns/pid"))
            .ok()
            .map(|target| target.to_string_lossy().into_owned())
    }

    /// The /proc stat start-time fence (matches the P08 identity source).
    pub fn proc_start_identity(pid: u32) -> Option<u64> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // Field 22 (starttime) follows the closing paren of comm.
        let after_paren = stat.rsplit(')').next()?;
        let mut fields = after_paren.split_whitespace();
        fields.nth(19)?.parse::<u64>().ok()
    }

    /// Poll one fd until readable or the deadline passes.
    fn poll_readable(fd: std::os::unix::io::RawFd, timeout: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let mut pollfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: pollfd is a plain in/out slot for one fd.
            let ready = unsafe { libc::poll(&mut pollfd, 1, 50) };
            if ready > 0 && (pollfd.revents & libc::POLLIN) != 0 {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
        }
    }

    /// Wait (bounded) for PID1's pidfd to become readable — the kernel
    /// marks a pidfd readable exactly when the target exits (zombie
    /// included; the monitor's reaping is its own business). True iff the
    /// exit was observed within the timeout.
    pub fn wait_pid1_exit(identity: &BwrapTreeIdentity, timeout: std::time::Duration) -> bool {
        poll_readable(identity.pid1_pidfd.raw(), timeout)
    }

    /// THE proof (P09.2): the namespace is empty exactly when its single
    /// PID1 exited — pidfd readable — AND the PID1's /proc entry is gone
    /// after the monitor reaped it. A pid recycled onto a foreign process
    /// keeps /proc alive, so only the entry's disappearance completes the
    /// proof; anything still observable is UNPROVEN, never guessed. This
    /// subsumes setsid/double-fork descendants (the kernel SIGKILLs them
    /// when PID1 dies) and never consults process groups.
    pub fn prove_namespace_empty(
        identity: &BwrapTreeIdentity,
        timeout: std::time::Duration,
    ) -> Result<bool, GuardianError> {
        if !wait_pid1_exit(identity, timeout) {
            return Ok(false);
        }
        // Give the monitor a moment to reap PID1 (its own waitpid), then
        // require the PID1 /proc entry to be gone.
        let reap_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < reap_deadline {
            if namespace_inode(identity.namespace_pid1).is_none() {
                return Ok(true);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        Ok(false)
    }

    /// Terminate the whole tree (P09.2): SIGKILL the namespace PID1
    /// through its pidfd — the kernel then kills every namespace member —
    /// plus the monitor as a belt (it exits once PID1 is reaped anyway).
    pub fn terminate_tree(identity: &mut BwrapTreeIdentity) -> bool {
        let delivered = pidfd_signal(&identity.pid1_pidfd, libc::SIGKILL);
        if let Some(mut child) = identity.monitor_child.take() {
            let _ = child.kill();
        }
        delivered
    }

    /// Block until the monitor child exits and reap it (bounded). The
    /// monitor is OUR child, so a plain wait is correct here.
    pub fn wait_monitor(identity: &mut BwrapTreeIdentity, timeout: std::time::Duration) -> bool {
        let Some(mut child) = identity.monitor_child.take() else {
            return true;
        };
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return true,
                Ok(None) => {}
                Err(_) => return false,
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// Launch one bwrap tree from the P16 plan (P09.1). The plan argv is
    /// amended with `--die-with-parent --info-fd 3` and the workload
    /// appended; fd 3 (a pre-exec dup2 of a pipe write end) receives the
    /// child-pid JSON handoff from the monitor BEFORE the sandbox runs,
    /// so the identity (PID1 pidfd + start fence + sandbox ns inode) is
    /// captured before supervision. The environment is cleared and only
    /// the allowlist values the caller provides are set (the P16 launcher
    /// contract); the child group is fresh so no stray signal reaches
    /// the host shell group.
    pub fn launch_bwrap_tree(
        bwrap_argv_prefix: &[String],
        workload_argv: &[String],
        environment: &BTreeMap<String, String>,
        cwd: Option<&std::path::Path>,
    ) -> Result<BwrapTreeIdentity, GuardianError> {
        // The plan's trailing placeholder ("--" + marker) is replaced by
        // the real workload terminator and argv.
        let prefix_end = bwrap_argv_prefix
            .iter()
            .rposition(|argument| argument == "--")
            .unwrap_or(bwrap_argv_prefix.len());
        let mut argv: Vec<String> = bwrap_argv_prefix[..prefix_end].to_vec();
        argv.push("--die-with-parent".into());
        argv.push("--info-fd".into());
        argv.push("3".into());
        argv.push("--".into());
        argv.extend(workload_argv.iter().cloned());

        // SAFETY: pipe fds are created before the command; the pre_exec
        // closure runs between fork and exec in the single-threaded child
        // (dup2 + close only — async-signal-safe).
        let (info_read, info_write) = unsafe {
            let mut fds = [0 as libc::c_int; 2];
            if libc::pipe(fds.as_mut_ptr()) != 0 {
                return Err(GuardianError::Io("info pipe failed".into()));
            }
            (fds[0], fds[1])
        };
        let mut info_file = unsafe { std::fs::File::from_raw_fd(info_read) };
        let mut command = std::process::Command::new(&argv[0]);
        let write_fd = info_write;
        unsafe {
            command.pre_exec(move || {
                if libc::dup2(write_fd, 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(write_fd);
                Ok(())
            });
        }
        command
            .args(&argv[1..])
            .env_clear()
            .envs(environment)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .process_group(0);
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        let mut child = command
            .spawn()
            .map_err(|error| GuardianError::Io(format!("bwrap spawn failed: {error}")))?;
        // The parent's copy of the write end must close so the read sees
        // EOF after the monitor's handoff.
        // SAFETY: plain close of the duplicated write end.
        unsafe { libc::close(info_write) };
        let mut handoff = String::new();
        info_file
            .read_to_string(&mut handoff)
            .map_err(|error| GuardianError::Io(format!("info-fd read failed: {error}")))?;
        let pid1 = serde_json::from_str::<serde_json::Value>(&handoff)
            .ok()
            .and_then(|json| {
                json.get("child-pid")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|pid| u32::try_from(pid).ok())
            })
            .ok_or_else(|| {
                GuardianError::Io(format!("info-fd handoff unparseable: {handoff:?}"))
            })?;
        let pid1_pidfd = pidfd_open(pid1)
            .ok_or_else(|| GuardianError::Io("pidfd_open failed for the sandbox PID1".into()))?;
        let pid1_start_identity = proc_start_identity(pid1)
            .ok_or_else(|| GuardianError::Io("sandbox PID1 start identity unreadable".into()))?;
        let namespace_inode = namespace_inode(pid1)
            .ok_or_else(|| GuardianError::Io("sandbox PID1 namespace inode unreadable".into()))?;
        let monitor_pid = child.id();
        Ok(BwrapTreeIdentity {
            namespace_pid1: pid1,
            pid1_start_identity,
            namespace_inode,
            pid1_pidfd,
            monitor_child: Some(child),
            monitor_pid,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_identity_is_readable_for_the_current_process() {
        let identity = process_start_identity(std::process::id());
        assert!(identity > 0, "/proc starttime must parse");
    }

    #[tokio::test]
    async fn guarded_child_runs_and_terminates_with_its_group() {
        let mut guarded = spawn_guarded("/bin/sh".as_ref(), &["-c".into(), "sleep 30".into()])
            .await
            .expect("guarded spawn");
        assert!(group_alive(guarded.pgid));
        guarded.cancel_tree().await;
        assert!(
            confirm_termination(guarded.pgid, Duration::from_secs(5)).await,
            "group terminated after cancellation"
        );
    }

    // P08 codec unit tests — pure wire-format checks. The file is cfg(unix),
    // so these run on Linux/macOS CI only; they never spawn processes
    // (integration coverage belongs to QA's s08 suite).

    #[test]
    fn p08_frames_roundtrip_through_codec_and_stream() {
        let frames = vec![
            GuardianFrame::Hello {
                version: guardian_protocol::PROTOCOL_VERSION,
            },
            GuardianFrame::Hello { version: 9 },
            GuardianFrame::SpawnRequest {
                executable: "/bin/sh".into(),
                arguments: vec!["-c".into(), "echo hi".into()],
                cwd: Some("/tmp".into()),
                environment: vec![("KEY".into(), "value".into())],
            },
            GuardianFrame::SpawnRequest {
                executable: "/bin/true".into(),
                arguments: Vec::new(),
                cwd: None,
                environment: Vec::new(),
            },
            GuardianFrame::Identity {
                outer_pid: 42,
                group_pid: 42,
                birth_start_identity: 123_456,
            },
            GuardianFrame::Release,
            GuardianFrame::ReleaseAck,
            GuardianFrame::Error {
                code: guardian_protocol::FrameErrorCode::VersionMismatch,
            },
        ];
        for frame in frames {
            // Transport path: write_frame into memory, read_frame back out.
            let mut wire = Vec::new();
            guardian_protocol::write_frame(&mut wire, &frame).expect("encode+write");
            let decoded = guardian_protocol::read_frame(&mut wire.as_slice())
                .expect("stream read")
                .expect("complete frame");
            assert_eq!(decoded, frame);
            // Pure codec path: encode, decode, whole buffer consumed.
            let bytes = guardian_protocol::encode_frame(&frame).expect("encode");
            let mut buffer: &[u8] = &bytes;
            assert_eq!(
                guardian_protocol::decode_frame(&mut buffer)
                    .expect("decode")
                    .expect("frame"),
                frame
            );
            assert!(buffer.is_empty(), "codec must consume the whole frame");
        }
    }

    #[test]
    fn p08_decode_rejects_foreign_versions_oversized_and_nul() {
        // Non-Hello frame with a foreign version.
        let mut bytes = guardian_protocol::encode_frame(&GuardianFrame::Release).expect("encode");
        bytes[1..5].copy_from_slice(&999u32.to_le_bytes());
        assert!(matches!(
            guardian_protocol::decode_frame(&mut bytes.as_slice()),
            Err(GuardianError::Protocol("control frame version mismatch"))
        ));
        // Oversized declared body.
        let mut bytes = guardian_protocol::encode_frame(&GuardianFrame::Release).expect("encode");
        bytes[5..9]
            .copy_from_slice(&((guardian_protocol::MAX_FRAME_BODY_BYTES + 1) as u32).to_le_bytes());
        assert!(matches!(
            guardian_protocol::decode_frame(&mut bytes.as_slice()),
            Err(GuardianError::Protocol(
                "frame body exceeds the hard length cap"
            ))
        ));
        // NUL in a spawn string cannot be encoded.
        let frame = GuardianFrame::SpawnRequest {
            executable: "/bin/sh\0".into(),
            arguments: Vec::new(),
            cwd: None,
            environment: Vec::new(),
        };
        assert!(guardian_protocol::encode_frame(&frame).is_err());
    }

    #[test]
    fn p08_decode_reports_incomplete_prefixes_as_none() {
        let bytes = guardian_protocol::encode_frame(&GuardianFrame::Identity {
            outer_pid: 1,
            group_pid: 2,
            birth_start_identity: 3,
        })
        .expect("encode");
        for cut in 0..bytes.len() {
            let mut prefix: &[u8] = &bytes[..cut];
            assert_eq!(
                guardian_protocol::decode_frame(&mut prefix).expect("prefixes never error"),
                None
            );
        }
    }

    #[test]
    fn p08_stream_rejects_mid_frame_truncation_and_unknown_kinds() {
        // A frame whose declared body the stream cannot deliver.
        let mut bytes = guardian_protocol::encode_frame(&GuardianFrame::Identity {
            outer_pid: 1,
            group_pid: 2,
            birth_start_identity: 3,
        })
        .expect("encode");
        let full_len = bytes.len();
        bytes.truncate(full_len - 8);
        assert!(matches!(
            guardian_protocol::read_frame(&mut bytes.as_slice()),
            Err(GuardianError::Protocol("control stream ended mid-frame"))
        ));
        // Unknown frame kind.
        let mut bytes = guardian_protocol::encode_frame(&GuardianFrame::Release).expect("encode");
        bytes[0] = 99;
        assert!(matches!(
            guardian_protocol::decode_frame(&mut bytes.as_slice()),
            Err(GuardianError::Protocol("unknown control frame kind"))
        ));
    }
}
