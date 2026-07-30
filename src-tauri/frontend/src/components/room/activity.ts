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
  SubagentAccessMode,
} from "../../lib/types";

export type ActivityPhase = AgentActivityPhase | "idle";
export type SubagentStatus =
  | "queued"
  | "running"
  | "waiting_permission"
  | "completed"
  | "failed"
  | "cancelled";

export interface ActivityQueueSummary {
  queued: number;
  dispatching: number;
  failed: number;
}

export type ActivitySubagentEventKind =
  | "lifecycle"
  | "activity"
  | "tool_call"
  | "tool_result"
  | "message"
  | "plan";

/** 子代理的公开动作记录；只保存后端已分类的信息和最终可见文本摘要。 */
export interface ActivitySubagentEvent {
  id: string;
  kind: ActivitySubagentEventKind;
  label: string;
  detail: string | null;
  at: number;
  isError?: boolean;
}

/** 一个子代理的可观察状态；不保存子模型的私有推理文本。 */
export interface ActivitySubagent {
  id: string;
  label: string;
  runtimeKind: AgentRun["runtime_kind"];
  model: string | null;
  accessMode: SubagentAccessMode;
  routingReason: string | null;
  status: SubagentStatus;
  phase: ActivityPhase;
  detail: string | null;
  startedAt: number;
  lastEventAt: number;
  endedAt: number | null;
  events: ActivitySubagentEvent[];
}

export interface ActivityTraceState {
  /** 当前可观察运行阶段；idle 表示没有活跃运行。 */
  phase: ActivityPhase;
  /** 面向用户的阶段文案，绝不承载或模拟推理过程。 */
  label: string;
  /** 当前运行的本地开始时间戳（毫秒）。 */
  startedAt: number | null;
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
    lastEventAt: null,
    running: false,
    queue: { queued: 0, dispatching: 0, failed: 0 },
    pendingPermissions: 0,
    subagents: [],
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
      return {
        ...state,
        running: true,
        phase: "steer_accepted",
        label: "已接纳，等待当前步骤完成",
        startedAt,
      };
    case "queue":
      return state;
    case "send_now":
      return {
        ...state,
        running: true,
        phase: "requesting",
        label: "正在切换到新请求",
        startedAt,
      };
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
      return applyActivityEvent(base, event.phase, event.detail);
    case "message":
      return { ...base, phase: "streaming", label: "正在生成回复" };
    case "tool_call": {
      const tool = observableToolName(event.name);
      return { ...base, phase: "tool", label: `正在使用工具：${tool}` };
    }
    case "tool_result":
      return {
        ...base,
        phase: "tool",
        label: event.is_error ? "工具执行失败" : "工具已完成",
      };
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
    runtimeKind: scope.runtime_kind ?? prior?.runtimeKind ?? "native",
    model: scope.model?.trim() || prior?.model || null,
    accessMode: scope.access_mode ?? prior?.accessMode ?? "read_only",
    routingReason: safeChildDetail(scope.routing_reason) ?? prior?.routingReason ?? null,
    status: prior?.status ?? "queued",
    phase: prior?.phase ?? "requesting",
    detail: prior?.detail ?? null,
    startedAt: prior?.startedAt ?? at,
    lastEventAt: at,
    endedAt: prior?.endedAt ?? null,
    events: prior?.events ?? [],
  };

  switch (event.type) {
    case "subagent_lifecycle":
      child.status = event.state;
      child.detail = safeChildDetail(event.detail);
      child.endedAt = isTerminalChildStatus(event.state) ? at : null;
      child.phase = childPhaseForStatus(event.state, child.phase);
      appendChildEvent(child, "lifecycle", childStatusEventLabel(event.state), child.detail, at,
        event.state === "failed");
      break;
    case "activity":
      child.phase = event.phase;
      child.detail = safeChildDetail(event.detail) ?? activityLabel(event.phase);
      if (event.phase === "waiting_permission") child.status = "waiting_permission";
      else if (!isTerminalChildStatus(child.status)) child.status = "running";
      appendChildEvent(child, "activity", "进度", child.detail, at);
      break;
    case "tool_call":
      child.phase = "tool";
      child.status = "running";
      child.detail = observableChildToolCall(event.name, event.input);
      appendChildEvent(child, "tool_call", observableToolName(event.name), child.detail, at);
      break;
    case "tool_result":
      child.phase = "tool";
      child.status = "running";
      child.detail = event.is_error ? "工具执行失败" : "工具已完成";
      appendChildEvent(child, "tool_result", event.is_error ? "工具失败" : "工具完成", child.detail, at,
        event.is_error);
      break;
    case "message":
      child.phase = "streaming";
      child.status = "running";
      child.detail = event.delta ? "正在生成可见结果" : "已生成一条可见结果";
      appendChildMessage(child, event.text, event.delta, at);
      break;
    case "plan":
      child.phase = "finalizing";
      child.status = "running";
      child.detail = "正在整理结果";
      appendChildEvent(child, "plan", "计划", `已更新 ${event.steps.length} 个步骤`, at);
      break;
    case "state":
      if (event.state === "interrupted") {
        child.status = "cancelled";
        child.endedAt = at;
        appendChildEvent(child, "lifecycle", "已停止", null, at);
      } else if (event.state === "review_ready") {
        child.status = "completed";
        child.endedAt = at;
        appendChildEvent(child, "lifecycle", "已完成", null, at);
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

function appendChildEvent(
  child: ActivitySubagent,
  kind: ActivitySubagentEventKind,
  label: string,
  detail: string | null,
  at: number,
  isError = false
) {
  const previous = child.events[child.events.length - 1];
  if (previous?.kind === kind && previous.label === label && previous.detail === detail) {
    return;
  }
  child.events = [
    ...child.events,
    {
      id: `${child.id}:${at}:${child.events.length}`,
      kind,
      label,
      detail,
      at,
      ...(isError ? { isError: true } : {}),
    },
  ].slice(-60);
}

/**
 * provider 的 message 事件通常是 token/chunk 增量。它们属于同一条公开回复，
 * 不能在右栏按 token 生成几十张“子智能体”消息卡。
 */
function appendChildMessage(
  child: ActivitySubagent,
  text: string,
  delta: boolean,
  at: number,
) {
  if (!text) return;
  const previous = child.events[child.events.length - 1];
  const isError = text.trimStart().startsWith("[error]");
  if (previous?.kind === "message") {
    const current = previous.detail ?? "";
    let combined: string;
    if (delta) {
      combined = current + text;
    } else if (!current || text.startsWith(current)) {
      combined = text;
    } else if (current.startsWith(text)) {
      combined = current;
    } else {
      combined = `${current}${text}`;
    }
    child.events = [
      ...child.events.slice(0, -1),
      {
        ...previous,
        detail: combined.slice(0, 12_000),
        at,
        ...(previous.isError || isError ? { isError: true } : {}),
      },
    ];
    return;
  }
  const messageEvent: ActivitySubagentEvent = {
    id: `${child.id}:${at}:${child.events.length}`,
    kind: "message",
    label: "可见结果",
    detail: text.slice(0, 12_000),
    at,
    ...(isError ? { isError: true } : {}),
  };
  child.events = [...child.events, messageEvent].slice(-60);
}

function childStatusEventLabel(status: SubagentStatus): string {
  switch (status) {
    case "queued": return "已加入队列";
    case "running": return "已开始";
    case "waiting_permission": return "等待权限";
    case "completed": return "已完成";
    case "failed": return "运行失败";
    case "cancelled": return "已停止";
  }
}

function observableChildToolCall(name: string, input: unknown): string {
  const tool = observableToolName(name);
  if (input && typeof input === "object" && "summary" in input) {
    const summary = (input as { summary?: unknown }).summary;
    if (typeof summary === "string") {
      const detail = safeChildDetail(summary);
      if (detail) return `${tool} · ${detail}`;
    }
  }
  return `正在使用工具：${tool}`;
}

function applyActivityEvent(
  state: ActivityTraceState,
  phase: AgentActivityPhase,
  detail: string | undefined
): ActivityTraceState {
  return { ...state, phase, label: activityLabel(phase, detail) };
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
      runtimeKind: run.runtime_kind,
      model: run.model || null,
      accessMode: run.access_mode ?? prior?.accessMode ?? "read_only",
      routingReason: safeChildDetail(run.routing_reason ?? undefined) ?? prior?.routingReason ?? null,
      status: persistedSubagentStatus(run),
      phase: prior?.phase ?? (run.ended_at ? "idle" : "requesting"),
      detail: safeChildDetail(run.summary ?? undefined) ?? prior?.detail ?? null,
      startedAt,
      lastEventAt: prior?.lastEventAt ?? endedAt ?? startedAt,
      endedAt,
      events: prior?.events ?? [],
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
    case "routing":
      return detail ? `正在路由：${observableToolDetail(detail)}` : "正在选择执行器";
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
    case "reviewing":
      return detail ? observableToolDetail(detail) : "正在质量复核";
  }
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
