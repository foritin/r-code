import { emitTo, listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { CompanionPreferences } from "../../store/companion";

export const COMPANION_WINDOW_LABEL = "companion";
export const MAIN_WINDOW_LABEL = "main";
export const COMPANION_PREFERENCES_EVENT = "r-code:companion-preferences";
export const COMPANION_PREFERENCES_APPLIED_EVENT = "r-code:companion-preferences-applied";
export const COMPANION_READY_EVENT = "r-code:companion-ready";
export const COMPANION_NAVIGATE_EVENT = "r-code:companion-navigate";
export const COMPANION_NAVIGATED_EVENT = "r-code:companion-navigated";
export const COMPANION_RESET_POSITION_EVENT = "r-code:companion-reset-position";

export interface CompanionNavigationRequest {
  requestId: string;
  taskId: string;
}

export interface CompanionNavigationResult {
  requestId: string;
  taskId: string;
  ok: boolean;
  message?: string;
}

export interface CompanionPreferencesApplied {
  revision: number;
}

const COMPANION_STARTUP_LISTENERS = new Set([
  COMPANION_PREFERENCES_EVENT,
  COMPANION_RESET_POSITION_EVENT,
  COMPANION_NAVIGATED_EVENT,
]);
const companionStartupListenerCounts = new Map<string, number>();
let pendingCompanionPreferences: {
  active: () => boolean;
  deliver: () => Promise<void>;
} | null = null;

function isCompanionRuntime(): boolean {
  return isTauriRuntime()
    && new URLSearchParams(window.location.search).get("window") === COMPANION_WINDOW_LABEL;
}

function companionStartupListenersReady(): boolean {
  return [...COMPANION_STARTUP_LISTENERS]
    .every((event) => (companionStartupListenerCounts.get(event) ?? 0) > 0);
}

async function flushPendingCompanionPreferences(): Promise<void> {
  if (!companionStartupListenersReady()) return;
  const pending = pendingCompanionPreferences;
  pendingCompanionPreferences = null;
  if (pending?.active()) await pending.deliver();
}

export function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export async function emitToWindow<T>(label: string, event: string, payload?: T): Promise<void> {
  if (!isTauriRuntime()) return;
  await emitTo(label, event, payload);
}

export async function listenFor<T>(
  event: string,
  handler: (payload: T) => void | Promise<void>,
): Promise<UnlistenFn> {
  if (!isTauriRuntime()) return () => {};
  const companionRuntime = isCompanionRuntime();
  let active = true;
  const deliver = async (payload: T) => {
    await handler(payload);
    // A preference event is the final leg of the native-window startup handshake. Acknowledge it
    // only after the companion-side handler has committed the snapshot, so the main controller
    // can distinguish delivery from a fire-and-forget event that raced WebView initialization.
    if (
      event === COMPANION_PREFERENCES_EVENT
      && companionRuntime
    ) {
      const revision = Number((payload as Partial<CompanionPreferences> | undefined)?.revision);
      if (Number.isFinite(revision)) {
        await emitTo(MAIN_WINDOW_LABEL, COMPANION_PREFERENCES_APPLIED_EVENT, { revision })
          .catch(() => {
            // The main WebView may already be exiting. Preference application is complete even
            // when there is no remaining controller to receive the delivery acknowledgement.
          });
      }
    }
  };
  const unlisten = await listen<T>(event, async ({ payload }) => {
    if (companionRuntime && event === COMPANION_PREFERENCES_EVENT
      && !companionStartupListenersReady()) {
      // Rust creates both WebViews concurrently. The first preference may arrive after this
      // listener exists but before reset/navigation listeners do. Keep only the newest idempotent
      // snapshot and apply it once the companion can safely become visible with all controls live.
      pendingCompanionPreferences = {
        active: () => active,
        deliver: () => deliver(payload),
      };
      return;
    }
    await deliver(payload);
  });
  if (companionRuntime && COMPANION_STARTUP_LISTENERS.has(event)) {
    companionStartupListenerCounts.set(
      event,
      (companionStartupListenerCounts.get(event) ?? 0) + 1,
    );
    await flushPendingCompanionPreferences();
  }
  return () => {
    active = false;
    if (companionRuntime && COMPANION_STARTUP_LISTENERS.has(event)) {
      const remaining = Math.max(0, (companionStartupListenerCounts.get(event) ?? 1) - 1);
      if (remaining === 0) companionStartupListenerCounts.delete(event);
      else companionStartupListenerCounts.set(event, remaining);
      if (pendingCompanionPreferences && !pendingCompanionPreferences.active()) {
        pendingCompanionPreferences = null;
      }
    }
    unlisten();
  };
}

export const sendCompanionPreferences = (snapshot: CompanionPreferences) =>
  emitToWindow(COMPANION_WINDOW_LABEL, COMPANION_PREFERENCES_EVENT, snapshot);
