import { useMemo, useState } from "react";
import { useAppStore } from "../../store/app";
import {
  useTasksStore,
  selectNeedsYouTaskIds,
  selectRunning,
} from "../../store/tasks";
import { usePoll } from "../../lib/poll";
import { keyLabel } from "../../lib/keys";
import { displayPath, elapsedMinutes } from "../../lib/format";
import { taskArchive, workspaceChoose } from "../../lib/ipc";
import type { Scene } from "../../store/app";
import type { Task } from "../../lib/types";
import { projectAccessModeShortLabel } from "../ProjectAccessSelector";
import {
  IconArchive,
  IconChevronDown,
  IconDeck,
  IconFolderOpen,
  IconHome,
  IconInbox,
  IconMessageCircle,
  IconPlus,
  IconProjects,
  IconSearch,
  IconSettings,
} from "../icons";

type ProjectTreeNode = {
  path: string;
  name: string;
  tasks: Task[];
  liveCount: number;
};

type RailHover =
  | { kind: "project"; project: ProjectTreeNode; rect: DOMRect }
  | { kind: "session"; task: Task; status: string; rect: DOMRect };

/** 项目与聊天会话树。项目根节点来自已附加工作区，不会因会话数量而被截断。 */
export function Rail() {
  const scene = useAppStore((s) => s.scene);
  const setScene = useAppStore((s) => s.setScene);
  const goHome = useAppStore((s) => s.goHome);
  const toggleSearch = useAppStore((s) => s.toggleSearch);
  const tasks = useTasksStore((s) => s.tasks);
  const refreshTasks = useTasksStore((s) => s.refreshTasks);
  const refreshDetails = useTasksStore((s) => s.refreshDetails);
  const refreshWorkspaces = useTasksStore((s) => s.refreshWorkspaces);
  const workspaces = useTasksStore((s) => s.workspaces);
  const details = useTasksStore((s) => s.details);
  const currentWorkspacePath = useTasksStore((s) => s.currentProjectId);
  const setCurrentWorkspace = useTasksStore((s) => s.setCurrentProject);
  const needsIds = useTasksStore(selectNeedsYouTaskIds);
  const runningCount = useTasksStore((s) => selectRunning(s).length);
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const [hoveredItem, setHoveredItem] = useState<RailHover | null>(null);

  usePoll(async () => {
    await refreshTasks();
    const active = useTasksStore
      .getState()
      .tasks.filter((task) => task.state !== "idle" && task.state !== "archived")
      .map((task) => task.id);
    if (active.length > 0) await refreshDetails(active);
  }, 2500);

  const tree = useMemo(() => {
    const byWorkspace = new Map<string, Task[]>();
    for (const workspace of workspaces) byWorkspace.set(workspace.canonical_path, []);
    const chats: Task[] = [];
    for (const task of tasks) {
      if (!task.workspace_path) {
        chats.push(task);
        continue;
      }
      const group = byWorkspace.get(task.workspace_path) ?? [];
      group.push(task);
      byWorkspace.set(task.workspace_path, group);
    }
    const live = (task: Task) =>
      task.state === "in_progress" ||
      task.state === "exploring" ||
      details[task.id]?.runs.some((run) => run.ended_at == null) === true;
    const sortTasks = (left: Task, right: Task) =>
      Number(live(right)) - Number(live(left)) || right.updated_at.localeCompare(left.updated_at);
    const workspaceByPath = new Map(workspaces.map((workspace) => [workspace.canonical_path, workspace]));
    const projects: ProjectTreeNode[] = [...byWorkspace.entries()]
      .map(([path, projectTasks]) => ({
        path,
        name:
          workspaceByPath.get(path)?.display_name ??
          path.split(/[\\/]/).pop() ??
          path,
        tasks: projectTasks.sort(sortTasks),
        liveCount: projectTasks.filter(live).length,
      }))
      .sort(
        (left, right) =>
          Number(right.liveCount > 0) - Number(left.liveCount > 0) ||
          (right.tasks[0]?.updated_at ?? "").localeCompare(left.tasks[0]?.updated_at ?? "") ||
          left.name.localeCompare(right.name)
      );
    return { projects, chats: chats.sort(sortTasks), live };
  }, [details, tasks, workspaces]);

  const isExpanded = (key: string) => expanded[key] ?? true;
  const toggleExpanded = (key: string) =>
    setExpanded((current) => ({ ...current, [key]: !(current[key] ?? true) }));

  const chooseFolder = async () => {
    try {
      const workspace = await workspaceChoose();
      if (!workspace) return;
      await refreshWorkspaces();
      setCurrentWorkspace(workspace.canonical_path);
      goHome();
    } catch {
      setScene("projects");
    }
  };

  const currentWorkspace = workspaces.find((workspace) => workspace.canonical_path === currentWorkspacePath);
  const collapsed = useAppStore((s) => s.railCollapsed);
  const toggleRail = useAppStore((s) => s.toggleRail);

  return (
    <aside className="rail" aria-label="会话和导航">
      <div className="rail-top">
        <button className="rail-new" onClick={goHome} title={`新对话（${keyLabel("new")}）`}>
          <IconPlus width={15} height={15} />
          <span className="rail-label">新对话</span>
        </button>
        <button
          className="rail-search"
          onClick={toggleSearch}
          title={`搜索本地文件（${keyLabel("search")}）`}
        >
          <IconSearch width={14} height={14} />
          <span className="rail-label">搜索</span>
          <kbd>{keyLabel("search")}</kbd>
        </button>
        <button
          className="rail-collapse iconbtn"
          onClick={toggleRail}
          aria-label={collapsed ? "展开侧栏" : "折叠侧栏"}
          aria-expanded={!collapsed}
          title={`${collapsed ? "展开" : "折叠"}侧栏（${keyLabel("toggleRail")}）`}
        >
          <IconChevronDown width={14} height={14} />
        </button>
      </div>

      <nav className="rail-nav" aria-label="功能">
        <NavItem icon={<IconHome />} label="对话" active={scene === "home" || scene === "room"} onClick={goHome} />
        <NavItem
          icon={<IconInbox />}
          label="待处理"
          active={scene === "inbox"}
          count={needsIds.size}
          onClick={() => setScene("inbox")}
        />
        <NavItem icon={<IconDeck />} label="活动" active={scene === "deck"} onClick={() => setScene("deck")} />
        <NavItem icon={<IconProjects />} label="文件夹" active={scene === "projects" || scene === "editor"} onClick={() => setScene("projects")} />
      </nav>

      <div className="rail-scroll">
        <div className="rail-tree">
          <section className="rail-tree-section" aria-label="项目">
            <div className="rail-tree-section-head">
              <span>项目</span>
              {runningCount > 0 && <span className="rail-running">{runningCount} 进行中</span>}
            </div>
            <div className="rail-project-list">
              {tree.projects.length === 0 ? (
                <div className="rail-tree-empty">尚未附加项目</div>
              ) : (
                tree.projects.map((project) => {
                  const key = `project:${project.path}`;
                  const open = isExpanded(key);
                  const current = currentWorkspacePath === project.path;
                  return (
                    <section className={`rail-project-node${current ? " current" : ""}`} key={project.path}>
                      <div
                        className="rail-project-row"
                        onMouseEnter={(event) =>
                          setHoveredItem({ kind: "project", project, rect: event.currentTarget.getBoundingClientRect() })
                        }
                        onMouseLeave={() => setHoveredItem(null)}
                        onFocus={(event) =>
                          setHoveredItem({ kind: "project", project, rect: event.currentTarget.getBoundingClientRect() })
                        }
                        onBlur={() => setHoveredItem(null)}
                      >
                        <button
                          type="button"
                          className={`rail-project-head${current ? " current" : ""}`}
                          aria-expanded={open}
                          onClick={() => toggleExpanded(key)}
                          title={displayPath(project.path)}
                        >
                          {open ? <IconFolderOpen width={17} height={17} /> : <IconProjects width={17} height={17} />}
                          <span className="rail-project-name">{project.name}</span>
                          {project.liveCount > 0 && <i className="rail-project-live">{project.liveCount}</i>}
                        </button>
                        <button
                          type="button"
                          className="rail-project-new"
                          title={`在 ${project.name} 中新建会话`}
                          aria-label={`在 ${project.name} 中新建会话`}
                          onClick={() => {
                            setCurrentWorkspace(project.path);
                            goHome();
                          }}
                        >
                          <IconPlus width={15} height={15} />
                        </button>
                      </div>
                      {open && (
                        <div className="rail-project-sessions">
                          {project.tasks.length === 0 ? (
                            <div className="rail-tree-empty">暂无会话</div>
                          ) : (
                            project.tasks.map((task) => (
                              <SessionRow
                                key={task.id}
                                task={task}
                                needsYou={needsIds.has(task.id)}
                                live={tree.live(task)}
                                onHover={(status, rect) => setHoveredItem({ kind: "session", task, status, rect })}
                                onLeave={() => setHoveredItem(null)}
                              />
                            ))
                          )}
                        </div>
                      )}
                    </section>
                  );
                })
              )}
            </div>
          </section>

          <section className="rail-tree-section rail-recent-section" aria-label="最近">
            <div className="rail-tree-section-head">
              <span>最近</span>
            </div>
            <div className="rail-recent-list">
              {tree.chats.length === 0 ? (
                <div className="rail-tree-empty">暂无聊天</div>
              ) : (
                tree.chats.map((task) => (
                  <SessionRow
                    key={task.id}
                    task={task}
                    needsYou={needsIds.has(task.id)}
                    live={tree.live(task)}
                    onHover={(status, rect) => setHoveredItem({ kind: "session", task, status, rect })}
                    onLeave={() => setHoveredItem(null)}
                  />
                ))
              )}
            </div>
          </section>
        </div>
      </div>
      {hoveredItem && (
        <div
          className={`rail-hover-card rail-hover-card-${hoveredItem.kind}`}
          role="tooltip"
          style={{ left: hoveredItem.rect.right + 8, top: hoveredItem.rect.top }}
        >
          {hoveredItem.kind === "project" ? (
            <>
              <div className="rail-hover-title"><IconFolderOpen width={18} height={18} /><strong>{hoveredItem.project.name}</strong></div>
              <span><IconMessageCircle width={17} height={17} />{hoveredItem.project.tasks.length} 个对话串</span>
              <span><IconProjects width={17} height={17} />{displayPath(hoveredItem.project.path)}</span>
            </>
          ) : (
            <>
              <div className="rail-hover-title"><strong>{sessionTitle(hoveredItem.task)}</strong><time>{elapsedMinutes(hoveredItem.task.updated_at)}</time></div>
              <span><IconProjects width={17} height={17} />{workspaceLabel(hoveredItem.task.workspace_path)}</span>
              <span><IconMessageCircle width={17} height={17} />{hoveredItem.status}</span>
            </>
          )}
        </div>
      )}

      <div className="rail-workspace">
        {currentWorkspace ? (
          <button className="rail-workspace-current" onClick={() => setScene("projects")} title={displayPath(currentWorkspace.canonical_path)}>
            <IconProjects width={13} height={13} />
            <span>{currentWorkspace.display_name}</span>
            <small>{projectAccessModeShortLabel(currentWorkspace.access_mode)}</small>
          </button>
        ) : (
          <button className="rail-workspace-current empty" onClick={() => void chooseFolder()}>
            <IconProjects width={13} height={13} />
            <span>附加文件夹</span>
          </button>
        )}
        <button className={`rail-settings${scene === "settings" ? " active" : ""}`} onClick={() => setScene("settings")}>
          <IconSettings width={14} height={14} /> 设置
        </button>
      </div>
    </aside>
  );
}

function NavItem({
  icon,
  label,
  active,
  count,
  onClick,
}: {
  icon: React.ReactNode;
  label: string;
  active: boolean;
  count?: number;
  onClick: () => void;
}) {
  return (
    <button
      className={`rail-nav-item${active ? " active" : ""}`}
      aria-current={active ? "page" : undefined}
      title={label}
      onClick={onClick}
    >
      {icon}
      <span className="rail-label">{label}</span>
      {count ? (
        <small aria-label={`${count} 项${label}`}>{count}</small>
      ) : null}
    </button>
  );
}

function sessionTitle(task: Task): string {
  return task.title.trim() || task.goal.trim() || "未命名会话";
}

function workspaceLabel(path: string | null): string {
  return path?.split(/[\\/]/).filter(Boolean).pop() ?? "最近聊天";
}

function sessionStatus(task: Task, needsYou: boolean, live: boolean): string {
  if (needsYou) return "需要处理";
  if (live) return task.state === "exploring" ? "正在分析" : "正在执行";
  if (task.state === "review_ready") return "等待审查";
  if (task.state === "interrupted") return "已中止";
  return "已完成";
}

function SessionRow({
  task,
  needsYou,
  live,
  onHover,
  onLeave,
}: {
  task: Task;
  needsYou: boolean;
  live: boolean;
  onHover: (status: string, rect: DOMRect) => void;
  onLeave: () => void;
}) {
  const openRoom = useAppStore((s) => s.openRoom);
  const goHome = useAppStore((s) => s.goHome);
  const currentTaskId = useAppStore((s) => s.currentTaskId);
  const scene = useAppStore((s) => s.scene);
  const refreshTasks = useTasksStore((s) => s.refreshTasks);
  const [archiving, setArchiving] = useState(false);
  const [archiveError, setArchiveError] = useState<string | null>(null);
  const status = sessionStatus(task, needsYou, live);

  const archive = async () => {
    if (live || archiving) return;
    setArchiving(true);
    setArchiveError(null);
    try {
      await taskArchive(task.id);
      onLeave();
      if (currentTaskId === task.id) goHome();
      await refreshTasks();
    } catch (cause) {
      setArchiveError(`归档失败：${String(cause)}`);
    } finally {
      setArchiving(false);
    }
  };

  return (
    <div
      className="srow-wrap"
      onMouseEnter={(event) => onHover(status, event.currentTarget.getBoundingClientRect())}
      onMouseLeave={onLeave}
    >
      <button
        className={`srow ring-inset${live ? " live" : ""}${needsYou ? " needs-you" : ""}${scene === "room" && currentTaskId === task.id ? " sel" : ""}`}
        onClick={() => openRoom(task.id)}
        title={`${sessionTitle(task)} · ${status}`}
      >
        <span className="srow-state" aria-label={status} />
        <span className="name">{sessionTitle(task)}</span>
        <span className="srow-time">{elapsedMinutes(task.updated_at)}</span>
      </button>
      <button
        type="button"
        className="srow-archive reveal-on-hover"
        disabled={live || archiving}
        title={live ? "会话仍在运行，请先停止后归档" : "归档会话"}
        aria-label="归档会话"
        onClick={() => void archive()}
      >
        <IconArchive width={13} height={13} />
      </button>
      {archiveError && <span className="srow-toast" role="alert">{archiveError}</span>}
    </div>
  );
}
