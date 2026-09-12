/**
 * R13 — 连接/错误引导投影（纯 TS）：技术错误 → 用户可行动文案。
 * 指纹不符 = 明确的重新配对引导（不是技术报错）。
 */

export interface ErrorGuidance {
  headline: string;
  detail: string;
  /** 修复动作：重新配对 / 直接重试 / 联系主机侧。 */
  action: "repair" | "retry" | "host";
}

/** RemoteClient/transport 的错误串 → 引导投影。 */
export function guidanceForError(error: string): ErrorGuidance {
  if (error.includes("fingerprint") || error.includes("pin mismatch")) {
    return {
      headline: "主机身份校验失败",
      detail: "这台电脑的证书与配对时不一致。为安全起见已拒绝连接——请在电脑端重新配对。",
      action: "repair",
    };
  }
  if (error.includes("revoked")) {
    return {
      headline: "本设备已被吊销",
      detail: "这台电脑已移除本设备的访问权。如需恢复，请在电脑端重新配对。",
      action: "repair",
    };
  }
  if (error.includes("unauthorized")) {
    return {
      headline: "认证失败",
      detail: "设备令牌不正确或已失效。请重新配对。",
      action: "repair",
    };
  }
  if (error.includes("无法到达主机") || error.includes("ConnectionRefused") || error.includes("timeout")) {
    return {
      headline: "连接不上电脑",
      detail: "请确认电脑在线、与你处于同一网络，且防火墙放行了该端口。",
      action: "host",
    };
  }
  return {
    headline: "出了点问题",
    detail: error,
    action: "retry",
  };
}
