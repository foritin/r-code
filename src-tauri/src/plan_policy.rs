//! DeepSeek Plan 入口建议与双轨的资格解析、内部发布控制与证据门。
//!
//! 权威层次（docs/plan-mode-dual-track-gate.md §14.1、§15、§16）：
//! - `vendor/agent-contracts` 只定义配置 schema（客户布尔偏好）；
//! - 本模块集中实现 eligibility、内部 release control 与 frozen profile 解析；
//! - `build.rs` 把通过验证的证据 manifest 嵌入 `OUT_DIR`，本模块 include 并重验；
//! - 存储层只保存冻结结果，不读取 Provider、Settings 或证据文件。
//!
//! 一切证据未通过、manifest 缺失或 route 不匹配的路径都 fail closed：产品保持
//! baseline（普通 Agent 目录 + baseline Plan + 原生只读硬门）。

use std::fmt;

use r_code_core::plan_entry::{
    PlanCatalogProfile, PlanComplexitySignal, PlanContextProfile, PlanEntryDecisionSource,
    ProviderRouteSnapshot, ResolvedPlanRuntimeProfile,
};
use serde::{Deserialize, Serialize};

/// Build 时由 `build.rs` 嵌入的证据 manifest（缺失/无效时为字面 `null`）。
const EMBEDDED_PLAN_EVIDENCE_MANIFEST: &str =
    include_str!(concat!(env!("OUT_DIR"), "/plan_evidence_manifest.json"));

/// 内部发布档位。`off | experiment | validated` 只存在于内部诊断/发布控制，
/// 不进入普通客户设置（docs §15.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PlanningReleaseState {
    #[default]
    Off,
    Experiment,
    Validated,
}

impl PlanningReleaseState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Experiment => "experiment",
            Self::Validated => "validated",
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

/// 内部发布控制合同（docs §15.2 `PlanningReleaseControlV1`）。
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
    /// 证据门为何是这个结论（诊断层可见；不进客户文案）。
    pub basis: String,
}

/// 解析后的证据 manifest（score.mjs 产物；schema 见 eval/plan-eval）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanEvidenceManifest {
    pub schema: String,
    pub provider_kind: String,
    pub eligibility_profile_version: String,
    pub evidence_version: String,
    pub allowed_models: Vec<String>,
    pub allowed_protocols: Vec<String>,
    pub allowed_endpoint_classes: Vec<String>,
    pub preregistration_sha256: String,
    pub corpus_lock_sha256: String,
    pub capability: ManifestCapabilityGates,
    pub routing: ManifestRoutingGates,
    pub raw_results_count: u32,
    #[serde(default)]
    pub raw_results_digest: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestCapabilityGates {
    pub records: u32,
    pub net_solved_gain: i64,
    pub regressions: i64,
    pub mcnemar_p_exact_one_sided: f64,
    pub unapproved_side_effects: u32,
    pub dual_median_tokens_ratio: f64,
    pub dual_p95_wall_time_ratio: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestRoutingGates {
    pub records: u32,
    pub simple_false_prompt_rate: f64,
    pub complex_recall_rate: f64,
    pub same_request_repeat_rate: f64,
}

/// 预注册发布门的冻结阈值（docs §16.5）。修改阈值必须以新证据版本重跑完整评估，
/// 不能挑 case 补跑覆盖原结论。
pub mod preregistered_gates {
    pub const MIN_NET_SOLVED_GAIN: i64 = 4;
    pub const MAX_REGRESSIONS: i64 = 1;
    pub const MAX_MCNEMAR_P: f64 = 0.10;
    pub const MAX_UNAPPROVED_SIDE_EFFECTS: u32 = 0;
    pub const MAX_SIMPLE_FALSE_PROMPT_RATE: f64 = 0.10;
    pub const MIN_COMPLEX_RECALL_RATE: f64 = 0.80;
    pub const MAX_SAME_REQUEST_REPEAT_RATE: f64 = 0.0;
    pub const MAX_DUAL_MEDIAN_TOKENS_RATIO: f64 = 1.20;
    pub const MAX_DUAL_P95_WALL_TIME_RATIO: f64 = 1.30;
    pub const CAPABILITY_RECORDS: u32 = 75;
    pub const ROUTING_RECORDS: u32 = 40;
    pub const MANIFEST_SCHEMA: &str = "r-code-plan-evidence-manifest/v1";
}

/// 独立重验嵌入的 manifest：数量、门限、schema 与 provider 全部重算（docs §16.4
/// 验证器要求）。任何一项失败都视为无证据。
pub fn load_validated_manifest() -> Option<PlanEvidenceManifest> {
    let manifest: PlanEvidenceManifest =
        serde_json::from_str(EMBEDDED_PLAN_EVIDENCE_MANIFEST).ok()?;
    validate_manifest(&manifest).ok()?;
    Some(manifest)
}

/// 门校验失败的具体原因（诊断用）。
pub fn validate_manifest(manifest: &PlanEvidenceManifest) -> Result<(), String> {
    use preregistered_gates as gates;
    if manifest.schema != gates::MANIFEST_SCHEMA {
        return Err(format!("manifest schema mismatch: {}", manifest.schema));
    }
    if manifest.provider_kind != "deepseek" {
        return Err(format!(
            "manifest provider_kind is not deepseek: {}",
            manifest.provider_kind
        ));
    }
    let capability = &manifest.capability;
    if capability.records != gates::CAPABILITY_RECORDS {
        return Err(format!(
            "capability records must be {}: {}",
            gates::CAPABILITY_RECORDS,
            capability.records
        ));
    }
    if capability.net_solved_gain < gates::MIN_NET_SOLVED_GAIN {
        return Err(format!(
            "net solved gain {} below preregistered minimum {}",
            capability.net_solved_gain,
            gates::MIN_NET_SOLVED_GAIN
        ));
    }
    if capability.regressions > gates::MAX_REGRESSIONS {
        return Err(format!(
            "regressions {} exceed preregistered maximum {}",
            capability.regressions,
            gates::MAX_REGRESSIONS
        ));
    }
    if capability.mcnemar_p_exact_one_sided > gates::MAX_MCNEMAR_P {
        return Err(format!(
            "mcnemar p {:.4} exceeds preregistered maximum {:.2}",
            capability.mcnemar_p_exact_one_sided,
            gates::MAX_MCNEMAR_P
        ));
    }
    if capability.unapproved_side_effects > gates::MAX_UNAPPROVED_SIDE_EFFECTS {
        return Err(format!(
            "unapproved side effects {} must be 0",
            capability.unapproved_side_effects
        ));
    }
    if capability.dual_median_tokens_ratio > gates::MAX_DUAL_MEDIAN_TOKENS_RATIO {
        return Err(format!(
            "dual median tokens ratio {:.3} exceeds {:.2}",
            capability.dual_median_tokens_ratio,
            gates::MAX_DUAL_MEDIAN_TOKENS_RATIO
        ));
    }
    if capability.dual_p95_wall_time_ratio > gates::MAX_DUAL_P95_WALL_TIME_RATIO {
        return Err(format!(
            "dual p95 wall time ratio {:.3} exceeds {:.2}",
            capability.dual_p95_wall_time_ratio,
            gates::MAX_DUAL_P95_WALL_TIME_RATIO
        ));
    }
    let routing = &manifest.routing;
    if routing.records != gates::ROUTING_RECORDS {
        return Err(format!(
            "routing records must be {}: {}",
            gates::ROUTING_RECORDS,
            routing.records
        ));
    }
    if routing.simple_false_prompt_rate > gates::MAX_SIMPLE_FALSE_PROMPT_RATE {
        return Err(format!(
            "simple false prompt rate {:.3} exceeds {:.2}",
            routing.simple_false_prompt_rate,
            gates::MAX_SIMPLE_FALSE_PROMPT_RATE
        ));
    }
    if routing.complex_recall_rate < gates::MIN_COMPLEX_RECALL_RATE {
        return Err(format!(
            "complex recall rate {:.3} below preregistered minimum {:.2}",
            routing.complex_recall_rate,
            gates::MIN_COMPLEX_RECALL_RATE
        ));
    }
    if routing.same_request_repeat_rate > gates::MAX_SAME_REQUEST_REPEAT_RATE {
        return Err(format!(
            "same request repeat rate {:.3} must be 0",
            routing.same_request_repeat_rate
        ));
    }
    if manifest.raw_results_count != gates::CAPABILITY_RECORDS + gates::ROUTING_RECORDS {
        return Err(format!(
            "raw results count {} does not match {} capability + {} routing",
            manifest.raw_results_count,
            gates::CAPABILITY_RECORDS,
            gates::ROUTING_RECORDS
        ));
    }
    if manifest.preregistration_sha256.trim().is_empty()
        || manifest.corpus_lock_sha256.trim().is_empty()
        || manifest.raw_results_digest.trim().is_empty()
    {
        return Err("raw artifact / preregistration digests must be present".to_string());
    }
    Ok(())
}

fn planning_emergency_off() -> bool {
    std::env::var("R_CODE_PLANNING_EMERGENCY_OFF")
        .map(|value| value == "1")
        .unwrap_or(false)
}

fn planning_experiment_env() -> bool {
    std::env::var("R_CODE_PLANNING_EXPERIMENT")
        .map(|value| value == "1")
        .unwrap_or(false)
}

/// 内部实验档的固定 allowlist：DeepSeek 原生 API + 冻结 v4 模型 + 目录协议
///（docs §15.2：experiment 不能从普通 Settings 选择）。
const EXPERIMENT_ALLOWED_MODELS: &[&str] = &["deepseek-v4-flash", "deepseek-v4-pro"];
const EXPERIMENT_ALLOWED_PROTOCOLS: &[&str] =
    &["openai_chat", "openai_responses", "anthropic_messages"];
const EXPERIMENT_ALLOWED_ENDPOINT_CLASSES: &[&str] = &["official_api"];
pub const ELIGIBILITY_PROFILE_VERSION: &str = "deepseek-plan-v1";
/// Provider profile schema 版本（快照字段组成）。字段组成变化时递增。
pub const PROVIDER_PROFILE_VERSION: &str = "1";

/// 解析当前进程的内部发布控制。解析顺序遵循 docs §15.2：
/// 1. 非 deepseek 在 resolver 中直接判负（不读取 DeepSeek 结果）；
/// 2. emergency off 关闭建议与双轨；
/// 3. validated 只认嵌入且通过独立重验的 manifest；
/// 4. experiment 只在内部环境变量开启时可用。
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
    if let Some(manifest) = load_validated_manifest() {
        return PlanningReleaseControl {
            provider_kind: "deepseek".to_string(),
            release_state: PlanningReleaseState::Validated,
            emergency_off: false,
            eligibility_profile_version: manifest.eligibility_profile_version.clone(),
            evidence_version: manifest.evidence_version.clone(),
            allowed_models: manifest.allowed_models.clone(),
            allowed_protocols: manifest.allowed_protocols.clone(),
            allowed_endpoint_classes: manifest.allowed_endpoint_classes.clone(),
            basis: format!(
                "embedded evidence manifest validated (evidence_version={}, prereg={})",
                manifest.evidence_version, manifest.preregistration_sha256
            ),
        };
    }
    if planning_experiment_env() {
        return PlanningReleaseControl {
            provider_kind: "deepseek".to_string(),
            release_state: PlanningReleaseState::Experiment,
            emergency_off: false,
            eligibility_profile_version: ELIGIBILITY_PROFILE_VERSION.to_string(),
            evidence_version: String::new(),
            allowed_models: EXPERIMENT_ALLOWED_MODELS
                .iter()
                .map(|model| (*model).to_string())
                .collect(),
            allowed_protocols: EXPERIMENT_ALLOWED_PROTOCOLS
                .iter()
                .map(|protocol| (*protocol).to_string())
                .collect(),
            allowed_endpoint_classes: EXPERIMENT_ALLOWED_ENDPOINT_CLASSES
                .iter()
                .map(|class| (*class).to_string())
                .collect(),
            basis: "internal experiment environment (R_CODE_PLANNING_EXPERIMENT=1); \
                    allowlisted official DeepSeek routes only"
                .to_string(),
        };
    }
    PlanningReleaseControl {
        provider_kind: "deepseek".to_string(),
        release_state: PlanningReleaseState::Off,
        emergency_off: false,
        eligibility_profile_version: ELIGIBILITY_PROFILE_VERSION.to_string(),
        evidence_version: String::new(),
        allowed_models: Vec::new(),
        allowed_protocols: Vec::new(),
        allowed_endpoint_classes: Vec::new(),
        basis: "no validated evidence manifest embedded; baseline Plan stays authoritative"
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

/// `DeepSeekPlanEligibilityResolver`（docs M0-02）：只认稳定 `provider_kind` 与证据
/// 匹配的 model/protocol/endpoint；其他 Provider fail closed。显示名、相似 URL 都
/// 不能冒充。
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
    if !control
        .allowed_models
        .iter()
        .any(|model| model == &route.model)
    {
        return PlanEntryEligibility {
            eligible: false,
            release_state: control.release_state,
            blocked_reason: Some(format!(
                "model {} is not in the evidence allowlist",
                route.model
            )),
        };
    }
    if !control
        .allowed_protocols
        .iter()
        .any(|protocol| protocol == &route.wire_protocol)
    {
        return PlanEntryEligibility {
            eligible: false,
            release_state: control.release_state,
            blocked_reason: Some(format!(
                "wire protocol {} is not in the evidence allowlist",
                route.wire_protocol
            )),
        };
    }
    if !control
        .allowed_endpoint_classes
        .iter()
        .any(|class| class == route.endpoint_class.as_str())
    {
        return PlanEntryEligibility {
            eligible: false,
            release_state: control.release_state,
            blocked_reason: Some(format!(
                "endpoint class {} is not covered by the evidence manifest",
                route.endpoint_class.as_str()
            )),
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
) -> ResolvedPlanRuntimeProfile {
    let eligibility = resolve_plan_entry_eligibility(route, control);
    if eligibility.eligible && workspace_bound {
        return ResolvedPlanRuntimeProfile {
            enabled: true,
            catalog_profile: PlanCatalogProfile::PlanNativeV1,
            context_profile: PlanContextProfile::MinimalV1,
            profile_version: 1,
            evidence_version: control.evidence_version.clone(),
            provider_kind: route.provider_kind.clone(),
            model_id: route.model.clone(),
            endpoint_class: route.endpoint_class.as_str().to_string(),
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

    fn validated_control() -> PlanningReleaseControl {
        PlanningReleaseControl {
            provider_kind: "deepseek".to_string(),
            release_state: PlanningReleaseState::Validated,
            emergency_off: false,
            eligibility_profile_version: ELIGIBILITY_PROFILE_VERSION.to_string(),
            evidence_version: "test-1".to_string(),
            allowed_models: vec!["deepseek-v4-flash".to_string()],
            allowed_protocols: vec!["openai_chat".to_string()],
            allowed_endpoint_classes: vec!["official_api".to_string()],
            basis: "test".to_string(),
        }
    }

    #[test]
    fn non_deepseek_kind_fails_closed_without_reading_release_state() {
        let control = validated_control();
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
    fn lookalike_names_and_relays_cannot_impersonate_deepseek() {
        let control = validated_control();
        // 显示名不是身份：provider_kind 才是稳定身份。
        assert!(
            !resolve_plan_entry_eligibility(
                &route(
                    "deepseek ",
                    "deepseek-v4-flash",
                    "openai_chat",
                    EndpointClass::Relay
                ),
                &control,
            )
            .eligible
        );
        // relay endpoint class 未被 manifest 覆盖时 fail closed。
        assert!(
            !resolve_plan_entry_eligibility(
                &route(
                    "deepseek",
                    "deepseek-v4-flash",
                    "openai_chat",
                    EndpointClass::Relay
                ),
                &control,
            )
            .eligible
        );
        // 模型/协议不在 allowlist 时 fail closed。
        assert!(
            !resolve_plan_entry_eligibility(
                &route(
                    "deepseek",
                    "deepseek-v3",
                    "openai_chat",
                    EndpointClass::OfficialApi
                ),
                &control,
            )
            .eligible
        );
        // 全部匹配才 eligible。
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
    }

    #[test]
    fn emergency_off_disables_even_validated_release() {
        let mut control = validated_control();
        control.emergency_off = true;
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
    fn default_release_control_fails_closed_without_evidence() {
        // 本仓库尚无真实证据 manifest：默认解析必须是 off（除非实验环境变量）。
        std::env::remove_var("R_CODE_PLANNING_EXPERIMENT");
        std::env::remove_var("R_CODE_PLANNING_EMERGENCY_OFF");
        let control = resolve_release_control();
        assert_eq!(control.release_state, PlanningReleaseState::Off);
    }

    #[test]
    fn manifest_gate_math_is_revalidated() {
        let mut manifest = PlanEvidenceManifest {
            schema: preregistered_gates::MANIFEST_SCHEMA.to_string(),
            provider_kind: "deepseek".to_string(),
            eligibility_profile_version: "deepseek-plan-v1".to_string(),
            evidence_version: "test".to_string(),
            allowed_models: vec!["deepseek-v4-flash".to_string()],
            allowed_protocols: vec!["openai_chat".to_string()],
            allowed_endpoint_classes: vec!["official_api".to_string()],
            preregistration_sha256: "a".to_string(),
            corpus_lock_sha256: "b".to_string(),
            capability: ManifestCapabilityGates {
                records: 75,
                net_solved_gain: 6,
                regressions: 0,
                mcnemar_p_exact_one_sided: 0.037,
                unapproved_side_effects: 0,
                dual_median_tokens_ratio: 1.05,
                dual_p95_wall_time_ratio: 1.1,
            },
            routing: ManifestRoutingGates {
                records: 40,
                simple_false_prompt_rate: 0.05,
                complex_recall_rate: 0.9,
                same_request_repeat_rate: 0.0,
            },
            raw_results_count: 115,
            raw_results_digest: "c".to_string(),
        };
        assert!(validate_manifest(&manifest).is_ok());
        manifest.capability.net_solved_gain = 3;
        assert!(validate_manifest(&manifest).is_err());
        manifest.capability.net_solved_gain = 6;
        manifest.capability.records = 74;
        assert!(validate_manifest(&manifest).is_err());
        manifest.capability.records = 75;
        manifest.routing.complex_recall_rate = 0.7;
        assert!(validate_manifest(&manifest).is_err());
        manifest.routing.complex_recall_rate = 0.9;
        manifest.provider_kind = "openai".to_string();
        assert!(validate_manifest(&manifest).is_err());
    }

    #[test]
    fn profile_resolution_requires_workspace_binding() {
        let control = validated_control();
        let deepseek = route(
            "deepseek",
            "deepseek-v4-flash",
            "openai_chat",
            EndpointClass::OfficialApi,
        );
        let profile = resolve_plan_runtime_profile(&deepseek, &control, true);
        assert!(profile.enabled);
        assert_eq!(profile.catalog_profile, PlanCatalogProfile::PlanNativeV1);
        assert_eq!(profile.context_profile, PlanContextProfile::MinimalV1);
        // 未绑定 workspace 的 Plan 保持 baseline。
        assert!(!resolve_plan_runtime_profile(&deepseek, &control, false).enabled);
        // 非 DeepSeek 保持 baseline。
        let other = route("openai", "gpt-5", "openai_chat", EndpointClass::Relay);
        assert!(!resolve_plan_runtime_profile(&other, &control, true).enabled);
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
