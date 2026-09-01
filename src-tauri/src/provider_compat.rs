//! ProviderCompat 声明式兼容层（docs/pi-alignment PRD §4.1 R-PRV-01 / M1-01）。
//!
//! 目标：残缺 OpenAI 兼容端点有一个统一的"关字段"入口——模型/Provider 哪些
//! 参数可发、会话亲和用什么 wire 形态，全部声明式合成，不再散落在各调用点的
//! if/else 里。合成层级（后者覆盖前者）：
//!
//! 1. 硬编码默认 [`builtin_compat`]：厂商直连身份（deepseek / kimi_coding /
//!    ark_*）的已核实事实，从 `agent_llm::dialect` 的线路行为推导；
//! 2. 用户 provider 级 compat；
//! 3. 用户 model 级 compat（覆盖 provider 级）。
//!
//! 安全不变量：厂商直连身份的硬编码声明是 R-Code 已测试行为的一部分，用户
//! compat 不得翻转（只能补空白）——见 [`protected_merge`]。这样"接一个残缺端点
//! 关掉 reasoning_effort"与"DeepSeek 前缀缓存事实不被误关"两个诉求同时成立。

use serde::{Deserialize, Serialize};

/// 会话亲和的 wire 表达格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionAffinityFormat {
    /// 无会话亲和：每请求自包含全量上下文。
    #[default]
    None,
    /// OpenAI Responses 的 `previous_response_id` 服务端续链。
    PreviousResponseId,
    /// 供应商侧自动维护（如 DeepSeek 前缀缓存），无需显式字段。
    Implicit,
}

/// thinking 预算的请求字段形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingTokenBudgetField {
    /// 不支持独立预算字段。
    #[default]
    None,
    /// Anthropic `thinking.budget_tokens` 嵌套对象。
    NestedBudgetTokens,
    /// OpenAI 系 `reasoning_effort` 离散档（无连续 token 预算）。
    ReasoningEffort,
}

/// 一层 compat 声明。字段全部 `Option`：`None` = 未声明（继承下一层），
/// `Some` = 显式覆盖。默认全空。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderCompat {
    /// 是否接受 `developer` role（OpenAI o 系）；false 时降级为 `system`。
    pub supports_developer_role: Option<bool>,
    /// 是否接受 reasoning_effort 参数。
    pub supports_reasoning_effort: Option<bool>,
    /// 是否支持长保留 prompt cache（如 Anthropic 1h TTL）。
    pub supports_long_cache_retention: Option<bool>,
    /// 是否需要显式 prompt cache 标记（Anthropic `cache_control`）。
    pub supports_explicit_prompt_cache_mode: Option<bool>,
    /// 是否存在 prompt caching 能力（DeepSeek 自动前缀缓存为厂商事实）。
    pub supports_prompt_caching: Option<bool>,
    pub session_affinity_format: Option<SessionAffinityFormat>,
    pub thinking_token_budget_field: Option<ThinkingTokenBudgetField>,
}

/// 字段级合并：`override_layer` 的 `Some` 覆盖 `base`，`None` 保留 `base`。
///
/// 这是 provider 级与 model 级合成的唯一实现（model 覆盖 provider）；
/// 再往下的硬编码默认也走同一函数，保证三层合成语义一致。
pub fn merge(base: &ProviderCompat, override_layer: &ProviderCompat) -> ProviderCompat {
    ProviderCompat {
        supports_developer_role: override_layer
            .supports_developer_role
            .or(base.supports_developer_role),
        supports_reasoning_effort: override_layer
            .supports_reasoning_effort
            .or(base.supports_reasoning_effort),
        supports_long_cache_retention: override_layer
            .supports_long_cache_retention
            .or(base.supports_long_cache_retention),
        supports_explicit_prompt_cache_mode: override_layer
            .supports_explicit_prompt_cache_mode
            .or(base.supports_explicit_prompt_cache_mode),
        supports_prompt_caching: override_layer
            .supports_prompt_caching
            .or(base.supports_prompt_caching),
        session_affinity_format: override_layer
            .session_affinity_format
            .or(base.session_affinity_format),
        thinking_token_budget_field: override_layer
            .thinking_token_budget_field
            .or(base.thinking_token_budget_field),
    }
}

/// 是否为厂商直连身份：硬编码默认受保护，用户 compat 只能补空白不能翻转。
pub fn is_vendor_direct_kind(provider_kind: &str) -> bool {
    matches!(
        provider_kind.trim().to_ascii_lowercase().as_str(),
        "deepseek" | "kimi_coding" | "ark_coding" | "ark_agent" | "ark_coding_openai"
    )
}

/// 保护式合成：厂商直连身份的已声明（`Some`）字段对用户层不可翻转；
/// 未声明（`None`）字段允许用户补齐。非厂商直连身份退化为普通 [`merge`]。
pub fn protected_merge(
    provider_kind: &str,
    builtin: &ProviderCompat,
    user: &ProviderCompat,
) -> ProviderCompat {
    let merged = merge(builtin, user);
    if !is_vendor_direct_kind(provider_kind) {
        return merged;
    }
    ProviderCompat {
        supports_developer_role: builtin
            .supports_developer_role
            .or(merged.supports_developer_role),
        supports_reasoning_effort: builtin
            .supports_reasoning_effort
            .or(merged.supports_reasoning_effort),
        supports_long_cache_retention: builtin
            .supports_long_cache_retention
            .or(merged.supports_long_cache_retention),
        supports_explicit_prompt_cache_mode: builtin
            .supports_explicit_prompt_cache_mode
            .or(merged.supports_explicit_prompt_cache_mode),
        supports_prompt_caching: builtin
            .supports_prompt_caching
            .or(merged.supports_prompt_caching),
        session_affinity_format: builtin
            .session_affinity_format
            .or(merged.session_affinity_format),
        thinking_token_budget_field: builtin
            .thinking_token_budget_field
            .or(merged.thinking_token_budget_field),
    }
}

/// 硬编码默认：目录内已实测冻结的厂商直连线路事实。
///
/// 推导依据 `agent_llm::dialect`（kimi/ark 的 wire 行为）与 DeepSeek
/// provider 的自动前缀缓存实现；未知/自定义 kind 返回全空（通用行为）。
pub fn builtin_compat(provider_kind: &str) -> ProviderCompat {
    let kind = provider_kind.trim().to_ascii_lowercase();
    match kind.as_str() {
        // 自动前缀缓存（无需 cache_control）、reasoning_content 回传、无
        // reasoning_effort——DeepSeekProvider 的已测试行为。
        "deepseek" => ProviderCompat {
            supports_developer_role: Some(false),
            supports_reasoning_effort: Some(false),
            supports_long_cache_retention: Some(false),
            supports_explicit_prompt_cache_mode: Some(false),
            supports_prompt_caching: Some(true),
            session_affinity_format: Some(SessionAffinityFormat::Implicit),
            thinking_token_budget_field: Some(ThinkingTokenBudgetField::None),
        },
        // Kimi：Anthropic 口注入 cache_control；thinking 走嵌套对象；
        // k3 系支持 effort 词表。
        "kimi_coding" => ProviderCompat {
            supports_reasoning_effort: Some(true),
            supports_explicit_prompt_cache_mode: Some(true),
            session_affinity_format: Some(SessionAffinityFormat::None),
            thinking_token_budget_field: Some(ThinkingTokenBudgetField::NestedBudgetTokens),
            ..ProviderCompat::default()
        },
        // Ark Anthropic 口注入 cache_control；OpenAI 口 effort 顶层字段。
        "ark_coding" | "ark_agent" => ProviderCompat {
            supports_explicit_prompt_cache_mode: Some(true),
            thinking_token_budget_field: Some(ThinkingTokenBudgetField::NestedBudgetTokens),
            ..ProviderCompat::default()
        },
        "ark_coding_openai" => ProviderCompat {
            supports_reasoning_effort: Some(true),
            thinking_token_budget_field: Some(ThinkingTokenBudgetField::ReasoningEffort),
            ..ProviderCompat::default()
        },
        _ => ProviderCompat::default(),
    }
}

/// 三层合成：硬编码默认 + 用户 provider 级 + 用户 model 级。
///
/// model 级覆盖 provider 级（同一合并函数，语义唯一）；厂商直连身份的
/// 硬编码声明受 [`protected_merge`] 保护。
pub fn effective_compat(
    provider_kind: &str,
    user_provider_level: &ProviderCompat,
    user_model_level: &ProviderCompat,
) -> ProviderCompat {
    let builtin = builtin_compat(provider_kind);
    let user_merged = merge(user_provider_level, user_model_level);
    protected_merge(provider_kind, &builtin, &user_merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M1-01.A1：结构体与字段集完整（七字段全部可声明、可序列化往返）。
    #[test]
    fn struct_field_set_complete_and_roundtrips() {
        let full = ProviderCompat {
            supports_developer_role: Some(true),
            supports_reasoning_effort: Some(false),
            supports_long_cache_retention: Some(true),
            supports_explicit_prompt_cache_mode: Some(false),
            supports_prompt_caching: Some(true),
            session_affinity_format: Some(SessionAffinityFormat::PreviousResponseId),
            thinking_token_budget_field: Some(ThinkingTokenBudgetField::ReasoningEffort),
        };
        let json = serde_json::to_string(&full).unwrap();
        for key in [
            "supports_developer_role",
            "supports_reasoning_effort",
            "supports_long_cache_retention",
            "supports_explicit_prompt_cache_mode",
            "supports_prompt_caching",
            "session_affinity_format",
            "thinking_token_budget_field",
        ] {
            assert!(json.contains(key), "missing field {key} in serialized form");
        }
        let back: ProviderCompat = serde_json::from_str(&json).unwrap();
        assert_eq!(back, full);
    }

    /// M1-01.A2：provider/model 两级合并——model 覆盖 provider，None 继承。
    #[test]
    fn model_level_overrides_provider_level_and_none_inherits() {
        let provider_level = ProviderCompat {
            supports_reasoning_effort: Some(true),
            supports_prompt_caching: Some(true),
            session_affinity_format: Some(SessionAffinityFormat::Implicit),
            ..ProviderCompat::default()
        };
        let model_level = ProviderCompat {
            supports_reasoning_effort: Some(false),
            thinking_token_budget_field: Some(ThinkingTokenBudgetField::ReasoningEffort),
            ..ProviderCompat::default()
        };
        let merged = merge(&provider_level, &model_level);
        assert_eq!(merged.supports_reasoning_effort, Some(false), "model wins");
        assert_eq!(merged.supports_prompt_caching, Some(true), "None inherits");
        assert_eq!(
            merged.session_affinity_format,
            Some(SessionAffinityFormat::Implicit),
            "untouched fields inherit"
        );
        assert_eq!(
            merged.thinking_token_budget_field,
            Some(ThinkingTokenBudgetField::ReasoningEffort),
            "model-only fields appear"
        );
    }

    /// M1-01.A2（续）：与硬编码默认的三层合成次序稳定。
    #[test]
    fn effective_composition_orders_builtin_provider_model() {
        let builtin = builtin_compat("custom_relay");
        assert_eq!(
            builtin,
            ProviderCompat::default(),
            "unknown kinds stay empty"
        );
        let user = ProviderCompat {
            supports_reasoning_effort: Some(false),
            ..ProviderCompat::default()
        };
        let model = ProviderCompat {
            supports_reasoning_effort: Some(true),
            ..ProviderCompat::default()
        };
        let effective = effective_compat("custom_relay", &user, &model);
        assert_eq!(effective.supports_reasoning_effort, Some(true));
    }

    /// M1-01.A3：DeepSeek 的 prompt caching 是厂商直连事实，用户 compat 不可关。
    #[test]
    fn deepseek_prompt_caching_survives_user_override() {
        let deny = ProviderCompat {
            supports_prompt_caching: Some(false),
            supports_reasoning_effort: Some(true),
            ..ProviderCompat::default()
        };
        let effective = effective_compat("deepseek", &deny, &ProviderCompat::default());
        assert_eq!(
            effective.supports_prompt_caching,
            Some(true),
            "vendor-direct fact must not be user-overridable"
        );
        // 已声明的其余硬编码字段同样保持（reasoning_effort DeepSeek 不支持）。
        assert_eq!(effective.supports_reasoning_effort, Some(false));
    }

    /// 厂商直连保护只挡"翻转"，不挡"补空白"。
    #[test]
    fn vendor_direct_protection_still_fills_gaps() {
        let gap_fill = ProviderCompat {
            supports_developer_role: Some(true),
            ..ProviderCompat::default()
        };
        let effective = effective_compat("kimi_coding", &gap_fill, &ProviderCompat::default());
        // kimi 未声明 developer_role → 用户补齐生效。
        assert_eq!(effective.supports_developer_role, Some(true));
        // 已声明的 effort 支持保持硬编码值。
        assert_eq!(effective.supports_reasoning_effort, Some(true));
    }

    /// 非厂商直连身份（自定义端点）不受保护：允许任意覆盖。
    #[test]
    fn custom_kinds_are_freely_overridable() {
        let deny = ProviderCompat {
            supports_reasoning_effort: Some(false),
            supports_prompt_caching: Some(false),
            ..ProviderCompat::default()
        };
        let effective = effective_compat("my_broken_relay", &deny, &ProviderCompat::default());
        assert_eq!(effective.supports_reasoning_effort, Some(false));
        assert_eq!(effective.supports_prompt_caching, Some(false));
    }
}
