/**
 * R26 — 原生设置/诊断屏 core 投影（纯 TS，PWA 与 RN 共享，无平台 API）。
 *
 * 屏④六段：已授能力（只读，授予在桌面端）、中继与连接质量、连接策略
 * （直连优先 / 仅中继）、重连日志（最近 N 条可复制）、本机令牌（清除=
 * 本地注销）、关于与指纹核对（防中间人自查）。
 *
 * 安全姿态（F6/F10/F15）：能力行没有任何 action——App 内不可自授；模型
 * 不含令牌字段（设置屏永不展示凭据）；日志只存连接元数据且详情有长度上限。
 */

import type { DeviceCapabilities } from "./capability-ui.ts";
import { deviceInfoUi } from "./capability-ui.ts";
import type { ConnectionSnapshot } from "./connection-state.ts";
import { stateBanner } from "./connection-state.ts";
import type { ConnectionPlan } from "./connection-strategy.ts";

/** 屏④唯一允许的行级操作（能力授予不在其中——见文件头）。 */
export type SettingsAction =
  | { kind: "copy"; payload: string }
  | { kind: "clear-token" }
  | { kind: "switch-strategy"; forceRelay: boolean };

export interface SettingsRow {
  id: string;
  label: string;
  value: string;
  /** null = 纯只读行（能力行、关于行）。 */
  action: SettingsAction | null;
  /** 解释性副文案（如“在电脑端管理”）。 */
  hint?: string;
}

export type SettingsSectionId =
  | "capabilities"
  | "relay"
  | "strategy"
  | "diagnostics"
  | "session"
  | "about";

export interface SettingsSection {
  id: SettingsSectionId;
  title: string;
  rows: SettingsRow[];
}

/** 重连日志条目：连接元数据（F15——正文不含命令内容/路径/密钥）。 */
export interface ReconnectLogEntry {
  atMs: number;
  event: "connected" | "disconnected" | "denied" | "retry";
  detail: string;
}

/** 环形日志上限（最近 N 条）。 */
export const MAX_LOG_ENTRIES = 50;
/** 单条详情字符上限——防止把载荷/路径整段塞进诊断日志。 */
export const MAX_DETAIL_CHARS = 120;

export type ConnectionQuality = "offline" | "poor" | "fair" | "good";

export interface SettingsInput {
  capabilities: DeviceCapabilities;
  snapshot: ConnectionSnapshot;
  plan: ConnectionPlan;
  relayUrl: string | null;
  forceRelay: boolean;
  log: ReconnectLogEntry[];
  /** 主机证书指纹（配对时钉扎）；null = 未配对。 */
  fingerprint: string | null;
  /** 往返时延毫秒（最近一次心跳）；null = 未知。 */
  rttMs: number | null;
  deviceName: string | null;
  pairedAtMs: number | null;
  version: string;
}

/** 连接质量：RTT 分档（无 RTT 时按状态机兜底）。 */
export function connectionQuality(
  snapshot: ConnectionSnapshot,
  rttMs: number | null,
): { quality: ConnectionQuality; label: string } {
  if (snapshot.state !== "online") {
    return { quality: "offline", label: stateBanner(snapshot) };
  }
  if (rttMs === null) {
    return { quality: "fair", label: "在线（时延未知）" };
  }
  if (rttMs <= 100) return { quality: "good", label: `在线 · ${rttMs}ms` };
  if (rttMs <= 400) return { quality: "fair", label: `在线 · ${rttMs}ms` };
  return { quality: "poor", label: `在线 · ${rttMs}ms（网络较差）` };
}

/** 策略文案（直连优先 / 仅中继 / 无端点）。 */
export function strategyLabel(plan: ConnectionPlan, forceRelay: boolean): string {
  if (plan.order.length === 0) {
    return forceRelay ? "仅中继（未配置中继）" : "无可用端点";
  }
  if (forceRelay) return "仅中继";
  return plan.reason === "relay-fallback" ? "直连优先（已回落中继）" : "直连优先";
}

/** 截断详情（日志只留元数据）。 */
export function sanitizeDetail(detail: string): string {
  const flat = detail.replace(/\s+/g, " ").trim();
  return flat.length <= MAX_DETAIL_CHARS ? flat : `${flat.slice(0, MAX_DETAIL_CHARS - 1)}…`;
}

/** 追加一条日志（环形：超出上限丢弃最旧）。返回新数组（纯函数）。 */
export function appendLog(
  log: ReconnectLogEntry[],
  entry: ReconnectLogEntry,
  nowMs: number = entry.atMs,
): ReconnectLogEntry[] {
  const next = [
    ...log,
    { atMs: nowMs || entry.atMs, event: entry.event, detail: sanitizeDetail(entry.detail) },
  ];
  return next.length > MAX_LOG_ENTRIES ? next.slice(next.length - MAX_LOG_ENTRIES) : next;
}

/** 最近 N 条（新→旧）与可复制文本。 */
export function reconnectLog(
  log: ReconnectLogEntry[],
  limit: number = MAX_LOG_ENTRIES,
): { entries: ReconnectLogEntry[]; copyText: string } {
  const entries = log.slice(-Math.max(1, limit)).reverse();
  const copyText = entries.map((entry) => `${clockOf(entry.atMs)} ${entry.event} ${entry.detail}`).join("\n");
  return { entries, copyText };
}

function clockOf(atMs: number): string {
  const date = new Date(atMs);
  const pad = (value: number) => String(value).padStart(2, "0");
  return `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

/** 指纹核对投影（防中间人自查）：4 位一段，便于人工比对。 */
export function fingerprintDisplay(fingerprint: string): string {
  return (fingerprint.match(/.{1,4}/g) ?? []).join(" ");
}

/**
 * 清除本机令牌：凭据与端点缓存一并清空 → 连接状态机回 unpaired。
 * 桌面端吊销是另一回事（在电脑的设备管理里做），本函数只做本地注销。
 */
export function clearLocalCredentials(state: {
  hasCredentials: boolean;
  lanHost: string | null;
  relayUrl: string | null;
}): void {
  state.hasCredentials = false;
  state.lanHost = null;
  state.relayUrl = null;
}

/** 屏④模型：六段只读行 + 三个显式 action，无能力授予入口。 */
export function settingsModel(input: SettingsInput): SettingsSection[] {
  const quality = connectionQuality(input.snapshot, input.rttMs);
  const { entries, copyText } = reconnectLog(input.log);
  const capabilityRows: SettingsRow[] = deviceInfoUi(input.capabilities).map((row, index) => ({
    id: `capability-${index}`,
    label: row.label,
    value: row.value,
    action: null,
    hint: "能力只能在电脑端 R-Code 设备管理中授予或收回，本 App 不可自授。",
  }));

  return [
    {
      id: "capabilities",
      title: "设备能力",
      rows: capabilityRows,
    },
    {
      id: "relay",
      title: "中继",
      rows: [
        {
          id: "relay-url",
          label: "中继地址",
          value: input.relayUrl ?? "未配置（仅局域网直连）",
          action: input.relayUrl ? { kind: "copy", payload: input.relayUrl } : null,
        },
        {
          id: "relay-quality",
          label: "连接质量",
          value: quality.label,
          action: null,
          hint: `重连次数 ${input.snapshot.attempt}`,
        },
      ],
    },
    {
      id: "strategy",
      title: "连接策略",
      rows: [
        {
          id: "strategy-current",
          label: "当前策略",
          value: strategyLabel(input.plan, input.forceRelay),
          action: null,
        },
        {
          id: "strategy-toggle",
          label: "切换为",
          value: input.forceRelay ? "直连优先" : "仅中继",
          action: { kind: "switch-strategy", forceRelay: !input.forceRelay },
          hint: "仅影响本机拨入次序，不改变已授予的能力。",
        },
      ],
    },
    {
      id: "diagnostics",
      title: "诊断",
      rows: [
        {
          id: "diagnostics-log",
          label: `重连日志（最近 ${entries.length} 条）`,
          value: entries.length > 0 ? `${clockOf(entries[0].atMs)} ${entries[0].event} ${entries[0].detail}` : "暂无记录",
          action: entries.length > 0 ? { kind: "copy", payload: copyText } : null,
          hint: "仅连接元数据，不含命令内容与文件路径。",
        },
      ],
    },
    {
      id: "session",
      title: "本机令牌",
      rows: [
        {
          id: "session-clear",
          label: "清除本机令牌",
          value: "本地注销（电脑端吊销请在其设备管理操作）",
          action: { kind: "clear-token" },
          hint: "清除后本 App 回到未配对态，需要重新扫码配对。",
        },
      ],
    },
    {
      id: "about",
      title: "关于",
      rows: [
        { id: "about-version", label: "版本", value: input.version, action: null },
        {
          id: "about-device",
          label: "本机设备名",
          value: input.deviceName ?? "未命名设备",
          action: null,
        },
        {
          id: "about-paired-at",
          label: "配对时间",
          value: input.pairedAtMs ? new Date(input.pairedAtMs).toLocaleString() : "—",
          action: null,
        },
        {
          id: "about-fingerprint",
          label: "主机指纹（核对）",
          value: input.fingerprint ? fingerprintDisplay(input.fingerprint) : "未配对",
          action: input.fingerprint ? { kind: "copy", payload: input.fingerprint } : null,
          hint: "与电脑端显示的指纹逐段比对，不一致请立即重新配对。",
        },
      ],
    },
  ];
}

/** 模型内全部 action 的 kind 集合（供守卫测试断言无授予入口）。 */
export function actionKinds(model: SettingsSection[]): string[] {
  return model.flatMap((section) => section.rows.flatMap((row) => (row.action ? [row.action.kind] : [])));
}
