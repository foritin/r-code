//! Provider 配置构建与就绪判定的唯一实现（F-arch-03 破环抽取）。
//!
//! 这些纯函数（不依赖 `CommandState`）原被困在 commands.rs 上帝模块里，
//! 迫使 memory_runtime / plan_entry_commands 反向依赖 commands 形成文件级环。
//! 现集中于此叶子模块：commands、memory_runtime、plan_entry_commands 都只依赖它。

use crate::provider_catalog::{Preset as ProviderPreset, Protocol as ProviderProtocol};

/// 常用服务的保守默认值，直接取自 [`crate::provider_catalog`]。用户仍可在设置页
/// 覆盖模型或地址。
///
/// 这里曾经维护一份只有 4 条的本地表，与目录里的 29 条预设各说各话；目录才是
/// 唯一事实来源，新增服务只改 `provider_catalog::PRESETS`。
pub(crate) fn provider_preset(name: &str) -> Option<&'static ProviderPreset> {
    crate::provider_catalog::find(name)
}

/// 决定一条已保存的配置该用哪个协议。
///
/// 优先级：**用户存下来的 `protocol` 字段 > 目录推断**。协议是计费和能力都不同
/// 的选择（同一个 base_url 常常多协议并存），只能由用户在设置页显式选，不能替他
/// 猜——所以存了就照存的走，哪怕目录声明了别的。
///
/// 没存过（升级前的旧配置）才推断，且**推断结果永不为 Responses**：Responses 与
/// Chat 在同一地址上往往都可用但计费不同，静默切过去等于替用户改了账单。目录声明
/// Responses 的一律降级为 Chat，等用户自己去设置页选。
pub(crate) fn resolve_effective_protocol(
    name: &str,
    pcfg: &agent_config::ProviderConfig,
) -> ProviderProtocol {
    pcfg.protocol
        .as_deref()
        .and_then(ProviderProtocol::parse)
        .unwrap_or_else(|| infer_protocol_never_responses(name, &pcfg.base_url))
}

/// 没存过 protocol 时的推断规则，**唯一实现**。
///
/// 保存路径和运行时路径必须共用它：任何一边多写一份，两份规则迟早漂移，结果就是
/// 设置页显示的协议和实际发出的请求对不上。
///
/// 规则 = `resolve_protocol` 的结果，但 Responses 一律降级为 Chat。Responses 与
/// Chat 常常在同一地址上都可用而计费不同，替用户选等于替他改账单。
pub(crate) fn infer_protocol_never_responses(name: &str, base_url: &str) -> ProviderProtocol {
    match crate::provider_catalog::resolve_protocol(name, base_url.trim()) {
        ProviderProtocol::OpenAiResponses => ProviderProtocol::OpenAiChat,
        other => other,
    }
}

/// 按 [`resolve_effective_protocol`] 的结果构造 Provider 配置。
///
/// 关键点：**分派依据是协议，不是服务名**。目录里 29 条预设有一多半（Kimi、
/// 智谱、MiniMax、火山 `/api/coding`、百炼……）走的是 Anthropic Messages 口，
/// 旧代码「除 anthropic / deepseek 外一律当 OpenAI Chat」会把它们全部发错协议。
pub(crate) fn build_provider_config(
    name: &str,
    pcfg: &agent_config::ProviderConfig,
) -> agent_llm::ProviderConfig {
    use crate::provider_catalog::{resolve_reasoning_replay, Protocol};

    let configured = pcfg.base_url.trim();
    // 地址留空时必须回填目录里的默认值：`ProviderConfig::Anthropic { base_url: None }`
    // 会让 AnthropicProvider 打到 api.anthropic.com，把 Kimi / 智谱这些 Anthropic 口
    // 的请求发到 Anthropic 官方去。对 id 为 `anthropic` 的服务回填是无害的——预设
    // 地址与 AnthropicProvider 的内置默认值本就是同一个。
    let base_url = if configured.is_empty() {
        provider_preset(name).map_or("", |preset| preset.base_url)
    } else {
        configured
    };
    let optional_base_url = (!base_url.is_empty()).then(|| base_url.to_string());
    let deepseek = is_deepseek_provider(pcfg);
    let kimi_coding = is_kimi_coding_provider(pcfg);
    let ark_kind = pcfg
        .provider_kind
        .as_deref()
        .map(str::trim)
        .filter(|kind| matches!(*kind, "ark_coding" | "ark_agent" | "ark_coding_openai"))
        .map(str::to_string);
    match resolve_effective_protocol(name, pcfg) {
        Protocol::AnthropicMessages if deepseek => agent_llm::ProviderConfig::DeepSeekAnthropic {
            api_key: pcfg.api_key.clone(),
            model: pcfg.model.clone(),
            base_url: optional_base_url,
        },
        Protocol::AnthropicMessages if ark_kind.is_some() => {
            agent_llm::ProviderConfig::ArkAnthropic {
                api_key: pcfg.api_key.clone(),
                model: pcfg.model.clone(),
                base_url: optional_base_url,
                kind: ark_kind.expect("checked above"),
            }
        }
        Protocol::AnthropicMessages if kimi_coding => {
            agent_llm::ProviderConfig::KimiCodingAnthropic {
                api_key: pcfg.api_key.clone(),
                model: pcfg.model.clone(),
                base_url: optional_base_url,
            }
        }
        Protocol::AnthropicMessages => agent_llm::ProviderConfig::Anthropic {
            api_key: pcfg.api_key.clone(),
            model: pcfg.model.clone(),
            base_url: optional_base_url,
        },
        Protocol::OpenAiResponses if deepseek => agent_llm::ProviderConfig::DeepSeekResponses {
            api_key: pcfg.api_key.clone(),
            model: pcfg.model.clone(),
            base_url: base_url.to_string(),
        },
        Protocol::OpenAiResponses if ark_kind.is_some() => {
            agent_llm::ProviderConfig::ArkResponses {
                api_key: pcfg.api_key.clone(),
                model: pcfg.model.clone(),
                base_url: base_url.to_string(),
                kind: ark_kind.expect("checked above"),
            }
        }
        Protocol::OpenAiResponses => agent_llm::ProviderConfig::Responses {
            api_key: pcfg.api_key.clone(),
            model: pcfg.model.clone(),
            base_url: base_url.to_string(),
            // 只有目录里明确标了 reasoning_replay 且地址未被改写的服务才打开；
            // 对不支持 `include=reasoning.encrypted_content` 的实现打开会 400。
            reasoning: if resolve_reasoning_replay(name, configured) {
                agent_llm::ReasoningMode::EncryptedReplay
            } else {
                agent_llm::ReasoningMode::Drop
            },
        },
        // DeepSeek 也是 OpenAI Chat，但 DeepSeekProvider 会按模型名报出正确的
        // 上下文窗口（v4 为 1M，其余 64K），压缩策略依赖这个值，故保留特例。
        Protocol::OpenAiChat if deepseek => agent_llm::ProviderConfig::DeepSeek {
            api_key: pcfg.api_key.clone(),
            model: pcfg.model.clone(),
            base_url: optional_base_url,
        },
        Protocol::OpenAiChat if ark_kind.is_some() => agent_llm::ProviderConfig::ArkChat {
            api_key: pcfg.api_key.clone(),
            model: pcfg.model.clone(),
            base_url: base_url.to_string(),
            kind: ark_kind.expect("checked above"),
        },
        Protocol::OpenAiChat if kimi_coding => agent_llm::ProviderConfig::KimiChat {
            api_key: pcfg.api_key.clone(),
            model: pcfg.model.clone(),
            base_url: base_url.to_string(),
        },
        Protocol::OpenAiChat => agent_llm::ProviderConfig::OpenAi {
            api_key: pcfg.api_key.clone(),
            model: pcfg.model.clone(),
            base_url: base_url.to_string(),
        },
    }
}

/// DeepSeek-specific request shaping follows persisted identity, never an editable label or URL.
pub(crate) fn is_deepseek_provider(provider: &agent_config::ProviderConfig) -> bool {
    provider
        .provider_kind
        .as_deref()
        .is_some_and(|kind| kind.eq_ignore_ascii_case("deepseek"))
}

pub(crate) fn is_kimi_coding_provider(provider: &agent_config::ProviderConfig) -> bool {
    provider
        .provider_kind
        .as_deref()
        .is_some_and(|kind| kind.eq_ignore_ascii_case("kimi_coding"))
}

/// 只校验一个 Provider 是否足以发起请求。与 `Config::validate` 不同，它不会
/// 让其它未完成的配置草稿影响当前默认服务。
pub(crate) fn provider_readiness_error(
    name: &str,
    provider: &agent_config::ProviderConfig,
) -> Option<String> {
    if provider.api_key.trim().is_empty() {
        return Some("缺少访问密钥".to_string());
    }
    if provider.model.trim().is_empty() {
        return Some("缺少默认模型".to_string());
    }
    // 地址留空只有在目录能补出默认值时才成立。
    if provider.base_url.trim().is_empty() && !has_default_base_url(name) {
        return Some("缺少接口地址".to_string());
    }
    // 带占位符的预设（Azure 的 ${RESOURCE_NAME}、Bedrock 的 ${AWS_REGION} 等）
    // 必须由用户替换后才能发请求，否则会打到一个字面量域名上。
    if provider.base_url.contains("${") {
        return Some("接口地址中的占位符尚未替换".to_string());
    }
    None
}

/// 地址留空时，目录或 runtime 能否补出一个可用的默认地址。
///
/// `anthropic` / `deepseek` 由 runtime 自带默认值（见 `DeepSeekProvider` 与
/// `AnthropicProvider`）；其余服务只有在目录里有一条不含占位符的 base_url 时才算。
pub(crate) fn has_default_base_url(name: &str) -> bool {
    matches!(name, "anthropic" | "deepseek")
        || provider_preset(name).is_some_and(|preset| !preset.needs_template())
}
