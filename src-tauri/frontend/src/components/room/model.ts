/**
 * Room 场景数据模型 —— 纯函数，不依赖 React。
 * - buildTimeline：sessionMessages（会话流）+ task events（带时间戳的工具锚点）+ AgentRun 合并为时间线条目。
 *   会话消息除 meta 外无时间戳：tool_call/tool_result 按顺序与 TaskEvent 对齐取 created_at，
 *   其余消息取「最近已知时间」（单调不减），供回放播放头调暗（data-t）用。
 */
import type {
  AgentEvent,
  AgentSendMode,
  AgentRun,
  QueuedMessage,
  QueuedMessageState,
  SessionMessage,
  TaskEvent,
} from "../../lib/types";
import { toolTarget } from "../../lib/format";

export interface PlanStep {
  description: string;
  completed: boolean;
}

export type ToolState = "active" | "ok" | "fail";

export type RunViewState = "active" | "finished" | "aborted" | "accepted" | "answered" | "failed";

export interface RunPresentation {
  state: RunViewState;
  label: string;
}

/** AgentRun 是运行呈现的唯一真源；任务事件继续保留给审计和时间锚点。 */
export function runPresentation(run: AgentRun): RunPresentation {
  if (run.ended_at == null) return { state: "active", label: "运行中" };
  switch (run.review_state) {
    case "rolled_back":
    case "aborted":
      return { state: "aborted", label: "已中止" };
    case "accepted":
    case "auto_accepted":
      return { state: "accepted", label: "已接受" };
    case "answered":
      return { state: "answered", label: "已答复" };
    case "failed":
      return { state: "failed", label: "执行失败" };
    case "pending":
      return { state: "finished", label: "已完成" };
    default:
      return { state: "finished", label: "已完成" };
  }
}

export type TimelineItem =
  | { kind: "ms"; id: string; t: number; ok: boolean | null; label: string }
  | {
      kind: "you";
      id: string;
      t: number;
      text: string;
      messageId?: string;
      sendMode: AgentSendMode;
      queuedState?: QueuedMessageState;
      queueId?: string;
    }
  | { kind: "agent"; id: string; t: number; text: string; streaming: boolean }
  | { kind: "plan"; id: string; t: number; steps: PlanStep[] }
  | {
      kind: "run";
      id: string;
      t: number;
      model: string;
      agentKind: AgentRun["agent_kind"];
      agentLabel: string | null;
      agentSummary: string | null;
      state: RunViewState;
      label: string;
    }
  | {
      kind: "tool";
      id: string;
      t: number;
      callId: string | null;
      name: string;
      target: string;
      state: ToolState;
      summary: string;
    };

/** RFC3339 → 相对会话起点的秒数（无法解析时回退 0，不为负）。 */
export function relSec(iso: string | null | undefined, startMs: number): number {
  if (!iso) return 0;
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return 0;
  return Math.max(0, (t - startMs) / 1000);
}

/** tool_result 输出 → 单行摘要。 */
export function summarizeOutput(outputJson: string | null | undefined, isError: boolean): string {
  const pick = (v: unknown): string => {
    if (typeof v === "string") return v;
    if (v && typeof v === "object") {
      const o = v as Record<string, unknown>;
      for (const k of ["content", "text", "stdout", "output", "message", "error"]) {
        const s = o[k];
        if (typeof s === "string" && s) return s;
      }
      try {
        return JSON.stringify(v);
      } catch {
        return String(v);
      }
    }
    return v == null ? "" : String(v);
  };
  let raw = "";
  if (outputJson) {
    try {
      raw = pick(JSON.parse(outputJson));
    } catch {
      raw = outputJson;
    }
  }
  const firstLine = raw.split("\n").find((l) => l.trim().length > 0) ?? "";
  const body = firstLine.trim();
  const cut = body.length > 72 ? body.slice(0, 71) + "…" : body;
  if (!cut) return isError ? "error" : "done";
  return cut;
}

/** 合并会话消息、任务事件的工具时间锚点与每次 AgentRun 为时间线条目。 */
export function buildTimeline(
  messages: SessionMessage[],
  events: TaskEvent[],
  runs: AgentRun[],
  startIso: string,
  queuedMessages: QueuedMessage[] = []
): TimelineItem[] {
  const startMs = Date.parse(startIso);
  const anchor = Number.isNaN(startMs) ? Date.now() : startMs;
  const sorted = [...events].sort((a, b) => a.id - b.id);
  const callTs = sorted.filter((e) => e.event_type === "tool_call").map((e) => e.created_at);
  const resultTs = sorted.filter((e) => e.event_type === "tool_result").map((e) => e.created_at);
  let callIdx = 0;
  let resultIdx = 0;

  const items: TimelineItem[] = [];
  let lastT = 0;
  let seq = 0;
  let nextUserSendMode: AgentSendMode | null = null;
  const nid = (p: string) => `${p}-${seq++}`;

  for (const m of messages) {
    switch (m.kind) {
      case "meta":
        break; // 会话元信息不进时间线（胶片 meta 行展示）
      case "message": {
        const text = (m.text ?? "").trim();
        if (!text) break;
        if (m.role === "user") {
          items.push({
            kind: "you",
            id: nid("you"),
            t: lastT,
            text,
            messageId: m.id,
            sendMode: nextUserSendMode ?? "auto",
          });
          nextUserSendMode = null;
        } else {
          items.push({ kind: "agent", id: nid("ag"), t: lastT, text, streaming: false });
        }
        break;
      }
      case "system": {
        if (m.text === "r_code_user_message_mode") {
          nextUserSendMode = parseUserSendMode(m.output_json) ?? "auto";
        } else if (m.text === "plan" && m.output_json) {
          try {
            const data = JSON.parse(m.output_json) as { steps?: PlanStep[] };
            if (Array.isArray(data.steps)) {
              items.push({ kind: "plan", id: nid("pl"), t: lastT, steps: data.steps });
            }
          } catch {
            /* 无法解析的 plan 负载直接跳过 */
          }
        } else if (m.text) {
          items.push({ kind: "ms", id: nid("ms"), t: lastT, ok: null, label: m.text });
        }
        break;
      }
      case "tool_call": {
        const ts = callTs[callIdx++];
        if (ts) lastT = Math.max(lastT, relSec(ts, anchor));
        items.push({
          kind: "tool",
          id: nid("tc"),
          t: lastT,
          callId: m.call_id ?? null,
          name: m.tool_name ?? "tool",
          target: toolTarget(m.input_json),
          state: "active",
          summary: "",
        });
        break;
      }
      case "tool_result": {
        const ts = resultTs[resultIdx++];
        if (ts) lastT = Math.max(lastT, relSec(ts, anchor));
        const summary = summarizeOutput(m.output_json, m.is_error === true);
        const state: ToolState = m.is_error === true ? "fail" : "ok";
        const row = [...items].reverse().find(
          (it): it is Extract<TimelineItem, { kind: "tool" }> =>
            it.kind === "tool" && it.callId != null && it.callId === m.call_id && it.state === "active"
        );
        if (row) {
          row.state = state;
          row.summary = summary;
        } else {
          items.push({
            kind: "tool",
            id: nid("tr"),
            t: lastT,
            callId: m.call_id ?? null,
            name: "result",
            target: "",
            state,
            summary,
          });
        }
        break;
      }
    }
  }

  for (const queued of queuedMessages) {
    const sendMode = queueSendMode(queued);
    if (
      items.some(
        (item) =>
          item.kind === "you" && item.text === queued.message && item.sendMode === sendMode
      )
    ) {
      continue;
    }
    const t = Math.max(lastT, relSec(queued.created_at, anchor));
    lastT = t;
    items.push({
      kind: "you",
      id: `queued-${queued.id}`,
      t,
      text: queued.message,
      sendMode,
      queuedState: queued.state,
      queueId: queued.id,
    });
  }

  return mergeRunItems(items, runs, anchor);
}

function parseUserSendMode(value: string | null | undefined): AgentSendMode | null {
  if (!value) return null;
  try {
    const payload = JSON.parse(value) as { mode?: unknown };
    switch (payload.mode) {
      case "auto":
      case "steer":
      case "queue":
      case "send_now":
        return payload.mode;
      default:
        return null;
    }
  } catch {
    return null;
  }
}

/** 当前持久化队列用优先级区分“普通排队”和“立即发送”的中止后优先分发。 */
function queueSendMode(message: QueuedMessage): AgentSendMode {
  return message.priority >= 1_000_000 ? "send_now" : "queue";
}

/** 运行条目按 run ID 去重，刷新时以 AgentRun 的最新状态替换旧条目。 */
export function mergeRunItems(
  items: TimelineItem[],
  runs: AgentRun[],
  startMs: number
): TimelineItem[] {
  const withoutRuns = items.filter((item) => item.kind !== "run");
  const seen = new Set<string>();
  const runItems: Extract<TimelineItem, { kind: "run" }>[] = [];

  for (const run of orderedUniqueRuns(runs)) {
    if (seen.has(run.id)) continue;
    seen.add(run.id);
    const presentation = runPresentation(run);
    runItems.push({
      kind: "run",
      id: `run-${run.id}`,
      t: relSec(run.started_at, startMs),
      model: run.model,
      agentKind: run.agent_kind,
      agentLabel: run.agent_label,
      agentSummary: run.summary,
      ...presentation,
    });
  }

  const merged: TimelineItem[] = [...withoutRuns];
  for (const runItem of runItems) {
    let index = 0;
    while (index < merged.length && merged[index].t <= runItem.t) index++;
    merged.splice(index, 0, runItem);
  }
  return merged;
}

function orderedUniqueRuns(runs: AgentRun[]): AgentRun[] {
  const seen = new Set<string>();
  return [...runs]
    .sort((a, b) => a.started_at.localeCompare(b.started_at) || a.id.localeCompare(b.id))
    .filter((run) => {
      if (seen.has(run.id)) return false;
      seen.add(run.id);
      return true;
    });
}

/** 将一条流式 AgentEvent 应用到时间线（返回新数组）。nowSec 由调用方提供。 */
export function applyAgentEvent(
  prev: TimelineItem[],
  ev: AgentEvent,
  nowSec: number,
  nid: () => string
): TimelineItem[] {
  const items = [...prev];
  const last = items[items.length - 1];
  switch (ev.type) {
    case "message": {
      if (last?.kind === "agent" && last.streaming) {
        items[items.length - 1] = { ...last, text: last.text + ev.text, streaming: ev.delta };
      } else {
        items.push({ kind: "agent", id: nid(), t: nowSec, text: ev.text, streaming: ev.delta });
      }
      return items;
    }
    case "tool_call": {
      let inputJson: string | undefined;
      try {
        inputJson = JSON.stringify(ev.input);
      } catch {
        inputJson = undefined;
      }
      items.push({
        kind: "tool",
        id: nid(),
        t: nowSec,
        callId: ev.call_id,
        name: ev.name,
        target: toolTarget(inputJson),
        state: "active",
        summary: "",
      });
      return items;
    }
    case "tool_result": {
      const summary = summarizeOutput(
        typeof ev.output === "string" ? ev.output : JSON.stringify(ev.output),
        ev.is_error
      );
      for (let i = items.length - 1; i >= 0; i--) {
        const it = items[i];
        if (it.kind === "tool" && it.callId === ev.call_id && it.state === "active") {
          items[i] = { ...it, state: ev.is_error ? "fail" : "ok", summary };
          return items;
        }
      }
      items.push({
        kind: "tool",
        id: nid(),
        t: nowSec,
        callId: ev.call_id,
        name: "result",
        target: "",
        state: ev.is_error ? "fail" : "ok",
        summary,
      });
      return items;
    }
    case "plan": {
      for (let i = items.length - 1; i >= 0; i--) {
        const it = items[i];
        if (it.kind === "plan") {
          items[i] = { ...it, steps: ev.steps };
          return items;
        }
      }
      items.push({ kind: "plan", id: nid(), t: nowSec, steps: ev.steps });
      return items;
    }
    case "activity":
      // 活动条消费该事件；时间线不展示内部活动或推理文本。
      return items;
    case "scoped":
    case "subagent_lifecycle":
      // 子代理详情由 Working 列表和持久化运行树呈现；主时间线默认保持折叠。
      return items;
    case "state":
      return items; // state 事件由订阅方触发 refresh + 历史重建
  }
}

// ---------- 终端输出 ----------

/* eslint-disable no-control-regex */
const ANSI_RE =
  /\x1B(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1B\\)|\([0-9A-B]|[@-_])/g;

/** 剥离 ANSI 转义序列（CSI/OSC/单字符），CR 归一化 —— 终端输出以纯文本 <pre> 展示。 */
export function stripAnsi(s: string): string {
  return s
    .replace(ANSI_RE, "")
    .replace(/\x07/g, "")
    .replace(/\r\n/g, "\n")
    .replace(/\r/g, "");
}
