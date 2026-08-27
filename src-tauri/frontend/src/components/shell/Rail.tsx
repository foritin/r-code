import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { useAppStore } from "../../store/app";
import { selectNeedsYouTaskIds, selectRunning, useTasksStore } from "../../store/tasks";
import { usePoll } from "../../lib/poll";
import { elapsedMinutes } from "../../lib/format";
import { keyLabel } from "../../lib/keys";
import { isTaskLive, taskStateLabel, taskTitle, visualTaskState } from "../../lib/presentation";
import type { Task, Workspace } from "../../lib/types";
import { ProjectActionsMenu } from "../ProjectActionsMenu";
import { TaskActionsMenu } from "../TaskActionsMenu";
import { useCreateConversation } from "../useCreateConversation";
import {
  IconActivity,
  IconArchive,
  IconChevronDown,
  IconChevronRight,
  IconFolderOpen,
  IconHistory,
  IconInbox,
  IconPlus,
  IconSearch,
  IconSettings,
  IconSidebar,
} from "../icons";

interface ProjectNode {
  workspace: Workspace;
  tasks: Task[];
}

// 原型 C 侧边栏的设计语言是「安静列表」：普通已结束的任务不加任何状态标记，
// 只有运行中（旋转环）和需要用户动作的状态（等待确认/待审阅/已中止）才点亮。
// 完整状态文字始终保留在条目的 title 提示里。
const MARKED_STATES = new Set(["running", "attention", "review", "stopped"]);

/** 左侧只负责全局导航与项目树，不承载项目动态；项目动态仅在 DashboardScene 内。 */
export function Rail() {
  const { t } = useTranslation();
  const scene = useAppStore((s) => s.scene);
  const setScene = useAppStore((s) => s.setScene);
  const openNewConversation = useAppStore((s) => s.openNewConversation);
  const openDashboard = useAppStore((s) => s.openDashboard);
  const openRoom = useAppStore((s) => s.openRoom);
  const currentTaskId = useAppStore((s) => s.currentTaskId);
  const collapsed = useAppStore((s) => s.railCollapsed);
  const toggleRail = useAppStore((s) => s.toggleRail);
  const toggleSearch = useAppStore((s) => s.toggleSearch);
  const tasks = useTasksStore((s) => s.tasks);
  const details = useTasksStore((s) => s.details);
  const workspaces = useTasksStore((s) => s.workspaces);
  const currentWorkspacePath = useTasksStore((s) => s.currentProjectId);
  const setCurrentWorkspace = useTasksStore((s) => s.setCurrentProject);
  const refreshTasks = useTasksStore((s) => s.refreshTasks);
  const refreshDetails = useTasksStore((s) => s.refreshDetails);
  const needsTaskIds = useTasksStore(selectNeedsYouTaskIds);
  const needsCount = needsTaskIds.size;
  const runningCount = useTasksStore((s) => selectRunning(s).length);
  const { createConversation, creating: creatingConversation } = useCreateConversation();
  const [collapsedProjects, setCollapsedProjects] = useState<Set<string>>(() => new Set());

  usePoll(async () => {
    await refreshTasks();
    const snapshot = useTasksStore.getState();
    const activeIds = snapshot.tasks
      .filter((task) => task.state !== "archived")
      .filter((task) => {
        const detail = snapshot.details[task.id];
        return !detail?.status
          || detail.task.updated_at !== task.updated_at
          || isTaskLive(task, detail);
      })
      // RoomScene already owns a faster detail poll for the visible task.
      .filter((task) => !(scene === "room" && task.id === currentTaskId))
      .map((task) => task.id);
    if (activeIds.length > 0) await refreshDetails(activeIds);
  }, 2500);

  const projects = useMemo<ProjectNode[]>(() => {
    const taskMap = new Map<string, Task[]>();
    for (const workspace of workspaces) taskMap.set(workspace.canonical_path, []);
    for (const task of tasks) {
      if (!task.workspace_path) continue;
      const projectTasks = taskMap.get(task.workspace_path) ?? [];
      projectTasks.push(task);
      taskMap.set(task.workspace_path, projectTasks);
    }
    const priority = (task: Task) => {
      const state = visualTaskState(task, details[task.id]);
      return state === "attention" ? 0 : state === "review" ? 1 : isTaskLive(task, details[task.id]) ? 2 : 3;
    };
    return workspaces.map((workspace) => ({
      workspace,
      tasks: (taskMap.get(workspace.canonical_path) ?? []).sort(
        (left, right) => priority(left) - priority(right) || right.updated_at.localeCompare(left.updated_at),
      ),
    }));
  }, [details, tasks, workspaces]);

  const floatingTasks = useMemo(
    () => tasks
      .filter((task) => !task.workspace_path)
      .sort((left, right) => right.updated_at.localeCompare(left.updated_at)),
    [tasks],
  );

  const toggleProject = (workspacePath: string) => {
    setCollapsedProjects((previous) => {
      const next = new Set(previous);
      if (next.has(workspacePath)) next.delete(workspacePath);
      else next.add(workspacePath);
      return next;
    });
  };

  const openProject = (workspacePath: string) => {
    openDashboard(workspacePath);
  };

  return (
    <aside className="rail app-sidebar" aria-label={t("shell.navigationLabel")}>
      <div className="sidebar-brand-row">
        <button className="sidebar-brand" onClick={() => openNewConversation(null)} title={t("shell.brandNewConversationTitle")} aria-label={t("shell.brandNewConversationAria")}>
          <span className="sidebar-brand-mark" aria-hidden="true">R</span>
          <span className="rail-label">R-Code</span>
        </button>
        <button className="sidebar-search" onClick={toggleSearch} title={t("shell.searchTitle", { shortcut: keyLabel("search") })} aria-label={t("shell.searchAria", { shortcut: keyLabel("search") })}>
          <IconSearch width={16} height={16} />
        </button>
      </div>
      <div className="sidebar-create">
        <button
          className="sidebar-new"
          onClick={() => void createConversation(null)}
          title={creatingConversation ? t("shell.creatingConversation") : t("shell.newConversation")}
          aria-label={t("shell.newConversation")}
          aria-busy={creatingConversation}
          disabled={creatingConversation}
        >
          <IconPlus width={17} height={17} />
          <span className="rail-label">{t("shell.newConversation")}</span>
        </button>
        <button type="button" className="sidebar-collapse" onClick={toggleRail} aria-label={collapsed ? t("shell.expandSidebar") : t("shell.collapseSidebar")} title={collapsed ? t("shell.expandSidebar") : t("shell.collapseSidebar")}>
          <IconSidebar width={17} height={17} />
        </button>
      </div>

      <nav className="sidebar-nav" aria-label={t("shell.globalNavigation")}>
        <NavItem icon={<IconHistory />} label={t("shell.conversations")} active={scene === "home" || scene === "conversations" || scene === "room"} onClick={() => setScene("conversations")} />
        <NavItem icon={<IconInbox />} label={t("shell.inbox")} active={scene === "inbox"} count={needsCount} onClick={() => setScene("inbox")} />
        <NavItem icon={<IconActivity />} label={t("shell.activity")} active={scene === "deck"} onClick={() => setScene("deck")} />
        <NavItem icon={<IconArchive />} label={t("shell.archive")} active={scene === "archive"} onClick={() => setScene("archive")} />
      </nav>

      <div className="sidebar-recent">
        <div className="sidebar-section-head">
          <span className="rail-label">{t("shell.recent")}</span>
        </div>
        {floatingTasks.length === 0 ? (
          <p className="sidebar-recent-empty rail-label">{t("shell.noChats")}</p>
        ) : (
          <div className="sidebar-task-list">
            {floatingTasks.map((task) => {
              const state = visualTaskState(task, details[task.id]);
              const active = scene === "room" && currentTaskId === task.id;
              return (
                <div className={`sidebar-task-row${active ? " active" : ""}`} key={task.id}>
                  <button
                    className={`sidebar-task${active ? " active" : ""}`}
                    onClick={() => {
                      setCurrentWorkspace(null);
                      openRoom(task.id);
                    }}
                    title={`${taskTitle(task)} · ${taskStateLabel(task.state, details[task.id])}`}
                  >
                    {MARKED_STATES.has(state) && <i className={`task-state-dot ${state}`} aria-hidden="true" />}
                    <span className="rail-label">{taskTitle(task)}</span>
                    <time className="rail-label">{elapsedMinutes(task.updated_at)}</time>
                  </button>
                  <TaskActionsMenu
                    task={task}
                    detail={details[task.id]}
                    className="sidebar-task-actions"
                    placement="right"
                  />
                </div>
              );
            })}
          </div>
        )}
      </div>

      <div className="sidebar-projects">
        <div className="sidebar-section-head">
          <span className="rail-label">{t("shell.projects")}</span>
          {runningCount > 0 && <small className="rail-label">{t("shell.runningCount", { count: runningCount })}</small>}
          <button
            className="sidebar-project-manage"
            onClick={() => setScene("projects")}
            aria-label={t("shell.addProject")}
            title={t("shell.addLocalProject")}
          >
            <IconPlus width={13} height={13} />
            <span className="rail-label">{t("shell.add")}</span>
          </button>
        </div>
        <div className="sidebar-project-list">
          {projects.length === 0 ? (
            <button className="sidebar-empty-project" onClick={() => setScene("projects")}>
              <IconPlus width={15} height={15} />
              <span className="rail-label">{t("shell.attachFirstProject")}</span>
            </button>
          ) : projects.map(({ workspace, tasks: projectTasks }) => {
            const current = workspace.canonical_path === currentWorkspacePath && scene === "dashboard";
            const isCollapsed = collapsedProjects.has(workspace.canonical_path);
            return (
              <section className={`sidebar-project${current ? " selected" : ""}${isCollapsed ? " is-collapsed" : ""}`} key={workspace.canonical_path}>
                <div className="sidebar-project-row">
                  <button type="button" className="sidebar-project-toggle" onClick={() => toggleProject(workspace.canonical_path)} aria-expanded={!isCollapsed} aria-label={t(isCollapsed ? "shell.expandProjectTasks" : "shell.collapseProjectTasks", { project: workspace.display_name })} title={isCollapsed ? t("shell.expandTasks") : t("shell.collapseTasks")}>
                    {isCollapsed ? <IconChevronRight width={14} height={14} /> : <IconChevronDown width={14} height={14} />}
                  </button>
                  <button className="sidebar-project-head" onClick={() => openProject(workspace.canonical_path)} title={t("shell.openProjectOverview", { project: workspace.display_name })}>
                    <IconFolderOpen width={16} height={16} />
                    <span className="rail-label">{workspace.display_name}</span>
                  </button>
                  <ProjectActionsMenu workspace={workspace} />
                </div>
                <div className="sidebar-task-list">
                  {projectTasks.slice(0, 6).map((task) => {
                    const state = visualTaskState(task, details[task.id]);
                    const active = scene === "room" && currentTaskId === task.id;
                    return (
                      <div className={`sidebar-task-row${active ? " active" : ""}`} key={task.id}>
                        <button
                          className={`sidebar-task${active ? " active" : ""}`}
                          onClick={() => {
                            openRoom(task.id);
                            if (task.workspace_path) setCurrentWorkspace(task.workspace_path);
                          }}
                          title={`${taskTitle(task)} · ${taskStateLabel(task.state, details[task.id])}`}
                        >
                          {MARKED_STATES.has(state) && <i className={`task-state-dot ${state}`} aria-hidden="true" />}
                          <span className="rail-label">{taskTitle(task)}</span>
                          <time className="rail-label">{elapsedMinutes(task.updated_at)}</time>
                        </button>
                        <TaskActionsMenu
                          task={task}
                          detail={details[task.id]}
                          className="sidebar-task-actions"
                          placement="right"
                        />
                      </div>
                    );
                  })}
                  {projectTasks.length > 6 && <button className="sidebar-more-tasks" onClick={() => openProject(workspace.canonical_path)}><span className="rail-label">{t("shell.viewAllTasks", { count: projectTasks.length })}</span></button>}
                </div>
              </section>
            );
          })}
        </div>
      </div>

      <div className="sidebar-footer">
        <button className={`sidebar-footer-action${scene === "settings" ? " active" : ""}`} onClick={() => setScene("settings")} aria-label={t("shell.settings")}>
          <IconSettings width={16} height={16} />
          <span className="rail-label">{t("shell.settings")}</span>
        </button>
      </div>
    </aside>
  );
}

function NavItem({ icon, label, active, count, onClick }: { icon: React.ReactNode; label: string; active: boolean; count?: number; onClick: () => void }) {
  return (
    <button className={`sidebar-nav-item${active ? " active" : ""}`} aria-current={active ? "page" : undefined} onClick={onClick} title={label}>
      {icon}
      <span className="rail-label">{label}</span>
      {count ? <b className="rail-label">{count}</b> : null}
    </button>
  );
}
