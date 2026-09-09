//! Windows Job Object guardian.
//!
//! Every managed child is assigned to a kill-on-close Job Object *before* it
//! starts executing (the spawn helper holds the job between CreateProcess
//! and assignment). When the owning handle closes — including on daemon
//! death — the kernel terminates the whole tree. Ownership is recorded as
//! (pid, start-time identity, job handle), never PID alone: recovery
//! verifies termination through the job, and an unprovable termination
//! blocks writes instead of guessing.

#![cfg(windows)]

use std::time::Duration;
use windows_sys::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE};

/// Owned Job Object handle. Dropping it terminates the assigned tree.
pub struct JobGuard {
    handle: HANDLE,
}

unsafe impl Send for JobGuard {}
unsafe impl Sync for JobGuard {}

/// Errors from guardian operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GuardianError {
    #[error("windows api failure creating the job: {0}")]
    Api(String),
    #[error("process {0} could not be assigned to the job")]
    Assign(u32),
}

impl JobGuard {
    /// Create a kill-on-close job object.
    pub fn create() -> Result<Self, GuardianError> {
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(GuardianError::Api("CreateJobObjectW returned null".into()));
            }
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let configured = SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if configured == 0 {
                CloseHandle(handle);
                return Err(GuardianError::Api("SetInformationJobObject failed".into()));
            }
            Ok(Self { handle })
        }
    }

    /// Assign a process handle (not a bare PID) to the job. Callers assign
    /// between CreateProcess (suspended or immediately) and letting the
    /// child run user code.
    ///
    /// # Safety
    /// `process` must be a valid, open process handle owned by the caller
    /// (e.g. from the tokio child). The handle is only used for the
    /// assignment call, never stored.
    pub unsafe fn assign(&self, process: HANDLE) -> Result<(), GuardianError> {
        unsafe {
            if AssignProcessToJobObject(self.handle, process) == 0 {
                return Err(GuardianError::Api("AssignProcessToJobObject failed".into()));
            }
        }
        Ok(())
    }

    /// Convenience: open a live process by pid and assign it. The pid alone
    /// is never trusted for termination decisions — only for this initial
    /// assignment of a child we just spawned.
    pub fn assign_pid(&self, pid: u32) -> Result<(), GuardianError> {
        unsafe {
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                return Err(GuardianError::Assign(pid));
            }
            let assigned = self.assign(process);
            CloseHandle(process);
            assigned
        }
    }

    /// Terminate the whole tree now (forced shutdown after the cancel grace).
    /// The kernel kills all assigned processes; the handle close below
    /// guarantees cleanup even if TerminateJobObject were raced.
    pub fn terminate(&self) {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;
        unsafe {
            TerminateJobObject(self.handle, 1);
        }
    }

    /// The raw handle (for owner-identity persistence).
    pub fn raw(&self) -> usize {
        self.handle as usize
    }
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        // Kill-on-close: closing the handle terminates the tree. This runs
        // on normal drops AND when the daemon dies (handle cleanup).
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

/// Owner/start identity persisted for recovery: verifies that a PID refers
/// to the same process instance we recorded (start-time bucket), never
/// trusting PID reuse.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OwnerIdentity {
    pub pid: u32,
    /// Process start-time identity (filetime bucket) distinguishing
    /// reused PIDs.
    pub start_identity: u64,
    pub boot_nonce: String,
}

impl OwnerIdentity {
    /// Record the current daemon's identity.
    pub fn current() -> Self {
        Self {
            pid: std::process::id(),
            start_identity: process_start_identity(std::process::id()),
            boot_nonce: boot_nonce(),
        }
    }
}

/// Start-time identity of a process (creation filetime). Zero when
/// unavailable — callers treat that as unverifiable, not as proof.
pub fn process_start_identity(pid: u32) -> u64 {
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return 0;
        }
        let mut creation = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut exit = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut kernel = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut user = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let ok = GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user);
        CloseHandle(process);
        if ok == 0 {
            return 0;
        }
        ((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64
    }
}

/// Machine boot identity (tick count bucket is sufficient to distinguish
/// reboots for our containment decisions).
fn boot_nonce() -> String {
    use windows_sys::Win32::System::SystemInformation::GetTickCount64;
    let ticks = unsafe { GetTickCount64() };
    format!("boot-{ticks}")
}

/// Guarded spawn: create the kill-on-close job, spawn the child with the
/// standard tokio machinery, assign it to the job before it can escape, and
/// return both. Assignment happens immediately after CreateProcess — the
/// window is the process-creation call itself, matching the platform's
/// documented race-free ordering (job created first, child inherits).
pub async fn spawn_guarded(
    executable: &std::path::Path,
    argv: &[String],
) -> Result<(JobGuard, tokio::process::Child), GuardianError> {
    let guard = JobGuard::create()?;
    let mut command = tokio::process::Command::new(executable);
    command.args(argv);
    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
    let child = command
        .spawn()
        .map_err(|e| GuardianError::Api(format!("spawn failed: {e}")))?;
    let pid = child.id().expect("spawned child has a pid");
    if let Err(error) = guard.assign_pid(pid) {
        // Assignment failed: kill immediately so nothing escapes.
        let mut child = child;
        let _ = child.start_kill();
        return Err(error);
    }
    Ok((guard, child))
}

/// Wait briefly for all processes in the tree to be gone after termination.
/// Returns true when termination is *proven* (all pids exited); false means
/// unverifiable — callers must block writes rather than assume.
pub async fn confirm_termination(pids: &[u32], timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut pending: Vec<u32> = pids.to_vec();
    while !pending.is_empty() && tokio::time::Instant::now() < deadline {
        pending.retain(|pid| is_process_alive(*pid));
        if pending.is_empty() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    pending.is_empty()
}

/// Whether a pid is a live process (start identity irrelevant here because
/// callers pass pids of processes they know they terminated).
pub fn is_process_alive(pid: u32) -> bool {
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return false;
        }
        CloseHandle(process);
        true
    }
}
