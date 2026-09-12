/**
 * R07b — 远程连接状态机（纯 TS；PWA 与 RN 共享，无 DOM 依赖）。
 *
 * 状态：connecting → online；断线 → reconnecting（自动重连，指数退避，
 * 有倒计时秒数投影）；认证被拒 → denied；未配对（无凭据）→ unpaired。
 */

export type ConnectionState =
  | "unpaired"
  | "connecting"
  | "online"
  | "reconnecting"
  | "denied";

export interface ConnectionSnapshot {
  state: ConnectionState;
  /** reconnecting 时的下次尝试倒计时（秒，向上取整）；其余状态 0。 */
  retryInSeconds: number;
  /** 已尝试的重连次数（指数退避输入）。 */
  attempt: number;
}

const MAX_BACKOFF_SECONDS = 30;

export interface ConnectionInput {
  hasCredentials: boolean;
  online: boolean;
  authDenied: boolean;
}

export const initialSnapshot: ConnectionSnapshot = {
  state: "connecting",
  retryInSeconds: 0,
  attempt: 0,
};

/** 事件驱动的状态转移（纯函数）。 */
export function transition(
  snapshot: ConnectionSnapshot,
  event:
    | { type: "connected" }
    | { type: "disconnected" }
    | { type: "denied" }
    | { type: "retryNow" }
    | { type: "tick"; nowSeconds: number }
    | { type: "credentialsChanged"; hasCredentials: boolean },
): ConnectionSnapshot {
  switch (event.type) {
    case "credentialsChanged":
      if (!event.hasCredentials) {
        return { state: "unpaired", retryInSeconds: 0, attempt: 0 };
      }
      return {
        ...snapshot,
        state: snapshot.state === "unpaired" ? "connecting" : snapshot.state,
      };
    case "connected":
      return { state: "online", retryInSeconds: 0, attempt: 0 };
    case "denied":
      // 认证拒绝不自动重试（避免凭据风暴）；引导重新配对。
      return { state: "denied", retryInSeconds: 0, attempt: 0 };
    case "disconnected":
      if (snapshot.state === "denied" || snapshot.state === "unpaired") {
        return snapshot;
      }
      return advanceBackoff({ ...snapshot, state: "reconnecting" });
    case "retryNow":
      return {
        ...snapshot,
        state: "connecting",
        retryInSeconds: 0,
        attempt: snapshot.attempt + 1,
      };
    case "tick": {
      if (snapshot.state !== "reconnecting" || snapshot.retryInSeconds <= 0) {
        return snapshot;
      }
      const remaining = Math.max(0, event.nowSeconds - snapshot.retryInSeconds);
      if (remaining === 0) {
        // 倒计时归零：发起一次连接尝试（attempt 由 disconnected 退避
        // 步进负责，这里只切换状态）。
        return { ...snapshot, state: "connecting", retryInSeconds: 0 };
      }
      return { ...snapshot, retryInSeconds: remaining };
    }
  }
}

function advanceBackoff(snapshot: ConnectionSnapshot): ConnectionSnapshot {
  const attempt = snapshot.attempt + 1;
  // 指数退避 1,2,4,8,…封顶 30s。
  const backoff = Math.min(MAX_BACKOFF_SECONDS, 2 ** (attempt - 1));
  return { ...snapshot, state: "reconnecting", attempt, retryInSeconds: backoff };
}

/** UI 文案（屏③断网全屏态）。 */
export function stateBanner(snapshot: ConnectionSnapshot): string {
  switch (snapshot.state) {
    case "online":
      return "在线";
    case "connecting":
      return "连接中…";
    case "reconnecting":
      return `连接断开，${snapshot.retryInSeconds}s 后重试`;
    case "denied":
      return "认证被拒绝——请在电脑上重新配对";
    case "unpaired":
      return "尚未配对——请先在电脑端开始配对";
  }
}
