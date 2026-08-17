import { useEffect, useRef } from "react";
import { createPortal } from "react-dom";
import { IconClose } from "./icons";

/**
 * 共享图片灯箱：backdrop 点击、Esc 关闭、焦点圈禁到关闭按钮。
 * 草稿附件托盘与时间线图片缩略图共用同一视觉与无障碍语义。
 */
export function ImageLightbox({
  src,
  alt,
  name,
  onClose,
}: {
  src: string;
  alt: string;
  name: string;
  onClose: () => void;
}) {
  const closeRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    const previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    closeRef.current?.focus();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        onClose();
        return;
      }
      if (event.key === "Tab") {
        event.preventDefault();
        closeRef.current?.focus();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("keydown", onKeyDown);
      if (previousFocus?.isConnected) previousFocus.focus();
    };
  }, [onClose]);

  return createPortal(
    <div
      className="attachment-preview-backdrop"
      role="presentation"
      onClick={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div
        className="attachment-preview"
        role="dialog"
        aria-modal="true"
        aria-label={`预览图片 ${name}`}
      >
        <header>
          <span>{name}</span>
          <button ref={closeRef} type="button" aria-label="关闭预览" onClick={onClose}>
            <IconClose width={16} height={16} />
          </button>
        </header>
        <img src={src} alt={alt} />
      </div>
    </div>,
    document.body,
  );
}
