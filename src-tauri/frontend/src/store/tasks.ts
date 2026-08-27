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
  kind: "permission" | "review_ready" | "plan_entry_offer";
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
  /** Insert a just-created task immediately instead of waiting for the next sidebar poll. */
  upsertTask: (task: Task) => void;
  setCurrentProject: (projectId: string | null) => void;
}

/** Polling returns fresh JSON graphs. Compare compact product revisions rather than serializing
 * every nested payload (large event histories made that second serialization visible in the UI). */
const signatureCache = new WeakMap<object, string>();

function cachedSignature(value: object, build: () => string) {
  const known = signatureCache.get(value);
  if (known !== undefined) return known;
  const next = build();
  signatureCache.set(value, next);
  return next;
}

function taskStamp(task: Task) {
  return `${task.id}:${task.updated_at}:${task.state}:${task.workspace_path ?? ""}:${task.provider_name ?? ""}:${task.model ?? ""}:${task.agent_engine}:${task.mode}`;
}

function tasksSignature(tasks: Task[]) {
  return cachedSignature(tasks, () => tasks.map(taskStamp).join("\u001e"));
}

function workspacesSignature(workspaces: Workspace[]) {
  return cachedSignature(workspaces, () => workspaces.map((workspace) =>
    `${workspace.id}:${workspace.last_opened_at}:${workspace.canonical_path}:${workspace.display_name}:${workspace.access_mode}:${workspace.memory_mode}:${workspace.memory_generation}`
  ).join("\u001e"));
}

function detailSignature(detail: TaskDetail | undefined) {
  if (!detail) return "";
  return cachedSignature(detail, () => [
    taskStamp(detail.task),
    `status:${detail.status?.display_state ?? "legacy"}:${detail.status?.attention.join(",") ?? ""}:${detail.status?.active_run_id ?? ""}:${detail.status?.queue_depth ?? 0}:${detail.status?.unread_count ?? 0}`,
    `active:${detail.active_branch.id}`,
    `branches:${detail.branches.map((item) => `${item.id}:${item.is_active}`).join(",")}`,
    `runs:${detail.runs.map((item) => `${item.id}:${item.started_at}:${item.ended_at ?? ""}:${item.review_state}:${item.summary ?? ""}`).join(",")}`,
    `events:${detail.events.map((item) => `${item.id}:${item.event_type}:${item.created_at}`).join(",")}`,
    `changes:${detail.changes.map((item) => `${item.id}:${item.change_type}:${item.path}:${item.before_hash ?? ""}:${item.after_hash ?? ""}`).join(",")}`,
    `permissions:${detail.permissions.map((item) => `${item.id}:${item.decision}:${item.decided_at ?? ""}`).join(",")}`,
    `verifications:${detail.verifications.map((item) => `${item.id}:${item.status}:${item.exit_code ?? ""}:${item.ended_at ?? ""}`).join(",")}`,
    `queue:${detail.queued_messages.map((item) => `${item.id}:${item.state}:${item.priority}:${item.updated_at}`).join(",")}`,
    // Plan 入口建议的待决决定必须参与签名，否则轮询永远观测不到新建议。
    `plan_offer:${detail.pending_plan_entry_offer
      ? `${detail.pending_plan_entry_offer.id}:${detail.pending_plan_entry_offer.revision}:${detail.pending_plan_entry_offer.state}`
      : ""}`,
  ].join("\u001f"));
}

function dashboardSignature(dashboard: WorkspaceDashboard | undefined) {
  if (!dashboard) return "";
  return cachedSignature(dashboard, () => [
    `${dashboard.workspace.id}:${dashboard.workspace.memory_generation}:${dashboard.workspace.access_mode}`,
    Object.values(dashboard.metrics).join(":"),
    dashboard.tasks.map((item) => `${taskStamp(item.task)}:${item.status?.display_state ?? "legacy"}:${item.status?.attention.join(",") ?? ""}:${item.status?.queue_depth ?? 0}:${item.status?.unread_count ?? 0}:${item.activity}:${item.pending_permission_count}:${item.active_run?.id ?? ""}:${item.active_run?.ended_at ?? ""}:${Object.values(item.change_summary).join(",")}:${item.latest_verification?.id ?? ""}:${item.latest_verification?.status ?? ""}`).join("\u001e"),
    dashboard.attention.map((item) => `${item.kind}:${item.task.id}:${item.permission?.id ?? ""}:${item.permission?.decision ?? ""}:${item.since}`).join("\u001e"),
    dashboard.archived.map(taskStamp).join("\u001e"),
  ].join("\u001f"));
}

function activitySignature(page: ProjectActivityPage | null | undefined) {
  if (!page) return "";
  return cachedSignature(page, () => `${page.next_cursor ?? ""}\u001f${page.items.map((item) => `${item.id}:${item.at}:${item.kind}:${item.task_id}`).join("\u001e")}`);
}

function mergeChangedDetails(
  current: Record<string, TaskDetail>,
  incoming: Record<string, TaskDetail>,
): Record<string, TaskDetail> {
  let next = current;
  for (const [taskId, detail] of Object.entries(incoming)) {
    if (detailSignature(current[taskId]) === detailSignature(detail)) continue;
    if (next === current) next = { ...current };
    next[taskId] = detail;
  }
  return next;
}

const detailRequests = new Map<string, Promise<TaskDetail>>();

interface TaskListResult {
  tasks: Task[];
  fallbackDetails: Record<string, TaskDetail> | null;
}

let tasksRequest: Promise<TaskListResult> | null = null;
let workspacesRequest: Promise<Workspace[]> | null = null;

async function loadTasks(): Promise<TaskListResult> {
  try {
    return { tasks: await ipc.taskList(undefined, false), fallbackDetails: null };
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    return { tasks: browserMockTasks, fallbackDetails: browserMockDetails };
  }
}

function requestTasks(): Promise<TaskListResult> {
  if (tasksRequest) return tasksRequest;
  const request = loadTasks().finally(() => {
    if (tasksRequest === request) tasksRequest = null;
  });
  tasksRequest = request;
  return request;
}

async function loadWorkspaces(): Promise<Workspace[]> {
  try {
    return await ipc.workspaceList();
  } catch (error) {
    if (!shouldUseBrowserMock()) throw error;
    return browserMockWorkspaces;
  }
}

function requestWorkspaces(): Promise<Workspace[]> {
  if (workspacesRequest) return workspacesRequest;
  const request = loadWorkspaces().finally(() => {
    if (workspacesRequest === request) workspacesRequest = null;
  });
  workspacesRequest = request;
  return request;
}

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
    const { tasks, fallbackDetails } = await requestTasks();
    set((s) => {
      const nextTasks = tasksSignature(s.tasks) === tasksSignature(tasks) ? s.tasks : tasks;
      const nextDetails = fallbackDetails
        ? mergeChangedDetails(s.details, fallbackDetails)
        : s.details;
      if (nextTasks === s.tasks && nextDetails === s.details) return s;
      return { tasks: nextTasks, details: nextDetails, refreshedAt: Date.now() };
    });
  },

  refreshWorkspaces: async () => {
    const workspaces = await requestWorkspaces();
    set((s) => {
      // 不能因为存在最近项目就自动把它附加到新对话：默认应是纯聊天。
      const currentProjectId = workspaces.some((w) => w.canonical_path === s.currentProjectId)
        ? s.currentProjectId
        : null;
      const nextWorkspaces = workspacesSignature(s.workspaces) === workspacesSignature(workspaces) ? s.workspaces : workspaces;
      if (nextWorkspaces === s.workspaces && currentProjectId === s.currentProjectId) return s;
      return { workspaces: nextWorkspaces, currentProjectId };
    });
  },

  refreshDetail: async (taskId) => {
    const detail = await requestTaskDetail(taskId);
    set((s) => {
      if (detailSignature(s.details[taskId]) === detailSignature(detail)) return s;
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
      dashboardSignature(s.dashboards[workspacePath]) === dashboardSignature(dashboard)
        ? s
        : { dashboards: { ...s.dashboards, [workspacePath]: dashboard } }
    );
  },

  refreshProjectActivity: async (workspacePath) => {
    const page = await ipc.projectActivityList(workspacePath);
    set((s) =>
      activitySignature(s.projectActivities[workspacePath]) === activitySignature(page)
        ? s
        : { projectActivities: { ...s.projectActivities, [workspacePath]: page } }
    );
  },

  refreshActivity: async () => {
    const activityPage = await ipc.activityList();
    set((s) => (activitySignature(s.activityPage) === activitySignature(activityPage) ? s : { activityPage }));
  },

  upsertTask: (task) => set((s) => {
    const existing = s.tasks.find((candidate) => candidate.id === task.id);
    const tasks = existing === task
      ? s.tasks
      : [task, ...s.tasks.filter((candidate) => candidate.id !== task.id)];
    const currentDetail = s.details[task.id];
    const details = currentDetail && currentDetail.task !== task
      ? { ...s.details, [task.id]: { ...currentDetail, task } }
      : s.details;
    return tasks === s.tasks && details === s.details ? s : { tasks, details };
  }),

  setCurrentProject: (currentProjectId) =>
    set((s) => (s.currentProjectId === currentProjectId ? s : { currentProjectId })),
}));

// ---------- 派生选择器 ----------

interface TaskDetailSelectorCache<T> {
  tasks: Task[] | null;
  details: TasksState["details"] | null;
  value: T;
}

let runningCache: TaskDetailSelectorCache<Task[]> = { tasks: null, details: null, value: [] };
let reviewReadyCache: TaskDetailSelectorCache<Task[]> = { tasks: null, details: null, value: [] };
let pendingPermissionsCache: TaskDetailSelectorCache<NeedsYouItem[]> = { tasks: null, details: null, value: [] };
let needsYouCache: {
  permissions: NeedsYouItem[] | null;
  planOffers?: number;
  reviewTasks: Task[] | null;
  value: NeedsYouItem[];
} = { permissions: null, reviewTasks: null, value: [] };
let needsYouTaskIdsCache: {
  permissions: NeedsYouItem[] | null;
  planOffers?: number;
  reviewTasks: Task[] | null;
  value: Set<string>;
} = { permissions: null, reviewTasks: null, value: new Set() };

export const selectRunning = (s: TasksState): Task[] => {
  if (runningCache.tasks === s.tasks && runningCache.details === s.details) {
    return runningCache.value;
  }
  const value = s.tasks.filter(
    (t) =>
      s.details[t.id]?.status?.display_state === "running" ||
      s.details[t.id]?.status?.display_state === "verifying" ||
      s.details[t.id]?.runs.some((run) => run.ended_at == null) === true
  );
  runningCache = { tasks: s.tasks, details: s.details, value };
  return value;
};

export const selectReviewReady = (s: TasksState): Task[] => {
  if (reviewReadyCache.tasks === s.tasks && reviewReadyCache.details === s.details) {
    return reviewReadyCache.value;
  }
  const value = s.tasks.filter((t) =>
    s.details[t.id]?.status?.display_state === "review_ready"
      || (!s.details[t.id] && t.state === "review_ready")
  );
  reviewReadyCache = { tasks: s.tasks, details: s.details, value };
  return value;
};

/** 待批权限（依赖 details 已加载）。 */
export const selectPendingPermissions = (s: TasksState): NeedsYouItem[] => {
  if (
    pendingPermissionsCache.tasks === s.tasks
    && pendingPermissionsCache.details === s.details
  ) {
    return pendingPermissionsCache.value;
  }
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
  pendingPermissionsCache = { tasks: s.tasks, details: s.details, value: items };
  return items;
};

/** Needs You 全集 = 待批权限 + review_ready 任务。 */
export const selectNeedsYou = (s: TasksState): NeedsYouItem[] => {
  const perms = selectPendingPermissions(s);
  const reviewTasks = selectReviewReady(s);
  // Plan 入口建议：非当前 task 只投影 Needs You，不能跨任务抢焦点（docs §6.5）。
  const planOffers: NeedsYouItem[] = [];
  for (const detail of Object.values(s.details)) {
    if (detail?.pending_plan_entry_offer) {
      planOffers.push({
        kind: "plan_entry_offer",
        task: detail.task,
        since: detail.task.updated_at,
      });
    }
  }
  if (
    needsYouCache.permissions === perms
    && needsYouCache.reviewTasks === reviewTasks
    && needsYouCache.planOffers === planOffers.length
  ) {
    return needsYouCache.value;
  }
  const reviews = reviewTasks.map((t) => ({
    kind: "review_ready" as const,
    task: t,
    since: t.updated_at,
  }));
  const value = [...perms, ...reviews, ...planOffers].sort((a, b) => a.since.localeCompare(b.since));
  needsYouCache = { permissions: perms, reviewTasks, planOffers: planOffers.length, value };
  return value;
};

/** 任务是否有待决项（rail 灯用）。 */
export const selectNeedsYouTaskIds = (s: TasksState): Set<string> => {
  const permissions = selectPendingPermissions(s);
  const reviewTasks = selectReviewReady(s);
  const planOfferIds = Object.values(s.details)
    .filter((detail) => detail?.pending_plan_entry_offer)
    .map((detail) => detail!.task.id);
  if (
    needsYouTaskIdsCache.permissions === permissions
    && needsYouTaskIdsCache.reviewTasks === reviewTasks
    && needsYouTaskIdsCache.planOffers === planOfferIds.length
  ) {
    return needsYouTaskIdsCache.value;
  }
  const ids = new Set(permissions.map((i) => i.task.id));
  for (const t of reviewTasks) ids.add(t.id);
  for (const id of planOfferIds) ids.add(id);
  needsYouTaskIdsCache = { permissions, reviewTasks, planOffers: planOfferIds.length, value: ids };
  return ids;
};
