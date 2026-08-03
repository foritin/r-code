import { create } from "zustand";

export interface SyncIssue {
  source: string;
  label: string;
  message: string;
  failedAt: number;
}

interface SyncHealthState {
  issues: Record<string, SyncIssue>;
  report: (source: string, label: string, cause: unknown) => void;
  clear: (source: string) => void;
}

function readableFailure(cause: unknown): string {
  const raw = cause instanceof Error ? cause.message : String(cause ?? "未知错误");
  return raw.replace(/\s+/g, " ").trim().slice(0, 240) || "未知错误";
}

export const useSyncHealthStore = create<SyncHealthState>((set) => ({
  issues: {},
  report: (source, label, cause) => set((state) => {
    const message = readableFailure(cause);
    const current = state.issues[source];
    if (current?.message === message && current.label === label) return state;
    return {
      issues: {
        ...state.issues,
        [source]: { source, label, message, failedAt: Date.now() },
      },
    };
  }),
  clear: (source) => set((state) => {
    if (!(source in state.issues)) return state;
    const issues = { ...state.issues };
    delete issues[source];
    return { issues };
  }),
}));

export function reportSyncFailure(source: string, label: string, cause: unknown): void {
  useSyncHealthStore.getState().report(source, label, cause);
}

export function clearSyncFailure(source: string): void {
  useSyncHealthStore.getState().clear(source);
}
