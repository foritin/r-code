import { emitTo, listen, type UnlistenFn } from "@tauri-apps/api/event";
import type { CompanionPreferences } from "../../store/companion";

export const COMPANION_WINDOW_LABEL = "companion";
export const MAIN_WINDOW_LABEL = "main";
export const COMPANION_PREFERENCES_EVENT = "r-code:companion-preferences";
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

export function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

export async function emitToWindow<T>(label: string, event: string, payload?: T): Promise<void> {
  if (!isTauriRuntime()) return;
  await emitTo(label, event, payload);
}

export async function listenFor<T>(event: string, handler: (payload: T) => void): Promise<UnlistenFn> {
  if (!isTauriRuntime()) return () => {};
  return listen<T>(event, ({ payload }) => handler(payload));
}

export const sendCompanionPreferences = (snapshot: CompanionPreferences) =>
  emitToWindow(COMPANION_WINDOW_LABEL, COMPANION_PREFERENCES_EVENT, snapshot);
