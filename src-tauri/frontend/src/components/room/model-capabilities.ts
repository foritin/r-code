import type {
  AttachmentKind,
  CodexCliPreferences,
  InferenceOptions,
  PresetModelInfo,
  TaskAgentEngine,
} from "../../lib/types";
import type { ProviderChoice } from "../../lib/provider";

export interface CapabilityOption {
  value: string;
  label: string;
  description?: string;
}

export interface CapabilityControl {
  label: string;
  options: CapabilityOption[];
  defaultLabel: string;
  /**
   * 本地默认策略与某个显式选项等价时，用它合并菜单入口。保存时仍清空字段，
   * 既兼容旧会话，也避免把 R-Code 的本地策略值误传给模型服务。
   */
  defaultValue?: string;
}

export interface ModelCapabilities {
  thinking?: CapabilityControl;
  reasoning?: CapabilityControl;
  verbosity?: CapabilityControl;
  note?: string;
}

export type ImageCapabilityState = "supported" | "unsupported" | "unknown";

export interface ImageCapability {
  state: ImageCapabilityState;
  modelLabel: string;
  reason: string;
}

const THINKING: CapabilityOption[] = [
  { value: "enabled", label: "开启", description: "让模型先推理再回答" },
  { value: "disabled", label: "关闭", description: "优先低延迟直接回答" },
];

const DEEPSEEK_THINKING: CapabilityOption[] = [
  {
    value: "adaptive",
    label: "智能平衡（推荐）",
    description: "R-Code 按任务阶段自动平衡响应速度与推理质量",
  },
  { value: "enabled", label: "始终开启", description: "每轮都保持所选推理强度" },
  { value: "disabled", label: "关闭", description: "关闭深度思考，优先响应速度" },
];

const EFFORT_LABELS: Record<string, string> = {
  none: "无",
  minimal: "最少",
  low: "低",
  medium: "中等",
  high: "高",
  xhigh: "极高",
  max: "最大",
  ultra: "超强",
};

const VERBOSITY: CapabilityOption[] = [
  { value: "low", label: "简洁" },
  { value: "medium", label: "适中" },
  { value: "high", label: "详细" },
];

function effort(values: string[], defaultLabel = "服务默认"): CapabilityControl {
  return {
    label: "推理强度",
    defaultLabel,
    options: values.map((value) => ({ value, label: EFFORT_LABELS[value] ?? value })),
  };
}

/**
 * 只为厂商已公开支持的参数开放入口。未知兼容网关保持“服务默认”，避免 UI
 * 生成 Provider 会拒绝的字段。模型名判断是自定义 Provider 的保守兜底。
 */
export function capabilitiesFor(provider: ProviderChoice | undefined, model: string): ModelCapabilities {
  const providerName = provider?.name.toLowerCase() ?? "";
  const providerKind = provider?.kind?.toLowerCase() ?? "";
  const modelName = model.toLowerCase();
  const protocol = provider?.protocol;

  if (providerKind === "deepseek") {
    const isFlash = modelName === "deepseek-v4-flash";
    const reasoningValues = isFlash ? ["low", "high", "max"] : ["high", "max"];
    return {
      thinking: {
        label: "思考模式",
        options: DEEPSEEK_THINKING,
        defaultLabel: "智能平衡（推荐）",
        defaultValue: "adaptive",
      },
      reasoning: effort(reasoningValues, "跟随智能平衡"),
      note: isFlash
        ? "智能平衡由 R-Code 按阶段调节；Flash 支持低/高/最大。选择固定强度或始终开启/关闭会退出自动调节。"
        : "智能平衡由 R-Code 按阶段调节；Pro 仅支持高/最大，不会发送不受支持的低档。选择固定强度或始终开启/关闭会退出自动调节。",
    };
  }

  if (providerKind === "kimi_coding") {
    return {
      thinking: {
        label: "思考模式",
        options: [{ value: "enabled", label: "始终思考" }],
        defaultLabel: "服务默认（持续思考）",
      },
      reasoning: effort(["low", "high", "max"]),
      note: "K3/K3-256k 始终思考；推理强度仅支持低/高/最大，且不会发送 temperature。",
    };
  }

  if (providerKind === "kimi") {
    return {
      thinking: { label: "思考模式", options: THINKING, defaultLabel: "服务默认" },
      reasoning: effort(["low", "medium", "high"]),
      note: "Kimi 的思考开关与推理强度会通过当前 Anthropic 兼容线路发送；留空时沿用服务默认。",
    };
  }

  if (
    providerKind === "ark_coding"
    || providerKind === "ark_coding_openai"
    || providerKind === "ark_agent"
  ) {
    const responses = protocol === "openai_responses";
    return {
      thinking: {
        label: "思考模式",
        options: [
          { value: "adaptive", label: "自适应(auto)" },
          { value: "enabled", label: "始终开启" },
          { value: "disabled", label: "关闭" },
        ],
        defaultLabel: "自适应(auto)",
        defaultValue: "adaptive",
      },
      reasoning: effort(
        responses ? ["low", "medium", "high", "xhigh", "max"] : ["minimal", "low", "medium", "high"],
      ),
      note: responses
        ? "Responses 口按 reasoning_effort 发送；不支持 none/minimal，关闭思考时省略该参数。"
        : "Anthropic 套餐口不发送推理强度；OpenAI 兼容口按 reasoning_effort 发送。",
    };
  }

  if (providerName === "anthropic" || modelName.includes("claude")) {
    return {
      reasoning: effort(["low", "medium", "high", "xhigh", "max"]),
      note: "选择推理强度后自动使用 Claude 自适应思考；留空时沿用服务默认。",
    };
  }

  if (providerName === "xai" || modelName.includes("grok")) {
    return {
      reasoning: effort(["low", "medium", "high"]),
      note: "Grok 推理模型始终进行推理，强度默认由服务决定。",
    };
  }

  if (providerName.includes("gemini") || modelName.includes("gemini")) {
    const values = modelName.includes("pro") || modelName.includes("gemini-3")
      ? ["low", "medium", "high"]
      : ["none", "low", "medium", "high"];
    return { reasoning: effort(values), note: "通过 OpenAI 兼容层映射到 Gemini thinking level。" };
  }

  if (modelName.includes("qwen") || providerName.includes("bailian")) {
    return {
      thinking: { label: "思考模式", options: THINKING, defaultLabel: "服务默认" },
      note: "混合思考模型使用 enable_thinking；未设置时沿用百炼服务默认。",
    };
  }

  if (modelName.includes("glm") || providerName === "zhipu" || providerName === "zai") {
    return {
      thinking: { label: "思考模式", options: THINKING, defaultLabel: "服务默认" },
      note: "仅向兼容接口发送原生 thinking 开关。",
    };
  }

  const openAiFamily = providerName === "openai"
    || providerName === "codex"
    || providerName.includes("azure")
    || modelName.startsWith("gpt-5")
    || modelName.startsWith("o1")
    || modelName.startsWith("o3")
    || modelName.startsWith("o4");
  if (openAiFamily) {
    const values = modelName.includes("5.6")
      ? ["none", "low", "medium", "high", "xhigh", "max"]
      : ["none", "low", "medium", "high", "xhigh"];
    return {
      reasoning: effort(values),
      verbosity: { label: "输出详略", options: VERBOSITY, defaultLabel: "服务默认" },
      note: protocol === "openai_responses"
        ? "使用 Responses 原生 reasoning 与 text 配置。"
        : "使用 OpenAI 兼容接口的推理强度与输出详略。",
    };
  }

  return { note: "该模型未声明可调推理参数；当前沿用模型服务默认值。" };
}

/**
 * 图片能力判定（C2 统一出口）。优先级：
 * 1. 预设目录标注（provider + model 精确命中）→ 权威（人工核对的一手信息）；
 * 2. Codex CLI 目录 supports_images → 权威（见 codexImageCapability）；
 * 3. 现有名称启发式 → 兜底；
 * 4. 其余 → unknown（让请求真实尝试，避免误判新模型）。
 *
 * 图片能力不能从 OpenAI/Anthropic 线路协议本身推出：同一端点可以同时承载文本与
 * 多模态模型。
 */
export function imageCapabilityFor(
  provider: ProviderChoice | undefined,
  model: string,
): ImageCapability {
  const providerName = provider?.name.toLowerCase() ?? "";
  const modelName = model.trim().toLowerCase();
  const modelLabel = model.trim() || provider?.model || "当前模型";

  // 1) 预设目录标注：人工核对的一手信息，权威（由 ProviderChoice 注入，避免本
  // 模块反向依赖 provider 目录装载）。
  const annotated = findPresetModelAnnotation(provider?.presetModels, model);
  if (annotated != null) {
    return annotated
      ? { state: "supported", modelLabel, reason: `${modelLabel} 支持图片输入。` }
      : {
          state: "unsupported",
          modelLabel,
          reason: `${modelLabel} 没有声明图片输入能力；图片会保留在草稿中，但不会发送。`,
        };
  }

  // 2) Codex CLI 目录由 codexImageCapability 处理（调用方按 agentEngine 分派）。
  // 3) 名称启发式兜底：目录未命中的同步/手填模型。
  const explicitVisionModel = [
    "vision",
    "qwen-vl",
    "qvq",
    "glm-4v",
    "glm-4.6v",
    "pixtral",
    "llava",
    "doubao-seed",
  ].some((needle) => modelName.includes(needle));
  if (explicitVisionModel) {
    return { state: "supported", modelLabel, reason: `${modelLabel} 声明了视觉输入能力。` };
  }

  if (
    providerName === "anthropic"
    || modelName.includes("claude")
    || providerName.includes("gemini")
    || modelName.includes("gemini")
    || modelName.startsWith("gpt-4o")
    || modelName.startsWith("gpt-4.1")
    || modelName.startsWith("gpt-5")
    || modelName.startsWith("o1")
    || modelName.startsWith("o3")
    || modelName.startsWith("o4")
  ) {
    return { state: "supported", modelLabel, reason: `${modelLabel} 支持图片输入。` };
  }

  const explicitlyTextOnly =
    (modelName.includes("glm-") && !explicitVisionModel)
    || modelName.includes("deepseek")
    || modelName.includes("ark-code")
    || modelName.includes("code-latest")
    || modelName.includes("codex-spark");
  if (explicitlyTextOnly) {
    return {
      state: "unsupported",
      modelLabel,
      reason: `${modelLabel} 没有声明图片输入能力；图片会保留在草稿中，但不会发送。`,
    };
  }

  return {
    state: "unknown",
    modelLabel,
    reason: `${modelLabel} 没有提供可读取的图片能力声明；R-Code 会尝试发送。`,
  };
}

/** Codex CLI 的模型目录明确提供 input_modalities，优先使用真实声明。 */
export function codexImageCapability(preferences: CodexCliPreferences | null): ImageCapability {
  const configured = preferences?.model
    ? preferences.models.find((option) => option.slug === preferences.model)
    : undefined;
  const effective = configured ?? preferences?.models[0];
  const modelLabel = effective?.display_name ?? preferences?.model ?? "Codex 默认模型";
  if (!effective) {
    return {
      state: "unknown",
      modelLabel,
      reason: "Codex CLI 尚未返回模型能力目录；R-Code 会尝试发送图片。",
    };
  }
  if (effective.supports_images == null) {
    return {
      state: "unknown",
      modelLabel,
      reason: `${modelLabel} 的 Codex 模型目录没有图片能力字段；R-Code 会尝试发送。`,
    };
  }
  return effective.supports_images
    ? { state: "supported", modelLabel, reason: `${modelLabel} 的 Codex 模型目录声明支持图片输入。` }
    : {
        state: "unsupported",
        modelLabel,
        reason: `${modelLabel} 不支持图片；图片会保留在草稿中，但不会发送。`,
      };
}

/**
 * 当前主模型是否**目录确认**多模态（决定图片是否原图直发、跳过理解引擎）：
 * - Codex 主 Agent：看 Codex 模型目录的 supports_images（选中模型或目录首项）；
 * - R-Code：只认预设目录 vision === true 的精确标注；启发式 supported 与能力
 *   未知（同步/手填/中转）都不算确认，仍按配置引擎分派。
 */
export function mainModelVisionConfirmed(options: {
  agentEngine: TaskAgentEngine;
  codexPreferences?: CodexCliPreferences | null;
  provider?: ProviderChoice;
  model: string;
}): boolean {
  if (options.agentEngine === "codex") {
    const preferences = options.codexPreferences ?? null;
    const slug = preferences?.model ?? preferences?.models[0]?.slug;
    const selected = preferences?.models.find((option) => option.slug === slug);
    return selected?.supports_images === true;
  }
  return findPresetModelAnnotation(options.provider?.presetModels, options.model) === true;
}

/** 预设目录命中时返回其能力标注（true=多模态）；未命中返回 undefined。 */
export function findPresetModelAnnotation(
  presetModels: PresetModelInfo[] | undefined,
  model: string,
): boolean | null | undefined {
  return presetModels?.find((candidate) => candidate.id === model.trim())?.vision;
}

/**
 * C2 三态统一出口（设置页 / 图片理解配置用）：
 * 1) 预设目录标注（精确命中）→ 权威；
 * 2) 名称启发式 → 兜底；
 * 3) 其余 → unknown。
 * Codex 目录命中由调用方在 agentEngine == "codex" 时优先走 codexImageCapability。
 */
export function resolveImageCapability(
  model: string,
  opts?: { presetModels?: PresetModelInfo[] },
): ImageCapabilityState {
  const annotated = findPresetModelAnnotation(opts?.presetModels, model);
  if (annotated != null) return annotated ? "supported" : "unsupported";
  const heuristic = imageCapabilityFor(undefined, model);
  return heuristic.state;
}

/** 模型候选徽标：未知能力不加标（前端下拉后缀用）。 */
export function modalityLabel(cap: ImageCapabilityState): "多模态" | "文本" | null {
  if (cap === "supported") return "多模态";
  if (cap === "unsupported") return "文本";
  return null;
}

/** 每个附件独立判定，避免一张不支持的图片把普通代码文件也一起划掉。 */
export function attachmentCapabilityFor(
  kind: AttachmentKind,
  imageCapability: ImageCapability,
  agentEngine: TaskAgentEngine,
  provider?: ProviderChoice,
): ImageCapability {
  if (kind === "image") return imageCapability;
  if (kind === "text") {
    return {
      state: "supported",
      modelLabel: "当前 Agent",
      reason: "文本、代码和结构化文本会作为带文件名的上下文发送。",
    };
  }
  if (agentEngine === "codex") {
    return {
      state: "supported",
      modelLabel: "Codex CLI",
      reason: "PDF 会作为本轮临时本地文件交给 Codex CLI 读取。",
    };
  }
  if (provider?.protocol === "anthropic_messages" || provider?.protocol === "openai_responses") {
    return {
      state: "supported",
      modelLabel: provider.label,
      reason: `${provider.label} 当前线路支持原生 PDF 文件输入。`,
    };
  }
  return {
    state: "unsupported",
    modelLabel: provider?.label ?? "当前模型服务",
    reason: `${provider?.label ?? "当前模型服务"} 的线路不能直接读取 PDF；文件会保留在草稿中，请切换到 Responses、Anthropic 或 Codex。`,
  };
}

export function normalizeInference(
  inference: InferenceOptions | null | undefined,
  capabilities: ModelCapabilities,
): InferenceOptions {
  const next: InferenceOptions = {};
  const thinking = inference?.thinking ?? undefined;
  if (thinking && capabilities.thinking?.options.some((option) => option.value === thinking)) {
    next.thinking = thinking;
  }
  const reasoning = inference?.reasoning_effort ?? undefined;
  if (reasoning && capabilities.reasoning?.options.some((option) => option.value === reasoning)) {
    next.reasoning_effort = reasoning;
    // Historical sessions may have stored an effort without a thinking mode. For DeepSeek an
    // effort is an explicit fixed-depth preference, so normalize the visible state to Always On
    // instead of falsely labelling the same request as Smart Balance.
    if (
      capabilities.thinking?.defaultValue === "adaptive"
      && (!next.thinking || next.thinking === capabilities.thinking.defaultValue)
    ) {
      next.thinking = "enabled";
    }
  }
  const verbosity = inference?.verbosity ?? undefined;
  if (verbosity && capabilities.verbosity?.options.some((option) => option.value === verbosity)) {
    next.verbosity = verbosity;
  }
  return next;
}

export function optionLabel(control: CapabilityControl | undefined, value: string | null | undefined): string {
  if (!control || !value) return control?.defaultLabel ?? "服务默认";
  return control.options.find((option) => option.value === value)?.label ?? value;
}

export function inferenceSummary(capabilities: ModelCapabilities, inference: InferenceOptions): string {
  const parts: string[] = [];
  if (capabilities.thinking && inference.thinking) {
    const label = optionLabel(capabilities.thinking, inference.thinking);
    parts.push(inference.thinking === capabilities.thinking.defaultValue ? label : `思考${label}`);
  } else if (capabilities.thinking?.defaultValue) {
    parts.push(capabilities.thinking.defaultLabel);
  }
  if (capabilities.reasoning && inference.reasoning_effort) {
    parts.push(optionLabel(capabilities.reasoning, inference.reasoning_effort));
  }
  if (capabilities.verbosity && inference.verbosity) {
    parts.push(optionLabel(capabilities.verbosity, inference.verbosity));
  }
  return parts.length > 0 ? parts.join(" · ") : "服务默认";
}
