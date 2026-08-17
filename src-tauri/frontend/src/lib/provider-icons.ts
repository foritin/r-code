/**
 * Provider 厂商图标映射。
 *
 * 图标为各厂商官网 favicon 转存（设计阶段来源记录在 design/icons/sources.md），
 * 仅作识别用途；未覆盖的厂商返回 null，调用方回退到字母 tile。
 */
import anthropicIcon from "../assets/providers/anthropic.png";
import arkIcon from "../assets/providers/ark.png";
import deepseekIcon from "../assets/providers/deepseek.png";
import kimiIcon from "../assets/providers/kimi.png";
import openaiIcon from "../assets/providers/openai.png";
import zhipuIcon from "../assets/providers/zhipu.png";

/** 按预设 id / provider_kind 归一化到图标。同一厂商的变体线路共用同一图标。 */
const ICON_BY_KIND: Record<string, string> = {
  anthropic: anthropicIcon,
  openai: openaiIcon,
  azure_openai: openaiIcon,
  deepseek: deepseekIcon,
  deepseek_anthropic: deepseekIcon,
  kimi: kimiIcon,
  kimi_coding: kimiIcon,
  zhipu: zhipuIcon,
  zhipu_coding: zhipuIcon,
  zai: zhipuIcon,
  ark: arkIcon,
  ark_coding: arkIcon,
  ark_coding_openai: arkIcon,
  ark_agent: arkIcon,
  byteplus: arkIcon,
  codex_cli: openaiIcon,
};

export function providerIconFor(kind: string | null | undefined): string | null {
  if (!kind) return null;
  return ICON_BY_KIND[kind] ?? null;
}

/** 回退字母 tile 的取字符：取名字里第一个可见字符。 */
export function providerInitial(label: string): string {
  const trimmed = label.trim();
  return trimmed ? [...trimmed][0].toUpperCase() : "?";
}
