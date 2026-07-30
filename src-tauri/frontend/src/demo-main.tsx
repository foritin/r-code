import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { ErrorBoundary } from "./components/ui/ErrorBoundary";
import { CodexCliGateProvider } from "./components/codex/CodexCliGate";
import {
  useAppStore,
  workbenchToolTab,
  type CanvasTab,
  type Scene,
  type SettingsPane,
  type ThemeMode,
  type WorkbenchMode,
} from "./store/app";
import { useTasksStore } from "./store/tasks";
import { browserMockDetails, browserMockTasks, browserMockWorkspaces } from "./lib/mock-data";
import "./styles/tokens.css";
import "./styles/base.css";
import "./styles/components.css";
import "./styles/markdown.css";
import "./styles/shell.css";
import "./styles/scenes.css";
import "./styles/r-code-ui.css";
import "./styles/product-ui.css";
import "./styles/workbench.css";
import "./styles/signature.css";

const SCENES = new Set<Scene>([
  "home",
  "dashboard",
  "conversations",
  "deck",
  "room",
  "inbox",
  "projects",
  "editor",
  "settings",
]);
const TABS = new Set<CanvasTab>(["summary", "changes", "files", "terminal", "review"]);
const SETTINGS_PANES = new Set<SettingsPane>(["providers", "agents", "preferences", "diagnostics", "codex"]);
const THEMES = new Set<ThemeMode>(["light", "dark", "system"]);

const TASK_ALIASES: Record<string, string> = {
  queue: "mock-task-queue",
  review: "mock-task-review",
  permission: "mock-task-permission",
  api: "mock-task-api",
  complete: "mock-task-complete",
};

const PROJECT_ALIASES: Record<string, string> = {
  "r-code": "D:/project/rust/r-code",
  api: "D:/project/rust/api-server",
  "api-server": "D:/project/rust/api-server",
};

function parseDemoRoute() {
  const params = new URLSearchParams(window.location.search);
  if (params.get("reset") === "1") {
    for (const key of ["r-code.rail.collapsed", "r-code.theme.mode", "r-code.room.split-pct"]) {
      window.localStorage.removeItem(key);
    }
  }

  const legacyState = params.get("state");
  const requestedScene = params.get("scene") ?? legacyState ?? "home";
  const aliasScene = requestedScene === "activity"
    ? "deck"
    : ["launcher", "run", "terminal", "files", "review", "review-collapsed", "hidden"].includes(requestedScene)
      ? "room"
      : requestedScene;
  const scene = SCENES.has(aliasScene as Scene) ? aliasScene as Scene : "home";

  const requestedTab = params.get("tab") ?? (
    legacyState === "terminal" || legacyState === "files" || legacyState === "review"
      ? legacyState
      : legacyState === "review-collapsed"
        ? "review"
        : "summary"
  );
  const canvasTab = TABS.has(requestedTab as CanvasTab) ? requestedTab as CanvasTab : "summary";
  const requestedTask = params.get("task") ?? (canvasTab === "review" ? "review" : "queue");
  const currentTaskId = TASK_ALIASES[requestedTask] ?? (
    browserMockDetails[requestedTask] ? requestedTask : "mock-task-queue"
  );
  const requestedSettingsPane = params.get("settings") ?? params.get("pane") ?? "providers";
  const settingsPane = SETTINGS_PANES.has(requestedSettingsPane as SettingsPane)
    ? requestedSettingsPane as SettingsPane
    : "providers";
  const requestedTheme = params.get("theme") ?? "dark";
  const themeMode = THEMES.has(requestedTheme as ThemeMode) ? requestedTheme as ThemeMode : "light";
  const requestedProject = params.get("project");
  const defaultProject = scene === "dashboard" || scene === "editor" ? "D:/project/rust/r-code" : null;
  const currentProjectId = requestedProject
    ? PROJECT_ALIASES[requestedProject] ?? requestedProject
    : defaultProject;
  const railCollapsed = params.get("rail") === "collapsed";
  const editorFile = params.get("file") || (scene === "editor" ? "src/main.rs" : null);
  const workbenchMode: WorkbenchMode = legacyState === "review-collapsed"
    ? "collapsed"
    : legacyState === "hidden"
      ? "hidden"
      : "docked";
  const workbenchLauncherOpen = scene === "room"
    && (legacyState === "launcher" || (!params.has("tab") && !legacyState));

  return {
    scene,
    canvasTab,
    currentTaskId,
    settingsPane,
    themeMode,
    currentProjectId,
    railCollapsed,
    editorFile,
    workbenchMode,
    workbenchLauncherOpen,
  };
}

const route = parseDemoRoute();

// 首帧直接注入同一份确定性数据；页面后续轮询仍走 browser-mock-runtime，
// 因而创建任务、审批、保存文件等操作都会继续更新，而不是停在静态截图。
useTasksStore.setState({
  tasks: browserMockTasks,
  details: browserMockDetails,
  workspaces: browserMockWorkspaces,
  currentProjectId: route.currentProjectId,
  refreshedAt: Date.now(),
});
useAppStore.setState({
  scene: route.scene,
  currentTaskId: route.scene === "room" ? route.currentTaskId : null,
  canvasTab: route.canvasTab,
  settingsPane: route.settingsPane,
  themeMode: route.themeMode,
  railCollapsed: route.railCollapsed,
  editorFile: route.editorFile,
  workbenchMode: route.workbenchMode,
  workbenchLauncherOpen: route.workbenchLauncherOpen,
  workbenches: route.scene === "room" ? {
    [route.currentTaskId]: {
      tab: route.canvasTab,
      lastTab: route.canvasTab,
      openTabs: route.workbenchLauncherOpen ? [] : [workbenchToolTab(route.canvasTab)],
      mode: route.workbenchMode,
      launcherOpen: route.workbenchLauncherOpen,
    },
  } : {},
});

document.documentElement.dataset.demo = "complete";
document.documentElement.dataset.demoScene = route.scene;
document.documentElement.dataset.demoTab = route.canvasTab;
document.documentElement.dataset.demoWorkbench = route.workbenchLauncherOpen ? "launcher" : route.workbenchMode;

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <ErrorBoundary>
      <CodexCliGateProvider>
        <App />
      </CodexCliGateProvider>
    </ErrorBoundary>
  </React.StrictMode>,
);

window.setTimeout(() => {
  document.documentElement.dataset.demoReady = "true";
  (window as Window & { __ready?: boolean }).__ready = true;
}, 300);
