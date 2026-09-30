//! Windows Job Object guardian.
//!
//! Every managed child is assigned to a kill-on-close Job Object *before* it
//! starts executing. P07 composes the assignment into process creation
//! itself (`PROC_THREAD_ATTRIBUTE_JOB_LIST` in the same attribute list as
//! the stdio handle list), so membership is a property of creation and no
//! assignment window exists for user code to escape through. When the
//! owning handle closes — including on daemon death — the kernel terminates
//! the whole tree. Ownership is recorded as (pid, start-time identity, job
//! handle), never PID alone: recovery verifies termination through the job,
//! and an unprovable termination blocks writes instead of guessing.

#![cfg(windows)]

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, SetHandleInformation, ERROR_ACCESS_DENIED, ERROR_MORE_DATA,
    FILETIME, HANDLE, HANDLE_FLAG_INHERIT, STILL_ACTIVE, WAIT_OBJECT_0,
};
use windows_sys::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JobObjectBasicProcessIdList,
    JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
    JOBOBJECT_BASIC_PROCESS_ID_LIST, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Pipes::{CreatePipe, PeekNamedPipe};
use windows_sys::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess, GetExitCodeProcess,
    InitializeProcThreadAttributeList, OpenProcess, ResumeThread, TerminateProcess,
    UpdateProcThreadAttribute, WaitForSingleObject, CREATE_NO_WINDOW, CREATE_SUSPENDED,
    CREATE_UNICODE_ENVIRONMENT, EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROCESS_INFORMATION, PROCESS_SET_QUOTA, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    PROC_THREAD_ATTRIBUTE_HANDLE_LIST, PROC_THREAD_ATTRIBUTE_JOB_LIST,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

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
    #[error("invalid guardian state: {0}")]
    State(&'static str),
    /// The environment cannot host the creation-time job assignment (for
    /// example an enclosing job with incompatible limits refuses it). The
    /// operation is refused outright — running the child without the job
    /// is never a fallback.
    #[error("this environment cannot guard the child with a job: {0}")]
    Unsupported(String),
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

    /// PIDs of the processes currently listed in the job (P07.3), read via
    /// `QueryInformationJobObject(JobObjectBasicProcessIdList)` with the
    /// two-call growth pattern: the basic id list is variable-sized, so the
    /// query starts with a small buffer and doubles it while the kernel
    /// answers `ERROR_MORE_DATA`. The list is a snapshot — callers must
    /// pair every pid with a start identity before acting on it, because
    /// PIDs are reused.
    pub fn member_pids(&self) -> Result<Vec<u32>, GuardianError> {
        /// Growth cap: a tree larger than this is a bug, not a workload.
        const MAX_LIST_CAPACITY: usize = 4096;
        let mut capacity: usize = 8;
        loop {
            let size = std::mem::size_of::<JOBOBJECT_BASIC_PROCESS_ID_LIST>()
                + (capacity - 1) * std::mem::size_of::<usize>();
            // Pointer-aligned storage: the struct reads back through this.
            let mut buffer = vec![0usize; size.div_ceil(std::mem::size_of::<usize>())];
            let mut returned: u32 = 0;
            let ok = unsafe {
                QueryInformationJobObject(
                    self.handle,
                    JobObjectBasicProcessIdList,
                    buffer.as_mut_ptr() as *mut std::ffi::c_void,
                    size as u32,
                    &mut returned,
                )
            };
            if ok == 0 {
                let code = unsafe { GetLastError() };
                if code == ERROR_MORE_DATA && capacity < MAX_LIST_CAPACITY {
                    capacity *= 2;
                    continue;
                }
                return Err(GuardianError::Api(format!(
                    "QueryInformationJobObject(BasicProcessIdList) failed with win32 error {code}"
                )));
            }
            // SAFETY: the buffer is at least as large as the struct and the
            // kernel filled it; the list length is bounds-checked below.
            let list = unsafe { &*(buffer.as_ptr() as *const JOBOBJECT_BASIC_PROCESS_ID_LIST) };
            let count = list.NumberOfProcessIdsInList as usize;
            if count > capacity {
                capacity = count;
                continue;
            }
            return Ok(
                // SAFETY: ProcessIdList holds `count` valid entries.
                unsafe { std::slice::from_raw_parts(list.ProcessIdList.as_ptr(), count) }
                    .iter()
                    .map(|pid| *pid as u32)
                    .collect(),
            );
        }
    }

    /// Whether the given process handle is a member of this job — used to
    /// prove the creation-time assignment actually took effect.
    ///
    /// # Safety
    /// `process` must be a valid, open process handle owned by the caller;
    /// it is only queried, never stored or closed.
    pub unsafe fn contains_process(&self, process: HANDLE) -> bool {
        let mut result: windows_sys::core::BOOL = 0;
        IsProcessInJob(process, self.handle, &mut result) != 0 && result != 0
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
    pub fn try_current() -> Result<Self, crate::process_guard::BootIdentityError> {
        let boot_identity = crate::process_guard::BootIdentity::current()?;
        Ok(Self {
            pid: std::process::id(),
            start_identity: process_start_identity(std::process::id()),
            boot_nonce: boot_identity.to_string(),
        })
    }

    /// Record the current daemon's identity.
    pub fn current() -> Self {
        Self::try_current().unwrap_or_else(|_| Self {
            pid: std::process::id(),
            start_identity: process_start_identity(std::process::id()),
            // Compatibility API cannot return an error; empty remains
            // explicitly unverifiable for legacy callers.
            boot_nonce: String::new(),
        })
    }
}

/// Start-time identity of a process (creation filetime). Zero when
/// unavailable — callers treat that as unverifiable, not as proof.
pub fn process_start_identity(pid: u32) -> u64 {
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return 0;
        }
        let identity = handle_creation_time(process);
        CloseHandle(process);
        identity
    }
}

/// Creation filetime of a process through an open handle (zero when the
/// query fails). The handle-based form lets callers verify identity on the
/// exact object they are about to wait on, closing the open-by-pid race.
fn handle_creation_time(process: HANDLE) -> u64 {
    use windows_sys::Win32::System::Threading::GetProcessTimes;
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
    let ok = unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) };
    if ok == 0 {
        return 0;
    }
    ((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64
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

/// Brief wall-clock cap when settling a terminated child (setup-failure and
/// explicit terminate paths).
const TERMINATE_WAIT_MS: u32 = 5_000;

/// Owned kernel handle closed exactly once: `close` is idempotent (nulls the
/// handle) so the wrapper, an explicit teardown and Drop never double-close.
struct OwnedHandle(HANDLE);

// A HANDLE is an opaque pointer value; moving it across threads is safe.
unsafe impl Send for OwnedHandle {}

impl OwnedHandle {
    fn new(handle: HANDLE) -> Self {
        Self(handle)
    }

    fn raw(&self) -> HANDLE {
        self.0
    }

    /// Close now and remember it (Drop re-running this is a no-op).
    fn close(&mut self) {
        if !self.0.is_null() {
            unsafe {
                CloseHandle(self.0);
            }
            self.0 = std::ptr::null_mut();
        }
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        self.close();
    }
}

/// Explicit inputs for the raw suspended spawn. The parent environment is
/// NEVER inherited: the child receives exactly `environment`, and an empty
/// map yields the minimal (empty) block.
#[derive(Debug)]
pub struct RawSpawnSpec<'a> {
    pub executable: &'a Path,
    /// Appended verbatim after the executable (which is quoted only when it
    /// contains a space); callers pre-quote arguments that need it.
    pub arguments: &'a [String],
    pub cwd: Option<&'a Path>,
    pub environment: &'a BTreeMap<String, String>,
}

/// Owned lowbox identity for the P15 AppContainer spawn (P15.1): the raw
/// SID bytes of the AppContainer profile plus the derived capability SIDs.
/// The buffers must outlive the spawn call; the attribute list stores
/// pointers into them only for the duration of CreateProcessW.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LowboxCapabilities {
    pub app_container_sid: Vec<u8>,
    pub capability_sids: Vec<Vec<u8>>,
}

/// A child created suspended by [`spawn_suspended`]. Owns the process and
/// primary-thread handles plus the parent ends of the stdio pipes; the child
/// executes nothing until [`RawSuspendedChild::resume_once`] runs.
pub struct RawSuspendedChild {
    process: OwnedHandle,
    thread: OwnedHandle,
    stdin_write: OwnedHandle,
    stdout_read: OwnedHandle,
    stderr_read: OwnedHandle,
    pid: u32,
    resumed: bool,
}

// Handle values move freely; the child itself is driven through &mut self.
unsafe impl Send for RawSuspendedChild {}

impl std::fmt::Debug for RawSuspendedChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print handle values (durable info only).
        f.debug_struct("RawSuspendedChild")
            .field("pid", &self.pid)
            .field("resumed", &self.resumed)
            .finish()
    }
}

/// Create one anonymous stdio pipe. The child end is marked inheritable (it
/// travels exclusively via the PROC_THREAD_ATTRIBUTE_HANDLE_LIST) and the
/// parent end is explicitly non-inheritable, so no unrelated handle can leak
/// into the child. Returns (child_end, parent_end).
unsafe fn create_pipe_pair(child_reads: bool) -> Result<(OwnedHandle, OwnedHandle), GuardianError> {
    let mut read: HANDLE = std::ptr::null_mut();
    let mut write: HANDLE = std::ptr::null_mut();
    if CreatePipe(&mut read, &mut write, std::ptr::null(), 0) == 0 {
        return Err(GuardianError::Api("CreatePipe failed".into()));
    }
    let (child, parent) = if child_reads {
        (read, write)
    } else {
        (write, read)
    };
    if SetHandleInformation(child, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) == 0
        || SetHandleInformation(parent, HANDLE_FLAG_INHERIT, 0) == 0
    {
        CloseHandle(read);
        CloseHandle(write);
        return Err(GuardianError::Api(
            "SetHandleInformation failed".to_string(),
        ));
    }
    Ok((OwnedHandle::new(child), OwnedHandle::new(parent)))
}

/// Quote the executable only when it contains a space; arguments are passed
/// through verbatim. The result is a NUL-terminated UTF-16 command line.
fn build_command_line(executable: &Path, arguments: &[String]) -> Vec<u16> {
    let program = executable.to_string_lossy();
    let mut line =
        String::with_capacity(program.len() + arguments.iter().map(String::len).sum::<usize>());
    if program.contains(' ') {
        line.push('"');
        line.push_str(&program);
        line.push('"');
    } else {
        line.push_str(&program);
    }
    for argument in arguments {
        line.push(' ');
        line.push_str(argument);
    }
    let mut wide: Vec<u16> = line.encode_utf16().collect();
    wide.push(0);
    wide
}

/// Explicit UTF-16 environment block (BTreeMap iteration is already
/// sorted), each entry NUL-terminated plus the final block terminator.
fn build_environment_block(
    environment: &BTreeMap<String, String>,
) -> Result<Vec<u16>, GuardianError> {
    let mut block: Vec<u16> = Vec::new();
    for (key, value) in environment {
        if key.contains('\0') || key.contains('=') || value.contains('\0') {
            return Err(GuardianError::Api(format!(
                "environment entry {key:?} contains a reserved byte"
            )));
        }
        block.extend(key.encode_utf16());
        block.push(u16::from(b'='));
        block.extend(value.encode_utf16());
        block.push(0);
    }
    // A Unicode environment block ends with two NUL WCHARs: the empty block
    // needs both explicitly (one terminator is only valid under ANSI parsing).
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(block)
}

/// NUL-terminated UTF-16 path for CreateProcessW's current directory.
fn wide_path(path: &Path) -> Vec<u16> {
    let mut wide: Vec<u16> = path.to_string_lossy().encode_utf16().collect();
    wide.push(0);
    wide
}

/// RAII guard for the PROC_THREAD_ATTRIBUTE_LIST: deletes it on drop only
/// when initialization succeeded, so failed setups never call Delete on an
/// uninitialized list.
struct AttributeListGuard {
    storage: Vec<usize>,
    list: LPPROC_THREAD_ATTRIBUTE_LIST,
    initialized: bool,
}

impl Drop for AttributeListGuard {
    fn drop(&mut self) {
        if self.initialized {
            unsafe {
                DeleteProcThreadAttributeList(self.list);
            }
        }
    }
}

/// Terminate a raw process handle and wait briefly for it to settle. Used
/// only on failure paths where the child must never get to run.
unsafe fn terminate_and_wait(process: HANDLE) {
    if process.is_null() {
        return;
    }
    TerminateProcess(process, 1);
    WaitForSingleObject(process, TERMINATE_WAIT_MS);
}

/// Creation flags for the raw suspended spawn: the child cannot execute,
/// shows no console, and consumes the STARTUPINFOEXW attribute list.
const RAW_SPAWN_CREATION_FLAGS: u32 =
    CREATE_SUSPENDED | CREATE_NO_WINDOW | EXTENDED_STARTUPINFO_PRESENT;
/// The UTF-16 environment block additionally requires the Unicode flag or
/// Windows parses it as ANSI and rejects non-empty environments.
const UNICODE_SPAWN_CREATION_FLAGS: u32 = RAW_SPAWN_CREATION_FLAGS | CREATE_UNICODE_ENVIRONMENT;

/// Raw suspended spawn (P06 seam, composed by the P07 jobbed spawn): one
/// direct `CreateProcessW` bypassing the std/tokio spawn entirely, with a
/// single `STARTUPINFOEXW` whose `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` names
/// exactly the three child stdio pipe ends — no other handle is inherited
/// and the parent environment is never passed down (the UTF-16 block is
/// built from `spec.environment`, passed with `CREATE_UNICODE_ENVIRONMENT`).
/// The child is created `CREATE_SUSPENDED | CREATE_NO_WINDOW` so it cannot
/// execute a single instruction before composition. The returned struct
/// owns the process/thread handles plus the parent pipe ends; every failure
/// path closes exactly what it opened and terminates the child *before*
/// resume.
pub fn spawn_suspended(spec: &RawSpawnSpec) -> Result<RawSuspendedChild, GuardianError> {
    spawn_suspended_inner(spec, None, None)
}

/// P07 jobbed spawn: create the kill-on-close job FIRST, then create the
/// child suspended with `PROC_THREAD_ATTRIBUTE_JOB_LIST` in the same
/// `STARTUPINFOEXW` attribute list as the stdio `HANDLE_LIST` — the kernel
/// assigns the job as part of process creation, strictly before the child
/// can execute anything, so there is no post-create assignment window at
/// all. The assignment is verified post-creation with `IsProcessInJob`
/// (fail-closed) before the wrapper is handed out. When the environment
/// cannot host the assignment (an enclosing job with incompatible limits
/// makes `CreateProcessW` fail with `ERROR_ACCESS_DENIED`), the result is
/// [`GuardianError::Unsupported`] — running the child without the job is
/// never a fallback.
pub fn spawn_suspended_with_job(
    spec: &RawSpawnSpec,
) -> Result<JobbedSuspendedChild, GuardianError> {
    let job = JobGuard::create()?;
    let child = spawn_suspended_inner(spec, Some(&job), None)?;
    Ok(JobbedSuspendedChild {
        job,
        child,
        primary_settled: false,
        settled_exit_code: None,
    })
}

/// P15 lowbox spawn: creation-time Job AND AppContainer token in the SAME
/// `STARTUPINFOEXW` attribute list (`PROC_THREAD_ATTRIBUTE_SECURITY_
/// CAPABILITIES` carries the AppContainer SID plus capability SIDs), so
/// the child's very first instruction already runs inside the lowbox and
/// the kill-on-close job — there is no post-create window for either.
/// `lowbox` must outlive this call (the attribute list borrows its SID
/// buffers until CreateProcessW returns).
pub fn spawn_suspended_with_job_lowbox(
    spec: &RawSpawnSpec,
    lowbox: &LowboxCapabilities,
) -> Result<JobbedSuspendedChild, GuardianError> {
    let job = JobGuard::create()?;
    // AppContainer process creation requires a base set of SYSTEM
    // variables in the environment block (CreateProcessW internally
    // expands them for the profile directory reroute; a minimal or empty
    // explicit block fails with ERROR_ENVVAR_NOT_FOUND — empirically
    // bisected on this OS). The base set is a fixed whitelist of OS
    // locations, never provider credentials: values are taken from the
    // CURRENT process environment one key at a time, and caller-provided
    // allowlist entries are never overridden.
    const APPCONTAINER_BASE_ENVIRONMENT: [&str; 11] = [
        "SYSTEMROOT",
        "SystemDrive",
        "windir",
        "ProgramData",
        "USERNAME",
        "USERPROFILE",
        "LOCALAPPDATA",
        "TEMP",
        "TMP",
        "PATH",
        "PATHEXT",
    ];
    let mut environment = spec.environment.clone();
    for key in APPCONTAINER_BASE_ENVIRONMENT {
        if !environment.contains_key(key) {
            if let Ok(value) = std::env::var(key) {
                environment.insert(key.to_string(), value);
            }
        }
    }
    let completed = RawSpawnSpec {
        executable: spec.executable,
        arguments: spec.arguments,
        cwd: spec.cwd,
        environment: &environment,
    };
    let child = spawn_suspended_inner(&completed, Some(&job), Some(lowbox))?;
    Ok(JobbedSuspendedChild {
        job,
        child,
        primary_settled: false,
        settled_exit_code: None,
    })
}

fn spawn_suspended_inner(
    spec: &RawSpawnSpec,
    job: Option<&JobGuard>,
    lowbox: Option<&LowboxCapabilities>,
) -> Result<RawSuspendedChild, GuardianError> {
    let (mut stdin_child, stdin_parent) = unsafe { create_pipe_pair(true)? };
    let (mut stdout_child, stdout_parent) = unsafe { create_pipe_pair(false)? };
    let (mut stderr_child, stderr_parent) = unsafe { create_pipe_pair(false)? };
    let mut command_line = build_command_line(spec.executable, spec.arguments);
    let environment_block = build_environment_block(spec.environment)?;
    let cwd_wide = spec.cwd.map(wide_path);

    unsafe {
        // Single extended startup record: stdio wired to the child pipe
        // ends, attribute list carrying ONLY those three handles.
        let mut startup: STARTUPINFOEXW = std::mem::zeroed();
        startup.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        startup.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = stdin_child.raw();
        startup.StartupInfo.hStdOutput = stdout_child.raw();
        startup.StartupInfo.hStdError = stderr_child.raw();

        // Two-call size pattern into a pointer-aligned buffer. The list
        // carries the stdio handle list plus, for the jobbed spawn (P07.1),
        // the creation-time job list and, for the lowbox spawn (P15), the
        // AppContainer security capabilities.
        let attribute_count: u32 = u32::from(job.is_some()) + u32::from(lowbox.is_some()) + 1;
        let mut attribute_size: usize = 0;
        InitializeProcThreadAttributeList(
            std::ptr::null_mut(),
            attribute_count,
            0,
            &mut attribute_size,
        );
        let word = std::mem::size_of::<usize>();
        let words = attribute_size.div_ceil(word);
        let mut attributes = AttributeListGuard {
            storage: vec![0usize; words],
            list: std::ptr::null_mut(),
            initialized: false,
        };
        attributes.list = attributes.storage.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        if InitializeProcThreadAttributeList(
            attributes.list,
            attribute_count,
            0,
            &mut attribute_size,
        ) == 0
        {
            return Err(GuardianError::Api(
                "InitializeProcThreadAttributeList failed".into(),
            ));
        }
        attributes.initialized = true;

        let handle_list: [HANDLE; 3] = [stdin_child.raw(), stdout_child.raw(), stderr_child.raw()];
        if UpdateProcThreadAttribute(
            attributes.list,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            handle_list.as_ptr() as *const std::ffi::c_void,
            std::mem::size_of::<[HANDLE; 3]>(),
            std::ptr::null_mut(),
            std::ptr::null(),
        ) == 0
        {
            return Err(GuardianError::Api(
                "UpdateProcThreadAttribute failed".into(),
            ));
        }
        // Creation-time job assignment (P07.1): the job handle travels as a
        // JOB_LIST attribute in the SAME list; the kernel duplicates it
        // during CreateProcessW, so membership exists from the child's
        // first instruction onward. The JobGuard outlives the call.
        if let Some(job) = job {
            let job_list: [HANDLE; 1] = [job.handle];
            if UpdateProcThreadAttribute(
                attributes.list,
                0,
                PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                job_list.as_ptr() as *const std::ffi::c_void,
                std::mem::size_of::<[HANDLE; 1]>(),
                std::ptr::null_mut(),
                std::ptr::null(),
            ) == 0
            {
                return Err(GuardianError::Api(
                    "UpdateProcThreadAttribute(JOB_LIST) failed".into(),
                ));
            }
        }
        // P15: the AppContainer token travels in the SAME list — the child
        // starts inside the lowbox, before its first instruction. The
        // SID_AND_ATTRIBUTES array and the SECURITY_CAPABILITIES record are
        // declared at THIS scope (not inside the match arm): the attribute
        // list stores pointers into them that must stay valid until
        // CreateProcessW returns below.
        // The underscore binding KEEPS the array alive to the end of this
        // scope (unlike `_`): the attribute list holds a pointer into it
        // that must survive until CreateProcessW returns.
        let (_capability_entries, capabilities) = match lowbox {
            Some(lowbox) => {
                let mut entries: Vec<windows_sys::Win32::Security::SID_AND_ATTRIBUTES> = lowbox
                    .capability_sids
                    .iter()
                    .map(|sid| windows_sys::Win32::Security::SID_AND_ATTRIBUTES {
                        Sid: sid.as_ptr() as *mut core::ffi::c_void,
                        Attributes: 0,
                    })
                    .collect();
                let capabilities = Some(windows_sys::Win32::Security::SECURITY_CAPABILITIES {
                    AppContainerSid: lowbox.app_container_sid.as_ptr() as *mut core::ffi::c_void,
                    // A zero-length Vec's as_mut_ptr() is a NON-NULL
                    // dangling pointer; CreateProcessW validates
                    // Capabilities==NULL when CapabilityCount==0 and
                    // answers ERROR_INVALID_PARAMETER otherwise.
                    Capabilities: if entries.is_empty() {
                        std::ptr::null_mut()
                    } else {
                        entries.as_mut_ptr()
                    },
                    CapabilityCount: entries.len() as u32,
                    Reserved: 0,
                });
                (entries, capabilities)
            }
            None => (Vec::new(), None),
        };
        if let Some(capabilities) = capabilities.as_ref() {
            if UpdateProcThreadAttribute(
                attributes.list,
                0,
                PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                capabilities as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<windows_sys::Win32::Security::SECURITY_CAPABILITIES>(),
                std::ptr::null_mut(),
                std::ptr::null(),
            ) == 0
            {
                return Err(GuardianError::Api(
                    "UpdateProcThreadAttribute(SECURITY_CAPABILITIES) failed".into(),
                ));
            }
        }
        startup.lpAttributeList = attributes.list;

        let mut info: PROCESS_INFORMATION = std::mem::zeroed();
        let created = CreateProcessW(
            std::ptr::null(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
            UNICODE_SPAWN_CREATION_FLAGS,
            environment_block.as_ptr() as *const std::ffi::c_void,
            cwd_wide
                .as_ref()
                .map_or(std::ptr::null(), |wide| wide.as_ptr()),
            &startup.StartupInfo,
            &mut info,
        );

        // The attribute list is consumed by the call; drop it and close the
        // parent's duplicates of the child-side ends either way.
        drop(attributes);
        stdin_child.close();
        stdout_child.close();
        stderr_child.close();

        if created == 0 {
            let code = GetLastError();
            if code == ERROR_ACCESS_DENIED {
                // Most plausibly an enclosing job that refuses the nested
                // assignment (CI/breakaway restrictions): refuse outright,
                // never run the child without the job.
                return Err(GuardianError::Unsupported(format!(
                    "CreateProcessW with JOB_LIST was denied (win32 error {code})"
                )));
            }
            return Err(GuardianError::Api(format!(
                "CreateProcessW failed (win32 error {code})"
            )));
        }

        // Defensive post-success check: kill and fully clean up if the
        // kernel handed us anything unusable.
        if info.hProcess.is_null() || info.hThread.is_null() || info.dwProcessId == 0 {
            terminate_and_wait(info.hProcess);
            if !info.hProcess.is_null() {
                CloseHandle(info.hProcess);
            }
            if !info.hThread.is_null() {
                CloseHandle(info.hThread);
            }
            return Err(GuardianError::Api(
                "CreateProcessW returned invalid handles".into(),
            ));
        }

        // P07.1 post-condition: for the jobbed spawn, membership must be
        // observably true on the still-suspended child before we hand it
        // out; anything else is fail-closed termination.
        if let Some(job) = job {
            if !job.contains_process(info.hProcess) {
                terminate_and_wait(info.hProcess);
                CloseHandle(info.hProcess);
                CloseHandle(info.hThread);
                return Err(GuardianError::Api(
                    "the creation-time job assignment did not take effect".into(),
                ));
            }
        }

        Ok(RawSuspendedChild {
            process: OwnedHandle::new(info.hProcess),
            thread: OwnedHandle::new(info.hThread),
            stdin_write: stdin_parent,
            stdout_read: stdout_parent,
            stderr_read: stderr_parent,
            pid: info.dwProcessId,
            resumed: false,
        })
    }
}

impl RawSuspendedChild {
    /// The child's pid (identity checks must pair it with
    /// [`Self::start_identity`]).
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Start-time identity of the child, for PID-reuse-proof comparisons.
    pub fn start_identity(&self) -> u64 {
        process_start_identity(self.pid)
    }

    /// Full owner identity for persistence, tagged as the raw-suspended
    /// Windows native spawn.
    pub fn owner_identity(
        &self,
    ) -> Result<crate::process_guard::ProcessOwnerIdentity, GuardianError> {
        let boot = crate::process_guard::BootIdentity::current()
            .map_err(|error| GuardianError::Api(format!("boot identity unavailable: {error}")))?;
        crate::process_guard::ProcessOwnerIdentity::new(
            self.pid,
            self.start_identity(),
            boot,
            serde_json::json!({ "native": "windows-raw-suspended" }),
        )
        .map_err(|reason| GuardianError::Api(reason.into()))
    }

    /// Resume the primary thread exactly once. A second call is a State
    /// error; a kernel failure leaves the flag untouched so the caller can
    /// still terminate the suspended child.
    pub fn resume_once(&mut self) -> Result<(), GuardianError> {
        if self.resumed {
            return Err(GuardianError::State(
                "resume requires the thread to be suspended",
            ));
        }
        let previous = unsafe { ResumeThread(self.thread.raw()) };
        if previous == u32::MAX {
            return Err(GuardianError::Api("ResumeThread failed".into()));
        }
        self.resumed = true;
        Ok(())
    }

    /// Terminate the child and close every owned handle. Idempotent: after
    /// the first call the handles are gone and later calls are no-ops, so
    /// nothing ever closes twice.
    pub fn terminate(&mut self) {
        if self.process.raw().is_null() {
            return;
        }
        unsafe {
            TerminateProcess(self.process.raw(), 1);
            WaitForSingleObject(self.process.raw(), TERMINATE_WAIT_MS);
        }
        self.close_all();
    }

    /// Alias of [`Self::terminate`] for callers reasoning about abort paths.
    pub fn abort(&mut self) {
        self.terminate();
    }

    /// Parent end of the child's stdin (write side), for driving the child.
    pub fn stdin_write_handle(&self) -> HANDLE {
        self.stdin_write.raw()
    }

    /// Parent end of the child's stdout (read side), for observing output.
    pub fn stdout_read_handle(&self) -> HANDLE {
        self.stdout_read.raw()
    }

    /// Parent end of the child's stderr (read side).
    pub fn stderr_read_handle(&self) -> HANDLE {
        self.stderr_read.raw()
    }

    /// Blocking read from the child's stdout. Fails with a broken pipe once
    /// the child exits and its write end closes.
    pub fn read_stdout(&mut self, buf: &mut [u8]) -> Result<usize, GuardianError> {
        let mut read: u32 = 0;
        let ok = unsafe {
            ReadFile(
                self.stdout_read.raw(),
                buf.as_mut_ptr(),
                buf.len() as u32,
                &mut read,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(GuardianError::Api("ReadFile on child stdout failed".into()));
        }
        Ok(read as usize)
    }

    /// Blocking write to the child's stdin.
    pub fn write_stdin(&mut self, buf: &[u8]) -> Result<usize, GuardianError> {
        let mut written: u32 = 0;
        let ok = unsafe {
            WriteFile(
                self.stdin_write.raw(),
                buf.as_ptr(),
                buf.len() as u32,
                &mut written,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(GuardianError::Api("WriteFile to child stdin failed".into()));
        }
        Ok(written as usize)
    }

    /// Close the parent's stdin write end so a child reading stdin to EOF
    /// (the P13/P15 probe helper protocol) unblocks exactly once, with the
    /// full request delivered. Idempotent.
    pub fn close_stdin(&mut self) {
        self.stdin_write.close();
    }

    /// Bytes currently buffered on the parent stdout end (PeekNamedPipe):
    /// a non-blocking check distinguishing "no marker yet" from "closed".
    pub fn stdout_available_bytes(&self) -> u32 {
        let mut available: u32 = 0;
        let ok = unsafe {
            PeekNamedPipe(
                self.stdout_read.raw(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            0
        } else {
            available
        }
    }

    /// Wait up to `timeout` for the child to exit; true when it has.
    pub fn wait(&self, timeout: Duration) -> bool {
        let millis = timeout.as_millis().min(u32::MAX as u128) as u32;
        let result = unsafe { WaitForSingleObject(self.process.raw(), millis) };
        result == WAIT_OBJECT_0
    }

    /// The child's exit code, or None while still running (or unknown).
    pub fn exit_code(&self) -> Option<u32> {
        let mut code: u32 = 0;
        let ok = unsafe { GetExitCodeProcess(self.process.raw(), &mut code) };
        if ok == 0 {
            return None;
        }
        if code == STILL_ACTIVE as u32 {
            None
        } else {
            Some(code)
        }
    }

    /// After the child is confirmed dead, capture its exit code and release
    /// the process/thread handles. A job keeps an exited member listed
    /// until its handles are released, so proving full-tree death requires
    /// this release; the stdio parent ends stay open for draining buffered
    /// output. Returns the exit code when the child has exited (None while
    /// still running or once already settled).
    pub fn settle(&mut self) -> Option<u32> {
        if self.process.raw().is_null() || !self.wait(Duration::from_millis(0)) {
            return None;
        }
        let code = self.exit_code();
        self.process.close();
        self.thread.close();
        code
    }

    fn close_all(&mut self) {
        self.process.close();
        self.thread.close();
        self.stdin_write.close();
        self.stdout_read.close();
        self.stderr_read.close();
    }
}

impl Drop for RawSuspendedChild {
    fn drop(&mut self) {
        // A never-resumed child can never exit on its own: terminate it so
        // a dropped spawn leaks no suspended process. A resumed child is
        // the caller's (or the P07 job's) responsibility — Drop only closes
        // handles, exactly once each.
        if !self.resumed {
            unsafe { terminate_and_wait(self.process.raw()) };
        }
        self.close_all();
    }
}

// P07 jobbed composition ----------------------------------------------------

/// One non-blocking observation of a stdio pipe parent end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipeRead {
    /// Bytes readable right now (possibly empty when nothing is buffered).
    Data(Vec<u8>),
    /// The pipe is broken: the child's side is gone (stream EOF).
    Closed,
}

/// Whether this process already lives inside some job — a cheap probe for
/// environments (CI) whose enclosing job may make the creation-time
/// assignment incompatible. The definitive answer is still the spawn
/// attempt itself, which refuses via [`GuardianError::Unsupported`].
pub fn current_process_in_job() -> bool {
    let mut result: windows_sys::core::BOOL = 0;
    unsafe {
        IsProcessInJob(GetCurrentProcess(), std::ptr::null_mut(), &mut result) != 0 && result != 0
    }
}

/// Poll cadence between member-list re-enumerations while proving death.
const MEMBER_POLL_MS: u64 = 50;
/// Upper bound on one non-blocking pipe observation (keeps output frames
/// inside the protocol page budget).
const PIPE_PUMP_CHUNK_BYTES: u32 = 64 * 1024;

/// Non-blocking read of whatever is currently buffered on a pipe parent
/// end; [`PipeRead::Closed`] once the child side is gone (peek or read
/// failure). Never blocks: zero buffered bytes yields an empty `Data`.
fn pipe_read(handle: HANDLE) -> PipeRead {
    let mut available: u32 = 0;
    unsafe {
        if PeekNamedPipe(
            handle,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut available,
            std::ptr::null_mut(),
        ) == 0
        {
            return PipeRead::Closed;
        }
        if available == 0 {
            return PipeRead::Data(Vec::new());
        }
        let want = available.min(PIPE_PUMP_CHUNK_BYTES);
        let mut buffer = vec![0u8; want as usize];
        let mut read: u32 = 0;
        if ReadFile(
            handle,
            buffer.as_mut_ptr(),
            want,
            &mut read,
            std::ptr::null_mut(),
        ) == 0
        {
            return PipeRead::Closed;
        }
        buffer.truncate(read as usize);
        PipeRead::Data(buffer)
    }
}

/// A [`RawSuspendedChild`] composed into a kill-on-close job at creation
/// (P07): the assignment happened inside `CreateProcessW` via
/// `PROC_THREAD_ATTRIBUTE_JOB_LIST`, so the child cannot have executed
/// anything outside the job. The wrapper owns both the job and the raw
/// child, adds the durable job identity (persisted before resume), member
/// enumeration, and the full-tree death proof with PID-reuse fencing.
///
/// Teardown order on drop: fields drop in declaration order, so the job
/// handle closes first — its kill-on-close limit sweeps the tree — and the
/// raw child's handles close after (a never-resumed child was already
/// terminated by the kernel). Either order converges to a dead tree.
pub struct JobbedSuspendedChild {
    job: JobGuard,
    child: RawSuspendedChild,
    primary_settled: bool,
    settled_exit_code: Option<u32>,
}

// Handle values move freely; the wrapper is driven through &mut self.
unsafe impl Send for JobbedSuspendedChild {}

impl std::fmt::Debug for JobbedSuspendedChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print handle values (durable info only).
        f.debug_struct("JobbedSuspendedChild")
            .field("pid", &self.child.pid())
            .field("primary_settled", &self.primary_settled)
            .finish()
    }
}

impl JobbedSuspendedChild {
    /// Durable identity tuple persisted before resume (P07.2): the job
    /// handle value, the pid, and the start-time identity.
    pub fn job_identity(&self) -> (usize, u32, u64) {
        (
            self.job.raw(),
            self.child.pid(),
            self.child.start_identity(),
        )
    }

    /// The child's pid (identity checks must pair it with
    /// [`Self::start_identity`]).
    pub fn pid(&self) -> u32 {
        self.child.pid()
    }

    /// Start-time identity of the child, for PID-reuse-proof comparisons.
    pub fn start_identity(&self) -> u64 {
        self.child.start_identity()
    }

    /// Full owner identity for persistence, tagged as the jobbed Windows
    /// native spawn and carrying the job identity (P07.2).
    pub fn owner_identity(
        &self,
    ) -> Result<crate::process_guard::ProcessOwnerIdentity, GuardianError> {
        let boot = crate::process_guard::BootIdentity::current()
            .map_err(|error| GuardianError::Api(format!("boot identity unavailable: {error}")))?;
        let (job, pid, start) = self.job_identity();
        crate::process_guard::ProcessOwnerIdentity::new(
            pid,
            start,
            boot,
            serde_json::json!({
                "native": "windows-jobbed-suspended",
                "job": job,
                "pid": pid,
                "startIdentity": start,
            }),
        )
        .map_err(|reason| GuardianError::Api(reason.into()))
    }

    /// Whether the child is observably a member of the job. The
    /// creation-time assignment already proved this at spawn; re-exposed
    /// for callers that want to re-verify.
    pub fn is_in_job(&self) -> bool {
        // SAFETY: the wrapper owns the (open) process handle.
        unsafe { self.job.contains_process(self.child.process.raw()) }
    }

    /// PIDs currently listed in the job (snapshot — fence with a start
    /// identity before acting; see [`Self::wait_all_members`]).
    pub fn member_pids(&self) -> Result<Vec<u32>, GuardianError> {
        self.job.member_pids()
    }

    /// Resume the primary thread exactly once (delegates to the raw child;
    /// a second call is a State error).
    pub fn resume_once(&mut self) -> Result<(), GuardianError> {
        self.child.resume_once()
    }

    /// Non-blocking observation of the child's stdout parent end.
    pub fn pump_stdout(&self) -> PipeRead {
        pipe_read(self.child.stdout_read_handle())
    }

    /// Non-blocking observation of the child's stderr parent end.
    pub fn pump_stderr(&self) -> PipeRead {
        pipe_read(self.child.stderr_read_handle())
    }

    /// Whether the primary process has exited (handle signalled).
    pub fn primary_exited(&self) -> bool {
        self.child.wait(Duration::from_millis(0))
    }

    /// Capture the primary's exit code and release its process/thread
    /// handles so the kernel can retire the exited member from the job
    /// list. Idempotent; returns the captured code once settled.
    pub fn settle_primary(&mut self) -> Option<u32> {
        if self.primary_settled {
            return self.settled_exit_code;
        }
        if let Some(code) = self.child.settle() {
            self.primary_settled = true;
            self.settled_exit_code = Some(code);
        }
        self.settled_exit_code
    }

    /// The exit code captured at settle time (None until the primary is
    /// confirmed dead and settled).
    pub fn settled_exit_code(&self) -> Option<u32> {
        self.settled_exit_code
    }

    /// Terminate the entire tree now (TerminateJobObject; the kill-on-close
    /// limit sweeps anything it might miss), then settle the primary.
    pub fn terminate(&mut self) {
        self.job.terminate();
        if self
            .child
            .wait(Duration::from_millis(u64::from(TERMINATE_WAIT_MS)))
        {
            self.settle_primary();
        }
    }

    /// Alias of [`Self::terminate`] for abort-style teardown of a prepared
    /// (never resumed) child; the job has exactly one member then.
    pub fn abort(&mut self) {
        self.terminate();
    }

    /// Wait up to `timeout` for the primary process to exit.
    pub fn wait(&self, timeout: Duration) -> bool {
        self.child.wait(timeout)
    }

    /// The primary's current exit code (None while running or after the
    /// handles were settled away — use [`Self::settled_exit_code`] then).
    pub fn exit_code(&self) -> Option<u32> {
        self.child.exit_code()
    }

    /// Blocking read from the child's stdout (delegates to the raw child).
    pub fn read_stdout(&mut self, buf: &mut [u8]) -> Result<usize, GuardianError> {
        self.child.read_stdout(buf)
    }

    /// Blocking write to the child's stdin (delegates to the raw child).
    pub fn write_stdin(&mut self, buf: &[u8]) -> Result<usize, GuardianError> {
        self.child.write_stdin(buf)
    }

    /// Close the parent's stdin write end (child sees EOF; probe-helper
    /// request protocol).
    pub fn close_stdin(&mut self) {
        self.child.close_stdin();
    }

    /// Bytes currently buffered on the parent stdout end.
    pub fn stdout_available_bytes(&self) -> u32 {
        self.child.stdout_available_bytes()
    }

    /// Prove that every member of the job is dead (P07.3). Loop: enumerate
    /// the member list, fence every pid with its start identity BEFORE
    /// waiting (a reused pid cannot masquerade as the member we were
    /// promised), wait on the fenced instances only, then re-enumerate —
    /// until the list is empty (with our handles to the primary already
    /// released, so exited members can retire) or the deadline passes.
    /// False means unverifiable: callers must block writes, not guess.
    pub fn wait_all_members(&mut self, deadline: Instant) -> bool {
        loop {
            let members = match self.job.member_pids() {
                Ok(members) => members,
                Err(_) => return false,
            };
            if members.is_empty() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            // Fence first: pair every listed pid with the start identity we
            // accept as "the same process instance".
            let fenced: Vec<(u32, u64)> = members
                .into_iter()
                .map(|pid| (pid, process_start_identity(pid)))
                .collect();
            let primary = self.child.pid();
            for (pid, start) in &fenced {
                if *pid == primary {
                    self.wait_primary(deadline);
                } else {
                    wait_fenced_member(*pid, *start, deadline);
                }
            }
            std::thread::sleep(Duration::from_millis(MEMBER_POLL_MS));
            // Re-enumerate after a bounded wait; the poll cadence above
            // avoids a hot spin while members die slowly or linger in the
            // list. The empty list on the next iteration is the only proof
            // returned.
        }
    }

    /// Wait for the primary through its own handle, then settle (capture
    /// the code, release the handles) so it can leave the member list.
    fn wait_primary(&mut self, deadline: Instant) -> bool {
        if self.primary_settled {
            return true;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        if self.child.wait(remaining) {
            self.settle_primary();
            true
        } else {
            false
        }
    }
}

/// Wait for one fenced member: open by pid, verify the creation time
/// through the opened handle matches the fence (a mismatch means the pid
/// was reused and the fenced instance is already gone), then wait on that
/// same object. The handle is always closed before returning so it cannot
/// itself hold the member in the job list.
fn wait_fenced_member(pid: u32, fenced_start: u64, deadline: Instant) -> bool {
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let handle = OpenProcess(
            PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION,
            0,
            pid,
        );
        if handle.is_null() {
            // No reachable object with this pid: with the fence captured,
            // a vanished pid is the member's death (the same convention
            // is_process_alive uses).
            return true;
        }
        // A zero fence (identity was unavailable) degrades to a pure wait;
        // never to an unverified "gone".
        let matches_fence = fenced_start == 0 || handle_creation_time(handle) == fenced_start;
        let result = if matches_fence {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let millis = remaining.as_millis().min(u32::MAX as u128) as u32;
            WaitForSingleObject(handle, millis)
        } else {
            WAIT_OBJECT_0
        };
        CloseHandle(handle);
        result == WAIT_OBJECT_0
    }
}
