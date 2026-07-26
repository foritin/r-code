import { create } from "zustand";
import * as ipc from "../lib/ipc";
import type { PermissionRequest, Task, TaskDetail, Workspace } from "../lib/types";

/**
 * 任务/工作区数据缓存 + 轮询驱动。
 * Deck / Rail / Home glance 的派生数据都从这里出。
 *
 * 注意：后端暂无聚合查询，needs-you 等靠逐任务 detail 派生；
 * 任务多时由调用方控制刷新范围（仅可见场景轮询）。
 */

/** Deck / Inbox 共用的「待决项」：一条待批权限或一个 review_ready 任务。 */
export interface NeedsYouItem {
  kind: "permission" | "review_ready";
  task: Task;
  permission?: PermissionRequest;
  /** 等待时长起点（created_at） */
  since: string;
}

interface TasksState {
  tasks: Task[];
  /** taskId → TaskDetail（LRU 语义靠调用方控制刷新） */
  details: Record<string, TaskDetail>;
  workspaces: Workspace[];
  /** 当前准备附加到新会话的工作区路径；null 即纯聊天。 */
  currentProjectId: string | null;
  /** 上次全量刷新时间（用于"今日"派生） */
  refreshedAt: number;

  refreshTasks: () => Promise<void>;
  refreshWorkspaces: () => Promise<void>;
  refreshDetail: (taskId: string) => Promise<void>;
  /** 批量刷新一组任务的 detail（Deck 聚合用，并发限 4） */
  refreshDetails: (taskIds: string[]) => Promise<void>;
  setCurrentProject: (projectId: string | null) => void;
}

export const useTasksStore = create<TasksState>((set, get) => ({
  tasks: [],
  details: {},
  workspaces: [],
  currentProjectId: null,
  refreshedAt: 0,

  refreshTasks: async () => {
    const tasks = await ipc.taskList(undefined, false);
    set({ tasks, refreshedAt: Date.now() });
  },

  refreshWorkspaces: async () => {
    const workspaces = await ipc.workspaceList();
    set((s) => ({
      workspaces,
      // 不能因为存在最近项目就自动把它附加到新对话：默认应是纯聊天。
      currentProjectId: workspaces.some((w) => w.canonical_path === s.currentProjectId)
        ? s.currentProjectId
        : null,
    }));
  },

  refreshDetail: async (taskId) => {
    const detail = await ipc.taskDetail(taskId);
    set((s) => ({ details: { ...s.details, [taskId]: detail } }));
  },

  refreshDetails: async (taskIds) => {
    const queue = [...new Set(taskIds)];
    const results: Record<string, TaskDetail> = {};
    const worker = async () => {
      for (;;) {
        const id = queue.shift();
        if (!id) return;
        try {
          results[id] = await ipc.taskDetail(id);
        } catch {
          /* 单任务失败不阻塞整批 */
        }
      }
    };
    await Promise.all(Array.from({ length: Math.min(4, queue.length) }, worker));
    set((s) => ({ details: { ...s.details, ...results } }));
  },

  setCurrentProject: (currentProjectId) => set({ currentProjectId }),
}));

// ---------- 派生选择器 ----------

export const selectRunning = (s: TasksState): Task[] =>
  s.tasks.filter(
    (t) =>
      t.state === "in_progress" ||
      t.state === "exploring" ||
      s.details[t.id]?.runs.some((run) => run.ended_at == null) === true
  );

export const selectReviewReady = (s: TasksState): Task[] =>
  s.tasks.filter((t) => t.state === "review_ready");

/** 待批权限（依赖 details 已加载）。 */
export const selectPendingPermissions = (s: TasksState): NeedsYouItem[] => {
  const items: NeedsYouItem[] = [];
  for (const t of s.tasks) {
    const d = s.details[t.id];
    if (!d) continue;
    for (const p of d.permissions) {
      if (p.decision === "pending") {
        items.push({ kind: "permission", task: t, permission: p, since: p.created_at });
      }
    }
  }
  return items;
};

/** Needs You 全集 = 待批权限 + review_ready 任务。 */
export const selectNeedsYou = (s: TasksState): NeedsYouItem[] => {
  const perms = selectPendingPermissions(s);
  const reviews = selectReviewReady(s).map((t) => ({
    kind: "review_ready" as const,
    task: t,
    since: t.updated_at,
  }));
  return [...perms, ...reviews].sort((a, b) => a.since.localeCompare(b.since));
};

/** 任务是否有待决项（rail 灯用）。 */
export const selectNeedsYouTaskIds = (s: TasksState): Set<string> => {
  const ids = new Set(selectPendingPermissions(s).map((i) => i.task.id));
  for (const t of selectReviewReady(s)) ids.add(t.id);
  return ids;
};
