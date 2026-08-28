//! Provider 名称身份判定的唯一实现（F-maint-02 收敛）。
//!
//! 名称别名清单（deepseek 三别名 / kimi_coding / ark 两别名）曾散落在
//! agent-worker 的 governor 内联 matches! 与 `is_deepseek_native_provider`，
//! host 侧另有按 `provider_kind` 配置字段的判定（provider_support）。本模块
//! 只收敛**名称口径**：同一份别名表，名字清洗规则（trim + 小写）一致。
//! 配置字段口径（provider_kind）语义不同（用户显式标注 vs 名称推断），
//! 保持在 host 的 provider_support 单点实现。

/// DeepSeek 家族名称别名：`deepseek` / `deepseek_responses` / `deepseek_anthropic`。
pub fn is_deepseek_provider_name(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "deepseek" | "deepseek_responses" | "deepseek_anthropic"
    )
}

/// Kimi Coding 名称别名：`kimi_coding`。
pub fn is_kimi_coding_provider_name(name: &str) -> bool {
    name.trim().eq_ignore_ascii_case("kimi_coding")
}

/// Ark Coding 名称别名：`ark_coding` / `ark_agent`。
pub fn is_ark_provider_name(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "ark_coding" | "ark_agent"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deepseek_aliases_cover_three_names_with_normalization() {
        for name in [
            "deepseek",
            "DeepSeek",
            " deepseek_responses ",
            "DEEPSEEK_ANTHROPIC",
        ] {
            assert!(is_deepseek_provider_name(name), "{name} should match");
        }
        assert!(!is_deepseek_provider_name("kimi_coding"));
        assert!(!is_deepseek_provider_name("deep"));
    }

    #[test]
    fn kimi_and_ark_aliases_are_exact_after_normalization() {
        assert!(is_kimi_coding_provider_name("KIMI_CODING"));
        assert!(!is_kimi_coding_provider_name("kimi"));
        assert!(is_ark_provider_name("ark_agent"));
        assert!(is_ark_provider_name(" ARK_Coding "));
        assert!(!is_ark_provider_name("ark"));
        assert!(!is_ark_provider_name("ark_coding_openai"));
    }
}
