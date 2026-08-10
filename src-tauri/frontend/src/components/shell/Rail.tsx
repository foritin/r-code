import { useMemo } from "react";
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
  IconFolderOpen,
  IconHistory,
  IconInbox,
  IconPlus,
  IconSearch,
  IconSettings,
  IconSidebar,
  IconText,
} from "../icons";

interface ProjectNode {
  workspace: Workspace;
  tasks: Task[];
}

/** 左侧只负责全局导航与项目树，不承载项目动态；项目动态仅在 DashboardScene 内。 */
export function Rail() {
  const scene = useAppStore((s) => s.scene);
  const setScene = useAppStore((s) => s.setScene);
  const goHome = useAppStore((s) => s.goHome);
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

  usePoll(async () => {
    await refreshTasks();
    const snapshot = useTasksStore.getState();
    const activeIds = snapshot.tasks
      .filter((task) => isTaskLive(task, snapshot.details[task.id]))
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

  const openProject = (workspacePath: string) => {
    openDashboard(workspacePath);
  };

  return (
    <aside className="rail app-sidebar" aria-label="项目与导航">
      <div className="sidebar-brand-row">
        <button className="sidebar-brand" onClick={goHome} title="R-Code — 新对话" aria-label="R-Code，新建对话">
          <span className="sidebar-brand-mark" aria-hidden="true">R</span>
          <span className="rail-label">R-Code</span>
        </button>
        <button className="sidebar-search" onClick={toggleSearch} title={`搜索（${keyLabel("search")}）`} aria-label={`搜索任务、文件和对话，${keyLabel("search")}`}>
          <IconSearch width={16} height={16} />
        </button>
      </div>
      <div className="sidebar-create">
        <button
          className="sidebar-new"
          onClick={() => void createConversation(currentWorkspacePath)}
          title={creatingConversation ? "正在创建新对话" : "新对话"}
          aria-label="新对话"
          aria-busy={creatingConversation}
          disabled={creatingConversation}
        >
          <IconPlus width={17} height={17} />
          <span className="rail-label">新对话</span>
        </button>
        <button type="button" className="sidebar-collapse" onClick={toggleRail} aria-label={collapsed ? "展开侧边栏" : "收起侧边栏"} title={collapsed ? "展开侧边栏" : "收起侧边栏"}>
          <IconSidebar width={17} height={17} />
        </button>
      </div>

      <nav className="sidebar-nav" aria-label="全局功能">
        <NavItem icon={<IconHistory />} label="对话" active={scene === "home" || scene === "conversations" || scene === "room"} onClick={() => setScene("conversations")} />
        <NavItem icon={<IconInbox />} label="待处理" active={scene === "inbox"} count={needsCount} onClick={() => setScene("inbox")} />
        <NavItem icon={<IconActivity />} label="活动" active={scene === "deck"} onClick={() => setScene("deck")} />
        <NavItem icon={<IconArchive />} label="归档" active={scene === "archive"} onClick={() => setScene("archive")} />
        <NavItem icon={<IconText />} label="知识与指令" active={scene === "knowledge"} onClick={() => setScene("knowledge")} />
      </nav>

      <div className="sidebar-projects">
        <div className="sidebar-section-head">
          <span className="rail-label">项目</span>
          {runningCount > 0 && <small className="rail-label">{runningCount} 运行中</small>}
          <button
            className="sidebar-project-manage"
            onClick={() => setScene("projects")}
            aria-label="添加项目"
            title="添加本地项目"
          >
            <IconPlus width={13} height={13} />
            <span className="rail-label">添加</span>
          </button>
        </div>
        <div className="sidebar-project-list">
          {projects.length === 0 ? (
            <button className="sidebar-empty-project" onClick={() => setScene("projects")}>
              <IconPlus width={15} height={15} />
              <span className="rail-label">附加第一个项目</span>
            </button>
          ) : projects.map(({ workspace, tasks: projectTasks }) => {
            const current = workspace.canonical_path === currentWorkspacePath && scene === "dashboard";
            return (
              <section className={`sidebar-project${current ? " selected" : ""}`} key={workspace.canonical_path}>
                <div className="sidebar-project-row">
                  <button className="sidebar-project-head" onClick={() => openProject(workspace.canonical_path)} title={`打开 ${workspace.display_name} 项目概览`}>
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
                          <i className={`task-state-dot ${state}`} />
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
                  {projectTasks.length > 6 && <button className="sidebar-more-tasks" onClick={() => openProject(workspace.canonical_path)}><span className="rail-label">查看全部 {projectTasks.length} 个任务</span></button>}
                </div>
              </section>
            );
          })}
        </div>
      </div>

      <div className="sidebar-footer">
        <button className={`sidebar-footer-action${scene === "settings" ? " active" : ""}`} onClick={() => setScene("settings")} aria-label="设置">
          <IconSettings width={16} height={16} />
          <span className="rail-label">设置</span>
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
