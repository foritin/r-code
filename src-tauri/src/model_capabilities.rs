//! 模型能力解析的唯一入口（docs/support/archive/implementation/multimodal-attachments-and-deepseek-plan-anchoring-implementation.md §5.1）。
//!
//! `provider_kind + model_id + selected protocol` 是能力键；显示名、URL 子串
//! 和错误消息中的厂商名不参与判定。以下调用方必须全部经由本模块，不得再
//! 各自启发：`main_model_handles_images_natively()`、Composer 的 capability
//! DTO、`build_provider_config()` 的能力字段、图片理解分派、请求预算估算、
//! Provider materialization 与 request audit。
//!
//! 目录是人工核对的一手信息：预设命中且地址未被改写时 `vision` 是 Confirmed /
//! Unsupported 的权威；中转/自建网关（地址改写）与目录未收录的模型为 Unknown
//! ——不得把目录已确认的多模态模型静默降级为 OCR，中转实际不支持时走
//! `VISION_CAPABILITY_DRIFT`。

use agent_config::{Config, ImageUnderstandingEngine};
use agent_contract::VisionBudgetProfile;
use r_code_core::dto::AgentEngine;

use crate::provider_catalog::{self, Preset};
use crate::provider_compat::{self, ProviderCompat};

/// 能力三态真值（§2.1）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityTruth {
    /// 目录人工核对确认支持。
    Confirmed,
    /// 目录确认不支持。
    Unsupported,
    /// 中转/自建网关/目录未收录：无从确认。
    Unknown,
}

/// 冻结到本次 origin request 的能力快照。后端在持有 task-local send lock 后
/// 解析一次并贯穿整条发送链路使用；运行中切换设置不得改变同一请求的判定。
#[derive(Debug, Clone)]
pub struct ResolvedModelCapabilities {
    /// 配置里的 provider key（profile 名）。
    pub provider_name: String,
    /// 稳定厂商身份（provider_kind）。
    pub provider_kind: String,
    pub model_id: String,
    /// 用户显式选择的协议标识（"anthropic_messages" / "openai_chat" /
    /// "openai_responses"）。
    pub protocol: String,
    pub vision: CapabilityTruth,
    pub vision_profile: Option<&'static str>,
    pub vision_budget: Option<VisionBudgetProfile>,
    pub context_window_tokens: Option<u32>,
    pub provider_max_output_tokens: Option<u32>,
    /// 命中的目录预设（地址未被改写时 Some）。
    pub matched_preset: Option<&'static Preset>,
    /// 声明式兼容层快照（PRD pi-alignment M1-01）：硬编码厂商事实经
    /// `provider_compat::effective_compat` 合成；用户 provider/model 级覆盖
    /// 随声明式端点配置（M1-02）接入，当前喂空层（= 纯硬编码默认）。
    pub compat: ProviderCompat,
}

/// 按配置解析任务主模型的能力。`provider_name`/`model_override` 为任务绑定值；
/// None 时回退全局默认服务（与运行时 route 解析同规则）。
pub fn resolve(
    config: &Config,
    agent_engine: AgentEngine,
    provider_name: Option<&str>,
    model_override: Option<&str>,
) -> ResolvedModelCapabilities {
    // Codex 主 Agent 的附件由 Codex 自身模型目录处理；引擎侧不做图片路由。
    if agent_engine == AgentEngine::Codex {
        return ResolvedModelCapabilities {
            provider_name: String::new(),
            provider_kind: "codex".to_string(),
            model_id: String::new(),
            protocol: "codex".to_string(),
            vision: CapabilityTruth::Confirmed,
            vision_profile: None,
            vision_budget: None,
            context_window_tokens: None,
            provider_max_output_tokens: None,
            matched_preset: None,
            // codex 非目录厂商身份：无硬编码事实，全空（通用行为）。
            compat: ProviderCompat::default(),
        };
    }
    let provider_name = provider_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(config.default_provider.as_str())
        .to_string();
    let provider = config.providers.get(&provider_name);
    let model_id = model_override
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            provider
                .map(|provider| provider.model.as_str())
                .unwrap_or_default()
        })
        .to_string();
    let protocol = provider
        .map(|provider| {
            match provider.protocol.as_deref() {
                // 用户显式选择的协议标签优先（不做再映射）。
                Some(selected) => selected.to_string(),
                None => protocol_label(provider_catalog::resolve_protocol(
                    provider.provider_kind.as_deref().unwrap_or(&provider_name),
                    &provider.base_url,
                ))
                .to_string(),
            }
        })
        .unwrap_or_else(|| "openai_chat".to_string());

    let provider_kind = provider
        .and_then(|provider| provider.provider_kind.clone())
        .unwrap_or_default();
    // 合成点：compat 随能力快照一次性冻结（用户层暂空，M1-02 接入声明式端点）。
    let compat = provider_compat::effective_compat(
        &provider_kind,
        &ProviderCompat::default(),
        &ProviderCompat::default(),
    );
    let mut resolved = ResolvedModelCapabilities {
        provider_name,
        provider_kind,
        model_id,
        protocol,
        vision: CapabilityTruth::Unknown,
        vision_profile: None,
        vision_budget: None,
        context_window_tokens: None,
        provider_max_output_tokens: None,
        matched_preset: None,
        compat,
    };
    let Some(provider) = provider else {
        return resolved;
    };
    if resolved.model_id.is_empty() {
        return resolved;
    }
    // 只信目录且地址未被改写的预设标注；中转/自建网关的能力无从确认。
    let identity = provider.provider_kind.as_deref().unwrap_or("");
    let preset = provider_catalog::preset_for(
        if identity.is_empty() {
            &resolved.provider_name
        } else {
            identity
        },
        &provider.base_url,
    );
    let Some(preset) = preset else {
        return resolved;
    };
    resolved.matched_preset = Some(preset);
    resolved.context_window_tokens = preset.context_window;
    resolved.provider_max_output_tokens = preset.max_output_tokens;
    let entry = preset
        .models
        .iter()
        .find(|entry| entry.id == resolved.model_id);
    let Some(entry) = entry else {
        // 同一预设下的手填/同步模型：目录未收录 → Unknown。
        return resolved;
    };
    resolved.vision = if entry.vision {
        CapabilityTruth::Confirmed
    } else {
        CapabilityTruth::Unsupported
    };
    if entry.vision {
        resolved.vision_budget = provider_catalog::vision_budget_for(preset, &resolved.model_id);
        resolved.vision_profile = resolved.vision_budget.map(|profile| profile.profile_id);
    }
    resolved
}

fn protocol_label(protocol: crate::provider_catalog::Protocol) -> &'static str {
    match protocol {
        crate::provider_catalog::Protocol::AnthropicMessages => "anthropic_messages",
        crate::provider_catalog::Protocol::OpenAiChat => "openai_chat",
        crate::provider_catalog::Protocol::OpenAiResponses => "openai_responses",
    }
}

/// §2.1 真值表驱动的图片路由决策（纯函数，可单测）。返回 None = 该请求没有
/// 图片附件需要路由。
///
/// | vision | 路径 | 失败行为 |
/// | --- | --- | --- |
/// | Confirmed | NativeMainVision（原图直发当前主模型） | 拒图 → VISION_CAPABILITY_DRIFT，OCR/helper = 0 |
/// | Unsupported | 按设置显式选择 OCR 或独立视觉模型 | 所选引擎失败即返回错误 |
/// | Unknown | 仅按用户显式选择的引擎；未配置则阻断 | 提示能力未知，不得猜多模态或静默 OCR |
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageDeliveryRouteV1 {
    NativeMainVision {
        route_revision: String,
        vision_profile: String,
    },
    OcrForTextMain {
        engine: String,
    },
    VisionHelperForTextMain {
        provider: String,
        model: String,
        route_revision: String,
    },
}

impl ImageDeliveryRouteV1 {
    /// 排队消息持久化与审计用的稳定标签。
    pub fn route_label(&self) -> &'static str {
        match self {
            ImageDeliveryRouteV1::NativeMainVision { .. } => "native_main_vision",
            ImageDeliveryRouteV1::OcrForTextMain { .. } => "ocr_for_text_main",
            ImageDeliveryRouteV1::VisionHelperForTextMain { .. } => "vision_helper_for_text_main",
        }
    }
}

/// 图片路由决策错误（发送前阻断，Provider/OCR/helper 调用均为 0）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageRouteError {
    /// Unknown 且未完成图片理解配置。
    UnknownCapabilityUnconfigured,
    /// 引擎为 model 但服务/模型缺失。
    HelperEngineMisconfigured(String),
}

/// 冻结 route revision：能力键（kind+model+protocol）的内容 hash 短前缀。
/// route 漂移按此比较（§8.7 / §4.4 route snapshot）。
pub fn route_revision(capabilities: &ResolvedModelCapabilities) -> String {
    let key = format!(
        "{}|{}|{}",
        capabilities.provider_kind, capabilities.model_id, capabilities.protocol
    );
    let digest = blake3::hash(key.as_bytes());
    let hex = digest.to_hex();
    hex.chars().take(16).collect()
}

/// 决策函数：`has_image_attachments` 表示本次发送携带图片（png/jpeg/gif/webp）。
pub fn resolve_image_delivery_route(
    capabilities: &ResolvedModelCapabilities,
    config: &Config,
    has_image_attachments: bool,
) -> Result<Option<ImageDeliveryRouteV1>, ImageRouteError> {
    if !has_image_attachments {
        return Ok(None);
    }
    match capabilities.vision {
        CapabilityTruth::Confirmed => {
            // 主模型 Confirmed 时即使设置页选择了 OCR 也必须优先原图直发
            //（§2.1）；图片理解引擎只服务不能直接读图的主模型。
            Ok(Some(ImageDeliveryRouteV1::NativeMainVision {
                route_revision: route_revision(capabilities),
                vision_profile: capabilities.vision_profile.unwrap_or("missing").to_string(),
            }))
        }
        CapabilityTruth::Unsupported => match config.image_understanding.engine {
            ImageUnderstandingEngine::Ocr => Ok(Some(ImageDeliveryRouteV1::OcrForTextMain {
                engine: "system_ocr".to_string(),
            })),
            ImageUnderstandingEngine::Model => {
                let provider = config
                    .image_understanding
                    .model_provider
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        ImageRouteError::HelperEngineMisconfigured(
                            "图片理解引擎为视觉模型，但未配置服务".to_string(),
                        )
                    })?
                    .to_string();
                let model = config
                    .image_understanding
                    .model
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        ImageRouteError::HelperEngineMisconfigured(
                            "图片理解引擎为视觉模型，但未配置模型".to_string(),
                        )
                    })?
                    .to_string();
                Ok(Some(ImageDeliveryRouteV1::VisionHelperForTextMain {
                    provider,
                    model,
                    route_revision: route_revision(capabilities),
                }))
            }
        },
        CapabilityTruth::Unknown => {
            // 仅按用户显式选择的图片理解引擎处理；未完成配置则阻断，
            // 不得猜成多模态，也不得静默 OCR。
            match config.image_understanding.engine {
                ImageUnderstandingEngine::Ocr => Ok(Some(ImageDeliveryRouteV1::OcrForTextMain {
                    engine: "system_ocr".to_string(),
                })),
                ImageUnderstandingEngine::Model => {
                    let configured = config
                        .image_understanding
                        .model_provider
                        .as_deref()
                        .is_some_and(|value| !value.trim().is_empty())
                        && config
                            .image_understanding
                            .model
                            .as_deref()
                            .is_some_and(|value| !value.trim().is_empty());
                    if !configured {
                        return Err(ImageRouteError::UnknownCapabilityUnconfigured);
                    }
                    Ok(Some(ImageDeliveryRouteV1::VisionHelperForTextMain {
                        provider: config
                            .image_understanding
                            .model_provider
                            .clone()
                            .unwrap_or_default(),
                        model: config.image_understanding.model.clone().unwrap_or_default(),
                        route_revision: route_revision(capabilities),
                    }))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_config::{Config, ImageUnderstandingConfig, ProviderConfig};

    fn deepseek_config(model: &str, base_url: &str) -> Config {
        let mut config = Config {
            default_provider: "deepseek-main".to_string(),
            ..Config::default()
        };
        config.providers.insert(
            "deepseek-main".to_string(),
            ProviderConfig {
                base_url: base_url.to_string(),
                api_key: "k".to_string(),
                model: model.to_string(),
                provider_kind: Some("deepseek".to_string()),
                max_tokens: None,
                temperature: None,
                protocol: None,
                show_reasoning: true,
            },
        );
        config
    }

    #[test]
    fn vision_truth_by_catalog_entry() {
        let config = deepseek_config("deepseek-v4-flash-vision-exp", "https://api.deepseek.com");
        let resolved = resolve(&config, AgentEngine::RCode, None, None);
        assert_eq!(resolved.vision, CapabilityTruth::Confirmed);
        assert_eq!(resolved.provider_kind, "deepseek");
        assert_eq!(resolved.vision_profile, Some("deepseek_vision_exp_v1"));
        assert_eq!(
            resolved.vision_budget.unwrap().image_tokens(1_818, 1_026),
            32_000
        );

        let config = deepseek_config("deepseek-v4-flash", "https://api.deepseek.com");
        let resolved = resolve(&config, AgentEngine::RCode, None, None);
        assert_eq!(resolved.vision, CapabilityTruth::Unsupported);
    }

    /// 地址被改写（中转）→ Unknown：目录已确认的模型不得因中转被静默降级
    /// OCR——是否可读图由拒图后的 VISION_CAPABILITY_DRIFT 呈现。
    #[test]
    fn rewritten_base_url_yields_unknown_truth() {
        let config = deepseek_config(
            "deepseek-v4-flash-vision-exp",
            "https://my-relay.example.com/v1",
        );
        let resolved = resolve(&config, AgentEngine::RCode, None, None);
        assert_eq!(resolved.vision, CapabilityTruth::Unknown);
        assert!(resolved.vision_budget.is_none());
    }

    /// §2.1 真值表：Confirmed 直发（覆盖 OCR 设置）；Unsupported 按显式引擎；
    /// Unknown 未配置时阻断。
    #[test]
    fn route_truth_table() {
        let mut config =
            deepseek_config("deepseek-v4-flash-vision-exp", "https://api.deepseek.com");
        config.image_understanding = ImageUnderstandingConfig {
            engine: agent_config::ImageUnderstandingEngine::Ocr,
            model_provider: None,
            model: None,
        };
        let vision = resolve(&config, AgentEngine::RCode, None, None);
        // Confirmed 优先 NativeMainVision，即使引擎选了 OCR。
        let route = resolve_image_delivery_route(&vision, &config, true)
            .unwrap()
            .unwrap();
        assert!(matches!(
            route,
            ImageDeliveryRouteV1::NativeMainVision { .. }
        ));
        assert_eq!(route.route_label(), "native_main_vision");
        // 无图片附件 → 无路由。
        assert!(resolve_image_delivery_route(&vision, &config, false)
            .unwrap()
            .is_none());

        // Unsupported + OCR 引擎。
        let mut config = deepseek_config("deepseek-v4-flash", "https://api.deepseek.com");
        config.image_understanding = ImageUnderstandingConfig {
            engine: agent_config::ImageUnderstandingEngine::Ocr,
            model_provider: None,
            model: None,
        };
        let text_main = resolve(&config, AgentEngine::RCode, None, None);
        let route = resolve_image_delivery_route(&text_main, &config, true)
            .unwrap()
            .unwrap();
        assert!(matches!(route, ImageDeliveryRouteV1::OcrForTextMain { .. }));

        // Unknown（中转）+ 未完成引擎配置 → 阻断，不得静默 OCR。
        let mut config = deepseek_config("some-model", "https://relay.example.com/v1");
        config.image_understanding = ImageUnderstandingConfig {
            engine: agent_config::ImageUnderstandingEngine::Ocr,
            model_provider: None,
            model: None,
        };
        config
            .providers
            .get_mut("deepseek-main")
            .unwrap()
            .provider_kind = None;
        let unknown = resolve(&config, AgentEngine::RCode, None, None);
        assert_eq!(unknown.vision, CapabilityTruth::Unknown);
        // OCR 是用户显式选择 → 允许（显式引擎路径）。
        assert!(resolve_image_delivery_route(&unknown, &config, true).is_ok());
        // model 引擎配置不完整 → 阻断。
        let mut config = deepseek_config("some-model", "https://relay.example.com/v1");
        config.image_understanding = ImageUnderstandingConfig {
            engine: agent_config::ImageUnderstandingEngine::Model,
            model_provider: Some("helper".to_string()),
            model: None,
        };
        config
            .providers
            .get_mut("deepseek-main")
            .unwrap()
            .provider_kind = None;
        let unknown = resolve(&config, AgentEngine::RCode, None, None);
        // Unknown + model 引擎配置不完整 → 明确阻断（不得猜多模态/静默 OCR）。
        assert_eq!(
            resolve_image_delivery_route(&unknown, &config, true).unwrap_err(),
            ImageRouteError::UnknownCapabilityUnconfigured
        );
    }

    /// M1-01 合成点接线：能力快照携带厂商直连 compat 事实（DeepSeek 自动
    /// 前缀缓存、无 reasoning_effort）；目录外 kind 为全空通用行为。
    #[test]
    fn resolved_capabilities_carry_builtin_compat() {
        let config = deepseek_config("deepseek-v4-flash", "https://api.deepseek.com");
        let resolved = resolve(&config, AgentEngine::RCode, None, None);
        assert_eq!(resolved.compat.supports_prompt_caching, Some(true));
        assert_eq!(resolved.compat.supports_reasoning_effort, Some(false));
        assert_eq!(
            resolved.compat.session_affinity_format,
            Some(crate::provider_compat::SessionAffinityFormat::Implicit)
        );

        let mut relay = deepseek_config("some-model", "https://relay.example.com/v1");
        relay
            .providers
            .get_mut("deepseek-main")
            .unwrap()
            .provider_kind = None;
        let resolved = resolve(&relay, AgentEngine::RCode, None, None);
        assert_eq!(
            resolved.compat,
            crate::provider_compat::ProviderCompat::default(),
            "unknown kind yields empty (generic) compat"
        );

        let codex = resolve(&Config::default(), AgentEngine::Codex, None, None);
        assert_eq!(
            codex.compat,
            crate::provider_compat::ProviderCompat::default()
        );
    }

    /// route revision 由能力键决定：同 kind+model+protocol 稳定，切换模型即变。
    #[test]
    fn route_revision_stability() {
        let a = deepseek_config("deepseek-v4-flash-vision-exp", "https://api.deepseek.com");
        let b = deepseek_config("deepseek-v4-flash", "https://api.deepseek.com");
        let ra = resolve(&a, AgentEngine::RCode, None, None);
        let rb = resolve(&b, AgentEngine::RCode, None, None);
        assert_eq!(route_revision(&ra), route_revision(&ra));
        assert_ne!(route_revision(&ra), route_revision(&rb));
        assert_eq!(route_revision(&ra).len(), 16);
    }
}
