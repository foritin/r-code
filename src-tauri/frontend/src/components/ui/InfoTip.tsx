import { useId, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { AnchoredSurface } from "./AnchoredSurface";

/**
 * 轻量说明提示（E3）：`?` 图标 + focus/hover 展开说明，键盘可达。
 * 替代原生 `title=`（无障碍与触控差）与行内 hint 的第三种说明形态；
 * 触发按钮本身保留 aria-describedby，读屏可直接朗读说明。
 */
export function InfoTip({ children, label }: { children: ReactNode; label?: string }) {
  const [open, setOpen] = useState(false);
  const anchorRef = useRef<HTMLButtonElement>(null);
  const descriptionId = useId();

  return (
    <>
      <button
        ref={anchorRef}
        type="button"
        className="info-tip"
        aria-label={label ?? "查看说明"}
        aria-describedby={open ? descriptionId : undefined}
        aria-expanded={open}
        onFocus={() => setOpen(true)}
        onBlur={() => setOpen(false)}
        onMouseEnter={() => setOpen(true)}
        onMouseLeave={() => setOpen(false)}
        onClick={() => setOpen((value) => !value)}
      >
        ?
      </button>
      {open
        && createPortal(
          <AnchoredSurface
            anchorRef={anchorRef}
            className="popover info-tip-popover"
            placement="down"
            align="left"
            gap={8}
            role="tooltip"
            label={label ?? "说明"}
          >
            <div className="info-tip-surface" id={descriptionId} role="tooltip">
              {children}
            </div>
          </AnchoredSurface>,
          document.body,
        )}
    </>
  );
}
