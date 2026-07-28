import { useEffect } from "react";
import { useAppStore } from "./store/app";
import { useTasksStore } from "./store/tasks";
import { useGlobalKeys } from "./lib/keys";
import { MenuBar } from "./components/shell/MenuBar";
import { Rail } from "./components/shell/Rail";
import { HomeScene } from "./components/scenes/HomeScene";
import { DashboardScene } from "./components/scenes/DashboardScene";
import { ConversationsScene } from "./components/scenes/ConversationsScene";
import { ActivityScene } from "./components/scenes/ActivityScene";
import { RoomScene } from "./components/scenes/RoomScene";
import { InboxScene } from "./components/scenes/InboxScene";
import { ProjectsScene } from "./components/scenes/ProjectsScene";
import { EditorScene } from "./components/scenes/EditorScene";
import { SettingsScene } from "./components/scenes/SettingsScene";
import { SearchOverlay } from "./components/SearchOverlay";
import { ToastHost, useTaskCompletionToasts } from "./components/ui/Toast";
import { selectRunning } from "./store/tasks";

/**
 * R-Code 应用根组件。
 * 紧凑标题栏 / 单一会话侧栏 / 主工作区（场景切换）。
 * 主题（亮/暗/跟随系统）解析后写入 <html data-theme>。
 * 界面缩放同步补偿根节点尺寸，避免放大后底栏和侧栏页脚被视口裁掉。
 */
export default function App() {
  const scene = useAppStore((s) => s.scene);
  const themeMode = useAppStore((s) => s.themeMode);
  const zoomLevel = useAppStore((s) => s.zoomLevel);
  const searchOpen = useAppStore((s) => s.searchOpen);
  const railCollapsed = useAppStore((s) => s.railCollapsed);
  const toggleRail = useAppStore((s) => s.toggleRail);
  const setScene = useAppStore((s) => s.setScene);
  const goHome = useAppStore((s) => s.goHome);
  const toggleSearch = useAppStore((s) => s.toggleSearch);
  const zoomIn = useAppStore((s) => s.zoomIn);
  const zoomOut = useAppStore((s) => s.zoomOut);
  const zoomReset = useAppStore((s) => s.zoomReset);

  const refreshTasks = useTasksStore((s) => s.refreshTasks);
  const refreshWorkspaces = useTasksStore((s) => s.refreshWorkspaces);

  useGlobalKeys({
    search: toggleSearch,
    editor: () => setScene("editor"),
    new: goHome,
    settings: () => setScene("settings"),
    toggleRail,
    shortcuts: () => window.dispatchEvent(new Event("r-code:shortcuts")),
    zoomIn,
    zoomOut,
    zoomReset,
  });

  // 主题解析：light → studio-light；dark → obsidian；system → 跟随 OS（含变化监听）
  useEffect(() => {
    const mq = window.matchMedia("(prefers-color-scheme: dark)");
    const apply = () => {
      const resolved =
        themeMode === "system" ? (mq.matches ? "obsidian" : "studio-light") : themeMode === "dark" ? "obsidian" : "studio-light";
      document.documentElement.dataset.theme = resolved;
    };
    apply();
    mq.addEventListener("change", apply);
    return () => mq.removeEventListener("change", apply);
  }, [themeMode]);

  useEffect(() => {
    void refreshWorkspaces().catch(() => {});
    void refreshTasks().catch(() => {});
  }, [refreshWorkspaces, refreshTasks]);

  // 后台任务跑完 / 权限卡住时播报（不在场就完全无感的那部分）
  useTaskCompletionToasts();

  const appScale = zoomLevel / 100;

  return (
    <div
      id="app"
      className={`app-shell scene-${scene}${railCollapsed ? " rail-is-collapsed" : ""}`}
      style={{
        zoom: appScale,
        width: `${100 / appScale}%`,
        height: `${100 / appScale}%`,
      }}
    >
      <a className="skip-link" href="#main-content">跳到主内容</a>
      <MenuBar />
      <Rail />
      <main className="main" id="main-content" role="main" tabIndex={-1}>
        {scene === "home" && <HomeScene />}
        {scene === "dashboard" && <DashboardScene />}
        {scene === "conversations" && <ConversationsScene />}
        {scene === "deck" && <ActivityScene />}
        {scene === "room" && <RoomScene />}
        {scene === "inbox" && <InboxScene />}
        {scene === "projects" && <ProjectsScene />}
        {scene === "editor" && <EditorScene />}
        {scene === "settings" && <SettingsScene />}
      </main>
      <AppStatusBar />
      {searchOpen && <SearchOverlay />}
      {/* 固定定位 + --z-toast，放在最后一个子节点：不被 .main/.scene 的 overflow 裁掉 */}
      <ToastHost />
    </div>
  );
}

function AppStatusBar() {
  const workspaceCount = useTasksStore((s) => s.workspaces.length);
  const runningCount = useTasksStore((s) => selectRunning(s).length);
  const refreshedAt = useTasksStore((s) => s.refreshedAt);
  return (
    <footer className="app-statusbar" aria-label="应用状态">
      <span><i className="status-live-dot" />{workspaceCount} 个项目</span>
      <span>{runningCount} 个任务运行中</span>
      <span className="app-statusbar-sync">{refreshedAt ? "数据已同步" : "正在连接数据"}</span>
    </footer>
  );
}
