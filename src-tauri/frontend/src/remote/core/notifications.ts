/**
 * R13 — 前台通知投影（纯 TS）：approval.requested / run.completed 触发；
 * 正文通用化——绝不含命令内容/文件路径/密钥（F15）。浏览器无推送能力时
 * 降级为应用内横幅并诚实标注（canUsePush）。
 */

import type { EventEnvelope } from "./projection.ts";

export interface NotificationContent {
  title: string;
  body: string;
  /** 点击后的导航意图（PWA 前台路由；推送形态 R25 复用）。 */
  route: "approvals" | "task";
  taskId?: string;
}

/** 事件 → 通知内容；无关事件返回 null。正文快照测试锁定（F15）。 */
export function notificationForEvent(event: EventEnvelope): NotificationContent | null {
  const kind = event.payload?.journalKind;
  if (kind === "approval.requested") {
    return {
      title: "R-Code Remote",
      body: "有 1 项工具调用待审批",
      route: "approvals",
    };
  }
  if (kind === "run.completed") {
    return {
      title: "R-Code Remote",
      body: "一个会话已完成",
      route: "task",
      taskId: event.task_id,
    };
  }
  return null;
}

/** 通知正文的守卫断言输入（测试与运行时共用）：任何疑似敏感载荷出现即
 * 违反 F15——命令文本、路径分隔符、长 hex 令牌都不允许进入正文。 */
export function bodyIsSanitized(body: string): boolean {
  const suspicious = [/path/i, /command/i, /\.[a-z]{2,4}\b.*\//, /[0-9a-f]{32,}/i];
  return !suspicious.some((pattern) => pattern.test(body));
}

/** 前台通知能力检测（诚实降级：无 Notification/无权限 → 应用内横幅）。 */
export interface NotificationSupport {
  canUseSystemNotifications: boolean;
  fallbackBanner: string;
}

export function notificationSupport(hasNotificationApi: boolean, permission: string | null): NotificationSupport {
  if (!hasNotificationApi) {
    return {
      canUseSystemNotifications: false,
      fallbackBanner: "当前浏览器不支持系统通知，重要事件将以应用内横幅提示",
    };
  }
  if (permission !== "granted") {
    return {
      canUseSystemNotifications: false,
      fallbackBanner:
        permission === "denied"
          ? "系统通知已被拒绝——重要事件将以应用内横幅提示"
          : "允许通知以在后台收到审批与完成提醒",
    };
  }
  return { canUseSystemNotifications: true, fallbackBanner: "" };
}
