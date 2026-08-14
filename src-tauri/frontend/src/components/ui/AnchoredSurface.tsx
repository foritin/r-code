import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type AriaRole,
  type CSSProperties,
  type KeyboardEventHandler,
  type ReactNode,
  type Ref,
  type RefObject,
} from "react";
import { createPortal } from "react-dom";

export type SurfacePlacement = "up" | "down" | "left" | "right";
export type SurfaceAlign = "left" | "right";

const DEFAULT_GAP = 6;
const MARGIN = 8;

interface SurfaceRect {
  top: number;
  right: number;
  bottom: number;
  left: number;
}

interface ViewportRect {
  top: number;
  left: number;
  width: number;
  height: number;
}

export interface SurfacePositionInput {
  anchor: SurfaceRect;
  surfaceWidth: number;
  surfaceHeight: number;
  viewport: ViewportRect;
  placement: SurfacePlacement;
  align: SurfaceAlign;
  gap?: number;
}

export interface SurfacePosition {
  top: number;
  left: number;
  maxHeight: number;
  maxWidth: number;
  placement: SurfacePlacement;
}

const clamp = (value: number, minimum: number, maximum: number) =>
  Math.min(Math.max(value, minimum), Math.max(minimum, maximum));

/**
 * 将浮层放入当前可见视口。
 *
 * 锚点可能已经被滚动到视口外（小窗口里的底部输入框就是这种情况），所以不能
 * 直接用 anchor.top / bottom 当作最终边界。上下可用空间和最终坐标都必须先钳进
 * visual viewport；同时绝不能用固定最小高度反向撑破视口。
 */
export function calculateSurfacePosition({
  anchor,
  surfaceWidth,
  surfaceHeight,
  viewport,
  placement,
  align,
  gap = DEFAULT_GAP,
}: SurfacePositionInput): SurfacePosition {
  const viewportTop = viewport.top + MARGIN;
  const viewportLeft = viewport.left + MARGIN;
  const viewportBottom = Math.max(viewportTop, viewport.top + viewport.height - MARGIN);
  const viewportRight = Math.max(viewportLeft, viewport.left + viewport.width - MARGIN);
  const viewportWidth = Math.max(0, viewportRight - viewportLeft);

  if (placement === "left" || placement === "right") {
    const leftEdge = clamp(anchor.left - gap, viewportLeft, viewportRight);
    const rightEdge = clamp(anchor.right + gap, viewportLeft, viewportRight);
    const spaceLeft = Math.max(0, leftEdge - viewportLeft);
    const spaceRight = Math.max(0, viewportRight - rightEdge);

    let useRight = placement === "right";
    const preferredSpace = useRight ? spaceRight : spaceLeft;
    const alternateSpace = useRight ? spaceLeft : spaceRight;
    if (preferredSpace < surfaceWidth && alternateSpace > preferredSpace) useRight = !useRight;

    const maxWidth = useRight ? spaceRight : spaceLeft;
    const maxHeight = Math.max(0, viewportBottom - viewportTop);
    const fittedWidth = Math.min(Math.max(0, surfaceWidth), maxWidth);
    const fittedHeight = Math.min(Math.max(0, surfaceHeight), maxHeight);
    const rawLeft = useRight ? rightEdge : leftEdge - fittedWidth;

    return {
      top: clamp(anchor.top, viewportTop, viewportBottom - fittedHeight),
      left: clamp(rawLeft, viewportLeft, viewportRight - fittedWidth),
      maxHeight,
      maxWidth,
      placement: useRight ? "right" : "left",
    };
  }

  const aboveEdge = clamp(anchor.top - gap, viewportTop, viewportBottom);
  const belowEdge = clamp(anchor.bottom + gap, viewportTop, viewportBottom);
  const spaceAbove = Math.max(0, aboveEdge - viewportTop);
  const spaceBelow = Math.max(0, viewportBottom - belowEdge);

  let useUp = placement === "up";
  const preferredSpace = useUp ? spaceAbove : spaceBelow;
  const alternateSpace = useUp ? spaceBelow : spaceAbove;
  if (preferredSpace < surfaceHeight && alternateSpace > preferredSpace) useUp = !useUp;

  const maxHeight = useUp ? spaceAbove : spaceBelow;
  const fittedHeight = Math.min(Math.max(0, surfaceHeight), maxHeight);
  const fittedWidth = Math.min(Math.max(0, surfaceWidth), viewportWidth);
  const edge = useUp ? aboveEdge : belowEdge;
  const rawTop = useUp ? edge - fittedHeight : edge;
  const rawLeft = align === "right" ? anchor.right - fittedWidth : anchor.left;

  return {
    top: clamp(rawTop, viewportTop, viewportBottom - fittedHeight),
    left: clamp(rawLeft, viewportLeft, viewportRight - fittedWidth),
    maxHeight,
    maxWidth: viewportWidth,
    placement: useUp ? "up" : "down",
  };
}

interface Props {
  anchorRef: RefObject<HTMLElement | null>;
  children: ReactNode;
  placement?: SurfacePlacement;
  align?: SurfaceAlign;
  className?: string;
  id?: string;
  role?: AriaRole;
  label?: string;
  matchAnchorWidth?: boolean;
  /** 触发器与浮层的间距；侧栏菜单可用更大间距跨过侧栏内边距。 */
  gap?: number;
  surfaceRef?: Ref<HTMLDivElement>;
  onKeyDown?: KeyboardEventHandler<HTMLDivElement>;
  onDismiss?: () => void;
}

function assignRef<T>(ref: Ref<T> | undefined, value: T | null) {
  if (typeof ref === "function") ref(value);
  else if (ref) (ref as { current: T | null }).current = value;
}

function visibleViewport(): ViewportRect {
  const visual = window.visualViewport;
  return {
    top: visual?.offsetTop ?? 0,
    left: visual?.offsetLeft ?? 0,
    width: visual?.width ?? document.documentElement.clientWidth,
    height: visual?.height ?? document.documentElement.clientHeight,
  };
}

/**
 * 受视口约束的 portal 浮层底座。只负责尺寸、翻转和定位；菜单语义、关闭规则与
 * 焦点管理仍由上层组件决定，因此也能复用于补全列表和搜索结果。
 */
export function AnchoredSurface({
  anchorRef,
  children,
  placement = "down",
  align = "left",
  className,
  id,
  role,
  label,
  matchAnchorWidth = false,
  gap = DEFAULT_GAP,
  surfaceRef,
  onKeyDown,
  onDismiss,
}: Props) {
  const ownRef = useRef<HTMLDivElement | null>(null);
  const [style, setStyle] = useState<CSSProperties>({
    position: "fixed",
    visibility: "hidden",
  });

  const reposition = useCallback(() => {
    const anchor = anchorRef.current;
    const surface = ownRef.current;
    if (!anchor || !surface) return;

    const anchorRect = anchor.getBoundingClientRect();
    const viewport = visibleViewport();
    const viewportWidth = Math.max(0, viewport.width - MARGIN * 2);

    // 临时解除上一次定位留下的约束，量出内容真实高度。宽度先固定到最终值，
    // 这样换行后的高度与实际展示一致；量完立即恢复，不产生可见跳动。
    const previousMaxHeight = surface.style.maxHeight;
    const previousMaxWidth = surface.style.maxWidth;
    const previousWidth = surface.style.width;
    surface.style.maxHeight = "none";
    surface.style.maxWidth = "none";
    if (matchAnchorWidth) surface.style.width = `${Math.min(anchorRect.width, viewportWidth)}px`;

    const surfaceRect = surface.getBoundingClientRect();
    const chromeHeight = Math.max(0, surfaceRect.height - surface.clientHeight);
    const naturalHeight = Math.max(surfaceRect.height, surface.scrollHeight + chromeHeight);
    const naturalWidth = matchAnchorWidth
      ? Math.min(anchorRect.width, viewportWidth)
      : Math.max(surfaceRect.width, surface.scrollWidth);

    surface.style.maxHeight = previousMaxHeight;
    surface.style.maxWidth = previousMaxWidth;
    surface.style.width = previousWidth;

    const next = calculateSurfacePosition({
      anchor: anchorRect,
      surfaceWidth: naturalWidth,
      surfaceHeight: naturalHeight,
      viewport,
      placement,
      align,
      gap,
    });
    const width = matchAnchorWidth ? Math.min(anchorRect.width, next.maxWidth) : undefined;
    const nextStyle: CSSProperties = {
      position: "fixed",
      top: Math.round(next.top),
      left: Math.round(next.left),
      maxHeight: Math.max(0, Math.floor(next.maxHeight)),
      maxWidth: Math.max(0, Math.floor(next.maxWidth)),
      width,
      visibility: "visible",
    };

    setStyle((current) =>
      current.top === nextStyle.top
      && current.left === nextStyle.left
      && current.maxHeight === nextStyle.maxHeight
      && current.maxWidth === nextStyle.maxWidth
      && current.width === nextStyle.width
      && current.visibility === "visible"
        ? current
        : nextStyle,
    );
  }, [align, anchorRef, gap, matchAnchorWidth, placement]);

  useLayoutEffect(() => {
    let frame = 0;
    const schedule = () => {
      cancelAnimationFrame(frame);
      frame = requestAnimationFrame(reposition);
    };
    const scheduleForAncestorScroll = (event: Event) => {
      const target = event.target;
      // `scroll` does not bubble, but the capture listener below still observes the
      // surface's own scrolling. Re-measuring temporarily removes max-height, which
      // makes WebView2 clamp scrollTop back to zero. Only ancestor/page scrolling can
      // move the anchor; scrolling inside the floating surface must keep its position.
      if (target instanceof Node && ownRef.current?.contains(target)) return;
      schedule();
    };

    reposition();
    schedule();
    window.addEventListener("resize", schedule);
    window.addEventListener("scroll", scheduleForAncestorScroll, true);
    window.visualViewport?.addEventListener("resize", schedule);
    window.visualViewport?.addEventListener("scroll", schedule);

    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(schedule);
    if (anchorRef.current) observer?.observe(anchorRef.current);
    if (ownRef.current) observer?.observe(ownRef.current);

    return () => {
      cancelAnimationFrame(frame);
      observer?.disconnect();
      window.removeEventListener("resize", schedule);
      window.removeEventListener("scroll", scheduleForAncestorScroll, true);
      window.visualViewport?.removeEventListener("resize", schedule);
      window.visualViewport?.removeEventListener("scroll", schedule);
    };
  }, [anchorRef, reposition]);

  useEffect(() => {
    if (!onDismiss) return;
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node;
      if (anchorRef.current?.contains(target) || ownRef.current?.contains(target)) return;
      onDismiss();
    };
    const onDocumentKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onDismiss();
    };
    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("keydown", onDocumentKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      document.removeEventListener("keydown", onDocumentKeyDown);
    };
  }, [anchorRef, onDismiss]);

  if (typeof document === "undefined") return null;

  return createPortal(
    <div
      id={id}
      ref={(node) => {
        ownRef.current = node;
        assignRef(surfaceRef, node);
      }}
      role={role}
      aria-label={label}
      className={className}
      style={style}
      onKeyDown={onKeyDown}
    >
      {children}
    </div>,
    document.body,
  );
}
