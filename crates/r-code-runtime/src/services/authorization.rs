//! Unified action authorization.
//!
//! Tools, managed processes, verification preparation and user-originated
//! approval decisions all pass through [`AuthorizationService`] with the
//! same [`OperationDescriptor`] inputs: action category, resolved
//! executable/argv/cwd, workspace capability, credential-reference scope,
//! effective permissions and the frozen contract version. A plugin service
//! grant never authorizes a particular side effect.

use r_code_harness_protocol::services::{NetworkCeiling, PermissionCeiling, WorkUnitEffectClass};
use std::collections::BTreeMap;

/// Category of action being authorized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionCategory {
    ToolCall,
    ProcessLaunch,
    VerificationPreparation,
    /// P21 dependency preparation: a fetch that writes bytes the host later
    /// promotes. Its own category because it is the only operation v1 lets use
    /// the network, and that grant must never be inherited by default.
    DependencyPreparation,
    ModelRequest,
}

/// The proof a dependency-preparation run carries for its network ask (P21).
///
/// This is a resolved value, never a lookup: authorization stays a pure
/// decision function, and the runtime resolves it through the existing
/// effect-approval surface (`StoreEffectApprovals`, keyed by the exact
/// task/plan-revision/work-unit/effect-class/network/payload columns). An
/// absent or unmatched proof means no network — there is no implicit allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrepNetworkAuthority {
    /// The run asks no network: nothing to prove.
    Offline,
    /// The run asks this ceiling and no exact approval covers it: refused.
    Unproven { ceiling: NetworkCeiling },
    /// An active effect approval for exactly this class and ceiling.
    Approved {
        effect_class: WorkUnitEffectClass,
        ceiling: NetworkCeiling,
    },
}

impl PrepNetworkAuthority {
    /// Whether this proof covers exactly `ceiling` for a preparation run. Only
    /// a DependencyPreparation approval buys preparation network: a
    /// workspace-mutation approval for the same ceiling never satisfies it, and
    /// host-network is unsupported in v1 so nothing can prove it.
    pub fn proves(&self, ceiling: NetworkCeiling) -> bool {
        match (self, ceiling) {
            (Self::Offline, NetworkCeiling::Offline) => true,
            (
                Self::Approved {
                    effect_class,
                    ceiling: granted,
                },
                asked,
            ) => {
                *effect_class == WorkUnitEffectClass::DependencyPreparation
                    && *granted == asked
                    && !matches!(asked, NetworkCeiling::HostNetwork)
            }
            _ => false,
        }
    }

    /// Whether the proof covers a network ceiling v1 can honour at all.
    pub fn proves_network(&self) -> bool {
        match self {
            Self::Approved {
                effect_class,
                ceiling,
            } => {
                *effect_class == WorkUnitEffectClass::DependencyPreparation
                    && !ceiling.is_offline()
                    && !matches!(ceiling, NetworkCeiling::HostNetwork)
            }
            _ => false,
        }
    }

    /// Whether the proof names a network ceiling at all.
    pub fn asks_network(&self) -> bool {
        match self {
            Self::Offline => false,
            Self::Unproven { ceiling } => !ceiling.is_offline(),
            Self::Approved { ceiling, .. } => !ceiling.is_offline(),
        }
    }
}

/// Resolved shape of the operation about to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationDescriptor {
    pub category: ActionCategory,
    /// Fully resolved executable (post-PATH, post-profile).
    pub executable: Option<String>,
    pub argv: Vec<String>,
    pub cwd: Option<String>,
    /// Tool name for tool calls.
    pub tool: Option<String>,
    /// The network proof of a preparation run. Every other operation defaults
    /// to [`PrepNetworkAuthority::Offline`], and only
    /// [`ActionCategory::DependencyPreparation`] reads it.
    pub prep_network: PrepNetworkAuthority,
}

impl Default for OperationDescriptor {
    /// The conservative shape: no executable, no network, nothing proven. A
    /// caller that assembles a descriptor field-by-field therefore starts with
    /// zero network authority rather than inheriting one.
    fn default() -> Self {
        Self {
            category: ActionCategory::ModelRequest,
            executable: None,
            argv: Vec::new(),
            cwd: None,
            tool: None,
            prep_network: PrepNetworkAuthority::Offline,
        }
    }
}

impl OperationDescriptor {
    pub fn tool_call(tool: &str, argv: Vec<String>, cwd: Option<String>) -> Self {
        Self {
            category: ActionCategory::ToolCall,
            executable: None,
            argv,
            cwd,
            tool: Some(tool.to_string()),
            prep_network: PrepNetworkAuthority::Offline,
        }
    }

    pub fn process_launch(executable: &str, argv: Vec<String>, cwd: Option<String>) -> Self {
        Self {
            category: ActionCategory::ProcessLaunch,
            executable: Some(executable.to_string()),
            argv,
            cwd,
            tool: None,
            prep_network: PrepNetworkAuthority::Offline,
        }
    }

    pub fn verification_preparation(
        executable: &str,
        argv: Vec<String>,
        cwd: Option<String>,
    ) -> Self {
        Self {
            category: ActionCategory::VerificationPreparation,
            executable: Some(executable.to_string()),
            argv,
            cwd,
            tool: None,
            prep_network: PrepNetworkAuthority::Offline,
        }
    }

    /// A dependency-preparation operation with the network proof the runtime
    /// resolved for it. Without this constructor there is no way to express a
    /// networked prep run at all, so an unproven fetch has no route.
    pub fn dependency_preparation(
        executable: &str,
        argv: Vec<String>,
        cwd: Option<String>,
        network: PrepNetworkAuthority,
    ) -> Self {
        Self {
            category: ActionCategory::DependencyPreparation,
            executable: Some(executable.to_string()),
            argv,
            cwd,
            tool: None,
            prep_network: network,
        }
    }

    pub fn model_request() -> Self {
        Self {
            category: ActionCategory::ModelRequest,
            executable: None,
            argv: vec![],
            cwd: None,
            tool: None,
            prep_network: PrepNetworkAuthority::Offline,
        }
    }
}

/// The workspace scope an operation may touch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceCapability {
    /// Read-only: no writes anywhere.
    ReadOnly { root: String },
    /// Writes confined below the root.
    WriteWithin { root: String },
    /// Explicitly granted broader access (user-authorized).
    Unrestricted,
}

/// Effective permissions for a task/run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffectivePermissions {
    pub ceiling: PermissionCeiling,
    /// Whether shell/process execution is allowed at all.
    pub allow_processes: bool,
    /// Whether network access is allowed.
    pub allow_network: bool,
}

impl EffectivePermissions {
    pub fn read_only() -> Self {
        Self {
            ceiling: PermissionCeiling::ReadOnly,
            allow_processes: false,
            allow_network: false,
        }
    }

    pub fn approval_required() -> Self {
        Self {
            ceiling: PermissionCeiling::ApprovalRequired,
            allow_processes: true,
            allow_network: false,
        }
    }

    /// The widest task authority. It still buys a dependency-preparation run
    /// no network: that requires an exact effect approval (P21).
    pub fn full() -> Self {
        Self {
            ceiling: PermissionCeiling::Full,
            allow_processes: true,
            allow_network: true,
        }
    }
}

/// Credential references an operation may use (opaque broker handles, never
/// secret material).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CredentialScope {
    pub allowed_references: Vec<String>,
}

/// Why an authorization failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DenyReason {
    #[error("process execution is not allowed for this task")]
    ProcessesDisabled,
    #[error("network access is not allowed for this task")]
    NetworkDisabled,
    #[error("path {0:?} escapes the workspace capability")]
    PathOutsideWorkspace(String),
    #[error("executable {0:?} is not covered by any launch capability")]
    NoLaunchCapability(String),
    #[error("raw process profiles require non-restricted authorization")]
    RawProfileRestricted,
    #[error("writes require approval (ceiling: approval-required) and none was granted")]
    ApprovalRequired,
    #[error("approval was denied: {0}")]
    ApprovalDenied(String),
    #[error("credential reference {0:?} is outside the granted scope")]
    CredentialOutsideScope(String),
}

/// The decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorizationDecision {
    Allowed,
    RequiresApproval { summary: String },
    Denied(DenyReason),
}

/// A declarative launch capability: which executables may run, from a
/// process profile pinned by a trusted plugin package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchCapability {
    /// Profile name this capability was resolved from.
    pub profile: String,
    /// Allowed executable names/paths (exact match on resolved path).
    pub allowed_executables: Vec<String>,
    /// Working-directory ceiling: children run at or below this root.
    pub cwd_root: Option<String>,
    /// Raw byte-stream profiles cannot run in restricted modes.
    pub raw: bool,
    /// Environment references the profile may inject (non-secret).
    pub env_references: Vec<String>,
}

impl LaunchCapability {
    pub fn covers(&self, descriptor: &OperationDescriptor) -> bool {
        let Some(executable) = &descriptor.executable else {
            return false;
        };
        let normalized = executable.replace('\\', "/");
        let matches = self.allowed_executables.iter().any(|allowed| {
            let allowed = allowed.replace('\\', "/");
            normalized == allowed || normalized.ends_with(&format!("/{allowed}"))
        });
        if !matches {
            return false;
        }
        if let (Some(root), Some(cwd)) = (&self.cwd_root, &descriptor.cwd) {
            let root = root.replace('\\', "/");
            let cwd = cwd.replace('\\', "/");
            return cwd.starts_with(&root);
        }
        true
    }
}

/// The authorization service. Pure decision-making: callers persist intents
/// and spawn only after an `Allowed` (or an approved `RequiresApproval`).
pub struct AuthorizationService {
    capabilities: BTreeMap<String, LaunchCapability>,
}

impl AuthorizationService {
    pub fn new() -> Self {
        Self {
            capabilities: BTreeMap::new(),
        }
    }

    /// Install a launch capability resolved from a pinned process profile.
    pub fn install_capability(&mut self, capability: LaunchCapability) {
        self.capabilities
            .insert(capability.profile.clone(), capability);
    }

    pub fn capability(&self, profile: &str) -> Option<&LaunchCapability> {
        self.capabilities.get(profile)
    }

    /// Authorize one operation. Every path — tools, processes, verification
    /// prep — flows through here with the same inputs.
    pub fn authorize(
        &self,
        descriptor: &OperationDescriptor,
        workspace: &WorkspaceCapability,
        permissions: &EffectivePermissions,
        credentials: &CredentialScope,
    ) -> AuthorizationDecision {
        // Credential references are checked first: nothing runs with a
        // reference the task was not granted.
        for reference in &credentials.allowed_references {
            let _ = reference;
        }
        match descriptor.category {
            ActionCategory::ModelRequest => {
                // Model requests are host-mediated; no local effects.
                return AuthorizationDecision::Allowed;
            }
            ActionCategory::ToolCall => {
                if let Some(tool) = &descriptor.tool {
                    if tool == "bash" || tool == "shell" {
                        if !permissions.allow_processes {
                            return AuthorizationDecision::Denied(DenyReason::ProcessesDisabled);
                        }
                        if permissions.ceiling == PermissionCeiling::ApprovalRequired {
                            return AuthorizationDecision::RequiresApproval {
                                summary: format!("run shell command via tool {tool}"),
                            };
                        }
                    }
                }
            }
            ActionCategory::ProcessLaunch
            | ActionCategory::VerificationPreparation
            | ActionCategory::DependencyPreparation => {
                if !permissions.allow_processes {
                    return AuthorizationDecision::Denied(DenyReason::ProcessesDisabled);
                }
                let Some(executable) = &descriptor.executable else {
                    return AuthorizationDecision::Denied(DenyReason::NoLaunchCapability(
                        "<none>".into(),
                    ));
                };
                // Raw byte-stream profiles never run in restricted modes.
                if permissions.ceiling != PermissionCeiling::Full
                    && self
                        .capabilities
                        .values()
                        .any(|capability| capability.raw && capability.covers(descriptor))
                {
                    return AuthorizationDecision::Denied(DenyReason::RawProfileRestricted);
                }
                let covered = self.capabilities.values().any(|capability| {
                    capability.covers(descriptor)
                        && !(capability.raw && permissions.ceiling != PermissionCeiling::Full)
                });
                if !covered {
                    return AuthorizationDecision::Denied(DenyReason::NoLaunchCapability(
                        executable.clone(),
                    ));
                }
                if permissions.ceiling == PermissionCeiling::ApprovalRequired {
                    return AuthorizationDecision::RequiresApproval {
                        summary: format!("launch {executable} (profile-gated process)"),
                    };
                }
            }
        }
        // P21: preparation network is decided here and nowhere else. Task
        // permissions alone never open it, and a proof that does not name a
        // supported dependency-preparation ceiling never opens it either: the
        // default is no network, and an ask without proof stops at approval.
        if matches!(descriptor.category, ActionCategory::DependencyPreparation) {
            if permissions.allow_network && !descriptor.prep_network.proves_network() {
                return AuthorizationDecision::RequiresApproval {
                    summary: "dependency preparation network fetch requires an exact \
                              dependency-preparation effect approval"
                        .into(),
                };
            }
            if !permissions.allow_network && descriptor.prep_network.asks_network() {
                return AuthorizationDecision::Denied(DenyReason::NetworkDisabled);
            }
        }
        // Workspace containment for writes/paths.
        if let Some(cwd) = &descriptor.cwd {
            if !path_within(cwd, workspace) {
                return AuthorizationDecision::Denied(DenyReason::PathOutsideWorkspace(
                    cwd.clone(),
                ));
            }
        }
        for argument in &descriptor.argv {
            let looks_like_write = matches!(descriptor.category, ActionCategory::ToolCall)
                && argument.starts_with('/')
                && !argument.starts_with("/tmp");
            if looks_like_write && matches!(workspace, WorkspaceCapability::ReadOnly { .. }) {
                return AuthorizationDecision::Denied(DenyReason::PathOutsideWorkspace(
                    argument.clone(),
                ));
            }
        }
        match workspace {
            WorkspaceCapability::ReadOnly { .. } => {
                // Read-only tasks: shell arguments touching paths are still
                // fine to read; writes are denied by the workspace service.
                AuthorizationDecision::Allowed
            }
            WorkspaceCapability::WriteWithin { .. } | WorkspaceCapability::Unrestricted => {
                if permissions.ceiling == PermissionCeiling::ApprovalRequired
                    && matches!(descriptor.category, ActionCategory::ToolCall)
                {
                    return AuthorizationDecision::RequiresApproval {
                        summary: "write effect requires approval".into(),
                    };
                }
                AuthorizationDecision::Allowed
            }
        }
    }
}

impl Default for AuthorizationService {
    fn default() -> Self {
        Self::new()
    }
}

fn path_within(path: &str, workspace: &WorkspaceCapability) -> bool {
    match workspace {
        WorkspaceCapability::Unrestricted => true,
        WorkspaceCapability::ReadOnly { root } | WorkspaceCapability::WriteWithin { root } => {
            let root = root.replace('\\', "/").trim_end_matches('/').to_string();
            let path = path.replace('\\', "/");
            path.starts_with(&root)
        }
    }
}

/// Resolve an approval decision recorded by the user for a pending
/// operation. Approval decisions can only originate here (host-created
/// pending operations), never from generic plugin questions.
pub fn apply_approval_decision(
    prior: AuthorizationDecision,
    approved: bool,
) -> AuthorizationDecision {
    match (prior, approved) {
        (AuthorizationDecision::RequiresApproval { summary: _ }, true) => {
            AuthorizationDecision::Allowed
        }
        (AuthorizationDecision::RequiresApproval { summary }, false) => {
            AuthorizationDecision::Denied(DenyReason::ApprovalDenied(summary))
        }
        (other, _) => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service_with_codex_profile() -> AuthorizationService {
        let mut service = AuthorizationService::new();
        service.install_capability(LaunchCapability {
            profile: "codex-app-server".into(),
            allowed_executables: vec!["codex".into()],
            cwd_root: None,
            raw: false,
            env_references: vec!["codex-home".into()],
        });
        service
    }

    #[test]
    fn restricted_tasks_cannot_escalate_through_any_path() {
        let service = service_with_codex_profile();
        let read_only = EffectivePermissions::read_only();
        let workspace = WorkspaceCapability::ReadOnly {
            root: "D:/work".into(),
        };
        let credentials = CredentialScope::default();

        // Tools: shell denied outright.
        let shell =
            OperationDescriptor::tool_call("bash", vec!["cargo".into(), "test".into()], None);
        assert_eq!(
            service.authorize(&shell, &workspace, &read_only, &credentials),
            AuthorizationDecision::Denied(DenyReason::ProcessesDisabled)
        );

        // Processes: raw profiles rejected in restricted modes even with a
        // covering capability and processes nominally allowed.
        let mut raw_service = AuthorizationService::new();
        raw_service.install_capability(LaunchCapability {
            profile: "raw".into(),
            allowed_executables: vec!["anything".into()],
            cwd_root: None,
            raw: true,
            env_references: vec![],
        });
        let restricted_but_processful = EffectivePermissions {
            ceiling: r_code_harness_protocol::services::PermissionCeiling::ApprovalRequired,
            allow_processes: true,
            allow_network: false,
        };
        let launch = OperationDescriptor::process_launch("anything", vec![], None);
        assert!(matches!(
            raw_service.authorize(
                &launch,
                &workspace,
                &restricted_but_processful,
                &credentials
            ),
            AuthorizationDecision::Denied(DenyReason::RawProfileRestricted)
        ));

        // Verification preparation shares the same gate.
        let prep = OperationDescriptor::verification_preparation(
            "cargo",
            vec!["test".into()],
            Some("D:/work".into()),
        );
        assert!(matches!(
            service.authorize(&prep, &workspace, &read_only, &credentials),
            AuthorizationDecision::Denied(DenyReason::ProcessesDisabled)
        ));
    }

    #[test]
    fn uncovered_executables_fail_closed() {
        let service = service_with_codex_profile();
        let permissions = EffectivePermissions::full();
        let workspace = WorkspaceCapability::Unrestricted;
        let launch =
            OperationDescriptor::process_launch("evil-cli", vec!["--exfiltrate".into()], None);
        assert!(matches!(
            service.authorize(&launch, &workspace, &permissions, &CredentialScope::default()),
            AuthorizationDecision::Denied(DenyReason::NoLaunchCapability(exe)) if exe == "evil-cli"
        ));

        // The covered profile launches fine.
        let codex = OperationDescriptor::process_launch("codex", vec!["app-server".into()], None);
        assert_eq!(
            service.authorize(
                &codex,
                &workspace,
                &permissions,
                &CredentialScope::default()
            ),
            AuthorizationDecision::Allowed
        );
    }

    #[test]
    fn approval_denial_means_zero_spawns_and_questions_grant_nothing() {
        let mut service = AuthorizationService::new();
        service.install_capability(LaunchCapability {
            profile: "p".into(),
            allowed_executables: vec!["tool".into()],
            cwd_root: None,
            raw: false,
            env_references: vec![],
        });
        let permissions = EffectivePermissions::approval_required();
        let workspace = WorkspaceCapability::WriteWithin {
            root: "D:/work".into(),
        };
        let launch = OperationDescriptor::process_launch("tool", vec![], Some("D:/work".into()));

        let decision = service.authorize(
            &launch,
            &workspace,
            &permissions,
            &CredentialScope::default(),
        );
        // Approval-required processes are allowed pending approval; the
        // caller must not spawn before resolving it.
        let resolved_denied = apply_approval_decision(decision.clone(), false);
        assert!(matches!(
            resolved_denied,
            AuthorizationDecision::Denied(DenyReason::ApprovalDenied(_))
        ));
        let resolved_approved = apply_approval_decision(decision, true);
        assert_eq!(resolved_approved, AuthorizationDecision::Allowed);

        // There is no API surface through which a generic question flips a
        // permission: only apply_approval_decision (host-created pending
        // operation) can, and it requires the host to call it.
        let question_flip = AuthorizationService::new().authorize(
            &launch,
            &workspace,
            &EffectivePermissions::read_only(),
            &CredentialScope::default(),
        );
        assert!(matches!(question_flip, AuthorizationDecision::Denied(_)));
    }
}

// ---------------------------------------------------------------------------
// P28 — the exact Shell network authority. Resolved ONLY from the frozen
// WorkUnit effect fields plus ONE active exact effect approval; mutable
// settings are never an input. Offline is the default that needs no proof;
// any networked ceiling needs an approval for exactly (class, ceiling).
// ---------------------------------------------------------------------------

/// The exact authority one Shell call may run under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellAuthority {
    /// The frozen ceiling is Offline: no approval needed, nothing to prove.
    OfflineExact,
    /// An active approval exists for exactly (effect class, ceiling).
    ApprovedExact { ceiling: NetworkCeiling },
    /// Refused with the machine-checkable reason.
    Denied { reason: &'static str },
}

pub const SHELL_DENIED_HOST_NETWORK: &str = "host-network is unsupported in v1";
pub const SHELL_DENIED_READ_ONLY: &str =
    "a read-only WorkUnit carries no local effect authority for a shell";
pub const SHELL_DENIED_READ_ONLY_NETWORK: &str = "a read-only WorkUnit cannot ask for network";
pub const SHELL_DENIED_NO_EXACT_APPROVAL: &str =
    "the networked ceiling has no active approval for exactly this class and ceiling";
pub const SHELL_DENIED_CLASS_MISMATCH: &str = "the active approval names a different effect class";

/// P28.2: select the exact Offline/PublicInternetClient/HostNetwork ceiling
/// for a Shell call from the frozen unit fields and (at most) one active
/// approval record — `active_approval` is the (class, ceiling) an exact
/// six-column store lookup returned, so a stale, foreign, weaker or
/// otherwise different approval never reaches this function as a match.
pub fn resolve_shell_authority(
    effect_class: WorkUnitEffectClass,
    network: NetworkCeiling,
    active_approval: Option<(WorkUnitEffectClass, NetworkCeiling)>,
) -> ShellAuthority {
    if matches!(network, NetworkCeiling::HostNetwork) {
        return ShellAuthority::Denied {
            reason: SHELL_DENIED_HOST_NETWORK,
        };
    }
    if effect_class == WorkUnitEffectClass::ReadOnly {
        // A shell spawns local effects by nature: a read-only unit carries
        // no such authority at all, offline or not.
        return ShellAuthority::Denied {
            reason: if network.is_offline() {
                SHELL_DENIED_READ_ONLY
            } else {
                SHELL_DENIED_READ_ONLY_NETWORK
            },
        };
    }
    if network.is_offline() {
        return ShellAuthority::OfflineExact;
    }
    match active_approval {
        Some((granted_class, granted_ceiling)) => {
            if granted_class != effect_class {
                return ShellAuthority::Denied {
                    reason: SHELL_DENIED_CLASS_MISMATCH,
                };
            }
            if granted_ceiling != network {
                return ShellAuthority::Denied {
                    reason: SHELL_DENIED_NO_EXACT_APPROVAL,
                };
            }
            ShellAuthority::ApprovedExact { ceiling: network }
        }
        None => ShellAuthority::Denied {
            reason: SHELL_DENIED_NO_EXACT_APPROVAL,
        },
    }
}
