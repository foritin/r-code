/**
 * 聚焦感知轮询 hook —— 动画预算红线：仅窗口可见且聚焦时轮询，失焦冷却。
 * Tauri WebView 下 document.visibilityState 与 window focus 均可用。
 */
import { useEffect, useRef } from "react";

export function usePoll(fn: () => void | Promise<void>, intervalMs: number, active = true): void {
  const fnRef = useRef(fn);
  fnRef.current = fn;

  useEffect(() => {
    if (!active) return;
    let timer: ReturnType<typeof setInterval> | null = null;
    let running = false;

    const tick = async () => {
      if (running) return; // 防重入
      running = true;
      try {
        await fnRef.current();
      } catch {
        /* 轮询错误静默，下一轮再试 */
      } finally {
        running = false;
      }
    };

    const shouldRun = () => document.visibilityState === "visible" && document.hasFocus();

    const start = () => {
      if (timer == null) {
        void tick();
        timer = setInterval(() => {
          if (shouldRun()) void tick();
        }, intervalMs);
      }
    };
    const stop = () => {
      if (timer != null) {
        clearInterval(timer);
        timer = null;
      }
    };
    const onWake = () => {
      if (shouldRun()) {
        stop();
        start(); // 唤醒立即刷一轮
      } else {
        stop();
      }
    };

    start();
    document.addEventListener("visibilitychange", onWake);
    window.addEventListener("focus", onWake);
    window.addEventListener("blur", onWake);
    return () => {
      stop();
      document.removeEventListener("visibilitychange", onWake);
      window.removeEventListener("focus", onWake);
      window.removeEventListener("blur", onWake);
    };
  }, [intervalMs, active]);
}
