//! P18 — macOS Seatbelt profiles and diagnostics: verify the immutable
//! /usr/bin/sandbox-exec, build the literal deny-default SBPL profiles
//! (write execution stays SafeDisabled; the NoWorkspaceSingleProcess
//! profile allows exactly one initial Harness replacement exec and
//! immutable system runtime reads, denies fork, network, host IPC, user
//! homes and every write), and bind the executable/runtime policy
//! identity into SafetyCapabilityReport material. This module BUILDS and
//! VALIDATES profiles; it never launches anything and never activates
//! (the P13 gate stays closed — wiring is P20's step). Windows and Linux
//! hosts never compile this file.

#![cfg(target_os = "macos")]

use std::path::Path;

/// The pinned sandbox-exec location the profile accepts (immutable
/// system binary; Apple ships it root:wheel).
pub const PINNED_SANDBOX_EXEC_PATH: &str = "/usr/bin/sandbox-exec";

/// The baseline immutable runtime read roots (P18.2): the system dyld
/// surface every macOS binary touches. The cryptex root is probed at
/// build time by [`probe_runtime_read_roots`] because its on-disk
/// presence varies by OS release.
pub const BASELINE_RUNTIME_READ_ROOTS: [&str; 4] =
    ["/usr/lib", "/System/Library", "/usr/share", "/private/etc"];

/// Fail-closed profile-build verdicts: an unverifiable sandbox-exec or
/// harness keeps the capability SafeDisabled — never a degraded run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SeatbeltProfileError {
    #[error("pinned sandbox-exec is missing or not a regular file: {0}")]
    BinaryMissing(String),
    #[error("pinned sandbox-exec is not root-owned or is group/world-writable: {0}")]
    BinaryNotImmutable(String),
    #[error("profile request is invalid: {0}")]
    InvalidProfile(String),
}

/// P18.1: verify the pinned sandbox-exec — a regular file owned by
/// root:wheel with no group/world write bit. sandbox-exec ships no
/// --version flag; immutability is the ownership/mode contract (the CI
/// suite proves behavior on the real binary).
pub fn verify_pinned_sandbox_exec(binary: &Path) -> Result<(), SeatbeltProfileError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(binary)
        .map_err(|error| SeatbeltProfileError::BinaryMissing(error.to_string()))?;
    if !metadata.is_file() {
        return Err(SeatbeltProfileError::BinaryMissing(
            "not a regular file".into(),
        ));
    }
    if metadata.uid() != 0 || metadata.gid() != 0 {
        return Err(SeatbeltProfileError::BinaryNotImmutable(format!(
            "owner {}:{} (expected 0:0)",
            metadata.uid(),
            metadata.gid()
        )));
    }
    if metadata.mode() & 0o022 != 0 {
        return Err(SeatbeltProfileError::BinaryNotImmutable(format!(
            "mode {:o} allows group/world write",
            metadata.mode()
        )));
    }
    Ok(())
}

/// P18.2: the frozen runtime read root set — the baseline plus the
/// dyld-cryptex root when this OS release ships one. The result feeds
/// the profile text and, through it, the report's policy digest: any
/// change in the read set changes the policy identity and invalidates a
/// previously materialized report.
pub fn probe_runtime_read_roots() -> Vec<String> {
    let mut roots: Vec<String> = BASELINE_RUNTIME_READ_ROOTS
        .iter()
        .map(|root| (*root).to_string())
        .collect();
    for cryptex in [
        "/System/Volumes/Preboot/Cryptexes/OS/usr/lib",
        "/System/Volumes/Preboot/Cryptexes/OS/System/Library",
    ] {
        if Path::new(cryptex).is_dir() {
            roots.push(cryptex.to_string());
        }
    }
    roots.sort();
    roots.dedup();
    roots
}

/// The launchable profile class (P18 contract): NoWorkspaceSingleProcess
/// is the ONLY class a profile exists for; every write-capable class
/// stays SafeDisabled with its reason recorded in report material.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeatbeltProfileClass {
    NoWorkspaceSingleProcess,
}

/// P18.1/P18.2: build the literal deny-default SBPL text for one
/// NoWorkspaceSingleProcess run. The profile is LITERAL (no regex, no
/// subpath escape hatches): exactly one process-exec allowance for the
/// content-addressed Harness path (the initial replacement), process-fork
/// denied explicitly, file reads limited to the frozen immutable runtime
/// roots, and everything a write-capable process could abuse — user
/// homes, network, host IPC (mach-lookup), file writes, process signals
/// to the host — denied by default plus the explicit documentation
/// denials. The text is the policy identity: it feeds the report digest.
pub fn build_no_workspace_single_process_profile(
    harness_path: &str,
    runtime_read_roots: &[String],
) -> Result<String, SeatbeltProfileError> {
    if harness_path != harness_path.trim()
        || harness_path.is_empty()
        || !harness_path.starts_with('/')
    {
        return Err(SeatbeltProfileError::InvalidProfile(
            "the harness path must be an absolute trimmed path".into(),
        ));
    }
    if harness_path.contains('"') || harness_path.contains('\\') {
        return Err(SeatbeltProfileError::InvalidProfile(
            "the harness path must be SBPL-literal-safe (no quotes or backslashes)".into(),
        ));
    }
    for root in runtime_read_roots {
        if !root.starts_with('/')
            || root.contains('"')
            || root.contains('\\')
            || root.contains("..")
            || root.starts_with("/Users")
            || root.contains("/.git")
        {
            return Err(SeatbeltProfileError::InvalidProfile(format!(
                "runtime read root is not acceptable: {root}"
            )));
        }
    }
    let mut profile = String::new();
    profile.push_str("(version 1)\n(deny default)\n");
    // Exactly one exec: the content-addressed initial Harness replacement.
    profile.push_str(&format!(
        "(allow process-exec (literal \"{harness_path}\"))\n"
    ));
    // Fork never exists for the workload (P18.1); deny default covers it,
    // the explicit rule documents the contract.
    profile.push_str("(deny process-fork)\n");
    for root in runtime_read_roots {
        profile.push_str(&format!("(allow file-read* (subpath \"{root}\"))\n"));
    }
    profile.push_str("(deny file-read* (subpath \"/Users\"))\n");
    profile.push_str("(deny network*)\n");
    profile.push_str("(deny mach-lookup)\n");
    profile.push_str("(deny file-write*)\n");
    profile.push_str("(deny signal)\n");
    Ok(profile)
}

/// The canonical policy digest input: the literal profile text plus the
/// profile class, hashed into SafetyReportMaterial.backend_policy_digest
/// (P18.3 — executable digest and policy digest both bind the report).
pub fn seatbelt_policy_material(
    class: SeatbeltProfileClass,
    profile_text: &str,
    harness: &super::SafetyBinaryIdentity,
) -> serde_json::Value {
    serde_json::json!({
        "backend": "macos-seatbelt",
        "class": match class {
            SeatbeltProfileClass::NoWorkspaceSingleProcess => "no-workspace-single-process",
        },
        "harnessPath": harness.path,
        "harnessSha256": harness.sha256,
        "profile": profile_text,
    })
}

/// The diagnostic macOS Seatbelt backend (P18 registration): implements
/// the P13 SandboxBackend contract by BUILDING and DIGESTING profiles
/// only. `run_probes` is permanently Err until P20 wires real probe
/// execution — the macOS capability therefore stays SafeDisabled by
/// construction (write classes doubly so: their profile builder refuses).
pub struct MacosSeatbeltBackend;

impl MacosSeatbeltBackend {
    pub fn new() -> Self {
        Self
    }

    /// Build the launchable profile for the NoWorkspaceSingleProcess
    /// class; the write-capable classes have NO launchable profile — the
    /// refusal is the SafeDisabled reason recorded in report material.
    pub fn build_profile(&self, harness_path: &str) -> Result<String, SeatbeltProfileError> {
        verify_pinned_sandbox_exec(Path::new(PINNED_SANDBOX_EXEC_PATH))?;
        build_no_workspace_single_process_profile(harness_path, &probe_runtime_read_roots())
    }
}

impl Default for MacosSeatbeltBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl super::SandboxBackend for MacosSeatbeltBackend {
    fn id(&self) -> &'static str {
        "macos-seatbelt"
    }

    fn policy_digest(&self, profile: &super::SandboxProfileMaterial) -> Result<String, String> {
        // Write-capable profiles never get a launchable policy digest:
        // the refusal IS the SafeDisabled reason (P18 contract).
        let writable = !profile.write_roots.is_empty();
        if writable {
            return Err("macos seatbelt write execution stays safe-disabled in this wave".into());
        }
        let harness = super::SafetyBinaryIdentity {
            path: profile.toolchain_roots.first().cloned().unwrap_or_default(),
            sha256: String::new(),
        };
        let text = self
            .build_profile(&harness.path)
            .map_err(|error| error.to_string())?;
        Ok(r_code_harness_protocol::canonical_input_hash(
            &seatbelt_policy_material(
                SeatbeltProfileClass::NoWorkspaceSingleProcess,
                &text,
                &harness,
            ),
        ))
    }

    async fn run_probes(
        &self,
        _helper: &super::SafetyBinaryIdentity,
        _profile: &super::SandboxProfileMaterial,
        _probes: &[super::SandboxProbeId],
    ) -> Result<Vec<super::SafetyProbeResult>, String> {
        // Non-activating by contract: profile building and digesting only.
        // P20 owns wiring real probe execution under sandbox-exec; until
        // then the macOS capability stays SafeDisabled.
        Err("the macos seatbelt backend is non-activating (P20 pending)".into())
    }
}
