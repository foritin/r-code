import { lazy, Suspense, useEffect, type CSSProperties } from "react";
import { MAX_RAIL_WIDTH, MIN_RAIL_WIDTH, useAppStore } from "./store/app";
import { useTasksStore } from "./store/tasks";
import { isMacPlatform, useGlobalKeys } from "./lib/keys";
import { MenuBar } from "./components/shell/MenuBar";
import { Rail } from "./components/shell/Rail";
import {
  MIN_MAIN_WIDTH,
  RailResizeHandle,
} from "./components/shell/RailResizeHandle";
import { SyncHealthBanner } from "./components/shell/SyncHealthBanner";
import { HomeScene } from "./components/scenes/HomeScene";
import { ToastHost, useTaskCompletionToasts } from "./components/ui/Toast";
import { CompanionWindowController } from "./components/companion/CompanionWindowController";
import { OnboardingCampaign } from "./components/onboarding/OnboardingCampaign";
import { GuideSheet } from "./components/settings/GuideSheet";
import { clearSyncFailure, reportSyncFailure } from "./store/sync-health";

const DashboardScene = lazy(() =>
  import("./components/scenes/DashboardScene").then((module) => ({ default: module.DashboardScene })),
);
const ConversationsScene = lazy(() =>
  import("./components/scenes/ConversationsScene").then((module) => ({ default: module.ConversationsScene })),
);
const ActivityScene = lazy(() =>
  import("./components/scenes/ActivityScene").then((module) => ({ default: module.ActivityScene })),
);
const ArchiveScene = lazy(() =>
  import("./components/scenes/ArchiveScene").then((module) => ({ default: module.ArchiveScene })),
);
const RoomScene = lazy(() =>
  import("./components/scenes/RoomScene").then((module) => ({ default: module.RoomScene })),
);
const InboxScene = lazy(() =>
  import("./components/scenes/InboxScene").then((module) => ({ default: module.InboxScene })),
);
const ProjectsScene = lazy(() =>
  import("./components/scenes/ProjectsScene").then((module) => ({ default: module.ProjectsScene })),
);
const EditorScene = lazy(() =>
  import("./components/scenes/EditorScene").then((module) => ({ default: module.EditorScene })),
);
const SettingsScene = lazy(() =>
  import("./components/scenes/SettingsScene").then((module) => ({ default: module.SettingsScene })),
);
const SearchOverlay = lazy(() =>
  import("./components/SearchOverlay").then((module) => ({ default: module.SearchOverlay })),
);

/**
 * R-Code 应用根组件。
 * 紧凑标题栏 / 单一会话侧栏 / 主工作区（场景切换）。
 * 主题（亮/暗/跟随系统）解析后写入 <html data-theme>。
 */
export default function App() {
  const scene = useAppStore((s) => s.scene);
  const themeMode = useAppStore((s) => s.themeMode);
  const searchOpen = useAppStore((s) => s.searchOpen);
  const railCollapsed = useAppStore((s) => s.railCollapsed);
  const railWidth = useAppStore((s) => s.railWidth);
  const toggleRail = useAppStore((s) => s.toggleRail);
  const setScene = useAppStore((s) => s.setScene);
  const goHome = useAppStore((s) => s.goHome);
  const toggleSearch = useAppStore((s) => s.toggleSearch);
  const refreshTasks = useTasksStore((s) => s.refreshTasks);
  const refreshWorkspaces = useTasksStore((s) => s.refreshWorkspaces);

  useGlobalKeys({
    search: toggleSearch,
    editor: () => setScene("editor"),
    new: goHome,
    settings: () => setScene("settings"),
    toggleRail,
    shortcuts: () => window.dispatchEvent(new Event("r-code:shortcuts")),
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
    const refreshStartupData = () => {
      void refreshWorkspaces()
        .then(() => clearSyncFailure("startup-workspaces"))
        .catch((cause) => reportSyncFailure("startup-workspaces", "项目列表", cause));
      void refreshTasks()
        .then(() => clearSyncFailure("startup-tasks"))
        .catch((cause) => reportSyncFailure("startup-tasks", "会话列表", cause));
    };
    refreshStartupData();
    window.addEventListener("r-code:refresh-now", refreshStartupData);
    return () => {
      window.removeEventListener("r-code:refresh-now", refreshStartupData);
      clearSyncFailure("startup-workspaces");
      clearSyncFailure("startup-tasks");
    };
  }, [refreshWorkspaces, refreshTasks]);

  // 后台任务跑完 / 权限卡住时播报（不在场就完全无感的那部分）
  useTaskCompletionToasts();

  return (
    <div
      id="app"
      className={`app-shell r-code-signature scene-${scene}${railCollapsed ? " rail-is-collapsed" : ""}${isMacPlatform() ? " platform-macos" : ""}`}
      style={{
        "--rc-rail-preferred-w": `${railWidth}px`,
        // The browser resolves this against the grid's current inline size, so resize
        // restoration is immediate and independent of React's resize-event scheduling.
        "--rc-rail-w": `max(${MIN_RAIL_WIDTH}px, min(${MAX_RAIL_WIDTH}px, var(--rc-rail-preferred-w), calc(100% - ${MIN_MAIN_WIDTH}px)))`,
      } as CSSProperties}
    >
      <a className="skip-link" href="#main-content">跳到主内容</a>
      <MenuBar />
      <Rail />
      <RailResizeHandle />
      <main className="main" id="main-content" role="main" tabIndex={-1}>
        <Suspense fallback={<div className="scene empty" role="status">正在打开…</div>}>
          {scene === "home" && <HomeScene />}
          {scene === "dashboard" && <DashboardScene />}
          {scene === "conversations" && <ConversationsScene />}
          {scene === "deck" && <ActivityScene />}
          {scene === "archive" && <ArchiveScene />}
          {scene === "room" && <RoomScene />}
          {scene === "inbox" && <InboxScene />}
          {scene === "projects" && <ProjectsScene />}
          {scene === "editor" && <EditorScene />}
          {scene === "settings" && <SettingsScene />}
        </Suspense>
      </main>
      {searchOpen && (
        <Suspense fallback={null}>
          <SearchOverlay />
        </Suspense>
      )}
      {/* 固定定位 + --z-toast，放在最后一个子节点：不被 .main/.scene 的 overflow 裁掉 */}
      <ToastHost />
      <CompanionWindowController />
      <SyncHealthBanner />
      <OnboardingCampaign />
      {/* 全局指引手册（Help 菜单跨场景入口；docs §6.3）。没有 offer 上下文，
          关闭时把焦点还给原入口，不创建或改变任何 Plan 入口建议。 */}
      <GlobalGuideSheetHost />
    </div>
  );
}

function GlobalGuideSheetHost() {
  const guideSheetId = useAppStore((s) => s.guideSheetId);
  const closeGuideSheet = useAppStore((s) => s.closeGuideSheet);
  const setSettingsPane = useAppStore((s) => s.setSettingsPane);
  return (
    <GuideSheet
      guideId={guideSheetId}
      onClose={closeGuideSheet}
      onAction={(action) => {
        if (action === "open-request-audit") setSettingsPane("diagnostics");
      }}
    />
  );
}
