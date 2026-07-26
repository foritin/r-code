/**
 * 运行活动轨迹 —— 仅聚合用户可观察的协议事件，不存储或推断模型私有推理。
 * 该 reducer 不读取时间、不访问 React，调用方须把时间戳随 action 传入。
 */
import type {
  AgentRun,
  AgentActivityPhase,
  AgentEvent,
  AgentEventScope,
  AgentSendMode,
  PermissionRequest,
  QueuedMessage,
} from "../../lib/types";

export type ActivityPhase = AgentActivityPhase | "idle";
export type ActivityKind = "tool" | "guide" | "permission" | "queue";
export type SubagentStatus =
  | "queued"
  | "running"
  | "waiting_permission"
  | "completed"
  | "failed"
  | "cancelled";

export interface ActivityTraceItem {
  id: number;
  at: number;
  kind: ActivityKind;
  label: string;
}

export interface ActivityQueueSummary {
  queued: number;
  dispatching: number;
  failed: number;
}

/** 一个子代理的可观察状态；不保存子模型的私有推理文本。 */
export interface ActivitySubagent {
  id: string;
  label: string;
  status: SubagentStatus;
  phase: ActivityPhase;
  detail: string | null;
  startedAt: number;
  endedAt: number | null;
}

export interface ActivityTraceState {
  /** 当前可观察运行阶段；idle 表示没有活跃运行。 */
  phase: ActivityPhase;
  /** 面向用户的阶段文案，绝不承载或模拟推理过程。 */
  label: string;
  /** 当前运行的本地开始时间戳（毫秒）。 */
  startedAt: number | null;
  /** 最近的工具、引导、权限和队列活动。 */
  recentActivities: ActivityTraceItem[];
  /** 最近一条 AgentEvent 的本地接收时间戳（毫秒）。 */
  lastEventAt: number | null;
  /** 来自 AgentRun 快照的运行状态。 */
  running: boolean;
  /** 来自队列快照的可见状态汇总。 */
  queue: ActivityQueueSummary;
  /** 来自权限快照的待批准数量。 */
  pendingPermissions: number;
  /** 当前/最近的子代理工作项，刷新后由 AgentRun 运行树恢复。 */
  subagents: ActivitySubagent[];
  nextActivityId: number;
}

export type ActivityTraceAction =
  | { type: "reset" }
  | { type: "event"; event: AgentEvent; at: number }
  | { type: "sent"; mode: AgentSendMode; at: number }
  | {
      type: "snapshot";
      running: boolean;
      runs: readonly AgentRun[];
      queuedMessages: readonly QueuedMessage[];
      pendingPermissions: readonly PermissionRequest[];
      at: number;
    };

export function createActivityTraceState(): ActivityTraceState {
  return {
    phase: "idle",
    label: "空闲",
    startedAt: null,
    recentActivities: [],
    lastEventAt: null,
    running: false,
    queue: { queued: 0, dispatching: 0, failed: 0 },
    pendingPermissions: 0,
    subagents: [],
    nextActivityId: 1,
  };
}

/**
 * 根据流式事件和后端快照维护可观察活动。
 * 注意：此处不接收、保存或生成任何“思考”内容；等待模型时固定文案为“等待模型响应”。
 */
export function activityTraceReducer(
  state: ActivityTraceState,
  action: ActivityTraceAction
): ActivityTraceState {
  switch (action.type) {
    case "reset":
      return createActivityTraceState();
    case "sent":
      return applySend(state, action.mode, action.at);
    case "event":
      return applyEvent(state, action.event, action.at);
    case "snapshot":
      return applySnapshot(state, action);
  }
}

function applySend(state: ActivityTraceState, mode: AgentSendMode, at: number): ActivityTraceState {
  const startedAt = state.startedAt ?? at;
  switch (mode) {
    case "steer":
      return appendActivity(
        {
          ...state,
          running: true,
          phase: "steer_accepted",
          label: "已接纳，等待当前步骤完成",
          startedAt,
        },
        "guide",
        "引导已接纳",
        at
      );
    case "queue":
      return appendActivity(state, "queue", "消息已加入队列", at);
    case "send_now":
      return appendActivity(
        {
          ...state,
          running: true,
          phase: "requesting",
          label: "正在切换到新请求",
          startedAt,
        },
        "queue",
        "已请求立即发送",
        at
      );
    case "auto":
      return {
        ...state,
        running: true,
        phase: "requesting",
        label: "等待模型响应",
        startedAt,
      };
  }
}

function applyEvent(state: ActivityTraceState, event: AgentEvent, at: number): ActivityTraceState {
  const base: ActivityTraceState = {
    ...state,
    running: true,
    startedAt: state.startedAt ?? at,
    lastEventAt: at,
  };

  switch (event.type) {
    case "scoped":
      return applyScopedEvent(state, event.scope, event.event, at);
    case "activity":
      return applyActivityEvent(base, event.phase, event.detail, at);
    case "message":
      return { ...base, phase: "streaming", label: "正在生成回复" };
    case "tool_call": {
      const tool = observableToolName(event.name);
      return appendActivity(
        { ...base, phase: "tool", label: `正在使用工具：${tool}` },
        "tool",
        `调用工具：${tool}`,
        at
      );
    }
    case "tool_result":
      return appendActivity(
        { ...base, phase: "tool", label: event.is_error ? "工具执行失败" : "工具已完成" },
        "tool",
        event.is_error ? "工具执行失败" : "工具已完成",
        at
      );
    case "plan":
      return { ...base, phase: "finalizing", label: "正在更新可见计划" };
    case "subagent_lifecycle":
      // 生命周期事件必须带 scoped 包装才会影响某一张子代理卡；裸事件只保留为审计兼容。
      return base;
    case "state":
      return base;
  }
}

function applyScopedEvent(
  state: ActivityTraceState,
  scope: AgentEventScope,
  event: AgentEvent,
  at: number
): ActivityTraceState {
  if (scope.agent_kind !== "subagent") {
    return applyEvent(state, event, at);
  }
  if (event.type === "scoped") {
    return applyScopedEvent(state, event.scope, event.event, at);
  }

  const prior = state.subagents.find((child) => child.id === scope.run_id);
  const child: ActivitySubagent = {
    id: scope.run_id,
    label: scope.agent_label?.trim() || prior?.label || "子代理",
    status: prior?.status ?? "queued",
    phase: prior?.phase ?? "requesting",
    detail: prior?.detail ?? null,
    startedAt: prior?.startedAt ?? at,
    endedAt: prior?.endedAt ?? null,
  };

  switch (event.type) {
    case "subagent_lifecycle":
      child.status = event.state;
      child.detail = safeChildDetail(event.detail);
      child.endedAt = isTerminalChildStatus(event.state) ? at : null;
      child.phase = childPhaseForStatus(event.state, child.phase);
      break;
    case "activity":
      child.phase = event.phase;
      child.detail = event.phase === "tool" ? safeChildDetail(event.detail) : null;
      if (event.phase === "waiting_permission") child.status = "waiting_permission";
      else if (!isTerminalChildStatus(child.status)) child.status = "running";
      break;
    case "tool_call":
      child.phase = "tool";
      child.status = "running";
      child.detail = `正在使用工具：${observableToolName(event.name)}`;
      break;
    case "tool_result":
      child.phase = "tool";
      child.status = "running";
      child.detail = event.is_error ? "工具执行失败" : "工具已完成";
      break;
    case "message":
      child.phase = "streaming";
      child.status = "running";
      child.detail = "正在生成可见结果";
      break;
    case "plan":
      child.phase = "finalizing";
      child.status = "running";
      child.detail = "正在整理结果";
      break;
    case "state":
      if (event.state === "interrupted") {
        child.status = "cancelled";
        child.endedAt = at;
      } else if (event.state === "review_ready") {
        child.status = "completed";
        child.endedAt = at;
      }
      break;
  }

  const subagents = [
    ...state.subagents.filter((item) => item.id !== child.id),
    child,
  ]
    .sort((a, b) => a.startedAt - b.startedAt || a.id.localeCompare(b.id));
  return {
    ...state,
    running: true,
    lastEventAt: at,
    subagents,
  };
}

function childPhaseForStatus(status: SubagentStatus, current: ActivityPhase): ActivityPhase {
  switch (status) {
    case "queued":
      return "requesting";
    case "running":
      return current === "idle" ? "requesting" : current;
    case "waiting_permission":
      return "waiting_permission";
    case "completed":
    case "failed":
    case "cancelled":
      return "idle";
  }
}

function isTerminalChildStatus(status: SubagentStatus): boolean {
  return status === "completed" || status === "failed" || status === "cancelled";
}

function safeChildDetail(value: string | undefined): string | null {
  if (!value) return null;
  return value.trim().replace(/\s+/g, " ").slice(0, 120) || null;
}

function applyActivityEvent(
  state: ActivityTraceState,
  phase: AgentActivityPhase,
  detail: string | undefined,
  at: number
): ActivityTraceState {
  const next = { ...state, phase, label: activityLabel(phase, detail) };
  switch (phase) {
    case "tool":
      return appendActivity(next, "tool", detail ? `使用工具：${observableToolDetail(detail)}` : "正在使用工具", at);
    case "waiting_permission":
      return appendActivity(next, "permission", "等待权限批准", at);
    case "steer_accepted":
      return appendActivity(next, "guide", "引导已接纳", at);
    case "steer_applied":
      return appendActivity(next, "guide", "引导已纳入下一次请求", at);
    default:
      return next;
  }
}

function applySnapshot(
  state: ActivityTraceState,
  action: Extract<ActivityTraceAction, { type: "snapshot" }>
): ActivityTraceState {
  const queue = summarizeQueue(action.queuedMessages);
  const pendingPermissions = action.pendingPermissions.length;

  if (!action.running) {
    return {
      ...state,
      running: false,
      phase: "idle",
      label: idleLabel(queue),
      startedAt: null,
      queue,
      pendingPermissions,
      subagents: mergePersistedSubagents(state.subagents, action.runs),
    };
  }

  const startedAt = state.running && state.startedAt != null ? state.startedAt : action.at;
  const base: ActivityTraceState = {
    ...state,
    running: true,
    startedAt,
    queue,
    pendingPermissions,
    subagents: mergePersistedSubagents(state.subagents, action.runs),
  };

  if (pendingPermissions > 0) {
    return {
      ...base,
      phase: "waiting_permission",
      label: pendingPermissions === 1 ? "等待权限批准" : `等待 ${pendingPermissions} 项权限批准`,
    };
  }

  if (state.phase === "idle" || state.phase === "waiting_permission") {
    return { ...base, phase: "requesting", label: "等待模型响应" };
  }

  return base;
}

function mergePersistedSubagents(
  current: readonly ActivitySubagent[],
  runs: readonly AgentRun[]
): ActivitySubagent[] {
  const live = new Map(current.map((item) => [item.id, item]));
  for (const run of runs) {
    if (run.agent_kind !== "subagent") continue;
    const startedAt = parseTimestamp(run.started_at);
    const endedAt = run.ended_at ? parseTimestamp(run.ended_at) : null;
    const prior = live.get(run.id);
    live.set(run.id, {
      id: run.id,
      label: run.agent_label?.trim() || "子代理",
      status: persistedSubagentStatus(run),
      phase: prior?.phase ?? (run.ended_at ? "idle" : "requesting"),
      detail: safeChildDetail(run.summary ?? undefined) ?? prior?.detail ?? null,
      startedAt,
      endedAt,
    });
  }
  return [...live.values()]
    .sort((a, b) => a.startedAt - b.startedAt || a.id.localeCompare(b.id));
}

function persistedSubagentStatus(run: AgentRun): SubagentStatus {
  if (run.ended_at == null) return "running";
  if (run.review_state === "failed") return "failed";
  if (run.review_state === "aborted") return "cancelled";
  return "completed";
}

function parseTimestamp(value: string): number {
  const parsed = Date.parse(value);
  return Number.isNaN(parsed) ? Date.now() : parsed;
}

function activityLabel(phase: AgentActivityPhase, detail?: string): string {
  switch (phase) {
    case "requesting":
      return "等待模型响应";
    case "streaming":
      return "正在生成回复";
    case "tool":
      return detail ? `正在使用工具：${observableToolDetail(detail)}` : "正在使用工具";
    case "waiting_permission":
      return "等待权限批准";
    case "steer_accepted":
      return "已接纳，等待当前步骤完成";
    case "steer_applied":
      return "已纳入下一次请求";
    case "finalizing":
      return "正在整理结果";
  }
}

function appendActivity(
  state: ActivityTraceState,
  kind: ActivityKind,
  label: string,
  at: number
): ActivityTraceState {
  const previous = state.recentActivities[state.recentActivities.length - 1];
  if (previous?.kind === kind && previous.label === label) return state;

  return {
    ...state,
    recentActivities: [
      ...state.recentActivities,
      { id: state.nextActivityId, at, kind, label },
    ].slice(-4),
    nextActivityId: state.nextActivityId + 1,
  };
}

function summarizeQueue(messages: readonly QueuedMessage[]): ActivityQueueSummary {
  return messages.reduce<ActivityQueueSummary>(
    (summary, message) => {
      if (message.state === "queued") summary.queued += 1;
      if (message.state === "dispatching") summary.dispatching += 1;
      if (message.state === "failed") summary.failed += 1;
      return summary;
    },
    { queued: 0, dispatching: 0, failed: 0 }
  );
}

function idleLabel(queue: ActivityQueueSummary): string {
  if (queue.dispatching > 0) return "正在发送排队消息";
  if (queue.failed > 0) return "有消息发送失败";
  if (queue.queued > 0) return `有 ${queue.queued} 条消息待发送`;
  return "空闲";
}

function observableToolName(name: string): string {
  const normalized = name.trim().replace(/\s+/g, " ");
  return normalized ? normalized.slice(0, 60) : "工具";
}

/**
 * activity.detail 只有在 tool 阶段可见，且仅作为后端已分类的可观察工具说明。
 * 其他阶段完全忽略 detail，避免把任何内部提示或推理带入界面。
 */
function observableToolDetail(detail: string): string {
  const normalized = detail.trim().replace(/\s+/g, " ");
  return normalized ? normalized.slice(0, 80) : "工具";
}
