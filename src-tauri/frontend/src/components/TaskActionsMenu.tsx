import { useCallback, useState } from "react";
import { taskArchive, taskDelete } from "../lib/ipc";
import { isTaskLive, taskTitle } from "../lib/presentation";
import { useAppStore } from "../store/app";
import { useTasksStore } from "../store/tasks";
import { pushToast } from "../store/toast";
import type { Task, TaskDetail } from "../lib/types";
import { IconArchive, IconMore, IconTrash } from "./icons";
import { ConfirmDialog } from "./ui/ConfirmDialog";
import { Menu, MenuItem, MenuSeparator } from "./ui/Menu";

interface Props {
  task: Task;
  detail?: TaskDetail;
  className?: string;
  placement?: "up" | "down";
  onChanged?: () => void;
}

/** 项目树、全局列表和任务页共用的一套会话生命周期菜单。 */
export function TaskActionsMenu({ task, detail, className, placement = "down", onChanged }: Props) {
  const refreshTasks = useTasksStore((state) => state.refreshTasks);
  const currentTaskId = useAppStore((state) => state.currentTaskId);
  const openConversations = useAppStore((state) => state.openConversations);
  const [busy, setBusy] = useState<"archive" | "delete" | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const live = isTaskLive(task, detail);
  const title = taskTitle(task);

  const leaveRemovedRoom = useCallback(() => {
    if (currentTaskId === task.id) openConversations();
  }, [currentTaskId, openConversations, task.id]);

  const archive = useCallback(async () => {
    if (busy || live) return;
    setBusy("archive");
    try {
      await taskArchive(task.id);
      await refreshTasks();
      leaveRemovedRoom();
      onChanged?.();
      pushToast({ kind: "success", title: "对话已归档", body: title });
    } catch (cause) {
      pushToast({ kind: "error", title: "无法归档对话", body: String(cause) });
    } finally {
      setBusy(null);
    }
  }, [busy, leaveRemovedRoom, live, onChanged, refreshTasks, task.id, title]);

  const remove = useCallback(async () => {
    if (busy || live) return;
    setBusy("delete");
    try {
      await taskDelete(task.id);
      await refreshTasks();
      setConfirmDelete(false);
      leaveRemovedRoom();
      onChanged?.();
      pushToast({ kind: "success", title: "对话已永久删除", body: title });
    } catch (cause) {
      pushToast({ kind: "error", title: "无法删除对话", body: String(cause) });
    } finally {
      setBusy(null);
    }
  }, [busy, leaveRemovedRoom, live, onChanged, refreshTasks, task.id, title]);

  const blockedHint = live ? "先停止当前运行" : undefined;

  return (
    <>
      <Menu
        className={className}
        placement={placement}
        align="right"
        label={`管理对话：${title}`}
        disabled={busy != null}
        menuClassName="task-actions-popover"
        trigger={(
          <button
            type="button"
            className="task-actions-trigger"
            aria-label={`管理对话：${title}`}
            title="对话选项"
            onClick={(event) => event.stopPropagation()}
          >
            <IconMore width={16} height={16} />
          </button>
        )}
      >
        {({ close }) => (
          <>
            <MenuItem
              disabled={live || task.state === "archived"}
              hint={blockedHint ?? "从项目列表移出，保留历史记录"}
              close={close}
              onSelect={() => void archive()}
            >
              <IconArchive width={15} height={15} />
              归档对话
            </MenuItem>
            <MenuSeparator />
            <MenuItem
              className="danger"
              disabled={live}
              hint={blockedHint ?? "永久删除对话与审计记录，不删除项目文件"}
              closeOnSelect={false}
              onSelect={() => {
                close();
                setConfirmDelete(true);
              }}
            >
              <IconTrash width={15} height={15} />
              永久删除…
            </MenuItem>
          </>
        )}
      </Menu>
      <ConfirmDialog
        open={confirmDelete}
        title="永久删除这段对话？"
        description={`“${title}”的消息、运行记录与审计历史会被永久删除。项目目录和其中的文件不会被删除。`}
        confirmLabel="永久删除"
        busy={busy === "delete"}
        onCancel={() => {
          if (!busy) setConfirmDelete(false);
        }}
        onConfirm={() => void remove()}
      />
    </>
  );
}
