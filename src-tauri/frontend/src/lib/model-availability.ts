/**
 * pi-alignment M1-03：ModelAvailability 三态快照的前端纯逻辑。
 *
 * 独立成无副作用模块（只依赖 types）是为了让 node --test 能像
 * provider-health.ts 一样直接转译加载；provider.ts / SettingsScene 共用。
 */
import type { ModelAvailabilitySnapshot } from "./types";

/** 模型选择面的候选（provider.ts 的 choice 形状子集）。 */
export interface NamedProviderChoice {
  name: string;
}

/**
 * 模型选择面只渲染 available（有鉴权）的服务。
 *
 * - 快照覆盖（出现在 all）但没有 available 条目的服务整体退出选择面；
 * - 快照未覆盖（旧后端返回 null、或 provider 不在 all 里）不臆造过滤，
 *   保持原样——缺数据时宁可多展示也不静默清空用户的选择面。
 */
export function dropUnavailableProviders<T extends NamedProviderChoice>(
  choices: T[],
  snapshot: ModelAvailabilitySnapshot | null,
): T[] {
  if (!snapshot) return choices;
  const available = new Set(snapshot.available.map((entry) => entry.provider));
  const covered = new Set(snapshot.all.map((entry) => entry.provider));
  return choices.filter(
    (choice) => !covered.has(choice.name) || available.has(choice.name),
  );
}

/** 组装失败诊断清单：composition_errors 逐条（provider / model / reason）。 */
export function compositionDiagnostics(
  snapshot: ModelAvailabilitySnapshot | null,
): Array<{ provider: string; model: string | null; reason: string }> {
  if (!snapshot) return [];
  return snapshot.composition_errors.map((error) => ({
    provider: error.provider,
    model: error.model ?? null,
    reason: error.reason,
  }));
}
