// M4-03：执行台信息架构（overview / subagents / changes）。
//
// 合同（R-WB-01/02、§4.4）：
// - 一级 tab 集合精确为这三个，无重复全局工具审计；
// - 自动聚焦优先级：Attention > active child > changes ready > overview；
// - 用户手动 tab 选择在本 Run 生命周期内保持（auto 规则只在首次打开/Attention 变化时介入）；
// - 打开/切换/抽屉化不触碰 OS 窗口几何。

export type WorkbenchIATab = "overview" | "subagents" | "changes";

export const WORKBENCH_TABS: readonly WorkbenchIATab[] = ["overview", "subagents", "changes"];

export interface WorkbenchFacts {
  readonly attentionCount: number;
  readonly activeChildCount: number;
  readonly changesReady: boolean;
}

/** 自动选择：Attention > active child > changes ready > overview。 */
export function autoSelectTab(facts: WorkbenchFacts): WorkbenchIATab {
  if (facts.attentionCount > 0) return "subagents";
  if (facts.activeChildCount > 0) return "subagents";
  if (facts.changesReady) return "changes";
  return "overview";
}

/**
 * 打开执行台时的初始 tab：
 * - 有用户手动选择（userTab 非 null）→ 保持用户选择；
 * - 否则按自动规则。
 */
export function initialTab(facts: WorkbenchFacts, userTab: WorkbenchIATab | null): WorkbenchIATab {
  return userTab ?? autoSelectTab(facts);
}

/** 抽屉/固定栏只是呈现容器：二者使用同一 tab 集与同一状态源。 */
export function isWorkbenchIATab(v: unknown): v is WorkbenchIATab {
  return typeof v === "string" && (WORKBENCH_TABS as readonly string[]).includes(v);
}
