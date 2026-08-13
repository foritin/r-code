import { useEffect, useRef } from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { useAppStore } from "../../store/app";
import { useTasksStore } from "../../store/tasks";
import {
  companionPreferenceSnapshot,
  useCompanionStore,
  type CompanionPreferences,
} from "../../store/companion";
import {
  COMPANION_NAVIGATED_EVENT,
  COMPANION_NAVIGATE_EVENT,
  COMPANION_PREFERENCES_EVENT,
  COMPANION_READY_EVENT,
  COMPANION_RESET_POSITION_EVENT,
  COMPANION_WINDOW_LABEL,
  emitToWindow,
  isTauriRuntime,
  listenFor,
  sendCompanionPreferences,
  type CompanionNavigationRequest,
  type CompanionNavigationResult,
} from "./bridge";

/**
 * Keeps the main Settings store and the independent native companion WebView in sync.
 * The component renders nothing; it deliberately lives in the main window only.
 */
export function CompanionWindowController() {
  const revision = useCompanionStore((state) => state.revision);
  const enabled = useCompanionStore((state) => state.enabled);
  const minimized = useCompanionStore((state) => state.minimized);
  const soundEnabled = useCompanionStore((state) => state.soundEnabled);
  const motion = useCompanionStore((state) => state.motion);
  const positionResetRevision = useCompanionStore((state) => state.positionResetRevision);
  const applySnapshot = useCompanionStore((state) => state.applySnapshot);
  const previousResetRevision = useRef(positionResetRevision);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    void sendCompanionPreferences({ revision, enabled, minimized, soundEnabled, motion });
  }, [enabled, minimized, motion, revision, soundEnabled]);

  useEffect(() => {
    if (previousResetRevision.current === positionResetRevision) return;
    previousResetRevision.current = positionResetRevision;
    void emitToWindow(COMPANION_WINDOW_LABEL, COMPANION_RESET_POSITION_EVENT, {
      revision: positionResetRevision,
    });
  }, [positionResetRevision]);

  useEffect(() => {
    let disposed = false;
    const cleanups: Array<() => void> = [];

    const attach = async () => {
      cleanups.push(await listenFor<undefined>(COMPANION_READY_EVENT, () => {
        void sendCompanionPreferences(companionPreferenceSnapshot());
      }));
      cleanups.push(await listenFor<CompanionPreferences>(COMPANION_PREFERENCES_EVENT, (snapshot) => {
        applySnapshot(snapshot);
      }));
      cleanups.push(await listenFor<CompanionNavigationRequest>(
        COMPANION_NAVIGATE_EVENT,
        async ({ requestId, taskId }) => {
          const result: CompanionNavigationResult = { requestId, taskId, ok: false };
          try {
            let tasks = useTasksStore.getState();
            let task = tasks.tasks.find((candidate) => candidate.id === taskId);
            if (!task) {
              await tasks.refreshTasks();
              tasks = useTasksStore.getState();
              task = tasks.tasks.find((candidate) => candidate.id === taskId);
            }
            if (!task) throw new Error("这个会话已不存在");
            tasks.setCurrentProject(task.workspace_path);
            useAppStore.getState().openRoom(taskId);
            if (isTauriRuntime()) {
              const mainWindow = getCurrentWindow();
              await mainWindow.show();
              await mainWindow.unminimize();
              await mainWindow.setFocus();
            }
            result.ok = true;
          } catch (error) {
            result.message = error instanceof Error ? error.message : String(error);
          }
          await emitToWindow(COMPANION_WINDOW_LABEL, COMPANION_NAVIGATED_EVENT, result);
        },
      ));
      // Both WebViews start concurrently. The first eager preference event can arrive before the
      // companion has installed its listener, while its first READY can likewise beat this
      // listener. Replaying after every main-side listener is live closes both halves of that race;
      // the companion also retries READY until this snapshot is observed.
      await sendCompanionPreferences(companionPreferenceSnapshot());
      if (disposed) cleanups.splice(0).forEach((cleanup) => cleanup());
    };
    void attach();
    return () => {
      disposed = true;
      cleanups.splice(0).forEach((cleanup) => cleanup());
    };
  }, [applySnapshot]);

  return null;
}
