/**
 * R22 — 原生配对（core 投影）：rcode://pair 载荷解析（与 Rust
 * parse_qr_payload 同规则）、错误态投影（过期/错指纹/不可达/权限拒绝）、
 * 相机权限请求语义。UI（RN 相机/PWA 输入）只消费本模块。
 */

export interface PairPayload {
  version: 1;
  host: string;
  port: number;
  pairSecret: string;
  fingerprint: string;
}

export type PairingFailure =
  | "expired"
  | "consumed"
  | "rejected"
  | "unreachable"
  | "fingerprint-mismatch"
  | "camera-denied";

export const pairingFailureCopy: Record<PairingFailure, string> = {
  expired: "配对码已过期——请在电脑端重新开始配对",
  consumed: "配对码已被使用——码只能用一次，请重新开始配对",
  rejected: "配对码不正确",
  unreachable: "连不上电脑——确认同一网络且防火墙放行",
  "fingerprint-mismatch": "主机身份校验失败——为安全起见已拒绝，请重新配对",
  "camera-denied": "未授权相机——可在系统设置中开启，或改用手动输入",
};

/** 严格解析（与 Rust parse_qr_payload 同一契约：R15 冻结 v1）。 */
export function parsePairPayload(text: string): PairPayload | null {
  if (!text.startsWith("rcode://pair?")) return null;
  const rest = text.slice("rcode://pair?".length);
  const fields = new Map<string, string>();
  const known = new Set(["v", "h", "p", "s", "fp"]);
  for (const pair of rest.split("&")) {
    const eq = pair.indexOf("=");
    if (eq < 0) return null;
    const [key, value] = [pair.slice(0, eq), pair.slice(eq + 1)];
    // 未知键/重复键 = 畸形载荷（与 Rust parse_qr_payload 同规则：fail closed）。
    if (!known.has(key) || fields.has(key)) return null;
    fields.set(key, value);
  }
  const version = Number(fields.get("v"));
  const port = Number(fields.get("p"));
  const host = fields.get("h") ?? "";
  const secret = fields.get("s") ?? "";
  const fp = fields.get("fp") ?? "";
  if (version !== 1 || !host || !Number.isInteger(port) || port <= 0 || !secret) return null;
  if (!/^[0-9a-fA-F]{64}$/.test(fp)) return null;
  return { version: 1, host, port, pairSecret: secret, fingerprint: fp.toLowerCase() };
}

/** 相机权限 → 用户语义（未授权不阻塞：改用手动输入路径）。 */
export function cameraPermissionOutcome(granted: boolean): {
  canScan: boolean;
  guidance: string;
} {
  return granted
    ? { canScan: true, guidance: "" }
    : { canScan: false, guidance: pairingFailureCopy["camera-denied"] };
}
