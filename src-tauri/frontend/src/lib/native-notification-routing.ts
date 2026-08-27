import type { NativeNotificationOpenPayload } from "./types";

/**
 * Durable hand-off between today's native-notification bridge and the future Automations UI.
 *
 * The native click can arrive before an Automations scene/route exists (or before its consumer is
 * mounted). Persisting the intent here means that click is not silently lost. The future consumer
 * should subscribe with `onPendingAutomationDeepLinkIntent` and claim work with
 * `consumePendingAutomationDeepLinkIntent`; consumption removes the item before returning it.
 */
export const AUTOMATION_DEEP_LINK_STORAGE_KEY = "r-code.deep-links.automation-runs.v1";
export const AUTOMATION_DEEP_LINK_PENDING_EVENT = "r-code:automation-deep-link-pending";

const STORAGE_VERSION = 1;
const MAX_PENDING_AUTOMATION_DEEP_LINKS = 32;

export interface PendingAutomationDeepLinkIntent {
  type: "automation_run";
  notification_id: string;
  automation_id: string;
  run_id: string;
  queued_at: string;
}

interface PersistedAutomationDeepLinkQueue {
  version: typeof STORAGE_VERSION;
  intents: PendingAutomationDeepLinkIntent[];
}

export interface DeepLinkStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

type PendingSignal = () => void;

function isNonEmptyString(value: unknown): value is string {
  return typeof value === "string" && value.trim().length > 0;
}

function isPendingAutomationDeepLinkIntent(
  value: unknown,
): value is PendingAutomationDeepLinkIntent {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Partial<PendingAutomationDeepLinkIntent>;
  return candidate.type === "automation_run"
    && isNonEmptyString(candidate.notification_id)
    && isNonEmptyString(candidate.automation_id)
    && isNonEmptyString(candidate.run_id)
    && isNonEmptyString(candidate.queued_at);
}

function normalizeQueue(value: unknown): PendingAutomationDeepLinkIntent[] {
  if (!value || typeof value !== "object") return [];
  const candidate = value as Partial<PersistedAutomationDeepLinkQueue>;
  if (candidate.version !== STORAGE_VERSION || !Array.isArray(candidate.intents)) return [];

  const seen = new Set<string>();
  return candidate.intents
    .filter(isPendingAutomationDeepLinkIntent)
    .filter((intent) => {
      if (seen.has(intent.notification_id)) return false;
      seen.add(intent.notification_id);
      return true;
    })
    .slice(-MAX_PENDING_AUTOMATION_DEEP_LINKS);
}

function browserStorage(): DeepLinkStorage | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

function browserPendingSignal(): void {
  if (typeof window === "undefined") return;
  window.dispatchEvent(new Event(AUTOMATION_DEEP_LINK_PENDING_EVENT));
}

/**
 * Small synchronous queue so claiming an intent and removing it from persistence are one JS turn.
 * A memory mirror keeps the current session usable if a restricted WebView rejects localStorage.
 */
export class AutomationDeepLinkQueue {
  private memory: PendingAutomationDeepLinkIntent[] = [];
  private storageUsable: boolean;

  constructor(
    private readonly storage: DeepLinkStorage | null = browserStorage(),
    private readonly signal: PendingSignal = browserPendingSignal,
    private readonly now: () => string = () => new Date().toISOString(),
  ) {
    this.storageUsable = storage != null;
    this.memory = this.readPersisted();
  }

  private readPersisted(): PendingAutomationDeepLinkIntent[] {
    if (!this.storage || !this.storageUsable) return [...this.memory];
    let raw: string | null;
    try {
      raw = this.storage.getItem(AUTOMATION_DEEP_LINK_STORAGE_KEY);
    } catch {
      this.storageUsable = false;
      return [...this.memory];
    }
    if (raw == null) return [...this.memory];
    try {
      return normalizeQueue(JSON.parse(raw));
    } catch {
      // A stale/corrupt snapshot is recoverable: the next mutation overwrites it with v1 data.
      return [];
    }
  }

  private persist(intents: PendingAutomationDeepLinkIntent[]): void {
    this.memory = intents;
    if (!this.storage || !this.storageUsable) return;
    try {
      const envelope: PersistedAutomationDeepLinkQueue = {
        version: STORAGE_VERSION,
        intents,
      };
      this.storage.setItem(AUTOMATION_DEEP_LINK_STORAGE_KEY, JSON.stringify(envelope));
    } catch {
      // Do not reload the stale persisted value in this WebView after a failed write.
      this.storageUsable = false;
    }
  }

  enqueue(
    payload: NativeNotificationOpenPayload & { target: { type: "automation_run" } },
  ): PendingAutomationDeepLinkIntent {
    const intents = this.readPersisted();
    const existing = intents.find(
      (intent) => intent.notification_id === payload.notification_id,
    );
    if (existing) return existing;

    const intent: PendingAutomationDeepLinkIntent = {
      type: "automation_run",
      notification_id: payload.notification_id,
      automation_id: payload.target.automation_id,
      run_id: payload.target.run_id,
      queued_at: this.now(),
    };
    this.persist([...intents, intent].slice(-MAX_PENDING_AUTOMATION_DEEP_LINKS));
    this.signal();
    return intent;
  }

  peek(): PendingAutomationDeepLinkIntent | null {
    const intents = this.readPersisted();
    this.memory = intents;
    return intents[0] ?? null;
  }

  /** Removes the oldest intent before returning it, so one queue item has one consumer. */
  consume(): PendingAutomationDeepLinkIntent | null {
    const intents = this.readPersisted();
    const intent = intents.shift() ?? null;
    if (intent) this.persist(intents);
    return intent;
  }

  snapshot(): PendingAutomationDeepLinkIntent[] {
    const intents = this.readPersisted();
    this.memory = intents;
    return [...intents];
  }
}

const automationDeepLinkQueue = new AutomationDeepLinkQueue();

export type NativeNotificationRouteResult =
  | { destination: "task"; task_id: string }
  | { destination: "automation_pending"; intent: PendingAutomationDeepLinkIntent };

/** Exhaustive router for every stable native notification target. */
export function routeNativeNotificationOpen(
  payload: NativeNotificationOpenPayload,
  openTask: (taskId: string) => void,
  queue: AutomationDeepLinkQueue = automationDeepLinkQueue,
): NativeNotificationRouteResult {
  switch (payload.target.type) {
    case "task":
      openTask(payload.target.task_id);
      return { destination: "task", task_id: payload.target.task_id };
    case "automation_run": {
      const intent = queue.enqueue({
        ...payload,
        target: payload.target,
      });
      return { destination: "automation_pending", intent };
    }
  }
}

export function peekPendingAutomationDeepLinkIntent(): PendingAutomationDeepLinkIntent | null {
  return automationDeepLinkQueue.peek();
}

export function consumePendingAutomationDeepLinkIntent(): PendingAutomationDeepLinkIntent | null {
  return automationDeepLinkQueue.consume();
}

export function pendingAutomationDeepLinkIntents(): PendingAutomationDeepLinkIntent[] {
  return automationDeepLinkQueue.snapshot();
}

/**
 * Subscribe to queue availability. A pending persisted item is announced immediately on subscribe,
 * so an Automations consumer mounted after app restart can recover the original click.
 */
export function onPendingAutomationDeepLinkIntent(
  handler: (intent: PendingAutomationDeepLinkIntent) => void,
): () => void {
  if (typeof window === "undefined") return () => {};
  const notify = () => {
    const intent = automationDeepLinkQueue.peek();
    if (intent) handler(intent);
  };
  window.addEventListener(AUTOMATION_DEEP_LINK_PENDING_EVENT, notify);
  notify();
  return () => window.removeEventListener(AUTOMATION_DEEP_LINK_PENDING_EVENT, notify);
}
