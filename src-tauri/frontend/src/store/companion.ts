import { create } from "zustand";

export type CompanionMotion = "system" | "full" | "reduced";

export interface CompanionPreferences {
  revision: number;
  enabled: boolean;
  minimized: boolean;
  soundEnabled: boolean;
  motion: CompanionMotion;
}

interface CompanionState extends CompanionPreferences {
  positionResetRevision: number;
  setEnabled: (enabled: boolean) => void;
  setMinimized: (minimized: boolean) => void;
  setSoundEnabled: (enabled: boolean) => void;
  setMotion: (motion: CompanionMotion) => void;
  applySnapshot: (snapshot: CompanionPreferences) => void;
  resetPosition: () => void;
}

const PREFERENCES_KEY = "r-code.companion.preferences.v2";
const LEGACY_ENABLED_KEY = "r-code.companion.enabled";
const LEGACY_MINIMIZED_KEY = "r-code.companion.minimized";
const LEGACY_SOUND_KEY = "r-code.companion.sound";
const LEGACY_MOTION_KEY = "r-code.companion.motion";

function safeStorageGet(key: string): string | null {
  try {
    return window.localStorage.getItem(key);
  } catch {
    return null;
  }
}

function legacyBoolean(key: string, fallback: boolean): boolean {
  const value = safeStorageGet(key);
  if (value === "1") return true;
  if (value === "0") return false;
  return fallback;
}

function normalizeMotion(value: unknown): CompanionMotion {
  return value === "full" || value === "reduced" || value === "system" ? value : "system";
}

function normalizeSnapshot(value: Partial<CompanionPreferences>): CompanionPreferences {
  return {
    revision: Number.isFinite(value.revision) ? Math.max(0, Number(value.revision)) : 0,
    enabled: typeof value.enabled === "boolean" ? value.enabled : true,
    minimized: typeof value.minimized === "boolean" ? value.minimized : false,
    soundEnabled: typeof value.soundEnabled === "boolean" ? value.soundEnabled : false,
    motion: normalizeMotion(value.motion),
  };
}

function readPreferences(): CompanionPreferences {
  try {
    const parsed = JSON.parse(safeStorageGet(PREFERENCES_KEY) ?? "null") as Partial<CompanionPreferences> | null;
    if (parsed) return normalizeSnapshot(parsed);
  } catch {
    // Malformed legacy state falls through to the migration defaults below.
  }
  return normalizeSnapshot({
    enabled: legacyBoolean(LEGACY_ENABLED_KEY, true),
    minimized: legacyBoolean(LEGACY_MINIMIZED_KEY, false),
    soundEnabled: legacyBoolean(LEGACY_SOUND_KEY, false),
    motion: normalizeMotion(safeStorageGet(LEGACY_MOTION_KEY)),
  });
}

function persist(snapshot: CompanionPreferences): void {
  try {
    window.localStorage.setItem(PREFERENCES_KEY, JSON.stringify(snapshot));
  } catch {
    // A restricted WebView still keeps the current in-memory preference.
  }
}

function nextRevision(current: number): number {
  return Math.max(Date.now(), current + 1);
}

const initial = readPreferences();

export function companionPreferenceSnapshot(): CompanionPreferences {
  const { revision, enabled, minimized, soundEnabled, motion } = useCompanionStore.getState();
  return { revision, enabled, minimized, soundEnabled, motion };
}

export const useCompanionStore = create<CompanionState>((set) => {
  const patchPreference = (patch: Partial<Omit<CompanionPreferences, "revision">>) => {
    set((state) => {
      const snapshot = normalizeSnapshot({
        revision: nextRevision(state.revision),
        enabled: patch.enabled ?? state.enabled,
        minimized: patch.minimized ?? state.minimized,
        soundEnabled: patch.soundEnabled ?? state.soundEnabled,
        motion: patch.motion ?? state.motion,
      });
      persist(snapshot);
      return snapshot;
    });
  };

  return {
    ...initial,
    positionResetRevision: 0,
    setEnabled: (enabled) => patchPreference({ enabled }),
    setMinimized: (minimized) => patchPreference({ minimized }),
    setSoundEnabled: (soundEnabled) => patchPreference({ soundEnabled }),
    setMotion: (motion) => patchPreference({ motion }),
    applySnapshot: (incoming) => {
      const snapshot = normalizeSnapshot(incoming);
      set((state) => {
        if (snapshot.revision < state.revision) return state;
        persist(snapshot);
        return snapshot;
      });
    },
    resetPosition: () => set((state) => ({
      positionResetRevision: nextRevision(state.positionResetRevision),
    })),
  };
});
