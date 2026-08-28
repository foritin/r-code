import { Fragment, useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { errText } from "../../lib/format";
import { useAppStore, type SettingsPane } from "../../store/app";
import { usePoll } from "../../lib/poll";
import { LanguageSettingsSection } from "../settings/LanguageSettingsSection";
import { NativeNotificationSettings } from "../settings/NativeNotificationSettings";
import { ApplicationUpdaterSettings } from "../settings/ApplicationUpdaterSettings";
import {
  codexCliPreferences,
  codexIntegrationStatus,
  codexSaveCliPreferences,
  codexSetupCollaboration,
  codexStartDeviceLogin,
  codexStartLogin,
  codexSyncCli,
  companionEnsure,
  logsTail,
  providerModels,
  settingsDeleteProvider,
  settingsGet,
  settingsSaveProvider,
  closeBehaviorGet,
  closeBehaviorSet,
  lifecycleExplicitQuit,
  planningStatus,
  settingsSelectProvider,
  settingsSet,
  supportBundleChoose,
  supportPreview,
} from "../../lib/ipc";
import { reloadImageUnderstandingEngine } from "../../lib/image-understanding";
import type { PlanningStatusView } from "../../lib/types";
import type {
  AppConfig,
  CodexCliPreferences,
  CodexCliSyncResult,
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
  RunBudgetConfig,
  SupportBundlePreview,
} from "../../lib/types";
import { clockTime } from "../../lib/format";
import {
  catalogHostedWebRoutes,
  catalogPresets,
  loadCatalog,
  presetOf,
  providerLabel,
  rememberedModelsFor,
  rememberModel,
  rememberSyncedModels,
  syncedModelsFor,
  PROVIDER_SYNC_TTL_MS,
} from "../../lib/provider";
import { useCodexCliGate } from "../codex/CodexCliGate";
import { CODEX_LOGIN_WAIT_MINUTES, nextCodexLoginPollDelay } from "../codex/login-watcher";
import { IconCheck, IconChevronDown, IconRefresh, IconSearch } from "../icons";
import { modalityLabel, resolveImageCapability } from "../room/model-capabilities";
import { Menu, MenuEmpty, MenuItem } from "../ui/Menu";
import { InfoTip } from "../ui/InfoTip";
import { ConfirmDialog } from "../ui/ConfirmDialog";
import { Drawer } from "../ui/Drawer";
import { pushToast } from "../../store/toast";
import { useCompanionStore, type CompanionMotion } from "../../store/companion";
import { providerIconFor, providerInitial } from "../../lib/provider-icons";
import { ExecutionEnvCard } from "./ExecutionEnvCard";
import { McpPanel } from "./McpPanel";
import { KnowledgeSettingsPane } from "./KnowledgeSettingsPane";
import { SubagentProvidersPanel } from "./SubagentProvidersPanel";
import { GuideSheet, type GuideAction, type GuideId } from "../settings/GuideSheet";

const LOG_LEVELS = ["debug", "info", "warn", "error"];
const LOG_FILTERS = ["all", "error", "warn", "info", "debug"] as const;
const EMPTY_PROVIDERS: NonNullable<AppConfig["providers"]> = {};
const OUTPUT_DEFAULT = "8192";
/** 自建网关：不套用任何预设，全部字段手填。 */
const CUSTOM_PRESET = "custom";

/** 行级“测试连接”的一次结果（SET-PROV-015 的 UI 前置，后端复用模型同步通道）。 */
interface ProbeState {
  state: "running" | "ok" | "failed";
  ms?: number;
  error?: string;
}

const SETTINGS_PANES: Array<{
  key: SettingsPane;
  label: string;
  description: string;
}> = [
  { key: "providers", label: "模型服务", description: "配置 R-Code 对话使用的模型与凭据。" },
  { key: "agents", label: "Agent 编排", description: "选择主 Agent、管理 Codex 运行时、委派路由和质量复核。" },
  { key: "subagents", label: "子代理配置", description: "管理候选来源、路由槽位、权重、Prompt 与连通测试。" },
  { key: "tools", label: "工具与连接", description: "管理内置工具、RTK 加速、MCP 服务、凭据和扩展市场。" },
  { key: "knowledge", label: "知识与指令", description: "管理公共与项目记忆、协作 Prompt 和可复用 Skills。" },
  { key: "permissions", label: "权限", description: "Codex 子代理权限五态的唯一编辑入口与当前生效值。" },
  { key: "security", label: "隐私与安全", description: "密钥存储、脱敏与 CSP/sandbox 的只读强制状态。" },
  { key: "appearance", label: "外观与语言", description: "选择界面主题、语言，并管理桌面小助手的反馈方式。" },
  { key: "notifications", label: "通知", description: "OS 权限、类别开关与应用内测试；拒绝仅降级。" },
  { key: "lifecycle", label: "启动与关闭", description: "关闭行为与启动检查合同（Host 状态机接入中）。" },
  { key: "updates", label: "更新", description: "检查、下载、安装重启与稍后重启。" },
  { key: "diagnostics", label: "诊断", description: "查看运行日志，或导出脱敏支持信息。" },
];

const CATEGORY_LABELS: Record<ProviderCategory, string> = {
  official: "海外官方",
  cn_official: "国内厂商",
  cloud_provider: "云厂商托管",
  aggregator: "路由 / 聚合",
};

/** E4 设置搜索索引：按区块标题 + 字段关键词 + 手册关键词过滤，命中跨面板时
 * 经 setSettingsPane + scrollIntoView + flash-target 深链定位。 */
interface SettingsSearchEntry {
  pane: SettingsPane;
  blockId: string;
  title: string;
  keywords: string[];
}

const SETTINGS_SEARCH_INDEX: SettingsSearchEntry[] = [
  { pane: "providers", blockId: "providers-block", title: "对话模型（模型服务）", keywords: ["服务", "provider", "默认服务", "设为默认", "密钥", "api key", "凭据", "预设", "模型", "协议", "线路", "多模态", "同步模型", "新建服务", "openai", "anthropic", "deepseek", "kimi", "glm", "火山", "百炼", "openrouter"] },
  { pane: "providers", blockId: "image-understanding-block", title: "图片理解", keywords: ["图片", "ocr", "视觉模型", "多模态", "截图", "贴图", "附件", "图片理解引擎"] },
  { pane: "agents", blockId: "orchestration-main-agent", title: "主 Agent", keywords: ["主 agent", "引擎", "r-code", "codex", "新会话默认"] },
  { pane: "agents", blockId: "orchestration-delegation", title: "委派路由", keywords: ["委派", "路由", "子代理", "复杂度", "跨引擎"] },
  { pane: "agents", blockId: "orchestration-quality", title: "质量复核", keywords: ["复核", "质量", "review", "轮次", "修订"] },
  { pane: "agents", blockId: "planning-suggestion-block", title: "复杂任务先建议制定计划", keywords: ["plan", "计划", "规划建议", "deepseek", "复杂任务", "先询问"] },
  { pane: "agents", blockId: "orchestration-run-budget", title: "运行护栏", keywords: ["护栏", "预算", "轮数", "时长", "思考量", "同错", "零进展", "变更范围", "测试连败", "checkpoint", "回滚"] },
  { pane: "agents", blockId: "orchestration-skills", title: "内置编排能力", keywords: ["任务拆解", "路由", "codex 子代理", "质量复核循环"] },
  { pane: "agents", blockId: "codex-setup-block", title: "Codex 运行时", keywords: ["codex", "登录", "认证", "协作", "cli", "更新", "升级", "mcp", "安装", "权限", "模型"] },
  { pane: "tools", blockId: "mcp-panel-block", title: "工具与连接（MCP）", keywords: ["mcp", "工具", "连接", "市场", "扩展", "rtk"] },
  { pane: "tools", blockId: "execution-env-block", title: "执行环境（Windows Shell）", keywords: ["shell", "bash", "git bash", "powershell", "执行环境", "命令", "路径", "回落", "方言"] },
  { pane: "knowledge", blockId: "knowledge-block", title: "知识与指令", keywords: ["记忆", "prompt", "提示词", "skills", "技能", "知识库", "指令"] },
  { pane: "appearance", blockId: "language-block", title: "界面语言", keywords: ["语言", "language", "中文", "english", "locale"] },
  { pane: "notifications", blockId: "native-notifications-block", title: "系统通知", keywords: ["通知", "系统通知", "桌面通知", "notification", "permission", "权限", "后台"] },
  { pane: "appearance", blockId: "appearance-block", title: "界面主题", keywords: ["主题", "外观", "亮色", "暗色", "跟随系统"] },
  { pane: "appearance", blockId: "companion-block", title: "桌面小助手", keywords: ["小助手", "悬浮", "提示音", "动效", "形态"] },
  { pane: "diagnostics", blockId: "request-audit-block", title: "请求构成审计", keywords: ["审计", "请求信封", "旁路", "request audit"] },
  { pane: "diagnostics", blockId: "log-level-block", title: "日志记录", keywords: ["日志", "级别", "log", "debug", "info", "warn", "error"] },
  { pane: "diagnostics", blockId: "log-section", title: "诊断日志", keywords: ["日志", "诊断", "实时", "过滤"] },
  { pane: "diagnostics", blockId: "support-block", title: "支持包", keywords: ["支持包", "导出", "脱敏", "诊断", "反馈"] },
  { pane: "subagents", blockId: "subagent-pool-block", title: "候选来源与路由池", keywords: ["子代理", "候选池", "槽位", "权重", "连通", "测试", "prompt", "codex cli"] },
];

function searchSettingsEntries(query: string): SettingsSearchEntry[] {
  const normalized = query.trim().toLowerCase();
  if (!normalized) return [];
  return SETTINGS_SEARCH_INDEX.filter((entry) => {
    const haystack = [entry.title, ...entry.keywords, entry.pane].join(" ").toLowerCase();
    // 空格分词：每个词都要命中（AND），支持"默认 服务"这类组合。
    return normalized.split(/\s+/).every((token) => haystack.includes(token));
  }).slice(0, 12);
}

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
  const { t } = useTranslation();
  const activePane = useAppStore((state) => state.settingsPane);
  const setActivePane = useAppStore((state) => state.setSettingsPane);
  const [config, setConfig] = useState<AppConfig | null>(null);
  const [configErr, setConfigErr] = useState<string | null>(null);
  const [validation, setValidation] = useState<string | null>(null);
  const [providerStatus, setProviderStatus] = useState<Record<string, ProviderStatus>>({});
  const [openGuide, setOpenGuide] = useState<GuideId | null>(null);
  // 手册页脚动作请求的跨页定位目标（如诊断页的审计卡）：页签渲染完成后闪烁并聚焦。
  const pendingPaneFocus = useRef<string | null>(null);

  const focusSettingsBlock = useCallback((targetId: string) => {
    const block = document.getElementById(targetId);
    if (!block) return false;
    block.scrollIntoView({ block: "center" });
    // 重启动画：先移除类再强制 reflow，保证连续两次跳转也能看到定位闪烁。
    block.classList.remove("flash-target");
    void block.offsetWidth;
    block.classList.add("flash-target");
    block.querySelector<HTMLElement>(".switch, .input, button")?.focus({ preventScroll: true });
    return true;
  }, []);

  const handleGuideAction = useCallback((action: GuideAction) => {
    if (action === "open-request-audit") {
      setOpenGuide(null);
      pendingPaneFocus.current = "request-audit-block";
      setActivePane("diagnostics");
      return;
    }
    if (action === "open-image-understanding") {
      setOpenGuide(null);
      if (activePane === "providers") {
        focusSettingsBlock("image-understanding-block");
        return;
      }
      pendingPaneFocus.current = "image-understanding-block";
      setActivePane("providers");
    }
  }, [activePane, focusSettingsBlock, setActivePane]);

  useEffect(() => {
    if (!pendingPaneFocus.current) return;
    // 目标区块可能要等 config 异步到达后才渲染；未命中时保留目标等下一次重试。
    if (focusSettingsBlock(pendingPaneFocus.current)) pendingPaneFocus.current = null;
  }, [activePane, config, focusSettingsBlock]);

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

  // Codex 协作的就绪签名变化（CLI 可用 / 登录 / 协作配置）会改变 Host 侧的
  // 子代理候选目录；把变化转成递增信号，让候选来源面板在不丢草稿的前提下刷新。
  const [subagentRefreshSignal, setSubagentRefreshSignal] = useState(0);
  const codexReadinessRef = useRef("");
  const handleCodexStatusChange = useCallback((status: CodexIntegrationStatus) => {
    const signature = [
      status.cli_available,
      status.auth_status,
      status.skill_status,
      status.mcp_server_configured,
    ].join("|");
    if (signature === codexReadinessRef.current) return;
    codexReadinessRef.current = signature;
    setSubagentRefreshSignal((value) => value + 1);
  }, []);

  // provider 档案名 → 目录 kind，用于给子代理候选来源匹配厂商图标。
  const providerKinds = useMemo(
    () => Object.fromEntries(
      Object.entries(config?.providers ?? EMPTY_PROVIDERS)
        .map(([name, profile]) => [name, (profile as ProviderConfig).provider_kind]),
    ),
    [config],
  );

  const settingsPanes = useMemo(() => SETTINGS_PANES.map((item) => (
    item.key === "appearance"
      ? {
          ...item,
          label: t("settings.preferences.label"),
          description: t("settings.preferences.description"),
        }
      : item
  )), [t]);
  const pane = settingsPanes.find((item) => item.key === activePane) ?? settingsPanes[0];

  // E4 设置搜索：命中跨面板区块时经既有深链机制定位（切换页签 + 闪烁聚焦）。
  const [searchQuery, setSearchQuery] = useState("");
  const searchResults = useMemo(() => searchSettingsEntries(searchQuery), [searchQuery]);
  const jumpToBlock = useCallback((entry: SettingsSearchEntry) => {
    setSearchQuery("");
    if (entry.pane === activePane) {
      // 同面板命中：zustand 不会触发重渲染，直接定位。
      focusSettingsBlock(entry.blockId);
      return;
    }
    pendingPaneFocus.current = entry.blockId;
    setActivePane(entry.pane);
  }, [activePane, focusSettingsBlock, setActivePane]);

  return (
    <div className="scene">
      <div className="scene-scroll">
        <div className="page-head">
          <h1>{t("settings.pageTitle")}</h1>
          <div className="settings-search">
            <IconSearch width={14} height={14} aria-hidden="true" />
            <input
              className="input settings-search-input"
              type="search"
              value={searchQuery}
              placeholder="搜索设置项（如：默认服务、OCR、子代理）"
              aria-label="搜索设置项"
              onChange={(event) => setSearchQuery(event.target.value)}
            />
            {searchQuery && (
              <button
                type="button"
                className="quiet-link"
                aria-label="清空搜索"
                onClick={() => setSearchQuery("")}
              >
                清空
              </button>
            )}
            {searchResults.length > 0 && (
              <ul className="settings-search-results" role="listbox" aria-label="搜索结果">
                {searchResults.map((entry) => (
                  <li key={`${entry.pane}:${entry.blockId}`}>
                    <button
                      type="button"
                      role="option"
                      aria-selected={false}
                      onClick={() => jumpToBlock(entry)}
                    >
                      <span className="settings-search-title">{entry.title}</span>
                      <span className="settings-search-pane">
                        {settingsPanes.find((item) => item.key === entry.pane)?.label ?? entry.pane}
                      </span>
                    </button>
                  </li>
                ))}
              </ul>
            )}
            {searchQuery.trim() && searchResults.length === 0 && (
              <p className="settings-search-empty" role="status">没有匹配的设置项；可换个关键词试试。</p>
            )}
          </div>
        </div>

        <div className="settings-layout">
          <nav className="settings-nav" aria-label={t("settings.categoriesLabel")}>
            {settingsPanes.map((item) => (
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

          <div className={`settings-detail${activePane === "agents" ? " settings-agent-detail" : ""}`}>
            <header className="settings-detail-head">
              {activePane === "agents" && <span className="settings-detail-eyebrow">AGENT</span>}
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
                  <>
                    <ProviderSection config={config} providerStatus={providerStatus} reload={loadConfig} onOpenGuide={setOpenGuide} />
                    <ImageUnderstandingSection config={config} providerStatus={providerStatus} reload={loadConfig} onOpenGuide={setOpenGuide} />
                  </>
                ) : (
                  !configErr && <div className="settings-loading">正在读取模型服务…</div>
                )}
              </div>
            )}

            {activePane === "appearance" && (
              <div className="settings-preferences">
                <LanguageSettingsSection />
                <AppearanceSection />
                <CompanionSection />
              </div>
            )}

            {activePane === "notifications" && (
              <div className="settings-preferences">
                <NativeNotificationSettings />
              </div>
            )}

            {activePane === "updates" && (
              <div className="settings-preferences">
                <ApplicationUpdaterSettings />
              </div>
            )}

            {activePane === "permissions" && <CodexPermissionSection />}

            {activePane === "security" && <SecuritySection />}

            {activePane === "lifecycle" && <LifecycleSection />}

            {activePane === "tools" && (
              <div className="settings-sheet settings-tools-sheet">
                <ExecutionEnvCard />
                <McpPanel />
              </div>
            )}

            {activePane === "knowledge" && <KnowledgeSettingsPane />}

            {activePane === "agents" && (
              <div className="settings-sheet settings-orchestration-sheet">
                {config ? (
                  <OrchestrationSection
                    config={config}
                    reload={loadConfig}
                    onOpenGuide={setOpenGuide}
                    codexRuntime={(
                      <CodexIntegrationSection onStatusChange={handleCodexStatusChange} />
                    )}
                  />
                ) : (
                  !configErr && <div className="settings-loading">正在读取 Agent 编排策略…</div>
                )}
              </div>
            )}

            {activePane === "diagnostics" && (
              <div className="settings-sheet">
                {config && <RequestAuditSection config={config} reload={loadConfig} />}
                {config && <LogLevelSection config={config} reload={loadConfig} />}
                <LogSection />
                <SupportSection />
              </div>
            )}

            {activePane === "subagents" && (
              <div className="settings-sheet subagent-configuration-sheet">
                <SubagentProvidersPanel
                  providerKinds={providerKinds}
                  refreshSignal={subagentRefreshSignal}
                  onOpenGuide={setOpenGuide}
                />
              </div>
            )}
          </div>
        </div>
      </div>
      <GuideSheet
        guideId={openGuide}
        onClose={() => setOpenGuide(null)}
        onAction={handleGuideAction}
      />
    </div>
  );
}

// ---------- Provider ----------

function ProviderSection({
  config,
  providerStatus,
  reload,
  onOpenGuide,
}: {
  config: AppConfig;
  providerStatus: Record<string, ProviderStatus>;
  reload: () => Promise<void>;
  onOpenGuide: (id: GuideId) => void;
}) {
  const configDefault = config.default_provider ?? "";
  const providers = config.providers ?? EMPTY_PROVIDERS;
  const names = Object.keys(providers).sort((a, b) => a.localeCompare(b));
  const [catalog, setCatalog] = useState<ProviderPreset[]>([]);
  const [hostedWebRoutes, setHostedWebRoutes] = useState<HostedWebRoute[]>([]);
  const [selectedProvider, setSelectedProvider] = useState<string | null>(null);
  const [presetName, setPresetName] = useState(CUSTOM_PRESET);
  // 新建草稿态：点击“新建服务”后打开新建抽屉，保存成功或取消后消失。
  const [drafting, setDrafting] = useState(false);
  // 编辑抽屉开关：列表行只负责“打开抽屉看某个服务”，不再常驻右栏。
  const [editorOpen, setEditorOpen] = useState(false);
  // 行级“测试”结果（SET-PROV-015 的 UI 前置）：key = 服务名。
  const [probe, setProbe] = useState<Record<string, ProbeState>>({});
  // 用户是否改过抽屉里的表单：关闭/切换目标前据此弹“放弃更改”确认。
  const [formDirty, setFormDirty] = useState(false);
  // 未保存守卫：close = 丢弃并关抽屉；switch = 丢弃并切换到目标服务。
  const [discard, setDiscard] = useState<{ mode: "close" } | { mode: "switch"; name: string } | null>(null);
  const [profileName, setProfileName] = useState("");
  const [fields, setFields] = useState({
    base_url: "",
    model: "",
    max_tokens: "",
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
  const probeRequest = useRef(0);
  // 用户手改字段 = 表单变脏；effect 里的程序化回填不走这里，避免误报。
  const mutateFields = (updater: (value: typeof fields) => typeof fields) => {
    setFormDirty(true);
    setFields(updater);
  };
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
      // 预设声明了单次输出上限时，直接采用厂商值并锁定，避免保存后被服务端 400；
      // 未声明时留空（显示"默认"，后端用目录上限兜底）。思考模型的 reasoning 计入
      // 输出预算，预填保守小值会让推理把预算耗尽后整轮报废。
      max_tokens: preset?.max_output_tokens != null
        ? String(preset.max_output_tokens)
        : "",
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
      setFormDirty(false);
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
      // 厂商预设声明了单次输出上限时，编辑态同样锁定为厂商值，避免历史误填继续生效；
      // 未声明的线路显示持久化值，持久化也为空时保持空（"默认"）——不能回填种子值，
      // 否则用户只是打开设置再保存就会把 8192 落盘，"默认"永远无法存活。
      max_tokens: preset?.max_output_tokens != null
        ? String(preset.max_output_tokens)
        : profile?.max_tokens != null ? String(profile.max_tokens) : "",
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
    setFormDirty(false);
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

  // 已保存服务：点开详情即自动同步模型并持久化（手动同步只保留给新建流程）。
  // 必须等表单同步到所选服务之后再触发（profileName 对齐）：选中后的第一帧
  // 字段仍是上一个服务的旧值，用旧闭包发请求会把结果算错对象。
  // 新鲜度窗口内不重复请求；失败退避 30 秒，避免反复点击/同步失败时打爆接口。
  const lastSyncAttemptRef = useRef<Record<string, number>>({});
  useEffect(() => {
    if (!selectedProvider || drafting || busy || modelsBusy) return;
    if (profileName.trim() !== selectedProvider) return;
    if (!providers[selectedProvider]) return;
    if (!providerStatus[selectedProvider]?.ready) return;
    if (!fields.base_url.trim()) return;
    const cached = syncedModelsFor(selectedProvider);
    if (cached && Date.now() - cached.at < PROVIDER_SYNC_TTL_MS) return;
    const lastAttempt = lastSyncAttemptRef.current[selectedProvider] ?? 0;
    if (Date.now() - lastAttempt < 30_000) return;
    lastSyncAttemptRef.current[selectedProvider] = Date.now();
    void fetchModels();
    // fetchModels 每次渲染重建；此 effect 只由下方依赖驱动，按需触发。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [selectedProvider, drafting, busy, modelsBusy, providers, providerStatus, profileName, fields.base_url]);

  const activePreset = presetOf(presetName);
  const pendingVars = unresolvedTemplateVars(activePreset, fields.base_url);
  const modelChoices = Array.from(
    new Set(
      [
        fields.model,
        ...remoteModels,
        ...(activePreset?.models ?? []).map((entry) => entry.id),
      ]
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
        // 程序化回填（同步结果预选首个模型）：不算用户修改，不打脏标。
        setFields((current) => ({ ...current, model: response.models[0] }));
      }
      // 已保存服务：同步结果持久化（供模型胶囊、图片理解下拉等跨页消费）。
      // 按本次请求的服务名记账，避免选中切换瞬间的过期闭包把清单记到别的服务上。
      const requestedName = profileName.trim();
      if (requestedName && providers[requestedName]) {
        rememberSyncedModels(requestedName, response.models);
      }
      setModelsMessage(`服务返回 ${response.models.length} 个可用模型`);
    } catch (cause) {
      if (modelRequest.current !== requestId) return;
      setModelsError(errText(cause));
    } finally {
      if (modelRequest.current === requestId) setModelsBusy(false);
    }
  };

  // SET-PROV-015 的 UI 前置：行级“测试”复用模型同步通道（providerModels）对已
  // 保存服务做一次轻量探测并计时。apiKey 传 null = 使用已保存凭据，与点开抽屉
  // 后的自动同步走同一条路；同名的同步结果顺手刷新，供模型胶囊等跨页消费。
  const probeProvider = async (name: string) => {
    const profile = providers[name] as ProviderConfig | undefined;
    if (!profile || busy) return;
    const preset = presetOf(profile.provider_kind ?? name);
    const requestId = ++probeRequest.current;
    const started = performance.now();
    setProbe((value) => ({ ...value, [name]: { state: "running" } }));
    try {
      const response = await providerModels({
        name,
        preset: preset?.id ?? null,
        baseUrl: profile.base_url ?? "",
        apiKey: null,
        protocol: providerStatus[name]?.effective_protocol ?? profile.protocol ?? "openai_chat",
      });
      if (probeRequest.current !== requestId) return;
      rememberSyncedModels(name, response.models);
      setProbe((value) => ({
        ...value,
        [name]: { state: "ok", ms: Math.max(1, Math.round(performance.now() - started)) },
      }));
    } catch (cause) {
      if (probeRequest.current !== requestId) return;
      setProbe((value) => ({ ...value, [name]: { state: "failed", error: errText(cause) } }));
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
      setFormDirty(false);
      setKeyInput("");
      // 保存后的配置已变化，旧行内测试结果不再代表当前状态。
      setProbe((value) => {
        if (!(name in value)) return value;
        const next = { ...value };
        delete next[name];
        return next;
      });
    }, activate ? "已保存，并设为默认服务" : "配置已保存");

  const selectProvider = (name: string) =>
    void run(() => settingsSelectProvider(name), "已设为默认，新对话将使用这项服务");

  const deleteProvider = (name: string) => {
    if (!window.confirm(`删除“${providerLabel(name)}”及其本机凭据？此操作无法撤销。`)) return;
    void run(async () => {
      await settingsDeleteProvider(name);
      if (selectedProvider === name) setSelectedProvider(null);
    }, "配置已删除");
  };

  const startNewProvider = () => {
    setDrafting(true);
    setDiscard(null);
    setFormDirty(false);
    setSelectedProvider(null);
    applyPreset(catalog[0]?.id ?? CUSTOM_PRESET);
    setEditorOpen(true);
  };

  const cancelDraft = () => {
    setDrafting(false);
    setDiscard(null);
    setFormDirty(false);
    setSelectedProvider(
      configDefault && providers[configDefault] ? configDefault : names[0] ?? null
    );
    setEditorOpen(false);
  };

  // 行点击 / “编辑”：抽屉里有未保存修改时先确认，再切换目标。
  const requestEdit = (name: string) => {
    if (formDirty) {
      setDiscard({ mode: "switch", name });
      return;
    }
    setDrafting(false);
    setSelectedProvider(name);
    setEditorOpen(true);
  };

  const requestCloseDrawer = () => {
    if (formDirty) {
      setDiscard({ mode: "close" });
      return;
    }
    if (drafting) {
      cancelDraft();
      return;
    }
    setEditorOpen(false);
  };

  const commitDiscard = () => {
    const action = discard;
    setDiscard(null);
    setFormDirty(false);
    if (!action) return;
    if (action.mode === "close") {
      if (drafting) {
        setDrafting(false);
        setSelectedProvider(
          configDefault && providers[configDefault] ? configDefault : names[0] ?? null
        );
      }
      setEditorOpen(false);
      return;
    }
    setDrafting(false);
    setSelectedProvider(action.name);
    setEditorOpen(true);
  };

  const editing = selectedProvider ? (providers[selectedProvider] as ProviderConfig | undefined) : undefined;
  const editingIcon = editing && selectedProvider
    ? providerIconFor(editing.provider_kind ?? presetOf(selectedProvider)?.id ?? selectedProvider)
    : null;
  const drawerProbe = selectedProvider ? probe[selectedProvider] : undefined;
  const drawerTile = drafting ? (
    <span className="provider-icon-tile is-fallback" aria-hidden="true">＋</span>
  ) : (
    <span className={`provider-icon-tile${editingIcon ? "" : " is-fallback"}`} aria-hidden="true">
      {editingIcon
        ? <img src={editingIcon} alt="" />
        : providerInitial(activePreset?.label ?? providerLabel(selectedProvider ?? ""))}
    </span>
  );
  const credential = selectedProvider ? providerStatus[selectedProvider] : undefined;
  const credentialLabel = credential?.configured
    ? credential.source === "environment"
      ? "由环境变量提供"
      : "已安全保存"
    : "尚未保存";
  const deepSeekV4 = isDeepSeekV4(fields.base_url, fields.model, presetName);
  const outputValue = Number(fields.max_tokens.trim());
  // docs/support/archive/implementation/multimodal-attachments-and-deepseek-plan-anchoring-implementation.md §6.4：本字段是「每轮最大输出」（用户可编辑，
  // 范围 2,048 到厂商上限）；Provider 的服务端上限只作为上界展示，不再锁死
  // 输入框。未填写时后端采用目录 recommended_output_tokens（如 DeepSeek 65,536）。
  const providerMaxOutput = activePreset?.max_output_tokens ?? null;
  const recommendedOutput = activePreset?.recommended_output_tokens ?? null;
  const maxOutputLocked = false;
  const outputBelowMinimum =
    Number.isFinite(outputValue) && fields.max_tokens.trim() !== "" && outputValue < 2_048;
  const outputExceedsProviderLimit = providerMaxOutput != null
    && Number.isFinite(outputValue)
    && fields.max_tokens.trim() !== ""
    && outputValue > providerMaxOutput;
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
    outputExceedsProviderLimit;
  // 占位符没替换就保存 = 一个必然 404 的地址进了配置
  const saveBlocked =
    busy
    || protocolMismatch
    || outputExceedsProviderLimit
    || outputBelowMinimum
    || deepSeekResponsesModelUnsupported
    || pendingVars.length > 0;

  return (
    <section className="settings-block provider-settings" id="providers-block">
      <div className="section-heading">
        <div>
          <h3>对话模型</h3>
          <p className="desc">R-Code 对话使用的模型服务。访问密钥只保存在当前设备的安全凭据存储中，界面不会回显已保存内容。</p>
        </div>
        <div className="section-heading-actions">
          <button
            type="button"
            className="guide-link"
            aria-haspopup="dialog"
            onClick={() => onOpenGuide("providers")}
          >
            指引手册 <span aria-hidden="true">→</span>
          </button>
          <button
            className="btn"
            disabled={busy}
            onClick={startNewProvider}
          >
            新建服务
          </button>
        </div>
      </div>

      {err && <div className="errbar" role="alert">{err}</div>}
      {saved && <div className="okbar" role="status">{saved}</div>}

      <div className="provider-roster" aria-label="已保存的模型服务">
        {names.length === 0 && !drafting ? (
          <div className="provider-empty">还没有服务。点击「新建服务」，选择预设并填入密钥即可开始对话。</div>
        ) : (
          names.map((name) => {
            const profile = providers[name] as ProviderConfig;
            const active = name === configDefault;
            const status = providerStatus[name];
            const icon = providerIconFor(profile.provider_kind ?? presetOf(name)?.id ?? name);
            const probeInfo = probe[name];
            const probeRunning = probeInfo?.state === "running";
            const stateClass = probeInfo?.state === "ok"
              ? " is-ok"
              : probeInfo?.state === "failed"
                ? " is-failed"
                : status?.ready ? " ready" : "";
            const stateLabel = probeRunning
              ? "测试中…"
              : probeInfo?.state === "ok"
                ? `连接正常 · ${probeInfo.ms}ms`
                : probeInfo?.state === "failed"
                  ? "连接失败"
                  : providerStateLabel(status);
            return (
              <div key={name} className="provider-item">
                <button
                  className="provider-row"
                  type="button"
                  disabled={busy}
                  onClick={() => requestEdit(name)}
                >
                  <span className={`provider-icon-tile${icon ? "" : " is-fallback"}`} aria-hidden="true">
                    {icon ? <img src={icon} alt="" /> : providerInitial(providerLabel(name))}
                  </span>
                  <span className="provider-row-text">
                    <span className="provider-row-title">
                      {providerLabel(name)}
                      {active && <em>默认</em>}
                    </span>
                    <span className="provider-row-model">{profile.model || "尚未设置模型"}</span>
                  </span>
                  <span
                    className={`provider-row-state${stateClass}`}
                    title={probeInfo?.state === "failed" ? probeInfo.error : undefined}
                  >
                    {probeRunning ? (
                      <IconRefresh width={12} height={12} />
                    ) : (
                      <span className="provider-state-dot" aria-hidden="true" />
                    )}
                    {stateLabel}
                  </span>
                </button>
                <div className="provider-row-actions">
                  {!active && (
                    <button
                      type="button"
                      className="provider-row-action accent"
                      disabled={busy || !status?.ready}
                      title="设为默认后，新对话将使用这项服务；已开始的对话不受影响"
                      onClick={() => selectProvider(name)}
                    >
                      设为默认
                    </button>
                  )}
                  <button
                    type="button"
                    className="provider-row-action"
                    disabled={busy || probeRunning}
                    title="向该服务发送一次轻量请求，验证连通性"
                    onClick={() => void probeProvider(name)}
                  >
                    测试
                  </button>
                  <button
                    type="button"
                    className="provider-row-action"
                    disabled={busy}
                    onClick={() => requestEdit(name)}
                  >
                    编辑
                  </button>
                </div>
              </div>
            );
          })
        )}
      </div>

      <Drawer
        open={editorOpen && (drafting || selectedProvider != null)}
        title={editing ? providerLabel(selectedProvider ?? "") : "新建服务"}
        subtitle={editing ? "编辑服务 · 更改保存后立即生效" : "选择预设，填入密钥后保存即可开始对话"}
        closeLabel="关闭"
        onClose={requestCloseDrawer}
        closeDisabled={busy}
        icon={drawerTile}
        footer={
          <>
            {editing && selectedProvider && selectedProvider !== configDefault && (
              <button className="quiet-link danger-link" disabled={busy} onClick={() => deleteProvider(selectedProvider)}>
                删除
              </button>
            )}
            {editing && selectedProvider && selectedProvider !== configDefault && (
              <button
                className="btn"
                disabled={busy || !providerStatus[selectedProvider]?.ready}
                title="设为默认后，新对话将使用这项服务；已开始的对话不受影响"
                onClick={() => selectProvider(selectedProvider)}
              >
                设为默认服务
              </button>
            )}
            {drafting && (
              <button className="btn" disabled={busy} onClick={cancelDraft}>取消</button>
            )}
            <span className="spacer" />
            <button className="btn accent" disabled={saveBlocked} onClick={() => saveProvider(true)}>保存并设为默认</button>
          </>
        }
      >
        <div className="provider-form">
          <div className="provider-form-grid">
              <div className="provider-form-field">
                <label>预设</label>
                {editing ? (
                  <div className="provider-preset-fixed">
                    <span className={`provider-icon-tile${editingIcon ? "" : " is-fallback"}`} aria-hidden="true">
                      {editingIcon
                        ? <img src={editingIcon} alt="" />
                        : providerInitial(activePreset?.label ?? providerLabel(selectedProvider ?? ""))}
                    </span>
                    <span className="provider-preset-fixed-text">
                      <strong>{activePreset?.label ?? "自建 / 其它 OpenAI 兼容接口"}</strong>
                      {activePreset && (
                        <small>
                          {PROTOCOL_LABELS[activePreset.protocol]}
                          {activePreset.context_window != null &&
                            ` · ${activePreset.context_window.toLocaleString()} 上下文`}
                          {activePreset.api_key_url && (
                            <>
                              {" · "}
                              <a href={activePreset.api_key_url} target="_blank" rel="noreferrer">获取密钥</a>
                            </>
                          )}
                        </small>
                      )}
                    </span>
                    <span className="provider-field-meta">更换预设请新建服务</span>
                  </div>
                ) : (
                  <div className="provider-preset-grid" role="radiogroup" aria-label="选择预设">
                    {groupByCategory(catalog).map(([category, presets]) =>
                      presets.map((preset) => {
                        const icon = providerIconFor(preset.id);
                        const selected = presetName === preset.id;
                        return (
                          <button
                            key={preset.id}
                            type="button"
                            role="radio"
                            aria-checked={selected}
                            className={`provider-preset${selected ? " selected" : ""}`}
                            disabled={busy}
                            title={CATEGORY_LABELS[category] ?? category}
                            onClick={() => { setFormDirty(true); applyPreset(preset.id); }}
                          >
                            <span className={`provider-icon-tile${icon ? "" : " is-fallback"}`} aria-hidden="true">
                              {icon ? <img src={icon} alt="" /> : providerInitial(preset.label)}
                            </span>
                            <span className="provider-preset-label">{preset.label}</span>
                            <span className="provider-preset-meta">{PROTOCOL_LABELS[preset.protocol]}</span>
                          </button>
                        );
                      })
                    )}
                    <button
                      type="button"
                      role="radio"
                      aria-checked={presetName === CUSTOM_PRESET}
                      className={`provider-preset${presetName === CUSTOM_PRESET ? " selected" : ""}`}
                      disabled={busy}
                      onClick={() => { setFormDirty(true); applyPreset(CUSTOM_PRESET); }}
                    >
                      <span className="provider-icon-tile is-fallback" aria-hidden="true">
                        {providerInitial("自建")}
                      </span>
                      <span className="provider-preset-label">自建 / 其它</span>
                      <span className="provider-preset-meta">OpenAI 兼容</span>
                    </button>
                  </div>
                )}
              </div>

              <div className="provider-form-field">
                <label htmlFor="set-profile-name">配置名称</label>
                <input id="set-profile-name"
                  className="input"
                  value={profileName}
                  readOnly={Boolean(editing)}
                  placeholder="例如：DeepSeek 工作账户"
                  onChange={(event) => { setFormDirty(true); setProfileName(event.target.value); }}
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
                  onChange={(event) => { setFormDirty(true); setKeyInput(event.target.value); }}
                />
              </div>

              <div className="provider-form-field provider-form-field-wide">
                <label htmlFor="set-model">模型 <InfoTip label="模型与多模态说明">候选来自预设目录与接口同步；[多模态] 模型可直接接收图片，[文本] 模型不支持图片输入，未标注的模型能力未确认。</InfoTip></label>
                <div className="provider-model-input">
                  {/* 输入框保留自由输入；候选列表改用 Menu 弹层——原生 datalist 只会显示与当前值前缀匹配的选项，预填默认模型后其余候选会被过滤掉。 */}
                  <input id="set-model"
                    className="provider-model-text"
                    value={fields.model}
                    placeholder="输入或同步模型名称"
                    onChange={(event) => mutateFields((value) => ({ ...value, model: event.target.value }))}
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
                            modelChoices.map((modelName) => {
                              // C3：预设目录命中的模型加能力徽标；未确认能力的模型不加标。
                              const modality = modalityLabel(
                                resolveImageCapability(modelName, {
                                  presetModels: activePreset?.models,
                                }),
                              );
                              return (
                                <MenuItem
                                  key={modelName}
                                  close={close}
                                  checked={fields.model.trim() === modelName}
                                  onSelect={() => {
                                    mutateFields((value) => ({ ...value, model: modelName }));
                                    rememberModel(profileName.trim() || activePreset?.id || "", modelName);
                                  }}
                                >
                                  <span className="model-name" title={modelName}>{modelName}</span>
                                  {modality && <span className="model-modality-badge">{modality}</span>}
                                </MenuItem>
                              );
                            })
                          )}
                        </>
                      )}
                    </Menu>
                    {!editing && (
                      <button
                        className={`provider-model-refresh${modelsBusy ? " loading" : ""}`}
                        type="button"
                        disabled={busy || modelsBusy || pendingVars.length > 0 || !fields.base_url.trim()}
                        title="填好密钥后从当前接口同步模型列表（已保存的服务会在点开时自动同步）"
                        onClick={() => void fetchModels()}
                      >
                        <IconRefresh width={15} height={15} />
                        {modelsBusy ? "同步中" : "同步模型"}
                      </button>
                    )}
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
                    mutateFields((value) => ({ ...value, show_reasoning: event.target.checked }))
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
                    onChange={(event) => mutateFields((value) => ({ ...value, base_url: event.target.value }))}
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
                        title={`${activePreset.base_url}（${(
                          allowedProtocols(activePreset, activePreset.base_url) ?? []
                        )
                          .map((protocol) => PROTOCOL_LABELS[protocol])
                          .join(" / ")}）`}
                        onClick={() =>
                          mutateFields((value) => ({
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
                            title={`${candidate.url}（${(
                              allowedProtocols(activePreset, candidate.url) ?? []
                            )
                              .map((protocol) => PROTOCOL_LABELS[protocol])
                              .join(" / ")}）`}
                            // 协议必须跟着地址一起切：多数备用线路是同一厂商的另一个协议口，
                            // 只改地址会把 Anthropic 的请求发到一个只有 Chat 的 endpoint 上。
                            onClick={() =>
                              mutateFields((value) => ({
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
                  <label htmlFor="set-protocol">线路协议 <InfoTip label="线路协议说明">协议决定请求体形状与计费线路：同一厂商的不同入口常是不同协议（如火山 /api/coding 是 Anthropic、/api/coding/v3 是 OpenAI）。切换接口线路时协议会一起切换。</InfoTip></label>
                  <select id="set-protocol"
                    className="input"
                    disabled={busy}
                    value={fields.protocol}
                    onChange={(event) =>
                      mutateFields((value) => ({
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
                  <label htmlFor="set-max-tokens">每轮最大输出</label>
                  <input id="set-max-tokens"
                    className="input"
                    inputMode="numeric"
                    value={fields.max_tokens}
                    disabled={maxOutputLocked}
                    onChange={(event) => mutateFields((value) => ({ ...value, max_tokens: event.target.value }))}
                  />
                  <span className="provider-field-meta">
                    {providerMaxOutput != null
                      ? recommendedOutput != null
                        ? `自动 ${Number(recommendedOutput).toLocaleString()} / 服务上限 ${Number(providerMaxOutput).toLocaleString()}`
                        : `服务上限 ${Number(providerMaxOutput).toLocaleString()}`
                      : "通常建议 8,192"}
                  </span>
                  {outputExceedsProviderLimit && (
                    <span className="provider-field-warning" role="alert">
                      当前值超出服务上限，请
                      <button className="quiet-link" type="button" onClick={() => mutateFields((value) => ({ ...value, max_tokens: String(providerMaxOutput ?? OUTPUT_DEFAULT) }))}>
                        恢复为 {Number(providerMaxOutput ?? OUTPUT_DEFAULT).toLocaleString()}
                      </button>
                    </span>
                  )}
                  {outputBelowMinimum && (
                    <span className="provider-field-warning" role="alert">
                      每轮输出不能低于 2,048
                    </span>
                  )}
                </div>

                <div className="provider-form-field">
                  <label htmlFor="set-temperature">随机性</label>
                  <input id="set-temperature"
                    className="input"
                    inputMode="decimal"
                    value={fields.temperature}
                    onChange={(event) => mutateFields((value) => ({ ...value, temperature: event.target.value }))}
                  />
                  <span className="provider-field-meta">编码任务建议 0.1–0.3</span>
                </div>
              </div>
            </details>

            {drawerProbe?.state === "ok" && (
              <span className="provider-field-success" role="status">连接正常 · {drawerProbe.ms}ms</span>
            )}
            {drawerProbe?.state === "failed" && (
              <span className="provider-field-warning" role="alert">连接失败：{drawerProbe.error}</span>
            )}
          </div>
        </Drawer>

      <ConfirmDialog
        open={discard != null}
        title="放弃未保存的更改？"
        description="抽屉里有尚未保存的修改，继续将丢弃这些内容。"
        confirmLabel="放弃更改"
        onConfirm={commitDiscard}
        onCancel={() => setDiscard(null)}
      />
    </section>
  );
}

// ---------- 图片理解（docs D3） ----------

/** 服务下的模型候选（C2 元数据）：预设候选 + 配置中的当前模型（能力未知）。
 * `vision`：true = 目录标注多模态；false = 目录标注纯文本；null = 能力未确认。 */
interface ImageModelOption {
  id: string;
  vision: boolean | null;
}

function imageModelsOfProvider(name: string, provider: ProviderConfig | undefined): ImageModelOption[] {
  const preset = presetOf(provider?.provider_kind ?? name);
  const options: ImageModelOption[] = [...(preset?.models ?? [])];
  const seen = new Set(options.map((entry) => entry.id));
  // 配置中的当前模型与用户用过并记住的模型（同步/手填）一并合并；这些模型
  // 不在人工核对的目录里，能力按未知三态展示（不加徽标）。
  for (const id of [
    (provider?.model ?? "").trim(),
    ...rememberedModelsFor(name),
    ...(syncedModelsFor(name)?.models ?? []),
  ]) {
    if (id && !seen.has(id)) {
      seen.add(id);
      options.push({ id, vision: null });
    }
  }
  return options;
}

/** 视觉模型的默认首选：第一个目录标注多模态的模型，否则第一个候选。 */
function preferredImageModel(models: ImageModelOption[]): string {
  return models.find((entry) => entry.vision === true)?.id ?? models[0]?.id ?? "";
}

function ImageUnderstandingSection({
  config,
  providerStatus,
  reload,
  onOpenGuide,
}: {
  config: AppConfig;
  providerStatus: Record<string, ProviderStatus>;
  reload: () => Promise<void>;
  onOpenGuide: (id: GuideId) => void;
}) {
  const engine = config.image_understanding?.engine === "model" ? "model" : "ocr";
  const configuredProvider = config.image_understanding?.model_provider?.trim() ?? "";
  const configuredModel = config.image_understanding?.model?.trim() ?? "";
  const [busy, setBusy] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);

  // 全部已配置服务（不过滤多模态能力：能力在模型级展示，服务级保持完整可见）。
  // 未就绪（缺密钥等）的条目在下拉中禁用并标注原因。
  const providers = Object.entries(config.providers ?? {})
    .map(([name, provider]) => {
      const profile = provider as ProviderConfig;
      return {
        name,
        label: providerLabel(name),
        ready: Boolean(providerStatus[name]?.ready),
        models: imageModelsOfProvider(name, profile),
      };
    })
    .sort((left, right) => left.label.localeCompare(right.label));

  const selected = providers.find((entry) => entry.name === configuredProvider);
  // 配置指向的服务已被删除：保留显示 + 明确提示，不静默清除。
  const providerDeleted = engine === "model" && Boolean(configuredProvider) && !selected;
  const modelOptions = selected?.models ?? [];
  const modelValue = configuredModel && modelOptions.some((entry) => entry.id === configuredModel)
    ? configuredModel
    : "";

  const save = async (field: "engine" | "model_provider" | "model", value: unknown) => {
    setBusy(field);
    setErr(null);
    try {
      await settingsSet(`image_understanding.${field}`, value);
      await reload();
      // 立即刷新共享引擎快照：输入区 chip 与发送标记据此分派，不等下一次挂载。
      void reloadImageUnderstandingEngine();
    } catch (cause) {
      setErr(errText(cause));
    } finally {
      setBusy(null);
    }
  };

  const chooseEngine = (next: "ocr" | "model") => {
    if (next === "ocr") {
      // 切回 OCR 时清空模型字段，配置不残留指向已删服务的悬空引用。
      void (async () => {
        setBusy("engine");
        setErr(null);
        try {
          await settingsSet("image_understanding.engine", "ocr");
          await settingsSet("image_understanding.model_provider", null);
          await settingsSet("image_understanding.model", null);
          await reload();
          void reloadImageUnderstandingEngine();
        } catch (cause) {
          setErr(errText(cause));
        } finally {
          setBusy(null);
        }
      })();
      return;
    }
    void save("engine", "model");
  };

  const selectProvider = (nextProvider: string) => {
    const next = providers.find((entry) => entry.name === nextProvider);
    void (async () => {
      setBusy("model_provider");
      setErr(null);
      try {
        await settingsSet("image_understanding.model_provider", nextProvider);
        // 服务切换后旧模型大概率不在新服务中：直接预选首选模型（优先多模态）。
        await settingsSet(
          "image_understanding.model",
          next ? preferredImageModel(next.models) || null : null,
        );
        await reload();
        void reloadImageUnderstandingEngine();
      } catch (cause) {
        setErr(errText(cause));
      } finally {
        setBusy(null);
      }
    })();
  };

  return (
    <section className="settings-block image-understanding-block" id="image-understanding-block">
      <div className="block-title-row">
        <h3>图片理解 <InfoTip label="图片理解说明">辅助引擎只服务文本主模型：主模型目录确认多模态时原图直发，不经本机 OCR 或视觉模型。OCR 只提取文字（离线免费）；视觉模型理解整张图并生成描述（消耗调用）。</InfoTip></h3>
        <span className="block-hint">只对文本主模型生效 · 主模型确认为多模态时原图直发，不经引擎</span>
        <button
          type="button"
          className="guide-link"
          aria-haspopup="dialog"
          onClick={() => onOpenGuide("image-understanding")}
        >
          指引手册 <span aria-hidden="true">→</span>
        </button>
      </div>
      <p className="desc">
        决定发送的图片如何被模型理解：本机 OCR 只提取文字；视觉模型会理解整张图片并生成
        结构化描述。切换后对新发送的图片生效；原图仅本地留存预览。
        <strong>主模型本身支持图片输入（目录确认多模态）时，原图直接发送、不经过引擎</strong>——
        这里选择的引擎只对文本主模型生效。
      </p>
      {err && <div className="errbar" role="alert">{err}</div>}
      <div className="image-understanding-engines" role="radiogroup" aria-label="图片理解引擎">
        <button
          type="button"
          role="radio"
          aria-checked={engine === "ocr"}
          className={`image-engine-option${engine === "ocr" ? " is-selected" : ""}`}
          disabled={busy != null}
          onClick={() => chooseEngine("ocr")}
        >
          <span className="image-engine-radio" aria-hidden="true" />
          <span className="image-engine-copy">
            <strong>本机 OCR（默认）</strong>
            <small>文本主模型的辅助：离线、免费，仅提取图片中的文字（PNG/JPEG）注入上下文。</small>
          </span>
        </button>
        <button
          type="button"
          role="radio"
          aria-checked={engine === "model"}
          className={`image-engine-option${engine === "model" ? " is-selected" : ""}`}
          disabled={busy != null}
          onClick={() => chooseEngine("model")}
        >
          <span className="image-engine-radio" aria-hidden="true" />
          <span className="image-engine-copy">
            <strong>视觉模型</strong>
            <small>文本主模型的辅助：由指定的多模态模型理解整张图片并生成描述；多图并发理解，每次消耗该服务的调用。</small>
          </span>
        </button>
      </div>
      {/* 选 OCR 时服务/模型两行不隐藏而是置灰：配置的位置感保留，误点被禁用挡住。 */}
      <div
        className={`image-understanding-target${engine === "model" ? "" : " is-off"}`}
        aria-hidden={engine !== "model"}
      >
          {providers.length === 0 ? (
            <p className="provider-field-warning" role="alert">
              先在上方配置模型服务并填好密钥，再选择视觉模型。
            </p>
          ) : (
            <>
              <div className="field">
                <label htmlFor="set-image-provider">服务</label>
                <select
                  id="set-image-provider"
                  className="input"
                  value={configuredProvider}
                  disabled={busy != null || engine !== "model"}
                  onChange={(event) => selectProvider(event.target.value)}
                >
                  <option value="" disabled>选择服务</option>
                  {providerDeleted && (
                    <option value={configuredProvider} disabled>
                      {configuredProvider}（服务已被删除，请重新选择）
                    </option>
                  )}
                  {providers.map((entry) => (
                    <option key={entry.name} value={entry.name} disabled={!entry.ready}>
                      {entry.label}{entry.ready ? "" : "（未就绪，先补全密钥）"}
                    </option>
                  ))}
                </select>
                <span className="hint">列出全部已配置服务；未就绪的服务需先在上方补全密钥。</span>
              </div>
              <div className="field">
                <label htmlFor="set-image-model">模型</label>
                <select
                  id="set-image-model"
                  className="input"
                  value={modelValue}
                  disabled={busy != null || !selected || engine !== "model"}
                  onChange={(event) => void save("model", event.target.value || null)}
                >
                  <option value="" disabled>{selected ? "选择模型" : "先选择服务"}</option>
                  {modelOptions.map((entry) => (
                    <option key={entry.id} value={entry.id}>
                      {entry.id}
                      {entry.vision === true ? " [多模态]" : entry.vision === false ? " [文本]" : ""}
                    </option>
                  ))}
                </select>
                <span className="hint">
                  优先选择标注 [多模态] 的模型；[文本] 模型不支持图片输入，未标注的模型能力未确认
                  （失败时 PNG/JPEG 会自动降级本机 OCR，其余返回错误）。
                </span>
              </div>
            </>
          )}
      </div>
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
  run_budget: {
    max_tool_rounds: 60,
    max_run_seconds: 14_400,
    reasoning_budget_chars: 120_000,
    same_error_limit: 3,
    no_progress_rounds: 24,
    replay_detection: true,
    diff_file_limit: 60,
    diff_byte_limit: 262_144,
    test_fail_limit: 3,
    checkpoint_enabled: true,
  },
};

/** DeepSeek 复杂任务建议卡：证据门已移除，客户滑钮是唯一开关。开关可用性只
 * 取决于「是否存在可用的 DeepSeek 服务」与全局急停；运行时按任务实际绑定的
 * 服务生效，无需把 DeepSeek 设为默认服务。 */
function PlanningSuggestionCard({ config, reload, onOpenGuide }: {
  config: AppConfig;
  reload: () => Promise<void>;
  onOpenGuide: (id: GuideId) => void;
}) {
  const [status, setStatus] = useState<PlanningStatusView | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const enabled = config.planning?.suggest_complex_tasks ?? false;

  useEffect(() => {
    let cancelled = false;
    const load = () => {
      planningStatus()
        .then((value) => {
          if (!cancelled) setStatus(value);
        })
        .catch(() => {
          if (!cancelled) setStatus(null);
        });
    };
    load();
    // 宿主推送/测试钩子：状态变化时重查（服务配置变化、急停开关）。
    window.addEventListener("r-code:planning-status-changed", load);
    return () => {
      cancelled = true;
      window.removeEventListener("r-code:planning-status-changed", load);
    };
  }, []);

  const switchEnabled = status?.customer_switch_enabled ?? false;
  const availabilityHint = !status
    ? "暂时无法读取功能状态，仍可手动使用 Plan 模式。"
    : status.emergency_off
      ? "功能当前已暂停，仍可手动使用 Plan 模式。"
      : !status.deepseek_configured
        ? "此功能只对使用 DeepSeek 的任务生效；尚未配置可用的 DeepSeek 服务。"
        : "开 = 复杂任务先询问；关 = 全部直接执行。";

  const toggle = async (value: boolean) => {
    setBusy(true);
    setErr(null);
    try {
      await settingsSet("planning.suggest_complex_tasks", value);
      await reload();
    } catch (cause) {
      setErr(errText(cause));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="settings-block" id="planning-suggestion-block">
      <div className="block-title-row">
        <h3>复杂任务先建议制定计划</h3>
        <button
          type="button"
          className="guide-link"
          aria-haspopup="dialog"
          onClick={() => onOpenGuide("plan-suggestion")}
        >
          指引手册 <span aria-hidden="true">→</span>
        </button>
      </div>
      <p className="desc">
        仅在 DeepSeek 识别到复杂任务时询问（每个任务最多一次）。选择直接继续后本任务不再
        主动弹出，仍可随时手动选择 Plan 模式。只对使用 DeepSeek 服务的任务生效，
        无需把 DeepSeek 设为默认服务。
      </p>
      <div className="field">
        <label htmlFor="set-planning-suggest">复杂任务先建议制定计划 <InfoTip label="开关效果说明">开启后 DeepSeek 在识别到复杂任务时询问一次"先列计划还是直接继续"；每个任务最多一次，拒绝后本任务不再弹出。按任务实际使用的服务生效，无需把 DeepSeek 设为默认。</InfoTip></label>
        <input
          id="set-planning-suggest"
          className="switch"
          type="checkbox"
          role="switch"
          checked={enabled}
          disabled={busy || !switchEnabled}
          onChange={(event) => void toggle(event.target.checked)}
        />
        <span className="hint">{availabilityHint}</span>
      </div>
      {err ? <p className="field-error" role="alert">{err}</p> : null}
      <AnchoringToggle config={config} reload={reload} status={status} busy={busy} setBusy={setBusy} />
    </section>
  );
}

/** DeepSeek Plan 锚定滑钮（docs/support/archive/implementation/multimodal-attachments-and-deepseek-plan-anchoring-implementation.md §9）。与建议开关互不
 * 替代：锚定控制实际进入 DeepSeek Plan 后是否启用最小轨迹与批准后的完整恢复；
 * 开关值在 Plan 创建时冻结，运行中切换只影响之后新建的 Plan。 */
function AnchoringToggle({ config, reload, status, busy, setBusy }: {
  config: AppConfig;
  reload: () => Promise<void>;
  status: PlanningStatusView | null;
  busy: boolean;
  setBusy: (value: boolean) => void;
}) {
  const [err, setErr] = useState<string | null>(null);
  // 只有至少存在一个 ready 的 deepseek 配置时显示该卡（cmd_planning_status
  // 的稳定状态；前端不按 provider 名称猜 DeepSeek）。
  if (!status?.deepseek_configured) return null;
  const anchoring = config.planning?.deepseek_plan_anchoring ?? false;
  const switchEnabled = status.customer_switch_enabled && !status.emergency_off;
  const hint = status.emergency_off
    ? "功能已全局急停（R_CODE_PLANNING_EMERGENCY_OFF），锚定暂不可用。"
    : !status.customer_switch_enabled
      ? "当前不可更改锚定设置。"
      : anchoring
        ? "开：规划期仅保留必要的只读工具与上下文，批准实施后恢复全部可用能力。"
        : "关：DeepSeek Plan 使用标准目录与上下文。";

  const toggle = async (value: boolean) => {
    setBusy(true);
    setErr(null);
    try {
      await settingsSet("planning.deepseek_plan_anchoring", value);
      await reload();
    } catch (cause) {
      setErr(errText(cause));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="field" id="planning-anchoring-field">
      <label htmlFor="set-planning-anchoring">
        DeepSeek Plan 锚定
        <InfoTip label="锚定效果说明">规划时仅保留必要的只读工具和上下文；批准实施后恢复当前任务的全部可用能力。开关只影响之后创建的计划；活动计划的设置在创建时已冻结。</InfoTip>
      </label>
      <input
        id="set-planning-anchoring"
        className="switch"
        type="checkbox"
        role="switch"
        checked={anchoring}
        disabled={busy || !switchEnabled}
        onChange={(event) => void toggle(event.target.checked)}
      />
      <span className="hint anchoring-hint">{hint}</span>
      {err ? <p className="field-error" role="alert">{err}</p> : null}
    </div>
  );
}

function OrchestrationSection({ config, reload, onOpenGuide, codexRuntime }: {
  config: AppConfig;
  reload: () => Promise<void>;
  onOpenGuide: (id: GuideId) => void;
  codexRuntime?: ReactNode;
}) {
  const policy = config.orchestration ?? DEFAULT_ORCHESTRATION;
  const budget: RunBudgetConfig = policy.run_budget ?? DEFAULT_ORCHESTRATION.run_budget!;
  const [busy, setBusy] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);

  const save = async (field: string, value: unknown) => {
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
      <section className="settings-block" id="orchestration-main-agent">
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

      {codexRuntime}

      <section className="settings-block" id="orchestration-delegation">
        <h3>委派路由</h3>
        <p className="desc">选择不同复杂度任务的首选执行者；Codex 不可用时仍会保留清晰的回退原因。</p>
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

      <section className="settings-block" id="orchestration-quality">
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

      <PlanningSuggestionCard config={config} reload={reload} onOpenGuide={onOpenGuide} />

      <section className="settings-block" id="orchestration-run-budget">
        <h3>运行护栏</h3>
        <p className="desc">
          宿主侧硬上限与停止信号：工具轮数、运行时长、思考量、同错连败、零进展、变更范围发散和测试连败
          任一触发后，run 会先做一次总结再进入审核，工作区改动保留、绝不自动回滚。
        </p>
        <div className="orchestration-budget-grid">
          <div className="orchestration-budget-group" role="group" aria-labelledby="orchestration-budget-execution">
            <h4 id="orchestration-budget-execution">执行上限</h4>
            <div className="field">
          <div className="orchestration-budget-label">
            <label htmlFor="set-budget-rounds">工具轮数上限</label>
            <InfoTip label="工具轮数上限说明">默认 60；模型回合产出工具调用即计 1 轮。</InfoTip>
          </div>
          <input
            id="set-budget-rounds"
            className="input"
            type="number"
            min={4}
            max={200}
            step={1}
            value={budget.max_tool_rounds}
            disabled={busy != null}
            onChange={(event) => void save("run_budget.max_tool_rounds", Number(event.target.value))}
          />
        </div>
        <div className="field">
          <div className="orchestration-budget-label">
            <label htmlFor="set-budget-seconds">运行时长上限（秒）</label>
            <InfoTip label="运行时长上限说明">默认 14400（4 小时）；超时前仍会先做收尾总结。</InfoTip>
          </div>
          <input
            id="set-budget-seconds"
            className="input"
            type="number"
            min={300}
            max={86_400}
            step={300}
            value={budget.max_run_seconds}
            disabled={busy != null}
            onChange={(event) => void save("run_budget.max_run_seconds", Number(event.target.value))}
          />
        </div>
        <div className="field">
          <div className="orchestration-budget-label">
            <label htmlFor="set-budget-reasoning">思考量上限（字符）</label>
            <InfoTip label="思考量上限说明">默认 120000；无流式用量时按 4 字符/token 估算。</InfoTip>
          </div>
          <input
            id="set-budget-reasoning"
            className="input"
            type="number"
            min={20_000}
            max={4_000_000}
            step={10_000}
            value={budget.reasoning_budget_chars}
            disabled={busy != null}
            onChange={(event) => void save("run_budget.reasoning_budget_chars", Number(event.target.value))}
          />
        </div>
        <div className="field">
          <div className="orchestration-budget-label">
            <label htmlFor="set-budget-same-error">同一错误连败上限</label>
            <InfoTip label="同一错误连败上限说明">默认 3；按「工具名 + 稳定参数 + 错误码」识别同一错误，成功即清零。</InfoTip>
          </div>
          <input
            id="set-budget-same-error"
            className="input"
            type="number"
            min={1}
            max={10}
            step={1}
            value={budget.same_error_limit}
            disabled={busy != null}
            onChange={(event) => void save("run_budget.same_error_limit", Number(event.target.value))}
          />
        </div>
        <div className="field">
          <div className="orchestration-budget-label">
            <label htmlFor="set-budget-no-progress">零进展轮数上限</label>
            <InfoTip label="零进展轮数上限说明">默认 24；连续没有成功修改或通过测试的轮次达到上限即停。</InfoTip>
          </div>
          <input
            id="set-budget-no-progress"
            className="input"
            type="number"
            min={2}
            max={200}
            step={1}
            value={budget.no_progress_rounds}
            disabled={busy != null}
            onChange={(event) => void save("run_budget.no_progress_rounds", Number(event.target.value))}
          />
            </div>
          </div>
          <div className="orchestration-budget-group" role="group" aria-labelledby="orchestration-budget-scope">
            <h4 id="orchestration-budget-scope">范围与恢复</h4>
            <div className="field">
          <div className="orchestration-budget-label">
            <label htmlFor="set-budget-diff-files">修改文件数上限</label>
            <InfoTip label="修改文件数上限说明">默认 60；超过即视为变更范围发散并停止。</InfoTip>
          </div>
          <input
            id="set-budget-diff-files"
            className="input"
            type="number"
            min={1}
            max={1_000}
            step={1}
            value={budget.diff_file_limit}
            disabled={busy != null}
            onChange={(event) => void save("run_budget.diff_file_limit", Number(event.target.value))}
          />
        </div>
        <div className="field">
          <div className="orchestration-budget-label">
            <label htmlFor="set-budget-diff-bytes">累计变更字节上限</label>
            <InfoTip label="累计变更字节上限说明">默认 262144（256 KiB）；按 old+new 内容长度累计。</InfoTip>
          </div>
          <input
            id="set-budget-diff-bytes"
            className="input"
            type="number"
            min={65_536}
            max={1_073_741_824}
            step={65_536}
            value={budget.diff_byte_limit}
            disabled={busy != null}
            onChange={(event) => void save("run_budget.diff_byte_limit", Number(event.target.value))}
          />
        </div>
        <div className="field">
          <div className="orchestration-budget-label">
            <label htmlFor="set-budget-test-fails">测试连败上限</label>
            <InfoTip label="测试连败上限说明">默认 3；覆盖 cargo/pytest/npm/pnpm/yarn/go/dotnet 测试命令。</InfoTip>
          </div>
          <input
            id="set-budget-test-fails"
            className="input"
            type="number"
            min={1}
            max={10}
            step={1}
            value={budget.test_fail_limit}
            disabled={busy != null}
            onChange={(event) => void save("run_budget.test_fail_limit", Number(event.target.value))}
          />
        </div>
        <div className="field">
          <div className="orchestration-budget-label">
            <label htmlFor="set-budget-replay">循环重放检测</label>
            <InfoTip label="循环重放检测说明">连续 3 轮工具调用与成败形态完全一致时停止。失败重试由同错连败统计；触发后先做一次无工具收尾总结再结束，改动保留。</InfoTip>
          </div>
          <input
            id="set-budget-replay"
            className="switch"
            type="checkbox"
            role="switch"
            checked={budget.replay_detection}
            disabled={busy != null}
            onChange={(event) => void save("run_budget.replay_detection", event.target.checked)}
          />
        </div>
        <div className="field">
          <div className="orchestration-budget-label">
            <label htmlFor="set-budget-checkpoint">绿灯 git checkpoint</label>
            <InfoTip label="绿灯 git checkpoint 说明">测试全绿后用 git stash 快照；审核页可一键回滚到最近绿灯，untracked 文件不回滚。</InfoTip>
          </div>
          <input
            id="set-budget-checkpoint"
            className="switch"
            type="checkbox"
            role="switch"
            checked={budget.checkpoint_enabled}
            disabled={busy != null}
            onChange={(event) => void save("run_budget.checkpoint_enabled", event.target.checked)}
          />
            </div>
          </div>
        </div>
      </section>

      <section className="settings-block" id="orchestration-skills">
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



function RequestAuditSection({ config, reload }: { config: AppConfig; reload: () => Promise<void> }) {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const enabled = config.diagnostics?.request_audit ?? false;

  const setAudit = async (value: boolean) => {
    setBusy(true);
    setErr(null);
    try {
      await settingsSet("diagnostics.request_audit", value);
      await reload();
    } catch (e) {
      setErr(errText(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="settings-block" id="request-audit-block">
      <h3>请求构成审计</h3>
      <p className="desc">
        开启后，每个会话发给模型的请求信封（工具清单、托管工具、输出上限）会写入旁路审计文件，
        用于核对请求构成；正式的会话记录不受影响。对新开始的会话生效。
      </p>
      {err && <div className="errbar" role="alert">{err}</div>}
      <div className="field">
        <label htmlFor="set-request-audit">旁路审计</label>
        <input
          id="set-request-audit"
          className="switch"
          type="checkbox"
          role="switch"
          checked={enabled}
          disabled={busy}
          onChange={(event) => void setAudit(event.target.checked)}
        />
        <span className="hint">审计文件按会话存放在应用数据目录的 sessions/request-audit/ 下。</span>
      </div>
    </section>
  );
}

function LogLevelSection({ config, reload }: { config: AppConfig; reload: () => Promise<void> }) {
  const [err, setErr] = useState<string | null>(null);

  const setLevel = async (v: string) => {
    setErr(null);
    try {
      await settingsSet("log_level", v);
      await reload();
      pushToast({ kind: "success", title: "日志级别已保存", timeout: 2_500 });
    } catch (e) {
      setErr(errText(e));
    }
  };

  return (
      <section className="settings-block" id="log-level-block">
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
    <section className="preference-section preference-appearance" id="appearance-block" aria-labelledby="appearance-heading">
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
    <section className="preference-section preference-companion" id="companion-block" aria-labelledby="companion-heading">
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
    <section className="settings-block" id="log-section">
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
    <section className="settings-block" id="support-block">
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

function CodexRuntimePreferences() {
  const [preferences, setPreferences] = useState<CodexCliPreferences | null>(null);
  const [draft, setDraft] = useState<CodexPreferenceDraft>({
    model: "",
    reasoningEffort: "",
    verbosity: "",
    permissionMode: "read_only",
  });
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

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

function CodexIntegrationSection({
  onStatusChange,
}: {
  /** Codex 状态每次刷新都会回调；用于联动子代理候选来源面板。 */
  onStatusChange?: (status: CodexIntegrationStatus) => void;
}) {
  const { runWithCodexCli } = useCodexCliGate();
  const [status, setStatus] = useState<CodexIntegrationStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [checking, setChecking] = useState(true);
  const [syncingCli, setSyncingCli] = useState(false);
  const [updateResult, setUpdateResult] = useState<CodexCliSyncResult | null>(null);
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
      onStatusChange?.(next);
      if (!quiet) setErr(null);
      return next;
    } catch (e) {
      if (!quiet) setErr(errText(e));
      return null;
    } finally {
      if (!quiet) setChecking(false);
    }
  }, [onStatusChange]);

  useEffect(() => {
    let active = true;
    const loadAndSync = async () => {
      // 进入页面先强制读取真实认证状态，不能沿用 Home/Room 可能较早的未登录快照。
      const initial = await refresh(false, true);
      if (!active || !initial?.cli_available) return;
      setSyncingCli(true);
      setUpdateResult(null);
      try {
        const result = await codexSyncCli();
        if (!active) return;
        setUpdateResult(result);
        setStatus(result.status);
        onStatusChange?.(result.status);
      } catch (e) {
        if (!active) return;
        // 自动更新是增强项；失败时保留刚刚确认可用的 CLI 与认证状态。
        setUpdateResult({
          update_state: "failed",
          previous_version: initial.cli_version,
          current_version: initial.cli_version,
          update_error: `自动更新检查失败：${errText(e)}。当前版本仍可使用。`,
          status: initial,
        });
      } finally {
        if (active) setSyncingCli(false);
      }
    };
    void loadAndSync();
    return () => {
      active = false;
    };
  }, [onStatusChange, refresh]);

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
        onStatusChange?.(next);
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
  const cliUpdateLabel = syncingCli
    ? "正在检查更新并自动升级…"
    : updateResult?.update_state === "updated"
      ? `已自动更新${updateResult.previous_version && updateResult.current_version
        ? `：${updateResult.previous_version} → ${updateResult.current_version}`
        : "到最新版本"}`
      : updateResult?.update_state === "up_to_date"
        ? "已是最新版本"
        : updateResult?.update_state === "failed"
          ? updateResult.update_error || "自动更新未完成，当前版本仍可使用。"
          : status?.cli_available
            ? "每次进入本页都会自动检查更新"
            : "安装后会在进入本页时自动检查更新";
  const mainDisabled = busy
    || checking
    || syncingCli
    || awaitingLogin
    || !status
    || setupState === "ready"
    || (setupState === "install_cli" && status.installer_available === false);
  const loginDisabled = busy
    || checking
    || syncingCli
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
    <section className="settings-block codex-setup" id="codex-setup-block">
      <div className="codex-setup-heading">
        <div>
          <h3 className="codex-setup-title">
            <span className="provider-icon-tile" aria-hidden="true"><img src={providerIconFor("codex_cli") ?? undefined} alt="" /></span>
            Codex 运行时
          </h3>
          <p className="desc">管理本机 Codex CLI 的版本、认证与编排接入。登录凭据始终由 Codex 管理，R-Code 不读取认证文件。</p>
        </div>
        <button
          className={`codex-status-refresh${checking || syncingCli ? " checking" : ""}`}
          disabled={busy || checking || syncingCli || awaitingLogin}
          onClick={() => void refresh()}
          aria-label="重新检测 Codex 状态"
          title="重新检测 Codex 状态"
        >
          <IconRefresh width={16} height={16} />
          <span>{checking || syncingCli ? "检测中…" : "重新检测"}</span>
        </button>
      </div>
      {err && <div className="errbar" role="alert">{err}</div>}
      <div className="codex-runtime-overview" aria-label="Codex 运行时状态">
        <article className={`codex-runtime-card${status?.cli_available ? " is-ready" : " is-attention"}`} data-testid="codex-runtime-card">
          <div className="codex-runtime-card-head">
            <span>运行时</span>
            <span className="codex-runtime-state">{status?.cli_available ? "可用" : checking ? "检测中" : "未安装"}</span>
          </div>
          <strong>{status?.cli_available ? status.cli_version || "Codex CLI" : "需要 Codex CLI"}</strong>
          <p className={updateResult?.update_state === "failed" ? "is-warning" : ""}>{cliUpdateLabel}</p>
          {status && !status.cli_available && (
            <button className="btn sm accent" disabled={mainDisabled} onClick={() => void completeSetup()}>
              {busy ? "正在处理…" : copy.action}
            </button>
          )}
        </article>

        <article className={`codex-runtime-card${authReady ? " is-ready" : " is-attention"}`} data-testid="codex-auth-card">
          <div className="codex-runtime-card-head">
            <span>认证</span>
            <span className="codex-runtime-state">{authReady ? "已确认" : setupState === "check" ? "待确认" : "待登录"}</span>
          </div>
          <strong>{loginLabel}</strong>
          <p>{authReady ? <>认证状态来自官方 <code>codex login status</code>。</> : "浏览器登录优先；回调受阻时使用设备码。"}</p>
          {!authReady && status?.cli_available && (
            <div className="codex-runtime-card-actions">
              {setupState === "check" ? (
                <button className="btn sm" disabled={busy || checking || syncingCli} onClick={() => void refresh()}>
                  重新检测
                </button>
              ) : (
                <>
                  <button className="btn sm accent" disabled={loginDisabled} title={loginDisabledReason} onClick={() => void startLogin("browser")}>
                    浏览器登录
                  </button>
                  <button className="btn sm ghost" disabled={loginDisabled} title={loginDisabledReason} onClick={() => void startLogin("device")}>
                    设备码
                  </button>
                </>
              )}
            </div>
          )}
        </article>

        <article className={`codex-runtime-card${collaborationReady ? " is-ready" : ""}`} data-testid="codex-collaboration-card">
          <div className="codex-runtime-card-head">
            <span>编排接入</span>
            <span className="codex-runtime-state">{collaborationReady ? "已连接" : authReady ? "待配置" : "等待认证"}</span>
          </div>
          <strong>{collaborationReady ? "R-Code 协作已连接" : "Skill 与 MCP"}</strong>
          <p>{collaborationReady ? "可用于跨引擎委派与 Codex 子代理。" : `Skill：${skillLabel} · MCP：${status?.mcp_server_configured ? "已启用" : "尚未启用"}`}</p>
          {authReady && !collaborationReady && (
            <button className="btn sm accent" disabled={mainDisabled} onClick={() => void completeSetup()}>
              {busy ? "正在配置…" : "完成协作配置"}
            </button>
          )}
        </article>
      </div>

      {notice && <p className="codex-inline-note" role="status"><IconCheck width={14} height={14} />{notice}</p>}
      {status?.cli_error && setupState === "install_cli" && <p className="codex-inline-warning">{status.cli_error}</p>}


      {authReady && !syncingCli && (
        <CodexRuntimePreferences />
      )}

      {status && (
        <details className="codex-advanced">
          <summary>路径与配置详情</summary>
          <div className="codex-advanced-body">
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

// ---------- M2-03：权限 / 安全 / 生命周期 ----------

const CODEX_PERMISSION_LABELS: Record<string, string> = {
  read_only: "仅查看",
  request_approval: "请求批准",
  auto_review: "替我审批",
  full_access: "完全访问权限",
  custom: "自定义（config.toml）",
};

/** Codex 子代理权限的唯一编辑面在「Agent 编排 → Codex 运行时」；
 * 本页是权威视图：显示当前生效值并提供深链，避免出现第二个编辑器。 */
function CodexPermissionSection() {
  const setActivePane = useAppStore((state) => state.setSettingsPane);
  const [mode, setMode] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    codexCliPreferences()
      .then((prefs) => { if (alive) setMode(prefs.permission_mode ?? "read_only"); })
      .catch((error) => { if (alive) setErr(errText(error)); });
    return () => { alive = false; };
  }, []);

  return (
    <div className="settings-sheet">
      <section className="settings-block" id="permissions-codex-block">
        <h3>Codex 子代理权限</h3>
        {err && <div className="errbar" role="alert">{err}</div>}
        <div className="field">
          <label htmlFor="perm-current-mode">当前生效模式</label>
          <input
            id="perm-current-mode"
            className="input"
            value={mode ? (CODEX_PERMISSION_LABELS[mode] ?? mode) : "读取中…"}
            readOnly
            aria-readonly="true"
          />
          <small className="settings-hint">
            五态（仅查看 / 请求批准 / 替我审批 / 完全访问 / 自定义）只有一个权威编辑入口：
            「Agent 编排 → Codex 运行时 → 子代理权限」。custom 表示检测到非预设 config.toml。
          </small>
        </div>
        <button
          type="button"
          className="btn primary"
          onClick={() => {
            setActivePane("agents");
            setTimeout(() => document.getElementById("codex-permission-mode")?.focus(), 60);
          }}
        >
          前往唯一编辑入口
        </button>
      </section>
    </div>
  );
}

/** 安全边界为只读强制状态：本页没有也不允许出现关闭安全边界的开关。 */
function SecuritySection() {
  const items = [
    { title: "密钥存储", body: "Provider 凭据仅保存在系统钥匙串/受保护配置中，界面与支持包只显示脱敏摘要。" },
    { title: "日志与支持包脱敏", body: "导出前强制清洗 API key、token、raw reasoning；该行为不可关闭。" },
    { title: "CSP / sandbox", body: "WebView 内容安全策略与 Tauri sandbox 为编译期强制状态，仅允许预览。" },
  ];
  return (
    <div className="settings-sheet">
      <section className="settings-block" id="security-forced-block">
        <h3>强制安全状态（只读）</h3>
        {items.map((item) => (
          <div className="field" key={item.title}>
            <label>🔒 {item.title}</label>
            <input className="input" value={`${item.title}：已强制启用`} readOnly aria-readonly="true" />
            <small className="settings-hint">{item.body}</small>
          </div>
        ))}
      </section>
    </div>
  );
}

/** 启动与关闭：close ask/hide/quit 状态机由 M3-01 的 Host 关闭合同落地后接入；
 * 在 Host 合同就绪前本页不提供任何伪控件。 */
function LifecycleSection() {
  const [behavior, setBehaviorState] = useState<string>("ask");
  const [loaded, setLoaded] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    closeBehaviorGet()
      .then((value) => { if (alive) { setBehaviorState(value); setLoaded(true); } })
      .catch((error) => { if (alive) setErr(errText(error)); });
    return () => { alive = false; };
  }, []);

  const setBehavior = async (value: string) => {
    setErr(null);
    try {
      await closeBehaviorSet(value);
      setBehaviorState(value);
      pushToast({ kind: "success", title: "关闭行为已保存", timeout: 2_500 });
    } catch (e) {
      setErr(errText(e));
    }
  };

  return (
    <div className="settings-sheet">
      <section className="settings-block" id="lifecycle-block">
        <h3>启动与关闭</h3>
        {err && <div className="errbar" role="alert">{err}</div>}
        <div className="field">
          <div style={{ display: "flex", gap: 8 }}>
            <button
              type="button"
              className="btn"
              onClick={() => void setBehavior("ask")}
              disabled={!loaded || behavior === "ask"}
            >
              重置为每次询问
            </button>
            <button
              type="button"
              className="btn danger"
              onClick={() => { void import("../../lib/ipc").then((m) => m.lifecycleExplicitQuit()); }}
            >
              立即退出应用
            </button>
          </div>
        </div>
        <div className="field">
          <label htmlFor="lifecycle-close-behavior">点击窗口关闭按钮时</label>
          <select
            id="lifecycle-close-behavior"
            className="input"
            value={behavior}
            disabled={!loaded}
            onChange={(e) => void setBehavior(e.target.value)}
          >
            <option value="ask">每次询问（推荐）</option>
            <option value="hide">最小化到后台继续运行</option>
            <option value="quit">直接退出</option>
          </select>
          <small className="settings-hint">
            选择「每次询问」时，关闭窗口会弹出确认：后台运行 / 退出 / 取消；
            退出前运行中的任务会收到统一收尾。选择为 Host 权威，立即生效。
          </small>
        </div>
      </section>
    </div>
  );
}
