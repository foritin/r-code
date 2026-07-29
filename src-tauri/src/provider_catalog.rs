//! 模型服务预设目录（provider catalog）。
//!
//! 数据来源：cc-switch（farion1231/cc-switch）的 `claudeProviderPresets.ts` /
//! `codexProviderPresets.ts`，2026-07 快照。只保留厂商官方直连入口，剔除了原表里
//! 全部带联盟返利码（aff / invite / ref）的中转站。
//!
//! 与 cc-switch 的关键差异：cc-switch 只负责往 `~/.claude/settings.json` 之类的
//! 文件里写环境变量，真正发请求的是 Claude Code；R-Code 自己发请求，所以预设里
//! **必须显式带上线路协议**，不能像原来那样靠 provider 名字猜。
//!
//! 已接线（`commands.rs`）：
//! - `provider_preset()` 直接转发到 [`find`]，本地那份 4 条的重复表已删除
//! - `build_provider_config()` 按 [`resolve_protocol`] 分派协议，不再按服务名；
//!   `Responses` 的 reasoning 策略取自 [`resolve_reasoning_replay`]
//! - 地址留空时由 [`find`] 回填 `base_url`，否则 Anthropic 口的服务会打到
//!   `api.anthropic.com`（`ProviderConfig::Anthropic { base_url: None }` 的默认值）
//! - `provider_readiness_error()` 用 [`find`] 判断是否有默认地址，并拦下
//!   [`Preset::needs_template`] 未替换的占位符
//! - `provider_max_output_tokens()` 用 [`preset_for`] 取 `max_output_tokens` 钳制
//! - IPC 命令 `cmd_provider_catalog` 把 [`catalog_dto`] 吐给前端的"新建服务"表单

use serde::Serialize;

// ─────────────────────────────────────────────────────────────────────────────
// 类型
// ─────────────────────────────────────────────────────────────────────────────

/// 线路协议（wire protocol）。决定请求体形状、SSE 事件格式和鉴权头。
///
/// 注意这与"厂商是谁"正交：同一家厂商的不同 base_url 往往是不同协议
/// （火山方舟 `/api/coding` 是 Anthropic 口，`/api/coding/v3` 是 OpenAI 口）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Protocol {
    /// Anthropic Messages：`{base}/v1/messages`，由 `hermes_llm::AnthropicProvider` 处理。
    #[serde(rename = "anthropic_messages")]
    AnthropicMessages,
    /// OpenAI Chat Completions：`{base}/chat/completions`，由 `hermes_llm::OpenAiProvider` 处理。
    #[serde(rename = "openai_chat")]
    OpenAiChat,
    /// OpenAI Responses：`{base}/responses`，由 `hermes_llm::ResponsesProvider` 处理。
    #[serde(rename = "openai_responses")]
    OpenAiResponses,
}

impl Protocol {
    /// 存进 `config.toml` 的稳定字面量。与各 variant 的 `serde(rename)`
    /// 下发给前端的值一致，前后端和配置文件用同一套 slug。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AnthropicMessages => "anthropic_messages",
            Self::OpenAiChat => "openai_chat",
            Self::OpenAiResponses => "openai_responses",
        }
    }

    /// 解析已保存的配置值。无法识别时返回 `None`，由调用方回退。
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "anthropic_messages" => Some(Self::AnthropicMessages),
            "openai_chat" => Some(Self::OpenAiChat),
            "openai_responses" => Some(Self::OpenAiResponses),
            _ => None,
        }
    }
}

/// 鉴权头风格。国内 Anthropic 兼容网关基本两种都收，但官方 API 只认一种。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthStyle {
    /// `x-api-key: <key>`（Anthropic 官方；`AnthropicProvider` 当前的写法）
    XApiKey,
    /// `Authorization: Bearer <key>`（OpenAI 系 + 绝大多数国内网关）
    Bearer,
}

/// 预设分类，用于设置页分组。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// 模型厂商官方（海外）
    Official,
    /// 模型厂商官方（国内）
    CnOfficial,
    /// 云厂商托管（Azure / Bedrock）
    CloudProvider,
    /// 路由/聚合平台
    Aggregator,
}

/// base_url 里的占位符，需要用户在表单里填。写法 `${NAME}`。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct TemplateVar {
    pub name: &'static str,
    pub label: &'static str,
    pub placeholder: &'static str,
}

/// 一条备用线路。
///
/// **协议必须跟着地址走。** 这些候选里有相当一部分是同一家厂商的*另一个协议口*
/// （火山 `/api/coding` 是 Anthropic、`/api/coding/v3` 是 OpenAI；MiniMax、小米、
/// 美团、OpenRouter 都是这个套路），只记 URL 会让"切一下线路"变成静默换协议。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Endpoint {
    pub url: &'static str,
    /// 该入口默认走的协议。
    pub protocol: Protocol,
    /// 该入口实际支持的全部协议，`protocol` 必须是其中之一。
    ///
    /// 与 [`Preset::native`] 同样是**按地址**声明：美团 LongCat 与小米 MiMo 的
    /// OpenAI 口官方声明支持 Responses，而它们的主入口（`/anthropic`）不支持。
    pub native: &'static [Protocol],
    /// 给用户看的一句话说明，例如"国际站"、"OpenAI 兼容口"。
    pub label: &'static str,
}

/// 一条预设。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Preset {
    /// 稳定标识，同时作为 `config.providers` 这张 map 的 key。只用 `[a-z0-9_]`。
    pub id: &'static str,
    /// 设置页展示名。
    pub label: &'static str,
    /// 该预设默认走的协议。同一个 base_url 支持多协议时，这里选最稳的那个。
    pub protocol: Protocol,
    /// **这个 base_url** 实际支持的全部协议，`protocol` 必须是其中之一。
    ///
    /// 注意是按地址、不是按厂商声明。`/anthropic` 这样的口只说 Anthropic
    /// Messages，即便同一家厂商另有 OpenAI 兼容口——那属于另一条 `Endpoint`
    /// 或另一条预设。写宽了会让设置页放行必然 404 的组合（比如往
    /// `https://api.z.ai/api/anthropic/chat/completions` 发请求）。
    pub native: &'static [Protocol],
    pub auth: AuthStyle,
    /// 接口根地址。拼接规则见 `hermes_llm::url`：
    /// OpenAI 系只对裸域名补 `/v1`（`/api/coding/paas/v4` 这类保持原样），
    /// Anthropic 系一律补 `/v1/messages`（`/api/coding` → `/api/coding/v1/messages`）。
    pub base_url: &'static str,
    /// 是否索取并回传加密的 reasoning（`include: ["reasoning.encrypted_content"]`）。
    /// 只有 OpenAI 官方与 xAI 支持；对其它 Responses 实现打开会 400 或被忽略。
    /// 仅在 `protocol == OpenAiResponses` 时有意义。
    pub reasoning_replay: bool,
    /// 默认模型。
    pub model: &'static str,
    /// 候选模型。填补 `lib/provider.ts` 里注释提到的"后端没有可用模型列表"这一层。
    pub models: &'static [&'static str],
    pub category: Category,
    pub website_url: &'static str,
    /// 领 key 的页面，和官网不是一个地址时才填。
    pub api_key_url: Option<&'static str>,
    /// 备用线路，用于地址切换/测速。每条自带协议，切线路时协议要跟着切。
    pub endpoint_candidates: &'static [Endpoint],
    /// base_url 里的占位符。
    pub template_vars: &'static [TemplateVar],
    /// 单次输出上限（不是上下文窗口）。填了会在保存时钳制 `max_tokens`。
    pub max_output_tokens: Option<u32>,
    /// 上下文窗口，仅供 UI 展示与压缩策略参考。
    pub context_window: Option<u32>,
    /// 给用户看的注意事项，主要是计费陷阱和已知不兼容。
    pub note: Option<&'static str>,
}

impl Preset {
    /// 该预设是否需要用户先替换 base_url 里的占位符。
    pub fn needs_template(&self) -> bool {
        !self.template_vars.is_empty()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 目录
// ─────────────────────────────────────────────────────────────────────────────

// `native` 是按**地址**声明的，所以不存在既说 Anthropic 又说 OpenAI 的组合：
// 两者请求体、路径和事件格式全然不同，一个 URL 只会是其中一种口。厂商同时提供
// 两种口时，那是两条 `Endpoint`（或两条预设），不是一条 native 里塞两个。
const P_A: &[Protocol] = &[Protocol::AnthropicMessages];
const P_C: &[Protocol] = &[Protocol::OpenAiChat];
const P_CR: &[Protocol] = &[Protocol::OpenAiChat, Protocol::OpenAiResponses];

/// 全部预设。顺序即设置页展示顺序。
pub const PRESETS: &[Preset] = &[
    // ── 海外官方 ────────────────────────────────────────────────────────────
    Preset {
        id: "anthropic",
        label: "Anthropic",
        protocol: Protocol::AnthropicMessages,
        native: P_A,
        auth: AuthStyle::XApiKey,
        base_url: "https://api.anthropic.com",
        reasoning_replay: false,
        model: "claude-sonnet-5",
        models: &[
            "claude-sonnet-5",
            "claude-opus-4-8",
            "claude-haiku-4-5-20251001",
        ],
        category: Category::Official,
        website_url: "https://www.anthropic.com/claude-code",
        api_key_url: Some("https://console.anthropic.com/settings/keys"),
        endpoint_candidates: &[],
        template_vars: &[],
        max_output_tokens: None,
        context_window: Some(200_000),
        note: None,
    },
    Preset {
        id: "openai",
        label: "OpenAI",
        // 官方推荐的新集成路径，且 codex 系列模型只在 Responses 上开放
        protocol: Protocol::OpenAiResponses,
        native: P_CR,
        auth: AuthStyle::Bearer,
        base_url: "https://api.openai.com/v1",
        reasoning_replay: true,
        model: "gpt-5.6-sol",
        models: &[
            "gpt-5.6-sol",
            "gpt-5.6-terra",
            "gpt-5.6-luna",
            "gpt-5.3-codex",
            "gpt-5.5",
        ],
        category: Category::Official,
        website_url: "https://openai.com/api",
        api_key_url: Some("https://platform.openai.com/api-keys"),
        endpoint_candidates: &[],
        template_vars: &[],
        max_output_tokens: None,
        context_window: Some(1_050_000),
        note: Some("走 Responses；Chat Completions 未弃用但调不到 codex 系列"),
    },
    Preset {
        id: "xai",
        label: "xAI (Grok)",
        // xAI 把 /v1/responses 作为一等端点，Chat Completions 才是兼容层
        protocol: Protocol::OpenAiResponses,
        native: P_CR,
        auth: AuthStyle::Bearer,
        base_url: "https://api.x.ai/v1",
        reasoning_replay: true,
        model: "grok-4.5",
        models: &["grok-4.5"],
        category: Category::Official,
        website_url: "https://x.ai/api",
        api_key_url: Some("https://console.x.ai"),
        endpoint_candidates: &[],
        template_vars: &[],
        max_output_tokens: None,
        context_window: Some(500_000),
        note: Some("支持 store:false + reasoning.encrypted_content，可无状态回传思维链"),
    },
    // ── 云厂商托管 ──────────────────────────────────────────────────────────
    Preset {
        id: "azure_openai",
        label: "Azure OpenAI",
        protocol: Protocol::OpenAiChat,
        native: P_CR,
        auth: AuthStyle::Bearer,
        // v1 GA 路径：不再有 /deployments/{name}/，deployment 名放请求体的 model
        base_url: "https://${RESOURCE_NAME}.openai.azure.com/openai/v1",
        reasoning_replay: false,
        model: "gpt-5.5",
        models: &["gpt-5.5"],
        category: Category::CloudProvider,
        website_url: "https://learn.microsoft.com/azure/foundry/openai/",
        api_key_url: None,
        endpoint_candidates: &[],
        template_vars: &[TemplateVar {
            name: "RESOURCE_NAME",
            label: "Azure 资源名",
            placeholder: "my-openai-resource",
        }],
        max_output_tokens: None,
        context_window: None,
        note: Some(
            "v1 GA 路径下 api-version 已是可选参数。⚠️ Azure 用 api-key 头或 Entra token，\
             我们目前只发 Authorization: Bearer，纯 API Key 会 401——需要 Entra token 才能用",
        ),
    },
    Preset {
        id: "bedrock",
        label: "AWS Bedrock",
        protocol: Protocol::AnthropicMessages,
        native: P_A,
        auth: AuthStyle::Bearer,
        base_url: "https://bedrock-runtime.${AWS_REGION}.amazonaws.com",
        reasoning_replay: false,
        model: "global.anthropic.claude-sonnet-5",
        models: &[
            "global.anthropic.claude-sonnet-5",
            "global.anthropic.claude-opus-4-8",
            "global.anthropic.claude-haiku-4-5-20251001-v1:0",
        ],
        category: Category::CloudProvider,
        website_url: "https://aws.amazon.com/bedrock/",
        api_key_url: None,
        endpoint_candidates: &[],
        template_vars: &[TemplateVar {
            name: "AWS_REGION",
            label: "AWS Region",
            placeholder: "us-west-2",
        }],
        max_output_tokens: None,
        context_window: Some(200_000),
        note: Some("需要 SigV4 签名；用长期 API Key 模式才能走现有的 Bearer 分支"),
    },
    // ── 国内厂商直连 ────────────────────────────────────────────────────────
    Preset {
        id: "deepseek",
        label: "DeepSeek",
        protocol: Protocol::OpenAiChat,
        native: P_C,
        auth: AuthStyle::Bearer,
        base_url: "https://api.deepseek.com",
        reasoning_replay: false,
        model: "deepseek-v4-flash",
        models: &["deepseek-v4-flash", "deepseek-v4-pro"],
        category: Category::CnOfficial,
        website_url: "https://platform.deepseek.com",
        api_key_url: Some("https://platform.deepseek.com/api_keys"),
        endpoint_candidates: &[],
        template_vars: &[],
        // V4 单次输出上限 384K，填上下文窗口会被服务端 400
        max_output_tokens: Some(393_216),
        context_window: Some(1_000_000),
        note: Some("deepseek-chat / deepseek-reasoner 已于 2026-07-24 下线"),
    },
    Preset {
        id: "deepseek_anthropic",
        label: "DeepSeek（Anthropic 口）",
        protocol: Protocol::AnthropicMessages,
        native: P_A,
        auth: AuthStyle::XApiKey,
        base_url: "https://api.deepseek.com/anthropic",
        reasoning_replay: false,
        model: "deepseek-v4-pro",
        models: &["deepseek-v4-pro", "deepseek-v4-flash"],
        category: Category::CnOfficial,
        website_url: "https://api-docs.deepseek.com/guides/anthropic_api/",
        api_key_url: Some("https://platform.deepseek.com/api_keys"),
        endpoint_candidates: &[],
        template_vars: &[],
        max_output_tokens: Some(393_216),
        context_window: Some(1_000_000),
        note: Some("system 必须走顶层字段，塞进 messages 会 400"),
    },
    Preset {
        id: "kimi",
        label: "Kimi（按量）",
        protocol: Protocol::AnthropicMessages,
        native: P_A,
        auth: AuthStyle::Bearer,
        base_url: "https://api.moonshot.cn/anthropic",
        reasoning_replay: false,
        model: "kimi-k2.7-code",
        models: &["kimi-k2.7-code", "kimi-k3", "kimi-k2.6"],
        category: Category::CnOfficial,
        website_url: "https://platform.kimi.com",
        api_key_url: Some("https://platform.kimi.com/console/api-keys"),
        endpoint_candidates: &[Endpoint {
            url: "https://api.moonshot.ai/anthropic",
            protocol: Protocol::AnthropicMessages,
            native: P_A,
            label: "国际站",
        }],
        template_vars: &[],
        max_output_tokens: None,
        context_window: Some(262_144),
        note: Some("模型名里的方括号后缀是上下文档位语法（如 kimi-k3[1m]），不要剥掉"),
    },
    Preset {
        id: "kimi_coding",
        label: "Kimi For Coding（订阅）",
        protocol: Protocol::AnthropicMessages,
        native: P_A,
        auth: AuthStyle::Bearer,
        base_url: "https://api.kimi.com/coding/",
        reasoning_replay: false,
        model: "kimi-for-coding",
        models: &["kimi-for-coding"],
        category: Category::CnOfficial,
        website_url: "https://www.kimi.com/code/",
        api_key_url: None,
        // OpenAI 口在 https://api.kimi.com/coding/v1
        endpoint_candidates: &[Endpoint {
            url: "https://api.kimi.com/coding/v1",
            protocol: Protocol::OpenAiChat,
            native: P_C,
            label: "OpenAI 兼容口",
        }],
        template_vars: &[],
        max_output_tokens: Some(32_768),
        context_window: Some(262_144),
        note: Some("必须用 kimi-for-coding 这个路由别名，填真实模型名不走订阅"),
    },
    Preset {
        id: "zhipu",
        label: "智谱 GLM（按量）",
        protocol: Protocol::AnthropicMessages,
        native: P_A,
        auth: AuthStyle::Bearer,
        base_url: "https://open.bigmodel.cn/api/anthropic",
        reasoning_replay: false,
        model: "glm-5.2",
        models: &["glm-5.2", "glm-5.1", "glm-4.7"],
        category: Category::CnOfficial,
        website_url: "https://open.bigmodel.cn",
        api_key_url: Some("https://www.bigmodel.cn/usercenter/apikeys"),
        endpoint_candidates: &[],
        template_vars: &[],
        max_output_tokens: None,
        context_window: Some(200_000),
        note: None,
    },
    Preset {
        id: "zhipu_coding",
        label: "智谱 GLM Coding Plan",
        protocol: Protocol::OpenAiChat,
        native: P_C,
        auth: AuthStyle::Bearer,
        // 注意是 /v4 不是 /v1
        base_url: "https://open.bigmodel.cn/api/coding/paas/v4",
        reasoning_replay: false,
        model: "glm-5.2",
        models: &["glm-5.2", "glm-5.1"],
        category: Category::CnOfficial,
        website_url: "https://docs.bigmodel.cn/cn/coding-plan/quick-start",
        api_key_url: None,
        endpoint_candidates: &[],
        template_vars: &[],
        max_output_tokens: None,
        context_window: Some(200_000),
        note: Some("路径以 /v4 结尾，OpenAiProvider 的补 /v1 逻辑会拼错，需按原样透传"),
    },
    Preset {
        id: "zai",
        label: "Z.ai（智谱国际）",
        protocol: Protocol::AnthropicMessages,
        native: P_A,
        auth: AuthStyle::Bearer,
        base_url: "https://api.z.ai/api/anthropic",
        reasoning_replay: false,
        model: "glm-5.2",
        models: &["glm-5.2", "glm-5.1", "glm-4.7"],
        category: Category::CnOfficial,
        website_url: "https://z.ai",
        api_key_url: Some("https://z.ai/manage-apikey/apikey-list"),
        endpoint_candidates: &[Endpoint {
            url: "https://api.z.ai/api/coding/paas/v4",
            protocol: Protocol::OpenAiChat,
            native: P_C,
            label: "OpenAI 兼容口（Coding Plan）",
        }],
        template_vars: &[],
        max_output_tokens: None,
        context_window: Some(200_000),
        note: None,
    },
    Preset {
        id: "ark_coding",
        label: "火山方舟 Coding Plan",
        protocol: Protocol::AnthropicMessages,
        native: P_A,
        auth: AuthStyle::Bearer,
        // 套餐网关：不带 /v3 的是 Anthropic 口，带 /v3 的是 OpenAI 口
        base_url: "https://ark.cn-beijing.volces.com/api/coding",
        reasoning_replay: false,
        model: "ark-code-latest",
        models: &["ark-code-latest"],
        category: Category::CnOfficial,
        website_url: "https://www.volcengine.com/docs/82379/2373740",
        api_key_url: Some("https://console.volcengine.com/ark/region:ark+cn-beijing/apiKey"),
        endpoint_candidates: &[Endpoint {
            url: "https://ark.cn-beijing.volces.com/api/coding/v3",
            protocol: Protocol::OpenAiChat,
            native: P_C,
            label: "OpenAI 兼容口",
        }],
        template_vars: &[],
        max_output_tokens: None,
        context_window: Some(256_000),
        note: Some("必须走 /api/coding 才计入套餐；走 /api/v3 会静默转成按量计费"),
    },
    Preset {
        id: "ark_coding_openai",
        label: "火山方舟 Coding Plan（OpenAI 口）",
        protocol: Protocol::OpenAiChat,
        native: P_C,
        auth: AuthStyle::Bearer,
        base_url: "https://ark.cn-beijing.volces.com/api/coding/v3",
        reasoning_replay: false,
        model: "ark-code-latest",
        models: &["ark-code-latest"],
        category: Category::CnOfficial,
        website_url: "https://www.volcengine.com/docs/82379/2556056",
        api_key_url: Some("https://console.volcengine.com/ark/region:ark+cn-beijing/apiKey"),
        endpoint_candidates: &[],
        template_vars: &[],
        max_output_tokens: None,
        context_window: Some(256_000),
        note: Some("套餐网关未见 /responses，只有 Chat Completions"),
    },
    Preset {
        id: "ark",
        label: "火山方舟（按量 API）",
        protocol: Protocol::OpenAiChat,
        native: P_CR,
        auth: AuthStyle::Bearer,
        base_url: "https://ark.cn-beijing.volces.com/api/v3",
        reasoning_replay: false,
        model: "doubao-seed-2-1-pro-260628",
        models: &[
            "doubao-seed-2-1-pro-260628",
            "doubao-seed-1-8-251228",
            "doubao-seed-code-preview-latest",
        ],
        category: Category::CnOfficial,
        website_url: "https://www.volcengine.com/docs/82379/1298459",
        api_key_url: Some("https://console.volcengine.com/ark/region:ark+cn-beijing/apiKey"),
        endpoint_candidates: &[Endpoint {
            url: "https://ark.cn-shanghai.volces.com/api/v3",
            protocol: Protocol::OpenAiChat,
            native: P_CR,
            label: "上海区域",
        }],
        template_vars: &[],
        max_output_tokens: None,
        context_window: Some(262_144),
        note: Some(
            "按量计费，不走套餐。同址有 /responses，但 Ark 的 Responses 不支持 include \
             与 reasoning.encrypted_content，只能靠服务端会话（store + previous_response_id）保持思维链",
        ),
    },
    Preset {
        id: "byteplus",
        label: "BytePlus ModelArk",
        protocol: Protocol::AnthropicMessages,
        native: P_A,
        auth: AuthStyle::Bearer,
        base_url: "https://ark.ap-southeast.bytepluses.com/api/coding",
        reasoning_replay: false,
        model: "ark-code-latest",
        models: &["ark-code-latest"],
        category: Category::CnOfficial,
        website_url: "https://www.byteplus.com/en/product/modelark",
        api_key_url: None,
        endpoint_candidates: &[Endpoint {
            url: "https://ark.ap-southeast.bytepluses.com/api/coding/v3",
            protocol: Protocol::OpenAiChat,
            native: P_C,
            label: "OpenAI 兼容口",
        }],
        template_vars: &[],
        max_output_tokens: None,
        context_window: Some(256_000),
        note: None,
    },
    Preset {
        id: "bailian",
        label: "阿里百炼（按量）",
        protocol: Protocol::AnthropicMessages,
        native: P_A,
        auth: AuthStyle::Bearer,
        base_url: "https://dashscope.aliyuncs.com/apps/anthropic",
        reasoning_replay: false,
        model: "qwen3-coder-plus",
        models: &["qwen3-coder-plus", "qwen3-coder-next", "qwen3.7-max"],
        category: Category::CnOfficial,
        website_url: "https://bailian.console.aliyun.com",
        api_key_url: Some("https://bailian.console.aliyun.com/#/api-key"),
        endpoint_candidates: &[],
        template_vars: &[],
        max_output_tokens: None,
        context_window: Some(1_048_576),
        note: Some("百炼 Anthropic 口还转发 glm / deepseek 等第三方模型"),
    },
    Preset {
        id: "bailian_coding",
        label: "阿里百炼 Coding Plan",
        protocol: Protocol::AnthropicMessages,
        native: P_A,
        auth: AuthStyle::Bearer,
        base_url: "https://coding.dashscope.aliyuncs.com/apps/anthropic",
        reasoning_replay: false,
        model: "qwen3-coder-plus",
        models: &["qwen3-coder-plus", "qwen3-coder-next"],
        category: Category::CnOfficial,
        website_url: "https://help.aliyun.com/zh/model-studio/claude-code",
        api_key_url: Some("https://bailian.console.aliyun.com/#/api-key"),
        endpoint_candidates: &[],
        template_vars: &[],
        max_output_tokens: None,
        context_window: Some(1_048_576),
        note: Some("套餐走 coding. 子域名，按量走 dashscope. 主域名，配错不计入套餐"),
    },
    Preset {
        id: "dashscope",
        label: "阿里百炼（OpenAI 兼容）",
        protocol: Protocol::OpenAiChat,
        native: P_CR,
        auth: AuthStyle::Bearer,
        base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        reasoning_replay: false,
        model: "qwen3-coder-plus",
        models: &["qwen3-coder-plus", "qwen3-coder-next", "qwen3.7-max"],
        category: Category::CnOfficial,
        website_url: "https://help.aliyun.com/zh/model-studio/compatibility-with-openai-responses-api",
        api_key_url: Some("https://bailian.console.aliyun.com/#/api-key"),
        endpoint_candidates: &[],
        template_vars: &[],
        max_output_tokens: None,
        context_window: Some(1_048_576),
        note: Some("同一 base_url 下原生支持 /responses，SSE 事件名与 OpenAI 一致；previous_response_id 有效期 7 天"),
    },
    Preset {
        id: "minimax",
        label: "MiniMax",
        protocol: Protocol::AnthropicMessages,
        native: P_A,
        auth: AuthStyle::Bearer,
        base_url: "https://api.minimaxi.com/anthropic",
        reasoning_replay: false,
        model: "MiniMax-M2.7",
        models: &["MiniMax-M2.7", "MiniMax-M3"],
        category: Category::CnOfficial,
        website_url: "https://platform.minimaxi.com",
        api_key_url: Some("https://platform.minimaxi.com/subscribe/coding-plan"),
        endpoint_candidates: &[Endpoint {
            url: "https://api.minimaxi.com/v1",
            protocol: Protocol::OpenAiChat,
            native: P_C,
            label: "OpenAI 兼容口",
        }],
        template_vars: &[],
        max_output_tokens: None,
        context_window: Some(1_000_000),
        note: Some("忽略 top_k / stop_sequences；1M 档位模型名写作 MiniMax-M3[1m]"),
    },
    Preset {
        id: "minimax_intl",
        label: "MiniMax（国际）",
        protocol: Protocol::AnthropicMessages,
        native: P_A,
        auth: AuthStyle::Bearer,
        base_url: "https://api.minimax.io/anthropic",
        reasoning_replay: false,
        model: "MiniMax-M2.7",
        models: &["MiniMax-M2.7", "MiniMax-M3"],
        category: Category::CnOfficial,
        website_url: "https://platform.minimax.io",
        api_key_url: Some("https://platform.minimax.io/subscribe/coding-plan"),
        endpoint_candidates: &[Endpoint {
            url: "https://api.minimax.io/v1",
            protocol: Protocol::OpenAiChat,
            native: P_C,
            label: "OpenAI 兼容口",
        }],
        template_vars: &[],
        max_output_tokens: None,
        context_window: Some(1_000_000),
        note: None,
    },
    Preset {
        id: "qianfan_coding",
        label: "百度千帆 Coding Plan",
        protocol: Protocol::AnthropicMessages,
        native: P_A,
        auth: AuthStyle::Bearer,
        base_url: "https://qianfan.baidubce.com/anthropic/coding",
        reasoning_replay: false,
        model: "qianfan-code-latest",
        models: &["qianfan-code-latest"],
        category: Category::CnOfficial,
        website_url: "https://cloud.baidu.com/product/qianfan_modelbuilder",
        api_key_url: Some(
            "https://console.bce.baidu.com/qianfan/ais/console/applicationConsole/application",
        ),
        endpoint_candidates: &[Endpoint {
            url: "https://qianfan.baidubce.com/v2/coding",
            protocol: Protocol::OpenAiChat,
            native: P_C,
            label: "OpenAI 兼容口",
        }],
        template_vars: &[],
        max_output_tokens: None,
        context_window: Some(131_072),
        note: None,
    },
    Preset {
        id: "stepfun",
        label: "阶跃星辰 Step Plan",
        protocol: Protocol::AnthropicMessages,
        native: P_A,
        auth: AuthStyle::Bearer,
        base_url: "https://api.stepfun.com/step_plan",
        reasoning_replay: false,
        model: "step-3.7-flash",
        models: &["step-3.7-flash", "step-3.5-flash-2603", "step-3.5-flash"],
        category: Category::CnOfficial,
        website_url: "https://platform.stepfun.com/step-plan",
        api_key_url: Some("https://platform.stepfun.com/interface-key"),
        endpoint_candidates: &[
            Endpoint {
                url: "https://api.stepfun.com/step_plan/v1",
                protocol: Protocol::OpenAiChat,
                native: P_C,
                label: "OpenAI 兼容口",
            },
            Endpoint {
                url: "https://api.stepfun.ai/step_plan",
                protocol: Protocol::AnthropicMessages,
                native: P_A,
                label: "国际站",
            },
        ],
        template_vars: &[],
        max_output_tokens: None,
        context_window: Some(262_144),
        note: None,
    },
    Preset {
        id: "longcat",
        label: "美团 LongCat",
        protocol: Protocol::AnthropicMessages,
        native: P_A,
        auth: AuthStyle::Bearer,
        base_url: "https://api.longcat.chat/anthropic",
        reasoning_replay: false,
        model: "LongCat-2.0",
        models: &["LongCat-2.0"],
        category: Category::CnOfficial,
        website_url: "https://longcat.chat/platform",
        api_key_url: Some("https://longcat.chat/platform/api_keys"),
        endpoint_candidates: &[Endpoint {
            url: "https://api.longcat.chat/openai/v1",
            protocol: Protocol::OpenAiChat,
            native: P_CR,
            label: "OpenAI 兼容口",
        }],
        template_vars: &[],
        max_output_tokens: Some(131_072),
        context_window: Some(1_048_576),
        note: Some("OpenAI 口（/openai/v1）原生支持 Responses"),
    },
    Preset {
        id: "xiaomi_mimo",
        label: "小米 MiMo（按量）",
        protocol: Protocol::AnthropicMessages,
        native: P_A,
        auth: AuthStyle::Bearer,
        base_url: "https://api.xiaomimimo.com/anthropic",
        reasoning_replay: false,
        model: "mimo-v2.5-pro",
        models: &["mimo-v2.5-pro", "mimo-v2.5"],
        category: Category::CnOfficial,
        website_url: "https://platform.xiaomimimo.com",
        api_key_url: Some("https://platform.xiaomimimo.com/#/console/api-keys"),
        endpoint_candidates: &[Endpoint {
            url: "https://api.xiaomimimo.com/v1",
            protocol: Protocol::OpenAiChat,
            native: P_CR,
            label: "OpenAI 兼容口",
        }],
        template_vars: &[],
        max_output_tokens: None,
        context_window: Some(1_048_576),
        note: Some("OpenAI 口官方声明原生支持 Responses"),
    },
    Preset {
        id: "xiaomi_mimo_plan",
        label: "小米 MiMo Token Plan",
        protocol: Protocol::AnthropicMessages,
        native: P_A,
        auth: AuthStyle::Bearer,
        base_url: "https://token-plan-cn.xiaomimimo.com/anthropic",
        reasoning_replay: false,
        model: "mimo-v2.5-pro",
        models: &["mimo-v2.5-pro", "mimo-v2.5"],
        category: Category::CnOfficial,
        website_url: "https://platform.xiaomimimo.com/#/token-plan",
        api_key_url: Some("https://platform.xiaomimimo.com/#/console/plan-manage"),
        endpoint_candidates: &[Endpoint {
            url: "https://token-plan-cn.xiaomimimo.com/v1",
            protocol: Protocol::OpenAiChat,
            native: P_C,
            label: "OpenAI 兼容口",
        }],
        template_vars: &[],
        max_output_tokens: None,
        context_window: Some(1_048_576),
        note: None,
    },
    Preset {
        id: "bailing",
        label: "蚂蚁百灵 Ling",
        protocol: Protocol::AnthropicMessages,
        native: P_A,
        auth: AuthStyle::Bearer,
        base_url: "https://api.tbox.cn/api/anthropic",
        reasoning_replay: false,
        model: "Ling-2.6-1T",
        models: &["Ling-2.6-1T", "Ling-2.5-1T"],
        category: Category::CnOfficial,
        website_url: "https://ling.tbox.cn/open",
        api_key_url: Some("https://ling.tbox.cn/open"),
        endpoint_candidates: &[Endpoint {
            url: "https://api.tbox.cn/api/llm/v1",
            protocol: Protocol::OpenAiChat,
            native: P_C,
            label: "OpenAI 兼容口",
        }],
        template_vars: &[],
        max_output_tokens: None,
        context_window: Some(262_144),
        note: None,
    },
    Preset {
        id: "kat_coder",
        label: "KAT-Coder（StreamLake）",
        protocol: Protocol::AnthropicMessages,
        native: P_A,
        auth: AuthStyle::Bearer,
        base_url: "https://vanchin.streamlake.ai/api/gateway/v1/endpoints/${ENDPOINT_ID}/claude-code-proxy",
        reasoning_replay: false,
        model: "KAT-Coder-Pro V1",
        models: &["KAT-Coder-Pro V1", "KAT-Coder-Air V1"],
        category: Category::CnOfficial,
        website_url: "https://console.streamlake.ai",
        api_key_url: Some("https://console.streamlake.ai/console/api-key"),
        endpoint_candidates: &[],
        template_vars: &[TemplateVar {
            name: "ENDPOINT_ID",
            label: "Vanchin Endpoint ID",
            placeholder: "ep-xxx-xxx",
        }],
        max_output_tokens: None,
        context_window: None,
        note: None,
    },
    // ── 聚合/路由 ───────────────────────────────────────────────────────────
    Preset {
        id: "openrouter",
        label: "OpenRouter",
        protocol: Protocol::AnthropicMessages,
        native: P_A,
        auth: AuthStyle::Bearer,
        base_url: "https://openrouter.ai/api",
        reasoning_replay: false,
        model: "anthropic/claude-sonnet-5",
        models: &[
            "anthropic/claude-sonnet-5",
            "anthropic/claude-opus-4.8",
            "anthropic/claude-haiku-4.5",
        ],
        category: Category::Aggregator,
        website_url: "https://openrouter.ai",
        api_key_url: Some("https://openrouter.ai/keys"),
        endpoint_candidates: &[Endpoint {
            url: "https://openrouter.ai/api/v1",
            protocol: Protocol::OpenAiChat,
            native: P_C,
            label: "OpenAI 兼容口",
        }],
        template_vars: &[],
        max_output_tokens: None,
        context_window: None,
        note: Some("模型名带厂商前缀，不要做归一化"),
    },
];

// ─────────────────────────────────────────────────────────────────────────────
// 查询
// ─────────────────────────────────────────────────────────────────────────────

/// 按 id 精确查找。
pub fn find(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|preset| preset.id == id)
}

fn normalize_url(url: &str) -> String {
    url.trim().trim_end_matches('/').to_ascii_lowercase()
}

/// 该地址在目录里对应的**具体入口**：命中的预设，以及那个入口实际说的协议。
///
/// 分三种命中方式：
/// 1. 地址留空 —— 保存时会回填 `preset.base_url`，按主入口算
/// 2. 地址等于 `preset.base_url` —— 主入口
/// 3. 地址等于某条 `endpoint_candidates` —— **按那条候选自己的协议算，不是预设的**
///
/// 第 3 条是关键：目录里 16 条备用线路有 13 条与主入口不同协议（火山
/// `/api/coding` 是 Anthropic、`/api/coding/v3` 是 OpenAI，MiniMax / 小米 / 美团 /
/// OpenRouter 同理）。按预设协议算等于用户点一下"备用线路"就被静默换了协议。
///
/// 地址被改写成目录以外的值时返回 `None`——第三方网关实现了什么无从知道。
fn matched_endpoint(id: &str, base_url: &str) -> Option<(&'static Preset, Protocol)> {
    let preset = find(id)?;
    let url = normalize_url(base_url);
    if url.is_empty() || url == normalize_url(preset.base_url) {
        return Some((preset, preset.protocol));
    }
    preset
        .endpoint_candidates
        .iter()
        .find(|candidate| normalize_url(candidate.url) == url)
        .map(|candidate| (preset, candidate.protocol))
}

/// 命中目录，**且用户没有把地址改掉**时返回该预设。
///
/// 地址一旦被改写（把 `openai` 指向自建网关是很常见的用法），预设声明的一切
/// 就不再可信——第三方网关多半只有 Chat Completions，输出上限也无从保证。
///
/// 对外公开是给 `commands.rs` 判断「这条已保存的配置还能不能沿用预设声明的
/// 输出上限」用的。注意它只回答"是哪条预设"，**协议要用 [`resolve_protocol`]**，
/// 因为同一条预设下不同入口的协议可以不同。
pub fn preset_for(id: &str, base_url: &str) -> Option<&'static Preset> {
    matched_endpoint(id, base_url).map(|(preset, _)| preset)
}

/// 该地址允许用户选哪些协议。`None` = 目录管不到，不设限。
///
/// 主入口给 `preset.native`、备用线路给 `candidate.native`——两者都是**按地址**的
/// 声明。不能一律用 `preset.native`：`ark_coding` 主入口只说 Anthropic，而它的
/// `/api/coding/v3` 候选是 OpenAI 口；反过来 `longcat` 的 `/openai/v1` 候选支持
/// Responses，主入口不支持。
pub fn allowed_protocols(id: &str, base_url: &str) -> Option<Vec<Protocol>> {
    let (_, native) = matched_native(id, base_url)?;
    Some(native.to_vec())
}

/// 命中的入口及其 `native` 列表。与 [`matched_endpoint`] 共用同一套匹配规则，
/// 抄两份迟早漂移。
fn matched_native(id: &str, base_url: &str) -> Option<(&'static Preset, &'static [Protocol])> {
    let preset = find(id)?;
    let url = normalize_url(base_url);
    if url.is_empty() || url == normalize_url(preset.base_url) {
        return Some((preset, preset.native));
    }
    preset
        .endpoint_candidates
        .iter()
        .find(|candidate| normalize_url(candidate.url) == url)
        .map(|candidate| (preset, candidate.native))
}

/// 判断某个已保存的 provider 该用哪个协议。
///
/// 优先查目录（地址仍是目录里某个入口时）；否则按 base_url 猜，最后退回历史行为
/// （未知 = OpenAI 兼容）。
///
/// **这是替换 `commands.rs` 里 `match provider_name.as_str()` 的那段分派逻辑用的。**
/// 原来的写法把除 anthropic / deepseek 外的一切都当成 OpenAI Chat，
/// 目录一旦引入 Kimi / 智谱 / 火山这些 Anthropic 口的预设就会全部发错协议。
pub fn resolve_protocol(id: &str, base_url: &str) -> Protocol {
    if let Some((_, protocol)) = matched_endpoint(id, base_url) {
        return protocol;
    }
    let url = normalize_url(base_url);
    if url.ends_with("/anthropic")
        || url.contains("/anthropic/")
        || url.contains("api.anthropic.com")
        || url.contains("/api/coding")
        || url.contains("/apps/anthropic")
        || url.contains("claude-code-proxy")
    {
        return Protocol::AnthropicMessages;
    }
    Protocol::OpenAiChat
}

/// 该 provider 是否应索取并回传加密 reasoning。
///
/// 只有 OpenAI 官方与 xAI 支持 `include=reasoning.encrypted_content`，所以地址
/// 被改写过、或目录里查不到的服务一律关闭——对不支持的实现打开会 400。
/// 命中的入口本身不是 Responses 时同样关闭。
pub fn resolve_reasoning_replay(id: &str, base_url: &str) -> bool {
    matched_endpoint(id, base_url).is_some_and(|(preset, protocol)| {
        preset.reasoning_replay && protocol == Protocol::OpenAiResponses
    })
}

/// 供 IPC 下发给前端的目录快照。
#[derive(Debug, Serialize)]
pub struct CatalogDto {
    pub presets: &'static [Preset],
}

/// 由 `commands.rs::provider_catalog()` 经 IPC 命令 `cmd_provider_catalog` 下发，
/// 前端设置页的"新建服务"据此列出预设。
pub fn catalog_dto() -> CatalogDto {
    CatalogDto { presets: PRESETS }
}

// ─────────────────────────────────────────────────────────────────────────────
// 接线说明
// ─────────────────────────────────────────────────────────────────────────────

/// 维护须知（不参与编译产物）。
///
/// **新增一条预设时**
/// 1. `id` 必须是新的 slug，它同时是 `config.providers` 的 key，改名等于换服务。
/// 2. `protocol` 决定发什么协议，**不要指望从名字或 base_url 推导**。同一家厂商
///    的套餐口和按量口经常是不同协议、不同计费，一律拆成两条独立预设。
/// 3. `reasoning_replay` 只能对确认支持 `include=reasoning.encrypted_content`
///    的服务打开（目前只有 OpenAI 官方与 xAI），单测会拦。
/// 4. `base_url` 按 `hermes_llm::url` 的规则写：OpenAI 系必须自带版本段，
///    Anthropic 系不带版本段。
///
/// **协议判定的优先级**
/// [`resolve_protocol`] 先查目录，查不到才按 base_url 猜。用户手填的自定义中转站
/// 走猜测分支：路径里有 `/anthropic`、`/api/coding`、`/apps/anthropic` 之类的
/// 视为 Anthropic 口，其余按 OpenAI Chat。猜错的补救办法是让用户把服务 id 改成
/// 目录里已有的那条，或者往目录里加一条。
///
/// **已知未覆盖**
/// - Azure 的 `api-key` 头：我们只发 `Authorization: Bearer`，纯 API Key 会 401。
/// - AWS Bedrock 的 SigV4 签名。
/// - Gemini 原生 `generateContent` 协议。
pub mod maintenance {}

// ─────────────────────────────────────────────────────────────────────────────
// 测试
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn protocol_json_slugs_match_config_and_frontend() {
        let cases = [
            (Protocol::AnthropicMessages, "anthropic_messages"),
            (Protocol::OpenAiChat, "openai_chat"),
            (Protocol::OpenAiResponses, "openai_responses"),
        ];
        for (protocol, expected) in cases {
            assert_eq!(protocol.as_str(), expected);
            assert_eq!(serde_json::to_value(protocol).unwrap(), expected);
            assert_eq!(Protocol::parse(expected), Some(protocol));
        }
    }

    #[test]
    fn ids_are_unique_and_slug_shaped() {
        let mut seen = HashSet::new();
        for preset in PRESETS {
            assert!(seen.insert(preset.id), "重复的 preset id: {}", preset.id);
            assert!(
                preset
                    .id
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'),
                "id 只能用 [a-z0-9_]: {}",
                preset.id
            );
        }
    }

    #[test]
    fn protocol_is_declared_native() {
        for preset in PRESETS {
            assert!(
                preset.native.contains(&preset.protocol),
                "{} 的 protocol 不在 native 列表里",
                preset.id
            );
            for candidate in preset.endpoint_candidates {
                assert!(
                    candidate.native.contains(&candidate.protocol),
                    "{} 的备用线路 {} 的 protocol 不在 native 列表里",
                    preset.id,
                    candidate.url
                );
            }
        }
    }

    /// `native` 是**按地址**的声明，所以一条 native 里不会既有 Anthropic Messages
    /// 又有 OpenAI 系协议——两者请求体、路径、SSE 事件格式全然不同，一个 URL 只会
    /// 是其中一种口。
    ///
    /// 混着写就是把"厂商支持什么"当成了"这个地址支持什么"，后果是设置页放行
    /// `https://api.z.ai/api/anthropic` + `openai_chat` 这种必然 404 的组合。厂商
    /// 同时提供两种口时，正确做法是拆成两条 `Endpoint` 或两条预设。
    #[test]
    fn native_never_mixes_anthropic_with_openai() {
        let check = |native: &[Protocol], who: &str, url: &str| {
            let has_anthropic = native.contains(&Protocol::AnthropicMessages);
            let has_openai = native
                .iter()
                .any(|p| matches!(p, Protocol::OpenAiChat | Protocol::OpenAiResponses));
            assert!(
                !(has_anthropic && has_openai),
                "{who} 的 {url} 同时声明了 Anthropic 与 OpenAI 协议"
            );
            assert!(!native.is_empty(), "{who} 的 {url} 没有声明任何协议");
        };
        for preset in PRESETS {
            check(preset.native, preset.id, preset.base_url);
            for candidate in preset.endpoint_candidates {
                check(candidate.native, preset.id, candidate.url);
            }
        }
    }

    /// `allowed_protocols` 必须覆盖 `resolve_protocol` 会选出来的那个协议，
    /// 否则设置页会显示一个"不被允许"的当前值。
    #[test]
    fn resolved_protocol_is_always_allowed() {
        for preset in PRESETS {
            let urls = std::iter::once(preset.base_url)
                .chain(preset.endpoint_candidates.iter().map(|c| c.url));
            for url in urls {
                let resolved = resolve_protocol(preset.id, url);
                let allowed = allowed_protocols(preset.id, url)
                    .unwrap_or_else(|| panic!("{} 的 {url} 没命中目录", preset.id));
                assert!(
                    allowed.contains(&resolved),
                    "{} 的 {url} 解析出 {:?}，但允许集是 {allowed:?}",
                    preset.id,
                    resolved
                );
            }
        }
    }

    #[test]
    fn reasoning_replay_only_on_responses_providers_that_support_it() {
        for preset in PRESETS {
            if !preset.reasoning_replay {
                continue;
            }
            assert_eq!(
                preset.protocol,
                Protocol::OpenAiResponses,
                "{} 打开了 reasoning_replay 但不走 Responses",
                preset.id
            );
            // 只有 OpenAI 官方与 xAI 支持 include=reasoning.encrypted_content；
            // 其余 Responses 实现（火山方舟、阿里、MiniMax…）打开会 400 或被忽略
            assert!(
                matches!(preset.id, "openai" | "xai"),
                "{} 不在已确认支持加密回传的名单里",
                preset.id
            );
        }
    }

    #[test]
    fn default_model_is_in_model_list() {
        for preset in PRESETS {
            assert!(
                preset.models.contains(&preset.model),
                "{} 的默认模型不在候选列表里",
                preset.id
            );
        }
    }

    #[test]
    fn base_url_has_no_trailing_slash_except_documented() {
        for preset in PRESETS {
            // Kimi For Coding 的官方写法就带结尾斜杠，保留原样
            if preset.id == "kimi_coding" {
                continue;
            }
            assert!(
                !preset.base_url.ends_with('/'),
                "{} 的 base_url 不该以斜杠结尾",
                preset.id
            );
        }
    }

    #[test]
    fn template_vars_appear_in_base_url() {
        for preset in PRESETS {
            for var in preset.template_vars {
                let token = format!("${{{}}}", var.name);
                assert!(
                    preset.base_url.contains(&token),
                    "{} 声明了占位符 {} 但 base_url 里没有",
                    preset.id,
                    token
                );
            }
        }
    }

    #[test]
    fn resolve_protocol_prefers_catalog_over_heuristic() {
        assert_eq!(
            resolve_protocol("kimi", "https://api.moonshot.cn/anthropic"),
            Protocol::AnthropicMessages
        );
        assert_eq!(
            resolve_protocol(
                "zhipu_coding",
                "https://open.bigmodel.cn/api/coding/paas/v4"
            ),
            Protocol::OpenAiChat
        );
    }

    #[test]
    fn resolve_protocol_falls_back_for_custom_providers() {
        assert_eq!(
            resolve_protocol("my_relay", "https://relay.example.com/anthropic"),
            Protocol::AnthropicMessages
        );
        assert_eq!(
            resolve_protocol("my_relay", "https://relay.example.com/v1"),
            Protocol::OpenAiChat
        );
    }

    #[test]
    fn repointing_a_preset_at_a_gateway_drops_back_to_guessing() {
        // 把内置 openai 指向第三方中转站是常见用法。中转站多半只有
        // Chat Completions，这时不能沿用预设声明的 Responses。
        assert_eq!(
            resolve_protocol("openai", "https://api.openai.com/v1"),
            Protocol::OpenAiResponses
        );
        assert_eq!(
            resolve_protocol("openai", "https://my-relay.example.com/v1"),
            Protocol::OpenAiChat
        );
        // 地址留空 = 保存时回填预设默认值，仍按预设算
        assert_eq!(resolve_protocol("openai", ""), Protocol::OpenAiResponses);
        // 预设自带的备用线路也算"没改"
        assert_eq!(
            resolve_protocol("kimi", "https://api.moonshot.ai/anthropic"),
            Protocol::AnthropicMessages
        );
    }

    #[test]
    fn reasoning_replay_is_off_once_the_endpoint_is_repointed() {
        assert!(resolve_reasoning_replay(
            "openai",
            "https://api.openai.com/v1"
        ));
        assert!(resolve_reasoning_replay("xai", "https://api.x.ai/v1"));
        assert!(!resolve_reasoning_replay(
            "openai",
            "https://my-relay.example.com/v1"
        ));
        assert!(!resolve_reasoning_replay(
            "kimi",
            "https://api.moonshot.cn/anthropic"
        ));
        assert!(!resolve_reasoning_replay(
            "my_relay",
            "https://x.example.com/v1"
        ));
    }

    /// 备用线路必须是"另一个地址"，否则切换按钮什么也没做。
    #[test]
    fn candidate_endpoints_are_distinct() {
        for preset in PRESETS {
            let mut seen = HashSet::new();
            for candidate in preset.endpoint_candidates {
                let url = normalize_url(candidate.url);
                assert_ne!(
                    url,
                    normalize_url(preset.base_url),
                    "{} 的备用线路与主入口相同",
                    preset.id
                );
                assert!(seen.insert(url), "{} 有重复的备用线路", preset.id);
                assert!(
                    !candidate.label.is_empty(),
                    "{} 的备用线路缺少说明",
                    preset.id
                );
            }
        }
    }

    /// 回归：切到备用线路要连协议一起切。
    ///
    /// 目录里 16 条备用线路有 13 条与主入口**不同协议**——火山 `/api/coding` 是
    /// Anthropic 而 `/api/coding/v3` 是 OpenAI，MiniMax / 小米 / 美团 / OpenRouter
    /// 同理。以前候选只记 URL，`preset_for` 一律沿用主入口协议，于是"点一下备用
    /// 线路"变成了静默换协议，而且这个错误结论还会被存进 config.toml。
    #[test]
    fn switching_to_a_candidate_endpoint_switches_protocol() {
        let cases = [
            (
                "ark_coding",
                "https://ark.cn-beijing.volces.com/api/coding/v3",
            ),
            ("openrouter", "https://openrouter.ai/api/v1"),
            ("longcat", "https://api.longcat.chat/openai/v1"),
            ("minimax", "https://api.minimaxi.com/v1"),
            ("zai", "https://api.z.ai/api/coding/paas/v4"),
        ];
        for (id, candidate_url) in cases {
            let preset = find(id).unwrap();
            assert_eq!(
                preset.protocol,
                Protocol::AnthropicMessages,
                "{id} 的主入口应是 Anthropic 口"
            );
            assert_eq!(
                resolve_protocol(id, candidate_url),
                Protocol::OpenAiChat,
                "{id} 切到 {candidate_url} 后应走 OpenAI Chat"
            );
        }

        // 同协议的候选（镜像站）不受影响
        assert_eq!(
            resolve_protocol("kimi", "https://api.moonshot.ai/anthropic"),
            Protocol::AnthropicMessages
        );
        assert_eq!(
            resolve_protocol("ark", "https://ark.cn-shanghai.volces.com/api/v3"),
            Protocol::OpenAiChat
        );
    }

    /// 可选协议的范围要跟着命中的入口走，而不是拿主入口的 `native` 一刀切。
    #[test]
    fn allowed_protocols_is_scoped_to_the_matched_endpoint() {
        // 主入口：给预设声明的全部 native
        assert_eq!(
            allowed_protocols("openai", "https://api.openai.com/v1"),
            Some(vec![Protocol::OpenAiChat, Protocol::OpenAiResponses])
        );
        // 地址留空同样按主入口算（保存时会回填）
        assert_eq!(
            allowed_protocols("openai", ""),
            Some(vec![Protocol::OpenAiChat, Protocol::OpenAiResponses])
        );
        // 备用线路：只认它自己那一个。ark_coding 的 native 是 [Anthropic]，
        // 但候选 /v3 是 OpenAI 口——拿 native 卡它会把合法组合拦下。
        let ark_openai_port = "https://ark.cn-beijing.volces.com/api/coding/v3";
        assert_eq!(
            allowed_protocols("ark_coding", ark_openai_port),
            Some(vec![Protocol::OpenAiChat])
        );
        // 改到目录以外：不设限
        let gateway = "https://relay.example.com/v1";
        assert_eq!(allowed_protocols("openai", gateway), None);
        assert_eq!(allowed_protocols("my_relay", gateway), None);
    }

    #[test]
    fn ark_plan_and_payg_are_separate_entries() {
        // 配错 base_url 会静默转成按量计费，两条必须分开存在
        let plan = find("ark_coding").unwrap();
        let payg = find("ark").unwrap();
        assert!(plan.base_url.contains("/api/coding"));
        assert!(!payg.base_url.contains("/api/coding"));
    }
}
