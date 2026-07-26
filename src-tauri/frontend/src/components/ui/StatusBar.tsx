/**
 * 统一的状态条。
 *
 * 取代 10 套各写各的实现（errbar / notebar / okbar / comp-error / deck-error /
 * home-error / panel-error / perm-error / srow-toast / room-provider-error）——
 * 它们的 danger 底色有 2 档、边框 4 档、字号 4 档、圆角 2 种，而且只有一半带
 * role="alert"，屏幕阅读器读不到另一半。
 */
import type { ReactNode } from "react";
import { IconAlert, IconCheck, IconClose } from "../icons";

type Kind = "error" | "warn" | "ok" | "info";

interface Props {
  kind?: Kind;
  children: ReactNode;
  /** 提供后右侧出现关闭按钮 */
  onDismiss?: () => void;
  /** 行动入口（"现在处理" 之类） */
  action?: { label: string; onClick: () => void; disabled?: boolean };
  /** 紧凑变体，用于侧栏行内与输入区 */
  compact?: boolean;
  className?: string;
  icon?: boolean;
}

const ROLE: Record<Kind, "alert" | "status"> = {
  error: "alert",
  warn: "status",
  ok: "status",
  info: "status",
};

export function StatusBar({
  kind = "info",
  children,
  onDismiss,
  action,
  compact = false,
  className,
  icon = true,
}: Props) {
  return (
    <div
      role={ROLE[kind]}
      aria-live={kind === "error" ? "assertive" : "polite"}
      className={
        `statusbar statusbar--${kind}` +
        (compact ? " statusbar--sm" : "") +
        (className ? ` ${className}` : "")
      }
    >
      {icon && (kind === "ok" ? <IconCheck width={14} height={14} /> : <IconAlert width={14} height={14} />)}
      <span className="statusbar-text">{children}</span>
      {action && (
        <button type="button" onClick={action.onClick} disabled={action.disabled}>
          {action.label}
        </button>
      )}
      {onDismiss && (
        <button type="button" className="iconbtn" onClick={onDismiss} aria-label="关闭提示">
          <IconClose width={12} height={12} />
        </button>
      )}
    </div>
  );
}
