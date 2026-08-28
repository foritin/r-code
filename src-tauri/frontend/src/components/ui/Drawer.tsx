import { useEffect, useId, useRef, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { useFocusTrap } from "../../lib/hooks";
import { IconClose } from "../icons";

interface Props {
  open: boolean;
  title: string;
  subtitle?: string;
  /** 标题左侧的可选标识（如 provider logo tile），由调用方渲染。 */
  icon?: ReactNode;
  /** 关闭按钮的无障碍名称（文案由调用方提供，本组件不内置文案）。 */
  closeLabel: string;
  onClose: () => void;
  /** 保存/测试进行中时锁定关闭入口（Esc、背板、按钮一并禁用）。 */
  closeDisabled?: boolean;
  footer?: ReactNode;
  children: ReactNode;
}

/**
 * 右侧滑入的编辑抽屉：ConfirmDialog 的姊妹壳——同样走 portal + useFocusTrap +
 * Esc / 背板关闭 + 焦点归还，只是面板形态从居中卡换成贴边竖栏。
 */
export function Drawer({
  open,
  title,
  subtitle,
  icon,
  closeLabel,
  onClose,
  closeDisabled = false,
  footer,
  children,
}: Props) {
  const titleId = useId();
  const panelRef = useRef<HTMLDivElement>(null);
  const returnFocusRef = useRef<HTMLElement | null>(null);
  const disabledRef = useRef(closeDisabled);
  const onCloseRef = useRef(onClose);
  disabledRef.current = closeDisabled;
  onCloseRef.current = onClose;
  useFocusTrap(panelRef, open);

  useEffect(() => {
    if (!open) return;
    returnFocusRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    panelRef.current?.focus();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !disabledRef.current) onCloseRef.current();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      const target = returnFocusRef.current;
      if (target && document.contains(target)) target.focus({ preventScroll: true });
    };
  }, [open]);

  if (!open) return null;

  return createPortal(
    <>
      <div
        className="drawer-backdrop"
        onPointerDown={(event) => {
          if (!closeDisabled && event.target === event.currentTarget) onClose();
        }}
      />
      <div
        ref={panelRef}
        className="drawer-panel"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
      >
        <header className="drawer-head">
          {icon && <div className="drawer-icon">{icon}</div>}
          <div className="drawer-titles">
            <h2 id={titleId}>{title}</h2>
            {subtitle && <p>{subtitle}</p>}
          </div>
          <button
            type="button"
            className="iconbtn drawer-close"
            disabled={closeDisabled}
            aria-label={closeLabel}
            onClick={onClose}
          >
            <IconClose width={14} height={14} />
          </button>
        </header>
        <div className="drawer-body">{children}</div>
        {footer && <footer className="drawer-foot">{footer}</footer>}
      </div>
    </>,
    document.body,
  );
}
