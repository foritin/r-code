import { create } from "zustand";
import { useTasksStore } from "./tasks";

/**
 * 全局应用状态（Zustand）。
 * 场景（scene）对应 activity 列入口；Room 由 openRoom(taskId) 进入。
 * 主题写入 <html data-theme>，缩放写入 #app zoom。
 */

export type Scene =
  | "home"
  | "dashboard"
  | "conversations"
  | "deck"
  | "room"
  | "inbox"
  | "projects"
  | "knowledge"
  | "editor"
  | "settings";

export interface NavigationEntry {
  scene: Scene;
  currentTaskId: string | null;
  workspacePath: string | null;
}

/** 外观模式：亮 / 暗 / 跟随系统。 */
export type ThemeMode = "light" | "dark" | "system";

/** 实际生效的 data-theme（由 mode + 系统偏好解析，App.tsx 统一写入）。 */
export type ResolvedTheme = "studio-light" | "obsidian";

/** Room 画布页签（titlebar 按钮可远程切换）。 */
export type CanvasTab = "summary" | "changes" | "files" | "terminal" | "review";
/** 工作台顶部可独立关闭的工具页签；changes 是 review 内部的子视图。 */
export type WorkbenchToolTab = Exclude<CanvasTab, "changes">;
export type WorkbenchMode = "docked" | "hidden" | "focus" | "collapsed";

export interface TaskWorkbenchState {
  tab: CanvasTab;
  lastTab: CanvasTab;
  openTabs: WorkbenchToolTab[];
  mode: WorkbenchMode;
  launcherOpen: boolean;
}
export interface WorkbenchFileNavigation {
  path: string;
  line: number | null;
  column: number | null;
  requestId: number;
}
export interface TaskFileReferenceRequest {
  requestId: number;
  path: string;
}
export type SettingsPane = "providers" | "agents" | "preferences" | "diagnostics" | "codex";
export type KnowledgeTab = "memory" | "prompts" | "skills" | "mcp";

interface AppState {
  scene: Scene;
  navigationBack: NavigationEntry[];
  navigationForward: NavigationEntry[];
  /** 当前 Room 打开的任务 */
  currentTaskId: string | null;
  /** Room 画布激活页签 */
  canvasTab: CanvasTab;
  /** 当前任务工作台的展示模式；具体状态同时按任务隔离保存。 */
  workbenchMode: WorkbenchMode;
  workbenchLauncherOpen: boolean;
  workbenches: Record<string, TaskWorkbenchState>;
  /** Model/file-link navigation requests, isolated by task just like the workbench itself. */
  workbenchFiles: Record<string, WorkbenchFileNavigation>;
  /** Pending unsent Composer insertions, isolated by destination task. */
  taskFileReferences: Record<string, TaskFileReferenceRequest>;
  /** 设置页当前分类，允许命令和深链直接打开目标区域。 */
  settingsPane: SettingsPane;
  /** 知识控制面当前分类；MCP 建议可深链到对应配置。 */
  knowledgeTab: KnowledgeTab;
  /** 来自 Agent MCP 建议的可选市场检索词。 */
  mcpMarketQuery: string | null;
  /** Ctrl K 搜索 overlay */
  searchOpen: boolean;
  /** Editor 当前浏览的文件（Ctrl K 搜索写入，Editor 场景消费） */
  editorFile: string | null;
  /** 侧栏是否折叠（Ctrl+B） */
  railCollapsed: boolean;
  /** 展开时的侧栏宽度；折叠宽度仍由布局 token 控制。 */
  railWidth: number;
  /** Deck 密度模式 */
  deckDensity: "cards" | "rows";
  /** 外观模式（亮/暗/跟随系统） */
  themeMode: ThemeMode;
  /** 窗口缩放 80-200（A11Y-003） */
  zoomLevel: number;
  /** 无障碍 Diff 文本模式（A11Y-005） */
  accessibleDiffMode: boolean;

  setScene: (scene: Scene) => void;
  goBack: () => void;
  goForward: () => void;
  forgetTaskNavigation: (taskId: string) => void;
  goHome: () => void;
  openNewConversation: (workspacePath: string | null) => void;
  openDashboard: (workspacePath?: string) => void;
  openConversations: () => void;
  openDeck: () => void;
  openRoom: (taskId: string, tab?: CanvasTab) => void;
  setCanvasTab: (tab: CanvasTab) => void;
  openWorkbenchFile: (taskId: string, path: string, line?: number | null, column?: number | null) => void;
  queueTaskFileReference: (taskId: string, path: string) => void;
  acknowledgeTaskFileReference: (taskId: string, requestId: number) => void;
  closeWorkbenchTab: (tab?: CanvasTab) => void;
  showWorkbenchLauncher: () => void;
  closeWorkbenchLauncher: () => void;
  hideWorkbench: (collapseReview?: boolean) => void;
  restoreWorkbench: () => void;
  toggleWorkbenchFocus: () => void;
  expandReview: () => void;
  setSettingsPane: (pane: SettingsPane) => void;
  openKnowledge: (tab?: KnowledgeTab, marketQuery?: string | null) => void;
  toggleSearch: () => void;
  setSearchOpen: (open: boolean) => void;
  setEditorFile: (path: string | null) => void;
  toggleRail: () => void;
  setRailWidth: (width: number) => void;
  setDeckDensity: (d: "cards" | "rows") => void;
  setThemeMode: (mode: ThemeMode) => void;
  setZoom: (level: number) => void;
  zoomIn: () => void;
  zoomOut: () => void;
  zoomReset: () => void;
  toggleDiffMode: () => void;
}

const RAIL_KEY = "r-code.rail.collapsed";
const RAIL_WIDTH_KEY = "r-code.rail.width";
export const DEFAULT_RAIL_WIDTH = 300;
export const MIN_RAIL_WIDTH = 232;
export const MAX_RAIL_WIDTH = 520;
const THEME_KEY = "r-code.theme.mode";
let fileNavigationSequence = 0;
let fileReferenceSequence = 0;

function createWorkbenchState(tab: CanvasTab = "summary"): TaskWorkbenchState {
  return {
    tab,
    lastTab: tab,
    openTabs: [],
    mode: "hidden",
    // 新任务先给出工具启动器，避免把高级工具一次性铺满。
    launcherOpen: tab === "summary",
  };
}

/** changes / review 共用同一个顶层 Tab，只在审核工具内部切换子视图。 */
export function workbenchToolTab(tab: CanvasTab): WorkbenchToolTab {
  return tab === "changes" ? "review" : tab;
}

function appendWorkbenchTab(tabs: WorkbenchToolTab[], tab: CanvasTab): WorkbenchToolTab[] {
  const tool = workbenchToolTab(tab);
  return tabs.includes(tool) ? tabs : [...tabs, tool];
}

function withCurrentWorkbench(
  state: AppState,
  update: (workbench: TaskWorkbenchState) => TaskWorkbenchState,
): Partial<AppState> {
  const taskId = state.currentTaskId;
  const current = taskId
    ? state.workbenches[taskId] ?? createWorkbenchState(state.canvasTab)
    : createWorkbenchState(state.canvasTab);
  const next = update(current);

  return {
    canvasTab: next.tab,
    workbenchMode: next.mode,
    workbenchLauncherOpen: next.launcherOpen,
    ...(taskId ? { workbenches: { ...state.workbenches, [taskId]: next } } : {}),
  };
}

function readCollapsed(): boolean {
  try {
    return window.localStorage.getItem(RAIL_KEY) === "1";
  } catch {
    return false;
  }
}

export function clampRailWidth(width: number): number {
  if (!Number.isFinite(width)) return DEFAULT_RAIL_WIDTH;
  return Math.round(Math.min(MAX_RAIL_WIDTH, Math.max(MIN_RAIL_WIDTH, width)));
}

function readRailWidth(): number {
  try {
    const saved = Number(window.localStorage.getItem(RAIL_WIDTH_KEY));
    return Number.isFinite(saved) && saved > 0 ? clampRailWidth(saved) : DEFAULT_RAIL_WIDTH;
  } catch {
    return DEFAULT_RAIL_WIDTH;
  }
}

function readThemeMode(): ThemeMode {
  try {
    const saved = window.localStorage.getItem(THEME_KEY);
    if (saved === "light" || saved === "dark" || saved === "system") return saved;
  } catch {
    // 受限环境下使用产品默认值
  }
  return "dark";
}

const NAVIGATION_LIMIT = 80;

function currentLocation(state: AppState): NavigationEntry {
  return {
    scene: state.scene,
    currentTaskId: state.scene === "room" ? state.currentTaskId : null,
    workspacePath: useTasksStore.getState().currentProjectId,
  };
}

function sameLocation(left: NavigationEntry, right: NavigationEntry): boolean {
  return left.scene === right.scene
    && left.currentTaskId === right.currentTaskId
    && left.workspacePath === right.workspacePath;
}

function navigateTo(state: AppState, destination: NavigationEntry): Partial<AppState> {
  const current = currentLocation(state);
  if (sameLocation(current, destination)) {
    return {
      scene: destination.scene,
      ...(destination.scene === "room" ? { currentTaskId: destination.currentTaskId } : {}),
    };
  }
  return {
    scene: destination.scene,
    ...(destination.scene === "room" ? { currentTaskId: destination.currentTaskId } : {}),
    navigationBack: [...state.navigationBack, current].slice(-NAVIGATION_LIMIT),
    navigationForward: [],
  };
}

function restoreLocation(
  state: AppState,
  destination: NavigationEntry,
  navigationBack: NavigationEntry[],
  navigationForward: NavigationEntry[],
): Partial<AppState> {
  const patch: Partial<AppState> = {
    scene: destination.scene,
    navigationBack,
    navigationForward,
  };
  if (destination.scene !== "room" || !destination.currentTaskId) return patch;

  const saved = state.workbenches[destination.currentTaskId] ?? createWorkbenchState();
  return {
    ...patch,
    currentTaskId: destination.currentTaskId,
    canvasTab: saved.tab,
    workbenchMode: saved.mode,
    workbenchLauncherOpen: saved.launcherOpen,
    workbenches: { ...state.workbenches, [destination.currentTaskId]: saved },
  };
}

export const useAppStore = create<AppState>((set, get) => ({
  scene: "home",
  navigationBack: [],
  navigationForward: [],
  currentTaskId: null,
  canvasTab: "summary",
  workbenchMode: "docked",
  workbenchLauncherOpen: true,
  workbenches: {},
  workbenchFiles: {},
  taskFileReferences: {},
  settingsPane: "providers",
  knowledgeTab: "memory",
  mcpMarketQuery: null,
  searchOpen: false,
  editorFile: null,
  railCollapsed: readCollapsed(),
  railWidth: readRailWidth(),
  deckDensity: "cards",
  themeMode: readThemeMode(),
  zoomLevel: 100,
  accessibleDiffMode: false,

  setScene: (scene) => set((state) => navigateTo(state, {
    scene,
    currentTaskId: scene === "room" ? state.currentTaskId : null,
    workspacePath: useTasksStore.getState().currentProjectId,
  })),
  goBack: () => {
    const state = get();
    const destination = state.navigationBack[state.navigationBack.length - 1];
    if (!destination) return;
    const current = currentLocation(state);
    useTasksStore.getState().setCurrentProject(destination.workspacePath);
    set(restoreLocation(
      state,
      destination,
      state.navigationBack.slice(0, -1),
      [...state.navigationForward, current].slice(-NAVIGATION_LIMIT),
    ));
  },
  goForward: () => {
    const state = get();
    const destination = state.navigationForward[state.navigationForward.length - 1];
    if (!destination) return;
    const current = currentLocation(state);
    useTasksStore.getState().setCurrentProject(destination.workspacePath);
    set(restoreLocation(
      state,
      destination,
      [...state.navigationBack, current].slice(-NAVIGATION_LIMIT),
      state.navigationForward.slice(0, -1),
    ));
  },
  forgetTaskNavigation: (taskId) => set((state) => ({
    navigationBack: state.navigationBack.filter((entry) => entry.currentTaskId !== taskId),
    navigationForward: state.navigationForward.filter((entry) => entry.currentTaskId !== taskId),
  })),
  goHome: () => set((state) => navigateTo(state, {
    scene: "home",
    currentTaskId: null,
    workspacePath: useTasksStore.getState().currentProjectId,
  })),
  openNewConversation: (workspacePath) => {
    set((state) => navigateTo(state, {
      scene: "home",
      currentTaskId: null,
      workspacePath,
    }));
    useTasksStore.getState().setCurrentProject(workspacePath);
  },
  openDashboard: (workspacePath) => {
    const destinationPath = workspacePath ?? useTasksStore.getState().currentProjectId;
    set((state) => navigateTo(state, {
      scene: "dashboard",
      currentTaskId: null,
      workspacePath: destinationPath,
    }));
    if (workspacePath !== undefined) useTasksStore.getState().setCurrentProject(workspacePath);
  },
  openConversations: () => set((state) => navigateTo(state, {
    scene: "conversations",
    currentTaskId: null,
    workspacePath: useTasksStore.getState().currentProjectId,
  })),
  openDeck: () => set((state) => navigateTo(state, {
    scene: "deck",
    currentTaskId: null,
    workspacePath: useTasksStore.getState().currentProjectId,
  })),
  openRoom: (taskId, requestedTab) =>
    set((state) => {
      const saved = state.workbenches[taskId] ?? createWorkbenchState(requestedTab ?? "summary");
      const next: TaskWorkbenchState = requestedTab
        ? {
            ...saved,
            tab: requestedTab,
            lastTab: requestedTab,
            openTabs: appendWorkbenchTab(saved.openTabs, requestedTab),
            mode: "docked",
            launcherOpen: false,
          }
        : saved;
      return {
        ...navigateTo(state, {
          scene: "room",
          currentTaskId: taskId,
          workspacePath: useTasksStore.getState().currentProjectId,
        }),
        scene: "room",
        currentTaskId: taskId,
        canvasTab: next.tab,
        workbenchMode: next.mode,
        workbenchLauncherOpen: next.launcherOpen,
        workbenches: { ...state.workbenches, [taskId]: next },
      };
    }),
  setCanvasTab: (canvasTab) =>
    set((state) => withCurrentWorkbench(state, (current) => ({
      ...current,
      tab: canvasTab,
      lastTab: canvasTab,
      openTabs: appendWorkbenchTab(current.openTabs, canvasTab),
      mode: current.mode === "hidden" || current.mode === "collapsed" ? "docked" : current.mode,
      launcherOpen: false,
    }))),
  openWorkbenchFile: (taskId, path, line = null, column = null) =>
    set((state) => {
      const saved = state.workbenches[taskId] ?? createWorkbenchState("files");
      const next: TaskWorkbenchState = {
        ...saved,
        tab: "files",
        lastTab: "files",
        openTabs: appendWorkbenchTab(saved.openTabs, "files"),
        mode: "docked",
        launcherOpen: false,
      };
      return {
        ...navigateTo(state, {
          scene: "room",
          currentTaskId: taskId,
          workspacePath: useTasksStore.getState().currentProjectId,
        }),
        scene: "room",
        currentTaskId: taskId,
        canvasTab: "files",
        workbenchMode: "docked",
        workbenchLauncherOpen: false,
        workbenches: { ...state.workbenches, [taskId]: next },
        workbenchFiles: {
          ...state.workbenchFiles,
          [taskId]: {
            path,
            line,
            column,
            requestId: ++fileNavigationSequence,
          },
        },
      };
    }),
  queueTaskFileReference: (taskId, path) => set((state) => ({
    taskFileReferences: {
      ...state.taskFileReferences,
      [taskId]: { requestId: ++fileReferenceSequence, path },
    },
  })),
  acknowledgeTaskFileReference: (taskId, requestId) => set((state) => {
    if (state.taskFileReferences[taskId]?.requestId !== requestId) return state;
    const taskFileReferences = { ...state.taskFileReferences };
    delete taskFileReferences[taskId];
    return { taskFileReferences };
  }),
  closeWorkbenchTab: (requestedTab) =>
    set((state) => withCurrentWorkbench(state, (current) => {
      const closing = workbenchToolTab(requestedTab ?? current.tab);
      const closingIndex = current.openTabs.indexOf(closing);
      if (closingIndex < 0) return current;

      const openTabs = current.openTabs.filter((tab) => tab !== closing);
      if (openTabs.length === 0) {
        return {
          ...current,
          openTabs,
          mode: "hidden",
          // 下一次从右栏按钮进入时展示默认功能选择，而不是复活已关闭的工具。
          launcherOpen: true,
        };
      }

      if (workbenchToolTab(current.tab) !== closing) {
        return { ...current, openTabs };
      }

      // 关闭当前 Tab 后优先回到左侧相邻项；关闭首项时使用新的首项。
      const next = openTabs[Math.min(Math.max(closingIndex - 1, 0), openTabs.length - 1)];
      return {
        ...current,
        openTabs,
        tab: next,
        lastTab: next,
        launcherOpen: false,
      };
    })),
  showWorkbenchLauncher: () =>
    set((state) => withCurrentWorkbench(state, (current) => ({
      ...current,
      mode: "docked",
      launcherOpen: true,
    }))),
  closeWorkbenchLauncher: () =>
    set((state) => withCurrentWorkbench(state, (current) => ({
      ...current,
      mode: current.openTabs.length === 0 ? "hidden" : current.mode,
      // 空工作台隐藏后仍记住默认入口；已有 Tab 时只关闭启动器。
      launcherOpen: current.openTabs.length === 0,
    }))),
  hideWorkbench: (shouldCollapseReview = false) =>
    set((state) => withCurrentWorkbench(state, (current) => ({
      ...current,
      mode: shouldCollapseReview ? "collapsed" : "hidden",
    }))),
  restoreWorkbench: () =>
    set((state) => withCurrentWorkbench(state, (current) => {
      const activeTool = workbenchToolTab(current.tab);
      const fallback = current.openTabs[current.openTabs.length - 1];
      const tab = current.openTabs.includes(activeTool) ? current.tab : fallback ?? current.lastTab;
      return {
        ...current,
        tab,
        mode: "docked",
        launcherOpen: current.openTabs.length === 0 || current.launcherOpen,
      };
    })),
  toggleWorkbenchFocus: () =>
    set((state) => withCurrentWorkbench(state, (current) => ({
      ...current,
      mode: current.mode === "focus" ? "docked" : "focus",
    }))),
  expandReview: () =>
    set((state) => withCurrentWorkbench(state, (current) => ({
      ...current,
      mode: "docked",
      launcherOpen: false,
    }))),
  setSettingsPane: (settingsPane) => set((state) => ({
    ...navigateTo(state, {
      scene: "settings",
      currentTaskId: null,
      workspacePath: useTasksStore.getState().currentProjectId,
    }),
    settingsPane,
  })),
  openKnowledge: (knowledgeTab = "memory", marketQuery = null) => set((state) => ({
    ...navigateTo(state, {
      scene: "knowledge",
      currentTaskId: null,
      workspacePath: useTasksStore.getState().currentProjectId,
    }),
    knowledgeTab,
    mcpMarketQuery: marketQuery,
  })),
  toggleSearch: () => set((s) => ({ searchOpen: !s.searchOpen })),
  setSearchOpen: (searchOpen) => set({ searchOpen }),
  setEditorFile: (editorFile) => set({ editorFile }),
  toggleRail: () =>
    set((s) => {
      const railCollapsed = !s.railCollapsed;
      try {
        window.localStorage.setItem(RAIL_KEY, railCollapsed ? "1" : "0");
      } catch {
        // 受限环境下不持久化，不影响本次使用
      }
      return { railCollapsed };
    }),
  setRailWidth: (width) => {
    const railWidth = clampRailWidth(width);
    try {
      window.localStorage.setItem(RAIL_WIDTH_KEY, String(railWidth));
    } catch {
      // 受限环境下不持久化，不影响本次使用
    }
    set({ railWidth });
  },
  setDeckDensity: (deckDensity) => set({ deckDensity }),
  setThemeMode: (themeMode) => {
    try {
      window.localStorage.setItem(THEME_KEY, themeMode);
    } catch {
      // 受限环境下不持久化，不影响本次使用
    }
    set({ themeMode });
  },
  setZoom: (level) => set({ zoomLevel: Math.max(80, Math.min(200, level)) }),
  zoomIn: () => set((s) => ({ zoomLevel: Math.min(200, s.zoomLevel + 10) })),
  zoomOut: () => set((s) => ({ zoomLevel: Math.max(80, s.zoomLevel - 10) })),
  zoomReset: () => set({ zoomLevel: 100 }),
  toggleDiffMode: () => set((s) => ({ accessibleDiffMode: !s.accessibleDiffMode })),
}));
