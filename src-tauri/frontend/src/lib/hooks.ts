/**
 * 跨场景复用的交互 hook。
 *
 * 审计发现三类样板被手写了几十遍：
 * - `setBusy(true); try{…}catch{setErr(String(e))}finally{setBusy(false)}` 约 20 处
 * - "二次点击确认" 4 套实现，超时 3000/4000ms 不一，还有一处用 window.confirm
 * - "点击外部关闭" 6 套，mousedown/pointerdown 混用，监听器有的常驻有的按需
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { errText } from "./format";

// ---------------------------------------------------------------- 异步动作
export interface AsyncAction<A extends unknown[]> {
  run: (...args: A) => Promise<void>;
  busy: boolean;
  error: string | null;
  clearError: () => void;
}

/**
 * 把 busy / error 的样板收进一处。fn 抛出时 error 走 errText 统一格式化
 * （原先 SettingsScene 用 String(e)、HomeScene 用 errText，文案格式不一致）。
 */
export function useAsyncAction<A extends unknown[]>(
  fn: (...args: A) => Promise<unknown>,
  options: { onError?: (message: string) => void; label?: string } = {}
): AsyncAction<A> {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const fnRef = useRef(fn);
  fnRef.current = fn;
  const optRef = useRef(options);
  optRef.current = options;
  const alive = useRef(true);
  useEffect(() => () => { alive.current = false; }, []);

  const run = useCallback(async (...args: A) => {
    if (alive.current) {
      setBusy(true);
      setError(null);
    }
    try {
      await fnRef.current(...args);
    } catch (cause) {
      const label = optRef.current.label;
      const message = label ? `${label}失败：${errText(cause)}` : errText(cause);
      if (alive.current) setError(message);
      optRef.current.onError?.(message);
    } finally {
      if (alive.current) setBusy(false);
    }
  }, []);

  const clearError = useCallback(() => setError(null), []);
  return { run, busy, error, clearError };
}

// ---------------------------------------------------------------- 二次确认
export interface ArmedAction {
  /** 是否已进入"再点一次确认"状态 */
  armed: boolean;
  /** 第一次调用进入待确认，第二次调用真正执行 */
  trigger: () => void;
  /** 主动取消待确认 */
  disarm: () => void;
}

const ARM_TIMEOUT_MS = 3500;

export function useArmedAction(action: () => void, timeoutMs = ARM_TIMEOUT_MS): ArmedAction {
  const [armed, setArmed] = useState(false);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const actionRef = useRef(action);
  actionRef.current = action;

  const disarm = useCallback(() => {
    if (timer.current) {
      clearTimeout(timer.current);
      timer.current = null;
    }
    setArmed(false);
  }, []);

  useEffect(() => disarm, [disarm]);

  const trigger = useCallback(() => {
    if (timer.current) {
      clearTimeout(timer.current);
      timer.current = null;
      setArmed(false);
      actionRef.current();
      return;
    }
    setArmed(true);
    timer.current = setTimeout(() => {
      timer.current = null;
      setArmed(false);
    }, timeoutMs);
  }, [timeoutMs]);

  return { armed, trigger, disarm };
}

// ---------------------------------------------------------------- 外部关闭
/**
 * 统一用 pointerdown（能正确处理触控与笔），且只在 open 为真时挂监听器。
 */
export function useDismiss(
  open: boolean,
  ref: React.RefObject<HTMLElement>,
  onDismiss: () => void,
  options: { escape?: boolean } = {}
): void {
  const { escape = true } = options;
  const cbRef = useRef(onDismiss);
  cbRef.current = onDismiss;

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: PointerEvent) => {
      if (ref.current?.contains(event.target as Node)) return;
      cbRef.current();
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.stopPropagation();
        cbRef.current();
      }
    };
    document.addEventListener("pointerdown", onPointerDown, true);
    if (escape) document.addEventListener("keydown", onKeyDown);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      if (escape) document.removeEventListener("keydown", onKeyDown);
    };
  }, [open, escape, ref]);
}

// ---------------------------------------------------------------- 焦点归还
/** 组件关闭后把焦点还给触发它的元素（原先全应用 0 处实现）。 */
export function useReturnFocus(active: boolean): void {
  const origin = useRef<HTMLElement | null>(null);
  useEffect(() => {
    if (active) {
      origin.current = document.activeElement as HTMLElement | null;
      return;
    }
    const target = origin.current;
    origin.current = null;
    if (target && document.contains(target)) target.focus();
  }, [active]);
}

// ---------------------------------------------------------------- 焦点陷阱
/**
 * 模态内的 Tab 循环。SearchOverlay 原先声明了 aria-modal="true" 却没有任何
 * 实现，Tab 会直接跑到背景的侧栏按钮上。
 */
export function useFocusTrap(ref: React.RefObject<HTMLElement>, active = true): void {
  useEffect(() => {
    if (!active) return;
    const node = ref.current;
    if (!node) return;

    const selector = [
      "a[href]", "button:not(:disabled)", "input:not(:disabled)",
      "textarea:not(:disabled)", "select:not(:disabled)", '[tabindex]:not([tabindex="-1"])',
    ].join(",");

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Tab") return;
      const items = Array.from(node.querySelectorAll<HTMLElement>(selector))
        .filter((el) => el.offsetParent !== null || el === document.activeElement);
      if (items.length === 0) return;
      const first = items[0];
      const last = items[items.length - 1];
      const current = document.activeElement;
      if (event.shiftKey && (current === first || !node.contains(current))) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && current === last) {
        event.preventDefault();
        first.focus();
      }
    };

    node.addEventListener("keydown", onKeyDown);
    return () => node.removeEventListener("keydown", onKeyDown);
  }, [ref, active]);
}
