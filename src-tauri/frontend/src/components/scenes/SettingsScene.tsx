import { Fragment, useCallback, useEffect, useRef, useState } from "react";
import { errText } from "../../lib/format";
import { useAppStore, type SettingsPane } from "../../store/app";
import { usePoll } from "../../lib/poll";
import {
  codexCliPreferences,
  codexIntegrationStatus,
  codexSaveCliPreferences,
  codexSetupCollaboration,
  codexStartDeviceLogin,
  codexStartLogin,
  companionEnsure,
  logsTail,
  providerModels,
  IpcCommandError,
  RTK_BLOCKED_BY_SECURITY_SOFTWARE,
  rtkOpenSecurityExclusions,
  rtkSetEnabled,
  rtkStatus,
  settingsDeleteProvider,
  settingsGet,
  settingsSaveProvider,
  settingsSelectProvider,
  settingsSet,
  supportBundleChoose,
  supportPreview,
} from "../../lib/ipc";
import type {
  AppConfig,
  CodexCliPreferences,
  CodexIntegrationStatus,
  CodexModelOption,
  HostedWebRoute,
  LogEntry,
  OrchestrationConfig,
  ProviderCategory,
  ProviderConfig,
  ProviderPreset,
  ProviderProtocol,
  ProviderStatus,
  RtkStatus,
  SupportBundlePreview,
} from "../../lib/types";
import { clockTime } from "../../lib/format";
import {
  catalogHostedWebRoutes,
  catalogPresets,
  loadCatalog,
  presetOf,
  providerLabel,
  rememberModel,
} from "../../lib/provider";
import { useCodexCliGate } from "../codex/CodexCliGate";
import { CODEX_LOGIN_WAIT_MINUTES, nextCodexLoginPollDelay } from "../codex/login-watcher";
import { IconCheck, IconChevronDown, IconRefresh, IconSearch } from "../icons";
import { Menu, MenuEmpty, MenuItem } from "../ui/Menu";
import { ConfirmDialog } from "../ui/ConfirmDialog";
import { pushToast } from "../../store/toast";
import { useCompanionStore, type CompanionMotion } from "../../store/companion";
import { McpPanel } from "./McpPanel";
import { KnowledgeSettingsPane } from "./KnowledgeSettingsPane";
import { SubagentProvidersPanel } from "./SubagentProvidersPanel";

const LOG_LEVELS = ["debug", "info", "warn", "error"];
const LOG_FILTERS = ["all", "error", "warn", "info", "debug"] as const;
const EMPTY_PROVIDERS: NonNullable<AppConfig["providers"]> = {};
const OUTPUT_DEFAULT = "8192";
/** 自建网关：不套用任何预设，全部字段手填。 */
const CUSTOM_PRESET = "custom";

const SETTINGS_PANES: Array<{
  key: SettingsPane;
  label: string;
  description: string;
}> = [
  { key: "providers", label: "模型服务", description: "配置 R-Code 对话使用的模型与凭据。" },
  { key: "agents", label: "Agent 编排", description: "选择主 Agent、委派路由和可观察的质量复核。" },
  { key: "tools", label: "工具与连接", description: "管理内置联网、MCP 服务、凭据和扩展市场。" },
  { key: "knowledge", label: "知识与指令", description: "管理公共与项目记忆、协作 Prompt 和可复用 Skills。" },
  { key: "preferences", label: "外观与小助手", description: "选择界面主题，并管理桌面小助手的反馈方式。" },
  { key: "diagnostics", label: "诊断", description: "查看运行日志，或导出脱敏支持信息。" },
  { key: "subagents", label: "子代理配置", description: "组合 API Provider 与 Codex CLI 来源，配置子代理槽位、权重、Prompt 与连通性。" },
];

const CATEGORY_LABELS: Record<ProviderCategory, string> = {
  official: "海外官方",
  cn_official: "国内厂商",
  cloud_provider: "云厂商托管",
  aggregator: "路由 / 聚合",
};

const PROTOCOL_LABELS: Record<ProviderProtocol, string> = {
  anthropic_messages: "Anthropic Messages",
  openai_chat: "OpenAI Chat Completions",
  openai_responses: "OpenAI Responses",
};

const DEEPSEEK_RESPONSES_MODELS = new Set(["deepseek-v4-flash", "deepseek-v4-pro"]);

/** 按 category 分组，保持目录里的原始顺序。 */
function groupByCategory(presets: ProviderPreset[]) {
  const groups = new Map<ProviderCategory, ProviderPreset[]>();
  for (const preset of presets) {
    const bucket = groups.get(preset.category);
    if (bucket) bucket.push(preset);
    else groups.set(preset.category, [preset]);
  }
  return [...groups.entries()];
}

const ALL_PROTOCOLS: ProviderProtocol[] = ["openai_chat", "anthropic_messages", "openai_responses"];

/**
 * 新建（还没有后端状态）时下拉框的初值。
 *
 * 与后端 `infer_protocol_never_responses` 同规则：预设推荐值，但 Responses 降级为
 * Chat。Responses 与 Chat 常在同一地址上都可用而计费不同，必须由用户主动选。
 */
function fallbackProtocol(preset: ProviderPreset | undefined): ProviderProtocol {
  const inferred = preset?.protocol ?? "openai_chat";
  return inferred === "openai_responses" ? "openai_chat" : inferred;
}

const normalizeUrl = (url: string) => url.trim().replace(/\/+$/, "").toLowerCase();

/**
 * 该地址允许选哪些协议，`null` = 目录管不到、不设限。
 *
 * 必须与后端 `provider_catalog::allowed_protocols` 逐条对齐，否则 UI 会拦下后端愿意
 * 接受的选择、或者放行后端会拒绝的。规则：主入口给 `native`；备用线路只给它自己
 * 那一个（我们对候选地址的了解仅限目录里写的那条）；改到目录以外则不设限。
 */
function allowedProtocols(
  preset: ProviderPreset | undefined,
  baseUrl: string
): ProviderProtocol[] | null {
  if (!preset) return null;
  const url = normalizeUrl(baseUrl);
  // 留空 = 保存时回填预设地址，按主入口算
  if (!url || url === normalizeUrl(preset.base_url)) return preset.native;
  const candidate = preset.endpoint_candidates.find((item) => normalizeUrl(item.url) === url);
  return candidate ? candidate.native : null;
}

function protocolChoices(
  preset: ProviderPreset | undefined,
  baseUrl: string,
  current: ProviderProtocol
): ProviderProtocol[] {
  const choices = allowedProtocols(preset, baseUrl) ?? ALL_PROTOCOLS;
  // 当前值必须在选项里，否则 <select> 会显示第一项而 state 仍是旧值，
  // 用户看到的和即将提交的对不上。
  return choices.includes(current) ? [...choices] : [...choices, current];
}

/** base_url 里还有没填的 `${VAR}` 占位符。 */
function unresolvedTemplateVars(preset: ProviderPreset | undefined, baseUrl: string) {
  if (!preset) return [];
  return preset.template_vars.filter((variable) => baseUrl.includes(`\${${variable.name}}`));
}

function optionalInteger(value: string) {
  const normalized = value.trim();
  if (!normalized) return null;
  const parsed = Number(normalized);
  if (!Number.isInteger(parsed) || parsed <= 0) {
    throw new Error("最大输出 Token 必须是大于 0 的整数");
  }
  return parsed;
}

function optionalDecimal(value: string) {
  const normalized = value.trim();
  if (!normalized) return null;
  const parsed = Number(normalized);
  if (!Number.isFinite(parsed)) throw new Error("随机性必须是数字");
  return parsed;
}

function displayNumber(value: number | undefined) {
  if (value == null) return "";
  return Number(value.toFixed(4)).toString();
}

function isDeepSeekV4(baseUrl: string, model: string, preset: string) {
  return (preset === "deepseek" || baseUrl.includes("api.deepseek.com")) &&
    model.trim().toLowerCase().startsWith("deepseek-v4-");
}

type ProviderWebGuidance = {
  state: "hosted" | "client" | "attention" | "gateway";
  badge: string;
  description: string;
  docsUrl?: string;
  docsLabel?: string;
};

type ParsedProviderUrl = {
  host: string;
  path: string;
  isOfficialTransport: boolean;
};

function parsedProviderUrl(baseUrl: string): ParsedProviderUrl | null {
  try {
    const url = new URL(baseUrl.trim());
    return {
      host: url.hostname.toLowerCase(),
      path: url.pathname.replace(/\/+$/, ""),
      isOfficialTransport:
        url.protocol === "https:" &&
        (url.port === "" || url.port === "443") &&
        url.username === "" &&
        url.password === "",
    };
  } catch {
    return null;
  }
}

function routeHostMatches(pattern: string, host: string) {
  const normalized = pattern.toLowerCase();
  if (!normalized.startsWith("*.")) return host === normalized;
  const suffix = normalized.slice(1);
  return host.endsWith(suffix) && host.length > suffix.length;
}

function routeEndpointMatches(route: HostedWebRoute, baseUrl: string) {
  const parsed = parsedProviderUrl(baseUrl);
  return Boolean(
    parsed &&
      parsed.isOfficialTransport &&
      routeHostMatches(route.host_pattern, parsed.host) &&
      route.path === parsed.path
  );
}

function routeModelMatches(patterns: string[], model: string) {
  if (patterns.length === 0) return true;
  const normalized = model.trim().toLowerCase();
  return patterns.some((pattern) => {
    const candidate = pattern.toLowerCase();
    if (candidate === "*") return true;
    return candidate.endsWith("*")
      ? normalized.startsWith(candidate.slice(0, -1))
      : normalized === candidate;
  });
}

const PROVIDER_HOSTED_ROUTE_HINTS: Record<string, string> = {
  openai: "请把线路协议切换为 Responses。Chat Completions 仍只使用 R-Code / MCP 工具。",
  xai: "请把线路协议切换为 Responses，才能启用 xAI 服务端 Web Search。",
  azure_openai:
    "请切换为 Responses；此外 Azure 订阅管理员必须允许 Web Search，当前鉴权仍需 Entra Token。",
  deepseek:
    "Responses 与 Anthropic 兼容口均支持 DeepSeek V4 Flash/Pro。Chat 只有普通 Tool Call。",
  ark: "请切换为 Responses，并使用已支持内置搜索的 Doubao Seed 2 系列模型。Coding / Agent Plan 订阅线路不在此范围。",
  dashscope:
    "请切换为 Responses，并使用 qwen3.8 / qwen3.7 / qwen3.6 / qwen3.5 或 qwen3-max 系列；Coder 候选未在官方支持清单中。",
  openrouter:
    "请切换到 https://openrouter.ai/api/v1 的 Chat 或 Responses 线路；Anthropic 兼容口不使用 OpenRouter Server Tools。",
};

const PROVIDER_WEB_ALTERNATIVES: Record<
  string,
  { badge?: string; description: string; docsUrl: string; docsLabel: string }
> = {
  kimi: {
    description:
      "Kimi 另有 Chat 的 $web_search 与 Formula 的 search/fetch 工具，但当前 Anthropic 兼容口不是同一套协议，R-Code 不会直接注入。当前对话仍走 R-Code / MCP 工具。",
    docsUrl: "https://platform.kimi.com/docs/guide/use-official-tools",
    docsLabel: "查看 Kimi 官方工具",
  },
  kimi_coding: {
    description:
      "Kimi 另有 Formula search/fetch 工具；Coding 订阅口未确认兼容这套服务端工作流，当前仍走 R-Code / MCP 工具。",
    docsUrl: "https://platform.kimi.com/docs/guide/use-official-tools",
    docsLabel: "查看 Kimi 官方工具",
  },
  zhipu: {
    description:
      "智谱主 Chat API 支持厂商 Web Search，但当前 Anthropic 兼容口不是该请求结构。R-Code 不会把协议兼容误当成服务端工具兼容。",
    docsUrl: "https://docs.bigmodel.cn/cn/guide/tools/web-search",
    docsLabel: "查看智谱 Web Search",
  },
  zhipu_coding: {
    description:
      "智谱另有 Web Search API / Chat 工具；Coding Plan 线路未确认支持相同参数，当前仍走 R-Code / MCP 工具。",
    docsUrl: "https://docs.bigmodel.cn/cn/guide/tools/web-search",
    docsLabel: "查看智谱 Web Search",
  },
  zai: {
    description:
      "智谱系另有 Web Search API / Chat 工具；当前 Z.ai Anthropic / Coding 线路未确认支持相同参数，当前仍走 R-Code / MCP 工具。",
    docsUrl: "https://docs.bigmodel.cn/cn/guide/tools/web-search",
    docsLabel: "查看智谱 Web Search",
  },
  ark_coding: {
    badge: "按量线路另有能力",
    description:
      "火山方舟按量 API 的 Responses + Doubao Seed 2 支持内置 Web Search；Coding Plan 是另一条计费与协议线路，不能直接注入。需要厂商托管搜索时请改用“火山方舟（按量 API）”。",
    docsUrl: "https://www.volcengine.com/docs/82379/1958524?lang=zh",
    docsLabel: "查看方舟 Responses 内置工具",
  },
  ark_coding_openai: {
    badge: "按量线路另有能力",
    description:
      "当前 Coding Plan OpenAI 口只有 Chat Completions；内置 Web Search 位于方舟按量 API 的 Responses + Doubao Seed 2 组合，不能跨线路套用。",
    docsUrl: "https://www.volcengine.com/docs/82379/1958524?lang=zh",
    docsLabel: "查看方舟 Responses 内置工具",
  },
  ark_agent: {
    badge: "订阅线路",
    description:
      "方舟官方现称 Agent Plan（Token Plan）。套餐另含豆包搜索 Harness，但 /api/plan 的 Anthropic 模型口不能直接套用按量 Responses 的 Web Search 参数；当前对话继续使用 R-Code / MCP 工具。",
    docsUrl: "https://www.volcengine.com/docs/82379/2366394?lang=zh",
    docsLabel: "查看 Agent Plan 套餐与模型",
  },
  byteplus: {
    badge: "可接远程 MCP",
    description:
      "BytePlus 按量 ModelArk Responses 支持远程 MCP，但当前预设是 Coding Plan，官方未确认它带内置 Web Search / Fetch。当前继续使用 R-Code / MCP，不自动注入厂商工具。",
    docsUrl: "https://docs.byteplus.com/en/docs/modelark/1585128",
    docsLabel: "查看 ModelArk Responses 工具",
  },
  bailian: {
    badge: "Responses 线路另有能力",
    description:
      "百炼 OpenAI 兼容 Responses 可声明 Web Search 与 Web Extractor；当前 Anthropic 兼容口不是同一请求结构。需要托管联网时请改用“阿里百炼（OpenAI 兼容）”并选择受支持的 Qwen 模型。",
    docsUrl: "https://help.aliyun.com/zh/model-studio/web-search/",
    docsLabel: "查看百炼联网搜索",
  },
  bailian_coding: {
    badge: "可配置搜索 MCP",
    description:
      "百炼 Coding Plan 官方通过 Web Search MCP 扩展联网，并要求单独的通用百炼 API Key；套餐模型口本身不会继承按量 Responses 的 Web Search / Extractor。",
    docsUrl: "https://help.aliyun.com/zh/model-studio/web-search-mcp",
    docsLabel: "查看百炼 Web Search MCP",
  },
  stepfun: {
    description:
      "阶跃标准 Chat API 有托管 Web Search，StepSearch MCP 同时提供 web_search / web_fetch；当前 Step Plan Anthropic 线路不直接注入这些参数。",
    docsUrl: "https://platform.stepfun.com/docs/zh/step-plan/integrations/search-mcp",
    docsLabel: "查看 StepSearch MCP",
  },
  longcat: {
    badge: "官方要求关闭",
    description:
      "LongCat 官方 Codex 配置明确设置 web_search = disabled；当前线路只按普通 Tool Call 使用 R-Code / MCP，不声明厂商托管搜索。",
    docsUrl: "https://longcat.chat/platform/docs/zh/Codex.html",
    docsLabel: "查看 LongCat Codex 配置",
  },
  xiaomi_mimo: {
    description:
      "小米官方目前明确记录的是 mimo-v2-flash 联网插件；当前 v2.5 模型与线路未确认同样可用，因此仍走 R-Code / MCP 工具。",
    docsUrl: "https://platform.xiaomimimo.com/docs/zh-CN/news/previous-news/news20260303",
    docsLabel: "查看 MiMo 联网说明",
  },
  xiaomi_mimo_plan: {
    description:
      "小米官方目前明确记录的是 mimo-v2-flash 联网插件；Token Plan 的 v2.5 线路未确认同样可用，因此仍走 R-Code / MCP 工具。",
    docsUrl: "https://platform.xiaomimimo.com/docs/zh-CN/news/previous-news/news20260303",
    docsLabel: "查看 MiMo 联网说明",
  },
  minimax: {
    description:
      "MiniMax 官方提供的是独立 Web Search MCP，不是当前 Anthropic 模型请求里的厂商托管工具；可安装 MCP，未安装时使用 R-Code 工具。",
    docsUrl: "https://platform.minimax.io/docs/token-plan/mcp-guide",
    docsLabel: "查看 MiniMax MCP",
  },
  minimax_intl: {
    description:
      "MiniMax 官方提供的是独立 Web Search MCP，不是当前 Anthropic 模型请求里的厂商托管工具；可安装 MCP，未安装时使用 R-Code 工具。",
    docsUrl: "https://platform.minimax.io/docs/token-plan/mcp-guide",
    docsLabel: "查看 MiniMax MCP",
  },
  qianfan_coding: {
    description:
      "百度千帆提供独立 AI Search API，但它使用单独端点与应用凭据，不是 Coding Plan 模型内置工具；当前仍走 R-Code / MCP 工具。",
    docsUrl: "https://cloud.baidu.com/doc/qianfan-api/s/Wmbq4z7e5",
    docsLabel: "查看千帆 Search API",
  },
  bedrock: {
    description:
      "Anthropic 官方 Web Search / Fetch 不在 Amazon Bedrock 提供；该线路使用 R-Code / MCP 工具。",
    docsUrl: "https://platform.claude.com/docs/en/agents-and-tools/tool-use/web-search-tool",
    docsLabel: "查看 Anthropic 可用范围",
  },
};

/**
 * 只把后端目录中已验证、已接线的组合显示为“厂商托管”。模型会普通 Tool Call
 * 不代表厂商会替客户端执行搜索，这是 401 误判最常见的来源。
 */
function providerWebGuidance(
  preset: ProviderPreset | undefined,
  baseUrl: string,
  protocol: ProviderProtocol,
  model: string,
  routes: HostedWebRoute[]
): ProviderWebGuidance {
  const matched = routes.find(
    (route) =>
      routeEndpointMatches(route, baseUrl) &&
      route.protocol === protocol &&
      routeModelMatches(route.model_patterns, model)
  );
  if (matched) {
    const description = matched.read === "dedicated"
      ? matched.format === "dash_scope"
        ? "当前线路会声明 Web Search 与 Web Extractor，搜索和指定网页读取都由阿里百炼执行，无需另配搜索服务密钥；调用可能另行计费。"
        : `当前线路会声明 Web Search 与 Web Fetch，两者都由 ${matched.provider_label} 服务端执行，无需另配搜索服务密钥；调用可能另行计费。`
      : matched.read === "via_search"
        ? `当前线路会启用 ${matched.provider_label} Web Search；打开网页和页内查找由同一托管工具完成，不需要独立 Web Fetch。`
        : `当前线路会启用 ${matched.provider_label} 服务端 Web Search。厂商未确认独立 Web Fetch，读取指定 URL 时仍由 R-Code / MCP 工具处理。`;
    return {
      state: "hosted",
      badge: `${matched.provider_label} 托管`,
      description,
      docsUrl: matched.docs_url,
      docsLabel: matched.docs_label,
    };
  }

  const endpointRoutes = routes.filter((route) => routeEndpointMatches(route, baseUrl));
  const presetRoutes = preset
    ? routes.filter((route) => route.provider_id === preset.id)
    : [];
  if (endpointRoutes.length > 0 || presetRoutes.length > 0) {
    const knownPresetUrl = Boolean(
      preset &&
        [preset.base_url, ...preset.endpoint_candidates.map((candidate) => candidate.url)]
          .some((url) => normalizeUrl(url) === normalizeUrl(baseUrl))
    );
    if (endpointRoutes.length > 0 || knownPresetUrl || preset?.template_vars.length) {
      const reference = endpointRoutes[0] ?? presetRoutes[0];
      return {
        state: "attention",
        badge: "需切换线路",
        description:
          PROVIDER_HOSTED_ROUTE_HINTS[preset?.id ?? reference.provider_id] ??
          "厂商端点已识别，但当前协议或模型不在 R-Code 已接入的托管联网组合中；当前仍使用 R-Code / MCP 工具。",
        docsUrl: reference.docs_url,
        docsLabel: reference.docs_label,
      };
    }
    return {
      state: "gateway",
      badge: "能力未确认",
      description:
        "当前地址不是目录中已验证的官方联网端点，因此不会自动注入厂商工具。兼容协议只代表请求格式相近；搜索与网页读取继续走 R-Code / MCP。",
    };
  }

  const alternative = preset ? PROVIDER_WEB_ALTERNATIVES[preset.id] : undefined;
  if (alternative) {
    return {
      state: "attention",
      badge: alternative.badge ?? "厂商另有接口",
      ...alternative,
    };
  }

  if (!preset) {
    return {
      state: "gateway",
      badge: "能力未确认",
      description:
        "自建服务的厂商托管工具能力无法从兼容协议推断。当前使用 R-Code 的 web_search / web_fetch 或已安装的 MCP。",
    };
  }

  return {
    state: "client",
    badge: "R-Code / MCP",
    description:
      "当前线路未启用厂商托管联网。模型仍可调用 R-Code 的 web_search / web_fetch 或已安装的 MCP；普通 Tool Call 能力不等于厂商自带搜索服务。",
    docsUrl: preset.website_url,
    docsLabel: "查看厂商接口说明",
  };
}

function providerStateLabel(status: ProviderStatus | undefined) {
  if (!status?.ready) return "待完成";
  return status.source === "environment" ? "环境变量" : "可使用";
}

/**
 * 设置页：模型服务、外观与小助手、日志、支持包与子代理配置。
 * settingsGet 失败（配置损坏等）时表单区显示错误条而非空白。
 */
export function SettingsScene() {
  const activePane = useAppStore((state) => state.settingsPane);
  const setActivePane = useAppStore((state) => state.setSettingsPane);
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [configErr, setConfigErr] = useState<string | null>(null);
  const [validation, setValidation] = useState<string | null>(null);
  const [providerStatus, setProviderStatus] = useState<Record<string, ProviderStatus>>({});

  const loadConfig = useCallback(async () => {
    try {
      const res = await settingsGet();
      setConfig(res.config);
      setValidation(res.validation);
      setProviderStatus(res.provider_status ?? {});
      setConfigErr(null);
    } catch (e) {
      setConfigErr(errText(e));
    }
  }, []);

  useEffect(() => {
    void loadConfig();
  }, [loadConfig]);

  const pane = SETTINGS_PANES.find((item) => item.key === activePane) ?? SETTINGS_PANES[0];

  return (
    <div className="scene">
      <div className="scene-scroll">
        <div className="page-head">
          <h1>设置</h1>
        </div>

        <div className="settings-layout">
          <nav className="settings-nav" aria-label="设置分类">
            {SETTINGS_PANES.map((item) => (
              <button
                key={item.key}
                className={activePane === item.key ? "active" : ""}
                aria-current={activePane === item.key ? "page" : undefined}
                onClick={() => setActivePane(item.key)}
              >
                {item.label}
              </button>
            ))}
          </nav>

          <div className="settings-detail">
            <header className="settings-detail-head">
              <h2>{pane.label}</h2>
              <p>{pane.description}</p>
            </header>

            {configErr && (activePane === "providers" || activePane === "agents" || activePane === "diagnostics" || activePane === "subagents") && (
              <div className="errbar" role="alert">
                读取配置失败：{configErr}
                <span className="spacer" />
                <button className="btn" onClick={() => void loadConfig()}>
                  重试
                </button>
              </div>
            )}
            {activePane === "providers" && validation && !configErr && (
              <div className="notebar" role="status">
                选择模型服务并保存访问密钥后即可开始对话。
                <span className="dim">{validation}</span>
              </div>
            )}

            {activePane === "providers" && (
              <div className="settings-sheet">
                {config ? (
                  <ProviderSection config={config} providerStatus={providerStatus} reload={loadConfig} />
                ) : (
                  !configErr && <div className="settings-loading">正在读取模型服务…</div>
                )}
              </div>
            )}

            {activePane === "preferences" && (
              <div className="settings-preferences">
                <AppearanceSection />
                <CompanionSection />
              </div>
            )}

            {activePane === "tools" && (
              <div className="settings-sheet settings-tools-sheet">
                <McpPanel />
              </div>
            )}

            {activePane === "knowledge" && <KnowledgeSettingsPane />}

            {activePane === "agents" && (
              <div className="settings-sheet">
                {config ? (
                  <OrchestrationSection config={config} reload={loadConfig} />
                ) : (
                  !configErr && <div className="settings-loading">正在读取 Agent 编排策略…</div>
                )}
              </div>
            )}

            {activePane === "diagnostics" && (
              <div className="settings-sheet">
                {config && <LogLevelSection config={config} reload={loadConfig} />}
                <LogSection />
                <SupportSection />
              </div>
            )}

            {activePane === "subagents" && (
              <div className="settings-sheet subagent-configuration-sheet">
                <SubagentProvidersPanel />
                <CodexIntegrationSection config={config} reloadConfig={loadConfig} />
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

// ---------- Provider ----------

function ProviderSection({
  config,
  providerStatus,
  reload,
}: {
  config: AppConfig;
  providerStatus: Record<string, ProviderStatus>;
  reload: () => Promise<void>;
}) {
  const configDefault = config.default_provider ?? "";
  const providers = config.providers ?? EMPTY_PROVIDERS;
  const names = Object.keys(providers).sort((a, b) => a.localeCompare(b));
  const [catalog, setCatalog] = useState<ProviderPreset[]>([]);
  const [hostedWebRoutes, setHostedWebRoutes] = useState<HostedWebRoute[]>([]);
  const [selectedProvider, setSelectedProvider] = useState<string | null>(null);
  const [presetName, setPresetName] = useState(CUSTOM_PRESET);
  // 新建草稿态：点击“新建服务”后左侧出现占位行，保存成功或取消后消失。
  const [drafting, setDrafting] = useState(false);
  // 新建态下点击已有服务时暂存目标，等用户确认“放弃未保存的更改”后再切换。
  const [pendingSelect, setPendingSelect] = useState<string | null>(null);
  const [profileName, setProfileName] = useState("");
  const [fields, setFields] = useState({
    base_url: "",
    model: "",
    max_tokens: OUTPUT_DEFAULT,
    temperature: "0.2",
    protocol: "openai_chat" as ProviderProtocol,
    show_reasoning: true,
  });
  const [keyInput, setKeyInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [saved, setSaved] = useState<string | null>(null);
  const [remoteModels, setRemoteModels] = useState<string[]>([]);
  const [modelsBusy, setModelsBusy] = useState(false);
  const [modelsMessage, setModelsMessage] = useState<string | null>(null);
  const [modelsError, setModelsError] = useState<string | null>(null);
  const modelRequest = useRef(0);
  const initialPresetApplied = useRef(false);

  // 目录来自后端 provider_catalog.rs：预设一旦分散成两份就会漂移，
  // 前端不再自带硬编码表。
  useEffect(() => {
    let alive = true;
    void loadCatalog().then(() => {
      if (!alive) return;
      const presets = catalogPresets();
      setCatalog(presets);
      setHostedWebRoutes(catalogHostedWebRoutes());
    });
    return () => {
      alive = false;
    };
  }, []);

  useEffect(() => {
    setSelectedProvider((current) => {
      if (current && providers[current]) return current;
      if (configDefault && providers[configDefault]) return configDefault;
      return names[0] ?? null;
    });
  }, [configDefault, names.join("|")]);

  const applyPreset = useCallback((nextPreset: string) => {
    const preset = presetOf(nextPreset);
    setPresetName(nextPreset);
    setProfileName(nextPreset === CUSTOM_PRESET ? "" : nextPreset);
    setFields({
      base_url: preset?.base_url ?? "",
      model: preset?.model ?? "",
      // 预设声明了单次输出上限时用它，避免保存后被服务端 400
      max_tokens: preset?.max_output_tokens != null
        ? String(Math.min(preset.max_output_tokens, Number(OUTPUT_DEFAULT)))
        : OUTPUT_DEFAULT,
      temperature: "0.2",
      // 新建同样不预选 Responses：下拉框里"看得见"不等于用户确认过。想用 Responses
      // 就自己去选一下，这条规矩对新建和编辑一视同仁。
      protocol: fallbackProtocol(preset),
      show_reasoning: true,
    });
  }, []);

  useEffect(() => {
    if (!selectedProvider) {
      setKeyInput("");
      setSaved(null);
      setErr(null);
      // 没有任何已保存服务时，目录异步返回后的首次初始化必须整组应用预设。
      // 只改 presetName 会造成下拉框显示 Anthropic，而名称、地址和协议仍是空白/Chat。
      if (!initialPresetApplied.current && catalog.length > 0 && names.length === 0) {
        initialPresetApplied.current = true;
        applyPreset(catalog[0].id);
      } else {
        applyPreset(presetName);
      }
      return;
    }
    const profile = providers[selectedProvider] as ProviderConfig | undefined;
    const preset = presetOf(profile?.provider_kind ?? selectedProvider);
    setProfileName(selectedProvider);
    setPresetName(preset?.id ?? CUSTOM_PRESET);
    setFields({
      base_url: profile?.base_url ?? preset?.base_url ?? "",
      model: profile?.model ?? preset?.model ?? "",
      max_tokens: profile?.max_tokens != null ? String(profile.max_tokens) : OUTPUT_DEFAULT,
      temperature: displayNumber(profile?.temperature) || "0.2",
      // 编辑已有配置时以后端算出的 effective_protocol 为准——它已经把"存过的值"
      // 和"地址被改写后的推断"都算进去了。前端再推一遍只会和后端对不上，而用户
      // 随手点个保存就会把错的那个存下来。
      protocol:
        profile?.protocol ??
        providerStatus[selectedProvider]?.effective_protocol ??
        fallbackProtocol(preset),
      show_reasoning: profile?.show_reasoning ?? true,
    });
    setKeyInput("");
    setSaved(null);
    setErr(null);
    // catalog 触发目录异步返回后的重算；否则已有内置配置可能被误显示为“自建服务”。
  }, [applyPreset, catalog, names.length, presetName, providers, providerStatus, selectedProvider]);

  // 地址、协议或编辑对象变化后，旧请求结果不再属于当前表单。
  useEffect(() => {
    modelRequest.current += 1;
    setRemoteModels([]);
    setModelsMessage(null);
    setModelsError(null);
    setModelsBusy(false);
  }, [selectedProvider, presetName, fields.base_url, fields.protocol]);

  const activePreset = presetOf(presetName);
  const pendingVars = unresolvedTemplateVars(activePreset, fields.base_url);
  const modelChoices = Array.from(
    new Set(
      [fields.model, ...remoteModels, ...(activePreset?.models ?? [])]
        .map((model) => model.trim())
        .filter(Boolean)
    )
  );

  const fetchModels = async () => {
    if (modelsBusy || busy) return;
    const requestId = ++modelRequest.current;
    setModelsBusy(true);
    setModelsMessage(null);
    setModelsError(null);
    try {
      const response = await providerModels({
        name: profileName.trim(),
        preset: activePreset?.id ?? null,
        baseUrl: fields.base_url,
        apiKey: keyInput.trim() || null,
        protocol: fields.protocol,
      });
      if (modelRequest.current !== requestId) return;
      setRemoteModels(response.models);
      if (!fields.model.trim() && response.models[0]) {
        setFields((value) => ({ ...value, model: response.models[0] }));
      }
      setModelsMessage(`服务返回 ${response.models.length} 个可用模型`);
    } catch (cause) {
      if (modelRequest.current !== requestId) return;
      setModelsError(errText(cause));
    } finally {
      if (modelRequest.current === requestId) setModelsBusy(false);
    }
  };

  const run = async (fn: () => Promise<void>, message: string) => {
    if (busy) return;
    setBusy(true);
    setErr(null);
    setSaved(null);
    try {
      await fn();
      await reload();
      setSaved(message);
    } catch (e) {
      setErr(errText(e));
    } finally {
      setBusy(false);
    }
  };

  const saveProvider = (activate: boolean) =>
    void run(async () => {
      const name = profileName.trim();
      if (!name) throw new Error("请为这项配置填写名称");
      await settingsSaveProvider({
        name,
        providerKind: activePreset?.id ?? "",
        baseUrl: fields.base_url,
        model: fields.model,
        apiKey: keyInput.trim() || null,
        maxTokens: optionalInteger(fields.max_tokens),
        temperature: optionalDecimal(fields.temperature),
        protocol: fields.protocol,
        showReasoning: fields.show_reasoning,
        activate,
      });
      setSelectedProvider(name);
      setDrafting(false);
      setPendingSelect(null);
      setKeyInput("");
    }, activate ? "已保存，并用于后续新对话" : "配置已保存");

  const selectProvider = (name: string) =>
    void run(() => settingsSelectProvider(name), "已切换，新对话将使用这项服务");

  const deleteProvider = (name: string) => {
    if (!window.confirm(`删除“${providerLabel(name)}”及其本机凭据？此操作无法撤销。`)) return;
    void run(async () => {
      await settingsDeleteProvider(name);
      if (selectedProvider === name) setSelectedProvider(null);
    }, "配置已删除");
  };

  const startNewProvider = () => {
    setDrafting(true);
    setPendingSelect(null);
    setSelectedProvider(null);
    applyPreset(catalog[0]?.id ?? CUSTOM_PRESET);
  };

  const cancelDraft = () => {
    setDrafting(false);
    setPendingSelect(null);
    setSelectedProvider(
      configDefault && providers[configDefault] ? configDefault : names[0] ?? null
    );
  };

  const commitPendingSelect = () => {
    const target = pendingSelect;
    setPendingSelect(null);
    setDrafting(false);
    setSelectedProvider(target);
  };

  const editing = selectedProvider ? (providers[selectedProvider] as ProviderConfig | undefined) : undefined;
  const credential = selectedProvider ? providerStatus[selectedProvider] : undefined;
  const credentialLabel = credential?.configured
    ? credential.source === "environment"
      ? "由环境变量提供"
      : "已安全保存"
    : "尚未保存";
  const deepSeekV4 = isDeepSeekV4(fields.base_url, fields.model, presetName);
  const outputValue = Number(fields.max_tokens.trim());
  const outputExceedsDeepSeekLimit = deepSeekV4 && Number.isFinite(outputValue) && outputValue > 393_216;
  const deepSeekResponsesModelUnsupported =
    activePreset?.id === "deepseek" &&
    normalizeUrl(fields.base_url) === normalizeUrl(activePreset.base_url) &&
    fields.protocol === "openai_responses" &&
    !DEEPSEEK_RESPONSES_MODELS.has(fields.model.trim().toLowerCase());
  const allowedProtocolOptions = allowedProtocols(activePreset, fields.base_url);
  const protocolMismatch = Boolean(
    allowedProtocolOptions && !allowedProtocolOptions.includes(fields.protocol)
  );
  const webGuidance = providerWebGuidance(
    activePreset,
    fields.base_url,
    fields.protocol,
    fields.model,
    hostedWebRoutes
  );
  const advancedMustOpen =
    presetName === CUSTOM_PRESET ||
    pendingVars.length > 0 ||
    protocolMismatch ||
    deepSeekResponsesModelUnsupported ||
    outputExceedsDeepSeekLimit;
  // 占位符没替换就保存 = 一个必然 404 的地址进了配置
  const saveBlocked =
    busy
    || protocolMismatch
    || outputExceedsDeepSeekLimit
    || deepSeekResponsesModelUnsupported
    || pendingVars.length > 0;

  return (
    <section className="settings-block provider-settings">
      <div className="section-heading">
        <div>
          <h3>对话模型</h3>
          <p className="desc">R-Code 对话使用的模型服务。访问密钥只保存在当前设备的安全凭据存储中，界面不会回显已保存内容。</p>
        </div>
        <button
          className="btn"
          disabled={busy}
          onClick={startNewProvider}
        >
          新建服务
        </button>
      </div>

      {err && <div className="errbar" role="alert">{err}</div>}
      {saved && <div className="okbar" role="status">{saved}</div>}

      <div className="provider-layout">
        <div className="provider-list" aria-label="已保存的模型服务">
          <div className="provider-list-label">已保存的服务</div>
          {drafting && (
            <button
              className="provider-row provider-row-draft selected"
              type="button"
              disabled={busy}
            >
              <span className="provider-row-title">未保存的新服务</span>
              <span className="provider-row-model">正在填写，保存后生效</span>
              <span className="provider-row-state">草稿</span>
            </button>
          )}
          {names.length === 0 && !drafting ? (
            <div className="provider-empty">还没有服务。选择一个预设，填入密钥即可开始聊天。</div>
          ) : (
            names.map((name) => {
              const profile = providers[name] as ProviderConfig;
              const active = name === configDefault;
              const status = providerStatus[name];
              return (
                <button
                  key={name}
                  className={`provider-row${name === selectedProvider ? " selected" : ""}`}
                  disabled={busy}
                  onClick={() => (drafting ? setPendingSelect(name) : setSelectedProvider(name))}
                >
                  <span className="provider-row-title">
                    {providerLabel(name)}
                    {active && <em>正在使用</em>}
                  </span>
                  <span className="provider-row-model">{profile.model || "尚未设置模型"}</span>
                  <span className={`provider-row-state${status?.ready ? " ready" : ""}`}>{providerStateLabel(status)}</span>
                </button>
              );
            })
          )}
        </div>

        <div className="provider-editor">
          <div className="provider-editor-head">
            <div>
              <span className="provider-editor-kicker">{editing ? "编辑服务" : "新建服务"}</span>
              <h4>{editing ? providerLabel(selectedProvider ?? "") : "添加一个模型服务"}</h4>
            </div>
            {editing && selectedProvider && selectedProvider !== configDefault && (
              <button className="quiet-link danger-link" disabled={busy} onClick={() => deleteProvider(selectedProvider)}>
                删除
              </button>
            )}
          </div>

          <div className="provider-form">
            <div className="provider-form-grid">
              <div className="provider-form-field">
                <label htmlFor="set-preset">预设</label>
                <select id="set-preset"
                  className="input"
                  value={presetName}
                  disabled={busy || Boolean(editing)}
                  onChange={(event) => applyPreset(event.target.value)}
                >
                  {groupByCategory(catalog).map(([category, presets]) => (
                    <optgroup key={category} label={CATEGORY_LABELS[category] ?? category}>
                      {presets.map((preset) => (
                        <option key={preset.id} value={preset.id}>{preset.label}</option>
                      ))}
                    </optgroup>
                  ))}
                  <option value={CUSTOM_PRESET}>自建 / 其它 OpenAI 兼容接口</option>
                </select>
                {activePreset && (
                  <span className="provider-field-meta">
                    {PROTOCOL_LABELS[activePreset.protocol]}
                    {activePreset.context_window != null &&
                      ` · ${activePreset.context_window.toLocaleString()} 上下文`}
                    {activePreset.api_key_url && (
                      <>
                        {" · "}
                        <a href={activePreset.api_key_url} target="_blank" rel="noreferrer">获取密钥</a>
                      </>
                    )}
                  </span>
                )}
                {editing && <span className="provider-field-meta">更换预设请新建服务</span>}
              </div>

              <div className="provider-form-field">
                <label htmlFor="set-profile-name">配置名称</label>
                <input id="set-profile-name"
                  className="input"
                  value={profileName}
                  readOnly={Boolean(editing)}
                  placeholder="例如：DeepSeek 工作账户"
                  onChange={(event) => setProfileName(event.target.value)}
                />
              </div>

              <div className="provider-form-field provider-form-field-wide">
                <div className="provider-field-label-row">
                  <label htmlFor="set-api-key">访问密钥</label>
                  <span className={credential?.configured ? "saved" : undefined}>{credentialLabel}</span>
                </div>
                <input id="set-api-key"
                  className="input"
                  type="password"
                  autoComplete="off"
                  placeholder={credential?.configured ? "留空则保留当前密钥" : "粘贴访问密钥"}
                  value={keyInput}
                  onChange={(event) => setKeyInput(event.target.value)}
                />
              </div>

              <div className="provider-form-field provider-form-field-wide">
                <label htmlFor="set-model">模型</label>
                <div className="provider-model-input">
                  {/* 输入框保留自由输入；候选列表改用 Menu 弹层——原生 datalist 只会显示与当前值前缀匹配的选项，预填默认模型后其余候选会被过滤掉。 */}
                  <input id="set-model"
                    className="provider-model-text"
                    value={fields.model}
                    placeholder="输入或同步模型名称"
                    onChange={(event) => setFields((value) => ({ ...value, model: event.target.value }))}
                  />
                  <div className="provider-model-actions">
                    <Menu
                      trigger={
                        <button
                          className="provider-model-options"
                          type="button"
                          disabled={busy || modelChoices.length === 0}
                          title={modelChoices.length === 0 ? "暂无可选模型，先同步模型列表" : "查看候选模型"}
                          aria-label="查看候选模型"
                        >
                          <IconChevronDown width={13} height={13} />
                        </button>
                      }
                      label="候选模型"
                      placement="down"
                      align="right"
                      scroll
                      menuClassName="model-menu"
                    >
                      {({ close }) => (
                        <>
                          <div className="popover-head">
                            <strong>候选模型</strong>
                            <span>预设候选与已同步模型</span>
                          </div>
                          {modelChoices.length === 0 ? (
                            <MenuEmpty>没有候选模型，先同步模型列表或手动输入</MenuEmpty>
                          ) : (
                            modelChoices.map((modelName) => (
                              <MenuItem
                                key={modelName}
                                close={close}
                                checked={fields.model.trim() === modelName}
                                onSelect={() => {
                                  setFields((value) => ({ ...value, model: modelName }));
                                  rememberModel(profileName.trim() || activePreset?.id || "", modelName);
                                }}
                              >
                                <span className="model-name" title={modelName}>{modelName}</span>
                              </MenuItem>
                            ))
                          )}
                        </>
                      )}
                    </Menu>
                    <button
                      className={`provider-model-refresh${modelsBusy ? " loading" : ""}`}
                      type="button"
                      disabled={busy || modelsBusy || pendingVars.length > 0 || !fields.base_url.trim()}
                      title="从当前接口同步模型列表"
                      onClick={() => void fetchModels()}
                    >
                      <IconRefresh width={15} height={15} />
                      {modelsBusy ? "同步中" : "同步模型"}
                    </button>
                  </div>
                </div>
                {modelsMessage && <span className="provider-field-success" role="status">{modelsMessage}</span>}
                {modelsError && <span className="provider-field-warning" role="alert">{modelsError}</span>}
              </div>

              <label className="provider-preference-row provider-form-field-wide" htmlFor="set-show-reasoning">
                <span>
                  <strong>显示思考过程</strong>
                  <small>展示模型服务明确返回的思考内容或摘要；关闭只隐藏展示，不会改变模型自身推理。</small>
                </span>
                <input
                  id="set-show-reasoning"
                  className="switch"
                  type="checkbox"
                  role="switch"
                  checked={fields.show_reasoning}
                  disabled={busy}
                  onChange={(event) =>
                    setFields((value) => ({ ...value, show_reasoning: event.target.checked }))
                  }
                />
              </label>
            </div>

            <aside
              className={`provider-search-capability is-${webGuidance.state}`}
              aria-label="当前模型服务的联网能力"
              aria-live="polite"
              data-search-state={webGuidance.state}
            >
              <span className="provider-search-capability-icon" aria-hidden="true">
                <IconSearch width={15} height={15} />
              </span>
              <div className="provider-search-capability-copy">
                <div className="provider-search-capability-head">
                  <strong>联网能力</strong>
                  <span>{webGuidance.badge}</span>
                </div>
                <p>{webGuidance.description}</p>
                {webGuidance.docsUrl && webGuidance.docsLabel && (
                  <a href={webGuidance.docsUrl} target="_blank" rel="noreferrer">
                    {webGuidance.docsLabel}
                  </a>
                )}
              </div>
            </aside>

            <details className="provider-advanced" open={advancedMustOpen || undefined}>
              <summary>
                <span>高级设置</span>
                <small>{PROTOCOL_LABELS[fields.protocol]} · 最大输出 {fields.max_tokens || "默认"}</small>
              </summary>
              <div className="provider-advanced-grid">
                {activePreset?.note && (
                  <p className="provider-advanced-note">{activePreset.note}</p>
                )}

                <div className="provider-form-field provider-form-field-wide">
                  <label htmlFor="set-base-url">接口地址</label>
                  <input id="set-base-url"
                    className="input"
                    value={fields.base_url}
                    placeholder="https://api.example.com/v1"
                    onChange={(event) => setFields((value) => ({ ...value, base_url: event.target.value }))}
                  />
                  {!activePreset && (
                    <span className="provider-field-meta">填写服务根地址，不含 /chat/completions</span>
                  )}
                  {pendingVars.length > 0 && (
                    <span className="provider-field-warning" role="alert">
                      地址里还有占位符待替换：
                      {pendingVars.map((variable) => `\${${variable.name}}（${variable.label}）`).join("、")}
                    </span>
                  )}
                  {activePreset && activePreset.endpoint_candidates.length > 0 && (
                    <span className="provider-route-switcher">
                      <span>接口线路</span>
                      <button
                        className="quiet-link"
                        type="button"
                        disabled={busy}
                        title={`${activePreset.base_url}（${PROTOCOL_LABELS[activePreset.protocol]}）`}
                        onClick={() =>
                          setFields((value) => ({
                            ...value,
                            base_url: activePreset.base_url,
                            protocol: activePreset.protocol,
                          }))
                        }
                      >
                        主入口
                      </button>
                      {activePreset.endpoint_candidates.map((candidate) => (
                        <Fragment key={candidate.url}>
                          <span aria-hidden="true">·</span>
                          <button
                            className="quiet-link"
                            type="button"
                            disabled={busy}
                            title={`${candidate.url}（${PROTOCOL_LABELS[candidate.protocol]}）`}
                            // 协议必须跟着地址一起切：多数备用线路是同一厂商的另一个协议口，
                            // 只改地址会把 Anthropic 的请求发到一个只有 Chat 的 endpoint 上。
                            onClick={() =>
                              setFields((value) => ({
                                ...value,
                                base_url: candidate.url,
                                protocol: candidate.protocol,
                              }))
                            }
                          >
                            {candidate.label}
                          </button>
                        </Fragment>
                      ))}
                    </span>
                  )}
                </div>

                <div className="provider-form-field provider-form-field-wide">
                  <label htmlFor="set-protocol">线路协议</label>
                  <select id="set-protocol"
                    className="input"
                    disabled={busy}
                    value={fields.protocol}
                    onChange={(event) =>
                      setFields((value) => ({
                        ...value,
                        protocol: event.target.value as ProviderProtocol,
                      }))
                    }
                  >
                    {protocolChoices(activePreset, fields.base_url, fields.protocol).map((protocol) => (
                      <option key={protocol} value={protocol}>{PROTOCOL_LABELS[protocol]}</option>
                    ))}
                  </select>
                  {deepSeekResponsesModelUnsupported && (
                    <span className="provider-field-warning" role="alert">
                      Responses 支持 deepseek-v4-flash（0731）与 deepseek-v4-pro；请使用以上模型。
                    </span>
                  )}
                  {fields.protocol === "openai_responses" && !deepSeekResponsesModelUnsupported && (
                    <span className="provider-field-meta">
                      {activePreset && !activePreset.reasoning_replay
                        ? "该服务不支持加密推理回放"
                        : "由 Responses 接口发送请求"}
                    </span>
                  )}
                  {!allowedProtocolOptions && activePreset && (
                    <span className="provider-field-meta">自定义地址，请按接口实现选择协议</span>
                  )}
                  {protocolMismatch && allowedProtocolOptions && (
                    <span className="provider-field-warning" role="alert">
                      当前地址不支持该协议。可选：
                      {allowedProtocolOptions.map((protocol) => PROTOCOL_LABELS[protocol]).join(" / ")}
                    </span>
                  )}
                </div>

                <div className="provider-form-field">
                  <label htmlFor="set-max-tokens">最大输出</label>
                  <input id="set-max-tokens"
                    className="input"
                    inputMode="numeric"
                    value={fields.max_tokens}
                    onChange={(event) => setFields((value) => ({ ...value, max_tokens: event.target.value }))}
                  />
                  <span className="provider-field-meta">
                    {deepSeekV4 ? "V4 最大 393,216，建议 8,192" : "通常建议 8,192"}
                  </span>
                  {outputExceedsDeepSeekLimit && (
                    <span className="provider-field-warning" role="alert">
                      当前值超出限制，请
                      <button className="quiet-link" type="button" onClick={() => setFields((value) => ({ ...value, max_tokens: OUTPUT_DEFAULT }))}>
                        恢复为 8,192
                      </button>
                    </span>
                  )}
                </div>

                <div className="provider-form-field">
                  <label htmlFor="set-temperature">随机性</label>
                  <input id="set-temperature"
                    className="input"
                    inputMode="decimal"
                    value={fields.temperature}
                    onChange={(event) => setFields((value) => ({ ...value, temperature: event.target.value }))}
                  />
                  <span className="provider-field-meta">编码任务建议 0.1–0.3</span>
                </div>
              </div>
            </details>
          </div>

          <div className="footbar provider-actions">
            {editing && selectedProvider && selectedProvider !== configDefault && (
              <button className="btn" disabled={busy || !providerStatus[selectedProvider]?.ready} onClick={() => selectProvider(selectedProvider)}>
                用于新对话
              </button>
            )}
            {drafting && (
              <button className="btn" disabled={busy} onClick={cancelDraft}>取消</button>
            )}
            <span className="spacer" />
            <button className="btn accent" disabled={saveBlocked} onClick={() => saveProvider(true)}>保存并用于新对话</button>
          </div>
        </div>
      </div>
      <ConfirmDialog
        open={pendingSelect != null}
        title="放弃未保存的更改？"
        description="你正在新建服务，切换后会丢弃尚未保存的配置。"
        confirmLabel="放弃更改"
        onConfirm={commitPendingSelect}
        onCancel={() => setPendingSelect(null)}
      />
    </section>
  );
}

// ---------- 通用 ----------

const DEFAULT_ORCHESTRATION: OrchestrationConfig = {
  default_agent_engine: "r_code",
  delegation_router: "balanced",
  allow_cross_engine_delegation: true,
  quality_loop: "off",
  quality_reviewer: "r_code",
  max_review_rounds: 1,
};

function OrchestrationSection({ config, reload }: { config: AppConfig; reload: () => Promise<void> }) {
  const policy = config.orchestration ?? DEFAULT_ORCHESTRATION;
  const [busy, setBusy] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);

  const save = async (field: keyof OrchestrationConfig, value: unknown) => {
    setBusy(field);
    setErr(null);
    try {
      await settingsSet(`orchestration.${field}`, value);
      await reload();
    } catch (cause) {
      setErr(errText(cause));
    } finally {
      setBusy(null);
    }
  };

  const skills = [
    {
      title: "任务拆解与路由",
      state: policy.delegation_router === "manual" ? "手动" : "已开启",
      detail: policy.delegation_router === "balanced"
        ? "简单任务留给 R-Code；复杂任务优先 Codex，不可用时会显示回退原因。"
        : "每次委派会记录执行器选择和路由原因，并显示在子智能体详情中。",
    },
    {
      title: "Codex 子代理",
      state: policy.allow_cross_engine_delegation ? "可调用" : "已关闭",
      detail: "子智能体默认只读；只有父任务明确授权时才能提升到完整访问，项目审批策略仍然生效。",
    },
    {
      title: "质量复核循环",
      state: policy.quality_loop === "off" ? "已关闭" : `${policy.max_review_rounds} 轮上限`,
      detail: policy.quality_loop === "off"
        ? "主结果直接交付，不启动额外复核。"
        : "R-Code 主 Agent 完成后由宿主启动可见复核；需要修订时再进入下一轮。Codex 主 Agent 保留其自身执行循环。",
    },
  ];

  return (
    <>
      <section className="settings-block">
        <h3>主 Agent</h3>
        <p className="desc">默认值只影响新会话。每个会话都能在输入区单独切换，运行中不会静默换引擎。</p>
        {err && <div className="errbar" role="alert">保存编排策略失败：{err}</div>}
        <div className="field">
          <label htmlFor="set-default-agent">新会话默认</label>
          <select
            id="set-default-agent"
            className="input"
            value={policy.default_agent_engine}
            disabled={busy != null}
            onChange={(event) => void save("default_agent_engine", event.target.value)}
          >
            <option value="r_code">R-Code · 自定义 Provider</option>
            <option value="codex">Codex CLI · 本机登录</option>
          </select>
          <span className="hint">Codex 主 Agent 需要已登录 CLI 和已附加工作区。</span>
        </div>
      </section>

      <section className="settings-block">
        <h3>委派路由</h3>
        <div className="field">
          <label htmlFor="set-delegation-router">复杂度策略</label>
          <select
            id="set-delegation-router"
            className="input"
            value={policy.delegation_router}
            disabled={busy != null}
            onChange={(event) => void save("delegation_router", event.target.value)}
          >
            <option value="balanced">均衡 · 复杂任务优先 Codex</option>
            <option value="r_code_first">R-Code 优先</option>
            <option value="codex_first">Codex 优先</option>
            <option value="manual">仅显式选择</option>
          </select>
        </div>
        <div className="field">
          <label htmlFor="set-cross-agent">允许 Codex 子代理</label>
          <input
            id="set-cross-agent"
            className="switch"
            type="checkbox"
            role="switch"
            checked={policy.allow_cross_engine_delegation}
            disabled={busy != null}
            onChange={(event) => void save("allow_cross_engine_delegation", event.target.checked)}
          />
          <span className="hint">关闭后新的 Codex 委派会自动回退 R-Code；已经启动的 Codex 子代理继续完成。</span>
        </div>
      </section>

      <section className="settings-block">
        <h3>质量复核</h3>
        <p className="desc">默认关闭以避免额外延迟和模型消耗；开启后，运行阶段、复核者和轮次都会明确显示。</p>
        <div className="field">
          <label htmlFor="set-quality-loop">触发方式</label>
          <select
            id="set-quality-loop"
            className="input"
            value={policy.quality_loop}
            disabled={busy != null}
            onChange={(event) => void save("quality_loop", event.target.value)}
          >
            <option value="off">关闭</option>
            <option value="auto">自动 · 仅工具型任务</option>
            <option value="always">始终复核</option>
          </select>
        </div>
        <div className="field">
          <label htmlFor="set-quality-reviewer">复核者</label>
          <select
            id="set-quality-reviewer"
            className="input"
            value={policy.quality_reviewer}
            disabled={busy != null || policy.quality_loop === "off"}
            onChange={(event) => void save("quality_reviewer", event.target.value)}
          >
            <option value="auto">自动交叉复核</option>
            <option value="r_code">R-Code</option>
            <option value="codex">Codex</option>
          </select>
        </div>
        <div className="field">
          <label htmlFor="set-review-rounds">修订上限</label>
          <select
            id="set-review-rounds"
            className="input"
            value={policy.max_review_rounds}
            disabled={busy != null || policy.quality_loop === "off"}
            onChange={(event) => void save("max_review_rounds", Number(event.target.value))}
          >
            <option value={1}>1 轮</option>
            <option value={2}>2 轮</option>
            <option value={3}>3 轮</option>
          </select>
        </div>
      </section>

      <section className="settings-block">
        <h3>内置编排能力</h3>
        <div className="orchestration-cards">
          {skills.map((skill) => (
            <article className="orchestration-card" key={skill.title}>
              <div><strong>{skill.title}</strong><span>{skill.state}</span></div>
              <p>{skill.detail}</p>
            </article>
          ))}
        </div>
      </section>
    </>
  );
}



function LogLevelSection({ config, reload }: { config: AppConfig; reload: () => Promise<void> }) {
  const [err, setErr] = useState<string | null>(null);

  const setLevel = async (v: string) => {
    setErr(null);
    try {
      await settingsSet("log_level", v);
      await reload();
    } catch (e) {
      setErr(errText(e));
    }
  };

  return (
    <section className="settings-block">
      <h3>日志记录</h3>
      {err && <div className="errbar" role="alert">{err}</div>}
      <div className="field">
        <label htmlFor="set-log-level">记录级别</label>
        <select id="set-log-level"
          className="input"
          value={config.log_level ?? "info"}
          onChange={(e) => void setLevel(e.target.value)}
        >
          {LOG_LEVELS.map((l) => (
            <option key={l} value={l}>
              {l}
            </option>
          ))}
        </select>
      </div>
    </section>
  );
}

// ---------- 外观 ----------

function AppearanceSection() {
  const themeMode = useAppStore((s) => s.themeMode);
  const setThemeMode = useAppStore((s) => s.setThemeMode);

  const modes: { key: "light" | "dark" | "system"; label: string; hint: string }[] = [
    { key: "light", label: "亮色", hint: "清晰明快" },
    { key: "dark", label: "暗色", hint: "沉浸专注" },
    { key: "system", label: "跟随系统", hint: "自动切换" },
  ];

  return (
    <section className="preference-section preference-appearance" aria-labelledby="appearance-heading">
      <div className="preference-section-heading">
        <div>
          <h3 id="appearance-heading">界面主题</h3>
          <p>让工作区保持舒适的明暗关系，其他视觉细节沿用系统设计。</p>
        </div>
      </div>
      <div className="theme-options" role="radiogroup" aria-label="界面主题">
        {modes.map((mode) => {
          const selected = themeMode === mode.key;
          return (
            <button
              key={mode.key}
              type="button"
              role="radio"
              aria-checked={selected}
              className={`theme-option is-${mode.key}${selected ? " is-selected" : ""}`}
              onClick={() => setThemeMode(mode.key)}
            >
              <span className="theme-option-preview" aria-hidden="true"><i /><i /></span>
              <span className="theme-option-copy">
                <strong>{mode.label}</strong>
                <small>{mode.hint}</small>
              </span>
              <span className="theme-option-check" aria-hidden="true">
                {selected && <IconCheck width={15} height={15} />}
              </span>
            </button>
          );
        })}
      </div>
    </section>
  );
}

function CompanionSection() {
  const enabled = useCompanionStore((state) => state.enabled);
  const minimized = useCompanionStore((state) => state.minimized);
  const soundEnabled = useCompanionStore((state) => state.soundEnabled);
  const motion = useCompanionStore((state) => state.motion);
  const setEnabled = useCompanionStore((state) => state.setEnabled);
  const setMinimized = useCompanionStore((state) => state.setMinimized);
  const setSoundEnabled = useCompanionStore((state) => state.setSoundEnabled);
  const setMotion = useCompanionStore((state) => state.setMotion);
  const resetPosition = useCompanionStore((state) => state.resetPosition);
  const [windowBusy, setWindowBusy] = useState(false);

  const setCompanionVisibility = async (next: boolean) => {
    if (!next) {
      setEnabled(false);
      return;
    }
    setWindowBusy(true);
    try {
      if (!await companionEnsure()) throw new Error("companion window was not created");
      setMinimized(false);
      setEnabled(true);
    } catch (cause) {
      console.warn("Companion window could not be enabled.", cause);
      setEnabled(false);
      pushToast({
        kind: "error",
        title: "无法开启小助手",
        body: "小助手窗口暂时不可用，请稍后重试。若问题持续，可前往“诊断”查看后台记录。",
        timeout: 4_000,
      });
    } finally {
      setWindowBusy(false);
    }
  };

  return (
    <section className="preference-section preference-companion" aria-labelledby="companion-heading">
      <header className="companion-preference-head">
        <div>
          <h3 id="companion-heading">R-Code 初音小助手</h3>
          <p>悬浮显示任务运行、等待与完成状态。数据只来自本机，不会额外调用模型。</p>
        </div>
        <label className="companion-master-control" htmlFor="set-companion-enabled">
          <span>{windowBusy ? "开启中…" : enabled ? "已开启" : "已关闭"}</span>
          <input
            id="set-companion-enabled"
            className="switch preference-master-switch"
            type="checkbox"
            role="switch"
            aria-label="显示小助手"
            checked={enabled}
            disabled={windowBusy}
            onChange={(event) => void setCompanionVisibility(event.target.checked)}
          />
        </label>
      </header>

      <div className={`preference-rows${enabled ? "" : " is-disabled"}`}>
        <div className="preference-row">
          <div className="preference-row-copy">
            <strong id="set-companion-shape-label">默认形态</strong>
            <span>完整形态更易看清反馈，迷你形态减少桌面遮挡。</span>
          </div>
          <div className="preference-segmented" role="radiogroup" aria-labelledby="set-companion-shape-label">
          <button
            type="button"
            role="radio"
            aria-checked={!minimized}
            className={`chipbtn${!minimized ? " on" : ""}`}
            disabled={!enabled}
            onClick={() => setMinimized(false)}
          >
            完整形态
          </button>
          <button
            type="button"
            role="radio"
            aria-checked={minimized}
            className={`chipbtn${minimized ? " on" : ""}`}
            disabled={!enabled}
            onClick={() => setMinimized(true)}
          >
            迷你形态
          </button>
        </div>
        </div>

        <div className="preference-row">
          <div className="preference-row-copy">
            <strong>悬浮位置</strong>
            <span>拖动角色可自由移动，一键恢复到主显示器右下角。</span>
          </div>
          <button className="preference-inline-action" type="button" disabled={!enabled} onClick={resetPosition}>
            <IconRefresh width={15} height={15} />
            恢复右下角
          </button>
        </div>

        <div className="preference-row">
          <div className="preference-row-copy">
            <label htmlFor="set-companion-motion">动效</label>
            <span>控制小助手的状态动作与移入反馈。</span>
          </div>
          <select
            id="set-companion-motion"
            className="input preference-select"
            value={motion}
            disabled={!enabled}
            onChange={(event) => setMotion(event.target.value as CompanionMotion)}
          >
            <option value="system">跟随系统</option>
            <option value="full">完整动效</option>
            <option value="reduced">静态形态</option>
          </select>
        </div>

        <div className="preference-row">
          <div className="preference-row-copy">
            <label htmlFor="set-companion-sound">状态提示音</label>
            <span>仅在授权、完成、失败或待审核等重要变化时提示一次。</span>
          </div>
          <label className="preference-switch-control" htmlFor="set-companion-sound">
            <span>{soundEnabled ? "已开启" : "已关闭"}</span>
            <input
              id="set-companion-sound"
              className="switch"
              type="checkbox"
              role="switch"
              checked={soundEnabled}
              disabled={!enabled}
              onChange={(event) => setSoundEnabled(event.target.checked)}
            />
          </label>
        </div>
      </div>

      <footer className="companion-usage-note" aria-label="小助手操作说明">
        <span>拖动移动</span>
        <span>左键查看任务</span>
        <span>右键关闭</span>
      </footer>
    </section>
  );
}

// ---------- 日志查看器 ----------

function LogSection() {
  const [filter, setFilter] = useState<(typeof LOG_FILTERS)[number]>("all");
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [err, setErr] = useState<string | null>(null);
  const boxRef = useRef<HTMLDivElement>(null);

  usePoll(async () => {
    try {
      setLogs(await logsTail(200, filter === "all" ? undefined : filter));
      setErr(null);
    } catch (e) {
      setErr(errText(e));
    }
  }, 1500);

  // 仅当用户停留在底部附近时才跟随最新日志
  useEffect(() => {
    const el = boxRef.current;
    if (!el) return;
    if (el.scrollHeight - el.scrollTop - el.clientHeight < 48) {
      el.scrollTop = el.scrollHeight;
    }
  }, [logs]);

  return (
    <section className="settings-block">
      <h3>诊断日志</h3>
      <p className="desc">当前进程与近期历史会实时汇合；日志按日滚动，固定保留最近 7 天。</p>
      <div className="field">
        <div className="chips" role="radiogroup" aria-label="日志级别过滤">
          {LOG_FILTERS.map((l) => (
            <button
              key={l}
              role="radio"
              aria-checked={filter === l}
              className={`chipbtn${filter === l ? " on" : ""}`}
              onClick={() => setFilter(l)}
            >
              {l === "all" ? "全部" : l}
            </button>
          ))}
        </div>
      </div>
      {err && <div className="errbar" role="alert">{err}</div>}
      <div className="logbox" role="log" aria-live="off" ref={boxRef}>
        {logs.length === 0 ? (
          <div className="empty">暂无日志</div>
        ) : (
          logs.map((l, i) => (
            <div className="logline" key={i}>
              <span className="t">{clockTime(l.timestamp)}</span>
              <span className={`lv ${l.level.toLowerCase()}`}>{l.level}</span>
              <span className="tg">{l.target}</span>
              <span className="msg">{l.message}</span>
            </div>
          ))
        )}
      </div>
    </section>
  );
}

// ---------- 支持包 ----------

function SupportSection() {
  const [preview, setPreview] = useState<SupportBundlePreview | null>(null);
  const [bundlePath, setBundlePath] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  const doPreview = async () => {
    setBusy(true);
    setErr(null);
    try {
      setPreview(await supportPreview());
    } catch (e) {
      setErr(`预览失败：${errText(e)}`);
    } finally {
      setBusy(false);
    }
  };

  const doExport = async () => {
    setBusy(true);
    setErr(null);
    setBundlePath(null);
    try {
      const path = await supportBundleChoose();
      if (path) setBundlePath(path);
    } catch (e) {
      setErr(`导出失败：${errText(e)}`);
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="settings-block">
      <h3>支持包</h3>
      <p className="desc">导出近 7 天脱敏后的 warning/error 明细、版本、平台和本地统计；预览不会写入文件。</p>
      {err && <div className="errbar" role="alert">{err}</div>}
      <div className="footbar">
        <button className="btn" disabled={busy} onClick={() => void doPreview()}>
          生成预览
        </button>
      </div>
      {preview && (
        <dl className="kv">
          <dt>版本</dt>
          <dd>{preview.version}</dd>
          <dt>平台</dt>
          <dd>{preview.platform}</dd>
          <dt>生成时间</dt>
          <dd>{preview.generated_at}</dd>
          <dt>警告/错误条数</dt>
          <dd>{preview.logs.length}</dd>
          <dt>本地统计</dt>
          <dd>
            任务 {preview.db_stats.task_count}，运行 {preview.db_stats.run_count}，工具调用{" "}
            {preview.db_stats.tool_call_count}
          </dd>
        </dl>
      )}
      <div className="footbar">
        <button className="btn accent" disabled={busy} onClick={() => void doExport()}>
          选择目录并导出
        </button>
      </div>
      {bundlePath && (
        <div className="okbar" role="status">
          已生成：<span className="val">{bundlePath}</span>
        </div>
      )}
    </section>
  );
}

// ---------- 外部 Agent ----------

type CodexSetupState = NonNullable<CodexIntegrationStatus["setup_state"]>;

function resolveCodexSetupState(status: CodexIntegrationStatus): CodexSetupState {
  if (status.setup_state) return status.setup_state;
  if (!status.cli_available) return "install_cli";
  if (status.auth_status === "not_authenticated") return "login";
  if (status.auth_status !== "authenticated") return "check";
  if (status.skill_status !== "up_to_date" || !status.mcp_server_configured) return "configure";
  return "ready";
}

function codexSetupCopy(status: CodexIntegrationStatus | null, state: CodexSetupState | "loading") {
  if (!status || state === "loading") {
    return { title: "正在检测 Codex", detail: "检查 CLI、登录和协作配置。", action: "正在检测…" };
  }
  if (state === "install_cli") {
    return {
      title: "还需要安装 Codex CLI",
      detail: status.installer_available === false
        ? "当前没有可用的 npm，请按下方说明手动安装。"
        : "R-Code 会先展示官方安装命令，确认后再执行。",
      action: status.installer_available === false ? "无法自动安装" : "安装并继续",
    };
  }
  if (state === "login") {
    return { title: "还需要登录 Codex", detail: "使用浏览器登录；设备码仅在浏览器回调不可用时使用。", action: "登录并继续" };
  }
  if (state === "check") {
    return { title: "暂时无法确认登录状态", detail: "不会重复打开登录页，先重新读取 Codex 的认证状态。", action: "重新检测" };
  }
  if (state === "configure") {
    return { title: "还差最后一步", detail: "一次更新协作 Skill，并补齐 R-Code 的 Codex MCP 配置。", action: "完成协作配置" };
  }
  return {
    title: "Codex 已就绪",
    detail: `已通过${status.auth_method ? ` ${status.auth_method}` : " Codex"} 登录，R-Code 协作已连接。`,
    action: "已就绪",
  };
}

type CodexPreferenceDraft = {
  model: string;
  reasoningEffort: string;
  verbosity: string;
  permissionMode: string;
};

const REASONING_LABELS: Record<string, string> = {
  minimal: "最少",
  low: "低",
  medium: "中等",
  high: "高",
  xhigh: "极高",
  max: "最大",
  ultra: "超强",
};

function codexPreferenceDraft(preferences: CodexCliPreferences): CodexPreferenceDraft {
  return {
    model: preferences.model ?? "",
    reasoningEffort: preferences.reasoning_effort ?? "",
    verbosity: preferences.verbosity ?? "",
    permissionMode: preferences.permission_mode ?? "read_only",
  };
}

function sameCodexPreference(
  left: CodexPreferenceDraft,
  right: CodexPreferenceDraft
) {
  return left.model === right.model
    && left.reasoningEffort === right.reasoningEffort
    && left.verbosity === right.verbosity
    && left.permissionMode === right.permissionMode;
}

function uniqueReasoningOptions(models: CodexModelOption[]) {
  const seen = new Set<string>();
  return models.flatMap((model) => model.supported_reasoning_efforts).filter((option) => {
    if (seen.has(option.effort)) return false;
    seen.add(option.effort);
    return true;
  });
}

function CodexRuntimePreferences({
  codexDelegationEnabled,
  reloadConfig,
}: {
  codexDelegationEnabled: boolean | null;
  reloadConfig: () => Promise<void>;
}) {
  const [preferences, setPreferences] = useState<CodexCliPreferences | null>(null);
  const [draft, setDraft] = useState<CodexPreferenceDraft>({
    model: "",
    reasoningEffort: "",
    verbosity: "",
    permissionMode: "read_only",
  });
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [delegationSaving, setDelegationSaving] = useState(false);
  const [delegationEnabled, setDelegationEnabled] = useState(codexDelegationEnabled ?? true);
  const [err, setErr] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    if (codexDelegationEnabled != null) setDelegationEnabled(codexDelegationEnabled);
  }, [codexDelegationEnabled]);

  const load = useCallback(async () => {
    setLoading(true);
    setErr(null);
    try {
      const next = await codexCliPreferences();
      setPreferences(next);
      setDraft(codexPreferenceDraft(next));
    } catch (e) {
      setErr(errText(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const selectedModel = preferences?.models.find((model) => model.slug === draft.model);
  const reasoningOptions = selectedModel
    ? selectedModel.supported_reasoning_efforts
    : uniqueReasoningOptions(preferences?.models ?? []);
  const reasoningValues = new Set(reasoningOptions.map((option) => option.effort));
  const displayedReasoningOptions = draft.reasoningEffort && !reasoningValues.has(draft.reasoningEffort)
    ? [{ effort: draft.reasoningEffort, description: "当前配置" }, ...reasoningOptions]
    : reasoningOptions;
  const savedDraft = preferences ? codexPreferenceDraft(preferences) : null;
  const dirty = savedDraft ? !sameCodexPreference(savedDraft, draft) : false;

  const changeModel = (model: string) => {
    const nextModel = preferences?.models.find((item) => item.slug === model);
    setDraft((current) => ({
      ...current,
      model,
      reasoningEffort:
        current.reasoningEffort
        && nextModel
        && !nextModel.supported_reasoning_efforts.some((option) => option.effort === current.reasoningEffort)
          ? ""
          : current.reasoningEffort,
    }));
    setNotice(null);
  };

  const save = async () => {
    if (!dirty || saving) return;
    setSaving(true);
    setErr(null);
    setNotice(null);
    try {
      const next = await codexSaveCliPreferences(
        draft.model,
        draft.reasoningEffort,
        draft.verbosity,
        draft.permissionMode,
      );
      setPreferences(next);
      setDraft(codexPreferenceDraft(next));
      setNotice("运行偏好已保存。");
    } catch (e) {
      setErr(errText(e));
    } finally {
      setSaving(false);
    }
  };

  const toggleCodexDelegation = async (enabled: boolean) => {
    if (delegationSaving || codexDelegationEnabled == null) return;
    const previous = delegationEnabled;
    setDelegationEnabled(enabled);
    setDelegationSaving(true);
    setErr(null);
    setNotice(null);
    try {
      await settingsSet("orchestration.allow_cross_engine_delegation", enabled);
      await reloadConfig();
      setNotice(enabled
        ? "Codex 子代理已开启；之后的新委派可以使用 Codex。"
        : "Codex 子代理已关闭；之后的新委派会自动改用 R-Code。");
    } catch (e) {
      setDelegationEnabled(previous);
      setErr(errText(e));
    } finally {
      setDelegationSaving(false);
    }
  };

  return (
    <div className="codex-runtime-preferences">
      <div className="codex-runtime-head">
        <div>
          <h4>运行偏好</h4>
          <p>保存到 Codex 的全局配置，也会用于其他 Codex CLI 会话。</p>
        </div>
        <button className="quiet-link" disabled={loading || saving} onClick={() => void load()}>
          重新读取
        </button>
      </div>

      <div className="settings-control-list codex-delegation-list">
        <label className="settings-control-row" htmlFor="codex-subagent-enabled">
          <span>
            <strong>允许 Codex 子代理</strong>
            <small>关闭后仅阻止新的 Codex 委派，并自动改用 R-Code；已启动的 Codex 子代理不会被中断。</small>
          </span>
          <input
            id="codex-subagent-enabled"
            className="switch"
            type="checkbox"
            role="switch"
            checked={delegationEnabled}
            disabled={delegationSaving || codexDelegationEnabled == null}
            onChange={(event) => void toggleCodexDelegation(event.target.checked)}
          />
        </label>
      </div>

      {loading && <div className="settings-loading">正在读取 Codex 可用模型…</div>}
      {err && (
        <div className="errbar" role="alert">
          {err}
          <span className="spacer" />
          <button className="btn sm" disabled={loading || saving} onClick={() => void load()}>
            重试
          </button>
        </div>
      )}

      {!loading && preferences && (
        <>
          <div className="settings-control-list">
            <label className="settings-control-row" htmlFor="codex-model">
              <span>
                <strong>模型</strong>
                <small>{selectedModel ? "可用列表由当前 Codex 账户与 CLI 版本提供。" : "留空时由 Codex 选择默认模型。"}</small>
              </span>
              <select
                id="codex-model"
                className="input"
                value={draft.model}
                onChange={(event) => changeModel(event.target.value)}
              >
                <option value="">Codex 默认</option>
                {draft.model && !preferences.models.some((model) => model.slug === draft.model) && (
                  <option value={draft.model}>{draft.model}（当前配置）</option>
                )}
                {preferences.models.map((model) => (
                  <option key={model.slug} value={model.slug}>{model.display_name}</option>
                ))}
              </select>
            </label>

            <label className="settings-control-row" htmlFor="codex-reasoning">
              <span>
                <strong>思考强度</strong>
                <small>{selectedModel ? `该模型默认：${REASONING_LABELS[selectedModel.default_reasoning_effort] ?? selectedModel.default_reasoning_effort}` : "留空时跟随所用模型的默认值。"}</small>
              </span>
              <select
                id="codex-reasoning"
                className="input"
                value={draft.reasoningEffort}
                onChange={(event) => {
                  setDraft((current) => ({ ...current, reasoningEffort: event.target.value }));
                  setNotice(null);
                }}
              >
                <option value="">随模型默认</option>
                {displayedReasoningOptions.map((option) => (
                  <option key={option.effort} value={option.effort}>
                    {REASONING_LABELS[option.effort] ?? option.effort}
                  </option>
                ))}
              </select>
            </label>

            <label className="settings-control-row" htmlFor="codex-verbosity">
              <span>
                <strong>回复详略</strong>
                <small>控制 Codex 最终回复的展开程度，不改变代码质量要求。</small>
              </span>
              <select
                id="codex-verbosity"
                className="input"
                value={draft.verbosity}
                onChange={(event) => {
                  setDraft((current) => ({ ...current, verbosity: event.target.value }));
                  setNotice(null);
                }}
              >
                <option value="">Codex 默认</option>
                <option value="low">精简</option>
                <option value="medium">标准</option>
                <option value="high">详细</option>
              </select>
            </label>

            <label className="settings-control-row" htmlFor="codex-permission-mode">
              <span>
                <strong>子代理权限</strong>
                <small>
                  {draft.permissionMode === "request_approval"
                    ? "工作区内可编辑；需要扩大权限时由 R-Code 显示审批卡。"
                    : draft.permissionMode === "auto_review"
                      ? "工作区内可编辑；Codex 自动审查需要额外权限的动作。"
                      : draft.permissionMode === "full_access"
                        ? "不受 Codex sandbox 限制，也不会显示审批卡。"
                        : draft.permissionMode === "custom"
                          ? "检测到非预设 config.toml；请直接维护该文件，或选择一个预设覆盖。"
                          : "只允许读取工作区；这是此前 R-Code 的默认行为。"}
                </small>
              </span>
              <select
                id="codex-permission-mode"
                className="input"
                value={draft.permissionMode}
                onChange={(event) => {
                  setDraft((current) => ({ ...current, permissionMode: event.target.value }));
                  setNotice(null);
                }}
              >
                <option value="read_only">仅查看</option>
                <option value="request_approval">请求批准</option>
                <option value="auto_review">替我审批</option>
                <option value="full_access">完全访问权限</option>
                {draft.permissionMode === "custom" && (
                  <option value="custom" disabled>自定义（config.toml）</option>
                )}
              </select>
            </label>
          </div>

          <div className="codex-runtime-actions">
            {notice && <span role="status">{notice}</span>}
            <button className="btn accent" disabled={!dirty || saving} onClick={() => void save()}>
              {saving ? "正在保存…" : "应用"}
            </button>
          </div>
        </>
      )}
    </div>
  );
}

type RtkTransition = "enabling" | "disabling" | null;

function RtkIntegrationControl() {
  const [status, setStatus] = useState<RtkStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [transition, setTransition] = useState<RtkTransition>(null);
  const [optimisticEnabled, setOptimisticEnabled] = useState(false);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const next = await rtkStatus();
      setStatus(next);
      setOptimisticEnabled(next.enabled);
    } catch {
      setStatus(null);
      setOptimisticEnabled(false);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const toggle = async (enabled: boolean) => {
    if (transition || loading) return;
    const previous = status;
    setOptimisticEnabled(enabled);
    setTransition(enabled ? "enabling" : "disabling");
    try {
      const next = await rtkSetEnabled(enabled);
      setStatus(next);
      setOptimisticEnabled(next.enabled);
    } catch (error) {
      setStatus(previous);
      setOptimisticEnabled(previous?.enabled ?? false);
      if (
        enabled &&
        error instanceof IpcCommandError &&
        error.code === RTK_BLOCKED_BY_SECURITY_SOFTWARE
      ) {
        pushToast({
          kind: "warn",
          title: "RTK 被安全软件拦截",
          body: `Windows 安全中心可能隔离了 RTK 二进制。请把「${status?.bin_dir ?? "R-Code 的 bin 目录"}」加入排除项，然后重新开启。`,
          action: {
            label: "打开安全中心",
            run: () => void rtkOpenSecurityExclusions().catch(() => {}),
          },
          timeout: 0,
        });
      } else {
        pushToast({
          kind: "warn",
          title: enabled ? "RTK 未能启用" : "RTK 未能关闭",
          body: "请稍后重试；详细原因已写入诊断日志。",
          timeout: 5200,
        });
      }
    } finally {
      setTransition(null);
    }
  };

  const busy = transition !== null;
  const state = transition === "enabling"
    ? "enabling"
    : transition === "disabling"
      ? "disabling"
      : status?.enabled
        ? "enabled"
        : status?.available
          ? "available"
          : "unavailable";
  const badge = loading
    ? "正在检测"
    : transition === "enabling"
      ? status?.available ? "正在配置" : "正在安装"
      : transition === "disabling"
        ? "正在关闭"
        : status?.enabled
          ? "已启用"
          : status?.available
            ? "已安装"
            : status
              ? "未安装"
              : "暂不可用";
  const detail = loading
    ? "正在确认本机 RTK 与 R-Code 策略状态。"
    : transition === "enabling"
      ? status?.available
        ? "正在为之后创建的 Codex 会话启用命令策略。"
        : "正在从 rtk-ai/rtk 官方 Release 下载并校验适合当前系统的版本。"
      : transition === "disabling"
        ? "正在停用新会话策略；已安装的 RTK 会完整保留。"
        : status?.enabled
          ? "之后启动的 Codex 主 Agent 与子代理会优先使用 RTK；当前运行不会重启。"
          : status?.available
            ? "已检测到可用的 RTK。开启后从下一次 Codex 运行开始生效。"
            : status
              ? "开启时自动检测系统安装；若不存在，将安全安装到 R-Code 的应用数据目录。"
              : "暂时无法读取状态。可重试，详细原因会保留在诊断日志。";
  const installation = status?.available
    ? `${status.version ?? "RTK"} · ${status.managed ? "R-Code 托管" : "系统安装"}`
    : status?.platform ?? "按当前系统选择版本";

  return (
    <div className={`rtk-control state-${state}`} aria-busy={loading || busy}>
      <div className="rtk-mark" aria-hidden="true">RTK</div>
      <div className="rtk-control-copy">
        <div className="rtk-control-title">
          <strong>RTK 命令压缩</strong>
          <span className="rtk-state-badge" role="status" aria-live="polite">{badge}</span>
        </div>
        <p>{detail}</p>
        <div className="rtk-control-meta">
          <span>{installation}</span>
          <a href="https://github.com/rtk-ai/rtk" target="_blank" rel="noreferrer">官方项目</a>
          {!loading && !status && (
            <button className="quiet-link" onClick={() => void load()}>重新检测</button>
          )}
        </div>
      </div>
      <input
        id="rtk-enabled"
        className="switch rtk-switch"
        type="checkbox"
        role="switch"
        aria-label="为新 Codex 会话启用 RTK"
        checked={optimisticEnabled}
        disabled={loading || busy || !status}
        onChange={(event) => void toggle(event.target.checked)}
      />
    </div>
  );
}

function CodexIntegrationSection({
  config,
  reloadConfig,
}: {
  config: AppConfig | null;
  reloadConfig: () => Promise<void>;
}) {
  const { runWithCodexCli } = useCodexCliGate();
  const [status, setStatus] = useState<CodexIntegrationStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [checking, setChecking] = useState(true);
  const [awaitingLogin, setAwaitingLogin] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const loginStartedAtRef = useRef(0);
  const setupState: CodexSetupState | "loading" = status ? resolveCodexSetupState(status) : "loading";

  const refresh = useCallback(async (quiet = false, force = true) => {
    if (!quiet) setChecking(true);
    try {
      const next = await codexIntegrationStatus(force);
      setStatus(next);
      if (!quiet) setErr(null);
      return next;
    } catch (e) {
      if (!quiet) setErr(errText(e));
      return null;
    } finally {
      if (!quiet) setChecking(false);
    }
  }, []);

  useEffect(() => {
    // 初次进入设置复用 Home/Room 已完成的应用级探测；手动刷新和登录轮询会强制重查。
    void refresh(false, false);
  }, [refresh]);

  useEffect(() => {
    if (!awaitingLogin) return;
    let active = true;
    let timer: number | undefined;
    const startedAt = loginStartedAtRef.current || Date.now();
    loginStartedAtRef.current = startedAt;

    const finishWithTimeout = () => {
      if (!active) return;
      loginStartedAtRef.current = 0;
      setAwaitingLogin(false);
      setNotice(`${CODEX_LOGIN_WAIT_MINUTES} 分钟内未检测到登录完成。可重新检测、重新打开浏览器，或改用设备码。`);
    };
    const scheduleNext = () => {
      if (!active) return;
      const delay = nextCodexLoginPollDelay(startedAt);
      if (delay === null) {
        finishWithTimeout();
        return;
      }
      timer = window.setTimeout(() => void check(), delay);
    };
    const check = async () => {
      const next = await refresh(true);
      if (!active) return;
      if (next?.auth_status === "authenticated") {
        loginStartedAtRef.current = 0;
        setAwaitingLogin(false);
        setNotice("已确认 Codex 登录，下一步可以完成协作配置。");
        return;
      }
      scheduleNext();
    };

    scheduleNext();
    return () => {
      active = false;
      if (timer !== undefined) window.clearTimeout(timer);
    };
  }, [awaitingLogin, refresh]);

  const startLogin = async (mode: "browser" | "device") => {
    setBusy(true);
    setErr(null);
    setNotice(null);
    try {
      await runWithCodexCli({ feature: "Codex 登录" }, async () => {
        if (mode === "browser") await codexStartLogin();
        else await codexStartDeviceLogin();
        loginStartedAtRef.current = Date.now();
        setAwaitingLogin(true);
        setNotice(`等待 Codex 完成登录；R-Code 会自动检测 ${CODEX_LOGIN_WAIT_MINUTES} 分钟，不需要手动刷新。`);
      });
    } catch (e) {
      setErr(errText(e));
    } finally {
      setBusy(false);
    }
  };

  const completeSetup = async () => {
    if (setupState === "check") {
      await refresh();
      return;
    }
    if (!status || setupState === "ready") return;
    setBusy(true);
    setErr(null);
    setNotice(null);
    try {
      await runWithCodexCli({ feature: "完成 Codex 设置", requireAuth: true }, async () => {
        const next = await codexSetupCollaboration();
        setStatus(next);
      setNotice("Codex 已就绪，可以作为 R-Code 的协作代理使用。");
      });
    } catch (e) {
      setErr(errText(e));
      void refresh(true);
    } finally {
      setBusy(false);
    }
  };

  const skillLabel =
    status?.skill_status === "up_to_date"
      ? "已安装"
      : status?.skill_status === "update_available"
        ? "可以更新"
        : "尚未安装";
  const loginLabel = status?.auth_status === "authenticated"
    ? `已登录${status.auth_method ? ` · ${status.auth_method}` : ""}`
    : status?.auth_status === "not_authenticated"
      ? "尚未登录"
      : "暂时无法确认";
  const skillReady = status?.skill_status === "up_to_date";
  const authReady = status?.auth_status === "authenticated";
  const collaborationReady = Boolean(skillReady && status?.mcp_server_configured);
  const copy = codexSetupCopy(status, setupState);
  const mainDisabled = busy
    || checking
    || awaitingLogin
    || !status
    || setupState === "ready"
    || (setupState === "install_cli" && status.installer_available === false);
  const loginDisabled = busy
    || checking
    || awaitingLogin
    || !status?.cli_available
    || status.auth_status !== "not_authenticated";
  const loginDisabledReason = authReady
    ? "当前已经登录，无需重复操作"
    : status?.auth_status === "unknown"
      ? "请先重新检测登录状态"
      : !status?.cli_available
        ? "请先安装 Codex CLI"
        : undefined;

  return (
    <section className="settings-block codex-setup">
      <div className="codex-setup-heading">
        <div>
          <h3>Codex 协作</h3>
          <p className="desc">连接本机 Codex CLI，权限预设会在每次委派子代理时自动读取。登录凭据始终由 Codex 管理。</p>
        </div>
        <button
          className={`codex-status-refresh${checking ? " checking" : ""}`}
          disabled={busy || checking || awaitingLogin}
          onClick={() => void refresh()}
          aria-label="重新检测 Codex 状态"
          title="重新检测 Codex 状态"
        >
          <IconRefresh width={16} height={16} />
        </button>
      </div>
      {err && <div className="errbar" role="alert">{err}</div>}
      <div className={`codex-setup-status state-${setupState}`} role="status" aria-live="polite">
        <div className="codex-setup-status-copy">
          <span className="codex-status-dot" aria-hidden="true" />
          <div>
            <strong>{copy.title}</strong>
            <p>{copy.detail}</p>
          </div>
        </div>
        <button
          className={`btn codex-primary-action${setupState === "ready" ? "" : " accent"}`}
          disabled={mainDisabled}
          onClick={() => void completeSetup()}
        >
          {busy ? "正在处理…" : awaitingLogin ? "等待登录…" : copy.action}
        </button>
      </div>

      <ol className="codex-setup-steps" aria-label="Codex 设置进度">
        <li className={status?.cli_available ? "done" : setupState === "install_cli" ? "current" : "pending"}>
          <span className="codex-step-mark">{status?.cli_available && <IconCheck width={12} height={12} />}</span>
          <div><strong>Codex CLI</strong><small>{status?.cli_available ? `可运行${status.cli_source_label ? ` · ${status.cli_source_label}` : ""}` : "待安装"}</small></div>
        </li>
        <li className={authReady ? "done" : setupState === "login" || setupState === "check" ? "current" : "pending"}>
          <span className="codex-step-mark">{authReady && <IconCheck width={12} height={12} />}</span>
          <div><strong>登录</strong><small>{loginLabel}</small></div>
        </li>
        <li className={collaborationReady ? "done" : setupState === "configure" ? "current" : "pending"}>
          <span className="codex-step-mark">{collaborationReady && <IconCheck width={12} height={12} />}</span>
          <div><strong>R-Code 协作</strong><small>{collaborationReady ? "Skill 与 MCP 已连接" : "待配置"}</small></div>
        </li>
      </ol>

      {notice && <p className="codex-inline-note" role="status"><IconCheck width={14} height={14} />{notice}</p>}
      {status?.cli_error && setupState === "install_cli" && <p className="codex-inline-warning">{status.cli_error}</p>}

      <RtkIntegrationControl />

      {setupState === "ready" && (
        <CodexRuntimePreferences
          codexDelegationEnabled={config?.orchestration?.allow_cross_engine_delegation ?? null}
          reloadConfig={reloadConfig}
        />
      )}

      {status && (
        <details className="codex-advanced">
          <summary>高级选项 <span>登录方式与配置详情</span></summary>
          <div className="codex-advanced-body">
            <div className="codex-login-options">
              <div>
                <strong>登录方式</strong>
                <small>{authReady ? "当前已登录，按钮已停用。" : "浏览器登录优先；设备码用于远程或回调受阻环境。"}</small>
              </div>
              <div>
                <button className="btn sm" disabled={loginDisabled} title={loginDisabledReason} onClick={() => void startLogin("browser")}>
                  浏览器登录
                </button>
                <button className="btn sm ghost" disabled={loginDisabled} title={loginDisabledReason} onClick={() => void startLogin("device")}>
                  设备码（备用）
                </button>
              </div>
            </div>
            <dl className="codex-details-list">
              <dt>CLI</dt>
              <dd>{status.cli_available ? `${status.cli_version || "可运行"}${status.cli_source_label ? ` · ${status.cli_source_label}` : ""}` : "不可用"}</dd>
              {status.cli_available && status.cli_path && <><dt>CLI 位置</dt><dd className="val">{status.cli_path}</dd></>}
              <dt>登录</dt>
              <dd>{loginLabel}</dd>
              <dt>协作 Skill</dt>
              <dd>{skillLabel}</dd>
              <dt>Codex MCP</dt>
              <dd>{status.mcp_server_configured ? "已启用" : "尚未启用"}</dd>
              <dt>配置位置</dt>
              <dd className="val">{status.config_path}</dd>
            </dl>
            {!status.cli_available && (
              <p className="codex-manual-install">手动安装：<code>{status.installer_command || "npm install -g @openai/codex"}</code></p>
            )}
          </div>
        </details>
      )}
    </section>
  );
}
