import { useEffect, useId, useRef, type FormEvent } from "react";
import { createPortal } from "react-dom";
import { useFocusTrap } from "../../lib/hooks";

interface Props {
  open: boolean;
  title: string;
  description?: string;
  label: string;
  value: string;
  maxLength?: number;
  confirmLabel: string;
  busyLabel?: string;
  busy?: boolean;
  error?: string | null;
  confirmDisabled?: boolean;
  onChange: (value: string) => void;
  onConfirm: () => void;
  onCancel: () => void;
}

/** 单行文本编辑层：打开时全选原值，Enter 提交，Esc / 点击背板取消。 */
export function TextInputDialog({
  open,
  title,
  description,
  label,
  value,
  maxLength,
  confirmLabel,
  busyLabel = "正在保存…",
  busy = false,
  error,
  confirmDisabled = false,
  onChange,
  onConfirm,
  onCancel,
}: Props) {
  const titleId = useId();
  const descriptionId = useId();
  const errorId = useId();
  const inputId = useId();
  const dialogRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const returnFocusRef = useRef<HTMLElement | null>(null);
  const busyRef = useRef(busy);
  const onCancelRef = useRef(onCancel);
  busyRef.current = busy;
  onCancelRef.current = onCancel;
  useFocusTrap(dialogRef, open);

  useEffect(() => {
    if (!open) return;
    returnFocusRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    inputRef.current?.focus();
    inputRef.current?.select();
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !busyRef.current) onCancelRef.current();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      const target = returnFocusRef.current;
      if (target && document.contains(target)) target.focus({ preventScroll: true });
    };
  }, [open]);

  if (!open) return null;

  const describedBy = [description ? descriptionId : null, error ? errorId : null]
    .filter(Boolean)
    .join(" ") || undefined;
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (!busy && !confirmDisabled) onConfirm();
  };

  return createPortal(
    <div
      className="confirm-backdrop"
      onPointerDown={(event) => {
        if (!busy && event.target === event.currentTarget) onCancel();
      }}
    >
      <div
        ref={dialogRef}
        className="confirm-dialog text-input-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={describedBy}
      >
        <h2 id={titleId}>{title}</h2>
        {description && <p id={descriptionId}>{description}</p>}
        <form onSubmit={submit}>
          <label htmlFor={inputId}>{label}</label>
          <input
            ref={inputRef}
            id={inputId}
            className="input"
            value={value}
            maxLength={maxLength}
            disabled={busy}
            aria-invalid={error ? true : undefined}
            onChange={(event) => onChange(event.target.value)}
          />
          {maxLength != null && (
            <small className="text-input-dialog-count">{Array.from(value).length}/{maxLength}</small>
          )}
          {error && <div id={errorId} className="text-input-dialog-error" role="alert">{error}</div>}
          <div className="confirm-dialog-actions">
            <button type="button" className="rc-button" disabled={busy} onClick={onCancel}>
              取消
            </button>
            <button
              type="submit"
              className="rc-button rc-button-primary"
              disabled={busy || confirmDisabled}
            >
              {busy ? busyLabel : confirmLabel}
            </button>
          </div>
        </form>
      </div>
    </div>,
    document.body,
  );
}
