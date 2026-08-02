import { create } from "zustand";
import * as ipc from "../lib/ipc";
import type {
  PermissionRequest,
  ProjectActivityPage,
  Task,
  TaskDetail,
  Workspace,
  WorkspaceDashboard,
} from "../lib/types";
import {
  browserMockDetails,
  browserMockTasks,
  browserMockWorkspaces,
  shouldUseBrowserMock,
} from "../lib/mock-data";

/**
 * 任务/工作区数据缓存 + 轮询驱动。
 * Deck / Rail / Home glance 的派生数据都从这里出。
 *
 * 项目仪表盘 / 活动流使用聚合 IPC；单任务细节仍保留在本缓存，供 Room、审核与
 * 侧栏即时交互使用。
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
  /** workspacePath → 服务端聚合仪表盘 */
  dashboards: Record<string, WorkspaceDashboard>;
  /** workspacePath → 服务端项目活动流第一页 */
  projectActivities: Record<string, ProjectActivityPage>;
  /** 跨项目活动流第一页 */
  activityPage: ProjectActivityPage | null;
  /** 当前准备附加到新会话的工作区路径；null 即纯聊天。 */
  currentProjectId: string | null;
  /** 上次全量刷新时间（用于"今日"派生） */
  refreshedAt: number;

  refreshTasks: () => Promise<void>;
  refreshWorkspaces: () => Promise<void>;
  refreshDetail: (taskId: string) => Promise<void>;
  /** 批量刷新一组任务的 detail（Deck 聚合用，并发限 4） */
  refreshDetails: (taskIds: string[]) => Promise<void>;
  refreshDashboard: (workspacePath: string) => Promise<void>;
  refreshProjectActivity: (workspacePath: string) => Promise<void>;
  refreshActivity: () => Promise<void>;
  setCurrentProject: (projectId: string | null) => void;
}

/**
 * Tauri IPC payloads are plain JSON values.  Polling returns a fresh object graph even when
 * nothing changed, so assigning every response used to wake every dependent React subtree.
 * Cache the serialization of retained graphs and preserve their references on equal payloads.
 */
const payloadSignatures = new WeakMap<object, string>();

function payloadSignature(value: object): string {
  const cached = payloadSignatures.get(value);
  if (cached !== undefined) return cached;
  const signature = JSON.stringify(value);
  payloadSignatures.set(value, signature);
  return signature;
}

function samePayload<T>(left: T, right: T): boolean {
  if (Object.is(left, right)) return true;
  if (left == null || right == null || typeof left !== "object" || typeof right !== "object") {
    return false;
  }
  return payloadSignature(left) === payloadSignature(right);
}

function mergeChangedDetails(
  current: Record<string, TaskDetail>,
  incoming: Record<string, TaskDetail>,
): Record<string, TaskDetail> {
  let next = current;
  for (const [taskId, detail] of Object.entries(incoming)) {
    if (samePayload(current[taskId], detail)) continue;
    if (next === current) next = { ...current };
    next[taskId] = detail;
  }
  return next;
}

const detailRequests = new Map<string, Promise<TaskDetail>>();

async function loadTaskDetail(taskId: string): Promise<TaskDetail> {
  try {
    return await ipc.taskDetail(taskId);
  } catch (error) {
    if (!shouldUseBrowserMock() || !browserMockDetails[taskId]) throw error;
    return browserMockDetails[taskId];
  }
}

function requestTaskDetail(taskId: string): Promise<TaskDetail> {
  const pending = detailRequests.get(taskId);
  if (pending) return pending;

  const request = loadTaskDetail(taskId).finally(() => {
    if (detailRequests.get(taskId) === request) detailRequests.delete(taskId);
  });
  detailRequests.set(taskId, request);
  return request;
}

export const useTasksStore = create<TasksState>((set, get) => ({
  tasks: [],
  details: {},
  workspaces: [],
  dashboards: {},
  projectActivities: {},
  activityPage: null,
  currentProjectId: null,
  refreshedAt: 0,

  refreshTasks: async () => {
    let tasks: Task[];
    let fallbackDetails: Record<string, TaskDetail> | null = null;
    try {
      tasks = await ipc.taskList(undefined, false);
    } catch (error) {
      if (!shouldUseBrowserMock()) throw error;
      tasks = browserMockTasks;
      fallbackDetails = browserMockDetails;
    }
    set((s) => {
      const nextTasks = samePayload(s.tasks, tasks) ? s.tasks : tasks;
      const nextDetails = fallbackDetails
        ? mergeChangedDetails(s.details, fallbackDetails)
        : s.details;
      if (nextTasks === s.tasks && nextDetails === s.details) return s;
      return { tasks: nextTasks, details: nextDetails, refreshedAt: Date.now() };
    });
  },

  refreshWorkspaces: async () => {
    let workspaces: Workspace[];
    try {
      workspaces = await ipc.workspaceList();
    } catch (error) {
      if (!shouldUseBrowserMock()) throw error;
      workspaces = browserMockWorkspaces;
    }
    set((s) => {
      // 不能因为存在最近项目就自动把它附加到新对话：默认应是纯聊天。
      const currentProjectId = workspaces.some((w) => w.canonical_path === s.currentProjectId)
        ? s.currentProjectId
        : null;
      const nextWorkspaces = samePayload(s.workspaces, workspaces) ? s.workspaces : workspaces;
      if (nextWorkspaces === s.workspaces && currentProjectId === s.currentProjectId) return s;
      return { workspaces: nextWorkspaces, currentProjectId };
    });
  },

  refreshDetail: async (taskId) => {
    const detail = await requestTaskDetail(taskId);
    set((s) => {
      if (samePayload(s.details[taskId], detail)) return s;
      return { details: { ...s.details, [taskId]: detail } };
    });
  },

  refreshDetails: async (taskIds) => {
    const ids = [...new Set(taskIds)].filter(Boolean);
    if (!ids.length) return;
    try {
      const batch = await ipc.taskDetailBatch(ids);
      const details = Object.fromEntries(batch.details.map((detail) => [detail.task.id, detail]));
      set((s) => {
        const nextDetails = mergeChangedDetails(s.details, details);
        return nextDetails === s.details ? s : { details: nextDetails };
      });
    } catch {
      // 批量接口在旧桌面端不可用时，保留逐项降级，避免一次升级影响现有会话。
      const results: Record<string, TaskDetail> = {};
      for (const id of ids) {
        try {
          results[id] = await requestTaskDetail(id);
        } catch {
          if (shouldUseBrowserMock() && browserMockDetails[id]) results[id] = browserMockDetails[id];
        }
      }
      if (Object.keys(results).length) {
        set((s) => {
          const nextDetails = mergeChangedDetails(s.details, results);
          return nextDetails === s.details ? s : { details: nextDetails };
        });
      }
    }
  },

  refreshDashboard: async (workspacePath) => {
    const dashboard = await ipc.workspaceDashboard(workspacePath);
    set((s) =>
      samePayload(s.dashboards[workspacePath], dashboard)
        ? s
        : { dashboards: { ...s.dashboards, [workspacePath]: dashboard } }
    );
  },

  refreshProjectActivity: async (workspacePath) => {
    const page = await ipc.projectActivityList(workspacePath);
    set((s) =>
      samePayload(s.projectActivities[workspacePath], page)
        ? s
        : { projectActivities: { ...s.projectActivities, [workspacePath]: page } }
    );
  },

  refreshActivity: async () => {
    const activityPage = await ipc.activityList();
    set((s) => (samePayload(s.activityPage, activityPage) ? s : { activityPage }));
  },

  setCurrentProject: (currentProjectId) =>
    set((s) => (s.currentProjectId === currentProjectId ? s : { currentProjectId })),
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
