/**
 * 全局快捷键注册。
 * - Ctrl+K：搜索 overlay（Mac 同时兼容 Command）
 * - Ctrl+E：Editor 切换
 * - Ctrl+N：新建会话（回 Home）
 * - Ctrl+,：设置
 * - Ctrl+/Ctrl-/Ctrl0：缩放
 * 场景内快捷键（j/k/x、a/g/d/r/p、←→⏎、F7）由各场景组件自行绑定（局部 keydown）。
 */
import { useEffect } from "react";

export interface GlobalKeyHandlers {
  onSearch: () => void;
  onEditor: () => void;
  onNew: () => void;
  onSettings: () => void;
  onZoomIn: () => void;
  onZoomOut: () => void;
  onZoomReset: () => void;
}

export function useGlobalKeys(handlers: GlobalKeyHandlers): void {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = e.metaKey || e.ctrlKey;
      if (!mod) return;
      const k = e.key.toLowerCase();
      if (k === "k") {
        e.preventDefault();
        handlers.onSearch();
      } else if (k === "e") {
        e.preventDefault();
        handlers.onEditor();
      } else if (k === "n") {
        e.preventDefault();
        handlers.onNew();
      } else if (k === ",") {
        e.preventDefault();
        handlers.onSettings();
      } else if (k === "=" || k === "+") {
        e.preventDefault();
        handlers.onZoomIn();
      } else if (k === "-") {
        e.preventDefault();
        handlers.onZoomOut();
      } else if (k === "0") {
        e.preventDefault();
        handlers.onZoomReset();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // handlers 由调用方保证稳定（useCallback 或 store action 引用）
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
}

/** 判断事件是否发生在输入控件内（场景快捷键应忽略）。 */
export function isTypingTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  const tag = target.tagName;
  return tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT" || target.isContentEditable;
}
