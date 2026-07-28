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
export type SettingsPane = "providers" | "preferences" | "diagnostics" | "codex";

interface AppState {
  scene: Scene;
  /** 当前 Room 打开的任务 */
  currentTaskId: string | null;
  /** Room 画布激活页签 */
  canvasTab: CanvasTab;
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
  openRoom: (taskId: string) => void;
  setCanvasTab: (tab: CanvasTab) => void;
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
  return "light";
}

export const useAppStore = create<AppState>((set) => ({
  scene: "home",
  currentTaskId: null,
  canvasTab: "summary",
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
  openRoom: (taskId) => set({ scene: "room", currentTaskId: taskId }),
  setCanvasTab: (canvasTab) => set({ canvasTab }),
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
