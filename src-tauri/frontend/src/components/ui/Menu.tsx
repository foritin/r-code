/**
 * 统一的下拉/弹层组件。
 *
 * 取代原先 8 处手写实现（MenuBar 菜单、ProjectAccessSelector、HomeScene 的
 * scope/provider 菜单、RoomScene 的 ProviderSwitcher、Composer 的 more-menu 与
 * 队列弹层、@ 文件补全）。那 8 处的行为矩阵是：
 *   - 点击外部关闭：7/8（其中 ProviderSwitcher 完全没有），mousedown 与
 *     pointerdown 混用，监听器有的常驻、有的按需
 *   - Escape 关闭：4/8
 *   - 方向键导航：1/8
 *   - 关闭后焦点归还触发器：0/8
 *   - role="menu" 下子项带 menuitem：5/8
 *
 * 定位用 portal + position:fixed，不用 absolute。原因：`.convo` 是
 * `overflow: hidden` 的 flex 列，绝对定位的弹层一旦超出左列边界就会被裁掉半个字。
 * 挂到 body 上还顺带解决了跨 stacking context 的层级问题 —— 弹层永远在最上层，
 * 不再依赖祖先的 z-index 是否够大。
 */
import {
  cloneElement,
  isValidElement,
  useCallback,
  useEffect,
  useId,
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type ReactElement,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";


/** 全局互斥：打开新菜单时关掉上一个。 */
let closeActive: (() => void) | null = null;

const ITEM_SELECTOR = '[role="menuitem"],[role="menuitemradio"],[role="option"]';
const GAP = 6;
const MARGIN = 8; // 与视口边缘的最小距离

export interface MenuRenderApi {
  close: () => void;
}

interface Props {
  /** 触发器。会被注入 ref / onClick / aria-* —— 必须是能接收这些 props 的元素。 */
  trigger: ReactElement;
  children: ReactNode | ((api: MenuRenderApi) => ReactNode);
  /** 首选展开方向；空间不足时自动翻转 */
  placement?: "up" | "down";
  /** 水平对齐基准边 */
  align?: "left" | "right";
  /** 弹层的可访问名称 */
  label?: string;
  /** dialog 用于非菜单式内容（如队列列表） */
  role?: "menu" | "dialog" | "listbox";
  disabled?: boolean;
  /** 弹层附加类名（宽度等场景差异） */
  menuClassName?: string;
  /** 包裹层附加类名 */
  className?: string;
  /** 内容可滚动（长列表） */
  scroll?: boolean;
  onOpenChange?: (open: boolean) => void;
}

export function Menu({
  trigger,
  children,
  placement = "down",
  align = "left",
  label,
  role = "menu",
  disabled = false,
  menuClassName,
  className,
  scroll = false,
  onOpenChange,
}: Props) {
  const [open, setOpen] = useState(false);
  const [style, setStyle] = useState<CSSProperties>({ visibility: "hidden" });
  const rootRef = useRef<HTMLDivElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLElement | null>(null);
  const menuId = useId();

  const close = useCallback((returnFocus = true) => {
    setOpen((wasOpen) => {
      if (!wasOpen) return wasOpen;
      if (returnFocus) triggerRef.current?.focus();
      return false;
    });
  }, []);

  useEffect(() => {
    onOpenChange?.(open);
    if (!open) return;
    closeActive?.();
    closeActive = () => close(false);
    return () => {
      if (closeActive) closeActive = null;
    };
    // onOpenChange 由调用方保证稳定；close 恒定
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  useEffect(() => {
    if (disabled && open) close(false);
  }, [disabled, open, close]);

  // 点击外部：portal 之后弹层不再是触发器的 DOM 后代，两边都要放行
  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node;
      if (rootRef.current?.contains(target)) return;
      if (menuRef.current?.contains(target)) return;
      close(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.stopPropagation();
        close();
      }
    };
    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      document.removeEventListener("keydown", onKeyDown);
    };
  }, [open, close]);

  /** 由触发器的视口坐标算出 fixed 位置，空间不足时翻转，最后钳进视口。 */
  const reposition = useCallback(() => {
    const anchor = triggerRef.current;
    const menu = menuRef.current;
    if (!anchor || !menu) return;

    const rect = anchor.getBoundingClientRect();
    const vw = document.documentElement.clientWidth;
    const vh = document.documentElement.clientHeight;
    const { width, height } = menu.getBoundingClientRect();

    const spaceBelow = vh - rect.bottom - GAP - MARGIN;
    const spaceAbove = rect.top - GAP - MARGIN;
    let up = placement === "up";
    if (up && spaceAbove < height && spaceBelow > spaceAbove) up = false;
    if (!up && spaceBelow < height && spaceAbove > spaceBelow) up = true;

    const top = up ? Math.max(MARGIN, rect.top - GAP - height) : rect.bottom + GAP;
    const rawLeft = align === "right" ? rect.right - width : rect.left;
    const left = Math.min(Math.max(MARGIN, rawLeft), Math.max(MARGIN, vw - width - MARGIN));
    const maxHeight = up ? rect.top - GAP - MARGIN : vh - rect.bottom - GAP - MARGIN;

    setStyle({
      top: Math.round(top),
      left: Math.round(left),
      maxHeight: Math.max(120, Math.floor(maxHeight)),
      visibility: "visible",
    });
  }, [placement, align]);

  // 先以 hidden 挂载量出尺寸，再定位，避免出现在错误位置后再跳一次
  useLayoutEffect(() => {
    if (!open) {
      setStyle({ visibility: "hidden" });
      return;
    }
    reposition();
    // 第一遍量的是未受 maxHeight 约束的高度；设完之后尺寸会变，再算一次才落准。
    const settle = requestAnimationFrame(() => reposition());
    const onChange = () => reposition();
    window.addEventListener("resize", onChange);
    // capture：祖先容器（时间线、侧栏）滚动时也要跟着走
    window.addEventListener("scroll", onChange, true);
    return () => {
      cancelAnimationFrame(settle);
      window.removeEventListener("resize", onChange);
      window.removeEventListener("scroll", onChange, true);
    };
  }, [open, reposition]);

  // 打开后把焦点移到第一个可用项（role="menu" 时）。dialog 交给内容自己决定。
  useEffect(() => {
    if (!open || role === "dialog") return;
    const first = menuRef.current?.querySelector<HTMLElement>(
      `${ITEM_SELECTOR}:not([disabled]):not([aria-disabled="true"])`
    );
    first?.focus();
  }, [open, role]);

  const items = useCallback(
    () =>
      Array.from(menuRef.current?.querySelectorAll<HTMLElement>(ITEM_SELECTOR) ?? []).filter(
        (el) => !el.hasAttribute("disabled") && el.getAttribute("aria-disabled") !== "true"
      ),
    []
  );

  const onMenuKeyDown = (event: React.KeyboardEvent<HTMLDivElement>) => {
    if (event.key === "Tab") {
      close();
      return;
    }
    if (role === "dialog") return;
    const list = items();
    if (list.length === 0) return;
    const index = list.indexOf(document.activeElement as HTMLElement);

    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const delta = event.key === "ArrowDown" ? 1 : -1;
      const next = index < 0 ? (delta > 0 ? 0 : list.length - 1) : (index + delta + list.length) % list.length;
      list[next].focus();
    } else if (event.key === "Home") {
      event.preventDefault();
      list[0].focus();
    } else if (event.key === "End") {
      event.preventDefault();
      list[list.length - 1].focus();
    }
  };

  const onTriggerKeyDown = (event: React.KeyboardEvent) => {
    if (open) return;
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      setOpen(true);
    }
  };

  if (!isValidElement(trigger)) {
    throw new Error("Menu: trigger 必须是单个 React 元素");
  }

  const triggerProps = trigger.props as Record<string, unknown>;
  const injected = cloneElement(trigger as ReactElement<Record<string, unknown>>, {
    ref: (node: HTMLElement | null) => {
      triggerRef.current = node;
      const original = (trigger as unknown as { ref?: unknown }).ref;
      if (typeof original === "function") (original as (n: HTMLElement | null) => void)(node);
      else if (original && typeof original === "object") {
        (original as { current: HTMLElement | null }).current = node;
      }
    },
    "aria-expanded": open,
    "aria-haspopup": role === "dialog" ? "dialog" : role,
    "aria-controls": open ? menuId : undefined,
    disabled: disabled || (triggerProps.disabled as boolean | undefined),
    onClick: (event: React.MouseEvent) => {
      (triggerProps.onClick as ((e: React.MouseEvent) => void) | undefined)?.(event);
      if (event.defaultPrevented || disabled) return;
      setOpen((value) => !value);
    },
    onKeyDown: (event: React.KeyboardEvent) => {
      (triggerProps.onKeyDown as ((e: React.KeyboardEvent) => void) | undefined)?.(event);
      onTriggerKeyDown(event);
    },
  } as Record<string, unknown>);

  return (
    <div className={"menu-root" + (className ? ` ${className}` : "")} ref={rootRef}>
      {injected}
      {open &&
        createPortal(
          <div
            id={menuId}
            ref={menuRef}
            role={role}
            aria-label={label}
            style={style}
            className={
              "popover" +
              (scroll ? " popover--scroll" : "") +
              (menuClassName ? ` ${menuClassName}` : "")
            }
            onKeyDown={onMenuKeyDown}
          >
            {typeof children === "function" ? children({ close }) : children}
          </div>,
          document.body
        )}
    </div>
  );
}

// ---------------------------------------------------------------------------
interface ItemProps {
  onSelect?: () => void;
  /** 单选语义（权限模式、provider 选择等）；会渲染成 menuitemradio */
  checked?: boolean;
  disabled?: boolean;
  /** 主文案 */
  children: ReactNode;
  /** 次要说明，渲染在下方 */
  hint?: ReactNode;
  /** 右侧快捷键提示 */
  shortcut?: ReactNode;
  className?: string;
  /** 命中后是否关闭菜单，默认 true */
  closeOnSelect?: boolean;
  close?: () => void;
}

export function MenuItem({
  onSelect,
  checked,
  disabled,
  children,
  hint,
  shortcut,
  className,
  closeOnSelect = true,
  close,
}: ItemProps) {
  const radio = checked !== undefined;
  return (
    <button
      type="button"
      role={radio ? "menuitemradio" : "menuitem"}
      aria-checked={radio ? checked : undefined}
      disabled={disabled}
      className={
        "menu-item ring-inset" +
        (hint ? " stacked" : "") +
        (checked ? " selected" : "") +
        (className ? ` ${className}` : "")
      }
      onClick={() => {
        onSelect?.();
        if (closeOnSelect) close?.();
      }}
    >
      {hint ? (
        <span className="menu-item-copy">
          <span>{children}</span>
          <small>{hint}</small>
        </span>
      ) : (
        children
      )}
      {shortcut && <span className="menu-item-key">{shortcut}</span>}
    </button>
  );
}

export function MenuSeparator() {
  return <div className="popover-sep" role="separator" />;
}

export function MenuEmpty({ children }: { children: ReactNode }) {
  return <span className="popover-empty">{children}</span>;
}
