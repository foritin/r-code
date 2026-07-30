import { create } from "zustand";

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
  | "editor"
  | "settings";

/** 外观模式：亮 / 暗 / 跟随系统。 */
export type ThemeMode = "light" | "dark" | "system";

/** 实际生效的 data-theme（由 mode + 系统偏好解析，App.tsx 统一写入）。 */
export type ResolvedTheme = "studio-light" | "obsidian";

/** Room 画布页签（titlebar 按钮可远程切换）。 */
export type CanvasTab = "summary" | "changes" | "files" | "terminal" | "review";
export type WorkbenchMode = "docked" | "hidden" | "focus" | "collapsed";

export interface TaskWorkbenchState {
  tab: CanvasTab;
  lastTab: CanvasTab;
  mode: WorkbenchMode;
  launcherOpen: boolean;
}
export type SettingsPane = "providers" | "preferences" | "diagnostics" | "codex";

interface AppState {
  scene: Scene;
  /** 当前 Room 打开的任务 */
  currentTaskId: string | null;
  /** Room 画布激活页签 */
  canvasTab: CanvasTab;
  /** 当前任务工作台的展示模式；具体状态同时按任务隔离保存。 */
  workbenchMode: WorkbenchMode;
  workbenchLauncherOpen: boolean;
  workbenches: Record<string, TaskWorkbenchState>;
  /** 设置页当前分类，允许命令和深链直接打开目标区域。 */
  settingsPane: SettingsPane;
  /** Ctrl K 搜索 overlay */
  searchOpen: boolean;
  /** Editor 当前浏览的文件（Ctrl K 搜索写入，Editor 场景消费） */
  editorFile: string | null;
  /** 侧栏是否折叠（Ctrl+B） */
  railCollapsed: boolean;
  /** Deck 密度模式 */
  deckDensity: "cards" | "rows";
  /** 外观模式（亮/暗/跟随系统） */
  themeMode: ThemeMode;
  /** 窗口缩放 80-200（A11Y-003） */
  zoomLevel: number;
  /** 无障碍 Diff 文本模式（A11Y-005） */
  accessibleDiffMode: boolean;

  setScene: (scene: Scene) => void;
  goHome: () => void;
  openDashboard: () => void;
  openConversations: () => void;
  openDeck: () => void;
  openRoom: (taskId: string, tab?: CanvasTab) => void;
  setCanvasTab: (tab: CanvasTab) => void;
  showWorkbenchLauncher: () => void;
  closeWorkbenchLauncher: () => void;
  hideWorkbench: (collapseReview?: boolean) => void;
  restoreWorkbench: () => void;
  toggleWorkbenchFocus: () => void;
  expandReview: () => void;
  setSettingsPane: (pane: SettingsPane) => void;
  toggleSearch: () => void;
  setSearchOpen: (open: boolean) => void;
  setEditorFile: (path: string | null) => void;
  toggleRail: () => void;
  setDeckDensity: (d: "cards" | "rows") => void;
  setThemeMode: (mode: ThemeMode) => void;
  setZoom: (level: number) => void;
  zoomIn: () => void;
  zoomOut: () => void;
  zoomReset: () => void;
  toggleDiffMode: () => void;
}

const RAIL_KEY = "r-code.rail.collapsed";
const THEME_KEY = "r-code.theme.mode";

function createWorkbenchState(tab: CanvasTab = "summary"): TaskWorkbenchState {
  return {
    tab,
    lastTab: tab,
    mode: "hidden",
    // 新任务先给出工具启动器，避免把高级工具一次性铺满。
    launcherOpen: tab === "summary",
  };
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

function readThemeMode(): ThemeMode {
  try {
    const saved = window.localStorage.getItem(THEME_KEY);
    if (saved === "light" || saved === "dark" || saved === "system") return saved;
  } catch {
    // 受限环境下使用产品默认值
  }
  return "dark";
}

export const useAppStore = create<AppState>((set) => ({
  scene: "home",
  currentTaskId: null,
  canvasTab: "summary",
  workbenchMode: "docked",
  workbenchLauncherOpen: true,
  workbenches: {},
  settingsPane: "providers",
  searchOpen: false,
  editorFile: null,
  railCollapsed: readCollapsed(),
  deckDensity: "cards",
  themeMode: readThemeMode(),
  zoomLevel: 100,
  accessibleDiffMode: false,

  setScene: (scene) => set({ scene }),
  goHome: () => set({ scene: "home" }),
  openDashboard: () => set({ scene: "dashboard" }),
  openConversations: () => set({ scene: "conversations" }),
  openDeck: () => set({ scene: "deck" }),
  openRoom: (taskId, requestedTab) =>
    set((state) => {
      const saved = state.workbenches[taskId] ?? createWorkbenchState(requestedTab ?? "summary");
      const next: TaskWorkbenchState = requestedTab
        ? { ...saved, tab: requestedTab, lastTab: requestedTab, mode: "docked", launcherOpen: false }
        : saved;
      return {
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
      mode: current.mode === "hidden" || current.mode === "collapsed" ? "docked" : current.mode,
      launcherOpen: false,
    }))),
  showWorkbenchLauncher: () =>
    set((state) => withCurrentWorkbench(state, (current) => ({
      ...current,
      mode: "docked",
      launcherOpen: true,
    }))),
  closeWorkbenchLauncher: () =>
    set((state) => withCurrentWorkbench(state, (current) => ({
      ...current,
      launcherOpen: false,
    }))),
  hideWorkbench: (shouldCollapseReview = false) =>
    set((state) => withCurrentWorkbench(state, (current) => ({
      ...current,
      mode: shouldCollapseReview ? "collapsed" : "hidden",
      launcherOpen: false,
    }))),
  restoreWorkbench: () =>
    set((state) => withCurrentWorkbench(state, (current) => ({
      ...current,
      tab: current.lastTab,
      mode: "docked",
      launcherOpen: current.launcherOpen,
    }))),
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
  setSettingsPane: (settingsPane) => set({ settingsPane, scene: "settings" }),
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
