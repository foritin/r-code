//! Plugin identity, manifest and capability contract for harness plugins.
//!
//! This module owns the `harness.json` wire format (schema
//! `schema/harness-v1.schema.json`) and the pure capability negotiation that
//! must reject incompatible plugins *before* the host spawns any process.

use semver::Version;
use serde::{Deserialize, Serialize};

/// Current harness manifest schema version.
pub const MANIFEST_SCHEMA_VERSION: &str = "1";

/// The minimum apiMinor a package must declare to ask the host for the
/// single-process guarantee (P23.4): only a 1.3 host can enforce a declared
/// no-fork/no-exec child-process policy, so a package that requires the
/// guarantee cannot declare an older minor and stay compatible. The floor is
/// also the final Wave 3 host minor — [`WAVE3_HOST_API`] derives from it.
pub const SINGLE_PROCESS_MIN_API_MINOR: u32 = 3;

/// The final Wave 3 host API the runtime negotiates and advertises (P23.4).
/// `rpc::HOST_API_VERSION` is the protocol's declared single source of truth
/// and must carry this exact value; while it still names an older minor the
/// runtime catalog — the place that decides install eligibility — uses this
/// constant, so the two can never disagree about what a package may ask for.
pub const WAVE3_HOST_API: ApiVersion = ApiVersion::new(1, SINGLE_PROCESS_MIN_API_MINOR);

/// String harness identity. Identities are opaque to the host; equality is
/// exact string equality and they are never parsed for meaning.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(transparent)]
pub struct HarnessId(pub String);

impl HarnessId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn is_valid(&self) -> bool {
        !self.0.trim().is_empty()
            && self
                .0
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            && !self.0.starts_with('.')
            && !self.0.ends_with('.')
    }
}

impl std::fmt::Display for HarnessId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A pinned, immutable reference to an installed plugin package.
///
/// `content_digest` is the content-addressed digest of the installed package
/// bytes; runs pin all three fields so upgrades only ever apply to new runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageRef {
    pub id: HarnessId,
    pub version: Version,
    #[serde(rename = "contentDigest")]
    pub content_digest: String,
}

/// Host API version. Manifests declare the minimum minor they require for a
/// given major; hosts advertise what they actually implement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiVersion {
    pub major: u32,
    pub minor: u32,
}

impl ApiVersion {
    pub const fn new(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }

    /// True when a plugin declaring `self` as its required API runs on a host
    /// advertising `host`.
    pub fn is_supported_by(&self, host: &ApiVersion) -> bool {
        self.major == host.major && self.minor <= host.minor
    }
}

/// Platforms a harness package can run on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Platform {
    WindowsX64,
    MacosArm64,
    MacosX64,
    LinuxX64,
}

impl Platform {
    /// The platform the current host build targets, for use in tests and
    /// availability derivation.
    pub fn current() -> Self {
        #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
        {
            Platform::WindowsX64
        }
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            Platform::MacosArm64
        }
        #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
        {
            Platform::MacosX64
        }
        #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
        {
            Platform::LinuxX64
        }
        #[cfg(not(any(
            all(target_os = "windows", target_arch = "x86_64"),
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "macos", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "x86_64"),
        )))]
        {
            compile_error!("unsupported host platform for harness plugins");
        }
    }
}

/// Per-platform entrypoint. `executable` is package-relative and must not
/// escape the package root; `argv` are literal extra arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformEntrypoint {
    pub platform: Platform,
    pub executable: String,
    #[serde(default)]
    pub argv: Vec<String>,
}

/// Framing declared by a package-pinned process profile. `Raw` byte streams
/// are unavailable for restricted (read-only/Plan) task modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProcessFraming {
    /// Newline-delimited JSON-RPC 2.0 with declarative method/field checks.
    NdjsonRpc,
    /// Opaque byte stream; requires non-restricted authorization.
    Raw,
}

/// Optional declarative process-profile summary carried by the manifest. The
/// full constraint schema lives beside the package; the host only records the
/// names and framing here to derive capability. A manifest without profiles
/// grants no raw-process capability in restricted modes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessProfileDecl {
    pub name: String,
    pub framing: ProcessFraming,
}

/// Host services a plugin may request. Serde names match the JSON-RPC method
/// names used on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HostService {
    #[serde(rename = "host.model.stream")]
    ModelStream,
    #[serde(rename = "host.tools.list")]
    ToolsList,
    #[serde(rename = "host.tools.call")]
    ToolsCall,
    #[serde(rename = "host.process.open")]
    ProcessOpen,
    #[serde(rename = "host.process.read")]
    ProcessRead,
    #[serde(rename = "host.process.write")]
    ProcessWrite,
    #[serde(rename = "host.process.close")]
    ProcessClose,
    #[serde(rename = "host.context.read")]
    ContextRead,
    #[serde(rename = "host.artifacts.put")]
    ArtifactsPut,
    #[serde(rename = "host.artifacts.read")]
    ArtifactsRead,
    #[serde(rename = "host.plan.publish")]
    PlanPublish,
    #[serde(rename = "host.plan.update")]
    PlanUpdate,
    #[serde(rename = "host.questions.ask")]
    QuestionsAsk,
    #[serde(rename = "host.approvals.request")]
    ApprovalsRequest,
    #[serde(rename = "host.children.spawn")]
    ChildrenSpawn,
    #[serde(rename = "host.children.wait")]
    ChildrenWait,
    #[serde(rename = "host.children.cancel")]
    ChildrenCancel,
    #[serde(rename = "host.verification.run")]
    VerificationRun,
    #[serde(rename = "host.checkpoint.save")]
    CheckpointSave,
    #[serde(rename = "host.completion.propose")]
    CompletionPropose,
}

impl HostService {
    /// Every service a fully featured host offers.
    pub const ALL: &'static [HostService] = &[
        HostService::ModelStream,
        HostService::ToolsList,
        HostService::ToolsCall,
        HostService::ProcessOpen,
        HostService::ProcessRead,
        HostService::ProcessWrite,
        HostService::ProcessClose,
        HostService::ContextRead,
        HostService::ArtifactsPut,
        HostService::ArtifactsRead,
        HostService::PlanPublish,
        HostService::PlanUpdate,
        HostService::QuestionsAsk,
        HostService::ApprovalsRequest,
        HostService::ChildrenSpawn,
        HostService::ChildrenWait,
        HostService::ChildrenCancel,
        HostService::VerificationRun,
        HostService::CheckpointSave,
        HostService::CompletionPropose,
    ];

    pub fn wire_name(&self) -> &'static str {
        match self {
            HostService::ModelStream => "host.model.stream",
            HostService::ToolsList => "host.tools.list",
            HostService::ToolsCall => "host.tools.call",
            HostService::ProcessOpen => "host.process.open",
            HostService::ProcessRead => "host.process.read",
            HostService::ProcessWrite => "host.process.write",
            HostService::ProcessClose => "host.process.close",
            HostService::ContextRead => "host.context.read",
            HostService::ArtifactsPut => "host.artifacts.put",
            HostService::ArtifactsRead => "host.artifacts.read",
            HostService::PlanPublish => "host.plan.publish",
            HostService::PlanUpdate => "host.plan.update",
            HostService::QuestionsAsk => "host.questions.ask",
            HostService::ApprovalsRequest => "host.approvals.request",
            HostService::ChildrenSpawn => "host.children.spawn",
            HostService::ChildrenWait => "host.children.wait",
            HostService::ChildrenCancel => "host.children.cancel",
            HostService::VerificationRun => "host.verification.run",
            HostService::CheckpointSave => "host.checkpoint.save",
            HostService::CompletionPropose => "host.completion.propose",
        }
    }
}

/// The `harness.json` manifest of a plugin package.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarnessManifest {
    pub schema_version: String,
    pub id: HarnessId,
    pub version: Version,
    #[serde(rename = "apiMajor")]
    pub api_major: u32,
    #[serde(rename = "apiMinor")]
    pub api_minor: u32,
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(rename = "supportedPlatforms")]
    pub supported_platforms: Vec<PlatformEntrypoint>,
    #[serde(rename = "supportedFeatures", default)]
    pub supported_features: Vec<String>,
    #[serde(rename = "requestedHostServices", default)]
    pub requested_host_services: Vec<HostService>,
    #[serde(rename = "configSchema")]
    pub config_schema: serde_json::Value,
    #[serde(
        rename = "processProfiles",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub process_profiles: Vec<ProcessProfileDecl>,
    /// P19A (API v1.2): the package declares that it emits WorkUnit
    /// effect/network fields. A manifest carrying this flag MUST declare
    /// apiMinor >= 2 — an older-minor package cannot require effect
    /// fields and stay compatible (validated below and enforced again at
    /// negotiation).
    #[serde(
        rename = "requiresEffectFields",
        default,
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub requires_effect_fields: bool,
    /// P23.1/P23.4 (API v1.3): the package declares that it runs as ONE
    /// process — it never forks and never execs another image — and therefore
    /// requires the host's single-process guarantee. A manifest carrying this
    /// flag MUST declare apiMinor >= 3: no host below 1.3 can deny fork or
    /// exec, so requiring the guarantee there would be a promise the host
    /// cannot keep (refused by name in validate()). Absent (the default) is
    /// NOT a claim of innocence: it means "undeclared", and the transport
    /// enforces containment for it rather than trusting it. Serializing stays
    /// byte-identical for every pre-P23 package because the flag is skipped
    /// when false.
    #[serde(
        rename = "requiresSingleProcess",
        default,
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub requires_single_process: bool,
}

/// Errors produced by manifest validation and capability negotiation.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ManifestError {
    #[error("manifest schema_version {found:?} is unsupported (expected {expected:?})")]
    UnsupportedSchemaVersion { found: String, expected: String },
    #[error("harness id {0:?} is invalid")]
    InvalidHarnessId(String),
    #[error("manifest version is invalid: {0}")]
    InvalidVersion(String),
    #[error("api {declared:?} cannot require effect fields (needs 1.2+)")]
    EffectFieldsNeedNewerApi { declared: (u32, u32) },
    /// P23.4: the downgrade refusal. Named by harness id and by the declaration
    /// that asked for a guarantee this apiMinor cannot receive, so a packaging
    /// check can match the reason text exactly.
    #[error(
        "harness {id:?} requires the single-process guarantee but declares api {declared:?} (needs 1.3+)"
    )]
    SingleProcessNeedsNewerApi { id: String, declared: (u32, u32) },
    #[error("platform {platform:?} is not supported by this host (supports {supported:?})")]
    UnsupportedPlatform {
        platform: String,
        supported: Vec<String>,
    },
    #[error("no entrypoint for platform {0:?}")]
    MissingEntrypoint(String),
    #[error("entrypoint executable {0:?} escapes the package root")]
    EntrypointEscapesPackage(String),
    #[error(
        "plugin requires API {required_major}.{required_minor}, host provides {host_major}.{host_minor}"
    )]
    IncompatibleApi {
        required_major: u32,
        required_minor: u32,
        host_major: u32,
        host_minor: u32,
    },
    #[error("plugin requires host service {0} which the host does not offer")]
    UnsupportedService(String),
    #[error("duplicate harness id {0}")]
    DuplicateHarnessId(String),
    #[error("duplicate requested host service {0}")]
    DuplicateService(String),
    #[error("duplicate process profile name {0}")]
    DuplicateProcessProfile(String),
    #[error("config schema is not a JSON object")]
    InvalidConfigSchema,
}

impl HarnessManifest {
    /// Validate structural invariants that must hold before any install or
    /// spawn decision is made.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.schema_version != MANIFEST_SCHEMA_VERSION {
            return Err(ManifestError::UnsupportedSchemaVersion {
                found: self.schema_version.clone(),
                expected: MANIFEST_SCHEMA_VERSION.to_string(),
            });
        }
        // P19A.4: a package requiring effect/network WorkUnit fields
        // cannot declare an apiMinor older than 2.
        if self.requires_effect_fields && (self.api_major != 1 || self.api_minor < 2) {
            return Err(ManifestError::EffectFieldsNeedNewerApi {
                declared: (self.api_major, self.api_minor),
            });
        }
        // P23.4: the same explicit, additive rule one minor later. A package
        // that requires the single-process guarantee names itself and the
        // declaration that cannot be honoured; additive older packages that
        // never ask for the guarantee are untouched by this arm.
        if self.requires_single_process
            && (self.api_major != 1 || self.api_minor < SINGLE_PROCESS_MIN_API_MINOR)
        {
            return Err(ManifestError::SingleProcessNeedsNewerApi {
                id: self.id.0.clone(),
                declared: (self.api_major, self.api_minor),
            });
        }
        if !self.id.is_valid() {
            return Err(ManifestError::InvalidHarnessId(self.id.0.clone()));
        }
        if self.version.major == 0 && self.version.minor == 0 && self.version.patch == 0 {
            // semver::Version never parses an empty string, but a defaulted
            // 0.0.0 signals an unfilled field.
            if self.version.build.is_empty() && self.version.pre.is_empty() {
                // Still allow explicit 0.0.0 for fixtures; nothing to reject.
            }
        }
        for entry in &self.supported_platforms {
            let exe = entry.executable.replace('\\', "/");
            if exe
                .split('/')
                .any(|seg| seg == ".." || seg.is_empty() && exe.starts_with('/'))
            {
                return Err(ManifestError::EntrypointEscapesPackage(
                    entry.executable.clone(),
                ));
            }
            if exe.starts_with('/') || exe.contains(':') && exe.as_bytes().get(1) == Some(&b':') {
                return Err(ManifestError::EntrypointEscapesPackage(
                    entry.executable.clone(),
                ));
            }
        }
        for (idx, svc) in self.requested_host_services.iter().enumerate() {
            if self.requested_host_services[..idx].contains(svc) {
                return Err(ManifestError::DuplicateService(svc.wire_name().to_string()));
            }
        }
        for (idx, profile) in self.process_profiles.iter().enumerate() {
            if self.process_profiles[..idx]
                .iter()
                .any(|p| p.name == profile.name)
            {
                return Err(ManifestError::DuplicateProcessProfile(profile.name.clone()));
            }
        }
        if !self.config_schema.is_object() {
            return Err(ManifestError::InvalidConfigSchema);
        }
        Ok(())
    }

    /// Entrypoint declared for `platform`, if any.
    pub fn entrypoint_for(&self, platform: Platform) -> Option<&PlatformEntrypoint> {
        self.supported_platforms
            .iter()
            .find(|entry| entry.platform == platform)
    }

    /// Whether the manifest's raw-byte process profiles grant any capability
    /// usable in restricted (read-only/Plan) task modes. They never do.
    pub fn has_restricted_process_profiles(&self) -> bool {
        self.process_profiles
            .iter()
            .any(|p| p.framing == ProcessFraming::NdjsonRpc)
    }

    /// Negotiate capabilities against a host. Returns an error — before any
    /// process is spawned — when the contract is incompatible.
    pub fn negotiate(
        &self,
        host_api: ApiVersion,
        platform: Platform,
        host_services: &[HostService],
    ) -> Result<NegotiatedCapabilities, ManifestError> {
        self.validate()?;
        let plugin_api = ApiVersion::new(self.api_major, self.api_minor);
        if !plugin_api.is_supported_by(&host_api) {
            return Err(ManifestError::IncompatibleApi {
                required_major: self.api_major,
                required_minor: self.api_minor,
                host_major: host_api.major,
                host_minor: host_api.minor,
            });
        }
        if self.entrypoint_for(platform).is_none() {
            let supported = self
                .supported_platforms
                .iter()
                .map(|entry| format!("{:?}", entry.platform))
                .collect::<Vec<_>>();
            return Err(ManifestError::UnsupportedPlatform {
                platform: format!("{platform:?}"),
                supported,
            });
        }
        let required = self.requested_host_services.clone();
        let mut granted = Vec::new();
        for svc in &required {
            if !host_services.contains(svc) {
                return Err(ManifestError::UnsupportedService(
                    svc.wire_name().to_string(),
                ));
            }
            granted.push(*svc);
        }
        Ok(NegotiatedCapabilities {
            plugin_api,
            host_api,
            granted_services: granted,
        })
    }
}

/// The effective service set agreed between host and plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NegotiatedCapabilities {
    pub plugin_api: ApiVersion,
    pub host_api: ApiVersion,
    pub granted_services: Vec<HostService>,
}

impl NegotiatedCapabilities {
    pub fn grants(&self, service: HostService) -> bool {
        self.granted_services.contains(&service)
    }
}

/// Reject a set of manifests containing duplicate harness identities. Used at
/// install/catalog time so one identity can never map to two different
/// packages.
pub fn ensure_unique_ids(manifests: &[HarnessManifest]) -> Result<(), ManifestError> {
    for (idx, manifest) in manifests.iter().enumerate() {
        if manifests[..idx].iter().any(|m| m.id == manifest.id) {
            return Err(ManifestError::DuplicateHarnessId(manifest.id.0.clone()));
        }
    }
    Ok(())
}

/// Builder used by fixtures and built-in plugin packages in tests.
#[derive(Debug, Clone)]
pub struct HarnessManifestBuilder {
    manifest: HarnessManifest,
}

impl HarnessManifestBuilder {
    pub fn new(id: &str, version: &str) -> Self {
        Self {
            manifest: HarnessManifest {
                schema_version: MANIFEST_SCHEMA_VERSION.to_string(),
                id: HarnessId::new(id),
                version: Version::parse(version).expect("valid semver"),
                api_major: 1,
                api_minor: 0,
                display_name: id.to_string(),
                description: None,
                supported_platforms: Vec::new(),
                supported_features: Vec::new(),
                requested_host_services: Vec::new(),
                config_schema: serde_json::json!({}),
                process_profiles: Vec::new(),
                requires_effect_fields: false,
                requires_single_process: false,
            },
        }
    }

    pub fn api(mut self, major: u32, minor: u32) -> Self {
        self.manifest.api_major = major;
        self.manifest.api_minor = minor;
        self
    }

    /// P19A: declare that the package emits WorkUnit effect/network
    /// fields (requires apiMinor >= 2; the manifest builder chain
    /// enforces it at validate time).
    pub fn requires_effect_fields(mut self) -> Self {
        self.manifest.requires_effect_fields = true;
        self
    }

    /// P23.1/P23.4: declare that the package runs as one process and requires
    /// the host's single-process guarantee (requires apiMinor >= 3; enforced
    /// at validate time, refused by name below it).
    pub fn requires_single_process(mut self) -> Self {
        self.manifest.requires_single_process = true;
        self
    }

    pub fn display_name(mut self, name: &str) -> Self {
        self.manifest.display_name = name.to_string();
        self
    }

    pub fn entrypoint(mut self, platform: Platform, executable: &str, argv: &[&str]) -> Self {
        self.manifest.supported_platforms.push(PlatformEntrypoint {
            platform,
            executable: executable.to_string(),
            argv: argv.iter().map(|s| s.to_string()).collect(),
        });
        self
    }

    pub fn features(mut self, features: &[&str]) -> Self {
        self.manifest.supported_features = features.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn services(mut self, services: &[HostService]) -> Self {
        self.manifest.requested_host_services = services.to_vec();
        self
    }

    pub fn process_profile(mut self, name: &str, framing: ProcessFraming) -> Self {
        self.manifest.process_profiles.push(ProcessProfileDecl {
            name: name.to_string(),
            framing,
        });
        self
    }

    pub fn build(self) -> HarnessManifest {
        self.manifest
    }
}
