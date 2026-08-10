import { useCallback, useState } from "react";
import { errText } from "../lib/format";
import {
  IpcCommandError,
  PROJECT_CONVERSATION_LIMIT_REACHED_CODE,
  projectConversationCreate,
  taskCreate,
  taskPrepare,
} from "../lib/ipc";
import type { Task } from "../lib/types";
import { useAppStore } from "../store/app";
import { useTasksStore } from "../store/tasks";
import { pushToast } from "../store/toast";

const pendingCreates = new Map<string, Promise<Task>>();

function creationKey(workspacePath: string | null): string {
  return workspacePath ?? "__standalone__";
}

async function persistConversation(workspacePath: string | null): Promise<Task> {
  const key = creationKey(workspacePath);
  const pending = pendingCreates.get(key);
  if (pending) return pending;

  const request = (workspacePath
    ? projectConversationCreate(workspacePath)
    : taskCreate(null, "新对话", "", "ask"))
    .finally(() => {
      if (pendingCreates.get(key) === request) pendingCreates.delete(key);
    });
  pendingCreates.set(key, request);
  return request;
}

/** Shared immediate-create path for every conversation plus button. Runtime preparation remains
 * background work so the durable sidebar row and focused composer appear as soon as SQLite returns. */
export function useCreateConversation() {
  const [creatingKey, setCreatingKey] = useState<string | null>(null);

  const createConversation = useCallback(async (workspacePath: string | null) => {
    const key = creationKey(workspacePath);
    if (creatingKey != null || pendingCreates.has(key)) return null;
    setCreatingKey(key);
    try {
      const task = await persistConversation(workspacePath);
      const tasks = useTasksStore.getState();
      tasks.upsertTask(task);
      tasks.setCurrentProject(workspacePath);
      useAppStore.getState().openRoom(task.id);
      void taskPrepare(task.id).catch(() => {
        pushToast({
          kind: "warn",
          title: "会话将在首次发送时完成准备",
          body: "对话已创建，可以直接输入消息。",
          timeout: 5000,
        });
      });
      return task;
    } catch (cause) {
      const message = errText(cause);
      if (
        cause instanceof IpcCommandError &&
        cause.code === PROJECT_CONVERSATION_LIMIT_REACHED_CODE
      ) {
        const limit = cause.limit ?? 5;
        pushToast({
          kind: "warn",
          title: `已达到 ${limit} 个对话上限`,
          body: "请先归档一个对话，再新建。",
          timeout: 5000,
        });
      } else {
        pushToast({ kind: "error", title: "无法创建新对话", body: message, timeout: 6000 });
      }
      return null;
    } finally {
      setCreatingKey(null);
    }
  }, [creatingKey]);

  return {
    createConversation,
    creating: creatingKey != null,
    isCreating: (workspacePath: string | null) => creatingKey === creationKey(workspacePath),
  };
}
