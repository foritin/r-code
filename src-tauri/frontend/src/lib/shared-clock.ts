import { useCallback, useRef, useSyncExternalStore } from "react";

type ClockListener = () => void;

interface ClockSubscription {
  intervalMs: number;
  nextTickAt: number;
}

const subscriptions = new Map<ClockListener, ClockSubscription>();
let snapshot = Date.now();
let timer: ReturnType<typeof setInterval> | null = null;
let timerIntervalMs: number | null = null;
let listeningForFocus = false;

function canTick(): boolean {
  return typeof document === "undefined"
    || (document.visibilityState === "visible" && document.hasFocus());
}

function stopTimer(): void {
  if (timer != null) {
    clearInterval(timer);
    timer = null;
  }
  timerIntervalMs = null;
}

function tick(): void {
  const now = Date.now();
  snapshot = now;
  for (const [listener, subscription] of subscriptions) {
    if (now < subscription.nextTickAt) continue;
    subscription.nextTickAt = now + subscription.intervalMs;
    listener();
  }
}

function syncTimer(): void {
  if (subscriptions.size === 0 || !canTick()) {
    stopTimer();
    return;
  }

  const nextIntervalMs = Math.min(
    ...Array.from(subscriptions.values(), ({ intervalMs }) => intervalMs),
  );
  if (timer != null && timerIntervalMs === nextIntervalMs) return;

  stopTimer();
  timerIntervalMs = nextIntervalMs;
  timer = setInterval(tick, nextIntervalMs);
}

function wakeClock(): void {
  stopTimer();
  if (!canTick()) return;

  const now = Date.now();
  snapshot = now;
  for (const [listener, subscription] of subscriptions) {
    subscription.nextTickAt = now + subscription.intervalMs;
    listener();
  }
  syncTimer();
}

function attachFocusListeners(): void {
  if (listeningForFocus || typeof document === "undefined") return;
  listeningForFocus = true;
  document.addEventListener("visibilitychange", wakeClock);
  window.addEventListener("focus", wakeClock);
  window.addEventListener("blur", wakeClock);
}

function detachFocusListeners(): void {
  if (!listeningForFocus || typeof document === "undefined") return;
  listeningForFocus = false;
  document.removeEventListener("visibilitychange", wakeClock);
  window.removeEventListener("focus", wakeClock);
  window.removeEventListener("blur", wakeClock);
}

function subscribe(listener: ClockListener, intervalMs: number): () => void {
  const normalizedInterval = Math.max(250, Math.floor(intervalMs));
  const now = Date.now();
  snapshot = now;
  subscriptions.set(listener, {
    intervalMs: normalizedInterval,
    nextTickAt: now + normalizedInterval,
  });
  attachFocusListeners();
  syncTimer();

  return () => {
    subscriptions.delete(listener);
    if (subscriptions.size === 0) {
      stopTimer();
      detachFocusListeners();
    } else {
      syncTimer();
    }
  };
}

function getSnapshot(): number {
  return snapshot;
}

/**
 * Shares one focus-aware wall clock across all elapsed/relative-time labels.
 * Passing null keeps the last snapshot without allocating a timer.
 */
export function useSharedNow(intervalMs: number | null): number {
  const inactiveSnapshotRef = useRef(Date.now());
  const subscribeAtInterval = useCallback(
    (listener: ClockListener) => intervalMs == null
      ? () => undefined
      : subscribe(listener, intervalMs),
    [intervalMs],
  );
  const now = useSyncExternalStore(subscribeAtInterval, getSnapshot, getSnapshot);
  if (intervalMs != null) inactiveSnapshotRef.current = now;
  return intervalMs == null ? inactiveSnapshotRef.current : now;
}
