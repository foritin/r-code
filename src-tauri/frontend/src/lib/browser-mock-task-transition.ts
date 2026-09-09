import type {
  AgentRun,
  PermissionRequest,
  QueuedMessage,
  Task,
  TaskAttention,
  TaskDetail,
  TaskDisplayState,
  VerificationRecord,
} from "./types";

/**
 * Browser regression tests mutate the in-memory task graph directly. Keep those fixtures on the
 * same status contract as the desktop host instead of leaving `task`, observations, and the
 * authoritative `status` projection out of sync.
 */
export interface MockTaskTransition {
  task?: Partial<Task>;
  runs?: AgentRun[];
  permissions?: PermissionRequest[];
  queued_messages?: QueuedMessage[];
  displayState?: TaskDisplayState;
}

export interface MockTaskStoreSlice {
  tasks: Task[];
  details: Record<string, TaskDetail>;
}

function newestByStartedAt<T extends { started_at: string }>(items: T[]): T | undefined {
  return items.reduce<T | undefined>((newest, item) => (
    !newest || item.started_at > newest.started_at ? item : newest
  ), undefined);
}

function relevantVerification(
  verifications: VerificationRecord[],
  latestMainRun: AgentRun | undefined,
): VerificationRecord | undefined {
  const latest = newestByStartedAt(verifications);
  if (!latest || !latestMainRun) return latest;
  return latest.run_id === latestMainRun.id || latest.started_at >= latestMainRun.started_at
    ? latest
    : undefined;
}

function projectMockStatus(detail: TaskDetail, displayState?: TaskDisplayState): TaskDetail["status"] {
  const { task, runs, permissions, queued_messages: queuedMessages } = detail;
  const pendingApproval = permissions.some((request) => request.decision === "pending");
  const latestMainRun = newestByStartedAt(runs.filter((run) => run.agent_kind === "main"));
  const activeMainRun = newestByStartedAt(
    runs.filter((run) => run.agent_kind === "main" && run.ended_at == null),
  );
  const verification = relevantVerification(detail.verifications, latestMainRun);
  const runFailed = latestMainRun?.review_state === "failed"
    || queuedMessages.some((message) => message.state === "failed")
    || verification?.status === "failed"
    || verification?.status === "timeout";
  const verificationRequired = verification?.status === "superseded"
    || verification?.status === "stale";
  const verifying = verification?.status === "running";
  const running = task.state === "exploring"
    || task.state === "in_progress"
    || runs.some((run) => run.ended_at == null);
  const queueDepth = queuedMessages.filter(
    (message) => message.state === "queued" || message.state === "dispatching",
  ).length;

  // These observations are supplied separately to the host projector and are not represented by
  // a TaskDetail collection, so preserve them while recomputing every signal that is represented.
  const pendingQuestion = detail.status.attention.includes("user_question");
  const workspaceBindingInvalid = detail.status.attention.includes("workspace_binding_invalid");

  const attention: TaskAttention[] = [];
  if (pendingApproval) attention.push("approval_required");
  if (pendingQuestion) attention.push("user_question");
  if (workspaceBindingInvalid) attention.push("workspace_binding_invalid");
  if (runFailed) attention.push("run_failed");
  if (verificationRequired) attention.push("verification_required");
  if (task.state === "review_ready") attention.push("review_required");

  const derivedDisplayState: TaskDisplayState = task.state === "archived"
    ? "archived"
    : pendingApproval
      ? "waiting_for_approval"
      : pendingQuestion
        ? "waiting_for_question"
        : runFailed
          ? "failed"
          : task.state === "interrupted"
            ? "interrupted"
            : workspaceBindingInvalid
              ? "workspace_binding_invalid"
              : task.state === "review_ready"
                ? "review_ready"
                : verificationRequired
                  ? "verification_required"
                  : verifying
                    ? "verifying"
                    : running
                      ? "running"
                      : queueDepth > 0
                        ? "queued"
                        : "idle";

  return {
    ...detail.status,
    task_id: task.id,
    persisted_state: task.state,
    display_state: displayState ?? derivedDisplayState,
    attention,
    active_run_id: activeMainRun?.id ?? null,
    queue_depth: queueDepth,
  };
}

export function transitionMockTaskDetail(
  detail: TaskDetail,
  transition: MockTaskTransition,
): TaskDetail {
  const next: TaskDetail = {
    ...detail,
    task: { ...detail.task, ...transition.task },
    runs: transition.runs ?? detail.runs,
    permissions: transition.permissions ?? detail.permissions,
    queued_messages: transition.queued_messages ?? detail.queued_messages,
  };
  return { ...next, status: projectMockStatus(next, transition.displayState) };
}

export function transitionMockTaskStore(
  state: MockTaskStoreSlice,
  taskId: string,
  transition: MockTaskTransition,
): MockTaskStoreSlice {
  const detail = state.details[taskId];
  if (!detail) throw new Error(`Cannot transition missing mock task detail: ${taskId}`);
  const nextDetail = transitionMockTaskDetail(detail, transition);
  return {
    tasks: state.tasks.map((task) => task.id === taskId ? nextDetail.task : task),
    details: { ...state.details, [taskId]: nextDetail },
  };
}

/** Apply a transition to the mutable browser-backend collections that sit behind IPC. */
export function applyMockTaskTransition(
  state: MockTaskStoreSlice,
  taskId: string,
  transition: MockTaskTransition,
): TaskDetail {
  const next = transitionMockTaskStore(state, taskId, transition);
  const taskIndex = state.tasks.findIndex((task) => task.id === taskId);
  if (taskIndex < 0) throw new Error(`Cannot transition missing mock task: ${taskId}`);
  state.tasks[taskIndex] = next.tasks[taskIndex];
  state.details[taskId] = next.details[taskId];
  return next.details[taskId];
}
