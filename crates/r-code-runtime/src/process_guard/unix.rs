//! Unix daemon-EOF guardian.
//!
//! Each managed child runs in its own process group. A small guardian
//! process holds the read end of a pipe whose write end the daemon owns;
//! when the daemon dies the pipe reaches EOF and the guardian sends TERM to
//! the group, escalating to KILL after a grace period. Recovery never acts
//! on PID alone: group membership and start identity are verified first.

#![cfg(unix)]

use std::io;
use std::os::fd::AsRawFd;
use std::time::Duration;

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
}
