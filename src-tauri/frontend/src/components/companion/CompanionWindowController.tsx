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
  // 头像按钮本体必须整块命中：只认内部 sprite/aura 矩形时，宠物边缘一圈会按“透明
  // 死区”处理而原生穿透，点击与拖拽在边缘直接落空。
  ".companion-avatar",
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

/** 命中测试的几何基底以 onMoved/onScaleChanged 事件驱动为主，但这些事件在睡眠恢复、
 * 显示器拓扑或 DPI 变化时可能漏发；每 ~1s（12×80ms）主动重取一次兜底，避免拿旧坐标
 * 把命中框整体偏移成“别处被隐形大框挡住、宠物本体却点不中”。 */
const GEOMETRY_REFRESH_TICKS = 12;
/** 原生拖拽不保证派发 pointerup：移动事件停顿这么久就视为按压结束，及时恢复穿透判定，
 * 否则整窗（含透明死区）会在拖拽结束后继续吞点击直到 3 秒兜底超时。 */
const POINTER_IDLE_RELEASE_MS = 640;

function rectContainsPoint(rect: DOMRect, x: number, y: number): boolean {
  return rect.width > 0 && rect.height > 0
    && x >= rect.left && x <= rect.right
    && y >= rect.top && y <= rect.bottom;
}

/** 布局矩形无法区分 visibility:hidden / display:none 的空壳与可见表面；只有可见元素才允许
 * 认领原生交互，否则其不可见边界会变成吞点击的"幽灵框"。opacity 不纳入判定（光晕等
 * 半透明装饰仍算可见）。 */
function elementClaimsInteractivity(element: HTMLElement): boolean {
  if (typeof element.checkVisibility === "function") {
    return element.checkVisibility({ checkVisibilityCSS: true });
  }
  return element.getClientRects().length > 0;
}

/** Visible companion surfaces remain interactive; transparent padding passes through natively. */
export function pointHitsCompanionSurface(documentRoot: Document, x: number, y: number): boolean {
  return COMPANION_INTERACTIVE_SURFACES.some((selector) =>
    [...documentRoot.querySelectorAll<HTMLElement>(selector)]
      .filter(elementClaimsInteractivity)
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
    // 瞬断 IPC 曾会让穿透一次性停摆（置标志位 + 停计时器），整窗从此以不可见大框吞掉背后
    // 所有点击。现在进入降级模式：fail-open 保持宠物可点可拖，并以慢节奏持续探活——一旦
    // 原生调用恢复成功，命中判定自动回到正常 80ms 循环。
    let consecutiveFailures = 0;
    let degradedWarned = false;
    let pointerActive = false;
    let pointerReleaseTimer: number | null = null;
    let lastNativeMoveAt = 0;
    let tickTimer: number | null = null;
    let tickInFlight = false;
    let effectivelyHidden = false;
    let geometryRefreshCountdown = 0;
    let windowPosition: PhysicalPosition | null = null;
    let scaleFactor = 1;
    const nativeListeners = createAsyncCleanupScope();

    const pointerDown = () => {
      pointerActive = true;
      lastNativeMoveAt = Date.now();
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
      if (!degradedWarned) {
        degradedWarned = true;
        console.warn("Companion click-through degraded after repeated cursor/native failures; probing continues.", error);
      }
      // Fail open，且必须真正下发：停摆时若上一次成功的是 setIgnored(true)，原生窗口会
      // 永远停留在 WS_EX_TRANSPARENT——点击与拖拽全部穿透，而 WebView 渲染不受影响，
      // 表现为“动画还在播但怎么点都没反应”。
      if (cursorPolicy.applied() !== false) void cursorPolicy.setIgnored(false).catch(() => {});
    };

    const tick = async () => {
      if (disposed || tickInFlight) return;
      tickInFlight = true;
      let healthy = true;
      try {
        // WebView2 的 visibilityState 可能滞留 hidden（最小化 / Win+D 恢复后事件未送达）：
        // DOM 说隐藏时用原生 isVisible 复核——真正可见的窗口绝不能整窗放开吞点击。
        let hidden = document.visibilityState === "hidden";
        if (hidden) hidden = !(await appWindow.isVisible());
        effectivelyHidden = hidden;
        if (hidden) {
          if (cursorPolicy.applied() !== false) await cursorPolicy.setIgnored(false);
        } else {
          geometryRefreshCountdown -= 1;
          if (!windowPosition || geometryRefreshCountdown <= 0) {
            const [position, scale] = await Promise.all([
              appWindow.outerPosition(),
              appWindow.scaleFactor(),
            ]);
            windowPosition = position;
            if (Number.isFinite(scale) && scale > 0) scaleFactor = scale;
            geometryRefreshCountdown = GEOMETRY_REFRESH_TICKS;
          }
          // 按压态只覆盖真实拖拽：指针按下但没有后续移动事件（原生拖拽不派发 pointerup
          // 的常见情形）时，超时释放，让透明死区尽快恢复穿透。
          if (pointerActive && Date.now() - lastNativeMoveAt > POINTER_IDLE_RELEASE_MS) {
            pointerActive = false;
            if (pointerReleaseTimer !== null) window.clearTimeout(pointerReleaseTimer);
            pointerReleaseTimer = null;
          }
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
        degradedWarned = false;
      } catch (error) {
        healthy = false;
        recordFailure(error);
      } finally {
        tickInFlight = false;
        if (!disposed) {
          tickTimer = window.setTimeout(tick, healthy && !effectivelyHidden ? 80 : 400);
        }
      }
    };

    let moveListenerReady = false;
    let scaleListenerReady = false;
    const attach = async () => {
      [windowPosition, scaleFactor] = await Promise.all([
        appWindow.outerPosition(),
        appWindow.scaleFactor(),
      ]);
      geometryRefreshCountdown = GEOMETRY_REFRESH_TICKS;
      if (!moveListenerReady) {
        if (!await nativeListeners.retain(appWindow.onMoved(({ payload }) => {
          windowPosition = payload;
          lastNativeMoveAt = Date.now();
        }))) return;
        moveListenerReady = true;
      }
      if (!scaleListenerReady) {
        if (!await nativeListeners.retain(appWindow.onScaleChanged(({ payload }) => {
          scaleFactor = payload.scaleFactor;
        }))) return;
        scaleListenerReady = true;
      }
      if (!nativeListeners.isDisposed()) await tick();
    };
    // 挂载失败（IPC 暂断）不再永久退化为整窗吞点击：保持 fail-open 并按退避重试。
    const attachWithRetry = async () => {
      let attempt = 0;
      while (!disposed) {
        try {
          await attach();
          return;
        } catch (error) {
          attempt += 1;
          if (attempt === 1 || attempt % 10 === 0) {
            console.warn("Companion click-through attach failed; retrying.", error);
          }
          if (cursorPolicy.applied() !== false) {
            await cursorPolicy.setIgnored(false).catch(() => {});
          }
          await new Promise((resolve) => {
            window.setTimeout(resolve, Math.min(2_000, 250 * attempt));
          });
        }
      }
    };
    // 隐藏期间的原生搬动（DPI 切换、显示器拓扑变化）不保证都能通过 onMoved 回放到
    // WebView；恢复可见时重取几何，避免拿旧坐标做命中测试而误判“光标不在宠物上”。
    const refreshGeometry = () => {
      if (disposed) return;
      void Promise.all([appWindow.outerPosition(), appWindow.scaleFactor()])
        .then(([position, scale]) => {
          if (disposed) return;
          windowPosition = position;
          scaleFactor = scale;
          geometryRefreshCountdown = GEOMETRY_REFRESH_TICKS;
        })
        .catch(() => {});
    };
    const onVisibilityChange = () => {
      if (document.visibilityState === "visible") refreshGeometry();
    };
    document.addEventListener("visibilitychange", onVisibilityChange);
    void attachWithRetry();

    return () => {
      disposed = true;
      if (tickTimer !== null) window.clearTimeout(tickTimer);
      if (pointerReleaseTimer !== null) window.clearTimeout(pointerReleaseTimer);
      window.removeEventListener("pointerdown", pointerDown, { capture: true });
      window.removeEventListener("pointerup", pointerUp, { capture: true });
      window.removeEventListener("pointercancel", pointerUp, { capture: true });
      document.removeEventListener("visibilitychange", onVisibilityChange);
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
