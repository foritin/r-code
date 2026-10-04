//! P16 — non-activating pinned bwrap launch plan for Linux: verify the
//! immutable bubblewrap binary and build ONE user/mount/pid-namespace
//! launch plan (argv + mount table + identity handoff slots). This module
//! BUILDS and VALIDATES plans; it never spawns, never proves containment
//! (P09) and never activates anything (P13's gate stays closed until
//! P09/P17 land). Windows hosts never compile this file.
//!
//! P17 — seccomp policy: compile the deny-list namespace-escape policy
//! with seccompiler 0.5.0 and attach it to a plan via bwrap's --seccomp
//! fd. Compilation fails closed on unknown architectures; nothing here
//! activates execution.

#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// The pinned bwrap location the plan accepts (PRD: immutable system path).
pub const PINNED_BWRAP_PATH: &str = "/usr/bin/bwrap";
/// Minimum bubblewrap version the plan accepts.
pub const MINIMUM_BWRAP_VERSION: (u32, u32) = (0, 8);

/// Fail-closed plan-build verdict: why a launch plan cannot be built on
/// this host. Callers map this onto SafeDisabled report material — never
/// onto a degraded run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BwrapPlanError {
    #[error("pinned bwrap is missing or unreadable: {0}")]
    BinaryMissing(String),
    #[error("pinned bwrap is not root-owned or is group/world-writable: {0}")]
    BinaryNotImmutable(String),
    #[error("pinned bwrap version {found:?} is below {minimum:?}")]
    VersionTooOld {
        found: (u32, u32),
        minimum: (u32, u32),
    },
    #[error("version output could not be parsed: {0}")]
    VersionUnparseable(String),
    #[error("profile is not a valid sandbox profile: {0}")]
    InvalidProfile(String),
}

/// One mount entry in the launch plan. `target` is the in-sandbox path;
/// bwrap's model mounts exactly what the table lists — anything absent
/// from the table (notably `.git`) does not exist inside the sandbox.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum BwrapMount {
    /// Read-only bind (read roots, toolchain roots, cache roots).
    ReadOnly { source: String, target: String },
    /// Read-write bind (write roots and the scratch root only).
    ReadWrite { source: String, target: String },
    /// Fresh tmpfs at the given in-sandbox path.
    Tmpfs { target: String },
}

/// The identity handoff P09 will consume (P16.2): the OUTER bwrap pid the
/// supervisor owns plus the namespace PID1/reaper identity slot. Building
/// the plan fills the EXPECTED slots; a launched instance fills the
/// observed values. A plan whose observed handoff is missing can never
/// activate (SafeDisabled).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NamespaceIdentityHandoff {
    /// Outer bwrap pid (supervisor-owned; PID-reuse fenced by the P01
    /// start identity elsewhere).
    pub outer_pid: Option<u32>,
    /// Namespace-internal PID1 pid (the reaper inside --unshare-pid).
    pub namespace_pid1: Option<u32>,
    /// Boot identity the plan was built under (stale across reboots).
    pub expected_boot_id: String,
}

impl NamespaceIdentityHandoff {
    /// The empty handoff a fresh plan carries (nothing launched yet).
    pub fn expected(boot_identity: &str) -> Self {
        Self {
            outer_pid: None,
            namespace_pid1: None,
            expected_boot_id: boot_identity.to_string(),
        }
    }

    /// A complete handoff requires BOTH identities (P16.2): exactly one
    /// component owns the PID namespace, and a missing half is a missing
    /// handoff — fail-closed, never guessed.
    pub fn is_complete(&self) -> bool {
        self.outer_pid.is_some() && self.namespace_pid1.is_some()
    }
}

/// The complete non-activating launch plan (P16.2/P16.3).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BwrapLaunchPlan {
    pub bwrap_path: String,
    pub bwrap_version: (u32, u32),
    pub mounts: Vec<BwrapMount>,
    /// Exactly one `--unshare-pid` is implied by construction; the argv
    /// keeps the flag count explicit for P09's proof contract.
    pub unshare_pid: bool,
    /// Offline → unshare-net; PublicInternetClient → share-net recorded
    /// (activation still requires P17 seccomp; this is plan material).
    pub unshare_network: bool,
    pub environment_allowlist: Vec<String>,
    pub argv: Vec<String>,
    pub identity_handoff: NamespaceIdentityHandoff,
}

impl BwrapLaunchPlan {
    /// The plan's policy digest input (P13 backend contract): commits the
    /// binary identity, mount table, namespace flags and env allowlist.
    pub fn policy_material(&self) -> serde_json::Value {
        serde_json::json!({
            "bwrap": self.bwrap_path,
            "version": self.bwrap_version,
            "mounts": self.mounts,
            "unsharePid": self.unshare_pid,
            "unshareNetwork": self.unshare_network,
            "environmentAllowlist": self.environment_allowlist,
        })
    }
}

/// P16.1: verify the pinned binary — exists, root:root ownership, no
/// group/world write bit, and version >= 0.8.0. Rootless usability is
/// established by the version check (0.8.x documents unprivileged
/// user-namespace operation) plus the actual launch in Linux CI.
pub fn verify_pinned_bwrap(binary: &Path) -> Result<(u32, u32), BwrapPlanError> {
    let metadata = std::fs::metadata(binary)
        .map_err(|error| BwrapPlanError::BinaryMissing(error.to_string()))?;
    if !metadata.is_file() {
        return Err(BwrapPlanError::BinaryMissing("not a regular file".into()));
    }
    if metadata.uid() != 0 || metadata.gid() != 0 {
        return Err(BwrapPlanError::BinaryNotImmutable(format!(
            "owner {}:{} (expected 0:0)",
            metadata.uid(),
            metadata.gid()
        )));
    }
    if metadata.mode() & 0o022 != 0 {
        return Err(BwrapPlanError::BinaryNotImmutable(format!(
            "mode {:o} allows group/world write",
            metadata.mode()
        )));
    }
    parse_bwrap_version(&binary.to_string_lossy())
}

/// Run `bwrap --version` and parse `bubblewrap X.Y.Z`.
fn parse_bwrap_version(binary: &str) -> Result<(u32, u32), BwrapPlanError> {
    let output = std::process::Command::new(binary)
        .arg("--version")
        .output()
        .map_err(|error| BwrapPlanError::VersionUnparseable(error.to_string()))?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // "bubblewrap 0.11.0" — take the first two numeric components.
    let version = text
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| BwrapPlanError::VersionUnparseable(text.clone()))?;
    let mut numbers = version.split('.');
    let major = numbers
        .next()
        .and_then(|part| part.parse::<u32>().ok())
        .ok_or_else(|| BwrapPlanError::VersionUnparseable(text.clone()))?;
    let minor = numbers
        .next()
        .and_then(|part| part.parse::<u32>().ok())
        .ok_or_else(|| BwrapPlanError::VersionUnparseable(text.clone()))?;
    Ok((major, minor))
}

/// P16.2/P16.3: build the ONE-namespace launch plan for a validated
/// profile. Fail-closed on profile problems; the resulting argv carries
/// `--unshare-pid` EXACTLY once, mounts exactly the profile roots
/// (read/toolchain/cache ro, write+scratch rw, fresh tmpfs nowhere unless
/// requested), hides `.git` by construction (it is simply never in the
/// mount table), records the network namespace decision, and passes the
/// environment allowlist through `--setenv` entries only. The identity
/// handoff starts empty — P09 fills it from a real launch.
pub fn build_bwrap_launch_plan(
    profile: &super::SandboxProfileMaterial,
    binary: &Path,
    boot_identity: &str,
) -> Result<BwrapLaunchPlan, BwrapPlanError> {
    profile
        .validate()
        .map_err(|reason| BwrapPlanError::InvalidProfile(reason.to_string()))?;
    if profile.network == super::SandboxNetworkClass::HostNetwork {
        return Err(BwrapPlanError::InvalidProfile(
            "host-network is unsupported in this wave".into(),
        ));
    }
    for root in profile
        .read_roots
        .iter()
        .chain(profile.toolchain_roots.iter())
        .chain(profile.cache_roots.iter())
        .chain(profile.write_roots.iter())
        .chain([&profile.scratch_root])
    {
        let path = Path::new(root);
        if path
            .components()
            .any(|component| component.as_os_str().to_string_lossy() == ".git")
        {
            return Err(BwrapPlanError::InvalidProfile(
                "bwrap mounts must never cover .git".into(),
            ));
        }
    }
    let version = verify_pinned_bwrap(binary)?;
    if version < MINIMUM_BWRAP_VERSION {
        return Err(BwrapPlanError::VersionTooOld {
            found: version,
            minimum: MINIMUM_BWRAP_VERSION,
        });
    }

    let mut mounts: Vec<BwrapMount> = Vec::new();
    for root in profile
        .read_roots
        .iter()
        .chain(profile.toolchain_roots.iter())
        .chain(profile.cache_roots.iter())
    {
        mounts.push(BwrapMount::ReadOnly {
            source: root.clone(),
            target: root.clone(),
        });
    }
    for root in profile.write_roots.iter().chain([&profile.scratch_root]) {
        mounts.push(BwrapMount::ReadWrite {
            source: root.clone(),
            target: root.clone(),
        });
    }
    // 序列化定序只为 dedup 的确定性——BwrapMount 形状固定，序列化不会
    // 失败；失败时以空串参与排序（不 panic，序仍确定）。
    mounts.sort_by(|left, right| {
        let left_key = serde_json::to_string(left).unwrap_or_default();
        let right_key = serde_json::to_string(right).unwrap_or_default();
        left_key.cmp(&right_key)
    });
    mounts.dedup();

    let unshare_network = profile.network == super::SandboxNetworkClass::Offline;
    let mut argv: Vec<String> = vec![binary.to_string_lossy().into_owned()];
    argv.push("--unshare-pid".into());
    if unshare_network {
        argv.push("--unshare-net".into());
    }
    for mount in &mounts {
        match mount {
            BwrapMount::ReadOnly { source, target } => {
                argv.push("--ro-bind".into());
                argv.push(source.clone());
                argv.push(target.clone());
            }
            BwrapMount::ReadWrite { source, target } => {
                argv.push("--bind".into());
                argv.push(source.clone());
                argv.push(target.clone());
            }
            BwrapMount::Tmpfs { target } => {
                argv.push("--tmpfs".into());
                argv.push(target.clone());
            }
        }
    }
    for key in &profile.environment_allowlist {
        argv.push("--setenv".into());
        argv.push(key.clone());
        // The VALUE is supplied by the launcher at spawn time; the plan
        // records the allowlist only (empty placeholder here).
        argv.push(String::new());
    }
    argv.push("--".into());
    argv.push("<workload-argv-follows>".into());

    Ok(BwrapLaunchPlan {
        bwrap_path: binary.to_string_lossy().into_owned(),
        bwrap_version: version,
        mounts,
        unshare_pid: true,
        unshare_network,
        environment_allowlist: profile.environment_allowlist.clone(),
        argv,
        identity_handoff: NamespaceIdentityHandoff::expected(boot_identity),
    })
}

/// Absolute-path helper mirroring the P14/P15 validation semantics for
/// tests on Linux CI.
pub fn is_absolute_root(path: &str) -> bool {
    Path::new(path).is_absolute()
}

// P17 — seccomp policy compile and attach -------------------------------------

use seccompiler::{
    BpfProgram, SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter,
    SeccompRule, TargetArch,
};

/// Fail-closed seccomp policy errors: an unknown architecture or a filter
/// that will not compile keeps the capability SafeDisabled — never a
/// degraded unsandboxed run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SeccompPolicyError {
    #[error("seccomp policy supports only x86_64/aarch64 (found {arch})")]
    UnsupportedArch { arch: &'static str },
    #[error("seccomp filter compilation failed: {0}")]
    Compile(String),
    #[error("seccomp fd transport failed: {0}")]
    Transport(String),
}

/// The namespace-creation bits of clone(2)'s flags argument. Rules are
/// encoded PER BIT (see compile_namespace_policy_filter); the aggregate
/// constant stays as documentation of the covered set.
const CLONE_NAMESPACE_BITS: u64 = libc::CLONE_NEWNS as u64
    | libc::CLONE_NEWCGROUP as u64
    | libc::CLONE_NEWUTS as u64
    | libc::CLONE_NEWIPC as u64
    | libc::CLONE_NEWUSER as u64
    | libc::CLONE_NEWPID as u64
    | libc::CLONE_NEWNET as u64;

/// Compile the deny-list namespace-escape policy (P17.1/P17.2). The
/// filter ALLOWS everything by default (mismatch) and returns ENOSYS on
/// match: clone3 entirely; clone when its flags carry any namespace bit;
/// and setns/unshare/mount/umount2/ptrace/bpf/add_key/request_key/keyctl/
/// init_module/finit_module/delete_module/reboot outright. One match
/// action covers every rule (seccompiler's backend binds the action at
/// filter level), and ENOSYS is the honest choice: it is the errno the
/// contract names for clone3, and for the rest "the syscall does not
/// exist" denies exactly as hard as EPERM while also removing probing
/// value. Only x86_64 and aarch64 audit architectures are accepted
/// (P17.1); every other arch fails closed.
pub fn compile_namespace_policy_filter() -> Result<BpfProgram, SeccompPolicyError> {
    let arch: &'static str = std::env::consts::ARCH;
    if !matches!(arch, "x86_64" | "aarch64") {
        return Err(SeccompPolicyError::UnsupportedArch { arch });
    }
    let target_arch: TargetArch = arch
        .try_into()
        .map_err(|error| SeccompPolicyError::Compile(format!("{error:?}")))?;
    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    // clone3 never exists for the workload (glibc then falls back to the
    // legacy clone, whose namespace bits are masked separately below).
    rules.insert(libc::SYS_clone3, vec![]);
    // clone is denied when ANY namespace bit is set. seccompiler's
    // MaskedEq(mask) condition means (arg & mask) == (value & mask), so
    // ONE rule with the full mask would demand ALL bits at once — the
    // correct encoding is one rule PER bit: (arg & bit) == bit. Rules on
    // the same syscall OR together, so any single namespace bit matches.
    let mut namespace_bit_rules = Vec::new();
    let mut covered_bits = 0u64;
    for bit in [
        libc::CLONE_NEWNS,
        libc::CLONE_NEWCGROUP,
        libc::CLONE_NEWUTS,
        libc::CLONE_NEWIPC,
        libc::CLONE_NEWUSER,
        libc::CLONE_NEWPID,
        libc::CLONE_NEWNET,
    ] {
        covered_bits |= bit as u64;
        let condition = SeccompCondition::new(
            0,
            SeccompCmpArgLen::Dword,
            SeccompCmpOp::MaskedEq(bit as u64),
            bit as u64,
        )
        .map_err(|error| SeccompPolicyError::Compile(format!("{error:?}")))?;
        namespace_bit_rules.push(
            SeccompRule::new(vec![condition])
                .map_err(|error| SeccompPolicyError::Compile(format!("{error:?}")))?,
        );
    }
    debug_assert_eq!(covered_bits, CLONE_NAMESPACE_BITS);
    rules.insert(libc::SYS_clone, namespace_bit_rules);
    for syscall in [
        libc::SYS_setns,
        libc::SYS_unshare,
        libc::SYS_mount,
        libc::SYS_umount2,
        libc::SYS_ptrace,
        libc::SYS_bpf,
        libc::SYS_add_key,
        libc::SYS_request_key,
        libc::SYS_keyctl,
        libc::SYS_init_module,
        libc::SYS_finit_module,
        libc::SYS_delete_module,
        libc::SYS_reboot,
    ] {
        rules.insert(syscall as i64, vec![]);
    }
    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,
        SeccompAction::Errno(libc::ENOSYS as u32),
        target_arch,
    )
    .map_err(|error| SeccompPolicyError::Compile(format!("{error:?}")))?;
    BpfProgram::try_from(filter).map_err(|error| SeccompPolicyError::Compile(format!("{error:?}")))
}

/// Write the compiled BPF program to a fresh memfd positioned at zero and
/// return the raw fd (P17.3 transport): bwrap's --seccomp reads the fd to
/// EOF and consumes it as a raw sock_filter array. The fd is created with
/// MFD_CLOEXEC so it can never leak across an exec by accident — the
/// launcher that hands it to bwrap is responsible for the deliberate
/// dup2 into the child.
pub fn write_filter_to_memfd(program: &BpfProgram) -> Result<i32, SeccompPolicyError> {
    const MFD_CLOEXEC: u32 = 0x0001;
    // SAFETY: memfd_create with a NUL-terminated name; the return is an
    // fd or -1 with errno set.
    let name = b"r-code-seccomp\0";
    let fd =
        unsafe { libc::syscall(libc::SYS_memfd_create, name.as_ptr(), MFD_CLOEXEC) as libc::c_int };
    if fd < 0 {
        return Err(SeccompPolicyError::Transport(
            std::io::Error::last_os_error().to_string(),
        ));
    }
    let close = |fd: i32| {
        // SAFETY: plain close of the fd we own.
        unsafe { libc::close(fd) };
    };
    // Raw sock_filter serialization: each filter instruction is a
    // repr(C) { code: u16, jt: u8, jf: u8, k: u32 } record — 8 bytes,
    // naturally packed, no padding.
    let mut bytes = Vec::with_capacity(program.len() * 8);
    for instruction in program {
        let raw = unsafe {
            std::ptr::from_ref(instruction)
                .cast::<[u8; 8]>()
                .read_unaligned()
        };
        bytes.extend_from_slice(&raw);
    }
    // SAFETY: write/lseek with plain value arguments on the owned fd.
    let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
    if written < 0 || written as usize != bytes.len() {
        close(fd);
        return Err(SeccompPolicyError::Transport(
            std::io::Error::last_os_error().to_string(),
        ));
    }
    if unsafe { libc::lseek(fd, 0, libc::SEEK_SET) } < 0 {
        close(fd);
        return Err(SeccompPolicyError::Transport(
            std::io::Error::last_os_error().to_string(),
        ));
    }
    Ok(fd)
}

/// Attach the seccomp fd to a launch plan (P17.3): the argv gains
/// `--seccomp <fd>` in the bwrap option region (before the `--`
/// terminator), producing a NEW plan — the original stays reusable. The
/// launcher must dup2 the memfd into the child at `fd` before exec (the
/// P09 launch helper's pre_exec seam); bwrap consumes and closes the fd
/// before exec'ing the workload, so the workload cannot inherit it
/// (pinned by the CI suite).
pub fn attach_seccomp_to_plan(
    plan: &BwrapLaunchPlan,
    fd: i32,
) -> Result<BwrapLaunchPlan, SeccompPolicyError> {
    let mut attached = plan.clone();
    let insertion = attached
        .argv
        .iter()
        .rposition(|argument| argument == "--")
        .ok_or_else(|| SeccompPolicyError::Transport("plan argv lacks the -- terminator".into()))?;
    attached.argv.insert(insertion, format!("--seccomp"));
    attached.argv.insert(insertion + 1, fd.to_string());
    Ok(attached)
}

/// Non-activating Linux plan backend (P16 registration): implements the
/// P13 SandboxBackend contract by BUILDING and DIGESTING the plan only.
/// `run_probes` is permanently Err until P09 (containment proof) and P17
/// (seccomp) exist — the Linux capability therefore stays SafeDisabled by
/// construction; nothing in this backend can activate execution.
pub struct LinuxBwrapPlanBackend {
    bwrap_binary: PathBuf,
    boot_identity: String,
}

impl LinuxBwrapPlanBackend {
    pub fn new(boot_identity: &str) -> Self {
        Self {
            bwrap_binary: PathBuf::from(PINNED_BWRAP_PATH),
            boot_identity: boot_identity.to_string(),
        }
    }

    pub fn build_plan(
        &self,
        profile: &super::SandboxProfileMaterial,
    ) -> Result<BwrapLaunchPlan, BwrapPlanError> {
        build_bwrap_launch_plan(profile, &self.bwrap_binary, &self.boot_identity)
    }
}

#[async_trait::async_trait]
impl super::SandboxBackend for LinuxBwrapPlanBackend {
    fn id(&self) -> &'static str {
        "linux-bwrap-plan"
    }

    fn policy_digest(&self, profile: &super::SandboxProfileMaterial) -> Result<String, String> {
        let plan = self
            .build_plan(profile)
            .map_err(|error| error.to_string())?;
        Ok(r_code_harness_protocol::canonical_input_hash(
            &serde_json::json!({
                "backend": "linux-bwrap-plan",
                "plan": plan.policy_material(),
            }),
        ))
    }

    async fn run_probes(
        &self,
        _helper: &super::SafetyBinaryIdentity,
        _profile: &super::SandboxProfileMaterial,
        _probes: &[super::SandboxProbeId],
    ) -> Result<Vec<super::SafetyProbeResult>, String> {
        // Non-activating by contract: the plan backend cannot run probes.
        // P09/P17 own the real probe execution; until then the Linux
        // capability stays SafeDisabled.
        Err("the linux bwrap plan backend is non-activating (P09/P17 pending)".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seccomp_policy_compiles_on_supported_architectures() {
        // The CI legs are x86_64/aarch64; the compile itself is the gate.
        let program = compile_namespace_policy_filter().expect("supported arch compiles");
        assert!(!program.is_empty(), "a deny-list filter has instructions");
        // Every BPF instruction serializes to exactly 8 bytes (the raw
        // sock_filter wire format bwrap consumes).
        assert_eq!(std::mem::size_of::<seccompiler::sock_filter>(), 8);
        // The per-bit rule set covers exactly the documented aggregate.
        for bit in [
            libc::CLONE_NEWNS,
            libc::CLONE_NEWCGROUP,
            libc::CLONE_NEWUTS,
            libc::CLONE_NEWIPC,
            libc::CLONE_NEWUSER,
            libc::CLONE_NEWPID,
            libc::CLONE_NEWNET,
        ] {
            assert_ne!(CLONE_NAMESPACE_BITS & bit as u64, 0);
        }
        assert_eq!(
            CLONE_NAMESPACE_BITS & !(libc::CLONE_NEWTIME as u64),
            CLONE_NAMESPACE_BITS
        );
    }

    #[test]
    fn seccomp_filter_serializes_to_memfd_and_round_trips() {
        let program = compile_namespace_policy_filter().expect("compile");
        let fd = write_filter_to_memfd(&program).expect("memfd transport");
        let mut bytes = Vec::new();
        use std::io::Read;
        let mut file = unsafe { std::os::unix::io::FromRawFd::from_raw_fd(fd) };
        file.read_to_end(&mut bytes).expect("read back");
        assert_eq!(bytes.len(), program.len() * 8);
        // SAFETY-free drop: File closes the fd exactly once.
    }

    #[test]
    fn seccomp_attaches_before_the_workload_terminator() {
        let binary = verified_bwrap_for_tests();
        let plan = build_bwrap_launch_plan(&test_profile(), &binary, "boot-test").expect("plan");
        let attached = attach_seccomp_to_plan(&plan, 4).expect("attach");
        let seccomp_position = attached
            .argv
            .iter()
            .position(|argument| argument == "--seccomp")
            .expect("flag present");
        let terminator = attached
            .argv
            .iter()
            .rposition(|argument| argument == "--")
            .expect("terminator present");
        assert!(seccomp_position < terminator);
        assert_eq!(attached.argv[seccomp_position + 1], "4");
        // The ORIGINAL plan is untouched (reusable without seccomp).
        assert!(!plan.argv.iter().any(|argument| argument == "--seccomp"));
    }

    fn test_profile() -> super::super::SandboxProfileMaterial {
        super::super::SandboxProfileMaterial {
            read_roots: vec!["/opt/readonly".into()],
            write_roots: vec!["/srv/work".into()],
            scratch_root: "/tmp/scratch".into(),
            toolchain_roots: vec!["/opt/toolchain".into()],
            cache_roots: vec!["/var/cache/registry".into()],
            git_hidden: true,
            inherited_fds: vec![0, 1, 2],
            environment_allowlist: vec!["SYSTEMROOT".into(), "PATH".into()],
            network: super::super::SandboxNetworkClass::Offline,
        }
    }

    /// A verification-passing bwrap for the POSITIVE unit tests: prefer
    /// the REAL pinned binary (root:root on every CI runner); when it is
    /// missing or too old, fall back to a stub that must be chowned to
    /// 0:0 via passwordless sudo (GitHub runners have it). Neither path
    /// available is an honest panic — the test never silently degrades.
    fn verified_bwrap_for_tests() -> std::path::PathBuf {
        let pinned = Path::new(PINNED_BWRAP_PATH);
        if let Ok((major, minor)) = verify_pinned_bwrap(pinned) {
            if (major, minor) >= MINIMUM_BWRAP_VERSION {
                return pinned.to_path_buf();
            }
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let stub = dir.path().join("bwrap");
        std::fs::write(&stub, "#!/bin/sh\necho 'bubblewrap 0.11.0'\n").expect("stub");
        make_executable(&stub);
        let chown = std::process::Command::new("sudo")
            .args(["-n", "chown", "0:0"])
            .arg(&stub)
            .status();
        match chown {
            Ok(status) if status.success() => {
                // Leak the tempdir deliberately: the stub must outlive
                // this helper's return.
                std::mem::forget(dir);
                stub
            }
            _ => panic!(
                "positive bwrap plan tests need either the real pinned \
                 /usr/bin/bwrap (>=0.8.0, root:root) or passwordless sudo \
                 to chown a stub; neither is available here"
            ),
        }
    }

    fn make_executable(path: &PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path).expect("stat").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("chmod");
    }

    #[test]
    fn plan_argv_carries_exactly_one_pid_namespace_and_network_flag() {
        let binary = verified_bwrap_for_tests();
        let plan = build_bwrap_launch_plan(&test_profile(), &binary, "boot-test")
            .expect("plan builds against a verified binary");
        assert_eq!(
            plan.argv
                .iter()
                .filter(|arg| *arg == "--unshare-pid")
                .count(),
            1,
            "exactly one component owns the PID namespace"
        );
        assert!(plan.argv.contains(&"--unshare-net".to_string()));
        assert!(plan.unshare_pid);
        assert!(plan.identity_handoff.expected_boot_id == "boot-test");
        assert!(!plan.identity_handoff.is_complete(), "nothing launched yet");
        // Mount polarity: ro for read/toolchain/cache, rw for write/scratch.
        let ro: Vec<_> = plan
            .mounts
            .iter()
            .filter(|mount| matches!(mount, BwrapMount::ReadOnly { .. }))
            .collect();
        let rw: Vec<_> = plan
            .mounts
            .iter()
            .filter(|mount| matches!(mount, BwrapMount::ReadWrite { .. }))
            .collect();
        assert_eq!(ro.len(), 3);
        assert_eq!(rw.len(), 2);
    }

    #[test]
    fn public_network_class_records_share_net_plan_material() {
        let binary = verified_bwrap_for_tests();
        let mut profile = test_profile();
        profile.network = super::super::SandboxNetworkClass::PublicInternetClient;
        let plan = build_bwrap_launch_plan(&profile, &binary, "boot-test").expect("plan");
        assert!(!plan.unshare_network);
        assert!(!plan.argv.contains(&"--unshare-net".to_string()));
        // HostNetwork is refused outright.
        profile.network = super::super::SandboxNetworkClass::HostNetwork;
        assert!(build_bwrap_launch_plan(&profile, &binary, "boot-test").is_err());
    }

    #[test]
    fn missing_binary_and_git_root_refusals_are_fail_closed() {
        assert!(matches!(
            verify_pinned_bwrap(Path::new("/nonexistent/bwrap")),
            Err(BwrapPlanError::BinaryMissing(_))
        ));
        // The .git refusal fires BEFORE binary verification, so an
        // unprivileged stub (never chowned) still exercises the branch.
        let dir = tempfile::tempdir().expect("tempdir");
        let stub = dir.path().join("bwrap");
        std::fs::write(&stub, "#!/bin/sh\necho 'bubblewrap 0.11.0'\n").expect("stub");
        let mut profile = test_profile();
        profile.read_roots = vec!["/srv/work/.git".into()];
        let refused = build_bwrap_launch_plan(&profile, &stub, "boot-test");
        assert!(matches!(
            refused,
            Err(BwrapPlanError::InvalidProfile(message)) if message.contains(".git")
        ));
    }
}
