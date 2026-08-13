import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { Menu, MenuItem } from "@tauri-apps/api/menu";
import { LogicalSize, PhysicalPosition } from "@tauri-apps/api/dpi";
import {
  availableMonitors,
  currentMonitor,
  getCurrentWindow,
  primaryMonitor,
  type Monitor,
} from "@tauri-apps/api/window";
import companionSprite from "../../assets/companion/r-code-session-assistant-v2.png";
import { onAgentEvent } from "../../lib/ipc";
import type { Task, TaskDetail } from "../../lib/types";
import {
  companionPreferenceSnapshot,
  useCompanionStore,
  type CompanionMotion,
  type CompanionPreferences,
} from "../../store/companion";
import { useTasksStore } from "../../store/tasks";
import { shouldPlayCompanionCue, type CompanionMood } from "./policy";
import {
  COMPANION_NAVIGATED_EVENT,
  COMPANION_NAVIGATE_EVENT,
  COMPANION_PREFERENCES_EVENT,
  COMPANION_READY_EVENT,
  COMPANION_RESET_POSITION_EVENT,
  MAIN_WINDOW_LABEL,
  emitToWindow,
  isTauriRuntime,
  listenFor,
  type CompanionNavigationRequest,
  type CompanionNavigationResult,
} from "./bridge";

const COLLAPSED_FULL = { width: 168, height: 196 };
const COLLAPSED_MINI = { width: 108, height: 116 };
const EXPANDED = { width: 420, height: 360 };
const WINDOW_INSET = 18;
const POSITION_KEY = "r-code.companion.native-position.v1";
const UNREAD_KEY = "r-code.companion.unread-sessions.v1";

const MOOD_LABEL: Record<CompanionMood, string> = {
  idle: "随时可以开始",
  working: "正在处理任务",
  attention: "有会话等待确认",
  success: "会话已完成",
  error: "会话遇到了问题",
  review: "有修改等待审核",
};

const MOOD_PRIORITY: Record<CompanionMood, number> = {
  attention: 6,
  error: 5,
  review: 4,
  working: 3,
  success: 2,
  idle: 1,
};

interface PersistedPosition {
  x: number;
  y: number;
  monitorName?: string | null;
  relativeX?: number;
  relativeY?: number;
  scaleFactor?: number;
}

interface SessionProgress {
  task: Task;
  mood: CompanionMood;
  label: string;
  unread: boolean;
}

type CompanionPerformance = "sing" | "dance";
type CompanionVisualState = CompanionMood | CompanionPerformance;

function shouldReduceMotion(motion: CompanionMotion): boolean {
  if (motion === "reduced") return true;
  if (motion === "full") return false;
  return window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
}

function collapsedSize(minimized: boolean) {
  return minimized ? COLLAPSED_MINI : COLLAPSED_FULL;
}

function pendingPermissionCount(detail: TaskDetail | undefined): number {
  return detail?.permissions.filter((permission) => permission.decision === "pending").length ?? 0;
}

function taskMood(task: Task, detail: TaskDetail | undefined): CompanionMood {
  if (pendingPermissionCount(detail) > 0) return "attention";
  if (task.state === "interrupted") return "error";
  if (task.state === "review_ready") return "review";
  if (task.state === "exploring" || task.state === "in_progress") return "working";
  return "idle";
}

function taskSignature(task: Task, detail: TaskDetail | undefined): string {
  return `${task.state}:${pendingPermissionCount(detail)}`;
}

function taskProgressLabel(task: Task, detail: TaskDetail | undefined): string {
  const pending = pendingPermissionCount(detail);
  if (pending > 0) return `${pending} 项授权等待确认`;
  if (task.state === "interrupted") return "运行已中断";
  if (task.state === "review_ready") {
    const changes = detail?.changes.length ?? 0;
    return changes > 0 ? `${changes} 个文件待审阅` : "修改等待审阅";
  }
  if (task.state === "exploring") return "正在分析项目";
  if (task.state === "in_progress") return "正在实施";
  return "最近已完成";
}

function relativeTime(value: string): string {
  const delta = Math.max(0, Date.now() - Date.parse(value));
  const minutes = Math.floor(delta / 60_000);
  if (minutes < 1) return "刚刚";
  if (minutes < 60) return `${minutes} 分钟前`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours} 小时前`;
  return `${Math.floor(hours / 24)} 天前`;
}

function readUnread(): Set<string> {
  try {
    const parsed = JSON.parse(window.localStorage.getItem(UNREAD_KEY) ?? "[]") as unknown;
    return new Set(Array.isArray(parsed) ? parsed.filter((item): item is string => typeof item === "string") : []);
  } catch {
    return new Set();
  }
}

function persistUnread(unread: Set<string>): void {
  try {
    window.localStorage.setItem(UNREAD_KEY, JSON.stringify([...unread]));
  } catch {
    // The in-memory unread state remains useful even in restricted WebViews.
  }
}

function readPosition(): PersistedPosition | null {
  try {
    const parsed = JSON.parse(window.localStorage.getItem(POSITION_KEY) ?? "null") as Partial<PersistedPosition> | null;
    if (parsed && Number.isFinite(parsed.x) && Number.isFinite(parsed.y)) {
      return {
        x: Number(parsed.x),
        y: Number(parsed.y),
        monitorName: typeof parsed.monitorName === "string" ? parsed.monitorName : null,
        relativeX: Number.isFinite(parsed.relativeX) ? Number(parsed.relativeX) : undefined,
        relativeY: Number.isFinite(parsed.relativeY) ? Number(parsed.relativeY) : undefined,
        scaleFactor: Number.isFinite(parsed.scaleFactor) ? Number(parsed.scaleFactor) : undefined,
      };
    }
  } catch {
    // Restore the safe default below.
  }
  return null;
}

function persistPosition(position: PersistedPosition): void {
  try {
    window.localStorage.setItem(POSITION_KEY, JSON.stringify(position));
  } catch {
    // Position persistence is best effort and must never disable the assistant.
  }
}

function intersectsWorkArea(position: PersistedPosition, width: number, height: number, monitor: Monitor): boolean {
  const area = monitor.workArea;
  return position.x + width > area.position.x
    && position.y + height > area.position.y
    && position.x < area.position.x + area.size.width
    && position.y < area.position.y + area.size.height;
}

function workAreaIntersection(position: PersistedPosition, width: number, height: number, monitor: Monitor): number {
  const area = monitor.workArea;
  const left = Math.max(position.x, area.position.x);
  const top = Math.max(position.y, area.position.y);
  const right = Math.min(position.x + width, area.position.x + area.size.width);
  const bottom = Math.min(position.y + height, area.position.y + area.size.height);
  return Math.max(0, right - left) * Math.max(0, bottom - top);
}

async function playCue(mood: CompanionMood): Promise<void> {
  const AudioContextCtor = window.AudioContext
    ?? (window as typeof window & { webkitAudioContext?: typeof AudioContext }).webkitAudioContext;
  if (!AudioContextCtor) return;
  const context = new AudioContextCtor();
  let closed = false;
  const close = () => {
    if (closed) return;
    closed = true;
    void context.close().catch(() => {});
  };
  // A separate WebView may not inherit the main window's autoplay grant. Always release the
  // native audio graph even when resume is rejected or its media timeline never advances.
  const closeTimer = window.setTimeout(close, 1_000);
  try {
    await context.resume();
  } catch {
    window.clearTimeout(closeTimer);
    close();
    return;
  }
  if (context.state !== "running") {
    window.clearTimeout(closeTimer);
    close();
    return;
  }
  const gain = context.createGain();
  const oscillator = context.createOscillator();
  oscillator.type = "sine";
  oscillator.frequency.value = mood === "error" ? 196 : mood === "attention" ? 330 : 523;
  gain.gain.setValueAtTime(0.0001, context.currentTime);
  gain.gain.exponentialRampToValueAtTime(0.04, context.currentTime + 0.012);
  gain.gain.exponentialRampToValueAtTime(0.0001, context.currentTime + 0.18);
  oscillator.connect(gain);
  gain.connect(context.destination);
  oscillator.start();
  oscillator.stop(context.currentTime + 0.19);
  oscillator.addEventListener("ended", () => {
    window.clearTimeout(closeTimer);
    close();
  }, { once: true });
}

export function CompanionWindow() {
  const enabled = useCompanionStore((state) => state.enabled);
  const minimized = useCompanionStore((state) => state.minimized);
  const soundEnabled = useCompanionStore((state) => state.soundEnabled);
  const motion = useCompanionStore((state) => state.motion);
  const applySnapshot = useCompanionStore((state) => state.applySnapshot);
  const setEnabled = useCompanionStore((state) => state.setEnabled);
  const tasks = useTasksStore((state) => state.tasks);
  const details = useTasksStore((state) => state.details);
  const refreshTasks = useTasksStore((state) => state.refreshTasks);
  const refreshDetail = useTasksStore((state) => state.refreshDetail);
  const refreshDetails = useTasksStore((state) => state.refreshDetails);
  const [panelOpen, setPanelOpen] = useState(false);
  const [panelSide, setPanelSide] = useState<"left" | "right">("left");
  const [browserMenuOpen, setBrowserMenuOpen] = useState(false);
  const [navigationError, setNavigationError] = useState<string | null>(null);
  const [unread, setUnread] = useState<Set<string>>(readUnread);
  const [successUntil, setSuccessUntil] = useState(0);
  const [performance, setPerformance] = useState<CompanionPerformance | null>(null);
  const [hovered, setHovered] = useState(false);
  const initializedTasks = useRef(false);
  const taskSignatures = useRef(new Map<string, string>());
  const anchorPosition = useRef<PersistedPosition | null>(null);
  const panelOpenRef = useRef(panelOpen);
  const movedDuringPointer = useRef(false);
  const pointerDragCandidate = useRef(false);
  const suppressNextClick = useRef(false);
  const dragOrigin = useRef<PersistedPosition | null>(null);
  const dragOriginRequest = useRef<Promise<void> | null>(null);
  const receivedPreferences = useRef(false);
  const previousCueMood = useRef<CompanionMood>("idle");
  const avatarRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLElement>(null);
  const performanceEndTimer = useRef<number | null>(null);
  const performanceCooldownUntil = useRef(0);
  const interactionTurn = useRef(0);
  const reconciliationInFlight = useRef<Promise<void> | null>(null);
  const pendingNavigation = useRef(new Map<string, (result: CompanionNavigationResult) => void>());

  panelOpenRef.current = panelOpen;
  const native = isTauriRuntime();

  const stopPerformance = useCallback(() => {
    if (performanceEndTimer.current !== null) {
      window.clearTimeout(performanceEndTimer.current);
      performanceEndTimer.current = null;
    }
    performanceCooldownUntil.current = Date.now() + 450;
    setPerformance(null);
  }, []);

  const closePanel = useCallback(async () => {
    panelOpenRef.current = false;
    setPanelOpen(false);
    setNavigationError(null);
    window.setTimeout(() => avatarRef.current?.focus(), 0);
    if (!native) return;
    const appWindow = getCurrentWindow();
    await appWindow.setSize(new LogicalSize(
      collapsedSize(minimized).width,
      collapsedSize(minimized).height,
    ));
    if (anchorPosition.current) {
      await appWindow.setPosition(new PhysicalPosition(anchorPosition.current.x, anchorPosition.current.y));
    }
  }, [minimized, native]);

  const positionForMonitor = useCallback((position: PersistedPosition, width: number, height: number, monitor: Monitor) => {
    const area = monitor.workArea;
    const maxX = Math.max(area.position.x, area.position.x + area.size.width - width);
    const maxY = Math.max(area.position.y, area.position.y + area.size.height - height);
    return {
      x: Math.min(Math.max(position.x, area.position.x), maxX),
      y: Math.min(Math.max(position.y, area.position.y), maxY),
    };
  }, []);

  const persistedPosition = useCallback((position: PersistedPosition, width: number, height: number, monitor: Monitor): PersistedPosition => {
    const area = monitor.workArea;
    return {
      ...position,
      monitorName: monitor.name,
      relativeX: (position.x - area.position.x) / Math.max(1, area.size.width - width),
      relativeY: (position.y - area.position.y) / Math.max(1, area.size.height - height),
      scaleFactor: monitor.scaleFactor,
    };
  }, []);

  const resetNativePosition = useCallback(async () => {
    if (!native) return;
    const appWindow = getCurrentWindow();
    const monitor = await primaryMonitor();
    if (!monitor) return;
    const size = await appWindow.outerSize();
    const rawPosition = {
      x: monitor.workArea.position.x + monitor.workArea.size.width - size.width - WINDOW_INSET,
      y: monitor.workArea.position.y + monitor.workArea.size.height - size.height - WINDOW_INSET,
    };
    const position = positionForMonitor(rawPosition, size.width, size.height, monitor);
    await appWindow.setPosition(new PhysicalPosition(position.x, position.y));
    anchorPosition.current = position;
    persistPosition(persistedPosition(position, size.width, size.height, monitor));
  }, [native, persistedPosition, positionForMonitor]);

  const restoreNativePosition = useCallback(async () => {
    if (!native) return;
    const appWindow = getCurrentWindow();
    const saved = readPosition();
    const [monitors, size] = await Promise.all([availableMonitors(), appWindow.outerSize()]);
    if (saved) {
      const named = monitors.find((monitor) => monitor.name && monitor.name === saved.monitorName);
      const intersecting = monitors
        .filter((monitor) => intersectsWorkArea(saved, size.width, size.height, monitor))
        .sort((a, b) => workAreaIntersection(saved, size.width, size.height, b)
          - workAreaIntersection(saved, size.width, size.height, a))[0];
      const monitor = named ?? intersecting;
      if (monitor) {
        const area = monitor.workArea;
        const restored = named && Number.isFinite(saved.relativeX) && Number.isFinite(saved.relativeY)
          ? {
            x: area.position.x + Math.max(0, Math.min(1, saved.relativeX!)) * Math.max(0, area.size.width - size.width),
            y: area.position.y + Math.max(0, Math.min(1, saved.relativeY!)) * Math.max(0, area.size.height - size.height),
          }
          : saved;
        const position = positionForMonitor(restored, size.width, size.height, monitor);
        anchorPosition.current = position;
        persistPosition(persistedPosition(position, size.width, size.height, monitor));
        await appWindow.setPosition(new PhysicalPosition(position.x, position.y));
        return;
      }
    }
    await resetNativePosition();
  }, [native, persistedPosition, positionForMonitor, resetNativePosition]);

  const openPanel = useCallback(async () => {
    setNavigationError(null);
    if (!native) {
      setPanelOpen(true);
      return;
    }
    const appWindow = getCurrentWindow();
    panelOpenRef.current = true;
    const [position, scale, monitor] = await Promise.all([
      appWindow.outerPosition(),
      appWindow.scaleFactor(),
      currentMonitor(),
    ]);
    anchorPosition.current = { x: position.x, y: position.y };
    const compact = collapsedSize(minimized);
    const center = monitor
      ? monitor.workArea.position.x + monitor.workArea.size.width / 2
      : position.x + compact.width * scale / 2;
    const side = position.x + compact.width * scale / 2 >= center ? "left" : "right";
    setPanelSide(side);
    await appWindow.setSize(new LogicalSize(EXPANDED.width, EXPANDED.height));
    const deltaX = (EXPANDED.width - compact.width) * scale;
    const deltaY = (EXPANDED.height - compact.height) * scale;
    const next = {
      x: side === "left" ? position.x - deltaX : position.x,
      y: position.y - deltaY,
    };
    if (monitor) {
      next.x = Math.min(
        Math.max(next.x, monitor.workArea.position.x),
        monitor.workArea.position.x + monitor.workArea.size.width - EXPANDED.width * scale,
      );
      next.y = Math.min(
        Math.max(next.y, monitor.workArea.position.y),
        monitor.workArea.position.y + monitor.workArea.size.height - EXPANDED.height * scale,
      );
    }
    await appWindow.setPosition(new PhysicalPosition(next.x, next.y));
    setPanelOpen(true);
  }, [minimized, native]);

  const disableCompanion = useCallback(async () => {
    setBrowserMenuOpen(false);
    await closePanel();
    setEnabled(false);
    await emitToWindow(MAIN_WINDOW_LABEL, COMPANION_PREFERENCES_EVENT, companionPreferenceSnapshot());
    if (native) await getCurrentWindow().hide();
  }, [closePanel, native, setEnabled]);

  useEffect(() => {
    document.documentElement.classList.add("companion-window-document");
    document.body.classList.add("companion-window-body");
    const preventDefaultMenu = (event: MouseEvent) => event.preventDefault();
    document.addEventListener("contextmenu", preventDefaultMenu, { capture: true });
    return () => {
      document.removeEventListener("contextmenu", preventDefaultMenu, { capture: true });
      document.documentElement.classList.remove("companion-window-document");
      document.body.classList.remove("companion-window-body");
    };
  }, []);

  useEffect(() => {
    if (!native) return;
    let disposed = false;
    const cleanups: Array<() => void> = [];
    const attach = async () => {
      cleanups.push(await listenFor<CompanionPreferences>(COMPANION_PREFERENCES_EVENT, (snapshot) => {
        receivedPreferences.current = true;
        applySnapshot(snapshot);
      }));
      cleanups.push(await listenFor(COMPANION_RESET_POSITION_EVENT, () => {
        void closePanel().then(resetNativePosition);
      }));
      cleanups.push(await listenFor<CompanionNavigationResult>(COMPANION_NAVIGATED_EVENT, (result) => {
        pendingNavigation.current.get(result.requestId)?.(result);
        pendingNavigation.current.delete(result.requestId);
      }));
      // Main and companion are created concurrently. Retry the idempotent READY handshake until
      // the main controller replies with a preference snapshot; this prevents an enabled helper
      // from remaining permanently hidden when either WebView wins startup by a few milliseconds.
      for (let attempt = 0; attempt < 20 && !receivedPreferences.current && !disposed; attempt += 1) {
        await emitToWindow(MAIN_WINDOW_LABEL, COMPANION_READY_EVENT);
        await new Promise((resolve) => window.setTimeout(resolve, 250));
      }
      if (disposed) cleanups.splice(0).forEach((cleanup) => cleanup());
    };
    void attach();
    return () => {
      disposed = true;
      cleanups.splice(0).forEach((cleanup) => cleanup());
    };
  }, [applySnapshot, closePanel, native, resetNativePosition]);

  useEffect(() => {
    if (!native) return;
    const appWindow = getCurrentWindow();
    let disposed = false;
    let unlisten: (() => void) | undefined;
    let persistTimer: number | null = null;
    let moveGeneration = 0;
    let latestPosition: PersistedPosition | null = null;
    void appWindow.onMoved(({ payload }) => {
      if (pointerDragCandidate.current) movedDuringPointer.current = true;
      if (panelOpenRef.current) return;
      const position = { x: payload.x, y: payload.y };
      anchorPosition.current = position;
      latestPosition = position;
      moveGeneration += 1;
      const generation = moveGeneration;
      if (persistTimer !== null) window.clearTimeout(persistTimer);
      // Native move events can arrive at display refresh rate. Persist only the trailing position
      // after movement settles, so dragging never creates an IPC/localStorage write storm.
      persistTimer = window.setTimeout(() => {
        persistTimer = null;
        const settled = latestPosition;
        if (!settled) return;
        void Promise.all([appWindow.outerSize(), currentMonitor()]).then(([size, monitor]) => {
          if (disposed || generation !== moveGeneration) return;
          persistPosition(monitor
            ? persistedPosition(settled, size.width, size.height, monitor)
            : settled);
        });
      }, 160);
    }).then((cleanup) => {
      if (disposed) cleanup();
      else unlisten = cleanup;
    });
    return () => {
      disposed = true;
      if (persistTimer !== null) window.clearTimeout(persistTimer);
      unlisten?.();
    };
  }, [native, persistedPosition]);

  useEffect(() => {
    if (!native) return;
    const appWindow = getCurrentWindow();
    let disposed = false;
    const update = async () => {
      if (!enabled) {
        await appWindow.hide().catch(() => {});
        return;
      }
      try {
        await appWindow.setAlwaysOnTop(true);
      } catch {
        // Some Linux window managers can reject the hint; the companion should still be usable.
      }
      try {
        await appWindow.setVisibleOnAllWorkspaces(true);
      } catch {
        // Windows does not expose all-workspace placement; always-on-top still applies.
      }
      if (disposed) return;
      await closePanel();
      if (disposed) return;
      await restoreNativePosition();
      if (disposed) return;
      await appWindow.show();
    };
    void update();
    return () => {
      disposed = true;
    };
  }, [closePanel, enabled, minimized, native, restoreNativePosition]);

  const reconcileTasks = useCallback(async () => {
    if (reconciliationInFlight.current) {
      await reconciliationInFlight.current;
      return;
    }
    const request = (async () => {
      await refreshTasks();
      const snapshot = useTasksStore.getState();
      const ids = snapshot.tasks
        .filter((task) => task.state !== "archived")
        .sort((a, b) => Date.parse(b.updated_at) - Date.parse(a.updated_at))
        .slice(0, 12)
        .filter((task) => {
          const detail = snapshot.details[task.id];
          return task.state === "exploring"
            || task.state === "in_progress"
            || !detail
            || detail.task.updated_at !== task.updated_at
            || detail.task.state !== task.state;
        })
        .map((task) => task.id);
      if (ids.length) await refreshDetails(ids);
    })();
    reconciliationInFlight.current = request;
    try {
      await request;
    } finally {
      if (reconciliationInFlight.current === request) reconciliationInFlight.current = null;
    }
  }, [refreshDetails, refreshTasks]);

  useEffect(() => {
    if (!enabled) return;
    let disposed = false;
    let timer: number | null = null;
    const loop = async () => {
      try {
        await reconcileTasks();
      } catch {
        // Agent events and the next reconciliation tick recover transient errors.
      }
      if (!disposed) {
        const snapshot = useTasksStore.getState();
        const active = snapshot.tasks.some((task) => task.state === "exploring"
          || task.state === "in_progress"
          || pendingPermissionCount(snapshot.details[task.id]) > 0);
        // Agent events remain immediate. The fallback only stays brisk while work is active; an
        // idle always-on-top window must not repeatedly hydrate large task histories all day.
        timer = window.setTimeout(loop, active ? 5_000 : 20_000);
      }
    };
    void loop();
    return () => {
      disposed = true;
      if (timer !== null) window.clearTimeout(timer);
    };
  }, [enabled, reconcileTasks]);

  useEffect(() => {
    if (!enabled) return;
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void onAgentEvent((taskId, event) => {
      if (event.type === "state") void reconcileTasks().catch(() => {});
      if (event.type === "state" || (event.type === "scoped" && event.event.type === "subagent_lifecycle")) {
        void refreshDetail(taskId);
      }
    }).then((cleanup) => {
      if (disposed) cleanup();
      else unlisten = cleanup;
    }).catch(() => {});
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [enabled, reconcileTasks, refreshDetail]);

  useEffect(() => {
    if (!tasks.length) return;
    if (
      !initializedTasks.current
      && tasks
        .filter((task) => task.state !== "archived")
        .sort((a, b) => Date.parse(b.updated_at) - Date.parse(a.updated_at))
        .slice(0, 12)
        .some((task) => !details[task.id])
    ) {
      // Tasks and details arrive in separate IPC batches. Treat both as one startup snapshot so
      // an already-pending permission is not misclassified as a new unread event.
      return;
    }
    const nextSignatures = new Map<string, string>();
    const liveIds = new Set(tasks
      .filter((task) => task.state !== "archived")
      .map((task) => task.id));
    const meaningful: Array<{ taskId: string; mood: CompanionMood }> = [];
    let completed = false;

    for (const task of tasks) {
      const detail = details[task.id];
      const signature = taskSignature(task, detail);
      nextSignatures.set(task.id, signature);
      if (!initializedTasks.current) continue;
      const previous = taskSignatures.current.get(task.id);
      if (!previous || previous === signature) continue;
      const mood = taskMood(task, detail);
      const previousWasActive = previous.startsWith("exploring:") || previous.startsWith("in_progress:");
      if (previousWasActive && task.state === "idle") {
        meaningful.push({ taskId: task.id, mood: "success" });
        completed = true;
      } else if (mood === "attention" || mood === "error" || mood === "review") {
        meaningful.push({ taskId: task.id, mood });
      }
    }

    taskSignatures.current = nextSignatures;
    if (!initializedTasks.current) {
      initializedTasks.current = true;
      previousCueMood.current = "idle";
      setUnread((current) => {
        const next = new Set([...current].filter((taskId) => liveIds.has(taskId)));
        persistUnread(next);
        return next;
      });
      return;
    }
    setUnread((current) => {
      // A task can disappear through archive/delete without another state transition. Prune on
      // every reconciliation so the badge never points at a session that cannot be opened.
      const next = new Set([...current].filter((taskId) => liveIds.has(taskId)));
      meaningful.forEach(({ taskId }) => next.add(taskId));
      persistUnread(next);
      return next;
    });
    if (!meaningful.length) return;
    if (completed) setSuccessUntil(Date.now() + 2_400);
    const loudest = meaningful.sort((a, b) => MOOD_PRIORITY[b.mood] - MOOD_PRIORITY[a.mood])[0];
    if (loudest) {
      if (soundEnabled && shouldPlayCompanionCue(previousCueMood.current, loudest.mood)) {
        void playCue(loudest.mood);
      }
      previousCueMood.current = loudest.mood;
    }
  }, [details, soundEnabled, tasks]);

  useEffect(() => {
    if (successUntil <= Date.now()) return;
    const timer = window.setTimeout(() => setSuccessUntil(0), successUntil - Date.now());
    return () => window.clearTimeout(timer);
  }, [successUntil]);

  useEffect(() => {
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      setBrowserMenuOpen(false);
      if (panelOpenRef.current) void closePanel();
      window.setTimeout(() => avatarRef.current?.focus(), 0);
    };
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [closePanel]);

  const allSessions = useMemo<SessionProgress[]>(() => tasks
    .filter((task) => task.state !== "archived")
    .map((task) => ({
      task,
      mood: taskMood(task, details[task.id]),
      label: taskProgressLabel(task, details[task.id]),
      unread: unread.has(task.id),
    })), [details, tasks, unread]);

  const sessions = useMemo<SessionProgress[]>(() => [...allSessions]
    .sort((a, b) => {
      const priorityDelta = MOOD_PRIORITY[b.mood] - MOOD_PRIORITY[a.mood];
      if (priorityDelta) return priorityDelta;
      const unreadDelta = Number(b.unread) - Number(a.unread);
      return unreadDelta || Date.parse(b.task.updated_at) - Date.parse(a.task.updated_at);
    })
    .slice(0, 4), [allSessions]);

  const mood = useMemo<CompanionMood>(() => {
    // The avatar summarizes every live session, not only the four rows that fit in the panel.
    const moods = allSessions.map((session) => session.mood);
    if (moods.includes("attention")) return "attention";
    if (moods.includes("error")) return "error";
    if (moods.includes("review")) return "review";
    if (moods.includes("working")) return "working";
    if (successUntil > Date.now()) return "success";
    return "idle";
  }, [allSessions, successUntil]);

  const visualState: CompanionVisualState = mood === "idle" && performance ? performance : mood;
  const visualMoodRef = useRef<CompanionVisualState>(visualState);
  const [visualMood, setVisualMood] = useState<CompanionVisualState>(visualState);
  const [previousVisualMood, setPreviousVisualMood] = useState<CompanionVisualState | null>(null);
  useEffect(() => {
    const previous = visualMoodRef.current;
    if (previous === visualState) return;
    visualMoodRef.current = visualState;
    setPreviousVisualMood(previous);
    setVisualMood(visualState);
    const timer = window.setTimeout(() => setPreviousVisualMood(null), 220);
    return () => window.clearTimeout(timer);
  }, [visualState]);

  const beginPerformance = useCallback((next: CompanionPerformance, duration?: number) => {
    if (!enabled || document.visibilityState === "hidden") return;
    if (mood !== "idle" || panelOpenRef.current || shouldReduceMotion(motion)) return;
    if (performanceEndTimer.current !== null || Date.now() < performanceCooldownUntil.current) return;
    setPerformance(next);
    performanceEndTimer.current = window.setTimeout(() => {
      performanceEndTimer.current = null;
      performanceCooldownUntil.current = Date.now() + 450;
      setPerformance(null);
    }, duration ?? (next === "sing" ? 2_600 : 3_600));
  }, [enabled, mood, motion]);

  useEffect(() => {
    if (!enabled || mood !== "idle" || panelOpen || shouldReduceMotion(motion)) {
      if (performanceEndTimer.current !== null || performance) stopPerformance();
      return;
    }
    if (performance || document.visibilityState === "hidden") return;
    // One coarse timeout drives an occasional performance. All movement stays on compositor-only
    // CSS transforms: no pointermove listener, requestAnimationFrame loop, canvas or video decoder.
    const timer = window.setTimeout(() => {
      beginPerformance(Math.random() < 0.5 ? "sing" : "dance");
    }, 8_000 + Math.random() * 8_000);
    return () => window.clearTimeout(timer);
  }, [beginPerformance, enabled, mood, motion, panelOpen, performance, stopPerformance]);

  useEffect(() => () => {
    if (performanceEndTimer.current !== null) window.clearTimeout(performanceEndTimer.current);
  }, []);

  const unreadCount = allSessions.filter((session) => session.unread).length;
  const activeCount = allSessions.filter((session) => session.mood === "working").length;
  const ariaLabel = `R-Code session 助手，${unreadCount} 个未读，${activeCount} 个任务正在运行`;

  useEffect(() => {
    if (!panelOpen) return;
    window.requestAnimationFrame(() => {
      const panel = panelRef.current;
      const target = panel?.querySelector<HTMLElement>(".companion-session-row.is-unread")
        ?? panel?.querySelector<HTMLElement>(".companion-session-row")
        ?? panel?.querySelector<HTMLElement>("[data-companion-panel-close]");
      target?.focus();
    });
  }, [panelOpen]);

  const openCloseMenu = async () => {
    stopPerformance();
    if (!native) {
      setBrowserMenuOpen(true);
      return;
    }
    const item = await MenuItem.new({
      id: "close-companion",
      text: "关闭小助手",
      action: () => void disableCompanion(),
    });
    const menu = await Menu.new({ items: [item] });
    try {
      await menu.popup(undefined, getCurrentWindow());
    } finally {
      await Promise.allSettled([menu.close(), item.close()]);
      avatarRef.current?.focus();
    }
  };

  const showCloseMenu = (event: React.MouseEvent) => {
    event.preventDefault();
    event.stopPropagation();
    void openCloseMenu();
  };

  const handlePointerDown = (event: ReactPointerEvent<HTMLButtonElement>) => {
    if (event.button !== 0 || !event.isPrimary || event.ctrlKey || panelOpen) return;
    stopPerformance();
    pointerDragCandidate.current = true;
    movedDuringPointer.current = false;
    suppressNextClick.current = false;
    dragOrigin.current = null;
    if (native) {
      const appWindow = getCurrentWindow();
      const originRequest = appWindow.outerPosition()
        .then((position) => {
          dragOrigin.current = { x: position.x, y: position.y };
        });
      dragOriginRequest.current = originRequest;
      // startDragging is dispatched to the platform window loop. Keep the candidate alive until
      // the subsequent click consumes it; on Windows/Linux the promise can settle before the OS
      // has delivered its move events.
      void originRequest.then(() => appWindow.startDragging()).catch(() => {
        pointerDragCandidate.current = false;
      });
    }
  };

  const togglePanel = async () => {
    if (native && pointerDragCandidate.current) {
      await dragOriginRequest.current?.catch(() => {});
      // Give the platform move event one short turn to arrive before classifying a release as a
      // click. A true click only pays this delay; dragging remains native and compositor-driven.
      await new Promise((resolve) => window.setTimeout(resolve, 60));
      const origin = dragOrigin.current;
      if (origin) {
        const current = await getCurrentWindow().outerPosition().catch(() => null);
        if (current && Math.abs(current.x - origin.x) + Math.abs(current.y - origin.y) > 2) {
          movedDuringPointer.current = true;
          suppressNextClick.current = true;
        }
      }
    }
    pointerDragCandidate.current = false;
    dragOriginRequest.current = null;
    if (suppressNextClick.current || movedDuringPointer.current) {
      suppressNextClick.current = false;
      movedDuringPointer.current = false;
      return;
    }
    stopPerformance();
    if (panelOpen) void closePanel();
    else void openPanel();
  };

  const navigateToSession = async (taskId: string) => {
    setNavigationError(null);
    if (!native) {
      setUnread((current) => {
        const next = new Set(current);
        next.delete(taskId);
        persistUnread(next);
        return next;
      });
      await closePanel();
      return;
    }
    const requestId = `companion-${Date.now()}-${Math.random().toString(36).slice(2)}`;
    const result = new Promise<CompanionNavigationResult>((resolve) => {
      pendingNavigation.current.set(requestId, resolve);
      window.setTimeout(() => {
        if (!pendingNavigation.current.delete(requestId)) return;
        resolve({ requestId, taskId, ok: false, message: "打开会话超时" });
      }, 5_000);
    });
    const request: CompanionNavigationRequest = { requestId, taskId };
    await emitToWindow(MAIN_WINDOW_LABEL, COMPANION_NAVIGATE_EVENT, request);
    const response = await result;
    if (!response.ok) {
      setNavigationError(response.message ?? "无法打开这个会话");
      return;
    }
    setUnread((current) => {
      const next = new Set(current);
      next.delete(taskId);
      persistUnread(next);
      return next;
    });
    await closePanel();
  };

  if (!enabled) return null;

  return (
    <main
      className={`companion-window-root${panelOpen ? " is-expanded" : ""}${hovered ? " is-hovered" : ""} panel-${panelSide} state-${mood} performance-${performance ?? "none"} motion-${motion}${minimized ? " is-mini" : ""}`}
      style={{ "--companion-sprite": `url(${companionSprite})` } as CSSProperties}
      onContextMenu={showCloseMenu}
      aria-label="R-Code session 助手窗口"
    >
      {panelOpen && (
        <section ref={panelRef} id="companion-session-panel" className="companion-session-panel" role="dialog" aria-modal="false" aria-labelledby="companion-session-title">
          <header>
            <div>
              <span className="companion-eyebrow">SESSION PULSE</span>
              <h1 id="companion-session-title">最近任务</h1>
            </div>
            <button data-companion-panel-close type="button" onClick={() => void closePanel()} aria-label="关闭最近任务">×</button>
          </header>
          <div className="companion-session-list" role="list">
            {sessions.length === 0 ? (
              <p className="companion-empty">还没有会话，我会在这里反馈进度。</p>
            ) : sessions.map((session) => (
              <div role="listitem" key={session.task.id}>
                <button
                  type="button"
                  className={`companion-session-row state-${session.mood}${session.unread ? " is-unread" : ""}`}
                  onClick={() => void navigateToSession(session.task.id)}
                  aria-label={`${session.task.title}，${session.label}，${relativeTime(session.task.updated_at)}${session.unread ? "，未读" : ""}`}
                >
                  <span className="companion-session-dot" aria-hidden="true" />
                  <span className="companion-session-copy">
                    <strong>{session.task.title}</strong>
                    <small>{session.label} · {relativeTime(session.task.updated_at)}</small>
                  </span>
                  {session.unread && <i aria-hidden="true" />}
                  <span className="companion-session-arrow" aria-hidden="true">›</span>
                </button>
              </div>
            ))}
          </div>
          {navigationError && <p className="companion-navigation-error" role="alert">{navigationError}</p>}
        </section>
      )}

      <button
        ref={avatarRef}
        type="button"
        className="companion-avatar"
        aria-label={ariaLabel}
        aria-expanded={panelOpen}
        aria-controls="companion-session-panel"
        title={`${MOOD_LABEL[mood]} · 拖动移动，点击查看任务，右键关闭`}
        onPointerDown={handlePointerDown}
        onPointerEnter={() => {
          setHovered(true);
          if (mood === "idle") {
            const next = interactionTurn.current % 2 === 0 ? "sing" : "dance";
            interactionTurn.current += 1;
            beginPerformance(next);
          }
        }}
        onPointerLeave={() => setHovered(false)}
        onClick={() => void togglePanel()}
        onContextMenu={showCloseMenu}
        onKeyDown={(event) => {
          if (event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) {
            event.preventDefault();
            void openCloseMenu();
          }
        }}
        draggable={false}
        onDragStart={(event) => event.preventDefault()}
      >
        <span className="companion-aura" aria-hidden="true" />
        <span className="companion-hover-spark" aria-hidden="true">♪</span>
        {previousVisualMood && (
          <span
            key={`leaving-${previousVisualMood}`}
            className="companion-frame-layer is-leaving"
            aria-hidden="true"
          >
            <span className={`companion-sprite-frame sprite-state-${previousVisualMood}`} />
          </span>
        )}
        <span
          key={`current-${visualMood}`}
          className="companion-frame-layer is-current"
          aria-hidden="true"
        >
          <span className={`companion-sprite-frame sprite-state-${visualMood}`} />
        </span>
        {unreadCount > 0 && (
          <span className="companion-unread-badge" aria-hidden="true">
            {unreadCount > 9 ? "9+" : unreadCount}
          </span>
        )}
        <span className="companion-ground-shadow" aria-hidden="true" />
      </button>

      {browserMenuOpen && (
        <div className="companion-browser-menu" role="menu">
          <button type="button" role="menuitem" autoFocus onClick={() => void disableCompanion()}>
            关闭小助手
          </button>
        </div>
      )}
      <p className="sr-only" aria-live="polite">{MOOD_LABEL[mood]}</p>
    </main>
  );
}
