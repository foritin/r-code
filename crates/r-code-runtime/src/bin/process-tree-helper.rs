//! P06/P07 process-tree helper: the child driven by the s06/s07 Windows
//! tests.
//!
//! Contract: after resume, the FIRST bytes this process writes to stdout are
//! the literal marker `resumed\n` — nothing may precede it, so a non-empty
//! peek before resume proves the suspension leaked. Modes (all strictly
//! after the marker, combinable):
//!
//! * `--env NAME`     prints `env:NAME=<value>` or `env:NAME=absent`.
//! * `--canary VALUE` probes inherited-handle value `VALUE`; prints
//!   `canary=absent` (not inherited) or `canary=PRESENT` (leak — fail).
//! * `--canary-self`  probes a pipe the child created itself; must print
//!   `canary-self=PRESENT` — the positive control proving the probe
//!   discriminates valid handles.
//! * `--echo-stdin N` reads exactly N stdin bytes, prints `stdin:<bytes>`.
//! * `--exit CODE`    exits with CODE.
//!
//! P07 tree fixtures (executed after argument parsing, still marker-first):
//!
//! * `--grandchild N` spawns N copies of this exe (default stdio inherits
//!   this process's stdout/stderr pipe ends, so their markers flow to the
//!   same observer) and waits for all of them. Each grandchild receives the
//!   same `--sleep` so members stay observable long enough to enumerate.
//! * `--nested-job`   creates its OWN kill-on-close job, assigns itself and
//!   spawns one child into it, waits for the child, then closes the job —
//!   a job-creating descendant that the outer job must still contain.
//! * `--sleep MS`     sleeps MS before exiting.

#[cfg(windows)]
fn main() {
    use std::io::Write;

    let mut stdout = std::io::stdout();
    stdout.write_all(b"resumed\n").expect("marker write");
    stdout.flush().expect("marker flush");

    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let mut exit_code = 0;
    let mut index = 0;
    let mut grandchild_count: usize = 0;
    let mut sleep_ms: u64 = 0;
    let mut nested_job = false;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--env" if index + 1 < arguments.len() => {
                index += 1;
                let name = arguments[index].as_str();
                let line = match std::env::var(name) {
                    Ok(value) => format!("env:{name}={value}\n"),
                    Err(_) => format!("env:{name}=absent\n"),
                };
                stdout.write_all(line.as_bytes()).expect("env line");
            }
            "--canary" if index + 1 < arguments.len() => {
                index += 1;
                let verdict = match arguments[index].parse::<usize>() {
                    Ok(value) if probe_handle(value) => "PRESENT",
                    _ => "absent",
                };
                writeln!(stdout, "canary={verdict}").expect("canary line");
            }
            "--canary-self" => {
                let verdict = if self_probe_reports_present() {
                    "PRESENT"
                } else {
                    "absent"
                };
                writeln!(stdout, "canary-self={verdict}").expect("self probe line");
            }
            "--echo-stdin" if index + 1 < arguments.len() => {
                index += 1;
                let wanted: usize = arguments[index].parse().unwrap_or(0);
                let echoed = read_exact_stdin(wanted);
                writeln!(stdout, "stdin:{}", String::from_utf8_lossy(&echoed)).expect("stdin echo");
            }
            "--exit" if index + 1 < arguments.len() => {
                index += 1;
                exit_code = arguments[index].parse().unwrap_or(0);
            }
            "--grandchild" if index + 1 < arguments.len() => {
                index += 1;
                grandchild_count = arguments[index].parse().unwrap_or(0);
            }
            "--nested-job" => nested_job = true,
            "--sleep" if index + 1 < arguments.len() => {
                index += 1;
                sleep_ms = arguments[index].parse().unwrap_or(0);
            }
            _ => {}
        }
        index += 1;
    }
    if grandchild_count > 0 {
        spawn_grandchildren(grandchild_count, sleep_ms);
    }
    if nested_job {
        run_nested_job(sleep_ms);
    }
    if sleep_ms > 0 {
        std::thread::sleep(std::time::Duration::from_millis(sleep_ms));
    }
    stdout.flush().expect("final flush");
    std::process::exit(exit_code);
}

#[cfg(not(windows))]
fn main() {}

#[cfg(windows)]
fn read_exact_stdin(wanted: usize) -> Vec<u8> {
    use std::io::Read;

    let mut data = vec![0u8; wanted];
    let mut filled = 0;
    while filled < wanted {
        match std::io::stdin().read(&mut data[filled..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => filled += n,
        }
    }
    data.truncate(filled);
    data
}

#[cfg(windows)]
fn self_probe_reports_present() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::Pipes::CreatePipe;

    let mut read: HANDLE = std::ptr::null_mut();
    let mut write: HANDLE = std::ptr::null_mut();
    // SAFETY: valid out-pointers; both ends are closed before returning.
    let created = unsafe { CreatePipe(&mut read, &mut write, std::ptr::null(), 0) };
    if created == 0 {
        return false;
    }
    let present = probe_handle(read as usize);
    // SAFETY: both values came from CreatePipe above and are closed once.
    unsafe {
        CloseHandle(read);
        CloseHandle(write);
    }
    present
}

#[cfg(windows)]
fn probe_handle(value: usize) -> bool {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    // SAFETY: `information` is a valid out-pointer; the probed value comes
    // from the test command line and is only ever passed to this query.
    unsafe {
        let mut information: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        GetFileInformationByHandle(value as HANDLE, &mut information) != 0
    }
}

/// Spawn `count` copies of this executable whose stdout/stderr inherit this
/// process's pipe ends (std defaults to inherit when nothing is piped), so
/// their `resumed\n` markers flow to the same observer. The grandchildren
/// carry the same `--sleep` so they stay alive — and stay listed in the
/// owning job — long enough for the test to enumerate the member list.
#[cfg(windows)]
fn spawn_grandchildren(count: usize, sleep_ms: u64) {
    use std::process::Command;

    let Ok(program) = std::env::current_exe() else {
        return;
    };
    let mut children = Vec::new();
    for _ in 0..count {
        let mut command = Command::new(&program);
        if sleep_ms > 0 {
            command.arg("--sleep").arg(sleep_ms.to_string());
        }
        if let Ok(child) = command.spawn() {
            children.push(child);
        }
    }
    for mut child in children {
        let _ = child.wait();
    }
}

/// Create a nested kill-on-close job, assign this process to it, and spawn
/// one child (which joins both jobs through the hierarchy). The job handle
/// is held until the child exits, then closed — proving a job-creating
/// descendant never escapes the OUTER job (Windows 8+ hierarchical nesting).
#[cfg(windows)]
fn run_nested_job(sleep_ms: u64) {
    use std::process::Command;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    // SAFETY: every handle below is created or opened here and closed
    // exactly once on every path (the job on early returns, the child
    // handle by Command's Drop).
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            return;
        }
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let configured = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &limits as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if configured == 0 {
            CloseHandle(job);
            return;
        }
        if AssignProcessToJobObject(job, GetCurrentProcess()) == 0 {
            CloseHandle(job);
            return;
        }
        if let Ok(program) = std::env::current_exe() {
            let mut command = Command::new(&program);
            if sleep_ms > 0 {
                command.arg("--sleep").arg(sleep_ms.to_string());
            }
            if let Ok(mut child) = command.spawn() {
                let _ = child.wait();
            }
        }
        CloseHandle(job);
    }
}
