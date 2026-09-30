//! r-code-process-guardian — guardian-as-spawner binary (P08).
//!
//! The daemon starts this binary FIRST via `spawn_via_guardian`, which
//! dup2's the two O_CLOEXEC control pipes onto fixed descriptors: 3 carries
//! commands (daemon → guardian), 4 carries replies. The guardian inherits
//! only /dev/null stdio plus 3 and 4, forks the gated workload behind a
//! SIGSTOP release gate, and enforces the daemon-EOF kill. One session per
//! process: exit 0 released, 1 daemon-EOF kill, 2 protocol violation (see
//! `GUARDIAN_EXIT_*`). Non-Linux builds compile to an empty main (the
//! process-tree-helper cross-platform pattern).

#[cfg(target_os = "linux")]
fn main() {
    use r_code_runtime::process_guard::unix::{
        guardian_protocol::{GUARDIAN_CONTROL_FD, GUARDIAN_REPLY_FD},
        serve_guardian,
    };
    use std::os::fd::FromRawFd;

    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let mut control_fd = GUARDIAN_CONTROL_FD;
    let mut reply_fd = GUARDIAN_REPLY_FD;
    let mut index = 0;
    while index + 1 < arguments.len() {
        match arguments[index].as_str() {
            "--control-fd" => {
                control_fd = arguments[index + 1].parse().unwrap_or(control_fd);
                index += 1;
            }
            "--reply-fd" => {
                reply_fd = arguments[index + 1].parse().unwrap_or(reply_fd);
                index += 1;
            }
            _ => {}
        }
        index += 1;
    }
    if control_fd < 3 || reply_fd < 3 || control_fd == reply_fd {
        // Refuse to trample stdio or to double-book one descriptor.
        eprintln!("r-code-process-guardian: control descriptors must be distinct and >= 3");
        std::process::exit(2);
    }
    // SAFETY: the parent dup2'd the control pipes onto exactly these
    // descriptor numbers before exec (or we defaulted to the fixed 3/4 the
    // parent always passes); taking ownership wraps each descriptor once and
    // `serve_guardian` drops it once at session end.
    let commands = unsafe { std::fs::File::from_raw_fd(control_fd) };
    let replies = unsafe { std::fs::File::from_raw_fd(reply_fd) };
    let code = serve_guardian(commands, replies);
    std::process::exit(code);
}

#[cfg(not(target_os = "linux"))]
fn main() {}
