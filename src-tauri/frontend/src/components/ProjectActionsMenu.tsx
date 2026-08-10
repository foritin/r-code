import { useCallback, useMemo, useState } from "react";
import { taskList, workspaceForget } from "../lib/ipc";
import { isTaskLive } from "../lib/presentation";
import type { Workspace } from "../lib/types";
import { useAppStore } from "../store/app";
import { useTasksStore } from "../store/tasks";
import { pushToast } from "../store/toast";
import { IconEditor, IconFolderOpen, IconMore, IconPlus, IconText, IconTrash } from "./icons";
import { ConfirmDialog } from "./ui/ConfirmDialog";
import { Menu, MenuItem, MenuSeparator } from "./ui/Menu";
import { useCreateConversation } from "./useCreateConversation";

interface PendingRemoval {
  taskIds: string[];
  sessionCount: number;
}

/** 项目行的次要操作；菜单由 portal 承载，不受侧栏滚动容器裁剪。 */
export function ProjectActionsMenu({ workspace }: { workspace: Workspace }) {
  const tasks = useTasksStore((state) => state.tasks);
  const details = useTasksStore((state) => state.details);
  const refreshWorkspaces = useTasksStore((state) => state.refreshWorkspaces);
  const refreshTasks = useTasksStore((state) => state.refreshTasks);
  const refreshActivity = useTasksStore((state) => state.refreshActivity);
  const setCurrentProject = useTasksStore((state) => state.setCurrentProject);
  const scene = useAppStore((state) => state.scene);
  const openDashboard = useAppStore((state) => state.openDashboard);
  const openConversations = useAppStore((state) => state.openConversations);
  const setScene = useAppStore((state) => state.setScene);
  const forgetTaskNavigation = useAppStore((state) => state.forgetTaskNavigation);
  const [checking, setChecking] = useState(false);
  const [removing, setRemoving] = useState(false);
  const [pendingRemoval, setPendingRemoval] = useState<PendingRemoval | null>(null);
  const { createConversation: createPreparedConversation, isCreating } = useCreateConversation();
  const creatingConversation = isCreating(workspace.canonical_path);

  const projectTasks = useMemo(
    () => tasks.filter((task) => task.workspace_path === workspace.canonical_path),
    [tasks, workspace.canonical_path],
  );
  const hasLiveTask = projectTasks.some((task) => isTaskLive(task, details[task.id]));

  const openProjectFiles = useCallback(() => {
    setCurrentProject(workspace.canonical_path);
    setScene("editor");
  }, [setCurrentProject, setScene, workspace.canonical_path]);

  const createConversation = useCallback(() => {
    void createPreparedConversation(workspace.canonical_path);
  }, [createPreparedConversation, workspace.canonical_path]);

  const openKnowledge = useCallback(() => {
    setCurrentProject(workspace.canonical_path);
    setScene("knowledge");
  }, [setCurrentProject, setScene, workspace.canonical_path]);

  const prepareRemoval = useCallback(async () => {
    if (checking || removing || hasLiveTask) return;
    setChecking(true);
    try {
      const sessions = await taskList(workspace.canonical_path, true);
      setPendingRemoval({ sessionCount: sessions.length, taskIds: sessions.map((task) => task.id) });
    } catch (cause) {
      pushToast({ kind: "error", title: "无法检查项目记录", body: String(cause) });
    } finally {
      setChecking(false);
    }
  }, [checking, hasLiveTask, removing, workspace.canonical_path]);

  const removeProject = useCallback(async () => {
    if (!pendingRemoval || removing) return;
    setRemoving(true);
    try {
      const result = await workspaceForget(workspace.canonical_path);
      pendingRemoval.taskIds.forEach(forgetTaskNavigation);
      await Promise.all([refreshWorkspaces(), refreshTasks(), refreshActivity()]);
      if (scene === "dashboard" || scene === "editor") openConversations();
      setPendingRemoval(null);
      pushToast({
        kind: "success",
        title: "项目已从 R-Code 清除",
        body: `${workspace.display_name} · 已清除 ${result.removed_sessions} 段对话 · 磁盘文件未更改`,
      });
    } catch (cause) {
      pushToast({ kind: "error", title: "无法清除项目", body: String(cause) });
    } finally {
      setRemoving(false);
    }
  }, [forgetTaskNavigation, openConversations, pendingRemoval, refreshActivity, refreshTasks, refreshWorkspaces, removing, scene, workspace.canonical_path, workspace.display_name]);

  return (
    <>
      <Menu
        className="sidebar-project-actions"
        placement="right"
        gap={18}
        label={`${workspace.display_name} 项目操作`}
        disabled={checking || removing || creatingConversation}
        menuClassName="project-actions-popover"
        trigger={(
          <button
            type="button"
            className="sidebar-project-actions-trigger"
            aria-label={`${workspace.display_name} 项目操作`}
            title="项目选项"
            onClick={(event) => event.stopPropagation()}
          >
            <IconMore width={16} height={16} />
          </button>
        )}
      >
        {({ close }) => <>
          <MenuItem close={close} onSelect={createConversation}>
            <IconPlus width={15} height={15} />
            {creatingConversation ? "正在创建…" : "新建对话"}
          </MenuItem>
          <MenuSeparator />
          <MenuItem close={close} onSelect={() => openDashboard(workspace.canonical_path)}>
            <IconFolderOpen width={15} height={15} />
            打开项目
          </MenuItem>
          <MenuItem close={close} onSelect={openProjectFiles}>
            <IconEditor width={15} height={15} />
            项目文件
          </MenuItem>
          <MenuItem close={close} onSelect={openKnowledge}>
            <IconText width={15} height={15} />
            知识与指令
          </MenuItem>
          <MenuSeparator />
          <MenuItem
            className="danger project-remove-menu-item"
            disabled={hasLiveTask}
            closeOnSelect={false}
            onSelect={() => {
              close();
              void prepareRemoval();
            }}
          >
            <IconTrash width={15} height={15} />
            {checking ? "正在检查…" : "从 R-Code 移除…"}
          </MenuItem>
        </>}
      </Menu>
      <ConfirmDialog
        open={pendingRemoval != null}
        title="从 R-Code 中清除这个项目？"
        description={`这会永久清除“${workspace.display_name}”在 R-Code 中的项目记录、${pendingRemoval?.sessionCount ?? 0} 段对话以及关联的运行与审计数据。真实文件夹及其中的文件不会被删除、移动或修改。`}
        confirmLabel="清除项目"
        busyLabel="正在清除…"
        busy={removing}
        onCancel={() => {
          if (!removing) setPendingRemoval(null);
        }}
        onConfirm={() => void removeProject()}
      />
    </>
  );
}
