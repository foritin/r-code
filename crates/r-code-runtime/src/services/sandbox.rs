//! P12 — runtime-consumed SafetyCapabilityReport material: the
//! content-addressed identity of one platform safety stack plus the
//! fail-closed activation predicate that consumes it. No signature is
//! claimed — safety rests on content addressing, the local store and the
//! predicate's exact-match rule. Nothing here probes the platform yet
//! (P13 owns the probe protocol); a report with no probe evidence can
//! never evaluate to Activated.

use r_code_store::v1::safety::SafetyReportRecord;
use serde::{Deserialize, Serialize};

/// Windows ACL journaling primitives (P14): capture/apply/readback/CAS
/// restore of security descriptors. Compiled only on Windows; the macOS
/// and Linux sandbox backends keep their own modules later.
#[cfg(windows)]
pub mod windows;

/// Non-activating pinned bwrap launch-plan builder (P16), Linux only:
/// verifies the immutable binary and builds one user/mount/pid-namespace
/// plan with identity-handoff slots. Windows hosts never compile it.
#[cfg(target_os = "linux")]
pub mod linux;

/// macOS Seatbelt profiles and diagnostics (P18): literal deny-default
/// SBPL for the NoWorkspaceSingleProcess class (one initial Harness
/// exec, immutable runtime reads, fork denied), write classes refused.
/// Non-activating — profile building and digesting only. Windows and
/// Linux hosts never compile it.
#[cfg(target_os = "macos")]
pub mod macos;

/// Bump when the material shape changes; old persisted reports then fail
/// the material-version check and never activate.
pub const SAFETY_REPORT_MATERIAL_VERSION: u16 = 1;

/// The capability a report speaks for. One report per capability; this
/// wave only write execution is gated (O-GATE/release consume it).
pub const SAFETY_CAPABILITY_WRITE_EXECUTION: &str = "write-execution";

/// Identity of one binary input to the safety stack (probe helper or the
/// executable the capability asserts about): path plus content digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyBinaryIdentity {
    pub path: String,
    pub sha256: String,
}

/// One probe outcome inside report material (P13 runs real probes; P12
/// defines the shape). `detail_digest` commits the probe's evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyProbeResult {
    pub probe_id: String,
    pub passed: bool,
    pub detail_digest: String,
}

/// Rich report status. `SafeDisabled` and `Unsupported` are visible but
/// never activate; `Activated` additionally requires the exact current
/// material plus complete passing probes at evaluation time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
pub enum SafetyStatus {
    Unsupported { reason: String },
    SafeDisabled { reason: String },
    Activated,
}

impl SafetyStatus {
    /// The coarse persisted column value.
    pub fn as_persisted(&self) -> &'static str {
        match self {
            Self::Unsupported { .. } => "unsupported",
            Self::SafeDisabled { .. } => "safe-disabled",
            Self::Activated => "activated",
        }
    }
}

/// Full report material (P12.1): every identity input whose change must
/// invalidate a persisted report — OS, arch, boot identity, backend,
/// backend policy digest, helper/executable identity and probe material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyReportMaterial {
    pub material_version: u16,
    pub capability: String,
    pub os: String,
    pub arch: String,
    pub boot_identity: String,
    pub backend: String,
    pub backend_policy_digest: String,
    pub helper: Option<SafetyBinaryIdentity>,
    pub executable: Option<SafetyBinaryIdentity>,
    pub probes: Vec<SafetyProbeResult>,
    pub status: SafetyStatus,
    /// Wall-clock stamp; excluded from the digest so regenerating the same
    /// inputs under the same identity is byte-idempotent.
    pub generated_at_ms: i64,
}

impl SafetyReportMaterial {
    /// The canonical, timestamp-free identity JSON the digest commits.
    pub fn identity_json(&self) -> serde_json::Value {
        serde_json::json!({
            "materialVersion": self.material_version,
            "capability": self.capability,
            "os": self.os,
            "arch": self.arch,
            "bootIdentity": self.boot_identity,
            "backend": self.backend,
            "backendPolicyDigest": self.backend_policy_digest,
            "helper": self.helper,
            "executable": self.executable,
            "probes": self.probes,
            "status": self.status,
        })
    }

    pub fn digest(&self) -> String {
        r_code_harness_protocol::canonical_input_hash(&self.identity_json())
    }

    /// Content-addressed report id (`safety-<digest>`).
    pub fn report_id(&self) -> String {
        format!("safety-{}", self.digest())
    }

    /// Structural completeness (P12.1): any empty identity input makes the
    /// material unusable — an incomplete report can never activate.
    pub fn validate(&self) -> Result<(), &'static str> {
        let binary_ok = |identity: &Option<SafetyBinaryIdentity>| {
            identity.as_ref().is_none_or(|binary| {
                !binary.path.trim().is_empty() && !binary.sha256.trim().is_empty()
            })
        };
        let probe_ids = self
            .probes
            .iter()
            .map(|probe| probe.probe_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        if self.material_version != SAFETY_REPORT_MATERIAL_VERSION
            || self.capability.trim().is_empty()
            || self.os.trim().is_empty()
            || self.arch.trim().is_empty()
            || self.boot_identity.trim().is_empty()
            || self.backend.trim().is_empty()
            || self.backend_policy_digest.trim().is_empty()
            || !binary_ok(&self.helper)
            || !binary_ok(&self.executable)
            || probe_ids.len() != self.probes.len()
            || probe_ids.contains("")
        {
            return Err("safety report material is incomplete");
        }
        Ok(())
    }
}

/// The activation verdict of one persisted report against the current
/// runtime material (P12.2/P12.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafetyActivation {
    Activated {
        report_id: String,
    },
    NotActivated {
        reason: &'static str,
        status: Option<String>,
    },
}

/// Fail-closed activation predicate: a persisted report may yield
/// `Activated` ONLY when its stored digest matches its own material
/// (tamper check), its report id matches that digest, its material equals
/// the CURRENT runtime material on every identity input, its status is
/// `Activated`, and every recorded probe passed with at least one probe.
/// Everything else — missing, tampered, stale boot, foreign platform,
/// changed policy/helper/executable, SafeDisabled, Unsupported, hollow
/// probes — is `NotActivated`. `SafeDisabled`/`Unsupported` reports that
/// match the current platform still pass the identity checks (they are
/// portable diagnostics) but never activate (P12.3).
pub fn evaluate_safety_activation(
    persisted: Option<&SafetyReportRecord>,
    current: &SafetyReportMaterial,
) -> SafetyActivation {
    let Some(record) = persisted else {
        return not_activated("report-missing", None);
    };
    let material = match serde_json::from_str::<SafetyReportMaterial>(&record.material_json) {
        Ok(material) => material,
        Err(_) => return not_activated("material-unparseable", None),
    };
    if material.digest() != record.material_digest {
        return not_activated("digest-mismatch", None);
    }
    if material.report_id() != record.report_id {
        return not_activated("report-id-mismatch", None);
    }
    if material.capability != record.capability {
        return not_activated("capability-mismatch", None);
    }
    if let Err(reason) = material.validate() {
        return not_activated(reason, None);
    }
    // Ordered identity checks produce the most specific staleness reason.
    let identity_mismatch = if material.boot_identity != current.boot_identity {
        Some("boot-identity-changed")
    } else if material.os != current.os || material.arch != current.arch {
        Some("platform-mismatch")
    } else if material.backend != current.backend {
        Some("backend-changed")
    } else if material.backend_policy_digest != current.backend_policy_digest {
        Some("policy-changed")
    } else if material.helper != current.helper {
        Some("helper-changed")
    } else if material.executable != current.executable {
        Some("executable-changed")
    } else if material.probes != current.probes {
        Some("probes-changed")
    } else {
        None
    };
    let status_label = Some(material.status.as_persisted().to_string());
    if let Some(reason) = identity_mismatch {
        return not_activated(reason, status_label);
    }
    match material.status {
        SafetyStatus::Activated => {
            if material.probes.is_empty() || material.probes.iter().any(|probe| !probe.passed) {
                not_activated("probes-incomplete", status_label)
            } else {
                SafetyActivation::Activated {
                    report_id: record.report_id.clone(),
                }
            }
        }
        SafetyStatus::SafeDisabled { .. } => not_activated("status-safe-disabled", status_label),
        SafetyStatus::Unsupported { .. } => not_activated("status-unsupported", status_label),
    }
}

fn not_activated(reason: &'static str, status: Option<String>) -> SafetyActivation {
    SafetyActivation::NotActivated { reason, status }
}

/// Persist a freshly built report and invalidate stale rows (P12.2):
/// idempotent by construction — the same material yields the same report
/// id, replaces its own row and prunes nothing. The store head ends at
/// the current report.
pub fn regenerate_safety_report(
    store: &r_code_store::v1::V1Store,
    material: &SafetyReportMaterial,
) -> Result<String, String> {
    material.validate()?;
    let record = SafetyReportRecord {
        report_id: material.report_id(),
        capability: material.capability.clone(),
        material_digest: material.digest(),
        status: match &material.status {
            SafetyStatus::Unsupported { .. } => {
                r_code_store::v1::safety::SafetyReportStatus::Unsupported
            }
            SafetyStatus::SafeDisabled { .. } => {
                r_code_store::v1::safety::SafetyReportStatus::SafeDisabled
            }
            SafetyStatus::Activated => r_code_store::v1::safety::SafetyReportStatus::Activated,
        },
        material_json: serde_json::to_string(material).map_err(|error| error.to_string())?,
        created_at_ms: material.generated_at_ms,
    };
    let report_id = record.report_id.clone();
    store.put_safety_report(record)?;
    store.prune_safety_reports(&report_id)?;
    Ok(report_id)
}

// P13 — sandbox profiles, native probe protocol and activation gates --------

/// Network classes are DISTINCT authority ceilings (P13.1): Offline has no
/// network at all; PublicInternetClient may open outbound client
/// connections to the public internet only; HostNetwork would see the
/// host's stack and is unsupported everywhere in v1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxNetworkClass {
    Offline,
    PublicInternetClient,
    HostNetwork,
}

/// Exact filesystem/FD/env/network material of one sandbox profile
/// (P13.1). Every path is absolute; the profile is the complete truth a
/// backend enforces — nothing outside these entries is grantable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxProfileMaterial {
    pub read_roots: Vec<String>,
    pub write_roots: Vec<String>,
    pub scratch_root: String,
    pub toolchain_roots: Vec<String>,
    pub cache_roots: Vec<String>,
    /// The .git directory is NEVER visible inside a sandbox (INV-06);
    /// validate() rejects a profile that claims otherwise.
    pub git_hidden: bool,
    /// The exact inherited descriptor set (superset is a violation).
    pub inherited_fds: Vec<i32>,
    /// The exact environment key allowlist.
    pub environment_allowlist: Vec<String>,
    pub network: SandboxNetworkClass,
}

impl SandboxProfileMaterial {
    /// Canonical policy digest committed into the report material (P13.3):
    /// the backend policy and the profile it enforces change identity
    /// together.
    pub fn policy_digest(&self) -> String {
        r_code_harness_protocol::canonical_input_hash(&serde_json::json!({
            "readRoots": self.read_roots,
            "writeRoots": self.write_roots,
            "scratchRoot": self.scratch_root,
            "toolchainRoots": self.toolchain_roots,
            "cacheRoots": self.cache_roots,
            "gitHidden": self.git_hidden,
            "inheritedFds": self.inherited_fds,
            "environmentAllowlist": self.environment_allowlist,
            "network": self.network,
        }))
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        let unique_nonempty = |values: &[String]| {
            let set = values.iter().collect::<std::collections::BTreeSet<_>>();
            !values.is_empty()
                && set.len() == values.len()
                && values.iter().all(|value| {
                    value.starts_with('/') || value.starts_with(r"\\") || {
                        let bytes = value.as_bytes();
                        bytes.len() >= 3
                            && bytes[1] == b':'
                            && (bytes[2] == b'/' || bytes[2] == b'\\')
                    }
                })
        };
        if !unique_nonempty(&self.read_roots)
            || !unique_nonempty(&self.write_roots)
            || self.scratch_root.trim().is_empty()
            || !unique_nonempty(&self.toolchain_roots)
            || !unique_nonempty(&self.cache_roots)
            || !self.git_hidden
            || self
                .inherited_fds
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.inherited_fds.len()
            || self
                .environment_allowlist
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != self.environment_allowlist.len()
            || self
                .environment_allowlist
                .iter()
                .any(|key| key != &key.to_ascii_uppercase())
        {
            return Err("sandbox profile material is incomplete");
        }
        Ok(())
    }
}

/// The deny-probe set (P13.2). Every probe ATTEMPTS a forbidden operation
/// under the sandbox: the only passing outcome is an observed denial. A
/// probe that succeeds, errors ambiguously or goes missing fails the
/// suite — omission can never satisfy activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SandboxProbeId {
    WriteOutsideAllowlist,
    DotGitAccess,
    NetworkOutbound,
    RegistryOrKeychain,
    DeviceAccess,
    IpcAccess,
    ChildEscape,
    EnvironmentLeakage,
}

impl SandboxProbeId {
    pub const ALL: &'static [SandboxProbeId] = &[
        SandboxProbeId::WriteOutsideAllowlist,
        SandboxProbeId::DotGitAccess,
        SandboxProbeId::NetworkOutbound,
        SandboxProbeId::RegistryOrKeychain,
        SandboxProbeId::DeviceAccess,
        SandboxProbeId::IpcAccess,
        SandboxProbeId::ChildEscape,
        SandboxProbeId::EnvironmentLeakage,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::WriteOutsideAllowlist => "write-outside-allowlist",
            Self::DotGitAccess => "dot-git-access",
            Self::NetworkOutbound => "network-outbound",
            Self::RegistryOrKeychain => "registry-or-keychain",
            Self::DeviceAccess => "device-access",
            Self::IpcAccess => "ipc-access",
            Self::ChildEscape => "child-escape",
            Self::EnvironmentLeakage => "environment-leakage",
        }
    }
}

/// P13.3 status derivation from one probe suite: `Activated` requires a
/// real backend that ran every required probe exactly once, all passing.
/// Missing, duplicated, failed or ambiguous probes — or no backend at
/// all — keep the capability SafeDisabled/Unsupported. No OS-name or
/// binary-presence inference happens anywhere in this decision.
pub fn status_from_probes(
    backend_available: bool,
    required: &[SandboxProbeId],
    results: &[SafetyProbeResult],
) -> SafetyStatus {
    if !backend_available {
        return SafetyStatus::Unsupported {
            reason: "no native sandbox backend is available on this platform".into(),
        };
    }
    let mut failed: Vec<String> = Vec::new();
    for probe in required {
        let matches: Vec<&SafetyProbeResult> = results
            .iter()
            .filter(|result| result.probe_id == probe.as_str())
            .collect();
        match matches.as_slice() {
            [single] if single.passed => {}
            [single] => failed.push(single.probe_id.clone()),
            // Zero occurrences (omitted) or duplicates (ambiguous) both
            // fail the suite — never satisfy activation with silence.
            _ => failed.push(probe.as_str().to_string()),
        }
    }
    if failed.is_empty() {
        SafetyStatus::Activated
    } else {
        SafetyStatus::SafeDisabled {
            reason: format!("probes-failed: {}", failed.join(",")),
        }
    }
}

/// Sandbox backend contract (P13): builds and enforces one profile, and
/// runs the deny-probe helper under it. Real backends arrive with
/// P15/P16/P18; until then no production backend exists on any platform
/// and the platform gate stays honestly closed.
#[async_trait::async_trait]
pub trait SandboxBackend: Send + Sync {
    /// Stable backend identity committed into report material.
    fn id(&self) -> &'static str;

    /// Digest of the exact policy this backend would enforce for the
    /// profile (binds policy into the report, P13.3).
    fn policy_digest(&self, profile: &SandboxProfileMaterial) -> Result<String, String>;

    /// Run every requested probe under this backend and profile and
    /// return one result per probe. A backend that cannot run must
    /// Err — partial results never reach a report.
    async fn run_probes(
        &self,
        helper: &SafetyBinaryIdentity,
        profile: &SandboxProfileMaterial,
        probes: &[SandboxProbeId],
    ) -> Result<Vec<SafetyProbeResult>, String>;
}

/// Deterministic fake backend (P13 test seam, the P04 fake precedent):
/// outcomes are configured per probe so tests can drive every branch of
/// [`status_from_probes`] and the activation predicate. It performs NO
/// real sandboxing and is never selected by any production path —
/// production backends are constructed by platform code that does not
/// exist in this wave.
#[derive(Default)]
pub struct DeterministicFakeSandboxBackend {
    outcomes: std::collections::BTreeMap<&'static str, bool>,
    policy_digest: Option<String>,
}

impl DeterministicFakeSandboxBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Configure the outcome of one probe id.
    pub fn set_outcome(&mut self, probe: SandboxProbeId, passed: bool) {
        self.outcomes.insert(probe.as_str(), passed);
    }

    pub fn set_policy_digest(&mut self, digest: String) {
        self.policy_digest = Some(digest);
    }
}

#[async_trait::async_trait]
impl SandboxBackend for DeterministicFakeSandboxBackend {
    fn id(&self) -> &'static str {
        "fake"
    }

    fn policy_digest(&self, profile: &SandboxProfileMaterial) -> Result<String, String> {
        Ok(self
            .policy_digest
            .clone()
            .unwrap_or_else(|| profile.policy_digest()))
    }

    async fn run_probes(
        &self,
        _helper: &SafetyBinaryIdentity,
        _profile: &SandboxProfileMaterial,
        probes: &[SandboxProbeId],
    ) -> Result<Vec<SafetyProbeResult>, String> {
        Ok(probes
            .iter()
            .map(|probe| SafetyProbeResult {
                probe_id: probe.as_str().to_string(),
                passed: *self.outcomes.get(probe.as_str()).unwrap_or(&false),
                detail_digest: "fake".to_string(),
            })
            .collect())
    }
}

/// The current-platform report material for the discovery gate. Static
/// platform knowledge only, in this wave: no production sandbox backend
/// exists anywhere (P15 Windows AppContainer, P16/P09/P17 Linux, P18
/// macOS Seatbelt land them), so the honest status is Unsupported and no
/// probe is claimed. The probe helper binary and its digest become inputs
/// when real backends run it (P13.3 seam: helper is already a material
/// field). Regenerating this material is idempotent (the digest excludes
/// the timestamp).
pub fn current_platform_material(boot_identity: &str) -> SafetyReportMaterial {
    SafetyReportMaterial {
        material_version: SAFETY_REPORT_MATERIAL_VERSION,
        capability: SAFETY_CAPABILITY_WRITE_EXECUTION.to_string(),
        os: std::env::consts::OS.to_string(),
        arch: std::env::consts::ARCH.to_string(),
        boot_identity: boot_identity.to_string(),
        backend: "none-this-wave".to_string(),
        backend_policy_digest: r_code_harness_protocol::canonical_input_hash(
            &serde_json::json!({"backend": "none-this-wave"}),
        ),
        helper: None,
        executable: None,
        probes: Vec::new(),
        status: SafetyStatus::Unsupported {
            reason: "no native sandbox backend is available in this wave".into(),
        },
        generated_at_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as i64)
            .unwrap_or(0),
    }
}

/// P13 discovery gate: regenerate the current platform report (idempotent)
/// and evaluate the persisted head against it — the ONLY input the router
/// and run manager may consult for effect-service availability. Failures
/// anywhere in the pipeline fall through to NotActivated; an exact
/// Activated report is required to open anything.
pub fn platform_activation_gate(
    store: &r_code_store::v1::V1Store,
    boot_identity: &str,
) -> SafetyActivation {
    let material = current_platform_material(boot_identity);
    if material.validate().is_ok() {
        // Best-effort persistence: the gate's verdict never depends on the
        // write succeeding — evaluation below is the authority.
        let _ = regenerate_safety_report(store, &material);
    }
    let persisted = store
        .current_safety_report(&material.capability)
        .ok()
        .flatten();
    evaluate_safety_activation(persisted.as_ref(), &material)
}
