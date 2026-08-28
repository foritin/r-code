// M3-04：Provider 连接健康共享视图（canonical configured+connectivity 快照的唯一投影）。
// 合同：configured 不冒充 connected；状态不只依赖颜色（文字+图形并存）；
// retry 仅 degraded/failed/unknown 可用；spinner 只属于 checking。

export type ConnectivityState = "unknown" | "checking" | "connected" | "degraded" | "failed";

export interface ConnectivityView {
  readonly state: ConnectivityState;
  readonly label: string;
  readonly glyph: string;
  readonly spinning: boolean;
  readonly retryable: boolean;
}

const VIEWS: Readonly<Record<ConnectivityState, ConnectivityView>> = {
  unknown: { state: "unknown", label: "未检测", glyph: "·", spinning: false, retryable: true },
  checking: { state: "checking", label: "检测中", glyph: "◐", spinning: true, retryable: false },
  connected: { state: "connected", label: "已连接", glyph: "✓", spinning: false, retryable: true },
  degraded: { state: "degraded", label: "受限", glyph: "△", spinning: false, retryable: true },
  failed: { state: "failed", label: "连接失败", glyph: "✕", spinning: false, retryable: true },
};

export function connectivityView(state: ConnectivityState): ConnectivityView {
  return VIEWS[state];
}

export const CONNECTIVITY_STATES: readonly ConnectivityState[] = [
  "unknown", "checking", "connected", "degraded", "failed",
];

/** configured（有凭据/已配置）不等于 connected（探测通过）。 */
export const CONFIGURED_LABEL = "已配置（未探测）";
