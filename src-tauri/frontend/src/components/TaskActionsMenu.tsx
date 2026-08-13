import { useCallback, useState } from "react";
import { taskArchive, taskDelete, taskRename, taskRestore } from "../lib/ipc";
import { isTaskLive, taskTitle } from "../lib/presentation";
import { useAppStore } from "../store/app";
import { useTasksStore } from "../store/tasks";
import { pushToast } from "../store/toast";
import type { Task, TaskDetail } from "../lib/types";
import { IconArchive, IconEdit, IconMore, IconRestore, IconTrash } from "./icons";
import { ConfirmDialog } from "./ui/ConfirmDialog";
import { TextInputDialog } from "./ui/TextInputDialog";
import type { SurfacePlacement } from "./ui/AnchoredSurface";
import { Menu, MenuItem, MenuSeparator } from "./ui/Menu";

interface Props {
  task: Task;
  detail?: TaskDetail;
  className?: string;
  placement?: SurfacePlacement;
  onChanged?: () => void;
}

/** 项目树、全局列表和任务页共用的一套会话生命周期菜单。 */
export function TaskActionsMenu({ task, detail, className, placement = "down", onChanged }: Props) {
  const refreshTasks = useTasksStore((state) => state.refreshTasks);
  const currentTaskId = useAppStore((state) => state.currentTaskId);
  const openConversations = useAppStore((state) => state.openConversations);
  const forgetTaskNavigation = useAppStore((state) => state.forgetTaskNavigation);
  const refreshDetail = useTasksStore((state) => state.refreshDetail);
  const [busy, setBusy] = useState<"rename" | "archive" | "restore" | "delete" | null>(null);
  const [renameOpen, setRenameOpen] = useState(false);
  const [renameValue, setRenameValue] = useState("");
  const [renameError, setRenameError] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const live = isTaskLive(task, detail);
  const title = taskTitle(task);
  const nextTitle = renameValue.trim();
  const renameDisabled = !nextTitle || nextTitle === task.title.trim();

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

  const rename = useCallback(async () => {
    if (busy || task.state === "archived") return;
    const next = renameValue.trim();
    if (!next) {
      setRenameError("请输入新的会话名称");
      return;
    }
    setBusy("rename");
    setRenameError(null);
    try {
      const renamed = await taskRename(task.id, next);
      await Promise.all([refreshTasks(), refreshDetail(task.id)]);
      setRenameOpen(false);
      onChanged?.();
      pushToast({ kind: "success", title: "对话已重命名", body: renamed.title });
    } catch (cause) {
      const message = String(cause);
      setRenameError(message);
      pushToast({ kind: "error", title: "无法重命名对话", body: message });
    } finally {
      setBusy(null);
    }
  }, [busy, onChanged, refreshDetail, refreshTasks, renameValue, task.id, task.state]);

  const remove = useCallback(async () => {
    if (busy || live) return;
    setBusy("delete");
    try {
      await taskDelete(task.id);
      await refreshTasks();
      setConfirmDelete(false);
      leaveRemovedRoom();
      forgetTaskNavigation(task.id);
      onChanged?.();
      pushToast({ kind: "success", title: "对话已永久删除", body: title });
    } catch (cause) {
      pushToast({ kind: "error", title: "无法删除对话", body: String(cause) });
    } finally {
      setBusy(null);
    }
  }, [busy, forgetTaskNavigation, leaveRemovedRoom, live, onChanged, refreshTasks, task.id, title]);

  const restore = useCallback(async () => {
    if (busy || task.state !== "archived") return;
    setBusy("restore");
    try {
      await taskRestore(task.id);
      await Promise.all([refreshTasks(), refreshDetail(task.id)]);
      onChanged?.();
      pushToast({ kind: "success", title: "对话已还原", body: title });
    } catch (cause) {
      pushToast({ kind: "error", title: "无法还原对话", body: String(cause) });
    } finally {
      setBusy(null);
    }
  }, [busy, onChanged, refreshDetail, refreshTasks, task.id, task.state, title]);

  return (
    <>
      <Menu
        className={className}
        placement={placement}
        gap={placement === "left" || placement === "right" ? 18 : undefined}
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
            {task.state === "archived" ? (
              <MenuItem close={close} onSelect={() => void restore()}>
                <IconRestore width={15} height={15} />
                还原对话
              </MenuItem>
            ) : (
              <>
                <MenuItem
                  close={close}
                  onSelect={() => {
                    setRenameValue(task.title.trim() || title);
                    setRenameError(null);
                    setRenameOpen(true);
                  }}
                >
                  <IconEdit width={15} height={15} />
                  重命名对话…
                </MenuItem>
                <MenuItem disabled={live} close={close} onSelect={() => void archive()}>
                  <IconArchive width={15} height={15} />
                  归档对话
                </MenuItem>
              </>
            )}
            <MenuSeparator />
            <MenuItem
              className="danger"
              disabled={live}
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
      <TextInputDialog
        open={renameOpen}
        title="重命名对话"
        description="只修改列表中显示的名称，不会改变消息、运行记录或项目文件。"
        label="会话名称"
        value={renameValue}
        maxLength={96}
        confirmLabel="保存名称"
        busy={busy === "rename"}
        error={renameError}
        confirmDisabled={renameDisabled}
        onChange={(value) => {
          setRenameValue(value);
          setRenameError(null);
        }}
        onCancel={() => {
          if (busy !== "rename") {
            setRenameOpen(false);
            setRenameError(null);
          }
        }}
        onConfirm={() => void rename()}
      />
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
