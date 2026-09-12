/**
 * R19.A3 — 直连/回落连接策略（纯 TS，PWA 与 RN 共享）：
 * 同网段探测成功 → 直连；失败/超时 → 回落中继；可强制中继。
 * 密语/指纹校验在配对层（R17/R22），本模块只决定端点次序。
 */

export interface ConnectionPlan {
  /** 拨入次序：LAN 直连优先（配置了强制中继时除外）。 */
  order: Array<"lan" | "relay">;
  reason: "direct-preferred" | "force-relay" | "relay-fallback" | "no-endpoints";
}

export interface StrategyInput {
  lanHost: string | null;
  lanPort: number | null;
  relayUrl: string | null;
  forceRelay: boolean;
}

export function planConnection(input: StrategyInput): ConnectionPlan {
  const hasLan = input.lanHost !== null && input.lanPort !== null;
  const hasRelay = input.relayUrl !== null && input.relayUrl !== "";
  if (!hasLan && !hasRelay) {
    return { order: [], reason: "no-endpoints" };
  }
  if (input.forceRelay) {
    return hasRelay
      ? { order: ["relay"], reason: "force-relay" }
      : { order: [], reason: "no-endpoints" };
  }
  if (hasLan) {
    return hasRelay
      ? { order: ["lan", "relay"], reason: "direct-preferred" }
      : { order: ["lan"], reason: "direct-preferred" };
  }
  return { order: ["relay"], reason: "relay-fallback" };
}

/** 拨入结果 → 下一步（直连失败自动回落；中继也失败才报错）。 */
export function nextAfterFailure(plan: ConnectionPlan, failed: "lan" | "relay"): ConnectionPlan {
  const remaining = plan.order.filter((hop) => hop !== failed);
  if (remaining.length === 0) {
    return { order: [], reason: failed === "lan" ? "relay-fallback" : plan.reason };
  }
  return { order: remaining, reason: failed === "lan" ? "relay-fallback" : plan.reason };
}

/** 设备凭据变更（吊销）时清空端点缓存：直连与中继都拒绝。 */
export function invalidateEndpoints(state: { lanHost: string | null; relayUrl: string | null }): void {
  state.lanHost = null;
  state.relayUrl = null;
}
