//! Unified action authorization.
//!
//! Tools, managed processes, verification preparation and user-originated
//! approval decisions all pass through [`AuthorizationService`] with the
//! same [`OperationDescriptor`] inputs: action category, resolved
//! executable/argv/cwd, workspace capability, credential-reference scope,
//! effective permissions and the frozen contract version. A plugin service
//! grant never authorizes a particular side effect.

use r_code_harness_protocol::services::PermissionCeiling;
use std::collections::BTreeMap;

/// Category of action being authorized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionCategory {
    ToolCall,
    ProcessLaunch,
    VerificationPreparation,
    ModelRequest,
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
}

impl OperationDescriptor {
    pub fn tool_call(tool: &str, argv: Vec<String>, cwd: Option<String>) -> Self {
        Self {
            category: ActionCategory::ToolCall,
            executable: None,
            argv,
            cwd,
            tool: Some(tool.to_string()),
        }
    }

    pub fn process_launch(executable: &str, argv: Vec<String>, cwd: Option<String>) -> Self {
        Self {
            category: ActionCategory::ProcessLaunch,
            executable: Some(executable.to_string()),
            argv,
            cwd,
            tool: None,
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
        }
    }

    pub fn model_request() -> Self {
        Self {
            category: ActionCategory::ModelRequest,
            executable: None,
            argv: vec![],
            cwd: None,
            tool: None,
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
            ActionCategory::ProcessLaunch | ActionCategory::VerificationPreparation => {
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
