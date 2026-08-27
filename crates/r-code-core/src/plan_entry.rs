//! Plan 入口建议（PlanEntryOffer）与 Plan 冻结运行 profile 的领域合同。
//!
//! 本模块只定义持久化形态与稳定枚举；资格解析、证据门与客户文案模板位于宿主
//! `r_code_host::plan_policy`（见 docs/support/archive/implementation/plan-mode-dual-track-gate.md 第 11、14 节）。
//! 持久化拼写是 SQLite 与 IPC 合同的一部分，保持 `as_str`/serde 同步。

use std::fmt;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

macro_rules! stable_string_enum {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        impl $name {
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $value,)+
                }
            }

            pub fn try_from_str(value: &str) -> Option<Self> {
                match value {
                    $($value => Some(Self::$variant),)+
                    _ => None,
                }
            }

            pub const ALL: &'static [Self] = &[$(Self::$variant),+];
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

/// `propose_plan_mode` 可提交的受控复杂度信号。模型不能自造枚举值，宿主按固定
/// 优先级只取一个映射为客户文案模板。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanComplexitySignal {
    MultiSubsystem,
    MigrationOrData,
    DesignDecision,
    ExpensiveRollback,
    MultiStageVerification,
}

stable_string_enum!(PlanComplexitySignal {
    MultiSubsystem => "multi_subsystem",
    MigrationOrData => "migration_or_data",
    DesignDecision => "design_decision",
    ExpensiveRollback => "expensive_rollback",
    MultiStageVerification => "multi_stage_verification",
});

impl PlanComplexitySignal {
    /// 宿主选择客户文案模板时使用的固定优先级（docs §9.1）。靠前的信号胜出。
    pub const PRIORITY: &'static [Self] = &[
        Self::MigrationOrData,
        Self::ExpensiveRollback,
        Self::DesignDecision,
        Self::MultiStageVerification,
        Self::MultiSubsystem,
    ];

    pub fn primary_of(signals: &[Self]) -> Option<Self> {
        Self::PRIORITY
            .iter()
            .copied()
            .find(|candidate| signals.contains(candidate))
    }
}

/// 建议聚合的生命周期。`pending` 与 `Plan` 是不同状态：出现建议不等于进入 Plan。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanEntryOfferState {
    #[default]
    Pending,
    Accepted,
    Declined,
    SupersededProviderChanged,
    Expired,
}

stable_string_enum!(PlanEntryOfferState {
    Pending => "pending",
    Accepted => "accepted",
    Declined => "declined",
    SupersededProviderChanged => "superseded_provider_changed",
    Expired => "expired",
});

impl PlanEntryOfferState {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Pending)
    }
}

/// 客户决定的来源。`continue | close | escape` 在存储上都等价于拒绝，但分别记录
/// 以支撑安静策略与审计。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanEntryDecisionSource {
    Accept,
    Continue,
    Close,
    Escape,
}

stable_string_enum!(PlanEntryDecisionSource {
    Accept => "accept",
    Continue => "continue",
    Close => "close",
    Escape => "escape",
});

impl PlanEntryDecisionSource {
    pub fn is_decline(self) -> bool {
        matches!(self, Self::Continue | Self::Close | Self::Escape)
    }
}

/// 建议决定的 durable 续接子状态（docs §12.4 的 at-least-once 合同）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanEntryContinuationState {
    #[default]
    None,
    Queued,
    Dispatching,
    Sent,
    Failed,
}

stable_string_enum!(PlanEntryContinuationState {
    None => "none",
    Queued => "queued",
    Dispatching => "dispatching",
    Sent => "sent",
    Failed => "failed",
});

/// 真实用户请求的发送路径分类（docs §10.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginRequestKind {
    Direct,
    Queued,
    Steer,
    HostContinuation,
}

stable_string_enum!(OriginRequestKind {
    Direct => "direct",
    Queued => "queued",
    Steer => "steer",
    HostContinuation => "host_continuation",
});

/// 宿主在所有发送分支之前创建的统一请求信封。`request_key` 回答“这是不是同一次
/// 真实请求”，是幂等与恢复边界；branch suggestion state 回答“还要不要再打断客户”。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OriginRequestEnvelope {
    pub request_key: String,
    pub kind: OriginRequestKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_request_key: Option<String>,
    pub operation_id: String,
    pub task_id: String,
    pub branch_id: String,
    pub created_at: DateTime<Utc>,
}

/// 冻结的非秘密 Provider route 身份。绝不包含 API key、授权头或可携带凭据的完整
/// URL；接受前重新比较当前 route 与该快照（docs §4.3、§12.3）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProviderRouteSnapshot {
    pub provider_kind: String,
    pub provider_profile_id: String,
    pub provider_profile_version: String,
    pub provider_route_revision: String,
    pub model_id: String,
    pub wire_protocol: String,
    pub endpoint_class: String,
}

/// Plan 冻结运行 profile 的枚举（docs §14）。`baseline` 是现状目录；`plan_native_v1`
/// 是 DeepSeek 双轨的 5→8 只读目录。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanCatalogProfile {
    #[default]
    Baseline,
    PlanNativeV1,
}

stable_string_enum!(PlanCatalogProfile {
    Baseline => "baseline",
    PlanNativeV1 => "plan_native_v1",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanContextProfile {
    #[default]
    Default,
    MinimalV1,
}

stable_string_enum!(PlanContextProfile {
    Default => "default",
    MinimalV1 => "minimal_v1",
});

/// `plans.catalog_phase` 的权威值。只允许 `bootstrap -> resident` 单向 CAS。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanCatalogPhase {
    #[default]
    Bootstrap,
    Resident,
}

stable_string_enum!(PlanCatalogPhase {
    Bootstrap => "bootstrap",
    Resident => "resident",
});

/// 创建 Plan 时由宿主解析并冻结的运行 profile（docs §14）。全局设置只参与创建时的
/// 解析；创建后 Plan 使用该不可变快照。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedPlanRuntimeProfile {
    pub enabled: bool,
    pub catalog_profile: PlanCatalogProfile,
    pub context_profile: PlanContextProfile,
    pub profile_version: u32,
    pub evidence_version: String,
    pub provider_kind: String,
    pub model_id: String,
    pub endpoint_class: String,
    /// v2（docs/support/archive/implementation/multimodal-attachments-and-deepseek-plan-anchoring-implementation.md §8.2）：用户显式选择的协议标识。
    #[serde(default)]
    pub protocol: String,
    /// v2：冻结 route revision（route 漂移按此比较，§8.7）。
    #[serde(default)]
    pub provider_route_revision: String,
    /// v2：创建时冻结的 `planning.deepseek_plan_anchoring` 偏好。
    #[serde(default)]
    pub anchoring_preference: bool,
}

impl ResolvedPlanRuntimeProfile {
    /// baseline 快照：所有非资格路径（其他 Provider、旧 Plan、scope decision 临时
    /// Plan）一律使用该值，行为与双轨之前完全一致。
    pub fn baseline() -> Self {
        Self {
            enabled: false,
            catalog_profile: PlanCatalogProfile::Baseline,
            context_profile: PlanContextProfile::Default,
            profile_version: 2,
            evidence_version: String::new(),
            provider_kind: String::new(),
            model_id: String::new(),
            endpoint_class: String::new(),
            protocol: String::new(),
            provider_route_revision: String::new(),
            anchoring_preference: false,
        }
    }
}

impl Default for ResolvedPlanRuntimeProfile {
    fn default() -> Self {
        Self::baseline()
    }
}

/// 建议聚合的持久形态（docs §11 建议表全字段）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanEntryOffer {
    pub id: String,
    pub task_id: String,
    pub branch_id: String,
    pub source_run_id: String,
    pub request_key: String,
    pub original_mode: String,
    /// 仅本地脱敏审计。普通 UI、通知、GuideSheet 和遥测都不能显示或上传。
    pub reason_audit: String,
    pub signals: Vec<PlanComplexitySignal>,
    pub primary_signal: PlanComplexitySignal,
    pub customer_copy_key: String,
    pub customer_copy_version: u32,
    pub provider: ProviderRouteSnapshot,
    pub eligibility_profile_version: String,
    pub evidence_version: String,
    pub resolved_plan_runtime_profile: ResolvedPlanRuntimeProfile,
    pub revision: u64,
    pub state: PlanEntryOfferState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<PlanEntryDecisionSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    pub continuation_state: PlanEntryContinuationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 决定幂等键：同一按钮交互复用同一键，双击不产生第二个决定。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision_idempotency_key: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<DateTime<Utc>>,
}

/// branch 级建议频率预算的持久状态。不能用前端内存布尔值代替；应用重启不能把已
/// 拒绝 branch 的预算重置为可再次弹窗。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanSuggestionBranchState {
    pub task_id: String,
    pub branch_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggestion_budget_consumed_at: Option<DateTime<Utc>>,
    pub quiet_after_decline: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet_reason: Option<PlanEntryDecisionSource>,
    pub revision: u64,
    pub updated_at: DateTime<Utc>,
}

impl PlanSuggestionBranchState {
    /// 该 branch 是否仍可产生一次主动阻断建议。
    pub fn can_suggest(&self) -> bool {
        self.suggestion_budget_consumed_at.is_none() && !self.quiet_after_decline
    }
}

/// 客户对建议的一次决定。`expected_revision` 是 CAS；`idempotency_key` 由前端为
/// 同一按钮交互生成并复用，避免双击产生两个决定。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanEntryDecisionInput {
    pub offer_id: String,
    pub expected_revision: u64,
    pub decision: PlanEntryDecisionSource,
    pub idempotency_key: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_priority_picks_most_specific_first() {
        let signals = vec![
            PlanComplexitySignal::MultiSubsystem,
            PlanComplexitySignal::ExpensiveRollback,
        ];
        assert_eq!(
            PlanComplexitySignal::primary_of(&signals),
            Some(PlanComplexitySignal::ExpensiveRollback)
        );
        assert_eq!(PlanComplexitySignal::primary_of(&[]), None);
    }

    #[test]
    fn offer_state_spellings_are_stable() {
        assert_eq!(PlanEntryOfferState::Pending.as_str(), "pending");
        assert_eq!(
            PlanEntryOfferState::try_from_str("superseded_provider_changed"),
            Some(PlanEntryOfferState::SupersededProviderChanged)
        );
        assert!(!PlanEntryOfferState::Pending.is_terminal());
        assert!(PlanEntryOfferState::Declined.is_terminal());
    }

    #[test]
    fn branch_state_budget_and_quiet_are_independent_gates() {
        let mut state = PlanSuggestionBranchState {
            task_id: "t".into(),
            branch_id: "b".into(),
            suggestion_budget_consumed_at: None,
            quiet_after_decline: false,
            quiet_reason: None,
            revision: 1,
            updated_at: Utc::now(),
        };
        assert!(state.can_suggest());
        state.quiet_after_decline = true;
        assert!(!state.can_suggest());
        state.quiet_after_decline = false;
        state.suggestion_budget_consumed_at = Some(Utc::now());
        assert!(!state.can_suggest());
    }
}
