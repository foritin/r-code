/**
 * 聚焦感知轮询调度器。
 *
 * 所有 usePoll 订阅共享一个最近到期定时器和一套窗口生命周期监听；每个订阅
 * 仍独立维护周期、防重入和同步错误来源。失焦时完全停表，恢复焦点后立即刷新。
 */
import { useEffect, useRef } from "react";
import { clearSyncFailure, reportSyncFailure } from "../store/sync-health";

interface PollRegistration {
  id: string;
  intervalMs: number;
  label: string;
  invoke: () => void | Promise<void>;
  nextTickAt: number;
  running: boolean;
  disposed: boolean;
}

const registrations = new Map<string, PollRegistration>();
let pollSequence = 0;
let schedulerTimer: ReturnType<typeof setTimeout> | null = null;
let lifecycleListenersAttached = false;

function shouldRun(): boolean {
  return document.visibilityState === "visible" && document.hasFocus();
}

function stopScheduler(): void {
  if (schedulerTimer != null) {
    clearTimeout(schedulerTimer);
    schedulerTimer = null;
  }
}

async function runRegistration(registration: PollRegistration): Promise<void> {
  if (registration.disposed || registration.running) return;
  registration.running = true;
  try {
    await registration.invoke();
    if (!registration.disposed) clearSyncFailure(registration.id);
  } catch (cause) {
    if (!registration.disposed) {
      reportSyncFailure(registration.id, registration.label, cause);
    }
  } finally {
    registration.running = false;
  }
}

function scheduleNext(): void {
  stopScheduler();
  if (registrations.size === 0 || !shouldRun()) return;

  const now = Date.now();
  let nextTickAt = Number.POSITIVE_INFINITY;
  for (const registration of registrations.values()) {
    nextTickAt = Math.min(nextTickAt, registration.nextTickAt);
  }
  const delay = Math.max(16, nextTickAt - now);
  schedulerTimer = setTimeout(() => {
    schedulerTimer = null;
    if (!shouldRun()) return;

    const tickedAt = Date.now();
    for (const registration of registrations.values()) {
      if (registration.nextTickAt > tickedAt) continue;
      registration.nextTickAt = tickedAt + registration.intervalMs;
      void runRegistration(registration);
    }
    scheduleNext();
  }, delay);
}

function wakeScheduler(): void {
  stopScheduler();
  if (!shouldRun()) return;

  const now = Date.now();
  for (const registration of registrations.values()) {
    registration.nextTickAt = now + registration.intervalMs;
    void runRegistration(registration);
  }
  scheduleNext();
}

function refreshNow(): void {
  for (const registration of registrations.values()) {
    void runRegistration(registration);
  }
}

function attachLifecycleListeners(): void {
  if (lifecycleListenersAttached) return;
  lifecycleListenersAttached = true;
  document.addEventListener("visibilitychange", wakeScheduler);
  window.addEventListener("focus", wakeScheduler);
  window.addEventListener("blur", wakeScheduler);
  window.addEventListener("r-code:refresh-now", refreshNow);
}

function detachLifecycleListeners(): void {
  if (!lifecycleListenersAttached) return;
  lifecycleListenersAttached = false;
  document.removeEventListener("visibilitychange", wakeScheduler);
  window.removeEventListener("focus", wakeScheduler);
  window.removeEventListener("blur", wakeScheduler);
  window.removeEventListener("r-code:refresh-now", refreshNow);
}

function registerPoll(registration: PollRegistration): () => void {
  registrations.set(registration.id, registration);
  attachLifecycleListeners();
  if (shouldRun()) void runRegistration(registration);
  scheduleNext();

  return () => {
    registration.disposed = true;
    registrations.delete(registration.id);
    clearSyncFailure(registration.id);
    if (registrations.size === 0) {
      stopScheduler();
      detachLifecycleListeners();
    } else {
      scheduleNext();
    }
  };
}

export function usePoll(
  fn: () => void | Promise<void>,
  intervalMs: number,
  active = true,
  label = "后台数据",
): void {
  const fnRef = useRef(fn);
  const sourceRef = useRef("");
  if (!sourceRef.current) sourceRef.current = `poll-${++pollSequence}`;
  fnRef.current = fn;

  useEffect(() => {
    if (!active) return;
    const normalizedInterval = Math.max(16, Math.floor(intervalMs));
    const registration: PollRegistration = {
      id: sourceRef.current,
      intervalMs: normalizedInterval,
      label,
      invoke: () => fnRef.current(),
      nextTickAt: Date.now() + normalizedInterval,
      running: false,
      disposed: false,
    };
    return registerPoll(registration);
  }, [intervalMs, active, label]);
}
