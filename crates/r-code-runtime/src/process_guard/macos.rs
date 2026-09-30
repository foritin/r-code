//! macOS guardian-as-spawner (P10): a guardian launch gate plus a
//! `PROC_PIDTBSDINFO` birth tuple (pid/ppid/start seconds/useconds) —
//! never `/proc` (absent on macOS; the PRD's sysctl/kinfo_proc fallback is
//! unnecessary because libc 0.2.189 binds `proc_pidinfo` directly, so no
//! hand-computed struct offsets exist anywhere here). The architecture
//! mirrors P08's `unix.rs`: the guardian exists FIRST, creates the workload
//! behind a release gate, and reports identity over the same versioned
//! control protocol (`guardian_protocol`, reused verbatim); the daemon
//! persists the identity before opening the gate. macOS-native deviations
//! (stop gate, descriptor discipline, in-process hosting) are documented at
//! [`spawn_stopped_workload_macos`], [`macos_spawn_via_guardian`] and
//! [`serve_macos_guardian`].

#![cfg(target_os = "macos")]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use crate::process_guard::unix::guardian_protocol::{
    self, FrameErrorCode, GuardianFrame, PROTOCOL_VERSION,
};
use crate::process_guard::unix::{
    group_alive, GuardianError, GuardianIdentity, GuardianSpawnRequest, GUARDIAN_EXIT_DAEMON_EOF,
    GUARDIAN_EXIT_PROTOCOL, GUARDIAN_EXIT_RELEASED,
};
use crate::process_guard::ProcessOwnerIdentity;

// P10.1 — birth identity via PROC_PIDTBSDINFO --------------------------------

/// XNU `bsd/sys/proc_info.h` `struct proc_bsdinfo`, the payload of
/// `proc_pidinfo(_, PROC_PIDTBSDINFO, _)`. The layout is a stable kernel
/// ABI (libproc consumers such as `ps` read it); `repr(C)` derives the
/// offsets and the size assertion pins the declaration to the documented
/// 136-byte form (MAXCOMLEN = 16), so a platform drift fails at compile
/// time instead of misreading a birth tuple.
#[repr(C)]
struct ProcBsdInfo {
    pbi_flags: u32,        // 0
    pbi_status: u32,       // 4
    pbi_xstatus: u32,      // 8
    pbi_pid: u32,          // 12
    pbi_ppid: u32,         // 16
    pbi_uid: u32,          // 20 (uid_t/gid_t are u32 on Darwin)
    pbi_gid: u32,          // 24
    pbi_ruid: u32,         // 28
    pbi_rgid: u32,         // 32
    pbi_svuid: u32,        // 36
    pbi_svgid: u32,        // 40
    rfu_1: u32,            // 44 (reserved)
    pbi_comm: [u8; 16],    // 48 (MAXCOMLEN)
    pbi_name: [u8; 32],    // 64 (2 * MAXCOMLEN)
    pbi_nfiles: u32,       // 96
    pbi_pgid: u32,         // 100
    pbi_pjobc: u32,        // 104
    e_tdev: u32,           // 108
    e_tpgid: u32,          // 112
    pbi_nice: u32,         // 116
    pbi_start_tvsec: u64,  // 120
    pbi_start_tvusec: u64, // 128 (total 136)
}

const _: () = assert!(std::mem::size_of::<ProcBsdInfo>() == 136);

/// Birth identity of one process instance (P10.1): the pid, the parent pid
/// and the start-time tuple read from `PROC_PIDTBSDINFO`. Any two distinct
/// live process instances — including a recycled pid — differ in at least
/// one component; the start tuple is the pid-reuse fence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct MacosBirthIdentity {
    pub pid: u32,
    pub ppid: u32,
    pub start_seconds: u64,
    pub start_useconds: u64,
}

impl MacosBirthIdentity {
    /// 64-bit start identity: `(seconds << 32) | microseconds`. The
    /// microsecond component is always < 1_000_000 (< 2^32), so the
    /// encoding is injective. The same value is carried as
    /// `ProcessOwnerIdentity::start_identity` and as the P08 wire
    /// protocol's `birth_start_identity`, so producer and consumer agree
    /// by construction.
    pub fn start_identity(&self) -> u64 {
        (self.start_seconds << 32) | u64::from(self.start_useconds)
    }

    /// Whether the tuple can identify a real child process. `start_seconds`
    /// is epoch time and never zero for a spawned process;
    /// `start_useconds` MAY legitimately be zero and is not required.
    pub fn is_complete(&self) -> bool {
        self.pid != 0 && self.ppid != 0 && self.start_seconds != 0
    }
}

/// Read the birth identity of `pid` WITHOUT any /proc dependency (P10.1):
/// one `proc_pidinfo(pid, PROC_PIDTBSDINFO, ...)` call. Fail-closed: a
/// partial or failed read, a pid mismatch (recycled-pid race) or an
/// incomplete tuple yields None — an unreadable identity is never guessed.
pub fn macos_process_birth_identity(pid: u32) -> Option<MacosBirthIdentity> {
    if pid == 0 {
        // Pid 0 is the kernel; it is never a workload and never an owner.
        return None;
    }
    let mut info = std::mem::MaybeUninit::<ProcBsdInfo>::zeroed();
    let expected = std::mem::size_of::<ProcBsdInfo>() as libc::c_int;
    // SAFETY: `info` has room for exactly `expected` bytes and proc_pidinfo
    // writes at most that much; the return value is the byte count written.
    let returned = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            expected,
        )
    };
    if returned != expected {
        // A short read (vanished or zombie pid, kernel entry, EPERM) is
        // unverifiable, never a zero identity.
        return None;
    }
    // SAFETY: proc_pidinfo reported the full documented size; the payload
    // is plain data whose layout is pinned by the size assertion above.
    let info = unsafe { info.assume_init() };
    let birth = MacosBirthIdentity {
        pid: info.pbi_pid,
        ppid: info.pbi_ppid,
        start_seconds: info.pbi_start_tvsec,
        start_useconds: info.pbi_start_tvusec,
    };
    if birth.pid != pid || !birth.is_complete() {
        return None;
    }
    Some(birth)
}

// P10.3 — persisted owner identity -------------------------------------------

/// Full owner identity of a gated macOS workload for persistence BEFORE the
/// release gate opens (P10.3): pid + start identity + boot identity plus
/// the platform birth tuple, so recovery verifies a pid's instance without
/// /proc and without trusting pid reuse.
pub fn macos_owner_identity(pid: u32) -> Result<ProcessOwnerIdentity, GuardianError> {
    let birth = macos_process_birth_identity(pid).ok_or_else(|| {
        GuardianError::Io("macOS birth identity (PROC_PIDTBSDINFO) is unreadable".into())
    })?;
    let boot = crate::process_guard::BootIdentity::current()
        .map_err(|error| GuardianError::Io(format!("boot identity unavailable: {error}")))?;
    ProcessOwnerIdentity::new(
        pid,
        birth.start_identity(),
        boot,
        serde_json::json!({
            "native": "macos-gated",
            "ppid": birth.ppid,
            "startSeconds": birth.start_seconds,
            "startUseconds": birth.start_useconds,
        }),
    )
    .map_err(|reason| GuardianError::Io(format!("process owner identity is incomplete: {reason}")))
}

// P11 — descendant diagnostics (never containment proof) ----------------------

/// Snapshot of the process table (P11.1): pids from one `proc_listallpids`
/// call plus per-pid `PROC_PIDTBSDINFO` birth tuples, so every entry is
/// identity-checked by construction. Pids whose identity is unreadable
/// (vanished, EPERM) are omitted — enumeration is best-effort BY DESIGN,
/// which is exactly why it can never support a containment claim or feed an
/// activation predicate (P12/P13 own activation).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MacosProcessSnapshot {
    entries: BTreeMap<u32, MacosBirthIdentity>,
}

impl MacosProcessSnapshot {
    pub fn get(&self, pid: u32) -> Option<&MacosBirthIdentity> {
        self.entries.get(&pid)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Enumerate the process table for diagnostics (P11.1). The pid list comes
/// from `proc_listallpids` — a plain `pid_t` array, so NO kernel process
/// struct layout is parsed anywhere (the sysctl all-procs table walk would
/// require hand-computed offsets over a struct the libc 0.2.189 apple
/// bindings do not even define). Library note: `proc_listallpids` is a
/// stable libproc API (macOS 10.7+) bound by libc. Fail-closed: a failed
/// listing yields None, never an empty snapshot that could masquerade as
/// "no descendants".
pub fn macos_snapshot_processes() -> Option<MacosProcessSnapshot> {
    let mut capacity = 256usize;
    loop {
        let mut buffer = vec![0i32; capacity];
        // SAFETY: `buffer` holds `capacity` pid_t values and
        // proc_listallpids writes at most that many; the return value is the
        // pid count (a bigger count means the table grew: retry larger).
        let count = unsafe {
            libc::proc_listallpids(
                buffer.as_mut_ptr().cast(),
                (buffer.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int,
            )
        };
        if count < 0 {
            return None;
        }
        let count = count as usize;
        if count > capacity {
            capacity = count + 64;
            continue;
        }
        let mut entries = BTreeMap::new();
        for pid in buffer.into_iter().take(count) {
            if pid <= 1 {
                // Pid 0 is the kernel and pid 1 is launchd: neither is ever
                // a workload descendant.
                continue;
            }
            if let Some(birth) = macos_process_birth_identity(pid as u32) {
                entries.insert(pid as u32, birth);
            }
        }
        return Some(MacosProcessSnapshot { entries });
    }
}

/// Identity-checked strict descendants of `root` in `snapshot` (P11.1,
/// pure). The walk follows only ppid edges whose entry is complete and whose
/// pid matches its map key; anything inconsistent is a wall, never a guess.
fn reachable_descendants(
    snapshot: &MacosProcessSnapshot,
    root: &MacosBirthIdentity,
) -> BTreeMap<u32, MacosBirthIdentity> {
    let mut members = BTreeMap::new();
    let mut frontier = vec![*root];
    while let Some(current) = frontier.pop() {
        for (&pid, entry) in snapshot.entries.iter() {
            if members.contains_key(&pid) {
                continue;
            }
            if entry.ppid == current.pid && entry.pid == pid && entry.is_complete() {
                members.insert(pid, *entry);
                frontier.push(*entry);
            }
        }
    }
    members
}

/// P11.2 classification — the adversarial verdict. `known_member_pids` are
/// STRICT descendants the caller knows about (workload self-report); each
/// must be reachable from the root in the identity-checked snapshot.
/// DIAGNOSTIC ONLY: `SnapshotReachable` means "was found in one
/// enumeration", NEVER kernel containment — a setsid/double-fork escapee is
/// invisible to ppid edges the moment it re-parents, which is precisely why
/// write execution stays SafeDisabled (P11.3) regardless of this verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MacosContainmentVerdict {
    /// Every known member was reachable and identity-checked. Still not a
    /// containment claim; never feeds activation.
    SnapshotReachable { members: usize },
    /// Known members are absent from the enumeration (escape or death): the
    /// tree is not enumerable — Unverifiable territory, never contained.
    Escaped { escaped_pids: Vec<u32> },
    /// The enumeration itself cannot support any conclusion.
    Unverifiable { reason: &'static str },
}

pub fn macos_classify_descendant_containment(
    snapshot: Option<&MacosProcessSnapshot>,
    expected_root: &MacosBirthIdentity,
    known_member_pids: &[u32],
) -> MacosContainmentVerdict {
    let Some(snapshot) = snapshot else {
        return MacosContainmentVerdict::Unverifiable {
            reason: "process enumeration failed",
        };
    };
    match snapshot.get(expected_root.pid) {
        Some(entry) if *entry == *expected_root => {}
        _ => {
            return MacosContainmentVerdict::Unverifiable {
                reason: "root birth identity mismatch",
            };
        }
    }
    let members = reachable_descendants(snapshot, expected_root);
    let escaped: Vec<u32> = known_member_pids
        .iter()
        .copied()
        .filter(|pid| !members.contains_key(pid))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if escaped.is_empty() {
        MacosContainmentVerdict::SnapshotReachable {
            members: members.len(),
        }
    } else {
        MacosContainmentVerdict::Escaped {
            escaped_pids: escaped,
        }
    }
}

/// Depth of each member relative to the root (pure; used only to order the
/// best-effort sweep deepest-first). Every chain terminates at the root by
/// construction of [`reachable_descendants`]; an orphaned chain yields no
/// entry (defensive, never reached).
fn descendant_depth(
    members: &BTreeMap<u32, MacosBirthIdentity>,
    root_pid: u32,
) -> BTreeMap<u32, u32> {
    let mut depths = BTreeMap::new();
    for (&pid, entry) in members {
        let mut depth = 0u32;
        let mut hop = entry.ppid;
        while let Some(parent) = members.get(&hop) {
            depth += 1;
            hop = parent.ppid;
        }
        if hop == root_pid {
            depths.insert(pid, depth + 1);
        }
    }
    depths
}

/// Best-effort SIGKILL sweep of the identity-checked strict descendants of
/// `expected_root` (P11.1 diagnostics path — the ONLY macOS termination
/// story until P24H's out-of-process guardian). The root itself is the
/// guardian's group contract and is never signalled here. Every candidate
/// is re-probed immediately before signalling: a pid whose live birth tuple
/// no longer matches the snapshot entry (PID reuse) is skipped — a recycled
/// foreign process is NEVER signalled. Returns the pids signalled,
/// deepest-first. This is not a death proof: unprovable trees stay
/// quarantined (P01) and write execution stays SafeDisabled (P11.3).
pub fn macos_terminate_descendants_best_effort(
    snapshot: &MacosProcessSnapshot,
    expected_root: &MacosBirthIdentity,
) -> Vec<u32> {
    let Some(root_entry) = snapshot.get(expected_root.pid) else {
        return Vec::new();
    };
    if root_entry != expected_root {
        return Vec::new();
    }
    let members = reachable_descendants(snapshot, expected_root);
    let depths = descendant_depth(&members, expected_root.pid);
    let mut order: Vec<u32> = depths.keys().copied().collect();
    // Deepest first (children die before parents so a mid-sweep parent
    // cannot spawn replacements); pid descending as the tiebreaker.
    order.sort_by(|a, b| {
        depths
            .get(b)
            .unwrap_or(&0)
            .cmp(depths.get(a).unwrap_or(&0))
            .then(b.cmp(a))
    });
    let mut signalled = Vec::new();
    for pid in order {
        let Some(entry) = members.get(&pid) else {
            continue;
        };
        if pid == std::process::id() {
            // Absolute guard: the sweep never signals the diagnosing
            // process itself, whatever the snapshot claims.
            continue;
        }
        // PID-reuse fence: signal only the exact instance enumerated.
        if macos_process_birth_identity(pid).as_ref() != Some(entry) {
            continue;
        }
        // SAFETY: kill(2) to an identity-verified descendant pid; delivering
        // the signal IS the intended effect and no memory is involved.
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        signalled.push(pid);
    }
    signalled
}

// P10.2 — the guardian launch gate -------------------------------------------

/// RAII owner of a `posix_spawnattr_t` handle: `posix_spawnattr_init`
/// allocates the attribute record and `destroy` frees it; the initialized
/// flag makes every error path release it exactly once (the windows.rs
/// AttributeListGuard discipline).
struct SpawnAttrGuard {
    attr: libc::posix_spawnattr_t,
    initialized: bool,
}

impl SpawnAttrGuard {
    fn new() -> Result<Self, GuardianError> {
        let mut attr: libc::posix_spawnattr_t = std::ptr::null_mut();
        // SAFETY: initializes (allocates) the handle stored in `attr`;
        // Drop releases it exactly once via destroy.
        let status = unsafe { libc::posix_spawnattr_init(&mut attr) };
        if status != 0 {
            return Err(spawn_errno("posix_spawnattr_init failed", status));
        }
        Ok(Self {
            attr,
            initialized: true,
        })
    }
}

impl Drop for SpawnAttrGuard {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: the handle was allocated by init and is destroyed once.
            unsafe { libc::posix_spawnattr_destroy(&mut self.attr) };
        }
    }
}

/// RAII owner of a `posix_spawn_file_actions_t` handle (same discipline as
/// [`SpawnAttrGuard`]).
struct FileActionsGuard {
    actions: libc::posix_spawn_file_actions_t,
    initialized: bool,
}

impl FileActionsGuard {
    fn new() -> Result<Self, GuardianError> {
        let mut actions: libc::posix_spawn_file_actions_t = std::ptr::null_mut();
        // SAFETY: initializes (allocates) the handle stored in `actions`;
        // Drop releases it exactly once via destroy.
        let status = unsafe { libc::posix_spawn_file_actions_init(&mut actions) };
        if status != 0 {
            return Err(spawn_errno("posix_spawn_file_actions_init failed", status));
        }
        Ok(Self {
            actions,
            initialized: true,
        })
    }
}

impl Drop for FileActionsGuard {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: the handle was allocated by init and is destroyed once.
            unsafe { libc::posix_spawn_file_actions_destroy(&mut self.actions) };
        }
    }
}

/// libc 0.2.189 predates the Apple binding of
/// `posix_spawn_file_actions_addchdir_np` (XNU `bsd/sys/spawn.h`, stable
/// libsystem_kernel API, macOS 10.15+), so it is declared here to keep the
/// dependency set unchanged. It is the only way to give a posix_spawn child
/// a working directory; fork+exec (std's pre-exec chdir) was rejected for
/// P10 because it cannot match `POSIX_SPAWN_CLOEXEC_DEFAULT`'s descriptor
/// discipline (see [`spawn_stopped_workload_macos`]).
extern "C" {
    fn posix_spawn_file_actions_addchdir_np(
        actions: *mut libc::posix_spawn_file_actions_t,
        path: *const libc::c_char,
    ) -> libc::c_int;
}

fn cstring(value: &str) -> Result<CString, GuardianError> {
    CString::new(value.as_bytes())
        .map_err(|_| GuardianError::Protocol("spawn strings must not contain NUL"))
}

/// posix_spawn and its attribute/file-action setup report failures as a
/// direct errno (not -1); wrap it with context.
fn spawn_errno(context: &str, status: libc::c_int) -> GuardianError {
    GuardianError::Io(format!(
        "{context}: {}",
        io::Error::from_raw_os_error(status)
    ))
}

/// A workload the guardian created and owns: it is reaped via waitpid and
/// killed by signal — the obligations std::process::Child would carry, held
/// as a raw pid because posix_spawn exposes no Child type.
struct MacosStoppedWorkload {
    pid: libc::pid_t,
}

impl MacosStoppedWorkload {
    /// Reap the workload (blocking). Intended to block: the guardian's
    /// lifetime doubles as zombie hygiene (P08 wording). EINTR is retried;
    /// any other waitpid failure ends the reap attempt.
    fn wait(&mut self) {
        loop {
            let mut status: libc::c_int = 0;
            // SAFETY: waitpid on our direct child; `status` is a plain
            // out-slot and no memory beyond it is touched.
            let reaped = unsafe { libc::waitpid(self.pid, &mut status, 0) };
            if reaped == self.pid {
                return;
            }
            if reaped < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                return;
            }
        }
    }
}

/// Kill our own just-spawned child on a verification-failure path. SIGKILL
/// lands on stopped processes too.
fn kill_workload_pid(pid: libc::pid_t) {
    // SAFETY: kill(2) to a process we just spawned; delivering the signal
    // IS the intended effect and no memory is involved.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
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

/// Spawn the workload STOPPED behind the release gate (P10.2). One
/// `posix_spawn` call with `POSIX_SPAWN_START_SUSPENDED |
/// POSIX_SPAWN_SETPGROUP(pgroup 0) | POSIX_SPAWN_CLOEXEC_DEFAULT`:
/// START_SUSPENDED is applied inside the syscall, so not one workload
/// instruction can precede the gate; the only opener is SIGCONT from
/// [`serve_macos_guardian`] after a valid Release. CLOEXEC_DEFAULT makes
/// the inherited set exactly {0, 1, 2} regardless of the spawner's table
/// (P10.3); the only file action is addchdir for an explicit cwd, and
/// argv/environment are exactly the request's, never the parent's.
///
/// Fork is avoided entirely (Apple's guidance for multithreaded
/// processes).
fn spawn_stopped_workload_macos(
    executable: &str,
    arguments: &[String],
    cwd: Option<&str>,
    environment: &[(String, String)],
) -> Result<(MacosStoppedWorkload, MacosBirthIdentity), GuardianError> {
    let executable_c = cstring(executable)?;
    // argv[0] carries the executable, mirroring std::process conventions.
    let mut argv: Vec<CString> = Vec::with_capacity(arguments.len() + 2);
    argv.push(executable_c.clone());
    for argument in arguments {
        argv.push(cstring(argument)?);
    }
    let mut envp: Vec<CString> = Vec::with_capacity(environment.len() + 1);
    for (key, value) in environment {
        // '=' and NUL cannot survive the control-protocol codec; this is
        // the last line of defense before environment-block parsing.
        envp.push(cstring(&format!("{key}={value}"))?);
    }
    // POSIX requires the environment array to end with an empty string.
    envp.push(CString::new("").expect("empty string has no NUL"));

    let spawn_attr = SpawnAttrGuard::new()?;
    let flags = (libc::POSIX_SPAWN_START_SUSPENDED
        | libc::POSIX_SPAWN_SETPGROUP
        | libc::POSIX_SPAWN_CLOEXEC_DEFAULT) as libc::c_short;
    // SAFETY: both setters mutate the attribute record allocated by init.
    unsafe {
        if libc::posix_spawnattr_setflags(&mut spawn_attr.attr, flags) != 0 {
            let error = io::Error::last_os_error();
            return Err(GuardianError::Io(format!(
                "posix_spawnattr_setflags failed: {error}"
            )));
        }
        // pgroup 0 + SETPGROUP: the child becomes the leader of a NEW
        // process group (verified via getpgid after the spawn, never assumed).
        if libc::posix_spawnattr_setpgroup(&mut spawn_attr.attr, 0) != 0 {
            let error = io::Error::last_os_error();
            return Err(GuardianError::Io(format!(
                "posix_spawnattr_setpgroup failed: {error}"
            )));
        }
    }

    let file_actions = FileActionsGuard::new()?;
    if let Some(cwd) = cwd {
        let cwd_c = cstring(cwd)?;
        // SAFETY: appends the chdir action to the record allocated above;
        // the string outlives the call (the action copies it).
        let status = unsafe {
            posix_spawn_file_actions_addchdir_np(&mut file_actions.actions, cwd_c.as_ptr())
        };
        if status != 0 {
            return Err(spawn_errno(
                "posix_spawn_file_actions_addchdir_np failed",
                status,
            ));
        }
    }

    let mut argv_pointers: Vec<*mut libc::c_char> = argv
        .iter()
        .map(|value| value.as_ptr() as *mut libc::c_char)
        .collect();
    argv_pointers.push(std::ptr::null_mut());
    let mut envp_pointers: Vec<*mut libc::c_char> = envp
        .iter()
        .map(|value| value.as_ptr() as *mut libc::c_char)
        .collect();
    envp_pointers.push(std::ptr::null_mut());

    let mut pid: libc::pid_t = 0;
    // SAFETY: the argv/envp pointer arrays are null-terminated and their
    // strings live until the call returns; the guards own the attribute and
    // file-actions records for the duration; `pid` is a plain out-slot.
    let status = unsafe {
        libc::posix_spawn(
            &mut pid,
            executable_c.as_ptr(),
            &file_actions.actions,
            &spawn_attr.attr,
            argv_pointers.as_ptr(),
            envp_pointers.as_ptr(),
        )
    };
    if status != 0 {
        return Err(spawn_errno("posix_spawn failed", status));
    }
    if pid <= 0 {
        // A successful status with no usable pid would violate the kernel
        // contract; refuse it rather than reason about pid 0.
        return Err(GuardianError::Io(
            "posix_spawn returned no usable pid".into(),
        ));
    }

    // The group id is verified via getpgid, never assumed (P08 rule): the
    // negative case covers both the getpgid failure and a wrong group.
    let pgid = unsafe { libc::getpgid(pid) };
    if pgid != pid {
        kill_workload_pid(pid);
        MacosStoppedWorkload { pid }.wait();
        return Err(GuardianError::Io(
            "the gated workload did not become its own group leader".into(),
        ));
    }
    // Birth identity from PROC_PIDTBSDINFO, fail-closed (P10.1): an
    // unreadable identity is not delivered and the gate stays shut. The
    // parentage is pinned too: the spawned child's ppid must be THIS
    // guardian process.
    let birth = match macos_process_birth_identity(pid as u32) {
        Some(birth) if birth.ppid == std::process::id() => birth,
        _ => {
            kill_workload_pid(pid);
            MacosStoppedWorkload { pid }.wait();
            return Err(GuardianError::Io(
                "the gated workload birth identity is unreadable via PROC_PIDTBSDINFO".into(),
            ));
        }
    };
    Ok((MacosStoppedWorkload { pid }, birth))
}

/// The macOS guardian's control session (mirror of P08's
/// `guardian_session`): handshake → gated spawn → release-or-EOF over
/// arbitrary streams, with P08's exit codes. Every abnormal path kills the
/// group — the session never leaves a live gate behind. The only unsafe
/// lives in [`spawn_stopped_workload_macos`], [`kill_group`] and
/// [`kill_workload_pid`]. Hosting note: P08 drives this from a dedicated
/// guardian BINARY; on macOS the P10 composition runs it on a thread
/// inside the daemon (see [`macos_spawn_via_guardian`]), and a real
/// out-of-process guardian is a future thin fd-3/4 wrapper around this
/// very function.
pub fn serve_macos_guardian<R: Read, W: Write>(commands: &mut R, replies: &mut W) -> i32 {
    use guardian_protocol::{read_frame, write_frame};

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
        // Peer died before the handshake: nothing is gated.
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
    let (mut workload, birth) =
        match spawn_stopped_workload_macos(&executable, &arguments, cwd.as_deref(), &environment) {
            Ok(created) => created,
            Err(_) => {
                // Nothing is gated; report and drain until the peer closes.
                let _ = write_frame(
                    replies,
                    &GuardianFrame::Error {
                        code: FrameErrorCode::SpawnFailed,
                    },
                );
                return drain_to_eof(commands);
            }
        };
    let identity = GuardianIdentity {
        outer_pid: birth.pid,
        // Verified equal via getpgid in spawn_stopped_workload_macos.
        group_pid: birth.pid,
        birth_start_identity: birth.start_identity(),
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
        // The peer never learned the identity: fail closed.
        kill_group(identity.group_pid as i32, libc::SIGKILL);
        workload.wait();
        return GUARDIAN_EXIT_PROTOCOL;
    }

    // Release-or-EOF loop: the group stays kernel-held until `Release`.
    loop {
        match read_frame(commands) {
            Ok(Some(GuardianFrame::Release)) => {
                // Open the gate exactly once.
                kill_group(identity.group_pid as i32, libc::SIGCONT);
                if write_frame(replies, &GuardianFrame::ReleaseAck).is_err() {
                    // An undeliverable ack means the release was never
                    // confirmed: fail closed — never leave the peer owning
                    // a live tree it did not acknowledge.
                    kill_group(identity.group_pid as i32, libc::SIGKILL);
                    workload.wait();
                    return GUARDIAN_EXIT_PROTOCOL;
                }
                // Reaping the released workload is intended to block: the
                // guardian's lifetime doubles as zombie hygiene.
                workload.wait();
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
                workload.wait();
                return GUARDIAN_EXIT_PROTOCOL;
            }
            // EOF BEFORE release (dropped gate or dead peer): the gate must
            // never open — kill the still-suspended group.
            Ok(None) => {
                kill_group(identity.group_pid as i32, libc::SIGKILL);
                workload.wait();
                return GUARDIAN_EXIT_DAEMON_EOF;
            }
            Err(_) => {
                kill_group(identity.group_pid as i32, libc::SIGKILL);
                workload.wait();
                return GUARDIAN_EXIT_PROTOCOL;
            }
        }
    }
}

/// Drain the command stream to EOF (spawn-failure aftermath): no workload
/// exists, so a belated Release must not be acknowledged and an EOF must
/// simply end the session.
fn drain_to_eof<R: Read>(commands: &mut R) -> i32 {
    let mut sink = [0u8; 512];
    loop {
        match commands.read(&mut sink) {
            Ok(0) | Err(_) => return GUARDIAN_EXIT_DAEMON_EOF,
            Ok(_) => continue,
        }
    }
}

// P10.3 — the daemon side: persist-then-release -------------------------------

/// Bounded waits (P08 values): a guardian that cannot answer in time is
/// treated as failed, and every failure path tears the session down
/// fail-closed.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const IDENTITY_TIMEOUT: Duration = Duration::from_secs(10);
const RELEASE_ACK_TIMEOUT: Duration = Duration::from_secs(10);

/// One bounded control-frame read (P08's design, adapted to socket clones):
/// the blocking read runs on a detached helper thread holding a CLONE of
/// the stream and races `recv_timeout`. On timeout the helper stays blocked
/// on the clone and self-terminates at EOF — the guardian session's
/// fail-closed paths always close the reply end, so the clone is released;
/// the original reply end is dropped by the caller.
fn read_frame_bounded(
    stream: &UnixStream,
    timeout: Duration,
    stage: &'static str,
) -> Result<GuardianFrame, GuardianError> {
    let reader = stream
        .try_clone()
        .map_err(|error| GuardianError::Io(error.to_string()))?;
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut reader = reader;
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

/// Spawn the workload BEHIND the guardian's release gate (P10): the
/// guardian session is hosted in-process (a dedicated thread — see the
/// module docs for the honest scope), the P08 protocol handshake runs, then
/// `SpawnRequest` makes the guardian create the kernel-held workload and
/// report its birth identity. The returned gate blocks all workload
/// execution until [`MacosGatedWorkload::release_once`] — persist the
/// identity first (e.g. [`macos_owner_identity`]). Fully synchronous
/// (std only); the supervisor wires async later.
pub fn macos_spawn_via_guardian(
    request: GuardianSpawnRequest,
) -> Result<MacosGatedWorkload, GuardianError> {
    let (daemon_command, guardian_command) =
        UnixStream::pair().map_err(|error| GuardianError::Io(error.to_string()))?;
    let (guardian_reply, daemon_reply) =
        UnixStream::pair().map_err(|error| GuardianError::Io(error.to_string()))?;
    // Both streams were created by std (O_CLOEXEC) and never cross an exec
    // in this composition; the workload's inheritance is pinned to stdio by
    // POSIX_SPAWN_CLOEXEC_DEFAULT regardless.
    let guardian = std::thread::Builder::new()
        .name("r-code-macos-guardian".into())
        .spawn(move || {
            let mut commands = guardian_command;
            let mut replies = guardian_reply;
            serve_macos_guardian(&mut commands, &mut replies);
        })
        .map_err(|error| GuardianError::Io(format!("guardian thread spawn failed: {error}")))?;
    let (identity, command_write, reply_read, guardian) =
        negotiate_macos_guardian_spawn(daemon_command, daemon_reply, Some(guardian), &request)?;
    Ok(MacosGatedWorkload {
        control_write: Some(command_write),
        reply_read: Some(reply_read),
        identity,
        guardian,
    })
}

/// Handshake + gated spawn over an already-started guardian session. Owns
/// the session ends so EVERY failure path tears them down fail-closed:
/// close the command end (EOF is the guardian's own fail-closed trigger —
/// it kills the group), close the reply end, then join the guardian thread.
fn negotiate_macos_guardian_spawn(
    mut command_write: UnixStream,
    reply_read: UnixStream,
    guardian: Option<std::thread::JoinHandle<()>>,
    request: &GuardianSpawnRequest,
) -> Result<
    (
        GuardianIdentity,
        UnixStream,
        UnixStream,
        Option<std::thread::JoinHandle<()>>,
    ),
    GuardianError,
> {
    let negotiated = (|| -> Result<GuardianIdentity, GuardianError> {
        guardian_protocol::write_frame(
            &mut command_write,
            &GuardianFrame::Hello {
                version: PROTOCOL_VERSION,
            },
        )?;
        match read_frame_bounded(&reply_read, HANDSHAKE_TIMEOUT, "guardian handshake") {
            Ok(GuardianFrame::Hello {
                version: PROTOCOL_VERSION,
            }) => {}
            Ok(GuardianFrame::Error {
                code: FrameErrorCode::VersionMismatch,
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
        match read_frame_bounded(&reply_read, IDENTITY_TIMEOUT, "workload identity") {
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
                // Fail closed on an unverifiable identity (P08 rule); on
                // macOS the daemon additionally re-probes the birth tuple
                // from PROC_PIDTBSDINFO itself, so what gets persisted
                // never rests on trusting the frame alone.
                if identity.birth_start_identity == 0 || !group_alive(identity.group_pid as i32) {
                    return Err(GuardianError::Protocol(
                        "guardian returned an unverifiable workload identity",
                    ));
                }
                match macos_process_birth_identity(identity.outer_pid) {
                    Some(birth) if birth.start_identity() == identity.birth_start_identity => {}
                    _ => {
                        return Err(GuardianError::Protocol(
                            "the workload's PROC_PIDTBSDINFO birth identity does not match the guardian's report",
                        ));
                    }
                }
                Ok(identity)
            }
            Ok(GuardianFrame::Error {
                code: FrameErrorCode::SpawnFailed,
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
        // Fail-closed teardown (P08 ordering, thread-adapted): close the
        // command end FIRST (EOF makes the guardian kill the group itself),
        // close the reply end, then join the guardian thread — its every
        // exit path is bounded by the SIGKILL it delivers to the group.
        drop(command_write);
        drop(reply_read);
        if let Some(guardian) = guardian {
            let _ = guardian.join();
        }
        return Err(error);
    }
    let identity = negotiated.expect("negotiation result checked above");
    Ok((identity, command_write, reply_read, guardian))
}

/// A workload created BEHIND the guardian's release gate (P10.2, mirror of
/// unix.rs GatedWorkload): at handout it exists only as a kernel-held image
/// in its own process group — no instruction of its binary has run and it
/// holds no descriptor beyond stdio. Persist [`MacosGatedWorkload::identity`]
/// first, then open the gate with [`MacosGatedWorkload::release_once`]
/// exactly once. Dropping the gate WITHOUT releasing is fail-closed: the
/// command end closes, the guardian reads EOF and SIGKILLs the
/// still-suspended group. Honest P10 scope: the guardian is an in-process
/// thread; daemon-death → group-kill propagation across processes is the
/// out-of-process successor's contract (P24H helper packaging).
pub struct MacosGatedWorkload {
    /// Guardian → daemon commands; taken (None) by `release_once`.
    control_write: Option<UnixStream>,
    /// Guardian → daemon replies, held for the release acknowledgement.
    reply_read: Option<UnixStream>,
    identity: GuardianIdentity,
    /// The in-process guardian thread; joined on every owned path.
    guardian: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for MacosGatedWorkload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Descriptors and thread handles are never printed; durable
        // identity only.
        f.debug_struct("MacosGatedWorkload")
            .field("identity", &self.identity)
            .finish()
    }
}

impl MacosGatedWorkload {
    /// Identity to persist BEFORE releasing the gate.
    pub fn identity(&self) -> GuardianIdentity {
        self.identity
    }

    /// Open the release gate exactly once (P10.3 ordering is the caller's:
    /// persist the identity — e.g. via [`macos_owner_identity`] — BEFORE
    /// calling). The gate is consumed, so a second release cannot be
    /// expressed. Anything other than a confirmed `ReleaseAck` fails
    /// closed: the group is killed and the guardian thread joined, so no
    /// live handle is ever returned — dead, never running-unowned.
    pub fn release_once(mut self) -> Result<MacosReleasedWorkload, GuardianError> {
        let mut control_write = self
            .control_write
            .take()
            .expect("gate fields are present until release_once takes them");
        let reply_read = self
            .reply_read
            .take()
            .expect("gate fields are present until release_once takes them");
        let guardian = self
            .guardian
            .take()
            .expect("gate fields are present until release_once takes them");
        let identity = self.identity;
        guardian_protocol::write_frame(&mut control_write, &GuardianFrame::Release)?;
        match read_frame_bounded(&reply_read, RELEASE_ACK_TIMEOUT, "release acknowledgement") {
            Ok(GuardianFrame::ReleaseAck) => {
                // Gate open; both session ends close (the guardian is
                // finishing its reap and needs nothing further).
                drop(control_write);
                drop(reply_read);
                Ok(MacosReleasedWorkload {
                    identity,
                    guardian: Some(guardian),
                })
            }
            outcome => {
                // Anything else (Error frame, foreign frame, EOF, timeout)
                // means the release was never confirmed: fail closed — kill
                // the group, then join the guardian thread.
                drop(control_write);
                drop(reply_read);
                kill_group(identity.group_pid as i32, libc::SIGKILL);
                let _ = guardian.join();
                Err(match outcome {
                    Ok(_) => GuardianError::Protocol("guardian did not acknowledge the release"),
                    Err(error) => error,
                })
            }
        }
    }
}

impl Drop for MacosGatedWorkload {
    fn drop(&mut self) {
        // Fail-closed teardown of an UNRELEASED gate, in strict order (P08
        // mirror): close the command end (EOF → the guardian SIGKILLs the
        // still-suspended group), close the reply end, then join the
        // guardian thread. Taken fields are None: a consumed gate never
        // double-closes and never kills a released workload.
        drop(self.control_write.take());
        drop(self.reply_read.take());
        if let Some(guardian) = self.guardian.take() {
            let _ = guardian.join();
        }
    }
}

/// A released workload (post-[`MacosGatedWorkload::release_once`]): the
/// gate is open — the group was SIGCONTed exactly once — and the identity
/// was already persisted by the daemon. The guardian thread lives until the
/// workload exits (its waitpid is the reaper); [`Self::wait_guardian`]
/// joins it.
#[derive(Debug)]
pub struct MacosReleasedWorkload {
    identity: GuardianIdentity,
    guardian: Option<std::thread::JoinHandle<()>>,
}

impl MacosReleasedWorkload {
    /// Identity of the released workload (as persisted before the release).
    pub fn identity(&self) -> GuardianIdentity {
        self.identity
    }

    /// Join the guardian thread — which returns exactly when the released
    /// workload (and its group) has been reaped.
    pub fn wait_guardian(&mut self) {
        if let Some(guardian) = self.guardian.take() {
            let _ = guardian.join();
        }
    }
}

impl Drop for MacosReleasedWorkload {
    fn drop(&mut self) {
        // Post-release the guardian thread only reaps the (running)
        // workload; it is NEVER killed here. Detaching (dropping the
        // JoinHandle) leaves reaping to the thread; the daemon-death story
        // belongs to the out-of-process guardian (P24H).
        let _ = self.guardian.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sleep_request() -> GuardianSpawnRequest {
        GuardianSpawnRequest {
            executable: "/bin/sleep".into(),
            arguments: vec!["30".into()],
            cwd: None,
            environment: BTreeMap::new(),
        }
    }

    #[test]
    fn birth_identity_of_the_current_process_is_complete() {
        let birth = macos_process_birth_identity(std::process::id())
            .expect("the test process must have a PROC_PIDTBSDINFO birth identity");
        assert!(birth.is_complete());
        assert_eq!(birth.pid, std::process::id());
        assert_ne!(birth.start_identity(), 0);
    }

    #[test]
    fn owner_identity_carries_the_birth_tuple() {
        let identity = macos_owner_identity(std::process::id()).expect("owner identity");
        assert_eq!(identity.pid, std::process::id());
        assert_ne!(identity.start_identity, 0);
        assert_eq!(
            identity.platform_identity["native"].as_str(),
            Some("macos-gated")
        );
        assert!(
            identity.platform_identity["startSeconds"]
                .as_u64()
                .expect("seconds")
                > 0
        );
    }

    #[test]
    fn unreleased_gate_kills_the_suspended_group() {
        let workload = macos_spawn_via_guardian(sleep_request()).expect("gated spawn");
        let identity = workload.identity();
        assert_eq!(
            identity.outer_pid, identity.group_pid,
            "the workload must lead its own group"
        );
        assert_ne!(identity.birth_start_identity, 0);
        assert!(group_alive(identity.group_pid as i32));
        // Fail-closed drop: an unreleased gate never leaves the group alive.
        drop(workload);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while group_alive(identity.group_pid as i32) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            !group_alive(identity.group_pid as i32),
            "the unreleased gate must kill the suspended group"
        );
    }

    #[test]
    fn released_workload_is_reaped_with_its_guardian() {
        let workload = macos_spawn_via_guardian(sleep_request()).expect("gated spawn");
        let identity = workload.identity();
        let mut released = workload.release_once().expect("release");
        assert_eq!(released.identity(), identity);
        // Daemon-side cancellation: SIGKILL the group; the guardian thread's
        // waitpid unblocks and reaping finishes.
        kill_group(identity.group_pid as i32, libc::SIGKILL);
        released.wait_guardian();
        assert!(
            !group_alive(identity.group_pid as i32),
            "the released workload must die with its group"
        );
    }

    // P11 pure-logic tests: synthetic snapshots only. Live adversarial
    // fixtures (setsid/double-fork escapes) belong to the s11 macOS CI
    // suite; these pin the classification state machine itself.

    fn synthetic_entry(pid: u32, ppid: u32, start: u64) -> MacosBirthIdentity {
        MacosBirthIdentity {
            pid,
            ppid,
            start_seconds: 1_700_000_000 + start,
            start_useconds: start * 7 % 1_000_000,
        }
    }

    fn synthetic_snapshot(entries: &[(u32, u32, u64)]) -> MacosProcessSnapshot {
        MacosProcessSnapshot {
            entries: entries
                .iter()
                .map(|&(pid, ppid, start)| (pid, synthetic_entry(pid, ppid, start)))
                .collect(),
        }
    }

    #[test]
    fn classify_reports_reachable_members_as_diagnostics_only() {
        // root 100 -> 101 -> 102 (grandchild) plus an unrelated tree.
        let snapshot = synthetic_snapshot(&[
            (100, 1, 10),
            (101, 100, 11),
            (102, 101, 12),
            (200, 1, 20),
            (201, 200, 21),
        ]);
        let root = synthetic_entry(100, 1, 10);
        let verdict = macos_classify_descendant_containment(Some(&snapshot), &root, &[101, 102]);
        assert_eq!(
            verdict,
            MacosContainmentVerdict::SnapshotReachable { members: 2 }
        );
    }

    #[test]
    fn classify_reports_a_reparented_descendant_as_escaped() {
        // 102 setsid+double-forked: its ppid became 1 (launchd), so it is
        // invisible to the ppid walk even though the caller knows its pid.
        let snapshot = synthetic_snapshot(&[(100, 1, 10), (101, 100, 11), (102, 1, 12)]);
        let root = synthetic_entry(100, 1, 10);
        let verdict = macos_classify_descendant_containment(Some(&snapshot), &root, &[101, 102]);
        assert_eq!(
            verdict,
            MacosContainmentVerdict::Escaped {
                escaped_pids: vec![102]
            }
        );
    }

    #[test]
    fn classify_refuses_a_root_identity_mismatch_and_a_failed_snapshot() {
        let snapshot = synthetic_snapshot(&[(100, 1, 10), (101, 100, 11)]);
        // A recycled pid 100 carrying a different start tuple is NOT our
        // root: the verdict is Unverifiable, never a guess.
        let recycled = synthetic_entry(100, 1, 99);
        assert_eq!(
            macos_classify_descendant_containment(Some(&snapshot), &recycled, &[101]),
            MacosContainmentVerdict::Unverifiable {
                reason: "root birth identity mismatch"
            }
        );
        assert_eq!(
            macos_classify_descendant_containment(None, &synthetic_entry(100, 1, 10), &[101]),
            MacosContainmentVerdict::Unverifiable {
                reason: "process enumeration failed"
            }
        );
    }

    #[test]
    fn terminate_sweep_orders_members_deepest_first() {
        let members: BTreeMap<u32, MacosBirthIdentity> = [
            synthetic_entry(101, 100, 11),
            synthetic_entry(102, 101, 12),
            synthetic_entry(103, 100, 13),
        ]
        .into_iter()
        .map(|entry| (entry.pid, entry))
        .collect();
        let depths = descendant_depth(&members, 100);
        assert_eq!(depths.get(&101), Some(&1));
        assert_eq!(depths.get(&102), Some(&2));
        assert_eq!(depths.get(&103), Some(&1));
        // A chain that never reaches the root yields no depth (defensive).
        let orphaned: BTreeMap<u32, MacosBirthIdentity> = [synthetic_entry(300, 299, 1)]
            .into_iter()
            .map(|entry| (entry.pid, entry))
            .collect();
        assert!(!descendant_depth(&orphaned, 100).contains_key(&300));
    }

    #[test]
    fn snapshot_enumeration_is_nonempty_and_self_consistent() {
        // Runs on macOS CI: the local process must appear with a complete
        // identity whose pid matches its map key.
        let snapshot = macos_snapshot_processes().expect("process enumeration");
        let own = snapshot.get(std::process::id()).expect("own pid listed");
        assert_eq!(own.pid, std::process::id());
        assert!(own.is_complete());
    }
}
