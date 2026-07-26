import { useEffect } from "react";
import { useAppStore } from "./store/app";
import { useTasksStore } from "./store/tasks";
import { useGlobalKeys } from "./lib/keys";
import { MenuBar } from "./components/shell/MenuBar";
import { Rail } from "./components/shell/Rail";
import { HomeScene } from "./components/scenes/HomeScene";
import { DeckScene } from "./components/scenes/DeckScene";
import { RoomScene } from "./components/scenes/RoomScene";
import { InboxScene } from "./components/scenes/InboxScene";
import { ProjectsScene } from "./components/scenes/ProjectsScene";
import { EditorScene } from "./components/scenes/EditorScene";
import { SettingsScene } from "./components/scenes/SettingsScene";
import { SearchOverlay } from "./components/SearchOverlay";

/**
 * R-Code 应用根组件。
 * 紧凑标题栏 / 单一会话侧栏 / 主工作区（场景切换）。
 * 主题（亮/暗/跟随系统）解析后写入 <html data-theme>，缩放写入 #app zoom。
 */
export default function App() {
  const scene = useAppStore((s) => s.scene);
  const themeMode = useAppStore((s) => s.themeMode);
  const zoomLevel = useAppStore((s) => s.zoomLevel);
  const searchOpen = useAppStore((s) => s.searchOpen);
  const setScene = useAppStore((s) => s.setScene);
  const goHome = useAppStore((s) => s.goHome);
  const toggleSearch = useAppStore((s) => s.toggleSearch);
  const zoomIn = useAppStore((s) => s.zoomIn);
  const zoomOut = useAppStore((s) => s.zoomOut);
  const zoomReset = useAppStore((s) => s.zoomReset);

  const refreshTasks = useTasksStore((s) => s.refreshTasks);
  const refreshWorkspaces = useTasksStore((s) => s.refreshWorkspaces);

  useGlobalKeys({
    onSearch: toggleSearch,
    onEditor: () => setScene("editor"),
    onNew: goHome,
    onSettings: () => setScene("settings"),
    onZoomIn: zoomIn,
    onZoomOut: zoomOut,
    onZoomReset: zoomReset,
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

  return (
    <div id="app" style={{ zoom: zoomLevel / 100 }}>
      <MenuBar />
      <Rail />
      <main className="main" role="main">
        {scene === "home" && <HomeScene />}
        {scene === "deck" && <DeckScene />}
        {scene === "room" && <RoomScene />}
        {scene === "inbox" && <InboxScene />}
        {scene === "projects" && <ProjectsScene />}
        {scene === "editor" && <EditorScene />}
        {scene === "settings" && <SettingsScene />}
      </main>
      {searchOpen && <SearchOverlay />}
    </div>
  );
}
