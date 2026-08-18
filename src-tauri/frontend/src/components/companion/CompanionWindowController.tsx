import { useEffect, useRef } from "react";
import {
  cursorPosition,
  getCurrentWindow,
  type PhysicalPosition,
} from "@tauri-apps/api/window";
import { useAppStore } from "../../store/app";
import { useTasksStore } from "../../store/tasks";
import { companionEnsure } from "../../lib/ipc";
import {
  companionPreferenceSnapshot,
  useCompanionStore,
  type CompanionPreferences,
} from "../../store/companion";
import {
  COMPANION_NAVIGATED_EVENT,
  COMPANION_NAVIGATE_EVENT,
  COMPANION_PREFERENCES_APPLIED_EVENT,
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
  type CompanionPreferencesApplied,
} from "./bridge";

type ControllerCleanup = () => void;
type ControllerListen = <T>(
  event: string,
  handler: (payload: T) => void | Promise<void>,
) => Promise<ControllerCleanup>;

interface AsyncCleanupScope {
  retain: (registration: Promise<ControllerCleanup>) => Promise<boolean>;
  dispose: () => void;
  isDisposed: () => boolean;
}

/** Tauri listener registration is asynchronous. A cleanup scope closes the race where an effect
 * unmounts before that registration resolves, immediately releasing every late native listener. */
export function createAsyncCleanupScope(): AsyncCleanupScope {
  let disposed = false;
  const cleanups: ControllerCleanup[] = [];
  return {
    async retain(registration) {
      const cleanup = await registration;
      if (disposed) {
        cleanup();
        return false;
      }
      cleanups.push(cleanup);
      return true;
    },
    dispose() {
      if (disposed) return;
      disposed = true;
      cleanups.splice(0).forEach((cleanup) => cleanup());
    },
    isDisposed: () => disposed,
  };
}

interface MainCompanionHandshakePorts {
  listen: ControllerListen;
  readSnapshot: () => CompanionPreferences;
  sendSnapshot: (snapshot: CompanionPreferences) => Promise<void>;
  applySnapshot: (snapshot: CompanionPreferences) => void;
}

/**
 * Install both halves of the main-window preference handshake before publishing its first event.
 * READY is intentionally replayable, and a stale delivery ACK immediately resends the latest
 * revision, so either WebView may win startup without permanently losing the preference snapshot.
 */
export async function attachMainCompanionHandshake(
  ports: MainCompanionHandshakePorts,
): Promise<ControllerCleanup> {
  const cleanups: ControllerCleanup[] = [];
  let disposed = false;
  const register = async <T,>(
    event: string,
    handler: (payload: T) => void | Promise<void>,
  ): Promise<boolean> => {
    const cleanup = await ports.listen<T>(event, handler);
    if (disposed) {
      cleanup();
      return false;
    }
    cleanups.push(cleanup);
    return true;
  };

  if (!await register<CompanionPreferencesApplied>(
    COMPANION_PREFERENCES_APPLIED_EVENT,
    async ({ revision }) => {
      const latest = ports.readSnapshot();
      if (revision < latest.revision) await ports.sendSnapshot(latest);
    },
  )) return () => {};
  if (!await register<undefined>(COMPANION_READY_EVENT, async () => {
    await ports.sendSnapshot(ports.readSnapshot());
  })) return () => {};
  if (!await register<CompanionPreferences>(COMPANION_PREFERENCES_EVENT, (snapshot) => {
    ports.applySnapshot(snapshot);
  })) return () => {};

  await ports.sendSnapshot(ports.readSnapshot());
  return () => {
    disposed = true;
    cleanups.splice(0).forEach((cleanup) => cleanup());
  };
}

const COMPANION_INTERACTIVE_SURFACES = [
  ".companion-session-panel",
  ".companion-session-card",
  ".companion-pulse-more",
  ".companion-pulse-error",
  ".companion-browser-menu",
  ".companion-sprite-frame",
  ".companion-aura",
  ".companion-unread-badge",
  ".companion-tracking-toggle",
] as const;

function rectContainsPoint(rect: DOMRect, x: number, y: number): boolean {
  return rect.width > 0 && rect.height > 0
    && x >= rect.left && x <= rect.right
    && y >= rect.top && y <= rect.bottom;
}

/** Visible companion surfaces remain interactive; transparent padding passes through natively. */
export function pointHitsCompanionSurface(documentRoot: Document, x: number, y: number): boolean {
  return COMPANION_INTERACTIVE_SURFACES.some((selector) =>
    [...documentRoot.querySelectorAll<HTMLElement>(selector)]
      .some((element) => rectContainsPoint(element.getBoundingClientRect(), x, y)));
}

/** Converting global physical cursor coordinates through one native scale keeps Windows mixed-DPI
 * hit testing aligned with WebView CSS pixels, including monitors positioned left of the primary. */
export function physicalCursorToLogicalPoint(
  cursor: { x: number; y: number },
  windowPosition: { x: number; y: number },
  scaleFactor: number,
): { x: number; y: number } {
  const scale = Number.isFinite(scaleFactor) && scaleFactor > 0 ? scaleFactor : 1;
  return {
    x: (cursor.x - windowPosition.x) / scale,
    y: (cursor.y - windowPosition.y) / scale,
  };
}

interface RestorableMainWindow {
  show: () => Promise<void>;
  unminimize: () => Promise<void>;
  setFocus: () => Promise<void>;
}

interface CursorEventPolicy {
  setIgnored: (ignored: boolean) => Promise<void>;
  /** 上一次真正下发到原生窗口的值；null 表示尚未下发过。 */
  applied: () => boolean | null;
}

/** Native window commands cannot be cancelled once dispatched. Serialize cursor-event updates so
 * a slow stale `true` can never finish after the `false` requested while hiding the companion.
 *
 * The policy is the single source of truth for what the native window currently honors: it
 * remembers the last applied value so redundant requests are skipped, and any caller (the
 * polling controller, the view's pre-show reset) observes the same state. Bypassing it with a
 * direct `setIgnoreCursorEvents` call desynchronizes the poller, which then believes the window
 * is click-through while it actually swallows every click in its transparent padding. */
export function createCursorEventPolicy(
  apply: (ignored: boolean) => Promise<void>,
): CursorEventPolicy {
  let appliedState: boolean | null = null;
  let desired = false;
  let chain = Promise.resolve();
  return {
    setIgnored(ignored) {
      desired = ignored;
      const request = chain.catch(() => {}).then(async () => {
        if (desired !== ignored) return;
        if (appliedState === ignored) return;
        await apply(ignored);
        appliedState = ignored;
      });
      // A rejected native command must not prevent a later recovery request from running.
      chain = request.catch(() => {});
      return request;
    },
    applied() {
      return appliedState;
    },
  };
}

let sharedCursorEventPolicy: CursorEventPolicy | null = null;

/** The whole companion WebView shares one cursor-event policy so every `setIgnoreCursorEvents`
 * caller stays in sync with the native window state the poller deduplicates against. */
export function companionCursorEventPolicy(): CursorEventPolicy | null {
  if (!isTauriRuntime()) return null;
  if (!sharedCursorEventPolicy) {
    const appWindow = getCurrentWindow();
    sharedCursorEventPolicy = createCursorEventPolicy(
      (ignored) => appWindow.setIgnoreCursorEvents(ignored),
    );
  }
  return sharedCursorEventPolicy;
}

/** Native focus policy is advisory on Windows. Once application routing succeeds, shell restore
 * failures must not turn a successful navigation into a false error acknowledgement. */
export async function restoreMainWindowBestEffort(
  mainWindow: RestorableMainWindow,
  onFailure: (operation: string, error: unknown) => void = () => {},
): Promise<void> {
  for (const [operation, restore] of [
    ["show", () => mainWindow.show()],
    ["unminimize", () => mainWindow.unminimize()],
    ["focus", () => mainWindow.setFocus()],
  ] as const) {
    try {
      await restore();
    } catch (error) {
      onFailure(operation, error);
    }
  }
}

/**
 * The companion store reads shared localStorage before React mounts. Hold only this WebView's
 * in-memory copy closed until a main-window snapshot crosses the acknowledged handshake.
 */
export function prepareNativeCompanionWindow(): void {
  if (!isTauriRuntime()) return;
  useCompanionStore.setState({ enabled: false });
}

/** Owns native hit testing without coupling window policy to the visual CompanionWindow tree. */
export function NativeCompanionWindowController() {
  const enabled = useCompanionStore((state) => state.enabled);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    const appWindow = getCurrentWindow();
    const cursorPolicy = companionCursorEventPolicy();
    if (!cursorPolicy) return;
    if (!enabled) {
      // Reset WS_EX_TRANSPARENT while hidden and do not keep polling cursorPosition through IPC.
      void cursorPolicy.setIgnored(false).catch(() => {});
      return;
    }
    let disposed = false;
    let clickThroughSupported = true;
    // A single transient IPC hiccup must not permanently disable pass-through: the dead zone
    // would silently revert to swallowing clicks. Only repeated failures give up (fail open).
    let consecutiveFailures = 0;
    let pointerActive = false;
    let pointerReleaseTimer: number | null = null;
    let tickTimer: number | null = null;
    let tickInFlight = false;
    let windowPosition: PhysicalPosition | null = null;
    let scaleFactor = 1;
    const nativeListeners = createAsyncCleanupScope();

    const pointerDown = () => {
      pointerActive = true;
      if (pointerReleaseTimer !== null) window.clearTimeout(pointerReleaseTimer);
      pointerReleaseTimer = window.setTimeout(() => { pointerActive = false; }, 3_000);
    };
    const pointerUp = () => {
      pointerActive = false;
      if (pointerReleaseTimer !== null) window.clearTimeout(pointerReleaseTimer);
      pointerReleaseTimer = null;
    };
    window.addEventListener("pointerdown", pointerDown, { capture: true });
    window.addEventListener("pointerup", pointerUp, { capture: true });
    window.addEventListener("pointercancel", pointerUp, { capture: true });

    const recordFailure = (error: unknown) => {
      consecutiveFailures += 1;
      if (consecutiveFailures < 3) return;
      if (clickThroughSupported) {
        clickThroughSupported = false;
        console.warn("Companion click-through disabled after repeated cursor/native failures.", error);
      }
    };

    const tick = async () => {
      if (disposed || !clickThroughSupported || tickInFlight) return;
      tickInFlight = true;
      try {
        if (document.visibilityState === "hidden") {
          if (cursorPolicy.applied() !== false) await cursorPolicy.setIgnored(false);
        } else {
          if (!windowPosition) windowPosition = await appWindow.outerPosition();
          if (pointerActive) {
            if (cursorPolicy.applied() !== false) await cursorPolicy.setIgnored(false);
          } else {
            const cursor = await cursorPosition();
            const point = physicalCursorToLogicalPoint(cursor, windowPosition, scaleFactor);
            const next = !pointHitsCompanionSurface(document, point.x, point.y);
            if (cursorPolicy.applied() !== next) await cursorPolicy.setIgnored(next);
          }
        }
        consecutiveFailures = 0;
      } catch (error) {
        recordFailure(error);
      } finally {
        tickInFlight = false;
        if (!disposed && clickThroughSupported) {
          tickTimer = window.setTimeout(tick, document.visibilityState === "hidden" ? 400 : 80);
        }
      }
    };

    const attach = async () => {
      [windowPosition, scaleFactor] = await Promise.all([
        appWindow.outerPosition(),
        appWindow.scaleFactor(),
      ]);
      if (!await nativeListeners.retain(appWindow.onMoved(({ payload }) => {
        windowPosition = payload;
      }))) return;
      if (!await nativeListeners.retain(appWindow.onScaleChanged(({ payload }) => {
        scaleFactor = payload.scaleFactor;
      }))) return;
      if (!nativeListeners.isDisposed()) await tick();
    };
    void attach().catch(() => { clickThroughSupported = false; });

    return () => {
      disposed = true;
      if (tickTimer !== null) window.clearTimeout(tickTimer);
      if (pointerReleaseTimer !== null) window.clearTimeout(pointerReleaseTimer);
      window.removeEventListener("pointerdown", pointerDown, { capture: true });
      window.removeEventListener("pointerup", pointerUp, { capture: true });
      window.removeEventListener("pointercancel", pointerUp, { capture: true });
      nativeListeners.dispose();
      // Always enqueue the recovery state. `ignored` may still be false while an earlier native
      // `true` request is in flight; the shared policy guarantees this runs after that request.
      void cursorPolicy.setIgnored(false).catch(() => {});
    };
  }, [enabled]);

  return null;
}

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
  const bridgeGeneration = useRef(0);
  const bridgeReady = useRef(false);
  const pendingPositionReset = useRef(false);

  useEffect(() => {
    if (!isTauriRuntime() || !enabled) return;
    let active = true;
    void companionEnsure().then((available) => {
      if (!available && active) useCompanionStore.getState().setEnabled(false);
    }).catch((error) => {
      console.warn("Native companion window could not be recovered.", error);
      if (active) useCompanionStore.getState().setEnabled(false);
    });
    return () => { active = false; };
  }, [enabled]);

  useEffect(() => {
    if (!isTauriRuntime() || !bridgeReady.current) return;
    void sendCompanionPreferences({ revision, enabled, minimized, soundEnabled, motion });
  }, [enabled, minimized, motion, revision, soundEnabled]);

  useEffect(() => {
    if (previousResetRevision.current === positionResetRevision) return;
    previousResetRevision.current = positionResetRevision;
    if (isTauriRuntime() && !bridgeReady.current) {
      pendingPositionReset.current = true;
      return;
    }
    void emitToWindow(COMPANION_WINDOW_LABEL, COMPANION_RESET_POSITION_EVENT, {
      revision: positionResetRevision,
    });
  }, [positionResetRevision]);

  useEffect(() => {
    if (!isTauriRuntime()) return;
    const generation = ++bridgeGeneration.current;
    bridgeReady.current = false;
    let disposed = false;
    const cleanups: Array<() => void> = [];

    const attach = async () => {
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
              await restoreMainWindowBestEffort(mainWindow, (operation, error) => {
                console.warn(`Companion navigation succeeded but main-window ${operation} failed.`, error);
              });
            }
            result.ok = true;
          } catch (error) {
            result.message = error instanceof Error ? error.message : String(error);
          }
          await emitToWindow(COMPANION_WINDOW_LABEL, COMPANION_NAVIGATED_EVENT, result);
        },
      ));
      cleanups.push(await attachMainCompanionHandshake({
        listen: listenFor,
        readSnapshot: companionPreferenceSnapshot,
        sendSnapshot: sendCompanionPreferences,
        applySnapshot,
      }));
      if (!disposed && bridgeGeneration.current === generation) {
        bridgeReady.current = true;
        // Close the final narrow race where Settings changed while the initial emit was awaiting
        // delivery but before the reactive publisher was marked ready. ACK de-duplicates this
        // replay and resends once more only if an even newer revision appeared.
        await sendCompanionPreferences(companionPreferenceSnapshot());
        if (pendingPositionReset.current) {
          pendingPositionReset.current = false;
          await emitToWindow(COMPANION_WINDOW_LABEL, COMPANION_RESET_POSITION_EVENT, {
            revision: useCompanionStore.getState().positionResetRevision,
          });
        }
      }
      if (disposed) cleanups.splice(0).forEach((cleanup) => cleanup());
    };
    void attach().catch(() => {
      if (bridgeGeneration.current === generation) bridgeReady.current = false;
      cleanups.splice(0).forEach((cleanup) => cleanup());
    });
    return () => {
      disposed = true;
      if (bridgeGeneration.current === generation) bridgeReady.current = false;
      cleanups.splice(0).forEach((cleanup) => cleanup());
    };
  }, [applySnapshot]);

  return null;
}
