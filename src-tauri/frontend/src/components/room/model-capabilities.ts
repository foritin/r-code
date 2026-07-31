import type {
  AttachmentKind,
  CodexCliPreferences,
  InferenceOptions,
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
  const modelName = model.toLowerCase();
  const protocol = provider?.protocol;

  if (providerName.includes("deepseek") || modelName.includes("deepseek")) {
    return {
      thinking: { label: "思考模式", options: THINKING, defaultLabel: "服务默认" },
      reasoning: effort(["high", "max"]),
      note: "DeepSeek 思考模式下温度由服务忽略；高/最大映射到原生 reasoning_effort。",
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
 * 图片能力不能从 OpenAI/Anthropic 线路协议本身推出：同一端点可以同时承载文本与
 * 多模态模型。这里只认模型族的明确声明；其余返回 unknown，让请求真实尝试，避免
 * 把一个新发布的多模态模型误判成不可用。
 */
export function imageCapabilityFor(
  provider: ProviderChoice | undefined,
  model: string,
): ImageCapability {
  const providerName = provider?.name.toLowerCase() ?? "";
  const modelName = model.trim().toLowerCase();
  const modelLabel = model.trim() || provider?.model || "当前模型";

  const explicitVisionModel = [
    "vision",
    "qwen-vl",
    "qvq",
    "glm-4v",
    "glm-4.6v",
    "pixtral",
    "llava",
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
    parts.push(`思考${optionLabel(capabilities.thinking, inference.thinking)}`);
  }
  if (capabilities.reasoning && inference.reasoning_effort) {
    parts.push(optionLabel(capabilities.reasoning, inference.reasoning_effort));
  }
  if (capabilities.verbosity && inference.verbosity) {
    parts.push(optionLabel(capabilities.verbosity, inference.verbosity));
  }
  return parts.length > 0 ? parts.join(" · ") : "服务默认";
}
