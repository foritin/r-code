/**
 * R25 — fake push adapter（core）：APNs/FCM 真实投递前，推送以 fake
 * adapter 全闭环验证——句柄登记/触发/吊销/前台降级；正文快照断言不含
 * 命令内容/路径/密钥（F15）。真实 APNs/FCM 归 production_release_ready。
 */

import { notificationForEvent } from "./notifications.ts";
import type { EventEnvelope } from "./projection.ts";

export interface PushHandle {
  platform: "ios" | "android";
  token: string;
  deviceId: string;
}

export interface PushNotification {
  handle: PushHandle;
  title: string;
  body: string;
  /** F15 守卫：正文必须通过 sanitize 检查才入栈。 */
  sanitized: boolean;
}

export type PushEvent =
  | { type: "registered"; handle: PushHandle }
  // R25.A1/A2 的验收证据按扁平字段断言（handle/body/sanitized），
  // 与 push() 实际入栈的形状一致；PushNotification 描述同一形状。
  | ({ type: "sent" } & PushNotification)
  | { type: "dropped-foreground"; handle: PushHandle }
  | { type: "revoked"; deviceId: string };

/** Fake adapter：测试构建注入；事件序列即验收证据。 */
export class FakePushAdapter {
  readonly events: PushEvent[] = [];
  private handles = new Map<string, PushHandle>(); // deviceId → handle
  private foreground = false;

  /** 前台状态（App 可见时推送降级为页内通知）。 */
  setForeground(isForeground: boolean): void {
    this.foreground = isForeground;
  }

  /** 设备登记句柄（随设备登记，可吊销；无账号）。 */
  register(handle: PushHandle): void {
    this.handles.set(handle.deviceId, handle);
    this.events.push({ type: "registered", handle });
  }

  /** 句柄失效（App 卸载等）：清理。 */
  revoke(deviceId: string): void {
    if (this.handles.delete(deviceId)) {
      this.events.push({ type: "revoked", deviceId });
    }
  }

  /** daemon 事件 → 推送（正文经 notificationForEvent 投影，F15）。 */
  push(event: EventEnvelope): void {
    const content = notificationForEvent(event);
    if (!content) return;
    for (const handle of this.handles.values()) {
      if (this.foreground) {
        this.events.push({ type: "dropped-foreground", handle });
        continue;
      }
      this.events.push({
        type: "sent",
        handle,
        title: content.title,
        body: content.body,
        sanitized: bodyIsClean(content.body),
      });
    }
  }

  get registeredCount(): number {
    return this.handles.size;
  }
}

/** 正文守卫（与 notifications.bodyIsSanitized 同规则，独立导出避免依赖环）。 */
export function bodyIsClean(body: string): boolean {
  const suspicious = [/path/i, /command/i, /\.[a-z]{2,4}\b.*\//, /[0-9a-f]{32,}/i];
  return !suspicious.some((pattern) => pattern.test(body));
}
