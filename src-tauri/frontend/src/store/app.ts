import { create } from "zustand";

/**
 * 全局应用状态（Zustand）。
 * 场景（scene）对应 activity 列入口；Room 由 openRoom(taskId) 进入。
 * 主题写入 <html data-theme>，缩放写入 #app zoom。
 */

export type Scene =
  | "home"
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

export type RailTab = "sessions" | "files";

/** Room 画布页签（titlebar 按钮可远程切换）。 */
export type CanvasTab = "summary" | "changes" | "files" | "terminal" | "review";

interface AppState {
  scene: Scene;
  /** 当前 Room 打开的任务 */
  currentTaskId: string | null;
  /** Room 画布激活页签 */
  canvasTab: CanvasTab;
  /** Ctrl K 搜索 overlay */
  searchOpen: boolean;
  /** Editor 当前浏览的文件（Ctrl K 搜索写入，Editor 场景消费） */
  editorFile: string | null;
  /** Rail 面板页签 */
  railTab: RailTab;
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
  openDeck: () => void;
  openRoom: (taskId: string) => void;
  setCanvasTab: (tab: CanvasTab) => void;
  toggleSearch: () => void;
  setSearchOpen: (open: boolean) => void;
  setEditorFile: (path: string | null) => void;
  setRailTab: (tab: RailTab) => void;
  setDeckDensity: (d: "cards" | "rows") => void;
  setThemeMode: (mode: ThemeMode) => void;
  setZoom: (level: number) => void;
  zoomIn: () => void;
  zoomOut: () => void;
  zoomReset: () => void;
  toggleDiffMode: () => void;
}

export const useAppStore = create<AppState>((set) => ({
  scene: "home",
  currentTaskId: null,
  canvasTab: "summary",
  searchOpen: false,
  editorFile: null,
  railTab: "sessions",
  deckDensity: "cards",
  themeMode: "dark",
  zoomLevel: 100,
  accessibleDiffMode: false,

  setScene: (scene) => set({ scene }),
  goHome: () => set({ scene: "home" }),
  openDeck: () => set({ scene: "deck" }),
  openRoom: (taskId) => set({ scene: "room", currentTaskId: taskId }),
  setCanvasTab: (canvasTab) => set({ canvasTab }),
  toggleSearch: () => set((s) => ({ searchOpen: !s.searchOpen })),
  setSearchOpen: (searchOpen) => set({ searchOpen }),
  setEditorFile: (editorFile) => set({ editorFile }),
  setRailTab: (railTab) => set({ railTab }),
  setDeckDensity: (deckDensity) => set({ deckDensity }),
  setThemeMode: (themeMode) => set({ themeMode }),
  setZoom: (level) => set({ zoomLevel: Math.max(80, Math.min(200, level)) }),
  zoomIn: () => set((s) => ({ zoomLevel: Math.min(200, s.zoomLevel + 10) })),
  zoomOut: () => set((s) => ({ zoomLevel: Math.max(80, s.zoomLevel - 10) })),
  zoomReset: () => set({ zoomLevel: 100 }),
  toggleDiffMode: () => set((s) => ({ accessibleDiffMode: !s.accessibleDiffMode })),
}));
