import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
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
import { agentAbort, onAgentEvent } from "../../lib/ipc";
import { isTaskLive } from "../../lib/presentation";
import type { Task, TaskDetail } from "../../lib/types";
import {
  companionPreferenceSnapshot,
  useCompanionStore,
  type CompanionMotion,
  type CompanionPreferences,
} from "../../store/companion";
import { useTasksStore } from "../../store/tasks";
import { shouldPlayCompanionCue, type CompanionMood } from "./policy";
import { CompanionSprite, type CompanionSpriteState } from "./CompanionSprite";
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
const PULSE = { width: 420, height: 360 };
const PULSE_ROW_STRIDE = 80;
const MAX_PULSE_SESSIONS = 2;
const TRACKING_FOOTER_HEIGHT = 40;
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

interface RenderedSessionProgress extends SessionProgress {
  exiting: boolean;
}

type CompanionPerformance = "sing" | "dance";
type CompanionVisualState = CompanionSpriteState;
type CompanionPanelSide = "left" | "right";
type CompanionAvatarVertical = "top" | "bottom";

interface CompanionWindowPlacement {
  avatarVertical: CompanionAvatarVertical;
  panelSide: CompanionPanelSide;
  position: PersistedPosition;
}

interface CompanionCompactLayout {
  minimized: boolean;
  hasTracking: boolean;
  rows: number;
}

function shouldReduceMotion(motion: CompanionMotion): boolean {
  if (motion === "reduced") return true;
  if (motion === "full") return false;
  return window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;
}

function collapsedSize(minimized: boolean) {
  return minimized ? COLLAPSED_MINI : COLLAPSED_FULL;
}

function companionFootprint(minimized: boolean, hasTracking: boolean) {
  const compact = collapsedSize(minimized);
  return hasTracking
    ? { width: compact.width, height: compact.height + TRACKING_FOOTER_HEIGHT }
    : compact;
}

function pulseWindowSize(rows: number) {
  return {
    width: PULSE.width,
    height: PULSE.height + Math.max(0, rows - 1) * PULSE_ROW_STRIDE,
  };
}

export function compactLayoutForNativeHeight(
  minimized: boolean,
  hasTracking: boolean,
  requestedRows: number,
  logicalHeight: number,
): CompanionCompactLayout {
  if (!hasTracking || logicalHeight < PULSE.height - 2) {
    return { minimized, hasTracking: false, rows: 0 };
  }
  const capacity = 1 + Math.max(
    0,
    Math.floor((logicalHeight - PULSE.height + 2) / PULSE_ROW_STRIDE),
  );
  return {
    minimized,
    hasTracking: true,
    rows: Math.max(1, Math.min(requestedRows, capacity)),
  };
}

/** Tauri serializes PhysicalPosition as i32; monitor/DPI math may leave IEEE-754 fractions. */
export function integerPhysicalPosition(position: { x: number; y: number }): { x: number; y: number } {
  return {
    x: Math.round(position.x),
    y: Math.round(position.y),
  };
}

function nativePhysicalPosition(position: { x: number; y: number }): PhysicalPosition {
  const normalized = integerPhysicalPosition(position);
  return new PhysicalPosition(normalized.x, normalized.y);
}

function positionForMonitor(position: PersistedPosition, width: number, height: number, monitor: Monitor) {
  const area = monitor.workArea;
  const maxX = Math.max(area.position.x, area.position.x + area.size.width - width);
  const maxY = Math.max(area.position.y, area.position.y + area.size.height - height);
  return {
    x: Math.min(Math.max(position.x, area.position.x), maxX),
    y: Math.min(Math.max(position.y, area.position.y), maxY),
  };
}

/** Restore a compact anchor using the destination monitor's physical scale, not the scale of the
 * monitor where the hidden WebView happened to start. This is critical for Windows mixed-DPI
 * layouts, whose monitor work areas and native positions are all physical coordinates. */
export function restoredAnchorForMonitor(
  position: Pick<PersistedPosition, "x" | "y" | "relativeX" | "relativeY">,
  logicalSize: { width: number; height: number },
  monitor: Monitor,
): PersistedPosition {
  const width = logicalSize.width * monitor.scaleFactor;
  const height = logicalSize.height * monitor.scaleFactor;
  const area = monitor.workArea;
  const restored = Number.isFinite(position.relativeX) && Number.isFinite(position.relativeY)
    ? {
      x: area.position.x + Math.max(0, Math.min(1, position.relativeX!)) * Math.max(0, area.size.width - width),
      y: area.position.y + Math.max(0, Math.min(1, position.relativeY!)) * Math.max(0, area.size.height - height),
    }
    : position;
  return positionForMonitor(restored, width, height, monitor);
}

/**
 * Expand around the pet's compact top-left anchor, choosing the screen quadrant with room first.
 * `panelSide` names the content side, so a left panel keeps the pet on the window's right edge.
 */
function placementAroundAvatar(
  avatar: PersistedPosition,
  compact: { width: number; height: number },
  target: { width: number; height: number },
  scale: number,
  monitor: Monitor | null,
  verticalPlacement: "adaptive" | "above" = "adaptive",
): CompanionWindowPlacement {
  const compactWidth = compact.width * scale;
  const compactHeight = compact.height * scale;
  const targetWidth = target.width * scale;
  const targetHeight = target.height * scale;
  const deltaX = Math.max(0, targetWidth - compactWidth);
  const deltaY = Math.max(0, targetHeight - compactHeight);

  let panelSide: CompanionPanelSide = "left";
  let avatarVertical: CompanionAvatarVertical = "bottom";
  if (monitor) {
    const area = monitor.workArea;
    const roomLeft = avatar.x - area.position.x;
    const roomRight = area.position.x + area.size.width - (avatar.x + compactWidth);
    const roomAbove = avatar.y - area.position.y;
    const roomBelow = area.position.y + area.size.height - (avatar.y + compactHeight);
    panelSide = roomLeft >= deltaX || roomLeft >= roomRight ? "left" : "right";
    // Automatic progress is a spatial attachment to the assistant, not a generic popover. Keep
    // it above the head even near a monitor edge; clamping moves the combined composition instead
    // of flipping the progress card underneath the assistant.
    if (verticalPlacement === "adaptive") {
      avatarVertical = roomAbove >= deltaY || roomAbove >= roomBelow ? "bottom" : "top";
    }
  }

  const raw = {
    x: panelSide === "left" ? avatar.x - deltaX : avatar.x,
    y: avatarVertical === "bottom" ? avatar.y - deltaY : avatar.y,
  };
  return {
    avatarVertical,
    panelSide,
    position: monitor ? positionForMonitor(raw, targetWidth, targetHeight, monitor) : raw,
  };
}

function avatarAnchorFromWindow(
  position: PersistedPosition,
  size: { width: number; height: number },
  compact: { width: number; height: number },
  scale: number,
  panelSide: CompanionPanelSide,
  avatarVertical: CompanionAvatarVertical,
): PersistedPosition {
  return {
    x: position.x + (panelSide === "left" ? Math.max(0, size.width - compact.width * scale) : 0),
    y: position.y + (avatarVertical === "bottom" ? Math.max(0, size.height - compact.height * scale) : 0),
  };
}

function persistedPosition(
  position: PersistedPosition,
  width: number,
  height: number,
  monitor: Monitor,
): PersistedPosition {
  const area = monitor.workArea;
  return {
    ...position,
    monitorName: monitor.name,
    relativeX: (position.x - area.position.x) / Math.max(1, area.size.width - width),
    relativeY: (position.y - area.position.y) / Math.max(1, area.size.height - height),
    scaleFactor: monitor.scaleFactor,
  };
}

function pendingPermissionCount(detail: TaskDetail | undefined): number {
  return detail?.permissions.filter((permission) => permission.decision === "pending").length ?? 0;
}

function taskMood(task: Task, detail: TaskDetail | undefined): CompanionMood {
  if (pendingPermissionCount(detail) > 0) return "attention";
  if (isTaskLive(task, detail)) return "working";
  if (task.state === "interrupted") return "error";
  if (task.state === "review_ready") return "review";
  return "idle";
}

function taskSignature(task: Task, detail: TaskDetail | undefined): string {
  return `${task.state}:${Number(isTaskLive(task, detail))}:${pendingPermissionCount(detail)}`;
}

function signatureWasLive(signature: string | undefined): boolean {
  return signature?.split(":")[1] === "1";
}

function taskProgressLabel(task: Task, detail: TaskDetail | undefined): string {
  const pending = pendingPermissionCount(detail);
  if (pending > 0) return `${pending} 项授权等待确认`;
  if (isTaskLive(task, detail)) {
    return task.state === "exploring" ? "正在分析项目" : "正在实施";
  }
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
  const [panelSide, setPanelSide] = useState<CompanionPanelSide>("left");
  const [avatarVertical, setAvatarVertical] = useState<CompanionAvatarVertical>("bottom");
  const [browserMenuOpen, setBrowserMenuOpen] = useState(false);
  const [navigationError, setNavigationError] = useState<string | null>(null);
  const [stoppingTaskIds, setStoppingTaskIds] = useState<Set<string>>(() => new Set());
  const [stopErrors, setStopErrors] = useState<Record<string, string>>({});
  const [navigatingTaskId, setNavigatingTaskId] = useState<string | null>(null);
  const [unread, setUnread] = useState<Set<string>>(readUnread);
  const [successUntil, setSuccessUntil] = useState(0);
  const [performance, setPerformance] = useState<CompanionPerformance | null>(null);
  const [hovered, setHovered] = useState(false);
  const [trackingCollapsed, setTrackingCollapsed] = useState(false);
  const [trackingShowingAll, setTrackingShowingAll] = useState(false);
  const [nativeCompactLayout, setNativeCompactLayout] = useState<CompanionCompactLayout>(() => ({
    minimized,
    hasTracking: false,
    rows: 0,
  }));
  const initializedTasks = useRef(false);
  const taskSignatures = useRef(new Map<string, string>());
  const anchorPosition = useRef<PersistedPosition | null>(null);
  const panelOpenRef = useRef(panelOpen);
  const panelSideRef = useRef<CompanionPanelSide>(panelSide);
  const avatarVerticalRef = useRef<CompanionAvatarVertical>(avatarVertical);
  const nativeScaleFactorRef = useRef(1);
  const nativeLayoutGeneration = useRef(0);
  const nativeLayoutInFlight = useRef(false);
  const suppressNativeMovesUntil = useRef(0);
  const pulseOpenRef = useRef(false);
  const pulseRowsRef = useRef(1);
  const movedDuringPointer = useRef(false);
  const pointerDragCandidate = useRef(false);
  const suppressNextClick = useRef(false);
  const dragOrigin = useRef<PersistedPosition | null>(null);
  const dragOriginRequest = useRef<Promise<void> | null>(null);
  const receivedPreferences = useRef(false);
  const previousCueMood = useRef<CompanionMood>("idle");
  const avatarRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLElement>(null);
  const pulseListRef = useRef<HTMLDivElement>(null);
  const performanceEndTimer = useRef<number | null>(null);
  const hoverIntentTimer = useRef<number | null>(null);
  const performanceCooldownUntil = useRef(0);
  const interactionTurn = useRef(0);
  const reconciliationInFlight = useRef<Promise<void> | null>(null);
  const pendingNavigation = useRef(new Map<string, (result: CompanionNavigationResult) => void>());
  const navigatingTaskIdRef = useRef<string | null>(null);
  const stoppingTaskIdsRef = useRef(new Set<string>());
  const sessionNodes = useRef(new Map<string, HTMLElement>());
  const previousSessionRects = useRef(new Map<string, DOMRect>());
  const previousVisibleSessions = useRef<SessionProgress[]>([]);
  const sessionExitTimers = useRef(new Map<string, number>());

  const native = isTauriRuntime();

  const commitNativeCompactLayout = useCallback((next: CompanionCompactLayout) => {
    setNativeCompactLayout((current) => (
      current.minimized === next.minimized
      && current.hasTracking === next.hasTracking
      && current.rows === next.rows
        ? current
        : next
    ));
  }, []);

  useEffect(() => {
    panelOpenRef.current = panelOpen;
  }, [panelOpen]);

  const applyWindowPlacement = useCallback((placement: CompanionWindowPlacement) => {
    panelSideRef.current = placement.panelSide;
    avatarVerticalRef.current = placement.avatarVertical;
    setPanelSide(placement.panelSide);
    setAvatarVertical(placement.avatarVertical);
  }, []);

  const beginNativeLayout = useCallback(() => {
    const generation = nativeLayoutGeneration.current + 1;
    nativeLayoutGeneration.current = generation;
    nativeLayoutInFlight.current = true;
    return generation;
  }, []);

  const nativeLayoutIsCurrent = useCallback(
    (generation: number) => nativeLayoutGeneration.current === generation,
    [],
  );

  const finishNativeLayout = useCallback((generation: number) => {
    if (nativeLayoutGeneration.current !== generation) return;
    nativeLayoutInFlight.current = false;
    // Tao/WebKit can deliver the move event just after setPosition resolves. Do not reinterpret
    // that trailing programmatic event as a user drag and persist a panel-window coordinate.
    suppressNativeMovesUntil.current = Date.now() + 260;
  }, []);

  const stopPerformance = useCallback(() => {
    if (hoverIntentTimer.current !== null) {
      window.clearTimeout(hoverIntentTimer.current);
      hoverIntentTimer.current = null;
    }
    if (performanceEndTimer.current !== null) {
      window.clearTimeout(performanceEndTimer.current);
      performanceEndTimer.current = null;
    }
    performanceCooldownUntil.current = Date.now() + 650;
    setPerformance(null);
  }, []);

  const closePanel = useCallback(async () => {
    panelOpenRef.current = false;
    setPanelOpen(false);
    setNavigationError(null);
    window.setTimeout(() => avatarRef.current?.focus(), 0);
    if (!native) return;
    const appWindow = getCurrentWindow();
    const hasTracking = pulseOpenRef.current;
    const compact = companionFootprint(minimized, hasTracking);
    const nextSize = hasTracking ? pulseWindowSize(pulseRowsRef.current) : collapsedSize(minimized);
    const generation = beginNativeLayout();
    try {
      await appWindow.setSize(new LogicalSize(nextSize.width, nextSize.height));
      if (!nativeLayoutIsCurrent(generation)) return;
      if (anchorPosition.current) {
        const [scale, monitor] = await Promise.all([appWindow.scaleFactor(), currentMonitor()]);
        nativeScaleFactorRef.current = scale;
        if (!nativeLayoutIsCurrent(generation)) return;
        const placement = pulseOpenRef.current
          ? placementAroundAvatar(anchorPosition.current, compact, nextSize, scale, monitor, "above")
          : {
            avatarVertical: "bottom" as const,
            panelSide: "left" as const,
            position: anchorPosition.current,
          };
        applyWindowPlacement(placement);
        await appWindow.setPosition(nativePhysicalPosition(placement.position));
      }
      if (nativeLayoutIsCurrent(generation)) {
        const [actualSize, actualScale] = await Promise.all([
          appWindow.outerSize(),
          appWindow.scaleFactor(),
        ]);
        commitNativeCompactLayout(compactLayoutForNativeHeight(
          minimized,
          hasTracking,
          pulseRowsRef.current,
          actualSize.height / actualScale,
        ));
      }
    } finally {
      finishNativeLayout(generation);
    }
  }, [
    applyWindowPlacement,
    beginNativeLayout,
    commitNativeCompactLayout,
    finishNativeLayout,
    minimized,
    native,
    nativeLayoutIsCurrent,
  ]);

  const resetNativePosition = useCallback(async () => {
    if (!native) return;
    const appWindow = getCurrentWindow();
    const monitor = await primaryMonitor();
    if (!monitor) return;
    const [size, currentScale] = await Promise.all([appWindow.outerSize(), appWindow.scaleFactor()]);
    const targetScale = monitor.scaleFactor;
    nativeScaleFactorRef.current = targetScale;
    const hasTracking = pulseOpenRef.current;
    const compact = companionFootprint(minimized, hasTracking);
    const compactWidth = compact.width * targetScale;
    const compactHeight = compact.height * targetScale;
    const avatarAnchor = {
      x: monitor.workArea.position.x + monitor.workArea.size.width - compactWidth - WINDOW_INSET * targetScale,
      y: monitor.workArea.position.y + monitor.workArea.size.height - compactHeight - WINDOW_INSET * targetScale,
    };
    const compactAnchor = positionForMonitor(avatarAnchor, compactWidth, compactHeight, monitor);
    const target = { width: size.width / currentScale, height: size.height / currentScale };
    const placement = target.width > compact.width + 1 || target.height > compact.height + 1
      ? placementAroundAvatar(
        compactAnchor,
        compact,
        target,
        targetScale,
        monitor,
        hasTracking ? "above" : "adaptive",
      )
      : {
        avatarVertical: "bottom" as const,
        panelSide: "left" as const,
        position: compactAnchor,
      };
    applyWindowPlacement(placement);
    await appWindow.setPosition(nativePhysicalPosition(placement.position));
    anchorPosition.current = compactAnchor;
    persistPosition(persistedPosition(compactAnchor, compactWidth, compactHeight, monitor));
  }, [applyWindowPlacement, minimized, native]);

  const restoreNativePosition = useCallback(async () => {
    if (!native) return;
    const appWindow = getCurrentWindow();
    const saved = readPosition();
    const [monitors, size, currentScale] = await Promise.all([
      availableMonitors(),
      appWindow.outerSize(),
      appWindow.scaleFactor(),
    ]);
    const hasTracking = pulseOpenRef.current;
    const compact = companionFootprint(minimized, hasTracking);
    if (saved) {
      const named = monitors.find((monitor) => monitor.name && monitor.name === saved.monitorName);
      const savedScale = saved.scaleFactor && saved.scaleFactor > 0
        ? saved.scaleFactor
        : currentScale;
      const savedWidth = compact.width * savedScale;
      const savedHeight = compact.height * savedScale;
      const intersecting = monitors
        .filter((monitor) => intersectsWorkArea(saved, savedWidth, savedHeight, monitor))
        .sort((a, b) => workAreaIntersection(saved, savedWidth, savedHeight, b)
          - workAreaIntersection(saved, savedWidth, savedHeight, a))[0];
      const monitor = named ?? intersecting;
      if (monitor) {
        const targetScale = monitor.scaleFactor;
        const compactWidth = compact.width * targetScale;
        const compactHeight = compact.height * targetScale;
        const compactAnchor = named
          ? restoredAnchorForMonitor(saved, compact, monitor)
          : positionForMonitor(saved, compactWidth, compactHeight, monitor);
        const target = { width: size.width / currentScale, height: size.height / currentScale };
        const placement = target.width > compact.width + 1 || target.height > compact.height + 1
          ? placementAroundAvatar(
            compactAnchor,
            compact,
            target,
            targetScale,
            monitor,
            hasTracking ? "above" : "adaptive",
          )
          : {
            avatarVertical: "bottom" as const,
            panelSide: "left" as const,
            position: compactAnchor,
          };
        applyWindowPlacement(placement);
        nativeScaleFactorRef.current = targetScale;
        anchorPosition.current = compactAnchor;
        persistPosition(persistedPosition(compactAnchor, compactWidth, compactHeight, monitor));
        await appWindow.setPosition(nativePhysicalPosition(placement.position));
        return;
      }
    }
    await resetNativePosition();
  }, [applyWindowPlacement, minimized, native, resetNativePosition]);

  const openPanel = useCallback(async () => {
    setNavigationError(null);
    if (!native) {
      setPanelOpen(true);
      return;
    }
    const appWindow = getCurrentWindow();
    panelOpenRef.current = true;
    const [position, currentSize, scale, monitor] = await Promise.all([
      appWindow.outerPosition(),
      appWindow.outerSize(),
      appWindow.scaleFactor(),
      currentMonitor(),
    ]);
    nativeScaleFactorRef.current = scale;
    const currentFootprint = companionFootprint(minimized, pulseOpenRef.current);
    const compact = collapsedSize(minimized);
    // The actual window position is authoritative. A drag can be followed by a click before the
    // debounced persistence timer fires; using a cached anchor there would make close jump back.
    const avatarPosition = avatarAnchorFromWindow(
      position,
      currentSize,
      currentFootprint,
      scale,
      panelSideRef.current,
      avatarVerticalRef.current,
    );
    anchorPosition.current = avatarPosition;
    const placement = placementAroundAvatar(avatarPosition, compact, EXPANDED, scale, monitor);
    applyWindowPlacement(placement);
    setPanelOpen(true);
    const generation = beginNativeLayout();
    try {
      await appWindow.setSize(new LogicalSize(EXPANDED.width, EXPANDED.height));
      if (!nativeLayoutIsCurrent(generation)) return;
      await appWindow.setPosition(nativePhysicalPosition(placement.position));
    } finally {
      finishNativeLayout(generation);
    }
  }, [applyWindowPlacement, beginNativeLayout, finishNativeLayout, minimized, native, nativeLayoutIsCurrent]);

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
    let latestAnchor: PersistedPosition | null = null;
    let latestFootprint = collapsedSize(useCompanionStore.getState().minimized);
    void appWindow.onMoved(({ payload }) => {
      if (pointerDragCandidate.current) movedDuringPointer.current = true;
      if (
        panelOpenRef.current
        || nativeLayoutInFlight.current
        || Date.now() < suppressNativeMovesUntil.current
      ) return;
      const compact = companionFootprint(
        useCompanionStore.getState().minimized,
        pulseOpenRef.current,
      );
      latestFootprint = compact;
      const scale = nativeScaleFactorRef.current;
      // Snapshot the layout at the move event. A pulse can finish before the persistence debounce;
      // deriving the anchor later from mutable refs would reinterpret this coordinate system.
      latestAnchor = pulseOpenRef.current
        ? avatarAnchorFromWindow(
          { x: payload.x, y: payload.y },
          {
            width: PULSE.width * scale,
            height: pulseWindowSize(pulseRowsRef.current).height * scale,
          },
          compact,
          scale,
          panelSideRef.current,
          avatarVerticalRef.current,
        )
        : { x: payload.x, y: payload.y };
      anchorPosition.current = latestAnchor;
      moveGeneration += 1;
      const generation = moveGeneration;
      if (persistTimer !== null) window.clearTimeout(persistTimer);
      // Native move events can arrive at display refresh rate. Persist only the trailing position
      // after movement settles, so dragging never creates an IPC/localStorage write storm.
      persistTimer = window.setTimeout(() => {
        persistTimer = null;
        const settledAnchor = latestAnchor;
        if (!settledAnchor) return;
        void Promise.all([appWindow.scaleFactor(), currentMonitor()]).then(([scale, monitor]) => {
          if (disposed || generation !== moveGeneration) return;
          nativeScaleFactorRef.current = scale;
          persistPosition(monitor
            ? persistedPosition(
              settledAnchor,
              latestFootprint.width * scale,
              latestFootprint.height * scale,
              monitor,
            )
            : settledAnchor);
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
  }, [native]);

  useEffect(() => {
    if (!native) return;
    const appWindow = getCurrentWindow();
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void appWindow.onScaleChanged(({ payload }) => {
      // Windows emits this while crossing monitors with different scaling. Update the drag/pulse
      // coordinate basis immediately; persistence still queries the settled scale once more.
      nativeScaleFactorRef.current = payload.scaleFactor;
    }).then((cleanup) => {
      if (disposed) cleanup();
      else unlisten = cleanup;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [native]);

  useEffect(() => {
    if (!native) return;
    const appWindow = getCurrentWindow();
    let disposed = false;
    const update = async () => {
      if (!enabled) {
        await appWindow.setIgnoreCursorEvents(false).catch(() => {});
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
      try {
        await closePanel();
      } catch (error) {
        console.warn("Companion compact layout could not be restored; showing at its current size.", error);
      }
      if (disposed) return;
      try {
        await restoreNativePosition();
      } catch (error) {
        // Position persistence is convenience state. A stale/corrupt coordinate must never leave
        // an enabled companion permanently hidden.
        console.warn("Companion position could not be restored; showing at its current position.", error);
      }
      if (disposed) return;
      // A previously hidden Windows window can still carry WS_EX_TRANSPARENT. Restore native
      // interactivity before showing it so the user's first click cannot fall through.
      await appWindow.setIgnoreCursorEvents(false).catch(() => {});
      if (disposed) return;
      await appWindow.show();
    };
    void update().catch((error) => {
      console.error("Companion window could not be shown.", error);
    });
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
          return isTaskLive(task, detail)
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
        const active = snapshot.tasks.some((task) => isTaskLive(task, snapshot.details[task.id])
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
      const previousWasActive = previous.split(":")[1] === "1";
      if (previousWasActive && !isTaskLive(task, detail) && task.state === "idle") {
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
    }), [allSessions]);

  const trackedSessions = useMemo<SessionProgress[]>(() => [...allSessions]
    .filter((session) => {
      const live = isTaskLive(session.task, details[session.task.id]);
      // Completion/error/review notifications are acknowledged in the task-signature effect below.
      // Keep the previous live footprint for that one render so the native window never shrinks
      // underneath an avatar that has already switched back to the tracked (40 px footer) layout.
      const pendingUnreadTransition = !live
        && signatureWasLive(taskSignatures.current.get(session.task.id));
      return live || session.unread || pendingUnreadTransition;
    })
    .sort((a, b) => {
      // The red badge is an unread count, so its rows must win the compact viewport. Live but
      // already-seen work remains reachable through the explicit overflow/full panel.
      const unreadDelta = Number(b.unread) - Number(a.unread);
      if (unreadDelta) return unreadDelta;
      const priorityDelta = MOOD_PRIORITY[b.mood] - MOOD_PRIORITY[a.mood];
      if (priorityDelta) return priorityDelta;
      return Date.parse(b.task.updated_at) - Date.parse(a.task.updated_at);
    }), [allSessions, details]);
  const pulseSessions = useMemo(
    () => trackedSessions.slice(0, MAX_PULSE_SESSIONS),
    [trackedSessions],
  );
  const hiddenPulseCount = Math.max(0, trackedSessions.length - pulseSessions.length);
  const pulseRows = pulseSessions.length + Number(hiddenPulseCount > 0);

  const hasTracking = trackedSessions.length > 0;
  // React can describe the next task-row layout before WebView2/WKWebView has completed the native
  // resize. Keep rendering the last committed compact geometry until the native promise resolves;
  // otherwise a second row is painted into a one-row window and the avatar footer clips its head.
  const displayLayout = native
    ? nativeCompactLayout
    : { minimized, hasTracking, rows: hasTracking ? pulseRows : 0 };
  const displayHasTracking = displayLayout.hasTracking;
  const displayPulseRows = displayHasTracking
    ? Math.min(pulseRows, Math.max(1, displayLayout.rows))
    : 0;
  const compactDisplayedPulseSessions = pulseSessions.slice(
    0,
    Math.min(pulseSessions.length, displayPulseRows),
  );
  const displayPulseOverflow = hiddenPulseCount > 0
    && displayPulseRows > compactDisplayedPulseSessions.length;
  const showingAllPulseSessions = trackingShowingAll && displayPulseOverflow;
  const displayedPulseSessions = showingAllPulseSessions
    ? trackedSessions
    : compactDisplayedPulseSessions;
  const pulseVisible = !panelOpen && displayHasTracking && !trackingCollapsed;
  pulseOpenRef.current = hasTracking;
  pulseRowsRef.current = Math.max(1, pulseRows);

  useEffect(() => {
    if (!hasTracking) setTrackingCollapsed(false);
    if (!hasTracking || hiddenPulseCount === 0 || trackingCollapsed || panelOpen) {
      setTrackingShowingAll(false);
    }
  }, [hasTracking, hiddenPulseCount, panelOpen, trackingCollapsed]);

  useEffect(() => {
    if (!trackingShowingAll) pulseListRef.current?.scrollTo({ top: 0 });
  }, [trackingShowingAll]);

  useEffect(() => {
    if (!native || panelOpen) return;
    const appWindow = getCurrentWindow();
    let disposed = false;
    const resize = async () => {
      while (nativeLayoutInFlight.current && !disposed) {
        await new Promise((resolve) => window.setTimeout(resolve, 32));
      }
      if (disposed || panelOpenRef.current) return;
      const currentSize = await appWindow.outerSize();
      const scale = await appWindow.scaleFactor();
      nativeScaleFactorRef.current = scale;
      const target = hasTracking ? pulseWindowSize(pulseRows) : collapsedSize(minimized);
      const targetWidth = target.width * scale;
      const targetHeight = target.height * scale;
      if (
        Math.abs(currentSize.width - targetWidth) < 1
        && Math.abs(currentSize.height - targetHeight) < 1
      ) {
        commitNativeCompactLayout(compactLayoutForNativeHeight(
          minimized,
          hasTracking,
          pulseRows,
          currentSize.height / scale,
        ));
        return;
      }
      const [position, monitor] = await Promise.all([appWindow.outerPosition(), currentMonitor()]);
      const compact = companionFootprint(minimized, hasTracking);
      const avatarAnchor = anchorPosition.current ?? avatarAnchorFromWindow(
        position,
        currentSize,
        compact,
        scale,
        panelSideRef.current,
        avatarVerticalRef.current,
      );
      const generation = beginNativeLayout();
      try {
        const placement = hasTracking
          ? placementAroundAvatar(avatarAnchor, compact, target, scale, monitor, "above")
          : {
            avatarVertical: "bottom" as const,
            panelSide: "left" as const,
            position: avatarAnchor,
          };
        applyWindowPlacement(placement);
        await appWindow.setSize(new LogicalSize(target.width, target.height));
        if (disposed || !nativeLayoutIsCurrent(generation)) return;
        await appWindow.setPosition(nativePhysicalPosition(placement.position));
        if (!disposed && nativeLayoutIsCurrent(generation)) {
          const [actualSize, actualScale] = await Promise.all([
            appWindow.outerSize(),
            appWindow.scaleFactor(),
          ]);
          if (disposed || !nativeLayoutIsCurrent(generation)) return;
          commitNativeCompactLayout(compactLayoutForNativeHeight(
            minimized,
            hasTracking,
            pulseRows,
            actualSize.height / actualScale,
          ));
        }
      } finally {
        finishNativeLayout(generation);
      }
    };
    void resize().catch(() => {});
    return () => { disposed = true; };
  }, [
    applyWindowPlacement,
    beginNativeLayout,
    commitNativeCompactLayout,
    finishNativeLayout,
    minimized,
    native,
    nativeLayoutIsCurrent,
    panelOpen,
    hasTracking,
    pulseRows,
  ]);

  const [renderedSessions, setRenderedSessions] = useState<RenderedSessionProgress[]>(() =>
    sessions.map((session) => ({ ...session, exiting: false })),
  );

  useEffect(() => {
    const nextIds = new Set(sessions.map((session) => session.task.id));
    for (const session of sessions) {
      const timer = sessionExitTimers.current.get(session.task.id);
      if (timer !== undefined) window.clearTimeout(timer);
      sessionExitTimers.current.delete(session.task.id);
    }
    const removed = previousVisibleSessions.current.filter((session) => !nextIds.has(session.task.id));
    setRenderedSessions((current) => {
      const currentById = new Map(current.map((session) => [session.task.id, session]));
      const exiting = current
        .filter((session) => !nextIds.has(session.task.id))
        .map((session) => ({ ...session, exiting: true }));
      for (const session of removed) {
        if (!currentById.has(session.task.id)) exiting.push({ ...session, exiting: true });
      }
      return [
        ...sessions.map((session) => ({ ...session, exiting: false })),
        ...exiting,
      ];
    });
    for (const session of removed) {
      if (sessionExitTimers.current.has(session.task.id)) continue;
      const timer = window.setTimeout(() => {
        sessionExitTimers.current.delete(session.task.id);
        setRenderedSessions((current) => current.filter((item) => item.task.id !== session.task.id));
      }, 190);
      sessionExitTimers.current.set(session.task.id, timer);
    }
    previousVisibleSessions.current = sessions;
  }, [sessions]);

  useEffect(() => () => {
    for (const timer of sessionExitTimers.current.values()) window.clearTimeout(timer);
    sessionExitTimers.current.clear();
  }, []);

  useLayoutEffect(() => {
    const nextRects = new Map<string, DOMRect>();
    for (const [taskId, node] of sessionNodes.current) nextRects.set(taskId, node.getBoundingClientRect());
    if (!shouldReduceMotion(motion)) {
      for (const [taskId, nextRect] of nextRects) {
        const previousRect = previousSessionRects.current.get(taskId);
        const node = sessionNodes.current.get(taskId);
        if (!previousRect || !node || typeof node.animate !== "function") continue;
        const deltaY = previousRect.top - nextRect.top;
        if (Math.abs(deltaY) < 1) continue;
        node.animate(
          [{ transform: `translateY(${deltaY}px)` }, { transform: "translateY(0)" }],
          { duration: 220, easing: "cubic-bezier(.2,.8,.2,1)" },
        );
      }
    }
    previousSessionRects.current = nextRects;
  }, [motion, renderedSessions]);

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

  const beginPerformance = useCallback((next: CompanionPerformance, duration?: number) => {
    if (!enabled || document.visibilityState === "hidden") return;
    if (mood !== "idle" || panelOpenRef.current || shouldReduceMotion(motion)) return;
    if (performanceEndTimer.current !== null || Date.now() < performanceCooldownUntil.current) return;
    setPerformance(next);
    performanceEndTimer.current = window.setTimeout(() => {
      performanceEndTimer.current = null;
      performanceCooldownUntil.current = Date.now() + 650;
      setPerformance(null);
    }, duration ?? (next === "sing" ? 1_550 : 1_800));
  }, [enabled, mood, motion]);

  useEffect(() => () => {
    if (hoverIntentTimer.current !== null) window.clearTimeout(hoverIntentTimer.current);
    if (performanceEndTimer.current !== null) window.clearTimeout(performanceEndTimer.current);
  }, []);

  useEffect(() => {
    if (mood === "idle" && !panelOpen && enabled && !shouldReduceMotion(motion)) return;
    if (performanceEndTimer.current !== null || hoverIntentTimer.current !== null || performance) {
      stopPerformance();
    }
  }, [enabled, mood, motion, panelOpen, performance, stopPerformance]);

  const unreadCount = allSessions.filter((session) => session.unread).length;
  const activeCount = allSessions.filter((session) =>
    isTaskLive(session.task, details[session.task.id])).length;
  // The badge is visible only while something is unread, but its numeral mirrors the tracker it
  // opens. This avoids showing “2” beside a compact list that expands to four task reminders.
  const trackingCount = trackedSessions.length;
  const badgeCount = unreadCount > 0 ? trackingCount : 0;
  const ariaLabel = `R-Code session 助手，${trackingCount} 个任务正在追踪，其中 ${unreadCount} 个未读，${activeCount} 个任务正在运行`;

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
    if (navigatingTaskIdRef.current !== null) return;
    navigatingTaskIdRef.current = taskId;
    setNavigatingTaskId(taskId);
    setNavigationError(null);
    try {
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
    } finally {
      navigatingTaskIdRef.current = null;
      setNavigatingTaskId(null);
    }
  };

  const stopSession = async (taskId: string) => {
    // State updates are asynchronous; this ref closes the same-tick double-click window too.
    if (stoppingTaskIdsRef.current.has(taskId)) return;
    stoppingTaskIdsRef.current.add(taskId);
    setStoppingTaskIds((current) => new Set(current).add(taskId));
    setStopErrors((current) => {
      if (!(taskId in current)) return current;
      const next = { ...current };
      delete next[taskId];
      return next;
    });
    try {
      await agentAbort(taskId);
      await Promise.allSettled([refreshTasks(), refreshDetail(taskId)]);
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      setStopErrors((current) => ({ ...current, [taskId]: `停止失败：${message}` }));
      // A failed stop must remain actionable and unread; a later successful navigation is the
      // only interaction that acknowledges this session.
      setUnread((current) => {
        const next = new Set(current).add(taskId);
        persistUnread(next);
        return next;
      });
      await Promise.allSettled([refreshTasks(), refreshDetail(taskId)]);
    } finally {
      stoppingTaskIdsRef.current.delete(taskId);
      setStoppingTaskIds((current) => {
        const next = new Set(current);
        next.delete(taskId);
        return next;
      });
    }
  };

  const renderSessionCard = (session: SessionProgress, compact: boolean) => {
    const detail = details[session.task.id];
    const live = isTaskLive(session.task, detail);
    const stopping = stoppingTaskIds.has(session.task.id);
    const navigating = navigatingTaskId === session.task.id;
    const stopError = stopErrors[session.task.id];
    return (
      <div
        role="listitem"
        key={session.task.id}
        className={`companion-session-card${compact ? " is-pulse" : ""}`}
      >
        <button
          type="button"
          className={`companion-session-row state-${session.mood}${session.unread ? " is-unread" : ""}`}
          disabled={navigating}
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
        <div className="companion-session-actions" aria-label={`${session.task.title} 操作`}>
          <button
            type="button"
            disabled={navigating || stopping}
            onClick={() => void navigateToSession(session.task.id)}
          >
            {navigating ? "打开中…" : "继续跟进"}
          </button>
          {live && (
            <button
              type="button"
              className="is-stop"
              disabled={stopping || navigating}
              onClick={() => void stopSession(session.task.id)}
            >
              {stopping ? "停止中…" : "停止当前运行"}
            </button>
          )}
        </div>
        {stopError && <p className="companion-session-error" role="alert">{stopError}</p>}
      </div>
    );
  };

  if (!enabled) return null;

  return (
    <main
      className={`companion-window-root${panelOpen ? " is-expanded" : ""}${displayHasTracking ? " has-tracking" : ""}${pulseVisible ? " has-pulses" : ""}${hovered ? " is-hovered" : ""} panel-${panelSide} avatar-${avatarVertical} state-${mood} performance-${performance ?? "none"} motion-${motion}${displayLayout.minimized ? " is-mini" : ""}`}
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
            {renderedSessions.length === 0 ? (
              <p className="companion-empty">还没有会话，我会在这里反馈进度。</p>
            ) : renderedSessions.map((session) => (
              <div
                role="listitem"
                key={session.task.id}
                ref={(node) => {
                  if (node) sessionNodes.current.set(session.task.id, node);
                  else sessionNodes.current.delete(session.task.id);
                }}
                className={`companion-session-card${session.exiting ? " is-exiting" : ""}`}
              >
                {renderSessionCard(session, false).props.children}
              </div>
            ))}
          </div>
          {navigationError && <p className="companion-navigation-error" role="alert">{navigationError}</p>}
        </section>
      )}

      {pulseVisible && (
        <section
          id="companion-pulse-stack"
          className={`companion-pulse-stack${showingAllPulseSessions ? " is-showing-all" : ""}`}
          aria-label={`Session 进度提醒，共 ${trackingCount} 个任务`}
          aria-live="polite"
        >
          <div ref={pulseListRef} id="companion-pulse-list" role="list">
            {displayedPulseSessions.map((session) => renderSessionCard(session, true))}
          </div>
          {displayPulseOverflow && (
            <button
              type="button"
              className="companion-pulse-more"
              aria-controls="companion-pulse-list"
              aria-expanded={showingAllPulseSessions}
              onClick={(event) => {
                event.stopPropagation();
                setTrackingShowingAll((current) => !current);
              }}
            >
              {showingAllPulseSessions
                ? `全部 ${trackingCount} 个任务 · 收起`
                : `还有 ${hiddenPulseCount} 个任务 · 查看全部`}
            </button>
          )}
          {navigationError && <p className="companion-pulse-error" role="alert">{navigationError}</p>}
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
          if (mood !== "idle" || hoverIntentTimer.current !== null) return;
          // Codex uses an intentional hover reaction rather than random background motion. A
          // short intent delay prevents repeated edge crossings from restarting full-body poses.
          hoverIntentTimer.current = window.setTimeout(() => {
            hoverIntentTimer.current = null;
            const next = interactionTurn.current % 2 === 0 ? "sing" : "dance";
            interactionTurn.current += 1;
            beginPerformance(next);
          }, 320);
        }}
        onPointerLeave={() => {
          setHovered(false);
          if (hoverIntentTimer.current !== null) {
            window.clearTimeout(hoverIntentTimer.current);
            hoverIntentTimer.current = null;
          }
        }}
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
        <span
          key="companion-sequence"
          className="companion-frame-layer is-current"
          aria-hidden="true"
        >
          <CompanionSprite motion={motion} state={visualState} />
        </span>
        {badgeCount > 0 && (
          <span className="companion-unread-badge" aria-hidden="true">
            {badgeCount > 9 ? "9+" : badgeCount}
          </span>
        )}
      </button>

      {!panelOpen && displayHasTracking && (
        <button
          type="button"
          className={`companion-tracking-toggle${trackingCollapsed ? " is-collapsed" : ""}`}
          aria-label={trackingCollapsed
            ? `${activeCount} 个任务正在运行，展开 Session 追踪`
            : "收起 Session 追踪"}
          aria-controls="companion-pulse-stack"
          aria-expanded={!trackingCollapsed}
          title={trackingCollapsed ? "展开 Session 追踪" : "收起 Session 追踪"}
          onClick={(event) => {
            event.stopPropagation();
            if (!trackingCollapsed) setTrackingShowingAll(false);
            setTrackingCollapsed((current) => !current);
          }}
        >
          {trackingCollapsed ? (
            <span>{activeCount > 9 ? "9+" : activeCount}</span>
          ) : (
            <svg viewBox="0 0 20 20" aria-hidden="true">
              <path d="m5 7.5 5 5 5-5" />
            </svg>
          )}
        </button>
      )}

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
