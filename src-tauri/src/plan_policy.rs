//! DeepSeek Plan 入口建议与双轨的资格解析与内部发布控制。
//!
//! 权威层次（docs/archive/implementation/settings-ux-and-image-understanding.md A3）：
//! - `vendor/agent-contracts` 只定义配置 schema（客户布尔偏好）；
//! - 本模块集中实现 eligibility 与内部 release control；
//! - 客户滑钮 `planning.suggest_complex_tasks` 是唯一开关，打开即生效；
//! - `R_CODE_PLANNING_EMERGENCY_OFF` 是唯一的宿主级兜底（一键全局急停）；
//! - 存储层只保存冻结结果，不读取 Provider、Settings 或证据文件。
//!
//! 历史上的预注册证据门（manifest 嵌入 + allowlist）已于 2026-08-22 移除；
//! `eval/plan-eval/` 降级为可选的事后质量回归工具，不再阻塞功能启用。

use std::fmt;

use r_code_core::plan_entry::{
    PlanCatalogProfile, PlanComplexitySignal, PlanContextProfile, PlanEntryDecisionSource,
    ProviderRouteSnapshot, ResolvedPlanRuntimeProfile,
};
use serde::{Deserialize, Serialize};

/// 内部发布档位。`off | open` 只存在于内部诊断/发布控制，不进入普通客户设置。
/// `off` 仅在急停开关（`R_CODE_PLANNING_EMERGENCY_OFF=1`）时出现。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanningReleaseState {
    #[default]
    Off,
    Open,
}

impl PlanningReleaseState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Open => "open",
        }
    }
}

impl fmt::Display for PlanningReleaseState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Endpoint class：官方原生 API 与自定义中转是不同的证据面（docs §4.1、§16.6）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EndpointClass {
    OfficialApi,
    Relay,
}

impl EndpointClass {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OfficialApi => "official_api",
            Self::Relay => "relay",
        }
    }

    /// 只按主机名分类，绝不用显示名或 URL 子串猜测 Provider 身份。分类结果只影响
    /// 证据匹配；`provider_kind` 本身仍来自冻结配置字段。
    pub fn classify(base_url: &str) -> Self {
        let lowered = base_url.trim().to_ascii_lowercase();
        let host = lowered
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap_or_default();
        match host {
            "api.deepseek.com" | "www.deepseek.com" => Self::OfficialApi,
            _ => Self::Relay,
        }
    }
}

/// 内部发布控制合同。证据门移除后 `release_state` 只有两值：急停 = `off`，
/// 其余 = `open`。`allowed_*` 与 `evidence_version` 字段仅为审计快照与存储层
/// 兼容保留，恒为空，资格判定不再读取。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanningReleaseControl {
    pub provider_kind: String,
    pub release_state: PlanningReleaseState,
    pub emergency_off: bool,
    pub eligibility_profile_version: String,
    pub evidence_version: String,
    pub allowed_models: Vec<String>,
    pub allowed_protocols: Vec<String>,
    pub allowed_endpoint_classes: Vec<String>,
    /// 当前结论的依据（诊断层可见；不进客户文案）。
    pub basis: String,
}

fn planning_emergency_off() -> bool {
    std::env::var("R_CODE_PLANNING_EMERGENCY_OFF")
        .map(|value| value == "1")
        .unwrap_or(false)
}

pub const ELIGIBILITY_PROFILE_VERSION: &str = "deepseek-plan-v1";
/// Provider profile schema 版本（快照字段组成）。字段组成变化时递增。
pub const PROVIDER_PROFILE_VERSION: &str = "1";

/// 解析当前进程的内部发布控制：
/// 1. `R_CODE_PLANNING_EMERGENCY_OFF=1` 时急停（release_state = off）；
/// 2. 其余情况开放（release_state = open），是否启用由客户滑钮决定。
pub fn resolve_release_control() -> PlanningReleaseControl {
    let emergency_off = planning_emergency_off();
    if emergency_off {
        return PlanningReleaseControl {
            provider_kind: "deepseek".to_string(),
            release_state: PlanningReleaseState::Off,
            emergency_off: true,
            eligibility_profile_version: ELIGIBILITY_PROFILE_VERSION.to_string(),
            evidence_version: String::new(),
            allowed_models: Vec::new(),
            allowed_protocols: Vec::new(),
            allowed_endpoint_classes: Vec::new(),
            basis: "emergency off: suggestions and dual-track disabled; read-only Plan hard \
                    gate unchanged"
                .to_string(),
        };
    }
    PlanningReleaseControl {
        provider_kind: "deepseek".to_string(),
        release_state: PlanningReleaseState::Open,
        emergency_off: false,
        eligibility_profile_version: ELIGIBILITY_PROFILE_VERSION.to_string(),
        evidence_version: String::new(),
        allowed_models: Vec::new(),
        allowed_protocols: Vec::new(),
        allowed_endpoint_classes: Vec::new(),
        basis: "open release: the customer switch is the sole gate; emergency off via \
                R_CODE_PLANNING_EMERGENCY_OFF=1"
            .to_string(),
    }
}

/// 资格解析的输入：宿主从冻结 route 与任务上下文绑定的非秘密字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRouteContext {
    pub provider_name: String,
    pub provider_kind: String,
    pub model: String,
    pub wire_protocol: String,
    pub endpoint_class: EndpointClass,
}

/// 资格结论。`blocked_reason` 只进诊断/审计，绝不做客户文案。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEntryEligibility {
    pub eligible: bool,
    pub release_state: PlanningReleaseState,
    pub blocked_reason: Option<String>,
}

/// `DeepSeekPlanEligibilityResolver`：只认稳定 `provider_kind` 身份（显示名、
/// 相似 URL 都不能冒充），任意 DeepSeek 模型、任意线路（官方或中转）、任意协议
/// 均可；急停开关全局判负。
pub fn resolve_plan_entry_eligibility(
    route: &ProviderRouteContext,
    control: &PlanningReleaseControl,
) -> PlanEntryEligibility {
    // 解析顺序 1：provider_kind 不匹配直接判负，不读取 DeepSeek 的档位结论。
    if !route.provider_kind.trim().eq_ignore_ascii_case("deepseek") {
        return PlanEntryEligibility {
            eligible: false,
            release_state: control.release_state,
            blocked_reason: Some(format!(
                "provider_kind {} is not deepseek",
                route.provider_kind
            )),
        };
    }
    if control.emergency_off {
        return PlanEntryEligibility {
            eligible: false,
            release_state: control.release_state,
            blocked_reason: Some("emergency off".to_string()),
        };
    }
    if control.release_state == PlanningReleaseState::Off {
        return PlanEntryEligibility {
            eligible: false,
            release_state: control.release_state,
            blocked_reason: Some("release state is off".to_string()),
        };
    }
    PlanEntryEligibility {
        eligible: true,
        release_state: control.release_state,
        blocked_reason: None,
    }
}

/// 冻结非秘密 route 身份并计算 route revision（快照比对用，docs §4.3/§12.3）。
pub fn provider_route_snapshot(
    route: &ProviderRouteContext,
    profile_version: &str,
) -> ProviderRouteSnapshot {
    let identity = format!(
        "plan-route-v1\0{}\0{}\0{}\0{}\0{}",
        route.provider_kind,
        route.provider_name,
        route.model,
        route.wire_protocol,
        route.endpoint_class.as_str(),
    );
    ProviderRouteSnapshot {
        provider_kind: route.provider_kind.clone(),
        provider_profile_id: route.provider_name.clone(),
        provider_profile_version: profile_version.to_string(),
        provider_route_revision: sha256_hex(identity.as_bytes()),
        model_id: route.model.clone(),
        wire_protocol: route.wire_protocol.clone(),
        endpoint_class: route.endpoint_class.as_str().to_string(),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    // 稳定摘要只用于变更检测（route revision），不是密码学边界；用内置
    // 简单 FNV 组合会鼓励碰撞实验，直接复用宿主已依赖的 sha2 更直白。
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 为一个即将创建的 Plan 解析冻结运行 profile（docs §14）。所有创建路径都必须
/// 经过本函数；存储层不自己读设置。
pub fn resolve_plan_runtime_profile(
    route: &ProviderRouteContext,
    control: &PlanningReleaseControl,
    workspace_bound: bool,
    anchoring_preference: bool,
) -> ResolvedPlanRuntimeProfile {
    // docs/archive/implementation/multimodal-attachments-and-deepseek-plan-anchoring-implementation.md §8.1/§8.2：`deepseek_plan_anchoring` 与
    // `suggest_complex_tasks` 互不替代——建议开关只控制是否注册
    // propose_plan_mode；锚定开关控制实际进入 DeepSeek Plan 后是否启用最小
    // 轨迹。急停（R_CODE_PLANNING_EMERGENCY_OFF=1）同时关闭两者，但不关闭
    // Plan 的只读安全硬门。
    let eligibility = resolve_plan_entry_eligibility(route, control);
    if eligibility.eligible && workspace_bound && anchoring_preference {
        let route_snapshot = provider_route_snapshot(route, PROVIDER_PROFILE_VERSION);
        return ResolvedPlanRuntimeProfile {
            enabled: true,
            catalog_profile: PlanCatalogProfile::PlanNativeV1,
            context_profile: PlanContextProfile::MinimalV1,
            profile_version: 2,
            evidence_version: control.evidence_version.clone(),
            provider_kind: route.provider_kind.clone(),
            model_id: route.model.clone(),
            endpoint_class: route.endpoint_class.as_str().to_string(),
            protocol: route.wire_protocol.clone(),
            provider_route_revision: route_snapshot.provider_route_revision,
            anchoring_preference,
        };
    }
    ResolvedPlanRuntimeProfile::baseline()
}

/// 客户文案模板注册表（docs §9.1）。模板与优先级随客户端版本发布；模型 reason
/// 绝不直接给客户看。
pub struct CustomerCopyTemplate {
    pub key: &'static str,
    pub version: u32,
    pub lead: &'static str,
}

pub const CUSTOMER_COPY_VERSION: u32 = 1;
pub const CUSTOMER_COPY_SUFFIX: &str = "先制定计划可以让你确认范围和顺序，再开始修改。";

pub fn customer_copy_template(signal: PlanComplexitySignal) -> CustomerCopyTemplate {
    let (key, lead) = match signal {
        PlanComplexitySignal::MultiSubsystem => ("multi_subsystem", "它涉及多个相互关联的改动。"),
        PlanComplexitySignal::MigrationOrData => (
            "migration_or_data",
            "它涉及数据或兼容性变化，先确认步骤会更稳妥。",
        ),
        PlanComplexitySignal::DesignDecision => ("design_decision", "开始前有几项方案需要你确认。"),
        PlanComplexitySignal::ExpensiveRollback => (
            "expensive_rollback",
            "如果直接修改，出错后的恢复成本会比较高。",
        ),
        PlanComplexitySignal::MultiStageVerification => {
            ("multi_stage_verification", "它需要分阶段完成和验证。")
        }
    };
    CustomerCopyTemplate {
        key,
        version: CUSTOMER_COPY_VERSION,
        lead,
    }
}

/// 拒绝后的低强调说明（docs §6.2）：安静策略由辅助文案一次讲清。
pub const DECLINE_QUIET_NOTE: &str =
    "选择直接继续后，本任务不再主动弹出；你仍可随时手动选择 Plan。";

/// 状态条：Provider 切换后的一次性非阻断提示（docs §4.3）。
pub const SUPERSEDED_NOTICE: &str = "模型服务已切换，这次建议已取消；你仍可手动选择 Plan。";

/// 模型 reason 的脱敏合同：只做长度限制与控制字符清理，随后进入本地审计；
/// 普通 UI、通知、GuideSheet 和遥测都不能显示或上传它（docs §9.3）。
pub fn sanitize_reason_for_audit(reason: &str) -> String {
    let cleaned: String = reason
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect();
    let trimmed = cleaned.trim();
    trimmed.chars().take(1000).collect()
}

/// 决定来源与安静原因的映射（decline 事务写入用）。
pub fn quiet_reason_for(source: PlanEntryDecisionSource) -> Option<&'static str> {
    match source {
        PlanEntryDecisionSource::Continue => Some("continue"),
        PlanEntryDecisionSource::Close => Some("close"),
        PlanEntryDecisionSource::Escape => Some("escape"),
        PlanEntryDecisionSource::Accept => None,
    }
}

/// 宿主内存中的「建议武装」登记：run 启动时按资格解析结果武装，propose 工具
/// 执行时据此复核（目录缺席时历史诱导调用也要 fail closed）。进程内存态即可
/// ——持久权威在 plan_entry_offers 的唯一约束与 branch 预算。
#[derive(Debug, Clone)]
pub struct ArmedPlanSuggestion {
    pub route: ProviderRouteContext,
    pub control: PlanningReleaseControl,
    pub profile: ResolvedPlanRuntimeProfile,
}

#[derive(Default)]
pub struct PlanSuggestionGate {
    armed: std::sync::Mutex<std::collections::HashMap<String, ArmedPlanSuggestion>>,
}

impl PlanSuggestionGate {
    pub fn arm(&self, run_id: &str, suggestion: ArmedPlanSuggestion) {
        self.armed
            .lock()
            .expect("plan suggestion gate poisoned")
            .insert(run_id.to_string(), suggestion);
    }

    pub fn disarm(&self, run_id: &str) {
        self.armed
            .lock()
            .expect("plan suggestion gate poisoned")
            .remove(run_id);
    }

    pub fn armed(&self, run_id: &str) -> Option<ArmedPlanSuggestion> {
        self.armed
            .lock()
            .expect("plan suggestion gate poisoned")
            .get(run_id)
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(
        kind: &str,
        model: &str,
        protocol: &str,
        class: EndpointClass,
    ) -> ProviderRouteContext {
        ProviderRouteContext {
            provider_name: "DeepSeek".to_string(),
            provider_kind: kind.to_string(),
            model: model.to_string(),
            wire_protocol: protocol.to_string(),
            endpoint_class: class,
        }
    }

    fn open_control() -> PlanningReleaseControl {
        resolve_release_control()
    }

    #[test]
    fn non_deepseek_kind_fails_closed_without_reading_release_state() {
        let control = open_control();
        let eligibility = resolve_plan_entry_eligibility(
            &route("openai", "gpt-5", "openai_chat", EndpointClass::Relay),
            &control,
        );
        assert!(!eligibility.eligible);
        assert!(eligibility
            .blocked_reason
            .as_deref()
            .unwrap()
            .contains("not deepseek"));
    }

    #[test]
    fn lookalike_names_cannot_impersonate_deepseek_but_any_deepseek_route_passes() {
        let control = open_control();
        // 显示名不是身份：provider_kind 才是稳定身份；前缀相同的冒名者必须判负。
        // （尾随空白属于归一化而非冒名：identity 比较按 trim 后进行。）
        assert!(
            !resolve_plan_entry_eligibility(
                &route(
                    "deepseek-clone",
                    "deepseek-v4-flash",
                    "openai_chat",
                    EndpointClass::OfficialApi
                ),
                &control,
            )
            .eligible
        );
        assert!(
            !resolve_plan_entry_eligibility(
                &route(
                    "notdeepseek",
                    "deepseek-v4-flash",
                    "openai_chat",
                    EndpointClass::Relay
                ),
                &control,
            )
            .eligible
        );
        // 证据门移除后：任意 DeepSeek 模型、任意线路（官方或中转）、任意协议均放行。
        assert!(
            resolve_plan_entry_eligibility(
                &route(
                    "deepseek",
                    "deepseek-v4-flash",
                    "openai_chat",
                    EndpointClass::OfficialApi
                ),
                &control,
            )
            .eligible
        );
        assert!(
            resolve_plan_entry_eligibility(
                &route(
                    "deepseek",
                    "deepseek-v3-custom",
                    "openai_chat",
                    EndpointClass::Relay
                ),
                &control,
            )
            .eligible
        );
        assert!(
            resolve_plan_entry_eligibility(
                &route(
                    "deepseek",
                    "deepseek-v4-pro",
                    "anthropic_messages",
                    EndpointClass::OfficialApi
                ),
                &control,
            )
            .eligible
        );
    }

    #[test]
    fn emergency_off_disables_even_open_release() {
        let mut control = open_control();
        control.emergency_off = true;
        control.release_state = PlanningReleaseState::Off;
        assert!(
            !resolve_plan_entry_eligibility(
                &route(
                    "deepseek",
                    "deepseek-v4-flash",
                    "openai_chat",
                    EndpointClass::OfficialApi
                ),
                &control,
            )
            .eligible
        );
    }

    #[test]
    fn endpoint_class_uses_host_only() {
        assert_eq!(
            EndpointClass::classify("https://api.deepseek.com/v1"),
            EndpointClass::OfficialApi
        );
        assert_eq!(
            EndpointClass::classify("https://api.deepseek.com.example/anthropic"),
            EndpointClass::Relay
        );
        assert_eq!(
            EndpointClass::classify("https://relay.internal/api"),
            EndpointClass::Relay
        );
    }

    #[test]
    fn default_release_control_is_open_without_any_evidence() {
        // 证据门已移除：无 manifest、无实验环境变量时默认即开放，客户滑钮是唯一开关。
        std::env::remove_var("R_CODE_PLANNING_EMERGENCY_OFF");
        let control = resolve_release_control();
        assert_eq!(control.release_state, PlanningReleaseState::Open);
        assert!(!control.emergency_off);
        assert!(control.allowed_models.is_empty());
        assert!(control.allowed_protocols.is_empty());
        assert!(control.allowed_endpoint_classes.is_empty());
    }

    #[test]
    fn profile_resolution_requires_workspace_binding() {
        let control = open_control();
        let deepseek = route(
            "deepseek",
            "deepseek-v4-flash",
            "openai_chat",
            EndpointClass::OfficialApi,
        );
        let profile = resolve_plan_runtime_profile(&deepseek, &control, true, true);
        assert!(profile.enabled);
        assert_eq!(profile.catalog_profile, PlanCatalogProfile::PlanNativeV1);
        assert_eq!(profile.context_profile, PlanContextProfile::MinimalV1);
        // 未绑定 workspace 的 Plan 保持 baseline。
        assert!(!resolve_plan_runtime_profile(&deepseek, &control, false, true).enabled);
        // 非 DeepSeek 保持 baseline。
        let other = route("openai", "gpt-5", "openai_chat", EndpointClass::Relay);
        assert!(!resolve_plan_runtime_profile(&other, &control, true, true).enabled);
    }

    #[test]
    fn customer_copy_registry_is_fixed_and_localized() {
        let template = customer_copy_template(PlanComplexitySignal::MigrationOrData);
        assert_eq!(template.key, "migration_or_data");
        assert_eq!(template.version, 1);
        assert_eq!(
            template.lead,
            "它涉及数据或兼容性变化，先确认步骤会更稳妥。"
        );
    }

    #[test]
    fn reason_sanitizer_strips_control_chars_and_clamps() {
        let sanitized = sanitize_reason_for_audit("a\u{0000}b\u{0007}c");
        assert_eq!(sanitized, "a b c");
        let long: String = std::iter::repeat_n('x', 2000).collect();
        assert_eq!(sanitize_reason_for_audit(&long).chars().count(), 1000);
    }
}
