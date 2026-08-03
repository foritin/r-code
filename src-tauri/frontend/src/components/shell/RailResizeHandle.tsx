import { useEffect, useRef } from "react";
import {
  DEFAULT_RAIL_WIDTH,
  MAX_RAIL_WIDTH,
  MIN_RAIL_WIDTH,
  clampRailWidth,
  useAppStore,
} from "../../store/app";

export const MIN_MAIN_WIDTH = 420;

export function railWidthForViewport(
  preferredWidth: number,
  renderedViewportWidth: number,
  scale = 1,
): number {
  const safeScale = Math.max(scale, 0.01);
  const logicalMaximum = (renderedViewportWidth - MIN_MAIN_WIDTH) / safeScale;
  const maximum = Math.max(MIN_RAIL_WIDTH, Math.min(MAX_RAIL_WIDTH, logicalMaximum));
  return Math.min(clampRailWidth(preferredWidth), maximum);
}

function viewportMaximum(): number {
  const app = document.getElementById("app");
  const rect = app?.getBoundingClientRect();
  const scale = rect && app && rect.width > 0 && app.clientWidth > 0
    ? rect.width / app.clientWidth
    : 1;
  return railWidthForViewport(MAX_RAIL_WIDTH, window.innerWidth, scale);
}

function pointerRailWidth(clientX: number): number {
  const app = document.getElementById("app");
  if (!app) return DEFAULT_RAIL_WIDTH;
  const rect = app.getBoundingClientRect();
  // CSS zoom scales getBoundingClientRect/clientX, while the grid track remains in
  // unscaled CSS pixels. Convert back before updating --rc-rail-w.
  const scale = rect.width > 0 && app.clientWidth > 0 ? rect.width / app.clientWidth : 1;
  return (clientX - rect.left) / Math.max(scale, 0.01);
}

function previewRailWidth(width: number): number {
  const next = Math.min(clampRailWidth(width), viewportMaximum());
  document.getElementById("app")?.style.setProperty("--rc-rail-preferred-w", `${next}px`);
  return next;
}

/** Thin visual divider with a forgiving pointer and keyboard hit target. */
export function RailResizeHandle() {
  const collapsed = useAppStore((state) => state.railCollapsed);
  const railWidth = useAppStore((state) => state.railWidth);
  const setRailWidth = useAppStore((state) => state.setRailWidth);
  const dragging = useRef(false);
  const previewWidth = useRef(railWidth);

  useEffect(() => {
    previewWidth.current = railWidth;
  }, [railWidth]);

  useEffect(() => () => {
    document.getElementById("app")?.classList.remove("rail-is-resizing");
  }, []);

  const finishDrag = (event: React.PointerEvent<HTMLDivElement>) => {
    if (!dragging.current) return;
    dragging.current = false;
    event.currentTarget.classList.remove("is-active");
    document.getElementById("app")?.classList.remove("rail-is-resizing");
    if (event.currentTarget.hasPointerCapture(event.pointerId)) {
      event.currentTarget.releasePointerCapture(event.pointerId);
    }
    setRailWidth(previewWidth.current);
  };

  return (
    <div
      className="rail-resizer"
      role="separator"
      tabIndex={collapsed ? -1 : 0}
      aria-hidden={collapsed || undefined}
      aria-label="调整左侧边栏宽度"
      aria-orientation="vertical"
      aria-valuemin={MIN_RAIL_WIDTH}
      aria-valuemax={Math.round(viewportMaximum())}
      aria-valuenow={Math.round(railWidth)}
      aria-valuetext={`左侧边栏 ${Math.round(railWidth)} 像素`}
      title="拖拽调整侧栏；双击恢复默认宽度"
      onDoubleClick={() => setRailWidth(DEFAULT_RAIL_WIDTH)}
      onKeyDown={(event) => {
        const step = event.shiftKey ? 24 : 8;
        let next: number | null = null;
        if (event.key === "ArrowLeft") next = railWidth - step;
        if (event.key === "ArrowRight") next = railWidth + step;
        if (event.key === "Home") next = MIN_RAIL_WIDTH;
        if (event.key === "End") next = viewportMaximum();
        if (next == null) return;
        event.preventDefault();
        setRailWidth(Math.min(next, viewportMaximum()));
      }}
      onPointerDown={(event) => {
        if (collapsed || event.button !== 0) return;
        event.preventDefault();
        dragging.current = true;
        previewWidth.current = railWidth;
        event.currentTarget.classList.add("is-active");
        document.getElementById("app")?.classList.add("rail-is-resizing");
        event.currentTarget.setPointerCapture(event.pointerId);
      }}
      onPointerMove={(event) => {
        if (!dragging.current) return;
        previewWidth.current = previewRailWidth(pointerRailWidth(event.clientX));
      }}
      onPointerUp={finishDrag}
      onPointerCancel={finishDrag}
    />
  );
}
