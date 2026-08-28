// 产品能力开关的前端唯一读取与展示门控层（M1-02）。
//
// 契约：
// - 与 Host `src-tauri/src/feature_flags.rs::ProductFeatureFlags` 字段一一对应；
//   新增字段必须是带默认值的 additive 变更，未知字段安全忽略（向前兼容）。
// - 关闭的能力：UI 入口不可见；即使被强制触发，后端 require() 也会以
//   `<feature>.feature_disabled` 拒绝。前端隐藏只是第一层防线，不是授权边界。

/** 与 Rust ProductFeature 同名的稳定 key。 */
export type FeatureKey = "browser" | "automation" | "worktree";

export interface ProductFeatureFlags {
  readonly browser_enabled: boolean;
  readonly automation_enabled: boolean;
  readonly worktree_enabled: boolean;
}

export const DEFAULT_FEATURE_FLAGS: ProductFeatureFlags = {
  browser_enabled: false,
  automation_enabled: false,
  worktree_enabled: false,
};

/** 容错解析：布尔化已知字段、丢弃未知字段；非对象输入回退全关。 */
export function normalizeFeatureFlags(raw: unknown): ProductFeatureFlags {
  if (typeof raw !== "object" || raw === null || Array.isArray(raw)) {
    return { ...DEFAULT_FEATURE_FLAGS };
  }
  const record = raw as Record<string, unknown>;
  return {
    browser_enabled: record.browser_enabled === true,
    automation_enabled: record.automation_enabled === true,
    worktree_enabled: record.worktree_enabled === true,
  };
}

/** UI 层门控结果。visible=false 时 reasonCode 用于统一走结构化错误文案。 */
export interface FeatureEntryVisibility {
  readonly visible: boolean;
  readonly reasonCode?: string;
}

function entryVisibility(enabled: boolean, featureKey: FeatureKey): FeatureEntryVisibility {
  return enabled ? { visible: true } : { visible: false, reasonCode: `${featureKey}.feature_disabled` };
}

export function browserEntryVisibility(flags: ProductFeatureFlags): FeatureEntryVisibility {
  return entryVisibility(flags.browser_enabled, "browser");
}

export function automationEntryVisibility(flags: ProductFeatureFlags): FeatureEntryVisibility {
  return entryVisibility(flags.automation_enabled, "automation");
}

export function worktreeEntryVisibility(flags: ProductFeatureFlags): FeatureEntryVisibility {
  return entryVisibility(flags.worktree_enabled, "worktree");
}
