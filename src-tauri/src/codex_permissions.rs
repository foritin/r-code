//! Codex 子代理的权限配置映射。
//!
//! 这里的值与 Codex `config.toml` / App Server 使用的公开枚举保持一一对应。
//! 解析配置时绝不把未知字符串回传给 CLI：未来版本或手写错误会降级为只读，
//! 同时在设置页标记为 `custom`，等待用户显式修正。

use serde::{Deserialize, Serialize};

/// 设置页展示的 Codex 子代理权限预设。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexPermissionMode {
    /// R-Code 的历史默认值；只允许读取工作区。
    #[default]
    ReadOnly,
    /// Codex 请求额外权限时由 R-Code 呈现审批卡。
    RequestApproval,
    /// Codex 在需要额外权限时自行审查并继续。
    AutoReview,
    /// 不使用 Codex sandbox，也不请求审批。
    FullAccess,
    /// 用户在 config.toml 中配置了非预设组合。
    Custom,
}

impl CodexPermissionMode {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "read_only" => Some(Self::ReadOnly),
            "request_approval" => Some(Self::RequestApproval),
            "auto_review" => Some(Self::AutoReview),
            "full_access" => Some(Self::FullAccess),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }

    pub(crate) fn display_name(self) -> &'static str {
        match self {
            Self::ReadOnly => "仅查看",
            Self::RequestApproval => "请求批准",
            Self::AutoReview => "替我审批",
            Self::FullAccess => "完全访问权限",
            Self::Custom => "自定义 config.toml",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexSandboxMode {
    ReadOnly,
    WorkspaceWrite,
    DangerFullAccess,
}

impl CodexSandboxMode {
    fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "read-only" => Some(Self::ReadOnly),
            "workspace-write" => Some(Self::WorkspaceWrite),
            "danger-full-access" => Some(Self::DangerFullAccess),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexApprovalPolicy {
    Untrusted,
    OnRequest,
    Never,
}

impl CodexApprovalPolicy {
    fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "untrusted" => Some(Self::Untrusted),
            "on-request" => Some(Self::OnRequest),
            "never" => Some(Self::Never),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Untrusted => "untrusted",
            Self::OnRequest => "on-request",
            Self::Never => "never",
        }
    }

    /// 仅由本模块生成的固定 TOML 覆盖片段；不接受任何用户拼接内容。
    pub(crate) fn config_override(self) -> &'static str {
        match self {
            Self::Untrusted => r#"approval_policy="untrusted""#,
            Self::OnRequest => r#"approval_policy="on-request""#,
            Self::Never => r#"approval_policy="never""#,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CodexApprovalsReviewer {
    User,
    AutoReview,
}

impl CodexApprovalsReviewer {
    fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "user" => Some(Self::User),
            "auto_review" => Some(Self::AutoReview),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::AutoReview => "auto_review",
        }
    }

    /// 仅由本模块生成的固定 TOML 覆盖片段；不接受任何用户拼接内容。
    pub(crate) fn config_override(self) -> &'static str {
        match self {
            Self::User => r#"approvals_reviewer="user""#,
            Self::AutoReview => r#"approvals_reviewer="auto_review""#,
        }
    }
}

/// 一个在进程启动前解析完成的、只含已验证枚举值的 Codex 权限快照。
///
/// 每次子代理启动都会重新读取 config.toml；因此设置保存后不需要重启 R-Code。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CodexDelegationPermissions {
    mode: CodexPermissionMode,
    sandbox: CodexSandboxMode,
    approval_policy: CodexApprovalPolicy,
    approvals_reviewer: CodexApprovalsReviewer,
}

impl Default for CodexDelegationPermissions {
    fn default() -> Self {
        Self::read_only()
    }
}

impl CodexDelegationPermissions {
    pub(crate) const fn read_only() -> Self {
        Self {
            mode: CodexPermissionMode::ReadOnly,
            sandbox: CodexSandboxMode::ReadOnly,
            approval_policy: CodexApprovalPolicy::Never,
            approvals_reviewer: CodexApprovalsReviewer::User,
        }
    }

    pub(crate) const fn from_mode(mode: CodexPermissionMode) -> Option<Self> {
        match mode {
            CodexPermissionMode::ReadOnly => Some(Self::read_only()),
            CodexPermissionMode::RequestApproval => Some(Self {
                mode,
                sandbox: CodexSandboxMode::WorkspaceWrite,
                approval_policy: CodexApprovalPolicy::OnRequest,
                approvals_reviewer: CodexApprovalsReviewer::User,
            }),
            CodexPermissionMode::AutoReview => Some(Self {
                mode,
                sandbox: CodexSandboxMode::WorkspaceWrite,
                approval_policy: CodexApprovalPolicy::OnRequest,
                approvals_reviewer: CodexApprovalsReviewer::AutoReview,
            }),
            CodexPermissionMode::FullAccess => Some(Self {
                mode,
                sandbox: CodexSandboxMode::DangerFullAccess,
                approval_policy: CodexApprovalPolicy::Never,
                approvals_reviewer: CodexApprovalsReviewer::User,
            }),
            CodexPermissionMode::Custom => None,
        }
    }

    pub(crate) fn from_config(
        sandbox: Option<&str>,
        approval_policy: Option<&str>,
        approvals_reviewer: Option<&str>,
    ) -> Self {
        let parsed_sandbox = sandbox.and_then(CodexSandboxMode::parse);
        let parsed_policy = approval_policy.and_then(CodexApprovalPolicy::parse);
        let parsed_reviewer = approvals_reviewer.and_then(CodexApprovalsReviewer::parse);
        let has_unknown = sandbox.is_some_and(|_| parsed_sandbox.is_none())
            || approval_policy.is_some_and(|_| parsed_policy.is_none())
            || approvals_reviewer.is_some_and(|_| parsed_reviewer.is_none());

        // `codex exec` 在没有显式 profile 时是只读。沿用此前 R-Code 的安全默认，
        // 而不是猜测交互式 Codex CLI 的默认审批策略。
        let sandbox = parsed_sandbox.unwrap_or(CodexSandboxMode::ReadOnly);
        let approval_policy = parsed_policy.unwrap_or(CodexApprovalPolicy::Never);
        let approvals_reviewer = parsed_reviewer.unwrap_or(CodexApprovalsReviewer::User);
        let mode = if has_unknown {
            CodexPermissionMode::Custom
        } else {
            match (sandbox, approval_policy, approvals_reviewer) {
                (
                    CodexSandboxMode::ReadOnly,
                    CodexApprovalPolicy::Never,
                    CodexApprovalsReviewer::User,
                ) => CodexPermissionMode::ReadOnly,
                (
                    CodexSandboxMode::WorkspaceWrite,
                    CodexApprovalPolicy::OnRequest,
                    CodexApprovalsReviewer::User,
                ) => CodexPermissionMode::RequestApproval,
                (
                    CodexSandboxMode::WorkspaceWrite,
                    CodexApprovalPolicy::OnRequest,
                    CodexApprovalsReviewer::AutoReview,
                ) => CodexPermissionMode::AutoReview,
                (CodexSandboxMode::DangerFullAccess, CodexApprovalPolicy::Never, _) => {
                    CodexPermissionMode::FullAccess
                }
                _ => CodexPermissionMode::Custom,
            }
        };
        Self {
            mode,
            sandbox,
            approval_policy,
            approvals_reviewer,
        }
    }

    pub(crate) const fn mode(self) -> CodexPermissionMode {
        self.mode
    }

    pub(crate) const fn as_custom(self) -> Self {
        Self {
            mode: CodexPermissionMode::Custom,
            ..self
        }
    }

    pub(crate) const fn sandbox(self) -> CodexSandboxMode {
        self.sandbox
    }

    pub(crate) const fn approval_policy(self) -> CodexApprovalPolicy {
        self.approval_policy
    }

    pub(crate) const fn approvals_reviewer(self) -> CodexApprovalsReviewer {
        self.approvals_reviewer
    }

    pub(crate) const fn requests_r_code_approval(self) -> bool {
        matches!(self.approvals_reviewer, CodexApprovalsReviewer::User)
            && !matches!(self.approval_policy, CodexApprovalPolicy::Never)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_configuration_preserves_the_historical_read_only_default() {
        assert_eq!(
            CodexDelegationPermissions::from_config(None, None, None),
            CodexDelegationPermissions::read_only()
        );
    }

    #[test]
    fn known_presets_map_to_the_expected_cli_values() {
        let request = CodexDelegationPermissions::from_config(
            Some("workspace-write"),
            Some("on-request"),
            Some("user"),
        );
        assert_eq!(request.mode(), CodexPermissionMode::RequestApproval);
        assert!(request.requests_r_code_approval());

        let review = CodexDelegationPermissions::from_mode(CodexPermissionMode::AutoReview)
            .expect("preset must have a profile");
        assert_eq!(review.sandbox().as_str(), "workspace-write");
        assert_eq!(review.approval_policy().as_str(), "on-request");
        assert_eq!(review.approvals_reviewer().as_str(), "auto_review");

        let full = CodexDelegationPermissions::from_mode(CodexPermissionMode::FullAccess)
            .expect("preset must have a profile");
        assert_eq!(full.sandbox().as_str(), "danger-full-access");
        assert_eq!(full.approval_policy().as_str(), "never");
    }

    #[test]
    fn unknown_config_never_becomes_an_unvalidated_cli_argument() {
        let profile = CodexDelegationPermissions::from_config(
            Some("future-sandbox"),
            Some("unexpected"),
            Some("unknown"),
        );
        assert_eq!(profile.mode(), CodexPermissionMode::Custom);
        assert_eq!(profile.sandbox().as_str(), "read-only");
        assert_eq!(profile.approval_policy().as_str(), "never");
    }

    #[test]
    fn custom_user_review_profiles_still_use_the_host_approval_bridge() {
        let profile = CodexDelegationPermissions::from_config(
            Some("danger-full-access"),
            Some("on-request"),
            Some("user"),
        );
        assert_eq!(profile.mode(), CodexPermissionMode::Custom);
        assert!(profile.requests_r_code_approval());
    }
}
